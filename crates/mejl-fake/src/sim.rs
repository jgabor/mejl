//! Simulation helpers: incoming mail, changes made by "another client", and
//! the deterministic synthetic dataset for benchmarks.

use std::collections::{BTreeSet, VecDeque};

use jiff::{SignedDuration, Timestamp};

use crate::ids::Rng;
use crate::model::{Address, Attachment, Body, MaskedEmail, MaskedEmailState};
use crate::store::{NewEmail, Store};

/// A message arriving from outside, for
/// [`FakeServer::deliver_incoming`](crate::FakeServer::deliver_incoming).
///
/// Built with [`IncomingMessage::new`] and the chained setters. When `to` is
/// empty the message is addressed to the account's own address.
#[derive(Debug, Clone)]
pub struct IncomingMessage {
    /// The sender.
    pub from: Address,
    /// The To header. Empty means the account's own address.
    pub to: Vec<Address>,
    /// The Cc header.
    pub cc: Vec<Address>,
    /// Extra envelope recipients that are not in the headers, as for Bcc or
    /// mailing list delivery.
    pub envelope_to: Vec<String>,
    /// The Subject header.
    pub subject: String,
    /// The text/plain body, if any.
    pub text: Option<String>,
    /// The text/html body, if any.
    pub html: Option<String>,
    /// Defaults to the server clock.
    pub received_at: Option<Timestamp>,
    /// A role (`inbox`), mailbox id, name or path. Defaults to the inbox.
    pub mailbox: Option<String>,
    /// Keywords such as `$seen` or `$flagged`. Invalid ones are dropped.
    pub keywords: Vec<String>,
    /// The id of an email in the account this message replies to. The reply
    /// gets matching `In-Reply-To` and `References`, so it joins the thread.
    pub in_reply_to: Option<String>,
    /// Attachment metadata.
    pub attachments: Vec<AttachmentSpec>,
    /// Extra header fields, such as `List-Id`.
    pub headers: Vec<(String, String)>,
}

/// Attachment metadata. The fake stores no content; downloads return
/// placeholder bytes of the given size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentSpec {
    /// The file name.
    pub name: String,
    /// The media type, such as `application/pdf`.
    pub mime: String,
    /// The size in bytes.
    pub size: u64,
}

impl AttachmentSpec {
    /// Attachment metadata with a name, media type and size in bytes.
    pub fn new(name: impl Into<String>, mime: impl Into<String>, size: u64) -> Self {
        AttachmentSpec {
            name: name.into(),
            mime: mime.into(),
            size,
        }
    }
}

impl IncomingMessage {
    /// A message from `from` with the given subject and an empty body.
    pub fn new(from: Address, subject: impl Into<String>) -> Self {
        IncomingMessage {
            from,
            to: Vec::new(),
            cc: Vec::new(),
            envelope_to: Vec::new(),
            subject: subject.into(),
            text: None,
            html: None,
            received_at: None,
            mailbox: None,
            keywords: Vec::new(),
            in_reply_to: None,
            attachments: Vec::new(),
            headers: Vec::new(),
        }
    }

    /// Adds a To recipient.
    pub fn to(mut self, a: Address) -> Self {
        self.to.push(a);
        self
    }

    /// Adds a Cc recipient.
    pub fn cc(mut self, a: Address) -> Self {
        self.cc.push(a);
        self
    }

    /// Adds an envelope recipient that does not appear in the headers.
    pub fn envelope_to(mut self, address: impl Into<String>) -> Self {
        self.envelope_to.push(address.into());
        self
    }

    /// Sets the text/plain body.
    pub fn text(mut self, body: impl Into<String>) -> Self {
        self.text = Some(body.into());
        self
    }

    /// Sets the text/html body.
    pub fn html(mut self, body: impl Into<String>) -> Self {
        self.html = Some(body.into());
        self
    }

    /// Sets `receivedAt` instead of using the server clock.
    pub fn received_at(mut self, t: Timestamp) -> Self {
        self.received_at = Some(t);
        self
    }

    /// Files the message in a mailbox given by role, id, name or path.
    pub fn mailbox(mut self, mailbox: impl Into<String>) -> Self {
        self.mailbox = Some(mailbox.into());
        self
    }

    /// Adds a keyword.
    pub fn keyword(mut self, k: impl Into<String>) -> Self {
        self.keywords.push(k.into());
        self
    }

