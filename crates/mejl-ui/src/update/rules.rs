//! The rules screen in `update`.

use mejl_sieve::{
    MailboxRef, MessageFacts, PasteStatus, PlannedAction, Rule, SuggestionKind, check_paste,
    render_block, rule_from_message,
};

use crate::action::Action;
use crate::data::{MailData, MailboxId, MailboxRole};
use crate::event::{Cmd, Keyword, Mutation, Target, TargetIds, ToastLevel};
use crate::input::LineInput;
use crate::keymap::Key;
use crate::rules::{
    DRY_RUN_LIMIT, DRY_RUN_SAMPLE, DryRun, DryRunHit, FieldEdit, FormRow, RuleEntry, RulesPane,
    RulesScreen, action_phrase, condition_phrase, form_rows, other_mode, parse_action,
    parse_condition, to_json,
};
use crate::state::{AppState, Confirm, ConfirmAction, Overlay, Prompt, PromptAction, Screen};

/// Renders kept for the paste check and the "last copied" line.
const RENDER_HISTORY: usize = 50;

impl AppState {
    /// Opens the rules screen, at `id` if given.
    pub(crate) fn open_rules(&mut self, data: &dyn MailData, id: Option<&str>) {
        let mut cmds = Vec::new();
        self.leave_screen(&mut cmds);
        let rules = data.rules();
        let renders = data.rule_renders(RENDER_HISTORY);
        let mut screen = match self.rules.take() {
            Some(mut s) => {
                s.reload(&rules, renders);
                s
            }
            None => Box::new(RulesScreen::new(&rules, renders)),
        };
        if let Some(id) = id {
            screen.focus(id);
        }
        self.rules = Some(screen);
        self.screen = Screen::Rules;
        self.pending.clear();
        self.toast = None;
    }

    /// The rules screen has no text inputs of its own; prompts take text.
    pub(crate) fn rules_key(&mut self, data: &dyn MailData, key: Key, cmds: &mut Vec<Cmd>) -> bool {
        let _ = (data, key, cmds);
        false
    }

    fn new_rule_id(&mut self) -> String {
        self.draft_counter += 1;
        format!(
            "r_{:x}{}",
            self.clock.now().as_millisecond().unsigned_abs(),
            self.draft_counter
        )
    }

    /// Saves one rule and shows it in the list at once.
    fn save_rule(&mut self, rule: Rule, cmds: &mut Vec<Cmd>) {
        cmds.push(Cmd::SaveRule(to_json(&rule)));
        if let Some(s) = self.rules.as_mut() {
            let id = rule.id.as_str().to_owned();
            match s.rules.iter_mut().find(|r| r.id() == id) {
                Some(e) => *e = RuleEntry::Rule(rule),
                None => s.rules.push(RuleEntry::Rule(rule)),
            }
            s.focus(&id);
            s.dry_run = None;
        }
    }

    fn adopt_new_rule(&mut self, rule: Rule, cmds: &mut Vec<Cmd>) {
        self.save_rule(rule, cmds);
        if let Some(s) = self.rules.as_mut() {
            s.pane = RulesPane::Form;
            // The cursor goes to "add action", the next thing to fill in.
            let rows = s.selected_rule().map(form_rows).unwrap_or_default();
            s.form_cursor = rows
                .iter()
                .position(|r| *r == FormRow::AddAction)
                .unwrap_or(0);
        }
        self.notify(
            "Rule saved locally. Add an action, then P copies the block for Fastmail",
            ToastLevel::Success,
            false,
            cmds,
        );
    }

