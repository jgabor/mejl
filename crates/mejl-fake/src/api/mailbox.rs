//! `Mailbox/get`, `Mailbox/changes` and `Mailbox/set` (RFC 8621 section 2).

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use super::{
    Args, Ctx, MethodError, SetError, SetResult, changes_from_log, check_set, set_args, simple_get,
};
use crate::model::{MAILBOX_ROLES, Mailbox};
use crate::store::Store;

const PROPS: &[&str] = &[
    "id",
    "name",
    "parentId",
    "role",
    "sortOrder",
    "totalEmails",
    "unreadEmails",
    "totalThreads",
    "unreadThreads",
    "myRights",
    "isSubscribed",
];

const COUNT_PROPS: &[&str] = &[
    "totalEmails",
    "unreadEmails",
    "totalThreads",
    "unreadThreads",
];

const MAX_NAME_LEN: usize = 490;

fn to_json(store: &Store, mb: &Mailbox) -> Value {
    let c = store.counts(&mb.id);
    json!({
        "id": mb.id,
        "name": mb.name,
        "parentId": mb.parent_id,
        "role": mb.role,
        "sortOrder": mb.sort_order,
        "totalEmails": c.total_emails,
        "unreadEmails": c.unread_emails,
        "totalThreads": c.total_threads,
        "unreadThreads": c.unread_threads,
        "myRights": {
            "mayReadItems": true,
            "mayAddItems": true,
            "mayRemoveItems": true,
            "maySetSeen": true,
            "maySetKeywords": true,
            "mayCreateChild": true,
            "mayRename": mb.role.as_deref() != Some("inbox"),
            "mayDelete": mb.role.as_deref() != Some("inbox"),
            "maySubmit": true,
        },
        "isSubscribed": mb.is_subscribed,
    })
}

pub(super) fn get(ctx: &mut Ctx<'_>, args: &Map<String, Value>) -> Result<Value, MethodError> {
    let store = &*ctx.store;
    let all = store.mailboxes.keys().cloned().collect();
    simple_get(
        ctx,
        args,
        PROPS,
        json!(store.logs.mailbox.state()),
        all,
        |id| store.mailboxes.get(id).map(|mb| to_json(store, mb)),
    )
}

pub(super) fn changes(ctx: &mut Ctx<'_>, args: &Map<String, Value>) -> Result<Value, MethodError> {
    changes_from_log(ctx, args, &ctx.store.logs.mailbox, |page, out| {
        let props = if page.updated_counts_only {
            json!(COUNT_PROPS)
        } else {
            Value::Null
        };
        out.insert("updatedProperties".to_owned(), props);
    })
}

/// Validated mailbox fields from a create or update object.
#[derive(Default)]
struct Fields {
    name: Option<String>,
    parent: Option<Option<String>>,
    role: Option<Option<String>>,
    sort_order: Option<u32>,
    is_subscribed: Option<bool>,
}

fn parse_fields(
    ctx: &Ctx<'_>,
    obj: &Map<String, Value>,
    creating: bool,
) -> Result<Fields, SetError> {
    let mut f = Fields::default();
    for (key, value) in obj {
        match key.as_str() {
            "name" => match value.as_str() {
                Some(name) if !name.trim().is_empty() && name.len() <= MAX_NAME_LEN => {
                    f.name = Some(name.to_owned());
                }
                _ => {
                    return Err(SetError::props(
                        "name must be a non-empty string of at most 490 bytes",
                        &["name"],
                    ));
                }
            },
            "parentId" => match value {
                Value::Null => f.parent = Some(None),
                Value::String(id) => match ctx.resolve_id(id) {
                    Some(resolved) if ctx.store.mailboxes.contains_key(&resolved) => {
                        f.parent = Some(Some(resolved));
                    }
                    _ => return Err(SetError::props("parentId does not exist", &["parentId"])),
                },
                _ => {
                    return Err(SetError::props(
                        "parentId must be an id or null",
                        &["parentId"],
                    ));
                }
            },
            "role" if creating => match value {
                Value::Null => f.role = Some(None),
                Value::String(role) if MAILBOX_ROLES.contains(&role.as_str()) => {
                    if ctx.store.role_id(role).is_some() {
                        return Err(SetError::props(
                            "a mailbox with this role exists",
                            &["role"],
                        ));
                    }
                    f.role = Some(Some(role.clone()));
                }
                _ => return Err(SetError::props("unknown role", &["role"])),
            },
            "sortOrder" => match value.as_u64().and_then(|n| u32::try_from(n).ok()) {
                Some(n) => f.sort_order = Some(n),
                None => {
                    return Err(SetError::props(
                        "sortOrder must be an UnsignedInt",
                        &["sortOrder"],
                    ));
                }
            },
            "isSubscribed" => match value.as_bool() {
                Some(b) => f.is_subscribed = Some(b),
                None => {
                    return Err(SetError::props(
                        "isSubscribed must be a boolean",
                        &["isSubscribed"],
                    ));
                }
            },
            other => {
                return Err(SetError::props(
                    format!("{other} cannot be set by the client here"),
                    &[other],
                ));
            }
        }
    }
    Ok(f)
}

