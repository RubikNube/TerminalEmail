use super::*;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

fn test_config(port: u16, starttls: bool) -> Config {
    let mut config: Config = toml::from_str(include_str!("../../config.example.toml")).unwrap();
    config.account.imap.host = "localhost".into();
    config.account.imap.port = port;
    config.account.imap.tls = if starttls {
        TlsMode::Starttls
    } else {
        TlsMode::Implicit
    };
    config
}

fn certificate() -> (native_tls::Identity, native_tls::Certificate) {
    use openssl::{
        asn1::Asn1Time,
        hash::MessageDigest,
        pkey::PKey,
        rsa::Rsa,
        x509::{
            X509, X509NameBuilder,
            extension::{BasicConstraints, SubjectAlternativeName},
        },
    };
    let key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let mut name = X509NameBuilder::new().unwrap();
    name.append_entry_by_text("CN", "localhost").unwrap();
    let name = name.build();
    let mut cert = X509::builder().unwrap();
    cert.set_version(2).unwrap();
    cert.set_subject_name(&name).unwrap();
    cert.set_issuer_name(&name).unwrap();
    cert.set_pubkey(&key).unwrap();
    cert.set_not_before(&Asn1Time::days_from_now(0).unwrap())
        .unwrap();
    cert.set_not_after(&Asn1Time::days_from_now(1).unwrap())
        .unwrap();
    cert.append_extension(BasicConstraints::new().critical().ca().build().unwrap())
        .unwrap();
    let san = SubjectAlternativeName::new()
        .dns("localhost")
        .build(&cert.x509v3_context(None, None))
        .unwrap();
    cert.append_extension(san).unwrap();
    cert.sign(&key, MessageDigest::sha256()).unwrap();
    let pem = cert.build().to_pem().unwrap();
    (
        native_tls::Identity::from_pkcs8(&pem, &key.private_key_to_pem_pkcs8().unwrap()).unwrap(),
        native_tls::Certificate::from_pem(&pem).unwrap(),
    )
}

