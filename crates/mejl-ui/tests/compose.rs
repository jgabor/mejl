//! Compose: opening, fields, completion, replies, drafts, sending with
//! undo, and the screen at the mock-up sizes.

mod common;

use common::Harness;
use jiff::SignedDuration;
use mejl_ui::compose::Field;
use mejl_ui::{
    BlobId, Cmd, ComposeKind, DraftId, DraftView, Event, MailData, Msg, OutgoingAttachment,
    OutgoingMessage, Overlay, Participant, PushState, Screen, SendOutcome, SendReport, SyncStatus,
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

const BODY: &str = "Hi,

We would like to book the lakeside cabin from Friday 14 November to
Sunday 16 November, for two adults.

A few questions:

- Is late check-in after 21:00 possible?
- Can we bring a small dog?

The floor plan you sent last year is attached. We would like the same
layout if it is free.

Thanks,
Mira";

/// The compose mock-up: a new message to the cabins, with a body, an
/// attachment and a saved draft.
fn mockup(w: u16, h: u16) -> Harness {
    let mut hs = synced(w, h);
    hs.keys("c");
    typed(&mut hs, "bookings@cabins.example.net");
    hs.keys("enter tab tab");
    typed(&mut hs, "Booking for 14 to 16 November");
    hs.keys("tab");
    hs.event(Event::Paste(BODY.into()));
    hs.keys("ctrl-o a");
    typed(&mut hs, "~/floorplan-2025.pdf");
    hs.keys("enter");
    let draft = hs.state.draft_id().unwrap().clone();
    hs.event(Event::Msg(Msg::Attached {
        draft_id: draft.clone(),
        path: "~/floorplan-2025.pdf".into(),
        result: Ok(OutgoingAttachment {
            blob_id: BlobId::from("b1"),
            name: "floorplan-2025.pdf".into(),
            mime_type: "application/pdf".into(),
            size: 212 * 1024,
        }),
    }));
    hs.clock.advance(SignedDuration::from_secs(2));
    let cmds = hs.event(Event::Msg(Msg::Tick));
    assert!(cmds.iter().any(|c| matches!(c, Cmd::SaveDraft(_))));
    hs.event(Event::Msg(Msg::DraftSaved {
        draft_id: draft,
        error: None,
    }));
    // The toast from attaching expires as the saved time reaches 3 s.
    hs.clock.advance(SignedDuration::from_secs(3));
    hs.event(Event::Msg(Msg::Tick));
    hs
}

#[test]
fn compose_120x36() {
    let hs = mockup(120, 36);
    insta::assert_snapshot!(hs.text());
}

#[test]
fn compose_80x24() {
    let hs = mockup(80, 24);
    insta::assert_snapshot!(hs.text());
}

#[test]
fn compose_160x40() {
    let hs = mockup(160, 40);
    insta::assert_snapshot!(hs.text());
}

#[test]
fn compose_from_picker_120x36() {
    let mut hs = mockup(120, 36);
    hs.keys("esc ctrl-o f");
    insta::assert_snapshot!(hs.text());
}

#[test]
fn new_message_starts_in_to_with_the_signature() {
    let mut hs = synced(120, 36);
    hs.keys("c");
    assert_eq!(*hs.state.screen(), Screen::Compose);
    let c = hs.state.compose_body().unwrap();
    assert_eq!(c, "\n\n-- \nMira\n");
    assert!(hs.text().contains("New message"));
    assert!(hs.text().contains("not saved yet"));
}

#[test]
fn completion_offers_contacts_and_tab_accepts() {
    let mut hs = synced(120, 36);
    hs.keys("c");
    typed(&mut hs, "jun");
    assert!(
        hs.text()
            .contains("Juniper Tran <juniper@garden.example.com>")
    );
    hs.keys("tab");
    let text = hs.text();
    assert!(
        text.contains("To       Juniper Tran <juniper@garden.example.com>"),
        "{text}"
    );
    // Typing an invalid address and leaving the field keeps it with a
    // reason.
    typed(&mut hs, "owen");
    hs.keys("esc");
    assert!(hs.text().contains("owen"));
}

#[test]
fn invalid_addresses_stop_send() {
    let mut hs = synced(120, 36);
    hs.keys("c");
    typed(&mut hs, "not-an-address");
    let cmds = hs.keys("ctrl-o s");
    assert!(!cmds.iter().any(|c| matches!(c, Cmd::Send(_))));
    assert!(hs.state.toast().unwrap().text.contains("needs an @"));
    assert_eq!(*hs.state.screen(), Screen::Compose);
    // No recipients at all also stops it.
    hs.keys("ctrl-u");
    let cmds = hs.keys("ctrl-o s");
    assert!(!cmds.iter().any(|c| matches!(c, Cmd::Send(_))));
    assert!(
        hs.state
            .toast()
            .unwrap()
            .text
            .contains("at least one recipient")
    );
}

#[test]
fn send_with_undo_countdown_and_cancel() {
    let mut hs = synced(120, 36);
    hs.keys("c");
    typed(&mut hs, "ada@example.com");
    hs.keys("tab tab");
    typed(&mut hs, "Hello");
    let cmds = hs.keys("ctrl-o s");
    let sent = cmds
        .iter()
        .find_map(|c| match c {
            Cmd::Send(m) => Some(m.clone()),
            _ => None,
        })
        .expect("a send");
    assert_eq!(
        sent.to,
        vec![Participant {
            name: None,
            email: "ada@example.com".into()
        }]
    );
    assert_eq!(sent.subject, "Hello");
    assert_eq!(sent.from.email, "mira@example.org");
    assert_eq!(sent.identity_id.as_deref(), Some("i1"));
    assert_eq!(*hs.state.screen(), Screen::Mail);
    assert!(hs.line(34).contains("Sending in 10 s"), "{}", hs.line(34));
    assert!(hs.line(34).contains("z undo"));
    hs.clock.advance(SignedDuration::from_millis(3_200));
    hs.event(Event::Msg(Msg::Tick));
    assert!(hs.line(34).contains("Sending in 7 s"), "{}", hs.line(34));
    assert!(hs.state.next_wakeup().is_some());
    let cmds = hs.keys("z");
    assert!(cmds.contains(&Cmd::CancelSend(sent.draft_id.clone())));
    assert!(
        !cmds.iter().any(|c| matches!(c, Cmd::Mutate(_))),
        "undo cancels the send, not a mail change"
    );
    hs.event(Event::Msg(Msg::Send(SendReport {
        draft_id: sent.draft_id.clone(),
        outcome: SendOutcome::Cancelled {
            message: sent.clone(),
        },
    })));
    assert_eq!(
        *hs.state.screen(),
        Screen::Compose,
        "the message comes back"
    );
    assert_eq!(hs.state.draft_id(), Some(&sent.draft_id));
    assert!(hs.text().contains("ada@example.com"));
}

#[test]
fn send_outcomes() {
    let mut hs = synced(120, 36);
    hs.keys("c");
    typed(&mut hs, "ada@example.com");
    hs.keys("tab tab");
    typed(&mut hs, "Hi");
    let cmds = hs.keys("ctrl-o s");
    let Some(Cmd::Send(m)) = cmds.iter().find(|c| matches!(c, Cmd::Send(_))).cloned() else {
        panic!("no send");
    };
    let at = mejl_ui::Clock::now(&*hs.clock)
        .checked_add(SignedDuration::from_secs(20))
        .unwrap();
    hs.event(Event::Msg(Msg::Send(SendReport {
        draft_id: m.draft_id.clone(),
        outcome: SendOutcome::Scheduled { at },
    })));
    assert_eq!(hs.state.toast().unwrap().expires, at);
    hs.event(Event::Msg(Msg::Send(SendReport {
        draft_id: m.draft_id.clone(),
        outcome: SendOutcome::Sent,
    })));
    assert!(hs.state.toast().unwrap().text.contains("Sent \"Hi\""));
    // Undo now is an ordinary undo.
    let cmds = hs.keys("z");
    assert!(
        cmds.iter()
            .any(|c| matches!(c, Cmd::Mutate(mejl_ui::Mutation::Undo)))
    );
    assert!(hs.state.toast().unwrap().text.contains("Undid"));
    hs.event(Event::Msg(Msg::Send(SendReport {
        draft_id: m.draft_id.clone(),
        outcome: SendOutcome::Failed {
            error: "quota".into(),
            message: m.clone(),
        },
    })));
    assert_eq!(*hs.state.screen(), Screen::Compose);
    assert!(hs.state.toast().unwrap().text.contains("failed: quota"));
    hs.keys("esc esc");
    hs.event(Event::Msg(Msg::Send(SendReport {
        draft_id: m.draft_id.clone(),
        outcome: SendOutcome::TooLate,
    })));
    assert!(hs.state.toast().unwrap().text.contains("Too late"));
}

#[test]
fn missing_subject_and_attachment_ask_first() {
    let mut hs = synced(120, 36);
    hs.keys("c");
    typed(&mut hs, "ada@example.com");
    let cmds = hs.keys("ctrl-o s");
    assert!(!cmds.iter().any(|c| matches!(c, Cmd::Send(_))));
    assert!(matches!(
        hs.state.overlays().last(),
        Some(Overlay::Confirm(_))
    ));
    assert!(hs.text().contains("The subject is empty. Send anyway?"));
    let cmds = hs.keys("y");
    assert!(cmds.iter().any(|c| matches!(c, Cmd::Send(_))));
    // Mentioning an attachment without one asks too.
    hs.keys("c");
    typed(&mut hs, "ada@example.com");
    hs.keys("tab tab");
    typed(&mut hs, "Plan");
    hs.keys("tab");
    typed(&mut hs, "The plan is attached.");
    let cmds = hs.keys("ctrl-o s");
    assert!(!cmds.iter().any(|c| matches!(c, Cmd::Send(_))));
    assert!(hs.text().contains("nothing is attached"));
    let cmds = hs.keys("n");
    assert!(!cmds.iter().any(|c| matches!(c, Cmd::Send(_))));
    assert_eq!(*hs.state.screen(), Screen::Compose);
}

#[test]
fn reply_quotes_the_focused_message() {
    let mut hs = synced(160, 40);
    hs.keys("enter r");
    assert_eq!(*hs.state.screen(), Screen::Compose);
    let body = hs.state.compose_body().unwrap();
    assert!(body.contains("Juniper Tran wrote:\n> "), "{body}");
    assert!(body.contains("\n-- \nMira\n"), "{body}");
    let text = hs.text();
    assert!(
        text.contains("Subject  Re: Garden club plot rota"),
        "{text}"
    );
    assert!(text.contains("To       Juniper Tran <"), "{text}");
    let cmds = hs.keys("ctrl-s");
    let Some(Cmd::SaveDraft(m)) = cmds.iter().find(|c| matches!(c, Cmd::SaveDraft(_))) else {
        panic!("{cmds:?}");
    };
    assert_eq!(m.kind, ComposeKind::Reply);
    assert_eq!(m.in_reply_to.len(), 1);
    assert!(!m.references.is_empty());
    assert!(m.original.is_some());
}

#[test]
fn reply_all_and_forward() {
    let mut hs = synced(160, 40);
    hs.keys("a");
    let cmds = hs.keys("ctrl-s");
    let Some(Cmd::SaveDraft(m)) = cmds
        .iter()
        .find(|c| matches!(c, Cmd::SaveDraft(_)))
        .cloned()
    else {
        panic!("{cmds:?}");
    };
    assert_eq!(m.kind, ComposeKind::ReplyAll);
    assert!(!m.to.iter().any(|p| p.email == "mira@example.org"), "{m:?}");
    hs.keys("esc esc");
    hs.keys("f");
    let text = hs.text();
    assert!(
        text.contains("Subject  Fwd: Re: Garden club plot rota"),
        "{text}"
    );
    assert!(
        hs.state
            .compose_body()
            .unwrap()
            .contains("---------- Forwarded message ----------")
    );
}

#[test]
fn reply_waits_for_an_uncached_body() {
    let mut hs = synced(160, 40);
    let conv = hs
        .data
        .conversation(&hs.state.list().cursor_thread().unwrap().clone())
        .unwrap();
    let last = conv.messages.last().unwrap().email_id.clone();
    let body = hs.data.remove_body(&last).unwrap();
    let cmds = hs.keys("r");
    assert!(cmds.contains(&Cmd::FetchBody(last.clone())), "{cmds:?}");
    assert!(!hs.state.compose_body().unwrap().contains("wrote:"));
    hs.data.add_body(body);
    hs.event(Event::Msg(Msg::BodyFetched(last)));
    assert!(hs.state.compose_body().unwrap().contains("wrote:"));
}

#[test]
fn leaving_saves_the_draft_and_discard_asks() {
    let mut hs = synced(120, 36);
    hs.keys("c");
    typed(&mut hs, "ada@example.com");
    let cmds = hs.keys("esc esc");
    assert!(cmds.iter().any(|c| matches!(c, Cmd::SaveDraft(_))));
    assert_eq!(*hs.state.screen(), Screen::Mail);
    assert!(hs.state.toast().unwrap().text.contains("Draft saved"));
    // A blank message is not kept.
    hs.keys("c");
    hs.keys("ctrl-o d");
    assert!(hs.text().contains("Discard this draft?"));
    let cmds = hs.keys("y");
    assert!(cmds.iter().any(|c| matches!(c, Cmd::DeleteDraft(_))));
    assert_eq!(*hs.state.screen(), Screen::Mail);
    hs.keys("c");
    let cmds = hs.keys("esc esc");
    assert!(!cmds.iter().any(|c| matches!(c, Cmd::SaveDraft(_))));
}

#[test]
fn autosave_waits_for_a_pause_and_reports_errors() {
    let mut hs = synced(120, 36);
    hs.keys("c");
    let cmds = typed(&mut hs, "a");
    assert!(cmds.iter().any(|c| matches!(c, Cmd::WakeAt(_))));
    assert!(!cmds.iter().any(|c| matches!(c, Cmd::SaveDraft(_))));
    hs.clock.advance(SignedDuration::from_secs(1));
    let cmds = hs.event(Event::Msg(Msg::Tick));
    assert!(!cmds.iter().any(|c| matches!(c, Cmd::SaveDraft(_))));
    hs.clock.advance(SignedDuration::from_secs(2));
    let cmds = hs.event(Event::Msg(Msg::Tick));
    assert!(cmds.iter().any(|c| matches!(c, Cmd::SaveDraft(_))));
    assert!(hs.text().contains("Saving the draft"));
    let id = hs.state.draft_id().unwrap().clone();
    hs.event(Event::Msg(Msg::DraftSaved {
        draft_id: id,
        error: Some("disk full".into()),
    }));
    assert!(hs.text().contains("! Draft not saved: disk full"));
}

#[test]
fn external_editor_round_trip() {
    let mut hs = synced(120, 36);
    hs.keys("c");
    let cmds = hs.keys("ctrl-o e");
    let Some(Cmd::OpenEditor { draft_id, text }) = cmds
        .iter()
        .find(|c| matches!(c, Cmd::OpenEditor { .. }))
        .cloned()
    else {
        panic!("{cmds:?}");
    };
    assert_eq!(text, "\n\n-- \nMira\n");
    hs.event(Event::Msg(Msg::EditorDone {
        draft_id: draft_id.clone(),
        result: Ok("Written in vim\n".into()),
    }));
    assert_eq!(hs.state.compose_body().unwrap(), "Written in vim\n");
    hs.event(Event::Msg(Msg::EditorDone {
        draft_id,
        result: Err("vim not found".into()),
    }));
    assert!(hs.state.toast().unwrap().text.contains("vim not found"));
}

#[test]
fn attachments_add_fail_and_remove() {
    let mut hs = synced(120, 36);
    hs.keys("c ctrl-o a");
    assert!(hs.line(34).contains("Attach file:"));
    hs.event(Event::Paste("/tmp/report.pdf".into()));
    let cmds = hs.keys("enter");
    let id = hs.state.draft_id().unwrap().clone();
    assert!(cmds.contains(&Cmd::AttachFile {
        draft_id: id.clone(),
        path: "/tmp/report.pdf".into()
    }));
    assert!(hs.text().contains("/tmp/report.pdf uploading"));
    hs.event(Event::Msg(Msg::Attached {
        draft_id: id.clone(),
        path: "/tmp/report.pdf".into(),
        result: Err("no such file".into()),
    }));
    assert!(hs.state.toast().unwrap().text.contains("no such file"));
    assert!(!hs.text().contains("uploading"));
    hs.event(Event::Msg(Msg::Attached {
        draft_id: id,
        path: "/tmp/report.pdf".into(),
        result: Ok(OutgoingAttachment {
            blob_id: BlobId::from("b9"),
            name: "report.pdf".into(),
            mime_type: "application/pdf".into(),
            size: 2048,
        }),
    }));
    assert!(hs.text().contains("report.pdf  2 KB"));
    // Focus the attachment row and remove it.
    hs.keys("shift-tab shift-tab delete");
    assert!(!hs.text().contains("report.pdf  2 KB"), "{}", hs.text());
}

#[test]
fn from_picker_lists_identities_and_masks() {
    let mut hs = synced(120, 36);
    hs.keys("c");
    typed(&mut hs, "bookings@cabins.example.net");
    hs.keys("enter ctrl-o f");
    let text = hs.text();
    assert!(
        text.contains("Mira at work <mira.holt@work.example.com>"),
        "{text}"
    );
    assert!(text.contains("+ New masked email for cabins.example.net"));
    assert!(text.contains("kettle.fern4821@example.org"));
    typed(&mut hs, "kettle");
    hs.keys("enter");
    assert!(
        hs.text()
            .contains("From     Mira Holt <kettle.fern4821@example.org>")
    );
    let cmds = hs.keys("ctrl-s");
    let Some(Cmd::SaveDraft(m)) = cmds.iter().find(|c| matches!(c, Cmd::SaveDraft(_))) else {
        panic!();
    };
    assert_eq!(m.from.email, "kettle.fern4821@example.org");
    // A new mask for the recipient's site.
    let cmds = hs.keys("ctrl-o m");
    assert!(cmds.is_empty() || !cmds.iter().any(|c| matches!(c, Cmd::MaskedCreate { .. })));
    assert!(hs.line(34).contains("cabins.example.net"));
    typed(&mut hs, "Lakeside cabins");
    let cmds = hs.keys("enter");
    let draft = hs.state.draft_id().cloned();
    assert!(cmds.contains(&Cmd::MaskedCreate {
        for_domain: "cabins.example.net".into(),
        description: "Lakeside cabins".into(),
        draft_id: draft.clone(),
    }));
    let mut mask = hs.data.masked_emails()[0].clone();
    mask.email = "fresh.cabin0001@example.org".into();
    hs.event(Event::Msg(Msg::MaskedCreated {
        draft_id: draft,
        result: Ok(mask),
    }));
    assert!(hs.text().contains("fresh.cabin0001@example.org"));
}

#[test]
fn bcc_toggle_and_mouse_focus() {
    let mut hs = synced(120, 36);
    hs.keys("c ctrl-o b");
    assert!(hs.text().contains("│ Bcc"));
    let (x, y) = hs.find("Subject").unwrap();
    hs.click(x, y);
    typed(&mut hs, "Clicked");
    assert!(hs.text().contains("Subject  Clicked"));
    let (x, y) = hs.find("-- ").unwrap();
    hs.click(x, y);
    typed(&mut hs, "x");
    assert!(hs.state.compose_body().unwrap().starts_with('x'));
}

#[test]
fn drafts_resume_from_the_palette_and_the_drafts_mailbox() {
    let mut hs = synced(160, 40);
    let message = OutgoingMessage {
        draft_id: DraftId::from("d-old"),
        to: vec![Participant::new("Ada", "ada@example.com")],
        subject: "Old draft".into(),
        text_body: "Unfinished".into(),
        ..OutgoingMessage::default()
    };
    hs.data.add_draft(DraftView {
        message: message.clone(),
        server_email_id: None,
        updated_at: common::now(),
    });
    hs.keys(":");
    typed(&mut hs, "resume");
    hs.keys("enter");
    assert_eq!(*hs.state.screen(), Screen::Compose);
    assert_eq!(hs.state.draft_id(), Some(&DraftId::from("d-old")));
    assert_eq!(hs.state.compose_body().unwrap(), "Unfinished");
}

#[test]
fn compose_keys_are_in_the_keymap() {
    use mejl_ui::keymap::{Context, Keymap, Pending, Resolution};
    use mejl_ui::{Action, Key};
    let map = Keymap::default();
    for (keys, action) in [
        ("ctrl-o s", Action::Send),
        ("ctrl-s", Action::SaveDraft),
        ("ctrl-o a", Action::Attach),
        ("ctrl-o e", Action::ExternalEditor),
        ("ctrl-o d", Action::DiscardDraft),
        ("ctrl-o b", Action::ToggleBcc),
        ("ctrl-o f", Action::PickFrom),
        ("ctrl-o m", Action::NewMaskFrom),
        ("ctrl-o :", Action::Palette),
        ("tab", Action::NextField),
        ("shift-tab", Action::PrevField),
        ("esc", Action::LeaveField),
    ] {
        let mut pending = Pending::default();
        let mut last = Resolution::Unbound;
        for k in keys.split_whitespace() {
            let key: Key = k.parse().unwrap();
            last = map.feed(&mut pending, &[Context::Compose], key);
        }
        assert_eq!(
            last,
            Resolution::Action {
                action,
                count: None
            },
            "{keys}"
        );
    }
    // Plain letters are not bound, so they type.
    let mut pending = Pending::default();
    assert_eq!(
        map.feed(&mut pending, &[Context::Compose], "x".parse().unwrap()),
        Resolution::Unbound
    );
}

#[test]
fn which_key_shows_the_compose_prefix() {
    let mut hs = synced(120, 36);
    hs.keys("c ctrl-o");
    let text = hs.text();
    assert!(text.contains("^o  compose"), "{text}");
    assert!(text.contains("s  send"));
    hs.keys("esc");
    typed(&mut hs, "ok@example.com");
    assert!(hs.text().contains("ok@example.com"));
}

#[test]
fn field_order_skips_hidden_bcc() {
    let mut hs = synced(120, 36);
    hs.keys("c");
    let order: Vec<Field> = (0..7)
        .map(|_| {
            hs.keys("tab");
            hs.state.compose_field().unwrap()
        })
        .collect();
    assert_eq!(
        order,
        [
            Field::Cc,
            Field::Subject,
            Field::Body,
            Field::Attachments,
            Field::From,
            Field::To,
            Field::Cc
        ]
    );
}

#[test]
fn quit_saves_the_draft_first() {
    let mut hs = synced(120, 36);
    hs.keys("c");
    typed(&mut hs, "ada@example.com");
    let cmds = hs.keys("esc Q");
    let save = cmds.iter().position(|c| matches!(c, Cmd::SaveDraft(_)));
    let quit = cmds.iter().position(|c| matches!(c, Cmd::Quit));
    assert!(save.is_some() && quit.is_some() && save < quit, "{cmds:?}");
    // A compose left open behind search is saved too.
    let mut hs = synced(120, 36);
    hs.keys("c");
    typed(&mut hs, "ada@example.com");
    hs.keys("esc /");
    assert_eq!(*hs.state.screen(), Screen::Search);
    hs.keys("esc");
    let cmds = hs.keys("q");
    assert!(
        cmds.iter().any(|c| matches!(c, Cmd::SaveDraft(_))),
        "{cmds:?}"
    );
    assert!(cmds.contains(&Cmd::Quit));
}

#[test]
fn closing_a_resumed_body_only_draft_keeps_it() {
    let mut hs = synced(160, 40);
    hs.data.add_draft(DraftView {
        message: OutgoingMessage {
            draft_id: DraftId::from("d-body"),
            text_body: "Just a note to self".into(),
            ..OutgoingMessage::default()
        },
        server_email_id: None,
        updated_at: common::now(),
    });
    hs.keys(":");
    typed(&mut hs, "resume");
    hs.keys("enter");
    assert_eq!(hs.state.draft_id(), Some(&DraftId::from("d-body")));
    let cmds = hs.keys("esc esc");
    assert!(
        !cmds.iter().any(|c| matches!(c, Cmd::DeleteDraft(_))),
        "{cmds:?}"
    );
}

#[test]
fn the_countdown_survives_another_notice() {
    let mut hs = synced(120, 36);
    hs.keys("c");
    typed(&mut hs, "ada@example.com");
    hs.keys("tab tab");
    typed(&mut hs, "Hi");
    let cmds = hs.keys("ctrl-o s");
    let Some(Cmd::Send(m)) = cmds.iter().find(|c| matches!(c, Cmd::Send(_))).cloned() else {
        panic!("{cmds:?}");
    };
    // Another notice replaces the countdown; z then undoes that change.
    hs.event(Event::Msg(Msg::Error("sync hiccup".into())));
    assert!(hs.line(34).contains("sync hiccup"));
    let cmds = hs.keys("z");
    assert!(!cmds.contains(&Cmd::CancelSend(m.draft_id.clone())));
    // Once that notice is gone the countdown is back, and z cancels.
    hs.clock.advance(SignedDuration::from_secs(5));
    hs.event(Event::Msg(Msg::Tick));
    assert!(hs.line(34).contains("Sending in"), "{}", hs.line(34));
    let cmds = hs.keys("z");
    assert!(
        cmds.contains(&Cmd::CancelSend(m.draft_id.clone())),
        "{cmds:?}"
    );
}
