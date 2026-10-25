# Plan: OAuth login for Fastmail

## Context

mejl logs in today with a pasted API token. Fastmail's dev docs say apps that others install "should implement OAuth", and Basic-plan users get no API tokens at all. mejl is now public (v0.1.0), so it needs an OAuth login.

Your answers:
- **Client id:** register dynamically on first login, once per device, through the open RFC 7591 registration Fastmail's metadata advertises. A config key `oauth_client_id` overrides it with a client id Fastmail gives you by hand.
- **`mejl login`:** asks "Log in with your browser" or "Paste an API token". `--oauth` and `--token` skip the question.
- **Masked Email:** ask for the scope. If it is refused, log in without it, and the mask screens explain why.

Known facts (`docs/fastmail-platform.md` 197-246, verified 2026-10-23):
- Fastmail uses Authorization Code with mandatory PKCE S256.
- Discovery metadata at `api.fastmail.com/.well-known/oauth-authorization-server` lists:
  - `registration_endpoint` `/oauth/register`
  - `token_endpoint` `/oauth/refresh` (form-encoded, no secret)
  - `revocation_endpoint` `/oauth/revoke`
  - `authorization_response_iss_parameter_supported`
  - scopes `urn:ietf:params:oauth:scope:{mail,contacts}` and `offline_access`, with no Masked Email scope
- Refresh tokens rotate on every use. Reusing an old one revokes the grant.
- `invalid_grant` may carry `temporary: true`.
- There is no device flow.
- Loopback redirect `http://127.0.0.1:<port>/`.

Not known: whether open registration works for a native client, which JMAP capabilities the `mail` scope grants, and whether the Masked Email scope can be had. Only a live login answers these. **No code, test or tool call contacts Fastmail.** You run the first live login yourself, and it creates a client on Fastmail's servers.

## Design

### Protocol layer (`crates/mejl-jmap`)

- A new module, `oauth.rs`. It does plain HTTP through the existing `Transport` trait, so tests reach only the fake.
  - **`discover(session_url)`:**
    - GET the session without a token, and read `resource_metadata` from the 401 `WWW-Authenticate` (already kept in `Error::Unauthorized`).
    - Then fetch the protected resource metadata, then the authorization server metadata (RFC 8414 well-known path).
    - Fallback: `{origin}/.well-known/oauth-authorization-server`.
    - Every endpoint must be https, or loopback in tests (same rule as `check_session_url`).
  - **`register`:** JSON POST with `client_name: "mejl"`, `redirect_uris: ["http://127.0.0.1/"]`, `token_endpoint_auth_method: "none"`, and the grant types. Returns the client id.
  - **`Pkce`:** a random verifier from `getrandom`, with challenge = base64url-no-pad(sha256(verifier)). `state` is random as well.
  - **`authorize_url`**, plus form-encoded POSTs for `exchange_code`, `refresh` and `revoke`.
  - **`TokenSet`:** access, refresh, `expires_at`, granted scope.
  - **`OAuthError`:** `error`, `description` and `temporary`, parsed from the RFC 6749 error JSON.
- **401 is split from 403:** `Error::Unauthorized` gains the status. Only a 401 triggers a refresh. A 403 (missing scope) stays an auth error with no refresh.
- **Credentials in `Client`:** `Inner.token: RwLock<Secret>` becomes a `Credentials` provider.
  - `Static(Secret)` keeps today's behaviour and `set_token`.
  - `OAuth`:
    - It holds a `tokio::sync::Mutex<TokenSet>`, the token endpoint, the client id and a `persist` callback.
    - It refreshes on its own when a token has less than 60 s left.
    - On a 401 it refreshes once, only if the rejected token is still the current one. That makes the refresh single-flight, so a rotated token is never reused.
    - It calls `persist` (on `spawn_blocking`) before it uses the new access token.
    - `invalid_grant` becomes `Error::Unauthorized`, so today's `auth_failed` and `wait_auth` path handles it unchanged.
  - `Client::send` resends once after a refresh. `Client::push` takes the current token at each connect.
  - The engine's and the app's clones share one provider (`Arc<Inner>`).

### Fake server (`crates/mejl-fake`)

