//! The key editor and the macro editor in it.

mod common;

use common::Harness;
use mejl_ui::keymap::{CustomPreset, KeymapSpec, Pending, Resolution};
use mejl_ui::{
    Action, Cmd, Context, Event, FolderRef, KeyOverrides, KeySeq, Keymap, KeysEdit, Macro,
    MailData, Msg, Overlay, Preset, PushState, Screen, Setting, Step, SyncStatus, UiConfig,
};

fn seq(s: &str) -> KeySeq {
    s.parse().unwrap()
}

/// A config whose active preset is `name`, built as the shell builds it.
fn built(mut c: UiConfig, name: &str) -> UiConfig {
    let (base, own) = match Preset::from_name(name) {
        Some(p) => (p, KeyOverrides::default()),
        None => {
            let p = c.presets.iter().find(|p| p.name == name).unwrap();
            (p.base, p.overrides.clone())
        }
    };
    c.keymap = Keymap::build(KeymapSpec {
        base,
        name,
        preset: &own,
        user: &c.user_keys,
        macros: &c.macros,
    })
    .unwrap();
    c
}

/// A config with the custom preset `mine`, based on fastmail, active.
fn mine() -> UiConfig {
    let c = UiConfig {
        presets: vec![CustomPreset {
            name: "mine".into(),
            base: Preset::Fastmail,
            overrides: KeyOverrides::default(),
        }],
        ..UiConfig::default()
    };
    built(c, "mine")
}

fn harness(config: UiConfig, w: u16, h: u16) -> Harness {
    let mut hs = Harness::with(mejl_ui::MemoryData::demo(common::now()), config, w, h);
    hs.event(Event::Msg(Msg::Sync(SyncStatus::Synced)));
    hs.event(Event::Msg(Msg::Push(PushState::Connected)));
    hs
}

