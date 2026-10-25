# Fastmail as a platform for a third-party JMAP client

Research date: 2026-10-23. Web pages were fetched on that date unless a source says otherwise. No call was made to the user's real mailbox or to Fastmail's MCP tools. Public discovery endpoints on `api.fastmail.com` were read with unauthenticated `curl` requests, which return metadata only.

## Summary

- **Rules (Sieve) are the biggest gap.** Fastmail has no public API for rules or Sieve. The Sieve FAQ says Fastmail "does not support the managesieve extension" and "the only way to modify your script is by logging in to the web interface" (verified: [Sieve FAQ](https://www.fastmail.help/hc/en-us/articles/360058753814-Sieve-frequently-asked-questions)). Neither the OAuth scope list nor the API token scope list mentions rules or Sieve (verified: [Fastmail dev docs](https://www.fastmail.com/dev/), [API tokens help](https://www.fastmail.help/hc/en-us/articles/5254602856719-API-tokens)). The web app uses a private JMAP capability, `https://www.fastmail.com/dev/rules`, with `Rule/get`, `Rule/set`, `SieveBlocks/get` and `SieveBlocks/set`. Third-party tools reach it with a bearer token and session cookie copied from the browser, and writes "usually need browser-session authentication, not a normal API token", sometimes with a "sudo" re-auth (verified: README and source of [dvcrn/fastmail-rules-cli](https://github.com/dvcrn/fastmail-rules-cli) and [fuyu0425/Fastmail-Sieve-Sync](https://github.com/fuyu0425/Fastmail-Sieve-Sync)). The [API Developer Policy](https://www.fastmail.com/policies/api-developer-policy/) forbids "Using unpublished APIs" and "bypassing or circumventing Fastmail protocols and access controls". A distributed mejl cannot ship that path. Seamless rules management needs Fastmail to publish an API (for example [RFC 9661](https://www.rfc-editor.org/rfc/rfc9661.html), JMAP for Sieve Scripts). Cyrus, Fastmail's server, already has JMAP Sieve code and tests (verified: [cyrus-imapd `JMAPSieve.pm`](https://github.com/cyrusimap/cyrus-imapd/blob/master/cassandane/Cassandane/TestSuite/JMAPSieve.pm)), and the IETF OAuth draft edited by Fastmail's Neil Jenkins maps `urn:ietf:params:jmap:sieve` and ManageSieve to the open `mail` scope (verified: [draft-ietf-mailmaint-oauth-public-06](https://datatracker.ietf.org/doc/draft-ietf-mailmaint-oauth-public/) sections 5.1.5 and 5.4.1). So the ask is about policy, not new engineering (inferred). Until then mejl can only author Sieve locally and help the user paste it in.
- **Masked Email is the opposite case.** It is a documented, public JMAP extension (`https://www.fastmail.com/dev/maskedemail`) with its own OAuth scope and API token checkbox (verified: [Fastmail dev docs](https://www.fastmail.com/dev/), [API tokens help](https://www.fastmail.help/hc/en-us/articles/5254602856719-API-tokens)). mejl can support it fully with an API token or a manually registered OAuth client. Two gaps: the `dev/maskedemail` scope is missing from the new open OAuth metadata, and Basic plans include Masked Email but no API tokens (verified: discovery metadata, [pricing](https://www.fastmail.com/pricing/)). See [fastmail-masked-email.md](fastmail-masked-email.md) for detail.
- **Core mail is solid and standard.** Mail ([RFC 8621](https://www.rfc-editor.org/rfc/rfc8621.html)), submission, vacation response and contacts ([RFC 9610](https://www.rfc-editor.org/rfc/rfc9610.html)) are public JMAP. Calendars are CalDAV only for third parties: Fastmail will open JMAP calendars "as soon as the specification is finalized" (verified: [Fastmail dev docs](https://www.fastmail.com/dev/)). That draft (-33) is now in the RFC Editor queue (verified: IESG state "RFC Ed Queue" on the [draft-ietf-jmap-calendars datatracker page](https://datatracker.ietf.org/doc/draft-ietf-jmap-calendars/)).
- **OAuth is moving, and Fastmail expects it for distributed apps.** The developer page says "If you are building an app to distribute to others, you should implement OAuth", and that API tokens are "for testing purposes, or building an app for just yourself" (verified: [Fastmail dev docs](https://www.fastmail.com/dev/)). It still says OAuth clients "are currently registered manually by contact with Fastmail developers" (verified: [Fastmail dev docs](https://www.fastmail.com/dev/#registration)). But `api.fastmail.com` now publishes OAuth discovery metadata with a dynamic client registration endpoint (`/oauth/register`) and the interoperable scopes `urn:ietf:params:oauth:scope:mail`, `...:contacts` and `...:calendars` (verified: `curl https://api.fastmail.com/.well-known/oauth-authorization-server`, 2026-10-23). This matches [draft-ietf-mailmaint-oauth-public](https://datatracker.ietf.org/doc/draft-ietf-mailmaint-oauth-public/), edited by Fastmail's Neil Jenkins, which requires open registration with no prior relationship. If open registration works as the draft says, mejl can ship OAuth with a loopback redirect and PKCE, and needs no manual registration (inferred; not tested, to avoid creating a client on Fastmail's servers).
- **Push works through EventSource.** Third-party API tokens have been able to connect to `https://api.fastmail.com/jmap/event/` since about September 2022 (verified: Fastmail staff in [JMAP-Samples issue 7](https://github.com/fastmail/JMAP-Samples/issues/7)). We found no evidence of JMAP over WebSocket ([RFC 8887](https://www.rfc-editor.org/rfc/rfc8887.html)) for third parties (inferred). Web Push (`PushSubscription`) needs a public HTTPS URL, so it does not suit a TUI.
- **Several Fastmail features have no public JMAP surface.** Snooze, saved searches, grouped views, the rules that power "Screening", and undo send are web-app features. Some have standard building blocks: keywords from [RFC 9979](https://www.rfc-editor.org/rfc/rfc9979.html) (`$muted`, `$followed`, `$memo`), mailbox attributes (`Snoozed`, `Scheduled`, `Memos`) and `maxDelayedSend` for delayed sending. Whether Fastmail exposes the matching behaviour to tokens must be tested against a live session.
- **The official MCP server is not a client backend.** It launched on 2026-04-22 at `https://api.fastmail.com/mcp` (verified: [Fastmail blog](https://www.fastmail.com/blog/an-mcp-server-for-fastmail/)). It exposes 30 task-level tools for AI assistants and nothing for Masked Email, rules or mailbox management. mejl should not build on it. mejl could expose its own local MCP server later that covers what Fastmail's lacks.
- **The UX baseline is a fast web app in several wrappers.** Fastmail's desktop app (October 2025) wraps the web app in Electron (verified: Fastmail's CPO under "Why Electron?" in the [HN launch thread](https://news.ycombinator.com/item?id=45564619); [Coywolf](https://coywolf.com/news/productivity/fastmail-launches-native-desktop-apps-for-mac-windows-and-linux/)). Users praise speed, search and JMAP (verified: HN thread). They complain about Electron memory use and no IMAP accounts (verified: HN thread), and reportedly about mobile hangs (unverified). A native, keyboard-first TUI that is instant on a cold start has room to compete (inferred).

## How to read this doc

- "(verified: source)" means a primary source confirms the claim. Primary sources are Fastmail's own pages, RFCs and drafts, source code that calls the live API, and HTTP responses we fetched ourselves.
- "(inferred)" means we reasoned it from verified facts.
- "(unverified)" means we could not confirm it. Treat it as a hypothesis to test.
- Session values come from published dumps, not from a live session. Live values can differ. The first open question gives the one `curl` command that settles most of them.

## Product features relevant to a mail client

### Feature map

| Feature | What it does at Fastmail | How a JMAP client sees it | Label |
|---|---|---|---|
| Folders or labels | One account-wide choice in Settings, Mail preferences. Folders are converted to labels on switch. In label mode, Archive becomes "All Mail" | Both are JMAP `Mailbox` objects. An email can be in many mailboxes (`mailboxIds`). Label mode is a UI convention, not a server difference | verified: [labels help](https://www.fastmail.help/hc/en-us/articles/360058753554-Setting-up-and-using-labels); mapping inferred |
| Conversations | Replies group with the original. A new subject starts a new thread. Threads split after 100 messages | JMAP `Thread` objects and `threadId` on each email | verified: [conversations help](https://www.fastmail.help/hc/en-us/articles/1500001969861-Conversations); mapping inferred |
| Identities and aliases | Aliases both send and receive. Each alias has its own sending name and signature. Catch-all aliases let you send from any address at your domain | `Identity` objects (RFC 8621 section 6). A `*@domain` identity means any local part is allowed | verified: [identities help](https://www.fastmail.help/hc/en-us/articles/1500000280401-Identities), [RFC 8621](https://www.rfc-editor.org/rfc/rfc8621.html); Fastmail's use of `*@` identities unverified |
| Custom domains | Not on the Basic plan | No JMAP management API. Domains show up only as identities and addresses | verified: [pricing](https://www.fastmail.com/pricing/); API absence inferred |
| Plus and subdomain addressing | `user+tag@` and `tag@user.domain` delivery | Plain addresses in headers | verified: [plus addressing help](https://www.fastmail.help/hc/en-us/articles/360060591053-Plus-addressing-and-subdomain-addressing) |
| Masked Email | Generated per-site addresses with states | Public extension `https://www.fastmail.com/dev/maskedemail`. Incoming mail carries `X-FM-Masked-Email` | verified: [dev docs](https://www.fastmail.com/dev/), [Masked Email help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email) |
| Pinned | Fastmail's name for flagged. Other clients may show "Starred". Search alias `is:pinned` = `flagged` | `$flagged` keyword | verified: [pin help](https://www.fastmail.help/hc/en-us/articles/1500000280341-Pin-important-messages), [search help](https://www.fastmail.help/hc/en-us/articles/360060591213-Searching-your-mail); keyword inferred from the `flagged` alias |
| Snooze | Moves mail to a Snoozed folder and back at a set time. Not on Basic, or on legacy plans with less than 10 GB of mail storage. "We are unable to provide the ability to snooze messages from a third party mail client" | No public JMAP property. An expired draft by Fastmail authors defined `urn:ietf:params:jmap:mail:snooze`, a `snoozed` mailbox role and an Email `snoozed` property (`until`, `moveToMailboxId`) | verified: [snooze help](https://www.fastmail.help/hc/en-us/articles/360058753634-Snoozing-mail), [draft-ietf-extra-email-snooze-00](https://datatracker.ietf.org/doc/draft-ietf-extra-email-snooze/); Fastmail token support unverified |
| Scheduled send | Pick a send time. The message waits in a Scheduled folder. Cancel returns it to Drafts. Not on Basic | `EmailSubmission` with a delay. The 2024 session dump shows `maxDelayedSend: 44236800` (512 days) | verified: [scheduled send help](https://www.fastmail.help/hc/en-us/articles/4686576659727-Scheduled-Send); see "Sending" |
| Undo send | 15 seconds to undo. Delivery after 20 seconds. Web and Fastmail apps only: "It can't be used with third party email clients" | Build it with a delayed submission and `undoStatus: canceled`, or a client-side hold | verified: [undo send help](https://www.fastmail.help/hc/en-us/articles/1500000278222-Undo-send); approach inferred |
| Mute | "Mute replies": later replies go straight to Archive with no notification. Not on Basic. `is:muted` search | `$muted` keyword (RFC 9979, co-written by Fastmail) | verified: [mute help](https://www.fastmail.help/hc/en-us/articles/7788624824463-Mute-conversations), [RFC 9979](https://www.rfc-editor.org/rfc/rfc9979.html); Fastmail's keyword name inferred |
| Follow | "Notify me of replies" on a conversation | `$followed` keyword (RFC 9979) | verified: [conversations help](https://www.fastmail.help/hc/en-us/articles/1500001969861-Conversations); keyword inferred |
| Memos | Private notes on a message, searchable with `memo:` and `has:memo` | `$memo` on the note message and `$hasmemo` on the annotated one, plus a `Memos` mailbox attribute (RFC 9979) | verified: [search help](https://www.fastmail.help/hc/en-us/articles/360060591213-Searching-your-mail), [RFC 9979](https://www.rfc-editor.org/rfc/rfc9979.html); Fastmail storage inferred |
| Saved searches | Saved from the search bar. Shown as labels in the web UI only, not in IMAP | No standard JMAP object. Probably stored in private user preferences | verified: [labels help](https://www.fastmail.help/hc/en-us/articles/360058753554-Setting-up-and-using-labels); storage unverified |
| Grouped mail view | New on 2026-10-08. All plans. Sections such as Pinned, Unread, date buckets, or custom groups written as search queries that cannot search bodies. Per folder, label or saved search. Syncs across devices. `Shift+J` and `Shift+K` jump between groups | No public API. mejl can rebuild it locally from cached data | verified: [grouped view help](https://www.fastmail.help/hc/en-us/articles/17915089518863-Grouped-mail-view), [blog](https://www.fastmail.com/blog/grouped-mailbox/); API absence inferred |
| VIP contacts | Starred contacts, a VIPs group, `fromin:vips`. Notifications can be limited to VIPs | Contacts data (JMAP Contacts). How the VIP flag is stored is unknown | verified: [VIP help](https://www.fastmail.help/hc/en-us/articles/17254978619791-VIP-contacts); storage unverified |
| Screening | Not a product feature. It is a rule: "Sender is not a contact" then move to a folder. Approve by adding the sender to contacts | Depends on rules, so no public API (see "Rules") | verified: [screening help](https://www.fastmail.help/hc/en-us/articles/8661280554511-Screening-emails) |
| Spam | SpamAssassin score, default threshold 5. Personal Bayes trained by "spam" and "not spam" reports. Contacts are trusted. `X-Spam-*` headers on each message | Spam is a mailbox with role `junk`. RFC 8621 lists `$junk` and `$notjunk` keywords. Whether a JMAP move into or out of Spam trains the filter is unknown | verified: [spam help](https://www.fastmail.help/hc/en-us/articles/360060591413-Spam-filtering); training via JMAP unverified |
| Unsubscribe | One-click unsubscribe in the web UI | RFC 9979 defines `$canunsubscribe` and `$unsubscribed` keywords | verified: [RFC 9979](https://www.rfc-editor.org/rfc/rfc9979.html); Fastmail use unverified |
| Vacation response | Auto-reply settings | `urn:ietf:params:jmap:vacationresponse` (RFC 8621 section 8) | verified: [dev docs scopes](https://www.fastmail.com/dev/#scopes) |
| Contacts | Address books, groups, shared contacts | JMAP Contacts (RFC 9610) or CardDAV | verified: [dev docs](https://www.fastmail.com/dev/) |
| Calendars | Full calendar app. World clocks and attendee local times added 2026-10-15 | CalDAV only for third parties | verified: [dev docs](https://www.fastmail.com/dev/), [releasebot summary](https://releasebot.io/updates/fastmail) |
| Notes | Stored as emails in a Notes folder. Apple Notes compatible. Renaming the folder from another client breaks sync | Visible as a mailbox over JMAP and IMAP. No public notes API | verified: [notes help](https://www.fastmail.help/hc/en-us/articles/1500000280181-Notes); JMAP view inferred |
| Files | Storage (10 GB on Individual) and simple websites | WebDAV only for third parties. The JMAP FileNode draft is at -18 | verified: [dev docs](https://www.fastmail.com/dev/), [server names](https://www.fastmail.help/hc/en-us/articles/1500000278342-Server-names-and-ports), [IETF JMAP WG](https://datatracker.ietf.org/wg/jmap/documents/) |
| Offline | Web and mobile can cache recent or all mail and queue changes (August 2025). The desktop app has the same | Shows Fastmail expects clients to work offline. mejl should too | verified: [offline help](https://www.fastmail.help/hc/en-us/articles/11517883953039-Offline-support) |
| Data region | EU (Amsterdam) or US primary storage, since 2026-08-03. Generic hostnames keep working. Optional `eu-` and `us-` prefixed hostnames exist for IMAP, SMTP, DAV | `api.fastmail.com` is not mentioned. Use the generic host | verified: [data residency help](https://www.fastmail.help/hc/en-us/articles/16796454162063-Choosing-your-data-residency), [blog](https://www.fastmail.com/blog/fastmail-offers-eu-data-region/) |

### Plans matter

- The Basic plan has no custom domains of its own, no Scheduled Send, no Snooze, no Mute and no "Use with third-party apps" (verified: [pricing](https://www.fastmail.com/pricing/)). Basic users can still use a custom domain shared by a Standard or Professional user on the same account (verified: pricing FAQ). Legacy plans with less than 10 GB of mail storage also lack Snooze and Scheduled Send (verified: [snooze help](https://www.fastmail.help/hc/en-us/articles/360058753634-Snoozing-mail), [scheduled send help](https://www.fastmail.help/hc/en-us/articles/4686576659727-Scheduled-Send)).
- Masked Email is listed for Basic (verified: pricing), but API tokens are not (see below). So a Basic user has Masked Email but, as far as we know, no way to manage it from mejl (inferred).
- "API tokens are not available to users on Basic subscription plans" (verified: [API tokens help](https://www.fastmail.help/hc/en-us/articles/5254602856719-API-tokens)). Basic plans also cannot use IMAP, SMTP, CalDAV or CardDAV (verified: [app passwords help](https://www.fastmail.help/hc/en-us/articles/360058752854-App-passwords)).
- Whether OAuth for a third-party JMAP client works on Basic is unknown (unverified). mejl should detect it and show a clear message.

### What is new in 2025 and 2026

| Date | Change | Source |
|---|---|---|
| 2025-02-12 | New Cyrus database format "twom" in use on all servers (post 2025-06-13) | [releasebot summary](https://releasebot.io/updates/fastmail) |
| 2025-08-05 | Refreshed themes, faster app and user switching, refined search filters, undo and redo in menus | [blog](https://www.fastmail.com/blog/better-themes-better-search/) |
| 2025-08-26 | Full offline support on web and mobile | [releasebot summary](https://releasebot.io/updates/fastmail), [offline help](https://www.fastmail.help/hc/en-us/articles/11517883953039-Offline-support) |
| 2025-10-13 | Desktop app for macOS, Windows and Linux | [blog](https://www.fastmail.com/blog/desktop-app/) |
| 2026-03-05 | Multi-window compose and reading, inline replies, bottom reading pane | [blog](https://www.fastmail.com/blog/multi-window-support/) |
| 2026-04-22 | Official MCP server | [blog](https://www.fastmail.com/blog/an-mcp-server-for-fastmail/) |
| 2026-05 | RFC 9979 published: keywords and mailbox attributes, by N. Jenkins (Fastmail) and D. Eggert (Apple) | [RFC 9979](https://www.rfc-editor.org/rfc/rfc9979.html) |
| 2026-08-03 | EU data region | [blog](https://www.fastmail.com/blog/fastmail-offers-eu-data-region/) |
| 2026-08-19 | Incident write-up: a Cyrus replication bug wiped a subset of folders for 8 accounts. Folders were restored from backup, "recovering all emails for all but one user" | [blog](https://www.fastmail.com/blog/data-resilience-at-fastmail/) |
| 2026-10-08 | Grouped mail view | [blog](https://www.fastmail.com/blog/grouped-mailbox/) |
| 2026-10-15 | Calendar world clocks, extra time zone columns | [releasebot summary](https://releasebot.io/updates/fastmail) |

The releasebot dates are an aggregator's reading of Fastmail's blog (unverified against each post, except where a Fastmail link is given).

## JMAP at Fastmail

### Endpoints

| Purpose | URL | Label |
|---|---|---|
| Session | `https://api.fastmail.com/jmap/session` | verified: [dev docs](https://www.fastmail.com/dev/) |
| Autodiscovery | `https://api.fastmail.com/.well-known/jmap` redirects (302) to the session URL | verified: `curl`, 2026-10-23 |
| API | `https://api.fastmail.com/jmap/api/` | verified: 2024 session dump in [demo-fastmail-api-jmap](https://github.com/joelparkerhenderson/demo-fastmail-api-jmap) |
| Upload | `https://api.fastmail.com/jmap/upload/{accountId}/` | verified: same dump |
| Download | `https://www.fastmailusercontent.com/jmap/download/{accountId}/{blobId}/{name}?type={type}` (separate origin) | verified: same dump |
| EventSource | `https://api.fastmail.com/jmap/event/` (no URL template variables) | verified: same dump |

Notes for implementers:

- The 2021 session in the [1Password post](https://www.fastmail.com/blog/how-and-why-we-built-masked-email-with-jmap-an-open-api-standard/) used the host `jmap.fastmail.com`. The 2024 dump uses `api.fastmail.com`. Always read URLs from the session, never hard-code them (inferred).
- RFC 8620 says the `eventSourceUrl` "MUST contain variables called types, closeafter, and ping" (verified: [RFC 8620 section 2](https://www.rfc-editor.org/rfc/rfc8620.html)). Fastmail's URL has none. A client must handle a URL with no template, and append query parameters itself if it wants them (inferred).
- The download URL is on another domain. Send the bearer token there too, and do not treat it as a CORS-free resource in any future web view (inferred).
- An unauthenticated request to the session returns `401` with `WWW-Authenticate: Bearer resource_metadata="https://api.fastmail.com/.well-known/oauth-protected-resource/jmap/session"` (verified: `curl`, 2026-10-23). This is the [RFC 9728](https://www.rfc-editor.org/rfc/rfc9728.html) discovery hook that the OAuth profile uses.

### Capabilities

Fastmail sends a session object whose capabilities depend on the token's scopes (inferred from the scope model). The table lists every capability we found evidence for.

| Capability URI | Spec and status | Available to third parties | Evidence |
|---|---|---|---|
| `urn:ietf:params:jmap:core` | [RFC 8620](https://www.rfc-editor.org/rfc/rfc8620.html), Proposed Standard | Yes | verified: dev docs, session dumps |
| `urn:ietf:params:jmap:mail` | [RFC 8621](https://www.rfc-editor.org/rfc/rfc8621.html) | Yes ("Email" token scope) | verified: dev docs, dumps |
| `urn:ietf:params:jmap:submission` | RFC 8621 | Yes ("Email submission" scope, needs Email) | verified: dev docs, token help |
| `urn:ietf:params:jmap:vacationresponse` | RFC 8621 | Yes as an OAuth scope. The API token UI does not list it separately | verified: dev docs; token behaviour unverified |
| `urn:ietf:params:jmap:contacts` | [RFC 9610](https://www.rfc-editor.org/rfc/rfc9610.html), Dec 2024. Data format is JSContact ([RFC 9553](https://www.rfc-editor.org/rfc/rfc9553.html), May 2024, verified) | Yes ("Contacts and contact groups" scope) | verified: dev docs, token help |
| `https://www.fastmail.com/dev/maskedemail` | Fastmail extension, documented | Yes ("Masked Email" scope) | verified: dev docs |
| `urn:ietf:params:jmap:calendars` | [draft-ietf-jmap-calendars-33](https://datatracker.ietf.org/doc/draft-ietf-jmap-calendars/), in RFC Editor queue | No, not yet. Used by the web app | verified: dev docs say CalDAV only; web use inferred from rules-cli list |
| `urn:ietf:params:jmap:principals` | [RFC 9670](https://www.rfc-editor.org/rfc/rfc9670.html) (Sharing) | Unknown | inferred: in web app list |
| `urn:ietf:params:jmap:quota` | [RFC 9425](https://www.rfc-editor.org/rfc/rfc9425.html) | Unknown | inferred: in web app list |
| `urn:ietf:params:jmap:blob` | [RFC 9404](https://www.rfc-editor.org/rfc/rfc9404.html) | Unknown | inferred: in web app list |
| `urn:ietf:params:jmap:websocket` | [RFC 8887](https://www.rfc-editor.org/rfc/rfc8887.html) | No evidence | inferred: absent from all dumps and the web app list; a 2022 developer reported WebSocket push unavailable ([issue 7](https://github.com/fastmail/JMAP-Samples/issues/7)) |
| `urn:ietf:params:jmap:sieve` | [RFC 9661](https://www.rfc-editor.org/rfc/rfc9661.html), Sept 2024 | No evidence | inferred: absent everywhere; Sieve FAQ says web UI only. Cyrus has JMAP Sieve tests ([`JMAPSieve.pm`](https://github.com/cyrusimap/cyrus-imapd/blob/master/cassandane/Cassandane/TestSuite/JMAPSieve.pm), verified), so the server code exists; Fastmail production use is unknown |
| `urn:ietf:params:jmap:mdn` | [RFC 9007](https://www.rfc-editor.org/rfc/rfc9007.html) | No evidence | unverified |
| `urn:ietf:params:jmap:mail:snooze` | Expired [draft](https://datatracker.ietf.org/doc/draft-ietf-extra-email-snooze/) by Fastmail authors | No evidence | unverified |
| Private `https://www.fastmail.com/dev/*` URIs (see below) | Undocumented | Not for third parties | verified: present in third-party code copying web app requests |

The Fastmail web app sends this `using` list, as copied into [fastmail-rules-cli `jmap/types.go`](https://github.com/dvcrn/fastmail-rules-cli/blob/main/jmap/types.go) (inferred: third-party code, likely copied from a browser request, date 2026-04):

```text
https://www.fastmail.com/dev/user        https://www.fastmail.com/dev/rules
https://www.fastmail.com/dev/customer    https://www.fastmail.com/dev/calendars
urn:ietf:params:jmap:quota               urn:ietf:params:jmap:principals
urn:ietf:params:jmap:calendars           urn:ietf:params:jmap:submission
urn:ietf:params:jmap:core                https://www.fastmail.com/dev/compress
https://www.fastmail.com/dev/performance urn:ietf:params:jmap:contacts
urn:ietf:params:jmap:blob                https://www.fastmail.com/dev/backup
https://www.fastmail.com/dev/notes       https://www.fastmail.com/dev/auth
https://www.fastmail.com/dev/mail        https://www.fastmail.com/dev/filenode
https://www.fastmail.com/dev/blobext     https://www.fastmail.com/dev/files
urn:ietf:params:jmap:vacationresponse    https://www.fastmail.com/dev/contacts
urn:ietf:params:jmap:mail
```

What this tells us (inferred):

- The web app runs on JMAP for everything, including calendars, notes, files, rules and backups. Fastmail's API Terms say "parts of our API are undocumented" and "you should not rely on their behaviours" (verified: [API Terms](https://www.fastmail.com/policies/api-terms-of-service/)).
- `maskedemail` is missing from the list, so the list is incomplete or specific to the screens that were captured.
- `websocket` is missing, which fits the view that Fastmail does not offer RFC 8887.
- Features like snooze and saved searches probably live under `dev/mail` and `dev/user`. That is private surface, so mejl must not use it.

### Other JMAP RFCs and drafts worth tracking

| Document | Status (2026-10-23) | Why it matters to mejl |
|---|---|---|
| [RFC 9219](https://www.rfc-editor.org/rfc/rfc9219.html) S/MIME signature verification | RFC | Low priority |
| [RFC 9749](https://www.rfc-editor.org/rfc/rfc9749.html) VAPID for JMAP Web Push | RFC, March 2025 | Not useful for a TUI |
| [RFC 9979](https://www.rfc-editor.org/rfc/rfc9979.html) keywords and mailbox attributes | RFC, May 2026, Informational | Names for mute, follow, memo, masked email, snoozed, scheduled |
| draft-ietf-jmap-calendars-33 | RFC Editor queue | Trigger for Fastmail opening JMAP calendars |
| draft-ietf-jmap-filenode-18 | Active | Possible future Files access |
| draft-ietf-jmap-blobext-02, -conditional-01, -metadata-04, -object-history-02, -refplus-02, -mail-sharing-02, -emailpush-03 | Active WG drafts | Watch list. Not needed for v1 |
| draft-ietf-mailmaint-oauth-public-06 | Active, Informational | The OAuth profile Fastmail appears to implement |

Source for statuses: [IETF JMAP WG documents](https://datatracker.ietf.org/wg/jmap/documents/) and [mailmaint draft page](https://datatracker.ietf.org/doc/draft-ietf-mailmaint-oauth-public/) (verified).

## Authentication

### API tokens (for personal use and development)

How to create one (verified: [API tokens help](https://www.fastmail.help/hc/en-us/articles/5254602856719-API-tokens), [dev docs](https://www.fastmail.com/dev/)):

1. Settings, Privacy & Security, "Connected apps & API tokens", "Manage API tokens" ([direct link](https://app.fastmail.com/settings/security/tokens)).
2. "New API token". A "Verify it's you" password prompt may appear.
3. Name it.
4. Pick the **Type**: JMAP or MCP.
5. Pick the **Scopes**.
6. Copy the token. It is shown only once.

Scopes offered in the UI. This list is the strongest evidence of what Fastmail lets third parties touch.

| Token type | UI scope | Likely JMAP capability | Label |
|---|---|---|---|
| JMAP | Read-only access | Same capabilities, with writes refused. Session accounts carry an `isReadOnly` flag (false in the 2024 dump) | verified: UI text; mechanism inferred |
| JMAP | Email ("download, modify, and permanently delete your mail") | `urn:ietf:params:jmap:mail` | verified: UI text; mapping inferred |
| JMAP | Email submission (requires Email) | `urn:ietf:params:jmap:submission` | verified: UI text; mapping inferred |
| JMAP | Contacts and contact groups | `urn:ietf:params:jmap:contacts` | verified: UI text; mapping inferred |
| JMAP | Masked Email | `https://www.fastmail.com/dev/maskedemail` | verified: UI text; mapping inferred |
| MCP | Read data | MCP tools only | verified: UI text |
| MCP | Make changes | MCP tools only | verified: UI text |
| MCP | Send email | MCP tools only | verified: UI text |

What the list does not contain: calendars, rules or Sieve, files, notes, settings, vacation response (as its own checkbox), quotas. Calendars are offered to MCP tokens but not to JMAP tokens. That fits the MCP server using Fastmail's private calendar JMAP internally (inferred).

Usage: `Authorization: Bearer <token>` on every request. The JMAP API is CORS-enabled (verified: [dev docs](https://www.fastmail.com/dev/)). Fastmail shows each token's first use and last use, and the user can revoke it (verified: API tokens help).

For mejl, an API token is the simplest v1 login: the user pastes it once, mejl stores it in the OS keyring (inferred recommendation). Note the conflict: the dev docs say API tokens are "for testing purposes, or building an app for just yourself", and that an app "to distribute to others" should implement OAuth (verified: [dev docs](https://www.fastmail.com/dev/)). Token paste is fine for development and for each user running their own copy, but a public release should offer OAuth (inferred).

### App passwords (for IMAP, SMTP and DAV)

App passwords can be limited to Mail (IMAP, POP, SMTP), Contacts (CardDAV), Calendars (CalDAV) or Files (WebDAV). They are not available on Basic (verified: [app passwords help](https://www.fastmail.help/hc/en-us/articles/360058752854-App-passwords)). mejl needs one only for the CalDAV or WebDAV fallbacks.

### OAuth 2.0: the documented flow

From the [dev docs](https://www.fastmail.com/dev/#o-auth-at-fastmail) (verified):

- Authorization Code grant ([RFC 6749](https://www.rfc-editor.org/rfc/rfc6749.html)) with **mandatory PKCE**, `S256` only. `response_type` must be `code`.
- Authorize at `https://api.fastmail.com/oauth/authorize`. Exchange and refresh at `https://api.fastmail.com/oauth/refresh` (form-encoded, no client secret). Revoke at `https://api.fastmail.com/oauth/revoke`.
- Documented scopes: `urn:ietf:params:jmap:core` (always required), `...:mail`, `...:submission`, `...:vacationresponse`, `...:contacts`, and `https://www.fastmail.com/dev/maskedemail`.
- Redirect URIs may be a claimed `https` URI, a private-use scheme with a dot (`com.example:/`), or the loopback `http://localhost/`, where "an arbitrary port may be given by the client" and `127.0.0.1` or `::1` may replace `localhost`. A loopback redirect suits a TUI that opens the system browser.
- The authorization code lives 10 minutes and is single use. Reuse revokes all tokens from it.
- Access tokens expire (`expires_in`). On `401`, refresh.
- **Refresh tokens rotate.** "The client MUST NOT try to use an old refresh token again; this will result in the authorization being revoked." Each device must get its own authorization. `invalid_grant` may carry `temporary: true` (for example, an admin lock).
- Registration: "Clients are currently registered manually by contact with Fastmail developers" via the [partners page](https://www.fastmail.com/company/partners/), which gives the address `partnerships@fastmailteam.com` (verified). Fastmail needs name, logo, URLs for terms, privacy and support, scopes, redirect URIs and CORS origins.
- The dev docs also say "You can use OAuth with all of our supported protocols", which covers IMAP, SMTP, CalDAV, CardDAV and WebDAV, although the documented scopes are all JMAP ones (verified: [dev docs](https://www.fastmail.com/dev/)). How a non-JMAP OAuth grant is scoped is not documented (unverified).
- No device authorization flow ([RFC 8628](https://www.rfc-editor.org/rfc/rfc8628.html)) is documented, and the discovery metadata lists only `authorization_code` and `refresh_token` grants (verified: `curl`, 2026-10-23). A headless SSH session therefore needs a token paste, or a loopback flow through port forwarding (inferred).

### OAuth 2.0: what the server now advertises

Unauthenticated discovery on 2026-10-23 (verified: `curl`):

```jsonc
// GET https://api.fastmail.com/.well-known/oauth-authorization-server (abridged)
{
  "issuer": "https://api.fastmail.com",
  "registration_endpoint": "https://api.fastmail.com/oauth/register",
  "authorization_endpoint": "https://api.fastmail.com/oauth/authorize",
  "token_endpoint": "https://api.fastmail.com/oauth/refresh",
  "scopes_supported": [
    "urn:ietf:params:oauth:scope:mail",
    "urn:ietf:params:oauth:scope:contacts",
    "urn:ietf:params:oauth:scope:calendars",
    "https://www.fastmail.com/dev/mcp",
    "openid", "profile", "email", "offline_access"
  ],
  "grant_types_supported": ["authorization_code", "refresh_token"],
  "token_endpoint_auth_methods_supported": ["none"],
  "code_challenge_methods_supported": ["S256"],
  "authorization_response_iss_parameter_supported": true,
  "revocation_endpoint": "https://api.fastmail.com/oauth/revoke"
}
```

- OpenID Connect discovery is also served, with `userinfo` and `jwks` endpoints (verified: `/.well-known/openid-configuration`).
- Protected resource metadata exists for `/jmap/session`, `/jmap/api` and `/jmap/event`. Each lists the three `urn:ietf:params:oauth:scope:*` scopes plus `offline_access`. The MCP resource lists only `https://www.fastmail.com/dev/mcp` (verified: `/.well-known/oauth-protected-resource/...`).
- These scopes and endpoints match [draft-ietf-mailmaint-oauth-public](https://datatracker.ietf.org/doc/draft-ietf-mailmaint-oauth-public/) (-06, 16 September 2026, edited by Neil Jenkins of Fastmail with Ben Bucksch of Beonex). The draft says a server "MUST accept registration requests that carry no initial access token". It allows only loopback (`http://127.0.0.1/` or `http://[::1]/`, any port) and private-use scheme redirects. Unlike Fastmail's dev docs, it does not list `localhost`, and claimed `https` redirects "are deliberately not permitted". `token_endpoint_auth_method` "MUST be none". It forbids embedded web views, requires the `iss` check from [RFC 9207](https://www.rfc-editor.org/rfc/rfc9207.html), and requires the server to keep the old refresh token valid until the new one (or its access token) is first used, so a lost response can be retried (verified: [draft -06 text](https://www.ietf.org/archive/id/draft-ietf-mailmaint-oauth-public-06.txt) sections 3.3, 3.4 and 3.10).

What this means (inferred):

- Fastmail very likely supports open dynamic registration now. The MCP connector flow in Claude and ChatGPT needs some form of automatic client registration, and it works against this server. The dev docs have not caught up. This is a docs-versus-behaviour mismatch. Report it to Fastmail and test before relying on it.
- The new `urn:ietf:params:oauth:scope:mail` scope is coarse. The draft says it "MUST include full access" via IMAP, POP, SMTP submission, ManageSieve and every JMAP capability registered for it. Its table maps the `mail` scope to `core`, `mail`, `submission`, `vacationresponse`, `mdn`, `smimeverify`, `sieve`, `blob`, `quota`, `principals`, `websocket` and `webpush-vapid` (verified: draft section 5.4.1). So per the draft, `mail` covers submission. Whether Fastmail's server grants all of that is untested (unverified). Vendor capabilities such as `https://www.fastmail.com/dev/maskedemail` have no interoperable scope, and that scope is not in `scopes_supported` (verified: metadata). Masked Email over open-registered OAuth is therefore an open question.
- Rust support exists: [neverlight-mail-oauth](https://github.com/jstelzer/neverlight-mail-oauth) implements this profile (discovery, RFC 7591 registration, PKCE, loopback server) and starts its example from Fastmail's session URL. It is a Git dependency, not on crates.io (verified: crates.io API returned not found, 2026-10-23). Its Fastmail testing is not stated (unverified).

### Recommended login strategy for mejl (inferred)

1. v1 (development and early users): API token paste, with a checklist of the scopes to tick. Works today on Standard and Professional plans. The dev docs frame tokens as for testing or personal use, so do not make this the only login of a public release.
2. v1.x: OAuth via the open profile: discovery from the session `401`, registration, loopback redirect, PKCE, `iss` check, rotate and persist the refresh token before using the new access token, serialize refreshes.
3. Fallback: if open registration fails, apply for a registered client through the partners page.
4. Never use web session cookies. That is the path to the private rules API and is against policy.

## Push and incremental sync

### EventSource (the usable push channel)

- Endpoint: the session's `eventSourceUrl`, today `https://api.fastmail.com/jmap/event/`. Authenticate with the same `Authorization: Bearer` header (verified: 2024 dump; [issue 7](https://github.com/fastmail/JMAP-Samples/issues/7)).
- History: in 2021 Fastmail said its EventSource "doesn't allow non-Fastmail clients to connect, yet". Personal API tokens started working on beta in August 2022 and on production a few weeks before 2022-10-03 (verified: Fastmail staff comments in issue 7).
- Known quirks reported after the issue closed (unverified today): on 2023-01-30 one developer posted a single event that had `"type": "connect"` instead of the spec's `"@type": "StateChange"`, with an `EmailDelivery` entry in `changed`. Fastmail staff replied on 2023-02-01: "You have found a bug." The same developer then said "the `ping` and `closeAfterState` URL params don't seem to change the behavior" (verified: [issue 7](https://github.com/fastmail/JMAP-Samples/issues/7) comments). The RFC parameter is `closeafter`, so it is not clear what was tested (inferred). mejl's parser should accept both shapes, read the `changed` map, and not depend on server pings for liveness.
- RFC behaviour to code against (verified: [RFC 8620 section 7.3](https://www.rfc-editor.org/rfc/rfc8620.html)): `types=*` or a list; `closeafter=state|no`; `ping=<seconds>` makes the server send a `ping` event with `{"interval": n}`. A `StateChange` has `changed: { accountId: { TypeName: state } }`.
- Practical client design (inferred): one long-lived SSE connection per account; on any event, run one batched sync request; reconnect with backoff and send `Last-Event-ID` if the server sets ids; set a client-side idle timeout (for example 5 minutes) and reconnect when no data arrives; after a reconnect, always run a sync because events may have been missed.

### JMAP over WebSocket

No evidence that Fastmail offers `urn:ietf:params:jmap:websocket` to third parties (inferred, see the capability table). Fastmail does maintain a [JMAP-Tester-WebSocket](https://github.com/fastmail) Perl test tool, so Cyrus supports it in some form (inferred). Check the live session before ruling it out. If present, it would let mejl send requests and receive pushes on one connection.

### PushSubscription (Web Push)

- Requires an `https` URL that the server can reach and verify with a `PushVerification` message (verified: [RFC 8620 section 7.2](https://www.rfc-editor.org/rfc/rfc8620.html)).
- Fastmail had not implemented it in 2021 (verified: issue 7). A Cloudflare Worker created on 2026-09-18, [fastmail-jmap-push-bridge](https://github.com/vezaynk/fastmail-jmap-push-bridge), calls `PushSubscription/set` with a bearer API token (verified: its `src/jmap.ts`). That suggests it works now, but we did not see it run (unverified).
- Not useful for a local TUI. It could matter for a future mejl notification relay.

### State strings and the sync loop

Every `/get` response has a `state`. Every `/query` response has a `queryState`. The session has its own `state`; a change means re-fetch the session. `/changes` returns `created`, `updated`, `destroyed` since a state, with `hasMoreChanges`. `/queryChanges` returns `removed` and `added` (with indexes) for a cached query. If the server cannot compute changes it returns `cannotCalculateChanges`, and the client must drop that cache and refetch (verified: [RFC 8620 sections 5.2 and 5.6](https://www.rfc-editor.org/rfc/rfc8620.html)).

`Email/queryChanges` takes the same `collapseThreads` value as the original query. `Thread/changes` is a standard `/changes`. Thread `emailIds` are sorted by `receivedAt`, oldest first (verified: [RFC 8621](https://www.rfc-editor.org/rfc/rfc8621.html)).

A fast first paint and cheap resync (inferred design):

1. **Cold start.** One request: `Mailbox/get` (all), `Identity/get`, then `Email/query` on the inbox (`collapseThreads: true`, sort `receivedAt` descending, `limit` about 100), then `Email/get` by back-reference (`#ids`) with only list properties (`threadId`, `mailboxIds`, `keywords`, `from`, `subject`, `receivedAt`, `preview`, `size`, `hasAttachment`), then `Thread/get` by back-reference. Store every `state` and `queryState`.
2. **Background fill.** Page older results with `position` or `anchor` while idle. `maxObjectsInGet` is 4096 (2024 dump), so fetch in large chunks.
3. **On a push event or a timer.** One request with `Mailbox/changes`, `Email/changes` (with `maxChanges`), `Thread/changes`, `Email/get` for `#created` and `#updated` ids (back-references to `/created` and `/updated`), and `Email/queryChanges` for each visible list. Loop while `hasMoreChanges`.
4. **Bodies on demand.** Fetch `bodyValues` with `fetchTextBodyValues` and a `maxBodyValueBytes` cap when the user opens a message. Prefetch the next few messages in the list.
5. **Handle `cannotCalculateChanges`** by refetching that type or query only.

Example sync request (illustrative; ids are placeholders):

```json
{
  "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
  "methodCalls": [
    ["Mailbox/changes", {"accountId": "u1", "sinceState": "M1"}, "m"],
    ["Email/changes", {"accountId": "u1", "sinceState": "E1", "maxChanges": 500}, "e"],
    ["Email/get", {"accountId": "u1",
      "#ids": {"resultOf": "e", "name": "Email/changes", "path": "/created"},
      "properties": ["threadId", "mailboxIds", "keywords", "from", "subject", "receivedAt", "preview"]}, "ec"],
    ["Email/get", {"accountId": "u1",
      "#ids": {"resultOf": "e", "name": "Email/changes", "path": "/updated"},
      "properties": ["mailboxIds", "keywords"]}, "eu"],
    ["Thread/changes", {"accountId": "u1", "sinceState": "T1"}, "t"],
    ["Email/queryChanges", {"accountId": "u1",
      "filter": {"inMailbox": "inbox-id"},
      "sort": [{"property": "receivedAt", "isAscending": false}],
      "collapseThreads": true, "sinceQueryState": "Q1"}, "q"]
  ]
}
```

Six calls is well under `maxCallsInRequest` (50 in the 2024 dump).

## Limits, rate limits and terms of use

### Published session limits

| Limit | Value | Source |
|---|---|---|
| `maxSizeUpload` | 250,000,000 bytes | verified: 2021 and 2024 dumps; live value may differ |
| `maxConcurrentUpload` | 10 | same |
| `maxSizeRequest` | 10,000,000 bytes | same |
| `maxConcurrentRequests` | 10 | same |
| `maxCallsInRequest` | 50 | same |
| `maxObjectsInGet` | 4096 | same |
| `maxObjectsInSet` | 4096 | same |
| `collationAlgorithms` | `i;ascii-numeric`, `i;ascii-casemap`, `i;octet` | 2024 dump |
| Mail `maxSizeMailboxName` | 490 | 2024 dump |
| Mail `maxMailboxDepth` | null (no limit) | 2024 dump |
| Mail `maxMailboxesPerEmail` | 1000 | 2024 dump |
| Mail `maxSizeAttachmentsPerEmail` | 50,000,000 bytes | 2024 dump |
| Mail `emailQuerySortOptions` | `receivedAt`, `from`, `to`, `subject`, `size`, `header.x-spam-score` | 2024 dump |
| Submission `maxDelayedSend` | 44,236,800 seconds (512 days) | 2024 dump |
| Submission `submissionExtensions` | `{}` (empty) | 2024 dump |

The 2024 dump is the "Example result" in the README of [joelparkerhenderson/demo-fastmail-api-jmap](https://github.com/joelparkerhenderson/demo-fastmail-api-jmap) (repo created 2024-11-12, last commit 2025-07-22), with account ids redacted to `u00000000` (verified). Its exact capture date is not stated. The 2021 dump is from the [1Password post](https://www.fastmail.com/blog/how-and-why-we-built-masked-email-with-jmap-an-open-api-standard/) (2021-11-11), which shows only the core limits and the old `jmap.fastmail.com` URLs (verified). Both agree on the core limits.

Note: `sentAt` and `hasKeyword`-style sorts are not in the sort list. `header.x-spam-score` is a Fastmail-specific sort (inferred: useful for a "likely spam" view).

### Rate limits

- No numeric API rate limit is published (verified: absent from [dev docs](https://www.fastmail.com/dev/), [API Terms](https://www.fastmail.com/policies/api-terms-of-service/), [Terms of Service](https://www.fastmail.com/policies/terms-of-service/); a third-party review also found none, [anchorterminal](https://www.anchorterminal.com/tools/fastmail)).
- `MaskedEmail/set` create returns a standard `rateLimit` SetError when limits are hit (verified: dev docs). The help page says to "Try again in 24 hours" (verified: [Masked Email help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email)).
- mejl should treat HTTP `429` and `503` with `Retry-After`, the JMAP `limit` request error, and `rateLimit` SetErrors as backoff signals, keep at most a few requests in flight (well below 10), and batch calls instead of firing many small requests (inferred).

### API terms and developer policy

Key rules for a distributed client (verified: [API Developer Policy](https://www.fastmail.com/policies/api-developer-policy/), [API Terms](https://www.fastmail.com/policies/api-terms-of-service/)):

- Prohibited: "Using unpublished APIs"; "bypassing or circumventing Fastmail protocols and access controls"; reverse engineering the API; exceeding rate limits or "excessive or abusive usage".
- "You may not use the Fastmail API to replicate or compete with core products or services offered by Fastmail, including the Services." This sentence sits under the heading "Circumventing Fastmail's intended limitations (including pricing, features and access structures)" (verified: Developer Policy). The API Terms add a ban on accessing the APIs "in order to replicate or compete with the Core Services" (verified: API Terms). The heading suggests the target is getting around plan limits. That also means rebuilding plan-gated features (Snooze, Scheduled Send, Mute) on the client for plans that exclude them could be read as circumvention (inferred). A third-party mail client is plausibly fine: Fastmail documents setup for many third-party clients and its dev page invites "an app to distribute to others". But the clause is broad. Ask Fastmail before a public release (inferred).
- Required: a support contact and installation instructions; a public privacy policy; minimal scopes; delete user data within 14 business days when the user removes the app; tell Fastmail when the app changes function or is discontinued; report data breaches to `dataprotection@fastmailteam.com`.
- No logo that "resembles the Fastmail icon". No "implying a Fastmail endorsement, certification, affiliation or partnership" without permission.
- Fastmail may make "backwards incompatible changes" and suggests following its changelogs.
- The customer Terms of Service add that Fastmail is "for human email communications", not "machine-to-machine workflows", and ban processes that place "an unreasonable load" on the service (verified: [ToS](https://www.fastmail.com/policies/terms-of-service/)).

For mejl (inferred): an open source client where each user brings their own token or OAuth grant, talks only to documented capabilities, keeps data local, and caches aggressively fits these rules. Shipping the private rules API does not.

## Sending

### Identities

- `Identity/get` returns the sending identities. Each has `name`, `email` (immutable), `replyTo`, `bcc`, `textSignature`, `htmlSignature` and `mayDelete`. A `*` local part means any address at that domain (verified: [RFC 8621 section 6](https://www.rfc-editor.org/rfc/rfc8621.html)).
- At Fastmail, aliases are the sending addresses, with per-alias name and signature. Default sender can be set per account and per folder (verified: [identities help](https://www.fastmail.help/hc/en-us/articles/1500000280401-Identities)). The per-folder default is not in JMAP; mejl would keep its own rule (inferred).
- Whether masked addresses appear in `Identity/get` is an open question, covered in [fastmail-masked-email.md](fastmail-masked-email.md).

### EmailSubmission

Create the draft and submit in one request, then move it to Sent on success (illustrative):

```json
{
  "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail", "urn:ietf:params:jmap:submission"],
  "methodCalls": [
    ["Email/set", {"accountId": "u1", "create": {"d1": {
      "mailboxIds": {"drafts-id": true},
      "keywords": {"$draft": true, "$seen": true},
      "from": [{"name": "Me", "email": "me@example.com"}],
      "to": [{"email": "you@example.org"}],
      "subject": "Hello",
      "bodyValues": {"t": {"value": "Hi there"}},
      "textBody": [{"partId": "t", "type": "text/plain"}]
    }}}, "0"],
    ["EmailSubmission/set", {"accountId": "u1",
      "create": {"s1": {"identityId": "identity-id", "emailId": "#d1"}},
      "onSuccessUpdateEmail": {"#s1": {
        "mailboxIds/drafts-id": null, "mailboxIds/sent-id": true, "keywords/$draft": null}}
    }, "1"]
  ]
}
```

`onSuccessUpdateEmail` and `onSuccessDestroyEmail` run as an implicit `Email/set` after the submission succeeds (verified: RFC 8621 section 7.5).

### Delayed send, scheduled send and undo

- RFC 8621 defines `maxDelayedSend` (seconds; 0 means unsupported), `sendAt` ("the date the submission was/will be released for delivery"), and a generic `parameters` map on envelope addresses. It names FUTURERELEASE ([RFC 4865](https://www.rfc-editor.org/rfc/rfc4865.html)) as an example submission extension (verified: [RFC 8621](https://www.rfc-editor.org/rfc/rfc8621.html)). RFC 4865 defines two MAIL FROM parameters, of which a client sends one: `HOLDFOR=<seconds>` (1 to 999999999) or `HOLDUNTIL=<UTC date-time>` (verified: RFC 4865). Passing them as `envelope.mailFrom.parameters` keys follows from the two specs (inferred).
- `undoStatus` is `pending`, `final` or `canceled`. While `pending`, a client may set it to `canceled`. Failure returns `cannotUnsend` (verified: RFC 8621).
- Fastmail advertises `maxDelayedSend` of 512 days but an empty `submissionExtensions` map (verified: 2024 dump). Whether it accepts `HOLDFOR` and `HOLDUNTIL` with that empty map is unknown (unverified). Test it first.
- If delayed submission works, mejl gets both features from one primitive (inferred):
  - Undo send: submit with `HOLDFOR` of 10 to 30 seconds, show an undo bar, and set `undoStatus: canceled` if the user presses undo. This survives a mejl crash.
  - Scheduled send: submit with `HOLDUNTIL`. Then list pending items with `EmailSubmission/query` (filter `undoStatus: pending`). Check whether Fastmail also files these in its Scheduled folder (role or attribute `scheduled`, per [RFC 9979](https://www.rfc-editor.org/rfc/rfc9979.html)) so the web app shows them (unverified).
- If it does not work, mejl can still offer undo with a client-side hold before submitting. Scheduled send would then need mejl to stay running, which is a weaker feature (inferred).
- Fastmail's own undo send delays web and app sends by 20 seconds and "can't be used with third party email clients" (verified: [undo send help](https://www.fastmail.help/hc/en-us/articles/1500000278222-Undo-send)). That suggests the web app uses the same server-side hold (inferred).

### Other sending notes

- The ToS says the service is "not designed for sending automated or transactional emails" and forbids sending "programmatically generated emails to addresses that are outside your Customer Account" (verified: [ToS](https://www.fastmail.com/policies/terms-of-service/) section 3). Irrelevant to normal client use, but it rules out mail-merge style features.
- Basic plan users cannot schedule send (verified: [scheduled send help](https://www.fastmail.help/hc/en-us/articles/4686576659727-Scheduled-Send)).

## Search

### Server-side filters in JMAP

`Email/query` `FilterCondition` (verified: [RFC 8621 section 4.4.1](https://www.rfc-editor.org/rfc/rfc8621.html)): `inMailbox`, `inMailboxOtherThan`, `before`, `after`, `minSize`, `maxSize`, `allInThreadHaveKeyword`, `someInThreadHaveKeyword`, `noneInThreadHaveKeyword`, `hasKeyword`, `notKeyword`, `hasAttachment`, `text`, `from`, `to`, `cc`, `bcc`, `subject`, `body`, `header` (one or two strings). `FilterOperator` combines them with `AND`, `OR`, `NOT`.

Fastmail's web search syntax is richer (verified: [search help](https://www.fastmail.help/hc/en-us/articles/360060591213-Searching-your-mail)). Mapping to standard JMAP:

| Fastmail operator | Standard JMAP equivalent | Notes |
|---|---|---|
| bare words, `"phrase"` | `text` | Stemming on the server; quotes turn it off at Fastmail. Whether JMAP `text` gets the same stemming is unverified |
| `from:`, `to:`, `cc:`, `bcc:`, `subject:`, `body:` | same-named conditions | Fastmail's `to:` also matches Cc and Bcc; JMAP `to` is To only (inferred difference) |
| `with:` | `OR` of from, to, cc, bcc | client-side expansion |
| `in:folder` | `inMailbox` | resolve the name to an id locally |
| `in:*` | omit `inMailbox` | Fastmail excludes Spam and Trash by default in its UI; JMAP has no such default (inferred) |
| `before:`, `after:`, `date:` | `before`, `after` | relative dates resolved in the client |
| `larger:`, `smaller:` | `minSize`, `maxSize` | |
| `is:unread`, `is:pinned`, `is:replied`, `is:draft`, `is:muted` | `notKeyword`/`hasKeyword` with `$seen`, `$flagged`, `$answered`, `$draft`, `$muted` | `$muted` name inferred |
| `has:attachment` | `hasAttachment` | |
| `header:"Name:text"`, `list:` | `header: ["Name", "text"]`, `header: ["List-Id", "..."]` | |
| `keyword:` | `hasKeyword` | |
| `fromin:contacts`, `fromin:vips`, `tonotcc:`, `deliveredto:`, `memo:`, `attached:`, `filename:`, `filetype:`, `msgid:`, `priority:high` | no standard equivalent | Some can be emulated with `header`, a local contacts join or a local index. Fastmail may support them through a private filter (unverified) |

Fastmail search limits that carry over (verified: search help): no regular expressions; punctuation is not indexed; `*` works only at the end of a word.

### Search snippets

`SearchSnippet/get` takes `accountId`, the same `filter` as the query, and `emailIds`. It returns `subject` and `preview` with matches marked by `<mark>` (preview at most 255 octets). Results can differ between calls (verified: [RFC 8621 section 5](https://www.rfc-editor.org/rfc/rfc8621.html)). Chain it after `Email/query` with a back-reference to `/ids` so one request returns hits and highlights (inferred). A TUI can render `<mark>` as reverse video.

### Performance

- Fastmail search is a Xapian index inside Cyrus with a tiered layout: new mail is indexed into a small, fast database and merged into larger ones later (verified: [2014 blog](https://www.fastmail.com/blog/email-search-system/)). Users and the CPO cite search as a strength ([HN thread](https://news.ycombinator.com/item?id=45564619)).
- There may be a short delay before brand-new mail is searchable (inferred from the tiered design).
- Design for mejl (inferred): run a local index (headers, previews, and bodies the user has opened) for instant as-you-type results, and send the same query to the server in parallel, merging when it returns. Use server results as the source of truth for full-text body and attachment search.

## Rules (Sieve)

This section is the core finding for mejl's rule management goal. [fastmail-rules-sieve.md](fastmail-rules-sieve.md) covers Sieve itself in more depth.

### What exists publicly

- Fastmail runs rules as Sieve. The UI generates the script. The custom Sieve editor exposes "four unchangeable blocks with Sieve code automatically generated by us" interleaved with "four editable text areas" (verified: [Sieve in Fastmail](https://www.fastmail.help/hc/en-us/articles/1500000280481-Using-Sieve-scripts-in-Fastmail)).
- Supported extensions include `fileinto`, `reject`, `vacation`, `vacation-seconds`, `relational`, `envelope`, `body`, `comparator-i;ascii-numeric`, `variables`, `subaddress`, `copy`, `regex`, `mailbox`, `mboxmetadata`, `date`, `index`, `imap4flags`, `editheader`, `duplicate`. The script's `require` line also lists `servermetadata`, `fcc`, `vnd.cyrus.jmapquery`, `vnd.cyrus.log`, `mailboxid`, `special-use`, `vnd.cyrus.snooze`, `vnd.cyrus.imip`, `vnd.cyrus.implicit_keep_target` (verified: same page).
- The help page says "you can also import or export your Sieve scripts as .json files" from Settings, Filters & Rules (verified: same page). Despite the wording, the file is a `.json` export, so it likely carries rules too (inferred). The format is undocumented (unverified).
- Sieve can push-notify the Fastmail app with `addflag "$notify"` (verified: [Sieve FAQ](https://www.fastmail.help/hc/en-us/articles/360058753814-Sieve-frequently-asked-questions)). `$notify` is one of the RFC 9979 keywords, so Fastmail does use at least this one (verified: [RFC 9979](https://www.rfc-editor.org/rfc/rfc9979.html) section 7.8). Sieve runs after the SMTP connection closes, so it cannot reject before DATA (verified: Sieve FAQ).
- No ManageSieve. "The only way to modify your script is by logging in to the web interface." A Sieve tester exists in the web UI (verified: [Sieve FAQ](https://www.fastmail.help/hc/en-us/articles/360058753814-Sieve-frequently-asked-questions)).
- No rules scope for tokens or OAuth (verified: token help and dev docs). No `urn:ietf:params:jmap:sieve` in any session we saw (inferred).

### What the web app uses (private)

From third-party source code that replays web app requests (verified: code in [fastmail-rules-cli](https://github.com/dvcrn/fastmail-rules-cli), last commit 2026-04-28, and [Fastmail-Sieve-Sync](https://github.com/fuyu0425/Fastmail-Sieve-Sync), last commit 2023-05-06):

- Capability `https://www.fastmail.com/dev/rules` (with `https://www.fastmail.com/dev/user`).
- `Rule/get` and `Rule/set`. A rule has `name`, `conditions` (each with `lookHow`, `lookFor`, `lookIn`), `combinator`, `isEnabled`, `sortOrder`, `stop`, `skipInbox`, `markRead`, `snoozeUntil`, `redirectTo`, `search`, `filter` and more.
- `SieveBlocks/get` and `SieveBlocks/set` on a `singleton` object with `sieveRequire`, `sieveAtStart`, `sieveForBlocked`, `sieveAtMiddle`, `sieveForRules`, `sieveAtEnd` (verified: both repos). Which fields the server lets you write is unverified. Fastmail-Sieve-Sync writes only `sieveAtStart`, `sieveAtMiddle` and `sieveAtEnd`, while fastmail-rules-cli lets the user patch any of the six (verified: their source). The help page describes "four unchangeable blocks" and "four editable text areas", which does not match three custom fields in this object (unverified mismatch).
- fastmail-rules-cli also calls a `Validate/sieve` method that returns `isValid` and `errorDescription` (verified: `jmap/sieve.go`). That is a private server-side Sieve validator.
- Auth: a browser bearer token plus the `__Host-s_...` session cookie. Some writes fail with `forbidden` or `needsSudo` until a fresh `/auth/sudo` password prompt is replayed. The rules CLI says writes "usually need browser-session authentication, not a normal API token."

The API Developer Policy forbids unpublished APIs and circumventing access controls (verified, see "Limits"). This path is off limits for a distributed mejl.

### Options for mejl (inferred)

| Option | User experience | Risk |
|---|---|---|
| A. Ask Fastmail to expose RFC 9661 `SieveScript` or `dev/rules` to OAuth and tokens | Seamless, the goal | Depends on Fastmail. Cyrus, Fastmail's server ([open source page](https://www.fastmail.com/company/open-source/)), has JMAP Sieve code and tests ([`JMAPSieve.pm`](https://github.com/cyrusimap/cyrus-imapd/blob/master/cassandane/Cassandane/TestSuite/JMAPSieve.pm), verified), and the IETF OAuth draft edited by Fastmail's Neil Jenkins already maps `jmap:sieve` to the `mail` scope. So the ask is about policy, not engineering (inferred) |
| B. Local rule editor that generates Sieve for the editable blocks, validates it locally, and copies it to the clipboard with a deep link to Settings, Filters & Rules | One paste per change | Low. Policy-safe |
| C. Client-side rules: mejl applies actions after sync (move, label, mark read) using JMAP | Works only while mejl runs; no delivery-time effects such as reject or vacation | Low, but duplicates server rules and can fight them |
| D. Opt-in "unsupported" mode that uses the private API with a user-supplied browser session | Seamless while it works | Breaks without notice; violates the Developer Policy; needs the user's session cookie and sudo; not shippable as a default |
| E. Read-only view: parse a user-exported rules JSON or pasted Sieve to show and search rules in the TUI | Helpful for auditing | Low |

A practical plan: B plus E for v1, open a conversation with Fastmail about A at once, and keep D out of the official build.

## Masked Email (public)

- Capability `https://www.fastmail.com/dev/maskedemail`. Methods `MaskedEmail/get` (`ids: null` fetches all) and `MaskedEmail/set` (standard RFC 8620 `/set`) (verified: [dev docs](https://www.fastmail.com/dev/#masked-email-api)).
- Properties: `id`, `email`, `state` (`pending`, `enabled`, `disabled`, `deleted`), `forDomain`, `description`, `lastMessageAt`, `createdAt`, `createdBy` (server-set from the client name), `url`, and create-only `emailPrefix` (at most 64 characters of `a-z`, `0-9`, `_`; "not returned when MaskedEmail objects are fetched") (verified: dev docs).
- States: `enabled` receives mail normally; `disabled` is active but "mail is sent straight to trash"; `deleted` bounces mail. Once a state leaves `pending` it "cannot be set back to pending" (verified: dev docs). The web UI calls these Active, Blocked and Deleted, and says deleted addresses "can be restored at any time" (verified: [Masked Email help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email)). The mapping Blocked = `disabled` is inferred from the matching descriptions.
- Pending addresses are "automatically deleted 24h after creation and are not shown in the Fastmail UI", unless mail arrives first, which turns them `enabled` (verified: dev docs). Create with `state: enabled` when the user wants the address now (inferred).
- The address domain follows the account's Masked Email domain setting, which can be a custom domain. Generated addresses look like `dog.food3495@domain.tld` (verified: dev docs, Masked Email help).
- The web UI can compose from a masked address through "Show all" in the sender menu, and replies to mail sent to a masked address use that address by default (verified: Masked Email help). Whether JMAP `Identity/get` exposes these addresses is still open.
- Field rules: do not set `createdBy`; `forDomain` is an origin only, for password managers; `description` is the user's text, not the app name; `url` is a deep link or null (verified: dev docs). For a mail client, `forDomain` can be the origin of the site the user is signing up for, if mejl asks (inferred).
- Creation is rate limited (`rateLimit` SetError on create) (verified: dev docs). The help page says to "Try again in 24 hours" (verified: Masked Email help).
- Detail, sending questions and UI ideas are in [fastmail-masked-email.md](fastmail-masked-email.md).

Example (illustrative):

```json
{
  "using": ["urn:ietf:params:jmap:core", "https://www.fastmail.com/dev/maskedemail"],
  "methodCalls": [
    ["MaskedEmail/set", {"accountId": "u1", "create": {"m1": {
      "state": "enabled", "description": "Bike shop newsletter", "emailPrefix": "bikes"
    }}}, "0"]
  ]
}
```

## IMAP, SMTP, CalDAV, CardDAV and WebDAV fallbacks

| Protocol | Host | When mejl might need it | Label |
|---|---|---|---|
| IMAP | `imap.fastmail.com:993` (TLS) | Probably never. JMAP covers mail. Maybe for a user who refuses API tokens | verified: [server names](https://www.fastmail.help/hc/en-us/articles/1500000278342-Server-names-and-ports) |
| SMTP | `smtp.fastmail.com:465` (TLS) or 587 (STARTTLS) | Never for normal sending (JMAP submission is better) | verified: same |
| POP | `pop.fastmail.com:995` | Never | verified: same |
| CalDAV | `https://caldav.fastmail.com/` | Calendar invites (accept, decline), agenda pane, until JMAP calendars opens | verified: same; need inferred |
| CardDAV | `https://carddav.fastmail.com/` | Not needed: JMAP Contacts is public | verified: same |
| WebDAV | `https://webdav.fastmail.com/` or `https://myfiles.fastmail.com/` | "Attach from Files" or "save attachment to Files" | verified: same |
| Regional hosts | `eu-` and `us-` prefixed IMAP, SMTP, POP, CalDAV, CardDAV, MyFiles | Not needed; generic hosts route correctly | verified: [data residency help](https://www.fastmail.help/hc/en-us/articles/16796454162063-Choosing-your-data-residency) |

These protocols use app passwords, not API tokens: "You will need to get an app password to connect to these servers" (verified: server names page). The dev docs also say "You can use OAuth with all of our supported protocols" (verified: [dev docs](https://www.fastmail.com/dev/)). The OAuth metadata lists a `calendars` scope, and the open OAuth draft says a server that offers CalDAV "MUST support" it (verified: draft section 3.9). So OAuth for CalDAV is documented in principle, but we have not seen it work with an open-registered client (unverified).

## Fastmail's official MCP server

### Facts

- Launched 2026-04-22 ("National Email Day") at `https://api.fastmail.com/mcp`. Fastmail says it has "not integrated AI into Fastmail"; the MCP server is "simply another API endpoint" (verified: [Fastmail blog](https://www.fastmail.com/blog/an-mcp-server-for-fastmail/), Rob Mueller, CTO).
- Auth: OAuth 2.0 with a consent screen offering three levels: read, write, send. API tokens of type "MCP" with scopes "Read data", "Make changes", "Send email" also work (verified: [MCP help](https://www.fastmail.help/hc/en-us/articles/15869557281295-Connecting-AI-tools-via-Fastmail-s-MCP-server), [API tokens help](https://www.fastmail.help/hc/en-us/articles/5254602856719-API-tokens)). The OAuth scope is `https://www.fastmail.com/dev/mcp` (verified: protected resource metadata via `curl`).
- Connected AI clients appear under Settings, Privacy & Security, Connected apps, with first and last access times (verified: MCP help).
- The help page says "we don't support accessing attachments via MCP" (verified: MCP help). The tool list below includes `read_attachment` and `upload_attachment`. The help page is probably stale (inferred). This is a docs mismatch to report.

### Tools exposed

Primary-source observation from the user's connected Fastmail MCP (tool names only; no tool was called):

| Area | Tools |
|---|---|
| Mail | `search_email`, `read_email`, `read_thread`, `draft_email`, `update_email`, `archive_email`, `delete_email`, `set_memo`, `list_folders`, `list_identities`, `read_attachment`, `upload_attachment` |
| Contacts | `search_contacts`, `search_org_contacts`, `create_contact`, `update_contact`, `delete_contact` |
| Calendar | `list_calendars`, `search_events`, `create_event`, `update_event`, `delete_event`, `rsvp_event`, `compose_event` |
| Notes | `search_notes`, `read_note`, `create_note`, `update_note`, `add_to_note`, `delete_note` |

The server instructions also mention `send_email`. It is not in the exposed list, which suggests this connection was granted without the send level (inferred). The instructions say bodies accept markdown converted to HTML with a plain text fallback; replies use `mode: reply` with threading and quoting handled by the server; `compose_event` stages an event in a widget for the user to confirm. The widget suggests use of the MCP Apps extension, which the launch post names (inferred).

### What is absent

- Masked Email: no tools.
- Rules, Sieve, filters, spam settings: no tools.
- Mailbox management: `list_folders` only; no create, rename, delete or reorder.
- Vacation response, identities management, settings, Files: no tools.
- Sync primitives: no state strings, no change feeds, no push.

### Does it matter to mejl? (inferred)

- **Not as a backend.** It is shaped for language models: coarse tools, server-side markdown conversion, no incremental sync. JMAP is strictly more capable for a client.
- **As a signal.** Fastmail ships notes, calendar and memo features through its private JMAP to this server, while JMAP tokens still lack calendars. Fastmail may widen public JMAP once calendars is an RFC.
- **As coexistence.** A user's AI assistant may move, archive or draft mail through MCP while mejl is open. mejl sees those as ordinary state changes through push. Nothing special is needed beyond correct sync.
- **As an opportunity.** mejl could later run its own local MCP server (stdio) on top of its cache. It could offer what Fastmail's lacks: Masked Email create and disable, rule drafting, instant local search, bulk triage. That is optional and should wait until the core client is done (YAGNI).

## Developer ecosystem

### Fastmail on GitHub

From [github.com/fastmail](https://github.com/fastmail) (36 repositories; verified 2026-10-23):

- [JMAP-Samples](https://github.com/fastmail/JMAP-Samples): starter code in JavaScript, Lua, Perl 5 and Python 3. MIT. Small; does not cover push.
- JMAP-TestSuite, JMAP-Tester, JMAP-Tester-WebSocket (Perl): Fastmail's own JMAP conformance tooling. Last commits 2026-10-02, 2026-09-18 and 2026-06-18 (verified: GitHub API).
- Text-JSCalendar, JSCalendar-Duration (Perl): calendar data conversion.
- overture (JS app framework of the web client), Squire (rich text editor), authentication_milter, mail-spf.
- Cyrus IMAP lives at [cyrusimap/cyrus-imapd](https://github.com/cyrusimap/cyrus-imapd). Fastmail is its main maintainer (verified: [open source page](https://www.fastmail.com/company/open-source/)). Cyrus source is the best reference for undocumented server behaviour, but mejl should still only call documented capabilities.

### Documentation and posts

- [fastmail.com/dev](https://www.fastmail.com/dev/): the only official API page. Covers session URL, tokens, Masked Email and OAuth.
- [JMAP crash course](https://jmap.io/crash-course.html) and [jmap.io specs](https://jmap.io/spec/index.html).
- Blog posts worth reading: the [1Password Masked Email post](https://www.fastmail.com/blog/how-and-why-we-built-masked-email-with-jmap-an-open-api-standard/) (2021), the [email search system](https://www.fastmail.com/blog/email-search-system/) (2014), the [MCP launch](https://www.fastmail.com/blog/an-mcp-server-for-fastmail/) (2026), [data resilience](https://www.fastmail.com/blog/data-resilience-at-fastmail/) (2026).

### Clients and libraries listed on jmap.io

From [jmap.io/software](https://jmap.io/software/index.html) (verified 2026-10-23):

- Terminal clients: aerc (Go), meli (Rust), MailSalon (Go). A community-run aerc Fastmail guide puts an app password step before both its JMAP and IMAP options, and suggests direct IMAP if you "need features not yet supported by aerc's JMAP backend" (verified: [aerc-docs.com](https://aerc-docs.com/providers/fastmail/)). Whether that site is official aerc documentation is unclear (unverified).
- Desktop and mobile JMAP clients: Allodia, Boogie, Flectar Mail (Rust, native), Mailo, Parula, Ltt.rs, Mailtemi, Sterna Mail, Twake, Aria.
- Rust libraries: [jmap-client](https://github.com/stalwartlabs/jmap-client) 0.4.3 (Core, Mail, WebSocket; updated 2026-09-28), melib 0.8.13 (2026-01-05), jmap-tools 0.1.8, calcard 0.3.14, [Missive](https://github.com/jeffhuen/missive) 0.7.0 (verified: crates.io API, 2026-10-23). jmap.io also lists jmap-rs, but no crate of that name exists on crates.io (verified: crates.io API).
- Rules prior art: [JMAP Screener](https://github.com/kwatson/jmap-screener) (Go, last commit 2026-10-12) "turns mailbox moves into learned sender rules" and writes them as generated Sieve. It targets Stalwart and asks for Stalwart's `jmapSieveScriptGet` and `jmapSieveScriptSet` token permissions (verified: README), which means standard JMAP Sieve (inferred). So it would not work on Fastmail today. See [rust-crates.md](rust-crates.md) for the crate evaluation and [email-clients.md](email-clients.md) for client comparisons.

### Community Fastmail tools

- Rules and Sieve: [fastmail-rules-cli](https://github.com/dvcrn/fastmail-rules-cli) (Go) and [Fastmail-Sieve-Sync](https://github.com/fuyu0425/Fastmail-Sieve-Sync) (Python). Both use the private API with browser cookies. They prove the demand and show the data model.
- Masked Email: Fastmask (iOS), Bitwarden and 1Password integrations (verified: [Masked Email help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email)).
- Many unofficial Fastmail MCP servers on JMAP (for example [MadLlama25/fastmail-mcp](https://github.com/MadLlama25/fastmail-mcp)). Its README says "the `urn:ietf:params:jmap:calendars` scope is not available" to JMAP API tokens (verified: README). That matches the token help page.
- Browser add-ons that patch the web UI, such as [better-fastmail](https://github.com/davestewart/better-fastmail) (2022, no commits since), which says "common keyboard shortcuts are not exactly intuitive". This is a dated hint at unmet UX wants.

## Fastmail's own apps as the UX baseline

### What exists

- Web app at `app.fastmail.com`, built on Fastmail's Overture framework (verified: [open source page](https://www.fastmail.com/company/open-source/)). It can be installed as a PWA (verified: Coywolf calls the desktop app a wrapper for "its progressive web app (PWA)", and the CPO mentions PWA use in the [HN thread](https://news.ycombinator.com/item?id=45564619)).
- Desktop app for macOS 12+, Windows 10+ and "recent versions of Linux" since 2025-10-13. It is an Electron wrapper around the web app, with offline support, native notifications and default mail client handling (verified: [desktop help](https://www.fastmail.help/hc/en-us/articles/14026270682511-The-Fastmail-desktop-app), [blog](https://www.fastmail.com/blog/desktop-app/); Electron per the CPO's "Why Electron?" answer in the [HN thread](https://news.ycombinator.com/item?id=45564619) and [Coywolf](https://coywolf.com/news/productivity/fastmail-launches-native-desktop-apps-for-mac-windows-and-linux/)). The CPO said it reuses the web app code base and took "about 4 months work for 2 developers" (verified: HN thread).
- iOS and Android apps, also built on the web app (inferred from shared features and timing). iOS rating 3.8 from 583 ratings; Android 4.03 from 2,417 (verified on 2026-10-23: [App Store page](https://apps.apple.com/us/app/fastmail-email-calendar/id931370077), [AppBrain](https://www.appbrain.com/app/fastmail/com.fastmail.app)).
- Keyboard shortcuts are Gmail-like and extensive: `j`/`k`, `o`, `/`, `g` (go to folder by typing), `y` archive, `[`/`]` archive and move, `d` trash, `r`/`a`/`f`, `c`, `m` move, `l` labels, `b` then `1` to `7` snooze, `s` pin, `x` select, `Shift+M` mute, `z` undo (last 10 actions), `?` help (verified: [shortcuts help](https://www.fastmail.help/hc/en-us/articles/360058753534-Keyboard-shortcuts)). The help page offers only an on or off switch in Settings, Display options; it mentions no remapping and no command palette, so we infer neither exists (inferred).

### What users praise

- Speed and JMAP sync. The CPO says the app "resyncs way faster when you open the app, and uses a lot less data (JMAP is so much more efficient)", and that moving between messages is "usually instant". One user found the desktop app "much faster than Thunderbird" (verified: [HN thread](https://news.ycombinator.com/item?id=45564619)).
- Search quality (verified: HN thread).
- Integration of mail, contacts and calendar; aliases and Masked Email (verified: HN thread; a 2026-02-23 [Fastmail blog post](https://www.fastmail.com/blog/why-customers-trust-fastmail/) says aliases and the Masked Email integration "are among the most frequently celebrated features across the reviews").
- "Fastmail's killer feature is JMAP, and there are not so many GUI clients for that" (verified: HN thread).

### What users complain about

- Electron: memory use (one user saw 700 MB RAM on macOS) and little gain over the browser ("the app doesn't even add anything of value") (verified: HN thread). The original draft also cited a Reddit thread for "zero value add"; we could not open it (unverified).
- Not native: "the app actually doesn't look native at all" on macOS (verified: HN thread).
- No IMAP accounts in the desktop app: "I would use this desktop app if it was compatible with IMAP" (verified: HN thread). No unified inbox with other providers follows from that (inferred).
- Mobile hangs needing force restarts (2024 reviews); fiddly copying of addresses; no "all accounts" view (unverified: from App Store reviews the original author saw; the fact-check did not re-read them).
- Shortcuts are fixed and not always intuitive (verified: better-fastmail README; remapping absence from the shortcuts help).
- Third-party clients lose Fastmail features: snooze, undo send and rules are web-only (verified: snooze, undo and Sieve help pages).

### What this means for mejl (inferred)

- Match Fastmail's shortcut vocabulary by default so users feel at home, and make every key remappable.
- Win on cold start, memory and latency: a local cache shown before the network answers, and one batched request per user action.
- Support Fastmail-specific concepts the third-party world lacks: pins, mute and follow keywords, memos, masked addresses, grouped views built locally, label mode.
- Be honest in the UI about the web-only features (rules, snooze) and offer the best policy-safe path for each.

## Open questions

1. **What does a live session advertise to a scoped API token?** Run `curl -s -H "Authorization: Bearer $TOKEN" https://api.fastmail.com/jmap/session | jq .` with a token that has every JMAP scope, and again with a read-only token. Record `capabilities`, `accountCapabilities` (especially `submissionExtensions` and `maxDelayedSend`), limits, and whether `websocket`, `quota`, `blob`, `principals` or `urn:ietf:params:jmap:mail:snooze` appear. Commit the redacted result as a test fixture.
2. **Does open OAuth registration work?** POST a minimal RFC 7591 body (public client, loopback redirect, `urn:ietf:params:oauth:scope:mail offline_access`) to `https://api.fastmail.com/oauth/register`, complete the flow, then read the session. Check which capabilities the `mail` scope grants in practice. The draft says it includes submission and vacation response; confirm Fastmail follows that, and whether Masked Email can be requested at all (for example by asking for `https://www.fastmail.com/dev/maskedemail` at registration even though it is not in `scopes_supported`). Ask Fastmail to update the dev docs, which still describe manual registration.
3. **Does delayed submission work?** Submit to yourself with `envelope.mailFrom.parameters` `{"HOLDFOR": "60"}`, check `undoStatus` and `sendAt`, cancel one, let another send. Check whether held messages appear in the web app's Scheduled folder.
4. **Will Fastmail expose rules?** Write to Fastmail (partners page or support) asking for RFC 9661 `SieveScript` or the `dev/rules` capability under a documented OAuth and token scope. Also ask whether a third-party client counts as "replicating core products".
5. **Is EventSource still non-standard?** Connect with `curl -N -H "Authorization: Bearer $TOKEN" "https://api.fastmail.com/jmap/event/?types=*&closeafter=no&ping=60"`. Check the event shape (`@type` versus `type`), whether `ping` is honoured, and whether `closeafter=state` closes the stream (the 2023 report tested a parameter named `closeAfterState`, which is not the RFC name).
6. **Which keywords does Fastmail actually set?** Pin, mute, follow and add a memo in the web app, then read `keywords` via JMAP. Confirm `$flagged`, `$muted`, `$followed`, `$memo`, `$hasmemo`, and check the mailbox `role` values for Snoozed and Scheduled.
7. **Can snoozed state be read?** Snooze a message in the web app and `Email/get` it with all properties. If nothing exposes the wake time, mejl can show the Snoozed mailbox but not the time.
8. **Does moving mail into or out of Spam via JMAP train the filter?** Ask Fastmail support or test with `$junk` and `$notjunk` keywords plus a move. Check the next message's `X-Spam-*` headers.
9. **Do masked addresses appear in `Identity/get`, and can `EmailSubmission/set` send from them?** Tracked in [fastmail-masked-email.md](fastmail-masked-email.md).
10. **Does OAuth for JMAP work on the Basic plan?** Test with a Basic trial account, or ask support.
11. **Is the rules export JSON stable?** Export once, document the schema, and decide whether mejl can read it for option E.
12. **When does JMAP calendars open?** Watch draft-ietf-jmap-calendars in the RFC Editor queue and the Fastmail dev page. Until then plan CalDAV for invites only.
13. **Is a token-paste login acceptable for a public mejl release?** The dev docs say tokens are for testing or personal use and that distributed apps "should implement OAuth". Ask Fastmail whether an open source client where each user creates their own token is fine, or whether OAuth is expected from day one.
14. **Which `SieveBlocks` fields are writable, and why does the help page say four editable areas when the private object has three custom fields?** Only matters if Fastmail ever documents the API. Ask Fastmail, do not probe.

## Fact-check notes

An adversarial fact-check ran on 2026-10-23. It re-fetched the cited pages (dev docs, help center, policies, blog posts, RFC and draft texts, GitHub repos and APIs, crates.io, jmap.io, the HN thread) and re-ran the unauthenticated `curl` discovery calls. It did not call the user's mailbox or Fastmail's MCP tools, and it did not register an OAuth client.

### Confirmed without change

Most claims held up: the OAuth and JMAP discovery responses, the dev docs (OAuth flow, scopes, Masked Email), API token scopes, the Sieve FAQ and extension list, the policy quotes, the MCP launch facts and tool list, RFC numbers, dates and the section-level RFC claims, draft statuses, issue 7, both session dumps, the private `using` list, blog dates and crate versions. Every cited link returned HTTP 200. The non-200 URLs in the doc are capability identifiers or service endpoints, not citations.

### Changed

- Fixed the dead anchor `/dev/#oauth-at-fastmail` to `/dev/#o-auth-at-fastmail`.
- Added the dev docs guidance that distributed apps "should implement OAuth" and that tokens are for testing or personal use. Updated the login strategy and the summary to match.
- Replaced "unclear whether the `mail` scope covers submission" with the draft's own table (section 5.4.1): `mail` covers submission, vacation response, `jmap:sieve`, ManageSieve and more. Fastmail behaviour is still untested.
- Corrected "OAuth may cover CalDAV in future": the dev docs already say OAuth works "with all of our supported protocols".
- Downgraded "only the start, middle and end Sieve blocks are writable" to unverified. One tool writes only those three; the other patches all six. Noted the conflict with the help page's "four editable text areas". Added the private `Validate/sieve` method.
- Corrected the EventSource quirk: one event on 2023-01-30, and the ignored parameter was reported as `closeAfterState`, not `closeafter`.
- Corrected the 2024 dump description: an "Example result" with redacted ids, not self-described as illustrative.
- Corrected the data resilience post: all emails were recovered "for all but one user", not all.
- Corrected the customer trust post: aliases and Masked Email are "most frequently celebrated" in reviews, not "most used".
- Corrected the ToS sending clause, the logo and endorsement wording, RFC 9979 authorship, and JMAP-Tester-WebSocket's last commit date.
- Reworded the aerc claim. The guide does not say JMAP needs an app password, and the site may not be official.
- Marked jmap-rs as not on crates.io. Added Missive 0.7.0 and JMAP Screener as rules prior art.
- Relabelled absence claims (no shortcut remapping, no command palette) as inferred. Replaced the HN "search snippet" label with the thread itself. Downgraded the unopened Reddit claim and the App Store review details to unverified.
- Added: Basic plan has Masked Email but no API tokens; Mute is not on Basic; legacy plans under 10 GB lack Snooze and Scheduled Send; grouped view is on all plans; Masked Email UI states, restore, and send-from-masked in the web UI; the `$notify` Sieve flag; the `partnerships@fastmailteam.com` contact; the "Circumventing Fastmail's intended limitations" heading above the "replicate or compete" clause.

### Still uncertain

- Everything that needs a live session or a test account: the per-scope capabilities, delayed submission, EventSource shape, open registration, OAuth on Basic, and keyword use for pin, mute, follow and memo.
- The HN thread and app ratings will drift. They reflect 2026-10-23.
- The releasebot dates for "twom" (post 2025-06-13, in use since 2025-02-12), offline support (2025-08-26) and world clocks (2026-10-15) come from an aggregator. They were re-read there, not on Fastmail's own pages.
