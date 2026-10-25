//! Logging to a rolling file, never to the terminal.
//!
//! While the TUI runs, anything written to stdout or stderr would corrupt
//! the screen, so every log line goes to `<state>/log/mejl.log.<date>`
//! instead. Files roll daily and the last [`KEEP_FILES`] are kept.
//!
//! The level comes from `--log-level`, then `MEJL_LOG` (full
//! `tracing_subscriber::EnvFilter` syntax, for example
//! `mejl=debug,mejl_jmap=trace`), then [`DEFAULT_LEVEL`].
//!
//! Tokens never reach the file. Every line passes through [`redact`]
//! before it is written: it replaces every token mejl has read (see
//! [`register_secret`]), anything after `Bearer `, and anything shaped like
//! a Fastmail token. HTTP crates that can log headers are also capped at
//! `info` by a second filter ([`under_cap`]), which can only remove lines:
//! `trace` does not make them chatty and `error` keeps them quiet.

use std::io::{self, Write};
use std::path::Path;
use std::sync::{OnceLock, PoisonError, RwLock};

use mejl_jmap::Secret;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::FilterExt;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;

use crate::paths::create_private_dir;

/// The log level when neither the flag nor `MEJL_LOG` sets one.
pub const DEFAULT_LEVEL: &str = "info";
/// Log file name prefix.
pub const FILE_PREFIX: &str = "mejl.log";
/// Daily log files kept.
pub const KEEP_FILES: usize = 7;
/// What replaces a secret in the log.
pub const REDACTED: &str = "[redacted]";

/// Targets capped at `info`, because at debug and trace they can print
/// request headers or TLS details.
const CAPPED_TARGETS: &[&str] = &[
    "hyper",
    "hyper_util",
    "h2",
    "reqwest",
    "rustls",
    "zbus",
    "keyring",
];

/// Secrets shorter than this are not registered: replacing every short
/// substring would make logs unreadable, and real tokens are long.
const MIN_REGISTERED_LEN: usize = 8;

fn registry() -> &'static RwLock<Vec<String>> {
    static SECRETS: OnceLock<RwLock<Vec<String>>> = OnceLock::new();
    SECRETS.get_or_init(|| RwLock::new(Vec::new()))
}

/// Remembers a token so [`redact`] removes it from every later log line.
pub fn register_secret(secret: &Secret) {
    let value = secret.expose();
    if value.len() < MIN_REGISTERED_LEN {
        return;
    }
    let mut list = registry().write().unwrap_or_else(PoisonError::into_inner);
    if !list.iter().any(|s| s == value) {
        list.push(value.to_owned());
        // Longest first, so a token that contains another is removed whole.
        list.sort_by_key(|s| std::cmp::Reverse(s.len()));
    }
}

/// Removes secrets from one log line.
pub fn redact(line: &str) -> String {
    let list = registry().read().unwrap_or_else(PoisonError::into_inner);
    redact_with(line, &list)
}

/// [`redact`] with an explicit secret list, for tests.
pub fn redact_with(line: &str, secrets: &[String]) -> String {
    let mut out = line.to_owned();
    for secret in secrets {
        if out.contains(secret.as_str()) {
            out = out.replace(secret.as_str(), REDACTED);
        }
    }
    out = redact_after(&out, "bearer ");
    redact_token_shapes(&out)
}

/// Replaces the word after each case-insensitive `marker`.
fn redact_after(line: &str, marker: &str) -> String {
    let lower = line.to_ascii_lowercase();
    let mut out = String::with_capacity(line.len());
    let mut pos = 0;
    while let Some(found) = lower[pos..].find(marker) {
        let start = pos + found + marker.len();
        out.push_str(&line[pos..start]);
        let rest = &line[start..];
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ')' | ']' | '}'))
            .unwrap_or(rest.len());
        if end > 0 && !rest.starts_with(REDACTED) {
            out.push_str(REDACTED);
        } else {
            out.push_str(&rest[..end]);
        }
        pos = start + end;
    }
    out.push_str(&line[pos..]);
    out
}

/// Replaces words shaped like Fastmail tokens: `fm`, a letter, a digit and
/// a dash, as in `fmu1-...`, followed by token characters.
fn redact_token_shapes(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    let mut copied = 0;
    while i + 5 <= bytes.len() {
        let starts_word = i == 0 || !is_token_byte(bytes[i - 1]);
        if starts_word
            && bytes[i] == b'f'
            && bytes[i + 1] == b'm'
            && bytes[i + 2].is_ascii_lowercase()
            && bytes[i + 3].is_ascii_digit()
            && bytes[i + 4] == b'-'
        {
            let mut end = i + 5;
            while end < bytes.len() && is_token_byte(bytes[end]) {
                end += 1;
            }
            if end - i >= 12 {
                out.push_str(&line[copied..i]);
                out.push_str(REDACTED);
                copied = end;
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&line[copied..]);
    out
}

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'+' | b'/' | b'=')
}

