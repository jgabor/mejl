//! The key editor in `update`: opening it, its keys, the key capture and
//! the questions it asks, and sending each change to the shell.
//!
//! The editor never shows a change before the shell has written it: each
//! edit sends one [`KeysEdit`] (or a [`Setting::Preset`] for a switch),
//! the top line says ", saving", and `Msg::Config` or
//! `Msg::SettingRefused` ends it. Edits wait while one is on its way.

use crate::action::Action;
use crate::data::{MailData, MailboxId, MailboxRole};
use crate::event::{Cmd, KeysEdit, Setting, ToastLevel};
use crate::input::{InputResult, LineInput};
use crate::keymap::{Context, Key, KeyName, KeySeq};
use crate::keys_screen::{
    Capture, CaptureTarget, Check, Edit, KeysPick, KeysPrompt, KeysScreen, MacroSteps, Mode,
    PendingBind, Section, StepPick, bad_preset_name, bind, bind_macro, capitalised, check,
    check_macro, copy_name, macro_clash, moved_step, remove, reset, with_step, without_step,
};
use crate::macros::{FolderRef, MAX_NAME, Macro, SIMPLE_STEPS, Step, id_for};
use crate::picker::{PickItem, PickValue, Picker, PickerKind};
use crate::settings::preset_names;
use crate::state::{AppState, Confirm, ConfirmAction, Overlay, Prompt, PromptAction, Screen};

impl AppState {
    /// Opens the key editor on the active preset.
    pub(crate) fn open_keys(&mut self, cmds: &mut Vec<Cmd>) {
        let from = if self.screen == Screen::Settings {
            Screen::Settings
        } else {
            self.leave_screen(cmds);
            Screen::Mail
        };
        match self.keys.as_mut() {
            Some(s) => {
                s.from = from;
                s.steps = None;
                s.filtering = false;
                s.reload(&self.config);
            }
            None => self.keys = Some(Box::new(KeysScreen::new(&self.config, from))),
        }
        self.screen = Screen::Keys;
        self.pending.clear();
        self.toast = None;
    }

    /// Keys while the filter has focus. Returns false to let the keymap
    /// have the key.
    pub(crate) fn keys_key(&mut self, key: Key, cmds: &mut Vec<Cmd>) -> bool {
        let _ = cmds;
        let Some(s) = self.keys.as_mut() else {
            return false;
        };
        if !s.filtering || !self.pending.is_empty() {
            return false;
        }
        match key.name {
            KeyName::Esc => {
                s.filtering = false;
                s.filter.clear();
                s.refilter(&self.config);
            }
            KeyName::Enter => s.filtering = false,
            KeyName::Up => s.cursor = s.cursor.saturating_sub(1),
            KeyName::Down => s.cursor = (s.cursor + 1).min(s.len().saturating_sub(1)),
            _ => {
                if s.filter.handle(&key) == InputResult::Changed {
                    s.cursor = 0;
                    s.refilter(&self.config);
                }
            }
        }
        true
    }

    /// Pasted text goes into the filter.
    pub(crate) fn keys_paste(&mut self, text: &str) {
        if let Some(s) = self.keys.as_mut() {
            s.filtering = true;
            s.filter.insert_str(text);
            s.cursor = 0;
            s.refilter(&self.config);
        }
    }

