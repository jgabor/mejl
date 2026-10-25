//! The TOML config file.
//!
//! Every key is optional. A missing file means all defaults, with one
//! implicit account called `default` on Fastmail. Run
//! `mejl doctor --print-config` for a commented example ([`example_config`]).
//!
//! Sections and defaults:
//!
//! | Key | Default | Meaning |
//! |---|---|---|
//! | `default_account` | none | Account used without `--account`. Needed only with several accounts |
//! | `accounts.<name>.session_url` | `https://api.fastmail.com/jmap/session` | JMAP session resource |
//! | `accounts.<name>.username` | none | Shown in the UI only. Never sent anywhere |
//! | `accounts.<name>.token_command` | none | Command that prints the token, for systems without a keyring |
//! | `ui.theme` | `terminal` | Built-in theme name, or `auto:<name>` |
//! | `ui.preset` | `fastmail` | Key preset: `fastmail` or `vim` |
//! | `ui.density` | `comfortable` | List density: `compact`, `comfortable` or `relaxed` |
//! | `ui.prefer_html` | `false` | Show the HTML part when a message has both |
//! | `ui.undo_send_seconds` | `10` | Hold sent mail this long so it can be undone (0 to 300) |
//! | `ui.mark_read_delay_ms` | `0` | Wait this long before marking an opened message read (0 to 60000) |
//! | `ui.mark_read_on_archive` | `false` | Archiving a conversation also marks it read |
//! | `ui.remote_images` | `off` | `off` or `on` |
//! | `ui.mouse` | `true` | Capture the mouse. `false` keeps the terminal's own text selection |
//! | `keys` | empty | Key overrides, `[keys.<context>] "key" = "action"`, checked by the UI |
//! | `sync.push` | `true` | Use EventSource push |
//! | `sync.poll_interval_secs` | `300` | Poll this often when push is off or down (30 to 86400) |
//! | `sync.prefetch` | `3` | Bodies to prefetch around the cursor (0 to 50) |
//! | `sync.body_cache_mb` | `256` | Most body text kept on disk (16 to 65536) |
//!
//! Unknown keys are warnings, not errors, so a config written for a newer
//! mejl still loads. Type errors and out-of-range values are errors with a
//! line and column.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::{Range, RangeInclusive};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::Spanned;
use toml::de::{DeTable, DeValue};

use crate::paths::validate_account_name;

/// Fastmail's documented JMAP session endpoint.
pub const FASTMAIL_SESSION_URL: &str = "https://api.fastmail.com/jmap/session";
/// The account name used when the config names none.
pub const DEFAULT_ACCOUNT: &str = "default";

/// The whole config.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Account used when `--account` is not given.
    pub default_account: Option<String>,
    /// Accounts by name.
    pub accounts: BTreeMap<String, AccountConfig>,
    /// Display settings.
    pub ui: UiSettings,
    /// Raw key overrides, passed to the UI keymap as they are.
    pub keys: toml::Table,
    /// Sync settings.
    pub sync: SyncSettings,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            default_account: None,
            accounts: BTreeMap::new(),
            ui: UiSettings::default(),
            keys: toml::Table::new(),
            sync: SyncSettings::default(),
        }
    }
}

/// One account.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct AccountConfig {
    /// The account's name: its key under `[accounts]`.
    #[serde(skip)]
    pub name: String,
    /// The JMAP session URL.
    pub session_url: String,
    /// For display only. mejl never sends it and never writes it.
    pub username: Option<String>,
    /// A command whose first output line is the API token, run through the
    /// shell. For systems without a keyring, for example
    /// `pass show fastmail/mejl`.
    pub token_command: Option<String>,
}

impl Default for AccountConfig {
    fn default() -> Self {
        AccountConfig {
            name: String::new(),
            session_url: FASTMAIL_SESSION_URL.to_owned(),
            username: None,
            token_command: None,
        }
    }
}

impl AccountConfig {
    /// An account with default settings.
    pub fn named(name: &str) -> Self {
        AccountConfig {
            name: name.to_owned(),
            ..AccountConfig::default()
        }
    }
}

