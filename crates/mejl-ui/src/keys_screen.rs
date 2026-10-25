//! The key editor: the active preset's bindings by section, the edits a
//! row can take, and the user's macros.
//!
//! The editor works on the preset's own map: its built-in base plus its
//! own overrides. The top-level `[keys]` table and macro keys are not part
//! of that map, so the diff saved to `[presets.<name>.keys]` never takes
//! them in. Every edit here is a pure function from one map to the next.
//! `update` turns the result into [`KeysEdit::SetPresetKeys`] and shows
//! the change when `Msg::Config` comes back, so one edit is in flight at a
//! time.
//!
//! A built-in preset is read-only; the user duplicates it first. Macros
//! belong to no preset, so they can be edited with any preset.
//!
//! Conflict checks look a key up the way it fires: in each table the row
//! writes, then the global table where that is active too. A clash with a
//! key of the same table asks before it takes the key; a key that starts
//! or cuts a prefix of the same table is refused; hiding a global key is
//! only a warning, and hiding a global prefix asks first.
//!
//! [`KeysEdit::SetPresetKeys`]: crate::KeysEdit::SetPresetKeys

use crate::action::{Action, Category};
use crate::input::LineInput;
use crate::keymap::{Binding, Context, Key, KeyName, KeySeq, Keymap, Lookup, MACRO_CONTEXTS};
use crate::macros::{Macro, SIMPLE_STEPS, Step};
use crate::state::{Screen, UiConfig};

/// Actions that always keep at least one key, so the user cannot lock
/// themselves out. Every action of the key editor's own table counts too.
pub const LOCKOUT: [Action; 5] = [
    Action::Back,
    Action::Help,
    Action::Palette,
    Action::QuitNow,
    Action::GoSettings,
];

/// A part of the key editor, one per Tab stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    /// Actions with the same keys in the list and the reader. An edit here
    /// writes both.
    Mail,
    /// Actions whose list keys differ from their reader keys: the list.
    ListOnly,
    /// The same actions in the reader.
    ReaderOnly,
    /// Keys that work on the mail screen and on most other screens.
    Global,
    /// Compose.
    Compose,
    /// The search bar.
    Search,
    /// The mailbox sidebar.
    Sidebar,
    /// The Masked Email screen.
    Masked,
    /// The rules screen.
    Rules,
    /// The settings screen.
    Settings,
    /// The help overlay.
    Help,
    /// The key editor itself.
    Keys,
    /// The user's macros.
    Macros,
}

impl Section {
    /// Every section, in Tab order.
    pub const ALL: [Section; 13] = [
        Section::Mail,
        Section::ListOnly,
        Section::ReaderOnly,
        Section::Global,
        Section::Compose,
        Section::Search,
        Section::Sidebar,
        Section::Masked,
        Section::Rules,
        Section::Settings,
        Section::Help,
        Section::Keys,
        Section::Macros,
    ];

    /// The name on the section line.
    pub fn label(self) -> &'static str {
        match self {
            Section::Mail => "Mail",
            Section::ListOnly => "List only",
            Section::ReaderOnly => "Reader only",
            Section::Global => "Global",
            Section::Compose => "Compose",
            Section::Search => "Search",
            Section::Sidebar => "Sidebar",
            Section::Masked => "Masked",
            Section::Rules => "Rules",
            Section::Settings => "Settings",
            Section::Help => "Help",
            Section::Keys => "Keys",
            Section::Macros => "Macros",
        }
    }

    /// The tables an edit in this section writes. None for macros.
    pub fn contexts(self) -> &'static [Context] {
        match self {
            Section::Mail => &[Context::List, Context::Reader],
            Section::ListOnly => &[Context::List],
            Section::ReaderOnly => &[Context::Reader],
            Section::Global => &[Context::Global],
            Section::Compose => &[Context::Compose],
            Section::Search => &[Context::Search],
            Section::Sidebar => &[Context::Sidebar],
            Section::Masked => &[Context::Masked],
            Section::Rules => &[Context::Rules],
            Section::Settings => &[Context::Settings],
            Section::Help => &[Context::Help],
            Section::Keys => &[Context::Keys],
            Section::Macros => &[],
        }
    }

    /// Where a key of `context` is looked up when it fires: the context,
    /// then the global table where that is active as well.
    fn lookup_contexts(self, context: Context) -> Vec<Context> {
        match self {
            Section::Global | Section::Search | Section::Help => vec![context],
            _ => vec![context, Context::Global],
        }
    }

    /// The field a plain key types into here, if one does.
    fn field(self) -> Option<&'static str> {
        match self {
            Section::Compose => Some("compose"),
            Section::Search => Some("the search bar"),
            Section::Help => Some("help"),
            _ => None,
        }
    }

    /// The next or previous section, wrapping.
    pub fn step(self, forward: bool) -> Section {
        let n = Section::ALL.len();
        let i = Section::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Section::ALL[if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        }]
    }
}

/// One row of a section: an action and its keys there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The action.
    pub action: Action,
    /// Its keys in the section, the hinted one first.
    pub keys: Vec<KeySeq>,
    /// The name it shows: the action's label, or its description where
    /// two rows of the section share a label, as pin and toggle-pin do.
    pub name: String,
}

impl Row {
    /// The name the row shows.
    pub fn label(&self) -> String {
        self.name.clone()
    }
}

/// `s` with its first letter in upper case.
pub fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(first) => first.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn keys(map: &Keymap, context: Context, action: Action) -> Vec<KeySeq> {
    map.keys_of(context, Binding::Action(action))
}

fn same_keys(a: &[KeySeq], b: &[KeySeq]) -> bool {
    let (mut a, mut b) = (a.to_vec(), b.to_vec());
    a.sort();
    b.sort();
    a == b
}

fn bound_nowhere(map: &Keymap, base: &Keymap, action: Action) -> bool {
    Context::ALL
        .iter()
        .all(|&c| keys(map, c, action).is_empty() && keys(base, c, action).is_empty())
}

