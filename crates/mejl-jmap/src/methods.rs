//! The methods mejl uses, one marker type each.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::filter::{EmailFilter, EmailFilterCondition};
use crate::method::{
    ChangesArgs, ChangesResponse, GetArgs, GetResponse, Method, QueryArgs, QueryChangesArgs,
    QueryChangesResponse, QueryResponse, SetArgs, SetResponse,
};
use crate::patch::Patch;
use crate::session::capability::{CONTACTS, CORE, MAIL, MASKED_EMAIL, SUBMISSION};
use crate::types::core::{Id, null_as_default};
use crate::types::{
    ContactCard, Email, EmailSubmission, Identity, Mailbox, MaskedEmail, SearchSnippet, Thread,
};

macro_rules! method {
    ($(#[$meta:meta])* $marker:ident, $name:literal, [$($cap:expr),*], $args:ty, $resp:ty) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
        pub struct $marker;

        impl Method for $marker {
            const NAME: &'static str = $name;
            const CAPABILITIES: &'static [&'static str] = &[$($cap),*];
            type Args = $args;
            type Response = $resp;
        }
    };
}

method!(
    /// `Core/echo`: returns its arguments. Useful as a connectivity check.
    CoreEcho, "Core/echo", [CORE], Map<String, Value>, Map<String, Value>
);

method!(
    MailboxGet,
    "Mailbox/get",
    [MAIL],
    GetArgs<Mailbox>,
    GetResponse<Mailbox>
);
method!(
    MailboxChanges,
    "Mailbox/changes",
    [MAIL],
    ChangesArgs<Mailbox>,
    ChangesResponse<Mailbox>
);
method!(
    MailboxSet,
    "Mailbox/set",
    [MAIL],
    MailboxSetArgs,
    SetResponse<Mailbox>
);

method!(
    EmailGet,
    "Email/get",
    [MAIL],
    EmailGetArgs,
    GetResponse<Email>
);
method!(
    EmailChanges,
    "Email/changes",
    [MAIL],
    ChangesArgs<Email>,
    ChangesResponse<Email>
);
method!(
    EmailQuery,
    "Email/query",
    [MAIL],
    EmailQueryArgs,
    QueryResponse
);
method!(
    EmailQueryChanges,
    "Email/queryChanges",
    [MAIL],
    EmailQueryChangesArgs,
    QueryChangesResponse
);
method!(
    EmailSet,
    "Email/set",
    [MAIL],
    SetArgs<Email>,
    SetResponse<Email>
);

method!(
    ThreadGet,
    "Thread/get",
    [MAIL],
    GetArgs<Thread>,
    GetResponse<Thread>
);
method!(
    ThreadChanges,
    "Thread/changes",
    [MAIL],
    ChangesArgs<Thread>,
    ChangesResponse<Thread>
);

method!(
    IdentityGet,
    "Identity/get",
    [SUBMISSION],
    GetArgs<Identity>,
    GetResponse<Identity>
);
method!(
    IdentityChanges,
    "Identity/changes",
    [SUBMISSION],
    ChangesArgs<Identity>,
    ChangesResponse<Identity>
);

method!(
    /// `EmailSubmission/set`. Lists the mail capability too, because
    /// `onSuccessUpdateEmail` and `onSuccessDestroyEmail` run an implicit
    /// `Email/set`.
    EmailSubmissionSet,
    "EmailSubmission/set",
    [SUBMISSION, MAIL],
    EmailSubmissionSetArgs,
    SetResponse<EmailSubmission>
);

method!(
    SearchSnippetGet,
    "SearchSnippet/get",
    [MAIL],
    SearchSnippetGetArgs,
    SearchSnippetGetResponse
);

method!(
    MaskedEmailGet,
    "MaskedEmail/get",
    [MASKED_EMAIL],
    GetArgs<MaskedEmail>,
    GetResponse<MaskedEmail>
);
method!(
    MaskedEmailSet,
    "MaskedEmail/set",
    [MASKED_EMAIL],
    SetArgs<MaskedEmail>,
    SetResponse<MaskedEmail>
);

method!(
    ContactCardGet,
    "ContactCard/get",
    [CONTACTS],
    GetArgs<ContactCard>,
    GetResponse<ContactCard>
);
method!(
    ContactCardChanges,
    "ContactCard/changes",
    [CONTACTS],
    ChangesArgs<ContactCard>,
    ChangesResponse<ContactCard>
);

/// `Mailbox/set` arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailboxSetArgs {
    #[serde(flatten)]
    pub set: SetArgs<Mailbox>,
    /// Destroy a mailbox even if it has email, removing the email from it
    /// (and destroying email in no other mailbox).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_destroy_remove_emails: Option<bool>,
}

impl From<SetArgs<Mailbox>> for MailboxSetArgs {
    fn from(set: SetArgs<Mailbox>) -> Self {
        Self {
            set,
            on_destroy_remove_emails: None,
        }
    }
}

/// `Email/get` arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailGetArgs {
    #[serde(flatten)]
    pub get: GetArgs<Email>,
    /// Properties to return for each body part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_properties: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_text_body_values: Option<bool>,
    // RFC 8621 spells this with "HTML" in capitals.
    #[serde(
        rename = "fetchHTMLBodyValues",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub fetch_html_body_values: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_all_body_values: Option<bool>,
    /// Truncate each body value to this many bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_body_value_bytes: Option<u64>,
}

