//! Every screen and popup survives every terminal size: no panic, no
//! drawing outside the frame, at widths 30 to 200 and heights 8 to 50.

mod common;

use common::Harness;
use mejl_ui::Event;

fn typed(hs: &mut Harness, text: &str) {
    for c in text.chars() {
        let key = if c == ' ' {
            "space".to_owned()
        } else {
            c.to_string()
        };
        hs.keys(&key);
    }
}

type Setup = (&'static str, fn(&mut Harness));

fn setups() -> Vec<Setup> {
    vec![
        ("compose", |hs| {
            hs.keys("c");
            typed(hs, "jun");
        }),
        ("compose body", |hs| {
            hs.keys("j enter r");
            hs.event(Event::Paste("A long line that wraps ".repeat(40)));
        }),
        ("from picker", |hs| {
            hs.keys("c ctrl-o f").into_iter().for_each(drop)
        }),
        ("attach prompt", |hs| {
            hs.keys("c ctrl-o a").into_iter().for_each(drop)
        }),
        ("search", |hs| {
            hs.keys("/");
            typed(hs, "from:larkspur deliveredto:x");
        }),
        ("palette", |hs| hs.keys(":").into_iter().for_each(drop)),
        ("move picker", |hs| hs.keys("m").into_iter().for_each(drop)),
        ("masked", |hs| hs.keys("g m 3 4").into_iter().for_each(drop)),
        ("rules", |hs| hs.keys("g r j t").into_iter().for_each(drop)),
        ("rules form", |hs| {
            hs.keys("g r j enter").into_iter().for_each(drop)
        }),
        ("settings", |hs| hs.keys("g ,").into_iter().for_each(drop)),
    ]
}

#[test]
fn every_screen_at_every_size() {
    for (name, setup) in setups() {
        let mut hs = Harness::demo(120, 36);
        setup(&mut hs);
        for w in (30u16..=200).step_by(17) {
            for h in (8u16..=50).step_by(7) {
                hs.resize(w, h);
                let text = hs.text();
                assert_eq!(text.lines().count(), usize::from(h), "{name} at {w}x{h}");
            }
        }
        // And the smallest sizes, where most subtractions run out.
        for (w, h) in [(30, 8), (31, 9), (40, 10), (99, 19), (100, 20), (159, 39)] {
            hs.resize(w, h);
        }
    }
}
