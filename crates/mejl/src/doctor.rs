//! `mejl doctor`: what mejl sees, in one screen.
//!
//! Prints the version, every path, the config check, each account's login
//! source, store sizes and lock state, and the terminal's capabilities. It
//! never prints a token: only where one would come from. It does not run
//! `token_command`, which might prompt or be slow. For an OAuth login it
//! shows the client id's source, the granted scope and the stored expiry,
//! and never refreshes: only the TUI and `mejl login` may.

use std::io::{self, Write};
use std::path::Path;

use crate::config::{AccountConfig, ConfigError, Loaded};
use crate::env::Env;
use crate::lock;
use crate::paths::{ACCOUNTS_DIR, Paths};
use crate::secrets::{LoginSource, SecretStore, login_source};
use crate::terminal::probe::{Background, Capabilities, Multiplexer, ProbeStatus};

/// The store files mejl-store keeps in an account folder.
pub const STORE_FILES: &[&str] = &mejl_store::STORE_FILES;

/// Everything the report needs.
pub struct DoctorInput<'a> {
    /// mejl's directories.
    pub paths: &'a Paths,
    /// The environment.
    pub env: &'a Env,
    /// The loaded config, or why it failed.
    pub config: Result<&'a Loaded, &'a ConfigError>,
    /// The account `--account` names, if any.
    pub account: Option<&'a str>,
    /// Where tokens are kept.
    pub store: &'a dyn SecretStore,
    /// The terminal.
    pub caps: &'a Capabilities,
}

/// What the report found, for the exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DoctorOutcome {
    /// The config has errors.
    pub config_errors: bool,
}

/// Writes the report.
pub fn report(input: &DoctorInput<'_>, out: &mut dyn Write) -> io::Result<DoctorOutcome> {
    let p = input.paths;
    writeln!(out, "mejl {}", env!("CARGO_PKG_VERSION"))?;
    writeln!(out)?;
    writeln!(out, "Paths")?;
    let config_state = match input.config {
        Ok(l) if l.from_file => "found",
        Ok(_) => "not found, defaults in use",
        Err(_) => "has errors",
    };
    row(
        out,
        "config file",
        &format!("{} ({config_state})", p.config_file.display()),
    )?;
    row(out, "data", &p.data_dir.display().to_string())?;
    row(out, "cache", &p.cache_dir.display().to_string())?;
    row(out, "state", &p.state_dir.display().to_string())?;
    row(out, "logs", &p.log_dir().display().to_string())?;
    writeln!(out)?;

    writeln!(out, "Config")?;
    let mut outcome = DoctorOutcome {
        config_errors: false,
    };
    match input.config {
        Ok(loaded) => {
            if loaded.warnings.is_empty() {
                writeln!(out, "  ok")?;
            }
            for w in &loaded.warnings {
                writeln!(out, "  warning: {w}")?;
            }
        }
        Err(e) => {
            outcome.config_errors = true;
            for line in e.to_string().lines() {
                writeln!(out, "  {line}")?;
            }
        }
    }
    writeln!(out)?;

    if let Ok(loaded) = input.config {
        let current = match loaded.config.account(input.account) {
            Ok(account) => {
                account_section(input, &account, out)?;
                Some(account.name)
            }
            Err(e) => {
                writeln!(out, "Account\n  {e}\n")?;
                None
            }
        };
        for name in loaded.config.account_names() {
            if current.as_deref() != Some(name.as_str()) {
                writeln!(out, "Other account {name:?}")?;
                store_rows(&p.account_dir(&name), out)?;
                writeln!(out)?;
            }
        }
    }
    orphan_dirs(input, out)?;
    terminal_section(input.caps, out)?;
    Ok(outcome)
}

fn row(out: &mut dyn Write, label: &str, value: &str) -> io::Result<()> {
    writeln!(out, "  {label:<15} {value}")
}