/// The keys `action` has in `section`, or `None` when it is not a row
/// there. An action whose keys were all removed keeps its row, so it can
/// get keys again; one that no table binds shows in Mail or Global by its
/// category.
fn member(map: &Keymap, base: &Keymap, section: Section, action: Action) -> Option<Vec<KeySeq>> {
    let k = |m: &Keymap, c| keys(m, c, action);
    match section {
        Section::Macros => None,
        Section::Mail | Section::ListOnly | Section::ReaderOnly => {
            let (l, r) = (k(map, Context::List), k(map, Context::Reader));
            let (bl, br) = (k(base, Context::List), k(base, Context::Reader));
            if l.is_empty() && r.is_empty() && bl.is_empty() && br.is_empty() {
                let mail = matches!(
                    action.category(),
                    Category::Navigate | Category::Select | Category::Triage | Category::Read
                );
                return (section == Section::Mail && mail && bound_nowhere(map, base, action))
                    .then(Vec::new);
            }
            let same = if l.is_empty() && r.is_empty() {
                same_keys(&bl, &br)
            } else {
                same_keys(&l, &r)
            };
            match section {
                Section::Mail => same.then_some(l),
                Section::ListOnly => (!same && !(l.is_empty() && bl.is_empty())).then_some(l),
                _ => (!same && !(r.is_empty() && br.is_empty())).then_some(r),
            }
        }
        _ => {
            let c = section.contexts()[0];
            let now = k(map, c);
            let unbound = section == Section::Global
                && matches!(
                    action.category(),
                    Category::App | Category::Go | Category::Tools
                )
                && bound_nowhere(map, base, action);
            (!now.is_empty() || !k(base, c).is_empty() || unbound).then_some(now)
        }
    }
}

/// The rows of a section, grouped by category.
pub fn rows(map: &Keymap, base: &Keymap, section: Section) -> Vec<Row> {
    let mut out: Vec<(Category, usize, Row)> = Action::ALL
        .iter()
        .enumerate()
        .filter_map(|(i, &action)| {
            member(map, base, section, action).map(|keys| {
                let name = capitalised(action.label());
                (action.category(), i, Row { action, keys, name })
            })
        })
        .collect();
    out.sort_by_key(|(c, i, _)| (*c, *i));
    let mut rows: Vec<Row> = out.into_iter().map(|(_, _, r)| r).collect();
    let labels: Vec<&'static str> = rows.iter().map(|r| r.action.label()).collect();
    for r in &mut rows {
        if labels.iter().filter(|l| **l == r.action.label()).count() > 1 {
            r.name = r.action.description().to_owned();
        }
    }
    rows
}

/// The section that shows `action` among the list and reader sections.
fn mail_section(map: &Keymap, base: &Keymap, action: Action) -> Option<Section> {
    [Section::Mail, Section::ListOnly, Section::ReaderOnly]
        .into_iter()
        .find(|s| member(map, base, *s, action).is_some())
}

/// Whether `action` must keep a key in `context`.
fn protected(binding: Binding, context: Context) -> bool {
    context == Context::Keys || matches!(binding, Binding::Action(a) if LOCKOUT.contains(&a))
}

fn name_of(binding: Binding) -> String {
    match binding {
        Binding::Action(a) => a.label().to_owned(),
        Binding::Macro(_) => "a macro".to_owned(),
    }
}

fn context_name(c: Context) -> &'static str {
    match c {
        Context::List => "list",
        Context::Reader => "reader",
        Context::Sidebar => "sidebar",
        _ => c.name(),
    }
}

/// Whether a key types into the field of a section instead of reaching
/// the keymap. Only modified keys, F-keys and Tab are safe everywhere;
/// the single-line search bar and help filter also leave Enter, the
/// arrows and the page keys alone.
fn types_into_field(section: Section, key: Key) -> bool {
    if section.field().is_none() || key.ctrl || key.alt {
        return false;
    }
    match key.name {
        KeyName::F(_) | KeyName::Tab | KeyName::BackTab => false,
        KeyName::Enter | KeyName::Up | KeyName::Down | KeyName::PageUp | KeyName::PageDown => {
            section == Section::Compose
        }
        _ => true,
    }
}

/// Warnings for keys that legacy terminal encoding cannot deliver.
fn legacy_warnings(keys: &KeySeq) -> Vec<String> {
    keys.0
        .iter()
        .find_map(Key::legacy_problem)
        .map(|reason| {
            format!(
                "{reason}; {keys} works only with the kitty keyboard protocol or tmux extended \
                 keys"
            )
        })
        .into_iter()
        .collect()
}

/// How a captured key changes a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The key becomes the row's only key (Enter).
    Replace,
    /// The key joins the row's keys (`a`).
    Add,
}

/// What binding a captured key would do, before anything changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    /// The key is free, or only hides global keys: bind it. The warnings
    /// say what to know.
    Free {
        /// Hidden global keys and legacy encoding problems.
        warnings: Vec<String>,
    },
    /// The row already has exactly this key.
    Same,
    /// The key does something else: ask before taking it.
    Ask {
        /// The question, such as "y is Archive".
        question: String,
        /// Whether Swap can be offered: the row has a key to give back.
        swap: bool,
        /// Things to know either way.
        warnings: Vec<String>,
    },
    /// The key cannot be bound here, and why.
    Refused(String),
}

