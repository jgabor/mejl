//! Display width: CJK, emoji, combining marks and hostile text never break
//! a column.

mod common;

use common::{Harness, now};
use jiff::SignedDuration;
use mejl_ui::{AccountView, BodyView, MailboxRole, MemoryData, NewEmail, Participant, UiConfig};

fn hostile() -> MemoryData {
    let mut d = MemoryData::new(AccountView {
        name: "Mira Holt".into(),
        email: "mira@example.org".into(),
    });
    d.add_mailbox("in", None, "Inbox", Some(MailboxRole::Inbox), 0);
    let rows: [(&str, &str, &str); 8] = [
        (
            "山田太郎",
            "taro@example.jp",
            "会議の議事録について確認お願いします、来週の予定も",
        ),
        (
            "👨\u{200D}👩\u{200D}👧 Family Chat",
            "family@example.com",
            "🎉🎉 Party time 🎉 bring snacks 🍕🍕🍕🍕",
        ),
        (
            "Zoe\u{0308} Ame\u{0301}lie",
            "zoe@example.fr",
            "Cafe\u{0301} au lait, encore une fois",
        ),
        (
            "Evil \u{202E}gnp.exe",
            "evil@example.net",
            "Invoice\u{202E}fdp.exe attached\u{200B}\u{200B}",
        ),
        (
            "Line\nBreak\tTab",
            "lb@example.org",
            "Subject\nwith\nnewlines",
        ),
        (
            "한국어 이름입니다 아주 긴 이름",
            "kim@example.kr",
            "안녕하세요 반갑습니다 오늘 회의 있어요",
        ),
        (
            "",
            "no.name.at.all.with.a.very.long.address@example.com",
            "",
        ),
        ("ｶﾞｷﾞｸﾞ Halfwidth", "hw@example.jp", "ﾊﾟﾋﾟﾌﾟ sound marks"),
    ];
    for (i, (name, email, subject)) in rows.into_iter().enumerate() {
        let date = now()
            .checked_sub(SignedDuration::from_mins(i as i64 * 30))
            .unwrap();
        let mut e = NewEmail::new(
            format!("e{i}").as_str(),
            format!("t{i}").as_str(),
            Participant::new(name, email),
            subject,
            date,
            "in",
        );
        e.preview = format!("{subject} {subject}");
        e.unread = i % 2 == 0;
        e.to = vec![Participant::new("Mira Holt", "mira@example.org")];
        d.add_email(e);
        d.add_body(BodyView {
            email_id: format!("e{i}").into(),
            text: Some(format!("{subject}\n\n{name}\n")),
            html: None,
            attachments: Vec::new(),
            headers: Vec::new(),
        });
    }
    d
}

fn is_vertical(sym: &str) -> bool {
    matches!(sym, "\u{2502}" | "\u{2503}" | "\u{252C}" | "\u{2534}")
}

#[test]
fn borders_and_dates_stay_put() {
    for (w, h, borders) in [
        (160u16, 40u16, vec![0u16, 23, 97, 159]),
        (120, 36, vec![0, 58, 119]),
        (80, 24, vec![0, 79]),
    ] {
        let hs = Harness::with(hostile(), UiConfig::default(), w, h);
        let buf = hs.buffer();
        let frame_bottom = h - 3;
        for y in 2..frame_bottom {
            for &x in &borders {
                let sym = buf[(x, y)].symbol();
                assert!(
                    is_vertical(sym),
                    "{w}x{h}: border at ({x},{y}) is {sym:?}\n{}",
                    hs.text()
                );
            }
        }
        // Every date ends one cell before the list's right border.
        let list = hs.state.layout().list.unwrap();
        let date_end = list.right() - 2;
        for y in 3..3 + 8u16 {
            let sym = buf[(date_end, y)].symbol();
            assert!(
                sym.chars().all(|c| c.is_ascii_digit()),
                "{w}x{h} row {y}: {sym:?}\n{}",
                hs.text()
            );
        }
    }
}

#[test]
fn hostile_characters_never_reach_the_screen() {
    let hs = Harness::with(hostile(), UiConfig::default(), 160, 40);
    for cell in hs.buffer().content() {
        for c in cell.symbol().chars() {
            assert!(
                !matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200B}'),
                "{c:?}"
            );
            assert!(!c.is_control(), "{c:?}");
        }
    }
    let text = hs.text();
    assert!(text.contains("Evil gnp.exe"));
    assert!(text.contains("Line Break Tab"));
}

#[test]
fn wide_names_truncate_on_grapheme_boundaries() {
    let hs = Harness::with(hostile(), UiConfig::default(), 120, 36);
    let text = hs.text();
    // Sender column is 16 cells at 120: CJK names cut to whole characters
    // and end with the ellipsis; nothing is split.
    assert!(text.contains("山田太郎"), "{text}");
    assert!(text.contains("한국어 이름입니⋯"), "{text}");
    assert!(text.contains("no.name.at.all.⋯"), "{text}");
    // A wide character that does not fit leaves a pad cell, never half a
    // glyph.
    let buf = hs.buffer();
    for y in 2..33 {
        for x in 0..120 {
            let sym = buf[(x, y)].symbol();
            assert!(!sym.contains('\u{FFFD}') || sym == "\u{FFFD}", "{sym:?}");
        }
    }
}

#[test]
fn reader_handles_wide_text() {
    let mut hs = Harness::with(hostile(), UiConfig::default(), 160, 40);
    hs.keys("j");
    let lines = hs.state.reader().text_lines().join("\n");
    assert!(
        lines.contains("👨\u{200D}👩\u{200D}👧 Family Chat"),
        "{lines}"
    );
    let reader = hs.state.layout().reader.unwrap();
    for y in reader.y..reader.bottom() {
        let sym = hs.buffer()[(reader.right(), y)].symbol();
        assert!(
            is_vertical(sym),
            "reader border at row {y} is {sym:?}\n{}",
            hs.text()
        );
    }
}
