use crate::{
    compose::{self, ComposeKind, Draft},
    config::Config,
    keys::KeyMap,
    mail::{self, Request, Update, Worker},
    model::{Attachment, Folder, Message, threads},
};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Folders,
    Threads,
    Message,
}

pub enum Prompt {
    Search(String),
    AttachmentIndex(String),
    AttachmentPath {
        attachment: Attachment,
        input: String,
    },
    Overwrite {
        attachment: Attachment,
        path: std::path::PathBuf,
    },
    Trash(Message),
    Send {
        draft: Draft,
        message: Box<lettre::Message>,
        preview: String,
    },
}

type MessageKey = (String, u32, u32);

fn message_key(message: &Message) -> MessageKey {
    (message.folder.clone(), message.uid_validity, message.uid)
}

enum BodyState {
    Loading,
    Failed(String),
}

pub struct App {
    pub config: Config,
    pub keys: KeyMap,
    pub folders: Vec<Folder>,
    pub folder_cursor: usize,
    pub active_folder: String,
    pub messages: Vec<Message>,
    pub groups: Vec<Vec<usize>>,
    pub thread_cursor: usize,
    pub message_cursor: usize,
    pub scroll: u16,
    pub view: View,
    pub status: String,
    pub error: bool,
    pub help: bool,
    pub help_scroll: u16,
    pub prompt: Option<Prompt>,
    pub search: String,
    pub search_ids: Option<HashSet<u32>>,
    pub demo: bool,
    pub offline: bool,
    pub quit: bool,
    pub page_height: usize,
    body_states: HashMap<MessageKey, BodyState>,
}

impl App {
    pub fn new(config: Config, demo: bool, offline: bool) -> Result<Self> {
        Ok(Self {
            keys: KeyMap::new(&config.keybindings.contexts, config.keybindings.timeout_ms)?,
            config,
            folders: vec![],
            folder_cursor: 0,
            active_folder: "INBOX".into(),
            messages: vec![],
            groups: vec![],
            thread_cursor: 0,
            message_cursor: 0,
            scroll: 0,
            view: View::Folders,
            status: "Loading cached mail…".into(),
            error: false,
            help: false,
            help_scroll: 0,
            prompt: None,
            search: String::new(),
            search_ids: None,
            demo,
            offline,
            body_states: HashMap::new(),
            quit: false,
            page_height: 20,
        })
    }

