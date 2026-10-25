//! Local dry run of mejl's rules against cached mail.
//!
//! This evaluates the same Sieve tests the renderer writes, with Sieve
//! semantics:
//!
//! - Comparisons use `i;ascii-casemap`: ASCII letters compare without case,
//!   every other octet compares exactly. `É` and `é` differ.
//! - `:matches` globs work on octets, as RFC 4790 defines the comparator, so
//!   `?` matches one octet. A non-ASCII character is more than one octet.
//! - A header test is true when any occurrence of the header matches. A
//!   missing header never matches, so `not` on it is true.
//! - An address test looks at each address on its own. An address without
//!   `@` has itself as local part and an empty domain.
//! - `addflag` changes the flags for every later `fileinto` and the implicit
//!   keep, across rules. `fileinto` without `:copy` cancels the implicit
//!   keep.
//!
//! It simulates mejl's block only. Fastmail's generated code (spam, masked
//! email, its own rules) runs around the block and is not simulated.

use std::collections::BTreeSet;

use crate::lower::{DELIVERED_TO, MatchType, SUBJECT, SieveTest, execution_order, lower_rule_test};
use crate::model::{Action, AddressPart, Flag, MailboxRef, Rule, RuleId};

/// An address as JMAP gives it: an optional display name and an email.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EmailAddress {
    /// Display name, such as `Juniper Tran`.
    pub name: Option<String>,
    /// The address, such as `juniper@example.com`.
    pub email: String,
}

impl EmailAddress {
    /// An address with a display name.
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            email: email.into(),
        }
    }

    /// An address without a display name.
    pub fn bare(email: impl Into<String>) -> Self {
        Self {
            name: None,
            email: email.into(),
        }
    }
}

/// What the matcher knows about a message, all from cached data.
///
/// Header values are expected decoded (RFC 2047) and unfolded, as Sieve sees
/// them. The dedicated fields win over `headers`: for From, To, Cc, Subject
/// and X-Delivered-to, entries with the same name in `headers` are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MessageFacts {
    /// From addresses.
    pub from: Vec<EmailAddress>,
    /// To addresses.
    pub to: Vec<EmailAddress>,
    /// Cc addresses.
    pub cc: Vec<EmailAddress>,
    /// Subject, `None` when the message has no Subject header.
    pub subject: Option<String>,
    /// Other headers as name and value pairs, in message order. Names
    /// compare without ASCII case.
    pub headers: Vec<(String, String)>,
    /// Size in octets.
    pub size: u64,
    /// Values of the `X-Delivered-to` header Fastmail adds at delivery,
    /// which hold the address the message was sent to, masked or not.
    pub delivered_to: Vec<String>,
}

impl MessageFacts {
    /// Every value of a header, by name, ASCII case-insensitive.
    pub fn header_values(&self, name: &str) -> Vec<String> {
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "from" => joined_addresses(&self.from),
            "to" => joined_addresses(&self.to),
            "cc" => joined_addresses(&self.cc),
            SUBJECT => self.subject.iter().cloned().collect(),
            DELIVERED_TO => self.delivered_to.clone(),
            _ => self
                .headers
                .iter()
                .filter(|(n, _)| n.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
                .collect(),
        }
    }

    fn addresses(&self, header: &str) -> &[EmailAddress] {
        match header {
            "from" => &self.from,
            "to" => &self.to,
            "cc" => &self.cc,
            _ => &[],
        }
    }
}

/// Rebuilds a header value from a parsed address list, so a header test on
/// From, To or Cc sees something close to the original text.
fn joined_addresses(list: &[EmailAddress]) -> Vec<String> {
    if list.is_empty() {
        return Vec::new();
    }
    let parts: Vec<String> = list
        .iter()
        .map(|a| match &a.name {
            Some(name) if !name.is_empty() => format!("{name} <{}>", a.email),
            _ => a.email.clone(),
        })
        .collect();
    vec![parts.join(", ")]
}

/// One action a matching rule would take, in the order the script runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannedAction {
    /// Mark read.
    MarkRead,
    /// Pin.
    Pin,
    /// Add a copy to a mailbox.
    AddLabel(MailboxRef),
    /// File into a mailbox and leave the Inbox.
    MoveTo(MailboxRef),
    /// File into Trash.
    MoveToTrash,
    /// Stop the script.
    Stop,
}

