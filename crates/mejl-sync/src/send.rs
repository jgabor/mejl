//! Sending with a client-side undo hold.
//!
//! A send is an outbox entry held until its undo window ends. When it goes
//! out, one request creates the email in Drafts and submits it; on success
//! the server moves it to Sent and clears `$draft`. The message carries a
//! client-made Message-ID, so a send that may already have reached the
//! server (interrupted by a crash or a lost response) is looked up first and
//! never sent twice.
//!
//! A send from a saved draft leaves no copy in Drafts: once it is sent, the
//! local draft is deleted and its server copies go to the draft trash
//! (see [`crate::drafts`]). A send that fails for good destroys the copy it
//! made, since the local draft still holds the message.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use mejl_jmap::method::{GetArgs, QueryArgs, SetArgs};
use mejl_jmap::methods::{
    EmailGet, EmailGetArgs, EmailQuery, EmailQueryArgs, EmailSet, EmailSubmissionSet,
    EmailSubmissionSetArgs,
};
use mejl_jmap::types::{Email, EmailBodyPart, EmailBodyValue, EmailSubmission, keyword};
use mejl_jmap::{EmailFilter, EmailFilterCondition, Id, Patch, RequestBuilder};
use mejl_store::{
    CancelOutcome, EnqueueOptions, FlushOutcome, FlushUnit, Mutation, Seq, type_name,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::backoff::random_u64;
use crate::command::{OutgoingMessage, RoleMailboxes};
use crate::error::{SyncError, describe_set_error, set_error_is_transient};
use crate::event::{DataChanges, SyncEvent};
use crate::flush::{Post, from_error};
use crate::shared::Shared;

const PAYLOAD_VERSION: u32 = 1;
const EMAIL_CREATION_ID: &str = "draft";
const SUBMISSION_CREATION_ID: &str = "send";

/// What the outbox stores for a send. Payloads queued by older builds may
/// carry more fields; they are ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendPayload {
    v: u32,
    message_id: String,
    message: OutgoingMessage,
}

/// The message inside a send's outbox payload, as handed back by
/// [`SyncEvent::Undone`](crate::SyncEvent::Undone) when an undo cancels a
/// held send. `None` when the payload is not a send.
pub fn decode_send_payload(payload: &Value) -> Option<OutgoingMessage> {
    serde_json::from_value::<SendPayload>(payload.clone())
        .ok()
        .map(|p| p.message)
}

/// A new Message-ID, without angle brackets: random, at a domain taken
/// from the From address so it reads as the sender's.
pub(crate) fn make_message_id(from: &str) -> String {
    let domain = from
        .rsplit_once('@')
        .map(|(_, d)| d)
        .unwrap_or_default()
        .trim()
        .trim_end_matches('>')
        .to_ascii_lowercase();
    let clean = !domain.is_empty()
        && domain
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        && !domain.starts_with('.')
        && !domain.ends_with('.');
    let domain = if clean {
        domain
    } else {
        "mejl.invalid".to_owned()
    };
    let millis = jiff::Timestamp::now().as_millisecond();
    format!(
        "{:x}.{:016x}{:08x}@{domain}",
        millis.max(0),
        random_u64(),
        random_u64() as u32
    )
}

