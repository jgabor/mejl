//! Search: live local results, server results, results as a list, saved
//! searches, and the screen at the mock-up sizes.

mod common;

use common::Harness;
use mejl_ui::keymap::{Context, Keymap, Pending, Resolution};
use mejl_ui::{
    Action, Cmd, Event, Key, MailData, MailboxRole, Msg, Overlay, PushState, Screen, SyncStatus,
};

fn synced(w: u16, h: u16) -> Harness {
    let mut hs = Harness::demo(w, h);
    hs.event(Event::Msg(Msg::Sync(SyncStatus::Synced)));
    hs.event(Event::Msg(Msg::Push(PushState::Connected)));
    hs
}

fn typed(hs: &mut Harness, text: &str) -> Vec<Cmd> {
    let mut all = Vec::new();
    for c in text.chars() {
        let key = if c == ' ' {
            "space".to_owned()
        } else {
            c.to_string()
        };
        all.extend(hs.keys(&key));
    }
    all
}

const QUERY: &str = "from:larkspur has:attachment deliveredto:shop";

/// The filter mejl-sync's `filter_preview` makes of [`QUERY`]; its test
/// `preview_is_the_filter_the_search_sends` checks the same string.
const FILTER: &str = r#"{"operator":"AND","conditions":[{"from":"larkspur"},{"hasAttachment":true},{"header":["X-Delivered-To","shop"]}]}"#;

/// The search mock-up: a query with operators, the JMAP filter it sends,
/// local results and two more from the server.
fn mockup(w: u16, h: u16) -> Harness {
    let mut hs = synced(w, h);
    hs.data.set_filter_preview(QUERY, FILTER);
    hs.keys("/");
    typed(&mut hs, QUERY);
    let cmds = hs.keys("enter");
    assert!(cmds.contains(&Cmd::ServerSearch {
        query: QUERY.into(),
        limit: 100,
    }));
    let archive = hs.data.mailbox_for_role(MailboxRole::Archive).unwrap();
    let rows = hs.data.conversation_page(&archive, 0, 2).items;
    hs.event(Event::Msg(Msg::ServerSearch {
        query: QUERY.into(),
        result: Ok(rows),
    }));
    hs
}

#[test]
fn search_120x36() {
    let hs = mockup(120, 36);
    insta::assert_snapshot!(hs.text());
}

#[test]
fn search_80x24() {
    let hs = mockup(80, 24);
    insta::assert_snapshot!(hs.text());
}

#[test]
fn search_160x40() {
    let hs = mockup(160, 40);
    insta::assert_snapshot!(hs.text());
}

