//! `mejl keys --check`.
//!
//! The `[keys]` table is parsed with `mejl_ui::KeyOverrides::from_table`,
//! applied over the configured preset with `Keymap::with_overrides`, and
//! the result is checked with `Keymap::check`. Every problem is listed at
//! once: parse errors and unreachable bindings are errors, keys a legacy
//! terminal cannot send and shadowed bindings are warnings.

use mejl_ui::keymap::Severity;
use mejl_ui::{KeyOverrides, Keymap};

use crate::app::settings::preset;
use crate::config::Preset;

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

/// Checks the `[keys]` table against the default (Fastmail) preset.
pub fn check_keys(keys: &toml::Table) -> KeysReport {
    check_keys_with(keys, Preset::default())
}

/// Checks the `[keys]` table against a preset.
pub fn check_keys_with(keys: &toml::Table, base: Preset) -> KeysReport {
    let contexts = keys.len();
    let bindings: usize = keys
        .values()
        .map(|v| v.as_table().map_or(1, toml::Table::len))
        .sum();
    let preset_name = match base {
        Preset::Fastmail => "fastmail",
        Preset::Vim => "vim",
    };
    let summary = if contexts == 0 {
        format!("Your config has no [keys] overrides; checked the {preset_name} preset.")
    } else {
        format!(
            "{} in {}, checked against the {preset_name} preset.",
            plural(bindings, "key override"),
            plural(contexts, "context")
        )
    };
    let mut lines = vec![summary];
    let overrides = match KeyOverrides::from_table(keys) {
        Ok(o) => o,
        Err(errors) => {
            lines.extend(errors.iter().map(|e| format!("error: {e}")));
            return KeysReport {
                lines,
                has_errors: true,
            };
        }
    };
    let keymap = match Keymap::with_overrides(preset(base), &overrides) {
        Ok(k) => k,
        Err(errors) => {
            lines.extend(errors.iter().map(|e| e.to_string()));
            return KeysReport {
                lines,
                has_errors: true,
            };
        }
    };
    let found = keymap.check();
    let has_errors = found.iter().any(|c| c.severity == Severity::Error);
    if found.is_empty() {
        lines.push("No problems found.".to_owned());
    }
    lines.extend(found.iter().map(ToString::to_string));
    KeysReport { lines, has_errors }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> toml::Table {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn empty_table_checks_the_preset() {
        let r = check_keys(&toml::Table::new());
        assert!(!r.has_errors, "{:?}", r.lines);
        assert!(r.lines[0].contains("no [keys]"), "{:?}", r.lines);
    }

    #[test]
    fn counts_and_accepts_good_overrides() {
        let r = check_keys(&table(
            "[list]\nj = \"next\"\nk = \"prev\"\n[reader]\nq = \"back\"\n",
        ));
        assert!(
            r.lines[0].contains("3 key overrides in 2 contexts"),
            "{:?}",
            r.lines
        );
        assert!(!r.has_errors, "{:?}", r.lines);
    }

    #[test]
    fn unknown_actions_and_contexts_are_errors() {
        let r = check_keys(&table("[list]\nj = \"nxet\"\n[nowhere]\nx = \"next\"\n"));
        assert!(r.has_errors);
        let text = r.lines.join("\n");
        assert!(text.contains("nxet"), "{text}");
        assert!(text.contains("nowhere"), "{text}");
    }

    #[test]
    fn the_preset_is_honoured() {
        let r = check_keys_with(&toml::Table::new(), Preset::Vim);
        assert!(r.lines[0].contains("vim"), "{:?}", r.lines);
    }
}
