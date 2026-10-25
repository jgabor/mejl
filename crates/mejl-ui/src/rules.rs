//! The rules screen: mejl's rules, an edit form, the rendered Sieve block,
//! a dry run against cached mail and the paste check.
//!
//! Fastmail has no public API for rules or Sieve, so mejl keeps its own
//! rules (as `mejl_sieve::Rule` JSON through [`MailData::rules`]) and
//! renders them into one marked block the user pastes into Fastmail. `P`
//! copies the block and records the copy ([`Cmd::RecordRuleRender`]); `v`
//! takes the user's pasted script back and reports with
//! `mejl_sieve::check_paste` whether Fastmail has the current block.
//!
//! Conditions and actions are edited as short phrases in a prompt, which
//! [`parse_condition`] and [`parse_action`] turn into the rule model:
//! `from-domain is shop.example.com`, `not subject contains sale`,
//! `move to Receipts`, `mark read`.
//!
//! [`MailData::rules`]: crate::MailData::rules
//! [`Cmd::RecordRuleRender`]: crate::Cmd::RecordRuleRender

use mejl_sieve::{
    Action as RuleAction, AddressHeader, AddressPart, Condition, MailboxRef, MatchMode, Rule, Test,
    TextOp,
};

use crate::data::{EmailId, RuleJson, RuleRenderView};

/// Cached messages the dry run reads at most.
pub const DRY_RUN_LIMIT: usize = 2_000;
/// Matches the dry run lists.
pub const DRY_RUN_SAMPLE: usize = 8;

/// The parts of Fastmail's generated Sieve script in the order they run,
/// with whether the user can edit them, and true for the part the mejl
/// block goes in. The order comes from third-party samples of the
/// generated script, not from Fastmail's docs, so the screen calls it
/// unconfirmed and names the target by position, never by number.
pub const BLOCK_ORDER: [(&str, &str, bool); 8] = [
    ("require line", "generated", false),
    ("your code (top)", "custom", false),
    ("blocked senders", "generated", false),
    ("masked email", "generated", false),
    ("spam, address rules", "generated", false),
    ("mejl block", "custom", true),
    ("Fastmail UI rules", "generated", false),
    ("your code (bottom)", "custom", false),
];

/// A rule as stored: parsed, or kept as an error row when its JSON does
/// not parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleEntry {
    /// A rule.
    Rule(Rule),
    /// A rule whose JSON could not be read.
    Broken {
        /// The stored id.
        id: String,
        /// Why it could not be read.
        error: String,
    },
}

impl RuleEntry {
    /// Parses a stored rule.
    pub fn parse(r: &RuleJson) -> RuleEntry {
        match serde_json::from_str::<Rule>(&r.json) {
            Ok(rule) => RuleEntry::Rule(rule),
            Err(e) => RuleEntry::Broken {
                id: r.id.clone(),
                error: e.to_string(),
            },
        }
    }

    /// The rule id.
    pub fn id(&self) -> &str {
        match self {
            RuleEntry::Rule(r) => r.id.as_str(),
            RuleEntry::Broken { id, .. } => id,
        }
    }

    /// The parsed rule, if it parsed.
    pub fn rule(&self) -> Option<&Rule> {
        match self {
            RuleEntry::Rule(r) => Some(r),
            RuleEntry::Broken { .. } => None,
        }
    }
}

/// Which part of the rules screen has the keys.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RulesPane {
    /// The list of rules.
    #[default]
    List,
    /// The edit form of the rule under the cursor.
    Form,
}

/// A row of the edit form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormRow {
    /// The name.
    Name,
    /// On or off.
    Enabled,
    /// Any or all conditions.
    Match,
    /// A condition by index.
    Condition(usize),
    /// Add a condition.
    AddCondition,
    /// An action by index.
    Action(usize),
    /// Add an action.
    AddAction,
    /// Stop after this rule.
    Stop,
}

/// The rows of the form for a rule.
pub fn form_rows(rule: &Rule) -> Vec<FormRow> {
    let mut v = vec![FormRow::Name, FormRow::Enabled, FormRow::Match];
    v.extend((0..rule.conditions.len()).map(FormRow::Condition));
    v.push(FormRow::AddCondition);
    v.extend((0..rule.actions.len()).map(FormRow::Action));
    v.push(FormRow::AddAction);
    v.push(FormRow::Stop);
    v
}

/// A rule field being edited through a prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldEdit {
    /// The rule's name.
    Name {
        /// Rule id.
        id: String,
    },
    /// A condition; `None` adds one.
    Condition {
        /// Rule id.
        id: String,
        /// Which condition.
        index: Option<usize>,
    },
    /// An action; `None` adds one.
    Action {
        /// Rule id.
        id: String,
        /// Which action.
        index: Option<usize>,
    },
}

