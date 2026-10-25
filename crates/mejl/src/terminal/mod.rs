//! Terminal setup, restore and suspend.
//!
//! [`enter`] switches the terminal into the mode the TUI needs: raw input,
//! the alternate screen, bracketed paste, focus events, mouse buttons and
//! drag with SGR coordinates (modes 1000, 1002 and 1006), kitty keyboard
//! flags when the terminal supports them, and theme reports (mode 2031)
//! when asked. [`restore`] undoes exactly what was
//! turned on, in reverse order, and is safe to call more than once and from
//! any thread.
//!
//! The terminal is restored on every way out:
//!
//! - a normal quit, when the [`TerminalGuard`] drops;
//! - a panic that will end the process ([`install_panic_hook`]);
//! - SIGTERM, SIGINT or SIGHUP ([`signals`]). Raw mode turns Ctrl+C into a
//!   key, so these only come from `kill` or a closed terminal.
//!
//! [`suspend_for`] leaves the TUI to run something in the foreground, such
//! as `$EDITOR` ([`editor`]) or a Ctrl+Z stop ([`suspend_process`]), and
//! comes back. The caller must pause the input thread first, or it steals
//! the editor's keystrokes.

pub mod cache;
pub mod editor;
pub mod osc;
pub mod probe;
pub mod quirks;
pub mod signals;
pub mod theme;

use std::cell::Cell;
use std::fmt;
use std::io::{self, Write};
use std::sync::{Mutex, PoisonError};

use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{Command, cursor, execute, terminal};

/// What to switch on when entering the TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalOptions {
    /// Mouse buttons, wheel and drag (modes 1000, 1002, 1006). No motion
    /// tracking (1003), which floods events.
    pub mouse: bool,
    /// Bracketed paste (2004), so a pasted text arrives as one event.
    pub bracketed_paste: bool,
    /// Focus in and out events (1004).
    pub focus_events: bool,
    /// Push kitty keyboard flags. Only set this when the probe found
    /// support; see [`probe::Capabilities::kitty_keyboard`].
    pub kitty_keyboard: bool,
    /// Theme reports (mode 2031). Only set this when the probe found
    /// support and something reads the reports; see [`theme`].
    pub theme_reports: bool,
}

impl Default for TerminalOptions {
    fn default() -> Self {
        TerminalOptions {
            mouse: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: false,
            theme_reports: false,
        }
    }
}

/// The kitty flags mejl asks for: unambiguous Esc and modified keys, plus
/// the base layout key for shortcuts on non-US layouts. No release events;
/// the UI does not use them.
pub const KITTY_FLAGS: KeyboardEnhancementFlags =
    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        .union(KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS);

/// Mouse buttons and drag with SGR coordinates: modes 1000, 1002 and 1006.
///
/// crossterm's `EnableMouseCapture` also turns on 1003 (every motion) and
/// 1015, which floods the input thread with motion events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnableMouseButtons;

impl Command for EnableMouseButtons {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("\x1b[?1000h\x1b[?1002h\x1b[?1006h")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        crossterm::event::EnableMouseCapture.execute_winapi()
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        false
    }
}

static ACTIVE: Mutex<Option<TerminalOptions>> = Mutex::new(None);

/// Restores the terminal when dropped.
#[derive(Debug)]
pub struct TerminalGuard {
    _private: (),
}

impl TerminalGuard {
    /// The options the terminal is in, while it is active.
    pub fn options(&self) -> Option<TerminalOptions> {
        *ACTIVE.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = restore();
    }
}

/// Switches the terminal into TUI mode. If any step fails, what was already
/// switched on is undone before the error returns.
pub fn enter(options: TerminalOptions) -> io::Result<TerminalGuard> {
    let mut active = ACTIVE.lock().unwrap_or_else(PoisonError::into_inner);
    if active.is_some() {
        return Err(io::Error::other("the terminal is already in TUI mode"));
    }
    *active = Some(options);
    drop(active);
    if let Err(e) = switch_on(options) {
        let _ = restore();
        return Err(e);
    }
    Ok(TerminalGuard { _private: () })
}

