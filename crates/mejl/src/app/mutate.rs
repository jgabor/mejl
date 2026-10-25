//! Turns the UI's mutations into engine commands and overlay snapshots.
//!
//! Single conversations and messages use [`BulkAction::patch`] per email,
//! the same rule the engine uses for `* a`, so an action means the same
//! thing however the mail was selected. Thread targets expand to the
//! thread's messages in the mailbox the user is looking at.
//!
//! A macro ([`ui::Mutation::Batch`]) applies its steps in order to each
//! email's flags and queues the difference, one patch per email, as one
//! entry with one undo. With `* a` its steps become one
//! [`BulkAction::Patch`] that the engine copies to every email.

use std::collections::HashSet;

use mejl_jmap::types::keyword;
use mejl_jmap::{Id, Patch};
use mejl_store::{EmailFlags, EmailPatchTarget, Mutation};
use mejl_sync::{BulkAction, MacroPatch, RoleMailboxes};
use mejl_ui::{self as ui, BatchStep, Keyword, TargetIds};

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
        /// The undo and failure label. `None` makes one from the action,
        /// such as "Archive 12".
        label: Option<String>,
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
        BulkAction::Patch(p) => p.move_to.clone(),
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
        ui::Mutation::Batch {
            target,
            steps,
            label,
        } => return plan_batch(data, &roles, target, steps, label, mark_read_on_archive),
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
        label: None,
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

/// The move a batch step makes, as the bulk action that means the same.
fn step_move(step: &BatchStep, mark_read_on_archive: bool) -> Option<BulkAction> {
    match step {
        BatchStep::Archive if mark_read_on_archive => Some(BulkAction::ArchiveRead),
        BatchStep::Archive => Some(BulkAction::Archive),
        BatchStep::Trash => Some(BulkAction::Trash),
        BatchStep::Spam => Some(BulkAction::Spam),
        BatchStep::MoveTo(id) => Some(BulkAction::MoveTo(Id::from(id.as_str()))),
        BatchStep::Keyword { .. } | BatchStep::SetMailbox { .. } => None,
    }
}

fn keyword_name(keyword: Keyword) -> &'static str {
    match keyword {
        Keyword::Seen => keyword::SEEN,
        Keyword::Flagged => keyword::FLAGGED,
        Keyword::Muted => keyword::MUTED,
    }
}

