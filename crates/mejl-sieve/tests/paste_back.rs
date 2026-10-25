//! Paste-back and drift check cases.

mod common;

use common::{Rng, mockup_rules};
use mejl_sieve::{FoundRule, PasteStatus, check_paste, render_block};

const NO_HISTORY: &[&str] = &[];

/// A fictional, abbreviated script in the shape of Fastmail's generated
/// code, with mejl's block in the middle custom section.
fn fastmail_script(block: &str) -> String {
    format!(
        r#"require ["fileinto", "reject", "vacation", "envelope", "body", "relational", "regex", "subaddress", "copy", "mailbox", "mboxmetadata", "servermetadata", "date", "index", "comparator-i;ascii-numeric", "variables", "imap4flags", "editheader", "duplicate", "vacation-seconds", "fcc", "vnd.cyrus.jmapquery", "vnd.cyrus.log", "mailboxid", "special-use", "vnd.cyrus.snooze", "vnd.cyrus.imip", "vnd.cyrus.implicit_keep_target"];

### Sieve generated for blocked senders
if address :is "from" "spammer@example.net" {{
  fileinto :specialuse "\\Trash" "INBOX.Trash";
  stop;
}}

### Sieve generated for disabled masked email addresses
if header :contains "Fastmail-MaskedEmail" " state=disabled" {{
  addflag "\\Seen";
  fileinto :specialuse "\\Trash" "INBOX.Trash";
  stop;
}}

### Sieve generated for spam protection
if not string :is "${{spam}}" "N" {{
  set "spam" "N";
}}

{block}
### Calculate rule actions
# No user configured rules

### Execute rule actions
if string :is "${{stop}}" "Y" {{
  stop;
}}
"#
    )
}

fn rendered() -> mejl_sieve::RenderedBlock {
    render_block(&mockup_rules()).expect("render")
}

#[test]
fn intact_block_alone() {
    let block = rendered();
    let report = check_paste(&block.text, &block.body_sha256, NO_HISTORY);
    assert_eq!(report.status, PasteStatus::Intact);
    assert_eq!(report.blocks.len(), 1);
    let found = &report.blocks[0];
    assert_eq!(found.begin_line, Some(1));
    assert_eq!(found.end_line, Some(21));
    assert_eq!(found.format_version, Some(1));
    assert!(found.hash_matches());
    assert_eq!(
        found.rules,
        [
            FoundRule {
                id: "r_18aa".into(),
                enabled: Some(true)
            },
            FoundRule {
                id: "r_2c41".into(),
                enabled: Some(true)
            },
            FoundRule {
                id: "r_5d07".into(),
                enabled: Some(true)
            },
            FoundRule {
                id: "r_77e2".into(),
                enabled: Some(false)
            },
        ]
    );
}

#[test]
fn intact_inside_the_full_fastmail_script() {
    let block = rendered();
    let script = fastmail_script(&block.text);
    let report = check_paste(&script, &block.body_sha256, NO_HISTORY);
    assert_eq!(report.status, PasteStatus::Intact);
    assert_eq!(report.blocks[0].rules.len(), 4);
    assert!(report.blocks[0].begin_line > Some(1));
}

#[test]
fn crlf_cr_trailing_spaces_and_indentation_of_markers_are_tolerated() {
    let block = rendered();
    let crlf = fastmail_script(&block.text).replace('\n', "\r\n");
    assert_eq!(
        check_paste(&crlf, &block.body_sha256, NO_HISTORY).status,
        PasteStatus::Intact
    );
    let cr = block.text.replace('\n', "\r");
    assert_eq!(
        check_paste(&cr, &block.body_sha256, NO_HISTORY).status,
        PasteStatus::Intact
    );
    let trailing: String = block.text.lines().map(|l| format!("{l}   \t\n")).collect();
    assert_eq!(
        check_paste(&trailing, &block.body_sha256, NO_HISTORY).status,
        PasteStatus::Intact
    );
    let no_final_newline = block.text.trim_end();
    assert_eq!(
        check_paste(no_final_newline, &block.body_sha256, NO_HISTORY).status,
        PasteStatus::Intact
    );
    let mut lines: Vec<String> = block.text.lines().map(str::to_owned).collect();
    let last = lines.len() - 1;
    lines[0] = format!("    {}", lines[0]);
    lines[last] = format!("\t{}", lines[last]);
    let indented = lines.join("\n");
    assert_eq!(
        check_paste(&indented, &block.body_sha256, NO_HISTORY).status,
        PasteStatus::Intact
    );
}