fn switch_on(options: TerminalOptions) -> io::Result<()> {
    terminal::enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen)?;
    if options.bracketed_paste {
        execute!(out, EnableBracketedPaste)?;
    }
    if options.focus_events {
        execute!(out, EnableFocusChange)?;
    }
    if options.mouse {
        execute!(out, EnableMouseButtons)?;
    }
    if options.kitty_keyboard {
        execute!(out, PushKeyboardEnhancementFlags(KITTY_FLAGS))?;
    }
    if options.theme_reports {
        execute!(out, theme::EnableThemeReports)?;
    }
    out.flush()
}

/// Puts the terminal back as it was. Does nothing when the TUI is not
/// active. Every step runs even if an earlier one fails; the first error is
/// returned.
pub fn restore() -> io::Result<()> {
    let options = ACTIVE.lock().unwrap_or_else(PoisonError::into_inner).take();
    match options {
        Some(options) => switch_off(options),
        None => Ok(()),
    }
}

/// The panic path: never blocks on the state lock, in case the panic
/// happened while it was held.
fn restore_after_panic() {
    let options = match ACTIVE.try_lock() {
        Ok(mut guard) => guard.take(),
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner().take(),
        // Held elsewhere: restore everything, which is always safe.
        Err(std::sync::TryLockError::WouldBlock) => Some(TerminalOptions {
            mouse: true,
            bracketed_paste: true,
            focus_events: true,
            kitty_keyboard: true,
            theme_reports: true,
        }),
    };
    if let Some(options) = options {
        let _ = switch_off(options);
    }
}

fn switch_off(options: TerminalOptions) -> io::Result<()> {
    let mut out = io::stdout();
    let mut first: Option<io::Error> = None;
    let mut keep = |r: io::Result<()>| {
        if let Err(e) = r
            && first.is_none()
        {
            first = Some(e);
        }
    };
    if options.theme_reports {
        keep(execute!(out, theme::DisableThemeReports));
    }
    if options.kitty_keyboard {
        keep(execute!(out, PopKeyboardEnhancementFlags));
    }
    if options.mouse {
        keep(execute!(out, DisableMouseCapture));
    }
    if options.focus_events {
        keep(execute!(out, DisableFocusChange));
    }
    if options.bracketed_paste {
        keep(execute!(out, DisableBracketedPaste));
    }
    keep(execute!(out, LeaveAlternateScreen, cursor::Show));
    keep(out.flush());
    keep(terminal::disable_raw_mode());
    first.map_or(Ok(()), Err)
}

/// Whether the TUI mode is on.
pub fn is_active() -> bool {
    ACTIVE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_some()
}

/// Leaves TUI mode, runs `f` on the normal screen, and enters TUI mode
/// again with the same options. When the TUI is not active, `f` just runs.
///
/// Pause the input thread before calling this. After it returns, the UI
/// must clear and redraw the whole screen.
pub fn suspend_for<T>(f: impl FnOnce() -> T) -> io::Result<T> {
    let options = ACTIVE.lock().unwrap_or_else(PoisonError::into_inner).take();
    let Some(options) = options else {
        return Ok(f());
    };
    switch_off(options)?;
    let result = f();
    *ACTIVE.lock().unwrap_or_else(PoisonError::into_inner) = Some(options);
    if let Err(e) = switch_on(options) {
        let _ = restore();
        return Err(e);
    }
    Ok(result)
}

/// Stops the process as Ctrl+Z does in a normal program, with the terminal
/// restored while stopped. Returns once the shell resumes mejl with `fg`.
///
/// In raw mode Ctrl+Z is a key, so the UI calls this when it sees one.
#[cfg(unix)]
pub fn suspend_process() -> io::Result<()> {
    suspend_for(|| {
        // SAFETY: raise only sends a signal to this process. SIGTSTP is left
        // at its default action, which stops the process until SIGCONT.
        unsafe {
            libc::raise(libc::SIGTSTP);
        }
    })
}

thread_local! {
    static CATCHES_PANICS: Cell<bool> = const { Cell::new(false) };
}

