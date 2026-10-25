//! Keyboard behaviour: crossterm events in, commands and state out.

mod common;

use common::{Harness, key_event};
use crossterm::event::{KeyEventKind, KeyModifiers};
use jiff::SignedDuration;
use mejl_ui::{
    Cmd, Event, Focus, Keyword, MailData, MailboxRole, Msg, Mutation, Overlay, Preset, TargetIds,
    ThreadId, UiConfig,
};

fn mutations(cmds: &[Cmd]) -> Vec<&Mutation> {
    cmds.iter()
        .filter_map(|c| match c {
            Cmd::Mutate(m) => Some(m),
            _ => None,
        })
        .collect()
}

fn toast(hs: &Harness) -> String {
    hs.state.toast().map(|t| t.text.clone()).unwrap_or_default()
}

fn cursor_thread(hs: &Harness) -> ThreadId {
    hs.state.list().cursor_thread().cloned().unwrap()
}

#[test]
fn moves_and_jumps() {
    let mut hs = Harness::demo(160, 40);
    assert_eq!(hs.state.list().cursor(), 0);
    hs.keys("j j j");
    assert_eq!(hs.state.list().cursor(), 3);
    hs.keys("k");
    assert_eq!(hs.state.list().cursor(), 2);
    hs.keys("down up up up up");
    assert_eq!(hs.state.list().cursor(), 0, "clamped at the top");
    hs.keys("G");
    assert_eq!(hs.state.list().cursor(), 213);
    // 214 rows, 35 lines and no headings in view: the last row sits on
    // the last line.
    assert_eq!(hs.state.list().offset(), 179, "view follows the cursor");
    hs.keys("g g");
    assert_eq!(hs.state.list().cursor(), 0);
    assert_eq!(hs.state.list().offset(), 0);
    hs.keys("space");
    assert!(hs.state.list().cursor() > 20, "space pages down");
    hs.keys("pgup");
    assert_eq!(hs.state.list().cursor(), 0);
    hs.keys("ctrl-d");
    assert_eq!(hs.state.list().cursor(), 17);
    hs.keys("ctrl-u");
    assert_eq!(hs.state.list().cursor(), 0);
}

#[test]
fn key_release_and_paste_do_nothing() {
    let mut hs = Harness::demo(160, 40);
    let mut release = key_event("j");
    release.kind = KeyEventKind::Release;
    assert!(hs.event(Event::Key(release)).is_empty());
    assert_eq!(hs.state.list().cursor(), 0);
    hs.event(Event::Paste("jjjjyyy dddd".into()));
    assert_eq!(hs.state.list().cursor(), 0);
    assert!(mutations(&hs.log).is_empty());
}

