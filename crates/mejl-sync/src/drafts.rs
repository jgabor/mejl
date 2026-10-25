//! Server copies of local drafts.
//!
//! JMAP emails are immutable, so a new version of a draft is a new email:
//! one `Email/set` creates it in the Drafts mailbox, with `$draft` and
//! `$seen`, and destroys the previous copy. The caller saves the local
//! draft first and then asks for a server copy with
//! [`Command::SaveDraftToServer`](crate::Command::SaveDraftToServer). The
//! message is stored on the draft row in `state.db` under a new revision,
//! so the newest version wins and nothing is lost offline or across a
//! restart.
//!
//! One worker does all the network work, one request at a time, so a draft
//! never has two uploads in flight:
//!
//! - First it empties the draft trash: copies of drafts that were sent or
//!   deleted, copies whose destroy failed, and copies made by an upload
//!   that finished after its draft was deleted. Only emails that still
//!   have `$draft` are destroyed, so mail sent from elsewhere is safe.
//! - Then it uploads the newest revision of each changed draft. Every copy
//!   of a draft carries the same Message-ID. An upload is marked before it
//!   is sent, so after a crash or a lost answer the next upload looks up
//!   stray copies by that Message-ID and destroys them too.
//!
//! Without a Drafts mailbox, drafts stay local and
//! [`SyncStatus::drafts_local_only`](crate::SyncStatus::drafts_local_only)
//! says so. Uploads wait, and are tried again after the next sync pass.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use mejl_jmap::method::{GetArgs, SetArgs};
use mejl_jmap::methods::{EmailGet, EmailGetArgs, EmailSet};
use mejl_jmap::types::keyword;
use mejl_jmap::{Id, RequestBuilder, SetErrorType};
use mejl_store::{DraftTrash, DraftUpload, DraftUploadDone, type_name};

use crate::backoff::delay_for;
use crate::command::{OutgoingMessage, RoleMailboxes};
use crate::error::{Class, SyncError, describe_set_error, set_error_is_transient};
use crate::send::{build_email, find_by_message_id, make_message_id};
use crate::shared::Shared;
use crate::store_thread::StoreCtx;

const CREATION_ID: &str = "draft";

/// How [`SyncStatus::last_error`](crate::SyncStatus::last_error) starts
/// when the server refused a draft copy, so the UI can tell that error
/// from others.
pub const DRAFT_COPY_ERROR: &str = "Cannot save the draft to the server";

/// Trash entries handled in one pass.
const TRASH_BATCH: usize = 50;

/// How long the worker sleeps with nothing to do. Any change wakes it
/// sooner.
const IDLE: Duration = Duration::from_secs(3600);

/// Stores a new version of a draft's server copy and wakes the worker.
pub(crate) fn queue(sh: &Arc<Shared>, local_id: String, message: OutgoingMessage) {
    let posted = sh.store.post(move |ctx| {
        let queued = serde_json::to_value(&message)
            .map_err(|e| SyncError::Invalid(e.to_string()))
            .and_then(|payload| Ok(ctx.store.queue_draft_upload(&local_id, &payload)?));
        match queued {
            Ok(true) => {
                refresh_pending(ctx);
                ctx.draft_wake.notify_one();
            }
            Ok(false) => tracing::debug!("no local draft to copy to the server"),
            Err(e) => tracing::warn!(error = %e, "cannot queue a draft copy"),
        }
    });
    if !posted {
        tracing::debug!("draft copy: the engine has stopped");
    }
}

/// Copies the number of drafts waiting for the server into the status
/// line. The worker does this on every round, but it does not run while
/// offline, so the store jobs that change the count call it too.
pub(crate) fn refresh_pending(ctx: &mut StoreCtx) {
    match ctx.store.draft_uploads() {
        Ok(uploads) => {
            let pending = uploads.len();
            ctx.status.update(|s| s.drafts_pending = pending);
        }
        Err(e) => tracing::warn!(error = %e, "cannot count draft copies to upload"),
    }
}

/// What one round of work did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Nothing to do until something changes.
    Idle,
    /// Did some work; look again at once.
    Progress,
    /// Failed in a way that may pass; wait, then look again.
    Retry(Class),
}

pub(crate) async fn draft_loop(sh: Arc<Shared>) {
    // Revisions the server refused for good, by draft. They are tried
    // again only once the draft changes.
    let mut refused: HashMap<String, i64> = HashMap::new();
    let mut failures = 0u32;
    loop {
        if sh.cancel.is_cancelled() {
            return;
        }
        if sh.is_auth_failed() {
            if !sh.wait_auth().await {
                return;
            }
            continue;
        }
        if !sh.is_online() {
            // Coming back online wakes this loop.
            if !wait(&sh, IDLE).await {
                return;
            }
            continue;
        }
        let step = match step(&sh, &mut refused).await {
            Ok(step) => step,
            Err(SyncError::StoreGone) => return,
            Err(e) => {
                tracing::error!(error = %e, "draft copies: the store failed");
                Step::Retry(Class::Transient)
            }
        };
        match step {
            Step::Idle => {
                failures = 0;
                if !wait(&sh, IDLE).await {
                    return;
                }
            }
            Step::Progress => failures = 0,
            Step::Retry(class) => {
                failures = failures.saturating_add(1);
                let delay =
                    class.retry_delay(delay_for(sh.cfg.backoff_min, sh.cfg.backoff_max, failures));
                if !wait(&sh, delay).await {
                    return;
                }
            }
        }
    }
}