/// Opens the editor from the Keys row of settings.
fn open(config: UiConfig, w: u16, h: u16) -> Harness {
    let mut hs = harness(config, w, h);
    hs.keys("g , j enter");
    assert_eq!(*hs.state.screen(), Screen::Keys, "{}", hs.text());
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

fn edits(cmds: &[Cmd]) -> Vec<&KeysEdit> {
    cmds.iter()
        .filter_map(|c| match c {
            Cmd::EditKeys(e) => Some(e),
            _ => None,
        })
        .collect()
}

fn toast(hs: &Harness) -> String {
    hs.state.toast().map(|t| t.text.clone()).unwrap_or_default()
}

fn top(hs: &Harness) -> Option<&Overlay> {
    hs.state.overlays().last()
}

/// Plays the shell's side of key editor commands: writes the change into
/// the config and answers with `Msg::Config`, as `app/settings.rs` does.
fn shell(hs: &mut Harness, cmds: &[Cmd]) {
    for cmd in cmds {
        let mut c = hs.state.config().clone();
        let mut name = c.keymap.name().to_owned();
        match cmd {
            Cmd::EditKeys(KeysEdit::SetPresetKeys {
                preset,
                overrides,
                drop_user,
            }) => {
                let p = c.presets.iter_mut().find(|p| p.name == *preset).unwrap();
                p.overrides = overrides.clone();
                c.user_keys = c.user_keys.without(drop_user);
            }
            Cmd::EditKeys(KeysEdit::NewPreset {
                name: n,
                base,
                overrides,
                take_user,
            }) => {
                let mut entries = overrides.entries().to_vec();
                if *take_user {
                    entries.extend(c.user_keys.entries().iter().cloned());
                    c.user_keys = KeyOverrides::default();
                }
                c.presets.push(CustomPreset {
                    name: n.clone(),
                    base: *base,
                    overrides: KeyOverrides::from_entries(entries),
                });
                name = n.clone();
            }
            Cmd::EditKeys(KeysEdit::RenamePreset { from, to }) => {
                c.presets.iter_mut().find(|p| p.name == *from).unwrap().name = to.clone();
                if name == *from {
                    name = to.clone();
                }
            }
            Cmd::EditKeys(KeysEdit::DeletePreset { name: n }) => {
                let base = c.presets.iter().find(|p| p.name == *n).unwrap().base;
                c.presets.retain(|p| p.name != *n);
                if name == *n {
                    name = base.name().to_owned();
                }
            }
            Cmd::EditKeys(KeysEdit::SaveMacro(m)) => {
                m.validate().unwrap();
                match c.macros.iter_mut().find(|x| x.id == m.id) {
                    Some(x) => *x = (**m).clone(),
                    None => c.macros.push((**m).clone()),
                }
            }
            Cmd::EditKeys(KeysEdit::DeleteMacro { id }) => c.macros.retain(|m| m.id != *id),
            Cmd::SaveSetting(Setting::Preset(n)) => name = n.clone(),
            _ => continue,
        }
        let next = built(c, &name);
        hs.event(Event::Msg(Msg::Config(Box::new(next))));
    }
}

/// Focuses the first row the filter finds.
fn filter(hs: &mut Harness, text: &str) {
    hs.keys("/");
    typed(hs, text);
    hs.keys("enter");
}

// ----- Snapshots -----

#[test]
fn keys_editor_80x24() {
    let hs = open(mine(), 80, 24);
    insta::assert_snapshot!(hs.text());
}

#[test]
fn keys_editor_120x36() {
    let hs = open(mine(), 120, 36);
    insta::assert_snapshot!(hs.text());
}

#[test]
fn keys_editor_160x40() {
    let hs = open(mine(), 160, 40);
    insta::assert_snapshot!(hs.text());
}

#[test]
fn keys_editor_built_in_120x36() {
    let mut hs = open(UiConfig::default(), 120, 36);
    hs.keys("tab tab tab");
    insta::assert_snapshot!(hs.text());
}

// ----- Opening, sections and keys -----

#[test]
fn opens_from_settings_and_the_palette() {
    let mut hs = harness(mine(), 120, 36);
    hs.keys("g ,");
    let cmds = hs.keys("enter");
    assert!(
        cmds.contains(&Cmd::SaveSetting(Setting::Theme("mono".into()))),
        "Enter on other rows still changes them"
    );
    hs.keys("j enter");
    assert_eq!(*hs.state.screen(), Screen::Keys);
    let text = hs.text();
    assert!(text.contains("Preset: mine (based on fastmail)"), "{text}");
    hs.keys("esc");
    assert_eq!(
        *hs.state.screen(),
        Screen::Settings,
        "back where it came from"
    );
    hs.keys("esc :");
    typed(&mut hs, "edit key bindings");
    hs.keys("enter");
    assert_eq!(*hs.state.screen(), Screen::Keys);
    hs.keys("esc");
    assert_eq!(*hs.state.screen(), Screen::Mail);
    // The Keys row still cycles presets with the arrows.
    hs.keys("g , k k j");
    let cmds = hs.keys("right");
    assert!(cmds.contains(&Cmd::SaveSetting(Setting::Preset("fastmail".into()))));
}

#[test]
fn keys_editor_bindings() {
    let map = Keymap::default();
    for (k, a) in [
        ("enter", Action::KeyReplace),
        ("a", Action::KeyAdd),
        ("x", Action::KeyRemove),
        ("u", Action::KeyReset),
        ("d", Action::PresetDuplicate),
        ("R", Action::PresetRename),
        ("D", Action::PresetDelete),
        ("p", Action::PresetSwitch),
        ("/", Action::Filter),
        ("tab", Action::NextPane),
        ("n", Action::MacroNew),
        ("b", Action::MacroBind),
        ("K", Action::MacroStepUp),
        ("esc", Action::Back),
    ] {
        for preset in [Preset::Fastmail, Preset::Vim] {
            let map = if preset == Preset::Vim {
                Keymap::preset(Preset::Vim)
            } else {
                map.clone()
            };
            let mut pending = Pending::default();
            let key = k.parse().unwrap();
            assert_eq!(
                map.feed(&mut pending, &[Context::Keys, Context::Global], key),
                Resolution::Action {
                    action: a,
                    count: None
                },
                "{preset:?} {k}"
            );
        }
    }
    let mut pending = Pending::default();
    assert_eq!(
        map.feed(
            &mut pending,
            &[Context::Settings, Context::Global],
            "enter".parse().unwrap()
        ),
        Resolution::Action {
            action: Action::SettingOpen,
            count: None
        }
    );
}

#[test]
fn sections_cycle_and_the_footer_describes_the_row() {
    let mut hs = open(mine(), 160, 40);
    let text = hs.text();
    assert!(
        text.contains("Mail  List only  Reader only  Global"),
        "{text}"
    );
    assert!(text.contains("Archive"));
    filter(&mut hs, "move to a");
    assert!(
        hs.text().contains("│ Move to: Move to a mailbox"),
        "{}",
        hs.text()
    );
    // Two rows called pin show their descriptions instead.
    hs.keys("esc");
    filter(&mut hs, "pin");
    let text = hs.text();
    assert!(
        text.contains("Pin or unpin") && text.contains("    Pin   "),
        "{text}"
    );
    hs.keys("esc tab");
    let text = hs.text();
    assert!(text.contains("Next"), "{text}");
    assert!(text.contains("j  Down"), "the list keys of next: {text}");
    hs.keys("shift-tab shift-tab");
    assert!(hs.text().contains("No macros yet"), "{}", hs.text());
    // Clicking a section name shows it.
    let (x, y) = hs.find("Global").unwrap();
    hs.click(x, y);
    assert!(hs.text().contains("Commands"), "{}", hs.text());
    assert!(hs.text().contains(":  ^k  ^p"), "{}", hs.text());
}

// ----- Read-only built-in presets and duplicating -----

#[test]
fn built_in_presets_are_read_only() {
    let mut hs = open(UiConfig::default(), 120, 36);
    for k in ["enter", "a", "x", "u", "R", "D"] {
        let cmds = hs.keys(k);
        assert!(edits(&cmds).is_empty(), "{k}");
        assert_eq!(
            toast(&hs),
            "fastmail is built in. Press d to make your own copy.",
            "{k}"
        );
        assert!(top(&hs).is_none(), "{k}: no capture, prompt or confirm");
    }
    assert!(
        hs.text()
            .contains("fastmail is built in. Press d to make your own copy.")
    );
}

#[test]
fn duplicate_sends_new_preset_and_switches_to_it() {
    let mut hs = open(UiConfig::default(), 120, 36);
    hs.keys("d");
    let Some(Overlay::Prompt(p)) = top(&hs) else {
        panic!("{:?}", top(&hs));
    };
    assert_eq!(p.input.text(), "fastmail-custom");
    let cmds = hs.keys("enter");
    assert_eq!(
        edits(&cmds),
        [&KeysEdit::NewPreset {
            name: "fastmail-custom".into(),
            base: Preset::Fastmail,
            overrides: KeyOverrides::default(),
            take_user: true,
        }]
    );
    assert!(hs.text().contains(", saving"), "{}", hs.text());
    // Edits wait while the change is on its way.
    let more = hs.keys("d");
    assert!(edits(&more).is_empty());
    assert!(toast(&hs).contains("Still saving"));
    shell(&mut hs, &cmds);
    let text = hs.text();
    assert!(
        text.contains("Preset: fastmail-custom (based on fastmail)"),
        "{text}"
    );
    assert!(!text.contains(", saving"));
    // A copy of a custom preset copies its base and changes.
    let mut c = mine();
    c.presets[0].base = Preset::Vim;
    c.presets[0].overrides =
        KeyOverrides::from_toml("[keys.list]\n\"F2\" = \"archive\"\n").unwrap();
    let c = built(c, "mine");
    let mut hs = open(c, 120, 36);
    hs.keys("d");
    let Some(Overlay::Prompt(p)) = top(&hs) else {
        panic!();
    };
    assert_eq!(p.input.text(), "mine-copy");
    let cmds = hs.keys("enter");
    let [
        KeysEdit::NewPreset {
            base, overrides, ..
        },
    ] = edits(&cmds)[..]
    else {
        panic!("{cmds:?}");
    };
    assert_eq!(*base, Preset::Vim);
    assert_eq!(overrides.entries().len(), 1);
    shell(&mut hs, &cmds);
    // A taken or bad name is refused.
    hs.keys("d ctrl-u");
    typed(&mut hs, "vim");
    let cmds = hs.keys("enter");
    assert!(edits(&cmds).is_empty());
    assert!(
        toast(&hs).contains("already a preset called vim"),
        "{}",
        toast(&hs)
    );
}

// ----- Capture -----

#[test]
fn capture_replaces_the_keys_of_a_row() {
    let mut hs = open(mine(), 120, 36);
    filter(&mut hs, "archive");
    hs.keys("enter");
    assert!(matches!(top(&hs), Some(Overlay::CaptureKey(_))));
    assert!(
        hs.text()
            .contains("Press the new key for Archive (Esc cancels)"),
        "{}",
        hs.text()
    );
    let cmds = hs.keys("F2");
    let [
        KeysEdit::SetPresetKeys {
            preset,
            overrides,
            drop_user,
        },
    ] = edits(&cmds)[..]
    else {
        panic!("{cmds:?}");
    };
    assert_eq!(preset, "mine");
    assert!(drop_user.is_empty());
    // The overrides applied to the base give the edited map.
    let rebuilt = Keymap::with_overrides(Preset::Fastmail, overrides).unwrap();
    for c in [Context::List, Context::Reader] {
        assert_eq!(rebuilt.keys_for(Action::Archive, &[c]), Some(seq("F2")));
        assert_eq!(rebuilt.get(c, &seq("y")), None);
    }
    assert!(hs.text().contains(", saving"));
    shell(&mut hs, &cmds);
    let text = hs.text();
    assert!(!text.contains(", saving"));
    assert!(text.contains("F2"), "{text}");
    assert_eq!(
        hs.state
            .config()
            .keymap
            .keys_for(Action::Archive, &[Context::List]),
        Some(seq("F2"))
    );
}

#[test]
fn capture_waits_on_a_prefix_and_esc_cancels() {
    let mut hs = open(mine(), 120, 36);
    filter(&mut hs, "archive");
    hs.keys("enter");
    let cmds = hs.keys(".");
    assert!(edits(&cmds).is_empty());
    assert!(
        matches!(top(&hs), Some(Overlay::CaptureKey(c)) if c.keys.len() == 1),
        ". starts . r in the list, so the capture waits"
    );
    let cmds = hs.keys("x");
    let [KeysEdit::SetPresetKeys { overrides, .. }] = edits(&cmds)[..] else {
        panic!("{cmds:?}");
    };
    let rebuilt = Keymap::with_overrides(Preset::Fastmail, overrides).unwrap();
    assert_eq!(
        rebuilt.keys_for(Action::Archive, &[Context::List]),
        Some(seq(". x"))
    );
    shell(&mut hs, &cmds);
    hs.keys("enter");
    let cmds = hs.keys("esc");
    assert!(edits(&cmds).is_empty());
    assert!(top(&hs).is_none());
    assert_eq!(toast(&hs), "Cancelled");
    // A click cancels too.
    hs.keys("enter");
    let cmds = hs.click(5, 5);
    assert!(edits(&cmds).is_empty());
    assert!(top(&hs).is_none());
}

#[test]
fn a_clash_asks_and_swap_moves_the_key_back() {
    let mut hs = open(mine(), 120, 36);
    filter(&mut hs, "archive");
    hs.keys("enter d");
    let Some(Overlay::Picker(p)) = top(&hs) else {
        panic!("{:?}", top(&hs));
    };
    assert_eq!(p.title, "d is Trash");
    let labels: Vec<&str> = p.items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(
        labels,
        ["Swap: Trash gets y", "Replace: Trash loses d", "Cancel"]
    );
    let cmds = hs.keys("enter");
    let [KeysEdit::SetPresetKeys { overrides, .. }] = edits(&cmds)[..] else {
        panic!("{cmds:?}");
    };
    let rebuilt = Keymap::with_overrides(Preset::Fastmail, overrides).unwrap();
    for c in [Context::List, Context::Reader] {
        assert_eq!(rebuilt.keys_for(Action::Archive, &[c]), Some(seq("d")));
        assert_eq!(rebuilt.keys_for(Action::Trash, &[c]), Some(seq("y")));
    }
    shell(&mut hs, &cmds);
    // Replace takes the key; Cancel leaves everything.
    hs.keys("esc");
    filter(&mut hs, "trash");
    hs.keys("enter s");
    let Some(Overlay::Picker(p)) = top(&hs) else {
        panic!();
    };
    assert_eq!(p.title, "s is Pin");
    let cmds = hs.keys("down enter");
    let [KeysEdit::SetPresetKeys { overrides, .. }] = edits(&cmds)[..] else {
        panic!("{cmds:?}");
    };
    let rebuilt = Keymap::with_overrides(Preset::Fastmail, overrides).unwrap();
    assert_eq!(
        rebuilt.keys_for(Action::TogglePin, &[Context::List]),
        None,
        "pin lost s"
    );
    shell(&mut hs, &cmds);
    hs.keys("enter d");
    let cmds = hs.keys("down down enter");
    assert!(edits(&cmds).is_empty());
    assert_eq!(toast(&hs), "Cancelled");
}

#[test]
fn hiding_a_global_key_warns_and_a_global_prefix_waits() {
    let mut hs = open(mine(), 120, 36);
    filter(&mut hs, "archive");
    let cmds = hs.keys("enter c");
    assert_eq!(edits(&cmds).len(), 1, "a warning only");
    assert!(
        toast(&hs).contains("hides the global compose"),
        "{}",
        toast(&hs)
    );
    shell(&mut hs, &cmds);
    // In Global, g starts the go to keys of the same table, so the
    // capture waits for the next key.
    hs.keys("esc tab tab tab");
    filter(&mut hs, "?");
    let cmds = hs.keys("a g i");
    assert!(edits(&cmds).is_empty());
    let Some(Overlay::Picker(p)) = top(&hs) else {
        panic!("{}", hs.text());
    };
    assert_eq!(p.title, "g i is Inbox");
}

// ----- Guards -----

#[test]
fn the_lockout_guard_keeps_the_last_key_of_help() {
    let mut hs = open(mine(), 120, 36);
    hs.keys("tab tab tab");
    filter(&mut hs, "?");
    assert!(hs.text().contains("Help: Show the keys"), "{}", hs.text());
    let cmds = hs.keys("x");
    assert!(edits(&cmds).is_empty());
    assert!(
        toast(&hs).contains("Help keeps its last key"),
        "{}",
        toast(&hs)
    );
    // A key of the editor itself keeps its last key too.
    hs.keys("esc");
    for _ in 0..8 {
        hs.keys("tab");
    }
    filter(&mut hs, "remove key");
    let cmds = hs.keys("x");
    assert!(edits(&cmds).is_empty());
    assert!(toast(&hs).contains("keeps its last key"), "{}", toast(&hs));
    // Several keys: a picker asks which one goes.
    hs.keys("esc");
    for _ in 0..5 {
        hs.keys("tab");
    }
    filter(&mut hs, "ctrl-k");
    hs.keys("x");
    let Some(Overlay::Picker(p)) = top(&hs) else {
        panic!("{}", hs.text());
    };
    assert_eq!(p.title, "Remove which key from Commands?");
    let cmds = hs.keys("down enter");
    let [KeysEdit::SetPresetKeys { overrides, .. }] = edits(&cmds)[..] else {
        panic!("{cmds:?}");
    };
    let rebuilt = Keymap::with_overrides(Preset::Fastmail, overrides).unwrap();
    assert_eq!(
        rebuilt.keys_of(Context::Global, Action::Palette.into()),
        vec![seq(":"), seq("ctrl-p")]
    );
}

#[test]
fn the_typing_guard_refuses_plain_keys_in_compose() {
    let mut hs = open(mine(), 120, 36);
    for _ in 0..4 {
        hs.keys("tab");
    }
    assert!(
        hs.text().contains("Plain keys type into the field"),
        "{}",
        hs.text()
    );
    filter(&mut hs, "save");
    let cmds = hs.keys("enter x");
    assert!(edits(&cmds).is_empty());
    assert!(
        toast(&hs).contains("types into the field"),
        "{}",
        toast(&hs)
    );
    let cmds = hs.keys("enter F5");
    let [KeysEdit::SetPresetKeys { overrides, .. }] = edits(&cmds)[..] else {
        panic!("{cmds:?}");
    };
    let rebuilt = Keymap::with_overrides(Preset::Fastmail, overrides).unwrap();
    assert_eq!(
        rebuilt.keys_for(Action::SaveDraft, &[Context::Compose]),
        Some(seq("F5"))
    );
}

#[test]
fn reset_and_the_keys_table_are_kept_apart() {
    // [keys] moves archive to F3; the preset's own map still has y.
    let mut c = mine();
    c.user_keys = KeyOverrides::from_toml("[keys.list]\n\"F3\" = \"archive\"\n").unwrap();
    let c = built(c, "mine");
    let mut hs = open(c, 120, 36);
    assert!(
        hs.text().contains("[keys] changes 1 key on top"),
        "{}",
        hs.text()
    );
    // Taking F3 in the preset drops it from [keys].
    filter(&mut hs, "archive");
    let cmds = hs.keys("a F3");
    let [
        KeysEdit::SetPresetKeys {
            overrides,
            drop_user,
            ..
        },
    ] = edits(&cmds)[..]
    else {
        panic!("{cmds:?}");
    };
    assert_eq!(drop_user, &[(Context::List, seq("F3"))]);
    assert!(
        overrides.entries().iter().all(|o| o.keys != seq("y")),
        "the diff is from the preset map, not the effective one"
    );
    shell(&mut hs, &cmds);
    assert!(hs.state.config().user_keys.entries().is_empty());
    let cmds = hs.keys("u");
    let [KeysEdit::SetPresetKeys { overrides, .. }] = edits(&cmds)[..] else {
        panic!("{cmds:?}");
    };
    assert!(overrides.entries().is_empty(), "back to the base");
}

#[test]
fn rename_delete_and_switch_presets() {
    let mut hs = open(mine(), 120, 36);
    hs.keys("R ctrl-u");
    typed(&mut hs, "work");
    let cmds = hs.keys("enter");
    assert_eq!(
        edits(&cmds),
        [&KeysEdit::RenamePreset {
            from: "mine".into(),
            to: "work".into()
        }]
    );
    shell(&mut hs, &cmds);
    assert!(hs.text().contains("Preset: work (based on fastmail)"));
    hs.keys("p");
    let Some(Overlay::Picker(p)) = top(&hs) else {
        panic!();
    };
    assert_eq!(p.title, "Use a preset");
    typed(&mut hs, "vim");
    let cmds = hs.keys("enter");
    assert!(cmds.contains(&Cmd::SaveSetting(Setting::Preset("vim".into()))));
    assert!(hs.text().contains(", saving"));
    shell(&mut hs, &cmds);
    assert!(
        hs.text().contains("Preset: vim (built in)"),
        "{}",
        hs.text()
    );
    hs.keys("p");
    typed(&mut hs, "work");
    let cmds = hs.keys("enter");
    shell(&mut hs, &cmds);
    let cmds = hs.keys("D");
    assert!(edits(&cmds).is_empty());
    let Some(Overlay::Confirm(c)) = top(&hs) else {
        panic!();
    };
    assert_eq!(c.prompt, "Delete the preset work? fastmail takes over.");
    let cmds = hs.keys("y");
    assert_eq!(
        edits(&cmds),
        [&KeysEdit::DeletePreset {
            name: "work".into()
        }]
    );
    shell(&mut hs, &cmds);
    assert!(hs.text().contains("Preset: fastmail (built in)"));
}

#[test]
fn a_refused_edit_ends_saving_and_keeps_the_keys() {
    let mut hs = open(mine(), 120, 36);
    filter(&mut hs, "archive");
    hs.keys("enter F2");
    assert!(hs.text().contains(", saving"));
    hs.event(Event::Msg(Msg::SettingRefused {
        error: "cannot write the file".into(),
    }));
    let text = hs.text();
    assert!(!text.contains(", saving"));
    assert!(toast(&hs).contains("Not changed"));
    assert!(!text.contains("F2"), "{text}");
}

// ----- Macros -----

/// Opens the editor on the Macros section and makes "File receipt" with
/// a mark read step.
fn with_macro(w: u16, h: u16) -> Harness {
    let mut hs = open(UiConfig::default(), w, h);
    hs.keys("shift-tab n");
    typed(&mut hs, "File receipt");
    hs.keys("enter");
    let Some(Overlay::Picker(p)) = top(&hs) else {
        panic!("{}", hs.text());
    };
    assert_eq!(p.title, "Add a step to File receipt");
    let cmds = hs.keys("enter");
    assert_eq!(
        edits(&cmds),
        [&KeysEdit::SaveMacro(Box::new(Macro {
            id: "file-receipt".into(),
            label: "File receipt".into(),
            keys: Vec::new(),
            steps: vec![Step::MarkRead],
        }))]
    );
    shell(&mut hs, &cmds);
    hs
}

fn steps_of(hs: &Harness) -> Vec<Step> {
    hs.state.config().macros[0].steps.clone()
}

#[test]
fn macro_editor_160x40() {
    let mut hs = with_macro(160, 40);
    hs.keys("a");
    typed(&mut hs, "unpin");
    let cmds = hs.keys("enter");
    shell(&mut hs, &cmds);
    insta::assert_snapshot!(hs.text());
}

#[test]
fn a_new_macro_gets_a_plain_step() {
    let mut hs = with_macro(120, 36);
    let text = hs.text();
    assert!(text.contains("Macro: File receipt"), "{text}");
    assert!(text.contains("1. Mark read"), "{text}");
    // The macro works on a built-in preset: nothing asked to duplicate.
    assert!(!toast(&hs).contains("built in"));
    // A second move-like step is refused by validate.
    hs.keys("a");
    typed(&mut hs, "archive");
    let cmds = hs.keys("enter");
    shell(&mut hs, &cmds);
    hs.keys("a");
    typed(&mut hs, "trash");
    let cmds = hs.keys("enter");
    assert!(edits(&cmds).is_empty());
    assert!(toast(&hs).contains("only once"), "{}", toast(&hs));
    // The last step stays.
    let cmds = hs.keys("x");
    shell(&mut hs, &cmds);
    let cmds = hs.keys("x");
    assert!(edits(&cmds).is_empty());
    assert!(toast(&hs).contains("needs a step"), "{}", toast(&hs));
}

#[test]
fn a_folder_step_comes_from_the_folder_picker() {
    let mut hs = with_macro(120, 36);
    hs.keys("a");
    typed(&mut hs, "move to a folder");
    hs.keys("enter");
    let Some(Overlay::Picker(p)) = top(&hs) else {
        panic!("{}", hs.text());
    };
    assert_eq!(p.kind, mejl_ui::picker::PickerKind::MacroFolder);
    assert!(!p.can_create && p.target.is_none(), "it changes no mail");
    typed(&mut hs, "hall");
    let cmds = hs.keys("enter");
    assert!(
        !cmds.iter().any(|c| matches!(c, Cmd::Mutate(_))),
        "picking a folder moves nothing"
    );
    assert_eq!(*hs.state.screen(), Screen::Keys);
    let hall = hs
        .data
        .mailboxes()
        .into_iter()
        .find(|m| m.name == "Hall repairs")
        .unwrap()
        .id;
    let [KeysEdit::SaveMacro(m)] = edits(&cmds)[..] else {
        panic!("{cmds:?}");
    };
    assert_eq!(
        m.steps,
        vec![
            Step::MarkRead,
            Step::Move(FolderRef {
                path: "Projects/Hall repairs".into(),
                id: Some(hall),
            })
        ]
    );
    shell(&mut hs, &cmds);
    assert!(hs.text().contains("2. Move to Projects/Hall repairs"));
    // Labels list only folders.
    hs.keys("a");
    typed(&mut hs, "add a label");
    hs.keys("enter");
    let Some(Overlay::Picker(p)) = top(&hs) else {
        panic!();
    };
    assert!(p.items.iter().all(|i| i.label != "Inbox"));
}

#[test]
fn steps_reorder_with_k_and_j() {
    let mut hs = with_macro(120, 36);
    hs.keys("a");
    typed(&mut hs, "unpin");
    let cmds = hs.keys("enter");
    shell(&mut hs, &cmds);
    assert_eq!(steps_of(&hs), [Step::MarkRead, Step::Unpin]);
    let cmds = hs.keys("K");
    shell(&mut hs, &cmds);
    assert_eq!(steps_of(&hs), [Step::Unpin, Step::MarkRead]);
    let cmds = hs.keys("K");
    assert!(edits(&cmds).is_empty(), "already first");
    let cmds = hs.keys("J");
    shell(&mut hs, &cmds);
    assert_eq!(steps_of(&hs), [Step::MarkRead, Step::Unpin]);
}

#[test]
fn binding_a_macro_key_checks_the_active_keymap() {
    let mut hs = with_macro(120, 36);
    hs.keys("b");
    assert!(
        hs.text()
            .contains("Press a key for the macro File receipt (Esc cancels)")
    );
    hs.keys("T");
    let Some(Overlay::Picker(p)) = top(&hs) else {
        panic!("{}", hs.text());
    };
    assert_eq!(p.title, "T is thread view; the macro will take it");
    let labels: Vec<&str> = p.items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, ["Replace: the macro takes T", "Cancel"]);
    let cmds = hs.keys("enter");
    let [KeysEdit::SaveMacro(m)] = edits(&cmds)[..] else {
        panic!("{cmds:?}");
    };
    assert_eq!(m.keys, vec![seq("T")]);
    shell(&mut hs, &cmds);
    assert!(hs.text().contains("keys: T"), "{}", hs.text());
    // The capture waits on a prefix, and a key of a command asks.
    let cmds = hs.keys("b . r");
    assert!(edits(&cmds).is_empty());
    let Some(Overlay::Picker(p)) = top(&hs) else {
        panic!("{}", hs.text());
    };
    assert_eq!(p.title, ". r is read; the macro will take it");
    // A free key binds at once, from the macro list too.
    hs.keys("esc esc b");
    let cmds = hs.keys("F6");
    let [KeysEdit::SaveMacro(m)] = edits(&cmds)[..] else {
        panic!("{cmds:?}");
    };
    assert_eq!(m.keys, vec![seq("F6")]);
}

