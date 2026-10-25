//! Pickers, prompts and menus in `update`: building them, their keys and
//! what choosing an item does.

use crate::action::{Action, Category};
use crate::address;
use crate::data::{ComposeKind, MailData, MailboxId, MailboxRole, MaskedState, Participant};
use crate::event::{Cmd, Mutation, Target, TargetIds, ToastLevel};
use crate::input::{InputResult, LineInput};
use crate::keymap::{Key, KeyName};
use crate::picker::{PickItem, PickValue, Picker, PickerKind};
use crate::state::{
    AppState, Confirm, ConfirmAction, Focus, Overlay, Prompt, PromptAction, Screen,
};

/// Most recent actions remembered for the palette.
const RECENT: usize = 8;

fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

/// Can the palette offer this action on this screen?
pub(crate) fn in_scope(action: Action, screen: Screen) -> bool {
    if matches!(action, Action::Palette | Action::Repeat) {
        return false;
    }
    match action.category() {
        Category::Compose => screen == Screen::Compose,
        Category::Search => screen == Screen::Search,
        Category::Masked => screen == Screen::Masked,
        Category::Rules => screen == Screen::Rules,
        Category::Settings => screen == Screen::Settings,
        Category::Navigate | Category::Select | Category::Triage | Category::Read => {
            screen == Screen::Mail
        }
        Category::Go | Category::Tools | Category::App => true,
    }
}

/// The short tag the palette shows for an action's group.
fn category_tag(c: Category) -> &'static str {
    match c {
        Category::Navigate => "move",
        Category::Go => "go to",
        Category::Tools => "tools",
        Category::Select => "select",
        Category::Triage => "triage",
        Category::Read => "read",
        Category::App => "app",
        Category::Compose => "compose",
        Category::Search => "search",
        Category::Masked => "masked",
        Category::Rules => "rules",
        Category::Settings => "settings",
    }
}

fn rule_name(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    v.get("name")?.as_str().map(str::to_owned)
}

impl AppState {
    /// Remembers an action for the palette's "recent first" order.
    pub(crate) fn remember(&mut self, action: Action) {
        if action.takes_count()
            || matches!(
                action,
                Action::Palette | Action::Back | Action::Help | Action::Open | Action::Repeat
            )
        {
            return;
        }
        self.recent.retain(|a| *a != action);
        self.recent.insert(0, action);
        self.recent.truncate(RECENT);
    }

    /// Opens the command palette.
    pub(crate) fn open_palette(&mut self, data: &dyn MailData) {
        let contexts = self.contexts();
        let keymap = &self.config.keymap;
        let key = |a: Action| {
            keymap
                .keys_for(a, &contexts)
                .map(|k| k.to_string())
                .unwrap_or_default()
        };
        let mut order: Vec<Action> = self
            .recent
            .iter()
            .copied()
            .filter(|a| in_scope(*a, self.screen))
            .collect();
        for &a in Action::ALL {
            if in_scope(a, self.screen) && !order.contains(&a) {
                order.push(a);
            }
        }
        let sender = self
            .list
            .pages
            .row(self.list.cursor)
            .map(|r| r.from.display().to_owned());
        let mut items: Vec<PickItem> = order
            .into_iter()
            .map(|a| {
                let mut label = a.description().to_owned();
                if matches!(a, Action::NewRule | Action::Reply | Action::Forward)
                    && let Some(name) = &sender
                    && self.screen == Screen::Mail
                {
                    label.push_str(&format!(" ({name})"));
                }
                PickItem::new(label, PickValue::Action(a))
                    .key(key(a))
                    .tag(category_tag(a.category()))
            })
            .collect();
        let go = key(Action::GoMailbox);
        for it in &self.sidebar {
            if let crate::state::SidebarItem::Mailbox { id, name, .. } = it {
                items.push(
                    PickItem::new(format!("Go to: {name}"), PickValue::Mailbox(id.clone()))
                        .key(go.clone())
                        .tag("mailbox"),
                );
            }
        }
        for s in data.saved_searches() {
            items.push(
                PickItem::new(
                    format!("Search: {}", s.name),
                    PickValue::SavedSearch(s.query),
                )
                .tag("saved search"),
            );
        }
        for d in data.drafts() {
            let subject = if d.message.subject.trim().is_empty() {
                "(no subject)".to_owned()
            } else {
                d.message.subject.clone()
            };
            items.push(
                PickItem::new(
                    format!("Resume draft: {subject}"),
                    PickValue::Draft(d.message.draft_id),
                )
                .tag("draft"),
            );
        }
        for c in data.contacts_prefix("", 200) {
            for e in &c.emails {
                let p = Participant {
                    name: (!c.name.trim().is_empty()).then(|| c.name.clone()),
                    email: e.clone(),
                };
                items.push(
                    PickItem::new(
                        format!("Contact: {}", address::format(&p)),
                        PickValue::Contact(p),
                    )
                    .tag("contact"),
                );
            }
        }
        for m in data.masked_emails() {
            let what = if m.description.trim().is_empty() {
                m.for_domain.clone()
            } else {
                m.description.clone()
            };
            items.push(
                PickItem::new(
                    format!("Masked email: {} ({what})", m.email),
                    PickValue::Masked(m.id),
                )
                .tag("masked"),
            );
        }
        for r in data.rules() {
            let name = rule_name(&r.json).unwrap_or_else(|| r.id.clone());
            items.push(PickItem::new(format!("Rule: {name}"), PickValue::Rule(r.id)).tag("rule"));
        }
        let mut p = Picker::new(PickerKind::Palette, "Commands", items);
        p.hint = ": or Ctrl-k".into();
        p.footer = vec![
            "Each row shows its key, so the palette teaches the shortcuts.".into(),
            "Also searches: mailboxes, saved searches, contacts, rules, masked addresses.".into(),
        ];
        self.overlays.push(Overlay::Picker(Box::new(p)));
    }

