# mejl architecture and product direction

Date: 2026-10-23. This doc synthesizes the six research docs in this folder. It adds no new web research. Claims carried over from a research doc keep the label they had there, and the doc is linked so the source can be checked. Design decisions made here are (inferred) unless stated otherwise. The research docs are:

- [fastmail-platform.md](fastmail-platform.md): Fastmail's JMAP, auth, push, limits, policy and MCP server.
- [fastmail-rules-sieve.md](fastmail-rules-sieve.md): what can and cannot be done with rules and Sieve.
- [fastmail-masked-email.md](fastmail-masked-email.md): the Masked Email API and UX ideas.
- [email-clients.md](email-clients.md): survey of terminal and GUI clients.
- [rust-crates.md](rust-crates.md): crate picks with versions and a verified probe build.
- [terminal-ux.md](terminal-ux.md): terminal protocols, input, layout, rendering and a UX spec sketch.

## Summary

- **Build one deep Fastmail client, local first.** The UI reads only from a local SQLite store. A background sync engine keeps the store current with JMAP state strings, `/changes`, `/queryChanges` and EventSource push. Every user action is applied locally in the same frame and sent later from a durable outbox. This is the design Fastmail's own web app uses ([email-clients.md](email-clients.md), verified there from Fastmail's offline blog posts), rebuilt in Rust (inferred).
- **Feasibility of the five headline goals (details below):**

  | Goal | Verdict | One line |
  |---|---|---|
  | JMAP-first | **Yes**, for mail, sending, contacts, Masked Email | Calendars are CalDAV only, files are WebDAV only, rules have no API at all |
  | Rules (Sieve) management | **Partial** | mejl can author, preview, validate locally and apply rules to existing mail. It cannot read or write the server script. The last step is a user paste into the web UI |
  | Masked Email | **Yes, with gaps** | List, create, enable, disable, delete and describe work over a documented API. Send-as-mask, delta sync and push for masks are unverified |
  | Instant UI | **Yes by design, unproven** | Reachable only if the UI thread never waits on the network or on unbounded disk work. Bounded, indexed page reads are allowed within a measured 2 ms p99 budget. Budgets and a measurement harness are defined here |
  | Keyboard-first plus mouse | **Yes** | Legacy-safe keymap, kitty keys as extras, mouse on by default with in-house hit testing |

- **Policy gates a public release, not the code.** Fastmail's API Developer Policy forbids "Using unpublished APIs" and using the API "to replicate or compete with core products" (verified: [fastmail-platform.md](fastmail-platform.md)). Its dev docs say distributed apps "should implement OAuth" and that API tokens are "for testing purposes, or building an app for just yourself" (verified: same). So mejl can be built and used personally with an API token today, but a public release should wait for Fastmail's written answer on the compete clause, token use and a rules API (inferred).
- **Auth:** API token paste first (works today on Standard and Professional plans). OAuth with PKCE and a `127.0.0.1` loopback redirect next, via the open dynamic registration that Fastmail's discovery metadata advertises but nobody has tested (unverified). Secrets go in the OS keyring, with a `token_command` fallback (inferred).
- **Stack:** ratatui 0.30.2, crossterm 0.29.0, tokio 1.54.1, reqwest 0.13.5 with sse-stream 0.3.0, rusqlite 0.40.2 (bundled, FTS5), html2text 0.17.4, nucleo-matcher 0.3.1, keyring 4.2.0, jiff 0.2.38. A thin custom JMAP client on reqwest and serde, because no crate supports MaskedEmail or vendor methods (verified: [rust-crates.md](rust-crates.md)).
- **Three decisions are needed before code lands:** the license (MIT/Apache rules out sieve-rs, which is AGPL-3.0-only), whether to support Basic-plan users (no API tokens; OAuth unknown), and when to write to Fastmail. These are the user's calls. This doc recommends answers but does not make them.

## Guiding principles

