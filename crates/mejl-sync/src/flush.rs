//! The outbox flusher: takes units from the store, sends them, and records
//! the outcome.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use mejl_jmap::method::{Method, SetArgs, SetResponse};
use mejl_jmap::methods::{EmailSet, MailboxSet, MailboxSetArgs, MaskedEmailSet};
use mejl_jmap::types::ContactCard;
use mejl_jmap::{Id, Patch, RequestBuilder, SetErrorType, capability};
use mejl_store::{FinishReport, FlushOp, FlushOutcome, FlushUnit};

use crate::backoff::delay_for;
use crate::error::{Class, SyncError, describe_set_error, set_error_is_transient};
use crate::event::{DataChanges, SyncEvent};
use crate::send;
use crate::shared::Shared;
use crate::store_thread::{Kinds, StoreCtx};

/// The creation id for the one object a create unit makes. Store creation
/// ids such as `local:12` are not valid JMAP ids, so they never go on the
/// wire.
pub(crate) const CREATION_ID: &str = "k0";

/// `ContactCard/set` (RFC 9610). mejl-jmap has no marker for it yet, so
/// the flusher carries its own; [`Method`] is open for that.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ContactCardSet;

impl Method for ContactCardSet {
    const NAME: &'static str = "ContactCard/set";
    const CAPABILITIES: &'static [&'static str] = &[capability::CONTACTS];
    type Args = SetArgs<ContactCard>;
    type Response = SetResponse<ContactCard>;
}

/// Work to do on the store thread after a unit is recorded.
#[derive(Debug, Default)]
pub(crate) enum Post {
    #[default]
    None,
    Sent {
        message_id: String,
        email_id: Option<Id>,
        draft_local_id: Option<String>,
    },
    /// A send failed for good after it made `email_id` in Drafts. The
    /// local draft still holds the message, so the copy is trashed.
    SendFailed {
        email_id: Id,
        draft_local_id: String,
    },
    MaskedCreated {
        id: Id,
        email: Option<String>,
    },
    MaskedChanged,
}

pub(crate) async fn flush_loop(sh: Arc<Shared>) {
    let mut failures = 0u32;
    loop {
        if sh.cancel.is_cancelled() {
            return;
        }
        if sh.is_auth_failed() && !sh.wait_auth().await {
            return;
        }
        if !sh.is_online() {
            // Mutations still queue; the sync loop's retries bring the
            // engine back online, which wakes this loop.
            tokio::select! {
                _ = sh.cancel.cancelled() => return,
                _ = sh.flush_wake.notified() => {}
            }
            continue;
        }
        let max = usize::try_from(sh.limits().max_objects_in_set)
            .unwrap_or(usize::MAX)
            .max(1);
        let now = sh.now();
        let next = sh
            .store
            .call(move |ctx| ctx.store.next_flush(now, max))
            .await;
        match next {
            Err(SyncError::StoreGone) => return,
            Err(e) => {
                tracing::error!(error = %e, "cannot read the outbox");
                failures += 1;
                let delay = delay_for(sh.cfg.backoff_min, sh.cfg.backoff_max, failures);
                if !sh.sleep(delay).await {
                    return;
                }
            }
            Ok(Err(e)) => {
                tracing::error!(error = %e, "cannot read the outbox");
                sh.status.update(|s| s.last_error = Some(e.to_string()));
                failures += 1;
                let delay = delay_for(sh.cfg.backoff_min, sh.cfg.backoff_max, failures);
                if !sh.sleep(delay).await {
                    return;
                }
            }
            Ok(Ok(None)) => {
                failures = 0;
                if !wait_for_work(&sh).await {
                    return;
                }
            }
            Ok(Ok(Some(unit))) => {
                failures = 0;
                run_unit(&sh, unit).await;
            }
        }
    }
}

