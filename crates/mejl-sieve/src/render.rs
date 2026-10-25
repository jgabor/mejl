//! Renders mejl's rules into one marked Sieve block.
//!
//! The block is meant for one custom section of Fastmail's script, by
//! default the one directly below spam protection. It looks like this:
//!
//! ```text
//! # >>> mejl managed block v1 (do not edit by hand) >>>
//! # mejl:rule id=r_18aa name="Newsletters" enabled=1
//! if address :domain :is "from" "news.example.net" {
//!   fileinto :mailboxid "M2a90" "INBOX.Newsletters";
//! }
//! # mejl:rule id=r_77e2 name="Old promo" enabled=0
//! if false # address :domain :is "from" "promo.example.com"
//! {
//!   fileinto :specialuse "\\Trash" "INBOX.Trash";
//! }
//! # <<< mejl managed block v1 sha256=<64 hex digits> <<<
//! ```
//!
//! Rules the renderer follows:
//!
//! - No `require` line. Fastmail generates it at the top of the script and
//!   custom sections sit below it. Only extensions from that line are used.
//! - Never `keep` or `discard`. `stop` only when a rule asks for it.
//! - The comparator is left at Sieve's default, `i;ascii-casemap`, which is
//!   ASCII case-insensitive.
//! - `fileinto :mailboxid "<id>" "<name>"` (RFC 9042), so a renamed folder
//!   still works. The name is only a fallback when no mailbox has the id.
//! - A disabled rule renders as `if false # <test>` with its block on the
//!   next line, so the block keeps its shape and its line count.
//! - The end marker carries the sha256 of the body, the lines between the
//!   markers, so the paste check can see hand edits.
//!
//! Output is deterministic: the same rules in the same order always give the
//! same bytes.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::ops::RangeInclusive;

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::lower::{SieveTest, execution_order, lower_rule_test};
use crate::model::{Action, AddressPart, MailboxRef, Rule, RuleId, Test};

/// Format version written into the markers.
pub const FORMAT_VERSION: u32 = 1;

/// Largest size limit the renderer writes. Sieve implementations only have
/// to support 31-bit numbers, and no mail is that large anyway.
pub const MAX_SIZE_BYTES: u64 = (1 << 31) - 1;

/// The begin marker line for the current format version.
pub fn begin_marker() -> String {
    format!("# >>> mejl managed block v{FORMAT_VERSION} (do not edit by hand) >>>")
}

/// The end marker line for the current format version and a body hash.
pub fn end_marker(body_sha256: &str) -> String {
    format!("# <<< mejl managed block v{FORMAT_VERSION} sha256={body_sha256} <<<")
}

/// A rendered block, ready for the clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedBlock {
    /// The whole block, markers included, with LF line endings and a final
    /// newline.
    pub text: String,
    /// Lowercase hex sha256 of the body, as written in the end marker.
    pub body_sha256: String,
    /// Where each rule sits in `text`, in render order.
    pub rules: Vec<RenderedRule>,
}

impl RenderedBlock {
    /// Number of lines in the block, markers included.
    pub fn line_count(&self) -> usize {
        self.text.lines().count()
    }
}

/// The lines of one rule inside a rendered block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedRule {
    /// The rule's id.
    pub id: RuleId,
    /// One-based line numbers in [`RenderedBlock::text`], from the
    /// `# mejl:rule` comment to the closing brace.
    pub lines: RangeInclusive<usize>,
}

/// Why a rule cannot be rendered.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("rule {rule_id}: {kind}")]
pub struct RenderError {
    /// The rule that failed.
    pub rule_id: RuleId,
    /// What is wrong with it.
    pub kind: RenderErrorKind,
}

/// The kinds of rule problems that stop rendering. Each would either break
/// the Sieve syntax or let user text escape into live Sieve code.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RenderErrorKind {
    /// The id is not 1 to 64 ASCII letters, digits, `_` or `-`.
    #[error("invalid rule id")]
    InvalidRuleId,
    /// Another rule earlier in the list has the same id.
    #[error("duplicate rule id")]
    DuplicateRuleId,
    /// A text field holds a control character such as a line break, which
    /// would end a comment line early.
    #[error("{field} contains a control character")]
    ControlCharacter {
        /// Which field, such as `name` or `condition value`.
        field: &'static str,
    },
    /// A header name is not an RFC 5322 field name.
    #[error("invalid header name {name:?}")]
    InvalidHeaderName {
        /// The rejected name.
        name: String,
    },
    /// A mailbox id is not a JMAP id (1 to 255 of `A-Z a-z 0-9 - _`).
    #[error("invalid mailbox id {id:?}")]
    InvalidMailboxId {
        /// The rejected id.
        id: String,
    },
    /// A mailbox path has an empty segment.
    #[error("mailbox path has an empty segment")]
    EmptyMailboxSegment,
    /// A size limit is larger than [`MAX_SIZE_BYTES`].
    #[error("size {bytes} is larger than {MAX_SIZE_BYTES}")]
    SizeOutOfRange {
        /// The rejected size.
        bytes: u64,
    },
}

