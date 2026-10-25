//! `mejl --demo`: a full mejl with no account.
//!
//! [`DemoBackend::start`] fills mejl-fake's fictional demo mailbox (Mira
//! Holt at example.org, matching the mock-ups), serves it over HTTP on
//! 127.0.0.1 with a free port on a small tokio runtime of its own, and
//! hands back the session URL and token. mejl then talks to it exactly as
//! it talks to Fastmail, push included.
//!
//! A simulator can deliver a new demo message every so often, so new mail
//! and push can be seen working. Stores go to a temporary directory that
//! is removed when the backend drops; a demo never touches the user's real
//! data.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use jiff::Timestamp;
use mejl_fake::{Address, FakeError, FakeServer, HttpHandle, IncomingMessage};
use mejl_jmap::Secret;
use tempfile::TempDir;

use crate::paths::Paths;

/// The account name demo stores use.
pub const DEMO_ACCOUNT: &str = "demo";

/// How often the simulator delivers mail in `mejl --demo`.
pub const DEFAULT_DELIVERY: Duration = Duration::from_secs(45);

/// Demo settings.
#[derive(Debug, Clone, Default)]
pub struct DemoOptions {
    /// Deliver a new message this often. `None` turns the simulator off,
    /// which tests want.
    pub deliver_every: Option<Duration>,
    /// The demo's "now". `None` means the real current time, so dates in
    /// the list read naturally.
    pub now: Option<Timestamp>,
}

impl DemoOptions {
    /// The settings `mejl --demo` uses.
    pub fn interactive() -> Self {
        DemoOptions {
            deliver_every: Some(DEFAULT_DELIVERY),
            now: None,
        }
    }
}

/// A running demo server.
pub struct DemoBackend {
    server: FakeServer,
    session_url: String,
    http: Option<HttpHandle>,
    // Dropped explicitly, after the server handle, in Drop.
    runtime: Option<tokio::runtime::Runtime>,
    dir: TempDir,
    delivered: Arc<AtomicUsize>,
}

impl std::fmt::Debug for DemoBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DemoBackend")
            .field("session_url", &self.session_url)
            .field("dir", &self.dir.path())
            .finish_non_exhaustive()
    }
}

impl DemoBackend {
    /// Starts the demo server. Call it from a plain thread, not from inside
    /// another tokio runtime.
    pub fn start(options: DemoOptions) -> std::io::Result<DemoBackend> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("mejl-demo")
            // tokio catches task panics, so they must not tear down the TUI.
            .on_thread_start(crate::terminal::mark_thread_catches_panics)
            .enable_all()
            .build()?;
        let now = options.now.unwrap_or_else(Timestamp::now);
        let server = FakeServer::demo(now);
        let loopback = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
        let http = runtime.block_on(server.serve_http(loopback))?;
        let session_url = http.session_url();
        let dir = tempfile::Builder::new().prefix("mejl-demo-").tempdir()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
        }
        let delivered = Arc::new(AtomicUsize::new(0));
        if let Some(every) = options.deliver_every {
            let server = server.clone();
            let delivered = delivered.clone();
            runtime.spawn(async move {
                let mut ticker = tokio::time::interval(every.max(Duration::from_millis(10)));
                // The first tick fires at once; skip it so the demo opens
                // on the mailbox as it is.
                ticker.tick().await;
                loop {
                    ticker.tick().await;
                    let n = delivered.load(Ordering::Relaxed);
                    match server.deliver_incoming(demo_message(n, Timestamp::now())) {
                        Ok(id) => {
                            delivered.fetch_add(1, Ordering::Relaxed);
                            tracing::debug!(email = %id, "demo delivered a message");
                        }
                        Err(e) => tracing::warn!("demo delivery failed: {e}"),
                    }
                }
            });
        }
        Ok(DemoBackend {
            server,
            session_url,
            http: Some(http),
            runtime: Some(runtime),
            dir,
            delivered,
        })
    }

    /// The loopback session URL.
    pub fn session_url(&self) -> &str {
        &self.session_url
    }

    /// The token the demo server accepts. It is fake, not a secret, but it
    /// travels as one so the code path is the same as for a real account.
    pub fn token(&self) -> Secret {
        Secret::new(self.server.token())
    }

    /// The temporary base directory; removed on drop.
    pub fn base_dir(&self) -> &Path {
        self.dir.path()
    }

    /// mejl's directories for the demo, all under [`DemoBackend::base_dir`].
    pub fn paths(&self) -> Paths {
        Paths::under(self.dir.path())
    }

    /// The fake server, to simulate changes from elsewhere.
    pub fn server(&self) -> &FakeServer {
        &self.server
    }

    /// The demo runtime, for tasks that belong with the demo.
    pub fn runtime_handle(&self) -> Option<tokio::runtime::Handle> {
        self.runtime.as_ref().map(|r| r.handle().clone())
    }

    /// Delivers the next demo message now. Returns its email id.
    pub fn deliver_next(&self) -> Result<String, FakeError> {
        let n = self.delivered.fetch_add(1, Ordering::Relaxed);
        self.server
            .deliver_incoming(demo_message(n, Timestamp::now()))
    }

    /// Messages the simulator and [`DemoBackend::deliver_next`] delivered.
    pub fn delivered(&self) -> usize {
        self.delivered.load(Ordering::Relaxed)
    }
}

