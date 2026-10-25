//! The Masked Email screen in `update`, and the `@` actions on a masked
//! message from the mail screen.

use crate::action::Action;
use crate::address;
use crate::data::{MailData, MaskedEmailView, MaskedState};
use crate::event::{Cmd, ToastLevel};
use crate::input::{InputResult, LineInput};
use crate::keymap::{Key, KeyName};
use crate::masked::{MaskedScreen, STATES, Senders, SilentFilter, state_name};
use crate::state::{AppState, Confirm, ConfirmAction, Overlay, Prompt, PromptAction, Screen};
use crate::text::sanitize;

/// Messages read to list who mails an address.
const SENDER_SAMPLE: usize = 50;

impl AppState {
    /// Opens the Masked Email screen, at `id` if given.
    pub(crate) fn open_masked(&mut self, data: &dyn MailData, id: Option<&str>) {
        let mut cmds = Vec::new();
        self.leave_screen(&mut cmds);
        let all = data.masked_emails();
        let now = self.clock.now();
        let mut screen = match self.masked.take() {
            Some(mut s) => {
                s.reload(all, now);
                s
            }
            None => Box::new(MaskedScreen::new(all, now)),
        };
        if let Some(id) = id {
            screen.focus(id);
        }
        self.masked = Some(screen);
        self.screen = Screen::Masked;
        self.pending.clear();
        self.refresh_senders(data);
        self.toast = None;
        let _ = cmds;
    }

    /// Reads who sends to the address under the cursor, once per address.
    pub(crate) fn refresh_senders(&mut self, data: &dyn MailData) {
        let Some(m) = self.masked.as_ref().and_then(|s| s.selected().cloned()) else {
            return;
        };
        if self
            .masked
            .as_ref()
            .and_then(|s| s.senders.as_ref())
            .is_some_and(|s| s.id == m.id)
        {
            return;
        }
        let results = data.search_local(&format!("to:{}", m.email), SENDER_SAMPLE);
        let mut senders: Vec<(String, usize)> = Vec::new();
        for h in results.hits {
            let email = h.row.from.email.to_lowercase();
            match senders.iter_mut().find(|(e, _)| *e == email) {
                Some((_, n)) => *n += 1,
                None => senders.push((email, 1)),
            }
        }
        senders.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        if let Some(s) = self.masked.as_mut() {
            s.senders = Some(Senders { id: m.id, senders });
        }
    }

    /// Typing into the filter. Returns false to let the keymap have the
    /// key.
    pub(crate) fn masked_key(
        &mut self,
        data: &dyn MailData,
        key: Key,
        cmds: &mut Vec<Cmd>,
    ) -> bool {
        let _ = cmds;
        let Some(s) = self.masked.as_mut() else {
            return false;
        };
        if !s.filtering || !self.pending.is_empty() {
            return false;
        }
        match key.name {
            KeyName::Esc | KeyName::Enter => {
                s.filtering = false;
            }
            KeyName::Up => s.step(-1),
            KeyName::Down => s.step(1),
            _ => {
                if s.filter.handle(&key) == InputResult::Changed {
                    s.cursor = 0;
                    s.refilter(None);
                }
            }
        }
        self.refresh_senders(data);
        true
    }

    /// Pasted text on the Masked Email screen goes into the filter.
    pub(crate) fn masked_paste(&mut self, text: &str) {
        if let Some(s) = self.masked.as_mut() {
            s.filtering = true;
            s.filter.insert_str(text);
            s.cursor = 0;
            s.refilter(None);
        }
    }

    fn masked_prompt_new(&mut self, domain: Option<String>) {
        match domain {
            Some(for_domain) => self.overlays.push(Overlay::Prompt(Prompt {
                label: format!("Description for a new masked address for {for_domain}:"),
                input: LineInput::default(),
                on_enter: PromptAction::MaskedDescription {
                    for_domain,
                    draft_id: None,
                },
            })),
            None => self.overlays.push(Overlay::Prompt(Prompt {
                label: "Site for the new masked address:".into(),
                input: LineInput::default(),
                on_enter: PromptAction::MaskedDomain,
            })),
        }
    }