    fn mailbox_path(&self, id: &MailboxId) -> String {
        let mut parts = Vec::new();
        let mut cur = Some(id.clone());
        let mut guard = 0;
        while let Some(c) = cur {
            guard += 1;
            let Some(m) = self.mailboxes.iter().find(|m| m.id == c) else {
                break;
            };
            parts.push(if m.name.trim().is_empty() {
                m.role
                    .map_or("Mailbox", MailboxRole::default_name)
                    .to_owned()
            } else {
                m.name.clone()
            });
            cur = m.parent.clone();
            if guard > 16 {
                break;
            }
        }
        parts.reverse();
        parts.join("/")
    }

    /// Opens the move picker (`label: false`) or the label picker.
    pub(crate) fn open_mailbox_picker(
        &mut self,
        data: &dyn MailData,
        label: bool,
        cmds: &mut Vec<Cmd>,
    ) {
        let Some((target, n)) = self.action_target() else {
            self.notify("Nothing to move", ToastLevel::Info, false, cmds);
            return;
        };
        let members: Vec<MailboxId> = match &target.ids {
            TargetIds::Threads(t) if t.len() == 1 => data
                .conversation(&t[0])
                .map(|c| {
                    let mut v: Vec<MailboxId> = c
                        .messages
                        .iter()
                        .flat_map(|m| m.mailboxes.iter().cloned())
                        .collect();
                    v.sort();
                    v.dedup();
                    v
                })
                .unwrap_or_default(),
            TargetIds::Emails(e) => self
                .reader
                .conversation
                .as_ref()
                .and_then(|c| c.messages.iter().find(|m| e.contains(&m.email_id)))
                .map(|m| m.mailboxes.clone())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let current = self.mailbox.clone();
        let mut items = Vec::new();
        let mut boxes: Vec<(String, MailboxId, Option<MailboxRole>)> = self
            .sidebar
            .iter()
            .filter_map(|it| match it {
                crate::state::SidebarItem::Mailbox { id, .. } => {
                    let role = self
                        .mailboxes
                        .iter()
                        .find(|m| &m.id == id)
                        .and_then(|m| m.role);
                    Some((self.mailbox_path(id), id.clone(), role))
                }
                _ => None,
            })
            .collect();
        if label {
            boxes.retain(|(_, _, role)| role.is_none());
        } else {
            boxes.retain(|(_, id, role)| {
                Some(id) != current.as_ref() && *role != Some(MailboxRole::Drafts)
            });
        }
        for (path, id, _) in boxes {
            let mut item = PickItem::new(path, PickValue::Mailbox(id.clone()));
            if label {
                item.checked = Some(members.contains(&id));
            }
            items.push(item);
        }
        let title = if label {
            "Labels".to_owned()
        } else {
            format!("Move {} to", plural(n, "conversation"))
        };
        let mut p = Picker::new(
            if label {
                PickerKind::Label
            } else {
                PickerKind::MoveTo
            },
            title,
            items,
        );
        p.can_create = true;
        p.target = Some(target);
        p.hint = if label {
            "Enter or Space toggles".into()
        } else {
            "type to filter".into()
        };
        self.overlays.push(Overlay::Picker(Box::new(p)));
    }

    /// The target of a menu action: the focused message in the reader,
    /// else the selection or the cursor row.
    pub(crate) fn action_target(&self) -> Option<(Target, usize)> {
        let selection_active = !self.list.selection.is_empty(self.list.total);
        if self.focus == Focus::Reader
            && !selection_active
            && let Some(id) = self.focused_email()
        {
            return Some((
                Target {
                    ids: TargetIds::Emails(vec![id]),
                    from_mailbox: self.target_mailbox(),
                },
                1,
            ));
        }
        self.target()
    }

    fn focused_message(&self) -> Option<crate::data::MessageSummary> {
        let conv = self.reader.conversation.as_ref()?;
        conv.messages
            .get(self.reader.focused)
            .or(conv.messages.last())
            .cloned()
    }

    /// Opens the links overlay for the focused message.
    pub(crate) fn open_links(&mut self, cmds: &mut Vec<Cmd>) {
        let Some(id) = self.focused_email() else {
            self.notify("Open a message first", ToastLevel::Info, false, cmds);
            return;
        };
        let links = self.bodies.links(&id);
        if links.is_empty() {
            self.notify("No links in this message", ToastLevel::Info, false, cmds);
            return;
        }
        let items = links
            .iter()
            .map(|l| {
                let text = crate::text::sanitize(&l.text).into_owned();
                let label = if text.is_empty() || text == l.url {
                    format!("[{}] {}", l.index, l.url)
                } else {
                    format!("[{}] {}  {text}", l.index, l.url)
                };
                PickItem::new(label, PickValue::Url(l.url.clone()))
            })
            .collect();
        let mut p = Picker::new(PickerKind::Links, "Links", items);
        p.hint = "Enter open  ^y copy".into();
        p.footer = vec!["Links open in the browser with the system handler.".into()];
        self.overlays.push(Overlay::Picker(Box::new(p)));
    }

    /// Opens the copy menu for the focused message.
    pub(crate) fn open_copy_menu(&mut self, cmds: &mut Vec<Cmd>) {
        let Some(m) = self.focused_message().or_else(|| {
            self.list
                .pages
                .row(self.list.cursor)
                .map(|r| crate::data::MessageSummary {
                    email_id: r.latest_email_id.clone(),
                    from: r.from.clone(),
                    to: Vec::new(),
                    cc: Vec::new(),
                    date: r.date,
                    preview: r.preview.clone(),
                    unread: r.unread,
                    flagged: r.flagged,
                    has_attachment: r.has_attachment,
                    mailboxes: Vec::new(),
                    reply_to: Vec::new(),
                    message_id: None,
                    references: Vec::new(),
                })
        }) else {
            self.notify("Nothing to copy", ToastLevel::Info, false, cmds);
            return;
        };
        let subject = self
            .reader
            .conversation
            .as_ref()
            .map(|c| c.subject.clone())
            .or_else(|| {
                self.list
                    .pages
                    .row(self.list.cursor)
                    .map(|r| r.subject.clone())
            })
            .unwrap_or_default();
        let mut items = vec![
            PickItem::new(
                format!("Sender address: {}", m.from.email),
                PickValue::Copy(m.from.email.clone()),
            ),
            PickItem::new(
                format!("Sender: {}", address::format(&m.from)),
                PickValue::Copy(address::format(&m.from)),
            ),
            PickItem::new(
                format!("Subject: {subject}"),
                PickValue::Copy(subject.clone()),
            ),
        ];
        let recipients: Vec<String> = m.to.iter().chain(&m.cc).map(address::format).collect();
        if !recipients.is_empty() {
            items.push(PickItem::new(
                format!("Recipients: {}", recipients.join(", ")),
                PickValue::Copy(recipients.join(", ")),
            ));
        }
        if let Some(mid) = &m.message_id {
            items.push(PickItem::new(
                format!("Message id: <{mid}>"),
                PickValue::Copy(format!("<{mid}>")),
            ));
        }
        for l in self.bodies.links(&m.email_id) {
            items.push(PickItem::new(
                format!("Link [{}]: {}", l.index, l.url),
                PickValue::Copy(l.url.clone()),
            ));
        }
        let mut p = Picker::new(PickerKind::Copy, "Copy", items);
        p.hint = "Enter copies".into();
        self.overlays.push(Overlay::Picker(Box::new(p)));
    }

    /// Opens the actions menu for the conversation or message.
    pub(crate) fn open_actions_menu(&mut self, cmds: &mut Vec<Cmd>) {
        if self.list.pages.row(self.list.cursor).is_none() && self.reader.conversation.is_none() {
            self.notify("Nothing here to act on", ToastLevel::Info, false, cmds);
            return;
        }
        let contexts = self.contexts();
        let keymap = &self.config.keymap;
        let items = [
            Action::Reply,
            Action::ReplyAll,
            Action::Forward,
            Action::Archive,
            Action::Trash,
            Action::ReportSpam,
            Action::MoveTo,
            Action::Label,
            Action::TogglePin,
            Action::MarkRead,
            Action::MarkUnread,
            Action::Mute,
            Action::NewRule,
            Action::MaskActions,
            Action::CopyMenu,
            Action::Links,
            Action::ToggleHtml,
            Action::OpenInBrowser,
        ]
        .into_iter()
        .map(|a| {
            PickItem::new(a.description(), PickValue::Action(a)).key(
                keymap
                    .keys_for(a, &contexts)
                    .map(|k| k.to_string())
                    .unwrap_or_default(),
            )
        })
        .collect();
        let mut p = Picker::new(PickerKind::Actions, "Actions", items);
        p.hint = "; or right click".into();
        self.overlays.push(Overlay::Picker(Box::new(p)));
    }

    /// Masked Email actions for the focused message (`@`).
    pub(crate) fn open_mask_menu(&mut self, data: &dyn MailData, cmds: &mut Vec<Cmd>) {
        let Some(m) = self.focused_message().or_else(|| {
            let row = self.list.pages.row(self.list.cursor)?;
            data.conversation(&row.thread_id)?.messages.last().cloned()
        }) else {
            self.notify("Open a message first", ToastLevel::Info, false, cmds);
            return;
        };
        let mask =
            m.to.iter()
                .chain(&m.cc)
                .find_map(|p| data.masked_for_address(&p.email));
        let mut items = Vec::new();
        match &mask {
            Some(mask) => {
                let state = match mask.state {
                    MaskedState::Enabled => "enabled",
                    MaskedState::Disabled => "disabled",
                    MaskedState::Pending => "pending",
                    MaskedState::Deleted => "deleted",
                };
                items.push(PickItem::new(
                    format!("Delivered to {} ({state})", mask.email),
                    PickValue::Info,
                ));
                if mask.state != MaskedState::Disabled {
                    items.push(PickItem::new(
                        "Disable this mask: future mail goes to Trash",
                        PickValue::MaskState {
                            id: mask.id.clone(),
                            state: MaskedState::Disabled,
                        },
                    ));
                }
                if mask.state != MaskedState::Enabled {
                    items.push(PickItem::new(
                        "Enable this mask",
                        PickValue::MaskState {
                            id: mask.id.clone(),
                            state: MaskedState::Enabled,
                        },
                    ));
                }
                if mask.state != MaskedState::Deleted {
                    items.push(PickItem::new(
                        "Delete this mask: future mail bounces",
                        PickValue::MaskState {
                            id: mask.id.clone(),
                            state: MaskedState::Deleted,
                        },
                    ));
                }
                items.push(PickItem::new(
                    "Copy the masked address",
                    PickValue::Copy(mask.email.clone()),
                ));
                items.push(PickItem::new(
                    "Show it in Masked Email",
                    PickValue::Masked(mask.id.clone()),
                ));
            }
            None => items.push(PickItem::new(
                "This message was not sent to a masked address",
                PickValue::Info,
            )),
        }
        if let Some(domain) = address::domain(&m.from.email) {
            items.push(PickItem::new(
                format!("New masked email for {domain}"),
                PickValue::NewMask(domain),
            ));
        }
        let mut p = Picker::new(PickerKind::Mask, "Masked Email", items);
        p.hint = "@".into();
        self.overlays.push(Overlay::Picker(Box::new(p)));
    }

    /// Keys while a picker is on top.
    pub(crate) fn picker_key(&mut self, data: &dyn MailData, key: Key, cmds: &mut Vec<Cmd>) {
        let Some(Overlay::Picker(p)) = self.overlays.last_mut() else {
            return;
        };
        match key.name {
            KeyName::Esc => {
                self.overlays.pop();
            }
            KeyName::Enter => self.choose(data, cmds),
            KeyName::Up => p.step(-1),
            KeyName::Down => p.step(1),
            KeyName::Char('p') if key.ctrl => p.step(-1),
            KeyName::Char('n') if key.ctrl => p.step(1),
            KeyName::PageUp => p.page(-10),
            KeyName::PageDown => p.page(10),
            KeyName::BackTab => p.step(-1),
            KeyName::Tab if p.kind == PickerKind::Palette => {
                // Complete the filter to the highlighted row.
                if let Some(label) = p.selected().map(|i| i.label.clone()) {
                    p.filter.set(label);
                    p.refilter();
                }
            }
            KeyName::Tab => p.step(1),
            KeyName::Char('y') if key.ctrl => {
                if let Some(PickValue::Url(u)) = p.selected().map(|i| i.value.clone()) {
                    self.overlays.pop();
                    cmds.push(Cmd::CopyToClipboard(u));
                    self.notify("Link copied", ToastLevel::Success, false, cmds);
                }
            }
            KeyName::Char(' ') if p.kind == PickerKind::Label && p.filter.is_empty() => {
                self.choose(data, cmds);
            }
            _ => {
                if p.filter.handle(&key) == InputResult::Changed {
                    p.cursor = 0;
                    p.refilter();
                }
            }
        }
    }

    /// Runs the highlighted picker row.
    pub(crate) fn choose(&mut self, data: &dyn MailData, cmds: &mut Vec<Cmd>) {
        let Some(Overlay::Picker(p)) = self.overlays.last() else {
            return;
        };
        let Some(item) = p.selected().cloned() else {
            return;
        };
        let kind = p.kind;
        let target = p.target.clone();
        let filter = p.filter.text().trim().to_owned();
        if kind != PickerKind::Label {
            self.overlays.pop();
        }
        match item.value {
            PickValue::Info => {}
            PickValue::Action(a) => {
                self.remember(a);
                self.apply(data, a, None, cmds);
            }
            PickValue::Mailbox(id) => match kind {
                PickerKind::MoveTo => {
                    if let Some(target) = target {
                        let n = self.target_count(&target);
                        let name = self.mailbox_path(&id);
                        cmds.push(Cmd::Mutate(Mutation::MoveTo {
                            target,
                            mailbox: id,
                        }));
                        self.list.selection.clear();
                        self.notify(
                            format!("Moved {} to {name}", plural(n, "conversation")),
                            ToastLevel::Success,
                            true,
                            cmds,
                        );
                    }
                }
                PickerKind::Label => {
                    let member = !item.checked.unwrap_or(false);
                    if let Some(Overlay::Picker(p)) = self.overlays.last_mut()
                        && let Some(it) = p.selected_mut()
                    {
                        it.checked = Some(member);
                    }
                    if let Some(target) = target {
                        let name = self.mailbox_path(&id);
                        cmds.push(Cmd::Mutate(Mutation::SetMailbox {
                            target,
                            mailbox: id,
                            member,
                        }));
                        let verb = if member {
                            "Added label"
                        } else {
                            "Removed label"
                        };
                        self.notify(format!("{verb} {name}"), ToastLevel::Success, true, cmds);
                    }
                }
                _ => {
                    self.screen = Screen::Mail;
                    self.results = None;
                    self.switch_mailbox(id);
                    self.sidebar_overlay = false;
                    self.focus = Focus::List;
                }
            },
            PickValue::CreateMailbox => {
                if kind == PickerKind::Label {
                    self.overlays.pop();
                }
                if filter.is_empty() {
                    return;
                }
                cmds.push(Cmd::CreateMailbox {
                    name: filter.clone(),
                    parent: None,
                    then_add: target,
                    then_move: kind == PickerKind::MoveTo,
                });
                self.list.selection.clear();
                self.notify(
                    format!("Created mailbox {filter}"),
                    ToastLevel::Success,
                    false,
                    cmds,
                );
            }
            PickValue::SavedSearch(q) => self.open_results_for(data, &q, cmds),
            PickValue::Contact(p) => self.open_compose(data, ComposeKind::New, vec![p], cmds),
            PickValue::Draft(id) => self.resume_draft(data, &id),
            PickValue::Masked(id) => self.open_masked(data, Some(&id)),
            PickValue::Rule(id) => self.open_rules(data, Some(&id)),
            PickValue::From { identity_id, from } => self.set_from(identity_id, from),
            PickValue::NewMask(for_domain) => {
                let draft_id = (kind == PickerKind::From)
                    .then(|| self.compose.as_ref().map(|c| c.draft_id.clone()))
                    .flatten();
                self.overlays.push(Overlay::Prompt(Prompt {
                    label: format!("Description for a new masked address for {for_domain}:"),
                    input: LineInput::default(),
                    on_enter: PromptAction::MaskedDescription {
                        for_domain,
                        draft_id,
                    },
                }));
            }
            PickValue::Url(u) => cmds.push(Cmd::OpenUrl(u)),
            PickValue::Copy(text) => {
                cmds.push(Cmd::CopyToClipboard(text));
                self.notify("Copied", ToastLevel::Success, false, cmds);
            }
            PickValue::MaskState { id, state } => self.set_mask_state(data, &id, state, cmds),
            PickValue::Quit(choice) => self.quit_choice(choice, cmds),
        }
    }

    /// Enables or disables a mask, or asks before deleting it.
    pub(crate) fn set_mask_state(
        &mut self,
        data: &dyn MailData,
        id: &str,
        state: MaskedState,
        cmds: &mut Vec<Cmd>,
    ) {
        let email = data
            .masked_emails()
            .into_iter()
            .find(|m| m.id == id)
            .map(|m| m.email)
            .unwrap_or_else(|| id.to_owned());
        match state {
            MaskedState::Deleted => self.overlays.push(Overlay::Confirm(Confirm {
                prompt: format!("Delete {email}? Mail to it will bounce."),
                on_yes: ConfirmAction::DeleteMasked { id: id.to_owned() },
            })),
            _ => {
                cmds.push(Cmd::MaskedSetState {
                    id: id.to_owned(),
                    state,
                });
                let text = match state {
                    MaskedState::Disabled => {
                        format!("Disabled {email}. Future mail goes to Trash.")
                    }
                    MaskedState::Enabled => format!("Enabled {email}"),
                    _ => format!("Changed {email}"),
                };
                self.notify(text, ToastLevel::Success, false, cmds);
            }
        }
    }

    /// Keys while a prompt is on top.
    pub(crate) fn prompt_key(&mut self, data: &dyn MailData, key: Key, cmds: &mut Vec<Cmd>) {
        let Some(Overlay::Prompt(p)) = self.overlays.last_mut() else {
            return;
        };
        match key.name {
            KeyName::Esc => {
                self.overlays.pop();
                self.notify("Cancelled", ToastLevel::Info, false, cmds);
            }
            KeyName::Enter => {
                let answer = p.input.text().trim().to_owned();
                let action = p.on_enter.clone();
                self.overlays.pop();
                self.prompt_answer(data, action, answer, cmds);
            }
            _ => {
                p.input.handle(&key);
            }
        }
    }

    fn prompt_answer(
        &mut self,
        data: &dyn MailData,
        action: PromptAction,
        answer: String,
        cmds: &mut Vec<Cmd>,
    ) {
        match action {
            PromptAction::AttachPath => self.attach_path(&answer, cmds),
            PromptAction::SaveSearchName { query } => {
                if answer.is_empty() {
                    return;
                }
                cmds.push(Cmd::SaveSearch {
                    name: answer.clone(),
                    query,
                });
                self.notify(
                    format!("Saved the search as {answer}"),
                    ToastLevel::Success,
                    false,
                    cmds,
                );
            }
            PromptAction::MaskedDomain => {
                let domain = answer
                    .trim_start_matches("https://")
                    .trim_start_matches("http://")
                    .split('/')
                    .next()
                    .unwrap_or("")
                    .to_lowercase();
                if !domain.contains('.') || domain.contains(' ') {
                    self.notify(
                        "That is not a site name such as shop.example.com",
                        ToastLevel::Error,
                        false,
                        cmds,
                    );
                    return;
                }
                let draft_id = (self.screen == Screen::Compose)
                    .then(|| self.compose.as_ref().map(|c| c.draft_id.clone()))
                    .flatten();
                self.overlays.push(Overlay::Prompt(Prompt {
                    label: format!("Description for {domain}:"),
                    input: LineInput::default(),
                    on_enter: PromptAction::MaskedDescription {
                        for_domain: domain,
                        draft_id,
                    },
                }));
            }
            PromptAction::MaskedDescription {
                for_domain,
                draft_id,
            } => {
                cmds.push(Cmd::MaskedCreate {
                    for_domain: for_domain.clone(),
                    description: answer,
                    draft_id,
                });
                self.notify(
                    format!("Making a masked address for {for_domain}"),
                    ToastLevel::Info,
                    false,
                    cmds,
                );
            }
            PromptAction::MaskedRename { id } => {
                cmds.push(Cmd::MaskedSetDescription {
                    id,
                    description: answer,
                });
                self.notify("Description changed", ToastLevel::Success, false, cmds);
            }
            PromptAction::RuleField(edit) => self.rule_field_answer(data, edit, answer, cmds),
            PromptAction::RulePaste => self.rule_paste_answer(data, &answer, cmds),
        }
    }

    /// Pasted text into the top prompt or picker filter. Returns true when
    /// one took it.
    pub(crate) fn overlay_paste(
        &mut self,
        data: &dyn MailData,
        text: &str,
        cmds: &mut Vec<Cmd>,
    ) -> bool {
        match self.overlays.last_mut() {
            Some(Overlay::Prompt(p)) if p.on_enter == PromptAction::RulePaste => {
                // The paste check needs the script with its line breaks,
                // so the whole paste is the answer.
                self.overlays.pop();
                self.rule_paste_answer(data, text, cmds);
                true
            }
            Some(Overlay::Prompt(p)) => {
                p.input.insert_str(text);
                true
            }
            Some(Overlay::Picker(p)) => {
                p.filter.insert_str(text);
                p.cursor = 0;
                p.refilter();
                true
            }
            Some(Overlay::Help(help)) => {
                help.filter.push_str(&crate::text::sanitize(text));
                help.scroll = 0;
                true
            }
            _ => false,
        }
    }

    /// A Masked Email address was made.
    pub(crate) fn masked_created(
        &mut self,
        draft_id: Option<crate::data::DraftId>,
        result: Result<crate::data::MaskedEmailView, String>,
        cmds: &mut Vec<Cmd>,
    ) {
        match result {
            Ok(m) => {
                if let Some(id) = draft_id
                    && let Some(c) = self.compose.as_mut().filter(|c| c.draft_id == id)
                {
                    c.from = Participant {
                        name: c.from.name.clone(),
                        email: m.email.clone(),
                    };
                    c.dirty = true;
                    c.last_edit = Some(self.clock.now());
                    self.notify(
                        format!("Sending from the new masked address {}", m.email),
                        ToastLevel::Success,
                        false,
                        cmds,
                    );
                } else {
                    self.notify(
                        format!("Made {} for {}", m.email, m.for_domain),
                        ToastLevel::Success,
                        false,
                        cmds,
                    );
                }
            }
            Err(e) => self.notify(
                format!(
                    "Could not make a masked address: {}",
                    crate::text::sanitize(&e)
                ),
                ToastLevel::Error,
                false,
                cmds,
            ),
        }
    }
}