/// What binding `keys` to the row of `action` in `section` would do.
pub fn check(map: &Keymap, section: Section, action: Action, mode: Mode, keys: &KeySeq) -> Check {
    let Some(&first) = keys.0.first() else {
        return Check::Refused("no key was pressed".into());
    };
    if section == Section::Macros {
        return Check::Refused("macros are bound with b".into());
    }
    if types_into_field(section, first)
        && let Some(field) = section.field()
    {
        let others = if section == Section::Compose {
            "an F-key or Tab"
        } else {
            "an F-key, Tab, Enter or the arrows"
        };
        return Check::Refused(format!(
            "in {field} {first} types into the field; use Ctrl, Alt, {others}"
        ));
    }
    let ours = Binding::Action(action);
    let row: Vec<Vec<KeySeq>> = section
        .contexts()
        .iter()
        .map(|&c| map.keys_of(c, ours))
        .collect();
    let already = match mode {
        Mode::Replace => row.iter().all(|k| k.len() == 1 && k[0] == *keys),
        Mode::Add => row.iter().all(|k| k.contains(keys)),
    };
    if already {
        return Check::Same;
    }
    let mut warnings = legacy_warnings(keys);
    let mut clash: Option<Binding> = None;
    let mut hides_prefix: Option<KeySeq> = None;
    for &c in section.contexts() {
        match map.lookup(&section.lookup_contexts(c), keys) {
            Lookup::Free => {}
            Lookup::Bound { binding, .. } if binding == ours => {}
            Lookup::Bound { context, binding } if context == c => {
                clash.get_or_insert(binding);
            }
            Lookup::Bound { binding, .. } => warnings.push(format!(
                "{keys} hides the global {} in the {}",
                name_of(binding),
                context_name(c)
            )),
            Lookup::Longer { context, example } if context == c => {
                let what = map.get(c, &example).map(name_of).unwrap_or_default();
                return Check::Refused(format!(
                    "{keys} starts {example} ({what}) here, so it cannot be a key of its own"
                ));
            }
            Lookup::Longer { example, .. } => {
                hides_prefix.get_or_insert(example);
            }
            Lookup::Shorter {
                context,
                keys: head,
                binding,
            } if context == c => {
                return Check::Refused(format!(
                    "{head} is already {} here, so the rest of {keys} would never be read",
                    name_of(binding)
                ));
            }
            Lookup::Shorter {
                keys: head,
                binding,
                ..
            } => warnings.push(format!(
                "{keys} hides the global {} ({head}) in the {}",
                name_of(binding),
                context_name(c)
            )),
        }
    }
    if section == Section::Global {
        for pane in [Context::List, Context::Reader, Context::Sidebar] {
            match map.lookup(&[pane], keys) {
                Lookup::Free => {}
                Lookup::Bound { binding, .. } | Lookup::Shorter { binding, .. } => {
                    warnings.push(format!(
                        "{keys} is {} in the {}, so it does nothing there",
                        name_of(binding),
                        context_name(pane)
                    ));
                }
                Lookup::Longer { .. } => warnings.push(format!(
                    "{keys} starts other keys in the {}, so it does nothing there",
                    context_name(pane)
                )),
            }
        }
    }
    if let Some(other) = clash {
        let swap = mode == Mode::Replace && row.iter().all(|k| !k.is_empty());
        return Check::Ask {
            question: format!("{keys} is {}", capitalised(&name_of(other))),
            swap,
            warnings,
        };
    }
    if let Some(example) = hides_prefix {
        return Check::Ask {
            question: format!("{keys} starts global keys such as {example}; binding it hides them"),
            swap: false,
            warnings,
        };
    }
    Check::Free { warnings }
}

/// A changed map and the keys the change touched, for dropping them from
/// the top-level `[keys]` table.
#[derive(Clone, Debug, PartialEq)]
pub struct Edit {
    /// The preset's map after the change.
    pub map: Keymap,
    /// Every (context, keys) whose binding changed.
    pub touched: Vec<(Context, KeySeq)>,
}

impl Edit {
    fn new(before: &Keymap, after: Keymap) -> Edit {
        let touched = after
            .diff_from(before)
            .entries()
            .iter()
            .map(|o| (o.context, o.keys.clone()))
            .collect();
        Edit {
            map: after,
            touched,
        }
    }
}

/// Binds `keys` to the row of `action` in `section`. A clash with another
/// row takes the key from it; with `swap`, that row gets this row's first
/// key back. Refuses what [`check`] refuses, and taking the last key of an
/// action that must keep one.
pub fn bind(
    map: &Keymap,
    section: Section,
    action: Action,
    mode: Mode,
    keys: &KeySeq,
    swap: bool,
) -> Result<Edit, String> {
    match check(map, section, action, mode, keys) {
        Check::Refused(why) => return Err(why),
        Check::Same => return Ok(Edit::new(map, map.clone())),
        Check::Free { .. } | Check::Ask { .. } => {}
    }
    let ours = Binding::Action(action);
    let mut next = map.clone();
    for &c in section.contexts() {
        let old = map.keys_of(c, ours);
        let victim = map.get(c, keys).filter(|b| *b != ours);
        let give = if swap && mode == Mode::Replace {
            old.first().filter(|o| *o != keys).cloned()
        } else {
            None
        };
        if let Some(v) = victim
            && give.is_none()
            && protected(v, c)
            && map.keys_of(c, v).len() <= 1
        {
            return Err(format!(
                "{keys} is the last key of {}; pick Swap or another key",
                capitalised(&name_of(v))
            ));
        }
        if mode == Mode::Replace {
            for o in &old {
                next.unbind(c, o);
            }
        }
        if victim.is_some() {
            next.unbind(c, keys);
        }
        if !(mode == Mode::Add && old.contains(keys)) {
            next.bind_to(c, keys.clone(), ours);
        }
        if let (Some(v), Some(g)) = (victim, give) {
            next.bind_to(c, g, v);
        }
    }
    Ok(Edit::new(map, next))
}

/// Removes one key from the row of `action` in `section`. An action that
/// must keep a key keeps its last one.
pub fn remove(
    map: &Keymap,
    section: Section,
    action: Action,
    keys: &KeySeq,
) -> Result<Edit, String> {
    let ours = Binding::Action(action);
    for &c in section.contexts() {
        let has = map.keys_of(c, ours);
        if has.contains(keys) && has.len() == 1 && protected(ours, c) {
            return Err(format!(
                "{} keeps its last key, so you can always find your way back; add another \
                 key first",
                capitalised(action.label())
            ));
        }
    }
    let mut next = map.clone();
    for &c in section.contexts() {
        if map.get(c, keys) == Some(ours) {
            next.unbind(c, keys);
        }
    }
    Ok(Edit::new(map, next))
}

/// Gives the row of `action` in `section` its keys from `base` again.
/// Refuses when another row now has one of them.
pub fn reset(
    map: &Keymap,
    base: &Keymap,
    section: Section,
    action: Action,
) -> Result<Edit, String> {
    let ours = Binding::Action(action);
    let mut next = map.clone();
    for &c in section.contexts() {
        let want = base.keys_of(c, ours);
        for w in &want {
            match map.lookup(&[c], w) {
                Lookup::Free => {}
                Lookup::Bound { binding, .. } if binding == ours => {}
                Lookup::Bound { binding, .. } | Lookup::Shorter { binding, .. } => {
                    return Err(format!(
                        "{w} is {} now; change that row first",
                        capitalised(&name_of(binding))
                    ));
                }
                Lookup::Longer { example, .. } => {
                    return Err(format!("{w} starts {example} now; change that row first"));
                }
            }
        }
        for o in map.keys_of(c, ours) {
            next.unbind(c, &o);
        }
        for w in want {
            next.bind_to(c, w, ours);
        }
    }
    Ok(Edit::new(map, next))
}

