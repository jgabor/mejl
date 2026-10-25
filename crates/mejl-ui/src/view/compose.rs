//! The compose screen, after the compose mock-up: header fields, the body
//! editor, the attachment and format rows, draft state in the status line.

use ratatui::layout::{Position, Rect};
use ratatui::style::Modifier;

use super::{Hint, MailView, Painter};
use crate::action::Action;
use crate::address;
use crate::compose::{AddressField, ComposeState, Field};
use crate::data::{ComposeKind, DraftId};
use crate::hit::HitTarget;
use crate::reader::DocSpan;
use crate::state::DraftSync;
use crate::text::{fit, sanitize, truncate, width};
use crate::theme::StyleName;

/// Column where field values start, inside the box.
const VALUE_X: u16 = 10;

fn kind_title(kind: ComposeKind) -> &'static str {
    match kind {
        ComposeKind::New => "New message",
        ComposeKind::Reply => "Reply",
        ComposeKind::ReplyAll => "Reply all",
        ComposeKind::Forward => "Forward",
    }
}

fn size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{} KB", bytes.div_ceil(1024).max(1))
    }
}

/// Where the saved draft is, in words for the status line. Only "Drafts"
/// when the newest version is on the server.
fn where_drafts(d: &DraftSync, id: &DraftId) -> &'static str {
    if d.local_only {
        "on this device: the account has no Drafts folder"
    } else if d.refused.contains_key(id) {
        "on this device"
    } else if d.waiting.contains(id) && d.online {
        "saving to Drafts"
    } else if d.waiting.contains(id) {
        "on this device until online"
    } else {
        "Drafts"
    }
}

/// The server's reason for refusing this draft's newest version.
fn draft_error<'a>(d: &'a DraftSync, id: &DraftId) -> Option<&'a str> {
    d.refused.get(id).map(String::as_str)
}

