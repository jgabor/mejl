//! `mejl keys --check`.
//!
//! The key setup is parsed the way the app does it
//! ([`key_setup`](crate::app::settings::key_setup)): the top-level `[keys]`
//! table, every custom preset under `[presets]` and the macros. Each preset
//! is then built with `Keymap::build` and checked with `Keymap::check`.
//! Every problem is listed at once: parse errors and unreachable bindings
//! are errors, keys a legacy terminal cannot send, shadowed bindings and
//! macro keys that were skipped are warnings.

use mejl_ui::keymap::Severity;

use crate::app::settings::{key_setup, keymap_for};
use crate::config::{Config, Preset};

/// The result of a keymap check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeysReport {
    /// Lines to print.
    pub lines: Vec<String>,
    /// Whether any problem should make the command fail.
    pub has_errors: bool,
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

/// Checks the key setup of a config: the active preset first, then every
/// other custom preset.
pub fn check_keys(config: &Config) -> KeysReport {
    let overrides: usize = config
        .keys
        .values()
        .map(|v| v.as_table().map_or(1, toml::Table::len))
        .sum();
    let active = config.ui.preset.as_str();
    let mut summary = format!("Checked the {active} preset");
    if let Some(p) = config.presets.get(active) {
        summary.push_str(&format!(" (based on {})", p.base.name()));
    }
    let mut parts = Vec::new();
    if overrides > 0 {
        parts.push(plural(overrides, "[keys] override"));
    }
    if !config.presets.is_empty() {
        parts.push(plural(config.presets.len(), "custom preset"));
    }
    if !config.macros.is_empty() {
        parts.push(plural(config.macros.len(), "macro"));
    }
    if parts.is_empty() {
        summary.push_str("; your config has no [keys] overrides, custom presets or macros.");
    } else {
        summary.push_str(&format!(" with {}.", parts.join(", ")));
    }
    let mut lines = vec![summary];
    let setup = match key_setup(config) {
        Ok(s) => s,
        Err(errors) => {
            lines.extend(errors.iter().map(|e| format!("error: {e}")));
            return KeysReport {
                lines,
                has_errors: true,
            };
        }
    };
    let mut names = vec![active.to_owned()];
    names.extend(config.presets.keys().filter(|n| *n != active).cloned());
    let mut has_errors = false;
    let mut found_any = false;
    for (i, name) in names.iter().enumerate() {
        let label = |line: String| {
            if i == 0 {
                line
            } else {
                format!("preset {name}: {line}")
            }
        };
        if i == 0 && Preset::from_name(name).is_none() && !config.presets.contains_key(name) {
            lines.push(format!("error: there is no key preset called {name:?}"));
            has_errors = true;
            continue;
        }
        match keymap_for(name, &setup) {
            Ok(keymap) => {
                let mut found = keymap.warnings().to_vec();
                for c in keymap.check() {
                    if !found.contains(&c) {
                        found.push(c);
                    }
                }
                has_errors |= found.iter().any(|c| c.severity == Severity::Error);
                found_any |= !found.is_empty();
                lines.extend(found.iter().map(|c| label(c.to_string())));
            }
            Err(errors) => {
                has_errors = true;
                found_any = true;
                lines.extend(errors.into_iter().map(label));
            }
        }
    }
    if !found_any {
        lines.push("No problems found.".to_owned());
    }
    KeysReport { lines, has_errors }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> Config {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn an_empty_config_checks_the_preset() {
        let r = check_keys(&Config::default());
        assert!(!r.has_errors, "{:?}", r.lines);
        assert!(r.lines[0].contains("no [keys]"), "{:?}", r.lines);
        assert!(r.lines.iter().any(|l| l == "No problems found."));
    }

    #[test]
    fn counts_and_accepts_good_overrides() {
        let r = check_keys(&config(
            "[keys.list]\nj = \"next\"\nk = \"prev\"\n[keys.reader]\nq = \"back\"\n",
        ));
        assert!(r.lines[0].contains("3 [keys] overrides"), "{:?}", r.lines);
        assert!(!r.has_errors, "{:?}", r.lines);
    }

    #[test]
    fn unknown_actions_and_contexts_are_errors() {
        let r = check_keys(&config(
            "[keys.list]\nj = \"nxet\"\n[keys.nowhere]\nx = \"next\"\n",
        ));
        assert!(r.has_errors);
        let text = r.lines.join("\n");
        assert!(text.contains("nxet"), "{text}");
        assert!(text.contains("nowhere"), "{text}");
    }

    #[test]
    fn the_preset_is_honoured() {
        let r = check_keys(&config("[ui]\npreset = \"vim\"\n"));
        assert!(r.lines[0].contains("vim"), "{:?}", r.lines);
    }

    #[test]
    fn custom_presets_and_macros_are_checked() {
        let r = check_keys(&config(
            "[ui]\npreset = \"mine\"\n\
             [presets.mine]\nbase = \"vim\"\n[presets.mine.keys.list]\ne = \"archive\"\n\
             [presets.other.keys.list]\n\".\" = \"archive\"\n\
             [macros.tidy]\nkeys = [\"r\"]\nsteps = [\"mark-read\"]\n",
        ));
        let text = r.lines.join("\n");
        assert!(r.lines[0].contains("mine preset (based on vim)"), "{text}");
        assert!(r.lines[0].contains("2 custom presets, 1 macro"), "{text}");
        assert!(text.contains("macro tidy is not bound"), "{text}");
        assert!(text.contains("preset other:"), "{text}");
        assert!(r.has_errors, "the other preset's `.` hides `. r`: {text}");
        let r = check_keys(&config("[macros.tidy]\nsteps = [\"mark-raed\"]\n"));
        assert!(r.has_errors);
        assert!(r.lines.join("\n").contains("did you mean"), "{:?}", r.lines);
    }
}
