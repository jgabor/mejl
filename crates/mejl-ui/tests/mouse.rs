//! Mouse: hit testing against the last frame, and the actions it triggers.

mod common;

use common::Harness;
use crossterm::event::{MouseButton, MouseEventKind};
use jiff::SignedDuration;
use mejl_ui::{Cmd, Focus, HitTarget, MailboxRole, Overlay, Pane};

fn at(hs: &Harness, needle: &str) -> (u16, u16) {
    hs.find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not on screen:\n{}", hs.text()))
}

#[test]
fn hit_map_covers_the_mail_screen() {
    let hs = Harness::demo(160, 40);
    let map = hs.state.hit_map();
    assert_eq!(map.hit(30, 3), Some(&HitTarget::ListRow(0)));
    assert_eq!(map.hit(24, 4), Some(&HitTarget::ListMarker(1)));
    assert_eq!(
        map.hit(97, 10),
        Some(&HitTarget::Splitter(mejl_ui::layout::Splitter::ListReader))
    );
    assert_eq!(
        map.hit(23, 10),
        Some(&HitTarget::Splitter(mejl_ui::layout::Splitter::SidebarList))
    );
    assert_eq!(map.hit(5, 3), Some(&HitTarget::SidebarRow(1)));
    assert!(matches!(
        map.hit(120, 30),
        Some(HitTarget::Pane(Pane::Reader))
    ));
    assert!(matches!(
        map.hit(3, 39),
        Some(HitTarget::Action(mejl_ui::Action::Next))
    ));
    assert_eq!(map.hit(4, 0), None, "the brand tag is not clickable");
}

#[test]
fn click_selects_and_previews_double_click_opens() {
    let mut hs = Harness::demo(160, 40);
    hs.click(40, 5);
    assert_eq!(hs.state.list().cursor(), 2);
    assert_eq!(hs.state.focus(), Focus::List);
    assert!(
        hs.text().contains("Re: Your order 48213 has shipped ─"),
        "preview title"
    );
    // A second click on the same cell within the interval opens.
    let cmds = hs.click(40, 5);
    assert_eq!(hs.state.focus(), Focus::Reader);
    assert!(
        cmds.iter().any(|c| matches!(c, Cmd::Mutate(_))),
        "opening marks read"
    );
    // Clicks far apart in time are two single clicks.
    let mut hs = Harness::demo(80, 24);
    hs.click(40, 4);
    hs.clock.advance(SignedDuration::from_millis(900));
    hs.click(40, 4);
    assert!(!hs.state.reader_full());
    hs.clock.advance(SignedDuration::from_millis(900));
    hs.click(40, 4);
    hs.click(40, 4);
    assert!(
        hs.state.reader_full(),
        "double click opens full screen when narrow"
    );
}

#[test]
fn marker_column_toggles_selection() {
    let mut hs = Harness::demo(160, 40);
    hs.click(24, 4);
    assert_eq!(hs.state.list().selection().count(214), 1);
    // The selection column appears once something is selected.
    assert!(hs.line(4).contains('\u{2713}'), "{}", hs.line(4));
    hs.clock.advance(SignedDuration::from_secs(1));
    hs.click(24, 4);
    assert_eq!(hs.state.list().selection().count(214), 0);
}

#[test]
fn wheel_scrolls_the_pane_under_the_pointer() {
    let mut hs = Harness::demo(160, 40);
    hs.mouse(MouseEventKind::ScrollDown, 50, 10);
    assert_eq!(hs.state.list().offset(), 3);
    assert_eq!(hs.state.list().cursor(), 3, "the cursor is kept in view");
    hs.mouse(MouseEventKind::ScrollUp, 50, 10);
    assert_eq!(hs.state.list().offset(), 0);
    // Over the reader, the reader scrolls and the list does not.
    let mut hs = Harness::demo(160, 16);
    hs.mouse(MouseEventKind::ScrollDown, 120, 8);
    assert_eq!(hs.state.reader().scroll(), 3);
    assert_eq!(hs.state.list().offset(), 0);
}

#[test]
fn dragging_a_border_resizes_and_saves() {
    let mut hs = Harness::demo(160, 40);
    hs.mouse(MouseEventKind::Down(MouseButton::Left), 97, 10);
    hs.mouse(MouseEventKind::Drag(MouseButton::Left), 80, 10);
    let cmds = hs.mouse(MouseEventKind::Up(MouseButton::Left), 80, 10);
    let saved = cmds.iter().find_map(|c| match c {
        Cmd::SaveLayout(p) => Some(*p),
        _ => None,
    });
    assert!(saved.is_some_and(|p| p.list_ratio.is_some()));
    assert_eq!(hs.state.layout().separators[1].0, 80);
    assert_eq!(hs.buffer()[(80, 1)].symbol(), "\u{252C}");
    // Sidebar border.
    hs.mouse(MouseEventKind::Down(MouseButton::Left), 23, 10);
    hs.mouse(MouseEventKind::Drag(MouseButton::Left), 31, 10);
    hs.mouse(MouseEventKind::Up(MouseButton::Left), 31, 10);
    assert_eq!(hs.state.pane_sizes().sidebar, 30);
    // Moving without a button held does nothing.
    let before = hs.state.pane_sizes();
    hs.mouse(MouseEventKind::Drag(MouseButton::Left), 60, 10);
    assert_eq!(hs.state.pane_sizes(), before);
}

