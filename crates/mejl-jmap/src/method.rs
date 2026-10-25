//! The `Method` trait and the standard argument and response shapes of
//! RFC 8620 section 5.
//!
//! Each JMAP method is a zero-sized marker type that implements [`Method`].
//! Vendor methods such as `MaskedEmail/get` are ordinary impls, and other
//! crates can add their own.

use std::collections::BTreeMap;
use std::marker::PhantomData;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::SetError;
use crate::filter::{Comparator, Filter};
use crate::patch::Patch;
use crate::types::core::{Extra, Id, State, null_as_default};

/// A JMAP method: its wire name, the capabilities it needs in `using`, and
/// its argument and response types.
pub trait Method {
    /// The method name, such as `Email/get`.
    const NAME: &'static str;
    /// Capability URIs the method needs. `urn:ietf:params:jmap:core` is
    /// always added to `using` and need not be listed.
    const CAPABILITIES: &'static [&'static str];
    /// The arguments.
    type Args: Serialize;
    /// The successful response.
    type Response: DeserializeOwned;
}

/// `/get` arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetArgs<T> {
    pub account_id: Id,
    /// `None` fetches every object, up to `maxObjectsInGet`. Sent as an
    /// explicit `null`, unless a result reference replaces it.
    #[serde(default)]
    pub ids: Option<Vec<Id>>,
    /// `None` fetches every property.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<Vec<String>>,
    #[serde(skip)]
    _object: PhantomData<fn() -> T>,
}

impl<T> GetArgs<T> {
    /// Fetch the given ids.
    pub fn new(account_id: impl Into<Id>, ids: impl IntoIterator<Item = Id>) -> Self {
        Self {
            account_id: account_id.into(),
            ids: Some(ids.into_iter().collect()),
            properties: None,
            _object: PhantomData,
        }
    }

    /// Fetch every object (`ids: null`).
    pub fn all(account_id: impl Into<Id>) -> Self {
        Self {
            account_id: account_id.into(),
            ids: None,
            properties: None,
            _object: PhantomData,
        }
    }

    /// Fetch ids that a result reference fills in. Same as [`GetArgs::all`];
    /// the reference replaces `ids` when the call is added.
    pub fn by_reference(account_id: impl Into<Id>) -> Self {
        Self::all(account_id)
    }

    /// Limit the properties returned.
    pub fn properties<S: Into<String>>(mut self, properties: impl IntoIterator<Item = S>) -> Self {
        self.properties = Some(properties.into_iter().map(Into::into).collect());
        self
    }
}

/// `/get` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: Deserialize<'de>"))]
pub struct GetResponse<T> {
    pub account_id: Id,
    #[serde(default, deserialize_with = "null_as_default")]
    pub state: State,
    #[serde(default, deserialize_with = "null_as_default")]
    pub list: Vec<T>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub not_found: Vec<Id>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// `/changes` arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangesArgs<T> {
    pub account_id: Id,
    pub since_state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_changes: Option<u64>,
    #[serde(skip)]
    _object: PhantomData<fn() -> T>,
}

impl<T> ChangesArgs<T> {
    /// Changes since `since_state`.
    pub fn new(account_id: impl Into<Id>, since_state: impl Into<State>) -> Self {
        Self {
            account_id: account_id.into(),
            since_state: since_state.into(),
            max_changes: None,
            _object: PhantomData,
        }
    }

    /// Cap the number of ids returned.
    pub fn max_changes(mut self, max: u64) -> Self {
        self.max_changes = Some(max);
        self
    }
}

/// `/changes` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangesResponse<T> {
    pub account_id: Id,
    #[serde(default, deserialize_with = "null_as_default")]
    pub old_state: State,
    #[serde(default, deserialize_with = "null_as_default")]
    pub new_state: State,
    #[serde(default)]
    pub has_more_changes: bool,
    #[serde(default, deserialize_with = "null_as_default")]
    pub created: Vec<Id>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub updated: Vec<Id>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub destroyed: Vec<Id>,
    /// Only `Mailbox/changes` sends this: when set, only these properties
    /// changed on the updated mailboxes (RFC 8621 section 2.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_properties: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: Extra,
    #[serde(skip)]
    _object: PhantomData<fn() -> T>,
}

