//! Dispatch for the Masked Email, rules, settings and key editor screens,
//! the settings screen itself, and mouse targets shared by screens.

use crate::action::Action;
use crate::data::MailData;
use crate::event::{Cmd, DataChange, ToastLevel};
use crate::rules::{RulesPane, form_rows};
use crate::settings::{ROWS, Row, SettingsScreen};
use crate::state::{AppState, Screen};

impl AppState {
    /// Runs an action of the current screen when it has its own meaning
    /// there. Returns false to let the mail screen's handling run.
    pub(crate) fn screen_action(
        &mut self,
        data: &dyn MailData,
        action: Action,
        count: Option<u32>,
        cmds: &mut Vec<Cmd>,
    ) -> bool {
        let _ = count;
        match self.screen {
            Screen::Masked => self.masked_action(data, action, cmds),
            Screen::Rules => self.rules_action(data, action, cmds),
            Screen::Settings => self.settings_action(action, cmds),
            Screen::Keys => self.keys_action(data, action, cmds),
            Screen::Mail => self.mail_mask_action(data, action, cmds),
            Screen::Compose | Screen::Search => false,
        }
    }

    /// Opens settings.
    pub(crate) fn open_settings(&mut self) {
        let mut cmds = Vec::new();
        self.leave_screen(&mut cmds);
        if self.settings.is_none() {
            self.settings = Some(SettingsScreen::default());
        }
        self.screen = Screen::Settings;
        self.pending.clear();
        self.toast = None;
    }

    fn settings_action(&mut self, action: Action, cmds: &mut Vec<Cmd>) -> bool {
        let Some(s) = self.settings.as_mut() else {
            return false;
        };
        match action {
            Action::Next => s.cursor = (s.cursor + 1).min(ROWS.len() - 1),
            Action::Prev => s.cursor = s.cursor.saturating_sub(1),
            Action::GoTop => s.cursor = 0,
            Action::GoBottom => s.cursor = ROWS.len() - 1,
            Action::SettingOpen if ROWS[s.cursor.min(ROWS.len() - 1)] == Row::Preset => {
                self.open_keys(cmds);
            }
            Action::SettingChange
            | Action::SettingChangeBack
            | Action::SettingOpen
            | Action::Open => {
                let row = ROWS[s.cursor.min(ROWS.len() - 1)];
                let setting = row.step(&self.config, action != Action::SettingChangeBack);
                s.saving = Some(row);
                cmds.push(Cmd::SaveSetting(setting));
                self.notify(
                    format!("Saving {}", row.label().to_lowercase()),
                    ToastLevel::Info,
                    false,
                    cmds,
                );
            }
            Action::Back => self.screen = Screen::Mail,
            _ => return false,
        }
        true
    }

    /// Wheel scrolling on a screen other than mail. Returns true when used.
    pub(crate) fn screen_wheel(&mut self, down: bool) -> bool {
        let delta: isize = if down { 1 } else { -1 };
        match self.screen {
            Screen::Search => {
                if let Some(s) = self.search.as_mut()
                    && !s.rows.is_empty()
                {
                    s.cursor =
                        (s.cursor as isize + delta).clamp(0, s.rows.len() as isize - 1) as usize;
                }
                true
            }
            Screen::Masked => {
                if let Some(s) = self.masked.as_mut() {
                    s.step(delta);
                }
                true
            }
            Screen::Rules => {
                if let Some(s) = self.rules.as_mut()
                    && !s.rules.is_empty()
                {
                    s.cursor =
                        (s.cursor as isize + delta).clamp(0, s.rules.len() as isize - 1) as usize;
                }
                true
            }
            Screen::Keys => {
                self.keys_wheel(down);
                true
            }
            Screen::Mail => false,
            Screen::Compose | Screen::Settings => true,
        }
    }

    /// A click on a form field of the current screen.
    pub(crate) fn click_field(&mut self, data: &dyn MailData, index: usize, cmds: &mut Vec<Cmd>) {
        let _ = data;
        match self.screen {
            Screen::Compose => {
                if let Some(c) = self.compose.as_mut() {
                    c.focus_index(index);
                    c.sync_cursor();
                }
            }
            Screen::Settings => {
                if let Some(s) = self.settings.as_mut() {
                    if s.cursor == index {
                        self.settings_action(Action::SettingOpen, cmds);
                    } else {
                        s.cursor = index.min(ROWS.len() - 1);
                    }
                }
            }
            Screen::Rules => {
                if let Some(s) = self.rules.as_mut() {
                    let rows = s.selected_rule().map(form_rows).unwrap_or_default().len();
                    s.pane = RulesPane::Form;
                    s.form_cursor = index.min(rows.saturating_sub(1));
                }
            }
            Screen::Keys => self.keys_click_section(index),
            Screen::Mail | Screen::Search | Screen::Masked => {}
        }
    }

    /// A click on a row of the current screen's list.
    pub(crate) fn click_row(
        &mut self,
        data: &dyn MailData,
        index: usize,
        double: bool,
        cmds: &mut Vec<Cmd>,
    ) {
        match self.screen {
            Screen::Search => {
                if let Some(s) = self.search.as_mut()
                    && index < s.rows.len()
                {
                    s.cursor = index;
                    s.results_focused = true;
                    if double {
                        self.apply(data, Action::Open, None, cmds);
                    }
                }
            }
            Screen::Masked => {
                if let Some(s) = self.masked.as_mut()
                    && index < s.shown.len()
                {
                    s.cursor = index;
                    s.filtering = false;
                    self.refresh_senders(data);
                    if double {
                        self.apply(data, Action::MaskedMessages, None, cmds);
                    }
                }
            }
            Screen::Rules => {
                if let Some(s) = self.rules.as_mut()
                    && index < s.rules.len()
                {
                    s.cursor = index;
                    s.form_cursor = 0;
                    s.pane = if double {
                        RulesPane::Form
                    } else {
                        RulesPane::List
                    };
                }
            }
            Screen::Keys => self.keys_click_row(data, index, cmds),
            Screen::Mail | Screen::Compose | Screen::Settings => {}
        }
    }

    /// Data changed: refresh screens that show it.
    pub(crate) fn screens_data_changed(&mut self, data: &dyn MailData, change: &DataChange) {
        if (change.masked || change.all)
            && let Some(s) = self.masked.as_mut()
        {
            s.reload(data.masked_emails(), self.clock.now());
            s.senders = None;
            if self.screen == Screen::Masked {
                self.refresh_senders(data);
            }
        }
        if (change.rules || change.all)
            && let Some(s) = self.rules.as_mut()
        {
            s.reload(&data.rules(), data.rule_renders(50));
        }
    }
}
