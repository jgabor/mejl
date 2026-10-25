//! Fastmail's `MaskedEmail/get`, `/changes` and `/set`, written from
//! Fastmail's developer docs (see `docs/fastmail-masked-email.md`).
//!
//! Choices where the docs are silent or reports are unverified:
//! - `destroy` sets the state to `deleted` and the object stays visible, as
//!   in Fastmail's web UI. Whether the real API destroys never-used masks
//!   is unverified.
//! - `emailPrefix` is checked against the documented rules and a few
//!   reportedly reserved words, and gets a random suffix.
//! - With [`Quirks::masked_email_without_state`](crate::Quirks), states are
//!   empty or null and `/changes` always fails, as third parties report.

use serde_json::{Map, Value, json};

use super::{
    Args, Ctx, MethodError, SetError, SetResult, changes_from_log, check_set, set_args, simple_get,
};
use crate::changes::ChangeKind;
use crate::model::{MaskedEmail, MaskedEmailState};
use crate::store::Store;

const PROPS: &[&str] = &[
    "id",
    "email",
    "state",
    "forDomain",
    "description",
    "url",
    "createdBy",
    "createdAt",
    "lastMessageAt",
];

const RESERVED_PREFIXES: &[&str] = &["admin", "beta", "shop", "store"];

const WORDS: &[&str] = &[
    "amber", "brisk", "cedar", "dune", "ember", "fern", "gale", "hazel", "iris", "juniper",
    "kettle", "loom", "maple", "nettle", "otter", "pearl", "quill", "rusty", "sunny", "tidal",
    "umber", "violet", "willow", "yarrow", "zephyr", "heron", "brook", "cove", "drift", "flax",
];

fn state_json(ctx: &Ctx<'_>) -> Value {
    if ctx.masked_without_state {
        json!("")
    } else {
        json!(ctx.store.logs.masked.state())
    }
}

pub(super) fn get(ctx: &mut Ctx<'_>, args: &Map<String, Value>) -> Result<Value, MethodError> {
    let store = &*ctx.store;
    let all = store.masked.keys().cloned().collect();
    simple_get(ctx, args, PROPS, state_json(ctx), all, |id| {
        store.masked.get(id).map(MaskedEmail::to_json)
    })
}

pub(super) fn changes(ctx: &mut Ctx<'_>, args: &Map<String, Value>) -> Result<Value, MethodError> {
    if ctx.masked_without_state {
        return Err(MethodError::with(
            "cannotCalculateChanges",
            "masked email has no change tracking",
        ));
    }
    changes_from_log(ctx, args, &ctx.store.logs.masked, |_, _| {})
}

fn new_address(store: &mut Store, prefix: Option<&str>) -> String {
    let domain = store.masked_domain.clone();
    loop {
        let local = match prefix {
            Some(p) => {
                let suffix: String = (0..5)
                    .map(|_| char::from(b'a' + store.rng.below(26) as u8))
                    .collect();
                format!("{p}.{suffix}")
            }
            None => {
                let a = store.rng.pick(WORDS).to_owned();
                let b = store.rng.pick(WORDS).to_owned();
                format!("{a}.{b}{:04}", store.rng.below(10_000))
            }
        };
        let address = format!("{local}@{domain}");
        if !store
            .masked
            .values()
            .any(|m| m.email.eq_ignore_ascii_case(&address))
        {
            return address;
        }
    }
}

fn text_field(v: &Value, prop: &str, nullable: bool) -> Result<Option<String>, SetError> {
    match v {
        Value::String(s) => Ok(Some(s.clone())),
        Value::Null if nullable => Ok(None),
        _ => Err(SetError::props(format!("{prop} must be a string"), &[prop])),
    }
}

fn create_one(ctx: &mut Ctx<'_>, value: &Value) -> Result<Value, SetError> {
    let Some(obj) = value.as_object() else {
        return Err(SetError::new(
            "invalidProperties",
            "create value must be an object",
        ));
    };
    let mut state = MaskedEmailState::Pending;
    let mut for_domain = String::new();
    let mut description = String::new();
    let mut url = None;
    let mut prefix = None;
    for (k, v) in obj {
        match k.as_str() {
            "state" => {
                state = v
                    .as_str()
                    .and_then(MaskedEmailState::parse)
                    .ok_or_else(|| SetError::props("state is not a valid state", &["state"]))?;
            }
            "forDomain" => for_domain = text_field(v, "forDomain", false)?.unwrap_or_default(),
            "description" => description = text_field(v, "description", false)?.unwrap_or_default(),
            "url" => url = text_field(v, "url", true)?,
            "emailPrefix" => match v {
                Value::Null => {}
                Value::String(p) => {
                    let valid = !p.is_empty()
                        && p.len() <= 64
                        && p.bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
                    if !valid {
                        return Err(SetError::props(
                            "emailPrefix must be 1 to 64 characters of a-z, 0-9 and _",
                            &["emailPrefix"],
                        ));
                    }
                    if RESERVED_PREFIXES.contains(&p.as_str()) {
                        return Err(SetError::props("Name is reserved", &["emailPrefix"]));
                    }
                    prefix = Some(p.clone());
                }
                _ => {
                    return Err(SetError::props(
                        "emailPrefix must be a string",
                        &["emailPrefix"],
                    ));
                }
            },
            other => {
                return Err(SetError::props(
                    format!("{other} cannot be set by the client"),
                    &[other],
                ));
            }
        }
    }
    {
        let mut quirks = ctx.quirks();
        if quirks.masked_email_rate_limit_next > 0 {
            quirks.masked_email_rate_limit_next -= 1;
            return Err(SetError::new(
                "rateLimit",
                "Too many masked email addresses created recently",
            ));
        }
    }
    let store = &mut *ctx.store;
    let email = new_address(store, prefix.as_deref());
    let id = format!("masked-{}", store.ids.make('m').trim_start_matches('m'));
    let created_at = store.now();
    let mask = MaskedEmail {
        id: id.clone(),
        email: email.clone(),
        state,
        for_domain,
        description,
        url,
        created_by: store.masked_created_by.clone(),
        created_at,
        last_message_at: None,
    };
    let full = mask.to_json();
    store.masked.insert(id.clone(), mask);
    store.logs.masked.record(&id, ChangeKind::Created);
    // Return every property the client did not send: the server-set ones
    // and any defaults.
    let mut out = Map::new();
    if let Value::Object(full) = full {
        for (k, v) in full {
            if !obj.contains_key(&k) {
                out.insert(k, v);
            }
        }
    }
    Ok(Value::Object(out))
}

