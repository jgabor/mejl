# Email clients: a survey for mejl

Research date: 2026-10-23. Scope: terminal clients, then desktop, web and mobile clients known for speed and user experience. The goal is to find what mejl should copy, what it should avoid, and where the gaps are.

## Summary

- **No mature terminal client is built around JMAP or Fastmail.** aerc has a real JMAP backend with push ([aerc-jmap(5)](https://man.archlinux.org/man/aerc-jmap.5.en)), and meli rates its JMAP backend as "functional" ([meli README](https://github.com/meli/meli)). Both treat JMAP as one backend among many. The JMAP-first terminal clients that exist (jmap-tui, fm-cli, blorp, elma) were all first released or created in 2026 and are small, with 0 to 7 GitHub stars. (elma's repo dates from 2018, but its first crate release was 2026-03-01.) The niche is open. (verified: GitHub API, 2026-10-23)
- **No client other than Fastmail's own manages Fastmail Sieve rules or Masked Email.** Among third-party clients, Mimestream comes closest: it edits Gmail's server-side filters inside the app ([mimestream.com](https://mimestream.com/)). himalaya 2.2.1 added a ManageSieve backend on 2026-10-02 ([releases](https://github.com/pimalaya/himalaya/releases)). (inferred from feature lists; no client lists Masked Email)
- **Fastmail's own web app sets the speed bar mejl has to beat.** Full offline support went GA on 2025-08-26 ([Fastmail blog](https://www.fastmail.com/blog/fastmail-works-offline/)). Its design is worth copying almost directly: a JMAP-speaking local cache, one shared EventSource push connection, modseq change tracking, ordered patch replay with last-write-wins, and an on-device search index ([architecture](https://www.fastmail.com/blog/offline-architecture/), [sync](https://www.fastmail.com/blog/offline-sync/), [storage](https://www.fastmail.com/blog/offline-mail-storage/)). (verified)
- **Fast clients feel fast for the same reasons.** They render from a local store, apply changes optimistically, sync in the background, prefetch what you will open next, and measure latency in buckets. Superhuman treats 100 ms as the ceiling and aims for under 50 ms ([Superhuman blog](https://blog.superhuman.com/superhuman-is-built-for-speed/)). It splits each action into a synchronous local `modify` and an idempotent, queued `persist` ([offline post](https://blog.superhuman.com/architecting-a-web-app-to-just-work-offline-part-1/)). (verified)
- **Keyboard UX has settled on three layers.** Single-key verbs come first, then a command palette or `:` command line, then contextual `?` help. aerc 0.22 added a popup that shows which keys can follow a multi-key prefix ([aerc CHANGELOG](https://git.sr.ht/~rjarry/aerc/tree/master/item/CHANGELOG.md)). (verified)
- **Triage ideas that work.** Done or archive on one key, snooze, Reply Later, split inbox, sender screening (HEY Screener, Spark Gatekeeper) and undo for everything. mejl can build screening on Sieve, which ties the triage flow to the rules goal. (inferred)
- **Warning signs.** Notion Mail shut down on 2026-09-22, about 17 months after launch, as Notion moved to AI agents ([TechCrunch](https://techcrunch.com/2026/06/25/notion-mail-shuts-down-amid-agent-takeover/)). Spark stores message subjects and snippets on its servers to send push notifications ([Readdle support](https://support.readdle.com/spark/privacy/privacy-explained)). Thunderbird is rewriting its per-folder message databases into one global SQLite database ([Panorama docs](https://source-docs.thunderbird.net/en/latest/panorama/index.html)). (verified)
- **Vendors now ship terminal and agent interfaces.** In 2026, HEY shipped an official CLI and TUI ([hey.com/agents](https://www.hey.com/agents/)), Spark shipped a CLI for agents ([Spark blog](https://sparkmailapp.com/blog/introducing-spark-cli)) and Fastmail launched an MCP server ([Fastmail blog](https://www.fastmail.com/blog/an-mcp-server-for-fastmail/)). A scriptable JSON surface is now normal, not exotic. (verified)

What this means for mejl: build one deep Fastmail client, not a generic one. Make a local JMAP store the single source the UI reads from. Use Fastmail web's keymap as the default so Fastmail users feel at home. Make Sieve rules and Masked Email actions you can take straight from a message, not settings screens.

## Method and labels

- Sources: project repositories (read through the GitHub API, plus a direct clone of aerc from sourcehut), official docs and blogs, release notes, and package registries. Third-party reviews are used only for user complaints, and those claims are labeled.
- Labels used in this doc:
  - **(verified: source)**: a primary source confirms the claim.
  - **(inferred)**: my own reasoning from verified facts.
  - **(unverified)**: I could not confirm it, or it comes from a secondary source or a single user report.
  - **(project claim)**: the project says this about itself, and nobody has measured it independently.
- Star counts and push dates come from the GitHub API on 2026-10-23. They show activity, not quality.

## Terminal clients

### At a glance

| Client | Language | Protocols | Native JMAP | Sync and cache model | Mouse | Latest release | Activity |
|---|---|---|---|---|---|---|---|
| [aerc](https://aerc-mail.org/) | Go | IMAP, JMAP, Maildir, notmuch, mbox; SMTP, JMAP, sendmail out | Yes, RFC 8620/8621, with EventSource push | Online by default; optional levelDB state cache and blob cache for JMAP, header cache for IMAP | Opt-in (`mouse-enabled`, default false) | 0.22.0, 2026-07-28 | Active |
| [NeoMutt](https://neomutt.org/) | C | IMAP, POP, Maildir, mbox, MH, notmuch (notmuch unverified) | No (unverified) | Online; optional header cache, body cache, QRESYNC | None found (unverified) | 20260616, 2026-06-16 | Active, releases every 1 to 3 months |
| [Mutt](http://www.mutt.org/) | C | IMAP, POP, Maildir, mbox, MH | No (unverified) | Online; optional header and body cache (unverified) | None found (unverified) | 2.3.3, 2026-06-12 | Maintained, small releases |
| [meli](https://github.com/meli/meli) | Rust | Maildir, mbox (read only), notmuch, IMAP, JMAP, NNTP | Yes, rated "functional" | SQLite caches, optional SQLite search index | Opt-in (`use_mouse`, default false) | 0.8.13, 2026-01-05 | Active commits |
| [himalaya](https://github.com/pimalaya/himalaya) | Rust | IMAP, SMTP, JMAP, Gmail, Microsoft Graph, Maildir, m2dir, mbox, ManageSieve | Yes (CLI) | Stateless CLI; sync handed to neverest and a local "pimdir" store | n/a (CLI) | 2.2.1, 2026-10-02 | Very active, grant funded |
| [himalaya-tui](https://github.com/pimalaya/himalaya-tui) | Rust | IMAP, SMTP, JMAP, Maildir | Yes | Same libraries as the CLI | None found | Unreleased (v0.x) | Active |
| [alot](https://github.com/pazz/alot) | Python | notmuch (local) | No | Local notmuch index; you supply the sync tool | Wheel scroll bindings | 0.12, 2025-12-11 | Slow but alive |
| [mu / mu4e](https://github.com/djcb/mu) | C++ / Emacs Lisp | Maildir (local) | No | Local Xapian index; you supply the sync tool | Emacs | 1.14.4, 2026-10-14 | Active |
| [notmuch](https://notmuchmail.org/) | C | Maildir (local) | No | Local tag index (Xapian, unverified); you supply the sync tool | Depends on frontend | 0.40, 2026-01-31 | Active |
| [sup](https://github.com/sup-heliotrope/sup) | Ruby | mbox, Maildir (local) | No | Local index; limited sync back | None found | 1.4, 2026-04-21 | Maintenance only |
| [Matcha](https://github.com/floatpane/matcha) | Go | IMAP; JMAP claimed (unverified) | Claimed | Background daemon (details unverified) | Configurable (unverified detail) | 1.1.0, 2026-10-10 | Very active, new (2025) |
| [HEY TUI](https://github.com/basecamp/hey-cli) | Go | HEY's own API only | No | Live updates from HEY | Unknown | 1.10.0, 2026-10-21 | Very active, new (2026) |
| [jmap-tui](https://github.com/CaffeinatedTech/jmap-tui) | Go | JMAP only | Yes | No local mail storage, live push | Unknown | None | New (2026-09) |
| [fm-cli](https://github.com/ninepointlabs/fm-cli) | Go | Fastmail JMAP | Yes | Live `watch` over push | Unknown | Has releases | New (2026-01) |
| [elma](https://github.com/roblillack/elma) | Rust (ratatui) | Gmail over IMAP, any JMAP | Yes | Not documented | Unknown | 0.2.1, 2026-08-18 | New |
| [blorp](https://github.com/tanem/blorp) | Rust | Fastmail JMAP | Yes | Not documented | Unknown | None | Early (2026-10) |

Sources for the table: each project's README or docs, plus the GitHub API, the aerc sourcehut tags, the [mutt-announce list](https://lists.mutt.org/pipermail/mutt-announce/Week-of-Mon-20260608/000064.html), the [notmuch news page](https://notmuchmail.org/news/) and [RubyGems](https://rubygems.org/gems/sup). Details and caveats follow in each section.

### aerc

- **Basics.** Written in Go. Drew DeVault created it in 2019, and Robin Jarry has maintained it since 2021 (verified: [FOSDEM 2025 slides](https://aerc-mail.org/fosdem-2025/)). The canonical repo is on sourcehut. The GitHub repo `rjarry/aerc` is a mirror whose tags stop at 0.20.1, so do not judge activity from it. (verified: sourcehut clone)
- **Releases.** 0.21.0 on 2025-08-28 and 0.22.0 on 2026-07-28. (verified: sourcehut tags)
- **Protocols.** IMAP, Maildir, notmuch, mbox and JMAP for reading. IMAP, JMAP, SMTP and sendmail for sending. (verified: [aerc-mail.org](https://aerc-mail.org/))
- **JMAP.** Implements RFC 8620 and 8621 (verified: [aerc-jmap(5)](https://man.archlinux.org/man/aerc-jmap.5.en)). The details:
  - Auth is HTTP Basic (`jmap://`) or OAUTHBEARER. Setting `outgoing = jmap://` sends mail over the same session.
  - `cache-state` keeps mailboxes, headers and flags in levelDB. `cache-blobs` stores bodies and attachments as files. Both are off by default.
  - The blob cache "is never purged automatically", and the man page suggests a cron job to clean it.
  - `use-labels` turns the archive mailbox into a virtual "All mail" folder and enables a `:tag` command.
  - The backend listens on a JMAP EventSource push stream. `server-ping` sets the ping interval. (verified: `worker/jmap/push.go` at tag 0.22.0)
- **Sync and cache.** aerc "only downloads the information which is necessary to present the UI" (project claim, [aerc-mail.org](https://aerc-mail.org/)). Offline work across IMAP, JMAP and SMTP was listed as future work in early 2025 (verified: FOSDEM 2025 slides). IMAP has a `cache-headers` option. (verified: `doc/aerc-imap.5.scd`)
- **UX ideas worth noting:**
  - Vim-style keys and an ex-style command line, so any key sequence can be bound to any command. Bindings are scoped per context in `binds.conf`. (verified: FOSDEM slides)
  - `?` runs `:help keys` and shows the bindings for the current context. 0.22 added a keyhint popup that lists the possible next keys after a multi-key prefix. (verified: FOSDEM slides, CHANGELOG 0.22.0)
  - Compose runs your `$EDITOR` in an embedded terminal, tmux style, so you can still read other mail while writing. (verified: aerc-mail.org)
  - Tabs, including a shell tab, and first-class `git send-email` patch review. (verified: aerc-mail.org)
  - Message parts go through user-defined "filters" mapped to MIME types: part, then filter, then pager, then virtual terminal. HTML goes through a sandboxed w3m. 0.22 can inline `cid:` images as sixel graphics. (verified: FOSDEM slides, CHANGELOG)
  - Message list columns are Go templates. Settings can be overridden per account or per folder. `:reload` applies config without a restart, and 0.22 also reloads on SIGHUP. (verified)
  - Threading can be toggled at runtime and works with or without server support. (verified: aerc-mail.org, FOSDEM)
- **Mouse.** `mouse-enabled` defaults to false. 0.22 fixed mouse handling in the message viewer, which suggests mouse support gets less testing than the keyboard. (verified: `doc/aerc-config.5.scd`, CHANGELOG; the second point is inferred)
- **Pain points.** The JMAP caches are opt-in and do not clean themselves up. Full offline use is not there yet. Configuration is spread over three files (`aerc.conf`, `accounts.conf`, `binds.conf`) plus stylesets. (inferred from docs)

### NeoMutt and Mutt

- **Basics.** Both are written in C. Mutt dates from the mid 1990s (unverified). NeoMutt is a fork that collects patches and releases more often (unverified as a description; the release cadence below is verified). NeoMutt released 20260105, 20260406, 20260504 and 20260616 this year (verified: GitHub releases). Mutt 2.3.0 came out on 2026-01-25 (verified: [mutt-announce](https://lists.mutt.org/pipermail/mutt-announce/Week-of-Mon-20260119/000061.html)). Its release notes call 2.3 "a very tiny release" (verified: [Mutt 2.3 release notes](http://www.mutt.org/relnotes/2.3/)). Mutt 2.3.3 followed on 2026-06-12 with a crash fix. (verified: mutt-announce)
- **Performance is opt-in and needs tuning.** The NeoMutt guide says (verified: [NeoMutt performance tuning](https://neomutt.org/guide/tuning.html)):
  - Header caching "greatly" speeds up opening large IMAP and Maildir folders.
  - By default, NeoMutt caches only about 10 recently viewed messages per session, and they are gone next session. You need to enable body caching to keep them.
  - `imap_qresync` (QRESYNC) "can provide a huge speed boost" and needs the header cache. `imap_deflate` turns on compression.
  - Progress counter settings (`read_inc`, `time_inc`) can slow down reading a folder if they are set too low.
  - Regex search is "several times slower" than plain string search. On IMAP, searches run on the server, so they are case sensitive.
- **UX ideas.** Index and pager views, a sidebar, a pattern language for limit and search (`~f`, `=L`, and so on), macros, hooks, and a mailcap pipeline for HTML. Compose runs `$EDITOR` in place of the UI. (unverified in this session; long-standing and widely documented)
- **Pain points.** Users must tune caches, write a long muttrc, and configure the HTML viewer themselves (inferred from the tuning guide). No JMAP support was found. (unverified)

### meli

- **Basics.** Written in Rust. Supports Maildir, mbox (read only), notmuch and IMAP fully. JMAP and NNTP are rated "functional". JMAP is built in by default. MSRV is 1.88. (verified: [meli README](https://github.com/meli/meli))
- **Releases.** v0.8.12 and v0.8.13 both came out on 2026-01-05, and v0.8.11 on 2025-04-21. Commits continue as of 2026-10-22. (verified: GitHub API)
- **Cache and search.** SQLite is "used in caches", with an optional SQLite3 search index. Notmuch accounts have no direct search across mailboxes. (verified: README)
- **UX.** HTML goes through w3m by default and can be changed with `pager.html_filter`. Your editor runs inside meli through an embedded terminal emulator. Config is plain TOML, and shortcuts are configurable. Mail and attachments can be piped to external commands. (verified: README)
- **Mouse.** A `use_mouse` setting exists and defaults to false. (verified: `meli/src/conf/terminal.rs`)
- **For mejl.** meli is the closest Rust relative: an async multi-backend client with an embedded editor and a SQLite cache. Its JMAP backend shows JMAP working inside a mostly IMAP-shaped design, but does not show what a JMAP-first design would look like. (inferred)

### Himalaya and the Pimalaya libraries

- **Scope.** himalaya is a CLI, not a TUI. Pimalaya says it manages mail "over IMAP, SMTP, JMAP, Gmail, Microsoft, Maildir, m2dir and mbox". (verified: [pimalaya.org](https://pimalaya.org/))
- **Funding.** NLnet NGI grants have funded the project since 2022, now through 2027. (verified: io-jmap README)
- **Release history** (verified: [GitHub releases](https://github.com/pimalaya/himalaya/releases), dates from the GitHub API):
  - v1.0.0 (2024-12-09) narrowed the CLI's scope. Sync and envelope watching moved to separate tools, Neverest and Mirador.
  - v2.0.0 (2026-07-26) added Gmail and Microsoft Graph REST backends. It also removed the built-in composer and reader in favor of external tools, and changed the config format.
  - v2.1.0 (2026-08-16) added a pimdir backend, which reads the local store that the sync engine keeps.
  - v2.2.1 (2026-10-02) added a ManageSieve backend and mbox support. pimdir writes now go to a queue for the sync engine, and reads lay that queue over the store so changes show at once.
- **The CLI and TUI split.** `himalaya-tui` is a separate binary that shares the same TOML config. It is unreleased (v0.x) and has 83 stars. Features (verified: [himalaya-tui README](https://github.com/pimalaya/himalaya-tui)):
  - A three-pane layout: mailboxes, envelopes, and the message or composer.
  - An in-app composer that can hand off to your system editor.
  - Recipient completion from any contact command.
  - JMAP with basic or bearer auth.
  - An account wizard that finds services from an email address. It tries known provider rules, the PACC draft, Thunderbird autoconfig, SRV records and JMAP session resolution.
- **Sync.** Neverest syncs mail over IMAP and Microsoft Graph into a local pimdir store. Its README says "JMAP sources configure but have no backend yet". (verified: [neverest README](https://github.com/pimalaya/neverest))
- **Libraries that may matter to mejl:**
  - `io-jmap` has three layers. The bottom layer is I/O-free, `no_std` state machines. Above it sit a blocking light client and a full client with TLS. It covers RFC 8620 (including EventSource and PushSubscription), RFC 8621 and RFC 9610 contacts, plus draft calendars. (verified: [io-jmap README](https://github.com/pimalaya/io-jmap))
  - `io-managesieve` covers all of RFC 5804. It refuses cleartext credentials by default and blocks STARTTLS injection. It "never parses" Sieve, so the server compiles scripts and reports errors. (verified: [io-managesieve README](https://github.com/pimalaya/io-managesieve))
  - The crates are young, with 1 to 3 stars each. The rust-crates doc should judge them. (verified star counts; the judgment is inferred)
- **Lesson.** The project has broken its config and scope twice in two years (v1.0, v2.0). It is a good library source, but a risky one to depend on without pinning versions. (inferred)

### Notmuch-based setups: notmuch, alot, mu4e, sup

- **Model.** These tools index a local Maildir. A separate tool (mbsync, offlineimap, lieer and others) fetches the mail. Tags replace folders. Search is local and fast. (verified for mu: [mu README](https://github.com/djcb/mu) says it indexes into Xapian. The rest is inferred or long-standing practice.)
- **notmuch.** 0.40 was released on 2026-01-31 and 0.39 on 2025-03-15. (verified: [notmuch news](https://notmuchmail.org/news/))
- **alot** is Python with urwid. It is "command prompt driven": press `:` for a prompt with tab completion and usage help for every command. Other features: it can open terminal windows so you can edit mail without blocking, it uses Python hooks, and it manages "a write queue to the notmuch index". The default bindings include mouse wheel scrolling. 0.12 came out on 2025-12-11, more than a year after 0.11. (verified: [alot README](https://github.com/pazz/alot), `alot/defaults/default.bindings`, GitHub API)
- **mu and mu4e.** mu is a set of C++ tools over a Xapian index, and mu4e is its Emacs client. 1.14.4 came out on 2026-10-14. (verified: README, GitHub API)
- **sup** is Ruby, with "GMail-like thread-centered archiving, tagging and muting" and fast full-text search. Its own README admits it "does in general not play nicely with other mail clients" because not all changes sync back. Maintainers focus "on preserving the current feature set", and pull requests "for major new features are unlikely to be merged". 1.4 was released on 2026-04-21. (verified: [sup README](https://github.com/sup-heliotrope/sup), RubyGems)
- **JMAP to notmuch is a gap.**
  - `mujmap`, a bridge that syncs mail and tags between JMAP and notmuch, was last pushed on 2024-07-03. It is not archived, but looks dormant. (verified dates; dormancy inferred)
  - Newer sync daemons (`jmapsyncd`, `jma-mail`) have 0 to 3 stars. (verified: GitHub API)
  - Neverest has no JMAP backend yet. (verified)
  - So notmuch and mu4e users on Fastmail mostly sync over IMAP. That loses JMAP push and the fact that one message can live in several mailboxes (labels). (inferred)
- **Pain points.** Users have to assemble the setup from several tools. Tags drift from server state. Server-side features such as rules and aliases do not reach the local index. (inferred)

### New terminal clients, 2024 to 2026

- **Matcha.** Go and Bubble Tea, created 2025-07-28, 1.1k stars. 1.1.0 came out on 2026-10-10, with nightly builds as well. (verified: GitHub API) Features (verified: [docs.matcha.email](https://docs.matcha.email), [README](https://github.com/floatpane/matcha)):
  - A background daemon, and a macOS menu bar helper that shows the unread count.
  - Lua plugins with an in-app plugin "marketplace".
  - Split and threaded views, a calendar, PGP and S/MIME.
  - Markdown compose and AI rewrite.

  A Reddit post says it supports custom IMAP and JMAP accounts (unverified: [r/commandline](https://www.reddit.com/r/commandline/comments/1t3mkso/matcha_a_modern_secure_terminal_email_client/)). It is the most visible new TUI and is going the plugin-heavy, feature-rich way. (inferred)
- **HEY TUI.** In 2026, 37signals shipped an official CLI and TUI for HEY (`basecamp/hey-cli`, Go, created 2026-03-03, v1.10.0 on 2026-10-21). (verified: GitHub API) What the README says (verified: [hey-cli README](https://github.com/basecamp/hey-cli), [hey.com/agents](https://www.hey.com/agents/)):
  - The TUI covers Mail, Contacts, Calendar and Journal, uses HEY's web keyboard shortcuts, and shows them with `?`.
  - It "follows HEY live: a thread you archive on your phone leaves the screen a moment later."
  - CLI commands print JSON when piped and have a built-in `--jq` filter. `hey watch` prints one JSON line per new email.

  A large mail vendor building a TUI that reuses its web keymap is the closest product to mejl's idea, but it works only with HEY. (inferred)
- **elma.** Rust and ratatui. Backends: Gmail over IMAP, and any JMAP server, Fastmail included, with an API token. Features (verified: [lib.rs/elma-tui](https://lib.rs/crates/elma-tui)):
  - HTML rendering and server-side search.
  - "Scheduled actions": archive, delete and spam are staged, then applied together with `$`.
  - Snapshot tests for each major view.

  0.2.1 came out on 2026-08-18.
- **jmap-tui.** Go, "JMAP only. One protocol, done properly". It claims (all project claims, unverified, [README](https://github.com/CaffeinatedTech/jmap-tui)):
  - Mail is never written to disk.
  - Live push.
  - A 5-second undo on anything destructive, and a send delay you can cancel.
  - Server-side search, and a unified inbox across accounts.
  - "Every keystroke draws immediately ... even with 100,000 messages in a folder".

  Created 2026-09-28, 0 stars.
- **fm-cli.** Go, Fastmail-only JMAP client with a full-screen app, JSON commands, and a live `watch` over Fastmail's push connection. `fm-cli auth login` "opens Fastmail's consent page in your browser", stores tokens in the system keyring and refreshes them. An API token with Email scope is the fallback. It powers an Omarchy Fastmail plugin. (verified: [fm-cli README](https://github.com/ninepointlabs/fm-cli)) This is evidence that some kind of OAuth flow for third-party Fastmail clients exists. See open questions.
- **blorp.** Rust, Fastmail JMAP, six keys so far (`j k e o m q`), and an API token kept in the platform keyring. (verified: [blorp README](https://github.com/tanem/blorp))
- **Also seen:** `brieftaube` (WIP Rust JMAP TUI), `mail_cli` (Go; Gmail, M365 and Fastmail JMAP), `murat` (Go), `nitidus` (a ratatui frontend over himalaya, proof of concept). All are tiny. (verified: GitHub search)

### What terminal clients get wrong, again and again

1. **Speed is opt-in.** NeoMutt's header and body caches, aerc's `cache-state`, and meli's search index all start off. A new user's first experience is the slow one. (verified: docs above)
2. **The network blocks the UI**, or used to. aerc lists async IMAP and JMAP as a feature because older clients froze on a flaky network (project claim, aerc-mail.org). mejl should never do network I/O on the render path. (inferred)
3. **Users build the stack themselves.** Getting a good notmuch or mu4e setup takes a sync tool, an indexer, a frontend, an HTML converter, and a contacts tool. (inferred)
4. **Provider features are invisible.** None of these clients exposes server-side rules, aliases, vacation replies or Masked Email. himalaya's ManageSieve backend is the first step in that direction. (inferred)
5. **The mouse is an afterthought.** aerc and meli ship it off by default, and aerc had to fix it in 0.22. (verified)
6. **HTML is handed to w3m, lynx or a filter chain.** It works, but it depends on the user's setup. (verified for aerc and meli)

## Desktop, web and mobile clients

### At a glance

| Client | Platforms and stack | Providers | Local data model | What makes it feel fast | Triage ideas | Status (date) | Common complaints |
|---|---|---|---|---|---|---|---|
| Superhuman Mail | Web, desktop, mobile; web tech | Gmail, Outlook | Local DB in app or browser, offline queue | Under 50 ms target, preload and prerender, Cmd+K, shortcut hints | Done (`e`), Remind Me (`h`), split inbox, Send + Mark Done | Owned by Superhuman (formerly Grammarly) since 2025 | Price, steep first week (unverified) |
| HEY | Web, desktop, mobile, CLI/TUI | HEY only | Server-rendered web (unverified) | Search shows the first 7 results as you type | Screener, Imbox, Feed, Paper Trail, Reply Later, Set Aside, Bubble Up | Active, TUI in 2026 | Closed ecosystem (inferred) |
| Fastmail web | Web plus Fastmail's own apps; Overture JS | Fastmail | IndexedDB cache that speaks JMAP, full offline | Local-first, shared-worker push, on-device search fallback | Archive `y`, snooze `b`, undo `z` for the last 10 actions, pin | Offline GA 2025-08-26 | (none collected) |
| Mimestream | macOS native (Swift, unverified) | Gmail | Local, Gmail API direct, no middle server | Native app | Server-side Gmail filters in the app, undo send | Active | Mac and Gmail only (inferred) |
| Spark | Mac, Win, iOS (Android unverified) | Most providers | Server-side processing for push | Smart Inbox | Smart Inbox, Gatekeeper | Active; CLI 2026-05-13 | Privacy of server-side data |
| Notion Mail | Web, Mac (unverified) | Gmail | Synced with Gmail | Cmd+K, `/` search | AI views | Shut down 2026-09-22 | Product was killed |
| Shortwave | Mac, Win, iOS, Android | Gmail-focused | Not documented | Command palette, shortcuts | Splits, bundles, Done, AI filters | Active | (none collected) |
| Thunderbird | Desktop, Android (K-9 code), iOS in development | IMAP, POP, EWS; JMAP on iOS/Android roadmaps | Per-folder mork DBs, moving to one SQLite DB (Panorama) | n/a | Filters, tags | Desktop 157.0 2026-09-30 | Large-folder speed, old architecture (inferred) |
| Apple Mail | macOS, iOS | IMAP, Exchange, iCloud | Local Envelope Index plus Spotlight (unverified) | Native | Categories (iOS) | Ships with OS | Search broken or slow on macOS 26 (user reports) |
| Mailspring | Electron UI + C++ sync engine | IMAP-based (provider list unverified) | Local store via sync process (SQLite unverified) | Separate native sync process | Snooze, send later, rules | 1.26.2, 2026-10-22 | Electron footprint (inferred) |
| Airmail | macOS, iOS | Most providers | Local | Native | Many actions | Airmail 26 needs iOS 26 | Price increase 2026 (unverified) |
| Canary | Mac, iOS, Win, Android | Most providers | Not documented | n/a | AI sorting | Active | (none collected) |
| Marco | iOS, Mac, web; React Native | Any IMAP | IndexedDB or SQLite, offline-first | Local metadata, "single-digit ms" search (project claim) | Unified inbox | New, bootstrapped | (none collected) |

### Superhuman Mail

- **Ownership.** Grammarly bought Superhuman in July 2025. On 2025-10-29 Grammarly renamed the company Superhuman, with Superhuman Mail as one product in a suite. (verified: [Business Wire](https://www.businesswire.com/news/home/20251029236844/en/Grammarly-Rebrands-Company-as-Superhuman-Introduces-Superhuman-Suite-and-Superhuman-Go), [The Verge](https://www.theverge.com/news/808472/grammarly-superhuman-ai-rebrand-relaunch); the July date is unverified, from [Carly](https://www.usecarly.com/blog/superhuman-acquired-by-grammarly/))
- **Providers.** It "runs on top of Gmail and Outlook". (verified: [Superhuman blog](https://blog.superhuman.com/superhuman-is-built-for-speed/))
- **Latency targets.** "Superhuman Mail treats the 100ms rule as the maximum, and actually aims for latency less than 50ms whenever possible." A 2019 tweet from the CEO, quoted in the same post, says a new renderer shows emails "in 1-2 Chrome frames! (<32 ms)". (verified)
- **How it gets there** (verified: same post):
  - It "keeps a database of your emails stored in your app or browser".
  - It "preloads and prerenders the email threads that you're most likely to view soon".
  - It uses minimal animation and prefers keyboard to gestures.
  - It shows "keyboard shortcut hints right in the user interface", and Cmd+K opens a command library when you don't know the shortcut.
- **How it measures** (verified: [Performance metrics for blazingly fast web apps](https://blog.superhuman.com/performance-metrics-for-blazingly-fast-web-apps/), 2019):
  - Start the timer at the OS event timestamp, not inside the handler. This catches time the main thread was busy.
  - Stop the timer when the frame is ready to flush.
  - Throw away samples taken while the page was hidden.
  - Track the percentage of events under target, not percentiles.
  - Bucket results into under 50 ms, under 100 ms, under 1000 ms, and "terrible". The 50 ms bucket shows regressions first.
- **Offline and optimistic design** (verified: [Architecting a web app to "just work" offline](https://blog.superhuman.com/architecting-a-web-app-to-just-work-offline-part-1/)):
  - Every action is a "modifier". Its `modify()` changes local data synchronously, and its `persist()` sends the change to the server asynchronously and must be idempotent.
  - Modifiers go into one queue per thread, are saved to disk, and wait while offline.
  - A single process persists modifiers for all tabs.
  - If a modifier fails for good, it is removed and the UI rebuilds without it.
- **Triage** (verified: [Superhuman help center](https://help.superhuman.com/hc/en-us/articles/46005796611341-Clear-Your-Inbox)):
  - `e` marks Done, and `h` sets a Remind Me.
  - Custom split inboxes.
  - "Send + Mark Done" sends a reply and moves the thread to Done in one step ([Mark Done](https://help.superhuman.com/hc/en-us/articles/47439134613773-Mark-Done)).
  - "Select All from Here", plus Snippets, Instant Intro, Unsubscribe and Block.
- **Complaints.** Price, a steep first week, and AI drafts that still need checking. (unverified: [Carly review](https://www.usecarly.com/blog/superhuman-review/))
- **For mejl.** Copy the modifier pattern, the latency buckets, and the in-UI shortcut hints. A TUI has no browser frame to wait for, so "keypress to terminal flush" is the matching measure. (inferred)

### HEY

- **Features** (verified: [hey.com/features](https://www.hey.com/features/)):
  - The Screener asks whether a first-time sender may reach you.
  - The Imbox holds important mail. The Feed holds newsletters, and Paper Trail holds receipts.
  - Reply Later and Set Aside are piles at the bottom of the screen. Focus & Reply lets you answer the Reply Later pile one after another.
  - Bubble Up hides an email and brings it back to the top later. Mute stops new replies in a thread from showing up as new mail.
  - Rename Subjects changes a thread's subject for you only.
  - The Spy Pixel Blocker blocks trackers and shows who tried to track you.
  - Bundles collapse a sender's mail into one row.
- **Recent changes** (verified: [hey.com/new](https://www.hey.com/new/)). "Search is now up to 7x faster", and the first seven results now appear as you type. Search can be refined by sender, recipient, date and label. Many changes are keyboard additions, such as `u` for mark unread and `Shift+W` to write in a new window.
- **TUI and CLI.** See the terminal section. HEY reused its web shortcuts in the TUI. (verified)
- **For mejl.** The Screener, Reply Later and Set Aside are workflow ideas, not protocol features. mejl can build them from Sieve rules, JMAP keywords and mailboxes. (inferred)

### Fastmail web: the bar mejl must beat

- **Framework.** Built on Overture, Fastmail's open source JS framework, which aims at "performance at, or surpassing, native apps". (verified: [github.com/fastmail/overture](https://github.com/fastmail/overture))
- **Offline.** Public beta on 2024-12-16 ([blog](https://www.fastmail.com/blog/offline-in-beta/)). Full offline for all customers, on the web and in the apps, since 2025-08-26 ([blog](https://www.fastmail.com/blog/fastmail-works-offline/)). (verified)
- **Architecture** (verified: [Building offline: general architecture](https://www.fastmail.com/blog/offline-architecture/)):
  - All app data goes through JMAP. A caching layer sits between the app and the server and speaks JMAP on both sides.
  - The cache tries to run each request locally. As soon as one method call needs the server, it sends the rest of the request there.
  - It runs in a shared worker, so all tabs share one persistent EventSource push connection.
  - Storage is IndexedDB, with one object store per data type. Keys are packed `[accountId, id]` ArrayBuffers. Each record has created and updated modseqs.
  - A per-account, per-type modseq doubles as the JMAP state string. Deleted records stay as tombstones until a purge threshold, and a "highest purged modseq" marks how far back changes can be computed.
- **Sync back to the server** (verified: [Building offline: syncing changes](https://www.fastmail.com/blog/offline-sync/)):
  - A change log plus per-record server state, stored as patches.
  - Repeat edits collapse into one update. A change that ends up doing nothing is dropped.
  - Creates are ordered before the records that depend on them.
  - Policy: "Last write wins" and "All updates are patches", so edits to different properties merge. The client resolves conflicts itself instead of asking the user.
- **Mail storage and search** (verified: [Building offline: mail storage](https://www.fastmail.com/blog/offline-mail-storage/)):
  - Mutable metadata (mailboxes, keywords, thread) is stored apart from immutable content.
  - Loading is staged: bare IDs first, then metadata and basic headers in batches so any folder can render, then bodies. Bodies are kept for pinned and recent mail, or for all mail if you opt in.
  - A per-mailbox index uses added and removed modseqs, so changes since a given state can be found with a range query.
  - The search index is built on the device and supports phrase search. Search goes to the server first and uses the local index only when offline. Limits: no attachment text, no stemming, prefix match by default.
- **Keyboard** (verified: [Fastmail keyboard shortcuts](https://www.fastmail.help/hc/en-us/articles/360058753534-Keyboard-shortcuts)):
  - Lists: `j`/`k` to move, `o` or Enter to open, `x` to select, `Shift+x` to select a range.
  - Actions: `y` archive, `[` and `]` to archive and go to the previous or next message, `d` trash, `!` spam.
  - Moving and labels: `m` or `v` to move to a folder, `l` for labels, `g` to go to a folder.
  - Snooze and undo: `b` then `1` to `7` to snooze, `z` to undo up to the last 10 actions.
  - Help and navigation: `/` to search, `?` (or a double tap of Ctrl) for help, `Shift+g` then a letter to switch apps.
  - Compose: `Ctrl+Enter` to send, `Ctrl+Shift+Enter` to schedule.
  - No command palette is documented.
- **For mejl.** Fastmail has already solved the hard parts of a local-first JMAP client in the browser, and has written up how. mejl should copy this design in Rust with SQLite in place of IndexedDB. Its default keymap should match this one. mejl can beat the web app on startup time, memory use, terminal workflow, rule editing and Masked Email flows. Raw data speed will be hard to beat, because both clients read from a local store. (inferred)

### Mimestream

- **Platform.** macOS only (12 or newer). It uses the Gmail API directly "with no intermediary sync service", and data and tokens stay on the device. (verified: [mimestream.com](https://mimestream.com/))
- **Provider features in the client.** It manages server-side Gmail filters, Gmail aliases, labels with colors, vacation responses and inbox categories in the app. It has undo send (`Cmd+Z`) and send-and-archive (`Shift+Cmd+Return`). (verified: same page)
- **For mejl.** This is the closest model for "manage provider rules from the client". Mimestream treats Gmail's own features as first-class, which is what mejl should do for Sieve, aliases and Masked Email. (inferred)

### Spark

- **Features.** Smart Inbox sorts mail into Personal, Notifications and Newsletters. Gatekeeper screens new senders. The "new Spark" replaced Spark Classic. (verified: [Spark blog](https://sparkmailapp.com/blog/spark-vs-spark-classic))
- **Privacy.** "Spark syncs the subject and a part of your message, encrypts this information and stores it on its secure servers" to send push notifications. (verified: [Readdle support](https://support.readdle.com/spark/privacy/privacy-explained)) Users have objected to server-side storage of credentials for years. (unverified: Reddit threads)
- **Spark CLI** (2026-05-13). It gives AI agents access to mail, contacts and calendar by reading Spark Desktop's local storage. Spark Desktop must be running. Read access is free, and triage actions are paid. (verified: [Spark blog](https://sparkmailapp.com/blog/introducing-spark-cli))
- **For mejl.** JMAP push straight from Fastmail removes any need for a middle server. mejl should never relay mail or credentials through a server of its own. (inferred)

### Notion Mail (shut down)

- **History.** Launched on 2025-04-15 as a Gmail-based inbox with AI views and keyboard navigation (verified: [Notion blog](https://www.notion.com/blog/introducing-notion-mail)). Notion announced on 2026-06-25 that it would close the product on 2026-09-22 "in favor of its AI agent offering" (verified: [TechCrunch](https://techcrunch.com/2026/06/25/notion-mail-shuts-down-amid-agent-takeover/)). Mail stayed in Gmail, because Notion Mail synced both ways. (unverified: [heise](https://www.heise.de/en/news/Notion-Mail-is-shutting-down-What-users-need-to-do-now-11345988.html))
- **Keys.** Cmd+K for commands, `/` for search, `?` for shortcuts. (unverified: user posts)
- **Lesson.** A client that keeps all state on the server can disappear without losing mail. That is good for users and a reason mejl should keep its local cache disposable. The shutdown also shows that a product's future can hinge on an AI strategy. (inferred)

### Shortwave

- **Features.** Splits (tabs for senders, labels or queries), bundles for promotions and updates, Done and todos, a command palette, and shortcuts for most actions. Also AI filters written "in plain English" that label, star or archive. Runs on iOS, Android, Mac and Windows, and is Gmail-focused. (verified: [shortwave.com](https://www.shortwave.com/))
- **For mejl.** Shortwave's plain-English filters are the AI version of rules. mejl's version is a readable rule builder that writes Sieve. (inferred)

### Thunderbird and its JMAP plans

- **Desktop roadmap** (updated 2026-07-23). Active work: Exchange (EWS and Graph), the Panorama global database, add-on APIs, encryption, and settings. Shipped: Exchange email (November 2025) and an Account Hub. **JMAP is not listed.** (verified: [desktop roadmap](https://roadmaps.thunderbird.net/en-US/desktop/))
- **iOS roadmap.** "JMAP Support - Exploration" is active and about 80% done. Full JMAP is planned as "one of the first protocols shipped beyond IMAP". IMAP and the first user release come first. (verified: [iOS roadmap](https://roadmaps.thunderbird.net/ios/))
- **Android.** "JMAP Support Exploration" is on the 2026 roadmap. Thunderbird says the Android code, built on K-9 Mail, "is 15 years old". Thundermail, Thunderbird's own mail service, "includes support for JMAP". (verified: [roadmap](https://roadmaps.thunderbird.net/android/), [State of the Thunder 14](https://blog.thunderbird.net/2025/12/state-of-the-thunder-14-the-2026-mobile-roadmap/))
- **A conflict.** An eM Client forum post says the desktop and Android versions "have JMAP explicitly on the 2026 roadmap" ([forum](https://forum.emclient.com/t/jmap-support-wanted/78419/14)). The official desktop roadmap does not list it. I trust the roadmap. Desktop JMAP is not planned in public as of 2026-07-23. (verified absence; the forum claim is unverified)
- **Panorama** replaces "one database per folder" with one global SQLite database (verified: [Panorama docs](https://source-docs.thunderbird.net/en/latest/panorama/index.html)):
  - "LiveView" collections update themselves and convert messages to plain JS objects "for performance reasons".
  - A sparse adapter loads list rows on demand.
  - Full-text search will replace the old Gloda index.
  - The docs note that IMAP UIDs, which are unique only within a folder, cannot be keys in a global DB.
- **For mejl.** Design one global message table from day one, keyed by JMAP email ID, with mailbox membership as a separate relation. JMAP email IDs are already global and immutable, unlike IMAP UIDs (inferred). Also make the message list a sparse, windowed view over a query, as Panorama's LiveView does. (inferred)

### Apple Mail

- **Search problems.** Apple Community threads after macOS 26 Tahoe report search that finds only the latest few messages, or takes 20 seconds. One reported cause is that search now covers only the current folder by default. (unverified: user reports, [thread 1](https://discussions.apple.com/thread/256154843), [thread 2](https://discussions.apple.com/thread/256282266))
- **For mejl.** Search that relies on an OS-wide indexer fails in ways the app cannot see or fix. mejl should own its index, or rely on Fastmail's server search and say clearly which one answered. (inferred)

### Mailspring

- **Architecture.** An Electron and React UI, with a separate C++ sync engine based on Mailcore2 that "runs locally on your computer". It claims "roughly half the RAM and CPU of Nylas Mail" and idles "with almost zero CPU Wakes" (project claim). "Mailspring does not send your email credentials to the cloud." 1.26.2 came out on 2026-10-22. (verified: [Mailspring README](https://github.com/Foundry376/Mailspring), GitHub API)
- **For mejl.** Splitting a sync engine from the UI works, and a single Rust binary can do the same with a sync task that writes to SQLite while the UI only reads. (inferred)

### Airmail, Canary and newer entrants

- **Airmail.** Apple-only, and an Apple Design Award winner in 2017. Airmail 26 requires iOS 26 (verified: [App Store](https://apps.apple.com/us/app/airmail-lightning-fast-email/id918858936?mt=12), [Airmail help](https://help.airmailapp.com/en-us/article/airmail-ios-26-1ml4tbr/)). Users complained about a large subscription price increase in 2026. (unverified: [Reddit](https://www.reddit.com/r/macapps/comments/1rq82ec/airmail_233_subscription_price_increase/))
- **Canary.** Cross-platform, with AI features and PGP plus "SecureSend" encryption. (verified: [canarymail.io](https://canarymail.io/features/ai))
- **Marco.** IMAP-native and offline-first, built with React Native and Expo, TanStack DB, and IndexedDB on web or SQLite on mobile. A Node backend handles push. No AI drafting. The author tried several sync engines and found they "either choked on performance or added complexity I didn't need". The author also says "search is single-digit ms" (project claim). (verified: author posts on [HN](https://news.ycombinator.com/item?id=47519302), [HN](https://news.ycombinator.com/item?id=45436555))
- **Zero (0.email).** Open source AI email client. The only release is v0.1 (2025-07-18), and the last push was 2026-05-26, so it is (inferred) stalled. (verified dates: GitHub API)
- **Inbox Zero.** Open source AI inbox tool. Very active: a desktop release came out on 2026-10-21. (verified: GitHub API)

### A 2026 trend: agent and terminal access

HEY (CLI and TUI), Spark (CLI), Superhuman (a "Mail MCP" listed in its site nav) and Fastmail (an MCP server, announced 2026-04-22) all added machine-friendly interfaces this year (verified: pages linked above, [Superhuman blog nav](https://blog.superhuman.com/superhuman-is-built-for-speed/), [Fastmail blog](https://www.fastmail.com/blog/an-mcp-server-for-fastmail/)). For mejl this means:

- A small scriptable surface, such as `mejl --json` for queries and actions, will likely be expected later. (inferred)
- It should not be in the first milestone (YAGNI). Keep the core in a library so a CLI is cheap to add. (inferred)
- The Fastmail MCP server already covers agent use, so mejl does not need its own. (inferred)

## What makes a client feel fast

| Technique | Who does it (evidence) | How mejl would do it (inferred) |
|---|---|---|
| Render only from a local store | Superhuman local DB; Fastmail IndexedDB cache; Marco | SQLite store per account. The UI reads only from it, never waits on the network. |
| Optimistic mutations with an ordered queue | Superhuman modifiers (per-thread queue, idempotent persist); Fastmail change log of patches | Every action writes locally and to a durable outbox, then a sync task sends `Email/set` patches. Repeat edits collapse. Failures roll back where you can see them. |
| Push instead of polling | Fastmail shared-worker EventSource; aerc JMAP EventSource; fm-cli and jmap-tui | One EventSource connection per account, then `*/changes` calls to catch up. |
| Incremental sync by state | Fastmail modseq and tombstones; JMAP `/changes` | Store the JMAP `state` per type. Call `Email/changes`, `Mailbox/changes` and `Thread/changes`. Use `Email/queryChanges` for open views. |
| Staged loading | Fastmail: IDs, then metadata and headers, then bodies | Fetch list properties first. Fetch bodies for the visible thread and the next few. Load the rest in the background. |
| Prefetch the likely next thread | Superhuman "preloads and prerenders" | When the cursor moves, fetch and render the bodies of the next N rows. Cache rendered text. |
| Measure latency in buckets | Superhuman under 50, 100 and 1000 ms, plus "terrible" | Log keypress-to-flush time per action into the same buckets. Add a debug overlay. Fail CI if local actions slow down. |
| Minimal animation | Superhuman | No spinners for local actions. Status-line hints only for network work. |
| Shortcut hints in the UI | Superhuman; aerc keyhint popup; Fastmail button tooltips | A footer with context keys. A popup after a prefix key. A `?` overlay. |
| A fast search path | HEY results as you type; Fastmail server-first with local fallback | Instant local filter over cached metadata. Server `Email/query` for full search. Label which one answered. |
| Separate sync from UI | Mailspring C++ sync process; Fastmail shared worker | A tokio sync task and a UI thread talking over channels. The UI thread never does I/O. |

## Lessons for mejl

### Design principles

1. **Local first, always.**
   - The UI renders from the local store. The network only reconciles.
   - The store is a cache: deleting it must never lose user data, because JMAP state lives on the server.
   - Outbox entries that are still waiting are the one exception. They need durable storage and a clear "unsent changes" indicator.

   (inferred from Fastmail, Superhuman, Notion Mail)
2. **Design for JMAP, not IMAP.**
   - Use global immutable IDs, many mailboxes per email, keywords, and the `/changes` and `/queryChanges` methods.
   - Batch method calls and use back-references, so one request can do query, get and thread expansion.
   - Do not copy IMAP-era structures such as per-folder databases or UID keys. Panorama shows how costly those are to remove later.

   (inferred)
3. **One provider, done deeply.** Mimestream and Superhuman show that a client built for one provider can beat generic ones. mejl should surface Fastmail-only features: Masked Email, Sieve, aliases and identities, snooze, pins, and the "Files" and "Notes" apps only if users ask for them. (inferred)
4. **Treat latency as a budget.**
   - Under 16 ms (one frame at 60 Hz) for any action served from the local store.
   - Under 50 ms for opening a cached thread, which matches Superhuman's internal target.
   - Network actions are optimistic, so they get the same budget for the local effect.

   The budgets are inferred. The 50 ms figure is verified for Superhuman.
5. **Make every action undoable.** Destructive actions get undo: Fastmail's `z` covers the last 10 actions, and jmap-tui gives 5 seconds. Sending gets a delay you can cancel. Actions that cannot be undone, such as destroying an email for good, need a confirmation. (verified examples; principle inferred)
6. **Three layers of keyboard.**
   1. Single-key verbs.
   2. A `:` command line with completion (aerc, alot) and a fuzzy command palette (Superhuman Cmd+K, Shortwave, Notion Mail).
   3. Contextual help: `?` plus a popup after prefix keys.

   Every palette entry shows its key, so the palette teaches the shortcuts. (inferred from Superhuman's shortcut hints)
7. **The mouse is a convenience, but a working one.**
   - Turn it on by default: click to select, double-click to open, wheel to scroll, click on footer key hints.
   - Every mouse action must have a key, and no action may need the mouse.

   aerc and meli default it off, and that is a known weak spot. (inferred)
8. **Stable, small config.** One TOML file, defaults that need no setup, and versioned migrations. himalaya broke its config at v1 and again at v2. (inferred)
9. **Privacy by default.**
   - Remote images off by default, following HEY's spy pixel blocker.
   - No servers of mejl's own. Credentials go in the OS keyring, as fm-cli and blorp do.

   (inferred)

### Concrete feature ideas

- **Default keymap matches Fastmail web.**
  - Lists: `j`/`k` to move, `o` or Enter to open, `x` and `Shift+x` to select.
  - Actions: `y` archive, `[`/`]` to archive and step, `d` trash, `!` spam, `s` pin.
  - Moving: `m` or `v` to move, `l` for labels, `g` to go to a mailbox.
  - Snooze: `b` then `1` to `7`.
  - Other: `z` undo, `/` search, `c` compose, `r`/`a`/`f` reply, reply all, forward, `?` help.

  Offer a vim-leaning preset as an option. (verified source for Fastmail keys; choice inferred)
- **"Rule from this message."** One key opens a rule builder filled in from the selected message (sender, list-id, subject). It previews which existing messages match (via `Email/query`), then writes Sieve. This is the Mimestream filters idea, built on Sieve. (inferred)
- **A Screener built on Sieve.** First-time senders go to a "Screener" mailbox. Approving a sender adds them to an allow list that Sieve uses, and blocking adds them to a block list. This works like HEY's Screener and Spark's Gatekeeper, but runs on the server, so it applies on every device. (inferred; depends on the Fastmail Sieve findings)
- **Masked Email where it is needed.**
  - In compose, pick or create a masked address as the From.
  - From a message sent to a masked address, a key can disable or delete that address.
  - The message list shows which masked address a message arrived at.

  (inferred; API details belong in the Masked Email doc)
- **Split inbox from saved queries.** Tabs defined by JMAP filters (VIPs, lists, notifications). They are cheap because the local store answers them, as Superhuman and Shortwave do. (inferred)
- **Reply Later and Set Aside as keywords.** Use JMAP keywords (for example `$replylater`) or mailboxes, plus a "Focus & Reply" mode that steps through the pile. Keywords sync across devices but are invisible in Fastmail web. Check whether Fastmail web shows custom keywords. (inferred; open question)
- **Send + Done.** Send a reply and archive the thread in one key, like Superhuman's Send + Mark Done. (verified source; idea inferred)
- **Staged bulk actions.** Like elma's `$` commit: mark several messages for archive or delete, review, then apply. This fits terminal users who like to batch. (verified source; idea inferred)
- **Compose with `$EDITOR`.**
  - Default: suspend the TUI and run the editor full screen. It is simple and works with any editor.
  - Option: an embedded terminal pane, as aerc and meli do.
  - Keep a quick inline composer for short replies, as himalaya-tui does.

  (inferred)
- **HTML rendering.** Prefer the `text/plain` part. Turn HTML into readable text in-process, with links as numbered footnotes. Offer "open in browser" as an escape hatch. Inline images only when the terminal supports a graphics protocol, as in aerc 0.22's sixel option. (inferred)
- **Latency overlay.** A debug toggle that shows the last action's keypress-to-flush time and bucket. This makes the 100 ms rule something you can test. (inferred)
- **Account setup from an email address.** Find the JMAP session URL from the address, then authenticate with an API token or Fastmail OAuth if one is available (see open questions), as fm-cli and himalaya-tui do. (inferred)

### Anti-patterns to avoid

| Anti-pattern | Evidence | What to do instead |
|---|---|---|
| Speed features that are off by default | NeoMutt hcache and bodycache, aerc `cache-state`, meli search index | Caching and prefetch on by default, with tuned limits |
| Caches that never clean up | aerc `cache-blobs` "is never purged automatically" | Size-capped blob cache with LRU eviction, and drop items when the server destroys them |
| Per-folder databases and folder-local keys | Thunderbird's Panorama rewrite; IMAP UIDs cannot be global keys | One message table keyed by JMAP ID, with a mailbox membership relation |
| Network I/O on the UI path | Why aerc advertises async IMAP and JMAP | Keep the UI thread free of I/O. Use channels to a sync task. |
| Search that depends on an OS indexer | Apple Mail search failures on macOS 26 (user reports) | Server `Email/query`, plus a local index mejl controls |
| Server-side relay of credentials or content | Spark stores subjects and snippets on its servers for push | Direct JMAP push from Fastmail. No mejl servers. |
| Config and scope churn | himalaya v1.0 and v2.0 breaking changes | Versioned config with automatic migration |
| A bag of plugins in place of a core | Matcha's 35+ plugin marketplace (verified count); risk is inferred | Build the core features in. Add hooks only when a second real use case shows up (YAGNI) |
| Building on a platform's strategic whim | Notion Mail closed after about 17 months | Stay small and independent, and keep the local cache disposable |
| A mouse path that breaks without anyone noticing | aerc 0.22 fixed mouse in the viewer | Test mouse handling the same way as keys, for example with snapshot tests like elma's |

## Feature comparison

Legend: Y = yes, N = no or not found, P = partial, ? = unknown. A bare cell rests on the sources cited in the sections above, and cells that were not checked carry their own label. Mouse support in GUI apps is assumed (inferred). The Fastmail web Rules and Masked Email cells come from the product's settings, which this doc did not re-check (unverified here; see [fastmail-masked-email.md](fastmail-masked-email.md) for Masked Email). "Rules UI" means editing server-side rules inside the client. "Masked Email" means creating or managing Fastmail Masked Email inside the client.

| Client | JMAP native | Offline local cache | Push | Undo for actions | Command palette or `:` line | Rules UI | Masked Email | Mouse |
|---|---|---|---|---|---|---|---|---|
| aerc | Y | P (opt-in state and blob cache) | Y (EventSource) | ? | Y (`:` commands) | N | N | P (opt-in) |
| NeoMutt | N (unverified) | P (opt-in hcache and bodycache) | ? | ? | Y (`:` and `<enter-command>`, unverified) | N | N | ? |
| meli | Y ("functional") | P (SQLite caches) | ? | ? | Y (unverified) | N | N | P (opt-in) |
| himalaya CLI | Y | P (via pimdir and neverest, no JMAP sync yet) | N | n/a | n/a | P (ManageSieve backend, 2.2.1) | N | n/a |
| himalaya-tui | Y | ? | ? | ? | ? | N | N | ? |
| alot (notmuch) | N | Y (local index) | N | ? | Y (`:` prompt) | N | N | P (wheel) |
| mu4e | N | Y (local index) | N | ? | Y (Emacs M-x) | N | N | Y (Emacs) |
| jmap-tui | Y | N (by design) | Y (project claim) | Y (5 s, project claim) | ? | N | N | ? |
| HEY TUI | N (HEY API) | ? | Y (live) | ? | ? | N | N | ? |
| Superhuman | N (Gmail, Outlook APIs) | Y | ? | ? (unverified) | Y (Cmd+K) | N | N | Y |
| Fastmail web | Y (JMAP throughout) | Y (GA 2025-08) | Y (EventSource) | Y (`z`, last 10) | N (not documented) | Y (in settings) | Y | Y |
| Mimestream | N (Gmail API) | Y (local, unverified depth) | ? | Y (undo send) | ? | Y (Gmail filters) | N | Y |
| Spark | N | ? | Y (via Spark servers) | ? | ? | ? | N | Y |
| Thunderbird | N (desktop) | Y | P (IMAP IDLE, unverified) | ? | N | N (local filters only; Sieve via add-on, unverified) | N | Y |
| Mailspring | N | Y (local SQLite, unverified) | ? | ? | ? | N ("Mail Rules" run in the client, inferred) | N | Y |

The gap mejl can fill is the row nobody has: a JMAP-native terminal client that works offline and has push, undo, a palette, and Rules and Masked Email built in. (inferred)

## Open questions

1. **Does Fastmail expose ManageSieve or JMAP for Sieve Scripts to third-party clients, and with what limits?** JMAP for Sieve is [RFC 9661](https://www.rfc-editor.org/rfc/rfc9661), September 2024 (verified). Whether Fastmail offers either protocol is not checked here (unverified). This decides whether "rule from this message" and the Sieve Screener can work. Resolve in the Fastmail API doc: check fastmail.com/dev and the JMAP session capabilities with a test account.
2. **What OAuth flow does fm-cli use with Fastmail?** Is client registration public, and does it support PKCE for a native app? Resolve by reading fm-cli's auth code and Fastmail's developer docs. This decides whether mejl can skip API tokens. (unverified)
3. **Does Fastmail web show custom JMAP keywords**, such as a `$replylater` keyword set by mejl? Or should Reply Later and Set Aside use mailboxes? Resolve by setting a keyword on a test account and checking the web UI. (unverified)
4. **What is aerc's JMAP offline status after 0.22?** FOSDEM 2025 listed full offline as future work. Resolve by reading `worker/jmap` and the 0.21 changelog in the cloned repo. (unverified)
5. **Does Matcha really support JMAP**, and how does its daemon cache? The only claim is a Reddit post. Resolve by reading `floatpane/matcha` backends. (unverified)
6. **How fast are terminal clients, measured?** No benchmarks were found for any client in this survey. Resolve by building a small harness: a Fastmail test account (or a local Stalwart or Cyrus JMAP server) with 50k messages, then measure startup, folder open, thread open and archive latency for aerc, meli, himalaya-tui and mejl prototypes. (open)
7. **Which Rust crates should mejl use for JMAP and ManageSieve?** Options include io-jmap, io-managesieve, jmap-client and others. Resolve in the Rust crates doc. Weigh the maturity of the young Pimalaya crates and their I/O-free design against more established options. (open)
8. **Is Thunderbird desktop planning JMAP?** The official roadmap says no as of 2026-07-23, and a forum post says yes. Recheck roadmaps.thunderbird.net each quarter. (verified absence, may change)
9. **How do Superhuman's undo per action and its onboarding work today?** These came from secondary reviews and were not checked against Superhuman's help center. Low priority. (unverified)
10. **What makes users leave HEY, Spark and Superhuman?** Complaints here come from reviews and Reddit, not from systematic data. If triage features matter for prioritizing, do a focused pass over r/fastmail, r/commandline and HN threads. (unverified)
