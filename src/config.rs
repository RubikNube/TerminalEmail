use crate::keys::{Bindings, KeyMap};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub account: Account,
    #[serde(default)]
    pub editor: Editor,
    #[serde(default)]
    pub display: Display,
    #[serde(default)]
    pub keybindings: KeyConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Display {
    pub theme: String,
    pub time_grouping: bool,
}

impl Default for Display {
    fn default() -> Self {
        Self {
            theme: "default".into(),
            time_grouping: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
    pub email: String,
    #[serde(default)]
    pub display_name: String,
    pub username: String,
    pub imap: Server,
    pub smtp: Server,
    pub archive_folder: Option<String>,
    pub trash_folder: Option<String>,
    pub sent_folder: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub host: String,
    pub port: u16,
    pub tls: TlsMode,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    Implicit,
    Starttls,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Editor {
    pub command: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyConfig {
    #[serde(default = "timeout")]
    pub timeout_ms: u64,
    #[serde(flatten)]
    pub contexts: Bindings,
}
fn timeout() -> u64 {
    750
}
impl Default for KeyConfig {
    fn default() -> Self {
        Self {
            timeout_ms: timeout(),
            contexts: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| {
            format!(
                "Cannot read {}. Run terminal-email init first.",
                path.display()
            )
        })?;
        let config: Self =
            toml::from_str(&text).with_context(|| format!("Invalid TOML in {}", path.display()))?;
        config
            .validate()
            .with_context(|| format!("Invalid configuration in {}", path.display()))?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        self.account
            .email
            .parse::<lettre::Address>()
            .context("account.email: invalid address")?;
        if self.account.username.trim().is_empty() {
            bail!("account.username is required");
        }
        for (name, server) in [("imap", &self.account.imap), ("smtp", &self.account.smtp)] {
            if server.host.trim().is_empty() || server.host.contains(char::is_whitespace) {
                bail!("account.{name}.host: expected a hostname");
            }
            if server.port == 0 {
                bail!("account.{name}.port must be between 1 and 65535");
            }
        }
        if !(50..=10000).contains(&self.keybindings.timeout_ms) {
            bail!("keybindings.timeout_ms must be between 50 and 10000");
        }
        if let Some(command) = &self.editor.command
            && shell_words::split(command)?.is_empty()
        {
            bail!("editor.command cannot be empty");
        }
        KeyMap::new(&self.keybindings.contexts, self.keybindings.timeout_ms)?;
        Ok(())
    }

    pub fn credential_key(&self) -> String {
        format!(
            "{}@{}:{}",
            self.account.username, self.account.imap.host, self.account.imap.port
        )
    }
}

pub fn config_path() -> Result<PathBuf> {
    let base = directories::BaseDirs::new().context("Cannot determine user directories")?;
    Ok(base.config_dir().join("terminal-email/config.toml"))
}

pub fn cache_path(config: &Config) -> Result<PathBuf> {
    use std::hash::{Hash, Hasher};
    let base = directories::BaseDirs::new().context("Cannot determine user directories")?;
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    config.credential_key().hash(&mut hash);
    Ok(base
        .cache_dir()
        .join("terminal-email")
        .join(format!("{:016x}/mail.db", hash.finish())))
}

pub fn private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn theme_defaults_and_type_validation() {
        let text = include_str!("../config.example.toml");
        let legacy: Config = toml::from_str(&text.replace("theme = \"default\"", "")).unwrap();
        assert_eq!(legacy.display.theme, "default");
        let custom: Config =
            toml::from_str(&text.replace("theme = \"default\"", "theme = \"custom\"")).unwrap();
        assert_eq!(custom.display.theme, "custom");
        assert!(
            toml::from_str::<Config>(&text.replace("theme = \"default\"", "theme = 42")).is_err()
        );
    }

    #[test]
    fn time_grouping_defaults_and_toml_validation() {
        let text = include_str!("../config.example.toml");
        let defaulted: Config = toml::from_str(&text.replace("time_grouping = true", "")).unwrap();
        assert!(defaulted.display.time_grouping);
        let legacy: Config = toml::from_str(
            &text
                .replace("[display]", "")
                .replace("theme = \"default\"", "")
                .replace("time_grouping = true", ""),
        )
        .unwrap();
        assert!(legacy.display.time_grouping);
        let disabled: Config =
            toml::from_str(&text.replace("time_grouping = true", "time_grouping = false")).unwrap();
        assert!(!disabled.display.time_grouping);
        disabled.validate().unwrap();
        assert!(
            toml::from_str::<Config>(
                &text.replace("time_grouping = true", "time_grouping = \"true\"")
            )
            .is_err()
        );
        assert!(
            toml::from_str::<Config>(
                &text.replace("time_grouping = true", "time_grouping = true\ninvalid = 1")
            )
            .is_err()
        );
    }

    #[test]
    fn example_validates_and_rejects_plaintext() {
        let text = include_str!("../config.example.toml");
        let config: Config = toml::from_str(text).unwrap();
        config.validate().unwrap();
        assert!(toml::from_str::<Config>(&text.replace("implicit", "plaintext")).is_err());
        assert!(toml::from_str::<Config>(&format!("{text}\n[account.extra]\nx=1")).is_err());
    }
}
