//! `mejl login`: paste an API token, check it, store it.
//!
//! 1. The token is read without echo ([`TokenInput`] in raw mode, or one
//!    line from stdin when it is not a terminal, for scripts).
//! 2. It is checked by fetching the JMAP session with it ([`verify_token`]).
//! 3. mejl reports the account and which capabilities the token grants
//!    ([`SessionReport`]): mail is required; sending, Masked Email and
//!    contacts are optional, and each missing one is explained.
//! 4. The token goes into the secret store under the account name.
//!
//! The session's `username` is the user's address. It is printed for the
//! user to confirm, and never written to any file.
//!
//! # OAuth
//!
//! Only API tokens exist today ([`LoginMethod::ApiToken`]). OAuth (dynamic
//! registration, loopback redirect, PKCE; see `docs/architecture.md`) will
//! be another [`LoginMethod`] that ends in the same place: a bearer token
//! passed to [`verify_token`] and then stored. Its refresh token needs a
//! second keyring entry and a refresh lock, which the store trait can carry
//! with an extra key per account.

use std::io::{self, BufRead, Write};
use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use mejl_jmap::transport::Transport;
use mejl_jmap::{Client, Secret, Session, capability};

use crate::secrets::{MAX_TOKEN_LEN, SecretError, SecretStore, TokenProblem, check_token};

/// Where Fastmail users create API tokens.
pub const TOKEN_SETTINGS_URL: &str = "https://app.fastmail.com/settings/security/tokens";

/// How a login gets its token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LoginMethod {
    /// A pasted API token.
    ApiToken,
}

/// A capability mejl looks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    /// `urn:ietf:params:jmap:mail`. Required.
    Mail,
    /// `urn:ietf:params:jmap:submission`: sending.
    Submission,
    /// Fastmail's Masked Email.
    MaskedEmail,
    /// `urn:ietf:params:jmap:contacts`.
    Contacts,
}

impl Capability {
    /// All of them, in report order.
    pub const ALL: [Capability; 4] = [
        Capability::Mail,
        Capability::Submission,
        Capability::MaskedEmail,
        Capability::Contacts,
    ];

    /// The capability URI.
    pub fn uri(self) -> &'static str {
        match self {
            Capability::Mail => capability::MAIL,
            Capability::Submission => capability::SUBMISSION,
            Capability::MaskedEmail => capability::MASKED_EMAIL,
            Capability::Contacts => capability::CONTACTS,
        }
    }

    /// The scope's name in Fastmail's token settings.
    pub fn scope_name(self) -> &'static str {
        match self {
            Capability::Mail => "Email",
            Capability::Submission => "Email submission",
            Capability::MaskedEmail => "Masked Email",
            Capability::Contacts => "Contacts",
        }
    }

    /// What mejl cannot do without it.
    pub fn without(self) -> &'static str {
        match self {
            Capability::Mail => "mejl cannot read mail",
            Capability::Submission => "you can read and organise mail, but not send",
            Capability::MaskedEmail => "Masked Email features are off",
            Capability::Contacts => "address completion uses only addresses from your mail",
        }
    }
}

/// What the session says about a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionReport {
    /// The primary mail account's id, if there is one.
    pub account_id: Option<String>,
    /// That account's name.
    pub account_name: String,
    /// The session username, usually the user's address. Print it, never
    /// store it.
    pub username: String,
    /// The account refuses changes.
    pub read_only: bool,
    /// Capabilities granted.
    pub present: Vec<Capability>,
    /// Capabilities missing.
    pub missing: Vec<Capability>,
}

impl SessionReport {
    /// Whether mejl can work with this token.
    pub fn usable(&self) -> bool {
        self.account_id.is_some() && self.present.contains(&Capability::Mail)
    }

