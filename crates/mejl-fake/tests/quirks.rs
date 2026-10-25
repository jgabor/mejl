//! Every quirk switch, and push behaviour, through the in-process transport.

mod common;

use std::time::{Duration, Instant};

use common::{Client, Frame, Frames, next_frame};
use mejl_fake::{
    Address, EventSourceUrl, FakeConfig, FakeServer, IncomingMessage, PushShape, Quirks,
};
use serde_json::{Value, json};

fn mail(subject: &str) -> IncomingMessage {
    IncomingMessage::new(Address::new("Ada Abara", "ada@example.com"), subject).text("hello")
}

fn changed<'a>(data: &'a Value, account: &str) -> &'a serde_json::Map<String, Value> {
    data["changed"][account].as_object().unwrap()
}

#[tokio::test]
async fn push_sends_rfc_state_changes() {
    let c = Client::empty();
    let mut frames = Frames::new(c.open_events("?types=*&closeafter=no&ping=0").await);
    c.server.deliver_incoming(mail("one")).unwrap();
    let Frame::Event { event, data } = next_frame(&mut frames).await else {
        panic!("expected an event");
    };
    assert_eq!(event, "state");
    assert_eq!(data["@type"], "StateChange");
    let ch = changed(&data, &c.account);
    for ty in ["Email", "Thread", "Mailbox", "EmailDelivery"] {
        assert!(ch.contains_key(ty), "{ty} in {ch:?}");
    }
    let email_state = c.call("Email/get", json!({"ids": []})).await["state"].clone();
    assert_eq!(ch["Email"], email_state);
    assert!(!ch.contains_key("Identity"), "only changed types");

    // A client write pushes too, and EmailDelivery stays put.
    let id = c.call("Email/query", json!({})).await["ids"][0]
        .as_str()
        .unwrap()
        .to_owned();
    c.call(
        "Email/set",
        json!({"update": {id: {"keywords/$seen": true}}}),
    )
    .await;
    let Frame::Event { data, .. } = next_frame(&mut frames).await else {
        panic!("expected an event");
    };
    let ch = changed(&data, &c.account);
    assert!(ch.contains_key("Email"));
    assert!(!ch.contains_key("EmailDelivery"));
}

#[tokio::test]
async fn push_filters_types_and_closes_after_state() {
    let c = Client::empty();
    let mut frames = Frames::new(
        c.open_events("?types=Mailbox&closeafter=state&ping=0")
            .await,
    );
    // A no-op write changes nothing and pushes nothing.
    c.call("Email/set", json!({})).await;
    c.server.deliver_incoming(mail("one")).unwrap();
    let Frame::Event { data, .. } = next_frame(&mut frames).await else {
        panic!("expected an event");
    };
    let ch = changed(&data, &c.account);
    assert_eq!(ch.keys().collect::<Vec<_>>(), vec!["Mailbox"]);
    assert_eq!(next_frame(&mut frames).await, Frame::End);

    // Mailbox-only changes do not reach an Email subscriber.
    let mut frames = Frames::new(c.open_events("?types=Email&closeafter=state&ping=0").await);
    c.call("Mailbox/set", json!({"create": {"a": {"name": "Folder"}}}))
        .await;
    c.server.deliver_incoming(mail("two")).unwrap();
    let Frame::Event { data, .. } = next_frame(&mut frames).await else {
        panic!("expected an event");
    };
    assert!(changed(&data, &c.account).contains_key("Email"));
}

#[tokio::test]
async fn push_rejects_bad_parameters() {
    let c = Client::empty();
    for q in [
        "?types={types}&closeafter={closeafter}&ping={ping}",
        "?closeafter=sometimes",
        "?ping=fast",
    ] {
        let resp = c.get(&format!("/jmap/event/{q}")).await;
        assert_eq!(resp.status, 400, "{q}");
    }
    // Calling send on the stream endpoint is a usage error, not a hang.
    use mejl_jmap::transport::{HttpMethod, HttpRequest, Transport};
    let r = c
        .server
        .transport()
        .send(HttpRequest {
            method: HttpMethod::Get,
            url: format!("{}/jmap/event/", mejl_fake::IN_PROCESS_BASE),
            headers: c.auth(),
            body: None,
        })
        .await;
    assert!(r.is_err());
}