    pub fn context(&self) -> &'static str {
        if self.prompt.is_some() {
            "prompt"
        } else {
            match self.view {
                View::Folders => "folders",
                View::Threads => "threads",
                View::Message => "message",
            }
        }
    }

    pub fn selected(&self) -> Option<&Message> {
        let group = self.groups.get(self.thread_cursor)?;
        let index = if self.view == View::Message {
            self.message_cursor
        } else {
            group.len().saturating_sub(1)
        };
        self.messages.get(*group.get(index)?)
    }

    pub fn set_messages(&mut self, messages: Vec<Message>) {
        let selected = self.selected().map(|m| m.uid);
        self.messages = messages;
        self.rebuild();
        if let Some(uid) = selected {
            if let Some((group, offset)) = self.groups.iter().enumerate().find_map(|(g, group)| {
                group
                    .iter()
                    .position(|&i| self.messages[i].uid == uid)
                    .map(|o| (g, o))
            }) {
                self.thread_cursor = group;
                self.message_cursor = offset;
            } else if self.view == View::Message {
                self.view = View::Threads;
            }
        }
    }

    fn rebuild(&mut self) {
        self.groups = threads(&self.messages);
        if let Some(ids) = &self.search_ids {
            self.groups
                .retain(|group| group.iter().any(|&i| ids.contains(&self.messages[i].uid)));
        }
        self.thread_cursor = self.thread_cursor.min(self.groups.len().saturating_sub(1));
        self.message_cursor = self.message_cursor.min(
            self.groups
                .get(self.thread_cursor)
                .map_or(0, |g| g.len().saturating_sub(1)),
        );
    }

    pub fn status(&mut self, message: impl Into<String>, error: bool) {
        self.status = message.into();
        self.error = error;
    }

    fn request(&mut self, worker: Option<&Worker>, request: Request) {
        if let Some(worker) = worker
            && worker.requests.try_send(request).is_err()
        {
            self.status("Mail worker is busy. Please try again shortly.", true);
        }
    }

    pub fn update(&mut self, update: Update, worker: Option<&Worker>) {
        match update {
            Update::Folders(folders) => {
                self.folders = folders;
                self.folder_cursor = self.folder_cursor.min(self.folders.len().saturating_sub(1));
            }
            Update::Messages { folder, messages } if folder == self.active_folder => {
                self.set_messages(messages);
                if !self.search.is_empty() {
                    self.request(
                        worker,
                        Request::Search {
                            folder,
                            query: self.search.clone(),
                        },
                    );
                }
            }
            Update::Search {
                folder,
                query,
                messages,
            } if folder == self.active_folder && query == self.search => {
                self.search_ids = Some(messages.iter().map(|m| m.uid).collect());
                self.rebuild();
                self.status(
                    format!("Local search: {} matching cached messages", messages.len()),
                    false,
                );
            }
            Update::Body(message) => {
                if message.body.is_some() {
                    self.body_states.remove(&message_key(&message));
                }
                if message.folder == self.active_folder
                    && let Some(old) = self
                        .messages
                        .iter_mut()
                        .find(|m| m.uid == message.uid && m.uid_validity == message.uid_validity)
                {
                    *old = *message;
                }
            }
            Update::BodyError { message, error } => {
                let key = message_key(&message);
                // A late failure must not replace a successfully loaded body.
                if matches!(self.body_states.get(&key), Some(BodyState::Loading)) {
                    self.body_states.insert(key, BodyState::Failed(error));
                }
            }
            Update::Status(message) => self.status(message, false),
            Update::Error(message) => self.status(message, true),
            Update::Sent { path, warning } => self.status(
                warning
                    .clone()
                    .unwrap_or_else(|| format!("Message sent. Draft {} removed.", path.display())),
                warning.is_some(),
            ),
            _ => {}
        }
    }

    fn load_body(&mut self, worker: Option<&Worker>, message: &Message) {
        if self.offline
            || matches!(
                self.body_states.get(&message_key(message)),
                Some(BodyState::Loading)
            )
        {
            return;
        }
        let state = match worker {
            Some(worker) => match worker.requests.try_send(Request::Body(message.clone())) {
                Ok(()) => BodyState::Loading,
                Err(_) => BodyState::Failed("Mail worker is busy or unavailable.".into()),
            },
            None => BodyState::Failed("Mail worker is unavailable.".into()),
        };
        self.body_states.insert(message_key(message), state);
    }

    pub fn body_notice(&self, message: &Message) -> String {
        if self.offline {
            return "This message has not been downloaded. Reconnect to read its body. Cached headers remain available.".into();
        }
        match self.body_states.get(&message_key(message)) {
            Some(BodyState::Loading) => "Loading message… Waiting for the current mail operation or downloading its body. Cached headers remain available.".into(),
            Some(BodyState::Failed(error)) => format!("Could not load message: {error}\nCtrl-r to retry."),
            None => "Message body is not downloaded. Ctrl-r to load it.".into(),
        }
    }

    fn open_message(&mut self, worker: Option<&Worker>) {
        self.view = View::Message;
        self.scroll = 0;
        if let Some(message) = self.selected().cloned()
            && !self.demo
        {
            if message.body.is_none() {
                self.load_body(worker, &message);
            }
            if !message.seen && !self.offline {
                self.request(
                    worker,
                    Request::Flag {
                        message,
                        starred: false,
                        value: true,
                    },
                );
            }
        }
    }

    pub fn command(&mut self, command: &str, worker: Option<&Worker>) -> Option<ComposeKind> {
        let step = self.page_height.max(2) / 2;
        if self.help {
            match command {
                "cursor.down" => self.help_scroll = self.help_scroll.saturating_add(1),
                "cursor.up" => self.help_scroll = self.help_scroll.saturating_sub(1),
                "page.down" => self.help_scroll = self.help_scroll.saturating_add(step as u16),
                "page.up" => self.help_scroll = self.help_scroll.saturating_sub(step as u16),
                "cursor.first" => self.help_scroll = 0,
                "help" | "back" => self.help = false,
                "quit" => self.quit = true,
                _ => {}
            }
            return None;
        }
        match command {
            "quit" => self.quit = true,
            "help" => {
                self.help = !self.help;
                self.help_scroll = 0;
            }
            "cursor.down" | "cursor.up" | "page.down" | "page.up" | "cursor.first"
            | "cursor.last" => {
                let down = matches!(command, "cursor.down" | "page.down");
                let amount = if command.starts_with("page.") {
                    step
                } else {
                    1
                };
                let bound = match self.view {
                    View::Folders => self.folders.len().saturating_sub(1),
                    View::Threads => self.groups.len().saturating_sub(1),
                    View::Message => self
                        .selected()
                        .and_then(|m| m.body.as_ref())
                        .map_or(0, |s| s.chars().count().min(u16::MAX as usize)),
                };
                let cursor = match self.view {
                    View::Folders => &mut self.folder_cursor,
                    View::Threads => &mut self.thread_cursor,
                    View::Message => {
                        self.scroll = match command {
                            "cursor.first" => 0,
                            "cursor.last" => bound as u16,
                            _ if down => {
                                self.scroll.saturating_add(amount as u16).min(bound as u16)
                            }
                            _ => self.scroll.saturating_sub(amount as u16),
                        };
                        return None;
                    }
                };
                *cursor = match command {
                    "cursor.first" => 0,
                    "cursor.last" => bound,
                    _ if down => cursor.saturating_add(amount).min(bound),
                    _ => cursor.saturating_sub(amount),
                };
            }
            "open" => match self.view {
                View::Folders => {
                    if let Some(folder) = self.folders.get(self.folder_cursor) {
                        let name = folder.name.clone();
                        if self.active_folder != name {
                            self.messages.clear();
                            self.groups.clear();
                            self.search.clear();
                            self.search_ids = None;
                            self.thread_cursor = 0;
                            self.message_cursor = 0;
                        }
                        self.active_folder = name.clone();
                        self.view = View::Threads;
                        if !self.demo {
                            self.request(worker, Request::Load(name));
                        }
                    }
                }
                View::Threads if !self.groups.is_empty() => {
                    self.message_cursor = self.groups[self.thread_cursor].len().saturating_sub(1);
                    self.open_message(worker);
                }
                _ => {}
            },
            "back" => match self.view {
                View::Message => self.view = View::Threads,
                View::Threads if !self.search.is_empty() => {
                    self.search.clear();
                    self.search_ids = None;
                    self.rebuild();
                }
                View::Threads => self.view = View::Folders,
                View::Folders => self.quit = true,
            },
            "search" => {
                self.prompt = Some(Prompt::Search(self.search.clone()));
                self.keys.reset();
            }
            "search.next" | "search.previous" => {
                if !self.groups.is_empty() {
                    self.thread_cursor = if command == "search.next" {
                        (self.thread_cursor + 1) % self.groups.len()
                    } else {
                        (self.thread_cursor + self.groups.len() - 1) % self.groups.len()
                    };
                    if self.view == View::Message {
                        self.message_cursor =
                            self.groups[self.thread_cursor].len().saturating_sub(1);
                        self.open_message(worker);
                    }
                }
            }
            "refresh" => {
                if !self.demo {
                    if self.view == View::Message
                        && let Some(message) = self.selected().filter(|m| m.body.is_none()).cloned()
                    {
                        self.load_body(worker, &message);
                    } else {
                        self.request(worker, Request::Refresh(self.active_folder.clone()));
                    }
                }
            }
            "message.next" | "message.previous" => {
                if let Some(group) = self.groups.get(self.thread_cursor) {
                    self.message_cursor = if command == "message.next" {
                        (self.message_cursor + 1).min(group.len().saturating_sub(1))
                    } else {
                        self.message_cursor.saturating_sub(1)
                    };
                    self.open_message(worker);
                }
            }
            "compose" => return Some(ComposeKind::New),
            "reply" | "reply_all" | "forward" => {
                if let Some(message) = self.selected() {
                    if message.body.is_none() {
                        self.status(
                            "Open the message and wait for its body before replying or forwarding.",
                            true,
                        );
                    } else {
                        return Some(match command {
                            "reply" => ComposeKind::Reply,
                            "reply_all" => ComposeKind::ReplyAll,
                            _ => ComposeKind::Forward,
                        });
                    }
                }
            }
            "toggle_read" | "toggle_star" => {
                if let Some(message) = self.selected().cloned() {
                    let starred = command == "toggle_star";
                    let value = !(if starred {
                        message.starred
                    } else {
                        message.seen
                    });
                    if self.demo {
                        if let Some(old) = self.messages.iter_mut().find(|m| m.uid == message.uid) {
                            if starred {
                                old.starred = value;
                            } else {
                                old.seen = value;
                            }
                        }
                    } else {
                        self.request(
                            worker,
                            Request::Flag {
                                message,
                                starred,
                                value,
                            },
                        );
                    }
                }
            }
            "archive" | "trash" => {
                if let Some(message) = self.selected().cloned() {
                    if command == "trash" {
                        self.prompt = Some(Prompt::Trash(message));
                    } else if self.demo {
                        self.messages.retain(|m| m.uid != message.uid);
                        self.rebuild();
                    } else {
                        self.request(
                            worker,
                            Request::Move {
                                message,
                                role: "archive".into(),
                            },
                        );
                    }
                }
            }
            "attachment.save" => {
                if let Some(message) = self.selected() {
                    if message.attachments.is_empty() {
                        self.status("No downloaded attachments in this message.", false);
                    } else {
                        self.prompt = Some(Prompt::AttachmentIndex("1".into()));
                    }
                }
            }
            _ => {}
        }
        None
    }

    fn search(&mut self, query: String, worker: Option<&Worker>) {
        self.search = query.clone();
        self.thread_cursor = 0;
        self.view = View::Threads;
        if query.is_empty() {
            self.search_ids = None;
            self.rebuild();
        } else if self.demo {
            let words: Vec<_> = query
                .to_lowercase()
                .split_whitespace()
                .map(str::to_owned)
                .collect();
            self.search_ids = Some(
                self.messages
                    .iter()
                    .filter(|m| {
                        let text = format!(
                            "{} {} {} {} {}",
                            m.from,
                            m.to,
                            m.cc,
                            m.subject,
                            m.body.as_deref().unwrap_or_default()
                        )
                        .to_lowercase();
                        words.iter().all(|word| text.contains(word))
                    })
                    .map(|m| m.uid)
                    .collect(),
            );
            self.rebuild();
        } else {
            self.request(
                worker,
                Request::Search {
                    folder: self.active_folder.clone(),
                    query,
                },
            );
        }
    }

    pub fn prompt_key(&mut self, mut key: KeyEvent, worker: Option<&Worker>) {
        match self.keys.feed("prompt", key, Instant::now()).as_deref() {
            Some("prompt.accept") => key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            Some("prompt.cancel") => key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            Some("prompt.delete") => key = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            Some("quit") => {
                self.quit = true;
                return;
            }
            _ if !matches!(key.code, KeyCode::Char(_))
                || key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                return;
            }
            _ => {}
        }
        self.apply_prompt_key(key, worker);
    }

    fn apply_prompt_key(&mut self, key: KeyEvent, worker: Option<&Worker>) {
        let Some(mut prompt) = self.prompt.take() else {
            return;
        };
        if key.code == KeyCode::Esc {
            if let Prompt::Send { draft, .. } = prompt {
                self.status(format!("Draft retained at {}", draft.path.display()), false);
            }
            self.keys.reset();
            return;
        }
        match &mut prompt {
            Prompt::Search(input)
            | Prompt::AttachmentIndex(input)
            | Prompt::AttachmentPath { input, .. } => match key.code {
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    input.push(c)
                }
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Enter => {
                    match prompt {
                        Prompt::Search(query) => self.search(query, worker),
                        Prompt::AttachmentIndex(index) => {
                            if let Some(attachment) = index
                                .parse::<usize>()
                                .ok()
                                .and_then(|i| i.checked_sub(1))
                                .and_then(|i| self.selected()?.attachments.get(i))
                                .cloned()
                            {
                                self.prompt = Some(Prompt::AttachmentPath {
                                    input: compose::safe_filename(&attachment.name),
                                    attachment,
                                });
                            } else {
                                self.status("Invalid attachment number", true);
                            }
                        }
                        Prompt::AttachmentPath { attachment, input } => {
                            let path = std::path::PathBuf::from(input);
                            if path.exists() {
                                self.prompt = Some(Prompt::Overwrite { attachment, path });
                            } else if let Err(error) =
                                compose::save_attachment(&path, &attachment.data, false)
                            {
                                self.status(error.to_string(), true);
                            } else {
                                self.status(
                                    format!("Attachment saved to {}", path.display()),
                                    false,
                                );
                            }
                        }
                        _ => {}
                    }
                    return;
                }
                _ => {}
            },
            Prompt::Overwrite { attachment, path } => {
                if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                    let result = compose::save_attachment(path, &attachment.data, true);
                    let failed = result.is_err();
                    self.status(
                        result
                            .map(|_| format!("Attachment saved to {}", path.display()))
                            .unwrap_or_else(|e| e.to_string()),
                        failed,
                    );
                    return;
                } else if matches!(key.code, KeyCode::Char('n' | 'N')) {
                    return;
                }
            }
            Prompt::Trash(message) => {
                if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                    if self.demo {
                        self.messages.retain(|m| m.uid != message.uid);
                        self.rebuild();
                    } else {
                        self.request(
                            worker,
                            Request::Move {
                                message: message.clone(),
                                role: "trash".into(),
                            },
                        );
                    }
                    return;
                } else if matches!(key.code, KeyCode::Char('n' | 'N')) {
                    return;
                }
            }
            Prompt::Send { .. } => {
                if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                    if let Prompt::Send { draft, message, .. } = prompt {
                        if self.demo {
                            self.status(
                                format!(
                                    "Demo: message not sent; draft retained at {}",
                                    draft.path.display()
                                ),
                                false,
                            );
                        } else {
                            self.status(
                                format!(
                                    "Sending… Draft retained at {} until delivery is confirmed.",
                                    draft.path.display()
                                ),
                                false,
                            );
                            self.request(worker, Request::Send { draft, message });
                        }
                    }
                    return;
                } else if matches!(key.code, KeyCode::Char('n' | 'N')) {
                    if let Prompt::Send { draft, .. } = prompt {
                        self.status(format!("Draft retained at {}", draft.path.display()), false);
                    }
                    return;
                }
            }
        }
        self.prompt = Some(prompt);
    }
}

