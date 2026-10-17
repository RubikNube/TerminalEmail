# TerminalEmail

A Rust terminal email client with Vim navigation and configurable TOML keybindings.

## Build and try it

Install stable Rust with [rustup](https://rustup.rs). On Linux, building also requires a C compiler, `pkg-config`, OpenSSL development files, and D-Bus development files. On Arch Linux these are provided by `base-devel`, `pkgconf`, `openssl`, and `dbus`.

```sh
cargo build --release
cargo run -- demo
```

The demo has sample conversations and an attachment. It does not contact mail servers or send mail. Compose in the demo retains a local draft.

## Connect an account

```sh
cargo run -- init
cargo run -- check-config
cargo run
```

Use an account that permits IMAP/SMTP password authentication, or an app password from your provider. OAuth2 is not implemented. IMAP and SMTP share the configured username and password. The setup command defaults to IMAP implicit TLS on port 993 and SMTP STARTTLS on port 587; edit the file to use other TLS ports or STARTTLS for IMAP. Certificate verification is mandatory.

Configuration lives at `$XDG_CONFIG_HOME/terminal-email/config.toml` (usually `~/.config/terminal-email/config.toml`). See [config.example.toml](config.example.toml). Use `--config PATH` to select a different file. `check-config` validates settings and bindings without network access.

Passwords are stored in your Linux Secret Service keyring (for example GNOME Keyring or KeePassXC with Secret Service enabled). Use `terminal-email set-password` to replace the saved secret. If the keyring is unavailable, startup can use a prompted password for that session. Leave the startup password empty to browse cached messages offline. Passwords are not written into configuration or the mail cache.

The cache is an ordinary, unencrypted SQLite database under `$XDG_CACHE_HOME/terminal-email/<account-id>/mail.db`. Its directory is private to your user. Protect your user account or use disk encryption if your mail requires protection at rest.

## Navigation

| Keys | Action |
| --- | --- |
| `j` / `k` | Move or scroll |
| `gg` / `G` | First / last |
| `Ctrl-d` / `Ctrl-u` | Half-page down / up |
| `Enter` / `l` | Open folder or conversation |
| `h` / `Esc` / `q` | Back; quit from folders |
| `Ctrl-c` | Quit |
| `/` | Search cached mail in the active folder |
| `n` / `N` | Next / previous result or conversation |
| `[` / `]` | Previous / next message within a conversation |
| `c` | Compose |
| `r` / `R` / `f` | Reply / reply-all / forward |
| `m` / `s` | Toggle read / starred on the selected message |
| `a` / `d` | Archive / move selected message to Trash |
| `S` | Save a downloaded attachment |
| `Ctrl-r` | Retry a missing message body, otherwise refresh |
| `?` | Show configured bindings |

Contexts are `global`, `folders`, `threads`, `message`, and `prompt`. Context mappings override equivalent global mappings. Add or replace keys with command names displayed in `?` help:

```toml
[keybindings]
timeout_ms = 750

[keybindings.global]
"Ctrl-j" = "cursor.down"
"g g" = "cursor.first" # Equivalent to gg; do not define both in one context.
"x" = "unbound"

[keybindings.threads]
"J" = "cursor.down"
```

Plain sequences such as `gg` are character sequences. For named or modified keys use tokens such as `Enter`, `Esc`, `Ctrl-r`, `Alt-j`, or `Ctrl-x Ctrl-r`. Ambiguous exact/prefix bindings wait for the configured timeout. `Esc` cancels a pending sequence. In text prompts, printable keys enter text, `Backspace` removes a character, `Enter` accepts, and `Esc` cancels. Confirmation prompts require `y` or `n`.

## Mail behavior

- Folder headers and flags synchronize when opened and every 60 seconds. Bodies and attachments download on demand; previously downloaded content is readable offline. Opening messages takes priority between synchronization steps. If a body download fails, Ctrl-r in the message view retries it.
- Conversations are grouped using Message-ID and References/In-Reply-To. Messages without usable identifiers remain separate.
- Search uses SQLite FTS5 over cached sender, recipients, subject, and downloaded body text in the active folder. Words are combined with AND. Search can miss bodies which have never been downloaded. Back clears the search.
- Plain text is preferred. HTML-only email is converted to text; remote images are never loaded, and terminal control characters are removed.
- Archive/Trash/Sent destinations use SPECIAL-USE attributes. Set `archive_folder`, `trash_folder`, or `sent_folder` under `[account]` if needed. Moving requires IMAP MOVE or UIDPLUS support. Deletion moves mail to Trash after confirmation; there is no permanent-delete command.
- Mailbox operations and sending require connectivity. They are never queued for later or automatically retried after failure.

## Composing

The client temporarily releases the terminal and opens a draft using `[editor].command`, then `$EDITOR`, then `vi`. Editor commands are split into arguments without a shell. Edit the `To`, `Cc`, `Bcc`, and `Subject` headers, preserve the blank line, and write the message body below it. Save and exit the editor, then review and confirm sending in the client. Bcc recipients are included in the SMTP envelope but omitted from transmitted headers.

Drafts are private files under `~/.local/share/terminal-email/drafts` (respecting XDG data directories). Cancelled or failed drafts are retained, with their path in the status message. Successful sends are appended to Sent and the draft is removed. If SMTP accepts a message but saving to Sent fails, the client explicitly reports delivery and retains the draft; do not resend it without checking delivery. Replies and forwards quote/include text only; outgoing attachments and a draft browser are not yet supported.

## Development

See the [developer guide](docs/development.md) for architecture diagrams,
runtime and storage details, extension checklists, testing, and troubleshooting.

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

The code separates terminal rendering, input dispatch, app state, cache, draft construction, and the IMAP/SMTP adapter. A dedicated worker owns the SQLite connection and async network runtime, so network requests do not block terminal navigation. Tests exercise key sequences and conflicts, cache identity/search/reconciliation, MIME parsing, draft validation, attachment writes, app workflows, and rendering at several terminal sizes. Protocol tests connect to local mock servers, including IMAP with verified TLS and refusal of plaintext IMAP/SMTP authentication. Live-provider authentication requires an actual account and is not exercised by the tests.
