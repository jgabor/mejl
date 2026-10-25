//! `EmailSubmission/get`, `/changes` and `/set` (RFC 8621 section 7).
//!
//! Sending is immediate: every submission is `final` at once. When a
//! recipient is one of the account's own addresses or a masked address, a
//! copy is delivered back into the account, so a demo user who writes to
//! themselves sees the message arrive.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use super::email::{destroy_emails, update_emails};
use super::{
    Args, Ctx, MethodError, SetError, SetResult, changes_from_log, check_set, set_args, simple_get,
};
use crate::changes::ChangeKind;
use crate::model::{Address, Submission};
use crate::store::NewEmail;

const PROPS: &[&str] = &[
    "id",
    "identityId",
    "emailId",
    "threadId",
    "envelope",
    "sendAt",
    "undoStatus",
    "deliveryStatus",
    "dsnBlobIds",
    "mdnBlobIds",
];

pub(super) fn get(ctx: &mut Ctx<'_>, args: &Map<String, Value>) -> Result<Value, MethodError> {
    let store = &*ctx.store;
    let all = store.submissions.keys().cloned().collect();
    simple_get(
        ctx,
        args,
        PROPS,
        json!(store.logs.submission.state()),
        all,
        |id| store.submissions.get(id).map(Submission::to_json),
    )
}

pub(super) fn changes(ctx: &mut Ctx<'_>, args: &Map<String, Value>) -> Result<Value, MethodError> {
    changes_from_log(ctx, args, &ctx.store.logs.submission, |_, _| {})
}

fn envelope_address(v: &Value) -> Option<String> {
    let obj = v.as_object()?;
    if obj.keys().any(|k| k != "email" && k != "parameters") {
        return None;
    }
    match obj.get("parameters") {
        None | Some(Value::Null) | Some(Value::Object(_)) => {}
        Some(_) => return None,
    }
    obj.get("email")?.as_str().map(str::to_owned)
}

fn looks_like_address(a: &str) -> bool {
    a.split_once('@')
        .is_some_and(|(l, d)| !l.is_empty() && !d.is_empty() && !a.contains([' ', '<', '>']))
}