/// Sleeps until woken or `d` passes. Returns false on shutdown.
async fn wait(sh: &Shared, d: Duration) -> bool {
    tokio::select! {
        _ = sh.cancel.cancelled() => false,
        _ = sh.draft_wake.notified() => true,
        _ = tokio::time::sleep(d) => true,
    }
}

/// The outcome of a request that failed as a whole.
fn failed(sh: &Shared, e: &SyncError) -> Step {
    Step::Retry(sh.note(e))
}

async fn step(sh: &Shared, refused: &mut HashMap<String, i64>) -> Result<Step, SyncError> {
    let account = match sh.account() {
        Ok(a) => a,
        Err(e) => return Ok(failed(sh, &e)),
    };
    let trash = sh
        .store
        .call(|ctx| ctx.store.draft_trash(TRASH_BATCH))
        .await??;
    if !trash.is_empty() {
        return empty_trash(sh, &account, trash).await;
    }
    let (uploads, roles) = sh
        .store
        .call(|ctx| -> Result<_, SyncError> {
            let uploads = ctx.store.draft_uploads()?;
            let roles = match ctx.store.type_state(type_name::MAILBOX)? {
                Some(_) => Some(RoleMailboxes::from_rows(&ctx.reader()?.mailboxes()?)),
                None => None,
            };
            Ok((uploads, roles))
        })
        .await??;
    refused.retain(|local, rev| {
        uploads
            .iter()
            .any(|u| &u.local_id == local && u.rev == *rev)
    });
    let pending = uploads.len();
    sh.status.update(|s| {
        s.drafts_pending = pending;
        if let Some(roles) = &roles {
            s.drafts_local_only = roles.drafts.is_none();
        }
    });
    let Some(job) = uploads
        .into_iter()
        .find(|u| refused.get(&u.local_id) != Some(&u.rev))
    else {
        return Ok(Step::Idle);
    };
    let Some(roles) = roles else {
        // The cache was rebuilt and the mailboxes are not back yet. The
        // sync pass that brings them wakes this loop.
        sh.sync.fire();
        return Ok(Step::Idle);
    };
    let Some(drafts) = roles.drafts else {
        // Local only. A later sync pass looks again.
        return Ok(Step::Idle);
    };
    upload(sh, &account, &drafts, job, refused).await
}

/// Uploads one revision of a draft: creates the new copy and destroys the
/// old one in one `Email/set`.
async fn upload(
    sh: &Shared,
    account: &Id,
    drafts: &Id,
    job: DraftUpload,
    refused: &mut HashMap<String, i64>,
) -> Result<Step, SyncError> {
    let message: OutgoingMessage = match serde_json::from_value(job.payload.clone()) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "a queued draft copy cannot be read");
            refused.insert(job.local_id, job.rev);
            return Ok(Step::Progress);
        }
    };
    let local = job.local_id.clone();
    let fresh = make_message_id(&message.from.email);
    let old = job.server_email_id.clone();
    let (claimed, replace_old) = sh
        .store
        .call(move |ctx| -> Result<_, SyncError> {
            let claimed = ctx.store.begin_draft_upload(&local, &fresh)?;
            // A copy that was sent from elsewhere is mail now, not a draft
            // to replace. The cache may lag; the trash checks the server.
            let replace_old = match &old {
                Some(id) => ctx
                    .reader()?
                    .email(id.as_str())?
                    .is_none_or(|e| e.headers.row.draft),
                None => false,
            };
            Ok((claimed, replace_old))
        })
        .await??;
    let Some(message_id) = claimed else {
        // Deleted meanwhile; its copies are in the trash.
        return Ok(Step::Progress);
    };
    let mut destroy: Vec<Id> = job
        .server_email_id
        .iter()
        .filter(|_| replace_old)
        .cloned()
        .collect();
    if job.uncertain {
        // An earlier upload may have made a copy that was never recorded.
        match find_by_message_id(sh, account, &message_id, true).await {
            Ok(found) => {
                for e in found {
                    if !destroy.contains(&e.id) {
                        destroy.push(e.id);
                    }
                }
            }
            Err(e) => return Ok(failed(sh, &e)),
        }
    }
    let email = build_email(&message, &message_id, drafts, sh.now());
    let mut args = SetArgs::new(account.clone()).create(CREATION_ID, email);
    for id in &destroy {
        args = args.destroy(id.clone());
    }
    let mut req = RequestBuilder::new();
    let h = req.call::<EmailSet>(args);
    let set = match sh.send(req).await.and_then(|r| Ok(r.get(&h)?)) {
        Ok(set) => set,
        Err(e) => {
            // The upload stays marked as sent, so the next try looks for
            // a copy this one may have made.
            let step = failed(sh, &e);
            if step == Step::Retry(Class::Permanent) {
                tracing::warn!(error = %e, "the server refused a draft copy");
                refused.insert(job.local_id, job.rev);
                return Ok(Step::Progress);
            }
            return Ok(step);
        }
    };
    let gone = |id: &Id| {
        set.not_destroyed
            .get(id)
            .is_none_or(|err| err.kind == SetErrorType::NotFound)
    };
    let kept: Vec<Id> = destroy.iter().filter(|id| !gone(id)).cloned().collect();
    let created = set
        .created
        .get(CREATION_ID)
        .map(|e| e.id.clone())
        .filter(|id| !id.is_empty());
    let (current, trash) = match &created {
        Some(id) => (Some(id.clone()), kept),
        None => {
            // Nothing new; the old copy stays current while it exists.
            let old = job.server_email_id.clone().filter(|o| kept.contains(o));
            let trash = kept
                .into_iter()
                .filter(|k| Some(k) != old.as_ref())
                .collect();
            (old, trash)
        }
    };
    let done = DraftUploadDone {
        local_id: job.local_id.clone(),
        rev: job.rev,
        uploaded: created.is_some(),
        server_email_id: current,
        trash,
    };
    let now = sh.now();
    sh.store
        .call(move |ctx| ctx.store.finish_draft_upload(&done, now))
        .await??;
    if created.is_some() {
        // The cache picks up the new copy and drops the old one.
        sh.sync.fire();
        return Ok(Step::Progress);
    }
    match set.not_created.get(CREATION_ID) {
        Some(err) if set_error_is_transient(err) => Ok(Step::Retry(Class::Backoff(None))),
        refusal => {
            let error = refusal.map_or_else(
                || "the server did not say what it created".to_owned(),
                describe_set_error,
            );
            tracing::warn!(%error, "the server refused a draft copy");
            sh.status.update(|s| {
                s.last_error = Some(format!("{DRAFT_COPY_ERROR}: {error}"));
            });
            refused.insert(job.local_id, job.rev);
            Ok(Step::Progress)
        }
    }
}

