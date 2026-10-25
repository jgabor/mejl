# mejl research docs

## Summary

mejl is a planned TUI email client in Rust, built only for Fastmail. This folder holds the research done on 2026-10-23 and the architecture that follows from it. The short version:

- Mail, sending, contacts and Masked Email are all public JMAP, so mejl can be JMAP-first with no IMAP code.
- Rules (Sieve) have no public API. mejl can author and preview rules, but the user pastes the result into Fastmail's web UI.
- Masked Email can be managed fully. Sending from a mask over JMAP is unverified.
- An instant UI is reachable with a local-first design like Fastmail's own web app, but it has to be measured.
- A public release should wait for Fastmail's answer on its API Developer Policy.

Start with [architecture.md](architecture.md). The open questions at the end of this page list every unknown with the step that settles it.

## Docs in this folder

| File | What it covers |
|---|---|
| [architecture.md](architecture.md) | Recommended architecture and product direction: principles, feasibility verdicts, threads and sync engine, data model, auth, crate picks, performance budgets, testing, roadmap, risk register, and a table of where the research docs disagree |
| [fastmail-platform.md](fastmail-platform.md) | Fastmail as a platform: JMAP capabilities and endpoints, API tokens and OAuth, push, session limits, the API Developer Policy, sending and delayed send, search mapping, the official MCP server, Fastmail's own apps as the UX baseline |
| [fastmail-rules-sieve.md](fastmail-rules-sieve.md) | What mejl can and cannot do with Fastmail rules and Sieve: RFC 9661 and ManageSieve status, the private web API (rejected), how generated and custom Sieve coexist, Rust Sieve crates, prior art, the recommended marked-block design, and a verification procedure |
| [fastmail-masked-email.md](fastmail-masked-email.md) | The Masked Email API: object, states, methods, errors, examples, sending from a mask, how 1Password and Bitwarden use it, and UX ideas for mejl |
| [email-clients.md](email-clients.md) | Survey of 16 terminal clients and 13 desktop, web and mobile clients: what makes them fast, triage ideas, anti-patterns, a feature comparison |
| [rust-crates.md](rust-crates.md) | About 150 crates with versions, licenses and repo signals, a verified starter `Cargo.toml`, build time and binary size measurements, version and license traps |
| [terminal-ux.md](terminal-ux.md) | Terminal protocols and a support matrix, detection strategy, keymap design, mouse, layout, HTML rendering, compose, performance, accessibility, and a UX spec sketch with a full default keymap |
| [mockups.html](mockups.html) | Self-contained HTML mock-ups of mejl's main screens |

## Project vision

mejl is the fastest way to live in a Fastmail account from a terminal. It opens to your inbox from a local cache in a blink, applies every action at once and syncs in the background over JMAP, and stays usable offline, over SSH and inside tmux. It speaks Fastmail's own keyboard language by default, works fully from the keyboard, and lets the mouse help where it is quicker. It treats Fastmail's distinctive features as first-class: you can disable the masked address a spammer found, or turn the message in front of you into a rule with a live preview, without leaving the conversation. It stays small: one provider done deeply, no plugins, no AI, no servers of its own.

## Top 10 findings

