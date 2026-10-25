//! Bodies: fetch on demand, render off the UI thread, prefetch, and the
//! messages that report outcomes.

mod common;

use std::sync::Arc;

use common::{Harness, key_event, now};
use mejl_ui::{
    AppState, Cmd, EmailId, Event, MailData, ManualClock, MemoryData, Msg, OutboxOutcome,
    OutboxReport, RenderRequest, ToastLevel, UiConfig, render_body, update,
};

fn first_latest(data: &MemoryData) -> EmailId {
    let inbox = data.mailbox_for_role(mejl_ui::MailboxRole::Inbox).unwrap();
    data.conversation_page(&inbox, 0, 1).items[0]
        .latest_email_id
        .clone()
}

#[test]
fn missing_body_is_fetched_then_shown() {
    let mut data = MemoryData::demo(now());
    let id = first_latest(&data);
    let body = data.remove_body(&id).unwrap();
    let clock = Arc::new(ManualClock::new(now()));
    let mut state = AppState::new(UiConfig::default(), clock, (160, 40));
    let cmds = state.start(&data);
    assert!(cmds.contains(&Cmd::FetchBody(id.clone())), "{cmds:?}");
    assert!(
        state
            .reader()
            .text_lines()
            .join("\n")
            .contains("loading the message")
    );
    // Asking again does not repeat the fetch.
    let cmds = update(&mut state, &data, Event::Key(key_event("down")));
    let cmds2 = update(&mut state, &data, Event::Key(key_event("up")));
    assert!(
        !cmds
            .iter()
            .chain(&cmds2)
            .any(|c| *c == Cmd::FetchBody(id.clone()))
    );
    data.add_body(body);
    let cmds = update(&mut state, &data, Event::Msg(Msg::BodyFetched(id.clone())));
    // The body is HTML, so it goes to the worker.
    let req = cmds.iter().find_map(|c| match c {
        Cmd::RenderBody(r) => Some(r.clone()),
        _ => None,
    });
    let req = req.expect("render request");
    assert_eq!(
        req,
        RenderRequest {
            email_id: id.clone(),
            width: 56,
            prefer_html: false
        }
    );
    assert!(
        state
            .reader()
            .text_lines()
            .join("\n")
            .contains("preparing the message")
    );
    let view = data.message_body(&id).unwrap();
    let rendered = render_body(&view, req.width, false);
    update(
        &mut state,
        &data,
        Event::Msg(Msg::BodyRendered {
            email_id: id,
            prefer_html: false,
            body: Box::new(rendered),
        }),
    );
    assert!(
        state
            .reader()
            .text_lines()
            .join("\n")
            .contains("Swapping Saturday")
    );
}

#[test]
fn failed_fetch_is_reported_in_words() {
    let mut data = MemoryData::demo(now());
    let id = first_latest(&data);
    data.remove_body(&id);
    let clock = Arc::new(ManualClock::new(now()));
    let mut state = AppState::new(UiConfig::default(), clock, (160, 40));
    state.start(&data);
    update(
        &mut state,
        &data,
        Event::Msg(Msg::BodyFailed {
            email_id: id,
            error: "offline".into(),
        }),
    );
    assert!(
        state
            .reader()
            .text_lines()
            .join("\n")
            .contains("could not load this message: offline")
    );
}

#[test]
fn resize_keeps_the_old_rendering_until_the_new_one_lands() {
    let data = MemoryData::demo(now());
    let clock = Arc::new(ManualClock::new(now()));
    let mut state = AppState::new(UiConfig::default(), clock, (160, 40));
    let cmds = state.start(&data);
    let id = first_latest(&data);
    for c in cmds {
        if let Cmd::RenderBody(r) = c {
            let view = data.message_body(&r.email_id).unwrap();
            update(
                &mut state,
                &data,
                Event::Msg(Msg::BodyRendered {
                    email_id: r.email_id,
                    prefer_html: false,
                    body: Box::new(render_body(&view, r.width, false)),
                }),
            );
        }
    }
    let cmds = update(&mut state, &data, Event::Resize(180, 40));
    assert!(
        cmds.iter()
            .any(|c| matches!(c, Cmd::RenderBody(r) if r.email_id == id && r.width != 56))
    );
    let lines = state.reader().text_lines().join("\n");
    assert!(lines.contains("Swapping Saturday"), "{lines}");
    assert!(lines.contains("rewrapping"), "{lines}");
}