/// Key presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Preset {
    /// Fastmail web's keys.
    #[default]
    Fastmail,
    /// Vim-style keys.
    Vim,
}

/// Message list density.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Density {
    /// One line per conversation, no gaps.
    Compact,
    /// The mock-up layout.
    #[default]
    Comfortable,
    /// Extra room and sender badges.
    Relaxed,
}

/// Remote image policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RemoteImages {
    /// Never load remote images. Trackers stay blind.
    #[default]
    Off,
    /// Load them where the terminal can show images.
    On,
}

/// Built-in theme names. `auto:<name>` picks a light or dark variant.
pub const BUILTIN_THEMES: &[&str] = &["terminal", "mono", "high-contrast", "dark", "light"];

/// The `[ui]` section.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    /// Theme name.
    pub theme: String,
    /// Key preset.
    pub preset: Preset,
    /// List density.
    pub density: Density,
    /// Show the HTML part when a message has both.
    pub prefer_html: bool,
    /// Seconds a sent message is held so it can be undone.
    pub undo_send_seconds: u32,
    /// Milliseconds before an opened message is marked read.
    pub mark_read_delay_ms: u32,
    /// Archiving also marks the mail read.
    pub mark_read_on_archive: bool,
    /// Remote image policy.
    pub remote_images: RemoteImages,
    /// Capture the mouse.
    pub mouse: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        UiSettings {
            theme: "terminal".to_owned(),
            preset: Preset::Fastmail,
            density: Density::Comfortable,
            prefer_html: false,
            undo_send_seconds: 10,
            mark_read_delay_ms: 0,
            mark_read_on_archive: false,
            remote_images: RemoteImages::Off,
            mouse: true,
        }
    }
}

/// The `[sync]` section.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct SyncSettings {
    /// Use EventSource push.
    pub push: bool,
    /// Seconds between polls when push is off or down.
    pub poll_interval_secs: u32,
    /// Bodies to prefetch around the cursor.
    pub prefetch: u32,
    /// Most body text kept on disk, in MiB.
    pub body_cache_mb: u32,
}

impl Default for SyncSettings {
    fn default() -> Self {
        SyncSettings {
            push: true,
            poll_interval_secs: 300,
            prefetch: 3,
            body_cache_mb: 256,
        }
    }
}

impl SyncSettings {
    /// The body cache cap in bytes.
    pub fn body_cache_bytes(&self) -> u64 {
        u64::from(self.body_cache_mb) * 1024 * 1024
    }
}

/// A problem at a place in the file. Line and column start at 1; both are
/// 0 when the place is unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// 1-based line, or 0.
    pub line: usize,
    /// 1-based column in characters, or 0.
    pub column: usize,
    /// What is wrong, in plain words.
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            f.write_str(&self.message)
        } else {
            write!(
                f,
                "line {}, column {}: {}",
                self.line, self.column, self.message
            )
        }
    }
}

/// Why a config could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file exists but cannot be read.
    #[error("cannot read config file {path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// The cause.
        source: std::io::Error,
    },
    /// The file is not valid.
    #[error("{}", format_invalid(path.as_deref(), errors))]
    Invalid {
        /// The file, when the text came from one.
        path: Option<PathBuf>,
        /// Every problem found.
        errors: Vec<Diagnostic>,
    },
    /// The requested account cannot be used.
    #[error("{0}")]
    Account(String),
}

fn format_invalid(path: Option<&Path>, errors: &[Diagnostic]) -> String {
    let mut out = match path {
        Some(p) => format!("config file {} has errors:", p.display()),
        None => "config has errors:".to_owned(),
    };
    for e in errors {
        out.push_str("\n  ");
        out.push_str(&e.to_string());
    }
    out
}

/// A loaded config and the warnings found on the way.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    /// The config, with defaults filled in.
    pub config: Config,
    /// Unknown keys and other things that do not stop mejl.
    pub warnings: Vec<Diagnostic>,
    /// Whether a file was read. `false` means defaults only.
    pub from_file: bool,
}

