//! The fake account's data model.
//!
//! Objects are typed here and turned into JMAP JSON at the protocol edge, in
//! the `api` modules. Only `keywords` and `mailboxIds` of an email change
//! after creation, as in RFC 8621.

use std::borrow::Cow;
use std::collections::BTreeSet;

use jiff::Timestamp;
use serde_json::{Map, Value, json};

use crate::text;

/// A mailbox address with an optional display name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Address {
    /// The display name, if any.
    pub name: Option<String>,
    /// The address itself, such as `ada@example.com`.
    pub email: String,
}

impl Address {
    /// An address with a display name.
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Address {
            name: Some(name.into()),
            email: email.into(),
        }
    }

    /// An address without a display name.
    pub fn bare(email: impl Into<String>) -> Self {
        Address {
            name: None,
            email: email.into(),
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({ "name": self.name, "email": self.email })
    }

    /// Parses a JMAP `EmailAddress`. Unknown keys are rejected.
    pub(crate) fn from_json(v: &Value) -> Option<Address> {
        let obj = v.as_object()?;
        if obj.keys().any(|k| k != "name" && k != "email") {
            return None;
        }
        let email = obj.get("email")?.as_str()?.to_owned();
        let name = match obj.get("name") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => return None,
        };
        Some(Address { name, email })
    }
}

pub(crate) fn addresses_json(list: &Option<Vec<Address>>) -> Value {
    match list {
        None => Value::Null,
        Some(list) => Value::Array(list.iter().map(Address::to_json).collect()),
    }
}

/// The four states of a Fastmail masked address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MaskedEmailState {
    /// Hidden in Fastmail's UI and removed after 24 hours unless mail
    /// arrives. The fake never removes pending addresses.
    Pending,
    /// Mail is delivered normally.
    Enabled,
    /// Mail is accepted and filed in Trash.
    Disabled,
    /// Mail bounces.
    Deleted,
}

impl MaskedEmailState {
    /// The state as JMAP writes it, such as `"enabled"`.
    pub fn as_str(self) -> &'static str {
        match self {
            MaskedEmailState::Pending => "pending",
            MaskedEmailState::Enabled => "enabled",
            MaskedEmailState::Disabled => "disabled",
            MaskedEmailState::Deleted => "deleted",
        }
    }

    /// Parses a JMAP state string. Anything but the four states is `None`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(MaskedEmailState::Pending),
            "enabled" => Some(MaskedEmailState::Enabled),
            "disabled" => Some(MaskedEmailState::Disabled),
            "deleted" => Some(MaskedEmailState::Deleted),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Mailbox {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub role: Option<String>,
    pub sort_order: u32,
    pub is_subscribed: bool,
}

/// The mailbox roles RFC 8621 registers that the fake accepts.
pub(crate) const MAILBOX_ROLES: &[&str] = &[
    "all",
    "archive",
    "drafts",
    "flagged",
    "important",
    "inbox",
    "junk",
    "sent",
    "subscribed",
    "trash",
];

/// Where a text body comes from. Synthetic bodies are generated on demand,
/// so a 100k message account does not hold every body in memory.
#[derive(Debug, Clone)]
pub(crate) enum Body {
    Text(String),
    Synthetic { seed: u64, len: u32 },
}