fn name_taken(store: &Store, parent: &Option<String>, name: &str, except: Option<&str>) -> bool {
    store
        .mailboxes
        .values()
        .any(|m| m.parent_id == *parent && m.name == name && Some(m.id.as_str()) != except)
}

/// True when making `new_parent` the parent of `id` would create a cycle.
fn creates_cycle(store: &Store, id: &str, new_parent: &Option<String>) -> bool {
    let mut cur = new_parent.clone();
    let mut steps = 0;
    while let Some(p) = cur {
        if p == id || steps > store.mailboxes.len() {
            return true;
        }
        cur = store.mailboxes.get(&p).and_then(|m| m.parent_id.clone());
        steps += 1;
    }
    false
}

pub(super) fn set(ctx: &mut Ctx<'_>, args: &Map<String, Value>) -> Result<Value, MethodError> {
    let args = Args::new(
        args,
        &[
            "accountId",
            "ifInState",
            "create",
            "update",
            "destroy",
            "onDestroyRemoveEmails",
        ],
    )?;
    args.account(ctx.store)?;
    let old_state = ctx.store.logs.mailbox.state();
    check_set(ctx, &args, &old_state)?;
    let remove_emails = args.bool_opt("onDestroyRemoveEmails")?.unwrap_or(false);
    let ops = set_args(&args)?;
    let mut result = SetResult::default();

    // Creates may name a parent created in the same call, so they run in
    // passes until every entry is placed or none can be.
    let mut pending: Vec<(String, &Value)> = ops.create;
    while !pending.is_empty() {
        let unplaced: BTreeSet<String> = pending.iter().map(|(cid, _)| cid.clone()).collect();
        let mut waiting = Vec::new();
        let mut progressed = false;
        for (cid, value) in pending {
            let parent_ref = value
                .get("parentId")
                .and_then(Value::as_str)
                .and_then(|p| p.strip_prefix('#'));
            if let Some(r) = parent_ref
                && r != cid
                && unplaced.contains(r)
                && !ctx.created_ids.contains_key(r)
            {
                waiting.push((cid, value));
                continue;
            }
            progressed = true;
            match create_one(ctx, value) {
                Ok(id) => {
                    let created = ctx.store.mailboxes.get(&id).map(|mb| {
                        let full = to_json(ctx.store, mb);
                        let mut out = Map::new();
                        for key in ["id", "myRights", "isSubscribed", "sortOrder"]
                            .iter()
                            .chain(COUNT_PROPS)
                        {
                            if let Some(v) = full.get(*key) {
                                out.insert((*key).to_owned(), v.clone());
                            }
                        }
                        Value::Object(out)
                    });
                    ctx.created_ids.insert(cid.clone(), id);
                    result.created.insert(cid, created.unwrap_or(Value::Null));
                }
                Err(e) => {
                    result.not_created.insert(cid, e.to_json());
                }
            }
        }
        if !progressed {
            // Every remaining entry waits on another remaining entry: a cycle.
            for (cid, _) in waiting {
                result.not_created.insert(
                    cid,
                    SetError::props(
                        "parentId refers to a creation that cannot be placed",
                        &["parentId"],
                    )
                    .to_json(),
                );
            }
            break;
        }
        pending = waiting;
    }

    for (id, value) in ops.update {
        match update_one(ctx, &id, value) {
            Ok(()) => {
                result
                    .updated
                    .insert(ctx.resolve_id(&id).unwrap_or(id), Value::Null);
            }
            Err(e) => {
                result.not_updated.insert(id, e.to_json());
            }
        }
    }

    let mut destroy: Vec<(String, Option<String>)> = ops
        .destroy
        .into_iter()
        .map(|id| {
            let resolved = ctx.resolve_id(&id);
            (id, resolved)
        })
        .collect();
    // Children first, so destroying a parent and its child together works.
    let depth = |store: &Store, id: &Option<String>| {
        let mut d = 0usize;
        let mut cur = id
            .as_ref()
            .and_then(|i| store.mailboxes.get(i))
            .and_then(|m| m.parent_id.clone());
        while let Some(p) = cur {
            d += 1;
            if d > store.mailboxes.len() {
                break;
            }
            cur = store.mailboxes.get(&p).and_then(|m| m.parent_id.clone());
        }
        d
    };
    destroy.sort_by_key(|(_, r)| std::cmp::Reverse(depth(ctx.store, r)));
    for (given, resolved) in destroy {
        let Some(id) = resolved.filter(|r| ctx.store.mailboxes.contains_key(r)) else {
            result
                .not_destroyed
                .insert(given, SetError::not_found().to_json());
            continue;
        };
        let store = &mut *ctx.store;
        if store.mailboxes.get(&id).and_then(|m| m.role.as_deref()) == Some("inbox") {
            result.not_destroyed.insert(
                given,
                SetError::new("forbidden", "the inbox cannot be destroyed").to_json(),
            );
            continue;
        }
        if store
            .mailboxes
            .values()
            .any(|m| m.parent_id.as_deref() == Some(id.as_str()))
        {
            result.not_destroyed.insert(
                given,
                SetError::new("mailboxHasChild", "mailbox has children").to_json(),
            );
            continue;
        }
        let has_mail = store.emails.values().any(|e| e.mailbox_ids.contains(&id));
        if has_mail && !remove_emails {
            result.not_destroyed.insert(
                given,
                SetError::new("mailboxHasEmail", "mailbox is not empty").to_json(),
            );
            continue;
        }
        store.destroy_mailbox(&id);
        result.destroyed.push(id);
    }

    let new_state = ctx.store.logs.mailbox.state();
    Ok(result.into_json(&ctx.store.account_id, json!(old_state), json!(new_state)))
}