/// Creates one submission. Returns its id and the recipients.
fn create_one(ctx: &mut Ctx<'_>, value: &Value) -> Result<(String, Vec<String>), SetError> {
    let Some(obj) = value.as_object() else {
        return Err(SetError::new(
            "invalidProperties",
            "create value must be an object",
        ));
    };
    if let Some(k) = obj
        .keys()
        .find(|k| !matches!(k.as_str(), "identityId" | "emailId" | "envelope"))
    {
        return Err(SetError::props(
            format!("{k} cannot be set by the client"),
            &[k],
        ));
    }
    let identity = obj
        .get("identityId")
        .and_then(Value::as_str)
        .and_then(|i| ctx.resolve_id(i))
        .and_then(|i| ctx.store.identities.get(&i).cloned())
        .ok_or_else(|| SetError::props("identityId does not exist", &["identityId"]))?;
    let email_id = obj
        .get("emailId")
        .and_then(Value::as_str)
        .and_then(|i| ctx.resolve_id(i))
        .filter(|i| ctx.store.emails.contains_key(i))
        .ok_or_else(|| SetError::props("emailId does not exist", &["emailId"]))?;
    let email = ctx.store.emails[&email_id].clone();
    let from = match email.from.as_deref() {
        Some([one]) => one.clone(),
        _ => {
            let mut e = SetError::new("invalidEmail", "the email needs exactly one From address");
            e.properties = Some(vec!["from".into()]);
            return Err(e);
        }
    };
    if !identity.allows(&from.email) {
        return Err(SetError::new(
            "forbiddenFrom",
            format!("identity {} may not send as {}", identity.email, from.email),
        ));
    }
    let (mail_from, rcpt_to) = match obj.get("envelope") {
        None | Some(Value::Null) => {
            let rcpts: Vec<String> = [&email.to, &email.cc, &email.bcc]
                .into_iter()
                .flatten()
                .flatten()
                .map(|a: &Address| a.email.clone())
                .collect();
            (from.email.clone(), rcpts)
        }
        Some(env) => {
            let mail_from = env
                .get("mailFrom")
                .and_then(envelope_address)
                .ok_or_else(|| SetError::props("envelope.mailFrom is invalid", &["envelope"]))?;
            let rcpts = env
                .get("rcptTo")
                .and_then(Value::as_array)
                .ok_or_else(|| SetError::props("envelope.rcptTo must be an array", &["envelope"]))?
                .iter()
                .map(envelope_address)
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| SetError::props("envelope.rcptTo is invalid", &["envelope"]))?;
            if !identity.allows(&mail_from) {
                return Err(SetError::new(
                    "forbiddenMailFrom",
                    format!("may not use {mail_from}"),
                ));
            }
            (mail_from, rcpts)
        }
    };
    let mut seen = BTreeSet::new();
    let rcpt_to: Vec<String> = rcpt_to
        .into_iter()
        .filter(|r| seen.insert(r.to_ascii_lowercase()))
        .collect();
    if rcpt_to.is_empty() {
        return Err(SetError::new("noRecipients", "the email has no recipients"));
    }
    let invalid: Vec<&String> = rcpt_to.iter().filter(|r| !looks_like_address(r)).collect();
    if !invalid.is_empty() {
        let mut e = SetError::new(
            "invalidRecipients",
            "some recipients are not valid addresses",
        );
        e.extra
            .insert("invalidRecipients".to_owned(), json!(invalid));
        return Err(e);
    }
    let id = ctx.store.ids.make('S');
    let send_at = ctx.store.now();
    let submission = Submission {
        id: id.clone(),
        identity_id: identity.id.clone(),
        email_id: email_id.clone(),
        thread_id: email.thread_id.clone(),
        envelope: json!({
            "mailFrom": {"email": mail_from, "parameters": null},
            "rcptTo": rcpt_to.iter().map(|r| json!({"email": r, "parameters": null})).collect::<Vec<_>>(),
        }),
        send_at,
    };
    ctx.store.submissions.insert(id.clone(), submission);
    ctx.store.logs.submission.record(&id, ChangeKind::Created);
    Ok((id, rcpt_to))
}

/// Delivers a copy to the account when a recipient is the account itself
/// or one of its masked addresses.
fn deliver_to_self(ctx: &mut Ctx<'_>, email_id: &str, recipients: &[String]) {
    let store = &mut *ctx.store;
    let own: BTreeSet<String> = store
        .identities
        .values()
        .map(|i| i.email.to_ascii_lowercase())
        .filter(|e| !e.starts_with("*@"))
        .chain(std::iter::once(store.username.to_ascii_lowercase()))
        .collect();
    let masks: BTreeSet<String> = store
        .masked
        .values()
        .map(|m| m.email.to_ascii_lowercase())
        .collect();
    let local: Vec<String> = recipients
        .iter()
        .filter(|r| {
            let r = r.to_ascii_lowercase();
            own.contains(&r) || masks.contains(&r)
        })
        .cloned()
        .collect();
    if local.is_empty() {
        return;
    }
    let Some(sent) = store.emails.get(email_id).cloned() else {
        return;
    };
    let copy = NewEmail {
        mailbox_ids: BTreeSet::new(),
        keywords: BTreeSet::new(),
        received_at: None,
        sent_at: sent.sent_at.or(Some(sent.received_at)),
        message_id: sent.message_id.as_ref().and_then(|m| m.first().cloned()),
        in_reply_to: sent.in_reply_to.clone(),
        references: sent.references.clone(),
        sender: sent.sender.clone(),
        from: sent.from.clone(),
        to: sent.to.clone(),
        cc: sent.cc.clone(),
        bcc: None,
        reply_to: sent.reply_to.clone(),
        subject: sent.subject.clone(),
        extra_headers: sent.extra_headers.clone(),
        text: sent.text.clone(),
        html: sent.html.clone(),
        attachments: sent.attachments.clone(),
    };
    // A bounce from a deleted mask produces no copy, as for real mail.
    let _ = store.deliver(copy, &local);
}

