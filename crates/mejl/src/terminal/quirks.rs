//! Terminals that need the probe handled differently.
//!
//! The probe asks the terminal what it can do, which works almost
//! everywhere. This table lists the few terminals where asking itself goes
//! wrong, keyed by `TERM` or `TERM_PROGRAM` because those are known before
//! anything is sent. Keep it short: every entry says why it is here and
//! whether that was verified or inferred.
//!
//! | Match | Effect | Why |
//! |---|---|---|
//! | `TERM=dumb` | No probe | A dumb terminal has no escape sequences, so it would print the queries as text (inferred from what `dumb` means) |
//! | `TERM=eterm*` | No probe | Emacs `term.el` answers none of the queries and shows unknown ones (inferred) |
//! | `TERM_PROGRAM=vscode` | 400 ms timeout | The slowest terminal measured: about 26 ms median for OSC 11, with a long tail (verified median: `docs/terminal-ux.md`; tail inferred) |
//!
//! A longer timeout costs nothing when the terminal is fast, since DA1 ends
//! the wait.

use std::time::Duration;

/// What to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuirkMatch {
    /// `TERM` is exactly this.
    Term(&'static str),
    /// `TERM` starts with this.
    TermPrefix(&'static str),
    /// `TERM_PROGRAM` is exactly this.
    TermProgram(&'static str),
}

/// One terminal's quirk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quirk {
    /// A short name for logs.
    pub name: &'static str,
    /// Which terminal.
    pub matches: QuirkMatch,
    /// Send no queries at all.
    pub skip_probe: bool,
    /// Wait at least this long for the probe.
    pub probe_timeout: Option<Duration>,
}

/// The table. The first match wins.
pub const QUIRKS: &[Quirk] = &[
    Quirk {
        name: "dumb",
        matches: QuirkMatch::Term("dumb"),
        skip_probe: true,
        probe_timeout: None,
    },
    Quirk {
        name: "emacs-term",
        matches: QuirkMatch::TermPrefix("eterm"),
        skip_probe: true,
        probe_timeout: None,
    },
    Quirk {
        name: "vscode",
        matches: QuirkMatch::TermProgram("vscode"),
        skip_probe: false,
        probe_timeout: Some(Duration::from_millis(400)),
    },
];

/// The quirk for this `TERM` and `TERM_PROGRAM`, if any.
pub fn lookup(term: Option<&str>, term_program: Option<&str>) -> Option<&'static Quirk> {
    QUIRKS.iter().find(|q| match q.matches {
        QuirkMatch::Term(t) => term == Some(t),
        QuirkMatch::TermPrefix(p) => term.is_some_and(|t| t.starts_with(p)),
        QuirkMatch::TermProgram(p) => term_program == Some(p),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_terminals_match() {
        assert!(lookup(Some("dumb"), None).is_some_and(|q| q.skip_probe));
        assert!(lookup(Some("eterm-color"), None).is_some_and(|q| q.skip_probe));
        let vscode = lookup(Some("xterm-256color"), Some("vscode")).unwrap();
        assert!(!vscode.skip_probe);
        assert_eq!(vscode.probe_timeout, Some(Duration::from_millis(400)));
    }

    #[test]
    fn other_terminals_have_no_quirk() {
        assert_eq!(lookup(Some("xterm-kitty"), Some("kitty")), None);
        assert_eq!(lookup(Some("dumber"), None), None);
        assert_eq!(lookup(None, None), None);
        // Only TERM_PROGRAM names VS Code; a TERM of that name does not.
        assert_eq!(lookup(Some("vscode"), None), None);
    }

    #[test]
    fn every_entry_does_something() {
        for q in QUIRKS {
            assert!(q.skip_probe || q.probe_timeout.is_some(), "{}", q.name);
        }
    }
}
