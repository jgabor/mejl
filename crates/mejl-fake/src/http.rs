//! The one HTTP entry point both the in-process transport and the loopback
//! server call. Routing, auth and the HTTP fault quirks live here, so the
//! two paths cannot drift apart.

use bytes::Bytes;
use mejl_jmap::transport::{HttpMethod, HttpRequest, HttpResponse, StreamingResponse};
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::FakeServer;
use crate::api::{self, CAPABILITIES, CONTACTS, CORE, MAIL, MASKED, SUBMISSION};
use crate::config::{EventSourceUrl, Quirks};
use crate::model::{PartKind, Upload};
use crate::oauth;
use crate::push;
use crate::store::Store;
use crate::text;

/// Base URL for in-process requests whose URL has no scheme and host. The
/// `.invalid` top-level domain never resolves, so a client that leaks one of
/// these URLs to a real HTTP stack fails at once instead of reaching out.
pub const IN_PROCESS_BASE: &str = "http://mejl-fake.invalid";

pub(crate) enum Reply {
    Full(HttpResponse),
    Stream(StreamingResponse),
    /// The request was handled but the connection drops before the client
    /// gets the response.
    Lost,
}

pub(crate) fn response(status: u16, content_type: &str, body: impl Into<Bytes>) -> Reply {
    Reply::Full(HttpResponse {
        status,
        headers: vec![("content-type".to_owned(), content_type.to_owned())],
        body: body.into(),
    })
}

pub(crate) fn json_reply(status: u16, v: &Value) -> Reply {
    response(status, "application/json", v.to_string())
}

fn problem(status: u16, title: &str, detail: &str) -> Reply {
    response(
        status,
        "application/problem+json",
        json!({"type": "about:blank", "status": status, "title": title, "detail": detail})
            .to_string(),
    )
}

pub(crate) fn with_header(mut reply: Reply, name: &str, value: &str) -> Reply {
    if let Reply::Full(r) = &mut reply {
        r.headers.push((name.to_owned(), value.to_owned()));
    }
    reply
}

/// Splits a URL into its `scheme://authority` base, path and query pairs.
pub(crate) fn split_url(url: &str) -> (Option<String>, String, Vec<(String, String)>) {
    let (base, rest) = match url.find("://") {
        Some(i) => {
            let after = url.get(i + 3..).unwrap_or("");
            let end = after.find(['/', '?', '#']).unwrap_or(after.len());
            let base = url.get(..i + 3 + end).unwrap_or(url).to_owned();
            (Some(base), after.get(end..).unwrap_or("").to_owned())
        }
        None => (None, url.to_owned()),
    };
    let rest = rest.split('#').next().unwrap_or("").to_owned();
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p.to_owned(), q.to_owned()),
        None => (rest, String::new()),
    };
    let pairs = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (text::percent_decode(k), text::percent_decode(v)),
            None => (text::percent_decode(p), String::new()),
        })
        .collect();
    let path = if path.is_empty() {
        "/".to_owned()
    } else {
        path
    };
    (base, path, pairs)
}

fn header<'a>(req: &'a HttpRequest, name: &str) -> Option<&'a str> {
    req.headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// The bearer token of a request, if it has a non-empty one.
fn bearer(req: &HttpRequest) -> Option<&str> {
    let value = header(req, "authorization")?;
    let mut parts = value.splitn(2, ' ');
    let scheme = parts.next().unwrap_or("");
    let given = parts.next().unwrap_or("").trim();
    (scheme.eq_ignore_ascii_case("bearer") && !given.is_empty()).then_some(given)
}

/// Whether a JMAP request names Masked Email in `using`.
fn uses_masked_email(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| {
            v.get("using")?
                .as_array()
                .map(|u| u.iter().any(|c| c.as_str() == Some(MASKED)))
        })
        .unwrap_or(false)
}

