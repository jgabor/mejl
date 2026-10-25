//! Drawing.
//!
//! [`render`] draws the whole frame from [`AppState`] and returns the
//! frame's [`HitMap`]; [`draw`] does the same and stores the map in the
//! state for the next mouse event. Rendering never changes state and never
//! reads more than the visible page from [`MailData`] (and only when the
//! state's cache does not already hold it).

mod compose;
mod overlays;
mod panes;
mod picker;
mod screens;
mod search;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::symbols::merge::MergeStrategy;

use crate::action::Action;
use crate::data::{MailData, MailboxRole};
use crate::dates;
use crate::event::{PushState, SyncStatus, ToastLevel};
use crate::hit::{HitMap, HitTarget};
use crate::keymap::{Context, KeySeq};
use crate::layout::{LayoutMode, MailLayout};
use crate::reader::DocSpan;
use crate::state::{AppState, Focus, Overlay, Screen};
use crate::text::{truncate, width};
use crate::theme::{StyleName, Theme};

/// What a frame produced besides pixels.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderOutput {
    /// Clickable regions.
    pub hits: HitMap,
    /// Where to park the terminal cursor for screen readers (accessible
    /// mode only).
    pub cursor: Option<Position>,
}

/// Draws a frame and stores its hit map in `state`.
pub fn draw(state: &mut AppState, data: &dyn MailData, frame: &mut Frame<'_>) {
    let out = render(state, data, frame);
    state.hits = out.hits;
}

/// Draws a frame.
pub fn render(state: &AppState, data: &dyn MailData, frame: &mut Frame<'_>) -> RenderOutput {
    let area = frame.area();
    let mut p = Painter {
        buf: frame.buffer_mut(),
        theme: &state.config.theme,
        hits: HitMap::new(),
        cursor: None,
        clip: area,
    };
    if MailLayout::too_small(area) {
        let msg = "mejl needs at least 30 by 8 cells";
        let y = area.y + area.height / 2;
        let x = area.x + area.width.saturating_sub(width(msg) as u16) / 2;
        let style = p.style(StyleName::Text);
        p.put(x, y, msg, style, area.right());
        return RenderOutput {
            hits: p.hits,
            cursor: None,
        };
    }
    // The layout comes from the state's size. The frame can differ when the
    // shell draws before the resize event arrives; the painter clips to it.
    let layout = state.layout();
    let mail = MailView {
        state,
        data,
        layout: &layout,
    };
    match state.screen {
        Screen::Mail => mail.draw(&mut p),
        Screen::Compose => mail.compose(&mut p),
        Screen::Search => mail.search(&mut p),
        Screen::Masked => mail.masked(&mut p),
        Screen::Rules => mail.rules(&mut p),
        Screen::Settings => mail.settings(&mut p),
    }
    mail.overlays(&mut p);
    if state.announce && state.toast().is_some() {
        // Accessible mode reads the line under the cursor, so a new
        // notice is announced by parking the cursor on it for a frame.
        p.cursor = Some(Position::new(layout.status.x + 1, layout.status.y));
    }
    let Painter { hits, cursor, .. } = p;
    let cursor = cursor.filter(|_| state.config.accessible);
    if let Some(pos) = cursor {
        frame.set_cursor_position(pos);
    }
    RenderOutput { hits, cursor }
}

/// Writes styled text into the buffer and records hit regions.
pub(crate) struct Painter<'a> {
    pub(crate) buf: &'a mut Buffer,
    pub(crate) theme: &'a Theme,
    pub(crate) hits: HitMap,
    pub(crate) cursor: Option<Position>,
    pub(crate) clip: Rect,
}

