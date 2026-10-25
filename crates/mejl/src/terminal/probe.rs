//! What the terminal can do.
//!
//! Two sources:
//!
//! - The environment ([`from_env`]): `COLORTERM` for truecolor, `NO_COLOR`,
//!   `TERM`, `TERM_PROGRAM`, multiplexers (`TMUX`, `ZELLIJ`, `STY`) and
//!   `SSH_TTY`. Free and instant.
//! - One burst of queries written to the terminal ([`probe_tty`]): the kitty
//!   keyboard flags (`CSI ? u`), synchronized output (DECRQM 2026), the
//!   theme mode (`CSI ? 996 n`), the background colour (OSC 11) and the
//!   terminal's name (XTVERSION), then DA1 last. Terminals answer in order
//!   and almost all answer DA1, so once its reply arrives, any reply still
//!   missing means "not supported". A timeout bounds slow or silent
//!   terminals.
//!
//! The probe reads the replies itself, so it must finish before the input
//! thread starts; otherwise crossterm would read the replies as keys. It
//! runs only when stdin and stdout are both terminals, so `mejl doctor |
//! less` never fights `less` for the keyboard.
//!
//! When the time runs out before DA1, the replies still on their way would
//! reach crossterm instead. That is worse than it sounds: crossterm reads
//! an OSC or DCS string as keys, and a `CSI ? ... $ y` or `CSI ? ... n`
//! reply makes it buffer, and drop, every key up to the next `u` or `c`.
//! So after a timeout [`drain_late_replies`] keeps reading until the
//! terminal goes quiet, with a hard cap. Anything else read then, such as
//! keys typed during startup, is dropped: there is no portable way to hand
//! bytes back to the terminal.
//!
//! [`quirks`](super::quirks) can skip the probe or give it more time, and
//! [`cache`](super::cache) fills in answers a slow terminal did not give.

use std::time::{Duration, Instant};

use crate::env::{Env, var};

/// Whether a feature works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Support {
    /// The terminal said so.
    Yes,
    /// The terminal answered DA1 but not this query.
    No,
    /// No answer, or no probe ran.
    #[default]
    Unknown,
}

impl Support {
    /// Plain words for `mejl doctor`.
    pub fn describe(self) -> &'static str {
        match self {
            Support::Yes => "yes",
            Support::No => "no",
            Support::Unknown => "unknown",
        }
    }
}

/// Light or dark background.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Background {
    /// A dark background.
    Dark,
    /// A light background.
    Light,
    /// Not known.
    #[default]
    Unknown,
}

/// A terminal multiplexer between mejl and the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Multiplexer {
    /// tmux.
    Tmux,
    /// Zellij.
    Zellij,
    /// GNU screen.
    Screen,
}

/// How the probe went.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ProbeStatus {
    /// No probe ran (not a terminal, or not asked).
    #[default]
    NotRun,
    /// DA1 arrived, so every answer is final.
    Complete,
    /// The time ran out before DA1. Missing answers are unknown.
    TimedOut,
    /// The terminal could not be queried.
    Failed(String),
}

/// Everything mejl knows about the terminal. A plain struct, so tests and
/// the UI can build one directly.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Capabilities {
    /// Columns and rows, when known.
    pub size: Option<(u16, u16)>,
    /// 24-bit colour works.
    pub truecolor: bool,
    /// `NO_COLOR` asks for no colour.
    pub no_color: bool,
    /// Background brightness.
    pub background: Background,
    /// The OSC 11 background colour, if the terminal sent one.
    pub background_rgb: Option<(u8, u8, u8)>,
    /// Whether the terminal reports theme changes (mode 2031).
    pub theme_reports: Support,
    /// The kitty keyboard protocol.
    pub kitty_keyboard: Support,
    /// Synchronized output (mode 2026).
    pub synchronized_output: Support,
    /// The XTVERSION reply, such as `kitty(0.49.2)`.
    pub terminal_version: Option<String>,
    /// `TERM`.
    pub term: Option<String>,
    /// `TERM_PROGRAM`.
    pub term_program: Option<String>,
    /// The multiplexer in between, if any.
    pub multiplexer: Option<Multiplexer>,
    /// Running over SSH.
    pub ssh: bool,
    /// How the probe went.
    pub probe: ProbeStatus,
}

impl Capabilities {
    /// Whether OSC 8 links are worth emitting. GNU screen corrupts the
    /// screen on long OSC 8 URLs, so they are off there.
    pub fn osc8_links(&self) -> bool {
        self.multiplexer != Some(Multiplexer::Screen)
    }