    /// Makes the message a reply to an email already in the account.
    pub fn in_reply_to(mut self, email_id: impl Into<String>) -> Self {
        self.in_reply_to = Some(email_id.into());
        self
    }

    /// Adds attachment metadata.
    pub fn attachment(mut self, a: AttachmentSpec) -> Self {
        self.attachments.push(a);
        self
    }

    /// Adds a header field.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

/// A change made by some other client, for
/// [`FakeServer::mutate_from_elsewhere`](crate::FakeServer::mutate_from_elsewhere).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalChange {
    /// Sets or clears a keyword, for example `$seen` or `$flagged`.
    SetKeyword {
        /// The email to change.
        email_id: String,
        /// The keyword, in any case.
        keyword: String,
        /// True to set it, false to clear it.
        value: bool,
    },
    /// Replaces all of the email's mailboxes with one.
    MoveToMailbox {
        /// The email to move.
        email_id: String,
        /// The only mailbox it will be in.
        mailbox_id: String,
    },
    /// Adds the email to one more mailbox, like applying a label.
    AddToMailbox {
        /// The email to change.
        email_id: String,
        /// The mailbox to add.
        mailbox_id: String,
    },
    /// Removes the email from one mailbox. Fails if it is the last one.
    RemoveFromMailbox {
        /// The email to change.
        email_id: String,
        /// The mailbox to remove.
        mailbox_id: String,
    },
    /// Destroys the email for good.
    DestroyEmail {
        /// The email to destroy.
        email_id: String,
    },
    /// Takes the role off a mailbox, as an admin or a migration might, so
    /// the account has no mailbox with that role. Clients see a mailbox
    /// change.
    ClearMailboxRole {
        /// The mailbox.
        mailbox_id: String,
    },
    /// Renames a mailbox.
    RenameMailbox {
        /// The mailbox to rename.
        mailbox_id: String,
        /// The new name.
        name: String,
    },
    /// Changes a masked address's state, as the web UI or a password
    /// manager would.
    SetMaskedEmailState {
        /// The `MaskedEmail` id.
        id: String,
        /// The new state. Going back to pending is refused.
        state: MaskedEmailState,
    },
}

const WORDS: &[&str] = &[
    "the",
    "garden",
    "meeting",
    "plan",
    "budget",
    "notes",
    "update",
    "schedule",
    "next",
    "week",
    "please",
    "review",
    "draft",
    "thanks",
    "quick",
    "question",
    "about",
    "project",
    "invoice",
    "order",
    "delivery",
    "team",
    "report",
    "summary",
    "agenda",
    "call",
    "tomorrow",
    "morning",
    "afternoon",
    "list",
    "photos",
    "trip",
    "booking",
    "confirm",
    "details",
    "changes",
    "share",
    "document",
    "library",
    "club",
    "hall",
    "repairs",
    "roof",
    "window",
    "kitchen",
    "train",
    "ticket",
    "weekend",
    "dinner",
    "recipe",
    "coffee",
    "office",
    "paint",
    "seeds",
    "rota",
    "shift",
    "volunteer",
    "event",
    "flyer",
    "printing",
    "deadline",
    "monday",
    "friday",
    "and",
    "with",
    "for",
    "from",
    "will",
    "can",
    "we",
    "you",
    "this",
    "that",
    "our",
    "some",
    "new",
];

const FIRST: &[&str] = &[
    "Ada", "Bram", "Cleo", "Dev", "Esme", "Finn", "Gita", "Hal", "Ines", "Jory", "Kai", "Lior",
    "Maeve", "Nils", "Oona", "Pax", "Quinn", "Rosa", "Silas", "Tova", "Uri", "Vera", "Wynn", "Xan",
    "Yara", "Zeb",
];

const LAST: &[&str] = &[
    "Abara", "Birch", "Calloway", "Dunmore", "Ellery", "Fairley", "Gorse", "Hollis", "Imrie",
    "Jessup", "Kestler", "Lindell", "Moray", "Norcott", "Orwin", "Pell", "Quayle", "Rook",
    "Selwyn", "Thorne",
];

const DOMAINS: &[&str] = &[
    "example.com",
    "example.org",
    "example.net",
    "mail.example.com",
    "post.example.net",
];

