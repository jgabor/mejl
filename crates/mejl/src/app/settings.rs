//! Building the UI's settings from the config file, and writing changes
//! from the settings screen back to it.
//!
//! Changes go into the config file itself, edited in place with toml_edit,
//! so the user's comments, ordering and other settings survive. The new
//! text is checked by parsing it the way startup does before it replaces
//! the old file, and the replace is atomic (a temporary file in the same
//! directory, then a rename). When the file cannot be written, the change
//! still applies until mejl quits and the toast says so.

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::tz::TimeZone;
use mejl_ui::{self as ui, KeyOverrides, Keymap, Setting, Theme, UiConfig};
use toml_edit::{DocumentMut, Item, value};

use crate::config::{self, Config, Density, Preset, RemoteImages};

/// The settings the UI runs with and where changes are saved.
#[derive(Debug, Clone)]
pub struct Settings {
    path: PathBuf,
    config: Config,
    no_color: bool,
    time_zone: TimeZone,
}

/// The UI's preset for a config preset.
pub fn preset(p: Preset) -> ui::Preset {
    match p {
        Preset::Fastmail => ui::Preset::Fastmail,
        Preset::Vim => ui::Preset::Vim,
    }
}

fn density(d: Density) -> ui::Density {
    match d {
        Density::Compact => ui::Density::Compact,
        Density::Comfortable => ui::Density::Comfortable,
        Density::Relaxed => ui::Density::Relaxed,
    }
}

/// The keymap for a preset and the `[keys]` table. Errors are every
/// problem found, as `mejl keys --check` prints them.
pub fn keymap(p: Preset, keys: &toml::Table) -> Result<Keymap, Vec<String>> {
    let overrides = KeyOverrides::from_table(keys)
        .map_err(|errors| errors.iter().map(ToString::to_string).collect::<Vec<_>>())?;
    Keymap::with_overrides(preset(p), &overrides)
        .map_err(|errors| errors.iter().map(ToString::to_string).collect())
}

/// The theme for a name. `NO_COLOR` (or `no_color`) always gives mono.
/// An unknown name falls back to the default theme with a warning.
pub fn theme(name: &str, no_color: bool) -> (Theme, Option<String>) {
    if no_color {
        return match Theme::named("mono", true) {
            Ok(t) => (t, None),
            Err(e) => (Theme::default(), Some(e.to_string())),
        };
    }
    match Theme::named(name, false) {
        Ok(t) => (t, None),
        Err(e) => (
            Theme::default(),
            Some(format!("theme {name:?}: {e}; using the default theme")),
        ),
    }
}

impl Settings {
    /// Settings from a loaded config. `path` is where changes are saved.
    pub fn new(path: PathBuf, config: Config, no_color: bool, time_zone: TimeZone) -> Settings {
        Settings {
            path,
            config,
            no_color,
            time_zone,
        }
    }

    /// The config as loaded and changed since.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The file changes are saved to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The UI's settings. Fails when the key overrides have errors; a bad
    /// theme name only warns.
    pub fn ui_config(&self) -> Result<(UiConfig, Vec<String>), Vec<String>> {
        let ui = &self.config.ui;
        let keymap = keymap(ui.preset, &self.config.keys)?;
        let mut warnings: Vec<String> = keymap.warnings().iter().map(ToString::to_string).collect();
        let (theme, warning) = theme(&ui.theme, self.no_color);
        warnings.extend(warning);
        let config = UiConfig {
            keymap,
            theme,
            time_zone: self.time_zone.clone(),
            prefetch: usize::try_from(self.config.sync.prefetch).unwrap_or(3),
            density: density(ui.density),
            prefer_html: ui.prefer_html,
            undo_send: Duration::from_secs(u64::from(ui.undo_send_seconds)),
            remote_images: ui.remote_images == RemoteImages::On,
            mark_read_on_archive: ui.mark_read_on_archive,
            mark_read_delay: Duration::from_millis(u64::from(ui.mark_read_delay_ms)),
            ..UiConfig::default()
        };
        Ok((config, warnings))
    }

    /// Applies a setting: in memory at once, then to the config file. The
    /// returned config is the whole rebuilt UI config. The error, if any,
    /// says what could not be saved; the change applies anyway.
    pub fn apply(&mut self, setting: &Setting) -> (Option<UiConfig>, Option<String>) {
        let mut next = self.config.clone();
        set_in_config(&mut next, setting);
        let candidate = Settings {
            config: next,
            ..self.clone()
        };
        let ui = match candidate.ui_config() {
            Ok((ui, _)) => ui,
            Err(errors) => {
                return (
                    None,
                    Some(format!(
                        "that setting does not work with your key overrides: {}",
                        errors.join("; ")
                    )),
                );
            }
        };
        let saved = self.write(setting);
        self.config = candidate.config;
        (Some(ui), saved.err())
    }

