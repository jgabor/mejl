//! The terminal capability cache, `caps.toml` in the cache directory.
//!
//! The probe runs on every start. A slow terminal, such as one over SSH,
//! may not answer before the timeout, and mejl would then do without kitty
//! keys and synchronized output. When an earlier start on the same terminal
//! got full answers, they fill those gaps instead.
//!
//! - Only answers that belong to the terminal are kept: kitty keyboard,
//!   synchronized output, theme reports and the XTVERSION name. The
//!   background colour and the size change too often.
//! - Entries are keyed by `TERM`, `TERM_PROGRAM`, `TERM_PROGRAM_VERSION`,
//!   the multiplexer and SSH. A different terminal, or the same one after an
//!   upgrade that changes `TERM_PROGRAM_VERSION`, finds no entry and keeps
//!   only what its own probe found.
//! - When this run's XTVERSION reply differs from the entry's, the entry
//!   belongs to another terminal behind the same environment and is not
//!   used.
//! - Only a complete probe (DA1 arrived) writes an entry, and only when it
//!   changed. The newest entries are kept, up to [`MAX_ENTRIES`].
//! - The file is safe to delete. One that cannot be read or parsed, or has
//!   another version, counts as empty and is replaced on the next write.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::probe::{Capabilities, Multiplexer, ProbeStatus, Support};
use crate::env::Env;

/// The file format version. A file with another version is ignored.
pub const VERSION: u32 = 1;

/// The most terminals remembered.
pub const MAX_ENTRIES: usize = 16;

/// Set by iTerm2, WezTerm, VS Code, Ghostty and Apple Terminal.
const TERM_PROGRAM_VERSION: &str = "TERM_PROGRAM_VERSION";

/// The whole file.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CapsCache {
    /// [`VERSION`].
    pub version: u32,
    /// Newest first.
    #[serde(default, rename = "terminal")]
    pub entries: Vec<Entry>,
}

/// What one terminal answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// See [`key`].
    pub key: String,
    /// The XTVERSION reply, when the terminal sent one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_version: Option<String>,
    /// The kitty keyboard protocol.
    pub kitty_keyboard: Answer,
    /// Synchronized output (mode 2026).
    pub synchronized_output: Answer,
    /// Theme reports (mode 2031).
    pub theme_reports: Answer,
}

/// A definite answer. "Unknown" is never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Answer {
    /// Supported.
    Yes,
    /// Not supported.
    No,
}

impl Answer {
    fn from_support(s: Support) -> Option<Answer> {
        match s {
            Support::Yes => Some(Answer::Yes),
            Support::No => Some(Answer::No),
            Support::Unknown => None,
        }
    }

    fn support(self) -> Support {
        match self {
            Answer::Yes => Support::Yes,
            Answer::No => Support::No,
        }
    }
}

/// The cache key for the terminal described by `caps` and `env`. Only
/// environment values go in, so the key is known before the probe.
pub fn key(caps: &Capabilities, env: &Env) -> String {
    let mux = match caps.multiplexer {
        Some(Multiplexer::Tmux) => "tmux",
        Some(Multiplexer::Zellij) => "zellij",
        Some(Multiplexer::Screen) => "screen",
        None => "none",
    };
    format!(
        "term={};program={};version={};mux={mux};ssh={}",
        caps.term.as_deref().unwrap_or(""),
        caps.term_program.as_deref().unwrap_or(""),
        env.non_empty(TERM_PROGRAM_VERSION).unwrap_or(""),
        if caps.ssh { "yes" } else { "no" },
    )
}

impl CapsCache {
    /// Reads the file. Anything wrong with it gives an empty cache.
    pub fn load(path: &Path) -> CapsCache {
        let empty = CapsCache {
            version: VERSION,
            entries: Vec::new(),
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return empty;
        };
        match toml::from_str::<CapsCache>(&text) {
            Ok(cache) if cache.version == VERSION => cache,
            Ok(_) => empty,
            Err(e) => {
                tracing::debug!(error = %e, "ignoring an unreadable caps.toml");
                empty
            }
        }
    }

