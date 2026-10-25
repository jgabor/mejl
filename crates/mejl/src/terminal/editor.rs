//! Handing text to `$VISUAL` or `$EDITOR`.
//!
//! [`edit_text`] writes the text to a private temporary file, runs the
//! editor on it in the foreground and reads the result back. It does not
//! touch the terminal; wrap it in [`super::suspend_for`] (with the input
//! thread paused) when the TUI is running.

use std::io::Write;
use std::process::Command;

use crate::env::{Env, var};

/// The editor to run: a program and its leading arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorCommand {
    /// The program.
    pub program: String,
    /// Arguments before the file name, such as `--wait`.
    pub args: Vec<String>,
}

/// The editor used when neither `VISUAL` nor `EDITOR` is set.
pub const FALLBACK_EDITOR: &str = if cfg!(windows) { "notepad" } else { "vi" };

impl EditorCommand {
    /// `VISUAL`, then `EDITOR`, then [`FALLBACK_EDITOR`]. The value is
    /// split like a shell would split simple words, so `code --wait` and
    /// quoted paths work. A value that cannot be split is skipped.
    pub fn from_env(env: &Env) -> EditorCommand {
        [var::VISUAL, var::EDITOR]
            .iter()
            .filter_map(|name| env.non_empty(name))
            .find_map(EditorCommand::parse)
            .unwrap_or_else(|| EditorCommand {
                program: FALLBACK_EDITOR.to_owned(),
                args: Vec::new(),
            })
    }

    /// Splits a command line into a program and arguments.
    pub fn parse(line: &str) -> Option<EditorCommand> {
        let mut words = split_words(line)?;
        if words.is_empty() {
            return None;
        }
        let program = words.remove(0);
        Some(EditorCommand {
            program,
            args: words,
        })
    }
}

/// Splits on whitespace, honouring single quotes, double quotes and
/// backslash escapes outside single quotes. Returns `None` on an
/// unclosed quote.
fn split_words(line: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => word.push(c),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => word.push(chars.next()?),
                        c => word.push(c),
                    }
                }
            }
            '\\' => {
                in_word = true;
                word.push(chars.next()?);
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Some(words)
}

/// Why editing failed.
#[derive(Debug, thiserror::Error)]
pub enum EditorError {
    /// The temporary file could not be written or read.
    #[error("cannot use a temporary file for the editor: {0}")]
    TempFile(std::io::Error),
    /// The editor could not be started.
    #[error("cannot start the editor {program:?}: {source}")]
    Start {
        /// What was run.
        program: String,
        /// The cause.
        source: std::io::Error,
    },
    /// The editor exited with an error, which usually means the user
    /// quit without saving (for example `:cq` in vim).
    #[error("the editor exited with {0}; the text was not changed")]
    Failed(std::process::ExitStatus),
    /// The saved file is not UTF-8.
    #[error("the edited text is not UTF-8")]
    NotUtf8,
}

/// Runs the editor on `initial` and returns the saved text. `suffix` names
/// the file type for the editor's syntax highlighting, such as `.md` or
/// `.eml`. The file is created with mode 0600 and removed afterwards.
pub fn edit_text(cmd: &EditorCommand, initial: &str, suffix: &str) -> Result<String, EditorError> {
    let mut file = tempfile::Builder::new()
        .prefix("mejl-")
        .suffix(suffix)
        .tempfile()
        .map_err(EditorError::TempFile)?;
    file.write_all(initial.as_bytes())
        .and_then(|()| file.flush())
        .map_err(EditorError::TempFile)?;
    let status = Command::new(&cmd.program)
        .args(&cmd.args)
        .arg(file.path())
        .status()
        .map_err(|source| EditorError::Start {
            program: cmd.program.clone(),
            source,
        })?;
    if !status.success() {
        return Err(EditorError::Failed(status));
    }
    // Editors often replace the file rather than rewrite it, so read it by
    // path, not through the open handle.
    let bytes = std::fs::read(file.path()).map_err(EditorError::TempFile)?;
    String::from_utf8(bytes).map_err(|_| EditorError::NotUtf8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visual_wins_over_editor() {
        let env = Env::from_pairs([("VISUAL", "hx"), ("EDITOR", "nano")]);
        assert_eq!(EditorCommand::from_env(&env).program, "hx");
        let env = Env::from_pairs([("VISUAL", "  "), ("EDITOR", "nano -w")]);
        let cmd = EditorCommand::from_env(&env);
        assert_eq!(cmd.program, "nano");
        assert_eq!(cmd.args, ["-w"]);
        assert_eq!(
            EditorCommand::from_env(&Env::default()).program,
            FALLBACK_EDITOR
        );
    }

    #[test]
    fn unclosed_quote_falls_back() {
        let env = Env::from_pairs([("VISUAL", "\"broken"), ("EDITOR", "nano")]);
        assert_eq!(EditorCommand::from_env(&env).program, "nano");
    }

    #[test]
    fn words_split_like_a_shell() {
        let cmd = EditorCommand::parse(r#"'/opt/My Editor/bin/ed' --wait "a b" c\ d"#).unwrap();
        assert_eq!(cmd.program, "/opt/My Editor/bin/ed");
        assert_eq!(cmd.args, ["--wait", "a b", "c d"]);
        assert_eq!(EditorCommand::parse("   "), None);
        assert_eq!(EditorCommand::parse("vim\\"), None);
        assert_eq!(EditorCommand::parse("''").unwrap().program, "");
    }

    /// A fake editor run through `sh`. Executing a freshly written script
    /// directly can fail with "text file busy" when another test forks at
    /// the same moment and the child briefly holds the write descriptor.
    #[cfg(unix)]
    fn script(dir: &std::path::Path, body: &str) -> EditorCommand {
        let path = dir.join(format!(
            "fake-editor-{}.sh",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, format!("{body}\n")).unwrap();
        EditorCommand {
            program: "sh".into(),
            args: vec![path.display().to_string()],
        }
    }

    #[cfg(unix)]
    #[test]
    fn edited_text_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        // Appends a line, and checks the file was private.
        let cmd = script(
            dir.path(),
            r#"case "$(stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1")" in 600) ;; *) exit 9;; esac
printf 'added\n' >> "$1""#,
        );
        let out = edit_text(&cmd, "hello\n", ".md").unwrap();
        assert_eq!(out, "hello\nadded\n");
    }

    #[cfg(unix)]
    #[test]
    fn editor_that_replaces_the_file_works() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = script(dir.path(), r#"printf 'new' > "$1.tmp" && mv "$1.tmp" "$1""#);
        assert_eq!(edit_text(&cmd, "old", ".txt").unwrap(), "new");
    }

    #[cfg(unix)]
    #[test]
    fn editor_failures_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = script(dir.path(), "exit 1");
        assert!(matches!(
            edit_text(&cmd, "x", ".md"),
            Err(EditorError::Failed(_))
        ));
        let cmd = script(dir.path(), r#"printf '\377' > "$1""#);
        assert!(matches!(
            edit_text(&cmd, "x", ".md"),
            Err(EditorError::NotUtf8)
        ));
        let missing = EditorCommand {
            program: dir.path().join("no-such-editor").display().to_string(),
            args: Vec::new(),
        };
        assert!(matches!(
            edit_text(&missing, "x", ".md"),
            Err(EditorError::Start { .. })
        ));
    }
}
