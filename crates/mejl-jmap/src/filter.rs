//! Query filters and sort comparators (RFC 8620 section 5.5, RFC 8621
//! section 4.4).

use serde::{Deserialize, Serialize};

use crate::types::core::{Extra, Id, UtcDate};

/// A filter: either a condition or an operator over nested filters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Filter<C> {
    // Listed first: untagged tries variants in order, and an all-optional
    // condition with a flattened map would otherwise accept any object.
    Operator(FilterOperator<C>),
    Condition(C),
}

/// `AND`, `OR` or `NOT` over a list of filters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilterOperator<C> {
    pub operator: Operator,
    pub conditions: Vec<Filter<C>>,
}

/// A filter operator. `NOT` means none of the conditions match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Operator {
    #[serde(rename = "AND")]
    And,
    #[serde(rename = "OR")]
    Or,
    #[serde(rename = "NOT")]
    Not,
}

impl<C> Filter<C> {
    /// All of the filters must match.
    pub fn and(conditions: impl IntoIterator<Item = Filter<C>>) -> Self {
        Self::op(Operator::And, conditions)
    }

    /// Any of the filters must match.
    pub fn or(conditions: impl IntoIterator<Item = Filter<C>>) -> Self {
        Self::op(Operator::Or, conditions)
    }

    /// None of the filters may match.
    pub fn not(conditions: impl IntoIterator<Item = Filter<C>>) -> Self {
        Self::op(Operator::Not, conditions)
    }

    fn op(operator: Operator, conditions: impl IntoIterator<Item = Filter<C>>) -> Self {
        Filter::Operator(FilterOperator {
            operator,
            conditions: conditions.into_iter().collect(),
        })
    }
}

impl<C> From<C> for Filter<C> {
    fn from(condition: C) -> Self {
        Filter::Condition(condition)
    }
}

/// An `Email/query` filter condition. All set fields must match.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailFilterCondition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_mailbox: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_mailbox_other_than: Option<Vec<Id>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<UtcDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<UtcDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub all_in_thread_have_keyword: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub some_in_thread_have_keyword: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub none_in_thread_have_keyword: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_keyword: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_keyword: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_attachment: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bcc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// One element: the header must exist. Two: its value must contain the
    /// second string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<Vec<String>>,
    /// Vendor conditions.
    #[serde(flatten)]
    pub extra: Extra,
}

/// The `Email/query` filter type.
pub type EmailFilter = Filter<EmailFilterCondition>;

/// A sort comparator.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Comparator {
    pub property: String,
    /// Defaults to `true` on the server when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_ascending: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collation: Option<String>,
    /// For the keyword sorts in RFC 8621, such as `hasKeyword`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyword: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Comparator {
    /// Sort ascending by a property.
    pub fn asc(property: impl Into<String>) -> Self {
        Self {
            property: property.into(),
            is_ascending: Some(true),
            ..Default::default()
        }
    }

    /// Sort descending by a property.
    pub fn desc(property: impl Into<String>) -> Self {
        Self {
            property: property.into(),
            is_ascending: Some(false),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn operator_is_not_swallowed_by_condition() {
        let json = json!({
            "operator": "OR",
            "conditions": [{"hasKeyword": "music"}, {"hasKeyword": "video"}]
        });
        let filter: EmailFilter = serde_json::from_value(json.clone()).unwrap();
        let Filter::Operator(op) = &filter else {
            panic!("expected operator, got {filter:?}");
        };
        assert_eq!(op.operator, Operator::Or);
        assert_eq!(op.conditions.len(), 2);
        assert_eq!(serde_json::to_value(&filter).unwrap(), json);
    }

    #[test]
    fn nested_filters_round_trip() {
        let filter = EmailFilter::and([
            EmailFilterCondition {
                in_mailbox: Some("inbox".into()),
                ..Default::default()
            }
            .into(),
            EmailFilter::not([EmailFilterCondition {
                has_keyword: Some("$seen".into()),
                ..Default::default()
            }
            .into()]),
        ]);
        let json = serde_json::to_value(&filter).unwrap();
        assert_eq!(
            json,
            json!({"operator": "AND", "conditions": [
                {"inMailbox": "inbox"},
                {"operator": "NOT", "conditions": [{"hasKeyword": "$seen"}]}
            ]})
        );
        let back: EmailFilter = serde_json::from_value(json).unwrap();
        assert_eq!(back, filter);
    }

    #[test]
    fn unknown_condition_keys_kept() {
        let c: EmailFilter = serde_json::from_value(json!({"x-vendor": 1, "text": "hi"})).unwrap();
        let Filter::Condition(c) = c else { panic!() };
        assert_eq!(c.text.as_deref(), Some("hi"));
        assert_eq!(c.extra["x-vendor"], 1);
    }

    #[test]
    fn comparators() {
        assert_eq!(
            serde_json::to_value(Comparator::desc("receivedAt")).unwrap(),
            json!({"property": "receivedAt", "isAscending": false})
        );
    }
}
