//! The JMAP Session resource (RFC 8620 section 2) and capability URIs.

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::core::{Extra, Id, State};

/// Capability URIs mejl knows about.
pub mod capability {
    /// RFC 8620 core. Always sent in `using`.
    pub const CORE: &str = "urn:ietf:params:jmap:core";
    /// RFC 8621 mail.
    pub const MAIL: &str = "urn:ietf:params:jmap:mail";
    /// RFC 8621 submission (identities and sending).
    pub const SUBMISSION: &str = "urn:ietf:params:jmap:submission";
    /// RFC 8621 vacation response.
    pub const VACATION_RESPONSE: &str = "urn:ietf:params:jmap:vacationresponse";
    /// RFC 9610 contacts.
    pub const CONTACTS: &str = "urn:ietf:params:jmap:contacts";
    /// Fastmail's documented Masked Email extension.
    pub const MASKED_EMAIL: &str = "https://www.fastmail.com/dev/maskedemail";
}

/// The Session object a server returns from its session URL.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    /// Server capabilities by URI. Values are capability specific.
    #[serde(default)]
    pub capabilities: BTreeMap<String, Value>,
    /// Accounts the credentials can access.
    #[serde(default)]
    pub accounts: BTreeMap<Id, Account>,
    /// The primary account per capability URI.
    #[serde(default)]
    pub primary_accounts: BTreeMap<String, Id>,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub api_url: String,
    /// URI template with `accountId`, `blobId`, `type` and `name`.
    #[serde(default)]
    pub download_url: String,
    /// URI template with `accountId`.
    #[serde(default)]
    pub upload_url: String,
    /// URI template with `types`, `closeafter` and `ping`, or a plain URL.
    #[serde(default)]
    pub event_source_url: String,
    /// Changes whenever anything in the session changes.
    #[serde(default)]
    pub state: State,
    #[serde(flatten)]
    pub extra: Extra,
}

/// One account in the session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub is_personal: bool,
    #[serde(default)]
    pub is_read_only: bool,
    /// Capability URIs this account supports, with per-account settings.
    #[serde(default)]
    pub account_capabilities: BTreeMap<String, Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The `urn:ietf:params:jmap:core` capability object.
///
/// Every limit is optional here because real sessions omit some of them. Use
/// [`Session::core_limits`] for values with fallbacks filled in.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreCapability {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_size_upload: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_upload: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_size_request: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_requests: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_calls_in_request: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_objects_in_get: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_objects_in_set: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collation_algorithms: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Core limits with every value present.
///
/// A missing or zero value falls back to the minimum RFC 8620 section 2
/// says a server should support, so callers stay within what any compliant
/// server accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreLimits {
    pub max_size_upload: u64,
    pub max_concurrent_upload: u64,
    pub max_size_request: u64,
    pub max_concurrent_requests: u64,
    pub max_calls_in_request: u64,
    pub max_objects_in_get: u64,
    pub max_objects_in_set: u64,
}

impl CoreLimits {
    /// The RFC 8620 suggested minimums.
    pub const FALLBACK: CoreLimits = CoreLimits {
        max_size_upload: 50_000_000,
        max_concurrent_upload: 4,
        max_size_request: 10_000_000,
        max_concurrent_requests: 4,
        max_calls_in_request: 16,
        max_objects_in_get: 500,
        max_objects_in_set: 500,
    };
}

impl Default for CoreLimits {
    fn default() -> Self {
        Self::FALLBACK
    }
}

impl CoreCapability {
    /// The limits with fallbacks for missing values.
    pub fn limits(&self) -> CoreLimits {
        let pick = |value: Option<u64>, fallback: u64| match value {
            Some(v) if v > 0 => v,
            _ => fallback,
        };
        let f = CoreLimits::FALLBACK;
        CoreLimits {
            max_size_upload: pick(self.max_size_upload, f.max_size_upload),
            max_concurrent_upload: pick(self.max_concurrent_upload, f.max_concurrent_upload),
            max_size_request: pick(self.max_size_request, f.max_size_request),
            max_concurrent_requests: pick(self.max_concurrent_requests, f.max_concurrent_requests),
            max_calls_in_request: pick(self.max_calls_in_request, f.max_calls_in_request),
            max_objects_in_get: pick(self.max_objects_in_get, f.max_objects_in_get),
            max_objects_in_set: pick(self.max_objects_in_set, f.max_objects_in_set),
        }
    }
}

impl Session {
    /// Whether the server advertises this capability.
    pub fn has_capability(&self, uri: &str) -> bool {
        self.capabilities.contains_key(uri)
    }

    /// The core capability, or `None` if absent or malformed.
    pub fn core_capability(&self) -> Option<CoreCapability> {
        self.capability_as(capability::CORE)
    }

    /// Core limits with fallbacks. Never fails: a session without a usable
    /// core capability gets the RFC minimums.
    pub fn core_limits(&self) -> CoreLimits {
        self.core_capability()
            .map(|c| c.limits())
            .unwrap_or_default()
    }

    /// Decode a server capability object into `T`.
    pub fn capability_as<T: DeserializeOwned>(&self, uri: &str) -> Option<T> {
        let value = self.capabilities.get(uri)?;
        T::deserialize(value).ok()
    }

    /// The primary account for a capability.
    pub fn primary_account(&self, uri: &str) -> Option<&Id> {
        self.primary_accounts.get(uri)
    }

    /// An account by id.
    pub fn account(&self, id: &str) -> Option<&Account> {
        self.accounts.get(id)
    }

    /// Whether the account lists this capability.
    pub fn account_has_capability(&self, account_id: &str, uri: &str) -> bool {
        self.account(account_id)
            .is_some_and(|a| a.account_capabilities.contains_key(uri))
    }

    /// Decode an account capability object into `T`.
    pub fn account_capability_as<T: DeserializeOwned>(
        &self,
        account_id: &str,
        uri: &str,
    ) -> Option<T> {
        let value = self.account(account_id)?.account_capabilities.get(uri)?;
        T::deserialize(value).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_fall_back_for_missing_and_zero() {
        let cap = CoreCapability {
            max_objects_in_get: Some(4096),
            max_calls_in_request: Some(0),
            ..Default::default()
        };
        let limits = cap.limits();
        assert_eq!(limits.max_objects_in_get, 4096);
        assert_eq!(limits.max_calls_in_request, 16);
        assert_eq!(limits.max_size_request, 10_000_000);
    }

    #[test]
    fn malformed_core_capability_gives_fallback_limits() {
        let mut session = Session::default();
        session
            .capabilities
            .insert(capability::CORE.into(), Value::String("nope".into()));
        assert!(session.core_capability().is_none());
        assert_eq!(session.core_limits(), CoreLimits::FALLBACK);
    }

    #[test]
    fn empty_object_parses() {
        let session: Session = serde_json::from_str("{}").unwrap();
        assert!(session.api_url.is_empty());
        assert_eq!(session.core_limits(), CoreLimits::FALLBACK);
    }
}