    /// Plain words about OSC 52 for `mejl doctor`, since no terminal can be
    /// asked whether it allows clipboard writes.
    pub fn clipboard_note(&self) -> &'static str {
        match self.multiplexer {
            Some(Multiplexer::Tmux) => {
                "OSC 52 through tmux needs `set -g set-clipboard on` in tmux.conf"
            }
            _ => "OSC 52 is used; some terminals ask first or turn it off by default",
        }
    }

    /// Adds probe answers to environment-based capabilities.
    pub fn apply_probe(&mut self, replies: &ProbeReplies, status: ProbeStatus) {
        let complete = status == ProbeStatus::Complete;
        let answered = |yes: Option<bool>| match (yes, complete) {
            (Some(true), _) => Support::Yes,
            (Some(false), _) => Support::No,
            (None, true) => Support::No,
            (None, false) => Support::Unknown,
        };
        self.kitty_keyboard = answered(replies.kitty_flags.map(|_| true));
        self.synchronized_output = answered(replies.sync_output);
        self.theme_reports = answered(replies.theme_dark.map(|_| true));
        self.background_rgb = replies.background;
        self.background = match (replies.theme_dark, replies.background) {
            (Some(true), _) => Background::Dark,
            (Some(false), _) => Background::Light,
            (None, Some(rgb)) => background_of(rgb),
            (None, None) => Background::Unknown,
        };
        if replies.terminal_version.is_some() {
            self.terminal_version = replies.terminal_version.clone();
        }
        self.probe = status;
    }
}

/// Capabilities from the environment alone.
pub fn from_env(env: &Env) -> Capabilities {
    let term = env.non_empty(var::TERM).map(str::to_owned);
    let colorterm = env
        .non_empty(var::COLORTERM)
        .unwrap_or("")
        .to_ascii_lowercase();
    let truecolor = matches!(colorterm.as_str(), "truecolor" | "24bit")
        || term
            .as_deref()
            .is_some_and(|t| t.ends_with("direct") || t.ends_with("truecolor"));
    let multiplexer = if env.non_empty(var::TMUX).is_some() {
        Some(Multiplexer::Tmux)
    } else if env.non_empty(var::ZELLIJ).is_some() {
        Some(Multiplexer::Zellij)
    } else if env.non_empty(var::STY).is_some() {
        Some(Multiplexer::Screen)
    } else {
        None
    };
    Capabilities {
        truecolor,
        no_color: env.get(var::NO_COLOR).is_some_and(|v| !v.is_empty()),
        term,
        term_program: env.non_empty(var::TERM_PROGRAM).map(str::to_owned),
        multiplexer,
        ssh: env.non_empty(var::SSH_TTY).is_some(),
        ..Capabilities::default()
    }
}

/// Light or dark from a colour, by perceived brightness.
pub fn background_of((r, g, b): (u8, u8, u8)) -> Background {
    let luma = 0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b);
    if luma < 128.0 {
        Background::Dark
    } else {
        Background::Light
    }
}

/// The query burst, DA1 last.
pub const QUERIES: &str = concat!(
    "\x1b[?u",         // kitty keyboard flags
    "\x1b[?2026$p",    // DECRQM synchronized output
    "\x1b[?996n",      // theme mode (2031)
    "\x1b]11;?\x1b\\", // background colour
    "\x1b[>q",         // XTVERSION
    "\x1b[c",          // DA1, the sentinel
);

/// The answers found in the terminal's reply bytes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProbeReplies {
    /// The kitty flags reply.
    pub kitty_flags: Option<u8>,
    /// DECRPM for 2026: `Some(true)` for set or reset, `Some(false)` for
    /// not recognised or permanently reset.
    pub sync_output: Option<bool>,
    /// Theme mode: `Some(true)` dark, `Some(false)` light.
    pub theme_dark: Option<bool>,
    /// OSC 11 background, scaled to 8 bits per channel.
    pub background: Option<(u8, u8, u8)>,
    /// XTVERSION text, control characters removed.
    pub terminal_version: Option<String>,
    /// The DA1 reply arrived.
    pub da1: bool,
}

/// Parses everything recognisable in `bytes`. Unknown sequences and stray
/// bytes are skipped; an unfinished sequence at the end is ignored.
pub fn parse_replies(bytes: &[u8]) -> ProbeReplies {
    let mut out = ProbeReplies::default();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != 0x1b || i + 1 >= bytes.len() {
            i += 1;
            continue;
        }
        match bytes[i + 1] {
            b'[' => match parse_csi(&bytes[i + 2..]) {
                Some((csi, used)) => {
                    apply_csi(&csi, &mut out);
                    i += 2 + used;
                }
                None => break,
            },
            b']' => match string_end(&bytes[i + 2..]) {
                Some((body, used)) => {
                    if let Some(spec) = body.strip_prefix(b"11;") {
                        out.background = std::str::from_utf8(spec).ok().and_then(parse_color);
                    }
                    i += 2 + used;
                }
                None => break,
            },
            b'P' => match string_end(&bytes[i + 2..]) {
                Some((body, used)) => {
                    if let Some(name) = body.strip_prefix(b">|") {
                        let text: String = String::from_utf8_lossy(name)
                            .chars()
                            .filter(|c| !c.is_control())
                            .take(128)
                            .collect();
                        if !text.trim().is_empty() {
                            out.terminal_version = Some(text.trim().to_owned());
                        }
                    }
                    i += 2 + used;
                }
                None => break,
            },
            _ => i += 1,
        }
    }
    out
}

