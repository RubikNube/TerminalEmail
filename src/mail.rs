use crate::{
    cache::Cache,
    compose::Draft,
    config::{Config, TlsMode},
    model::{Folder, Message, parse_message, safe_text},
};
use anyhow::{Context, Result, bail};
use async_imap::types::{Flag, NameAttribute};
use futures_util::TryStreamExt;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Tokio1Executor,
    transport::smtp::authentication::Credentials,
};
use std::{
    collections::{HashSet, VecDeque},
    path::PathBuf,
    time::Duration,
};
use tokio::{net::TcpStream, sync::mpsc};

type Session = async_imap::Session<tokio_native_tls::TlsStream<TcpStream>>;

pub enum Request {
    Refresh(String),
    Load(String),
    Body(Message),
    Search {
        folder: String,
        query: String,
    },
    Flag {
        message: Message,
        starred: bool,
        value: bool,
    },
    Move {
        message: Message,
        role: String,
    },
    Send {
        draft: Draft,
        message: Box<lettre::Message>,
    },
}

pub enum Update {
    Folders(Vec<Folder>),
    Messages {
        folder: String,
        messages: Vec<Message>,
    },
    Search {
        folder: String,
        query: String,
        messages: Vec<Message>,
    },
    Body(Box<Message>),
    BodyError {
        message: Box<Message>,
        error: String,
    },
    Status(String),
    Error(String),
    Sent {
        path: PathBuf,
        warning: Option<String>,
    },
}

pub struct Worker {
    pub requests: mpsc::Sender<Request>,
    pub updates: mpsc::UnboundedReceiver<Update>,
}

pub fn start(config: Config, password: String, path: PathBuf) -> Result<Worker> {
    let cache = Cache::open(&path)?;
    let backend = ImapSmtp::new(config, password)?;
    let (requests, receiver) = mpsc::channel(32);
    let (sender, updates) = mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("mail-worker".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = sender.send(Update::Error(error.to_string()));
                    return;
                }
            };
            runtime.block_on(async move {
                let mut worker = MailWorker {
                    cache,
                    backend,
                    sender,
                    jobs: VecDeque::new(),
                    selected: None,
                };
                if let Ok(folders) = worker.cache.folders() {
                    worker.emit(Update::Folders(folders));
                }
                worker.run(receiver).await;
            });
        })?;
    Ok(Worker { requests, updates })
}

/// Protocol boundary used by the worker and deterministic fake-backend tests.
trait MailBackend {
    async fn folders(&mut self) -> Result<Vec<Folder>>;
    async fn unread(&mut self, folder: &str) -> Result<u32>;
    fn disconnect(&mut self);
    async fn select(&mut self, folder: &str, expected: Option<u32>) -> Result<u32>;
    async fn uids(&mut self) -> Result<HashSet<u32>>;
    async fn fetch(
        &mut self,
        folder: &str,
        validity: u32,
        uids: &[u32],
        body: bool,
    ) -> Result<Vec<Message>>;
    async fn flag(&mut self, uid: u32, starred: bool, value: bool) -> Result<()>;
    async fn move_to(&mut self, uid: u32, destination: &str) -> Result<()>;
    async fn send(&mut self, message: &lettre::Message) -> Result<()>;
    async fn append_sent(&mut self, message: &lettre::Message, folders: &[Folder]) -> Result<()>;
    fn destination(&self, role: &str, folders: &[Folder]) -> Result<String>;
}

struct ImapSmtp {
    config: Config,
    password: String,
    session: Option<Session>,
    tls_connector: native_tls::TlsConnector,
}

impl ImapSmtp {
    fn new(config: Config, password: String) -> Result<Self> {
        Ok(Self {
            config,
            password,
            session: None,
            tls_connector: native_tls::TlsConnector::new()?,
        })
    }