pub fn demo_app() -> Result<App> {
    let config = toml::from_str(include_str!("../config.example.toml"))?;
    let mut app = App::new(config, true, false)?;
    app.folders = vec![Folder {
        name: "INBOX".into(),
        unread: 2,
        role: None,
    }];
    let first = Message {
        uid: 1, folder: "INBOX".into(), uid_validity: 1, message_id: "welcome@terminal-email".into(),
        from: "TerminalEmail <hello@example.org>".into(), to: "you@example.com".into(),
        subject: "Welcome to your terminal inbox".into(), date: "2026-10-17 10:00".into(), timestamp: 1,
        body: Some("Your inbox, at the speed of a keystroke.\n\nUse j/k to move, Enter or l to open, h or q to go back.\n\ngg and G jump to the first and last item. Ctrl-d/u scroll half a page.\n\nPress ? for every configured binding. Remap keys in config.toml.\n\nThis is an offline demo. No mail server is contacted.\n\nYou can save the attached example with S.\n\nHappy reading!".into()),
        attachments: vec![Attachment { name: "welcome.txt".into(), content_type: "text/plain".into(), data: b"Welcome to TerminalEmail!\n".to_vec() }], ..Default::default()
    };
    let reply = Message { uid: 2, message_id: "reply@terminal-email".into(), references: vec![first.message_id.clone()], subject: "Re: Welcome to your terminal inbox".into(), timestamp: 2, body: Some("Conversations stay together.\n\nUse [ and ] in the message view to move between messages in this thread.\n\nPress / to search the locally cached sender, recipients, subject, and message text.".into()), attachments: vec![], ..first.clone() };
    let other = Message { uid: 3, message_id: "keys@terminal-email".into(), subject: "Make it yours: configurable Vim bindings".into(), timestamp: 3, body: Some("Keybindings are commands grouped by context: global, folders, threads, message, and prompt.\n\nExample TOML:\n\n[keybindings.global]\n\"j\" = \"cursor.down\"\n\"gg\" = \"cursor.first\"\n\nThe default sequence timeout is 750 ms.\n\nCreate an account with: terminal-email init".into()), attachments: vec![], ..first.clone() };
    app.set_messages(vec![other, reply, first]);
    app.status("DEMO · Enter opens INBOX · ? help · q back/quit", false);
    Ok(app)
}

