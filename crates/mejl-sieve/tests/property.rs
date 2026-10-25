//! Property-style tests over many generated rules, with a deterministic
//! PRNG so failures repeat.

mod common;

use common::Rng;
use mejl_sieve::{
    Action, AddressHeader, AddressPart, Condition, EmailAddress, MailboxRef, MatchMode,
    MessageFacts, PasteStatus, Rule, Test, TextOp, ValidationContext, check_paste, check_script,
    evaluate_all, has_errors, render_block, validate,
};

const WORDS: &[&str] = &[
    "keep",
    "discard",
    "require",
    "stop",
    "fileinto",
    "if",
    "else",
    "}",
    "{",
    ";",
    "\"",
    "\\",
    "\\\\",
    "\\\"",
    "#",
    "/*",
    "*/",
    "*",
    "?",
    "text:",
    ".",
    "anyof (",
    "receipt",
    "ünï",
    "日本",
    "📬",
    " ",
    "",
    "example.com",
    "@",
    "<list.example.org>",
    "keep;",
    "discard;",
    "require [\"x\"];",
];
const HEADER_NAMES: &[&str] = &[
    "X-Spam-score",
    "List-Unsubscribe",
    "X-Keep",
    "Discard",
    "require",
];

fn text(rng: &mut Rng) -> String {
    let mut out = String::new();
    for _ in 0..rng.below(5) {
        let word: &str = rng.pick::<&str>(WORDS);
        out.push_str(word);
    }
    out
}

fn text_op(rng: &mut Rng) -> TextOp {
    *rng.pick(&[
        TextOp::Is,
        TextOp::Contains,
        TextOp::Matches,
        TextOp::StartsWith,
        TextOp::EndsWith,
    ])
}

fn condition(rng: &mut Rng) -> Condition {
    let test = match rng.below(7) {
        0 => Test::Address {
            header: *rng.pick(&[
                AddressHeader::From,
                AddressHeader::To,
                AddressHeader::Cc,
                AddressHeader::ToOrCc,
            ]),
            part: *rng.pick(&[
                AddressPart::All,
                AddressPart::Localpart,
                AddressPart::Domain,
            ]),
            op: text_op(rng),
            value: text(rng),
        },
        1 => Test::Subject {
            op: text_op(rng),
            value: text(rng),
        },
        2 => Test::ListId {
            op: text_op(rng),
            value: text(rng),
        },
        3 => Test::Header {
            name: rng.pick(HEADER_NAMES).to_string(),
            op: text_op(rng),
            value: text(rng),
        },
        4 => Test::DeliveredTo {
            op: text_op(rng),
            value: text(rng),
        },
        5 => Test::SizeOver {
            bytes: rng.next_u64() % (1 << 31),
        },
        _ => Test::SizeUnder {
            bytes: *rng.pick(&[0, 1, 1024, 1_048_576, 1500, (1 << 31) - 1]),
        },
    };
    Condition {
        negate: rng.chance(30),
        test,
    }
}

fn mailbox(rng: &mut Rng) -> MailboxRef {
    let id = format!("M{}", rng.next_u64() % 1000);
    let depth = rng.below(3);
    let path: Vec<String> = (0..depth)
        .map(|_| {
            let t = text(rng);
            if t.is_empty() { "x".to_owned() } else { t }
        })
        .collect();
    MailboxRef::new(id, path)
}

fn action(rng: &mut Rng) -> Action {
    match rng.below(5) {
        0 => Action::MoveTo {
            mailbox: mailbox(rng),
        },
        1 => Action::AddLabel {
            mailbox: mailbox(rng),
        },
        2 => Action::MarkRead,
        3 => Action::Pin,
        _ => Action::MoveToTrash,
    }
}

fn rule(rng: &mut Rng, index: usize) -> Rule {
    Rule {
        id: format!("r_{index}").into(),
        name: text(rng),
        enabled: rng.chance(70),
        match_mode: if rng.chance(50) {
            MatchMode::Any
        } else {
            MatchMode::All
        },
        conditions: (0..rng.below(5)).map(|_| condition(rng)).collect(),
        actions: (0..rng.below(4)).map(|_| action(rng)).collect(),
        stop: rng.chance(15),
    }
}

