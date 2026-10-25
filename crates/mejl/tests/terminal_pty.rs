//! Terminal setup, restore and suspend on a real pty.
//!
//! Each test starts this test binary again as a child whose stdin, stdout
//! and stderr are the slave side of a fresh pty, with `MEJL_PTY_SCENARIO`
//! naming what the child does (see [`pty_child`]). The parent reads the
//! master side, checks the escape sequences and the termios state at each
//! step, and types keys when the child waits for them.
//!
//! The developer's own terminal is never touched. The children use stdin,
//! which crossterm picks over `/dev/tty` when it is a terminal. The one
//! scenario that opens `/dev/tty` (the probe) starts a new session and makes
//! the pty its controlling terminal first, and fails if that does not work.

#![cfg(unix)]

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode};
use mejl::events::{self, LoopEvent};
use mejl::input::InputThread;
use mejl::terminal::{self, TerminalOptions};

const SCENARIO: &str = "MEJL_PTY_SCENARIO";
const WAIT: Duration = Duration::from_secs(10);

// The parent side.

struct Pty {
    master: OwnedFd,
    slave: OwnedFd,
}

fn open_pty() -> Pty {
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    let mut size = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: openpty writes two descriptors into the out pointers; the
    // name and termios pointers may be null.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            // A raw pointer: Linux takes `*const`, macOS `*mut`.
            &raw mut size,
        )
    };
    assert_eq!(rc, 0, "openpty: {}", std::io::Error::last_os_error());
    // SAFETY: both descriptors are fresh and owned by nobody else.
    unsafe {
        Pty {
            master: OwnedFd::from_raw_fd(master),
            slave: OwnedFd::from_raw_fd(slave),
        }
    }
}