impl<T> ChangesResponse<T> {
    /// A response with no changes, for servers and fakes to fill in.
    pub fn new(
        account_id: impl Into<Id>,
        old_state: impl Into<State>,
        new_state: impl Into<State>,
    ) -> Self {
        Self {
            account_id: account_id.into(),
            old_state: old_state.into(),
            new_state: new_state.into(),
            has_more_changes: false,
            created: Vec::new(),
            updated: Vec::new(),
            destroyed: Vec::new(),
            updated_properties: None,
            extra: Extra::new(),
            _object: PhantomData,
        }
    }
}

/// `/set` arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: Deserialize<'de>"))]
pub struct SetArgs<T> {
    pub account_id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_in_state: Option<State>,
    /// New objects by creation id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create: Option<BTreeMap<String, T>>,
    /// Patches by object id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<BTreeMap<Id, Patch>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destroy: Option<Vec<Id>>,
}

impl<T> SetArgs<T> {
    /// An empty `/set` for an account.
    pub fn new(account_id: impl Into<Id>) -> Self {
        Self {
            account_id: account_id.into(),
            if_in_state: None,
            create: None,
            update: None,
            destroy: None,
        }
    }

    /// Add an object to create.
    pub fn create(mut self, creation_id: impl Into<String>, object: T) -> Self {
        self.create
            .get_or_insert_with(BTreeMap::new)
            .insert(creation_id.into(), object);
        self
    }

    /// Add a patch. Patches to the same id are merged.
    pub fn update(mut self, id: impl Into<Id>, patch: Patch) -> Self {
        self.update
            .get_or_insert_with(BTreeMap::new)
            .entry(id.into())
            .or_default()
            .merge(patch);
        self
    }

    /// Add an id to destroy.
    pub fn destroy(mut self, id: impl Into<Id>) -> Self {
        self.destroy.get_or_insert_with(Vec::new).push(id.into());
        self
    }

    /// Only apply if the server state still matches.
    pub fn if_in_state(mut self, state: impl Into<State>) -> Self {
        self.if_in_state = Some(state.into());
        self
    }
}