    /// Runs an action on the key editor. Returns false for actions it
    /// leaves to the global table, such as help and the palette.
    pub(crate) fn keys_action(
        &mut self,
        data: &dyn MailData,
        action: Action,
        cmds: &mut Vec<Cmd>,
    ) -> bool {
        let Some(s) = self.keys.as_mut() else {
            return false;
        };
        if s.steps.is_some() {
            return self.steps_action(data, action, cmds);
        }
        let macros = s.section == Section::Macros;
        match action {
            Action::Next | Action::Prev | Action::PageDown | Action::PageUp => {
                let step: isize = match action {
                    Action::Next => 1,
                    Action::Prev => -1,
                    Action::PageDown => 10,
                    _ => -10,
                };
                s.cursor = (s.cursor as isize + step).clamp(0, s.len().saturating_sub(1) as isize)
                    as usize;
            }
            Action::GoTop => s.cursor = 0,
            Action::GoBottom => s.cursor = s.len().saturating_sub(1),
            Action::NextPane | Action::PrevPane => {
                let next = s.section.step(action == Action::NextPane);
                s.show(next, &self.config);
            }
            Action::Filter => s.filtering = true,
            Action::Back | Action::Quit => {
                if s.filter.is_empty() {
                    self.screen = s.from;
                } else {
                    s.filter.clear();
                    s.refilter(&self.config);
                }
            }
            Action::KeyReplace if macros => {
                if let Some(m) = s.focused_macro(&self.config) {
                    s.steps = Some(MacroSteps {
                        id: m.id,
                        label: m.label,
                        cursor: 0,
                    });
                }
            }
            Action::KeyAdd if macros => {
                if let Some(m) = s.focused_macro(&self.config) {
                    s.steps = Some(MacroSteps {
                        id: m.id,
                        label: m.label,
                        cursor: 0,
                    });
                    self.open_step_picker(cmds);
                }
            }
            Action::KeyRemove if macros => {
                if let Some(m) = s.focused_macro(&self.config) {
                    self.overlays.push(Overlay::Confirm(Confirm {
                        prompt: format!("Delete the macro {}?", m.label),
                        on_yes: ConfirmAction::DeleteMacro { id: m.id },
                    }));
                }
            }
            Action::MacroRename if macros => self.prompt_macro_rename(),
            Action::MacroBind if macros => self.capture_macro(cmds),
            Action::KeyReplace | Action::KeyAdd if !macros => {
                let mode = if action == Action::KeyReplace {
                    Mode::Replace
                } else {
                    Mode::Add
                };
                if !self.keys_read_only(cmds) && !self.keys_busy(cmds) {
                    self.capture_row(mode);
                }
            }
            Action::KeyRemove if !macros => {
                if !self.keys_read_only(cmds) && !self.keys_busy(cmds) {
                    self.remove_key(cmds);
                }
            }
            Action::KeyReset if !macros => {
                if !self.keys_read_only(cmds) && !self.keys_busy(cmds) {
                    self.reset_row(cmds);
                }
            }
            Action::PresetDuplicate => {
                let from = s.name.clone();
                if !self.keys_busy(cmds) {
                    let name = copy_name(&self.config, &from);
                    self.overlays.push(Overlay::Prompt(Prompt {
                        label: format!("Name for your copy of {from}:"),
                        input: LineInput::new(name),
                        on_enter: PromptAction::Keys(KeysPrompt::Duplicate),
                    }));
                }
            }
            Action::PresetRename => {
                if !self.keys_read_only(cmds) && !self.keys_busy(cmds) {
                    let from = self
                        .keys
                        .as_ref()
                        .map(|s| s.name.clone())
                        .unwrap_or_default();
                    self.overlays.push(Overlay::Prompt(Prompt {
                        label: format!("New name for {from}:"),
                        input: LineInput::new(from.clone()),
                        on_enter: PromptAction::Keys(KeysPrompt::RenamePreset { from }),
                    }));
                }
            }
            Action::PresetDelete => {
                if !self.keys_read_only(cmds) && !self.keys_busy(cmds) {
                    let (name, base) = self
                        .keys
                        .as_ref()
                        .map(|s| (s.name.clone(), s.base.preset_kind().name()))
                        .unwrap_or_default();
                    self.overlays.push(Overlay::Confirm(Confirm {
                        prompt: format!("Delete the preset {name}? {base} takes over."),
                        on_yes: ConfirmAction::DeletePreset { name },
                    }));
                }
            }
            Action::PresetSwitch => {
                if !self.keys_busy(cmds) {
                    self.open_preset_picker();
                }
            }
            Action::MacroNew => {
                if !self.keys_busy(cmds) {
                    self.overlays.push(Overlay::Prompt(Prompt {
                        label: "Name of the new macro:".into(),
                        input: LineInput::default(),
                        on_enter: PromptAction::Keys(KeysPrompt::NewMacro),
                    }));
                }
            }
            a if a.category() == crate::action::Category::Keys => {}
            _ => return false,
        }
        true
    }