fn account_section(
    input: &DoctorInput<'_>,
    account: &AccountConfig,
    out: &mut dyn Write,
) -> io::Result<()> {
    writeln!(out, "Account {:?}", account.name)?;
    row(out, "session URL", &account.session_url)?;
    row(
        out,
        "username",
        account.username.as_deref().unwrap_or("(not set)"),
    )?;
    let found = login_source(account, input.env, input.store);
    let login = match &found {
        Ok(Some(LoginSource::Env)) => {
            "API token from MEJL_TOKEN (set in the environment)".to_owned()
        }
        Ok(Some(LoginSource::Command)) => {
            "API token from token_command (not run by doctor)".to_owned()
        }
        Ok(Some(LoginSource::Store(_))) => {
            format!("API token, found in the {}", input.store.describe())
        }
        Ok(Some(LoginSource::OAuth(_))) => {
            format!("OAuth, found in the {}", input.store.describe())
        }
        Ok(None) => "not found; run `mejl login`".to_owned(),
        Err(e) => format!("cannot check: {e}"),
    };
    row(out, "login", &login)?;
    if let Ok(Some(LoginSource::OAuth(record))) = &found {
        row(out, "client id", record.client_source.describe())?;
        row(out, "issuer", &record.issuer)?;
        let scope = if record.tokens.scope.is_empty() {
            "(the server did not say)".to_owned()
        } else {
            record.tokens.scope.join(" ")
        };
        row(out, "scope", &scope)?;
        row(
            out,
            "expires",
            &expiry(record.tokens.expires_at, jiff::Timestamp::now()),
        )?;
    }
    if account.oauth_client_id.is_some() && account.token_command.is_some() {
        row(
            out,
            "note",
            "oauth_client_id is not used while token_command is set",
        )?;
    }
    store_rows(&input.paths.account_dir(&account.name), out)?;
    writeln!(out)
}

/// When the stored access token expires, in plain words. mejl refreshes it
/// on its own, so an expired one is not a problem.
fn expiry(at: jiff::Timestamp, now: jiff::Timestamp) -> String {
    let secs = at.as_second() - now.as_second();
    let stamp = at.strftime("%Y-%m-%d %H:%M UTC");
    if secs <= 0 {
        return format!("{stamp} (expired; mejl refreshes it when it starts)");
    }
    let left = match secs {
        s if s < 120 => format!("{s} seconds"),
        s if s < 2 * 3600 => format!("{} minutes", s / 60),
        s if s < 2 * 86_400 => format!("{} hours", s / 3600),
        s => format!("{} days", s / 86_400),
    };
    format!("{stamp} (in {left}; mejl refreshes it before then)")
}

fn store_rows(dir: &Path, out: &mut dyn Write) -> io::Result<()> {
    row(out, "store", &dir.display().to_string())?;
    if !dir.is_dir() {
        return row(out, "store size", "nothing stored yet");
    }
    let sizes: Vec<String> = STORE_FILES
        .iter()
        .map(|f| format!("{f} {}", human_size(store_file_size(dir, f))))
        .collect();
    row(out, "store size", &sizes.join(", "))?;
    row(
        out,
        "running",
        if lock::is_held(dir) {
            "yes, another mejl has this account open"
        } else {
            "no"
        },
    )
}

/// A store file's size including its SQLite write-ahead log and shared
/// memory files.
pub fn store_file_size(dir: &Path, file: &str) -> u64 {
    ["", "-wal", "-shm"]
        .iter()
        .filter_map(|suffix| std::fs::metadata(dir.join(format!("{file}{suffix}"))).ok())
        .map(|m| m.len())
        .sum()
}

/// Sizes in binary units, one decimal: `0 B`, `512 B`, `1.5 KiB`, `12.0 MiB`.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Account folders on disk that the config no longer names.
fn orphan_dirs(input: &DoctorInput<'_>, out: &mut dyn Write) -> io::Result<()> {
    let Ok(loaded) = input.config else {
        return Ok(());
    };
    let known = loaded.config.account_names();
    let current = loaded.config.account(input.account).ok().map(|a| a.name);
    let Ok(entries) = std::fs::read_dir(input.paths.data_dir.join(ACCOUNTS_DIR)) else {
        return Ok(());
    };
    let mut orphans: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !known.contains(n) && current.as_deref() != Some(n.as_str()))
        .collect();
    orphans.sort();
    for name in orphans {
        writeln!(out, "Stored account {name:?} (not in the config)")?;
        store_rows(&input.paths.account_dir(&name), out)?;
        writeln!(out)?;
    }
    Ok(())
}