- A new module, `oauth.rs`, routed in `http.rs::handle` before the `jmap/` check:
  - the discovery documents
  - a 401 with `resource_metadata`
  - `/oauth/register`
  - `/oauth/authorize`, which approves at once with a 302 to the loopback redirect carrying `code`, `state` and `iss`
  - `/oauth/token`, with code plus PKCE check, and refresh with rotation (reusing an old refresh token revokes the grant)
  - `/oauth/revoke`
- **Tokens and state:** the issued tokens live in `Mutex<OAuthState>`. `authorized()` accepts the static token or a live access token, so every existing harness keeps working.
- **Scopes in the session:** the session's capabilities follow the granted scopes. Masked Email shows only when its scope was granted.
- **Quirks:** expire all access tokens now, refuse the Masked Email scope (`error=invalid_scope`), refuse registration, answer `invalid_grant` with `temporary`.

### App (`crates/mejl`)

- **Secrets:**
  - A second keyring entry, user `<account>:oauth`. `:` cannot appear in an account name. It holds compact JSON: client id, where the id came from, issuer, token and revocation endpoints, access token, refresh token, expiry and granted scope.
  - It gets its own read path with a larger size limit, outside `check_token`. `MemoryStore` supports it.
  - **Which credential is used:** the order stays `MEJL_TOKEN`, then `token_command`, then the keyring, where an OAuth record wins over a stored API token. One login per account: an OAuth login deletes the stored token, and a token login deletes the OAuth record.
  - Every token is registered with the log redactor, refreshed ones included, through the `persist` callback.
- **`mejl login`:**
  - With a tty it asks which method. `--oauth`, `--token` and `--port <n>` exist; `--port` is for SSH port forwarding.
  - It takes the `InstanceLock` and refuses while mejl runs, because a running mejl would hold a refresh token that the new login makes useless.
  - **OAuth flow:**
    1. Discover the servers.
    2. Pick the client id: `oauth_client_id` from config, else the stored one for this issuer, else register and store it.
    3. Start a loopback listener on `127.0.0.1:<port or 0>`. It parses one request by hand and answers with a small "You can close this tab" page.
    4. Open the browser through an opener seam (the `open` crate in production, a test closure in tests), and print the URL for SSH.
    5. Wait up to 5 minutes. Check `state`, and check that `iss` equals the issuer.
    6. Exchange the code.
    7. Check the session with the new token (`verify_token`, `assess`) and store the record.
  - **Scopes:**
    - With the interoperable scopes advertised, it asks for `mail`, `contacts`, `offline_access` and the Masked Email scope.
    - If the answer is `invalid_scope`, it says so and does a second browser round without Masked Email.
    - A configured client id uses the documented JMAP scopes instead: `core`, `mail`, `submission`, `contacts`, `maskedemail`.
- **`mejl logout`:** reads the record, revokes the refresh token (best effort, with a message on failure), then deletes. Logout tests inject the fake transport, so they never reach the default Fastmail URL.
- **`mejl doctor`:**
  - It shows the method (API token or OAuth), the client id source, the granted scope and the expiry, never a token.
  - It shares one resolver with `resolve_token`, instead of repeating the order by hand.
- **Config:** `accounts.<name>.oauth_client_id`, added to `ACCOUNT_KEYS`, the doc table, `example_config` and the README.
- **UI:**
  - The auth-failure text follows the method. For OAuth: "Your Fastmail login expired or was revoked. Quit and run `mejl login`." A temporary `invalid_grant` gets its own text. `auth_error` is cleared when sync resumes.
  - Without the Masked Email capability, the masks screen and the mask actions say: "This login has no Masked Email access. Log in with an API token to use masks."

### Rules from review (they go into both agent briefs)

1. **Only one process ever refreshes.**
   - Only the TUI and `mejl login` may refresh. `doctor`, `logout` and every other command report the stored expiry and never refresh. `logout` revokes the refresh token, which needs no refresh.
   - Just before the TUI refreshes, it re-reads the keyring record and uses a newer refresh token if it finds one.
   - Test: two clients share one `MemoryStore`.