    /// The entry for `key`.
    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.key == key)
    }

    /// Records a complete probe's answers under `key`, newest first.
    /// Returns whether anything changed. Probes that did not complete, or
    /// that left an answer unknown, are not recorded.
    pub fn record(&mut self, key: &str, caps: &Capabilities) -> bool {
        if caps.probe != ProbeStatus::Complete {
            return false;
        }
        let (Some(kitty_keyboard), Some(synchronized_output), Some(theme_reports)) = (
            Answer::from_support(caps.kitty_keyboard),
            Answer::from_support(caps.synchronized_output),
            Answer::from_support(caps.theme_reports),
        ) else {
            return false;
        };
        let entry = Entry {
            key: key.to_owned(),
            terminal_version: caps.terminal_version.clone(),
            kitty_keyboard,
            synchronized_output,
            theme_reports,
        };
        if self.entries.first() == Some(&entry) {
            return false;
        }
        self.entries.retain(|e| e.key != key);
        self.entries.insert(0, entry);
        self.entries.truncate(MAX_ENTRIES);
        true
    }

    /// Drops the entry for `key`. Returns whether there was one.
    pub fn forget(&mut self, key: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.key != key);
        self.entries.len() != before
    }

    /// Writes the file atomically: a temporary file in the same directory,
    /// then a rename. Creates the directory when needed.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = toml::to_string(self).map_err(std::io::Error::other)?;
        let dir = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        std::io::Write::write_all(&mut tmp, text.as_bytes())?;
        tmp.persist(path).map_err(|e| e.error)?;
        Ok(())
    }
}

/// Fills answers the probe left unknown from `entry`, unless this run's
/// XTVERSION shows it is another terminal. Returns whether any answer came
/// from the entry.
pub fn fill_unknown(caps: &mut Capabilities, entry: &Entry) -> bool {
    if caps.terminal_version.is_some() && caps.terminal_version != entry.terminal_version {
        return false;
    }
    let mut used = false;
    for (field, cached) in [
        (&mut caps.kitty_keyboard, entry.kitty_keyboard),
        (&mut caps.synchronized_output, entry.synchronized_output),
        (&mut caps.theme_reports, entry.theme_reports),
    ] {
        if *field == Support::Unknown {
            *field = cached.support();
            used = true;
        }
    }
    if caps.terminal_version.is_none() && entry.terminal_version.is_some() {
        caps.terminal_version = entry.terminal_version.clone();
    }
    used
}