impl From<&Action> for PlannedAction {
    fn from(action: &Action) -> Self {
        match action {
            Action::MarkRead => PlannedAction::MarkRead,
            Action::Pin => PlannedAction::Pin,
            Action::AddLabel { mailbox } => PlannedAction::AddLabel(mailbox.clone()),
            Action::MoveTo { mailbox } => PlannedAction::MoveTo(mailbox.clone()),
            Action::MoveToTrash => PlannedAction::MoveToTrash,
        }
    }
}

/// What one rule would do to a message.
///
/// Returns `None` when the rule is disabled or does not match. Otherwise
/// returns its actions in execution order, with [`PlannedAction::Stop`] last
/// when the rule stops.
pub fn evaluate(rule: &Rule, facts: &MessageFacts) -> Option<Vec<PlannedAction>> {
    if !rule.enabled || !eval_test(&lower_rule_test(rule), facts) {
        return None;
    }
    let mut actions: Vec<PlannedAction> = execution_order(&rule.actions)
        .into_iter()
        .map(PlannedAction::from)
        .collect();
    if rule.stop {
        actions.push(PlannedAction::Stop);
    }
    Some(actions)
}

/// One rule that matched during [`evaluate_all`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleMatch {
    /// The rule.
    pub rule_id: RuleId,
    /// Its actions in execution order.
    pub actions: Vec<PlannedAction>,
}

/// Where a copy of the message would be filed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryTarget {
    /// A mailbox by id.
    Mailbox(MailboxRef),
    /// The Trash special-use mailbox.
    Trash,
}

/// One copy of the message filed by `fileinto`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// Where it goes.
    pub target: DeliveryTarget,
    /// Flags it is stored with.
    pub flags: BTreeSet<Flag>,
}

/// The combined result of running every rule in order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Evaluation {
    /// Rules that matched, in order.
    pub matches: Vec<RuleMatch>,
    /// Copies filed by mejl's block, one per target. Filing into the same
    /// target twice stores one copy, with the flags of both (inferred from
    /// RFC 5228, which says not to file the same message twice).
    pub deliveries: Vec<Delivery>,
    /// True when no rule filed the message without `:copy`, so it still
    /// goes where it would have gone (the Inbox, unless Fastmail's own rules
    /// move it).
    pub implicit_keep: bool,
    /// Flags set by the end of mejl's block. The implicit keep uses them.
    pub flags: BTreeSet<Flag>,
    /// The rule that stopped the script, if any.
    pub stopped_by: Option<RuleId>,
}

/// Runs every rule in order, as the rendered block would, honoring `stop`.
pub fn evaluate_all(rules: &[Rule], facts: &MessageFacts) -> Evaluation {
    let mut out = Evaluation {
        implicit_keep: true,
        ..Evaluation::default()
    };
    for rule in rules {
        let Some(actions) = evaluate(rule, facts) else {
            continue;
        };
        let mut stop = false;
        for action in &actions {
            match action {
                PlannedAction::MarkRead => {
                    out.flags.insert(Flag::Seen);
                }
                PlannedAction::Pin => {
                    out.flags.insert(Flag::Flagged);
                }
                PlannedAction::AddLabel(mailbox) => {
                    deliver(&mut out, DeliveryTarget::Mailbox(mailbox.clone()));
                }
                PlannedAction::MoveTo(mailbox) => {
                    deliver(&mut out, DeliveryTarget::Mailbox(mailbox.clone()));
                    out.implicit_keep = false;
                }
                PlannedAction::MoveToTrash => {
                    deliver(&mut out, DeliveryTarget::Trash);
                    out.implicit_keep = false;
                }
                PlannedAction::Stop => stop = true,
            }
        }
        out.matches.push(RuleMatch {
            rule_id: rule.id.clone(),
            actions,
        });
        if stop {
            out.stopped_by = Some(rule.id.clone());
            break;
        }
    }
    out
}

fn same_target(a: &DeliveryTarget, b: &DeliveryTarget) -> bool {
    match (a, b) {
        (DeliveryTarget::Mailbox(x), DeliveryTarget::Mailbox(y)) => x.id == y.id,
        (DeliveryTarget::Trash, DeliveryTarget::Trash) => true,
        _ => false,
    }
}

fn deliver(out: &mut Evaluation, target: DeliveryTarget) {
    let flags = out.flags.clone();
    if let Some(existing) = out
        .deliveries
        .iter_mut()
        .find(|d| same_target(&d.target, &target))
    {
        existing.flags.extend(flags);
    } else {
        out.deliveries.push(Delivery { target, flags });
    }
}