2. **The loopback listener copes with a real browser.**
   - It keeps accepting connections, with a short read timeout on each one.
   - Any path other than the callback gets a small reply and is ignored. It stops only on a callback with the right `state`.
   - The terminal says Ctrl-C or Esc cancels the wait. There is also a 5-minute timeout.
   - Test: a favicon request and an empty connection come before the real callback.
3. **When a refresh fails partway.**
   - **No response (timeout or reset):** retry once with the same refresh token. Retrying is never worse than not retrying:
     - The draft keeps the old token valid until the new one is used, so the retry works.
     - Fastmail's docs say reuse revokes. But without the response mejl has no new refresh token anyway, so the grant would be lost either way.
     - After that, it becomes an auth failure.
   - **The keyring write fails:** keep the new tokens in memory, warn once that the login will not survive a restart, and try to save again at the next refresh.
   - Fake quirk: drop the refresh response once.
4. **Scopes.**
   - The Masked Email scope goes together with `urn:ietf:params:jmap:core`. The fallback round drops both.
   - The granted scope in the token response and the session capabilities are the truth, whether or not the server answered `invalid_scope`.
   - The message on the masks screen comes from the session capability.
5. **A stricter fake.**
   - It serves the token endpoint at `/oauth/refresh`, as Fastmail does, so a hard-coded path fails.
   - It requires the token request's `redirect_uri` to equal the one sent to authorize.
   - Login fails when the server advertises `iss` support and the callback has none.
   - An unknown client id gets `invalid_client`. Login then drops the stored id and registers once more.
6. **No network.** Every new test that builds a transport injects the fake; `transport: None` reaches api.fastmail.com. The binary tests never run the OAuth flow. Grep the new tests for `transport: None` before handing off.

### Dependencies (I make these edits)

`getrandom = "0.4"` and `base64 = "0.22"` are already in `Cargo.lock`, and `sha2` is already in the workspace. All three go into `mejl-jmap`, and `sha2` and `base64` into `mejl-fake`. Licences are MIT/Apache.

## Work order

1. **Me:** the dependency edits.
2. **Agent A:** the protocol layer and the fake (`mejl-jmap`, `mejl-fake`), with their tests.
3. **Me:** review A's API before B starts.
4. **Agent B:** the app integration (`mejl` crate) and the UI texts, with their tests.
5. **An independent review agent:** security and correctness, focused on token handling.
6. **Docs:** the README login section, the architecture "Auth approach" rewritten as built, and the status of open questions 3 and 17 in `docs/README.md` ("implemented, untested live").

## Critical files

- `crates/mejl-jmap/src/{oauth.rs (new), client.rs, push.rs, error.rs, lib.rs}`
- `crates/mejl-fake/src/{oauth.rs (new), http.rs, lib.rs, config.rs}`
- `crates/mejl/src/{login.rs, secrets.rs, cli.rs, doctor.rs, config.rs, tui.rs, logging.rs, app/mod.rs}`
- The masked screen in `crates/mejl-ui`

## Verification

- **Unit tests:**
  - PKCE against the RFC 7636 appendix B vector.
  - Parsing the discovery documents and the 401 header.
  - The error JSON, with `temporary`.
  - The credentials provider:
    - it refreshes once when many requests get a 401 at the same time
    - it never sends an old refresh token
    - it persists before the new token is used
    - `invalid_grant` becomes an auth failure
    - a 403 gives no refresh
- **Fake tests:** register, authorize, token and refresh rotation, where reusing an old refresh token revokes the grant, and revoke.
- **App tests** (loopback fake, `MemoryStore`, temp dirs, no real keyring):
  - `mejl login --oauth` end to end, with a test opener that follows the redirect into mejl's listener.
  - A wrong `state` and a wrong `iss` are refused.
  - The Masked Email refusal leads to the second round, and the masks screen explains it.
  - The configured client id skips registration.
  - Logout revokes.
  - Doctor output contains no token.
  - The TUI keeps syncing across a forced token expiry, and the rotated refresh token is persisted.
  - The binary test sandbox still never touches the keyring.
- **Gate:** `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`, plus `cargo +1.88 check --workspace`.
- **Live, by you only:** `mejl login --oauth`, then `mejl doctor` to see the granted scope, then `mejl`. The result settles open questions 3 and 17.