    /// Facts for the focused or cursor message, for "new rule from this
    /// message".
    fn message_facts(&self, data: &dyn MailData) -> Option<MessageFacts> {
        let conv = self.reader.conversation.clone().or_else(|| {
            let row = self.list.pages.row(self.list.cursor)?;
            data.conversation(&row.thread_id)
        })?;
        let m = conv
            .messages
            .get(self.reader.focused)
            .or(conv.messages.last())?
            .clone();
        let addr = |p: &crate::data::Participant| mejl_sieve::EmailAddress {
            name: p.name.clone(),
            email: p.email.clone(),
        };
        let headers = data
            .message_body(&m.email_id)
            .map(|b| b.headers)
            .unwrap_or_default();
        let delivered_to =
            m.to.iter()
                .chain(&m.cc)
                .filter(|p| data.masked_for_address(&p.email).is_some())
                .map(|p| p.email.clone())
                .collect();
        Some(MessageFacts {
            from: vec![addr(&m.from)],
            to: m.to.iter().map(addr).collect(),
            cc: m.cc.iter().map(addr).collect(),
            subject: Some(conv.subject.clone()),
            headers,
            size: 0,
            delivered_to,
        })
    }

    /// `F`: a new rule from the focused message, by list, masked address
    /// or sender domain, whichever the message has.
    pub(crate) fn new_rule_from_message(&mut self, data: &dyn MailData, cmds: &mut Vec<Cmd>) {
        let Some(facts) = self.message_facts(data) else {
            self.notify("Open a message first", ToastLevel::Info, false, cmds);
            return;
        };
        let id = self.new_rule_id();
        let rule = [
            SuggestionKind::ListId,
            SuggestionKind::MaskedAddress,
            SuggestionKind::SenderDomain,
            SuggestionKind::SenderAddress,
        ]
        .into_iter()
        .find_map(|k| rule_from_message(id.as_str(), &facts, k));
        let Some(rule) = rule else {
            self.notify(
                "This message has nothing a rule can match on",
                ToastLevel::Warning,
                false,
                cmds,
            );
            return;
        };
        self.open_rules(data, None);
        self.adopt_new_rule(rule, cmds);
    }

    /// A new rule for mail sent to an address (from Masked Email).
    pub(crate) fn new_rule_for_address(
        &mut self,
        data: &dyn MailData,
        email: &str,
        cmds: &mut Vec<Cmd>,
    ) {
        let facts = MessageFacts {
            delivered_to: vec![email.to_owned()],
            ..MessageFacts::default()
        };
        let id = self.new_rule_id();
        if let Some(rule) = rule_from_message(id.as_str(), &facts, SuggestionKind::MaskedAddress) {
            self.open_rules(data, None);
            self.adopt_new_rule(rule, cmds);
        }
    }

    /// Deletes a rule after the confirm.
    pub(crate) fn delete_rule(&mut self, data: &dyn MailData, id: &str, cmds: &mut Vec<Cmd>) {
        let _ = data;
        cmds.push(Cmd::DeleteRule(id.to_owned()));
        if let Some(s) = self.rules.as_mut() {
            s.rules.retain(|r| r.id() != id);
            s.cursor = s.cursor.min(s.rules.len().saturating_sub(1));
            s.pane = RulesPane::List;
            s.dry_run = None;
        }
        self.notify("Rule deleted locally", ToastLevel::Success, false, cmds);
    }

    fn find_mailbox(&self, name: &str) -> Option<MailboxRef> {
        let want = name.trim().trim_matches('/').to_lowercase();
        let path_of = |id: &MailboxId| -> Vec<String> {
            let mut parts = Vec::new();
            let mut cur = Some(id.clone());
            let mut guard = 0;
            while let Some(c) = cur {
                guard += 1;
                let Some(m) = self.mailboxes.iter().find(|m| m.id == c) else {
                    break;
                };
                if m.role != Some(MailboxRole::Inbox) {
                    parts.push(m.name.clone());
                }
                cur = m.parent.clone();
                if guard > 16 {
                    break;
                }
            }
            parts.reverse();
            parts
        };
        let found = self
            .mailboxes
            .iter()
            .find(|m| path_of(&m.id).join("/").to_lowercase() == want)
            .or_else(|| {
                self.mailboxes
                    .iter()
                    .find(|m| m.name.to_lowercase() == want)
            })?;
        Some(MailboxRef::new(found.id.as_str(), path_of(&found.id)))
    }