pub(crate) fn eval_test(test: &SieveTest, facts: &MessageFacts) -> bool {
    match test {
        SieveTest::True => true,
        SieveTest::Not(inner) => !eval_test(inner, facts),
        SieveTest::AnyOf(tests) => tests.iter().any(|t| eval_test(t, facts)),
        SieveTest::AllOf(tests) => tests.iter().all(|t| eval_test(t, facts)),
        SieveTest::Size { over, limit } => {
            if *over {
                facts.size > *limit
            } else {
                facts.size < *limit
            }
        }
        SieveTest::Header {
            name,
            match_type,
            key,
        } => facts
            .header_values(name)
            .iter()
            .any(|value| compare(*match_type, value, key)),
        SieveTest::Address {
            headers,
            part,
            match_type,
            key,
        } => headers.iter().any(|header| {
            facts
                .addresses(header)
                .iter()
                .any(|address| compare(*match_type, address_part(&address.email, *part), key))
        }),
    }
}

fn address_part(email: &str, part: AddressPart) -> &str {
    match part {
        AddressPart::All => email,
        AddressPart::Localpart => email.rsplit_once('@').map_or(email, |(local, _)| local),
        AddressPart::Domain => email.rsplit_once('@').map_or("", |(_, domain)| domain),
    }
}

