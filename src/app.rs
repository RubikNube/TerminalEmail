use crate::{
    browser::Browser,
    compose::{self, ComposeKind, Draft},
    config::Config,
    grouping::{Period, Row, local_period},
    keys::KeyMap,
    mail::{self, Request, Update, Worker},
    model::{Attachment, Folder, Message, threads},
    theme::{self, Picker, Theme},
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
    pub theme: Theme,
    pub theme_id: String,
    pub theme_warning: Option<String>,
    pub theme_picker: Option<Picker>,
    theme_config_path: Option<std::path::PathBuf>,
    pub keys: KeyMap,
    pub folders: Vec<Folder>,
    pub folder_cursor: usize,
    pub active_folder: String,
    pub messages: Vec<Message>,
    pub groups: Vec<Vec<usize>>,
    pub thread_cursor: usize,
    pub list_cursor: usize,
    pub rows: Vec<Row>,
    pub time_grouping: bool,
    pub collapsed: HashSet<Period>,
    grouping_date: chrono::NaiveDate,
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
    browser: Browser,
    browser_pending: HashSet<MessageKey>,
}

impl App {
    pub fn new(config: Config, demo: bool, offline: bool) -> Result<Self> {
        Ok(Self {
            theme: Theme::default(),
            theme_id: "default".into(),
            theme_warning: None,
            theme_picker: None,
            theme_config_path: None,
            keys: KeyMap::new(&config.keybindings.contexts, config.keybindings.timeout_ms)?,
            time_grouping: config.display.time_grouping,
            config,
            folders: vec![],
            folder_cursor: 0,
            active_folder: "INBOX".into(),
            messages: vec![],
            groups: vec![],
            thread_cursor: 0,
            list_cursor: 0,
            rows: vec![],
            collapsed: HashSet::new(),
            grouping_date: chrono::Local::now().date_naive(),
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
            browser: Browser::default(),
            browser_pending: HashSet::new(),
            quit: false,
            page_height: 20,
        })
    }

