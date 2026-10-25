//! Validation and lints for a list of rules.
//!
//! Errors block rendering. Warnings point at rules that render fine but
//! probably do not do what the user wants.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::model::{Action, MailboxRef, MatchMode, Rule, RuleId, Test, TextOp};
use crate::render::{RenderErrorKind, render_block, rule_problems};
use crate::script::check_script;

/// How serious a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// The block cannot be rendered, or would be unsafe.
    Error,
    /// The block renders, but the rule probably does not do what was meant.
    Warning,
}

/// Machine-readable diagnostic kinds, for tests and for the UI to pick an
/// icon or a fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticCode {
    /// The rule id is not usable in the block.
    InvalidRuleId,
    /// Two rules share an id.
    DuplicateRuleId,
    /// A text field holds a control character.
    ControlCharacter,
    /// A header name is not an RFC 5322 field name.
    InvalidHeaderName,
    /// A mailbox id is not a JMAP id.
    InvalidMailboxId,
    /// A mailbox path has an empty segment.
    EmptyMailboxSegment,
    /// A size limit is too large.
    SizeOutOfRange,
    /// The rendered block failed the script check. This means a renderer
    /// bug, since validated rules should always render clean Sieve.
    Script,
    /// The rule has no conditions, so it matches every message.
    NoConditions,
    /// The conditions match practically every message.
    MatchesEverything,
    /// The conditions can never match.
    NeverMatches,
    /// The rule has no actions and no stop.
    NoActions,
    /// A mailbox id is not among the account's known mailboxes. The rule
    /// falls back to the mailbox name.
    UnknownMailbox,
    /// Actions that work against each other, such as Trash and a move.
    ConflictingActions,
    /// The same action twice.
    DuplicateAction,
    /// The rule stops the script, which also skips Fastmail's own rules.
    StopSkipsFastmail,
    /// A mailbox name holds `.`, which the fallback name cannot express.
    AmbiguousFallbackName,
}

/// One finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// The rule it is about. `None` for findings about the whole block.
    pub rule_id: Option<RuleId>,
    /// How serious it is.
    pub severity: Severity,
    /// What kind of finding.
    pub code: DiagnosticCode,
    /// A sentence for the user.
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let level = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        match &self.rule_id {
            Some(id) => write!(f, "{level}: rule {id}: {}", self.message),
            None => write!(f, "{level}: {}", self.message),
        }
    }
}

/// What the validator knows about the account.
#[derive(Debug, Clone, Default)]
pub struct ValidationContext {
    /// JMAP ids of the account's mailboxes. `None` skips the unknown
    /// mailbox check, for example before the first sync.
    pub known_mailbox_ids: Option<HashSet<String>>,
}

impl ValidationContext {
    /// A context that knows these mailbox ids.
    pub fn with_mailboxes<I, S>(ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            known_mailbox_ids: Some(ids.into_iter().map(Into::into).collect()),
        }
    }
}

/// True when any diagnostic is an error.
pub fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(|d| d.severity == Severity::Error)
}

