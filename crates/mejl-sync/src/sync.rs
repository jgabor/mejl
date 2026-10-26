//! Cold, warm and full sync, query windows and load-more.

use std::collections::BTreeSet;
use std::sync::Arc;

use mejl_jmap::method::{
    ChangesArgs, ChangesResponse, GetArgs, GetResponse, QueryArgs, QueryChangesArgs,
};
use mejl_jmap::methods::{
    ContactCardChanges, ContactCardGet, EmailChanges, EmailGet, EmailGetArgs, EmailQuery,
    EmailQueryArgs, EmailQueryChanges, EmailQueryChangesArgs, IdentityChanges, IdentityGet,
    MailboxChanges, MailboxGet, ThreadChanges, ThreadGet,
};
use mejl_jmap::types::{ContactCard, Email, Identity, Mailbox, Thread};
use mejl_jmap::{
    CallHandle, Comparator, EmailFilter, EmailFilterCondition, Id, MethodErrorType, RequestBuilder,
    Response, State, capability,
};
use mejl_store::{QueryChangesOutcome, type_name};

use crate::apply::{self, Applied, Apply, mailbox_query_key};
use crate::backoff::Backoff;
use crate::command::RoleMailboxes;
use crate::error::{Class, SyncError};
use crate::event::{DataChanges, SyncEvent, SyncPhase};
use crate::shared::Shared;
use crate::store_thread::Stamp;

/// What the store needs for list rows, conversation rows and replies. Both
/// masked header names are asked for until a live check settles which one
/// Fastmail sets.
pub(crate) const LIST_PROPERTIES: &[&str] = &[
    "id",
    "threadId",
    "mailboxIds",
    "keywords",
    "from",
    "to",
    "cc",
    "bcc",
    "replyTo",
    "sender",
    "subject",
    "sentAt",
    "receivedAt",
    "size",
    "preview",
    "hasAttachment",
    "messageId",
    "inReplyTo",
    "references",
    "header:X-FM-Masked-Email:asText",
    "header:Fastmail-MaskedEmail:asText",
];

/// What changes on an email that is already cached.
pub(crate) const FLAG_PROPERTIES: &[&str] = &["id", "mailboxIds", "keywords"];

/// The role mailboxes the first sync fills after the Inbox.
const FIRST_SYNC_ROLES: &[&str] = &["archive", "sent", "drafts", "junk", "trash"];

/// A cap on `hasMoreChanges` rounds, so a server that keeps saying "more"
/// cannot hold the sync loop forever.
const MAX_CHANGE_ROUNDS: usize = 1000;

/// How many stale responses in a row are retried at once before backing off.
const STALE_RETRIES: u32 = 5;

/// The sync loop: one pass per trigger, with backoff while offline.
pub(crate) async fn sync_loop(sh: Arc<Shared>) {
    let mut backoff = Backoff::new(sh.cfg.backoff_min, sh.cfg.backoff_max);
    let mut stale_runs = 0u32;
    loop {
        let polling = !sh.cfg.push || sh.polling.load(std::sync::atomic::Ordering::SeqCst);
        tokio::select! {
            _ = sh.cancel.cancelled() => return,
            _ = sh.sync.wait() => {}
            _ = tokio::time::sleep(sh.cfg.poll_interval), if polling => sh.sync.fire(),
        }
        if sh.is_auth_failed() {
            if !sh.wait_auth().await {
                return;
            }
            continue;
        }
        if !sh.sleep(sh.cfg.push_debounce).await {
            return;
        }
        sh.sync.take();
        let result = run_sync(&sh).await;
        sh.status.update(|s| s.activity.syncing = None);
        match result {
            Ok(()) => {
                backoff.reset();
                stale_runs = 0;
                let now = sh.now();
                sh.status.update(|s| {
                    s.last_sync = Some(now);
                    s.last_error = None;
                });
                // The mailbox list may have changed, for example gained a
                // Drafts mailbox, so waiting draft uploads look again.
                sh.draft_wake.notify_one();
            }
            Err(SyncError::Stale) => {
                stale_runs += 1;
                tracing::debug!(stale_runs, "sync data was stale; fetching again");
                if stale_runs > STALE_RETRIES && !sh.sleep(backoff.next_delay()).await {
                    return;
                }
                sh.sync.fire();
            }
            Err(e) => {
                let class = sh.note(&e);
                tracing::warn!(error = %e, ?class, "sync failed");
                match class {
                    Class::Auth => {}
                    Class::Permanent => {}
                    Class::Network | Class::Transient | Class::Backoff(_) => {
                        let delay = class.retry_delay(backoff.next_delay());
                        if !sh.sleep_or_online(delay).await {
                            return;
                        }
                        sh.sync.fire();
                    }
                }
            }
        }
    }
}

