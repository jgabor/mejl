//! One-line, readable summaries of rules for the rules list.

use crate::model::{Action, AddressHeader, AddressPart, Condition, MatchMode, Rule, Test, TextOp};
use crate::render::is_line_breaking;

/// Summarizes a rule's conditions and actions on one line, such as
/// `From domain is shop.example.com or Subject contains receipt -> Move to
/// Receipts, mark read`.
///
/// Actions are listed in the order the user added them. Control characters
/// in user text become spaces so the line stays one line.
pub fn summarize(rule: &Rule) -> String {
    format!(
        "{} -> {}",
        summarize_conditions(rule),
        summarize_actions(rule)
    )
}

/// The conditions part of [`summarize`]. `Every message` when there are no
/// conditions.
pub fn summarize_conditions(rule: &Rule) -> String {
    if rule.conditions.is_empty() {
        return "Every message".to_owned();
    }
    let joiner = match rule.match_mode {
        MatchMode::Any => " or ",
        MatchMode::All => " and ",
    };
    rule.conditions
        .iter()
        .map(condition_text)
        .collect::<Vec<_>>()
        .join(joiner)
}

/// The actions part of [`summarize`]. `Do nothing` when there are no actions
/// and no stop.
pub fn summarize_actions(rule: &Rule) -> String {
    let mut parts: Vec<String> = rule.actions.iter().map(action_text).collect();
    if rule.stop {
        parts.push("stop".to_owned());
    }
    if parts.is_empty() {
        return "Do nothing".to_owned();
    }
    capitalize(&parts.join(", "))
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if is_line_breaking(c) { ' ' } else { c })
        .collect()
}

fn shown_value(value: &str) -> String {
    if value.is_empty() {
        "\"\"".to_owned()
    } else {
        one_line(value)
    }
}

fn op_text(op: TextOp, negate: bool) -> &'static str {
    match (op, negate) {
        (TextOp::Is, false) => "is",
        (TextOp::Is, true) => "is not",
        (TextOp::Contains, false) => "contains",
        (TextOp::Contains, true) => "does not contain",
        (TextOp::Matches, false) => "matches",
        (TextOp::Matches, true) => "does not match",
        (TextOp::StartsWith, false) => "starts with",
        (TextOp::StartsWith, true) => "does not start with",
        (TextOp::EndsWith, false) => "ends with",
        (TextOp::EndsWith, true) => "does not end with",
    }
}

fn size_text(bytes: u64) -> String {
    const K: u64 = 1024;
    const M: u64 = 1024 * 1024;
    if bytes != 0 && bytes.is_multiple_of(M) {
        format!("{} MB", bytes / M)
    } else if bytes != 0 && bytes.is_multiple_of(K) {
        format!("{} KB", bytes / K)
    } else if bytes == 1 {
        "1 byte".to_owned()
    } else {
        format!("{bytes} bytes")
    }
}

fn condition_text(condition: &Condition) -> String {
    let negate = condition.negate;
    let (field, op, value) = match &condition.test {
        Test::Address {
            header,
            part,
            op,
            value,
        } => {
            let header = match header {
                AddressHeader::From => "From",
                AddressHeader::To => "To",
                AddressHeader::Cc => "Cc",
                AddressHeader::ToOrCc => "To or Cc",
            };
            let field = match part {
                AddressPart::All => header.to_owned(),
                AddressPart::Localpart => format!("{header} local part"),
                AddressPart::Domain => format!("{header} domain"),
            };
            (field, *op, value)
        }
        Test::Subject { op, value } => ("Subject".to_owned(), *op, value),
        Test::ListId { op, value } => ("List-Id".to_owned(), *op, value),
        Test::Header { name, op, value } => (one_line(name), *op, value),
        Test::DeliveredTo { op, value } => ("Delivered to".to_owned(), *op, value),
        Test::SizeOver { bytes } => {
            let word = if negate { "not over" } else { "over" };
            return format!("Size {word} {}", size_text(*bytes));
        }
        Test::SizeUnder { bytes } => {
            let word = if negate { "not under" } else { "under" };
            return format!("Size {word} {}", size_text(*bytes));
        }
    };
    format!("{field} {} {}", op_text(op, negate), shown_value(value))
}

