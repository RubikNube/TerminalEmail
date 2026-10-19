use crate::{
    config::Config,
    model::{Message, safe_text},
};
use anyhow::{Context, Result, bail};
use std::{io::Write, path::Path};

/// Run the configured editor directly, inheriting the released terminal.
pub fn launch(config: &Config, path: &Path) -> Result<()> {
    let command = config
        .editor
        .command
        .clone()
        .or_else(|| std::env::var("EDITOR").ok())
        .unwrap_or_else(|| "vi".into());
    let words = shell_words::split(&command).context("Invalid editor command")?;
    let executable = words.first().context("Editor command is empty")?;
    let status = std::process::Command::new(executable)
        .args(&words[1..])
        .arg(path)
        .status()
        .context("Cannot launch editor")?;
    if !status.success() {
        bail!("Editor exited with {status}");
    }
    Ok(())
}

/// The private directory also contains any backup files the editor creates.
pub struct MessageCopy {
    _directory: tempfile::TempDir,
    file: tempfile::NamedTempFile,
}

impl MessageCopy {
    pub fn create(message: &Message) -> Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("terminal-email-editor-")
            .tempdir()?;
        crate::config::private_dir(directory.path())?;
        let mut file = tempfile::Builder::new()
            .prefix("message-")
            .suffix(".eml")
            .tempfile_in(directory.path())?;
        for (name, value) in [
            ("From", &message.from),
            ("To", &message.to),
            ("Cc", &message.cc),
            ("Date", &message.date),
            ("Subject", &message.subject),
        ] {
            writeln!(
                file,
                "{name}: {}",
                safe_text(value).replace(['\n', '\r'], " ")
            )?;
        }
        writeln!(
            file,
            "\n{}",
            safe_text(
                message
                    .body
                    .as_deref()
                    .context("Message body is not downloaded")?
            )
        )?;
        file.flush()?;
        Ok(Self {
            _directory: directory,
            file,
        })
    }

    pub fn path(&self) -> &Path {
        self.file.path()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn copy_contains_readable_text_and_cleans_up_editor_backups() {
        let message = Message {
            from: "Sender <sender@example.org>".into(),
            to: "reader@example.org".into(),
            cc: "copy@example.org".into(),
            subject: "Hello\nInjected: header".into(),
            date: "2026-10-19".into(),
            body: Some("Hello, 世界\nSecond line".into()),
            html_body: Some("<b>HTML omitted</b>".into()),
            ..Default::default()
        };
        let copy = MessageCopy::create(&message).unwrap();
        let path = copy.path().to_owned();
        let directory = path.parent().unwrap().to_owned();
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "From: Sender <sender@example.org>\nTo: reader@example.org\nCc: copy@example.org\nDate: 2026-10-19\nSubject: Hello Injected: header\n\nHello, 世界\nSecond line\n"
        );
        std::fs::write(directory.join("message.eml~"), "backup").unwrap();
        drop(copy);
        assert!(!directory.exists());
        assert_eq!(message.body.as_deref(), Some("Hello, 世界\nSecond line"));
        assert!(MessageCopy::create(&Message::default()).is_err());
    }

    #[test]
    fn launcher_preserves_arguments_and_reports_failures() {
        let mut config: Config = toml::from_str(include_str!("../config.example.toml")).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("fake editor");
        std::fs::write(&script, "#!/bin/sh\n[ \"$1\" = 'argument with spaces' ] || exit 9\nprintf 'edited copy' > \"$2\"\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        config.editor.command = Some(format!("'{}' 'argument with spaces'", script.display()));
        let copy = MessageCopy::create(&Message {
            body: Some("original".into()),
            ..Default::default()
        })
        .unwrap();
        launch(&config, copy.path()).unwrap();
        assert_eq!(std::fs::read_to_string(copy.path()).unwrap(), "edited copy");
        config.editor.command = Some(format!("'{}' wrong-argument", script.display()));
        assert!(
            launch(&config, copy.path())
                .unwrap_err()
                .to_string()
                .contains("Editor exited")
        );
        config.editor.command = Some("/nonexistent/terminal-email-editor".into());
        assert!(launch(&config, copy.path()).is_err());
        config.editor.command = Some("'".into());
        assert!(launch(&config, copy.path()).is_err());
        config.editor.command = Some(String::new());
        assert!(launch(&config, copy.path()).is_err());
        let path = copy.path().to_owned();
        drop(copy);
        assert!(!path.exists());
    }
}