    fn masked_rename(&mut self, m: &MaskedEmailView) {
        self.overlays.push(Overlay::Prompt(Prompt {
            label: format!("Description for {}:", m.email),
            input: LineInput::new(m.description.clone()),
            on_enter: PromptAction::MaskedRename { id: m.id.clone() },
        }));
    }

    /// Runs a Masked Email action on the masked screen. Returns false for
    /// other actions.
    pub(crate) fn masked_action(
        &mut self,
        data: &dyn MailData,
        action: Action,
        cmds: &mut Vec<Cmd>,
    ) -> bool {
        let Some(s) = self.masked.as_mut() else {
            return false;
        };
        let selected = s.selected().cloned();
        match action {
            Action::Next | Action::Prev => s.step(if action == Action::Next { 1 } else { -1 }),
            Action::PageDown | Action::HalfPageDown => s.step(10),
            Action::PageUp | Action::HalfPageUp => s.step(-10),
            Action::GoTop => s.cursor = 0,
            Action::GoBottom => s.cursor = s.shown.len().saturating_sub(1),
            Action::Filter => s.filtering = true,
            Action::MaskedShowEnabled
            | Action::MaskedShowDisabled
            | Action::MaskedShowPending
            | Action::MaskedShowDeleted => {
                let i = match action {
                    Action::MaskedShowEnabled => 0,
                    Action::MaskedShowDisabled => 1,
                    Action::MaskedShowPending => 2,
                    _ => 3,
                };
                s.show[i] = !s.show[i];
                s.refilter(None);
                let text = format!(
                    "{} {} addresses",
                    if s.show[i] { "Showing" } else { "Hiding" },
                    state_name(STATES[i])
                );
                self.notify(text, ToastLevel::Info, false, cmds);
            }
            Action::MaskedCycleSilent => {
                s.silent = s.silent.next();
                s.now = self.clock.now();
                s.refilter(None);
                let text = match s.silent {
                    SilentFilter::Any => "Showing addresses with or without recent mail",
                    SilentFilter::Silent => "Showing silent addresses: no mail for 6+ months",
                    SilentFilter::Active => "Showing active addresses: mail within 6 months",
                };
                self.notify(text, ToastLevel::Info, false, cmds);
            }
            Action::MaskedCycleCreatedBy => {
                s.cycle_created_by();
                s.refilter(None);
                let text = match &s.created_by {
                    Some(by) => format!("Showing addresses created by {}", sanitize(by)),
                    None => "Showing addresses created by any client".to_owned(),
                };
                self.notify(text, ToastLevel::Info, false, cmds);
            }
            Action::MaskedNew => self.masked_prompt_new(None),
            Action::MaskedEnable | Action::MaskedDisable => {
                if let Some(m) = selected {
                    let state = if action == Action::MaskedEnable {
                        MaskedState::Enabled
                    } else {
                        MaskedState::Disabled
                    };
                    if m.state == state {
                        self.notify(
                            format!("{} is already {}", m.email, state_name(state)),
                            ToastLevel::Info,
                            false,
                            cmds,
                        );
                    } else {
                        self.set_mask_state(data, &m.id, state, cmds);
                        if let Some(t) = self.toast.as_mut() {
                            t.undo = true;
                        }
                    }
                }
            }
            Action::MaskedDelete => {
                if let Some(m) = selected {
                    self.set_mask_state(data, &m.id, MaskedState::Deleted, cmds);
                }
            }
            Action::MaskedRename => {
                if let Some(m) = selected {
                    self.masked_rename(&m);
                }
            }
            Action::MaskedCopy => {
                if let Some(m) = selected {
                    cmds.push(Cmd::CopyToClipboard(m.email.clone()));
                    self.notify(
                        format!("Copied {}", m.email),
                        ToastLevel::Success,
                        false,
                        cmds,
                    );
                }
            }
            Action::MaskedMessages | Action::Open => {
                if let Some(m) = selected {
                    self.open_results_for(data, &format!("to:{}", m.email), cmds);
                }
            }
            Action::MaskedRule => {
                if let Some(m) = selected {
                    self.new_rule_for_address(data, &m.email, cmds);
                }
            }
            Action::Back => {
                if s.filtering {
                    s.filtering = false;
                } else if !s.filter.is_empty() {
                    s.filter.clear();
                    s.refilter(None);
                } else {
                    self.screen = Screen::Mail;
                }
            }
            _ => return false,
        }
        self.refresh_senders(data);
        true
    }