fn action_text(action: &Action) -> String {
    match action {
        Action::MoveTo { mailbox } => format!("move to {}", one_line(&mailbox.display_path())),
        Action::AddLabel { mailbox } => format!("add label {}", one_line(&mailbox.display_path())),
        Action::MarkRead => "mark read".to_owned(),
        Action::Pin => "pin".to_owned(),
        Action::MoveToTrash => "move to Trash".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MailboxRef;

    #[test]
    fn task_example() {
        let mut rule = Rule::new("r_2c41", "Receipts to folder");
        rule.match_mode = MatchMode::Any;
        rule.conditions = vec![
            Condition::new(Test::Address {
                header: AddressHeader::From,
                part: AddressPart::Domain,
                op: TextOp::Is,
                value: "shop.example.com".into(),
            }),
            Condition::new(Test::Subject {
                op: TextOp::Contains,
                value: "receipt".into(),
            }),
        ];
        rule.actions = vec![
            Action::MoveTo {
                mailbox: MailboxRef::new("M7d1e", ["Receipts"]),
            },
            Action::MarkRead,
        ];
        assert_eq!(
            summarize(&rule),
            "From domain is shop.example.com or Subject contains receipt -> Move to Receipts, mark read"
        );
    }

    #[test]
    fn every_condition_and_action() {
        let mut rule = Rule::new("r_1", "x");
        rule.conditions = vec![
            Condition::negated(Test::Address {
                header: AddressHeader::ToOrCc,
                part: AddressPart::Localpart,
                op: TextOp::StartsWith,
                value: "billing".into(),
            }),
            Condition::new(Test::Address {
                header: AddressHeader::Cc,
                part: AddressPart::All,
                op: TextOp::Matches,
                value: "*@example.org".into(),
            }),
            Condition::negated(Test::Address {
                header: AddressHeader::To,
                part: AddressPart::All,
                op: TextOp::EndsWith,
                value: ".example".into(),
            }),
            Condition::new(Test::ListId {
                op: TextOp::Contains,
                value: "<garden-club.example.org>".into(),
            }),
            Condition::negated(Test::Header {
                name: "X-Spam-score".into(),
                op: TextOp::Is,
                value: String::new(),
            }),
            Condition::new(Test::DeliveredTo {
                op: TextOp::Is,
                value: "shop.k3x9@example.org".into(),
            }),
            Condition::new(Test::SizeOver { bytes: 102_400 }),
            Condition::negated(Test::SizeUnder {
                bytes: 2 * 1_048_576,
            }),
            Condition::new(Test::SizeUnder { bytes: 1 }),
            Condition::new(Test::SizeUnder { bytes: 1500 }),
            Condition::negated(Test::Subject {
                op: TextOp::Contains,
                value: "line\nbreak".into(),
            }),
        ];
        rule.actions = vec![
            Action::Pin,
            Action::AddLabel {
                mailbox: MailboxRef::new("M1", ["Work", "Clients"]),
            },
            Action::MoveToTrash,
        ];
        rule.stop = true;
        assert_eq!(
            summarize(&rule),
            "To or Cc local part does not start with billing and \
             Cc matches *@example.org and \
             To does not end with .example and \
             List-Id contains <garden-club.example.org> and \
             X-Spam-score is not \"\" and \
             Delivered to is shop.k3x9@example.org and \
             Size over 100 KB and \
             Size not under 2 MB and \
             Size under 1 byte and \
             Size under 1500 bytes and \
             Subject does not contain line break \
             -> Pin, add label Work/Clients, move to Trash, stop"
        );
    }

    #[test]
    fn empty_rule() {
        let mut rule = Rule::new("r_1", "x");
        assert_eq!(summarize(&rule), "Every message -> Do nothing");
        rule.stop = true;
        assert_eq!(summarize(&rule), "Every message -> Stop");
    }
}