    fn write(&self, setting: &Setting) -> Result<(), String> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("cannot read {}: {e}", self.path.display())),
        };
        let updated = edit_text(&text, setting)?;
        config::parse(&updated).map_err(|e| {
            format!(
                "{} would not load after the change ({e}); edit it by hand",
                self.path.display()
            )
        })?;
        write_atomic(&self.path, &updated)
            .map_err(|e| format!("cannot write {}: {e}", self.path.display()))
    }
}

fn set_in_config(c: &mut Config, setting: &Setting) {
    let ui = &mut c.ui;
    match setting {
        Setting::Theme(t) => ui.theme = t.clone(),
        Setting::Preset(p) => {
            ui.preset = match p {
                ui::Preset::Vim => Preset::Vim,
                _ => Preset::Fastmail,
            }
        }
        Setting::Density(d) => {
            ui.density = match d {
                ui::Density::Compact => Density::Compact,
                ui::Density::Comfortable => Density::Comfortable,
                ui::Density::Relaxed => Density::Relaxed,
            }
        }
        Setting::PreferHtml(b) => ui.prefer_html = *b,
        Setting::UndoSend(d) => ui.undo_send_seconds = undo_seconds(*d),
        Setting::RemoteImages(b) => {
            ui.remote_images = if *b {
                RemoteImages::On
            } else {
                RemoteImages::Off
            }
        }
        Setting::MarkReadDelay(d) => ui.mark_read_delay_ms = delay_ms(*d),
        Setting::MarkReadOnArchive(b) => ui.mark_read_on_archive = *b,
    }
}

fn undo_seconds(d: Duration) -> u32 {
    u32::try_from(d.as_secs().min(300)).unwrap_or(300)
}

fn delay_ms(d: Duration) -> u32 {
    u32::try_from(d.as_millis().min(60_000)).unwrap_or(60_000)
}

/// The `[ui]` key and TOML value for a setting.
fn key_value(setting: &Setting) -> (&'static str, Item) {
    match setting {
        Setting::Theme(t) => ("theme", value(t.as_str())),
        Setting::Preset(p) => (
            "preset",
            value(match p {
                ui::Preset::Vim => "vim",
                _ => "fastmail",
            }),
        ),
        Setting::Density(d) => ("density", value(d.name())),
        Setting::PreferHtml(b) => ("prefer_html", value(*b)),
        Setting::UndoSend(d) => ("undo_send_seconds", value(i64::from(undo_seconds(*d)))),
        Setting::RemoteImages(b) => ("remote_images", value(if *b { "on" } else { "off" })),
        Setting::MarkReadDelay(d) => ("mark_read_delay_ms", value(i64::from(delay_ms(*d)))),
        Setting::MarkReadOnArchive(b) => ("mark_read_on_archive", value(*b)),
    }
}

/// The config text with one `[ui]` setting changed, everything else as it
/// was, comments included.
pub fn edit_text(text: &str, setting: &Setting) -> Result<String, String> {
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("the config file is not valid TOML: {e}"))?;
    if !doc.contains_key("ui") {
        doc.insert("ui", toml_edit::table());
    }
    let (key, item) = key_value(setting);
    let ui = doc
        .get_mut("ui")
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| "[ui] in the config file is not a table".to_owned())?;
    match ui.get_mut(key) {
        // Keep the comment and spacing around an existing value.
        Some(Item::Value(old)) => {
            if let Item::Value(mut new) = item {
                *new.decor_mut() = old.decor().clone();
                *old = new;
            }
        }
        _ => {
            ui.insert(key, item);
        }
    }
    Ok(doc.to_string())
}

