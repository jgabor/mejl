//! Identity and EmailSubmission (RFC 8621 sections 6 and 7).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::core::{Extra, Id, UtcDate, string_enum};
use super::email::EmailAddress;

/// A sending identity.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    #[serde(default, skip_serializing_if = "Id::is_empty")]
    pub id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// May be `*@domain`, meaning any address at that domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<Vec<EmailAddress>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bcc: Option<Vec<EmailAddress>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html_signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub may_delete: Option<bool>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Identity {
    /// Whether this identity may send as `address`, honouring `*@domain`.
    pub fn allows(&self, address: &str) -> bool {
        let Some(email) = self.email.as_deref() else {
            return false;
        };
        if let Some(domain) = email.strip_prefix("*@") {
            return address
                .rsplit_once('@')
                .is_some_and(|(local, d)| !local.is_empty() && d.eq_ignore_ascii_case(domain));
        }
        email.eq_ignore_ascii_case(address)
    }
}

string_enum! {
    /// Whether a submission can still be cancelled.
    pub enum UndoStatus {
        Pending => "pending",
        Final => "final",
        Canceled => "canceled",
        @other Other,
    }
}

/// A message submission for delivery.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailSubmission {
    #[serde(default, skip_serializing_if = "Id::is_empty")]
    pub id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_id: Option<Id>,
    /// An email id, or `#creationId` to refer to an email created earlier in
    /// the same request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email_id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope: Option<Envelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_at: Option<UtcDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo_status: Option<UndoStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_status: Option<BTreeMap<String, DeliveryStatus>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsn_blob_ids: Option<Vec<Id>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mdn_blob_ids: Option<Vec<Id>>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The SMTP envelope.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    pub mail_from: EnvelopeAddress,
    #[serde(default)]
    pub rcpt_to: Vec<EnvelopeAddress>,
}

/// An envelope address with optional SMTP parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeAddress {
    pub email: String,
    /// SMTP extension parameters, for example `HOLDFOR`. A `None` value is a
    /// parameter with no value. The whole map is `null` when there are none.
    #[serde(default)]
    pub parameters: Option<BTreeMap<String, Option<String>>>,
}

impl EnvelopeAddress {
    /// An address with no parameters.
    pub fn new(email: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            parameters: None,
        }
    }
}

/// Delivery state for one recipient.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryStatus {
    #[serde(default)]
    pub smtp_reply: String,
    /// `queued`, `yes`, `no` or `unknown`.
    #[serde(default)]
    pub delivered: String,
    /// `unknown` or `yes`.
    #[serde(default)]
    pub displayed: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(email: &str) -> Identity {
        Identity {
            email: Some(email.into()),
            ..Default::default()
        }
    }

    #[test]
    fn identity_allows_exact_and_wildcard() {
        assert!(identity("alice@example.com").allows("Alice@Example.com"));
        assert!(!identity("alice@example.com").allows("bob@example.com"));
        let wild = identity("*@example.org");
        assert!(wild.allows("anything@example.org"));
        assert!(wild.allows("x@EXAMPLE.org"));
        assert!(!wild.allows("@example.org"));
        assert!(!wild.allows("x@example.net"));
        assert!(!wild.allows("no-at-sign"));
        assert!(!Identity::default().allows("alice@example.com"));
    }

    #[test]
    fn envelope_parameters_keep_nulls() {
        let mut params = BTreeMap::new();
        params.insert("HOLDFOR".to_string(), Some("60".to_string()));
        params.insert("SMTPUTF8".to_string(), None);
        let addr = EnvelopeAddress {
            email: "alice@example.com".into(),
            parameters: Some(params),
        };
        let json = serde_json::to_value(&addr).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"email": "alice@example.com",
                "parameters": {"HOLDFOR": "60", "SMTPUTF8": null}})
        );
        let back: EnvelopeAddress = serde_json::from_value(json).unwrap();
        assert_eq!(back, addr);
    }

    #[test]
    fn undo_status_values() {
        let s: UndoStatus = serde_json::from_str("\"canceled\"").unwrap();
        assert_eq!(s, UndoStatus::Canceled);
    }
}
