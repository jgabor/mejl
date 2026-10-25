//! A snapshot of the environment variables mejl reads.
//!
//! Code below the CLI never calls `std::env::var` directly. It reads an
//! [`Env`] instead, so tests can pass any set of variables without touching
//! the process environment (`set_var` is unsafe in edition 2024 and races
//! with parallel tests).

use std::collections::BTreeMap;
use std::ffi::OsString;

/// Variables mejl reads, by name.
pub mod var {
    /// API token override for scripted use. Wins over the keyring.
    pub const MEJL_TOKEN: &str = "MEJL_TOKEN";
    /// Log filter, in `tracing_subscriber::EnvFilter` syntax.
    pub const MEJL_LOG: &str = "MEJL_LOG";
    /// Present and non-empty disables colour (no-color.org).
    pub const NO_COLOR: &str = "NO_COLOR";
    /// `truecolor` or `24bit` means 24-bit colour works.
    pub const COLORTERM: &str = "COLORTERM";
    /// The terminal type.
    pub const TERM: &str = "TERM";
    /// The terminal program name, often missing over SSH.
    pub const TERM_PROGRAM: &str = "TERM_PROGRAM";
    /// The terminal's version, set by iTerm2, WezTerm, VS Code, Ghostty and
    /// Apple Terminal.
    pub const TERM_PROGRAM_VERSION: &str = "TERM_PROGRAM_VERSION";
    /// Set inside tmux.
    pub const TMUX: &str = "TMUX";
    /// Set inside Zellij.
    pub const ZELLIJ: &str = "ZELLIJ";
    /// Set inside GNU screen.
    pub const STY: &str = "STY";
    /// Set in an SSH session with a terminal.
    pub const SSH_TTY: &str = "SSH_TTY";
    /// Preferred full-screen editor.
    pub const VISUAL: &str = "VISUAL";
    /// Fallback editor.
    pub const EDITOR: &str = "EDITOR";

    /// Every name above, for [`super::Env::from_process`].
    pub const ALL: &[&str] = &[
        MEJL_TOKEN,
        MEJL_LOG,
        NO_COLOR,
        COLORTERM,
        TERM,
        TERM_PROGRAM,
        TERM_PROGRAM_VERSION,
        TMUX,
        ZELLIJ,
        STY,
        SSH_TTY,
        VISUAL,
        EDITOR,
    ];
}

/// Environment variables captured once at startup.
///
/// Values that are not valid UTF-8 are kept out: every variable mejl reads
/// is text, and a garbled value would only cause confusing behaviour later.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Env {
    vars: BTreeMap<String, String>,
}

// MEJL_TOKEN holds a credential, so Debug lists names only.
impl std::fmt::Debug for Env {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.vars.keys()).finish()
    }
}

impl Env {
    /// Reads the variables in [`var::ALL`] from the process environment.
    pub fn from_process() -> Self {
        Self::from_os_pairs(
            var::ALL
                .iter()
                .filter_map(|name| std::env::var_os(name).map(|v| ((*name).into(), v))),
        )
    }

    /// Builds a snapshot from name and value pairs, for tests.
    pub fn from_pairs<K: Into<String>, V: Into<String>>(
        pairs: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        Env {
            vars: pairs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }

    fn from_os_pairs(pairs: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        Env {
            vars: pairs
                .into_iter()
                .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
                .collect(),
        }
    }

    /// The value of a variable, if set.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(String::as_str)
    }

    /// The value of a variable if it is set and not empty after trimming.
    pub fn non_empty(&self, name: &str) -> Option<&str> {
        self.get(name).map(str::trim).filter(|v| !v.is_empty())
    }

    /// Whether a variable is set at all, even to an empty string.
    pub fn is_set(&self, name: &str) -> bool {
        self.vars.contains_key(name)
    }

    /// Returns a copy with one variable set, for tests.
    pub fn with(mut self, name: &str, value: &str) -> Self {
        self.vars.insert(name.to_owned(), value.to_owned());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_empty_ignores_blank_values() {
        let env = Env::from_pairs([("A", "  "), ("B", " x ")]);
        assert_eq!(env.non_empty("A"), None);
        assert_eq!(env.non_empty("B"), Some("x"));
        assert!(env.is_set("A"));
        assert!(!env.is_set("C"));
    }

    #[test]
    fn non_utf8_values_are_dropped() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let env = Env::from_os_pairs([
                ("GOOD".into(), "yes".into()),
                ("BAD".into(), OsString::from_vec(vec![0xff, 0xfe])),
            ]);
            assert_eq!(env.get("GOOD"), Some("yes"));
            assert_eq!(env.get("BAD"), None);
        }
    }

    #[test]
    fn debug_never_shows_values() {
        let env = Env::from_pairs([(var::MEJL_TOKEN, "fmu1-secret-value")]);
        let shown = format!("{env:?}");
        assert!(shown.contains("MEJL_TOKEN"));
        assert!(!shown.contains("secret"));
    }
}