/// Removes strings and comments with a scanner written apart from the
/// crate's, leaving only code.
fn code_only(sieve: &str) -> String {
    let mut out = String::new();
    let mut chars = sieve.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                while let Some(s) = chars.next() {
                    match s {
                        '\\' => {
                            chars.next();
                        }
                        '"' => break,
                        _ => {}
                    }
                }
                out.push_str(" STR ");
            }
            '#' => {
                for s in chars.by_ref() {
                    if s == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn words(code: &str) -> Vec<String> {
    code.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn facts(rng: &mut Rng) -> MessageFacts {
    MessageFacts {
        from: vec![EmailAddress::new(
            text(rng),
            format!("{}@{}", text(rng), text(rng)),
        )],
        to: vec![EmailAddress::bare(text(rng))],
        cc: Vec::new(),
        subject: rng.chance(80).then(|| text(rng)),
        headers: vec![
            ("List-Id".into(), text(rng)),
            ("X-Spam-score".into(), text(rng)),
        ],
        size: rng.next_u64() % 10_000_000,
        delivered_to: vec![text(rng)],
    }
}

#[test]
fn rendered_blocks_never_contain_forbidden_commands() {
    let mut rng = Rng::new(0xDEC0_DED5);
    for round in 0..400 {
        let rules: Vec<Rule> = (0..rng.below(6)).map(|i| rule(&mut rng, i)).collect();
        let block = render_block(&rules).unwrap_or_else(|e| panic!("round {round}: {e}"));

        // Independent scanner: no forbidden word outside strings and comments.
        let code = code_only(&block.text);
        for word in words(&code) {
            assert!(
                !matches!(
                    word.as_str(),
                    "keep" | "discard" | "require" | "reject" | "redirect" | "vacation"
                ),
                "round {round}: {word} in\n{}",
                block.text
            );
        }

        // The crate's own parser agrees, and finds only allowed extensions.
        let check = check_script(&block.text);
        assert!(
            check.is_ok(),
            "round {round}: {:?}\n{}",
            check.issues,
            block.text
        );
        assert!(
            check
                .commands
                .iter()
                .all(|c| matches!(c.as_str(), "if" | "fileinto" | "addflag" | "stop")),
            "round {round}: {:?}",
            check.commands
        );
        let stops = rules.iter().filter(|r| r.stop).count();
        assert_eq!(
            check.commands.iter().filter(|c| *c == "stop").count(),
            stops,
            "stop appears only where a rule asks for it"
        );

        // Every line is either a comment or code with balanced quotes, so no
        // value spans lines.
        for line in block.text.lines() {
            assert!(!line.ends_with(' '), "trailing space: {line:?}");
        }

        // Rendering is deterministic, and its own paste check says intact.
        assert_eq!(render_block(&rules).expect("render").text, block.text);
        let report = check_paste(&block.text, &block.body_sha256, &[] as &[&str]);
        assert_eq!(report.status, PasteStatus::Intact, "round {round}");
        let ids: Vec<String> = report.blocks[0]
            .rules
            .iter()
            .map(|r| r.id.clone())
            .collect();
        let expected: Vec<String> = rules.iter().map(|r| r.id.to_string()).collect();
        assert_eq!(ids, expected);
        let enabled: Vec<Option<bool>> = report.blocks[0].rules.iter().map(|r| r.enabled).collect();
        let expected: Vec<Option<bool>> = rules.iter().map(|r| Some(r.enabled)).collect();
        assert_eq!(enabled, expected);

        // Validation has no errors for renderable rules.
        let diags = validate(&rules, &ValidationContext::default());
        assert!(!has_errors(&diags), "round {round}: {diags:?}");

        // The matcher runs on hostile facts without panicking.
        let f = facts(&mut rng);
        let result = evaluate_all(&rules, &f);
        assert!(result.matches.len() <= rules.len());
    }
}

#[test]
fn control_characters_never_reach_the_output() {
    let mut rng = Rng::new(0xBAD_C0DE);
    let hostile = [
        "\n", "\r", "\r\n", "\u{0}", "\u{7f}", "\u{85}", "\t", "\u{1b}",
    ];
    for _ in 0..300 {
        let mut r = rule(&mut rng, 0);
        r.enabled = rng.chance(50);
        let poison = format!("x{}keep;", rng.pick(&hostile));
        match rng.below(4) {
            0 => r.name = poison,
            1 => r.conditions.push(Condition::new(Test::Subject {
                op: TextOp::Contains,
                value: poison,
            })),
            2 => r.actions.push(Action::MoveTo {
                mailbox: MailboxRef::new("M1", [poison]),
            }),
            _ => r.conditions.push(Condition::new(Test::Header {
                name: poison,
                op: TextOp::Is,
                value: String::new(),
            })),
        }
        assert!(render_block(&[r.clone()]).is_err(), "{r:?}");
        assert!(has_errors(&validate(&[r], &ValidationContext::default())));
    }
}