impl Painter<'_> {
    pub(crate) fn style(&self, name: StyleName) -> Style {
        self.theme.style(name)
    }

    fn inside(&self, x: u16, y: u16) -> bool {
        let a = self.buf.area.intersection(self.clip);
        x >= a.x && x < a.right() && y >= a.y && y < a.bottom()
    }

    /// Writes `text` from `x` up to (not including) `max_x`. Returns the
    /// column after the last cell written.
    pub(crate) fn put(&mut self, x: u16, y: u16, text: &str, style: Style, max_x: u16) -> u16 {
        let right = max_x.min(self.buf.area.right()).min(self.clip.right());
        if x >= right || !self.inside(x, y) {
            return x;
        }
        self.buf
            .set_stringn(x, y, text, usize::from(right - x), style)
            .0
    }

    /// Writes a span with its theme style and records its target.
    pub(crate) fn span(&mut self, x: u16, y: u16, span: &DocSpan, max_x: u16) -> u16 {
        let style = self.style(span.style).add_modifier(span.add);
        let end = self.put(x, y, &span.text, style, max_x);
        if let Some(t) = &span.target {
            self.hits
                .push(Rect::new(x, y, end.saturating_sub(x), 1), t.clone());
        }
        end
    }

    /// Writes spans left to right.
    pub(crate) fn spans(&mut self, mut x: u16, y: u16, spans: &[DocSpan], max_x: u16) -> u16 {
        for s in spans {
            x = self.span(x, y, s, max_x);
        }
        x
    }

    /// Patches a style over an area, keeping the text.
    pub(crate) fn patch(&mut self, area: Rect, style: Style) {
        let a = area.intersection(self.buf.area).intersection(self.clip);
        if !a.is_empty() {
            self.buf.set_style(a, style);
        }
    }

    /// Clears an area to blank cells with the default style.
    pub(crate) fn clear(&mut self, area: Rect) {
        let a = area.intersection(self.buf.area).intersection(self.clip);
        for y in a.y..a.bottom() {
            for x in a.x..a.right() {
                self.buf[(x, y)].reset();
            }
        }
    }

    /// Puts one line drawing symbol, merging it with what is there when
    /// the theme's lines allow it.
    pub(crate) fn line_cell(&mut self, x: u16, y: u16, symbol: &str, style: Style, merge: bool) {
        if !self.inside(x, y) {
            return;
        }
        let mergeable = self.theme.lines.mergeable;
        let cell = &mut self.buf[(x, y)];
        let existing_is_line = cell
            .symbol()
            .chars()
            .next()
            .is_some_and(|c| ('\u{2500}'..='\u{257F}').contains(&c));
        if merge && mergeable && existing_is_line {
            let merged = MergeStrategy::Exact.merge(cell.symbol(), symbol).to_owned();
            cell.set_symbol(&merged);
        } else {
            cell.set_symbol(symbol);
        }
        cell.set_style(style);
    }

    /// Draws a box outline.
    pub(crate) fn frame_box(&mut self, r: Rect, style: Style, merge: bool) {
        if r.width < 2 || r.height < 2 {
            return;
        }
        let l = self.theme.lines;
        let (right, bottom) = (r.right() - 1, r.bottom() - 1);
        for x in r.x + 1..right {
            self.line_cell(x, r.y, l.horizontal, style, merge);
            self.line_cell(x, bottom, l.horizontal, style, merge);
        }
        for y in r.y + 1..bottom {
            self.line_cell(r.x, y, l.vertical, style, merge);
            self.line_cell(right, y, l.vertical, style, merge);
        }
        self.line_cell(r.x, r.y, l.corners[0], style, merge);
        self.line_cell(right, r.y, l.corners[1], style, merge);
        self.line_cell(r.x, bottom, l.corners[2], style, merge);
        self.line_cell(right, bottom, l.corners[3], style, merge);
    }

    /// Draws a vertical separator inside a box, with tees at both ends.
    pub(crate) fn separator(&mut self, x: u16, top: u16, bottom: u16, style: Style) {
        let l = self.theme.lines;
        for y in top + 1..bottom {
            self.line_cell(x, y, l.vertical, style, false);
        }
        self.line_cell(x, top, l.tees[0], style, false);
        self.line_cell(x, bottom, l.tees[1], style, false);
    }

    /// Draws a horizontal separator inside a box, with tees at both ends.
    pub(crate) fn hseparator(&mut self, x1: u16, x2: u16, y: u16, style: Style) {
        let l = self.theme.lines;
        for x in x1 + 1..x2 {
            self.line_cell(x, y, l.horizontal, style, false);
        }
        self.line_cell(x1, y, l.tees[2], style, false);
        self.line_cell(x2, y, l.tees[3], style, false);
    }

    /// Writes ` title ` on a border.
    pub(crate) fn title(&mut self, x: u16, y: u16, text: &str, style: Style, max_x: u16) {
        let room = usize::from(max_x.saturating_sub(x));
        if room < 3 {
            return;
        }
        let (t, _) = truncate(text, room - 2, self.theme.glyphs.ellipsis);
        let end = self.put(x, y, " ", style, max_x);
        let end = self.put(end, y, &t, style, max_x);
        self.put(end, y, " ", style, max_x);
    }
}

