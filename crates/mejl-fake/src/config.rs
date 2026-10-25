//! Server configuration and the runtime quirk switches.

use std::time::Duration;

use jiff::Timestamp;

/// Configuration for a [`FakeServer`](crate::FakeServer).
///
/// The defaults describe a small, empty account on a server that follows the
/// RFCs. Use [`Quirks`] to switch on Fastmail's reported deviations.
#[derive(Debug, Clone)]
pub struct FakeConfig {
    /// The bearer token every request must carry.
    pub token: String,
    /// The JMAP account id.
    pub account_id: String,
    /// The account's own address. It is the session `username` and the
    /// address of the default identity.
    pub username: String,
    /// Display name for the default identity.
    pub display_name: String,
    /// The domain new masked addresses are created in.
    pub masked_email_domain: String,
    /// The value `createdBy` gets on masked addresses made through the API.
    pub masked_email_created_by: String,
    /// A fixed start time. With `Some`, the server clock starts here and moves
    /// one second forward on every read, so runs are deterministic. With
    /// `None`, the server reads the system clock once at startup and then
    /// does the same.
    pub now: Option<Timestamp>,
    /// Limits advertised in the session and enforced on every request.
    pub limits: Limits,
    /// How many changes each type's change log keeps. A `/changes` call with
    /// an older state gets `cannotCalculateChanges`.
    pub change_log_limit: usize,
    /// Initial quirk switches. They can be changed later with
    /// [`FakeServer::quirks_mut`](crate::FakeServer::quirks_mut).
    pub quirks: Quirks,
}

impl Default for FakeConfig {
    fn default() -> Self {
        FakeConfig {
            token: "mejl-fake-token".to_owned(),
            account_id: "u1f2e3d4c".to_owned(),
            username: "user@example.org".to_owned(),
            display_name: "Test User".to_owned(),
            masked_email_domain: "example.org".to_owned(),
            masked_email_created_by: "mejl".to_owned(),
            now: None,
            limits: Limits::default(),
            change_log_limit: 10_000,
            quirks: Quirks::default(),
        }
    }
}

/// Session limits. The defaults match the values in published Fastmail
/// session dumps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Largest accepted upload in bytes.
    pub max_size_upload: u64,
    /// Advertised only. The fake does not limit concurrent uploads.
    pub max_concurrent_upload: u64,
    /// Largest accepted API request body in bytes.
    pub max_size_request: u64,
    /// Advertised only. The fake does not limit concurrency.
    pub max_concurrent_requests: u64,
    /// Most method calls in one request.
    pub max_calls_in_request: u64,
    /// Most ids in one `/get`, and most objects `ids: null` may return.
    pub max_objects_in_get: u64,
    /// Most creates, updates and destroys together in one `/set`.
    pub max_objects_in_set: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_size_upload: 250_000_000,
            max_concurrent_upload: 10,
            max_size_request: 10_000_000,
            max_concurrent_requests: 10,
            max_calls_in_request: 50,
            max_objects_in_get: 4096,
            max_objects_in_set: 4096,
        }
    }
}

/// The JSON shape of push `state` events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PushShape {
    /// RFC 8620 section 7.1: `{"@type": "StateChange", "changed": {...}}`.
    #[default]
    Rfc,
    /// The shape one developer saw from Fastmail in 2023 and Fastmail staff
    /// called a bug: `{"type": "connect", "changed": {...}}`, with no
    /// `@type`. The `changed` map is the same.
    FastmailConnect,
}

/// How the session advertises `eventSourceUrl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EventSourceUrl {
    /// An RFC 6570 template with `{types}`, `{closeafter}` and `{ping}`, as
    /// RFC 8620 requires.
    #[default]
    Template,
    /// A plain URL with no template variables, as in the 2024 Fastmail
    /// session dump. Clients must append the query parameters themselves.
    Plain,
}

