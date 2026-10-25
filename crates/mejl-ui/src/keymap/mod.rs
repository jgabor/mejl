//! The key binding engine.
//!
//! Bindings live in per-[`Context`] tables. While the list has focus the
//! active contexts are `[List, Global]`; a modal overlay such as help
//! replaces them with its own context only. For each key the first active
//! context that knows the sequence so far decides: an exact match runs its
//! action, a strict prefix waits for the next key. So a context binding
//! shadows a global one, and contexts can extend a global prefix.
//!
//! There are no timeouts. A prefix key never does anything on its own,
//! the which-key popup lists what can follow, `Esc` cancels, and any key
//! that is not listed cancels without doing anything else.
//!
//! The engine is plain data: [`Keymap::feed`] takes the pending state and a
//! key and returns a [`Resolution`]. The same tables drive the hint bar,
//! help, which-key and the conflict checker behind `mejl keys --check`.

mod config;
mod key;
mod preset;

use std::collections::BTreeMap;
use std::fmt;

pub use config::{KeyOverrides, KeymapError, Override};
pub use key::{Key, KeyName, KeyParseError, KeySeq};
pub use preset::Preset;

use crate::action::Action;

/// Where a binding applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Context {
    /// Everywhere on the mail screen, after the focused pane's context.
    Global,
    /// The conversation list.
    List,
    /// The reader.
    Reader,
    /// The mailbox sidebar.
    Sidebar,
    /// The help overlay. Printable keys not bound here type into the
    /// filter.
    Help,
    /// Compose. While a field is being edited only this context is
    /// active, and it binds only keys a text field cannot need (the
    /// `Ctrl-o` prefix, `Ctrl-s`, Tab, Esc); every other key types.
    Compose,
    /// The search bar. Like compose, it binds only non-typing keys.
    Search,
    /// The Masked Email screen, before the global table.
    Masked,
    /// The rules screen, before the global table.
    Rules,
    /// The settings screen, before the global table.
    Settings,
}

impl Context {
    /// Every context.
    pub const ALL: [Context; 10] = [
        Context::Global,
        Context::List,
        Context::Reader,
        Context::Sidebar,
        Context::Help,
        Context::Compose,
        Context::Search,
        Context::Masked,
        Context::Rules,
        Context::Settings,
    ];

    /// True for contexts of a whole screen or modal layer. Their bindings
    /// replace global ones on purpose, so the conflict checker does not
    /// report them as hiding global keys.
    pub fn is_modal(self) -> bool {
        !matches!(self, Context::List | Context::Reader | Context::Sidebar)
    }

    /// Config table name, as in `[keys.list]`.
    pub fn name(self) -> &'static str {
        match self {
            Context::Global => "global",
            Context::List => "list",
            Context::Reader => "reader",
            Context::Sidebar => "sidebar",
            Context::Help => "help",
            Context::Compose => "compose",
            Context::Search => "search",
            Context::Masked => "masked",
            Context::Rules => "rules",
            Context::Settings => "settings",
        }
    }

    /// Looks a context up by config name.
    pub fn from_name(name: &str) -> Option<Context> {
        Context::ALL.into_iter().find(|c| c.name() == name)
    }
}

/// Keys typed so far that did not resolve yet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pending {
    /// Prefix keys pressed so far.
    pub keys: Vec<Key>,
    /// A repeat count typed before a motion (vim preset only).
    pub count: Option<u32>,
}

impl Pending {
    /// True when nothing is pending.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.count.is_none()
    }

    /// Forgets the pending keys and count.
    pub fn clear(&mut self) {
        self.keys.clear();
        self.count = None;
    }
}

/// What a key press resolved to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Run an action, with the count typed before it, if any.
    Action {
        /// The action.
        action: Action,
        /// Repeat count, if one was typed.
        count: Option<u32>,
    },
    /// The keys so far are a prefix (or a count). Wait for the next key.
    Pending,
    /// A pending prefix was cancelled by Esc or by a key that does not
    /// follow it. Nothing else happens.
    Cancelled,
    /// The key is not bound in the active contexts.
    Unbound,
}