struct Csi<'a> {
    params: &'a [u8],
    intermediates: &'a [u8],
    final_byte: u8,
}

/// Splits a CSI body (after `ESC [`) and returns it with the bytes used.
fn parse_csi(rest: &[u8]) -> Option<(Csi<'_>, usize)> {
    let params_end = rest
        .iter()
        .position(|b| !(0x30..=0x3f).contains(b))
        .unwrap_or(rest.len());
    let inter_end = params_end
        + rest[params_end..]
            .iter()
            .position(|b| !(0x20..=0x2f).contains(b))
            .unwrap_or(rest.len() - params_end);
    let final_byte = *rest.get(inter_end)?;
    if !(0x40..=0x7e).contains(&final_byte) {
        // Not a valid CSI. Skip the introducer only.
        return Some((
            Csi {
                params: &[],
                intermediates: &[],
                final_byte: 0,
            },
            0,
        ));
    }
    Some((
        Csi {
            params: &rest[..params_end],
            intermediates: &rest[params_end..inter_end],
            final_byte,
        },
        inter_end + 1,
    ))
}

fn apply_csi(csi: &Csi<'_>, out: &mut ProbeReplies) {
    let Some(params) = csi.params.strip_prefix(b"?") else {
        return;
    };
    let text = std::str::from_utf8(params).unwrap_or("");
    let nums: Vec<Option<u32>> = text.split(';').map(|p| p.parse().ok()).collect();
    match (csi.final_byte, csi.intermediates) {
        (b'u', b"") => {
            if let Some(Some(flags)) = nums.first() {
                out.kitty_flags = u8::try_from(*flags).ok();
            }
        }
        (b'y', b"$") => {
            if let [Some(2026), Some(ps)] = nums.as_slice() {
                out.sync_output = Some(matches!(ps, 1 | 2));
            }
        }
        (b'n', b"") => {
            if let [Some(997), Some(ps)] = nums.as_slice() {
                out.theme_dark = match ps {
                    1 => Some(true),
                    2 => Some(false),
                    _ => None,
                };
            }
        }
        (b'c', b"") => out.da1 = true,
        _ => {}
    }
}

/// Finds the end of an OSC or DCS string: BEL or ST (`ESC \`). Returns the
/// body and the bytes used, terminator included.
fn string_end(rest: &[u8]) -> Option<(&[u8], usize)> {
    for (i, &b) in rest.iter().enumerate() {
        if b == 0x07 {
            return Some((&rest[..i], i + 1));
        }
        if b == 0x1b {
            return match rest.get(i + 1) {
                Some(b'\\') => Some((&rest[..i], i + 2)),
                // Another sequence starts: this one was cut off.
                Some(_) => Some((&rest[..i], i)),
                None => None,
            };
        }
    }
    None
}

/// Parses an X11 colour spec as terminals send it: `rgb:R/G/B` with 1 to 4
/// hex digits per channel, `rgba:R/G/B/A`, or `#RGB`, `#RRGGBB`,
/// `#RRRGGGBBB`, `#RRRRGGGGBBBB`.
pub fn parse_color(spec: &str) -> Option<(u8, u8, u8)> {
    let spec = spec.trim();
    if let Some(rest) = spec
        .strip_prefix("rgb:")
        .or_else(|| spec.strip_prefix("rgba:"))
    {
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() < 3 || parts.len() > 4 {
            return None;
        }
        return Some((
            scale_hex(parts[0])?,
            scale_hex(parts[1])?,
            scale_hex(parts[2])?,
        ));
    }
    let hex = spec.strip_prefix('#')?;
    if hex.is_empty() || hex.len() % 3 != 0 || hex.len() > 12 {
        return None;
    }
    let n = hex.len() / 3;
    Some((
        scale_hex(&hex[..n])?,
        scale_hex(&hex[n..2 * n])?,
        scale_hex(&hex[2 * n..])?,
    ))
}

fn scale_hex(digits: &str) -> Option<u8> {
    if digits.is_empty() || digits.len() > 4 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(digits, 16).ok()?;
    let max = (1u32 << (4 * digits.len() as u32)) - 1;
    u8::try_from((value * 255 + max / 2) / max).ok()
}

/// The default probe timeout. Fast terminals answer in well under a
/// millisecond and most within 30 ms; DA1 ends the wait early.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(150);

/// Probes the terminal and returns environment and probe results together.
/// With `timeout` of `None`, or when stdin or stdout is not a terminal, no
/// queries are sent. The [`quirks`](super::quirks) table can skip the probe
/// or lengthen `timeout`.
pub fn detect(env: &Env, timeout: Option<Duration>) -> Capabilities {
    detect_with(env, timeout, None)
}