/// Handles one request. `base` is the `scheme://host` the client used, so
/// session URLs point back at the same server.
pub(crate) async fn handle(
    server: &FakeServer,
    req: HttpRequest,
    base: Option<String>,
    shutdown: Option<watch::Receiver<bool>>,
) -> Reply {
    let (url_base, path, query) = split_url(&req.url);
    let base = base
        .or(url_base)
        .unwrap_or_else(|| IN_PROCESS_BASE.to_owned());

    let (latency, fault, retry_after) = {
        let mut q = server.quirks_mut();
        let fault = if q.fail_next_with_503 > 0 {
            q.fail_next_with_503 -= 1;
            Some(503)
        } else if q.http_429_next > 0 {
            q.http_429_next -= 1;
            Some(429)
        } else {
            None
        };
        (q.latency, fault, q.retry_after_seconds)
    };
    if !latency.is_zero() {
        tokio::time::sleep(latency).await;
    }
    match fault {
        Some(503) => {
            let r = problem(503, "Service Unavailable", "injected fault");
            return with_header(r, "retry-after", &retry_after.to_string());
        }
        Some(_) => {
            let r = problem(429, "Too Many Requests", "injected rate limit");
            return with_header(r, "retry-after", &retry_after.to_string());
        }
        None => {}
    }

    let trimmed = path.trim_end_matches('/');
    let segments: Vec<String> = trimmed
        .split('/')
        .skip(1)
        .map(text::percent_decode)
        .collect();
    let segs: Vec<&str> = segments.iter().map(String::as_str).collect();

    if segs.is_empty() {
        return problem(404, "Not Found", "nothing here");
    }
    if segs == [".well-known", "jmap"] {
        let r = response(302, "text/plain", "");
        return with_header(r, "location", &format!("{base}/jmap/session"));
    }
    if let Some(reply) = oauth::route(server, &req, &segs, &query, &base) {
        return reply;
    }
    if segs.first() != Some(&"jmap") {
        return problem(404, "Not Found", "unknown path");
    }
    let given = bearer(&req);
    let Some(auth) = given.and_then(|token| server.authenticate(token)) else {
        let r = problem(401, "Unauthorized", "a valid bearer token is required");
        return with_header(
            r,
            "www-authenticate",
            &oauth::challenge(&base, given.is_some()),
        );
    };
    let forbidden = {
        let mut q = server.quirks_mut();
        let hit = q.http_403_next > 0;
        q.http_403_next = q.http_403_next.saturating_sub(1);
        hit
    };
    if forbidden {
        return problem(403, "Forbidden", "injected fault: the token lacks a scope");
    }
    let method = req.method;
    let wrong_method = || problem(405, "Method Not Allowed", "wrong method for this path");
    match segs.as_slice() {
        ["jmap", "session"] => match method {
            HttpMethod::Get => {
                let mut quirks = server.quirks_snapshot();
                if !auth.masked_email() {
                    quirks.session_without.push(MASKED.to_owned());
                }
                let session = server.with_store(|s| session_json(s, &quirks, &base));
                json_reply(200, &session)
            }
            HttpMethod::Post => wrong_method(),
        },
        ["jmap", "api"] => match method {
            HttpMethod::Post => {
                let body = req.body.clone().unwrap_or_default();
                if !auth.masked_email() && uses_masked_email(&body) {
                    let problem = api::RequestError::UnknownCapability(format!(
                        "{MASKED} was not granted to this token"
                    ));
                    return response(
                        400,
                        "application/problem+json",
                        problem.to_problem().to_string(),
                    );
                }
                let result =
                    server.commit(|store| api::process(store, &server.inner.quirks, &body));
                let lose = {
                    let mut q = server.quirks_mut();
                    let lose = q.lose_api_response_next > 0;
                    q.lose_api_response_next = q.lose_api_response_next.saturating_sub(1);
                    lose
                };
                if lose {
                    return Reply::Lost;
                }
                match result {
                    Ok(v) => json_reply(200, &v),
                    Err(e) => response(400, "application/problem+json", e.to_problem().to_string()),
                }
            }
            HttpMethod::Get => wrong_method(),
        },
        ["jmap", "event"] => match method {
            HttpMethod::Get => match push::parse_params(&query) {
                Ok(params) => Reply::Stream(StreamingResponse {
                    status: 200,
                    headers: vec![
                        ("content-type".to_owned(), "text/event-stream".to_owned()),
                        ("cache-control".to_owned(), "no-cache".to_owned()),
                    ],
                    body: push::event_stream(server.clone(), params, shutdown),
                }),
                Err(why) => problem(400, "Bad Request", &why),
            },
            HttpMethod::Post => wrong_method(),
        },
        ["jmap", "upload", account] => match method {
            HttpMethod::Post => upload(server, &req, account),
            HttpMethod::Get => wrong_method(),
        },
        ["jmap", "download", account, blob, name] => match method {
            HttpMethod::Get => download(server, account, blob, name, &query),
            HttpMethod::Post => wrong_method(),
        },
        _ => problem(404, "Not Found", "unknown path"),
    }
}