/// A key that can follow a pending prefix, for the which-key popup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Continuation {
    /// The next key.
    pub key: Key,
    /// What it does.
    pub target: ContinuationTarget,
}

/// What a continuation leads to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContinuationTarget {
    /// Runs this action.
    Action(Action),
    /// Opens a deeper prefix with this label.
    Prefix(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    action: Action,
    order: u32,
}

/// Severity of a [`Conflict`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Worth knowing; the binding works in some setups.
    Warning,
    /// A binding can never fire.
    Error,
}

/// A problem found by [`Keymap::check`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    /// How bad it is.
    pub severity: Severity,
    /// The context of the binding.
    pub context: Context,
    /// The key sequence.
    pub keys: KeySeq,
    /// A plain description.
    pub message: String,
}

impl fmt::Display for Conflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let level = match self.severity {
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        write!(
            f,
            "{level}: [keys.{}] \"{}\": {}",
            self.context.name(),
            self.keys.to_config(),
            self.message
        )
    }
}

/// Context-aware key bindings.
#[derive(Clone, Debug, PartialEq)]
pub struct Keymap {
    preset: Preset,
    tables: BTreeMap<Context, BTreeMap<KeySeq, Entry>>,
    prefix_labels: BTreeMap<KeySeq, String>,
    next_order: u32,
    warnings: Vec<Conflict>,
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap::preset(Preset::Fastmail)
    }
}

impl Keymap {
    /// The bindings of a preset, with no user overrides.
    pub fn preset(preset: Preset) -> Keymap {
        let mut map = Keymap {
            preset,
            tables: BTreeMap::new(),
            prefix_labels: BTreeMap::new(),
            next_order: 0,
            warnings: Vec::new(),
        };
        for (ctx, keys, action) in preset.bindings() {
            // Preset tables are static and covered by tests, so a parse
            // failure here is a programming error caught in CI; skipping
            // keeps release builds running regardless.
            if let Ok(seq) = keys.parse::<KeySeq>() {
                map.bind(ctx, seq, action);
            }
        }
        for (keys, label) in preset.prefix_labels() {
            if let Ok(seq) = keys.parse::<KeySeq>() {
                map.prefix_labels.insert(seq, (*label).to_owned());
            }
        }
        map
    }

    /// A preset with user overrides applied on top. Fails when an override
    /// makes a binding unreachable; warnings are kept in
    /// [`Keymap::warnings`].
    pub fn with_overrides(
        preset: Preset,
        overrides: &KeyOverrides,
    ) -> Result<Keymap, Vec<KeymapError>> {
        let mut map = Keymap::preset(preset);
        for o in overrides.entries() {
            match o.action {
                Some(action) => map.bind(o.context, o.keys.clone(), action),
                None => map.unbind(o.context, &o.keys),
            }
        }
        let (errors, warnings): (Vec<Conflict>, Vec<Conflict>) = map
            .check()
            .into_iter()
            .partition(|c| c.severity == Severity::Error);
        if !errors.is_empty() {
            return Err(errors.into_iter().map(KeymapError::Conflict).collect());
        }
        map.warnings = warnings;
        Ok(map)
    }

    /// The preset this keymap started from.
    pub fn preset_kind(&self) -> Preset {
        self.preset
    }

    /// Warnings found when overrides were applied.
    pub fn warnings(&self) -> &[Conflict] {
        &self.warnings
    }

    /// Adds or replaces a binding.
    pub fn bind(&mut self, context: Context, keys: KeySeq, action: Action) {
        let order = self.next_order;
        self.next_order += 1;
        self.tables
            .entry(context)
            .or_default()
            .insert(keys, Entry { action, order });
    }

    /// Removes a binding, and every longer binding it is a prefix of, so a
    /// user can free a prefix key such as `g` for their own use.
    pub fn unbind(&mut self, context: Context, keys: &KeySeq) {
        if let Some(table) = self.tables.get_mut(&context) {
            table.retain(|k, _| k != keys && !keys.is_prefix_of(k));
        }
    }

