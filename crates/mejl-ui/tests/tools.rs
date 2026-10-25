//! The Masked Email, rules and settings screens.

mod common;

use std::time::Duration;

use common::Harness;
use mejl_ui::keymap::{Context, Keymap, Pending, Resolution};
use mejl_ui::{
    Action, Cmd, DataChange, Event, Key, MailData, MaskedState, Msg, Mutation, Overlay, PushState,
    RuleJson, Screen, Setting, SyncStatus, UiConfig,
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

fn resolves(contexts: &[Context], keys: &str, action: Action) {
    let map = Keymap::default();
    let mut pending = Pending::default();
    let mut last = Resolution::Unbound;
    for k in keys.split_whitespace() {
        let key: Key = k.parse().unwrap();
        last = map.feed(&mut pending, contexts, key);
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

// ----- Masked Email -----

fn masked(w: u16, h: u16) -> Harness {
    let mut hs = synced(w, h);
    hs.keys("g m 3 4");
    hs.clock.advance(jiff::SignedDuration::from_secs(6));
    hs.event(Event::Msg(Msg::Tick));
    hs
}

#[test]
fn masked_120x36() {
    let hs = masked(120, 36);
    insta::assert_snapshot!(hs.text());
}

#[test]
fn masked_80x24() {
    let hs = masked(80, 24);
    insta::assert_snapshot!(hs.text());
}

#[test]
fn masked_160x40() {
    let hs = masked(160, 40);
    insta::assert_snapshot!(hs.text());
}

#[test]
fn masked_filters_and_states() {
    let mut hs = synced(120, 36);
    hs.keys("g m");
    assert_eq!(*hs.state.screen(), Screen::Masked);
    let text = hs.text();
    assert!(
        !text.contains("cedar.pixel0408"),
        "pending is hidden by default"
    );
    hs.keys("3");
    assert!(hs.text().contains("cedar.pixel0408"));
    hs.keys("/");
    typed(&mut hs, "bank");
    let text = hs.text();
    assert!(text.contains("maple.drift5513@example.org"), "{text}");
    assert!(!text.contains("kettle.fern4821"));
    hs.keys("enter");
    hs.event(Event::Paste("zzz".into()));
    assert!(hs.text().contains("Nothing matches"));
    hs.keys("esc esc");
    assert_eq!(
        *hs.state.screen(),
        Screen::Masked,
        "Esc clears the filter first"
    );
    assert!(hs.text().contains("kettle.fern4821"));
}

#[test]
fn masked_actions_send_commands() {
    let mut hs = synced(120, 36);
    hs.keys("g m");
    let id = hs.data.masked_emails()[0].id.clone();
    let cmds = hs.keys("x");
    assert!(cmds.contains(&Cmd::MaskedSetState {
        id: id.clone(),
        state: MaskedState::Disabled
    }));
    assert!(
        hs.line(34).contains("Future mail goes to Trash.  z undo"),
        "{}",
        hs.line(34)
    );
    let cmds = hs.keys("e");
    assert!(
        !cmds.iter().any(|c| matches!(c, Cmd::MaskedSetState { .. })),
        "already enabled in the data"
    );
    hs.keys("D");
    assert!(hs.text().contains("Mail to it will bounce"));
    let cmds = hs.keys("y");
    assert!(cmds.contains(&Cmd::MaskedSetState {
        id: id.clone(),
        state: MaskedState::Deleted
    }));
    let cmds = hs.keys("y");
    assert!(cmds.contains(&Cmd::CopyToClipboard("kettle.fern4821@example.org".into())));
    hs.keys("r ctrl-u");
    typed(&mut hs, "Shop");
    let cmds = hs.keys("enter");
    assert!(cmds.contains(&Cmd::MaskedSetDescription {
        id: id.clone(),
        description: "Shop".into()
    }));
    hs.keys("n");
    typed(&mut hs, "https://cabins.example.net/book");
    hs.keys("enter");
    typed(&mut hs, "Cabins");
    let cmds = hs.keys("enter");
    assert!(cmds.contains(&Cmd::MaskedCreate {
        for_domain: "cabins.example.net".into(),
        description: "Cabins".into(),
        draft_id: None
    }));
    let cmds = hs.keys("enter");
    assert!(cmds.contains(&Cmd::ServerSearch {
        query: "to:kettle.fern4821@example.org".into(),
        limit: 100
    }));
    assert_eq!(*hs.state.screen(), Screen::Mail);
    assert!(hs.text().contains("Search: to:kettle.fern4821@example.org"));
}

#[test]
fn masked_reloads_on_data_change() {
    let mut hs = synced(120, 36);
    hs.keys("g m j");
    let mut m = hs.data.masked_emails()[1].clone();
    m.state = MaskedState::Disabled;
    hs.data.set_masked(m.clone());
    hs.event(Event::Msg(Msg::DataChanged(DataChange {
        masked: true,
        ..DataChange::default()
    })));
    assert!(
        hs.text().contains("✗ disabled bright.otter2207"),
        "{}",
        hs.text()
    );
}

#[test]
fn masked_keys_and_mouse() {
    for (k, a) in [
        ("j", Action::Next),
        ("/", Action::Filter),
        ("1", Action::MaskedShowEnabled),
        ("4", Action::MaskedShowDeleted),
        ("n", Action::MaskedNew),
        ("e", Action::MaskedEnable),
        ("x", Action::MaskedDisable),
        ("D", Action::MaskedDelete),
        ("r", Action::MaskedRename),
        ("enter", Action::MaskedMessages),
        ("y", Action::MaskedCopy),
        ("R", Action::MaskedRule),
    ] {
        resolves(&[Context::Masked, Context::Global], k, a);
    }
    let mut hs = synced(120, 36);
    hs.keys("g m");
    let (x, y) = hs.find("amber.loom3390").unwrap();
    hs.click(x, y);
    assert!(hs.text().contains("├─ amber.loom3390@example.org"));
    let (x, y) = hs.find("2 ✓ disabled").unwrap();
    hs.click(x, y);
    assert!(hs.text().contains("2 ✗ disabled"));
}

/// The status line count on the Masked Email screen, once the toast that
/// covers it has gone.
fn shown(hs: &mut Harness) -> String {
    hs.clock.advance(jiff::SignedDuration::from_secs(6));
    hs.event(Event::Msg(Msg::Tick));
    let text = hs.text();
    text.lines()
        .find_map(|l| l.split_once(" shown").map(|(a, _)| a.trim().to_owned()))
        .unwrap_or_default()
}

#[test]
fn masked_silent_and_created_by_filters() {
    let mut hs = synced(120, 36);
    hs.keys("g m 3 4");
    let all = hs.data.masked_emails();
    let total = all.len();
    assert_eq!(shown(&mut hs), format!("{total} of {total}"));
    assert!(hs.text().contains("5 silent: any   6 created by: any"));

    // Silent shows exactly what the "silent for 6+ months" count counts.
    hs.keys("5");
    let text = hs.text();
    assert!(text.contains("5 silent: silent"), "{text}");
    assert!(text.contains("1 silent for 6+ months"), "{text}");
    assert!(
        hs.state
            .toast()
            .unwrap()
            .text
            .contains("Showing silent addresses")
    );
    assert_eq!(shown(&mut hs), format!("1 of {total}"));
    hs.keys("5");
    assert!(hs.text().contains("5 silent: active"));
    hs.keys("5");
    assert!(hs.text().contains("5 silent: any"));
    assert_eq!(shown(&mut hs), format!("{total} of {total}"));

    // Created by: any, then each value sorted, then any.
    let mut creators: Vec<String> = Vec::new();
    for m in &all {
        if !creators
            .iter()
            .any(|c| c.eq_ignore_ascii_case(&m.created_by))
        {
            creators.push(m.created_by.clone());
        }
    }
    creators.sort_by_key(|c| c.to_lowercase());
    assert!(creators.len() >= 2, "{creators:?}");
    for c in &creators {
        hs.keys("6");
        let n = all
            .iter()
            .filter(|m| m.created_by.eq_ignore_ascii_case(c))
            .count();
        assert!(
            hs.text().contains(&format!("6 created by: {c}")),
            "{}",
            hs.text()
        );
        assert_eq!(shown(&mut hs), format!("{n} of {total}"), "created by {c}");
    }
    hs.keys("6");
    assert!(hs.text().contains("6 created by: any"));

    // The status line names active filters, for widths that cut the row.
    let mut narrow = synced(80, 24);
    narrow.keys("g m 5 6");
    shown(&mut narrow);
    assert!(
        narrow.text().contains(&format!(
            "of {total} shown, silent only, created by {}",
            creators[0]
        )),
        "{}",
        narrow.text()
    );

    // The chips are mouse targets too.
    let (x, y) = hs.find("6 created by").unwrap();
    hs.click(x + 4, y);
    assert!(
        hs.text()
            .contains(&format!("6 created by: {}", creators[0]))
    );
    let (x, y) = hs.find("5 silent").unwrap();
    hs.click(x + 2, y);
    assert!(hs.text().contains("5 silent: silent"));

    // Jumping to an address from the palette clears both filters, so the
    // cursor lands on it.
    hs.keys("6 :");
    typed(&mut hs, "quiet.heron7731");
    hs.keys("enter");
    let text = hs.text();
    assert_eq!(*hs.state.screen(), Screen::Masked);
    assert!(text.contains("5 silent: any   6 created by: any"), "{text}");
    assert!(text.contains("├─ quiet.heron7731@example.org"), "{text}");
}

#[test]
fn masked_filter_keys_in_both_presets() {
    for (k, a) in [
        ("5", Action::MaskedCycleSilent),
        ("6", Action::MaskedCycleCreatedBy),
    ] {
        resolves(&[Context::Masked, Context::Global], k, a);
    }
    // In the vim preset digits are counts, but not where a digit is bound.
    let config = UiConfig {
        keymap: mejl_ui::Keymap::preset(mejl_ui::Preset::Vim),
        ..UiConfig::default()
    };
    let mut hs = Harness::with(mejl_ui::MemoryData::demo(common::now()), config, 120, 36);
    hs.keys("g m 5");
    assert!(hs.text().contains("5 silent: silent"), "{}", hs.text());
    hs.keys("6");
    assert!(!hs.text().contains("6 created by: any"), "{}", hs.text());
}

#[test]
fn mask_prefix_from_the_reader() {
    let mut hs = synced(120, 36);
    hs.keys("j j enter @");
    assert!(hs.text().contains("@  masked email"), "{}", hs.text());
    hs.keys("x");
    assert!(hs.text().contains(
        "Disable kettle.fern4821@example.org (Larkspur shop)? Future mail goes to Trash."
    ));
    let cmds = hs.keys("y");
    assert!(cmds.iter().any(|c| matches!(
        c,
        Cmd::MaskedSetState {
            state: MaskedState::Disabled,
            ..
        }
    )));
    let cmds = hs.keys("@ y");
    assert!(cmds.contains(&Cmd::CopyToClipboard("kettle.fern4821@example.org".into())));
    hs.keys("@ r");
    assert_eq!(*hs.state.screen(), Screen::Rules);
    assert!(hs.text().contains("Sent to kettle.fern4821@example.org"));
    // A message not sent to a mask says so.
    let mut hs = synced(120, 36);
    hs.keys("enter @ x");
    assert!(
        hs.state
            .toast()
            .unwrap()
            .text
            .contains("not sent to a masked address")
    );
}

// ----- Rules -----

#[test]
fn rules_160x40() {
    let mut hs = synced(160, 40);
    hs.keys("g r j t");
    hs.clock.advance(jiff::SignedDuration::from_secs(6));
    hs.event(Event::Msg(Msg::Tick));
    insta::assert_snapshot!(hs.text());
}

#[test]
fn rules_120x36() {
    let mut hs = synced(120, 36);
    hs.keys("g r j");
    insta::assert_snapshot!(hs.text());
}

#[test]
fn rules_80x24() {
    let mut hs = synced(80, 24);
    hs.keys("g r j");
    insta::assert_snapshot!(hs.text());
}

#[test]
fn rules_list_toggle_reorder_delete() {
    let mut hs = synced(160, 40);
    hs.keys("g r");
    assert!(hs.text().contains("mejl rules (4)"));
    assert!(hs.text().contains("last copied v1, Oct 20"));
    let cmds = hs.keys("space");
    let Some(Cmd::SaveRule(r)) = cmds.iter().find(|c| matches!(c, Cmd::SaveRule(_))) else {
        panic!("{cmds:?}");
    };
    assert!(r.json.contains("\"enabled\":false"));
    let cmds = hs.keys("J");
    assert!(cmds.contains(&Cmd::ReorderRules(vec![
        "r_2c41".into(),
        "r_18aa".into(),
        "r_5d07".into(),
        "r_77e2".into()
    ])));
    hs.keys("D");
    assert!(hs.text().contains("Delete the rule \"Newsletters\"?"));
    let cmds = hs.keys("y");
    assert!(cmds.contains(&Cmd::DeleteRule("r_18aa".into())));
    assert!(hs.text().contains("mejl rules (3)"));
}

#[test]
fn rules_edit_form() {
    let mut hs = synced(160, 40);
    hs.keys("g r j enter");
    // Name.
    hs.keys("enter ctrl-u");
    typed(&mut hs, "Receipts");
    let cmds = hs.keys("enter");
    assert!(
        cmds.iter()
            .any(|c| matches!(c, Cmd::SaveRule(r) if r.json.contains("\"name\":\"Receipts\"")))
    );
    // Add a condition.
    hs.keys("j j j j j enter");
    typed(&mut hs, "from is billing@shop.example.com");
    let cmds = hs.keys("enter");
    assert!(
        cmds.iter()
            .any(|c| matches!(c, Cmd::SaveRule(r) if r.json.contains("billing@shop.example.com")))
    );
    assert!(
        hs.text().contains("From is billing@shop.example.com"),
        "{}",
        hs.text()
    );
    // A bad condition is refused with a reason.
    hs.keys("enter ctrl-u");
    typed(&mut hs, "colour is red");
    let cmds = hs.keys("enter");
    assert!(!cmds.iter().any(|c| matches!(c, Cmd::SaveRule(_))));
    assert!(hs.state.toast().unwrap().text.contains("not a field"));
    // Add an action naming a mailbox.
    hs.keys("j j j j enter");
    typed(&mut hs, "label Travel");
    let cmds = hs.keys("enter");
    assert!(
        cmds.iter()
            .any(|c| matches!(c, Cmd::SaveRule(r) if r.json.contains("mb-travel"))),
        "{cmds:?}"
    );
    hs.keys("enter ctrl-u");
    typed(&mut hs, "move to Nowhere");
    hs.keys("enter");
    assert!(hs.state.toast().unwrap().text.contains("no mailbox"));
    // Toggle the match mode and delete a condition.
    hs.keys("g g j j space");
    assert!(hs.text().contains("all of these match"), "{}", hs.text());
    hs.keys("j D");
    assert!(!hs.text().contains("From domain is shop.example.com"));
    hs.keys("esc");
    assert_eq!(*hs.state.screen(), Screen::Rules);
    hs.keys("esc");
    assert_eq!(*hs.state.screen(), Screen::Mail);
}

#[test]
fn rules_new_from_message_and_new() {
    let mut hs = synced(160, 40);
    hs.keys("F");
    assert_eq!(*hs.state.screen(), Screen::Rules);
    assert!(
        hs.log
            .iter()
            .any(|c| matches!(c, Cmd::SaveRule(r) if r.json.contains("garden-club"))),
        "{:?}",
        hs.log
    );
    assert!(hs.text().contains("mejl rules (5)"));
    hs.keys("esc n");
    assert!(hs.line(38).contains("Rule name:"), "{}", hs.line(38));
    hs.keys("ctrl-u");
    typed(&mut hs, "Bank");
    let cmds = hs.keys("enter");
    assert!(
        cmds.iter()
            .any(|c| matches!(c, Cmd::SaveRule(r) if r.json.contains("\"name\":\"Bank\"")))
    );
}

#[test]
fn rules_copy_block_and_paste_check() {
    let mut hs = synced(160, 40);
    hs.keys("g r");
    let cmds = hs.keys("P");
    let sieve = cmds
        .iter()
        .find_map(|c| match c {
            Cmd::CopyToClipboard(t) => Some(t.clone()),
            _ => None,
        })
        .expect("a copy");
    assert!(sieve.contains("# >>> mejl managed block v1"));
    assert!(!sieve.contains("keep"));
    let Some(Cmd::RecordRuleRender { body_sha256, .. }) = cmds
        .iter()
        .find(|c| matches!(c, Cmd::RecordRuleRender { .. }))
        .cloned()
    else {
        panic!("{cmds:?}");
    };
    hs.data.add_rule_render(mejl_ui::RuleRenderView {
        id: 2,
        rendered_at: common::now(),
        sieve: sieve.clone(),
        body_sha256,
        confirmed_at: None,
    });
    hs.event(Event::Msg(Msg::DataChanged(DataChange {
        rules: true,
        ..DataChange::default()
    })));
    assert!(hs.text().contains("last copied v2"));
    // Paste the current block back.
    hs.keys("v");
    let cmds = hs.event(Event::Paste(format!(
        "require [\"fileinto\"];\n{sieve}\n# my own code\n"
    )));
    assert!(cmds.contains(&Cmd::ConfirmRulePaste { render_id: 2 }));
    assert!(
        hs.state
            .toast()
            .unwrap()
            .text
            .contains("Fastmail has the current mejl block")
    );
    // An older block.
    let old = hs.data.rule_renders(5)[1].sieve.clone();
    hs.keys("v");
    hs.event(Event::Paste(old));
    assert!(
        hs.state.toast().unwrap().text.contains("older block (v1)"),
        "{}",
        hs.state.toast().unwrap().text
    );
    // Edited by hand, and nothing found.
    hs.keys("v");
    hs.event(Event::Paste(sieve.replace("Newsletters", "News")));
    assert!(hs.state.toast().unwrap().text.contains("edited by hand"));
    hs.keys("v");
    hs.event(Event::Paste("if true { stop; }".into()));
    assert!(hs.state.toast().unwrap().text.contains("No mejl block"));
    hs.keys("v esc");
    assert!(hs.state.overlays().is_empty());
}

#[test]
fn rules_dry_run_and_apply() {
    let mut hs = synced(160, 40);
    hs.keys("g r j a");
    assert!(
        hs.state
            .toast()
            .unwrap()
            .text
            .contains("Run the dry run first")
    );
    hs.keys("t");
    let text = hs.text();
    assert!(text.contains("would match"), "{text}");
    let cmds = hs.keys("a");
    assert!(
        cmds.iter()
            .any(|c| matches!(c, Cmd::Mutate(Mutation::MoveTo { .. }))),
        "{cmds:?}"
    );
    assert!(hs.state.toast().unwrap().text.contains("Applied the rule"));
}

#[test]
fn broken_rules_are_rows() {
    let mut hs = synced(160, 40);
    hs.data.add_rule(RuleJson {
        id: "r_bad".into(),
        json: "{\"id\": 3".into(),
    });
    hs.keys("g r");
    let text = hs.text();
    assert!(text.contains("! r_bad cannot be read"), "{text}");
    hs.keys("G");
    assert!(hs.text().contains("This rule cannot be read"));
    // The other rules still render and copy.
    let cmds = hs.keys("P");
    assert!(cmds.iter().any(|c| matches!(c, Cmd::CopyToClipboard(_))));
    hs.keys("D y");
    assert!(hs.log.contains(&Cmd::DeleteRule("r_bad".into())));
}

#[test]
fn rules_keys() {
    for (k, a) in [
        ("n", Action::RuleNew),
        ("enter", Action::Open),
        ("space", Action::RuleToggle),
        ("J", Action::RuleDown),
        ("K", Action::RuleUp),
        ("D", Action::RuleDelete),
        ("t", Action::RuleDryRun),
        ("a", Action::RuleApply),
        ("P", Action::RuleCopyBlock),
        ("v", Action::RulePasteCheck),
        ("tab", Action::NextPane),
    ] {
        resolves(&[Context::Rules, Context::Global], k, a);
    }
}

// ----- Settings -----

#[test]
fn settings_120x36() {
    let mut hs = synced(120, 36);
    hs.keys("g ,");
    insta::assert_snapshot!(hs.text());
}

#[test]
fn settings_160x40() {
    let mut hs = synced(160, 40);
    hs.keys("g ,");
    insta::assert_snapshot!(hs.text());
}

#[test]
fn settings_80x24() {
    let mut hs = synced(80, 24);
    hs.keys("g ,");
    insta::assert_snapshot!(hs.text());
}

#[test]
fn settings_change_and_apply() {
    let mut hs = synced(120, 36);
    hs.keys("g ,");
    let cmds = hs.keys("enter");
    assert!(cmds.contains(&Cmd::SaveSetting(Setting::Theme("mono".into()))));
    assert!(hs.text().contains("terminal, saving"));
    let cmds = hs.keys("j j h");
    assert!(cmds.contains(&Cmd::SaveSetting(Setting::Density(
        mejl_ui::Density::Compact
    ))));
    let config = UiConfig {
        density: mejl_ui::Density::Compact,
        undo_send: Duration::from_secs(5),
        ..UiConfig::default()
    };
    hs.event(Event::Msg(Msg::Config(Box::new(config))));
    let text = hs.text();
    assert!(text.contains("compact"), "{text}");
    assert!(text.contains("5 s"));
    assert!(!text.contains("saving"));
    let (x, y) = hs.find("Prefer HTML").unwrap();
    hs.click(x, y);
    let cmds = hs.click(x, y);
    assert!(cmds.contains(&Cmd::SaveSetting(Setting::PreferHtml(true))));
    for (k, a) in [
        ("enter", Action::SettingOpen),
        ("space", Action::SettingChange),
        ("l", Action::SettingChange),
        ("h", Action::SettingChangeBack),
        ("j", Action::Next),
    ] {
        resolves(&[Context::Settings, Context::Global], k, a);
    }
}

#[test]
fn densities_change_the_list() {
    let mut hs = synced(160, 40);
    assert!(hs.text().contains("Today"));
    let compact = UiConfig {
        density: mejl_ui::Density::Compact,
        ..UiConfig::default()
    };
    hs.event(Event::Msg(Msg::Config(Box::new(compact))));
    assert!(!hs.text().contains("Yesterday"), "no date headings");
    let relaxed = UiConfig {
        density: mejl_ui::Density::Relaxed,
        ..UiConfig::default()
    };
    hs.event(Event::Msg(Msg::Config(Box::new(relaxed))));
    let text = hs.text();
    assert!(text.contains("Today"));
    // The preview sits on its own line under the row.
    let (_, y) = hs.find("Juniper Tran (3)").unwrap();
    assert!(
        hs.line(y + 1).contains("Swapping Saturday"),
        "{}",
        hs.line(y + 1)
    );
    hs.keys("j");
    assert_eq!(hs.state.list().cursor(), 1);
    let _ = Overlay::Perf;
}

#[test]
fn mark_read_waits_for_the_delay() {
    let config = UiConfig {
        mark_read_delay: Duration::from_secs(3),
        ..UiConfig::default()
    };
    let mut hs = Harness::with(mejl_ui::MemoryData::demo(common::now()), config, 160, 40);
    let cmds = hs.keys("enter");
    assert!(!cmds.iter().any(|c| matches!(c, Cmd::Mutate(_))));
    assert!(hs.state.next_wakeup().is_some());
    hs.clock.advance(jiff::SignedDuration::from_secs(3));
    let cmds = hs.event(Event::Msg(Msg::Tick));
    assert!(
        cmds.iter()
            .any(|c| matches!(c, Cmd::Mutate(Mutation::SetKeyword { .. })))
    );
}
