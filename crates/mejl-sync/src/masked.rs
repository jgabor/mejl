//! Masked Email: full refetches and the commands that change masks.
//!
//! Fastmail reportedly has no usable `MaskedEmail/changes` and an empty
//! `state`, so the list is fetched whole at start, when push names the
//! type, on a slow timer and after every change mejl makes. The store diffs
//! it.

use std::sync::Arc;

use mejl_jmap::method::GetArgs;
use mejl_jmap::methods::MaskedEmailGet;
use mejl_jmap::types::{MaskedEmail, MaskedEmailState};
use mejl_jmap::{Id, Patch, RequestBuilder, capability};
use mejl_store::Mutation;

use crate::apply::Apply;
use crate::backoff::Backoff;
use crate::command::MaskedState;
use crate::error::{Class, SyncError};
use crate::event::SyncEvent;
use crate::mutate;
use crate::shared::Shared;
use crate::sync::apply_now;

pub(crate) async fn masked_loop(sh: Arc<Shared>) {
    let mut backoff = Backoff::new(sh.cfg.backoff_min, sh.cfg.backoff_max);
    loop {
        tokio::select! {
            _ = sh.cancel.cancelled() => return,
            _ = sh.masked.wait() => {}
            _ = tokio::time::sleep(sh.cfg.masked_refresh) => sh.masked.fire(),
        }
        if sh.is_auth_failed() {
            if !sh.wait_auth().await {
                return;
            }
            continue;
        }
        sh.masked.take();
        match refresh(&sh).await {
            Ok(()) => backoff.reset(),
            Err(SyncError::Stale) => sh.masked.fire(),
            Err(e) => {
                let class = sh.note(&e);
                tracing::warn!(error = %e, "masked email refresh failed");
                if class.is_retryable() && class != Class::Auth {
                    if !sh
                        .sleep_or_online(class.retry_delay(backoff.next_delay()))
                        .await
                    {
                        return;
                    }
                    sh.masked.fire();
                }
            }
        }
    }
}

/// Fetches every mask and lets the store diff them.
pub(crate) async fn refresh(sh: &Shared) -> Result<(), SyncError> {
    sh.session().await?;
    if !sh.has_capability(capability::MASKED_EMAIL) {
        return Ok(());
    }
    let account = sh.account()?;
    let stamp = sh.store.stamp();
    let mut req = RequestBuilder::new();
    let h = req.call::<MaskedEmailGet>(GetArgs::all(account));
    let get = sh.send(req).await?.get(&h)?;
    let mut a = Apply::new(stamp);
    a.masked_all = Some(get);
    apply_now(sh, a).await?;
    Ok(())
}

/// Queues a new mask. A mask's address comes from the server, so this is
/// refused offline instead of queued.
pub(crate) fn create(
    sh: &Arc<Shared>,
    token: u64,
    for_domain: String,
    description: String,
    email_prefix: Option<String>,
) {
    if !sh.is_online() {
        sh.events.emit(SyncEvent::Rejected {
            token,
            error: "creating a masked address needs a connection".into(),
        });
        return;
    }
    if let Some(prefix) = &email_prefix
        && !MaskedEmail::is_valid_prefix(prefix)
    {
        sh.events.emit(SyncEvent::Rejected {
            token,
            error: "the prefix may only hold a-z, 0-9 and _, at most 64 characters".into(),
        });
        return;
    }
    let mut masked = MaskedEmail::new_enabled(description);
    masked.for_domain = Some(for_domain);
    masked.email_prefix = email_prefix;
    mutate::enqueue(
        sh,
        token,
        Mutation::MaskedEmailCreate(masked),
        Some("Create masked address".into()),
        false,
    );
}

/// Queues a state change, which is undoable.
pub(crate) fn set_state(sh: &Arc<Shared>, token: u64, id: Id, state: MaskedState) {
    let label = match state {
        MaskedState::Enabled => "Enable masked address",
        MaskedState::Disabled => "Block masked address",
        MaskedState::Deleted => "Delete masked address",
    };
    let wire = match state {
        MaskedState::Enabled => MaskedEmailState::Enabled,
        MaskedState::Disabled => MaskedEmailState::Disabled,
        MaskedState::Deleted => MaskedEmailState::Deleted,
    };
    mutate::enqueue(
        sh,
        token,
        Mutation::MaskedEmailUpdate {
            id,
            patch: Patch::new().property("state", wire.as_str()),
            inverse: None,
        },
        Some(label.into()),
        true,
    );
}

/// Queues a new description, which is undoable.
pub(crate) fn set_description(sh: &Arc<Shared>, token: u64, id: Id, description: String) {
    mutate::enqueue(
        sh,
        token,
        Mutation::MaskedEmailUpdate {
            id,
            patch: Patch::new().property("description", description),
            inverse: None,
        },
        Some("Change masked address description".into()),
        true,
    );
}