/// Replaces a file atomically, creating its directory when needed and
/// keeping its permissions.
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    if !dir.as_os_str().is_empty() && !dir.exists() {
        crate::paths::create_private_dir(dir)?;
    }
    let mut tmp = tempfile::Builder::new()
        .prefix(".mejl-config-")
        .tempfile_in(dir)?;
    std::io::Write::write_all(&mut tmp, text.as_bytes())?;
    tmp.as_file().sync_all()?;
    if let Ok(meta) = std::fs::metadata(path) {
        std::fs::set_permissions(tmp.path(), meta.permissions())?;
    }
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_keep_comments_and_other_settings() {
        let text = "# my mejl\n[ui]\n# colours\ntheme = \"dark\" # was light\nmouse = false\n\n[sync]\npush = false\n";
        let out = edit_text(text, &Setting::Theme("mono".into())).unwrap();
        assert!(out.contains("# my mejl"), "{out}");
        assert!(out.contains("# colours"), "{out}");
        assert!(out.contains("theme = \"mono\" # was light"), "{out}");
        assert!(out.contains("mouse = false"));
        assert!(out.contains("push = false"));
        let loaded = config::parse(&out).unwrap();
        assert_eq!(loaded.config.ui.theme, "mono");
    }

    #[test]
    fn edits_add_missing_keys_and_sections() {
        let out = edit_text("", &Setting::UndoSend(Duration::from_secs(20))).unwrap();
        assert_eq!(config::parse(&out).unwrap().config.ui.undo_send_seconds, 20);
        let out = edit_text(
            "[sync]\npush = true\n",
            &Setting::Density(ui::Density::Compact),
        )
        .unwrap();
        let c = config::parse(&out).unwrap().config;
        assert_eq!(c.ui.density, Density::Compact);
        assert!(c.sync.push);
        let out = edit_text("ui = { theme = \"dark\" }\n", &Setting::PreferHtml(true)).unwrap();
        let c = config::parse(&out).unwrap().config;
        assert!(c.ui.prefer_html);
        assert_eq!(c.ui.theme, "dark");
        let out = edit_text("", &Setting::RemoteImages(true)).unwrap();
        assert_eq!(
            config::parse(&out).unwrap().config.ui.remote_images,
            RemoteImages::On
        );
        let out = edit_text("", &Setting::MarkReadDelay(Duration::from_secs(90))).unwrap();
        assert_eq!(
            config::parse(&out).unwrap().config.ui.mark_read_delay_ms,
            60_000
        );
        let out = edit_text("", &Setting::MarkReadOnArchive(true)).unwrap();
        assert!(config::parse(&out).unwrap().config.ui.mark_read_on_archive);
        let out = edit_text("", &Setting::Preset(ui::Preset::Vim)).unwrap();
        assert_eq!(config::parse(&out).unwrap().config.ui.preset, Preset::Vim);
    }

    #[test]
    fn broken_files_are_refused() {
        assert!(edit_text("[ui\n", &Setting::PreferHtml(true)).is_err());
        assert!(edit_text("ui = 3\n", &Setting::PreferHtml(true)).is_err());
    }

    #[test]
    fn apply_writes_the_file_and_rebuilds_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conf").join("config.toml");
        let mut s = Settings::new(path.clone(), Config::default(), false, TimeZone::UTC);
        let (ui, err) = s.apply(&Setting::UndoSend(Duration::from_secs(4)));
        assert_eq!(err, None);
        assert_eq!(ui.unwrap().undo_send, Duration::from_secs(4));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("undo_send_seconds = 4"), "{text}");
        assert_eq!(s.config().ui.undo_send_seconds, 4);
    }

    #[test]
    fn apply_keeps_the_change_when_the_file_cannot_be_written() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the file should be: reading it fails.
        let path = dir.path().join("config.toml");
        std::fs::create_dir(&path).unwrap();
        let mut s = Settings::new(path, Config::default(), false, TimeZone::UTC);
        let (ui, err) = s.apply(&Setting::PreferHtml(true));
        assert!(ui.unwrap().prefer_html);
        assert!(err.is_some());
        assert!(s.config().ui.prefer_html);
    }

    #[test]
    fn no_color_forces_mono_and_bad_keys_fail() {
        let s = Settings::new(PathBuf::new(), Config::default(), true, TimeZone::UTC);
        let (ui, warnings) = s.ui_config().unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(ui.theme, Theme::named("mono", true).unwrap());
        let config = Config {
            keys: toml::from_str("[list]\nj = \"no-such-action\"\n").unwrap(),
            ..Config::default()
        };
        let s = Settings::new(PathBuf::new(), config, false, TimeZone::UTC);
        let errors = s.ui_config().unwrap_err();
        assert!(errors[0].contains("no-such-action"), "{errors:?}");
        let mut config = Config::default();
        config.ui.theme = "sparkly".into();
        let s = Settings::new(PathBuf::new(), config, false, TimeZone::UTC);
        let (_, warnings) = s.ui_config().unwrap();
        assert!(warnings[0].contains("sparkly"), "{warnings:?}");
    }
}