/// Why a preset row cannot take `keys` because a macro has them in the
/// full active keymap, if one does. Macro keys bind after the preset, so
/// the macro would keep the key and the edit would do nothing there. A
/// global key is checked in the list and the reader too, where the macro
/// would hide it.
pub fn macro_clash(active: &Keymap, section: Section, keys: &KeySeq) -> Option<String> {
    let contexts: &[Context] = if section == Section::Global {
        &MACRO_CONTEXTS
    } else {
        section.contexts()
    };
    contexts
        .iter()
        .filter(|c| MACRO_CONTEXTS.contains(c))
        .find_map(|&c| match active.get(c, keys) {
            Some(Binding::Macro(id)) => Some(format!(
                "{keys} runs the macro {} in the {}; give the macro another key with b first",
                active.macro_label(id),
                context_name(c)
            )),
            _ => None,
        })
}

/// What binding `keys` to the macro `id` would do, checked against the
/// full active keymap in the list, the reader and the global table.
pub fn check_macro(active: &Keymap, macros: &[Macro], id: &str, keys: &KeySeq) -> Check {
    if keys.0.is_empty() {
        return Check::Refused("no key was pressed".into());
    }
    if macros
        .iter()
        .any(|m| m.id == id && m.keys == [keys.clone()])
    {
        return Check::Same;
    }
    let mut clash: Option<String> = None;
    for c in MACRO_CONTEXTS {
        match active.lookup(&[c, Context::Global], keys) {
            Lookup::Free => {}
            Lookup::Bound {
                binding: Binding::Macro(m),
                ..
            } => match macros.get(usize::from(m.0)) {
                Some(other) if other.id != id => {
                    clash.get_or_insert(format!("{keys} runs the macro {}", other.label));
                }
                _ => {}
            },
            Lookup::Bound {
                binding: Binding::Action(a),
                ..
            } => {
                clash.get_or_insert(format!("{keys} is {}", a.label()));
            }
            Lookup::Longer { example, .. } => {
                return Check::Refused(format!(
                    "{keys} starts {example}, so the macro would never run"
                ));
            }
            Lookup::Shorter {
                keys: head,
                binding,
                ..
            } => {
                return Check::Refused(format!(
                    "{head} is already {}, so the rest of {keys} would never be read",
                    active.binding_label(binding)
                ));
            }
        }
    }
    match clash {
        Some(what) => Check::Ask {
            question: format!("{what}; the macro will take it"),
            swap: false,
            warnings: legacy_warnings(keys),
        },
        None => Check::Free {
            warnings: legacy_warnings(keys),
        },
    }
}

/// The macros to save when `id` takes `keys`: the macro with that key as
/// its only one, and any other macro that had it, without it.
pub fn bind_macro(macros: &[Macro], target: &Macro, keys: &KeySeq) -> Vec<Macro> {
    let mut out: Vec<Macro> = macros
        .iter()
        .filter(|m| m.id != target.id && m.keys.contains(keys))
        .map(|m| Macro {
            keys: m.keys.iter().filter(|k| *k != keys).cloned().collect(),
            ..m.clone()
        })
        .collect();
    out.push(Macro {
        keys: vec![keys.clone()],
        ..target.clone()
    });
    out
}

/// The macro with one more step, if it is still valid.
pub fn with_step(m: &Macro, step: Step) -> Result<Macro, String> {
    let mut next = m.clone();
    next.steps.push(step);
    next.validate().map(|()| next)
}

/// The macro without step `i`. The last step stays: a macro needs one.
pub fn without_step(m: &Macro, i: usize) -> Result<Macro, String> {
    if m.steps.len() <= 1 {
        return Err("a macro needs a step; x on the macro list deletes it".into());
    }
    if i >= m.steps.len() {
        return Err("no step there".into());
    }
    let mut next = m.clone();
    next.steps.remove(i);
    next.validate().map(|()| next)
}

/// The macro with step `i` moved one place up or down, or `None` at the
/// edge.
pub fn moved_step(m: &Macro, i: usize, up: bool) -> Option<Macro> {
    let j = if up { i.checked_sub(1)? } else { i + 1 };
    if j >= m.steps.len() || i >= m.steps.len() {
        return None;
    }
    let mut next = m.clone();
    next.steps.swap(i, j);
    Some(next)
}

/// A free name for a copy of the preset `name`: `fastmail-custom` for a
/// built-in one, `mine-copy` for a custom one, with a number when taken.
pub fn copy_name(config: &UiConfig, name: &str) -> String {
    let taken: Vec<&str> = crate::keymap::Preset::ALL
        .iter()
        .map(|p| p.name())
        .chain(config.presets.iter().map(|p| p.name.as_str()))
        .collect();
    let suffix = if crate::keymap::Preset::from_name(name).is_some() {
        "custom"
    } else {
        "copy"
    };
    crate::macros::id_for(&format!("{name}-{suffix}"), &taken)
}

/// Why `name` cannot name a new preset, if it cannot.
pub fn bad_preset_name(config: &UiConfig, name: &str) -> Option<String> {
    if !crate::macros::valid_id(name) {
        return Some(format!(
            "{name:?} cannot name a preset: use lowercase letters, digits and dashes"
        ));
    }
    if crate::keymap::Preset::from_name(name).is_some()
        || config.presets.iter().any(|p| p.name == name)
    {
        return Some(format!("there is already a preset called {name}"));
    }
    None
}

/// What a key capture is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureTarget {
    /// A row of a preset section.
    Row {
        /// The section.
        section: Section,
        /// The row's action.
        action: Action,
        /// Replace the row's keys or add one.
        mode: Mode,
    },
    /// A macro.
    Macro {
        /// The macro's id.
        id: String,
    },
}

/// A raw key capture in progress, shown in the status line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    /// The question, such as "Press the new key for Archive (Esc cancels)".
    pub prompt: String,
    /// Keys pressed so far, while they start longer bindings.
    pub keys: Vec<Key>,
    /// What the key is for.
    pub target: CaptureTarget,
}

/// A choice in one of the key editor's pickers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeysPick {
    /// Take the key and give the other row this row's key.
    Swap,
    /// Take the key from the other row.
    Replace,
    /// Leave everything as it is.
    Cancel,
    /// Remove this key from the row.
    RemoveKey(KeySeq),
    /// Use this preset.
    Preset(String),
    /// Add this step to the open macro.
    Step(StepPick),
}