    /// Actions in a macro's steps view.
    fn steps_action(&mut self, data: &dyn MailData, action: Action, cmds: &mut Vec<Cmd>) -> bool {
        let _ = data;
        let Some(s) = self.keys.as_mut() else {
            return false;
        };
        let Some(m) = s.focused_macro(&self.config) else {
            return false;
        };
        let Some(st) = s.steps.as_mut() else {
            return false;
        };
        let last = m.steps.len().saturating_sub(1);
        match action {
            Action::Next => st.cursor = (st.cursor + 1).min(last),
            Action::Prev => st.cursor = st.cursor.saturating_sub(1),
            Action::GoTop | Action::PageUp => st.cursor = 0,
            Action::GoBottom | Action::PageDown => st.cursor = last,
            Action::Back | Action::Quit => s.steps = None,
            Action::NextPane | Action::PrevPane => {
                let next = s.section.step(action == Action::NextPane);
                s.show(next, &self.config);
            }
            Action::KeyAdd => {
                if !self.keys_busy(cmds) {
                    self.open_step_picker(cmds);
                }
            }
            Action::KeyRemove => {
                let i = st.cursor;
                if !self.keys_busy(cmds) {
                    match without_step(&m, i) {
                        Ok(next) => {
                            let text = format!("Removed {} from {}", m.steps[i], m.label);
                            self.save_macros(vec![next], text, cmds);
                        }
                        Err(e) => self.notify(capitalised(&e), ToastLevel::Warning, false, cmds),
                    }
                }
            }
            Action::MacroStepUp | Action::MacroStepDown => {
                let up = action == Action::MacroStepUp;
                let i = st.cursor;
                if let Some(next) = moved_step(&m, i, up)
                    && !self.keys_busy(cmds)
                {
                    if let Some(st) = self.keys.as_mut().and_then(|s| s.steps.as_mut()) {
                        st.cursor = if up { i - 1 } else { i + 1 };
                    }
                    let text = format!("Moved {} {}", m.steps[i], if up { "up" } else { "down" });
                    self.save_macros(vec![next], text, cmds);
                }
            }
            Action::MacroRename => self.prompt_macro_rename(),
            Action::MacroBind => self.capture_macro(cmds),
            Action::Filter => {}
            a if a.category() == crate::action::Category::Keys => {}
            _ => return false,
        }
        true
    }

    /// Says so and returns true when the active preset is built in.
    fn keys_read_only(&mut self, cmds: &mut Vec<Cmd>) -> bool {
        let Some(s) = self.keys.as_ref() else {
            return true;
        };
        if s.custom {
            return false;
        }
        let key = self
            .config
            .keymap
            .keys_for(Action::PresetDuplicate, &[Context::Keys])
            .map_or_else(|| "d".to_owned(), |k| k.to_string());
        let text = format!("{} is built in. Press {key} to make your own copy.", s.name);
        self.notify(text, ToastLevel::Info, false, cmds);
        true
    }

    /// Says so and returns true while a change is on its way to the shell.
    fn keys_busy(&mut self, cmds: &mut Vec<Cmd>) -> bool {
        if self.keys.as_ref().is_some_and(|s| s.saving > 0) {
            self.notify(
                "Still saving the last change; try again in a moment",
                ToastLevel::Info,
                false,
                cmds,
            );
            return true;
        }
        false
    }

    fn capture_row(&mut self, mode: Mode) {
        let Some(s) = self.keys.as_ref() else {
            return;
        };
        let Some(row) = s.row() else {
            return;
        };
        let prompt = match mode {
            Mode::Replace => format!("Press the new key for {} (Esc cancels)", row.label()),
            Mode::Add => format!("Press a key to add to {} (Esc cancels)", row.label()),
        };
        let target = CaptureTarget::Row {
            section: s.section,
            action: row.action,
            mode,
        };
        self.overlays.push(Overlay::CaptureKey(Capture {
            prompt,
            keys: Vec::new(),
            target,
        }));
    }

    fn capture_macro(&mut self, cmds: &mut Vec<Cmd>) {
        let Some(m) = self
            .keys
            .as_ref()
            .and_then(|s| s.focused_macro(&self.config))
        else {
            return;
        };
        if !self.config.macros.iter().any(|x| x.id == m.id) {
            self.notify(
                "Add a step first; a macro is saved once it has one",
                ToastLevel::Info,
                false,
                cmds,
            );
            return;
        }
        if self.keys_busy(cmds) {
            return;
        }
        self.overlays.push(Overlay::CaptureKey(Capture {
            prompt: format!("Press a key for the macro {} (Esc cancels)", m.label),
            keys: Vec::new(),
            target: CaptureTarget::Macro { id: m.id },
        }));
    }

