//! Mailbox (RFC 8621 section 2).

use serde::{Deserialize, Serialize};

use super::core::{Extra, Id, string_enum};

string_enum! {
    /// A mailbox role from the IANA "IMAP Mailbox Name Attributes" registry.
    pub enum Role {
        All => "all",
        Archive => "archive",
        Drafts => "drafts",
        Flagged => "flagged",
        Important => "important",
        Inbox => "inbox",
        Junk => "junk",
        Sent => "sent",
        Subscribed => "subscribed",
        Trash => "trash",
        /// RFC 9979.
        Snoozed => "snoozed",
        /// RFC 9979.
        Scheduled => "scheduled",
        /// RFC 9979.
        Memos => "memos",
        @other Other,
    }
}

/// A mailbox. Every property except `id` is optional because `/get` with a
/// `properties` list and `/set` `created` return partial objects.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mailbox {
    #[serde(default, skip_serializing_if = "Id::is_empty")]
    pub id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `None` for a top-level mailbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_emails: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unread_emails: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_threads: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unread_threads: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_rights: Option<MailboxRights>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_subscribed: Option<bool>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// What the user may do with a mailbox.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailboxRights {
    #[serde(default)]
    pub may_read_items: bool,
    #[serde(default)]
    pub may_add_items: bool,
    #[serde(default)]
    pub may_remove_items: bool,
    #[serde(default)]
    pub may_set_seen: bool,
    #[serde(default)]
    pub may_set_keywords: bool,
    #[serde(default)]
    pub may_create_child: bool,
    #[serde(default)]
    pub may_rename: bool,
    #[serde(default)]
    pub may_delete: bool,
    #[serde(default)]
    pub may_submit: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_mailbox_parses() {
        let mb: Mailbox = serde_json::from_str(r#"{"id":"M1","role":"snoozed"}"#).unwrap();
        assert_eq!(mb.role, Some(Role::Snoozed));
        assert!(mb.name.is_none());
    }

    #[test]
    fn unknown_role_and_fields_survive() {
        let json = r#"{"id":"M1","role":"x-future","isCollapsed":true}"#;
        let mb: Mailbox = serde_json::from_str(json).unwrap();
        assert_eq!(mb.role, Some(Role::Other("x-future".into())));
        assert_eq!(mb.extra["isCollapsed"], serde_json::Value::Bool(true));
        let back = serde_json::to_value(&mb).unwrap();
        assert_eq!(
            back,
            serde_json::from_str::<serde_json::Value>(json).unwrap()
        );
    }

    #[test]
    fn create_object_omits_id() {
        let mb = Mailbox {
            name: Some("Receipts".into()),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_string(&mb).unwrap(),
            r#"{"name":"Receipts"}"#
        );
    }
}