/// Plans a macro: every step, in order, on each email of the target, as one
/// queued patch with one undo entry.
fn plan_batch(
    data: &StoreData,
    roles: &RoleMailboxes,
    target: &ui::Target,
    steps: &[BatchStep],
    name: &str,
    mark_read_on_archive: bool,
) -> Result<Plan, String> {
    let moving = steps
        .iter()
        .find_map(|s| step_move(s, mark_read_on_archive));
    if let Some(action) = &moving
        && move_target(action, roles).is_none()
    {
        return Err(nothing_moved(data, action, roles));
    }
    let from = target.from_mailbox.as_ref().map(ui::MailboxId::as_str);
    let (groups, unit): (Vec<Group>, &str) = match &target.ids {
        TargetIds::AllInMailbox { mailbox, except } => {
            return plan_batch_bulk(roles, steps, name, mark_read_on_archive, mailbox, except);
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
    // The store refuses an email twice in one patch.
    let mut queued: HashSet<String> = HashSet::new();
    let mut targets: Vec<EmailPatchTarget> = Vec::new();
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
            if !wanted || queued.contains(id.as_str()) {
                continue;
            }
            let start = flags.clone();
            // Found once, before any step, so an earlier label step cannot
            // hide where the mail came from. A move carries it forward.
            let mut source = source_of(flags, from, roles);
            for step in steps {
                let patch = match step {
                    BatchStep::Keyword { keyword, value } => {
                        Some(keyword_patch(*keyword, *value, roles))
                    }
                    BatchStep::SetMailbox { mailbox, member } => {
                        Some(Patch::new().mailbox(mailbox.as_str(), *member))
                    }
                    moving => {
                        // A move needs to know where the mail is; with no
                        // source it is left out and the other steps apply.
                        let action = step_move(moving, mark_read_on_archive);
                        match (action, &source) {
                            (Some(action), Some(src)) => {
                                let patch = action.patch(src, roles);
                                if patch.is_some() {
                                    source = move_target(&action, roles);
                                }
                                patch
                            }
                            _ => None,
                        }
                    }
                };
                if let Some(patch) = patch {
                    flags.apply(&patch);
                }
            }
            let patch = start.diff(flags);
            if patch.is_empty() {
                continue;
            }
            queued.insert(id.clone());
            targets.push(EmailPatchTarget::new(id.as_str(), patch));
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
    if targets.is_empty() {
        return Err(match &moving {
            Some(action) if steps.len() == 1 => nothing_moved(data, action, roles),
            _ => format!("{name}: nothing to change"),
        });
    }
    Ok(Plan::Queue {
        mutation: Mutation::EmailPatch(targets),
        label: format!("Run {name} on {}", plural(count, unit)),
        undoable: true,
        snapshots,
    })
}

/// A macro's steps as one patch for every email of a mailbox, from `* a`.
/// It means the same as [`plan_batch`] for mail in that mailbox.
fn plan_batch_bulk(
    roles: &RoleMailboxes,
    steps: &[BatchStep],
    name: &str,
    mark_read_on_archive: bool,
    mailbox: &ui::MailboxId,
    except: &[ui::ThreadId],
) -> Result<Plan, String> {
    let source = Id::from(mailbox.as_str());
    let p = macro_patch(steps, &source, roles, mark_read_on_archive);
    if BulkAction::Patch(p.clone()).patch(&source, roles).is_none() {
        return Err(format!("{name}: nothing to change"));
    }
    let keyword_value = |k: &str| p.keywords.iter().find(|(n, _)| n == k).map(|(_, v)| *v);
    let target = p.move_to.as_ref().map(|t| t.as_str().to_owned());
    // The overlay shows the move, read state and pins in the same frame.
    // Mute and labels have no bulk overlay: they show once the store has
    // the change, one round trip through the store thread later.
    let overlay = BulkOverlay {
        mailbox: mailbox.as_str().to_owned(),
        except: except.iter().map(|t| t.as_str().to_owned()).collect(),
        hide: target.is_some() || p.labels.contains(&(source.clone(), false)),
        target,
        seen: keyword_value(keyword::SEEN),
        flagged: keyword_value(keyword::FLAGGED),
    };
    Ok(Plan::Bulk {
        mailbox: source.clone(),
        except: except.iter().map(|t| Id::from(t.as_str())).collect(),
        label: Some(format!("Run {name} on the whole mailbox")),
        overlay,
        action: BulkAction::Patch(p),
    })
}

/// Folds a macro's steps into one [`MacroPatch`] for mail in `source`,
/// keeping the result of applying them one by one.
fn macro_patch(
    steps: &[BatchStep],
    source: &Id,
    roles: &RoleMailboxes,
    mark_read_on_archive: bool,
) -> MacroPatch {
    let mut out = MacroPatch::default();
    for step in steps {
        match step {
            BatchStep::Keyword { keyword, value } => out.keyword(keyword_name(*keyword), *value),
            BatchStep::SetMailbox { mailbox, member } => {
                out.label(Id::from(mailbox.as_str()), *member)
            }
            moving => {
                let Some(action) = step_move(moving, mark_read_on_archive) else {
                    continue;
                };
                // A move into the mailbox the mail is in does nothing, as
                // `BulkAction::patch` decides for one email.
                let Some(dest) = move_target(&action, roles).filter(|t| t != source) else {
                    continue;
                };
                out.move_from(source, dest);
                match action {
                    BulkAction::ArchiveRead => out.keyword(keyword::SEEN, true),
                    BulkAction::Spam => {
                        out.keyword(keyword::JUNK, true);
                        out.keyword(keyword::NOT_JUNK, false);
                    }
                    _ => {}
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use jiff::tz::TimeZone;
    use mejl_jmap::method::GetResponse;
    use mejl_jmap::types::{Email, Mailbox};
    use mejl_store::{Store, StoreOptions};
    use serde_json::{Value, json};

    use super::*;

    const INBOX: &str = "mb-inbox";
    const ARCHIVE: &str = "mb-archive";
    const RECEIPTS: &str = "mb-rc";

    fn mailbox(id: &str, name: &str, role: Option<&str>) -> Mailbox {
        serde_json::from_value(json!({
            "id": id, "name": name, "role": role, "sortOrder": 10,
            "totalEmails": 0, "unreadEmails": 0,
            "totalThreads": 0, "unreadThreads": 0, "isSubscribed": true
        }))
        .unwrap()
    }

    fn email(id: &str, thread: &str, mailbox: &str, keywords: Value, day: u32) -> Email {
        serde_json::from_value(json!({
            "id": id,
            "threadId": thread,
            "mailboxIds": {mailbox: true},
            "keywords": keywords,
            "receivedAt": format!("2026-10-{day:02}T09:00:00Z"),
            "size": 1000,
            "from": [{"name": "Sender", "email": "sender@example.org"}],
            "to": [{"name": "Mira Holt", "email": "mira@example.org"}],
            "subject": format!("Subject {thread}"),
            "preview": "",
            "hasAttachment": false,
        }))
        .unwrap()
    }

    /// t1 has e1 (Inbox, unread, pinned), e2 (Inbox, read) and e3 (only in
    /// Archive). t2 has e4 (Inbox, unread).
    fn setup() -> (tempfile::TempDir, Store, StoreData) {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("acct"), StoreOptions::default()).unwrap();
        let mailboxes: GetResponse<Mailbox> = serde_json::from_value(json!({
            "accountId": "A1",
            "state": "m1",
            "list": [
                mailbox(INBOX, "Inbox", Some("inbox")),
                mailbox(ARCHIVE, "Archive", Some("archive")),
                mailbox(RECEIPTS, "Receipts", None),
            ],
            "notFound": []
        }))
        .unwrap();
        let mut batch = store.batch().unwrap();
        batch.replace_mailboxes(&mailboxes).unwrap();
        let emails = vec![
            email("e1", "t1", INBOX, json!({"$flagged": true}), 1),
            email("e2", "t1", INBOX, json!({"$seen": true}), 2),
            email("e3", "t1", ARCHIVE, json!({}), 3),
            email("e4", "t2", INBOX, json!({}), 4),
        ];
        batch.upsert_emails(&emails).unwrap();
        batch.commit().unwrap();
        let data = StoreData::new(store.reader().unwrap(), "test", TimeZone::UTC);
        (dir, store, data)
    }

    fn file_receipt() -> Vec<BatchStep> {
        vec![
            BatchStep::Keyword {
                keyword: Keyword::Seen,
                value: true,
            },
            BatchStep::Keyword {
                keyword: Keyword::Flagged,
                value: false,
            },
            BatchStep::MoveTo(ui::MailboxId::new(RECEIPTS)),
        ]
    }

    fn batch(ids: TargetIds, steps: Vec<BatchStep>) -> ui::Mutation {
        ui::Mutation::Batch {
            target: ui::Target {
                ids,
                from_mailbox: Some(ui::MailboxId::new(INBOX)),
            },
            steps,
            label: "File receipt".into(),
        }
    }

    fn threads(ids: &[&str]) -> TargetIds {
        TargetIds::Threads(ids.iter().map(|t| ui::ThreadId::new(*t)).collect())
    }

    fn patches(plan: &Plan) -> Vec<(String, Value)> {
        match plan {
            Plan::Queue {
                mutation: Mutation::EmailPatch(targets),
                ..
            } => targets
                .iter()
                .map(|t| {
                    (
                        t.email_id.as_str().to_owned(),
                        serde_json::to_value(&t.patch).unwrap(),
                    )
                })
                .collect(),
            other => panic!("not one queued patch: {other:?}"),
        }
    }

    #[test]
    fn a_macro_is_one_queued_patch_with_one_undo_label() {
        let (_dir, _store, data) = setup();
        let plan = plan(&data, &batch(threads(&["t1", "t2"]), file_receipt()), false).unwrap();
        let mut got = patches(&plan);
        got.sort_by(|a, b| a.0.cmp(&b.0));
        // Each email once, only the steps that change it, and e3 (not in
        // the Inbox) left alone.
        assert_eq!(
            got,
            vec![
                (
                    "e1".to_owned(),
                    json!({
                        "keywords/$seen": true, "keywords/$flagged": null,
                        "mailboxIds/mb-rc": true, "mailboxIds/mb-inbox": null
                    })
                ),
                (
                    "e2".to_owned(),
                    json!({"mailboxIds/mb-rc": true, "mailboxIds/mb-inbox": null})
                ),
                (
                    "e4".to_owned(),
                    json!({
                        "keywords/$seen": true,
                        "mailboxIds/mb-rc": true, "mailboxIds/mb-inbox": null
                    })
                ),
            ]
        );
        let Plan::Queue {
            label,
            undoable,
            snapshots,
            ..
        } = plan
        else {
            unreachable!()
        };
        assert_eq!(label, "Run File receipt on 2 conversations");
        assert!(undoable);
        assert_eq!(snapshots.len(), 2);
        assert!(snapshots.iter().all(|s| !s.in_mailbox_after(INBOX)));
        assert!(snapshots.iter().all(|s| s.in_mailbox_after(RECEIPTS)));
    }

    #[test]
    fn the_archive_step_follows_mark_read_on_archive() {
        let (_dir, _store, data) = setup();
        let steps = vec![BatchStep::Archive];
        let off = patches(&plan(&data, &batch(threads(&["t2"]), steps.clone()), false).unwrap());
        assert_eq!(
            off,
            vec![(
                "e4".to_owned(),
                json!({"mailboxIds/mb-archive": true, "mailboxIds/mb-inbox": null})
            )]
        );
        let on = patches(&plan(&data, &batch(threads(&["t2"]), steps), true).unwrap());
        assert_eq!(
            on,
            vec![(
                "e4".to_owned(),
                json!({
                    "keywords/$seen": true,
                    "mailboxIds/mb-archive": true, "mailboxIds/mb-inbox": null
                })
            )]
        );
    }

    #[test]
    fn later_steps_win_and_a_move_into_the_same_mailbox_is_left_out() {
        let (_dir, _store, data) = setup();
        let steps = vec![
            BatchStep::Keyword {
                keyword: Keyword::Seen,
                value: true,
            },
            BatchStep::Keyword {
                keyword: Keyword::Seen,
                value: false,
            },
            BatchStep::MoveTo(ui::MailboxId::new(INBOX)),
            BatchStep::Keyword {
                keyword: Keyword::Muted,
                value: true,
            },
        ];
        let got = patches(&plan(&data, &batch(threads(&["t2"]), steps), false).unwrap());
        assert_eq!(
            got,
            vec![("e4".to_owned(), json!({"keywords/$muted": true}))]
        );
        // A macro that only moves the mail where it is changes nothing.
        let err = plan(
            &data,
            &batch(
                threads(&["t2"]),
                vec![BatchStep::MoveTo(ui::MailboxId::new(INBOX))],
            ),
            false,
        )
        .unwrap_err();
        assert!(err.contains("Already in Inbox"), "{err}");
        // A role the account lacks refuses the whole macro.
        let err = plan(
            &data,
            &batch(threads(&["t2"]), {
                let mut s = file_receipt();
                s[2] = BatchStep::Trash;
                s
            }),
            false,
        )
        .unwrap_err();
        assert!(err.contains("no Trash mailbox"), "{err}");
    }

    #[test]
    fn select_all_builds_one_bulk_patch_that_means_the_same() {
        let (_dir, _store, data) = setup();
        let all = TargetIds::AllInMailbox {
            mailbox: ui::MailboxId::new(INBOX),
            except: vec![],
        };
        let plan_all = plan(&data, &batch(all, file_receipt()), false).unwrap();
        let Plan::Bulk {
            mailbox,
            action,
            label,
            overlay,
            ..
        } = plan_all
        else {
            panic!("not a bulk plan: {plan_all:?}");
        };
        assert_eq!(mailbox.as_str(), INBOX);
        assert_eq!(
            label.as_deref(),
            Some("Run File receipt on the whole mailbox")
        );
        assert_eq!(
            action,
            BulkAction::Patch(MacroPatch {
                keywords: vec![("$seen".into(), true), ("$flagged".into(), false)],
                move_to: Some(RECEIPTS.into()),
                labels: vec![],
            })
        );
        assert!(overlay.hide);
        assert_eq!(overlay.target.as_deref(), Some(RECEIPTS));
        assert_eq!((overlay.seen, overlay.flagged), (Some(true), Some(false)));
        // The bulk patch on each Inbox email gives what the one by one
        // plan gives.
        let roles = data.roles();
        let bulk = action.patch(&Id::from(INBOX), &roles).unwrap();
        let one_by_one =
            plan(&data, &batch(threads(&["t1", "t2"]), file_receipt()), false).unwrap();
        let Plan::Queue { snapshots, .. } = one_by_one else {
            unreachable!()
        };
        for s in &snapshots {
            for ((id, before), (_, after)) in s.before.iter().zip(&s.after) {
                if before.in_mailbox(INBOX) {
                    assert_eq!(&before.applied(&bulk), after, "{id}");
                }
            }
        }
    }

    #[test]
    fn a_label_step_before_the_move_does_not_lose_the_source() {
        let (_dir, _store, data) = setup();
        let steps = vec![
            BatchStep::SetMailbox {
                mailbox: ui::MailboxId::new(INBOX),
                member: false,
            },
            BatchStep::Archive,
        ];
        let got = patches(&plan(&data, &batch(threads(&["t2"]), steps.clone()), false).unwrap());
        assert_eq!(
            got,
            vec![(
                "e4".to_owned(),
                json!({"mailboxIds/mb-archive": true, "mailboxIds/mb-inbox": null})
            )]
        );
        let all = TargetIds::AllInMailbox {
            mailbox: ui::MailboxId::new(INBOX),
            except: vec![],
        };
        let Plan::Bulk { action, .. } = plan(&data, &batch(all, steps), false).unwrap() else {
            panic!("not a bulk plan");
        };
        assert_eq!(
            serde_json::to_value(action.patch(&Id::from(INBOX), &data.roles()).unwrap()).unwrap(),
            json!({"mailboxIds/mb-archive": true, "mailboxIds/mb-inbox": null})
        );
    }
}