/// Validates rules in render order. Diagnostics come grouped by rule, in
/// rule order, then any block-level findings.
pub fn validate(rules: &[Rule], context: &ValidationContext) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let mut id_counts: HashMap<&str, usize> = HashMap::new();
    for rule in rules {
        *id_counts.entry(rule.id.as_str()).or_default() += 1;
    }
    let mut reported_duplicates = HashSet::new();
    for rule in rules {
        let mut push = |severity, code, message: String| {
            out.push(Diagnostic {
                rule_id: Some(rule.id.clone()),
                severity,
                code,
                message,
            });
        };
        for problem in rule_problems(rule) {
            let code = match &problem {
                RenderErrorKind::InvalidRuleId => DiagnosticCode::InvalidRuleId,
                RenderErrorKind::DuplicateRuleId => DiagnosticCode::DuplicateRuleId,
                RenderErrorKind::ControlCharacter { .. } => DiagnosticCode::ControlCharacter,
                RenderErrorKind::InvalidHeaderName { .. } => DiagnosticCode::InvalidHeaderName,
                RenderErrorKind::InvalidMailboxId { .. } => DiagnosticCode::InvalidMailboxId,
                RenderErrorKind::EmptyMailboxSegment => DiagnosticCode::EmptyMailboxSegment,
                RenderErrorKind::SizeOutOfRange { .. } => DiagnosticCode::SizeOutOfRange,
            };
            push(Severity::Error, code, capitalize(&problem.to_string()));
        }
        if id_counts.get(rule.id.as_str()).copied().unwrap_or(0) > 1
            && reported_duplicates.insert(rule.id.as_str())
        {
            push(
                Severity::Error,
                DiagnosticCode::DuplicateRuleId,
                "Another rule has the same id".to_owned(),
            );
        }
        lint_conditions(rule, &mut push);
        lint_actions(rule, context, &mut push);
    }

    if !has_errors(&out) {
        match render_block(rules) {
            Ok(block) => {
                for issue in check_script(&block.text).issues {
                    let rule_id = block
                        .rules
                        .iter()
                        .find(|r| r.lines.contains(&issue.line))
                        .map(|r| r.id.clone());
                    out.push(Diagnostic {
                        rule_id,
                        severity: Severity::Error,
                        code: DiagnosticCode::Script,
                        message: format!("Rendered Sieve failed the check: {issue}"),
                    });
                }
            }
            Err(err) => out.push(Diagnostic {
                rule_id: Some(err.rule_id),
                severity: Severity::Error,
                code: DiagnosticCode::Script,
                message: capitalize(&err.kind.to_string()),
            }),
        }
    }
    out
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// What a condition can say about every message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// True for practically every message that has the tested header.
    Always,
    /// False for every message.
    Never,
    /// Depends on the message.
    Some,
}

fn text_reach(op: TextOp, value: &str) -> Reach {
    let all_stars = !value.is_empty() && value.bytes().all(|b| b == b'*');
    match op {
        TextOp::Contains | TextOp::StartsWith | TextOp::EndsWith if value.is_empty() => {
            Reach::Always
        }
        TextOp::Matches if all_stars => Reach::Always,
        _ => Reach::Some,
    }
}

fn condition_reach(test: &Test, negate: bool) -> Reach {
    let reach = match test {
        Test::Address { op, value, .. }
        | Test::Subject { op, value }
        | Test::ListId { op, value }
        | Test::Header { op, value, .. }
        | Test::DeliveredTo { op, value } => {
            let reach = text_reach(*op, value);
            // A negated "always" is false only when the header exists, so
            // it is not a reliable "never". Keep the check one-sided.
            if negate && reach == Reach::Always {
                return Reach::Some;
            }
            reach
        }
        Test::SizeOver { bytes: 0 } => Reach::Always,
        Test::SizeUnder { bytes: 0 } => Reach::Never,
        Test::SizeOver { .. } | Test::SizeUnder { .. } => Reach::Some,
    };
    match (reach, negate) {
        (Reach::Always, true) => Reach::Never,
        (Reach::Never, true) => Reach::Always,
        (reach, _) => reach,
    }
}

fn lint_conditions(rule: &Rule, push: &mut impl FnMut(Severity, DiagnosticCode, String)) {
    if rule.conditions.is_empty() {
        push(
            Severity::Warning,
            DiagnosticCode::NoConditions,
            "Rule has no conditions, so it matches every message".to_owned(),
        );
        return;
    }
    let reaches: Vec<Reach> = rule
        .conditions
        .iter()
        .map(|c| condition_reach(&c.test, c.negate))
        .collect();
    let (always, never) = match rule.match_mode {
        MatchMode::Any => (
            reaches.contains(&Reach::Always),
            reaches.iter().all(|r| *r == Reach::Never),
        ),
        MatchMode::All => (
            reaches.iter().all(|r| *r == Reach::Always),
            reaches.contains(&Reach::Never),
        ),
    };
    if never {
        push(
            Severity::Warning,
            DiagnosticCode::NeverMatches,
            "Rule can never match".to_owned(),
        );
    } else if always {
        push(
            Severity::Warning,
            DiagnosticCode::MatchesEverything,
            "Rule matches practically every message".to_owned(),
        );
    }
}

