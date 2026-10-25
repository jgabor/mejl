//! Turns the UI's mutations into engine commands and overlay snapshots.
//!
//! Single conversations and messages use [`BulkAction::patch`] per email,
//! the same rule the engine uses for `* a`, so an action means the same
//! thing however the mail was selected. Thread targets expand to the
//! thread's messages in the mailbox the user is looking at.

use mejl_jmap::{Id, Patch};
use mejl_store::{EmailFlags, EmailPatchTarget, Mutation};
use mejl_sync::{BulkAction, RoleMailboxes};
use mejl_ui::{self as ui, Keyword, TargetIds};

use super::data::{StoreData, by_thread};
use super::overlay::{BulkOverlay, ThreadSnapshot};

/// What a UI mutation becomes.
#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    /// Queue one store mutation, with snapshots for the overlay.
    Queue {
        /// The store mutation.
        mutation: Mutation,
        /// The undo and failure label.
        label: String,
        /// Whether undo can revert it.
        undoable: bool,
        /// Conversations before and after, for same-frame feedback.
        snapshots: Vec<ThreadSnapshot>,
    },
    /// An action on every cached conversation of a mailbox.
    Bulk {
        /// The mailbox.
        mailbox: Id,
        /// Conversations left out.
        except: Vec<Id>,
        /// The action.
        action: BulkAction,
        /// Same-frame feedback.
        overlay: BulkOverlay,
    },
    /// Undo the newest undoable action.
    Undo,
    /// Redo the newest undone action.
    Redo,
}

/// A thread and, for message targets, the messages of it to change. `None`
/// means the thread's messages in the mailbox being viewed.
type Group = (String, Option<Vec<String>>);

enum Act {
    Move(BulkAction),
    Patch(Patch),
    Destroy,
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

fn keyword_patch(keyword: Keyword, value: bool, roles: &RoleMailboxes) -> Patch {
    let any = Id::from("");
    match keyword {
        Keyword::Seen => BulkAction::SetSeen(value).patch(&any, roles),
        Keyword::Flagged => BulkAction::SetFlagged(value).patch(&any, roles),
        Keyword::Muted => None,
    }
    .unwrap_or_else(|| Patch::new().keyword("$muted", value))
}

fn keyword_verb(keyword: Keyword, value: bool) -> &'static str {
    match (keyword, value) {
        (Keyword::Seen, true) => "Mark read",
        (Keyword::Seen, false) => "Mark unread",
        (Keyword::Flagged, true) => "Pin",
        (Keyword::Flagged, false) => "Unpin",
        (Keyword::Muted, true) => "Mute",
        (Keyword::Muted, false) => "Unmute",
    }
}

fn mailbox_name(data: &StoreData, id: Option<&Id>) -> Option<String> {
    let id = id?;
    data.ok("mailboxes", data.reader().mailboxes())
        .into_iter()
        .find(|m| &m.id == id)
        .map(|m| m.name)
}

fn move_target(action: &BulkAction, roles: &RoleMailboxes) -> Option<Id> {
    match action {
        BulkAction::Archive | BulkAction::ArchiveRead => roles.archive.clone(),
        BulkAction::Trash => roles.trash.clone(),
        BulkAction::Spam => roles.junk.clone(),
        BulkAction::MoveTo(id) => Some(id.clone()),
        BulkAction::SetSeen(_) | BulkAction::SetFlagged(_) => None,
    }
}

/// Why a move changed nothing, in words.
fn nothing_moved(data: &StoreData, action: &BulkAction, roles: &RoleMailboxes) -> String {
    let fallback = match action {
        BulkAction::Archive | BulkAction::ArchiveRead => "Archive",
        BulkAction::Trash => "Trash",
        BulkAction::Spam => "Spam",
        _ => "that mailbox",
    };
    match move_target(action, roles) {
        None => format!("This account has no {fallback} mailbox"),
        Some(id) => format!(
            "Already in {}",
            mailbox_name(data, Some(&id)).unwrap_or_else(|| fallback.to_owned())
        ),
    }
}

/// The mailbox a move takes an email out of: the one the user is looking
/// at, or with none (search results), the Inbox, or the only mailbox the
/// email is in.
fn source_of(flags: &EmailFlags, from: Option<&str>, roles: &RoleMailboxes) -> Option<Id> {
    if let Some(m) = from {
        return flags.in_mailbox(m).then(|| Id::from(m));
    }
    if let Some(inbox) = roles
        .inbox
        .as_ref()
        .filter(|i| flags.in_mailbox(i.as_str()))
    {
        return Some(inbox.clone());
    }
    let mut it = flags.mailbox_ids.iter();
    match (it.next(), it.next()) {
        (Some(only), None) => Some(Id::from(only.as_str())),
        _ => None,
    }
}

