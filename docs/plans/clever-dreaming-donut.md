# Plan: custom key presets with a key editor, and macros

## Context

You use mejl on your real account (reading, moving, pinning, archiving and links all work). You asked for two features:

1. A key binding editor, like the controls screen in a PC game: duplicate a preset (for example `fastmail`) into your own named preset, then rebind each command one at a time.
2. Macros, for example "mark read, unpin, move to Folder", bound to one key.

Your answers so far:
- Macros are made and edited in the app, and saved to `config.toml`.
- Macro steps only change mail: mark read or unread, pin or unpin, mute, add or remove a label, move to a named folder, archive, trash, spam. Every step acts on the same conversations, the steps go to the server as one change, and one `z` undoes the whole macro.
- A custom preset stores only its base and your changes. It works like a full copy, and actions added to mejl later get their default keys.

Hard rule kept: nothing in this work sends mail. Tests use `mejl-fake` only.

What exists today:
- `crates/mejl-ui/src/keymap/` has per-`Context` tables of `KeySeq -> Entry{action, order}`. There are two built-in presets (`preset.rs`). Overrides come from `[keys.<ctx>]` (`config.rs`) and are applied by `Keymap::with_overrides`, which runs `check()`.
- `Action` is a closed `Copy` enum (`action.rs`). Which-key, help, hints, the palette, `HitTarget::Action`, `last_action` and Repeat all assume it.
- Settings rows (`mejl-ui/src/settings.rs`) push `Cmd::SaveSetting`. `crates/mejl/src/app/settings.rs` edits the file with `toml_edit` (comments kept, atomic write), rebuilds `UiConfig` (keymap included) and sends `Msg::Config`.
- Triage handlers in `update.rs` each build one `Mutation`, clear the selection and toast. `crates/mejl/src/app/mutate.rs::plan()` turns one mutation into one outbox entry, which gives one undo entry. `BulkAction::ArchiveRead` already combines a move with `$seen` in one patch.

## Design

### Config format

```toml
[ui]
preset = "mine"              # "fastmail", "vim", or a name under [presets]

[presets.mine]
base = "fastmail"
[presets.mine.keys.list]     # same syntax as today's [keys.<ctx>]
"e" = "archive"
"y" = "none"

[macros.file-receipt]
label = "File receipt"
keys = ["T"]                 # bound in the list and the reader
steps = ["mark-read", "unpin", { move = "Receipts/2026", id = "M123" }]
```

- `ui.preset` changes from a closed enum to a name, validated against built-ins plus `[presets]`. The four copies of the preset name mapping (`app/settings.rs:30,168,208`, `keys.rs:44`) become one function, so a new name can no longer save as `fastmail` silently.
- The top-level `[keys]` table keeps working and applies on top of any preset. When the key editor writes a key into a preset, it removes any `[keys]` entry for the same context and keys, so the edit takes effect. Duplicating the active preset moves the `[keys]` entries into the copy, so nothing changes in behaviour.
- A macro's keys live in the macro, not in a preset. They work with built-in presets too, and are applied last in the list and reader contexts.
  - Capturing a macro key runs the same conflict prompt against the full keymap that will be active. For example, `T` is "thread view" in fastmail.
  - When the keymap is built, a macro key that clashes with the active preset is skipped and kept as a warning, shown in the editor and by `mejl keys --check`. For example, `r` is a prefix in vim. It never fails the build, so switching preset or starting mejl cannot break on a macro key.
- A preset's stored diff is computed from its base plus its own overrides only, never from the effective keymap that also includes `[keys]` and macros.
- Steps: `mark-read`, `mark-unread`, `pin`, `unpin`, `mute`, `archive`, `trash`, `spam`, `{ move = ref }`, `{ label = ref }`, `{ unlabel = ref }`.
  - A folder ref is a path plus an optional id. The id is tried first, then the path (case-insensitive, same convention as `mailbox_path` in `update/overlays.rs:199`). This survives renames, as `MailboxRef` in `mejl-sieve` does.
  - At most one of move, archive, trash and spam per macro. At most 10 steps.
  - `delete-permanently` and snooze are not allowed.
- `"macros"` and `"presets"` are added to `TOP_KEYS` (`config.rs:388`). Unknown step names error with a "did you mean". `mejl keys --check` also checks custom presets and macros.

### Keymap core (`mejl-ui/src/keymap/`)

- `Entry` holds a `Binding` (`enum Binding { Action(Action), Macro(MacroId) }`, which is `Copy`; `MacroId` is a small index). It replaces the bare `Action`.
- New variants: `Resolution::Macro { id }` and `ContinuationTarget::Macro(id)`.
- `bindings()` returns `Binding`. Add `keys_for_macro`. The messages in `check()` name the binding. `keys_for(action)` keeps its signature.
- `Keymap` keeps the base `Preset` (for counts and prefix labels) and gains a display name.
- A new constructor builds a keymap from base, preset overrides, `[keys]` overrides and macros, in that order. It replaces the call sites of `with_overrides`.
- New pure helpers, unit tested:
  - `diff(base, edited) -> KeyOverrides`: the minimal binds and `none`s. Rule tested: base plus diff equals edited.
  - `lookup(contexts, seq)`: what a sequence does. It reports an exact match, "starts a longer binding" or "a shorter binding starts it".
  - Rebind, add-key, remove-key and swap operations on a keymap, so the editor has no keymap logic of its own.