/// Waits for an enqueue, a hold or backoff to end, or a reconnect.
async fn wait_for_work(sh: &Shared) -> bool {
    let next = sh
        .store
        .call(|ctx| ctx.store.next_flush_time())
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
    let wait = match next {
        Some(at) => {
            let now = sh.now();
            if at <= now {
                // Due but blocked behind other work; look again soon.
                sh.cfg.backoff_min.max(Duration::from_millis(50))
            } else {
                Duration::try_from(at.duration_since(now)).unwrap_or(Duration::ZERO)
            }
        }
        None => Duration::from_secs(3600),
    };
    tokio::select! {
        _ = sh.cancel.cancelled() => false,
        _ = sh.flush_wake.notified() => true,
        _ = tokio::time::sleep(wait) => true,
    }
}

async fn run_unit(sh: &Arc<Shared>, unit: FlushUnit) {
    sh.status.update(|s| s.activity.flushing = true);
    let (outcome, post) = execute(sh, &unit).await;
    sh.status.update(|s| s.activity.flushing = false);
    let applied = matches!(outcome, FlushOutcome::Applied { .. });
    if let FlushOutcome::Retry { error, .. } = &outcome {
        tracing::debug!(%error, attempt = unit.attempt, "outbox unit will be retried");
    }
    let kinds = kinds_of(&unit.op);
    let now = sh.now();
    let recorded = sh
        .store
        .call(move |ctx| record(ctx, &unit, outcome, post, applied, kinds, now))
        .await;
    match recorded {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            // The entries stay in flight until the next start, which
            // retries them; say so rather than hide it.
            tracing::error!(error = %e, "cannot record a flush result");
            sh.status.update(|s| s.last_error = Some(e.to_string()));
        }
        Err(e) => {
            tracing::error!(error = %e, "cannot record a flush result");
            sh.status.update(|s| s.last_error = Some(e.to_string()));
        }
    }
    if applied {
        // The next pass confirms the overlays, so the rows settle on
        // server truth soon even without push.
        sh.sync.fire();
        if kinds.masked {
            // Masks have no change feed; a full refetch confirms them.
            sh.masked.fire();
        }
    }
}

fn record(
    ctx: &mut StoreCtx,
    unit: &FlushUnit,
    outcome: FlushOutcome,
    post: Post,
    applied: bool,
    kinds: Kinds,
    now: jiff::Timestamp,
) -> Result<(), SyncError> {
    let emails = email_targets(&unit.op);
    let before = ctx.memberships(&emails);
    let result = ctx.store.finish_flush(unit, outcome, now);
    if applied {
        // Recorded before any later data is applied, which is what makes
        // older responses detectably stale.
        ctx.gens.bump(kinds);
    }
    let report: FinishReport = result?;
    let mut changes = ctx.email_scope(&emails, &before);
    changes.merge(scope_of(&unit.op));
    if !report.done.is_empty() {
        ctx.events.emit(SyncEvent::OutboxDone {
            seqs: report.done.clone(),
        });
    }
    for f in &report.failed {
        tracing::info!(seq = f.seq.0, error = %f.error, "outbox entry failed");
        ctx.events.emit(SyncEvent::OutboxFailed {
            seq: f.seq,
            label: f.label.clone(),
            error: f.error.clone(),
            failed_targets: f.failed_targets.clone(),
            entry_failed: f.entry_failed,
        });
    }
    if let Post::SendFailed {
        email_id,
        draft_local_id,
    } = &post
        && !report.failed.is_empty()
    {
        let keep = ctx
            .reader()
            .and_then(|r| Ok(r.draft(draft_local_id)?.is_some()))
            .unwrap_or(false);
        if keep {
            match ctx
                .store
                .trash_draft_copies(std::slice::from_ref(email_id), &[], now)
            {
                Ok(()) => ctx.draft_wake.notify_one(),
                Err(e) => tracing::warn!(error = %e, "cannot trash a failed send's copy"),
            }
        }
    }
    if let (Some(seq), true) = (unit.seqs.first(), !report.done.is_empty()) {
        match post {
            Post::None | Post::SendFailed { .. } => {}
            Post::Sent {
                message_id,
                email_id,
                draft_local_id,
            } => {
                // Deleting the draft trashes its server copies; the draft
                // worker destroys them.
                if let Some(local) = &draft_local_id {
                    match ctx.store.delete_draft(local, now) {
                        Ok(_) => {
                            crate::drafts::refresh_pending(ctx);
                            ctx.draft_wake.notify_one();
                        }
                        Err(e) => tracing::warn!(error = %e, "cannot delete the sent draft"),
                    }
                }
                ctx.events.emit(SyncEvent::Sent {
                    seq: *seq,
                    message_id,
                    email_id,
                });
            }
            Post::MaskedCreated { id, email } => {
                ctx.events.emit(SyncEvent::MaskedCreated {
                    seq: *seq,
                    id,
                    email,
                });
            }
            Post::MaskedChanged => {}
        }
    }
    ctx.events.data(changes);
    ctx.refresh_outbox_status();
    Ok(())
}

