//! mejl's own rule model.
//!
//! The types here are the stored form of a rule. They derive serde with
//! explicit snake_case names so the JSON written to `state.db` stays stable
//! across releases. Unknown fields are ignored on read, so a newer mejl can
//! add optional fields without breaking an older one.
//!
//! The shape borrows from Fastmail's own rule concepts (any or all of a list
//! of conditions, then actions), because that is what users already know from
//! the web UI. mejl never reads or writes Fastmail's rules. It renders these
//! rules into one Sieve block that the user pastes into a custom section.
//!
//! # Actions that are left out on purpose
//!
//! Version 1 has no `keep`, `discard`, `reject`, `redirect` or `vacation`:
//!
//! - `keep`: an explicit keep in custom code was reported to break Fastmail's
//!   alias folder delivery, which relies on the implicit keep. Leaving mail in
//!   the Inbox needs no action at all.
//! - `discard`: it bypasses Fastmail's backups, so discarded mail cannot be
//!   restored. Move to Trash covers the same need safely.
//! - `reject`: Sieve runs after the SMTP session at Fastmail, so a reject
//!   becomes a bounce message that leaks the address is live.
//! - `redirect`: forwarding sends mail to a third party and needs care with
//!   loops and `:copy`. It also needs an address picker and is not part of
//!   the v1 rules screen.
//! - `vacation`: Fastmail already generates vacation code and exposes the
//!   vacation response through JMAP. A second one in custom code would clash.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Longest rule id the renderer accepts.
pub const MAX_RULE_ID_LEN: usize = 64;

/// Stable identifier of a rule, such as `r_18aa`.
///
/// Ids are written into comment lines of the rendered block and read back by
/// the paste check, so they are limited to ASCII letters, digits, `_` and `-`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuleId(String);

impl RuleId {
    /// Wraps a string as a rule id without checking it.
    ///
    /// Use [`RuleId::is_valid`] or the validator to check it. The renderer
    /// refuses invalid ids.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True when the id is 1 to [`MAX_RULE_ID_LEN`] characters of ASCII
    /// letters, digits, `_` or `-`.
    pub fn is_valid(&self) -> bool {
        is_valid_rule_id(&self.0)
    }
}

impl fmt::Display for RuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for RuleId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for RuleId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

pub(crate) fn is_valid_rule_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_RULE_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// One mejl rule: when its conditions match, its actions run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// Stable id, also written into the rendered block.
    pub id: RuleId,
    /// Name shown in the rules list and in the block's comment line.
    pub name: String,
    /// Disabled rules still render, as `if false`, so the block keeps its
    /// shape and the paste check can see them.
    pub enabled: bool,
    /// Whether any or all conditions must match.
    pub match_mode: MatchMode,
    /// Conditions. An empty list matches every message.
    pub conditions: Vec<Condition>,
    /// Actions, in the order the user added them. The renderer runs flag
    /// actions first, because a `fileinto` only carries the flags set before
    /// it.
    pub actions: Vec<Action>,
    /// Stop the whole Sieve script after this rule. Off by default: in the
    /// section below spam protection it also skips Fastmail's own rules,
    /// notifications, calendar handling and vacation replies.
    #[serde(default)]
    pub stop: bool,
}

impl Rule {
    /// A new enabled rule with no conditions, no actions and `stop` off.
    pub fn new(id: impl Into<RuleId>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            enabled: true,
            match_mode: MatchMode::All,
            conditions: Vec::new(),
            actions: Vec::new(),
            stop: false,
        }
    }
}

/// How the conditions of a rule combine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchMode {
    /// At least one condition must match (Sieve `anyof`).
    Any,
    /// Every condition must match (Sieve `allof`).
    All,
}

/// One condition, optionally negated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Condition {
    /// Invert the test (Sieve `not`). A negated header test is true when the
    /// header is missing.
    #[serde(default)]
    pub negate: bool,
    /// What to test.
    #[serde(flatten)]
    pub test: Test,
}

impl Condition {
    /// A condition that is not negated.
    pub fn new(test: Test) -> Self {
        Self {
            negate: false,
            test,
        }
    }

    /// The same condition with `negate` set.
    pub fn negated(test: Test) -> Self {
        Self { negate: true, test }
    }
}

impl From<Test> for Condition {
    fn from(test: Test) -> Self {
        Self::new(test)
    }
}