#[test]
fn open_marks_read_and_focuses_the_reader() {
    let mut hs = Harness::demo(160, 40);
    let cmds = hs.keys("enter");
    let m = mutations(&cmds);
    assert_eq!(m.len(), 1);
    match m[0] {
        Mutation::SetKeyword {
            target,
            keyword: Keyword::Seen,
            value: true,
        } => {
            assert!(matches!(&target.ids, TargetIds::Emails(ids) if ids.len() == 1));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(hs.state.focus(), Focus::Reader);
    assert!(!hs.state.reader_full(), "three panes keep the list visible");
    // The row is read now, in the same frame.
    assert!(!hs.text().lines().nth(3).unwrap().contains('\u{25E6}'));
    hs.keys("u");
    assert_eq!(hs.state.focus(), Focus::List);
}

#[test]
fn narrow_layout_opens_the_reader_full_screen() {
    let mut hs = Harness::demo(80, 24);
    hs.keys("j j enter");
    assert!(hs.state.reader_full());
    assert!(hs.text().contains("Re: Your order 48213 has shipped"));
    assert!(hs.text().contains("ORDER UPDATE"));
    // j in the reader moves to the next conversation, staying in the reader.
    hs.keys("j");
    assert!(hs.state.reader_full());
    assert_eq!(hs.state.list().cursor(), 3);
    hs.keys("esc");
    assert!(!hs.state.reader_full());
    assert_eq!(hs.state.focus(), Focus::List);
}

#[test]
fn archive_is_optimistic_and_undoable() {
    let mut hs = Harness::demo(160, 40);
    hs.keys("j");
    let archived = cursor_thread(&hs);
    let list_part = |hs: &Harness, y: u16| hs.line(y).chars().skip(24).take(73).collect::<String>();
    let next_line = list_part(&hs, 5);
    let cmds = hs.keys("y");
    let m = mutations(&cmds);
    assert!(
        matches!(m[0], Mutation::Archive(t) if t.ids == TargetIds::Threads(vec![archived.clone()]))
    );
    // Same frame: the row is gone, the cursor stays on the same index, and
    // the next conversation slid into it.
    assert_eq!(hs.state.list().cursor(), 1);
    assert_ne!(cursor_thread(&hs), archived);
    assert_eq!(list_part(&hs, 4), next_line);
    assert_eq!(toast(&hs), "Archived 1 conversation");
    assert!(hs.text().contains("z undo"));
    let inbox = hs.data.mailbox_for_role(MailboxRole::Inbox).unwrap();
    assert_eq!(hs.data.conversation_page(&inbox, 0, 1).total, 213);
    let cmds = hs.keys("z");
    assert_eq!(mutations(&cmds), vec![&Mutation::Undo]);
    assert_eq!(hs.data.conversation_page(&inbox, 0, 1).total, 214);
    hs.keys("ctrl-y");
    assert_eq!(hs.data.conversation_page(&inbox, 0, 1).total, 213);
}

#[test]
fn archive_next_and_previous() {
    let mut hs = Harness::demo(160, 40);
    hs.keys("j j j ]");
    assert_eq!(hs.state.list().cursor(), 3);
    hs.keys("[");
    assert_eq!(hs.state.list().cursor(), 2);
    let inbox = hs.data.mailbox_for_role(MailboxRole::Inbox).unwrap();
    assert_eq!(hs.data.conversation_page(&inbox, 0, 1).total, 212);
}

#[test]
fn new_mail_keeps_the_cursor_on_its_conversation() {
    let mut hs = Harness::demo(160, 40);
    hs.keys("j j");
    let before = cursor_thread(&hs);
    let inbox = hs.data.mailbox_for_role(MailboxRole::Inbox).unwrap();
    let mut mail = mejl_ui::NewEmail::new(
        "new-1",
        "t-new",
        mejl_ui::Participant::new("Theo Park", "theo.park@example.net"),
        "Just arrived",
        common::now(),
        inbox,
    );
    mail.unread = true;
    hs.data.add_email(mail);
    hs.event(Event::Msg(Msg::DataChanged(mejl_ui::DataChange {
        mailboxes: true,
        threads: vec!["t-new".into()],
        ..Default::default()
    })));
    assert_eq!(cursor_thread(&hs), before);
    assert_eq!(hs.state.list().cursor(), 3);
    assert!(hs.text().contains("Just arrived"));
    assert!(hs.text().contains("13 unread, 215 conversations"));
}

#[test]
fn selection_range_all_and_bulk_trash() {
    let mut hs = Harness::demo(160, 40);
    hs.keys("x j j X");
    assert_eq!(hs.state.list().selection().count(214), 3);
    assert!(hs.text().contains("selection: 3"));
    hs.keys("* n");
    assert_eq!(hs.state.list().selection().count(214), 0);
    hs.keys("* a x");
    assert_eq!(hs.state.list().selection().count(214), 213);
    let cmds = hs.keys("d");
    match mutations(&cmds)[0] {
        Mutation::Trash(t) => match &t.ids {
            TargetIds::AllInMailbox { except, .. } => assert_eq!(except.len(), 1),
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
    assert_eq!(toast(&hs), "Moved 213 conversations to Trash");
    assert_eq!(hs.state.list().total(), 1);
    assert!(hs.state.list().selection().is_empty(1));
}

#[test]
fn pin_and_mark_keys() {
    let mut hs = Harness::demo(160, 40);
    hs.keys("j");
    let cmds = hs.keys("s");
    assert!(matches!(
        mutations(&cmds)[0],
        Mutation::SetKeyword {
            keyword: Keyword::Flagged,
            value: false,
            ..
        }
    ));
    let cmds = hs.keys(". p");
    assert!(matches!(
        mutations(&cmds)[0],
        Mutation::SetKeyword {
            keyword: Keyword::Flagged,
            value: true,
            ..
        }
    ));
    let cmds = hs.keys(". u");
    assert!(matches!(
        mutations(&cmds)[0],
        Mutation::SetKeyword {
            keyword: Keyword::Seen,
            value: false,
            ..
        }
    ));
    // In the reader, marks act on the focused message.
    let cmds = hs.keys("tab U");
    match mutations(&cmds)[0] {
        Mutation::SetKeyword { target, .. } => {
            assert!(matches!(&target.ids, TargetIds::Emails(e) if e.len() == 1));
        }
        other => panic!("{other:?}"),
    }
    assert!(toast(&hs).contains("1 message"));
}

#[test]
fn permanent_delete_needs_trash_and_a_confirm() {
    let mut hs = Harness::demo(160, 40);
    let cmds = hs.keys("#");
    assert!(mutations(&cmds).is_empty());
    assert!(toast(&hs).contains("Trash and Spam"));
    hs.keys("g j");
    assert!(hs.text().contains("Spam"));
    hs.keys("#");
    assert!(matches!(
        hs.state.overlays().last(),
        Some(Overlay::Confirm(_))
    ));
    assert!(hs.line(38).contains("Delete 1 conversation permanently?"));
    let cmds = hs.keys("n");
    assert!(mutations(&cmds).is_empty());
    assert_eq!(toast(&hs), "Cancelled");
    hs.keys("#");
    let cmds = hs.keys("y");
    assert!(matches!(
        mutations(&cmds)[0],
        Mutation::DeletePermanently(_)
    ));
    let junk = hs.data.mailbox_for_role(MailboxRole::Junk).unwrap();
    assert_eq!(hs.data.conversation_page(&junk, 0, 10).total, 2);
}

#[test]
fn moves_refuse_their_own_mailbox() {
    let mut hs = Harness::demo(160, 40);
    hs.keys("g a");
    let cmds = hs.keys("y");
    assert!(mutations(&cmds).is_empty());
    assert_eq!(toast(&hs), "Already in Archive");
    hs.keys("g t d");
    assert!(toast(&hs).starts_with("Already in Trash"));
    hs.keys("g j !");
    assert_eq!(toast(&hs), "Already in Spam");
}

#[test]
fn prefix_popup_cancel_and_go_to() {
    let mut hs = Harness::demo(120, 36);
    hs.keys("g");
    assert_eq!(hs.state.pending().keys.len(), 1);
    assert!(hs.text().contains("g  go to"));
    assert!(hs.line(34).contains("pending: g"));
    hs.keys("esc");
    assert!(hs.state.pending().is_empty());
    assert!(!hs.text().contains("g  go to"));
    // An unlisted key cancels and does nothing else.
    let cmds = hs.keys("g y");
    assert!(mutations(&cmds).is_empty());
    assert!(hs.state.pending().is_empty());
    hs.keys("g s");
    assert!(hs.text().contains("Sent"));
    assert_eq!(
        hs.state.mailbox(),
        hs.data.mailbox_for_role(MailboxRole::Sent).as_ref()
    );
    // ? during a prefix opens help.
    hs.keys("g ?");
    assert!(matches!(hs.state.overlays().last(), Some(Overlay::Help(_))));
}

#[test]
fn help_filters_and_closes() {
    let mut hs = Harness::demo(160, 40);
    hs.keys("?");
    assert!(matches!(hs.state.overlays().last(), Some(Overlay::Help(_))));
    assert!(hs.text().contains("Keys: list"));
    hs.event(Event::Paste("arch".into()));
    let text = hs.text();
    assert!(text.contains("archive"), "{text}");
    assert!(!text.contains("Move to trash"), "{text}");
    hs.keys("backspace backspace backspace backspace q z z z");
    assert!(hs.text().contains("Nothing matches the filter"));
    hs.keys("esc");
    assert!(hs.state.overlays().is_empty());
}

#[test]
fn panes_cycle_with_tab() {
    let mut hs = Harness::demo(160, 40);
    hs.keys("tab");
    assert_eq!(hs.state.focus(), Focus::Reader);
    hs.keys("tab");
    assert_eq!(hs.state.focus(), Focus::Sidebar);
    hs.keys("shift-tab");
    assert_eq!(hs.state.focus(), Focus::Reader);
    hs.keys("tab j j enter");
    // Sidebar: two steps down from Inbox is Sent.
    assert_eq!(
        hs.state.mailbox(),
        hs.data.mailbox_for_role(MailboxRole::Sent).as_ref()
    );
    assert_eq!(hs.state.focus(), Focus::List);
}

#[test]
fn sidebar_overlay_in_two_panes() {
    let mut hs = Harness::demo(120, 36);
    hs.keys("g f");
    assert!(hs.state.sidebar_overlay());
    assert!(hs.text().contains("Hall repairs"));
    hs.keys("esc");
    assert!(!hs.state.sidebar_overlay());
    hs.keys("g f j enter");
    assert_eq!(
        hs.state.mailbox(),
        hs.data.mailbox_for_role(MailboxRole::Drafts).as_ref()
    );
    assert!(!hs.state.sidebar_overlay());
}

#[test]
fn reader_messages_folds_and_headers() {
    let mut hs = Harness::demo(160, 40);
    hs.keys("tab");
    let lines = hs.state.reader().text_lines().join("\n");
    assert!(lines.contains("12 quoted lines"), "{lines}");
    assert!(lines.contains("signature, 3 lines"));
    hs.keys("\"");
    let lines = hs.state.reader().text_lines().join("\n");
    assert!(lines.contains("> I am away that weekend"), "{lines}");
    hs.keys("\"");
    assert!(
        !hs.state
            .reader()
            .text_lines()
            .join("\n")
            .contains("> I am away")
    );
    // p focuses and expands the previous message.
    let conv = hs.state.reader().conversation().unwrap().clone();
    hs.keys("p");
    assert_eq!(hs.state.reader().focused(), 1);
    assert!(hs.state.reader().is_expanded(&conv.messages[1].email_id));
    hs.keys("e");
    assert!(!hs.state.reader().is_expanded(&conv.messages[1].email_id));
    hs.keys("E");
    assert!(
        conv.messages
            .iter()
            .all(|m| hs.state.reader().is_expanded(&m.email_id))
    );
    hs.keys("E");
    assert_eq!(
        conv.messages
            .iter()
            .filter(|m| hs.state.reader().is_expanded(&m.email_id))
            .count(),
        1
    );
    hs.keys("H");
    assert!(hs.state.reader().headers_full());
    assert!(
        hs.state
            .reader()
            .text_lines()
            .join("\n")
            .contains("List-Id: <garden-club.example.org>")
    );
}

#[test]
fn reader_scrolls_within_bounds() {
    let mut hs = Harness::demo(80, 24);
    hs.keys("j j enter");
    hs.keys("down down");
    assert_eq!(hs.state.reader().scroll(), 2);
    hs.keys("up up up");
    assert_eq!(hs.state.reader().scroll(), 0);
    hs.keys("space space space space space space space space");
    let max = hs.state.reader().text_lines().len() - 19;
    assert_eq!(hs.state.reader().scroll(), max);
    assert!(
        hs.text()
            .contains("[3] https://shop.example.com/preferences")
    );
}

#[test]
fn former_placeholders_open_their_screens() {
    use mejl_ui::Screen;
    let mut hs = Harness::demo(160, 40);
    for (keys, screen) in [
        ("c", Screen::Compose),
        ("/", Screen::Search),
        ("g r", Screen::Rules),
        ("g m", Screen::Masked),
        ("g ,", Screen::Settings),
    ] {
        hs.keys(keys);
        assert_eq!(*hs.state.screen(), screen, "{keys}");
        // Leave again without side effects.
        let cmds = hs.keys("esc esc");
        assert_eq!(*hs.state.screen(), Screen::Mail, "{keys}: {cmds:?}");
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c, Cmd::Mutate(_) | Cmd::Send(_))),
            "{keys}: {cmds:?}"
        );
    }
    let cmds = hs.keys("b 3");
    assert!(toast(&hs).starts_with("Snooze is web only"));
    assert!(cmds.iter().all(|c| matches!(c, Cmd::WakeAt(_))));
    hs.keys(":");
    assert!(matches!(
        hs.state.overlays().last(),
        Some(mejl_ui::Overlay::Picker(_))
    ));
}

#[test]
fn toasts_expire_with_the_clock() {
    let mut hs = Harness::demo(160, 40);
    let cmds = hs.keys("b 1");
    let wake = cmds.iter().find_map(|c| match c {
        Cmd::WakeAt(t) => Some(*t),
        _ => None,
    });
    assert_eq!(wake, hs.state.next_wakeup());
    hs.clock.advance(SignedDuration::from_secs(6));
    hs.event(Event::Msg(Msg::Tick));
    assert!(hs.state.toast().is_none());
    assert!(hs.state.next_wakeup().is_none());
}

#[test]
fn quit_refresh_and_perf() {
    let mut hs = Harness::demo(160, 40);
    assert!(hs.keys("ctrl-l").contains(&Cmd::RefreshNow));
    hs.keys("tab");
    // q backs out of the reader first, then quits.
    assert!(!hs.keys("q").contains(&Cmd::Quit));
    assert!(hs.keys("q").contains(&Cmd::Quit));
    assert!(hs.keys("Q").contains(&Cmd::Quit));
    for ms in [3u64, 5, 7, 40] {
        hs.event(Event::Msg(Msg::Perf(mejl_ui::FrameSample {
            input_to_flush: std::time::Duration::from_millis(ms),
        })));
    }
    hs.keys("g p");
    assert!(matches!(hs.state.overlays().last(), Some(Overlay::Perf)));
    let text = hs.text();
    assert!(text.contains("Frame timings"));
    assert!(text.contains("4 frames in the window"), "{text}");
    assert!(text.contains("p99 40.0 ms"), "{text}");
    hs.keys("x");
    assert!(hs.state.overlays().is_empty());
}

#[test]
fn vim_preset_counts_archive_and_repeat() {
    let config = UiConfig {
        keymap: mejl_ui::Keymap::preset(Preset::Vim),
        ..UiConfig::default()
    };
    let mut hs = Harness::with(mejl_ui::MemoryData::demo(common::now()), config, 160, 40);
    hs.keys("3 j");
    assert_eq!(hs.state.list().cursor(), 3);
    let cmds = hs.keys("a");
    assert!(matches!(mutations(&cmds)[0], Mutation::Archive(_)));
    let cmds = hs.keys(".");
    assert!(matches!(mutations(&cmds)[0], Mutation::Archive(_)));
    let cmds = hs.keys("u");
    assert_eq!(mutations(&cmds), vec![&Mutation::Undo]);
    assert!(hs.text().contains("vim preset"));
    // In the reader, j scrolls and J moves on.
    let at = hs.state.list().cursor();
    hs.keys("tab j");
    assert_eq!(hs.state.list().cursor(), at);
    hs.keys("J");
    assert_eq!(hs.state.list().cursor(), at + 1);
}

#[test]
fn user_overrides_apply() {
    let overrides = mejl_ui::KeyOverrides::from_toml(
        r#"
        [keys.list]
        "e" = "archive"
        "y" = "none"
        "#,
    )
    .unwrap();
    let keymap = mejl_ui::Keymap::with_overrides(Preset::Fastmail, &overrides).unwrap();
    let config = UiConfig {
        keymap,
        ..UiConfig::default()
    };
    let mut hs = Harness::with(mejl_ui::MemoryData::demo(common::now()), config, 160, 40);
    assert!(mutations(&hs.keys("y")).is_empty());
    assert!(matches!(mutations(&hs.keys("e"))[0], Mutation::Archive(_)));
    // The hint bar shows the new key.
    assert!(hs.line(39).contains("e archive"), "{}", hs.line(39));
}

#[test]
fn ctrl_letters_work_with_legacy_and_kitty_encoding() {
    let mut hs = Harness::demo(160, 40);
    let mut ev = key_event("ctrl-d");
    hs.event(Event::Key(ev));
    assert_eq!(hs.state.list().cursor(), 17);
    ev.modifiers = KeyModifiers::CONTROL;
    ev.code = crossterm::event::KeyCode::Char('u');
    hs.event(Event::Key(ev));
    assert_eq!(hs.state.list().cursor(), 0);
}