/// The cache step of [`super::probe::detect_with`]: after a complete probe
/// the file is updated; otherwise its entry fills in what is missing. A
/// failed write is logged and otherwise ignored, since the cache is only a
/// shortcut.
pub fn apply(caps: &mut Capabilities, env: &Env, path: &Path) {
    let key = key(caps, env);
    let mut cache = CapsCache::load(path);
    let changed = if caps.probe == ProbeStatus::Complete {
        cache.record(&key, caps)
    } else {
        match cache.get(&key).cloned() {
            Some(entry) if fill_unknown(caps, &entry) => {
                tracing::debug!("filled terminal answers from caps.toml");
                false
            }
            Some(entry)
                if caps.terminal_version.is_some()
                    && caps.terminal_version != entry.terminal_version =>
            {
                // Another terminal behind the same environment.
                cache.forget(&key)
            }
            _ => false,
        }
    };
    if changed && let Err(e) = cache.save(path) {
        tracing::warn!(error = %e, path = %path.display(), "cannot write caps.toml");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kitty_env() -> Env {
        Env::from_pairs([
            ("TERM", "xterm-kitty"),
            ("TERM_PROGRAM", "kitty"),
            ("TERM_PROGRAM_VERSION", "0.49.2"),
        ])
    }

    fn complete(env: &Env) -> Capabilities {
        Capabilities {
            kitty_keyboard: Support::Yes,
            synchronized_output: Support::Yes,
            theme_reports: Support::No,
            terminal_version: Some("kitty(0.49.2)".into()),
            probe: ProbeStatus::Complete,
            ..super::super::probe::from_env(env)
        }
    }

    fn timed_out(env: &Env) -> Capabilities {
        Capabilities {
            probe: ProbeStatus::TimedOut,
            ..super::super::probe::from_env(env)
        }
    }

    #[test]
    fn complete_probe_is_written_and_fills_a_later_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache").join("caps.toml");
        let env = kitty_env();
        let mut first = complete(&env);
        apply(&mut first, &env, &path);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("version = 1"), "{text}");
        assert!(text.contains("kitty_keyboard = \"yes\""), "{text}");
        assert!(!text.contains("background"), "{text}");

        let mut slow = timed_out(&env);
        apply(&mut slow, &env, &path);
        assert_eq!(slow.kitty_keyboard, Support::Yes);
        assert_eq!(slow.synchronized_output, Support::Yes);
        assert_eq!(slow.theme_reports, Support::No);
        assert_eq!(slow.terminal_version.as_deref(), Some("kitty(0.49.2)"));
        // The probe's own status is kept, so doctor can tell.
        assert_eq!(slow.probe, ProbeStatus::TimedOut);
    }

    #[test]
    fn a_changed_terminal_gets_no_cached_answers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caps.toml");
        let env = kitty_env();
        apply(&mut complete(&env), &env, &path);

        for other in [
            kitty_env().with("TERM_PROGRAM_VERSION", "0.50.0"),
            kitty_env().with("TERM", "xterm-256color"),
            kitty_env().with("TMUX", "/tmp/tmux-1/default,1,0"),
            kitty_env().with("SSH_TTY", "/dev/pts/1"),
        ] {
            let mut caps = timed_out(&other);
            apply(&mut caps, &other, &path);
            assert_eq!(caps.kitty_keyboard, Support::Unknown, "{other:?}");
        }
    }

    #[test]
    fn a_different_xtversion_drops_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caps.toml");
        let env = kitty_env();
        apply(&mut complete(&env), &env, &path);

        let mut caps = timed_out(&env);
        caps.terminal_version = Some("WezTerm 20240203".into());
        apply(&mut caps, &env, &path);
        assert_eq!(caps.kitty_keyboard, Support::Unknown);
        assert!(CapsCache::load(&path).get(&key(&caps, &env)).is_none());
    }

    #[test]
    fn only_complete_and_definite_probes_are_recorded() {
        let env = kitty_env();
        let mut cache = CapsCache::default();
        let k = key(&complete(&env), &env);
        assert!(!cache.record(&k, &timed_out(&env)));
        let mut partial = complete(&env);
        partial.theme_reports = Support::Unknown;
        assert!(!cache.record(&k, &partial));
        assert!(cache.record(&k, &complete(&env)));
        // The same answers again change nothing.
        assert!(!cache.record(&k, &complete(&env)));
    }

    #[test]
    fn newest_entries_are_kept() {
        let mut cache = CapsCache::default();
        for i in 0..MAX_ENTRIES + 3 {
            let env = kitty_env().with("TERM_PROGRAM_VERSION", &i.to_string());
            let caps = complete(&env);
            assert!(cache.record(&key(&caps, &env), &caps));
        }
        assert_eq!(cache.entries.len(), MAX_ENTRIES);
        assert!(
            cache.entries[0]
                .key
                .contains(&format!("version={}", MAX_ENTRIES + 2))
        );
    }

    #[test]
    fn bad_files_count_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caps.toml");
        assert_eq!(CapsCache::load(&path).entries, Vec::new());
        for text in [
            "not toml at all [[[",
            "version = 99\n[[terminal]]\nkey = \"x\"\nkitty_keyboard = \"yes\"\nsynchronized_output = \"yes\"\ntheme_reports = \"yes\"\n",
            "version = 1\n[[terminal]]\nkey = \"x\"\nkitty_keyboard = \"maybe\"\n",
        ] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(CapsCache::load(&path).entries, Vec::new(), "{text}");
        }
        // A broken file is replaced by the next complete probe.
        let env = kitty_env();
        apply(&mut complete(&env), &env, &path);
        assert_eq!(CapsCache::load(&path).entries.len(), 1);
    }

    #[test]
    fn key_names_every_part() {
        let env = kitty_env().with("TMUX", "x").with("SSH_TTY", "/dev/pts/2");
        let caps = super::super::probe::from_env(&env);
        assert_eq!(
            key(&caps, &env),
            "term=xterm-kitty;program=kitty;version=0.49.2;mux=tmux;ssh=yes"
        );
    }
}
