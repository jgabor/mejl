//! The Masked Email, rules and settings screens, after their mock-ups.

use mejl_sieve::{ValidationContext, check_script, render_block, validate};
use ratatui::layout::{Position, Rect};

use super::{Hint, MailView, Painter};
use crate::action::Action;
use crate::data::MaskedState;
use crate::hit::HitTarget;
use crate::masked::{MaskedScreen, STATES, SilentFilter, last_seen, state_name};
use crate::reader::DocSpan;
use crate::rules::{
    BLOCK_ORDER, FormRow, RuleEntry, RulesPane, RulesScreen, action_text, condition_text, form_rows,
};
use crate::settings::ROWS;
use crate::state::Screen;
use crate::text::{fit, rfit, sanitize, truncate, width};
use crate::theme::StyleName;

/// One part of a line: text and style.
type Part = (String, StyleName);

fn inner_of(r: Rect) -> Rect {
    Rect::new(
        r.x + 1,
        r.y + 1,
        r.width.saturating_sub(2),
        r.height.saturating_sub(2),
    )
}

impl MailView<'_> {
    fn parts(&self, p: &mut Painter<'_>, x: u16, y: u16, parts: &[Part], max: u16) -> u16 {
        let mut x = x;
        for (t, s) in parts {
            x = p.put(x, y, t, p.style(*s), max);
        }
        x
    }

    fn key_hint_parts(&self, action: Action, label: &str) -> Vec<Part> {
        match self.keys_for(action) {
            Some(k) => vec![
                (k, StyleName::HintKey),
                (format!(" {label}"), StyleName::HintLabel),
            ],
            None => Vec::new(),
        }
    }

    // ----- Masked Email -----

    pub(super) fn masked(&self, p: &mut Painter<'_>) {
        let s = self.state;
        let l = self.layout;
        let Some(m) = s.masked.as_deref() else {
            return;
        };
        let g = &s.config.theme.glyphs;
        let ell = g.ellipsis;
        let domain = {
            let mut counts: Vec<(String, usize)> = Vec::new();
            for a in &m.all {
                if let Some(d) = crate::address::domain(&a.email) {
                    match counts.iter_mut().find(|(x, _)| *x == d) {
                        Some((_, n)) => *n += 1,
                        None => counts.push((d, 1)),
                    }
                }
            }
            counts.sort_by_key(|c| std::cmp::Reverse(c.1));
            counts.first().map(|(d, _)| format!("mask domain: {d}"))
        };
        self.screen_top(p, "Masked Email", domain.as_deref().unwrap_or(""));
        let border = p.style(StyleName::Border);
        p.frame_box(l.frame, border, false);
        let fr = l.frame.right().saturating_sub(1);
        p.title(
            l.frame.x + 2,
            l.frame.y,
            &format!(
                "Masked Email  {} {}",
                m.all.len(),
                if m.all.len() == 1 {
                    "address"
                } else {
                    "addresses"
                }
            ),
            p.style(StyleName::PaneTitleFocused),
            fr,
        );
        let inner = inner_of(l.frame);
        if inner.height < 6 || inner.width < 30 {
            return;
        }
        let max = inner.right().saturating_sub(1);
        let now = s.now_zoned();
        // Counts.
        let c = m.counts();
        let mut y = inner.y;
        let mut parts: Vec<Part> = Vec::new();
        for (i, st) in STATES.iter().enumerate() {
            parts.push((format!("{} {}   ", c[i], state_name(*st)), StyleName::Text));
        }
        parts.push((
            format!(
                "     {} silent for 6+ months   {} got spam",
                m.silent(s.clock.now()),
                m.with_spam()
            ),
            StyleName::Dim,
        ));
        self.parts(p, inner.x + 1, y, &parts, max);
        // State filters.
        y += 1;
        let mut x = p.put(inner.x + 1, y, "Show ", p.style(StyleName::Dim), max);
        for (i, st) in STATES.iter().enumerate() {
            let action = [
                Action::MaskedShowEnabled,
                Action::MaskedShowDisabled,
                Action::MaskedShowPending,
                Action::MaskedShowDeleted,
            ][i];
            let start = x;
            x = p.put(
                x,
                y,
                &format!("{}", i + 1),
                p.style(StyleName::HintKey),
                max,
            );
            let mark = if m.show[i] { g.ok } else { g.fail };
            x = p.put(
                x,
                y,
                &format!(" {mark} {}  ", state_name(*st)),
                p.style(if m.show[i] {
                    StyleName::Text
                } else {
                    StyleName::Dim
                }),
                max,
            );
            p.hits.push(
                Rect::new(start, y, x.saturating_sub(start), 1),
                HitTarget::Action(action),
            );
        }
        // The silent and created-by filters, each with its key, then the
        // sort while it fits.
        let by = m
            .created_by
            .as_deref()
            .map_or_else(|| "any".to_owned(), |c| sanitize(c).into_owned());
        let mut x = x + 3;
        for (action, label, value, set) in [
            (
                Action::MaskedCycleSilent,
                "silent",
                m.silent.name().to_owned(),
                m.silent != SilentFilter::Any,
            ),
            (
                Action::MaskedCycleCreatedBy,
                "created by",
                by,
                m.created_by.is_some(),
            ),
        ] {
            let start = x;
            if let Some(k) = self.keys_for(action) {
                x = p.put(x, y, &format!("{k} "), p.style(StyleName::HintKey), max);
            }
            x = p.put(
                x,
                y,
                &format!("{label}: {value}"),
                p.style(if set { StyleName::Text } else { StyleName::Dim }),
                max,
            );
            if x > start {
                p.hits.push(
                    Rect::new(start, y, x.saturating_sub(start), 1),
                    HitTarget::Action(action),
                );
            }
            x = p.put(x, y, "   ", p.style(StyleName::Text), max);
        }
        let sort = "sort: last message";
        if x + width(sort) as u16 <= max {
            p.put(x, y, sort, p.style(StyleName::Dim), max);
        }
        // Filter.
        y += 1;
        let fx = p.put(inner.x + 1, y, "/ ", p.style(StyleName::PopupTitle), max);
        let note = "default view hides pending and deleted, as Fastmail does";
        let note_fits = max.saturating_sub(width(note) as u16) > fx + 40;
        let note_x = if note_fits {
            max.saturating_sub(width(note) as u16)
        } else {
            max
        };
        if m.filter.is_empty() && !m.filtering {
            p.put(
                fx,
                y,
                "search address, description or site",
                p.style(StyleName::Dim),
                note_x.saturating_sub(1),
            );
        } else {
            let room = usize::from(note_x.saturating_sub(fx + 2));
            let (t, cx) = m.filter.view(room.max(1));
            p.put(fx, y, &t, p.style(StyleName::Text), note_x);
            if m.filtering {
                let pos = Position::new(fx + cx as u16, y);
                p.patch(
                    Rect::new(pos.x, pos.y, 1, 1),
                    p.style(StyleName::Text)
                        .add_modifier(ratatui::style::Modifier::REVERSED),
                );
                p.cursor = Some(pos);
            }
        }
        if note_fits {
            p.put(note_x, y, note, p.style(StyleName::Dim), max + 1);
        }
        y += 1;
        p.hseparator(l.frame.x, fr, y, border);
        // Columns.
        let wide = inner.width >= 110;
        let cols = MaskedCols::new(inner.width, wide);
        y += 1;
        self.masked_header(p, &cols, inner.x, y, max);
        let detail_h: u16 = 11u16.min(inner.bottom().saturating_sub(y + 1) / 2);
        let show_detail = detail_h >= 4;
        let list_bottom = if show_detail {
            inner.bottom().saturating_sub(detail_h)
        } else {
            inner.bottom()
        };
        let first = y + 1;
        let rows = usize::from(list_bottom.saturating_sub(first));
        let offset = m.cursor.saturating_sub(rows.saturating_sub(1));
        for (k, &i) in m.shown.iter().enumerate().skip(offset).take(rows) {
            let Some(a) = m.all.get(i) else {
                continue;
            };
            let ry = first + (k - offset) as u16;
            let line = Rect::new(inner.x, ry, inner.width, 1);
            p.hits.push(line, HitTarget::Row(k));
            if k == m.cursor {
                p.put(
                    inner.x,
                    ry,
                    g.collapsed,
                    p.style(StyleName::ReaderMarker),
                    max,
                );
            }
            let (glyph, style) = state_glyph(a.state, g);
            let mut x = inner.x + 1;
            x = p.put(
                x,
                ry,
                &fit(&format!("{glyph} {}", state_name(a.state)), cols.state, ell),
                p.style(style),
                max,
            );
            x = p.put(
                x,
                ry,
                &fit(&sanitize(&a.email), cols.address, ell),
                p.style(StyleName::ReaderMasked),
                max,
            );
            let desc = if a.description.trim().is_empty() {
                "(no description)".to_owned()
            } else {
                sanitize(&a.description).into_owned()
            };
            x = p.put(
                x,
                ry,
                &fit(&desc, cols.desc, ell),
                p.style(StyleName::Text),
                max,
            );
            if cols.site > 0 {
                x = p.put(
                    x,
                    ry,
                    &fit(&sanitize(&a.for_domain), cols.site, ell),
                    p.style(StyleName::Dim),
                    max,
                );
            }
            x = p.put(
                x,
                ry,
                &rfit(&last_seen(a.last_message_at, &now), 6, ell),
                p.style(StyleName::ListDate),
                max,
            );
            x = p.put(x, ry, "  ", p.style(StyleName::Text), max);
            if cols.by > 0 {
                x = p.put(
                    x,
                    ry,
                    &fit(&sanitize(&a.created_by), cols.by, ell),
                    p.style(StyleName::Dim),
                    max,
                );
            }
            x = p.put(
                x,
                ry,
                &rfit(&a.local_messages.to_string(), 5, ell),
                p.style(StyleName::Text),
                max,
            );
            if a.local_in_spam > 0 {
                p.put(x + 4, ry, "!", p.style(StyleName::StatusWarning), max);
            }
            if k == m.cursor && !m.filtering {
                p.patch(line, p.style(StyleName::ListCursor));
                p.cursor = Some(Position::new(inner.x, ry));
            }
        }
        if m.shown.is_empty() && first < list_bottom {
            p.put(
                inner.x + 2,
                first,
                if m.all.is_empty() {
                    "No masked addresses yet. n makes one for a site."
                } else {
                    "Nothing matches. Clear the search, show more states, or set silent and created by to any."
                },
                p.style(StyleName::Dim),
                max,
            );
        }
        if show_detail && let Some(a) = m.selected() {
            self.masked_detail(p, m, a, inner, list_bottom, fr);
        }
        // The silent and created-by filters again, as a narrow screen cuts
        // them off the filter row.
        let mut status = format!("{} of {} shown", m.shown.len(), m.all.len());
        if m.silent != SilentFilter::Any {
            status.push_str(&format!(", {} only", m.silent.name()));
        }
        if let Some(c) = &m.created_by {
            status.push_str(&format!(", created by {}", sanitize(c)));
        }
        let left = vec![DocSpan::new(status, StyleName::StatusText)];
        self.status_line(p, Some(left), Vec::new());
        if let Some(r) = l.hints {
            self.hint_bar(p, r);
        }
    }

    fn masked_header(&self, p: &mut Painter<'_>, c: &MaskedCols, x: u16, y: u16, max: u16) {
        let ell = self.state.config.theme.glyphs.ellipsis;
        let mut text = String::from(" ");
        text.push_str(&fit("state", c.state, ell));
        text.push_str(&fit("address", c.address, ell));
        text.push_str(&fit("description", c.desc, ell));
        if c.site > 0 {
            text.push_str(&fit("site", c.site, ell));
        }
        text.push_str(&rfit("last", 6, ell));
        text.push_str("  ");
        if c.by > 0 {
            text.push_str(&fit("created by", c.by, ell));
        }
        text.push_str(&rfit("mail", 5, ell));
        p.put(x, y, &text, p.style(StyleName::Dim), max);
    }

    fn masked_detail(
        &self,
        p: &mut Painter<'_>,
        m: &MaskedScreen,
        a: &crate::data::MaskedEmailView,
        inner: Rect,
        top: u16,
        fr: u16,
    ) {
        let s = self.state;
        let g = &s.config.theme.glyphs;
        let ell = g.ellipsis;
        let border = p.style(StyleName::Border);
        let l = self.layout;
        p.hseparator(l.frame.x, fr, top, border);
        p.title(
            l.frame.x + 2,
            top,
            &sanitize(&a.email),
            p.style(StyleName::ReaderMasked),
            fr,
        );
        let max = inner.right().saturating_sub(1);
        let now = s.now_zoned();
        let label = |t: &str| (format!("  {}", fit(t, 10, ell)), StyleName::Dim);
        let (glyph, style) = state_glyph(a.state, g);
        let desc = if a.description.trim().is_empty() {
            "(no description)".to_owned()
        } else {
            sanitize(&a.description).into_owned()
        };
        let created = a
            .created_at
            .map(|t| {
                t.to_zoned(now.time_zone().clone())
                    .strftime("%Y-%m-%d")
                    .to_string()
            })
            .unwrap_or_else(|| "unknown".into());
        let senders = m
            .senders
            .as_ref()
            .filter(|x| x.id == a.id)
            .map(|x| x.senders.clone())
            .unwrap_or_default();
        let site = a.for_domain.to_lowercase();
        let off_site = senders
            .iter()
            .filter(|(e, _)| {
                let d = crate::address::domain(e).unwrap_or_default();
                site.is_empty() || !(d == site || d.ends_with(&format!(".{site}")))
            })
            .count();
        let mut lines: Vec<Vec<Part>> = vec![
            vec![
                label("name"),
                (format!("{desc}    "), StyleName::ReaderSender),
                (format!("{glyph} {}", state_name(a.state)), style),
            ],
            vec![
                label("site"),
                (
                    format!("https://{}", sanitize(&a.for_domain)),
                    StyleName::Text,
                ),
                (
                    format!(
                        "    created {created} by {}    last mail {}",
                        sanitize(&a.created_by),
                        last_seen(a.last_message_at, &now)
                    ),
                    StyleName::Dim,
                ),
            ],
            vec![
                label("mail"),
                (
                    format!(
                        "{} in the local index, {} in Spam",
                        super::plural(a.local_messages as usize, "message"),
                        a.local_in_spam
                    ),
                    StyleName::Text,
                ),
            ],
            vec![
                label("senders"),
                (
                    if senders.is_empty() {
                        "none in the local index".to_owned()
                    } else {
                        senders
                            .iter()
                            .map(|(e, n)| format!("{} ({n})", sanitize(e)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    },
                    StyleName::Text,
                ),
            ],
            if senders.is_empty() {
                vec![
                    label("check"),
                    ("nothing to check yet".into(), StyleName::Dim),
                ]
            } else if off_site == 0 {
                vec![
                    label("check"),
                    (
                        format!("{} every sender matches the site", g.ok),
                        StyleName::StatusOk,
                    ),
                ]
            } else {
                vec![
                    label("check"),
                    (
                        format!("! {} not from the site", super::plural(off_site, "sender")),
                        StyleName::StatusWarning,
                    ),
                ]
            },
            Vec::new(),
        ];
        let mut keys: Vec<Part> = vec![("  ".into(), StyleName::Text)];
        for (action, label) in [
            (Action::MaskedEnable, "enable"),
            (Action::MaskedDisable, "disable"),
            (Action::MaskedDelete, "delete"),
            (Action::MaskedRename, "rename"),
            (Action::MaskedMessages, "show messages"),
            (Action::MaskedCopy, "copy address"),
            (Action::MaskedRule, "rule for this address"),
        ] {
            let k = self.key_hint_parts(action, label);
            if !k.is_empty() {
                keys.extend(k);
                keys.push(("   ".into(), StyleName::Text));
            }
        }
        lines.push(keys);
        lines.push(Vec::new());
        lines.push(vec![(
            "  Disabled masks keep accepting mail and file it in Trash. Deleted masks bounce. Both can be set back to enabled."
                .into(),
            StyleName::Dim,
        )]);
        for (i, parts) in lines.iter().enumerate() {
            let y = top + 1 + i as u16;
            if y >= inner.bottom() {
                break;
            }
            let mut x = inner.x;
            for (t, st) in parts {
                let room = usize::from(max.saturating_sub(x));
                let (t, _) = truncate(t, room, ell);
                x = p.put(x, y, &t, p.style(*st), max);
            }
        }
    }

    // ----- Rules -----

    pub(super) fn rules(&self, p: &mut Painter<'_>) {
        let s = self.state;
        let l = self.layout;
        let Some(r) = s.rules.as_deref() else {
            return;
        };
        let g = &s.config.theme.glyphs;
        let ell = g.ellipsis;
        let parsed = r.parsed();
        let block = render_block(&parsed).ok();
        let copies = r.renders.len();
        let last = r.renders.first();
        let current_sha = block.as_ref().map(|b| b.body_sha256.clone());
        let changed_since = last.is_none_or(|x| Some(&x.body_sha256) != current_sha.as_ref());
        let version = if changed_since {
            copies + 1
        } else {
            copies.max(1)
        };
        let changed_rules: Vec<bool> = r
            .rules
            .iter()
            .map(|e| match (e, last, &block) {
                (RuleEntry::Rule(rule), Some(last), Some(b)) => b
                    .rules
                    .iter()
                    .find(|x| x.id == rule.id)
                    .map(|x| {
                        let lines: Vec<&str> = b.text.lines().collect();
                        let start = x.lines.start().saturating_sub(1);
                        let end = (*x.lines.end()).min(lines.len());
                        let piece = lines
                            .get(start..end)
                            .map(|v| v.join("\n"))
                            .unwrap_or_default();
                        !last.sieve.contains(&piece)
                    })
                    .unwrap_or(true),
                (RuleEntry::Rule(_), None, _) => true,
                _ => false,
            })
            .collect();
        let changes = changed_rules.iter().filter(|c| **c).count();
        let now = s.now_zoned();
        let when = |t: jiff::Timestamp| {
            t.to_zoned(now.time_zone().clone())
                .strftime("%b %-d")
                .to_string()
        };
        let right = match last {
            Some(x) => format!(
                "mejl block v{version}   last copied v{copies}, {}",
                when(x.rendered_at)
            ),
            None => format!("mejl block v{version}   never copied"),
        };
        self.screen_top(p, "Rules (Sieve)", &right);
        let border = p.style(StyleName::Border);
        p.frame_box(l.frame, border, false);
        let fr = l.frame.right().saturating_sub(1);
        p.title(
            l.frame.x + 2,
            l.frame.y,
            "Rules",
            p.style(StyleName::PaneTitleFocused),
            fr,
        );
        let inner = inner_of(l.frame);
        if inner.height < 8 || inner.width < 30 {
            return;
        }
        let max = inner.right().saturating_sub(1);
        // Banner.
        let x = p.put(
            inner.x + 1,
            inner.y,
            "! ",
            p.style(StyleName::StatusWarning),
            max,
        );
        p.put(
            x,
            inner.y,
            "Paste required. Fastmail has no public API for rules or Sieve, so mejl cannot write or read your server script.",
            p.style(StyleName::Text),
            max,
        );
        let copy_key = self.keys_for(Action::RuleCopyBlock).unwrap_or_default();
        let second = match last {
            None => format!(
                "  The block has not been copied yet. Press {copy_key} to copy it, then paste it in Settings > Filters & Rules."
            ),
            Some(x) if changed_since => format!(
                "  The mejl block changed since you last copied it (v{copies}, {}, {}). Press {copy_key} to copy it and paste it in Settings > Filters & Rules.",
                when(x.rendered_at),
                super::plural(changes, "change")
            ),
            Some(_) => format!(
                "  Fastmail should have the current block (v{copies}). Press {} to paste your script back and check.",
                self.keys_for(Action::RulePasteCheck).unwrap_or_default()
            ),
        };
        let (t, _) = truncate(&second, usize::from(max.saturating_sub(inner.x)), ell);
        p.put(inner.x, inner.y + 1, &t, p.style(StyleName::Text), max);
        let sep_y = inner.y + 2;
        p.hseparator(l.frame.x, fr, sep_y, border);
        let two = inner.width >= 110;
        let left_w: u16 = if two { 39 } else { inner.width };
        let split_x = inner.x + left_w;
        if two {
            p.separator(split_x, sep_y, l.frame.bottom().saturating_sub(1), border);
        }
        p.title(
            l.frame.x + 2,
            sep_y,
            &format!("mejl rules ({})", r.rules.len()),
            p.style(if r.pane == RulesPane::List {
                StyleName::PaneTitleFocused
            } else {
                StyleName::PaneTitle
            }),
            split_x,
        );
        let body = Rect::new(
            inner.x,
            sep_y + 1,
            left_w,
            inner.bottom().saturating_sub(sep_y + 1),
        );
        if two || r.pane == RulesPane::List {
            self.rules_left(
                p,
                r,
                &changed_rules,
                body,
                copies,
                changes,
                version,
                last.map(|x| when(x.rendered_at)),
            );
        }
        if two {
            p.title(
                split_x + 2,
                sep_y,
                "Edit rule",
                p.style(if r.pane == RulesPane::Form {
                    StyleName::PaneTitleFocused
                } else {
                    StyleName::PaneTitle
                }),
                fr,
            );
            let right = Rect::new(
                split_x + 1,
                sep_y + 1,
                inner.right().saturating_sub(split_x + 1),
                body.height,
            );
            self.rules_right(p, r, block.as_ref(), right, fr);
        } else if r.pane == RulesPane::Form {
            self.rules_form(p, r, body);
        }
        let left = vec![DocSpan::new(
            match (copies, changes) {
                (0, _) => "Rules are saved locally. Copy the block to Fastmail with P.".to_owned(),
                (_, 0) => {
                    "Rules saved locally. Fastmail has them if you pasted the last copy.".to_owned()
                }
                (_, n) => format!(
                    "Rules saved locally.  {} not on Fastmail yet.",
                    super::plural(n, "change")
                ),
            },
            StyleName::StatusText,
        )];
        self.status_line(p, Some(left), Vec::new());
        if let Some(r) = l.hints {
            self.hint_bar(p, r);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rules_left(
        &self,
        p: &mut Painter<'_>,
        r: &RulesScreen,
        changed: &[bool],
        area: Rect,
        copies: usize,
        changes: usize,
        version: usize,
        last_when: Option<String>,
    ) {
        let s = self.state;
        let g = &s.config.theme.glyphs;
        let ell = g.ellipsis;
        let max = area.right().saturating_sub(1);
        let mut y = area.y + 1;
        let name_w = usize::from(area.width.saturating_sub(10));
        for (i, e) in r.rules.iter().enumerate() {
            if y >= area.bottom() {
                break;
            }
            let line = Rect::new(area.x, y, area.width, 1);
            p.hits.push(line, HitTarget::Row(i));
            if i == r.cursor {
                p.put(
                    area.x,
                    y,
                    g.collapsed,
                    p.style(StyleName::ReaderMarker),
                    max,
                );
            }
            match e {
                RuleEntry::Rule(rule) => {
                    let (glyph, st) = if rule.enabled {
                        (g.ok, StyleName::StatusOk)
                    } else {
                        (g.fail, StyleName::Dim)
                    };
                    let x = p.put(area.x + 2, y, glyph, p.style(st), max);
                    p.put(
                        x + 1,
                        y,
                        &fit(&sanitize(&rule.name), name_w, ell),
                        p.style(StyleName::Text),
                        max,
                    );
                    let flag = if !rule.enabled {
                        "off"
                    } else if changed.get(i).copied().unwrap_or(false) {
                        "*"
                    } else {
                        ""
                    };
                    p.put(
                        area.x + 4 + name_w as u16,
                        y,
                        &rfit(flag, 3, ell),
                        p.style(StyleName::StatusWarning),
                        max,
                    );
                }
                RuleEntry::Broken { id, .. } => {
                    p.put(
                        area.x + 2,
                        y,
                        &fit(
                            &format!("! {} cannot be read", sanitize(id)),
                            usize::from(area.width.saturating_sub(3)),
                            ell,
                        ),
                        p.style(StyleName::StatusError),
                        max,
                    );
                }
            }
            if i == r.cursor && r.pane == RulesPane::List {
                p.patch(line, p.style(StyleName::ListCursor));
                p.cursor = Some(Position::new(area.x, y));
            }
            y += 1;
        }
        if r.rules.is_empty() {
            p.put(area.x + 2, y, "No rules yet.", p.style(StyleName::Dim), max);
            y += 1;
        }
        if let Some(RuleEntry::Broken { error, .. }) = r.selected() {
            let (t, _) = truncate(
                &format!("  {}", sanitize(error)),
                usize::from(area.width),
                ell,
            );
            p.put(area.x, y, &t, p.style(StyleName::StatusError), max);
            y += 1;
        }
        y += 1;
        if y < area.bottom() {
            p.put(
                area.x + 2,
                y,
                "* changed since last copy",
                p.style(StyleName::Dim),
                max,
            );
            y += 1;
        }
        if y < area.bottom() {
            let mut parts: Vec<Part> = vec![("  ".into(), StyleName::Text)];
            for (a, label) in [
                (Action::RuleNew, "new"),
                (Action::RuleDown, ""),
                (Action::RuleToggle, "on/off"),
            ] {
                if a == Action::RuleDown {
                    if let (Some(d), Some(u)) = (
                        self.keys_for(Action::RuleDown),
                        self.keys_for(Action::RuleUp),
                    ) {
                        parts.push((format!("{d}/{u}"), StyleName::HintKey));
                        parts.push((" order  ".into(), StyleName::HintLabel));
                    }
                    continue;
                }
                let k = self.key_hint_parts(a, label);
                if !k.is_empty() {
                    parts.extend(k);
                    parts.push(("  ".into(), StyleName::Text));
                }
            }
            self.parts(p, area.x, y, &parts, max);
            y += 1;
        }
        let border = p.style(StyleName::Border);
        let l = self.layout;
        let section = |p: &mut Painter<'_>, y: u16, title: &str| {
            if area.width < l.frame.width.saturating_sub(4) {
                p.hseparator(l.frame.x, area.right(), y, border);
            } else {
                p.hseparator(l.frame.x, l.frame.right().saturating_sub(1), y, border);
            }
            p.title(
                l.frame.x + 2,
                y,
                title,
                p.style(StyleName::PaneTitle),
                area.right(),
            );
        };
        y += 1;
        let ui_lines = [
            "Not readable by mejl: there is no",
            "public rules API. Edit them in",
            "Fastmail: Settings > Filters & Rules",
        ];
        if y + 5 < area.bottom() {
            section(p, y, "Fastmail UI rules");
            y += 1;
            for t in ui_lines {
                p.put(area.x + 2, y, t, p.style(StyleName::Dim), max);
                y += 1;
            }
            y += 1;
        }
        // Where the block runs: a title, the order and two notes, shown
        // only when Paste status still fits below it (at 160x40 and up).
        let order_h = BLOCK_ORDER.len() as u16 + 3;
        let paste_h = 7;
        if y + order_h + paste_h <= area.bottom() {
            section(p, y, "Where the block runs");
            y += 1;
            for (part, kind, mejl) in BLOCK_ORDER {
                if mejl {
                    p.put(
                        area.x,
                        y,
                        g.collapsed,
                        p.style(StyleName::ReaderMarker),
                        max,
                    );
                }
                let (name_style, kind_style) = if mejl {
                    (StyleName::PaneTitleFocused, StyleName::PaneTitleFocused)
                } else {
                    (StyleName::Text, StyleName::Dim)
                };
                let x = p.put(area.x + 2, y, &fit(part, 23, ell), p.style(name_style), max);
                p.put(x, y, kind, p.style(kind_style), max);
                y += 1;
            }
            for note in [
                "Paste below spam protection.",
                "Order from samples, unconfirmed.",
            ] {
                p.put(area.x + 2, y, note, p.style(StyleName::Dim), max);
                y += 1;
            }
        }
        if y + 3 < area.bottom() {
            section(p, y, "Paste status");
            y += 1;
            let on_fastmail = match (&r.paste, r.renders.first()) {
                (Some((true, _)), _) => {
                    ("matches the current block".to_owned(), StyleName::StatusOk)
                }
                (Some((false, t)), _) => (t.clone(), StyleName::StatusWarning),
                (None, Some(x)) if x.confirmed_at.is_some() => (
                    format!("matched v{copies} when you checked"),
                    StyleName::StatusOk,
                ),
                _ => ("unknown".to_owned(), StyleName::Dim),
            };
            let rows: Vec<Vec<Part>> = vec![
                vec![
                    ("  local block   ".into(), StyleName::Dim),
                    (
                        format!("v{version}, {}", super::plural(changes, "change")),
                        StyleName::Text,
                    ),
                ],
                vec![
                    ("  last copied   ".into(), StyleName::Dim),
                    (
                        match &last_when {
                            Some(w) => format!("v{copies}, {w}"),
                            None => "never".into(),
                        },
                        StyleName::Text,
                    ),
                ],
                vec![("  on Fastmail   ".into(), StyleName::Dim), on_fastmail],
                Vec::new(),
                {
                    let mut v: Vec<Part> = vec![("  ".into(), StyleName::Text)];
                    v.extend(self.key_hint_parts(
                        Action::RulePasteCheck,
                        " paste the section back to check",
                    ));
                    v
                },
                {
                    let mut v: Vec<Part> = vec![("  ".into(), StyleName::Text)];
                    v.extend(
                        self.key_hint_parts(Action::RuleCopyBlock, " copy block for Fastmail"),
                    );
                    v
                },
            ];
            for parts in rows {
                if y >= area.bottom() {
                    break;
                }
                let mut x = area.x;
                for (t, st) in &parts {
                    let (t, _) = truncate(t, usize::from(max.saturating_sub(x)), ell);
                    x = p.put(x, y, &t, p.style(*st), max);
                }
                y += 1;
            }
        }
    }

    fn rules_form(&self, p: &mut Painter<'_>, r: &RulesScreen, area: Rect) -> u16 {
        let s = self.state;
        let g = &s.config.theme.glyphs;
        let ell = g.ellipsis;
        let max = area.right().saturating_sub(1);
        let Some(rule) = r.selected_rule() else {
            let msg = match r.selected() {
                Some(RuleEntry::Broken { .. }) => {
                    "This rule cannot be read. D deletes it; the other rules still render."
                }
                _ => "No rule selected. n makes a new one.",
            };
            p.put(area.x + 1, area.y, msg, p.style(StyleName::Dim), max);
            return area.y + 1;
        };
        let rows = form_rows(rule);
        let mut y = area.y;
        let label_w = 9usize;
        for (i, row) in rows.iter().enumerate() {
            if y >= area.bottom() {
                break;
            }
            let label = match row {
                FormRow::Name => "Name",
                FormRow::Enabled => "State",
                FormRow::Match => "When",
                FormRow::Action(0) => "Then",
                FormRow::AddAction if rule.actions.is_empty() => "Then",
                FormRow::Stop => "Stop",
                _ => "",
            };
            let (value, style): (String, StyleName) = match row {
                FormRow::Name => (sanitize(&rule.name).into_owned(), StyleName::ReaderSender),
                FormRow::Enabled => {
                    if rule.enabled {
                        (format!("{} enabled", g.ok), StyleName::StatusOk)
                    } else {
                        (format!("{} off", g.fail), StyleName::Dim)
                    }
                }
                FormRow::Match => (
                    match rule.match_mode {
                        mejl_sieve::MatchMode::Any => "any of these match",
                        mejl_sieve::MatchMode::All => "all of these match",
                    }
                    .into(),
                    StyleName::Text,
                ),
                FormRow::Condition(c) => (
                    rule.conditions
                        .get(*c)
                        .map(|c| sanitize(&condition_text(c)).into_owned())
                        .unwrap_or_default(),
                    StyleName::Text,
                ),
                FormRow::AddCondition => (
                    "+ add condition   from, to, subject, header, list-id, delivered-to, size"
                        .into(),
                    StyleName::Dim,
                ),
                FormRow::Action(a) => (
                    rule.actions
                        .get(*a)
                        .map(|a| sanitize(&action_text(a)).into_owned())
                        .unwrap_or_default(),
                    StyleName::Text,
                ),
                FormRow::AddAction => (
                    "+ add action   move to, label, mark read, pin, trash".into(),
                    StyleName::Dim,
                ),
                FormRow::Stop => {
                    if rule.stop {
                        (
                            "on   ! stop here also skips Fastmail UI rules, notifications, calendar handling and vacation".into(),
                            StyleName::StatusWarning,
                        )
                    } else {
                        ("off".into(), StyleName::Text)
                    }
                }
            };
            let indent = if matches!(row, FormRow::Condition(_) | FormRow::AddCondition) {
                2
            } else {
                0
            };
            let line = Rect::new(area.x, y, area.width, 1);
            p.hits.push(line, HitTarget::Field(i));
            p.put(
                area.x + 1,
                y,
                &fit(label, label_w, ell),
                p.style(StyleName::Dim),
                max,
            );
            let vx = area.x + 1 + label_w as u16 + indent;
            let (t, _) = truncate(&value, usize::from(max.saturating_sub(vx)), ell);
            p.put(vx, y, &t, p.style(style), max);
            if i == r.form_cursor && r.pane == RulesPane::Form {
                p.patch(line, p.style(StyleName::ListCursor));
                p.cursor = Some(Position::new(area.x, y));
            }
            y += 1;
        }
        // Checks.
        if y < area.bottom() {
            let mailboxes: Vec<String> = s
                .mailboxes
                .iter()
                .map(|m| m.id.as_str().to_owned())
                .collect();
            let diags = validate(
                std::slice::from_ref(rule),
                &ValidationContext::with_mailboxes(mailboxes),
            );
            let script_ok = render_block(std::slice::from_ref(rule))
                .map(|b| check_script(&b.text).is_ok())
                .unwrap_or(false);
            let mut parts: Vec<Part> = vec![(" Checks   ".into(), StyleName::Dim)];
            if script_ok {
                parts.push((
                    format!(
                        "{} parses   {} only allowed extensions   {} no keep   {} no discard",
                        g.ok, g.ok, g.ok, g.ok
                    ),
                    StyleName::StatusOk,
                ));
            } else {
                parts.push((
                    format!("{} the rule does not render", g.fail),
                    StyleName::StatusError,
                ));
            }
            for d in diags.iter().take(2) {
                let (mark, st) = match d.severity {
                    mejl_sieve::Severity::Error => (g.fail, StyleName::StatusError),
                    mejl_sieve::Severity::Warning => ("!", StyleName::StatusWarning),
                };
                parts.push((format!("   {mark} {}", sanitize(&d.message)), st));
            }
            let mut x = area.x;
            for (t, st) in &parts {
                let (t, _) = truncate(t, usize::from(max.saturating_sub(x)), ell);
                x = p.put(x, y, &t, p.style(*st), max);
            }
            y += 1;
        }
        y
    }

    fn rules_right(
        &self,
        p: &mut Painter<'_>,
        r: &RulesScreen,
        block: Option<&mejl_sieve::RenderedBlock>,
        area: Rect,
        fr: u16,
    ) {
        let s = self.state;
        let g = &s.config.theme.glyphs;
        let ell = g.ellipsis;
        let border = p.style(StyleName::Border);
        let form_end = self.rules_form(
            p,
            r,
            Rect::new(area.x, area.y, area.width, area.height.min(14)),
        );
        let sep = (form_end + 1).max(area.y + 4);
        if sep + 3 >= area.bottom() {
            return;
        }
        p.hseparator(area.x.saturating_sub(1), fr, sep, border);
        // The left column's separator meets this one.
        p.line_cell(
            area.x.saturating_sub(1),
            sep,
            s.config.theme.lines.tees[2],
            border,
            false,
        );
        let both = area.width >= 110;
        let preview_w = if both {
            60u16.min(area.width)
        } else {
            area.width
        };
        let show_dry = r
            .dry_run
            .as_ref()
            .filter(|d| Some(d.rule_id.as_str()) == r.selected().map(RuleEntry::id));
        let lines: Vec<&str> = block.map(|b| b.text.lines().collect()).unwrap_or_default();
        let mut preview_title = "Sieve preview, read-only".to_owned();
        if !lines.is_empty() {
            preview_title.push_str(&format!(" ── {} lines", lines.len()));
        }
        let body_top = sep + 1;
        let body_h = usize::from(area.bottom().saturating_sub(body_top));
        let draw_preview = both || show_dry.is_none();
        if draw_preview {
            p.title(
                area.x + 1,
                sep,
                &preview_title,
                p.style(StyleName::PaneTitle),
                area.x + preview_w,
            );
            let (lo, hi) = block
                .and_then(|b| {
                    b.rules
                        .iter()
                        .find(|x| Some(x.id.as_str()) == r.selected().map(RuleEntry::id))
                })
                .map_or((0, 0), |x| (*x.lines.start(), *x.lines.end()));
            let scroll = hi.saturating_sub(body_h);
            let max = area.x + preview_w.saturating_sub(1);
            for (k, line) in lines.iter().enumerate().skip(scroll).take(body_h) {
                let y = body_top + (k - scroll) as u16;
                let n = k + 1;
                let mark = if n >= lo && n <= hi && lo > 0 {
                    g.collapsed
                } else {
                    " "
                };
                p.put(area.x, y, mark, p.style(StyleName::ReaderMarker), max);
                let (t, _) = truncate(
                    &indented(line),
                    usize::from(preview_w.saturating_sub(2)),
                    ell,
                );
                p.put(area.x + 1, y, &t, p.style(StyleName::ReaderCode), max);
            }
            if lines.is_empty() {
                p.put(
                    area.x + 1,
                    body_top,
                    "Nothing to render: a rule has an error.",
                    p.style(StyleName::StatusError),
                    max,
                );
            }
        }
        let (dx, dw) = if both {
            (
                area.x + preview_w + 1,
                area.width.saturating_sub(preview_w + 1),
            )
        } else {
            (area.x, area.width)
        };
        if both {
            p.separator(
                area.x + preview_w,
                sep,
                self.layout.frame.bottom().saturating_sub(1),
                border,
            );
        }
        if !both && show_dry.is_none() {
            return;
        }
        let max = dx + dw.saturating_sub(1);
        p.title(dx + 1, sep, "Dry run", p.style(StyleName::PaneTitle), fr);
        let mut y = body_top;
        let mut put = |p: &mut Painter<'_>, parts: &[Part]| {
            if y >= area.bottom() {
                return;
            }
            let mut x = dx + 1;
            for (t, st) in parts {
                let (t, _) = truncate(t, usize::from(max.saturating_sub(x)), ell);
                x = p.put(x, y, &t, p.style(*st), max);
            }
            y += 1;
        };
        match show_dry {
            None => {
                let mut v: Vec<Part> =
                    self.key_hint_parts(Action::RuleDryRun, " try this rule against cached mail");
                if v.is_empty() {
                    v.push(("Try this rule against cached mail".into(), StyleName::Dim));
                }
                put(p, &v);
            }
            Some(d) => {
                let name = r
                    .selected_rule()
                    .map(|x| x.name.clone())
                    .unwrap_or_default();
                put(
                    p,
                    &[(
                        format!("{} against {} cached messages", sanitize(&name), d.scanned),
                        StyleName::Text,
                    )],
                );
                put(
                    p,
                    &[("local mejl matcher, newest first".into(), StyleName::Dim)],
                );
                put(
                    p,
                    &[(
                        format!(
                            "{} would match   {} already in place",
                            d.plans.len(),
                            d.already
                        ),
                        StyleName::ReaderSender,
                    )],
                );
                put(p, &[]);
                let now = s.now_zoned();
                for h in &d.hits {
                    let date = crate::dates::list_date(h.date, &now);
                    let text = format!(
                        "{}{}{} -> {}",
                        fit(&date, 7, ell),
                        fit(&format!("{} ", sanitize(&h.from)), 17, ell),
                        fit(
                            &sanitize(&h.subject),
                            usize::from(dw.saturating_sub(39)).max(8),
                            ell
                        ),
                        sanitize(&h.target)
                    );
                    put(p, &[(text, StyleName::Text)]);
                }
                if d.plans.len() > d.hits.len() {
                    put(
                        p,
                        &[(
                            format!("{} {} more", ell, d.plans.len() - d.hits.len()),
                            StyleName::Dim,
                        )],
                    );
                }
                put(p, &[]);
                let mut v = self.key_hint_parts(
                    Action::RuleApply,
                    &format!(" apply to these {} now (undo with z)", d.plans.len()),
                );
                if v.is_empty() {
                    v.push(("".into(), StyleName::Text));
                }
                put(p, &v);
                put(p, &[]);
                for line in crate::text::wrap(
                    "Simulates the mejl block only, using stored headers. Fastmail's generated \
                     blocks (spam, masked email, UI rules) are not simulated.",
                    usize::from(dw.saturating_sub(3)),
                ) {
                    put(p, &[(line, StyleName::Dim)]);
                }
            }
        }
    }

    // ----- Settings -----

    pub(super) fn settings(&self, p: &mut Painter<'_>) {
        let s = self.state;
        let l = self.layout;
        let Some(st) = s.settings.as_ref() else {
            return;
        };
        let ell = s.config.theme.glyphs.ellipsis;
        self.screen_top(p, "Settings", "written to the config file");
        let border = p.style(StyleName::Border);
        p.frame_box(l.frame, border, false);
        let fr = l.frame.right().saturating_sub(1);
        p.title(
            l.frame.x + 2,
            l.frame.y,
            "Settings",
            p.style(StyleName::PaneTitleFocused),
            fr,
        );
        let inner = inner_of(l.frame);
        if inner.height < 4 || inner.width < 30 {
            return;
        }
        let max = inner.right().saturating_sub(1);
        let mut y = inner.y + 1;
        let help_w = usize::from(inner.width.saturating_sub(39));
        for (i, row) in ROWS.iter().enumerate() {
            if y >= inner.bottom() {
                break;
            }
            let line = Rect::new(inner.x, y, inner.width, 1);
            p.hits.push(line, HitTarget::Field(i));
            if i == st.cursor {
                p.put(
                    inner.x,
                    y,
                    s.config.theme.glyphs.collapsed,
                    p.style(StyleName::ReaderMarker),
                    max,
                );
            }
            let mut x = p.put(
                inner.x + 2,
                y,
                &fit(row.label(), 16, ell),
                p.style(StyleName::Dim),
                max,
            );
            let mut value = row.value(&s.config);
            if st.saving == Some(*row) {
                value.push_str(", saving");
            }
            x = p.put(
                x,
                y,
                &fit(&value, 20, ell),
                p.style(StyleName::ReaderSender),
                max,
            );
            if help_w > 10 {
                p.put(
                    x,
                    y,
                    &fit(row.help(), help_w, ell),
                    p.style(StyleName::Dim),
                    max,
                );
            }
            if i == st.cursor {
                p.patch(line, p.style(StyleName::ListCursor));
                p.cursor = Some(Position::new(inner.x, y));
            }
            y += 1;
        }
        y += 1;
        let info: [(&str, String); 3] = [
            ("Layout", format!("{:?}", s.config.layout).to_lowercase()),
            (
                "Accessible mode",
                if s.config.accessible { "on" } else { "off" }.into(),
            ),
            ("Hint bar", if s.config.hints { "on" } else { "off" }.into()),
        ];
        for (label, value) in info {
            if y >= inner.bottom() {
                break;
            }
            let x = p.put(
                inner.x + 2,
                y,
                &fit(label, 16, ell),
                p.style(StyleName::Dim),
                max,
            );
            p.put(
                x,
                y,
                &format!("{value}   (set in the config file)"),
                p.style(StyleName::Dim),
                max,
            );
            y += 1;
        }
        y += 1;
        if y < inner.bottom() {
            p.put(
                inner.x + 2,
                y,
                "Changes are written to the config file and apply at once.",
                p.style(StyleName::Dim),
                max,
            );
        }
        self.status_line(p, Some(Vec::new()), Vec::new());
        if let Some(r) = l.hints {
            self.hint_bar(p, r);
        }
    }

    pub(super) fn screen_hints(&self) -> Vec<Hint> {
        let s = self.state;
        let one = |a: Action, label: &'static str, short: &'static str, important: bool| {
            self.keys_for(a).map(|k| Hint {
                keys: k,
                label,
                short,
                target: HitTarget::Action(a),
                important,
            })
        };
        let pair = |a: Action, b: Action, label: &'static str, important: bool| match (
            self.keys_for(a),
            self.keys_for(b),
        ) {
            (Some(x), Some(y)) => Some(Hint {
                keys: format!("{x}/{y}"),
                label,
                short: label,
                target: HitTarget::Action(a),
                important,
            }),
            _ => None,
        };
        let v: Vec<Option<Hint>> = match s.screen {
            Screen::Masked => {
                if s.masked.as_ref().is_some_and(|m| m.filtering) {
                    vec![
                        Some(Hint::fixed("type", "to filter", HitTarget::Popup)),
                        Some(Hint::fixed("Enter", "done", HitTarget::Popup)),
                        Some(Hint::fixed("Esc", "done", HitTarget::Popup)),
                    ]
                } else {
                    vec![
                        pair(Action::Next, Action::Prev, "move", true),
                        one(Action::Filter, "search", "search", true),
                        Some(Hint {
                            keys: "1-4".into(),
                            label: "state filter",
                            short: "filter",
                            target: HitTarget::Popup,
                            important: false,
                        }),
                        one(Action::MaskedNew, "new", "new", true),
                        one(Action::MaskedEnable, "enable", "on", true),
                        one(Action::MaskedDisable, "disable", "off", true),
                        one(Action::MaskedDelete, "delete", "del", false),
                        one(Action::MaskedRename, "rename", "rename", false),
                        one(Action::MaskedMessages, "messages", "mail", false),
                        one(Action::MaskedCopy, "copy", "copy", false),
                        one(Action::Help, "help", "help", true),
                    ]
                }
            }
            Screen::Rules => vec![
                one(Action::RuleNew, "new", "new", true),
                one(Action::Open, "edit", "edit", true),
                one(Action::RuleToggle, "on/off", "on/off", true),
                pair(Action::RuleDown, Action::RuleUp, "reorder", false),
                one(Action::RuleDryRun, "dry run", "try", true),
                one(Action::RuleApply, "apply to existing", "apply", false),
                one(
                    Action::RuleCopyBlock,
                    "copy block for Fastmail",
                    "copy",
                    true,
                ),
                one(
                    Action::RulePasteCheck,
                    "paste back to check",
                    "check",
                    false,
                ),
                one(Action::Back, "back", "back", true),
                one(Action::Help, "help", "help", true),
            ],
            _ => vec![
                pair(Action::Next, Action::Prev, "move", true),
                one(Action::SettingOpen, "change", "change", true),
                Some(Hint {
                    keys: "Left/Right".into(),
                    label: "previous/next",
                    short: "prev/next",
                    target: HitTarget::Popup,
                    important: false,
                }),
                one(Action::Back, "back", "back", true),
                one(Action::Help, "help", "help", true),
            ],
        };
        v.into_iter().flatten().collect()
    }
}