    /// The report in plain lines, for the terminal.
    pub fn lines(&self) -> Vec<String> {
        let mut out = vec![format!(
            "Signed in as {} (account {}).",
            self.username,
            if self.account_name.is_empty() {
                "without a name"
            } else {
                &self.account_name
            }
        )];
        for cap in Capability::ALL {
            if self.present.contains(&cap) {
                out.push(format!("  {}: yes", cap.scope_name()));
            } else {
                out.push(format!(
                    "  {}: missing, so {}",
                    cap.scope_name(),
                    cap.without()
                ));
            }
        }
        if self.read_only {
            out.push("  This token is read-only: archive, flags and sending will fail.".to_owned());
        }
        if !self.missing.is_empty() {
            out.push(format!(
                "To add scopes, make a new token at {TOKEN_SETTINGS_URL}"
            ));
        }
        out
    }
}

/// Reads capabilities from a session. A capability counts only when the
/// server lists it and the primary mail account lists it too.
pub fn assess(session: &Session) -> SessionReport {
    let account_id = session
        .primary_account(capability::MAIL)
        .map(|id| id.to_string())
        .or_else(|| {
            // No primary mail account: fall back to the only account.
            (session.accounts.len() == 1)
                .then(|| session.accounts.keys().next().map(|id| id.to_string()))
                .flatten()
        });
    let account = account_id.as_deref().and_then(|id| session.account(id));
    let (present, missing): (Vec<Capability>, Vec<Capability>) =
        Capability::ALL.iter().partition(|cap| {
            session.has_capability(cap.uri())
                && account_id
                    .as_deref()
                    .is_some_and(|id| session.account_has_capability(id, cap.uri()))
        });
    SessionReport {
        account_name: account.map(|a| a.name.clone()).unwrap_or_default(),
        read_only: account.is_some_and(|a| a.is_read_only),
        username: session.username.clone(),
        account_id,
        present,
        missing,
    }
}

/// Why a login failed.
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    /// The input was cancelled.
    #[error("login cancelled")]
    Cancelled,
    /// The pasted text is not a token.
    #[error("{0}")]
    BadToken(#[from] TokenProblem),
    /// The server refused the token.
    #[error(
        "the server did not accept this token (HTTP 401 or 403). Check that you copied all of it, that it is a JMAP token and not an MCP token, and that it was not revoked"
    )]
    Unauthorized,
    /// The server could not be reached.
    #[error("cannot reach {url}: {reason}")]
    Network {
        /// The session URL.
        url: String,
        /// What went wrong.
        reason: String,
    },
    /// The server answered with something unusable.
    #[error("the server at {url} gave an unusable answer: {reason}")]
    Server {
        /// The session URL.
        url: String,
        /// What went wrong.
        reason: String,
    },
    /// The token works but grants no mail access.
    #[error(
        "this token has no access to mail. Make a new token with the Email scope at {TOKEN_SETTINGS_URL}"
    )]
    NoMail(Box<SessionReport>),
    /// The token could not be saved.
    #[error("cannot save the token: {0}")]
    Store(#[from] SecretError),
    /// Reading the terminal failed.
    #[error("cannot read the token: {0}")]
    Io(#[from] io::Error),
}

/// Fetches the session with `token` and reports what it grants. Fails with
/// [`LoginError::NoMail`] when mejl could not work with it.
pub async fn verify_token(
    transport: Arc<dyn Transport>,
    session_url: &str,
    token: Secret,
) -> Result<SessionReport, LoginError> {
    let client = Client::new(transport, session_url, token);
    let session = client.fetch_session().await.map_err(|e| {
        if e.is_unauthorized() {
            LoginError::Unauthorized
        } else if matches!(e, mejl_jmap::Error::Transport(_)) {
            LoginError::Network {
                url: session_url.to_owned(),
                reason: e.to_string(),
            }
        } else {
            LoginError::Server {
                url: session_url.to_owned(),
                reason: e.to_string(),
            }
        }
    })?;
    let report = assess(&session);
    if report.usable() {
        Ok(report)
    } else {
        Err(LoginError::NoMail(Box::new(report)))
    }
}

/// Checks a token and stores it. Builds a small runtime for the check and
/// stores the token after it, outside any runtime, because the keyring
/// may block on D-Bus. Call from a plain thread.
pub fn login_with_token(
    transport: Arc<dyn Transport>,
    session_url: &str,
    account: &str,
    token: Secret,
    store: &dyn SecretStore,
) -> Result<SessionReport, LoginError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let report = runtime.block_on(verify_token(transport, session_url, token.clone()))?;
    drop(runtime);
    store.set(account, &token)?;
    crate::logging::register_secret(&token);
    Ok(report)
}