impl Drop for DemoBackend {
    fn drop(&mut self) {
        // Stop listening first, then end the runtime, then the TempDir
        // field drops and removes the stores.
        drop(self.http.take());
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_timeout(Duration::from_millis(500));
        }
    }
}

/// Fills the demo account's local-only data, which no server holds: the
/// rules and the saved searches from the mock-ups, and one earlier copy of
/// the rule block, so the rules screen shows a block that changed since it
/// was last pasted. Mailbox ids come from the fake server, so the rules'
/// targets resolve. A rule whose mailbox is missing is left out.
pub fn seed_local(
    store: &mut mejl_store::Store,
    server: &FakeServer,
    now: Timestamp,
) -> mejl_store::Result<()> {
    use mejl_sieve::{
        Action as A, AddressHeader, AddressPart, Condition, MailboxRef, MatchMode, Rule, Test,
        TextOp,
    };
    let from_domain = |domain: &str| {
        Condition::new(Test::Address {
            header: AddressHeader::From,
            part: AddressPart::Domain,
            op: TextOp::Is,
            value: domain.into(),
        })
    };
    let target = |path: &[&str]| -> Option<MailboxRef> {
        let id = server.mailbox_id(&path.join("/"))?;
        Some(MailboxRef::new(id, path.iter().map(|p| (*p).to_owned())))
    };
    let mut rules: Vec<Rule> = Vec::new();
    let mut old_block: Vec<Rule> = Vec::new();
    if let Some(news_box) = target(&["Newsletters"]) {
        let mut news = Rule::new("r_18aa", "Newsletters");
        news.conditions.push(from_domain("news.example.net"));
        news.actions.push(A::MoveTo { mailbox: news_box });
        old_block.push(news.clone());
        rules.push(news);
    }
    if let Some(receipts_box) = target(&["Receipts"]) {
        let mut receipts = Rule::new("r_2c41", "Receipts to folder");
        receipts.match_mode = MatchMode::Any;
        receipts.conditions.push(from_domain("shop.example.com"));
        receipts.conditions.push(Condition::new(Test::Subject {
            op: TextOp::Contains,
            value: "receipt".into(),
        }));
        receipts.actions.push(A::MoveTo {
            mailbox: receipts_box,
        });
        // The copy pasted earlier did not mark receipts read yet.
        old_block.push(receipts.clone());
        receipts.actions.push(A::MarkRead);
        rules.push(receipts);
    }
    if let Some(garden_box) = target(&["Projects", "Garden club"]) {
        let mut garden = Rule::new("r_5d07", "Garden club label");
        garden.conditions.push(Condition::new(Test::ListId {
            op: TextOp::Contains,
            value: "garden-club.example.org".into(),
        }));
        garden.actions.push(A::AddLabel {
            mailbox: garden_box,
        });
        rules.push(garden);
    }
    let mut promo = Rule::new("r_77e2", "Old promo");
    promo.enabled = false;
    promo.conditions.push(from_domain("promo.example.com"));
    promo.actions.push(A::MoveToTrash);
    rules.push(promo);
    for (order, rule) in rules.iter().enumerate() {
        let json = mejl_ui::rules::to_json(rule);
        store.save_rule(&mejl_store::RuleRecord {
            id: json.id,
            name: rule.name.clone(),
            enabled: rule.enabled,
            sort_order: i64::try_from(order).unwrap_or(i64::MAX),
            rule_json: json.json,
            updated_at: now,
        })?;
    }
    if let Ok(block) = mejl_sieve::render_block(&old_block) {
        let copied = now
            .checked_sub(jiff::SignedDuration::from_hours(72))
            .unwrap_or(now);
        store.record_rule_render(&block.text, &block.body_sha256, copied)?;
    }
    let saved = serde_json::json!([
        {"id": "s1", "name": "VIPs, unread", "query": "is:unread from:juniper"},
        {"id": "s2", "name": "Masked, 7 days", "query": "deliveredto:masked"},
        {"id": "s3", "name": "Has attachment", "query": "has:attachment"},
    ]);
    store.set_ui_value(
        crate::app::data::SAVED_SEARCHES_KEY,
        &saved.to_string(),
        now,
    )?;
    Ok(())
}

