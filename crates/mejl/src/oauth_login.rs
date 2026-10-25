//! `mejl login --oauth`: log in with the browser.
//!
//! 1. Find the login server from the session URL ([`oauth::discover`]).
//! 2. Pick the client id: `oauth_client_id` from the config, else the one
//!    stored with this account's last OAuth login for the same server, else
//!    register mejl through the server's open registration.
//! 3. Listen on `127.0.0.1`, on `--port` or a random port ([`Loopback`]).
//! 4. Open the browser at the authorization URL, and print it too.
//! 5. Wait for the browser to come back with the right `state` and `iss`.
//! 6. Exchange the code, check the session with the new access token, and
//!    store the login.
//!
//! When the server refuses the Masked Email scope, mejl explains and asks
//! again without it. The granted scope and the session decide what the
//! login can do, whatever the server answered.
//!
//! This flow never refreshes a token. Only the TUI does.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use mejl_jmap::oauth::{self, Pkce, ServerMetadata, random_state, scope};
use mejl_jmap::transport::Transport;
use mejl_jmap::{Callback, OAuthErrorKind, Secret, TokenSet};

use crate::login::{LoginError, SessionReport, verify_token};
use crate::oauth::{ClientSource, OAuthRecord};
use crate::secrets::{SecretError, SecretStore};

/// How long mejl waits for the browser by default.
pub const WAIT_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// How long one browser connection may take to send its request.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest request head read from the browser.
const MAX_REQUEST: usize = 16 * 1024;
/// How long one browser connection may take in all, however slowly it
/// trickles in.
const REQUEST_DEADLINE: Duration = Duration::from_secs(10);
/// Browser connections served at once. More are closed at once, so a
/// flood of connections cannot use up threads.
const MAX_CONNECTIONS: usize = 32;
/// The port the SSH hint suggests.
const SUGGESTED_PORT: u16 = 8765;

/// What a browser login needs.
pub struct OAuthLogin<'a> {
    /// The HTTP transport.
    pub transport: Arc<dyn Transport>,
    /// The account's session URL.
    pub session_url: &'a str,
    /// The account name, for messages and the store.
    pub account: &'a str,
    /// `oauth_client_id` from the config.
    pub client_id: Option<&'a str>,
    /// `--port`, for SSH port forwarding. `None` picks a free port.
    pub port: Option<u16>,
    /// How long to wait for the browser.
    pub timeout: Duration,
    /// Opens a URL in the browser.
    pub open: &'a dyn Fn(&str) -> Result<(), String>,
}

/// A finished browser login.
#[derive(Debug)]
pub struct OAuthOutcome {
    /// What the session says about the new login.
    pub report: SessionReport,
    /// What was stored.
    pub record: OAuthRecord,
    /// A stored API token for the account was removed.
    pub replaced_token: bool,
}

