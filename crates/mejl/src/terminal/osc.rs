//! OSC 52 clipboard writes and OSC 8 hyperlinks.
//!
//! Both are plain escape sequences, written here by hand: the OSC 52
//! crates on crates.io are copyleft, and the sequences are small.
//!
//! - OSC 52 sets the system clipboard through the terminal, which also
//!   works over SSH. tmux passes it on only with `set -g set-clipboard on`.
//! - OSC 8 makes text a link the terminal opens itself. Terminals without
//!   support show the text. There is no way to detect support.
//!
//! Text from email ends up in these sequences, so everything is checked:
//! a stray ESC or BEL inside a URL would end the sequence early and let the
//! rest of the URL run as terminal commands.

/// The largest text [`osc52_copy`] accepts by default, in bytes. Its base64
/// form plus the sequence stays under 100 000 bytes, the smallest limit in
/// common terminals (hterm). Larger copies fail loudly instead of being cut.
pub const OSC52_MAX_BYTES: usize = 74_990;

/// Why a copy cannot be sent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClipboardError {
    /// The text is over the cap.
    #[error("{len} bytes is too much to copy through the terminal (the limit is {max})")]
    TooLarge {
        /// The text's size.
        len: usize,
        /// The cap.
        max: usize,
    },
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding (RFC 4648 section 4).
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let sextet = |shift: u32| char::from(BASE64[((n >> shift) & 0x3f) as usize]);
        out.push(sextet(18));
        out.push(sextet(12));
        out.push(if chunk.len() > 1 { sextet(6) } else { '=' });
        out.push(if chunk.len() > 2 { sextet(0) } else { '=' });
    }
    out
}

/// The OSC 52 sequence that puts `text` on the clipboard, capped at
/// [`OSC52_MAX_BYTES`].
pub fn osc52_copy(text: &str) -> Result<String, ClipboardError> {
    osc52_copy_capped(text, OSC52_MAX_BYTES)
}

/// [`osc52_copy`] with another cap.
pub fn osc52_copy_capped(text: &str, max: usize) -> Result<String, ClipboardError> {
    if text.len() > max {
        return Err(ClipboardError::TooLarge {
            len: text.len(),
            max,
        });
    }
    // "c" is the clipboard selection. ST ends it; every terminal that
    // supports OSC 52 accepts ST.
    Ok(format!(
        "\x1b]52;c;{}\x1b\\",
        base64_encode(text.as_bytes())
    ))
}

/// Schemes [`hyperlink`] turns into links. Anything else, such as
/// `javascript:` or `file:`, stays plain text, because the terminal would
/// open it without asking.
pub const LINK_SCHEMES: &[&str] = &["http", "https", "mailto"];

/// The longest URL put in a link. Longer ones stay plain text; GNU screen
/// and some terminals corrupt the screen on very long OSC 8 URLs.
pub const MAX_LINK_URL: usize = 2048;

/// `text` wrapped in an OSC 8 hyperlink to `url`.
///
/// The URL is percent-encoded outside printable ASCII. When it has another
/// scheme, is too long or has no scheme, the result is the text alone.
/// Control characters in the text are dropped either way.
pub fn hyperlink(url: &str, text: &str) -> String {
    let text: String = text.chars().filter(|c| !c.is_control()).collect();
    match link_target(url) {
        Some(target) => format!("\x1b]8;;{target}\x1b\\{text}\x1b]8;;\x1b\\"),
        None => text,
    }
}

/// The URL as it may appear inside OSC 8, or `None` when it must not be a
/// link.
pub fn link_target(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, _) = url.split_once(':')?;
    if !LINK_SCHEMES.iter().any(|s| s.eq_ignore_ascii_case(scheme)) {
        return None;
    }
    let mut out = String::with_capacity(url.len());
    for b in url.bytes() {
        // Printable ASCII except space passes; everything else is encoded,
        // which also removes ESC, BEL and C1 controls.
        if (0x21..=0x7e).contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    (out.len() <= MAX_LINK_URL).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc_4648_vectors() {
        let cases = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (input, want) in cases {
            assert_eq!(base64_encode(input.as_bytes()), want, "{input:?}");
        }
        assert_eq!(base64_encode(&[0xff, 0xfe, 0xfd]), "//79");
        assert_eq!(base64_encode("å".as_bytes()), "w6U=");
    }

    #[test]
    fn osc52_sequence_shape() {
        assert_eq!(osc52_copy("hi").unwrap(), "\x1b]52;c;aGk=\x1b\\");
        assert_eq!(osc52_copy("").unwrap(), "\x1b]52;c;\x1b\\");
    }

    #[test]
    fn osc52_cap_is_enforced() {
        let at_cap = "a".repeat(OSC52_MAX_BYTES);
        let seq = osc52_copy(&at_cap).unwrap();
        assert!(seq.len() < 100_000, "{}", seq.len());
        let over = "a".repeat(OSC52_MAX_BYTES + 1);
        assert_eq!(
            osc52_copy(&over).unwrap_err(),
            ClipboardError::TooLarge {
                len: OSC52_MAX_BYTES + 1,
                max: OSC52_MAX_BYTES
            }
        );
        assert!(osc52_copy_capped("abcd", 3).is_err());
        assert!(osc52_copy_capped("abc", 3).is_ok());
    }

    #[test]
    fn osc52_payload_never_holds_raw_escapes() {
        let seq = osc52_copy("evil\x1b]0;title\x07").unwrap();
        let payload = &seq["\x1b]52;c;".len()..seq.len() - 2];
        assert!(
            payload
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
        );
    }

    #[test]
    fn hyperlink_wraps_safe_urls() {
        assert_eq!(
            hyperlink("https://example.com/a?b=c", "site"),
            "\x1b]8;;https://example.com/a?b=c\x1b\\site\x1b]8;;\x1b\\"
        );
        assert!(hyperlink("mailto:ada@example.com", "Ada").starts_with("\x1b]8;;mailto:"));
        assert!(hyperlink("HTTPS://EXAMPLE.COM", "x").contains("HTTPS://EXAMPLE.COM"));
    }

    #[test]
    fn hyperlink_refuses_other_schemes() {
        assert_eq!(hyperlink("javascript:alert(1)", "x"), "x");
        assert_eq!(hyperlink("file:///etc/passwd", "x"), "x");
        assert_eq!(hyperlink("no scheme", "x"), "x");
        assert_eq!(
            hyperlink(&format!("https://e.com/{}", "a".repeat(3000)), "x"),
            "x"
        );
    }

    #[test]
    fn hyperlink_encodes_escapes_and_spaces() {
        let link = hyperlink("https://example.com/\x1b]0;pwned\x07 x\u{9b}é", "t\x1bext");
        let target = link_target("https://example.com/\x1b]0;pwned\x07 x\u{9b}é").unwrap();
        assert_eq!(target, "https://example.com/%1B]0;pwned%07%20x%C2%9B%C3%A9");
        // Only the sequence's own ESC bytes remain.
        assert_eq!(link.matches('\x1b').count(), 4, "{link:?}");
        assert!(link.contains("\x1b\\text\x1b"));
        assert!(!link.contains('\x07'));
    }
}