#[test]
fn the_jmap_filter_follows_the_query() {
    let mut hs = synced(120, 36);
    hs.data.set_filter_preview("", "null");
    hs.data.set_filter_preview("garden", r#"{"text":"garden"}"#);
    hs.keys("/");
    let row = |hs: &Harness| hs.line(6);
    assert!(
        row(&hs).contains("JMAP     Email/query  filter: appears as you type"),
        "{}",
        hs.text()
    );
    assert!(hs.line(8).contains("sort: receivedAt descending"));
    typed(&mut hs, "garden");
    assert!(
        row(&hs).contains(r#"Email/query  filter: {"text":"garden"}"#),
        "{}",
        row(&hs)
    );
    // A query the data cannot preview drops the filter rows.
    typed(&mut hs, "s");
    assert!(
        row(&hs).contains("Email/query  sort: receivedAt descending   collapseThreads: true"),
        "{}",
        hs.text()
    );
    // The filter is sanitised before it is drawn.
    hs.data
        .set_filter_preview("gardensx", "{\"text\":\"gardens\u{202e}x\"}");
    typed(&mut hs, "x");
    assert!(row(&hs).contains(r#"{"text":"gardensx"}"#), "{}", row(&hs));
}

#[test]
fn results_update_per_keystroke_and_report_skipped_terms() {
    let mut hs = synced(120, 36);
    hs.keys("/");
    assert_eq!(*hs.state.screen(), Screen::Search);
    assert!(hs.text().contains("Type a query"));
    typed(&mut hs, "garden");
    assert!(
        hs.text().contains("Re: Garden club plot rota"),
        "{}",
        hs.text()
    );
    typed(&mut hs, " zzzz");
    assert!(hs.text().contains("No local matches"));
    hs.keys("ctrl-w ctrl-w");
    typed(&mut hs, "deliveredto:x");
    let text = hs.text();
    assert!(
        text.contains("skipped deliveredto:x (not in the local index)"),
        "{text}"
    );
    assert!(text.contains("[deliveredto: x]"));
}

#[test]
fn esc_restores_the_mailbox_and_cursor() {
    let mut hs = synced(160, 40);
    hs.keys("j j");
    let mailbox = hs.state.mailbox().cloned();
    assert_eq!(hs.state.list().cursor(), 2);
    hs.keys("/");
    typed(&mut hs, "receipt");
    hs.keys("esc");
    assert_eq!(*hs.state.screen(), Screen::Mail);
    assert_eq!(hs.state.mailbox().cloned(), mailbox);
    assert_eq!(hs.state.list().cursor(), 2);
}

#[test]
fn results_open_as_a_list_and_esc_goes_back() {
    let mut hs = synced(160, 40);
    hs.keys("j j j");
    hs.keys("/");
    typed(&mut hs, "larkspur");
    hs.keys("enter");
    hs.event(Event::Msg(Msg::ServerSearch {
        query: "larkspur".into(),
        result: Ok(Vec::new()),
    }));
    hs.keys("down enter");
    assert_eq!(*hs.state.screen(), Screen::Mail);
    let text = hs.text();
    assert!(text.contains("Search: larkspur"), "{text}");
    assert_eq!(hs.state.list().cursor(), 1);
    assert!(hs.state.reader().conversation().is_some());
    // Triage works on results and acts from the result's mailbox.
    let cmds = hs.keys("s");
    assert!(cmds.iter().any(|c| matches!(c, Cmd::Mutate(_))), "{cmds:?}");
    // `/` reopens the search with the query.
    hs.keys("/");
    assert_eq!(hs.state.search_query(), Some("larkspur"));
    hs.keys("esc");
    assert!(hs.text().contains("Search: larkspur"));
    hs.keys("esc esc esc");
    assert!(!hs.text().contains("Search: larkspur"));
    assert_eq!(hs.state.list().cursor(), 3, "the inbox cursor is back");
}

#[test]
fn stale_server_answers_are_ignored() {
    let mut hs = synced(120, 36);
    hs.keys("/");
    typed(&mut hs, "garden");
    hs.keys("enter");
    typed(&mut hs, "s");
    let archive = hs.data.mailbox_for_role(MailboxRole::Archive).unwrap();
    let rows = hs.data.conversation_page(&archive, 0, 3).items;
    hs.event(Event::Msg(Msg::ServerSearch {
        query: "garden".into(),
        result: Ok(rows),
    }));
    assert!(!hs.text().contains("more from the server"), "{}", hs.text());
    hs.event(Event::Msg(Msg::ServerSearch {
        query: "gardens".into(),
        result: Err("offline".into()),
    }));
    assert!(hs.state.toast().unwrap().text.contains("offline"));
    assert!(hs.text().contains("the server search failed"));
}

#[test]
fn save_search_and_run_it_from_the_sidebar() {
    let mut hs = synced(160, 40);
    hs.keys("/");
    typed(&mut hs, "is:unread");
    hs.keys("ctrl-s");
    assert!(hs.line(38).contains("Save search as:"));
    hs.keys("ctrl-u");
    typed(&mut hs, "Unread");
    let cmds = hs.keys("enter");
    assert!(cmds.contains(&Cmd::SaveSearch {
        name: "Unread".into(),
        query: "is:unread".into()
    }));
    hs.keys("esc");
    // The demo's saved searches are in the sidebar.
    let (x, y) = hs.find("Has attachment").unwrap();
    let cmds = hs.click(x, y);
    assert!(cmds.contains(&Cmd::ServerSearch {
        query: "has:attachment".into(),
        limit: 100
    }));
    assert!(hs.text().contains("Search: has:attachment"));
    assert!(hs.state.list().total() > 0);
}

#[test]
fn mouse_selects_and_opens_results() {
    let mut hs = synced(120, 36);
    hs.keys("/");
    typed(&mut hs, "larkspur");
    let (_, y) = hs.find("Larkspur").unwrap();
    hs.click(10, y + 1);
    assert!(hs.text().contains("Enter open as a list"));
    hs.click(10, y + 1);
    assert_eq!(*hs.state.screen(), Screen::Mail, "double click opens");
}

#[test]
fn paste_goes_into_the_query() {
    let mut hs = synced(120, 36);
    hs.keys("/");
    hs.event(Event::Paste("garden\nclub".into()));
    assert_eq!(hs.state.search_query(), Some("garden club"));
}

#[test]
fn search_keys_are_in_the_keymap() {
    let map = Keymap::default();
    for (key, action) in [
        ("enter", Action::Open),
        ("down", Action::Next),
        ("up", Action::Prev),
        ("ctrl-n", Action::Next),
        ("ctrl-p", Action::Prev),
        ("tab", Action::NextField),
        ("ctrl-r", Action::SearchServer),
        ("ctrl-s", Action::SaveSearch),
        ("esc", Action::Back),
    ] {
        let k: Key = key.parse().unwrap();
        assert_eq!(
            map.feed(&mut Pending::default(), &[Context::Search], k),
            Resolution::Action {
                action,
                count: None
            },
            "{key}"
        );
    }
    let k: Key = "j".parse().unwrap();
    assert_eq!(
        map.feed(&mut Pending::default(), &[Context::Search], k),
        Resolution::Unbound,
        "letters type"
    );
}

#[test]
fn save_prompt_cancels() {
    let mut hs = synced(120, 36);
    hs.keys("/");
    typed(&mut hs, "x");
    hs.keys("ctrl-s");
    assert!(matches!(
        hs.state.overlays().last(),
        Some(Overlay::Prompt(_))
    ));
    let cmds = hs.keys("esc");
    assert!(!cmds.iter().any(|c| matches!(c, Cmd::SaveSearch { .. })));
    assert!(hs.state.overlays().is_empty());
    assert_eq!(*hs.state.screen(), Screen::Search);
}