impl MailView<'_> {
    pub(super) fn compose(&self, p: &mut Painter<'_>) {
        let s = self.state;
        let l = self.layout;
        let Some(c) = s.compose.as_deref() else {
            return;
        };
        let right = if c.ever_saved && s.draft_sync.on_device(&c.draft_id) {
            "draft on this device"
        } else if c.ever_saved {
            "draft in Drafts"
        } else {
            "not saved yet"
        };
        self.screen_top(p, kind_title(c.kind), right);
        let border = p.style(StyleName::Border);
        p.frame_box(l.frame, border, false);
        p.title(
            l.frame.x + 2,
            l.frame.y,
            kind_title(c.kind),
            p.style(StyleName::PaneTitleFocused),
            l.frame.right().saturating_sub(1),
        );
        let inner = Rect::new(
            l.frame.x + 1,
            l.frame.y + 1,
            l.frame.width.saturating_sub(2),
            l.frame.height.saturating_sub(2),
        );
        if inner.height < 4 || inner.width < 20 {
            return;
        }
        let fields = c.fields();
        let header: Vec<Field> = fields
            .iter()
            .copied()
            .filter(|f| !matches!(f, Field::Body | Field::Attachments))
            .collect();
        let mut y = inner.y;
        let mut cursor: Option<Position> = None;
        let mut suggest_at: Option<(u16, u16)> = None;
        for f in &header {
            if y >= inner.bottom() {
                break;
            }
            let index = fields.iter().position(|x| x == f).unwrap_or(0);
            let row = Rect::new(inner.x, y, inner.width, 1);
            p.hits.push(row, HitTarget::Field(index));
            self.field_label(p, c, *f, row);
            let vx = inner.x + VALUE_X;
            let max = inner.right().saturating_sub(1);
            let room = usize::from(max.saturating_sub(vx));
            let focused = c.field == *f;
            match f {
                Field::From => {
                    let text = format!(
                        "{} {}",
                        sanitize(&address::format(&c.from)),
                        s.config.theme.glyphs.expanded
                    );
                    p.put(
                        vx,
                        y,
                        &fit(&text, room, s.config.theme.glyphs.ellipsis),
                        p.style(StyleName::Text),
                        max,
                    );
                    if focused && c.editing {
                        cursor = Some(Position::new(vx, y));
                    }
                }
                Field::Subject => {
                    if focused && c.editing {
                        let (text, cx) = c.subject.view(room.max(1));
                        p.put(vx, y, &text, p.style(StyleName::Text), max);
                        let pos = Position::new(vx + cx as u16, y);
                        p.patch(
                            Rect::new(pos.x, pos.y, 1, 1),
                            p.style(StyleName::Text).add_modifier(Modifier::REVERSED),
                        );
                        cursor = Some(pos);
                    } else if c.subject.is_empty() {
                        p.put(vx, y, "add a subject", p.style(StyleName::Dim), max);
                    } else {
                        let (t, _) = truncate(
                            &sanitize(c.subject.text()),
                            room,
                            s.config.theme.glyphs.ellipsis,
                        );
                        p.put(vx, y, &t, p.style(StyleName::Text), max);
                    }
                }
                Field::To | Field::Cc | Field::Bcc => {
                    if let Some(a) = c.address(*f) {
                        let editing = focused && c.editing;
                        if let Some(pos) = self.address_value(p, a, vx, y, room, editing) {
                            cursor = Some(pos);
                            suggest_at = Some((vx, y + 1));
                        }
                    }
                }
                Field::Body | Field::Attachments => {}
            }
            y += 1;
        }
        // Separator, body, separator, attachment and format rows.
        let bottom_rows: u16 = if inner.height >= 14 { 2 } else { 1 };
        let sep1 = y;
        let sep2 = inner.bottom().saturating_sub(bottom_rows + 1);
        if sep2 > sep1 + 1 {
            p.hseparator(l.frame.x, l.frame.right().saturating_sub(1), sep1, border);
            p.hseparator(l.frame.x, l.frame.right().saturating_sub(1), sep2, border);
            // A blank line above the text, as in the mock-up, when there
            // is room for it.
            let pad: u16 = if sep2 - sep1 > 10 { 1 } else { 0 };
            let body = Rect::new(
                inner.x + 3,
                sep1 + 1 + pad,
                inner.width.saturating_sub(4),
                sep2 - sep1 - 1 - pad,
            );
            let body_index = fields.iter().position(|x| *x == Field::Body).unwrap_or(0);
            p.hits.push(
                Rect::new(inner.x, body.y, inner.width, body.height),
                HitTarget::Field(body_index),
            );
            let area = body.intersection(p.buf.area).intersection(p.clip);
            let (row, col) = if area.is_empty() {
                (0, 0)
            } else {
                c.body.render(area, p.buf)
            };
            if c.field == Field::Body && c.editing {
                let pos = Position::new(
                    (body.x + col.min(usize::from(u16::MAX)) as u16)
                        .min(body.right().saturating_sub(1)),
                    (body.y + row.min(usize::from(u16::MAX)) as u16)
                        .min(body.bottom().saturating_sub(1)),
                );
                cursor = Some(pos);
            }
            let attach_y = sep2 + 1;
            self.attachment_row(p, c, inner, attach_y, &fields);
            if bottom_rows > 1 {
                let fy = attach_y + 1;
                let row = Rect::new(inner.x, fy, inner.width, 1);
                p.put(
                    row.x + 1,
                    fy,
                    "Format",
                    p.style(StyleName::Dim),
                    row.right(),
                );
                p.put(
                    row.x + VALUE_X,
                    fy,
                    "plain text, sent as text/plain",
                    p.style(StyleName::Text),
                    row.right().saturating_sub(1),
                );
            }
        }
        if let (Some((sx, sy)), false) = (suggest_at, c.suggest.is_empty()) {
            self.suggestions(p, c, sx, sy);
        }
        if let Some(pos) = cursor {
            p.cursor = Some(pos);
        } else if let Some(i) = fields.iter().position(|f| *f == c.field)
            && let Some(r) = p.hits.find(&HitTarget::Field(i))
        {
            p.cursor = Some(Position::new(r.x + 1, r.y));
        }
        self.compose_status(p, c);
        if let Some(r) = l.hints {
            self.hint_bar(p, r);
        }
    }

    fn field_label(&self, p: &mut Painter<'_>, c: &ComposeState, f: Field, row: Rect) {
        let focused = c.field == f;
        let style = if focused {
            p.style(StyleName::PaneTitleFocused)
        } else {
            p.style(StyleName::Dim)
        };
        p.put(row.x + 1, row.y, f.label(), style, row.x + VALUE_X);
        if focused && !c.editing {
            p.patch(
                Rect::new(row.x + 1, row.y, width(f.label()) as u16, 1),
                p.style(StyleName::SidebarCursor),
            );
        }
    }

    /// Draws chips and the typed text. Returns the cursor when editing.
    fn address_value(
        &self,
        p: &mut Painter<'_>,
        a: &AddressField,
        x: u16,
        y: u16,
        room: usize,
        editing: bool,
    ) -> Option<Position> {
        let ell = self.state.config.theme.glyphs.ellipsis;
        let max = x + room as u16;
        let chips: Vec<String> = a
            .chips
            .iter()
            .map(|c| sanitize(&address::format(c)).into_owned())
            .collect();
        if !editing {
            if chips.is_empty() && a.input.is_empty() {
                p.put(x, y, "add people", p.style(StyleName::Dim), max);
                return None;
            }
            let mut all = chips.join(", ");
            if !a.input.is_empty() {
                if !all.is_empty() {
                    all.push_str(", ");
                }
                all.push_str(a.input.text());
            }
            let (t, _) = truncate(&all, room, ell);
            p.put(x, y, &t, p.style(StyleName::Text), max);
            if a.error.is_some() {
                let w = width(&t) as u16;
                p.put(x + w + 1, y, "!", p.style(StyleName::StatusError), max);
            }
            return None;
        }
        // Editing: keep the typed text visible, dropping chips from the
        // front when the line is full.
        let input_room = room.saturating_sub(1).max(8).min(room);
        let (typed, cx) = a.input.view(input_room.max(1));
        let typed_w = width(&typed).max(cx + 1);
        let mut shown: Vec<&String> = chips.iter().collect();
        let mut hidden = 0usize;
        let prefix_w = |shown: &[&String], hidden: usize| -> usize {
            let mut w: usize = shown.iter().map(|s| width(s) + 2).sum();
            if hidden > 0 {
                w += width(&format!("+{hidden}, "));
            }
            w
        };
        while !shown.is_empty() && prefix_w(&shown, hidden) + typed_w > room {
            shown.remove(0);
            hidden += 1;
        }
        let mut cx_pos = x;
        if hidden > 0 {
            cx_pos = p.put(
                cx_pos,
                y,
                &format!("+{hidden}, "),
                p.style(StyleName::Dim),
                max,
            );
        }
        for chip in shown {
            cx_pos = p.put(cx_pos, y, chip, p.style(StyleName::Text), max);
            cx_pos = p.put(cx_pos, y, ", ", p.style(StyleName::Dim), max);
        }
        let style = if a.error.is_some() {
            p.style(StyleName::StatusError)
        } else {
            p.style(StyleName::Text)
        };
        p.put(cx_pos, y, &typed, style, max);
        let pos = Position::new((cx_pos + cx as u16).min(max.saturating_sub(1)), y);
        p.patch(
            Rect::new(pos.x, pos.y, 1, 1),
            p.style(StyleName::Text).add_modifier(Modifier::REVERSED),
        );
        Some(pos)
    }

    fn suggestions(&self, p: &mut Painter<'_>, c: &ComposeState, x: u16, y: u16) {
        let ell = self.state.config.theme.glyphs.ellipsis;
        let frame = self.layout.frame;
        let w = 52u16.min(frame.right().saturating_sub(x + 1));
        let h = (c.suggest.len() as u16 + 2).min(frame.bottom().saturating_sub(y));
        if w < 10 || h < 3 {
            return;
        }
        let r = Rect::new(x.saturating_sub(1), y, w, h);
        p.clear(r);
        p.frame_box(r, p.style(StyleName::PopupBorder), false);
        for (i, s) in c.suggest.iter().enumerate().take(usize::from(h - 2)) {
            let row_y = r.y + 1 + i as u16;
            let text = fit(
                &sanitize(&address::format(s)),
                usize::from(w.saturating_sub(4)),
                ell,
            );
            p.put(
                r.x + 2,
                row_y,
                &text,
                p.style(StyleName::Text),
                r.right() - 1,
            );
            if i == c.suggest_cursor {
                p.patch(
                    Rect::new(r.x + 1, row_y, w.saturating_sub(2), 1),
                    p.style(StyleName::PopupCursor),
                );
            }
        }
    }

    fn attachment_row(
        &self,
        p: &mut Painter<'_>,
        c: &ComposeState,
        inner: Rect,
        y: u16,
        fields: &[Field],
    ) {
        let s = self.state;
        let row = Rect::new(inner.x, y, inner.width, 1);
        let index = fields
            .iter()
            .position(|f| *f == Field::Attachments)
            .unwrap_or(0);
        p.hits.push(row, HitTarget::Field(index));
        self.field_label(p, c, Field::Attachments, row);
        let max = row.right().saturating_sub(1);
        let mut x = row.x + VALUE_X;
        let focused = c.field == Field::Attachments && c.editing;
        for (i, a) in c.attachments.iter().enumerate() {
            let start = x;
            x = p.put(
                x,
                y,
                &sanitize(&a.name),
                p.style(StyleName::ReaderAttachment),
                max,
            );
            x = p.put(
                x,
                y,
                &format!("  {}", size(a.size)),
                p.style(StyleName::Dim),
                max,
            );
            if focused && i == c.attach_cursor {
                p.patch(
                    Rect::new(start, y, x.saturating_sub(start), 1),
                    p.style(StyleName::PopupCursor),
                );
            }
            x = p.put(x, y, "    ", p.style(StyleName::Text), max);
        }
        for path in &c.uploading {
            x = p.put(
                x,
                y,
                &format!("{} uploading  ", sanitize(path)),
                p.style(StyleName::Dim),
                max,
            );
        }
        let keys = |a: Action| self.keys_for(a);
        if !c.attachments.is_empty() {
            x = p.put(x, y, "Del", p.style(StyleName::HintKey), max);
            x = p.put(x, y, " remove   ", p.style(StyleName::HintLabel), max);
        }
        if let Some(k) = keys(Action::Attach) {
            let start = x;
            x = p.put(x, y, &k, p.style(StyleName::HintKey), max);
            x = p.put(x, y, " add a file", p.style(StyleName::HintLabel), max);
            p.hits.push(
                Rect::new(start, y, x.saturating_sub(start), 1),
                HitTarget::Action(Action::Attach),
            );
        } else if c.attachments.is_empty() {
            p.put(x, y, "none", p.style(StyleName::Dim), max);
        }
        let _ = s;
    }

    fn compose_status(&self, p: &mut Painter<'_>, c: &ComposeState) {
        let s = self.state;
        let g = &s.config.theme.glyphs;
        let now = s.clock.now();
        let field_error = c.address(c.field).and_then(|a| a.error.clone());
        let left: Vec<DocSpan> = if let Some(e) = field_error {
            vec![DocSpan::new(
                format!("! {}", sanitize(&e)),
                StyleName::StatusError,
            )]
        } else if let Some(e) = &c.save_error {
            vec![DocSpan::new(
                format!("! Draft not saved: {}", sanitize(e)),
                StyleName::StatusError,
            )]
        } else if c.saving {
            vec![DocSpan::new(
                format!("{} Saving the draft", g.ellipsis),
                StyleName::Dim,
            )]
        } else if let Some(at) = c.saved_at {
            let secs = now.duration_since(at).as_secs().max(0);
            let ago = if secs < 60 {
                format!("{secs} s ago")
            } else {
                format!("{} min ago", secs / 60)
            };
            let mut spans = vec![
                DocSpan::new(format!("{} ", g.ok), StyleName::StatusOk),
                DocSpan::new(format!("Draft saved {ago}"), StyleName::StatusText),
                DocSpan::new(
                    format!("  ({})", where_drafts(&s.draft_sync, &c.draft_id)),
                    StyleName::Dim,
                ),
            ];
            if let Some(e) = draft_error(&s.draft_sync, &c.draft_id) {
                spans.push(DocSpan::new(
                    format!("  ! {}", sanitize(e)),
                    StyleName::StatusError,
                ));
            }
            spans
        } else if c.dirty {
            vec![DocSpan::new("Not saved yet", StyleName::Dim)]
        } else {
            vec![DocSpan::new("Saves itself as you type", StyleName::Dim)]
        };
        let words = c.words();
        let right = vec![DocSpan::new(
            format!("{words} {}   ", if words == 1 { "word" } else { "words" }),
            StyleName::Dim,
        )];
        self.status_line(p, Some(left), right);
    }

    pub(super) fn compose_hints(&self) -> Vec<Hint> {
        let editing = self.state.compose.as_ref().is_some_and(|c| c.editing);
        let one = |a: Action, label: &'static str, short: &'static str, important: bool| {
            self.keys_for(a).map(|k| Hint {
                keys: k,
                label,
                short,
                target: HitTarget::Action(a),
                important,
            })
        };
        let mut v = vec![
            one(Action::NextField, "next field", "next", true),
            one(Action::Send, "send", "send", true),
            one(Action::SaveDraft, "save", "save", false),
            one(Action::Attach, "attach", "attach", false),
            one(Action::NewMaskFrom, "new mask", "mask", false),
            one(Action::ExternalEditor, "$EDITOR", "editor", false),
            one(Action::DiscardDraft, "discard", "discard", false),
        ];
        if editing {
            v.push(one(Action::LeaveField, "leave field", "leave", true));
        } else {
            v.push(Some(Hint::fixed("Enter", "edit", HitTarget::Popup)));
            v.push(one(Action::LeaveField, "close, keep draft", "close", true));
        }
        v.into_iter().flatten().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(size(212 * 1024), "212 KB");
        assert_eq!(size(1), "1 KB");
        assert_eq!(size(4 * 1024 * 1024 + 300_000), "4.3 MB");
        assert_eq!(kind_title(ComposeKind::Forward), "Forward");
    }

    #[test]
    fn the_status_says_where_the_draft_is() {
        let me = DraftId::from("d1");
        let other = DraftId::from("d2");
        let mut d = DraftSync::default();
        assert_eq!(where_drafts(&d, &me), "Drafts");
        d.waiting.insert(me.clone());
        assert_eq!(where_drafts(&d, &me), "on this device until online");
        d.online = true;
        assert_eq!(where_drafts(&d, &me), "saving to Drafts");
        assert_eq!(where_drafts(&d, &other), "Drafts", "only its own upload");
        d.refused.insert(
            me.clone(),
            "Cannot save the draft to the server: too big".into(),
        );
        assert_eq!(
            where_drafts(&d, &me),
            "on this device",
            "not saving any more"
        );
        assert_eq!(
            draft_error(&d, &me),
            Some("Cannot save the draft to the server: too big")
        );
        assert_eq!(draft_error(&d, &other), None, "not another draft's refusal");
        assert!(d.on_device(&me) && !d.on_device(&other));
        d.local_only = true;
        assert_eq!(
            where_drafts(&d, &other),
            "on this device: the account has no Drafts folder"
        );
    }
}
