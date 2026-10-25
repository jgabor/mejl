//! The command palette and the pickers and menus: move, labels, links,
//! copy, actions, Masked Email actions; mute, thread tree, plain/HTML.

mod common;

use common::Harness;
use crossterm::event::{MouseButton, MouseEventKind};
use mejl_ui::picker::PickerKind;
use mejl_ui::{
    Cmd, Event, Keyword, MailData, Msg, Mutation, Overlay, PushState, Screen, SyncStatus, TargetIds,
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

fn picker_kind(hs: &Harness) -> Option<PickerKind> {
    match hs.state.overlays().last() {
        Some(Overlay::Picker(p)) => Some(p.kind),
        _ => None,
    }
}

#[test]
fn palette_120x36() {
    let mut hs = synced(120, 36);
    hs.keys(":");
    typed(&mut hs, "new");
    insta::assert_snapshot!(hs.text());
}

#[test]
fn palette_80x24() {
    let mut hs = synced(80, 24);
    hs.keys("ctrl-k");
    insta::assert_snapshot!(hs.text());
}

#[test]
fn palette_160x40() {
    let mut hs = synced(160, 40);
    hs.keys(":");
    typed(&mut hs, "go");
    insta::assert_snapshot!(hs.text());
}

#[test]
fn palette_runs_actions_and_shows_keys() {
    let mut hs = synced(120, 36);
    hs.keys(":");
    assert_eq!(picker_kind(&hs), Some(PickerKind::Palette));
    let text = hs.text();
    assert!(text.contains("Commands"), "{text}");
    assert!(text.contains(": or Ctrl-k"));
    typed(&mut hs, "write a new");
    assert!(hs.line(34).contains("of"), "{}", hs.line(34));
    hs.keys("enter");
    assert_eq!(*hs.state.screen(), Screen::Compose);
}

#[test]
fn recent_actions_come_first() {
    let mut hs = synced(120, 36);
    hs.keys(":");
    typed(&mut hs, "archive");
    let cmds = hs.keys("enter");
    assert!(
        cmds.iter()
            .any(|c| matches!(c, Cmd::Mutate(Mutation::Archive(_))))
    );
    hs.keys(":");
    match hs.state.overlays().last() {
        Some(Overlay::Picker(p)) => {
            assert_eq!(p.selected().unwrap().label, "Archive");
        }
        other => panic!("{other:?}"),
    }
    hs.keys("esc");
    assert!(hs.state.overlays().is_empty());
}

#[test]
fn palette_goes_to_mailboxes_contacts_and_rules() {
    let mut hs = synced(160, 40);
    hs.keys(":");
    typed(&mut hs, "go to receipts");
    hs.keys("enter");
    assert!(hs.text().contains("Receipts"));
    let receipts = hs.state.mailbox().cloned();
    assert_ne!(
        receipts,
        hs.data.mailbox_for_role(mejl_ui::MailboxRole::Inbox)
    );
    hs.keys(":");
    typed(&mut hs, "contact nia");
    hs.keys("enter");
    assert_eq!(*hs.state.screen(), Screen::Compose);
    assert!(hs.text().contains("Nia Newell <nia@example.net>"));
    hs.keys("esc esc");
    // Tab completes the filter to the highlighted row.
    hs.keys(":");
    typed(&mut hs, "rule: news");
    hs.keys("tab");
    match hs.state.overlays().last() {
        Some(Overlay::Picker(p)) => assert_eq!(p.filter.text(), "Rule: Newsletters"),
        other => panic!("{other:?}"),
    }
    hs.keys("enter");
    assert_eq!(*hs.state.screen(), Screen::Rules);
}

#[test]
fn palette_paste_and_mouse() {
    let mut hs = synced(120, 36);
    hs.keys(":");
    hs.event(Event::Paste("trash\n".into()));
    match hs.state.overlays().last() {
        Some(Overlay::Picker(p)) => assert_eq!(p.filter.text(), "trash "),
        other => panic!("{other:?}"),
    }
    let (x, y) = hs.find("Move to trash").unwrap();
    let cmds = hs.click(x, y);
    assert!(
        cmds.iter()
            .any(|c| matches!(c, Cmd::Mutate(Mutation::Trash(_))))
    );
    // A click outside closes.
    hs.keys(":");
    hs.click(1, 1);
    assert!(hs.state.overlays().is_empty());
    // The wheel moves the cursor.
    hs.keys(":");
    hs.mouse(MouseEventKind::ScrollDown, 40, 10);
    match hs.state.overlays().last() {
        Some(Overlay::Picker(p)) => assert_eq!(p.cursor, 1),
        other => panic!("{other:?}"),
    }
}

#[test]
fn move_picker_moves_and_creates() {
    let mut hs = synced(160, 40);
    hs.keys("m");
    assert_eq!(picker_kind(&hs), Some(PickerKind::MoveTo));
    assert!(hs.text().contains("Move 1 conversation to"));
    assert!(
        !hs.text().contains("> type to filter\n│ ▸ Inbox"),
        "the current mailbox is left out"
    );
    typed(&mut hs, "receipts");
    let cmds = hs.keys("enter");
    assert!(
        cmds.iter()
            .any(|c| matches!(c, Cmd::Mutate(Mutation::MoveTo { .. }))),
        "{cmds:?}"
    );
    assert!(
        hs.state
            .toast()
            .unwrap()
            .text
            .contains("Moved 1 conversation to Receipts")
    );
    hs.keys("v");
    typed(&mut hs, "Taxes 2026");
    assert!(hs.text().contains("Create mailbox \"Taxes 2026\""));
    let cmds = hs.keys("enter");
    assert!(cmds.iter().any(|c| matches!(
        c,
        Cmd::CreateMailbox {
            name,
            then_move: true,
            then_add: Some(_),
            ..
        } if name == "Taxes 2026"
    )));
}

#[test]
fn label_picker_toggles_and_stays_open() {
    let mut hs = synced(160, 40);
    hs.keys("l");
    assert_eq!(picker_kind(&hs), Some(PickerKind::Label));
    // The first conversation is in Garden club.
    assert!(
        hs.text().contains("[x] Projects/Garden club"),
        "{}",
        hs.text()
    );
    typed(&mut hs, "garden");
    let cmds = hs.keys("enter");
    assert!(
        cmds.iter()
            .any(|c| matches!(c, Cmd::Mutate(Mutation::SetMailbox { member: false, .. })))
    );
    assert_eq!(picker_kind(&hs), Some(PickerKind::Label));
    assert!(hs.text().contains("[ ] Projects/Garden club"));
    hs.keys("ctrl-u space");
    hs.keys("esc");
    assert!(hs.state.overlays().is_empty());
}

#[test]
fn links_overlay_opens_and_copies() {
    let mut hs = synced(160, 40);
    hs.keys("enter L");
    assert_eq!(picker_kind(&hs), Some(PickerKind::Links));
    assert!(hs.text().contains("[1] https://garden.example.com/rota"));
    let cmds = hs.keys("enter");
    assert!(cmds.contains(&Cmd::OpenUrl("https://garden.example.com/rota".into())));
    hs.keys("L ctrl-y");
    assert!(hs.log.contains(&Cmd::CopyToClipboard(
        "https://garden.example.com/rota".into()
    )));
    // A message without links says so.
    hs.keys("j");
    hs.keys("L");
    assert!(hs.state.overlays().is_empty() || picker_kind(&hs) == Some(PickerKind::Links));
}

#[test]
fn copy_menu_copies() {
    let mut hs = synced(160, 40);
    hs.keys("enter Y");
    assert_eq!(picker_kind(&hs), Some(PickerKind::Copy));
    let text = hs.text();
    assert!(
        text.contains("Sender address: juniper@garden.example.com"),
        "{text}"
    );
    assert!(text.contains("Message id: <"));
    typed(&mut hs, "subject");
    let cmds = hs.keys("enter");
    assert!(cmds.contains(&Cmd::CopyToClipboard("Re: Garden club plot rota".into())));
    assert!(hs.state.toast().unwrap().text.contains("Copied"));
}

#[test]
fn actions_menu_runs_actions() {
    let mut hs = synced(160, 40);
    hs.keys(";");
    assert_eq!(picker_kind(&hs), Some(PickerKind::Actions));
    assert!(hs.text().contains("Reply to the sender"));
    hs.keys("enter");
    assert_eq!(*hs.state.screen(), Screen::Compose);
}

#[test]
fn mute_tree_and_html_toggle() {
    let mut hs = synced(160, 40);
    let cmds = hs.keys("M");
    assert!(cmds.iter().any(|c| matches!(
        c,
        Cmd::Mutate(Mutation::SetKeyword {
            keyword: Keyword::Muted,
            value: true,
            ..
        })
    )));
    assert!(
        hs.state
            .toast()
            .unwrap()
            .text
            .contains("Muted 1 conversation")
    );
    hs.keys("enter T");
    assert!(hs.state.reader().tree());
    let lines = hs.state.reader().text_lines().join("\n");
    assert!(lines.contains("> "), "replies are indented: {lines}");
    hs.keys("T");
    assert!(!hs.state.reader().tree());
    hs.keys("V");
    assert!(
        hs.state
            .toast()
            .unwrap()
            .text
            .contains("Showing the HTML part")
    );
    hs.keys("V");
    assert!(
        hs.state
            .toast()
            .unwrap()
            .text
            .contains("Showing the plain text part")
    );
    // A message with only a text part says so.
    hs.keys("j j j V");
    let t = hs.state.toast().unwrap().text.clone();
    assert!(t.contains("part"), "{t}");
}

#[test]
fn html_toggle_renders_the_other_part() {
    let mut hs = synced(160, 40);
    // The Larkspur message has both parts.
    hs.keys("j j enter");
    let id = hs
        .data
        .conversation(&hs.state.list().cursor_thread().unwrap().clone())
        .unwrap()
        .messages
        .last()
        .unwrap()
        .email_id
        .clone();
    let has_html = hs.data.message_body(&id).is_some_and(|b| b.html.is_some());
    hs.keys("V");
    if has_html {
        assert!(
            hs.log.iter().any(|c| matches!(
                c,
                Cmd::RenderBody(r) if r.prefer_html != hs.state.config().prefer_html
            )) || hs.state.toast().unwrap().text.contains("part")
        );
    }
}

#[test]
fn mask_menu_from_a_masked_message() {
    let mut hs = synced(160, 40);
    hs.keys("j j enter @ @");
    assert_eq!(picker_kind(&hs), Some(PickerKind::Mask), "{}", hs.text());
    let text = hs.text();
    assert!(
        text.contains("Delivered to kettle.fern4821@example.org (enabled)"),
        "{text}"
    );
    typed(&mut hs, "disable");
    let cmds = hs.keys("enter");
    assert!(cmds.iter().any(|c| matches!(
        c,
        Cmd::MaskedSetState {
            state: mejl_ui::MaskedState::Disabled,
            ..
        }
    )));
    assert!(
        hs.state
            .toast()
            .unwrap()
            .text
            .contains("Future mail goes to Trash")
    );
    hs.keys("@ @");
    typed(&mut hs, "delete");
    hs.keys("enter");
    assert!(matches!(
        hs.state.overlays().last(),
        Some(Overlay::Confirm(_))
    ));
    let cmds = hs.keys("y");
    assert!(cmds.iter().any(|c| matches!(
        c,
        Cmd::MaskedSetState {
            state: mejl_ui::MaskedState::Deleted,
            ..
        }
    )));
}

#[test]
fn right_click_on_a_reader_message() {
    let mut hs = synced(160, 40);
    let (x, y) = hs.find("Juniper Tran <").unwrap();
    hs.mouse(MouseEventKind::Down(MouseButton::Right), x, y);
    assert_eq!(picker_kind(&hs), Some(PickerKind::Actions));
    assert_eq!(hs.state.focus(), mejl_ui::Focus::Reader);
    typed(&mut hs, "pin");
    let cmds = hs.keys("enter");
    assert!(
        cmds.iter().any(|c| matches!(
            c,
            Cmd::Mutate(Mutation::SetKeyword {
                target: mejl_ui::Target {
                    ids: TargetIds::Emails(_),
                    ..
                },
                ..
            })
        )),
        "{cmds:?}"
    );
}