pub(super) fn set(
    ctx: &mut Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<Vec<(String, Value)>, MethodError> {
    let args = Args::new(
        args,
        &[
            "accountId",
            "ifInState",
            "create",
            "update",
            "destroy",
            "onSuccessUpdateEmail",
            "onSuccessDestroyEmail",
        ],
    )?;
    args.account(ctx.store)?;
    let old_state = ctx.store.logs.submission.state();
    check_set(ctx, &args, &old_state)?;
    let on_update = args.obj_opt("onSuccessUpdateEmail")?.cloned();
    let on_destroy = args.ids_opt("onSuccessDestroyEmail")?;
    let ops = set_args(&args)?;
    let mut result = SetResult::default();
    let mut succeeded: BTreeSet<String> = BTreeSet::new();

    for (cid, value) in ops.create {
        match create_one(ctx, value) {
            Ok((id, rcpts)) => {
                let email_id = ctx.store.submissions[&id].email_id.clone();
                deliver_to_self(ctx, &email_id, &rcpts);
                ctx.created_ids.insert(cid.clone(), id.clone());
                succeeded.insert(id.clone());
                let send_at = ctx.store.submissions[&id].to_json()["sendAt"].clone();
                result.created.insert(
                    cid,
                    json!({"id": id, "threadId": ctx.store.submissions[&id].thread_id, "sendAt": send_at, "undoStatus": "final"}),
                );
            }
            Err(e) => {
                result.not_created.insert(cid, e.to_json());
            }
        }
    }
    for (given, value) in ops.update {
        let Some(id) = ctx
            .resolve_id(&given)
            .filter(|i| ctx.store.submissions.contains_key(i))
        else {
            result
                .not_updated
                .insert(given, SetError::not_found().to_json());
            continue;
        };
        let err = match value.as_object() {
            Some(obj) if obj.len() == 1 && obj.get("undoStatus") == Some(&json!("canceled")) => {
                SetError::new("cannotUnsend", "the message has already been sent")
            }
            _ => SetError::props("only undoStatus can be updated", &["undoStatus"]),
        };
        result.not_updated.insert(id, err.to_json());
    }
    for given in ops.destroy {
        match ctx
            .resolve_id(&given)
            .filter(|i| ctx.store.submissions.contains_key(i))
        {
            Some(id) => {
                ctx.store.submissions.remove(&id);
                ctx.store.logs.submission.record(&id, ChangeKind::Destroyed);
                result.destroyed.push(id);
            }
            None => {
                result
                    .not_destroyed
                    .insert(given, SetError::not_found().to_json());
            }
        }
    }
    let new_state = ctx.store.logs.submission.state();
    let mut responses = vec![(
        "EmailSubmission/set".to_owned(),
        result.into_json(&ctx.store.account_id, json!(old_state), json!(new_state)),
    )];

    // The implicit Email/set runs only for submissions that succeeded in
    // this call, and its response follows with the same call id.
    let email_for = |ctx: &Ctx<'_>, key: &str| -> Option<String> {
        let sub_id = ctx.resolve_id(key)?;
        if !succeeded.contains(&sub_id) {
            return None;
        }
        ctx.store
            .submissions
            .get(&sub_id)
            .map(|s| s.email_id.clone())
    };
    let mut updates = Vec::new();
    if let Some(map) = &on_update {
        for (key, patch) in map {
            if let Some(email_id) = email_for(ctx, key) {
                updates.push((email_id, patch.clone()));
            }
        }
    }
    let mut destroys = Vec::new();
    for key in on_destroy.iter().flatten() {
        if let Some(email_id) = email_for(ctx, key) {
            destroys.push(email_id);
        }
    }
    if on_update.is_some() || on_destroy.is_some() {
        let email_old = ctx.store.logs.email.state();
        let mut email_result = SetResult::default();
        update_emails(ctx, updates, &mut email_result);
        destroy_emails(ctx, destroys, &mut email_result);
        let email_new = ctx.store.logs.email.state();
        responses.push((
            "Email/set".to_owned(),
            email_result.into_json(&ctx.store.account_id, json!(email_old), json!(email_new)),
        ));
    }
    Ok(responses)
}
