//! ContactCard (RFC 9610, data in JSContact format, RFC 9553).
//!
//! Only the properties mejl uses are typed. Everything else, including
//! `@type`, `version`, `addressBookIds` and phones, stays in `extra`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::core::{Extra, Id};

/// A contact card or contact group.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactCard {
    #[serde(default, skip_serializing_if = "Id::is_empty")]
    pub id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    /// `individual`, `group`, `org` and so on. Absent means individual.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<ContactName>,
    /// Email addresses keyed by an id local to the card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emails: Option<BTreeMap<String, ContactEmail>>,
    /// For groups: member card uids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members: Option<BTreeMap<String, bool>>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A JSContact `Name`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactName {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub components: Option<Vec<NameComponent>>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// One part of a name, such as the given name.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NameComponent {
    /// `given`, `surname`, `title` and so on.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub value: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A JSContact `EmailAddress`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactEmail {
    #[serde(default)]
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contexts: Option<BTreeMap<String, bool>>,
    /// Lower is preferred, 1 to 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pref: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl ContactCard {
    /// Whether this card is a group.
    pub fn is_group(&self) -> bool {
        self.kind.as_deref() == Some("group")
    }

    /// A name to show: `name.full`, or the components joined with spaces.
    pub fn display_name(&self) -> Option<String> {
        let name = self.name.as_ref()?;
        if let Some(full) = name
            .full
            .as_deref()
            .map(str::trim)
            .filter(|f| !f.is_empty())
        {
            return Some(full.to_owned());
        }
        let joined = name
            .components
            .iter()
            .flatten()
            .filter(|c| c.kind != "separator")
            .map(|c| c.value.trim())
            .filter(|v| !v.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        (!joined.is_empty()).then_some(joined)
    }

    /// Email addresses, most preferred first.
    pub fn email_addresses(&self) -> Vec<&str> {
        let mut emails: Vec<&ContactEmail> = self.emails.iter().flat_map(|m| m.values()).collect();
        emails.sort_by_key(|e| e.pref.unwrap_or(u32::MAX));
        emails
            .into_iter()
            .map(|e| e.address.as_str())
            .filter(|a| !a.is_empty())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn display_name_prefers_full() {
        let card: ContactCard = serde_json::from_value(json!({
            "id": "c1",
            "name": {"full": " Robin Example ", "components": [{"kind": "given", "value": "R"}]}
        }))
        .unwrap();
        assert_eq!(card.display_name().as_deref(), Some("Robin Example"));
    }

    #[test]
    fn display_name_from_components() {
        let card: ContactCard = serde_json::from_value(json!({
            "id": "c1",
            "name": {"components": [
                {"kind": "given", "value": "Robin"},
                {"kind": "separator", "value": ", "},
                {"kind": "surname", "value": "Example"}
            ]}
        }))
        .unwrap();
        assert_eq!(card.display_name().as_deref(), Some("Robin Example"));
        assert_eq!(ContactCard::default().display_name(), None);
    }

    #[test]
    fn emails_sorted_by_pref() {
        let card: ContactCard = serde_json::from_value(json!({
            "id": "c1",
            "emails": {
                "a": {"address": "home@example.org", "pref": 5},
                "b": {"address": "work@example.com", "pref": 1},
                "c": {"address": "old@example.net"},
                "d": {"address": ""}
            }
        }))
        .unwrap();
        assert_eq!(
            card.email_addresses(),
            vec!["work@example.com", "home@example.org", "old@example.net"]
        );
    }
}
