//! Keys and key sequences: normalisation, parsing and display.
//!
//! Terminals report the same key in different ways. Legacy encoding sets
//! SHIFT on uppercase letters, the kitty protocol may also set it on `?` or
//! `*`, and Shift-Tab arrives as BackTab. [`Key::from_event`] folds all of
//! these into one canonical form, so a binding written as `"X"` or
//! `"shift-tab"` matches on every terminal.

use std::fmt;
use std::str::FromStr;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A key without modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KeyName {
    /// A printable character. Shift is already folded into it, so `X`
    /// means Shift+x. Space is `Char(' ')`.
    Char(char),
    /// Enter or Return.
    Enter,
    /// Escape.
    Esc,
    /// Tab.
    Tab,
    /// Shift+Tab.
    BackTab,
    /// Backspace.
    Backspace,
    /// Delete.
    Delete,
    /// Insert.
    Insert,
    /// Arrow up.
    Up,
    /// Arrow down.
    Down,
    /// Arrow left.
    Left,
    /// Arrow right.
    Right,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// A function key, F1 to F24.
    F(u8),
}

/// A key with modifiers, in canonical form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    /// The key.
    pub name: KeyName,
    /// Control held.
    pub ctrl: bool,
    /// Alt (Meta) held.
    pub alt: bool,
    /// Shift held, only for keys where Shift is not already part of the
    /// character: named keys, Space, and Ctrl+letter.
    pub shift: bool,
}