### Key editor screen (new `Screen::Keys`)

- **Opening it:** Enter on the "Keys" row of settings, or the new palette action `edit-keys` ("Edit key bindings", Tools). Left and right on that row still cycle presets, now including custom ones.
- **Top line:** `Preset: mine (based on fastmail)`, with keys to duplicate, rename, delete or switch preset.
- **Sections** (Tab and Shift-Tab): Mail, Global, Compose, Search, Sidebar, Masked, Rules, Settings, Help, Macros.
  - Mail holds the actions bound to the same keys in both the list and the reader (the shared triage table), and an edit there writes both contexts.
  - Actions whose keys differ between the list and the reader show as "List only" and "Reader only" groups.
  - Rows are grouped by `Category`: label, keys (up to three, then `+N`), and the description of the focused row in the footer.
- **Keys** (a new `Context::Keys` table): Enter replaces a row's keys, `a` adds a key, `x` removes one (with a small picker when there are several), `u` resets the row to the base preset, `d` duplicates the preset, `/` filters.
- **Built-in presets are read-only.** An edit key there says "fastmail is built in. Press d to make your own copy."
- **Capturing a key** is a raw overlay (`Overlay::CaptureKey`), handled in `on_key` before the keymap (as the confirm overlay is, `update.rs:200`):
  - The prompt reads "Press the new key for Archive (Esc cancels)".
  - If the key so far starts existing bindings (for example `g` or `.`), the overlay waits for the next key. Otherwise the capture is done.
  - Esc cancels, so Esc itself can only be bound in the config file. Ctrl-Z can never be captured, because suspend runs first.
  - It shows `legacy_problem()` warnings.
- **Conflicts:**
  - An exact clash asks: "`y` is Archive. Swap / Replace / Cancel", like a game.
  - A prefix clash is refused with the reason.
  - Hiding a global key shows a warning only.
- **Conflict checks** run against every context that is active when the key fires: List, Reader and Global for a Mail row. Hiding a global key is a warning. Hiding a global prefix (for example archive on `g`, which would remove the go-to menu) goes through the same prompt.
- **Swap with a row that has several keys:** only the key being replaced moves.
- **Lockout guard:** the editor refuses to remove the last key of `back`, `help`, `palette`, `quit-now`, `go-settings` or any key in the `Keys` context.
- **Typing guard:** in Compose, Search and Help, a key that is not bound types into the field. So the editor refuses plain printable keys there and accepts only modified keys, F-keys, Tab and Enter.
- **Opening the editor from settings:** Enter is `setting-change` today. A new `setting-open` action, bound to Enter, opens the editor on the Keys row and acts as change on every other row. Space, left and right keep cycling.
- **Preset lifecycle:**
  - `base` is always a built-in preset. Duplicating a custom preset copies its base and overrides, so presets never chain.
  - Renaming the active preset updates `ui.preset`. Deleting it asks first, then falls back to its base.
- **Saving:** each edit sends `Cmd::SaveKeys(KeyEdit)`. The app computes the diff, rewrites `[presets.<name>.keys.*]` with `toml_edit`, rebuilds the keymap and sends `Msg::Config`. A refused edit sends `Msg::SettingRefused`, which also fixes today's stuck ", saving" row and the wrong "applies until you quit" toast (`app/mod.rs:994`).

### Macros

- **Model:** a new `mejl-ui/src/macros.rs` holds `Macro { id, label, keys, steps }` and `Step`. It is parsed in `crates/mejl/src/config.rs` and carried in `UiConfig`.
- **Macro editor:** the Macros section of the key editor.
  - `n` makes a new macro (asks for a name through `Overlay::Prompt`).
  - Enter opens a macro's steps.
    - `a` adds a step from a picker. For move and label it then opens the existing mailbox picker and stores the id and path.
    - `x` removes a step, and `K`/`J` move steps up and down.
    - `r` renames the macro, and `b` binds a key through the same capture overlay (in the Mail contexts).
  - Deleting a macro asks first.
  - Each change sends `Cmd::SaveMacro` or `Cmd::DeleteMacro`, written as `[macros.<id>]`.
- **Running a macro** (`update.rs`, new `run_macro`):
  - It works out the target once with `target()`: the selection, else the cursor conversation, else `AllInMailbox` after `* a`. Macros act on whole conversations, as archive does, also in the reader.
  - Folder refs are resolved against `self.mailboxes`. A missing folder gives an error toast ("File receipt: no folder Receipts/2026") and nothing runs.
  - It pushes one `Mutation::Batch { target, steps, label }`, clears the selection once, and shows one toast with undo: "File receipt: 3 conversations".