pub fn run_demo() -> Result<()> {
    event_loop(demo_app()?, None)
}

pub fn run(config: Config, password: String) -> Result<()> {
    let path = crate::config::cache_path(&config)?;
    let offline = password.is_empty();
    let app = App::new(config.clone(), false, offline)?;
    let worker = mail::start(config, password, path)?;
    worker
        .requests
        .blocking_send(Request::Refresh("INBOX".into()))?;
    event_loop(app, Some(worker))
}

/// RAII restores terminal modes even when rendering, input, or the editor fails.
struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

fn event_loop(mut app: App, mut worker: Option<Worker>) -> Result<()> {
    let _guard = TerminalGuard;
    let mut terminal = ratatui::try_init()?;
    let mut refreshed = Instant::now();
    let mut dirty = true;
    loop {
        if let Some(worker) = worker.as_mut() {
            while let Ok(update) = worker.updates.try_recv() {
                app.update(update, Some(worker));
                dirty = true;
            }
        }
        if app.quit {
            break;
        }
        if dirty {
            terminal.draw(|frame| crate::ui::draw(frame, &mut app))?;
            dirty = false;
        }
        let context = app.context();
        let mut command = app.keys.expire(context, Instant::now());
        if command.is_none() && event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    dirty = true;
                    if app.prompt.is_some() {
                        app.prompt_key(key, worker.as_ref());
                        continue;
                    }
                    command = app.keys.feed(app.context(), key, Instant::now());
                }
                Event::Resize(_, _) => {
                    terminal.clear()?;
                    dirty = true;
                }
                _ => {}
            }
        }
        if let Some(command) = command {
            dirty = true;
            if app.prompt.is_some() {
                let key = match command.as_str() {
                    "prompt.accept" => Some(KeyCode::Enter),
                    "prompt.cancel" => Some(KeyCode::Esc),
                    "prompt.delete" => Some(KeyCode::Backspace),
                    "quit" => {
                        app.quit = true;
                        None
                    }
                    _ => None,
                };
                if let Some(key) = key {
                    app.apply_prompt_key(KeyEvent::new(key, KeyModifiers::NONE), worker.as_ref());
                }
                continue;
            }
            let old_context = app.context();
            if let Some(kind) = app.command(&command, worker.as_ref()) {
                let original = app.selected().cloned();
                let result =
                    Draft::create(&app.config, kind, original.as_ref()).and_then(|draft| {
                        ratatui::restore();
                        let edited = draft.edit(&app.config);
                        terminal = ratatui::try_init()?;
                        terminal.clear()?;
                        edited?;
                        let message = draft.build(&app.config).with_context(|| {
                            format!("Draft retained at {}", draft.path.display())
                        })?;
                        let preview = std::fs::read_to_string(&draft.path)?;
                        Ok(Prompt::Send {
                            draft,
                            message: Box::new(message),
                            preview,
                        })
                    });
                match result {
                    Ok(prompt) => app.prompt = Some(prompt),
                    Err(error) => app.status(format!("{error:#}"), true),
                }
            }
            if app.context() != old_context {
                app.keys.reset();
            }
        }
        if !app.demo && !app.offline && refreshed.elapsed() >= Duration::from_secs(60) {
            app.request(worker.as_ref(), Request::Refresh(app.active_folder.clone()));
            refreshed = Instant::now();
        }
    }
    Ok(())
}