/// [`detect`], then the capability cache at `cache`: answers a timed-out
/// probe is missing come from an earlier run on the same terminal, and a
/// complete probe updates the file. The cache is skipped when `cache` is
/// `None`, which tests and demo mode use.
pub fn detect_with(
    env: &Env,
    timeout: Option<Duration>,
    cache: Option<&std::path::Path>,
) -> Capabilities {
    let mut caps = from_env(env);
    caps.size = super::size();
    let quirk = super::quirks::lookup(caps.term.as_deref(), caps.term_program.as_deref());
    if let Some(q) = quirk {
        tracing::debug!(quirk = q.name, "terminal quirk applies");
    }
    let timeout = match (timeout, quirk) {
        (_, Some(q)) if q.skip_probe => None,
        (Some(t), Some(q)) => Some(q.probe_timeout.map_or(t, |qt| qt.max(t))),
        (t, _) => t,
    };
    if let Some(timeout) = timeout
        && is_interactive()
    {
        match probe_tty(timeout) {
            Ok((replies, status)) => caps.apply_probe(&replies, status),
            Err(e) => caps.probe = ProbeStatus::Failed(e.to_string()),
        }
    }
    if let Some(path) = cache {
        super::cache::apply(&mut caps, env, path);
    }
    caps
}

/// Where the probe reads reply bytes from. The real one polls `/dev/tty`;
/// tests use a script with a clock of its own.
pub trait ReplySource {
    /// The current time on this source's clock.
    fn now(&self) -> Instant;

    /// Waits up to `timeout` for bytes and reads some into `buf`. `Ok(None)`
    /// when the time ran out, `Ok(Some(0))` at the end of input.
    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration)
    -> std::io::Result<Option<usize>>;
}

/// The most reply bytes kept. A terminal that floods more is cut off.
pub const MAX_REPLY_BYTES: usize = 8192;

/// How long [`drain_late_replies`] reads after a timed-out probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrainLimits {
    /// Stop after this long with no bytes, unless a reply is half read.
    pub quiet: Duration,
    /// Never read longer than this.
    pub cap: Duration,
}

/// The drain after a timed-out probe: a terminal that answered nothing
/// costs `quiet` more, a slow one at most `cap`.
pub const DRAIN: DrainLimits = DrainLimits {
    quiet: Duration::from_millis(50),
    cap: Duration::from_millis(500),
};

/// Reads replies until DA1 arrives or `timeout` passes, then, after a
/// timeout, drains late replies within [`DRAIN`]. A DA1 found while
/// draining still makes the probe complete.
pub fn run_probe<S: ReplySource>(
    src: &mut S,
    timeout: Duration,
) -> std::io::Result<(ProbeReplies, ProbeStatus)> {
    run_probe_with(src, timeout, DRAIN)
}

/// [`run_probe`] with other drain limits.
pub fn run_probe_with<S: ReplySource>(
    src: &mut S,
    timeout: Duration,
    limits: DrainLimits,
) -> std::io::Result<(ProbeReplies, ProbeStatus)> {
    let deadline = src.now() + timeout;
    let mut buf = Vec::with_capacity(512);
    let mut chunk = [0u8; 512];
    loop {
        let left = deadline.saturating_duration_since(src.now());
        if left.is_zero() {
            break;
        }
        match src.read_timeout(&mut chunk, left)? {
            None => continue,
            Some(0) => return Ok((parse_replies(&buf), ProbeStatus::TimedOut)),
            Some(n) => buf.extend_from_slice(&chunk[..n]),
        }
        let replies = parse_replies(&buf);
        if replies.da1 {
            return Ok((replies, ProbeStatus::Complete));
        }
        if buf.len() > MAX_REPLY_BYTES {
            return Ok((replies, ProbeStatus::TimedOut));
        }
    }
    let read_before = buf.len();
    let da1 = drain_late_replies(src, &mut buf, limits)?;
    if buf.len() > read_before {
        tracing::debug!(
            bytes = buf.len() - read_before,
            da1,
            "read late terminal replies after the probe timed out"
        );
    }
    let status = if da1 {
        ProbeStatus::Complete
    } else {
        ProbeStatus::TimedOut
    };
    Ok((parse_replies(&buf), status))
}