/// `/set` response.
///
/// Fastmail sends `null` for `oldState` and `newState` on MaskedEmail, so
/// both are optional.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: Deserialize<'de>"))]
pub struct SetResponse<T> {
    pub account_id: Id,
    #[serde(default)]
    pub old_state: Option<State>,
    #[serde(default)]
    pub new_state: Option<State>,
    /// Server-set properties of created objects, by creation id. Empty
    /// collections are left out on the wire, which reads as `null`.
    #[serde(
        default,
        deserialize_with = "null_as_default",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub created: BTreeMap<String, T>,
    /// Updated ids. The value holds server-changed properties, or `None`.
    #[serde(
        default,
        deserialize_with = "null_as_default",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub updated: BTreeMap<Id, Option<T>>,
    #[serde(
        default,
        deserialize_with = "null_as_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub destroyed: Vec<Id>,
    #[serde(
        default,
        deserialize_with = "null_as_default",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub not_created: BTreeMap<String, SetError>,
    #[serde(
        default,
        deserialize_with = "null_as_default",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub not_updated: BTreeMap<Id, SetError>,
    #[serde(
        default,
        deserialize_with = "null_as_default",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub not_destroyed: BTreeMap<Id, SetError>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl<T> SetResponse<T> {
    /// Whether every create, update and destroy succeeded.
    pub fn is_complete_success(&self) -> bool {
        self.not_created.is_empty() && self.not_updated.is_empty() && self.not_destroyed.is_empty()
    }
}

/// `/query` arguments. `C` is the filter condition type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(bound(serialize = "C: Serialize", deserialize = "C: Deserialize<'de>"))]
pub struct QueryArgs<C> {
    pub account_id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<Filter<C>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<Vec<Comparator>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_offset: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calculate_total: Option<bool>,
}

impl<C> QueryArgs<C> {
    /// A query with no filter, sort or window.
    pub fn new(account_id: impl Into<Id>) -> Self {
        Self {
            account_id: account_id.into(),
            filter: None,
            sort: None,
            position: None,
            anchor: None,
            anchor_offset: None,
            limit: None,
            calculate_total: None,
        }
    }
}

/// `/query` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResponse {
    pub account_id: Id,
    #[serde(default, deserialize_with = "null_as_default")]
    pub query_state: State,
    #[serde(default)]
    pub can_calculate_changes: bool,
    #[serde(default)]
    pub position: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    pub ids: Vec<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// `/queryChanges` arguments. `C` is the filter condition type. Filter and
/// sort must match the original query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(bound(serialize = "C: Serialize", deserialize = "C: Deserialize<'de>"))]
pub struct QueryChangesArgs<C> {
    pub account_id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<Filter<C>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<Vec<Comparator>>,
    pub since_query_state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_changes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_to_id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calculate_total: Option<bool>,
}

impl<C> QueryChangesArgs<C> {
    /// Changes to a query since `since_query_state`.
    pub fn new(account_id: impl Into<Id>, since_query_state: impl Into<State>) -> Self {
        Self {
            account_id: account_id.into(),
            filter: None,
            sort: None,
            since_query_state: since_query_state.into(),
            max_changes: None,
            up_to_id: None,
            calculate_total: None,
        }
    }
}

/// `/queryChanges` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryChangesResponse {
    pub account_id: Id,
    #[serde(default, deserialize_with = "null_as_default")]
    pub old_query_state: State,
    #[serde(default, deserialize_with = "null_as_default")]
    pub new_query_state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub removed: Vec<Id>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub added: Vec<AddedItem>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// An id added to a query result, at its new index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddedItem {
    pub id: Id,
    pub index: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Mailbox, MaskedEmail};
    use serde_json::json;

    #[test]
    fn get_all_sends_explicit_null() {
        let args = GetArgs::<Mailbox>::all("A1");
        assert_eq!(
            serde_json::to_value(&args).unwrap(),
            json!({"accountId": "A1", "ids": null})
        );
        let args = GetArgs::<Mailbox>::new("A1", ["M1".into()]).properties(["name"]);
        assert_eq!(
            serde_json::to_value(&args).unwrap(),
            json!({"accountId": "A1", "ids": ["M1"], "properties": ["name"]})
        );
    }

    #[test]
    fn set_args_merge_patches_per_id() {
        let args = SetArgs::<Mailbox>::new("A1")
            .update("E1", Patch::new().keyword("$seen", true))
            .update("E1", Patch::new().mailbox("M1", false))
            .destroy("E2");
        assert_eq!(
            serde_json::to_value(&args).unwrap(),
            json!({
                "accountId": "A1",
                "update": {"E1": {"keywords/$seen": true, "mailboxIds/M1": null}},
                "destroy": ["E2"]
            })
        );
    }

    #[test]
    fn set_response_with_nulls() {
        let resp: SetResponse<MaskedEmail> = serde_json::from_value(json!({
            "accountId": "u1",
            "oldState": null,
            "newState": null,
            "created": null,
            "notCreated": {"k1": {"type": "rateLimit"}}
        }))
        .unwrap();
        assert!(resp.created.is_empty());
        assert!(resp.new_state.is_none());
        assert!(!resp.is_complete_success());
    }

    #[test]
    fn changes_response_tolerates_nulls() {
        let resp: ChangesResponse<Mailbox> = serde_json::from_value(json!({
            "accountId": "u1",
            "oldState": "1",
            "newState": "2",
            "hasMoreChanges": false,
            "created": null,
            "updated": ["M1"],
            "destroyed": [],
            "updatedProperties": ["totalEmails"]
        }))
        .unwrap();
        assert!(resp.created.is_empty());
        assert_eq!(resp.updated, vec![Id::from("M1")]);
        assert_eq!(
            resp.updated_properties.as_deref().map(<[String]>::len),
            Some(1)
        );
    }
}