/// Runtime switches for reported Fastmail quirks and injected faults.
///
/// Counters such as [`Quirks::http_429_next`] count down as requests arrive,
/// so a test sets one and the next N requests see the fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quirks {
    /// Shape of push `state` events.
    pub push_shape: PushShape,
    /// Template or plain `eventSourceUrl` in the session.
    pub event_source_url: EventSourceUrl,
    /// Fastmail's reported masked email behaviour: `MaskedEmail/get` returns
    /// `state: ""`, `MaskedEmail/set` returns `oldState` and `newState` as
    /// null, `MaskedEmail/changes` always fails with
    /// `cannotCalculateChanges`, and push never names `MaskedEmail`.
    pub masked_email_without_state: bool,
    /// The next N masked email creates fail with a `rateLimit` SetError.
    pub masked_email_rate_limit_next: u32,
    /// The next N HTTP requests get `429 Too Many Requests`.
    pub http_429_next: u32,
    /// The `Retry-After` value, in seconds, sent with 429 and 503 replies.
    pub retry_after_seconds: u32,
    /// The next N HTTP requests get `503 Service Unavailable`.
    pub fail_next_with_503: u32,
    /// Artificial latency added before every HTTP request is handled.
    pub latency: Duration,
    /// Each push stream ends abruptly after this many events, pings
    /// included, as a dropped connection would.
    pub drop_push_after_events: Option<u32>,
    /// Split every push frame into chunks of at most this many bytes, so the
    /// client's SSE parser sees frames cut at awkward places.
    pub push_chunk_size: Option<usize>,
    /// The next N API requests are processed and committed, then the
    /// response is lost: the in-process transport reports a closed
    /// connection and the loopback server cuts the response body. This is
    /// the case of a connection that drops after the server acted, which a
    /// client must not answer by repeating a non-idempotent call such as a
    /// send.
    pub lose_api_response_next: u32,
    /// Capability URIs left out of the session resource: from the server
    /// capabilities, the account's capabilities and the primary accounts,
    /// as a token without that scope would see it. The API still answers
    /// methods of these capabilities; this only changes what is advertised.
    pub session_without: Vec<String>,
    /// The next N JMAP requests get `403 Forbidden`, as a token that lacks
    /// a needed scope would.
    pub http_403_next: u32,
    /// How long new OAuth access tokens live. Expiry is checked against
    /// the fake's clock, which moves one second each time the server reads
    /// it. A lifetime under the client's 60 second margin makes the client
    /// refresh before every request.
    pub access_token_lifetime: Duration,
    /// `/oauth/authorize` answers `invalid_scope` when the Masked Email
    /// scope is asked for.
    pub refuse_masked_scope: bool,
    /// `/oauth/register` refuses every registration.
    pub refuse_registration: bool,
    /// The next N refreshes get `invalid_grant` with `temporary: true`, as
    /// a locked account would. Nothing changes on the server.
    pub refresh_temporary_invalid_grant_next: u32,
    /// The next N refreshes take effect, then the response is lost, as in
    /// [`Quirks::lose_api_response_next`].
    pub lose_refresh_response_next: u32,
}

impl Default for Quirks {
    fn default() -> Self {
        Quirks {
            push_shape: PushShape::Rfc,
            event_source_url: EventSourceUrl::Template,
            masked_email_without_state: false,
            masked_email_rate_limit_next: 0,
            http_429_next: 0,
            retry_after_seconds: 1,
            fail_next_with_503: 0,
            latency: Duration::ZERO,
            drop_push_after_events: None,
            push_chunk_size: None,
            lose_api_response_next: 0,
            session_without: Vec::new(),
            http_403_next: 0,
            access_token_lifetime: Duration::from_secs(3600),
            refuse_masked_scope: false,
            refuse_registration: false,
            refresh_temporary_invalid_grant_next: 0,
            lose_refresh_response_next: 0,
        }
    }
}

impl Quirks {
    /// The quirks reported for Fastmail by third parties: a plain
    /// `eventSourceUrl` and masked email without state strings. The
    /// non-standard push shape is left off, because Fastmail staff called it
    /// a bug.
    pub fn fastmail_reported() -> Self {
        Quirks {
            event_source_url: EventSourceUrl::Plain,
            masked_email_without_state: true,
            ..Quirks::default()
        }
    }
}
