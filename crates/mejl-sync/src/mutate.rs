//! Mutation commands: they run on the store thread and answer with the
//! caller's token once the overlay is in the store.

use std::sync::Arc;

use mejl_jmap::Id;
use mejl_store::{EmailPatchTarget, EnqueueOptions, Enqueued, Mutation};

use crate::command::{BulkAction, RoleMailboxes};
use crate::error::SyncError;
use crate::event::{DataChanges, SyncEvent};
use crate::shared::Shared;
use crate::store_thread::StoreCtx;

fn stopped(sh: &Shared, token: u64) {
    sh.events.emit(SyncEvent::Rejected {
        token,
        error: "the engine has stopped".into(),
    });
}

/// Queues a mutation and reports [`SyncEvent::Applied`] or
/// [`SyncEvent::Rejected`].
pub(crate) fn enqueue(
    sh: &Arc<Shared>,
    token: u64,
    mutation: Mutation,
    label: Option<String>,
    undoable: bool,
) {
    let posted = sh.store.post(move |ctx| {
        let result = enqueue_on_store(ctx, mutation, label.as_deref(), undoable);
        answer(ctx, token, result);
    });
    if !posted {
        stopped(sh, token);
    }
}

/// Expands a bulk action over a mailbox on the store thread and queues it
/// as one undoable action.
pub(crate) fn bulk(
    sh: &Arc<Shared>,
    token: u64,
    mailbox: Id,
    except_threads: Vec<Id>,
    action: BulkAction,
    label: Option<String>,
) {
    let posted = sh.store.post(move |ctx| {
        let result = (|| {
            let roles = RoleMailboxes::from_rows(&ctx.reader()?.mailboxes()?);
            let patch = action.patch(&mailbox, &roles).ok_or_else(|| {
                SyncError::Invalid("there is no mailbox to move this mail to".into())
            })?;
            let ids = ctx
                .store
                .mailbox_email_ids(mailbox.as_str(), &except_threads)?;
            if ids.is_empty() {
                return Err(SyncError::Invalid("nothing to change".into()));
            }
            let label = label.unwrap_or_else(|| action.label(ids.len()));
            let targets = ids
                .into_iter()
                .map(|id| EmailPatchTarget::new(id, patch.clone()))
                .collect();
            enqueue_on_store(ctx, Mutation::EmailPatch(targets), Some(&label), true)
        })();
        answer(ctx, token, result);
    });
    if !posted {
        stopped(sh, token);
    }
}

fn answer(ctx: &mut StoreCtx, token: u64, result: Result<(Enqueued, DataChanges), SyncError>) {
    match result {
        Ok((enqueued, changed)) => {
            ctx.events.emit(SyncEvent::Applied {
                token,
                seq: enqueued.seq,
                temp_id: enqueued.temp_id,
                changed,
            });
            ctx.refresh_outbox_status();
            ctx.flush_wake.notify_one();
        }
        Err(e) => ctx.events.emit(SyncEvent::Rejected {
            token,
            error: e.to_string(),
        }),
    }
}

/// The email ids a mutation touches.
fn email_ids(mutation: &Mutation) -> Vec<Id> {
    match mutation {
        Mutation::EmailPatch(targets) => targets.iter().map(|t| t.email_id.clone()).collect(),
        Mutation::EmailDestroy(ids) => ids.clone(),
        _ => Vec::new(),
    }
}

fn kind_scope(mutation: &Mutation) -> DataChanges {
    let mut c = DataChanges::default();
    match mutation {
        Mutation::EmailPatch(_) | Mutation::EmailDestroy(_) => {}
        Mutation::MailboxCreate(_) => c.mailboxes = true,
        Mutation::MaskedEmailCreate(_) | Mutation::MaskedEmailUpdate { .. } => {
            c.masked_emails = true
        }
        Mutation::ContactCreate(_)
        | Mutation::ContactUpdate { .. }
        | Mutation::ContactDestroy(_) => c.contacts = true,
        Mutation::EmailSubmit { .. } => c.outbox = true,
    }
    c
}

pub(crate) fn enqueue_on_store(
    ctx: &mut StoreCtx,
    mutation: Mutation,
    label: Option<&str>,
    undoable: bool,
) -> Result<(Enqueued, DataChanges), SyncError> {
    let ids = email_ids(&mutation);
    let before = ctx.memberships(&ids);
    let mut changed = kind_scope(&mutation);
    let now = ctx.now();
    let enqueued = ctx
        .store
        .enqueue(mutation, EnqueueOptions { label, undoable }, now)?;
    changed.merge(ctx.email_scope(&ids, &before));
    Ok((enqueued, changed))
}

/// Undo or redo on the store thread, with the scope of what moved.
pub(crate) fn undo_redo(sh: &Arc<Shared>, token: u64, redo: bool) {
    let posted = sh.store.post(move |ctx| {
        let targets = if redo {
            ctx.store.redo_targets()
        } else {
            ctx.store.undo_targets()
        }
        .unwrap_or_default();
        let before = ctx.memberships(&targets);
        let now = ctx.now();
        let event = if redo {
            ctx.store.redo(now).map(|outcome| {
                let changed = scope_after(ctx, &targets, &before, outcome.is_some());
                SyncEvent::Redone {
                    token,
                    outcome,
                    changed,
                }
            })
        } else {
            ctx.store.undo(now).map(|outcome| {
                let changed = scope_after(ctx, &targets, &before, outcome.is_some());
                SyncEvent::Undone {
                    token,
                    outcome,
                    changed,
                }
            })
        };
        match event {
            Ok(e) => {
                ctx.events.emit(e);
                ctx.refresh_outbox_status();
                ctx.flush_wake.notify_one();
            }
            Err(e) => ctx.events.emit(SyncEvent::Rejected {
                token,
                error: e.to_string(),
            }),
        }
    });
    if !posted {
        stopped(sh, token);
    }
}

fn scope_after(
    ctx: &StoreCtx,
    targets: &[Id],
    before: &std::collections::BTreeMap<Id, Vec<Id>>,
    acted: bool,
) -> DataChanges {
    if !acted {
        return DataChanges::default();
    }
    let mut changed = ctx.email_scope(targets, before);
    // Undo can also touch masks, contacts and queued sends, which have no
    // email targets; name those scopes too, as they are cheap to re-read.
    changed.masked_emails = true;
    changed.outbox = true;
    changed
}