impl Key {
    /// A plain character key.
    pub const fn char(c: char) -> Key {
        Key {
            name: KeyName::Char(c),
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    /// A named key without modifiers.
    pub const fn named(name: KeyName) -> Key {
        Key {
            name,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    /// Ctrl plus a character.
    pub const fn ctrl(c: char) -> Key {
        Key {
            name: KeyName::Char(c),
            ctrl: true,
            alt: false,
            shift: false,
        }
    }

    /// The Escape key.
    pub const ESC: Key = Key::named(KeyName::Esc);

    /// Canonical form of a crossterm key event, or `None` for keys mejl
    /// never binds (media keys, modifier-only presses, and so on).
    pub fn from_event(event: &KeyEvent) -> Option<Key> {
        let m = event.modifiers;
        let ctrl = m.contains(KeyModifiers::CONTROL);
        let alt = m.contains(KeyModifiers::ALT);
        let shift = m.contains(KeyModifiers::SHIFT);
        let (name, shift) = match event.code {
            KeyCode::Char(c) if ctrl && c.is_ascii_alphabetic() => (
                KeyName::Char(c.to_ascii_lowercase()),
                shift || c.is_ascii_uppercase(),
            ),
            KeyCode::Char(' ') => (KeyName::Char(' '), shift),
            KeyCode::Char(c) => (KeyName::Char(c), false),
            KeyCode::Enter => (KeyName::Enter, shift),
            KeyCode::Esc => (KeyName::Esc, shift),
            KeyCode::Tab if shift => (KeyName::BackTab, false),
            KeyCode::Tab => (KeyName::Tab, false),
            KeyCode::BackTab => (KeyName::BackTab, false),
            KeyCode::Backspace => (KeyName::Backspace, shift),
            KeyCode::Delete => (KeyName::Delete, shift),
            KeyCode::Insert => (KeyName::Insert, shift),
            KeyCode::Up => (KeyName::Up, shift),
            KeyCode::Down => (KeyName::Down, shift),
            KeyCode::Left => (KeyName::Left, shift),
            KeyCode::Right => (KeyName::Right, shift),
            KeyCode::Home => (KeyName::Home, shift),
            KeyCode::End => (KeyName::End, shift),
            KeyCode::PageUp => (KeyName::PageUp, shift),
            KeyCode::PageDown => (KeyName::PageDown, shift),
            KeyCode::F(n) if (1..=24).contains(&n) => (KeyName::F(n), shift),
            _ => return None,
        };
        Some(Key {
            name,
            ctrl,
            alt,
            shift,
        })
    }

    /// The character typed, for text inputs: a printable key with no Ctrl
    /// or Alt.
    pub fn text(&self) -> Option<char> {
        match self.name {
            KeyName::Char(c) if !self.ctrl && !self.alt => Some(c),
            _ => None,
        }
    }

    /// Why legacy key encoding cannot deliver this key, if it cannot.
    /// Bindings with a reason still work on terminals with the kitty
    /// keyboard protocol, so they are warnings, not errors.
    pub fn legacy_problem(&self) -> Option<&'static str> {
        if self.alt {
            return Some(
                "Alt arrives as Esc followed by the key without the kitty keyboard protocol, \
                 and over SSH or tmux the two can split",
            );
        }
        match (self.name, self.ctrl, self.shift) {
            (KeyName::Char('i'), true, false) => {
                Some("Ctrl-i is the same byte as Tab in legacy encoding")
            }
            (KeyName::Char('m'), true, false) => {
                Some("Ctrl-m is the same byte as Enter in legacy encoding")
            }
            (KeyName::Char('h'), true, false) => {
                Some("Ctrl-h is sent as Backspace by many terminals")
            }
            (KeyName::Char('['), true, _) => Some("Ctrl-[ is the same byte as Esc"),
            (KeyName::Char(c), true, true) if c.is_ascii_alphabetic() => {
                Some("Ctrl+Shift+letter cannot be told apart from Ctrl+letter in legacy encoding")
            }
            (KeyName::Char(c), true, _) if c.is_ascii_digit() => {
                Some("most terminals send nothing for Ctrl with a digit in legacy encoding")
            }
            (KeyName::Char(c), true, _)
                if !c.is_ascii_alphabetic() && !matches!(c, ' ' | ']' | '\\' | '^' | '_' | '@') =>
            {
                Some("legacy encoding has no code for Ctrl with this character")
            }
            (KeyName::Char(' '), false, true) => {
                Some("Shift-Space is sent as Space in legacy encoding")
            }
            (KeyName::Enter | KeyName::Tab | KeyName::Backspace | KeyName::Esc, true, _) => {
                Some("Ctrl with this key needs the kitty keyboard protocol")
            }
            (KeyName::Enter | KeyName::Backspace | KeyName::Esc, false, true) => {
                Some("Shift with this key needs the kitty keyboard protocol")
            }
            _ => None,
        }
    }

    /// The key in config syntax, for example `ctrl-k`, `shift-tab`,
    /// `space` or `X`. Parsing the result gives the same key.
    pub fn to_config(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("ctrl-");
        }
        if self.alt {
            s.push_str("alt-");
        }
        if self.shift {
            s.push_str("shift-");
        }
        match self.name {
            KeyName::Char(' ') => s.push_str("space"),
            KeyName::Char(c) => s.push(c),
            KeyName::Enter => s.push_str("enter"),
            KeyName::Esc => s.push_str("esc"),
            KeyName::Tab => s.push_str("tab"),
            KeyName::BackTab => s.push_str("shift-tab"),
            KeyName::Backspace => s.push_str("backspace"),
            KeyName::Delete => s.push_str("delete"),
            KeyName::Insert => s.push_str("insert"),
            KeyName::Up => s.push_str("up"),
            KeyName::Down => s.push_str("down"),
            KeyName::Left => s.push_str("left"),
            KeyName::Right => s.push_str("right"),
            KeyName::Home => s.push_str("home"),
            KeyName::End => s.push_str("end"),
            KeyName::PageUp => s.push_str("pgup"),
            KeyName::PageDown => s.push_str("pgdn"),
            KeyName::F(n) => s.push_str(&format!("f{n}")),
        }
        s
    }
}

/// The short display form used in hint bars and popups: `j`, `^k`,
/// `Enter`, `S-Tab`, `PgDn`.
impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let named = !matches!(self.name, KeyName::Char(c) if c != ' ');
        if self.ctrl {
            f.write_str(if named { "Ctrl-" } else { "^" })?;
        }
        if self.alt {
            f.write_str("Alt-")?;
        }
        if self.shift {
            f.write_str("S-")?;
        }
        match self.name {
            KeyName::Char(' ') => f.write_str("Space"),
            KeyName::Char(c) => write!(f, "{c}"),
            KeyName::Enter => f.write_str("Enter"),
            KeyName::Esc => f.write_str("Esc"),
            KeyName::Tab => f.write_str("Tab"),
            KeyName::BackTab => f.write_str("S-Tab"),
            KeyName::Backspace => f.write_str("Bksp"),
            KeyName::Delete => f.write_str("Del"),
            KeyName::Insert => f.write_str("Ins"),
            KeyName::Up => f.write_str("Up"),
            KeyName::Down => f.write_str("Down"),
            KeyName::Left => f.write_str("Left"),
            KeyName::Right => f.write_str("Right"),
            KeyName::Home => f.write_str("Home"),
            KeyName::End => f.write_str("End"),
            KeyName::PageUp => f.write_str("PgUp"),
            KeyName::PageDown => f.write_str("PgDn"),
            KeyName::F(n) => write!(f, "F{n}"),
        }
    }
}

/// Why a key string did not parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyParseError(pub String);

impl fmt::Display for KeyParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for KeyParseError {}

