//! Snapshots of the mail screen at the mock-up sizes, plus style and
//! layout checks that a text snapshot cannot see.

mod common;

use common::Harness;
use mejl_ui::{Event, Msg, PushState, SyncStatus, Theme, UiConfig};
use ratatui::style::{Color, Modifier};

fn synced(w: u16, h: u16) -> Harness {
    let mut hs = Harness::demo(w, h);
    hs.event(Event::Msg(Msg::Sync(SyncStatus::Synced)));
    hs.event(Event::Msg(Msg::Push(PushState::Connected)));
    hs
}

#[test]
fn inbox_160x40() {
    let mut hs = synced(160, 40);
    // The mock-up selects Priya Raman and Marco Bellini, with the cursor
    // back on the first row.
    hs.keys("j j j j j x j j x g g");
    insta::assert_snapshot!(hs.text());
}

#[test]
fn inbox_120x36() {
    let hs = synced(120, 36);
    insta::assert_snapshot!(hs.text());
}

#[test]
fn inbox_80x24() {
    let hs = synced(80, 24);
    insta::assert_snapshot!(hs.text());
}

#[test]
fn which_key_120x36() {
    let mut hs = synced(120, 36);
    hs.keys("g");
    insta::assert_snapshot!(hs.text());
}

#[test]
fn which_key_mark_prefix_160x40() {
    let mut hs = synced(160, 40);
    hs.keys(".");
    insta::assert_snapshot!(hs.text());
}

#[test]
fn help_160x40() {
    let mut hs = synced(160, 40);
    hs.keys("?");
    insta::assert_snapshot!(hs.text());
}

#[test]
fn help_filtered_120x36() {
    let mut hs = synced(120, 36);
    hs.keys("tab ? q u o t");
    insta::assert_snapshot!(hs.text());
}

#[test]
fn reader_full_120x36() {
    let mut hs = synced(120, 36);
    hs.keys("j j enter");
    insta::assert_snapshot!(hs.text());
}

#[test]
fn reader_full_80x24() {
    let mut hs = synced(80, 24);
    hs.keys("j j enter");
    insta::assert_snapshot!(hs.text());
}

#[test]
fn sidebar_overlay_120x36() {
    let mut hs = synced(120, 36);
    hs.keys("g f");
    insta::assert_snapshot!(hs.text());
}

#[test]
fn offline_with_toast_80x24() {
    let mut hs = Harness::demo(80, 24);
    hs.event(Event::Msg(Msg::Sync(SyncStatus::Offline { pending: 3 })));
    insta::assert_snapshot!("offline_status", hs.line(22));
    hs.keys("y");
    insta::assert_snapshot!("archive_toast", hs.line(22));
}

#[test]
fn ascii_mono_80x24() {
    let theme = Theme::named("terminal", true).unwrap().with_ascii(true);
    let config = UiConfig {
        theme,
        ..UiConfig::default()
    };
    let mut hs = Harness::with(mejl_ui::MemoryData::demo(common::now()), config, 80, 24);
    hs.event(Event::Msg(Msg::Sync(SyncStatus::Synced)));
    hs.event(Event::Msg(Msg::Push(PushState::Connected)));
    let text = hs.text();
    assert!(text.is_ascii(), "{text}");
    insta::assert_snapshot!(text);
    // Mono: no colours anywhere.
    let buf = hs.buffer();
    for cell in buf.content() {
        assert!(
            cell.fg == Color::Reset && cell.bg == Color::Reset,
            "{cell:?}"
        );
    }
}