/// Reads what the terminal still sends after a timed-out probe, appending
/// it to `buf`. Stops when DA1 arrives (and returns `true`), after
/// `limits.quiet` with no bytes while no reply is half read, at
/// `limits.cap`, at the end of input, or past [`MAX_REPLY_BYTES`] more
/// bytes.
pub fn drain_late_replies<S: ReplySource>(
    src: &mut S,
    buf: &mut Vec<u8>,
    limits: DrainLimits,
) -> std::io::Result<bool> {
    let start = src.now();
    let hard_stop = start + limits.cap;
    let mut scanner = ReplyScanner::default();
    scanner.feed(buf);
    let mut quiet_until = start + limits.quiet;
    let mut drained = 0usize;
    let mut chunk = [0u8; 512];
    loop {
        let now = src.now();
        let stop = if scanner.in_sequence() {
            hard_stop
        } else {
            quiet_until.min(hard_stop)
        };
        let left = stop.saturating_duration_since(now);
        if left.is_zero() {
            return Ok(false);
        }
        let n = match src.read_timeout(&mut chunk, left)? {
            None => continue,
            Some(0) => return Ok(false),
            Some(n) => n,
        };
        let bytes = &chunk[..n];
        let from = buf.len();
        buf.extend_from_slice(bytes);
        scanner.feed(bytes);
        drained += n;
        quiet_until = src.now() + limits.quiet;
        // Only look at what came in, plus enough before it to find a DA1
        // split across reads.
        let tail = from.saturating_sub(64);
        if parse_replies(&buf[tail..]).da1 {
            return Ok(true);
        }
        if drained > MAX_REPLY_BYTES {
            return Ok(false);
        }
    }
}

/// Tracks whether a byte stream is in the middle of an escape sequence, so
/// the drain never stops inside a reply. The state carries across chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReplyScanner {
    state: ScanState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ScanState {
    /// Between sequences.
    #[default]
    Ground,
    /// After ESC.
    Escape,
    /// Inside a CSI, before its final byte.
    Csi,
    /// Inside an OSC, DCS, APC, PM or SOS string.
    Text,
    /// After ESC inside a string: `\` ends it.
    TextEscape,
}

impl ReplyScanner {
    /// Advances over `bytes`.
    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.state = match (self.state, b) {
                (ScanState::Ground, 0x1b) => ScanState::Escape,
                (ScanState::Ground, _) => ScanState::Ground,
                (ScanState::Escape, b'[') => ScanState::Csi,
                (ScanState::Escape, b']' | b'P' | b'_' | b'^' | b'X') => ScanState::Text,
                (ScanState::Escape, 0x1b) => ScanState::Escape,
                (ScanState::Escape, _) => ScanState::Ground,
                (ScanState::Csi, 0x40..=0x7e) => ScanState::Ground,
                (ScanState::Csi, 0x1b) => ScanState::Escape,
                (ScanState::Csi, 0x20..=0x3f) => ScanState::Csi,
                // Anything else cannot be in a CSI: it was cut off.
                (ScanState::Csi, _) => ScanState::Ground,
                (ScanState::Text, 0x07) => ScanState::Ground,
                (ScanState::Text, 0x1b) => ScanState::TextEscape,
                (ScanState::Text, _) => ScanState::Text,
                (ScanState::TextEscape, b'\\') => ScanState::Ground,
                // ESC then something else: the string was cut off and a
                // new sequence starts.
                (ScanState::TextEscape, b'[') => ScanState::Csi,
                (ScanState::TextEscape, b']' | b'P' | b'_' | b'^' | b'X') => ScanState::Text,
                (ScanState::TextEscape, 0x1b) => ScanState::Escape,
                (ScanState::TextEscape, _) => ScanState::Ground,
            };
        }
    }

    /// Whether the bytes so far end inside a sequence.
    pub fn in_sequence(&self) -> bool {
        self.state != ScanState::Ground
    }
}

/// Whether stdin and stdout are both terminals.
pub fn is_interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// Sends [`QUERIES`] to the terminal and reads replies with [`run_probe`].
/// Raw mode is switched on for the probe if it is off.
#[cfg(unix)]
pub fn probe_tty(timeout: Duration) -> std::io::Result<(ProbeReplies, ProbeStatus)> {
    use std::io::Write;

    let was_raw = crossterm::terminal::is_raw_mode_enabled()?;
    if !was_raw {
        crossterm::terminal::enable_raw_mode()?;
    }
    let result = (|| {
        let mut tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")?;
        tty.write_all(QUERIES.as_bytes())?;
        tty.flush()?;
        run_probe(&mut TtySource { tty }, timeout)
    })();
    if !was_raw {
        let _ = crossterm::terminal::disable_raw_mode();
    }
    result
}

/// `/dev/tty` as a [`ReplySource`].
#[cfg(unix)]
struct TtySource {
    tty: std::fs::File,
}

#[cfg(unix)]
impl ReplySource for TtySource {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn read_timeout(
        &mut self,
        buf: &mut [u8],
        timeout: Duration,
    ) -> std::io::Result<Option<usize>> {
        use std::io::Read;
        use std::os::fd::AsRawFd;

        let mut pfd = libc::pollfd {
            fd: self.tty.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = libc::c_int::try_from(timeout.as_millis().max(1)).unwrap_or(libc::c_int::MAX);
        // SAFETY: one valid pollfd for a descriptor `self.tty` keeps open.
        let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                return Ok(None);
            }
            return Err(err);
        }
        if rc == 0 {
            return Ok(None);
        }
        self.tty.read(buf).map(Some)
    }
}

