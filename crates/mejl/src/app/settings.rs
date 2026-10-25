//! Building the UI's settings from the config file, and writing changes
//! from the settings screen back to it.
//!
//! Changes go into the config file itself, edited in place with toml_edit,
//! so the user's comments, ordering and other settings survive. The new
//! text is checked by parsing it the way startup does before it replaces
//! the old file, and the replace is atomic (a temporary file in the same
//! directory, then a rename). When the file cannot be written, a setting
//! still applies until mejl quits and the toast says so.
//!
//! Key editor changes ([`KeysEdit`]) are different: they edit custom
//! presets and macros, which live only in the file, so they apply only
//! when the file is written.

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::tz::TimeZone;
use mejl_ui::{
    self as ui, KeyOverrides, Keymap, KeymapSpec, KeysEdit, Macro, Setting, Theme, UiConfig,
};
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

/// The parsed key setup of a config: custom presets, the top-level
/// `[keys]` overrides and macros.
#[derive(Debug, Clone, Default)]
pub struct KeySetup {
    /// Custom presets, by name.
    pub presets: Vec<ui::keymap::CustomPreset>,
    /// The top-level `[keys]` overrides.
    pub user: KeyOverrides,
    /// Macros, sorted by id.
    pub macros: Vec<Macro>,
}

/// Parses the key setup. Errors are every problem found, as
/// `mejl keys --check` prints them.
pub fn key_setup(c: &Config) -> Result<KeySetup, Vec<String>> {
    let mut errors = Vec::new();
    let user = KeyOverrides::from_table(&c.keys).unwrap_or_else(|e| {
        errors.extend(e.iter().map(ToString::to_string));
        KeyOverrides::default()
    });
    let mut presets = Vec::new();
    for (name, p) in &c.presets {
        match KeyOverrides::from_table(&p.keys) {
            Ok(overrides) => presets.push(ui::keymap::CustomPreset {
                name: name.clone(),
                base: preset(p.base),
                overrides,
            }),
            Err(e) => errors.extend(e.iter().map(|e| format!("[presets.{name}] {e}"))),
        }
    }
    let macros = ui::macros::macros_from_table(&c.macros).unwrap_or_else(|e| {
        errors.extend(e.iter().map(ToString::to_string));
        Vec::new()
    });
    if errors.is_empty() {
        Ok(KeySetup {
            presets,
            user,
            macros,
        })
    } else {
        Err(errors)
    }
}

/// The keymap for the named preset: a built-in one, or a custom one from
/// `setup`.
pub fn keymap_for(name: &str, setup: &KeySetup) -> Result<Keymap, Vec<String>> {
    let none = KeyOverrides::default();
    let (base, own) = match Preset::from_name(name) {
        Some(p) => (preset(p), &none),
        None => match setup.presets.iter().find(|p| p.name == name) {
            Some(p) => (p.base, &p.overrides),
            None => return Err(vec![format!("there is no key preset called {name:?}")]),
        },
    };
    Keymap::build(KeymapSpec {
        base,
        name,
        preset: own,
        user: &setup.user,
        macros: &setup.macros,
    })
    .map_err(|errors| {
        errors
            .iter()
            .map(|e| match Preset::from_name(name) {
                Some(_) => e.to_string(),
                None => format!("preset {name}: {e}"),
            })
            .collect()
    })
}