    /// Feeds one key. `pending` is updated in place: it grows on a prefix
    /// and is cleared when the sequence resolves or is cancelled.
    pub fn feed(&self, pending: &mut Pending, contexts: &[Context], key: Key) -> Resolution {
        if !pending.is_empty() && key == Key::ESC {
            pending.clear();
            return Resolution::Cancelled;
        }
        // A digit starts a count unless a context binds it on its own, as
        // the Masked Email screen binds its filters to 1 to 6.
        if self.preset.counts()
            && pending.keys.is_empty()
            && let Some(d) = key.text().and_then(|c| c.to_digit(10))
            && (d > 0 || pending.count.is_some())
            && (pending.count.is_some() || !self.binds(contexts, key))
        {
            let count = pending
                .count
                .unwrap_or(0)
                .saturating_mul(10)
                .saturating_add(d);
            pending.count = Some(count.min(99_999));
            return Resolution::Pending;
        }
        let mut seq = pending.keys.clone();
        seq.push(key);
        let seq = KeySeq(seq);
        for ctx in contexts {
            let Some(table) = self.tables.get(ctx) else {
                continue;
            };
            if let Some(entry) = table.get(&seq) {
                let count = pending.count;
                pending.clear();
                return Resolution::Action {
                    action: entry.action,
                    count,
                };
            }
            if Self::has_prefix(table, &seq) {
                pending.keys = seq.0;
                return Resolution::Pending;
            }
        }
        let was_pending = !pending.is_empty();
        pending.clear();
        if was_pending {
            Resolution::Cancelled
        } else {
            Resolution::Unbound
        }
    }

    /// Whether one of `contexts` binds `key` alone or as a prefix.
    fn binds(&self, contexts: &[Context], key: Key) -> bool {
        let seq = KeySeq(vec![key]);
        contexts.iter().any(|ctx| {
            self.tables
                .get(ctx)
                .is_some_and(|t| t.contains_key(&seq) || Self::has_prefix(t, &seq))
        })
    }

    fn has_prefix(table: &BTreeMap<KeySeq, Entry>, seq: &KeySeq) -> bool {
        use std::ops::Bound;
        table
            .range((Bound::Excluded(seq.clone()), Bound::Unbounded))
            .next()
            .is_some_and(|(k, _)| seq.is_prefix_of(k))
    }

    /// Keys that can follow a pending `prefix` in the active contexts, in
    /// preset order. For each next key the first context that knows the
    /// longer sequence decides, exactly as [`Keymap::feed`] does.
    pub fn continuations(&self, prefix: &[Key], contexts: &[Context]) -> Vec<Continuation> {
        let mut out: Vec<(u32, Continuation)> = Vec::new();
        let mut decided: Vec<Key> = Vec::new();
        let prefix_seq = KeySeq(prefix.to_vec());
        for ctx in contexts {
            let Some(table) = self.tables.get(ctx) else {
                continue;
            };
            let mut found: Vec<(Key, u32, ContinuationTarget)> = Vec::new();
            for (seq, entry) in table.range(prefix_seq.clone()..) {
                if *seq == prefix_seq {
                    continue;
                }
                if !prefix_seq.is_prefix_of(seq) {
                    break;
                }
                let next = seq.0[prefix.len()];
                if decided.contains(&next) {
                    continue;
                }
                let exact = seq.0.len() == prefix.len() + 1;
                match found.iter_mut().find(|(k, _, _)| *k == next) {
                    Some(slot) => {
                        slot.1 = slot.1.min(entry.order);
                        if exact {
                            slot.2 = ContinuationTarget::Action(entry.action);
                        }
                    }
                    None => {
                        let target = if exact {
                            ContinuationTarget::Action(entry.action)
                        } else {
                            let mut deeper = prefix.to_vec();
                            deeper.push(next);
                            ContinuationTarget::Prefix(
                                self.prefix_label(&KeySeq(deeper)).to_owned(),
                            )
                        };
                        found.push((next, entry.order, target));
                    }
                }
            }
            for (key, order, target) in found {
                decided.push(key);
                out.push((order, Continuation { key, target }));
            }
        }
        out.sort_by_key(|(order, _)| *order);
        out.into_iter().map(|(_, c)| c).collect()
    }

    /// The label of a prefix, for example "go to" for `g`.
    pub fn prefix_label(&self, prefix: &KeySeq) -> &str {
        self.prefix_labels
            .get(prefix)
            .map(String::as_str)
            .unwrap_or("more")
    }