/// Reads the config at `path`. A missing file gives the defaults.
pub fn load(path: &Path) -> Result<Loaded, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text)
            .map(|mut loaded| {
                loaded.from_file = true;
                loaded
            })
            .map_err(|e| match e {
                ConfigError::Invalid { errors, .. } => ConfigError::Invalid {
                    path: Some(path.to_owned()),
                    errors,
                },
                other => other,
            }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Loaded {
            config: Config::default(),
            warnings: Vec::new(),
            from_file: false,
        }),
        Err(source) => Err(ConfigError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

/// Parses config text.
pub fn parse(text: &str) -> Result<Loaded, ConfigError> {
    let doc = DeTable::parse(text).map_err(|e| invalid(vec![from_toml_error(text, &e)]))?;
    let mut config: Config =
        toml::from_str(text).map_err(|e| invalid(vec![from_toml_error(text, &e)]))?;
    for (name, account) in &mut config.accounts {
        account.name = name.clone();
    }
    let mut warnings = Vec::new();
    check_unknown_keys(text, doc.get_ref(), &mut warnings);
    let mut errors = Vec::new();
    validate(text, doc.get_ref(), &config, &mut errors, &mut warnings);
    // File order reads best, whatever order the checks ran in.
    warnings.sort_by_key(|d| (d.line, d.column));
    errors.sort_by_key(|d| (d.line, d.column));
    if errors.is_empty() {
        Ok(Loaded {
            config,
            warnings,
            from_file: false,
        })
    } else {
        Err(invalid(errors))
    }
}

fn invalid(errors: Vec<Diagnostic>) -> ConfigError {
    ConfigError::Invalid { path: None, errors }
}

fn from_toml_error(text: &str, e: &toml::de::Error) -> Diagnostic {
    let (line, column) = e.span().map(|s| line_col(text, s.start)).unwrap_or((0, 0));
    Diagnostic {
        line,
        column,
        message: e.message().trim().to_owned(),
    }
}

/// 1-based line and character column of a byte offset.
pub fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(text.len());
    let mut start = offset;
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let before = &text[..start];
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let column = before[line_start..].chars().count() + 1;
    (line, column)
}

fn at(text: &str, span: Range<usize>, message: String) -> Diagnostic {
    let (line, column) = line_col(text, span.start);
    Diagnostic {
        line,
        column,
        message,
    }
}

const TOP_KEYS: &[&str] = &["default_account", "accounts", "ui", "keys", "sync"];
const ACCOUNT_KEYS: &[&str] = &["session_url", "username", "token_command"];
const UI_KEYS: &[&str] = &[
    "theme",
    "preset",
    "density",
    "prefer_html",
    "undo_send_seconds",
    "mark_read_delay_ms",
    "mark_read_on_archive",
    "remote_images",
    "mouse",
];
const SYNC_KEYS: &[&str] = &["push", "poll_interval_secs", "prefetch", "body_cache_mb"];

fn check_unknown_keys(text: &str, doc: &DeTable<'_>, warnings: &mut Vec<Diagnostic>) {
    unknown_in(text, doc, "", TOP_KEYS, warnings);
    if let Some(DeValue::Table(accounts)) = doc.get("accounts").map(Spanned::get_ref) {
        for (name, value) in accounts.iter() {
            if let DeValue::Table(account) = value.get_ref() {
                let prefix = format!("accounts.{}.", name.get_ref());
                unknown_in(text, account, &prefix, ACCOUNT_KEYS, warnings);
            }
        }
    }
    for (section, known) in [("ui", UI_KEYS), ("sync", SYNC_KEYS)] {
        if let Some(DeValue::Table(table)) = doc.get(section).map(Spanned::get_ref) {
            unknown_in(text, table, &format!("{section}."), known, warnings);
        }
    }
}