/// What one input event did to a [`TokenInput`].
#[derive(Debug)]
pub enum InputStep {
    /// Keep reading.
    Continue,
    /// Enter was pressed; here is what was typed.
    Done(Secret),
    /// Esc, Ctrl+C or Ctrl+D.
    Cancelled,
}

/// Collects a token from raw-mode key and paste events without echo.
#[derive(Default)]
pub struct TokenInput {
    buf: String,
}

// The buffer is a token being typed.
impl std::fmt::Debug for TokenInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenInput")
            .field("len", &self.buf.chars().count())
            .finish()
    }
}

impl TokenInput {
    /// An empty input.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one event.
    pub fn feed(&mut self, event: &Event) -> InputStep {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Esc => InputStep::Cancelled,
                    KeyCode::Char('c' | 'd') if ctrl => InputStep::Cancelled,
                    KeyCode::Char('u') if ctrl => {
                        self.buf.clear();
                        InputStep::Continue
                    }
                    KeyCode::Enter => self.finish(),
                    KeyCode::Char('m' | 'j') if ctrl => self.finish(),
                    KeyCode::Backspace => {
                        self.buf.pop();
                        InputStep::Continue
                    }
                    KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                        self.push(&c.to_string());
                        InputStep::Continue
                    }
                    _ => InputStep::Continue,
                }
            }
            Event::Paste(text) => match text.find(['\r', '\n']) {
                // A paste that ends in a newline means "and Enter".
                Some(end) => {
                    self.push(&text[..end]);
                    self.finish()
                }
                None => {
                    self.push(text);
                    InputStep::Continue
                }
            },
            _ => InputStep::Continue,
        }
    }

    fn push(&mut self, text: &str) {
        // Far above any real token, so a runaway paste cannot grow it
        // without bound; check_token reports the length.
        let room = (MAX_TOKEN_LEN + 1).saturating_sub(self.buf.len());
        let mut end = text.len().min(room);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.buf.push_str(&text[..end]);
    }

    fn finish(&mut self) -> InputStep {
        InputStep::Done(Secret::new(std::mem::take(&mut self.buf)))
    }
}

/// Feeds events until the input is done or cancelled. `None` means
/// cancelled or the events ran out.
pub fn read_token_events(
    events: impl IntoIterator<Item = io::Result<Event>>,
) -> io::Result<Option<Secret>> {
    let mut input = TokenInput::new();
    for event in events {
        match input.feed(&event?) {
            InputStep::Continue => {}
            InputStep::Done(token) => return Ok(Some(token)),
            InputStep::Cancelled => return Ok(None),
        }
    }
    Ok(None)
}

/// Reads one line, for a token piped in by a script.
pub fn read_token_line(reader: &mut dyn BufRead) -> io::Result<Option<Secret>> {
    let mut line = String::new();
    // Take at most a little more than a token, so piping a large file
    // cannot exhaust memory.
    let limit = (MAX_TOKEN_LEN as u64) * 2;
    let n = io::Read::take(reader, limit).read_line(&mut line)?;
    if n == 0 {
        return Ok(None);
    }
    Ok(Some(Secret::new(line.trim_end_matches(['\r', '\n']))))
}

/// Prompts on stderr and reads a token from the terminal in raw mode, so
/// nothing is echoed. Pasting works, with or without bracketed paste.
pub fn prompt_token(prompt: &str) -> io::Result<Option<Secret>> {
    let mut err = io::stderr();
    write!(err, "{prompt}")?;
    err.flush()?;
    crossterm::terminal::enable_raw_mode()?;
    let _ = crossterm::execute!(err, crossterm::event::EnableBracketedPaste);
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = crossterm::execute!(io::stderr(), crossterm::event::DisableBracketedPaste);
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
    let restore = Restore;
    let result = read_token_events(std::iter::from_fn(|| Some(crossterm::event::read())));
    drop(restore);
    writeln!(err)?;
    result
}