1. **No public rules or Sieve API.** Fastmail's FAQ says "the only way to modify your script is by logging in to the web interface" and that ManageSieve is not supported. No OAuth or token scope covers rules. The web app's private `dev/rules` API is banned by the API Developer Policy ("Using unpublished APIs"). mejl's rules feature must end in a user paste (verified: [fastmail-rules-sieve.md](fastmail-rules-sieve.md)).
2. **Masked Email is a small, public, documented API.** `MaskedEmail/get` and `MaskedEmail/set` under `https://www.fastmail.com/dev/maskedemail`, four states, a `rateLimit` error on create (verified). Delta sync, push and sending as a mask are unverified, so mejl refetches the full list and treats reply-from-mask as a goal, not a promise ([fastmail-masked-email.md](fastmail-masked-email.md)).
3. **Policy shapes the release plan.** Fastmail's dev docs say distributed apps "should implement OAuth" and that tokens are for testing or personal use. The policy also bans using the API "to replicate or compete with core products" (verified: [fastmail-platform.md](fastmail-platform.md)). Ask Fastmail before a public release.
4. **OAuth is in flux.** The dev docs say clients are registered by hand. Live discovery metadata shows open dynamic registration, PKCE S256 and coarse interoperable scopes (verified by curl). Registration is untested, and Masked Email is missing from the advertised scopes ([fastmail-platform.md](fastmail-platform.md)). Basic plans get no API tokens at all (verified).
5. **Push works over EventSource**, with one reported non-standard event shape. No evidence of JMAP over WebSocket. mejl needs a tolerant parser, its own idle timeout and a resync after every reconnect ([fastmail-platform.md](fastmail-platform.md)).
6. **Fastmail web is the speed bar, and its design is public.** It has been fully offline over JMAP since 2025-08-26, with a local cache that speaks JMAP, one shared push connection, a patch change log with last-write-wins, and staged loading (verified: [email-clients.md](email-clients.md)). mejl copies this in Rust with SQLite and must win on startup, memory and terminal workflow instead.
7. **The niche is open.** No mature terminal client is built around JMAP or Fastmail, and no client other than Fastmail's own manages Fastmail rules or Masked Email (verified and inferred: [email-clients.md](email-clients.md)).
8. **The Rust stack is clear, with three traps.** ratatui 0.30.2 with crossterm 0.29.0, tokio, reqwest 0.13 with sse-stream, rusqlite bundled with FTS5, html2text (verified probe build). Traps: tui-textarea does not work with ratatui 0.30, jmap-client cannot do MaskedEmail (closed method enum), and sieve-rs is AGPL-3.0-only ([rust-crates.md](rust-crates.md)).
9. **Terminals support what mejl wants, but stock tmux hides it.** Most modern terminals implement the kitty keyboard protocol and synchronized output, while tmux 3.8 defaults turn off extended keys, focus events and app clipboard writes (verified: [terminal-ux.md](terminal-ux.md)). mejl uses a legacy-safe keymap modelled on Fastmail's web shortcuts and ships a tmux snippet.
10. **"Instant" has a number.** Terminals add 5 to 45 ms on their own (dated measurement), and 100 ms is the instant limit. So mejl's own work per local action gets a p99 budget of 8 ms, and a warm start gets 50 ms (inferred targets: [terminal-ux.md](terminal-ux.md), [architecture.md](architecture.md)).

## Decisions the user needs to make

