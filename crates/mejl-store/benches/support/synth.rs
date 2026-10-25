//! Deterministic synthetic mail for benchmarks and the ignored perf test.
//!
//! Rows go in through the store's own writer API, in batches of 5000, so
//! the derived tables are exactly what real sync would produce.

#![allow(dead_code)]

use std::path::Path;

use jiff::Timestamp;
use mejl_jmap::method::GetResponse;
use mejl_jmap::types::{Email, Mailbox};
use mejl_store::{Store, StoreOptions};
use serde_json::{Value, json};

pub const INBOX: &str = "mb-inbox";
pub const LABEL: &str = "mb-receipts";

/// A tiny xorshift64* generator. Fixed seeds give the same store every run.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    pub fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[usize::try_from(self.below(items.len() as u64)).unwrap_or(0)]
    }
}

const FIRST: &[&str] = &[
    "Juniper", "Owen", "Lena", "Priya", "Marco", "Dana", "Sam", "Ravi", "Grace", "Felix", "Noor",
    "Theo", "Ines", "Bea", "Pia", "Hugo", "Mira", "Zoë", "Åsa", "Kenji",
];
const LAST: &[&str] = &[
    "Tran",
    "Castillo",
    "Okafor",
    "Raman",
    "Bellini",
    "Whitfield",
    "Ito",
    "Menon",
    "Lindqvist",
    "Moreau",
    "Haddad",
    "Park",
    "Duarte",
    "Kowalski",
    "Sorensen",
    "Brandt",
    "Holt",
    "Ångström",
];
const DOMAINS: &[&str] = &["example.org", "example.net", "example.com"];
const WORDS: &[&str] = &[
    "garden",
    "rota",
    "budget",
    "hall",
    "repairs",
    "order",
    "shipped",
    "standup",
    "notes",
    "reservation",
    "library",
    "trip",
    "coast",
    "invoice",
    "statement",
    "photos",
    "climbing",
    "board",
    "game",
    "night",
    "renewal",
    "newsletter",
    "draft",
    "appointment",
    "reminder",
    "book",
    "club",
    "report",
    "signups",
    "seed",
    "list",
    "spare",
    "room",
    "receipt",
    "slides",
    "meetup",
    "membership",
    "translation",
    "review",
    "calendar",
    "running",
    "route",
    "policy",
    "documents",
    "agenda",
    "choir",
    "rehearsal",
    "deposit",
    "café",
    "plot",
    "compost",
    "kettle",
];

fn address(rng: &mut Rng) -> Value {
    let first = rng.pick(FIRST);
    let last = rng.pick(LAST);
    let domain = rng.pick(DOMAINS);
    json!({
        "name": format!("{first} {last}"),
        "email": format!("{}.{}@{domain}", first.to_lowercase(), last.to_lowercase()),
    })
}

fn subject(rng: &mut Rng) -> String {
    let n = 3 + rng.below(5);
    let words: Vec<&str> = (0..n).map(|_| *rng.pick(WORDS)).collect();
    let mut s = words.join(" ");
    if let Some(first) = s.get(..1) {
        s = first.to_uppercase() + &s[1..];
    }
    s
}

fn preview(rng: &mut Rng) -> String {
    let n = 10 + rng.below(14);
    (0..n)
        .map(|_| *rng.pick(WORDS))
        .collect::<Vec<_>>()
        .join(" ")
}

fn mailboxes() -> GetResponse<Mailbox> {
    let list: Vec<Value> = [
        (INBOX, "Inbox", Some("inbox")),
        ("mb-archive", "Archive", Some("archive")),
        ("mb-sent", "Sent", Some("sent")),
        ("mb-trash", "Trash", Some("trash")),
        ("mb-junk", "Spam", Some("junk")),
        (LABEL, "Receipts", None),
    ]
    .into_iter()
    .map(|(id, name, role)| json!({"id": id, "name": name, "role": role, "totalEmails": 0}))
    .collect();
    serde_json::from_value(json!({"accountId": "A1", "state": "1", "list": list, "notFound": []}))
        .expect("mailboxes")
}