#[test]
fn deleting_a_macro_asks_first() {
    let mut hs = with_macro(120, 36);
    hs.keys("esc");
    assert!(hs.text().contains("File receipt"));
    let cmds = hs.keys("x");
    assert!(edits(&cmds).is_empty());
    let Some(Overlay::Confirm(c)) = top(&hs) else {
        panic!("{}", hs.text());
    };
    assert_eq!(c.prompt, "Delete the macro File receipt?");
    let cmds = hs.keys("n");
    assert!(edits(&cmds).is_empty());
    hs.keys("x");
    let cmds = hs.keys("y");
    assert_eq!(
        edits(&cmds),
        [&KeysEdit::DeleteMacro {
            id: "file-receipt".into()
        }]
    );
    shell(&mut hs, &cmds);
    assert!(hs.state.config().macros.is_empty());
    assert!(hs.text().contains("No macros yet"));
}

#[test]
fn renaming_a_macro_keeps_its_id() {
    let mut hs = with_macro(120, 36);
    hs.keys("r ctrl-u");
    typed(&mut hs, "Receipts");
    let cmds = hs.keys("enter");
    let [KeysEdit::SaveMacro(m)] = edits(&cmds)[..] else {
        panic!("{cmds:?}");
    };
    assert_eq!(
        (m.id.as_str(), m.label.as_str()),
        ("file-receipt", "Receipts")
    );
}

#[test]
fn a_preset_row_cannot_take_a_macro_key() {
    let mut c = mine();
    c.macros = vec![Macro {
        id: "file-receipt".into(),
        label: "File receipt".into(),
        keys: vec![seq("F6")],
        steps: vec![Step::MarkRead],
    }];
    let c = built(c, "mine");
    let mut hs = open(c, 120, 36);
    filter(&mut hs, "archive");
    let cmds = hs.keys("enter F6");
    assert!(edits(&cmds).is_empty());
    assert!(
        toast(&hs).contains("F6 runs the macro File receipt in the list"),
        "{}",
        toast(&hs)
    );
}