/// One message the dry run matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DryRunHit {
    /// The message.
    pub email_id: EmailId,
    /// Sender name or address.
    pub from: String,
    /// Subject.
    pub subject: String,
    /// Received.
    pub date: jiff::Timestamp,
    /// Where the rule would file it, for the sample.
    pub target: String,
}

/// A dry run of one rule against cached mail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DryRun {
    /// The rule tried.
    pub rule_id: String,
    /// Messages read.
    pub scanned: usize,
    /// Matching messages, newest first.
    pub hits: Vec<DryRunHit>,
    /// Matches already where the rule would move them.
    pub already: usize,
    /// Planned changes per match, for "apply to existing".
    pub plans: Vec<(EmailId, Vec<mejl_sieve::PlannedAction>)>,
}

/// The rules screen.
#[derive(Clone, Debug, Default)]
pub struct RulesScreen {
    pub(crate) rules: Vec<RuleEntry>,
    pub(crate) cursor: usize,
    pub(crate) pane: RulesPane,
    pub(crate) form_cursor: usize,
    pub(crate) renders: Vec<RuleRenderView>,
    pub(crate) dry_run: Option<DryRun>,
    pub(crate) paste: Option<(bool, String)>,
}

impl RulesScreen {
    pub(crate) fn new(rules: &[RuleJson], renders: Vec<RuleRenderView>) -> RulesScreen {
        RulesScreen {
            rules: rules.iter().map(RuleEntry::parse).collect(),
            renders,
            ..RulesScreen::default()
        }
    }

    /// Replaces the rules, keeping the cursor on the same rule id.
    pub(crate) fn reload(&mut self, rules: &[RuleJson], renders: Vec<RuleRenderView>) {
        let keep = self.selected().map(|r| r.id().to_owned());
        self.rules = rules.iter().map(RuleEntry::parse).collect();
        self.renders = renders;
        if let Some(id) = keep {
            self.focus(&id);
        }
        self.cursor = self.cursor.min(self.rules.len().saturating_sub(1));
    }

    pub(crate) fn focus(&mut self, id: &str) {
        if let Some(i) = self.rules.iter().position(|r| r.id() == id) {
            self.cursor = i;
        }
    }

    pub(crate) fn selected(&self) -> Option<&RuleEntry> {
        self.rules.get(self.cursor)
    }

    pub(crate) fn selected_rule(&self) -> Option<&Rule> {
        self.selected().and_then(RuleEntry::rule)
    }

    /// The parsed rules, in order, for rendering.
    pub(crate) fn parsed(&self) -> Vec<Rule> {
        self.rules
            .iter()
            .filter_map(|r| r.rule().cloned())
            .collect()
    }

    /// The form row under the form cursor.
    pub(crate) fn form_row(&self) -> Option<FormRow> {
        let rule = self.selected_rule()?;
        form_rows(rule).get(self.form_cursor).copied()
    }
}

/// The rule as stored JSON.
pub fn to_json(rule: &Rule) -> RuleJson {
    RuleJson {
        id: rule.id.as_str().to_owned(),
        json: serde_json::to_string(rule).unwrap_or_default(),
    }
}

/// A one-condition rule's summary is the condition's text.
pub fn condition_text(c: &Condition) -> String {
    let mut r = Rule::new("x", "x");
    r.conditions.push(c.clone());
    mejl_sieve::summary::summarize_conditions(&r)
}

/// The text of one action.
pub fn action_text(a: &RuleAction) -> String {
    let mut r = Rule::new("x", "x");
    r.actions.push(a.clone());
    mejl_sieve::summary::summarize_actions(&r)
}

fn op_word(op: TextOp) -> &'static str {
    match op {
        TextOp::Is => "is",
        TextOp::Contains => "contains",
        TextOp::Matches => "matches",
        TextOp::StartsWith => "starts-with",
        TextOp::EndsWith => "ends-with",
    }
}

