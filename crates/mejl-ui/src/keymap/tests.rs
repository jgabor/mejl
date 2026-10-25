use super::*;

fn k(s: &str) -> Key {
    s.parse().unwrap()
}

const LIST: &[Context] = &[Context::List, Context::Global];
const READER: &[Context] = &[Context::Reader, Context::Global];

fn run(map: &Keymap, contexts: &[Context], keys: &str) -> Vec<Resolution> {
    let mut pending = Pending::default();
    keys.split_whitespace()
        .map(|key| map.feed(&mut pending, contexts, k(key)))
        .collect()
}

fn act(action: Action) -> Resolution {
    Resolution::Action {
        action,
        count: None,
    }
}

#[test]
fn presets_have_no_conflicts_or_legacy_only_keys() {
    for preset in [Preset::Fastmail, Preset::Vim] {
        let map = Keymap::preset(preset);
        let problems = map.check();
        assert!(problems.is_empty(), "{preset:?}: {problems:#?}");
        let count: usize = Context::ALL.iter().map(|c| map.bindings(*c).len()).sum();
        let listed = preset.bindings().len();
        assert_eq!(
            count, listed,
            "{preset:?}: a preset binding failed to parse or repeats"
        );
    }
}

#[test]
fn single_keys_resolve_per_context() {
    let map = Keymap::default();
    assert_eq!(run(&map, LIST, "j"), [act(Action::Next)]);
    assert_eq!(run(&map, READER, "down"), [act(Action::ScrollDown)]);
    assert_eq!(run(&map, LIST, "down"), [act(Action::Next)]);
    assert_eq!(run(&map, LIST, "y"), [act(Action::Archive)]);
    assert_eq!(run(&map, LIST, "?"), [act(Action::Help)]);
    assert_eq!(run(&map, LIST, "ctrl-k"), [act(Action::Palette)]);
    assert_eq!(run(&map, LIST, "F12"), [Resolution::Unbound]);
}

#[test]
fn prefixes_wait_without_timeout() {
    let map = Keymap::default();
    assert_eq!(
        run(&map, LIST, "g i"),
        [Resolution::Pending, act(Action::GoInbox)]
    );
    assert_eq!(
        run(&map, LIST, "* a"),
        [Resolution::Pending, act(Action::SelectAll)]
    );
    assert_eq!(
        run(&map, LIST, ". r"),
        [Resolution::Pending, act(Action::MarkRead)]
    );
    // A key that does not follow the prefix cancels and does nothing else.
    assert_eq!(
        run(&map, LIST, "g y"),
        [Resolution::Pending, Resolution::Cancelled]
    );
    assert_eq!(
        run(&map, LIST, "g esc"),
        [Resolution::Pending, Resolution::Cancelled]
    );
    // After a cancel the next key works normally.
    assert_eq!(
        run(&map, LIST, "g esc j"),
        [
            Resolution::Pending,
            Resolution::Cancelled,
            act(Action::Next)
        ]
    );
    // b is Fastmail's snooze prefix; every follow-up says it is web only.
    assert_eq!(
        run(&map, LIST, "b 3"),
        [Resolution::Pending, act(Action::Snooze)]
    );
    assert_eq!(map.prefix_label(&"b".parse().unwrap()), "snooze (web only)");
    // Esc alone is a normal key.
    assert_eq!(run(&map, LIST, "esc"), [act(Action::Back)]);
}

#[test]
fn which_key_lists_continuations_in_preset_order() {
    let map = Keymap::default();
    let g = map.continuations(&[k("g")], LIST);
    let keys: Vec<String> = g.iter().map(|c| c.key.to_string()).collect();
    assert_eq!(
        keys,
        ["i", "a", "s", "d", "t", "j", "f", "g", "r", ",", "m", "p"]
    );
    assert_eq!(g[0].target, ContinuationTarget::Action(Action::GoInbox));
    assert_eq!(map.prefix_label(&"g".parse().unwrap()), "go to");
    assert_eq!(map.prefix_label(&"q q".parse().unwrap()), "more");
    let star = map.continuations(&[k("*")], LIST);
    assert_eq!(star.len(), 2);
}

#[test]
fn hints_use_the_first_unshadowed_binding() {
    let map = Keymap::default();
    assert_eq!(
        map.keys_for(Action::Open, LIST).map(|s| s.to_string()),
        Some("o".into())
    );
    assert_eq!(
        map.keys_for(Action::Palette, LIST).map(|s| s.to_string()),
        Some(":".into())
    );
    assert_eq!(
        map.keys_for(Action::GoInbox, LIST).map(|s| s.to_string()),
        Some("g i".into())
    );
    assert_eq!(map.keys_for(Action::Expand, LIST), None);
}

#[test]
fn vim_counts_and_reader_scrolling() {
    let map = Keymap::preset(Preset::Vim);
    assert_eq!(
        run(&map, LIST, "5 j"),
        [
            Resolution::Pending,
            Resolution::Action {
                action: Action::Next,
                count: Some(5)
            }
        ]
    );
    assert_eq!(
        run(&map, LIST, "1 0 j"),
        [
            Resolution::Pending,
            Resolution::Pending,
            Resolution::Action {
                action: Action::Next,
                count: Some(10)
            }
        ]
    );
    assert_eq!(run(&map, READER, "j"), [act(Action::ScrollDown)]);
    assert_eq!(run(&map, READER, "J"), [act(Action::Next)]);
    assert_eq!(run(&map, LIST, "a"), [act(Action::Archive)]);
    assert_eq!(run(&map, LIST, "u"), [act(Action::Undo)]);
    assert_eq!(
        run(&map, LIST, "r r"),
        [Resolution::Pending, act(Action::ReplyAll)]
    );
    assert_eq!(
        run(&map, LIST, "space r"),
        [Resolution::Pending, act(Action::GoRules)]
    );
    // 0 alone is not a count.
    assert_eq!(run(&map, LIST, "0"), [Resolution::Unbound]);
    // Fastmail has no counts: digits are unbound.
    assert_eq!(run(&Keymap::default(), LIST, "5"), [Resolution::Unbound]);
    // A digit a context binds is that binding, not a count.
    let masked = &[Context::Masked, Context::Global];
    assert_eq!(run(&map, masked, "1"), [act(Action::MaskedShowEnabled)]);
    assert_eq!(run(&map, masked, "5"), [act(Action::MaskedCycleSilent)]);
    assert_eq!(run(&map, masked, "6"), [act(Action::MaskedCycleCreatedBy)]);
}

