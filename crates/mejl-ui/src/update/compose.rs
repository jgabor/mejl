//! Compose in `update`: opening, keys, sending with undo, drafts.

use jiff::SignedDuration;

use crate::action::Action;
use crate::address;
use crate::compose::{ComposeState, Field, KeyOutcome, SUGGESTIONS, Start};
use crate::data::{ComposeKind, DraftId, MailData, OutgoingMessage, Participant};
use crate::event::{Cmd, SendOutcome, SendReport, ToastLevel};
use crate::input::LineInput;
use crate::keymap::{Context, Key, KeyName, Pending, Resolution};
use crate::picker::{PickItem, PickValue, Picker, PickerKind};
use crate::state::{
    AppState, Confirm, ConfirmAction, Focus, Overlay, PendingSend, Prompt, PromptAction, Screen,
    Toast,
};

impl AppState {
    fn new_draft_id(&mut self) -> DraftId {
        self.draft_counter += 1;
        DraftId(format!(
            "d{}-{}",
            self.clock.now().as_millisecond(),
            self.draft_counter
        ))
    }

    /// Opens compose for a new message, a reply or a forward of the
    /// focused message.
    pub(crate) fn open_compose(
        &mut self,
        data: &dyn MailData,
        kind: ComposeKind,
        to: Vec<Participant>,
        cmds: &mut Vec<Cmd>,
    ) {
        if self.compose.is_some() {
            // One compose at a time: go back to it rather than lose it.
            self.screen = Screen::Compose;
            return;
        }
        let identities = data.identities();
        let account = data.account();
        let account = Participant {
            name: (!account.name.trim().is_empty()).then(|| account.name.clone()),
            email: account.email,
        };
        let original = if kind == ComposeKind::New {
            None
        } else {
            let conv = match (&self.reader.conversation, self.focus) {
                (Some(c), Focus::Reader) => Some(c.clone()),
                _ => self
                    .list
                    .pages
                    .row(self.list.cursor)
                    .and_then(|r| data.conversation(&r.thread_id)),
            };
            let Some(conv) = conv else {
                self.notify(
                    "Pick a conversation to answer first",
                    ToastLevel::Info,
                    false,
                    cmds,
                );
                return;
            };
            let index = if self.focus == Focus::Reader {
                self.reader.focused
            } else {
                usize::MAX
            };
            let msg = conv.messages.get(index).or(conv.messages.last()).cloned();
            msg.map(|m| (m, conv.subject.clone()))
        };
        let body = original
            .as_ref()
            .and_then(|(m, _)| data.message_body(&m.email_id));
        let masked_to = original.as_ref().and_then(|(m, _)| {
            m.to.iter()
                .chain(&m.cc)
                .find(|p| data.masked_for_address(&p.email).is_some())
                .cloned()
        });
        let mine_list: Vec<String> = self.identities.clone();
        let mine = |e: &str| mine_list.iter().any(|i| i.eq_ignore_ascii_case(e));
        let now = self.now_zoned();
        let draft_id = self.new_draft_id();
        let mut c = ComposeState::start(&Start {
            draft_id,
            kind,
            identities: &identities,
            account,
            original: original.as_ref().map(|(m, _)| m),
            subject: original.as_ref().map_or("", |(_, s)| s.as_str()),
            body: body.as_ref(),
            mine: &mine,
            masked_to,
            now: &now,
        });
        for p in to {
            c.to.chips.push(p);
        }
        if !c.to.chips.is_empty() && kind == ComposeKind::New {
            c.field = Field::Subject;
        }
        if let Some((id, _)) = &c.quote_wait {
            cmds.push(Cmd::FetchBody(id.clone()));
        }
        c.sync_cursor();
        self.compose = Some(Box::new(c));
        self.screen = Screen::Compose;
        self.pending.clear();
    }

    /// Opens a saved draft or a message that came back from a send.
    pub(crate) fn resume_compose(&mut self, message: &OutgoingMessage, saved: bool) {
        let mut c = ComposeState::resume(message, saved);
        c.sync_cursor();
        self.compose = Some(Box::new(c));
        self.screen = Screen::Compose;
    }

    fn touch_compose(&mut self, cmds: &mut Vec<Cmd>) {
        let now = self.clock.now();
        if let Some(c) = self.compose.as_mut() {
            let at = c.touch(now);
            cmds.push(Cmd::WakeAt(at));
        }
    }

