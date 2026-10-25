//! The small read-only types: `Thread`, `Identity`, `AddressBook` and
//! `ContactCard`.

use serde_json::{Map, Value, json};

use super::{Ctx, MethodError, changes_from_log, simple_get};
use crate::model::ADDRESS_BOOK_ID;

pub(super) fn thread_get(
    ctx: &mut Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<Value, MethodError> {
    let store = &*ctx.store;
    let all = store.threads.keys().cloned().collect();
    simple_get(
        ctx,
        args,
        &["id", "emailIds"],
        json!(store.logs.thread.state()),
        all,
        |id| {
            store
                .threads
                .get(id)
                .map(|t| json!({"id": t.id, "emailIds": t.email_ids}))
        },
    )
}

pub(super) fn thread_changes(
    ctx: &mut Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<Value, MethodError> {
    changes_from_log(ctx, args, &ctx.store.logs.thread, |_, _| {})
}

const IDENTITY_PROPS: &[&str] = &[
    "id",
    "name",
    "email",
    "replyTo",
    "bcc",
    "textSignature",
    "htmlSignature",
    "mayDelete",
];

pub(super) fn identity_get(
    ctx: &mut Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<Value, MethodError> {
    let store = &*ctx.store;
    let all = store.identities.keys().cloned().collect();
    simple_get(
        ctx,
        args,
        IDENTITY_PROPS,
        json!(store.logs.identity.state()),
        all,
        |id| store.identities.get(id).map(|i| i.to_json()),
    )
}

pub(super) fn identity_changes(
    ctx: &mut Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<Value, MethodError> {
    changes_from_log(ctx, args, &ctx.store.logs.identity, |_, _| {})
}

pub(super) fn address_book_get(
    ctx: &mut Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<Value, MethodError> {
    let store = &*ctx.store;
    let props = &[
        "id",
        "name",
        "description",
        "sortOrder",
        "isDefault",
        "isSubscribed",
        "shareWith",
        "myRights",
    ];
    simple_get(
        ctx,
        args,
        props,
        json!(store.logs.address_book.state()),
        vec![ADDRESS_BOOK_ID.to_owned()],
        |id| {
            (id == ADDRESS_BOOK_ID).then(|| {
                json!({
                    "id": ADDRESS_BOOK_ID,
                    "name": "Personal",
                    "description": null,
                    "sortOrder": 0,
                    "isDefault": true,
                    "isSubscribed": true,
                    "shareWith": null,
                    "myRights": {"mayRead": true, "mayWrite": false, "mayShare": false, "mayDelete": false},
                })
            })
        },
    )
}

const CONTACT_PROPS: &[&str] = &[
    "id",
    "@type",
    "version",
    "uid",
    "addressBookIds",
    "kind",
    "created",
    "updated",
    "name",
    "emails",
];

pub(super) fn contact_get(
    ctx: &mut Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<Value, MethodError> {
    let store = &*ctx.store;
    let all = store.contacts.keys().cloned().collect();
    simple_get(
        ctx,
        args,
        CONTACT_PROPS,
        json!(store.logs.contact.state()),
        all,
        |id| store.contacts.get(id).map(|c| c.to_json()),
    )
}

pub(super) fn contact_changes(
    ctx: &mut Ctx<'_>,
    args: &Map<String, Value>,
) -> Result<Value, MethodError> {
    changes_from_log(ctx, args, &ctx.store.logs.contact, |_, _| {})
}
