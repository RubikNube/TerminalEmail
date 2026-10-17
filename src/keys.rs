use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub const COMMANDS: &[&str] = &[
    "threads.toggle_time_grouping",
    "cursor.down",
    "cursor.up",
    "cursor.first",
    "cursor.last",
    "page.down",
    "page.up",
    "open",
    "back",
    "quit",
    "help",
    "search",
    "search.next",
    "search.previous",
    "refresh",
    "compose",
    "reply",
    "reply_all",
    "forward",
    "toggle_read",
    "toggle_star",
    "archive",
    "trash",
    "message.next",
    "message.previous",
    "message.open_browser",
    "attachment.save",
    "prompt.accept",
    "prompt.cancel",
    "prompt.delete",
];

pub type Bindings = BTreeMap<String, BTreeMap<String, String>>;

pub fn defaults() -> Bindings {
    let mut bindings = Bindings::new();
    let global = bindings.entry("global".into()).or_default();
    for (key, command) in [
        ("j", "cursor.down"),
        ("k", "cursor.up"),
        ("gg", "cursor.first"),
        ("G", "cursor.last"),
        ("Ctrl-d", "page.down"),
        ("Ctrl-u", "page.up"),
        ("Enter", "open"),
        ("l", "open"),
        ("h", "back"),
        ("Esc", "back"),
        ("q", "back"),
        ("Ctrl-c", "quit"),
        ("?", "help"),
        ("/", "search"),
        ("n", "search.next"),
        ("N", "search.previous"),
        ("Ctrl-r", "refresh"),
        ("c", "compose"),
    ] {
        global.insert(key.into(), command.into());
    }
    for context in ["threads", "message"] {
        let map = bindings.entry(context.into()).or_default();
        for (key, command) in [
            ("r", "reply"),
            ("R", "reply_all"),
            ("f", "forward"),
            ("m", "toggle_read"),
            ("s", "toggle_star"),
            ("a", "archive"),
            ("d", "trash"),
        ] {
            map.insert(key.into(), command.into());
        }
    }
    bindings
        .entry("threads".into())
        .or_default()
        .insert("t".into(), "threads.toggle_time_grouping".into());
    let message = bindings.entry("message".into()).or_default();
    message.insert("]".into(), "message.next".into());
    message.insert("[".into(), "message.previous".into());
    message.insert("S".into(), "attachment.save".into());
    message.insert("b".into(), "message.open_browser".into());
    bindings.insert(
        "prompt".into(),
        [
            ("Enter".into(), "prompt.accept".into()),
            ("Esc".into(), "prompt.cancel".into()),
            ("Backspace".into(), "prompt.delete".into()),
        ]
        .into(),
    );
    bindings
}

pub fn parse_sequence(input: &str) -> Result<Vec<KeyEvent>> {
    if input.is_empty() {
        bail!("empty key sequence");
    }
    let special = |s: &str| -> Result<KeyEvent> {
        let mut modifiers = KeyModifiers::empty();
        let mut name = s;
        loop {
            if let Some(rest) = name.strip_prefix("Ctrl-") {
                modifiers |= KeyModifiers::CONTROL;
                name = rest;
            } else if let Some(rest) = name.strip_prefix("Alt-") {
                modifiers |= KeyModifiers::ALT;
                name = rest;
            } else if let Some(rest) = name.strip_prefix("Shift-") {
                modifiers |= KeyModifiers::SHIFT;
                name = rest;
            } else {
                break;
            }
        }
        let code = match name {
            "Enter" => KeyCode::Enter,
            "Esc" => KeyCode::Esc,
            "Tab" => KeyCode::Tab,
            "Backspace" => KeyCode::Backspace,
            "Space" => KeyCode::Char(' '),
            "Up" => KeyCode::Up,
            "Down" => KeyCode::Down,
            "Left" => KeyCode::Left,
            "Right" => KeyCode::Right,
            "Home" => KeyCode::Home,
            "End" => KeyCode::End,
            "PageDown" => KeyCode::PageDown,
            "PageUp" => KeyCode::PageUp,
            s if s.chars().count() == 1 => KeyCode::Char(s.chars().next().unwrap()),
            _ => bail!("invalid key token: {s}"),
        };
        Ok(normalize(KeyEvent::new(code, modifiers)))
    };
    // Named/modifier keys use whitespace-separated tokens. Plain Vim sequences
    // such as gg are parsed one character at a time.
    if input.contains(' ') {
        let sequence: Vec<_> = input
            .split_whitespace()
            .map(special)
            .collect::<Result<_>>()?;
        if sequence.is_empty() {
            bail!("empty key sequence; use Space for the space key");
        }
        Ok(sequence)
    } else if input.contains('-')
        || input.chars().count() > 1
            && matches!(
                input,
                "Enter"
                    | "Esc"
                    | "Tab"
                    | "Backspace"
                    | "Space"
                    | "Up"
                    | "Down"
                    | "Left"
                    | "Right"
                    | "Home"
                    | "End"
                    | "PageDown"
                    | "PageUp"
            )
    {
        Ok(vec![special(input)?])
    } else {
        input.chars().map(|c| special(&c.to_string())).collect()
    }
}