    fn refresh_suggestions(&mut self, data: &dyn MailData) {
        let Some(c) = self.compose.as_mut() else {
            return;
        };
        let Some(prefix) = c.completion_prefix() else {
            c.suggest.clear();
            return;
        };
        let taken: Vec<String> = [&c.to, &c.cc, &c.bcc]
            .iter()
            .flat_map(|a| a.chips.iter().map(|p| p.email.to_lowercase()))
            .collect();
        let mut out: Vec<Participant> = Vec::new();
        for contact in data.contacts_prefix(&prefix, SUGGESTIONS) {
            for email in &contact.emails {
                if out.len() >= SUGGESTIONS {
                    break;
                }
                if taken.contains(&email.to_lowercase())
                    || out.iter().any(|p| p.email.eq_ignore_ascii_case(email))
                {
                    continue;
                }
                out.push(Participant {
                    name: (!contact.name.trim().is_empty()).then(|| contact.name.clone()),
                    email: email.clone(),
                });
            }
        }
        c.suggest = out;
        c.suggest_cursor = c.suggest_cursor.min(c.suggest.len().saturating_sub(1));
    }

    /// Keys on the compose screen. Returns false to let the keymap handle
    /// the key.
    pub(crate) fn compose_key(
        &mut self,
        data: &dyn MailData,
        key: Key,
        cmds: &mut Vec<Cmd>,
    ) -> bool {
        if !self.pending.is_empty() {
            return false;
        }
        let Some(c) = self.compose.as_mut() else {
            return false;
        };
        if c.editing {
            if !c.suggest.is_empty() && matches!(key.name, KeyName::Tab | KeyName::Esc) {
                if key.name == KeyName::Tab {
                    c.accept_suggestion();
                    self.touch_compose(cmds);
                } else {
                    c.suggest.clear();
                }
                return true;
            }
            // Keys the compose table binds (or starts a sequence with) go
            // to the keymap; everything else types.
            let mut scratch = Pending::default();
            let res = self
                .config
                .keymap
                .feed(&mut scratch, &[Context::Compose], key);
            if matches!(res, Resolution::Action { .. } | Resolution::Pending) {
                return false;
            }
        }
        let Some(c) = self.compose.as_mut() else {
            return false;
        };
        let outcome = c.key(&key);
        c.sync_cursor();
        match outcome {
            KeyOutcome::Pass => return false,
            KeyOutcome::Used { changed } => {
                if changed {
                    self.touch_compose(cmds);
                }
            }
            KeyOutcome::PickFrom => self.open_from_picker(data),
            KeyOutcome::AttachPrompt => self.attach_prompt(),
        }
        self.refresh_suggestions(data);
        true
    }

    /// Pasted text in compose.
    pub(crate) fn compose_paste(&mut self, data: &dyn MailData, text: &str, cmds: &mut Vec<Cmd>) {
        let Some(c) = self.compose.as_mut() else {
            return;
        };
        if c.paste(text) {
            c.sync_cursor();
            self.touch_compose(cmds);
        }
        self.refresh_suggestions(data);
    }

    fn attach_prompt(&mut self) {
        self.overlays.push(Overlay::Prompt(Prompt {
            label: "Attach file:".into(),
            input: LineInput::default(),
            on_enter: PromptAction::AttachPath,
        }));
    }

