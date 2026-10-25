# Rust crates for mejl

## Summary

This is the team's reference for Rust crates that mejl could use. Every version, release date and license below comes from the live crates.io API, fetched on 2026-10-23. Repo health signals come from the GitHub API on the same day. A probe crate with the proposed starter set was built and run on this machine, so the starter set is known to resolve, compile and work together.

Main findings and what they mean for mejl:

- **TUI:** use [ratatui](https://crates.io/crates/ratatui) 0.30.2 with [crossterm](https://crates.io/crates/crossterm) 0.29.0. Crossterm covers the kitty keyboard protocol, mouse capture, bracketed paste, focus events, synchronized updates and OSC 52 clipboard (verified: [docs.rs crossterm event](https://docs.rs/crossterm/0.29.0/crossterm/event/index.html), [docs.rs crossterm terminal](https://docs.rs/crossterm/0.29.0/crossterm/terminal/index.html), probe build). For the composer, this research picked [ratatui-textarea](https://crates.io/crates/ratatui-textarea), not `tui-textarea`. As built, mejl has its own body editor, because ratatui-textarea re-wraps the whole buffer on every edit; ratatui-textarea is now a dev-dependency used as a test reference. `tui-textarea` 0.7.0 still needs ratatui ^0.29 and does not work with 0.30 (verified: crates.io dependencies API).
- **JMAP:** write a thin custom JMAP client on `reqwest` and `serde`. Stalwart's [jmap-client](https://crates.io/crates/jmap-client) 0.4.3 is well maintained and has JMAP Sieve support, but its method set is a closed enum. It has no raw-request escape hatch and no MaskedEmail support (verified: source at `src/lib.rs:257`, grep of the repo). Use it as a reference and test oracle. Its MIT/Apache license lets us copy code with attribution.
- **Sieve:** [sieve-rs](https://crates.io/crates/sieve-rs) is AGPL-3.0-only in every release (verified: crates.io versions API). It is off the table unless mejl itself ships under the AGPL. Do Sieve editing as text plus server-side validation, and keep a local parser optional.
- **Mail formats:** [mail-parser](https://crates.io/crates/mail-parser) and [mail-builder](https://crates.io/crates/mail-builder) (Stalwart, MIT/Apache) are the best picks. With JMAP, the server already parses bodies, so they are only needed for raw source, attached `message/rfc822` parts and `Email/import` (inferred).
- **HTML email:** [html2text](https://crates.io/crates/html2text) 0.17.4 with its rich decorator gives annotated lines that map onto ratatui spans, with link footnotes (verified: [docs.rs Config](https://docs.rs/html2text/0.17.4/html2text/config/struct.Config.html)). Pre-clean with [lol_html](https://crates.io/crates/lol_html) if needed. Hand off to the browser via [ammonia](https://crates.io/crates/ammonia) plus [open](https://crates.io/crates/open).
- **Push:** JMAP EventSource over plain `reqwest` streaming plus [sse-stream](https://crates.io/crates/sse-stream) 0.3.0 compiled in the probe. `reqwest-eventsource` is stale and pins reqwest 0.12.
- **Cache and search:** [rusqlite](https://crates.io/crates/rusqlite) 0.40.2 with the `bundled` feature includes FTS5 (verified: `libsqlite3-sys/build.rs` sets `-DSQLITE_ENABLE_FTS5`, and the probe created an FTS5 table at runtime). For 100k to 1M messages, SQLite plus FTS5 is the low-risk choice. Keep [tantivy](https://crates.io/crates/tantivy) as a later upgrade if FTS5 ranking or speed falls short.
- **Dates:** pick [jiff](https://crates.io/crates/jiff). Note that chrono stays in the tree if we ever add `jmap-client` or `oauth2`.
- **Build cost (measured):** the probe was 281 crates (284 for the final starter set). A clean release build took 48.8 s on 16 cores, including downloads. An incremental release rebuild took 7.5 s, and an incremental debug rebuild 0.5 s. The stripped binary is 14 MB, or 8.8 MB with `opt-level = "s"` and `panic = "abort"`.
- **License traps:** `sieve-rs` (AGPL-3.0), `html2md` (GPL-3.0+), `osc52` (GPL-3.0), `meli`/`melib` (EUPL-1.2 or GPL-3.0), `mujmap` (GPL-3.0). `nucleo` is MPL-2.0, which is file-level copyleft and normally fine for a dependency.

## How this was gathered

- Crate metadata came from `https://crates.io/api/v1/crates/<name>`: `max_stable_version`, the creation date of that version, and the license and `rust_version` of that version. Dependency requirements came from `.../<name>/<version>/dependencies`. All values in the tables are (verified: crates.io API, 2026-10-23) unless a row says otherwise.
- Repo signals (stars, open issues plus PRs, last push, archived flag) came from `gh api repos/<owner>/<repo>` (verified: GitHub API, 2026-10-23). "Open issues" in GitHub's API includes open pull requests.
- Source checks: `stalwartlabs/jmap-client`, `MarkAtwood/crate-jmap`, `pimalaya/io-jmap` and `stalwartlabs/jmap-tools` were shallow-cloned and grepped.
- A probe crate (`edition = "2024"`, rustc 1.99.1, 16 cores, `lto = "thin"`, `codegen-units = 1`, `strip = true`) used every starter crate in real code, ran successfully, and passed an `insta` snapshot test of a ratatui `TestBackend`. Two variant builds measured the cost of `keyring` and `jmap-client`.
- "Verdict" key: **Pick** = recommended for the starter set. **Later** = good, add when the feature arrives. **Alt** = viable alternative. **Ref** = study, do not depend on. **Avoid** = do not use, reason given.

## TUI

### Core and backends

| Crate | Version | Released | License | Verdict | Why / why not |
|---|---|---|---|---|---|
| [ratatui](https://crates.io/crates/ratatui) | 0.30.2 | 2026-06-19 | MIT | **Pick** | The de facto Rust TUI library. Immediate-mode rendering with cell diffing. 23.1k stars, pushed 2026-10-21. 0.30 split it into `ratatui-core`, `ratatui-widgets` and backend crates, and added `ratatui::run()` (verified: [0.30 highlights](https://ratatui.rs/highlights/v030/)). Declared MSRV is 1.86.0 for 0.30.0 and 1.88.0 for 0.30.1 and 0.30.2 (verified: crates.io versions API). The highlights page's "1.86" refers to 0.30.0. |
| [ratatui-core](https://crates.io/crates/ratatui-core) | 0.1.2 | 2026-06-19 | MIT | Pick (transitive) | Stable core types for widget authors. If mejl splits into crates, widget crates should depend on this only (verified: 0.30 highlights). |
| [ratatui-widgets](https://crates.io/crates/ratatui-widgets) | 0.3.2 | 2026-06-19 | MIT | Pick (transitive) | Built-in widgets (List, Table, Paragraph, Tabs, Scrollbar). Comes through `ratatui`. |
| [ratatui-crossterm](https://crates.io/crates/ratatui-crossterm) | 0.1.2 | 2026-06-19 | MIT | Pick (transitive) | Crossterm backend. Supports crossterm 0.28 and 0.29 behind features. |
| [ratatui-macros](https://crates.io/crates/ratatui-macros) | 0.7.2 | 2026-06-19 | MIT | Pick (default feature) | `line!`, `span!`, `constraints!` helpers. On by default in `ratatui`. |
| [crossterm](https://crates.io/crates/crossterm) | 0.29.0 | 2025-04-05 | MIT | **Pick** | Cross-platform terminal I/O. Kitty keyboard protocol via `PushKeyboardEnhancementFlags`, `supports_keyboard_enhancement()`, `EnableMouseCapture`, `EnableBracketedPaste`, `EnableFocusChange`, `BeginSynchronizedUpdate`/`EndSynchronizedUpdate`, async `EventStream` (verified: docs.rs). An `osc52` feature gives `clipboard::CopyToClipboard` (verified: probe build). The last release is 18 months old, but the repo is active (pushed 2026-10-20, 253 open issues plus PRs). |
| [termion](https://crates.io/crates/termion) | 4.0.6 | 2025-11-21 | MIT | Avoid | Unix-only and light (unverified for current platform list). No kitty keyboard support found (unverified). meli uses termion 1.5 (verified: crates.io deps). No reason to pick it over crossterm. |
| [termwiz](https://crates.io/crates/termwiz) | 0.23.3 | 2025-03-20 | MIT | Avoid | WezTerm's terminal library, released from the wezterm monorepo (1,929 open issues plus PRs). The ratatui-image README says its termwiz backend does not work (verified: [ratatui-image README](https://github.com/ratatui/ratatui-image)). |
| [termina](https://crates.io/crates/termina) | 0.4.0 | 2026-08-31 | MIT OR MPL-2.0 | Later (watch) | Helix's lower-level VT library, designed to absorb new VT extensions such as the kitty keyboard protocol (verified: [termina README](https://github.com/helix-editor/termina)). [ratatui-termina](https://crates.io/crates/ratatui-termina) 0.1.0 depends on termina ^0.3 while termina is at 0.4.0 (verified: crates.io deps). So the ratatui backend lags behind. |
| [cursive](https://crates.io/crates/cursive) | 0.21.1 | 2024-08-03 | MIT | Avoid | Retained-mode, callback-style views. The repo is active (pushed 2026-09-25), but the last release is 2024. The view-tree model fights the "instant, custom list rendering" goal (inferred). |
| [r3bl_tui](https://crates.io/crates/r3bl_tui) | 0.7.8 | 2026-01-23 | Apache-2.0 | Avoid | A full framework with a very heavy dependency list (reqwest 0.12, syntect, portable-pty, mimalloc, kv, notify-rust and more) (verified: crates.io deps). Too opinionated for mejl. |
| [ftui-core](https://crates.io/crates/ftui-core) (FrankenTUI) | 0.9.0 | 2026-10-03 | LicenseRef-MIT-OpenAI-Anthropic-Rider | Avoid | New, with a custom non-SPDX license. Needs a legal read before any use. |

### Ratatui ecosystem widgets

| Crate | Version | Released | License | Verdict | Why / why not |
|---|---|---|---|---|---|
| [ratatui-textarea](https://crates.io/crates/ratatui-textarea) | 0.9.3 | 2026-10-05 | MIT | Test reference | Was the pick for the composer (superseded by mejl's own editor, see above). Multi-line editor and the Sieve editor: undo/redo, selection, regex search, soft wrap, mouse scrolling, yank (verified: [README](https://github.com/ratatui/ratatui-textarea)). The README says it is "a Ratatui fork of tui-textarea and maintained independently". Depends on `ratatui-core` ^0.1.1, so it works with 0.30 (verified: crates.io deps, probe build). 97 stars, pushed 2026-10-08. |
| [tui-textarea](https://crates.io/crates/tui-textarea) | 0.7.0 | 2024-10-22 | MIT | Avoid | Needs `ratatui ^0.29.0` and does not work with 0.30 (verified: crates.io deps). Last push to the repo was 2024-12-01. |
| [tui-textarea-2](https://crates.io/crates/tui-textarea-2) | 0.13.2 | 2026-08-23 | MIT | Alt | Another community fork, ratatui 0.30 compatible. 11 stars. Prefer the ratatui-org fork. |
| [edtui](https://crates.io/crates/edtui) | 0.11.7 | 2026-08-16 | MIT | Alt | A vim-style editor widget with optional syntect highlighting. Consider it if users want modal editing in the composer without spawning `$EDITOR`. |
| [tui-input](https://crates.io/crates/tui-input) | 0.15.5 | 2026-09-26 | MIT | **Pick** | Single-line input for the command line, search box and picker prompt. Supports ratatui ^0.30.2 and crossterm ^0.29 (verified: crates.io deps). Small and maintained (2 open issues). |
| [tui-tree-widget](https://crates.io/crates/tui-tree-widget) | 0.24.1 | 2026-08-09 | MIT | **Pick** | Tree view for the mailbox sidebar (nested Fastmail folders) and possibly threads. Built on `ratatui-core`/`ratatui-widgets` (verified: crates.io deps). |
| [tui-scrollview](https://crates.io/crates/tui-scrollview) | 0.6.8 | 2026-09-24 | MIT OR Apache-2.0 | Later | Scrollable viewport for content bigger than the screen, for example settings pages. The message pane is better rendered by hand from pre-wrapped lines (inferred). Part of [tui-widgets](https://crates.io/crates/tui-widgets) 0.7.12. |
| [ratatui-image](https://crates.io/crates/ratatui-image) | 11.1.0 (12.0.0-rc.3 out) | 2026-09-17 | MIT | Later | Inline images through the kitty, sixel and iTerm2 protocols, with a halfblocks fallback. `Picker::from_query_stdio()` detects the protocol and font size (verified: README). The default features include `chafa-dyn`, which needs pkg-config and system libchafa (verified: crates.io features). Use `default-features = false, features = ["crossterm", "image-defaults"]`. |
| [tachyonfx](https://crates.io/crates/tachyonfx) | 0.25.2 | 2026-09-06 | MIT | Avoid (for now) | Shader-like effects and transitions. Animation conflicts with the "instant" goal. Maybe a subtle fade on toasts later. |
| [throbber-widgets-tui](https://crates.io/crates/throbber-widgets-tui) | 0.11.1 | 2026-06-19 | Zlib | Later | Spinner for sync or send-in-progress states. Tiny. A hand-rolled spinner is about 20 lines, so this one is optional. |
| [tui-logger](https://crates.io/crates/tui-logger) | 0.18.3 | 2026-07-04 | MIT | Later | In-app log pane with a `tracing` integration feature. Useful for a debug screen. |
| [ansi-to-tui](https://crates.io/crates/ansi-to-tui) | 8.0.1 | 2026-01-10 | MIT | Later | Converts ANSI-colored text to ratatui `Text`. Needed if mejl pipes HTML through an external renderer (w3m, lynx) or a pager. |
| [tui-markdown](https://crates.io/crates/tui-markdown) | 0.3.12 | 2026-10-19 | MIT OR Apache-2.0 | Later | Markdown to ratatui `Text` (pulldown-cmark). For a markdown compose preview, or for an HTML to markdown to TUI pipeline (see HTML rendering). |
| [crokey](https://crates.io/crates/crokey) | 1.5.1 | 2026-10-17 | MIT | Later | Parses and prints key combinations such as `ctrl-shift-k` for user keymaps in config. Uses crossterm ^0.29. |
| [rat-text](https://crates.io/crates/rat-text) | 3.1.0 | 2026-03-08 | MIT/Apache-2.0 | Avoid | Rich input widgets, but it pulls a whole `rat-*` focus and event framework (verified: crates.io deps). |

### Keyboard and mouse notes

- **Kitty keyboard protocol:** crossterm's `PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES | REPORT_EVENT_TYPES | ...)` turns it on. `crossterm::terminal::supports_keyboard_enhancement()` detects support (verified: docs.rs; individual flag names are from memory, so (unverified)). With it, mejl can tell `Ctrl-i` from `Tab` and `Esc` from `Alt` prefixes, and can see key release events. Pop the flags on exit and in the panic hook.
- **Mouse:** `EnableMouseCapture` gives click, drag, scroll and move events (verified: docs.rs). Mouse capture blocks the terminal's native text selection. Most terminals let Shift bypass capture (inferred, varies by terminal). A config toggle for mouse capture is worth having.
- **Paste:** bracketed paste (`Event::Paste`) is on by default in crossterm's features (verified: crates.io features `default = bracketed-paste, events, windows, derive-more`). Use it so pasting into the composer does not trigger keybindings.
- **Flicker-free redraw:** wrap each frame in `BeginSynchronizedUpdate`/`EndSynchronizedUpdate` (verified: these exist as crossterm commands). Ratatui's diffing already keeps writes small.
- **Async input:** crossterm's `event-stream` feature gives `EventStream` for a `tokio::select!` loop over input, JMAP push and timers (verified: probe build with the feature).

## JMAP

### Crate inventory

| Crate | Version | Released | License | Verdict | Why / why not |
|---|---|---|---|---|---|
| [jmap-client](https://crates.io/crates/jmap-client) (Stalwart) | 0.4.3 | 2026-09-28 | Apache-2.0 OR MIT | **Ref** (Alt) | Full RFC 8620/8621/8887 client plus JMAP Sieve (draft-14), EventSource and WebSocket streams. Features: `async`, `websockets`, `blocking`, `aws_lc_rs` (default), `ring`, `debug` (verified: Cargo.toml). Steady releases in 2025 and 2026; 124 stars, 13 open issues plus PRs. No MaskedEmail. Closed method enum. Details below. |
| [jmap-tools](https://crates.io/crates/jmap-tools) (Stalwart) | 0.1.8 | 2026-08-20 | Apache-2.0 OR MIT | Avoid | Generic JMAP object parsing plus JSON Pointer (RFC 6901) query and patch (verified: README). Aimed at servers. Small (9 stars). |
| [jmap-types](https://crates.io/crates/jmap-types), [jmap-base-client](https://crates.io/crates/jmap-base-client), [jmap-mail-types](https://crates.io/crates/jmap-mail-types), [jmap-mail-client](https://crates.io/crates/jmap-mail-client), [jmap-mime](https://crates.io/crates/jmap-mime) | 0.3.0 | 2026-10-20 | MIT OR Apache-2.0 | Avoid (watch) | A large new workspace ([MarkAtwood/crate-jmap](https://github.com/MarkAtwood/crate-jmap)) with types, client and server crates for Mail, Contacts, Calendars, Sieve (RFC 9661, server side), Chat and more. First published 2026-05-02; 2 stars; single author; dozens of commits a day; ships `AGENTS.md` and `CLAUDE.md` (verified: git log, repo listing). `jmap-base-client` pins reqwest ^0.12, which would sit next to our 0.13 (verified: crates.io deps). Too young to depend on. `jmap-types` (serde, serde_json, thiserror only) may be worth a second look in 2027. |
| [io-jmap](https://crates.io/crates/io-jmap) (Pimalaya) | 0.6.0 | 2026-10-09 | MIT OR Apache-2.0 | Ref | Sans-IO JMAP: no_std state machines, a blocking "light client" and a full client with TLS (verified: README). Covers core, mail, contacts, calendars and EventSource. No Sieve or MaskedEmail found by grep. The coroutine design is clever, but driving it from tokio means writing our own I/O loop (inferred). Used by himalaya 2.2.1 (verified: crates.io deps). |
| [jmap](https://crates.io/crates/jmap) | 0.0.5 | 2016-02-22 | MIT | Avoid | An abandoned 2016 crate by Rob N (robn/jmap-rs). |
| [mujmap](https://crates.io/crates/mujmap) | 0.2.0 | 2022-06-07 | GPL-3.0 | Ref | JMAP to notmuch sync tool. It hand-rolls its JMAP layer on `ureq` and `serde` (verified: crates.io deps). That is a precedent for a thin client. GPL, and stale (pushed 2024-07-03). |

### What jmap-client can and cannot do

Verified by reading the source at tag 0.4.3 (commit "Replace base64 with encodify", 2026-09-28):

- **Method set is closed.** `pub enum Method` in `src/lib.rs:257` lists the known method names as serde-renamed variants. Responses are matched per type with `_ => Err("Response type mismatch")` arms in `src/core/response.rs`. Adding `MaskedEmail/get` or `MaskedEmail/set` means forking the crate (verified).
- **No raw-request escape hatch.** `Request::send()` returns `Response<TaggedMethodResponse>` (`src/core/request.rs:505`). There is no public way to send an arbitrary method call through the same client (verified by grep; a deeper API read could find a path, so treat as high-confidence, not certain).
- **No MaskedEmail.** `grep -ri masked src` returns nothing (verified). The only capability URIs in the source are `urn:ietf:params:jmap:*` ones (verified).
- **Unknown session capabilities parse fine.** `Capabilities` is `#[serde(untagged)]` with an `Other(serde_json::Value)` variant (`src/core/session.rs:66-73`). So Fastmail's `https://www.fastmail.com/dev/maskedemail` capability will not break session parsing (inferred from code, not run against Fastmail).
- **Sieve follows draft-14, not RFC 9661.** The README cites "DRAFT-SIEVE-14". The final spec is [RFC 9661](https://www.rfc-editor.org/rfc/rfc9661.html) (September 2024), with capability `urn:ietf:params:jmap:sieve` and methods `SieveScript/get`, `/set`, `/query`, `/validate` (verified: rfc-editor). Wire differences between draft-14 and the RFC are (unverified). The probe compiled a call to `sieve_script_query` (verified).
- **Auth is Basic or Bearer only.** `enum Credentials { Basic(String), Bearer(String) }` (`src/client.rs:45`). Fastmail's OAuth refresh tokens rotate on every refresh, and reusing an old one revokes the grant (verified: [fastmail.com/dev](https://www.fastmail.com/dev/)). Token refresh would have to wrap the client from outside.
- **Dependencies:** reqwest ^0.13 (matches ours), chrono, ahash, parking_lot, maybe-async. With `websockets`, it adds tokio-tungstenite ^0.29 while 0.30 is current (verified: crates.io deps). In the probe, adding it took the tree from 281 to 304 crates with no visible binary size change (measured).

### MaskedEmail

Fastmail's extension uses capability `https://www.fastmail.com/dev/maskedemail`. It has two methods, `MaskedEmail/get` and `MaskedEmail/set`, over an object with `id`, `email`, `state` (`pending`, `enabled`, `disabled`, `deleted`), `forDomain`, `description`, `lastMessageAt`, `createdAt`, `createdBy`, `url` and the create-only `emailPrefix` (verified: [fastmail.com/dev](https://www.fastmail.com/dev/)). No Rust crate on crates.io implements it. A search for "masked" found nothing relevant (verified: crates.io search). The only typed implementation found is the TypeScript [jmap-kit](https://lachlanhunt.github.io/jmap-kit/variables/capabilities_maskedemail-capability.MaskedEmailCapability.html). This is about 100 lines of serde types plus two method calls in a thin client (inferred). See `docs/fastmail-masked-email.md` for the full API.

### Recommendation: thin custom client

| | Thin client on reqwest + serde | jmap-client 0.4.3 | Fork of jmap-client |
|---|---|---|---|
| MaskedEmail | Easy: just another method | Not possible without fork | Add variants |
| Batching with back-references (`#ref`) | Full control | Supported through its builder | Supported |
| Only fetch needed properties | Yes, typed per view | Yes | Yes |
| Token refresh (rotating) | Built in | External wrapper | Patchable |
| Dependency control (reqwest, jiff) | Full | chrono, ahash, maybe-async | Same as upstream |
| Upfront work | Types for Session, Mailbox, Thread, Email, EmailSubmission, Identity, SearchSnippet, VacationResponse, MaskedEmail, SieveScript; `/get`, `/changes`, `/query`, `/queryChanges`, `/set` plumbing (inferred: 1.5k to 3k lines) | Near zero | Low, plus ongoing rebase cost |
| Risk | Our own bugs in state handling | Upstream design limits | Divergence |

The recommendation is a thin client (inferred, based on the verified limits above). Mejl's core value is Fastmail specifics (MaskedEmail, rules), and those are exactly what jmap-client cannot reach. The suggested shape:

- An `Invocation` type (`(String, serde_json::Value, String)` on the wire) and a request builder that hands out typed `ResultRef<T>` handles for back-references.
- A `trait Method { const NAME: &str; type Args: Serialize; type Response: DeserializeOwned; const CAPABILITIES: &[&str]; }`. One impl per method keeps it open for vendor extensions.
- Object structs that keep unknown fields with `#[serde(flatten)] extra: serde_json::Map<..>`, so new Fastmail properties do not break parsing.
- Copy test fixtures and edge cases from jmap-client (Apache/MIT allows it with attribution) and from [fastmail/JMAP-Samples](https://github.com/fastmail/JMAP-Samples) (MIT; 123 stars, pushed 2026-04-06).

## Sieve

| Crate | Version | Released | License | Verdict | Why / why not |
|---|---|---|---|---|---|
| [sieve-rs](https://crates.io/crates/sieve-rs) (Stalwart) | 1.0.2 | 2026-09-22 | AGPL-3.0-only | Avoid (license) | The most complete Rust Sieve implementation: parser, compiler and interpreter, used in the Stalwart server. Every release from 0.1.0 (2022) to 1.0.2 is AGPL-3.0-only (verified: crates.io versions API). Linking it would make the mejl binary AGPL. Only acceptable if mejl chooses the AGPL. |
| [mailrs-sieve-core](https://crates.io/crates/mailrs-sieve-core) | 0.3.0 | 2026-08-04 | Apache-2.0 OR MIT | Alt (unverified quality) | Claims a native RFC 5228 tokenizer, parser and evaluator "built from the spec, no AGPL dependencies" (verified: crates.io description). 211 total downloads; quality not checked. |
| [sieve-form](https://crates.io/crates/sieve-form) | 0.2.1 | 2026-06-18 | MIT | Alt (unverified quality) | RFC 5228 plus 5229 on a uniform form representation. Same author as crate-jmap. 63 downloads. |
| [sieve-kit](https://crates.io/crates/sieve-kit) | 0.2.1 | 2026-09-12 | MIT OR Apache-2.0 | Alt (unverified quality) | A structured rules engine with Sieve semantics. 223 downloads. |
| [io-managesieve](https://crates.io/crates/io-managesieve) (Pimalaya) | 0.2.0 | 2026-09-29 | MIT OR Apache-2.0 | Later (if needed) | ManageSieve (RFC 5804) client. Only relevant if Fastmail exposes ManageSieve, which is (unverified). |

What this means for mejl (inferred):

1. Mejl does not need to execute Sieve. Fastmail runs the scripts. Mejl needs to read, edit, validate and save them, plus give a structured "rule builder" view.
2. Validation can be done server-side if Fastmail offers `SieveScript/validate`. Whether Fastmail's session advertises `urn:ietf:params:jmap:sieve` is (unverified). The OAuth scope list on [fastmail.com/dev](https://www.fastmail.com/dev/) does not mention Sieve (verified).
3. For syntax highlighting and a structured view, a small local tokenizer or parser is enough. Writing one for the subset Fastmail's rules generate is feasible (inferred). One of the permissive crates above can be evaluated first.

## Mail formats

| Crate | Version | Released | License | Verdict | Why / why not |
|---|---|---|---|---|---|
| [mail-parser](https://crates.io/crates/mail-parser) (Stalwart) | 0.11.9 | 2026-09-09 | Apache-2.0 OR MIT | **Pick** | Fast MIME parser with wide charset support (`full_encoding` feature) and optional `serde`/`rkyv`. Dependencies are only `hashify` plus optional extras (verified: crates.io deps). 466 stars, 3 open issues. Needed for "view source", `message/rfc822` attachments and opening `.eml` files. |
| [mail-builder](https://crates.io/crates/mail-builder) (Stalwart) | 1.0.0 | 2026-09-12 | Apache-2.0 OR MIT | Later | MIME builder; reached 1.0 in September 2026. Only needed if mejl uploads raw messages (`Email/import`, or a blob for `Email/set`). JMAP can also build the MIME on the server from `bodyStructure` and `bodyValues` (inferred from RFC 8621). |
| [mail-auth](https://crates.io/crates/mail-auth) (Stalwart) | 0.13.3 | 2026-09-18 | Apache-2.0 OR MIT | Avoid | DKIM, SPF, DMARC and ARC verify and sign. Fastmail already adds `Authentication-Results` headers that mejl can just display (inferred). |
| [mail-send](https://crates.io/crates/mail-send) (Stalwart) | 0.6.2 | 2026-08-18 | Apache-2.0 OR MIT | Avoid | SMTP client. Mejl sends with JMAP `EmailSubmission/set`. |
| [lettre](https://crates.io/crates/lettre) | 0.11.23 | 2026-08-03 | MIT | Avoid | The popular SMTP library (2.3k stars). Not needed with JMAP submission. |
| [mailparse](https://crates.io/crates/mailparse) | 0.18.0 | 2026-10-05 | 0BSD | Alt | A simpler, long-lived MIME parser (226 stars, 1 open issue). mail-parser is more complete (inferred). |
| [email_address](https://crates.io/crates/email_address) | 0.2.9 | 2024-07-31 | MIT | Later | RFC 5322 address validation for the composer's recipient fields. Last release 2024, but the scope is small. |
| [linkify](https://crates.io/crates/linkify) | 0.11.0 | 2026-04-12 | MIT OR Apache-2.0 | **Pick** | Finds URLs and emails in plain-text bodies so they can be highlighted and opened. meli uses it (verified: crates.io deps). |

## HTML to terminal

| Crate | Version | Released | License | Verdict | Why / why not |
|---|---|---|---|---|---|
| [html2text](https://crates.io/crates/html2text) | 0.17.4 | 2026-10-18 | MIT | **Pick** | Renders HTML to width-wrapped text. `config::rich()` with `lines_from_read` gives lines annotated with rich tags. `Config` has `link_footnotes`, `max_wrap_width`, `no_table_borders`, `raw_mode`, `no_link_wrapping` and `render_to_lines` (verified: [docs.rs Config](https://docs.rs/html2text/0.17.4/html2text/config/struct.Config.html)). A `css` feature exists (verified: crates.io features); exactly which document CSS it honors is (unverified). Built on html5ever. Ships an `html2term` example viewer. |
| [ammonia](https://crates.io/crates/ammonia) | 4.2.4 | 2026-10-17 | MIT OR Apache-2.0 | Later | Whitelist HTML sanitizer. Use it when writing HTML to a temp file for "open in browser", so remote content and scripts are stripped (inferred design). |
| [lol_html](https://crates.io/crates/lol_html) | 3.0.2 | 2026-10-23 | BSD-3-Clause | Later | Cloudflare's streaming HTML rewriter with CSS selectors. Good for a fast pre-pass: drop tracking pixels, `<style>` and hidden preheaders, and rewrite `cid:` image links before html2text (inferred). |
| [scraper](https://crates.io/crates/scraper) | 0.29.0 | 2026-10-21 | ISC | Alt | DOM plus CSS selector queries on html5ever. Heavier than lol_html for rewriting. Useful to pull unsubscribe links or the quoted part of a reply. |
| [html5ever](https://crates.io/crates/html5ever) | 0.40.1 | 2026-09-14 | MIT OR Apache-2.0 | Pick (transitive) | Servo's spec-compliant parser. Comes in through html2text. |
| [htmd](https://crates.io/crates/htmd) | 0.5.6 | 2026-10-15 | Apache-2.0 | Alt | HTML to Markdown, turndown.js style. With tui-markdown, this gives an alternative pipeline. |
| [html2md](https://crates.io/crates/html2md) | 0.2.17 | 2026-08-20 | GPL-3.0+ | Avoid (license) | GPL. |
| [fast_html2md](https://crates.io/crates/fast_html2md) | 0.0.63 | 2026-09-08 | MIT | Avoid | Pre-1.0 (0.0.x), built for scraping. |
| [nanohtml2text](https://crates.io/crates/nanohtml2text) | 0.3.0 | 2026-08-27 | MIT | Alt | Zero-dependency, very small. Good for list previews. JMAP already gives `preview` server-side, though. |

How to render HTML email well in a terminal (inferred design, based on the APIs above and on how mutt, aerc and meli handle it):

1. **Prefer `text/plain` when it is real.** JMAP's `textBody` and `htmlBody` lists tell mejl which alternative exists. Some senders' plain parts are junk ("view this email in a browser"). A heuristic (very short plain part, or one that only contains a URL) can switch to HTML.
2. **Pre-clean** with lol_html: remove `display:none` preheaders, 1x1 images, `<style>` and `<script>`. Replace `<img>` with `[image: alt]` placeholders. Collapse layout tables.
3. **Render** with `html2text::config::rich().link_footnotes(true).max_wrap_width(80)`, then `lines_from_read(html, width)`. Map each rich annotation (emphasis, strong, link, code, colour) to a ratatui `Style`. Keep the link list for numbered "open link N" commands with `open`.
4. **Cache** rendered lines per (message id, width), so scrolling and re-entry are instant. Re-render only on resize.
5. **Images:** off by default. Remote images are tracking. Show `cid:` inline images on demand with ratatui-image.
6. **Escape hatches:** "open in browser" (ammonia, then a temp file, then `open`), and an optional external filter command (`w3m -dump -T text/html`, read through ansi-to-tui) for users who prefer it.

## Async, HTTP and push

| Crate | Version | Released | License | Verdict | Why / why not |
|---|---|---|---|---|---|
| [tokio](https://crates.io/crates/tokio) | 1.54.1 | 2026-10-21 | MIT | **Pick** | Runtime for HTTP, push, timers and signals. Use features `rt-multi-thread`, `macros`, `sync`, `time`, `signal` (verified: probe). Keep the UI loop on one task, and send work to others over channels. |
| [reqwest](https://crates.io/crates/reqwest) | 0.13.5 | 2026-09-08 | MIT OR Apache-2.0 | **Pick** | HTTP/2 client with connection reuse, gzip and streaming. In 0.13, `default-tls = rustls`, and the `rustls` feature uses aws-lc-rs plus `rustls-platform-verifier` (verified: crates.io features). aws-lc-rs needs a C toolchain (and cmake on some targets) (unverified for this version). `rustls-no-provider` plus an explicit ring provider is the lighter option (unverified build). |
| [hyper](https://crates.io/crates/hyper) | 1.12.0 | 2026-10-06 | MIT | Pick (transitive) | Under reqwest. No need to use it directly. |
| [ureq](https://crates.io/crates/ureq) | 3.5.0 | 2026-10-21 | MIT OR Apache-2.0 | Alt | Blocking and small. An option for a CLI helper, not for the async TUI. |
| [sse-stream](https://crates.io/crates/sse-stream) | 0.3.0 | 2026-09-18 | MIT OR Apache-2.0 | **Pick** | SSE decoder. `SseByteStream::new(resp.bytes_stream())` works directly on a reqwest stream (verified: probe build). Few deps (bytes, futures-util, http-body). High downloads (16M recent) through the MCP ecosystem. Reconnect, `Last-Event-ID` and `ping` handling are ours to write, which is fine for JMAP's simple push (inferred). |
| [eventsource-stream](https://crates.io/crates/eventsource-stream) | 0.2.3 | 2022-02-17 | MIT OR Apache-2.0 | Alt | Same job, stable but untouched since 2022. |
| [reqwest-eventsource](https://crates.io/crates/reqwest-eventsource) | 0.6.0 | 2024-03-29 | MIT OR Apache-2.0 | Avoid | Pins reqwest ^0.12 (verified: crates.io deps), so it would add a second reqwest. Repo last pushed 2024-06-13. |
| [reqwest-sse](https://crates.io/crates/reqwest-sse) | 0.2.0 | 2026-05-08 | MIT | Alt | Uses reqwest ^0.13. 6 stars; single maintainer. |
| [eventsource-client](https://crates.io/crates/eventsource-client) | 0.18.1 | 2026-10-07 | Apache-2.0 | Avoid | LaunchDarkly's client. Brings its own transport crate, and its MSRV is 1.97 (verified: crates.io). |
| [tokio-tungstenite](https://crates.io/crates/tokio-tungstenite) | 0.30.0 | 2026-07-11 | MIT | Later | WebSocket for JMAP over WebSocket (RFC 8887). Only useful if Fastmail advertises `urn:ietf:params:jmap:websocket`, which is (unverified). EventSource plus HTTP/2 request pipelining is enough to start (inferred). |
| [rustls](https://crates.io/crates/rustls) | 0.23.46 | 2026-10-14 | Apache-2.0 OR ISC OR MIT | Pick (transitive) | TLS. Through reqwest. 0.23.46 fixes RUSTSEC-2026-0346 to 0349, per io-jmap's commit log (verified: io-jmap git log; the advisories themselves are (unverified)). Keep `cargo update` and `cargo audit` in CI. |
| [rustls-platform-verifier](https://crates.io/crates/rustls-platform-verifier) | 0.7.1 | 2026-09-24 | MIT OR Apache-2.0 | Pick (transitive) | Uses the OS trust store. Comes with reqwest's `rustls` feature. |
| [futures](https://crates.io/crates/futures) | 0.3.34 | 2026-08-11 | MIT OR Apache-2.0 | **Pick** | `StreamExt` for push and input streams. |
| [tokio-util](https://crates.io/crates/tokio-util) | 0.7.20 | 2026-10-09 | MIT | Later | `CancellationToken` for clean shutdown of push and sync tasks. |

## Local storage and full-text search

| Crate | Version | Released | License | Verdict | Why / why not |
|---|---|---|---|---|---|
| [rusqlite](https://crates.io/crates/rusqlite) | 0.40.2 | 2026-08-08 | MIT | **Pick** | SQLite bindings. With `bundled`, it builds SQLite with FTS5, JSON and RTREE enabled (verified: `libsqlite3-sys/build.rs` lines 175 to 180; probe created an FTS5 table). 4.4k stars, active. Run it on a dedicated thread and talk to it over channels. |
| [tokio-rusqlite](https://crates.io/crates/tokio-rusqlite) | 0.8.0 | 2026-09-06 | MIT | Alt | Wraps a rusqlite connection in a background thread with an async `call`. Saves about 50 lines; still optional. |
| [sqlx](https://crates.io/crates/sqlx) | 0.9.0 | 2026-05-21 | MIT OR Apache-2.0 | Avoid | Async with compile-time-checked queries, but heavy: macros, many features, MSRV 1.94 (verified). Repo moved to `transact-rs/sqlx` (775 open issues plus PRs). SQLite is synchronous underneath anyway. |
| [redb](https://crates.io/crates/redb) | 4.4.0 | 2026-10-13 | MIT OR Apache-2.0 | Alt | Pure-Rust ACID key-value store (copy-on-write B+trees, LMDB-inspired). 4.9k stars, 2 open issues. We would build every index and the search ourselves. |
| [fjall](https://crates.io/crates/fjall) | 3.1.12 | 2026-10-03 | MIT OR Apache-2.0 | Alt | Pure-Rust LSM-tree key-value store with keyspaces. Good write throughput for big initial syncs. Same "build your own indexes" cost. MSRV 1.90. |
| [heed](https://crates.io/crates/heed) | 0.23.0 | 2026-10-22 | MIT | Alt | Typed LMDB wrapper by Meilisearch. Very fast memory-mapped reads, but it is C (lmdb-master-sys) and needs a fixed map size (inferred from LMDB design). |
| [sled](https://crates.io/crates/sled) | 0.34.7 (1.0.0-alpha.124) | 2021-09-12 | MIT/Apache-2.0 | Avoid | The README says "if reliability is your primary constraint, use SQLite. sled is beta", and mentions a large in-progress rewrite (verified: README). The last stable release is from 2021. |
| [duckdb](https://crates.io/crates/duckdb) | 1.20000.0 | 2026-10-22 | MIT | Avoid | A columnar OLAP engine. Wrong shape for row lookups and frequent small writes, and the bundled C++ build is slow (inferred). |
| [libsql](https://crates.io/crates/libsql) | 0.10.0 | 2026-10-13 | MIT | Avoid | Turso's SQLite fork with replication features we do not need. |
| [turso](https://crates.io/crates/turso) | 0.8.3 (0.9.0-pre.1) | 2026-10-12 | MIT | Avoid (watch) | Rust rewrite of SQLite. Pre-1.0. [limbo](https://crates.io/crates/limbo) 0.0.22 is its old name (last release 2025-06-19). |
| [tantivy](https://crates.io/crates/tantivy) | 0.27.0 | 2026-10-17 | MIT | Later | Lucene-style search with BM25, phrase queries, fuzzy and stemming. 16.3k stars, active. Heavy: about 60 direct deps (verified: crates.io deps). Add if FTS5 is not good enough. |
| [milli](https://crates.io/crates/milli) | 0.1.0 | 2020-07-15 | MIT | Avoid | The crates.io crate is a 2020 stub. Meilisearch's real `milli` has `publish = false` in its Cargo.toml (verified: `meilisearch/crates/milli/Cargo.toml`). Not usable as a dependency. |
| [rkyv](https://crates.io/crates/rkyv) | 0.8.18 | 2026-08-05 | MIT | Later | Zero-copy serialization. mail-parser and jmap-tools have `rkyv` features. Only relevant for a KV-store design. |
| [zstd](https://crates.io/crates/zstd) | 0.14.3 | 2026-10-16 | BSD-3-Clause | Later | Compress cached raw messages or bodies on disk. C library under the hood. |
| [lz4_flex](https://crates.io/crates/lz4_flex) | 0.14.0 | 2026-07-14 | MIT | Alt | Pure-Rust LZ4. Faster and lower ratio than zstd. |
| [moka](https://crates.io/crates/moka) / [lru](https://crates.io/crates/lru) | 0.12.16 / 0.19.0 | 2026-08-09 / 2026-10-18 | (MIT OR Apache-2.0) AND Apache-2.0 / MIT | Later | In-memory caches for rendered messages. `lru` is enough for a single-threaded UI cache (inferred). |

### Comparison for an email cache of 100k to 1M messages

All points here are (inferred) unless marked. Benchmark before committing (see Misc, divan).

| Concern | SQLite + FTS5 (rusqlite) | KV store (redb / fjall / heed) + tantivy |
|---|---|---|
| "Mailbox X sorted by date, page N" | One composite index `(mailbox_id, received_at DESC)`. Fast at 1M rows. | Hand-built secondary index keys. Fast, but every new view needs new code. |
| Thread grouping, unread counts, keyword filters | SQL `GROUP BY` and partial indexes | Hand-maintained counters, with risk of drift |
| Local full-text search | FTS5 with `bm25()`, prefix queries, snippet and highlight functions. External-content tables avoid storing bodies twice. | tantivy: better ranking, faster on large corpora, separate index directory |
| Applying JMAP `/changes` deltas | One transaction per batch. WAL mode lets the UI read during writes. | Same, plus keeping the tantivy index in sync |
| Crash safety | Mature, well understood | redb and heed are ACID; fjall is journaled |
| Debuggability | `sqlite3 cache.db` in a shell | Custom tooling |
| Build cost | One C file (amalgamation), already in the probe | Pure Rust (redb, fjall) or C (heed), plus tantivy's large tree |

Design notes for mejl (inferred):

- Fastmail's server-side `Email/query` with a `text` filter searches the whole account. Local FTS covers instant search over what is cached and offline use. The UI can run both and merge.
- Store metadata (ids, thread id, mailbox ids, keywords, from/to, subject, preview, size, dates) in normal tables. Store body text in FTS5. Store raw blobs, if cached at all, as zstd files or a separate table, so the hot pages stay small.
- Persist the JMAP `state` strings per type, so start-up is "load from SQLite, render, then `/changes`". That is the key to an instant first frame.

## Fuzzy matching and pickers

| Crate | Version | Released | License | Verdict | Why / why not |
|---|---|---|---|---|---|
| [nucleo-matcher](https://crates.io/crates/nucleo-matcher) | 0.3.1 | 2024-02-20 | MPL-2.0 | **Pick** | Helix's matcher: fzf scoring, Unicode grapheme aware, about 6x faster than skim's matcher on its benchmark (verified: [README](https://github.com/helix-editor/nucleo)). The README calls the matcher "finished" with rare breaking changes expected, which explains the old release date. `Pattern::parse(...).match_list(...)` worked in the probe (verified). MPL-2.0 is file-level copyleft: fine as an unmodified dependency (inferred, not legal advice). |
| [nucleo](https://crates.io/crates/nucleo) | 0.5.0 | 2024-04-02 | MPL-2.0 | Later | The high-level, multithreaded (rayon) picker with incremental results. The README says its API may still change (verified). Worth it for very large candidate lists, for example a "jump to any message" picker over 1M subjects. |
| [frizbee](https://crates.io/crates/frizbee) | 0.13.0 | 2026-08-13 | MIT | Alt | SIMD Smith-Waterman, typo-resistant, used by the blink.cmp plugin (unverified). MSRV 1.89. skim uses it as an optional matcher (verified: crates.io deps). |
| [fuzzy-matcher](https://crates.io/crates/fuzzy-matcher) | 0.3.7 | 2020-10-04 | MIT | Avoid | The repo is archived (verified: GitHub API). |
| [skim](https://crates.io/crates/skim) | 5.8.0 | 2026-10-20 | MIT | Avoid | A whole fzf-like application (portable-pty, nix, mimalloc, its own ratatui UI) (verified: crates.io deps). Too heavy to embed. |

## Config, secrets, paths and OAuth

| Crate | Version | Released | License | Verdict | Why / why not |
|---|---|---|---|---|---|
| [serde](https://crates.io/crates/serde) / [serde_json](https://crates.io/crates/serde_json) | 1.0.229 / 1.0.154 | 2026-07-18 / 2026-10-11 | MIT OR Apache-2.0 | **Pick** | JMAP is JSON. serde_json's `RawValue` (feature `raw_value`) can defer parsing big payloads (unverified feature name for this version). |
| [toml](https://crates.io/crates/toml) | 1.1.8+spec-1.1.0 | 2026-10-09 | MIT OR Apache-2.0 | **Pick** | The config format. Now 1.x and follows TOML 1.1 (verified: version string). |
| [toml_edit](https://crates.io/crates/toml_edit) | 0.25.17+spec-1.1.0 | 2026-10-09 | MIT OR Apache-2.0 | Later | Edits config while keeping user comments and layout. Needed if mejl writes settings back (for example "remember this choice"). |
| [figment](https://crates.io/crates/figment) | 0.10.19 | 2024-05-17 | MIT OR Apache-2.0 | Alt | Layered config (file, env, CLI) with good error provenance. No release since 2024, but the repo was pushed 2026-10-13. Plain `toml` plus a few env overrides is enough to start (inferred). |
| [config](https://crates.io/crates/config) | 0.15.27 | 2026-09-30 | MIT OR Apache-2.0 | Alt | Similar to figment. MSRV 1.88. |
| [clap](https://crates.io/crates/clap) | 4.6.7 | 2026-09-14 | MIT OR Apache-2.0 | Later | CLI flags (`--account`, `--config`, subcommands like `mejl masked new`). |
| [keyring](https://crates.io/crates/keyring) | 4.2.0 | 2026-08-29 | MIT OR Apache-2.0 | **Pick** | Version 4 is a thin wrapper over [keyring-core](https://crates.io/crates/keyring-core) 1.0.1 plus per-platform store crates (verified: [README](https://github.com/open-source-cooperative/keyring-rs)). The default `v1` feature keeps the classic `Entry::new(service, user)` and `get_password()` API (verified: probe build). It enables Apple Keychain, Windows and zbus Secret Service stores (verified: crates.io features). Cost: 65 extra crates and about 3 MB of binary (measured: 216 vs 281 crates, 11 vs 14 MB). |
| [secrecy](https://crates.io/crates/secrecy) | 0.10.3 | 2024-10-09 | Apache-2.0 OR MIT | **Pick** | `SecretString` keeps tokens out of `Debug` output and logs, and zeroizes on drop. himalaya and io-jmap use it (verified: crates.io deps). |
| [zeroize](https://crates.io/crates/zeroize) | 1.9.1 | 2026-10-06 | Apache-2.0 OR MIT | Pick (transitive) | Through secrecy. |
| [etcetera](https://crates.io/crates/etcetera) | 0.11.0 | 2025-10-28 | MIT OR Apache-2.0 | **Pick** | `choose_base_strategy()` for config, data and cache dirs. Can also force XDG on macOS (`base_strategy::Xdg`), which many terminal users expect (verified: probe build of both). |
| [dirs](https://crates.io/crates/dirs) | 7.0.0 | 2026-09-05 | MIT OR Apache-2.0 | Alt | Simple functions per directory. Now on Codeberg. |
| [directories](https://crates.io/crates/directories) | 6.0.0 | 2025-01-12 | MIT OR Apache-2.0 | Alt | The GitHub repo is archived with "moved to codeberg.org/dirs/directories-rs" (verified: GitHub API). Works, but always uses platform-native paths. |
| [oauth2](https://crates.io/crates/oauth2) | 5.0.0 | 2025-01-21 | MIT OR Apache-2.0 | Later | Authorization code plus PKCE. Its optional `reqwest` integration pins reqwest ^0.12 (verified: crates.io deps), so with our 0.13 use its custom HTTP client support (unverified API details) or hand-roll the token exchange. It also pulls chrono and rand 0.8. Fastmail requires PKCE with S256, rotates refresh tokens, and registers OAuth clients by hand through its developers (verified: [fastmail.com/dev](https://www.fastmail.com/dev/)). Day one should use an API token from Settings, Privacy & Security, with Bearer auth. |
| [openidconnect](https://crates.io/crates/openidconnect) | 4.0.1 | 2025-07-06 | MIT | Avoid | Fastmail's documented flow is plain OAuth 2.0, not OIDC (verified: fastmail.com/dev). |

## Misc

| Crate | Version | Released | License | Verdict | Why / why not |
|---|---|---|---|---|---|
| [tracing](https://crates.io/crates/tracing) | 0.1.44 | 2025-12-18 | MIT | **Pick** | Structured logs and spans. Time each JMAP call and each frame. |
| [tracing-subscriber](https://crates.io/crates/tracing-subscriber) | 0.3.23 | 2026-03-13 | MIT | **Pick** | `EnvFilter` (`RUST_LOG`). |
| [tracing-appender](https://crates.io/crates/tracing-appender) | 0.2.5 | 2026-04-17 | MIT | **Pick** | A TUI cannot log to stdout. Write rolling logs to the state dir with a non-blocking writer. |
| [anyhow](https://crates.io/crates/anyhow) | 1.0.104 | 2026-07-18 | MIT OR Apache-2.0 | **Pick** | Error handling in the binary. |
| [thiserror](https://crates.io/crates/thiserror) | 2.0.21 | 2026-09-23 | MIT OR Apache-2.0 | **Pick** | Typed errors in library modules (the JMAP client, the cache). |
| [color-eyre](https://crates.io/crates/color-eyre) | 0.6.5 | 2025-05-30 | MIT OR Apache-2.0 | Alt | Pretty reports and panic hooks. Ratatui 0.30's `init()`/`run()` already restore the terminal on panic (unverified for the hook details). anyhow is simpler. |
| [miette](https://crates.io/crates/miette) | 7.6.0 | 2025-04-27 | Apache-2.0 | Later | Diagnostic reports with source spans. A nice fit for Sieve and config errors with line and column, but it can wait. |
| [human-panic](https://crates.io/crates/human-panic) | 2.0.8 | 2026-04-02 | MIT OR Apache-2.0 | Later | Friendly crash report file. |
| [notify-rust](https://crates.io/crates/notify-rust) | 4.18.2 | 2026-10-07 | MIT OR Apache-2.0 | Later | Desktop notifications. Uses zbus on Linux (shared with keyring's Secret Service store, inferred from both using zbus 5) and mac-notification-sys on macOS (verified: crates.io deps). |
| [arboard](https://crates.io/crates/arboard) | 3.7.0 | 2026-10-15 | MIT OR Apache-2.0 | Alt | System clipboard by 1Password (X11, Wayland via `wayland-data-control`, macOS, Windows). Does not work over SSH. |
| crossterm `osc52` feature | 0.29.0 | 2025-04-05 | MIT | **Pick** | `crossterm::clipboard::CopyToClipboard::to_clipboard_from(..)` writes OSC 52, which works over SSH and tmux if the terminal allows it (verified: probe build; terminal behavior (inferred)). Use OSC 52 by default and arboard as a fallback. |
| [osc52](https://crates.io/crates/osc52) | 0.1.0 | 2023-08-11 | GPL-3.0-only | Avoid (license) | GPL, and replaced by crossterm's feature. |
| [copypasta](https://crates.io/crates/copypasta) | 0.10.2 | 2025-04-25 | MIT / Apache-2.0 | Avoid | Alacritty's clipboard. arboard is more active. |
| [open](https://crates.io/crates/open) | 5.4.5 | 2026-10-22 | MIT | **Pick** | Opens URLs and attachments with the system handler. Used by himalaya (verified: crates.io deps). |
| [opener](https://crates.io/crates/opener) | 0.9.0 | 2026-09-29 | MIT OR Apache-2.0 | Alt | Same job, plus "reveal in file manager". |
| [mime_guess](https://crates.io/crates/mime_guess) | 2.0.5 | 2024-06-29 | MIT | Later | Content type from file extension for attachment uploads. Old but stable. |
| [mime](https://crates.io/crates/mime) | 0.3.17 | 2023-03-20 | MIT OR Apache-2.0 | Avoid | Plain strings are enough for JMAP `type` fields (inferred). |
| [jiff](https://crates.io/crates/jiff) | 0.2.38 | 2026-10-06 | Unlicense OR MIT | **Pick** | **Recommended date library.** Time zone aware (`Zoned`), DST-correct arithmetic, bundled or system tzdb, serde support. JMAP `UTCDate` maps to `jiff::Timestamp` (inferred). By BurntSushi, 3k stars, active. |
| [chrono](https://crates.io/crates/chrono) | 0.4.45 | 2026-06-04 | MIT OR Apache-2.0 | Avoid (direct use) | Still fine, but jiff has the better time zone model. chrono enters the tree through jmap-client and oauth2 if those are added (verified: crates.io deps). |
| [time](https://crates.io/crates/time) | 0.3.55 | 2026-08-01 | MIT OR Apache-2.0 | Avoid | Comes in through tantivy if added. No reason to use directly. |
| [unicode-width](https://crates.io/crates/unicode-width) | 0.2.2 | 2025-10-06 | MIT OR Apache-2.0 | Pick (transitive) | Display width of CJK and emoji for column layout. Already used by ratatui and textarea. |
| [unicode-segmentation](https://crates.io/crates/unicode-segmentation) | 1.13.3 | 2026-06-01 | MIT OR Apache-2.0 | Pick (transitive) | Grapheme-safe truncation of subjects and names ("..."). |
| [textwrap](https://crates.io/crates/textwrap) | 0.16.4 | 2026-09-13 | MIT | Later | Re-wrap plain-text and `format=flowed` bodies, and wrap quoted replies. |
| [syntect](https://crates.io/crates/syntect) | 5.3.0 | 2025-09-27 | MIT | Later | Syntax highlighting for patch emails and code attachments. Heavy (regex engine plus syntax dumps). 142 open issues plus PRs. |
| [two-face](https://crates.io/crates/two-face) | 0.5.2+bat-0.26.1 | 2026-08-07 | MIT OR Apache-2.0 | Later | bat's extra syntaxes and themes for syntect (diff, TOML and more). |
| [image](https://crates.io/crates/image) | 0.25.10 | 2026-03-10 | MIT OR Apache-2.0 | Later | Decoders for ratatui-image. Trim formats with features to save build time (inferred). |
| [criterion](https://crates.io/crates/criterion) | 0.8.2 | 2026-02-04 | Apache-2.0 OR MIT | Alt | Statistical benchmarks. Now maintained under `criterion-rs` (verified: crates.io repo URL). |
| [divan](https://crates.io/crates/divan) | 0.1.21 | 2025-04-10 | MIT OR Apache-2.0 | **Pick (dev)** | Simpler benchmark API with allocation counting. Good for "render 1M-row list" and "parse 10k emails" benchmarks. Last release April 2025, repo pushed 2026-07. |
| [insta](https://crates.io/crates/insta) | 1.49.0 | 2026-10-03 | Apache-2.0 | **Pick (dev)** | Snapshot tests. `insta::assert_snapshot!(terminal.backend())` on a ratatui `TestBackend` works and produces a readable grid of cells (verified: probe test passed). |
| ratatui `TestBackend` | (in ratatui 0.30.2) | | MIT | **Pick (dev)** | In-memory backend for render tests. Pair with insta. |
| [vt100](https://crates.io/crates/vt100) | 0.18.1 | 2026-10-19 | MIT | Later (dev) | Terminal emulator parser for end-to-end tests that run the real binary in a pty and inspect the screen. |
| [signal-hook](https://crates.io/crates/signal-hook) | 0.4.5 | 2026-10-04 | MIT OR Apache-2.0 | Avoid (direct) | crossterm handles SIGWINCH, and tokio's `signal` handles SIGTERM. Suspend (Ctrl-Z, SIGTSTP) needs care either way (inferred). |
| [compact_str](https://crates.io/crates/compact_str) | 0.10.0 | 2026-07-13 | MIT | Later | Inline small strings (ids, keywords) to cut allocations in big lists, if profiling shows a need. |
| [uuid](https://crates.io/crates/uuid) | 1.29.0 | 2026-10-23 | Apache-2.0 OR MIT | Avoid | JMAP creation ids can be simple counters (inferred). |

## Prior art in Rust

| Project | Crate version | License | What to learn |
|---|---|---|---|
| [meli](https://crates.io/crates/meli) / [melib](https://crates.io/crates/melib) | 0.8.13 (2026-01-05) | EUPL-1.2 OR GPL-3.0-or-later | A full terminal MUA with a JMAP backend (verified: [crates.io melib](https://crates.io/crates/melib) "Provides the JMAP backend"). Uses termion 1.5 and its own widget layer, with no ratatui (verified: crates.io deps). melib's JMAP uses isahc (curl), smol and optional rusqlite for caching (verified: crates.io deps). Study its threading, cache schema and JMAP sync logic. Do not copy code (copyleft). Source on [git.meli-email.org](https://git.meli-email.org/meli/meli.git). |
| [himalaya](https://crates.io/crates/himalaya) | 2.2.1 (2026-10-02) | MIT OR Apache-2.0 | A CLI (not a TUI) built from Pimalaya's sans-IO crates: io-jmap, io-imap, io-smtp, io-managesieve, io-maildir and more (verified: crates.io deps). 7.5k stars, 6 open issues plus PRs. Good reference for config shape, account discovery (`io-pim-discovery`) and secret handling (`secrecy`). |
| [email-lib](https://crates.io/crates/email-lib), [mml-lib](https://crates.io/crates/mml-lib) | 0.27.0, 1.1.2 (2026-02-19) | MIT | Pimalaya's older async stack (mail-parser 0.9, mail-builder 0.3). himalaya 2.x no longer depends on email-lib (verified: himalaya 2.2.1 deps). Treat it as legacy. MML (a MIME meta language) is an interesting compose format (inferred). |
| [pimalaya-tui](https://crates.io/crates/pimalaya-tui) | 0.3.1 (2026-02-19) | MIT | Prompts and wizards on crossterm 0.27 plus `inquire`. Not a full-screen TUI (verified: crates.io deps). Little to reuse. |
| [jmap-client](https://github.com/stalwartlabs/jmap-client) | 0.4.3 | Apache-2.0 OR MIT | Best reference for the JMAP request builder, back-references, EventSource parsing (`src/event_source/parser.rs`) and Sieve methods. Copyable with attribution. |
| [Stalwart server](https://github.com/stalwartlabs/stalwart) | n/a (not on crates.io) | AGPL-3.0 (unverified exact SPDX; the GitHub API shows no SPDX id) | 15.3k stars. Its JMAP test suite is a good source of protocol edge cases. Read only, given the license. |
| [mujmap](https://github.com/elizagamedev/mujmap) | 0.2.0 | GPL-3.0 | A thin JMAP client on ureq and serde for notmuch sync. Shows the thin-client approach is workable. Stale. |
| [io-jmap](https://github.com/pimalaya/io-jmap) | 0.6.0 | MIT OR Apache-2.0 | Sans-IO JMAP design. Its EventSource handling yields "one state change per push frame with cooperative shutdown" (verified: README), a good model for mejl's push task. |
| [crate-jmap](https://github.com/MarkAtwood/crate-jmap) | 0.3.0 | MIT OR Apache-2.0 | Typed JMAP wire structs for many extensions, including RFC 9661 Sieve on the server side. Useful to cross-check field names. |

Non-Rust prior art (aerc, Mailspring, Superhuman, Fastmail's own web app) is covered in other docs.

## Version and license traps

Collected in one place, each verified from the crates.io dependencies or versions API:

| Trap | Effect | What to do |
|---|---|---|
| `tui-textarea` 0.7.0 needs `ratatui ^0.29` | Does not work with ratatui 0.30 | Use `ratatui-textarea` 0.9.3 |
| `reqwest-eventsource` 0.6.0 and `jmap-base-client` 0.3.0 need `reqwest ^0.12` | Second reqwest and hyper stack in the tree | Use `sse-stream` on reqwest 0.13 |
| `oauth2` 5.0.0's `reqwest` feature needs `reqwest ^0.12` | Same duplication | Custom HTTP client or a hand-rolled token exchange |
| `ratatui-termina` 0.1.0 needs `termina ^0.3`, latest is 0.4.0 | Backend lags behind its library | Stay on crossterm |
| `jmap-client` 0.4.3 `websockets` needs `tokio-tungstenite ^0.29`, latest is 0.30.0 | Duplicate tungstenite if we also use 0.30 | Only relevant if adopting jmap-client |
| `ratatui-image` default feature `chafa-dyn` | Needs pkg-config and system libchafa at build time | `default-features = false, features = ["crossterm", "image-defaults"]` |
| reqwest 0.13 `rustls` uses aws-lc-rs (so do jmap-client's defaults) | C build step, slower compile, harder cross-compiling (inferred) | Accept for now, or use `rustls-no-provider` plus ring (unverified build) |
| `sieve-rs` is AGPL-3.0-only | Makes mejl AGPL if linked | Do not link unless mejl is AGPL |
| `html2md` GPL-3.0+, `osc52` GPL-3.0-only, `meli`/`melib` EUPL or GPL, `mujmap` GPL-3.0 | Copyleft | Use alternatives listed above |
| `nucleo`/`nucleo-matcher` MPL-2.0 | File-level copyleft on changes to its files | Fine as an unmodified dependency (inferred) |
| MSRV outliers: `eventsource-client` 1.97, `sqlx` 1.94, `skim` 1.91, `redb`/`fjall` 1.90 | Raises mejl's MSRV | None are in the starter set. Starter direct deps declare at most 1.88 (ratatui, keyring). |

## Recommended starter dependency set

Every uncommented line below was in the probe crate. The probe resolved, compiled, ran, passed an insta snapshot test and ran a divan benchmark on 2026-10-23 with rustc 1.99.1 (verified). The commented "add when needed" lines have versions verified on crates.io. Building them together with the starter set is (unverified), except `mail-builder`, which was in the probe.

```toml
[dependencies]
# TUI
ratatui = "0.30.2"                       # default features: crossterm, macros, layout-cache, underline-color, all-widgets
crossterm = { version = "0.29.0", features = ["event-stream", "osc52"] }
ratatui-textarea = "0.9.3"
tui-input = "0.15.5"
tui-tree-widget = "0.24.1"

# Async and HTTP (thin JMAP client)
tokio = { version = "1.54.1", features = ["rt-multi-thread", "macros", "sync", "time", "signal"] }
futures = "0.3.34"
reqwest = { version = "0.13.5", default-features = false, features = ["rustls", "http2", "json", "stream", "gzip"] }
sse-stream = "0.3.0"
serde = { version = "1.0.229", features = ["derive"] }
serde_json = "1.0.154"

# Mail and HTML
mail-parser = "0.11.9"
html2text = "0.17.4"
linkify = "0.11.0"

# Cache and search
rusqlite = { version = "0.40.2", features = ["bundled"] }   # includes FTS5

# Pickers
nucleo-matcher = "0.3.1"

# Config, secrets, paths
toml = "1.1.8"
etcetera = "0.11.0"
keyring = "4.2.0"                        # default "v1" feature: macOS Keychain, Windows, Secret Service
secrecy = "0.10.3"

# Misc
jiff = { version = "0.2.38", features = ["serde"] }
anyhow = "1.0.104"
thiserror = "2.0.21"
tracing = "0.1.44"
tracing-subscriber = { version = "0.3.23", features = ["env-filter"] }
tracing-appender = "0.2.5"
open = "5.4.5"

# Add when needed
# mail-builder = "1.0.0"                 # raw MIME for Email/import (was in probe)
# ammonia = "4.2.4"                      # sanitize before "open in browser"
# lol_html = "3.0.2"                     # HTML pre-clean pass
# ratatui-image = { version = "11.1.0", default-features = false, features = ["crossterm", "image-defaults"] }
# notify-rust = "4.18.2"
# arboard = "3.7.0"                      # clipboard fallback when OSC 52 is unavailable
# toml_edit = "0.25.17"                  # write config without losing comments
# clap = { version = "4.6.7", features = ["derive"] }
# tokio-util = "0.7.20"                  # CancellationToken
# tantivy = "0.27.0"                     # only if FTS5 falls short
# tokio-tungstenite = { version = "0.30.0", features = ["rustls-tls-native-roots"] }  # only if Fastmail offers JMAP WebSocket

[dev-dependencies]
insta = "1.49.0"
divan = "0.1.21"

[profile.release]
lto = "thin"
codegen-units = 1
strip = true
```

Deliberately left out: `jmap-client` (see JMAP), `sieve-rs` (license), `oauth2` (manual client registration at Fastmail plus the reqwest 0.12 pin; start with API tokens), `chrono` (jiff instead).

## Compile time and binary size

Measured on this machine: rustc 1.99.1, 16 cores, 30 GB RAM, Linux x86_64, release profile `lto = "thin"`, `codegen-units = 1`, `strip = true`. No `RUSTC_WRAPPER`, no sccache, and no cargo config file, so the linker was rustc's default for the target (verified: environment check). Whether that default is rust-lld on this toolchain is (unverified); it would explain the fast relinks. One run each, so treat the numbers as rough.

| Build | Crates in tree | Wall time | Binary |
|---|---|---|---|
| Starter probe, clean release (includes crate downloads) | 281 | 48.8 s (3 min 51 s CPU) | 14 MB |
| Starter probe, incremental release after touching `main.rs` | 281 | 7.5 s | 14 MB |
| Starter probe, clean debug (registry warm) | 281 | 12.9 s | 185 MB (unstripped, debug info) |
| Starter probe, incremental debug | 281 | 0.5 s | |
| Starter probe, `opt-level = "s"`, `panic = "abort"`, clean | 281 | 25.7 s | 8.8 MB |
| Starter without `keyring` | 216 | 40.4 s | 11 MB |
| Starter plus `jmap-client` 0.4.3 (default features) | 304 | 49.2 s | 14 MB |
| Final starter set (above, adds tui-input, tui-tree-widget, linkify, jiff `serde`, divan) | 284 | not timed clean | 14 MB |

The first six rows used the earlier probe (281 crates). The final starter set adds 3 crates and builds to the same size (verified).

Duplicate versions in the final starter tree (verified: full `cargo tree -d -e normal` output): `base64` 0.22.1 (via crossterm's `osc52`) and 0.23.1 (via reqwest and hyper-util), and `syn` 2 and 3. `hashbrown` 0.17.1, `toml_datetime` 1.1.2 and `toml_parser` 1.1.5 also appear twice, but with the same version. That is one version built twice for different feature sets or for host and proc-macro use (inferred), not a version conflict. Adding jmap-client adds duplicate `digest`, `block-buffer`, `crypto-common` and `getrandom` versions (verified).

Notes (inferred unless marked):

- The incremental debug loop (0.5 s) is what matters day to day, and it is already fast. Keep mejl as a workspace of a few crates (for example `mejl-jmap`, `mejl-cache`, `mejl-ui`, `mejl`) so edits to the UI do not recompile the JMAP types.
- The 14 MB release binary is mostly the TLS crypto backend (aws-lc-rs), bundled SQLite, html5ever and its tables, tokio and hyper, plus zbus from keyring. Only keyring was measured (about 3 MB). Run `cargo bloat --release --crates` once the app exists. It is not installed here.
- `opt-level = "s"` cut the binary by about 37%. Measure frame and parse times before choosing it, since "s" can be slower in hot loops.
- Swapping aws-lc-rs for ring should cut compile time and size, and remove the C/cmake step. Not measured.
- A faster linker (mold or lld) and `cargo nextest` help more as the codebase grows. Not measured here.

## Appendix: repo signals for every crate

One row per crate linked in this doc (verified: GitHub API via `gh api repos/<owner>/<repo>`, 2026-10-23). The repo comes from the crate's `repository` field on crates.io. "Open" is GitHub's `open_issues_count`, which includes open pull requests. Several crates can share one repo (monorepos), so their numbers repeat. Repos not on GitHub were not checked.

| Crate | Repo (owner/name) | Stars | Open | Last push | Archived |
|---|---|---|---|---|---|
| ammonia | [rust-ammonia/ammonia](https://github.com/rust-ammonia/ammonia) | 682 | 23 | 2026-10-17 | no |
| ansi-to-tui | [ratatui/ansi-to-tui](https://github.com/ratatui/ansi-to-tui) | 101 | 7 | 2026-10-10 | no |
| anyhow | [dtolnay/anyhow](https://github.com/dtolnay/anyhow) | 6684 | 46 | 2026-09-29 | no |
| arboard | [1Password/arboard](https://github.com/1Password/arboard) | 975 | 40 | 2026-10-16 | no |
| chrono | [chronotope/chrono](https://github.com/chronotope/chrono) | 3926 | 194 | 2026-09-07 | no |
| clap | [clap-rs/clap](https://github.com/clap-rs/clap) | 16794 | 452 | 2026-10-06 | no |
| color-eyre | [eyre-rs/eyre](https://github.com/eyre-rs/eyre) | 1786 | 56 | 2026-08-11 | no |
| compact_str | [ParkMyCar/compact_str](https://github.com/ParkMyCar/compact_str) | 877 | 26 | 2026-10-03 | no |
| config | [rust-cli/config-rs](https://github.com/rust-cli/config-rs) | 3221 | 138 | 2026-10-13 | no |
| copypasta | [alacritty/copypasta](https://github.com/alacritty/copypasta) | 347 | 6 | 2025-05-09 | no |
| criterion | [criterion-rs/criterion.rs](https://github.com/criterion-rs/criterion.rs) | 480 | 29 | 2026-10-13 | no |
| crokey | [Canop/crokey](https://github.com/Canop/crokey) | 45 | 0 | 2026-10-16 | no |
| crossterm | [crossterm-rs/crossterm](https://github.com/crossterm-rs/crossterm) | 4248 | 253 | 2026-10-20 | no |
| cursive | [gyscos/cursive](https://github.com/gyscos/cursive) | 4857 | 207 | 2026-09-25 | no |
| directories | [dirs-dev/directories-rs](https://github.com/dirs-dev/directories-rs) | 836 | 19 | 2025-01-12 | yes |
| dirs | https://codeberg.org/dirs/dirs-rs | - | - | - | not on GitHub (codeberg.org), not checked |
| divan | [nvzqz/divan](https://github.com/nvzqz/divan) | 1460 | 51 | 2026-07-19 | no |
| duckdb | [duckdb/duckdb-rs](https://github.com/duckdb/duckdb-rs) | 981 | 23 | 2026-10-23 | no |
| edtui | [preiter93/edtui](https://github.com/preiter93/edtui) | 162 | 5 | 2026-10-10 | no |
| email_address | [johnstonskj/rust-email_address](https://github.com/johnstonskj/rust-email_address) | 95 | 20 | 2025-10-26 | no |
| email-lib | [pimalaya/core](https://github.com/pimalaya/core) | 44 | 23 | 2026-03-01 | no |
| etcetera | [lunacookies/etcetera](https://github.com/lunacookies/etcetera) | 153 | 2 | 2026-08-04 | no |
| eventsource-client | [launchdarkly/rust-eventsource-client](https://github.com/launchdarkly/rust-eventsource-client) | 113 | 0 | 2026-10-07 | no |
| eventsource-stream | [jpopesculian/eventsource-stream](https://github.com/jpopesculian/eventsource-stream) | 39 | 8 | 2024-08-15 | no |
| fast_html2md | [spider-rs/html2md](https://github.com/spider-rs/html2md) | 78 | 0 | 2026-09-08 | no |
| figment | [SergioBenitez/Figment](https://github.com/SergioBenitez/Figment) | 916 | 7 | 2026-10-13 | no |
| fjall | [fjall-rs/fjall](https://github.com/fjall-rs/fjall) | 2396 | 46 | 2026-10-21 | no |
| frizbee | [saghen/frizbee](https://github.com/saghen/frizbee) | 592 | 7 | 2026-09-18 | no |
| ftui-core | [Dicklesworthstone/frankentui](https://github.com/Dicklesworthstone/frankentui) | 291 | 4 | 2026-10-20 | no |
| futures | [rust-lang/futures-rs](https://github.com/rust-lang/futures-rs) | 5934 | 280 | 2026-10-01 | no |
| fuzzy-matcher | [skim-rs/fuzzy-matcher](https://github.com/skim-rs/fuzzy-matcher) | 295 | 5 | 2024-06-29 | yes |
| heed | [meilisearch/heed](https://github.com/meilisearch/heed) | 921 | 52 | 2026-10-22 | no |
| himalaya | [pimalaya/himalaya](https://github.com/pimalaya/himalaya) | 7522 | 6 | 2026-10-17 | no |
| htmd | [letmutex/htmd](https://github.com/letmutex/htmd) | 463 | 2 | 2026-10-20 | no |
| html2md | https://gitlab.com/Kanedias/html2md | - | - | - | not on GitHub (gitlab.com), not checked |
| html2text | [jugglerchris/rust-html2text](https://github.com/jugglerchris/rust-html2text) | 245 | 14 | 2026-10-18 | no |
| html5ever | [servo/html5ever](https://github.com/servo/html5ever) | 2643 | 81 | 2026-10-05 | no |
| human-panic | [rust-cli/human-panic](https://github.com/rust-cli/human-panic) | 1853 | 14 | 2026-10-04 | no |
| hyper | [hyperium/hyper](https://github.com/hyperium/hyper) | 16371 | 221 | 2026-10-13 | no |
| image | [image-rs/image](https://github.com/image-rs/image) | 5893 | 237 | 2026-09-15 | no |
| insta | [mitsuhiko/insta](https://github.com/mitsuhiko/insta) | 2976 | 79 | 2026-10-10 | no |
| io-jmap | [pimalaya/io-jmap](https://github.com/pimalaya/io-jmap) | 3 | 0 | 2026-10-16 | no |
| io-managesieve | [pimalaya/io-managesieve](https://github.com/pimalaya/io-managesieve) | 1 | 0 | 2026-10-16 | no |
| jiff | [BurntSushi/jiff](https://github.com/BurntSushi/jiff) | 2958 | 55 | 2026-10-14 | no |
| jmap | [robn/jmap-rs](https://github.com/robn/jmap-rs) | 15 | 5 | 2022-08-17 | yes |
| jmap-base-client | [MarkAtwood/crate-jmap](https://github.com/MarkAtwood/crate-jmap) | 2 | 0 | 2026-10-23 | no |
| jmap-client | [stalwartlabs/jmap-client](https://github.com/stalwartlabs/jmap-client) | 124 | 13 | 2026-09-28 | no |
| jmap-mail-client | [MarkAtwood/crate-jmap](https://github.com/MarkAtwood/crate-jmap) | 2 | 0 | 2026-10-23 | no |
| jmap-mail-types | [MarkAtwood/crate-jmap](https://github.com/MarkAtwood/crate-jmap) | 2 | 0 | 2026-10-23 | no |
| jmap-mime | [MarkAtwood/crate-jmap](https://github.com/MarkAtwood/crate-jmap) | 2 | 0 | 2026-10-23 | no |
| jmap-tools | [stalwartlabs/jmap-tools](https://github.com/stalwartlabs/jmap-tools) | 9 | 0 | 2026-10-01 | no |
| jmap-types | [MarkAtwood/crate-jmap](https://github.com/MarkAtwood/crate-jmap) | 2 | 0 | 2026-10-23 | no |
| keyring | [open-source-cooperative/keyring-rs](https://github.com/open-source-cooperative/keyring-rs) | 773 | 6 | 2026-10-22 | no |
| keyring-core | [open-source-cooperative/keyring-core](https://github.com/open-source-cooperative/keyring-core) | 39 | 5 | 2026-10-16 | no |
| lettre | [lettre/lettre](https://github.com/lettre/lettre) | 2272 | 93 | 2026-10-02 | no |
| libsql | [tursodatabase/libsql](https://github.com/tursodatabase/libsql) | 17285 | 471 | 2026-10-17 | no |
| limbo | [tursodatabase/turso](https://github.com/tursodatabase/turso) | 24983 | 1484 | 2026-10-23 | no |
| linkify | [robinst/linkify](https://github.com/robinst/linkify) | 241 | 17 | 2026-08-23 | no |
| lol_html | [cloudflare/lol-html](https://github.com/cloudflare/lol-html) | 2079 | 43 | 2026-10-23 | no |
| lru | [jeromefroe/lru-rs](https://github.com/jeromefroe/lru-rs) | 845 | 25 | 2026-10-18 | no |
| lz4_flex | [PSeitz/lz4_flex](https://github.com/PSeitz/lz4_flex) | 619 | 40 | 2026-08-11 | no |
| mail-auth | [stalwartlabs/mail-auth](https://github.com/stalwartlabs/mail-auth) | 127 | 6 | 2026-10-19 | no |
| mail-builder | [stalwartlabs/mail-builder](https://github.com/stalwartlabs/mail-builder) | 83 | 11 | 2026-10-07 | no |
| mailparse | [staktrace/mailparse](https://github.com/staktrace/mailparse) | 226 | 1 | 2026-10-05 | no |
| mail-parser | [stalwartlabs/mail-parser](https://github.com/stalwartlabs/mail-parser) | 466 | 3 | 2026-10-19 | no |
| mailrs-sieve-core | [goliajp/mailrs](https://github.com/goliajp/mailrs) | 7 | 0 | 2026-10-23 | no |
| mail-send | [stalwartlabs/mail-send](https://github.com/stalwartlabs/mail-send) | 275 | 15 | 2026-09-28 | no |
| meli | https://git.meli-email.org/meli/meli.git | - | - | - | not on GitHub (git.meli-email.org), not checked |
| melib | https://git.meli-email.org/meli/meli.git | - | - | - | not on GitHub (git.meli-email.org), not checked |
| miette | [zkat/miette](https://github.com/zkat/miette) | 2619 | 115 | 2026-06-25 | no |
| milli | [meilisearch/milli](https://github.com/meilisearch/milli) | 466 | 5 | 2023-04-04 | yes |
| mime | [hyperium/mime](https://github.com/hyperium/mime) | 223 | 52 | 2024-10-31 | no |
| mime_guess | [abonander/mime_guess](https://github.com/abonander/mime_guess) | 221 | 16 | 2025-09-12 | no |
| mml-lib | [pimalaya/core](https://github.com/pimalaya/core) | 44 | 23 | 2026-03-01 | no |
| moka | [moka-rs/moka](https://github.com/moka-rs/moka) | 2711 | 73 | 2026-08-09 | no |
| mujmap | [elizagamedev/mujmap](https://github.com/elizagamedev/mujmap) | 89 | 40 | 2024-07-03 | no |
| nanohtml2text | [alexwennerberg/nanohtml2text](https://github.com/alexwennerberg/nanohtml2text) | 0 | 0 | 2026-08-27 | no |
| notify-rust | [hoodie/notify-rust](https://github.com/hoodie/notify-rust) | 1431 | 26 | 2026-10-16 | no |
| nucleo | [helix-editor/nucleo](https://github.com/helix-editor/nucleo) | 1517 | 32 | 2026-06-24 | no |
| nucleo-matcher | [helix-editor/nucleo](https://github.com/helix-editor/nucleo) | 1517 | 32 | 2026-06-24 | no |
| oauth2 | [ramosbugs/oauth2-rs](https://github.com/ramosbugs/oauth2-rs) | 1212 | 49 | 2026-02-22 | no |
| open | [Byron/open-rs](https://github.com/Byron/open-rs) | 405 | 13 | 2026-10-22 | no |
| opener | [Seeker14491/opener](https://github.com/Seeker14491/opener) | 74 | 4 | 2026-10-06 | no |
| openidconnect | [ramosbugs/openidconnect-rs](https://github.com/ramosbugs/openidconnect-rs) | 647 | 79 | 2026-10-21 | no |
| osc52 | [jnkel/osc52](https://github.com/jnkel/osc52) | 2 | 0 | 2023-08-11 | no |
| pimalaya-tui | [pimalaya/tui](https://github.com/pimalaya/tui) | 1 | 1 | 2026-02-21 | no |
| r3bl_tui | [r3bl-org/r3bl-open-core](https://github.com/r3bl-org/r3bl-open-core) | 486 | 72 | 2026-10-23 | no |
| ratatui | [ratatui/ratatui](https://github.com/ratatui/ratatui) | 23058 | 205 | 2026-10-21 | no |
| ratatui-core | [ratatui/ratatui](https://github.com/ratatui/ratatui) | 23058 | 205 | 2026-10-21 | no |
| ratatui-crossterm | [ratatui/ratatui](https://github.com/ratatui/ratatui) | 23058 | 205 | 2026-10-21 | no |
| ratatui-image | [ratatui/ratatui-image](https://github.com/ratatui/ratatui-image) | 410 | 4 | 2026-10-18 | no |
| ratatui-macros | [ratatui/ratatui](https://github.com/ratatui/ratatui) | 23058 | 205 | 2026-10-21 | no |
| ratatui-termina | [ratatui/ratatui](https://github.com/ratatui/ratatui) | 23058 | 205 | 2026-10-21 | no |
| ratatui-textarea | [ratatui/ratatui-textarea](https://github.com/ratatui/ratatui-textarea) | 97 | 7 | 2026-10-08 | no |
| ratatui-widgets | [ratatui/ratatui](https://github.com/ratatui/ratatui) | 23058 | 205 | 2026-10-21 | no |
| rat-text | [thscharler/rat-salsa](https://github.com/thscharler/rat-salsa) | 64 | 9 | 2026-07-03 | no |
| redb | [cberner/redb](https://github.com/cberner/redb) | 4865 | 2 | 2026-10-22 | no |
| reqwest | [seanmonstar/reqwest](https://github.com/seanmonstar/reqwest) | 11885 | 481 | 2026-10-19 | no |
| reqwest-eventsource | [jpopesculian/reqwest-eventsource](https://github.com/jpopesculian/reqwest-eventsource) | 72 | 16 | 2024-06-13 | no |
| reqwest-sse | [vvvinceocam/reqwest-sse](https://github.com/vvvinceocam/reqwest-sse) | 6 | 3 | 2026-05-10 | no |
| rkyv | [rkyv/rkyv](https://github.com/rkyv/rkyv) | 4379 | 82 | 2026-09-25 | no |
| rusqlite | [rusqlite/rusqlite](https://github.com/rusqlite/rusqlite) | 4445 | 169 | 2026-10-18 | no |
| rustls | [rustls/rustls](https://github.com/rustls/rustls) | 7699 | 91 | 2026-10-23 | no |
| rustls-platform-verifier | [rustls/rustls-platform-verifier](https://github.com/rustls/rustls-platform-verifier) | 160 | 36 | 2026-10-01 | no |
| scraper | [rust-scraper/scraper](https://github.com/rust-scraper/scraper) | 2425 | 11 | 2026-10-21 | no |
| secrecy | [iqlusioninc/crates](https://github.com/iqlusioninc/crates) | 580 | 47 | 2026-10-17 | no |
| serde | [serde-rs/serde](https://github.com/serde-rs/serde) | 10903 | 409 | 2026-10-16 | no |
| serde_json | [serde-rs/json](https://github.com/serde-rs/json) | 5657 | 244 | 2026-10-16 | no |
| sieve-form | [MarkAtwood/crate-sieve-form](https://github.com/MarkAtwood/crate-sieve-form) | 0 | 0 | 2026-10-23 | no |
| sieve-kit | [WyattAu/sieve-kit](https://github.com/WyattAu/sieve-kit) | 0 | 0 | 2026-10-23 | no |
| sieve-rs | [stalwartlabs/sieve](https://github.com/stalwartlabs/sieve) | 32 | 5 | 2026-10-04 | no |
| signal-hook | [vorner/signal-hook](https://github.com/vorner/signal-hook) | 878 | 27 | 2026-10-04 | no |
| skim | [skim-rs/skim](https://github.com/skim-rs/skim) | 7001 | 7 | 2026-10-20 | no |
| sled | [spacejam/sled](https://github.com/spacejam/sled) | 9101 | 172 | 2026-04-04 | no |
| sqlx | [transact-rs/sqlx](https://github.com/transact-rs/sqlx) | 17593 | 775 | 2026-10-22 | no |
| sse-stream | [4t145/sse-stream](https://github.com/4t145/sse-stream) | 11 | 0 | 2026-09-18 | no |
| syntect | [trishume/syntect](https://github.com/trishume/syntect) | 2448 | 142 | 2026-04-28 | no |
| tachyonfx | [ratatui/tachyonfx](https://github.com/ratatui/tachyonfx) | 1312 | 7 | 2026-09-06 | no |
| tantivy | [quickwit-oss/tantivy](https://github.com/quickwit-oss/tantivy) | 16253 | 488 | 2026-10-23 | no |
| termina | [helix-editor/termina](https://github.com/helix-editor/termina) | 93 | 6 | 2026-08-31 | no |
| termion | https://gitlab.redox-os.org/redox-os/termion | - | - | - | not on GitHub (gitlab.redox-os.org), not checked |
| termwiz | [wezterm/wezterm](https://github.com/wezterm/wezterm) | 29327 | 1929 | 2026-10-19 | no |
| textwrap | [mgeisler/textwrap](https://github.com/mgeisler/textwrap) | 527 | 32 | 2026-10-01 | no |
| thiserror | [dtolnay/thiserror](https://github.com/dtolnay/thiserror) | 5584 | 33 | 2026-10-16 | no |
| throbber-widgets-tui | [arkbig/throbber-widgets-tui](https://github.com/arkbig/throbber-widgets-tui) | 54 | 0 | 2026-08-19 | no |
| time | [time-rs/time](https://github.com/time-rs/time) | 1344 | 11 | 2026-10-16 | no |
| tokio | [tokio-rs/tokio](https://github.com/tokio-rs/tokio) | 33496 | 443 | 2026-10-23 | no |
| tokio-rusqlite | [programatik29/tokio-rusqlite](https://github.com/programatik29/tokio-rusqlite) | 131 | 0 | 2026-09-06 | no |
| tokio-tungstenite | [snapview/tokio-tungstenite](https://github.com/snapview/tokio-tungstenite) | 2515 | 22 | 2026-07-11 | no |
| tokio-util | [tokio-rs/tokio](https://github.com/tokio-rs/tokio) | 33496 | 443 | 2026-10-23 | no |
| toml | [toml-rs/toml](https://github.com/toml-rs/toml) | 1085 | 68 | 2026-10-09 | no |
| toml_edit | [toml-rs/toml](https://github.com/toml-rs/toml) | 1085 | 68 | 2026-10-09 | no |
| tracing | [tokio-rs/tracing](https://github.com/tokio-rs/tracing) | 6940 | 900 | 2026-10-09 | no |
| tracing-appender | [tokio-rs/tracing](https://github.com/tokio-rs/tracing) | 6940 | 900 | 2026-10-09 | no |
| tracing-subscriber | [tokio-rs/tracing](https://github.com/tokio-rs/tracing) | 6940 | 900 | 2026-10-09 | no |
| tui-input | [sayanarijit/tui-input](https://github.com/sayanarijit/tui-input) | 207 | 2 | 2026-09-26 | no |
| tui-logger | [gin66/tui-logger](https://github.com/gin66/tui-logger) | 320 | 2 | 2026-08-12 | no |
| tui-markdown | [joshka/tui-markdown](https://github.com/joshka/tui-markdown) | 118 | 3 | 2026-10-20 | no |
| tui-scrollview | [ratatui/tui-widgets](https://github.com/ratatui/tui-widgets) | 241 | 12 | 2026-10-21 | no |
| tui-textarea | [rhysd/tui-textarea](https://github.com/rhysd/tui-textarea) | 513 | 50 | 2024-12-01 | no |
| tui-textarea-2 | [srothgan/tui-textarea](https://github.com/srothgan/tui-textarea) | 11 | 3 | 2026-10-19 | no |
| tui-tree-widget | [EdJoPaTo/tui-rs-tree-widget](https://github.com/EdJoPaTo/tui-rs-tree-widget) | 129 | 8 | 2026-10-23 | no |
| tui-widgets | [ratatui/tui-widgets](https://github.com/ratatui/tui-widgets) | 241 | 12 | 2026-10-21 | no |
| turso | [tursodatabase/turso](https://github.com/tursodatabase/turso) | 24983 | 1484 | 2026-10-23 | no |
| two-face | https://codeberg.org/CosmicHarper/two-face | - | - | - | not on GitHub (codeberg.org), not checked |
| unicode-segmentation | [unicode-rs/unicode-segmentation](https://github.com/unicode-rs/unicode-segmentation) | 683 | 28 | 2026-10-18 | no |
| unicode-width | [unicode-rs/unicode-width](https://github.com/unicode-rs/unicode-width) | 321 | 9 | 2026-09-16 | no |
| ureq | [algesten/ureq](https://github.com/algesten/ureq) | 2215 | 42 | 2026-10-22 | no |
| uuid | [uuid-rs/uuid](https://github.com/uuid-rs/uuid) | 1250 | 8 | 2026-10-23 | no |
| vt100 | [doy/vt100-rust](https://github.com/doy/vt100-rust) | 123 | 12 | 2026-10-19 | no |
| zeroize | [RustCrypto/utils](https://github.com/RustCrypto/utils) | 685 | 37 | 2026-10-23 | no |
| zstd | [gyscos/zstd-rs](https://github.com/gyscos/zstd-rs) | 662 | 62 | 2026-10-21 | no |

Notable: `directories-rs`, `fuzzy-matcher`, `robn/jmap-rs` and `meilisearch/milli` (the old standalone repo) are archived. The `osc52` crate's repository URL now resolves to `jnkel/osc52`. The three permissive Sieve crates have 0 to 7 stars.

## Open questions

- **Does Fastmail advertise `urn:ietf:params:jmap:sieve` (RFC 9661) or `urn:ietf:params:jmap:websocket`?** This decides whether mejl edits rules over JMAP, which validation path it uses, and whether tokio-tungstenite is ever needed. Resolve: `GET https://api.fastmail.com/jmap/session` with a test API token (not the user's real account through MCP) and read `capabilities` and `accounts[*].accountCapabilities`.
- **Does Fastmail expose ManageSieve?** If not, `io-managesieve` is irrelevant. Resolve: Fastmail help pages on server names and ports, or ask Fastmail support.
- **What license will mejl use?** MIT/Apache rules out `sieve-rs`. AGPL would allow it. Resolve: decide before the first dependency lands.
- **Are the permissive Sieve parsers (`mailrs-sieve-core`, `sieve-form`, `sieve-kit`) any good?** Resolve: run each against a corpus of real Fastmail-generated Sieve scripts (exported from Settings) and the Sieve RFC examples.
- **How do draft-14 JMAP Sieve and RFC 9661 differ on the wire?** It matters only if borrowing code from jmap-client. Resolve: diff the draft and the RFC on datatracker.
- **Is SQLite FTS5 fast enough at 1M messages with mejl's schema?** Resolve: a divan benchmark that loads 1M synthetic messages (or an export of a large real mailbox) and measures list-page queries, FTS queries and `/changes` batch writes.
- **How much does ring vs aws-lc-rs change build time and binary size?** Resolve: a probe build with `reqwest` `rustls-no-provider` plus `rustls` with `ring`, installing the provider at start-up.
- **Will Fastmail register an OAuth client for mejl?** Fastmail registers clients by hand. Resolve: contact Fastmail developers. Until then, API tokens with Bearer auth.
- **Does html2text's `css` feature honor inline `style` colors well enough for marketing email?** Resolve: render a sample set of real HTML newsletters with and without the feature and compare.
- **Exact crossterm `KeyboardEnhancementFlags` names and terminal coverage** (kitty, Ghostty, WezTerm, foot, iTerm2, Windows Terminal). Resolve: read docs.rs for the flags type, and test with a small key-event logger in each terminal.
- **What MSRV will mejl promise?** The starter set's direct deps declare at most 1.88 (ratatui 0.30.2, keyring 4.2.0), but transitive deps may need more. Resolve: run `cargo +1.88 build` on the starter set (or use `cargo msrv`) once the workspace exists.