fn unknown_in(
    text: &str,
    table: &DeTable<'_>,
    prefix: &str,
    known: &[&str],
    warnings: &mut Vec<Diagnostic>,
) {
    for (key, _) in table.iter() {
        let name: &str = key.get_ref();
        if known.contains(&name) {
            continue;
        }
        let mut message = format!("unknown key `{prefix}{name}` is ignored");
        if let Some(guess) = closest(name, known) {
            message.push_str(&format!(" (did you mean `{prefix}{guess}`?)"));
        }
        warnings.push(at(text, key.span(), message));
    }
}

/// The known key closest to `name`, if it is a likely typo.
fn closest<'a>(name: &str, known: &[&'a str]) -> Option<&'a str> {
    known
        .iter()
        .map(|k| (edit_distance(name, k), *k))
        .filter(|(d, k)| *d <= 2.max(k.len() / 4))
        .min_by_key(|(d, _)| *d)
        .map(|(_, k)| k)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The span of a value by path, or of the closest table that exists.
fn span_of(doc: &DeTable<'_>, path: &[&str]) -> Range<usize> {
    let mut table = doc;
    let mut span = 0..0;
    for (i, part) in path.iter().enumerate() {
        let Some(value) = table.get(*part) else {
            break;
        };
        span = value.span();
        match value.get_ref() {
            DeValue::Table(t) if i + 1 < path.len() => table = t,
            _ => break,
        }
    }
    span
}

fn validate(
    text: &str,
    doc: &DeTable<'_>,
    config: &Config,
    errors: &mut Vec<Diagnostic>,
    warnings: &mut Vec<Diagnostic>,
) {
    let mut range = |section: &str, key: &str, value: u32, allowed: RangeInclusive<u32>| {
        if !allowed.contains(&value) {
            errors.push(at(
                text,
                span_of(doc, &[section, key]),
                format!(
                    "`{section}.{key}` is {value}, but must be between {} and {}",
                    allowed.start(),
                    allowed.end()
                ),
            ));
        }
    };
    range(
        "ui",
        "undo_send_seconds",
        config.ui.undo_send_seconds,
        0..=300,
    );
    range(
        "ui",
        "mark_read_delay_ms",
        config.ui.mark_read_delay_ms,
        0..=60_000,
    );
    range(
        "sync",
        "poll_interval_secs",
        config.sync.poll_interval_secs,
        30..=86_400,
    );
    range("sync", "prefetch", config.sync.prefetch, 0..=50);
    range(
        "sync",
        "body_cache_mb",
        config.sync.body_cache_mb,
        16..=65_536,
    );

    if !theme_is_known(&config.ui.theme) {
        warnings.push(at(
            text,
            span_of(doc, &["ui", "theme"]),
            format!(
                "theme {:?} is not built in; the `terminal` theme is used. Built-in themes: {}",
                config.ui.theme,
                BUILTIN_THEMES.join(", ")
            ),
        ));
    }

    for (name, account) in &config.accounts {
        let path = ["accounts", name.as_str()];
        if let Err(e) = validate_account_name(name) {
            errors.push(at(text, span_of(doc, &path), e));
        }
        if let Err(e) = check_session_url(&account.session_url) {
            errors.push(at(
                text,
                span_of(doc, &["accounts", name.as_str(), "session_url"]),
                format!("`accounts.{name}.session_url`: {e}"),
            ));
        }
        if account
            .token_command
            .as_deref()
            .is_some_and(|c| c.trim().is_empty())
        {
            errors.push(at(
                text,
                span_of(doc, &["accounts", name.as_str(), "token_command"]),
                format!("`accounts.{name}.token_command` is empty; remove it or name a command"),
            ));
        }
    }

    if let Some(default) = &config.default_account
        && !config.accounts.contains_key(default)
    {
        errors.push(at(
            text,
            span_of(doc, &["default_account"]),
            format!(
                "`default_account` is {default:?}, but there is no [accounts.{default}] section"
            ),
        ));
    }
}

fn theme_is_known(theme: &str) -> bool {
    let name = theme.strip_prefix("auto:").unwrap_or(theme);
    BUILTIN_THEMES.contains(&name)
}