    /// The answer to a rule field prompt.
    pub(crate) fn rule_field_answer(
        &mut self,
        data: &dyn MailData,
        edit: FieldEdit,
        answer: String,
        cmds: &mut Vec<Cmd>,
    ) {
        let _ = data;
        let id = match &edit {
            FieldEdit::Name { id }
            | FieldEdit::Condition { id, .. }
            | FieldEdit::Action { id, .. } => id.clone(),
        };
        let Some(mut rule) = self
            .rules
            .as_ref()
            .and_then(|s| s.rules.iter().find(|r| r.id() == id))
            .and_then(RuleEntry::rule)
            .cloned()
        else {
            return;
        };
        let result = match edit {
            FieldEdit::Name { .. } => {
                if answer.is_empty() {
                    Err("a rule needs a name".to_owned())
                } else {
                    rule.name = answer;
                    Ok(())
                }
            }
            FieldEdit::Condition { index, .. } => parse_condition(&answer).map(|c| match index {
                Some(i) if i < rule.conditions.len() => rule.conditions[i] = c,
                _ => rule.conditions.push(c),
            }),
            FieldEdit::Action { index, .. } => {
                let find = |n: &str| self.find_mailbox(n);
                parse_action(&answer, &find).map(|a| match index {
                    Some(i) if i < rule.actions.len() => rule.actions[i] = a,
                    _ => rule.actions.push(a),
                })
            }
        };
        match result {
            Ok(()) => {
                self.save_rule(rule, cmds);
                self.notify("Rule saved locally", ToastLevel::Success, false, cmds);
            }
            Err(e) => self.notify(format!("Not saved: {e}"), ToastLevel::Error, false, cmds),
        }
    }

    fn field_prompt(&mut self, label: String, text: String, edit: FieldEdit) {
        self.overlays.push(Overlay::Prompt(Prompt {
            label,
            input: LineInput::new(text),
            on_enter: PromptAction::RuleField(edit),
        }));
    }

    /// Enter on a form row.
    fn edit_form_row(&mut self, cmds: &mut Vec<Cmd>) {
        let Some(s) = self.rules.as_ref() else {
            return;
        };
        let (Some(row), Some(rule)) = (s.form_row(), s.selected_rule().cloned()) else {
            return;
        };
        let id = rule.id.as_str().to_owned();
        match row {
            FormRow::Name => {
                self.field_prompt("Rule name:".into(), rule.name, FieldEdit::Name { id })
            }
            FormRow::Condition(i) => self.field_prompt(
                "Condition:".into(),
                rule.conditions
                    .get(i)
                    .map(condition_phrase)
                    .unwrap_or_default(),
                FieldEdit::Condition { id, index: Some(i) },
            ),
            FormRow::AddCondition => self.field_prompt(
                "New condition (such as subject contains receipt):".into(),
                String::new(),
                FieldEdit::Condition { id, index: None },
            ),
            FormRow::Action(i) => self.field_prompt(
                "Action:".into(),
                rule.actions.get(i).map(action_phrase).unwrap_or_default(),
                FieldEdit::Action { id, index: Some(i) },
            ),
            FormRow::AddAction => self.field_prompt(
                "New action (move to <mailbox>, label <mailbox>, mark read, pin, trash):".into(),
                String::new(),
                FieldEdit::Action { id, index: None },
            ),
            FormRow::Enabled | FormRow::Match | FormRow::Stop => self.toggle_form_row(cmds),
        }
    }

    fn toggle_form_row(&mut self, cmds: &mut Vec<Cmd>) {
        let Some(s) = self.rules.as_ref() else {
            return;
        };
        let (Some(row), Some(mut rule)) = (s.form_row(), s.selected_rule().cloned()) else {
            return;
        };
        match row {
            FormRow::Enabled => rule.enabled = !rule.enabled,
            FormRow::Match => rule.match_mode = other_mode(rule.match_mode),
            FormRow::Stop => rule.stop = !rule.stop,
            FormRow::Condition(i) => {
                if let Some(c) = rule.conditions.get_mut(i) {
                    c.negate = !c.negate;
                }
            }
            _ => return,
        }
        self.save_rule(rule, cmds);
    }