fn upload(server: &FakeServer, req: &HttpRequest, account: &str) -> Reply {
    let mime = header(req, "content-type")
        .unwrap_or("application/octet-stream")
        .to_owned();
    let data = req.body.clone().unwrap_or_default();
    server.with_store(|store| {
        if account != store.account_id {
            return problem(404, "Not Found", "unknown account");
        }
        if data.len() as u64 > store.limits.max_size_upload {
            return problem(413, "Payload Too Large", "upload exceeds maxSizeUpload");
        }
        let blob_id = store.ids.make('U');
        let size = data.len();
        store.uploads.insert(
            blob_id.clone(),
            Upload {
                mime: mime.clone(),
                data,
            },
        );
        json_reply(
            201,
            &json!({"accountId": account, "blobId": blob_id, "type": mime, "size": size}),
        )
    })
}

/// Placeholder bytes for attachments the fake has no content for.
fn placeholder(size: u64) -> Bytes {
    let pattern = b"mejl-fake attachment placeholder\n";
    let len = usize::try_from(size).unwrap_or(0).min(64 * 1024 * 1024);
    Bytes::from(
        pattern
            .iter()
            .copied()
            .cycle()
            .take(len)
            .collect::<Vec<u8>>(),
    )
}

/// The bytes and type of a blob: an upload, a whole email or one part.
fn blob(store: &Store, blob_id: &str) -> Option<(Bytes, String)> {
    if let Some(up) = store.uploads.get(blob_id) {
        return Some((up.data.clone(), up.mime.clone()));
    }
    let rest = blob_id.strip_prefix('G')?;
    let (email_id, part_id) = match rest.split_once('.') {
        Some((e, p)) => (e, Some(p)),
        None => (rest, None),
    };
    let email = store.emails.get(email_id)?;
    let Some(part_id) = part_id else {
        return Some((Bytes::from(email.to_rfc5322()), "message/rfc822".to_owned()));
    };
    let parts = email.parts();
    let part = parts
        .text_body
        .iter()
        .chain(&parts.html_body)
        .chain(&parts.attachments)
        .find(|p| p.part_id == part_id)?;
    let bytes = match part.kind {
        PartKind::Attachment(_) => placeholder(part.size),
        kind => Bytes::from(email.part_text(kind)?.into_owned()),
    };
    Some((bytes, part.mime.clone()))
}

/// Size and type of a blob, for attachment references in `Email/set`.
pub(crate) fn blob_size(store: &Store, blob_id: &str) -> Option<(u64, String)> {
    blob(store, blob_id).map(|(b, t)| (b.len() as u64, t))
}

fn download(
    server: &FakeServer,
    account: &str,
    blob_id: &str,
    name: &str,
    query: &[(String, String)],
) -> Reply {
    server.with_store(|store| {
        if account != store.account_id {
            return problem(404, "Not Found", "unknown account");
        }
        let Some((bytes, mime)) = blob(store, blob_id) else {
            return problem(404, "Not Found", "unknown blob");
        };
        let mime = query
            .iter()
            .find(|(k, _)| k == "type")
            .map(|(_, v)| v.clone())
            .filter(|v| !v.is_empty())
            .unwrap_or(mime);
        let safe_name: String = name
            .chars()
            .filter(|c| !c.is_control() && *c != '"' && *c != '\\')
            .collect();
        let r = response(200, &mime, bytes);
        with_header(
            r,
            "content-disposition",
            &format!("attachment; filename=\"{safe_name}\""),
        )
    })
}

