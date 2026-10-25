//! Drift check for a block the user pastes back from Fastmail.
//!
//! mejl cannot read the server script. To check what is there, the user
//! copies the whole script, or just the custom section, from Fastmail's
//! "Edit custom Sieve code" screen and pastes it into mejl. This module finds
//! mejl's block by its markers, checks the body hash and reads back the rule
//! ids and enabled flags.
//!
//! Lines are compared after normalizing CRLF and CR to LF and trimming
//! trailing whitespace, since browsers and editors change both freely.
//! Markers are found after trimming leading whitespace too.

use crate::render::{FORMAT_VERSION, body_hash};

const BEGIN_PREFIX: &str = "# >>> mejl managed block v";
const BEGIN_SUFFIX: &str = " (do not edit by hand) >>>";
const END_PREFIX: &str = "# <<< mejl managed block v";
const END_SUFFIX: &str = " <<<";
const RULE_PREFIX: &str = "# mejl:rule ";

/// The overall result of a paste check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteStatus {
    /// No mejl marker anywhere in the text.
    Missing,
    /// One block, unedited, matching the current render.
    Intact,
    /// One block whose body no longer matches the hash in its end marker.
    EditedByHand,
    /// One unedited block from an earlier render.
    OlderVersion {
        /// Index of the matching hash in the `previous` list given to
        /// [`check_paste`].
        index: usize,
    },
    /// One unedited block whose hash mejl does not know, for example one
    /// rendered on another machine.
    UnknownVersion,
    /// More than one begin marker. Only one block should be on Fastmail.
    Duplicate {
        /// Number of begin markers found.
        count: usize,
    },
    /// A begin marker without an end marker, or the other way round. Often
    /// a paste that was cut short.
    Incomplete,
    /// One block in a format version this mejl does not understand.
    UnsupportedFormat {
        /// The version in the marker.
        version: u32,
    },
}

/// A block found in the pasted text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundBlock {
    /// One-based line of the begin marker, if there was one.
    pub begin_line: Option<usize>,
    /// One-based line of the end marker, if there was one.
    pub end_line: Option<usize>,
    /// Format version from the marker, if it could be read.
    pub format_version: Option<u32>,
    /// Hash written in the end marker, lowercase.
    pub claimed_sha256: Option<String>,
    /// Hash of the body as pasted. Only set when both markers are present.
    pub actual_sha256: Option<String>,
    /// Rule comment lines in the body, in order.
    pub rules: Vec<FoundRule>,
}

impl FoundBlock {
    /// True when both markers are present and the body matches the claimed
    /// hash.
    pub fn hash_matches(&self) -> bool {
        matches!((&self.claimed_sha256, &self.actual_sha256), (Some(c), Some(a)) if c == a)
    }
}

/// A rule read back from a `# mejl:rule` comment line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundRule {
    /// The rule id as written.
    pub id: String,
    /// The enabled flag as written. `None` when the line had no readable
    /// `enabled=0` or `enabled=1`.
    pub enabled: Option<bool>,
}

/// The full result of a paste check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteReport {
    /// Overall status.
    pub status: PasteStatus,
    /// Every block or stray marker found, in text order.
    pub blocks: Vec<FoundBlock>,
}

/// Checks pasted text against the current render and earlier renders.
///
/// `current_sha256` is the body hash of what mejl would render now.
/// `previous` lists the hashes of earlier renders, in any order, for example
/// newest first from the `rule_render` table. Hash comparison ignores ASCII
/// case.
pub fn check_paste<S: AsRef<str>>(
    pasted: &str,
    current_sha256: &str,
    previous: &[S],
) -> PasteReport {
    let blocks = find_blocks(pasted);
    let begins = blocks.iter().filter(|b| b.begin_line.is_some()).count();
    let status = if blocks.is_empty() {
        PasteStatus::Missing
    } else if begins > 1 {
        PasteStatus::Duplicate { count: begins }
    } else if blocks.len() > 1 || blocks[0].begin_line.is_none() || blocks[0].end_line.is_none() {
        PasteStatus::Incomplete
    } else {
        let block = &blocks[0];
        match block.format_version {
            Some(FORMAT_VERSION) => {
                if !block.hash_matches() {
                    PasteStatus::EditedByHand
                } else {
                    let claimed = block.claimed_sha256.as_deref().unwrap_or_default();
                    if claimed.eq_ignore_ascii_case(current_sha256) {
                        PasteStatus::Intact
                    } else if let Some(index) = previous
                        .iter()
                        .position(|h| h.as_ref().eq_ignore_ascii_case(claimed))
                    {
                        PasteStatus::OlderVersion { index }
                    } else {
                        PasteStatus::UnknownVersion
                    }
                }
            }
            Some(version) => PasteStatus::UnsupportedFormat { version },
            // A marker whose version cannot be read was edited.
            None => PasteStatus::EditedByHand,
        }
    };
    PasteReport { status, blocks }
}

/// Splits on LF, CRLF or a lone CR.
fn normalized_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        match rest.find(['\r', '\n']) {
            Some(pos) => {
                lines.push(&rest[..pos]);
                let skip = if rest[pos..].starts_with("\r\n") {
                    2
                } else {
                    1
                };
                rest = &rest[pos + skip..];
            }
            None => {
                lines.push(rest);
                rest = "";
            }
        }
    }
    lines
}