    fn delete_form_row(&mut self, cmds: &mut Vec<Cmd>) {
        let Some(s) = self.rules.as_ref() else {
            return;
        };
        let (Some(row), Some(mut rule)) = (s.form_row(), s.selected_rule().cloned()) else {
            return;
        };
        match row {
            FormRow::Condition(i) if i < rule.conditions.len() => {
                rule.conditions.remove(i);
            }
            FormRow::Action(i) if i < rule.actions.len() => {
                rule.actions.remove(i);
            }
            _ => return,
        }
        self.save_rule(rule, cmds);
    }

    fn copy_block(&mut self, cmds: &mut Vec<Cmd>) {
        let Some(s) = self.rules.as_ref() else {
            return;
        };
        match render_block(&s.parsed()) {
            Ok(block) => {
                let lines = block.line_count();
                cmds.push(Cmd::CopyToClipboard(block.text.clone()));
                cmds.push(Cmd::RecordRuleRender {
                    sieve: block.text,
                    body_sha256: block.body_sha256,
                });
                self.notify(
                    format!(
                        "Copied the mejl block ({lines} lines). In Fastmail: Settings > Filters & \
                         Rules > Edit custom Sieve code, below spam protection"
                    ),
                    ToastLevel::Success,
                    false,
                    cmds,
                );
            }
            Err(e) => self.notify(
                format!(
                    "Not copied: rule {} cannot be rendered ({:?})",
                    e.rule_id, e.kind
                ),
                ToastLevel::Error,
                false,
                cmds,
            ),
        }
    }

    /// The pasted script for the paste check.
    pub(crate) fn rule_paste_answer(
        &mut self,
        data: &dyn MailData,
        text: &str,
        cmds: &mut Vec<Cmd>,
    ) {
        let _ = data;
        let Some(s) = self.rules.as_mut() else {
            return;
        };
        let current = match render_block(&s.parsed()) {
            Ok(b) => b.body_sha256,
            Err(_) => String::new(),
        };
        let previous: Vec<String> = s.renders.iter().map(|r| r.body_sha256.clone()).collect();
        let report = check_paste(text, &current, &previous);
        let version = |index: usize| s.renders.len().saturating_sub(index);
        let (ok, words) = match report.status {
            PasteStatus::Intact => (true, "Fastmail has the current mejl block".to_owned()),
            PasteStatus::OlderVersion { index } => (
                false,
                format!(
                    "Fastmail has an older block (v{}); press P and paste the new one",
                    version(index)
                ),
            ),
            PasteStatus::EditedByHand => (
                false,
                "the block on Fastmail was edited by hand; press P and replace it".to_owned(),
            ),
            PasteStatus::Missing => (
                false,
                "no mejl block in what you pasted; paste the whole custom section".to_owned(),
            ),
            PasteStatus::UnknownVersion => (
                false,
                "the block on Fastmail is not one this mejl copied; press P to copy it again"
                    .to_owned(),
            ),
            PasteStatus::Duplicate { count } => (
                false,
                format!("{count} mejl blocks found; keep exactly one"),
            ),
            PasteStatus::Incomplete => (
                false,
                "the block is cut short: a begin or end marker is missing".to_owned(),
            ),
            PasteStatus::UnsupportedFormat { version } => (
                false,
                format!("the block uses format v{version}, which this mejl cannot read"),
            ),
        };
        if ok && let Some(r) = s.renders.iter().find(|r| r.body_sha256 == current) {
            cmds.push(Cmd::ConfirmRulePaste { render_id: r.id });
        }
        s.paste = Some((ok, words.clone()));
        let level = if ok {
            ToastLevel::Success
        } else {
            ToastLevel::Warning
        };
        let mut first = words.chars();
        let words: String = first
            .next()
            .map(|c| c.to_uppercase().chain(first).collect())
            .unwrap_or_default();
        self.notify(words, level, false, cmds);
    }