    /// A key while the capture overlay is on top. It reads raw keys, so
    /// any key but Esc can be bound, and waits while the keys so far start
    /// longer bindings.
    pub(crate) fn capture_key(&mut self, key: Key, cmds: &mut Vec<Cmd>) {
        if key == Key::ESC {
            self.overlays.pop();
            self.notify("Cancelled", ToastLevel::Info, false, cmds);
            return;
        }
        let Some(Overlay::CaptureKey(c)) = self.overlays.last_mut() else {
            return;
        };
        c.keys.push(key);
        let seq = KeySeq(c.keys.clone());
        let more = match &c.target {
            CaptureTarget::Row { section, .. } => self.keys.as_ref().is_some_and(|s| {
                section
                    .contexts()
                    .iter()
                    .any(|&ctx| s.map.is_prefix(ctx, &seq))
            }),
            CaptureTarget::Macro { .. } => [Context::List, Context::Reader, Context::Global]
                .iter()
                .any(|&ctx| self.config.keymap.is_prefix(ctx, &seq)),
        };
        if more {
            return;
        }
        let target = c.target.clone();
        self.overlays.pop();
        self.captured(target, seq, cmds);
    }

    fn captured(&mut self, target: CaptureTarget, keys: KeySeq, cmds: &mut Vec<Cmd>) {
        let result = match &target {
            CaptureTarget::Row {
                section,
                action,
                mode,
            } => match self.keys.as_ref() {
                Some(s) => match macro_clash(&self.config.keymap, *section, &keys) {
                    Some(why) => Check::Refused(why),
                    None => check(&s.map, *section, *action, *mode, &keys),
                },
                None => return,
            },
            CaptureTarget::Macro { id } => {
                check_macro(&self.config.keymap, &self.config.macros, id, &keys)
            }
        };
        match result {
            Check::Refused(why) => self.notify(
                format!("Not bound: {why}"),
                ToastLevel::Warning,
                false,
                cmds,
            ),
            Check::Same => self.notify(
                format!("{keys} is already the key"),
                ToastLevel::Info,
                false,
                cmds,
            ),
            Check::Free { warnings } => self.apply_capture(target, keys, false, warnings, cmds),
            Check::Ask {
                question,
                swap,
                warnings,
            } => self.ask_clash(target, keys, question, swap, warnings),
        }
    }

    /// Asks Swap, Replace or Cancel about a clash.
    fn ask_clash(
        &mut self,
        target: CaptureTarget,
        keys: KeySeq,
        question: String,
        swap: bool,
        warnings: Vec<String>,
    ) {
        let Some(s) = self.keys.as_mut() else {
            return;
        };
        let mut items = Vec::new();
        match &target {
            CaptureTarget::Row {
                section, action, ..
            } => {
                let victim = section
                    .contexts()
                    .iter()
                    .find_map(|&c| s.map.get(c, &keys))
                    .map(|b| capitalised(s.map.binding_label(b)));
                let mine = section
                    .contexts()
                    .iter()
                    .find_map(|&c| s.map.keys_of(c, (*action).into()).first().cloned());
                if swap && let (Some(v), Some(k)) = (&victim, &mine) {
                    items.push(PickItem::new(
                        format!("Swap: {v} gets {k}"),
                        PickValue::Keys(KeysPick::Swap),
                    ));
                }
                let replace = match victim {
                    Some(v) => format!("Replace: {v} loses {keys}"),
                    None => format!("Replace: bind {keys} here anyway"),
                };
                items.push(PickItem::new(replace, PickValue::Keys(KeysPick::Replace)));
            }
            CaptureTarget::Macro { .. } => items.push(PickItem::new(
                format!("Replace: the macro takes {keys}"),
                PickValue::Keys(KeysPick::Replace),
            )),
        }
        items.push(PickItem::new("Cancel", PickValue::Keys(KeysPick::Cancel)).key("esc"));
        s.pending = Some(PendingBind { target, keys });
        let mut p = Picker::new(PickerKind::Actions, question, items);
        p.footer = warnings;
        self.overlays.push(Overlay::Picker(Box::new(p)));
    }

    /// Binds a captured key, after the check or the clash question.
    fn apply_capture(
        &mut self,
        target: CaptureTarget,
        keys: KeySeq,
        swap: bool,
        warnings: Vec<String>,
        cmds: &mut Vec<Cmd>,
    ) {
        match target {
            CaptureTarget::Row {
                section,
                action,
                mode,
            } => {
                let Some(s) = self.keys.as_ref() else {
                    return;
                };
                match bind(&s.map, section, action, mode, &keys, swap) {
                    Ok(edit) => {
                        let now: Vec<String> = section
                            .contexts()
                            .first()
                            .map(|&c| edit.map.keys_of(c, action.into()))
                            .unwrap_or_default()
                            .iter()
                            .map(ToString::to_string)
                            .collect();
                        let mut text =
                            format!("{} is now {}", capitalised(action.label()), now.join(" "));
                        for w in &warnings {
                            text.push_str(". Note: ");
                            text.push_str(w);
                        }
                        let level = if warnings.is_empty() {
                            ToastLevel::Info
                        } else {
                            ToastLevel::Warning
                        };
                        self.save_preset(edit, text, level, cmds);
                    }
                    Err(why) => self.notify(
                        format!("Not bound: {why}"),
                        ToastLevel::Warning,
                        false,
                        cmds,
                    ),
                }
            }
            CaptureTarget::Macro { id } => {
                let Some(m) = self.config.macros.iter().find(|m| m.id == id).cloned() else {
                    return;
                };
                let saved = bind_macro(&self.config.macros, &m, &keys);
                let mut text = format!("{} runs on {keys}", m.label);
                for w in &warnings {
                    text.push_str(". Note: ");
                    text.push_str(w);
                }
                self.save_macros(saved, text, cmds);
            }
        }
    }