/// Queues a send held for `window`.
pub(crate) fn queue(sh: &Arc<Shared>, token: u64, message: OutgoingMessage, window: Duration) {
    if message.from.email.trim().is_empty() {
        sh.events.emit(SyncEvent::Rejected {
            token,
            error: "the message has no From address".into(),
        });
        return;
    }
    if message.to.is_empty() && message.cc.is_empty() && message.bcc.is_empty() {
        sh.events.emit(SyncEvent::Rejected {
            token,
            error: "the message has no recipients".into(),
        });
        return;
    }
    let message_id = make_message_id(&message.from.email);
    let now = sh.now();
    let hold_until = now.checked_add(window).unwrap_or(now);
    let posted = sh.store.post(move |ctx| {
        let payload = SendPayload {
            v: PAYLOAD_VERSION,
            message_id: message_id.clone(),
            message,
        };
        let result = serde_json::to_value(&payload)
            .map_err(|e| SyncError::Invalid(e.to_string()))
            .and_then(|payload| {
                Ok(ctx.store.enqueue(
                    Mutation::EmailSubmit {
                        payload,
                        hold_until: Some(hold_until),
                    },
                    EnqueueOptions {
                        label: Some("Send"),
                        undoable: true,
                    },
                    now,
                )?)
            });
        match result {
            Ok(enqueued) => {
                ctx.events.emit(SyncEvent::SendQueued {
                    token,
                    seq: enqueued.seq,
                    message_id,
                    hold_until,
                });
                ctx.events.data(DataChanges {
                    outbox: true,
                    ..Default::default()
                });
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
        sh.events.emit(SyncEvent::Rejected {
            token,
            error: "the engine has stopped".into(),
        });
    }
}

/// Cancels a held send and hands its message back.
pub(crate) fn cancel(sh: &Arc<Shared>, token: u64, seq: Seq) {
    let posted = sh.store.post(move |ctx| {
        let now = ctx.now();
        match ctx.store.cancel(seq, now) {
            Ok(CancelOutcome::Cancelled { payload }) => {
                let message = payload.as_ref().and_then(decode_send_payload).map(Box::new);
                ctx.events.emit(SyncEvent::SendCancelled {
                    token,
                    seq,
                    message,
                });
                ctx.events.data(DataChanges {
                    outbox: true,
                    ..Default::default()
                });
                ctx.refresh_outbox_status();
            }
            Ok(CancelOutcome::TooLate) => ctx.events.emit(SyncEvent::SendCancelled {
                token,
                seq,
                message: None,
            }),
            Err(e) => ctx.events.emit(SyncEvent::Rejected {
                token,
                error: e.to_string(),
            }),
        }
    });
    if !posted {
        sh.events.emit(SyncEvent::Rejected {
            token,
            error: "the engine has stopped".into(),
        });
    }
}

/// Ends a send's hold.
pub(crate) fn send_now(sh: &Arc<Shared>, seq: Seq) {
    sh.store.post(move |ctx| match ctx.store.send_now(seq) {
        Ok(true) => {
            ctx.refresh_outbox_status();
            ctx.flush_wake.notify_one();
        }
        Ok(false) => tracing::debug!(seq = seq.0, "send now: not pending"),
        Err(e) => tracing::warn!(error = %e, "send now failed"),
    });
}

/// What a lookup by Message-ID found.
enum Existing {
    None,
    /// Created but not submitted: still a draft.
    Draft(Id),
    /// Already submitted.
    Sent(Id),
}

/// Sends one held message.
pub(crate) async fn execute(
    sh: &Shared,
    account: &Id,
    unit: &FlushUnit,
    payload: &Value,
) -> (FlushOutcome, Post) {
    let payload: SendPayload = match serde_json::from_value(payload.clone()) {
        Ok(p) => p,
        Err(e) => {
            return (
                FlushOutcome::Failed {
                    error: format!("the queued message cannot be read: {e}"),
                },
                Post::None,
            );
        }
    };
    let roles = match sh
        .store
        .call(|ctx| -> Result<Option<RoleMailboxes>, SyncError> {
            if ctx.store.type_state(type_name::MAILBOX)?.is_none() {
                return Ok(None);
            }
            Ok(Some(RoleMailboxes::from_rows(&ctx.reader()?.mailboxes()?)))
        })
        .await
    {
        Ok(Ok(Some(r))) => r,
        Ok(Ok(None)) => {
            // The cache was rebuilt and the mailboxes are not back yet. The
            // send is the user's intent; wait for them rather than fail.
            sh.sync.fire();
            return (
                FlushOutcome::Retry {
                    error: "waiting for the mailbox list".into(),
                    not_before: sh
                        .now()
                        .checked_add(crate::backoff::delay_for(
                            sh.cfg.backoff_min,
                            sh.cfg.backoff_max,
                            unit.attempt,
                        ))
                        .ok(),
                },
                Post::None,
            );
        }
        Ok(Err(e)) | Err(e) => return (from_error(sh, &e, unit.attempt), Post::None),
    };
    let (Some(drafts), Some(sent)) = (roles.drafts.clone(), roles.sent.clone()) else {
        return (
            FlushOutcome::Failed {
                error: "the account has no Drafts or Sent mailbox".into(),
            },
            Post::None,
        );
    };
    let post = |email_id: Option<Id>| Post::Sent {
        message_id: payload.message_id.clone(),
        email_id,
        draft_local_id: payload.message.draft_local_id.clone(),
    };
    // A copy made for a send that then failed for good duplicates the
    // local draft, which still holds the message.
    let failed = |outcome: FlushOutcome, created: Option<Id>| {
        let post = match (&outcome, created, &payload.message.draft_local_id) {
            (FlushOutcome::Failed { .. }, Some(email_id), Some(local)) => Post::SendFailed {
                email_id,
                draft_local_id: local.clone(),
            },
            _ => Post::None,
        };
        (outcome, post)
    };
    // The request may have reached the server before a crash or a lost
    // response. Look before sending again.
    if unit.interrupted || unit.attempt > 1 {
        match find_existing(sh, account, &payload.message_id).await {
            Err(e) => return (from_error(sh, &e, unit.attempt), Post::None),
            Ok(Existing::Sent(id)) => {
                tracing::info!("a send that may have been lost had reached the server");
                return (FlushOutcome::ok(), post(Some(id)));
            }
            Ok(Existing::Draft(id)) => {
                let outcome = submit(sh, account, &payload, &drafts, &sent, Some(&id), unit).await;
                return match outcome {
                    Ok(()) => (FlushOutcome::ok(), post(Some(id))),
                    Err(o) => failed(o, Some(id)),
                };
            }
            Ok(Existing::None) => {}
        }
    }
    let mut req = RequestBuilder::new();
    let email = build_email(&payload.message, &payload.message_id, &drafts, sh.now());
    let eh = req.call::<EmailSet>(SetArgs::new(account.clone()).create(EMAIL_CREATION_ID, email));
    let sub = submission_args(account, &payload, &drafts, &sent, None);
    let shh = req.call::<EmailSubmissionSet>(sub);
    let resp = match sh.send(req).await {
        Ok(r) => r,
        Err(e) => return (from_error(sh, &e, unit.attempt), Post::None),
    };
    let created = match resp.get(&eh) {
        Ok(set) => {
            if let Some(err) = set.not_created.get(EMAIL_CREATION_ID) {
                return (set_failure(sh, err, unit.attempt), Post::None);
            }
            set.created.get(EMAIL_CREATION_ID).map(|e| e.id.clone())
        }
        Err(e) => return (from_error(sh, &e.into(), unit.attempt), Post::None),
    };
    match resp.get(&shh) {
        Ok(set) => {
            if let Some(err) = set.not_created.get(SUBMISSION_CREATION_ID) {
                // The email is in Drafts but was not sent. A retry finds
                // it by Message-ID and submits that copy.
                return failed(set_failure(sh, err, unit.attempt), created);
            }
            (FlushOutcome::ok(), post(created))
        }
        Err(e) => (from_error(sh, &e.into(), unit.attempt), Post::None),
    }
}

fn set_failure(sh: &Shared, err: &mejl_jmap::SetError, attempt: u32) -> FlushOutcome {
    if set_error_is_transient(err) {
        FlushOutcome::Retry {
            error: describe_set_error(err),
            not_before: sh
                .now()
                .checked_add(crate::backoff::delay_for(
                    sh.cfg.backoff_min,
                    sh.cfg.backoff_max,
                    attempt,
                ))
                .ok(),
        }
    } else {
        FlushOutcome::Failed {
            error: describe_set_error(err),
        }
    }
}

/// Submits an email already on the server.
async fn submit(
    sh: &Shared,
    account: &Id,
    payload: &SendPayload,
    drafts: &Id,
    sent: &Id,
    existing: Option<&Id>,
    unit: &FlushUnit,
) -> Result<(), FlushOutcome> {
    let mut req = RequestBuilder::new();
    let h =
        req.call::<EmailSubmissionSet>(submission_args(account, payload, drafts, sent, existing));
    let resp = sh
        .send(req)
        .await
        .map_err(|e| from_error(sh, &e, unit.attempt))?;
    match resp.get(&h) {
        Ok(set) => match set.not_created.get(SUBMISSION_CREATION_ID) {
            Some(err) => Err(set_failure(sh, err, unit.attempt)),
            None => Ok(()),
        },
        Err(e) => Err(from_error(sh, &e.into(), unit.attempt)),
    }
}

fn submission_args(
    account: &Id,
    payload: &SendPayload,
    drafts: &Id,
    sent: &Id,
    existing: Option<&Id>,
) -> EmailSubmissionSetArgs {
    let email_id = match existing {
        Some(id) => id.clone(),
        None => Id::from(format!("#{EMAIL_CREATION_ID}")),
    };
    let submission = EmailSubmission {
        identity_id: Some(payload.message.identity_id.clone()),
        email_id: Some(email_id),
        ..Default::default()
    };
    EmailSubmissionSetArgs::from(
        SetArgs::new(account.clone()).create(SUBMISSION_CREATION_ID, submission),
    )
    .on_success_update(
        SUBMISSION_CREATION_ID,
        Patch::new()
            .mailbox(drafts.as_str(), false)
            .mailbox(sent.as_str(), true)
            .keyword(keyword::DRAFT, false),
    )
}

fn non_empty<T: Clone>(v: &[T]) -> Option<Vec<T>> {
    (!v.is_empty()).then(|| v.to_vec())
}

/// The email to create in Drafts for `m`, with `$draft` and `$seen`. A
/// send and a server copy of a draft are built the same way. An empty From
/// address is left out, which only a draft can have.
pub(crate) fn build_email(
    m: &OutgoingMessage,
    message_id: &str,
    drafts: &Id,
    now: jiff::Timestamp,
) -> Email {
    let mut values = BTreeMap::new();
    values.insert("text".to_owned(), EmailBodyValue::new(m.text_body.clone()));
    let text_part = EmailBodyPart {
        part_id: Some("text".into()),
        content_type: Some("text/plain".into()),
        ..Default::default()
    };
    let html_body = m.html_body.as_ref().map(|html| {
        values.insert("html".to_owned(), EmailBodyValue::new(html.clone()));
        vec![EmailBodyPart {
            part_id: Some("html".into()),
            content_type: Some("text/html".into()),
            ..Default::default()
        }]
    });
    let attachments: Vec<EmailBodyPart> = m
        .attachments
        .iter()
        .map(|a| EmailBodyPart {
            blob_id: Some(a.blob_id.clone()),
            name: Some(a.name.clone()),
            content_type: Some(a.content_type.clone()),
            disposition: Some("attachment".into()),
            ..Default::default()
        })
        .collect();
    let mut mailbox_ids = BTreeMap::new();
    mailbox_ids.insert(drafts.clone(), true);
    let mut keywords = BTreeMap::new();
    keywords.insert(keyword::DRAFT.to_owned(), true);
    keywords.insert(keyword::SEEN.to_owned(), true);
    Email {
        mailbox_ids: Some(mailbox_ids),
        keywords: Some(keywords),
        message_id: Some(vec![message_id.to_owned()]),
        in_reply_to: non_empty(&m.in_reply_to),
        references: non_empty(&m.references),
        from: (!m.from.email.trim().is_empty()).then(|| vec![m.from.clone()]),
        to: non_empty(&m.to),
        cc: non_empty(&m.cc),
        bcc: non_empty(&m.bcc),
        reply_to: non_empty(&m.reply_to),
        subject: Some(m.subject.clone()),
        sent_at: Some(now.to_string()),
        body_values: Some(values),
        text_body: Some(vec![text_part]),
        html_body,
        attachments: (!attachments.is_empty()).then_some(attachments),
        ..Default::default()
    }
}

/// The emails with this Message-ID, with `keywords` and `messageId`.
/// `draft_copies` limits it to drafts, the only mail a cleanup may
/// destroy.
pub(crate) async fn find_by_message_id(
    sh: &Shared,
    account: &Id,
    message_id: &str,
    draft_copies: bool,
) -> Result<Vec<Email>, SyncError> {
    let mut q = QueryArgs::new(account.clone());
    q.filter = Some(EmailFilter::Condition(EmailFilterCondition {
        header: Some(vec!["Message-ID".to_owned(), message_id.to_owned()]),
        has_keyword: draft_copies.then(|| keyword::DRAFT.to_owned()),
        ..Default::default()
    }));
    q.limit = Some(10);
    let mut req = RequestBuilder::new();
    let qh = req.call::<EmailQuery>(EmailQueryArgs::from(q));
    let gh = req.call_with_refs::<EmailGet>(
        EmailGetArgs::from(GetArgs::by_reference(account.clone()).properties([
            "id",
            "keywords",
            "messageId",
        ])),
        [("ids", qh.ids())],
    );
    let resp = sh.send(req).await?;
    let found = resp.get(&gh)?.list;
    // The header filter matches by substring; only an exact id counts.
    Ok(found
        .into_iter()
        .filter(|e| {
            e.message_id
                .as_ref()
                .is_none_or(|ids| ids.iter().any(|i| i == message_id))
        })
        .filter(|e| !draft_copies || e.has_keyword(keyword::DRAFT))
        .collect())
}

/// Looks for an email with this Message-ID.
async fn find_existing(sh: &Shared, account: &Id, message_id: &str) -> Result<Existing, SyncError> {
    let matching = find_by_message_id(sh, account, message_id, false).await?;
    if let Some(sent) = matching.iter().find(|e| !e.has_keyword(keyword::DRAFT)) {
        return Ok(Existing::Sent(sent.id.clone()));
    }
    Ok(match matching.first() {
        Some(draft) => Existing::Draft(draft.id.clone()),
        None => Existing::None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mejl_jmap::types::EmailAddress;

    fn message() -> OutgoingMessage {
        OutgoingMessage {
            identity_id: "I1".into(),
            from: EmailAddress::with_name("Mira Holt", "mira@example.org"),
            to: vec![EmailAddress::new("juniper@example.org")],
            cc: vec![],
            bcc: vec![EmailAddress::new("hidden@example.net")],
            reply_to: vec![],
            subject: "Rota".into(),
            text_body: "See you Tuesday".into(),
            html_body: Some("<p>See you Tuesday</p>".into()),
            in_reply_to: vec!["parent@example.org".into()],
            references: vec!["root@example.org".into(), "parent@example.org".into()],
            attachments: vec![crate::command::OutgoingAttachment {
                blob_id: "B1".into(),
                name: "rota.pdf".into(),
                content_type: "application/pdf".into(),
            }],
            draft_local_id: Some("d1".into()),
        }
    }

    #[test]
    fn message_ids_are_unique_and_use_the_sender_domain() {
        let a = make_message_id("mira@example.org");
        let b = make_message_id("Mira@Example.ORG");
        assert_ne!(a, b);
        assert!(a.ends_with("@example.org"), "{a}");
        assert!(b.ends_with("@example.org"), "{b}");
        assert!(!a.contains('<') && !a.contains('>'));
        assert!(make_message_id("no-domain").ends_with("@mejl.invalid"));
        assert!(make_message_id("x@bad domain").ends_with("@mejl.invalid"));
        assert!(make_message_id("x@.example.org").ends_with("@mejl.invalid"));
    }

    #[test]
    fn payload_round_trips() {
        let payload = SendPayload {
            v: PAYLOAD_VERSION,
            message_id: "m1@example.org".into(),
            message: message(),
        };
        let value = serde_json::to_value(&payload).unwrap();
        assert_eq!(decode_send_payload(&value), Some(message()));
        // A payload from a build that also stored the server draft id.
        let mut older = value.clone();
        older["serverDraftId"] = serde_json::json!("M1");
        assert_eq!(decode_send_payload(&older), Some(message()));
        assert_eq!(
            decode_send_payload(&serde_json::json!({"subject": 1})),
            None
        );
    }

    #[test]
    fn email_create_object() {
        let payload = SendPayload {
            v: PAYLOAD_VERSION,
            message_id: "m1@example.org".into(),
            message: message(),
        };
        let email = build_email(
            &payload.message,
            &payload.message_id,
            &Id::from("drafts"),
            "2026-10-23T14:27:00Z".parse().unwrap(),
        );
        let v = serde_json::to_value(&email).unwrap();
        assert_eq!(v["mailboxIds"], serde_json::json!({"drafts": true}));
        assert_eq!(
            v["keywords"],
            serde_json::json!({"$draft": true, "$seen": true})
        );
        assert_eq!(v["messageId"], serde_json::json!(["m1@example.org"]));
        assert_eq!(v["inReplyTo"], serde_json::json!(["parent@example.org"]));
        assert_eq!(v["bcc"][0]["email"], "hidden@example.net");
        assert!(v.get("cc").is_none(), "empty lists are left out");
        assert_eq!(v["textBody"][0]["partId"], "text");
        assert_eq!(v["htmlBody"][0]["type"], "text/html");
        assert_eq!(v["bodyValues"]["html"]["value"], "<p>See you Tuesday</p>");
        assert_eq!(v["attachments"][0]["blobId"], "B1");
        assert_eq!(v["attachments"][0]["disposition"], "attachment");
        assert!(v.get("id").is_none());
        assert_eq!(v["from"][0]["email"], "mira@example.org");

        // A draft may have no From address yet; it is left out.
        let mut bare = message();
        bare.from = EmailAddress::new(" ");
        let v = serde_json::to_value(build_email(
            &bare,
            "m2@example.org",
            &Id::from("drafts"),
            "2026-10-23T14:27:00Z".parse().unwrap(),
        ))
        .unwrap();
        assert!(v.get("from").is_none(), "{v}");

        let sub = submission_args(
            &Id::from("A1"),
            &payload,
            &Id::from("drafts"),
            &Id::from("sent"),
            None,
        );
        let v = serde_json::to_value(&sub).unwrap();
        assert_eq!(v["create"]["send"]["emailId"], "#draft");
        assert_eq!(
            v["onSuccessUpdateEmail"]["#send"],
            serde_json::json!({"mailboxIds/drafts": null, "mailboxIds/sent": true, "keywords/$draft": null})
        );
    }
}