    fn dry_run(&mut self, data: &dyn MailData, cmds: &mut Vec<Cmd>) {
        let Some(mut rule) = self.rules.as_ref().and_then(|s| s.selected_rule().cloned()) else {
            return;
        };
        // A dry run tries the rule even while it is off.
        rule.enabled = true;
        let messages = data.dry_run_messages(DRY_RUN_LIMIT);
        let mut hits = Vec::new();
        let mut plans = Vec::new();
        let mut already = 0;
        for m in &messages {
            let Some(actions) = mejl_sieve::evaluate(&rule, &m.facts) else {
                continue;
            };
            let target = actions
                .iter()
                .find_map(|a| match a {
                    PlannedAction::MoveTo(mb) => Some(mb.display_path()),
                    PlannedAction::MoveToTrash => Some("Trash".to_owned()),
                    _ => None,
                })
                .unwrap_or_else(|| "stays".to_owned());
            let in_place = actions.iter().any(|a| match a {
                PlannedAction::MoveTo(mb) => m.mailboxes.iter().any(|x| x.as_str() == mb.id),
                _ => false,
            });
            if in_place {
                already += 1;
            }
            if hits.len() < DRY_RUN_SAMPLE {
                hits.push(DryRunHit {
                    email_id: m.email_id.clone(),
                    from: m
                        .facts
                        .from
                        .first()
                        .map(|a| a.name.clone().unwrap_or_else(|| a.email.clone()))
                        .unwrap_or_default(),
                    subject: m.facts.subject.clone().unwrap_or_default(),
                    date: m.date,
                    target,
                });
            }
            plans.push((m.email_id.clone(), actions));
        }
        let n = plans.len();
        if let Some(s) = self.rules.as_mut() {
            s.dry_run = Some(DryRun {
                rule_id: rule.id.as_str().to_owned(),
                scanned: messages.len(),
                hits,
                already,
                plans,
            });
        }
        self.notify(
            format!(
                "{} would match out of {} cached messages",
                n,
                messages.len()
            ),
            ToastLevel::Info,
            false,
            cmds,
        );
    }

    fn apply_rule(&mut self, cmds: &mut Vec<Cmd>) {
        let Some(s) = self.rules.as_ref() else {
            return;
        };
        let Some(run) = s
            .dry_run
            .as_ref()
            .filter(|r| Some(r.rule_id.as_str()) == s.selected().map(RuleEntry::id))
        else {
            self.notify(
                "Run the dry run first (t), to see what would change",
                ToastLevel::Info,
                false,
                cmds,
            );
            return;
        };
        let inbox = self.mailbox_id(MailboxRole::Inbox);
        let mut count = 0;
        for (id, actions) in &run.plans {
            let target = || Target {
                ids: TargetIds::Emails(vec![id.clone()]),
                from_mailbox: inbox.clone(),
            };
            for a in actions {
                let m = match a {
                    PlannedAction::MarkRead => Some(Mutation::SetKeyword {
                        target: target(),
                        keyword: Keyword::Seen,
                        value: true,
                    }),
                    PlannedAction::Pin => Some(Mutation::SetKeyword {
                        target: target(),
                        keyword: Keyword::Flagged,
                        value: true,
                    }),
                    PlannedAction::AddLabel(mb) => Some(Mutation::SetMailbox {
                        target: target(),
                        mailbox: MailboxId::from(mb.id.as_str()),
                        member: true,
                    }),
                    PlannedAction::MoveTo(mb) => Some(Mutation::MoveTo {
                        target: target(),
                        mailbox: MailboxId::from(mb.id.as_str()),
                    }),
                    PlannedAction::MoveToTrash => Some(Mutation::Trash(target())),
                    PlannedAction::Stop => None,
                };
                if let Some(m) = m {
                    cmds.push(Cmd::Mutate(m));
                }
            }
            count += 1;
        }
        self.notify(
            format!("Applied the rule to {count} messages"),
            ToastLevel::Success,
            true,
            cmds,
        );
    }

