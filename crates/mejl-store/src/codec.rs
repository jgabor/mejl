//! Small conversions shared by the writer and the readers.

use std::borrow::Cow;

use jiff::Timestamp;
use mejl_jmap::Id;
use mejl_jmap::types::EmailAddress;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{Error, Result};

/// Stored timestamps are milliseconds since the Unix epoch.
pub(crate) fn ts_ms(ts: Timestamp) -> i64 {
    ts.as_millisecond()
}

/// The inverse of [`ts_ms`]. Out-of-range values clamp, because a bad
/// stored number must not make a whole page unreadable.
pub(crate) fn ms_ts(ms: i64) -> Timestamp {
    Timestamp::from_millisecond(ms).unwrap_or(if ms < 0 {
        Timestamp::MIN
    } else {
        Timestamp::MAX
    })
}

pub(crate) fn to_i64(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

pub(crate) fn to_u64(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

pub(crate) fn usize_i64(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

pub(crate) fn json<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}

pub(crate) fn opt_json<T: Serialize>(value: Option<&T>) -> Result<Option<String>> {
    value.map(json).transpose()
}

pub(crate) fn from_json<T: DeserializeOwned>(text: &str) -> Result<T> {
    Ok(serde_json::from_str(text)?)
}

/// Decodes an optional JSON column, treating NULL as the type's default.
pub(crate) fn from_opt_json<T: DeserializeOwned + Default>(text: Option<&str>) -> Result<T> {
    match text {
        Some(text) => from_json(text),
        None => Ok(T::default()),
    }
}

/// Packs an id list as length-prefixed strings. A query result can hold
/// 100k ids, and this decodes several times faster than a JSON array.
pub(crate) fn encode_ids(ids: &[Id]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(ids.len() * 16);
    for id in ids {
        let bytes = id.as_str().as_bytes();
        let len = u32::try_from(bytes.len())
            .map_err(|_| Error::Invalid("an id longer than 4 GiB".into()))?;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(bytes);
    }
    Ok(out)
}

/// The inverse of [`encode_ids`].
pub(crate) fn decode_ids(mut blob: &[u8]) -> Result<Vec<Id>> {
    let mut out = Vec::new();
    while !blob.is_empty() {
        let (len, rest) = blob
            .split_first_chunk::<4>()
            .ok_or_else(|| Error::Corrupt("truncated id list".into()))?;
        let len = usize::try_from(u32::from_le_bytes(*len))
            .map_err(|_| Error::Corrupt("id length".into()))?;
        if rest.len() < len {
            return Err(Error::Corrupt("truncated id list".into()));
        }
        let (id, rest) = rest.split_at(len);
        let id = std::str::from_utf8(id).map_err(|_| Error::Corrupt("id is not UTF-8".into()))?;
        out.push(Id::from(id));
        blob = rest;
    }
    Ok(out)
}

/// Replaces control characters with spaces. Search snippets use STX and ETX
/// as highlight markers, so they must never occur in indexed text.
pub(crate) fn clean_text(text: &str) -> Cow<'_, str> {
    let bad = |c: char| c.is_control() && c != '\n' && c != '\t';
    if !text.chars().any(bad) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.chars().map(|c| if bad(c) { ' ' } else { c }).collect())
}

/// Names and addresses as plain words for the full-text index.
pub(crate) fn addresses_text<'a>(lists: impl IntoIterator<Item = &'a [EmailAddress]>) -> String {
    let mut out = String::new();
    for list in lists {
        for addr in list {
            if let Some(name) = addr.name.as_deref().filter(|n| !n.trim().is_empty()) {
                push_word(&mut out, name);
            }
            if !addr.email.is_empty() {
                push_word(&mut out, &addr.email);
            }
        }
    }
    clean_text(&out).into_owned()
}

fn push_word(out: &mut String, word: &str) {
    if !out.is_empty() {
        out.push(' ');
    }
    out.push_str(word.trim());
}

/// Escapes `%`, `_` and `\` for a LIKE pattern that uses `ESCAPE '\'`.
pub(crate) fn like_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    for c in text.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Lowercased words of a name, each preceded by a space, so a prefix match
/// on any word is `LIKE '% pre%'`.
pub(crate) fn name_words(name: &str) -> String {
    let mut out = String::new();
    for word in name.split(|c: char| c.is_whitespace() || c == ',' || c == '"') {
        if word.is_empty() {
            continue;
        }
        out.push(' ');
        out.push_str(&word.to_lowercase());
    }
    out
}

/// The address inside a header value such as `<a@example.org>` or
/// `Name <a@example.org>`, lowercased for matching.
pub(crate) fn normalise_address(value: &str) -> String {
    let value = value.trim();
    let inner = match (value.rfind('<'), value.rfind('>')) {
        (Some(start), Some(end)) if start < end => &value[start + 1..end],
        _ => value,
    };
    inner.trim().to_lowercase()
}

