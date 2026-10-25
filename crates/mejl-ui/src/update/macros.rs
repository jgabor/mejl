//! Running macros in `update`.
//!
//! A macro acts on whole conversations: the selection, else the one under
//! the cursor, else everything after `* a`, in the reader too. Its folders
//! are looked up once, before anything runs, so a missing folder changes
//! nothing. All steps go to the shell as one [`Mutation::Batch`], which
//! makes one undo entry.

use super::plural;
use crate::data::{MailData, MailboxId, MailboxRole};
use crate::event::{BatchStep, Cmd, Keyword, Mutation, ToastLevel};
use crate::keymap::Binding;
use crate::macros::{FolderRef, MacroId, Step};
use crate::state::AppState;

impl AppState {
    /// Runs a macro on the selection or the conversation under the cursor.
    pub(crate) fn run_macro(&mut self, _data: &dyn MailData, id: MacroId, cmds: &mut Vec<Cmd>) {
        self.last_action = Some((Binding::Macro(id), None));
        let Some(m) = self.config.macros.get(usize::from(id.0)).cloned() else {
            self.notify("That macro is gone", ToastLevel::Warning, false, cmds);
            return;
        };
        let Some((target, n)) = self.target() else {
            self.notify(
                format!("{}: nothing to change", m.label),
                ToastLevel::Info,
                false,
                cmds,
            );
            return;
        };
        let from = target.from_mailbox.clone();
        let mut steps = Vec::with_capacity(m.steps.len());
        // Moves into the mailbox the mail is already in are left out, so
        // the rest of the macro still runs there.
        let mut already = None;
        for step in &m.steps {
            let resolved = match step {
                Step::MarkRead => keyword(Keyword::Seen, true),
                Step::MarkUnread => keyword(Keyword::Seen, false),
                Step::Pin => keyword(Keyword::Flagged, true),
                Step::Unpin => keyword(Keyword::Flagged, false),
                Step::Mute => keyword(Keyword::Muted, true),
                Step::Archive | Step::Trash | Step::Spam => {
                    let (role, step, name) = match step {
                        Step::Archive => (MailboxRole::Archive, BatchStep::Archive, "Archive"),
                        Step::Trash => (MailboxRole::Trash, BatchStep::Trash, "Trash"),
                        _ => (MailboxRole::Junk, BatchStep::Spam, "Spam"),
                    };
                    let Some(dest) = self.mailbox_id(role) else {
                        self.missing(&m.label, &format!("{name} folder"), cmds);
                        return;
                    };
                    if from.as_ref() == Some(&dest) {
                        already = Some(name.to_owned());
                        continue;
                    }
                    step
                }
                Step::Move(f) => {
                    let Some(dest) = self.resolve_folder(f) else {
                        self.missing(&m.label, &format!("folder {}", f.path), cmds);
                        return;
                    };
                    if from.as_ref() == Some(&dest) {
                        already = Some(self.mailbox_path(&dest));
                        continue;
                    }
                    BatchStep::MoveTo(dest)
                }
                Step::AddLabel(f) | Step::RemoveLabel(f) => {
                    let Some(mailbox) = self.resolve_folder(f) else {
                        self.missing(&m.label, &format!("folder {}", f.path), cmds);
                        return;
                    };
                    BatchStep::SetMailbox {
                        mailbox,
                        member: matches!(step, Step::AddLabel(_)),
                    }
                }
            };
            steps.push(resolved);
        }
        if steps.is_empty() {
            let place = already.unwrap_or_else(|| "that folder".to_owned());
            self.notify(
                format!("{}: already in {place}", m.label),
                ToastLevel::Info,
                false,
                cmds,
            );
            return;
        }
        cmds.push(Cmd::Mutate(Mutation::Batch {
            target,
            steps,
            label: m.label.clone(),
        }));
        self.list.selection.clear();
        self.notify(
            format!("{}: {}", m.label, plural(n, "conversation")),
            ToastLevel::Success,
            true,
            cmds,
        );
    }

    fn missing(&mut self, label: &str, what: &str, cmds: &mut Vec<Cmd>) {
        self.notify(
            format!("{label}: no {what}"),
            ToastLevel::Error,
            false,
            cmds,
        );
    }

    /// The mailbox a step names: by id while that id exists, else by full
    /// path, ignoring case.
    pub(crate) fn resolve_folder(&self, folder: &FolderRef) -> Option<MailboxId> {
        if let Some(id) = &folder.id
            && self.mailboxes.iter().any(|m| &m.id == id)
        {
            return Some(id.clone());
        }
        let want = normalize(&folder.path);
        if want.is_empty() {
            return None;
        }
        self.mailboxes
            .iter()
            .find(|m| normalize(&self.mailbox_path(&m.id)) == want)
            .map(|m| m.id.clone())
    }
}

fn keyword(keyword: Keyword, value: bool) -> BatchStep {
    BatchStep::Keyword { keyword, value }
}

