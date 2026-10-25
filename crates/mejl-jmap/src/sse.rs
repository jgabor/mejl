//! An incremental `text/event-stream` parser (WHATWG HTML, "Server-sent
//! events", section 9.2.6).
//!
//! It accepts input split at any byte, CRLF, LF and CR line endings, a
//! leading BOM, comments, multi-line data and the `id` and `retry` fields.
//! Invalid UTF-8 is replaced, not fatal. Lines and events are size-capped so
//! a hostile server cannot grow memory without bound.

use std::time::Duration;

/// One dispatched event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event` field, or `message` when absent.
    pub event: String,
    /// The `data` lines joined with `\n`.
    pub data: String,
    /// The last event id at dispatch time, if any was set.
    pub id: Option<String>,
}

/// The stream broke the parser's size limit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("event stream line or event exceeds {limit} bytes")]
pub struct SseTooLarge {
    pub limit: usize,
}

/// Incremental parser state.
#[derive(Debug)]
pub struct SseParser {
    max_size: usize,
    line: Vec<u8>,
    // A CR ended the last line; a LF right after it belongs to that line end,
    // even if it arrives in the next chunk.
    after_cr: bool,
    at_start: bool,
    data: String,
    event: String,
    last_id: Option<String>,
    retry: Option<Duration>,
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new(Self::DEFAULT_MAX_SIZE)
    }
}

impl SseParser {
    /// The default cap on one line and on one event's data: 1 MiB.
    pub const DEFAULT_MAX_SIZE: usize = 1 << 20;

    /// A parser that rejects lines or events larger than `max_size` bytes.
    pub fn new(max_size: usize) -> Self {
        Self {
            max_size,
            line: Vec::new(),
            after_cr: false,
            at_start: true,
            data: String::new(),
            event: String::new(),
            last_id: None,
            retry: None,
        }
    }

    /// The last event id the stream set, for a `Last-Event-ID` reconnect.
    pub fn last_event_id(&self) -> Option<&str> {
        self.last_id.as_deref().filter(|id| !id.is_empty())
    }

    /// The reconnection time the stream asked for, if any.
    pub fn retry(&self) -> Option<Duration> {
        self.retry
    }

    /// Feed bytes and append any completed events to `out`.
    pub fn feed(&mut self, mut chunk: &[u8], out: &mut Vec<SseEvent>) -> Result<(), SseTooLarge> {
        while !chunk.is_empty() {
            if self.after_cr {
                self.after_cr = false;
                if chunk[0] == b'\n' {
                    chunk = &chunk[1..];
                    continue;
                }
            }
            match chunk.iter().position(|&b| b == b'\n' || b == b'\r') {
                Some(end) => {
                    self.push_line_bytes(&chunk[..end])?;
                    self.after_cr = chunk[end] == b'\r';
                    chunk = &chunk[end + 1..];
                    let line = std::mem::take(&mut self.line);
                    self.process_line(&line, out)?;
                    self.line = line;
                    self.line.clear();
                }
                None => {
                    self.push_line_bytes(chunk)?;
                    chunk = &[];
                }
            }
        }
        Ok(())
    }

    /// End of stream. A partial final event is discarded, as the spec says.
    pub fn finish(&mut self) {
        self.line.clear();
        self.data.clear();
        self.event.clear();
        self.after_cr = false;
    }

    fn push_line_bytes(&mut self, bytes: &[u8]) -> Result<(), SseTooLarge> {
        if self.line.len() + bytes.len() > self.max_size {
            return Err(SseTooLarge {
                limit: self.max_size,
            });
        }
        self.line.extend_from_slice(bytes);
        Ok(())
    }

