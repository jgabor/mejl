//! EventSource push (RFC 8620 section 7.3).
//!
//! Every committed change broadcasts the new state strings. Each open
//! stream filters them by its `types`, renders SSE frames and applies the
//! push quirks.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;

use bytes::Bytes;
use mejl_jmap::transport::{BoxStream, TransportError};
use serde_json::{Value, json};
use tokio::sync::{broadcast, watch};
use tokio::time::{Instant, Interval};

use crate::FakeServer;
use crate::config::PushShape;

/// The state strings that changed in one commit.
#[derive(Debug, Clone)]
pub(crate) struct StateChange {
    pub changed: BTreeMap<String, String>,
}

/// Parsed EventSource query parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PushParams {
    /// `None` means every type.
    pub types: Option<BTreeSet<String>>,
    pub close_after_state: bool,
    pub ping_seconds: u64,
}

/// Parses `types`, `closeafter` and `ping`. Missing parameters take the
/// defaults a plain Fastmail URL implies. Values that look like an
/// unexpanded URI template are rejected, so a client that forgets to expand
/// the template fails loudly.
pub(crate) fn parse_params(query: &[(String, String)]) -> Result<PushParams, String> {
    let mut params = PushParams {
        types: None,
        close_after_state: false,
        ping_seconds: 0,
    };
    for (k, v) in query {
        if v.contains(['{', '}']) {
            return Err(format!("{k}={v} looks like an unexpanded URI template"));
        }
        match k.as_str() {
            "types" => {
                if v == "*" {
                    params.types = None;
                } else {
                    let set: BTreeSet<String> = v
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect();
                    if set.is_empty() {
                        return Err("types must be * or a comma-separated list".into());
                    }
                    if let Some(bad) = set
                        .iter()
                        .find(|t| !t.bytes().all(|b| b.is_ascii_alphanumeric()))
                    {
                        return Err(format!("bad type name {bad:?}"));
                    }
                    params.types = Some(set);
                }
            }
            "closeafter" => {
                params.close_after_state = match v.as_str() {
                    "state" => true,
                    "no" => false,
                    _ => return Err("closeafter must be state or no".into()),
                };
            }
            "ping" => {
                params.ping_seconds = v
                    .parse::<u64>()
                    .ok()
                    .filter(|n| *n <= 86_400)
                    .ok_or("ping must be a number of seconds")?;
            }
            _ => {}
        }
    }
    Ok(params)
}

fn state_frame(shape: PushShape, account_id: &str, changed: &BTreeMap<String, String>) -> Bytes {
    let mut data = match shape {
        PushShape::Rfc => json!({"@type": "StateChange"}),
        PushShape::FastmailConnect => json!({"type": "connect"}),
    };
    if let Value::Object(m) = &mut data {
        m.insert("changed".to_owned(), json!({ account_id: changed }));
    }
    Bytes::from(format!("event: state\ndata: {data}\n\n"))
}

fn ping_frame(interval: u64) -> Bytes {
    Bytes::from(format!(
        "event: ping\ndata: {}\n\n",
        json!({"interval": interval})
    ))
}

struct StreamState {
    server: FakeServer,
    rx: broadcast::Receiver<StateChange>,
    shutdown: Option<watch::Receiver<bool>>,
    params: PushParams,
    ping: Option<Interval>,
    shape: PushShape,
    drop_after: Option<u32>,
    chunk: Option<usize>,
    sent: u32,
    queue: VecDeque<Bytes>,
    finished: bool,
    fail_next: bool,
}

enum Next {
    Change(BTreeMap<String, String>),
    Ping,
    End,
}

async fn tick(ping: &mut Option<Interval>) {
    match ping {
        Some(i) => {
            i.tick().await;
        }
        None => std::future::pending::<()>().await,
    }
}

async fn stopped(shutdown: &mut Option<watch::Receiver<bool>>) {
    match shutdown {
        Some(rx) => {
            if rx.wait_for(|v| *v).await.is_err() {
                // The handle is gone without signalling; keep running.
                std::future::pending::<()>().await;
            }
        }
        None => std::future::pending::<()>().await,
    }
}

impl StreamState {
    fn filter(&self, changed: BTreeMap<String, String>) -> BTreeMap<String, String> {
        match &self.params.types {
            None => changed,
            Some(types) => changed
                .into_iter()
                .filter(|(k, _)| types.contains(k))
                .collect(),
        }
    }