/// Rough text from HTML for the search index when a message has no plain
/// part. Rendering for display is mejl-render's job; this only has to give
/// the tokenizer the right words.
pub(crate) fn html_to_index_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        push_decoded(&mut out, &rest[..lt]);
        let tag = &rest[lt..];
        let lower_start: String = tag.chars().take(8).collect::<String>().to_ascii_lowercase();
        let skip_to = if lower_start.starts_with("<script") {
            Some("</script")
        } else if lower_start.starts_with("<style") {
            Some("</style")
        } else if lower_start.starts_with("<!--") {
            Some("-->")
        } else {
            None
        };
        if let Some(end_marker) = skip_to {
            match find_ascii_ci(tag, end_marker) {
                Some(pos) => {
                    let after = &tag[pos..];
                    rest = after.find('>').map_or("", |gt| &after[gt + 1..]);
                }
                None => rest = "",
            }
            out.push(' ');
            continue;
        }
        match tag.find('>') {
            Some(gt) => rest = &tag[gt + 1..],
            None => rest = "",
        }
        out.push(' ');
    }
    push_decoded(&mut out, rest);
    let collapsed: Vec<&str> = out.split_whitespace().collect();
    clean_text(&collapsed.join(" ")).into_owned()
}

fn find_ascii_ci(haystack: &str, needle: &str) -> Option<usize> {
    let hay = haystack.as_bytes();
    let needle = needle.as_bytes();
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()].eq_ignore_ascii_case(needle))
}

fn push_decoded(out: &mut String, text: &str) {
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let entity = &rest[amp..];
        let decoded = [
            ("&amp;", "&"),
            ("&lt;", "<"),
            ("&gt;", ">"),
            ("&quot;", "\""),
            ("&#39;", "'"),
            ("&apos;", "'"),
            ("&nbsp;", " "),
        ]
        .into_iter()
        .find(|(name, _)| entity.starts_with(name));
        match decoded {
            Some((name, value)) => {
                out.push_str(value);
                rest = &entity[name.len()..];
            }
            None => {
                out.push('&');
                rest = &entity[1..];
            }
        }
    }
    out.push_str(rest);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_lists_round_trip() {
        let ids: Vec<Id> = ["Ma", "", "ünï", "x".repeat(300).as_str()]
            .into_iter()
            .map(Id::from)
            .collect();
        let blob = encode_ids(&ids).unwrap();
        assert_eq!(decode_ids(&blob).unwrap(), ids);
        assert!(decode_ids(&[]).unwrap().is_empty());
    }

    #[test]
    fn truncated_id_lists_are_errors() {
        let blob = encode_ids(&[Id::from("abcdef")]).unwrap();
        assert!(decode_ids(&blob[..blob.len() - 1]).is_err());
        assert!(decode_ids(&blob[..3]).is_err());
        assert!(decode_ids(&[1, 0, 0, 0, 0xff]).is_err());
    }

    #[test]
    fn control_characters_are_replaced() {
        assert!(matches!(clean_text("plain\ttext\n"), Cow::Borrowed(_)));
        assert_eq!(clean_text("a\u{2}b\u{3}c\u{7f}"), "a b c ");
    }

    #[test]
    fn address_words() {
        let list = vec![
            EmailAddress::with_name("Juniper Tran", "juniper@example.org"),
            EmailAddress::new("solo@example.net"),
            EmailAddress::with_name("  ", "blank@example.com"),
        ];
        assert_eq!(
            addresses_text([list.as_slice()]),
            "Juniper Tran juniper@example.org solo@example.net blank@example.com"
        );
    }

    #[test]
    fn like_patterns_escape_wildcards() {
        assert_eq!(like_escape(r"50%_off\now"), r"50\%\_off\\now");
    }

    #[test]
    fn name_words_are_lowercase_with_leading_spaces() {
        assert_eq!(name_words("Ångström, Åsa  Lind"), " ångström åsa lind");
        assert_eq!(name_words(""), "");
    }

    #[test]
    fn addresses_are_normalised() {
        assert_eq!(
            normalise_address(" <Kettle.Fern@Example.org> "),
            "kettle.fern@example.org"
        );
        assert_eq!(normalise_address("Shop <a@example.com>"), "a@example.com");
        assert_eq!(normalise_address("plain@example.net"), "plain@example.net");
    }

    #[test]
    fn html_index_text() {
        let html = "<html><head><style>p{color:red}</style><script>var x=1;</script></head>\
                    <body><!-- hidden --><p>Fish &amp; chips</p><p>at&nbsp;noon &copy;</p></body></html>";
        assert_eq!(html_to_index_text(html), "Fish & chips at noon &copy;");
        assert_eq!(html_to_index_text("no tags"), "no tags");
        assert_eq!(html_to_index_text("<b>unclosed"), "unclosed");
        assert_eq!(html_to_index_text("<script>never ends"), "");
    }

    #[test]
    fn timestamps_clamp() {
        assert_eq!(ms_ts(i64::MAX), Timestamp::MAX);
        assert_eq!(ms_ts(i64::MIN), Timestamp::MIN);
        let ts: Timestamp = "2026-10-23T14:27:00Z".parse().unwrap();
        assert_eq!(ms_ts(ts_ms(ts)), ts);
    }
}