/// Marks the current thread as one that catches its own panics, such as a
/// render worker. A panic there is logged but leaves the TUI running.
///
/// Every tokio runtime mejl builds must pass this to
/// `Builder::on_thread_start`, whatever its threads are called: tokio
/// catches task panics, and the mark covers its blocking threads too.
pub fn mark_thread_catches_panics() {
    CATCHES_PANICS.with(|c| c.set(true));
}

/// Thread name prefixes whose panics are caught by their owner, or end only
/// that thread while the TUI keeps running:
///
/// - `tokio-runtime-worker`: tokio catches panics in tasks and in
///   `spawn_blocking`, and reports them as a `JoinError`.
/// - `mejl-sync`: the sync engine's runtime (`mejl-sync-worker`) is built
///   inside mejl-sync, so it cannot be marked with
///   [`mark_thread_catches_panics`]; tokio catches its task panics. A panic
///   in the engine's driver thread (`mejl-sync`) stops the engine, and the
///   app then reports that changes cannot be saved.
/// - `mejl-store`: the store thread runs each job under `catch_unwind`.
pub const CATCHING_THREAD_PREFIXES: &[&str] = &["tokio-runtime-worker", "mejl-sync", "mejl-store"];

/// Whether a panic on this thread will end the process, so the terminal
/// must be restored before the message prints.
///
/// mejl-render catches html2text panics and tokio catches task panics, but
/// a panic hook runs before any of that unwinding. Restoring the terminal
/// for those would tear down a TUI that keeps running.
pub fn panic_is_fatal(thread_name: Option<&str>, thread_catches: bool) -> bool {
    if thread_catches {
        return false;
    }
    match thread_name {
        Some(name) => !CATCHING_THREAD_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix)),
        None => true,
    }
}

/// Installs a panic hook that logs every panic and, for panics that end the
/// process, restores the terminal before the default message prints.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let catches = CATCHES_PANICS.with(Cell::get);
        let fatal = panic_is_fatal(thread.name(), catches);
        tracing::error!(
            thread = thread.name().unwrap_or("unnamed"),
            fatal,
            "panic: {info}"
        );
        if fatal {
            restore_after_panic();
            previous(info);
        }
    }));
}

/// The terminal size in columns and rows, if it can be read.
pub fn size() -> Option<(u16, u16)> {
    terminal::size().ok().filter(|&(c, r)| c > 0 && r > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panics_on_main_and_ui_threads_are_fatal() {
        assert!(panic_is_fatal(Some("main"), false));
        assert!(panic_is_fatal(Some("mejl-input"), false));
        assert!(panic_is_fatal(None, false));
    }

    #[test]
    fn caught_panics_are_not_fatal() {
        assert!(!panic_is_fatal(Some("mejl-render-0"), true));
        assert!(!panic_is_fatal(Some("tokio-runtime-worker"), false));
        assert!(!panic_is_fatal(None, true));
        // The engine's threads, which mejl cannot mark from outside.
        assert!(!panic_is_fatal(Some("mejl-sync-worker"), false));
        assert!(!panic_is_fatal(Some("mejl-sync"), false));
        assert!(!panic_is_fatal(Some("mejl-store"), false));
    }

    #[test]
    fn marking_is_per_thread() {
        let marked = std::thread::spawn(|| {
            mark_thread_catches_panics();
            CATCHES_PANICS.with(Cell::get)
        })
        .join()
        .unwrap();
        assert!(marked);
        assert!(!CATCHES_PANICS.with(Cell::get));
    }

    #[test]
    fn mouse_command_has_no_motion_tracking() {
        let mut s = String::new();
        EnableMouseButtons.write_ansi(&mut s).unwrap();
        assert_eq!(s, "\x1b[?1000h\x1b[?1002h\x1b[?1006h");
        assert!(!s.contains("1003"));
    }

    #[test]
    fn restore_without_enter_is_a_no_op() {
        assert!(restore().is_ok());
        assert!(!is_active());
        assert_eq!(suspend_for(|| 7).unwrap(), 7);
    }

    #[test]
    fn kitty_flags_are_the_safe_pair() {
        assert!(KITTY_FLAGS.contains(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES));
        assert!(!KITTY_FLAGS.contains(KeyboardEnhancementFlags::REPORT_EVENT_TYPES));
    }
}