/// Deterministic filler text of exactly `len` bytes, ASCII only.
pub(crate) fn synthetic_text(seed: u64, len: usize) -> String {
    let mut rng = Rng::new(seed);
    let mut out = String::with_capacity(len + 64);
    while out.len() < len {
        let sentences = rng.between(2, 5);
        for _ in 0..sentences {
            let words = rng.between(6, 16);
            for w in 0..words {
                let word = rng.pick(WORDS);
                if w == 0 {
                    let mut cs = word.chars();
                    if let Some(first) = cs.next() {
                        out.push(first.to_ascii_uppercase());
                        out.push_str(cs.as_str());
                    }
                } else {
                    out.push(' ');
                    out.push_str(word);
                }
            }
            out.push_str(". ");
        }
        out.push_str("\n\n");
    }
    out.truncate(len);
    out
}

fn subject(rng: &mut Rng) -> String {
    let target = rng.between(20, 80) as usize;
    let mut s = String::new();
    while s.len() < target {
        if !s.is_empty() {
            s.push(' ');
        }
        s.push_str(rng.pick(WORDS));
    }
    let mut cs = s.chars();
    match cs.next() {
        Some(f) => format!("{}{}", f.to_ascii_uppercase(), cs.as_str()),
        None => s,
    }
}

fn person(rng: &mut Rng) -> Address {
    let first = rng.pick(FIRST);
    let last = rng.pick(LAST);
    let domain = rng.pick(DOMAINS);
    Address::new(
        format!("{first} {last}"),
        format!(
            "{}.{}@{domain}",
            first.to_ascii_lowercase(),
            last.to_ascii_lowercase()
        ),
    )
}

struct RecentThread {
    message_ids: Vec<String>,
    subject: String,
    size: u32,
}

