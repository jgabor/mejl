//! How a JMAP patch changes an email's keywords and mailboxes.
//!
//! The store uses these functions to apply pending outbox entries on top of
//! server truth, to compute inverse patches for undo and to collapse several
//! patches into one minimal patch at flush time. They are public so the UI
//! can apply the same rules to the rows it already holds in memory, which is
//! what makes an action show in the same frame: the store's own overlay
//! lands one round trip through the store thread later.
//!
//! Only `keywords`, `keywords/<k>`, `mailboxIds` and `mailboxIds/<id>` paths
//! are understood. Other paths are passed to the server unchanged and have
//! no local effect. Keywords are case-insensitive in JMAP, so they are
//! compared in lowercase.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use mejl_jmap::patch::{pointer, unescape_segment};
use mejl_jmap::{Id, Patch};
use serde_json::{Map, Value};

/// The parts of an email that mejl's optimistic changes touch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmailFlags {
    /// Set keywords, lowercased.
    pub keywords: BTreeSet<String>,
    /// Mailbox ids the email is in.
    pub mailbox_ids: BTreeSet<String>,
}

enum PathKind<'a> {
    Keywords,
    Keyword(Cow<'a, str>),
    Mailboxes,
    Mailbox(Cow<'a, str>),
    Other,
}

fn classify(path: &str) -> PathKind<'_> {
    match path.split_once('/') {
        None => match path {
            "keywords" => PathKind::Keywords,
            "mailboxIds" => PathKind::Mailboxes,
            _ => PathKind::Other,
        },
        Some((head, rest)) => {
            if rest.contains('/') {
                return PathKind::Other;
            }
            let Some(segment) = unescape_segment(rest) else {
                return PathKind::Other;
            };
            match head {
                "keywords" => PathKind::Keyword(segment),
                "mailboxIds" => PathKind::Mailbox(segment),
                _ => PathKind::Other,
            }
        }
    }
}

fn truthy(value: &Value) -> bool {
    matches!(value, Value::Bool(true))
}

fn set_of(value: &Value, lowercase: bool) -> BTreeSet<String> {
    value
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, v)| truthy(v))
        .map(|(k, _)| {
            if lowercase {
                k.to_lowercase()
            } else {
                k.clone()
            }
        })
        .collect()
}

fn object_of<'a>(items: impl IntoIterator<Item = &'a String>) -> Value {
    Value::Object(
        items
            .into_iter()
            .map(|k| (k.clone(), Value::Bool(true)))
            .collect(),
    )
}

impl EmailFlags {
    /// Flags from the maps of a JMAP Email. Keywords set to `false` are
    /// ignored, as RFC 8621 says they must not appear.
    pub fn from_maps(
        keywords: Option<&BTreeMap<String, bool>>,
        mailbox_ids: Option<&BTreeMap<Id, bool>>,
    ) -> Self {
        Self {
            keywords: keywords
                .into_iter()
                .flatten()
                .filter(|(_, set)| **set)
                .map(|(k, _)| k.to_lowercase())
                .collect(),
            mailbox_ids: mailbox_ids
                .into_iter()
                .flatten()
                .filter(|(_, member)| **member)
                .map(|(id, _)| id.as_str().to_owned())
                .collect(),
        }
    }

    /// Whether a keyword is set, ignoring case.
    pub fn has_keyword(&self, keyword: &str) -> bool {
        self.keywords.contains(&keyword.to_lowercase())
    }

    /// Whether the email is in a mailbox.
    pub fn in_mailbox(&self, mailbox_id: &str) -> bool {
        self.mailbox_ids.contains(mailbox_id)
    }

    /// Apply a patch in place.
    pub fn apply(&mut self, patch: &Patch) {
        for (path, value) in patch.as_map() {
            match classify(path) {
                PathKind::Keywords => self.keywords = set_of(value, true),
                PathKind::Keyword(k) => {
                    let k = k.to_lowercase();
                    if truthy(value) {
                        self.keywords.insert(k);
                    } else {
                        self.keywords.remove(&k);
                    }
                }
                PathKind::Mailboxes => self.mailbox_ids = set_of(value, false),
                PathKind::Mailbox(m) => {
                    if truthy(value) {
                        self.mailbox_ids.insert(m.into_owned());
                    } else {
                        self.mailbox_ids.remove(m.as_ref());
                    }
                }
                PathKind::Other => {}
            }
        }
    }

    /// A copy with a patch applied.
    pub fn applied(&self, patch: &Patch) -> Self {
        let mut out = self.clone();
        out.apply(patch);
        out
    }

    /// Whether applying `patch` would change nothing that this type tracks.
    pub fn reflects(&self, patch: &Patch) -> bool {
        self.applied(patch) == *self
    }

    /// The patch that restores these flags after `patch` was applied to
    /// them. Paths the store does not understand get no inverse.
    pub fn inverse(&self, patch: &Patch) -> Patch {
        let mut out = Patch::new();
        for path in patch.as_map().keys() {
            let value = match classify(path) {
                PathKind::Keywords => object_of(&self.keywords),
                PathKind::Keyword(k) => present(self.has_keyword(&k)),
                PathKind::Mailboxes => object_of(&self.mailbox_ids),
                PathKind::Mailbox(m) => present(self.in_mailbox(&m)),
                PathKind::Other => continue,
            };
            out.insert_raw(path.clone(), value);
        }
        out
    }

