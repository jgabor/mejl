//! Fastmail's MaskedEmail object (`https://www.fastmail.com/dev/maskedemail`).

use serde::{Deserialize, Serialize};

use super::core::{Extra, Id, UtcDate, lenient_date, string_enum};

string_enum! {
    /// The state of a masked address.
    pub enum MaskedEmailState {
        /// Created but unused. The server deletes it after 24 hours unless
        /// mail arrives first.
        Pending => "pending",
        /// Receives mail normally.
        Enabled => "enabled",
        /// Accepts mail and files it straight to Trash.
        Disabled => "disabled",
        /// Bounces mail. Can be restored.
        Deleted => "deleted",
        @other Unknown,
    }
}

/// A masked email address.
///
/// All properties except `id` are optional: `/set` `created` returns only
/// the server-set ones (`id`, `email`, `createdAt`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaskedEmail {
    #[serde(default, skip_serializing_if = "Id::is_empty")]
    pub id: Id,
    /// Server-generated. Never predict it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<MaskedEmailState>,
    /// The origin of the site the address is for. Real data also has bare
    /// hosts and empty strings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub for_domain: Option<String>,
    /// The user's own note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    // Dates are parsed leniently: this extension is less strict than core
    // JMAP, and one odd date must not hide every mask.
    #[serde(
        default,
        deserialize_with = "lenient_date",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_message_at: Option<UtcDate>,
    #[serde(
        default,
        deserialize_with = "lenient_date",
        skip_serializing_if = "Option::is_none"
    )]
    pub created_at: Option<UtcDate>,
    /// Set by the server from the credentials. Clients must not set it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    /// A deep link back into the creating app, not the site URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Create only, never returned. See [`MaskedEmail::is_valid_prefix`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email_prefix: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl MaskedEmail {
    /// The longest `emailPrefix` the server accepts.
    pub const MAX_PREFIX_LEN: usize = 64;

    /// Whether `prefix` meets the documented rules: 1 to 64 characters of
    /// `a-z`, `0-9` and `_`. The server may still reject reserved words.
    pub fn is_valid_prefix(prefix: &str) -> bool {
        !prefix.is_empty()
            && prefix.len() <= Self::MAX_PREFIX_LEN
            && prefix
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    }

    /// A create object for an address that is usable at once.
    ///
    /// New masks default to `pending`, which the server deletes after a day,
    /// so mejl always creates them `enabled`.
    pub fn new_enabled(description: impl Into<String>) -> Self {
        Self {
            state: Some(MaskedEmailState::Enabled),
            description: Some(description.into()),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prefix_rules() {
        assert!(MaskedEmail::is_valid_prefix("bikes_2026"));
        assert!(!MaskedEmail::is_valid_prefix(""));
        assert!(!MaskedEmail::is_valid_prefix("Bikes"));
        assert!(!MaskedEmail::is_valid_prefix("bike-shop"));
        assert!(!MaskedEmail::is_valid_prefix("bikés"));
        assert!(MaskedEmail::is_valid_prefix(&"a".repeat(64)));
        assert!(!MaskedEmail::is_valid_prefix(&"a".repeat(65)));
    }

    #[test]
    fn unknown_state_falls_back() {
        let m: MaskedEmail =
            serde_json::from_value(json!({"id": "masked-1", "state": "archived"})).unwrap();
        assert_eq!(m.state, Some(MaskedEmailState::Unknown("archived".into())));
        let empty: MaskedEmail =
            serde_json::from_value(json!({"id": "masked-1", "state": ""})).unwrap();
        assert_eq!(empty.state, Some(MaskedEmailState::Unknown(String::new())));
    }

    #[test]
    fn odd_dates_do_not_fail_the_object() {
        let m: MaskedEmail = serde_json::from_value(json!({
            "id": "masked-1",
            "createdAt": "",
            "lastMessageAt": null,
        }))
        .unwrap();
        assert!(m.created_at.is_none());
        assert!(m.last_message_at.is_none());
    }

    #[test]
    fn new_enabled_create_object() {
        let mut m = MaskedEmail::new_enabled("Bike shop");
        m.email_prefix = Some("bikes".into());
        assert_eq!(
            serde_json::to_value(&m).unwrap(),
            json!({"state": "enabled", "description": "Bike shop", "emailPrefix": "bikes"})
        );
    }
}