/// The active keymap of a config.
pub fn keymap(c: &Config) -> Result<(Keymap, KeySetup), Vec<String>> {
    let setup = key_setup(c)?;
    let keymap = keymap_for(&c.ui.preset, &setup)?;
    Ok((keymap, setup))
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
        let (keymap, setup) = keymap(&self.config)?;
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
            presets: setup.presets,
            user_keys: setup.user,
            macros: setup.macros,
            ..UiConfig::default()
        };
        Ok((config, warnings))
    }

    /// Applies a setting: in memory at once, then to the config file. The
    /// returned config is the whole rebuilt UI config. The error, if any,
    /// says what could not be saved; the change applies anyway. With no
    /// config, the change was refused and nothing changed.
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

    fn read(&self) -> Result<String, String> {
        match std::fs::read_to_string(&self.path) {
            Ok(t) => Ok(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(format!("cannot read {}: {e}", self.path.display())),
        }
    }

    /// Applies a key editor change by editing the config file and loading
    /// the result. Nothing changes, in memory or on disk, unless the new
    /// file loads, builds a keymap and is written.
    pub fn edit_keys(&mut self, edit: &KeysEdit) -> Result<UiConfig, String> {
        let text = self.read()?;
        let updated = edit_keys_text(&text, edit)?;
        let loaded =
            config::parse(&updated).map_err(|e| format!("the change would not load: {e}"))?;
        let candidate = Settings {
            config: loaded.config,
            ..self.clone()
        };
        let (ui, _) = candidate.ui_config().map_err(|errors| errors.join("; "))?;
        write_atomic(&self.path, &updated)
            .map_err(|e| format!("cannot write {}: {e}", self.path.display()))?;
        self.config = candidate.config;
        Ok(ui)
    }

    fn write(&self, setting: &Setting) -> Result<(), String> {
        let text = self.read()?;
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
        Setting::Preset(name) => ui.preset = name.clone(),
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
        Setting::Preset(name) => ("preset", value(name.as_str())),
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

/// The config text with a key editor change made, everything else as it
/// was, comments included.
pub fn edit_keys_text(text: &str, edit: &KeysEdit) -> Result<String, String> {
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("the config file is not valid TOML: {e}"))?;
    match edit {
        KeysEdit::SetPresetKeys {
            preset,
            overrides,
            drop_user,
        } => {
            let p = custom_preset(&mut doc, preset)?;
            set_keys(p, overrides);
            drop_user_keys(&mut doc, drop_user);
        }
        KeysEdit::NewPreset {
            name,
            base,
            overrides,
            take_user,
        } => {
            if Preset::from_name(name).is_some() || !ui::macros::valid_id(name) {
                return Err(format!(
                    "{name:?} cannot name a preset: use lowercase letters, digits and dashes"
                ));
            }
            let presets = section(&mut doc, "presets")?;
            if presets.contains_key(name) {
                return Err(format!("there is already a preset called {name}"));
            }
            let mut overrides = overrides.clone();
            if *take_user && doc.contains_key("keys") {
                // The copy must keep exactly the keys in effect, so the two
                // layers are applied in order and stored as one diff.
                let user = user_keys(text).ok_or_else(|| {
                    "the [keys] table in the config file has errors; fix them first".to_owned()
                })?;
                let base_map = Keymap::preset(*base);
                let mut map = base_map.clone();
                map.apply(&overrides);
                map.apply(&user);
                overrides = map.diff_from(&base_map);
                doc.remove("keys");
            }
            let mut table = toml_edit::Table::new();
            table.insert("base", value(base.name()));
            let presets = section(&mut doc, "presets")?;
            presets.insert(name, Item::Table(table));
            let p = custom_preset(&mut doc, name)?;
            set_keys(p, &overrides);
            set_ui(&mut doc, "preset", value(name.as_str()))?;
        }
        KeysEdit::RenamePreset { from, to } => {
            if Preset::from_name(to).is_some() || !ui::macros::valid_id(to) {
                return Err(format!(
                    "{to:?} cannot name a preset: use lowercase letters, digits and dashes"
                ));
            }
            let presets = section(&mut doc, "presets")?;
            if presets.contains_key(to) {
                return Err(format!("there is already a preset called {to}"));
            }
            let item = presets
                .remove(from)
                .ok_or_else(|| format!("there is no preset called {from}"))?;
            presets.insert(to, item);
            if active_preset(&doc).as_deref() == Some(from.as_str()) {
                set_ui(&mut doc, "preset", value(to.as_str()))?;
            }
        }
        KeysEdit::DeletePreset { name } => {
            let presets = section(&mut doc, "presets")?;
            let item = presets
                .remove(name)
                .ok_or_else(|| format!("there is no preset called {name}"))?;
            if active_preset(&doc).as_deref() == Some(name.as_str()) {
                let base = item
                    .get("base")
                    .and_then(Item::as_str)
                    .and_then(Preset::from_name)
                    .unwrap_or_default();
                set_ui(&mut doc, "preset", value(base.name()))?;
            }
        }
        KeysEdit::SaveMacro(m) => {
            m.validate()
                .map_err(|e| format!("macro {}: {e}", m.label))?;
            let macros = section(&mut doc, "macros")?;
            let new = macro_table(m);
            match macros.get_mut(&m.id).and_then(Item::as_table_mut) {
                // Keep the header comment and the comments on each value.
                Some(old) => {
                    let stale: Vec<String> = old
                        .iter()
                        .map(|(k, _)| k.to_owned())
                        .filter(|k| !new.contains_key(k))
                        .collect();
                    for k in stale {
                        old.remove(&k);
                    }
                    for (k, v) in new.iter() {
                        set_kept(old, k, v.clone());
                    }
                }
                None => {
                    macros.insert(&m.id, Item::Table(new));
                }
            }
        }
        KeysEdit::DeleteMacro { id } => {
            let macros = section(&mut doc, "macros")?;
            macros
                .remove(id)
                .ok_or_else(|| format!("there is no macro called {id}"))?;
        }
    }
    Ok(doc.to_string())
}

/// The top-level `[keys]` overrides of a config text.
fn user_keys(text: &str) -> Option<KeyOverrides> {
    let doc: toml::Table = text.parse().ok()?;
    KeyOverrides::from_table(doc.get("keys")?.as_table()?).ok()
}

fn active_preset(doc: &DocumentMut) -> Option<String> {
    doc.get("ui")?.get("preset")?.as_str().map(str::to_owned)
}

/// A top-level table, made (implicit, so it prints no empty header) when
/// missing.
fn section<'a>(doc: &'a mut DocumentMut, name: &str) -> Result<&'a mut toml_edit::Table, String> {
    if !doc.contains_key(name) {
        let mut t = toml_edit::Table::new();
        t.set_implicit(true);
        doc.insert(name, Item::Table(t));
    }
    doc.get_mut(name)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| format!("[{name}] in the config file is not a table"))
}

