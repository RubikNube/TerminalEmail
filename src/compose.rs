use crate::{
    config::{Config, private_dir},
    model::{Message, safe_text},
};
use anyhow::{Context, Result, bail};
use lettre::{
    Message as Outgoing,
    message::{Mailbox, Mailboxes, header::ContentType},
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
            "To: {to}\nCc: {cc}\nBcc: \nSubject: {subject}\n\n{body}"
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

    pub fn build(&self, config: &Config) -> Result<Outgoing> {
        build(
            &std::fs::read_to_string(&self.path)?,
            config,
            self.kind,
            self.original.as_ref(),
        )
    }
}

pub fn build(
    text: &str,
    config: &Config,
    kind: ComposeKind,
    original: Option<&Message>,
) -> Result<Outgoing> {
    let normalized = text.replace("\r\n", "\n");
    let (headers, body) = normalized
        .split_once("\n\n")
        .context("Draft needs a blank line between headers and body")?;
    let mut fields = std::collections::HashMap::new();
    for line in headers.lines() {
        let (name, value) = line
            .split_once(':')
            .context("Expected Header: value in draft")?;
        let name = name.trim().to_lowercase();
        if !["to", "cc", "bcc", "subject"].contains(&name.as_str()) {
            bail!("Unsupported draft header: {name}");
        }
        if fields.insert(name.clone(), value.trim()).is_some() {
            bail!("Duplicate draft header: {name}");
        }
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
    builder = builder.subject(subject).header(ContentType::TEXT_PLAIN);
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
    Ok(builder.body(body.to_owned())?)
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
    #[test]
    fn draft_validation_and_bcc_privacy() {
        let config: Config = toml::from_str(include_str!("../config.example.toml")).unwrap();
        assert!(
            build(
                "To: bad\nSubject: Hi\n\nBody",
                &config,
                ComposeKind::New,
                None
            )
            .is_err()
        );
        assert!(build("To: \nSubject: Hi\n\nBody", &config, ComposeKind::New, None).is_err());
        let msg = build(
            "To: a@example.org\nBcc: hidden@example.org\nSubject: Hello\n\nBody",
            &config,
            ComposeKind::Reply,
            Some(&Message {
                message_id: "a@test".into(),
                ..Default::default()
            }),
        )
        .unwrap();
        let raw = String::from_utf8(msg.formatted()).unwrap();
        assert!(!raw.contains("Bcc:"));
        assert!(raw.contains("In-Reply-To: <a@test>"));
        assert_eq!(msg.envelope().to().len(), 2);
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