/// Generated emails, oldest first, with thread ids. Most threads have one
/// to three messages; one in fifty has six.
pub fn emails(n: usize, seed: u64, now: Timestamp) -> Vec<Email> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::with_capacity(n);
    let start = now.as_second() - i64::try_from(n).unwrap_or(0) * 60;
    let mut thread = 0usize;
    while out.len() < n {
        thread += 1;
        let size = match rng.below(50) {
            0 => 6,
            1..=9 => 3,
            10..=24 => 2,
            _ => 1,
        };
        let base_subject = subject(&mut rng);
        let masked = rng
            .chance(5)
            .then(|| format!("mask{}@example.org", rng.below(50)));
        for i in 0..size {
            if out.len() >= n {
                break;
            }
            let k = out.len();
            let at =
                Timestamp::from_second(start + i64::try_from(k).unwrap_or(0) * 60).unwrap_or(now);
            let mut mailboxes = serde_json::Map::new();
            mailboxes.insert(INBOX.into(), json!(true));
            if rng.chance(10) {
                mailboxes.insert(LABEL.into(), json!(true));
            }
            let mut keywords = serde_json::Map::new();
            if rng.chance(70) {
                keywords.insert("$seen".into(), json!(true));
            }
            if rng.chance(3) {
                keywords.insert("$flagged".into(), json!(true));
            }
            let subject = if i == 0 {
                base_subject.clone()
            } else {
                format!("Re: {base_subject}")
            };
            let value = json!({
                "id": format!("M{k:06}"),
                "threadId": format!("T{thread:06}"),
                "mailboxIds": mailboxes,
                "keywords": keywords,
                "receivedAt": at.to_string(),
                "size": 2000 + rng.below(200_000),
                "from": [address(&mut rng)],
                "to": [address(&mut rng)],
                "cc": [],
                "subject": subject,
                "preview": preview(&mut rng),
                "hasAttachment": rng.chance(10),
                "header:X-FM-Masked-Email:asText": masked,
            });
            out.push(serde_json::from_value(value).expect("email"));
        }
    }
    out
}

/// Builds a store with `n` messages in one Inbox.
pub fn build(dir: &Path, n: usize, now: Timestamp) -> Store {
    let mut store = Store::open(dir, StoreOptions::default()).expect("open");
    let mut batch = store.batch().expect("batch");
    batch.replace_mailboxes(&mailboxes()).expect("mailboxes");
    batch.commit().expect("commit");
    let all = emails(n, 0x6d65_6a6c, now);
    for chunk in all.chunks(5000) {
        let mut batch = store.batch().expect("batch");
        batch.upsert_emails(chunk).expect("upsert");
        batch.commit().expect("commit");
    }
    store
}

/// A flags-only `Email/get` result for `count` emails spread over the
/// store, flipping `$seen` on each call with a different `round`.
pub fn flag_batch(n: usize, count: usize, round: u64) -> Vec<Email> {
    let mut rng = Rng::new(round + 7);
    (0..count)
        .map(|_| {
            let k = rng.below(n as u64);
            let mut keywords = serde_json::Map::new();
            if round.is_multiple_of(2) {
                keywords.insert("$seen".into(), json!(true));
            }
            serde_json::from_value(json!({
                "id": format!("M{k:06}"),
                "mailboxIds": {INBOX: true},
                "keywords": keywords,
            }))
            .expect("flags")
        })
        .collect()
}

/// The id of a thread with six messages, for conversation detail.
pub fn long_thread(n: usize, now: Timestamp) -> String {
    let all = emails(n.min(20_000), 0x6d65_6a6c, now);
    let mut counts = std::collections::HashMap::<String, usize>::new();
    for e in &all {
        if let Some(t) = &e.thread_id {
            *counts.entry(t.as_str().to_owned()).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .filter(|(_, c)| *c == 6)
        .map(|(t, _)| t)
        .min()
        .expect("a six-message thread")
}

pub fn now() -> Timestamp {
    "2026-10-23T14:27:00Z".parse().expect("now")
}