1. **Local first.** The UI renders from the local store. The network only reconciles. No network or bulk disk I/O on the render path (inferred, from [email-clients.md](email-clients.md) and [terminal-ux.md](terminal-ux.md)).
2. **Cache is disposable, user intent is not.** Deleting the cache must never lose data. The outbox, unsent drafts and mejl's own rules live in a separate durable store (inferred).
3. **Documented APIs only.** No private `https://www.fastmail.com/dev/*` capabilities except the documented `dev/maskedemail`. No browser cookies. No scraping (verified policy: [fastmail-platform.md](fastmail-platform.md)).
4. **Honest about web-only features.** Snooze times, saved searches, the blocked-senders list and Fastmail's own rules editor have no public API. mejl shows them as web-only, offers the best policy-safe path, and links to the web UI (inferred).
5. **Read limits from the session.** URLs, `maxCallsInRequest`, `maxObjectsInGet`, `maxObjectsInSet`, `maxConcurrentRequests` and capabilities come from the live session, never from constants (inferred, from [fastmail-platform.md](fastmail-platform.md)).
6. **Every action undoable, nothing destructive by default.** Undo replaces confirmation for reversible actions. Irreversible ones (permanent delete, `deleted` state on a mask) get a confirm (inferred, following Fastmail's `z` undo).
7. **Fastmail keymap by default, every key remappable.** Fastmail users should feel at home on day one (inferred, from [terminal-ux.md](terminal-ux.md)).
8. **Degrade without losing function.** Every action works with legacy key encoding, without mouse, without kitty protocol, inside stock tmux and over SSH (inferred).
9. **YAGNI on integrations.** No plugins, no AI features, no own MCP server, no IMAP, no multi-provider support in v1 (inferred, from the anti-patterns in [email-clients.md](email-clients.md)).
10. **Measure the speed claim.** "Ultrafast" is a test, not a slogan. Latency budgets are enforced in CI (inferred).

## Product direction

**Position.** mejl is the fast, keyboard-first way to live in a Fastmail account from a terminal. It does not try to be a general mail client. Fastmail web is already local first and offline over JMAP, so mejl cannot win on raw data speed alone (inferred, from [email-clients.md](email-clients.md)). It wins on:

- Cold start and memory. A native binary that shows the inbox in under 50 ms from a warm cache, against an Electron desktop app that one user saw at 700 MB of RAM (verified: HN thread cited in [fastmail-platform.md](fastmail-platform.md); mejl numbers are targets).
- Terminal workflow: SSH, tmux, a palette, a remappable keymap, scripting later.
- Fastmail-specific power: Masked Email actions from the message you are reading, and rules you build from a message with a live preview.

**Non-goals for v1** (inferred): calendars (CalDAV only for third parties), Files and Notes, other providers, IMAP, plugins, AI features, a local MCP server, a mail-merge or automation surface (the ToS rules out "programmatically generated emails", verified in [fastmail-platform.md](fastmail-platform.md)).

**Plan-gated features.** Snooze, Scheduled Send and Mute are not on Basic, and Snooze and Scheduled Send are also missing on legacy plans under 10 GB (verified: [fastmail-platform.md](fastmail-platform.md)). Rebuilding them on the client for plans that exclude them could be read as "circumventing Fastmail's intended limitations" (inferred there). So v1 does not build a client-side snooze timer or a client-side scheduled send. It shows the Snoozed and Scheduled mailboxes if present, and reserves the `b` prefix with a "snooze is web-only" message until Fastmail answers (inferred decision).

## Feasibility verdicts

### JMAP-first

| Area | Status | Source |
|---|---|---|
| Mail, threads, mailboxes, keywords, search, snippets | Public JMAP (RFC 8621) | verified: [fastmail-platform.md](fastmail-platform.md) |
| Sending (`EmailSubmission`, `Identity`) | Public. Token scope "Email submission" needs "Email" | verified: same |
| Contacts | Public JMAP Contacts (RFC 9610) | verified: same |
| Masked Email | Public Fastmail extension `https://www.fastmail.com/dev/maskedemail` | verified: [fastmail-masked-email.md](fastmail-masked-email.md) |
| Vacation response | Public JMAP, but only as an OAuth scope. API tokens list no vacation scope | verified: [fastmail-rules-sieve.md](fastmail-rules-sieve.md) |
| Push | EventSource works for third-party tokens since about September 2022. No evidence of WebSocket | verified and inferred: [fastmail-platform.md](fastmail-platform.md) |
| Calendars | CalDAV only until JMAP Calendars is final. The draft is in the RFC Editor queue | verified: same |
| Files | WebDAV only | verified: same |
| Rules, Sieve | No public API, no ManageSieve | verified: [fastmail-rules-sieve.md](fastmail-rules-sieve.md) |
| Snooze time, saved searches, grouped view, blocked senders | No public surface | verified absence and inferred: [fastmail-platform.md](fastmail-platform.md) |

**Verdict: yes, for everything mejl v1 needs except rules.** No IMAP or SMTP code is needed. CalDAV for calendar invites is a later, optional add-on with a second credential (inferred).

### Rules and Sieve management

**What is possible today** (inferred design on verified facts from [fastmail-rules-sieve.md](fastmail-rules-sieve.md)):

- A mejl-owned rule model (name, enabled, conditions as a JMAP Email filter, actions), stored in mejl's durable state.
- "Rule from this message" (`F`): conditions filled from sender, list-id or subject.
- Live preview: run the rule's filter as `Email/query` against the server and the local cache, and show "these 14 messages would match".
- Apply to existing mail with `Email/set`, as Fastmail's own rule editor offers.
- Local validation: an extension allowlist (only extensions in both the help-page `require` line and the 2025 sample line), checks on command names, and a dry run against cached mail for standard Sieve.
- Render one marked block (start and end markers, a hash, Roundcube-style `# mejl:rule` comments) for one custom section, by default the one below spam protection. Rules use `fileinto :mailboxid "<id>" "INBOX.Name"`, never `keep`, never `discard`, and no `stop` unless the user asks for it.
- Copy the block (OSC 52) and open `https://app.fastmail.com/settings/filters`. The user pastes and saves.
- Drift check when the user pastes the section back.
- Allow sender with `ContactCard/set` (feeds `X-Spam-known-sender`; does not override a DMARC failure).
- Disable a mask with `MaskedEmail/set`, which routes its mail to Trash.

**What is not possible today** (verified absences in [fastmail-rules-sieve.md](fastmail-rules-sieve.md)): reading the server script or UI rules, writing Sieve, rules or the block list, ManageSieve, and server-side validation through an API. The private `dev/rules` API exists but is off limits by policy.

**Degraded design.** The paste is one step per change. Two ideas reduce how often it is needed:

1. **Data-driven rules through contact groups (inferred, needs verification).** Fastmail says `X-Spam-known-sender` can contain "one of the contact group UUIDs" (verified: [fastmail-rules-sieve.md](fastmail-rules-sieve.md), from the delivery process page). If that holds for any group, a rule pasted once can test `header :contains "X-Spam-known-sender" "<group uid>"`. After that, mejl changes who the rule applies to by editing the group's members with JMAP Contacts, with no new paste. This would power a Screener (first-time senders go to a Screener mailbox, approve adds to contacts), a "Feed" group and a mejl block group. The verification step is in the README.
2. **JSON export as a read path (needs a spike).** Fastmail exports rules and Sieve as JSON (verified there). If the format is stable, mejl can show the user's real UI rules read-only. Import tests run only on a trial account.

If Fastmail ever exposes RFC 9661, the same rule model is pushed through a thin `SieveScript` adapter that still edits only mejl's marked region. That adapter is not built until the capability appears (YAGNI).

**Verdict: partial.** "Seamless" two-way rules management is not possible without Fastmail's cooperation. mejl can make authoring excellent and the paste step short.

### Masked Email

**Possible today** (verified API in [fastmail-masked-email.md](fastmail-masked-email.md)): full list with `MaskedEmail/get` and `ids: null`; create with `state: enabled` (never leave a mask `pending`, it is deleted after 24 hours); update `state` and `description`; handle `rateLimit` on create.

**Unverified:** API destroy and API restore (`deleted` to `enabled`), `/changes` and `/query` behaviour, push for masks, the value of the `X-FM-Masked-Email` header, and sending from a mask over JMAP.

**Degraded design** (inferred):

- Sync: full refetch at startup, after any mejl write and on a timer, then a local diff. No reliance on `/changes`.
- Linking messages to masks: a local `address -> mask id` map matched against `to`, `cc` and the masked header. Masks with no header match still link through `to`/`cc`.
- Send-as-mask: offered only after the live test passes. Until then, replying to mail that reached a mask shows "reply from this address works in Fastmail web" with an open-in-browser key, instead of sending from the wrong identity.
- Basic plan users have Masked Email but no API tokens (verified). Whether OAuth works for them is unknown. And Masked Email has no interoperable OAuth scope in the open metadata (verified). So the API-token login stays a first-class path for mask features.

**Verdict: yes, with gaps.** Management works. Reply-from-mask is the one headline feature that may not be possible over JMAP.

### Instant UI

The terminal itself adds 5 to 45 ms of keypress-to-screen latency (verified: Dan Luu, cited in [terminal-ux.md](terminal-ux.md); dated 2017). Nielsen's "instantaneous" limit is 0.1 s and Superhuman applies the same 100 ms rule, aiming under 50 ms (verified there). So mejl's own share must fit in one frame. This works only if:

- Every frame is built from in-memory state and bounded, indexed reads of the local store (2 ms p99 budget), never from the network.
- Every mutation is optimistic.
- Bodies of likely next messages are prefetched and pre-rendered at the current width.
- The message list is a custom virtual list (ratatui's `List` builds every item, verified there).

**Verdict: yes by design, unproven.** The budgets below and the measurement plan are how the claim gets proved or dropped.

### Keyboard-first with mouse convenience

- Kitty keyboard protocol is widely supported, but stock tmux 3.8 turns off extended keys, focus events and app clipboard writes (verified: [terminal-ux.md](terminal-ux.md)). So the default keymap avoids keys that legacy encoding cannot deliver, and mejl documents a tmux snippet.
- crossterm's `EnableMouseCapture` turns on any-motion tracking (verified there). mejl writes its own mode set (1000, 1002, 1006) and coalesces motion.
- ratatui has no hit testing. mejl records `(Rect, target)` regions per frame (about 100 lines, inferred there).

**Verdict: yes.** No research finding blocks it.

## System architecture

### Process and threads

One binary, one process per user session (inferred). Threads:

```
 stdin --> [input thread] --Event(ts)--+
                                       v
                         [UI thread: owns terminal]
                          drain channel -> keymap -> Action
                          update(&mut State, Action) -> Vec<Cmd>
                          if dirty: render (ratatui) inside sync-update, flush
                              |  Cmd                 ^  Msg
                              v                      |
 [tokio multi-thread runtime] -----------------------+
   session, JMAP requests, EventSource push, outbox flusher,
   body fetch and prefetch, server search, masked email refresh
   spawn_blocking: html2text render, image decode
                              |  StoreCmd            ^  StoreMsg
                              v                      |
 [store thread: owns the SQLite writer connections]
   apply /changes batches, FTS indexing, outbox persistence
```

- **Input thread.** Blocking `crossterm::event::read`, stamps each event with `Instant::now()` at read time, forwards it to the UI channel. The timestamp is the start of the latency measurement (inferred; Superhuman starts its timer at the OS event, verified in [email-clients.md](email-clients.md)).
- **UI thread.** A plain loop, no async. Waits on one channel that carries input events and messages from the runtime and the store. It drains every pending message, applies them, then renders at most once. Holding `j` costs one frame per refresh, not one per key (inferred there).
- **tokio runtime.** All network work. At most a few requests in flight, well below the session's `maxConcurrentRequests` (10 in published dumps).
- **Store thread.** Owns the SQLite write connections and does every write, FTS update and large query. SQLite runs in WAL mode, so readers do not block on the writer.
- **UI reads.** The UI thread keeps a read-only connection for small, indexed page reads (one screen of rows by `(mailbox, received_at)`). They must stay under 2 ms p99. If benchmarks show they do not, the page reads move to the store thread and the list shows skeleton rows for misses (inferred; decide with the divan benchmark below).
- crossterm's `event-stream` feature from the verified starter set is not needed by this design. Keeping it costs little (inferred).

### Event loop

Elm-style, as documented by ratatui ([terminal-ux.md](terminal-ux.md)):

1. Block on the channel. Then drain everything pending without blocking.
2. For each item: input goes through the keymap into an `Action`. Runtime and store results arrive as `Msg`. `update(&mut State, ...)` changes state and returns `Cmd` values. It never awaits.
3. Send `Cmd`s to the runtime or the store.
4. If state is dirty, render once: `BeginSynchronizedUpdate`, `terminal.draw`, `EndSynchronizedUpdate`, flush a `BufWriter`. ratatui's `draw` does not wrap frames itself (verified there).
5. Record `input_to_flush` for every input event handled in this frame.

No fixed-rate render timer. Spinners and toasts schedule their own wake-ups. An idle mejl does no work (inferred).

### State store

Two SQLite files per account, in the platform data and cache directories from `etcetera` (inferred):

| File | Contents | Safe to delete? |
|---|---|---|
| `cache.db` | Session copy, state strings, mailboxes, emails, threads, identities, contacts, masked emails, bodies, FTS index, query caches, rendered-text cache | Yes. mejl refetches |
| `state.db` | Outbox, local draft copies, mejl rules and their render history, undo stack, UI state (last mailbox, pane ratios) | No. It holds user intent |
| `caps.toml` (cache dir) | Cached terminal capabilities (kitty keyboard, sync 2026, theme reports, XTVERSION name) keyed by `TERM`, `TERM_PROGRAM`, `TERM_PROGRAM_VERSION`, multiplexer and SSH. XTVERSION only checks the entry is not from another terminal. Only a complete probe writes it. mejl still probes on every start; the cache fills gaps a timed-out probe left | Yes |

The rule model may later sync between machines through Fastmail Files over WebDAV. That needs an app password, which is a second credential (verified need: [fastmail-rules-sieve.md](fastmail-rules-sieve.md)). Not in v1.

### JMAP client

A thin client on reqwest and serde, about 1.5k to 3k lines (inferred estimate from [rust-crates.md](rust-crates.md)):

- `trait Method { const NAME; type Args; type Response; const CAPABILITIES; }`, one impl per method, so vendor methods like `MaskedEmail/get` are ordinary.
- A request builder that hands out typed result references for `#ids` back-references, and computes `using` from the methods in the batch. Always include `urn:ietf:params:jmap:core` in `using` (verified advice: [fastmail-masked-email.md](fastmail-masked-email.md)).
- Object structs keep unknown fields in a flattened map, so new Fastmail properties never break parsing.
- A `Transport` trait with the reqwest implementation and an in-process fake for tests. The fake is the second implementation that justifies the trait (inferred).
- Reference and test oracle: Stalwart's jmap-client 0.4.3 (MIT/Apache, copy with attribution). Not a dependency, because its method enum is closed and it has no MaskedEmail (verified: [rust-crates.md](rust-crates.md)).

### Sync engine

**Session.** Load the cached session for the first frame. Fetch the live session in the background. Refetch it whenever a response's `sessionState` differs (verified mechanism: [fastmail-platform.md](fastmail-platform.md)).

**Per-type strategy** (inferred unless marked):

| Type | Strategy |
|---|---|
| Mailbox | `Mailbox/changes` from the stored state. `Mailbox/get` all on `cannotCalculateChanges` |
| Email | `Email/changes` with `maxChanges`, then `Email/get` of `#created` (list properties) and `#updated` (`mailboxIds`, `keywords`), looping on `hasMoreChanges` |
| Thread | `Thread/changes`, `Thread/get` for changed ids |
| Open views | `Email/queryChanges` per cached query with the same `collapseThreads` value as the original (verified rule: RFC 8621 via [fastmail-platform.md](fastmail-platform.md)). Refetch the query on `cannotCalculateChanges` |
| Identity | `Identity/changes`, or `Identity/get` all (the set is tiny) |
| Contacts | `ContactCard/changes` for autocomplete and the allow-list features |
| MaskedEmail | Full `MaskedEmail/get` with `ids: null` and a local diff. Reported `cannotCalculateChanges` on `/changes` and an empty `state` (unverified: maton notes in [fastmail-masked-email.md](fastmail-masked-email.md)) |
| VacationResponse | `VacationResponse/get` on demand, only with an OAuth grant that has the scope |

**Cold start with an empty cache.** One batched request: `Mailbox/get`, `Identity/get`, `Email/query` on the inbox (`collapseThreads: true`, newest first, limit about three screens), `Email/get` by back-reference with list properties only, `Thread/get` by back-reference, and `MaskedEmail/get` when that capability is present. Store every `state` and `queryState`. Then fill older mail per mailbox in background pages of up to `maxObjectsInGet` (inferred, after [fastmail-platform.md](fastmail-platform.md)).

**Warm start.** Render from cache. Then one batched sync request: all `/changes` calls, `Email/queryChanges` for the visible list, and the mask refetch. Six to eight calls fit well under `maxCallsInRequest` (50 in published dumps).

**List properties fetched per email** (inferred): `id`, `threadId`, `mailboxIds`, `keywords`, `from`, `to`, `cc`, `subject`, `receivedAt`, `size`, `preview`, `hasAttachment`, and `header:X-FM-Masked-Email:asText`. Because of the header-name disagreement below, the first build also asks for `header:Fastmail-MaskedEmail:asText` until the live test settles which one is present on stored mail.

**Bodies.** `Email/get` with `bodyValues`, `fetchTextBodyValues`, `fetchHTMLBodyValues` and a `maxBodyValueBytes` cap when a message is opened. Prefetch the next and previous few in the direction of travel, and pre-render them at the current width. Cancel stale prefetches when the user jumps (inferred).

**Push.**

- One EventSource connection per account, opened after the first frame.
- Build the URL from the session's `eventSourceUrl`. If it is a URI template, expand `types=*`, `closeafter=no`, `ping=<n>`. If it has no template variables, append those as query parameters (resolves the disagreement below).
- The parser accepts the RFC shape (`@type: StateChange`) and the reported non-standard shape (`type: connect`), and reads only the `changed` map (verified quirk report: [fastmail-platform.md](fastmail-platform.md)).
- Any event triggers one debounced sync (coalesce events that arrive within about 100 ms).
- mejl's own idle timeout (about 5 minutes) reconnects even if server pings never arrive. Reconnect with exponential backoff and jitter. Always run a full sync after reconnect, because events may have been missed.
- If push cannot connect, fall back to a poll timer and show "push off" in the status line.
- WebSocket (RFC 8887) only if a live session ever advertises `urn:ietf:params:jmap:websocket`. Not built until then.

**Backoff and limits.** Treat HTTP 429 and 503 with `Retry-After`, the JMAP `limit` request error and `rateLimit` SetErrors as backoff signals. Fastmail publishes no rate limits (verified: [fastmail-platform.md](fastmail-platform.md)).

### Local cache and search index

- rusqlite 0.40.2 with `bundled`, which compiles in FTS5 (verified: [rust-crates.md](rust-crates.md)). WAL mode.
- One global email table keyed by JMAP email id, with mailbox membership as a separate relation. JMAP ids are global and immutable, so no per-folder keys (inferred, after Thunderbird's Panorama rewrite in [email-clients.md](email-clients.md)).
- An FTS5 external-content table over subject, sender names and addresses, preview and the text of bodies the user has fetched.
- tantivy 0.27.0 is the upgrade path only if FTS5 ranking or speed falls short at 1M messages (benchmark first).
- Body blob cache is size-capped with LRU eviction, and entries are dropped when the server destroys the email. aerc's blob cache that "is never purged automatically" is the anti-pattern (verified there).

**Search behaviour** (inferred):

1. As the user types, query the local FTS index and show results within one frame. Label them "local".
2. In parallel, send `Email/query` with the mapped filter plus `SearchSnippet/get` by back-reference. Merge server results when they arrive and relabel "server". The server is the source of truth for full-text body and attachment search.
3. Map Fastmail search syntax to JMAP filters where a mapping exists (table in [fastmail-platform.md](fastmail-platform.md)). For operators with no JMAP equivalent (`fromin:`, `memo:`, `filename:`), say so instead of guessing.
4. JMAP `to` matches only the To header, unlike Fastmail web's `to:`. mejl expands `to:` to an `OR` of `to`, `cc` and `bcc` to match Fastmail's meaning (inferred, from the verified difference).

### Optimistic mutations with rollback

The pattern combines Superhuman's modifiers (synchronous local `modify`, idempotent queued `persist`) and Fastmail web's patch change log with last-write-wins (verified: [email-clients.md](email-clients.md)). The mejl version (inferred):

1. A user action creates an outbox entry in `state.db`: account, target ids, a JMAP patch (`keywords/$seen: true`, `mailboxIds/<id>: null`), the inverse patch, a sequence number and a status. The entry is written by the store thread, which owns the writer, so it lands one store-thread round trip after the key press (usually a millisecond or two).
2. The action shows in the same frame through two overlays, one after the other (as built):
   - **The UI thread's pending overlay.** Before it sends the command, the binary reads the affected conversations through its own Reader (every cached message with keywords and mailboxes), computes the patched flags with the same `mejl_store::overlay::EmailFlags` rules the store uses, and keeps a before and after snapshot under the command's token. Every read the UI makes applies the newest snapshot of each conversation: flags change, rows whose messages all left the mailbox are hidden (page offsets, totals and positions shift to match), and mailbox counts move by each entry's after-minus-before. Masked Email changes and `* a` actions on a whole mailbox get simpler entries of the same kind.
   - **The store's overlay.** When the store thread has queued the entry and materialised its overlay in `cache.db`, the engine sends `Applied` for the token; on `Rejected` nothing was queued. Either way the binary drops its snapshot and re-reads, in the same turn of the loop, so no frame shows neither overlay.
   - From then on views read cache rows with all pending overlays applied, so a `/changes` result that arrives before the flush does not make the row flicker back.
   - Undo and redo are not predicted on the UI thread: the store computes the inverse, and the result shows one store round trip later.
3. The outbox flusher batches entries into one `Email/set` (up to `maxObjectsInSet`). Repeat edits to the same object collapse into one patch, and an edit that ends as a no-op is dropped. Creates are ordered before the entries that depend on them (for example create a mailbox, then move mail into it).
4. Patches, not whole-object writes, and no `ifInState`. Edits to different properties merge on the server, matching Fastmail web's policy (inferred choice).
5. On success: mark the entry done. Drop the overlay once the next sync shows the server state.
6. On `notUpdated` or a request error: drop the overlay, so the row shows server truth again, and show a toast with the error and a retry key. Permanent failures are removed from the outbox, as Superhuman does.
7. Undo (`z`) pushes the inverse patch as a new optimistic mutation. The undo stack keeps the last 10 actions, like Fastmail web (verified there).
8. Mask state changes and contact changes use the same outbox with `MaskedEmail/set` and `ContactCard/set`.

### Sending and undo send

- Compose saves a local draft in `state.db` on every typing pause (about 1 s) and a server draft (`Email/set` into Drafts) at most every 10 seconds, on blur, on close and on the save key. An update creates the new copy and destroys the old one in one request. Sent or discarded drafts have their server copies destroyed (durably, through a `draft_trash` table). The `draft` table also holds `upload_json`, `upload_rev`, `uploaded_rev`, `upload_in_flight` and `server_message_id` (migration STATE_V3). Accounts with no Drafts mailbox keep drafts on the device. The local copy is written first, atomically (inferred, from [terminal-ux.md](terminal-ux.md)).
- Send creates the email and the `EmailSubmission` in one request, with `onSuccessUpdateEmail` moving it from Drafts to Sent (verified pattern: [fastmail-platform.md](fastmail-platform.md)).
- **Undo send, v1: a client-side hold.** Sends sit in the durable outbox for a configurable delay (default 10 s) with an undo bar. On quit during a hold, mejl asks "send now or keep queued". Queued sends go out on the next start (inferred).
- **Undo send, later: a server-side hold**, only if the live test shows `HOLDFOR` works despite the empty `submissionExtensions` (unverified). It would survive a crash and use `undoStatus: canceled`.
- No client-side scheduled send in v1 (see plan-gated features).

### Offline behaviour

- Reading, searching cached mail, triage and composing all work offline from the cache and the outbox (inferred).
- The status line shows "offline, 3 changes pending". Sends made offline wait in the outbox with an explicit "queued" badge.
- On reconnect: flush the outbox, then run a full sync, then reconnect push.
- Masked Email create needs the server (the address is server-generated). Offline, mejl says so instead of queueing it.

### HTML rendering

From [terminal-ux.md](terminal-ux.md) and [rust-crates.md](rust-crates.md) (inferred pipeline on verified crate features):

1. Prefer `text/plain` when it is real. Fall back to HTML when the plain part is very short or only a URL. One key toggles.
2. Optional pre-clean with lol_html (remove tracking pixels, hidden preheaders, `<style>`).
3. html2text 0.17.4 `config::rich()` with link footnotes, on `spawn_blocking`, with a time and size limit and a panic catch. Recent html2text releases fixed panics and an infinite loop (verified there).
4. Map rich annotations to ratatui styles. Ignore author colours by default.
5. Cache rendered lines by `(email id, width, theme)`.
6. Remote images blocked by default. Never loaded in Spam. "Open in browser" writes sanitized HTML (ammonia) to a private temp file with a CSP that blocks remote loads.

## Data model sketch

Abbreviated DDL for `cache.db` and `state.db` (inferred design):

```sql
-- cache.db: everything here can be rebuilt from the server
CREATE TABLE account (id TEXT PRIMARY KEY, username TEXT, session_json TEXT, session_state TEXT);
CREATE TABLE sync_state (account_id TEXT, type TEXT, state TEXT, PRIMARY KEY (account_id, type));

CREATE TABLE mailbox (
  account_id TEXT, id TEXT, parent_id TEXT, name TEXT, role TEXT, sort_order INTEGER,
  total_emails INTEGER, unread_emails INTEGER, total_threads INTEGER, unread_threads INTEGER,
  my_rights_json TEXT, is_subscribed INTEGER, PRIMARY KEY (account_id, id));

CREATE TABLE email (
  account_id TEXT, id TEXT, thread_id TEXT, received_at INTEGER, size INTEGER,
  from_json TEXT, to_json TEXT, cc_json TEXT, subject TEXT, preview TEXT,
  has_attachment INTEGER, keywords_json TEXT,
  is_seen INTEGER, is_flagged INTEGER, is_answered INTEGER, is_draft INTEGER,  -- hot keywords as columns
  masked_header TEXT, masked_email_id TEXT,                                     -- link to masked_email
  PRIMARY KEY (account_id, id));

CREATE TABLE email_mailbox (account_id TEXT, email_id TEXT, mailbox_id TEXT, received_at INTEGER,
  PRIMARY KEY (account_id, mailbox_id, email_id));
CREATE INDEX email_mailbox_by_date ON email_mailbox (account_id, mailbox_id, received_at DESC);

CREATE TABLE thread (account_id TEXT, id TEXT, email_ids_json TEXT, PRIMARY KEY (account_id, id));

CREATE TABLE body (account_id TEXT, email_id TEXT, part_id TEXT, mime_type TEXT, text TEXT,
  fetched_at INTEGER, PRIMARY KEY (account_id, email_id, part_id));
CREATE TABLE search_doc (rowid INTEGER PRIMARY KEY, account_id TEXT, email_id TEXT,
  subject TEXT, from_text TEXT, preview TEXT, body_text TEXT);                  -- one row per email
CREATE VIRTUAL TABLE email_fts USING fts5(subject, from_text, preview, body_text,
  content='search_doc', content_rowid='rowid', tokenize='unicode61');           -- external content

CREATE TABLE query_cache (account_id TEXT, query_key TEXT, query_state TEXT,
  collapse_threads INTEGER, total INTEGER, ids_blob BLOB, updated_at INTEGER,
  PRIMARY KEY (account_id, query_key));

CREATE TABLE identity (account_id TEXT, id TEXT, name TEXT, email TEXT, reply_to_json TEXT,
  text_signature TEXT, html_signature TEXT, PRIMARY KEY (account_id, id));

CREATE TABLE contact (account_id TEXT, id TEXT, uid TEXT, kind TEXT, name TEXT,
  emails_json TEXT, member_uids_json TEXT, PRIMARY KEY (account_id, id));        -- kind 'group' for groups

CREATE TABLE masked_email (account_id TEXT, id TEXT, email TEXT, state TEXT, for_domain TEXT,
  description TEXT, url TEXT, created_by TEXT, created_at INTEGER, last_message_at INTEGER,
  PRIMARY KEY (account_id, id));
CREATE UNIQUE INDEX masked_by_address ON masked_email (account_id, lower(email));

CREATE TABLE rendered (account_id TEXT, email_id TEXT, width INTEGER, theme TEXT, lines_blob BLOB,
  PRIMARY KEY (account_id, email_id, width, theme));

-- state.db: user intent, never deleted by "clear cache"
CREATE TABLE outbox (seq INTEGER PRIMARY KEY, account_id TEXT, created_at INTEGER,
  kind TEXT,               -- email_patch, email_create, submission, masked_set, contact_set
  target_ids_json TEXT, patch_json TEXT, inverse_json TEXT,
  status TEXT,             -- pending, in_flight, done, failed
  hold_until INTEGER,      -- undo-send hold
  attempts INTEGER, last_error TEXT);

CREATE TABLE draft (account_id TEXT, local_id TEXT PRIMARY KEY, server_email_id TEXT,
  headers_json TEXT, body TEXT, format TEXT, updated_at INTEGER);

CREATE TABLE rule (id TEXT PRIMARY KEY, name TEXT, enabled INTEGER, sort_order INTEGER,
  combinator TEXT, filter_json TEXT, actions_json TEXT, section TEXT, updated_at INTEGER);
CREATE TABLE rule_render (rendered_at INTEGER PRIMARY KEY, section TEXT, sieve TEXT,
  body_sha256 TEXT, user_confirmed_paste_at INTEGER);

CREATE TABLE undo (seq INTEGER PRIMARY KEY, outbox_seq INTEGER, label TEXT);
```

Notes (inferred):

- The rule model borrows the shape of Fastmail's undocumented `Rule` object (filter as a JMAP Email filter, `fileIn` as a mailbox id) because it maps one to one onto Fastmail's UI concepts ([fastmail-rules-sieve.md](fastmail-rules-sieve.md)). mejl never calls that API.
- The FTS table uses external content, as [rust-crates.md](rust-crates.md) recommends. That keeps `snippet()` and `highlight()` for local results and lets deletes from `/changes` work without newer SQLite features. `search_doc` may later replace the `body` text column so bodies are not stored twice. A contentless table is the alternative if the benchmark shows a clear win.
- `query_cache.ids_blob` holds the sorted id list for the virtual list. 100k ids is about 1 to 3 MB (inferred in [terminal-ux.md](terminal-ux.md)).
- Schema changes use numbered migrations. `cache.db` may simply be dropped and rebuilt on a major schema change. `state.db` must migrate.

## Auth approach

### Logins

| Path | Status | Use |
|---|---|---|
| API token paste | Works today on Standard and Professional. Not on Basic (verified: [fastmail-platform.md](fastmail-platform.md)) | v0 and onward. Required scopes: Email, Email submission, Contacts, Masked Email. mejl shows the checklist and a direct link to the token settings page |
| OAuth, open dynamic registration | Discovery metadata advertises `/oauth/register`, PKCE S256, auth method `none`, scopes `urn:ietf:params:oauth:scope:mail`, `...:contacts`, `offline_access` (verified: curl, 2026-10-23). Registration itself untested live (unverified) | Built: `mejl login --oauth`, the default choice in `mejl login` |
| OAuth, manually registered client | Documented: clients "are currently registered manually by contact with Fastmail developers" (verified) | Fallback if open registration fails, or to get the `dev/maskedemail` scope |
| Browser cookies, private APIs | Forbidden by policy | Never |

### OAuth flow (v1, inferred design on verified requirements)

1. Discover from the session's `401` `WWW-Authenticate` `resource_metadata` link, then the authorization server metadata.
2. Register once per device (RFC 7591) as a public client. Fastmail says each device must get its own authorization (verified).
3. Redirect to `http://127.0.0.1:<random port>/`. The dev docs allow `localhost`, `127.0.0.1` or `::1`; the open profile draft allows only `127.0.0.1` or `[::1]`. `127.0.0.1` satisfies both (resolves a disagreement below).
4. Open the system browser with `open`. Never an embedded web view (the draft forbids it, verified).
5. PKCE S256, check `iss` (RFC 9207), exchange the code.
6. Refresh tokens rotate on every use, and reusing an old one revokes the grant (verified). So: serialize refreshes behind one mutex, write the new refresh token to the keyring and fsync before using the new access token, and never retry an old refresh token. The draft says servers should keep the old token valid until the new one is used, which softens a lost response, but Fastmail's behaviour is untested (unverified).
7. Headless SSH: no device flow exists (verified). Offer token paste, or loopback through SSH port forwarding.
8. The token exchange is hand-rolled on reqwest. The `oauth2` crate's reqwest integration pins reqwest 0.12 (verified: [rust-crates.md](rust-crates.md)).

As built (2026-10-25):

- `mejl_jmap::oauth` holds discovery, registration, PKCE, the authorize URL, code exchange, refresh and revoke over the `Transport` trait. `mejl_jmap::credentials` puts a self-refreshing OAuth provider inside `Client`: it refreshes 60 s before expiry and once on a 401 (not a 403), single-flight, re-reads the stored login before each refresh to adopt another process's newer one, saves before it uses a new token, retries a lost refresh response once with the same refresh token, and never sends a spent refresh token.
- Only the TUI and `mejl login` refresh. `doctor` and `logout` read the stored record; `logout` revokes.
- The login lives in the keyring as compact JSON under the user `<account>:oauth`. An OAuth login and an API token login replace each other.
- A temporary `invalid_grant` keeps mejl waiting and retrying; any other refusal asks for `mejl login`.
- A stored, dynamically registered client id is checked first with a junk refresh token: `invalid_client` means register again (a heuristic; the authorize page shows an error page instead of redirecting).

Masked Email over open-registered OAuth is an open question: `dev/maskedemail` is not in `scopes_supported` (verified). If it cannot be requested, mejl keeps a token login for mask features or asks Fastmail for a registered client.

### Secret storage

- keyring 4.2.0 with the default `v1` feature: macOS Keychain, Windows, Secret Service (verified: probe build). Tokens held in memory as `secrecy::SecretString` 0.10.3, never in logs.
- Fallback for systems with no Secret Service (headless Linux, containers): a `token_command` in config, for example `pass show fastmail/mejl`, read at startup (inferred, after aerc-style credential commands).
- No plain-text token in the config file. If the user insists, mejl refuses and points to `token_command` (inferred).
- `mejl logout` revokes the OAuth grant, deletes keyring entries, `cache.db` and, after a confirm, `state.db`. This also covers the Developer Policy's data-deletion duty for a local app (inferred reading of the 14-business-days rule in [fastmail-platform.md](fastmail-platform.md)).

## Crate picks

All versions from [rust-crates.md](rust-crates.md) (verified there against crates.io on 2026-10-23, and built together in a probe crate unless marked).

| Area | Crate and version | Note |
|---|---|---|
| TUI | ratatui 0.30.2, crossterm 0.29.0 (`osc52`) | MSRV 1.88 for ratatui 0.30.2 |
| Text editing | mejl's own body editor (`mejl-ui` `compose/editor.rs`), tui-input 0.15.5 | Built after ratatui-textarea re-wrapped the whole buffer on every edit (10 ms per key at 5000 lines). ratatui-textarea stays as a dev-dependency, the reference its tests compare against |
| Sidebar tree | tui-tree-widget 0.24.1 | |
| Runtime and HTTP | tokio 1.54.1, futures 0.3.34, reqwest 0.13.5 (rustls, http2, json, stream, gzip) | aws-lc-rs needs a C toolchain. ring not measured |
| Push | sse-stream 0.3.0 | Not reqwest-eventsource, which pins reqwest 0.12 |
| JSON | serde 1.0.229, serde_json 1.0.154 | |
| Mail | mail-parser 0.11.9, linkify 0.11.0. Later: mail-builder 1.0.0 | JMAP parses bodies server-side. mail-parser is for raw source and attached messages |
| HTML | html2text 0.17.4. Later: lol_html 3.0.2, ammonia 4.2.4 | |
| Store and search | rusqlite 0.40.2 (`bundled`, FTS5). Later: tantivy 0.27.0 | |
| Fuzzy | nucleo-matcher 0.3.1 | MPL-2.0, fine unmodified |
| Config and paths | toml 1.1.8, etcetera 0.11.0. Later: toml_edit 0.25.17, clap 4.6.7, crokey 1.5.1 | |
| Secrets | keyring 4.2.0, secrecy 0.10.3 | keyring costs about 3 MB and 65 crates (measured) |
| Dates | jiff 0.2.38 (`serde`) | |
| Errors and logs | anyhow 1.0.104, thiserror 2.0.21, tracing 0.1.44, tracing-subscriber 0.3.23, tracing-appender 0.2.5 | |
| OS integration | open 5.4.5. Later: arboard 3.7.0, notify-rust 4.18.2 | OSC 52 first, arboard as fallback |
| Images (later) | ratatui-image 11.1.0 with `default-features = false` | Default features need libchafa |
| Tests and benches | insta 1.49.0, divan 0.1.21, ratatui `TestBackend`. Later: vt100 0.18.1 | |
| Profiling | tracing-chrome 0.7.2, tracing-tracy 0.12.0 | Versions from [terminal-ux.md](terminal-ux.md), not in the probe |
| Markdown compose | comrak 0.57.0 or pulldown-cmark 0.13.5 | Versions from [terminal-ux.md](terminal-ux.md), not in the probe |

**Excluded:** jmap-client (closed method enum, no MaskedEmail; reference only), sieve-rs (AGPL-3.0-only; only if mejl is AGPL), oauth2 (reqwest 0.12 pin), tui-textarea, reqwest-eventsource, chrono as a direct dependency (verified reasons in [rust-crates.md](rust-crates.md)).

**Workspace layout** (inferred): `mejl-jmap` (client and types), `mejl-store` (SQLite, sync application, outbox), `mejl-sieve` (rule model, renderer, allowlist checks), `mejl-ui` (widgets, keymap, layout, hit testing), `mejl` (binary). UI edits then do not recompile the JMAP types. The probe measured a 0.5 s incremental debug build (verified there).

**License recommendation** (decision for the user): MIT OR Apache-2.0. It matches the whole starter set and leaves sieve-rs out. Sieve work then uses a small in-house tokenizer for mejl's own subset, or one of the permissive parsers after an evaluation (inferred).

## Performance budgets

Budgets are targets (inferred), built from the verified reference points in [terminal-ux.md](terminal-ux.md) and [email-clients.md](email-clients.md).

| Metric | Budget | Measured how |
|---|---|---|
| Warm start: exec to first useful frame (cached mailbox list and message rows) | p95 under 50 ms | pty harness with vt100: time from spawn to the first flush that contains list rows |
| Cold disk start (OS page cache dropped) | p95 under 150 ms | Same harness after dropping caches |
| First run ever (empty cache) | UI shell under 150 ms. Inbox rows after two round trips (session, one batched request) | Same harness against the mock server with injected latency |
| Local action (move, archive, mark, pin, select): event read to bytes flushed | p99 under 8 ms, p50 under 2 ms | `input_to_flush` span per event |
| Scroll the list of a 100k-message mailbox | Frame p99 under 8 ms | `TestBackend` benchmark with synthetic data |
| Open a message with cached rendered text | p99 under 16 ms | `input_to_flush` |
| Open a message with cached body, not yet rendered | p95 under 50 ms (Superhuman's internal target) | `input_to_flush` plus the render span |
| Local search per keystroke, 100k cached messages | p95 under 16 ms | divan bench on FTS5 queries |
| Palette fuzzy match per keystroke | under 1 ms | perf test of mejl-ui's own fuzzy matcher (nucleo-matcher is MPL-2.0) |
| Network actions | Local effect within the local-action budget. Server time shown only in the status line | Outbox spans |
| Idle | No timer wake-ups when nothing changes | Count wake-ups in a 60 s idle trace |
| Memory | RSS under 100 MB with 100k cached messages | `/proc/self/status` in the bench harness |
| Binary | Track. Starter set is 14 MB stripped, 8.8 MB with `opt-level = "s"` (measured) | CI artifact size |

How to measure (inferred, after Superhuman's method in [email-clients.md](email-clients.md)):

- Start the timer at input read time (input thread timestamp). Stop it when `Backend::flush` returns.
- Report the share of events in buckets: under 8 ms, under 16 ms, under 50 ms, under 100 ms, worse. The smallest bucket shows regressions first.
- Keep a ring buffer of the last 1,000 frames. A `:perf` overlay shows p50, p95, p99 and the slowest recent frames.
- A synthetic data generator builds 100k and 1M message stores with realistic subject lengths, thread sizes and mailbox spread.
- CI fails if a benchmark's p99 regresses past its budget. Network latency is never part of a local-action budget.
- The terminal's own latency (5 to 45 ms, dated measurement) is outside mejl's control and outside these budgets.

## Testing strategy

| Layer | What | How |
|---|---|---|
| Wire types | Serde round trips of every JMAP type, unknown fields preserved | Fixtures: the redacted live session from the first verification step, RFC examples, jmap-client test cases (copied with attribution) |
| JMAP client | Request building, back-references, `using` computation, error mapping | Unit tests against the fake `Transport` |
| Sync engine | `/changes` loops, `hasMoreChanges`, `cannotCalculateChanges`, query changes, session state change, reconnect resync | An in-process mock JMAP server with a state counter per type. Property tests: random server edits plus random client actions must converge to the same state |
| Fastmail quirks | EventSource without template variables and with them, the `type: connect` event, MaskedEmail with empty `state` and failing `/changes`, `rateLimit` on create | The same mock server, with switches for each reported quirk |
| Outbox | Overlay correctness, collapse, ordering, rollback, undo, crash recovery | Kill the store thread mid-flush in tests. Replay `state.db` |
| Standards server | Real JMAP behaviour for core, mail, submission, RFC 9661 | Stalwart run as an external binary or container. It implements RFC 9661 and ManageSieve (verified: [fastmail-rules-sieve.md](fastmail-rules-sieve.md)). It does not reproduce Fastmail's MaskedEmail API (its masks are `x:MaskedEmail` and Enterprise only) or Fastmail's push quirks (verified: [fastmail-masked-email.md](fastmail-masked-email.md)). Running it as a separate process keeps its AGPL away from mejl's code (inferred) |
| Live contract tests | A small read-only suite against Fastmail | Opt-in, a dedicated test account and test token only. Never the user's real mailbox and never through the Fastmail MCP tools. Write tests only on a trial account |
| Views | Every screen at 80, 120 and 200 columns, light and mono themes | ratatui `TestBackend` plus insta snapshots (verified to work: [rust-crates.md](rust-crates.md)) |
| Input | Keymap conflicts, legacy-encoding reachability of every default binding, mouse hit tests | Unit tests that feed crossterm events and assert `Action`s. `mejl keys --check` uses the same code |
| End to end | Startup timing, raw-mode restore on panic, `$EDITOR` handoff | Run the real binary in a pty and read the screen with vt100 |
| Sieve renderer | Output is stable, uses only allowlisted extensions, never `keep` or `discard`, markers and hash correct | Golden files. Optional dry run with a permissive parser if the license allows |
| HTML | No panics, bounded time on hostile input | Fuzzing with `cargo fuzz` over the html2text step, plus a corpus of real newsletters |
| Performance | Budgets in the table above | divan benches and the pty harness, gated in CI |

## Phased roadmap

Each phase lists what it proves. A phase is done when its proof holds, not when its feature list is typed out.

### Phase 0: verify and decide (no app code)

- Run the verification steps in [README.md](README.md) with a test token or trial account: session capabilities, EventSource shape, open OAuth registration, `HOLDFOR`, send-as-mask, masked header names, keywords for pin, mute and follow, contact group UUIDs in `X-Spam-known-sender`.
- Write to Fastmail (partnerships@fastmailteam.com): is a third-party mail client fine under the compete clause, is token paste acceptable for a public release, will RFC 9661 or a rules scope come, can Masked Email be requested over open OAuth, does OAuth work on Basic.
- Decide: license, Basic-plan support, MSRV.
- **Proves:** which feasibility verdicts in this doc hold, and whether a public release is allowed.

### Phase 1: read fast (MVP)

- Token login, keyring, session, cold and warm sync, EventSource push, `cache.db`.
- Sidebar, virtual conversation list, reader with html2text, responsive layout, hint bar, `?` help, Fastmail keymap preset, mouse select and scroll.
- Triage with optimistic outbox and undo: archive, trash, spam, read and unread, pin, move, labels, select and bulk actions.
- `:perf` overlay and the benchmark harness.
- **Proves:** the instant-UI budgets on real data, and that the sync engine converges (property tests plus a week of daily use).

### Phase 2: write and find

- Compose with built-in editor and `$EDITOR` handoff, identities, address autocomplete from JMAP contacts, attachments, drafts, client-side undo send.
- Search: local FTS plus server `Email/query` and snippets, Fastmail syntax mapping.
- Command palette.
- **Proves:** mejl can replace the web app for daily mail, and search meets its per-keystroke budget.

### Phase 3: Masked Email

- Masks screen (`g m`), create inline in compose, `@` actions from a message, mask badge in list and reader, per-mask message list, stats from `lastMessageAt` and the local index, disable with undo, transactional-mail warning.
- Reply-from-mask only if the Phase 0 test passed.
- **Proves:** the first Fastmail-specific differentiator works end to end.

### Phase 4: rules

- Rule model in `state.db`, "rule from this message" (`F`), live preview, apply to existing mail, allowlist validation, marked-block renderer, copy and open, drift check by paste-back.
- Contact-group-driven Screener and block group, only if Phase 0 confirmed group UUIDs in `X-Spam-known-sender`.
- JSON export read-only view, only if the format spike succeeded.
- **Proves:** rules authoring is good enough that one paste per change is acceptable, or shows it is not.

### Phase 5: v1

- OAuth (open registration, or a registered client), vacation response if the grant covers it.
- vim preset, accessibility mode (cursor on focused row, no spinners), themes (terminal default, mono, high contrast, `NO_COLOR`), terminal capability probe and quirks table, tmux snippet in docs.
- Offline polish, `mejl logout`, crash reports, packaging.
- Public privacy policy, support contact and install instructions, as the Developer Policy requires (verified: [fastmail-platform.md](fastmail-platform.md)).
- **Proves:** mejl is releasable to other people within Fastmail's rules.

### Later (only on evidence)

- RFC 9661 adapter when Fastmail advertises `urn:ietf:params:jmap:sieve`.
- WebSocket push if advertised.
- Server-side undo send and scheduled send if `HOLDFOR` works and Fastmail is fine with it on the user's plan.
- CalDAV for invites (needs an app password).
- A `mejl --json` CLI over the same core, then maybe a local MCP server for what Fastmail's MCP lacks (masks, rules drafting). Fastmail's own MCP already covers general agent use (verified there).
- Inline images through ratatui-image.

## Risk register

| # | Risk | Likelihood | Impact | Mitigation | Trigger to act |
|---|---|---|---|---|---|
| 1 | Fastmail says a third-party client conflicts with "replicate or compete", or wants OAuth only | Low to medium (inferred) | Blocks public release | Ask in Phase 0. Keep mejl personal-use until answered | Fastmail reply |
| 2 | No rules API ever. Rules stay paste-only | High (verified today) | Headline goal stays partial | Marked block, contact-group data rules, JSON view. Keep the RFC 9661 adapter thin and ready | Session advertises `jmap:sieve` |
| 3 | Open OAuth registration does not work or lacks Masked Email | Medium (unverified) | Token paste stays the only login for masks | Keep token login first-class. Apply for a registered client | Phase 0 test |
| 4 | Refresh-token rotation logs users out after a crash mid-refresh | Medium | Annoyance, support load | Single refresh mutex, persist before use, never reuse | Logout reports |
| 5 | Basic-plan users cannot use mejl (no tokens, OAuth unknown) | High for tokens (verified) | Smaller audience | Detect and explain. Test OAuth on Basic | Phase 0 test |
| 6 | Send-as-mask is impossible over JMAP | Medium (unverified) | One mask feature lost | Web fallback key. Do not fake it with the wrong identity | Phase 0 test |
| 7 | EventSource quirks or silent drops | Medium (reported 2023) | Stale UI | Tolerant parser, own idle timeout, resync on reconnect, poll fallback | Push health metric |
| 8 | Outbox and overlay bugs lose or duplicate actions | Medium (inferred: hardest part) | Trust in the speed promise | Property tests with the mock server, crash-replay tests, idempotent patches | Any convergence failure |
| 9 | Instant budgets missed (SQLite page reads, html2text, wide lists) | Medium | "Ultrafast" claim fails | Benchmarks from Phase 1, move reads off the UI thread, pre-render, virtual list | CI budget failure |
| 10 | Generated-script layout, pipeline variables or require line change | Medium (undocumented) | Pasted block misbehaves | Never depend on `${spam}`, `${stop}` or section counts. Describe sections by position. Allowlist only shared extensions | User reports, Phase 0 script read |
| 11 | A pasted `stop` skips Fastmail's mute, notify, calendar and vacation blocks | Medium (inferred) | Surprising behaviour | No `stop` by default, warning when enabled | n/a |
| 12 | Rebuilding plan-gated features reads as circumvention | Low to medium (inferred) | Policy breach | No client-side snooze or scheduled send in v1 | Fastmail reply |
| 13 | Young or lagging crates (crossterm release gap since April 2025, ratatui OSC 8 and hit testing missing) | Medium | Delays terminal features | Own thin layers (hit test, sync wrap, footnote links). Watch termina and ratatui-layout | New releases |
| 14 | stock tmux hides kitty keys, focus, clipboard | High (verified defaults) | Degraded keys and copy | Legacy-safe keymap, tmux snippet, arboard fallback | n/a |
| 15 | html2text panics or hangs on hostile mail | Medium (fixed bugs seen) | Reader freeze or crash | Worker thread, time and size limits, panic catch, fuzzing | Fuzz findings |
| 16 | Remote content leaks IP or read time | High if not blocked | Privacy | Block by default, CSP on open-in-browser, never in Spam | n/a |
| 17 | Fastmail changes API behaviour without notice | Low to medium (reserved right, verified) | Breakage | Read everything from the session, unknown-field tolerant types, live contract tests | Contract test failure |
| 18 | License choice blocks a needed crate (sieve-rs AGPL) | Certain if MIT/Apache | Sieve parsing by hand | Small in-house tokenizer for mejl's subset, evaluate permissive parsers | License decision |
| 19 | Mask disable silently trashes password resets and receipts | Medium | User harm | Confirm, undo, warning on recent transactional mail | n/a |
| 20 | Scope creep (plugins, AI, other providers) | Medium | Loses the minimal goal | Non-goals list, YAGNI, two-real-uses rule for hooks | Feature requests |

## Where the research docs disagree

Each disagreement is resolved here or reported as open. Nothing was picked silently.

| # | Topic | What the docs say | Resolution |
|---|---|---|---|
| 1 | OAuth client registration | [rust-crates.md](rust-crates.md), [fastmail-masked-email.md](fastmail-masked-email.md) and [fastmail-rules-sieve.md](fastmail-rules-sieve.md) quote the dev docs: registration is manual. [fastmail-platform.md](fastmail-platform.md) found open dynamic registration in live discovery metadata, untested | Both are true at once: the docs and the server disagree. Token paste for v0. Target open registration for v1 after the Phase 0 test. Report the mismatch to Fastmail. **Open** until tested |
| 2 | Masked Email over OAuth | The dev docs list a `dev/maskedemail` OAuth scope. The open metadata's `scopes_supported` lacks it ([fastmail-platform.md](fastmail-platform.md)) | **Open.** Keep the token login first-class for masks until a registered or open client is shown to get the scope |
| 3 | Loopback redirect host | Dev docs allow `localhost`, `127.0.0.1` or `::1`. Draft-06 allows only `127.0.0.1` or `[::1]` | **Resolved:** use `127.0.0.1`, which both accept |
| 4 | `eventSourceUrl` shape and API host | [fastmail-platform.md](fastmail-platform.md): no template variables (2024 dump, `api.fastmail.com`). [fastmail-masked-email.md](fastmail-masked-email.md): a URI template with `{types}`, `{closeafter}`, `{ping}` and a regional `phl.api.fastmail.com` host (2026 maton dump, unverified) | **Resolved in code:** expand the template if present, else append parameters. Never hard-code hosts. **Open** which shape the live session has today |
| 5 | Masked email header name | Fastmail help and [fastmail-masked-email.md](fastmail-masked-email.md): `X-FM-Masked-Email`. Generated Sieve samples in [fastmail-rules-sieve.md](fastmail-rules-sieve.md) test `Fastmail-MaskedEmail` containing ` state=disabled` | **Open.** Possibly two headers, one added before Sieve and one stored (inferred guess). Fetch both on stored mail in Phase 0. The sync engine requests both until then |
| 6 | Does mail to a disabled mask pass through Sieve? | [fastmail-masked-email.md](fastmail-masked-email.md): unknown. [fastmail-rules-sieve.md](fastmail-rules-sieve.md): samples put the disabled-mask block after the top custom section, so top-section code runs (unverified samples) | **Partly resolved:** likely only the top section sees it. Confirm with one test on a trial account |
| 7 | Server-side Sieve validation | [rust-crates.md](rust-crates.md) assumes `SieveScript/validate` "if Fastmail offers" it. [fastmail-rules-sieve.md](fastmail-rules-sieve.md) shows no public validator, only the web Sieve tester by hand | **Resolved:** no server validation in the design. Local allowlist plus optional permissive parser, then the user's save in the web UI is the real check |
| 8 | "Rule from this message" and the Screener | [email-clients.md](email-clients.md) assumes mejl "writes Sieve" and builds a server-side Screener. [fastmail-rules-sieve.md](fastmail-rules-sieve.md) says writing is paste-only | **Resolved:** both go through the marked-block paste. The Screener's approve and block steps use contacts and contact groups, which are writable over JMAP (group-UUID mechanism unverified) |
| 9 | Compose editor default | [email-clients.md](email-clients.md): suspend the TUI and run `$EDITOR` by default. [terminal-ux.md](terminal-ux.md): built-in editor by default, `Ctrl-o e` hands off | **Resolved: built-in by default.** The header form, identity and mask picker must be built in anyway. Built-in avoids leaving the alternate screen and keeps compose inside the frame budget. `$EDITOR` is one key away and `compose.editor = "external"` makes it the default |
| 10 | Masked Email keys | [fastmail-masked-email.md](fastmail-masked-email.md) proposes `M` (masks view), `x` in the reader (disable), `y` (copy), `r` (edit), `e` (enable). [terminal-ux.md](terminal-ux.md) binds those keys to mute, select, archive, reply and expand, and uses `g m` and `@` for masks | **Resolved:** the [terminal-ux.md](terminal-ux.md) keymap is authoritative, except `b` (row 22). Mask keys apply only inside the masks screen and after the `@` prefix |
| 11 | Option letters for rules | [fastmail-platform.md](fastmail-platform.md) options A to E and [fastmail-rules-sieve.md](fastmail-rules-sieve.md) options A to C use the same letters for different things (platform B equals rules-sieve A) | **Resolved:** this doc names designs (marked block, JSON round trip, RFC 9661 adapter) instead of letters |
| 12 | Own MCP server | [fastmail-platform.md](fastmail-platform.md): maybe later, for masks and rules. [email-clients.md](email-clients.md): not needed, Fastmail's covers agents | **Resolved:** out of scope for v1. Revisit after a CLI exists |
| 13 | Vacation response access | [fastmail-rules-sieve.md](fastmail-rules-sieve.md): needs a registered OAuth client. [fastmail-platform.md](fastmail-platform.md): draft section 5.4.1 puts `vacationresponse` under the open `mail` scope | **Open:** gated on any OAuth grant that actually carries it. Test in Phase 0 |
| 14 | Search order | Fastmail web: server first, local index only offline ([email-clients.md](email-clients.md)). [fastmail-platform.md](fastmail-platform.md): local and server in parallel | **Resolved:** parallel, local shown first and labelled, server authoritative. A terminal user typing benefits from the instant local layer |
| 15 | Latency budgets | [email-clients.md](email-clients.md): under 16 ms for local actions, under 50 ms for opening a cached thread. [terminal-ux.md](terminal-ux.md): p99 under 8 ms of mejl's own work | **Resolved:** 8 ms p99 is mejl's own share, which keeps the whole frame under 16 ms. 50 ms applies to opening a message that still needs rendering |
| 16 | Fuzzy matcher | [rust-crates.md](rust-crates.md): nucleo-matcher 0.3.1. [terminal-ux.md](terminal-ux.md): nucleo 0.5.0 or frizbee | **Resolved:** nucleo-matcher, which is in the verified probe. nucleo for huge lists later |
| 17 | sieve-rs license wording | crates.io: AGPL-3.0-only. README: "or any later version" ([fastmail-rules-sieve.md](fastmail-rules-sieve.md)) | **Open,** but moot under an MIT/Apache license. Matters only if mejl chooses the AGPL |
| 18 | Editable Sieve sections: three or four | Help page: four. Third-party sources: three | **Open.** mejl names sections by position ("below spam protection"), never by count |
| 19 | Which `require` line is current | Help page line with `vnd.cyrus.imip`, or a 2025 sample with `processcalendar` | **Open.** mejl uses only extensions in both lines and never emits its own `require` |
| 20 | Storage and auth decisions | [terminal-ux.md](terminal-ux.md) defers to "the storage doc", [fastmail-masked-email.md](fastmail-masked-email.md) to "the auth doc". Neither exists | **Resolved:** this doc makes both decisions (SQLite with two files; token then OAuth) |
| 21 | Thunderbird desktop JMAP | Official roadmap does not list it. A forum post says it does ([email-clients.md](email-clients.md)) | Resolved there in favour of the roadmap. Not relevant to mejl's design |
| 22 | Snooze in the default keymap | [email-clients.md](email-clients.md) and [terminal-ux.md](terminal-ux.md) put `b` then `1` to `7` (snooze) in the default keymap. [fastmail-platform.md](fastmail-platform.md) says Fastmail cannot snooze from third-party clients, exposes no snooze time over JMAP, and flags client-side versions of plan-gated features as possible circumvention | **Resolved for v1:** `b` is reserved and shows "snooze is web-only". No client-side snooze timer. Revisit when Fastmail answers question 18 in [README.md](README.md) |
| 23 | Disk reads on the UI thread | [terminal-ux.md](terminal-ux.md): the UI thread never waits on the network or disk. This doc: the UI thread may do bounded, indexed SQLite page reads within 2 ms p99 | **Resolved provisionally:** allowed, because page reads avoid skeleton flicker during fast scrolling. The divan benchmark decides. If reads miss the budget, they move to the store thread and the list shows skeleton rows |

## Open questions

Architecture-specific questions. The consolidated list with verification steps is in [README.md](README.md).

| Question | How to resolve |
|---|---|
| Can the UI thread do indexed SQLite page reads within 2 ms p99 at 1M messages? | divan benchmark on a synthetic 1M store, WAL mode, with a concurrent writer applying `/changes` batches |
| Is a contentless FTS5 table fast enough, or is external content better for snippets and highlights? | Same benchmark, both variants |
| How much memory does the row cache plus id lists use at 100k and 1M? | Bench harness RSS readings |
| Does ring instead of aws-lc-rs shrink build time and binary enough to matter? | Probe build with `rustls-no-provider` plus ring |
| Can ratatui park a hidden cursor on the focused row for screen readers? | Prototype a backend wrapper, test with Orca, VoiceOver and NVDA |
| Does the contact-group mechanism really work for data-driven rules? | README verification step on a trial account |
| What undo-send hold delay feels right without the server hold? | Try 5, 10 and 20 s in daily use. Fastmail uses a 15 s undo window and 20 s delivery delay (verified: [fastmail-platform.md](fastmail-platform.md)) |
| Will the license be MIT OR Apache-2.0? | User decision before the first dependency lands |
| Will mejl support Basic-plan users? | User decision after Fastmail answers the OAuth-on-Basic question |