/// A step in the add step picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepPick {
    /// A step without a folder, by index into [`SIMPLE_STEPS`].
    Simple(usize),
    /// Move to a folder; the folder picker comes next.
    Move,
    /// Add a label; the folder picker comes next.
    AddLabel,
    /// Remove a label; the folder picker comes next.
    RemoveLabel,
}

impl StepPick {
    /// The step for a folder pick, or the plain step.
    pub fn step(self, folder: Option<crate::macros::FolderRef>) -> Option<Step> {
        match (self, folder) {
            (StepPick::Simple(i), _) => SIMPLE_STEPS.get(i).map(|(_, s)| s.clone()),
            (StepPick::Move, Some(f)) => Some(Step::Move(f)),
            (StepPick::AddLabel, Some(f)) => Some(Step::AddLabel(f)),
            (StepPick::RemoveLabel, Some(f)) => Some(Step::RemoveLabel(f)),
            _ => None,
        }
    }
}

/// A name the key editor asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeysPrompt {
    /// The name of a copy of the active preset.
    Duplicate,
    /// A new name for a custom preset.
    RenamePreset {
        /// Its name now.
        from: String,
    },
    /// The label of a new macro.
    NewMacro,
    /// A new label for a macro.
    RenameMacro {
        /// The macro's id.
        id: String,
    },
}

/// The steps view of one macro.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MacroSteps {
    /// The macro's id.
    pub(crate) id: String,
    /// Its label, kept for a new macro that is not saved yet.
    pub(crate) label: String,
    /// The focused step.
    pub(crate) cursor: usize,
}

/// A captured key waiting for the answer to a clash question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingBind {
    pub(crate) target: CaptureTarget,
    pub(crate) keys: KeySeq,
}

/// The key editor's state.
#[derive(Clone, Debug)]
pub struct KeysScreen {
    /// The section shown.
    pub(crate) section: Section,
    /// The focused row among the shown ones.
    pub(crate) cursor: usize,
    /// The filter typed after `/`.
    pub(crate) filter: LineInput,
    /// Keys go to the filter.
    pub(crate) filtering: bool,
    /// The active preset's name.
    pub(crate) name: String,
    /// Its own map: base plus its own overrides.
    pub(crate) map: Keymap,
    /// Its built-in base.
    pub(crate) base: Keymap,
    /// True for a custom preset, which can be edited.
    pub(crate) custom: bool,
    /// The shown rows of a preset section.
    pub(crate) rows: Vec<Row>,
    /// The shown macros, by index into the config's macros.
    pub(crate) macros: Vec<usize>,
    /// Changes sent and not answered yet.
    pub(crate) saving: u8,
    /// The screen to go back to.
    pub(crate) from: Screen,
    /// The steps view, when a macro is open.
    pub(crate) steps: Option<MacroSteps>,
    /// A captured key waiting for the clash question.
    pub(crate) pending: Option<PendingBind>,
    /// The folder step the folder picker is for.
    pub(crate) folder_step: Option<StepPick>,
}

impl KeysScreen {
    /// The editor on the active preset of `config`.
    pub fn new(config: &UiConfig, from: Screen) -> KeysScreen {
        let mut s = KeysScreen {
            section: Section::Mail,
            cursor: 0,
            filter: LineInput::default(),
            filtering: false,
            name: String::new(),
            map: Keymap::default(),
            base: Keymap::default(),
            custom: false,
            rows: Vec::new(),
            macros: Vec::new(),
            saving: 0,
            from,
            steps: None,
            pending: None,
            folder_step: None,
        };
        s.reload(config);
        s
    }

    /// Reads the active preset from `config` again, keeping the cursor on
    /// the same action when it is still shown. An action that moved
    /// between Mail, List only and Reader only is followed there.
    pub fn reload(&mut self, config: &UiConfig) {
        let focused = self.row().map(|r| r.action);
        self.name = config.keymap.name().to_owned();
        match config.presets.iter().find(|p| p.name == self.name) {
            Some(p) => {
                self.base = Keymap::preset(p.base);
                // Applied without the check: a preset may only load
                // together with `[keys]`, and its edits must still keep
                // every one of its own entries. The shell checks the
                // whole keymap before it saves.
                let mut map = self.base.clone();
                map.apply(&p.overrides);
                self.map = map;
                self.custom = true;
            }
            None => {
                self.base = Keymap::preset(config.keymap.preset_kind());
                self.map = self.base.clone();
                self.custom = false;
            }
        }
        if let Some(a) = focused
            && matches!(
                self.section,
                Section::Mail | Section::ListOnly | Section::ReaderOnly
            )
            && member(&self.map, &self.base, self.section, a).is_none()
            && let Some(s) = mail_section(&self.map, &self.base, a)
        {
            self.section = s;
        }
        self.refilter(config);
        if let Some(a) = focused
            && let Some(i) = self.rows.iter().position(|r| r.action == a)
        {
            self.cursor = i;
        }
        if let Some(st) = self.steps.as_mut() {
            let n = config
                .macros
                .iter()
                .find(|m| m.id == st.id)
                .map_or(0, |m| m.steps.len());
            st.cursor = st.cursor.min(n.saturating_sub(1));
        }
    }