fn terminal_section(caps: &Capabilities, out: &mut dyn Write) -> io::Result<()> {
    writeln!(out, "Terminal")?;
    row(
        out,
        "size",
        &caps
            .size
            .map_or("unknown".to_owned(), |(c, r)| format!("{c} x {r}")),
    )?;
    row(out, "TERM", caps.term.as_deref().unwrap_or("(not set)"))?;
    row(
        out,
        "TERM_PROGRAM",
        caps.term_program.as_deref().unwrap_or("(not set)"),
    )?;
    row(
        out,
        "terminal",
        caps.terminal_version.as_deref().unwrap_or("unknown"),
    )?;
    row(out, "truecolor", yes_no(caps.truecolor))?;
    row(
        out,
        "colour",
        if caps.no_color {
            "off (NO_COLOR is set)"
        } else {
            "on"
        },
    )?;
    let bg = match caps.background {
        Background::Dark => "dark",
        Background::Light => "light",
        Background::Unknown => "unknown",
    };
    let bg = match caps.background_rgb {
        Some((r, g, b)) => format!("{bg} (#{r:02x}{g:02x}{b:02x})"),
        None => bg.to_owned(),
    };
    row(out, "background", &bg)?;
    row(out, "theme reports", caps.theme_reports.describe())?;
    row(out, "kitty keys", caps.kitty_keyboard.describe())?;
    row(out, "sync output", caps.synchronized_output.describe())?;
    row(
        out,
        "links (OSC 8)",
        if caps.osc8_links() {
            "on"
        } else {
            "off under GNU screen"
        },
    )?;
    row(out, "clipboard", caps.clipboard_note())?;
    let mux = match caps.multiplexer {
        Some(Multiplexer::Tmux) => "tmux",
        Some(Multiplexer::Zellij) => "Zellij",
        Some(Multiplexer::Screen) => "GNU screen",
        None => "none",
    };
    row(out, "multiplexer", mux)?;
    row(out, "over SSH", yes_no(caps.ssh))?;
    let probe = match &caps.probe {
        ProbeStatus::NotRun => "not run (not a terminal, or --no-probe)".to_owned(),
        ProbeStatus::Complete => "complete".to_owned(),
        ProbeStatus::TimedOut => "timed out; missing answers are unknown".to_owned(),
        ProbeStatus::Failed(e) => format!("failed: {e}"),
    };
    row(out, "probe", &probe)
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse;
    use crate::env::var;
    use crate::secrets::MemoryStore;
    use crate::terminal::probe::from_env;
    use mejl_jmap::Secret;

    fn run(
        paths: &Paths,
        env: &Env,
        config: Result<&Loaded, &ConfigError>,
        store: &MemoryStore,
    ) -> (String, DoctorOutcome) {
        let caps = from_env(env);
        let input = DoctorInput {
            paths,
            env,
            config,
            account: None,
            store,
            caps: &caps,
        };
        let mut out = Vec::new();
        let outcome = report(&input, &mut out).unwrap();
        (String::from_utf8(out).unwrap(), outcome)
    }

    #[test]
    fn report_never_prints_the_token() {
        let base = tempfile::tempdir().unwrap();
        let paths = Paths::under(base.path());
        let env = Env::from_pairs([(var::MEJL_TOKEN, "fmu1-doctor-secret-value-1234")]);
        let store = MemoryStore::new();
        store
            .set("default", &Secret::new("fmu1-keyring-secret-value-5678"))
            .unwrap();
        let loaded = parse("").unwrap();
        let (text, outcome) = run(&paths, &env, Ok(&loaded), &store);
        assert!(!text.contains("doctor-secret"), "{text}");
        assert!(!text.contains("keyring-secret"), "{text}");
        assert!(text.contains("from MEJL_TOKEN"));
        assert!(!outcome.config_errors);

        let (text, _) = run(&paths, &Env::default(), Ok(&loaded), &store);
        assert!(
            text.contains("API token, found in the memory store"),
            "{text}"
        );
        assert!(!text.contains("keyring-secret"), "{text}");
    }

    #[test]
    fn an_oauth_login_shows_scope_and_expiry_but_no_token() {
        let base = tempfile::tempdir().unwrap();
        let paths = Paths::under(base.path());
        let store = MemoryStore::new();
        store
            .set("default", &Secret::new("fmu1-keyring-secret-value-5678"))
            .unwrap();
        let record = crate::oauth::OAuthRecord {
            client_id: "doctor-client-id".into(),
            client_source: crate::oauth::ClientSource::Registered,
            issuer: "https://api.example.com".into(),
            token_endpoint: "https://api.example.com/oauth/refresh".into(),
            revocation_endpoint: None,
            tokens: mejl_jmap::TokenSet {
                access: Secret::new("doctor-access-secret-0001"),
                refresh: Secret::new("doctor-refresh-secret-0001"),
                expires_at: jiff::Timestamp::now() + jiff::SignedDuration::from_mins(30),
                scope: vec![
                    "urn:ietf:params:oauth:scope:mail".into(),
                    "offline_access".into(),
                ],
            },
        };
        store.set_oauth("default", &record).unwrap();
        let loaded = parse("").unwrap();
        let (text, _) = run(&paths, &Env::default(), Ok(&loaded), &store);
        assert!(text.contains("OAuth, found in the memory store"), "{text}");
        assert!(text.contains("registered by `mejl login`"), "{text}");
        assert!(
            text.contains("scope           urn:ietf:params:oauth:scope:mail offline_access"),
            "{text}"
        );
        assert!(
            text.contains("(in 29 minutes") || text.contains("(in 30 minutes"),
            "{text}"
        );
        for secret in ["doctor-access", "doctor-refresh", "keyring-secret"] {
            assert!(!text.contains(secret), "{secret}: {text}");
        }
        assert_eq!(
            expiry(jiff::Timestamp::UNIX_EPOCH, jiff::Timestamp::now()),
            "1970-01-01 00:00 UTC (expired; mejl refreshes it when it starts)"
        );
    }

    #[test]
    fn report_shows_paths_config_and_terminal() {
        let base = tempfile::tempdir().unwrap();
        let paths = Paths::under(base.path());
        let env = Env::from_pairs([("TERM", "xterm-256color"), ("NO_COLOR", "1")]);
        let store = MemoryStore::new();
        let loaded = parse("[ui]\nthme = 1\n").unwrap();
        let (text, _) = run(&paths, &env, Ok(&loaded), &store);
        assert!(text.contains(&paths.config_file.display().to_string()));
        assert!(text.contains("not found, defaults in use"));
        assert!(text.contains("warning: line 2, column 1"), "{text}");
        assert!(text.contains("not found; run `mejl login`"));
        assert!(text.contains("nothing stored yet"));
        assert!(text.contains("xterm-256color"));
        assert!(text.contains("off (NO_COLOR is set)"));
        assert!(text.contains("probe           not run"), "{text}");
    }

    #[test]
    fn config_errors_are_listed_and_flagged() {
        let base = tempfile::tempdir().unwrap();
        let paths = Paths::under(base.path());
        let err = parse("[ui]\nmouse = 3\n").unwrap_err();
        let (text, outcome) = run(&paths, &Env::default(), Err(&err), &MemoryStore::new());
        assert!(outcome.config_errors);
        assert!(text.contains("line 2"), "{text}");
        assert!(text.contains("has errors"));
    }

    #[test]
    fn store_sizes_and_lock_state() {
        let base = tempfile::tempdir().unwrap();
        let paths = Paths::under(base.path());
        let dir = paths.ensure_account_dir("default").unwrap();
        std::fs::write(dir.join("cache.db"), vec![0u8; 2048]).unwrap();
        std::fs::write(dir.join("cache.db-wal"), vec![0u8; 1024]).unwrap();
        std::fs::write(dir.join("state.db"), vec![0u8; 10]).unwrap();
        let _lock = lock::InstanceLock::acquire(&dir).unwrap();
        let loaded = parse("").unwrap();
        let (text, _) = run(&paths, &Env::default(), Ok(&loaded), &MemoryStore::new());
        assert!(text.contains("cache.db 3.0 KiB, state.db 10 B"), "{text}");
        assert!(text.contains("yes, another mejl"), "{text}");
    }

    #[test]
    fn other_and_orphan_accounts_are_listed() {
        let base = tempfile::tempdir().unwrap();
        let paths = Paths::under(base.path());
        paths.ensure_account_dir("old").unwrap();
        let loaded = parse("default_account = \"a\"\n[accounts.a]\n[accounts.b]\n").unwrap();
        let (text, _) = run(&paths, &Env::default(), Ok(&loaded), &MemoryStore::new());
        assert!(text.contains("Account \"a\""), "{text}");
        assert!(text.contains("Other account \"b\""), "{text}");
        assert!(
            text.contains("Stored account \"old\" (not in the config)"),
            "{text}"
        );
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(256 * 1024 * 1024), "256.0 MiB");
        assert!(human_size(u64::MAX).ends_with(" TiB"));
    }
}