impl Body {
    pub(crate) fn content(&self) -> Cow<'_, str> {
        match self {
            Body::Text(s) => Cow::Borrowed(s),
            Body::Synthetic { seed, len } => {
                Cow::Owned(crate::sim::synthetic_text(*seed, *len as usize))
            }
        }
    }

    pub(crate) fn len(&self) -> usize {
        match self {
            Body::Text(s) => s.len(),
            Body::Synthetic { len, .. } => *len as usize,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Attachment {
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub inline: bool,
    pub cid: Option<String>,
    /// The uploaded blob this attachment was created from, if any. Without
    /// one, downloads return placeholder bytes of the right size.
    pub upload: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Email {
    pub id: String,
    pub thread_id: String,
    pub mailbox_ids: BTreeSet<String>,
    pub keywords: BTreeSet<String>,
    pub received_at: Timestamp,
    pub sent_at: Option<Timestamp>,
    pub size: u64,
    pub message_id: Option<Vec<String>>,
    pub in_reply_to: Option<Vec<String>>,
    pub references: Option<Vec<String>>,
    pub sender: Option<Vec<Address>>,
    pub from: Option<Vec<Address>>,
    pub to: Option<Vec<Address>>,
    pub cc: Option<Vec<Address>>,
    pub bcc: Option<Vec<Address>>,
    pub reply_to: Option<Vec<Address>>,
    pub subject: Option<String>,
    /// Headers beyond the ones derived from the fields above, in order.
    pub extra_headers: Vec<(String, String)>,
    pub text: Option<Body>,
    pub html: Option<String>,
    pub attachments: Vec<Attachment>,
}

/// One leaf of an email's MIME tree.
#[derive(Debug, Clone)]
pub(crate) struct Part {
    pub part_id: String,
    pub blob_id: String,
    pub size: u64,
    pub name: Option<String>,
    pub mime: String,
    pub charset: Option<String>,
    pub disposition: Option<String>,
    pub cid: Option<String>,
    pub kind: PartKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PartKind {
    Text,
    Html,
    Attachment(usize),
}

#[derive(Debug, Clone)]
pub(crate) enum Node {
    Leaf(Part),
    Multi { mime: String, children: Vec<Node> },
}

/// An email's parts as RFC 8621 groups them.
pub(crate) struct Parts {
    pub structure: Node,
    pub text_body: Vec<Part>,
    pub html_body: Vec<Part>,
    pub attachments: Vec<Part>,
}

impl Email {
    pub(crate) fn blob_id(&self) -> String {
        format!("G{}", self.id)
    }

    pub(crate) fn is_unread(&self) -> bool {
        !self.keywords.contains("$seen") && !self.keywords.contains("$draft")
    }

    pub(crate) fn text_content(&self) -> Option<Cow<'_, str>> {
        self.text.as_ref().map(Body::content)
    }

    /// Plain text for previews and search: the text part, or the HTML part
    /// with tags removed.
    pub(crate) fn plain_text(&self) -> String {
        if let Some(t) = self.text_content() {
            return t.into_owned();
        }
        self.html
            .as_deref()
            .map(text::html_to_text)
            .unwrap_or_default()
    }

    pub(crate) fn preview(&self) -> String {
        text::preview(&self.plain_text())
    }

    pub(crate) fn has_attachment(&self) -> bool {
        self.attachments.iter().any(|a| !a.inline)
    }

    /// Every header field, derived ones first, as `(name, unfolded value)`.
    pub(crate) fn headers(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        let mut push_addrs = |name: &str, list: &Option<Vec<Address>>| {
            if let Some(list) = list {
                out.push((name.to_owned(), text::format_address_list(list)));
            }
        };
        push_addrs("From", &self.from);
        push_addrs("Sender", &self.sender);
        push_addrs("Reply-To", &self.reply_to);
        push_addrs("To", &self.to);
        push_addrs("Cc", &self.cc);
        push_addrs("Bcc", &self.bcc);
        if let Some(s) = &self.subject {
            out.push(("Subject".to_owned(), s.clone()));
        }
        out.push((
            "Date".to_owned(),
            text::format_rfc5322_date(self.sent_at.unwrap_or(self.received_at)),
        ));
        let mut push_ids = |name: &str, ids: &Option<Vec<String>>| {
            if let Some(ids) = ids
                && !ids.is_empty()
            {
                out.push((name.to_owned(), text::format_message_ids(ids)));
            }
        };
        push_ids("Message-ID", &self.message_id);
        push_ids("In-Reply-To", &self.in_reply_to);
        push_ids("References", &self.references);
        out.extend(self.extra_headers.iter().cloned());
        out.push(("MIME-Version".to_owned(), "1.0".to_owned()));
        let parts = self.parts();
        out.push(("Content-Type".to_owned(), content_type_of(&parts.structure)));
        out
    }

    pub(crate) fn parts(&self) -> Parts {
        let blob = self.blob_id();
        let leaf = |part_id: String, kind: PartKind, mime: &str, size: u64| Part {
            blob_id: format!("{blob}.{part_id}"),
            part_id,
            size,
            name: None,
            mime: mime.to_owned(),
            charset: Some("utf-8".to_owned()),
            disposition: None,
            cid: None,
            kind,
        };
        let mut next = 1usize;
        let mut take_id = || {
            let id = next.to_string();
            next += 1;
            id
        };
        let text_part = self
            .text
            .as_ref()
            .map(|b| leaf(take_id(), PartKind::Text, "text/plain", b.len() as u64));
        let html_part = self
            .html
            .as_ref()
            .map(|h| leaf(take_id(), PartKind::Html, "text/html", h.len() as u64));
        let body_node = match (&text_part, &html_part) {
            (Some(t), Some(h)) => Node::Multi {
                mime: "multipart/alternative".to_owned(),
                children: vec![Node::Leaf(t.clone()), Node::Leaf(h.clone())],
            },
            (Some(t), None) => Node::Leaf(t.clone()),
            (None, Some(h)) => Node::Leaf(h.clone()),
            (None, None) => Node::Leaf(leaf(take_id(), PartKind::Text, "text/plain", 0)),
        };
        let attachments: Vec<Part> = self
            .attachments
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let part_id = take_id();
                Part {
                    blob_id: match &a.upload {
                        Some(up) => up.clone(),
                        None => format!("{blob}.{part_id}"),
                    },
                    part_id,
                    size: a.size,
                    name: Some(a.name.clone()),
                    mime: a.mime.clone(),
                    charset: None,
                    disposition: Some(if a.inline { "inline" } else { "attachment" }.to_owned()),
                    cid: a.cid.clone(),
                    kind: PartKind::Attachment(i),
                }
            })
            .collect();
        let structure = if attachments.is_empty() {
            body_node.clone()
        } else {
            let mut children = vec![body_node.clone()];
            children.extend(attachments.iter().cloned().map(Node::Leaf));
            Node::Multi {
                mime: "multipart/mixed".to_owned(),
                children,
            }
        };
        let (text_body, html_body) = match (text_part, html_part) {
            (Some(t), Some(h)) => (vec![t], vec![h]),
            (Some(t), None) => (vec![t.clone()], vec![t]),
            (None, Some(h)) => (vec![h.clone()], vec![h]),
            (None, None) => match body_node {
                Node::Leaf(p) => (vec![p.clone()], vec![p]),
                Node::Multi { .. } => (Vec::new(), Vec::new()),
            },
        };
        Parts {
            structure,
            text_body,
            html_body,
            attachments,
        }
    }

    /// The decoded content of a text or HTML part.
    pub(crate) fn part_text(&self, kind: PartKind) -> Option<Cow<'_, str>> {
        match kind {
            PartKind::Text => Some(self.text_content().unwrap_or(Cow::Borrowed(""))),
            PartKind::Html => self.html.as_deref().map(Cow::Borrowed),
            PartKind::Attachment(_) => None,
        }
    }

    /// An approximate RFC 5322 size: headers, bodies and base64 attachments.
    pub(crate) fn compute_size(&self) -> u64 {
        let headers: usize = self
            .headers()
            .iter()
            .map(|(n, v)| n.len() + v.len() + 4)
            .sum();
        let bodies =
            self.text.as_ref().map_or(0, Body::len) + self.html.as_ref().map_or(0, String::len);
        let attachments: u64 = self.attachments.iter().map(|a| a.size * 4 / 3 + 200).sum();
        headers as u64 + bodies as u64 + attachments + 120
    }

    /// The message as RFC 5322 text, for blob downloads.
    pub(crate) fn to_rfc5322(&self) -> String {
        let mut out = String::new();
        let parts = self.parts();
        for (name, value) in self.headers() {
            if name.eq_ignore_ascii_case("Content-Type")
                || name.eq_ignore_ascii_case("MIME-Version")
            {
                continue;
            }
            out.push_str(&format!("{name}: {value}\r\n"));
        }
        out.push_str("MIME-Version: 1.0\r\n");
        let mut boundary_n = 0usize;
        write_node(self, &parts.structure, &mut out, &mut boundary_n);
        out
    }
}

