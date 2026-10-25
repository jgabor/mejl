//! User macros: a named list of changes to mail, bound to keys.
//!
//! ```toml
//! [macros.file-receipt]
//! label = "File receipt"
//! keys = ["T"]
//! steps = ["mark-read", "unpin", { move = "Receipts/2026", id = "M123" }]
//! ```
//!
//! Every step changes the same conversations: the selection, or the one
//! under the cursor. The shell sends all steps as one change, so one undo
//! takes the whole macro back. Steps only change mail; there is no
//! navigation, no sending and no permanent delete.
//!
//! A macro's keys belong to the macro, not to a preset, so they work with
//! every preset. They bind in the list and the reader. A key that would
//! break the active preset is skipped with a warning instead.

use std::fmt;

use thiserror::Error;

use crate::data::MailboxId;
use crate::keymap::KeySeq;

/// Most steps in one macro.
pub const MAX_STEPS: usize = 10;

/// Longest macro id and label, in characters.
pub const MAX_NAME: usize = 40;

/// A macro, by its place in [`UiConfig::macros`](crate::UiConfig::macros).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MacroId(pub u16);

/// A folder a step names: the path the user picked and, when known, the
/// mailbox id. The id is tried first, so a renamed folder still matches;
/// the path is the fallback and what the user reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderRef {
    /// The full path, `/` between levels, for example `Receipts/2026`.
    pub path: String,
    /// The mailbox id when the folder was picked in the app.
    pub id: Option<MailboxId>,
}

impl FolderRef {
    /// A reference by path only.
    pub fn path(path: impl Into<String>) -> FolderRef {
        FolderRef {
            path: path.into(),
            id: None,
        }
    }
}

/// One change a macro makes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Mark read.
    MarkRead,
    /// Mark unread.
    MarkUnread,
    /// Pin.
    Pin,
    /// Unpin.
    Unpin,
    /// Mute the conversation.
    Mute,
    /// Move to the archive.
    Archive,
    /// Move to trash.
    Trash,
    /// Report spam.
    Spam,
    /// Move to a folder.
    Move(FolderRef),
    /// Add a label (a folder the mail is also in).
    AddLabel(FolderRef),
    /// Remove a label.
    RemoveLabel(FolderRef),
}

/// Plain steps by config name, in the order the step picker lists them.
pub const SIMPLE_STEPS: &[(&str, Step)] = &[
    ("mark-read", Step::MarkRead),
    ("mark-unread", Step::MarkUnread),
    ("pin", Step::Pin),
    ("unpin", Step::Unpin),
    ("mute", Step::Mute),
    ("archive", Step::Archive),
    ("trash", Step::Trash),
    ("spam", Step::Spam),
];

/// Steps that name a folder, by config key.
pub const FOLDER_STEPS: &[&str] = &["move", "label", "unlabel"];

impl Step {
    /// Whether the step takes the mail out of its mailbox. A macro has at
    /// most one.
    pub fn moves(&self) -> bool {
        matches!(
            self,
            Step::Archive | Step::Trash | Step::Spam | Step::Move(_)
        )
    }

    /// The folder the step names, if any.
    pub fn folder(&self) -> Option<&FolderRef> {
        match self {
            Step::Move(f) | Step::AddLabel(f) | Step::RemoveLabel(f) => Some(f),
            _ => None,
        }
    }

    /// The folder, to change it.
    pub fn folder_mut(&mut self) -> Option<&mut FolderRef> {
        match self {
            Step::Move(f) | Step::AddLabel(f) | Step::RemoveLabel(f) => Some(f),
            _ => None,
        }
    }

