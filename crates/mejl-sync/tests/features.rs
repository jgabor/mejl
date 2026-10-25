//! Masked Email and server search against the fake.

mod common;

use std::time::Duration;

use common::*;
use mejl_fake::{ExternalChange, FakeServer, MaskedEmailState};
use mejl_jmap::types::MaskedEmailState as WireState;
use mejl_store::UndoOutcome;
use mejl_sync::{Command, MaskedState, SearchRequest, SyncEvent};
use serde_json::json;

fn masked_on_server(server: &FakeServer, id: &str) -> serde_json::Value {
    server
        .call("MaskedEmail/get", json!({"ids": [id]}))
        .unwrap()["list"][0]
        .clone()
}

fn masked_round_trip(quirk: bool) {
    let server = FakeServer::demo(now());
    server.quirks_mut().masked_email_without_state = quirk;
    let h = Harness::start(server, Setup::default());
    h.synced();
    h.wait_store("masks", |r| r.masked_emails().unwrap().len() == 20);

    h.handle.send(Command::MaskedCreate {
        token: 1,
        for_domain: "https://shop.example.com".into(),
        description: "Bike shop".into(),
        email_prefix: Some("bikes".into()),
    });
    let applied = h.events.wait("applied", |e| {
        matches!(e, SyncEvent::Applied { token: 1, .. })
    });
    let SyncEvent::Applied { temp_id, seq, .. } = applied else {
        panic!("expected applied");
    };
    assert!(temp_id.is_some_and(|t| t.starts_with("local:")));
    let created = h.events.wait(
        "created",
        |e| matches!(e, SyncEvent::MaskedCreated { seq: s, .. } if *s == seq),
    );
    let SyncEvent::MaskedCreated { id, email, .. } = created else {
        panic!("expected a created mask");
    };
    let address = email.expect("the server made an address");
    assert!(address.starts_with("bikes"), "{address}");
    let on_server = masked_on_server(&h.server, id.as_str());
    assert_eq!(on_server["state"], "enabled");
    assert_eq!(on_server["description"], "Bike shop");
    h.wait_store("new mask cached", |r| {
        r.masked_emails().unwrap().iter().any(|m| {
            m.masked.id == id && !m.pending && m.masked.email.as_deref() == Some(address.as_str())
        })
    });

    h.handle.send(Command::MaskedSetState {
        token: 2,
        id: id.clone(),
        state: MaskedState::Disabled,
    });
    h.events.wait("disable applied", |e| {
        matches!(e, SyncEvent::Applied { token: 2, .. })
    });
    // Shown at once through the overlay.
    assert!(
        h.read(|r| r.masked_emails().unwrap())
            .iter()
            .any(|m| { m.masked.id == id && m.masked.state == Some(WireState::Disabled) })
    );
    h.wait_server("disabled on the server", |s| {
        masked_on_server(s, id.as_str())["state"] == "disabled"
    });
    h.wait_store("disabled and confirmed", |r| {
        r.masked_emails()
            .unwrap()
            .iter()
            .any(|m| m.masked.id == id && !m.pending && m.masked.state == Some(WireState::Disabled))
    });

    // Undo enables it again.
    h.handle.send(Command::Undo { token: 3 });
    assert!(matches!(
        h.events.wait("undone", |e| matches!(
            e,
            SyncEvent::Undone { token: 3, .. }
        )),
        SyncEvent::Undone {
            outcome: Some(UndoOutcome::Reverted { .. }),
            ..
        }
    ));
    h.wait_server("enabled again", |s| {
        masked_on_server(s, id.as_str())["state"] == "enabled"
    });

    // The description changes too.
    h.handle.send(Command::MaskedSetDescription {
        token: 4,
        id: id.clone(),
        description: "Bike shop, spares".into(),
    });
    h.wait_server("described", |s| {
        masked_on_server(s, id.as_str())["description"] == "Bike shop, spares"
    });
    if quirk {
        assert_eq!(h.transport.calls_named("MaskedEmail/changes"), 0);
    }
    h.shutdown();
}

#[test]
fn masked_email_create_and_disable_round_trip() {
    masked_round_trip(false);
}

#[test]
fn masked_email_without_state_strings() {
    masked_round_trip(true);
}

#[test]
fn masked_changes_elsewhere_arrive_by_refetch() {
    let server = FakeServer::demo(now());
    server.quirks_mut().masked_email_without_state = true;
    let mut setup = Setup::default();
    setup.config.masked_refresh = Duration::from_millis(200);
    let h = Harness::start(server, setup);
    h.synced();
    h.wait_store("masks", |r| r.masked_emails().unwrap().len() == 20);
    let target = h.read(|r| {
        r.masked_emails()
            .unwrap()
            .into_iter()
            .find(|m| m.masked.state == Some(WireState::Enabled))
            .unwrap()
            .masked
            .id
    });
    h.server
        .mutate_from_elsewhere(ExternalChange::SetMaskedEmailState {
            id: target.to_string(),
            state: MaskedEmailState::Disabled,
        })
        .unwrap();
    // Push does not name masks with this quirk; the slow timer finds it.
    h.wait_store("disabled elsewhere", |r| {
        r.masked_emails()
            .unwrap()
            .iter()
            .any(|m| m.masked.id == target && m.masked.state == Some(WireState::Disabled))
    });
    h.shutdown();
}