fn email_targets(op: &FlushOp) -> Vec<Id> {
    match op {
        FlushOp::EmailSet { update, destroy } => update
            .keys()
            .cloned()
            .chain(destroy.iter().cloned())
            .collect(),
        _ => Vec::new(),
    }
}

fn scope_of(op: &FlushOp) -> DataChanges {
    let mut c = DataChanges::default();
    match op {
        FlushOp::EmailSet { .. } => {}
        FlushOp::MailboxCreate { .. } => c.mailboxes = true,
        FlushOp::MaskedEmailCreate { .. } | FlushOp::MaskedEmailUpdate { .. } => {
            c.masked_emails = true
        }
        FlushOp::ContactCreate { .. }
        | FlushOp::ContactUpdate { .. }
        | FlushOp::ContactDestroy { .. } => c.contacts = true,
        FlushOp::EmailSubmit { .. } => {
            c.outbox = true;
            c.mailboxes = true;
        }
    }
    c
}

fn kinds_of(op: &FlushOp) -> Kinds {
    match op {
        FlushOp::EmailSet { .. } | FlushOp::EmailSubmit { .. } => Kinds {
            email: true,
            ..Default::default()
        },
        FlushOp::MailboxCreate { .. } => Kinds {
            mailbox: true,
            ..Default::default()
        },
        FlushOp::MaskedEmailCreate { .. } | FlushOp::MaskedEmailUpdate { .. } => Kinds {
            masked: true,
            ..Default::default()
        },
        FlushOp::ContactCreate { .. }
        | FlushOp::ContactUpdate { .. }
        | FlushOp::ContactDestroy { .. } => Kinds {
            contact: true,
            ..Default::default()
        },
    }
}

/// The outcome for a request that failed as a whole.
pub(crate) fn from_error(sh: &Shared, err: &SyncError, attempt: u32) -> FlushOutcome {
    let class = sh.note(err);
    let error = err.to_string();
    let backoff = delay_for(
        sh.cfg.backoff_min,
        sh.cfg.backoff_max,
        attempt.saturating_sub(1),
    );
    let after = |d: Duration| sh.now().checked_add(d).ok();
    match class {
        Class::Permanent => FlushOutcome::Failed { error },
        // Waits on the auth gate; no need to push the time out.
        Class::Auth => FlushOutcome::Retry {
            error,
            not_before: None,
        },
        Class::Network | Class::Transient => FlushOutcome::Retry {
            error,
            not_before: after(backoff),
        },
        Class::Backoff(asked) => FlushOutcome::Retry {
            error,
            not_before: after(asked.unwrap_or_default().max(backoff)),
        },
    }
}

