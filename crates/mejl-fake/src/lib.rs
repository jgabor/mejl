//! A fake Fastmail-like JMAP server for tests, benchmarks and `mejl --demo`.
//!
//! It is written from RFC 8620 (JMAP core), RFC 8621 (JMAP mail) and
//! Fastmail's Masked Email documentation, not from mejl's client code, so it
//! can catch client bugs instead of mirroring them.
//!
//! # Use
//!
//! ```no_run
//! # async fn demo() -> std::io::Result<()> {
//! use mejl_fake::{FakeServer, IncomingMessage, Address};
//! use mejl_jmap::transport::Transport;
//!
//! let server = FakeServer::demo("2026-10-23T14:27:00Z".parse().unwrap());
//! let transport = server.transport(); // in process, no sockets
//! let http = server.serve_http("127.0.0.1:0".parse().unwrap()).await?; // loopback
//! server
//!     .deliver_incoming(IncomingMessage::new(Address::new("Ada", "ada@example.com"), "Hi").text("Hello"))
//!     .ok();
//! # drop((transport, http)); Ok(()) }
//! ```
//!
//! # What it implements
//!
//! - Session at `GET /jmap/session` with core, mail, submission, contacts
//!   and Fastmail's masked email capabilities, and bearer token auth.
//! - `POST /jmap/api`: `Core/echo`, `Mailbox/*`, `Email/get`, `/changes`,
//!   `/query`, `/queryChanges` and `/set`, `Thread/*`, `Identity/*`,
//!   `EmailSubmission/*`, `SearchSnippet/get`, `MaskedEmail/*`,
//!   `AddressBook/get` and `ContactCard/get` and `/changes`, with result
//!   references, creation ids, state strings and change logs.
//! - EventSource push at `GET /jmap/event` and blob upload and download.
//! - OAuth as Fastmail serves it: discovery documents, open registration,
//!   `/oauth/authorize`, the token endpoint at `/oauth/refresh` with
//!   rotating refresh tokens, and `/oauth/revoke`. The fixed token keeps
//!   working with every capability; an OAuth token sees Masked Email only
//!   with that scope.
//! - Fastmail quirks and injected faults, switchable at runtime through
//!   [`FakeServer::quirks_mut`].

#![warn(missing_docs)]

mod api;
mod changes;
mod config;
mod demo;
mod http;
mod ids;
mod model;
mod oauth;
mod push;
mod serve;
mod sim;
mod store;
mod text;
mod transport;

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use jiff::Timestamp;
use serde_json::{Value, json};
use tokio::sync::broadcast;

pub use config::{EventSourceUrl, FakeConfig, Limits, PushShape, Quirks};
pub use http::IN_PROCESS_BASE;
pub use model::{Address, MaskedEmailState};
pub use oauth::OAuthGrant;
pub use serve::HttpHandle;
pub use sim::{AttachmentSpec, ExternalChange, IncomingMessage};
pub use store::MASKED_EMAIL_HEADER;
pub use transport::FakeTransport;

use push::StateChange;
use store::{DeliveryError, NewEmail, Store};

/// Errors from the simulation and inspection helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeError {
    /// An id, mailbox or other reference does not exist.
    NotFound(String),
    /// The message went to a deleted masked address and bounced.
    Bounced(String),
    /// The JMAP call failed with a method-level error.
    Method {
        /// The error `type`, such as `cannotCalculateChanges`.
        kind: String,
        /// The error `description`, when the server gave one.
        description: Option<String>,
    },
    /// A `/set` call rejected the object; holds the SetError JSON.
    SetError(String),
    /// The request was rejected before any method ran.
    Request(String),
}

impl fmt::Display for FakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FakeError::NotFound(what) => write!(f, "not found: {what}"),
            FakeError::Bounced(address) => {
                write!(f, "bounced: {address} is a deleted masked address")
            }
            FakeError::Method { kind, description } => match description {
                Some(d) => write!(f, "method error {kind}: {d}"),
                None => write!(f, "method error {kind}"),
            },
            FakeError::SetError(e) => write!(f, "set error: {e}"),
            FakeError::Request(e) => write!(f, "request error: {e}"),
        }
    }
}