    /// Sends the preset's new overrides, a diff from its base.
    fn save_preset(&mut self, edit: Edit, text: String, level: ToastLevel, cmds: &mut Vec<Cmd>) {
        if edit.touched.is_empty() {
            self.notify("Nothing changed", ToastLevel::Info, false, cmds);
            return;
        }
        let Some(s) = self.keys.as_mut() else {
            return;
        };
        let overrides = s.overrides(&edit.map);
        let user = self.config.user_keys.entries();
        let drop_user = edit
            .touched
            .into_iter()
            .filter(|(c, k)| user.iter().any(|o| o.context == *c && o.keys == *k))
            .collect();
        cmds.push(Cmd::EditKeys(KeysEdit::SetPresetKeys {
            preset: s.name.clone(),
            overrides,
            drop_user,
        }));
        s.saving += 1;
        self.notify(text, level, false, cmds);
    }

    /// Sends macros to save.
    fn save_macros(&mut self, macros: Vec<Macro>, text: String, cmds: &mut Vec<Cmd>) {
        let Some(s) = self.keys.as_mut() else {
            return;
        };
        for m in macros {
            cmds.push(Cmd::EditKeys(KeysEdit::SaveMacro(Box::new(m))));
            s.saving += 1;
        }
        self.notify(text, ToastLevel::Info, false, cmds);
    }

    fn remove_key(&mut self, cmds: &mut Vec<Cmd>) {
        let Some(row) = self.keys.as_ref().and_then(|s| s.row().cloned()) else {
            return;
        };
        match row.keys.as_slice() {
            [] => self.notify(
                format!("{} has no keys", row.label()),
                ToastLevel::Info,
                false,
                cmds,
            ),
            [one] => {
                let one = one.clone();
                self.remove_row_key(&one, cmds);
            }
            many => {
                let items = many
                    .iter()
                    .map(|k| {
                        PickItem::new(
                            k.to_string(),
                            PickValue::Keys(KeysPick::RemoveKey(k.clone())),
                        )
                    })
                    .collect();
                let p = Picker::new(
                    PickerKind::Actions,
                    format!("Remove which key from {}?", row.label()),
                    items,
                );
                self.overlays.push(Overlay::Picker(Box::new(p)));
            }
        }
    }

    fn remove_row_key(&mut self, keys: &KeySeq, cmds: &mut Vec<Cmd>) {
        let Some(s) = self.keys.as_ref() else {
            return;
        };
        let Some(row) = s.row().cloned() else {
            return;
        };
        match remove(&s.map, s.section, row.action, keys) {
            Ok(edit) => {
                let text = format!("Removed {keys} from {}", row.label());
                self.save_preset(edit, text, ToastLevel::Info, cmds);
            }
            Err(why) => self.notify(capitalised(&why), ToastLevel::Warning, false, cmds),
        }
    }

    fn reset_row(&mut self, cmds: &mut Vec<Cmd>) {
        let Some(s) = self.keys.as_ref() else {
            return;
        };
        let Some(row) = s.row().cloned() else {
            return;
        };
        let base = s.base.preset_kind().name();
        match reset(&s.map, &s.base, s.section, row.action) {
            Ok(edit) if edit.touched.is_empty() => self.notify(
                format!("{} already has its {base} keys", row.label()),
                ToastLevel::Info,
                false,
                cmds,
            ),
            Ok(edit) => {
                // A macro that holds one of the restored keys keeps it.
                let held = edit
                    .touched
                    .iter()
                    .filter(|(c, k)| {
                        edit.map.get(*c, k) == Some(crate::keymap::Binding::Action(row.action))
                    })
                    .find_map(|(_, k)| macro_clash(&self.config.keymap, s.section, k));
                let (text, level) = match held {
                    Some(why) => (
                        format!("{} has its {base} keys again, but {why}", row.label()),
                        ToastLevel::Warning,
                    ),
                    None => (
                        format!("{} has its {base} keys again", row.label()),
                        ToastLevel::Info,
                    ),
                };
                self.save_preset(edit, text, level, cmds);
            }
            Err(why) => self.notify(
                format!("Not reset: {why}"),
                ToastLevel::Warning,
                false,
                cmds,
            ),
        }
    }