/// A condition in the prompt's phrase syntax, for editing it.
pub fn condition_phrase(c: &Condition) -> String {
    let not = if c.negate { "not " } else { "" };
    let body = match &c.test {
        Test::Address {
            header,
            part,
            op,
            value,
        } => {
            let h = match header {
                AddressHeader::From => "from",
                AddressHeader::To => "to",
                AddressHeader::Cc => "cc",
                AddressHeader::ToOrCc => "to-or-cc",
            };
            let p = match part {
                AddressPart::All => "",
                AddressPart::Localpart => "-local",
                AddressPart::Domain => "-domain",
            };
            format!("{h}{p} {} {value}", op_word(*op))
        }
        Test::Subject { op, value } => format!("subject {} {value}", op_word(*op)),
        Test::ListId { op, value } => format!("list-id {} {value}", op_word(*op)),
        Test::Header { name, op, value } => format!("header:{name} {} {value}", op_word(*op)),
        Test::DeliveredTo { op, value } => format!("delivered-to {} {value}", op_word(*op)),
        Test::SizeOver { bytes } => format!("size-over {bytes}"),
        Test::SizeUnder { bytes } => format!("size-under {bytes}"),
    };
    format!("{not}{body}")
}

fn parse_size(s: &str) -> Option<u64> {
    let t = s.trim().to_lowercase();
    let (num, mult) = match t.chars().last()? {
        'k' => (&t[..t.len() - 1], 1024u64),
        'm' => (&t[..t.len() - 1], 1024 * 1024),
        'g' => (&t[..t.len() - 1], 1024 * 1024 * 1024),
        _ => (t.as_str(), 1),
    };
    num.trim().parse::<u64>().ok()?.checked_mul(mult)
}

/// Parses a condition phrase: `[not] <field> <op> <value>`, where field is
/// `from`, `from-domain`, `from-local`, `to`, `to-domain`, `cc`,
/// `to-or-cc`, `subject`, `list-id`, `delivered-to` or `header:<Name>`,
/// and op is `is`, `contains`, `matches`, `starts-with` or `ends-with`.
/// Sizes are `size-over 2m` and `size-under 100k`.
pub fn parse_condition(text: &str) -> Result<Condition, String> {
    let mut t = text.trim();
    let mut negate = false;
    if let Some(rest) = t.strip_prefix("not ") {
        negate = true;
        t = rest.trim_start();
    }
    let (field, rest) = t.split_once(' ').ok_or_else(|| {
        "write a field, an operator and a value, such as subject contains receipt".to_owned()
    })?;
    let raw_field = field;
    let field = field.to_lowercase();
    if field == "size-over" || field == "size-under" {
        let bytes = parse_size(rest).ok_or("a size is a number with an optional k, m or g")?;
        let test = if field == "size-over" {
            Test::SizeOver { bytes }
        } else {
            Test::SizeUnder { bytes }
        };
        return Ok(Condition { negate, test });
    }
    let (op, value) = rest
        .trim_start()
        .split_once(' ')
        .map(|(o, v)| (o.to_lowercase(), v.trim().to_owned()))
        .ok_or("an operator and a value must follow the field")?;
    let op = match op.as_str() {
        "is" => TextOp::Is,
        "contains" => TextOp::Contains,
        "matches" => TextOp::Matches,
        "starts-with" => TextOp::StartsWith,
        "ends-with" => TextOp::EndsWith,
        other => {
            return Err(format!(
                "\"{other}\" is not an operator; use is, contains, matches, starts-with or ends-with"
            ));
        }
    };
    if value.is_empty() {
        return Err("the value is empty".into());
    }
    let address = |header, part| Test::Address {
        header,
        part,
        op,
        value: value.clone(),
    };
    let test = match field.as_str() {
        "from" => address(AddressHeader::From, AddressPart::All),
        "from-domain" => address(AddressHeader::From, AddressPart::Domain),
        "from-local" => address(AddressHeader::From, AddressPart::Localpart),
        "to" => address(AddressHeader::To, AddressPart::All),
        "to-domain" => address(AddressHeader::To, AddressPart::Domain),
        "cc" => address(AddressHeader::Cc, AddressPart::All),
        "to-or-cc" => address(AddressHeader::ToOrCc, AddressPart::All),
        "subject" => Test::Subject { op, value },
        "list-id" => Test::ListId { op, value },
        "delivered-to" => Test::DeliveredTo { op, value },
        f => match raw_field.get(7..).filter(|_| f.starts_with("header:")) {
            Some(name) if mejl_sieve::render::is_valid_header_name(name) => Test::Header {
                name: name.to_owned(),
                op,
                value,
            },
            Some(_) => return Err("that is not a valid header name".into()),
            None => {
                return Err(format!(
                    "\"{f}\" is not a field; use from, from-domain, to, cc, subject, list-id, \
                     delivered-to, header:Name, size-over or size-under"
                ));
            }
        },
    };
    Ok(Condition { negate, test })
}

/// An action in the prompt's phrase syntax.
pub fn action_phrase(a: &RuleAction) -> String {
    match a {
        RuleAction::MoveTo { mailbox } => format!("move to {}", mailbox.display_path()),
        RuleAction::AddLabel { mailbox } => format!("label {}", mailbox.display_path()),
        RuleAction::MarkRead => "mark read".into(),
        RuleAction::Pin => "pin".into(),
        RuleAction::MoveToTrash => "trash".into(),
    }
}