    /// The config name of a plain step, or the key of a folder step.
    pub fn name(&self) -> &'static str {
        match self {
            Step::Move(_) => "move",
            Step::AddLabel(_) => "label",
            Step::RemoveLabel(_) => "unlabel",
            s => SIMPLE_STEPS
                .iter()
                .find(|(_, step)| step == s)
                .map_or("", |(name, _)| name),
        }
    }

    /// The step from its config form: a name, or a one-key table such as
    /// `{ move = "Receipts", id = "M1" }`.
    pub fn from_toml(value: &toml::Value) -> Result<Step, String> {
        match value {
            toml::Value::String(name) => {
                let name = name.trim();
                if let Some((_, step)) = SIMPLE_STEPS.iter().find(|(n, _)| *n == name) {
                    return Ok(step.clone());
                }
                if FOLDER_STEPS.contains(&name) {
                    return Err(format!(
                        "\"{name}\" needs a folder: {{ {name} = \"Folder/Path\" }}"
                    ));
                }
                Err(format!("unknown step \"{name}\"{}", suggest(name)))
            }
            toml::Value::Table(t) => {
                let kind = FOLDER_STEPS
                    .iter()
                    .copied()
                    .find(|k| t.contains_key(*k))
                    .ok_or_else(|| "a step table needs one of: move, label, unlabel".to_owned())?;
                if let Some(extra) = t.keys().find(|k| *k != kind && *k != "id") {
                    return Err(format!("unknown key \"{extra}\" in a {kind} step"));
                }
                let path = t
                    .get(kind)
                    .and_then(toml::Value::as_str)
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .ok_or_else(|| format!("{kind} needs a folder path in quotes"))?;
                let id = match t.get("id") {
                    None => None,
                    Some(toml::Value::String(id)) if !id.trim().is_empty() => {
                        Some(MailboxId::from(id.trim()))
                    }
                    Some(_) => return Err("id must be a mailbox id in quotes".to_owned()),
                };
                let folder = FolderRef {
                    path: path.to_owned(),
                    id,
                };
                Ok(match kind {
                    "move" => Step::Move(folder),
                    "label" => Step::AddLabel(folder),
                    _ => Step::RemoveLabel(folder),
                })
            }
            _ => Err("a step is a name in quotes or a { move = \"Folder\" } table".to_owned()),
        }
    }

    /// The config form, as [`Step::from_toml`] reads it.
    pub fn to_toml(&self) -> toml::Value {
        match self.folder() {
            None => toml::Value::String(self.name().to_owned()),
            Some(f) => {
                let mut t = toml::Table::new();
                t.insert(self.name().to_owned(), f.path.clone().into());
                if let Some(id) = &f.id {
                    t.insert("id".into(), id.as_str().into());
                }
                toml::Value::Table(t)
            }
        }
    }
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Step::MarkRead => f.write_str("Mark read"),
            Step::MarkUnread => f.write_str("Mark unread"),
            Step::Pin => f.write_str("Pin"),
            Step::Unpin => f.write_str("Unpin"),
            Step::Mute => f.write_str("Mute"),
            Step::Archive => f.write_str("Archive"),
            Step::Trash => f.write_str("Move to trash"),
            Step::Spam => f.write_str("Report spam"),
            Step::Move(d) => write!(f, "Move to {}", d.path),
            Step::AddLabel(d) => write!(f, "Add label {}", d.path),
            Step::RemoveLabel(d) => write!(f, "Remove label {}", d.path),
        }
    }
}

fn suggest(name: &str) -> String {
    SIMPLE_STEPS
        .iter()
        .map(|(n, _)| *n)
        .chain(FOLDER_STEPS.iter().copied())
        .map(|n| (crate::action::edit_distance(name, n), n))
        .filter(|(d, n)| *d <= 2.max(n.len() / 3))
        .min_by_key(|(d, _)| *d)
        .map(|(_, n)| format!(", did you mean \"{n}\"?"))
        .unwrap_or_default()
}

/// A named list of steps and the keys that run it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Macro {
    /// The config table name, as in `[macros.<id>]`.
    pub id: String,
    /// What the user calls it. Shown in help, the palette and toasts.
    pub label: String,
    /// Keys that run it in the list and the reader.
    pub keys: Vec<KeySeq>,
    /// The changes, in order.
    pub steps: Vec<Step>,
}

