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
| `Enter` / `l` | Open folder/conversation or expand/collapse a time section |
| `t` | Toggle time grouping in the conversation list |
| `h` / `Esc` | Back; stay open at folders |
| `q` | Quit from any screen (type normally in prompts) |
| `/` | Search cached mail in the active folder |
| `n` / `N` | Next / previous result or conversation |
| `[` / `]` | Previous / next message within a conversation |
| `c` | Compose |
| `r` / `R` / `f` | Reply / reply-all / forward |
| `m` / `s` | Toggle read / starred on the selected message |
| `a` / `d` | Archive / move selected message to Trash |
| `S` | Save a downloaded attachment |
| `b` | Open the current message’s HTML in the default browser |
| `Ctrl-r` | Retry a missing message body, otherwise refresh |
| `Space t` | Search and preview themes |
| `?` | Show configured bindings |

Contexts are `global`, `folders`, `threads`, `message`, `prompt`, and `theme_picker`. Context mappings override equivalent global mappings; the theme picker uses only its own bindings so printable keys remain search text. Add or replace keys with command names displayed in `?` help:

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

## Time grouping

Conversations appear under selectable time headings: Future, Today, Yesterday, This week, This month, Older, and Unknown date. Empty sections are hidden. Each conversation stays together under its newest message's date. Dates use your local timezone, with weeks starting Monday.

Move to a heading with `j` / `k` and press `Enter` or `l` to collapse or expand it. Headings show conversation counts; message actions on headings do nothing. `n` / `N` skip collapsed conversations in the list. Search expands sections for the new results.

Set the startup behavior in your existing `config.toml`:

```toml
[display]
time_grouping = true # Default; set false for a flat list.
```

Press `t` to toggle grouping for the session. Remap it using `threads.toggle_time_grouping` under `[keybindings.threads]`. Session changes do not rewrite configuration. Collapsed sections reset when changing folders.

## Themes

Press `Space` then `t` to open the theme picker from folders, conversations, or a message. Type to fuzzy-filter theme names and IDs; use Up/Down or Ctrl-p/Ctrl-n to browse. The highlighted theme previews immediately across the UI. Enter confirms and saves it; Escape restores the theme from before the picker opened. Letters such as `j`, `k`, and `q` enter the query. Backspace deletes a character.

Four themes ship with the binary: `default` (the original appearance), `dark`, `light`, and `matrix` (green on black). Choose a startup theme with:

```toml
[display]
theme = "default"
```

Confirmed selections update only `display.theme` in the active configuration file, preserving comments, unrelated settings, and file permissions. If saving fails, the picker stays open with an error; Escape still cancels the preview. Demo mode keeps selections for that session without writing configuration.

Custom themes live in `themes/` beside the active configuration file, normally `~/.config/terminal-email/themes/`. With `--config /path/to/config.toml`, the directory is `/path/to/themes/`, including in demo mode. Each `.toml` file represents one theme; its filename stem is its ID. An optional `name` sets the display label. A custom file with a built-in ID overrides that built-in theme.

Copy the annotated [complete theme template](themes/default.toml) into that directory as, for example, `my-theme.toml`, then edit it. Or start with a small partial theme:

```toml
name = "My Theme"

[palette]
background = "#12171f"
foreground = "#d6dde8"
accent = "#64d7cf"
selection = "#2b3d49"

[styles.folder_selection]
fg = "$accent"
bg = "$selection"
bold = true
```

Palette colors accept `"#RRGGBB"`, an integer from 0 to 255, `"reset"` for the terminal default, or an ANSI name: `black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `gray`, `dark_gray`, `light_red`, `light_green`, `light_yellow`, `light_blue`, `light_magenta`, `light_cyan`, `white`. Add reusable palette names and reference them with `$name`; palette entries can also reference each other, without cycles.

Each `[styles.<role>]` accepts `fg`, `bg`, `bold`, `italic`, and `underline`. Colors can be literals or palette references; flags can be `true` or `false`. Roles cover background, panels, text, selections, borders, unread/read messages, starred indicators, time headings, errors, mode indicators, prompt input, and picker selection/matches; the template lists every role. Omitted palette entries inherit default colors. Omitted style fields inherit the default role definition resolved against your palette.

Reopen the picker after editing files to load changes. Invalid files appear as disabled results with diagnostics. Missing or invalid startup themes use the embedded default and report a warning; invalid overrides leave their built-in theme available. `check-config` validates the selected theme and every discovered custom theme without contacting mail servers.

Remap the opening shortcut using `theme.select` in `[keybindings.global]`. Picker actions are configured separately:

```toml
[keybindings.theme_picker]
"Ctrl-n" = "theme_picker.next"
"Ctrl-p" = "theme_picker.previous"
"Enter" = "theme_picker.accept"
"Esc" = "theme_picker.cancel"
"Backspace" = "theme_picker.delete"
```

## Mail behavior

- Folder headers and flags synchronize when opened and every 60 seconds. Bodies and attachments download on demand; previously downloaded content is readable offline. Opening messages takes priority between synchronization steps. If a body download fails, Ctrl-r in the message view retries it.
- Conversations are grouped using Message-ID and References/In-Reply-To. Messages without usable identifiers remain separate.
- Search uses SQLite FTS5 over cached sender, recipients, subject, and downloaded body text in the active folder. Words are combined with AND. Search can miss bodies which have never been downloaded. Back clears the search.
- Plain text is preferred in the terminal. HTML-only email is converted to text; remote images are never loaded in the terminal, and terminal control characters are removed.
- In the message view, press `b` to open original HTML in your Linux default browser through `xdg-open`. Remap it with `message.open_browser` under `[keybindings.message]`. The browser may load external images and other resources. Missing HTML (including older cached messages) downloads on demand when connected; cached HTML works offline. Plain-text-only messages show a status notice. Private temporary HTML files remain available until the app exits. Embedded `cid:` images are not extracted.
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
