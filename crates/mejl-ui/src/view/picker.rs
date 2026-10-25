//! The picker popup: palette, move and label pickers, From, links and the
//! menus. Laid out after the palette mock-up: a filter line, a list with
//! label, key and tag columns, and footer lines.

use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};

use super::{Hint, MailView, Painter};
use crate::hit::HitTarget;
use crate::picker::{PickValue, Picker, PickerKind};
use crate::text::{fit, sanitize, width};
use crate::theme::StyleName;

/// Key and tag column widths, from the mock-up.
const KEY_W: usize = 10;
const TAG_W: usize = 17;

/// Hints while a picker is open.
pub(super) fn hints(p: &Picker) -> Vec<Hint> {
    let mut v = vec![
        Hint::fixed("Up/Down", "move", HitTarget::Popup),
        Hint::fixed(
            "Enter",
            match p.kind {
                PickerKind::Palette | PickerKind::Actions | PickerKind::Mask => "run",
                PickerKind::MoveTo => "move",
                PickerKind::Label => "toggle",
                PickerKind::From => "use",
                PickerKind::Links => "open",
                PickerKind::Copy => "copy",
                PickerKind::MacroFolder => "pick",
            },
            HitTarget::Popup,
        ),
    ];
    match p.kind {
        PickerKind::Palette => v.push(Hint::fixed("Tab", "complete", HitTarget::Popup)),
        PickerKind::Links => v.push(Hint::fixed("^y", "copy", HitTarget::Popup)),
        _ => {}
    }
    v.push(Hint::fixed("^n/^p", "next/prev", HitTarget::Popup));
    v.push(Hint::fixed("Esc", "close", HitTarget::Backdrop));
    v
}