    /// Runs a compose action. Returns false when the action is not a
    /// compose action.
    pub(crate) fn compose_action(
        &mut self,
        data: &dyn MailData,
        action: Action,
        cmds: &mut Vec<Cmd>,
    ) -> bool {
        if self.screen != Screen::Compose || self.compose.is_none() {
            return false;
        }
        match action {
            Action::LeaveField => {
                let Some(c) = self.compose.as_mut() else {
                    return true;
                };
                if c.editing {
                    let f = c.field;
                    if let Some(a) = c.address_mut(f) {
                        a.commit();
                    }
                    c.suggest.clear();
                    c.editing = false;
                    c.sync_cursor();
                } else {
                    self.close_compose(cmds);
                }
            }
            Action::NextField | Action::PrevField => {
                if let Some(c) = self.compose.as_mut() {
                    c.step_field(action == Action::NextField);
                    c.sync_cursor();
                }
                self.refresh_suggestions(data);
            }
            Action::Send => self.send(false, cmds),
            Action::SaveDraft => self.save_draft(cmds),
            Action::Attach => self.attach_prompt(),
            Action::ExternalEditor => {
                if let Some(c) = &self.compose {
                    cmds.push(Cmd::OpenEditor {
                        draft_id: c.draft_id.clone(),
                        text: c.body_text(),
                    });
                }
            }
            Action::DiscardDraft => self.overlays.push(Overlay::Confirm(Confirm {
                prompt: "Discard this draft?".into(),
                on_yes: ConfirmAction::DiscardDraft,
            })),
            Action::ToggleBcc => {
                if let Some(c) = self.compose.as_mut() {
                    c.toggle_bcc();
                    c.sync_cursor();
                }
            }
            Action::PickFrom => self.open_from_picker(data),
            Action::NewMaskFrom => self.new_mask_for_compose(),
            _ => return false,
        }
        true
    }

    fn new_mask_for_compose(&mut self) {
        let Some(c) = &self.compose else {
            return;
        };
        let domain =
            c.to.chips
                .first()
                .or(c.cc.chips.first())
                .and_then(|p| address::domain(&p.email));
        let draft_id = Some(c.draft_id.clone());
        match domain {
            Some(for_domain) => self.overlays.push(Overlay::Prompt(Prompt {
                label: format!("Description for a new masked address for {for_domain}:"),
                input: LineInput::default(),
                on_enter: PromptAction::MaskedDescription {
                    for_domain,
                    draft_id,
                },
            })),
            None => self.overlays.push(Overlay::Prompt(Prompt {
                label: "Site for the new masked address:".into(),
                input: LineInput::default(),
                on_enter: PromptAction::MaskedDomain,
            })),
        }
    }

    fn open_from_picker(&mut self, data: &dyn MailData) {
        let Some(c) = &self.compose else {
            return;
        };
        let mut items = Vec::new();
        for (i, id) in data.identities().into_iter().enumerate() {
            let from = Participant {
                name: (!id.name.trim().is_empty()).then(|| id.name.clone()),
                email: id.email.clone(),
            };
            let tag = if i == 0 { "default" } else { "identity" };
            items.push(
                PickItem::new(
                    address::format(&from),
                    PickValue::From {
                        identity_id: Some(id.id.clone()),
                        from,
                    },
                )
                .tag(tag),
            );
        }
        let default_id = items.iter().find_map(|i| match &i.value {
            PickValue::From { identity_id, .. } => identity_id.clone(),
            _ => None,
        });
        let name = c.from.name.clone();
        if let Some(domain) = c.to.chips.first().and_then(|p| address::domain(&p.email)) {
            items.push(
                PickItem::new(
                    format!("+ New masked email for {domain}"),
                    PickValue::NewMask(domain),
                )
                .tag("new"),
            );
        }
        let masks: Vec<_> = data
            .masked_emails()
            .into_iter()
            .filter(|m| m.state == crate::data::MaskedState::Enabled)
            .collect();
        if !masks.is_empty() {
            items.push(PickItem::new(
                format!("Masked addresses ({} enabled)", masks.len()),
                PickValue::Info,
            ));
        }
        for m in masks {
            let label = if m.description.trim().is_empty() {
                m.for_domain.clone()
            } else {
                m.description.clone()
            };
            items.push(
                PickItem::new(
                    m.email.clone(),
                    PickValue::From {
                        identity_id: default_id.clone(),
                        from: Participant {
                            name: name.clone(),
                            email: m.email.clone(),
                        },
                    },
                )
                .tag(label),
            );
        }
        let mut p = Picker::new(PickerKind::From, "Send as", items);
        p.hint = "type to filter".into();
        p.footer = vec!["! Sending as a mask over JMAP is not yet tested".into()];
        self.overlays.push(Overlay::Picker(Box::new(p)));
    }

    /// Sets the From address from the picker.
    pub(crate) fn set_from(&mut self, identity_id: Option<String>, from: Participant) {
        if let Some(c) = self.compose.as_mut() {
            c.identity_id = identity_id;
            c.from = from;
            c.dirty = true;
            c.last_edit = Some(self.clock.now());
        }
    }