/// A Sieve line with its indentation kept and everything else
/// sanitised.
fn indented(line: &str) -> String {
    let rest = line.trim_start_matches([' ', '\t']);
    let lead: usize = line[..line.len() - rest.len()]
        .chars()
        .map(|c| if c == '\t' { 2 } else { 1 })
        .sum();
    format!("{}{}", " ".repeat(lead.min(16)), sanitize(rest))
}

fn state_glyph(state: MaskedState, g: &crate::theme::Glyphs) -> (&'static str, StyleName) {
    match state {
        MaskedState::Enabled => (g.ok, StyleName::StatusOk),
        MaskedState::Disabled => (g.fail, StyleName::StatusError),
        MaskedState::Pending => (g.unread, StyleName::Dim),
        MaskedState::Deleted => ("-", StyleName::Dim),
    }
}

/// Column widths of the masked list.
struct MaskedCols {
    state: usize,
    address: usize,
    desc: usize,
    site: usize,
    by: usize,
}

impl MaskedCols {
    fn new(inner_w: u16, wide: bool) -> MaskedCols {
        let w = usize::from(inner_w);
        if wide {
            MaskedCols {
                state: 11,
                address: 31,
                desc: 20,
                site: 23,
                by: 12,
            }
        } else {
            // The marker, state, last, gap, mail and the right margin take
            // 27 cells.
            let rest = w.saturating_sub(27);
            let address = (rest * 3 / 5).max(10);
            MaskedCols {
                state: 11,
                address,
                desc: rest.saturating_sub(address),
                site: 0,
                by: 0,
            }
        }
    }
}
