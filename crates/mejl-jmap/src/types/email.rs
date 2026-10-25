//! Email, Thread and SearchSnippet (RFC 8621 sections 3 to 5).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::core::{Extra, Id, UtcDate, is_false, null_as_default};

/// Standard keywords (RFC 8621 section 4.1.1 and RFC 9979).
pub mod keyword {
    pub const SEEN: &str = "$seen";
    pub const FLAGGED: &str = "$flagged";
    pub const ANSWERED: &str = "$answered";
    pub const DRAFT: &str = "$draft";
    pub const FORWARDED: &str = "$forwarded";
    pub const PHISHING: &str = "$phishing";
    pub const JUNK: &str = "$junk";
    pub const NOT_JUNK: &str = "$notjunk";
    pub const MUTED: &str = "$muted";
    pub const FOLLOWED: &str = "$followed";
    pub const MEMO: &str = "$memo";
    pub const HAS_MEMO: &str = "$hasmemo";
}

/// An email. Every property except `id` is optional because which ones are
/// present depends on the `properties` the client asked for.
///
/// Dynamic header properties such as `header:X-FM-Masked-Email:asText` land
/// in `extra`. Read them with [`Email::header_property`] or
/// [`Email::header_text`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Email {
    #[serde(default, skip_serializing_if = "Id::is_empty")]
    pub id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mailbox_ids: Option<BTreeMap<Id, bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keywords: Option<BTreeMap<String, bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_at: Option<UtcDate>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<Vec<EmailHeader>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub references: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<Vec<EmailAddress>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Vec<EmailAddress>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<Vec<EmailAddress>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc: Option<Vec<EmailAddress>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bcc: Option<Vec<EmailAddress>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<Vec<EmailAddress>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// An RFC 3339 date with the sender's offset, kept as text so the offset
    /// survives. See [`Email::sent_at_timestamp`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_structure: Option<Box<EmailBodyPart>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_values: Option<BTreeMap<String, EmailBodyValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_body: Option<Vec<EmailBodyPart>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html_body: Option<Vec<EmailBodyPart>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<EmailBodyPart>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_attachment: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,

    #[serde(flatten)]
    pub extra: Extra,
}

/// A name and address pair.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailAddress {
    #[serde(default)]
    pub name: Option<String>,
    // Missing only in broken data. An empty string keeps the rest of the
    // email readable.
    #[serde(default)]
    pub email: String,
}

impl EmailAddress {
    /// An address with no display name.
    pub fn new(email: impl Into<String>) -> Self {
        Self {
            name: None,
            email: email.into(),
        }
    }

    /// An address with a display name.
    pub fn with_name(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            email: email.into(),
        }
    }
}

/// A raw header field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailHeader {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: String,
}

/// A MIME body part.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailBodyPart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<Vec<EmailHeader>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_parts: Option<Vec<EmailBodyPart>>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The decoded text of a body part.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailBodyValue {
    #[serde(default)]
    pub value: String,
    // RFC 8621 says these must be false or absent on create, so false is
    // left out on the wire.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_encoding_problem: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_truncated: bool,
}

impl EmailBodyValue {
    /// A body value for creating an email.
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            ..Default::default()
        }
    }
}

/// The parsed forms a header property can ask for (RFC 8621 section 4.1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HeaderForm {
    Raw,
    Text,
    Addresses,
    GroupedAddresses,
    MessageIds,
    Date,
    Urls,
}

impl HeaderForm {
    /// The suffix used in the property name.
    pub fn as_str(self) -> &'static str {
        match self {
            HeaderForm::Raw => "asRaw",
            HeaderForm::Text => "asText",
            HeaderForm::Addresses => "asAddresses",
            HeaderForm::GroupedAddresses => "asGroupedAddresses",
            HeaderForm::MessageIds => "asMessageIds",
            HeaderForm::Date => "asDate",
            HeaderForm::Urls => "asURLs",
        }
    }

    fn parse(value: &str) -> Option<HeaderForm> {
        [
            HeaderForm::Raw,
            HeaderForm::Text,
            HeaderForm::Addresses,
            HeaderForm::GroupedAddresses,
            HeaderForm::MessageIds,
            HeaderForm::Date,
            HeaderForm::Urls,
        ]
        .into_iter()
        .find(|f| f.as_str() == value)
    }
}

/// Builds a dynamic header property name, such as
/// `header:X-FM-Masked-Email:asText` or `header:Received:asText:all`.
pub fn header_property(name: &str, form: HeaderForm, all: bool) -> String {
    let mut out = format!("header:{name}");
    if form != HeaderForm::Raw {
        out.push(':');
        out.push_str(form.as_str());
    }
    if all {
        out.push_str(":all");
    }
    out
}

/// The pieces of a `header:` property name.
struct HeaderKey<'a> {
    name: &'a str,
    form: HeaderForm,
    all: bool,
}

fn parse_header_key(key: &str) -> Option<HeaderKey<'_>> {
    let rest = key.strip_prefix("header:")?;
    let mut parts = rest.split(':');
    let name = parts.next().filter(|n| !n.is_empty())?;
    let mut form = HeaderForm::Raw;
    let mut all = false;
    let mut next = parts.next();
    if let Some(part) = next
        && part != "all"
    {
        form = HeaderForm::parse(part)?;
        next = parts.next();
    }
    if let Some(part) = next {
        if part != "all" {
            return None;
        }
        all = true;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(HeaderKey { name, form, all })
}