#[test]
fn hand_edits_are_detected() {
    let block = rendered();
    let edits = [
        block.text.replace("news.example.net", "news.example.com"),
        block.text.replace("enabled=0", "enabled=1"),
        block.text.replace("if false # ", "if "),
        block.text.replace("  addflag", "    addflag"),
        block.text.replace("\n}\n", "\n}\n\n"),
        block
            .text
            .replace("# mejl:rule id=r_5d07", "# mejl:rule id=r_5d08"),
        block
            .text
            .replace("  fileinto :copy", "  keep;\n  fileinto :copy"),
    ];
    for edited in edits {
        assert_ne!(edited, block.text);
        let report = check_paste(&edited, &block.body_sha256, NO_HISTORY);
        assert_eq!(report.status, PasteStatus::EditedByHand, "{edited}");
        assert!(!report.blocks[0].hash_matches());
    }
    let bad_hash = block.text.replace(&block.body_sha256, &"0".repeat(64));
    assert_eq!(
        check_paste(&bad_hash, &block.body_sha256, NO_HISTORY).status,
        PasteStatus::EditedByHand
    );
    let short_hash = block.text.replace(&block.body_sha256, "9c2e41b7d0");
    assert_eq!(
        check_paste(&short_hash, &block.body_sha256, NO_HISTORY).status,
        PasteStatus::EditedByHand
    );
}

#[test]
fn edited_rules_are_still_read_back() {
    let block = rendered();
    let edited = block.text.replace("enabled=0", "enabled=1");
    let report = check_paste(&edited, &block.body_sha256, NO_HISTORY);
    assert_eq!(report.blocks[0].rules[3].enabled, Some(true));
}

#[test]
fn missing_block() {
    let block = rendered();
    for text in [
        String::new(),
        "\n\n".to_owned(),
        fastmail_script(""),
        "# mejl:rule id=r_1 name=\"x\" enabled=1\n".to_owned(),
    ] {
        let report = check_paste(&text, &block.body_sha256, NO_HISTORY);
        assert_eq!(report.status, PasteStatus::Missing);
        assert!(report.blocks.is_empty());
    }
}

#[test]
fn duplicated_blocks() {
    let block = rendered();
    let twice = format!("{}\n{}", block.text, block.text);
    let report = check_paste(&fastmail_script(&twice), &block.body_sha256, NO_HISTORY);
    assert_eq!(report.status, PasteStatus::Duplicate { count: 2 });
    assert_eq!(report.blocks.len(), 2);
    assert!(report.blocks.iter().all(|b| b.hash_matches()));

    let old = {
        let mut rules = mockup_rules();
        rules.pop();
        render_block(&rules).expect("render")
    };
    let mixed = format!("{}{}", old.text, block.text);
    let report = check_paste(&mixed, &block.body_sha256, &[old.body_sha256.as_str()]);
    assert_eq!(report.status, PasteStatus::Duplicate { count: 2 });
}

#[test]
fn older_and_unknown_versions() {
    let mut rules = mockup_rules();
    let v1 = render_block(&rules).expect("render");
    rules[3].enabled = true;
    let v2 = render_block(&rules).expect("render");
    rules.remove(0);
    let v3 = render_block(&rules).expect("render");

    let history = [v2.body_sha256.clone(), v1.body_sha256.clone()];
    assert_eq!(
        check_paste(&v1.text, &v3.body_sha256, &history).status,
        PasteStatus::OlderVersion { index: 1 }
    );
    assert_eq!(
        check_paste(&v2.text, &v3.body_sha256, &history).status,
        PasteStatus::OlderVersion { index: 0 }
    );
    assert_eq!(
        check_paste(&v3.text, &v3.body_sha256, &history).status,
        PasteStatus::Intact
    );
    assert_eq!(
        check_paste(&v1.text, &v3.body_sha256, NO_HISTORY).status,
        PasteStatus::UnknownVersion
    );
    // Hash comparison ignores case.
    let upper = v3.body_sha256.to_ascii_uppercase();
    assert_eq!(
        check_paste(&v3.text, &upper, NO_HISTORY).status,
        PasteStatus::Intact
    );
}

