# Fix known gaps and add "Mark as read on archive"

## Context

The wave 4 build left a list of known gaps. The user asked to fix them and to add a
setting, "Mark as read on archive". Nothing is committed; a snapshot is at
`$S/snapshot-pre-gaps.tgz` (taken). Decisions made with the user:

- Quit during the undo-send window asks. Choices: Send now (default), Keep queued, Cancel send, Stay.
- The setting applies to every archive path: single, selection, whole-mailbox bulk, archive-next/prev. Undo restores unread.

No git worktrees (repo has no commits). Work in the shared tree, one CARGO_TARGET_DIR per agent.
New dependencies go through the main agent only (`Cargo.toml`, `Cargo.lock`).

## Step 0: the setting (main agent, first, because it touches hot files)

- `crates/mejl/src/config.rs`: `ui.mark_read_on_archive` (bool, default false). Doc table, known-keys list (~l.392), validate list (~l.505), `example_config`, parse test.
- `crates/mejl-ui/src/state.rs` + `settings.rs`: config field and a settings row. `crates/mejl/src/app/settings.rs`: `Setting` variant, apply arm, TOML write-back, round-trip test.
- Live toggle: `plan()` in `crates/mejl/src/app/mutate.rs` reads current app settings per call (never captured at startup).
- Single path: chain `.keyword("$seen", true)` onto the Archive move patch before `flags.apply(&patch)`, so snapshots, overlay and unread count are right in the same frame; undo restores `before`. "Already in Archive" stays an error.
- Bulk path: `crates/mejl-sync/src/command.rs` `BulkAction::Archive` gains a way to carry mark-read; `BulkOverlay.seen = Some(true)`. First read how bulk undo works in sync/store; if undo only moves mail back, record and restore `$seen`, else reject with a message.
- Tests: headless app test (setting on/off, unread count in same frame, undo brings unread back), plus bulk variant.

## Step 1: parallel streams, disjoint file ownership

Hot files have one owner: `mejl-ui/src/update.rs`, `mejl/src/app/mod.rs`, `mejl/tests/app.rs` belong to Stream A. Other streams write requests for those files into an API note.

**Stream A: behaviour (owns update.rs, app/mod.rs, tests/app.rs)**
- Undo one step late: find why the overlay arrives a turn late first; test asserts the restored row shows in the same frame as the undo key.
- Quit during undo-send: prompt per decision above, default Send now. Tests for each choice.
- Ctrl+Z: bind in all contexts (check compose and vim preset clashes); after SIGCONT re-enter terminal and fully redraw.
- Hostile-mail test for mask descriptions.

**Stream B: terminal (owns `mejl/src/terminal/*`, `events.rs`, `input.rs`, `tui.rs`)**
- `$EDITOR` lost keystrokes: stop and join the input reader before spawning the editor.
- Probe leaks: drain responses with a timeout and filter OSC/DCS after the probe.
- `caps.toml` capability cache and terminal quirks table.
- Mode 2031 live theme notifications (emit an event; the ui handler request goes in the API note).
- pty tests for `enter`/`restore`/`suspend_*`. Run `cargo tree -i rustix` first; prefer enabling a feature on an existing dep over a new crate.

**Stream C: UI (owns `mejl-ui/src/compose.rs`, `view/*`, `reader.rs`, `rules.rs`, `update/{compose,rules,masked,search}.rs`)**
- Compose editor past ~800 lines: measure at 5k lines before and after against the key-to-frame budget; fix the hot path.
- Mock-up details: reader side-panel rows, search filter block, Masked Email filters, rules "where the block runs" section. Insta snapshots.

**Stream D: drafts to server and HTML (owns `mejl-sync`, `mejl-store`, `mejl-fake`, `mejl-jmap`, `mejl-render`)**
- Drafts: `Email/set` create in the Drafts role mailbox with `$draft` and `$seen`; update is create-new plus destroy-old in one request; store the server id in the state.db draft row; destroy the server copy on send and discard; no Drafts role means local only, with a message. `mejl-fake` supports all of it.
- Open in browser: add an HTML sanitizer in addition to the CSP. Check licence and `cargo +1.88` build first; the dependency add goes through the main agent.

## Brief for every agent

Hard rules from `CLAUDE.md`: no `mcp__claude_ai_Fastmail__*`; no network except 127.0.0.1; no real addresses; tests never touch the real keyring or XDG dirs; no em dashes; no `unwrap()` on external data; no `todo!()`. Each fix needs a test that fails before the change where feasible. Plain words in docs.

## Verification and done

- Main agent runs the full gate: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo +1.88 check --workspace --all-targets`.
- Licence scan rerun after any new dependency.
- Real-terminal check in tmux at 160x40, 120x36, 80x24: archive with the setting on, undo, quit during undo-send, Ctrl+Z and `fg`.
- Docs updated (DTC): config table, README config section, the known-gaps list.
- Final report lists every gap as fixed with its test, partly fixed, or not done with the reason.