#[test]
fn styles_carry_state() {
    let hs = synced(160, 40);
    let buf = hs.buffer();
    // Unread rows are bold; read rows are not.
    let juniper = hs.find("Juniper Tran (3)").unwrap();
    assert!(buf[juniper].modifier.contains(Modifier::BOLD));
    let lena = hs.find("Lena Okafor (2)").unwrap();
    assert!(!buf[lena].modifier.contains(Modifier::BOLD));
    // The cursor row is reverse video in the terminal theme, end to end.
    for x in 24..97 {
        assert!(buf[(x, 3)].modifier.contains(Modifier::REVERSED), "x={x}");
    }
    assert!(!buf[(30, 4)].modifier.contains(Modifier::REVERSED));
    // Markers: unread blue, pinned red, masked magenta.
    assert_eq!(buf[(25, 4)].symbol(), "\u{25E6}");
    assert_eq!(buf[(25, 4)].fg, Color::Blue);
    assert_eq!(buf[(26, 4)].symbol(), "\u{2691}");
    assert_eq!(buf[(26, 4)].fg, Color::Red);
    assert_eq!(buf[(27, 5)].symbol(), "@");
    assert_eq!(buf[(27, 5)].fg, Color::Magenta);
    // Previews are dim.
    let preview = hs.find("Please find the inv").unwrap();
    assert_eq!(buf[preview].fg, Color::DarkGray);
    // Hint keys are blue and bold; labels are dim.
    assert_eq!(buf[(1, 39)].fg, Color::Blue);
    assert!(buf[(1, 39)].modifier.contains(Modifier::BOLD));
    assert_eq!(buf[(5, 39)].fg, Color::DarkGray);
    // The focused pane title is accented.
    let title = hs.find("\u{2500} Inbox \u{2500}").unwrap();
    assert_eq!(buf[(title.0 + 2, title.1)].fg, Color::Blue);
    // Links are cyan and underlined.
    let link = hs.find("https://garden.example.com/rota").unwrap();
    assert_eq!(buf[link].fg, Color::Cyan);
    assert!(buf[link].modifier.contains(Modifier::UNDERLINED));
}

#[test]
fn truecolor_dark_theme_paints_the_cursor_row() {
    let config = UiConfig {
        theme: Theme::named("dark", false).unwrap(),
        ..UiConfig::default()
    };
    let hs = Harness::with(mejl_ui::MemoryData::demo(common::now()), config, 160, 40);
    let buf = hs.buffer();
    assert_eq!(buf[(40, 3)].bg, Color::Rgb(0x25, 0x29, 0x32));
    assert_eq!(
        buf[(40, 4)].bg,
        Color::Reset,
        "no background fill elsewhere"
    );
}

#[test]
fn layout_breakpoints_and_resizes() {
    let mut hs = synced(160, 40);
    let border = |hs: &Harness, x: u16| hs.buffer()[(x, 1)].symbol().to_owned();
    assert_eq!(border(&hs, 23), "\u{252C}");
    assert_eq!(border(&hs, 97), "\u{252C}");
    hs.keys("j j j");
    hs.resize(120, 36);
    assert_eq!(border(&hs, 58), "\u{252C}");
    assert_eq!(border(&hs, 23), "\u{2500}", "no sidebar at 120");
    assert_eq!(hs.state.list().cursor(), 3, "the cursor survives a resize");
    assert!(hs.text().contains("Inbox \u{25BE}"));
    hs.resize(80, 24);
    assert!(!hs.text().contains("\u{252C}"), "one pane at 80");
    assert!(hs.line(23).contains("j/k move"));
    // Short terminals drop the hint bar.
    hs.resize(80, 18);
    assert!(!hs.text().contains("j/k move"));
    assert!(hs.line(17).contains("4 of 214"));
    // Too small to draw.
    hs.resize(20, 6);
    assert!(hs.text().contains("mejl needs"));
    hs.resize(160, 40);
    assert_eq!(border(&hs, 97), "\u{252C}");
}

#[test]
fn forced_layouts() {
    let config = UiConfig {
        layout: mejl_ui::LayoutPref::Stack,
        ..UiConfig::default()
    };
    let hs = Harness::with(mejl_ui::MemoryData::demo(common::now()), config, 160, 40);
    assert_eq!(hs.state.layout().mode, mejl_ui::LayoutMode::Stack);
    assert!(!hs.text().contains("Mailboxes"));
}

#[test]
fn accessible_mode_parks_the_cursor_on_the_focused_row() {
    let config = UiConfig {
        accessible: true,
        ..UiConfig::default()
    };
    let mut hs = Harness::with(mejl_ui::MemoryData::demo(common::now()), config, 160, 40);
    hs.keys("j");
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 40)).unwrap();
    let mut out = None;
    terminal
        .draw(|f| out = Some(mejl_ui::render(&hs.state, &hs.data, f)))
        .unwrap();
    assert_eq!(
        out.unwrap().cursor,
        Some(ratatui::layout::Position::new(24, 4))
    );
}

#[test]
fn the_focused_message_is_marked_only_while_the_reader_has_focus() {
    let mut hs = synced(160, 40);
    let (x, y) = hs.find("\u{25BE} Juniper Tran").unwrap();
    assert!(!hs.buffer()[(x, y)].modifier.contains(Modifier::REVERSED));
    hs.keys("tab");
    assert!(hs.buffer()[(x, y)].modifier.contains(Modifier::REVERSED));
    hs.keys("tab");
    assert!(!hs.buffer()[(x, y)].modifier.contains(Modifier::REVERSED));
}