| Decision | Options | Recommendation (inferred) |
|---|---|---|
| License | MIT OR Apache-2.0, or AGPL-3.0 | MIT OR Apache-2.0. It fits the whole starter set and leaves out sieve-rs |
| Basic-plan users | Support through OAuth if it works, or exclude | Decide after Fastmail answers. Detect the plan and explain clearly either way |
| When to contact Fastmail | Now, or after a prototype | Now. The compete clause and token-paste questions gate a public release |
| MSRV | 1.88 (starter set's highest direct requirement) or later | 1.88, checked with `cargo +1.88 build` once the workspace exists |

## Open questions

Consolidated from all docs and deduplicated. Safety rules for every step:

- **Read-only probes** (session GET, `Core/echo`, EventSource, a manual look in the web UI, one manual export) may use the user's own account with a temporary token that is deleted afterwards, as the procedure in [fastmail-rules-sieve.md](fastmail-rules-sieve.md) does. A dedicated test account is better where one exists.
- **Every write test** (`SieveScript/set`, rules import, mask create or destroy, sending, keyword changes, new contact groups) runs **only on a trial account**.
- **Nothing ever goes through the Fastmail MCP tools.** They act on the user's real mailbox.
- Never call anything under `https://www.fastmail.com/dev/rules`, even read-only.
- Session output contains account ids and the username. Redact before saving it as a fixture.

### Fastmail API behaviour (live tests)

| # | Question | Verification step | Source docs |
|---|---|---|---|
| 1 | What does a live session advertise per token scope? In particular `urn:ietf:params:jmap:sieve`, `dev/rules`, `websocket`, `quota`, `blob`, `principals`, `mail:snooze`, `vacationresponse`, `submissionExtensions`, `maxDelayedSend`, the `eventSourceUrl` shape and the `apiUrl` host | `curl -s -H "Authorization: Bearer $TOKEN" https://api.fastmail.com/jmap/session \| jq '{caps: (.capabilities\|keys), acct: [.accounts[].accountCapabilities\|keys], eventSourceUrl, apiUrl}'` with an all-scopes test token, then again with a read-only token. Then POST `{"using":["urn:ietf:params:jmap:core","urn:ietf:params:jmap:sieve"],"methodCalls":[["Core/echo",{},"0"]]}` to `apiUrl` and expect `unknownCapability`. Save the redacted session as a test fixture | platform, rules-sieve, rust-crates, masked-email |
| 2 | Is the EventSource payload spec-compliant (`@type: StateChange`)? Are `ping` and `closeafter` honoured? Does push ever name `MaskedEmail`? | `curl -N -H "Authorization: Bearer $TOKEN" "https://api.fastmail.com/jmap/event/?types=*&closeafter=no&ping=60"` (or the expanded session template). Watch for pings and event shape. Change a mask in the web UI of the test account and watch for a `MaskedEmail` entry | platform, masked-email |
| 3 | Does open OAuth registration work? Which capabilities does the `mail` scope grant (submission, vacation)? Can Masked Email be requested? Does it work on Basic? | On a trial account: POST an RFC 7591 body (public client, `token_endpoint_auth_method: none`, redirect `http://127.0.0.1:<port>/`, scopes `urn:ietf:params:oauth:scope:mail urn:ietf:params:oauth:scope:contacts offline_access`) to `https://api.fastmail.com/oauth/register`. Complete PKCE in a browser, then run step 1 with the access token. Repeat asking for `https://www.fastmail.com/dev/maskedemail`. This creates a client on Fastmail's servers, so do it deliberately and say so when writing to Fastmail | platform, masked-email, rules-sieve |
| 4 | Does delayed submission work (`HOLDFOR`, `HOLDUNTIL`) despite empty `submissionExtensions`? Does `undoStatus: canceled` work? Do held mails show in the web Scheduled folder? | On a trial account, submit to yourself with `envelope.mailFrom.parameters` `{"HOLDFOR": "60"}`. Read `undoStatus` and `sendAt`. Cancel one, let another send. Check the web UI | platform, terminal-ux |
| 5 | Can JMAP send as a masked address? Does `Identity/get` list masks? Does `EmailSubmission/set` accept a mask in `From`? Can `Identity/set` create one? | Trial account with Email, Email submission and Masked Email scopes: call `Identity/get`, draft from a mask to yourself, submit with the main identity, then try `Identity/set`. Record SetError types | masked-email, platform |
| 6 | Which masked-email headers exist on stored mail, and what values? `X-FM-Masked-Email`, `X-FM-Masked-Email-Reply`, and `Fastmail-MaskedEmail` (seen in generated Sieve) | Send test mail to a mask via To, then via Bcc, and reply from it in the web UI. `Email/get` with `header:X-FM-Masked-Email:all`, `header:X-FM-Masked-Email-Reply:all` and `header:Fastmail-MaskedEmail:all` | masked-email, rules-sieve |
| 7 | How do `MaskedEmail/changes` and `MaskedEmail/query` behave? Is `/get` `state` empty? Does API destroy work on a never-used mask, and `deleted` to `enabled`? Is `core` in `using` required? | Trial account: call each, record exact errors. Create a fresh mask and destroy it. Delete and restore another. Send one request without `core` | masked-email |
| 8 | Which keywords and mailbox roles does Fastmail set for pin, mute, follow, memo, snoozed and scheduled? Can the snooze wake time be read? | In the test account's web UI, pin, mute, follow, add a memo and snooze one message each. `Email/get` all properties, `Mailbox/get` roles | platform |
| 9 | Does Fastmail web show custom keywords such as `$replylater`, or should Reply Later use a mailbox? | Set the keyword with `Email/set` on a test account, look in the web UI and search `keyword:$replylater` | email-clients |
| 10 | Do `X-Spam-score`, `X-Spam-known-sender`, `X-Delivered-to` and `X-Mail-from` come back through `Email/get` on stored mail? | `Email/get` a few test messages with `header:X-Spam-score:asText` and the others | rules-sieve |
| 11 | Does `X-Spam-known-sender` contain a contact group's id for any group, so a pasted rule can be driven by group membership? | Trial account: create a contact group with a test sender in it, send mail from that sender, read `header:X-Spam-known-sender:asText`, compare with the group's `uid` from JMAP Contacts. Then test a rule matching that value in the Sieve tester | architecture (from rules-sieve) |
| 12 | Does moving mail into or out of Spam over JMAP (or setting `$junk` / `$notjunk`) train the Bayes filter? | Ask Fastmail support. Optionally test on a trial account and watch later `X-Spam-*` headers | platform |
| 13 | What is the deep-link format for a message in Fastmail web, for an "open in Fastmail" key that keeps Fastmail's image proxy? | Open messages in the test account's web UI and read the URLs by hand | terminal-ux |

### Fastmail policy and product (ask Fastmail)

Write to partnerships@fastmailteam.com (the address on the [partners page](https://www.fastmail.com/company/partners/), verified in [fastmail-platform.md](fastmail-platform.md)).

| # | Question | Source docs |
|---|---|---|
| 14 | Does an open-source, local-only third-party mail client count as "replicating or competing with core products"? | platform, rules-sieve |
| 15 | Is token paste acceptable for a public release where each user makes their own token, or is OAuth expected from day one? | platform |
| 16 | Will Fastmail expose RFC 9661 `SieveScript` or a rules scope, and how would it map onto the generated script blocks? | platform, rules-sieve, email-clients |
| 17 | Can mejl get the Masked Email scope over OAuth (open or registered client)? Does OAuth JMAP access work on Basic plans? | platform, masked-email, rules-sieve |
| 18 | Would client-side versions of plan-gated features (snooze, scheduled send) be seen as circumvention? | platform |
| 19 | What are the real Masked Email creation limit and total cap, and which SetError does the cap return? Do not probe a real account | masked-email |
| 20 | Are the generated Sieve pipeline variables (`spam`, `skipinbox`, `stop`) stable? Treat them as unstable until answered | rules-sieve |
| 21 | When will JMAP Calendars open to third parties after the draft becomes an RFC? Also watch the [datatracker page](https://datatracker.ietf.org/doc/draft-ietf-jmap-calendars/) | platform |

### Rules and Sieve

| # | Question | Verification step | Source docs |
|---|---|---|---|
| 22 | Three or four editable custom Sieve sections? Which `require` line is current (`vnd.cyrus.imip` or `processcalendar`)? | The user opens Settings, Filters & Rules, Edit custom Sieve code in their own account (read-only look), counts the "(Insert Sieve code here)" areas and copies the require line | rules-sieve, platform |
| 23 | How does a UI rule compile (`jmapquery`, `${stop}` guard, `:mailboxid`)? Do JMAP Mailbox ids equal the ids Sieve accepts in `:mailboxid`? | Trial account: create one throwaway UI rule that files into a folder, read the generated block, compare its id with `Mailbox/get` | rules-sieve |
| 24 | Does an explicit `keep` in custom code break alias folder delivery? Does the web UI reject invalid custom code on save, with line numbers? Does `:regex` follow POSIX ERE? | Trial account and the [Sieve tester](https://app.fastmail.com/sievetester): add `keep`, a syntax error, and a PCRE-only pattern | rules-sieve |
| 25 | Does mail to a disabled mask run through the top custom section before Trash? Does plus addressing work on masks? | Trial account: a top-section rule that adds a flag, a disabled mask, a test message. Then mail `mask+tag@domain` | masked-email, rules-sieve |
| 26 | What is the rules and Sieve JSON export format, and does import replace, merge or duplicate? | One redacted export from the real account through the web UI (a read-only action the user does by hand). Import tests on a trial account only | rules-sieve, platform |
| 27 | Are the permissive Sieve parsers (mailrs-sieve-core, sieve-form, sieve-kit) good enough, or does mejl write its own tokenizer? Is sieve-rs AGPL-3.0-only or "or later"? | After the license decision, rerun the [fastmail-rules-sieve.md](fastmail-rules-sieve.md) section 6.1 experiment on each permissive crate. Ask Stalwart about the license wording only if AGPL is chosen | rules-sieve, rust-crates |
| 28 | How do JMAP Sieve draft-14 (in jmap-client) and RFC 9661 differ on the wire? Only matters if code is borrowed | Diff the draft and the RFC on datatracker | rust-crates |

### Rust, storage and build

| # | Question | Verification step | Source docs |
|---|---|---|---|
| 29 | Is SQLite FTS5 fast enough at 1M messages, and can the UI thread do indexed page reads within 2 ms p99 while a writer applies `/changes`? | divan benchmark on a synthetic 1M-message store in WAL mode: list pages, FTS queries, batch writes, contentless versus external-content FTS | rust-crates, architecture |
| 30 | How much do ring instead of aws-lc-rs and `opt-level = "s"` change build time, size and hot-loop speed? | Probe build with `reqwest` `rustls-no-provider` plus ring. Benchmark render and parse at both opt levels | rust-crates |
| 31 | What MSRV can mejl promise? | `cargo +1.88 build` (or `cargo msrv`) once the workspace exists | rust-crates |
| 32 | Does html2text's `css` feature honour inline colours well enough, and is html2text robust on large and hostile newsletters? | Render a corpus of real newsletters with and without the feature. Fuzz the conversion step with `cargo fuzz` | rust-crates, terminal-ux |

### Terminal and UX

| # | Question | Verification step | Source docs |
|---|---|---|---|
| 33 | The "?" cells in the terminal support matrix, and which kitty keyboard flags each terminal honours. What are crossterm's exact `KeyboardEnhancementFlags` names? | Build a small `mejl probe` command that sends the batched queries and echoes keys. Run it in kitty, Ghostty, WezTerm, foot, Alacritty, iTerm2, Windows Terminal, Konsole, tmux and zellij. Read docs.rs for the flag names | terminal-ux, rust-crates |
| 34 | Does tmux with the default `set-clipboard external` pass an app's OSC 52 to the outer terminal? | Test on tmux 3.8 with and without `set -g set-clipboard on` | terminal-ux |
| 35 | Can ratatui park a hidden cursor on the focused row for screen readers? | Prototype a backend wrapper. Test with Orca, VoiceOver and NVDA | terminal-ux |
| 36 | Why was ratatui PR #2853 (per-cell hyperlinks) closed, and is the Termina backend in a released ratatui? | Read the PR thread and the ratatui changelog after 0.30.2 | terminal-ux |
| 37 | Do the startup (50 ms warm, 150 ms cold) and 8 ms frame budgets hold on real hardware? How do aerc, meli and himalaya-tui compare? | pty harness with vt100 measuring exec to first flush, with a 50k-message test account or a local Stalwart server, run for mejl prototypes and the other clients | terminal-ux, email-clients |
| 38 | Good defaults for double-click interval, wheel step, which-key popup delay and undo-send hold | Make each configurable, try values in daily use. Read helix, Neovim which-key and yazi docs for popup timing | terminal-ux, architecture |
| 39 | Real-world HTML quote and signature markers (Gmail, Outlook, Apple Mail) | Build a sanitized test corpus and test the folding heuristic | terminal-ux |
| 40 | A colour-blind-safe default palette for truecolor themes | Use a published palette such as Okabe-Ito and check contrast with a simulator | terminal-ux |

### Low priority, competitor facts

| # | Question | Verification step | Source docs |
|---|---|---|---|
| 41 | What is aerc's JMAP offline status after 0.22? Does Matcha really support JMAP? What OAuth flow does fm-cli use? | Read `worker/jmap` and the 0.21 changelog in aerc, the backends in floatpane/matcha, and fm-cli's auth code | email-clients |
| 42 | Is Thunderbird desktop planning JMAP? | Recheck roadmaps.thunderbird.net each quarter | email-clients |

## Status of the known gaps (2026-10-25)

Fixed, each with tests:

- Undo shows in the same frame as the key, for actions of this run. Undoing an action from an earlier run, or a whole-mailbox action, still shows one store round trip later.
- Quitting while a message is in its undo-send window asks: Send now (default), Keep queued, Cancel send, Stay. `Q` quits at once. Send now waits up to 5 seconds.
- `Ctrl-z` suspends from every context and redraws on resume. In the compose body, undo is `Ctrl-_`, `Ctrl-/` or `Ctrl-u`; redo is `Ctrl-y` or `Ctrl-r`.
- Compose has its own body editor and stays fast at 5000 lines (p99 key to frame about 0.4 ms, was 10 ms).
- Drafts are saved to the server (Drafts mailbox, `$draft`): at once on save key, close and blur, and at most every 10 seconds while typing. Without a Drafts mailbox they stay on the device and the compose line says so. A cancelled send saves the draft first. When the server refuses a draft, only that draft's compose line shows the reason and "on this device", until the draft changes.
- "Open in browser" sanitizes the HTML with ammonia, in addition to the CSP. ammonia brings two MPL-2.0 crates (cssparser, dtoa-short), a user decision on 2026-10-25.
- Mask descriptions are covered by the hostile-mail test. Copied text now has control and bidi characters removed.
- `$EDITOR` pause waits for crossterm's reader to let go. The probe drains late replies. `caps.toml` and a small quirks table exist. Pty tests cover `enter`, `restore` and `suspend_*`.
- Reader side-panel rows, the search filter block, Masked Email silent and created-by filters (keys `5`, `6`) and the rules "where the block runs" section (at 160x40 only) follow the mock-ups.
- New setting `ui.mark_read_on_archive` (off by default).
- Custom key presets with an in-app key editor (settings, Enter on Keys): duplicate a built-in preset, rebind each command, swap or replace on a clash. A preset stores its base and its changes only (`[presets.<name>]`).
- OAuth login (`mejl login --oauth`): discovery, open dynamic registration (or `oauth_client_id`), PKCE S256 with a `127.0.0.1` loopback redirect, `iss` check, refresh tokens that rotate and are never reused, and revoke on logout. Built and tested against the fake only. A live login answers open questions 3 and 17; it registers a client on Fastmail's servers, so the user runs it.
- Macros (`[macros.<name>]`): several changes to mail on one key, sent as one change with one undo, built in the key editor's Macros section.

Not done:

- Mode 2031 live theme notifications. crossterm 0.29 stops reading input after `CSI ? 997 ; n` replies, so turning it on needs our own input parser or a crossterm patch. The enable code and event type are in `terminal/theme.rs`.
- mejl probes on every start. Skipping the probe on a cache hit would be faster over SSH but can go stale.
- Keys typed during the short probe drain are dropped.