    async fn session(&mut self) -> Result<&mut Session> {
        if self.password.is_empty() {
            bail!("Offline: no password supplied. Cached messages can still be read and searched.");
        }
        if self.session.is_none() {
            let server = &self.config.account.imap;
            let tcp = TcpStream::connect((server.host.as_str(), server.port))
                .await
                .context("Cannot connect to IMAP")?;
            let connector = tokio_native_tls::TlsConnector::from(self.tls_connector.clone());
            let client = match server.tls {
                TlsMode::Implicit => {
                    let stream = connector
                        .connect(&server.host, tcp)
                        .await
                        .context("IMAP TLS handshake failed")?;
                    let mut client = async_imap::Client::new(stream);
                    client
                        .read_response()
                        .await?
                        .context("Missing IMAP greeting")?;
                    client
                }
                TlsMode::Starttls => {
                    let mut client = async_imap::Client::new(tcp);
                    client
                        .read_response()
                        .await?
                        .context("Missing IMAP greeting")?;
                    client
                        .run_command_and_check_ok("STARTTLS", None)
                        .await
                        .context("IMAP server refused STARTTLS")?;
                    let stream = connector
                        .connect(&server.host, client.into_inner())
                        .await
                        .context("IMAP STARTTLS handshake failed")?;
                    async_imap::Client::new(stream)
                }
            };
            self.session = Some(
                client
                    .login(&self.config.account.username, &self.password)
                    .await
                    .map_err(|(error, _)| anyhow::anyhow!("IMAP authentication failed: {error}"))?,
            );
        }
        Ok(self.session.as_mut().unwrap())
    }
}

impl MailBackend for ImapSmtp {
    async fn folders(&mut self) -> Result<Vec<Folder>> {
        let session = self.session().await?;
        let names: Vec<_> = session.list(None, Some("*")).await?.try_collect().await?;
        let mut folders = Vec::new();
        for name in names {
            let flags = name.attributes();
            if flags.contains(&NameAttribute::NoSelect) {
                continue;
            }
            let role = [
                (NameAttribute::Archive, "archive"),
                (NameAttribute::Trash, "trash"),
                (NameAttribute::Sent, "sent"),
            ]
            .into_iter()
            .find(|(attribute, _)| flags.contains(attribute))
            .map(|(_, role)| role.to_owned());
            folders.push(Folder {
                name: if name.name().eq_ignore_ascii_case("INBOX") {
                    "INBOX".into()
                } else {
                    name.name().into()
                },
                unread: 0,
                role,
            });
        }
        folders.sort_by_key(|f| (!f.name.eq_ignore_ascii_case("INBOX"), f.name.clone()));
        Ok(folders)
    }

    async fn unread(&mut self, folder: &str) -> Result<u32> {
        Ok(self
            .session()
            .await?
            .status(folder, "(UNSEEN)")
            .await?
            .unseen
            .unwrap_or(0))
    }

    fn disconnect(&mut self) {
        self.session = None;
    }

    async fn select(&mut self, folder: &str, expected: Option<u32>) -> Result<u32> {
        let mailbox = self.session().await?.select(folder).await?;
        let validity = mailbox
            .uid_validity
            .context("IMAP server omitted UIDVALIDITY")?;
        if expected.is_some_and(|old| old != validity) {
            bail!("Mailbox identity changed. Refresh before acting on this message.");
        }
        Ok(validity)
    }

    async fn uids(&mut self) -> Result<HashSet<u32>> {
        Ok(self.session().await?.uid_search("UNDELETED").await?)
    }

    async fn fetch(
        &mut self,
        folder: &str,
        validity: u32,
        uids: &[u32],
        body: bool,
    ) -> Result<Vec<Message>> {
        if uids.is_empty() {
            return Ok(vec![]);
        }
        let set = uids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let query = if body {
            "(UID FLAGS BODY.PEEK[])"
        } else {
            "(UID FLAGS BODY.PEEK[HEADER])"
        };
        let fetches: Vec<_> = self
            .session()
            .await?
            .uid_fetch(&set, query)
            .await?
            .try_collect()
            .await?;
        let mut messages = Vec::new();
        for fetched in fetches {
            let Some(uid) = fetched.uid.filter(|uid| uids.contains(uid)) else {
                continue;
            };
            let bytes = if body {
                fetched.body()
            } else {
                fetched.header()
            };
            let Some(bytes) = bytes else {
                continue;
            };
            let mut message = parse_message(bytes, uid, folder, validity);
            if !body {
                message.body = None;
                message.html_body = None;
                message.html_loaded = false;
                message.attachments.clear();
            } else {
                message.html_loaded = true;
                if message.body.is_none() {
                    message.body = Some("(No readable text body)".into());
                }
            }
            message.seen = fetched.flags().any(|f| f == Flag::Seen);
            message.starred = fetched.flags().any(|f| f == Flag::Flagged);
            messages.push(message);
        }
        Ok(messages)
    }