impl MailView<'_> {
    /// The status line text while a picker is open.
    pub(super) fn picker_status(&self, p: &Picker) -> String {
        let what = match p.kind {
            PickerKind::Palette => "commands",
            PickerKind::MoveTo | PickerKind::Label | PickerKind::MacroFolder => "mailboxes",
            PickerKind::From => "addresses",
            PickerKind::Links => "links",
            PickerKind::Copy | PickerKind::Actions | PickerKind::Mask => "items",
        };
        let total = p.choosable();
        let filter = p.filter.text().trim();
        if filter.is_empty() {
            format!("{total} {what}")
        } else {
            let shown = p
                .shown
                .iter()
                .filter(|s| {
                    p.items
                        .get(s.index)
                        .is_some_and(|i| i.value != PickValue::CreateMailbox)
                })
                .count();
            format!("{shown} of {total} {what} match \"{}\"", sanitize(filter))
        }
    }

    pub(super) fn picker(&self, p: &mut Painter<'_>, pk: &Picker) {
        let s = self.state;
        let ell = s.config.theme.glyphs.ellipsis;
        let frame = self.layout.frame;
        let palette = pk.kind == PickerKind::Palette;
        let has_tags = pk.items.iter().any(|i| !i.tag.is_empty());
        let has_keys = pk.items.iter().any(|i| !i.key.is_empty());
        let want_w: u16 = if palette { 84 } else { 72 };
        let w = want_w
            .min(frame.width.saturating_sub(2))
            .max(20.min(frame.width));
        let footer = pk.footer.len() as u16;
        let fixed_rows = 2 + 2 + if footer > 0 { footer + 2 } else { 0 };
        let top = frame.y + 3.min(frame.height / 4);
        let max_h = frame.bottom().saturating_sub(top + 1);
        let list_rows = (pk.shown.len().max(1) as u16).min(max_h.saturating_sub(fixed_rows).max(1));
        let h = (list_rows + fixed_rows).min(max_h.max(5));
        // Centred, as in the palette mock-up.
        let x = (frame.x + frame.width.saturating_sub(w) / 2).max(frame.x + 1);
        let r = if pk.kind == PickerKind::From {
            // Next to the From field, as in the compose mock-up.
            let w = w.min(frame.width.saturating_sub(6));
            let x = frame.right().saturating_sub(3 + w).max(frame.x + 1);
            let top = frame.y + 1;
            let max_h = frame.bottom().saturating_sub(top + 1);
            Rect::new(x, top, w, h.min(max_h.max(5)))
        } else {
            Rect::new(x, top, w, h)
        };
        p.hits
            .push(frame.union(self.layout.top), HitTarget::Backdrop);
        p.clear(Rect::new(
            r.x + 1,
            r.y + 1,
            r.width.saturating_sub(2),
            r.height.saturating_sub(2),
        ));
        p.hits.push(r, HitTarget::Popup);
        let border = p.style(StyleName::PopupBorder);
        p.frame_box(r, border, false);
        let right = r.right().saturating_sub(1);
        p.title(
            r.x + 2,
            r.y,
            &pk.title,
            p.style(StyleName::PopupTitle),
            right,
        );
        if !pk.hint.is_empty() {
            let hw = width(&pk.hint) as u16 + 2;
            let hx = right.saturating_sub(hw + 2);
            if hx > r.x + width(&pk.title) as u16 + 6 {
                p.title(hx, r.y, &pk.hint, p.style(StyleName::Dim), right);
            }
        }
        let inner_x = r.x + 2;
        let inner_right = right;
        // Filter line.
        let y = r.y + 1;
        let mut fx = p.put(
            inner_x,
            y,
            "> ",
            p.style(StyleName::PopupTitle),
            inner_right,
        );
        let room = usize::from(inner_right.saturating_sub(fx + 1));
        let (text, cx) = pk.filter.view(room.max(1));
        if pk.filter.is_empty() {
            p.put(
                fx,
                y,
                "type to filter",
                p.style(StyleName::Dim),
                inner_right,
            );
            p.patch(
                Rect::new(fx, y, 1, 1),
                p.style(StyleName::Text).add_modifier(Modifier::REVERSED),
            );
            p.cursor = Some(Position::new(fx, y));
        } else {
            let start = fx;
            fx = p.put(fx, y, &text, p.style(StyleName::Text), inner_right);
            let _ = fx;
            let cur = start + cx as u16;
            p.patch(
                Rect::new(cur, y, 1, 1),
                p.style(StyleName::Text).add_modifier(Modifier::REVERSED),
            );
            p.cursor = Some(Position::new(cur, y));
        }
        p.hseparator(r.x, right, r.y + 2, border);
        // Rows.
        let inner_w = usize::from(r.width.saturating_sub(5));
        let key_w = if has_keys { KEY_W } else { 0 };
        let tag_w = if has_tags { TAG_W.min(inner_w / 3) } else { 0 };
        let label_w = inner_w.saturating_sub(2 + key_w + tag_w);
        let offset = pk
            .cursor
            .saturating_sub(usize::from(list_rows).saturating_sub(1));
        let first = r.y + 3;
        if pk.shown.is_empty() {
            p.put(
                inner_x + 2,
                first,
                "Nothing matches",
                p.style(StyleName::Dim),
                inner_right,
            );
        }
        for (k, shown) in pk
            .shown
            .iter()
            .enumerate()
            .skip(offset)
            .take(usize::from(list_rows))
        {
            let Some(item) = pk.items.get(shown.index) else {
                continue;
            };
            let y = first + (k - offset) as u16;
            let line = Rect::new(r.x + 1, y, r.width.saturating_sub(2), 1);
            let info = item.value == PickValue::Info;
            let cursor = k == pk.cursor && !info;
            let marker = if cursor {
                s.config.theme.glyphs.collapsed
            } else {
                " "
            };
            p.put(
                inner_x,
                y,
                marker,
                p.style(StyleName::ReaderMarker),
                inner_right,
            );
            let mut label = String::new();
            if let Some(c) = item.checked {
                label.push_str(if c { "[x] " } else { "[ ] " });
            }
            let offset_chars = label.chars().count();
            label.push_str(&sanitize(&item.label));
            // One blank cell always separates the label from the key.
            let label = format!("{} ", fit(&label, label_w.saturating_sub(1), ell));
            let base = if info {
                p.style(StyleName::PopupGroup)
            } else {
                p.style(StyleName::Text)
            };
            let lx = inner_x + 2;
            if shown.positions.is_empty() {
                p.put(lx, y, &label, base, inner_right);
            } else {
                let hl = p.style(StyleName::HelpMatch);
                let mut cx = lx;
                for (ci, ch) in label.chars().enumerate() {
                    let matched =
                        ci >= offset_chars && shown.positions.contains(&(ci - offset_chars));
                    let mut buf = [0u8; 4];
                    cx = p.put(
                        cx,
                        y,
                        ch.encode_utf8(&mut buf),
                        if matched { hl } else { base },
                        inner_right,
                    );
                }
            }
            let kx = lx + label_w as u16;
            if key_w > 0 {
                p.put(
                    kx,
                    y,
                    &fit(&item.key, key_w, ell),
                    p.style(StyleName::HintKey),
                    inner_right,
                );
            }
            if tag_w > 0 {
                p.put(
                    kx + key_w as u16,
                    y,
                    &fit(&sanitize(&item.tag), tag_w, ell),
                    p.style(StyleName::Dim),
                    inner_right,
                );
            }
            if cursor {
                p.patch(line, p.style(StyleName::PopupCursor));
            }
            if !info {
                p.hits.push(line, HitTarget::PickerRow(k));
            }
        }
        if footer > 0 {
            let sep = r.bottom().saturating_sub(footer + 2);
            if sep > first {
                p.hseparator(r.x, right, sep, border);
                for (i, f) in pk.footer.iter().enumerate() {
                    p.put(
                        inner_x,
                        sep + 1 + i as u16,
                        &fit(f, usize::from(r.width.saturating_sub(4)), ell),
                        p.style(StyleName::Dim),
                        inner_right,
                    );
                }
            }
        }
        let _ = Style::new();
    }
}
