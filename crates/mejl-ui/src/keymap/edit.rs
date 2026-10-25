//! What the key editor asks of a keymap: what a key does now, which keys a
//! binding has, and the overrides that turn one keymap into another, which
//! is what a custom preset stores.

use super::{Binding, Context, KeyOverrides, KeySeq, Keymap, Override};

/// What a key sequence does, for conflict checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lookup {
    /// Nothing uses it.
    Free,
    /// It runs a binding.
    Bound {
        /// Where.
        context: Context,
        /// What it runs.
        binding: Binding,
    },
    /// It starts longer bindings, as `g` starts `g i`. With no timeouts, a
    /// binding on it could never run alongside them.
    Longer {
        /// Where.
        context: Context,
        /// One of the longer bindings.
        example: KeySeq,
    },
    /// Its first keys already run a binding of their own, so the rest of
    /// the sequence is never read.
    Shorter {
        /// Where.
        context: Context,
        /// The shorter keys.
        keys: KeySeq,
        /// What they run.
        binding: Binding,
    },
}

impl Keymap {
    /// What `keys` does in the first of `contexts` that knows any part of
    /// it, the way [`Keymap::feed`] walks the contexts. It errs on the side
    /// of a conflict where feed would fall through to a later context.
    pub fn lookup(&self, contexts: &[Context], keys: &KeySeq) -> Lookup {
        for &context in contexts {
            let Some(table) = self.tables.get(&context) else {
                continue;
            };
            for n in 1..keys.0.len() {
                let head = KeySeq(keys.0[..n].to_vec());
                if let Some(e) = table.get(&head) {
                    return Lookup::Shorter {
                        context,
                        keys: head,
                        binding: e.binding,
                    };
                }
            }
            if let Some(e) = table.get(keys) {
                return Lookup::Bound {
                    context,
                    binding: e.binding,
                };
            }
            if let Some(example) = table.keys().find(|k| keys.is_prefix_of(k) && *k != keys) {
                return Lookup::Longer {
                    context,
                    example: example.clone(),
                };
            }
        }
        Lookup::Free
    }

    /// The binding of exactly `keys` in one context.
    pub fn get(&self, context: Context, keys: &KeySeq) -> Option<Binding> {
        self.tables
            .get(&context)
            .and_then(|t| t.get(keys))
            .map(|e| e.binding)
    }

    /// Every key of a binding in one context, the key hints show first.
    pub fn keys_of(&self, context: Context, binding: Binding) -> Vec<KeySeq> {
        let mut keys: Vec<(&(u32, u32), &KeySeq)> = self
            .tables
            .get(&context)
            .map(|t| {
                t.iter()
                    .filter(|(_, e)| e.binding == binding)
                    .map(|(k, e)| (&e.order, k))
                    .collect()
            })
            .unwrap_or_default();
        keys.sort();
        keys.into_iter().map(|(_, k)| k.clone()).collect()
    }

    /// Whether `keys` is the start of a longer binding in `context`, so a
    /// key capture should wait for the next key.
    pub fn is_prefix(&self, context: Context, keys: &KeySeq) -> bool {
        self.tables
            .get(&context)
            .is_some_and(|t| t.keys().any(|k| keys.is_prefix_of(k) && k != keys))
    }

    /// The overrides that turn `base` into this keymap: an unbind for each
    /// key it lost and a bind for each key it gained or changed. Only
    /// actions count; macro keys are not part of a preset.
    ///
    /// The result sorts the way a config table does, and applying it to
    /// `base` in that order gives back this keymap's bindings.
    pub fn diff_from(&self, base: &Keymap) -> KeyOverrides {
        let mut entries = Vec::new();
        for context in Context::ALL {
            let action_of = |map: &Keymap, keys: &KeySeq| match map.get(context, keys) {
                Some(Binding::Action(a)) => Some(a),
                _ => None,
            };
            let mut keys: Vec<&KeySeq> = Vec::new();
            for map in [base, self] {
                if let Some(t) = map.tables.get(&context) {
                    keys.extend(t.keys());
                }
            }
            keys.sort_by_key(|k| k.to_config());
            keys.dedup();
            for k in keys {
                let (was, now) = (action_of(base, k), action_of(self, k));
                if was != now {
                    entries.push(Override {
                        context,
                        keys: k.clone(),
                        action: now,
                    });
                }
            }
        }
        KeyOverrides::from_entries(entries)
    }