fn normalize(mut key: KeyEvent) -> KeyEvent {
    if let KeyCode::Char(c) = key.code
        && c.is_uppercase()
    {
        key.modifiers.remove(KeyModifiers::SHIFT);
    }
    KeyEvent::new(key.code, key.modifiers)
}

#[derive(Debug)]
pub struct KeyMap {
    pub bindings: Bindings,
    pending: Vec<KeyEvent>,
    deadline: Option<Instant>,
    timeout: Duration,
}

impl KeyMap {
    pub fn new(overrides: &Bindings, timeout_ms: u64) -> Result<Self> {
        let mut bindings = defaults();
        for (context, map) in overrides {
            if !["global", "folders", "threads", "message", "prompt"].contains(&context.as_str()) {
                bail!("keybindings.{context}: unknown context");
            }
            let mut sequences = Vec::new();
            for (key, command) in map {
                let sequence = parse_sequence(key)?;
                if sequences.contains(&sequence) {
                    bail!("keybindings.{context}: duplicate equivalent binding {key}");
                }
                sequences.push(sequence.clone());
                if !COMMANDS.contains(&command.as_str()) && command != "unbound" {
                    bail!("keybindings.{context}.{key}: unknown command {command}");
                }
                let effective = bindings.entry(context.clone()).or_default();
                effective
                    .retain(|key, _| parse_sequence(key).expect("validated default") != sequence);
                effective.insert(key.clone(), command.clone());
            }
        }
        Ok(Self {
            bindings,
            pending: vec![],
            deadline: None,
            timeout: Duration::from_millis(timeout_ms),
        })
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.deadline = None;
    }

    fn effective(&self, context: &str) -> Vec<(Vec<KeyEvent>, String)> {
        let mut result: Vec<(Vec<KeyEvent>, String)> = Vec::new();
        for scope in ["global", context] {
            if let Some(map) = self.bindings.get(scope) {
                for (key, command) in map {
                    let sequence = parse_sequence(key).expect("validated key");
                    result.retain(|(seq, _)| seq != &sequence);
                    result.push((sequence, command.clone()));
                }
            }
        }
        result
    }

    pub fn feed(&mut self, context: &str, key: KeyEvent, now: Instant) -> Option<String> {
        if key.code == KeyCode::Esc && !self.pending.is_empty() {
            self.reset();
            return None;
        }
        self.pending.push(normalize(key));
        let bindings = self.effective(context);
        let matching: Vec<_> = bindings
            .iter()
            .filter(|(seq, _)| seq.starts_with(&self.pending))
            .collect();
        if matching.is_empty() {
            self.reset();
            // Retry a nonmatching last key as the start of a new sequence.
            self.pending.push(normalize(key));
            if !bindings
                .iter()
                .any(|(seq, _)| seq.starts_with(&self.pending))
            {
                self.reset();
                return None;
            }
        }
        let exact = bindings.iter().find(|(seq, _)| seq == &self.pending);
        let longer = bindings
            .iter()
            .any(|(seq, _)| seq.starts_with(&self.pending) && seq.len() > self.pending.len());
        if !longer {
            let result = exact
                .map(|(_, command)| command.clone())
                .filter(|s| s != "unbound");
            self.reset();
            result
        } else {
            self.deadline = Some(now + self.timeout);
            None
        }
    }