    fn save_draft(&mut self, cmds: &mut Vec<Cmd>) {
        if let Some(c) = self.compose.as_mut() {
            cmds.push(Cmd::SaveDraft(Box::new(c.message())));
            c.dirty = false;
            c.saving = true;
        }
    }

    /// Autosaves the draft when it has been idle long enough.
    pub(crate) fn autosave(&mut self, cmds: &mut Vec<Cmd>) {
        let now = self.clock.now();
        if let Some(c) = self.compose.as_mut()
            && c.autosave_at().is_some_and(|at| at <= now)
        {
            cmds.push(Cmd::SaveDraft(Box::new(c.message())));
            c.dirty = false;
            c.saving = true;
        }
    }

    /// Leaves compose, keeping the draft unless nothing was written.
    pub(crate) fn close_compose(&mut self, cmds: &mut Vec<Cmd>) {
        let Some(c) = self.compose.take() else {
            return;
        };
        self.screen = Screen::Mail;
        if c.is_blank() {
            if c.ever_saved {
                cmds.push(Cmd::DeleteDraft(c.draft_id.clone()));
            }
            self.notify(
                "Nothing written; no draft kept",
                ToastLevel::Info,
                false,
                cmds,
            );
            return;
        }
        if c.dirty || !c.ever_saved || c.saving {
            cmds.push(Cmd::SaveDraft(Box::new(c.message())));
        }
        let text = if self.draft_sync().on_device(&c.draft_id) {
            "Draft saved on this device"
        } else {
            "Draft saved in Drafts"
        };
        self.notify(text, ToastLevel::Success, false, cmds);
    }

    /// Saves an open draft before quitting, so `Q` never loses text.
    pub(crate) fn flush_compose(&mut self, cmds: &mut Vec<Cmd>) {
        if let Some(c) = self.compose.as_mut()
            && !c.is_blank()
            && (c.dirty || !c.ever_saved || c.saving)
        {
            cmds.push(Cmd::SaveDraft(Box::new(c.message())));
            c.dirty = false;
        }
    }

    /// Discards the open draft after the confirm.
    pub(crate) fn discard_draft(&mut self, cmds: &mut Vec<Cmd>) {
        if let Some(c) = self.compose.take() {
            cmds.push(Cmd::DeleteDraft(c.draft_id.clone()));
            self.screen = Screen::Mail;
            self.notify("Draft discarded", ToastLevel::Success, false, cmds);
        }
    }

    /// Checks and sends the open draft. With `force`, warnings that asked
    /// for a confirm are skipped.
    pub(crate) fn send(&mut self, force: bool, cmds: &mut Vec<Cmd>) {
        let Some(c) = self.compose.as_mut() else {
            return;
        };
        if !c.commit_all() {
            let err = [&c.to, &c.cc, &c.bcc]
                .iter()
                .find_map(|a| a.error.clone())
                .unwrap_or_else(|| "an address is not valid".into());
            c.field = if c.to.error.is_some() {
                Field::To
            } else if c.cc.error.is_some() {
                Field::Cc
            } else {
                Field::Bcc
            };
            c.editing = true;
            c.sync_cursor();
            self.notify(format!("Not sent: {err}"), ToastLevel::Error, false, cmds);
            return;
        }
        if c.to.chips.is_empty() && c.cc.chips.is_empty() && c.bcc.chips.is_empty() {
            c.field = Field::To;
            c.editing = true;
            c.sync_cursor();
            self.notify(
                "Not sent: add at least one recipient",
                ToastLevel::Error,
                false,
                cmds,
            );
            return;
        }
        if !c.uploading.is_empty() {
            self.notify(
                "Not sent yet: an attachment is still uploading",
                ToastLevel::Warning,
                false,
                cmds,
            );
            return;
        }
        if !force {
            let warning = if c.subject.text().trim().is_empty() {
                Some("The subject is empty. Send anyway?")
            } else if c.missing_attachment() {
                Some("The message mentions an attachment, but nothing is attached. Send anyway?")
            } else {
                None
            };
            if let Some(w) = warning {
                self.overlays.push(Overlay::Confirm(Confirm {
                    prompt: w.into(),
                    on_yes: ConfirmAction::SendAnyway,
                }));
                return;
            }
        }
        let message = c.message();
        cmds.push(Cmd::Send(Box::new(message.clone())));
        self.compose = None;
        self.screen = Screen::Mail;
        let now = self.clock.now();
        let window =
            SignedDuration::try_from(self.config.undo_send).unwrap_or(SignedDuration::ZERO);
        let deadline = now.checked_add(window).unwrap_or(now);
        self.sends
            .retain(|s| s.message.draft_id != message.draft_id);
        let draft_id = message.draft_id.clone();
        self.sends.push(PendingSend { message, deadline });
        if window.is_zero() {
            self.notify("Sending", ToastLevel::Info, false, cmds);
        } else {
            self.toast = Some(Toast {
                text: "Sending".into(),
                level: ToastLevel::Info,
                undo: true,
                expires: deadline,
                sending: Some(draft_id),
            });
            cmds.push(Cmd::WakeAt(
                now.checked_add(SignedDuration::from_secs(1))
                    .unwrap_or(deadline)
                    .min(deadline),
            ));
            cmds.push(Cmd::WakeAt(deadline));
        }
    }