#[test]
fn hint_bar_and_top_line_run_actions() {
    let mut hs = Harness::demo(160, 40);
    let (x, _) = hs.find("y archive").unwrap();
    let cmds = hs.click(x + 2, 39);
    assert!(
        cmds.iter()
            .any(|c| matches!(c, Cmd::Mutate(mejl_ui::Mutation::Archive(_))))
    );
    let (x, y) = at(&hs, "? help");
    hs.click(x, y);
    assert!(matches!(hs.state.overlays().last(), Some(Overlay::Help(_))));
    // A click outside the help box closes it.
    hs.click(1, 1);
    assert!(hs.state.overlays().is_empty());
    // "g go" opens the prefix popup.
    let (x, _) = hs.find("g go").unwrap();
    hs.click(x, 39);
    assert_eq!(hs.state.pending().keys.len(), 1);
    assert!(hs.text().contains("g  go to"));
}

#[test]
fn sidebar_click_switches_mailbox() {
    let mut hs = Harness::demo(160, 40);
    let (x, y) = at(&hs, "  Garden club");
    hs.click(x, y);
    assert!(hs.text().contains("Compost bins for plot 9"));
    let (x, y) = at(&hs, "Rules ");
    hs.click(x, y);
    assert_eq!(*hs.state.screen(), mejl_ui::Screen::Rules);
}

#[test]
fn which_key_entries_are_clickable() {
    let mut hs = Harness::demo(120, 36);
    hs.keys("g");
    let (x, y) = at(&hs, "Trash  ");
    hs.click(x, y);
    assert_eq!(
        hs.state.mailbox(),
        hs.data.mailbox_for_role(MailboxRole::Trash).as_ref()
    );
    assert!(hs.state.pending().is_empty());
    // Any other click cancels the prefix without doing anything.
    hs.keys("g");
    hs.click(10, 10);
    assert!(hs.state.pending().is_empty());
    assert_eq!(
        hs.state.mailbox(),
        hs.data.mailbox_for_role(MailboxRole::Trash).as_ref()
    );
}

#[test]
fn mailbox_title_opens_the_sidebar_overlay() {
    let mut hs = Harness::demo(120, 36);
    let (x, y) = at(&hs, "Inbox \u{25BE}");
    hs.click(x, y);
    assert!(hs.state.sidebar_overlay());
    assert_eq!(hs.state.focus(), Focus::Sidebar);
    // Clicking the list behind it closes the overlay instead of selecting.
    hs.click(50, 20);
    assert!(!hs.state.sidebar_overlay());
    assert_eq!(hs.state.list().cursor(), 0);
}

#[test]
fn reader_folds_links_and_attachments() {
    let mut hs = Harness::demo(160, 40);
    let (x, y) = at(&hs, "12 quoted lines");
    hs.click(x, y);
    assert!(hs.text().contains("> I am away that weekend"));
    let (x, y) = at(&hs, "https://garden.example.com/rota");
    let cmds = hs.click(x + 3, y);
    assert!(cmds.contains(&Cmd::OpenUrl("https://garden.example.com/rota".into())));
    // Clicking a collapsed message header expands it.
    let (x, y) = at(&hs, "Mira Holt (you)");
    hs.click(x, y);
    assert_eq!(hs.state.focus(), Focus::Reader);
    let conv = hs.state.reader().conversation().unwrap().clone();
    assert!(hs.state.reader().is_expanded(&conv.messages[1].email_id));

    let mut hs = Harness::demo(120, 36);
    hs.keys("j j enter");
    let (x, y) = at(&hs, "invoice-48213.pdf");
    let cmds = hs.click(x, y);
    assert!(
        cmds.iter()
            .any(|c| matches!(c, Cmd::OpenAttachment { name, .. } if name == "invoice-48213.pdf"))
    );
    let (x, y) = at(&hs, "O open in browser");
    let cmds = hs.click(x, y);
    assert!(cmds.iter().any(|c| matches!(c, Cmd::OpenInBrowser(_))));
}

#[test]
fn scrollbar_click_jumps_in_the_narrow_layout() {
    let mut hs = Harness::demo(80, 24);
    hs.click(79, 20);
    assert_eq!(hs.state.list().cursor(), 213);
    hs.clock.advance(SignedDuration::from_secs(1));
    hs.click(79, 2);
    assert_eq!(hs.state.list().cursor(), 0);
}

#[test]
fn right_click_offers_the_actions_menu() {
    let mut hs = Harness::demo(160, 40);
    hs.mouse(MouseEventKind::Down(MouseButton::Right), 40, 6);
    assert_eq!(hs.state.list().cursor(), 3);
    match hs.state.overlays().last() {
        Some(Overlay::Picker(p)) => {
            assert_eq!(p.kind, mejl_ui::picker::PickerKind::Actions);
            // The menu acts on the row that was right clicked.
            assert!(hs.text().contains("Archive"));
        }
        other => panic!("{other:?}"),
    }
    // Clicking a menu row runs it.
    let (x, y) = at(&hs, "Move to trash");
    let cmds = hs.click(x, y);
    assert!(
        cmds.iter()
            .any(|c| matches!(c, Cmd::Mutate(mejl_ui::Mutation::Trash(_)))),
        "{cmds:?}"
    );
    assert!(hs.state.overlays().is_empty());
}

#[test]
fn confirm_prompt_cancels_on_click() {
    let mut hs = Harness::demo(160, 40);
    hs.keys("g t #");
    assert!(matches!(
        hs.state.overlays().last(),
        Some(Overlay::Confirm(_))
    ));
    let cmds = hs.click(40, 20);
    assert!(!cmds.iter().any(|c| matches!(c, Cmd::Mutate(_))));
    assert!(hs.state.overlays().is_empty());
}