/// The mail screen.
pub(crate) struct MailView<'a> {
    pub(crate) state: &'a AppState,
    pub(crate) data: &'a dyn MailData,
    pub(crate) layout: &'a MailLayout,
}

/// One hint bar entry.
#[derive(Clone)]
pub(crate) struct Hint {
    pub(crate) keys: String,
    pub(crate) label: &'static str,
    pub(crate) short: &'static str,
    pub(crate) target: HitTarget,
    pub(crate) important: bool,
}

impl Hint {
    /// A hint that is not tied to a binding.
    pub(crate) fn fixed(keys: &str, label: &'static str, target: HitTarget) -> Hint {
        Hint {
            keys: keys.to_owned(),
            label,
            short: label,
            target,
            important: true,
        }
    }
}

impl MailView<'_> {
    pub(crate) fn keys_for(&self, action: Action) -> Option<String> {
        self.state
            .config
            .keymap
            .keys_for(action, &self.state.contexts())
            .map(|s| s.to_string())
    }

    fn draw(&self, p: &mut Painter<'_>) {
        let s = self.state;
        let l = self.layout;
        self.top_line(p);
        let border = p.style(StyleName::Border);
        p.frame_box(l.frame, border, false);
        let (top, bottom) = (l.frame.y, l.frame.bottom().saturating_sub(1));
        for (x, _) in &l.separators {
            p.separator(*x, top, bottom, border);
            if let Some(Some(splitter)) =
                l.separators.iter().find(|(sx, _)| sx == x).map(|(_, s)| *s)
            {
                p.hits.push(
                    Rect::new(*x, top + 1, 1, bottom.saturating_sub(top + 1)),
                    HitTarget::Splitter(splitter),
                );
            }
        }
        if l.reader_full {
            self.full_reader(p);
        } else {
            if let Some(r) = l.sidebar {
                self.sidebar(p, r, s.focus == Focus::Sidebar);
            }
            if let Some(r) = l.list {
                self.list(p, r);
            }
            if let Some(r) = l.reader {
                self.reader_pane(p, r);
            }
        }
        if let Some(r) = l.sidebar_overlay {
            self.sidebar_overlay(p, r);
        }
        self.status_line(p, None, Vec::new());
        if let Some(r) = l.hints {
            self.hint_bar(p, r);
        }
    }

    /// Popups over any screen: which-key, help, perf and pickers.
    pub(crate) fn overlays(&self, p: &mut Painter<'_>) {
        let s = self.state;
        if !s.pending.keys.is_empty() && s.overlays.is_empty() {
            self.which_key(p);
        }
        match s.overlays.last() {
            Some(Overlay::Help(h)) => self.help(p, h),
            Some(Overlay::Perf) => self.perf(p),
            Some(Overlay::Picker(pk)) => self.picker(p, pk),
            Some(Overlay::Confirm(_) | Overlay::Prompt(_)) | None => {}
        }
    }

    /// The top line of a screen other than mail: brand, account, title,
    /// and text on the right.
    pub(crate) fn screen_top(&self, p: &mut Painter<'_>, title: &str, right: &str) {
        let s = self.state;
        let l = self.layout;
        let y = l.top.y;
        let edge = l.top.x + l.top.width.saturating_sub(1);
        let mut x = p.put(l.top.x, y, " mejl ", p.style(StyleName::Brand), edge);
        x = p.put(x, y, "  ", Style::new(), edge);
        if l.top.width >= crate::layout::TWO_PANE_MIN
            && let Some(acc) = &s.account
        {
            x = p.put(
                x,
                y,
                &crate::text::sanitize(&acc.email),
                p.style(StyleName::TopInfo),
                edge,
            );
            x = p.put(x, y, "   ", Style::new(), edge);
        }
        let rw = width(right) as u16;
        let rx = edge.saturating_sub(rw);
        let room = usize::from(rx.saturating_sub(x + 1));
        let (t, _) = truncate(title, room, s.config.theme.glyphs.ellipsis);
        p.put(
            x,
            y,
            &t,
            p.style(StyleName::TopMailbox),
            rx.saturating_sub(1),
        );
        p.put(rx, y, right, p.style(StyleName::TopInfo), edge);
    }

    fn mailbox_name(&self) -> String {
        if let Some(r) = &self.state.results {
            return format!("Search: {}", crate::text::sanitize(&r.query));
        }
        self.state
            .mailbox_view()
            .map(|m| {
                if m.name.trim().is_empty() {
                    m.role
                        .map(MailboxRole::default_name)
                        .unwrap_or("Mailbox")
                        .to_owned()
                } else {
                    crate::text::sanitize(&m.name).into_owned()
                }
            })
            .unwrap_or_else(|| "Mail".to_owned())
    }

    fn top_line(&self, p: &mut Painter<'_>) {
        let s = self.state;
        let l = self.layout;
        let y = l.top.y;
        let w = l.top.width;
        let right_edge = l.top.x + w.saturating_sub(1);
        let mut x = p.put(l.top.x, y, " mejl ", p.style(StyleName::Brand), right_edge);
        x = p.put(x, y, "  ", Style::new(), right_edge);
        let wide = w >= crate::layout::TWO_PANE_MIN;
        if wide && let Some(acc) = &s.account {
            x = p.put(
                x,
                y,
                &crate::text::sanitize(&acc.email),
                p.style(StyleName::TopInfo),
                right_edge,
            );
            x = p.put(x, y, "   ", Style::new(), right_edge);
        }
        let mb = s.mailbox_view();
        let unread = mb.map_or(0, |m| m.unread_threads);
        let total = s.list.total;
        let (right, right_target): (Vec<(String, StyleName)>, Option<HitTarget>) = if l.reader_full
        {
            let pos = if total > 0 {
                format!("{} of {total}", s.list.cursor + 1)
            } else {
                String::new()
            };
            (vec![(pos, StyleName::TopInfo)], None)
        } else {
            let help = s
                .config
                .keymap
                .keys_for(Action::Help, &[Context::Global])
                .map(|k| format!("{k} help"));
            let mut v = Vec::new();
            if wide {
                v.push((
                    format!("{} preset", s.config.keymap.preset_kind().name()),
                    StyleName::TopInfo,
                ));
                v.push(("   ".to_owned(), StyleName::TopInfo));
            }
            if let Some(h) = help {
                v.push((h, StyleName::TopInfo));
            }
            (v, Some(HitTarget::Action(Action::Help)))
        };
        let right_w: usize = right.iter().map(|(t, _)| width(t)).sum();
        let right_x = right_edge.saturating_sub(right_w as u16);
        let left_max = right_x.saturating_sub(1);
        if l.reader_full {
            let name = self.mailbox_name();
            x = p.put(x, y, &name, p.style(StyleName::TopInfo), left_max);
            x = p.put(
                x,
                y,
                &format!(" {} ", s.config.theme.glyphs.crumb),
                p.style(StyleName::TopInfo),
                left_max,
            );
            let subject = s
                .reader
                .conversation
                .as_ref()
                .map(|c| crate::text::sanitize(&c.subject).into_owned())
                .unwrap_or_default();
            let room = usize::from(left_max.saturating_sub(x));
            let (t, _) = truncate(&subject, room, s.config.theme.glyphs.ellipsis);
            p.put(x, y, &t, p.style(StyleName::TopMailbox), left_max);
        } else {
            let mut name = self.mailbox_name();
            if l.mode != LayoutMode::Three {
                name.push(' ');
                name.push_str(s.config.theme.glyphs.expanded);
            }
            let start = x;
            x = p.put(x, y, &name, p.style(StyleName::TopMailbox), left_max);
            p.hits
                .push(Rect::new(start, y, x - start, 1), HitTarget::MailboxTitle);
            let counts = if l.mode == LayoutMode::Three {
                format!("  {unread} unread, {}", plural(total, "conversation"))
            } else {
                format!("  {unread} unread")
            };
            p.put(x, y, &counts, p.style(StyleName::TopInfo), left_max);
        }
        let mut rx = right_x;
        for (t, st) in &right {
            rx = p.put(rx, y, t, p.style(*st), right_edge);
        }
        if let Some(t) = right_target {
            p.hits.push(Rect::new(right_x, y, right_w as u16, 1), t);
        }
    }

    fn sync_text(&self) -> (String, String, StyleName) {
        let g = &self.state.config.theme.glyphs;
        match (&self.state.sync, self.state.push) {
            (SyncStatus::Synced, PushState::Connected) => (
                g.ok.to_owned(),
                "synced via JMAP push".to_owned(),
                StyleName::StatusOk,
            ),
            (SyncStatus::Synced, PushState::Off) => (
                g.ok.to_owned(),
                "synced, push off".to_owned(),
                StyleName::StatusOk,
            ),
            (SyncStatus::Synced, PushState::Connecting) => (
                g.ok.to_owned(),
                "synced, connecting push".to_owned(),
                StyleName::StatusOk,
            ),
            (SyncStatus::Syncing, _) => {
                (g.ellipsis.to_owned(), "syncing".to_owned(), StyleName::Dim)
            }
            (SyncStatus::Starting, _) => (
                g.ellipsis.to_owned(),
                "starting, showing cached mail".to_owned(),
                StyleName::Dim,
            ),
            (SyncStatus::Offline { pending }, _) => {
                let text = if *pending == 0 {
                    "offline".to_owned()
                } else {
                    format!("offline, {} pending", plural(*pending, "change"))
                };
                (g.fail.to_owned(), text, StyleName::StatusWarning)
            }
            (SyncStatus::Failed { error }, _) => (
                "!".to_owned(),
                format!("sync failed: {}", crate::text::sanitize(error)),
                StyleName::StatusError,
            ),
        }
    }

    /// The status line. `left` replaces the mail screen's selection
    /// counter on other screens; `right` goes before the sync state.
    pub(crate) fn status_line(
        &self,
        p: &mut Painter<'_>,
        left: Option<Vec<DocSpan>>,
        extra_right: Vec<DocSpan>,
    ) {
        let s = self.state;
        let left = match s.overlays.last() {
            Some(Overlay::Picker(pk)) => Some(vec![DocSpan::new(
                self.picker_status(pk),
                StyleName::StatusText,
            )]),
            _ => left,
        };
        let r = self.layout.status;
        let y = r.y;
        let edge = r.x + r.width.saturating_sub(1);
        let wide = r.width >= crate::layout::TWO_PANE_MIN;
        let (glyph, sync, sync_style) = self.sync_text();
        let toast = s.toast();
        let selection = if s.screen == Screen::Mail {
            s.list.selection.count(s.list.total)
        } else {
            0
        };
        let confirm = match s.overlays.last() {
            Some(Overlay::Confirm(c)) => Some(c),
            _ => None,
        };
        let prompt = match s.overlays.last() {
            Some(Overlay::Prompt(pr)) => Some(pr),
            _ => None,
        };
        let countdown = if toast.is_none() { s.countdown() } else { None };
        let left_busy = confirm.is_some()
            || prompt.is_some()
            || toast.is_some()
            || countdown.is_some()
            || selection > 0
            || left.as_ref().is_some_and(|l| !l.is_empty());
        // Right side.
        let mut right: Vec<DocSpan> = Vec::new();
        if !s.pending.is_empty() {
            right.push(DocSpan::new("pending: ", StyleName::Dim));
            let mut keys: Vec<String> = Vec::new();
            if let Some(c) = s.pending.count {
                keys.push(c.to_string());
            }
            if !s.pending.keys.is_empty() {
                keys.push(KeySeq(s.pending.keys.clone()).to_string());
            }
            right.push(DocSpan::new(keys.join(" "), StyleName::HintKey));
        } else if wide {
            right.extend(extra_right);
            right.push(DocSpan::new(glyph.clone(), sync_style));
            right.push(DocSpan::new(format!(" {sync}"), StyleName::Dim));
            if s.clock_visible() {
                right.push(DocSpan::new(
                    format!("  {}", dates::clock(&s.now_zoned())),
                    StyleName::Dim,
                ));
            }
        } else if s.list.total > 0 && s.screen == Screen::Mail {
            right.push(DocSpan::new(
                format!("{} of {}", s.list.cursor + 1, s.list.total),
                StyleName::Dim,
            ));
        }
        let right_w: usize = right.iter().map(|sp| width(&sp.text)).sum();
        let right_x = edge.saturating_sub(right_w as u16);
        let left_max = right_x.saturating_sub(2);
        let mut x = r.x + 1;
        if let Some(pr) = prompt {
            x = p.put(
                x,
                y,
                &pr.label,
                p.style(StyleName::StatusWarning)
                    .add_modifier(ratatui::style::Modifier::BOLD),
                left_max,
            );
            x = p.put(x, y, " ", Style::new(), left_max);
            let room = usize::from(left_max.saturating_sub(x));
            let (text, cx) = pr.input.view(room.max(1));
            let start = x;
            p.put(x, y, &text, p.style(StyleName::Text), left_max);
            let cur = start + cx as u16;
            p.patch(
                Rect::new(cur, y, 1, 1),
                p.style(StyleName::Text)
                    .add_modifier(ratatui::style::Modifier::REVERSED),
            );
            p.cursor = Some(Position::new(cur, y));
        } else if let Some(c) = confirm {
            x = p.put(
                x,
                y,
                &c.prompt,
                p.style(StyleName::StatusWarning)
                    .add_modifier(ratatui::style::Modifier::BOLD),
                left_max,
            );
            x = p.put(x, y, "  ", Style::new(), left_max);
            x = p.put(x, y, "y", p.style(StyleName::HintKey), left_max);
            x = p.put(x, y, "/", p.style(StyleName::HintLabel), left_max);
            p.put(x, y, "N", p.style(StyleName::HintKey), left_max);
        } else if let Some(t) = toast {
            let style = match t.level {
                ToastLevel::Info => StyleName::StatusText,
                ToastLevel::Success => StyleName::StatusOk,
                ToastLevel::Warning => StyleName::StatusWarning,
                ToastLevel::Error => StyleName::StatusError,
            };
            let prefix = if t.level == ToastLevel::Error {
                "! "
            } else {
                ""
            };
            let text = match &t.sending {
                Some(_) => {
                    let left = t.expires.duration_since(s.clock.now()).as_secs_f64();
                    format!("Sending in {} s", left.ceil().max(0.0) as i64)
                }
                None => crate::text::sanitize(&t.text).into_owned(),
            };
            x = p.put(x, y, &format!("{prefix}{text}"), p.style(style), left_max);
            if t.undo
                && let Some(k) = self.keys_for(Action::Undo)
            {
                x = p.put(x, y, "  ", Style::new(), left_max);
                let start = x;
                x = p.put(x, y, &k, p.style(StyleName::HintKey), left_max);
                x = p.put(x, y, " undo", p.style(StyleName::HintLabel), left_max);
                p.hits.push(
                    Rect::new(start, y, x - start, 1),
                    HitTarget::Action(Action::Undo),
                );
            }
        } else if let Some((_, at)) = countdown {
            let left = at.duration_since(s.clock.now()).as_secs_f64();
            x = p.put(
                x,
                y,
                &format!("Sending in {} s", left.ceil().max(0.0) as i64),
                p.style(StyleName::StatusText),
                left_max,
            );
            if let Some(k) = self.keys_for(Action::Undo) {
                x = p.put(x, y, "  ", Style::new(), left_max);
                let start = x;
                x = p.put(x, y, &k, p.style(StyleName::HintKey), left_max);
                x = p.put(x, y, " undo", p.style(StyleName::HintLabel), left_max);
                p.hits.push(
                    Rect::new(start, y, x - start, 1),
                    HitTarget::Action(Action::Undo),
                );
            }
        } else if selection > 0 {
            x = p.put(
                x,
                y,
                &format!("selection: {selection}"),
                p.style(StyleName::StatusSelection),
                left_max,
            );
            x = p.put(x, y, "    ", Style::new(), left_max);
            let mut first = true;
            for (action, label) in [
                (Action::Archive, "archive"),
                (Action::Trash, "trash"),
                (Action::SelectNone, "clear"),
            ] {
                if let Some(k) = self.keys_for(action) {
                    if !first {
                        x = p.put(x, y, "  ", Style::new(), left_max);
                    }
                    first = false;
                    let start = x;
                    x = p.put(x, y, &k, p.style(StyleName::HintKey), left_max);
                    x = p.put(
                        x,
                        y,
                        &format!(" {label}"),
                        p.style(StyleName::HintLabel),
                        left_max,
                    );
                    p.hits
                        .push(Rect::new(start, y, x - start, 1), HitTarget::Action(action));
                }
            }
        } else if let Some(spans) = left {
            x = p.spans(x, y, &spans, left_max);
        }
        if !wide && !left_busy {
            x = p.put(x, y, &glyph, p.style(sync_style), left_max);
            p.put(x, y, &format!(" {sync}"), p.style(StyleName::Dim), left_max);
        }
        p.spans(right_x, y, &right, edge);
    }

    pub(crate) fn hints(&self) -> Vec<Hint> {
        let s = self.state;
        let keys = |a: Action| self.keys_for(a);
        let one = |a: Action, label: &'static str, short: &'static str, important: bool| {
            keys(a).map(|k| Hint {
                keys: k,
                label,
                short,
                target: HitTarget::Action(a),
                important,
            })
        };
        let pair = |a: Action,
                    b: Action,
                    label: &'static str,
                    short: &'static str,
                    important: bool| {
            match (keys(a), keys(b)) {
                (Some(x), Some(y)) => Some(Hint {
                    keys: format!("{x}/{y}"),
                    label,
                    short,
                    target: HitTarget::Action(a),
                    important,
                }),
                _ => None,
            }
        };
        let prefix = |k: &str, label: &'static str, important: bool| {
            let seq: KeySeq = k.parse().ok()?;
            let contexts = s.contexts();
            let known = !s.config.keymap.continuations(&seq.0, &contexts).is_empty();
            known.then(|| Hint {
                keys: seq.to_string(),
                label,
                short: label,
                target: HitTarget::Prefix(seq),
                important,
            })
        };
        if !s.pending.keys.is_empty() {
            let seq = KeySeq(s.pending.keys.clone());
            let label = s.config.keymap.prefix_label(&seq);
            let first = Hint {
                keys: seq.to_string(),
                label: match label {
                    "go to" => "go to: press a key",
                    "mark" => "mark: press a key",
                    "select" => "select: press a key",
                    _ => "press a key",
                },
                short: "press a key",
                target: HitTarget::Popup,
                important: true,
            };
            return vec![
                Some(first),
                Some(Hint {
                    keys: "Esc".into(),
                    label: "cancel",
                    short: "cancel",
                    target: HitTarget::Backdrop,
                    important: true,
                }),
                Some(Hint {
                    keys: "?".into(),
                    label: "all keys",
                    short: "all keys",
                    target: HitTarget::Action(Action::Help),
                    important: true,
                }),
            ]
            .into_iter()
            .flatten()
            .collect();
        }
        match s.overlays.last() {
            Some(Overlay::Help(_)) => {
                return vec![
                    Hint {
                        keys: "type".into(),
                        label: "to filter",
                        short: "filter",
                        target: HitTarget::Popup,
                        important: true,
                    },
                    Hint {
                        keys: "Up/Down".into(),
                        label: "scroll",
                        short: "scroll",
                        target: HitTarget::Popup,
                        important: true,
                    },
                    Hint {
                        keys: "Esc".into(),
                        label: "close",
                        short: "close",
                        target: HitTarget::Action(Action::Back),
                        important: true,
                    },
                ];
            }
            Some(Overlay::Perf) => {
                return vec![Hint {
                    keys: "any key".into(),
                    label: "closes",
                    short: "closes",
                    target: HitTarget::Backdrop,
                    important: true,
                }];
            }
            Some(Overlay::Confirm(_)) => {
                return vec![
                    Hint {
                        keys: "y".into(),
                        label: "confirm",
                        short: "yes",
                        target: HitTarget::Popup,
                        important: true,
                    },
                    Hint {
                        keys: "any other key".into(),
                        label: "cancels",
                        short: "no",
                        target: HitTarget::Popup,
                        important: true,
                    },
                ];
            }
            Some(Overlay::Picker(pk)) => return picker::hints(pk),
            Some(Overlay::Prompt(_)) => {
                return vec![
                    Hint::fixed("Enter", "confirm", HitTarget::Popup),
                    Hint::fixed("Esc", "cancel", HitTarget::Backdrop),
                ];
            }
            None => {}
        }
        match s.screen {
            Screen::Mail => {}
            Screen::Compose => return self.compose_hints(),
            Screen::Search => return self.search_hints(),
            Screen::Masked | Screen::Rules | Screen::Settings => return self.screen_hints(),
        }
        let v: Vec<Option<Hint>> = match s.focus {
            Focus::List => vec![
                pair(Action::Next, Action::Prev, "move", "move", true),
                one(Action::Open, "open", "open", true),
                one(Action::ToggleSelect, "select", "select", false),
                one(Action::Archive, "archive", "archive", true),
                one(Action::Trash, "trash", "trash", true),
                one(Action::TogglePin, "pin", "pin", false),
                one(Action::MoveTo, "move to", "move", false),
                one(Action::Reply, "reply", "reply", true),
                one(Action::Compose, "compose", "new", false),
                one(Action::Search, "search", "search", true),
                one(Action::Palette, "commands", "cmds", true),
                prefix("g", "go", true),
                one(Action::Undo, "undo", "undo", false),
                one(Action::Help, "help", "help", true),
            ],
            Focus::Reader => vec![
                pair(Action::Next, Action::Prev, "conversation", "conv", true),
                pair(
                    Action::NextMessage,
                    Action::PrevMessage,
                    "message",
                    "msg",
                    true,
                ),
                one(Action::ToggleQuotes, "quotes", "quotes", true),
                one(Action::Links, "links", "links", false),
                prefix("@", "masked", false),
                one(Action::Archive, "archive", "archive", true),
                one(Action::Trash, "trash", "trash", true),
                one(Action::Reply, "reply", "reply", true),
                one(Action::Back, "back", "back", true),
                one(Action::Help, "help", "help", true),
            ],
            Focus::Sidebar => vec![
                pair(Action::Next, Action::Prev, "move", "move", true),
                one(Action::Open, "open", "open", true),
                one(Action::NextPane, "next pane", "pane", true),
                one(Action::Back, "back", "back", true),
                one(Action::Help, "help", "help", true),
            ],
        };
        v.into_iter().flatten().collect()
    }

    pub(crate) fn hint_bar(&self, p: &mut Painter<'_>, r: Rect) {
        let mut hints = self.hints();
        let avail = usize::from(r.width.saturating_sub(1));
        let width_of = |hs: &[&Hint], short: bool| -> usize {
            hs.iter()
                .map(|h| width(&h.keys) + 1 + width(if short { h.short } else { h.label }))
                .sum::<usize>()
                + hs.len().saturating_sub(1) * 2
                + 1
        };
        // Full labels first, dropping the less important entries from the
        // end; then short labels; then important entries too.
        let all = std::mem::take(&mut hints);
        let mut short = true;
        'fit: for try_short in [false, true] {
            let mut hs: Vec<&Hint> = all.iter().collect();
            loop {
                if width_of(&hs, try_short) <= avail {
                    short = try_short;
                    hints = hs.into_iter().cloned().collect();
                    break 'fit;
                }
                match hs.iter().rposition(|h| !h.important) {
                    Some(i) => {
                        hs.remove(i);
                    }
                    None => break,
                }
            }
        }
        if hints.is_empty() {
            hints = all.into_iter().filter(|h| h.important).collect();
            while hints.len() > 1 && width_of(&hints.iter().collect::<Vec<_>>(), true) > avail {
                hints.pop();
            }
        }
        let mut x = r.x + 1;
        let max = r.x + r.width;
        for (i, h) in hints.iter().enumerate() {
            if i > 0 {
                x = p.put(x, r.y, "  ", Style::new(), max);
            }
            let start = x;
            x = p.put(x, r.y, &h.keys, p.style(StyleName::HintKey), max);
            let label = if short { h.short } else { h.label };
            x = p.put(
                x,
                r.y,
                &format!(" {label}"),
                p.style(StyleName::HintLabel),
                max,
            );
            p.hits
                .push(Rect::new(start, r.y, x - start, 1), h.target.clone());
        }
    }

    pub(crate) fn contexts(&self) -> Vec<Context> {
        self.state.contexts()
    }
}

