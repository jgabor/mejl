//! The settings screen: the settings mejl can change while running.
//!
//! Each row shows the value from the current [`UiConfig`]. Changing one
//! sends [`Cmd::SaveSetting`]; the shell writes the config file, builds a
//! new `UiConfig` (theme files and key overrides included) and sends it
//! back with `Msg::Config`, so the screen always shows what is in effect.
//!
//! [`UiConfig`]: crate::UiConfig
//! [`Cmd::SaveSetting`]: crate::Cmd::SaveSetting

use std::time::Duration;

use crate::event::Setting;
use crate::keymap::Preset;
use crate::state::{Density, UiConfig};
use crate::theme::Theme;

/// A settings row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// Theme.
    Theme,
    /// Key preset.
    Preset,
    /// List density.
    Density,
    /// Prefer HTML.
    PreferHtml,
    /// Undo send window.
    UndoSend,
    /// Remote images in the browser view.
    RemoteImages,
    /// Mark read delay.
    MarkRead,
    /// Mark read when archiving.
    MarkReadOnArchive,
}

/// Rows in screen order.
pub const ROWS: [Row; 8] = [
    Row::Theme,
    Row::Preset,
    Row::Density,
    Row::PreferHtml,
    Row::UndoSend,
    Row::RemoteImages,
    Row::MarkRead,
    Row::MarkReadOnArchive,
];

const UNDO_STEPS: [u64; 5] = [0, 5, 10, 20, 30];
const MARK_READ_STEPS: [u64; 4] = [0, 1, 3, 5];

impl Row {
    /// The row's label.
    pub fn label(self) -> &'static str {
        match self {
            Row::Theme => "Theme",
            Row::Preset => "Keys",
            Row::Density => "List density",
            Row::PreferHtml => "Prefer HTML",
            Row::UndoSend => "Undo send",
            Row::RemoteImages => "Remote images",
            Row::MarkRead => "Mark read",
            Row::MarkReadOnArchive => "Archive reads",
        }
    }

    /// A line about what the row does.
    pub fn help(self) -> &'static str {
        match self {
            Row::Theme => "terminal uses your terminal's colours; mono and high-contrast use none",
            Row::Preset => {
                "fastmail follows Fastmail's web shortcuts, vim follows aerc; Enter edits keys"
            }
            Row::Density => "compact drops date headings; relaxed shows a preview line",
            Row::PreferHtml => "show the HTML part when a message has both; V switches per message",
            Row::UndoSend => "how long a sent message waits so z can cancel it",
            Row::RemoteImages => "whether the browser view may load remote images",
            Row::MarkRead => "how long a conversation is open before it is marked read",
            Row::MarkReadOnArchive => {
                "archiving a conversation also marks it read; undo brings it back unread"
            }
        }
    }

    /// The value shown for the row.
    pub fn value(self, c: &UiConfig) -> String {
        let secs = |d: Duration| {
            if d.is_zero() {
                "off".to_owned()
            } else {
                format!("{} s", d.as_secs())
            }
        };
        match self {
            Row::Theme => c.theme.name().to_owned(),
            Row::Preset => c.keymap.name().to_owned(),
            Row::Density => c.density.name().to_owned(),
            Row::PreferHtml => on_off(c.prefer_html),
            Row::UndoSend => secs(c.undo_send),
            Row::RemoteImages => on_off(c.remote_images),
            Row::MarkReadOnArchive => on_off(c.mark_read_on_archive),
            Row::MarkRead => {
                if c.mark_read_delay.is_zero() {
                    "on open".to_owned()
                } else {
                    format!("after {} s", c.mark_read_delay.as_secs())
                }
            }
        }
    }

    /// The next (or previous) value, as a setting to save.
    pub fn step(self, c: &UiConfig, forward: bool) -> Setting {
        fn cycle<T: PartialEq + Copy>(all: &[T], cur: T, forward: bool) -> T {
            let i = all.iter().position(|x| *x == cur).unwrap_or(0);
            let n = all.len();
            if forward {
                all[(i + 1) % n]
            } else {
                all[(i + n - 1) % n]
            }
        }
        match self {
            Row::Theme => {
                let names = Theme::BUILT_IN;
                let cur = names.iter().position(|n| *n == c.theme.name()).unwrap_or(0);
                Setting::Theme(cycle(&names, names[cur], forward).to_owned())
            }
            Row::Preset => {
                let names = preset_names(c);
                let cur = names.iter().position(|n| n == c.keymap.name()).unwrap_or(0);
                let n = names.len();
                let next = if forward {
                    (cur + 1) % n
                } else {
                    (cur + n - 1) % n
                };
                Setting::Preset(names[next].clone())
            }
            Row::Density => Setting::Density(cycle(&Density::ALL, c.density, forward)),
            Row::PreferHtml => Setting::PreferHtml(!c.prefer_html),
            Row::UndoSend => Setting::UndoSend(Duration::from_secs(cycle(
                &UNDO_STEPS,
                c.undo_send.as_secs(),
                forward,
            ))),
            Row::RemoteImages => Setting::RemoteImages(!c.remote_images),
            Row::MarkReadOnArchive => Setting::MarkReadOnArchive(!c.mark_read_on_archive),
            Row::MarkRead => Setting::MarkReadDelay(Duration::from_secs(cycle(
                &MARK_READ_STEPS,
                c.mark_read_delay.as_secs(),
                forward,
            ))),
        }
    }
}

/// Every preset name the Keys row cycles through: the built-in ones, then
/// the user's own.
pub fn preset_names(c: &UiConfig) -> Vec<String> {
    Preset::ALL
        .iter()
        .map(|p| p.name().to_owned())
        .chain(c.presets.iter().map(|p| p.name.clone()))
        .collect()
}

fn on_off(b: bool) -> String {
    if b { "on" } else { "off" }.to_owned()
}

/// The settings screen.
#[derive(Clone, Debug, Default)]
pub struct SettingsScreen {
    pub(crate) cursor: usize,
    /// The row whose new value is on its way back from the shell.
    pub(crate) saving: Option<Row>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_and_steps() {
        let c = UiConfig::default();
        assert_eq!(Row::Theme.value(&c), "terminal");
        assert_eq!(Row::Theme.step(&c, true), Setting::Theme("mono".into()));
        assert_eq!(Row::Theme.step(&c, false), Setting::Theme("light".into()));
        assert_eq!(Row::Preset.step(&c, true), Setting::Preset("vim".into()));
        assert_eq!(
            Row::Density.step(&c, true),
            Setting::Density(Density::Relaxed)
        );
        assert_eq!(Row::UndoSend.value(&c), "10 s");
        assert_eq!(
            Row::UndoSend.step(&c, true),
            Setting::UndoSend(Duration::from_secs(20))
        );
        assert_eq!(Row::MarkRead.value(&c), "on open");
        assert_eq!(
            Row::MarkRead.step(&c, false),
            Setting::MarkReadDelay(Duration::from_secs(5))
        );
        assert_eq!(Row::MarkReadOnArchive.value(&c), "off");
        assert_eq!(
            Row::MarkReadOnArchive.step(&c, true),
            Setting::MarkReadOnArchive(true)
        );
        assert_eq!(Row::PreferHtml.step(&c, true), Setting::PreferHtml(true));
        assert_eq!(Row::RemoteImages.value(&c), "off");
        for r in ROWS {
            assert!(!r.label().is_empty() && !r.help().is_empty());
        }
    }
}