    fn open_preset_picker(&mut self) {
        let current = self.config.keymap.name().to_owned();
        let items = preset_names(&self.config)
            .into_iter()
            .map(|n| {
                let tag = if n == current {
                    "active"
                } else if crate::keymap::Preset::from_name(&n).is_some() {
                    "built in"
                } else {
                    ""
                };
                PickItem::new(n.clone(), PickValue::Keys(KeysPick::Preset(n))).tag(tag)
            })
            .collect();
        let p = Picker::new(PickerKind::Actions, "Use a preset", items);
        self.overlays.push(Overlay::Picker(Box::new(p)));
    }

    fn open_step_picker(&mut self, cmds: &mut Vec<Cmd>) {
        let Some(m) = self
            .keys
            .as_ref()
            .and_then(|s| s.focused_macro(&self.config))
        else {
            return;
        };
        if m.steps.len() >= crate::macros::MAX_STEPS {
            self.notify(
                format!("{} has the most steps a macro can have", m.label),
                ToastLevel::Info,
                false,
                cmds,
            );
            return;
        }
        let mut items: Vec<PickItem> = SIMPLE_STEPS
            .iter()
            .enumerate()
            .map(|(i, (_, step))| {
                PickItem::new(
                    step.to_string(),
                    PickValue::Keys(KeysPick::Step(StepPick::Simple(i))),
                )
            })
            .collect();
        for (label, pick) in [
            ("Move to a folder", StepPick::Move),
            ("Add a label", StepPick::AddLabel),
            ("Remove a label", StepPick::RemoveLabel),
        ] {
            items.push(PickItem::new(label, PickValue::Keys(KeysPick::Step(pick))).tag("folder"));
        }
        let p = Picker::new(
            PickerKind::Actions,
            format!("Add a step to {}", m.label),
            items,
        );
        self.overlays.push(Overlay::Picker(Box::new(p)));
    }

    /// The folder picker for a move or label step. It only picks a folder;
    /// no mail changes.
    fn open_folder_picker(&mut self, pick: StepPick) {
        let label = pick != StepPick::Move;
        let items: Vec<PickItem> = self
            .sidebar
            .iter()
            .filter_map(|it| match it {
                crate::state::SidebarItem::Mailbox { id, .. } => {
                    let role = self
                        .mailboxes
                        .iter()
                        .find(|m| &m.id == id)
                        .and_then(|m| m.role);
                    let keep = if label {
                        role.is_none()
                    } else {
                        role != Some(MailboxRole::Drafts)
                    };
                    keep.then(|| {
                        PickItem::new(self.mailbox_path(id), PickValue::Mailbox(id.clone()))
                    })
                }
                _ => None,
            })
            .collect();
        let title = match pick {
            StepPick::Move => "Move to which folder?",
            StepPick::AddLabel => "Add which label?",
            _ => "Remove which label?",
        };
        if let Some(s) = self.keys.as_mut() {
            s.folder_step = Some(pick);
        }
        let mut p = Picker::new(PickerKind::MacroFolder, title, items);
        p.hint = "type to filter".into();
        self.overlays.push(Overlay::Picker(Box::new(p)));
    }

    /// A folder was picked for a macro step.
    pub(crate) fn macro_folder_picked(&mut self, id: MailboxId, cmds: &mut Vec<Cmd>) {
        let Some(pick) = self.keys.as_mut().and_then(|s| s.folder_step.take()) else {
            return;
        };
        let folder = FolderRef {
            path: self.mailbox_path(&id),
            id: Some(id),
        };
        if let Some(step) = pick.step(Some(folder)) {
            self.add_step(step, cmds);
        }
    }

    fn add_step(&mut self, step: Step, cmds: &mut Vec<Cmd>) {
        let Some(m) = self
            .keys
            .as_ref()
            .and_then(|s| s.focused_macro(&self.config))
        else {
            return;
        };
        match with_step(&m, step.clone()) {
            Ok(next) => {
                if let Some(st) = self.keys.as_mut().and_then(|s| s.steps.as_mut()) {
                    st.cursor = next.steps.len() - 1;
                }
                let text = format!("Added {step} to {}", m.label);
                self.save_macros(vec![next], text, cmds);
            }
            Err(e) => self.notify(
                format!("{}: {e}", m.label),
                ToastLevel::Warning,
                false,
                cmds,
            ),
        }
    }