/// A writer that redacts each buffer before passing it on. tracing's fmt
/// layer writes one whole event per call, so a secret is never split
/// across two writes.
pub struct RedactingWriter<W> {
    inner: W,
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        self.inner.write_all(redact(&text).as_bytes())?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Wraps a [`MakeWriter`] so every writer it makes redacts.
#[derive(Clone)]
pub struct RedactingMakeWriter<M> {
    inner: M,
}

impl<M> RedactingMakeWriter<M> {
    /// Wraps `inner`.
    pub fn new(inner: M) -> Self {
        RedactingMakeWriter { inner }
    }
}

impl<'a, M: MakeWriter<'a>> MakeWriter<'a> for RedactingMakeWriter<M> {
    type Writer = RedactingWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter {
            inner: self.inner.make_writer(),
        }
    }
}

/// Errors setting up logging.
#[derive(Debug, thiserror::Error)]
pub enum LogError {
    /// The filter text is not valid.
    #[error("invalid log filter {filter:?}: {reason}")]
    Filter {
        /// What was given.
        filter: String,
        /// Why it failed.
        reason: String,
    },
    /// The log directory cannot be created.
    #[error("cannot create log directory {path}: {source}")]
    Dir {
        /// The directory.
        path: std::path::PathBuf,
        /// The cause.
        source: io::Error,
    },
    /// The rolling appender cannot start.
    #[error("cannot open log file: {0}")]
    Appender(String),
}

/// Builds the filter from the CLI level and `MEJL_LOG`. The flag wins.
pub fn build_filter(
    cli_level: Option<&str>,
    env_filter: Option<&str>,
) -> Result<EnvFilter, LogError> {
    let spec = cli_level
        .or(env_filter)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_LEVEL);
    EnvFilter::builder()
        .parse(spec)
        .map_err(|e| LogError::Filter {
            filter: spec.to_owned(),
            reason: e.to_string(),
        })
}

/// Keeps the background log writer alive. Logs written just before exit are
/// flushed when this drops, so hold it in `main` until the end.
pub struct LogGuard {
    _guard: tracing_appender::non_blocking::WorkerGuard,
}

/// Whether an event or span passes the cap on [`CAPPED_TARGETS`]. This is a
/// second filter on top of the user's: it can only remove lines, so a
/// quiet level such as `error` stays quiet for these crates too.
pub fn under_cap(target: &str, level: &tracing::Level) -> bool {
    let capped = CAPPED_TARGETS.iter().any(|t| {
        target
            .strip_prefix(t)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
    });
    !capped || *level <= tracing::Level::INFO
}

/// Builds the file logging subscriber without installing it.
pub fn file_subscriber(
    log_dir: &Path,
    filter: EnvFilter,
) -> Result<(impl tracing::Subscriber + Send + Sync + 'static, LogGuard), LogError> {
    create_private_dir(log_dir).map_err(|source| LogError::Dir {
        path: log_dir.to_owned(),
        source,
    })?;
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(FILE_PREFIX)
        .max_log_files(KEEP_FILES)
        .build(log_dir)
        .map_err(|e| LogError::Appender(e.to_string()))?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let cap = tracing_subscriber::filter::filter_fn(|meta| under_cap(meta.target(), meta.level()));
    let layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_thread_names(true)
        .with_writer(RedactingMakeWriter::new(writer))
        .with_filter(filter.and(cap));
    let subscriber = tracing_subscriber::registry().with(layer);
    Ok((subscriber, LogGuard { _guard: guard }))
}

