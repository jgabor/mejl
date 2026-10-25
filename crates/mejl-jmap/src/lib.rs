//! JMAP client for mejl.
//!
//! - [`types`]: wire types for the session, mail, submission, contacts and
//!   Fastmail's Masked Email. Unknown properties are kept in `extra` maps.
//! - [`method`] and [`methods`]: the [`Method`] trait, the standard
//!   `/get`, `/changes`, `/query`, `/queryChanges` and `/set` shapes, and
//!   one marker type per method.
//! - [`request`]: a builder with typed call handles and result references,
//!   and typed decoding of responses.
//! - [`client`]: session fetch, API calls, blob upload and download over a
//!   [`Transport`]. [`http::HttpTransport`] is the reqwest implementation.
//! - [`push`]: the EventSource URL, an SSE parser and push events.
//! - [`oauth`]: OAuth discovery, registration, PKCE and the token
//!   endpoint. [`credentials`]: the self-refreshing OAuth login a
//!   [`Client`] can carry, and the [`TokenStore`] it saves through.
//!
//! The crate has no UI or storage concerns.

pub mod client;
pub mod credentials;
pub mod error;
pub mod filter;
pub mod http;
pub mod method;
pub mod methods;
pub mod oauth;
pub mod patch;
pub mod push;
pub mod request;
pub mod secret;
pub mod session;
pub mod sse;
pub mod template;
pub mod transport;
pub mod types;

pub use client::{Client, USER_AGENT, UploadResponse};
pub use credentials::{OAuthCredentials, TokenStore};
pub use error::{
    Error, MethodError, MethodErrorType, OAuthError, OAuthErrorKind, RequestError,
    RequestErrorType, SetError, SetErrorType,
};
pub use filter::{Comparator, EmailFilter, EmailFilterCondition, Filter, Operator};
pub use http::{HttpConfig, HttpTransport};
pub use method::Method;
pub use oauth::{Callback, Pkce, ServerMetadata, TokenSet};
pub use patch::Patch;
pub use push::{PushError, PushEvent, PushOptions, PushStream};
pub use request::{CallHandle, Invocation, Request, RequestBuilder, Response, ResultReference};
pub use secret::Secret;
pub use session::{Account, CoreCapability, CoreLimits, Session, capability};
pub use transport::Transport;
pub use types::{Id, State};
