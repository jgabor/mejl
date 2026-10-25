//! Lowering from the rule model to a small Sieve syntax tree.
//!
//! The renderer prints this tree and the dry-run matcher evaluates it. Both
//! work from the same tree, so the dry run cannot drift from what the block
//! says.

use crate::model::{Action, AddressHeader, AddressPart, Condition, MatchMode, Rule, Test, TextOp};

/// Sieve match types used by mejl.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MatchType {
    Is,
    Contains,
    Matches,
}

impl MatchType {
    pub(crate) fn tag(self) -> &'static str {
        match self {
            MatchType::Is => ":is",
            MatchType::Contains => ":contains",
            MatchType::Matches => ":matches",
        }
    }
}

/// A Sieve test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SieveTest {
    True,
    Address {
        headers: &'static [&'static str],
        part: AddressPart,
        match_type: MatchType,
        key: String,
    },
    Header {
        name: String,
        match_type: MatchType,
        key: String,
    },
    Size {
        over: bool,
        limit: u64,
    },
    Not(Box<SieveTest>),
    AnyOf(Vec<SieveTest>),
    AllOf(Vec<SieveTest>),
}

/// Header names mejl tests. Lowercase, as in the mockup and Fastmail's own
/// generated code. Header names compare case-insensitively in Sieve.
pub(crate) const SUBJECT: &str = "subject";
pub(crate) const LIST_ID: &str = "list-id";
pub(crate) const DELIVERED_TO: &str = "x-delivered-to";

fn address_headers(header: AddressHeader) -> &'static [&'static str] {
    match header {
        AddressHeader::From => &["from"],
        AddressHeader::To => &["to"],
        AddressHeader::Cc => &["cc"],
        AddressHeader::ToOrCc => &["to", "cc"],
    }
}

/// Escapes the glob characters `\`, `*` and `?` so the text matches itself
/// under `:matches`.
pub(crate) fn escape_glob(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '*' | '?') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Maps a text operator and value to a Sieve match type and key.
pub(crate) fn match_key(op: TextOp, value: &str) -> (MatchType, String) {
    match op {
        TextOp::Is => (MatchType::Is, value.to_owned()),
        TextOp::Contains => (MatchType::Contains, value.to_owned()),
        TextOp::Matches => (MatchType::Matches, value.to_owned()),
        TextOp::StartsWith => (MatchType::Matches, format!("{}*", escape_glob(value))),
        TextOp::EndsWith => (MatchType::Matches, format!("*{}", escape_glob(value))),
    }
}

fn header_test(name: &str, op: TextOp, value: &str) -> SieveTest {
    let (match_type, key) = match_key(op, value);
    SieveTest::Header {
        name: name.to_owned(),
        match_type,
        key,
    }
}

pub(crate) fn lower_condition(condition: &Condition) -> SieveTest {
    let test = match &condition.test {
        Test::Address {
            header,
            part,
            op,
            value,
        } => {
            let (match_type, key) = match_key(*op, value);
            SieveTest::Address {
                headers: address_headers(*header),
                part: *part,
                match_type,
                key,
            }
        }
        Test::Subject { op, value } => header_test(SUBJECT, *op, value),
        Test::ListId { op, value } => header_test(LIST_ID, *op, value),
        Test::Header { name, op, value } => header_test(name, *op, value),
        Test::DeliveredTo { op, value } => header_test(DELIVERED_TO, *op, value),
        Test::SizeOver { bytes } => SieveTest::Size {
            over: true,
            limit: *bytes,
        },
        Test::SizeUnder { bytes } => SieveTest::Size {
            over: false,
            limit: *bytes,
        },
    };
    if condition.negate {
        SieveTest::Not(Box::new(test))
    } else {
        test
    }
}

/// The test for a whole rule, ignoring `enabled`. No conditions means
/// `true`, since Sieve has no empty `anyof` or `allof`.
pub(crate) fn lower_rule_test(rule: &Rule) -> SieveTest {
    let mut tests: Vec<SieveTest> = rule.conditions.iter().map(lower_condition).collect();
    match tests.len() {
        0 => SieveTest::True,
        1 => tests.remove(0),
        _ => match rule.match_mode {
            MatchMode::Any => SieveTest::AnyOf(tests),
            MatchMode::All => SieveTest::AllOf(tests),
        },
    }
}

/// The actions in the order the script runs them: flags first, then label
/// copies, then moves, then Trash. `addflag` only affects `fileinto`
/// commands after it (RFC 5232), so flags have to come first for "move and
/// mark read" to file a read message.
pub(crate) fn execution_order(actions: &[Action]) -> Vec<&Action> {
    fn rank(action: &Action) -> u8 {
        match action {
            Action::MarkRead | Action::Pin => 0,
            Action::AddLabel { .. } => 1,
            Action::MoveTo { .. } => 2,
            Action::MoveToTrash => 3,
        }
    }
    let mut ordered: Vec<&Action> = actions.iter().collect();
    // A stable sort keeps the user's order inside each group.
    ordered.sort_by_key(|a| rank(a));
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MailboxRef;

    #[test]
    fn glob_escaping() {
        assert_eq!(escape_glob("plain"), "plain");
        assert_eq!(escape_glob("a*b?c\\d"), "a\\*b\\?c\\\\d");
        assert_eq!(escape_glob(""), "");
        assert_eq!(escape_glob("ünï*"), "ünï\\*");
    }

    #[test]
    fn starts_and_ends_with_become_matches() {
        assert_eq!(
            match_key(TextOp::StartsWith, "inv*"),
            (MatchType::Matches, "inv\\**".to_owned())
        );
        assert_eq!(
            match_key(TextOp::EndsWith, "?x"),
            (MatchType::Matches, "*\\?x".to_owned())
        );
        assert_eq!(
            match_key(TextOp::Matches, "a*"),
            (MatchType::Matches, "a*".to_owned())
        );
    }

    #[test]
    fn execution_order_puts_flags_first_and_keeps_user_order() {
        let a = MailboxRef::new("A", ["A"]);
        let b = MailboxRef::new("B", ["B"]);
        let actions = vec![
            Action::MoveToTrash,
            Action::MoveTo { mailbox: a.clone() },
            Action::AddLabel { mailbox: b.clone() },
            Action::Pin,
            Action::MoveTo { mailbox: b.clone() },
            Action::MarkRead,
        ];
        let ordered: Vec<Action> = execution_order(&actions).into_iter().cloned().collect();
        assert_eq!(
            ordered,
            vec![
                Action::Pin,
                Action::MarkRead,
                Action::AddLabel { mailbox: b.clone() },
                Action::MoveTo { mailbox: a },
                Action::MoveTo { mailbox: b },
                Action::MoveToTrash,
            ]
        );
    }

    #[test]
    fn rule_test_shapes() {
        let mut rule = Rule::new("r_1", "x");
        assert_eq!(lower_rule_test(&rule), SieveTest::True);
        rule.conditions
            .push(Condition::new(Test::SizeOver { bytes: 1 }));
        assert_eq!(
            lower_rule_test(&rule),
            SieveTest::Size {
                over: true,
                limit: 1
            }
        );
        rule.conditions
            .push(Condition::negated(Test::SizeUnder { bytes: 2 }));
        rule.match_mode = MatchMode::Any;
        assert!(matches!(lower_rule_test(&rule), SieveTest::AnyOf(v) if v.len() == 2));
        rule.match_mode = MatchMode::All;
        let SieveTest::AllOf(v) = lower_rule_test(&rule) else {
            panic!("expected allof");
        };
        assert!(matches!(&v[1], SieveTest::Not(_)));
    }
}
