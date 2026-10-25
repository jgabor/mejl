//! Rendering never panics and never draws over a pane border, at any
//! size, layout, overlay or restored pane size.

mod common;

use common::{Harness, key_event, now};
use mejl_ui::{Event, LayoutPref, MemoryData, PaneSizes, UiConfig};

fn is_vertical(sym: &str) -> bool {
    matches!(
        sym,
        "\u{2502}" | "\u{2503}" | "\u{252C}" | "\u{2534}" | "\u{251C}" | "\u{2524}" | "\u{253C}"
    )
}

fn check_separators(hs: &Harness, what: &str) {
    let layout = hs.state.layout();
    let (w, h) = hs.state.size();
    if mejl_ui::layout::MailLayout::too_small(ratatui::layout::Rect::new(0, 0, w, h)) {
        return;
    }
    // Overlays sit on top of the panes, so only plain screens are checked.
    if !hs.state.overlays().is_empty()
        || !hs.state.pending().is_empty()
        || hs.state.sidebar_overlay()
    {
        return;
    }
    let top = layout.frame.y + 1;
    let bottom = layout.frame.bottom().saturating_sub(1);
    for (x, _) in &layout.separators {
        for y in top..bottom {
            let sym = hs.buffer()[(*x, y)].symbol();
            assert!(
                is_vertical(sym),
                "{what}: separator at ({x},{y}) is {sym:?}\n{}",
                hs.text()
            );
        }
    }
}

#[test]
fn sweep_sizes_layouts_and_states() {
    let prefs = [
        LayoutPref::Auto,
        LayoutPref::Three,
        LayoutPref::Two,
        LayoutPref::Stack,
    ];
    let heights = [8u16, 19, 24, 40];
    let states: [&str; 7] = ["", "j j enter", "g f", "?", "g", "g t #", "x j X"];
    for pref in prefs {
        for w in (30u16..=200).step_by(23) {
            for h in heights {
                for keys in states {
                    let config = UiConfig {
                        layout: pref,
                        ..UiConfig::default()
                    };
                    let mut hs = Harness::with(MemoryData::demo(now()), config, w, h);
                    hs.keys(keys);
                    check_separators(&hs, &format!("{pref:?} {w}x{h} [{keys}]"));
                }
            }
        }
    }
}

#[test]
fn hostile_pane_sizes_are_clamped() {
    let sizes = [
        PaneSizes {
            sidebar: 0,
            list_ratio: Some(f32::NAN),
        },
        PaneSizes {
            sidebar: u16::MAX,
            list_ratio: Some(-1.0),
        },
        PaneSizes {
            sidebar: 3,
            list_ratio: Some(2.0),
        },
        PaneSizes {
            sidebar: 22,
            list_ratio: Some(f32::INFINITY),
        },
    ];
    for panes in sizes {
        for (w, h) in [(160u16, 40u16), (120, 36), (80, 24), (40, 12)] {
            for keys in ["", "g f", "j j enter"] {
                let mut hs = Harness::demo(w, h);
                hs.state.set_pane_sizes(panes);
                hs.event(Event::Msg(mejl_ui::Msg::Tick));
                hs.keys(keys);
                let got = hs.state.pane_sizes();
                assert!((14..=40).contains(&got.sidebar), "{got:?}");
                assert!(
                    got.list_ratio.is_none_or(|r| (0.0..=1.0).contains(&r)),
                    "{got:?}"
                );
                check_separators(&hs, &format!("{panes:?} {w}x{h} [{keys}]"));
            }
        }
    }
}

#[test]
fn resizing_through_every_width_keeps_state_valid() {
    let mut hs = Harness::demo(200, 40);
    hs.keys("j j j tab");
    for w in (0u16..=200).rev().step_by(3) {
        hs.resize(w, 30);
        hs.event(Event::Key(key_event("j")));
        assert!(hs.state.list().cursor() < 214);
    }
    for w in (0u16..=200).step_by(5) {
        hs.resize(w, 40);
        check_separators(&hs, &format!("{w}x40"));
    }
}
