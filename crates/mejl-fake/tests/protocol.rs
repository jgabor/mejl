//! Protocol behaviour through the in-process transport.

mod common;

use std::collections::BTreeSet;

use common::{ALL, Client};
use mejl_fake::{
    Address, AttachmentSpec, ExternalChange, FakeConfig, FakeError, FakeServer, IncomingMessage,
    Limits, MaskedEmailState,
};
use serde_json::{Value, json};

fn ids(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|i| i.as_str().unwrap().to_owned())
        .collect()
}

fn incoming(subject: &str) -> IncomingMessage {
    IncomingMessage::new(Address::new("Ada Abara", "ada@example.com"), subject)
        .text(format!("Body of {subject}"))
}

#[tokio::test]
async fn session_shape_and_auth() {
    let c = Client::empty();
    let resp = c.get("/jmap/session").await;
    assert_eq!(resp.status, 200);
    let s: Value = serde_json::from_slice(&resp.body).unwrap();
    let core = &s["capabilities"]["urn:ietf:params:jmap:core"];
    assert_eq!(core["maxCallsInRequest"], 50);
    assert_eq!(core["maxObjectsInGet"], 4096);
    assert_eq!(core["maxObjectsInSet"], 4096);
    assert_eq!(core["maxConcurrentRequests"], 10);
    for cap in ALL {
        assert!(s["capabilities"].get(*cap).is_some(), "{cap}");
    }
    assert_eq!(
        s["primaryAccounts"]["https://www.fastmail.com/dev/maskedemail"],
        c.account
    );
    assert_eq!(s["accounts"][&c.account]["isPersonal"], true);
    let mail_caps = &s["accounts"][&c.account]["accountCapabilities"]["urn:ietf:params:jmap:mail"];
    assert!(
        mail_caps["emailQuerySortOptions"]
            .as_array()
            .unwrap()
            .contains(&json!("receivedAt"))
    );
    assert_eq!(s["apiUrl"], "http://mejl-fake.invalid/jmap/api/");
    assert_eq!(
        s["eventSourceUrl"],
        "http://mejl-fake.invalid/jmap/event/?types={types}&closeafter={closeafter}&ping={ping}"
    );
    assert!(s["downloadUrl"].as_str().unwrap().contains("{blobId}"));
    assert!(s["state"].is_string());

    let transport = c.server.transport();
    use mejl_jmap::transport::{HttpMethod, HttpRequest, Transport};
    for headers in [
        vec![],
        vec![("authorization".to_owned(), "Bearer wrong".to_owned())],
        vec![(
            "authorization".to_owned(),
            format!("Basic {}", c.server.token()),
        )],
    ] {
        let r = transport
            .send(HttpRequest {
                method: HttpMethod::Get,
                url: c.server.session_url(),
                headers,
                body: None,
            })
            .await
            .unwrap();
        assert_eq!(r.status, 401);
        assert!(r.header("www-authenticate").unwrap().starts_with("Bearer"));
    }
    let r = c.post_raw("{}").await;
    assert_eq!(r.status, 400);
    let r = transport
        .send(HttpRequest {
            method: HttpMethod::Post,
            url: c.server.session_url(),
            headers: c.auth(),
            body: None,
        })
        .await
        .unwrap();
    assert_eq!(r.status, 405);
    assert_eq!(c.get("/nope").await.status, 404);
    let wk = c.get("/.well-known/jmap").await;
    assert_eq!(wk.status, 302);
    assert_eq!(
        wk.header("location"),
        Some("http://mejl-fake.invalid/jmap/session")
    );
}

#[tokio::test]
async fn request_level_errors() {
    let c = Client::empty();
    let r = c
        .post_raw(
            json!({"using": ["urn:ietf:params:jmap:core", "urn:example:nope"], "methodCalls": []})
                .to_string(),
        )
        .await;
    assert_eq!(r.status, 400);
    assert_eq!(r.header("content-type"), Some("application/problem+json"));
    let p: Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(p["type"], "urn:ietf:params:jmap:error:unknownCapability");
    assert_eq!(p["status"], 400);

    let r = c.post_raw("not json").await;
    let p: Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(p["type"], "urn:ietf:params:jmap:error:notJSON");

    for bad in [
        json!({"methodCalls": []}),
        json!({"using": "core", "methodCalls": []}),
        json!({"using": [], "methodCalls": [["Core/echo", {}]]}),
        json!({"using": [], "methodCalls": [["Core/echo", [], "0"]]}),
        json!({"using": [], "methodCalls": [], "extra": 1}),
        json!([1, 2]),
    ] {
        let r = c.post_raw(bad.to_string()).await;
        let p: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(p["type"], "urn:ietf:params:jmap:error:notRequest", "{bad}");
    }

    let server = FakeServer::new(FakeConfig {
        limits: Limits {
            max_calls_in_request: 2,
            ..Limits::default()
        },
        ..FakeConfig::default()
    });
    let c2 = Client::new(server);
    let calls: Vec<Value> = (0..3)
        .map(|i| json!(["Core/echo", {}, i.to_string()]))
        .collect();
    let r = c2
        .post_raw(json!({"using": ALL, "methodCalls": calls}).to_string())
        .await;
    let p: Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(p["type"], "urn:ietf:params:jmap:error:limit");
    assert_eq!(p["limit"], "maxCallsInRequest");
}

#[tokio::test]
async fn method_level_errors() {
    let c = Client::empty();
    let r = c
        .calls(json!([
            ["Core/echo", {"hello": [1, 2]}, "a"],
            ["Nope/get", {}, "b"],
            ["Email/get", {"accountId": c.account, "ids": [], "fetchTextBodyValue": true}, "c"],
            ["Email/get", {"accountId": "someone-else", "ids": []}, "d"],
            ["Email/get", {"ids": []}, "e"],
        ]))
        .await;
    assert_eq!(r[0], json!(["Core/echo", {"hello": [1, 2]}, "a"]));
    assert_eq!(r[1][1]["type"], "unknownMethod");
    assert_eq!(r[2][1]["type"], "invalidArguments");
    assert_eq!(r[3][1]["type"], "accountNotFound");
    assert_eq!(r[4][1]["type"], "invalidArguments");

    // A method whose capability is missing from `using`.
    let r = c
        .request(
            &["urn:ietf:params:jmap:core"],
            json!([["Mailbox/get", {"accountId": c.account}, "0"]]),
        )
        .await;
    assert_eq!(r["methodResponses"][0][1]["type"], "unknownMethod");
    // Core missing from `using`, as Fastmail reportedly rejects.
    let r = c
        .request(
            &["https://www.fastmail.com/dev/maskedemail"],
            json!([["MaskedEmail/get", {"accountId": c.account}, "0"]]),
        )
        .await;
    assert_eq!(r["methodResponses"][0][1]["type"], "unknownMethod");
}