#[tokio::test(start_paused = true)]
async fn push_pings_when_asked() {
    let c = Client::empty();
    // The clock is paused, so it jumps to the ping timer, which must fire
    // before the helper's five second timeout.
    let mut frames = Frames::new(c.open_events("?types=*&closeafter=no&ping=2").await);
    let Frame::Event { event, data } = next_frame(&mut frames).await else {
        panic!("expected a ping");
    };
    assert_eq!(event, "ping");
    assert_eq!(data, json!({"interval": 2}));
    // A state event resets the ping timer; pings keep coming after it.
    c.server.deliver_incoming(mail("between pings")).unwrap();
    let Frame::Event { event, .. } = next_frame(&mut frames).await else {
        panic!("expected a state event");
    };
    assert_eq!(event, "state");
    let Frame::Event { event, .. } = next_frame(&mut frames).await else {
        panic!("expected a ping");
    };
    assert_eq!(event, "ping");
}

#[tokio::test]
async fn quirk_fastmail_connect_shape() {
    let c = Client::empty();
    c.server.quirks_mut().push_shape = PushShape::FastmailConnect;
    let mut frames = Frames::new(c.open_events("").await);
    c.server.deliver_incoming(mail("one")).unwrap();
    let Frame::Event { event, data } = next_frame(&mut frames).await else {
        panic!("expected an event");
    };
    assert_eq!(event, "state");
    assert_eq!(data["type"], "connect");
    assert!(data.get("@type").is_none());
    assert!(changed(&data, &c.account).contains_key("EmailDelivery"));
}

#[tokio::test]
async fn quirk_event_source_url_style() {
    let c = Client::empty();
    let session = |c: &Client| {
        let c = c.server.clone();
        async move {
            let t = Client::new(c);
            let r = t.get("/jmap/session").await;
            serde_json::from_slice::<Value>(&r.body).unwrap()["eventSourceUrl"]
                .as_str()
                .unwrap()
                .to_owned()
        }
    };
    assert!(session(&c).await.contains("{types}"));
    c.server.quirks_mut().event_source_url = EventSourceUrl::Plain;
    assert_eq!(session(&c).await, "http://mejl-fake.invalid/jmap/event/");
    // The plain URL works without any parameters.
    let mut frames = Frames::new(c.open_events("").await);
    c.server.deliver_incoming(mail("plain")).unwrap();
    assert!(matches!(next_frame(&mut frames).await, Frame::Event { .. }));
}

#[tokio::test]
async fn quirk_masked_email_without_state() {
    let c = Client::empty();
    let state_before = c.call("MaskedEmail/get", json!({})).await["state"].clone();
    assert_ne!(state_before, json!(""));
    c.server.quirks_mut().masked_email_without_state = true;
    let mut frames = Frames::new(c.open_events("?types=*&closeafter=state&ping=0").await);
    let got = c.call("MaskedEmail/get", json!({"ids": null})).await;
    assert_eq!(got["state"], "");
    let set = c
        .call(
            "MaskedEmail/set",
            json!({"create": {"k": {"state": "enabled", "description": "x"}}}),
        )
        .await;
    assert_eq!(set["oldState"], Value::Null);
    assert_eq!(set["newState"], Value::Null);
    assert!(set["created"]["k"]["email"].is_string());
    assert_eq!(
        c.call_err("MaskedEmail/changes", json!({"sinceState": "0"}))
            .await,
        "cannotCalculateChanges"
    );
    // Mask changes never show up in push under this quirk.
    c.server.deliver_incoming(mail("x")).unwrap();
    let Frame::Event { data, .. } = next_frame(&mut frames).await else {
        panic!("expected an event");
    };
    assert!(!changed(&data, &c.account).contains_key("MaskedEmail"));
    assert!(Quirks::fastmail_reported().masked_email_without_state);
}

#[tokio::test]
async fn quirk_masked_email_rate_limit() {
    let c = Client::empty();
    c.server.quirks_mut().masked_email_rate_limit_next = 2;
    let r = c
        .call(
            "MaskedEmail/set",
            json!({"create": {
                "a": {"state": "enabled"}, "b": {"state": "enabled"}, "c": {"state": "enabled"}
            }}),
        )
        .await;
    assert_eq!(r["notCreated"]["a"]["type"], "rateLimit");
    assert_eq!(r["notCreated"]["b"]["type"], "rateLimit");
    assert!(r["created"]["c"]["id"].is_string());
    assert_eq!(c.server.quirks_mut().masked_email_rate_limit_next, 0);
}