/// Logs in with the browser and stores the login in `store`, replacing
/// any stored API token. Progress and the browser URL go to `err`.
///
/// Builds a small runtime for the network steps. The store is read before
/// it and written after it is gone, because the keyring may block on
/// D-Bus. Call from a plain thread.
pub fn login_with_oauth(
    opts: &OAuthLogin<'_>,
    store: &dyn SecretStore,
    err: &mut dyn Write,
) -> Result<OAuthOutcome, LoginError> {
    let stored = match store.get_oauth(opts.account) {
        Ok(record) => record,
        Err(SecretError::Corrupt(why)) => {
            writeln!(err, "Ignoring the stored OAuth login: {why}.")?;
            None
        }
        Err(e) => return Err(e.into()),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let transport = &*opts.transport;
    let meta = runtime
        .block_on(oauth::discover(transport, opts.session_url))
        .map_err(|e| failure(opts.session_url, "cannot find the login server", e))?;
    let previous = stored.clone();
    let (client_id, client_source) = runtime.block_on(pick_client(opts, &meta, stored, err))?;
    let mut scopes = wanted_scopes(&meta, client_source);

    let loopback = Loopback::bind(opts.port)?;
    let mut first = true;
    let (code, pkce, redirect_uri) = loop {
        let pkce = Pkce::generate().map_err(|e| LoginError::OAuth(e.to_string()))?;
        let state = random_state().map_err(|e| LoginError::OAuth(e.to_string()))?;
        let redirect_uri = loopback.redirect_uri();
        let url = oauth::authorize_url(
            &meta.authorization_endpoint,
            &client_id,
            &redirect_uri,
            &scopes,
            &state,
            &pkce,
        )
        .map_err(|e| LoginError::OAuth(e.to_string()))?;
        if first {
            write_instructions(err, opts, &loopback, &url)?;
        } else {
            writeln!(
                err,
                "Opening the browser again. If it does not open, go to:\n\n  {url}\n"
            )?;
        }
        if let Err(why) = (opts.open)(&url) {
            writeln!(
                err,
                "Could not open a browser ({why}); open the address above yourself."
            )?;
        }
        first = false;
        let fallback = without_masked(&scopes);
        let callback = loopback.wait(
            &Expect {
                state,
                issuer: meta.issuer.clone(),
                iss_required: meta.authorization_response_iss_parameter_supported,
                scope_retry: fallback.is_some(),
            },
            opts.timeout,
        )?;
        match (callback.error, fallback) {
            (None, _) => {
                let code = callback.code.ok_or_else(|| {
                    LoginError::OAuth("the browser came back without a code".into())
                })?;
                break (code, pkce, redirect_uri);
            }
            (Some(e), Some(fewer)) if e.error == OAuthErrorKind::InvalidScope => {
                writeln!(
                    err,
                    "Fastmail did not grant the Masked Email scope{}. Asking again without it; masks stay off for this login.",
                    e.description
                        .as_deref()
                        .map(|d| format!(" ({})", clean(d)))
                        .unwrap_or_default()
                )?;
                scopes = fewer;
            }
            (Some(e), _) => return Err(LoginError::Refused(cleaned(e))),
        }
    };

    let tokens = runtime
        .block_on(oauth::exchange_code(
            transport,
            &meta.token_endpoint,
            &client_id,
            &code,
            &redirect_uri,
            &pkce,
        ))
        .map_err(|e| failure(&meta.token_endpoint, "cannot finish the login", e))?;
    let checked = runtime.block_on(verify_token(
        opts.transport.clone(),
        opts.session_url,
        tokens.access.clone(),
    ));
    let report = match checked {
        Ok(report) => report,
        Err(e) => {
            // The new grant is no use; end it rather than leave it behind.
            if let Some(endpoint) = &meta.revocation_endpoint {
                let _ = runtime.block_on(oauth::revoke(
                    transport,
                    endpoint,
                    &client_id,
                    &tokens.refresh,
                ));
            }
            return Err(match e {
                LoginError::NoMail(report) => LoginError::OAuthNoMail(report),
                LoginError::Unauthorized => {
                    LoginError::OAuth("the server did not accept the new login".into())
                }
                other => other,
            });
        }
    };

    let record = OAuthRecord {
        client_id,
        client_source,
        issuer: meta.issuer,
        token_endpoint: meta.token_endpoint,
        revocation_endpoint: meta.revocation_endpoint,
        tokens,
    };
    record.register_secrets();
    store.set_oauth(opts.account, &record)?;
    // The login this one replaces would otherwise stay live at Fastmail.
    // Best effort: the new login is already saved.
    if let Some(old) = previous
        && old.tokens.refresh != record.tokens.refresh
        && let Some(endpoint) = &old.revocation_endpoint
        && let Err(e) = runtime.block_on(oauth::revoke(
            transport,
            endpoint,
            &old.client_id,
            &old.tokens.refresh,
        ))
    {
        writeln!(err, "Warning: could not end the previous login: {e}")?;
    }
    drop(runtime);
    // One login per account. The OAuth login outranks a stored token
    // anyway, so a failure here only leaves a token behind.
    let replaced_token = match store.delete(opts.account) {
        Ok(had) => had,
        Err(e) => {
            writeln!(err, "Warning: could not remove the old API token: {e}")?;
            false
        }
    };
    Ok(OAuthOutcome {
        report,
        record,
        replaced_token,
    })
}

fn write_instructions(
    err: &mut dyn Write,
    opts: &OAuthLogin<'_>,
    loopback: &Loopback,
    url: &str,
) -> io::Result<()> {
    writeln!(
        err,
        "Log in to account {:?} in your browser. If it does not open, go to:\n\n  {url}\n",
        opts.account
    )?;
    match opts.port {
        Some(port) => writeln!(
            err,
            "Over SSH, forward the port first: ssh -L {port}:127.0.0.1:{port} <host>"
        )?,
        None => writeln!(
            err,
            "Over SSH, the browser cannot reach port {}. Run `mejl login --port {SUGGESTED_PORT}` instead, after `ssh -L {SUGGESTED_PORT}:127.0.0.1:{SUGGESTED_PORT} <host>`.",
            loopback.port()
        )?,
    }
    writeln!(
        err,
        "Waiting for the browser (up to {}). Press Ctrl-C to cancel.",
        minutes(opts.timeout)
    )
}

fn minutes(d: Duration) -> String {
    match d.as_secs() {
        s if s >= 120 => format!("{} minutes", s / 60),
        60..=119 => "1 minute".to_owned(),
        s => format!("{s} seconds"),
    }
}

/// A network failure, or an OAuth step that went wrong.
fn failure(url: &str, what: &str, e: mejl_jmap::Error) -> LoginError {
    match e {
        mejl_jmap::Error::Transport(_) => LoginError::Network {
            url: url.to_owned(),
            reason: e.to_string(),
        },
        e => LoginError::OAuth(clean(&format!("{what}: {e}"))),
    }
}

/// The client id to use and where it came from.
async fn pick_client(
    opts: &OAuthLogin<'_>,
    meta: &ServerMetadata,
    stored: Option<OAuthRecord>,
    err: &mut dyn Write,
) -> Result<(String, ClientSource), LoginError> {
    if let Some(id) = opts.client_id {
        return Ok((id.to_owned(), ClientSource::Config));
    }
    let transport = &*opts.transport;
    if let Some(record) = stored.filter(|r| same_issuer(&r.issuer, &meta.issuer)) {
        match client_known(transport, &meta.token_endpoint, &record.client_id).await {
            Ok(true) => return Ok((record.client_id, record.client_source)),
            Ok(false) => writeln!(
                err,
                "The server no longer knows mejl's client registration; registering mejl again."
            )?,
            Err(e) => return Err(failure(&meta.token_endpoint, "cannot check the client", e)),
        }
    }
    let Some(endpoint) = &meta.registration_endpoint else {
        return Err(LoginError::Registration(
            "the server does not offer client registration".into(),
        ));
    };
    let id = oauth::register(transport, endpoint)
        .await
        .map_err(|e| match e {
            mejl_jmap::Error::Transport(_) => failure(endpoint, "", e),
            e => LoginError::Registration(clean(&e.to_string())),
        })?;
    Ok((id, ClientSource::Registered))
}

/// Whether the token endpoint still knows `client_id`.
///
/// This is a heuristic. An authorization server shows an unknown client an
/// error page and never redirects back, so a stale client id would leave
/// the user stuck in the browser. Instead mejl first sends a refresh with a
/// made-up refresh token: an unknown client gets `invalid_client`, and a
/// known one gets some other error, usually `invalid_grant`. Only
/// `invalid_client` counts as stale. The made-up token matches no grant, so
/// nothing is used up.
async fn client_known(
    transport: &dyn Transport,
    token_endpoint: &str,
    client_id: &str,
) -> Result<bool, mejl_jmap::Error> {
    let junk = TokenSet {
        access: Secret::new(""),
        refresh: Secret::new(format!("mejl-client-check-{}", random_state()?)),
        expires_at: Timestamp::UNIX_EPOCH,
        scope: Vec::new(),
    };
    match oauth::refresh(transport, token_endpoint, client_id, &junk).await {
        Err(mejl_jmap::Error::OAuth(e)) if e.error == OAuthErrorKind::InvalidClient => Ok(false),
        Err(e @ mejl_jmap::Error::Transport(_)) => Err(e),
        _ => Ok(true),
    }
}

/// The scopes to ask for.
///
/// - A client mejl registered, with the interoperable scopes advertised:
///   mail, contacts and `offline_access`, plus Masked Email together with
///   JMAP core.
/// - A client id from the config, or a server that does not advertise the
///   interoperable scopes: Fastmail's documented JMAP scopes.
fn wanted_scopes(meta: &ServerMetadata, source: ClientSource) -> Vec<&'static str> {
    let advertised = |s: &str| meta.scopes_supported.iter().any(|x| x == s);
    if source == ClientSource::Registered && advertised(scope::MAIL) {
        return vec![
            scope::MAIL,
            scope::CONTACTS,
            scope::OFFLINE_ACCESS,
            scope::JMAP_CORE,
            scope::MASKED_EMAIL,
        ];
    }
    let mut scopes = vec![
        scope::JMAP_CORE,
        scope::JMAP_MAIL,
        scope::JMAP_SUBMISSION,
        scope::JMAP_CONTACTS,
        scope::MASKED_EMAIL,
    ];
    if source == ClientSource::Registered && advertised(scope::OFFLINE_ACCESS) {
        scopes.push(scope::OFFLINE_ACCESS);
    }
    scopes
}