/// The version in a begin marker. `Some(None)` for a begin marker whose
/// version is unreadable.
fn parse_begin(line: &str) -> Option<Option<u32>> {
    let rest = line.strip_prefix(BEGIN_PREFIX)?;
    Some(
        rest.strip_suffix(BEGIN_SUFFIX)
            .and_then(|v| v.parse::<u32>().ok()),
    )
}

/// Version and hash from an end marker.
fn parse_end(line: &str) -> Option<(Option<u32>, Option<String>)> {
    let rest = line.strip_prefix(END_PREFIX)?;
    let Some((version, rest)) = rest.split_once(" sha256=") else {
        return Some((None, None));
    };
    let version = version.parse::<u32>().ok();
    let hash = rest
        .strip_suffix(END_SUFFIX)
        .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase);
    Some((version, hash))
}

fn parse_rule_line(line: &str) -> Option<FoundRule> {
    let rest = line.strip_prefix(RULE_PREFIX)?;
    let rest = rest.strip_prefix("id=")?;
    let id_len = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(rest.len());
    let id = rest.get(..id_len)?.to_owned();
    if id.is_empty() {
        return None;
    }
    // The name is a quoted string that may hold "enabled=1", so read the
    // flag from the end of the line.
    let enabled = if line.ends_with(" enabled=1") {
        Some(true)
    } else if line.ends_with(" enabled=0") {
        Some(false)
    } else {
        None
    };
    Some(FoundRule { id, enabled })
}

fn find_blocks(text: &str) -> Vec<FoundBlock> {
    let lines = normalized_lines(text);
    let mut blocks = Vec::new();
    // The open block, with the index of its first body line.
    let mut open: Option<(FoundBlock, usize)> = None;
    for (index, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        let number = index + 1;
        if let Some(version) = parse_begin(line) {
            if let Some((unterminated, _)) = open.take() {
                blocks.push(unterminated);
            }
            open = Some((
                FoundBlock {
                    begin_line: Some(number),
                    end_line: None,
                    format_version: version,
                    claimed_sha256: None,
                    actual_sha256: None,
                    rules: Vec::new(),
                },
                index + 1,
            ));
        } else if let Some((version, hash)) = parse_end(line) {
            match open.take() {
                Some((mut block, body_start)) => {
                    let body = lines.get(body_start..index).unwrap_or_default();
                    block.end_line = Some(number);
                    block.actual_sha256 = Some(body_hash(body.iter().copied()));
                    block.claimed_sha256 = hash;
                    if block.format_version != version {
                        block.format_version = None;
                    }
                    blocks.push(block);
                }
                None => blocks.push(FoundBlock {
                    begin_line: None,
                    end_line: Some(number),
                    format_version: version,
                    claimed_sha256: hash,
                    actual_sha256: None,
                    rules: Vec::new(),
                }),
            }
        } else if let Some((block, _)) = open.as_mut()
            && let Some(rule) = parse_rule_line(line)
        {
            block.rules.push(rule);
        }
    }
    if let Some((unterminated, _)) = open {
        blocks.push(unterminated);
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_splitting_handles_every_ending() {
        assert_eq!(normalized_lines("a\nb\r\nc\rd"), ["a", "b", "c", "d"]);
        assert_eq!(normalized_lines("a\n"), ["a"]);
        assert_eq!(normalized_lines("a\n\nb"), ["a", "", "b"]);
        assert_eq!(normalized_lines("\r\n"), [""]);
        assert!(normalized_lines("").is_empty());
    }

    #[test]
    fn marker_parsing() {
        assert_eq!(
            parse_begin("# >>> mejl managed block v1 (do not edit by hand) >>>"),
            Some(Some(1))
        );
        assert_eq!(
            parse_begin("# >>> mejl managed block v2 (do not edit by hand) >>>"),
            Some(Some(2))
        );
        assert_eq!(parse_begin("# >>> mejl managed block vX >>>"), Some(None));
        assert_eq!(parse_begin("# mejl:rule id=x"), None);
        let hash = "A".repeat(64);
        assert_eq!(
            parse_end(&format!("# <<< mejl managed block v1 sha256={hash} <<<")),
            Some((Some(1), Some("a".repeat(64))))
        );
        assert_eq!(
            parse_end("# <<< mejl managed block v1 sha256=abc <<<"),
            Some((Some(1), None))
        );
        assert_eq!(
            parse_end("# <<< mejl managed block v1 <<<"),
            Some((None, None))
        );
    }

    #[test]
    fn rule_line_parsing() {
        assert_eq!(
            parse_rule_line("# mejl:rule id=r_18aa name=\"Newsletters\" enabled=1"),
            Some(FoundRule {
                id: "r_18aa".into(),
                enabled: Some(true)
            })
        );
        assert_eq!(
            parse_rule_line("# mejl:rule id=r_1 name=\"x enabled=1\" enabled=0"),
            Some(FoundRule {
                id: "r_1".into(),
                enabled: Some(false)
            })
        );
        assert_eq!(
            parse_rule_line("# mejl:rule id=r_1 name=\"x\""),
            Some(FoundRule {
                id: "r_1".into(),
                enabled: None
            })
        );
        assert_eq!(parse_rule_line("# mejl:rule id= name=\"x\""), None);
        assert_eq!(parse_rule_line("# mejl:rule name=\"x\""), None);
    }
}
