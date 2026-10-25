//! Golden tests for the rendered block.

mod common;

use common::{from_domain_is, mockup_rules};
use mejl_sieve::{
    Action, AddressHeader, AddressPart, Condition, MailboxRef, MatchMode, Rule, Test, TextOp,
    check_script, render::end_marker, render_block,
};

/// The block shown in the Sieve preview on the rules screen of
/// docs/mockups.html, apart from the hash, which the mockup shortens.
const MOCKUP_BODY: &str = r#"# mejl:rule id=r_18aa name="Newsletters" enabled=1
if address :domain :is "from" "news.example.net" {
  fileinto :mailboxid "M2a90" "INBOX.Newsletters";
}
# mejl:rule id=r_2c41 name="Receipts to folder" enabled=1
if anyof (address :domain :is "from" "shop.example.com",
          header :contains "subject" "receipt") {
  addflag "\\Seen";
  fileinto :mailboxid "M7d1e" "INBOX.Receipts";
}
# mejl:rule id=r_5d07 name="Garden club label" enabled=1
if header :contains "list-id" "garden-club.example.org" {
  fileinto :copy :mailboxid "M4c3b" "INBOX.Garden club";
}
# mejl:rule id=r_77e2 name="Old promo" enabled=0
if false # address :domain :is "from" "promo.example.com"
{
  fileinto :specialuse "\\Trash" "INBOX.Trash";
}
"#;

/// Computed outside the crate with `sha256sum` over `MOCKUP_BODY`. If this
/// changes, every block users have pasted reads as edited, so it must only
/// change together with a format version bump.
const MOCKUP_BODY_SHA256: &str = "cfb73c46e5151e064db6ac5cb7df938c8ea24040d80d5fd7713c025f732a857d";

#[test]
fn mockup_block_matches_the_mockup() {
    let block = render_block(&mockup_rules()).expect("render");
    let expected = format!(
        "# >>> mejl managed block v1 (do not edit by hand) >>>\n{MOCKUP_BODY}{}\n",
        end_marker(&block.body_sha256)
    );
    assert_eq!(block.text, expected);
    assert_eq!(block.line_count(), 21);
    let spans: Vec<_> = block.rules.iter().map(|r| r.lines.clone()).collect();
    assert_eq!(spans, [2..=5, 6..=11, 12..=15, 16..=20]);
    let check = check_script(&block.text);
    assert!(check.is_ok(), "{:?}", check.issues);
}

#[test]
fn mockup_hash_is_stable() {
    let block = render_block(&mockup_rules()).expect("render");
    assert_eq!(block.body_sha256, MOCKUP_BODY_SHA256);
    let again = render_block(&mockup_rules()).expect("render");
    assert_eq!(block, again);
}

#[test]
fn mockup_block_golden() {
    let block = render_block(&mockup_rules()).expect("render");
    insta::assert_snapshot!("mockup_block", block.text);
}

fn mailbox(id: &str, path: &[&str]) -> MailboxRef {
    MailboxRef::new(id, path.iter().copied())
}

fn address(header: AddressHeader, part: AddressPart, op: TextOp, value: &str) -> Condition {
    Condition::new(Test::Address {
        header,
        part,
        op,
        value: value.into(),
    })
}

#[test]
fn every_condition_golden() {
    use AddressHeader::*;
    use AddressPart::*;
    use TextOp::*;
    let mut rules = Vec::new();
    let conditions = vec![
        (
            "from_all_is",
            address(From, All, Is, "orders@shop.example.com"),
        ),
        (
            "from_domain_is",
            address(From, Domain, Is, "shop.example.com"),
        ),
        ("from_localpart", address(From, Localpart, Is, "orders")),
        ("to_contains", address(To, All, Contains, "team")),
        (
            "cc_matches",
            address(Cc, All, Matches, "*@lists.example.net"),
        ),
        (
            "to_or_cc_starts",
            address(ToOrCc, All, StartsWith, "billing+"),
        ),
        (
            "to_or_cc_ends",
            address(ToOrCc, Domain, EndsWith, ".example.org"),
        ),
        (
            "subject_is",
            Condition::new(Test::Subject {
                op: Is,
                value: "Weekly digest".into(),
            }),
        ),
        (
            "list_id",
            Condition::new(Test::ListId {
                op: Contains,
                value: "<garden-club.example.org>".into(),
            }),
        ),
        (
            "named_header",
            Condition::new(Test::Header {
                name: "X-Spam-score".into(),
                op: StartsWith,
                value: "0.".into(),
            }),
        ),
        (
            "delivered_to",
            Condition::new(Test::DeliveredTo {
                op: Is,
                value: "shop.k3x9@example.org".into(),
            }),
        ),
        (
            "size_over_k",
            Condition::new(Test::SizeOver { bytes: 102_400 }),
        ),
        (
            "size_under_m",
            Condition::new(Test::SizeUnder {
                bytes: 5 * 1_048_576,
            }),
        ),
        (
            "size_over_odd",
            Condition::new(Test::SizeOver { bytes: 1500 }),
        ),
        (
            "negated",
            Condition::negated(Test::Subject {
                op: Contains,
                value: "unsubscribe".into(),
            }),
        ),
    ];
    for (i, (name, condition)) in conditions.into_iter().enumerate() {
        let mut rule = Rule::new(format!("r_c{i:02}"), name);
        rule.conditions.push(condition);
        rule.actions.push(Action::Pin);
        rules.push(rule);
    }
    let block = render_block(&rules).expect("render");
    assert!(check_script(&block.text).is_ok());
    insta::assert_snapshot!("every_condition", block.text);
}