/// Destroys trashed copies that are still drafts, then clears their trash
/// entries.
async fn empty_trash(sh: &Shared, account: &Id, items: Vec<DraftTrash>) -> Result<Step, SyncError> {
    let destroyed_any = match destroy_trashed(sh, account, &items).await {
        Ok(any) => any,
        // A request the server refuses for good would block every upload
        // behind it, so these entries are given up.
        Err(Step::Retry(Class::Permanent)) => {
            tracing::warn!("giving up on destroying draft copies");
            false
        }
        Err(step) => return Ok(step),
    };
    let done: Vec<i64> = items.iter().map(|t| t.id).collect();
    sh.store
        .call(move |ctx| ctx.store.clear_draft_trash(&done))
        .await??;
    if destroyed_any {
        sh.sync.fire();
    }
    Ok(Step::Progress)
}

/// The network half of [`empty_trash`]. Says whether anything was
/// destroyed, or how to retry.
async fn destroy_trashed(sh: &Shared, account: &Id, items: &[DraftTrash]) -> Result<bool, Step> {
    let mut ids: Vec<Id> = Vec::new();
    for item in items {
        if let Some(id) = &item.email_id
            && !ids.contains(id)
        {
            ids.push(id.clone());
        }
        if let Some(mid) = &item.message_id {
            let found = find_by_message_id(sh, account, mid, true)
                .await
                .map_err(|e| failed(sh, &e))?;
            for e in found {
                if !ids.contains(&e.id) {
                    ids.push(e.id);
                }
            }
        }
    }
    if ids.is_empty() {
        return Ok(false);
    }
    // A copy that was sent from elsewhere meanwhile is mail now.
    let mut req = RequestBuilder::new();
    let h = req.call::<EmailGet>(EmailGetArgs::from(
        GetArgs::new(account.clone(), ids).properties(["id", "keywords"]),
    ));
    let drafts: Vec<Id> = sh
        .send(req)
        .await
        .and_then(|r| Ok(r.get(&h)?))
        .map_err(|e| failed(sh, &e))?
        .list
        .into_iter()
        .filter(|e| e.has_keyword(keyword::DRAFT))
        .map(|e| e.id)
        .collect();
    if drafts.is_empty() {
        return Ok(false);
    }
    let mut args = SetArgs::new(account.clone());
    for id in &drafts {
        args = args.destroy(id.clone());
    }
    let mut req = RequestBuilder::new();
    let h = req.call::<EmailSet>(args);
    let set = sh
        .send(req)
        .await
        .and_then(|r| Ok(r.get(&h)?))
        .map_err(|e| failed(sh, &e))?;
    if set.not_destroyed.values().any(set_error_is_transient) {
        return Err(Step::Retry(Class::Backoff(None)));
    }
    for (id, err) in &set.not_destroyed {
        if err.kind != SetErrorType::NotFound {
            // Retrying will not help; the copy stays.
            tracing::warn!(error = %describe_set_error(err), %id, "cannot destroy a draft copy");
        }
    }
    Ok(!set.destroyed.is_empty())
}