/// Parses an action phrase: `move to <mailbox>`, `label <mailbox>`,
/// `mark read`, `pin` or `trash`. `find` resolves a mailbox name or path.
pub fn parse_action(
    text: &str,
    find: &dyn Fn(&str) -> Option<MailboxRef>,
) -> Result<RuleAction, String> {
    let t = text.trim();
    let lower = t.to_lowercase();
    let mailbox = |name: &str| {
        find(name.trim()).ok_or_else(|| format!("there is no mailbox called \"{}\"", name.trim()))
    };
    if let Some(rest) = lower.strip_prefix("move to ") {
        let name = &t[t.len() - rest.len()..];
        return Ok(RuleAction::MoveTo {
            mailbox: mailbox(name)?,
        });
    }
    if let Some(rest) = lower.strip_prefix("label ") {
        let name = &t[t.len() - rest.len()..];
        return Ok(RuleAction::AddLabel {
            mailbox: mailbox(name)?,
        });
    }
    match lower.as_str() {
        "mark read" | "read" => Ok(RuleAction::MarkRead),
        "pin" => Ok(RuleAction::Pin),
        "trash" | "move to trash" => Ok(RuleAction::MoveToTrash),
        _ => Err("use move to <mailbox>, label <mailbox>, mark read, pin or trash".into()),
    }
}

/// Toggles any and all.
pub fn other_mode(m: MatchMode) -> MatchMode {
    match m {
        MatchMode::Any => MatchMode::All,
        MatchMode::All => MatchMode::Any,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(name: &str) -> Option<MailboxRef> {
        (name.eq_ignore_ascii_case("receipts"))
            .then(|| MailboxRef::new("mb-receipts", ["Receipts"]))
    }

    #[test]
    fn condition_phrases_round_trip() {
        for p in [
            "from-domain is shop.example.com",
            "not subject contains sale",
            "header:X-Spam-score starts-with 5",
            "delivered-to is kettle.fern4821@example.org",
            "list-id contains <garden-club.example.org>",
            "to-or-cc ends-with example.org",
            "size-over 2097152",
        ] {
            let c = parse_condition(p).unwrap_or_else(|e| panic!("{p}: {e}"));
            assert_eq!(condition_phrase(&c), p);
        }
        let c = parse_condition("size-under 100k").unwrap();
        assert_eq!(c.test, Test::SizeUnder { bytes: 102_400 });
        assert!(!condition_text(&c).is_empty());
    }

    #[test]
    fn condition_errors_say_why() {
        assert!(
            parse_condition("subject")
                .unwrap_err()
                .contains("field, an operator")
        );
        assert!(
            parse_condition("subject has x")
                .unwrap_err()
                .contains("not an operator")
        );
        assert!(
            parse_condition("color is red")
                .unwrap_err()
                .contains("not a field")
        );
        assert!(parse_condition("header:bad name is x").is_err());
        assert!(parse_condition("size-over lots").is_err());
        assert!(parse_condition("subject is ").is_err());
    }

    #[test]
    fn actions_parse_and_resolve_mailboxes() {
        let a = parse_action("move to Receipts", &find).unwrap();
        assert_eq!(action_phrase(&a), "move to Receipts");
        assert!(matches!(
            parse_action("label receipts", &find),
            Ok(RuleAction::AddLabel { .. })
        ));
        assert_eq!(parse_action("Mark read", &find), Ok(RuleAction::MarkRead));
        assert_eq!(parse_action("pin", &find), Ok(RuleAction::Pin));
        assert_eq!(parse_action("trash", &find), Ok(RuleAction::MoveToTrash));
        assert!(
            parse_action("move to Nowhere", &find)
                .unwrap_err()
                .contains("no mailbox")
        );
        assert!(parse_action("explode", &find).is_err());
        assert!(!action_text(&a).is_empty());
    }

    #[test]
    fn broken_json_is_a_row_not_a_panic() {
        let e = RuleEntry::parse(&RuleJson {
            id: "r_bad".into(),
            json: "{\"id\":".into(),
        });
        assert_eq!(e.id(), "r_bad");
        assert!(e.rule().is_none());
        let rule = Rule::new("r_1", "One");
        let back = RuleEntry::parse(&to_json(&rule));
        assert_eq!(back.rule(), Some(&rule));
        assert_eq!(form_rows(&rule).len(), 6);
        assert_eq!(other_mode(MatchMode::Any), MatchMode::All);
    }
}