#[tokio::test]
async fn real_imap_adapter_over_verified_tls() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (identity, root) = certificate();
    let acceptor =
        tokio_native_tls::TlsAcceptor::from(native_tls::TlsAcceptor::new(identity).unwrap());
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(acceptor.accept(tcp).await.unwrap());
        stream
            .get_mut()
            .write_all(b"* OK Mock IMAP ready\r\n")
            .await
            .unwrap();
        let mut transcript = Vec::new();
        loop {
            let mut line = String::new();
            let count = tokio::time::timeout(Duration::from_secs(5), stream.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            if count == 0 {
                break;
            }
            let (tag, command) = line.trim_end().split_once(' ').unwrap();
            transcript.push(command.to_owned());
            let mut response = String::new();
            if command.starts_with("LOGIN ") {
            } else if command.starts_with("LIST ") {
                response = "* LIST () \"/\" \"INBOX\"\r\n* LIST (\\Sent) \"/\" \"Sent\"\r\n".into();
            } else if command.starts_with("STATUS ") {
                let name = if command.contains("\"Sent\"") {
                    "Sent"
                } else {
                    "INBOX"
                };
                response = format!("* STATUS \"{name}\" (UNSEEN 1)\r\n");
            } else if command.starts_with("SELECT ") {
                response =
                    "* 1 EXISTS\r\n* FLAGS (\\Seen \\Flagged)\r\n* OK [UIDVALIDITY 7] Valid\r\n"
                        .into();
            } else if command == "UID SEARCH UNDELETED" {
                response = "* SEARCH 7\r\n".into();
            } else if command.starts_with("UID FETCH ") {
                let body = command.contains("BODY.PEEK[]");
                let header = "From: Alice <alice@example.org>\r\nTo: you@example.com\r\nSubject: TLS test\r\nMessage-ID: <tls@test>\r\nContent-Type: text/plain\r\n\r\n";
                let content = if body {
                    format!("{header}Hello over TLS")
                } else {
                    header.into()
                };
                let section = if body { "" } else { "HEADER" };
                response = format!(
                    "* 1 FETCH (UID 7 FLAGS (\\Seen \\Flagged) BODY[{section}] {{{}}}\r\n{content})\r\n",
                    content.len()
                );
            } else if command.starts_with("UID STORE ") || command.starts_with("UID MOVE ") {
            } else if command == "CAPABILITY" {
                response = "* CAPABILITY IMAP4rev1 MOVE UIDPLUS\r\n".into();
            } else if command.starts_with("APPEND ") {
                let length: usize = command
                    .rsplit_once('{')
                    .unwrap()
                    .1
                    .trim_end_matches('}')
                    .parse()
                    .unwrap();
                stream
                    .get_mut()
                    .write_all(b"+ Ready for literal\r\n")
                    .await
                    .unwrap();
                let mut literal = vec![0; length + 2];
                stream.read_exact(&mut literal).await.unwrap();
                assert!(String::from_utf8_lossy(&literal).contains("Subject: Outbound test"));
            } else {
                panic!("Unexpected test-server command");
            }
            response.push_str(&format!("{tag} OK Complete\r\n"));
            stream
                .get_mut()
                .write_all(response.as_bytes())
                .await
                .unwrap();
        }
        transcript
    });
    let mut backend = ImapSmtp::new(test_config(port, false), "test-secret".into()).unwrap();
    backend.tls_connector = native_tls::TlsConnector::builder()
        .add_root_certificate(root)
        .build()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        let folders = backend.folders().await.unwrap();
        assert_eq!(folders.len(), 2);
        assert_eq!(folders[1].role.as_deref(), Some("sent"));
        assert_eq!(backend.select("INBOX", None).await.unwrap(), 7);
        assert_eq!(backend.uids().await.unwrap(), HashSet::from([7]));
        let headers = backend.fetch("INBOX", 7, &[7], false).await.unwrap();
        assert_eq!(headers[0].subject, "TLS test");
        assert!(headers[0].seen && headers[0].starred);
        assert!(headers[0].body.is_none());
        let bodies = backend.fetch("INBOX", 7, &[7], true).await.unwrap();
        assert_eq!(bodies[0].body.as_deref(), Some("Hello over TLS"));
        backend.flag(7, true, false).await.unwrap();
        backend.move_to(7, "Archive").await.unwrap();
        let outgoing = lettre::Message::builder()
            .from("a@example.org".parse().unwrap())
            .to("b@example.org".parse().unwrap())
            .subject("Outbound test")
            .body("Hello".to_owned())
            .unwrap();
        backend.append_sent(&outgoing, &folders).await.unwrap();
    })
    .await
    .unwrap();
    drop(backend);
    let transcript = server.await.unwrap();
    assert!(transcript.iter().any(|c| c.starts_with("UID MOVE 7")));
    assert!(
        transcript
            .iter()
            .any(|c| c == "UID STORE 7 -FLAGS.SILENT (\\Flagged)")
    );
}

#[tokio::test]
async fn imap_refused_starttls_never_sends_credentials() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(tcp);
        stream
            .get_mut()
            .write_all(b"* OK Mock IMAP\r\n")
            .await
            .unwrap();
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        let tag = line.split_whitespace().next().unwrap();
        assert!(line.contains("STARTTLS"));
        stream
            .get_mut()
            .write_all(format!("{tag} NO TLS unavailable\r\n").as_bytes())
            .await
            .unwrap();
        let mut remaining = String::new();
        stream.read_to_string(&mut remaining).await.unwrap();
        assert!(!remaining.contains("LOGIN"));
        assert!(!remaining.contains("secret"));
    });
    let mut backend = ImapSmtp::new(test_config(port, true), "secret".into()).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), backend.session())
            .await
            .unwrap()
            .is_err()
    );
    server.await.unwrap();
}

#[tokio::test]
async fn smtp_without_starttls_never_authenticates_or_sends_mail() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(tcp);
        stream
            .get_mut()
            .write_all(b"220 localhost Mock SMTP\r\n")
            .await
            .unwrap();
        let mut hello = String::new();
        stream.read_line(&mut hello).await.unwrap();
        assert!(hello.starts_with("EHLO "));
        stream
            .get_mut()
            .write_all(b"250-localhost\r\n250 AUTH PLAIN LOGIN\r\n")
            .await
            .unwrap();
        let mut remaining = String::new();
        stream.read_to_string(&mut remaining).await.unwrap();
        assert!(!remaining.contains("AUTH"));
        assert!(!remaining.contains("MAIL FROM"));
    });
    let mut config = test_config(port, false);
    config.account.smtp = crate::config::Server {
        host: "localhost".into(),
        port,
        tls: TlsMode::Starttls,
    };
    let mut backend = ImapSmtp::new(config, "secret".into()).unwrap();
    let message = lettre::Message::builder()
        .from("a@example.org".parse().unwrap())
        .to("b@example.org".parse().unwrap())
        .subject("Not sent")
        .body("Private".to_owned())
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), backend.send(&message))
            .await
            .unwrap()
            .is_err()
    );
    server.await.unwrap();
}