async fn execute(sh: &Shared, unit: &FlushUnit) -> (FlushOutcome, Post) {
    let account = match sh.account() {
        Ok(a) => a,
        Err(e) => return (from_error(sh, &e, unit.attempt), Post::None),
    };
    match &unit.op {
        FlushOp::EmailSet { update, destroy } => (
            email_set(sh, &account, update, destroy, unit.attempt).await,
            Post::None,
        ),
        FlushOp::MailboxCreate { mailbox, .. } => {
            let args =
                MailboxSetArgs::from(SetArgs::new(account).create(CREATION_ID, mailbox.clone()));
            let mut req = RequestBuilder::new();
            let h = req.call::<MailboxSet>(args);
            let resp = match sh.send(req).await {
                Ok(r) => r,
                Err(e) => return (from_error(sh, &e, unit.attempt), Post::None),
            };
            (
                create_outcome(sh, resp.get(&h).map_err(Into::into), unit.attempt, |m| {
                    m.id.clone()
                }),
                Post::None,
            )
        }
        FlushOp::MaskedEmailCreate { masked, .. } => {
            let mut req = RequestBuilder::new();
            let h = req
                .call::<MaskedEmailSet>(SetArgs::new(account).create(CREATION_ID, masked.clone()));
            let resp = match sh.send(req).await {
                Ok(r) => r,
                Err(e) => return (from_error(sh, &e, unit.attempt), Post::None),
            };
            match resp.get(&h) {
                Ok(set) => {
                    if let Some(err) = set.not_created.get(CREATION_ID) {
                        // Fastmail says to try again in a day. Failing lets the
                        // user decide instead of a create waiting silently.
                        let error = if err.kind == SetErrorType::RateLimit {
                            "Masked Email creation is rate limited; try again later".to_owned()
                        } else {
                            describe_set_error(err)
                        };
                        return (FlushOutcome::Failed { error }, Post::None);
                    }
                    match set.created.get(CREATION_ID) {
                        Some(m) if !m.id.is_empty() => (
                            FlushOutcome::created(m.id.clone()),
                            Post::MaskedCreated {
                                id: m.id.clone(),
                                email: m.email.clone(),
                            },
                        ),
                        _ => (
                            FlushOutcome::Failed {
                                error: "the server did not say what it created".into(),
                            },
                            Post::None,
                        ),
                    }
                }
                Err(e) => (from_error(sh, &e.into(), unit.attempt), Post::None),
            }
        }
        FlushOp::MaskedEmailUpdate { id, patch } => {
            let mut req = RequestBuilder::new();
            let h =
                req.call::<MaskedEmailSet>(SetArgs::new(account).update(id.clone(), patch.clone()));
            let outcome = match sh.send(req).await {
                Ok(resp) => update_outcome(sh, resp.get(&h).map_err(Into::into), id, unit.attempt),
                Err(e) => from_error(sh, &e, unit.attempt),
            };
            (outcome, Post::MaskedChanged)
        }
        FlushOp::ContactCreate { card, .. } => {
            let mut req = RequestBuilder::new();
            let h =
                req.call::<ContactCardSet>(SetArgs::new(account).create(CREATION_ID, card.clone()));
            match sh.send(req).await {
                Ok(resp) => (
                    create_outcome(sh, resp.get(&h).map_err(Into::into), unit.attempt, |c| {
                        c.id.clone()
                    }),
                    Post::None,
                ),
                Err(e) => (from_error(sh, &e, unit.attempt), Post::None),
            }
        }
        FlushOp::ContactUpdate { id, patch } => {
            let mut req = RequestBuilder::new();
            let h =
                req.call::<ContactCardSet>(SetArgs::new(account).update(id.clone(), patch.clone()));
            let outcome = match sh.send(req).await {
                Ok(resp) => update_outcome(sh, resp.get(&h).map_err(Into::into), id, unit.attempt),
                Err(e) => from_error(sh, &e, unit.attempt),
            };
            (outcome, Post::None)
        }
        FlushOp::ContactDestroy { id } => {
            let mut req = RequestBuilder::new();
            let h = req.call::<ContactCardSet>(SetArgs::new(account).destroy(id.clone()));
            let outcome = match sh.send(req).await {
                Ok(resp) => match resp.get(&h) {
                    Ok(set) => match set.not_destroyed.get(id) {
                        Some(err) if err.kind != SetErrorType::NotFound => {
                            let mut rejected = BTreeMap::new();
                            rejected.insert(id.clone(), describe_set_error(err));
                            FlushOutcome::Applied {
                                created_id: None,
                                rejected,
                            }
                        }
                        _ => FlushOutcome::ok(),
                    },
                    Err(e) => from_error(sh, &e.into(), unit.attempt),
                },
                Err(e) => from_error(sh, &e, unit.attempt),
            };
            (outcome, Post::None)
        }
        FlushOp::EmailSubmit { payload } => send::execute(sh, &account, unit, payload).await,
    }
}

