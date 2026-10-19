mod app;
mod browser;
mod cache;
mod compose;
mod config;
mod editor;
mod grouping;
mod keys;
mod mail;
mod model;
mod theme;
mod ui;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use config::{Account, Config, Editor, KeyConfig, Server, TlsMode};
use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
};

#[derive(Parser)]
#[command(version, about = "Vim-inspired terminal email client")]
struct Cli {
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Set up an account and optionally store its password in the OS keyring.
    Init,
    /// Validate configuration without contacting mail servers.
    CheckConfig,
    /// Store or replace the account password in the OS keyring.
    SetPassword,
    /// Explore the UI using sample messages without a network or account.
    Demo,
}

fn prompt(label: &str, default: &str) -> Result<String> {
    print!(
        "{label}{}: ",
        if default.is_empty() {
            String::new()
        } else {
            format!(" [{default}]")
        }
    );
    io::stdout().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    Ok(if value.trim().is_empty() {
        default.into()
    } else {
        value.trim().into()
    })
}

fn keyring_entry(config: &Config) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(
        "terminal-email",
        &config.credential_key(),
    )?)
}

fn password(config: &Config) -> Result<String> {
    match keyring_entry(config).and_then(|entry| Ok(entry.get_password()?)) {
        Ok(value) => Ok(value),
        Err(_) => {
            let value = rpassword::prompt_password(
                "Account password/app password (empty for cached offline reading): ",
            )?;
            if !value.is_empty()
                && prompt("Save to OS keyring? (y/N)", "n")?.eq_ignore_ascii_case("y")
                && let Err(error) =
                    keyring_entry(config).and_then(|entry| Ok(entry.set_password(&value)?))
            {
                eprintln!(
                    "Could not save password to OS keyring: {error}. It will be used for this session only."
                );
            }
            Ok(value)
        }
    }
}

fn init(path: &std::path::Path) -> Result<()> {
    if path.exists() {
        bail!(
            "{} already exists; edit it directly or use --config with a new path",
            path.display()
        );
    }
    println!("TerminalEmail setup (password/app-password authentication)");
    let email = prompt("Email", "")?;
    let config = Config {
        account: Account {
            display_name: prompt("Display name", "")?,
            username: prompt("Username", &email)?,
            email,
            imap: Server {
                host: prompt("IMAP host", "")?,
                port: prompt("IMAP port (implicit TLS)", "993")?
                    .parse()
                    .context("Invalid IMAP port")?,
                tls: TlsMode::Implicit,
            },
            smtp: Server {
                host: prompt("SMTP host", "")?,
                port: prompt("SMTP port (STARTTLS)", "587")?
                    .parse()
                    .context("Invalid SMTP port")?,
                tls: TlsMode::Starttls,
            },
            archive_folder: None,
            trash_folder: None,
            sent_folder: None,
        },
        editor: Editor::default(),
        display: config::Display::default(),
        keybindings: KeyConfig::default(),
    };
    config.validate()?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        // A custom config may live in a shared directory. Do not chmod it.
        if !parent.exists() {
            config::private_dir(parent)?;
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(toml::to_string_pretty(&config)?.as_bytes())?;
    file.sync_all()?;
    println!("Configuration saved to {}", path.display());
    let secret = rpassword::prompt_password("Password/app password (empty to skip): ")?;
    if !secret.is_empty() {
        keyring_entry(&config)?.set_password(&secret).context("Could not save password; config was saved. Run set-password when your keyring is available")?;
        println!("Password saved to OS keyring.");
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = cli.config.unwrap_or(config::config_path()?);
    if matches!(cli.command, Some(Command::Demo)) {
        ensure_terminal()?;
        return app::run_demo(&path);
    }
    match cli.command {
        Some(Command::Init) => init(&path),
        Some(Command::CheckConfig) => {
            Config::load(&path).and_then(|config| theme::check(&path, &config.display.theme))?;
            println!("Configuration valid: {}", path.display());
            Ok(())
        }
        Some(Command::SetPassword) => {
            let config = Config::load(&path)?;
            let secret = rpassword::prompt_password("Password/app password: ")?;
            if secret.is_empty() {
                bail!("Password cannot be empty");
            }
            keyring_entry(&config)?.set_password(&secret)?;
            println!("Password saved to OS keyring.");
            Ok(())
        }
        Some(Command::Demo) => unreachable!(),
        None => {
            let config = Config::load(&path)?;
            ensure_terminal()?;
            let password = password(&config)?;
            app::run(config, password, &path)
        }
    }
}

fn ensure_terminal() -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("The interactive client requires a terminal. Try --help or check-config.");
    }
    Ok(())
}
