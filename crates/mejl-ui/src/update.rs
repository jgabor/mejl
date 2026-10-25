//! `update`: events in, state changes and commands out.
//!
//! [`update`] never blocks and never does I/O. It reads local data through
//! [`MailData`] (bounded page reads only), changes [`AppState`], and returns
//! [`Cmd`]s for the shell. After every event it runs one refresh pass that
//! loads the visible list chunk, keeps the cursor visible, follows the
//! cursor with the reader, makes sure expanded bodies are rendered or
//! requested, and lays the reader document out again when needed.

mod compose;
mod keys;
mod macros;
mod masked;
mod overlays;
mod rules;
mod screens;
mod search;

use std::collections::HashMap;

use crossterm::event::{KeyEventKind, MouseButton, MouseEvent, MouseEventKind};
use jiff::{SignedDuration, Timestamp, Zoned};

use crate::action::Action;
use crate::body::BodyState;
use crate::data::{
    ComposeKind, EmailId, MailData, MailboxId, MailboxRole, MaskedEmailView, ThreadId,
};
use crate::dates::{self, DateGroup};
use crate::event::{
    Cmd, DataChange, Event, Keyword, Msg, Mutation, OutboxOutcome, Target, TargetIds, ToastLevel,
};
use crate::hit::{HitTarget, Pane};
use crate::keymap::{Binding, Context, Key, KeyName, Resolution};
use crate::layout::{LayoutMode, MailLayout, Splitter, ratio_for_drag, sidebar_for_drag};
use crate::picker::{PickItem, PickValue, Picker, PickerKind, QuitChoice};
use crate::reader::{self, DocInput, DocMode, ReaderDoc};
use crate::state::Screen;
use crate::state::{
    AppState, Confirm, ConfirmAction, Focus, Follow, HelpState, ListState, Overlay, ReaderState,
    SidebarItem, Toast, sidebar_items,
};

/// Largest `X` range selected row by row. Bigger ranges ask for `* a`.
const MAX_RANGE: usize = 5_000;
/// Largest explicit selection checked against the list after a change.
const MAX_PRUNE: usize = 1_000;

/// Applies one event and returns the commands it produced.
///
/// See the crate docs for the optimistic contract: after the shell runs a
/// [`Cmd::Mutate`] locally it must feed `Msg::DataChanged` back in before
/// drawing.
pub fn update(state: &mut AppState, data: &dyn MailData, event: Event) -> Vec<Cmd> {
    let mut cmds = Vec::new();
    state.announce = false;
    match event {
        Event::Key(k) => {
            if k.kind != KeyEventKind::Release
                && let Some(key) = Key::from_event(&k)
            {
                state.on_key(data, key, &mut cmds);
            }
        }
        Event::Mouse(m) => state.on_mouse(data, m, &mut cmds),
        Event::Paste(text) => state.on_paste(data, &text, &mut cmds),
        Event::Resize(w, h) => {
            state.size = (w, h);
            state.reader.dirty = true;
        }
        Event::Focus(focused) => state.terminal_focused = focused,
        Event::Msg(msg) => state.on_msg(data, msg, &mut cmds),
    }
    state.refresh(data, &mut cmds);
    cmds
}

/// A line of the visible list window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ListLine {
    /// A date group heading.
    Heading(DateGroup),
    /// A conversation row by index.
    Row(usize),
    /// The preview line under a row, in the relaxed density.
    Preview(usize),
}

