//! The loopback HTTP server, driven by a plain HTTP client.

mod common;

use std::time::Duration;

use common::{Frame, Frames, next_frame};
use futures::StreamExt;
use mejl_fake::{Address, FakeServer, IncomingMessage};
use serde_json::{Value, json};

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

#[tokio::test]
async fn serves_session_api_and_blobs_over_loopback() {
    let server = FakeServer::demo(common::now());
    let handle = server
        .serve_http("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let base = handle.base_url();
    assert!(base.starts_with("http://127.0.0.1:"));
    let http = client();
    let auth = format!("Bearer {}", server.token());

    let r = http.get(handle.session_url()).send().await.unwrap();
    assert_eq!(r.status(), 401);
    assert!(r.headers().get("www-authenticate").is_some());

    let session: Value = http
        .get(handle.session_url())
        .header("authorization", &auth)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let api = session["apiUrl"].as_str().unwrap();
    assert_eq!(api, format!("{base}/jmap/api/"));

    let r: Value = http
        .post(api)
        .header("authorization", &auth)
        .json(&json!({
            "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
            "methodCalls": [
                ["Core/echo", {"ping": true}, "0"],
                ["Mailbox/get", {"accountId": server.account_id(), "properties": ["name", "totalEmails"]}, "1"]
            ]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["methodResponses"][0][1], json!({"ping": true}));
    assert!(r["methodResponses"][1][1]["list"].as_array().unwrap().len() >= 12);

    let r = http
        .post(api)
        .header("authorization", &auth)
        .body("{\"using\": [\"urn:example:nope\"], \"methodCalls\": []}")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    assert_eq!(r.headers()["content-type"], "application/problem+json");

    let upload_url = session["uploadUrl"]
        .as_str()
        .unwrap()
        .replace("{accountId}", server.account_id());
    let up: Value = http
        .post(&upload_url)
        .header("authorization", &auth)
        .header("content-type", "image/png")
        .body(vec![0u8, 1, 2, 3, 4])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(up["size"], 5);
    let download = session["downloadUrl"]
        .as_str()
        .unwrap()
        .replace("{accountId}", server.account_id())
        .replace("{blobId}", up["blobId"].as_str().unwrap())
        .replace("{name}", "dot.png")
        .replace("{type}", "image/png");
    let r = http
        .get(&download)
        .header("authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "image/png");
    assert_eq!(&r.bytes().await.unwrap()[..], &[0u8, 1, 2, 3, 4]);

    server.quirks_mut().http_429_next = 1;
    let r = http
        .get(handle.session_url())
        .header("authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 429);
    assert_eq!(r.headers()["retry-after"], "1");

    let r = http
        .put(handle.session_url())
        .header("authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 405);

    tokio::time::timeout(Duration::from_secs(5), handle.shutdown())
        .await
        .expect("shutdown finishes");
}

#[tokio::test]
async fn push_over_http_and_shutdown_with_open_stream() {
    let server = FakeServer::new(Default::default());
    let handle = server
        .serve_http("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let http = client();
    let r = http
        .get(format!(
            "{}/jmap/event/?types=*&closeafter=no&ping=0",
            handle.base_url()
        ))
        .header("authorization", format!("Bearer {}", server.token()))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "text/event-stream");
    let mut frames = Frames::new(r.bytes_stream().boxed());
    server
        .deliver_incoming(IncomingMessage::new(Address::bare("a@example.com"), "pushed").text("x"))
        .unwrap();
    let Frame::Event { event, data } = next_frame(&mut frames).await else {
        panic!("expected an event");
    };
    assert_eq!(event, "state");
    assert!(data["changed"][server.account_id()]["Email"].is_string());

    // Shutdown must not wait forever on the open push stream.
    tokio::time::timeout(Duration::from_secs(5), handle.shutdown())
        .await
        .expect("shutdown finishes with a push stream open");
    assert_eq!(next_frame(&mut frames).await, Frame::End);
}

#[tokio::test]
async fn dropped_push_aborts_the_connection() {
    let server = FakeServer::new(Default::default());
    server.quirks_mut().drop_push_after_events = Some(1);
    let handle = server
        .serve_http("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let r = client()
        .get(format!("{}/jmap/event/", handle.base_url()))
        .header("authorization", format!("Bearer {}", server.token()))
        .send()
        .await
        .unwrap();
    let mut frames = Frames::new(r.bytes_stream().boxed());
    server
        .deliver_incoming(IncomingMessage::new(Address::bare("a@example.com"), "one").text("x"))
        .unwrap();
    assert!(matches!(next_frame(&mut frames).await, Frame::Event { .. }));
    let after = next_frame(&mut frames).await;
    assert!(matches!(after, Frame::Error | Frame::End), "{after:?}");
    handle.shutdown().await;
}

#[tokio::test]
async fn refuses_non_loopback_addresses() {
    let server = FakeServer::new(Default::default());
    let err = server
        .serve_http("0.0.0.0:0".parse().unwrap())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    let err = server
        .serve_http("192.0.2.1:0".parse().unwrap())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[tokio::test]
async fn lost_api_response_drops_the_connection() {
    let server = FakeServer::demo(common::now());
    let handle = server
        .serve_http("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let http = client();
    let auth = format!("Bearer {}", server.token());
    server.quirks_mut().lose_api_response_next = 1;
    let request = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
        "methodCalls": [["Mailbox/set", {
            "accountId": server.account_id(),
            "create": {"a": {"name": "Kept anyway"}}
        }, "0"]]
    });
    let api = format!("{}/jmap/api/", handle.base_url());
    let result = async {
        http.post(&api)
            .header("authorization", &auth)
            .json(&request)
            .send()
            .await?
            .bytes()
            .await
    }
    .await;
    assert!(result.is_err(), "the response should be cut: {result:?}");
    assert!(server.mailbox_id("Kept anyway").is_some());
    handle.shutdown().await;
}

#[tokio::test]
async fn session_can_leave_capabilities_out() {
    let server = FakeServer::demo(common::now());
    server.quirks_mut().session_without = vec![
        "https://www.fastmail.com/dev/maskedemail".into(),
        "urn:ietf:params:jmap:submission".into(),
        // Core is never dropped: no client could talk to the server.
        "urn:ietf:params:jmap:core".into(),
    ];
    let handle = server
        .serve_http("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let session: Value = client()
        .get(handle.session_url())
        .header("authorization", format!("Bearer {}", server.token()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let caps = session["capabilities"].as_object().unwrap();
    assert!(caps.contains_key("urn:ietf:params:jmap:core"));
    assert!(caps.contains_key("urn:ietf:params:jmap:mail"));
    assert!(!caps.contains_key("urn:ietf:params:jmap:submission"));
    assert!(!caps.contains_key("https://www.fastmail.com/dev/maskedemail"));
    let account = &session["accounts"][server.account_id()]["accountCapabilities"];
    assert!(account.get("urn:ietf:params:jmap:mail").is_some());
    assert!(account.get("urn:ietf:params:jmap:submission").is_none());
    assert!(
        session["primaryAccounts"]
            .get("https://www.fastmail.com/dev/maskedemail")
            .is_none()
    );
}