pub(crate) fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::{ManualClock, MemoryData, UiConfig};

    #[test]
    fn render_without_a_cached_page_reads_only_the_window() {
        let now = "2026-10-23T14:02:31Z".parse().unwrap();
        let data = MemoryData::synthetic(100_000, now);
        let clock = Arc::new(ManualClock::new(now));
        let mut state = AppState::new(UiConfig::default(), clock, (160, 40));
        state.start(&data);
        state.list.offset = 50_000;
        state.list.cursor = 50_010;
        state.list.pages.invalidate();
        data.reset_stats();
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        terminal
            .draw(|f| {
                render(&state, &data, f);
            })
            .unwrap();
        let stats = data.stats();
        assert_eq!(stats.page_calls, 1, "{stats:?}");
        assert!(stats.max_limit <= 36, "{stats:?}");
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("50000"), "the window at the offset is drawn");
    }

    #[test]
    fn tiny_terminals_get_a_message() {
        let now = "2026-10-23T14:02:31Z".parse().unwrap();
        let data = MemoryData::demo(now);
        let clock = Arc::new(ManualClock::new(now));
        for (w, h) in [(0u16, 0u16), (1, 1), (29, 40), (160, 7)] {
            let mut state = AppState::new(UiConfig::default(), clock.clone(), (w, h));
            state.start(&data);
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| draw(&mut state, &data, f)).unwrap();
            assert!(state.hit_map().is_empty());
        }
    }
}