    fn prompt_macro_rename(&mut self) {
        let Some(m) = self
            .keys
            .as_ref()
            .and_then(|s| s.focused_macro(&self.config))
        else {
            return;
        };
        self.overlays.push(Overlay::Prompt(Prompt {
            label: format!("New name for {}:", m.label),
            input: LineInput::new(m.label),
            on_enter: PromptAction::Keys(KeysPrompt::RenameMacro { id: m.id }),
        }));
    }

    /// A choice in one of the key editor's pickers.
    pub(crate) fn keys_pick(&mut self, data: &dyn MailData, pick: KeysPick, cmds: &mut Vec<Cmd>) {
        let _ = data;
        match pick {
            KeysPick::Swap | KeysPick::Replace => {
                let Some(p) = self.keys.as_mut().and_then(|s| s.pending.take()) else {
                    return;
                };
                if self.keys_busy(cmds) {
                    return;
                }
                let warnings = match &p.target {
                    CaptureTarget::Row {
                        section,
                        action,
                        mode,
                    } => match self.keys.as_ref() {
                        Some(s) => match check(&s.map, *section, *action, *mode, &p.keys) {
                            Check::Ask { warnings, .. } | Check::Free { warnings } => warnings,
                            _ => Vec::new(),
                        },
                        None => return,
                    },
                    CaptureTarget::Macro { .. } => Vec::new(),
                };
                self.apply_capture(p.target, p.keys, pick == KeysPick::Swap, warnings, cmds);
            }
            KeysPick::Cancel => {
                if let Some(s) = self.keys.as_mut() {
                    s.pending = None;
                }
                self.notify("Cancelled", ToastLevel::Info, false, cmds);
            }
            KeysPick::RemoveKey(k) => self.remove_row_key(&k, cmds),
            KeysPick::Preset(name) => {
                if name == self.config.keymap.name() {
                    return;
                }
                cmds.push(Cmd::SaveSetting(Setting::Preset(name.clone())));
                if let Some(s) = self.keys.as_mut() {
                    s.saving += 1;
                }
                self.notify(
                    format!("Switching to {name}"),
                    ToastLevel::Info,
                    false,
                    cmds,
                );
            }
            KeysPick::Step(StepPick::Simple(i)) => {
                if let Some(step) = StepPick::Simple(i).step(None) {
                    self.add_step(step, cmds);
                }
            }
            KeysPick::Step(folder) => self.open_folder_picker(folder),
        }
    }

    /// The answer to one of the key editor's prompts.
    pub(crate) fn keys_prompt_answer(
        &mut self,
        prompt: KeysPrompt,
        answer: String,
        cmds: &mut Vec<Cmd>,
    ) {
        if answer.is_empty() {
            return;
        }
        match prompt {
            KeysPrompt::Duplicate => {
                if let Some(why) = bad_preset_name(&self.config, &answer) {
                    self.notify(capitalised(&why), ToastLevel::Warning, false, cmds);
                    return;
                }
                let Some(s) = self.keys.as_mut() else {
                    return;
                };
                let overrides = self
                    .config
                    .presets
                    .iter()
                    .find(|p| p.name == s.name)
                    .map(|p| p.overrides.clone())
                    .unwrap_or_default();
                cmds.push(Cmd::EditKeys(KeysEdit::NewPreset {
                    name: answer.clone(),
                    base: s.base.preset_kind(),
                    overrides,
                    take_user: true,
                }));
                s.saving += 1;
                self.notify(
                    format!("Made the preset {answer}; it is the active one now"),
                    ToastLevel::Info,
                    false,
                    cmds,
                );
            }
            KeysPrompt::RenamePreset { from } => {
                if answer == from {
                    return;
                }
                if let Some(why) = bad_preset_name(&self.config, &answer) {
                    self.notify(capitalised(&why), ToastLevel::Warning, false, cmds);
                    return;
                }
                cmds.push(Cmd::EditKeys(KeysEdit::RenamePreset {
                    from: from.clone(),
                    to: answer.clone(),
                }));
                if let Some(s) = self.keys.as_mut() {
                    s.saving += 1;
                }
                self.notify(
                    format!("Renamed {from} to {answer}"),
                    ToastLevel::Info,
                    false,
                    cmds,
                );
            }
            KeysPrompt::NewMacro => {
                if answer.chars().count() > MAX_NAME {
                    self.notify(
                        format!("A macro name has at most {MAX_NAME} characters"),
                        ToastLevel::Warning,
                        false,
                        cmds,
                    );
                    return;
                }
                let taken: Vec<&str> = self.config.macros.iter().map(|m| m.id.as_str()).collect();
                let id = id_for(&answer, &taken);
                let Some(s) = self.keys.as_mut() else {
                    return;
                };
                s.show(Section::Macros, &self.config);
                s.steps = Some(MacroSteps {
                    id,
                    label: answer,
                    cursor: 0,
                });
                self.open_step_picker(cmds);
            }
            KeysPrompt::RenameMacro { id } => {
                let saved = self.config.macros.iter().find(|m| m.id == id).cloned();
                match saved {
                    Some(m) => {
                        let next = Macro {
                            label: answer.clone(),
                            ..m.clone()
                        };
                        match next.validate() {
                            Ok(()) => {
                                let text = format!("Renamed {} to {answer}", m.label);
                                self.save_macros(vec![next], text, cmds);
                            }
                            Err(e) => self.notify(
                                format!("Not renamed: the macro {e}"),
                                ToastLevel::Warning,
                                false,
                                cmds,
                            ),
                        }
                    }
                    None => {
                        if let Some(st) = self.keys.as_mut().and_then(|s| s.steps.as_mut())
                            && st.id == id
                        {
                            st.label = answer;
                        }
                    }
                }
            }
        }
    }

