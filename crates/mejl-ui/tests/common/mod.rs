//! Shared test harness: a fixed clock, the demo data, a shell that runs
//! commands synchronously, and frame capture with ratatui's TestBackend.

#![allow(dead_code)]

use std::sync::Arc;

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use jiff::Timestamp;
use mejl_ui::{
    AppState, Cmd, Event, ManualClock, MemoryData, Msg, UiConfig, draw, render_body, update,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

pub const NOW: &str = "2026-10-23T14:02:31Z";

pub fn now() -> Timestamp {
    NOW.parse().unwrap()
}

pub struct Harness {
    pub state: AppState,
    pub data: MemoryData,
    pub clock: Arc<ManualClock>,
    pub terminal: Terminal<TestBackend>,
    /// Every command the UI produced, in order.
    pub log: Vec<Cmd>,
}

impl Harness {
    pub fn demo(w: u16, h: u16) -> Harness {
        Harness::with(MemoryData::demo(now()), UiConfig::default(), w, h)
    }

    pub fn with(data: MemoryData, config: UiConfig, w: u16, h: u16) -> Harness {
        let clock = Arc::new(ManualClock::new(now()));
        let state = AppState::new(config, clock.clone(), (w, h));
        let terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut hs = Harness {
            state,
            data,
            clock,
            terminal,
            log: Vec::new(),
        };
        let cmds = hs.state.start(&hs.data);
        hs.run(cmds);
        hs.draw();
        hs
    }

    /// Plays the shell: runs local commands at once and feeds results back.
    pub fn run(&mut self, cmds: Vec<Cmd>) {
        let mut queue = cmds;
        let mut guard = 0;
        while !queue.is_empty() {
            guard += 1;
            assert!(guard < 50, "command loop does not settle: {queue:?}");
            let mut next = Vec::new();
            for cmd in queue {
                self.log.push(cmd.clone());
                let msg = match &cmd {
                    Cmd::Mutate(m) => Some(Msg::DataChanged(self.data.apply(m))),
                    Cmd::RenderBody(req) => {
                        use mejl_ui::MailData;
                        self.data
                            .message_body(&req.email_id)
                            .map(|b| Msg::BodyRendered {
                                email_id: req.email_id.clone(),
                                prefer_html: req.prefer_html,
                                body: Box::new(render_body(&b, req.width, req.prefer_html)),
                            })
                    }
                    _ => None,
                };
                if let Some(msg) = msg {
                    next.extend(update(&mut self.state, &self.data, Event::Msg(msg)));
                }
            }
            queue = next;
        }
    }

    pub fn event(&mut self, event: Event) -> Vec<Cmd> {
        let cmds = update(&mut self.state, &self.data, event);
        self.run(cmds.clone());
        self.draw();
        cmds
    }

    /// Feeds keys written in config syntax, separated by spaces.
    pub fn keys(&mut self, keys: &str) -> Vec<Cmd> {
        let mut all = Vec::new();
        for k in keys.split_whitespace() {
            all.extend(self.event(Event::Key(key_event(k))));
        }
        all
    }

    pub fn click(&mut self, x: u16, y: u16) -> Vec<Cmd> {
        let mut cmds = self.mouse(MouseEventKind::Down(MouseButton::Left), x, y);
        cmds.extend(self.mouse(MouseEventKind::Up(MouseButton::Left), x, y));
        cmds
    }

    pub fn mouse(&mut self, kind: MouseEventKind, x: u16, y: u16) -> Vec<Cmd> {
        self.event(Event::Mouse(MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }))
    }

    pub fn resize(&mut self, w: u16, h: u16) {
        self.terminal.backend_mut().resize(w, h);
        self.event(Event::Resize(w, h));
    }

    pub fn draw(&mut self) {
        let Harness {
            state,
            data,
            terminal,
            ..
        } = self;
        terminal.draw(|f| draw(state, data, f)).unwrap();
    }

    pub fn buffer(&self) -> &Buffer {
        self.terminal.backend().buffer()
    }

    /// The screen as text, one line per row, wide characters once.
    pub fn text(&self) -> String {
        buffer_text(self.buffer())
    }

    pub fn line(&self, y: u16) -> String {
        line_text(self.buffer(), y)
    }

    /// Finds the first cell of `needle` on screen.
    pub fn find(&self, needle: &str) -> Option<(u16, u16)> {
        let buf = self.buffer();
        for y in 0..buf.area.height {
            let line = line_cells(buf, y);
            let joined: String = line.iter().map(|(_, s)| s.as_str()).collect();
            if let Some(byte) = joined.find(needle) {
                // Map the byte offset back to a column.
                let mut acc = 0;
                for (x, s) in &line {
                    if acc == byte {
                        return Some((*x, y));
                    }
                    acc += s.len();
                }
            }
        }
        None
    }
}

fn line_cells(buf: &Buffer, y: u16) -> Vec<(u16, String)> {
    let mut out = Vec::new();
    let mut skip = 0;
    for x in 0..buf.area.width {
        if skip > 0 {
            skip -= 1;
            continue;
        }
        let sym = buf[(x, y)].symbol().to_owned();
        let w = mejl_ui::text::width(&sym);
        if w > 1 {
            skip = w - 1;
        }
        out.push((x, sym));
    }
    out
}

pub fn line_text(buf: &Buffer, y: u16) -> String {
    line_cells(buf, y).into_iter().map(|(_, s)| s).collect()
}

pub fn buffer_text(buf: &Buffer) -> String {
    (0..buf.area.height)
        .map(|y| line_text(buf, y))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn key_event(spec: &str) -> KeyEvent {
    let key: mejl_ui::Key = spec.parse().unwrap_or_else(|e| panic!("{spec}: {e}"));
    let mut mods = KeyModifiers::NONE;
    if key.ctrl {
        mods |= KeyModifiers::CONTROL;
    }
    if key.alt {
        mods |= KeyModifiers::ALT;
    }
    if key.shift {
        mods |= KeyModifiers::SHIFT;
    }
    use mejl_ui::keymap::KeyName as N;
    let code = match key.name {
        N::Char(c) => {
            if c.is_ascii_uppercase() {
                // Legacy terminals report uppercase letters with SHIFT.
                mods |= KeyModifiers::SHIFT;
            }
            KeyCode::Char(c)
        }
        N::Enter => KeyCode::Enter,
        N::Esc => KeyCode::Esc,
        N::Tab => KeyCode::Tab,
        N::BackTab => {
            mods |= KeyModifiers::SHIFT;
            KeyCode::BackTab
        }
        N::Backspace => KeyCode::Backspace,
        N::Delete => KeyCode::Delete,
        N::Insert => KeyCode::Insert,
        N::Up => KeyCode::Up,
        N::Down => KeyCode::Down,
        N::Left => KeyCode::Left,
        N::Right => KeyCode::Right,
        N::Home => KeyCode::Home,
        N::End => KeyCode::End,
        N::PageUp => KeyCode::PageUp,
        N::PageDown => KeyCode::PageDown,
        N::F(n) => KeyCode::F(n),
    };
    KeyEvent {
        code,
        modifiers: mods,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}
