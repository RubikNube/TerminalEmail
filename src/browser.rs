use anyhow::{Context, Result, bail};
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc,
};

/// Owns private HTML files until the app exits. Launcher completion is polled
/// separately so a desktop opener cannot block terminal input.
pub struct Browser {
    directory: Option<tempfile::TempDir>,
    launcher: PathBuf,
    sender: mpsc::Sender<Result<()>>,
    receiver: mpsc::Receiver<Result<()>>,
}

impl Default for Browser {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            directory: None,
            launcher: "xdg-open".into(),
            sender,
            receiver,
        }
    }
}

impl Browser {
    pub fn open(&mut self, html: &str) -> Result<()> {
        if self.directory.is_none() {
            let directory = tempfile::Builder::new()
                .prefix("terminal-email-")
                .tempdir()?;
            crate::config::private_dir(directory.path())?;
            self.directory = Some(directory);
        }
        let mut file = tempfile::Builder::new()
            .prefix("message-")
            .suffix(".html")
            .tempfile_in(self.directory.as_ref().unwrap().path())?;
        // MIME decoding produces UTF-8, even when the original charset differs.
        file.write_all(b"<!doctype html><meta charset=\"utf-8\">\n")?;
        file.write_all(html.as_bytes())?;
        file.flush()?;
        let (_, path) = file.keep()?;
        let mut child = Command::new(&self.launcher)
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("Cannot launch default browser (xdg-open)")?;
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let status = child.wait().context("Cannot wait for browser launcher")?;
                if !status.success() {
                    bail!("Browser launcher failed: {status}");
                }
                Ok(())
            })();
            let _ = sender.send(result);
        });
        Ok(())
    }

    pub fn poll(&self) -> Option<Result<()>> {
        self.receiver.try_recv().ok()
    }

    #[cfg(test)]
    pub fn file_count(&self) -> usize {
        self.directory.as_ref().map_or(0, |directory| {
            std::fs::read_dir(directory.path()).unwrap().count()
        })
    }
}

#[cfg(test)]
pub fn fake() -> Browser {
    Browser {
        launcher: "/bin/true".into(),
        ..Browser::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_files_survive_launch_and_are_removed_on_drop() {
        let mut browser = Browser {
            launcher: "/bin/true".into(),
            ..Browser::default()
        };
        browser.open("<p>Grüße</p>").unwrap();
        assert!(browser.receiver.recv().unwrap().is_ok());
        let directory = browser.directory.as_ref().unwrap().path().to_owned();
        let path = std::fs::read_dir(&directory)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "<!doctype html><meta charset=\"utf-8\">\n<p>Grüße</p>"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        drop(browser);
        assert!(!directory.exists());
    }

    #[test]
    fn launcher_errors_are_reported() {
        let mut browser = Browser {
            launcher: "/bin/false".into(),
            ..Browser::default()
        };
        browser.open("<p>Hello</p>").unwrap();
        assert!(browser.receiver.recv().unwrap().is_err());
        browser.launcher = "/nonexistent/terminal-email-launcher".into();
        assert!(browser.open("<p>Hello</p>").is_err());
    }
}