    async fn flag(&mut self, uid: u32, starred: bool, value: bool) -> Result<()> {
        let flag = if starred { "\\Flagged" } else { "\\Seen" };
        let op = if value {
            "+FLAGS.SILENT"
        } else {
            "-FLAGS.SILENT"
        };
        let _: Vec<_> = self
            .session()
            .await?
            .uid_store(uid.to_string(), format!("{op} ({flag})"))
            .await?
            .try_collect()
            .await?;
        Ok(())
    }

    async fn move_to(&mut self, uid: u32, destination: &str) -> Result<()> {
        let session = self.session().await?;
        let capabilities = session.capabilities().await?;
        let uid = uid.to_string();
        if capabilities.has_str("MOVE") {
            session.uid_mv(&uid, destination).await?;
        } else if capabilities.has_str("UIDPLUS") {
            session.uid_copy(&uid, destination).await?;
            let _: Vec<_> = session
                .uid_store(&uid, "+FLAGS.SILENT (\\Deleted)")
                .await?
                .try_collect()
                .await?;
            let _: Vec<_> = session.uid_expunge(&uid).await?.try_collect().await?;
        } else {
            bail!("Moving mail requires the server to support MOVE or UIDPLUS");
        }
        Ok(())
    }

    async fn send(&mut self, message: &lettre::Message) -> Result<()> {
        if self.password.is_empty() {
            bail!("Sending requires a password and a connection");
        }
        let server = &self.config.account.smtp;
        let builder = match server.tls {
            TlsMode::Implicit => AsyncSmtpTransport::<Tokio1Executor>::relay(&server.host)?,
            TlsMode::Starttls => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&server.host)?
            }
        };
        builder
            .port(server.port)
            .credentials(Credentials::new(
                self.config.account.username.clone(),
                self.password.clone(),
            ))
            .timeout(Some(Duration::from_secs(45)))
            .build()
            .send(message.clone())
            .await
            .context(
                "SMTP send failed; check delivery before retrying if the connection was lost",
            )?;
        Ok(())
    }

    async fn append_sent(&mut self, message: &lettre::Message, folders: &[Folder]) -> Result<()> {
        let folder = self.destination("sent", folders)?;
        self.session()
            .await?
            .append(&folder, Some("(\\Seen)"), None, message.formatted())
            .await?;
        Ok(())
    }

    fn destination(&self, role: &str, folders: &[Folder]) -> Result<String> {
        let account = &self.config.account;
        let explicit = match role {
            "archive" => &account.archive_folder,
            "trash" => &account.trash_folder,
            "sent" => &account.sent_folder,
            _ => bail!("Unknown folder role"),
        };
        explicit
            .clone()
            .or_else(|| {
                folders
                    .iter()
                    .find(|f| f.role.as_deref() == Some(role))
                    .map(|f| f.name.clone())
            })
            .with_context(|| {
                format!("No {role} folder found. Set account.{role}_folder in config.toml")
            })
    }
}

struct MailWorker<B> {
    cache: Cache,
    backend: B,
    sender: mpsc::UnboundedSender<Update>,
    jobs: VecDeque<SyncJob>,
    selected: Option<(String, u32)>,
}

struct SyncJob {
    folder: String,
    phase: SyncPhase,
    validity: Option<u32>,
}

enum SyncPhase {
    Discover,
    Counts { folders: Vec<Folder>, next: usize },
    Select,
    Uids,
    Headers(VecDeque<Vec<u32>>),
}

impl<B: MailBackend> MailWorker<B> {
    fn emit(&self, update: Update) {
        let _ = self.sender.send(update);
    }
    fn cached(&self, folder: &str) -> Result<()> {
        self.emit(Update::Messages {
            folder: folder.into(),
            messages: self.cache.messages(folder)?,
        });
        Ok(())
    }
    fn schedule(&mut self, folder: String, refresh: bool) -> Result<()> {
        self.cached(&folder)?;
        if let Some(job) = self.jobs.iter_mut().find(|job| job.folder == folder) {
            // A pending discovery already includes the requested synchronization.
            if refresh && matches!(job.phase, SyncPhase::Select) {
                job.phase = SyncPhase::Discover;
            }
            return Ok(());
        }
        self.emit(Update::Status(format!(
            "Synchronizing {}…",
            safe_text(&folder)
        )));
        self.jobs.push_front(SyncJob {
            folder,
            phase: if refresh {
                SyncPhase::Discover
            } else {
                SyncPhase::Select
            },
            validity: None,
        });
        Ok(())
    }