fn lint_actions(
    rule: &Rule,
    context: &ValidationContext,
    push: &mut impl FnMut(Severity, DiagnosticCode, String),
) {
    if rule.actions.is_empty() && !rule.stop {
        push(
            Severity::Warning,
            DiagnosticCode::NoActions,
            "Rule has no actions".to_owned(),
        );
    }
    if rule.stop {
        push(
            Severity::Warning,
            DiagnosticCode::StopSkipsFastmail,
            "Stop also skips Fastmail's own rules, notifications, calendar handling and vacation replies"
                .to_owned(),
        );
    }

    let mut seen: Vec<&Action> = Vec::new();
    for action in &rule.actions {
        if seen.contains(&action) {
            push(
                Severity::Warning,
                DiagnosticCode::DuplicateAction,
                format!("Action {} appears more than once", action_name(action)),
            );
        } else {
            seen.push(action);
        }
    }

    let moves: Vec<&MailboxRef> = rule
        .actions
        .iter()
        .filter_map(|a| match a {
            Action::MoveTo { mailbox } => Some(mailbox),
            _ => None,
        })
        .collect();
    let labels: Vec<&MailboxRef> = rule
        .actions
        .iter()
        .filter_map(|a| match a {
            Action::AddLabel { mailbox } => Some(mailbox),
            _ => None,
        })
        .collect();
    let trash = rule.actions.contains(&Action::MoveToTrash);

    let mut move_ids: Vec<&str> = moves.iter().map(|m| m.id.as_str()).collect();
    move_ids.sort_unstable();
    move_ids.dedup();
    if move_ids.len() > 1 {
        push(
            Severity::Warning,
            DiagnosticCode::ConflictingActions,
            "Rule moves to more than one mailbox, so a copy lands in each".to_owned(),
        );
    }
    if trash && !moves.is_empty() {
        push(
            Severity::Warning,
            DiagnosticCode::ConflictingActions,
            "Rule moves to a mailbox and to Trash, so a copy lands in both".to_owned(),
        );
    }
    if trash && !labels.is_empty() {
        push(
            Severity::Warning,
            DiagnosticCode::ConflictingActions,
            "Rule adds a label and moves to Trash, so the labeled copy is kept".to_owned(),
        );
    }
    for label in &labels {
        if moves.iter().any(|m| m.id == label.id) {
            push(
                Severity::Warning,
                DiagnosticCode::DuplicateAction,
                format!("Rule both moves to and labels {}", label.display_path()),
            );
        }
    }

    let mut checked = HashSet::new();
    for mailbox in moves.iter().chain(labels.iter()) {
        if !checked.insert(mailbox.id.as_str()) {
            continue;
        }
        if let Some(known) = &context.known_mailbox_ids
            && !known.contains(&mailbox.id)
        {
            push(
                Severity::Warning,
                DiagnosticCode::UnknownMailbox,
                format!(
                    "Mailbox {} ({}) is not in this account, so Sieve falls back to the name {}",
                    mailbox.display_path(),
                    mailbox.id,
                    mailbox.fallback_name()
                ),
            );
        }
        if mailbox.path.iter().any(|s| s.contains('.')) {
            push(
                Severity::Warning,
                DiagnosticCode::AmbiguousFallbackName,
                format!(
                    "Mailbox {} has a dot in its name, so the fallback name {} may point elsewhere",
                    mailbox.display_path(),
                    mailbox.fallback_name()
                ),
            );
        }
    }
}