use anyhow::Context;

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;
    #[test]
    fn prompt_and_help_respect_remapped_keys() {
        let mut app = demo_app().unwrap();
        let overrides = [(
            "prompt".into(),
            [
                ("Enter".into(), "unbound".into()),
                ("Ctrl-y".into(), "prompt.accept".into()),
            ]
            .into(),
        )]
        .into();
        app.keys = KeyMap::new(&overrides, 750).unwrap();
        app.prompt = Some(Prompt::Search("speed".into()));
        app.prompt_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), None);
        assert!(app.prompt.is_some());
        app.prompt_key(
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
            None,
        );
        assert!(app.prompt.is_none());
        assert_eq!(app.groups.len(), 1);
        app.command("help", None);
        let cursor = app.thread_cursor;
        app.command("cursor.down", None);
        assert_eq!(app.help_scroll, 1);
        assert_eq!(app.thread_cursor, cursor);
        app.command("back", None);
        assert!(!app.help);
    }
    fn reader_with_worker() -> (App, Worker, mpsc::Receiver<Request>) {
        let mut app = demo_app().unwrap();
        app.demo = false;
        app.offline = false;
        app.view = View::Message;
        let index = app.groups[app.thread_cursor][app.message_cursor];
        app.messages[index].body = None;
        app.messages[index].seen = true;
        let (requests, receiver) = mpsc::channel(1);
        let (_sender, updates) = mpsc::unbounded_channel();
        (app, Worker { requests, updates }, receiver)
    }

    #[test]
    fn body_failure_and_refresh_retry_are_message_specific() {
        let (mut app, worker, mut requests) = reader_with_worker();
        let message = app.selected().unwrap().clone();
        app.open_message(Some(&worker));
        assert!(matches!(requests.try_recv(), Ok(Request::Body(_))));
        assert!(app.body_notice(&message).contains("Loading message"));
        app.command("refresh", Some(&worker));
        assert!(
            requests.try_recv().is_err(),
            "loading requests should be deduplicated"
        );
        app.update(
            Update::BodyError {
                message: Box::new(message.clone()),
                error: "Connection lost".into(),
            },
            Some(&worker),
        );
        assert!(app.body_notice(&message).contains("Connection lost"));
        assert!(app.body_notice(&message).contains("Ctrl-r to retry"));
        app.command("refresh", Some(&worker));
        assert!(matches!(requests.try_recv(), Ok(Request::Body(_))));
        let mut loaded = message.clone();
        loaded.body = Some("Downloaded".into());
        app.update(Update::Body(Box::new(loaded)), Some(&worker));
        assert_eq!(app.selected().unwrap().body.as_deref(), Some("Downloaded"));
        assert!(!app.body_states.contains_key(&message_key(&message)));
        app.command("refresh", Some(&worker));
        assert!(matches!(requests.try_recv(), Ok(Request::Refresh(_))));
        // A late failure cannot undo a successful download.
        app.update(
            Update::BodyError {
                message: Box::new(message.clone()),
                error: "Old failure".into(),
            },
            Some(&worker),
        );
        assert!(!app.body_states.contains_key(&message_key(&message)));
    }

    #[test]
    fn offline_body_notice_does_not_offer_retry_or_queue_download() {
        let (mut app, worker, mut requests) = reader_with_worker();
        app.offline = true;
        app.open_message(Some(&worker));
        app.command("refresh", Some(&worker));
        assert!(requests.try_recv().is_err());
        let notice = app.body_notice(app.selected().unwrap());
        assert!(notice.contains("Reconnect"));
        assert!(!notice.contains("Ctrl-r"));
        assert!(!notice.contains("Loading"));
    }

    #[test]
    fn rejected_body_request_can_be_retried_and_late_updates_do_not_affect_selection() {
        let (mut app, worker, mut requests) = reader_with_worker();
        let original = app.selected().unwrap().clone();
        worker
            .requests
            .try_send(Request::Refresh("INBOX".into()))
            .unwrap();
        app.open_message(Some(&worker));
        assert!(app.body_notice(&original).contains("busy or unavailable"));
        requests.try_recv().unwrap();
        app.command("refresh", Some(&worker));
        requests.try_recv().unwrap();
        let mut stale = original.clone();
        stale.uid_validity += 1;
        app.update(
            Update::BodyError {
                message: Box::new(stale),
                error: "Stale identity".into(),
            },
            Some(&worker),
        );
        assert!(app.body_notice(&original).contains("Loading"));
        app.command("back", Some(&worker));
        app.command("search.next", Some(&worker));
        let selected = message_key(app.selected().unwrap());
        app.update(
            Update::BodyError {
                message: Box::new(original.clone()),
                error: "Earlier download failed".into(),
            },
            Some(&worker),
        );
        assert_eq!(message_key(app.selected().unwrap()), selected);
        assert!(
            !app.body_notice(app.selected().unwrap())
                .contains("Earlier download failed")
        );
        // Completion in another folder also clears the original pending state.
        let mut late = original.clone();
        late.body = Some("Late completion".into());
        app.active_folder = "Other".into();
        app.update(Update::Body(Box::new(late)), Some(&worker));
        assert!(!app.body_states.contains_key(&message_key(&original)));
    }

    #[test]
    fn demo_navigation_threading_search_and_flags() {
        let mut app = demo_app().unwrap();
        app.command("open", None);
        assert_eq!(app.view, View::Threads);
        assert_eq!(app.groups.len(), 2);
        app.command("cursor.down", None);
        app.command("open", None);
        assert_eq!(app.view, View::Message);
        assert_eq!(app.selected().unwrap().uid, 2);
        app.command("message.previous", None);
        assert_eq!(app.selected().unwrap().uid, 1);
        app.command("toggle_star", None);
        assert!(app.selected().unwrap().starred);
        app.command("back", None);
        app.search("speed".into(), None);
        assert_eq!(app.groups.len(), 1);
        assert_eq!(app.groups[0].len(), 2);
        app.command("back", None);
        assert_eq!(app.groups.len(), 2);
        app.command("back", None);
        assert_eq!(app.view, View::Folders);
        app.command("back", None);
        assert!(app.quit);
    }
}
