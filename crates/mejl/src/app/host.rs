//! What the app needs from the machine around it: opening links and files,
//! the clipboard, the external editor, and framing each frame for the
//! terminal. The real terminal implements it in [`TerminalHost`]; tests use
//! a recording fake, so the app runs headless.

use std::io::{self, Write};
use std::path::Path;

use crossterm::QueueableCommand;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};

use crate::input::InputThread;
use crate::terminal::editor::{self, EditorCommand};
use crate::terminal::osc;

/// The app's side effects outside the UI.
pub trait Host {
    /// Called before a frame is drawn.
    fn frame_begin(&mut self) {}

    /// Called after a frame is drawn and flushed.
    fn frame_end(&mut self) {}

    /// Opens a URL with the system handler. The caller has checked the
    /// scheme.
    fn open_url(&mut self, url: &str) -> Result<(), String>;

    /// Opens a local file with the system handler.
    fn open_path(&mut self, path: &Path) -> Result<(), String>;

    /// Puts text on the clipboard.
    fn copy(&mut self, text: &str) -> Result<(), String>;

    /// Edits text in the user's editor, with the terminal handed over to it
    /// for as long as it runs.
    fn edit(&mut self, text: &str) -> Result<String, String>;

    /// Stops the process as Ctrl+Z does in a shell, with the terminal
    /// restored while it is stopped, and returns once it is resumed with
    /// the TUI entered again. The caller then redraws everything.
    fn suspend(&mut self) -> Result<(), String>;
}

/// The real terminal.
pub struct TerminalHost {
    input: Option<InputThread>,
    editor: EditorCommand,
    synchronized: bool,
}

impl std::fmt::Debug for TerminalHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalHost")
            .field("synchronized", &self.synchronized)
            .finish_non_exhaustive()
    }
}

impl TerminalHost {
    /// A host for the terminal. `input` is paused while the editor runs, so
    /// the editor gets every key. `synchronized` wraps frames in mode 2026
    /// so the terminal shows each frame whole.
    pub fn new(input: Option<InputThread>, editor: EditorCommand, synchronized: bool) -> Self {
        TerminalHost {
            input,
            editor,
            synchronized,
        }
    }

    fn write(bytes: &[u8]) -> io::Result<()> {
        let mut out = io::stdout();
        out.write_all(bytes)?;
        out.flush()
    }
}

impl Host for TerminalHost {
    fn frame_begin(&mut self) {
        if self.synchronized {
            let mut out = io::stdout();
            // A failed write here only costs the tear-free frame.
            let _ = out.queue(BeginSynchronizedUpdate);
        }
    }

    fn frame_end(&mut self) {
        if self.synchronized {
            let mut out = io::stdout();
            let _ = out.queue(EndSynchronizedUpdate);
            let _ = out.flush();
        }
    }

    fn open_url(&mut self, url: &str) -> Result<(), String> {
        open::that_detached(url).map_err(|e| format!("cannot open the link: {e}"))
    }

    fn open_path(&mut self, path: &Path) -> Result<(), String> {
        open::that_detached(path).map_err(|e| format!("cannot open {}: {e}", path.display()))
    }

    fn copy(&mut self, text: &str) -> Result<(), String> {
        let seq = osc::osc52_copy(text).map_err(|e| e.to_string())?;
        Self::write(seq.as_bytes()).map_err(|e| format!("cannot write to the terminal: {e}"))
    }

    fn edit(&mut self, text: &str) -> Result<String, String> {
        if let Some(input) = &self.input
            && let Err(e) = input.pause()
        {
            // A late pause would otherwise leave the thread paused for good.
            // Resume while running is a no-op.
            input.resume();
            return Err(format!("cannot hand the terminal to the editor: {e}"));
        }
        let editor = self.editor.clone();
        let result = crate::terminal::suspend_for(|| editor::edit_text(&editor, text, ".md"));
        if let Some(input) = &self.input {
            input.resume();
        }
        match result {
            Ok(Ok(text)) => Ok(text),
            Ok(Err(e)) => Err(e.to_string()),
            Err(e) => Err(format!("cannot leave the terminal for the editor: {e}")),
        }
    }

    #[cfg(unix)]
    fn suspend(&mut self) -> Result<(), String> {
        // The shell owns the terminal while mejl is stopped, so nothing may
        // read it from here until `fg`.
        if let Some(input) = &self.input
            && let Err(e) = input.pause()
        {
            // As in `edit`: a late pause must not leave the thread paused.
            input.resume();
            return Err(format!("cannot suspend: {e}"));
        }
        let result = crate::terminal::suspend_process();
        if let Some(input) = &self.input {
            input.resume();
        }
        result.map_err(|e| format!("cannot suspend: {e}"))
    }

    #[cfg(not(unix))]
    fn suspend(&mut self) -> Result<(), String> {
        Err("suspending needs a Unix shell".into())
    }
}

/// A host that records what it was asked, for headless tests.
#[derive(Debug, Default, Clone)]
pub struct RecordingHost {
    /// Shared log of requests.
    pub log: std::sync::Arc<std::sync::Mutex<HostLog>>,
}

/// What a [`RecordingHost`] saw.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HostLog {
    /// URLs opened.
    pub urls: Vec<String>,
    /// Files opened.
    pub paths: Vec<std::path::PathBuf>,
    /// Text copied.
    pub copied: Vec<String>,
    /// Text handed to the editor.
    pub edited: Vec<String>,
    /// Times the app asked to be suspended.
    pub suspends: usize,
    /// Frames drawn.
    pub frames: usize,
}

impl RecordingHost {
    fn with<T>(&self, f: impl FnOnce(&mut HostLog) -> T) -> T {
        let mut log = self
            .log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut log)
    }

    /// A copy of the log.
    pub fn snapshot(&self) -> HostLog {
        self.with(|l| l.clone())
    }
}

impl Host for RecordingHost {
    fn frame_end(&mut self) {
        self.with(|l| l.frames += 1);
    }

    fn open_url(&mut self, url: &str) -> Result<(), String> {
        self.with(|l| l.urls.push(url.to_owned()));
        Ok(())
    }

    fn open_path(&mut self, path: &Path) -> Result<(), String> {
        self.with(|l| l.paths.push(path.to_owned()));
        Ok(())
    }

    fn copy(&mut self, text: &str) -> Result<(), String> {
        self.with(|l| l.copied.push(text.to_owned()));
        Ok(())
    }

    fn edit(&mut self, text: &str) -> Result<String, String> {
        self.with(|l| l.edited.push(text.to_owned()));
        Ok(format!("{text}\nEdited in a test editor."))
    }

    fn suspend(&mut self) -> Result<(), String> {
        self.with(|l| l.suspends += 1);
        Ok(())
    }
}