#[test]
fn incomplete_blocks() {
    let block = rendered();
    let lines: Vec<&str> = block.text.lines().collect();
    let no_end = lines[..lines.len() - 1].join("\n");
    let report = check_paste(&no_end, &block.body_sha256, NO_HISTORY);
    assert_eq!(report.status, PasteStatus::Incomplete);
    assert_eq!(report.blocks[0].end_line, None);
    assert_eq!(report.blocks[0].rules.len(), 4);

    let no_begin = lines[1..].join("\n");
    let report = check_paste(&no_begin, &block.body_sha256, NO_HISTORY);
    assert_eq!(report.status, PasteStatus::Incomplete);
    assert_eq!(report.blocks[0].begin_line, None);

    let stray_end = format!("{}{}\n", block.text, lines[lines.len() - 1]);
    assert_eq!(
        check_paste(&stray_end, &block.body_sha256, NO_HISTORY).status,
        PasteStatus::Incomplete
    );
}

#[test]
fn version_markers() {
    let block = rendered();
    let v2 = block.text.replace("managed block v1", "managed block v2");
    assert_eq!(
        check_paste(&v2, &block.body_sha256, NO_HISTORY).status,
        PasteStatus::UnsupportedFormat { version: 2 }
    );
    let mismatched = block
        .text
        .replacen("managed block v1", "managed block v2", 1);
    assert_eq!(
        check_paste(&mismatched, &block.body_sha256, NO_HISTORY).status,
        PasteStatus::EditedByHand
    );
    let garbled = block
        .text
        .replacen("managed block v1 (", "managed block vX (", 1);
    assert_eq!(
        check_paste(&garbled, &block.body_sha256, NO_HISTORY).status,
        PasteStatus::EditedByHand
    );
}

/// Random edits never panic, and "intact" only comes back when the
/// normalized body is unchanged.
#[test]
fn mutation_fuzz() {
    let block = rendered();
    let original_body: Vec<String> = normalized_body(&block.text).expect("body");
    let inserts = [
        "x", "\r", "\n", "\r\n", "é", "📬", "\"", "\\", "#", " ", "\t", "\u{0}", "<<<", ">>>",
    ];
    let mut rng = Rng::new(0x5EED_CAFE);
    for _ in 0..3000 {
        let mut text: Vec<char> = block.text.chars().collect();
        for _ in 0..=rng.below(3) {
            match rng.below(5) {
                0 if !text.is_empty() => {
                    let at = rng.below(text.len());
                    text.remove(at);
                }
                1 => {
                    let at = rng.below(text.len() + 1);
                    for (k, c) in rng.pick(&inserts).chars().enumerate() {
                        text.insert(at + k, c);
                    }
                }
                2 => {
                    let joined: String = text.iter().collect();
                    let mut lines: Vec<&str> = joined.lines().collect();
                    if !lines.is_empty() {
                        let at = rng.below(lines.len());
                        lines.remove(at);
                    }
                    text = lines.join("\n").chars().collect();
                }
                3 => {
                    let joined: String = text.iter().collect();
                    let lines: Vec<&str> = joined.lines().collect();
                    if !lines.is_empty() {
                        let dup = lines[rng.below(lines.len())].to_owned();
                        let mut out = lines.join("\n");
                        out.push('\n');
                        out.push_str(&dup);
                        text = out.chars().collect();
                    }
                }
                _ => {
                    let joined: String = text.iter().collect();
                    text = joined.replace('\n', "\r\n").chars().collect();
                }
            }
        }
        let mutated: String = text.into_iter().collect();
        let report = check_paste(&mutated, &block.body_sha256, NO_HISTORY);
        if report.status == PasteStatus::Intact {
            assert_eq!(
                normalized_body(&mutated).as_ref(),
                Some(&original_body),
                "intact after a real edit: {mutated:?}"
            );
        }
    }
}

/// The body between the first begin and end markers, normalized the way the
/// hash sees it. Written independently of the crate.
fn normalized_body(text: &str) -> Option<Vec<String>> {
    let lines: Vec<String> = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(|l| l.trim_end().to_owned())
        .collect();
    let begin = lines
        .iter()
        .position(|l| l.trim_start().starts_with("# >>> mejl managed block"))?;
    let end = lines
        .iter()
        .position(|l| l.trim_start().starts_with("# <<< mejl managed block"))?;
    (begin < end).then(|| lines[begin + 1..end].to_vec())
}
