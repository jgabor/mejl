# Fastmail Masked Email and how mejl could manage it

Research date: 2026-10-23. All Fastmail pages were fetched on that date.

## Summary

- Masked Email is a small, Fastmail-specific JMAP extension. It has one data type (`MaskedEmail`) and two documented methods: `MaskedEmail/get` and `MaskedEmail/set` (verified: [Fastmail API docs](https://www.fastmail.com/dev/#masked-email-api)). The capability URI is `https://www.fastmail.com/dev/maskedemail`. It is an identifier, not a web page: fetching it returns 404 (verified: HTTP request, 2026-10-23).
- An address has four states: `pending`, `enabled`, `disabled` and `deleted`. `disabled` sends mail to Trash. `deleted` bounces it. `pending` addresses are hidden in the Fastmail UI and are removed 24 hours after creation unless mail arrives (verified: [Fastmail API docs](https://www.fastmail.com/dev/#the-masked-email-data-type)).
- The object is small: `id`, `email`, `state`, `forDomain`, `description`, `lastMessageAt`, `createdAt`, `createdBy`, `url`, plus the create-only `emailPrefix`. There is no message count, no expiry and no per-address domain choice.
- Access needs the Masked Email scope. API tokens offer it as a checkbox. OAuth offers it as the scope `https://www.fastmail.com/dev/maskedemail`, and the client "MUST" also request `urn:ietf:params:jmap:core` (verified: [API tokens help](https://www.fastmail.help/hc/en-us/articles/5254602856719-API-tokens), [Fastmail scopes](https://www.fastmail.com/dev/#scopes)). API tokens are not available on the Basic plan (verified: same help page).
- Third-party reports say Fastmail has no delta sync for masks: `MaskedEmail/changes` fails with `cannotCalculateChanges`. They also say an undocumented `MaskedEmail/query` exists and that a used address cannot be destroyed (unverified: [maton-ai notes](https://github.com/maton-ai/api-gateway-skill/blob/main/references/fastmail/README.md)). mejl should plan for a full `MaskedEmail/get` with `ids: null` at startup and on refresh, and keep its own index.
- The biggest open question for mejl is sending. In the web app, a reply to mail that arrived at a mask goes out from that mask by default. You can also pick a mask as sender with "Show all" (verified: [Masked Email help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email)). We could not confirm whether JMAP `Identity/get` lists masks, or whether `EmailSubmission/set` accepts a mask in `From`. That needs a live test (see open questions).
- For mejl this means the mask features are mostly client-side joins. mejl keeps a local map from address to `MaskedEmail` and matches each message's `To`, `Cc` and the Fastmail header `X-FM-Masked-Email` against it. That gives a mask badge in the reader, per-mask message lists and stats, and a one-key disable. Disabling is a single `MaskedEmail/set` update.

## Sources and labels

- Primary: [fastmail.com/dev](https://www.fastmail.com/dev/) (the developer page; `/for-developers/masked-email/` serves the same content), fastmail.help articles, RFC 8620 and RFC 8621, the 1Password support page, and the Bitwarden SDK source (primary for how Bitwarden calls the API).
- Third-party observations: open source clients and agent skill docs that call the live Fastmail API. Their claims are marked "(unverified)" unless a primary source agrees.
- Weigh the maton-ai notes with care. They were observed through Maton's own gateway (`api.maton.ai/fastmail/...`), not by calling Fastmail directly, and they contradict themselves: the safety note at the top says `disabled` or `deleted` "routes incoming mail to Trash with no bounce", while their own state table (and Fastmail's docs) say `deleted` bounces (verified: [maton-ai notes](https://github.com/maton-ai/api-gateway-skill/blob/main/references/fastmail/README.md)). Every maton-derived claim below stays "(unverified)".
- Labels used below: "(verified: source)", "(inferred)" and "(unverified)".

## Product behavior

### What a masked address is

- A masked address is a unique, server-generated address. Mail to it lands in the same Fastmail account (verified: [Masked Email help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email)).
- Generated addresses look like random words plus a number, for example `dog.food3495@domain.tld` (verified: [Masked Email help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email)). The server picks the local part "from a predefined corpus" unless `emailPrefix` is given (verified: [Fastmail API docs](https://www.fastmail.com/dev/#masked-email-set)).
- The domain is `fastmail.com` or one custom domain. It is chosen per account under Settings, Users & Sharing, Team settings. In multi-user accounts, only admins can change it (verified: [Masked Email help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email)). The API docs say "the domain will be set as per the Masked Email domain setting for the customer" (verified: [Fastmail API docs](https://www.fastmail.com/dev/#masked-email-set)). A client cannot pick a domain per address.
- If the custom domain is removed from the account, masks at that domain are deleted and mail to them bounces (verified: [Masked Email help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email)).

### States and what happens to mail

The web UI uses different words from the API. The mapping below is inferred from matching descriptions.

| API state | Web UI word | Mail sent to it | Visible in Fastmail UI | Source |
|---|---|---|---|---|
| `pending` | (hidden) | Delivered. The address flips to `enabled` on first message | No | verified: [API docs](https://www.fastmail.com/dev/#the-masked-email-data-type) |
| `enabled` | Active | Delivered normally (Inbox by default) | Yes | verified: [API docs](https://www.fastmail.com/dev/#the-masked-email-data-type), [help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email) |
| `disabled` | Blocked | Accepted, then "sent straight to trash". The sender is not told (inferred: no bounce is documented) | Yes | verified: [API docs](https://www.fastmail.com/dev/#the-masked-email-data-type), [help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email) |
| `deleted` | Deleted | Bounced | Under "Review deleted masked addresses" | verified: [API docs](https://www.fastmail.com/dev/#the-masked-email-data-type), [help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email) |
| (destroyed) | Permanently deleted | Presumably rejected as an unknown address | No | inferred |

Notes:

- Pending addresses "are automatically deleted 24h after creation and are not shown in the Fastmail UI" (verified: [API docs](https://www.fastmail.com/dev/#the-masked-email-data-type)). This suits password managers: they can offer an address, and if the user never signs up, it goes away. 1Password's "Regenerate" button probably uses this (inferred; [1Password support](https://support.1password.com/fastmail/) documents the button but not the API calls).
- Deleted addresses "can be restored at any time". An address that "has been created but never received mail" can be permanently deleted from the review screen (verified: [Masked Email help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email)).
- A bounce tells the sender the address is dead. A disabled address keeps accepting mail and hides that fact (inferred). For a spammy sender, `disabled` is the quieter choice. For a breached address you never want again, `deleted` is the clearer one.
- We do not know if mail to a `disabled` address passes through the user's Sieve rules before landing in Trash (unverified).

### Display and replies in the Fastmail web app

- Messages sent to a masked address show a mask icon in the message list, and the address description next to the sender in the reader (verified: [Masked Email help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email)).
- A reply to such a message uses the masked address as sender by default. To start a new message from a mask, open the sender list in Compose and choose "Show all" (verified: [Masked Email help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email)).
- Fastmail's own help suggests a rule on the header `X-FM-Masked-Email` to forward only masked mail (verified: [Masked Email help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email)). From this we infer that Fastmail adds the header at delivery (inferred). The help page only shows that rules can match it. Whether JMAP `Email/get` returns it on the stored message is also not documented (unverified; the Cyrus header cache entry below suggests yes, inferred). Its exact value (address, id or something else) is not documented (unverified).
- The open source Cyrus IMAP server, which Fastmail develops and is believed to run (inferred), caches two related headers: `x-fm-masked-email` and `x-fm-masked-email-reply` (verified: [cyrus-imapd mailbox_header_cache.gperf](https://github.com/cyrusimap/cyrus-imapd/blob/master/imap/mailbox_header_cache.gperf)). What the `-reply` header is for is unknown (unverified). It may mark outgoing replies sent from a mask (inferred guess only).
- The Settings, Masked Email screen lists addresses, the last time each received mail, and the state. It has a search box over description and address, and a "Search Mail" button per address. That search skips Trash and Spam unless you add `in:*` (verified: [Masked Email help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email)). This matters because mail to a disabled mask sits in Trash.

### Limits

- Fastmail limits both the total number of masked addresses and how many can be created in a period. It does not publish the numbers (verified: [Account limits](https://www.fastmail.help/hc/en-us/articles/1500000277382-Account-limits)). The troubleshooting advice is "Try again in 24 hours" (verified: [Masked Email help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email)).
- Reddit users report a total of 1000 masks and a creation cap of about 10 per week (unverified: [r/fastmail](https://www.reddit.com/r/fastmail/comments/vd0zkw/how_many_masked_email_can_i_create/), [r/fastmail](https://www.reddit.com/r/fastmail/comments/1j82wsf/masked_email_addresses_limit/)). In the second thread, one user says support lifted the limit for them after they hit it (unverified). Treat all of these as anecdotes.

### A privacy caveat from the forums

One forum user reports that a mask whose mail is forwarded off-Fastmail by a rule can leak the forwarding target to a sender. The method was not disclosed (unverified: [emaildiscussions thread](https://www.emaildiscussions.com/showthread.php?t=81287)). If mejl ever creates forwarding rules for masks, it should warn about this.

## The API

### Capability and session discovery

- The session resource is `https://api.fastmail.com/jmap/session` (verified: [Fastmail API docs](https://www.fastmail.com/dev/#getting-started-with-jmap)).
- The session advertises `https://www.fastmail.com/dev/maskedemail` "to represent support for the MaskedEmail data type and associated methods" (verified: [Fastmail API docs](https://www.fastmail.com/dev/#masked-email-api)).
- The account to use is `primaryAccounts["https://www.fastmail.com/dev/maskedemail"]`. Bitwarden reads exactly this key (verified: [Bitwarden sdk-internal fastmail.rs](https://github.com/bitwarden/sdk-internal/blob/main/crates/bitwarden-generators/src/username_forwarders/fastmail.rs)).
- The capability object's contents are not documented. No capability limits are published (verified: absence on [API docs](https://www.fastmail.com/dev/#masked-email-api)).

### Authentication and scope

| Method | How to get Masked Email access | Source |
|---|---|---|
| API token (personal use) | Settings, Privacy & Security, Manage API tokens. Choose type JMAP, then tick "Masked Email" ("Grants access to see, create, and manage your Masked Email addresses") | verified: [API tokens help](https://www.fastmail.help/hc/en-us/articles/5254602856719-API-tokens) |
| OAuth 2.0 (distributed apps) | Request scope `https://www.fastmail.com/dev/maskedemail` together with `urn:ietf:params:jmap:core`. The docs say "You MUST request this scope if requesting any other JMAP-based scope" | verified: [Fastmail scopes](https://www.fastmail.com/dev/#scopes) |

- API tokens are "not available to users on Basic subscription plans" (verified: [API tokens help](https://www.fastmail.help/hc/en-us/articles/5254602856719-API-tokens)).
- JMAP API tokens can be combined: read-only, Email, Email submission, Contacts and contact groups, Masked Email. Email submission requires Email to be selected too (verified: [API tokens help](https://www.fastmail.help/hc/en-us/articles/5254602856719-API-tokens)). For mail, sending and masks, mejl needs Email, Email submission and Masked Email. No token scope for Sieve or rules is listed (verified: [API tokens help](https://www.fastmail.help/hc/en-us/articles/5254602856719-API-tokens)); see the Sieve doc for how rules can be managed.
- OAuth clients are registered by hand with Fastmail. Fastmail asks for the minimal scope set (verified: [Fastmail OAuth registration](https://www.fastmail.com/dev/#registration)). A distributed mejl needs a registered client id. The flow is Authorization Code with PKCE S256, and a loopback redirect `http://localhost/` with any port is allowed (verified: [Fastmail authorization](https://www.fastmail.com/dev/#authorization)). This suits a terminal app.
- Refresh tokens rotate on every use. Reusing an old refresh token revokes the authorization, and each device must get its own authorization (verified: [Fastmail docs, getting a new access token](https://www.fastmail.com/dev/#getting-a-new-access-token)). This is general auth, not mask specific; see the auth doc.
- The `using` array should contain both `urn:ietf:params:jmap:core` and the masked email URI. This is a separate claim from the OAuth scope rule above. A bot author's commit is titled "fix: MaskedEmail/set now requires urn:ietf:params:jmap:core scope part #2" ([commit cc5778d, 2025-09-19](https://github.com/savely-krasovsky/masked-email-bot/commit/cc5778dab60106d005109dc1b7218510ba19313f)). That suggests a server-side change around September 2025 (unverified: one third-party commit). Bitwarden also sends both (verified: [Bitwarden source](https://github.com/bitwarden/sdk-internal/blob/main/crates/bitwarden-generators/src/username_forwarders/fastmail.rs)), and maton calls `core` "required in every request" (unverified). RFC 8620 itself does not require `core` in `using` ([RFC 8620 section 3.3](https://www.rfc-editor.org/rfc/rfc8620.html#section-3.3)). That Fastmail now strictly requires it is unverified; always send it.

### The MaskedEmail object

All fields come from the [Fastmail API docs](https://www.fastmail.com/dev/#the-masked-email-data-type) (verified), unless the notes column says otherwise.

| Property | Type | Who sets it | Notes |
|---|---|---|---|
| `id` | `String` | Server, immutable | Opaque id. Bron Gondwana's example shows `masked-123456` ([HN comment, 2021-09-28](https://news.ycombinator.com/item?id=28683128)); treat the format as illustrative |
| `email` | `String` | Server, immutable | The full address. Always read it from the response, never predict it |
| `state` | `String` | Client or server | One of `pending`, `enabled`, `disabled`, `deleted`. Default `pending`. Cannot go back to `pending` |
| `forDomain` | `String` | Client | Origin of the site, for example `https://www.example.com`. No path. "Intended to be added automatically by password managers". Real data does not always follow this: see observed details below |
| `description` | `String` | Client | Short user-supplied note. Shown next to the address and "used to tag mail sent to it". Use `""` if the user gave none. Do not put the product name or site domain here |
| `lastMessageAt` | `UTCDate\|null` | Server | Time of the most recent message received, or null |
| `createdAt` | `UTCDate` | Server, immutable | Creation time |
| `createdBy` | `String` | Server, immutable | Client name, derived from the credentials (OAuth client, API token, web UI). "Your product should not try to set this". The masked-email-bot README says its OAuth integration "allows my app to be shown as the creator" in the web UI (unverified: [bot README](https://github.com/savely-krasovsky/masked-email-bot)) |
| `url` | `String\|null` | Client | Deep link back into the integrator's record, for example a custom URI. Leave `null` if not supported. Not the site URL |
| `emailPrefix` | `String` | Client, create-only | Not returned on get. At most 64 characters, only `a-z`, `0-9`, `_`. The server address "will start with the given prefix" |

Observed details (unverified):

- `emailPrefix` gets a random suffix, for example `news.tztmu@fastmail.com`. Some prefixes are reserved and fail with `invalidProperties` ("Name is reserved"); reported examples are `shop`, `store`, `admin` and `beta` ([maton-ai notes](https://github.com/maton-ai/api-gateway-skill/blob/main/references/fastmail/README.md)). The masked-email-bot README gives a matching example, `google.cvbsd@fastmail.com` from prefix `google` ([bot README](https://github.com/savely-krasovsky/masked-email-bot)).
- A sample response captured in a code comment (createdAt in 2021) shows `createdBy: ""`, `forDomain: "fastmail.com"` (a bare host, not an origin) and `state: "deleted"` in an `ids: null` result ([dvcrn/maskedemail-cli requests.go](https://github.com/dvcrn/maskedemail-cli/blob/master/pkg/requests.go)). The comment does not say how that address was created, so do not read `""` as "made in the web UI".
- Bitwarden sends whatever website string it has as `forDomain`; its own test passes `"example.com"`, not an origin ([Bitwarden source](https://github.com/bitwarden/sdk-internal/blob/main/crates/bitwarden-generators/src/username_forwarders/fastmail.rs)). mejl should parse `forDomain` leniently (origin, bare host or empty) when it shows a host.

For mejl, `description` and `forDomain` are the human labels. When the user names a mask in mejl, store the name in `description`. Put the site origin in `forDomain` if known. Leave `url` as `null` unless mejl gains a deep-link scheme (inferred, following the integrator rules above).

### State transitions

| From \ To | `pending` | `enabled` | `disabled` | `deleted` | destroy |
|---|---|---|---|---|---|
| (create) | default | allowed | allowed (inferred) | allowed (inferred) | n/a |
| `pending` | n/a | allowed; also automatic on first message | allowed (inferred) | allowed (inferred) | unverified |
| `enabled` | forbidden | n/a | allowed | allowed | only if never received mail (unverified over the API) |
| `disabled` | forbidden | allowed | n/a | allowed | only if never received mail (unverified over the API) |
| `deleted` | forbidden | allowed ("restored", verified in the web UI only) | allowed (inferred) | n/a | only if never received mail (verified in the web UI) |

- "Once set to anything else, it cannot be set back to pending" (verified: [API docs](https://www.fastmail.com/dev/#the-masked-email-data-type)).
- Restoring deleted addresses is verified for the web UI ([help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email)). Over the API, a client would set `state` to `enabled` or `disabled` (inferred). [dvcrn/maskedemail-cli](https://github.com/dvcrn/maskedemail-cli) has enable, disable and delete commands that set `state`, but nothing confirms that the server accepts `deleted` to `enabled` (unverified).
- The help page only says a never-used address "can be permanently deleted from the Review deleted masked addresses screen" (verified: [help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email)). Whether API `destroy` works on a never-used address in any state is unverified. Destroying an address that has received mail reportedly fails with `forbidden` and `subType: "addressInUse"` (unverified: [maton-ai notes](https://github.com/maton-ai/api-gateway-skill/blob/main/references/fastmail/README.md)). That fits the help page's rule, so the subtype is plausible (inferred).
- Any value outside the four states reportedly fails with `invalidProperties: ["state"]` (unverified: maton-ai notes).

### Methods

| Method | Status | Notes |
|---|---|---|
| `MaskedEmail/get` | Documented | Standard RFC 8620 section 5.1 `/get`. `ids: null` fetches all (verified: [API docs](https://www.fastmail.com/dev/#masked-email-get)) |
| `MaskedEmail/set` | Documented | Standard RFC 8620 section 5.3 `/set` with `create`, `update`, `destroy` (verified: [API docs](https://www.fastmail.com/dev/#masked-email-set)) |
| `MaskedEmail/changes` | Not documented | Reported to fail with `cannotCalculateChanges` (unverified: [maton-ai notes](https://github.com/maton-ai/api-gateway-skill/blob/main/references/fastmail/README.md)) |
| `MaskedEmail/query` | Not documented | Reported to work with filters `forDomain`, `state`, `text`, returning `queryState: "unknown"` (unverified: maton-ai notes). Another Rust client says there is no `/query` ([penguin-mail masked.rs](https://github.com/c9dev/penguin-mail/blob/main/jmap/src/masked.rs)). The reports conflict |
| `MaskedEmail/queryChanges` | Not documented | Unknown |

More observed details (unverified, maton-ai notes): the `/get` response `state` is `""`, and `/set` returns `newState: null`. If true, the RFC 8620 sync model (store `state`, then call `/changes`) does not work for masks. A full refetch is the only safe sync.

Sizing note: RFC 8620 says `ids: null` returns all records only if "the number of records does not exceed the `maxObjectsInGet` limit" (verified: [RFC 8620 section 5.1](https://www.rfc-editor.org/rfc/rfc8620.html#section-5.1)). A 2026 session dump in the maton notes shows `maxObjectsInGet: 4096` (unverified: [maton-ai notes](https://github.com/maton-ai/api-gateway-skill/blob/main/references/fastmail/README.md)). The same value appeared in a 2021 session dump, as history only (verified: [1Password guest post on the Fastmail blog, 2021-11-11](https://www.fastmail.com/blog/how-and-why-we-built-masked-email-with-jmap-an-open-api-standard/); that dump uses the old `jmap.fastmail.com` host and does not list the masked email capability). mejl should read the live value from the session. Deleted masks count, because they are returned too. An account near the limit might get `requestTooLarge` (inferred). With a total cap around 1000 (unverified), this is unlikely in practice.

### Errors, rate limits and quotas

- Creation is rate limited. When exceeded, `/set` returns a standard `rateLimit` SetError on create (verified: [API docs](https://www.fastmail.com/dev/#masked-email-set)). RFC 8620 defines it as "Too many objects of this type have been created recently, and a server-defined rate limit has been reached. It may work if tried again later" (verified: [RFC 8620 section 5.3](https://www.rfc-editor.org/rfc/rfc8620.html#section-5.3)).
- No numbers, windows or `Retry-After` behavior are published (verified: absence on [API docs](https://www.fastmail.com/dev/) and [Account limits](https://www.fastmail.help/hc/en-us/articles/1500000277382-Account-limits)).
- A total-count cap exists (verified: [Account limits](https://www.fastmail.help/hc/en-us/articles/1500000277382-Account-limits)). Which SetError it produces (`overQuota` or `forbidden`) is unknown (unverified).
- Expected SetErrors to handle on create: `rateLimit`, `invalidProperties` (bad prefix, reserved prefix, bad state), possibly `overQuota`. On update: `notFound`, `invalidProperties`. On destroy: `forbidden` with `addressInUse` (inferred from the above).

### Full example

These exchanges are illustrative. Ids, addresses and dates are placeholders. The shapes follow RFC 8620, the Fastmail docs, [Bron Gondwana's HN example](https://news.ycombinator.com/item?id=28683128), a [captured get response](https://github.com/dvcrn/maskedemail-cli/blob/master/pkg/requests.go) and [Bitwarden's request](https://github.com/bitwarden/sdk-internal/blob/main/crates/bitwarden-generators/src/username_forwarders/fastmail.rs). Note that Bron's 2021 example sets `url` to the site (`https://news.ycombinator.com`), which the current docs say not to do; the examples below follow the current docs.

**1. Session (excerpt).** `GET https://api.fastmail.com/jmap/session` with `Authorization: Bearer <token>`.

```json
{
  "capabilities": {
    "urn:ietf:params:jmap:core": { "maxObjectsInGet": 4096, "maxCallsInRequest": 50 },
    "urn:ietf:params:jmap:mail": {},
    "urn:ietf:params:jmap:submission": {},
    "https://www.fastmail.com/dev/maskedemail": {}
  },
  "primaryAccounts": {
    "urn:ietf:params:jmap:mail": "u1234abcd",
    "https://www.fastmail.com/dev/maskedemail": "u1234abcd"
  },
  "apiUrl": "https://api.fastmail.com/jmap/api/",
  "eventSourceUrl": "https://api.fastmail.com/jmap/event/?types={types}&closeafter={closeafter}&ping={ping}"
}
```

The URLs here are simplified. A 2026 session dump shows a regional host (`https://phl.api.fastmail.com/jmap/api/`) and `eventSourceUrl` as a URI template with `{types}`, `{closeafter}` and `{ping}` (unverified: [maton-ai notes](https://github.com/maton-ai/api-gateway-skill/blob/main/references/fastmail/README.md)); RFC 8620 defines that template ([RFC 8620 section 7.3](https://www.rfc-editor.org/rfc/rfc8620.html#section-7.3)). Always read `apiUrl` and `eventSourceUrl` from the session and expand the template. Do not hard-code them.

**2. List all masks.** `POST` to `apiUrl`.

```json
{
  "using": ["urn:ietf:params:jmap:core", "https://www.fastmail.com/dev/maskedemail"],
  "methodCalls": [
    ["MaskedEmail/get", { "accountId": "u1234abcd", "ids": null }, "g0"]
  ]
}
```

Response:

```json
{
  "methodResponses": [
    ["MaskedEmail/get", {
      "accountId": "u1234abcd",
      "state": "",
      "list": [
        {
          "id": "masked-123456",
          "email": "flighty.emu5803@example.com",
          "state": "enabled",
          "forDomain": "https://news.ycombinator.com",
          "description": "Hacker News",
          "url": null,
          "createdBy": "mejl",
          "createdAt": "2026-10-01T09:12:44Z",
          "lastMessageAt": "2026-10-22T18:03:10Z"
        },
        {
          "id": "masked-654321",
          "email": "brisk.otter2210@example.com",
          "state": "deleted",
          "forDomain": "",
          "description": "Old shop",
          "url": null,
          "createdBy": "",
          "createdAt": "2023-03-14T08:00:00Z",
          "lastMessageAt": "2024-01-02T10:00:00Z"
        }
      ],
      "notFound": []
    }, "g0"]
  ],
  "sessionState": "cyrus-0;p-0"
}
```

The empty `state` matches a third-party report (unverified). Deleted entries are in the list, so filter them on the client. `createdBy: "mejl"` is illustrative: the server derives the value from the credentials, so with an API token it is probably the token name, and with OAuth the registered client name (unverified).

**3. Create an enabled mask while composing.**

```json
{
  "using": ["urn:ietf:params:jmap:core", "https://www.fastmail.com/dev/maskedemail"],
  "methodCalls": [
    ["MaskedEmail/set", {
      "accountId": "u1234abcd",
      "create": {
        "k1": {
          "state": "enabled",
          "forDomain": "https://shop.example",
          "description": "Shop example newsletter",
          "url": null,
          "emailPrefix": "examplenews"
        }
      }
    }, "s0"]
  ]
}
```

Success response:

```json
{
  "methodResponses": [
    ["MaskedEmail/set", {
      "accountId": "u1234abcd",
      "oldState": null,
      "newState": null,
      "created": {
        "k1": {
          "id": "masked-777777",
          "email": "examplenews.qz4ty@example.com",
          "createdAt": "2026-10-23T12:00:00Z"
        }
      },
      "notCreated": null
    }, "s0"]
  ],
  "sessionState": "cyrus-0;p-0"
}
```

Rate limited response (same request):

```json
["MaskedEmail/set", {
  "accountId": "u1234abcd",
  "oldState": null,
  "newState": null,
  "created": null,
  "notCreated": {
    "k1": { "type": "rateLimit", "description": "Too many masked email addresses created recently" }
  }
}, "s0"]
```

The `description` text is invented. Only `type: "rateLimit"` is documented.

**4. Disable a spammy mask, rename another.**

```json
{
  "using": ["urn:ietf:params:jmap:core", "https://www.fastmail.com/dev/maskedemail"],
  "methodCalls": [
    ["MaskedEmail/set", {
      "accountId": "u1234abcd",
      "update": {
        "masked-123456": { "state": "disabled" },
        "masked-777777": { "description": "Shop example (orders only)" }
      }
    }, "u0"]
  ]
}
```

Response:

```json
["MaskedEmail/set", {
  "accountId": "u1234abcd",
  "oldState": null,
  "newState": null,
  "updated": { "masked-123456": null, "masked-777777": null },
  "notUpdated": null
}, "u0"]
```

**5. Destroy a used mask (expected failure, unverified shape).**

```json
["MaskedEmail/set", {
  "accountId": "u1234abcd",
  "destroyed": null,
  "notDestroyed": {
    "masked-123456": { "type": "forbidden", "subType": "addressInUse" }
  }
}, "d0"]
```

A JMAP request may mix capabilities. mejl can send `MaskedEmail/get` in the same HTTP request as its `Email/query` and `Email/get` calls when `using` lists all needed URIs (verified: [RFC 8620 section 3.3](https://www.rfc-editor.org/rfc/rfc8620.html#section-3.3)). A new mask's `email` cannot be fed into an `Email/set` create in the same request. JMAP result references work on method arguments, not on values nested inside a create object (inferred from [RFC 8620 section 3.7](https://www.rfc-editor.org/rfc/rfc8620.html#section-3.7)). Creating a mask and then a draft from it takes two round trips.

## Sending from a masked address

What is verified:

- The Fastmail web app replies from the mask by default and can compose from any mask via "Show all" (verified: [Masked Email help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email)).
- Fastmail no longer has separate "sending identities". Aliases send and receive without extra setup. With a custom domain, a catch-all alias "allows you to send mail from any address at your domain" (verified: [Identities help](https://www.fastmail.help/hc/en-us/articles/1500000280401-Identities)). The help page does not say whether masks count as aliases for this.
- A 2021 HN commenter wrote that replies from a mask "seamlessly integrate with the sender identity feature in Fastmail" (unverified: [HN comment](https://news.ycombinator.com/item?id=28685052)). This is a hint that masks may appear as identities, not evidence about JMAP `Identity/get`.
- In JMAP, `EmailSubmission` needs an `identityId` (verified: [RFC 8621 section 7](https://www.rfc-editor.org/rfc/rfc8621.html#section-7)). A server may reject a `From` it does not permit with `forbiddenFrom` (verified: [RFC 8621 section 7.5](https://www.rfc-editor.org/rfc/rfc8621.html#section-7.5)). `Identity/set` create has its own `forbiddenFrom` for an address the user may not send from (verified: [RFC 8621 section 6.3](https://www.rfc-editor.org/rfc/rfc8621.html#section-6.3)). An Identity `email` of the form `*@example.com` allows any address in that domain (verified: [RFC 8621 section 6](https://www.rfc-editor.org/rfc/rfc8621.html#section-6)).

What is unknown:

- Whether `Identity/get` on Fastmail returns one identity per mask, none, or a wildcard (unverified).
- Whether `EmailSubmission/set` accepts a message with `From` set to a mask while `identityId` points at the user's main identity (unverified).
- Whether `Identity/set` can create an identity for a mask, or fails with `forbiddenFrom` (unverified).
- Possible escape hatch: if the mask domain is the user's own domain with a catch-all alias, a wildcard identity `*@domain` may cover masks (inferred).
- Contrast: a Stalwart 0.16.22 Enterprise server refused sending from a mask over both JMAP (`Identity/set` gave `invalidProperties`) and SMTP (`501 5.5.4`) (unverified: probes recorded in [inbuxa-server's draft spec, 2026-09-18](https://git.coffeylabs.org/inbuxa/inbuxa-server/src/branch/main/docs/spec/features/masked-email.md)). That is a different server. It says nothing about Fastmail.

How to settle it (a test plan, not run here): create a test API token with Email, Email submission and Masked Email scopes on a test account. Then:

1. Call `Identity/get` and check whether any mask appears.
2. Create a draft from a mask to the account itself.
3. Submit it with the main identity, then with any mask identity.
4. Record the SetError types and whether the delivered message carries `X-FM-Masked-Email-Reply`.

Until this is known, mejl's "reply from mask" feature is a design goal, not a confirmed capability.

## How integrations use the API

### 1Password

- Setup starts from Fastmail (Settings, Masked Email, "Connect to 1Password") or from 1Password.com, Integrations. The user is "prompted to allow 1Password access" (verified: [Masked Email help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email), [1Password support](https://support.1password.com/fastmail/)). That this is an OAuth grant is inferred from Fastmail's OAuth docs, which use a password manager as their scope example.
- On a sign-up page, the browser extension offers "Create Masked Email" with "Fill Email" and "Regenerate". Users can "Block Incoming Emails" and "Unblock Incoming Emails" from the login item. Deleting must be done in Fastmail (verified: [1Password support](https://support.1password.com/fastmail/)).
- Mapping to the API (inferred): create sets `forDomain` to the site origin; block maps to `state: "disabled"`; unblock maps to `"enabled"`. "Regenerate" likely leaves the unused address `pending` so it expires. `createdBy` shows 1Password's OAuth client name. Whether 1Password fills `url` with a deep link is unknown.
- 1Password had early access to the API, and the JMAP requests "were the easiest part" (verified: [guest post by 1Password's Madeline Hanley on the Fastmail blog, 2021-11-11](https://www.fastmail.com/blog/how-and-why-we-built-masked-email-with-jmap-an-open-api-standard/)).

### Bitwarden

- Bitwarden connects with a user-made API token that has only the Masked Email scope (verified: [Bitwarden help](https://www.fastmail.help/hc/en-us/articles/5469165600655-Using-Bitwarden-with-Fastmail)).
- Its Rust SDK first fetches `/.well-known/jmap` and takes `primaryAccounts["https://www.fastmail.com/dev/maskedemail"]`. It then posts one `MaskedEmail/set` create with `state: "enabled"`, `description: ""`, `forDomain: <website>` and `emailPrefix: null`. `using` is `["https://www.fastmail.com/dev/maskedemail", "urn:ietf:params:jmap:core"]`. It reads `created[...].email`, or `notCreated[...].description` on failure. HTTP 401 means a bad token (verified: [Bitwarden sdk-internal fastmail.rs](https://github.com/bitwarden/sdk-internal/blob/main/crates/bitwarden-generators/src/username_forwarders/fastmail.rs)).
- This is the closest thing to a reference Rust implementation. The repo is licensed as your choice of GPL-3.0 or the Bitwarden SDK License v2.0, unless a file header says otherwise (verified: [sdk-internal LICENSE](https://github.com/bitwarden/sdk-internal/blob/main/LICENSE)). Copying code is only an option if mejl is GPL-3.0 compatible. The API is small enough to write from the docs instead.

### Other integrations

| Project | Kind | Language | Notes |
|---|---|---|---|
| [Fastmask](https://fastmask.app/) | iOS app | n/a | Listed by Fastmail help as a third-party app; logs in with Fastmail credentials (verified: [help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email)) |
| [dvcrn/maskedemail-cli](https://github.com/dvcrn/maskedemail-cli) | CLI, npm wrapper, iOS app, Siri Shortcut | Go | Most used CLI (about 176 GitHub stars). create, list, enable, disable, delete, update. Its README links a Siri Shortcut that disables the mask of the selected Apple Mail message: prior art for mejl's one-key disable |
| [radiosilence/fastmail-cli](https://github.com/radiosilence/fastmail-cli) | CLI and MCP server | Rust | Mail, contacts and masked email over JMAP. Active in October 2026 |
| [c9dev/penguin-mail](https://github.com/c9dev/penguin-mail) | Mail client | Rust | Has a `masked.rs` that drops deleted masks while decoding and caps the list at 10,000 |
| [dan-hart/emask](https://github.com/dan-hart/emask) | CLI | Rust | Mints masks and copies them to the clipboard |
| [sharma233/fastmail_masked_manager](https://github.com/sharma233/fastmail_masked_manager) | CLI | Rust | Small, last pushed 2024 |
| [seanb4t/fastmail-cli](https://github.com/seanb4t/fastmail-cli) | CLI | Go | Defines `MaskedEmail/changes` types; whether it works is unknown |
| [ajyey/masked-email-manager](https://github.com/ajyey/masked-email-manager) | Browser extension | TypeScript | Fuzzy search, filter by state, favorites |
| [ajyey/fastmail-masked-email](https://github.com/ajyey/fastmail-masked-email), [melusc/fastmail-masked-email](https://github.com/melusc/fastmail-masked-email) | Node libraries | TypeScript | melusc's repo is archived |
| [hdb/fastmask](https://github.com/hdb/fastmask), [dwin/fastmask](https://github.com/dwin/fastmask) | Library and CLI | Python, Go | |
| [wheelercj/email-linter](https://github.com/wheelercj/email-linter) | CLI | Go | Lists each masked address and every sender it received from, to spot phishing. Uses a read-only JMAP token. Prior art for mejl's per-mask sender view |
| [pawelorzech/FastMask](https://github.com/pawelorzech/FastMask) | Android app | Kotlin | |
| [belaytzev/maskedmail-raycast-extension](https://github.com/belaytzev/maskedmail-raycast-extension), [hendryque/alfred-fastmail-masked-email](https://github.com/hendryque/alfred-fastmail-masked-email), [kejadlen/masked-email.alfredworkflow](https://github.com/kejadlen/masked-email.alfredworkflow) | Launcher plugins | TypeScript, Python, Rust | |
| [savely-krasovsky/masked-email-bot](https://github.com/savely-krasovsky/masked-email-bot) | Telegram bot | Go | Uses OAuth; source of the `core` in `using` fix |
| [marcaurele/pass-gen-masked](https://github.com/marcaurele/pass-gen-masked) | `pass` extension | Shell | |

Star counts and activity come from the GitHub API on 2026-10-23 (verified: each repo exists; activity is the last push date). A crates.io search for "fastmail", "masked", "maskedemail", "fastmask" and "masked email" found no dedicated Masked Email crate (verified: crates.io API search, re-run 2026-10-23). mejl will write its own small module. The API is about 10 fields and two methods, so this is cheap.

### Fastmail's own MCP server

- Fastmail runs an MCP server at `https://api.fastmail.com/mcp`, announced on 2026-04-22 (verified: [Fastmail blog, "An MCP server for Fastmail"](https://www.fastmail.com/blog/an-mcp-server-for-fastmail/)). Its access levels are read, write and send, over mail, contacts and calendars (verified: [MCP help](https://www.fastmail.help/hc/en-us/articles/15869557281295-Connecting-AI-tools-via-Fastmail-s-MCP-server)). Neither page mentions Masked Email. MCP API tokens offer "Read data", "Make changes" and "Send email", with no Masked Email scope (verified: [API tokens help](https://www.fastmail.help/hc/en-us/articles/5254602856719-API-tokens)).
- The tool names exposed by that server (from the connector list available to this research, not called) cover email, notes, contacts, calendars, identities and a memo. None is about masked email (inferred from names only). So Fastmail's MCP is not a route for mask management. mejl should use JMAP directly.
- Third-party MCP servers do cover masks over JMAP: [radiosilence/fastmail-cli](https://github.com/radiosilence/fastmail-cli) lists Masked Email and an MCP server in its README, and [fastmail-blade-mcp](https://pypi.org/project/fastmail-blade-mcp/) describes itself as covering "email, masked email, and push notifications" (unverified: project descriptions only). Not relevant to mejl's runtime.

### Other servers with a masked email API

Stalwart Enterprise has its own masked email object under `urn:stalwart:jmap` with `x:MaskedEmail/get`, `/query` and `/set`, an `enabled` boolean, `expiresAt` and `emailDomain` (verified: [Stalwart docs](https://stalw.art/docs/email/management/masked-email/)). Stalwart's masks are Enterprise only, and a disabled Stalwart mask rejects mail instead of filing it in Trash (verified: same page). inbuxa-server, a Stalwart-derived server, plans to serve both Stalwart's API and Fastmail's API shape. This is a draft spec dated 2026-09-18, not observed behavior, and the spec says its `disabled` has no exact Fastmail equivalent (unverified: [inbuxa draft spec](https://git.coffeylabs.org/inbuxa/inbuxa-server/src/branch/main/docs/spec/features/masked-email.md)). mejl targets Fastmail only. Neither server is a faithful stand-in for Fastmail's API, so use them, if at all, for rough smoke tests only (inferred).

## UX ideas for mejl

All ideas below are grounded in the API above. Keys are suggestions, in line with a keyboard-first design. Mouse clicks on the same targets should do the same thing.

### 1. Masks view: list, search, filter

- Data: one `MaskedEmail/get` with `ids: null` at startup, cached on disk. Refresh on demand, after any mejl write, and on a timer (say every 10 minutes). Push may not cover masks; see open questions.
- Columns: state glyph, `email`, `description`, `forDomain` (host only), `lastMessageAt` (relative, for example "3d"), `createdBy`, local message count.
- Search: fuzzy, local, over `email`, `description`, `forDomain`. This is instant because the whole set is in memory (a few hundred bytes per mask, so 1000 masks is well under 1 MB).
- Filters: state (default: hide `deleted` and `pending`, to match the Fastmail UI), "silent for N days" (`lastMessageAt` older than N or null), "created by" (1Password, Bitwarden, mejl), "has mail in Spam" (from the local index).
- Sort: last message, created, description, local message count.
- Keys: `M` opens the view; `/` searches; `e`, `x`, `D` set enabled, disabled, deleted; `r` edits description; `Enter` lists messages for the mask; `y` copies the address.

### 2. Create a mask inline while composing or replying

- In the From field, a "New masked address" action (`Ctrl-n` in the From picker) opens a one-line prompt: description (required in mejl, even though the API allows `""`), optional site.
- mejl sends `MaskedEmail/set` create with `state: "enabled"`. Do not leave it `pending`: a pending mask the user hands out but that gets no mail for a day would vanish (verified behavior of `pending`, so this choice is required).
- Use `emailPrefix` only if the user asks for it. Prefixes reveal intent ("shopname") and some are reserved (unverified).
- On `rateLimit`, show the plain message "Fastmail limits how many masked addresses you can create. Try again later." Offer to reuse an existing mask for the same `forDomain`.
- Signup helper: `mejl mask new --for https://site.example --desc "Site"` prints the new address, so it can be piped to the clipboard. This copies the Bitwarden and emask flow.
- Sending from the new mask depends on the open sending question.

### 3. Show which mask a message was delivered to

- When fetching messages, request `to`, `cc`, `bcc` and `header:X-FM-Masked-Email:asText` in `Email/get` properties (header property syntax verified: [RFC 8621 section 4.1.3](https://www.rfc-editor.org/rfc/rfc8621.html#section-4.1.3)). Cost is a few bytes per message.
- Reader header line: a mask glyph, then `description` and `forDomain`, for example "masked: Hacker News (news.ycombinator.com)". This mirrors the Fastmail web UI.
- List view: a one-cell mask glyph column, like Fastmail's mask icon.
- Phishing hint (inferred, after email-linter): if a message to a mask comes from a sender domain never seen before for that mask, or one unrelated to `forDomain`, show a soft warning. A message "from your bank" to a mask created for a shop is suspicious.

### 4. One-key disable from the reader

- Key `x` in the reader (the same key as in the masks view): "Disable masked address flighty.emu5803@example.com (Hacker News)? Future mail goes to Trash. [y/N]". On yes, mejl sends one `MaskedEmail/set` update to `disabled`.
- Show an undo toast for a few seconds (`u` restores `enabled`). Undo is always possible later from the masks view, because `disabled` and `deleted` can both go back to `enabled`.
- Warn when the mask has a recent `lastMessageAt` from senders that look transactional (password resets, receipts). Disabling means those mails go to Trash silently (inferred).
- Optional follow-up prompt: "Also move the N existing messages to Trash?" This is a separate `Email/set` and stays local to the user's choice.
- For a hard stop, `D` sets `deleted` (bounces) with a stronger confirm, matching the masks view.

### 5. Stats from `lastMessageAt` and the local index

- From the API alone: "last mail" per mask, "never received mail" (null), "silent for 6+ months", age from `createdAt`, counts by `createdBy`.
- From mejl's local mail index (no extra server calls): messages per mask, per month; senders per mask; share of each mask's mail in Spam or Trash. Count Trash, since mail to disabled masks lands there.
- A dashboard line in the masks view, for example: "412 enabled, 37 disabled, 120 deleted; 58 silent 6+ months; 3 got spam this week."
- Cleanup suggestion: masks with null `lastMessageAt` older than a month can be destroyed (allowed only for never-used masks). Masks silent for a long time can be offered for `disabled`. Always ask; never do this automatically.

### 6. Linking a masked address to messages (inferred design)

There is no server link from `MaskedEmail` to `Email`. mejl builds it:

1. Build `HashMap<lowercased email, MaskedEmail id>` from `MaskedEmail/get`, including deleted masks, so old mail still links.
2. For each message, collect candidate addresses: `to`, `cc`, `bcc` (present on sent copies only), and the `X-FM-Masked-Email` header value. Lowercase them. Also try stripping a `+tag` from the local part. Fastmail's help says plus addressing "will automatically work with any alias set up in your account", but it does not mention masks (verified: [Plus addressing and subdomain addressing](https://www.fastmail.help/hc/en-us/articles/360060591053-Plus-addressing-and-subdomain-addressing)). Whether it works on masks is unverified. Subdomain addressing needs an alias with no dot or underscore (verified: same page). Generated masks contain a dot (`dog.food3495`), so subdomain addressing likely does not apply to them (inferred).
3. The header wins when present, because it should cover BCC and mailing-list delivery, where the mask is not in `To` (inferred). If the header holds an address, look it up directly. If it holds something else, store it and work out the format with a live test.
4. A message can match at most one mask in normal use. If it matches several (one person CC'd two masks), show all.
5. Store the link in the local index, so per-mask views are a local lookup.

To fetch a mask's messages from the server (for example, mail older than the local cache), use `Email/query` with an `OR` filter over `{"to": addr}`, `{"cc": addr}` and `{"header": ["X-FM-Masked-Email", addr]}`, across all mailboxes including Trash. RFC 8621's `to` filter matches only the To header (verified: [RFC 8621 section 4.4.1](https://www.rfc-editor.org/rfc/rfc8621.html#section-4.4.1)), unlike Fastmail web search's `to:` which covers To, Cc and Bcc. Fastmail web search also has `deliveredto:` (verified: [Searching your mail](https://www.fastmail.help/hc/en-us/articles/360060591213-Searching-your-mail)). There is no standard JMAP filter for it. `{"header": ["Delivered-To", addr]}` might work if Fastmail writes that header (unverified).

### 7. Rules tie-in

- Fastmail rules can match "A header called X-FM-Masked-Email" (verified: [Masked Email help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email)). In Sieve that is `exists "X-FM-Masked-Email"`, or `header :contains "X-FM-Masked-Email" "<value>"` once the value format is known (inferred; see the Sieve doc for how mejl edits rules).
- From the reader, "Make a rule for this mask" (`R`) could prefill a rule: file all mail to this mask into a folder, or label it with the description. This is an alternative to disabling when the user still wants the mail.

### 8. Sync and speed

- Startup: one request that batches `MaskedEmail/get` with the mailbox and inbox calls. No extra round trip.
- Updates: write-through. Apply the state change to the local cache at once, send `MaskedEmail/set`, and roll back on `notUpdated`.
- Refresh: full `MaskedEmail/get`, then diff locally. With at most a few thousand masks this is cheap, so no `/changes` support is needed.

## What the API cannot do (gaps)

| Gap | Effect on mejl | Source |
|---|---|---|
| No `/changes` (reported) and no documented push type for masks | Must refetch the whole list; cannot react instantly to changes made in 1Password or the web UI | unverified: maton-ai notes |
| No documented `/query`; reports conflict | Do filtering and search locally | verified (absence in docs); conflict unverified |
| No message count or message ids on `MaskedEmail` | Counts and per-mask lists come from mejl's own index | verified: field list in [API docs](https://www.fastmail.com/dev/#the-masked-email-data-type) |
| No per-address domain choice | Domain is an account setting; mejl can only show it | verified: [API docs](https://www.fastmail.com/dev/#masked-email-set) |
| No expiry or auto-disable date | mejl could store a local "disable after" date and apply it while running, but nothing happens when mejl is closed | verified (absence); workaround inferred |
| Address shape not under client control | Only `emailPrefix`; the server always adds its own part (reported) | verified (prefix rules), suffix unverified |
| No published rate limits or quota numbers | Must handle `rateLimit` gracefully; cannot show "N left" | verified: [Account limits](https://www.fastmail.help/hc/en-us/articles/1500000277382-Account-limits) |
| `createdBy` is server-set | Masks made by mejl show the token or OAuth client name, which mejl cannot choose | verified: [API docs](https://www.fastmail.com/dev/#masked-email-api) |
| Used masks cannot be destroyed | "Delete" in mejl means `state: "deleted"` (restorable); true removal only for never-used masks | verified for the web UI: [help](https://www.fastmail.help/hc/en-us/articles/4406536368911-Masked-Email); API error shape unverified |
| Sending as a mask over JMAP not confirmed | Reply-from-mask may not be possible, or may need identity work | unverified |
| `X-FM-Masked-Email` value not documented | Linking must be tested before relying on the header | unverified |
| No mask tools in Fastmail's MCP server | Not relevant to mejl's runtime, but rules out MCP as a shortcut | inferred: [MCP help](https://www.fastmail.help/hc/en-us/articles/15869557281295-Connecting-AI-tools-via-Fastmail-s-MCP-server) |

## Open questions

| Question | How to resolve |
|---|---|
| Can JMAP send as a masked address? Does `Identity/get` list masks, does `EmailSubmission/set` accept a mask in `From`, and does `Identity/set` allow creating one? | Live test on a test account with Email, Email submission and Masked Email scopes (plan above). Ask Fastmail developer support if it fails |
| What is the value of `X-FM-Masked-Email`, and what is `X-FM-Masked-Email-Reply` for? | Send test mail to a mask (To, then BCC) and to a reply; fetch `header:X-FM-Masked-Email:all` |
| Is `using` with `core` strictly required now? | Send one request without `core` and check the error |
| Does `MaskedEmail/changes` really fail, and does `MaskedEmail/query` exist and with which filters? | Call both against a test account; record exact errors |
| Does the push channel (EventSource or WebSocket `StateChange`) ever name `MaskedEmail`? | Subscribe with `types=*`, change a mask in the web UI, watch the stream |
| What are the real creation rate limit and total cap, and which SetError does the cap produce? | Ask Fastmail support; do not probe the limit on a real account |
| Does mail to a `disabled` mask run through Sieve rules before it reaches Trash? | Create a test rule that labels mask mail; disable the mask; send a test |
| Does plus addressing work on masks (`mask+tag@domain`)? | Send a test message; check delivery and the header value |
| Does 1Password fill `url`, and which `createdBy` names appear in the wild? | Inspect a `MaskedEmail/get` response from a user who has 1Password and Bitwarden connected |
| Does the `maxObjectsInGet` limit apply to `ids: null` for accounts with many masks? | Read the session `maxObjectsInGet`; ask Fastmail; low priority given the reported total cap |
| What are the reserved `emailPrefix` words? | Collect `invalidProperties` errors as users hit them; there is no published list |
| Does API `destroy` work on a never-used mask that is not `deleted`, and does `deleted` to `enabled` work over the API? | Test on a test account with a fresh, unused mask |
## Fact-check notes

An adversarial fact-check on 2026-10-23 re-fetched every primary source cited here: fastmail.com/dev (all anchors exist; `/for-developers/masked-email/` serves byte-identical content; the capability URI returns 404), the Masked Email, API tokens, Account limits, MCP, Bitwarden, Identities, Searching and Plus addressing help pages, the 1Password support page, RFC 8620 and RFC 8621, and the cited GitHub files and repos. Most claims held up. Changes made:

- HN link: the Bron Gondwana example is comment [28683128](https://news.ycombinator.com/item?id=28683128), not the story 28682011. Added that his example sets `url` to the site, against current guidance.
- dvcrn capture: removed "web-created". The code comment does not say how the address was made. Added that its `forDomain` is a bare host, and that Bitwarden's test passes a bare host too.
- OAuth: added the docs' "You MUST request" wording for the `core` scope, and the refresh-token rotation rule. Kept the separate claim about `core` in `using` as unverified, quoting the commit title "MaskedEmail/set now requires urn:ietf:params:jmap:core".
- `X-FM-Masked-Email`: "Fastmail adds this header at delivery" is now labeled inferred. The help page only shows that rules can match it.
- State transitions: API `destroy` and API restore (`deleted` to `enabled`) are now unverified. The help page confirms both only in the web UI.
- `maxObjectsInGet`: the 2021 dump is a 1Password guest post (2021-11-11) on an old host and is kept as history. The current value is cited from maton's 2026 dump (unverified).
- Session example: `eventSourceUrl` is a URI template (RFC 8620 section 7.3), and the API host may be regional. Fixed the example and added a note.
- Sending: added the catch-all sending rule from the Identities help, a 2021 HN anecdote that masks act like sender identities (unverified), and the `Identity/set` `forbiddenFrom` from RFC 8621 section 6.3.
- Stalwart and inbuxa: inbuxa's Fastmail-compatible API is a draft spec, not observed behavior. The Stalwart sending refusal came from probes of Stalwart 0.16.22 and is labeled unverified.
- Bitwarden license: GPL-3.0 or the Bitwarden SDK License (verified). Replaced "check its license".
- MCP: added the launch date (2026-04-22) and two third-party MCP servers that do cover masks.
- Plus addressing: the help article was read. It does not mention masks. Subdomain addressing excludes dotted local parts.
- Smaller fixes: maton reliability caveat in "Sources and labels"; melusc repo is archived; Email submission requires Email in token scopes; one Reddit user had the limit lifted by support; the crates.io search was re-run with more terms.
- Style: no em or en dashes were found before or after edits. Headings were already in sentence case.

Still uncertain: everything sourced only from maton (no `/changes`, `/query` behavior, empty `state`, `addressInUse`, reserved prefixes), the `X-FM-Masked-Email` value, sending as a mask over JMAP, real rate limits and caps, and whether push covers masks. None of these could be checked without a live account, which this fact-check did not use.