    /// Runs a rules screen action. Returns false for other actions.
    pub(crate) fn rules_action(
        &mut self,
        data: &dyn MailData,
        action: Action,
        cmds: &mut Vec<Cmd>,
    ) -> bool {
        let Some(s) = self.rules.as_mut() else {
            return false;
        };
        let form = s.pane == RulesPane::Form;
        match action {
            Action::Next | Action::Prev => {
                let down = action == Action::Next;
                if form {
                    let rows = s.selected_rule().map(form_rows).unwrap_or_default().len();
                    s.form_cursor = if down {
                        (s.form_cursor + 1).min(rows.saturating_sub(1))
                    } else {
                        s.form_cursor.saturating_sub(1)
                    };
                } else if !s.rules.is_empty() {
                    s.cursor = if down {
                        (s.cursor + 1).min(s.rules.len() - 1)
                    } else {
                        s.cursor.saturating_sub(1)
                    };
                    s.form_cursor = 0;
                }
            }
            Action::GoTop if form => s.form_cursor = 0,
            Action::GoBottom if form => {
                s.form_cursor = s
                    .selected_rule()
                    .map(form_rows)
                    .unwrap_or_default()
                    .len()
                    .saturating_sub(1);
            }
            Action::GoTop => s.cursor = 0,
            Action::GoBottom => s.cursor = s.rules.len().saturating_sub(1),
            Action::NextPane | Action::PrevPane => {
                s.pane = if form || s.selected_rule().is_none() {
                    RulesPane::List
                } else {
                    RulesPane::Form
                };
            }
            Action::Open => {
                if form {
                    self.edit_form_row(cmds);
                } else if s.selected_rule().is_some() {
                    s.pane = RulesPane::Form;
                    s.form_cursor = 0;
                }
            }
            Action::RuleToggle => {
                if form {
                    self.toggle_form_row(cmds);
                } else if let Some(mut rule) = s.selected_rule().cloned() {
                    rule.enabled = !rule.enabled;
                    let text = if rule.enabled { "Rule on" } else { "Rule off" };
                    self.save_rule(rule, cmds);
                    self.notify(
                        format!("{text}. Copy the block again with P"),
                        ToastLevel::Success,
                        false,
                        cmds,
                    );
                }
            }
            Action::RuleUp | Action::RuleDown => {
                let i = s.cursor;
                let j = if action == Action::RuleUp {
                    i.checked_sub(1)
                } else {
                    (i + 1 < s.rules.len()).then_some(i + 1)
                };
                if let Some(j) = j {
                    s.rules.swap(i, j);
                    s.cursor = j;
                    cmds.push(Cmd::ReorderRules(
                        s.rules.iter().map(|r| r.id().to_owned()).collect(),
                    ));
                }
            }
            Action::RuleDelete => {
                if form {
                    self.delete_form_row(cmds);
                } else if let Some(e) = s.selected() {
                    let name = e
                        .rule()
                        .map(|r| r.name.clone())
                        .unwrap_or_else(|| e.id().to_owned());
                    let id = e.id().to_owned();
                    self.overlays.push(Overlay::Confirm(Confirm {
                        prompt: format!("Delete the rule \"{name}\"?"),
                        on_yes: ConfirmAction::DeleteRule { id },
                    }));
                }
            }
            Action::RuleNew => {
                let id = self.new_rule_id();
                let rule = Rule::new(id.as_str(), "New rule");
                self.adopt_new_rule(rule, cmds);
                if let Some(s) = self.rules.as_mut() {
                    s.form_cursor = 0;
                }
                self.edit_form_row(cmds);
            }
            Action::RuleDryRun => self.dry_run(data, cmds),
            Action::RuleApply => self.apply_rule(cmds),
            Action::RuleCopyBlock => self.copy_block(cmds),
            Action::RulePasteCheck => self.overlays.push(Overlay::Prompt(Prompt {
                label: "Paste your Fastmail Sieve script now (Esc cancels):".into(),
                input: LineInput::default(),
                on_enter: PromptAction::RulePaste,
            })),
            Action::Back => {
                if form {
                    s.pane = RulesPane::List;
                } else {
                    self.screen = Screen::Mail;
                }
            }
            _ => return false,
        }
        true
    }
}