impl std::error::Error for FakeError {}

pub(crate) struct Inner {
    pub(crate) store: Mutex<Store>,
    pub(crate) quirks: Mutex<Quirks>,
    pub(crate) events: broadcast::Sender<StateChange>,
    pub(crate) token: String,
    pub(crate) account_id: String,
    pub(crate) oauth: Mutex<oauth::OAuthState>,
}

/// The fake server. Cheap to clone; every clone shares one account.
#[derive(Clone)]
pub struct FakeServer {
    pub(crate) inner: Arc<Inner>,
}

impl fmt::Debug for FakeServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FakeServer")
            .field("account_id", &self.inner.account_id)
            .finish_non_exhaustive()
    }
}

impl FakeServer {
    /// A server with one account holding the standard mailboxes (Inbox,
    /// Drafts, Sent, Archive, Spam, Trash) and one identity, and no mail.
    pub fn new(config: FakeConfig) -> Self {
        let start = config.now.unwrap_or_else(Timestamp::now);
        let store = Store::new(&config, start);
        Self::from_store(store, &config)
    }

    fn from_store(store: Store, config: &FakeConfig) -> Self {
        let (events, _) = broadcast::channel(1024);
        FakeServer {
            inner: Arc::new(Inner {
                store: Mutex::new(store),
                quirks: Mutex::new(config.quirks.clone()),
                events,
                token: config.token.clone(),
                account_id: config.account_id.clone(),
                oauth: Mutex::new(oauth::OAuthState::default()),
            }),
        }
    }

    /// The demo account for `mejl --demo`: Mira Holt at mira@example.org,
    /// with about 80 messages over the two months before `now`, twenty
    /// masked addresses in every state, three identities and fifteen
    /// contacts. All of it is fictional.
    pub fn demo(now: Timestamp) -> Self {
        let config = FakeConfig {
            username: demo::USERNAME.to_owned(),
            display_name: demo::DISPLAY_NAME.to_owned(),
            now: Some(now),
            ..FakeConfig::default()
        };
        let mut store = Store::new(&config, now);
        demo::fill(&mut store, now);
        Self::from_store(store, &config)
    }

    /// An account with `n` generated messages, for benchmarks. The same
    /// `seed` and `now` always give the same data. Bodies are generated on
    /// demand, so 100k messages fit in modest memory.
    pub fn synthetic(n: usize, seed: u64, now: Timestamp) -> Self {
        let config = FakeConfig {
            now: Some(now),
            ..FakeConfig::default()
        };
        let mut store = Store::new(&config, now);
        sim::fill_synthetic(&mut store, n, seed, now);
        Self::from_store(store, &config)
    }

    /// An in-process [`Transport`](mejl_jmap::transport::Transport).
    pub fn transport(&self) -> FakeTransport {
        FakeTransport::new(self.clone())
    }

    /// The bearer token requests must carry.
    pub fn token(&self) -> &str {
        &self.inner.token
    }

    /// The account id.
    pub fn account_id(&self) -> &str {
        &self.inner.account_id
    }

    /// The session URL for the in-process transport. For the loopback
    /// server use [`HttpHandle::session_url`].
    pub fn session_url(&self) -> String {
        format!("{IN_PROCESS_BASE}/jmap/session")
    }

    /// The quirk switches, for reading or changing at runtime. Do not hold
    /// the guard across an `.await`.
    pub fn quirks_mut(&self) -> MutexGuard<'_, Quirks> {
        self.inner.quirks.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn quirks_snapshot(&self) -> Quirks {
        self.quirks_mut().clone()
    }

