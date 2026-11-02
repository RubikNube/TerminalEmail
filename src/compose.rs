use crate::{
    config::{Config, private_dir},
    model::{Message, safe_text},
};
use anyhow::{Context, Result, bail};
use lettre::{
    Message as Outgoing,
    message::{Attachment, Mailbox, Mailboxes, MultiPart, SinglePart, header::ContentType},
};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug)]
pub enum ComposeKind {
    New,
    Reply,
    ReplyAll,
    Forward,
}

#[derive(Clone, Debug)]
pub struct Draft {
    pub path: PathBuf,
    pub kind: ComposeKind,
    pub original: Option<Message>,
}

pub struct PreparedDraft {
    pub message: Outgoing,
    pub preview: String,
}

impl Draft {
    pub fn create(config: &Config, kind: ComposeKind, original: Option<&Message>) -> Result<Self> {
        let base = directories::BaseDirs::new().context("Cannot determine user directories")?;
        let directory = base.data_local_dir().join("terminal-email/drafts");
        private_dir(&directory)?;
        let file = tempfile::Builder::new()
            .prefix("draft-")
            .suffix(".eml")
            .tempfile_in(&directory)?;
        let (mut file, path) = file.keep()?;
        let (mut to, mut cc, mut subject, mut body) =
            (String::new(), String::new(), String::new(), String::new());
        if let Some(message) = original.filter(|_| !matches!(kind, ComposeKind::New)) {
            subject = message.subject.replace(['\n', '\r'], " ");
            match kind {
                ComposeKind::Reply | ComposeKind::ReplyAll => {
                    to = if message.reply_to.is_empty() {
                        &message.from
                    } else {
                        &message.reply_to
                    }
                    .replace(['\n', '\r'], " ");
                    if matches!(kind, ComposeKind::ReplyAll) {
                        let mut seen = HashSet::from([config.account.email.to_lowercase()]);
                        let mut recipients = Vec::new();
                        for list in [&to, &message.to, &message.cc] {
                            if let Ok(parsed) = list.parse::<Mailboxes>() {
                                for address in parsed {
                                    if seen.insert(address.email.to_string().to_lowercase()) {
                                        recipients.push(address.to_string());
                                    }
                                }
                            }
                        }
                        to = recipients.first().cloned().unwrap_or_default();
                        cc = recipients
                            .into_iter()
                            .skip(1)
                            .collect::<Vec<_>>()
                            .join(", ");
                    }
                    if !subject.to_lowercase().starts_with("re:") {
                        subject = format!("Re: {subject}");
                    }
                    body = format!(
                        "\n\nOn {}, {} wrote:\n{}",
                        message.date.replace(['\n', '\r'], " "),
                        message.from.replace(['\n', '\r'], " "),
                        message
                            .body
                            .as_deref()
                            .unwrap_or_default()
                            .lines()
                            .map(|line| format!("> {line}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    );
                }
                ComposeKind::Forward => {
                    if !subject.to_lowercase().starts_with("fwd:") {
                        subject = format!("Fwd: {subject}");
                    }
                    body = format!(
                        "\n\n---------- Forwarded message ----------\nFrom: {}\nDate: {}\nSubject: {}\nTo: {}\n\n{}",
                        message.from,
                        message.date,
                        message.subject,
                        message.to,
                        message.body.as_deref().unwrap_or_default()
                    );
                }
                ComposeKind::New => {}
            }
        }
        use std::io::Write;
        write!(
            file,
            "To: {to}\nCc: {cc}\nBcc: \nSubject: {subject}\nAttach: \n\n{body}"
        )?;
        file.sync_all()?;
        Ok(Self {
            path,
            kind,
            original: original.cloned(),
        })
    }

    pub fn edit(&self, config: &Config) -> Result<()> {
        crate::editor::launch(config, &self.path)
            .with_context(|| format!("Draft retained at {}", self.path.display()))
    }

    pub fn prepare(&self, config: &Config) -> Result<PreparedDraft> {
        prepare(
            &std::fs::read_to_string(&self.path)?,
            config,
            self.kind,
            self.original.as_ref(),
        )
    }
}

pub fn prepare(
    text: &str,
    config: &Config,
    kind: ComposeKind,
    original: Option<&Message>,
) -> Result<PreparedDraft> {
    let normalized = text.replace("\r\n", "\n");
    let (headers, body) = normalized
        .split_once("\n\n")
        .context("Draft needs a blank line between headers and body")?;
    let mut fields = std::collections::HashMap::new();
    let mut attachment_paths = Vec::new();
    let mut preview_headers = Vec::new();
    for line in headers.lines() {
        let (name, value) = line
            .split_once(':')
            .context("Expected Header: value in draft")?;
        let name = name.trim().to_lowercase();
        if name == "attach" {
            if !value.trim().is_empty() {
                attachment_paths.push(value.trim());
            }
            continue;
        }
        if !["to", "cc", "bcc", "subject"].contains(&name.as_str()) {
            bail!("Unsupported draft header: {name}");
        }
        if fields.insert(name.clone(), value.trim()).is_some() {
            bail!("Duplicate draft header: {name}");
        }
        preview_headers.push(line);
    }
    let from = Mailbox::new(
        if config.account.display_name.is_empty() {
            None
        } else {
            Some(config.account.display_name.clone())
        },
        config.account.email.parse()?,
    );
    let mut builder = Outgoing::builder().from(from).message_id(None).date_now();
    let mut count = 0;
    for name in ["to", "cc", "bcc"] {
        if let Some(value) = fields.get(name).filter(|v| !v.is_empty()) {
            let addresses: Mailboxes = value
                .parse()
                .with_context(|| format!("Invalid {name} address list"))?;
            for address in addresses {
                count += 1;
                builder = match name {
                    "cc" => builder.cc(address),
                    "bcc" => builder.bcc(address),
                    _ => builder.to(address),
                };
            }
        }
    }
    if count == 0 {
        bail!("At least one recipient is required");
    }
    let subject = fields.get("subject").copied().unwrap_or_default();
    if subject.is_empty() {
        bail!("Subject cannot be empty");
    }
    builder = builder.subject(subject);
    if matches!(kind, ComposeKind::Reply | ComposeKind::ReplyAll)
        && let Some(message) = original.filter(|m| !m.message_id.is_empty())
    {
        builder = builder.in_reply_to(format!("<{}>", message.message_id));
        let mut refs = message.references.clone();
        refs.push(message.message_id.clone());
        refs.dedup();
        builder = builder.references(
            refs.iter()
                .map(|id| format!("<{id}>"))
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    let mut preview = preview_headers.join("\n");
    let message = if attachment_paths.is_empty() {
        builder
            .header(ContentType::TEXT_PLAIN)
            .body(body.to_owned())?
    } else {
        let mut multipart = MultiPart::mixed().singlepart(SinglePart::plain(body.to_owned()));
        preview.push_str("\n\nAttachments:");
        for input in attachment_paths {
            let path = crate::path_completion::expand_home(
                input,
                crate::path_completion::home_dir().as_deref(),
            )
            .with_context(|| format!("Invalid attachment path: {input}"))?;
            let metadata = std::fs::metadata(&path)
                .with_context(|| format!("Cannot access attachment: {input}"))?;
            if !metadata.is_file() {
                bail!("Attachment must be a regular file: {input}");
            }
            let data =
                std::fs::read(&path).with_context(|| format!("Cannot read attachment: {input}"))?;
            let filename = safe_filename(
                &path
                    .file_name()
                    .context("Attachment needs a filename")?
                    .to_string_lossy(),
            );
            preview.push_str(&format!("\n  {filename} ({} bytes)", data.len()));
            multipart = multipart.singlepart(
                Attachment::new(filename)
                    .body(data, ContentType::parse("application/octet-stream")?),
            );
        }
        builder.multipart(multipart)?
    };
    preview.push_str("\n\n");
    preview.push_str(body);
    Ok(PreparedDraft { message, preview })
}

pub fn safe_filename(name: &str) -> String {
    let name = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = safe_text(name)
        .chars()
        .map(|c| {
            if matches!(c, '\n' | '\t' | ':' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "attachment".into()
    } else {
        cleaned
    }
}

pub fn save_attachment(path: &Path, data: &[u8], overwrite: bool) -> Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(data)?;
    temporary.as_file().sync_all()?;
    if overwrite {
        temporary.persist(path)?;
    } else {
        temporary.persist_noclobber(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mail_parser::MimeHeaders;

    fn config() -> Config {
        toml::from_str(include_str!("../config.example.toml")).unwrap()
    }

    #[test]
    fn attachments_round_trip_in_order_and_snapshot_contents() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("résumé photo.bin");
        let second = temp.path().join("empty.txt");
        let binary = [0, 255, 128, 13, 10, 42];
        std::fs::write(&first, binary).unwrap();
        std::fs::write(&second, []).unwrap();
        let text = format!(
            "To: a@example.org\nBcc: hidden@example.org\nSubject: Files\nAttach: {}\naTtAcH: \nAttach: {}\n\nHello\nAttach: body text",
            first.display(),
            second.display()
        );
        let prepared = prepare(
            &text,
            &config(),
            ComposeKind::Reply,
            Some(&Message {
                message_id: "parent@test".into(),
                ..Default::default()
            }),
        )
        .unwrap();
        std::fs::write(&first, b"changed").unwrap();
        std::fs::remove_file(&second).unwrap();
        let raw = prepared.message.formatted();
        let wire = String::from_utf8_lossy(&raw);
        assert!(wire.contains("multipart/mixed"));
        assert!(wire.contains("In-Reply-To: <parent@test>"));
        assert!(!wire.contains("Bcc:"));
        assert!(!wire.contains(&temp.path().display().to_string()));
        assert!(!wire.contains("aTtAcH:"));
        assert_eq!(prepared.message.envelope().to().len(), 2);
        let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
        assert_eq!(parsed.body_text(0).unwrap(), "Hello\r\nAttach: body text");
        let attachments: Vec<_> = parsed.attachments().collect();
        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0].attachment_name(), Some("résumé photo.bin"));
        assert_eq!(attachments[0].contents(), binary);
        assert_eq!(attachments[1].attachment_name(), Some("empty.txt"));
        assert!(attachments[1].contents().is_empty());
        assert!(wire.contains("application/octet-stream"));
        assert!(wire.contains("Content-Disposition: attachment"));
        assert!(prepared.preview.contains("résumé photo.bin (6 bytes)"));
        assert!(prepared.preview.contains("empty.txt (0 bytes)"));
        assert!(
            prepared.preview.find("Attachments:").unwrap()
                < prepared.preview.find("Hello").unwrap()
        );
        assert!(
            !prepared
                .preview
                .contains(&temp.path().display().to_string())
        );
    }

    #[test]
    fn empty_attachments_keep_plain_text_and_header_validation() {
        let prepared = prepare(
            "To: a@example.org\r\nSubject: Hi\r\nAttach: \r\nAttach:\r\n\r\nBody",
            &config(),
            ComposeKind::New,
            None,
        )
        .unwrap();
        let raw = String::from_utf8(prepared.message.formatted()).unwrap();
        assert!(raw.contains("Content-Type: text/plain"));
        assert!(!raw.contains("multipart/"));
        assert!(!raw.contains("Attach:"));
        assert_eq!(prepared.preview, "To: a@example.org\nSubject: Hi\n\nBody");
        for headers in ["Subject: Hi\nSubject: Again", "Subject: Hi\nUnknown: value"] {
            assert!(
                prepare(
                    &format!("To: a@example.org\n{headers}\n\nBody"),
                    &config(),
                    ComposeKind::New,
                    None
                )
                .is_err()
            );
        }
    }

    #[test]
    fn invalid_attachment_retains_draft_and_reports_path() {
        let temp = tempfile::tempdir().unwrap();
        let draft = Draft {
            path: temp.path().join("draft.eml"),
            kind: ComposeKind::New,
            original: None,
        };
        for path in [temp.path().join("missing"), temp.path().to_path_buf()] {
            let text = format!(
                "To: a@example.org\nSubject: Hi\nAttach: {}\n\nBody",
                path.display()
            );
            std::fs::write(&draft.path, &text).unwrap();
            let error = draft.prepare(&config()).err().unwrap();
            assert!(format!("{error:#}").contains(&path.display().to_string()));
            assert_eq!(std::fs::read_to_string(&draft.path).unwrap(), text);
        }
    }

    #[test]
    fn attaches_relative_path_from_working_directory() {
        let cwd = std::env::current_dir().unwrap();
        let temp = tempfile::tempdir_in(&cwd).unwrap();
        let path = temp.path().join("literal $file 'quoted'.txt");
        std::fs::write(&path, b"relative").unwrap();
        let relative = path.strip_prefix(&cwd).unwrap();
        let prepared = prepare(
            &format!(
                "To: a@example.org\nSubject: Hi\nAttach: {}\n\nBody",
                relative.display()
            ),
            &config(),
            ComposeKind::New,
            None,
        )
        .unwrap();
        let raw = prepared.message.formatted();
        let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
        assert_eq!(parsed.attachments().next().unwrap().contents(), b"relative");
    }

    #[test]
    fn draft_validation_and_bcc_privacy() {
        let config: Config = toml::from_str(include_str!("../config.example.toml")).unwrap();
        assert!(
            prepare(
                "To: bad\nSubject: Hi\n\nBody",
                &config,
                ComposeKind::New,
                None
            )
            .is_err()
        );
        assert!(prepare("To: \nSubject: Hi\n\nBody", &config, ComposeKind::New, None).is_err());
        let msg = prepare(
            "To: a@example.org\nBcc: hidden@example.org\nSubject: Hello\n\nBody",
            &config,
            ComposeKind::Reply,
            Some(&Message {
                message_id: "a@test".into(),
                ..Default::default()
            }),
        )
        .unwrap();
        let raw = String::from_utf8(msg.message.formatted()).unwrap();
        assert!(!raw.contains("Bcc:"));
        assert!(raw.contains("In-Reply-To: <a@test>"));
        assert_eq!(msg.message.envelope().to().len(), 2);
    }
    #[test]
    fn sanitizes_filenames_and_prevents_overwrite() {
        assert_eq!(safe_filename("../../evil.txt"), "evil.txt");
        assert_eq!(safe_filename("C:\\folder\\file.pdf"), "file.pdf");
        assert_eq!(safe_filename(".."), "attachment");
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("file");
        save_attachment(&path, b"one", false).unwrap();
        assert!(save_attachment(&path, b"two", false).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"one");
        save_attachment(&path, b"two", true).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
    }
}