/// Fills `store` with `n` messages spread over the two years before `now`.
///
/// The spread is fixed by `seed`: mailboxes (mostly Archive and Inbox),
/// threads of up to twelve messages, subjects of 20 to 80 characters, bodies
/// of 200 bytes to 20 kB generated on demand, a few attachments and masked
/// addresses.
pub(crate) fn fill_synthetic(store: &mut Store, n: usize, seed: u64, now: Timestamp) {
    let mut rng = Rng::new(seed);
    store.begin_bulk();
    let projects = store.create_mailbox("Projects", None, None, 20);
    let folders = [
        ("inbox", store.role_id("inbox")),
        ("archive", store.role_id("archive")),
        ("sent", store.role_id("sent")),
        ("junk", store.role_id("junk")),
        ("trash", store.role_id("trash")),
        ("drafts", store.role_id("drafts")),
        (
            "newsletters",
            Some(store.create_mailbox("Newsletters", None, None, 10)),
        ),
        (
            "receipts",
            Some(store.create_mailbox("Receipts", None, None, 11)),
        ),
        (
            "travel",
            Some(store.create_mailbox("Travel", None, None, 12)),
        ),
        (
            "work",
            Some(store.create_mailbox("Work", Some(projects.clone()), None, 1)),
        ),
        (
            "home",
            Some(store.create_mailbox("Home", Some(projects), None, 2)),
        ),
    ];
    // Weights in percent, matching the order above.
    let weights = [24u64, 38, 7, 3, 3, 1, 10, 6, 3, 3, 2];
    let me = Address::new(store.display_name.clone(), store.username.clone());
    let masks: Vec<String> = (0..30)
        .map(|i| {
            let id = format!("masked-s{i}");
            let email = format!(
                "{}.{}{:04}@{}",
                rng.pick(WORDS),
                rng.pick(WORDS),
                i,
                store.masked_domain
            );
            let state = match i % 10 {
                0 => MaskedEmailState::Disabled,
                1 => MaskedEmailState::Deleted,
                _ => MaskedEmailState::Enabled,
            };
            store.masked.insert(
                id.clone(),
                MaskedEmail {
                    id,
                    email: email.clone(),
                    state,
                    for_domain: format!("https://site{i}.example.com"),
                    description: format!("Site {i}"),
                    url: None,
                    created_by: "web".to_owned(),
                    created_at: now
                        .saturating_sub(SignedDuration::from_hours(24 * 800))
                        .unwrap_or(now),
                    last_message_at: None,
                },
            );
            email
        })
        .collect();
    let span = SignedDuration::from_hours(24 * 730).as_secs();
    let step = (span / n.max(1) as i64).max(1);
    let mut recent: VecDeque<RecentThread> = VecDeque::new();
    for i in 0..n {
        let offset = span - step * i as i64 - rng.below(step as u64) as i64;
        let received_at = now
            .checked_sub(SignedDuration::from_secs(offset.max(1)))
            .unwrap_or(now);
        let mut roll = rng.below(100);
        let mut folder = folders[1].clone();
        for (f, w) in folders.iter().zip(weights) {
            if roll < w {
                folder = f.clone();
                break;
            }
            roll -= w;
        }
        let (kind, mailbox) = folder;
        let Some(mailbox) = mailbox else { continue };
        let mut new = NewEmail {
            received_at: Some(received_at),
            sent_at: Some(
                received_at
                    .checked_sub(SignedDuration::from_secs(rng.between(5, 600) as i64))
                    .unwrap_or(received_at),
            ),
            ..NewEmail::default()
        };
        new.mailbox_ids.insert(mailbox);
        let msgid = format!("syn{i}.{seed:x}@example.net");
        new.message_id = Some(msgid.clone());
        let reply_to = if !recent.is_empty() && rng.percent(30) {
            let idx = rng.below(recent.len() as u64) as usize;
            recent.get_mut(idx).filter(|t| t.size < 12)
        } else {
            None
        };
        let subj = match reply_to {
            Some(t) => {
                new.in_reply_to = t.message_ids.last().map(|m| vec![m.clone()]);
                new.references = Some(t.message_ids.iter().rev().take(10).rev().cloned().collect());
                t.message_ids.push(msgid.clone());
                t.size += 1;
                format!("Re: {}", t.subject)
            }
            None => {
                let s = subject(&mut rng);
                recent.push_back(RecentThread {
                    message_ids: vec![msgid.clone()],
                    subject: s.clone(),
                    size: 1,
                });
                if recent.len() > 64 {
                    recent.pop_front();
                }
                s
            }
        };
        new.subject = Some(subj);
        let other = person(&mut rng);
        if kind == "sent" || kind == "drafts" {
            new.from = Some(vec![me.clone()]);
            new.to = Some(vec![other]);
        } else {
            new.from = Some(vec![other]);
            let to = if rng.percent(6) {
                let mask = rng.below(masks.len() as u64) as usize;
                let address = masks
                    .get(mask)
                    .cloned()
                    .unwrap_or_else(|| store.username.clone());
                new.extra_headers.push((
                    crate::store::MASKED_EMAIL_HEADER.to_owned(),
                    address.clone(),
                ));
                Address::bare(address)
            } else {
                me.clone()
            };
            new.to = Some(vec![to]);
        }
        let unread = match kind {
            "inbox" => rng.percent(15),
            "sent" | "drafts" => false,
            _ => rng.percent(2),
        };
        if !unread {
            new.keywords.insert("$seen".to_owned());
        }
        if kind == "drafts" {
            new.keywords.insert("$draft".to_owned());
        }
        if rng.percent(3) {
            new.keywords.insert("$flagged".to_owned());
        }
        let len = if rng.percent(5) {
            rng.between(5_000, 20_000)
        } else {
            rng.between(200, 4_000)
        };
        new.text = Some(Body::Synthetic {
            seed: seed ^ (i as u64).wrapping_mul(0x9E37_79B9),
            len: len as u32,
        });
        if rng.percent(8) {
            new.attachments.push(Attachment {
                name: format!("file-{i}.pdf"),
                mime: "application/pdf".to_owned(),
                size: rng.between(10_000, 2_000_000),
                inline: false,
                cid: None,
                upload: None,
            });
        }
        store.insert_email(new);
    }
    store.end_bulk();
}

/// Normalised keywords from user input; invalid ones are dropped.
pub(crate) fn keywords(list: &[String]) -> BTreeSet<String> {
    list.iter()
        .filter_map(|k| crate::api::normalize_keyword(k))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_text_is_exact_and_stable() {
        for len in [0, 1, 17, 500, 4000] {
            let t = synthetic_text(42, len);
            assert_eq!(t.len(), len);
            assert_eq!(t, synthetic_text(42, len));
            assert!(t.is_ascii());
        }
        assert_ne!(synthetic_text(1, 200), synthetic_text(2, 200));
    }

    #[test]
    fn subjects_have_realistic_length() {
        let mut rng = Rng::new(3);
        for _ in 0..100 {
            let s = subject(&mut rng);
            assert!((20..=100).contains(&s.len()), "{s}");
        }
    }
}