/// One sync pass: session, then a first sync or the changes since the
/// last one.
pub(crate) async fn run_sync(sh: &Shared) -> Result<(), SyncError> {
    if sh.loaded_session().is_none() {
        sh.status
            .update(|s| s.activity.syncing = Some(SyncPhase::Session));
    }
    sh.session().await?;
    let states = sh
        .store
        .call(|ctx| -> Result<_, SyncError> {
            let s = &ctx.store;
            Ok((
                s.type_state(type_name::MAILBOX)?,
                s.type_state(type_name::EMAIL)?,
                s.type_state(type_name::THREAD)?,
                s.type_state(type_name::IDENTITY)?,
                s.type_state(type_name::CONTACT_CARD)?,
            ))
        })
        .await??;
    let (mailbox, email, thread, identity, contact) = states;
    let first_sync = email.is_none();
    sh.status.update(|s| {
        s.activity.syncing = Some(if first_sync {
            SyncPhase::Mailboxes
        } else {
            SyncPhase::Changes
        })
    });
    let mut parts = vec![Part::Mailbox(mailbox)];
    if sh.has_capability(capability::SUBMISSION) {
        parts.push(Part::Identity(identity));
    }
    if sh.has_capability(capability::CONTACTS) {
        parts.push(Part::Contact(contact));
    }
    if let Some(email) = email {
        parts.push(Part::Email(email));
        if let Some(thread) = thread {
            parts.push(Part::Thread(thread));
        } else {
            parts.push(Part::ThreadResync);
        }
        parts.extend(cached_windows(sh).await?);
    }
    changes_loop(sh, parts).await?;
    if first_sync {
        first_windows(sh).await?;
    }
    Ok(())
}

/// One piece of a warm sync.
#[derive(Debug, Clone)]
enum Part {
    /// `/changes` from a state, or a full `/get` without one.
    Mailbox(Option<State>),
    Identity(Option<State>),
    Contact(Option<State>),
    Email(State),
    Thread(State),
    /// The thread state is missing; fetch every cached thread.
    ThreadResync,
    /// `Email/queryChanges` for a cached window.
    Query {
        key: String,
        mailbox: Id,
        state: State,
    },
}

impl Part {
    fn calls(&self) -> usize {
        match self {
            Part::Mailbox(None) | Part::Identity(None) | Part::Contact(None) => 1,
            Part::Mailbox(Some(_))
            | Part::Identity(Some(_))
            | Part::Contact(Some(_))
            | Part::Email(_)
            | Part::Thread(_) => 3,
            Part::ThreadResync => 0,
            Part::Query { .. } => 1,
        }
    }
}

/// The calls a part added to a request.
enum Calls {
    MailboxAll(CallHandle<MailboxGet>),
    Mailbox(
        CallHandle<MailboxChanges>,
        CallHandle<MailboxGet>,
        CallHandle<MailboxGet>,
    ),
    IdentityAll(CallHandle<IdentityGet>),
    Identity(
        CallHandle<IdentityChanges>,
        CallHandle<IdentityGet>,
        CallHandle<IdentityGet>,
    ),
    ContactAll(CallHandle<ContactCardGet>),
    Contact(
        CallHandle<ContactCardChanges>,
        CallHandle<ContactCardGet>,
        CallHandle<ContactCardGet>,
    ),
    Email(
        CallHandle<EmailChanges>,
        CallHandle<EmailGet>,
        CallHandle<EmailGet>,
    ),
    Thread(
        CallHandle<ThreadChanges>,
        CallHandle<ThreadGet>,
        CallHandle<ThreadGet>,
    ),
    Query(String, Id, CallHandle<EmailQueryChanges>),
}

