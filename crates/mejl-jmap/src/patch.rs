//! Patch objects for `/set` updates (RFC 8620 section 5.3).
//!
//! A patch maps JSON pointer paths (without the leading `/`) to new values.
//! `null` removes a property or map entry. Keys inside a path are escaped
//! per RFC 6901, so a keyword or id that contains `/` or `~` stays one
//! segment.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Escape one JSON pointer segment: `~` becomes `~0` and `/` becomes `~1`.
pub fn escape_segment(segment: &str) -> Cow<'_, str> {
    if !segment.contains(['~', '/']) {
        return Cow::Borrowed(segment);
    }
    let mut out = String::with_capacity(segment.len() + 4);
    for ch in segment.chars() {
        match ch {
            '~' => out.push_str("~0"),
            '/' => out.push_str("~1"),
            other => out.push(other),
        }
    }
    Cow::Owned(out)
}

/// Undo [`escape_segment`]. Returns `None` for a `~` not followed by `0` or
/// `1`, which RFC 6901 does not allow.
pub fn unescape_segment(segment: &str) -> Option<Cow<'_, str>> {
    if !segment.contains('~') {
        return Some(Cow::Borrowed(segment));
    }
    let mut out = String::with_capacity(segment.len());
    let mut chars = segment.chars();
    while let Some(ch) = chars.next() {
        if ch == '~' {
            match chars.next() {
                Some('0') => out.push('~'),
                Some('1') => out.push('/'),
                _ => return None,
            }
        } else {
            out.push(ch);
        }
    }
    Some(Cow::Owned(out))
}

/// Join segments into a patch path, escaping each one.
pub fn pointer<'a>(segments: impl IntoIterator<Item = &'a str>) -> String {
    let mut out = String::new();
    for (i, segment) in segments.into_iter().enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(&escape_segment(segment));
    }
    out
}

/// A `/set` update patch.
///
/// ```
/// use mejl_jmap::Patch;
///
/// let patch = Patch::new()
///     .keyword("$seen", true)
///     .mailbox("inbox-id", false);
/// assert_eq!(
///     serde_json::to_string(&patch).unwrap(),
///     r#"{"keywords/$seen":true,"mailboxIds/inbox-id":null}"#
/// );
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Patch(Map<String, Value>);

impl Patch {
    /// An empty patch.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a whole top-level property, for example `subject`.
    pub fn property(self, name: &str, value: impl Into<Value>) -> Self {
        self.path([name], value)
    }

    /// Set a value at a nested path. Each segment is escaped.
    pub fn path<'a>(
        mut self,
        segments: impl IntoIterator<Item = &'a str>,
        value: impl Into<Value>,
    ) -> Self {
        self.0.insert(pointer(segments), value.into());
        self
    }

    /// Add (`true`) or remove (`false`) a keyword.
    pub fn keyword(self, keyword: &str, set: bool) -> Self {
        self.path(["keywords", keyword], present(set))
    }

    /// Add (`true`) or remove (`false`) mailbox membership.
    pub fn mailbox(self, mailbox_id: &str, member: bool) -> Self {
        self.path(["mailboxIds", mailbox_id], present(member))
    }

    /// Insert a raw, already escaped path. Prefer [`Patch::path`].
    pub fn insert_raw(&mut self, path: impl Into<String>, value: Value) {
        self.0.insert(path.into(), value);
    }

    /// Merge another patch into this one. Later values win.
    pub fn merge(&mut self, other: Patch) {
        self.0.extend(other.0);
    }

    /// Whether the patch changes nothing.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The path to value map.
    pub fn as_map(&self) -> &Map<String, Value> {
        &self.0
    }

    /// Unwrap into the path to value map.
    pub fn into_map(self) -> Map<String, Value> {
        self.0
    }
}

impl From<Map<String, Value>> for Patch {
    fn from(map: Map<String, Value>) -> Self {
        Self(map)
    }
}

// Membership maps take `true` to add and `null` to remove. `false` is not
// valid there.
fn present(on: bool) -> Value {
    if on { Value::Bool(true) } else { Value::Null }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn escapes_tilde_before_slash() {
        assert_eq!(escape_segment("plain"), "plain");
        assert_eq!(escape_segment("a/b"), "a~1b");
        assert_eq!(escape_segment("a~b"), "a~0b");
        // "~1" literally must not turn into "/" on the way back.
        assert_eq!(escape_segment("~1"), "~01");
        assert_eq!(escape_segment("/~/"), "~1~0~1");
        assert_eq!(escape_segment(""), "");
    }

    #[test]
    fn unescape_round_trips() {
        for s in ["plain", "a/b", "a~b", "~1", "/~/", "~~//", "", "ünï/cødé~"] {
            let escaped = escape_segment(s);
            assert_eq!(unescape_segment(&escaped).as_deref(), Some(s), "{s}");
        }
        assert!(unescape_segment("bad~").is_none());
        assert!(unescape_segment("bad~2").is_none());
    }

    #[test]
    fn keyword_and_mailbox_patches() {
        let patch = Patch::new()
            .keyword("$seen", true)
            .keyword("$flagged", false)
            .mailbox("M/1", true)
            .mailbox("M2", false);
        assert_eq!(
            serde_json::to_value(&patch).unwrap(),
            json!({
                "keywords/$seen": true,
                "keywords/$flagged": null,
                "mailboxIds/M~11": true,
                "mailboxIds/M2": null,
            })
        );
    }

    #[test]
    fn keyword_with_tilde() {
        let patch = Patch::new().keyword("~weird/kw", true);
        assert!(patch.as_map().contains_key("keywords/~0weird~1kw"));
    }

    #[test]
    fn property_and_merge() {
        let mut a = Patch::new().property("subject", "Hi");
        a.merge(
            Patch::new()
                .property("subject", "Hello")
                .keyword("$draft", false),
        );
        assert_eq!(a.as_map()["subject"], "Hello");
        assert_eq!(a.as_map()["keywords/$draft"], Value::Null);
        assert!(!a.is_empty());
        assert!(Patch::new().is_empty());
    }

    #[test]
    fn pointer_joins_segments() {
        assert_eq!(pointer(["mailboxIds", "a/b"]), "mailboxIds/a~1b");
        assert_eq!(pointer([]), "");
    }
}