    /// The undo key during an undo send countdown cancels the send.
    /// Returns false when no countdown is showing.
    pub(crate) fn cancel_send(&mut self, cmds: &mut Vec<Cmd>) -> bool {
        let Some(id) = self.countdown().map(|(id, _)| id) else {
            return false;
        };
        cmds.push(Cmd::CancelSend(id));
        self.notify("Cancelling the send", ToastLevel::Info, false, cmds);
        true
    }

    /// Progress of a send from the shell.
    pub(crate) fn send_report(&mut self, report: SendReport, cmds: &mut Vec<Cmd>) {
        let id = report.draft_id;
        let subject = self
            .sends
            .iter()
            .find(|s| s.message.draft_id == id)
            .map(|s| s.message.subject.clone())
            .unwrap_or_default();
        let quoted = if subject.is_empty() {
            "the message".to_owned()
        } else {
            format!("\"{subject}\"")
        };
        match report.outcome {
            SendOutcome::Scheduled { at } => {
                if let Some(s) = self.sends.iter_mut().find(|s| s.message.draft_id == id) {
                    s.deadline = at;
                }
                if let Some(t) = self.toast.as_mut()
                    && t.sending.as_ref() == Some(&id)
                {
                    t.expires = at;
                    cmds.push(Cmd::WakeAt(at));
                }
            }
            SendOutcome::Sent => {
                self.sends.retain(|s| s.message.draft_id != id);
                self.notify(format!("Sent {quoted}"), ToastLevel::Success, false, cmds);
            }
            SendOutcome::TooLate => {
                self.sends.retain(|s| s.message.draft_id != id);
                self.notify(
                    "Too late to cancel: the message was sent",
                    ToastLevel::Warning,
                    false,
                    cmds,
                );
            }
            SendOutcome::Cancelled { message } => {
                self.sends.retain(|s| s.message.draft_id != id);
                self.reopen(&message, cmds);
                self.notify(
                    "Send cancelled. The message is back in compose",
                    ToastLevel::Success,
                    false,
                    cmds,
                );
            }
            SendOutcome::Failed { error, message } => {
                self.sends.retain(|s| s.message.draft_id != id);
                let back = self.reopen(&message, cmds);
                let where_ = if back {
                    "it is back in compose"
                } else {
                    "it is in Drafts"
                };
                self.notify(
                    format!(
                        "Sending {quoted} failed: {}; {where_}",
                        crate::text::sanitize(&error)
                    ),
                    ToastLevel::Error,
                    false,
                    cmds,
                );
            }
        }
    }

    /// Reopens a message in compose, or keeps it as a draft when another
    /// message is being written. Returns true when compose shows it.
    fn reopen(&mut self, message: &OutgoingMessage, cmds: &mut Vec<Cmd>) -> bool {
        match &self.compose {
            Some(c) if c.draft_id != message.draft_id => {
                cmds.push(Cmd::SaveDraft(Box::new(message.clone())));
                false
            }
            _ => {
                self.resume_compose(message, true);
                true
            }
        }
    }