/// The `n`th simulated message. Everything in it is fictional.
pub fn demo_message(n: usize, at: Timestamp) -> IncomingMessage {
    const MESSAGES: &[(&str, &str, &str, &str)] = &[
        (
            "Juniper Vale",
            "juniper@vale.example.net",
            "Seed swap moved to Saturday",
            "Hi Mira,\n\nThe seed swap moves to Saturday at ten, same hall.\nBring labels if you have spare ones.\n\nJuniper",
        ),
        (
            "Harbour Library",
            "notices@library.example.org",
            "Your reserved book is ready",
            "Hello,\n\nThe book you reserved is ready to collect at the front desk.\nWe keep it for seven days.\n\nHarbour Library",
        ),
        (
            "Otto Brand",
            "otto@brand.example.com",
            "Re: Hall repairs quote",
            "Mira,\n\nThe roofer can start on the 4th. The quote holds until then.\n\nOtto",
        ),
        (
            "Lumen Rail",
            "tickets@rail.example.com",
            "Booking confirmed: Northgate to Eastmoor",
            "Your booking is confirmed.\n\nDeparts 08:14, platform 3.\nSeat 42, coach C.\n\nLumen Rail",
        ),
        (
            "Priya Anand",
            "priya@anand.example.org",
            "Photos from the garden day",
            "Hi!\n\nI put the photos from Sunday in the shared folder.\nThe tomato row came out well.\n\nPriya",
        ),
    ];
    let (name, email, subject, text) = MESSAGES[n % MESSAGES.len()];
    IncomingMessage::new(Address::new(name, email), subject)
        .text(text)
        .received_at(at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mejl_jmap::{Client, HttpTransport, capability};

    fn options() -> DemoOptions {
        DemoOptions {
            deliver_every: None,
            now: Some("2026-10-23T14:27:00Z".parse().unwrap()),
        }
    }

    #[test]
    fn demo_serves_a_session_over_loopback() {
        let demo = DemoBackend::start(options()).unwrap();
        assert!(demo.session_url().starts_with("http://127.0.0.1:"));
        assert!(crate::config::check_session_url(demo.session_url()).is_ok());
        let transport = Arc::new(HttpTransport::new().unwrap());
        let client = Client::new(transport, demo.session_url(), demo.token());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let session = rt.block_on(client.fetch_session()).unwrap();
        assert_eq!(session.username, "mira@example.org");
        assert!(session.has_capability(capability::MAIL));
        assert!(session.has_capability(capability::MASKED_EMAIL));
        assert!(session.api_url.starts_with("http://127.0.0.1:"));
    }

    #[test]
    fn demo_dir_is_temporary_and_private() {
        let demo = DemoBackend::start(options()).unwrap();
        let base = demo.base_dir().to_owned();
        assert!(base.is_dir());
        let paths = demo.paths();
        assert!(paths.data_dir.starts_with(&base));
        let account = paths.ensure_account_dir(DEMO_ACCOUNT).unwrap();
        std::fs::write(account.join("cache.db"), b"x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&base).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        drop(demo);
        assert!(!base.exists());
    }

    #[test]
    fn deliver_next_adds_mail() {
        let demo = DemoBackend::start(options()).unwrap();
        let id = demo.deliver_next().unwrap();
        assert!(!id.is_empty());
        assert_eq!(demo.delivered(), 1);
        let got = demo
            .server()
            .call(
                "Email/get",
                serde_json::json!({"ids": [id], "properties": ["subject"]}),
            )
            .unwrap();
        assert_eq!(
            got["list"][0]["subject"].as_str(),
            Some("Seed swap moved to Saturday")
        );
    }

    #[test]
    fn simulator_delivers_on_its_own() {
        let demo = DemoBackend::start(DemoOptions {
            deliver_every: Some(Duration::from_millis(20)),
            ..options()
        })
        .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while demo.delivered() < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(demo.delivered() >= 2);
    }

    #[test]
    fn local_seed_has_the_mock_up_rules_and_searches() {
        let demo = DemoBackend::start(options()).unwrap();
        let dir = demo.paths().ensure_account_dir(DEMO_ACCOUNT).unwrap();
        let mut store = mejl_store::Store::open(&dir, mejl_store::StoreOptions::default()).unwrap();
        seed_local(&mut store, demo.server(), Timestamp::now()).unwrap();
        let reader = store.reader().unwrap();
        let rules = reader.rules().unwrap();
        let names: Vec<&str> = rules.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Newsletters",
                "Receipts to folder",
                "Garden club label",
                "Old promo"
            ]
        );
        assert!(!rules[3].enabled);
        for r in &rules {
            let parsed: mejl_sieve::Rule = serde_json::from_str(&r.rule_json).unwrap();
            assert_eq!(parsed.id.as_str(), r.id);
        }
        let renders = reader.rule_renders(5).unwrap();
        assert_eq!(renders.len(), 1);
        assert!(renders[0].sieve.contains("mejl managed block"));
        let saved = reader
            .ui_value(crate::app::data::SAVED_SEARCHES_KEY)
            .unwrap()
            .unwrap();
        assert!(saved.contains("Has attachment"));
    }

    #[test]
    fn demo_messages_are_fictional_and_cycle() {
        let at = Timestamp::UNIX_EPOCH;
        for n in 0..10 {
            let msg = format!("{:?}", demo_message(n, at));
            assert!(msg.contains("example."), "{msg}");
        }
        assert_eq!(
            format!("{:?}", demo_message(0, at)),
            format!("{:?}", demo_message(5, at))
        );
    }
}