impl FromStr for Key {
    type Err = KeyParseError;

    fn from_str(s: &str) -> Result<Key, KeyParseError> {
        if s.is_empty() {
            return Err(KeyParseError("empty key".into()));
        }
        let (mut ctrl, mut alt, mut shift) = (false, false, false);
        let mut rest = s;
        if let Some(r) = rest.strip_prefix('^').filter(|r| !r.is_empty()) {
            ctrl = true;
            rest = r;
        }
        loop {
            let lower = rest.to_ascii_lowercase();
            let prefix = ["ctrl-", "control-", "alt-", "meta-", "shift-"]
                .into_iter()
                .find(|p| lower.starts_with(p) && rest.len() > p.len());
            let Some(p) = prefix else { break };
            match p {
                "ctrl-" | "control-" => ctrl = true,
                "alt-" | "meta-" => alt = true,
                _ => shift = true,
            }
            rest = &rest[p.len()..];
        }
        let mut chars = rest.chars();
        let name = match (chars.next(), chars.next()) {
            (Some(c), None) => KeyName::Char(c),
            _ => match rest.to_ascii_lowercase().as_str() {
                "space" => KeyName::Char(' '),
                "enter" | "return" | "ret" => KeyName::Enter,
                "esc" | "escape" => KeyName::Esc,
                "tab" => KeyName::Tab,
                "backtab" => KeyName::BackTab,
                "backspace" | "bksp" | "bs" => KeyName::Backspace,
                "delete" | "del" => KeyName::Delete,
                "insert" | "ins" => KeyName::Insert,
                "up" => KeyName::Up,
                "down" => KeyName::Down,
                "left" => KeyName::Left,
                "right" => KeyName::Right,
                "home" => KeyName::Home,
                "end" => KeyName::End,
                "pgup" | "pageup" => KeyName::PageUp,
                "pgdn" | "pagedown" => KeyName::PageDown,
                other => match other.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                    Some(n) if (1..=24).contains(&n) => KeyName::F(n),
                    _ => {
                        return Err(KeyParseError(format!(
                            "unknown key name {rest:?}; use a single character or a name such \
                             as enter, esc, tab, space, up, pgdn or f1"
                        )));
                    }
                },
            },
        };
        let key = match name {
            KeyName::Char(c) if c.is_control() => {
                return Err(KeyParseError("control characters cannot be bound".into()));
            }
            KeyName::Char(c) if ctrl && c.is_ascii_alphabetic() => Key {
                name: KeyName::Char(c.to_ascii_lowercase()),
                ctrl,
                alt,
                shift: shift || c.is_ascii_uppercase(),
            },
            KeyName::Char(' ') => Key {
                name,
                ctrl,
                alt,
                shift,
            },
            KeyName::Char(c) if shift => {
                let upper: Vec<char> = c.to_uppercase().collect();
                match upper.as_slice() {
                    [u] if *u != c => Key {
                        name: KeyName::Char(*u),
                        ctrl,
                        alt,
                        shift: false,
                    },
                    _ => {
                        return Err(KeyParseError(format!(
                            "shift-{c} is ambiguous; write the shifted character itself"
                        )));
                    }
                }
            }
            KeyName::Char(_) => Key {
                name,
                ctrl,
                alt,
                shift: false,
            },
            KeyName::Tab if shift => Key {
                name: KeyName::BackTab,
                ctrl,
                alt,
                shift: false,
            },
            _ => Key {
                name,
                ctrl,
                alt,
                shift,
            },
        };
        Ok(key)
    }
}

/// A sequence of keys, such as `g i`. A prefix of a bound sequence has no
/// action of its own, so no binding ever waits on a timeout.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct KeySeq(pub Vec<Key>);

impl KeySeq {
    /// The keys.
    pub fn keys(&self) -> &[Key] {
        &self.0
    }

    /// True when `self` is a strict prefix of `other`.
    pub fn is_prefix_of(&self, other: &KeySeq) -> bool {
        self.0.len() < other.0.len() && other.0.starts_with(&self.0)
    }

    /// The sequence in config syntax, keys separated by spaces.
    pub fn to_config(&self) -> String {
        self.0
            .iter()
            .map(Key::to_config)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl From<Vec<Key>> for KeySeq {
    fn from(keys: Vec<Key>) -> Self {
        KeySeq(keys)
    }
}

impl fmt::Display for KeySeq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, k) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{k}")?;
        }
        Ok(())
    }
}

impl FromStr for KeySeq {
    type Err = KeyParseError;