/// Turns a raw token into a checked one.
pub fn checked(token: Secret) -> Result<Secret, LoginError> {
    Ok(check_token(token.expose())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemoryStore;
    use crossterm::event::KeyEvent;
    use mejl_fake::FakeServer;
    use mejl_jmap::HttpTransport;

    fn press(code: KeyCode) -> io::Result<Event> {
        Ok(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn ctrl(c: char) -> io::Result<Event> {
        Ok(Event::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::CONTROL,
        )))
    }

    fn typed(s: &str) -> Vec<io::Result<Event>> {
        s.chars().map(|c| press(KeyCode::Char(c))).collect()
    }

    #[test]
    fn typed_token_with_edits() {
        let mut events = typed("abcx");
        events.push(press(KeyCode::Backspace));
        events.extend(typed("d"));
        events.push(press(KeyCode::Enter));
        let token = read_token_events(events).unwrap().unwrap();
        assert_eq!(token.expose(), "abcd");
    }

    #[test]
    fn ctrl_u_clears_and_ctrl_c_cancels() {
        let mut events = typed("wrong");
        events.push(ctrl('u'));
        events.extend(typed("right"));
        events.push(press(KeyCode::Enter));
        assert_eq!(
            read_token_events(events).unwrap().unwrap().expose(),
            "right"
        );
        let mut events = typed("abc");
        events.push(ctrl('c'));
        assert!(read_token_events(events).unwrap().is_none());
        assert!(
            read_token_events(vec![press(KeyCode::Esc)])
                .unwrap()
                .is_none()
        );
        assert!(read_token_events(typed("no enter")).unwrap().is_none());
    }

    #[test]
    fn paste_with_newline_finishes() {
        let events = vec![Ok(Event::Paste("fmu1-pasted\nrest".into()))];
        assert_eq!(
            read_token_events(events).unwrap().unwrap().expose(),
            "fmu1-pasted"
        );
        let events = vec![Ok(Event::Paste("part".into())), press(KeyCode::Enter)];
        assert_eq!(read_token_events(events).unwrap().unwrap().expose(), "part");
    }

    #[test]
    fn releases_and_alt_keys_are_ignored() {
        let release = Ok(Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('z'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        )));
        let alt = Ok(Event::Key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::ALT,
        )));
        let mut events = typed("a");
        events.push(release);
        events.push(alt);
        events.push(Ok(Event::FocusLost));
        events.push(press(KeyCode::Enter));
        assert_eq!(read_token_events(events).unwrap().unwrap().expose(), "a");
    }

    #[test]
    fn runaway_paste_is_capped() {
        let huge = "x".repeat(MAX_TOKEN_LEN * 3);
        let events = vec![Ok(Event::Paste(huge)), press(KeyCode::Enter)];
        let token = read_token_events(events).unwrap().unwrap();
        assert_eq!(token.expose().len(), MAX_TOKEN_LEN + 1);
        assert!(matches!(
            checked(token),
            Err(LoginError::BadToken(TokenProblem::TooLong))
        ));
    }

    #[test]
    fn input_debug_hides_the_text() {
        let mut input = TokenInput::new();
        input.feed(&Event::Paste("secret".into()));
        let shown = format!("{input:?}");
        assert!(!shown.contains("secret"));
        assert!(shown.contains('6'));
    }

    #[test]
    fn piped_line() {
        let mut r = io::Cursor::new(b"fmu1-line\r\nnext\n".to_vec());
        assert_eq!(
            read_token_line(&mut r).unwrap().unwrap().expose(),
            "fmu1-line"
        );
        let mut empty = io::Cursor::new(Vec::new());
        assert!(read_token_line(&mut empty).unwrap().is_none());
    }

    fn session(json: serde_json::Value) -> Session {
        serde_json::from_value(json).unwrap()
    }

    fn full_caps() -> serde_json::Value {
        serde_json::json!({
            "urn:ietf:params:jmap:core": {},
            "urn:ietf:params:jmap:mail": {},
            "urn:ietf:params:jmap:submission": {},
            "urn:ietf:params:jmap:contacts": {},
            "https://www.fastmail.com/dev/maskedemail": {}
        })
    }

    #[test]
    fn all_capabilities_present() {
        let s = session(serde_json::json!({
            "capabilities": full_caps(),
            "accounts": {"A1": {"name": "ada@example.com", "isPersonal": true, "isReadOnly": false,
                "accountCapabilities": full_caps()}},
            "primaryAccounts": {"urn:ietf:params:jmap:mail": "A1"},
            "username": "ada@example.com"
        }));
        let r = assess(&s);
        assert!(r.usable());
        assert_eq!(r.present, Capability::ALL);
        assert!(r.missing.is_empty());
        let text = r.lines().join("\n");
        assert!(text.contains("Masked Email: yes"), "{text}");
        assert!(!text.contains("missing"));
    }

    #[test]
    fn missing_scopes_are_explained() {
        let caps = serde_json::json!({
            "urn:ietf:params:jmap:core": {},
            "urn:ietf:params:jmap:mail": {}
        });
        let s = session(serde_json::json!({
            "capabilities": caps,
            "accounts": {"A1": {"name": "ada@example.com", "isReadOnly": true, "accountCapabilities": caps}},
            "primaryAccounts": {"urn:ietf:params:jmap:mail": "A1"},
            "username": "ada@example.com"
        }));
        let r = assess(&s);
        assert!(r.usable());
        assert!(r.read_only);
        assert_eq!(
            r.missing,
            [
                Capability::Submission,
                Capability::MaskedEmail,
                Capability::Contacts
            ]
        );
        let text = r.lines().join("\n");
        assert!(text.contains(
            "Email submission: missing, so you can read and organise mail, but not send"
        ));
        assert!(text.contains("read-only"));
        assert!(text.contains(TOKEN_SETTINGS_URL));
    }

    #[test]
    fn capability_needs_the_account_too() {
        // The server knows Masked Email, but this account does not have it.
        let s = session(serde_json::json!({
            "capabilities": full_caps(),
            "accounts": {"A1": {"name": "x", "accountCapabilities": {
                "urn:ietf:params:jmap:mail": {}, "urn:ietf:params:jmap:submission": {}
            }}},
            "primaryAccounts": {"urn:ietf:params:jmap:mail": "A1"},
            "username": "x"
        }));
        let r = assess(&s);
        assert!(r.missing.contains(&Capability::MaskedEmail));
        assert!(r.missing.contains(&Capability::Contacts));
        assert!(r.present.contains(&Capability::Submission));
    }

    #[test]
    fn no_mail_is_unusable() {
        let caps = serde_json::json!({"urn:ietf:params:jmap:core": {}, "urn:ietf:params:jmap:contacts": {}});
        let s = session(serde_json::json!({
            "capabilities": caps,
            "accounts": {"A1": {"name": "x", "accountCapabilities": caps}},
            "primaryAccounts": {"urn:ietf:params:jmap:contacts": "A1"},
            "username": "x"
        }));
        let r = assess(&s);
        assert_eq!(
            r.account_id.as_deref(),
            Some("A1"),
            "single account fallback"
        );
        assert!(!r.usable());
        let empty = assess(&Session::default());
        assert!(!empty.usable());
        assert_eq!(empty.missing.len(), 4);
    }

    /// The fake on loopback HTTP, with a runtime that keeps it serving.
    fn serve() -> (tokio::runtime::Runtime, mejl_fake::HttpHandle, FakeServer) {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let server = FakeServer::demo("2026-10-23T14:27:00Z".parse().unwrap());
        let http = rt
            .block_on(server.serve_http("127.0.0.1:0".parse().unwrap()))
            .unwrap();
        (rt, http, server)
    }

    #[test]
    fn good_token_over_loopback_is_stored() {
        let (_rt, http, server) = serve();
        let store = MemoryStore::new();
        let transport = Arc::new(HttpTransport::new().unwrap());
        let report = login_with_token(
            transport,
            &http.session_url(),
            "default",
            Secret::new(server.token()),
            &store,
        )
        .unwrap();
        assert_eq!(report.username, "mira@example.org");
        assert_eq!(report.present, Capability::ALL);
        assert_eq!(
            store.get("default").unwrap().unwrap().expose(),
            server.token()
        );
    }

    #[test]
    fn missing_scopes_over_loopback_are_reported_but_still_usable() {
        let (_rt, http, server) = serve();
        server.quirks_mut().session_without = vec![
            Capability::MaskedEmail.uri().to_owned(),
            Capability::Contacts.uri().to_owned(),
        ];
        let store = MemoryStore::new();
        let transport = Arc::new(HttpTransport::new().unwrap());
        let report = login_with_token(
            transport,
            &http.session_url(),
            "default",
            Secret::new(server.token()),
            &store,
        )
        .unwrap();
        assert!(report.usable());
        assert_eq!(report.present, [Capability::Mail, Capability::Submission]);
        assert_eq!(
            report.missing,
            [Capability::MaskedEmail, Capability::Contacts]
        );
        let text = report.lines().join("\n");
        assert!(text.contains("Masked Email features are off"), "{text}");
        assert!(store.get("default").unwrap().is_some());
    }

    #[test]
    fn a_session_without_mail_is_refused_and_nothing_is_stored() {
        let (_rt, http, server) = serve();
        server.quirks_mut().session_without = vec![Capability::Mail.uri().to_owned()];
        let store = MemoryStore::new();
        let transport = Arc::new(HttpTransport::new().unwrap());
        let err = login_with_token(
            transport,
            &http.session_url(),
            "default",
            Secret::new(server.token()),
            &store,
        )
        .unwrap_err();
        assert!(matches!(err, LoginError::NoMail(_)), "{err:?}");
        assert!(store.get("default").unwrap().is_none());
    }

    #[test]
    fn bad_token_is_a_401_and_nothing_is_stored() {
        let (_rt, http, _server) = serve();
        let store = MemoryStore::new();
        let transport = Arc::new(HttpTransport::new().unwrap());
        let err = login_with_token(
            transport,
            &http.session_url(),
            "default",
            Secret::new("fmu1-wrong-token-000000"),
            &store,
        )
        .unwrap_err();
        assert!(matches!(err, LoginError::Unauthorized), "{err:?}");
        assert!(err.to_string().contains("401"));
        assert!(store.get("default").unwrap().is_none());
    }

    #[test]
    fn closed_port_is_a_network_error() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let url = format!("http://127.0.0.1:{port}/jmap/session");
        let store = MemoryStore::new();
        let transport = Arc::new(HttpTransport::new().unwrap());
        let err = login_with_token(
            transport,
            &url,
            "default",
            Secret::new("tok-12345678"),
            &store,
        )
        .unwrap_err();
        assert!(matches!(err, LoginError::Network { .. }), "{err:?}");
        assert!(err.to_string().contains(&url));
    }

    #[test]
    fn wrong_path_is_a_server_error() {
        let (_rt, http, server) = serve();
        let url = format!("{}/not-jmap", http.base_url());
        let store = MemoryStore::new();
        let transport = Arc::new(HttpTransport::new().unwrap());
        let err = login_with_token(
            transport,
            &url,
            "default",
            Secret::new(server.token()),
            &store,
        )
        .unwrap_err();
        assert!(matches!(err, LoginError::Server { .. }), "{err:?}");
    }

    #[test]
    fn store_failure_is_reported_after_a_good_check() {
        let (_rt, http, server) = serve();
        let store = MemoryStore::new();
        store.fail_with(Some("locked"));
        let transport = Arc::new(HttpTransport::new().unwrap());
        let err = login_with_token(
            transport,
            &http.session_url(),
            "default",
            Secret::new(server.token()),
            &store,
        )
        .unwrap_err();
        assert!(matches!(err, LoginError::Store(_)), "{err:?}");
    }
}