/// Probing needs a Unix terminal; elsewhere only the environment is used.
#[cfg(not(unix))]
pub fn probe_tty(_timeout: Duration) -> std::io::Result<(ProbeReplies, ProbeStatus)> {
    Ok((ProbeReplies::default(), ProbeStatus::NotRun))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_detection() {
        let env = Env::from_pairs([
            ("COLORTERM", "truecolor"),
            ("TERM", "xterm-kitty"),
            ("TERM_PROGRAM", "kitty"),
            ("TMUX", "/tmp/tmux-1/default,1,0"),
            ("SSH_TTY", "/dev/pts/3"),
        ]);
        let caps = from_env(&env);
        assert!(caps.truecolor);
        assert!(!caps.no_color);
        assert_eq!(caps.term.as_deref(), Some("xterm-kitty"));
        assert_eq!(caps.multiplexer, Some(Multiplexer::Tmux));
        assert!(caps.ssh);
        assert!(caps.clipboard_note().contains("set-clipboard"));
        assert_eq!(caps.probe, ProbeStatus::NotRun);
        assert_eq!(caps.kitty_keyboard, Support::Unknown);
    }

    #[test]
    fn truecolor_signals() {
        assert!(from_env(&Env::from_pairs([("COLORTERM", "24BIT")])).truecolor);
        assert!(from_env(&Env::from_pairs([("TERM", "xterm-direct")])).truecolor);
        assert!(!from_env(&Env::from_pairs([("TERM", "xterm-256color")])).truecolor);
        assert!(!from_env(&Env::default()).truecolor);
    }

    #[test]
    fn no_color_needs_a_non_empty_value() {
        assert!(from_env(&Env::from_pairs([("NO_COLOR", "1")])).no_color);
        assert!(!from_env(&Env::from_pairs([("NO_COLOR", "")])).no_color);
        assert!(!from_env(&Env::default()).no_color);
    }

    #[test]
    fn screen_turns_links_off() {
        let caps = from_env(&Env::from_pairs([("STY", "1234.pts-0.host")]));
        assert_eq!(caps.multiplexer, Some(Multiplexer::Screen));
        assert!(!caps.osc8_links());
        assert!(from_env(&Env::from_pairs([("ZELLIJ", "0")])).osc8_links());
    }

    #[test]
    fn full_reply_parses() {
        let reply = b"\x1b[?1u\x1b[?2026;2$y\x1b[?997;1n\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1bP>|kitty(0.49.2)\x1b\\\x1b[?62;22c";
        let r = parse_replies(reply);
        assert_eq!(r.kitty_flags, Some(1));
        assert_eq!(r.sync_output, Some(true));
        assert_eq!(r.theme_dark, Some(true));
        assert_eq!(r.background, Some((0x1e, 0x1e, 0x2e)));
        assert_eq!(r.terminal_version.as_deref(), Some("kitty(0.49.2)"));
        assert!(r.da1);

        let mut caps = Capabilities::default();
        caps.apply_probe(&r, ProbeStatus::Complete);
        assert_eq!(caps.kitty_keyboard, Support::Yes);
        assert_eq!(caps.synchronized_output, Support::Yes);
        assert_eq!(caps.theme_reports, Support::Yes);
        assert_eq!(caps.background, Background::Dark);
    }

    #[test]
    fn da1_only_means_unsupported() {
        let r = parse_replies(b"\x1b[?1;2c");
        assert!(r.da1);
        assert_eq!(r.kitty_flags, None);
        let mut caps = Capabilities::default();
        caps.apply_probe(&r, ProbeStatus::Complete);
        assert_eq!(caps.kitty_keyboard, Support::No);
        assert_eq!(caps.synchronized_output, Support::No);
        assert_eq!(caps.background, Background::Unknown);
    }

    #[test]
    fn timeout_leaves_missing_answers_unknown() {
        let r = parse_replies(b"\x1b]11;rgb:ffff/ffff/ffff\x07");
        let mut caps = Capabilities::default();
        caps.apply_probe(&r, ProbeStatus::TimedOut);
        assert_eq!(caps.kitty_keyboard, Support::Unknown);
        assert_eq!(caps.background, Background::Light);
        assert_eq!(caps.background_rgb, Some((255, 255, 255)));
    }

    #[test]
    fn sync_mode_not_recognised() {
        assert_eq!(parse_replies(b"\x1b[?2026;0$y").sync_output, Some(false));
        assert_eq!(parse_replies(b"\x1b[?2026;4$y").sync_output, Some(false));
        assert_eq!(parse_replies(b"\x1b[?2026;1$y").sync_output, Some(true));
        // Another mode's reply is not 2026.
        assert_eq!(parse_replies(b"\x1b[?2027;1$y").sync_output, None);
    }

    #[test]
    fn theme_reply_light_wins_over_dark_background() {
        let r = parse_replies(b"\x1b[?997;2n\x1b]11;rgb:0000/0000/0000\x07\x1b[?6c");
        let mut caps = Capabilities::default();
        caps.apply_probe(&r, ProbeStatus::Complete);
        assert_eq!(caps.background, Background::Light);
    }

    #[test]
    fn colour_specs() {
        assert_eq!(parse_color("rgb:ff/80/00"), Some((255, 128, 0)));
        assert_eq!(parse_color("rgb:f/8/0"), Some((255, 136, 0)));
        assert_eq!(parse_color("rgb:fff/000/800"), Some((255, 0, 128)));
        assert_eq!(parse_color("rgb:ffff/0000/8000"), Some((255, 0, 128)));
        assert_eq!(parse_color("rgba:ffff/0000/0000/ffff"), Some((255, 0, 0)));
        assert_eq!(parse_color("#102030"), Some((0x10, 0x20, 0x30)));
        assert_eq!(parse_color("#fff"), Some((255, 255, 255)));
        assert_eq!(parse_color("#ffff00000000"), Some((255, 0, 0)));
        for bad in [
            "",
            "rgb:",
            "rgb:ff/ff",
            "rgb:fffff/0/0",
            "rgb:zz/00/00",
            "#ff",
            "#12345",
            "red",
        ] {
            assert_eq!(parse_color(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn brightness_split() {
        assert_eq!(background_of((0, 0, 0)), Background::Dark);
        assert_eq!(background_of((40, 42, 54)), Background::Dark);
        assert_eq!(background_of((253, 246, 227)), Background::Light);
    }

    #[test]
    fn noise_and_cut_sequences_are_tolerated() {
        // Typed keys before the reply, an unknown CSI, a cut OSC and a
        // reply that never finished.
        let r = parse_replies(b"jk\x1b[5~\x1b]11;rgb:00\x1b[?7u\x1b[?62c\x1b[?99");
        assert_eq!(r.kitty_flags, Some(7));
        assert!(r.da1);
        assert_eq!(r.background, None);
        assert_eq!(parse_replies(b""), ProbeReplies::default());
        assert_eq!(parse_replies(b"\x1b"), ProbeReplies::default());
        assert_eq!(parse_replies(b"\x1b[\x01"), ProbeReplies::default());
    }

    #[test]
    fn xtversion_control_characters_are_removed() {
        let r = parse_replies(b"\x1bP>|evil\x08\x07");
        assert_eq!(r.terminal_version.as_deref(), Some("evil"));
    }

    #[test]
    fn queries_end_with_da1() {
        assert!(QUERIES.ends_with("\x1b[c"));
        assert!(QUERIES.starts_with("\x1b[?u"));
    }

    /// A scripted terminal on a clock of its own: bytes, and silences
    /// that pass only while the probe waits.
    struct Script {
        steps: std::collections::VecDeque<Step>,
        now: Instant,
    }

    enum Step {
        Bytes(&'static [u8]),
        Silence(Duration),
    }

    impl Script {
        fn new(steps: Vec<Step>) -> Self {
            Script {
                steps: steps.into(),
                now: Instant::now(),
            }
        }

        /// Bytes the probe never read.
        fn unread(&self) -> Vec<u8> {
            self.steps
                .iter()
                .flat_map(|s| match s {
                    Step::Bytes(b) => b.to_vec(),
                    Step::Silence(_) => Vec::new(),
                })
                .collect()
        }
    }

    impl ReplySource for Script {
        fn now(&self) -> Instant {
            self.now
        }

        fn read_timeout(
            &mut self,
            buf: &mut [u8],
            timeout: Duration,
        ) -> std::io::Result<Option<usize>> {
            match self.steps.pop_front() {
                None => {
                    // Silent from here on.
                    self.now += timeout;
                    Ok(None)
                }
                Some(Step::Silence(d)) if d > timeout => {
                    self.now += timeout;
                    self.steps.push_front(Step::Silence(d - timeout));
                    Ok(None)
                }
                Some(Step::Silence(d)) => {
                    self.now += d;
                    self.read_timeout(buf, timeout - d)
                }
                Some(Step::Bytes(b)) => {
                    let n = b.len().min(buf.len());
                    buf[..n].copy_from_slice(&b[..n]);
                    if n < b.len() {
                        self.steps.push_front(Step::Bytes(&b[n..]));
                    }
                    Ok(Some(n))
                }
            }
        }
    }

    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn fast_terminal_needs_no_drain() {
        let mut src = Script::new(vec![
            Step::Silence(MS(2)),
            Step::Bytes(b"\x1b[?1u\x1b[?62;22c"),
            Step::Bytes(b"j"),
        ]);
        let (r, status) = run_probe(&mut src, DEFAULT_TIMEOUT).unwrap();
        assert_eq!(status, ProbeStatus::Complete);
        assert_eq!(r.kitty_flags, Some(1));
        // A key typed after DA1 is left for the input thread.
        assert_eq!(src.unread(), b"j");
    }

    #[test]
    fn late_osc_reply_without_da1_is_drained() {
        // A slow terminal that never answers DA1: the background reply
        // comes after the timeout and would otherwise become keys.
        let mut src = Script::new(vec![
            Step::Silence(MS(180)),
            Step::Bytes(b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\"),
            Step::Silence(MS(10)),
            Step::Bytes(b"\x1bP>|slowterm 1.0\x1b\\"),
        ]);
        let (r, status) = run_probe(&mut src, DEFAULT_TIMEOUT).unwrap();
        assert_eq!(status, ProbeStatus::TimedOut);
        assert!(src.unread().is_empty(), "{:?}", src.unread());
        assert_eq!(r.background, Some((0x1e, 0x1e, 0x2e)));
        assert_eq!(r.terminal_version.as_deref(), Some("slowterm 1.0"));
    }

    #[test]
    fn late_csi_replies_are_drained_too() {
        // crossterm would hold every key after these until a `u` or `c`.
        let mut src = Script::new(vec![
            Step::Silence(MS(170)),
            Step::Bytes(b"\x1b[?2026;2$y\x1b[?997;1n"),
        ]);
        let (r, _) = run_probe(&mut src, DEFAULT_TIMEOUT).unwrap();
        assert!(src.unread().is_empty());
        assert_eq!(r.sync_output, Some(true));
        assert_eq!(r.theme_dark, Some(true));
    }

    #[test]
    fn late_da1_makes_the_probe_complete() {
        let mut src = Script::new(vec![
            Step::Silence(MS(200)),
            Step::Bytes(b"\x1b[?62;"),
            Step::Bytes(b"22c"),
            Step::Bytes(b"k"),
        ]);
        let (r, status) = run_probe(&mut src, DEFAULT_TIMEOUT).unwrap();
        assert_eq!(status, ProbeStatus::Complete);
        assert!(r.da1);
        let mut caps = Capabilities::default();
        caps.apply_probe(&r, status);
        assert_eq!(caps.kitty_keyboard, Support::No);
        assert_eq!(src.unread(), b"k");
    }

    #[test]
    fn drain_waits_out_a_half_read_reply() {
        // The quiet gap passes in the middle of a reply; the drain keeps
        // reading until the reply ends.
        let mut src = Script::new(vec![
            Step::Silence(MS(160)),
            Step::Bytes(b"\x1b]11;rgb:00"),
            Step::Silence(MS(120)),
            Step::Bytes(b"00/0000/0000\x07"),
        ]);
        let (r, _) = run_probe(&mut src, DEFAULT_TIMEOUT).unwrap();
        assert!(src.unread().is_empty());
        assert_eq!(r.background, Some((0, 0, 0)));
    }

    #[test]
    fn drain_has_a_hard_cap() {
        let mut src = Script::new(vec![
            Step::Silence(MS(160)),
            // A string that never ends.
            Step::Bytes(b"\x1b]11;rgb:"),
            Step::Silence(Duration::from_secs(60)),
            Step::Bytes(b"0/0/0\x07"),
        ]);
        let start = src.now();
        let (_, status) = run_probe(&mut src, DEFAULT_TIMEOUT).unwrap();
        assert_eq!(status, ProbeStatus::TimedOut);
        assert_eq!(src.now() - start, DEFAULT_TIMEOUT + DRAIN.cap);
    }

    #[test]
    fn silent_terminal_costs_only_the_quiet_gap() {
        let mut src = Script::new(Vec::new());
        let start = src.now();
        let (r, status) = run_probe(&mut src, DEFAULT_TIMEOUT).unwrap();
        assert_eq!(status, ProbeStatus::TimedOut);
        assert_eq!(r, ProbeReplies::default());
        assert_eq!(src.now() - start, DEFAULT_TIMEOUT + DRAIN.quiet);
    }

    #[test]
    fn scanner_tracks_sequences_across_chunks() {
        let mut s = ReplyScanner::default();
        assert!(!s.in_sequence());
        for (chunk, inside) in [
            (&b"ab"[..], false),
            (b"\x1b", true),
            (b"]11;rgb:", true),
            (b"00\x1b", true),
            (b"\\", false),
            (b"\x1b[?2026", true),
            (b";2$", true),
            (b"y", false),
            (b"\x1bP>|x\x07", false),
            // A string cut off by a new CSI.
            (b"\x1b]11;x\x1b[", true),
            (b"c", false),
        ] {
            s.feed(chunk);
            assert_eq!(s.in_sequence(), inside, "after {chunk:?}");
        }
    }

    #[test]
    fn detect_without_timeout_sends_nothing() {
        let caps = detect(&Env::from_pairs([("NO_COLOR", "1")]), None);
        assert!(caps.no_color);
        assert_eq!(caps.probe, ProbeStatus::NotRun);
    }
}