fn create_one(ctx: &mut Ctx<'_>, value: &Value) -> Result<String, SetError> {
    let Some(obj) = value.as_object() else {
        return Err(SetError::new(
            "invalidProperties",
            "create value must be an object",
        ));
    };
    let f = parse_fields(ctx, obj, true)?;
    let Some(name) = f.name else {
        return Err(SetError::props("name is required", &["name"]));
    };
    let parent = f.parent.unwrap_or(None);
    if name_taken(ctx.store, &parent, &name, None) {
        return Err(SetError::props(
            "a sibling mailbox has this name",
            &["name"],
        ));
    }
    let id = ctx.store.create_mailbox(
        &name,
        parent,
        f.role.flatten().as_deref(),
        f.sort_order.unwrap_or(0),
    );
    if let Some(sub) = f.is_subscribed {
        let store = &mut *ctx.store;
        if let Some(mb) = store.mailboxes.get_mut(&id) {
            mb.is_subscribed = sub;
        }
    }
    Ok(id)
}

fn update_one(ctx: &mut Ctx<'_>, given: &str, value: &Value) -> Result<(), SetError> {
    let Some(id) = ctx
        .resolve_id(given)
        .filter(|i| ctx.store.mailboxes.contains_key(i))
    else {
        return Err(SetError::not_found());
    };
    let Some(obj) = value.as_object() else {
        return Err(SetError::new(
            "invalidPatch",
            "update value must be a patch object",
        ));
    };
    if obj.contains_key("role") {
        return Err(SetError::props(
            "role cannot be changed in this server",
            &["role"],
        ));
    }
    let f = parse_fields(ctx, obj, false)?;
    let current = ctx.store.mailboxes[&id].clone();
    let parent = f.parent.clone().unwrap_or(current.parent_id.clone());
    let name = f.name.clone().unwrap_or(current.name.clone());
    if f.parent.is_some() && creates_cycle(ctx.store, &id, &parent) {
        return Err(SetError::props(
            "parentId would create a cycle",
            &["parentId"],
        ));
    }
    if (f.name.is_some() || f.parent.is_some()) && name_taken(ctx.store, &parent, &name, Some(&id))
    {
        return Err(SetError::props(
            "a sibling mailbox has this name",
            &["name"],
        ));
    }
    if f.name.is_some() && current.role.as_deref() == Some("inbox") && name != current.name {
        return Err(SetError::new("forbidden", "the inbox cannot be renamed"));
    }
    ctx.store.update_mailbox(&id, |mb| {
        mb.name = name;
        mb.parent_id = parent;
        if let Some(o) = f.sort_order {
            mb.sort_order = o;
        }
        if let Some(s) = f.is_subscribed {
            mb.is_subscribed = s;
        }
    });
    Ok(())
}