/// What a condition looks at.
///
/// "Sender domain" and the "domain is" operator are both an [`Test::Address`]
/// test with [`AddressPart::Domain`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "field", rename_all = "snake_case")]
pub enum Test {
    /// An address header, tested address by address (Sieve `address`).
    Address {
        /// Which header or headers.
        header: AddressHeader,
        /// Which part of each address.
        #[serde(default)]
        part: AddressPart,
        /// How to compare.
        op: TextOp,
        /// The value to compare with.
        value: String,
    },
    /// The Subject header.
    Subject {
        /// How to compare.
        op: TextOp,
        /// The value to compare with.
        value: String,
    },
    /// The List-Id header, such as `Garden Club <garden-club.example.org>`.
    ListId {
        /// How to compare.
        op: TextOp,
        /// The value to compare with.
        value: String,
    },
    /// Any header by name. The name must be an RFC 5322 field name.
    Header {
        /// Header field name, such as `X-Spam-score`.
        name: String,
        /// How to compare.
        op: TextOp,
        /// The value to compare with.
        value: String,
    },
    /// The address the message was delivered to, which is how a masked
    /// address shows up. Rendered as a header test on `X-Delivered-to`, a
    /// header Fastmail adds at local delivery before Sieve runs.
    DeliveredTo {
        /// How to compare.
        op: TextOp,
        /// The value to compare with.
        value: String,
    },
    /// Message size strictly greater than `bytes`.
    SizeOver {
        /// Size limit in octets.
        bytes: u64,
    },
    /// Message size strictly less than `bytes`.
    SizeUnder {
        /// Size limit in octets.
        bytes: u64,
    },
}

/// Address headers a rule can test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddressHeader {
    /// The From header.
    From,
    /// The To header.
    To,
    /// The Cc header.
    Cc,
    /// Either To or Cc.
    ToOrCc,
}

/// Which part of an address an address test compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddressPart {
    /// The whole address, `local@domain` (Sieve `:all`).
    #[default]
    All,
    /// The part before the `@` (Sieve `:localpart`).
    Localpart,
    /// The part after the `@` (Sieve `:domain`).
    Domain,
}

/// Text comparison operators. All comparisons are ASCII case-insensitive
/// (Sieve's default comparator, `i;ascii-casemap`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextOp {
    /// The whole value equals the text (Sieve `:is`).
    Is,
    /// The value contains the text (Sieve `:contains`).
    Contains,
    /// The value matches a glob where `*` is any run of characters, `?` is
    /// one character and `\` escapes the next character (Sieve `:matches`).
    Matches,
    /// The value starts with the text. Rendered as `:matches "text*"` with
    /// glob characters in the text escaped.
    StartsWith,
    /// The value ends with the text. Rendered as `:matches "*text"` with glob
    /// characters in the text escaped.
    EndsWith,
}

/// A JMAP mailbox a rule files into, with what the renderer needs for the
/// name fallback.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MailboxRef {
    /// JMAP Mailbox id, rendered as `fileinto :mailboxid` (RFC 9042) so the
    /// rule survives a rename.
    pub id: String,
    /// Display names from the top level down, not counting the Inbox, such
    /// as `["Receipts"]` or `["Work", "Clients"]`. Used for the summary and
    /// for the fallback name `INBOX.Work.Clients` that Sieve uses when no
    /// mailbox has the id. An empty path means the Inbox itself.
    pub path: Vec<String>,
}

impl MailboxRef {
    /// A mailbox reference from an id and a path of display names.
    pub fn new<I, S>(id: impl Into<String>, path: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            id: id.into(),
            path: path.into_iter().map(Into::into).collect(),
        }
    }

    /// The path as shown to the user, segments joined by `/`. The Inbox for
    /// an empty path.
    pub fn display_path(&self) -> String {
        if self.path.is_empty() {
            "Inbox".to_owned()
        } else {
            self.path.join("/")
        }
    }

    /// The Cyrus mailbox name Fastmail's Sieve uses, such as
    /// `INBOX.Work.Clients`. Used only as the `:mailboxid` fallback.
    pub fn fallback_name(&self) -> String {
        let mut name = String::from("INBOX");
        for segment in &self.path {
            name.push('.');
            name.push_str(segment);
        }
        name
    }
}

/// What a rule does to a matching message.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// File into a mailbox and leave the Inbox (`fileinto :mailboxid`).
    MoveTo {
        /// Target mailbox.
        mailbox: MailboxRef,
    },
    /// Add a copy to a mailbox, which is how Fastmail labels work. The
    /// message also stays where it would have gone (`fileinto :copy`).
    AddLabel {
        /// Target mailbox.
        mailbox: MailboxRef,
    },
    /// Mark read (`addflag "\\Seen"`).
    MarkRead,
    /// Pin, which is the IMAP flagged state (`addflag "\\Flagged"`).
    Pin,
    /// Move to the Trash special-use mailbox (`fileinto :specialuse`).
    MoveToTrash,
}

/// The IMAP system flags mejl's actions can set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Flag {
    /// `\Seen`, read.
    Seen,
    /// `\Flagged`, pinned.
    Flagged,
}

impl Flag {
    /// The IMAP flag name, such as `\Seen`.
    pub fn imap_name(self) -> &'static str {
        match self {
            Flag::Seen => "\\Seen",
            Flag::Flagged => "\\Flagged",
        }
    }
}