struct FakeMail {
    messages: Vec<Message>,
    sent: usize,
    sent_copy_fails: bool,
    send_fails: bool,
    selected: String,
    validity: u32,
    events: Option<mpsc::UnboundedSender<String>>,
    gate: Option<(String, mpsc::Receiver<()>)>,
}

impl FakeMail {
    async fn record(&mut self, event: String) {
        if let Some(events) = &self.events {
            let _ = events.send(event.clone());
        }
        if let Some((blocked, permits)) = &mut self.gate
            && event == *blocked
        {
            permits.recv().await.expect("test gate closed");
        }
    }
}
impl MailBackend for FakeMail {
    async fn unread(&mut self, folder: &str) -> Result<u32> {
        self.record(format!("unread:{folder}")).await;
        Ok(0)
    }
    fn disconnect(&mut self) {
        self.selected.clear();
        if let Some(events) = &self.events {
            let _ = events.send("disconnect".into());
        }
    }

    async fn folders(&mut self) -> Result<Vec<Folder>> {
        self.record("folders".into()).await;
        Ok(vec![Folder {
            name: "INBOX".into(),
            ..Default::default()
        }])
    }
    async fn select(&mut self, folder: &str, expected: Option<u32>) -> Result<u32> {
        self.record(format!("select:{folder}")).await;
        self.selected = folder.into();
        if expected.is_some_and(|v| v != self.validity) {
            bail!("UIDVALIDITY changed");
        }
        Ok(self.validity)
    }
    async fn uids(&mut self) -> Result<HashSet<u32>> {
        self.record(format!("uids:{}", self.selected)).await;
        Ok(self
            .messages
            .iter()
            .filter(|m| m.folder == self.selected)
            .map(|m| m.uid)
            .collect())
    }
    async fn fetch(
        &mut self,
        folder: &str,
        _: u32,
        uids: &[u32],
        body: bool,
    ) -> Result<Vec<Message>> {
        assert_eq!(
            self.selected, folder,
            "fetch used the wrong selected mailbox"
        );
        self.record(format!(
            "{}:{folder}",
            if body { "body" } else { "headers" }
        ))
        .await;
        Ok(self
            .messages
            .iter()
            .filter(|m| m.folder == folder && uids.contains(&m.uid))
            .cloned()
            .map(|mut m| {
                if !body {
                    m.body = None;
                    m.html_body = None;
                    m.html_loaded = false;
                } else {
                    m.html_loaded = true;
                }
                m
            })
            .collect())
    }
    async fn flag(&mut self, uid: u32, starred: bool, value: bool) -> Result<()> {
        self.record(format!("flag:{uid}:{value}")).await;
        if let Some(m) = self.messages.iter_mut().find(|m| m.uid == uid) {
            if starred {
                m.starred = value;
            } else {
                m.seen = value;
            }
        }
        Ok(())
    }
    async fn move_to(&mut self, uid: u32, _: &str) -> Result<()> {
        self.messages.retain(|m| m.uid != uid);
        Ok(())
    }
    async fn send(&mut self, _: &lettre::Message) -> Result<()> {
        if self.send_fails {
            bail!("SMTP rejected");
        }
        self.sent += 1;
        Ok(())
    }
    async fn append_sent(&mut self, _: &lettre::Message, _: &[Folder]) -> Result<()> {
        if self.sent_copy_fails {
            bail!("Sent mailbox unavailable");
        }
        Ok(())
    }
    fn destination(&self, role: &str, _: &[Folder]) -> Result<String> {
        Ok(role.to_owned())
    }
}

fn setup() -> (
    tempfile::TempDir,
    MailWorker<FakeMail>,
    mpsc::UnboundedReceiver<Update>,
) {
    let temporary = tempfile::tempdir().unwrap();
    let cache = Cache::open(&temporary.path().join("mail.db")).unwrap();
    let (sender, receiver) = mpsc::unbounded_channel();
    let backend = FakeMail {
        messages: vec![Message {
            uid: 1,
            folder: "INBOX".into(),
            uid_validity: 7,
            subject: "Hello".into(),
            body: Some("Downloaded text".into()),
            ..Default::default()
        }],
        sent: 0,
        sent_copy_fails: false,
        send_fails: false,
        selected: String::new(),
        validity: 7,
        events: None,
        gate: None,
    };
    (
        temporary,
        MailWorker {
            cache,
            backend,
            sender,
            jobs: VecDeque::new(),
            selected: None,
        },
        receiver,
    )
}