#[tokio::test]
async fn back_references_and_wildcards() {
    let c = Client::demo();
    let inbox = c.mailbox("inbox");
    let r = c
        .calls(json!([
            ["Email/query", {"accountId": c.account, "filter": {"inMailbox": inbox},
                "sort": [{"property": "receivedAt", "isAscending": false}], "collapseThreads": true, "limit": 5}, "q"],
            ["Email/get", {"accountId": c.account, "#ids": {"resultOf": "q", "name": "Email/query", "path": "/ids"},
                "properties": ["threadId", "subject"]}, "g"],
            ["Thread/get", {"accountId": c.account, "#ids": {"resultOf": "g", "name": "Email/get", "path": "/list/*/threadId"}}, "t"],
            ["Email/get", {"accountId": c.account, "#ids": {"resultOf": "t", "name": "Thread/get", "path": "/list/*/emailIds"},
                "properties": ["subject"]}, "all"],
            ["Email/get", {"accountId": c.account, "#ids": {"resultOf": "q", "name": "Email/get", "path": "/ids"}}, "bad1"],
            ["Email/get", {"accountId": c.account, "#ids": {"resultOf": "q", "name": "Email/query", "path": "/nope"}}, "bad2"],
            ["Mailbox/get", {"accountId": "wrong"}, "err"],
            ["Email/get", {"accountId": c.account, "#ids": {"resultOf": "err", "name": "Mailbox/get", "path": "/ids"}}, "bad3"],
            ["Email/get", {"accountId": c.account, "ids": [], "#ids": {"resultOf": "q", "name": "Email/query", "path": "/ids"}}, "bad4"],
        ]))
        .await;
    let query_ids = ids(&r[0][1]["ids"]);
    assert_eq!(query_ids.len(), 5);
    assert_eq!(r[1][1]["list"].as_array().unwrap().len(), 5);
    let threads = r[2][1]["list"].as_array().unwrap();
    assert_eq!(threads.len(), 5);
    let total: usize = threads
        .iter()
        .map(|t| t["emailIds"].as_array().unwrap().len())
        .sum();
    assert!(total > 5, "threads have replies");
    assert_eq!(
        r[3][1]["list"].as_array().unwrap().len(),
        total,
        "wildcard flattens arrays"
    );
    for i in [4, 5, 7] {
        assert_eq!(r[i][0], "error");
        assert_eq!(r[i][1]["type"], "invalidResultReference", "{}", r[i]);
    }
    assert_eq!(r[8][1]["type"], "invalidArguments");
}