fn custom_preset<'a>(
    doc: &'a mut DocumentMut,
    name: &str,
) -> Result<&'a mut toml_edit::Table, String> {
    section(doc, "presets")?
        .get_mut(name)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| format!("there is no preset called {name}"))
}

fn set_ui(doc: &mut DocumentMut, key: &str, item: Item) -> Result<(), String> {
    let ui = section(doc, "ui")?;
    ui.set_implicit(false);
    set_kept(ui, key, item);
    Ok(())
}

/// Sets a value, keeping the comment and spacing around an old one.
fn set_kept(table: &mut dyn toml_edit::TableLike, key: &str, item: Item) {
    match table.get_mut(key) {
        Some(Item::Value(old)) => {
            if let Item::Value(mut new) = item {
                *new.decor_mut() = old.decor().clone();
                *old = new;
            }
        }
        _ => {
            table.insert(key, item);
        }
    }
}

/// What a `[keys]` value means: an action name, or `None` for an unbind.
fn key_action(v: &Item) -> Option<String> {
    let name = v.as_str()?.trim();
    (!matches!(name, "" | "none" | "unbind")).then(|| name.to_owned())
}

/// Makes a preset's `keys` hold exactly these overrides, one table per
/// context. Entries that stay keep their spelling and comments; only the
/// ones that changed are rewritten.
fn set_keys(preset: &mut toml_edit::Table, overrides: &KeyOverrides) {
    let wanted = overrides.to_table();
    if wanted.is_empty() {
        preset.remove("keys");
        return;
    }
    if !preset.get("keys").is_some_and(Item::is_table_like) {
        let mut t = toml_edit::Table::new();
        t.set_implicit(true);
        preset.insert("keys", Item::Table(t));
    }
    let Some(keys) = preset.get_mut("keys").and_then(Item::as_table_like_mut) else {
        return;
    };
    let gone: Vec<String> = keys
        .iter()
        .map(|(k, _)| k.to_owned())
        .filter(|k| !wanted.contains_key(k))
        .collect();
    for k in gone {
        keys.remove(&k);
    }
    for (ctx, entries) in &wanted {
        let Some(entries) = entries.as_table() else {
            continue;
        };
        if !keys.get(ctx).is_some_and(Item::is_table_like) {
            keys.insert(ctx, Item::Table(toml_edit::Table::new()));
        }
        let Some(t) = keys.get_mut(ctx).and_then(Item::as_table_like_mut) else {
            continue;
        };
        // Match the file's spelling of each key by what it means.
        let mut seen: Vec<String> = Vec::new();
        let names: Vec<String> = t.iter().map(|(k, _)| k.to_owned()).collect();
        for name in names {
            let canonical = name.parse::<ui::KeySeq>().ok().map(|s| s.to_config());
            match canonical
                .as_ref()
                .and_then(|c| entries.get(c).map(|v| (c, v)))
            {
                Some((c, v)) if !seen.contains(c) => {
                    let want = v.as_str().filter(|a| *a != "none").map(str::to_owned);
                    if t.get(&name).and_then(key_action) != want {
                        set_kept(t, &name, value(v.as_str().unwrap_or("none")));
                    }
                    seen.push(c.clone());
                }
                _ => {
                    t.remove(&name);
                }
            }
        }
        for (k, v) in entries {
            if !seen.contains(k) {
                t.insert(k, value(v.as_str().unwrap_or("none")));
            }
        }
    }
}