    /// Every action binding, by context and keys, for comparing keymaps.
    pub fn action_bindings(&self) -> Vec<(Context, KeySeq, Binding)> {
        let mut out = Vec::new();
        for (&context, table) in &self.tables {
            for (k, e) in table {
                if matches!(e.binding, Binding::Action(_)) {
                    out.push((context, k.clone(), e.binding));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::Preset;
    use super::*;
    use crate::action::Action;

    fn seq(s: &str) -> KeySeq {
        s.parse().unwrap()
    }

    #[test]
    fn lookup_reports_exact_and_prefix_clashes() {
        let km = Keymap::preset(Preset::Fastmail);
        let mail = [Context::List, Context::Global];
        assert_eq!(
            km.lookup(&mail, &seq("y")),
            Lookup::Bound {
                context: Context::List,
                binding: Binding::Action(Action::Archive)
            }
        );
        assert!(matches!(
            km.lookup(&mail, &seq("g")),
            Lookup::Longer {
                context: Context::Global,
                ..
            }
        ));
        assert!(matches!(
            km.lookup(&mail, &seq("y x")),
            Lookup::Shorter { context: Context::List, ref keys, .. } if *keys == seq("y")
        ));
        assert_eq!(km.lookup(&mail, &seq("g x")), Lookup::Free);
        assert_eq!(km.lookup(&mail, &seq("F5")), Lookup::Free);
        assert!(km.is_prefix(Context::List, &seq(".")));
        assert!(!km.is_prefix(Context::List, &seq("y")));
    }

    #[test]
    fn keys_of_puts_the_first_key_first() {
        let mut km = Keymap::preset(Preset::Fastmail);
        assert_eq!(
            km.keys_of(Context::Global, Binding::Action(Action::Palette)),
            vec![seq(":"), seq("ctrl-k"), seq("ctrl-p")]
        );
        km.bind(Context::List, seq("F2"), Action::Archive);
        assert_eq!(
            km.keys_of(Context::List, Binding::Action(Action::Archive)),
            vec![seq("y"), seq("F2")],
            "an added key comes after the first"
        );
        assert_eq!(
            km.keys_for(Action::Archive, &[Context::List]),
            Some(seq("y"))
        );
    }

    #[test]
    fn a_diff_applied_to_the_base_gives_the_edited_keymap() {
        for preset in Preset::ALL {
            let base = Keymap::preset(preset);
            let mut edited = base.clone();
            edited.unbind(Context::List, &seq("y"));
            edited.unbind(Context::Reader, &seq("y"));
            edited.bind(Context::List, seq("e"), Action::Archive);
            edited.bind(Context::List, seq("y x"), Action::Trash);
            edited.unbind(Context::List, &seq("b 3"));
            edited.bind(Context::Global, seq("F1"), Action::Help);
            edited.bind(Context::List, seq("d"), Action::ReportSpam);
            let diff = edited.diff_from(&base);
            assert!(!diff.entries().is_empty());
            let mut rebuilt = base.clone();
            rebuilt.apply(&diff);
            assert_eq!(
                rebuilt.action_bindings(),
                edited.action_bindings(),
                "{preset:?}"
            );
            // And through the config table, which sorts its keys.
            let reread = KeyOverrides::from_table(&diff.to_table()).unwrap();
            let mut again = base.clone();
            again.apply(&reread);
            assert_eq!(again.action_bindings(), edited.action_bindings());
            assert!(base.diff_from(&base).entries().is_empty());
        }
    }
}
