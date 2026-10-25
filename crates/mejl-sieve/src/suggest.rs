//! "New rule from this message" helpers.

use crate::matcher::MessageFacts;
use crate::model::{AddressHeader, AddressPart, Condition, Rule, RuleId, Test, TextOp};
use crate::render::is_line_breaking;

/// Longest generated rule name, in characters.
const MAX_NAME_CHARS: usize = 80;

/// What to build a rule from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SuggestionKind {
    /// The sender's whole address: From is `orders@shop.example.com`.
    SenderAddress,
    /// The sender's domain: From domain is `shop.example.com`.
    SenderDomain,
    /// The mailing list: List-Id contains `<garden-club.example.org>`.
    ListId,
    /// The address the message was delivered to, such as a masked address:
    /// Delivered to is `shop.k3x9@example.com`.
    MaskedAddress,
}

/// Builds an enabled rule with one condition taken from the message and no
/// actions yet, for the user to finish in the editor.
///
/// The caller supplies the id, since this crate has no id source. Returns
/// `None` when the message lacks what `kind` needs, or when that value holds
/// control characters, which a rule cannot render.
pub fn rule_from_message(
    id: impl Into<RuleId>,
    facts: &MessageFacts,
    kind: SuggestionKind,
) -> Option<Rule> {
    let (name, test) = match kind {
        SuggestionKind::SenderAddress => {
            let sender = facts.from.first()?;
            let email = clean_value(&sender.email)?;
            let label = sender
                .name
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .unwrap_or(email);
            (
                format!("From {label}"),
                Test::Address {
                    header: AddressHeader::From,
                    part: AddressPart::All,
                    op: TextOp::Is,
                    value: email.to_owned(),
                },
            )
        }
        SuggestionKind::SenderDomain => {
            let sender = facts.from.first()?;
            let (_, domain) = sender.email.trim().rsplit_once('@')?;
            let domain = clean_value(domain)?.to_ascii_lowercase();
            (
                format!("From {domain}"),
                Test::Address {
                    header: AddressHeader::From,
                    part: AddressPart::Domain,
                    op: TextOp::Is,
                    value: domain,
                },
            )
        }
        SuggestionKind::ListId => {
            let header = facts.header_values("list-id").into_iter().next()?;
            let (phrase, value) = split_list_id(&header)?;
            let value = clean_value(&value)?.to_owned();
            let label = phrase.unwrap_or_else(|| {
                value
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_owned()
            });
            (
                format!("List {label}"),
                Test::ListId {
                    op: TextOp::Contains,
                    value,
                },
            )
        }
        SuggestionKind::MaskedAddress => {
            let address = clean_value(facts.delivered_to.first()?)?;
            (
                format!("Sent to {address}"),
                Test::DeliveredTo {
                    op: TextOp::Is,
                    value: address.to_owned(),
                },
            )
        }
    };
    let mut rule = Rule::new(id, clean_name(&name));
    rule.conditions.push(Condition::new(test));
    Some(rule)
}

/// A trimmed, non-empty value without control characters.
fn clean_value(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty() && !value.chars().any(is_line_breaking)).then_some(value)
}

/// Control characters become spaces and long names are cut, since names
/// come from message headers that anyone can write.
fn clean_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if is_line_breaking(c) { ' ' } else { c })
        .collect();
    let mut out: String = cleaned.trim().chars().take(MAX_NAME_CHARS).collect();
    if cleaned.trim().chars().count() > MAX_NAME_CHARS {
        out.push('…');
    }
    out
}