/// Follow-up work found while processing a response.
#[derive(Default)]
struct Followups {
    more: Vec<Part>,
    resync: Vec<Resync>,
    requery: Vec<(String, Id)>,
    fetch: BTreeSet<Id>,
    check_windows: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resync {
    Mailbox,
    Identity,
    Contact,
    Email,
    Thread,
}

fn by_ref<T>(account: &Id) -> GetArgs<T> {
    GetArgs::by_reference(account.clone())
}

fn email_get(account: &Id, props: &[&str]) -> EmailGetArgs {
    EmailGetArgs::from(GetArgs::by_reference(account.clone()).properties(props.iter().copied()))
}

fn add_calls(
    req: &mut RequestBuilder,
    account: &Id,
    part: &Part,
    max_changes: u64,
) -> Option<Calls> {
    macro_rules! changes {
        ($changes:ty, $get:ty, $state:expr, $variant:ident) => {{
            let ch = req.call::<$changes>(
                ChangesArgs::new(account.clone(), $state.clone()).max_changes(max_changes),
            );
            let created = req.call_with_refs::<$get>(by_ref(account), [("ids", ch.created())]);
            let updated = req.call_with_refs::<$get>(by_ref(account), [("ids", ch.updated())]);
            Calls::$variant(ch, created, updated)
        }};
    }
    Some(match part {
        Part::Mailbox(None) => {
            Calls::MailboxAll(req.call::<MailboxGet>(GetArgs::all(account.clone())))
        }
        Part::Mailbox(Some(s)) => changes!(MailboxChanges, MailboxGet, s, Mailbox),
        Part::Identity(None) => {
            Calls::IdentityAll(req.call::<IdentityGet>(GetArgs::all(account.clone())))
        }
        Part::Identity(Some(s)) => changes!(IdentityChanges, IdentityGet, s, Identity),
        Part::Contact(None) => {
            Calls::ContactAll(req.call::<ContactCardGet>(GetArgs::all(account.clone())))
        }
        Part::Contact(Some(s)) => changes!(ContactCardChanges, ContactCardGet, s, Contact),
        Part::Thread(s) => changes!(ThreadChanges, ThreadGet, s, Thread),
        Part::Email(s) => {
            let ch = req.call::<EmailChanges>(
                ChangesArgs::new(account.clone(), s.clone()).max_changes(max_changes),
            );
            let created = req.call_with_refs::<EmailGet>(
                email_get(account, LIST_PROPERTIES),
                [("ids", ch.created())],
            );
            let updated = req.call_with_refs::<EmailGet>(
                email_get(account, FLAG_PROPERTIES),
                [("ids", ch.updated())],
            );
            Calls::Email(ch, created, updated)
        }
        Part::Query {
            key,
            mailbox,
            state,
        } => {
            let mut args = QueryChangesArgs::new(account.clone(), state.clone());
            args.filter = Some(in_mailbox(mailbox));
            args.sort = Some(newest_first());
            args.calculate_total = Some(true);
            let mut args = EmailQueryChangesArgs::from(args);
            args.collapse_threads = Some(true);
            Calls::Query(
                key.clone(),
                mailbox.clone(),
                req.call::<EmailQueryChanges>(args),
            )
        }
        // Handled outside a request.
        Part::ThreadResync => return None,
    })
}

pub(crate) fn in_mailbox(mailbox: &Id) -> EmailFilter {
    EmailFilter::Condition(EmailFilterCondition {
        in_mailbox: Some(mailbox.clone()),
        ..Default::default()
    })
}

pub(crate) fn newest_first() -> Vec<Comparator> {
    vec![Comparator::desc("receivedAt")]
}

/// A `/changes` response with the objects it named.
type ChangesAndObjects<T> = (ChangesResponse<T>, Vec<T>);

/// The outcome of one `/changes` triple: changes plus the fetched objects,
/// or a resync or a hard failure.
fn changes_result<T, C, G>(
    resp: &Response,
    ch: &CallHandle<C>,
    created: &CallHandle<G>,
    updated: &CallHandle<G>,
) -> Result<Option<ChangesAndObjects<T>>, SyncError>
where
    C: mejl_jmap::Method<Response = ChangesResponse<T>>,
    G: mejl_jmap::Method<Response = GetResponse<T>>,
{
    match resp.get(ch) {
        Ok(changes) => {
            let mut fetched = resp.get(created)?.list;
            fetched.extend(resp.get(updated)?.list);
            Ok(Some((changes, fetched)))
        }
        Err(e) if e.is_method_error(&MethodErrorType::CannotCalculateChanges) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn more_part(part: &Part, changes_has_more: bool, new_state: &State) -> Option<Part> {
    if !changes_has_more || new_state.is_empty() {
        return None;
    }
    Some(match part {
        Part::Mailbox(_) => Part::Mailbox(Some(new_state.clone())),
        Part::Identity(_) => Part::Identity(Some(new_state.clone())),
        Part::Contact(_) => Part::Contact(Some(new_state.clone())),
        Part::Email(_) => Part::Email(new_state.clone()),
        Part::Thread(_) => Part::Thread(new_state.clone()),
        _ => return None,
    })
}

/// Records a per-part failure that should not stop the rest of the sync.
fn part_failed(what: &str, err: SyncError) -> Result<(), SyncError> {
    match err.class() {
        Class::Permanent => {
            tracing::warn!(part = what, error = %err, "skipping a sync part the server refused");
            Ok(())
        }
        _ => Err(err),
    }
}

fn process(
    resp: &Response,
    calls: Vec<(Part, Calls)>,
    a: &mut Apply,
    f: &mut Followups,
) -> Result<(), SyncError> {
    for (part, calls) in calls {
        match calls {
            Calls::MailboxAll(h) => a.mailboxes_all = Some(resp.get(&h)?),
            Calls::IdentityAll(h) => match resp.get(&h) {
                Ok(get) => a.identities_all = Some(get),
                Err(e) => part_failed("Identity/get", e.into())?,
            },
            Calls::ContactAll(h) => match resp.get(&h) {
                Ok(get) => a.contacts_all = Some(get),
                Err(e) => part_failed("ContactCard/get", e.into())?,
            },
            Calls::Mailbox(ch, c, u) => match changes_result::<Mailbox, _, _>(resp, &ch, &c, &u) {
                Ok(Some((changes, fetched))) => {
                    f.more.extend(more_part(
                        &part,
                        changes.has_more_changes,
                        &changes.new_state,
                    ));
                    a.mailbox_changes = Some((changes, fetched));
                }
                Ok(None) => f.resync.push(Resync::Mailbox),
                Err(e) => return Err(e),
            },
            Calls::Identity(ch, c, u) => {
                match changes_result::<Identity, _, _>(resp, &ch, &c, &u) {
                    Ok(Some((changes, fetched))) => {
                        f.more.extend(more_part(
                            &part,
                            changes.has_more_changes,
                            &changes.new_state,
                        ));
                        a.identity_changes = Some((changes, fetched));
                    }
                    Ok(None) => f.resync.push(Resync::Identity),
                    Err(e) => part_failed("Identity/changes", e)?,
                }
            }
            Calls::Contact(ch, c, u) => {
                match changes_result::<ContactCard, _, _>(resp, &ch, &c, &u) {
                    Ok(Some((changes, fetched))) => {
                        f.more.extend(more_part(
                            &part,
                            changes.has_more_changes,
                            &changes.new_state,
                        ));
                        a.contact_changes = Some((changes, fetched));
                    }
                    Ok(None) => f.resync.push(Resync::Contact),
                    Err(e) => part_failed("ContactCard/changes", e)?,
                }
            }
            Calls::Email(ch, c, u) => match changes_result::<Email, _, _>(resp, &ch, &c, &u) {
                Ok(Some((changes, fetched))) => {
                    f.more.extend(more_part(
                        &part,
                        changes.has_more_changes,
                        &changes.new_state,
                    ));
                    a.email_changes = Some((changes, fetched));
                }
                Ok(None) => f.resync.push(Resync::Email),
                Err(e) => return Err(e),
            },
            Calls::Thread(ch, c, u) => match changes_result::<Thread, _, _>(resp, &ch, &c, &u) {
                Ok(Some((changes, fetched))) => {
                    f.more.extend(more_part(
                        &part,
                        changes.has_more_changes,
                        &changes.new_state,
                    ));
                    a.thread_changes = Some((changes, fetched));
                }
                Ok(None) => f.resync.push(Resync::Thread),
                Err(e) => return Err(e),
            },
            Calls::Query(key, mailbox, h) => match resp.get(&h) {
                Ok(changes) => {
                    f.check_windows.push(key.clone());
                    a.query_changes.push((key, changes));
                }
                Err(e)
                    if e.is_method_error(&MethodErrorType::CannotCalculateChanges)
                        || e.is_method_error(&MethodErrorType::StateMismatch)
                        || e.is_method_error(&MethodErrorType::TooManyChanges) =>
                {
                    f.requery.push((key, mailbox));
                }
                Err(e) => {
                    let e: SyncError = e.into();
                    if e.class() == Class::Permanent {
                        tracing::warn!(error = %e, "query changes failed; fetching the window again");
                        f.requery.push((key, mailbox));
                    } else {
                        return Err(e);
                    }
                }
            },
        }
    }
    Ok(())
}

/// Packs parts into requests under `maxCallsInRequest`, applies each
/// response in one transaction, and repeats while servers report more
/// changes. Then runs resyncs, window refetches and missing fetches.
async fn changes_loop(sh: &Shared, parts: Vec<Part>) -> Result<(), SyncError> {
    let account = sh.account()?;
    let limits = sh.limits();
    let max_calls = usize::try_from(limits.max_calls_in_request)
        .unwrap_or(usize::MAX)
        .max(3);
    let max_changes = sh.cfg.max_changes.min(limits.max_objects_in_get).max(1);
    let mut resync: Vec<Resync> = Vec::new();
    let mut requery: Vec<(String, Id)> = Vec::new();
    let mut fetch: BTreeSet<Id> = BTreeSet::new();
    let mut check_windows: Vec<String> = Vec::new();
    let mut pending = parts;
    for round in 0.. {
        if pending.is_empty() {
            break;
        }
        if round >= MAX_CHANGE_ROUNDS {
            tracing::warn!("the server kept reporting more changes; stopping this pass");
            sh.sync.fire();
            break;
        }
        let mut next = Vec::new();
        let (in_request, outside): (Vec<Part>, Vec<Part>) =
            pending.into_iter().partition(|p| p.calls() > 0);
        for p in outside {
            if matches!(p, Part::ThreadResync) {
                resync.push(Resync::Thread);
            }
        }
        for chunk in pack(in_request, max_calls) {
            let mut req = RequestBuilder::new();
            let calls: Vec<(Part, Calls)> = chunk
                .into_iter()
                .filter_map(|p| {
                    let c = add_calls(&mut req, &account, &p, max_changes)?;
                    Some((p, c))
                })
                .collect();
            let stamp = sh.store.stamp();
            let resp = sh.send(req).await?;
            let mut a = Apply::new(stamp);
            let mut f = Followups::default();
            process(&resp, calls, &mut a, &mut f)?;
            let applied = apply_now(sh, a).await?;
            for (key, outcome) in &applied.query_outcomes {
                if matches!(
                    outcome,
                    QueryChangesOutcome::StateMismatch | QueryChangesOutcome::NotCached
                ) && let Some(mailbox) = apply::mailbox_of_key(key)
                {
                    requery.push((key.clone(), mailbox));
                }
            }
            fetch.extend(applied.needs_fetch);
            next.extend(f.more);
            resync.extend(f.resync);
            requery.extend(f.requery);
            fetch.extend(f.fetch);
            check_windows.extend(f.check_windows);
        }
        pending = next;
    }
    resync.sort_by_key(|r| *r as u8);
    resync.dedup();
    for r in &resync {
        full_resync(sh, &account, *r).await?;
        if *r == Resync::Email {
            // Every window is refetched below, so per-window checks and
            // queued fetches are covered.
            requery.extend(cached_window_keys(sh).await?);
        }
    }
    requery.sort();
    requery.dedup();
    if !requery.is_empty() {
        let windows: Vec<Window> = requery
            .iter()
            .map(|(key, mailbox)| Window {
                key: key.clone(),
                mailbox: mailbox.clone(),
                position: 0,
                limit: sh.cfg.window,
            })
            .collect();
        // Keep a window at least as long as it was.
        let windows = widen_to_cached(sh, windows).await?;
        fetch_windows(sh, &account, windows).await?;
    }
    for key in check_windows {
        if requery.iter().any(|(k, _)| *k == key) {
            continue;
        }
        let ids = sh
            .store
            .call(move |ctx| -> Result<Vec<Id>, SyncError> {
                let page = ctx.store.query_page(&key, 0, usize::MAX)?;
                let ids = page.map(|p| p.ids).unwrap_or_default();
                Ok(ctx.store.missing_emails(&ids)?)
            })
            .await??;
        fetch.extend(ids);
    }
    if !fetch.is_empty() {
        let ids: Vec<Id> = fetch.into_iter().collect();
        fetch_emails(sh, &account, &ids).await?;
    }
    Ok(())
}

/// Splits parts into groups whose calls fit in one request.
fn pack(parts: Vec<Part>, max_calls: usize) -> Vec<Vec<Part>> {
    let mut out: Vec<Vec<Part>> = Vec::new();
    let mut current: Vec<Part> = Vec::new();
    let mut used = 0;
    for p in parts {
        let n = p.calls();
        if !current.is_empty() && used + n > max_calls {
            out.push(std::mem::take(&mut current));
            used = 0;
        }
        used += n;
        current.push(p);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Applies on the store thread; stale data is an error so the caller
/// refetches.
pub(crate) async fn apply_now(sh: &Shared, a: Apply) -> Result<Applied, SyncError> {
    let applied = sh.store.call(move |ctx| apply::run(ctx, a)).await??;
    if applied.stale {
        return Err(SyncError::Stale);
    }
    Ok(applied)
}

/// Fetches list properties for emails that are not cached yet.
pub(crate) async fn fetch_emails(sh: &Shared, account: &Id, ids: &[Id]) -> Result<(), SyncError> {
    if ids.is_empty() {
        return Ok(());
    }
    let stamp = sh.store.stamp();
    let got = sh
        .get_chunked::<EmailGet, Email>(ids, |chunk| {
            EmailGetArgs::from(
                GetArgs::new(account.clone(), chunk).properties(LIST_PROPERTIES.iter().copied()),
            )
        })
        .await?;
    let mut a = Apply::new(stamp);
    for g in got {
        a.emails.extend(g.list);
    }
    apply_now(sh, a).await?;
    Ok(())
}

async fn full_resync(sh: &Shared, account: &Id, what: Resync) -> Result<(), SyncError> {
    tracing::info!(?what, "the server cannot list changes; fetching again");
    sh.status
        .update(|s| s.activity.syncing = Some(SyncPhase::Resync));
    let stamp = sh.store.stamp();
    let mut a = Apply::new(stamp);
    match what {
        Resync::Mailbox => {
            let mut req = RequestBuilder::new();
            let h = req.call::<MailboxGet>(GetArgs::all(account.clone()));
            a.mailboxes_all = Some(sh.send(req).await?.get(&h)?);
        }
        Resync::Identity => {
            let mut req = RequestBuilder::new();
            let h = req.call::<IdentityGet>(GetArgs::all(account.clone()));
            a.identities_all = Some(sh.send(req).await?.get(&h)?);
        }
        Resync::Contact => {
            let mut req = RequestBuilder::new();
            let h = req.call::<ContactCardGet>(GetArgs::all(account.clone()));
            a.contacts_all = Some(sh.send(req).await?.get(&h)?);
        }
        Resync::Thread => {
            let ids = sh.store.call(|ctx| ctx.store.cached_thread_ids()).await??;
            let state = current_state::<ThreadGet, Thread>(sh, account).await?;
            let got = sh
                .get_chunked::<ThreadGet, Thread>(&ids, |chunk| {
                    GetArgs::new(account.clone(), chunk)
                })
                .await?;
            for g in got {
                a.threads.extend(g.list);
            }
            a.thread_state = Some(state);
        }
        Resync::Email => {
            let ids = sh.store.call(|ctx| ctx.store.cached_email_ids()).await??;
            // The state comes first, so changes made while the cached
            // emails are fetched are seen again by the next /changes.
            let state = current_state::<EmailGet, Email>(sh, account).await?;
            let props = ["id", "threadId", "mailboxIds", "keywords"];
            let got = sh
                .get_chunked::<EmailGet, Email>(&ids, |chunk| {
                    EmailGetArgs::from(
                        GetArgs::new(account.clone(), chunk).properties(props.iter().copied()),
                    )
                })
                .await?;
            for g in got {
                a.destroyed_emails.extend(g.not_found);
                a.emails.extend(g.list);
            }
            a.email_state = Some(state);
        }
    }
    let applied = apply_now(sh, a).await?;
    if what == Resync::Email {
        sh.events.data(DataChanges {
            everything: true,
            ..Default::default()
        });
    }
    let _ = applied;
    Ok(())
}

/// The current state of a type, from a `/get` of no ids.
async fn current_state<M, T>(sh: &Shared, account: &Id) -> Result<State, SyncError>
where
    M: mejl_jmap::Method<Response = GetResponse<T>>,
    M::Args: From<GetArgs<T>>,
{
    let mut req = RequestBuilder::new();
    let h = req.call::<M>(M::Args::from(GetArgs::new(account.clone(), Vec::new())));
    Ok(sh.send(req).await?.get(&h)?.state)
}

/// A conversation window of one mailbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Window {
    pub key: String,
    pub mailbox: Id,
    pub position: u64,
    pub limit: u64,
}

impl Window {
    pub(crate) fn first(mailbox: &Id, limit: u64) -> Self {
        Window {
            key: mailbox_query_key(mailbox),
            mailbox: mailbox.clone(),
            position: 0,
            limit,
        }
    }
}

/// The result of fetching windows: for each, how many conversations are
/// cached now and the server's total.
pub(crate) struct WindowReport {
    pub mailbox: Id,
    pub cached: u64,
    pub total: Option<u64>,
    pub query_state: State,
}

/// Fetches windows: the query, the representative emails and their
/// threads by back-reference, then the threads' other emails that are not
/// cached. Stores everything and the windows in one transaction.
pub(crate) async fn fetch_windows(
    sh: &Shared,
    account: &Id,
    windows: Vec<Window>,
) -> Result<Vec<WindowReport>, SyncError> {
    if windows.is_empty() {
        return Ok(Vec::new());
    }
    let limits = sh.limits();
    let max_calls = usize::try_from(limits.max_calls_in_request)
        .unwrap_or(usize::MAX)
        .max(3);
    let stamp: Stamp = sh.store.stamp();
    let mut a = Apply::new(stamp);
    let mut reports = Vec::new();
    let mut siblings: BTreeSet<Id> = BTreeSet::new();
    let mut have: BTreeSet<Id> = BTreeSet::new();
    for chunk in windows.chunks(max_calls / 3) {
        let mut req = RequestBuilder::new();
        let mut handles = Vec::new();
        for w in chunk {
            let mut q = QueryArgs::new(account.clone());
            q.filter = Some(in_mailbox(&w.mailbox));
            q.sort = Some(newest_first());
            q.position = Some(i64::try_from(w.position).unwrap_or(i64::MAX));
            q.limit = Some(w.limit.min(limits.max_objects_in_get).max(1));
            q.calculate_total = Some(true);
            let mut q = EmailQueryArgs::from(q);
            q.collapse_threads = Some(true);
            let qh = req.call::<EmailQuery>(q);
            let gh = req.call_with_refs::<EmailGet>(
                email_get(account, LIST_PROPERTIES),
                [("ids", qh.ids())],
            );
            let th = req.call_with_refs::<ThreadGet>(by_ref(account), [("ids", gh.thread_ids())]);
            handles.push((w.clone(), qh, gh, th));
        }
        let resp = sh.send(req).await?;
        for (w, qh, gh, th) in handles {
            let query = resp.get(&qh)?;
            let emails = resp.get(&gh)?;
            let threads = resp.get(&th)?;
            if a.email_state_if_unset.is_none() {
                a.email_state_if_unset = Some(emails.state.clone());
            }
            if a.thread_state_if_unset.is_none() {
                a.thread_state_if_unset = Some(threads.state.clone());
            }
            have.extend(emails.list.iter().map(|e| e.id.clone()));
            for t in &threads.list {
                siblings.extend(t.email_ids.iter().cloned());
            }
            reports.push(WindowReport {
                mailbox: w.mailbox.clone(),
                cached: query.position + u64::try_from(query.ids.len()).unwrap_or(u64::MAX),
                total: query.total,
                query_state: query.query_state.clone(),
            });
            a.emails.extend(emails.list);
            a.threads.extend(threads.list);
            a.queries.push((w.key.clone(), query));
        }
    }
    let wanted: Vec<Id> = siblings.difference(&have).cloned().collect();
    let missing = sh
        .store
        .call(move |ctx| ctx.store.missing_emails(&wanted))
        .await??;
    if !missing.is_empty() {
        let got = sh
            .get_chunked::<EmailGet, Email>(&missing, |chunk| {
                EmailGetArgs::from(
                    GetArgs::new(account.clone(), chunk)
                        .properties(LIST_PROPERTIES.iter().copied()),
                )
            })
            .await?;
        for g in got {
            a.emails.extend(g.list);
        }
    }
    apply_now(sh, a).await?;
    Ok(reports)
}

/// Windows to refetch, each at least as long as what is cached now.
async fn widen_to_cached(sh: &Shared, windows: Vec<Window>) -> Result<Vec<Window>, SyncError> {
    sh.store
        .call(move |ctx| -> Result<Vec<Window>, SyncError> {
            let mut out = Vec::new();
            for mut w in windows {
                if let Some(page) = ctx.store.query_page(&w.key, 0, 0)? {
                    w.limit = w.limit.max(u64::try_from(page.cached).unwrap_or(u64::MAX));
                }
                out.push(w);
            }
            Ok(out)
        })
        .await?
}

/// The cached windows with their query state, for `Email/queryChanges`.
async fn cached_windows(sh: &Shared) -> Result<Vec<Part>, SyncError> {
    sh.store
        .call(|ctx| -> Result<Vec<Part>, SyncError> {
            let mailboxes = ctx.reader()?.mailboxes()?;
            let mut out = Vec::new();
            for m in mailboxes.iter().filter(|m| !m.pending) {
                let key = mailbox_query_key(&m.id);
                if let Some(page) = ctx.store.query_page(&key, 0, 0)?
                    && page.can_calculate_changes
                    && !page.query_state.is_empty()
                {
                    out.push(Part::Query {
                        key,
                        mailbox: m.id.clone(),
                        state: page.query_state,
                    });
                }
            }
            Ok(out)
        })
        .await?
}

async fn cached_window_keys(sh: &Shared) -> Result<Vec<(String, Id)>, SyncError> {
    sh.store
        .call(|ctx| -> Result<Vec<(String, Id)>, SyncError> {
            let mailboxes = ctx.reader()?.mailboxes()?;
            let mut out = Vec::new();
            for m in mailboxes.iter().filter(|m| !m.pending) {
                let key = mailbox_query_key(&m.id);
                if ctx.store.query_page(&key, 0, 0)?.is_some() {
                    out.push((key, m.id.clone()));
                }
            }
            Ok(out)
        })
        .await?
}

/// The first sync after the mailboxes: the Inbox, so it is readable at
/// once, then the other role mailboxes.
async fn first_windows(sh: &Shared) -> Result<(), SyncError> {
    let account = sh.account()?;
    let roles = sh
        .store
        .call(|ctx| -> Result<_, SyncError> {
            Ok(RoleMailboxes::from_rows(&ctx.reader()?.mailboxes()?))
        })
        .await??;
    let window = sh.cfg.window;
    if let Some(inbox) = &roles.inbox {
        sh.status
            .update(|s| s.activity.syncing = Some(SyncPhase::Inbox));
        let reports = fetch_windows(sh, &account, vec![Window::first(inbox, window)]).await?;
        emit_windows(sh, &reports);
    }
    sh.status
        .update(|s| s.activity.syncing = Some(SyncPhase::Folders));
    let others: Vec<Window> = FIRST_SYNC_ROLES
        .iter()
        .filter_map(|role| match *role {
            "archive" => roles.archive.as_ref(),
            "sent" => roles.sent.as_ref(),
            "drafts" => roles.drafts.as_ref(),
            "junk" => roles.junk.as_ref(),
            "trash" => roles.trash.as_ref(),
            _ => None,
        })
        .filter(|id| Some(*id) != roles.inbox.as_ref())
        .map(|id| Window::first(id, window))
        .collect();
    let reports = fetch_windows(sh, &account, others).await?;
    emit_windows(sh, &reports);
    // Without an Inbox no window set the states; take them now so the next
    // pass can ask for changes.
    let missing_state = sh
        .store
        .call(|ctx| ctx.store.type_state(type_name::EMAIL))
        .await??
        .is_none();
    if missing_state {
        let stamp = sh.store.stamp();
        let mut a = Apply::new(stamp);
        a.email_state_if_unset = Some(current_state::<EmailGet, Email>(sh, &account).await?);
        a.thread_state_if_unset = Some(current_state::<ThreadGet, Thread>(sh, &account).await?);
        apply_now(sh, a).await?;
    }
    Ok(())
}

fn emit_windows(sh: &Shared, reports: &[WindowReport]) {
    for r in reports {
        sh.events.emit(SyncEvent::WindowLoaded {
            mailbox: r.mailbox.clone(),
            cached: r.cached,
            total: r.total,
        });
    }
}

/// Extends a mailbox's cached window by one page.
pub(crate) async fn load_more(sh: Arc<Shared>, mailbox: Id) {
    {
        let mut busy = sh
            .loading_more
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !busy.insert(mailbox.clone()) {
            return;
        }
    }
    let result = load_more_inner(&sh, &mailbox).await;
    sh.loading_more
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&mailbox);
    sh.status.update(|s| {
        if s.activity.syncing == Some(SyncPhase::LoadingMore) {
            s.activity.syncing = None;
        }
    });
    match result {
        Ok(reports) => emit_windows(&sh, &reports),
        Err(e) => {
            sh.note(&e);
            tracing::warn!(error = %e, "load more failed");
        }
    }
}

async fn load_more_inner(sh: &Shared, mailbox: &Id) -> Result<Vec<WindowReport>, SyncError> {
    sh.session().await?;
    let account = sh.account()?;
    sh.status.update(|s| {
        if s.activity.syncing.is_none() {
            s.activity.syncing = Some(SyncPhase::LoadingMore);
        }
    });
    let key = mailbox_query_key(mailbox);
    let lookup = key.clone();
    let id = mailbox.clone();
    let (cached, pending) = sh
        .store
        .call(move |ctx| -> Result<_, SyncError> {
            let pending = ctx
                .reader()?
                .mailboxes()?
                .iter()
                .any(|m| m.id == id && m.pending);
            Ok((ctx.store.query_page(&lookup, 0, 0)?, pending))
        })
        .await??;
    let step = sh.cfg.load_more.max(1);
    let Some(page) = cached else {
        // A mailbox created here that the server has not confirmed yet has
        // no server id to query.
        if pending {
            return Ok(Vec::new());
        }
        return fetch_windows(sh, &account, vec![Window::first(mailbox, sh.cfg.window)]).await;
    };
    let cached_len = u64::try_from(page.cached).unwrap_or(u64::MAX);
    let extended = fetch_windows(
        sh,
        &account,
        vec![Window {
            key: key.clone(),
            mailbox: mailbox.clone(),
            position: cached_len,
            limit: step,
        }],
    )
    .await;
    match extended {
        Ok(reports) => {
            if reports.first().map(|r| &r.query_state) == Some(&page.query_state) {
                return Ok(reports);
            }
        }
        Err(SyncError::Store(e)) => {
            // The window changed length under us; refetch it below.
            tracing::debug!(error = %e, "cannot extend the window");
        }
        Err(e) => return Err(e),
    }
    // The query moved on since the window was cached, so the new page may
    // not line up with it. Fetch the whole, longer window again.
    let limit = cached_len.saturating_add(step);
    fetch_windows(
        sh,
        &account,
        vec![Window {
            key,
            mailbox: mailbox.clone(),
            position: 0,
            limit,
        }],
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packing_respects_the_call_limit() {
        let parts = vec![
            Part::Mailbox(Some(State::from("1"))),
            Part::Email(State::from("1")),
            Part::Thread(State::from("1")),
            Part::Identity(None),
            Part::Query {
                key: "k".into(),
                mailbox: "m".into(),
                state: "q".into(),
            },
        ];
        let packed = pack(parts.clone(), 16);
        assert_eq!(packed.len(), 1);
        let packed = pack(parts, 6);
        assert_eq!(packed.iter().map(Vec::len).collect::<Vec<_>>(), [2, 3]);
        for group in &packed {
            assert!(group.iter().map(Part::calls).sum::<usize>() <= 6);
        }
    }

    #[test]
    fn more_parts_only_when_the_server_says_so() {
        let p = Part::Email(State::from("1"));
        assert!(more_part(&p, false, &State::from("2")).is_none());
        assert!(more_part(&p, true, &State::from("")).is_none());
        assert!(matches!(
            more_part(&p, true, &State::from("2")),
            Some(Part::Email(s)) if s == "2"
        ));
    }

    #[test]
    fn list_properties_cover_the_masked_headers() {
        assert!(LIST_PROPERTIES.contains(&"header:X-FM-Masked-Email:asText"));
        assert!(LIST_PROPERTIES.contains(&"receivedAt"));
        assert!(FLAG_PROPERTIES.contains(&"keywords"));
    }
}