impl Email {
    /// The value of a dynamic header property, matching the header name
    /// case-insensitively and the form exactly. `Raw` matches a key with no
    /// form or with `asRaw`.
    ///
    /// Returns `None` when the property was not fetched or the header is
    /// absent (the server sends `null`).
    pub fn header_property(&self, name: &str, form: HeaderForm, all: bool) -> Option<&Value> {
        self.extra
            .iter()
            .find(|(key, _)| {
                parse_header_key(key).is_some_and(|k| {
                    k.name.eq_ignore_ascii_case(name) && k.form == form && k.all == all
                })
            })
            .map(|(_, value)| value)
            .filter(|value| !value.is_null())
    }

    /// The `asText` value of a header, such as `X-FM-Masked-Email`.
    pub fn header_text(&self, name: &str) -> Option<&str> {
        self.header_property(name, HeaderForm::Text, false)?
            .as_str()
    }

    /// Whether the email has a keyword. Keywords are case-insensitive.
    pub fn has_keyword(&self, keyword: &str) -> bool {
        self.keywords.as_ref().is_some_and(|k| {
            k.iter()
                .any(|(name, set)| *set && name.eq_ignore_ascii_case(keyword))
        })
    }

    /// Whether the email is in a mailbox.
    pub fn in_mailbox(&self, mailbox_id: &str) -> bool {
        self.mailbox_ids
            .as_ref()
            .is_some_and(|m| m.get(mailbox_id).copied().unwrap_or(false))
    }

    /// `sentAt` as a timestamp, if present and valid.
    pub fn sent_at_timestamp(&self) -> Option<UtcDate> {
        self.sent_at.as_deref()?.parse().ok()
    }
}

/// A thread: the ids of its emails, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    #[serde(default, skip_serializing_if = "Id::is_empty")]
    pub id: Id,
    #[serde(default, deserialize_with = "null_as_default")]
    pub email_ids: Vec<Id>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Highlighted search matches for one email. Matches are wrapped in
/// `<mark>` tags; the rest is HTML-escaped text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchSnippet {
    #[serde(default)]
    pub email_id: Id,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub preview: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn email_with(extra: Value) -> Email {
        serde_json::from_value(extra).unwrap()
    }

    #[test]
    fn header_property_names() {
        assert_eq!(
            header_property("X-FM-Masked-Email", HeaderForm::Text, false),
            "header:X-FM-Masked-Email:asText"
        );
        assert_eq!(
            header_property("List-Post", HeaderForm::Urls, true),
            "header:List-Post:asURLs:all"
        );
        assert_eq!(
            header_property("Received", HeaderForm::Raw, false),
            "header:Received"
        );
    }

    #[test]
    fn header_text_matches_name_case_insensitively() {
        let email = email_with(json!({
            "id": "M1",
            "header:x-fm-masked-email:asText": "masked-123456",
            "header:List-Post:asURLs": ["mailto:list@example.org"],
        }));
        assert_eq!(
            email.header_text("X-FM-Masked-Email"),
            Some("masked-123456")
        );
        assert!(
            email
                .header_property("list-post", HeaderForm::Urls, false)
                .is_some()
        );
        // Form must match exactly.
        assert!(
            email
                .header_property("List-Post", HeaderForm::Text, false)
                .is_none()
        );
        assert!(
            email
                .header_property("List-Post", HeaderForm::Urls, true)
                .is_none()
        );
    }

    #[test]
    fn header_text_null_and_missing() {
        let email = email_with(json!({
            "id": "M1",
            "header:Fastmail-MaskedEmail:asText": null,
        }));
        assert_eq!(email.header_text("Fastmail-MaskedEmail"), None);
        assert_eq!(email.header_text("X-FM-Masked-Email"), None);
    }

    #[test]
    fn header_all_and_raw_forms() {
        let email = email_with(json!({
            "id": "M1",
            "header:Received:all": [" from a", " from b"],
            "header:Subject:asRaw": " hi",
        }));
        let all = email
            .header_property("received", HeaderForm::Raw, true)
            .unwrap();
        assert_eq!(all.as_array().map(Vec::len), Some(2));
        assert!(
            email
                .header_property("Subject", HeaderForm::Raw, false)
                .is_some()
        );
    }

    #[test]
    fn malformed_header_keys_are_ignored() {
        for key in [
            "header:",
            "header::asText",
            "header:X:asNothing",
            "header:X:asText:all:extra",
            "header:X:asText:some",
        ] {
            assert!(parse_header_key(key).is_none(), "{key}");
        }
    }

    #[test]
    fn keywords_are_case_insensitive() {
        let email = email_with(json!({"id": "M1", "keywords": {"$Seen": true, "$flagged": false}}));
        assert!(email.has_keyword(keyword::SEEN));
        assert!(!email.has_keyword(keyword::FLAGGED));
        assert!(!Email::default().has_keyword(keyword::SEEN));
    }

    #[test]
    fn sent_at_keeps_offset() {
        let email = email_with(json!({"id": "M1", "sentAt": "2014-10-30T14:12:00+08:00"}));
        assert_eq!(
            serde_json::to_value(&email).unwrap()["sentAt"],
            "2014-10-30T14:12:00+08:00"
        );
        assert_eq!(
            email.sent_at_timestamp().map(|t| t.to_string()),
            Some("2014-10-30T06:12:00Z".to_string())
        );
        let bad = email_with(json!({"id": "M1", "sentAt": "not a date"}));
        assert!(bad.sent_at_timestamp().is_none());
    }

    #[test]
    fn body_value_omits_false_flags() {
        let v = EmailBodyValue::new("Hi");
        assert_eq!(serde_json::to_value(&v).unwrap(), json!({"value": "Hi"}));
    }

    #[test]
    fn thread_with_null_email_ids() {
        let t: Thread = serde_json::from_value(json!({"id": "T1", "emailIds": null})).unwrap();
        assert!(t.email_ids.is_empty());
    }
}