/// Compares under `i;ascii-casemap`.
pub(crate) fn compare(match_type: MatchType, value: &str, key: &str) -> bool {
    match match_type {
        MatchType::Is => value.eq_ignore_ascii_case(key),
        MatchType::Contains => value
            .to_ascii_lowercase()
            .contains(&key.to_ascii_lowercase()),
        MatchType::Matches => glob_match(key.as_bytes(), value.as_bytes()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GlobToken {
    Literal(u8),
    AnyOne,
    AnyRun,
}

fn glob_tokens(pattern: &[u8]) -> Vec<GlobToken> {
    let mut tokens = Vec::with_capacity(pattern.len());
    let mut i = 0;
    while let Some(&b) = pattern.get(i) {
        match b {
            b'*' => {
                // Runs of stars mean the same as one star.
                if tokens.last() != Some(&GlobToken::AnyRun) {
                    tokens.push(GlobToken::AnyRun);
                }
            }
            b'?' => tokens.push(GlobToken::AnyOne),
            b'\\' => match pattern.get(i + 1) {
                Some(&next) => {
                    tokens.push(GlobToken::Literal(next));
                    i += 1;
                }
                // A lone trailing backslash stands for itself.
                None => tokens.push(GlobToken::Literal(b'\\')),
            },
            other => tokens.push(GlobToken::Literal(other)),
        }
        i += 1;
    }
    tokens
}

/// Sieve `:matches` with `i;ascii-casemap`, iterative so hostile patterns
/// run in O(pattern x text) time with no recursion.
pub(crate) fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    let pattern = glob_tokens(pattern);
    let (mut p, mut t) = (0, 0);
    // Position after the last star, and the text position it is tried at.
    let mut backtrack: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some(GlobToken::AnyRun) => {
                p += 1;
                backtrack = Some((p, t));
                continue;
            }
            Some(GlobToken::AnyOne) => {
                p += 1;
                t += 1;
                continue;
            }
            Some(GlobToken::Literal(b)) if b.eq_ignore_ascii_case(&text[t]) => {
                p += 1;
                t += 1;
                continue;
            }
            _ => {}
        }
        match backtrack {
            Some((star_p, star_t)) => {
                p = star_p;
                t = star_t + 1;
                backtrack = Some((star_p, t));
            }
            None => return false,
        }
    }
    pattern[p..].iter().all(|tok| *tok == GlobToken::AnyRun)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AddressHeader, Condition, MatchMode, Test, TextOp};
    use std::time::{Duration, Instant};

    #[test]
    fn glob_table() {
        let cases: &[(&str, &str, bool)] = &[
            ("", "", true),
            ("", "a", false),
            ("*", "", true),
            ("*", "anything", true),
            ("?", "", false),
            ("?", "a", true),
            ("?", "ab", false),
            ("a*", "ABC", true),
            ("*c", "abC", true),
            ("a*c", "ac", true),
            ("a*c", "abbbc", true),
            ("a*c", "abbbd", false),
            ("a?c", "abc", true),
            ("a?c", "ac", false),
            ("*@example.com", "x@EXAMPLE.com", true),
            ("*@example.com", "x@example.com.example.net", false),
            ("a\\*b", "a*b", true),
            ("a\\*b", "axb", false),
            ("a\\?b", "a?b", true),
            ("a\\?b", "axb", false),
            ("a\\\\b", "a\\b", true),
            ("a\\", "a\\", true),
            ("\\a", "A", true),
            ("**a**", "bab", true),
            ("*a*b*c*", "xxaxxbxxc", true),
            ("*a*b*c*", "xxaxxcxxb", false),
            // ? is one octet, so a two-octet character needs two.
            ("caf?", "café", false),
            ("caf??", "café", true),
            ("CAFÉ", "cafÉ", true),
            ("CAFÉ", "café", false),
        ];
        for (pattern, text, expected) in cases {
            assert_eq!(
                glob_match(pattern.as_bytes(), text.as_bytes()),
                *expected,
                "{pattern:?} vs {text:?}"
            );
        }
    }

    #[test]
    fn glob_is_fast_on_hostile_patterns() {
        let pattern = format!("{}b", "*a".repeat(50));
        let text = "a".repeat(10_000);
        let start = Instant::now();
        assert!(!glob_match(pattern.as_bytes(), text.as_bytes()));
        let star_only = "*".repeat(5_000);
        assert!(glob_match(star_only.as_bytes(), text.as_bytes()));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn compare_table() {
        let cases = [
            (MatchType::Is, "Receipt", "receipt", true),
            (MatchType::Is, "Receipt ", "receipt", false),
            (MatchType::Is, "", "", true),
            (MatchType::Contains, "Your RECEIPT for May", "receipt", true),
            (MatchType::Contains, "anything", "", true),
            (MatchType::Contains, "", "", true),
            (MatchType::Contains, "Rcpt", "receipt", false),
            (MatchType::Contains, "ÉTÉ", "été", false),
            (MatchType::Contains, "été", "été", true),
        ];
        for (mt, value, key, expected) in cases {
            assert_eq!(
                compare(mt, value, key),
                expected,
                "{mt:?} {value:?} {key:?}"
            );
        }
    }

    #[test]
    fn address_parts() {
        assert_eq!(
            address_part("a@b.example.org", AddressPart::All),
            "a@b.example.org"
        );
        assert_eq!(address_part("a@b.example.org", AddressPart::Localpart), "a");
        assert_eq!(
            address_part("a@b.example.org", AddressPart::Domain),
            "b.example.org"
        );
        assert_eq!(
            address_part("\"x@y\"@b.example.org", AddressPart::Domain),
            "b.example.org"
        );
        assert_eq!(
            address_part("undisclosed", AddressPart::Localpart),
            "undisclosed"
        );
        assert_eq!(address_part("undisclosed", AddressPart::Domain), "");
    }

    fn facts() -> MessageFacts {
        MessageFacts {
            from: vec![EmailAddress::new(
                "Larkspur Outfitters",
                "orders@shop.example.com",
            )],
            to: vec![
                EmailAddress::bare("mira@example.org"),
                EmailAddress::new("Team", "team@lists.example.net"),
            ],
            cc: vec![EmailAddress::bare("billing@example.org")],
            subject: Some("Receipt for order 47720".into()),
            headers: vec![
                (
                    "List-Id".into(),
                    "Garden Club <garden-club.example.org>".into(),
                ),
                ("Received".into(), "from a.example.net".into()),
                ("received".into(), "from b.example.net".into()),
                ("Subject".into(), "ignored, the field wins".into()),
            ],
            size: 2048,
            delivered_to: vec!["shop.k3x9@example.org".into()],
        }
    }

    fn cond(test: Test) -> Rule {
        let mut rule = Rule::new("r_t", "t");
        rule.conditions.push(Condition::new(test));
        rule.actions.push(Action::MarkRead);
        rule
    }

    fn matches(test: Test) -> bool {
        evaluate(&cond(test), &facts()).is_some()
    }

    fn not_matches(test: Test) -> bool {
        let mut rule = cond(test);
        rule.conditions[0].negate = true;
        evaluate(&rule, &facts()).is_some()
    }

    fn addr(header: AddressHeader, part: AddressPart, op: TextOp, value: &str) -> Test {
        Test::Address {
            header,
            part,
            op,
            value: value.into(),
        }
    }

    fn hdr(name: &str, op: TextOp, value: &str) -> Test {
        Test::Header {
            name: name.into(),
            op,
            value: value.into(),
        }
    }

    #[test]
    fn condition_table() {
        use AddressHeader::*;
        use AddressPart::*;
        use TextOp::*;
        let cases: Vec<(Test, bool)> = vec![
            (addr(From, All, Is, "ORDERS@shop.example.com"), true),
            (addr(From, All, Is, "orders"), false),
            (addr(From, Domain, Is, "shop.example.com"), true),
            (addr(From, Domain, Is, "example.com"), false),
            (addr(From, Domain, EndsWith, ".example.com"), true),
            (addr(From, Localpart, Is, "orders"), true),
            (addr(From, Localpart, StartsWith, "ord"), true),
            (addr(From, All, Contains, "outfitters"), false),
            (addr(To, Domain, Is, "lists.example.net"), true),
            (addr(To, All, Is, "billing@example.org"), false),
            (addr(Cc, All, Is, "billing@example.org"), true),
            (addr(ToOrCc, All, Is, "billing@example.org"), true),
            (addr(ToOrCc, All, Is, "mira@example.org"), true),
            (addr(ToOrCc, All, Matches, "*@example.org"), true),
            (addr(ToOrCc, All, Matches, "*@example.com"), false),
            (
                Test::Subject {
                    op: Contains,
                    value: "receipt".into(),
                },
                true,
            ),
            (
                Test::Subject {
                    op: Is,
                    value: "ignored, the field wins".into(),
                },
                false,
            ),
            (
                Test::Subject {
                    op: StartsWith,
                    value: "receipt for".into(),
                },
                true,
            ),
            (
                Test::Subject {
                    op: EndsWith,
                    value: "47720".into(),
                },
                true,
            ),
            (
                Test::Subject {
                    op: Matches,
                    value: "Receipt for order ?????".into(),
                },
                true,
            ),
            (
                Test::ListId {
                    op: Contains,
                    value: "<garden-club.example.org>".into(),
                },
                true,
            ),
            (
                Test::ListId {
                    op: Is,
                    value: "garden-club.example.org".into(),
                },
                false,
            ),
            (hdr("RECEIVED", Contains, "b.example.net"), true),
            (hdr("received", Is, "from a.example.net"), true),
            (hdr("X-Missing", Contains, ""), false),
            (hdr("from", Contains, "Larkspur Outfitters <orders@"), true),
            (hdr("to", Contains, "Team <team@lists.example.net>"), true),
            (hdr("x-delivered-to", Is, "shop.k3x9@example.org"), true),
            (
                Test::DeliveredTo {
                    op: Is,
                    value: "SHOP.k3x9@example.org".into(),
                },
                true,
            ),
            (
                Test::DeliveredTo {
                    op: Is,
                    value: "mira@example.org".into(),
                },
                false,
            ),
            (Test::SizeOver { bytes: 2047 }, true),
            (Test::SizeOver { bytes: 2048 }, false),
            (Test::SizeUnder { bytes: 2048 }, false),
            (Test::SizeUnder { bytes: 2049 }, true),
        ];
        for (test, expected) in cases {
            assert_eq!(matches(test.clone()), expected, "{test:?}");
            assert_eq!(not_matches(test.clone()), !expected, "not {test:?}");
        }
    }

    #[test]
    fn missing_header_is_false_and_its_negation_true() {
        let empty = MessageFacts::default();
        let rule = cond(Test::Subject {
            op: TextOp::Contains,
            value: String::new(),
        });
        assert!(evaluate(&rule, &empty).is_none());
        let mut negated = rule.clone();
        negated.conditions[0].negate = true;
        assert!(evaluate(&negated, &empty).is_some());
        let from = cond(addr(
            AddressHeader::From,
            AddressPart::All,
            TextOp::Matches,
            "*",
        ));
        assert!(evaluate(&from, &empty).is_none());
    }

    #[test]
    fn match_modes_and_empty_conditions() {
        let yes = Condition::new(Test::SizeOver { bytes: 0 });
        let no = Condition::new(Test::SizeUnder { bytes: 0 });
        let mut rule = Rule::new("r_m", "m");
        assert!(
            evaluate(&rule, &facts()).is_some(),
            "no conditions matches every message"
        );
        rule.conditions = vec![yes.clone(), no.clone()];
        rule.match_mode = MatchMode::Any;
        assert!(evaluate(&rule, &facts()).is_some());
        rule.match_mode = MatchMode::All;
        assert!(evaluate(&rule, &facts()).is_none());
        rule.conditions = vec![yes.clone(), yes];
        assert!(evaluate(&rule, &facts()).is_some());
        rule.enabled = false;
        assert!(evaluate(&rule, &facts()).is_none());
    }

    #[test]
    fn planned_actions_follow_execution_order() {
        let receipts = MailboxRef::new("M7d1e", ["Receipts"]);
        let mut rule = Rule::new("r_p", "p");
        rule.actions = vec![
            Action::MoveTo {
                mailbox: receipts.clone(),
            },
            Action::MarkRead,
        ];
        rule.stop = true;
        assert_eq!(
            evaluate(&rule, &facts()),
            Some(vec![
                PlannedAction::MarkRead,
                PlannedAction::MoveTo(receipts),
                PlannedAction::Stop
            ])
        );
    }

    fn rule_with(id: &str, actions: Vec<Action>) -> Rule {
        let mut rule = Rule::new(id, id);
        rule.actions = actions;
        rule
    }

    #[test]
    fn flags_carry_into_later_rules_but_not_earlier_copies() {
        let label = MailboxRef::new("Mlab", ["Label"]);
        let folder = MailboxRef::new("Mfol", ["Folder"]);
        let rules = vec![
            rule_with(
                "r_1",
                vec![Action::AddLabel {
                    mailbox: label.clone(),
                }],
            ),
            rule_with("r_2", vec![Action::MarkRead]),
            rule_with(
                "r_3",
                vec![Action::MoveTo {
                    mailbox: folder.clone(),
                }],
            ),
        ];
        let result = evaluate_all(&rules, &facts());
        assert_eq!(result.matches.len(), 3);
        assert_eq!(
            result.deliveries,
            vec![
                Delivery {
                    target: DeliveryTarget::Mailbox(label),
                    flags: BTreeSet::new(),
                },
                Delivery {
                    target: DeliveryTarget::Mailbox(folder),
                    flags: BTreeSet::from([Flag::Seen]),
                },
            ]
        );
        assert!(!result.implicit_keep);
        assert_eq!(result.flags, BTreeSet::from([Flag::Seen]));
        assert_eq!(result.stopped_by, None);
    }

    #[test]
    fn copy_keeps_the_implicit_keep() {
        let label = MailboxRef::new("Mlab", ["Label"]);
        let rules = vec![rule_with(
            "r_1",
            vec![Action::AddLabel { mailbox: label }, Action::Pin],
        )];
        let result = evaluate_all(&rules, &facts());
        assert!(result.implicit_keep);
        assert_eq!(
            result.deliveries[0].flags,
            BTreeSet::from([Flag::Flagged]),
            "the flag runs before the copy"
        );
        let trash = vec![rule_with("r_1", vec![Action::MoveToTrash])];
        let result = evaluate_all(&trash, &facts());
        assert!(!result.implicit_keep);
        assert_eq!(result.deliveries[0].target, DeliveryTarget::Trash);
    }

    #[test]
    fn stop_ends_the_run() {
        let a = MailboxRef::new("Ma", ["A"]);
        let mut first = rule_with("r_1", vec![Action::MarkRead]);
        first.stop = true;
        let second = rule_with("r_2", vec![Action::MoveTo { mailbox: a }]);
        let result = evaluate_all(&[first.clone(), second.clone()], &facts());
        assert_eq!(result.stopped_by, Some(RuleId::new("r_1")));
        assert_eq!(result.matches.len(), 1);
        assert!(result.implicit_keep);
        // A stop on a rule that does not match changes nothing.
        first
            .conditions
            .push(Condition::new(Test::SizeUnder { bytes: 0 }));
        let result = evaluate_all(&[first, second], &facts());
        assert_eq!(result.stopped_by, None);
        assert_eq!(result.matches.len(), 1);
        assert!(!result.implicit_keep);
        // A disabled rule with stop changes nothing either.
        let mut disabled = rule_with("r_3", vec![]);
        disabled.stop = true;
        disabled.enabled = false;
        assert_eq!(evaluate_all(&[disabled], &facts()).stopped_by, None);
    }

    #[test]
    fn filing_twice_into_one_mailbox_stores_one_copy() {
        let a = MailboxRef::new("Ma", ["A"]);
        let rules = vec![
            rule_with("r_1", vec![Action::AddLabel { mailbox: a.clone() }]),
            rule_with(
                "r_2",
                vec![Action::Pin, Action::MoveTo { mailbox: a.clone() }],
            ),
        ];
        let result = evaluate_all(&rules, &facts());
        assert_eq!(result.deliveries.len(), 1);
        assert_eq!(result.deliveries[0].flags, BTreeSet::from([Flag::Flagged]));
    }
}