    /// The first binding of `action` in the active contexts, by preset
    /// order: the key shown in hints. Shadowed bindings are skipped.
    pub fn keys_for(&self, action: Action, contexts: &[Context]) -> Option<KeySeq> {
        for (i, ctx) in contexts.iter().enumerate() {
            let Some(table) = self.tables.get(ctx) else {
                continue;
            };
            let best = table
                .iter()
                .filter(|(_, e)| e.action == action)
                .filter(|(seq, _)| !self.shadowed(seq, &contexts[..i]))
                .min_by_key(|(_, e)| e.order);
            if let Some((seq, _)) = best {
                return Some(seq.clone());
            }
        }
        None
    }

    fn shadowed(&self, seq: &KeySeq, earlier: &[Context]) -> bool {
        earlier.iter().any(|ctx| {
            self.tables.get(ctx).is_some_and(|t| {
                (1..=seq.0.len()).any(|n| t.contains_key(&KeySeq(seq.0[..n].to_vec())))
                    || Self::has_prefix(t, seq)
            })
        })
    }

    /// Every binding of a context, in preset order.
    pub fn bindings(&self, context: Context) -> Vec<(KeySeq, Action)> {
        let mut v: Vec<(u32, KeySeq, Action)> = self
            .tables
            .get(&context)
            .map(|t| {
                t.iter()
                    .map(|(k, e)| (e.order, k.clone(), e.action))
                    .collect()
            })
            .unwrap_or_default();
        v.sort_by_key(|(o, _, _)| *o);
        v.into_iter().map(|(_, k, a)| (k, a)).collect()
    }

    /// Finds bindings that can never fire, bindings that hide global ones,
    /// and keys legacy encoding cannot deliver. This is what
    /// `mejl keys --check` reports.
    pub fn check(&self) -> Vec<Conflict> {
        let mut out = Vec::new();
        for (&ctx, table) in &self.tables {
            for (seq, entry) in table {
                let longer: Vec<String> = table
                    .keys()
                    .filter(|k| seq.is_prefix_of(k))
                    .map(KeySeq::to_config)
                    .collect();
                if !longer.is_empty() {
                    out.push(Conflict {
                        severity: Severity::Error,
                        context: ctx,
                        keys: seq.clone(),
                        message: format!(
                            "bound to {} but also starts {}; with no timeouts those can never \
                             run. Unbind one of them",
                            entry.action.name(),
                            longer.join(", ")
                        ),
                    });
                }
                for key in &seq.0 {
                    if let Some(reason) = key.legacy_problem() {
                        out.push(Conflict {
                            severity: Severity::Warning,
                            context: ctx,
                            keys: seq.clone(),
                            message: format!(
                                "{reason}; {} works only with the kitty keyboard protocol or \
                                 tmux extended keys",
                                entry.action.name()
                            ),
                        });
                        break;
                    }
                }
                if ctx.is_modal() {
                    continue;
                }
                let Some(global) = self.tables.get(&Context::Global) else {
                    continue;
                };
                if let Some(g) = global.get(seq).filter(|g| g.action != entry.action) {
                    out.push(Conflict {
                        severity: Severity::Warning,
                        context: ctx,
                        keys: seq.clone(),
                        message: format!(
                            "{} here hides the global {}",
                            entry.action.name(),
                            g.action.name()
                        ),
                    });
                }
                if Self::has_prefix(global, seq) {
                    out.push(Conflict {
                        severity: Severity::Warning,
                        context: ctx,
                        keys: seq.clone(),
                        message: format!(
                            "{} here hides the global {} prefix",
                            entry.action.name(),
                            seq.to_config()
                        ),
                    });
                }
                for n in 1..seq.0.len() {
                    let head = KeySeq(seq.0[..n].to_vec());
                    if let Some(g) = global.get(&head) {
                        out.push(Conflict {
                            severity: Severity::Warning,
                            context: ctx,
                            keys: seq.clone(),
                            message: format!(
                                "the prefix {} here hides the global {}",
                                head.to_config(),
                                g.action.name()
                            ),
                        });
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests;
