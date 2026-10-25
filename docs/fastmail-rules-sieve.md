# Fastmail rules and Sieve: what mejl can manage

## Summary

Research date: 2026-10-23. All "as of" statements below mean as of this date.

The short answer: as of 2026-10-23, Fastmail offers no documented, programmatic way for a third-party client to read or write a user's rules or Sieve script. That rules out a fully "seamless" rules manager that syncs both ways with Fastmail. mejl can still offer a good rules experience, but the last step (getting the Sieve onto the server) has to go through the user, the web UI, or a future Fastmail API.

Main findings:

- **JMAP for Sieve (RFC 9661) does not appear to be offered to customers.** Fastmail's developer docs and its API token page list no Sieve or rules scope (verified absence: [Fastmail dev docs](https://www.fastmail.com/dev/), [API tokens help](https://www.fastmail.help/hc/en-us/articles/5254602856719-API-tokens)). One third-party live probe found no `urn:ietf:params:jmap:sieve` in a Fastmail session, and one third-party project states that tokens lack a Sieve scope (unverified: [terva probe, 2026-07-02](https://github.com/terva-sh/terva-ext-jmap-mail/blob/main/docs/filters-research.md), [shellguard/fastmail-mcp README](https://github.com/shellguard/fastmail-mcp)). Nobody in this research ran the probe with the user's own token, so the absence of the capability itself is (unverified). RFC 9661's sole author works at Fastmail, and Cyrus implements it (verified: [RFC 9661](https://www.rfc-editor.org/rfc/rfc9661.html), [Cyrus RFC support](https://www.cyrusimap.org/imap/rfc-support.html)). Fastmail runs Cyrus: its own example headers show `Cyrus 3.1.6-...-fmstable` and `X-Sieve: CMU Sieve 3.0` (verified: [The email delivery process](https://www.fastmail.help/hc/en-us/articles/1500000278262-The-email-delivery-process), a 2019 example). Server support likely exists; customer exposure does not, as far as any source shows (inferred).
- **ManageSieve (RFC 5804) is not supported.** Fastmail says so directly: "the only way to modify your script is by logging in to the web interface. Fastmail does not support the 'managesieve' extension" (verified: [Sieve FAQ](https://www.fastmail.help/hc/en-us/articles/360058753814-Sieve-frequently-asked-questions)). No host or port is published.
- **An undocumented web-client API exists, but mejl must not use it.** The web app talks to a private JMAP capability `https://www.fastmail.com/dev/rules`, with methods `Rule/get`, `Rule/set`, `SieveBlocks/get`, `SieveBlocks/set` and `Validate/sieve` (unverified as Fastmail behavior: the names are taken from the source code of two third-party tools, which I read). Using it needs the web session's cookie, and sometimes a password re-prompt token. Fastmail's API Developer Policy forbids "Using unpublished APIs" (verified: [API Developer Policy](https://www.fastmail.com/policies/api-developer-policy/)).
- **UI rules and hand-written Sieve share one generated script.** Fastmail writes fixed blocks (blocked senders, masked email, spam, rules) and leaves insertion points for custom code. The help page says four fixed blocks and four editable areas (verified: [Using Sieve scripts](https://www.fastmail.help/hc/en-us/articles/1500000280481-Using-Sieve-scripts-in-Fastmail)). The third-party sources show three editable areas (unverified), and one of them says its layout is abbreviated. This conflict is unresolved.
- **sieve-rs (Stalwart) is a good Sieve engine but a poor Fastmail validator.** In a local test, it accepted unknown tests and unknown `require` strings, failed `jmapquery` at runtime, and refused to compile `snooze` and `implicit_keep_target` (verified: local experiment, sieve-rs 1.0.2, 2026-10-23, rerun by the fact-checker with the same result). crates.io lists it as AGPL-3.0-only, which affects mejl's license.
- **Recommended design:** mejl keeps its own rule model, renders one clearly marked Sieve block, and the user pastes it into one Fastmail custom section. Documented JMAP covers the parts around it: previewing matches, applying a rule to existing mail, allow-listing a sender through contacts, and disabling Masked Email addresses. If Fastmail ever exposes RFC 9661, the same model can be pushed directly.

## How to read this document

- "(verified: X)" means a primary source X confirms the claim, or I ran the check myself and say so.
- "(inferred)" means I reasoned it from verified facts. It may be wrong.
- "(unverified)" means a secondary or third-party source says it, or I could not confirm it.
- I did not call any tool connected to the user's real Fastmail mailbox. Statements about Fastmail's MCP tools come only from the tool names visible to this research session.

## 1. JMAP for Sieve (RFC 9661) at Fastmail

### What RFC 9661 would give mejl

[RFC 9661](https://www.rfc-editor.org/rfc/rfc9661.html) (Standards Track, September 2024) is exactly the API mejl would want (verified: RFC 9661):

| Item | Detail |
| --- | --- |
| Capability | `urn:ietf:params:jmap:sieve`. Session level has `implementation`. Account level has `maxSizeScriptName`, `maxSizeScript`, `maxNumberScripts`, `maxNumberRedirects`, `sieveExtensions`, `notificationMethods`, `externalLists`. |
| Object | `SieveScript` with `id`, `name`, `blobId`, `isActive`. Only one script can be active per account. |
| Methods | `SieveScript/get`, `SieveScript/set`, `SieveScript/query`, `SieveScript/validate`. |
| Activation | `SieveScript/set` takes `onSuccessActivateScript` and `onSuccessDeactivateScript`, so a client can upload a new script and switch to it atomically. |
| Content | Script text is a blob. Upload it through the session `uploadUrl`, download it through `downloadUrl` ([RFC 8620 sections 6.1 and 6.2](https://www.rfc-editor.org/rfc/rfc8620.html)). |
| Errors | `invalidSieve`, whose `description` SHOULD (not MUST) give at least the line number of the first error, and `sieveIsActive` on destroying the active script. |
| ManageSieve | The data model "is designed to allow a server to provide consistent access to the same scripts via ManageSieve ... as well as JMAP". `SieveScript/validate` is equivalent to `CHECKSCRIPT`, and downloading content is equivalent to `GETSCRIPT`. |
| Vacation | Section 4: if a server implements `VacationResponse` as a Sieve script, that script MUST be fetchable and activatable through `SieveScript/*`, but MUST NOT be updated or destroyed there (a `forbidden` SetError). |

The sole author is K. Murchison of Fastmail US LLC (verified: RFC 9661).

### What Fastmail exposes

- **No sieve scope in OAuth.** The dev docs list these OAuth scopes: `urn:ietf:params:jmap:core`, `:mail`, `:submission`, `:vacationresponse`, `:contacts`, and `https://www.fastmail.com/dev/maskedemail`. There is no Sieve or rules scope (verified: [Fastmail dev docs](https://www.fastmail.com/dev/)).
- **No sieve scope for API tokens.** The API token page offers read-only access, Email, Email submission, Contacts and contact groups, and Masked Email for the JMAP token type, plus three levels for the MCP token type. Nothing covers rules, filters, Sieve or settings (verified: [API tokens help](https://www.fastmail.help/hc/en-us/articles/5254602856719-API-tokens)). It also lists no vacation response scope, while OAuth has one (verified: same two pages). The same page says "API tokens are not available to users on Basic subscription plans" (verified).
- **Third-party probes found no sieve capability.** A live probe on 2026-07-02 with an API token saw only `core`, `mail`, `submission`, `contacts` and `maskedemail`. Its token was scoped for mail, submission, contacts and masked email, and the author notes that capability lists are filtered by scope (`vacationresponse` was also absent) (unverified: [terva filters research](https://github.com/terva-sh/terva-ext-jmap-mail/blob/main/docs/filters-research.md)). Another project states "Fastmail's public API tokens do not currently support Calendar or Sieve filter scopes" (unverified: [shellguard/fastmail-mcp README](https://github.com/shellguard/fastmail-mcp), last commit 2026-04-19 UTC). One personal blog post says "if I _really_ wanted to, I can use `SieveScript/set` - but I'm too lazy", so its author did not try it (unverified, untested: [shunueda.org, 2026-07-21](https://shunueda.org/2026-07-21--til-sieve-mail-filtering-language.html)). That claim conflicts with every other source and should be settled by the verification procedure.
- **Server capability is there.** Cyrus lists RFC 9661 and RFC 5804 as supported (verified: [Cyrus RFC support](https://www.cyrusimap.org/imap/rfc-support.html)). So the gap is a product decision at Fastmail, not missing server code (inferred).

**Verdict:** Fastmail does not advertise `urn:ietf:params:jmap:sieve` to API tokens or OAuth clients as far as any source shows (unverified, strongly suggested). The verification procedure at the end confirms this with the user's own token in a few minutes.

## 2. ManageSieve (RFC 5804)

- Fastmail's own FAQ: "Do I have to log in to the web interface to edit my script? Yes, the only way to modify your script is by logging in to the web interface. Fastmail does not support the 'managesieve' extension." (verified: [Sieve FAQ](https://www.fastmail.help/hc/en-us/articles/360058753814-Sieve-frequently-asked-questions)).
- The server names page lists IMAP 993, POP 995, SMTP 465/587, CalDAV, CardDAV and WebDAV. It does not mention Sieve, ManageSieve or port 4190 (verified: [Server names and ports](https://www.fastmail.help/hc/en-us/articles/1500000278342-Server-names-and-ports)).
- DNS on 2026-10-23: `_jmap._tcp.fastmail.com` points to `api.fastmail.com:443` and `_imaps._tcp.fastmail.com` to `imap.fastmail.com:993`. `_sieve._tcp.fastmail.com` returned no SRV record (verified: local DNS lookup, 2026-10-23).
- A TCP connect from one network to `imap.fastmail.com:4190` timed out while port 993 on the same host connected (verified: local probe, 2026-10-23, repeated by the fact-checker the same day). This is weak evidence, since one network's firewall could explain it. Note that `*.fastmail.com` has wildcard DNS, so a name like `sieve.fastmail.com` resolving means nothing (verified: `sieve.fastmail.com` and a random name both resolved to 103.168.172.37 and 103.168.172.52, while `imap.fastmail.com` resolved to other addresses). A direct query to 1.1.1.1 for `_sieve._tcp.fastmail.com` returned NXDOMAIN (verified: local probe, 2026-10-23).

**Verdict:** no ManageSieve, no host, no port (verified). Crates such as `io-managesieve` are only useful for non-Fastmail servers.

## 3. Other routes

### 3.1 The undocumented web-client API (rejected)

What is known, all from third-party code (unverified unless stated):

- The web app's JMAP session carries a private capability `https://www.fastmail.com/dev/rules`, with an account capability `{"rulesVersion": 3}` in a 2022 sample (unverified: [dwin/fastmask example session](https://github.com/dwin/fastmask/blob/main/pkg/fastmail/examples/successful_auth_response.json), a web-login session object, not an API-token session). That sample session also has no `urn:ietf:params:jmap:sieve`. It lists other private capabilities too, such as `https://www.fastmail.com/dev/notes`, `.../dev/files` and `.../dev/user` (unverified: same file, checked 2026-10-23).
- Methods used by [dvcrn/fastmail-rules-cli](https://github.com/dvcrn/fastmail-rules-cli) (Go, created April 2026; `jmap/rules.go` and `jmap/sieve.go`) and [fuyu0425/Fastmail-Sieve-Sync](https://github.com/fuyu0425/Fastmail-Sieve-Sync) (Python, 2023; `sieve_sync/sieve.py`, which uses `https://www.fastmail.com/dev/rules` and `https://www.fastmail.com/dev/user`) (unverified as Fastmail behavior; the method names are confirmed in that source code):

  | Method | Purpose |
  | --- | --- |
  | `Rule/get`, `Rule/set` | Read and write UI rules as objects. |
  | `SieveBlocks/get`, `SieveBlocks/set` | Read and write the script sections of a singleton object (`id: "singleton"`). |
  | `Validate/sieve` | Validate a whole script. Returns `isValid` and `errorDescription`. |

- `SieveBlocks` has six string fields, in script order: `sieveRequire`, `sieveAtStart`, `sieveForBlocked`, `sieveAtMiddle`, `sieveForRules`, `sieveAtEnd` (unverified: both tools agree). Sieve-Sync writes only `sieveAtStart`, `sieveAtMiddle` and `sieveAtEnd`, which suggests those are the editable parts (inferred).
- Auth: "Fastmail rule and Sieve writes usually need browser-session authentication, not a normal API token." Users copy the bearer token and cookie from browser devtools. Some writes fail with `needsSudo` until the user replays a password prompt request to `/auth/sudo` (unverified: [dvcrn README](https://github.com/dvcrn/fastmail-rules-cli)). Sieve-Sync needs a `__Host-s_...` session cookie (unverified: [Sieve-Sync README](https://github.com/fuyu0425/Fastmail-Sieve-Sync)), and its author writes "The API interface for Sieve is not public" and offers to take it down if Fastmail objects.

Why mejl must not use it:

- The [API Developer Policy](https://www.fastmail.com/policies/api-developer-policy/) lists "Using unpublished APIs" and "bypassing or circumventing Fastmail protocols and access controls" as violations, with consequences including "token revocation, developer suspension" and legal action (verified). It also prohibits "Asking End-Users to provide sensitive, private, and confidential personal information, such as credit card numbers or passwords unless specifically necessary" (verified), which a cookie-copying flow would come close to.
- The [API Terms](https://www.fastmail.com/policies/api-terms-of-service/) cover "our publicly available application programming interfaces" and say the contract "does not grant you any right to access or use ... any of our non-public application programming interfaces". They say undocumented parts "may change at any time, you should not rely on their behaviours", call APIs "that are not made generally available but that are otherwise made available to you" confidential information, and forbid attempts to "reverse engineer or otherwise derive source code, trade secrets, or know-how of our APIs or Core Services" (verified).
- Practically, mejl would have to handle a user's web session cookie and a sudo flow. That is a credential-handling risk and breaks whenever the web app changes (inferred).

This is the only route that would give true two-way sync today. It is out of bounds for a published client. A user may choose to run such a tool for themselves, but mejl should not ship it, document it as a feature, or depend on it.

### 3.2 Fastmail's MCP server

- Fastmail launched an MCP server at `https://api.fastmail.com/mcp` on 2026-04-22, with read, write and send access levels (verified: [blog post](https://www.fastmail.com/blog/an-mcp-server-for-fastmail/), [MCP help](https://www.fastmail.help/hc/en-us/articles/15869557281295-Connecting-AI-tools-via-Fastmail-s-MCP-server)). Neither page mentions rules, filters, Sieve or Masked Email.
- The Fastmail MCP tool names visible to this research session cover email, attachments, notes, contacts, calendars, identities, folders and memos. None is about rules, filters, Sieve or Masked Email (inferred: tool list visible to this session, rechecked by the fact-checker; no tool was invoked).

**Verdict:** MCP is not a rules route.

### 3.3 "Edit custom Sieve code" in the web UI

This is the only supported way to change Sieve. Settings, Filters & Rules, then **Edit custom Sieve code** at the bottom (verified: [Using Sieve scripts](https://www.fastmail.help/hc/en-us/articles/1500000280481-Using-Sieve-scripts-in-Fastmail)). The page shows the whole script with read-only generated blocks and text areas for custom code. Section 4 covers the layout. mejl can deep-link to `https://app.fastmail.com/settings/filters` (verified: the help pages link to `https://app.fastmail.com/settings/filters?sessionpicker=1`) and put text on the clipboard. The user does the paste.

### 3.4 Rules import and export as JSON

- "You can export your account's rules as a JSON file, and later import them into any account that accepts importing rules in JSON format." Import also accepts Gmail XML (verified: [Filters & Rules help](https://www.fastmail.help/hc/en-us/articles/1500000278122-Filters-Rules)).
- The Sieve help says "You can also import or export your Sieve scripts as .json files" from the same screen (verified: [Using Sieve scripts](https://www.fastmail.help/hc/en-us/articles/1500000280481-Using-Sieve-scripts-in-Fastmail)).
- The JSON format is not documented. Whether import replaces or appends rules is not documented (unverified). It may match the `Rule` objects in section 5.4 (inferred, unverified).

This is a user-mediated round trip that needs no unpublished API: the user exports, mejl reads the file, mejl writes a file, the user imports. It is worth testing (see open questions).

### 3.5 Fastmail's Sieve tester

Fastmail hosts a [Sieve tester](https://app.fastmail.com/sievetester) where you paste a full script and a raw message to see what it would do (verified: [How to use Sieve](https://www.fastmail.help/hc/en-us/articles/360060591373-How-to-use-Sieve); the URL returned HTTP 200 on 2026-10-23). It is a web page, not an API. mejl can point users to it for a server-side check of a pasted script (inferred).

### 3.6 Vacation response

Vacation appears to be Sieve under the hood (unverified: a generated `### Sieve generated for vacation responses` block appears in the [gr3enarr0w sample](https://github.com/gr3enarr0w/fastmail-mcp-server/blob/main/sieve/block3-execute-rule-actions.txt)). Fastmail exposes it as the standard JMAP `VacationResponse` object with OAuth scope `urn:ietf:params:jmap:vacationresponse` (verified: [Fastmail dev docs](https://www.fastmail.com/dev/)). API tokens do not list that scope (verified: [API tokens help](https://www.fastmail.help/hc/en-us/articles/5254602856719-API-tokens)). OAuth clients are "registered manually by contact with Fastmail developers" (verified: dev docs). So mejl can manage vacation only after registering an OAuth client (inferred).

## 4. How UI rules and custom Sieve coexist

### 4.1 One script, generated blocks plus insertion points

Fastmail's statement: "The Fastmail rules system generates a Sieve script behind the scenes, and the advanced rule editing functionality allows you to write additional filtering actions to supplement your existing rules." And: "The Sieve editing screen has four unchangeable blocks with Sieve code automatically generated by us based on choices you make within the Settings screens, separated by four editable text areas" (verified: [Using Sieve scripts](https://www.fastmail.help/hc/en-us/articles/1500000280481-Using-Sieve-scripts-in-Fastmail)).

Placement advice from Fastmail: "In most cases, you will want spam filtering to be run on your messages, so you would usually place your code under the block that is generated for spam filtering," and only in limited cases above it (verified: same page).

A possible explanation for "four" (inferred, untested): the Filters & Rules screen "is organized into four sections: Blocked senders, Spam protection, Address actions, and Rules" (verified: [Filters & Rules help](https://www.fastmail.help/hc/en-us/articles/1500000278122-Filters-Rules)), and the help page may count one generated block per settings section.

Observed layout, assembled from a user's description of the editor (Reddit, date not confirmed), generated code samples, and the `SieveBlocks` field names (unverified):

| Order | Section | Editable | Likely `SieveBlocks` field |
| --- | --- | --- | --- |
| 1 | `require [...]` line | No | `sieveRequire` |
| 2 | "(Insert Sieve code here)" | Yes | `sieveAtStart` |
| 3 | `### Sieve generated for save-on-SMTP identities` | No | `sieveForBlocked` |
| 4 | `### Sieve generated for blocked senders` | No | `sieveForBlocked` |
| 5 | `### Sieve generated for disabled masked email addresses` | No | `sieveForBlocked` |
| 6 | `### Sieve generated for spam protection` | No | `sieveForBlocked` |
| 7 | `### MailFetch Implicit Keep`, `### Address rules`, `### Execute spam filing` | No | `sieveForBlocked` |
| 8 | "(Insert Sieve code here)" | Yes | `sieveAtMiddle` |
| 9 | `### Calculate rule actions` (one block per UI rule) | No | `sieveForRules` |
| 10 | `### Execute rule actions` (trash, spam, folders, calendar, vacation) | No | `sieveForRules` |
| 11 | "(Insert Sieve code here)" | Yes | `sieveAtEnd` |

Sources for this table: the editor description by user benzado in a [Reddit thread](https://www.reddit.com/r/fastmail/comments/1u73j8l/migrating_from_selfhosted_multidomain_family_setup/) (three insertion points, with save-on-SMTP and blocked senders before the middle one, and "Calculate rule actions" and "Execute rule actions" before the last one; the commenter calls the layout "abbreviated", so it may omit an area); a sample in [xtuc/sieve2workers](https://github.com/xtuc/sieve2workers/blob/main/web-editor/www/index.js); a user's copy of their script in [gr3enarr0w/fastmail-mcp-server](https://github.com/gr3enarr0w/fastmail-mcp-server/tree/main/sieve) (2025, partly edited by that user, so not pristine); the `SieveBlocks` fields in section 3.1. The mapping of rows to fields is (inferred).

**Conflict to resolve:** the help page says four editable areas. The observed sources show three. The help page may count something else, or may be out of date (unverified either way). mejl should describe target sections by position ("directly below spam protection", "after all rules"), never by number.

### 4.2 How the generated code works

The generated code is a variables pipeline, not a chain of `if ... { fileinto; stop; }` blocks (unverified: observed samples above; consistent across sources):

- Early blocks set flags in variables instead of acting. Spam protection does `set "spam" "Y"`. In one variant, a score of 10 or more is discarded right away. That matches the documented Aggressive setting, which "permanently deletes messages with a spam score of 10 or higher" (verified: [Filters & Rules help](https://www.fastmail.help/hc/en-us/articles/1500000278122-Filters-Rules)).
- An `### Execute spam filing` block files mail with `${spam}` set to `"Y"` into Junk and stops, before the middle insertion point (unverified: gr3enarr0w sample). So spam never reaches the middle or bottom custom sections (inferred).
- Spam protection is skipped when `${spam}` is already `"N"` or when `X-Spam-Known-Sender` starts with `yes` (unverified: gr3enarr0w sample). So custom code in the top section can exempt mail from spam filtering with `set "spam" "N";`. One published generator does exactly this for mailing lists (unverified: [mshroyer gist](https://gist.github.com/mshroyer/0747de73595ecf244852a61402da4435), which says to paste into the topmost insertion point).
- No source shows the compiled form of a UI rule. The only generated rule-like code found is for an alias's "deliver to folder" setting (an address action): `if allof( not string :is "${stop}" "Y", jmapquery text: { "deliveredTo" : "..." } . ) { if not string :is "${spam}" "Y" { implicit_keep_target :mailboxid "<id>" "INBOX.<name>"; } }` (unverified: [emaildiscussions post #2, 2024-07-22](https://emaildiscussions.com/showthread.php?p=636137#post636137)). The gr3enarr0w sample's rules block says "No user configured rules". That UI rules compile to the same `jmapquery` plus `${stop}` guard shape is (inferred) from this sample and from the `filter` field of the undocumented `Rule` object (section 5.4).
- In the same thread, an explicit `keep` at the end of custom code made alias folder delivery stop working, and removing it fixed it (unverified: [posts #4 and #6](https://emaildiscussions.com/showthread.php?p=636161#post636161)). The likely reason is that `implicit_keep_target` only changes the implicit keep (inferred; the [Cyrus docs](https://www.cyrusimap.org/dev/reference/extensions/sieve.html) say it "redirects the implicit keep"). mejl should never emit `keep`.
- "Execute rule actions" then reads `${skipinbox}`, `${read}`, `${flagged}`, `${deletetotrash}`, `${spam}`, `${hasmailbox}` and `${stop}`, and files with `fileinto :specialuse "\\Trash" "INBOX.Trash"`, `:specialuse "\\Junk" "INBOX.Spam"` or `:specialuse "\\Archive" "INBOX.Archive"` (unverified: gr3enarr0w sample).
- Mute and follow use `jmapquery` on `someInThreadHaveKeyword` with `$muted` and `$followed`, and `$notify` controls push notification (unverified: gr3enarr0w sample). The FAQ confirms `addflag "$notify";` triggers a push notification in the Fastmail app (verified: [Sieve FAQ](https://www.fastmail.help/hc/en-us/articles/360058753814-Sieve-frequently-asked-questions)).
- Calendar invitations are handled inside "Execute rule actions" with `processcalendar` (unverified: gr3enarr0w sample).
- The masked email block reads `if header :contains "Fastmail-MaskedEmail" " state=disabled" { addflag "\\Seen"; fileinto :specialuse "\\Trash" "INBOX.Trash"; stop; }` (unverified: two samples agree). This block sits after the top insertion point and before spam protection. So code in the top custom section runs for mail to disabled masks, and code in the middle and bottom sections does not (inferred). This answers an open question in `docs/fastmail-masked-email.md`, with the same caveat.

Consequences for custom code (inferred unless marked):

- Custom code in the middle section runs after spam is decided but before UI rules. A `stop` there stops UI rules too.
- Custom code can set the same variables (`spam`, `skipinbox`, `stop`) to cooperate with the pipeline. This is powerful and fragile, since the variable names are not documented and could change.
- Generated code seen so far files by mailbox id (`:mailboxid`), so it should survive folder renames (unverified: the alias sample above; the undocumented `Rule` object stores `fileIn` as a mailbox id plus `previousFileInName`). Hand-written `fileinto "INBOX.Name"` does not: "if you've renamed a folder, make sure to update your relevant Sieve scripts; otherwise, messages intended for the renamed folder will be filed into the Inbox" (verified: Sieve FAQ). mejl should emit `fileinto :mailboxid "<JMAP Mailbox id>" "INBOX.Name"`. If no mailbox has that id, "the fileinto action proceeds as it would without the :mailboxid argument", so the name is a fallback (verified: [RFC 9042 section 4](https://www.rfc-editor.org/rfc/rfc9042.html)). The IMAP OBJECTID extension that defines MAILBOXID "is intentionally defined to be compatible with the data model in [JMAP-MAIL]" (verified: [RFC 8474](https://www.rfc-editor.org/rfc/rfc8474.html)). That Fastmail's JMAP Mailbox ids equal the ids its Sieve accepts is still (inferred).
- `discard` bypasses Fastmail's backups: "It is possible to create a rule which permanently discards mail without the protection of our backup system" (verified: [Using Sieve scripts](https://www.fastmail.help/hc/en-us/articles/1500000280481-Using-Sieve-scripts-in-Fastmail)), and "Any mail deleted via discard is gone without the possibility of restoration" (verified: [How to use Sieve](https://www.fastmail.help/hc/en-us/articles/360060591373-How-to-use-Sieve)). The delivery process page also says the Sieve script "is checked for matching Discard rules" before the spam check (verified: [The email delivery process](https://www.fastmail.help/hc/en-us/articles/1500000278262-The-email-delivery-process)). mejl should not emit `discard` by default.
- Use `redirect :copy` for forwarding to avoid copies landing in the Inbox unexpectedly (verified: Sieve FAQ).
- Sieve runs after the SMTP session, so `reject` cannot refuse mail before DATA (verified: Sieve FAQ).
- Sieve only filters incoming mail, not sent mail (verified: Sieve FAQ).

### 4.3 Require line and supported extensions

The help page shows this require line "at the top of your custom Sieve script" (verified: [Using Sieve scripts](https://www.fastmail.help/hc/en-us/articles/1500000280481-Using-Sieve-scripts-in-Fastmail)):

```sieve
require ["fileinto", "reject", "vacation", "envelope", "body", "relational", "regex", "subaddress", "copy", "mailbox", "mboxmetadata", "servermetadata", "date", "index", "comparator-i;ascii-numeric", "variables", "imap4flags", "editheader", "duplicate", "vacation-seconds", "fcc", "vnd.cyrus.jmapquery", "vnd.cyrus.log", "mailboxid", "special-use", "vnd.cyrus.snooze", "vnd.cyrus.imip", "vnd.cyrus.implicit_keep_target"];
```

Observed variants differ: an older one adds `"notify"` and lacks the last two entries ([paritcher](https://paritcher.com/FMsieve.html), which embeds [this gist](https://gist.github.com/yparitcher/61d1688eed101e167b5f6ed4197872eb)), and a 2025 one has `"processcalendar"` instead of `"vnd.cyrus.imip"` ([gr3enarr0w block1](https://github.com/gr3enarr0w/fastmail-mcp-server/blob/main/sieve/block1-calculate-rule-actions.txt)) (unverified). `processcalendar` is the capability name of [RFC 9671](https://www.rfc-editor.org/rfc/rfc9671.html), which Cyrus lists as supported (verified: [Cyrus RFC support](https://www.cyrusimap.org/imap/rfc-support.html)). Nothing shows whether the help page line or the gr3enarr0w line is newer, so neither should be called "current" (inferred). Because the require line is generated, mejl's custom code must not add its own `require` (inferred: Sieve requires `require` at the top of a script, per [RFC 5228](https://www.rfc-editor.org/rfc/rfc5228.html), and custom sections sit below it). mejl should restrict itself to extensions that appear in both the help page line and the 2025 sample line.

| Extension the task asked about | Status at Fastmail |
| --- | --- |
| `fileinto` | Supported (verified: require line) |
| `imap4flags` | Supported (verified: require line, FAQ) |
| `vacation`, `vacation-seconds` | Supported (verified: require line) |
| `variables` | Supported (verified: require line) |
| `regex` | Supported. The help page links [draft-ietf-sieve-regex-01](https://tools.ietf.org/html/draft-ietf-sieve-regex-01) (verified: Using Sieve scripts page source). UI regex rules use POSIX ERE: "POSIX EREs are supported, but not PCREs" (verified: [Rules using regular expressions](https://www.fastmail.help/hc/en-us/articles/360060591193-Rules-using-regular-expressions)). Cyrus's `:regex` allows "POSIX extended regular expression matching" (verified: [Cyrus Sieve extensions](https://www.cyrusimap.org/dev/reference/extensions/sieve.html)). That Fastmail's build behaves the same is (inferred). |
| `body` | Supported (verified: require line) |
| `date`, `index` | Supported (verified: require line) |
| `mailbox`, `mailboxid`, `special-use` | Supported (verified: require line) |
| `duplicate` | Supported (verified: require line) |
| `editheader` | Supported (verified: require line, FAQ) |
| `copy`, `subaddress`, `relational`, `envelope`, `reject`, `fcc`, `mboxmetadata`, `servermetadata` | Supported (verified: require line) |
| snooze | Supported as `vnd.cyrus.snooze` (verified: require line). Syntax: `snooze` with `:mailbox`, `:mailboxid`, `:specialuse`, `:addflags`, `:removeflags`, `:weekdays`, `:tzid` and a list of times of day (verified: [Cyrus Sieve extensions](https://www.cyrusimap.org/dev/reference/extensions/sieve.html), which also notes the snooze draft "is long since expired"). |
| `processcalendar` (RFC 9671) | In the 2025 gr3enarr0w require line, not in the help page line (unverified). Generated "Execute rule actions" code calls it (unverified: sample). mejl should not emit it. |
| `notify` (enotify) | Not in the current verified line. It appears in one older variant (unverified). Treat as unsupported. Use `addflag "$notify"` for push notifications instead (verified: FAQ). |
| `vnd.cyrus.jmapquery` | Supported (verified: require line). Takes a JMAP Email filter as JSON (verified: Cyrus docs). |
| `vnd.cyrus.log`, `vnd.cyrus.implicit_keep_target`, `vnd.cyrus.imip` | In the help page line (verified). Semantics per Cyrus docs (verified for `log` and `implicit_keep_target`; `imip` not described on that page). Cyrus says `vnd.cyrus.log` "is intentionally not advertised to ManageSieve clients" (verified: same page). |
| `include`, `spamtest`, `environment`, `ihave`, `foreverypart`/`mime`, `extlists` | Not in the require line. Treat as unsupported (inferred). |

### 4.4 Would uploading a whole script clobber UI rules?

- Today there is no supported way to upload anything, so the question is about future APIs and the undocumented one.
- With the undocumented `SieveBlocks/set`, tools write individual sections. Sieve-Sync writes only the three custom sections and leaves the generated ones alone (unverified as Fastmail behavior; confirmed in its `sieve_sync/sieve.py`, which sets only `sieveAtStart`, `sieveAtMiddle` and `sieveAtEnd`).
- With RFC 9661, a client uploads a whole script and activates it. The terva research doc gives the same warning: "Whole-script replacement can clobber UI-managed rules" (unverified: [terva](https://github.com/terva-sh/terva-ext-jmap-mail/blob/main/docs/filters-research.md)). If Fastmail exposed RFC 9661 over the same storage, replacing the active script would overwrite the generated blocks, and the next save in the web UI would likely regenerate the script and drop the client's version (inferred). How Fastmail would model this is unknown. It might expose only custom sections, or a separate non-generated script.
- Fastmail's rules are stored as structured objects (`Rule/*`) and compiled into Sieve (unverified: section 3.1). So the Sieve text is an output, not the source of truth for UI rules (inferred). A client that edits the generated text loses those edits on the next regeneration (inferred).

**Rule for mejl:** never replace a whole Fastmail script. Only ever own one marked region inside one custom section.

## 5. Fastmail's rule model in the UI

### 5.1 Processing order

"First, we check if the sender is blocked. After that, we check if the message should be marked as spam according to your settings. Next, we check if you have any actions set up for this particular address. Finally, we check how the message should be filtered according to your rules." (verified: [Filters & Rules help](https://www.fastmail.help/hc/en-us/articles/1500000278122-Filters-Rules)). This matches the generated blocks in section 4.1 (inferred).

The [email delivery process](https://www.fastmail.help/hc/en-us/articles/1500000278262-The-email-delivery-process) page adds detail (verified):

- At local delivery Fastmail adds `X-Mail-from` (SMTP MAIL FROM), `X-Delivered-to` (SMTP RCPT TO) and `X-Resolved-to` (final address after alias rewriting) headers, then runs the content filter.
- The Sieve script is checked for "matching Discard rules" before the spam check. Spam checking adds `X-Spam-score` and `X-Spam-hits`.
- "After that, the user's 'Sieve scripts' for Forwarding and Organize rules are run on the email, in addition to any custom Sieve added after spam filtering."
- "The 'envelope' value seen by the Sieve script is the resultant target value, not the original 'MAIL FROM' or 'RCPT TO' parts."
- Marking a message "Not spam" later re-checks "mail rules, email address actions, or custom Sieve" and applies the move, pin, mark-read and send-copy actions (verified: [Improving spam protection](https://www.fastmail.help/hc/en-us/articles/1500000278142-Improving-spam-protection)).

### 5.2 Conditions

All from the [Filters & Rules help](https://www.fastmail.help/hc/en-us/articles/1500000278122-Filters-Rules) (verified) unless marked:

- Combinator: **all** (and) or **any** (or).
- Common fields: From, To/Cc/Bcc, Subject, Body, Anywhere (body, attachments and the headers To, Cc, Bcc, Subject, Message-Id, References, In-Reply-To, X-Delivered-To).
- `NOT` before a value negates it.
- Up to 50 conditions per rule, up to 1000 rules per user.
- Rules can be created in Settings (with a preview of matching mail and an option to apply to existing mail), from a message ("Add rule from message...", no preview), or from a search ("Create rule" next to the search bar, conditions equal the search).
- "No-preview rules (regular expressions supported)": globs (`*`, `?`, anchored), POSIX ERE, "does not match" variants, matching on specified headers. No preview and no apply-to-existing. Standard and no-preview rules cannot be converted into each other (verified: same page and [Rules using regular expressions](https://www.fastmail.help/hc/en-us/articles/360060591193-Rules-using-regular-expressions)).
- Muted state is a rule condition: "You have muted the conversation" (verified: [Mute conversations](https://www.fastmail.help/hc/en-us/articles/7788624824463-Mute-conversations)). Replies to a muted conversation "go straight to your Archive folder", and for muted conversations a rule's "Notify me" action "will be ignored" (verified: same page). Mute is not available on Basic plans (verified: same page).
- "Rules are applied in the order they are listed, and they can be reordered by dragging and dropping." (verified).
- "Rules can only be created in our web interface, but they will still function if you use a mail client." (verified).

### 5.3 Actions and their Sieve shape

| UI action | Verified meaning (Filters & Rules help) | Likely Sieve shape (unverified, from samples) |
| --- | --- | --- |
| Mark read | Marks matching mail read | `set "read" "Y"`, later `addflag "\\Seen"` |
| Pin | Pins the message | `set "flagged" "Y"`, later `addflag "\\Flagged"` |
| Notify me | Sends a notification | `addflag "$notify"` |
| Move to | Moves out of Inbox into a folder (or label) | `implicit_keep_target :mailboxid` or `fileinto :mailboxid`, plus `set "skipinbox" "Y"` |
| Snooze until | Snoozes, reappears in Inbox later | `snooze` (Cyrus) |
| Send a copy to | Copies to external addresses | `redirect :copy` |
| Delete to trash | Goes to Trash | `set "deletetotrash" "Y"`, later `fileinto :specialuse "\\Trash"` |
| Send to spam | Goes to Spam | `set "spam" "Y"`, later `fileinto :specialuse "\\Junk"` |
| (Continue or stop) | Not described in help | `set "stop" "Y"` guard on later rules (unverified) |

### 5.4 The undocumented `Rule` object

For reference only, so the team understands what a Fastmail rule holds. Field list from [dvcrn/fastmail-rules-cli types.go](https://github.com/dvcrn/fastmail-rules-cli/blob/main/jmap/types.go) (unverified): `id`, `name`, `conditions` (each with `lookHow`, `lookFor`, `lookIn`), `combinator`, `search` (search-query string, for example `from:test`), `filter` (a JMAP Email filter object, for example `{"from": "test"}`), `markRead`, `markFlagged`, `showNotification`, `fileIn` (mailbox id), `skipInbox`, `snoozeUntil`, `redirectTo`, `discard`, `markSpam`, `stop`, `isEnabled`, `sortOrder`, `previousFileInName`, `created`, `updated`. The `filter` field matches the `jmapquery` tests in the generated Sieve (inferred). mejl's own rule model can borrow this shape, because it already maps one to one onto Fastmail's UI concepts (inferred).

### 5.5 Block and allow lists, quick rules

- **Blocked senders:** mail from blocked addresses or domains goes to Trash. Blocking an address deletes the matching contact. Block from a message via Actions, "Block [sender]", with "Block all [@domain]" except for major providers (verified: [Filters & Rules help](https://www.fastmail.help/hc/en-us/articles/1500000278122-Filters-Rules)). There is no documented API for the block list (verified absence: no scope or method in the dev docs).
- **Allow list:** contacts. "Emails that come from senders in your contact list will not get flagged as spam." A `*@domain.tld` contact covers a domain, and shared contact groups count. Messages from known senders carry `X-Spam-known-sender: yes` (verified: [Improving spam protection](https://www.fastmail.help/hc/en-us/articles/1500000278142-Improving-spam-protection)). The header is not a plain yes or no. Fastmail checks contacts against the SMTP MAIL FROM and the From header, and a contact can still yield values such as `no ("Email failed DMARC policy for domain"), in-addressbook`. Fastmail says to "only rely on the field starting with 'yes', containing 'in-addressbook' or containing one of the contact group UUIDs" (verified: [The email delivery process](https://www.fastmail.help/hc/en-us/articles/1500000278262-The-email-delivery-process)). The generated spam block skips spam checks when the header matches `yes*` (unverified: samples). Contacts are writable through documented JMAP (`urn:ietf:params:jmap:contacts`, verified: dev docs). So mejl can implement "allow sender" with `ContactCard/set` (inferred), but it does not override a DMARC failure (inferred from the header values above).
- **Screening:** Fastmail's screener is a plain rule, "Sender is not a contact", moving mail to a folder (verified: [Screening emails](https://www.fastmail.help/hc/en-us/articles/8661280554511-Screening-emails)).
- **Address actions:** per-address discard, redirect or move, configured on aliases, applied before rules (verified: Filters & Rules help).
- **VIP contacts:** a contact group used for notifications and the `fromin:vips` search. Not described as affecting filtering (verified: [VIP contacts](https://www.fastmail.help/hc/en-us/articles/17254978619791-VIP-contacts)).
- **"Organize" features and "Mail from this sender" quick rules:** I found no Fastmail help page describing an "Organize" menu or a "Mail from this sender" quick rule. The word appears only as a generic name: the delivery process page speaks of "Forwarding and Organize rules" (verified: [The email delivery process](https://www.fastmail.help/hc/en-us/articles/1500000278262-The-email-delivery-process)). The sender sidebar offers call, message, compose, invite, add to contacts or edit, and VIP, but no rule or block action (verified: [Sidebar](https://www.fastmail.help/hc/en-us/articles/1500007394662-Sidebar-info-at-a-glance)). Grouped mail view is display only: "Groups only change how your message list is laid out" (verified: [Grouped mail view](https://www.fastmail.help/hc/en-us/articles/17915089518863-Grouped-mail-view)). The quick-rule entry points that do exist are "Add rule from message..." and "Create rule" from a search (verified). Whether newer UI has more is (unverified).

## 6. Sieve tooling in Rust

### 6.1 sieve-rs (Stalwart)

| Item | Value |
| --- | --- |
| Crate | [`sieve-rs`](https://crates.io/crates/sieve-rs), import name `sieve` |
| Version | 1.0.2, released 2026-09-22 (verified: crates.io API, 2026-10-23) |
| License | `AGPL-3.0-only` on crates.io for every release from 0.1.0 (2022) to 1.0.2 (verified: crates.io versions API, 2026-10-23). The README instead says "either version 3 of the License, or (at your option) any later version", and GitHub detects no license file. This mismatch should be read as AGPL-3.0-only until clarified. The README offers a commercial license via licensing@stalw.art (verified: [README](https://github.com/stalwartlabs/sieve)) |
| Repo | [stalwartlabs/sieve](https://github.com/stalwartlabs/sieve), last push 2026-10-04 (verified: GitHub API) |
| API | `Compiler::compile` to bytecode, `Runtime::filter` or `filter_parsed` (takes a `mail_parser` message), `instance.run(&mut handler)` with a `Handler` that receives `SieveAction`s such as `Keep`, `FileInto`, `Discard`, `Reject`, `SendMessage`, `Notify`. External lookups return `Status::Pending` and resume (verified: README) |
| Extensions | RFC 5228 plus about 25 extension RFCs including variables, imap4flags, body, date, editheader, duplicate, mailbox, special-use, mailboxid, fcc, regex draft 01, and `vnd.stalwart.*` (verified: README). No Cyrus vendor extensions (verified: grep of the source for `jmapquery`, `snooze`, `implicit_keep_target`, `processcalendar` found nothing, 2026-10-23) |

**Local experiment** (verified: local experiment, sieve-rs 1.0.2, 2026-10-23; the fact-checker read the throwaway crate's source and reran it with identical results). I compiled and ran snippets in a throwaway crate. The key calls were:

```rust
let c = Compiler::new().with_no_capability_check(nocap);
c.compile(src.as_bytes())
```

Results:

| Input | Result |
| --- | --- |
| Fastmail's full require line, including `vnd.cyrus.*` | Compiles, with and without capability checking |
| `require ["vnd.nonsense.thing"];` | Compiles |
| `if frobnicate "x" { stop; }` (a made-up test) | Compiles |
| `jmapquery text: {...} .` test | Compiles. At runtime: `Script executed invalid instruction "jmapquery"` |
| `snooze :mailboxid "M1" :addflags "$x" ["09:00"];` | Compile error: `UnexpectedToken { expected: "command", found: "snooze" }` |
| `implicit_keep_target :mailboxid "M1" "INBOX.test";` | Compile error, same kind |
| Standard script with `fileinto :specialuse`, `addflag`, `header :value "ge"` on `X-Spam-score` | Compiles and runs. Three test messages produced `FileInto INBOX.Archive` with `\Seen`, `FileInto INBOX.Spam`, and `Keep` as expected |

What this means for mejl (inferred):

- sieve-rs is a grammar checker and a good dry-run engine for **standard** Sieve. It is not a validator for Fastmail. It accepts some invalid scripts and rejects some valid generated ones.
- mejl needs its own extension allowlist (the verified require line in section 4.3) and must check `require` strings and command names itself.
- mejl can dry-run its **own** rendered block locally against cached mail, as long as that block avoids Cyrus vendor extensions. It cannot faithfully dry-run Fastmail's full generated script.
- Fastmail adds `X-Spam-score`, `X-Spam-known-sender`, `X-Delivered-to`, `X-Mail-from` and `X-Resolved-to` to the message during delivery, and its example shows them in a delivered message's headers (verified: [The email delivery process](https://www.fastmail.help/hc/en-us/articles/1500000278262-The-email-delivery-process)). So tests on them can be evaluated against stored mail (inferred).
- Envelope tests can be approximated from `X-Mail-from` and `X-Resolved-to`, since Sieve's envelope at Fastmail is "the resultant target value" (inferred from the delivery process page). `duplicate` needs server-side state mejl does not have (inferred).
- AGPL-3.0-only is a licensing decision. If mejl is not AGPL, linking sieve-rs requires a commercial license or an AGPL relicense of mejl (inferred; legal review needed). See also `docs/rust-crates.md`.

### 6.2 Other crates and tools

| Tool | License | Status | Fit |
| --- | --- | --- | --- |
| [`mailrs-sieve-core`](https://crates.io/crates/mailrs-sieve-core) 0.3.0 | Apache-2.0 OR MIT | 211 downloads, 2026-08-04. Describes itself as a "Native RFC 5228 Sieve interpreter" with "no AGPL dependencies", still working toward "parity with `sieve-rs`" (verified: crates.io) | Permissive alternative, unproven (unverified quality) |
| [`sieve-kit`](https://crates.io/crates/sieve-kit) 0.2.1 | MIT OR Apache-2.0 | 223 downloads, 2026-09-12. "Structured mail filter rules engine with RFC 5228 Sieve semantics" (verified: crates.io) | Possibly close to mejl's own rule model, unproven (unverified) |
| [`sieve-form`](https://crates.io/crates/sieve-form) 0.2.1 | MIT | 63 downloads (verified: crates.io) | Unproven |
| [`sieve-generator`](https://crates.io/crates/sieve-generator) 0.3.2 | MIT | 2022, stale (verified: crates.io) | Reference for rendering only |
| [`sieve-parser`](https://crates.io/crates/sieve-parser) 0.1.2 | AGPL-3.0-or-later | 2023, no repo listed (verified: crates.io) | Avoid |
| [`io-managesieve`](https://crates.io/crates/io-managesieve) 0.2.0 | MIT OR Apache-2.0 | Pimalaya, 2026-09-29 (verified: crates.io) | Useless for Fastmail (no ManageSieve) |
| [tree-sitter-sieve](https://github.com/TLATER/tree-sitter-sieve) | MIT | "A parser and formatter", 1 star, not on crates.io (verified: GitHub, crates.io) | Possible syntax highlighting in an editor view |
| [aRustyDev/sieve-language-server](https://github.com/aRustyDev/sieve-language-server) | None detected by GitHub | "Sieve Language Server Protocol implementation in rust", last push 2025-12-24 (verified: GitHub API) | Unproven; no license means no reuse rights |

I found no mature Sieve formatter. mejl's rendered block should be formatted by mejl's own renderer, which makes formatting a non-issue for generated code (inferred).

## 7. Prior art

| Client | Sieve support | UX worth copying |
| --- | --- | --- |
| [Roundcube managesieve plugin](https://github.com/roundcube/roundcubemail/tree/master/plugins/managesieve) | ManageSieve. Filter sets, form-based rules, raw editor (`managesieve_raw_editor`), separate vacation and forward screens (verified: [config.inc.php.dist](https://github.com/roundcube/roundcubemail/blob/master/plugins/managesieve/config.inc.php.dist)) | Round-trips its own rules by writing `# rule:[Name]` before each rule and disables rules as `if false # ...` (verified: [rcube_sieve_script.php](https://github.com/roundcube/roundcubemail/blob/master/plugins/managesieve/lib/Roundcube/rcube_sieve_script.php)). Can hide reserved scripts. |
| [thsmi Sieve Editor](https://github.com/thsmi/sieve) | ManageSieve plus a graphical editor. Was a Thunderbird add-on, now a standalone app. Last release 0.6.1 on 2021-11-08, repo pushed 2026-07-20 (verified: GitHub API) | Graphical block editor with a raw view. Its [Sieve reference](https://thsmi.github.io/sieve-reference/en/) is linked from Fastmail's help. |
| Thunderbird for Android (K-9) | None. The "Sieve support" request [#1410](https://github.com/thunderbird/thunderbird-android/issues/1410) is still open (verified: GitHub API, 2026-10-23) | None |
| [himalaya](https://github.com/pimalaya/himalaya) 2.2.1 (Rust) | ManageSieve behind a `sieve` feature (verified: crates.io features). The README shows `sieve list`, `check`, `put` and `activate`; the source tree `src/sieve/` also has `capability`, `get`, `deactivate`, `rename`, `delete` and `raw` (verified: GitHub API, 2026-10-23) | CLI shape for script management |
| [meli](https://git.meli-email.org/meli/meli) (Rust TUI) | ManageSieve code in `melib/src/imap/managesieve.rs` and a `managesieve` subcommand in `meli/src/subcommands.rs` (verified: GitHub code search of the meli mirror; behavior not tested) | Shell, not an in-TUI editor (unverified) |
| aerc | No built-in Sieve found (unverified: its README does not mention Sieve) | None |
| [dvcrn/fastmail-rules-cli](https://github.com/dvcrn/fastmail-rules-cli) | Fastmail private API, browser cookie auth | Edits sections in `$EDITOR`, validates the assembled script before saving |
| [Fastmail-Sieve-Sync](https://github.com/fuyu0425/Fastmail-Sieve-Sync) | Fastmail private API, cookie auth | Owns only text between `#### Managed by Sieve Sync Start ####` and `... End ####` markers in each custom section |
| [shellguard/fastmail-mcp](https://github.com/shellguard/fastmail-mcp) | RFC 9661 tools that "will return clear errors until Fastmail expands their token scope model" (verified: README) | Its `SKILLS.md` tells the agent to "write the rules for them and instruct them to paste the rules into Fastmail's web UI" (verified: repo file) |

Lessons (inferred):

- Every tool that edits Fastmail's script safely owns a **marked region** and leaves the rest alone. That pattern is the core of option A below.
- Roundcube shows that a structured rule model can live inside Sieve text using comment markers, so mejl can parse back what it wrote.
- Good editors validate before saving and show the exact line of the first error. RFC 9661 says the `invalidSieve` description SHOULD give the line number too.

## 8. Design options for mejl

Ranked by feasibility today.

### Option A: mejl-owned rules, rendered into one marked block, pasted by the user (recommended)

How it works (inferred design):

1. mejl stores rules in its own model, close to Fastmail's `Rule` shape: name, enabled, combinator, conditions as a JMAP Email filter, actions (move, mark read, pin, notify, snooze, copy to, trash, spam, stop). Store it locally. To sync it between machines, use Fastmail Files over WebDAV with an app password, which Fastmail documents as supported (verified: [dev docs](https://www.fastmail.com/dev/) say Files "you can access via WebDAV"; [Server names and ports](https://www.fastmail.help/hc/en-us/articles/1500000278342-Server-names-and-ports) gives `https://webdav.fastmail.com/` and an app password "with access set to Files (WebDAV)"). That adds a second credential besides the JMAP token or OAuth grant (inferred). Do not use Fastmail Notes for this: the only Notes capability seen is `https://www.fastmail.com/dev/notes`, and it appeared only in a private web-session sample (unverified).
2. mejl renders one Sieve block, framed by fixed markers, for one target section (default: the middle section, below spam protection). Example:

   ```sieve
   # >>> mejl managed block v1 (do not edit by hand) >>>
   # mejl:rule id=r_7f3a name="Newsletters" enabled=1
   if address :domain :is "from" "news.example.com" {
     addflag "\\Seen";
     fileinto :mailboxid "M3b2c" "INBOX.Newsletters";
   }
   # mejl:rule id=r_91c0 name="Old promo" enabled=0
   if false # address :domain :is "from" "promo.example.com"
   {
     fileinto :specialuse "\\Trash" "INBOX.Trash";
   }
   # <<< mejl managed block v1 sha256=<hash of body> <<<
   ```

   Rules render **without** `stop` by default. An explicit `fileinto` already cancels the implicit keep, so the message still leaves the Inbox (verified: [RFC 5228 section 2.10.2](https://www.rfc-editor.org/rfc/rfc5228.html)). The cost is that later UI rules can still act on the same message, for example filing a second copy (inferred). `stop` should be an explicit per-rule option with a warning: in the middle section it would also skip Fastmail's "Calculate rule actions" and "Execute rule actions" blocks, which in the observed sample hold mute and follow handling, `$notify`, calendar invitation processing and the vacation response (inferred from the gr3enarr0w sample in section 4.2). The example avoids the `${stop}` guard that Fastmail's generated code uses, because those variable names are undocumented. It also never emits `keep`, since an explicit `keep` appears to defeat Fastmail's alias folder delivery (section 4.2).
3. mejl checks the block against the extensions shared by the help page require line and the 2025 sample line (section 4.3), compiles it with a Sieve parser, and dry-runs it against recent cached mail to show "these 14 messages would move".
4. The user presses a key. mejl copies the block to the clipboard (OSC 52 in the terminal) and opens `https://app.fastmail.com/settings/filters`. The user replaces the old block in the chosen section and saves. Fastmail validates on save (unverified: dvcrn's CLI calls `Validate/sieve` before saving, which suggests the web UI does the same).
5. To detect drift, mejl can ask the user to paste the section back. mejl then checks the markers and the hash.

Documented JMAP features around it (inferred unless marked):

- **Preview matches** with `Email/query` using the same filter mejl renders. This mirrors Fastmail's own rule preview.
- **Apply to existing mail** with `Email/set` (move, keywords). Fastmail offers the same option when creating rules (verified: Filters & Rules help).
- **Allow sender** with `ContactCard/set`, which feeds `X-Spam-Known-Sender` (verified: spam help, contacts scope in dev docs). A DMARC failure can still mark the sender as not known (section 5.5).
- **Disable a Masked Email address** with `MaskedEmail/set` `state: disabled`, which sends its mail to Trash through the generated block (verified: [dev docs](https://www.fastmail.com/dev/) for the state meaning).
- **Mailbox ids** from `Mailbox/get` for `fileinto :mailboxid`, so rules survive renames.

Limits:

- Not seamless. One manual paste per change.
- mejl cannot read the server script, so it cannot know if the user changed or removed the block.
- mejl rules run in a different place than UI rules. Mixing both needs care with `stop` and ordering. mejl should explain where its block runs relative to UI rules.
- "Block sender" stays a web UI action. mejl can render an equivalent Sieve rule in its block, but that is not Fastmail's block list.
- mejl must not depend on Fastmail's pipeline variables (`spam`, `skipinbox`, `stop` and others). They are undocumented and could change without notice (inferred).
- A mejl rule with `stop` in the middle section skips vacation replies and calendar processing for matched mail (inferred, see above).

### Option B: JSON export and import round trip (needs a format check first)

How it works (inferred design):

1. The user exports rules (and, per the Sieve help, Sieve) as JSON from Settings, Filters & Rules.
2. mejl reads the file, shows the UI rules read-only in its rules view, and lets the user edit or add rules in mejl.
3. mejl writes a new JSON file. The user imports it.

Unknowns that decide whether this works: the JSON schema, whether import replaces, merges or duplicates, whether custom Sieve sections are included and restored, and whether import touches blocked senders (all unverified). Test only on a trial account, never the real mailbox.

If it works, this is the closest to seamless that stays within public behavior: mejl could show and edit real UI rules, not only its own block. It is still two manual steps per change.

### Option C: RFC 9661 client behind capability detection (future-proofing)

How it works (inferred design):

1. On every session fetch, mejl checks `capabilities` and `accountCapabilities` for `urn:ietf:params:jmap:sieve`.
2. If present, mejl lists scripts, downloads the active one, validates new content with `SieveScript/validate`, and uploads with `SieveScript/set` and `onSuccessActivateScript`, always keeping a backup of the previous script.
3. On Fastmail today this path stays dormant. On Stalwart or Cyrus it works now. Stalwart implements RFC 9661 and RFC 5804 (verified: [Stalwart's RFC list](https://stalw.art/docs/development/rfcs/)), so it can be the test server.

Caveat: if Fastmail later exposes RFC 9661, the script model may not match its generated blocks (section 4.4). mejl would still only edit its marked region inside whatever Fastmail exposes.

This option costs real code for zero Fastmail benefit today. Under YAGNI it should wait until Fastmail ships the capability, but the rule model and renderer from option A should be written so this is a thin adapter later.

### Rejected: the undocumented web-client API

It gives true two-way sync of rules and Sieve sections, which is what "seamless" means. It is rejected because Fastmail's policy forbids "Using unpublished APIs", it needs the user's web session cookie and a sudo flow, and it can break at any time (section 3.1).

### Comparison

| | A: marked block, paste | B: JSON round trip | C: RFC 9661 | Rejected: private API |
| --- | --- | --- | --- | --- |
| Works on Fastmail today | Yes | Unknown | No | Yes |
| Allowed by Fastmail policy | Yes | Yes (inferred) | Yes | No |
| Reads UI rules | No | Yes, if format allows | Maybe, later | Yes |
| Writes without user steps | No | No | Yes, later | Yes |
| Effort | Medium | Medium, after a spike | Medium | Medium |
| Risk | Drift, user error on paste | Import side effects | Model mismatch | Account and legal risk |

### What is not possible today

All verified as absences in Fastmail's public docs, or inferred where marked:

- Reading the user's Sieve script or UI rules through a public API.
- Writing Sieve, rules, blocked senders, spam settings or address actions through a public API.
- ManageSieve access.
- Validating a script against Fastmail's real Sieve engine through a public API. The Sieve tester web page can do it by hand.
- Managing vacation responses with an API token. OAuth with a registered client can (verified: dev docs and API tokens page).
- Running Fastmail's full generated script faithfully on the client. It depends on Cyrus extensions and server state (inferred).

## 9. Verification procedure

Run these with the user's own API token. Create the token in Settings, Privacy & Security, Connected apps & API tokens, Manage API tokens, choose Type **JMAP**, enable **every scope** for the probe, then delete it afterwards (verified: [dev docs](https://www.fastmail.com/dev/) and [API tokens help](https://www.fastmail.help/hc/en-us/articles/5254602856719-API-tokens)). API tokens are not available on Basic plans (verified: API tokens help). Do not paste the session output into shared places: it contains account ids and the username. Use `jq`.

### Step 1: inspect the session

```sh
export FM_TOKEN='paste-token-here'

SESSION=$(curl -sS -H "Authorization: Bearer $FM_TOKEN" \
  https://api.fastmail.com/jmap/session)

# Session-level capabilities
echo "$SESSION" | jq '.capabilities | keys'

# Account-level capabilities, per account
echo "$SESSION" | jq '.accounts | to_entries[]
  | {id: .key, name: .value.name, caps: (.value.accountCapabilities | keys)}'

# Which account is primary for each capability
echo "$SESSION" | jq '.primaryAccounts'

# The two values that matter here (null means absent)
echo "$SESSION" | jq '{
  sieve: .capabilities["urn:ietf:params:jmap:sieve"],
  rules: .capabilities["https://www.fastmail.com/dev/rules"]
}'
```

Expected today: no `urn:ietf:params:jmap:sieve` and no `https://www.fastmail.com/dev/rules` (unverified expectation, from section 1). Record the full `keys` lists in the open questions below. Without a token, the session URL returns HTTP 401 (verified: local probe, 2026-10-23).

### Step 2: ask the API directly

RFC 8620 says a request that uses an unsupported capability fails with `urn:ietf:params:jmap:error:unknownCapability`, shown with status 400 in the RFC example (verified: [RFC 8620 section 3.6.1](https://www.rfc-editor.org/rfc/rfc8620.html)). `Core/echo` changes nothing.

```sh
API=$(echo "$SESSION" | jq -r .apiUrl)

curl -sS -i "$API" \
  -H "Authorization: Bearer $FM_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{
    "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:sieve"],
    "methodCalls": [["Core/echo", {"probe": "sieve"}, "0"]]
  }'
```

Expected today: HTTP 400 with `"type": "urn:ietf:params:jmap:error:unknownCapability"`. A 403 instead would suggest the capability exists but the token lacks a scope (inferred).

### Step 3: only if step 1 or 2 shows sieve support (read-only)

```sh
ACC=$(echo "$SESSION" | jq -r '.primaryAccounts["urn:ietf:params:jmap:sieve"]')

# Extensions and limits
echo "$SESSION" | jq --arg a "$ACC" \
  '.accounts[$a].accountCapabilities["urn:ietf:params:jmap:sieve"]'

# List scripts
curl -sS "$API" \
  -H "Authorization: Bearer $FM_TOKEN" -H 'Content-Type: application/json' \
  -d '{
    "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:sieve"],
    "methodCalls": [["SieveScript/get", {"accountId": "'"$ACC"'", "ids": null}, "0"]]
  }' | jq .

# Download the active script (set BLOB from the blobId of the script with isActive true)
BLOB='paste-blobId-here'
DL=$(echo "$SESSION" | jq -r .downloadUrl)
URL=$(printf '%s' "$DL" | sed -e "s/{accountId}/$ACC/" -e "s/{blobId}/$BLOB/" \
  -e "s/{name}/active.sieve/" -e "s#{type}#application%2Fsieve#")
curl -sS -H "Authorization: Bearer $FM_TOKEN" "$URL" -o active.sieve

# Validate a test script without changing anything
printf 'require ["fileinto"];\nif header :contains "subject" "mejl-probe" { fileinto "INBOX"; }\n' > probe.sieve
UP=$(echo "$SESSION" | jq -r .uploadUrl | sed "s/{accountId}/$ACC/")
NEWBLOB=$(curl -sS -H "Authorization: Bearer $FM_TOKEN" \
  -H 'Content-Type: application/sieve' --data-binary @probe.sieve "$UP" | jq -r .blobId)
curl -sS "$API" \
  -H "Authorization: Bearer $FM_TOKEN" -H 'Content-Type: application/json' \
  -d '{
    "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:sieve"],
    "methodCalls": [["SieveScript/validate", {"accountId": "'"$ACC"'", "blobId": "'"$NEWBLOB"'"}, "0"]]
  }' | jq .
```

Upload returns `accountId`, `blobId`, `type` and `size`, and the download URL template has `accountId`, `blobId`, `type` and `name` (verified: [RFC 8620 sections 6.1 and 6.2](https://www.rfc-editor.org/rfc/rfc8620.html)). Never run `SieveScript/set` against the real account. If a write test is ever needed, use a trial account.

Do not call `Rule/get`, `SieveBlocks/get` or any other method under `https://www.fastmail.com/dev/rules`, even read-only. That is using an unpublished API (section 3.1).

### Step 4: ManageSieve

Fastmail publishes no ManageSieve host, so test the IMAP host and the IANA port 4190. RFC 5804 also tells clients to look up a DNS SRV record with service "sieve" and proto "tcp" (verified: [RFC 5804 sections 1.8 and 6](https://www.rfc-editor.org/rfc/rfc5804.html)). Remember the wildcard DNS: a name resolving proves nothing.

```sh
# Is there an SRV record? (RFC 5804 defines _sieve._tcp)
dig +short SRV _sieve._tcp.fastmail.com
dig +short SRV _sieve._tcp.your-domain.example

# Is anything listening? -starttls sieve is supported by OpenSSL s_client
openssl s_client -connect imap.fastmail.com:4190 -starttls sieve -brief </dev/null

# If it connects, try listing with an app password (read from stdin, not argv)
sieve-connect --server imap.fastmail.com --port 4190 --nosrv \
  --user you@fastmail.com --passwordfd 0 --list
```

The `-starttls sieve` keyword is listed in the local `openssl-s_client` man page (verified: OpenSSL 3.6.5, rechecked by the fact-checker). The sieve-connect flags `--server`, `--port`, `--nosrv`, `--user`, `--passwordfd` and `--list` appear in its `sieve-connect.pre.pl` source and POD (verified: [sieve-connect source](https://github.com/philpennock/sieve-connect), rechecked). Expected today: connection timeout or refusal (verified twice from one network, section 2).

### Step 5: record and decide

- If steps 1 to 3 show RFC 9661: promote option C, and keep option A's marked-region rule.
- If not: build option A, and run the option B spike on a trial account.

## Open questions

| Question | How to resolve |
| --- | --- |
| Does the user's own token session advertise `urn:ietf:params:jmap:sieve` or `https://www.fastmail.com/dev/rules`? | Run steps 1 and 2 with an all-scopes token. Record both `keys` lists in this doc. |
| Does an OAuth session with all registrable scopes differ from an API-token session? | Ask Fastmail when registering an OAuth client (needed anyway for vacation). |
| Will Fastmail expose RFC 9661 or ManageSieve to customers, and how would it map onto generated blocks? | Ask through the [partners page](https://www.fastmail.com/company/partners/) when registering mejl's OAuth client. The RFC 9661 author works at Fastmail. Post on the JMAP list if useful. |
| Three or four custom Sieve sections? | The user opens Edit custom Sieve code and counts the "(Insert Sieve code here)" areas. Then fix section 4.1. |
| Exact current generated script and require line for this account | The user copies the script from the web UI into a local file (redact addresses). Compare with section 4. |
| JSON export format, and what import does (replace, merge, duplicate; includes Sieve sections; touches block list) | One redacted export from the real account (read-only action). Import tests only on a trial account. |
| Does the web UI reject a save when custom code is invalid, and does it show line numbers? | Try a deliberate syntax error in a trial account. |
| Do JMAP `Mailbox` ids equal the ids accepted by Sieve `:mailboxid` at Fastmail? | Compare a UI-generated `implicit_keep_target :mailboxid` value with `Mailbox/get` output. |
| Does Fastmail's Sieve build follow Cyrus's documented POSIX ERE `:regex` behavior? | Test in the [Sieve tester](https://app.fastmail.com/sievetester) with a PCRE-only pattern. |
| Can Basic-plan users use mejl at all? They get no API tokens and no IMAP. Does OAuth JMAP access work for them? | Ask Fastmail when registering the OAuth client. |
| Does any newer Fastmail UI offer "Organize" or "Mail from this sender" quick rules? | Check the current web UI and app. Nothing in the help center as of 2026-10-23. |
| Is sieve-rs's AGPL-3.0-only license acceptable for mejl, or is a permissive engine (mailrs-sieve-core, sieve-kit) good enough? | Decide mejl's license first. Then evaluate the permissive crates against the same experiment as section 6.1. |
| Do `X-Spam-score`, `X-Delivered-to` and `X-Mail-from` come back through JMAP on stored mail, so local dry-runs can use them? The delivery process page says Fastmail adds them at delivery, so this is now a low-risk check. | Fetch a few stored messages with `Email/get` and `header:X-Spam-score:asText`. |
| Is the variables pipeline (`spam`, `skipinbox`, `stop`) stable enough for custom code to rely on? | Unknown and undocumented. Treat as unstable. Ask Fastmail if mejl ever depends on it. |
| Which require line is current: the help page line with `vnd.cyrus.imip`, or the 2025 sample line with `processcalendar`? | Read the require line in the user's own Edit custom Sieve code screen. |
| How does a UI rule actually compile? No source shows one; the only generated sample is for an alias folder setting. | Create one throwaway rule in a trial account and read the generated block. |
| Does an explicit `keep` in custom code really defeat alias folder delivery? | Test in the Sieve tester or a trial account. |
| Does `SieveScript/set` work with an API token, as one blog post claims without testing? | Steps 1 and 2 of the verification procedure answer this. |

## Fact-check notes

An adversarial fact-check ran on 2026-10-23. It opened every cited Fastmail page, RFC and repository, and reran the local probes. Changes:

- **Corrected:** the claim that each UI rule compiles to a `jmapquery` block with a `${stop}` guard. The cited forum post (#8) has no code. The code is in post #2, and it is generated for an alias's deliver-to-folder setting, not a UI rule. The link is fixed and the UI rule claim is downgraded to (inferred). The claim that UI rules survive renames through `:mailboxid` is downgraded the same way.
- **Corrected:** RFC 9661 says the `invalidSieve` line number SHOULD be given, not MUST. "Maps one to one onto ManageSieve commands" overstated the RFC; it now quotes the RFC's own wording.
- **Corrected:** the grouped mail view quote "doesn't create rules" is not on that page. It now quotes "Groups only change how your message list is laid out".
- **Corrected:** sieve-rs is AGPL-3.0-only on crates.io for every release since 0.1.0, not since 0.4.0. Added the README's "or any later version" wording, which conflicts with crates.io.
- **Corrected:** the help page require line is no longer called "current", since a 2025 sample uses `processcalendar` (RFC 9671) instead of `vnd.cyrus.imip`, and nothing dates either line.
- **Corrected small details:** shellguard's last commit is 2026-04-19 UTC; the help pages link `settings/filters?sessionpicker=1`; the sidebar also offers invite and edit; the himalaya subcommand list comes from its source tree, not its README; the aRustyDev repo has no license; the Reddit thread date could not be confirmed, and the commenter calls the layout abbreviated; the wildcard DNS note now names the addresses.
- **Upgraded:** Cyrus's `:regex` is POSIX ERE (Cyrus docs). The sieve-rs experiment was rerun with the same results. The `openssl -starttls sieve` keyword and the sieve-connect flags were rechecked. `_sieve._tcp.fastmail.com` returns NXDOMAIN from 1.1.1.1.
- **Added:** that API tokens are not available on Basic plans, and that probe tokens need Type JMAP. Added from the delivery process page: the delivery headers, the order of discard and spam checks, the "Organize rules" wording, the `X-Spam-known-sender` caveat (a contact can still get `no (...DMARC...), in-addressbook`), and that Fastmail runs Cyrus. Also added: the RFC 9042 fallback for `:mailboxid`, the RFC 8474 note on JMAP compatibility, RFC 9661 section 4 on vacation scripts, and the policy clause against asking users for passwords. Also added the forum report that an explicit `keep` breaks alias folder delivery, so option A never emits `keep`, plus mute behavior and an untested blog claim about `SieveScript/set`.
- **Unchanged and confirmed:** OAuth scopes and manual client registration, API token scopes, the Sieve FAQ ManageSieve quote, the server names page, the policy and terms quotes, the MCP launch date (2026-04-22) and access levels, the Masked Email states, the rules limits (50 conditions, 1000 rules), import and export, the help page require line text, the third-party method and field names (read in their source code), the dwin sample's `rulesVersion: 3`, the terva and shellguard statements, Thunderbird Android issue #1410 being open, and the crates.io versions and licenses.

Still uncertain:

- Whether any Fastmail session advertises `urn:ietf:params:jmap:sieve`. No first-hand probe with the user's token was run.
- Whether the editor has three or four custom areas, and how UI rules compile.
- Everything about the generated script's layout and variables, which rests on third-party samples.
- That JMAP Mailbox ids equal Sieve `:mailboxid` values at Fastmail.
- Whether Fastmail's `:regex` matches Cyrus's documented POSIX ERE behavior.
- One possible risk outside this doc's scope: the API Developer Policy also bans using the API "to replicate or compete with core products or services offered by Fastmail" (verified: [API Developer Policy](https://www.fastmail.com/policies/api-developer-policy/)). How that applies to a third-party mail client should be asked when registering the OAuth client.
