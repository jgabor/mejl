//! Local search operators against a small store.

mod common;

use common::*;
use jiff::tz::TimeZone;
use mejl_store::{IssueKind, SearchOptions};

fn fixture() -> Fixture {
    let mut f = Fixture::new().with_mailboxes();
    f.apply(&[
        email("rota")
            .subject("Re: Garden club plot rota")
            .from("Juniper Tran", "juniper@example.org")
            .preview("Swapping Saturday works for me")
            .mailboxes(&[INBOX, GARDEN])
            .at("2026-10-23T12:02:00Z")
            .build(),
        email("budget")
            .subject("Draft budget for the hall repairs")
            .from("Owen Castillo", "owen@example.net")
            .preview("Attached the spreadsheet. The roof line is the big one")
            .keywords(&["$flagged"])
            .attachment()
            .size(250_000)
            .at("2026-10-23T11:47:00Z")
            .build(),
        email("shipped")
            .subject("Re: Your order 48213 has shipped")
            .from("Larkspur Support", "help@shop.example.com")
            .to("Mira Holt", "kettle.fern4821@example.org")
            .preview("Good news: we changed the delivery address")
            .seen()
            .at("2026-10-22T09:12:00Z")
            .build(),
        email("seeds")
            .subject("Seed order list")
            .from("Juniper Tran", "juniper@example.org")
            .preview("Final list attached")
            .mailboxes(&[ARCHIVE])
            .seen()
            .at("2026-10-12T08:00:00Z")
            .build(),
        email("junk")
            .subject("Garden gnomes on sale")
            .from("Spammy", "deals@example.com")
            .mailboxes(&[SPAM])
            .at("2026-10-23T13:00:00Z")
            .build(),
        email("binned")
            .subject("Old garden notes")
            .mailboxes(&[TRASH])
            .at("2026-10-01T13:00:00Z")
            .build(),
    ]);
    f
}

fn ids(f: &Fixture, q: &str) -> Vec<String> {
    ids_with(f, q, &SearchOptions::default())
}

fn ids_with(f: &Fixture, q: &str, opts: &SearchOptions) -> Vec<String> {
    let mut out: Vec<String> = f
        .reader()
        .search(q, opts)
        .unwrap()
        .hits
        .into_iter()
        .map(|h| h.row.email_id.into_inner())
        .collect();
    out.sort();
    out
}

#[test]
fn words_and_prefixes() {
    let f = fixture();
    assert_eq!(ids(&f, "garden"), vec!["rota"]);
    assert_eq!(ids(&f, "gard"), vec!["rota"], "the last word is a prefix");
    assert!(
        ids(&f, "gard ").is_empty(),
        "a trailing space ends the prefix"
    );
    assert_eq!(ids(&f, "order"), vec!["seeds", "shipped"]);
    assert_eq!(ids(&f, "\"order list\""), vec!["seeds"]);
    assert!(ids(&f, "\"list order\"").is_empty());
    assert!(ids(&f, "").is_empty());
    assert!(ids(&f, "   ").is_empty());
}

#[test]
fn trash_and_spam_are_left_out_by_default() {
    let f = fixture();
    let all = SearchOptions {
        include_trash_and_spam: true,
        ..Default::default()
    };
    assert_eq!(ids_with(&f, "garden", &all), vec!["binned", "junk", "rota"]);
    assert_eq!(ids(&f, "garden in:spam"), vec!["junk"]);
    assert_eq!(ids(&f, "in:trash"), vec!["binned"]);
}

#[test]
fn column_operators() {
    let f = fixture();
    assert_eq!(ids(&f, "from:juniper"), vec!["rota", "seeds"]);
    assert_eq!(ids(&f, "from:jun"), vec!["rota", "seeds"]);
    assert_eq!(ids(&f, "from:juniper@example.org"), vec!["rota", "seeds"]);
    assert_eq!(ids(&f, "from:\"Owen Castillo\""), vec!["budget"]);
    assert_eq!(ids(&f, "to:kettle.fern4821@example.org"), vec!["shipped"]);
    assert_eq!(ids(&f, "subject:order"), vec!["seeds", "shipped"]);
    assert!(ids(&f, "subject:juniper").is_empty());
    assert_eq!(ids(&f, "spreadsheet"), vec!["budget"], "preview is indexed");
}

#[test]
fn flag_and_attachment_filters() {
    let f = fixture();
    assert_eq!(ids(&f, "is:unread"), vec!["budget", "rota"]);
    assert_eq!(ids(&f, "is:read"), vec!["seeds", "shipped"]);
    assert_eq!(ids(&f, "is:pinned"), vec!["budget"]);
    assert_eq!(ids(&f, "is:flagged"), vec!["budget"]);
    assert_eq!(ids(&f, "-is:pinned is:unread"), vec!["rota"]);
    assert_eq!(ids(&f, "has:attachment"), vec!["budget"]);
    assert_eq!(ids(&f, "juniper is:unread"), vec!["rota"]);
}

#[test]
fn mailbox_filters() {
    let f = fixture();
    assert_eq!(ids(&f, "in:inbox"), vec!["budget", "rota", "shipped"]);
    assert_eq!(ids(&f, "in:archive"), vec!["seeds"]);
    assert_eq!(ids(&f, "in:\"Projects/Garden club\""), vec!["rota"]);
    assert_eq!(ids(&f, "in:\"garden club\" rota"), vec!["rota"]);
    assert_eq!(ids(&f, "juniper -in:archive"), vec!["rota"]);
    let r = f
        .reader()
        .search("in:nowhere juniper", &Default::default())
        .unwrap();
    assert_eq!(r.issues[0].kind, IssueKind::UnknownMailbox);
    assert_eq!(
        r.hits.len(),
        2,
        "the unknown filter is reported, not applied"
    );
}