/// Renders the rules, in the given order, into one marked block.
///
/// Fails on the first rule that could produce invalid or unsafe Sieve. The
/// validator reports the same problems, and more, as diagnostics.
pub fn render_block(rules: &[Rule]) -> Result<RenderedBlock, RenderError> {
    let mut seen = HashSet::new();
    for rule in rules {
        if let Some(kind) = rule_problems(rule).into_iter().next() {
            return Err(RenderError {
                rule_id: rule.id.clone(),
                kind,
            });
        }
        if !seen.insert(rule.id.as_str()) {
            return Err(RenderError {
                rule_id: rule.id.clone(),
                kind: RenderErrorKind::DuplicateRuleId,
            });
        }
    }

    let mut body: Vec<String> = Vec::new();
    let mut spans = Vec::with_capacity(rules.len());
    for rule in rules {
        // Line 1 is the begin marker, so body index i is line i + 2.
        let first = body.len() + 2;
        render_rule(rule, &mut body);
        spans.push(RenderedRule {
            id: rule.id.clone(),
            lines: first..=body.len() + 1,
        });
    }

    let body_sha256 = body_hash(body.iter().map(String::as_str));
    let mut text = String::new();
    text.push_str(&begin_marker());
    text.push('\n');
    for line in &body {
        text.push_str(line);
        text.push('\n');
    }
    text.push_str(&end_marker(&body_sha256));
    text.push('\n');

    Ok(RenderedBlock {
        text,
        body_sha256,
        rules: spans,
    })
}

/// The sha256 of a block body, as lowercase hex.
///
/// Each line is hashed without trailing whitespace and followed by one LF.
/// The paste check uses the same function, so CRLF line endings and trailing
/// spaces added by a browser or editor do not count as edits.
pub(crate) fn body_hash<'a>(lines: impl IntoIterator<Item = &'a str>) -> String {
    let mut hasher = Sha256::new();
    for line in lines {
        hasher.update(line.trim_end().as_bytes());
        hasher.update(b"\n");
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        // Writing to a String cannot fail.
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Every problem in one rule that stops rendering, in a fixed order.
pub(crate) fn rule_problems(rule: &Rule) -> Vec<RenderErrorKind> {
    let mut problems = Vec::new();
    if !rule.id.is_valid() {
        problems.push(RenderErrorKind::InvalidRuleId);
    }
    if has_control(&rule.name) {
        problems.push(RenderErrorKind::ControlCharacter { field: "name" });
    }
    for condition in &rule.conditions {
        match &condition.test {
            Test::Address { value, .. }
            | Test::Subject { value, .. }
            | Test::ListId { value, .. }
            | Test::DeliveredTo { value, .. } => {
                if has_control(value) {
                    problems.push(RenderErrorKind::ControlCharacter {
                        field: "condition value",
                    });
                }
            }
            Test::Header { name, value, .. } => {
                if !is_valid_header_name(name) {
                    problems.push(RenderErrorKind::InvalidHeaderName { name: name.clone() });
                }
                if has_control(value) {
                    problems.push(RenderErrorKind::ControlCharacter {
                        field: "condition value",
                    });
                }
            }
            Test::SizeOver { bytes } | Test::SizeUnder { bytes } => {
                if *bytes > MAX_SIZE_BYTES {
                    problems.push(RenderErrorKind::SizeOutOfRange { bytes: *bytes });
                }
            }
        }
    }
    for action in &rule.actions {
        if let Action::MoveTo { mailbox } | Action::AddLabel { mailbox } = action {
            mailbox_problems(mailbox, &mut problems);
        }
    }
    problems
}

fn mailbox_problems(mailbox: &MailboxRef, problems: &mut Vec<RenderErrorKind>) {
    if !is_valid_jmap_id(&mailbox.id) {
        problems.push(RenderErrorKind::InvalidMailboxId {
            id: mailbox.id.clone(),
        });
    }
    for segment in &mailbox.path {
        if segment.is_empty() {
            problems.push(RenderErrorKind::EmptyMailboxSegment);
        } else if has_control(segment) {
            problems.push(RenderErrorKind::ControlCharacter {
                field: "mailbox name",
            });
        }
    }
}

fn has_control(text: &str) -> bool {
    text.chars().any(is_line_breaking)
}

/// Control characters, plus the Unicode line and paragraph separators,
/// which some editors show as line breaks. Any of them inside a comment
/// line could make the rest of the line look like, or become, live code.
pub(crate) fn is_line_breaking(c: char) -> bool {
    c.is_control() || c == '\u{2028}' || c == '\u{2029}'
}

/// RFC 5322 field-name: one or more printable ASCII characters except `:`.
pub fn is_valid_header_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| matches!(b, 33..=57 | 59..=126))
}