/// Installs file logging for the whole process. Call once, early in `main`.
pub fn init(log_dir: &Path, filter: EnvFilter) -> Result<LogGuard, LogError> {
    let (subscriber, guard) = file_subscriber(log_dir, filter)?;
    // A second install fails; the first one stays, which is what we want.
    let _ = tracing::subscriber::set_global_default(subscriber);
    Ok(guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_secrets_are_replaced() {
        let secrets = vec!["abcdefgh12345678".to_owned()];
        let line = "token=abcdefgh12345678 and again abcdefgh12345678";
        assert_eq!(
            redact_with(line, &secrets),
            "token=[redacted] and again [redacted]"
        );
    }

    #[test]
    fn bearer_values_are_replaced() {
        assert_eq!(
            redact_with("authorization: Bearer xyz.123-abc, next", &[]),
            "authorization: Bearer [redacted], next"
        );
        assert_eq!(
            redact_with("(\"authorization\", \"bearer q\")", &[]),
            "(\"authorization\", \"bearer [redacted]\")"
        );
        // Already redacted stays as it is.
        assert_eq!(redact_with("Bearer [redacted]", &[]), "Bearer [redacted]");
        // Nothing after the marker.
        assert_eq!(redact_with("ends with bearer ", &[]), "ends with bearer ");
    }

    #[test]
    fn fastmail_token_shapes_are_replaced() {
        let line = "got fmu1-0a1b2c3d-4e5f60718293a4b5c6d7e8f9 from env";
        assert_eq!(redact_with(line, &[]), "got [redacted] from env");
        // Short or mid-word matches stay.
        assert_eq!(redact_with("fmu1-ab", &[]), "fmu1-ab");
        assert_eq!(redact_with("xfmu1-abcdefghijkl", &[]), "xfmu1-abcdefghijkl");
        assert_eq!(redact_with("café fm", &[]), "café fm");
    }

    #[test]
    fn global_registry_redacts() {
        let token = Secret::new("zz-registry-test-token-0042");
        register_secret(&token);
        register_secret(&token);
        register_secret(&Secret::new("short"));
        let out = redact("x zz-registry-test-token-0042 y short");
        assert_eq!(out, "x [redacted] y short");
    }

    #[test]
    fn writer_redacts_whole_lines() {
        let mut sink = Vec::new();
        {
            let mut w = RedactingWriter { inner: &mut sink };
            w.write_all(b"auth Bearer topsecret\n").unwrap();
            w.flush().unwrap();
        }
        assert_eq!(String::from_utf8(sink).unwrap(), "auth Bearer [redacted]\n");
    }

    #[test]
    fn filter_precedence_and_errors() {
        assert!(build_filter(None, None).is_ok());
        assert!(build_filter(Some("debug"), Some("nonsense=====")).is_ok());
        assert!(build_filter(None, Some("mejl=trace,mejl_jmap=debug")).is_ok());
        let err = build_filter(Some("mejl=notalevel"), None).unwrap_err();
        assert!(err.to_string().contains("notalevel"), "{err}");
    }

    #[test]
    fn cap_only_removes_lines() {
        use tracing::Level;
        assert!(under_cap("hyper", &Level::INFO));
        assert!(under_cap("hyper::proto::h1", &Level::WARN));
        assert!(!under_cap("hyper::proto::h1", &Level::DEBUG));
        assert!(!under_cap("reqwest", &Level::TRACE));
        assert!(under_cap("mejl::login", &Level::TRACE));
        // A crate whose name only starts like a capped one is not capped.
        assert!(under_cap("hyperion", &Level::TRACE));
    }

    fn log_with(level: &str, emit: impl FnOnce()) -> String {
        let dir = tempfile::tempdir().unwrap();
        let log_dir = dir.path().join("log");
        let filter = build_filter(Some(level), None).unwrap();
        let (subscriber, guard) = file_subscriber(&log_dir, filter).unwrap();
        tracing::subscriber::with_default(subscriber, emit);
        drop(guard);
        let mut text = String::new();
        for entry in std::fs::read_dir(&log_dir).unwrap() {
            text.push_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap());
        }
        text
    }

    #[test]
    fn quiet_levels_stay_quiet_for_capped_crates() {
        let text = log_with("off", || {
            tracing::info!(target: "hyper", "hyper info line");
            tracing::error!(target: "mejl", "mejl error line");
        });
        assert!(text.is_empty(), "{text}");
        let text = log_with("error", || {
            tracing::info!(target: "reqwest", "reqwest info line");
            tracing::error!(target: "mejl", "mejl error line");
        });
        assert!(!text.contains("reqwest info line"), "{text}");
        assert!(text.contains("mejl error line"), "{text}");
    }

    #[test]
    fn trace_level_still_drops_header_noise() {
        let text = log_with("trace", || {
            tracing::debug!(target: "hyper::proto", "hyper debug line");
            tracing::info!(target: "hyper::proto", "hyper info line");
            tracing::debug!(target: "mejl::sync", "mejl debug line");
        });
        assert!(!text.contains("hyper debug line"), "{text}");
        assert!(text.contains("hyper info line"), "{text}");
        assert!(text.contains("mejl debug line"), "{text}");
    }

    #[test]
    fn file_logging_writes_redacted_lines() {
        let dir = tempfile::tempdir().unwrap();
        let log_dir = dir.path().join("state").join("log");
        let filter = build_filter(Some("info"), None).unwrap();
        let (subscriber, guard) = file_subscriber(&log_dir, filter).unwrap();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("hello Bearer abc123 world");
            tracing::debug!("this is filtered out");
        });
        drop(guard);
        let mut text = String::new();
        for entry in std::fs::read_dir(&log_dir).unwrap() {
            text.push_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap());
        }
        assert!(text.contains("hello Bearer [redacted] world"), "{text}");
        assert!(!text.contains("abc123"));
        assert!(!text.contains("filtered out"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&log_dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
    }
}