/// The display lines for a window of the list starting at row `offset`.
/// A heading goes before the first row of each date group; the first
/// visible row only gets one when it starts its group.
pub(crate) fn list_lines(
    row_date: &dyn Fn(usize) -> Option<Timestamp>,
    now: &Zoned,
    offset: usize,
    total: usize,
    height: usize,
    density: crate::state::Density,
) -> Vec<ListLine> {
    use crate::state::Density;
    let mut out = Vec::with_capacity(height);
    let mut prev = if offset == 0 {
        None
    } else {
        row_date(offset - 1).map(|d| dates::group(d, now))
    };
    let mut i = offset;
    while out.len() < height && i < total {
        let group = row_date(i).map(|d| dates::group(d, now));
        if let Some(g) = group
            && Some(g) != prev
            && density != Density::Compact
        {
            out.push(ListLine::Heading(g));
            prev = Some(g);
            if out.len() == height {
                break;
            }
        }
        out.push(ListLine::Row(i));
        if density == Density::Relaxed && out.len() < height {
            out.push(ListLine::Preview(i));
        }
        i += 1;
    }
    out
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

impl AppState {
    pub(crate) fn now_zoned(&self) -> Zoned {
        self.clock.now().to_zoned(self.config.time_zone.clone())
    }

    pub(crate) fn notify(
        &mut self,
        text: impl Into<String>,
        level: ToastLevel,
        undo: bool,
        cmds: &mut Vec<Cmd>,
    ) {
        let now = self.clock.now();
        let expires = now
            .checked_add(
                SignedDuration::try_from(self.config.toast_time)
                    .unwrap_or(SignedDuration::from_secs(5)),
            )
            .unwrap_or(now);
        self.toast = Some(Toast {
            text: text.into(),
            level,
            undo,
            expires,
            sending: None,
        });
        cmds.push(Cmd::WakeAt(expires));
        self.announce = self.config.accessible;
    }

    fn list_height(&self, layout: &MailLayout) -> usize {
        layout
            .list
            .map(|r| r.height)
            .unwrap_or_else(|| layout.frame.height.saturating_sub(2))
            .max(1) as usize
    }

    fn reader_height(&self, layout: &MailLayout) -> usize {
        layout.reader.map(|r| r.height as usize).unwrap_or(0).max(1)
    }

    pub(crate) fn visible_lines(&self, height: usize) -> Vec<ListLine> {
        let now = self.now_zoned();
        let pages = &self.list.pages;
        list_lines(
            &|i| pages.row(i).map(|r| r.date),
            &now,
            self.list.offset,
            self.list.total,
            height,
            self.config.density,
        )
    }

    // ----- keys -----

    fn on_key(&mut self, data: &dyn MailData, key: Key, cmds: &mut Vec<Cmd>) {
        // Suspend works everywhere: before overlays, text inputs and
        // pending sequences get the key.
        if self.is_suspend_key(key) {
            self.pending.clear();
            cmds.push(Cmd::Suspend);
            return;
        }
        match self.overlays.last() {
            Some(Overlay::Confirm(_)) => {
                let Some(Overlay::Confirm(confirm)) = self.overlays.pop() else {
                    return;
                };
                if matches!(key.name, KeyName::Char('y' | 'Y')) && !key.ctrl && !key.alt {
                    self.confirmed(data, confirm.on_yes, cmds);
                } else {
                    self.notify("Cancelled", ToastLevel::Info, false, cmds);
                }
                return;
            }
            Some(Overlay::CaptureKey(_)) => {
                self.capture_key(key, cmds);
                return;
            }
            Some(Overlay::Perf) => {
                self.overlays.pop();
                return;
            }
            Some(Overlay::Help(_)) => {
                self.help_key(key);
                return;
            }
            Some(Overlay::Picker(_)) => {
                self.picker_key(data, key, cmds);
                return;
            }
            Some(Overlay::Prompt(_)) => {
                self.prompt_key(data, key, cmds);
                return;
            }
            None => {}
        }
        let used = match self.screen {
            Screen::Mail => false,
            Screen::Compose => self.compose_key(data, key, cmds),
            Screen::Search => self.search_key(data, key, cmds),
            Screen::Masked => self.masked_key(data, key, cmds),
            Screen::Rules => self.rules_key(data, key, cmds),
            Screen::Settings => false,
            Screen::Keys => self.keys_key(key, cmds),
        };
        if used {
            return;
        }
        if !self.pending.keys.is_empty() && key == Key::char('?') {
            // The which-key hint offers "? all keys": show help instead of
            // cancelling silently.
            self.pending.clear();
            self.apply(data, Action::Help, None, cmds);
            return;
        }
        let contexts = self.contexts();
        let res = self.config.keymap.feed(&mut self.pending, &contexts, key);
        match res {
            Resolution::Action { action, count } => self.apply(data, action, count, cmds),
            Resolution::Macro { id } => self.run_macro(data, id, cmds),
            _ => {}
        }
    }

    /// Whether `key` is bound to [`Action::Suspend`] in the global table.
    fn is_suspend_key(&self, key: Key) -> bool {
        let mut scratch = crate::keymap::Pending::default();
        matches!(
            self.config
                .keymap
                .feed(&mut scratch, &[Context::Global], key),
            Resolution::Action {
                action: Action::Suspend,
                ..
            }
        )
    }

    fn help_key(&mut self, key: Key) {
        let mut pending = crate::keymap::Pending::default();
        let res = self.config.keymap.feed(&mut pending, &[Context::Help], key);
        let Some(Overlay::Help(help)) = self.overlays.last_mut() else {
            return;
        };
        match res {
            Resolution::Action { action, .. } => match action {
                Action::Back | Action::Quit => {
                    self.overlays.pop();
                }
                Action::ScrollDown | Action::Next => help.scroll += 1,
                Action::ScrollUp | Action::Prev => help.scroll = help.scroll.saturating_sub(1),
                Action::PageDown | Action::HalfPageDown => help.scroll += 10,
                Action::PageUp | Action::HalfPageUp => help.scroll = help.scroll.saturating_sub(10),
                _ => {}
            },
            _ if key.name == KeyName::Backspace => {
                help.filter.pop();
                help.scroll = 0;
            }
            _ => {
                if let Some(c) = key.text() {
                    help.filter.push(c);
                    help.scroll = 0;
                }
            }
        }
    }

    fn on_paste(&mut self, data: &dyn MailData, text: &str, cmds: &mut Vec<Cmd>) {
        // Pasted text is never dispatched as keys; only text inputs take it.
        if self.overlay_paste(data, text, cmds) {
            return;
        }
        if !self.overlays.is_empty() {
            return;
        }
        match self.screen {
            Screen::Compose => self.compose_paste(data, text, cmds),
            Screen::Search => self.search_paste(data, text),
            Screen::Masked => self.masked_paste(text),
            Screen::Keys => self.keys_paste(text),
            Screen::Mail | Screen::Rules | Screen::Settings => {}
        }
    }

    fn confirmed(&mut self, data: &dyn MailData, action: ConfirmAction, cmds: &mut Vec<Cmd>) {
        match action {
            ConfirmAction::DeletePermanently(target) => {
                let n = self.target_count(&target);
                cmds.push(Cmd::Mutate(Mutation::DeletePermanently(target)));
                self.list.selection.clear();
                self.notify(
                    format!("Deleted {} permanently", plural(n, "conversation")),
                    ToastLevel::Success,
                    false,
                    cmds,
                );
            }
            ConfirmAction::DiscardDraft => self.discard_draft(cmds),
            ConfirmAction::SendAnyway => self.send(true, cmds),
            ConfirmAction::DeleteMasked { id } => {
                cmds.push(Cmd::MaskedSetState {
                    id,
                    state: crate::data::MaskedState::Deleted,
                });
                self.notify(
                    "Deleted the masked address. Mail to it now bounces.",
                    ToastLevel::Success,
                    false,
                    cmds,
                );
            }
            ConfirmAction::DeleteRule { id } => self.delete_rule(data, &id, cmds),
            ConfirmAction::DisableMasked { id } => {
                self.set_mask_state(data, &id, crate::data::MaskedState::Disabled, cmds);
            }
            ConfirmAction::DeletePreset { name } => self.delete_preset(name, cmds),
            ConfirmAction::DeleteMacro { id } => self.delete_macro(id, cmds),
        }
    }

    // ----- actions -----

    /// Runs an action as if its key was pressed.
    pub fn run_action(&mut self, data: &dyn MailData, action: Action) -> Vec<Cmd> {
        let mut cmds = Vec::new();
        self.apply(data, action, None, &mut cmds);
        self.refresh(data, &mut cmds);
        cmds
    }

    fn apply(
        &mut self,
        data: &dyn MailData,
        action: Action,
        count: Option<u32>,
        cmds: &mut Vec<Cmd>,
    ) {
        if action.is_repeatable() {
            self.last_action = Some((Binding::Action(action), count));
        }
        if self.compose_action(data, action, cmds)
            || self.search_action(data, action, cmds)
            || self.screen_action(data, action, count, cmds)
        {
            return;
        }
        let n = count.unwrap_or(1).max(1) as usize;
        let layout = self.layout();
        match action {
            Action::Help => {
                let context = self.contexts()[0];
                self.overlays.push(Overlay::Help(HelpState {
                    context,
                    filter: String::new(),
                    scroll: 0,
                }));
            }
            Action::Perf => self.overlays.push(Overlay::Perf),
            Action::Back => {
                self.back(cmds);
            }
            Action::Quit => {
                if !self.back(cmds) {
                    self.quit_or_ask(cmds);
                }
            }
            Action::QuitNow => {
                self.flush_compose(cmds);
                cmds.push(Cmd::Quit);
            }
            Action::Suspend => cmds.push(Cmd::Suspend),
            Action::Undo => {
                if !self.cancel_send(cmds) {
                    cmds.push(Cmd::Mutate(Mutation::Undo));
                    self.notify("Undid the last change", ToastLevel::Info, false, cmds);
                }
            }
            Action::Redo => {
                cmds.push(Cmd::Mutate(Mutation::Redo));
                self.notify("Redid the change", ToastLevel::Info, false, cmds);
            }
            Action::Refresh => {
                cmds.push(Cmd::RefreshNow);
                self.notify("Syncing now", ToastLevel::Info, false, cmds);
            }
            Action::Repeat => match self.last_action {
                Some((Binding::Action(a), c)) => self.apply(data, a, c, cmds),
                Some((Binding::Macro(id), _)) => self.run_macro(data, id, cmds),
                None => {}
            },
            Action::NextPane => self.cycle_pane(&layout, true),
            Action::PrevPane => self.cycle_pane(&layout, false),
            Action::ToggleSidebar => self.toggle_sidebar(&layout),
            Action::GoMailbox => self.open_sidebar(&layout),
            Action::GoInbox => self.go_role(MailboxRole::Inbox, cmds),
            Action::LeaveField | Action::NextField | Action::PrevField => {}
            Action::GoArchive => self.go_role(MailboxRole::Archive, cmds),
            Action::GoSent => self.go_role(MailboxRole::Sent, cmds),
            Action::GoDrafts => self.go_role(MailboxRole::Drafts, cmds),
            Action::GoTrash => self.go_role(MailboxRole::Trash, cmds),
            Action::GoSpam => self.go_role(MailboxRole::Junk, cmds),
            Action::GoTop => match self.focus {
                Focus::Sidebar => self.sidebar_move_to(0, true),
                _ => self.set_cursor(count.map_or(0, |c| (c as usize).saturating_sub(1))),
            },
            Action::GoBottom => match self.focus {
                Focus::Sidebar => self.sidebar_move_to(self.sidebar.len().saturating_sub(1), false),
                _ => {
                    let last = self.list.total.saturating_sub(1);
                    self.set_cursor(count.map_or(last, |c| (c as usize).saturating_sub(1)));
                }
            },
            Action::Next | Action::Prev => {
                let down = action == Action::Next;
                match self.focus {
                    Focus::Sidebar => {
                        for _ in 0..n {
                            self.sidebar_step(down);
                        }
                    }
                    _ => self.move_cursor(if down { n as isize } else { -(n as isize) }),
                }
            }
            Action::PageDown | Action::PageUp | Action::HalfPageDown | Action::HalfPageUp => {
                let down = matches!(action, Action::PageDown | Action::HalfPageDown);
                let half = matches!(action, Action::HalfPageDown | Action::HalfPageUp);
                if self.focus == Focus::Reader {
                    let h = self.reader_height(&layout);
                    let step = if half {
                        (h / 2).max(1)
                    } else {
                        h.saturating_sub(2).max(1)
                    } * n;
                    self.scroll_reader(
                        &layout,
                        if down {
                            step as isize
                        } else {
                            -(step as isize)
                        },
                    );
                } else {
                    let h = self.list_height(&layout);
                    let step = if half {
                        (h / 2).max(1)
                    } else {
                        h.saturating_sub(1).max(1)
                    } * n;
                    let delta = if down {
                        step as isize
                    } else {
                        -(step as isize)
                    };
                    self.list.offset = (self.list.offset as isize + delta).max(0) as usize;
                    self.move_cursor(delta);
                }
            }
            Action::ScrollDown => self.scroll_reader(&layout, n as isize),
            Action::ScrollUp => self.scroll_reader(&layout, -(n as isize)),
            Action::NextMessage | Action::PrevMessage => {
                let down = action == Action::NextMessage;
                for _ in 0..n {
                    self.step_message(down);
                }
            }
            Action::Open => self.open(data, &layout, cmds),
            Action::ToggleSelect => {
                if let Some(t) = self.cursor_thread() {
                    self.list.selection.toggle(&t);
                    self.list.anchor = Some(self.list.cursor);
                }
            }
            Action::SelectRange => self.select_range(data, cmds),
            Action::SelectAll => {
                self.list.selection.select_all();
                let total = self.list.total;
                self.notify(
                    format!("Selected all {}", plural(total, "conversation")),
                    ToastLevel::Info,
                    false,
                    cmds,
                );
            }
            Action::SelectNone => self.list.selection.clear(),
            Action::Archive | Action::ArchiveNext | Action::ArchivePrev => {
                self.archive(action, cmds);
            }
            Action::Trash => self.move_kind(MailboxRole::Trash, cmds),
            Action::ReportSpam => self.move_kind(MailboxRole::Junk, cmds),
            Action::DeletePermanently => self.delete_permanently(cmds),
            Action::TogglePin => {
                let pinned = self
                    .list
                    .pages
                    .row(self.list.cursor)
                    .is_some_and(|r| r.flagged);
                self.set_keyword(Keyword::Flagged, !pinned, cmds);
            }
            Action::Pin => self.set_keyword(Keyword::Flagged, true, cmds),
            Action::Unpin => self.set_keyword(Keyword::Flagged, false, cmds),
            Action::MarkRead => self.set_keyword(Keyword::Seen, true, cmds),
            Action::MarkUnread => self.set_keyword(Keyword::Seen, false, cmds),
            Action::Snooze => self.notify(
                "Snooze is web only: Fastmail offers no snooze to other apps yet",
                ToastLevel::Info,
                false,
                cmds,
            ),
            Action::Expand => {
                if let Some(id) = self.focused_email() {
                    if !self.reader.expanded.remove(&id) {
                        self.reader.expanded.insert(id);
                    }
                    self.reader.dirty = true;
                }
            }
            Action::ExpandAll => {
                if let Some(conv) = &self.reader.conversation {
                    let all = conv
                        .messages
                        .iter()
                        .all(|m| self.reader.expanded.contains(&m.email_id));
                    self.reader.expanded.clear();
                    if all {
                        if let Some(last) = conv.messages.last() {
                            self.reader.expanded.insert(last.email_id.clone());
                        }
                    } else {
                        self.reader
                            .expanded
                            .extend(conv.messages.iter().map(|m| m.email_id.clone()));
                    }
                    self.reader.dirty = true;
                }
            }
            Action::ToggleQuotes => {
                self.reader.quotes_shown = !self.reader.quotes_shown;
                self.reader.fold_overrides.clear();
                self.reader.dirty = true;
            }
            Action::ToggleHeaders => {
                self.reader.headers_full = !self.reader.headers_full;
                self.reader.dirty = true;
            }
            Action::OpenInBrowser => {
                if let Some(id) = self.focused_email() {
                    cmds.push(Cmd::OpenInBrowser(id));
                }
            }
            Action::Palette => self.open_palette(data),
            Action::Search => {
                let q = self
                    .results
                    .as_ref()
                    .map(|r| r.query.clone())
                    .unwrap_or_default();
                self.open_search(data, &q);
            }
            Action::Compose => self.open_compose(data, ComposeKind::New, Vec::new(), cmds),
            Action::Reply => self.open_compose(data, ComposeKind::Reply, Vec::new(), cmds),
            Action::ReplyAll => self.open_compose(data, ComposeKind::ReplyAll, Vec::new(), cmds),
            Action::Forward => self.open_compose(data, ComposeKind::Forward, Vec::new(), cmds),
            Action::MoveTo => self.open_mailbox_picker(data, false, cmds),
            Action::Label => self.open_mailbox_picker(data, true, cmds),
            Action::Mute => self.mute(cmds),
            Action::ToggleThreads => {
                self.reader.tree = !self.reader.tree;
                self.reader.dirty = true;
                let text = if self.reader.tree {
                    "Thread tree view"
                } else {
                    "Conversation view"
                };
                self.notify(text, ToastLevel::Info, false, cmds);
            }
            Action::ToggleHtml => self.toggle_html(data, cmds),
            Action::Links => self.open_links(cmds),
            Action::CopyMenu => self.open_copy_menu(cmds),
            Action::ActionsMenu => self.open_actions_menu(cmds),
            Action::NewRule => self.new_rule_from_message(data, cmds),
            Action::MaskActions => self.open_mask_menu(data, cmds),
            Action::GoRules => self.open_rules(data, None),
            Action::GoMasked => self.open_masked(data, None),
            Action::GoSettings => self.open_settings(),
            Action::EditKeys => self.open_keys(cmds),
            // Screen actions outside their screen do nothing.
            _ => {}
        }
    }

    /// Sends still in their undo send window.
    fn held_sends(&self) -> Vec<crate::data::DraftId> {
        let now = self.clock.now();
        self.sends
            .iter()
            .filter(|s| s.deadline > now)
            .map(|s| s.message.draft_id.clone())
            .collect()
    }

    /// Quits, or asks first while a sent message can still be cancelled.
    fn quit_or_ask(&mut self, cmds: &mut Vec<Cmd>) {
        let held = self.held_sends();
        if held.is_empty() {
            self.flush_compose(cmds);
            cmds.push(Cmd::Quit);
            return;
        }
        let (it, is) = if held.len() == 1 {
            ("it", "A sent message is")
        } else {
            ("them", "Sent messages are")
        };
        let items = vec![
            PickItem::new(
                format!("Send {it} now, then quit"),
                PickValue::Quit(QuitChoice::SendNow),
            )
            .key("enter"),
            PickItem::new(
                format!("Quit and keep {it} queued; mejl sends {it} when it starts again"),
                PickValue::Quit(QuitChoice::KeepQueued),
            ),
            PickItem::new(
                format!("Cancel the send and go back to {it}"),
                PickValue::Quit(QuitChoice::CancelSend),
            ),
            PickItem::new("Stay in mejl", PickValue::Quit(QuitChoice::Stay)).key("esc"),
        ];
        let mut p = Picker::new(PickerKind::Actions, "Quit mejl?", items);
        p.hint = "q".into();
        p.footer = vec![format!("{is} still in the undo send window.")];
        self.overlays.push(Overlay::Picker(Box::new(p)));
    }

    /// The answer to the quit question.
    pub(crate) fn quit_choice(&mut self, choice: QuitChoice, cmds: &mut Vec<Cmd>) {
        let held = self.held_sends();
        match choice {
            QuitChoice::SendNow => {
                self.flush_compose(cmds);
                cmds.push(Cmd::SendNowAndQuit(held));
                self.notify(
                    "Sending now; mejl quits once it is sent",
                    ToastLevel::Info,
                    false,
                    cmds,
                );
            }
            QuitChoice::KeepQueued => {
                self.flush_compose(cmds);
                cmds.push(Cmd::Quit);
            }
            QuitChoice::CancelSend => {
                for id in held {
                    cmds.push(Cmd::CancelSend(id));
                }
                self.notify("Cancelling the send", ToastLevel::Info, false, cmds);
            }
            QuitChoice::Stay => {}
        }
    }

    fn mute(&mut self, cmds: &mut Vec<Cmd>) {
        let Some((target, n)) = self.target() else {
            return;
        };
        cmds.push(Cmd::Mutate(Mutation::SetKeyword {
            target,
            keyword: Keyword::Muted,
            value: true,
        }));
        self.list.selection.clear();
        self.notify(
            format!(
                "Muted {}: replies skip the inbox",
                plural(n, "conversation")
            ),
            ToastLevel::Success,
            true,
            cmds,
        );
    }

    fn toggle_html(&mut self, data: &dyn MailData, cmds: &mut Vec<Cmd>) {
        let Some(id) = self.focused_email() else {
            return;
        };
        let has_html = data.message_body(&id).is_some_and(|b| b.html.is_some());
        if !has_html {
            self.notify(
                "This message has no HTML part",
                ToastLevel::Info,
                false,
                cmds,
            );
            return;
        }
        if !self.reader.html_flip.remove(&id) {
            self.reader.html_flip.insert(id.clone());
        }
        self.reader.expanded.insert(id.clone());
        self.reader.dirty = true;
        let html = self.config.prefer_html != self.reader.html_flip.contains(&id);
        self.notify(
            if html {
                "Showing the HTML part"
            } else {
                "Showing the plain text part"
            },
            ToastLevel::Info,
            false,
            cmds,
        );
    }

    /// Goes back one level. Returns false at the top level.
    fn back(&mut self, cmds: &mut Vec<Cmd>) -> bool {
        if !self.pending.is_empty() {
            self.pending.clear();
            return true;
        }
        if self.overlays.pop().is_some() {
            return true;
        }
        match self.screen {
            Screen::Mail => {}
            Screen::Compose => {
                self.close_compose(cmds);
                return true;
            }
            Screen::Search => {
                self.close_search();
                return true;
            }
            Screen::Masked | Screen::Rules | Screen::Settings => {
                self.screen = Screen::Mail;
                return true;
            }
            Screen::Keys => {
                self.screen = self.keys.as_ref().map_or(Screen::Mail, |k| k.from);
                return true;
            }
        }
        if self.sidebar_overlay {
            self.sidebar_overlay = false;
            self.focus = Focus::List;
            return true;
        }
        if self.reader_full {
            self.reader_full = false;
            self.focus = Focus::List;
            self.reader.dirty = true;
            return true;
        }
        if self.focus != Focus::List {
            self.focus = Focus::List;
            return true;
        }
        if !self.list.selection.is_empty(self.list.total) {
            self.list.selection.clear();
            return true;
        }
        self.close_results()
    }

    fn cycle_pane(&mut self, layout: &MailLayout, forward: bool) {
        let mut panes = Vec::new();
        if layout.sidebar.is_some() || layout.sidebar_overlay.is_some() {
            panes.push(Focus::Sidebar);
        }
        if layout.list.is_some() {
            panes.push(Focus::List);
        }
        if layout.reader.is_some() {
            panes.push(Focus::Reader);
        }
        if panes.len() < 2 {
            return;
        }
        let i = panes.iter().position(|p| *p == self.focus).unwrap_or(0);
        let next = if forward {
            (i + 1) % panes.len()
        } else {
            (i + panes.len() - 1) % panes.len()
        };
        self.focus = panes[next];
    }

    fn toggle_sidebar(&mut self, layout: &MailLayout) {
        if layout.mode == LayoutMode::Three && !self.reader_full {
            self.focus = if self.focus == Focus::Sidebar {
                Focus::List
            } else {
                Focus::Sidebar
            };
        } else {
            self.sidebar_overlay = !self.sidebar_overlay;
            self.focus = if self.sidebar_overlay {
                Focus::Sidebar
            } else {
                Focus::List
            };
            if self.sidebar_overlay {
                self.reader_full = false;
            }
        }
        self.sidebar_cursor_to_mailbox();
    }

    fn open_sidebar(&mut self, layout: &MailLayout) {
        if layout.mode == LayoutMode::Three && !self.reader_full {
            self.focus = Focus::Sidebar;
        } else {
            self.sidebar_overlay = true;
            self.reader_full = false;
            self.focus = Focus::Sidebar;
        }
        self.sidebar_cursor_to_mailbox();
    }

    fn sidebar_cursor_to_mailbox(&mut self) {
        if let Some(mb) = &self.mailbox
            && let Some(i) = self
                .sidebar
                .iter()
                .position(|it| matches!(it, SidebarItem::Mailbox { id, .. } if id == mb))
        {
            self.sidebar_cursor = i;
        }
    }

    fn sidebar_step(&mut self, down: bool) {
        let len = self.sidebar.len();
        let mut i = self.sidebar_cursor;
        loop {
            let next = if down { i + 1 } else { i.wrapping_sub(1) };
            if next >= len {
                return;
            }
            i = next;
            if self.sidebar[i].selectable() {
                self.sidebar_cursor = i;
                return;
            }
        }
    }

    fn sidebar_move_to(&mut self, i: usize, down: bool) {
        self.sidebar_cursor = i.min(self.sidebar.len().saturating_sub(1));
        if self
            .sidebar
            .get(self.sidebar_cursor)
            .is_some_and(|it| !it.selectable())
        {
            self.sidebar_step(down);
        }
    }

    fn activate_sidebar(&mut self, data: &dyn MailData, cmds: &mut Vec<Cmd>) {
        match self.sidebar.get(self.sidebar_cursor).cloned() {
            Some(SidebarItem::Mailbox { id, .. }) => {
                self.switch_mailbox(id);
                self.sidebar_overlay = false;
                self.focus = Focus::List;
            }
            Some(SidebarItem::SavedSearch { query, .. }) => {
                self.open_results_for(data, &query, cmds);
                self.focus = Focus::List;
            }
            Some(SidebarItem::Tool { action, .. }) => self.apply(data, action, None, cmds),
            Some(SidebarItem::Heading(_) | SidebarItem::Blank) | None => {}
        }
    }

    fn go_role(&mut self, role: MailboxRole, cmds: &mut Vec<Cmd>) {
        match self.mailboxes.iter().find(|m| m.role == Some(role)) {
            Some(m) => {
                let id = m.id.clone();
                self.leave_screen(cmds);
                self.switch_mailbox(id);
                self.sidebar_overlay = false;
                self.focus = Focus::List;
            }
            None => self.notify(
                format!("There is no {} mailbox", role.default_name()),
                ToastLevel::Warning,
                false,
                cmds,
            ),
        }
    }

    /// Shows another mailbox.
    pub(crate) fn switch_mailbox(&mut self, id: MailboxId) {
        let had_results = self.results.take().is_some();
        if self.mailbox.as_ref() == Some(&id) && !had_results {
            return;
        }
        self.mailbox = Some(id);
        self.list = ListState::default();
        self.reader = ReaderState {
            quotes_shown: self.reader.quotes_shown,
            dirty: true,
            ..ReaderState::default()
        };
        self.reader_full = false;
        self.sidebar_cursor_to_mailbox();
    }

    /// Leaves a non-mail screen for the mail screen, saving a draft if
    /// compose was open.
    pub(crate) fn leave_screen(&mut self, cmds: &mut Vec<Cmd>) {
        match self.screen {
            Screen::Mail => {}
            Screen::Compose => self.close_compose(cmds),
            Screen::Search => {
                self.search = None;
            }
            Screen::Masked | Screen::Rules | Screen::Settings | Screen::Keys => {}
        }
        self.screen = Screen::Mail;
    }

    /// The mailbox mutations act from: the viewed mailbox, or the mailbox a
    /// search result was found in.
    pub(crate) fn target_mailbox(&self) -> Option<MailboxId> {
        if self.results.is_some() {
            self.result_mailbox()
        } else {
            self.mailbox.clone()
        }
    }

    fn cursor_thread(&self) -> Option<ThreadId> {
        self.list
            .pages
            .row(self.list.cursor)
            .map(|r| r.thread_id.clone())
    }

    fn set_cursor(&mut self, index: usize) {
        let last = self.list.total.saturating_sub(1);
        let old = self.list.cursor;
        self.list.cursor = index.min(last);
        self.list.direction = if self.list.cursor >= old { 1 } else { -1 };
        self.list.follow = Follow::Cursor;
    }

    fn move_cursor(&mut self, delta: isize) {
        let target = (self.list.cursor as isize + delta).max(0) as usize;
        self.set_cursor(target);
    }

    fn scroll_reader(&mut self, layout: &MailLayout, delta: isize) {
        let h = self.reader_height(layout);
        let max = self.reader.doc.lines.len().saturating_sub(h);
        self.reader.scroll = ((self.reader.scroll as isize + delta).max(0) as usize).min(max);
    }

    pub(crate) fn focused_email(&self) -> Option<EmailId> {
        let conv = self.reader.conversation.as_ref()?;
        conv.messages
            .get(self.reader.focused)
            .or(conv.messages.last())
            .map(|m| m.email_id.clone())
    }

    fn step_message(&mut self, down: bool) {
        let Some(conv) = &self.reader.conversation else {
            return;
        };
        let len = conv.messages.len();
        if len == 0 {
            return;
        }
        let i = self.reader.focused.min(len - 1);
        let next = if down {
            (i + 1).min(len - 1)
        } else {
            i.saturating_sub(1)
        };
        self.reader.focused = next;
        let id = conv.messages[next].email_id.clone();
        self.reader.expanded.insert(id.clone());
        self.reader.dirty = true;
        self.reader.scroll_to = Some(id);
    }

    fn open(&mut self, data: &dyn MailData, layout: &MailLayout, cmds: &mut Vec<Cmd>) {
        match self.focus {
            Focus::Sidebar => self.activate_sidebar(data, cmds),
            Focus::Reader => {
                if !self.reader_full {
                    self.reader_full = true;
                    self.reader.dirty = true;
                }
            }
            Focus::List => {
                if self.resume_draft_row(data) {
                    return;
                }
                let Some(thread) = self.cursor_thread() else {
                    return;
                };
                let conv = if self.reader.thread.as_ref() == Some(&thread) {
                    self.reader.conversation.clone()
                } else {
                    data.conversation(&thread)
                };
                if let Some(conv) = conv {
                    let unread: Vec<EmailId> = conv
                        .messages
                        .iter()
                        .filter(|m| m.unread)
                        .map(|m| m.email_id.clone())
                        .collect();
                    if !unread.is_empty() {
                        let delay = SignedDuration::try_from(self.config.mark_read_delay)
                            .unwrap_or(SignedDuration::ZERO);
                        if delay.is_zero() {
                            cmds.push(Cmd::Mutate(Mutation::SetKeyword {
                                target: Target {
                                    ids: TargetIds::Emails(unread),
                                    from_mailbox: self.target_mailbox(),
                                },
                                keyword: Keyword::Seen,
                                value: true,
                            }));
                        } else {
                            let now = self.clock.now();
                            let at = now.checked_add(delay).unwrap_or(now);
                            self.mark_read_at = Some((thread.clone(), at));
                            cmds.push(Cmd::WakeAt(at));
                        }
                    }
                }
                if layout.mode != LayoutMode::Three {
                    self.reader_full = true;
                    self.reader.dirty = true;
                }
                self.focus = Focus::Reader;
            }
        }
    }

    fn select_range(&mut self, data: &dyn MailData, cmds: &mut Vec<Cmd>) {
        let Some(mailbox) = self.mailbox.clone() else {
            return;
        };
        let anchor = self.list.anchor.unwrap_or(self.list.cursor);
        let (lo, hi) = if anchor <= self.list.cursor {
            (anchor, self.list.cursor)
        } else {
            (self.list.cursor, anchor)
        };
        let len = hi - lo + 1;
        if len > MAX_RANGE {
            self.notify(
                format!("That range has {len} conversations; use * a to select all"),
                ToastLevel::Warning,
                false,
                cmds,
            );
            return;
        }
        let page = data.conversation_page(&mailbox, lo, len);
        for row in &page.items {
            self.list.selection.insert(&row.thread_id);
        }
        self.list.anchor = Some(self.list.cursor);
    }

    /// The conversations an action applies to: the selection, or the row
    /// under the cursor.
    pub(crate) fn target(&self) -> Option<(Target, usize)> {
        let sel = &self.list.selection;
        let total = self.list.total;
        let from_mailbox = self.target_mailbox();
        if !sel.is_empty(total) {
            let ids = match &self.results {
                None if sel.is_all() => TargetIds::AllInMailbox {
                    mailbox: from_mailbox.clone()?,
                    except: sel.ids().cloned().collect(),
                },
                // Select all in search results: every result row, as
                // `ids` holds the ones deselected since.
                Some(results) if sel.is_all() => TargetIds::Threads(
                    results
                        .rows
                        .iter()
                        .map(|r| r.row.thread_id.clone())
                        .filter(|t| sel.contains(t))
                        .collect(),
                ),
                _ => TargetIds::Threads(sel.ids().cloned().collect()),
            };
            return Some((Target { ids, from_mailbox }, sel.count(total)));
        }
        let thread = self.cursor_thread()?;
        Some((
            Target {
                ids: TargetIds::Threads(vec![thread]),
                from_mailbox,
            },
            1,
        ))
    }

    pub(crate) fn target_count(&self, target: &Target) -> usize {
        match &target.ids {
            TargetIds::Threads(t) => t.len(),
            TargetIds::Emails(e) => e.len(),
            TargetIds::AllInMailbox { except, .. } => self.list.total.saturating_sub(except.len()),
        }
    }

    fn mailbox_id(&self, role: MailboxRole) -> Option<MailboxId> {
        self.mailboxes
            .iter()
            .find(|m| m.role == Some(role))
            .map(|m| m.id.clone())
    }

    fn archive(&mut self, action: Action, cmds: &mut Vec<Cmd>) {
        if self.mailbox_role() == Some(MailboxRole::Archive) {
            self.notify("Already in Archive", ToastLevel::Info, false, cmds);
            return;
        }
        let Some((target, n)) = self.target() else {
            return;
        };
        cmds.push(Cmd::Mutate(Mutation::Archive(target)));
        self.list.selection.clear();
        if action == Action::ArchivePrev {
            self.move_cursor(-1);
        }
        self.notify(
            format!("Archived {}", plural(n, "conversation")),
            ToastLevel::Success,
            true,
            cmds,
        );
    }

    fn move_kind(&mut self, role: MailboxRole, cmds: &mut Vec<Cmd>) {
        if self.mailbox_role() == Some(role) {
            let text = match role {
                MailboxRole::Trash => "Already in Trash; # deletes permanently",
                _ => "Already in Spam",
            };
            self.notify(text, ToastLevel::Info, false, cmds);
            return;
        }
        let Some((target, n)) = self.target() else {
            return;
        };
        let (mutation, text) = match role {
            MailboxRole::Trash => (
                Mutation::Trash(target),
                format!("Moved {} to Trash", plural(n, "conversation")),
            ),
            _ => (
                Mutation::ReportSpam(target),
                format!("Reported {} as spam", plural(n, "conversation")),
            ),
        };
        cmds.push(Cmd::Mutate(mutation));
        self.list.selection.clear();
        self.notify(text, ToastLevel::Success, true, cmds);
    }

    fn delete_permanently(&mut self, cmds: &mut Vec<Cmd>) {
        if !matches!(
            self.mailbox_role(),
            Some(MailboxRole::Trash | MailboxRole::Junk)
        ) {
            self.notify(
                "Permanent delete works in Trash and Spam; d moves to Trash",
                ToastLevel::Info,
                false,
                cmds,
            );
            return;
        }
        let Some((target, n)) = self.target() else {
            return;
        };
        self.overlays.push(Overlay::Confirm(Confirm {
            prompt: format!(
                "Delete {} permanently? This cannot be undone.",
                plural(n, "conversation")
            ),
            on_yes: ConfirmAction::DeletePermanently(target),
        }));
    }

    fn set_keyword(&mut self, keyword: Keyword, value: bool, cmds: &mut Vec<Cmd>) {
        let selection_active = !self.list.selection.is_empty(self.list.total);
        let (target, n, unit) = if self.focus == Focus::Reader && !selection_active {
            let Some(id) = self.focused_email() else {
                return;
            };
            (
                Target {
                    ids: TargetIds::Emails(vec![id]),
                    from_mailbox: self.target_mailbox(),
                },
                1,
                "message",
            )
        } else {
            let Some((t, n)) = self.target() else {
                return;
            };
            (t, n, "conversation")
        };
        cmds.push(Cmd::Mutate(Mutation::SetKeyword {
            target,
            keyword,
            value,
        }));
        self.list.selection.clear();
        let verb = match (keyword, value) {
            (Keyword::Seen, true) => "Marked read",
            (Keyword::Seen, false) => "Marked unread",
            (Keyword::Flagged, true) => "Pinned",
            (Keyword::Flagged, false) => "Unpinned",
            (Keyword::Muted, true) => "Muted",
            (Keyword::Muted, false) => "Unmuted",
        };
        self.notify(
            format!("{verb}: {}", plural(n, unit)),
            ToastLevel::Success,
            true,
            cmds,
        );
    }

    // ----- mouse -----

    fn on_mouse(&mut self, data: &dyn MailData, m: MouseEvent, cmds: &mut Vec<Cmd>) {
        let (x, y) = (m.column, m.row);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => self.click(data, x, y, cmds),
            MouseEventKind::Down(MouseButton::Right) => {
                if !self.overlays.is_empty()
                    || !self.pending.is_empty()
                    || self.screen != Screen::Mail
                {
                    return;
                }
                let hit = self
                    .hits
                    .hits(x, y)
                    .find(|t| {
                        matches!(
                            t,
                            HitTarget::ListRow(_) | HitTarget::Pane(_) | HitTarget::Message(_)
                        )
                    })
                    .cloned();
                match hit {
                    Some(HitTarget::ListRow(i)) => {
                        self.focus = Focus::List;
                        self.set_cursor(i);
                        self.refresh(data, cmds);
                    }
                    Some(HitTarget::Message(id)) => {
                        self.focus = Focus::Reader;
                        if let Some(conv) = &self.reader.conversation
                            && let Some(i) = conv.messages.iter().position(|m| m.email_id == id)
                        {
                            self.reader.focused = i;
                        }
                    }
                    Some(HitTarget::Pane(Pane::Reader)) => self.focus = Focus::Reader,
                    Some(HitTarget::Pane(Pane::List)) => self.focus = Focus::List,
                    _ => return,
                }
                self.open_actions_menu(cmds);
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some(splitter) = self.drag else {
                    return;
                };
                let layout = self.layout();
                match splitter {
                    Splitter::ListReader => {
                        if let Some(r) = ratio_for_drag(&layout, x) {
                            self.panes.list_ratio = Some(r);
                        }
                    }
                    Splitter::SidebarList => {
                        if let Some(w) = sidebar_for_drag(&layout, x) {
                            self.panes.sidebar = w;
                        }
                    }
                }
                self.reader.dirty = true;
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.drag.take().is_some() {
                    cmds.push(Cmd::SaveLayout(self.panes));
                }
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let down = m.kind == MouseEventKind::ScrollDown;
                let step = self.config.wheel_lines.max(1) as isize;
                let delta = if down { step } else { -step };
                if let Some(Overlay::Help(help)) = self.overlays.last_mut() {
                    help.scroll = (help.scroll as isize + delta).max(0) as usize;
                    return;
                }
                if let Some(Overlay::Picker(p)) = self.overlays.last_mut() {
                    p.page(if down { 1 } else { -1 });
                    return;
                }
                if self.screen_wheel(down) {
                    return;
                }
                let pane = self.hits.hits(x, y).find_map(|t| match t {
                    HitTarget::Pane(p) => Some(*p),
                    _ => None,
                });
                match pane {
                    Some(Pane::List) => {
                        self.list.offset = (self.list.offset as isize + delta).max(0) as usize;
                        self.list.follow = Follow::Offset;
                    }
                    Some(Pane::Reader) => {
                        let layout = self.layout();
                        self.scroll_reader(&layout, delta);
                    }
                    Some(Pane::Sidebar) | None => {}
                }
            }
            _ => {}
        }
    }

    fn click(&mut self, data: &dyn MailData, x: u16, y: u16, cmds: &mut Vec<Cmd>) {
        let now = self.clock.now();
        let double = self.last_click.is_some_and(|(t, lx, ly)| {
            lx == x
                && ly == y
                && now.duration_since(t)
                    <= SignedDuration::try_from(self.config.double_click).unwrap_or_default()
        });
        self.last_click = if double { None } else { Some((now, x, y)) };
        let target = self.hits.hit(x, y).cloned();
        if !self.pending.is_empty() {
            self.pending.clear();
            match target {
                Some(HitTarget::Action(a)) => self.apply(data, a, None, cmds),
                Some(HitTarget::Macro(id)) => self.run_macro(data, id, cmds),
                _ => {}
            }
            return;
        }
        if let Some(Overlay::Confirm(_) | Overlay::Prompt(_) | Overlay::CaptureKey(_)) =
            self.overlays.last()
        {
            self.overlays.pop();
            self.notify("Cancelled", ToastLevel::Info, false, cmds);
            return;
        }
        let Some(target) = target else {
            return;
        };
        match target {
            HitTarget::Backdrop => {
                if !self.overlays.is_empty() {
                    self.overlays.pop();
                } else if self.sidebar_overlay {
                    self.sidebar_overlay = false;
                    self.focus = Focus::List;
                }
            }
            HitTarget::Popup => {}
            HitTarget::Pane(p) => {
                self.focus = match p {
                    Pane::Sidebar => Focus::Sidebar,
                    Pane::List => Focus::List,
                    Pane::Reader => Focus::Reader,
                };
            }
            HitTarget::ListRow(i) => {
                self.focus = Focus::List;
                self.set_cursor(i);
                if double {
                    // Load the row first so Open acts on it.
                    self.refresh(data, cmds);
                    let layout = self.layout();
                    self.open(data, &layout, cmds);
                }
            }
            HitTarget::ListMarker(i) => {
                self.focus = Focus::List;
                self.set_cursor(i);
                if let Some(row) = self.list.pages.row(i) {
                    let t = row.thread_id.clone();
                    self.list.selection.toggle(&t);
                    self.list.anchor = Some(i);
                }
            }
            HitTarget::SidebarRow(i) => {
                self.sidebar_cursor = i;
                self.activate_sidebar(data, cmds);
            }
            HitTarget::Splitter(s) => self.drag = Some(s),
            HitTarget::Scrollbar { pane, track } => {
                let span = track.height.saturating_sub(1).max(1) as f64;
                let f = (f64::from(y.saturating_sub(track.y)) / span).clamp(0.0, 1.0);
                match pane {
                    Pane::List => {
                        let last = self.list.total.saturating_sub(1);
                        self.set_cursor((f * last as f64).round() as usize);
                    }
                    Pane::Reader => {
                        let layout = self.layout();
                        let h = self.reader_height(&layout);
                        let max = self.reader.doc.lines.len().saturating_sub(h);
                        self.reader.scroll = (f * max as f64).round() as usize;
                    }
                    Pane::Sidebar => {}
                }
            }
            HitTarget::Action(a) => self.apply(data, a, None, cmds),
            HitTarget::Macro(id) => self.run_macro(data, id, cmds),
            HitTarget::Prefix(seq) => {
                self.pending.clear();
                self.pending.keys = seq.0;
            }
            HitTarget::Link(url) => cmds.push(Cmd::OpenUrl(url)),
            HitTarget::Fold { email_id, fold } => {
                let key = (email_id, fold);
                if !self.reader.fold_overrides.remove(&key) {
                    self.reader.fold_overrides.insert(key);
                }
                self.reader.dirty = true;
            }
            HitTarget::Attachment { email_id, index } => {
                if let Some(body) = data.message_body(&email_id)
                    && let Some(a) = body.attachments.get(index)
                {
                    cmds.push(Cmd::OpenAttachment {
                        email_id,
                        blob_id: a.blob_id.clone(),
                        name: a.name.clone(),
                    });
                }
            }
            HitTarget::Message(id) => {
                self.focus = Focus::Reader;
                if let Some(conv) = &self.reader.conversation
                    && let Some(i) = conv.messages.iter().position(|m| m.email_id == id)
                {
                    self.reader.focused = i;
                }
                if !self.reader.expanded.remove(&id) {
                    self.reader.expanded.insert(id);
                }
                self.reader.dirty = true;
            }
            HitTarget::MailboxTitle => {
                let layout = self.layout();
                self.toggle_sidebar(&layout);
            }
            HitTarget::Mailbox(id) => {
                self.switch_mailbox(id);
                self.sidebar_overlay = false;
                self.focus = Focus::List;
            }
            HitTarget::PickerRow(i) => {
                if let Some(Overlay::Picker(p)) = self.overlays.last_mut() {
                    let same = p.cursor == i;
                    p.cursor = i.min(p.shown.len().saturating_sub(1));
                    if same || double || p.kind != crate::picker::PickerKind::Label {
                        self.choose(data, cmds);
                    }
                }
            }
            HitTarget::Field(i) => self.click_field(data, i, cmds),
            HitTarget::Row(i) => self.click_row(data, i, double, cmds),
        }
    }

    // ----- messages -----

    fn on_msg(&mut self, data: &dyn MailData, msg: Msg, cmds: &mut Vec<Cmd>) {
        match msg {
            Msg::Sync(s) => {
                if s == crate::event::SyncStatus::Synced {
                    self.bodies.clear_failures();
                }
                self.sync = s;
            }
            Msg::Push(p) => self.push = p,
            Msg::DataChanged(change) => self.data_changed(data, &change),
            Msg::BodyFetched(id) => {
                self.bodies.fetched(&id);
                self.reader.dirty = true;
                self.compose_body_fetched(data, &id);
            }
            Msg::BodyFailed { email_id, error } => {
                self.bodies.fetch_failed(&email_id, error);
                self.reader.dirty = true;
            }
            Msg::BodyRendered {
                email_id,
                prefer_html,
                body,
            } => {
                self.bodies.rendered(&email_id, prefer_html, *body);
                self.reader.dirty = true;
            }
            Msg::Toast { text, level } => self.notify(text, level, false, cmds),
            Msg::Error(text) => self.notify(text, ToastLevel::Error, false, cmds),
            Msg::Outbox(report) => {
                if let OutboxOutcome::Failed { error } = report.outcome {
                    self.notify(
                        format!("{} failed: {error}", report.label),
                        ToastLevel::Error,
                        false,
                        cmds,
                    );
                }
            }
            Msg::Perf(sample) => self.perf.record(sample),
            Msg::Tick => {}
            Msg::DraftSaved { draft_id, error } => self.draft_saved(&draft_id, error, cmds),
            Msg::Send(report) => self.send_report(report, cmds),
            Msg::EditorDone { draft_id, result } => self.editor_done(&draft_id, result, cmds),
            Msg::Attached {
                draft_id,
                path,
                result,
            } => self.attached(&draft_id, &path, result, cmds),
            Msg::ServerSearch { query, result } => self.server_results(query, result, cmds),
            Msg::MaskedCreated { draft_id, result } => self.masked_created(draft_id, result, cmds),
            Msg::Config(config) => self.apply_config(*config),
            Msg::SettingRefused { error } => {
                if let Some(s) = self.settings.as_mut() {
                    s.saving = None;
                }
                self.keys_config_changed();
                self.notify(
                    format!("Not changed: {error}"),
                    ToastLevel::Warning,
                    false,
                    cmds,
                );
            }
        }
    }

    fn apply_config(&mut self, config: crate::state::UiConfig) {
        // A macro id is a place in the list, which a new or deleted macro
        // shifts: Repeat follows the macro by its name, or forgets it.
        if let Some((Binding::Macro(id), count)) = self.last_action {
            let name = self
                .config
                .macros
                .get(usize::from(id.0))
                .map(|m| m.id.clone());
            self.last_action = name
                .and_then(|n| config.macros.iter().position(|m| m.id == n))
                .and_then(|i| u16::try_from(i).ok())
                .map(|i| (Binding::Macro(crate::macros::MacroId(i)), count));
        }
        if let Some(s) = self.settings.as_mut() {
            s.saving = None;
        }
        if config.prefer_html != self.config.prefer_html {
            self.bodies.clear();
            self.reader.html_flip.clear();
        }
        self.config = config;
        self.pending.clear();
        self.reader.dirty = true;
        self.list.pages.invalidate();
        self.keys_config_changed();
    }

    fn data_changed(&mut self, data: &dyn MailData, change: &DataChange) {
        if change.mailboxes || change.all {
            self.mailboxes_dirty = true;
        }
        self.screens_data_changed(data, change);
        self.refresh_results(data);
        self.list.pages.invalidate();
        self.list.relocate = true;
        let affects_reader = change.all
            || self
                .reader
                .thread
                .as_ref()
                .is_some_and(|t| change.threads.contains(t))
            || self.reader.conversation.as_ref().is_some_and(|c| {
                c.messages
                    .iter()
                    .any(|m| change.emails.contains(&m.email_id))
            });
        if affects_reader && let Some(thread) = self.reader.thread.clone() {
            let old_len = self
                .reader
                .conversation
                .as_ref()
                .map_or(0, |c| c.messages.len());
            self.reader.conversation = data.conversation(&thread);
            if let Some(conv) = &self.reader.conversation
                && conv.messages.len() > old_len
                && let Some(last) = conv.messages.last()
            {
                self.reader.expanded.insert(last.email_id.clone());
            }
            self.reader.dirty = true;
        }
        if let Some(mailbox) = self.mailbox.clone()
            && self.results.is_none()
        {
            let sel = &mut self.list.selection;
            if !sel.is_all() && sel.ids().count() <= MAX_PRUNE {
                sel.retain(|t| data.conversation_index(&mailbox, t).is_some());
            }
        }
    }

    // ----- refresh -----

    pub(crate) fn refresh(&mut self, data: &dyn MailData, cmds: &mut Vec<Cmd>) {
        if self.mailboxes_dirty || self.mailboxes.is_empty() {
            self.mailboxes = data.mailboxes();
            self.sidebar = sidebar_items(&self.mailboxes, &data.saved_searches());
            self.identities = data
                .identities()
                .into_iter()
                .map(|i| i.email.to_lowercase())
                .collect();
            // Read the account with the mailboxes: on a first run the cache
            // is empty at the first frame, and the account only becomes
            // known once the session arrives with the first sync.
            let account = data.account();
            self.identities.push(account.email.to_lowercase());
            self.account = Some(account);
            self.mailboxes_dirty = false;
            if self
                .sidebar
                .get(self.sidebar_cursor)
                .is_none_or(|i| !i.selectable())
            {
                self.sidebar_move_to(self.sidebar_cursor, true);
            }
        }
        let valid = self
            .mailbox
            .as_ref()
            .is_some_and(|id| self.mailboxes.iter().any(|m| &m.id == id));
        if !valid {
            let pick = self
                .mailbox_id(MailboxRole::Inbox)
                .or_else(|| self.mailboxes.first().map(|m| m.id.clone()));
            self.mailbox = None;
            self.list = ListState::default();
            self.mailbox = pick;
            self.sidebar_cursor_to_mailbox();
        }
        self.autosave(cmds);
        self.mark_read_due(cmds);
        if self
            .toast
            .as_ref()
            .is_some_and(|t| t.expires <= self.clock.now())
        {
            self.toast = None;
        }
        if self.screen != Screen::Mail {
            // Other screens read only what they show; the list and reader
            // catch up when the mail screen is back.
            return;
        }
        let layout = self.layout();
        match self.focus {
            Focus::Sidebar if layout.sidebar.is_none() && layout.sidebar_overlay.is_none() => {
                self.focus = Focus::List;
            }
            Focus::Reader if layout.reader.is_none() => self.focus = Focus::List,
            _ => {}
        }
        if self.reader_full && !self.sidebar_overlay {
            self.focus = Focus::Reader;
        }
        self.refresh_list(data, &layout);
        self.prefetch(cmds);
        if layout.reader.is_some() {
            self.refresh_reader(data, &layout, cmds);
        }
    }

    fn mark_read_due(&mut self, cmds: &mut Vec<Cmd>) {
        let Some((thread, at)) = self.mark_read_at.clone() else {
            return;
        };
        if at > self.clock.now() {
            return;
        }
        self.mark_read_at = None;
        // Only if the conversation is still the one being read.
        if self.reader.thread.as_ref() != Some(&thread) || self.screen != Screen::Mail {
            return;
        }
        let unread: Vec<EmailId> = self
            .reader
            .conversation
            .iter()
            .flat_map(|c| c.messages.iter())
            .filter(|m| m.unread)
            .map(|m| m.email_id.clone())
            .collect();
        if !unread.is_empty() {
            cmds.push(Cmd::Mutate(Mutation::SetKeyword {
                target: Target {
                    ids: TargetIds::Emails(unread),
                    from_mailbox: self.target_mailbox(),
                },
                keyword: Keyword::Seen,
                value: true,
            }));
        }
    }

    fn refresh_list(&mut self, data: &dyn MailData, layout: &MailLayout) {
        let Some(mailbox) = self.mailbox.clone() else {
            self.list.total = 0;
            self.list.cursor_thread = None;
            return;
        };
        let height = self.list_height(layout);
        if self.list.relocate {
            self.list.relocate = false;
            let found = self.list.cursor_thread.clone().and_then(|t| {
                if self.list.pages.is_fixed() {
                    self.list.pages.fixed_index(&t)
                } else {
                    data.conversation_index(&mailbox, &t)
                }
            });
            if let Some(i) = found {
                let delta = i as isize - self.list.cursor as isize;
                self.list.cursor = i;
                // Keep the row where it was on screen, except at the
                // top of the list, where new mail should come into view.
                if self.list.offset > 0 {
                    self.list.offset = (self.list.offset as isize + delta).max(0) as usize;
                }
            }
        }
        self.list.pages.ensure(
            data,
            &mailbox,
            self.list.offset.saturating_sub(1),
            self.list.offset + height,
        );
        let total = self.list.pages.total().unwrap_or(0);
        self.list.total = total;
        if total == 0 {
            self.list.cursor = 0;
            self.list.offset = 0;
            self.list.cursor_thread = None;
            return;
        }
        self.list.cursor = self.list.cursor.min(total - 1);
        self.list.offset = self.list.offset.min(total - 1);
        match self.list.follow {
            Follow::Cursor => self.keep_cursor_visible(data, &mailbox, height),
            Follow::Offset => {
                self.load_window(data, &mailbox, height);
                let lines = self.visible_lines(height);
                let rows: Vec<usize> = lines
                    .iter()
                    .filter_map(|l| match l {
                        ListLine::Row(i) => Some(*i),
                        ListLine::Heading(_) | ListLine::Preview(_) => None,
                    })
                    .collect();
                if let (Some(&first), Some(&last)) = (rows.first(), rows.last()) {
                    self.list.cursor = self.list.cursor.clamp(first, last);
                }
                self.fill_bottom(data, &mailbox, height);
            }
        }
        self.load_window(data, &mailbox, height);
        self.list.cursor_thread = self
            .list
            .pages
            .row(self.list.cursor)
            .map(|r| r.thread_id.clone());
    }

    fn load_window(&mut self, data: &dyn MailData, mailbox: &MailboxId, height: usize) {
        let start = self.list.offset.saturating_sub(1);
        self.list
            .pages
            .ensure(data, mailbox, start, self.list.offset + height);
    }

    fn keep_cursor_visible(&mut self, data: &dyn MailData, mailbox: &MailboxId, height: usize) {
        if self.list.cursor < self.list.offset {
            self.list.offset = self.list.cursor;
        }
        // Each pass can only add date headings that were not counted yet,
        // and there are at most four, so this settles quickly.
        for _ in 0..8 {
            self.load_window(data, mailbox, height);
            let lines = self.visible_lines(height);
            let last = lines.iter().rev().find_map(|l| match l {
                ListLine::Row(i) => Some(*i),
                ListLine::Heading(_) | ListLine::Preview(_) => None,
            });
            match last {
                Some(last) if self.list.cursor > last => {
                    self.list.offset += self.list.cursor - last;
                }
                _ => break,
            }
        }
        self.fill_bottom(data, mailbox, height);
    }

    /// Scrolls up while the end of the list leaves empty lines below it.
    fn fill_bottom(&mut self, data: &dyn MailData, mailbox: &MailboxId, height: usize) {
        for _ in 0..height {
            if self.list.offset == 0 {
                break;
            }
            self.load_window(data, mailbox, height);
            if self.visible_lines(height).len() >= height {
                break;
            }
            self.list.offset -= 1;
            self.list.pages.ensure(
                data,
                mailbox,
                self.list.offset.saturating_sub(1),
                self.list.offset + 1,
            );
            let lines = self.visible_lines(height);
            let shows_cursor = lines.contains(&ListLine::Row(self.list.cursor));
            if !shows_cursor {
                self.list.offset += 1;
                break;
            }
        }
    }

    fn prefetch(&mut self, cmds: &mut Vec<Cmd>) {
        if self.list.last_prefetch == Some(self.list.cursor) || self.list.total == 0 {
            return;
        }
        self.list.last_prefetch = Some(self.list.cursor);
        let dir = if self.list.direction < 0 { -1isize } else { 1 };
        let mut ids = Vec::new();
        for k in 1..=self.config.prefetch as isize {
            let i = self.list.cursor as isize + dir * k;
            if i < 0 {
                break;
            }
            if let Some(row) = self.list.pages.row(i as usize) {
                ids.push(row.latest_email_id.clone());
            }
        }
        if !ids.is_empty() {
            cmds.push(Cmd::Prefetch(ids));
        }
    }

    fn refresh_reader(&mut self, data: &dyn MailData, layout: &MailLayout, cmds: &mut Vec<Cmd>) {
        let thread = self.list.cursor_thread.clone();
        if thread != self.reader.thread {
            let quotes = self.reader.quotes_shown;
            let tree = self.reader.tree;
            self.reader = ReaderState {
                quotes_shown: quotes,
                tree,
                dirty: true,
                ..ReaderState::default()
            };
            self.reader.conversation = thread.as_ref().and_then(|t| data.conversation(t));
            self.reader.thread = thread;
            if let Some(conv) = &self.reader.conversation {
                for m in &conv.messages {
                    if m.unread {
                        self.reader.expanded.insert(m.email_id.clone());
                    }
                }
                if let Some(last) = conv.messages.last() {
                    self.reader.expanded.insert(last.email_id.clone());
                }
                self.reader.focused = conv.messages.len().saturating_sub(1);
            }
        }
        let Some(conv) = self.reader.conversation.clone() else {
            if self.reader.doc.thread.is_some() || !self.reader.doc.lines.is_empty() {
                self.reader.doc = Default::default();
            }
            return;
        };
        let Some(area) = layout.reader else {
            return;
        };
        let mode = if self.reader_full {
            DocMode::Full {
                side: layout.side.is_some(),
            }
        } else {
            DocMode::Pane
        };
        let pane_w = area.width as usize;
        let bw = reader::body_width(mode, pane_w);
        let mut states: HashMap<EmailId, BodyState> = HashMap::new();
        for m in &conv.messages {
            if self.reader.expanded.contains(&m.email_id) {
                let html = self.config.prefer_html != self.reader.html_flip.contains(&m.email_id);
                let s = self.bodies.ensure(data, &m.email_id, bw, html, cmds);
                states.insert(m.email_id.clone(), s);
            }
        }
        let side = matches!(mode, DocMode::Full { side: true });
        let doc_side = !self.reader.doc.side.is_empty();
        let needs = self.reader.dirty
            || self.reader.doc.width != pane_w
            || self.reader.doc.thread.as_ref() != Some(&conv.thread_id)
            || side != doc_side
            || self.reader.doc.focus_marker != (self.focus == Focus::Reader);
        if !needs {
            return;
        }
        let now = self.now_zoned();
        let labels: Vec<String> = {
            let mut ids: Vec<&MailboxId> = Vec::new();
            for m in &conv.messages {
                for b in &m.mailboxes {
                    if Some(b) != self.mailbox.as_ref() && !ids.contains(&b) {
                        ids.push(b);
                    }
                }
            }
            self.sidebar
                .iter()
                .filter_map(|it| match it {
                    SidebarItem::Mailbox { id, name, .. } if ids.contains(&id) => {
                        let role = self
                            .mailboxes
                            .iter()
                            .find(|m| &m.id == id)
                            .and_then(|m| m.role);
                        role.is_none().then(|| name.clone())
                    }
                    _ => None,
                })
                .collect()
        };
        let mut masked: HashMap<String, MaskedEmailView> = HashMap::new();
        for m in &conv.messages {
            for p in m.to.iter().chain(&m.cc) {
                let key = p.email.to_lowercase();
                if !masked.contains_key(&key)
                    && let Some(v) = data.masked_for_address(&p.email)
                {
                    masked.insert(key, v);
                }
            }
        }
        let in_contacts = side
            && conv.messages.last().is_some_and(|m| {
                data.contacts_prefix(&m.from.email, 5).iter().any(|c| {
                    c.emails
                        .iter()
                        .any(|e| e.eq_ignore_ascii_case(&m.from.email))
                })
            });
        let keymap = &self.config.keymap;
        let contexts = [Context::Reader, Context::Global];
        let keys = |a: Action| keymap.keys_for(a, &contexts).map(|s| s.to_string());
        let input = DocInput {
            focused: self.reader.focused,
            focus_marker: self.focus == Focus::Reader,
            tree: self.reader.tree,
            conv: &conv,
            mode,
            width: pane_w,
            expanded: &self.reader.expanded,
            quotes_shown: self.reader.quotes_shown,
            fold_overrides: &self.reader.fold_overrides,
            headers_full: self.reader.headers_full,
            bodies: &states,
            now: &now,
            me: &self.identities,
            labels: &labels,
            masked: &masked,
            in_contacts,
            keys: &keys,
            glyphs: &self.config.theme.glyphs,
        };
        let doc: ReaderDoc = reader::build(&input);
        if let Some(id) = self.reader.scroll_to.take()
            && let Some((_, line)) = doc.message_lines.iter().find(|(m, _)| *m == id)
        {
            self.reader.scroll = *line;
        }
        self.reader.doc = std::sync::Arc::new(doc);
        self.reader.dirty = false;
        let h = self.reader_height(layout);
        let max = self.reader.doc.lines.len().saturating_sub(h);
        self.reader.scroll = self.reader.scroll.min(max);
    }
}