#[test]
fn overrides_bind_unbind_and_report_errors() {
    let doc = r#"
        [keys.list]
        "e" = "archive"
        "y" = "none"
        "g x" = "go-archive"
        [keys.global]
        "ctrl-o" = "help"
    "#;
    let overrides = KeyOverrides::from_toml(doc).unwrap();
    let map = Keymap::with_overrides(Preset::Fastmail, &overrides).unwrap();
    assert_eq!(run(&map, LIST, "e"), [act(Action::Archive)]);
    assert_eq!(run(&map, LIST, "y"), [Resolution::Unbound]);
    assert_eq!(
        run(&map, LIST, "g x"),
        [Resolution::Pending, act(Action::GoArchive)]
    );
    // The global g table still works through the list's own g prefix.
    assert_eq!(
        run(&map, LIST, "g i"),
        [Resolution::Pending, act(Action::GoInbox)]
    );
    assert_eq!(run(&map, LIST, "ctrl-o"), [act(Action::Help)]);
    // The reader keeps its own y.
    assert_eq!(run(&map, READER, "y"), [act(Action::Archive)]);
}

#[test]
fn unbinding_a_prefix_frees_it() {
    let doc = r#"
        [keys.list]
        "*" = "none"
    "#;
    let mut map =
        Keymap::with_overrides(Preset::Fastmail, &KeyOverrides::from_toml(doc).unwrap()).unwrap();
    map.bind(Context::List, "*".parse().unwrap(), Action::SelectAll);
    assert_eq!(run(&map, LIST, "*"), [act(Action::SelectAll)]);
    assert!(map.check().is_empty());
}

#[test]
fn override_errors_are_all_reported() {
    let doc = r#"
        [keys.lsit]
        "j" = "next"
        [keys.list]
        "y" = "archve"
        "bogus-key" = "archive"
        "k" = 3
    "#;
    let errors = KeyOverrides::from_toml(doc).unwrap_err();
    assert_eq!(errors.len(), 4, "{errors:#?}");
    let text: Vec<String> = errors.iter().map(ToString::to_string).collect();
    assert!(
        text.iter()
            .any(|t| t.contains("[keys.lsit] is not a known context"))
    );
    assert!(text.iter().any(|t| t.contains("did you mean \"archive\"")));
    assert!(text.iter().any(|t| t.contains("unknown key name")));
    assert!(text.iter().any(|t| t.contains("must be an action name")));
    assert!(matches!(
        KeyOverrides::from_toml("keys = 1").unwrap_err()[0],
        KeymapError::NotATable { .. }
    ));
    assert!(matches!(
        KeyOverrides::from_toml("[keys").unwrap_err()[0],
        KeymapError::Toml(_)
    ));
    assert_eq!(KeyOverrides::from_toml("").unwrap().entries().len(), 0);
}

#[test]
fn prefix_clash_is_an_error() {
    let doc = r#"
        [keys.list]
        "*" = "archive"
    "#;
    let errors = Keymap::with_overrides(Preset::Fastmail, &KeyOverrides::from_toml(doc).unwrap())
        .unwrap_err();
    assert_eq!(errors.len(), 1);
    let text = errors[0].to_string();
    assert!(text.starts_with("error: [keys.list] \"*\""), "{text}");
    assert!(text.contains("* a"), "{text}");
}

#[test]
fn shadowing_and_legacy_keys_are_warnings() {
    let doc = r#"
        [keys.list]
        "g" = "archive"
        "ctrl-i" = "help"
        "alt-x" = "archive"
        "q" = "trash"
    "#;
    let map =
        Keymap::with_overrides(Preset::Fastmail, &KeyOverrides::from_toml(doc).unwrap()).unwrap();
    let w: Vec<String> = map.warnings().iter().map(ToString::to_string).collect();
    assert!(
        w.iter()
            .any(|t| t.contains("\"g\"") && t.contains("hides the global g prefix")),
        "{w:#?}"
    );
    assert!(
        w.iter().any(|t| t.contains("ctrl-i") && t.contains("Tab")),
        "{w:#?}"
    );
    assert!(
        w.iter().any(|t| t.contains("alt-x") && t.contains("Esc")),
        "{w:#?}"
    );
    assert!(
        w.iter()
            .any(|t| t.contains("\"q\"") && t.contains("hides the global quit")),
        "{w:#?}"
    );
    assert!(
        map.warnings()
            .iter()
            .all(|c| c.severity == Severity::Warning)
    );
    // The list's g now archives right away.
    assert_eq!(run(&map, LIST, "g"), [act(Action::Archive)]);
}

#[test]
fn help_context_is_modal() {
    let map = Keymap::default();
    let help = &[Context::Help];
    assert_eq!(run(&map, help, "esc"), [act(Action::Back)]);
    assert_eq!(run(&map, help, "j"), [Resolution::Unbound]);
}