#[test]
fn plain_bodies_render_inline() {
    let mut hs = Harness::demo(160, 40);
    hs.log.clear();
    hs.keys("j");
    assert!(
        !hs.log.iter().any(|c| matches!(c, Cmd::RenderBody(_))),
        "{:?}",
        hs.log
    );
    assert!(
        hs.state
            .reader()
            .text_lines()
            .join("\n")
            .contains("Attached the spreadsheet")
    );
}

#[test]
fn prefetch_follows_the_direction_of_travel() {
    let mut hs = Harness::demo(160, 40);
    let inbox = hs
        .data
        .mailbox_for_role(mejl_ui::MailboxRole::Inbox)
        .unwrap();
    let page = hs.data.conversation_page(&inbox, 0, 20);
    let cmds = hs.keys("j j j j j j j j j j");
    let last = cmds.iter().rev().find_map(|c| match c {
        Cmd::Prefetch(ids) => Some(ids.clone()),
        _ => None,
    });
    let want: Vec<EmailId> = (11..14)
        .map(|i| page.items[i].latest_email_id.clone())
        .collect();
    assert_eq!(last, Some(want));
    let cmds = hs.keys("k");
    let up = cmds.iter().find_map(|c| match c {
        Cmd::Prefetch(ids) => Some(ids.clone()),
        _ => None,
    });
    let want: Vec<EmailId> = (6..9)
        .rev()
        .map(|i| page.items[i].latest_email_id.clone())
        .collect();
    assert_eq!(up, Some(want));
    // Standing still prefetches nothing new.
    let cmds = hs.keys("ctrl-l");
    assert!(!cmds.iter().any(|c| matches!(c, Cmd::Prefetch(_))));
}

#[test]
fn outbox_failures_and_errors_become_toasts() {
    let mut hs = Harness::demo(160, 40);
    hs.event(Event::Msg(Msg::Outbox(OutboxReport {
        label: "Archive 1 conversation".into(),
        outcome: OutboxOutcome::Failed {
            error: "the server said no".into(),
        },
    })));
    let t = hs.state.toast().unwrap();
    assert_eq!(t.level, ToastLevel::Error);
    assert_eq!(t.text, "Archive 1 conversation failed: the server said no");
    assert!(hs.line(38).contains("! Archive 1 conversation failed"));
    hs.event(Event::Msg(Msg::Outbox(OutboxReport {
        label: "x".into(),
        outcome: OutboxOutcome::Done,
    })));
    assert_eq!(
        hs.state.toast().unwrap().level,
        ToastLevel::Error,
        "success is silent"
    );
    hs.event(Event::Msg(Msg::Error("Token expired".into())));
    assert_eq!(hs.state.toast().unwrap().text, "Token expired");
    hs.event(Event::Msg(Msg::Toast {
        text: "3 new messages".into(),
        level: ToastLevel::Info,
    }));
    assert!(hs.line(38).contains("3 new messages"));
}

#[test]
fn sync_states_read_in_words() {
    let mut hs = Harness::demo(160, 40);
    let cases = [
        (
            mejl_ui::SyncStatus::Starting,
            "starting, showing cached mail",
        ),
        (mejl_ui::SyncStatus::Syncing, "syncing"),
        (
            mejl_ui::SyncStatus::Offline { pending: 1 },
            "offline, 1 change pending",
        ),
        (mejl_ui::SyncStatus::Offline { pending: 0 }, "offline"),
        (
            mejl_ui::SyncStatus::Failed {
                error: "timed out".into(),
            },
            "sync failed: timed out",
        ),
        (mejl_ui::SyncStatus::Synced, "synced, connecting push"),
    ];
    for (status, text) in cases {
        hs.event(Event::Msg(Msg::Sync(status)));
        assert!(hs.line(38).contains(text), "{text}: {}", hs.line(38));
    }
    hs.event(Event::Msg(Msg::Push(mejl_ui::PushState::Off)));
    assert!(hs.line(38).contains("synced, push off"));
    hs.event(Event::Msg(Msg::Push(mejl_ui::PushState::Connected)));
    assert!(hs.line(38).contains("synced via JMAP push"));
    assert!(hs.line(38).contains("14:02:31"));
    hs.clock.advance(jiff::SignedDuration::from_secs(9));
    hs.event(Event::Msg(Msg::Tick));
    assert!(hs.line(38).contains("14:02:40"));
}