    async fn run(&mut self, mut receiver: mpsc::Receiver<Request>) {
        let mut pending = VecDeque::new();
        loop {
            // Drain first so interactive work overtakes background refreshes.
            while let Ok(request) = receiver.try_recv() {
                self.enqueue(request, &mut pending);
            }
            if let Some(request) = pending.pop_front() {
                let body = match &request {
                    Request::Body(m) => Some(m.clone()),
                    _ => None,
                };
                // Foreground commands may change the selected mailbox.
                self.selected = None;
                let result =
                    tokio::time::timeout(Duration::from_secs(120), self.handle(request)).await;
                let error = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(safe_text(&format!("{error:#}"))),
                    Err(_) => Some(if body.is_some() {
                        "Message download timed out.".into()
                    } else {
                        "Request timed out. Cached mail remains available. A mutation or send may have reached the server; check before retrying.".into()
                    }),
                };
                // Requests arriving during an active download share its result, including failure.
                while let Ok(request) = receiver.try_recv() {
                    if let (Some(active), Request::Body(other)) = (&body, &request)
                        && active.folder == other.folder
                        && active.uid_validity == other.uid_validity
                        && active.uid == other.uid
                    {
                        continue;
                    }
                    self.enqueue(request, &mut pending);
                }
                if let Some(error) = error {
                    self.backend.disconnect();
                    if let Some(message) = body {
                        self.emit(Update::BodyError {
                            message: Box::new(message),
                            error,
                        });
                    } else {
                        self.emit(Update::Error(error));
                    }
                }
            } else if let Some(job) = self.jobs.pop_front() {
                match tokio::time::timeout(Duration::from_secs(30), self.advance(job)).await {
                    Ok(Ok(Some(job))) => self.jobs.push_front(job),
                    Ok(Ok(None)) => {}
                    result => {
                        self.backend.disconnect();
                        self.selected = None;
                        let error = match result {
                            Ok(Err(error)) => safe_text(&format!("{error:#}")),
                            _ => "Synchronization step timed out. Cached mail remains available."
                                .into(),
                        };
                        self.emit(Update::Error(error));
                    }
                }
            } else {
                match receiver.recv().await {
                    Some(request) => self.enqueue(request, &mut pending),
                    None => break,
                }
            }
        }
    }

    fn enqueue(&mut self, request: Request, pending: &mut VecDeque<Request>) {
        match request {
            Request::Refresh(folder) => {
                if let Err(error) = self.schedule(folder, true) {
                    self.emit(Update::Error(error.to_string()));
                }
            }
            Request::Load(folder) => {
                if let Err(error) = self.schedule(folder, false) {
                    self.emit(Update::Error(error.to_string()));
                }
            }
            Request::Body(ref message)
                if pending.iter().any(|request| {
                    matches!(request, Request::Body(other) if other.folder == message.folder
                    && other.uid_validity == message.uid_validity && other.uid == message.uid)
                }) => {}
            request => pending.push_back(request),
        }
    }

    async fn advance(&mut self, mut job: SyncJob) -> Result<Option<SyncJob>> {
        // Restore selection in its own step after foreground work or another job.
        if let Some(validity) = job.validity
            && self.selected.as_ref() != Some(&(job.folder.clone(), validity))
        {
            self.backend.select(&job.folder, Some(validity)).await?;
            self.selected = Some((job.folder.clone(), validity));
            return Ok(Some(job));
        }
        job.phase = match job.phase {
            SyncPhase::Discover => {
                let folders = self.backend.folders().await?;
                self.cache.set_folders(&folders)?;
                self.emit(Update::Folders(folders.clone()));
                SyncPhase::Counts { folders, next: 0 }
            }
            SyncPhase::Counts { mut folders, next } => {
                if next < folders.len() {
                    folders[next].unread = self.backend.unread(&folders[next].name).await?;
                    self.cache.set_folders(&folders)?;
                    self.emit(Update::Folders(folders.clone()));
                    SyncPhase::Counts {
                        folders,
                        next: next + 1,
                    }
                } else {
                    SyncPhase::Select
                }
            }
            SyncPhase::Select => {
                let validity = self.backend.select(&job.folder, None).await?;
                self.cache.check_validity(&job.folder, validity)?;
                self.cached(&job.folder)?;
                job.validity = Some(validity);
                self.selected = Some((job.folder.clone(), validity));
                SyncPhase::Uids
            }
            SyncPhase::Uids => {
                let remote = self.backend.uids().await?;
                self.cache.reconcile(&job.folder, &remote)?;
                self.cached(&job.folder)?;
                let mut uids: Vec<_> = remote.into_iter().collect();
                uids.sort_unstable_by(|a, b| b.cmp(a));
                SyncPhase::Headers(uids.chunks(100).map(<[u32]>::to_vec).collect())
            }
            SyncPhase::Headers(mut chunks) => {
                let Some(chunk) = chunks.pop_front() else {
                    self.emit(Update::Status(
                        "Synchronized · / searches cached mail · ? help".into(),
                    ));
                    return Ok(None);
                };
                let validity = job
                    .validity
                    .context("Missing synchronization mailbox identity")?;
                let updates = self
                    .backend
                    .fetch(&job.folder, validity, &chunk, false)
                    .await?;
                for mut message in updates {
                    if let Some(old) = self.cache.message(&job.folder, message.uid)?
                        && old.uid_validity == validity
                    {
                        message.body = old.body;
                        message.html_body = old.html_body;
                        message.html_loaded = old.html_loaded;
                        message.attachments = old.attachments;
                    }
                    self.cache.put(&message)?;
                }
                self.cached(&job.folder)?;
                SyncPhase::Headers(chunks)
            }
        };
        Ok(Some(job))
    }

    async fn handle(&mut self, request: Request) -> Result<()> {
        match request {
            Request::Refresh(folder) => self.schedule(folder, true)?,
            Request::Load(folder) => self.schedule(folder, false)?,
            Request::Search { folder, query } => self.emit(Update::Search {
                messages: self.cache.search(&folder, &query)?,
                folder,
                query,
            }),
            Request::Body(message) => {
                if let Some(cached) =
                    self.cache
                        .message(&message.folder, message.uid)?
                        .filter(|m| {
                            m.uid_validity == message.uid_validity
                                && m.body.is_some()
                                && m.html_loaded
                        })
                {
                    self.emit(Update::Body(Box::new(cached)));
                    return Ok(());
                }
                self.backend
                    .select(&message.folder, Some(message.uid_validity))
                    .await?;
                let body = self
                    .backend
                    .fetch(&message.folder, message.uid_validity, &[message.uid], true)
                    .await?
                    .into_iter()
                    .next()
                    .context("Message no longer exists; refresh the mailbox")?;
                self.cache.put(&body)?;
                self.emit(Update::Body(Box::new(body)));
            }
            Request::Flag {
                mut message,
                starred,
                value,
            } => {
                self.backend
                    .select(&message.folder, Some(message.uid_validity))
                    .await?;
                self.backend.flag(message.uid, starred, value).await?;
                if let Some(cached) = self.cache.message(&message.folder, message.uid)? {
                    message = cached;
                }
                if starred {
                    message.starred = value;
                } else {
                    message.seen = value;
                }
                self.cache.put(&message)?;
                self.emit(Update::Body(Box::new(message.clone())));
                self.cached(&message.folder)?;
                self.emit(Update::Status("Flag updated".into()));
            }
            Request::Move { message, role } => {
                let destination = self.backend.destination(&role, &self.cache.folders()?)?;
                if message.folder == destination {
                    bail!("Message is already in {destination}");
                }
                self.backend
                    .select(&message.folder, Some(message.uid_validity))
                    .await?;
                self.backend.move_to(message.uid, &destination).await?;
                // Drop a paused UID snapshot which predates this mutation.
                self.jobs.retain(|job| job.folder != message.folder);
                self.schedule(message.folder, false)?;
            }
            Request::Send { draft, message } => {
                self.emit(Update::Status("Sending message…".into()));
                self.backend
                    .send(&message)
                    .await
                    .with_context(|| format!("Draft retained at {}", draft.path.display()))?;
                // Once SMTP accepted the message, a Sent failure must never become a send failure.
                let sent_copy = match self.cache.folders() {
                    Ok(folders) => match tokio::time::timeout(
                        Duration::from_secs(30),
                        self.backend.append_sent(&message, &folders),
                    )
                    .await
                    {
                        Ok(result) => result,
                        Err(_) => Err(anyhow::anyhow!("Saving the Sent copy timed out")),
                    },
                    Err(error) => Err(error),
                };
                let warning = sent_copy.err().map(|error| format!("Delivered, but could not save a Sent copy: {error:#}. Draft retained at {}. Do not resend.", draft.path.display()));
                if warning.is_none() {
                    let _ = std::fs::remove_file(&draft.path);
                }
                self.emit(Update::Sent {
                    path: draft.path,
                    warning,
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