    /// Works out the shown rows again after the section, filter or config
    /// changed.
    pub fn refilter(&mut self, config: &UiConfig) {
        let needle = self.filter.text().trim().to_lowercase();
        let matches = |hay: &str| needle.is_empty() || hay.to_lowercase().contains(&needle);
        self.rows = rows(&self.map, &self.base, self.section)
            .into_iter()
            .filter(|r| {
                let keys: Vec<String> = r
                    .keys
                    .iter()
                    .flat_map(|k| [k.to_string(), k.to_config()])
                    .collect();
                matches(&format!(
                    "{} {} {} {}",
                    r.action.label(),
                    r.action.name(),
                    r.action.description(),
                    keys.join(" ")
                ))
            })
            .collect();
        self.macros = config
            .macros
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                let steps: Vec<String> = m.steps.iter().map(ToString::to_string).collect();
                matches(&format!("{} {}", m.label, steps.join(" ")))
            })
            .map(|(i, _)| i)
            .collect();
        self.cursor = self.cursor.min(self.len().saturating_sub(1));
    }

    /// Shows another section.
    pub fn show(&mut self, section: Section, config: &UiConfig) {
        self.section = section;
        self.cursor = 0;
        self.steps = None;
        self.refilter(config);
    }

    /// Rows in the current list.
    pub fn len(&self) -> usize {
        if self.section == Section::Macros {
            self.macros.len()
        } else {
            self.rows.len()
        }
    }

    /// True when the current list has no rows.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The focused preset row.
    pub fn row(&self) -> Option<&Row> {
        if self.section == Section::Macros {
            return None;
        }
        self.rows.get(self.cursor)
    }

    /// The focused macro, from the list or the steps view. A new macro
    /// that has no step yet is not in `config`, so it comes back empty.
    pub fn focused_macro(&self, config: &UiConfig) -> Option<Macro> {
        if let Some(st) = &self.steps {
            return Some(
                config
                    .macros
                    .iter()
                    .find(|m| m.id == st.id)
                    .cloned()
                    .unwrap_or_else(|| Macro {
                        id: st.id.clone(),
                        label: st.label.clone(),
                        keys: Vec::new(),
                        steps: Vec::new(),
                    }),
            );
        }
        if self.section != Section::Macros {
            return None;
        }
        self.macros
            .get(self.cursor)
            .and_then(|i| config.macros.get(*i))
            .cloned()
    }

    /// Whether the macro being edited is saved in the config yet.
    pub fn macro_saved(&self, config: &UiConfig, id: &str) -> bool {
        config.macros.iter().any(|m| m.id == id)
    }

    /// The overrides the preset stores for `map`: its diff from the base.
    pub fn overrides(&self, map: &Keymap) -> crate::keymap::KeyOverrides {
        map.diff_from(&self.base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{KeyOverrides, Preset};

    fn seq(s: &str) -> KeySeq {
        s.parse().unwrap()
    }

    fn fm() -> Keymap {
        Keymap::preset(Preset::Fastmail)
    }

    fn row(rows: &[Row], a: Action) -> Option<&Row> {
        rows.iter().find(|r| r.action == a)
    }

    #[test]
    fn a_preset_that_loads_only_with_the_keys_table_keeps_its_entries() {
        // `.` alone hides `. r`; the `[keys]` table frees those, so only
        // the whole keymap loads. The editor must still show the `.`.
        let overrides = KeyOverrides::from_toml("[keys.list]\n\".\" = \"archive\"\n").unwrap();
        let user = KeyOverrides::from_toml(
            "[keys.list]\n\". r\" = \"none\"\n\". u\" = \"none\"\n\". p\" = \"none\"\n\". n\" = \"none\"\n",
        )
        .unwrap();
        assert!(Keymap::with_overrides(Preset::Fastmail, &overrides).is_err());
        let keymap = Keymap::build(crate::keymap::KeymapSpec {
            base: Preset::Fastmail,
            name: "mine",
            preset: &overrides,
            user: &user,
            macros: &[],
        })
        .unwrap();
        let config = UiConfig {
            keymap,
            presets: vec![crate::keymap::CustomPreset {
                name: "mine".into(),
                base: Preset::Fastmail,
                overrides,
            }],
            user_keys: user,
            ..UiConfig::default()
        };
        let s = KeysScreen::new(&config, Screen::Settings);
        assert!(s.custom);
        assert_eq!(
            s.map.get(Context::List, &seq(".")),
            Some(Binding::Action(Action::Archive))
        );
    }

    #[test]
    fn sections_split_the_list_and_reader_by_their_keys() {
        let map = fm();
        let mail = rows(&map, &map, Section::Mail);
        assert_eq!(row(&mail, Action::Archive).unwrap().keys, vec![seq("y")]);
        assert_eq!(row(&mail, Action::Open).unwrap().keys.len(), 2);
        assert!(
            row(&mail, Action::Next).is_none(),
            "j/down in the list, j in the reader"
        );
        let list = rows(&map, &map, Section::ListOnly);
        assert_eq!(
            row(&list, Action::Next).unwrap().keys,
            vec![seq("j"), seq("down")]
        );
        let reader = rows(&map, &map, Section::ReaderOnly);
        assert_eq!(row(&reader, Action::Next).unwrap().keys, vec![seq("j")]);
        assert!(row(&reader, Action::ScrollDown).is_some());
        assert!(row(&list, Action::ScrollDown).is_none());
        let global = rows(&map, &map, Section::Global);
        assert_eq!(
            row(&global, Action::Palette).unwrap().keys,
            vec![seq(":"), seq("ctrl-k"), seq("ctrl-p")]
        );
        assert!(
            row(&global, Action::ToggleSidebar).is_some(),
            "an action no table binds can get a key"
        );
        assert!(row(&global, Action::EditKeys).is_some());
        let keys = rows(&map, &map, Section::Keys);
        assert!(row(&keys, Action::KeyReplace).is_some());
        // Grouped by category: Move before Triage before Read.
        let cats: Vec<Category> = mail.iter().map(|r| r.action.category()).collect();
        let mut sorted = cats.clone();
        sorted.sort();
        assert_eq!(cats, sorted);
        // vim has no keys for pin and unpin; they are rows to bind.
        let vim = Keymap::preset(Preset::Vim);
        let vmail = rows(&vim, &vim, Section::Mail);
        assert!(row(&vmail, Action::Pin).unwrap().keys.is_empty());
        assert!(rows(&map, &map, Section::Macros).is_empty());
    }

    #[test]
    fn a_free_key_binds_in_both_mail_tables() {
        let map = fm();
        assert_eq!(
            check(
                &map,
                Section::Mail,
                Action::Archive,
                Mode::Replace,
                &seq("F2")
            ),
            Check::Free { warnings: vec![] }
        );
        let e = bind(
            &map,
            Section::Mail,
            Action::Archive,
            Mode::Replace,
            &seq("F2"),
            false,
        )
        .unwrap();
        for c in [Context::List, Context::Reader] {
            assert_eq!(
                e.map.keys_of(c, Binding::Action(Action::Archive)),
                vec![seq("F2")]
            );
            assert_eq!(e.map.get(c, &seq("y")), None);
        }
        assert_eq!(e.touched.len(), 4, "{:?}", e.touched);
        let added = bind(
            &map,
            Section::Mail,
            Action::Archive,
            Mode::Add,
            &seq("F2"),
            false,
        )
        .unwrap();
        assert_eq!(
            added
                .map
                .keys_of(Context::List, Binding::Action(Action::Archive)),
            vec![seq("y"), seq("F2")]
        );
        assert_eq!(
            check(
                &map,
                Section::Mail,
                Action::Archive,
                Mode::Replace,
                &seq("y")
            ),
            Check::Same
        );
    }

    #[test]
    fn an_exact_clash_asks_and_swap_moves_one_key() {
        let map = fm();
        // e is Expand in the reader only, so the Mail row asks.
        let Check::Ask { question, swap, .. } = check(
            &map,
            Section::Mail,
            Action::Archive,
            Mode::Replace,
            &seq("d"),
        ) else {
            panic!()
        };
        assert_eq!(question, "d is Trash");
        assert!(swap);
        let e = bind(
            &map,
            Section::Mail,
            Action::Archive,
            Mode::Replace,
            &seq("d"),
            true,
        )
        .unwrap();
        for c in [Context::List, Context::Reader] {
            assert_eq!(
                e.map.get(c, &seq("d")),
                Some(Binding::Action(Action::Archive))
            );
            assert_eq!(
                e.map.get(c, &seq("y")),
                Some(Binding::Action(Action::Trash))
            );
        }
        let e = bind(
            &map,
            Section::Mail,
            Action::Archive,
            Mode::Replace,
            &seq("d"),
            false,
        )
        .unwrap();
        assert!(
            e.map
                .keys_of(Context::List, Binding::Action(Action::Trash))
                .is_empty()
        );
        assert_eq!(e.map.get(Context::List, &seq("y")), None);
        // Swapping with a row of several keys moves only the key taken.
        // The key it gets back sorts after its first key.
        let e = bind(
            &map,
            Section::Global,
            Action::Search,
            Mode::Replace,
            &seq("ctrl-k"),
            true,
        )
        .unwrap();
        assert_eq!(
            e.map
                .keys_of(Context::Global, Binding::Action(Action::Palette)),
            vec![seq(":"), seq("/"), seq("ctrl-p")]
        );
        assert_eq!(
            e.map
                .keys_of(Context::Global, Binding::Action(Action::Search)),
            vec![seq("ctrl-k")]
        );
        // Adding a key has nothing to give back.
        let Check::Ask { swap, .. } =
            check(&map, Section::Mail, Action::Archive, Mode::Add, &seq("d"))
        else {
            panic!()
        };
        assert!(!swap);
    }

    #[test]
    fn prefix_clashes_are_refused_and_global_ones_warn_or_ask() {
        let map = fm();
        let refused = |s: Section, a: Action, k: &str| {
            matches!(check(&map, s, a, Mode::Replace, &seq(k)), Check::Refused(_))
        };
        assert!(refused(Section::Mail, Action::Archive, "."), ". starts . r");
        assert!(refused(Section::Mail, Action::Archive, "y x"), "y is a key");
        assert!(refused(Section::Global, Action::Help, "g"), "g starts g i");
        // A single global key: a warning only.
        let Check::Free { warnings } = check(
            &map,
            Section::Mail,
            Action::Archive,
            Mode::Replace,
            &seq("c"),
        ) else {
            panic!()
        };
        assert!(
            warnings[0].contains("hides the global compose"),
            "{warnings:?}"
        );
        // A global prefix: asked.
        let Check::Ask { question, swap, .. } = check(
            &map,
            Section::Mail,
            Action::Archive,
            Mode::Replace,
            &seq("g"),
        ) else {
            panic!()
        };
        assert!(question.starts_with("g starts global keys"), "{question}");
        assert!(!swap);
        let e = bind(
            &map,
            Section::Mail,
            Action::Archive,
            Mode::Replace,
            &seq("g"),
            false,
        )
        .unwrap();
        assert_eq!(
            e.map.get(Context::List, &seq("g")),
            Some(Binding::Action(Action::Archive))
        );
        assert!(
            e.map.is_prefix(Context::Global, &seq("g")),
            "the global table keeps it"
        );
        // A global key that a pane uses warns.
        let Check::Free { warnings } =
            check(&map, Section::Global, Action::Help, Mode::Add, &seq("y"))
        else {
            panic!()
        };
        assert!(
            warnings.iter().any(|w| w.contains("archive in the list")),
            "{warnings:?}"
        );
        // Legacy keys warn.
        let Check::Free { warnings } = check(
            &map,
            Section::Global,
            Action::Help,
            Mode::Add,
            &seq("alt-h"),
        ) else {
            panic!()
        };
        assert!(warnings[0].contains("kitty"), "{warnings:?}");
    }

    #[test]
    fn the_lockout_guard_keeps_the_last_key() {
        let map = fm();
        let e = remove(&map, Section::Global, Action::Help, &seq("?"));
        assert!(e.unwrap_err().contains("keeps its last key"));
        let e = remove(&map, Section::Global, Action::Back, &seq("u")).unwrap();
        let again = remove(&e.map, Section::Global, Action::Back, &seq("esc"));
        assert!(again.is_err());
        assert!(remove(&map, Section::Keys, Action::KeyAdd, &seq("a")).is_err());
        assert!(remove(&map, Section::Mail, Action::Archive, &seq("y")).is_ok());
        // Taking the last key of help needs a swap.
        let e = bind(
            &map,
            Section::Global,
            Action::Compose,
            Mode::Replace,
            &seq("?"),
            false,
        );
        assert!(e.unwrap_err().contains("last key of Help"));
        let e = bind(
            &map,
            Section::Global,
            Action::Compose,
            Mode::Replace,
            &seq("?"),
            true,
        )
        .unwrap();
        assert_eq!(
            e.map.get(Context::Global, &seq("c")),
            Some(Binding::Action(Action::Help))
        );
    }

    #[test]
    fn the_typing_guard_refuses_plain_keys_in_fields() {
        let map = fm();
        let refused = |s: Section, k: &str| {
            matches!(
                check(&map, s, Action::SaveDraft, Mode::Add, &seq(k)),
                Check::Refused(_)
            )
        };
        assert!(refused(Section::Compose, "x"));
        assert!(refused(Section::Compose, "space"));
        assert!(refused(Section::Compose, "enter"));
        assert!(refused(Section::Compose, "backspace"));
        assert!(!refused(Section::Compose, "ctrl-g"));
        assert!(!refused(Section::Compose, "F5"));
        assert!(refused(Section::Search, "x"));
        assert!(!refused(Section::Search, "F5"));
        assert!(refused(Section::Help, "q"));
        assert!(!refused(Section::Mail, "F5"));
    }

    #[test]
    fn reset_brings_back_the_base_keys() {
        let base = fm();
        let e = bind(
            &base,
            Section::Mail,
            Action::Archive,
            Mode::Replace,
            &seq("F2"),
            false,
        )
        .unwrap();
        let back = reset(&e.map, &base, Section::Mail, Action::Archive).unwrap();
        assert_eq!(back.map.action_bindings(), base.action_bindings());
        // y now belongs to trash: reset refuses.
        let e = bind(
            &e.map,
            Section::Mail,
            Action::Trash,
            Mode::Add,
            &seq("y"),
            false,
        )
        .unwrap();
        assert!(reset(&e.map, &base, Section::Mail, Action::Archive).is_err());
    }

    #[test]
    fn saved_overrides_rebuild_the_edited_map() {
        for preset in Preset::ALL {
            let base = Keymap::preset(preset);
            let mut map = base.clone();
            let archive = keys(&base, Context::List, Action::Archive)[0].clone();
            // Taking the trash key swaps: archive's old key now trashes.
            let trash = keys(&base, Context::List, Action::Trash)[0].clone();
            map = bind(
                &map,
                Section::Mail,
                Action::Archive,
                Mode::Replace,
                &trash,
                true,
            )
            .unwrap()
            .map;
            map = bind(
                &map,
                Section::Global,
                Action::Help,
                Mode::Add,
                &seq("F1"),
                false,
            )
            .unwrap()
            .map;
            map = bind(
                &map,
                Section::ListOnly,
                Action::Next,
                Mode::Add,
                &seq("ctrl-n"),
                false,
            )
            .unwrap()
            .map;
            map = remove(&map, Section::Global, Action::Palette, &seq("ctrl-k"))
                .unwrap()
                .map;
            let overrides: KeyOverrides = map.diff_from(&base);
            let rebuilt = Keymap::with_overrides(preset, &overrides).unwrap();
            assert_eq!(
                rebuilt.action_bindings(),
                map.action_bindings(),
                "{preset:?}"
            );
            assert_eq!(
                rebuilt.get(Context::Reader, &archive),
                Some(Binding::Action(Action::Trash)),
                "{preset:?}"
            );
            // Through the config table too.
            let reread = KeyOverrides::from_table(&overrides.to_table()).unwrap();
            let again = Keymap::with_overrides(preset, &reread).unwrap();
            assert_eq!(again.action_bindings(), map.action_bindings());
        }
    }

    fn a_macro(id: &str, keys: &[&str]) -> Macro {
        Macro {
            id: id.into(),
            label: id.into(),
            keys: keys.iter().map(|k| seq(k)).collect(),
            steps: vec![Step::MarkRead],
        }
    }

    #[test]
    fn macro_keys_check_the_active_keymap() {
        let map = fm();
        let ms = [a_macro("one", &[]), a_macro("two", &["F3"])];
        let Check::Ask { question, .. } = check_macro(&map, &ms, "one", &seq("T")) else {
            panic!()
        };
        assert_eq!(question, "T is thread view; the macro will take it");
        assert!(matches!(
            check_macro(&map, &ms, "one", &seq(".")),
            Check::Refused(_)
        ));
        assert!(matches!(
            check_macro(&map, &ms, "one", &seq("y x")),
            Check::Refused(_)
        ));
        assert_eq!(
            check_macro(&map, &ms, "one", &seq("F4")),
            Check::Free { warnings: vec![] }
        );
        assert_eq!(check_macro(&map, &ms, "two", &seq("F3")), Check::Same);
        let saved = bind_macro(&ms, &ms[0], &seq("F3"));
        assert_eq!(saved.len(), 2);
        assert!(saved[0].keys.is_empty() && saved[0].id == "two");
        assert_eq!(saved[1].keys, vec![seq("F3")]);
    }

    #[test]
    fn a_macro_key_is_not_taken_by_a_preset_row() {
        let ms = [a_macro("receipt", &["F6"])];
        let active = Keymap::build(crate::keymap::KeymapSpec {
            base: Preset::Fastmail,
            name: "fastmail",
            preset: &KeyOverrides::default(),
            user: &KeyOverrides::default(),
            macros: &ms,
        })
        .unwrap();
        let why = macro_clash(&active, Section::Mail, &seq("F6")).unwrap();
        assert!(why.contains("runs the macro receipt in the list"), "{why}");
        assert!(macro_clash(&active, Section::ReaderOnly, &seq("F6")).is_some());
        assert!(
            macro_clash(&active, Section::Global, &seq("F6")).is_some(),
            "a macro key would hide the global key in the list"
        );
        assert!(macro_clash(&active, Section::Mail, &seq("F7")).is_none());
    }

    #[test]
    fn macro_steps_change_and_stay_valid() {
        let m = a_macro("m", &[]);
        let m = with_step(&m, Step::Unpin).unwrap();
        let m = with_step(&m, Step::Archive).unwrap();
        assert!(
            with_step(&m, Step::Trash)
                .unwrap_err()
                .contains("only once")
        );
        let up = moved_step(&m, 2, true).unwrap();
        assert_eq!(up.steps, vec![Step::MarkRead, Step::Archive, Step::Unpin]);
        assert!(moved_step(&m, 0, true).is_none());
        assert!(moved_step(&m, 2, false).is_none());
        let less = without_step(&m, 0).unwrap();
        assert_eq!(less.steps, vec![Step::Unpin, Step::Archive]);
        let one = a_macro("m", &[]);
        assert!(without_step(&one, 0).is_err());
    }

    #[test]
    fn copy_names_are_free() {
        let mut c = UiConfig::default();
        assert_eq!(copy_name(&c, "fastmail"), "fastmail-custom");
        c.presets.push(crate::keymap::CustomPreset {
            name: "fastmail-custom".into(),
            base: Preset::Fastmail,
            overrides: KeyOverrides::default(),
        });
        assert_eq!(copy_name(&c, "fastmail"), "fastmail-custom-2");
        assert_eq!(copy_name(&c, "fastmail-custom"), "fastmail-custom-copy");
        assert!(bad_preset_name(&c, "vim").is_some());
        assert!(bad_preset_name(&c, "fastmail-custom").is_some());
        assert!(bad_preset_name(&c, "Mine").is_some());
        assert!(bad_preset_name(&c, "mine").is_none());
    }
}