    /// The local save of a draft finished.
    pub(crate) fn draft_saved(
        &mut self,
        draft_id: &DraftId,
        error: Option<String>,
        cmds: &mut Vec<Cmd>,
    ) {
        let now = self.clock.now();
        let Some(c) = self.compose.as_mut().filter(|c| &c.draft_id == draft_id) else {
            if let Some(e) = error {
                self.notify(
                    format!("Draft not saved: {}", crate::text::sanitize(&e)),
                    ToastLevel::Error,
                    false,
                    cmds,
                );
            }
            return;
        };
        c.saving = false;
        match error {
            None => {
                c.saved_at = Some(now);
                c.save_error = None;
                c.ever_saved = true;
            }
            Some(e) => {
                c.save_error = Some(e);
                // Try again on the next pause.
                c.dirty = true;
            }
        }
    }

    /// The external editor closed.
    pub(crate) fn editor_done(
        &mut self,
        draft_id: &DraftId,
        result: Result<String, String>,
        cmds: &mut Vec<Cmd>,
    ) {
        match result {
            Ok(text) => {
                if let Some(c) = self.compose.as_mut().filter(|c| &c.draft_id == draft_id) {
                    c.replace_body(&text);
                    c.field = Field::Body;
                    c.editing = true;
                    c.sync_cursor();
                    self.touch_compose(cmds);
                }
            }
            Err(e) => self.notify(
                format!("The editor failed: {}", crate::text::sanitize(&e)),
                ToastLevel::Error,
                false,
                cmds,
            ),
        }
    }

    /// A file upload finished.
    pub(crate) fn attached(
        &mut self,
        draft_id: &DraftId,
        path: &str,
        result: Result<crate::data::OutgoingAttachment, String>,
        cmds: &mut Vec<Cmd>,
    ) {
        let Some(c) = self.compose.as_mut().filter(|c| &c.draft_id == draft_id) else {
            return;
        };
        if let Some(i) = c.uploading.iter().position(|p| p == path) {
            c.uploading.remove(i);
        }
        match result {
            Ok(a) => {
                let name = a.name.clone();
                c.attachments.push(a);
                self.touch_compose(cmds);
                self.notify(
                    format!("Attached {}", crate::text::sanitize(&name)),
                    ToastLevel::Success,
                    false,
                    cmds,
                );
            }
            Err(e) => self.notify(
                format!(
                    "Could not attach {}: {}",
                    crate::text::sanitize(path),
                    crate::text::sanitize(&e)
                ),
                ToastLevel::Error,
                false,
                cmds,
            ),
        }
    }

    /// Starts uploading a typed path.
    pub(crate) fn attach_path(&mut self, path: &str, cmds: &mut Vec<Cmd>) {
        let path = path.trim();
        if path.is_empty() {
            return;
        }
        if let Some(c) = self.compose.as_mut() {
            c.uploading.push(path.to_owned());
            cmds.push(Cmd::AttachFile {
                draft_id: c.draft_id.clone(),
                path: path.to_owned(),
            });
        }
    }

    /// The original body arrived: quote it into the reply.
    pub(crate) fn compose_body_fetched(&mut self, data: &dyn MailData, id: &crate::data::EmailId) {
        let Some(c) = self.compose.as_mut() else {
            return;
        };
        if c.quote_wait.as_ref().is_some_and(|(e, _)| e == id)
            && let Some(body) = data.message_body(id)
        {
            c.fill_quote(&body);
        }
    }

    /// Resumes the draft behind a row of the Drafts mailbox, if mejl has
    /// one. Returns true when compose opened.
    pub(crate) fn resume_draft_row(&mut self, data: &dyn MailData) -> bool {
        if self.mailbox_role() != Some(crate::data::MailboxRole::Drafts) {
            return false;
        }
        let Some(row) = self.list.pages.row(self.list.cursor) else {
            return false;
        };
        let latest = row.latest_email_id.clone();
        let found = data
            .drafts()
            .into_iter()
            .find(|d| d.server_email_id.as_ref() == Some(&latest));
        match found {
            Some(d) => {
                self.resume_compose(&d.message, true);
                true
            }
            None => false,
        }
    }

    /// Resumes a draft by id.
    pub(crate) fn resume_draft(&mut self, data: &dyn MailData, id: &DraftId) {
        if let Some(d) = data
            .drafts()
            .into_iter()
            .find(|d| &d.message.draft_id == id)
        {
            self.resume_compose(&d.message, true);
        }
    }
}