#[tokio::test]
async fn legacy_cached_text_refetches_html_then_uses_complete_cache() {
    let (_temp, mut worker, mut updates) = setup();
    let legacy = worker.backend.messages[0].clone();
    worker.cache.put(&legacy).unwrap();
    worker.backend.messages[0].html_body = Some("<p>Original HTML</p>".into());
    worker.handle(Request::Body(legacy.clone())).await.unwrap();
    let cached = worker.cache.message("INBOX", legacy.uid).unwrap().unwrap();
    assert!(cached.html_loaded);
    assert_eq!(cached.html_body.as_deref(), Some("<p>Original HTML</p>"));
    assert!(matches!(updates.try_recv(), Ok(Update::Body(message)) if message.html_loaded));
    // A known HTML result must avoid a second network fetch.
    worker.backend.messages.clear();
    worker.handle(Request::Body(legacy)).await.unwrap();
    assert!(matches!(updates.try_recv(), Ok(Update::Body(message)) if message.html_body.is_some()));
}

#[tokio::test]
async fn sync_lazy_download_flags_search_and_move() {
    let (_temp, mut worker, mut receiver) = setup();
    worker
        .handle(Request::Refresh("INBOX".into()))
        .await
        .unwrap();
    finish_sync(&mut worker).await;
    let header = worker.cache.message("INBOX", 1).unwrap().unwrap();
    assert!(header.body.is_none());
    worker.handle(Request::Body(header.clone())).await.unwrap();
    assert_eq!(
        worker
            .cache
            .message("INBOX", 1)
            .unwrap()
            .unwrap()
            .body
            .as_deref(),
        Some("Downloaded text")
    );
    worker
        .handle(Request::Flag {
            message: header.clone(),
            starred: true,
            value: true,
        })
        .await
        .unwrap();
    worker
        .handle(Request::Refresh("INBOX".into()))
        .await
        .unwrap();
    finish_sync(&mut worker).await;
    let saved = worker.cache.message("INBOX", 1).unwrap().unwrap();
    assert!(saved.starred);
    assert!(saved.body.is_some());
    worker
        .handle(Request::Search {
            folder: "INBOX".into(),
            query: "Downloaded".into(),
        })
        .await
        .unwrap();
    assert!(
        std::iter::from_fn(|| receiver.try_recv().ok())
            .any(|event| matches!(event, Update::Search { messages, .. } if messages.len() == 1))
    );
    worker
        .handle(Request::Move {
            message: saved,
            role: "trash".into(),
        })
        .await
        .unwrap();
    finish_sync(&mut worker).await;
    assert!(worker.cache.messages("INBOX").unwrap().is_empty());
    let mut stale = header;
    stale.uid_validity = 6;
    assert!(
        worker
            .handle(Request::Flag {
                message: stale,
                starred: false,
                value: true
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn accepted_smtp_and_failed_sent_copy_are_never_reported_as_send_failure() {
    let (temp, mut worker, mut receiver) = setup();
    worker.backend.sent_copy_fails = true;
    let file = tempfile::NamedTempFile::new_in(temp.path()).unwrap();
    let path = file.path().to_owned();
    let draft = Draft {
        path: path.clone(),
        kind: crate::compose::ComposeKind::New,
        original: None,
    };
    let message = lettre::Message::builder()
        .from("a@example.org".parse().unwrap())
        .to("b@example.org".parse().unwrap())
        .subject("Test")
        .body("Hello".to_owned())
        .unwrap();
    worker
        .handle(Request::Send {
            draft: draft.clone(),
            message: Box::new(message.clone()),
        })
        .await
        .unwrap();
    assert_eq!(worker.backend.sent, 1);
    assert!(path.exists());
    assert!(
        std::iter::from_fn(|| receiver.try_recv().ok()).any(|event| matches!(
            event,
            Update::Sent {
                warning: Some(_),
                ..
            }
        ))
    );
    worker.backend.send_fails = true;
    assert!(
        worker
            .handle(Request::Send {
                draft,
                message: Box::new(message)
            })
            .await
            .is_err()
    );
    assert!(path.exists());
    assert_eq!(worker.backend.sent, 1);
}

async fn finish_sync<B: MailBackend>(worker: &mut MailWorker<B>) {
    while let Some(job) = worker.jobs.pop_front() {
        if let Some(job) = worker.advance(job).await.unwrap() {
            worker.jobs.push_front(job);
        }
    }
}

fn observed(
    worker: &mut MailWorker<FakeMail>,
    blocked: &str,
) -> (mpsc::UnboundedReceiver<String>, mpsc::Sender<()>) {
    let (events, observed) = mpsc::unbounded_channel();
    let (permits, gate) = mpsc::channel(8);
    worker.backend.events = Some(events);
    worker.backend.gate = Some((blocked.into(), gate));
    (observed, permits)
}

async fn until(events: &mut mpsc::UnboundedReceiver<String>, expected: &str) {
    loop {
        let event = events
            .recv()
            .await
            .expect("worker stopped before expected operation");
        if event == expected {
            break;
        }
    }
}

#[tokio::test]
async fn opening_during_header_batch_overtakes_remaining_sync_and_preserves_body() {
    let (_temp, mut worker, mut updates) = setup();
    worker.backend.messages[0].html_body = Some("<p>Downloaded HTML</p>".into());
    let template = worker.backend.messages[0].clone();
    worker.backend.messages = (1..=201)
        .map(|uid| Message {
            uid,
            ..template.clone()
        })
        .collect();
    let (mut events, permits) = observed(&mut worker, "headers:INBOX");
    let (requests, receiver) = mpsc::channel(32);
    requests
        .send(Request::Refresh("INBOX".into()))
        .await
        .unwrap();
    let task = tokio::spawn(async move {
        worker.run(receiver).await;
        worker
    });
    until(&mut events, "headers:INBOX").await;
    requests
        .send(Request::Body(template.clone()))
        .await
        .unwrap();
    requests.send(Request::Body(template)).await.unwrap();
    requests
        .send(Request::Refresh("INBOX".into()))
        .await
        .unwrap();
    permits.send(()).await.unwrap();
    // After the current batch, body selection/fetch comes before the next headers.
    assert_eq!(events.recv().await.as_deref(), Some("select:INBOX"));
    assert_eq!(events.recv().await.as_deref(), Some("body:INBOX"));
    assert_eq!(events.recv().await.as_deref(), Some("select:INBOX"));
    assert_eq!(events.recv().await.as_deref(), Some("headers:INBOX"));
    permits.send(()).await.unwrap();
    until(&mut events, "headers:INBOX").await;
    permits.send(()).await.unwrap();
    drop(requests);
    let worker = task.await.unwrap();
    let cached = worker.cache.message("INBOX", 1).unwrap().unwrap();
    assert_eq!(cached.html_body.as_deref(), Some("<p>Downloaded HTML</p>"));
    assert!(cached.html_loaded);
    assert!(
        worker
            .cache
            .message("INBOX", 1)
            .unwrap()
            .unwrap()
            .body
            .is_some()
    );
    let bodies = std::iter::from_fn(|| updates.try_recv().ok())
        .filter(|u| matches!(u, Update::Body(_)))
        .count();
    assert_eq!(bodies, 1);
    assert!(!std::iter::from_fn(|| events.try_recv().ok()).any(|e| e == "folders"));
}

#[tokio::test]
async fn opening_during_discovery_overtakes_unread_counts() {
    let (_temp, mut worker, _updates) = setup();
    let message = worker.backend.messages[0].clone();
    let (mut events, permits) = observed(&mut worker, "folders");
    let (requests, receiver) = mpsc::channel(32);
    requests
        .send(Request::Refresh("INBOX".into()))
        .await
        .unwrap();
    let task = tokio::spawn(async move {
        worker.run(receiver).await;
    });
    until(&mut events, "folders").await;
    requests.send(Request::Body(message)).await.unwrap();
    permits.send(()).await.unwrap();
    assert_eq!(events.recv().await.as_deref(), Some("select:INBOX"));
    assert_eq!(events.recv().await.as_deref(), Some("body:INBOX"));
    assert_eq!(events.recv().await.as_deref(), Some("unread:INBOX"));
    drop(requests);
    task.await.unwrap();
}

#[tokio::test]
async fn sync_restores_mailbox_and_rejects_changed_identity() {
    let (_temp, mut worker, _updates) = setup();
    worker
        .handle(Request::Refresh("INBOX".into()))
        .await
        .unwrap();
    // Pause just before fetching headers.
    while !matches!(worker.jobs.front().unwrap().phase, SyncPhase::Headers(_)) {
        let job = worker.jobs.pop_front().unwrap();
        let job = worker.advance(job).await.unwrap().unwrap();
        worker.jobs.push_front(job);
    }
    let mut other = worker.backend.messages[0].clone();
    other.folder = "Other".into();
    worker.backend.messages.push(other.clone());
    worker.selected = None;
    worker.handle(Request::Body(other)).await.unwrap();
    assert_eq!(worker.backend.selected, "Other");
    let job = worker.jobs.pop_front().unwrap();
    let job = worker.advance(job).await.unwrap().unwrap();
    assert_eq!(worker.backend.selected, "INBOX");
    worker.selected = None;
    worker.backend.validity = 8;
    assert!(worker.advance(job).await.is_err());
    assert!(worker.cache.messages("INBOX").unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn background_timeout_disconnects_and_foreground_download_still_runs() {
    let (_temp, mut worker, mut updates) = setup();
    let message = worker.backend.messages[0].clone();
    let (mut events, _permits) = observed(&mut worker, "folders");
    let (requests, receiver) = mpsc::channel(32);
    requests
        .send(Request::Refresh("INBOX".into()))
        .await
        .unwrap();
    let task = tokio::spawn(async move {
        worker.run(receiver).await;
        worker
    });
    until(&mut events, "folders").await;
    requests.send(Request::Body(message)).await.unwrap();
    tokio::time::advance(Duration::from_secs(31)).await;
    assert_eq!(events.recv().await.as_deref(), Some("disconnect"));
    until(&mut events, "body:INBOX").await;
    drop(requests);
    let worker = task.await.unwrap();
    assert!(worker.jobs.is_empty());
    assert!(
        std::iter::from_fn(|| updates.try_recv().ok())
            .any(|u| matches!(u, Update::Error(e) if e.contains("timed out")))
    );
}

#[tokio::test(start_paused = true)]
async fn body_timeout_reports_identity_and_coalesces_in_flight_duplicates() {
    let (_temp, mut worker, mut updates) = setup();
    let message = worker.backend.messages[0].clone();
    let (mut events, _permits) = observed(&mut worker, "body:INBOX");
    let (requests, receiver) = mpsc::channel(32);
    requests.send(Request::Body(message.clone())).await.unwrap();
    let task = tokio::spawn(async move {
        worker.run(receiver).await;
    });
    until(&mut events, "body:INBOX").await;
    requests.send(Request::Body(message.clone())).await.unwrap();
    tokio::time::advance(Duration::from_secs(121)).await;
    until(&mut events, "disconnect").await;
    drop(requests);
    task.await.unwrap();
    let failures: Vec<_> = std::iter::from_fn(|| updates.try_recv().ok())
        .filter_map(|u| {
            if let Update::BodyError { message, error } = u {
                Some((message, error))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].0.uid, message.uid);
    assert_eq!(failures[0].0.uid_validity, message.uid_validity);
    assert!(failures[0].1.contains("timed out"));
}

#[tokio::test]
async fn foreground_mutations_keep_fifo_order_and_body_failure_is_not_retried() {
    let (_temp, mut worker, mut updates) = setup();
    let message = worker.backend.messages[0].clone();
    let (events, mut observed) = mpsc::unbounded_channel();
    worker.backend.events = Some(events);
    let (requests, receiver) = mpsc::channel(32);
    for value in [true, false] {
        requests
            .send(Request::Flag {
                message: message.clone(),
                starred: true,
                value,
            })
            .await
            .unwrap();
    }
    let mut absent = message;
    absent.uid = 99;
    requests.send(Request::Body(absent.clone())).await.unwrap();
    requests.send(Request::Body(absent)).await.unwrap();
    drop(requests);
    worker.run(receiver).await;
    let calls: Vec<_> = std::iter::from_fn(|| observed.try_recv().ok()).collect();
    let flags: Vec<_> = calls
        .iter()
        .filter(|call| call.starts_with("flag:"))
        .map(String::as_str)
        .collect();
    assert_eq!(flags, ["flag:1:true", "flag:1:false"]);
    assert_eq!(calls.iter().filter(|call| *call == "body:INBOX").count(), 1);
    assert!(!worker.backend.messages[0].starred);
    let errors: Vec<_> = std::iter::from_fn(|| updates.try_recv().ok())
        .filter_map(|u| {
            if let Update::BodyError { message, error } = u {
                Some((message, error))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0.uid, 99);
    assert!(errors[0].1.contains("no longer exists"));
}
