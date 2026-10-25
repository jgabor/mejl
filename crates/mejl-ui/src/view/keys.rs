//! The key editor screen: the preset line, the sections, the rows of the
//! current section grouped by category, and a footer about the focused
//! row. A macro's steps replace the rows while it is open.

use ratatui::layout::{Position, Rect};

use super::{Hint, MailView, Painter};
use crate::action::{Action, Category};
use crate::hit::HitTarget;
use crate::keymap::KeySeq;
use crate::keys_screen::{KeysScreen, Section, capitalised};
use crate::macros::Macro;
use crate::text::{fit, truncate, width};
use crate::theme::StyleName;

/// Keys a row shows before it says how many more there are.
const SHOWN_KEYS: usize = 3;
const LABEL_W: usize = 24;
const KEYS_W: usize = 24;

/// Up to three keys, then `+N` for the rest.
pub(super) fn keys_text(keys: &[KeySeq]) -> String {
    let mut parts: Vec<String> = keys
        .iter()
        .take(SHOWN_KEYS)
        .map(ToString::to_string)
        .collect();
    if keys.len() > SHOWN_KEYS {
        parts.push(format!("+{}", keys.len() - SHOWN_KEYS));
    }
    parts.join("  ")
}

/// One drawn line of the list: a category heading, or row `i`.
enum Line {
    Heading(Category),
    Row(usize),
}