    /// Deletes a custom preset, after the confirm.
    pub(crate) fn delete_preset(&mut self, name: String, cmds: &mut Vec<Cmd>) {
        cmds.push(Cmd::EditKeys(KeysEdit::DeletePreset { name: name.clone() }));
        if let Some(s) = self.keys.as_mut() {
            s.saving += 1;
        }
        self.notify(
            format!("Deleted the preset {name}"),
            ToastLevel::Success,
            false,
            cmds,
        );
    }

    /// Deletes a macro, after the confirm.
    pub(crate) fn delete_macro(&mut self, id: String, cmds: &mut Vec<Cmd>) {
        let label = self
            .config
            .macros
            .iter()
            .find(|m| m.id == id)
            .map_or_else(|| id.clone(), |m| m.label.clone());
        cmds.push(Cmd::EditKeys(KeysEdit::DeleteMacro { id }));
        if let Some(s) = self.keys.as_mut() {
            s.saving += 1;
        }
        self.notify(
            format!("Deleted the macro {label}"),
            ToastLevel::Success,
            false,
            cmds,
        );
    }

    /// A new config arrived, or a change was refused: one change less is
    /// on its way, and the editor shows what is in effect.
    pub(crate) fn keys_config_changed(&mut self) {
        if let Some(s) = self.keys.as_mut() {
            s.saving = s.saving.saturating_sub(1);
            s.pending = None;
            s.reload(&self.config);
        }
    }

    /// A click on a row of the key editor: the first focuses it, the
    /// second acts as Enter.
    pub(crate) fn keys_click_row(
        &mut self,
        data: &dyn MailData,
        index: usize,
        cmds: &mut Vec<Cmd>,
    ) {
        let Some(s) = self.keys.as_mut() else {
            return;
        };
        if let Some(st) = s.steps.as_mut() {
            // The hit map can be a frame old, from the list before the
            // steps opened.
            let n = self
                .config
                .macros
                .iter()
                .find(|m| m.id == st.id)
                .map_or(0, |m| m.steps.len());
            if index < n {
                st.cursor = index;
            }
            return;
        }
        if index >= s.len() {
            return;
        }
        if s.cursor == index {
            self.keys_action(data, Action::KeyReplace, cmds);
        } else {
            s.cursor = index;
        }
    }

    /// A click on a section name.
    pub(crate) fn keys_click_section(&mut self, index: usize) {
        if let Some(s) = self.keys.as_mut()
            && let Some(section) = Section::ALL.get(index)
        {
            s.show(*section, &self.config);
        }
    }

    /// Wheel scrolling on the key editor.
    pub(crate) fn keys_wheel(&mut self, down: bool) {
        let Some(s) = self.keys.as_mut() else {
            return;
        };
        if s.steps.is_some() {
            return;
        }
        let last = s.len().saturating_sub(1);
        s.cursor = if down {
            (s.cursor + 1).min(last)
        } else {
            s.cursor.saturating_sub(1)
        };
    }
}