#[tokio::test]
async fn quirk_http_429_then_recovers() {
    let c = Client::empty();
    {
        let mut q = c.server.quirks_mut();
        q.http_429_next = 2;
        q.retry_after_seconds = 7;
    }
    for _ in 0..2 {
        let r = c.get("/jmap/session").await;
        assert_eq!(r.status, 429);
        assert_eq!(r.header("retry-after"), Some("7"));
    }
    assert_eq!(c.get("/jmap/session").await.status, 200);
}

#[tokio::test]
async fn quirk_503_fault() {
    let c = Client::empty();
    c.server.quirks_mut().fail_next_with_503 = 1;
    let r = c
        .post_raw(json!({"using": [], "methodCalls": []}).to_string())
        .await;
    assert_eq!(r.status, 503);
    assert!(r.header("retry-after").is_some());
    assert_eq!(c.get("/jmap/session").await.status, 200);
    // Faults come before auth, as a proxy in front of the server would.
    c.server.quirks_mut().fail_next_with_503 = 1;
    use mejl_jmap::transport::{HttpMethod, HttpRequest, Transport};
    let r = c
        .server
        .transport()
        .send(HttpRequest {
            method: HttpMethod::Get,
            url: c.server.session_url(),
            headers: vec![],
            body: None,
        })
        .await
        .unwrap();
    assert_eq!(r.status, 503);
}

#[tokio::test]
async fn quirk_latency() {
    let c = Client::empty();
    c.server.quirks_mut().latency = Duration::from_millis(80);
    let start = Instant::now();
    assert_eq!(c.get("/jmap/session").await.status, 200);
    assert!(start.elapsed() >= Duration::from_millis(80));
}

#[tokio::test]
async fn quirk_drop_push_after_events() {
    let c = Client::empty();
    c.server.quirks_mut().drop_push_after_events = Some(2);
    let mut frames = Frames::new(c.open_events("").await);
    for i in 0..3 {
        c.server.deliver_incoming(mail(&format!("m{i}"))).unwrap();
    }
    assert!(matches!(next_frame(&mut frames).await, Frame::Event { .. }));
    assert!(matches!(next_frame(&mut frames).await, Frame::Event { .. }));
    assert_eq!(next_frame(&mut frames).await, Frame::Error);
    assert_eq!(next_frame(&mut frames).await, Frame::End);
}

#[tokio::test]
async fn quirk_push_chunking() {
    let c = Client::empty();
    c.server.quirks_mut().push_chunk_size = Some(7);
    let mut frames = Frames::new(c.open_events("").await);
    c.server.deliver_incoming(mail("chunky")).unwrap();
    let Frame::Event { data, .. } = next_frame(&mut frames).await else {
        panic!("expected an event");
    };
    assert_eq!(data["@type"], "StateChange");
    assert!(
        frames.chunks > 5,
        "frame arrived in {} chunks",
        frames.chunks
    );
}

#[tokio::test]
async fn quirks_from_config() {
    let server = FakeServer::new(FakeConfig {
        quirks: Quirks::fastmail_reported(),
        ..FakeConfig::default()
    });
    let c = Client::new(server);
    let r = c.get("/jmap/session").await;
    let s: Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(s["eventSourceUrl"], "http://mejl-fake.invalid/jmap/event/");
    assert_eq!(c.call("MaskedEmail/get", json!({})).await["state"], "");
}

#[tokio::test]
async fn lost_api_responses_still_commit() {
    use mejl_jmap::transport::{HttpMethod, HttpRequest, Transport, TransportError};

    let c = Client::empty();
    c.server.quirks_mut().lose_api_response_next = 1;
    let body = json!({
        "using": common::ALL,
        "methodCalls": [["Mailbox/set", {
            "accountId": c.account,
            "create": {"a": {"name": "Kept anyway"}}
        }, "0"]],
    })
    .to_string();
    let lost = c
        .server
        .transport()
        .send(HttpRequest {
            method: HttpMethod::Post,
            url: format!("{}/jmap/api/", mejl_fake::IN_PROCESS_BASE),
            headers: c.auth(),
            body: Some(body.into()),
        })
        .await;
    assert!(matches!(lost, Err(TransportError::Closed)), "{lost:?}");
    assert_eq!(c.server.quirks_mut().lose_api_response_next, 0);
    // The server acted although the client never heard back.
    assert!(c.server.mailbox_id("Kept anyway").is_some());
    // The counter ran out, so the next request is answered.
    let r = c.call("Mailbox/get", json!({"ids": null})).await;
    assert!(r["list"].as_array().unwrap().len() > 1);
}
