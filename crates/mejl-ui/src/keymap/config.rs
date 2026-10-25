//! User key overrides from the `[keys]` config table.
//!
//! ```toml
//! [keys.global]
//! "ctrl-k" = "palette"
//!
//! [keys.list]
//! "e" = "archive"   # bind
//! "b" = "none"      # unbind; also frees a prefix and everything under it
//! "g x" = "go-archive"
//! ```
//!
//! Every problem is collected, so `mejl keys --check` can list them all at
//! once instead of stopping at the first.

use thiserror::Error;

use super::{Conflict, Context, KeySeq};
use crate::action::Action;

/// A problem in the `[keys]` table.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum KeymapError {
    /// The config text is not valid TOML.
    #[error("the config is not valid TOML: {0}")]
    Toml(String),
    /// `keys` or `keys.<context>` is not a table.
    #[error("[{name}] must be a table of \"key\" = \"action\" entries")]
    NotATable {
        /// The table name.
        name: String,
    },
    /// A `[keys.<context>]` name mejl does not know.
    #[error("[keys.{name}] is not a known context; use one of: {known}")]
    UnknownContext {
        /// The name as written.
        name: String,
        /// The valid names.
        known: String,
    },
    /// A key string that does not parse.
    #[error("[keys.{context}] \"{key}\": {reason}")]
    InvalidKey {
        /// The context table.
        context: String,
        /// The key as written.
        key: String,
        /// Why it failed.
        reason: String,
    },
    /// The value is not a string.
    #[error("[keys.{context}] \"{key}\": the value must be an action name in quotes")]
    NotAString {
        /// The context table.
        context: String,
        /// The key as written.
        key: String,
    },
    /// An action name mejl does not know.
    #[error(
        "[keys.{context}] \"{key}\" = \"{action}\": unknown action{}",
        suggestion.map(|s| format!(", did you mean \"{s}\"?")).unwrap_or_default()
    )]
    UnknownAction {
        /// The context table.
        context: String,
        /// The key as written.
        key: String,
        /// The action as written.
        action: String,
        /// The closest known action name.
        suggestion: Option<&'static str>,
    },
    /// An unknown preset name.
    #[error("unknown key preset \"{0}\"; use \"fastmail\" or \"vim\"")]
    UnknownPreset(String),
    /// The overrides make a binding unreachable.
    #[error("{0}")]
    Conflict(Conflict),
}

/// One parsed override.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Override {
    /// Where it applies.
    pub context: Context,
    /// The keys.
    pub keys: KeySeq,
    /// The new action, or `None` to unbind.
    pub action: Option<Action>,
}

/// Parsed `[keys]` overrides, applied with
/// [`Keymap::with_overrides`](super::Keymap::with_overrides).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyOverrides {
    entries: Vec<Override>,
}

impl KeyOverrides {
    /// The overrides in file order.
    pub fn entries(&self) -> &[Override] {
        &self.entries
    }

    /// Parses a whole config document and reads its `[keys]` table. A
    /// document without one gives no overrides.
    pub fn from_toml(document: &str) -> Result<KeyOverrides, Vec<KeymapError>> {
        let doc: toml::Table = document
            .parse()
            .map_err(|e: toml::de::Error| vec![KeymapError::Toml(e.message().to_owned())])?;
        match doc.get("keys") {
            None => Ok(KeyOverrides::default()),
            Some(toml::Value::Table(t)) => KeyOverrides::from_table(t),
            Some(_) => Err(vec![KeymapError::NotATable {
                name: "keys".into(),
            }]),
        }
    }

    /// Reads the value of the `keys` table: one sub-table per context.
    pub fn from_table(keys: &toml::Table) -> Result<KeyOverrides, Vec<KeymapError>> {
        let mut entries = Vec::new();
        let mut errors = Vec::new();
        for (ctx_name, value) in keys {
            let Some(context) = Context::from_name(ctx_name) else {
                errors.push(KeymapError::UnknownContext {
                    name: ctx_name.clone(),
                    known: Context::ALL
                        .iter()
                        .map(|c| c.name())
                        .collect::<Vec<_>>()
                        .join(", "),
                });
                continue;
            };
            let toml::Value::Table(table) = value else {
                errors.push(KeymapError::NotATable {
                    name: format!("keys.{ctx_name}"),
                });
                continue;
            };
            for (key, value) in table {
                let keys = match key.parse::<KeySeq>() {
                    Ok(k) => k,
                    Err(e) => {
                        errors.push(KeymapError::InvalidKey {
                            context: ctx_name.clone(),
                            key: key.clone(),
                            reason: e.to_string(),
                        });
                        continue;
                    }
                };
                let Some(name) = value.as_str() else {
                    errors.push(KeymapError::NotAString {
                        context: ctx_name.clone(),
                        key: key.clone(),
                    });
                    continue;
                };
                let action = match name.trim() {
                    "" | "none" | "unbind" => None,
                    n => match Action::from_name(n) {
                        Some(a) => Some(a),
                        None => {
                            errors.push(KeymapError::UnknownAction {
                                context: ctx_name.clone(),
                                key: key.clone(),
                                action: n.to_owned(),
                                suggestion: Action::suggest(n),
                            });
                            continue;
                        }
                    },
                };
                entries.push(Override {
                    context,
                    keys,
                    action,
                });
            }
        }
        if errors.is_empty() {
            Ok(KeyOverrides { entries })
        } else {
            Err(errors)
        }
    }
}