fn create_outcome<T>(
    sh: &Shared,
    result: Result<SetResponse<T>, SyncError>,
    attempt: u32,
    id_of: impl Fn(&T) -> Id,
) -> FlushOutcome {
    match result {
        Ok(set) => {
            if let Some(err) = set.not_created.get(CREATION_ID) {
                if set_error_is_transient(err) {
                    return FlushOutcome::Retry {
                        error: describe_set_error(err),
                        not_before: sh
                            .now()
                            .checked_add(delay_for(sh.cfg.backoff_min, sh.cfg.backoff_max, attempt))
                            .ok(),
                    };
                }
                return FlushOutcome::Failed {
                    error: describe_set_error(err),
                };
            }
            match set.created.get(CREATION_ID).map(id_of) {
                Some(id) if !id.is_empty() => FlushOutcome::created(id),
                _ => FlushOutcome::Failed {
                    error: "the server did not say what it created".into(),
                },
            }
        }
        Err(e) => from_error(sh, &e, attempt),
    }
}

fn update_outcome<T>(
    sh: &Shared,
    result: Result<SetResponse<T>, SyncError>,
    id: &Id,
    attempt: u32,
) -> FlushOutcome {
    match result {
        Ok(set) => match set.not_updated.get(id) {
            Some(err) => {
                let mut rejected = BTreeMap::new();
                rejected.insert(id.clone(), describe_set_error(err));
                FlushOutcome::Applied {
                    created_id: None,
                    rejected,
                }
            }
            None => FlushOutcome::ok(),
        },
        Err(e) => from_error(sh, &e, attempt),
    }
}

/// One object in an `Email/set` unit.
enum Item<'a> {
    Update(&'a Id, &'a Patch),
    Destroy(&'a Id),
}

/// Sends an `Email/set` unit, split at `maxObjectsInSet` and packed up to
/// `maxCallsInRequest` calls per request. Per-object rejections from every
/// call are reported together. Re-sending after a partial failure is safe:
/// the patches set absolute values, and a repeated destroy is `notFound`,
/// which counts as done.
async fn email_set(
    sh: &Shared,
    account: &Id,
    update: &BTreeMap<Id, Patch>,
    destroy: &[Id],
    attempt: u32,
) -> FlushOutcome {
    let limits = sh.limits();
    let per_set = usize::try_from(limits.max_objects_in_set)
        .unwrap_or(usize::MAX)
        .max(1);
    let per_request = usize::try_from(limits.max_calls_in_request)
        .unwrap_or(usize::MAX)
        .max(1);
    let items: Vec<Item<'_>> = update
        .iter()
        .map(|(id, p)| Item::Update(id, p))
        .chain(destroy.iter().map(Item::Destroy))
        .collect();
    let calls: Vec<SetArgs<mejl_jmap::types::Email>> = items
        .chunks(per_set)
        .map(|chunk| {
            let mut args = SetArgs::new(account.clone());
            for item in chunk {
                args = match item {
                    Item::Update(id, patch) => args.update((*id).clone(), (*patch).clone()),
                    Item::Destroy(id) => args.destroy((*id).clone()),
                };
            }
            args
        })
        .collect();
    let mut rejected: BTreeMap<Id, String> = BTreeMap::new();
    let mut calls = calls.into_iter().peekable();
    while calls.peek().is_some() {
        let mut req = RequestBuilder::new();
        let handles: Vec<_> = calls
            .by_ref()
            .take(per_request)
            .map(|args| req.call::<EmailSet>(args))
            .collect();
        let resp = match sh.send(req).await {
            Ok(r) => r,
            Err(e) => return from_error(sh, &e, attempt),
        };
        for h in &handles {
            match resp.get(h) {
                Ok(set) => {
                    for (id, err) in &set.not_updated {
                        rejected.insert(id.clone(), describe_set_error(err));
                    }
                    for (id, err) in &set.not_destroyed {
                        if err.kind != SetErrorType::NotFound {
                            rejected.insert(id.clone(), describe_set_error(err));
                        }
                    }
                }
                Err(e) => return from_error(sh, &e.into(), attempt),
            }
        }
    }
    FlushOutcome::Applied {
        created_id: None,
        rejected,
    }
}