/// A path for comparing: lowercase, with no empty levels and no spaces
/// around each level.
fn normalize(path: &str) -> String {
    path.split('/')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("/")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::event::{Target, TargetIds};
    use crate::macros::Macro;
    use crate::{ManualClock, MemoryData, UiConfig};

    fn state_with(steps: Vec<Step>) -> (AppState, MemoryData) {
        let now = "2026-10-23T14:02:31Z".parse().unwrap();
        let data = MemoryData::demo(now);
        let config = UiConfig {
            macros: vec![Macro {
                id: "file-receipt".into(),
                label: "File receipt".into(),
                keys: Vec::new(),
                steps,
            }],
            ..UiConfig::default()
        };
        let mut state = AppState::new(config, Arc::new(ManualClock::new(now)), (160, 40));
        state.start(&data);
        (state, data)
    }

    fn run(state: &mut AppState, data: &MemoryData) -> Vec<Cmd> {
        let mut cmds = Vec::new();
        state.run_macro(data, MacroId(0), &mut cmds);
        cmds
    }

    fn batches(cmds: &[Cmd]) -> Vec<&Mutation> {
        cmds.iter()
            .filter_map(|c| match c {
                Cmd::Mutate(m) => Some(m),
                _ => None,
            })
            .collect()
    }

    fn toast(state: &AppState) -> (String, ToastLevel, bool) {
        let t = state.toast().expect("a toast");
        (t.text.clone(), t.level, t.undo)
    }

    #[test]
    fn one_batch_with_folders_resolved_and_one_toast() {
        let (mut state, data) = state_with(vec![
            Step::MarkRead,
            Step::Unpin,
            // A stale id falls back to the path, in any case.
            Step::Move(FolderRef {
                path: "projects/HALL repairs".into(),
                id: Some("gone".into()),
            }),
            Step::AddLabel(FolderRef {
                path: "whatever".into(),
                id: Some("mb-receipts".into()),
            }),
        ]);
        let thread = state.cursor_thread().expect("a cursor row");
        let cmds = run(&mut state, &data);
        assert_eq!(
            batches(&cmds),
            vec![&Mutation::Batch {
                target: Target {
                    ids: TargetIds::Threads(vec![thread]),
                    from_mailbox: Some("mb-inbox".into()),
                },
                steps: vec![
                    keyword(Keyword::Seen, true),
                    keyword(Keyword::Flagged, false),
                    BatchStep::MoveTo("mb-hall".into()),
                    BatchStep::SetMailbox {
                        mailbox: "mb-receipts".into(),
                        member: true,
                    },
                ],
                label: "File receipt".into(),
            }]
        );
        assert_eq!(
            toast(&state),
            (
                "File receipt: 1 conversation".into(),
                ToastLevel::Success,
                true
            )
        );
        assert_eq!(state.last_action, Some((Binding::Macro(MacroId(0)), None)));
    }

    #[test]
    fn a_missing_folder_runs_nothing() {
        let (mut state, data) = state_with(vec![
            Step::MarkRead,
            Step::Move(FolderRef::path("Receipts/2026")),
        ]);
        let cmds = run(&mut state, &data);
        assert!(batches(&cmds).is_empty());
        assert_eq!(
            toast(&state),
            (
                "File receipt: no folder Receipts/2026".into(),
                ToastLevel::Error,
                false
            )
        );
    }

    #[test]
    fn a_move_into_the_current_mailbox_is_left_out() {
        let (mut state, data) = state_with(vec![Step::Pin, Step::Move(FolderRef::path("inbox"))]);
        let cmds = run(&mut state, &data);
        match batches(&cmds).as_slice() {
            [Mutation::Batch { steps, .. }] => {
                assert_eq!(steps, &vec![keyword(Keyword::Flagged, true)])
            }
            other => panic!("{other:?}"),
        }
        let (mut state, data) = state_with(vec![Step::Move(FolderRef::path("Inbox"))]);
        let cmds = run(&mut state, &data);
        assert!(batches(&cmds).is_empty());
        assert_eq!(toast(&state).0, "File receipt: already in Inbox");
    }

    #[test]
    fn the_selection_is_the_target_and_is_cleared_once() {
        let (mut state, data) = state_with(vec![Step::Archive]);
        let mut cmds = Vec::new();
        state.apply(&data, crate::action::Action::ToggleSelect, None, &mut cmds);
        state.apply(&data, crate::action::Action::Next, None, &mut cmds);
        state.apply(&data, crate::action::Action::ToggleSelect, None, &mut cmds);
        let cmds = run(&mut state, &data);
        match batches(&cmds).as_slice() {
            [Mutation::Batch { target, steps, .. }] => {
                assert!(matches!(&target.ids, TargetIds::Threads(t) if t.len() == 2));
                assert_eq!(steps, &vec![BatchStep::Archive]);
            }
            other => panic!("{other:?}"),
        }
        assert!(state.list.selection.is_empty(state.list.total));
        assert_eq!(toast(&state).0, "File receipt: 2 conversations");
    }

    #[test]
    fn repeat_follows_the_macro_when_the_list_changes() {
        let (mut state, data) = state_with(vec![Step::MarkRead]);
        run(&mut state, &data);
        let named = |id: &str| Macro {
            id: id.into(),
            label: id.into(),
            keys: Vec::new(),
            steps: vec![Step::Trash],
        };
        // A macro that sorts first moves "file-receipt" to place 1.
        let mut config = state.config.clone();
        config.macros.insert(0, named("archive-news"));
        state.apply_config(config);
        assert_eq!(state.last_action, Some((Binding::Macro(MacroId(1)), None)));
        // Once it is gone, Repeat forgets it instead of running another.
        let mut config = state.config.clone();
        config.macros.remove(1);
        state.apply_config(config);
        assert_eq!(state.last_action, None);
    }
}