fn action_name(action: &Action) -> String {
    match action {
        Action::MoveTo { mailbox } => format!("move to {}", mailbox.display_path()),
        Action::AddLabel { mailbox } => format!("add label {}", mailbox.display_path()),
        Action::MarkRead => "mark read".to_owned(),
        Action::Pin => "pin".to_owned(),
        Action::MoveToTrash => "move to Trash".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AddressHeader, AddressPart, Condition};

    fn codes(diags: &[Diagnostic]) -> Vec<DiagnosticCode> {
        diags.iter().map(|d| d.code).collect()
    }

    fn good_rule(id: &str) -> Rule {
        let mut rule = Rule::new(id, "Good");
        rule.conditions.push(Condition::new(Test::Subject {
            op: TextOp::Contains,
            value: "receipt".into(),
        }));
        rule.actions.push(Action::MoveTo {
            mailbox: MailboxRef::new("M7d1e", ["Receipts"]),
        });
        rule
    }

    #[test]
    fn a_good_rule_is_clean() {
        let ctx = ValidationContext::with_mailboxes(["M7d1e"]);
        assert!(validate(&[good_rule("r_1")], &ctx).is_empty());
        assert!(validate(&[], &ctx).is_empty());
    }

    #[test]
    fn errors_carry_rule_ids() {
        let mut bad = good_rule("r bad");
        bad.name = "a\nb".into();
        bad.conditions.push(Condition::new(Test::Header {
            name: "Bad Name".into(),
            op: TextOp::Is,
            value: "x\u{0}".into(),
        }));
        bad.conditions
            .push(Condition::new(Test::SizeOver { bytes: u64::MAX }));
        bad.actions.push(Action::AddLabel {
            mailbox: MailboxRef::new("", ["", "x"]),
        });
        let diags = validate(&[bad], &ValidationContext::default());
        assert!(has_errors(&diags));
        assert!(
            diags
                .iter()
                .all(|d| d.rule_id == Some(RuleId::new("r bad")))
        );
        let found = codes(&diags);
        for code in [
            DiagnosticCode::InvalidRuleId,
            DiagnosticCode::ControlCharacter,
            DiagnosticCode::InvalidHeaderName,
            DiagnosticCode::SizeOutOfRange,
            DiagnosticCode::InvalidMailboxId,
            DiagnosticCode::EmptyMailboxSegment,
        ] {
            assert!(found.contains(&code), "{code:?} in {found:?}");
        }
        assert!(!found.contains(&DiagnosticCode::Script));
    }

    #[test]
    fn duplicate_ids_are_reported_once() {
        let diags = validate(
            &[good_rule("r_1"), good_rule("r_1")],
            &ValidationContext::default(),
        );
        assert_eq!(codes(&diags), [DiagnosticCode::DuplicateRuleId]);
    }

    #[test]
    fn condition_lints() {
        let ctx = ValidationContext::default();
        let mut rule = good_rule("r_1");
        rule.conditions.clear();
        assert_eq!(
            codes(&validate(&[rule.clone()], &ctx)),
            [DiagnosticCode::NoConditions]
        );

        let cases: Vec<(MatchMode, Vec<Condition>, Option<DiagnosticCode>)> = vec![
            (
                MatchMode::All,
                vec![Condition::new(Test::Subject {
                    op: TextOp::Contains,
                    value: String::new(),
                })],
                Some(DiagnosticCode::MatchesEverything),
            ),
            (
                MatchMode::Any,
                vec![
                    Condition::new(Test::Subject {
                        op: TextOp::Is,
                        value: "x".into(),
                    }),
                    Condition::new(Test::Address {
                        header: AddressHeader::From,
                        part: AddressPart::All,
                        op: TextOp::Matches,
                        value: "**".into(),
                    }),
                ],
                Some(DiagnosticCode::MatchesEverything),
            ),
            (
                MatchMode::All,
                vec![
                    Condition::new(Test::Subject {
                        op: TextOp::Is,
                        value: "x".into(),
                    }),
                    Condition::new(Test::SizeOver { bytes: 0 }),
                ],
                None,
            ),
            (
                MatchMode::All,
                vec![
                    Condition::new(Test::Subject {
                        op: TextOp::Is,
                        value: "x".into(),
                    }),
                    Condition::new(Test::SizeUnder { bytes: 0 }),
                ],
                Some(DiagnosticCode::NeverMatches),
            ),
            (
                MatchMode::Any,
                vec![Condition::negated(Test::SizeOver { bytes: 0 })],
                Some(DiagnosticCode::NeverMatches),
            ),
            (
                MatchMode::Any,
                vec![Condition::negated(Test::SizeUnder { bytes: 0 })],
                Some(DiagnosticCode::MatchesEverything),
            ),
            (
                // Not "contains nothing" is false only when the header
                // exists, so it is not flagged as never.
                MatchMode::Any,
                vec![Condition::negated(Test::ListId {
                    op: TextOp::Contains,
                    value: String::new(),
                })],
                None,
            ),
            (
                MatchMode::Any,
                vec![Condition::new(Test::Subject {
                    op: TextOp::Matches,
                    value: String::new(),
                })],
                None,
            ),
        ];
        for (mode, conditions, expected) in cases {
            let mut rule = good_rule("r_1");
            rule.match_mode = mode;
            rule.conditions = conditions.clone();
            let found = codes(&validate(&[rule], &ctx));
            let expected: Vec<DiagnosticCode> = expected.into_iter().collect();
            assert_eq!(found, expected, "{mode:?} {conditions:?}");
        }
    }

    #[test]
    fn action_lints() {
        let ctx = ValidationContext::with_mailboxes(["M7d1e", "Mb"]);
        let receipts = MailboxRef::new("M7d1e", ["Receipts"]);
        let other = MailboxRef::new("Mb", ["Other"]);

        let mut rule = good_rule("r_1");
        rule.actions.clear();
        assert_eq!(
            codes(&validate(&[rule.clone()], &ctx)),
            [DiagnosticCode::NoActions]
        );
        rule.stop = true;
        assert_eq!(
            codes(&validate(&[rule.clone()], &ctx)),
            [DiagnosticCode::StopSkipsFastmail]
        );

        let mut rule = good_rule("r_1");
        rule.actions = vec![
            Action::MoveTo {
                mailbox: receipts.clone(),
            },
            Action::MoveTo {
                mailbox: other.clone(),
            },
            Action::MoveToTrash,
            Action::AddLabel {
                mailbox: receipts.clone(),
            },
            Action::MarkRead,
            Action::MarkRead,
        ];
        let found = codes(&validate(&[rule], &ctx));
        assert_eq!(
            found,
            [
                DiagnosticCode::DuplicateAction,
                DiagnosticCode::ConflictingActions,
                DiagnosticCode::ConflictingActions,
                DiagnosticCode::ConflictingActions,
                DiagnosticCode::DuplicateAction,
            ]
        );

        // Trash with flags is fine: Fastmail's own disabled-mask code does it.
        let mut rule = good_rule("r_1");
        rule.actions = vec![Action::MarkRead, Action::MoveToTrash];
        assert!(validate(&[rule], &ctx).is_empty());
    }

    #[test]
    fn mailbox_lints() {
        let mut rule = good_rule("r_1");
        rule.actions = vec![
            Action::MoveTo {
                mailbox: MailboxRef::new("Mgone", ["v1.2 notes"]),
            },
            Action::AddLabel {
                mailbox: MailboxRef::new("Mgone", ["v1.2 notes"]),
            },
        ];
        let ctx = ValidationContext::with_mailboxes(["M7d1e"]);
        let found = codes(&validate(std::slice::from_ref(&rule), &ctx));
        assert_eq!(
            found,
            [
                DiagnosticCode::DuplicateAction,
                DiagnosticCode::UnknownMailbox,
                DiagnosticCode::AmbiguousFallbackName,
            ]
        );
        let found = codes(&validate(&[rule], &ValidationContext::default()));
        assert_eq!(
            found,
            [
                DiagnosticCode::DuplicateAction,
                DiagnosticCode::AmbiguousFallbackName
            ]
        );
    }

    #[test]
    fn display_is_readable() {
        let d = Diagnostic {
            rule_id: Some(RuleId::new("r_1")),
            severity: Severity::Warning,
            code: DiagnosticCode::NoActions,
            message: "Rule has no actions".into(),
        };
        assert_eq!(d.to_string(), "warning: rule r_1: Rule has no actions");
    }
}