/// Removes these entries from the top-level `[keys]`, and empty context
/// tables after them.
fn drop_user_keys(doc: &mut DocumentMut, drop: &[(ui::Context, ui::KeySeq)]) {
    let Some(keys) = doc.get_mut("keys").and_then(Item::as_table_like_mut) else {
        return;
    };
    for (ctx, seq) in drop {
        let Some(t) = keys.get_mut(ctx.name()).and_then(Item::as_table_like_mut) else {
            continue;
        };
        // The file may spell the key another way, as "^k" for "ctrl-k".
        let names: Vec<String> = t
            .iter()
            .map(|(k, _)| k.to_owned())
            .filter(|k| k.parse::<ui::KeySeq>().is_ok_and(|s| s == *seq))
            .collect();
        for n in names {
            t.remove(&n);
        }
    }
    let empty: Vec<String> = keys
        .iter()
        .filter(|(_, v)| v.as_table_like().is_some_and(|t| t.is_empty()))
        .map(|(k, _)| k.to_owned())
        .collect();
    for k in empty {
        keys.remove(&k);
    }
    if keys.is_empty() {
        doc.remove("keys");
    }
}

/// A macro's table in reading order: label, keys, then steps, each folder
/// step as `{ move = "Path", id = "M1" }`.
fn macro_table(m: &Macro) -> toml_edit::Table {
    let mut t = toml_edit::Table::new();
    t.insert("label", value(m.label.as_str()));
    if !m.keys.is_empty() {
        let mut keys = toml_edit::Array::new();
        for k in &m.keys {
            keys.push(k.to_config());
        }
        t.insert("keys", value(keys));
    }
    let mut steps = toml_edit::Array::new();
    for step in &m.steps {
        match step.folder() {
            None => steps.push(step.name()),
            Some(f) => {
                let mut inline = toml_edit::InlineTable::new();
                inline.insert(step.name(), f.path.as_str().into());
                if let Some(id) = &f.id {
                    inline.insert("id", id.as_str().into());
                }
                steps.push(inline);
            }
        }
    }
    t.insert("steps", value(steps));
    t
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
        let out = edit_text("", &Setting::Preset("vim".into())).unwrap();
        assert_eq!(config::parse(&out).unwrap().config.ui.preset, "vim");
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

    fn overrides(text: &str) -> KeyOverrides {
        KeyOverrides::from_toml(text).unwrap()
    }

    fn seq(s: &str) -> ui::KeySeq {
        s.parse().unwrap()
    }

    #[test]
    fn a_new_preset_takes_over_the_keys_table_and_becomes_active() {
        let text = "# mine\n[ui]\ntheme = \"dark\" # dark\n\n[keys.list]\n\"ctrl-n\" = \"next\"\n";
        let out = edit_keys_text(
            text,
            &KeysEdit::NewPreset {
                name: "mine".into(),
                base: ui::Preset::Fastmail,
                overrides: overrides("[keys.list]\ne = \"archive\"\n"),
                take_user: true,
            },
        )
        .unwrap();
        assert!(
            out.contains("# mine") && out.contains("theme = \"dark\" # dark"),
            "{out}"
        );
        let c = config::parse(&out).unwrap().config;
        assert_eq!(c.ui.preset, "mine");
        assert!(c.keys.is_empty(), "{out}");
        let p = &c.presets["mine"];
        assert_eq!(p.base, Preset::Fastmail);
        let list = p.keys["list"].as_table().unwrap();
        assert_eq!(list["e"].as_str(), Some("archive"));
        assert_eq!(list["ctrl-n"].as_str(), Some("next"));
        assert!(out.contains("[presets.mine.keys.list]"), "{out}");
        assert!(!out.contains("[presets]\n"), "no empty header: {out}");
        for bad in ["vim", "Mine", "", "a b"] {
            let e = edit_keys_text(
                &out,
                &KeysEdit::NewPreset {
                    name: bad.into(),
                    base: ui::Preset::Vim,
                    overrides: KeyOverrides::default(),
                    take_user: false,
                },
            );
            assert!(e.is_err(), "{bad:?}");
        }
    }

    #[test]
    fn preset_keys_are_replaced_and_take_keys_from_the_keys_table() {
        let text = "[ui]\npreset = \"mine\"\n[keys.list]\n\"e\" = \"trash\"\n\"j\" = \"next\"\n\
                    [presets.mine]\nbase = \"vim\"\n[presets.mine.keys.list]\nq = \"archive\"\n";
        let out = edit_keys_text(
            text,
            &KeysEdit::SetPresetKeys {
                preset: "mine".into(),
                overrides: overrides("[keys.list]\ne = \"archive\"\n\"y\" = \"none\"\n"),
                drop_user: vec![(ui::Context::List, seq("e"))],
            },
        )
        .unwrap();
        let c = config::parse(&out).unwrap().config;
        let list = c.presets["mine"].keys["list"].as_table().unwrap();
        assert_eq!(list.len(), 2, "{out}");
        assert_eq!(list["y"].as_str(), Some("none"));
        assert_eq!(c.presets["mine"].base, Preset::Vim);
        let user = c.keys["list"].as_table().unwrap();
        assert!(!user.contains_key("e") && user.contains_key("j"), "{out}");
        // Clearing every change leaves a bare preset.
        let out = edit_keys_text(
            &out,
            &KeysEdit::SetPresetKeys {
                preset: "mine".into(),
                overrides: KeyOverrides::default(),
                drop_user: vec![(ui::Context::List, seq("j"))],
            },
        )
        .unwrap();
        let c = config::parse(&out).unwrap().config;
        assert!(
            c.presets["mine"].keys.is_empty() && c.keys.is_empty(),
            "{out}"
        );
    }

    #[test]
    fn renaming_and_deleting_the_active_preset_moves_ui_preset() {
        let text = "[ui]\npreset = \"mine\"\n[presets.mine]\nbase = \"vim\"\n";
        let rename = |from: &str, to: &str| KeysEdit::RenamePreset {
            from: from.into(),
            to: to.into(),
        };
        let out = edit_keys_text(text, &rename("mine", "ours")).unwrap();
        let c = config::parse(&out).unwrap().config;
        assert_eq!(c.ui.preset, "ours");
        assert!(c.presets.contains_key("ours") && !c.presets.contains_key("mine"));
        assert!(edit_keys_text(&out, &rename("nope", "x")).is_err());
        assert!(edit_keys_text(&out, &rename("ours", "fastmail")).is_err());
        let out = edit_keys_text(
            &out,
            &KeysEdit::DeletePreset {
                name: "ours".into(),
            },
        )
        .unwrap();
        let c = config::parse(&out).unwrap().config;
        assert_eq!(c.ui.preset, "vim", "falls back to its base");
        assert!(c.presets.is_empty());
    }

    #[test]
    fn macros_are_written_and_deleted() {
        let m = Macro {
            id: "file-receipt".into(),
            label: "File receipt".into(),
            keys: vec![seq("F2")],
            steps: vec![
                ui::Step::MarkRead,
                ui::Step::Unpin,
                ui::Step::Move(ui::FolderRef {
                    path: "Receipts/2026".into(),
                    id: Some("M1".into()),
                }),
            ],
        };
        let out = edit_keys_text(
            "[ui]\ntheme = \"dark\" # hi\n",
            &KeysEdit::SaveMacro(Box::new(m.clone())),
        )
        .unwrap();
        assert!(
            out.contains("# hi") && out.contains("[macros.file-receipt]"),
            "{out}"
        );
        assert!(
            out.contains("label = \"File receipt\"\nkeys = [\"f2\"]\nsteps"),
            "{out}"
        );
        assert!(
            out.contains("{ move = \"Receipts/2026\", id = \"M1\" }"),
            "{out}"
        );
        let c = config::parse(&out).unwrap().config;
        let parsed = ui::macros::macros_from_table(&c.macros).unwrap();
        assert_eq!(parsed, vec![m.clone()]);
        let mut bad = m.clone();
        bad.steps.push(ui::Step::Trash);
        assert!(edit_keys_text(&out, &KeysEdit::SaveMacro(Box::new(bad))).is_err());
        let out = edit_keys_text(&out, &KeysEdit::DeleteMacro { id: m.id.clone() }).unwrap();
        assert!(
            config::parse(&out).unwrap().config.macros.is_empty(),
            "{out}"
        );
    }

    #[test]
    fn key_and_macro_edits_keep_comments() {
        let text = "[ui]\npreset = \"mine\"\n[presets.mine]\nbase = \"fastmail\"\n\
                    # my archive key\n[presets.mine.keys.list]\n\"e\" = \"archive\" # e for exit\n\
                    \"Control-x\" = \"trash\" # spelled my way\nq = \"next\"\n\
                    # receipts macro\n[macros.tidy]\nlabel = \"Tidy\" # shown in help\nsteps = [\"pin\"]\n";
        let out = edit_keys_text(
            text,
            &KeysEdit::SetPresetKeys {
                preset: "mine".into(),
                overrides: overrides(
                    "[keys.list]\ne = \"archive\"\n\"ctrl-x\" = \"report-spam\"\nz = \"none\"\n",
                ),
                drop_user: Vec::new(),
            },
        )
        .unwrap();
        assert!(out.contains("# my archive key"), "{out}");
        assert!(out.contains("\"e\" = \"archive\" # e for exit"), "{out}");
        assert!(
            out.contains("\"Control-x\" = \"report-spam\" # spelled my way"),
            "{out}"
        );
        assert!(!out.contains("q = "), "{out}");
        let c = config::parse(&out).unwrap().config;
        let list = c.presets["mine"].keys["list"].as_table().unwrap();
        assert_eq!(list["z"].as_str(), Some("none"));
        let m = Macro {
            id: "tidy".into(),
            label: "Tidy up".into(),
            keys: vec![seq("F2")],
            steps: vec![ui::Step::Unpin],
        };
        let out = edit_keys_text(&out, &KeysEdit::SaveMacro(Box::new(m))).unwrap();
        assert!(out.contains("# receipts macro"), "{out}");
        assert!(out.contains("label = \"Tidy up\" # shown in help"), "{out}");
        assert!(out.contains("steps = [\"unpin\"]"), "{out}");
    }

    #[test]
    fn a_copy_keeps_exactly_the_keys_in_effect() {
        // The preset binds `. z`; `[keys]` then frees the whole `.` prefix.
        let text = "[ui]\npreset = \"mine\"\n[keys.list]\n\".\" = \"none\"\n\
                    [presets.mine]\nbase = \"fastmail\"\n[presets.mine.keys.list]\n\". z\" = \"archive\"\n";
        let config = config::parse(text).unwrap().config;
        let s = Settings::new(PathBuf::new(), config, false, TimeZone::UTC);
        let before = s.ui_config().unwrap().0.keymap;
        let out = edit_keys_text(
            text,
            &KeysEdit::NewPreset {
                name: "copy".into(),
                base: ui::Preset::Fastmail,
                overrides: overrides("[keys.list]\n\". z\" = \"archive\"\n"),
                take_user: true,
            },
        )
        .unwrap();
        let config = config::parse(&out).unwrap().config;
        assert!(config.keys.is_empty(), "{out}");
        let s = Settings::new(PathBuf::new(), config, false, TimeZone::UTC);
        let after = s.ui_config().unwrap().0.keymap;
        assert_eq!(after.name(), "copy");
        assert_eq!(after.action_bindings(), before.action_bindings(), "{out}");
        // A `[keys]` table with errors is never thrown away.
        let bad = "[keys.list]\n\"ctrl-n\" = \"nxet\"\n";
        let e = edit_keys_text(
            bad,
            &KeysEdit::NewPreset {
                name: "copy".into(),
                base: ui::Preset::Fastmail,
                overrides: KeyOverrides::default(),
                take_user: true,
            },
        );
        assert!(e.unwrap_err().contains("has errors"));
    }

    #[test]
    fn a_refused_key_edit_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let text = "[ui]\npreset = \"mine\"\n[presets.mine]\nbase = \"fastmail\"\n";
        std::fs::write(&path, text).unwrap();
        let config = config::parse(text).unwrap().config;
        let mut s = Settings::new(path.clone(), config, false, TimeZone::UTC);
        // `.` also starts `. r`, so this preset could never load.
        let err = s
            .edit_keys(&KeysEdit::SetPresetKeys {
                preset: "mine".into(),
                overrides: overrides("[keys.list]\n\".\" = \"archive\"\n"),
                drop_user: Vec::new(),
            })
            .unwrap_err();
        assert!(err.contains("preset mine"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let ui = s
            .edit_keys(&KeysEdit::SetPresetKeys {
                preset: "mine".into(),
                overrides: overrides("[keys.list]\ne = \"archive\"\n"),
                drop_user: Vec::new(),
            })
            .unwrap();
        assert_eq!(ui.keymap.name(), "mine");
        assert_eq!(ui.presets.len(), 1);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("e = \"archive\"")
        );
        assert!(s.config().presets["mine"].keys.contains_key("list"));
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