#[test]
fn every_action_golden() {
    let mut rule = Rule::new("r_act", "Every action in user order");
    rule.conditions.push(from_domain_is("example.com"));
    rule.actions = vec![
        Action::MoveToTrash,
        Action::MoveTo {
            mailbox: mailbox("Mw1", &["Work", "Clients"]),
        },
        Action::AddLabel {
            mailbox: mailbox("Ml1", &["Label"]),
        },
        Action::Pin,
        Action::MarkRead,
    ];
    let mut inbox = Rule::new("r_inbox", "Back to the Inbox");
    inbox.actions.push(Action::MoveTo {
        mailbox: mailbox("Minbox", &[]),
    });
    let block = render_block(&[rule, inbox]).expect("render");
    assert!(check_script(&block.text).is_ok());
    insta::assert_snapshot!("every_action", block.text);
}

#[test]
fn escaping_and_unicode_golden() {
    let mut rule = Rule::new("r_esc", r#"Quote " and backslash \ and ✓ ünïcødé"#);
    rule.match_mode = MatchMode::All;
    rule.conditions = vec![
        Condition::new(Test::Subject {
            op: TextOp::Is,
            value: r#"say "hi" \o/"#.into(),
        }),
        Condition::new(Test::Subject {
            op: TextOp::StartsWith,
            value: r"50% off* now? \ok".into(),
        }),
        Condition::new(Test::Subject {
            op: TextOp::EndsWith,
            value: "*".into(),
        }),
        Condition::new(Test::Subject {
            op: TextOp::Matches,
            value: r"literal \* star and ? any".into(),
        }),
        Condition::new(Test::Header {
            name: "X-Unicode-Free".into(),
            op: TextOp::Contains,
            value: "日本語 – émoji 📬".into(),
        }),
        Condition::new(Test::Subject {
            op: TextOp::Contains,
            value: "keep; discard; require \"x\"; } # stop;".into(),
        }),
    ];
    rule.actions.push(Action::MoveTo {
        mailbox: mailbox("M_q-1", &["Quotes \"and\" \\slashes", "Ünder"]),
    });
    let mut disabled = rule.clone();
    disabled.id = "r_esc_off".into();
    disabled.enabled = false;
    let block = render_block(&[rule, disabled]).expect("render");
    let check = check_script(&block.text);
    assert!(check.is_ok(), "{:?}", check.issues);
    assert_eq!(check.commands, ["if", "fileinto", "if", "fileinto"]);
    insta::assert_snapshot!("escaping_and_unicode", block.text);
}

#[test]
fn disabled_and_stop_golden() {
    let mut empty_on = Rule::new("r_on", "No conditions");
    empty_on.actions.push(Action::MarkRead);
    let mut empty_off = empty_on.clone();
    empty_off.id = "r_off".into();
    empty_off.name = "No conditions, disabled".into();
    empty_off.enabled = false;

    let mut multi_off = Rule::new("r_multi", "All of three, disabled");
    multi_off.enabled = false;
    multi_off.match_mode = MatchMode::All;
    multi_off.conditions = vec![
        from_domain_is("example.net"),
        Condition::negated(Test::SizeOver { bytes: 1024 }),
        Condition::new(Test::Subject {
            op: TextOp::Contains,
            value: "invoice".into(),
        }),
    ];
    multi_off.actions.push(Action::MoveToTrash);
    multi_off.stop = true;

    let mut multi_on = multi_off.clone();
    multi_on.id = "r_multi_on".into();
    multi_on.name = "All of three, stops".into();
    multi_on.enabled = true;

    let mut stop_only = Rule::new("r_stop", "Stop only");
    stop_only.conditions.push(from_domain_is("example.org"));
    stop_only.stop = true;

    let block =
        render_block(&[empty_on, empty_off, multi_off, multi_on, stop_only]).expect("render");
    let check = check_script(&block.text);
    assert!(check.is_ok(), "{:?}", check.issues);
    insta::assert_snapshot!("disabled_and_stop", block.text);
}

#[test]
fn disabling_a_rule_keeps_the_ids_and_changes_the_hash() {
    let mut rules = mockup_rules();
    let on = render_block(&rules).expect("render");
    rules[0].enabled = false;
    let off = render_block(&rules).expect("render");
    assert_ne!(on.body_sha256, off.body_sha256);
    let ids_on: Vec<_> = on.rules.iter().map(|r| r.id.clone()).collect();
    let ids_off: Vec<_> = off.rules.iter().map(|r| r.id.clone()).collect();
    assert_eq!(ids_on, ids_off);
}

#[test]
fn rule_order_changes_the_output() {
    let rules = mockup_rules();
    let mut reversed = rules.clone();
    reversed.reverse();
    let a = render_block(&rules).expect("render");
    let b = render_block(&reversed).expect("render");
    assert_ne!(a.text, b.text);
    assert_ne!(a.body_sha256, b.body_sha256);
}