impl From<GetArgs<Email>> for EmailGetArgs {
    fn from(get: GetArgs<Email>) -> Self {
        Self {
            get,
            body_properties: None,
            fetch_text_body_values: None,
            fetch_html_body_values: None,
            fetch_all_body_values: None,
            max_body_value_bytes: None,
        }
    }
}

/// `Email/query` arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailQueryArgs {
    #[serde(flatten)]
    pub query: QueryArgs<EmailFilterCondition>,
    /// Return one email per thread. `Email/queryChanges` must use the same
    /// value as the query it updates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collapse_threads: Option<bool>,
}

impl From<QueryArgs<EmailFilterCondition>> for EmailQueryArgs {
    fn from(query: QueryArgs<EmailFilterCondition>) -> Self {
        Self {
            query,
            collapse_threads: None,
        }
    }
}

/// `Email/queryChanges` arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailQueryChangesArgs {
    #[serde(flatten)]
    pub query_changes: QueryChangesArgs<EmailFilterCondition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collapse_threads: Option<bool>,
}

impl From<QueryChangesArgs<EmailFilterCondition>> for EmailQueryChangesArgs {
    fn from(query_changes: QueryChangesArgs<EmailFilterCondition>) -> Self {
        Self {
            query_changes,
            collapse_threads: None,
        }
    }
}

/// `EmailSubmission/set` arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailSubmissionSetArgs {
    #[serde(flatten)]
    pub set: SetArgs<EmailSubmission>,
    /// Patches applied to the email of each successful submission. Keys are
    /// submission ids or `#creationId`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_success_update_email: Option<BTreeMap<String, Patch>>,
    /// Emails to destroy after a successful submission. Entries are
    /// submission ids or `#creationId`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_success_destroy_email: Option<Vec<String>>,
}

impl From<SetArgs<EmailSubmission>> for EmailSubmissionSetArgs {
    fn from(set: SetArgs<EmailSubmission>) -> Self {
        Self {
            set,
            on_success_update_email: None,
            on_success_destroy_email: None,
        }
    }
}

impl EmailSubmissionSetArgs {
    /// Patch the email once the submission created as `creation_id`
    /// succeeds, for example to move it from Drafts to Sent.
    pub fn on_success_update(mut self, creation_id: &str, patch: Patch) -> Self {
        self.on_success_update_email
            .get_or_insert_with(BTreeMap::new)
            .entry(format!("#{creation_id}"))
            .or_default()
            .merge(patch);
        self
    }
}

/// `SearchSnippet/get` arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchSnippetGetArgs {
    pub account_id: Id,
    /// The same filter as the `Email/query` the ids came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<EmailFilter>,
    /// Sent as `null` unless a result reference replaces it.
    #[serde(default)]
    pub email_ids: Option<Vec<Id>>,
}

/// `SearchSnippet/get` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchSnippetGetResponse {
    pub account_id: Id,
    #[serde(default, deserialize_with = "null_as_default")]
    pub list: Vec<SearchSnippet>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub not_found: Vec<Id>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn capabilities_per_method() {
        assert_eq!(IdentityGet::CAPABILITIES, &[SUBMISSION]);
        assert!(EmailSubmissionSet::CAPABILITIES.contains(&MAIL));
        assert_eq!(MaskedEmailSet::CAPABILITIES, &[MASKED_EMAIL]);
        assert_eq!(ContactCardChanges::CAPABILITIES, &[CONTACTS]);
        assert_eq!(EmailQueryChanges::NAME, "Email/queryChanges");
    }

    #[test]
    fn email_get_uses_rfc_spelling() {
        let mut args =
            EmailGetArgs::from(GetArgs::new("A1", ["E1".into()]).properties(["htmlBody"]));
        args.fetch_html_body_values = Some(true);
        args.max_body_value_bytes = Some(256);
        assert_eq!(
            serde_json::to_value(&args).unwrap(),
            json!({
                "accountId": "A1",
                "ids": ["E1"],
                "properties": ["htmlBody"],
                "fetchHTMLBodyValues": true,
                "maxBodyValueBytes": 256
            })
        );
    }

    #[test]
    fn on_success_update_keys_reference_creation() {
        let args = EmailSubmissionSetArgs::from(SetArgs::new("A1")).on_success_update(
            "s1",
            Patch::new()
                .mailbox("drafts", false)
                .keyword("$draft", false),
        );
        assert_eq!(
            serde_json::to_value(&args).unwrap(),
            json!({
                "accountId": "A1",
                "onSuccessUpdateEmail": {"#s1": {"mailboxIds/drafts": null, "keywords/$draft": null}}
            })
        );
    }

    #[test]
    fn flattened_args_deserialize() {
        let args: EmailQueryArgs = serde_json::from_value(json!({
            "accountId": "A1",
            "filter": {"inMailbox": "inbox"},
            "sort": [{"property": "receivedAt", "isAscending": false}],
            "collapseThreads": true,
            "limit": 10
        }))
        .unwrap();
        assert_eq!(args.collapse_threads, Some(true));
        assert_eq!(args.query.limit, Some(10));
    }
}