fn content_type_of(node: &Node) -> String {
    match node {
        Node::Leaf(p) => match &p.charset {
            Some(cs) => format!("{}; charset={cs}", p.mime),
            None => p.mime.clone(),
        },
        Node::Multi { mime, .. } => format!("{mime}; boundary=\"mejl-fake-0\""),
    }
}

fn write_node(email: &Email, node: &Node, out: &mut String, boundary_n: &mut usize) {
    match node {
        Node::Leaf(p) => {
            match &p.charset {
                Some(cs) => out.push_str(&format!("Content-Type: {}; charset={cs}\r\n", p.mime)),
                None => out.push_str(&format!("Content-Type: {}\r\n", p.mime)),
            }
            if let Some(d) = &p.disposition {
                let name = p.name.as_deref().unwrap_or("file");
                out.push_str(&format!(
                    "Content-Disposition: {d}; filename=\"{name}\"\r\n"
                ));
            }
            out.push_str("\r\n");
            match email.part_text(p.kind) {
                Some(t) => out.push_str(&t.replace('\n', "\r\n")),
                None => out.push_str("(attachment content is not stored by the fake server)"),
            }
            out.push_str("\r\n");
        }
        Node::Multi { mime, children } => {
            let boundary = format!("mejl-fake-{boundary_n}");
            *boundary_n += 1;
            out.push_str(&format!(
                "Content-Type: {mime}; boundary=\"{boundary}\"\r\n\r\n"
            ));
            for child in children {
                out.push_str(&format!("--{boundary}\r\n"));
                write_node(email, child, out, boundary_n);
            }
            out.push_str(&format!("--{boundary}--\r\n"));
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Thread {
    pub id: String,
    /// Sorted by `receivedAt`, oldest first, as RFC 8621 requires.
    pub email_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Identity {
    pub id: String,
    pub name: String,
    pub email: String,
    pub reply_to: Option<Vec<Address>>,
    pub bcc: Option<Vec<Address>>,
    pub text_signature: String,
    pub html_signature: String,
    pub may_delete: bool,
}

impl Identity {
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "email": self.email,
            "replyTo": addresses_json(&self.reply_to),
            "bcc": addresses_json(&self.bcc),
            "textSignature": self.text_signature,
            "htmlSignature": self.html_signature,
            "mayDelete": self.may_delete,
        })
    }

    /// True when this identity may send as `address`. A `*` local part
    /// covers the whole domain.
    pub(crate) fn allows(&self, address: &str) -> bool {
        let address = address.to_ascii_lowercase();
        let own = self.email.to_ascii_lowercase();
        match own.strip_prefix("*@") {
            Some(domain) => address.rsplit_once('@').is_some_and(|(_, d)| d == domain),
            None => own == address,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MaskedEmail {
    pub id: String,
    pub email: String,
    pub state: MaskedEmailState,
    pub for_domain: String,
    pub description: String,
    pub url: Option<String>,
    pub created_by: String,
    pub created_at: Timestamp,
    pub last_message_at: Option<Timestamp>,
}

impl MaskedEmail {
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "email": self.email,
            "state": self.state.as_str(),
            "forDomain": self.for_domain,
            "description": self.description,
            "url": self.url,
            "createdBy": self.created_by,
            "createdAt": text::utc_date(self.created_at),
            "lastMessageAt": self.last_message_at.map(text::utc_date),
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ContactCard {
    pub id: String,
    pub uid: String,
    pub given: String,
    pub surname: String,
    pub emails: Vec<(String, Option<&'static str>)>,
    pub created: Timestamp,
}

pub(crate) const ADDRESS_BOOK_ID: &str = "B0default";

impl ContactCard {
    pub(crate) fn full_name(&self) -> String {
        match (self.given.is_empty(), self.surname.is_empty()) {
            (false, false) => format!("{} {}", self.given, self.surname),
            (false, true) => self.given.clone(),
            (true, false) => self.surname.clone(),
            (true, true) => String::new(),
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        let mut components = Vec::new();
        if !self.given.is_empty() {
            components
                .push(json!({"@type": "NameComponent", "kind": "given", "value": self.given}));
        }
        if !self.surname.is_empty() {
            components
                .push(json!({"@type": "NameComponent", "kind": "surname", "value": self.surname}));
        }
        let mut emails = Map::new();
        for (i, (address, context)) in self.emails.iter().enumerate() {
            let mut e = json!({"@type": "EmailAddress", "address": address});
            if let (Some(ctx), Some(obj)) = (context, e.as_object_mut()) {
                obj.insert("contexts".to_owned(), json!({ *ctx: true }));
            }
            emails.insert(format!("e{}", i + 1), e);
        }
        json!({
            "@type": "Card",
            "version": "1.0",
            "id": self.id,
            "uid": self.uid,
            "addressBookIds": { ADDRESS_BOOK_ID: true },
            "kind": "individual",
            "created": text::utc_date(self.created),
            "updated": text::utc_date(self.created),
            "name": {
                "@type": "Name",
                "full": self.full_name(),
                "components": components,
            },
            "emails": emails,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Submission {
    pub id: String,
    pub identity_id: String,
    pub email_id: String,
    pub thread_id: String,
    pub envelope: Value,
    pub send_at: Timestamp,
}

impl Submission {
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "identityId": self.identity_id,
            "emailId": self.email_id,
            "threadId": self.thread_id,
            "envelope": self.envelope,
            "sendAt": text::utc_date(self.send_at),
            "undoStatus": "final",
            "deliveryStatus": null,
            "dsnBlobIds": [],
            "mdnBlobIds": [],
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Upload {
    pub mime: String,
    pub data: bytes::Bytes,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email() -> Email {
        Email {
            id: "E1".into(),
            thread_id: "T1".into(),
            mailbox_ids: BTreeSet::new(),
            keywords: BTreeSet::new(),
            received_at: "2026-10-23T09:12:00Z".parse().unwrap(),
            sent_at: None,
            size: 0,
            message_id: Some(vec!["a@example.org".into()]),
            in_reply_to: None,
            references: None,
            sender: None,
            from: Some(vec![Address::new("A", "a@example.org")]),
            to: Some(vec![Address::bare("b@example.org")]),
            cc: None,
            bcc: None,
            reply_to: None,
            subject: Some("Hi".into()),
            extra_headers: vec![("X-Test".into(), "yes".into())],
            text: Some(Body::Text("hello\nworld".into())),
            html: Some("<p>hello</p>".into()),
            attachments: vec![Attachment {
                name: "a.pdf".into(),
                mime: "application/pdf".into(),
                size: 1000,
                inline: false,
                cid: None,
                upload: None,
            }],
        }
    }

    #[test]
    fn parts_have_rfc_grouping() {
        let e = email();
        let p = e.parts();
        assert_eq!(p.text_body.len(), 1);
        assert_eq!(p.text_body[0].mime, "text/plain");
        assert_eq!(p.html_body[0].mime, "text/html");
        assert_eq!(p.attachments.len(), 1);
        assert_eq!(p.attachments[0].part_id, "3");
        match &p.structure {
            Node::Multi { mime, children } => {
                assert_eq!(mime, "multipart/mixed");
                assert_eq!(children.len(), 2);
            }
            Node::Leaf(_) => panic!("expected multipart"),
        }
        assert!(e.has_attachment());
    }

    #[test]
    fn html_only_email_uses_html_for_both_lists() {
        let mut e = email();
        e.text = None;
        e.attachments.clear();
        let p = e.parts();
        assert_eq!(p.text_body[0].mime, "text/html");
        assert_eq!(p.html_body[0].mime, "text/html");
        assert_eq!(e.plain_text().trim(), "hello");
    }

    #[test]
    fn raw_message_contains_headers_and_parts() {
        let raw = email().to_rfc5322();
        assert!(raw.contains("From: A <a@example.org>\r\n"));
        assert!(raw.contains("X-Test: yes\r\n"));
        assert!(raw.contains("multipart/alternative"));
        assert!(raw.contains("hello\r\nworld"));
        assert!(email().compute_size() > 1000);
    }

    #[test]
    fn identity_wildcards() {
        let id = Identity {
            id: "I1".into(),
            name: "x".into(),
            email: "*@Example.org".into(),
            reply_to: None,
            bcc: None,
            text_signature: String::new(),
            html_signature: String::new(),
            may_delete: false,
        };
        assert!(id.allows("anyone@example.org"));
        assert!(!id.allows("anyone@example.com"));
        assert!(!id.allows("no-at-sign"));
    }

    #[test]
    fn address_json_rejects_junk() {
        assert!(Address::from_json(&json!({"email": "a@b"})).is_some());
        assert!(Address::from_json(&json!({"email": "a@b", "name": null})).is_some());
        assert!(Address::from_json(&json!({"email": 1})).is_none());
        assert!(Address::from_json(&json!({"email": "a@b", "x": 1})).is_none());
        assert!(Address::from_json(&json!("a@b")).is_none());
    }
}