#[test]
fn dates_and_sizes() {
    let f = fixture();
    assert_eq!(ids(&f, "after:2026-10-23"), vec!["budget", "rota"]);
    assert_eq!(ids(&f, "before:2026-10-23"), vec!["seeds", "shipped"]);
    assert_eq!(ids(&f, "date:2026-10-22"), vec!["shipped"]);
    assert_eq!(
        ids(&f, "date:2026-10"),
        vec!["budget", "rota", "seeds", "shipped"]
    );
    assert_eq!(ids(&f, "larger:100k"), vec!["budget"]);
    assert_eq!(ids(&f, "smaller:100k"), vec!["rota", "seeds", "shipped"]);
    // A zone where 12:02 UTC on the 23rd is already the 24th.
    let east = SearchOptions {
        time_zone: TimeZone::fixed(jiff::tz::offset(13)),
        ..Default::default()
    };
    assert_eq!(
        ids_with(&f, "date:2026-10-24", &east),
        vec!["budget", "rota"]
    );
}

#[test]
fn negation_and_or() {
    let f = fixture();
    assert_eq!(ids(&f, "order -seed"), vec!["shipped"]);
    assert_eq!(ids(&f, "-juniper"), vec!["budget", "shipped"]);
    assert_eq!(ids(&f, "rota OR spreadsheet"), vec!["budget", "rota"]);
    assert_eq!(
        ids(&f, "from:owen OR from:larkspur"),
        vec!["budget", "shipped"]
    );
}

#[test]
fn unsupported_operators_are_reported_back() {
    let f = fixture();
    let r = f
        .reader()
        .search(
            "juniper cc:owen filename:rota.pdf is:muted",
            &Default::default(),
        )
        .unwrap();
    let kinds: Vec<IssueKind> = r.issues.iter().map(|i| i.kind).collect();
    assert_eq!(
        kinds,
        vec![
            IssueKind::UnsupportedOperator,
            IssueKind::UnsupportedOperator,
            IssueKind::UnsupportedValue
        ]
    );
    assert_eq!(r.issues[0].term, "cc:owen");
    assert_eq!(r.hits.len(), 2);
    assert!(r.full_text);
}

#[test]
fn ranking_and_snippets() {
    let f = fixture();
    let r = f.reader().search("order", &Default::default()).unwrap();
    assert!(r.full_text);
    for hit in &r.hits {
        let m = hit
            .subject
            .matches
            .first()
            .expect("subject highlighted")
            .clone();
        assert_eq!(hit.subject.text[m].to_lowercase(), "order");
        assert!(!hit.snippet.text.contains('\u{2}'));
    }
    // A subject match outranks a body-only match.
    let r = f.reader().search("final", &Default::default()).unwrap();
    assert_eq!(r.hits[0].row.email_id, "seeds");
    // Filter-only queries come newest first, unhighlighted.
    let r = f.reader().search("is:unread", &Default::default()).unwrap();
    assert!(!r.full_text);
    assert_eq!(r.hits[0].row.email_id, "rota");
    assert!(r.hits[0].subject.matches.is_empty());
}

#[test]
fn limit_and_injection_safety() {
    let f = fixture();
    let one = SearchOptions {
        limit: 1,
        ..Default::default()
    };
    assert_eq!(ids_with(&f, "order", &one).len(), 1);
    for nasty in [
        "\"",
        "\"\"",
        "*",
        "NEAR(a b)",
        "a AND",
        "subject:",
        "{subject}: x",
        "x\" OR \"y",
        "-",
        "--",
        "'; DROP TABLE email; --",
        "^garden",
        "garden*",
        "(",
        ")",
        "OR OR OR",
    ] {
        f.reader()
            .search(nasty, &Default::default())
            .unwrap_or_else(|e| panic!("{nasty:?} failed: {e}"));
    }
    assert_eq!(ids(&f, "garden*"), vec!["rota"]);
}

#[test]
fn pending_deletes_are_not_found() {
    let mut f = fixture();
    f.store
        .enqueue(
            mejl_store::Mutation::EmailDestroy(vec!["rota".into()]),
            Default::default(),
            now(),
        )
        .unwrap();
    assert!(ids(&f, "garden").is_empty());
    assert!(!ids(&f, "is:unread").contains(&"rota".to_owned()));
}

#[test]
fn ranking_window_widens_for_selective_filters() {
    let mut f = Fixture::new().with_mailboxes();
    // 2,600 matches for "kettle", newest first in rowid order. Only the
    // oldest few are pinned, so the first window of 2,000 has none.
    let emails: Vec<_> = (0..2600)
        .map(|i| {
            let e = email(&format!("k{i:04}"))
                .subject("kettle descaling notes")
                .at(&ts("2026-01-01T00:00:00Z")
                    .checked_add(jiff::SignedDuration::from_mins(i))
                    .unwrap()
                    .to_string());
            if i < 3 { e.keywords(&["$flagged"]) } else { e }.build()
        })
        .collect();
    f.apply(&emails);
    let pinned = ids(&f, "kettle is:pinned");
    assert_eq!(pinned, vec!["k0000", "k0001", "k0002"]);
    let all = f
        .reader()
        .search(
            "kettle",
            &SearchOptions {
                limit: 5,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(all.hits.len(), 5);
    let filter_only = f
        .reader()
        .search(
            "is:pinned",
            &SearchOptions {
                limit: 2,
                ..Default::default()
            },
        )
        .unwrap();
    let newest: Vec<&str> = filter_only
        .hits
        .iter()
        .map(|h| h.row.email_id.as_str())
        .collect();
    assert_eq!(
        newest,
        vec!["k0002", "k0001"],
        "filter-only results come newest first"
    );
}