impl MailView<'_> {
    pub(super) fn keys_screen(&self, p: &mut Painter<'_>) {
        let s = self.state;
        let l = self.layout;
        let Some(k) = s.keys.as_deref() else {
            return;
        };
        self.screen_top(p, "Key bindings", "written to the config file");
        let border = p.style(StyleName::Border);
        p.frame_box(l.frame, border, false);
        let fr = l.frame.right().saturating_sub(1);
        p.title(
            l.frame.x + 2,
            l.frame.y,
            "Keys",
            p.style(StyleName::PaneTitleFocused),
            fr,
        );
        let inner = Rect::new(
            l.frame.x + 1,
            l.frame.y + 1,
            l.frame.width.saturating_sub(2),
            l.frame.height.saturating_sub(2),
        );
        if inner.height < 6 || inner.width < 30 {
            return;
        }
        let max = inner.right().saturating_sub(1);
        let x0 = inner.x + 1;
        self.keys_preset_line(p, k, x0, inner.y, max);
        self.keys_sections(p, k, x0, inner.y + 1, max);
        let list = Rect::new(
            inner.x,
            inner.y + 3,
            inner.width,
            inner.height.saturating_sub(5),
        );
        let footer_y = inner.bottom().saturating_sub(2);
        let footer: [String; 2] = match &k.steps {
            Some(_) => {
                let m = k.focused_macro(&s.config);
                self.macro_steps(p, k, m.as_ref(), list);
                self.steps_footer(k, m.as_ref())
            }
            None if k.section == Section::Macros => {
                self.macro_rows(p, k, list);
                self.macro_footer(k)
            }
            None => {
                self.key_rows(p, k, list);
                self.row_footer(k)
            }
        };
        for (i, line) in footer.iter().enumerate() {
            let style = if i == 0 {
                StyleName::Text
            } else {
                StyleName::Dim
            };
            p.put(
                x0,
                footer_y + i as u16,
                &fit(
                    line,
                    usize::from(max.saturating_sub(x0)),
                    s.config.theme.glyphs.ellipsis,
                ),
                p.style(style),
                max,
            );
        }
        let left = if k.filtering || !k.filter.is_empty() {
            let mut spans = vec![
                crate::reader::DocSpan::new("Filter: ", StyleName::Dim),
                crate::reader::DocSpan::new(k.filter.text().to_owned(), StyleName::Text),
            ];
            if k.filtering {
                spans.push(crate::reader::DocSpan::new("_", StyleName::HintKey));
            }
            spans
        } else {
            Vec::new()
        };
        self.status_line(p, Some(left), Vec::new());
        if let Some(r) = l.hints {
            self.hint_bar(p, r);
        }
    }

    fn keys_preset_line(&self, p: &mut Painter<'_>, k: &KeysScreen, x: u16, y: u16, max: u16) {
        let mut x = p.put(x, y, "Preset: ", p.style(StyleName::Dim), max);
        x = p.put(x, y, &k.name, p.style(StyleName::ReaderSender), max);
        let base = k.base.preset_kind().name();
        let about = if k.custom {
            format!(" (based on {base})")
        } else {
            " (built in)".to_owned()
        };
        x = p.put(x, y, &about, p.style(StyleName::Dim), max);
        if k.saving > 0 {
            x = p.put(x, y, ", saving", p.style(StyleName::StatusWarning), max);
        }
        let user = self.state.config.user_keys.entries().len();
        if user > 0 {
            let text = format!("   [keys] changes {} on top", super::plural(user, "key"));
            p.put(x, y, &text, p.style(StyleName::Dim), max);
        }
    }

    /// The section names. When they do not all fit, the current one shows
    /// with its neighbours and its place.
    fn keys_sections(&self, p: &mut Painter<'_>, k: &KeysScreen, x: u16, y: u16, max: u16) {
        let current = Section::ALL
            .iter()
            .position(|s| *s == k.section)
            .unwrap_or(0);
        let all: usize = Section::ALL.iter().map(|s| width(s.label()) + 2).sum();
        let room = usize::from(max.saturating_sub(x));
        let shown: Vec<usize> = if all <= room {
            (0..Section::ALL.len()).collect()
        } else {
            let n = Section::ALL.len();
            vec![(current + n - 1) % n, current, (current + 1) % n]
        };
        let mut x = x;
        for i in &shown {
            let label = Section::ALL[*i].label();
            let style = if *i == current {
                StyleName::PaneTitleFocused
            } else {
                StyleName::Dim
            };
            let start = x;
            x = p.put(x, y, label, p.style(style), max);
            p.hits.push(
                Rect::new(start, y, x.saturating_sub(start), 1),
                HitTarget::Field(*i),
            );
            x = p.put(x, y, "  ", p.style(StyleName::Dim), max);
        }
        if all > room {
            p.put(
                x,
                y,
                &format!("{} of {}", current + 1, Section::ALL.len()),
                p.style(StyleName::Dim),
                max,
            );
        }
    }

    fn key_rows(&self, p: &mut Painter<'_>, k: &KeysScreen, area: Rect) {
        let s = self.state;
        let ell = s.config.theme.glyphs.ellipsis;
        if k.rows.is_empty() {
            let text = if k.filter.is_empty() {
                "Nothing to bind here."
            } else {
                "No row matches the filter."
            };
            p.put(
                area.x + 2,
                area.y,
                text,
                p.style(StyleName::Dim),
                area.right(),
            );
            return;
        }
        let mut lines = Vec::new();
        let mut last: Option<Category> = None;
        for (i, r) in k.rows.iter().enumerate() {
            let c = r.action.category();
            if last != Some(c) {
                lines.push(Line::Heading(c));
                last = Some(c);
            }
            lines.push(Line::Row(i));
        }
        let at = lines
            .iter()
            .position(|l| matches!(l, Line::Row(i) if *i == k.cursor))
            .unwrap_or(0);
        let h = usize::from(area.height).max(1);
        let start = (at + 1).saturating_sub(h);
        let max = area.right().saturating_sub(1);
        let desc_x = area.x + 2 + (LABEL_W + KEYS_W) as u16 + 2;
        let wide = area.width >= 100;
        for (n, line) in lines.iter().skip(start).take(h).enumerate() {
            let y = area.y + n as u16;
            match line {
                Line::Heading(c) => {
                    p.put(area.x + 2, y, c.label(), p.style(StyleName::ListGroup), max);
                }
                Line::Row(i) => {
                    let r = &k.rows[*i];
                    let rect = Rect::new(area.x, y, area.width, 1);
                    p.hits.push(rect, HitTarget::Row(*i));
                    if *i == k.cursor {
                        p.put(
                            area.x,
                            y,
                            s.config.theme.glyphs.collapsed,
                            p.style(StyleName::ReaderMarker),
                            max,
                        );
                    }
                    let mut x = p.put(
                        area.x + 2,
                        y,
                        &(fit(&format!("  {}", r.label()), LABEL_W - 1, ell) + " "),
                        p.style(StyleName::Text),
                        max,
                    );
                    let keys = if r.keys.is_empty() {
                        "no key".to_owned()
                    } else {
                        keys_text(&r.keys)
                    };
                    let style = if r.keys.is_empty() {
                        StyleName::Dim
                    } else {
                        StyleName::HintKey
                    };
                    x = p.put(
                        x,
                        y,
                        &(fit(&keys, KEYS_W - 1, ell) + " "),
                        p.style(style),
                        max,
                    );
                    if wide && x <= desc_x {
                        let room = usize::from(max.saturating_sub(desc_x));
                        let (t, _) = truncate(r.action.description(), room, ell);
                        p.put(desc_x, y, &t, p.style(StyleName::Dim), max);
                    }
                    if *i == k.cursor {
                        p.patch(rect, p.style(StyleName::ListCursor));
                        p.cursor = Some(Position::new(area.x, y));
                    }
                }
            }
        }
    }

    fn row_footer(&self, k: &KeysScreen) -> [String; 2] {
        let first = match k.row() {
            Some(r) if r.name == r.action.description() => r.name.clone(),
            Some(r) => format!("{}: {}", r.name, r.action.description()),
            None => String::new(),
        };
        let second = if !k.custom {
            let key = self
                .keys_for(Action::PresetDuplicate)
                .unwrap_or_else(|| "d".into());
            format!("{} is built in. Press {key} to make your own copy.", k.name)
        } else {
            match k.section {
                Section::Mail => "A change here applies to the list and the reader.".to_owned(),
                Section::Compose | Section::Search | Section::Help => {
                    "Plain keys type into the field here, so bind keys with Ctrl, Alt or F-keys."
                        .to_owned()
                }
                _ => String::new(),
            }
        };
        [first, second]
    }

    fn macro_rows(&self, p: &mut Painter<'_>, k: &KeysScreen, area: Rect) {
        let s = self.state;
        let ell = s.config.theme.glyphs.ellipsis;
        if k.macros.is_empty() {
            let new = self
                .keys_for(Action::MacroNew)
                .unwrap_or_else(|| "n".into());
            let text = if k.filter.is_empty() {
                format!("No macros yet. Press {new} to make one.")
            } else {
                "No macro matches the filter.".to_owned()
            };
            p.put(
                area.x + 2,
                area.y,
                &text,
                p.style(StyleName::Dim),
                area.right(),
            );
            return;
        }
        let h = usize::from(area.height).max(1);
        let start = (k.cursor + 1).saturating_sub(h);
        let max = area.right().saturating_sub(1);
        for (n, (i, mi)) in k.macros.iter().enumerate().skip(start).take(h).enumerate() {
            let Some(m) = s.config.macros.get(*mi) else {
                continue;
            };
            let y = area.y + n as u16;
            let rect = Rect::new(area.x, y, area.width, 1);
            p.hits.push(rect, HitTarget::Row(i));
            if i == k.cursor {
                p.put(
                    area.x,
                    y,
                    s.config.theme.glyphs.collapsed,
                    p.style(StyleName::ReaderMarker),
                    max,
                );
            }
            let mut x = p.put(
                area.x + 2,
                y,
                &(fit(&m.label, LABEL_W - 1, ell) + " "),
                p.style(StyleName::Text),
                max,
            );
            let (keys, style) = if m.keys.is_empty() {
                ("no key".to_owned(), StyleName::Dim)
            } else {
                (keys_text(&m.keys), StyleName::HintKey)
            };
            x = p.put(
                x,
                y,
                &(fit(&keys, KEYS_W - 1, ell) + " "),
                p.style(style),
                max,
            );
            let room = usize::from(max.saturating_sub(x));
            let (t, _) = truncate(&steps_summary(m), room, ell);
            p.put(x, y, &t, p.style(StyleName::Dim), max);
            if i == k.cursor {
                p.patch(rect, p.style(StyleName::ListCursor));
                p.cursor = Some(Position::new(area.x, y));
            }
        }
    }

    fn macro_footer(&self, k: &KeysScreen) -> [String; 2] {
        let s = self.state;
        let Some(m) = k.focused_macro(&s.config) else {
            return [
                "Macros change mail in one step, and one undo takes them back.".into(),
                "They work with every preset, in the list and the reader.".into(),
            ];
        };
        let warning = s
            .config
            .keymap
            .warnings()
            .iter()
            .find(|w| w.message.starts_with(&format!("macro {} ", m.label)))
            .map(|w| capitalised(&w.message));
        [
            format!("{}: {}", m.label, steps_summary(&m)),
            warning.unwrap_or_else(|| {
                "Macros work with every preset, in the list and the reader.".into()
            }),
        ]
    }

    fn macro_steps(&self, p: &mut Painter<'_>, k: &KeysScreen, m: Option<&Macro>, area: Rect) {
        let s = self.state;
        let Some(m) = m else {
            return;
        };
        let cursor = k.steps.as_ref().map_or(0, |st| st.cursor);
        let max = area.right().saturating_sub(1);
        let mut x = p.put(area.x + 2, area.y, "Macro: ", p.style(StyleName::Dim), max);
        x = p.put(x, area.y, &m.label, p.style(StyleName::ReaderSender), max);
        x = p.put(x, area.y, "   keys: ", p.style(StyleName::Dim), max);
        let keys = if m.keys.is_empty() {
            "none".to_owned()
        } else {
            keys_text(&m.keys)
        };
        p.put(x, area.y, &keys, p.style(StyleName::HintKey), max);
        if m.steps.is_empty() {
            let add = self.keys_for(Action::KeyAdd).unwrap_or_else(|| "a".into());
            p.put(
                area.x + 2,
                area.y + 2,
                &format!(
                    "No steps yet. Press {add} to add one; the macro is saved with its first step."
                ),
                p.style(StyleName::Dim),
                max,
            );
            return;
        }
        let h = usize::from(area.height.saturating_sub(2)).max(1);
        let start = (cursor + 1).saturating_sub(h);
        for (n, (i, step)) in m.steps.iter().enumerate().skip(start).take(h).enumerate() {
            let y = area.y + 2 + n as u16;
            let rect = Rect::new(area.x, y, area.width, 1);
            p.hits.push(rect, HitTarget::Row(i));
            if i == cursor {
                p.put(
                    area.x,
                    y,
                    s.config.theme.glyphs.collapsed,
                    p.style(StyleName::ReaderMarker),
                    max,
                );
            }
            p.put(
                area.x + 2,
                y,
                &format!("{:>2}. {step}", i + 1),
                p.style(StyleName::Text),
                max,
            );
            if i == cursor {
                p.patch(rect, p.style(StyleName::ListCursor));
                p.cursor = Some(Position::new(area.x, y));
            }
        }
    }

    fn steps_footer(&self, k: &KeysScreen, m: Option<&Macro>) -> [String; 2] {
        let saved = m.is_some_and(|m| k.macro_saved(&self.state.config, &m.id));
        let first = if saved {
            "Each change is saved at once. Every step acts on the same conversations.".to_owned()
        } else {
            "Not saved yet: a macro is saved once it has a step.".to_owned()
        };
        [
            first,
            format!(
                "At most {} steps, and one of move, archive, trash or spam.",
                crate::macros::MAX_STEPS
            ),
        ]
    }

    pub(super) fn keys_hints(&self) -> Vec<Hint> {
        let s = self.state;
        let Some(k) = s.keys.as_deref() else {
            return Vec::new();
        };
        if k.filtering {
            return vec![
                Hint::fixed("type", "to filter", HitTarget::Popup),
                Hint::fixed("Enter", "done", HitTarget::Popup),
                Hint::fixed("Esc", "clear", HitTarget::Popup),
            ];
        }
        let one = |a: Action, label: &'static str, short: &'static str, important: bool| {
            self.keys_for(a).map(|keys| Hint {
                keys,
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
        let v: Vec<Option<Hint>> = if k.steps.is_some() {
            vec![
                pair(Action::Next, Action::Prev, "move", true),
                one(Action::KeyAdd, "add step", "add", true),
                one(Action::KeyRemove, "remove step", "remove", true),
                pair(Action::MacroStepDown, Action::MacroStepUp, "reorder", false),
                one(Action::MacroRename, "rename", "rename", false),
                one(Action::MacroBind, "bind key", "bind", true),
                one(Action::Back, "back", "back", true),
                one(Action::Help, "help", "help", true),
            ]
        } else if k.section == Section::Macros {
            vec![
                pair(Action::Next, Action::Prev, "move", true),
                one(Action::MacroNew, "new", "new", true),
                one(Action::KeyReplace, "steps", "steps", true),
                one(Action::MacroBind, "bind key", "bind", true),
                one(Action::MacroRename, "rename", "rename", false),
                one(Action::KeyRemove, "delete", "delete", false),
                one(Action::NextPane, "section", "section", true),
                one(Action::Back, "back", "back", true),
                one(Action::Help, "help", "help", true),
            ]
        } else if !k.custom {
            vec![
                pair(Action::Next, Action::Prev, "move", true),
                one(
                    Action::PresetDuplicate,
                    "duplicate to edit",
                    "duplicate",
                    true,
                ),
                one(Action::PresetSwitch, "switch preset", "switch", false),
                one(Action::NextPane, "section", "section", true),
                one(Action::Filter, "filter", "filter", false),
                one(Action::Back, "back", "back", true),
                one(Action::Help, "help", "help", true),
            ]
        } else {
            vec![
                pair(Action::Next, Action::Prev, "move", true),
                one(Action::KeyReplace, "replace", "replace", true),
                one(Action::KeyAdd, "add", "add", true),
                one(Action::KeyRemove, "remove", "remove", true),
                one(Action::KeyReset, "reset", "reset", false),
                one(Action::NextPane, "section", "section", true),
                one(Action::Filter, "filter", "filter", false),
                one(Action::PresetSwitch, "switch preset", "switch", false),
                one(Action::Back, "back", "back", true),
                one(Action::Help, "help", "help", true),
            ]
        };
        v.into_iter().flatten().collect()
    }
}

fn steps_summary(m: &Macro) -> String {
    m.steps
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}
