//! Popups: the sidebar overlay, which-key, help and the perf overlay.

use std::time::Duration;

use ratatui::layout::Rect;

use super::{MailView, Painter};
use crate::action::{Action, Category};
use crate::hit::HitTarget;
use crate::keymap::{Context, ContinuationTarget, KeySeq};
use crate::perf::BUCKETS_MS;
use crate::state::HelpState;
use crate::text::{fit, truncate, width};
use crate::theme::StyleName;

/// A which-key entry: key, label, target.
type Entry = (String, String, HitTarget);
/// Entries under one heading.
type Group = (Option<Category>, Vec<Entry>);

/// Width of one which-key column, key and label.
const WK_COL: usize = 23;

impl MailView<'_> {
    pub(super) fn sidebar_overlay(&self, p: &mut Painter<'_>, r: Rect) {
        p.hits.push(self.layout.frame, HitTarget::Backdrop);
        p.clear(inner_of(r));
        p.hits.push(r, HitTarget::Popup);
        let border = p.style(StyleName::PopupBorder);
        p.frame_box(r, border, true);
        let inner = Rect::new(
            r.x + 1,
            r.y + 1,
            r.width.saturating_sub(2),
            r.height.saturating_sub(2),
        );
        self.sidebar(p, inner, true);
    }

    pub(super) fn which_key(&self, p: &mut Painter<'_>) {
        let s = self.state;
        let km = &s.config.keymap;
        let contexts = self.contexts();
        let prefix = s.pending.keys.clone();
        let seq = KeySeq(prefix.clone());
        let entries = km.continuations(&prefix, &contexts);
        if entries.is_empty() {
            return;
        }
        // Group: go-to and navigation first, then tools, then the rest by
        // category.
        let mut groups: Vec<Group> = Vec::new();
        for c in &entries {
            let (label, cat, target) = match &c.target {
                ContinuationTarget::Action(a) => (
                    a.label().to_owned(),
                    Some(match a.category() {
                        Category::Navigate => Category::Go,
                        other => other,
                    }),
                    HitTarget::Action(*a),
                ),
                ContinuationTarget::Prefix(label) => {
                    let mut next = prefix.clone();
                    next.push(c.key);
                    (
                        format!("{label} ..."),
                        None,
                        HitTarget::Prefix(KeySeq(next)),
                    )
                }
            };
            let key = c.key.to_string();
            match groups.iter_mut().find(|(g, _)| *g == cat) {
                Some((_, v)) => v.push((key, label, target)),
                None => groups.push((cat, vec![(key, label, target)])),
            }
        }
        let others: Vec<String> = km
            .continuations(&[], &contexts)
            .into_iter()
            .filter(|c| {
                matches!(c.target, ContinuationTarget::Prefix(_)) && Some(&c.key) != prefix.first()
            })
            .map(|c| {
                // Keep the footer short: "b snooze", not the full label.
                let label = km.prefix_label(&KeySeq(vec![c.key]));
                let short = label.split(" (").next().unwrap_or(label);
                format!("{} {short}", c.key)
            })
            .collect();
        let mut lines: Vec<Vec<(String, StyleName, Option<HitTarget>)>> = Vec::new();
        lines.push(Vec::new());
        for (gi, (cat, items)) in groups.iter().enumerate() {
            if gi > 0 {
                lines.push(Vec::new());
                let heading = match cat {
                    Some(c) => c.label().to_owned(),
                    None => "more".to_owned(),
                };
                lines.push(vec![(heading, StyleName::PopupGroup, None)]);
            }
            let rows = items.len().div_ceil(2);
            for row in 0..rows {
                let mut line = Vec::new();
                for col in 0..2 {
                    if let Some((key, label, target)) = items.get(col * rows + row) {
                        let text_w = WK_COL.saturating_sub(width(key) + 2);
                        line.push((key.clone(), StyleName::HintKey, Some(target.clone())));
                        let label = fit(label, text_w, s.config.theme.glyphs.ellipsis);
                        line.push((format!("  {label}"), StyleName::Text, Some(target.clone())));
                    }
                }
                lines.push(line);
            }
        }
        lines.push(Vec::new());
        let footer1 = vec![
            (
                "Esc".to_owned(),
                StyleName::HintKey,
                Some(HitTarget::Backdrop),
            ),
            (
                format!(" cancel   no timeout: {seq} does nothing alone"),
                StyleName::Dim,
                None,
            ),
        ];
        let mut footer2 = Vec::new();
        if !others.is_empty() {
            // Only whole entries that fit the popup, never a cut label.
            let room = usize::from(50u16.min(self.layout.frame.width).saturating_sub(4));
            let mut used = width("Same popup for ");
            footer2.push(("Same popup for ".to_owned(), StyleName::Dim, None));
            for (i, o) in others.iter().enumerate() {
                let (k, label) = o.split_once(' ').unwrap_or((o.as_str(), ""));
                let sep = if i > 0 { 2 } else { 0 };
                let w = sep + width(k) + 1 + width(label);
                if used + w > room {
                    break;
                }
                used += w;
                if i > 0 {
                    footer2.push(("  ".to_owned(), StyleName::Dim, None));
                }
                footer2.push((k.to_owned(), StyleName::HintKey, None));
                footer2.push((format!(" {label}"), StyleName::Dim, None));
            }
        }
        let frame = self.layout.frame;
        let pw = 50u16.min(frame.width);
        let footer_rows = if footer2.is_empty() { 1 } else { 2 };
        let ph = (lines.len() + 1 + footer_rows + 1 + 2) as u16;
        let ph = ph.min(frame.height);
        let px = frame.right().saturating_sub(pw);
        let py = frame
            .bottom()
            .saturating_sub(1)
            .saturating_sub(ph)
            .max(frame.y);
        let r = Rect::new(px, py, pw, ph);
        p.clear(inner_of(r));
        p.hits.push(r, HitTarget::Popup);
        let border = p.style(StyleName::PopupBorder);
        p.frame_box(r, border, true);
        let title = format!("{seq}  {}", km.prefix_label(&seq));
        p.title(
            r.x + 2,
            r.y,
            &title,
            p.style(StyleName::PopupTitle),
            r.right().saturating_sub(1),
        );
        let max = r.right().saturating_sub(1);
        let mut y = r.y + 1;
        let draw_line =
            |p: &mut Painter<'_>, y: u16, parts: &[(String, StyleName, Option<HitTarget>)]| {
                let mut x = r.x + 2;
                for (i, (text, style, target)) in parts.iter().enumerate() {
                    // Second column starts at a fixed offset.
                    if i == 2 {
                        x = r.x + 2 + WK_COL as u16;
                    }
                    let start = x;
                    x = p.put(x, y, text, p.style(*style), max);
                    if let Some(t) = target {
                        p.hits.push(Rect::new(start, y, x - start, 1), t.clone());
                    }
                }
            };
        for line in &lines {
            if y >= r.bottom().saturating_sub(1) {
                break;
            }
            draw_line(p, y, line);
            y += 1;
        }
        if y < r.bottom().saturating_sub(1) {
            p.hseparator(r.x, r.right() - 1, y, border);
            y += 1;
        }
        for f in [&footer1, &footer2] {
            if f.is_empty() || y >= r.bottom().saturating_sub(1) {
                continue;
            }
            let mut x = r.x + 2;
            for (text, style, target) in f.iter() {
                let start = x;
                x = p.put(x, y, text, p.style(*style), max);
                if let Some(t) = target {
                    p.hits.push(Rect::new(start, y, x - start, 1), t.clone());
                }
            }
            y += 1;
        }
    }

    /// A centred popup inside the mail box, clear of its borders.
    fn popup_rect(&self, w: u16, h: u16) -> Rect {
        let area = inner_of(self.layout.frame);
        let w = w.min(area.width).max(10.min(area.width));
        let h = h.min(area.height).max(3.min(area.height));
        Rect::new(
            area.x + (area.width.saturating_sub(w)) / 2,
            area.y + (area.height.saturating_sub(h)) / 2,
            w,
            h,
        )
    }

    pub(super) fn help(&self, p: &mut Painter<'_>, help: &HelpState) {
        let s = self.state;
        let km = &s.config.keymap;
        let ell = s.config.theme.glyphs.ellipsis;
        let size = s.size;
        let r = self.popup_rect(80, size.1);
        p.hits
            .push(Rect::new(0, 0, size.0, size.1), HitTarget::Backdrop);
        p.clear(r);
        p.hits.push(r, HitTarget::Popup);
        let border = p.style(StyleName::PopupBorder);
        p.frame_box(r, border, true);
        let title = format!("Keys: {}", help.context.name());
        p.title(
            r.x + 2,
            r.y,
            &title,
            p.style(StyleName::PopupTitle),
            r.right().saturating_sub(1),
        );
        let inner = Rect::new(
            r.x + 2,
            r.y + 1,
            r.width.saturating_sub(4),
            r.height.saturating_sub(2),
        );
        if inner.height < 3 || inner.width < 10 {
            return;
        }
        let max = inner.right();
        // Filter line.
        let mut x = p.put(inner.x, inner.y, "/ ", p.style(StyleName::PopupTitle), max);
        if help.filter.is_empty() {
            p.put(x, inner.y, "type to filter", p.style(StyleName::Dim), max);
        } else {
            x = p.put(x, inner.y, &help.filter, p.style(StyleName::Text), max);
            p.put(
                x,
                inner.y,
                " ",
                p.style(StyleName::Text)
                    .add_modifier(ratatui::style::Modifier::REVERSED),
                max,
            );
        }
        // Rows.
        let contexts: Vec<Context> = if help.context == Context::Global {
            vec![Context::Global]
        } else {
            vec![help.context, Context::Global]
        };
        let filter = help.filter.to_lowercase();
        let mut rows: Vec<(Category, String, Action)> = Vec::new();
        for (ci, ctx) in contexts.iter().enumerate() {
            for (seq, action) in km.bindings(*ctx) {
                // Skip bindings an earlier context hides.
                let hidden = contexts[..ci].iter().any(|earlier| {
                    km.bindings(*earlier)
                        .iter()
                        .any(|(k, _)| k == &seq || k.is_prefix_of(&seq) || seq.is_prefix_of(k))
                });
                if hidden {
                    continue;
                }
                let keys = seq.to_string();
                if !filter.is_empty() {
                    let hay = format!(
                        "{keys} {} {} {}",
                        action.name(),
                        action.label(),
                        action.description()
                    )
                    .to_lowercase();
                    if !hay.contains(&filter) {
                        continue;
                    }
                }
                match rows.iter_mut().find(|(_, _, a)| *a == action) {
                    Some(row) => {
                        row.1.push_str(", ");
                        row.1.push_str(&keys);
                    }
                    None => rows.push((action.category(), keys, action)),
                }
            }
        }
        rows.sort_by_key(|(c, _, _)| *c);
        // The body editor's undo keys are not keymap bindings (the editor
        // reads them itself), so compose help lists them on their own.
        let body: Vec<(&str, &str)> = if help.context == Context::Compose {
            crate::compose::BODY_UNDO_HELP
                .iter()
                .copied()
                .filter(|(k, d)| {
                    filter.is_empty() || format!("{k} {d}").to_lowercase().contains(&filter)
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut lines: Vec<Vec<(String, StyleName)>> = Vec::new();
        // First, as they are what a writer looks for.
        if !body.is_empty() {
            lines.push(vec![("Body editor".to_owned(), StyleName::PopupGroup)]);
            for (keys, desc) in &body {
                lines.push(vec![
                    (fit(keys, 14, ell), StyleName::HintKey),
                    (" ".to_owned(), StyleName::Text),
                    ((*desc).to_owned(), StyleName::Text),
                ]);
            }
        }
        let mut last_cat = None;
        for (cat, keys, action) in &rows {
            if last_cat != Some(*cat) {
                if last_cat.is_some() || !body.is_empty() {
                    lines.push(Vec::new());
                }
                lines.push(vec![(cat.label().to_owned(), StyleName::PopupGroup)]);
                last_cat = Some(*cat);
            }
            let key_w = 14usize;
            let name = action.name();
            let desc_w = usize::from(inner.width).saturating_sub(key_w + width(name) + 3);
            lines.push(vec![
                (fit(keys, key_w, ell), StyleName::HintKey),
                (" ".to_owned(), StyleName::Text),
                (fit(action.description(), desc_w, ell), StyleName::Text),
                (format!("  {name}"), StyleName::Dim),
            ]);
        }
        if rows.is_empty() && body.is_empty() {
            lines.push(vec![(
                "Nothing matches the filter".to_owned(),
                StyleName::Dim,
            )]);
        }
        lines.push(Vec::new());
        lines.push(vec![(
            "Hold Shift to select text with the mouse in most terminals.".to_owned(),
            StyleName::Dim,
        )]);
        let body_h = usize::from(inner.height - 2);
        let max_scroll = lines.len().saturating_sub(body_h);
        let scroll = help.scroll.min(max_scroll);
        p.hseparator(r.x, r.right() - 1, inner.y + 1, border);
        for (k, line) in lines.iter().skip(scroll).take(body_h).enumerate() {
            let y = inner.y + 2 + k as u16;
            let mut x = inner.x;
            for (text, style) in line {
                let (t, _) = truncate(text, usize::from(max.saturating_sub(x)), ell);
                x = p.put(x, y, &t, p.style(*style), max);
            }
        }
    }

    pub(super) fn perf(&self, p: &mut Painter<'_>) {
        let s = self.state;
        let sum = s.perf.summary();
        let r = self.popup_rect(56, 16);
        p.hits
            .push(Rect::new(0, 0, s.size.0, s.size.1), HitTarget::Backdrop);
        p.clear(r);
        p.hits.push(r, HitTarget::Popup);
        let border = p.style(StyleName::PopupBorder);
        p.frame_box(r, border, true);
        p.title(
            r.x + 2,
            r.y,
            "Frame timings",
            p.style(StyleName::PopupTitle),
            r.right().saturating_sub(1),
        );
        let ms = |d: Duration| format!("{:.1} ms", d.as_secs_f64() * 1000.0);
        let pct = |n: usize| {
            if sum.count == 0 {
                0.0
            } else {
                n as f64 * 100.0 / sum.count as f64
            }
        };
        let mut lines: Vec<(String, StyleName)> = vec![
            (
                format!(
                    "{} frames in the window, {} since start",
                    sum.count, sum.total
                ),
                StyleName::Dim,
            ),
            (String::new(), StyleName::Text),
            (
                format!(
                    "p50 {}   p95 {}   p99 {}",
                    ms(sum.p50),
                    ms(sum.p95),
                    ms(sum.p99)
                ),
                StyleName::Text,
            ),
            (format!("max {}", ms(sum.max)), StyleName::Text),
            (String::new(), StyleName::Text),
        ];
        let labels = BUCKETS_MS
            .iter()
            .map(|b| format!("under {b} ms"))
            .chain(std::iter::once(format!(
                "{} ms or more",
                BUCKETS_MS[BUCKETS_MS.len() - 1]
            )));
        for (label, n) in labels.zip(sum.buckets) {
            lines.push((
                format!("{}{n:>6}  {:>5.1}%", fit(&label, 18, "~"), pct(n)),
                StyleName::Text,
            ));
        }
        lines.push((String::new(), StyleName::Text));
        let slow: Vec<String> = sum.slowest.iter().map(|d| ms(*d)).collect();
        lines.push((
            if slow.is_empty() {
                "no frames measured yet".to_owned()
            } else {
                format!("slowest: {}", slow.join(", "))
            },
            StyleName::Text,
        ));
        lines.push((
            "budget: p99 under 8 ms for local actions".to_owned(),
            StyleName::Dim,
        ));
        let p99_ok = sum.count == 0 || sum.p99 < Duration::from_millis(8);
        lines.push(if p99_ok {
            (
                format!("{} within budget", s.config.theme.glyphs.ok),
                StyleName::StatusOk,
            )
        } else {
            (
                format!("{} over budget", s.config.theme.glyphs.fail),
                StyleName::StatusError,
            )
        });
        let max = r.right().saturating_sub(2);
        for (k, (text, style)) in lines.iter().enumerate() {
            let y = r.y + 1 + k as u16;
            if y >= r.bottom().saturating_sub(1) {
                break;
            }
            p.put(r.x + 2, y, text, p.style(*style), max);
        }
    }
}

/// The area inside a box's borders.
fn inner_of(r: Rect) -> Rect {
    Rect::new(
        r.x.saturating_add(1),
        r.y.saturating_add(1),
        r.width.saturating_sub(2),
        r.height.saturating_sub(2),
    )
}