/// Checks a session URL. It must be `https`, except on loopback hosts where
/// plain `http` is allowed for the demo server and tests. A token is never
/// sent in the clear over a network.
pub fn check_session_url(url: &str) -> Result<(), String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("{url:?} is not an absolute URL"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    if authority.contains('@') {
        return Err(format!(
            "{url:?} has user info; put the token in the keyring instead"
        ));
    }
    let host = if let Some(stripped) = host_port.strip_prefix('[') {
        stripped.split(']').next().unwrap_or("")
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    if host.is_empty() {
        return Err(format!("{url:?} has no host"));
    }
    match scheme.to_ascii_lowercase().as_str() {
        "https" => Ok(()),
        "http" if is_loopback_host(host) => Ok(()),
        "http" => Err(format!(
            "{url:?} uses plain http; only https is allowed for a remote server"
        )),
        other => Err(format!("{url:?} uses {other:?}; use https")),
    }
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

impl Config {
    /// Picks the account to use.
    ///
    /// - With `requested`, that account. A name that is not configured is
    ///   allowed only while the config has no accounts at all, so a typo
    ///   never silently lands on a fresh, empty account.
    /// - Without, `default_account`, else the only account, else the
    ///   implicit [`DEFAULT_ACCOUNT`] when none are configured.
    pub fn account(&self, requested: Option<&str>) -> Result<AccountConfig, ConfigError> {
        let name = match requested {
            Some(name) => name.to_owned(),
            None => match (&self.default_account, self.accounts.len()) {
                (Some(name), _) => name.clone(),
                (None, 0) => DEFAULT_ACCOUNT.to_owned(),
                (None, 1) => self.accounts.keys().next().cloned().unwrap_or_default(),
                (None, _) => {
                    return Err(ConfigError::Account(format!(
                        "the config has several accounts ({}); pick one with --account or set default_account",
                        self.account_names().join(", ")
                    )));
                }
            },
        };
        validate_account_name(&name).map_err(ConfigError::Account)?;
        match self.accounts.get(&name) {
            Some(account) => Ok(account.clone()),
            None if self.accounts.is_empty() => Ok(AccountConfig::named(&name)),
            None => Err(ConfigError::Account(format!(
                "there is no account {name:?} in the config; known accounts: {}",
                self.account_names().join(", ")
            ))),
        }
    }

    /// Configured account names in order.
    pub fn account_names(&self) -> Vec<String> {
        self.accounts.keys().cloned().collect()
    }
}

/// A commented example config. Every setting is shown with its default
/// value, commented out, so the file changes nothing until edited.
pub fn example_config() -> String {
    let ui = UiSettings::default();
    let sync = SyncSettings::default();
    format!(
        r#"# mejl config file. Every key is optional; the values shown are the defaults.
# Save this as the config path that `mejl doctor` prints.

# The account used when --account is not given. Needed only with several
# accounts.
# default_account = "personal"

# One section per account. Without any, mejl uses one account called
# "default" on Fastmail. The API token is never stored here: `mejl login`
# puts it in the OS keyring.
# [accounts.personal]
# session_url = "{FASTMAIL_SESSION_URL}"
# Shown in the UI only.
# username = "you@example.com"
# For systems without a keyring: a command whose first output line is the token.
# token_command = "pass show fastmail/mejl"

[ui]
# Built-in themes: {themes}. "auto:<name>" picks a light or dark variant.
# theme = "{theme}"
# Key preset: "fastmail" or "vim".
# preset = "fastmail"
# List density: "compact", "comfortable" or "relaxed".
# density = "comfortable"
# Show the HTML part when a message has both.
# prefer_html = {prefer_html}
# Hold sent mail this many seconds so it can be undone (0 to 300).
# undo_send_seconds = {undo}
# Wait this long before marking an opened message read, in ms (0 to 60000).
# mark_read_delay_ms = {mark}
# Archiving a conversation also marks it read.
# mark_read_on_archive = {mark_archive}
# Remote images: "off" or "on".
# remote_images = "off"
# Capture the mouse. false keeps the terminal's own text selection.
# mouse = {mouse}

# Key overrides per context: global, list, reader, sidebar or help.
# Check them with `mejl keys --check`.
# [keys.list]
# "ctrl-n" = "next"
# "j" = "none"

[sync]
# Use EventSource push for new mail.
# push = {push}
# Poll this often when push is off or down, in seconds (30 to 86400).
# poll_interval_secs = {poll}
# Bodies to prefetch around the cursor (0 to 50).
# prefetch = {prefetch}
# Most body text kept on disk, in MiB (16 to 65536).
# body_cache_mb = {cache}
"#,
        themes = BUILTIN_THEMES.join(", "),
        theme = ui.theme,
        prefer_html = ui.prefer_html,
        undo = ui.undo_send_seconds,
        mark = ui.mark_read_delay_ms,
        mark_archive = ui.mark_read_on_archive,
        mouse = ui.mouse,
        push = sync.push,
        poll = sync.poll_interval_secs,
        prefetch = sync.prefetch,
        cache = sync.body_cache_mb,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn errors(text: &str) -> Vec<Diagnostic> {
        match parse(text) {
            Err(ConfigError::Invalid { errors, .. }) => errors,
            other => panic!("expected errors, got {other:?}"),
        }
    }

    #[test]
    fn empty_text_gives_defaults() {
        let loaded = parse("").unwrap();
        assert_eq!(loaded.config, Config::default());
        assert!(loaded.warnings.is_empty());
        assert_eq!(loaded.config.ui.undo_send_seconds, 10);
        assert!(loaded.config.sync.push);
        assert_eq!(loaded.config.sync.body_cache_bytes(), 256 * 1024 * 1024);
    }

    #[test]
    fn full_config_parses() {
        let text = r#"
default_account = "work"

[accounts.work]
session_url = "https://jmap.example.com/session"
username = "ada@example.com"

[accounts.home]
token_command = "pass show mejl"

[ui]
theme = "auto:dark"
preset = "vim"
density = "compact"
prefer_html = true
undo_send_seconds = 20
mark_read_delay_ms = 1500
mark_read_on_archive = true
remote_images = "on"
mouse = false

[keys.list]
"ctrl-n" = "next"

[sync]
push = false
poll_interval_secs = 60
prefetch = 5
body_cache_mb = 512
"#;
        let loaded = parse(text).unwrap();
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        let c = loaded.config;
        assert_eq!(c.default_account.as_deref(), Some("work"));
        let work = &c.accounts["work"];
        assert_eq!(work.name, "work");
        assert_eq!(work.session_url, "https://jmap.example.com/session");
        assert_eq!(work.username.as_deref(), Some("ada@example.com"));
        let home = &c.accounts["home"];
        assert_eq!(home.session_url, FASTMAIL_SESSION_URL);
        assert_eq!(home.token_command.as_deref(), Some("pass show mejl"));
        assert_eq!(c.ui.preset, Preset::Vim);
        assert_eq!(c.ui.density, Density::Compact);
        assert_eq!(c.ui.remote_images, RemoteImages::On);
        assert!(!c.ui.mouse);
        assert_eq!(c.ui.mark_read_delay_ms, 1500);
        assert!(c.ui.mark_read_on_archive);
        let list = c.keys["list"].as_table().unwrap();
        assert_eq!(list["ctrl-n"].as_str(), Some("next"));
        assert_eq!(c.sync.prefetch, 5);
        assert!(!c.sync.push);
    }

    #[test]
    fn unknown_keys_warn_with_position_and_hint() {
        let text = "[ui]\nthme = \"dark\"\n\n[sync]\nfoo = 1\n[accounts.a]\nsesion_url = \"https://x.example.com\"\n";
        let loaded = parse(text).unwrap();
        let w = &loaded.warnings;
        assert_eq!(w.len(), 3, "{w:?}");
        assert_eq!((w[0].line, w[0].column), (2, 1));
        assert!(w[0].message.contains("`ui.thme`"), "{}", w[0].message);
        assert!(w[0].message.contains("did you mean `ui.theme`"));
        let all: String = w
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("`sync.foo`"), "{all}");
        assert!(
            all.contains("did you mean `accounts.a.session_url`"),
            "{all}"
        );
        assert!(!all.contains("did you mean `sync"), "{all}");
    }

    #[test]
    fn unknown_top_level_key_warns() {
        let loaded = parse("colour = true\n").unwrap();
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].message.contains("`colour`"));
    }

    #[test]
    fn syntax_error_has_line_and_column() {
        let e = errors("[ui]\ntheme = \"dark\nmouse = true\n");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].line, 2, "{e:?}");
        assert!(e[0].column >= 1);
    }

    #[test]
    fn type_error_has_line_and_column() {
        let e = errors("[ui]\n\nmouse = \"yes\"\n");
        assert_eq!(e[0].line, 3, "{e:?}");
        assert_eq!(e[0].column, 9, "{e:?}");
        let e = errors("[sync]\nprefetch = -1\n");
        assert_eq!(e[0].line, 2, "{e:?}");
    }

    #[test]
    fn bad_enum_value_is_an_error() {
        let e = errors("[ui]\npreset = \"emacs\"\n");
        assert_eq!(e[0].line, 2);
        assert!(
            e[0].message.contains("emacs") || e[0].message.contains("variant"),
            "{e:?}"
        );
    }

    #[test]
    fn out_of_range_values_are_errors_with_position() {
        let e = errors("[ui]\nundo_send_seconds = 301\n[sync]\nprefetch = 51\nbody_cache_mb = 1\n");
        assert_eq!(e.len(), 3, "{e:?}");
        assert_eq!((e[0].line, e[0].column), (2, 21));
        assert!(e[0].message.contains("between 0 and 300"));
        assert_eq!(e[1].line, 4);
        assert_eq!(e[2].line, 5);
    }

    #[test]
    fn poll_interval_lower_bound() {
        let e = errors("[sync]\npoll_interval_secs = 5\n");
        assert!(e[0].message.contains("between 30"), "{e:?}");
    }

    #[test]
    fn unknown_theme_is_a_warning() {
        let loaded = parse("[ui]\ntheme = \"neon\"\n").unwrap();
        assert_eq!(loaded.warnings.len(), 1);
        assert_eq!(loaded.warnings[0].line, 2);
        assert!(
            parse("[ui]\ntheme = \"auto:light\"\n")
                .unwrap()
                .warnings
                .is_empty()
        );
        assert!(
            !parse("[ui]\ntheme = \"auto:neon\"\n")
                .unwrap()
                .warnings
                .is_empty()
        );
    }

    #[test]
    fn plain_http_is_only_for_loopback() {
        assert!(check_session_url("https://api.fastmail.com/jmap/session").is_ok());
        assert!(check_session_url("http://127.0.0.1:8080/jmap/session").is_ok());
        assert!(check_session_url("http://localhost/jmap/session").is_ok());
        assert!(check_session_url("http://[::1]:9/jmap").is_ok());
        assert!(check_session_url("HTTP://127.0.0.1/x").is_ok());
        assert!(check_session_url("http://jmap.example.com/session").is_err());
        assert!(check_session_url("http://127.0.0.1.example.com/session").is_err());
        assert!(check_session_url("ftp://jmap.example.com").is_err());
        assert!(check_session_url("jmap.example.com/session").is_err());
        assert!(check_session_url("https:///session").is_err());
        assert!(check_session_url("https://user:pw@jmap.example.com/").is_err());
        let e = errors("[accounts.a]\nsession_url = \"http://jmap.example.com/s\"\n");
        assert_eq!(e[0].line, 2);
        assert!(e[0].message.contains("plain http"));
    }

    #[test]
    fn bad_account_entries_are_errors() {
        let e = errors("[accounts.\"bad name\"]\n");
        assert!(e[0].message.contains("bad name"), "{e:?}");
        let e = errors("[accounts.a]\ntoken_command = \"  \"\n");
        assert!(e[0].message.contains("token_command"), "{e:?}");
        let e = errors("default_account = \"nope\"\n[accounts.a]\n");
        assert_eq!(e[0].line, 1);
        assert!(e[0].message.contains("nope"));
    }

    #[test]
    fn account_selection() {
        let none = Config::default();
        assert_eq!(none.account(None).unwrap().name, DEFAULT_ACCOUNT);
        assert_eq!(
            none.account(None).unwrap().session_url,
            FASTMAIL_SESSION_URL
        );
        assert_eq!(none.account(Some("work")).unwrap().name, "work");
        assert!(none.account(Some("../x")).is_err());

        let one = parse("[accounts.home]\n").unwrap().config;
        assert_eq!(one.account(None).unwrap().name, "home");
        let err = one.account(Some("hmoe")).unwrap_err().to_string();
        assert!(err.contains("known accounts: home"), "{err}");

        let two = parse("[accounts.a]\n[accounts.b]\n").unwrap().config;
        let err = two.account(None).unwrap_err().to_string();
        assert!(err.contains("--account"), "{err}");
        assert_eq!(two.account(Some("b")).unwrap().name, "b");

        let with_default = parse("default_account = \"b\"\n[accounts.a]\n[accounts.b]\n")
            .unwrap()
            .config;
        assert_eq!(with_default.account(None).unwrap().name, "b");
    }

    #[test]
    fn example_config_is_valid_and_matches_defaults() {
        let example = example_config();
        let loaded = parse(&example).unwrap();
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert_eq!(loaded.config, Config::default());

        // Uncommenting every setting line must still give the defaults for
        // ui and sync, so the documented defaults cannot drift.
        let mut uncommented = String::new();
        let mut section = String::new();
        for line in example.lines() {
            let trimmed = line.trim_start_matches("# ");
            if trimmed.starts_with('[') {
                section = trimmed.to_owned();
            }
            let is_setting = trimmed.contains(" = ") && !trimmed.contains("  ");
            let keep = (section == "[ui]" || section == "[sync]")
                && (is_setting || trimmed.starts_with('['));
            if keep {
                uncommented.push_str(trimmed);
                uncommented.push('\n');
            }
        }
        let loaded = parse(&uncommented).unwrap();
        assert!(
            loaded.warnings.is_empty(),
            "{:?}\n{uncommented}",
            loaded.warnings
        );
        assert_eq!(loaded.config.ui, UiSettings::default(), "{uncommented}");
        assert_eq!(loaded.config.sync, SyncSettings::default());
        assert!(
            uncommented.contains("undo_send_seconds = 10"),
            "{uncommented}"
        );
    }

    #[test]
    fn example_never_contains_a_real_looking_address() {
        let example = example_config();
        for word in example.split_whitespace() {
            if word.contains('@') {
                assert!(word.contains("example.com"), "{word}");
            }
        }
    }

    #[test]
    fn load_missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load(&dir.path().join("none.toml")).unwrap();
        assert!(!loaded.from_file);
        assert_eq!(loaded.config, Config::default());
    }

    #[test]
    fn load_reports_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[ui]\nmouse = 3\n").unwrap();
        let err = load(&path).unwrap_err();
        let text = err.to_string();
        assert!(text.contains(&path.display().to_string()), "{text}");
        assert!(text.contains("line 2"), "{text}");
        std::fs::write(&path, "[ui]\nmouse = false\n").unwrap();
        let loaded = load(&path).unwrap();
        assert!(loaded.from_file);
        assert!(!loaded.config.ui.mouse);
    }

    #[test]
    fn load_directory_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(load(dir.path()), Err(ConfigError::Io { .. })));
    }

    #[test]
    fn line_col_counts_characters() {
        let text = "ab\nçd = x\n";
        assert_eq!(line_col(text, 0), (1, 1));
        assert_eq!(line_col(text, 3), (2, 1));
        // 'ç' is two bytes; the byte after it is column 2.
        assert_eq!(line_col(text, 5), (2, 2));
        assert_eq!(line_col(text, 4), (2, 1));
        assert_eq!(line_col(text, 999), (3, 1));
    }

    #[test]
    fn edit_distance_works() {
        assert_eq!(edit_distance("thme", "theme"), 1);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(closest("zzzzzz", UI_KEYS), None);
    }
}