/// RFC 8620 Id: 1 to 255 characters from the URL-safe base64 alphabet.
pub(crate) fn is_valid_jmap_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 255
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Quotes a Sieve string, escaping `\` and `"` (RFC 5228 section 2.4.2).
pub(crate) fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

fn string_list(items: &[&str]) -> String {
    if let [single] = items {
        return quote(single);
    }
    let quoted: Vec<String> = items.iter().map(|s| quote(s)).collect();
    format!("[{}]", quoted.join(", "))
}

/// A size with the largest exact K or M suffix (powers of 1024).
fn size_literal(bytes: u64) -> String {
    const K: u64 = 1024;
    const M: u64 = 1024 * 1024;
    if bytes != 0 && bytes.is_multiple_of(M) {
        format!("{}M", bytes / M)
    } else if bytes != 0 && bytes.is_multiple_of(K) {
        format!("{}K", bytes / K)
    } else {
        bytes.to_string()
    }
}

/// One test on one line. Used for leaf tests and for the comment of a
/// disabled rule.
pub(crate) fn test_inline(test: &SieveTest) -> String {
    match test {
        SieveTest::True => "true".to_owned(),
        SieveTest::Address {
            headers,
            part,
            match_type,
            key,
        } => {
            let part = match part {
                AddressPart::All => ":all",
                AddressPart::Localpart => ":localpart",
                AddressPart::Domain => ":domain",
            };
            format!(
                "address {part} {} {} {}",
                match_type.tag(),
                string_list(headers),
                quote(key)
            )
        }
        SieveTest::Header {
            name,
            match_type,
            key,
        } => format!("header {} {} {}", match_type.tag(), quote(name), quote(key)),
        SieveTest::Size { over, limit } => {
            let tag = if *over { ":over" } else { ":under" };
            format!("size {tag} {}", size_literal(*limit))
        }
        SieveTest::Not(inner) => format!("not {}", test_inline(inner)),
        SieveTest::AnyOf(tests) => format!("anyof ({})", inline_list(tests)),
        SieveTest::AllOf(tests) => format!("allof ({})", inline_list(tests)),
    }
}

fn inline_list(tests: &[SieveTest]) -> String {
    tests.iter().map(test_inline).collect::<Vec<_>>().join(", ")
}

fn rule_comment(rule: &Rule) -> String {
    format!(
        "# mejl:rule id={} name={} enabled={}",
        rule.id,
        quote(&rule.name),
        u8::from(rule.enabled)
    )
}

fn render_rule(rule: &Rule, out: &mut Vec<String>) {
    out.push(rule_comment(rule));
    let test = lower_rule_test(rule);
    if rule.enabled {
        match &test {
            SieveTest::AnyOf(tests) | SieveTest::AllOf(tests) => {
                let head = if matches!(test, SieveTest::AnyOf(_)) {
                    "if anyof ("
                } else {
                    "if allof ("
                };
                let indent = " ".repeat(head.len());
                let last = tests.len() - 1;
                for (i, t) in tests.iter().enumerate() {
                    let lead = if i == 0 { head } else { indent.as_str() };
                    let tail = if i == last { ") {" } else { "," };
                    out.push(format!("{lead}{}{tail}", test_inline(t)));
                }
            }
            _ => out.push(format!("if {} {{", test_inline(&test))),
        }
    } else {
        out.push(format!("if false # {}", test_inline(&test)));
        out.push("{".to_owned());
    }
    for action in execution_order(&rule.actions) {
        out.push(format!("  {}", action_command(action)));
    }
    if rule.stop {
        out.push("  stop;".to_owned());
    }
    out.push("}".to_owned());
}