    fn process_line(&mut self, line: &[u8], out: &mut Vec<SseEvent>) -> Result<(), SseTooLarge> {
        let mut line = line;
        if self.at_start {
            self.at_start = false;
            line = line.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(line);
        }
        if line.is_empty() {
            self.dispatch(out);
            return Ok(());
        }
        if line[0] == b':' {
            return Ok(());
        }
        let (field, value) = match line.iter().position(|&b| b == b':') {
            Some(colon) => {
                let value = &line[colon + 1..];
                (&line[..colon], value.strip_prefix(b" ").unwrap_or(value))
            }
            None => (line, &[][..]),
        };
        match field {
            b"event" => self.event = String::from_utf8_lossy(value).into_owned(),
            b"data" => {
                if self.data.len() + value.len() + 1 > self.max_size {
                    return Err(SseTooLarge {
                        limit: self.max_size,
                    });
                }
                self.data.push_str(&String::from_utf8_lossy(value));
                self.data.push('\n');
            }
            b"id" => {
                if !value.contains(&0) {
                    self.last_id = Some(String::from_utf8_lossy(value).into_owned());
                }
            }
            b"retry" if !value.is_empty() && value.iter().all(u8::is_ascii_digit) => {
                // Too many digits for u64 fails the parse and is ignored.
                if let Some(ms) = std::str::from_utf8(value).ok().and_then(|s| s.parse().ok()) {
                    self.retry = Some(Duration::from_millis(ms));
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn dispatch(&mut self, out: &mut Vec<SseEvent>) {
        let event = std::mem::take(&mut self.event);
        if self.data.is_empty() {
            return;
        }
        let mut data = std::mem::take(&mut self.data);
        if data.ends_with('\n') {
            data.pop();
        }
        out.push(SseEvent {
            event: if event.is_empty() {
                "message".to_owned()
            } else {
                event
            },
            data,
            id: self.last_id.clone().filter(|id| !id.is_empty()),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_chunks(chunks: &[&[u8]]) -> (Vec<SseEvent>, SseParser) {
        let mut parser = SseParser::default();
        let mut out = Vec::new();
        for chunk in chunks {
            parser.feed(chunk, &mut out).unwrap();
        }
        parser.finish();
        (out, parser)
    }

    fn parse(input: &[u8]) -> Vec<SseEvent> {
        parse_chunks(&[input]).0
    }

    fn ev(event: &str, data: &str, id: Option<&str>) -> SseEvent {
        SseEvent {
            event: event.into(),
            data: data.into(),
            id: id.map(Into::into),
        }
    }

    /// Every way to split `input` into two or three chunks, plus one byte
    /// at a time, must give the same events as one chunk.
    fn assert_split_invariant(input: &[u8]) {
        let whole = parse(input);
        let bytes: Vec<&[u8]> = input.chunks(1).collect();
        assert_eq!(parse_chunks(&bytes).0, whole, "byte at a time");
        for i in 0..=input.len() {
            let (a, b) = input.split_at(i);
            assert_eq!(parse_chunks(&[a, b]).0, whole, "split at {i}");
            if input.len() <= 160 {
                for j in i..=input.len() {
                    let (b1, c) = input[i..].split_at(j - i);
                    assert_eq!(parse_chunks(&[a, b1, c]).0, whole, "split at {i},{j}");
                }
            }
        }
    }

    const MIXED: &[u8] = b"\xEF\xBB\xBF: hello comment\r\n\
event: state\r\n\
data: {\"@type\":\"StateChange\",\r\n\
data:\"changed\":{}}\r\n\
id: 42\r\n\
\r\n\
event: ping\rdata: {\"interval\":30}\r\r\
retry: 5000\n\
retry: soon\n\
data: caf\xC3\xA9 \xF0\x9F\x93\xAC\n\
\n\
event: no-data\n\
\n\
data\n\
\n\
data: tail without blank line";

    #[test]
    fn mixed_stream() {
        let (events, parser) = parse_chunks(&[MIXED]);
        assert_eq!(
            events,
            vec![
                ev(
                    "state",
                    "{\"@type\":\"StateChange\",\n\"changed\":{}}",
                    Some("42")
                ),
                ev("ping", "{\"interval\":30}", Some("42")),
                ev("message", "caf\u{e9} \u{1f4ec}", Some("42")),
                ev("message", "", Some("42")),
            ]
        );
        assert_eq!(parser.retry(), Some(Duration::from_millis(5000)));
        assert_eq!(parser.last_event_id(), Some("42"));
    }

    #[test]
    fn mixed_stream_every_split_point() {
        assert_split_invariant(MIXED);
    }

    #[test]
    fn crlf_split_between_chunks_is_one_line_end() {
        let (events, _) = parse_chunks(&[b"data: a\r", b"\ndata: b\r", b"\n\r", b"\n"]);
        assert_eq!(events, vec![ev("message", "a\nb", None)]);
    }

    #[test]
    fn cr_only_line_endings() {
        assert_eq!(
            parse(b"data: x\r\rdata: y\r\r"),
            vec![ev("message", "x", None), ev("message", "y", None)]
        );
    }

    #[test]
    fn exactly_one_leading_space_is_stripped() {
        assert_eq!(parse(b"data:x\n\n"), vec![ev("message", "x", None)]);
        assert_eq!(parse(b"data:  x\n\n"), vec![ev("message", " x", None)]);
        assert_eq!(
            parse(b"event:  odd\ndata: x\n\n"),
            vec![ev(" odd", "x", None)]
        );
    }

    #[test]
    fn block_without_data_does_not_dispatch_and_resets_event() {
        assert_eq!(
            parse(b"event: state\nid: 1\n\ndata: x\n\n"),
            vec![ev("message", "x", Some("1"))]
        );
    }

    #[test]
    fn bom_only_at_start() {
        assert_eq!(
            parse(b"\xEF\xBB\xBFdata: a\n\n"),
            vec![ev("message", "a", None)]
        );
        // A second BOM is part of the field name, so the line is ignored.
        assert_eq!(parse(b"\n\xEF\xBB\xBFdata: a\n\n"), vec![]);
    }

    #[test]
    fn id_rules() {
        // An id with NUL is ignored; an empty id clears it.
        assert_eq!(
            parse(b"id: 7\ndata: a\n\nid: x\0y\ndata: b\n\nid\ndata: c\n\n"),
            vec![
                ev("message", "a", Some("7")),
                ev("message", "b", Some("7")),
                ev("message", "c", None)
            ]
        );
    }

    #[test]
    fn retry_rules() {
        let (_, p) = parse_chunks(&[b"retry: 10x\nretry:\nretry: -5\n"]);
        assert_eq!(p.retry(), None);
        let (_, p) = parse_chunks(&[b"retry: 99999999999999999999999999\n"]);
        assert_eq!(p.retry(), None);
        let (_, p) = parse_chunks(&[b"retry:250\n"]);
        assert_eq!(p.retry(), Some(Duration::from_millis(250)));
    }

    #[test]
    fn invalid_utf8_is_replaced() {
        assert_eq!(
            parse(b"data: a\xFFb\n\n"),
            vec![ev("message", "a\u{fffd}b", None)]
        );
    }

    #[test]
    fn unknown_fields_and_comments_are_ignored() {
        assert_eq!(
            parse(b":comment\nfoo: bar\nDATA: no\ndata: yes\n\n"),
            vec![ev("message", "yes", None)]
        );
    }

    #[test]
    fn partial_event_at_eof_is_dropped() {
        let (events, _) = parse_chunks(&[b"data: a\n\ndata: b\n"]);
        assert_eq!(events, vec![ev("message", "a", None)]);
    }

    #[test]
    fn line_limit() {
        let mut p = SseParser::new(16);
        let mut out = Vec::new();
        p.feed(b"data: 0123456789", &mut out).unwrap();
        assert!(p.feed(b"0123456789", &mut out).is_err());
    }

    #[test]
    fn event_limit_across_lines() {
        let mut p = SseParser::new(16);
        let mut out = Vec::new();
        p.feed(b"data: 01234\ndata: 56789\n", &mut out).unwrap();
        assert!(p.feed(b"data: abcde\n", &mut out).is_err());
    }

    #[test]
    fn garbage_never_panics() {
        // A small deterministic generator, so the test needs no extra crate.
        let mut seed: u64 = 0x5eed;
        let alphabet = b"data:event:id:retry: \r\n\xEF\xBB\xBF\xFF\x00x{}";
        for _ in 0..200 {
            let mut input = Vec::new();
            for _ in 0..64 {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let index = usize::try_from(seed >> 33).unwrap_or(0) % alphabet.len();
                input.push(alphabet[index]);
            }
            assert_split_invariant(&input);
        }
    }
}