#[test]
fn masked_create_is_refused_offline_and_with_a_bad_prefix() {
    let h = Harness::demo();
    h.synced();
    h.handle.send(Command::MaskedCreate {
        token: 5,
        for_domain: "https://shop.example.com".into(),
        description: "Bad".into(),
        email_prefix: Some("Not Valid!".into()),
    });
    h.events.wait("bad prefix", |e| {
        matches!(e, SyncEvent::Rejected { token: 5, .. })
    });
    h.transport
        .down
        .store(true, std::sync::atomic::Ordering::SeqCst);
    h.handle.send(Command::RefreshNow);
    h.events.wait(
        "offline",
        |e| matches!(e, SyncEvent::Status(s) if !s.online),
    );
    h.handle.send(Command::MaskedCreate {
        token: 6,
        for_domain: "https://shop.example.com".into(),
        description: "Offline".into(),
        email_prefix: None,
    });
    let SyncEvent::Rejected { error, .. } = h.events.wait("offline refusal", |e| {
        matches!(e, SyncEvent::Rejected { token: 6, .. })
    }) else {
        panic!("expected a refusal");
    };
    assert!(error.contains("connection"), "{error}");
    h.transport
        .down
        .store(false, std::sync::atomic::Ordering::SeqCst);
    h.shutdown();
}

#[test]
fn rate_limited_mask_create_fails_with_a_message() {
    let server = FakeServer::demo(now());
    server.quirks_mut().masked_email_rate_limit_next = 1;
    let h = Harness::start(server, Setup::default());
    h.synced();
    h.handle.send(Command::MaskedCreate {
        token: 7,
        for_domain: "https://shop.example.com".into(),
        description: "Too many".into(),
        email_prefix: None,
    });
    let SyncEvent::OutboxFailed { error, .. } = h
        .events
        .wait("failed", |e| matches!(e, SyncEvent::OutboxFailed { .. }))
    else {
        panic!("expected a failure");
    };
    assert!(error.contains("rate limited"), "{error}");
    h.wait_store("no pending mask left", |r| {
        r.masked_emails().unwrap().iter().all(|m| !m.pending)
    });
    h.shutdown();
}

#[test]
fn server_search_returns_ids_snippets_and_rows() {
    let h = Harness::demo();
    h.synced();
    h.handle.send(Command::Search {
        token: 8,
        request: SearchRequest::new("garden fromin:contacts"),
    });
    let SyncEvent::SearchResults { results, .. } = h.events.wait("results", |e| {
        matches!(e, SyncEvent::SearchResults { token: 8, .. })
    }) else {
        panic!("expected results");
    };
    assert!(!results.ids.is_empty());
    assert!(results.total.is_some_and(|t| t >= results.ids.len() as u64));
    assert_eq!(results.unsupported, ["fromin:contacts"]);
    assert!(
        results
            .snippets
            .iter()
            .any(|s| s.subject.as_deref().unwrap_or("").contains("<mark>")
                || s.preview.as_deref().unwrap_or("").contains("<mark>")),
        "{:?}",
        results.snippets
    );
    for id in &results.ids {
        assert!(
            h.read(|r| r.email(id.as_str()).unwrap()).is_some(),
            "row for {id}"
        );
    }

    // Operators narrow the result.
    let inbox = h.mailbox("inbox");
    let mut request = SearchRequest::new("is:unread");
    request.mailbox = Some(inbox.clone());
    h.handle.send(Command::Search { token: 9, request });
    let SyncEvent::SearchResults { results, .. } = h.events.wait("unread", |e| {
        matches!(e, SyncEvent::SearchResults { token: 9, .. })
    }) else {
        panic!("expected results");
    };
    for id in &results.ids {
        let detail = h.read(|r| r.email(id.as_str()).unwrap()).unwrap();
        assert!(!detail.headers.row.seen);
        assert!(detail.headers.mailbox_ids.contains(&inbox));
    }
    h.shutdown();
}

#[test]
fn local_writes_run_on_the_store_thread_and_answer() {
    use mejl_store::RuleRecord;
    use mejl_sync::{LocalOutcome, LocalWrite};
    let h = Harness::start(FakeServer::demo(now()), Setup::default());
    h.handle.send(Command::Local {
        token: 7,
        write: LocalWrite::SaveRule(RuleRecord {
            id: "r1".into(),
            name: "Receipts".into(),
            enabled: true,
            sort_order: 0,
            rule_json: "{\"id\":\"r1\"}".into(),
            updated_at: now(),
        }),
    });
    let done = h.events.wait("local done", |e| {
        matches!(e, SyncEvent::LocalDone { token: 7, .. })
    });
    assert_eq!(
        done,
        SyncEvent::LocalDone {
            token: 7,
            result: Ok(LocalOutcome::Done)
        }
    );
    h.wait_store("rule saved", |r| r.rules().unwrap().len() == 1);

    // A failing write answers with an error and changes nothing.
    h.handle.send(Command::Local {
        token: 8,
        write: LocalWrite::ReorderRules(vec!["missing".into()]),
    });
    let failed = h.events.wait("local failed", |e| {
        matches!(e, SyncEvent::LocalDone { token: 8, .. })
    });
    assert!(
        matches!(failed, SyncEvent::LocalDone { result: Err(_), .. }),
        "{failed:?}"
    );

    h.handle.send(Command::Local {
        token: 9,
        write: LocalWrite::SetUiValue {
            key: "saved_searches".into(),
            value: "[]".into(),
        },
    });
    h.events.wait("ui value", |e| {
        matches!(
            e,
            SyncEvent::LocalDone {
                token: 9,
                result: Ok(LocalOutcome::Done)
            }
        )
    });
    h.wait_store("ui value stored", |r| {
        r.ui_value("saved_searches").unwrap().as_deref() == Some("[]")
    });
}
