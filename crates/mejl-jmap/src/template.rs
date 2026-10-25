//! RFC 6570 level 1 URI templates, as used by the session's download,
//! upload and EventSource URLs.

/// Expand `{var}` expressions, percent-encoding every value byte that is
/// not an unreserved character (`A-Z a-z 0-9 - . _ ~`). Unknown variables
/// expand to nothing, as RFC 6570 says for undefined values. A `{` with no
/// closing `}` is copied as is.
pub fn expand(template: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let name = &after[..close];
        if let Some(value) = lookup(name) {
            percent_encode_into(&value, &mut out);
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// The variable names in a template, in order of appearance.
pub fn variables(template: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            break;
        };
        names.push(&after[..close]);
        rest = &after[close + 1..];
    }
    names
}

/// Percent-encode everything except unreserved characters.
pub fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    percent_encode_into(value, &mut out);
    out
}

fn percent_encode_into(value: &str, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(name: &str) -> Option<String> {
        match name {
            "accountId" => Some("u1234".into()),
            "blobId" => Some("B/1+2".into()),
            "type" => Some("text/plain".into()),
            "name" => Some("Report Q3 é.pdf".into()),
            _ => None,
        }
    }

    #[test]
    fn expands_fastmail_style_download_url() {
        let url = expand(
            "https://jmap.example.com/jmap/download/{accountId}/{blobId}/{name}?type={type}",
            vars,
        );
        assert_eq!(
            url,
            "https://jmap.example.com/jmap/download/u1234/B%2F1%2B2/Report%20Q3%20%C3%A9.pdf?type=text%2Fplain"
        );
    }

    #[test]
    fn unknown_and_unclosed() {
        assert_eq!(expand("a{nope}b", vars), "ab");
        assert_eq!(expand("a{accountId", vars), "a{accountId");
        assert_eq!(expand("}{accountId}{", vars), "}u1234{");
        assert_eq!(expand("", vars), "");
        assert_eq!(expand("{}", vars), "");
    }

    #[test]
    fn reserved_characters_are_encoded() {
        assert_eq!(percent_encode("*"), "%2A");
        assert_eq!(percent_encode("a-b.c_d~e"), "a-b.c_d~e");
        assert_eq!(percent_encode("{}/?#&="), "%7B%7D%2F%3F%23%26%3D");
    }

    #[test]
    fn lists_variables() {
        assert_eq!(
            variables("https://x/?types={types}&closeafter={closeafter}&ping={ping}"),
            vec!["types", "closeafter", "ping"]
        );
        assert!(variables("https://x/jmap/event/").is_empty());
        assert!(variables("https://x/{open").is_empty());
    }
}