/// The session resource (RFC 8620 section 2), with URLs under `base`.
pub(crate) fn session_json(store: &Store, quirks: &Quirks, base: &str) -> Value {
    let l = &store.limits;
    let event = match quirks.event_source_url {
        EventSourceUrl::Template => {
            format!("{base}/jmap/event/?types={{types}}&closeafter={{closeafter}}&ping={{ping}}")
        }
        EventSourceUrl::Plain => format!("{base}/jmap/event/"),
    };
    let account_caps = json!({
        MAIL: {
            "maxMailboxesPerEmail": 1000,
            "maxMailboxDepth": null,
            "maxSizeMailboxName": 490,
            "maxSizeAttachmentsPerEmail": 50_000_000,
            "emailQuerySortOptions": api::sort_options(),
            "mayCreateTopLevelMailbox": true,
        },
        SUBMISSION: {
            "maxDelayedSend": 0,
            "submissionExtensions": {},
        },
        CONTACTS: {
            "maxAddressBooksPerCard": 1,
            "mayCreateAddressBook": false,
        },
        MASKED: {},
    });
    let primary: serde_json::Map<String, Value> = CAPABILITIES
        .iter()
        .filter(|c| **c != CORE)
        .map(|c| ((*c).to_owned(), json!(store.account_id)))
        .collect();
    let mut session = json!({
        "capabilities": {
            CORE: {
                "maxSizeUpload": l.max_size_upload,
                "maxConcurrentUpload": l.max_concurrent_upload,
                "maxSizeRequest": l.max_size_request,
                "maxConcurrentRequests": l.max_concurrent_requests,
                "maxCallsInRequest": l.max_calls_in_request,
                "maxObjectsInGet": l.max_objects_in_get,
                "maxObjectsInSet": l.max_objects_in_set,
                "collationAlgorithms": api::collations(),
            },
            MAIL: {},
            SUBMISSION: {},
            CONTACTS: {},
            MASKED: {},
        },
        "accounts": {
            store.account_id.clone(): {
                "name": store.username,
                "isPersonal": true,
                "isReadOnly": false,
                "accountCapabilities": account_caps,
            }
        },
        "primaryAccounts": primary,
        "username": store.username,
        "apiUrl": format!("{base}/jmap/api/"),
        "downloadUrl": format!("{base}/jmap/download/{{accountId}}/{{blobId}}/{{name}}?type={{type}}"),
        "uploadUrl": format!("{base}/jmap/upload/{{accountId}}/"),
        "eventSourceUrl": event,
        "state": store.session_state,
    });
    for uri in &quirks.session_without {
        if uri == CORE {
            continue;
        }
        if let Some(caps) = session["capabilities"].as_object_mut() {
            caps.remove(uri);
        }
        if let Some(caps) =
            session["accounts"][&store.account_id]["accountCapabilities"].as_object_mut()
        {
            caps.remove(uri);
        }
        if let Some(primary) = session["primaryAccounts"].as_object_mut() {
            primary.remove(uri);
        }
    }
    session
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_split() {
        let (b, p, q) =
            split_url("http://127.0.0.1:8080/jmap/event/?types=Email%2CMailbox&ping=5#frag");
        assert_eq!(b.as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!(p, "/jmap/event/");
        assert_eq!(
            q,
            vec![
                ("types".to_owned(), "Email,Mailbox".to_owned()),
                ("ping".to_owned(), "5".to_owned())
            ]
        );
        let (b, p, q) = split_url("/jmap/session");
        assert_eq!(b, None);
        assert_eq!(p, "/jmap/session");
        assert!(q.is_empty());
        let (b, p, _) = split_url("https://host.example");
        assert_eq!(b.as_deref(), Some("https://host.example"));
        assert_eq!(p, "/");
        let (_, _, q) = split_url("/x?flag&a=1&&");
        assert_eq!(
            q,
            vec![
                ("flag".to_owned(), String::new()),
                ("a".to_owned(), "1".to_owned())
            ]
        );
    }

    #[test]
    fn auth_header_parsing() {
        let req = |v: Option<&str>| HttpRequest {
            method: HttpMethod::Get,
            url: "/jmap/session".into(),
            headers: v
                .map(|v| vec![("authorization".to_owned(), v.to_owned())])
                .unwrap_or_default(),
            body: None,
        };
        assert_eq!(bearer(&req(Some("Bearer t"))), Some("t"));
        assert_eq!(bearer(&req(Some("bearer t"))), Some("t"));
        assert_eq!(bearer(&req(Some("Bearer  t "))), Some("t"));
        assert_eq!(bearer(&req(Some("Basic t"))), None);
        assert_eq!(bearer(&req(Some("Bearer"))), None);
        assert_eq!(bearer(&req(Some("Bearer "))), None);
        assert_eq!(bearer(&req(None)), None);
        assert!(uses_masked_email(
            br#"{"using":["urn:ietf:params:jmap:core","https://www.fastmail.com/dev/maskedemail"]}"#
        ));
        assert!(!uses_masked_email(
            br#"{"using":["urn:ietf:params:jmap:core"]}"#
        ));
        assert!(!uses_masked_email(b"nope"));
    }
}