/// Everything the child wrote, read on a thread so the child never blocks
/// on a full pty buffer.
struct Output {
    bytes: Arc<Mutex<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Output {
    fn read_from(master: &OwnedFd) -> Output {
        let file = std::fs::File::from(master.try_clone().unwrap());
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::spawn({
            let bytes = Arc::clone(&bytes);
            let stop = Arc::clone(&stop);
            move || {
                let mut file = file;
                let mut buf = [0u8; 4096];
                while !stop.load(Ordering::SeqCst) {
                    if !readable(file.as_raw_fd(), Duration::from_millis(20)) {
                        continue;
                    }
                    match file.read(&mut buf) {
                        // EIO once every slave descriptor is closed.
                        Ok(0) | Err(_) => break,
                        Ok(n) => bytes.lock().unwrap().extend_from_slice(&buf[..n]),
                    }
                }
            }
        });
        Output {
            bytes,
            stop,
            thread: Some(thread),
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes.lock().unwrap()).into_owned()
    }

    /// Waits for `needle` and returns the output up to and including it.
    fn wait_for(&self, needle: &str) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let text = self.text();
            if let Some(at) = text.find(needle) {
                return text[..at + needle.len()].to_owned();
            }
            assert!(
                Instant::now() < deadline,
                "no {needle:?} from the child; it wrote {text:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn readable(fd: RawFd, timeout: Duration) -> bool {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
    // SAFETY: one valid pollfd for an open descriptor.
    unsafe { libc::poll(&mut pfd, 1, ms) > 0 }
}

/// Kills the child if a check fails, so no stopped or waiting child is
/// left behind.
struct Running {
    child: Option<Child>,
    output: Output,
    pty: Pty,
}

impl Running {
    fn start(scenario: &str) -> Running {
        Self::start_with(scenario, |_| {})
    }

    fn start_with(scenario: &str, adjust: impl FnOnce(&mut Command)) -> Running {
        let pty = open_pty();
        let output = Output::read_from(&pty.master);
        let stdio = || Stdio::from(pty.slave.try_clone().unwrap());
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", "pty_child", "--nocapture", "--test-threads=1"])
            .env(SCENARIO, scenario)
            .stdin(stdio())
            .stdout(stdio())
            .stderr(stdio());
        adjust(&mut cmd);
        let child = cmd.spawn().unwrap();
        Running {
            child: Some(child),
            output,
            pty,
        }
    }

    fn pid(&self) -> libc::pid_t {
        let id = self.child.as_ref().unwrap().id();
        libc::pid_t::try_from(id).unwrap()
    }

    fn type_keys(&self, bytes: &[u8]) {
        let mut master = std::fs::File::from(self.pty.master.try_clone().unwrap());
        master.write_all(bytes).unwrap();
    }

    /// Whether the pty is in raw mode: no line editing and no echo.
    fn raw(&self) -> bool {
        // SAFETY: termios is plain data, filled in by tcgetattr.
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: a valid descriptor and a valid termios pointer.
        let rc = unsafe { libc::tcgetattr(self.pty.slave.as_raw_fd(), &mut t) };
        assert_eq!(rc, 0, "tcgetattr: {}", std::io::Error::last_os_error());
        t.c_lflag & (libc::ICANON | libc::ECHO) == 0
    }

    /// Waits for the child to exit and checks it passed.
    fn finish(mut self) {
        let mut child = self.child.take().unwrap();
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                // Let the reader catch the last bytes.
                std::thread::sleep(Duration::from_millis(50));
                assert!(
                    status.success(),
                    "child failed with {status}: {:?}",
                    self.output.text()
                );
                return;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("child did not exit: {:?}", self.output.text());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Asserts that `needles` appear in `text` in this order.
fn in_order(text: &str, needles: &[&str]) {
    let mut from = 0;
    for n in needles {
        match text[from..].find(n) {
            Some(at) => from += at + n.len(),
            None => panic!("{n:?} missing or out of order in {text:?}"),
        }
    }
}

/// The part of `text` after the first `marker`.
fn after<'a>(text: &'a str, marker: &str) -> &'a str {
    let at = text.find(marker).expect("marker");
    &text[at + marker.len()..]
}

#[test]
fn enter_and_restore_switch_every_mode() {
    let run = Running::start("enter_restore");
    let entered = run.output.wait_for("<entered>");
    assert!(run.raw(), "raw mode is on in the TUI");
    in_order(
        &entered,
        &[
            "\x1b[?1049h",
            "\x1b[?2004h",
            "\x1b[?1004h",
            "\x1b[?1000h\x1b[?1002h\x1b[?1006h",
            "\x1b[>5u",
            "\x1b[?2031h",
            "<entered>",
        ],
    );
    assert!(!entered.contains("1003h"), "no motion tracking");

    run.type_keys(b"x");
    let restored = run.output.wait_for("<restored>");
    assert!(!run.raw(), "cooked mode is back");
    in_order(
        after(&restored, "<entered>"),
        &[
            "\x1b[?2031l",
            "\x1b[<1u",
            "\x1b[?1000l",
            "\x1b[?1004l",
            "\x1b[?2004l",
            "\x1b[?1049l",
            "\x1b[?25h",
            "<restored>",
        ],
    );
    run.finish();
}

#[test]
fn suspend_for_runs_on_the_normal_screen() {
    let run = Running::start("suspend_for");
    run.output.wait_for("<entered>");
    assert!(run.raw());
    run.type_keys(b"a");

    let inside = run.output.wait_for("<inside>");
    assert!(!run.raw(), "the editor gets a cooked terminal");
    in_order(after(&inside, "<entered>"), &["\x1b[?1049l", "<inside>"]);
    // Cooked mode: the line goes through on Enter.
    run.type_keys(b"b\n");

    let back = run.output.wait_for("<back>");
    assert!(run.raw(), "TUI mode again");
    in_order(
        after(&back, "<inside>"),
        &["\x1b[?1049h", "\x1b[?2004h", "<back>"],
    );
    run.type_keys(b"c");
    run.output.wait_for("<done>");
    assert!(!run.raw());
    run.finish();
}

#[test]
fn suspend_process_stops_with_the_terminal_restored() {
    let run = Running::start("suspend_process");
    run.output.wait_for("<entered>");
    assert!(run.raw());
    run.type_keys(b"a");

    // The child is in a process group of its own in this session, so the
    // stop is real; an orphaned group would ignore SIGTSTP.
    let pid = run.pid();
    let deadline = Instant::now() + WAIT;
    let status = loop {
        let mut status = 0;
        // SAFETY: waits on our own child; WNOHANG never blocks.
        let rc = unsafe { libc::waitpid(pid, &mut status, libc::WUNTRACED | libc::WNOHANG) };
        if rc == pid {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "child did not stop: {:?}",
            run.output.text()
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(libc::WIFSTOPPED(status), "stopped, not exited: {status}");
    assert_eq!(libc::WSTOPSIG(status), libc::SIGTSTP);
    assert!(!run.raw(), "the shell gets a cooked terminal while stopped");
    in_order(
        after(&run.output.text(), "<entered>"),
        &["\x1b[?1049l", "\x1b[?25h"],
    );

    // SAFETY: SIGCONT to our own stopped child.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGCONT) }, 0);
    let resumed = run.output.wait_for("<resumed>");
    assert!(run.raw(), "TUI mode again after fg");
    in_order(
        after(&resumed, "\x1b[?1049l"),
        &["\x1b[?1049h", "<resumed>"],
    );
    run.type_keys(b"b");
    run.output.wait_for("<done>");
    run.finish();
}

#[test]
fn paused_input_leaves_every_key_to_the_editor() {
    let run = Running::start("input_pause");
    run.output.wait_for("<ready>");
    run.type_keys(b"a");
    run.output.wait_for("<paused>");
    // What an editor would read. The child checks it got all three.
    run.type_keys(b"xyz");
    run.output.wait_for("<resumed>");
    run.type_keys(b"b");
    run.output.wait_for("<ok>");
    run.finish();
}

#[test]
fn late_probe_replies_never_reach_the_input() {
    let run = Running::start_with("probe", |cmd| {
        use std::os::unix::process::CommandExt;
        // SAFETY: only async-signal-safe calls between fork and exec. A new
        // session, then the pty (already stdin) as its controlling terminal,
        // so the probe's `/dev/tty` is the pty.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    });
    // A slow terminal that never answers DA1: half a reply well before the
    // 150 ms timeout, the rest well after it but inside the drain's cap.
    run.output.wait_for("\x1b[c");
    std::thread::sleep(Duration::from_millis(40));
    run.type_keys(b"\x1b]11;rgb:1e1e/");
    std::thread::sleep(Duration::from_millis(260));
    run.type_keys(b"1e1e/2e2e\x1b\\\x1b[?997;1n");
    run.output.wait_for("<probed>");
    run.type_keys(b"j");
    let done = run.output.wait_for("<done>");
    assert!(done.contains("<background (30, 30, 46)>"), "{done:?}");
    run.finish();
}

// The child side.

/// Runs the scenario the parent named; does nothing in a normal test run.
#[test]
fn pty_child() {
    let Ok(scenario) = std::env::var(SCENARIO) else {
        return;
    };
    match scenario.as_str() {
        "enter_restore" => {
            let guard = terminal::enter(TerminalOptions {
                kitty_keyboard: true,
                theme_reports: true,
                ..TerminalOptions::default()
            })
            .unwrap();
            say("<entered>");
            read_bytes(1);
            drop(guard);
            say("<restored>");
        }
        "suspend_for" => {
            let guard = terminal::enter(TerminalOptions::default()).unwrap();
            say("<entered>");
            read_bytes(1);
            terminal::suspend_for(|| {
                say("<inside>");
                read_bytes(2);
            })
            .unwrap();
            say("<back>");
            read_bytes(1);
            drop(guard);
            say("<done>");
        }
        "suspend_process" => {
            // SAFETY: moves this process into a group of its own.
            assert_eq!(unsafe { libc::setpgid(0, 0) }, 0);
            let guard = terminal::enter(TerminalOptions::default()).unwrap();
            say("<entered>");
            read_bytes(1);
            terminal::suspend_process().unwrap();
            say("<resumed>");
            read_bytes(1);
            drop(guard);
            say("<done>");
        }
        "input_pause" => {
            let guard = terminal::enter(TerminalOptions {
                mouse: false,
                focus_events: false,
                ..TerminalOptions::default()
            })
            .unwrap();
            let (tx, rx) = events::channel::<()>();
            let input = InputThread::spawn(tx).unwrap();
            say("<ready>");
            assert_eq!(next_key(&rx), KeyCode::Char('a'));
            input.pause().unwrap();
            say("<paused>");
            assert_eq!(read_bytes(3), b"xyz", "the editor got every key");
            input.resume();
            say("<resumed>");
            // No ghost of x, y or z comes through after the resume.
            assert_eq!(next_key(&rx), KeyCode::Char('b'));
            say("<ok>");
            drop(input);
            drop(guard);
        }
        "probe" => {
            let (replies, status) =
                terminal::probe::probe_tty(terminal::probe::DEFAULT_TIMEOUT).unwrap();
            assert_eq!(status, terminal::probe::ProbeStatus::TimedOut);
            assert_eq!(replies.theme_dark, Some(true));
            say("<probed>");
            crossterm::terminal::enable_raw_mode().unwrap();
            // Only the key typed after the probe is left to read.
            let rest = read_bytes(1);
            crossterm::terminal::disable_raw_mode().unwrap();
            assert_eq!(rest, b"j");
            assert!(!readable(0, Duration::from_millis(100)), "nothing else");
            let bg = replies.background.unwrap();
            say(&format!("<background {bg:?}><done>"));
        }
        other => panic!("unknown scenario {other:?}"),
    }
}

fn say(text: &str) {
    let mut out = std::io::stdout();
    out.write_all(text.as_bytes()).unwrap();
    out.flush().unwrap();
}

/// Reads exactly `n` bytes from fd 0, failing after [`WAIT`]. Not through
/// `std::io::stdin()`, whose buffer would hide bytes from `poll`.
fn read_bytes(n: usize) -> Vec<u8> {
    let deadline = Instant::now() + WAIT;
    let mut out = Vec::new();
    while out.len() < n {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "timed out after {out:?}");
        if readable(0, left) {
            let mut b = [0u8; 1];
            // SAFETY: reads one byte into a one-byte buffer.
            let rc = unsafe { libc::read(0, b.as_mut_ptr().cast(), 1) };
            if rc == 1 {
                out.push(b[0]);
            }
        }
    }
    out
}

/// The next key from the input thread, failing after [`WAIT`].
fn next_key(rx: &events::LoopReceiver<()>) -> KeyCode {
    let deadline = Instant::now() + WAIT;
    loop {
        for ev in rx.recv_batch_until(deadline) {
            match ev {
                LoopEvent::Input(t) => {
                    if let Event::Key(k) = t.event {
                        return k.code;
                    }
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(Instant::now() < deadline, "no key arrived");
    }
}