    fn enqueue(&mut self, frame: Bytes) {
        match self.chunk {
            Some(size) if size > 0 && frame.len() > size => {
                let mut start = 0;
                while start < frame.len() {
                    let end = (start + size).min(frame.len());
                    self.queue.push_back(frame.slice(start..end));
                    start = end;
                }
            }
            _ => self.queue.push_back(frame),
        }
        self.sent += 1;
        if let Some(i) = self.ping.as_mut() {
            i.reset();
        }
        if self.drop_after.is_some_and(|n| self.sent >= n) {
            self.fail_next = true;
        }
    }
}

/// Opens a push stream. It subscribes at once, so no change made after
/// this call is missed.
pub(crate) fn event_stream(
    server: FakeServer,
    params: PushParams,
    shutdown: Option<watch::Receiver<bool>>,
) -> BoxStream<'static, Result<Bytes, TransportError>> {
    let quirks = server.quirks_snapshot();
    let rx = server.inner.events.subscribe();
    let ping = (params.ping_seconds > 0).then(|| {
        let period = Duration::from_secs(params.ping_seconds);
        tokio::time::interval_at(Instant::now() + period, period)
    });
    let state = StreamState {
        server,
        rx,
        shutdown,
        params,
        ping,
        shape: quirks.push_shape,
        drop_after: quirks.drop_push_after_events,
        chunk: quirks.push_chunk_size,
        sent: 0,
        queue: VecDeque::new(),
        finished: false,
        fail_next: false,
    };
    Box::pin(futures::stream::unfold(state, |mut st| async move {
        loop {
            if let Some(chunk) = st.queue.pop_front() {
                return Some((Ok(chunk), st));
            }
            if st.fail_next {
                st.fail_next = false;
                st.finished = true;
                // Give an HTTP server time to flush the last frame before
                // the connection is cut.
                tokio::time::sleep(Duration::from_millis(20)).await;
                return Some((Err(TransportError::Closed), st));
            }
            if st.finished {
                return None;
            }
            let next = tokio::select! {
                r = st.rx.recv() => match r {
                    Ok(change) => Next::Change(change.changed),
                    // Missed events: send the current states instead, which
                    // tells the client everything it needs to resync.
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let masked = st.server.quirks_snapshot().masked_email_without_state;
                        Next::Change(st.server.with_store(|s| s.push_states(masked)))
                    }
                    Err(broadcast::error::RecvError::Closed) => Next::End,
                },
                _ = tick(&mut st.ping) => Next::Ping,
                _ = stopped(&mut st.shutdown) => Next::End,
            };
            match next {
                Next::Change(changed) => {
                    let changed = st.filter(changed);
                    if changed.is_empty() {
                        continue;
                    }
                    let account = st.server.inner.account_id.clone();
                    st.enqueue(state_frame(st.shape, &account, &changed));
                    if st.params.close_after_state {
                        st.finished = true;
                    }
                }
                Next::Ping => {
                    let interval = st.params.ping_seconds;
                    st.enqueue(ping_frame(interval));
                }
                Next::End => st.finished = true,
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn params_parse() {
        assert_eq!(
            parse_params(&[]).unwrap(),
            PushParams {
                types: None,
                close_after_state: false,
                ping_seconds: 0
            }
        );
        let p = parse_params(&q(&[
            ("types", "Email,Mailbox"),
            ("closeafter", "state"),
            ("ping", "30"),
        ]))
        .unwrap();
        assert_eq!(p.types.unwrap().len(), 2);
        assert!(p.close_after_state);
        assert_eq!(p.ping_seconds, 30);
        for bad in [
            q(&[("types", "{types}")]),
            q(&[("closeafter", "maybe")]),
            q(&[("ping", "-1")]),
            q(&[("ping", "soon")]),
            q(&[("types", ",")]),
            q(&[("types", "Em ail")]),
        ] {
            assert!(parse_params(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn frames() {
        let mut changed = BTreeMap::new();
        changed.insert("Email".to_owned(), "7".to_owned());
        let f = state_frame(PushShape::Rfc, "u1", &changed);
        assert_eq!(
            std::str::from_utf8(&f).unwrap(),
            "event: state\ndata: {\"@type\":\"StateChange\",\"changed\":{\"u1\":{\"Email\":\"7\"}}}\n\n"
        );
        let f = state_frame(PushShape::FastmailConnect, "u1", &changed);
        assert!(
            std::str::from_utf8(&f)
                .unwrap()
                .contains("\"type\":\"connect\"")
        );
        assert_eq!(
            &ping_frame(5)[..],
            b"event: ping\ndata: {\"interval\":5}\n\n"
        );
    }
}