    fn from_str(s: &str) -> Result<KeySeq, KeyParseError> {
        let keys = s
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<Vec<Key>, _>>()?;
        if keys.is_empty() {
            return Err(KeyParseError("empty key sequence".into()));
        }
        if keys.len() > 4 {
            return Err(KeyParseError(
                "key sequences are at most 4 keys long".into(),
            ));
        }
        Ok(KeySeq(keys))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn ev(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn shift_folds_into_characters() {
        let legacy = Key::from_event(&ev(KeyCode::Char('X'), KeyModifiers::SHIFT));
        let kitty = Key::from_event(&ev(KeyCode::Char('X'), KeyModifiers::NONE));
        assert_eq!(legacy, Some(Key::char('X')));
        assert_eq!(kitty, Some(Key::char('X')));
        let q = Key::from_event(&ev(KeyCode::Char('?'), KeyModifiers::SHIFT));
        assert_eq!(q, Some(Key::char('?')));
    }

    #[test]
    fn back_tab_and_ctrl_letters() {
        let bt = Key::from_event(&ev(KeyCode::BackTab, KeyModifiers::SHIFT));
        let st = Key::from_event(&ev(KeyCode::Tab, KeyModifiers::SHIFT));
        assert_eq!(bt, Some(Key::named(KeyName::BackTab)));
        assert_eq!(st, bt);
        let ck = Key::from_event(&ev(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(ck, Some(Key::ctrl('k')));
        let csk = Key::from_event(&ev(KeyCode::Char('K'), KeyModifiers::CONTROL));
        assert_eq!(csk.map(|k| k.shift), Some(true));
        assert_eq!(
            Key::from_event(&ev(KeyCode::Null, KeyModifiers::NONE)),
            None
        );
    }

    #[test]
    fn parse_and_format_round_trip() {
        for s in [
            "j",
            "J",
            "?",
            "*",
            ".",
            "ctrl-k",
            "ctrl-shift-k",
            "alt-x",
            "space",
            "shift-space",
            "enter",
            "esc",
            "tab",
            "shift-tab",
            "pgdn",
            "pgup",
            "f5",
            "-",
            "ctrl--",
            "\"",
            "#",
        ] {
            let k: Key = s.parse().unwrap_or_else(|e| panic!("{s}: {e}"));
            let again: Key = k.to_config().parse().unwrap();
            assert_eq!(k, again, "{s}");
        }
        assert_eq!("^k".parse::<Key>().unwrap(), Key::ctrl('k'));
        assert_eq!("shift-x".parse::<Key>().unwrap(), Key::char('X'));
        assert!("ctrl-K".parse::<Key>().unwrap().shift);
        assert!("".parse::<Key>().is_err());
        assert!("bogus".parse::<Key>().is_err());
        assert!("shift-1".parse::<Key>().is_err());
        assert!("f99".parse::<Key>().is_err());
    }

    #[test]
    fn display_is_short() {
        let shown: Vec<String> = [
            "j",
            "ctrl-k",
            "enter",
            "shift-tab",
            "pgdn",
            "space",
            "ctrl-enter",
        ]
        .iter()
        .map(|s| s.parse::<Key>().unwrap().to_string())
        .collect();
        assert_eq!(
            shown,
            ["j", "^k", "Enter", "S-Tab", "PgDn", "Space", "Ctrl-Enter"]
        );
        let seq: KeySeq = "g i".parse().unwrap();
        assert_eq!(seq.to_string(), "g i");
        assert_eq!(seq.to_config(), "g i");
    }

    #[test]
    fn sequences() {
        let g: KeySeq = "g".parse().unwrap();
        let gi: KeySeq = "g  i".parse().unwrap();
        assert!(g.is_prefix_of(&gi));
        assert!(!gi.is_prefix_of(&g));
        assert!(!g.is_prefix_of(&g));
        assert!("".parse::<KeySeq>().is_err());
        assert!("a b c d e".parse::<KeySeq>().is_err());
    }

    #[test]
    fn legacy_problems() {
        let bad = [
            "ctrl-i",
            "ctrl-m",
            "ctrl-h",
            "ctrl-[",
            "ctrl-shift-x",
            "alt-a",
            "shift-space",
            "ctrl-enter",
            "ctrl-1",
            "ctrl-;",
        ];
        for s in bad {
            assert!(s.parse::<Key>().unwrap().legacy_problem().is_some(), "{s}");
        }
        let good = [
            "ctrl-k",
            "ctrl-o",
            "ctrl-y",
            "j",
            "?",
            "enter",
            "shift-tab",
            "space",
            "ctrl-]",
        ];
        for s in good {
            assert!(s.parse::<Key>().unwrap().legacy_problem().is_none(), "{s}");
        }
    }
}