/// A problem in the `[macros]` table.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum MacroError {
    /// `macros` or `macros.<id>` is not a table.
    #[error("[{name}] must be a table")]
    NotATable {
        /// The table name.
        name: String,
    },
    /// Something wrong inside one macro.
    #[error("[macros.{id}] {reason}")]
    Invalid {
        /// The macro id.
        id: String,
        /// What is wrong.
        reason: String,
    },
}

impl Macro {
    /// Checks the rules every macro keeps: an id of lowercase letters,
    /// digits and dashes, a label, 1 to [`MAX_STEPS`] steps, and at most
    /// one step that moves the mail.
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.id) {
            return Err(format!(
                "the name must be 1 to {MAX_NAME} lowercase letters, digits and dashes"
            ));
        }
        if self.label.trim().is_empty() {
            return Err("needs a label".into());
        }
        if self.label.chars().count() > MAX_NAME {
            return Err(format!("the label is longer than {MAX_NAME} characters"));
        }
        if self.steps.is_empty() {
            return Err("needs at least one step".into());
        }
        if self.steps.len() > MAX_STEPS {
            return Err(format!("has more than {MAX_STEPS} steps"));
        }
        if self.steps.iter().filter(|s| s.moves()).count() > 1 {
            return Err(
                "can move the mail only once: keep one of move, archive, trash, spam".into(),
            );
        }
        Ok(())
    }

    /// Reads one `[macros.<id>]` table.
    pub fn from_toml(id: &str, table: &toml::Table) -> Result<Macro, MacroError> {
        let invalid = |reason: String| MacroError::Invalid {
            id: id.to_owned(),
            reason,
        };
        if let Some(extra) = table
            .keys()
            .find(|k| !["label", "keys", "steps"].contains(&k.as_str()))
        {
            return Err(invalid(format!(
                "unknown key \"{extra}\"; a macro has label, keys and steps"
            )));
        }
        let label = match table.get("label") {
            None => id.to_owned(),
            Some(toml::Value::String(s)) => s.trim().to_owned(),
            Some(_) => return Err(invalid("label must be text in quotes".into())),
        };
        let keys = match table.get("keys") {
            None => Vec::new(),
            Some(toml::Value::Array(keys)) => keys
                .iter()
                .map(|k| {
                    let text = k
                        .as_str()
                        .ok_or_else(|| invalid("keys must be key names in quotes".into()))?;
                    text.parse::<KeySeq>()
                        .map_err(|e| invalid(format!("key \"{text}\": {e}")))
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err(invalid("keys must be a list, as in keys = [\"T\"]".into())),
        };
        let steps = match table.get("steps") {
            Some(toml::Value::Array(steps)) => steps
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    Step::from_toml(s).map_err(|e| invalid(format!("step {}: {e}", i + 1)))
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err(invalid("steps must be a list".into())),
            None => return Err(invalid("needs steps = [ ... ]".into())),
        };
        let m = Macro {
            id: id.to_owned(),
            label,
            keys,
            steps,
        };
        m.validate().map_err(invalid)?;
        Ok(m)
    }

    /// The config table, as [`Macro::from_toml`] reads it.
    pub fn to_toml(&self) -> toml::Table {
        let mut t = toml::Table::new();
        t.insert("label".into(), self.label.clone().into());
        if !self.keys.is_empty() {
            t.insert(
                "keys".into(),
                toml::Value::Array(self.keys.iter().map(|k| k.to_config().into()).collect()),
            );
        }
        t.insert(
            "steps".into(),
            toml::Value::Array(self.steps.iter().map(Step::to_toml).collect()),
        );
        t
    }
}

/// Reads the whole `[macros]` table, sorted by id. Every problem is
/// collected, so `mejl keys --check` can list them all.
pub fn macros_from_table(table: &toml::Table) -> Result<Vec<Macro>, Vec<MacroError>> {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for (id, value) in table {
        let Some(t) = value.as_table() else {
            errors.push(MacroError::NotATable {
                name: format!("macros.{id}"),
            });
            continue;
        };
        match Macro::from_toml(id, t) {
            Ok(m) => out.push(m),
            Err(e) => errors.push(e),
        }
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

/// Whether `id` can name a macro or a preset: 1 to [`MAX_NAME`] lowercase
/// ASCII letters, digits and dashes, starting with a letter or digit.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_NAME
        && !id.starts_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A free id made from a label: lowercase, dashes for anything else, and a
/// number on the end when `taken` already has it.
pub fn id_for(label: &str, taken: &[&str]) -> String {
    let mut base = String::new();
    for c in label.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            base.push(c);
        } else if !base.ends_with('-') && !base.is_empty() {
            base.push('-');
        }
    }
    let mut base: String = base
        .trim_end_matches('-')
        .chars()
        .take(MAX_NAME - 3)
        .collect();
    if base.is_empty() {
        base.push_str("macro");
    }
    if !taken.contains(&base.as_str()) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken.contains(&id.as_str()))
        .unwrap_or(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> toml::Table {
        text.parse().unwrap()
    }

    #[test]
    fn a_macro_round_trips_through_toml() {
        let t = table(
            r#"
            [file-receipt]
            label = "File receipt"
            keys = ["T", "g x"]
            steps = ["mark-read", "unpin", { move = "Receipts/2026", id = "M1" }, { label = "Tax" }]
            "#,
        );
        let ms = macros_from_table(&t).unwrap();
        assert_eq!(ms.len(), 1);
        let m = &ms[0];
        assert_eq!(m.label, "File receipt");
        assert_eq!(m.keys.len(), 2);
        assert_eq!(
            m.steps,
            vec![
                Step::MarkRead,
                Step::Unpin,
                Step::Move(FolderRef {
                    path: "Receipts/2026".into(),
                    id: Some("M1".into())
                }),
                Step::AddLabel(FolderRef::path("Tax")),
            ]
        );
        let again = Macro::from_toml(&m.id, &m.to_toml()).unwrap();
        assert_eq!(&again, m);
        assert_eq!(m.steps[2].to_string(), "Move to Receipts/2026");
    }

    #[test]
    fn bad_macros_say_what_is_wrong() {
        let err = |text: &str| {
            macros_from_table(&table(text))
                .unwrap_err()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(err("[a]\nsteps = [\"mark-raed\"]").contains("did you mean \"mark-read\""));
        assert!(err("[a]\nsteps = [\"move\"]").contains("needs a folder"));
        assert!(err("[a]\nsteps = [\"archive\", \"trash\"]").contains("only once"));
        assert!(err("[a]\nsteps = []").contains("at least one step"));
        assert!(err("[a]\nlabel = \"x\"").contains("needs steps"));
        assert!(err("[a]\nsteps = [\"pin\"]\nkeys = [\"ctrl-\"]").contains("key \"ctrl-\""));
        assert!(err("[A]\nsteps = [\"pin\"]").contains("lowercase"));
        assert!(err("[a]\nsteps = [{ move = \"X\", to = \"Y\" }]").contains("unknown key \"to\""));
        assert!(err("[a]\nsteps = [\"pin\"]\nwhen = 1").contains("unknown key \"when\""));
        assert!(err("a = 1").contains("[macros.a] must be a table"));
        let many = format!("[a]\nsteps = [{}]", ["\"pin\""; 11].join(", "));
        assert!(err(&many).contains("more than 10"));
    }

    #[test]
    fn ids_come_from_labels() {
        assert_eq!(id_for("File receipt!", &[]), "file-receipt");
        assert_eq!(id_for("File receipt", &["file-receipt"]), "file-receipt-2");
        assert_eq!(id_for("  ", &[]), "macro");
        assert_eq!(id_for("Åsa's mail", &[]), "sa-s-mail");
        assert!(valid_id("a-1") && !valid_id("-a") && !valid_id("A") && !valid_id(""));
    }
}