#[tokio::test]
async fn changes_page_with_max_changes() {
    let c = Client::empty();
    let start = c.call("Email/get", json!({"ids": []})).await["state"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut delivered = BTreeSet::new();
    for i in 0..7 {
        delivered.insert(
            c.server
                .deliver_incoming(incoming(&format!("m{i}")))
                .unwrap(),
        );
    }
    let first = delivered.iter().next().unwrap().clone();
    c.server
        .mutate_from_elsewhere(ExternalChange::SetKeyword {
            email_id: first.clone(),
            keyword: "$seen".into(),
            value: true,
        })
        .unwrap();
    let mut since = start.clone();
    let mut created = BTreeSet::new();
    let mut updated = BTreeSet::new();
    let mut pages = 0;
    loop {
        let r = c
            .call(
                "Email/changes",
                json!({"sinceState": since, "maxChanges": 3}),
            )
            .await;
        pages += 1;
        let page_ids = ids(&r["created"]);
        let page_updates = ids(&r["updated"]);
        assert!(page_ids.len() + page_updates.len() <= 3);
        // Within one page, created then updated shows only as created.
        assert!(page_updates.iter().all(|u| !page_ids.contains(u)));
        created.extend(page_ids);
        updated.extend(page_updates);
        since = r["newState"].as_str().unwrap().to_owned();
        if r["hasMoreChanges"] == false {
            break;
        }
        assert!(pages < 10);
    }
    assert_eq!(pages, 3);
    assert_eq!(created, delivered);
    assert!(updated.is_subset(&delivered));

    // From the start in one page, the updated email is only in created.
    let r = c.call("Email/changes", json!({"sinceState": start})).await;
    assert_eq!(ids(&r["created"]).len(), 7);
    assert!(ids(&r["updated"]).is_empty());
    let now_state = c.call("Email/get", json!({"ids": []})).await["state"].clone();
    assert_eq!(json!(since), now_state);

    // An update after the page boundary shows up as updated.
    c.server
        .mutate_from_elsewhere(ExternalChange::SetKeyword {
            email_id: first.clone(),
            keyword: "$flagged".into(),
            value: true,
        })
        .unwrap();
    let r = c.call("Email/changes", json!({"sinceState": since})).await;
    assert_eq!(ids(&r["updated"]), vec![first.clone()]);
    let r = c.call("Thread/changes", json!({"sinceState": "0"})).await;
    assert!(r.get("created").is_some());

    assert_eq!(
        c.call_err("Email/changes", json!({"sinceState": "bogus"}))
            .await,
        "cannotCalculateChanges"
    );
    assert_eq!(
        c.call_err("Email/changes", json!({"sinceState": "999999"}))
            .await,
        "cannotCalculateChanges"
    );
    assert_eq!(
        c.call_err(
            "Email/changes",
            json!({"sinceState": since, "maxChanges": 0})
        )
        .await,
        "invalidArguments"
    );
    c.server.expire_change_logs();
    assert_eq!(
        c.call_err("Email/changes", json!({"sinceState": since}))
            .await,
        "cannotCalculateChanges"
    );
}

#[tokio::test]
async fn change_log_retention_expires_old_states() {
    let server = FakeServer::new(FakeConfig {
        change_log_limit: 3,
        ..FakeConfig::default()
    });
    let c = Client::new(server);
    let start = c.call("Email/get", json!({"ids": []})).await["state"]
        .as_str()
        .unwrap()
        .to_owned();
    for i in 0..5 {
        c.server
            .deliver_incoming(incoming(&format!("m{i}")))
            .unwrap();
    }
    assert_eq!(
        c.call_err("Email/changes", json!({"sinceState": start}))
            .await,
        "cannotCalculateChanges"
    );
}

/// Applies a queryChanges result to an old id list, as RFC 8620 5.6 says.
fn apply(old: &[String], removed: &Value, added: &Value) -> Vec<String> {
    let removed: BTreeSet<String> = ids(removed).into_iter().collect();
    let mut out: Vec<String> = old
        .iter()
        .filter(|i| !removed.contains(*i))
        .cloned()
        .collect();
    let mut adds: Vec<(usize, String)> = added
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["index"].as_u64().unwrap() as usize,
                a["id"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    adds.sort();
    for (idx, id) in adds {
        out.insert(idx.min(out.len()), id);
    }
    out
}

#[tokio::test]
async fn query_changes_converge() {
    for collapse in [false, true] {
        let c = Client::demo();
        let inbox = c.mailbox("inbox");
        let q = json!({
            "filter": {"operator": "AND", "conditions": [{"inMailbox": inbox}, {"notKeyword": "$flagged"}]},
            "sort": [{"property": "receivedAt", "isAscending": false}],
            "collapseThreads": collapse,
        });
        let before = c.call("Email/query", q.clone()).await;
        let old_ids = ids(&before["ids"]);
        let state = before["queryState"].as_str().unwrap().to_owned();

        // New mail, a reply in an existing thread, a flag, a move and a destroy.
        c.server.deliver_incoming(incoming("brand new")).unwrap();
        let oldest = old_ids.last().unwrap().clone();
        c.server
            .deliver_incoming(incoming("Re: reply").in_reply_to(oldest.clone()))
            .unwrap();
        c.server
            .mutate_from_elsewhere(ExternalChange::SetKeyword {
                email_id: old_ids[1].clone(),
                keyword: "$flagged".into(),
                value: true,
            })
            .unwrap();
        c.server
            .mutate_from_elsewhere(ExternalChange::MoveToMailbox {
                email_id: old_ids[2].clone(),
                mailbox_id: c.mailbox("archive"),
            })
            .unwrap();
        c.server
            .mutate_from_elsewhere(ExternalChange::DestroyEmail {
                email_id: old_ids[3].clone(),
            })
            .unwrap();

        let mut args = q.clone();
        args["sinceQueryState"] = json!(state);
        args["calculateTotal"] = json!(true);
        // upToId is accepted and ignored, so adds can land past it.
        args["upToId"] = json!(old_ids[0]);
        let ch = c.call("Email/queryChanges", args).await;
        let fresh = c.call("Email/query", q.clone()).await;
        let new_ids = ids(&fresh["ids"]);
        assert_eq!(
            apply(&old_ids, &ch["removed"], &ch["added"]),
            new_ids,
            "collapse={collapse}"
        );
        assert_eq!(ch["newQueryState"], fresh["queryState"]);
        assert_eq!(ch["total"], new_ids.len());
        let max_index = ch["added"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["index"].as_u64().unwrap())
            .max();
        assert!(
            max_index.unwrap() > 0,
            "an add lands beyond the first cached id"
        );

        // A different query, an unknown state and too many changes.
        let mut other = q.clone();
        other["sort"] = json!([{"property": "receivedAt", "isAscending": true}]);
        other["sinceQueryState"] = json!(state);
        assert_eq!(
            c.call_err("Email/queryChanges", other).await,
            "cannotCalculateChanges"
        );
        let mut unknown = q.clone();
        unknown["sinceQueryState"] = json!("nope");
        assert_eq!(
            c.call_err("Email/queryChanges", unknown).await,
            "cannotCalculateChanges"
        );
        let mut tight = q.clone();
        tight["sinceQueryState"] = json!(state);
        tight["maxChanges"] = json!(1);
        assert_eq!(
            c.call_err("Email/queryChanges", tight).await,
            "tooManyChanges"
        );
    }
}

#[tokio::test]
async fn query_filters_sorting_and_paging() {
    let c = Client::empty();
    let archive = c.mailbox("archive");
    let mut made = Vec::new();
    for (i, (from, subject, flagged)) in [
        ("Zed Zimmer", "Alpha report", false),
        ("Amy Ash", "re: beta notes", true),
        ("Bob Birch", "Gamma <plan>", false),
        ("Cleo Cole", "Delta budget", false),
    ]
    .iter()
    .enumerate()
    {
        let mut m = IncomingMessage::new(
            Address::new(
                *from,
                format!("{}@example.net", from.to_lowercase().replace(' ', ".")),
            ),
            *subject,
        )
        .text(format!("{} body with the word marmalade {i}", subject).repeat(i + 1))
        .received_at(format!("2026-10-0{}T10:00:00Z", i + 1).parse().unwrap())
        .header("X-Mejl-Test", format!("value-{i}"));
        if *flagged {
            m = m.keyword("$Flagged");
        }
        if i == 3 {
            m = m.mailbox("archive").attachment(AttachmentSpec::new(
                "budget.xlsx",
                "application/vnd.ms-excel",
                5000,
            ));
        }
        made.push(c.server.deliver_incoming(m).unwrap());
    }
    let q = |filter: Value| {
        let c = &c;
        async move {
            ids(&c
                .call(
                    "Email/query",
                    json!({"filter": filter, "sort": [{"property": "receivedAt"}]}),
                )
                .await["ids"])
        }
    };
    assert_eq!(
        q(json!({"after": "2026-10-02T10:00:00Z"})).await,
        made[1..].to_vec()
    );
    assert_eq!(
        q(json!({"before": "2026-10-02T10:00:00Z"})).await,
        made[..1].to_vec()
    );
    assert_eq!(
        q(json!({"hasKeyword": "$flagged"})).await,
        vec![made[1].clone()]
    );
    assert_eq!(q(json!({"notKeyword": "$flagged"})).await.len(), 3);
    assert_eq!(
        q(json!({"hasAttachment": true})).await,
        vec![made[3].clone()]
    );
    assert_eq!(
        q(json!({"inMailbox": archive})).await,
        vec![made[3].clone()]
    );
    assert_eq!(q(json!({"inMailboxOtherThan": [archive]})).await.len(), 3);
    assert_eq!(q(json!({"from": "birch"})).await, vec![made[2].clone()]);
    assert_eq!(q(json!({"subject": "GAMMA"})).await, vec![made[2].clone()]);
    assert_eq!(
        q(json!({"text": "budget.xlsx"})).await,
        vec![made[3].clone()]
    );
    assert_eq!(
        q(json!({"body": "marmalade 2"})).await,
        vec![made[2].clone()]
    );
    assert_eq!(
        q(json!({"header": ["X-Mejl-Test", "VALUE-1"]})).await,
        vec![made[1].clone()]
    );
    assert_eq!(q(json!({"header": ["X-Mejl-Test"]})).await.len(), 4);
    assert_eq!(
        q(json!({"operator": "OR", "conditions": [{"from": "amy"}, {"from": "zed"}]})).await,
        vec![made[0].clone(), made[1].clone()]
    );
    assert_eq!(
        q(json!({"operator": "NOT", "conditions": [{"from": "amy"}, {"from": "zed"}]})).await,
        vec![made[2].clone(), made[3].clone()]
    );
    let small = c
        .call(
            "Email/get",
            json!({"ids": [made[0]], "properties": ["size"]}),
        )
        .await["list"][0]["size"]
        .as_u64()
        .unwrap();
    assert!(q(json!({"maxSize": small})).await.is_empty());
    assert_eq!(
        q(json!({"minSize": small, "maxSize": small + 1})).await,
        vec![made[0].clone()]
    );

    let sorted = |sort: Value| {
        let c = &c;
        async move { ids(&c.call("Email/query", json!({"sort": sort})).await["ids"]) }
    };
    assert_eq!(
        sorted(json!([{"property": "subject"}])).await,
        vec![
            made[0].clone(),
            made[1].clone(),
            made[3].clone(),
            made[2].clone()
        ]
    );
    assert_eq!(
        sorted(json!([{"property": "from", "isAscending": false}])).await,
        vec![
            made[0].clone(),
            made[3].clone(),
            made[2].clone(),
            made[1].clone()
        ]
    );
    assert_eq!(
        sorted(json!([{"property": "size", "isAscending": false}])).await[0],
        made[3]
    );
    assert_eq!(
        sorted(json!([{"property": "hasKeyword", "keyword": "$flagged", "isAscending": false}, {"property": "receivedAt"}])).await[0],
        made[1]
    );

    let page = c
        .call("Email/query", json!({"sort": [{"property": "receivedAt"}], "position": -2, "limit": 1, "calculateTotal": true}))
        .await;
    assert_eq!(ids(&page["ids"]), vec![made[2].clone()]);
    assert_eq!(page["position"], 2);
    assert_eq!(page["total"], 4);
    let page = c
        .call("Email/query", json!({"sort": [{"property": "receivedAt"}], "anchor": made[1], "anchorOffset": -1, "limit": 2}))
        .await;
    assert_eq!(ids(&page["ids"]), vec![made[0].clone(), made[1].clone()]);
    let page = c.call("Email/query", json!({"position": 10})).await;
    assert!(ids(&page["ids"]).is_empty());
    assert_eq!(
        c.call_err("Email/query", json!({"anchor": "nope"})).await,
        "anchorNotFound"
    );
    assert_eq!(
        c.call_err("Email/query", json!({"filter": {"colour": "red"}}))
            .await,
        "unsupportedFilter"
    );
    assert_eq!(
        c.call_err("Email/query", json!({"sort": [{"property": "colour"}]}))
            .await,
        "unsupportedSort"
    );
    assert_eq!(
        c.call_err("Email/query", json!({"limit": -1})).await,
        "invalidArguments"
    );
}

#[tokio::test]
async fn email_get_bodies_and_headers() {
    let c = Client::demo();
    let id = ids(&c
        .call("Email/query", json!({"filter": {"subject": "has shipped"}}))
        .await["ids"])[0]
        .clone();
    let r = c
        .call(
            "Email/get",
            json!({
                "ids": [id],
                "properties": ["subject", "bodyValues", "htmlBody", "textBody", "attachments", "bodyStructure",
                    "header:X-FM-Masked-Email:asText", "header:From:asAddresses", "header:Date:asDate",
                    "header:List-Unsubscribe:asURLs", "header:Message-ID:asMessageIds", "header:X-Nope:all"],
                "fetchHTMLBodyValues": true,
                "maxBodyValueBytes": 300,
                "bodyProperties": ["partId", "type", "size", "name", "disposition"],
            }),
        )
        .await;
    let e = &r["list"][0];
    assert_eq!(
        e["header:X-FM-Masked-Email:asText"],
        "kettle.fern4821@example.org"
    );
    assert_eq!(
        e["header:From:asAddresses"],
        json!([{"name": "Larkspur Support", "email": "help@shop.example.com"}])
    );
    assert!(e["header:Date:asDate"].as_str().unwrap().ends_with('Z'));
    assert_eq!(
        e["header:List-Unsubscribe:asURLs"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(e["header:Message-ID:asMessageIds"].as_array().is_some());
    assert_eq!(e["header:X-Nope:all"], json!([]));
    let html_part = e["htmlBody"][0]["partId"].as_str().unwrap();
    let value = &e["bodyValues"][html_part];
    assert_eq!(value["isTruncated"], true);
    assert!(value["value"].as_str().unwrap().len() <= 300);
    assert!(!value["value"].as_str().unwrap().ends_with('<'));
    assert_eq!(e["attachments"][0]["name"], "invoice-48213.pdf");
    assert_eq!(e["attachments"][0]["disposition"], "attachment");
    assert_eq!(e["bodyStructure"]["type"], "multipart/mixed");
    assert!(e.get("blobId").is_none(), "only requested properties");
    assert!(
        e["textBody"][0].get("blobId").is_none(),
        "only requested body properties"
    );

    let r = c
        .call("Email/get", json!({"ids": [id], "properties": ["bodyValues", "textBody"], "fetchTextBodyValues": true}))
        .await;
    let e = &r["list"][0];
    let part = e["textBody"][0]["partId"].as_str().unwrap();
    assert_eq!(e["bodyValues"][part]["isTruncated"], false);

    let r = c.call("Email/get", json!({"ids": [id, "missing"]})).await;
    assert_eq!(r["notFound"], json!(["missing"]));
    assert!(r["list"][0]["preview"].as_str().unwrap().len() <= 1024);
    assert!(
        r["list"][0].get("bodyStructure").is_none(),
        "not a default property"
    );
    for bad in [
        json!({"ids": [id], "properties": ["header:From:asText"]}),
        json!({"ids": [id], "properties": ["nope"]}),
        json!({"ids": [id], "bodyProperties": ["nope"]}),
        json!({"ids": [id], "maxBodyValueBytes": 0}),
    ] {
        assert_eq!(c.call_err("Email/get", bad).await, "invalidArguments");
    }

    let server = FakeServer::new(FakeConfig {
        limits: Limits {
            max_objects_in_get: 2,
            ..Limits::default()
        },
        ..FakeConfig::default()
    });
    let small = Client::new(server);
    assert_eq!(
        small
            .call_err("Email/get", json!({"ids": ["a", "b", "c"]}))
            .await,
        "requestTooLarge"
    );
    assert_eq!(
        small.call_err("Mailbox/get", json!({"ids": null})).await,
        "requestTooLarge"
    );
}

#[tokio::test]
async fn email_set_patches_and_counts() {
    let c = Client::empty();
    let inbox = c.mailbox("inbox");
    let archive = c.mailbox("archive");
    let id = c.server.deliver_incoming(incoming("count me")).unwrap();
    let mb_state = c.call("Mailbox/get", json!({"ids": [inbox]})).await;
    assert_eq!(mb_state["list"][0]["unreadEmails"], 1);
    assert_eq!(mb_state["list"][0]["unreadThreads"], 1);
    let mailbox_state = mb_state["state"].clone();

    let r = c
        .call(
            "Email/set",
            json!({"update": {id.clone(): {"keywords/$seen": true, "keywords/a~1b": true}}}),
        )
        .await;
    assert_eq!(r["updated"], json!({id.clone(): null}));
    let e = c
        .call(
            "Email/get",
            json!({"ids": [id], "properties": ["keywords"]}),
        )
        .await;
    assert_eq!(
        e["list"][0]["keywords"],
        json!({"$seen": true, "a/b": true})
    );
    let mb = c.call("Mailbox/get", json!({"ids": [inbox]})).await;
    assert_eq!(mb["list"][0]["unreadEmails"], 0);
    assert_eq!(mb["list"][0]["totalEmails"], 1);
    let ch = c
        .call("Mailbox/changes", json!({"sinceState": mailbox_state}))
        .await;
    assert_eq!(ids(&ch["updated"]), vec![inbox.clone()]);
    assert_eq!(
        ch["updatedProperties"],
        json!([
            "totalEmails",
            "unreadEmails",
            "totalThreads",
            "unreadThreads"
        ])
    );

    let r = c
        .call("Email/set", json!({"update": {id.clone(): {format!("mailboxIds/{inbox}"): null, format!("mailboxIds/{archive}"): true}}}))
        .await;
    assert!(r["updated"].get(&id).is_some());
    let mb = c
        .call("Mailbox/get", json!({"ids": [inbox, archive]}))
        .await;
    assert_eq!(mb["list"][0]["totalEmails"], 0);
    assert_eq!(mb["list"][1]["totalEmails"], 1);

    let state = c.call("Email/get", json!({"ids": []})).await["state"].clone();
    let r = c
        .call(
            "Email/set",
            json!({"update": {
                id.clone(): {"keywords": {"$seen": true}, "keywords/$flagged": true},
            }}),
        )
        .await;
    assert_eq!(r["notUpdated"][&id]["type"], "invalidPatch");
    for (patch, kind) in [
        (json!({"subject": "new"}), "invalidProperties"),
        (json!({"mailboxIds": {}}), "invalidProperties"),
        (
            json!({format!("mailboxIds/{archive}"): null}),
            "invalidProperties",
        ),
        (json!({"mailboxIds/nope": true}), "invalidProperties"),
        (json!({"keywords/bad keyword": true}), "invalidProperties"),
        (json!({"keywords/$seen": false}), "invalidPatch"),
        (json!({"keywords/a/b": true}), "invalidPatch"),
        (json!({"keywords/~2": true}), "invalidPatch"),
    ] {
        let r = c
            .call("Email/set", json!({"update": {id.clone(): patch.clone()}}))
            .await;
        assert_eq!(r["notUpdated"][&id]["type"], kind, "{patch}");
    }
    let after = c.call("Email/get", json!({"ids": []})).await["state"].clone();
    assert_eq!(state, after, "rejected patches change nothing");
    let r = c
        .call(
            "Email/set",
            json!({"update": {"missing": {"keywords/$seen": true}}}),
        )
        .await;
    assert_eq!(r["notUpdated"]["missing"]["type"], "notFound");
    assert_eq!(
        c.call_err("Email/set", json!({"ifInState": "0", "destroy": [id]}))
            .await,
        "stateMismatch"
    );
    let r = c
        .call(
            "Email/set",
            json!({"ifInState": after, "destroy": [id, "missing"]}),
        )
        .await;
    assert_eq!(r["destroyed"], json!([id]));
    assert_eq!(r["notDestroyed"]["missing"]["type"], "notFound");
    let mb = c.call("Mailbox/get", json!({"ids": [archive]})).await;
    assert_eq!(mb["list"][0]["totalEmails"], 0);
    assert_eq!(mb["list"][0]["totalThreads"], 0);

    let server = FakeServer::new(FakeConfig {
        limits: Limits {
            max_objects_in_set: 1,
            ..Limits::default()
        },
        ..FakeConfig::default()
    });
    let small = Client::new(server);
    assert_eq!(
        small
            .call_err("Email/set", json!({"destroy": ["a", "b"]}))
            .await,
        "requestTooLarge"
    );
}

#[tokio::test]
async fn submission_flow_moves_to_sent_and_delivers_to_self() {
    let c = Client::demo();
    let drafts = c.mailbox("drafts");
    let sent = c.mailbox("sent");
    let inbox = c.mailbox("inbox");
    let identities = c.call("Identity/get", json!({})).await;
    assert_eq!(identities["list"].as_array().unwrap().len(), 3);
    let identity = identities["list"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["email"] == "mira@example.org")
        .unwrap()["id"]
        .clone();
    let inbox_before =
        c.call("Mailbox/get", json!({"ids": [inbox]})).await["list"][0]["totalEmails"]
            .as_u64()
            .unwrap();
    let body = json!({
        "using": ALL,
        "createdIds": {"earlier": "x1"},
        "methodCalls": [
            ["Email/set", {"accountId": c.account, "create": {"d1": {
                "mailboxIds": {drafts.clone(): true},
                "keywords": {"$draft": true, "$seen": true},
                "from": [{"name": "Mira Holt", "email": "mira@example.org"}],
                "to": [{"name": "Juniper Tran", "email": "juniper@garden.example.com"}],
                "cc": [{"email": "mira@example.org"}],
                "subject": "Rota swap",
                "bodyValues": {"t": {"value": "Happy to swap."}},
                "textBody": [{"partId": "t", "type": "text/plain"}]
            }}}, "0"],
            ["EmailSubmission/set", {"accountId": c.account,
                "create": {"s1": {"identityId": identity, "emailId": "#d1"}},
                "onSuccessUpdateEmail": {"#s1": {
                    format!("mailboxIds/{drafts}"): null,
                    format!("mailboxIds/{sent}"): true,
                    "keywords/$draft": null
                }}}, "1"],
            ["Email/get", {"accountId": c.account, "ids": ["#d1"], "properties": ["mailboxIds", "keywords", "threadId"]}, "2"]
        ]
    });
    let resp = c.post_raw(body.to_string()).await;
    let r: Value = serde_json::from_slice(&resp.body).unwrap();
    let mr = r["methodResponses"].as_array().unwrap();
    assert_eq!(mr.len(), 4);
    assert_eq!(mr[0][0], "Email/set");
    let draft_id = mr[0][1]["created"]["d1"]["id"].as_str().unwrap().to_owned();
    assert_eq!(mr[1][0], "EmailSubmission/set");
    assert!(mr[1][1]["created"]["s1"]["id"].is_string());
    assert_eq!(mr[2][0], "Email/set", "implicit update response");
    assert_eq!(mr[2][2], "1", "same call id");
    assert!(mr[2][1]["updated"].get(&draft_id).is_some());
    assert_eq!(
        mr[3][1]["list"][0]["mailboxIds"],
        json!({sent.clone(): true})
    );
    assert_eq!(mr[3][1]["list"][0]["keywords"], json!({"$seen": true}));
    assert_eq!(r["createdIds"]["earlier"], "x1");
    assert_eq!(r["createdIds"]["d1"], draft_id);

    // The cc to the account's own address delivered a copy to the inbox.
    let inbox_after =
        c.call("Mailbox/get", json!({"ids": [inbox]})).await["list"][0]["totalEmails"]
            .as_u64()
            .unwrap();
    assert_eq!(inbox_after, inbox_before + 1);
    let copy = c
        .call(
            "Email/query",
            json!({"filter": {"inMailbox": inbox, "subject": "Rota swap"}}),
        )
        .await;
    let copy_id = ids(&copy["ids"])[0].clone();
    let copy = c
        .call(
            "Email/get",
            json!({"ids": [copy_id], "properties": ["threadId", "keywords"]}),
        )
        .await;
    assert_eq!(copy["list"][0]["threadId"], mr[3][1]["list"][0]["threadId"]);
    assert_eq!(copy["list"][0]["keywords"], json!({}));

    // Wrong sender for the identity, and no recipients.
    let r = c
        .calls(json!([
            ["Email/set", {"accountId": c.account, "create": {
                "bad": {"mailboxIds": {drafts.clone(): true}, "from": [{"email": "someone@example.net"}], "to": [{"email": "x@example.com"}], "subject": "x"},
                "none": {"mailboxIds": {drafts.clone(): true}, "from": [{"email": "mira@example.org"}], "subject": "y"}
            }}, "0"],
            ["EmailSubmission/set", {"accountId": c.account, "create": {
                "s1": {"identityId": identity, "emailId": "#bad"},
                "s2": {"identityId": identity, "emailId": "#none"},
                "s3": {"identityId": "nope", "emailId": "#none"}
            }, "onSuccessUpdateEmail": {"#s1": {"keywords/$draft": null}}}, "1"]
        ]))
        .await;
    assert_eq!(r[1][1]["notCreated"]["s1"]["type"], "forbiddenFrom");
    assert_eq!(r[1][1]["notCreated"]["s2"]["type"], "noRecipients");
    assert_eq!(r[1][1]["notCreated"]["s3"]["type"], "invalidProperties");
    assert_eq!(r[2][0], "Email/set");
    assert_eq!(
        r[2][1]["updated"],
        Value::Null,
        "failed submissions do not update emails"
    );
}

#[tokio::test]
async fn masked_email_lifecycle_and_delivery() {
    let c = Client::empty();
    let r = c
        .call("MaskedEmail/set", json!({"create": {
            "k1": {"state": "enabled", "forDomain": "https://shop.example.com", "description": "Shop"},
            "k2": {"emailPrefix": "news_1"},
            "k3": {"emailPrefix": "Bad-Prefix"},
            "k4": {"emailPrefix": "shop"},
            "k5": {"state": "sleeping"},
            "k6": {"email": "chosen@example.org"}
        }}))
        .await;
    let k1 = &r["created"]["k1"];
    let email = k1["email"].as_str().unwrap().to_owned();
    assert!(email.ends_with("@example.org"));
    let local = email.split('@').next().unwrap();
    let (a, b) = local.split_once('.').unwrap();
    assert!(a.chars().all(|c| c.is_ascii_lowercase()));
    assert!(b.chars().rev().take(4).all(|c| c.is_ascii_digit()));
    assert!(
        k1.get("forDomain").is_none(),
        "client-sent properties are not echoed"
    );
    assert!(k1["createdAt"].is_string());
    assert_eq!(k1["createdBy"], "mejl");
    assert_eq!(r["created"]["k2"]["state"], "pending");
    assert!(
        r["created"]["k2"]["email"]
            .as_str()
            .unwrap()
            .starts_with("news_1.")
    );
    for k in ["k3", "k4", "k5", "k6"] {
        assert_eq!(r["notCreated"][k]["type"], "invalidProperties", "{k}");
    }
    let id1 = k1["id"].as_str().unwrap().to_owned();
    let id2 = r["created"]["k2"]["id"].as_str().unwrap().to_owned();

    // Pending flips to enabled on first mail and records the time.
    let mail = IncomingMessage::new(Address::new("Shop", "news@shop.example.com"), "Welcome")
        .to(Address::bare(r["created"]["k2"]["email"].as_str().unwrap()))
        .text("hi");
    let delivered = c.server.deliver_incoming(mail).unwrap();
    let got = c.call("MaskedEmail/get", json!({"ids": [id2]})).await;
    assert_eq!(got["list"][0]["state"], "enabled");
    assert!(got["list"][0]["lastMessageAt"].is_string());
    let e = c
        .call(
            "Email/get",
            json!({"ids": [delivered], "properties": ["header:X-FM-Masked-Email:asText"]}),
        )
        .await;
    assert_eq!(
        e["list"][0]["header:X-FM-Masked-Email:asText"],
        got["list"][0]["email"]
    );

    // Disable: mail goes to Trash. Delete: mail bounces.
    c.server
        .mutate_from_elsewhere(ExternalChange::SetMaskedEmailState {
            id: id1.clone(),
            state: MaskedEmailState::Disabled,
        })
        .unwrap();
    let to_mask = || {
        IncomingMessage::new(Address::bare("x@example.net"), "Promo")
            .to(Address::bare(email.clone()))
            .text("x")
    };
    let trashed = c.server.deliver_incoming(to_mask()).unwrap();
    let e = c
        .call(
            "Email/get",
            json!({"ids": [trashed], "properties": ["mailboxIds"]}),
        )
        .await;
    assert_eq!(
        e["list"][0]["mailboxIds"],
        json!({c.mailbox("trash"): true})
    );
    let r = c
        .call(
            "MaskedEmail/set",
            json!({"destroy": [id1], "update": {id2.clone(): {"state": "pending"}}}),
        )
        .await;
    assert_eq!(r["destroyed"], json!([id1]));
    assert_eq!(r["notUpdated"][&id2]["type"], "invalidProperties");
    let got = c.call("MaskedEmail/get", json!({"ids": [id1]})).await;
    assert_eq!(got["list"][0]["state"], "deleted", "destroy marks deleted");
    assert_eq!(
        c.server.deliver_incoming(to_mask()),
        Err(FakeError::Bounced(email.clone()))
    );

    // Restore and describe; changes track it without the quirk.
    let state = c.call("MaskedEmail/get", json!({"ids": []})).await["state"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!state.is_empty());
    let r = c
        .call("MaskedEmail/set", json!({"update": {id1.clone(): {"state": "enabled", "description": "Shop (orders)"}, "missing": {"state": "enabled"}}}))
        .await;
    assert!(r["updated"].get(&id1).is_some());
    assert_eq!(r["notUpdated"]["missing"]["type"], "notFound");
    let ch = c
        .call("MaskedEmail/changes", json!({"sinceState": state}))
        .await;
    assert_eq!(ids(&ch["updated"]), vec![id1.clone()]);
}

#[tokio::test]
async fn mailbox_set_lifecycle() {
    let c = Client::empty();
    let r = c
        .call(
            "Mailbox/set",
            json!({"create": {
                "child": {"name": "Garden club", "parentId": "#parent"},
                "parent": {"name": "Projects", "sortOrder": 5},
                "dupe": {"name": "Inbox"},
                "role": {"name": "Junk two", "role": "junk"},
                "loop1": {"name": "L1", "parentId": "#loop2"},
                "loop2": {"name": "L2", "parentId": "#loop1"},
                "server": {"name": "X", "totalEmails": 3}
            }}),
        )
        .await;
    let parent = r["created"]["parent"]["id"].as_str().unwrap().to_owned();
    let child = r["created"]["child"]["id"].as_str().unwrap().to_owned();
    assert_eq!(r["created"]["parent"]["totalEmails"], 0);
    for k in ["dupe", "role", "loop1", "loop2", "server"] {
        assert_eq!(r["notCreated"][k]["type"], "invalidProperties", "{k}");
    }
    let got = c.call("Mailbox/get", json!({"ids": [child]})).await;
    assert_eq!(got["list"][0]["parentId"], parent);
    assert_eq!(
        c.server.mailbox_id("Projects/Garden club"),
        Some(child.clone())
    );

    let r = c
        .call(
            "Mailbox/set",
            json!({"update": {
                parent.clone(): {"parentId": child},
                child.clone(): {"name": "Allotment"}
            }}),
        )
        .await;
    assert_eq!(
        r["notUpdated"][&parent]["type"], "invalidProperties",
        "cycle"
    );
    assert!(r["updated"].get(&child).is_some());

    c.server
        .deliver_incoming(incoming("filed").mailbox("Projects/Allotment"))
        .unwrap();
    let r = c
        .call(
            "Mailbox/set",
            json!({"destroy": [parent.clone(), c.mailbox("inbox")]}),
        )
        .await;
    assert_eq!(r["notDestroyed"][&parent]["type"], "mailboxHasChild");
    assert_eq!(r["notDestroyed"][&c.mailbox("inbox")]["type"], "forbidden");
    let r = c
        .call("Mailbox/set", json!({"destroy": [child.clone()]}))
        .await;
    assert_eq!(r["notDestroyed"][&child]["type"], "mailboxHasEmail");
    let r = c
        .call(
            "Mailbox/set",
            json!({"destroy": [parent.clone(), child.clone()], "onDestroyRemoveEmails": true}),
        )
        .await;
    assert_eq!(
        r["destroyed"].as_array().unwrap().len(),
        2,
        "children first, in one call"
    );
    assert_eq!(c.call("Email/query", json!({})).await["ids"], json!([]));
}

#[tokio::test]
async fn snippets_highlight_and_escape() {
    let c = Client::empty();
    let id = c
        .server
        .deliver_incoming(
            IncomingMessage::new(Address::bare("a@example.com"), "Plot <7> rota for November")
                .text("Here is the rota & the compost plan for the plots."),
        )
        .unwrap();
    let r = c
        .call(
            "SearchSnippet/get",
            json!({"filter": {"text": "rota"}, "emailIds": [id, "missing"]}),
        )
        .await;
    let s = &r["list"][0];
    assert_eq!(
        s["subject"],
        "Plot &lt;7&gt; <mark>rota</mark> for November"
    );
    assert!(
        s["preview"]
            .as_str()
            .unwrap()
            .contains("<mark>rota</mark> &amp; the compost")
    );
    assert_eq!(r["notFound"], json!(["missing"]));
    let r = c
        .call("SearchSnippet/get", json!({"filter": {"operator": "NOT", "conditions": [{"text": "rota"}]}, "emailIds": [id]}))
        .await;
    assert_eq!(r["list"][0]["subject"], Value::Null);
}

#[tokio::test]
async fn demo_contacts_identities_and_threads() {
    let c = Client::demo();
    let cards = c.call("ContactCard/get", json!({})).await;
    let list = cards["list"].as_array().unwrap();
    assert!(list.len() >= 12);
    assert!(
        list.iter()
            .any(|card| card["name"]["full"] == "Juniper Tran"
                && card["emails"]["e1"]["address"] == "juniper@garden.example.com")
    );
    let state = cards["state"].as_str().unwrap().to_owned();
    let ch = c
        .call("ContactCard/changes", json!({"sinceState": state}))
        .await;
    assert_eq!(ch["created"], json!([]));
    let books = c.call("AddressBook/get", json!({})).await;
    assert_eq!(books["list"][0]["isDefault"], true);
    let masks = c.call("MaskedEmail/get", json!({"ids": null})).await;
    assert_eq!(masks["list"].as_array().unwrap().len(), 20);
    let sorted = c
        .call("Email/query", json!({"filter": {"inMailbox": c.mailbox("inbox")}, "sort": [{"property": "receivedAt", "isAscending": false}], "collapseThreads": true, "limit": 3}))
        .await;
    let first = c
        .call(
            "Email/get",
            json!({"ids": ids(&sorted["ids"]), "properties": ["subject", "keywords"]}),
        )
        .await;
    let subjects: Vec<&str> = first["list"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["subject"].as_str().unwrap())
        .collect();
    assert_eq!(
        subjects,
        vec![
            "Re: Garden club plot rota",
            "Draft budget for the hall repairs",
            "Re: Your order 48213 has shipped"
        ]
    );
    let mb = c.call("Mailbox/get", json!({})).await;
    let names: BTreeSet<&str> = mb["list"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    for n in [
        "Inbox",
        "Drafts",
        "Sent",
        "Archive",
        "Spam",
        "Trash",
        "Newsletters",
        "Receipts",
        "Projects",
        "Garden club",
        "Hall repairs",
        "Travel",
    ] {
        assert!(names.contains(n), "{n}");
    }
}

#[tokio::test]
async fn upload_attach_and_download() {
    use mejl_jmap::transport::{HttpMethod, HttpRequest, Transport};
    let c = Client::empty();
    let t = c.server.transport();
    let up = t
        .send(HttpRequest {
            method: HttpMethod::Post,
            url: format!("{}/jmap/upload/{}/", mejl_fake::IN_PROCESS_BASE, c.account),
            headers: [c.auth(), vec![("content-type".into(), "text/csv".into())]].concat(),
            body: Some(bytes::Bytes::from_static(b"a,b\n1,2\n")),
        })
        .await
        .unwrap();
    assert_eq!(up.status, 201);
    let up: Value = serde_json::from_slice(&up.body).unwrap();
    assert_eq!(up["size"], 8);
    let blob = up["blobId"].as_str().unwrap();
    let r = c
        .call(
            "Email/set",
            json!({"create": {"d": {
                "mailboxIds": {c.mailbox("drafts"): true},
                "subject": "with file",
                "bodyValues": {"1": {"value": "see attached"}},
                "textBody": [{"partId": "1", "type": "text/plain"}],
                "attachments": [{"blobId": blob, "type": "text/csv", "name": "data.csv"}]
            }, "missing": {
                "mailboxIds": {c.mailbox("drafts"): true},
                "attachments": [{"blobId": "nope", "type": "text/csv", "name": "x.csv"}]
            }}}),
        )
        .await;
    assert_eq!(r["notCreated"]["missing"]["type"], "blobNotFound");
    let id = r["created"]["d"]["id"].as_str().unwrap();
    let e = c
        .call(
            "Email/get",
            json!({"ids": [id], "properties": ["attachments", "hasAttachment", "blobId"]}),
        )
        .await;
    assert_eq!(e["list"][0]["hasAttachment"], true);
    assert_eq!(e["list"][0]["attachments"][0]["blobId"], blob);
    let down = c
        .get(&format!(
            "/jmap/download/{}/{blob}/data.csv?type=text/csv",
            c.account
        ))
        .await;
    assert_eq!(down.status, 200);
    assert_eq!(&down.body[..], b"a,b\n1,2\n");
    assert_eq!(down.header("content-type"), Some("text/csv"));
    let raw = e["list"][0]["blobId"].as_str().unwrap();
    let down = c
        .get(&format!("/jmap/download/{}/{raw}/msg.eml", c.account))
        .await;
    assert!(String::from_utf8_lossy(&down.body).contains("Subject: with file"));
    assert_eq!(
        c.get(&format!("/jmap/download/{}/nope/x", c.account))
            .await
            .status,
        404
    );
    assert_eq!(
        c.get(&format!("/jmap/download/other/{blob}/x"))
            .await
            .status,
        404
    );
}

#[test]
fn server_call_helper_and_errors() {
    let server = FakeServer::demo(common::now());
    let r = server.call("Mailbox/get", json!({"ids": null})).unwrap();
    assert!(r["list"].as_array().unwrap().len() >= 12);
    let err = server
        .call("Email/changes", json!({"sinceState": "x"}))
        .unwrap_err();
    assert!(matches!(err, FakeError::Method { ref kind, .. } if kind == "cannotCalculateChanges"));
    assert!(server.call("Mailbox/get", json!([1])).is_err());
    let missing = server.mutate_from_elsewhere(ExternalChange::DestroyEmail {
        email_id: "nope".into(),
    });
    assert_eq!(missing, Err(FakeError::NotFound("nope".into())));
    let bad = server.deliver_incoming(incoming("x").mailbox("No such folder"));
    assert!(matches!(bad, Err(FakeError::NotFound(_))));
    assert!(server.mailbox_id("Projects/Hall repairs").is_some());
}

#[test]
fn synthetic_is_deterministic_and_sized() {
    let a = FakeServer::synthetic(2_000, 7, common::now());
    let b = FakeServer::synthetic(2_000, 7, common::now());
    let q = json!({"sort": [{"property": "receivedAt", "isAscending": false}], "limit": 50, "calculateTotal": true});
    let ra = a.call("Email/query", q.clone()).unwrap();
    let rb = b.call("Email/query", q).unwrap();
    assert_eq!(ra["ids"], rb["ids"]);
    assert_eq!(ra["total"], 2_000);
    let threads = a
        .call(
            "Email/query",
            json!({"collapseThreads": true, "calculateTotal": true, "limit": 0}),
        )
        .unwrap();
    assert!(
        threads["total"].as_u64().unwrap() < 2_000,
        "some messages are replies"
    );
    let mb = a.call("Mailbox/get", json!({})).unwrap();
    let total: u64 = mb["list"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["totalEmails"].as_u64().unwrap())
        .sum();
    assert!(total >= 2_000);
    let e = a
        .call("Email/get", json!({"ids": ra["ids"].as_array().unwrap()[..3].to_vec(), "properties": ["subject", "preview", "bodyValues"], "fetchTextBodyValues": true}))
        .unwrap();
    for item in e["list"].as_array().unwrap() {
        let s = item["subject"].as_str().unwrap();
        assert!(s.len() >= 20, "{s}");
        assert!(!item["preview"].as_str().unwrap().is_empty());
    }
}

#[test]
#[ignore = "slow in debug builds; run with --ignored or in release"]
fn synthetic_hundred_thousand() {
    let s = FakeServer::synthetic(100_000, 1, common::now());
    let r = s
        .call("Email/query", json!({"filter": {"inMailbox": s.mailbox_id("inbox").unwrap()}, "limit": 100, "calculateTotal": true}))
        .unwrap();
    assert!(r["total"].as_u64().unwrap() > 10_000);
}

#[test]
fn a_draft_is_replaced_by_create_and_destroy_in_one_call() {
    let server = FakeServer::new(FakeConfig::default());
    let drafts = server.mailbox_id("drafts").unwrap();
    let draft = |subject: &str| {
        json!({
            "mailboxIds": {drafts.clone(): true},
            "keywords": {"$draft": true, "$seen": true},
            "subject": subject,
            "messageId": ["d1@example.org"],
            "bodyValues": {"text": {"value": "Hi"}},
            "textBody": [{"partId": "text", "type": "text/plain"}],
        })
    };
    let first = server
        .call("Email/set", json!({"create": {"draft": draft("First")}}))
        .unwrap();
    let old = first["created"]["draft"]["id"].as_str().unwrap().to_owned();
    let second = server
        .call(
            "Email/set",
            json!({"create": {"draft": draft("Second")}, "destroy": [old.clone()]}),
        )
        .unwrap();
    assert_eq!(ids(&second["destroyed"]), vec![old.clone()]);
    let new = second["created"]["draft"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let got = server
        .call(
            "Email/get",
            json!({"ids": [old.clone(), new.clone()], "properties": ["subject", "keywords", "mailboxIds"]}),
        )
        .unwrap();
    assert_eq!(ids(&got["notFound"]), vec![old.clone()]);
    assert_eq!(got["list"][0]["subject"], "Second");
    assert_eq!(
        got["list"][0]["keywords"],
        json!({"$draft": true, "$seen": true})
    );
    // Found by its Message-ID among drafts.
    let q = server
        .call(
            "Email/query",
            json!({"filter": {"header": ["Message-ID", "d1@example.org"], "hasKeyword": "$draft", "inMailbox": drafts.clone()}}),
        )
        .unwrap();
    assert_eq!(ids(&q["ids"]), vec![new.clone()]);
    // Destroying it again is notFound.
    let again = server
        .call("Email/set", json!({"destroy": [old.clone()]}))
        .unwrap();
    assert_eq!(again["notDestroyed"][&old]["type"], "notFound");
    let mb = server
        .call("Mailbox/get", json!({"ids": [drafts.clone()]}))
        .unwrap();
    assert_eq!(mb["list"][0]["totalEmails"], 1);
    assert_eq!(mb["list"][0]["unreadEmails"], 0);
}

#[test]
fn a_mailbox_role_can_be_cleared_from_elsewhere() {
    let server = FakeServer::new(FakeConfig::default());
    let drafts = server.mailbox_id("drafts").unwrap();
    let before = server.call("Mailbox/get", json!({"ids": []})).unwrap()["state"]
        .as_str()
        .unwrap()
        .to_owned();
    server
        .mutate_from_elsewhere(ExternalChange::ClearMailboxRole {
            mailbox_id: drafts.clone(),
        })
        .unwrap();
    let got = server
        .call("Mailbox/get", json!({"ids": [drafts.clone()]}))
        .unwrap();
    assert_eq!(got["list"][0]["role"], Value::Null);
    assert_eq!(got["list"][0]["name"], "Drafts");
    let changes = server
        .call("Mailbox/changes", json!({"sinceState": before}))
        .unwrap();
    assert_eq!(ids(&changes["updated"]), vec![drafts.clone()]);
    assert!(matches!(
        server.mutate_from_elsewhere(ExternalChange::ClearMailboxRole {
            mailbox_id: "nope".into()
        }),
        Err(FakeError::NotFound(_))
    ));
}
