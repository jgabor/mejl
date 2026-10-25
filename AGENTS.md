# mejl

Fast, keyboard-first TUI email client for Fastmail, written in Rust, JMAP-first.
Research and architecture live in `docs/` (start with `docs/README.md` and `docs/architecture.md`).
The UI mock-ups in `docs/mockups.html` are the visual target.

## Layout

| Crate | Role |
|---|---|
| `crates/mejl-jmap` | JMAP wire types, typed methods, request builder, `Transport` trait, HTTP transport, EventSource push parser |
| `crates/mejl-fake` | In-process and loopback-HTTP fake JMAP server with fictional demo data. Tests, benches and `mejl --demo` |
| `crates/mejl-store` | SQLite `cache.db` and `state.db`: schema, sync application, page reads, FTS5 search, outbox with overlays, undo, drafts, rules |
| `crates/mejl-sync` | tokio sync engine: session, cold and warm sync, push, outbox flusher, bodies, server search, masked email |
| `crates/mejl-sieve` | mejl's rule model, Sieve renderer for the pasted block, validation, local dry-run matcher |
| `crates/mejl-render` | Message body to styled lines: html2text, link footnotes, quote and signature folding |
| `crates/mejl-ui` | ratatui app: state, update, views, keymap, mouse hit testing, themes |
| `crates/mejl` | Binary: CLI, config, login and secrets, thread wiring, demo mode |

## Commands

- Build: `cargo build`
- Gate before handing off: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
- Try it without an account: `cargo run -- --demo`
- MSRV check: `cargo +1.88 check --workspace`

## Hard rules

- Never call the `mcp__claude_ai_Fastmail__*` tools. They act on the user's real mailbox.
- No code, test or tool call may contact a Fastmail host. Tests use `mejl-fake`. Live checks are only run by the user.
- Never put the user's email address anywhere: not in `Cargo.toml`, User-Agent strings, fixtures, config defaults, comments or tool calls. mejl's User-Agent is `mejl/<version>`.
- Tests never touch the real OS keyring or real XDG config and data dirs. Use temp dirs and in-memory secret stores.
- Never call Fastmail's private `https://www.fastmail.com/dev/rules` API. Rules end in a Sieve block the user pastes.
- Rendered Sieve never contains `keep` or `discard`, only allowlisted extensions, and escapes `"` and `\`.
- The UI thread never awaits and never blocks on the network.
- Docs and comments: plain words, sentence case headings, no em dashes.
- Inline `#[cfg(test)] mod tests` blocks go last in their file. The CodeQL workflow treats everything from that line to the end of the file as test code (`.github/codeql/filter-tests.py`).