fn action_command(action: &Action) -> String {
    match action {
        Action::MoveTo { mailbox } => format!(
            "fileinto :mailboxid {} {};",
            quote(&mailbox.id),
            quote(&mailbox.fallback_name())
        ),
        Action::AddLabel { mailbox } => format!(
            "fileinto :copy :mailboxid {} {};",
            quote(&mailbox.id),
            quote(&mailbox.fallback_name())
        ),
        Action::MarkRead => format!("addflag {};", quote("\\Seen")),
        Action::Pin => format!("addflag {};", quote("\\Flagged")),
        Action::MoveToTrash => format!(
            "fileinto :specialuse {} {};",
            quote("\\Trash"),
            quote("INBOX.Trash")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AddressHeader, Condition, MatchMode, TextOp};

    #[test]
    fn quoting_escapes_backslash_and_quote_only() {
        assert_eq!(quote(""), "\"\"");
        assert_eq!(quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote("a\\b"), "\"a\\\\b\"");
        assert_eq!(quote("\\\""), "\"\\\\\\\"\"");
        assert_eq!(quote("ünïcødé ✓"), "\"ünïcødé ✓\"");
    }

    #[test]
    fn size_literals_use_exact_suffixes() {
        assert_eq!(size_literal(0), "0");
        assert_eq!(size_literal(1), "1");
        assert_eq!(size_literal(1023), "1023");
        assert_eq!(size_literal(1024), "1K");
        assert_eq!(size_literal(1536), "1536");
        assert_eq!(size_literal(102_400), "100K");
        assert_eq!(size_literal(1_048_576), "1M");
        assert_eq!(size_literal(1_049_600), "1025K");
        assert_eq!(size_literal(MAX_SIZE_BYTES), "2147483647");
        assert_eq!(size_literal(2047 * 1_048_576), "2047M");
    }

    #[test]
    fn header_names_follow_rfc_5322() {
        assert!(is_valid_header_name("X-Spam-score"));
        assert!(is_valid_header_name("List-Id"));
        assert!(is_valid_header_name("~!#$%&'*+-.^_`|"));
        assert!(!is_valid_header_name(""));
        assert!(!is_valid_header_name("Subject:"));
        assert!(!is_valid_header_name("X Header"));
        // RFC 5322 allows quotes in a field name. quote() escapes them.
        assert!(is_valid_header_name("X-\"q\""));
        assert!(is_valid_header_name("X-\\"));
        assert!(!is_valid_header_name("X-é"));
        assert!(!is_valid_header_name("X-\n"));
        assert!(!is_valid_header_name("X-\u{7f}"));
    }

    #[test]
    fn empty_rule_list_renders_markers_only() {
        let block = render_block(&[]).expect("render");
        let empty_sha = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(block.body_sha256, empty_sha);
        assert_eq!(
            block.text,
            format!("{}\n{}\n", begin_marker(), end_marker(empty_sha))
        );
        assert!(block.rules.is_empty());
        assert_eq!(block.line_count(), 2);
    }

    fn from_is(value: &str) -> Condition {
        Condition::new(Test::Address {
            header: AddressHeader::From,
            part: AddressPart::All,
            op: TextOp::Is,
            value: value.into(),
        })
    }

    #[test]
    fn line_spans_point_at_each_rule() {
        let mut a = Rule::new("r_a", "A");
        a.conditions.push(from_is("a@example.com"));
        a.actions.push(Action::MarkRead);
        let mut b = Rule::new("r_b", "B");
        b.match_mode = MatchMode::Any;
        b.conditions.push(from_is("b@example.com"));
        b.conditions.push(from_is("c@example.com"));
        let block = render_block(&[a, b]).expect("render");
        let lines: Vec<&str> = block.text.lines().collect();
        assert_eq!(block.rules[0].lines, 2..=5);
        assert_eq!(block.rules[1].lines, 6..=9);
        for span in &block.rules {
            assert!(lines[span.lines.start() - 1].starts_with("# mejl:rule id="));
            assert_eq!(lines[span.lines.end() - 1], "}");
        }
        assert_eq!(block.line_count(), 10);
    }

    #[test]
    fn duplicate_ids_are_refused() {
        let a = Rule::new("r_a", "A");
        let err = render_block(&[a.clone(), a]).expect_err("duplicate");
        assert_eq!(err.kind, RenderErrorKind::DuplicateRuleId);
    }

    #[test]
    fn control_characters_are_refused_everywhere() {
        let mut rule = Rule::new("r_a", "line\nbreak");
        assert_eq!(
            render_block(std::slice::from_ref(&rule))
                .expect_err("name")
                .kind,
            RenderErrorKind::ControlCharacter { field: "name" }
        );
        rule.name = "ok".into();
        rule.enabled = false;
        rule.conditions.push(from_is("x\r\nkeep;"));
        assert_eq!(
            render_block(std::slice::from_ref(&rule))
                .expect_err("value")
                .kind,
            RenderErrorKind::ControlCharacter {
                field: "condition value"
            }
        );
        rule.conditions.clear();
        rule.actions.push(Action::MoveTo {
            mailbox: MailboxRef::new("M1", ["a\u{0}b"]),
        });
        assert_eq!(
            render_block(std::slice::from_ref(&rule))
                .expect_err("mailbox")
                .kind,
            RenderErrorKind::ControlCharacter {
                field: "mailbox name"
            }
        );
    }

    #[test]
    fn unicode_line_separators_are_refused() {
        for sep in ['\u{2028}', '\u{2029}', '\u{85}'] {
            let rule = Rule::new("r_a", format!("a{sep}keep;"));
            assert!(render_block(&[rule]).is_err(), "{sep:?}");
        }
        // Other format characters, such as the joiner in emoji, are fine.
        let rule = Rule::new("r_a", "family \u{1F468}\u{200D}\u{1F469}");
        assert!(render_block(&[rule]).is_ok());
    }

    #[test]
    fn quotes_in_header_names_are_escaped() {
        let mut rule = Rule::new("r_q", "q");
        rule.conditions.push(Condition::new(Test::Header {
            name: "X-\"q\"\\".into(),
            op: TextOp::Is,
            value: "v".into(),
        }));
        let block = render_block(&[rule]).expect("render");
        assert!(block.text.contains(r#"header :is "X-\"q\"\\" "v""#));
        assert!(crate::script::check_script(&block.text).is_ok());
    }

    #[test]
    fn bad_ids_and_names_are_refused() {
        let rule = Rule::new("r 1", "x");
        assert_eq!(
            render_block(&[rule]).expect_err("id").kind,
            RenderErrorKind::InvalidRuleId
        );
        let mut rule = Rule::new("r_1", "x");
        rule.conditions.push(Condition::new(Test::Header {
            name: "Subject: x".into(),
            op: TextOp::Is,
            value: String::new(),
        }));
        assert!(matches!(
            render_block(&[rule]).expect_err("header").kind,
            RenderErrorKind::InvalidHeaderName { .. }
        ));
        let mut rule = Rule::new("r_1", "x");
        rule.actions.push(Action::AddLabel {
            mailbox: MailboxRef::new("M\"1", ["x"]),
        });
        assert!(matches!(
            render_block(&[rule]).expect_err("mailbox id").kind,
            RenderErrorKind::InvalidMailboxId { .. }
        ));
        let mut rule = Rule::new("r_1", "x");
        rule.actions.push(Action::AddLabel {
            mailbox: MailboxRef::new("M1", ["a", ""]),
        });
        assert_eq!(
            render_block(&[rule]).expect_err("segment").kind,
            RenderErrorKind::EmptyMailboxSegment
        );
        let mut rule = Rule::new("r_1", "x");
        rule.conditions.push(Condition::new(Test::SizeOver {
            bytes: MAX_SIZE_BYTES + 1,
        }));
        assert!(matches!(
            render_block(&[rule]).expect_err("size").kind,
            RenderErrorKind::SizeOutOfRange { .. }
        ));
    }

    #[test]
    fn hash_ignores_trailing_whitespace_only() {
        assert_eq!(body_hash(["a", "b"]), body_hash(["a  ", "b\t"]));
        assert_ne!(body_hash(["a", "b"]), body_hash([" a", "b"]));
        assert_ne!(body_hash(["a", "b"]), body_hash(["ab"]));
        assert_ne!(body_hash(["a"]), body_hash(["a", ""]));
    }
}