fn update_one(ctx: &mut Ctx<'_>, id: &str, value: &Value) -> Result<(), SetError> {
    let Some(obj) = value.as_object() else {
        return Err(SetError::new(
            "invalidPatch",
            "update value must be an object",
        ));
    };
    let Some(current) = ctx.store.masked.get(id).cloned() else {
        return Err(SetError::not_found());
    };
    let mut next = current.clone();
    for (k, v) in obj {
        match k.as_str() {
            "state" => {
                let s = v
                    .as_str()
                    .and_then(MaskedEmailState::parse)
                    .ok_or_else(|| SetError::props("state is not a valid state", &["state"]))?;
                if s == MaskedEmailState::Pending && current.state != MaskedEmailState::Pending {
                    return Err(SetError::props(
                        "state cannot go back to pending",
                        &["state"],
                    ));
                }
                next.state = s;
            }
            "description" => {
                next.description = text_field(v, "description", false)?.unwrap_or_default()
            }
            "forDomain" => next.for_domain = text_field(v, "forDomain", false)?.unwrap_or_default(),
            "url" => next.url = text_field(v, "url", true)?,
            other => {
                return Err(SetError::props(
                    format!("{other} cannot be changed"),
                    &[other],
                ));
            }
        }
    }
    let changed = next.state != current.state
        || next.description != current.description
        || next.for_domain != current.for_domain
        || next.url != current.url;
    if changed {
        ctx.store.masked.insert(id.to_owned(), next);
        ctx.store.logs.masked.record(id, ChangeKind::Updated);
    }
    Ok(())
}

pub(super) fn set(ctx: &mut Ctx<'_>, args: &Map<String, Value>) -> Result<Value, MethodError> {
    let args = Args::new(
        args,
        &["accountId", "ifInState", "create", "update", "destroy"],
    )?;
    args.account(ctx.store)?;
    let current = match state_json(ctx) {
        Value::String(s) => s,
        _ => String::new(),
    };
    check_set(ctx, &args, &current)?;
    let old_state = ctx.store.logs.masked.state();
    let ops = set_args(&args)?;
    let mut result = SetResult::default();
    for (cid, value) in ops.create {
        match create_one(ctx, value) {
            Ok(created) => {
                if let Some(id) = created.get("id").and_then(Value::as_str) {
                    ctx.created_ids.insert(cid.clone(), id.to_owned());
                }
                result.created.insert(cid, created);
            }
            Err(e) => {
                result.not_created.insert(cid, e.to_json());
            }
        }
    }
    for (given, value) in ops.update {
        let Some(id) = ctx.resolve_id(&given) else {
            result
                .not_updated
                .insert(given, SetError::not_found().to_json());
            continue;
        };
        match update_one(ctx, &id, value) {
            Ok(()) => {
                result.updated.insert(id, Value::Null);
            }
            Err(e) => {
                result.not_updated.insert(given, e.to_json());
            }
        }
    }
    for given in ops.destroy {
        let id = ctx.resolve_id(&given);
        match id.as_deref().and_then(|i| ctx.store.masked.get_mut(i)) {
            Some(mask) => {
                let changed = mask.state != MaskedEmailState::Deleted;
                mask.state = MaskedEmailState::Deleted;
                let id = mask.id.clone();
                if changed {
                    ctx.store.logs.masked.record(&id, ChangeKind::Updated);
                }
                result.destroyed.push(id);
            }
            None => {
                result
                    .not_destroyed
                    .insert(given, SetError::not_found().to_json());
            }
        }
    }
    let (old, new) = if ctx.masked_without_state {
        (Value::Null, Value::Null)
    } else {
        (json!(old_state), json!(ctx.store.logs.masked.state()))
    };
    Ok(result.into_json(&ctx.store.account_id, old, new))
}
