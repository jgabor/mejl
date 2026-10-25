//! The sidebar, the conversation list and the reader.

use ratatui::layout::{Position, Rect};

use super::{MailView, Painter};
use crate::data::{ConversationRow, Page};
use crate::dates::{self, LIST_DATE_WIDTH};
use crate::hit::{HitTarget, Pane};
use crate::layout::LayoutMode;
use crate::reader::DocLine;
use crate::state::{Focus, SidebarItem};
use crate::text::{fit, rfit, sanitize, width};
use crate::theme::StyleName;
use crate::update::ListLine;

/// Minimum preview cells worth showing after a subject.
const MIN_PREVIEW: usize = 12;
/// Count column in the sidebar.
const COUNT_W: usize = 4;

impl MailView<'_> {
    pub(super) fn sidebar(&self, p: &mut Painter<'_>, r: Rect, focused: bool) {
        let s = self.state;
        let g = &s.config.theme.glyphs;
        let ell = g.ellipsis;
        p.hits.push(r, HitTarget::Pane(Pane::Sidebar));
        let title_style = if focused {
            StyleName::PaneTitleFocused
        } else {
            StyleName::PaneTitle
        };
        if r.y > 0 && r.x > 0 {
            p.title(
                r.x + 1,
                r.y - 1,
                "Mailboxes",
                p.style(title_style),
                r.right(),
            );
        }
        let h = usize::from(r.height);
        let w = usize::from(r.width);
        if w < COUNT_W + 4 {
            return;
        }
        let count_x = r.x + (w - COUNT_W - 1) as u16;
        let top = s.sidebar_cursor.saturating_sub(h.saturating_sub(1));
        let contexts = [crate::keymap::Context::Global];
        for (row, (i, item)) in s.sidebar.iter().enumerate().skip(top).take(h).enumerate() {
            let y = r.y + row as u16;
            let x = r.x;
            let line = Rect::new(x, y, r.width, 1);
            let active =
                matches!(item, SidebarItem::Mailbox { id, .. } if Some(id) == s.mailbox.as_ref());
            let name_w = w.saturating_sub(COUNT_W + 2);
            match item {
                SidebarItem::Blank => {}
                SidebarItem::Heading(h) => {
                    p.put(
                        x + 1,
                        y,
                        &fit(h, w.saturating_sub(1), ell),
                        p.style(StyleName::SidebarHeading),
                        r.right(),
                    );
                }
                SidebarItem::Mailbox {
                    name, depth, count, ..
                } => {
                    let label = format!(
                        "{}{}",
                        "  ".repeat(usize::from(*depth).min(8)),
                        sanitize(name)
                    );
                    let count = count.map(|c| c.to_string()).unwrap_or_default();
                    if active {
                        let st = p.style(StyleName::SidebarActive);
                        p.put(
                            x,
                            y,
                            g.collapsed,
                            p.style(StyleName::ReaderMarker),
                            r.right(),
                        );
                        p.put(x + 1, y, &fit(&label, name_w, ell), st, r.right());
                        p.put(count_x, y, &rfit(&count, COUNT_W, ell), st, r.right());
                    } else {
                        p.put(
                            x + 1,
                            y,
                            &fit(&label, name_w, ell),
                            p.style(StyleName::SidebarItem),
                            r.right(),
                        );
                        p.put(
                            count_x,
                            y,
                            &rfit(&count, COUNT_W, ell),
                            p.style(StyleName::SidebarCount),
                            r.right(),
                        );
                    }
                }
                SidebarItem::SavedSearch { name, unread, .. } => {
                    let count = unread.map(|c| c.to_string()).unwrap_or_default();
                    p.put(
                        x + 1,
                        y,
                        &fit(&sanitize(name), name_w, ell),
                        p.style(StyleName::SidebarItem),
                        r.right(),
                    );
                    p.put(
                        count_x,
                        y,
                        &rfit(&count, COUNT_W, ell),
                        p.style(StyleName::SidebarCount),
                        r.right(),
                    );
                }
                SidebarItem::Tool { name, action } => {
                    let key = s
                        .config
                        .keymap
                        .keys_for(*action, &contexts)
                        .map(|k| k.to_string())
                        .unwrap_or_default();
                    p.put(
                        x + 1,
                        y,
                        &fit(name, name_w, ell),
                        p.style(StyleName::SidebarItem),
                        r.right(),
                    );
                    p.put(
                        count_x,
                        y,
                        &rfit(&key, COUNT_W, ell),
                        p.style(StyleName::HintKey),
                        r.right(),
                    );
                }
            }
            if item.selectable() {
                p.hits.push(line, HitTarget::SidebarRow(i));
            }
            let cursor_here = if focused {
                i == s.sidebar_cursor
            } else {
                active
            };
            if cursor_here {
                p.patch(line, p.style(StyleName::SidebarCursor));
                if focused {
                    p.cursor = Some(Position::new(x, y));
                }
            }
        }
    }

    /// Rows for the visible window: from the state's cache, or read once
    /// from the data when the cache does not cover the window (a draw
    /// before the first update).
    fn window_rows(&self, height: usize) -> (Vec<ListLine>, Option<Page<ConversationRow>>) {
        let s = self.state;
        let offset = s.list.offset;
        let covered = (offset.saturating_sub(1)..(offset + height).min(s.list.total))
            .all(|i| s.list.pages.row(i).is_some());
        let fallback = if covered {
            None
        } else {
            s.mailbox.as_ref().map(|m| {
                let start = offset.saturating_sub(1);
                self.data.conversation_page(m, start, height + 1)
            })
        };
        let total = fallback.as_ref().map_or(s.list.total, |p| p.total);
        let now = s.now_zoned();
        let row = |i: usize| -> Option<jiff::Timestamp> {
            match &fallback {
                Some(page) => page.items.get(i.checked_sub(page.offset)?).map(|r| r.date),
                None => s.list.pages.row(i).map(|r| r.date),
            }
        };
        let lines = crate::update::list_lines(&row, &now, offset, total, height, s.config.density);
        (lines, fallback)
    }

    pub(super) fn list(&self, p: &mut Painter<'_>, r: Rect) {
        let s = self.state;
        let l = self.layout;
        let focused = s.focus == Focus::List;
        p.hits.push(r, HitTarget::Pane(Pane::List));
        let title_style = if focused || l.mode == LayoutMode::Stack {
            StyleName::PaneTitleFocused
        } else {
            StyleName::PaneTitle
        };
        let mut title = self.mailbox_name();
        if l.mode == LayoutMode::Two {
            let unread = s.mailbox_view().map_or(0, |m| m.unread_threads);
            title.push_str(&format!("  {unread} unread"));
        }
        if r.y > 0 && r.x > 0 {
            p.title(r.x + 1, r.y - 1, &title, p.style(title_style), r.right());
        }
        let h = usize::from(r.height);
        let (lines, fallback) = self.window_rows(h);
        if lines.is_empty() {
            let msg = if s.mailbox.is_none() {
                "No mailboxes yet"
            } else {
                "No conversations here"
            };
            p.put(r.x + 1, r.y, msg, p.style(StyleName::Dim), r.right());
        }
        let now = s.now_zoned();
        let show_sel = !s.list.selection.is_empty(s.list.total);
        let opts = RowOpts {
            selection_column: show_sel,
            preview: l.mode == LayoutMode::Three
                && s.config.density != crate::state::Density::Relaxed,
            sender_w: if r.width >= 60 { 20 } else { 16 },
        };
        let mut rows_shown = 0usize;
        for (k, line) in lines.iter().enumerate() {
            let y = r.y + k as u16;
            match *line {
                ListLine::Heading(g) => {
                    p.put(
                        r.x + 1,
                        y,
                        g.label(),
                        p.style(StyleName::ListGroup),
                        r.right(),
                    );
                }
                ListLine::Preview(i) => {
                    let row = match &fallback {
                        Some(page) => i.checked_sub(page.offset).and_then(|j| page.items.get(j)),
                        None => s.list.pages.row(i),
                    };
                    let line_rect = Rect::new(r.x, y, r.width, 1);
                    if let Some(row) = row {
                        let ell = s.config.theme.glyphs.ellipsis;
                        let w = usize::from(r.width.saturating_sub(6));
                        p.put(
                            r.x + 5,
                            y,
                            &fit(&sanitize(&row.preview), w, ell),
                            p.style(StyleName::ListPreview),
                            r.right(),
                        );
                    }
                    p.hits.push(line_rect, HitTarget::ListRow(i));
                    if i == s.list.cursor {
                        p.patch(line_rect, p.style(StyleName::ListCursor));
                    }
                }
                ListLine::Row(i) => {
                    rows_shown += 1;
                    let row = match &fallback {
                        Some(page) => i.checked_sub(page.offset).and_then(|j| page.items.get(j)),
                        None => s.list.pages.row(i),
                    };
                    let line_rect = Rect::new(r.x, y, r.width, 1);
                    match row {
                        Some(row) => self.list_row(p, line_rect, row, &opts, &now),
                        None => {
                            p.put(
                                r.x + 1,
                                y,
                                s.config.theme.glyphs.ellipsis,
                                p.style(StyleName::ListSkeleton),
                                r.right(),
                            );
                        }
                    }
                    p.hits.push(line_rect, HitTarget::ListRow(i));
                    p.hits.push(
                        Rect::new(r.x, y, 2.min(r.width), 1),
                        HitTarget::ListMarker(i),
                    );
                    if i == s.list.cursor {
                        p.patch(line_rect, p.style(StyleName::ListCursor));
                        if focused {
                            p.cursor = Some(Position::new(r.x, y));
                        }
                    }
                }
            }
        }
        // Scrollbar thumb on the border right of the list.
        if let Some(sx) = l.list_scrollbar {
            let total = s.list.total;
            if total > rows_shown && h > 0 {
                let track = h;
                let size = ((track * rows_shown) / total).clamp(1, track);
                let max_offset = total.saturating_sub(rows_shown).max(1);
                let pos = ((track - size) * s.list.offset.min(max_offset)) / max_offset;
                let thumb = p.style(StyleName::ScrollThumb);
                for k in 0..size {
                    p.put(
                        sx,
                        r.y + (pos + k) as u16,
                        s.config.theme.glyphs.thumb,
                        thumb,
                        sx + 1,
                    );
                }
                if l.mode == LayoutMode::Stack {
                    let track_rect = Rect::new(sx, r.y, 1, r.height);
                    p.hits.push(
                        track_rect,
                        HitTarget::Scrollbar {
                            pane: Pane::List,
                            track: track_rect,
                        },
                    );
                }
            }
        }
    }

    fn list_row(
        &self,
        p: &mut Painter<'_>,
        r: Rect,
        row: &ConversationRow,
        o: &RowOpts,
        now: &jiff::Zoned,
    ) {
        let s = self.state;
        let g = &s.config.theme.glyphs;
        let ell = g.ellipsis;
        let lim = r.right();
        let y = r.y;
        let mut cx = r.x + 1;
        if o.selection_column {
            let sel = s.list.selection.contains(&row.thread_id);
            cx = p.put(
                cx,
                y,
                if sel { g.selected } else { " " },
                p.style(StyleName::ListSelectedMarker),
                lim,
            );
        }
        cx = p.put(
            cx,
            y,
            if row.unread { g.unread } else { " " },
            p.style(StyleName::ListUnreadMarker),
            lim,
        );
        cx = p.put(
            cx,
            y,
            if row.flagged { g.pinned } else { " " },
            p.style(StyleName::ListPinnedMarker),
            lim,
        );
        cx = p.put(
            cx,
            y,
            if row.masked { g.masked } else { " " },
            p.style(StyleName::ListMaskedMarker),
            lim,
        );
        cx += 1;
        let main = if row.unread {
            StyleName::ListUnread
        } else {
            StyleName::ListRow
        };
        let mut sender = sanitize(row.from.display()).into_owned();
        if row.message_count > 1 {
            sender.push_str(&format!(" ({})", row.message_count));
        }
        p.put(cx, y, &fit(&sender, o.sender_w, ell), p.style(main), lim);
        cx += o.sender_w as u16 + 1;
        let date_x = lim.saturating_sub(LIST_DATE_WIDTH as u16 + 1);
        let subj_w = usize::from(date_x.saturating_sub(cx + 1));
        let subject = sanitize(&row.subject);
        let subject_w = width(&subject);
        if o.preview && subject_w + 2 + MIN_PREVIEW <= subj_w {
            let end = p.put(cx, y, &subject, p.style(main), lim);
            let preview = sanitize(&row.preview);
            p.put(
                end + 2,
                y,
                &fit(&preview, subj_w - subject_w - 2, ell),
                p.style(StyleName::ListPreview),
                lim,
            );
        } else {
            p.put(cx, y, &fit(&subject, subj_w, ell), p.style(main), lim);
        }
        let date = dates::list_date(row.date, now);
        let date_style = if row.unread {
            StyleName::ListDateUnread
        } else {
            StyleName::ListDate
        };
        p.put(
            date_x,
            y,
            &rfit(&date, LIST_DATE_WIDTH, ell),
            p.style(date_style),
            lim,
        );
    }

    pub(super) fn reader_pane(&self, p: &mut Painter<'_>, r: Rect) {
        let s = self.state;
        let focused = s.focus == Focus::Reader;
        p.hits.push(r, HitTarget::Pane(Pane::Reader));
        if let Some(conv) = &s.reader.conversation {
            let style = if focused {
                StyleName::PaneTitleFocused
            } else {
                StyleName::PaneTitle
            };
            if r.y > 0 && r.x > 0 {
                p.title(
                    r.x + 1,
                    r.y - 1,
                    &sanitize(&conv.subject),
                    p.style(style),
                    r.right(),
                );
            }
        }
        self.doc_lines(p, r, &s.reader.doc.lines, s.reader.scroll);
        if focused {
            p.cursor = Some(Position::new(r.x, r.y));
        }
    }

    pub(super) fn doc_lines(&self, p: &mut Painter<'_>, r: Rect, lines: &[DocLine], scroll: usize) {
        for (k, line) in lines
            .iter()
            .skip(scroll)
            .take(usize::from(r.height))
            .enumerate()
        {
            let y = r.y + k as u16;
            let line_rect = Rect::new(r.x, y, r.width, 1);
            if let Some(t) = &line.target {
                p.hits.push(line_rect, t.clone());
            }
            let end = p.spans(r.x, y, &line.spans, r.right());
            if !line.right.is_empty() {
                let rw: usize = line.right.iter().map(|s| width(&s.text)).sum();
                let rx = r.right().saturating_sub(rw as u16 + 1);
                if rx > end {
                    p.spans(rx, y, &line.right, r.right());
                }
            }
        }
    }

    pub(super) fn full_reader(&self, p: &mut Painter<'_>) {
        let s = self.state;
        let l = self.layout;
        let Some(r) = l.reader else {
            return;
        };
        p.hits.push(r, HitTarget::Pane(Pane::Reader));
        if let Some(conv) = &s.reader.conversation {
            p.title(
                r.x + 1,
                r.y.saturating_sub(1),
                &sanitize(&conv.subject),
                p.style(StyleName::PaneTitleFocused),
                r.right(),
            );
        }
        self.doc_lines(p, r, &s.reader.doc.lines, s.reader.scroll);
        p.cursor = Some(Position::new(r.x, r.y));
        let Some(side) = l.side else {
            return;
        };
        let border = p.style(StyleName::Border);
        let sep_x = side.x.saturating_sub(1);
        let right_x = side.right();
        let mut y = side.y;
        for (i, section) in s.reader.doc.side.iter().enumerate() {
            if y >= side.bottom() {
                break;
            }
            let title_style = p.style(section.title_style.unwrap_or(StyleName::PaneTitle));
            if i == 0 {
                p.title(
                    side.x + 1,
                    side.y.saturating_sub(1),
                    &section.title,
                    title_style,
                    right_x,
                );
            } else {
                p.hseparator(sep_x, right_x, y, border);
                p.title(side.x + 1, y, &section.title, title_style, right_x);
                y += 1;
            }
            let room = usize::from(side.bottom().saturating_sub(y));
            let shown = section.lines.len().min(room);
            self.doc_lines(
                p,
                Rect::new(side.x, y, side.width, shown as u16),
                &section.lines,
                0,
            );
            y += shown as u16;
        }
    }
}

struct RowOpts {
    selection_column: bool,
    preview: bool,
    sender_w: usize,
}
