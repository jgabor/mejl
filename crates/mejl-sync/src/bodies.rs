//! Body fetching: the open message first, prefetch after, deduplicated and
//! with a bounded number of requests in flight.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use mejl_jmap::method::GetArgs;
use mejl_jmap::methods::{EmailGet, EmailGetArgs};
use mejl_jmap::{Id, RequestBuilder};
use tokio::sync::mpsc;

use crate::error::SyncError;
use crate::event::SyncEvent;
use crate::shared::Shared;

/// What the store needs to keep a body.
const BODY_PROPERTIES: &[&str] = &[
    "id",
    "bodyValues",
    "textBody",
    "htmlBody",
    "attachments",
    "headers",
];

#[derive(Debug)]
pub(crate) enum BodyMsg {
    /// The open message: before any prefetch.
    Fetch(Id),
    /// Likely next messages. Replaces the queued prefetch.
    Prefetch(Vec<Id>),
}

pub(crate) async fn body_loop(sh: Arc<Shared>, mut rx: mpsc::UnboundedReceiver<BodyMsg>) {
    let mut high: VecDeque<Id> = VecDeque::new();
    let mut low: VecDeque<Id> = VecDeque::new();
    let mut in_flight: HashSet<Id> = HashSet::new();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<Vec<Id>>();
    let limit = sh.cfg.body_concurrency.max(1);
    let batch = sh.cfg.prefetch_batch.max(1);
    loop {
        while in_flight.len() < limit {
            let (ids, open) = if let Some(id) = high.pop_front() {
                (vec![id], true)
            } else if !low.is_empty() {
                let take = batch.min(low.len());
                (low.drain(..take).collect::<Vec<_>>(), false)
            } else {
                break;
            };
            let ids: Vec<Id> = ids
                .into_iter()
                .filter(|id| in_flight.insert(id.clone()))
                .collect();
            if ids.is_empty() {
                continue;
            }
            let sh2 = sh.clone();
            let done = done_tx.clone();
            tokio::spawn(async move {
                fetch(&sh2, &ids, open).await;
                let _ = done.send(ids);
            });
        }
        let busy = in_flight.len();
        sh.status.update(|s| s.activity.bodies = busy);
        tokio::select! {
            _ = sh.cancel.cancelled() => return,
            msg = rx.recv() => match msg {
                None => return,
                Some(BodyMsg::Fetch(id)) => {
                    low.retain(|l| *l != id);
                    if !in_flight.contains(&id) && !high.contains(&id) {
                        high.push_back(id);
                    }
                }
                Some(BodyMsg::Prefetch(ids)) => {
                    // The user moved on; earlier guesses are stale.
                    low.clear();
                    let mut seen = HashSet::new();
                    for id in ids {
                        if !in_flight.contains(&id) && !high.contains(&id) && seen.insert(id.clone()) {
                            low.push_back(id);
                        }
                    }
                }
            },
            done = done_rx.recv() => {
                for id in done.into_iter().flatten() {
                    in_flight.remove(&id);
                }
            }
        }
    }
}

/// Fetches and stores bodies. Opened messages report failures; prefetch
/// failures are quiet.
async fn fetch(sh: &Shared, ids: &[Id], open: bool) {
    let wanted = ids.to_vec();
    let todo = sh
        .store
        .call(move |ctx| -> Result<(Vec<Id>, Vec<Id>), SyncError> {
            let reader = ctx.reader()?;
            let mut cached = Vec::new();
            let mut todo = Vec::new();
            for id in wanted {
                match reader.email(id.as_str())? {
                    Some(detail) if detail.body.is_some() => cached.push(id),
                    _ => todo.push(id),
                }
            }
            Ok((cached, todo))
        })
        .await;
    let (cached, todo) = match todo {
        Ok(Ok(split)) => split,
        Ok(Err(e)) | Err(e) => return fail(sh, ids, open, &e.to_string()),
    };
    for id in cached.into_iter().filter(|_| open) {
        sh.events.emit(SyncEvent::BodyReady { email_id: id });
    }
    if todo.is_empty() {
        return;
    }
    match fetch_now(sh, &todo).await {
        Ok((stored, missing)) => {
            for id in stored {
                sh.events.emit(SyncEvent::BodyReady { email_id: id });
            }
            if open {
                for id in missing {
                    sh.events.emit(SyncEvent::BodyFailed {
                        email_id: id,
                        error: "the message is not on the server or not cached".into(),
                    });
                }
            }
        }
        Err(e) => {
            sh.note(&e);
            tracing::debug!(error = %e, "body fetch failed");
            fail(sh, &todo, open, &e.to_string());
        }
    }
}

fn fail(sh: &Shared, ids: &[Id], open: bool, error: &str) {
    if !open {
        return;
    }
    for id in ids {
        sh.events.emit(SyncEvent::BodyFailed {
            email_id: id.clone(),
            error: error.to_owned(),
        });
    }
}

/// Returns the ids stored and the ids the server or cache did not have.
async fn fetch_now(sh: &Shared, ids: &[Id]) -> Result<(Vec<Id>, Vec<Id>), SyncError> {
    sh.session().await?;
    let account = sh.account()?;
    let mut args = EmailGetArgs::from(
        GetArgs::new(account, ids.to_vec()).properties(BODY_PROPERTIES.iter().copied()),
    );
    args.fetch_text_body_values = Some(true);
    args.fetch_html_body_values = Some(true);
    args.max_body_value_bytes = Some(sh.cfg.max_body_value_bytes);
    let mut req = RequestBuilder::new();
    let h = req.call::<EmailGet>(args);
    let got = sh.send(req).await?.get(&h)?;
    let mut missing = got.not_found;
    let list = got.list;
    let (stored, not_cached) = sh
        .store
        .call(move |ctx| -> Result<(Vec<Id>, Vec<Id>), SyncError> {
            let now = ctx.now();
            let mut stored = Vec::new();
            let mut not_cached = Vec::new();
            let mut batch = ctx.store.batch()?;
            for email in &list {
                if batch.upsert_body(email, now)? {
                    stored.push(email.id.clone());
                } else {
                    not_cached.push(email.id.clone());
                }
            }
            batch.commit()?;
            Ok((stored, not_cached))
        })
        .await??;
    missing.extend(not_cached);
    Ok((stored, missing))
}
