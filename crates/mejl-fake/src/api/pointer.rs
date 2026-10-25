//! JSON pointer evaluation for result references (RFC 8620 section 3.7).
//!
//! This is RFC 6901 plus the JMAP `*` wildcard: when the current value is an
//! array, `*` applies the rest of the pointer to every item and collects the
//! results, flattening one level when an item's result is itself an array.

use serde_json::Value;

/// Evaluates `path` against `value`. `None` means the path is malformed or
/// does not resolve.
pub(crate) fn evaluate(value: &Value, path: &str) -> Option<Value> {
    if path.is_empty() {
        return Some(value.clone());
    }
    let rest = path.strip_prefix('/')?;
    let tokens = rest
        .split('/')
        .map(unescape)
        .collect::<Option<Vec<String>>>()?;
    walk(value, &tokens)
}

/// Undoes RFC 6901 escaping. `~1` must be handled before `~0`, so the
/// string is scanned once rather than replaced twice.
pub(crate) fn unescape(token: &str) -> Option<String> {
    let mut out = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(c) = chars.next() {
        if c == '~' {
            match chars.next() {
                Some('0') => out.push('~'),
                Some('1') => out.push('/'),
                _ => return None,
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

fn walk(value: &Value, tokens: &[String]) -> Option<Value> {
    let Some((token, rest)) = tokens.split_first() else {
        return Some(value.clone());
    };
    match value {
        Value::Object(map) => walk(map.get(token)?, rest),
        Value::Array(items) => {
            if token == "*" {
                let mut out = Vec::new();
                for item in items {
                    match walk(item, rest)? {
                        Value::Array(inner) => out.extend(inner),
                        other => out.push(other),
                    }
                }
                Some(Value::Array(out))
            } else {
                walk(items.get(array_index(token)?)?, rest)
            }
        }
        _ => None,
    }
}

/// RFC 6901 array indexes: digits only, no leading zeros.
fn array_index(token: &str) -> Option<usize> {
    if token.is_empty() || !token.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if token.len() > 1 && token.starts_with('0') {
        return None;
    }
    token.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plain_paths() {
        let v = json!({"ids": ["a", "b"], "a/b": 1, "m~n": 2, "": 3});
        assert_eq!(evaluate(&v, "/ids"), Some(json!(["a", "b"])));
        assert_eq!(evaluate(&v, "/ids/1"), Some(json!("b")));
        assert_eq!(evaluate(&v, "/a~1b"), Some(json!(1)));
        assert_eq!(evaluate(&v, "/m~0n"), Some(json!(2)));
        assert_eq!(evaluate(&v, "/"), Some(json!(3)));
        assert_eq!(evaluate(&v, ""), Some(v.clone()));
    }

    #[test]
    fn bad_paths() {
        let v = json!({"ids": ["a"], "n": 1});
        assert_eq!(evaluate(&v, "ids"), None);
        assert_eq!(evaluate(&v, "/missing"), None);
        assert_eq!(evaluate(&v, "/ids/5"), None);
        assert_eq!(evaluate(&v, "/ids/01"), None);
        assert_eq!(evaluate(&v, "/ids/-"), None);
        assert_eq!(evaluate(&v, "/n/x"), None);
        assert_eq!(evaluate(&v, "/~2"), None);
        assert_eq!(evaluate(&v, "/~"), None);
    }

    #[test]
    fn escape_order_matters() {
        // "~01" is "~1" literally, not "/".
        assert_eq!(unescape("~01").as_deref(), Some("~1"));
        assert_eq!(unescape("a~1b~0c").as_deref(), Some("a/b~c"));
    }

    #[test]
    fn wildcard_collects_and_flattens() {
        let v = json!({
            "list": [
                {"id": "t1", "emailIds": ["e1", "e2"]},
                {"id": "t2", "emailIds": ["e3"]},
            ]
        });
        assert_eq!(evaluate(&v, "/list/*/id"), Some(json!(["t1", "t2"])));
        assert_eq!(
            evaluate(&v, "/list/*/emailIds"),
            Some(json!(["e1", "e2", "e3"]))
        );
        assert_eq!(evaluate(&v, "/list/*/missing"), None);
        assert_eq!(
            evaluate(&json!({"list": []}), "/list/*/id"),
            Some(json!([]))
        );
        // `*` on an object is a key lookup.
        assert_eq!(evaluate(&json!({"*": 1}), "/*"), Some(json!(1)));
    }

    #[test]
    fn nested_wildcards() {
        let v = json!({"a": [{"b": [{"c": 1}, {"c": 2}]}, {"b": [{"c": 3}]}]});
        assert_eq!(evaluate(&v, "/a/*/b/*/c"), Some(json!([1, 2, 3])));
    }
}