/// The scopes for a second round after `invalid_scope`: without Masked
/// Email, and without JMAP core when it was only there for Masked Email.
/// `None` when Masked Email was not asked for.
fn without_masked(scopes: &[&'static str]) -> Option<Vec<&'static str>> {
    if !scopes.contains(&scope::MASKED_EMAIL) {
        return None;
    }
    let interoperable = scopes.contains(&scope::MAIL);
    Some(
        scopes
            .iter()
            .copied()
            .filter(|s| *s != scope::MASKED_EMAIL && !(interoperable && *s == scope::JMAP_CORE))
            .collect(),
    )
}

fn same_issuer(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// Text from the server made safe for the terminal.
fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(200).collect()
}

/// An error from the redirect's query, made safe for the terminal.
fn cleaned(e: mejl_jmap::OAuthError) -> mejl_jmap::OAuthError {
    mejl_jmap::OAuthError {
        error: match e.error {
            OAuthErrorKind::Other(code) => OAuthErrorKind::Other(clean(&code)),
            known => known,
        },
        description: e.description.as_deref().map(clean),
        temporary: e.temporary,
    }
}

/// What the listener waits for.
#[derive(Debug, Clone)]
pub struct Expect {
    /// The `state` sent to the authorization endpoint.
    pub state: String,
    /// The issuer the `iss` parameter must name.
    pub issuer: String,
    /// The server says it sends `iss`, so a callback without one is
    /// refused.
    pub iss_required: bool,
    /// An `invalid_scope` answer leads to a second round, which the page
    /// says.
    pub scope_retry: bool,
}

/// The loopback listener the browser comes back to.
///
/// Browsers open extra connections, ask for `/favicon.ico` and sometimes
/// connect without sending anything. So the listener keeps accepting, reads
/// each connection on its own thread with a short timeout, answers and
/// ignores anything but the callback, and stops only on a callback with
/// the right `state`.
#[derive(Debug)]
pub struct Loopback {
    listener: TcpListener,
    port: u16,
}

impl Loopback {
    /// Listens on `127.0.0.1` at `port`, or a free port.
    pub fn bind(port: Option<u16>) -> Result<Loopback, LoginError> {
        let wanted = port.unwrap_or(0);
        let listen_error = |e: io::Error| LoginError::Listen {
            port: wanted,
            reason: e.to_string(),
        };
        let listener = TcpListener::bind(("127.0.0.1", wanted)).map_err(listen_error)?;
        listener.set_nonblocking(true).map_err(listen_error)?;
        let port = listener.local_addr().map_err(listen_error)?.port();
        Ok(Loopback { listener, port })
    }

    /// The port it listens on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The redirect URI with this port.
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }

    /// Waits for the callback that carries `expect.state`. A wrong or
    /// missing `iss` on it ends the wait with an error.
    pub fn wait(&self, expect: &Expect, timeout: Duration) -> Result<Callback, LoginError> {
        let deadline = Instant::now() + timeout;
        let (tx, rx) = mpsc::channel();
        let expect = Arc::new(expect.clone());
        let open = Arc::new(AtomicUsize::new(0));
        loop {
            if let Ok(result) = rx.try_recv() {
                return result;
            }
            if Instant::now() >= deadline {
                return Err(LoginError::TimedOut(minutes(timeout)));
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                        open.fetch_sub(1, Ordering::SeqCst);
                        drop(stream);
                        continue;
                    }
                    let tx = tx.clone();
                    let expect = expect.clone();
                    let count = open.clone();
                    let spawned =
                        std::thread::Builder::new()
                            .name("mejl-login".into())
                            .spawn(move || {
                                if let Some(result) = serve(stream, &expect) {
                                    let _ = tx.send(result);
                                }
                                count.fetch_sub(1, Ordering::SeqCst);
                            });
                    if let Err(e) = spawned {
                        open.fetch_sub(1, Ordering::SeqCst);
                        tracing::warn!(error = %e, "cannot start a thread for a browser connection");
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if let Ok(result) = rx.recv_timeout(Duration::from_millis(50)) {
                        return result;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    return Err(LoginError::Listen {
                        port: self.port,
                        reason: e.to_string(),
                    });
                }
            }
        }
    }
}

/// Reads one request and answers it. `Some` only for a callback with the
/// right state. The request line carries the code, so it is never logged.
fn serve(mut stream: TcpStream, expect: &Expect) -> Option<Result<Callback, LoginError>> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(READ_TIMEOUT));
    let head = read_head(&mut stream)?;
    let line = head.lines().next()?;
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next()?, parts.next()?);
    if method != "GET" {
        reply(
            &mut stream,
            "405 Method Not Allowed",
            &page("Not here", "mejl only answers the browser's login.", false),
        );
        return None;
    }
    let callback = oauth::parse_callback(target);
    if callback.path != "/" {
        reply(
            &mut stream,
            "404 Not Found",
            &page("Not here", "Nothing to see here.", false),
        );
        return None;
    }
    if callback.state.as_deref() != Some(expect.state.as_str()) {
        reply(
            &mut stream,
            "400 Bad Request",
            &page(
                "Not this login",
                "This page is not part of the mejl login that is waiting. You can close this tab.",
                false,
            ),
        );
        return None;
    }
    let iss_problem = match callback.iss.as_deref() {
        Some(iss) if !same_issuer(iss, &expect.issuer) => Some(LoginError::WrongIssuer {
            got: clean(iss),
            want: expect.issuer.clone(),
        }),
        None if expect.iss_required => Some(LoginError::MissingIssuer(expect.issuer.clone())),
        _ => None,
    };
    if let Some(problem) = iss_problem {
        let text = format!("{problem}. Go back to the terminal.");
        reply(
            &mut stream,
            "400 Bad Request",
            &page("mejl could not log in", &text, true),
        );
        return Some(Err(problem));
    }
    match &callback.error {
        Some(e) if e.error == OAuthErrorKind::InvalidScope && expect.scope_retry => {
            reply(
                &mut stream,
                "200 OK",
                &page(
                    "One more step",
                    "Fastmail did not grant every permission mejl asked for. mejl asks again with fewer in a new tab; you can close this one.",
                    true,
                ),
            );
        }
        Some(e) => {
            let text = format!(
                "The server answered {}{}. You can close this tab and go back to the terminal.",
                e.error,
                e.description
                    .as_deref()
                    .map(|d| format!(": {}", clean(d)))
                    .unwrap_or_default()
            );
            reply(
                &mut stream,
                "200 OK",
                &page("mejl could not log in", &text, true),
            );
        }
        None if callback.code.is_none() => {
            reply(
                &mut stream,
                "400 Bad Request",
                &page(
                    "mejl could not log in",
                    "The answer has no authorization code. Go back to the terminal.",
                    true,
                ),
            );
        }
        None => {
            reply(
                &mut stream,
                "200 OK",
                &page(
                    "mejl is logged in",
                    "You can close this tab and go back to the terminal.",
                    true,
                ),
            );
        }
    }
    Some(Ok(callback))
}