    pub fn expire(&mut self, context: &str, now: Instant) -> Option<String> {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            let result = self
                .effective(context)
                .into_iter()
                .find(|(seq, _)| seq == &self.pending)
                .map(|(_, command)| command)
                .filter(|s| s != "unbound");
            self.reset();
            result
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouping_binding_is_thread_only_and_remappable() {
        let mut map = KeyMap::new(&Bindings::new(), 750).unwrap();
        let now = Instant::now();
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        assert_eq!(
            map.feed("threads", key('t'), now).as_deref(),
            Some("threads.toggle_time_grouping")
        );
        assert_eq!(map.feed("message", key('t'), now), None);
        assert_eq!(map.feed("folders", key('t'), now), None);
        let overrides = [(
            "threads".into(),
            [
                ("t".into(), "unbound".into()),
                ("T".into(), "threads.toggle_time_grouping".into()),
            ]
            .into(),
        )]
        .into();
        let mut map = KeyMap::new(&overrides, 750).unwrap();
        assert_eq!(map.feed("threads", key('t'), now), None);
        assert_eq!(
            map.feed("threads", key('T'), now).as_deref(),
            Some("threads.toggle_time_grouping")
        );
    }

    #[test]
    fn browser_binding_is_message_only_and_remappable() {
        let mut map = KeyMap::new(&Bindings::new(), 750).unwrap();
        let now = Instant::now();
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        assert_eq!(map.feed("threads", key('b'), now), None);
        assert_eq!(
            map.feed("message", key('b'), now).as_deref(),
            Some("message.open_browser")
        );
        let overrides = [(
            "message".into(),
            [
                ("b".into(), "unbound".into()),
                ("v".into(), "message.open_browser".into()),
            ]
            .into(),
        )]
        .into();
        let mut map = KeyMap::new(&overrides, 750).unwrap();
        assert_eq!(map.feed("message", key('b'), now), None);
        assert_eq!(
            map.feed("message", key('v'), now).as_deref(),
            Some("message.open_browser")
        );
    }

    #[test]
    fn sequences_timeout_and_context_override() {
        let mut overrides = Bindings::new();
        overrides.insert(
            "threads".into(),
            [
                ("g".into(), "open".into()),
                ("j".into(), "cursor.up".into()),
            ]
            .into(),
        );
        let mut map = KeyMap::new(&overrides, 750).unwrap();
        let now = Instant::now();
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        assert_eq!(
            map.feed("threads", key('j'), now).as_deref(),
            Some("cursor.up")
        );
        assert_eq!(map.feed("threads", key('g'), now), None);
        assert_eq!(
            map.expire("threads", now + Duration::from_millis(751))
                .as_deref(),
            Some("open")
        );
        assert_eq!(map.feed("threads", key('g'), now), None);
        assert_eq!(
            map.feed("threads", key('g'), now).as_deref(),
            Some("cursor.first")
        );
    }

    #[test]
    fn rejects_unknown_commands_and_equivalent_sequences() {
        let overrides = [("global".into(), [("x".into(), "bogus".into())].into())].into();
        assert!(KeyMap::new(&overrides, 750).is_err());
        let overrides = [(
            "global".into(),
            [("g g".into(), "open".into()), ("gg".into(), "back".into())].into(),
        )]
        .into();
        assert!(KeyMap::new(&overrides, 750).is_err());
        assert!(parse_sequence("   ").is_err());
    }

    #[test]
    fn alternate_spelling_overrides_default_and_esc_cancels_prefix() {
        let overrides = [("global".into(), [("g g".into(), "open".into())].into())].into();
        let mut map = KeyMap::new(&overrides, 750).unwrap();
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let now = Instant::now();
        assert_eq!(map.feed("threads", key('g'), now), None);
        assert_eq!(
            map.feed(
                "threads",
                KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                now
            ),
            None
        );
        assert_eq!(map.feed("threads", key('g'), now), None);
        assert_eq!(map.feed("threads", key('g'), now).as_deref(), Some("open"));
    }
}