/// Splits a List-Id value into its display phrase and the value to match.
/// `Garden Club <garden-club.example.org>` gives the phrase `Garden Club`
/// and `<garden-club.example.org>`, brackets kept so the condition does not
/// also match a sub-list. Without brackets the whole value is used.
fn split_list_id(header: &str) -> Option<(Option<String>, String)> {
    let header = header.trim();
    if let Some(open) = header.rfind('<')
        && let Some(len) = header[open..].find('>')
    {
        let id = header[open + 1..open + len].trim();
        if id.is_empty() {
            return None;
        }
        let phrase = header[..open].trim().trim_matches('"').trim();
        let phrase = (!phrase.is_empty()).then(|| phrase.to_owned());
        return Some((phrase, format!("<{id}>")));
    }
    (!header.is_empty()).then(|| (None, header.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matcher::{EmailAddress, evaluate};
    use crate::model::Action;

    fn facts() -> MessageFacts {
        MessageFacts {
            from: vec![EmailAddress::new(
                "Juniper Tran",
                "juniper@Shop.Example.com",
            )],
            headers: vec![(
                "List-Id".into(),
                "\"Garden Club\" <garden-club.example.org>".into(),
            )],
            delivered_to: vec!["shop.k3x9@example.org".into()],
            subject: Some("Hello".into()),
            ..MessageFacts::default()
        }
    }

    #[test]
    fn each_kind_builds_a_rule_that_matches_its_own_message() {
        let facts = facts();
        for kind in [
            SuggestionKind::SenderAddress,
            SuggestionKind::SenderDomain,
            SuggestionKind::ListId,
            SuggestionKind::MaskedAddress,
        ] {
            let mut rule = rule_from_message("r_new", &facts, kind).expect("rule");
            assert!(rule.enabled);
            assert!(rule.actions.is_empty());
            rule.actions.push(Action::MarkRead);
            assert!(evaluate(&rule, &facts).is_some(), "{kind:?}");
            assert!(
                crate::render::render_block(&[rule]).is_ok(),
                "{kind:?} renders"
            );
        }
    }

    #[test]
    fn names_and_values() {
        let facts = facts();
        let rule = rule_from_message("r_1", &facts, SuggestionKind::SenderAddress).unwrap();
        assert_eq!(rule.name, "From Juniper Tran");
        let rule = rule_from_message("r_1", &facts, SuggestionKind::SenderDomain).unwrap();
        assert_eq!(rule.name, "From shop.example.com");
        assert_eq!(
            rule.conditions[0].test,
            Test::Address {
                header: AddressHeader::From,
                part: AddressPart::Domain,
                op: TextOp::Is,
                value: "shop.example.com".into()
            }
        );
        let rule = rule_from_message("r_1", &facts, SuggestionKind::ListId).unwrap();
        assert_eq!(rule.name, "List Garden Club");
        assert_eq!(
            rule.conditions[0].test,
            Test::ListId {
                op: TextOp::Contains,
                value: "<garden-club.example.org>".into()
            }
        );
        let rule = rule_from_message("r_1", &facts, SuggestionKind::MaskedAddress).unwrap();
        assert_eq!(rule.name, "Sent to shop.k3x9@example.org");
    }

    #[test]
    fn missing_data_gives_none() {
        let empty = MessageFacts::default();
        for kind in [
            SuggestionKind::SenderAddress,
            SuggestionKind::SenderDomain,
            SuggestionKind::ListId,
            SuggestionKind::MaskedAddress,
        ] {
            assert!(rule_from_message("r_1", &empty, kind).is_none(), "{kind:?}");
        }
        let no_domain = MessageFacts {
            from: vec![EmailAddress::bare("undisclosed")],
            ..MessageFacts::default()
        };
        assert!(rule_from_message("r_1", &no_domain, SuggestionKind::SenderDomain).is_none());
        let trailing_at = MessageFacts {
            from: vec![EmailAddress::bare("x@")],
            ..MessageFacts::default()
        };
        assert!(rule_from_message("r_1", &trailing_at, SuggestionKind::SenderDomain).is_none());
        let empty_list = MessageFacts {
            headers: vec![("List-Id".into(), "Name <>".into())],
            ..MessageFacts::default()
        };
        assert!(rule_from_message("r_1", &empty_list, SuggestionKind::ListId).is_none());
    }

    #[test]
    fn hostile_headers_are_cleaned_or_refused() {
        let hostile = MessageFacts {
            from: vec![EmailAddress::new(
                format!("Evil\r\nkeep;{}", "x".repeat(200)),
                "evil@example.net",
            )],
            delivered_to: vec!["a@example.org\nkeep;".into()],
            headers: vec![("List-Id".into(), "plain-list.example.net".into())],
            ..MessageFacts::default()
        };
        let rule = rule_from_message("r_1", &hostile, SuggestionKind::SenderAddress).unwrap();
        assert!(!rule.name.chars().any(char::is_control));
        assert_eq!(rule.name.chars().count(), MAX_NAME_CHARS + 1);
        assert!(crate::render::render_block(&[rule]).is_ok());
        assert!(rule_from_message("r_1", &hostile, SuggestionKind::MaskedAddress).is_none());
        let rule = rule_from_message("r_1", &hostile, SuggestionKind::ListId).unwrap();
        assert_eq!(
            rule.conditions[0].test,
            Test::ListId {
                op: TextOp::Contains,
                value: "plain-list.example.net".into()
            }
        );
    }
}