    /// The masked address the focused (or cursor) message was sent to.
    pub(crate) fn mail_mask(&self, data: &dyn MailData) -> Option<MaskedEmailView> {
        let conv = self.reader.conversation.as_ref();
        let msg = conv.and_then(|c| c.messages.get(self.reader.focused).or(c.messages.last()));
        let found = msg.and_then(|m| {
            m.to.iter()
                .chain(&m.cc)
                .find_map(|p| data.masked_for_address(&p.email))
        });
        if found.is_some() {
            return found;
        }
        let row = self.list.pages.row(self.list.cursor)?;
        let conv = data.conversation(&row.thread_id)?;
        conv.messages.iter().rev().find_map(|m| {
            m.to.iter()
                .chain(&m.cc)
                .find_map(|p| data.masked_for_address(&p.email))
        })
    }

    /// `@` actions on the mail screen: they act on the masked address the
    /// message was sent to. Returns false for other actions.
    pub(crate) fn mail_mask_action(
        &mut self,
        data: &dyn MailData,
        action: Action,
        cmds: &mut Vec<Cmd>,
    ) -> bool {
        if !matches!(
            action,
            Action::MaskedEnable
                | Action::MaskedDisable
                | Action::MaskedDelete
                | Action::MaskedRename
                | Action::MaskedCopy
                | Action::MaskedMessages
                | Action::MaskedRule
                | Action::MaskedNew
        ) {
            return false;
        }
        if action == Action::MaskedNew {
            let domain = self
                .reader
                .conversation
                .as_ref()
                .and_then(|c| c.messages.last())
                .and_then(|m| address::domain(&m.from.email))
                .or_else(|| {
                    self.list
                        .pages
                        .row(self.list.cursor)
                        .and_then(|r| address::domain(&r.from.email))
                });
            self.masked_prompt_new(domain);
            return true;
        }
        let Some(m) = self.mail_mask(data) else {
            self.notify(
                "This message was not sent to a masked address",
                ToastLevel::Info,
                false,
                cmds,
            );
            return true;
        };
        let what = if m.description.trim().is_empty() {
            m.email.clone()
        } else {
            format!("{} ({})", m.email, m.description)
        };
        match action {
            Action::MaskedDisable => self.overlays.push(Overlay::Confirm(Confirm {
                prompt: format!("Disable {what}? Future mail goes to Trash."),
                on_yes: ConfirmAction::DisableMasked { id: m.id.clone() },
            })),
            Action::MaskedEnable => self.set_mask_state(data, &m.id, MaskedState::Enabled, cmds),
            Action::MaskedDelete => self.set_mask_state(data, &m.id, MaskedState::Deleted, cmds),
            Action::MaskedRename => self.masked_rename(&m),
            Action::MaskedCopy => {
                cmds.push(Cmd::CopyToClipboard(m.email.clone()));
                self.notify(
                    format!("Copied {}", m.email),
                    ToastLevel::Success,
                    false,
                    cmds,
                );
            }
            Action::MaskedMessages => {
                self.open_results_for(data, &format!("to:{}", m.email), cmds);
            }
            Action::MaskedRule => self.new_rule_for_address(data, &m.email, cmds),
            _ => {}
        }
        true
    }
}