impl Action {
    /// The flag this action sets, if it is a flag action.
    pub fn flag(&self) -> Option<Flag> {
        match self {
            Action::MarkRead => Some(Flag::Seen),
            Action::Pin => Some(Flag::Flagged),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_variant_rule() -> Rule {
        let mailbox = MailboxRef::new("M7d1e", ["Receipts"]);
        Rule {
            id: RuleId::new("r_all"),
            name: "Every variant".into(),
            enabled: false,
            match_mode: MatchMode::Any,
            conditions: vec![
                Condition::new(Test::Address {
                    header: AddressHeader::From,
                    part: AddressPart::All,
                    op: TextOp::Is,
                    value: "a@example.com".into(),
                }),
                Condition::new(Test::Address {
                    header: AddressHeader::To,
                    part: AddressPart::Localpart,
                    op: TextOp::StartsWith,
                    value: "billing".into(),
                }),
                Condition::new(Test::Address {
                    header: AddressHeader::Cc,
                    part: AddressPart::Domain,
                    op: TextOp::EndsWith,
                    value: ".example.org".into(),
                }),
                Condition::negated(Test::Address {
                    header: AddressHeader::ToOrCc,
                    part: AddressPart::Domain,
                    op: TextOp::Matches,
                    value: "*.example.net".into(),
                }),
                Condition::new(Test::Subject {
                    op: TextOp::Contains,
                    value: "receipt".into(),
                }),
                Condition::new(Test::ListId {
                    op: TextOp::Contains,
                    value: "<garden-club.example.org>".into(),
                }),
                Condition::new(Test::Header {
                    name: "X-Spam-score".into(),
                    op: TextOp::Is,
                    value: "0".into(),
                }),
                Condition::new(Test::DeliveredTo {
                    op: TextOp::Is,
                    value: "shop.k3x9@example.com".into(),
                }),
                Condition::new(Test::SizeOver { bytes: 1_048_576 }),
                Condition::new(Test::SizeUnder { bytes: u64::MAX }),
            ],
            actions: vec![
                Action::MoveTo {
                    mailbox: mailbox.clone(),
                },
                Action::AddLabel { mailbox },
                Action::MarkRead,
                Action::Pin,
                Action::MoveToTrash,
            ],
            stop: true,
        }
    }

    #[test]
    fn json_round_trip_covers_every_variant() {
        let rule = every_variant_rule();
        let json = serde_json::to_string(&rule).expect("serialize");
        let back: Rule = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(rule, back);
    }

    #[test]
    fn json_shape_is_stable() {
        let json = serde_json::to_string_pretty(&every_variant_rule()).expect("serialize");
        insta::assert_snapshot!("rule_json_every_variant", json);
    }

    #[test]
    fn defaults_apply_when_fields_are_missing() {
        let json = r#"{
            "id": "r_1", "name": "n", "enabled": true, "match_mode": "all",
            "conditions": [{"field": "address", "header": "from", "op": "is", "value": "x"}],
            "actions": [{"action": "mark_read"}]
        }"#;
        let rule: Rule = serde_json::from_str(json).expect("deserialize");
        assert!(!rule.stop);
        assert!(!rule.conditions[0].negate);
        assert_eq!(
            rule.conditions[0].test,
            Test::Address {
                header: AddressHeader::From,
                part: AddressPart::All,
                op: TextOp::Is,
                value: "x".into()
            }
        );
    }

    #[test]
    fn unknown_fields_are_ignored_for_forward_compatibility() {
        let json = r#"{
            "id": "r_1", "name": "n", "enabled": true, "match_mode": "any",
            "conditions": [], "actions": [], "stop": false, "section": "middle"
        }"#;
        let rule: Rule = serde_json::from_str(json).expect("deserialize");
        assert_eq!(rule.match_mode, MatchMode::Any);
    }

    #[test]
    fn unknown_variants_are_rejected() {
        let json = r#"{"action": "discard"}"#;
        assert!(serde_json::from_str::<Action>(json).is_err());
        let json = r#"{"field": "body", "op": "contains", "value": "x"}"#;
        assert!(serde_json::from_str::<Condition>(json).is_err());
    }

    #[test]
    fn rule_id_validity() {
        assert!(RuleId::new("r_18aa").is_valid());
        assert!(RuleId::new("A-z_09").is_valid());
        assert!(!RuleId::new("").is_valid());
        assert!(!RuleId::new("r 1").is_valid());
        assert!(!RuleId::new("r\n1").is_valid());
        assert!(!RuleId::new("r\"1").is_valid());
        assert!(!RuleId::new("r_é").is_valid());
        assert!(RuleId::new("a".repeat(MAX_RULE_ID_LEN)).is_valid());
        assert!(!RuleId::new("a".repeat(MAX_RULE_ID_LEN + 1)).is_valid());
    }

    #[test]
    fn mailbox_names() {
        let nested = MailboxRef::new("M1", ["Work", "Clients"]);
        assert_eq!(nested.fallback_name(), "INBOX.Work.Clients");
        assert_eq!(nested.display_path(), "Work/Clients");
        let inbox = MailboxRef::new("M0", Vec::<String>::new());
        assert_eq!(inbox.fallback_name(), "INBOX");
        assert_eq!(inbox.display_path(), "Inbox");
    }
}