/// Plans a UI mutation against the current local state.
pub fn plan(
    data: &StoreData,
    m: &ui::Mutation,
    mark_read_on_archive: bool,
) -> Result<Plan, String> {
    let roles = data.roles();
    let (target, act, verb) = match m {
        ui::Mutation::Undo => return Ok(Plan::Undo),
        ui::Mutation::Redo => return Ok(Plan::Redo),
        ui::Mutation::Archive(t) => (
            t,
            Act::Move(if mark_read_on_archive {
                BulkAction::ArchiveRead
            } else {
                BulkAction::Archive
            }),
            "Archive",
        ),
        ui::Mutation::Trash(t) => (t, Act::Move(BulkAction::Trash), "Trash"),
        ui::Mutation::ReportSpam(t) => (t, Act::Move(BulkAction::Spam), "Report spam"),
        ui::Mutation::MoveTo { target, mailbox } => (
            target,
            Act::Move(BulkAction::MoveTo(Id::from(mailbox.as_str()))),
            "Move",
        ),
        ui::Mutation::SetKeyword {
            target,
            keyword,
            value,
        } => (
            target,
            Act::Patch(keyword_patch(*keyword, *value, &roles)),
            keyword_verb(*keyword, *value),
        ),
        ui::Mutation::SetMailbox {
            target,
            mailbox,
            member,
        } => (
            target,
            Act::Patch(Patch::new().mailbox(mailbox.as_str(), *member)),
            if *member {
                "Label"
            } else {
                "Remove label from"
            },
        ),
        ui::Mutation::DeletePermanently(t) => (t, Act::Destroy, "Delete"),
    };
    let from = target.from_mailbox.as_ref().map(ui::MailboxId::as_str);
    let (groups, unit): (Vec<Group>, &str) = match &target.ids {
        TargetIds::AllInMailbox { mailbox, except } => {
            return plan_bulk(data, &roles, m, act, mailbox, except);
        }
        TargetIds::Threads(threads) => (
            threads
                .iter()
                .map(|t| (t.as_str().to_owned(), None))
                .collect(),
            "conversation",
        ),
        TargetIds::Emails(emails) => (
            by_thread(data, emails)
                .into_iter()
                .map(|(t, ids)| (t, Some(ids)))
                .collect(),
            "message",
        ),
    };
    let count = match &target.ids {
        TargetIds::Threads(t) => t.len(),
        TargetIds::Emails(e) => e.len(),
        TargetIds::AllInMailbox { .. } => 0,
    };
    let mut targets: Vec<EmailPatchTarget> = Vec::new();
    let mut destroy: Vec<Id> = Vec::new();
    let mut snapshots = Vec::new();
    for (thread, only) in groups {
        let before = data.thread_flags(&thread);
        let mut after = before.clone();
        let mut touched = false;
        for (id, flags) in &mut after {
            let wanted = match &only {
                Some(ids) => ids.contains(id),
                // A conversation acts on its messages in this mailbox.
                None => from.is_none_or(|m| flags.in_mailbox(m)),
            };
            if !wanted {
                continue;
            }
            match &act {
                Act::Move(action) => {
                    let Some(source) = source_of(flags, from, &roles) else {
                        continue;
                    };
                    let Some(patch) = action.patch(&source, &roles) else {
                        continue;
                    };
                    flags.apply(&patch);
                    targets.push(EmailPatchTarget::new(id.as_str(), patch));
                }
                Act::Patch(patch) => {
                    flags.apply(patch);
                    targets.push(EmailPatchTarget::new(id.as_str(), patch.clone()));
                }
                Act::Destroy => {
                    *flags = EmailFlags::default();
                    destroy.push(Id::from(id.as_str()));
                }
            }
            touched = true;
        }
        if touched {
            snapshots.push(ThreadSnapshot {
                thread,
                before,
                after,
            });
        }
    }
    let label = format!("{verb} {}", plural(count, unit));
    match act {
        Act::Destroy if !destroy.is_empty() => Ok(Plan::Queue {
            mutation: Mutation::EmailDestroy(destroy),
            label,
            // The store refuses undo for destroys: the server cannot bring
            // the mail back.
            undoable: false,
            snapshots,
        }),
        Act::Move(action) if targets.is_empty() => Err(nothing_moved(data, &action, &roles)),
        _ if targets.is_empty() => Err("Nothing to change".to_owned()),
        _ => Ok(Plan::Queue {
            mutation: Mutation::EmailPatch(targets),
            label,
            undoable: true,
            snapshots,
        }),
    }
}

fn plan_bulk(
    data: &StoreData,
    roles: &RoleMailboxes,
    m: &ui::Mutation,
    act: Act,
    mailbox: &ui::MailboxId,
    except: &[ui::ThreadId],
) -> Result<Plan, String> {
    let unsupported =
        || "This does not work on a whole mailbox yet; select the conversations instead".to_owned();
    let (action, seen, flagged) = match (m, act) {
        (_, Act::Move(action)) => {
            let seen = matches!(action, BulkAction::ArchiveRead).then_some(true);
            (action, seen, None)
        }
        (
            ui::Mutation::SetKeyword {
                keyword: Keyword::Seen,
                value,
                ..
            },
            _,
        ) => (BulkAction::SetSeen(*value), Some(*value), None),
        (
            ui::Mutation::SetKeyword {
                keyword: Keyword::Flagged,
                value,
                ..
            },
            _,
        ) => (BulkAction::SetFlagged(*value), None, Some(*value)),
        _ => return Err(unsupported()),
    };
    let id = Id::from(mailbox.as_str());
    if action.patch(&id, roles).is_none() {
        return Err(nothing_moved(data, &action, roles));
    }
    let target = move_target(&action, roles).map(|t| t.as_str().to_owned());
    Ok(Plan::Bulk {
        mailbox: id,
        except: except.iter().map(|t| Id::from(t.as_str())).collect(),
        overlay: BulkOverlay {
            mailbox: mailbox.as_str().to_owned(),
            except: except.iter().map(|t| t.as_str().to_owned()).collect(),
            hide: target.is_some(),
            target,
            seen,
            flagged,
        },
        action,
    })
}