    pub fn context(&self) -> &'static str {
        if self.theme_picker.is_some() {
            "theme_picker"
        } else if self.prompt.is_some() {
            "prompt"
        } else {
            match self.view {
                View::Folders => "folders",
                View::Threads => "threads",
                View::Message => "message",
            }
        }
    }

    pub fn load_themes(&mut self, path: &std::path::Path) {
        self.theme_config_path = Some(path.to_path_buf());
        let catalog = theme::discover(Some(path));
        let requested = &self.config.display.theme;
        let mut warnings = catalog.diagnostics;
        if let Some(entry) = catalog
            .entries
            .iter()
            .find(|e| &e.id == requested && e.theme.is_some())
        {
            self.theme = entry.theme.clone().unwrap();
            self.theme_id = entry.id.clone();
        } else {
            self.theme = Theme::default();
            self.theme_id = "default".into();
            warnings.insert(
                0,
                format!("Unknown or invalid theme {requested:?}; using default"),
            );
        }
        self.theme_warning = if warnings.is_empty() {
            None
        } else {
            Some(warnings.join("; "))
        };
        if let Some(warning) = &self.theme_warning {
            self.status(format!("Theme warning: {warning}"), true);
        }
    }

    fn open_theme_picker(&mut self) {
        let catalog = theme::discover(self.theme_config_path.as_deref());
        let picker = Picker::new(catalog, self.theme.clone(), self.theme_id.clone());
        self.theme = picker.preview();
        self.theme_picker = Some(picker);
        self.keys.reset();
    }

    fn theme_picker_command(&mut self, command: &str) {
        let Some(mut picker) = self.theme_picker.take() else {
            return;
        };
        match command {
            "theme_picker.cancel" => {
                self.theme = picker.original;
                self.theme_id = picker.original_id;
                self.keys.reset();
                return;
            }
            "theme_picker.accept" => {
                if let Some(entry) = picker.entry().filter(|e| e.theme.is_some()) {
                    let saved = if self.demo {
                        Ok(())
                    } else {
                        self.theme_config_path
                            .as_deref()
                            .context("No active configuration file")
                            .and_then(|path| theme::save_selection(path, &entry.id))
                    };
                    match saved {
                        Ok(()) => {
                            self.theme_warning = None;
                            self.theme_id = entry.id.clone();
                            self.config.display.theme = entry.id.clone();
                            self.theme = entry.theme.clone().unwrap();
                            self.status(
                                format!(
                                    "Theme: {}{}",
                                    entry.name,
                                    if self.demo {
                                        " (demo session only)"
                                    } else {
                                        ""
                                    }
                                ),
                                false,
                            );
                            self.keys.reset();
                            return;
                        }
                        Err(error) => {
                            picker.error = Some(format!("Could not save theme: {error:#}"))
                        }
                    }
                }
            }
            "theme_picker.next" => {
                picker.selected = (picker.selected + 1).min(picker.matches.len().saturating_sub(1))
            }
            "theme_picker.previous" => picker.selected = picker.selected.saturating_sub(1),
            "theme_picker.delete" => {
                picker.query.pop();
                picker.filter();
            }
            _ => {}
        }
        self.theme = picker.preview();
        self.theme_picker = Some(picker);
    }

    pub fn theme_picker_key(&mut self, key: KeyEvent) {
        if let Some(command) = self.keys.feed("theme_picker", key, Instant::now()) {
            self.theme_picker_command(&command);
        } else if let KeyCode::Char(c) = key.code
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && let Some(picker) = self.theme_picker.as_mut()
        {
            // Sequence prefixes are reserved for mapped commands, as in text prompts.
            if self.keys.is_pending() {
                return;
            }
            picker.query.push(c);
            picker.filter();
            self.theme = picker.preview();
        }
    }

    pub fn selected(&self) -> Option<&Message> {
        if self.view == View::Threads
            && matches!(self.rows.get(self.list_cursor), Some(Row::Heading(..)))
        {
            return None;
        }
        let group = self.groups.get(self.thread_cursor)?;
        let index = if self.view == View::Message {
            self.message_cursor
        } else {
            group.len().saturating_sub(1)
        };
        self.messages.get(*group.get(index)?)
    }

    pub fn set_messages(&mut self, messages: Vec<Message>) {
        let selected = self.selected().map(message_key);
        let heading = if self.view == View::Threads {
            match self.rows.get(self.list_cursor) {
                Some(Row::Heading(period, _)) => Some(*period),
                _ => None,
            }
        } else {
            None
        };
        self.messages = messages;
        self.rebuild();
        if let Some(key) = selected {
            if let Some((group, offset)) = self.groups.iter().enumerate().find_map(|(g, group)| {
                group
                    .iter()
                    .position(|&i| message_key(&self.messages[i]) == key)
                    .map(|o| (g, o))
            }) {
                self.thread_cursor = group;
                self.message_cursor = offset;
                self.reveal_thread();
            } else if self.view == View::Message {
                self.view = View::Threads;
            }
        }
        if heading.is_some() {
            self.rebuild_rows(heading);
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
        if self.view == View::Message {
            self.collapsed.remove(&self.period(self.thread_cursor));
        }
        self.rebuild_rows(None);
    }

    fn period(&self, group: usize) -> Period {
        self.groups
            .get(group)
            .and_then(|g| g.last())
            .and_then(|&i| self.messages.get(i))
            .map_or(Period::Unknown, |m| {
                local_period(m.timestamp, self.grouping_date)
            })
    }

    fn rebuild_rows(&mut self, heading: Option<Period>) {
        self.rows.clear();
        if self.time_grouping {
            for period in Period::ALL {
                let groups: Vec<_> = (0..self.groups.len())
                    .filter(|&g| self.period(g) == period)
                    .collect();
                if groups.is_empty() {
                    continue;
                }
                self.rows.push(Row::Heading(period, groups.len()));
                if !self.collapsed.contains(&period) {
                    self.rows.extend(groups.into_iter().map(Row::Conversation));
                }
            }
        } else {
            self.rows
                .extend((0..self.groups.len()).map(Row::Conversation));
        }
        let target = self.rows.iter().position(|row| match row {
            Row::Heading(period, _) => heading == Some(*period),
            Row::Conversation(group) => heading.is_none() && *group == self.thread_cursor,
        });
        self.list_cursor =
            target.unwrap_or_else(|| self.list_cursor.min(self.rows.len().saturating_sub(1)));
        self.sync_list_selection();
    }

    fn sync_list_selection(&mut self) {
        if let Some(Row::Conversation(group)) = self.rows.get(self.list_cursor) {
            self.thread_cursor = *group;
        }
    }

    fn reveal_thread(&mut self) {
        self.collapsed.remove(&self.period(self.thread_cursor));
        self.rebuild_rows(None);
    }

    pub fn check_grouping_date(&mut self, date: chrono::NaiveDate) -> bool {
        if self.grouping_date == date {
            return false;
        }
        let heading = match self.rows.get(self.list_cursor) {
            Some(Row::Heading(period, _)) => Some(*period),
            _ => None,
        };
        self.grouping_date = date;
        if heading.is_none() {
            self.reveal_thread();
        } else {
            self.rebuild_rows(heading);
        }
        true
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
                if message.html_loaded && self.browser_pending.remove(&message_key(&message)) {
                    self.open_html(&message);
                }
                if message.body.is_some()
                    && (!self.browser_pending.contains(&message_key(&message))
                        || message.html_loaded)
                {
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
                if self.browser_pending.remove(&key) {
                    self.status(
                        format!("Cannot open HTML email: {error}. Press b to retry."),
                        true,
                    );
                }
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

    fn open_html(&mut self, message: &Message) {
        let Some(html) = message.html_body.as_deref() else {
            self.status("This message has no HTML body.", false);
            return;
        };
        match self.browser.open(html) {
            Ok(()) => self.status("Opening HTML email in your default browser…", false),
            Err(error) => self.status(format!("Cannot open HTML email: {error:#}"), true),
        }
    }

    fn open_browser(&mut self, worker: Option<&Worker>) {
        let Some(message) = self.selected().cloned() else {
            return;
        };
        if message.html_body.is_some() || message.html_loaded {
            self.open_html(&message);
        } else if self.offline {
            self.status(
                "HTML is not cached. Reconnect and press b to download it.",
                true,
            );
        } else if self.demo {
            self.status("This demo message has no HTML body.", false);
        } else {
            self.load_body(worker, &message);
            let key = message_key(&message);
            match self.body_states.get(&key) {
                Some(BodyState::Loading) => {
                    self.browser_pending.insert(key);
                    self.status("Downloading HTML email to open in your browser…", false);
                }
                Some(BodyState::Failed(error)) => {
                    self.status(
                        format!("Cannot open HTML email: {error}. Press b to retry."),
                        true,
                    );
                }
                None => {}
            }
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
        if self.theme_picker.is_some() {
            self.theme_picker_command(command);
            return None;
        }
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
            "theme.select" if self.prompt.is_none() => self.open_theme_picker(),
            "threads.toggle_time_grouping" if self.view == View::Threads => {
                self.time_grouping = !self.time_grouping;
                self.reveal_thread();
                self.status(
                    if self.time_grouping {
                        "Time grouping enabled"
                    } else {
                        "Time grouping disabled"
                    },
                    false,
                );
            }
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
                    View::Threads => self.rows.len().saturating_sub(1),
                    View::Message => self
                        .selected()
                        .and_then(|m| m.body.as_ref())
                        .map_or(0, |s| s.chars().count().min(u16::MAX as usize)),
                };
                let cursor = match self.view {
                    View::Folders => &mut self.folder_cursor,
                    View::Threads => &mut self.list_cursor,
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
                if self.view == View::Threads {
                    self.sync_list_selection();
                }
            }
            "open" => match self.view {
                View::Folders => {
                    if let Some(folder) = self.folders.get(self.folder_cursor) {
                        let name = folder.name.clone();
                        if self.active_folder != name {
                            self.messages.clear();
                            self.groups.clear();
                            self.rows.clear();
                            self.list_cursor = 0;
                            self.collapsed.clear();
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
                View::Threads
                    if matches!(self.rows.get(self.list_cursor), Some(Row::Heading(..))) =>
                {
                    if let Some(Row::Heading(period, _)) = self.rows.get(self.list_cursor).copied()
                    {
                        if !self.collapsed.remove(&period) {
                            self.collapsed.insert(period);
                        }
                        self.rebuild_rows(Some(period));
                    }
                }
                View::Threads if !self.groups.is_empty() => {
                    self.message_cursor = self.groups[self.thread_cursor].len().saturating_sub(1);
                    self.open_message(worker);
                }
                _ => {}
            },
            "back" => match self.view {
                View::Message => {
                    self.view = View::Threads;
                    self.reveal_thread();
                }
                View::Threads if !self.search.is_empty() => {
                    self.search.clear();
                    self.search_ids = None;
                    self.rebuild();
                }
                View::Threads => self.view = View::Folders,
                View::Folders => {}
            },
            "search" => {
                self.prompt = Some(Prompt::Search(self.search.clone()));
                self.keys.reset();
            }
            "search.next" | "search.previous" => {
                if self.view == View::Threads {
                    let candidates: Vec<_> = self
                        .rows
                        .iter()
                        .enumerate()
                        .filter_map(|(i, row)| matches!(row, Row::Conversation(_)).then_some(i))
                        .collect();
                    let next = if command == "search.next" {
                        candidates
                            .iter()
                            .copied()
                            .find(|&i| i > self.list_cursor)
                            .or_else(|| candidates.first().copied())
                    } else {
                        candidates
                            .iter()
                            .copied()
                            .rev()
                            .find(|&i| i < self.list_cursor)
                            .or_else(|| candidates.last().copied())
                    };
                    if let Some(next) = next {
                        self.list_cursor = next;
                        self.sync_list_selection();
                    }
                } else if self.view == View::Message && !self.groups.is_empty() {
                    self.thread_cursor = if command == "search.next" {
                        (self.thread_cursor + 1) % self.groups.len()
                    } else {
                        (self.thread_cursor + self.groups.len() - 1) % self.groups.len()
                    };
                    self.reveal_thread();
                    self.message_cursor = self.groups[self.thread_cursor].len().saturating_sub(1);
                    self.open_message(worker);
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
            "message.open_browser" if self.view == View::Message => self.open_browser(worker),
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
        self.collapsed.clear();
        self.thread_cursor = 0;
        self.list_cursor = 0;
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
    let now = chrono::Local::now();
    let today = now.date_naive();
    let timestamp = today
        .and_hms_opt(0, 0, 0)
        .and_then(|date| date.and_local_timezone(chrono::Local).earliest())
        .map_or(now.timestamp(), |date| date.timestamp());
    let first = Message {
        uid: 1, folder: "INBOX".into(), uid_validity: 1, message_id: "welcome@terminal-email".into(),
        from: "TerminalEmail <hello@example.org>".into(), to: "you@example.com".into(),
        subject: "Welcome to your terminal inbox".into(), date: chrono::DateTime::from_timestamp(timestamp, 0).unwrap().with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string(), timestamp,
        body: Some("Your inbox, at the speed of a keystroke.\n\nUse j/k to move, Enter or l to open, h or Esc to go back, q to quit.\n\ngg and G jump to the first and last item. Ctrl-d/u scroll half a page.\n\nPress ? for every configured binding. Remap keys in config.toml.\n\nThis is an offline demo. No mail server is contacted.\n\nYou can save the attached example with S.\n\nHappy reading!".into()),
        html_body: Some("<html><body><h1>Welcome to TerminalEmail</h1><p>Your inbox, at the speed of a keystroke.</p><p>Press <b>b</b> in the message view to open HTML in your browser.</p></body></html>".into()),
        html_loaded: true,
        attachments: vec![Attachment { name: "welcome.txt".into(), content_type: "text/plain".into(), data: b"Welcome to TerminalEmail!\n".to_vec() }], ..Default::default()
    };
    let reply = Message { uid: 2, message_id: "reply@terminal-email".into(), references: vec![first.message_id.clone()], subject: "Re: Welcome to your terminal inbox".into(), timestamp: timestamp + 1, body: Some("Conversations stay together.\n\nUse [ and ] in the message view to move between messages in this thread.\n\nPress / to search the locally cached sender, recipients, subject, and message text.".into()), attachments: vec![], html_body: None, ..first.clone() };
    let other = Message { uid: 3, message_id: "keys@terminal-email".into(), subject: "Make it yours: configurable Vim bindings".into(), timestamp: timestamp + 2, body: Some("Keybindings are commands grouped by context: global, folders, threads, message, and prompt.\n\nExample TOML:\n\n[keybindings.global]\n\"j\" = \"cursor.down\"\n\"gg\" = \"cursor.first\"\n\nThe default sequence timeout is 750 ms.\n\nCreate an account with: terminal-email init".into()), attachments: vec![], html_body: None, ..first.clone() };
    let mut messages = vec![other, reply, first];
    for message in &mut messages {
        message.date = chrono::DateTime::from_timestamp(message.timestamp, 0)
            .unwrap()
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M")
            .to_string();
    }
    app.set_messages(messages);
    app.status("DEMO · Enter opens INBOX · ? help · q quit", false);
    Ok(app)
}

pub fn run_demo(path: &std::path::Path) -> Result<()> {
    let mut app = demo_app()?;
    app.load_themes(path);
    event_loop(app, None)
}

pub fn run(config: Config, password: String, config_path: &std::path::Path) -> Result<()> {
    let path = crate::config::cache_path(&config)?;
    let offline = password.is_empty();
    let mut app = App::new(config.clone(), false, offline)?;
    app.load_themes(config_path);
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
        if app.check_grouping_date(chrono::Local::now().date_naive()) {
            dirty = true;
        }
        while let Some(result) = app.browser.poll() {
            match result {
                Ok(()) => app.status("HTML email opened in your default browser.", false),
                Err(error) => app.status(format!("Cannot open HTML email: {error:#}"), true),
            }
            dirty = true;
        }
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
                    if app.theme_picker.is_some() {
                        app.theme_picker_key(key);
                        continue;
                    }
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

    fn picker_key(app: &mut App, code: KeyCode) {
        app.theme_picker_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn search_theme(app: &mut App, query: &str) {
        let picker = app.theme_picker.as_mut().unwrap();
        picker.query.clear();
        picker.filter();
        for ch in query.chars() {
            picker_key(app, KeyCode::Char(ch));
        }
    }

    #[test]
    fn theme_picker_previews_cancels_and_owns_printable_input() {
        let mut app = demo_app().unwrap();
        app.view = View::Message;
        app.scroll = 3;
        let original = app.theme.clone();
        app.command("theme.select", None);
        assert_eq!(app.context(), "theme_picker");
        search_theme(&mut app, "light");
        assert_ne!(app.theme, original);
        assert_eq!(app.theme_id, "default");
        search_theme(&mut app, "jkq");
        assert_eq!(app.theme_picker.as_ref().unwrap().query, "jkq");
        assert!(!app.quit);
        assert_eq!(app.theme, original);
        picker_key(&mut app, KeyCode::Enter);
        assert!(app.theme_picker.is_some());
        picker_key(&mut app, KeyCode::Esc);
        assert_eq!(app.theme, original);
        assert_eq!(app.context(), "message");
        assert_eq!(app.scroll, 3);
    }

    #[test]
    fn theme_selection_saves_and_restores_on_startup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("alternate.toml");
        std::fs::write(&path, include_str!("../config.example.toml")).unwrap();
        let mut app = demo_app().unwrap();
        app.demo = false;
        app.load_themes(&path);
        app.command("theme.select", None);
        search_theme(&mut app, "light");
        picker_key(&mut app, KeyCode::Enter);
        assert!(app.theme_picker.is_none());
        assert_eq!(app.theme_id, "light");
        let config = Config::load(&path).unwrap();
        assert_eq!(config.display.theme, "light");
        let mut restarted = App::new(config, false, true).unwrap();
        restarted.load_themes(&path);
        assert_eq!(restarted.theme, app.theme);
        assert_eq!(restarted.theme_id, "light");
    }

    #[test]
    fn theme_picker_save_failure_can_be_cancelled_and_demo_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.toml");
        let mut app = demo_app().unwrap();
        app.load_themes(&missing);
        app.demo = false;
        let original = app.theme.clone();
        app.command("theme.select", None);
        search_theme(&mut app, "light");
        picker_key(&mut app, KeyCode::Enter);
        assert!(
            app.theme_picker
                .as_ref()
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("Could not save")
        );
        assert_ne!(app.theme, original);
        picker_key(&mut app, KeyCode::Esc);
        assert_eq!(app.theme, original);
        app.demo = true;
        app.command("theme.select", None);
        search_theme(&mut app, "light");
        picker_key(&mut app, KeyCode::Enter);
        assert!(app.theme_picker.is_none());
        assert_eq!(app.theme_id, "light");
        assert!(!missing.exists());
    }

    #[test]
    fn theme_picker_reloads_custom_files_and_handles_invalid_startup_theme() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::create_dir(theme::directory(&path)).unwrap();
        let custom = theme::directory(&path).join("custom.toml");
        std::fs::write(&custom, "name = 'First'").unwrap();
        let mut app = demo_app().unwrap();
        app.config.display.theme = "missing".into();
        app.load_themes(&path);
        assert!(app.error);
        assert_eq!(app.theme_id, "default");
        app.command("theme.select", None);
        search_theme(&mut app, "custom");
        assert_eq!(app.theme.name, "First");
        picker_key(&mut app, KeyCode::Esc);
        std::fs::write(&custom, "name = 'Second'").unwrap();
        app.command("theme.select", None);
        search_theme(&mut app, "custom");
        assert_eq!(app.theme.name, "Second");
        picker_key(&mut app, KeyCode::Esc);
        std::fs::write(&custom, "invalid!").unwrap();
        app.command("theme.select", None);
        search_theme(&mut app, "custom");
        assert_eq!(app.theme, Theme::default());
        picker_key(&mut app, KeyCode::Enter);
        assert!(app.theme_picker.is_some());
        assert!(
            app.theme_picker
                .as_ref()
                .unwrap()
                .entry()
                .unwrap()
                .error
                .is_some()
        );
    }

    #[test]
    fn theme_picker_remapped_sequences_and_timeout_dispatch() {
        let mut app = demo_app().unwrap();
        let bindings = [(
            "theme_picker".into(),
            [
                ("Enter".into(), "unbound".into()),
                ("Ctrl-y".into(), "theme_picker.accept".into()),
                ("Ctrl-x".into(), "theme_picker.next".into()),
                ("Ctrl-x Ctrl-p".into(), "theme_picker.previous".into()),
            ]
            .into(),
        )]
        .into();
        app.keys = KeyMap::new(&bindings, 50).unwrap();
        app.command("theme.select", None);
        let original_selected = app.theme_picker.as_ref().unwrap().selected;
        app.theme_picker_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
        let command = app
            .keys
            .expire("theme_picker", Instant::now() + Duration::from_millis(100))
            .unwrap();
        app.command(&command, None);
        assert_eq!(
            app.theme_picker.as_ref().unwrap().selected,
            original_selected + 1
        );
        search_theme(&mut app, "light");
        picker_key(&mut app, KeyCode::Enter);
        assert!(app.theme_picker.is_some());
        app.theme_picker_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert!(app.theme_picker.is_none());
        assert!(!app.keys.is_pending());
        assert_eq!(app.theme_id, "light");
    }

    fn grouped_app() -> App {
        use chrono::TimeZone;
        let mut app = demo_app().unwrap();
        app.grouping_date = chrono::NaiveDate::from_ymd_opt(2026, 10, 17).unwrap();
        app.view = View::Threads;
        app.set_messages(vec![]);
        let messages = [(1, 17), (2, 16), (3, 14), (4, 2)]
            .into_iter()
            .map(|(uid, day)| Message {
                uid,
                folder: "INBOX".into(),
                uid_validity: 1,
                timestamp: chrono::Local
                    .with_ymd_and_hms(2026, 10, day, 12, 0, 0)
                    .earliest()
                    .unwrap()
                    .timestamp(),
                subject: format!("Message {uid}"),
                body: Some("test body".into()),
                ..Default::default()
            })
            .collect();
        app.set_messages(messages);
        app
    }

    #[test]
    fn headings_collapse_and_message_actions_are_harmless() {
        let mut app = grouped_app();
        assert_eq!(app.rows.len(), 8);
        assert_eq!(app.selected().unwrap().uid, 1);
        app.command("cursor.first", None);
        assert_eq!(app.rows[app.list_cursor], Row::Heading(Period::Today, 1));
        assert!(app.selected().is_none());
        let original_flags: Vec<_> = app.messages.iter().map(|m| (m.seen, m.starred)).collect();
        for command in [
            "toggle_read",
            "toggle_star",
            "archive",
            "trash",
            "reply",
            "reply_all",
            "forward",
            "attachment.save",
        ] {
            assert!(app.command(command, None).is_none());
        }
        assert_eq!(app.messages.len(), 4);
        assert_eq!(
            app.messages
                .iter()
                .map(|m| (m.seen, m.starred))
                .collect::<Vec<_>>(),
            original_flags
        );
        assert!(app.prompt.is_none());
        app.command("open", None);
        assert_eq!(app.view, View::Threads);
        assert!(app.collapsed.contains(&Period::Today));
        assert_eq!(app.rows.len(), 7);
        app.command("cursor.down", None);
        assert_eq!(
            app.rows[app.list_cursor],
            Row::Heading(Period::Yesterday, 1)
        );
        app.command("cursor.up", None);
        app.command("open", None);
        app.command("cursor.down", None);
        app.command("open", None);
        assert_eq!(app.view, View::Message);
        assert_eq!(app.selected().unwrap().uid, 1);
        app.command("back", None);
        assert_eq!(
            app.rows[app.list_cursor],
            Row::Conversation(app.thread_cursor)
        );
    }

    #[test]
    fn flat_and_grouped_modes_preserve_selection_and_config() {
        let mut app = grouped_app();
        app.command("search.next", None);
        let uid = app.selected().unwrap().uid;
        app.command("threads.toggle_time_grouping", None);
        assert!(!app.time_grouping);
        assert_eq!(app.rows.len(), 4);
        assert_eq!(app.selected().unwrap().uid, uid);
        assert!(app.config.display.time_grouping);
        app.collapsed.insert(Period::Yesterday);
        app.command("threads.toggle_time_grouping", None);
        assert_eq!(app.selected().unwrap().uid, uid);
        assert!(!app.collapsed.contains(&Period::Yesterday));

        let mut config = app.config.clone();
        config.display.time_grouping = false;
        let mut flat = App::new(config, true, false).unwrap();
        flat.set_messages(app.messages.clone());
        assert!(!flat.time_grouping);
        assert_eq!(flat.rows.len(), 4);
    }

    #[test]
    fn search_skips_collapsed_rows_and_expands_new_results() {
        let mut app = grouped_app();
        app.command("cursor.first", None);
        app.command("open", None);
        app.command("search.next", None);
        assert_eq!(app.selected().unwrap().uid, 2);
        app.command("search.previous", None);
        assert_eq!(app.selected().unwrap().uid, 4);
        app.command("open", None);
        app.command("search.next", None);
        assert_eq!(app.selected().unwrap().uid, 1);
        assert!(!app.collapsed.contains(&Period::Today));
        app.command("back", None);
        app.collapsed.insert(Period::Today);
        app.search("Message 1".into(), None);
        assert!(app.collapsed.is_empty());
        assert_eq!(app.groups.len(), 1);
        assert_eq!(app.selected().unwrap().uid, 1);
        app.command("back", None);
        assert_eq!(app.groups.len(), 4);
        app.search("no matching message".into(), None);
        assert!(app.rows.is_empty());
        app.command("cursor.last", None);
        app.command("open", None);
        app.command("search.next", None);
        assert!(app.selected().is_none());
    }

    #[test]
    fn refresh_and_midnight_reveal_selected_conversation() {
        let mut app = grouped_app();
        let mut messages = app.messages.clone();
        // Move the selected conversation into a currently collapsed section.
        app.collapsed.insert(Period::Yesterday);
        messages[0].timestamp = messages[1].timestamp;
        app.set_messages(messages.clone());
        assert_eq!(app.selected().unwrap().uid, 1);
        assert!(!app.collapsed.contains(&Period::Yesterday));
        app.collapsed.insert(Period::ThisWeek);
        let tomorrow = app.grouping_date.succ_opt().unwrap();
        assert!(app.check_grouping_date(tomorrow));
        assert_eq!(app.selected().unwrap().uid, 1);
        assert!(!app.collapsed.contains(&Period::ThisWeek));
        assert!(!app.check_grouping_date(tomorrow));
        // Refresh must preserve selected headings and their collapse state.
        app.command("cursor.first", None);
        let heading = app.rows[app.list_cursor];
        app.command("open", None);
        app.set_messages(messages);
        assert_eq!(app.rows[app.list_cursor], heading);
        assert!(app.selected().is_none());
        // Removing the selected conversation leaves a valid neighboring selection.
        app.command("open", None);
        app.command("cursor.down", None);
        let removed = app.selected().unwrap().uid;
        app.set_messages(
            app.messages
                .iter()
                .filter(|m| m.uid != removed)
                .cloned()
                .collect(),
        );
        assert!(app.list_cursor < app.rows.len());
        assert!(app.selected().is_none_or(|m| m.uid != removed));
    }

    #[test]
    fn folders_reset_collapse_state_and_keep_grouping_mode() {
        let mut app = grouped_app();
        app.command("threads.toggle_time_grouping", None);
        app.collapsed.insert(Period::Today);
        app.folders.push(Folder {
            name: "Other".into(),
            ..Default::default()
        });
        app.command("back", None);
        app.command("cursor.down", None);
        app.command("open", None);
        assert_eq!(app.active_folder, "Other");
        assert!(!app.time_grouping);
        assert!(app.collapsed.is_empty());
        assert!(app.rows.is_empty());
    }

    #[test]
    fn conversation_uses_newest_message_period_and_stays_intact() {
        let mut app = grouped_app();
        let mut messages = app.messages.clone();
        messages[0].message_id = "new".into();
        messages[3].references = vec!["new".into()];
        app.set_messages(messages);
        assert_eq!(app.groups.len(), 3);
        let group = app.groups.iter().position(|g| g.len() == 2).unwrap();
        assert_eq!(app.period(group), Period::Today);
        assert!(app.rows.contains(&Row::Heading(Period::Today, 1)));
        assert!(
            !app.rows
                .iter()
                .any(|row| matches!(row, Row::Heading(Period::ThisMonth, _)))
        );
    }

    #[test]
    fn browser_fetches_legacy_html_once_and_opens_requested_identity() {
        let (mut app, worker, mut requests) = reader_with_worker();
        app.browser = crate::browser::fake();
        let index = app.groups[app.thread_cursor][app.message_cursor];
        app.messages[index].body = Some("Previously cached text".into());
        app.messages[index].html_body = None;
        app.messages[index].html_loaded = false;
        let original = app.selected().unwrap().clone();
        app.command("message.open_browser", Some(&worker));
        assert!(matches!(requests.try_recv(), Ok(Request::Body(_))));
        // Flag updates reuse Body updates but may still contain legacy content.
        app.update(Update::Body(Box::new(original.clone())), Some(&worker));
        app.command("message.open_browser", Some(&worker));
        assert!(requests.try_recv().is_err());
        let mut loaded = original.clone();
        loaded.html_body = Some("<p>Original HTML</p>".into());
        loaded.html_loaded = true;
        let mut unrelated = loaded.clone();
        unrelated.uid_validity += 1;
        app.update(Update::Body(Box::new(unrelated)), Some(&worker));
        assert_eq!(app.browser.file_count(), 0);
        // Navigation while loading must not open the newly selected message.
        app.active_folder = "Other".into();
        app.update(Update::Body(Box::new(loaded.clone())), Some(&worker));
        assert_eq!(app.browser.file_count(), 1);
        app.update(Update::Body(Box::new(loaded)), Some(&worker));
        assert_eq!(app.browser.file_count(), 1);
        assert!(app.browser_pending.is_empty());
    }

    #[test]
    fn browser_handles_offline_plaintext_failure_and_retry() {
        let (mut app, worker, mut requests) = reader_with_worker();
        app.browser = crate::browser::fake();
        let index = app.groups[app.thread_cursor][app.message_cursor];
        app.messages[index].html_body = None;
        app.messages[index].html_loaded = false;
        let original = app.selected().unwrap().clone();
        app.offline = true;
        app.command("message.open_browser", Some(&worker));
        assert!(app.status.contains("Reconnect"));
        assert!(requests.try_recv().is_err());
        app.messages[index].html_body = Some("<p>Cached HTML</p>".into());
        app.command("message.open_browser", Some(&worker));
        assert_eq!(app.browser.file_count(), 1);
        app.messages[index].html_body = None;
        app.messages[index].html_loaded = true;
        app.command("message.open_browser", Some(&worker));
        assert!(app.status.contains("no HTML body"));
        app.messages[index].html_loaded = false;
        app.offline = false;
        // A full request queue must leave no pending browser launch.
        worker
            .requests
            .try_send(Request::Refresh("INBOX".into()))
            .unwrap();
        app.command("message.open_browser", Some(&worker));
        assert!(app.error);
        assert!(app.browser_pending.is_empty());
        requests.try_recv().unwrap();
        app.command("message.open_browser", Some(&worker));
        assert!(matches!(requests.try_recv(), Ok(Request::Body(_))));
        app.update(
            Update::BodyError {
                message: Box::new(original),
                error: "Connection lost".into(),
            },
            Some(&worker),
        );
        assert!(app.error);
        assert!(app.status.contains("Connection lost"));
        assert!(app.browser_pending.is_empty());
        app.command("message.open_browser", Some(&worker));
        assert!(matches!(requests.try_recv(), Ok(Request::Body(_))));
        let mut plain = app.selected().unwrap().clone();
        plain.body = Some("Plain text".into());
        plain.html_loaded = true;
        app.update(Update::Body(Box::new(plain)), Some(&worker));
        assert!(app.status.contains("no HTML body"));
        assert_eq!(app.browser.file_count(), 1);
    }

    #[test]
    fn demo_browser_shortcut_launches_html_from_the_message_view() {
        let mut app = demo_app().unwrap();
        app.browser = crate::browser::fake();
        app.command("open", None);
        app.command("cursor.down", None);
        app.command("open", None);
        app.command("message.previous", None);
        assert_eq!(app.selected().unwrap().uid, 1);
        let command = app
            .keys
            .feed(
                "message",
                KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE),
                Instant::now(),
            )
            .unwrap();
        app.command(&command, None);
        assert_eq!(app.browser.file_count(), 1);
        assert!(!app.error);
    }

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
    fn back_keys_navigate_without_quitting_and_q_quits_every_view() {
        for key in [KeyCode::Char('h'), KeyCode::Esc] {
            let mut app = demo_app().unwrap();
            app.command("open", None);
            app.command("open", None);
            assert_eq!(app.view, View::Message);
            app.command("help", None);
            for (view, help) in [
                (View::Message, false),
                (View::Threads, false),
                (View::Folders, false),
                (View::Folders, false),
                (View::Folders, false),
            ] {
                let command = app
                    .keys
                    .feed(
                        app.context(),
                        KeyEvent::new(key, KeyModifiers::NONE),
                        Instant::now(),
                    )
                    .unwrap();
                app.command(&command, None);
                assert_eq!(app.view, view);
                assert_eq!(app.help, help);
                assert!(!app.quit);
            }
        }
        for view in [View::Folders, View::Threads, View::Message] {
            for help in [false, true] {
                let mut app = demo_app().unwrap();
                app.view = view;
                app.help = help;
                let command = app
                    .keys
                    .feed(
                        app.context(),
                        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
                        Instant::now(),
                    )
                    .unwrap();
                app.command(&command, None);
                assert!(app.quit);
            }
        }
    }

    #[test]
    fn q_is_text_in_prompts() {
        let mut app = demo_app().unwrap();
        app.prompt = Some(Prompt::Search(String::new()));
        app.prompt_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE), None);
        assert!(matches!(app.prompt, Some(Prompt::Search(ref input)) if input == "q"));
        assert!(!app.quit);
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
        assert!(!app.quit);
        app.command("quit", None);
        assert!(app.quit);
    }
}
