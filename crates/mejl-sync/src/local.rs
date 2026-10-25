//! Local-only writes: drafts, rules, their copy history and UI state.
//!
//! These live in `state.db` and never go to the server, but the store is
//! owned by the store thread, so the caller cannot write them itself. Each
//! write runs as one store job, in order with mutations, and answers with
//! [`SyncEvent::LocalDone`].

use std::sync::Arc;

use jiff::Timestamp;
use mejl_store::{Draft, RuleRecord};

use crate::error::SyncError;
use crate::event::SyncEvent;
use crate::shared::Shared;
use crate::store_thread::StoreCtx;

/// A write to `state.db` that needs no server.
#[derive(Debug, Clone, PartialEq)]
pub enum LocalWrite {
    /// Insert or replace a local draft, in one transaction.
    SaveDraft(Box<Draft>),
    /// Delete a local draft by its local id, and destroy its server copy.
    DeleteDraft(String),
    /// Insert or replace a rule by id.
    SaveRule(RuleRecord),
    /// Delete a rule by id.
    DeleteRule(String),
    /// Put every rule in this order. All ids must be given; an unknown id
    /// fails the whole write and nothing moves.
    ReorderRules(Vec<String>),
    /// Record that the rendered rule block was copied for pasting.
    RecordRuleRender {
        /// The whole block.
        sieve: String,
        /// Its body hash.
        body_sha256: String,
    },
    /// The user confirmed a copy was pasted into Fastmail.
    ConfirmRulePaste {
        /// The render history entry.
        render_id: i64,
    },
    /// Set a UI value, such as pane sizes or saved searches.
    SetUiValue {
        /// The key.
        key: String,
        /// The value, usually JSON.
        value: String,
    },
    /// Remove a UI value.
    DeleteUiValue(String),
}

/// What a [`LocalWrite`] did, for callers that need more than "done".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalOutcome {
    /// The write happened.
    Done,
    /// A delete or confirm found nothing with that id. Not an error: the
    /// state the caller wanted already holds.
    NotFound,
    /// A rule render was recorded under this history id.
    Recorded(i64),
}

/// Runs a local write on the store thread and reports
/// [`SyncEvent::LocalDone`].
pub(crate) fn write(sh: &Arc<Shared>, token: u64, write: LocalWrite) {
    let posted = sh.store.post(move |ctx| {
        let now = ctx.now();
        let result = apply(ctx, write, now).map_err(|e| e.to_string());
        ctx.events.emit(SyncEvent::LocalDone { token, result });
    });
    if !posted {
        sh.events.emit(SyncEvent::LocalDone {
            token,
            result: Err("the engine has stopped".into()),
        });
    }
}

fn found(hit: bool) -> LocalOutcome {
    if hit {
        LocalOutcome::Done
    } else {
        LocalOutcome::NotFound
    }
}