    /// The smallest patch of single keyword and mailbox changes that turns
    /// these flags into `target`. Empty when they are equal.
    pub fn diff(&self, target: &EmailFlags) -> Patch {
        let mut out = Patch::new();
        for k in target.keywords.difference(&self.keywords) {
            out = out.keyword(k, true);
        }
        for k in self.keywords.difference(&target.keywords) {
            out = out.keyword(k, false);
        }
        for m in target.mailbox_ids.difference(&self.mailbox_ids) {
            out = out.mailbox(m, true);
        }
        for m in self.mailbox_ids.difference(&target.mailbox_ids) {
            out = out.mailbox(m, false);
        }
        out
    }
}

fn present(on: bool) -> Value {
    if on { Value::Bool(true) } else { Value::Null }
}

/// The entries of a patch that touch neither keywords nor mailboxes.
pub fn untracked_paths(patch: &Patch) -> Map<String, Value> {
    patch
        .as_map()
        .iter()
        .filter(|(path, _)| matches!(classify(path), PathKind::Other))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Whether a patch mentions a mailbox id, as a path or as a key of a whole
/// `mailboxIds` value.
pub fn references_mailbox(patch: &Patch, mailbox_id: &str) -> bool {
    patch
        .as_map()
        .iter()
        .any(|(path, value)| match classify(path) {
            PathKind::Mailbox(m) => m == mailbox_id,
            PathKind::Mailboxes => value
                .as_object()
                .is_some_and(|o| o.contains_key(mailbox_id)),
            _ => false,
        })
}

/// A copy of a patch with mailbox id `from` replaced by `to`. Used when a
/// locally created mailbox gets its real server id.
pub fn rename_mailbox(patch: &Patch, from: &str, to: &str) -> Patch {
    let mut out = Patch::new();
    for (path, value) in patch.as_map() {
        match classify(path) {
            PathKind::Mailbox(m) if m == from => {
                out.insert_raw(pointer(["mailboxIds", to]), value.clone());
            }
            PathKind::Mailboxes => {
                let renamed = match value {
                    Value::Object(map) => Value::Object(
                        map.iter()
                            .map(|(k, v)| {
                                let key = if k == from { to.to_owned() } else { k.clone() };
                                (key, v.clone())
                            })
                            .collect(),
                    ),
                    other => other.clone(),
                };
                out.insert_raw(path.clone(), renamed);
            }
            _ => out.insert_raw(path.clone(), value.clone()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn flags(keywords: &[&str], mailboxes: &[&str]) -> EmailFlags {
        EmailFlags {
            keywords: keywords.iter().map(|s| s.to_string()).collect(),
            mailbox_ids: mailboxes.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn patch(value: Value) -> Patch {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn applies_sub_paths_and_whole_values() {
        let mut f = flags(&["$seen"], &["inbox"]);
        f.apply(
            &Patch::new()
                .keyword("$Flagged", true)
                .keyword("$seen", false),
        );
        assert_eq!(f, flags(&["$flagged"], &["inbox"]));
        f.apply(
            &Patch::new()
                .mailbox("inbox", false)
                .mailbox("archive", true),
        );
        assert_eq!(f, flags(&["$flagged"], &["archive"]));
        f.apply(&patch(
            json!({"keywords": {"$Draft": true, "$junk": false}}),
        ));
        assert_eq!(f.keywords, BTreeSet::from(["$draft".to_owned()]));
        f.apply(&patch(json!({"mailboxIds": {"a": true, "b": true}})));
        assert_eq!(f.mailbox_ids.len(), 2);
    }

    #[test]
    fn unknown_paths_do_nothing() {
        let f = flags(&["$seen"], &["inbox"]);
        let p = patch(json!({"subject": "x", "keywords/a/b": true, "keywords/bad~2": true}));
        assert!(f.reflects(&p));
        assert_eq!(untracked_paths(&p).len(), 3);
    }

    #[test]
    fn escaped_segments_are_decoded() {
        let mut f = EmailFlags::default();
        f.apply(&Patch::new().mailbox("odd/id~x", true));
        assert!(f.in_mailbox("odd/id~x"));
    }

    #[test]
    fn inverse_restores() {
        let before = flags(&["$seen"], &["inbox"]);
        let p = Patch::new()
            .keyword("$seen", false)
            .keyword("$flagged", true)
            .mailbox("inbox", false)
            .mailbox("archive", true);
        let inverse = before.inverse(&p);
        let after = before.applied(&p);
        assert_eq!(after.applied(&inverse), before);

        let whole = patch(json!({"mailboxIds": {"trash": true}}));
        let inverse = before.inverse(&whole);
        assert_eq!(before.applied(&whole).applied(&inverse), before);
    }

    #[test]
    fn diff_is_minimal() {
        let a = flags(&["$seen", "x"], &["inbox"]);
        let b = flags(&["$seen", "$flagged"], &["archive"]);
        let d = a.diff(&b);
        assert_eq!(
            serde_json::to_value(&d).unwrap(),
            json!({"keywords/$flagged": true, "keywords/x": null, "mailboxIds/archive": true, "mailboxIds/inbox": null})
        );
        assert_eq!(a.applied(&d), b);
        assert!(a.diff(&a).is_empty());
    }

    #[test]
    fn rename_and_reference() {
        let p = patch(
            json!({"mailboxIds/local:3": true, "mailboxIds/inbox": null, "keywords/$seen": true}),
        );
        assert!(references_mailbox(&p, "local:3"));
        let r = rename_mailbox(&p, "local:3", "M9");
        assert!(!references_mailbox(&r, "local:3"));
        assert!(references_mailbox(&r, "M9"));
        assert_eq!(r.as_map().len(), 3);

        let whole = patch(json!({"mailboxIds": {"local:3": true}}));
        assert!(references_mailbox(&whole, "local:3"));
        let r = rename_mailbox(&whole, "local:3", "M9");
        assert_eq!(r.as_map()["mailboxIds"], json!({"M9": true}));
    }
}