    pub(crate) fn with_store<R>(&self, f: impl FnOnce(&mut Store) -> R) -> R {
        let mut store = self.inner.store.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut store)
    }

    /// Runs a mutation and then pushes a state change for every type whose
    /// state moved. The lock is released before anyone is notified.
    pub(crate) fn commit<R>(&self, f: impl FnOnce(&mut Store) -> R) -> R {
        let masked = self.quirks_mut().masked_email_without_state;
        let (result, changed) = self.with_store(|store| {
            let before = store.push_states(masked);
            let result = f(store);
            store.flush_counts();
            let after = store.push_states(masked);
            let changed: BTreeMap<String, String> = after
                .into_iter()
                .filter(|(k, v)| before.get(k) != Some(v))
                .collect();
            (result, changed)
        });
        if !changed.is_empty() {
            // No receivers is fine: nobody is listening for push.
            let _ = self.inner.events.send(StateChange { changed });
        }
        result
    }

    pub(crate) fn body_limit(&self) -> usize {
        self.with_store(|s| {
            let limit = s.limits.max_size_upload.max(s.limits.max_size_request);
            usize::try_from(limit)
                .unwrap_or(usize::MAX)
                .saturating_add(1)
        })
    }

    /// Simulates a message arriving from outside. Returns the new email id.
    ///
    /// States advance and push fires as for real mail. Mail to a masked
    /// address gets the [`MASKED_EMAIL_HEADER`]; a deleted mask bounces with
    /// [`FakeError::Bounced`] and a disabled one files the mail in Trash.
    pub fn deliver_incoming(&self, msg: IncomingMessage) -> Result<String, FakeError> {
        self.commit(|store| {
            let mailbox = match &msg.mailbox {
                Some(key) => Some(
                    store
                        .find_mailbox(key)
                        .ok_or_else(|| FakeError::NotFound(format!("mailbox {key}")))?,
                ),
                None => None,
            };
            let to = if msg.to.is_empty() {
                vec![Address::new(
                    store.display_name.clone(),
                    store.username.clone(),
                )]
            } else {
                msg.to.clone()
            };
            let mut recipients: Vec<String> = to.iter().map(|a| a.email.clone()).collect();
            recipients.extend(msg.cc.iter().map(|a| a.email.clone()));
            recipients.extend(msg.envelope_to.iter().cloned());
            let (in_reply_to, references) = match &msg.in_reply_to {
                Some(parent_id) => {
                    let parent = store
                        .emails
                        .get(parent_id)
                        .ok_or_else(|| FakeError::NotFound(format!("email {parent_id}")))?;
                    let parent_msgid = parent.message_id.clone().unwrap_or_default();
                    let mut refs = parent.references.clone().unwrap_or_default();
                    refs.extend(parent_msgid.iter().cloned());
                    (Some(parent_msgid), Some(refs))
                }
                None => (None, None),
            };
            let new = NewEmail {
                mailbox_ids: mailbox.into_iter().collect(),
                keywords: sim::keywords(&msg.keywords),
                received_at: msg.received_at,
                sent_at: msg.received_at,
                in_reply_to,
                references,
                from: Some(vec![msg.from.clone()]),
                to: Some(to),
                cc: if msg.cc.is_empty() {
                    None
                } else {
                    Some(msg.cc.clone())
                },
                subject: Some(msg.subject.clone()),
                extra_headers: msg.headers.clone(),
                text: msg
                    .text
                    .clone()
                    .map(model::Body::Text)
                    .or_else(|| msg.html.is_none().then(|| model::Body::Text(String::new()))),
                html: msg.html.clone(),
                attachments: msg
                    .attachments
                    .iter()
                    .map(|a| model::Attachment {
                        name: a.name.clone(),
                        mime: a.mime.clone(),
                        size: a.size,
                        inline: false,
                        cid: None,
                        upload: None,
                    })
                    .collect(),
                ..NewEmail::default()
            };
            store.deliver(new, &recipients).map_err(|e| match e {
                DeliveryError::Bounced(a) => FakeError::Bounced(a),
                DeliveryError::NoMailbox => FakeError::NotFound("inbox or trash mailbox".into()),
            })
        })
    }

    /// Applies a change as if another client made it. States advance and
    /// push fires.
    pub fn mutate_from_elsewhere(&self, change: ExternalChange) -> Result<(), FakeError> {
        let escape = |s: &str| s.replace('~', "~0").replace('/', "~1");
        let (method, args) = match change {
            // Roles cannot be changed through Mailbox/set in this server.
            ExternalChange::ClearMailboxRole { mailbox_id } => {
                let found =
                    self.commit(|store| store.update_mailbox(&mailbox_id, |mb| mb.role = None));
                return if found {
                    Ok(())
                } else {
                    Err(FakeError::NotFound(mailbox_id))
                };
            }
            ExternalChange::SetKeyword {
                email_id,
                keyword,
                value,
            } => (
                "Email/set",
                json!({"update": {email_id: {format!("keywords/{}", escape(&keyword)): if value { json!(true) } else { Value::Null }}}}),
            ),
            ExternalChange::MoveToMailbox {
                email_id,
                mailbox_id,
            } => (
                "Email/set",
                json!({"update": {email_id: {"mailboxIds": {mailbox_id: true}}}}),
            ),
            ExternalChange::AddToMailbox {
                email_id,
                mailbox_id,
            } => (
                "Email/set",
                json!({"update": {email_id: {format!("mailboxIds/{}", escape(&mailbox_id)): true}}}),
            ),
            ExternalChange::RemoveFromMailbox {
                email_id,
                mailbox_id,
            } => (
                "Email/set",
                json!({"update": {email_id: {format!("mailboxIds/{}", escape(&mailbox_id)): null}}}),
            ),
            ExternalChange::DestroyEmail { email_id } => {
                ("Email/set", json!({"destroy": [email_id]}))
            }
            ExternalChange::RenameMailbox { mailbox_id, name } => (
                "Mailbox/set",
                json!({"update": {mailbox_id: {"name": name}}}),
            ),
            ExternalChange::SetMaskedEmailState { id, state } => (
                "MaskedEmail/set",
                json!({"update": {id: {"state": state.as_str()}}}),
            ),
        };
        let response = self.call(method, args)?;
        for key in ["notUpdated", "notDestroyed", "notCreated"] {
            if let Some(Value::Object(errors)) = response.get(key)
                && let Some((id, err)) = errors.iter().next()
            {
                if err.get("type").and_then(Value::as_str) == Some("notFound") {
                    return Err(FakeError::NotFound(id.clone()));
                }
                return Err(FakeError::SetError(err.to_string()));
            }
        }
        Ok(())
    }

    /// Runs one JMAP method directly, with every capability, and returns
    /// the first response's arguments. `accountId` is filled in when
    /// missing. Useful for setting up and inspecting state in tests; it
    /// skips HTTP, auth and the HTTP quirks but advances states and fires
    /// push like any client call.
    pub fn call(&self, method: &str, args: Value) -> Result<Value, FakeError> {
        let mut args = match args {
            Value::Object(m) => m,
            Value::Null => serde_json::Map::new(),
            _ => return Err(FakeError::Request("arguments must be an object".into())),
        };
        args.entry("accountId")
            .or_insert_with(|| json!(self.inner.account_id));
        let request = json!({
            "using": api::CAPABILITIES,
            "methodCalls": [[method, args, "c0"]],
        });
        let body = request.to_string();
        let response = self
            .commit(|store| api::process(store, &self.inner.quirks, body.as_bytes()))
            .map_err(|e| FakeError::Request(e.to_problem().to_string()))?;
        let first = response
            .get("methodResponses")
            .and_then(|r| r.get(0))
            .ok_or_else(|| FakeError::Request("no method response".into()))?;
        let name = first.get(0).and_then(Value::as_str).unwrap_or("");
        let out = first.get(1).cloned().unwrap_or(Value::Null);
        if name == "error" {
            return Err(FakeError::Method {
                kind: out
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                description: out
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            });
        }
        Ok(out)
    }

    /// The id of a mailbox found by role (`inbox`, `junk`), name or path
    /// (`Projects/Garden club`).
    pub fn mailbox_id(&self, role_or_path: &str) -> Option<String> {
        self.with_store(|s| s.find_mailbox(role_or_path))
    }

    /// Forgets all change history, so every client's stored states become
    /// unknown and the next `/changes` gets `cannotCalculateChanges`.
    pub fn expire_change_logs(&self) {
        self.with_store(|s| s.logs.expire_all());
    }
}