- **App plan** (`app/mutate.rs`): a new `plan()` arm applies each step to each email's `EmailFlags` in order.
  - The source mailbox carries forward from `source_of` through `BulkAction::patch`.
  - The archive step honours `ui.mark_read_on_archive`, which you asked to cover every archive path.
  - Each email gets one `EmailPatchTarget` from `before.diff(&after)`. The result is one `Plan::Queue`: one undo entry, one journal record, and snapshots for the optimistic overlay.
- **Whole mailbox (`* a`):** a new `BulkAction::Patch(MacroPatch)` in `mejl-sync/src/command.rs` (keywords, optional move, labels), built once and cloned to each email as the existing bulk path does.
  - `plan_bulk` accepts it.
  - `BulkOverlay` shows hide, seen and flagged at once. Mute and labels show after the store catches up, one round trip later.
- **Elsewhere:** Repeat (`.` in vim) can replay a macro, because `last_action` stores a `Binding`. The palette lists macros as "Macro: File receipt" with their key. Which-key and help show macros under a "Macros" heading.

## Work order

0. The draft-status fix (9 files) is still uncommitted. If you approve, I commit it first as "Show draft refusals on their own draft", so this work starts from a clean tree. I don't commit without your yes.
1. **Foundation**, done by me, everything else depends on it:
   - `Binding` through the keymap, which-key, help, palette, Repeat and `HitTarget`.
   - Config `[presets]` and `[macros]` parsing, the name-based preset, and the diff, lookup and edit helpers.
   - The writer paths in `app/settings.rs`, and the refused-save fix.
2. **Then, in parallel**, two streams in the shared tree (the draft-status fix is uncommitted, so no worktrees), with separate `CARGO_TARGET_DIR`s, and `Cargo.toml` edits only by me:
   - A: the key editor screen, the capture overlay and conflict prompts.
   - B: the macro run path only: `run_macro`, `Mutation::Batch`, the `plan()` arm, and the sync `BulkAction::Patch`. It stays out of A's files.
3. **After A lands:** the macro editor UI inside the key editor's Macros section.
4. **Docs:**
   - `README.md` config section.
   - `docs/terminal-ux.md`: the configurable keymaps section, plus a macros section that replaces the "macro-free flows" stance.
   - The `docs/README.md` status list and the Cmd table in `mejl-ui/src/lib.rs`.

## Critical files

- `crates/mejl-ui/src/keymap/{mod,config,preset}.rs`, `action.rs`, new `macros.rs`, new `keys_screen.rs` (state) and `view/keys.rs`
- `crates/mejl-ui/src/{update.rs,update/overlays.rs,update/screens.rs,state.rs,event.rs,settings.rs,view/overlays.rs,view/mod.rs}`
- `crates/mejl/src/{config.rs,keys.rs,app/settings.rs,app/mutate.rs,app/mod.rs}`
- `crates/mejl-sync/src/{command.rs,mutate.rs}`, `crates/mejl/src/app/overlay.rs` (`BulkOverlay`)

## Verification

- **Unit tests:**
  - Keymap: `Binding::Macro` feed and which-key; `diff` round trip on both presets; `lookup` for exact and prefix clashes; swap and replace.
  - Config: presets, macros, a bad step, an unknown preset name.
  - Writer: a preset keys table, a macro table, comments kept, duplicate moving `[keys]`.
- **UI tests** (`mejl-ui/tests`):
  - Key editor snapshots at 80x24, 120x36 and 160x40.
  - Capture flow: Enter, key, swap prompt.
  - The built-in read-only message, the lockout guard, and the macro editor flows.
  - Re-accepted settings, help and palette snapshots.
- **App tests** (`crates/mejl/tests/app.rs` with `mejl-fake`):
  - A macro on the cursor conversation sends one patch that combines `$seen`, `$flagged` and the move, and makes one undo entry.
  - `z` restores read state, pin and folder together, on a three-conversation selection too.
  - `* a` through the bulk patch.
  - A missing folder gives an error and no change.
  - Repeat, `mark_read_on_archive` inside a macro, and a rebind saved to `config.toml` that works without a restart.
- **`mejl-sync` test:** the `BulkAction::Patch` JSON.
- **Gate:** `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`, plus `cargo +1.88 check --workspace`.
- **App harness pitfall:** any save rebuilds `UiConfig`, which resets the harness's `undo_send` and `toast_time` overrides. Rebind tests must allow for that.
- **The real `config.toml` is never written.** Every test uses a temp dir. The manual pass runs `cargo run -- --demo --config <scratchpad>/config.toml`.
  - The demo copies that file into its own private dir at each start, so edits do not survive a restart.
  - Persistence is checked by reading the demo copy during the run, and by the writer tests.
- **Manual pass:** tmux at 80x24 and 160x40.
  - Duplicate fastmail, rebind archive to `e`, check the hint bar and help, then check the demo copy of the config.
  - Build "mark read, unpin, move to a folder", bind `T`, run it, then `z`.
  - Never send mail in this pass.