/// The request line and headers, or `None` for a connection that sent
/// nothing usable before the timeout.
fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    let deadline = Instant::now() + REQUEST_DEADLINE;
    while buf.len() < MAX_REQUEST && Instant::now() < deadline {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.windows(2).any(|w| w == b"\n\n")
                {
                    break;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    text.contains('\n').then_some(text)
}

fn reply(stream: &mut TcpStream, status: &str, body: &str) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// A small HTML page. `text` is escaped.
fn page(title: &str, text: &str, from_mejl: bool) -> String {
    let footer = if from_mejl {
        "<p style=\"color:#666\">mejl, the terminal email client</p>"
    } else {
        ""
    };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title></head><body style=\"font-family:sans-serif;max-width:34em;margin:4em auto;line-height:1.5\"><h1>{}</h1><p>{}</p>{footer}</body></html>",
        html_escape(title),
        html_escape(title),
        html_escape(text)
    )
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(scopes: &[&str]) -> ServerMetadata {
        ServerMetadata {
            issuer: "https://api.example.com".into(),
            authorization_endpoint: "https://api.example.com/oauth/authorize".into(),
            token_endpoint: "https://api.example.com/oauth/refresh".into(),
            registration_endpoint: None,
            revocation_endpoint: None,
            scopes_supported: scopes.iter().map(|s| (*s).to_owned()).collect(),
            authorization_response_iss_parameter_supported: true,
        }
    }

    #[test]
    fn scopes_follow_the_client_and_the_metadata() {
        let fastmail = meta(&[scope::MAIL, scope::CONTACTS, scope::OFFLINE_ACCESS]);
        let asked = wanted_scopes(&fastmail, ClientSource::Registered);
        assert_eq!(
            asked,
            [
                scope::MAIL,
                scope::CONTACTS,
                scope::OFFLINE_ACCESS,
                scope::JMAP_CORE,
                scope::MASKED_EMAIL
            ]
        );
        assert_eq!(
            without_masked(&asked).unwrap(),
            [scope::MAIL, scope::CONTACTS, scope::OFFLINE_ACCESS]
        );
        let configured = wanted_scopes(&fastmail, ClientSource::Config);
        assert_eq!(
            configured,
            [
                scope::JMAP_CORE,
                scope::JMAP_MAIL,
                scope::JMAP_SUBMISSION,
                scope::JMAP_CONTACTS,
                scope::MASKED_EMAIL
            ]
        );
        // JMAP core stays when it is not only there for Masked Email.
        assert_eq!(
            without_masked(&configured).unwrap(),
            [
                scope::JMAP_CORE,
                scope::JMAP_MAIL,
                scope::JMAP_SUBMISSION,
                scope::JMAP_CONTACTS
            ]
        );
        assert_eq!(without_masked(&[scope::MAIL]), None);
        let other = wanted_scopes(&meta(&[scope::OFFLINE_ACCESS]), ClientSource::Registered);
        assert_eq!(other.first(), Some(&scope::JMAP_CORE));
        assert_eq!(other.last(), Some(&scope::OFFLINE_ACCESS));
    }

    #[test]
    fn pages_escape_what_the_server_said() {
        let p = page("t", "<script>alert('x')</script> & \"q\"", true);
        assert!(!p.contains("<script>"), "{p}");
        assert!(p.contains("&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt; &amp; &quot;q&quot;"));
        assert_eq!(minutes(WAIT_TIMEOUT), "5 minutes");
        assert_eq!(minutes(Duration::from_secs(90)), "1 minute");
        assert_eq!(minutes(Duration::from_secs(2)), "2 seconds");
        assert_eq!(clean("a\u{1b}[31mb\n"), "a[31mb");
        let e = cleaned(mejl_jmap::OAuthError {
            error: OAuthErrorKind::Other("odd\u{1b}]52;c;x\u{7}".into()),
            description: Some("no\u{1b}[2J".into()),
            temporary: false,
        });
        let shown = LoginError::Refused(e).to_string();
        assert!(!shown.chars().any(char::is_control), "{shown:?}");
    }

    fn get(port: u16, target: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(s, "GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    fn expect() -> Expect {
        Expect {
            state: "st-1".into(),
            issuer: "https://api.example.com".into(),
            iss_required: true,
            scope_retry: false,
        }
    }

    #[test]
    fn the_listener_ignores_strangers_and_waits_for_its_state() {
        let lb = Loopback::bind(None).unwrap();
        let port = lb.port();
        assert_eq!(lb.redirect_uri(), format!("http://127.0.0.1:{port}/"));
        let browser = std::thread::spawn(move || {
            // A connection that never sends anything, held open.
            let idle = TcpStream::connect(("127.0.0.1", port)).unwrap();
            // One that closes at once.
            drop(TcpStream::connect(("127.0.0.1", port)).unwrap());
            let favicon = get(port, "/favicon.ico");
            let wrong = get(
                port,
                "/?code=c&state=other&iss=https%3A%2F%2Fapi.example.com",
            );
            let post = {
                let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
                write!(s, "POST /?state=st-1 HTTP/1.1\r\n\r\n").unwrap();
                let mut out = String::new();
                s.read_to_string(&mut out).unwrap();
                out
            };
            let right = get(
                port,
                "/?code=the-code&state=st-1&iss=https%3A%2F%2Fapi.example.com",
            );
            drop(idle);
            (favicon, wrong, post, right)
        });
        let cb = lb.wait(&expect(), Duration::from_secs(20)).unwrap();
        assert_eq!(cb.code.as_ref().map(Secret::expose), Some("the-code"));
        let (favicon, wrong, post, right) = browser.join().unwrap();
        assert!(favicon.starts_with("HTTP/1.1 404"), "{favicon}");
        assert!(wrong.starts_with("HTTP/1.1 400"), "{wrong}");
        assert!(wrong.contains("not part of the mejl login"), "{wrong}");
        assert!(post.starts_with("HTTP/1.1 405"), "{post}");
        assert!(right.starts_with("HTTP/1.1 200"), "{right}");
        assert!(right.contains("You can close this tab"), "{right}");
        assert!(right.contains("Content-Length:"));
        assert!(!right.contains("the-code"));
    }

    #[test]
    fn the_listener_refuses_a_wrong_or_missing_issuer() {
        for (target, want) in [
            (
                "/?code=c&state=st-1&iss=https%3A%2F%2Fevil.example.org",
                "evil.example.org",
            ),
            ("/?code=c&state=st-1", "did not say"),
        ] {
            let lb = Loopback::bind(None).unwrap();
            let port = lb.port();
            let page = std::thread::spawn(move || get(port, target));
            let e = lb.wait(&expect(), Duration::from_secs(20)).unwrap_err();
            assert!(
                matches!(
                    e,
                    LoginError::WrongIssuer { .. } | LoginError::MissingIssuer(_)
                ),
                "{e:?}"
            );
            assert!(e.to_string().contains(want), "{e}");
            let page = page.join().unwrap();
            assert!(page.contains("could not log in"), "{page}");
        }
        // Without iss support advertised, a missing iss is fine.
        let lb = Loopback::bind(None).unwrap();
        let port = lb.port();
        let page = std::thread::spawn(move || get(port, "/?code=c&state=st-1"));
        let lax = Expect {
            iss_required: false,
            ..expect()
        };
        assert!(lb.wait(&lax, Duration::from_secs(20)).is_ok());
        page.join().unwrap();
    }

    #[test]
    fn the_listener_names_errors_and_times_out() {
        let lb = Loopback::bind(None).unwrap();
        let port = lb.port();
        let page = std::thread::spawn(move || {
            get(
                port,
                "/?error=access_denied&error_description=%3Cb%3Eno%3C%2Fb%3E&state=st-1&iss=https%3A%2F%2Fapi.example.com",
            )
        });
        let cb = lb.wait(&expect(), Duration::from_secs(20)).unwrap();
        assert_eq!(cb.error.unwrap().error, OAuthErrorKind::AccessDenied);
        let page = page.join().unwrap();
        assert!(
            page.contains("access_denied: &lt;b&gt;no&lt;/b&gt;"),
            "{page}"
        );

        let e = lb.wait(&expect(), Duration::from_millis(200)).unwrap_err();
        assert!(matches!(e, LoginError::TimedOut(_)), "{e:?}");
        assert!(
            Loopback::bind(Some(port)).is_err(),
            "the port is still held"
        );
    }
}