pub(crate) fn apply(
    ctx: &mut StoreCtx,
    write: LocalWrite,
    now: Timestamp,
) -> Result<LocalOutcome, SyncError> {
    let store = &mut ctx.store;
    Ok(match write {
        LocalWrite::SaveDraft(draft) => {
            store.save_draft(&draft)?;
            LocalOutcome::Done
        }
        LocalWrite::DeleteDraft(id) => {
            let hit = store.delete_draft(&id, now)?;
            // Its server copies are in the draft trash now.
            crate::drafts::refresh_pending(ctx);
            ctx.draft_wake.notify_one();
            found(hit)
        }
        LocalWrite::SaveRule(rule) => {
            store.save_rule(&rule)?;
            LocalOutcome::Done
        }
        LocalWrite::DeleteRule(id) => found(store.delete_rule(&id)?),
        LocalWrite::ReorderRules(ids) => {
            let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
            store.reorder_rules(&ids, now)?;
            LocalOutcome::Done
        }
        LocalWrite::RecordRuleRender { sieve, body_sha256 } => {
            LocalOutcome::Recorded(store.record_rule_render(&sieve, &body_sha256, now)?)
        }
        LocalWrite::ConfirmRulePaste { render_id } => {
            found(store.confirm_rule_paste(render_id, now)?)
        }
        LocalWrite::SetUiValue { key, value } => {
            store.set_ui_value(&key, &value, now)?;
            LocalOutcome::Done
        }
        LocalWrite::DeleteUiValue(key) => found(store.delete_ui_value(&key)?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mejl_store::{DraftHeaders, Store, StoreOptions};

    fn ctx() -> (tempfile::TempDir, StoreCtx) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a"), StoreOptions::default()).unwrap();
        (dir, StoreCtx::for_test(store))
    }

    fn now() -> Timestamp {
        "2026-10-23T14:27:00Z".parse().unwrap()
    }

    fn rule(id: &str, order: i64) -> RuleRecord {
        RuleRecord {
            id: id.into(),
            name: format!("Rule {id}"),
            enabled: true,
            sort_order: order,
            rule_json: format!("{{\"id\":\"{id}\"}}"),
            updated_at: now(),
        }
    }

    #[test]
    fn drafts_rules_and_ui_values_round_trip() {
        let (_dir, mut ctx) = ctx();
        let draft = Draft {
            local_id: "d1".into(),
            server_email_id: None,
            identity_id: None,
            headers: DraftHeaders {
                subject: "Rota".into(),
                ..Default::default()
            },
            body: "Hi".into(),
            format: "plain".into(),
            attachments: serde_json::json!([]),
            created_at: now(),
            updated_at: now(),
        };
        let w = |ctx: &mut StoreCtx, w| apply(ctx, w, now()).unwrap();
        assert_eq!(
            w(&mut ctx, LocalWrite::SaveDraft(Box::new(draft))),
            LocalOutcome::Done
        );
        assert_eq!(ctx.store.reader().unwrap().drafts().unwrap().len(), 1);
        assert_eq!(
            w(&mut ctx, LocalWrite::DeleteDraft("d1".into())),
            LocalOutcome::Done
        );
        assert_eq!(
            w(&mut ctx, LocalWrite::DeleteDraft("d1".into())),
            LocalOutcome::NotFound
        );

        w(&mut ctx, LocalWrite::SaveRule(rule("a", 0)));
        w(&mut ctx, LocalWrite::SaveRule(rule("b", 1)));
        w(
            &mut ctx,
            LocalWrite::ReorderRules(vec!["b".into(), "a".into()]),
        );
        let ids: Vec<String> = ctx
            .store
            .reader()
            .unwrap()
            .rules()
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, ["b", "a"]);
        // An unknown id moves nothing and reports an error.
        assert!(
            apply(
                &mut ctx,
                LocalWrite::ReorderRules(vec!["a".into(), "zz".into()]),
                now()
            )
            .is_err()
        );
        assert_eq!(
            w(&mut ctx, LocalWrite::DeleteRule("a".into())),
            LocalOutcome::Done
        );

        let LocalOutcome::Recorded(id) = w(
            &mut ctx,
            LocalWrite::RecordRuleRender {
                sieve: "# block".into(),
                body_sha256: "ab".into(),
            },
        ) else {
            panic!("expected a render id");
        };
        assert_eq!(
            w(&mut ctx, LocalWrite::ConfirmRulePaste { render_id: id }),
            LocalOutcome::Done
        );
        assert_eq!(
            w(
                &mut ctx,
                LocalWrite::ConfirmRulePaste {
                    render_id: id + 100
                }
            ),
            LocalOutcome::NotFound
        );

        w(
            &mut ctx,
            LocalWrite::SetUiValue {
                key: "k".into(),
                value: "v".into(),
            },
        );
        assert_eq!(ctx.store.ui_value("k").unwrap().as_deref(), Some("v"));
        assert_eq!(
            w(&mut ctx, LocalWrite::DeleteUiValue("k".into())),
            LocalOutcome::Done
        );
        assert_eq!(ctx.store.ui_value("k").unwrap(), None);
    }
}
