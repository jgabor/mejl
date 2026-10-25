# Terminal UX and performance research

## Summary

Main findings, and what they mean for mejl:

1. **Modern terminals have converged on most features mejl needs, but tmux defaults hide them.** The kitty keyboard protocol is now implemented by kitty, Ghostty, foot, Alacritty, iTerm2, WezTerm (opt-in, off by default), Windows Terminal (1.25, stable in October 2026), Rio, Warp and others ([kitty docs](https://sw.kovidgoyal.net/kitty/keyboard-protocol/), [Windows Terminal 1.25 release](https://github.com/microsoft/terminal/releases)). Synchronized output (mode 2026) is near universal outside VTE ([spec and adoption list](https://github.com/contour-terminal/vt-extensions/blob/master/synchronized-output.md)). But stock tmux 3.8 ships with `extended-keys off`, `focus-events off`, `allow-passthrough off` and `set-clipboard external` (verified: [tmux options-table.c](https://github.com/tmux/tmux/blob/master/options-table.c)). **mejl must work fully with legacy key encoding, and should ship a recommended tmux snippet.**
2. **Capability detection is unreliable, so detect, cache, and let config override.** iTerm2 answers "supported" to every DECRQM query, even fictional modes. Konsole does not answer DECRQM at all (verified: [Quast 2025](https://www.jeffquast.com/post/state-of-terminal-emulation-2025/)). tmux answers mode 2026 from its own state, not the outer terminal's ([tmux #5470](https://github.com/tmux/tmux/issues/5470)). Use "query plus DA1 sentinel", identify the terminal with XTVERSION, batch all probes into one round trip, and cache the result.
3. **ratatui 0.30 is the right base, with known gaps.** ratatui 0.30.2 (June 2026) with crossterm 0.29 is current (verified: crates.io). Its `Terminal::draw` does not wrap frames in synchronized output, has no first-class OSC 8 hyperlinks (the official example is a workaround for open issue [#902](https://github.com/ratatui/ratatui/issues/902); per-cell hyperlinks PR [#2853](https://github.com/ratatui/ratatui/pull/2853) was closed unmerged on 2026-10-23), and has no hit-testing layer. mejl needs a thin layer of its own for all three.
4. **crossterm's `EnableMouseCapture` turns on any-motion tracking (mode 1003)** (verified: [crossterm 0.29 source](https://github.com/crossterm-rs/crossterm/blob/0.29/src/event.rs)). Every pointer move becomes an event. mejl should either send its own mode set (1000 + 1002 + 1006, no hover) or coalesce motion events hard.
5. **Fastmail's web shortcuts are a ready-made default keymap.** They are documented, single-key, mnemonic, and use prefix keys with follow-ups (`.` then `r`, `*` then `a`, `b` then `1` to `7`) (verified: [Fastmail keyboard shortcuts](https://www.fastmail.help/hc/en-us/articles/360058753534-Keyboard-shortcuts)). Recommendation: a non-modal, mnemonic, Fastmail-compatible default preset, plus a vim/aerc preset, with which-key popups for every prefix and a fuzzy command palette that shows each command's key.
6. **"Instant" means under 100 ms from key to finished frame, and mejl's own share of that should fit inside one frame (8 to 16 ms) for local actions, because the terminal itself already adds 5 to 45 ms.** The 0.1 s limit for "feels instantaneous" comes from [Nielsen](https://www.nngroup.com/articles/response-times-3-important-limits/), and Superhuman applies the same 100 ms rule to email ([Superhuman blog](https://blog.superhuman.com/superhuman-is-built-for-speed/)). That is only reachable if the UI thread never waits on the network or disk: render from a local cache first, apply every mutation optimistically, and push all JMAP work to background tasks.
7. **HTML mail should be converted to styled text with `html2text`, remote content blocked by default.** Fastmail proxies remote images only in its web and mobile apps (verified: [Fastmail help](https://www.fastmail.help/hc/en-us/articles/1500000278102-Blocking-remote-images)). A third-party client that loads them directly leaks the user's IP address. Links become numbered footnotes by default, with OSC 8 as an optional extra.
8. **Accessibility starts with cursor placement, then colour.** Screen readers follow the terminal cursor, so mejl should park the (hidden) cursor on the focused row ([blind.guru on brick](https://blind.guru/blog/2021-06-25-brick.html)). Never encode state by colour alone, honour `NO_COLOR` ([no-color.org](https://no-color.org/)), and default to the terminal's 16-colour palette so the user's theme, contrast choices and light/dark mode carry over.

## Scope and labels

Research date: 2026-10-23. Scope: what a keyboard-first, mouse-friendly, instant TUI email client for Fastmail can rely on in today's terminals, how comparable tools design input and layout, and how to build the render and event loop in Rust so it stays fast. The last section is a concrete UX spec sketch for mejl.

Labels used throughout:

- "(verified: source)" means a primary source or the project's own code or docs confirmed it on the research date.
- "(inferred)" means reasoned from verified facts, not directly confirmed.
- "(unverified)" means I could not confirm it. Treat it as a lead, not a fact.

## Terminal capabilities and protocols in 2026

### What each protocol does and how to detect it

| Feature | Sequence | What it gives mejl | Detection |
|---|---|---|---|
| Kitty keyboard protocol | `CSI > flags u` push, `CSI < u` pop, `CSI ? u` query | Unambiguous Esc, Ctrl+Shift combos, Ctrl+Enter, key release and repeat events. Flags: 1 disambiguate, 2 event types, 4 alternate keys, 8 all keys as escapes, 16 associated text (verified: [kitty docs](https://sw.kovidgoyal.net/kitty/keyboard-protocol/)) | Send `CSI ? u` then DA1. A DA1 reply with no flags reply means unsupported (verified: same) |
| xterm modifyOtherKeys / CSI u (legacy) | `CSI > 4 ; 2 m` | Fallback for some modified keys, mainly inside tmux with `extended-keys` (verified: [tmux CHANGES 3.5](https://github.com/tmux/tmux/blob/master/CHANGES)) | No reliable query (inferred) |
| SGR mouse | `CSI ? 1000 h` (press/release), `1002` (drag), `1003` (any motion), `1006` (SGR encoding) | Clicks, wheel, drag. 1006 removes the 223-column limit of the old X10 encoding (inferred from encoding design) | No query in practice. Assume available in all modern terminals (inferred) |
| Focus events | `CSI ? 1004 h` | Pause animations, refresh on focus, mark-as-read timers | None. Events either arrive or not |
| Bracketed paste | `CSI ? 2004 h` | Pasted text arrives as one `Paste(String)` event, so pasting an address list never fires shortcuts | None |
| Synchronized output | `CSI ? 2026 h` ... `CSI ? 2026 l` | No tearing on large redraws | DECRQM `CSI ? 2026 $ p`, reply `CSI ? 2026 ; Ps $ y`. Ps 1 or 2 means supported, 0 or 4 means not (verified: [spec](https://github.com/contour-terminal/vt-extensions/blob/master/synchronized-output.md)) |
| Grapheme clustering | Mode 2027 | Terminal measures ZWJ emoji sequences as one cluster | DECRQM 2027. Weak signal, see Unicode section |
| Theme change notification | Mode 2031, reply `CSI ? 997 ; 1 n` (dark) or `; 2 n` (light), query `CSI ? 996 n` | Switch light/dark theme live when the OS theme changes (verified: [Contour spec](https://github.com/contour-terminal/contour/blob/master/docs/vt-extensions/color-palette-update-notifications.md)) | Query `CSI ? 996 n` + DA1 |
| Colour query | OSC 10 / OSC 11 | Read actual fg/bg colour to decide dark vs light, and to pick readable colours for HTML mail | `terminal-colorsaurus` sends OSC 10/11 then DA1, with 1 s fallback timeout (verified: [docs.rs](https://docs.rs/terminal-colorsaurus/latest/terminal_colorsaurus/feature_detection/index.html)) |
| OSC 8 hyperlinks | `OSC 8 ; params ; URI ST` text `OSC 8 ; ; ST` | Clickable links handled by the terminal | **No detection method exists** (verified: [egmontkob gist](https://gist.github.com/egmontkob/eb114294efbcd5adb1944c9f3cb5feda)). Correct terminals ignore unknown OSC 8 and show the text |
| OSC 52 clipboard | `OSC 52 ; c ; base64 ST` | Copy URL, address or message id to the system clipboard, also over SSH | No reliable query. Write is often allowed, read usually prompts or is disabled |
| Notifications | OSC 9 (iTerm2), OSC 777 `notify` (urxvt), OSC 99 (kitty, richer) | New-mail notification without a desktop daemon, works over SSH | OSC 99 has a query (`p=?`) and click reporting (`a=report`) (verified: [kitty notifications](https://sw.kovidgoyal.net/kitty/desktop-notifications/)). OSC 9 and 777 have no query |
| Truecolor | SGR `38;2;r;g;b` | Exact theme colours | `COLORTERM=truecolor` or `24bit`, or `TERM` ending in `direct`/`truecolor` (verified: [supports-color source](https://github.com/zkat/supports-color/blob/main/src/lib.rs)) |
| Kitty graphics | APC `G` | Inline images with placement and deletion. Unicode placeholders make it work in tmux | Query action `a=q` (unverified detail). `ratatui-image` handles detection |
| Sixel | DCS `q` | Inline images, widest legacy support | DA1 attribute 4 (inferred, standard practice) |
| iTerm2 inline images | OSC 1337 `File=` | Inline images on iTerm2, WezTerm, Rio, Bobcat | Env vars (`TERM_PROGRAM`) mostly |
| XTVERSION | `CSI > q` | Identify terminal name and version, also over SSH. Supported by 21 terminals in ucs-detect's survey (verified: [Quast 2026](https://www.jeffquast.com/post/perfecting-terminal-character-width-using-correction-tables/)) | DCS reply or nothing |

### Support matrix

Versions are the latest stable releases found on 2026-10-23: kitty 0.49.2, Ghostty 1.3.1, WezTerm 20240203 (no newer stable release), Alacritty 0.17.0, Windows Terminal 1.25.2733.0, tmux 3.8, iTerm2 3.6.9 (verified: GitHub releases and tags, [terminfo.dev](https://terminfo.dev/) for iTerm2). "?" means I found no source; it does not mean "no".

| Terminal | Kitty keyboard | Sync 2026 | OSC 8 | OSC 52 write | Notifications | Images | Mode 2027 | Mode 2031 |
|---|---|---|---|---|---|---|---|---|
| kitty | yes, reference [k] | yes [s] | yes [t] | yes [t] | OSC 99, OSC 9 [kn] | kitty protocol. No sixel [a] | ? | yes, 0.38.1+ [c] |
| Ghostty | yes [k] | yes [s] | yes [t] | yes. Read prompts [g] | OSC 9 [t]. OSC 777 (unverified) | kitty protocol with Unicode placeholders. No sixel [a][r] | yes [q] | yes, 1.0+ [c] |
| WezTerm | **opt-in**, `enable_kitty_keyboard` defaults to false [w] | yes [s] | ? | ? | (unverified) | sixel, kitty, iTerm2. Only iTerm2 works without bugs in ratatui-image [r] | yes [q] | **no** [c] |
| foot | yes [k] | yes [s][f] | yes [f] | yes, copy and paste [f] | OSC 777 `notify` only [f] | sixel, 1.2.0+ [a] | yes [q] | ? |
| Alacritty | yes [k][al] | yes, 0.13+ [al] | yes, 0.11+ [al] | **copy only by default** (`OnlyCopy`) [ac] | no (unverified) | none [a] | ? | ? |
| iTerm2 | yes [k] | yes [s] | yes [e] | **off until "Applications in terminal may access clipboard" is enabled** [i] | OSC 9 [t] | iTerm2 protocol, kitty protocol, sixel 3.3.0+ [i2][a] | ? (answers yes to every DECRQM [q2]) | ? |
| Windows Terminal | yes, 1.25+ [wt] | yes [s] | yes (inferred: only versions up to 0.9 were listed as broken [e]) | yes, can be disabled via `compatibility.allowOSC52` [wt2] | not in 1.25 stable (inferred). The 2020 request was closed as completed in June 2026 via PR #20014 [wt3]; a third-party project reports OSC 777 only from Preview 1.26, opt-in (unverified) | sixel, 1.22+ [a] | yes [q] | ? |
| GNOME Terminal (VTE) | no (inferred: not in kitty's list) | **no**, "not yet" [s] | yes [e] | **no** (unverified, many downstream bug reports [v]) | ? | none. VTE's sixel setting is inert in stable releases [a] | no [q] | yes, VTE 0.82+ [c] |
| Konsole | ? | unknown [s] | ? | ? | ? | sixel, 22.04+ [a]. Glitches with ratatui-image [r] | no [q] | ? |
| tmux 3.8 | **no**. Forwards CSI u when `extended-keys on` and `extended-keys-format csi-u` (3.5+) [tm] | yes for apps, 3.7+, but DECRQM answer ignores the outer terminal [tm][5470] | yes, 3.4+ [tm] | only with `set-clipboard on`. Default `external` ignores apps [tm1] | no. Request closed in June 2026 as "added to todo list"; no entry in the 3.8 CHANGES [tm5136] | sixel only if built with `--enable-sixel`. kitty graphics needs `allow-passthrough on` [tm][a] | ? | yes, 3.6+ [tm] |
| Zellij | yes, `support_kitty_keyboard_protocol` option, with open bugs [z] | yes [s] | ? | ? | ? | sixel, 0.31.0+ [a] | ? | yes, 0.44.2+ [c] |
| GNU screen | ? | ? | buggy with URLs over 700 chars [e] | ? | ? | ? | ? | ? |

Mouse 1006, focus 1004 and bracketed paste 2004 are not in the table because every app terminal on terminfo.dev supports all three (iTerm2, Ghostty, kitty, VS Code, Warp, Cursor, Terminal.app) (verified: [terminfo.dev](https://terminfo.dev/)). For the other rows they are (unverified) but very likely. tmux needs `focus-events on` to forward focus events (verified: [tmux.1](https://github.com/tmux/tmux/blob/master/tmux.1)).

Sources: [k] [kitty keyboard protocol page](https://sw.kovidgoyal.net/kitty/keyboard-protocol/) list of implementers. [s] [synchronized output adoption list](https://github.com/contour-terminal/vt-extensions/blob/master/synchronized-output.md). [t] [terminfo.dev](https://terminfo.dev/), automated probes on macOS app terminals; for headless backends (Alacritty, WezTerm) it only shows that the parser accepts a sequence, so I did not use those rows. [kn] [kitty notifications](https://sw.kovidgoyal.net/kitty/desktop-notifications/). [a] [arewesixelyet.com](https://www.arewesixelyet.com/). [c] [Contour mode 2031 adoption table](https://github.com/contour-terminal/contour/blob/master/docs/vt-extensions/color-palette-update-notifications.md). [g] [Ghostty config reference](https://ghostty.org/docs/config/reference). [r] [ratatui-image compatibility table](https://github.com/benjajaja/ratatui-image). [q] [Quast 2026](https://www.jeffquast.com/post/perfecting-terminal-character-width-using-correction-tables/). [q2] [Quast 2025](https://www.jeffquast.com/post/state-of-terminal-emulation-2025/). [w] [WezTerm docs](https://wezterm.org/config/lua/config/enable_kitty_keyboard.html). [f] [foot README](https://codeberg.org/dnkl/foot). [al] [Alacritty CHANGELOG](https://github.com/alacritty/alacritty/blob/master/CHANGELOG.md). [ac] [Alacritty config](https://alacritty.org/config-alacritty.html). [e] [OSC 8 gist](https://gist.github.com/egmontkob/eb114294efbcd5adb1944c9f3cb5feda). [i] [iTerm2 preferences](https://iterm2.com/3.5/documentation-preferences-general.html). [i2] [iTerm2 images doc](https://iterm2.com/documentation-images.html). [wt] [Windows Terminal 1.25 stable coverage](https://9to5windows.com/windows-terminal-125-stable-kitty-protocol-memory-leaks-fixes/) and [4sysops](https://4sysops.com/archives/windows-terminal-preview-125-kitty-protocol-settings-search-and-gui-for-key-bindings/). [wt2] [Windows Terminal 1.22 release notes](https://github.com/microsoft/terminal/discussions/18516). [wt3] [microsoft/terminal #7718](https://github.com/microsoft/terminal/issues/7718) and [PR #20014](https://github.com/microsoft/terminal/pull/20014). [v] for example [pi #7761](https://github.com/earendil-works/pi/issues/7761). [tm] [tmux CHANGES](https://github.com/tmux/tmux/blob/master/CHANGES). [tm1] [tmux.1](https://github.com/tmux/tmux/blob/master/tmux.1) and options-table.c. [5470] [tmux #5470](https://github.com/tmux/tmux/issues/5470). [tm5136] [tmux #5136](https://github.com/tmux/tmux/issues/5136). [z] [zellij #5191](https://github.com/zellij-org/zellij/issues/5191).

### Detection strategy for mejl

1. **One probe burst at startup, before the first frame is flushed.** Write all queries in one `write()`: `CSI ? u` (kitty keys), `CSI ? 2026 $ p`, `CSI ? 2027 $ p`, `CSI ? 996 n` (theme), OSC 11 (background), `CSI > q` (XTVERSION), then DA1 `CSI c` last as the sentinel. Terminals process sequences in order and almost all answer DA1, so when the DA1 reply arrives, anything still missing is unsupported (verified technique: [kitty docs](https://sw.kovidgoyal.net/kitty/keyboard-protocol/), [colorsaurus](https://docs.rs/terminal-colorsaurus/latest/terminal_colorsaurus/feature_detection/index.html)).
2. **Budget for slow responders.** terminal-colorsaurus measured median OSC 11 round trips of about 25 to 200 µs on fast terminals, but about 3 ms (kitty, WezTerm) up to about 26 ms (VS Code) on slow ones (verified, read from charts: [colorsaurus latency](https://docs.rs/terminal-colorsaurus/latest/terminal_colorsaurus/latency/index.html)). Over SSH add the network RTT. So: render the first frame from cache using last run's cached capabilities, and apply the fresh probe result when it lands (inferred).
3. **Cache by terminal identity.** Key the cache on XTVERSION string, `TERM`, `TERM_PROGRAM` and whether `TMUX`/`ZELLIJ` is set. `TERM_PROGRAM` is unreliable over SSH (verified: Quast 2026).
4. **Distrust positive answers from known liars.** iTerm2 claims every DECRQM mode, Konsole answers none, tmux answers 2026 for itself (verified: sources above). As built, the quirks table (`terminal/quirks.rs`) is keyed by `TERM` and `TERM_PROGRAM` and changes only how the probe runs: `TERM=dumb` and `eterm*` skip it, VS Code gets a 400 ms timeout. There is no XTVERSION liar table yet.
5. **Config always wins.** Every capability gets a `auto | on | off` setting (`keyboard.kitty`, `render.sync`, `links.osc8`, `clipboard.osc52`, `images`, `notifications`).
6. **Degrade without loss of function.** Every action must have a binding that works with legacy encoding. Kitty keys only add comfort (Ctrl+Enter to send, Ctrl+Shift combos, Esc without delay).

Legacy-encoding traps that the default keymap must avoid (inferred from how C0 control codes map): `Ctrl-I` is Tab, `Ctrl-M` is Enter, `Ctrl-H` is Backspace on many terminals, `Ctrl-[` is Esc, and `Alt-x` arrives as Esc followed by `x`. Ctrl+Shift+letter is usually indistinguishable from Ctrl+letter. Without the disambiguate flag, a lone Esc needs a timeout to tell it from the start of a sequence; tmux adds its own `escape-time` (default 10 ms, verified: options-table.c).

### Unicode width pitfalls

- **Terminals disagree on width.** Across about 35 terminals, ucs-detect found 23 implementations that differ on wide codepoints, 19 on ZWJ emoji sequences, and 6 to 7 on VS15/VS16 handling. Mode 2027 support does not predict correct widths. Multiplexers can disagree with the host terminal: Zellij measured width 1 where Ghostty drew width 2, corrupting its border (verified: [Quast 2026](https://www.jeffquast.com/post/perfecting-terminal-character-width-using-correction-tables/)).
- **ratatui uses `unicode-width` per grapheme**, with a fast path for ASCII and a fix-up for halfwidth katakana sound marks, and its buffer diff has special handling for VS16 trailing cells (verified: [ratatui-core cell_width.rs and diff.rs](https://github.com/ratatui/ratatui/tree/main/ratatui-core/src/buffer)). It cannot know what the actual terminal will do.
- **Email is full of hostile text.** Subjects contain emoji, flags, ZWJ families, CJK, RTL scripts, zero-width joiners, and invisible tracking characters. Practical rules for mejl (inferred):
  - Truncate by grapheme cluster and display width, never by `char` or byte.
  - Strip or replace control and bidi override characters (U+202A to U+202E, U+2066 to U+2069) in subjects and names before rendering. They can reorder the list row visually and are a known phishing trick.
  - Clamp each grapheme to at most 2 cells, matching wcwidth's rule since 0.8.0 (verified: Quast 2026).
  - Leave a one-cell guard before column borders, so a mismeasured emoji spills into padding rather than over a border.
  - Provide `ui.ascii_glyphs = true` and a "safe glyphs" default.
- **UI glyphs need care too.** Many common symbols are East Asian "Ambiguous" width, which CJK locales and some terminal settings render as 2 cells. Checked with Python's `unicodedata` (Unicode 16.0): ● ○ • ★ ☆ ▶ ▼ … and all box drawing characters (│ ─ ╭) are Ambiguous. ◦ ⚑ ⚐ ✓ ✗ ▸ ▾ ↩ ↪ ⋯ are Neutral. 📎 is Wide (verified: local `unicodedata.east_asian_width` run). Recommendation: pick indicator glyphs from the Neutral set, never emit VS16 after them, and offer an ASCII set (`*`, `!`, `@`, `>`).

### tmux, zellij, screen and SSH caveats

| Concern | What happens | What mejl does |
|---|---|---|
| Keys in tmux | Default `extended-keys off`, so Ctrl+Enter, Ctrl+Shift combos and Esc disambiguation are lost. `extended-keys-format` defaults to `xterm`; `csi-u` exists since 3.5 (verified: options-table.c, CHANGES) | Full legacy-safe keymap. Docs snippet: `set -g extended-keys on`, `set -g extended-keys-format csi-u` |
| Focus in tmux | `focus-events off` by default (verified) | Do not rely on focus for correctness. Snippet: `set -g focus-events on` |
| Clipboard in tmux | `set-clipboard external` (default) "will attempt to set the terminal clipboard but ignore attempts by applications" (verified: tmux.1) | Snippet: `set -g set-clipboard on`. Fallback: `arboard` when running locally with a display server |
| Sync output in tmux | Supported for apps since 3.7, but the DECRQM reply reflects tmux, not the outer terminal ([#5470](https://github.com/tmux/tmux/issues/5470)) | Fine to use: tmux itself redraws atomically to the outer terminal when it can (inferred) |
| Images in tmux | kitty graphics needs `allow-passthrough on` (default off) and Unicode placeholders; sixel needs a tmux built with `--enable-sixel` (verified) | Default to text placeholders inside tmux. Images are opt-in |
| Hyperlinks in tmux | OSC 8 supported since 3.4 (verified) | OK |
| Zellij | Kitty keyboard support has open bugs with Alt+Shift (verified: [#5191](https://github.com/zellij-org/zellij/issues/5191)); width can mismatch the host (verified: Quast) | Same legacy-safe keymap. Guard cells |
| GNU screen | Old, limited. Corrupts on long OSC 8 URLs (verified: OSC 8 gist) | Detect `STY`, disable OSC 8 and images |
| SSH | Extra RTT on every probe; `TERM_PROGRAM` often missing; OSC 52 is the only clipboard path; notifications via OSC work, desktop D-Bus does not | Cached caps, generous probe timeout, OSC 52 + OSC 9/99/777 preferred over `notify-rust` when `SSH_TTY` is set |

## Input design

### What comparable tools do

| Tool | Model | Discoverability | Notable ideas for mejl |
|---|---|---|---|
| Fastmail web | Non-modal single keys, mnemonic, a few prefix keys | `?` shows shortcuts for the current screen. Tooltips show keys (verified: [Fastmail shortcuts](https://www.fastmail.help/hc/en-us/articles/360058753534-Keyboard-shortcuts)) | `j/k`, `o`/Enter, `u` back, `c` compose, `r` reply, `a` reply all, `f` forward, `y` archive, `d` trash, `!` spam, `m`/`v` move, `l` label, `x` select, `X` range, `s` pin, `z` undo, `/` search, `g` go to folder, `e` expand, `"` toggle quotes, `;` actions menu, `:` details, `[`/`]` archive and go to next/previous. Prefixes: `.` then `r`/`u`/`p`/`n`, `*` then `a`/`n`, `b` then `1` to `7` |
| aerc | Non-modal, vim-flavoured, two-key chords, `:` command line | Man pages, `:help keys` (unverified) | `rr` reply all, `Rr` reply, `rq`/`Rq` quoted, `zc`/`zo` fold threads, `T` toggle threads, `v` mark, `<Space>` mark and next, `J`/`K` next/previous folder, `\` filter vs `/` search, `\|` pipe (verified: [aerc binds.conf](https://github.com/rjarry/aerc/blob/master/config/binds.conf)). aerc also supports the kitty keyboard protocol (verified: kitty list) |
| neomutt | Non-modal, single keys, heavy use of macros | `?` help per menu (unverified detail) | Index/pager split, sidebar. Macros are the escape hatch, which mejl should make unnecessary |
| lazygit | Non-modal, panels, per-panel meaning of keys | `?` opens a filterable keybindings menu for the current panel; bottom bar shows hints (verified: [lazygit keybindings](https://github.com/jesseduffield/lazygit/blob/master/docs/keybindings/Keybindings_en.md)) | Same key means different things per panel (`d` discard vs drop). Menus filter with `/` |
| helix | Modal, selection first, minor modes (`g` goto, `m` match, `z` view, `Space`, `Ctrl-w` window) | Info boxes after a prefix, controlled by `auto-info` (default true) (verified: [helix keymap](https://docs.helix-editor.com/keymap.html), [editor config](https://docs.helix-editor.com/editor.html)) | Prefix key plus popup is the which-key pattern. Select then act maps well to "select messages, then archive" |
| yazi | Vim-like, chords such as `c` then `d` | `F1` or `~` opens help (verified: [yazi quick start](https://yazi-rs.github.io/docs/quick-start/)) | Which-key style popup for chords (unverified detail) |
| k9s | `:` command mode with resource names and aliases, `/` filter with regex, fuzzy and inverse | `?` shows active mnemonics, `Ctrl-a` lists aliases (verified: [k9s commands](https://k9scli.io/topics/commands/)) | Command bar as navigation: `:inbox`, `:rules`, `:masked` |
| btop | Mouse plus keys | "All buttons with a highlighted key are clickable" (verified: [btop README](https://github.com/aristocratos/btop)) | Show the key letter on every clickable label, so mouse use teaches keys |
| zellij | Modal (mode keys), with an "Unlock-First (non-colliding)" preset since 0.41 (verified: [zellij presets](https://zellij.dev/documentation/keybinding-presets.html)) | Status bar shows the keys of the current mode | Shipping named presets is a clean way to serve two audiences |
| gitui | Non-modal, bottom command bar listing keys for the focused panel (unverified detail) | Always visible hint bar | Hint bar that changes with focus |
| Superhuman | Non-modal single keys, `Cmd+K` palette for everything | Palette shows each command's shortcut (unverified: third-party write-up [blakecrosley.com](https://blakecrosley.com/guides/design/superhuman)) | Palette as the teaching tool. 100 ms rule (verified: [Superhuman blog](https://blog.superhuman.com/superhuman-is-built-for-speed/)) |

### Modal vs non-modal

Email triage is mostly "move, then act on the current item". There is no text to edit in the list or reader, so a vim-style normal/insert split brings no benefit there (inferred). Fastmail, Gmail-style clients, neomutt and aerc are all effectively non-modal in their message views. The only real "insert" contexts are compose, search input, the palette and the Sieve editor, and those are focus-based text fields, not global modes.

**Recommendation:** non-modal, focus-based. A text field that has focus swallows printable keys; Esc leaves it. No global mode indicator is needed except a visible "selection: 12" counter when a multi-selection is active.

### Key styles

- **Mnemonic single keys** (Fastmail, Gmail, Superhuman, neomutt): fastest for frequent actions, easy to learn. Collisions appear quickly once there are more than about 30 actions (inferred).
- **Vim-style** (aerc, helix, yazi): `j/k`, `gg`/`G`, `Ctrl-d`/`Ctrl-u`, counts, `z` fold prefix. Familiar to the target audience of a TUI mail client (inferred).
- **Emacs-style** (`Ctrl-n`/`Ctrl-p`, `Ctrl-x` prefixes): good inside text fields, poor for triage because every action needs a modifier (inferred). mejl's body editor follows the Emacs-like table that `ratatui-textarea` ships (verified: [ratatui-textarea README](https://github.com/ratatui/ratatui-textarea)).

Where Fastmail and aerc/vim collide:

| Key | Fastmail | aerc / vim | mejl default (fastmail preset) | vim preset |
|---|---|---|---|---|
| `a` | reply all | archive | reply all | archive |
| `y` | archive | (vim: yank) | archive | copy (yank) menu |
| `d` | move to trash | delete with confirm | trash, undoable, no confirm | trash |
| `g` | go to folder | `gg` top (vim) | `g` prefix: `g i` inbox, `g g` top, ... | same |
| `r` | reply (or list) | `rr` reply all (prefix) | reply | `r` prefix like aerc |
| `j/k` in reader | next/previous conversation | scroll | next/previous conversation | scroll |
| `n/p` | next/previous message in conversation | next/previous search result | as Fastmail | `n`/`N` search results |
| `m` | move | compose (aerc) | move | move |

### Leader keys, chords and the timeout problem

Two-key sequences come in two kinds:

- **Prefix with a popup** (Fastmail `.` then `r`, helix `g`, `Space`): the first key has no action of its own, so there is no ambiguity and no timeout. A which-key popup lists the follow-up keys (helix's info boxes, verified via its `auto-info` option; the popup delay in helix and Neovim's which-key is unverified).
- **Overlapping chords** (vim `g` vs `gg`, aerc `r` vs `rr`): the first key is also a command, so the app must wait for a timeout. That adds latency to the single-key command and breaks the "instant" goal (inferred).

**Recommendation:** only non-overlapping prefixes. A prefix key never does anything on its own. Prefixes in the default preset: `g` (go), `.` (mark: read, unread, pin, unpin, Fastmail-compatible), `*` (select all/none, Fastmail-compatible), `b` (snooze, Fastmail-compatible), `z` reserved for view/fold toggles in the vim preset only, and `Space` as a leader for mejl-specific tools (rules, masked email, account) in the vim preset.

### Command palette

- Fuzzy over every command, every mailbox, every saved search, and recent contacts ("Go to: Receipts", "Rule: new from this sender", "Masked email: new for example.com"). Each row shows its key binding, so palette use teaches shortcuts (the Superhuman pattern; inferred value).
- Open with `:` and `Ctrl-k` (and `Ctrl-p` as an alias). `:` matches k9s and vim. Fastmail uses `:` for "toggle message details" (verified: Fastmail shortcuts); mejl moves that to `H` (headers), following aerc's `H` toggle-headers (verified: aerc binds).
- Matcher: `nucleo` (helix's matcher; 0.5.0, last release 2024-04) or `frizbee` (SIMD Smith-Waterman, typo tolerant) (verified: crates.io). At palette scale (a few thousand items) any of them is far below 1 ms per keystroke (inferred).
- Rank by fuzzy score plus frecency of the user's own command use (inferred).

### Which-key hints and discoverability

Layers, from always-on to on-demand:

1. **Hint bar** (one line, bottom): 5 to 8 most useful keys for the focused pane, each clickable. Hidden with `ui.hints = false`. Changes with focus, like lazygit and gitui.
2. **Prefix popup**: after a prefix key, a small box lists the follow-up keys. Escape cancels.
3. **`?` help overlay**: all bindings for the current context, filterable with typing, grouped by category. Matches Fastmail's `?` (verified) and lazygit's filterable menu (verified).
4. **Palette**: every command, searchable by name, with its key.
5. **Actions menu** (`;`, as in Fastmail, and right-click): context actions for the focused message.

### Configurable keymaps

- TOML file with `[keys.list]`, `[keys.reader]`, `[keys.compose]`, `[keys.global]` sections, each mapping a key string to a command name. Same command names as the palette, so users can discover a name in the palette and bind it (inferred).
- Named presets (`preset = "fastmail" | "vim"`), with user overrides on top, like zellij's presets (verified that zellij ships presets).
- Key parsing: `crokey` (1.5.1) parses and formats key combos such as `ctrl-k` for crossterm events (verified: crates.io; API detail unverified). A small in-house parser is also realistic.
- `mejl keys --check` reports conflicts and keys that legacy encoding cannot deliver (for example `ctrl-shift-x` or `ctrl-i` distinct from Tab) (inferred).

### Counts, repetition and macro-free flows

- Counts (`5j`) are cheap to support for motions only. They conflict with digit shortcuts, so digits are counts only in the vim preset; in the Fastmail preset digits are unused outside prefixes (`b` then `1` to `7`) (inferred).
- Bulk work goes through selection, not counts or macros: `x` toggle, `X` range (verified: Fastmail), `*` `a` all in view, `*` `n` none, plus "select all matching search" which applies to the whole JMAP query result, not just loaded rows (inferred, JMAP `Email/query` gives ids for the full result).
- `.` repeat-last-action is useful in vim muscle memory (aerc binds `.` to `:repeat`, verified) but collides with Fastmail's `.` prefix. In the vim preset `.` repeats; in the Fastmail preset `.` is the mark prefix.
- Neomutt-style macros are mostly used to chain "mark, move, sync". mejl should cover those with first-class commands: "archive and next" (`[`/`]`, Fastmail), "move to X and next", "create rule from this message", "mask this sender" (inferred).

### Recommended keymap philosophy

1. Non-modal, focus-based. Text fields own printable keys; Esc always backs out one level.
2. Single mnemonic keys for the 20 most frequent actions, taken from Fastmail's web shortcuts so Fastmail users have zero relearning.
3. Non-overlapping prefixes with which-key popups. No timeouts on any binding.
4. Everything is a named command, reachable from the palette, bindable in config.
5. Every default binding works with legacy encoding. Kitty-only keys (Ctrl+Enter, Ctrl+Shift) are alternates, never the only way.
6. Two presets: `fastmail` (default) and `vim` (aerc-like). User overrides on top.
7. Destructive actions are undoable (`z`, as in Fastmail) instead of confirmed.

## Mouse as a convenience

### What to support

| Gesture | Behaviour | Notes |
|---|---|---|
| Left click on list row | Focus and select that row; preview in reader pane if visible | Single click must never be destructive |
| Double click on row | Open full reader (narrow layout) or move focus to reader | Double-click detection is app-side: two presses within about 400 ms on the same cell (inferred; crossterm reports raw presses) |
| Click on checkbox column | Toggle selection (same as `x`) | Shift+click range is often eaten by the terminal for selection (see below), so offer Ctrl+click or a "range from last" action (inferred) |
| Wheel | Scroll the pane under the pointer, not the focused pane | 3 rows per notch is a common default (unverified). Coalesce bursts into one scroll per frame |
| Drag splitter | Resize panes, store ratio in state file | Needs mode 1002 (button-event tracking) |
| Click sidebar mailbox | Switch mailbox | |
| Click hint bar entry | Run that command | btop's "highlighted key is clickable" pattern (verified) |
| Click link footnote marker or URL | Open with `open` crate (5.4.5) or copy | OSC 8 links are handled by the terminal itself, usually with Ctrl or Cmd click (unverified per terminal) |
| Right click | Context actions menu, same content as `;` | Small, keyboard navigable, and it shows each action's key. Keeps parity between mouse and keyboard users (inferred) |
| Hover | Off by default | Needs mode 1003, which floods events. Optional `mouse.hover = true` for tooltips on truncated subjects |

### Terminal realities

- **Mouse capture steals text selection.** Most terminals let the user bypass capture by holding Shift: kitty documents "You can select text with kitty even when a terminal program has grabbed the mouse by holding down the Shift key" (verified: [kitty overview](https://sw.kovidgoyal.net/kitty/overview/)); Ghostty's `mouse-shift-capture` defaults to false so Shift is not sent to the app and extends the selection (verified: [Ghostty config](https://ghostty.org/docs/config/reference)). Others (unverified). mejl should state this in `?` help and offer `mouse = false` for users who prefer native selection. Also offer `y` copy actions so selection is rarely needed.
- **crossterm's `EnableMouseCapture` enables 1000, 1002, 1003, 1015 and 1006** (verified: [crossterm 0.29 event.rs](https://github.com/crossterm-rs/crossterm/blob/0.29/src/event.rs)). 1003 reports every motion. mejl should write its own enable sequence (`?1000h ?1002h ?1006h`) unless hover is on, and still coalesce `Moved`/`Drag` events to one per frame (inferred).
- **tmux** needs `set -g mouse on` to forward mouse events to apps (unverified default; widely documented).

### Hit testing in ratatui

ratatui is immediate mode and has no widget tree, so it has no built-in hit testing (verified: ratatui 0.30 highlights list no mouse APIs). The standard approach (inferred, and the model that `ratatui-layout` formalises):

1. During `draw`, every interactive element records `(Rect, TargetId, z)` into a frame-local `Regions` list.
2. After the frame, keep that list as "last frame's hit map".
3. On a mouse event, walk the list from top z down and return the first `TargetId` whose `Rect` contains the point. Translate it into the same `Action` a key would produce.
4. For virtual lists, store the list area plus the scroll offset; row index = `offset + (y - area.y)`. No per-row regions needed.

`ratatui-layout` 0.0.1-alpha.0 (September 2026, by joshka, in the ratatui repo) provides `Regions::hit_test`, `PointerTargets`, `FocusTargets` and `VirtualList` with exactly this model, but it is explicitly "extremely experimental" (verified: [docs.rs/ratatui-layout](https://docs.rs/ratatui-layout)). Recommendation: write the 100 lines in-house now, watch `ratatui-layout`, adopt later if it stabilises.

## Layout patterns for email

### Pane models

| Model | Used by | Strengths | Weaknesses |
|---|---|---|---|
| Three-pane (sidebar, list, reader) | Thunderbird, Apple Mail, Outlook, aerc with sidebar (unverified detail for desktop apps) | Everything visible, mouse friendly | Needs about 160+ columns to give the reader 72+ columns (inferred) |
| Two-pane (list + reader, sidebar as overlay or toggle) | Fastmail web with reading pane (unverified), Mimestream (unverified) | Good on 100 to 160 columns | Mailbox switching is one step away |
| Single pane with a stack (list, then reader, then back) | neomutt index/pager, Superhuman (unverified), mobile apps | Works on 80 columns and in tmux splits, full width for reading | No preview while triaging |

**Recommendation:** one layout engine with breakpoints on terminal width (inferred thresholds, to be tuned with real use):

| Width | Layout |
|---|---|
| 160 columns or more | Three panes: sidebar 20 to 24, list about 40 percent, reader rest |
| 100 to 159 | Two panes: list and reader. Sidebar opens as an overlay on `g` or click on the mailbox title |
| under 100 | Single pane stack. Enter opens reader full width, Esc or `u` goes back |
| height under 20 rows | Hide header block in reader; one-line hint bar becomes optional |

The user can force a layout (`layout = "auto" | "three" | "two" | "stack"`) and toggle the reader pane with a key. Pane ratios are draggable and persisted.

### Threads and conversations

- Fastmail is conversation-centric: shortcuts act on "conversations" and `n/p` move between messages inside one (verified: Fastmail shortcuts). JMAP exposes `threadId` on every Email and `Email/query` supports `collapseThreads` (verified: [RFC 8621](https://www.rfc-editor.org/rfc/rfc8621.html), detail of option name unverified here; see the JMAP doc).
- **Default: one row per conversation** in the list, with a count badge (`3`) and the newest message's date. The reader shows the conversation as a stack of messages: read ones collapsed to one line (sender, date, first line), unread and the latest expanded. `e` expands/collapses the focused message, `E` all (verified: Fastmail `e`, `Shift+e`).
- **Optional: tree view** (aerc `T` toggles threads, `zc/zo` fold, verified) for mailing lists, drawn with indentation rather than box-drawing art to avoid Ambiguous-width glyphs.

### Row design and density

One list row, left to right (inferred design):

```
▸ ◦ ⚑  Ada Lovelace (3)     Re: Engine notes for Thursday   quick question...   14:02
```

- Selection marker, unread marker, pinned marker, sender (or recipients in Sent), thread count, subject, dimmed preview (JMAP `preview` property), right-aligned date.
- Unread is shown by **bold text and a glyph**, never colour alone. Pinned (Fastmail's name for flagged) by a glyph.
- Densities: `compact` (one line, no preview), `comfortable` (one line with preview, default), `relaxed` (two lines: sender + date, subject + preview).
- Relative dates: `14:02` today, `Tue` this week, `Oct 3` this year, `2024-10-03` older. Absolute timestamp in the reader header and on hover. Use a fixed-width date column so rows do not jitter (inferred).
- Sender avatars as initials: a 2-cell coloured badge (`AL`) is cheap and helps scanning in the relaxed density. Colour derived from a hash of the address, but the letters carry the meaning, so it is still readable without colour (inferred). Real avatar images are out of scope by default.

### Quoted text and signatures

- Plain text quoting: lines starting with `>`; the quote depth is the count of leading `>` (verified: [RFC 3676 section 4.5](https://www.rfc-editor.org/rfc/rfc3676.html)). Respect `format=flowed` when reflowing (verified: RFC 3676 section 4).
- Signature: the line `-- ` (dash dash space) starts the signature (verified: RFC 3676 section 4.3).
- HTML quoting: `<blockquote>` and vendor markers such as Gmail's `gmail_quote` class (unverified list of markers; build a small heuristic set and test it on real mail).
- Default: fold quoted blocks after the first 3 lines with a one-line marker `⋯ 42 quoted lines (")`, and fold signatures entirely. `"` toggles quotes (verified: Fastmail uses `"`). Mouse click on the marker toggles it too.

## Rendering HTML email in a terminal

### Approaches

| Approach | Used by | Pros | Cons |
|---|---|---|---|
| External filter (`w3m -dump`, `lynx`) | aerc's default `text/html` filter uses w3m, optionally sandboxed with `unshare` or `socksify` (verified: [aerc-tutorial(7)](https://man.archlinux.org/man/aerc-tutorial.7.en)), neomutt via mailcap (unverified detail) | Mature table layout | Extra process per message, extra dependency, plain ANSI output that is hard to make interactive |
| In-process `html2text` | Many Rust TUIs (inferred) | Pure Rust, gives structured output with annotations, fast, no subprocess | Table layout is simpler than w3m; CSS support is basic |
| Prefer `text/plain` alternative | aerc can be configured to prefer it (verified: [aerc docs](https://aerc-docs.com/guides/filters/)) | Cleanest for human mail | Many senders put junk or nothing in the plain part |

**Recommendation:** `html2text` in rich mode, in-process, on a worker thread, with a per-message choice: use `text/plain` when present and not obviously degenerate, else HTML; one key toggles between the two (inferred).

`html2text` 0.17.4 (2026-10-18) offers (verified: [crate source lib.rs and text_renderer.rs](https://github.com/jugglerchris/rust-html2text)):

- `config::rich()` producing lines tagged with `RichAnnotation` variants: `Default`, `Link(String)`, `Image(String)`, `Emphasis`, `Strong`, `Strikeout`, `Code`, `Preformat(bool)`, `Colour`, `BgColour`. These map directly to ratatui `Style`s and to link and image targets.
- Config: `link_footnotes(bool)`, `max_wrap_width`, `min_wrap_width`, `allow_width_overflow`, `no_table_borders`, `fence_tables`, `no_link_wrapping`, `use_doc_css`, `add_css`, `add_agent_css`, `empty_img_mode`, `raw_mode`, `render_to_lines`.
- Recent fixes include an infinite loop on ill-formed CSS (0.17.4) and several rowspan/colspan panics (0.17.2). Hostile HTML is normal in email, so run conversion with a time and size limit and catch panics (inferred).

### Pipeline

1. Pick the body part (JMAP `htmlBody` / `textBody`). Fetch body values in the background; prefetch neighbours.
2. **Sanitise first**: drop `<script>`, `<style>` beyond what CSS support needs, forms, iframes. `ammonia` 4.2.4 is a widely used sanitiser (19M downloads) (verified: crates.io). For streaming rewrites (for example rewriting or stripping remote URLs before handing HTML to a browser), `lol_html` 3.0.2 (verified: crates.io).
3. **Remove tracking images**: images with width or height 0 or 1, `display:none`, or known tracker hosts render as nothing (inferred heuristic). Count them and show "2 trackers blocked" in the header.
4. Convert with `html2text::config::rich()` at the reader width (cap at about 100 columns for readability; inferred).
5. **Tables**: most tables in commercial mail are layout tables (nested grids for columns and spacing), while human mail and receipts contain a few real data tables. Render layout tables without borders (`no_table_borders`) so they collapse into plain flowing text. For data tables that do not fit the reader width, either let them overflow (`allow_width_overflow`) and scroll horizontally in the reader, or fence them as preformatted blocks (`fence_tables`). A simple heuristic to tell them apart: nesting depth, presence of `<th>`, and cell count. These html2text options apply to a whole conversion, so a per-table choice needs a pre-pass that marks or rewrites tables before conversion (inferred; test on a corpus).
6. Map annotations to styles. Cache the result keyed by `(emailId, width, theme)`. Re-wrap on resize in the worker, keep the old render visible until the new one is ready.

### Links

- **Default: numbered footnotes.** Inline text shows `Read the report [3]`; a link list sits at the end and in a `L` links overlay that lets the user pick, open, or copy a link. Works in every terminal, survives tmux and SSH, and shows the real URL, which matters for phishing (inferred).
- **OSC 8 as an option** (`links.osc8 = auto`): wrap link text in OSC 8 so terminals make it clickable. There is no detection method (verified: OSC 8 gist), and ratatui has no first-class support: the official example writes the escape sequence into cells in 2-character chunks to dodge a width bug (verified: [ratatui hyperlink example](https://github.com/ratatui/ratatui/tree/main/examples/apps/hyperlink), issue [#902](https://github.com/ratatui/ratatui/issues/902) open). Treat it as experimental until ratatui has per-cell hyperlinks.
- Show the real host next to link text when the visible text looks like a different URL (`[paypal.com -> evil.example]`) (inferred anti-phishing rule).

### Images

- Default: placeholder text `[image: alt text, 640x480]` from the `Image(src)` annotation.
- Inline attached images (`cid:` parts) can be shown on demand with `ratatui-image` 11.1.0 (verified: crates.io), which supports kitty, sixel, iTerm2 and a halfblocks fallback, guesses the protocol from env vars, then queries, and queries font size to map cells to pixels (verified: [ratatui-image README](https://github.com/benjajaja/ratatui-image)). Known good: kitty, Ghostty, foot, xterm, iTerm2, Rio; WezTerm only via iTerm2 protocol; broken on Alacritty, Konsole, Contour, Warp (verified: same table).
- Images must be decoded and resized off the UI thread. Never let an image block a frame (inferred).
- **Remote images: blocked by default, per-sender allow list, never in Spam.** Fastmail's own proxy only covers the web and mobile apps (verified: [Fastmail help](https://www.fastmail.help/hc/en-us/articles/1500000278102-Blocking-remote-images)). If mejl loads a remote image itself, the sender learns the user's IP address and the time of reading (inferred). Fastmail always blocks remote images in Spam (verified: same); mejl should match.

### Colour and dark mode

- Email HTML assumes a white background. Author colours on a dark terminal produce unreadable text (inferred).
- Default: **ignore author foreground and background colours**, keep bold, italic, underline, strikethrough, and render links and quotes with theme styles. Option `html.colors = "ignore" | "contrast" | "author"`. `contrast` keeps author colours but clamps them to a minimum contrast ratio against the actual terminal background read via OSC 11 (inferred design; OSC 11 query verified via colorsaurus).

### Open in browser escape hatch

- `O` writes the sanitised HTML to a private temp file (mode 0600, in `$XDG_RUNTIME_DIR` when available) and opens it with the `open` crate (5.4.5, verified: crates.io).
- Inject a `Content-Security-Policy` meta tag that blocks remote loads (`img-src data: cid:` style) unless the user chose "load remote content" for this message. Otherwise opening in a browser silently leaks what mejl worked to block (inferred).
- Alternative: open the message in Fastmail's web app, which proxies images. The URL scheme for a message deep link is (unverified); see open questions.

## Compose

### Built-in editor vs `$EDITOR`

| Option | Pros | Cons |
|---|---|---|
| Built-in (mejl's own editor, modelled on `ratatui-textarea`) | Instant, no screen flash, inline autocomplete, live markdown preview, spell hints later. Emacs-like keys, undo/redo, soft wrap, search, selection, mouse scroll (verified: [README](https://github.com/ratatui/ratatui-textarea)) | Not the user's editor; no modal editing |
| `edtui` 0.11.7 (vim-like widget) | Vim motions inside mejl (verified: crates.io, detail unverified) | Partial vim, uncanny valley for real vim users (inferred) |
| `$EDITOR` handoff | Users keep their editor and config. aerc and neomutt do this (unverified detail) | Leaves the alternate screen; slow editors make compose feel slow; harder to autocomplete addresses |

Note on crates: `tui-textarea` (rhysd) last changed in December 2024; `ratatui-textarea` is the Ratatui organisation's maintained fork (verified: [README note](https://github.com/ratatui/ratatui-textarea)). Use the fork.

**Recommendation:** both. Headers (From, To, Cc, Bcc, Subject) are always a built-in form, because address autocomplete and identity choice (including Masked Email) need mejl's UI. The body uses the built-in editor by default, `Ctrl-o` then `e` hands the body to `$VISUAL`/`$EDITOR` and returns (`Ctrl-e` is left alone because it is line end in the Emacs-style editor), and `compose.editor = "external"` makes that the default. Handoff: leave raw mode and the alternate screen, run the editor on a temp file, re-enter, force a full redraw (inferred; ratatui's `Terminal::clear` resets the diff).

### Markdown to HTML

- Fastmail's MCP server takes markdown bodies and converts them to HTML with a plain-text fallback (verified: the Fastmail MCP server instructions served to this research session; the server exists, see [Fastmail help](https://www.fastmail.help/hc/en-us/articles/15869557281295-Connecting-AI-tools-via-Fastmail-s-MCP-server) and [Fastmail blog](https://www.fastmail.com/blog/an-mcp-server-for-fastmail/)). That conversion happens inside the MCP server; JMAP itself takes explicit `text/plain` and `text/html` body parts (inferred from RFC 8621's body model). mejl should do the conversion locally.
- Crates: `comrak` 0.57.0 (GFM, CommonMark) or `pulldown-cmark` 0.13.5 (verified: crates.io). Send `multipart/alternative` with the markdown source as `text/plain` (it reads well as plain text) and the rendered HTML (inferred).
- Default `compose.format = "markdown"`, with `"plain"` for mailing lists and `format=flowed` for long lines (RFC 3676, verified above).

### Addresses, attachments, drafts

- **Autocomplete**: fuzzy over local contact cache (JMAP contacts on Fastmail) plus recent correspondents, ranked by frecency. Show name and address; Tab accepts; paste of a list is split on commas and validated (bracketed paste makes this safe).
- **Attachments**: `Ctrl-o` then `a` opens a path picker with fuzzy file search; drag and drop from a GUI file manager pastes a path in most terminals (unverified), which bracketed paste lets mejl recognise and turn into an attachment (inferred). Warn when the body says "attached" and nothing is attached.
- **Drafts autosave**: save locally on every pause in typing (for example 1 s idle) and to the server as a JMAP draft every few seconds or on blur (inferred). Never lose text on crash: write the local copy first, atomically (inferred).
- **Send**: `Ctrl-Enter` like Fastmail (verified: Fastmail uses Ctrl+Enter), but that needs the kitty protocol or extended keys to be distinct from Enter. Legacy-safe alternative: the compose prefix `Ctrl-o` then `s`, or "send" from the palette (inferred). Add "undo send" with a short delay (Fastmail has undo; whether the send delay is server-side via JMAP is in the JMAP research doc).

## Performance

### What "instant" means

| Threshold | Meaning | Source |
|---|---|---|
| 0.1 s | "the limit for having the user feel that the system is reacting instantaneously" | (verified: [Nielsen](https://www.nngroup.com/articles/response-times-3-important-limits/)) |
| 1 s | limit for uninterrupted flow of thought; show progress | (verified: same) |
| 10 s | limit for keeping attention; show percent done and allow cancel | (verified: same) |
| 100 ms | Superhuman's "100ms rule" for every action in an email client | (verified: [Superhuman blog](https://blog.superhuman.com/superhuman-is-built-for-speed/)); claims of a 50 to 60 ms internal target are (unverified: third-party) |
| 5 to 45 ms | Median keypress-to-screen latency of terminals themselves, measured around 2017; tails up to 111 ms under load | (verified: [Dan Luu](https://danluu.com/term-latency/), dated) |
| 16.7 ms / 8.3 ms | One frame at 60 Hz / 120 Hz | arithmetic |

The terminal already spends 5 to 45 ms. To stay under 100 ms end to end, mejl's own share (input parse, update, render, write) should be under one frame for local actions: **p99 under 8 ms from event read to bytes flushed** (inferred target). Anything that needs the network (search on server, open an uncached message) must show the cached or skeleton state within one frame and fill in later.

### Architecture: keep the render loop off the network

```
               +--------------------- UI thread (sync, owns terminal) ---------------------+
 stdin  ---->  | input reader -> Event -> keymap -> Action -> update(state) -> Effects     |
               |                                        |                    |            |
               |                                 dirty? render (ratatui)     |            |
               +--------------------------------------------------^---------|------------+
                                                                  | Msg     | Cmd
               +------------------ tokio runtime (worker threads) |---------v------------+
               |  JMAP client, EventSource push, body fetch, search, send, Sieve, masked  |
               |  html2text + image decode (spawn_blocking), cache writes (storage task)  |
               +--------------------------------------------------------------------------+
```

- **Elm-style update** (verified as a documented ratatui pattern: [The Elm Architecture](https://ratatui.rs/concepts/application-patterns/the-elm-architecture/)): `update(&mut State, Action) -> Vec<Cmd>`. `update` is pure state change plus command values; it never awaits. `Cmd`s go to the runtime over an `mpsc` channel; results come back as `Msg`s through another channel and are fed to `update` like any other action (inferred design).
- ratatui's async template wires crossterm `EventStream`, a tick interval and a render interval with `tokio::select!`, forwarding into an unbounded `mpsc` (defaults: 4 ticks/s, 60 frames/s) (verified: [ratatui recipe](https://ratatui.rs/recipes/apps/terminal-and-event-handler/)). ratatui's FAQ says ratatui "isn't a native async library" and that async mostly helps with reading input (verified: [FAQ](https://ratatui.rs/faq/)).
- **Render on change, not on a timer.** Redraw only when state is dirty, capped at one frame per refresh interval. A fixed 60 fps render interval wastes CPU and battery in a mail client that is idle most of the time (inferred). Spinners and toasts schedule their own wake-ups.
- **Drain input before drawing.** Read all pending events, apply them, then draw once. Holding `j` with fast key repeat then costs one frame per refresh, not one frame per key (inferred). ratatui's FAQ also says to call `draw` once per loop (verified).
- **Wrap each frame in synchronized output** when supported: `BeginSynchronizedUpdate` / `EndSynchronizedUpdate` (crossterm writes `?2026h` / `?2026l`, verified: crossterm source). ratatui's `draw` does not do this itself; its steps are autoresize, render, flush diff, cursor, swap buffers, backend flush (verified: [render.rs](https://github.com/ratatui/ratatui/blob/main/ratatui-core/src/terminal/render.rs)).
- **Write to a buffered stdout.** ratatui's FAQ notes stdout is buffered and faster than unbuffered stderr (verified: FAQ). The async template writes to stderr (verified: recipe); mejl should use a `BufWriter` either way.
- Backend choice: crossterm 0.29 is ratatui's default; ratatui also has termion, termwiz, and a newly merged Termina backend (helix's terminal library; PR merged 2026-06-13, release inclusion unverified) (verified: [PR #2561](https://github.com/ratatui/ratatui/pull/2561)). crossterm lacks the kitty "report associated text" flag (commented out in source, verified) and its last release is April 2025, though the repo is active in October 2026 (verified). Start with crossterm; keep the backend behind ratatui's trait.

### Diff-based rendering

ratatui keeps two buffers and writes only cells that changed: "each frame is compared to the previous frame to determine what has changed, and only the changes are written" (verified: render.rs docs). The diff is a zero-allocation iterator with special handling for wide characters (verified: diff.rs). Consequences (inferred):

- Rendering the full widget tree every frame is fine; the expensive part is building widgets, not writing bytes. Keep widget construction cheap: pre-formatted row strings, no `format!` per cell per frame.
- Scrolling a list by one row rewrites the whole list area, since every row moved. That is still small (a 50 by 200 area is 10,000 cells). Scroll-region optimisations are not worth the complexity at first.
- ratatui 0.30 keeps a layout cache on by default (verified: [0.30 highlights](https://ratatui.rs/highlights/v030/)).

### Virtualised lists for 100k messages

- ratatui's `List` takes all items up front (`List::new(items)`), so passing 100k `ListItem`s per frame would allocate and style 100k rows every frame (inferred from the API shape). Build only the visible window.
- mejl's list widget (inferred design):
  - State: `ids: Vec<EmailId>` (or thread ids) in sort order from JMAP `Email/query`, `offset`, `cursor`, `selected: HashSet`.
  - Render: for rows `offset..offset+height`, look up a `RowCache` (`HashMap<Id, PreparedRow>`, pre-truncated strings and styles at the current width), fill misses from the local store, render directly into the `Buffer`.
  - Missing rows (not yet synced) render as a dim skeleton and request a page from the worker.
  - Scrollbar position from `offset / ids.len()`; jumping to the end is O(1).
- 100k ids is about 1 to 3 MB of memory depending on id length (inferred), trivial. JMAP `Email/query` supports position and anchor paging, so the full id list does not have to be loaded at once (inferred; details in the JMAP doc).
- `tui-scrollview` 0.6.8 and `ratatui-layout`'s `VirtualList` exist (verified: crates.io, docs.rs) but a mail list is simple enough to own.

### Startup

Target: **first useful frame (cached mailbox list and message list) under 50 ms from exec on a warm disk cache; under 150 ms cold** (inferred targets; compare the 100 ms rule).

How to hit it (inferred):

1. Do the minimum before the first frame: parse config, open the local store, read the last viewed mailbox's first screen of rows, enter raw mode and the alternate screen, draw.
2. Use last run's cached terminal capabilities for that first frame; send the probe burst at the same time and apply results when they arrive. (As built: the probe still runs and blocks before the first frame, because crossterm would read the replies as keys. After a timed-out probe mejl drains late replies, stopping at 50 ms of quiet unless a reply is half read, with a 500 ms cap; keys typed meanwhile are dropped. The cache only fills answers a timed-out probe lacks.)
3. Do **not** wait for: JMAP session fetch, token refresh, push connection, capability probe replies, theme detection, contact cache, Sieve scripts.
4. Local store: something that opens in a few ms and can read a page of rows without parsing the whole database. SQLite in WAL mode, or an mmap'd key-value store (LMDB via `heed`, `redb`, `fjall`) all fit (unverified for exact open times; benchmark). The storage choice belongs to the storage doc; this doc only sets the budget.
5. Lazy-initialise heavy things: HTML converter, syntax tables, image protocol picker, fuzzy matcher index.
6. Keep the binary lean: avoid pulling in a TLS stack initialisation or large static tables on the startup path (inferred).

### Optimistic updates with rollback

- Every mutation (archive, move, pin, mark read, delete, label) updates local state and the UI in the same frame, then sends the JMAP call (inferred design).
- Keep an `Outbox` of pending operations, each with its inverse. On failure: roll back, show a toast with the error and a retry key.
- On success, reconcile with the server's new state string; if a push update arrives first, the outbox entry is matched and dropped.
- Undo (`z`, Fastmail-compatible) applies the inverse operation; it is just another optimistic mutation.
- Batch rapid actions: archiving 20 messages in 2 s should become one `Email/set` call, not 20 (inferred).

### Prefetching

- When the cursor rests on a message, fetch its body; prefetch the next and previous few (direction-aware: prefetch further in the direction the user is moving). Prefetch bodies of the first screen of unread messages on startup in the background (inferred).
- Pre-render prefetched HTML to text at the current width in the worker, so opening is a cache hit.
- Cancel stale prefetches when the user jumps far (tokio task handles or `CancellationToken`).

### Measuring

- `tracing` 0.1.44 and `tracing-subscriber` 0.3.23 for spans; `tracing-chrome` 0.7.2 for Chrome/Perfetto traces; `tracing-tracy` 0.12.0 for live profiling with Tracy (verified: crates.io).
- Instrument: `input_to_flush` (from event timestamp to `Backend::flush` returning), `update`, `build_widgets`, `diff_and_write`, `bytes_written`, and network round trips per JMAP method.
- Keep a ring buffer of the last 1,000 frame times; a `:perf` overlay shows p50/p95/p99 and the slowest recent frames (inferred). `tui-logger` 0.18.3 can show logs inside the TUI (verified: crates.io).
- CI performance test: render the list with 100k synthetic messages using ratatui's `TestBackend` and assert a per-frame budget; replay a recorded key sequence and assert p99 (inferred). Add a startup benchmark that measures exec to first flush.

## Accessibility

### Screen readers

- Full-screen TUIs that redraw spatially are hard for screen readers: the cursor jumps around and output is not a linear stream ([Casey Reeves, "The text mode lie"](https://xogium.me/the-text-mode-lie-why-modern-tuis-are-a-nightmare-for-accessibility), summarised by [OSnews](https://www.osnews.com/story/144892/the-text-mode-lie-why-modern-tuis-are-a-nightmare-for-accessibility/)).
- Screen readers use the terminal cursor position as the focus signal. Brick added `putCursor` so apps can place a hidden cursor on the focused element (verified: [blind.guru](https://blind.guru/blog/2021-06-25-brick.html)).
- What mejl should do (inferred from the above):
  - After every frame, move the cursor to the start of the focused row or field, even when it is hidden. In ratatui, call `Frame::set_cursor_position` and choose visibility separately (needs a small backend tweak, since ratatui shows the cursor when a position is set; detail to verify).
  - `ui.accessible = true` mode: no spinners (status text instead), no animations, no decorative box drawing, the focused row also marked with a text marker, and the status line announces changes as plain words ("Archived 3 conversations").
  - Every action has a non-visual confirmation in the status line.
  - Longer term: a line-oriented CLI (`mejl list`, `mejl read`) for screen-reader users and scripts, which the TUI shares code with (a reader suggestion in the OSnews thread, unverified value).

### Colour

- `NO_COLOR` present and non-empty disables colour; user config and flags override it (verified: [no-color.org](https://no-color.org/)). In monochrome mode, state is carried by bold, reverse video, underline and glyphs.
- `FORCE_COLOR`, `CLICOLOR_FORCE`, `CLICOLOR`, `COLORTERM` and `TERM` are the other signals `supports-color` checks (verified: [supports-color source](https://github.com/zkat/supports-color/blob/main/src/lib.rs)).
- Never encode meaning by colour alone: unread is bold plus glyph, pinned is a glyph, errors carry a word ("failed"), not only red.
- Colour-blind safety: avoid red/green as the only pair; prefer blue/orange for good/bad contrasts when truecolor themes are used (common guidance, unverified source).
- High contrast: a `high-contrast` theme that uses only the default fg/bg plus reverse video for selection.

### Respecting the terminal palette

- **Default theme uses the 16 ANSI colours plus the terminal's default fg/bg** (inferred recommendation). The user's terminal theme, contrast choices and light/dark switching then apply automatically, with no detection needed.
- Truecolor themes are opt-in, chosen by name, with light and dark variants. Switch automatically using mode 2031 notifications where supported (kitty, Ghostty, VTE, tmux 3.6+, Zellij; not WezTerm) or the OSC 11 background read at startup (verified: support sources above).
- Never paint a full background colour by default; it breaks transparent terminals and light/dark switching (inferred).

## UX spec sketch for mejl

This is a starting spec, not a final design. Everything here is (inferred) from the research above unless it cites a source. The HTML mock-ups should follow it.

### Screens

| Screen | Reached by | Contents |
|---|---|---|
| Mail | start, `g i`, `Esc` from overlays | Responsive panes: sidebar (mailboxes, counts, saved searches), conversation list, reader. Top line: account, mailbox, sync state. Bottom: hint bar and status/toast line |
| Reader (full) | `Enter` in stack layout, double click | Conversation stack, collapsed read messages, header block (`H` toggles full headers), body, link footnotes, attachment list, "2 trackers blocked, remote images off" notice |
| Compose | `c`, `r`, `a`, `f` | Header form (From with identity and Masked Email picker, To, Cc, Bcc, Subject), body editor, attachment row, footer with send/save keys and draft state ("saved 3 s ago") |
| Search | `/` | Query input with Fastmail search syntax, instant local results, then server results merged in; `Enter` turns it into a list view |
| Command palette | `:` or `Ctrl-k` | Fuzzy list of commands, mailboxes, contacts, rules, masked addresses, each with its key |
| Rules (Sieve) | `g r`, palette, `F` from a message | Rule list (name, condition summary, action, enabled), rule editor form with a raw Sieve tab, "test against this message" preview, validation errors inline |
| Masked Email | `g m`, palette, `@` from a message | List of masked addresses (address, domain, description, state, last used), filter, create for a domain, enable, disable, delete; from a message: "which mask received this", "disable this mask" |
| Help | `?` | Bindings for the current context, filterable, grouped |
| Settings | `g ,` or palette | Read-mostly view of effective config and detected terminal capabilities, with the config file path; "probe terminal again" action |

Responsive layout, as specified in the layout section:

```
>= 160 cols                                    100 to 159 cols                 < 100 cols
+--------+----------------+--------------+     +--------------+-----------+    +--------------+
|Inbox 12|> Ada  Re: Eng..| From: Ada    |     |> Ada  Re: ...| From: Ada |    |> Ada  Re: ...|
|Archive |  Bob  Invoice  | Subject: ... |     |  Bob  Invoice| ...       |    |  Bob  Invoice|
|Sent    |  ...           | body ...     |     |              |           |    |              |
+--------+----------------+--------------+     +--------------+-----------+    +--------------+
| j/k move  o open  y archive  d trash  r reply  / search  : commands  ? help                   |
```

### Default keymap (fastmail preset)

"Origin" shows where the binding comes from: FM = Fastmail web (verified: [Fastmail shortcuts](https://www.fastmail.help/hc/en-us/articles/360058753534-Keyboard-shortcuts)), aerc (verified: [binds.conf](https://github.com/rjarry/aerc/blob/master/config/binds.conf)), new = mejl-specific.

Global:

| Key | Action | Origin |
|---|---|---|
| `?` | Help for current context | FM |
| `:` / `Ctrl-k` | Command palette | k9s/vim / Superhuman-like |
| `/` | Search | FM |
| `g` | Go prefix: `g i` inbox, `g a` archive, `g s` sent, `g d` drafts, `g t` trash, `g j` spam, `g f` mailbox picker, `g r` rules, `g m` masked email, `g ,` settings, `g g` top | FM (`g` = go to folder), extended |
| `G` | Bottom of list | vim |
| `Tab` / `Shift-Tab` | Next / previous pane | common |
| `Esc` / `u` | Back one level (close overlay, reader to list) | FM (`u` returns to mailbox) |
| `q` / `Q` | Back, or quit at top level / quit now | common |
| `z` / `Ctrl-y` | Undo / redo | FM |
| `c` | Compose | FM |

List and reader (the same action keys work in both; in the reader they act on the focused message or conversation):

| Key | Action | Origin |
|---|---|---|
| `j` / `k`, `Down` / `Up` | Next / previous conversation | FM |
| `o` / `Enter` | Open | FM |
| `Space` / `PgDn`, `PgUp`, `Ctrl-d` / `Ctrl-u` | Page or half page | FM (`Space`), vim. `Shift-Space` is FM's page up but is indistinguishable from `Space` without the kitty protocol, so `PgUp` and `Ctrl-u` are the legacy-safe keys |
| `n` / `p` | Next / previous message inside the conversation | FM |
| `[` / `]` | Archive, then open previous / next | FM |
| `x` / `X` | Toggle select / select range | FM |
| `*` then `a` / `n` | Select all / none | FM |
| `y` | Archive | FM |
| `d` | Move to trash (undoable) | FM |
| `#` | Delete permanently (in Trash or Spam only) | FM |
| `!` | Report spam | FM |
| `m` / `v` | Move to mailbox (fuzzy picker) | FM |
| `l` | Labels picker | FM |
| `s` | Toggle pin (flag) | FM |
| `.` then `r` / `u` / `p` / `n` | Mark read / unread / pin / unpin | FM |
| `I` / `U` | Mark read / unread | FM (Shift+i, Shift+u) |
| `M` | Mute conversation | FM |
| `b` then `1` to `7` | Snooze options | FM |
| `r` / `a` / `f` | Reply / reply all / forward | FM |
| `e` / `E` | Expand or collapse message / all | FM |
| `"` | Toggle quoted text | FM |
| `;` | Actions menu (same as right click) | FM |
| `H` | Toggle full headers | aerc (FM uses `:`, which mejl gives to the palette) |
| `T` | Toggle conversation / thread tree view | aerc |
| `L` | Links overlay (open or copy a link) | new |
| `O` | Open in browser (sanitised, remote content still blocked) | aerc (`O` open) |
| `Y` | Copy menu: address, subject, message id, link | new |
| `F` | New rule from this message (Sieve) | new |
| `@` | Masked Email actions for this message | new |

Compose:

| Key | Action | Note |
|---|---|---|
| `Tab` / `Shift-Tab` | Next / previous field | aerc |
| `Ctrl-Enter` | Send | FM. Needs kitty protocol or tmux extended keys |
| `Ctrl-o` then `s` | Send (legacy-safe) | new. `Ctrl-o` is the compose prefix and opens a which-key popup |
| `Ctrl-o` then `a` | Attach file (fuzzy path picker) | new |
| `Ctrl-o` then `e` | Edit body in `$VISUAL` / `$EDITOR` | new |
| `Ctrl-o` then `p` | Toggle markdown preview | new |
| `Ctrl-o` then `m` | New Masked Email address as From for this recipient | new |
| `Ctrl-o` then `d` | Discard draft (undoable for a short time) | new |
| `Ctrl-o` then `:` | Command palette | new |
| `Ctrl-s` | Save draft | FM |
| `Esc` | Leave the text field (focus moves to the field list); a second `Esc` closes compose and keeps the draft | new. Never discards text |

Precedence rule: a focused text field owns every key its editor binds. the Emacs-like defaults the body editor follows include `C-a`/`C-e`, `C-n`/`C-p`, `C-f`/`C-b`, `C-h`/`C-d`, `C-k`, `C-j` and yank (verified: [README](https://github.com/ratatui/ratatui-textarea)), so global `Ctrl-k`/`Ctrl-p` (palette) and `Ctrl-y` (redo) do not apply inside the body. mejl reserves only `Ctrl-o`, `Ctrl-s` and `Ctrl-Enter` in text fields and intercepts them before the widget sees the event (inferred; check the widget's full key table and whether it offers input handling without its default shortcuts). Global keys are reachable after `Esc`.

Alt keys are deliberately absent from the compose defaults. In legacy encoding `Alt-x` arrives as Esc followed by `x`; over SSH, or with tmux's 10 ms `escape-time`, the two bytes can arrive in separate reads, so `Alt-a` could be read as "Esc" then "a" (inferred from the encoding). With the kitty protocol's disambiguate flag this problem goes away, so Alt bindings are fine as user overrides on terminals that support it.

Vim preset differences: `j`/`k` scroll in the reader and `J`/`K` move between conversations; `gg`/`G`; counts on motions; `a` archive; `r` becomes a prefix (`r r` reply all, `r s` reply sender, `r q` quoted, after aerc's `rr`/`Rr`/`rq`); `.` repeats the last action; `u` undo; `z` fold prefix (`z c`, `z o`, `z a`); `Space` leader for mejl tools (`Space r` rules, `Space m` masked email).

### Mouse behaviours

- Enabled by default with modes 1000, 1002 and 1006; no motion tracking. `mouse = false` disables capture entirely; `mouse.hover = true` adds 1003.
- Click selects and previews; double click opens; click on the select column toggles selection; wheel scrolls the pane under the pointer; drag on pane borders resizes; click on mailboxes, hint bar entries, fold markers, footnote markers and attachment names acts on them; right click opens the actions menu.
- Every clickable label shows its key (btop pattern), so the mouse teaches the keyboard.
- Help text notes "hold Shift to select text" for kitty and Ghostty (verified above), and `Y` copy actions remove most need for text selection.

### Theme approach

- Default theme `terminal`: ANSI 16 colours plus default fg/bg, no background fills, bold for unread, dim for previews and quotes, reverse video for the cursor row. Works on light and dark terminals without detection.
- Optional truecolor themes with light and dark variants; `theme = "auto:<name>"` picks the variant from mode 2031 or OSC 11.
- `NO_COLOR` respected; `theme = "mono"` and `theme = "high-contrast"` built in.
- State never relies on colour alone; glyphs from the Neutral width class (◦ ⚑ ✓ ✗ ▸ ▾ ↩ ↪ ⋯) with an ASCII fallback set.
- HTML author colours ignored by default, `contrast` mode optional.
- Theme files are TOML, with semantic names (`list.unread`, `list.cursor`, `reader.quote.1`, `status.error`) rather than raw colours, so a theme cannot accidentally lose a state distinction.

## Open questions

| Question | Why it matters | How to resolve |
|---|---|---|
| The "?" cells in the support matrix (Alacritty and kitty mode 2027, WezTerm OSC 8/52/notifications, Konsole kitty keys, iTerm2 2027/2031) | Defaults per terminal | Build a small `mejl probe` command that sends the batched queries and prints answers; run it on each terminal, or run [ucs-detect](https://ucs-detect.readthedocs.io/) and `npx terminfo.dev` |
| Which kitty keyboard flags each terminal really honours (for example Windows Terminal 1.25, iTerm2) | Ctrl+Enter send, Esc latency | Same probe, with an interactive key echo screen |
| Does tmux with the default `set-clipboard external` forward an app's OSC 52 to the outer terminal? The man page says app attempts are ignored | Copy actions inside tmux | Test on tmux 3.8 with and without `set-clipboard on` |
| Can ratatui place the cursor for screen readers while keeping it invisible? `draw` shows the cursor whenever a position is set | Accessibility mode | Read `ratatui-core` cursor handling; prototype with a custom backend wrapper; test with Orca, VoiceOver and NVDA in real terminals |
| Why PR #2853 (per-cell hyperlinks) was closed on 2026-10-23, and what replaces it | OSC 8 support | Read the PR thread; ask in ratatui's tracker |
| Is Termina (merged as a ratatui backend in June 2026) in a released ratatui, and is it better than crossterm for kitty keys? | Backend choice | Check ratatui changelog after 0.30.2; compare key handling |
| Fastmail web deep link format for a message or thread | "Open in Fastmail" escape hatch with image proxy | Check Fastmail docs; inspect URLs in the web app manually (not via the user's mailbox tools) |
| Real-world quote and signature markers in HTML mail (Gmail, Outlook, Apple Mail) | Quote folding quality | Build a test corpus of sanitised samples |
| `html2text` speed and robustness on large newsletters | Reader open time, crash safety | Benchmark on a corpus; fuzz with `cargo fuzz`; wrap in panic catch and size limits |
| Startup budget (50 ms warm, 150 ms cold) on real hardware with the chosen store | "Instant" promise | Prototype the startup path with the storage engine from the storage doc; measure exec to first flush |
| Double-click interval and wheel step size | Mouse feel | Terminals do not report OS settings; make both configurable, pick defaults by testing |
| Undo send and send delay: server-side support in Fastmail JMAP? | Compose safety | See the JMAP research doc; test against a test account, not the user's mailbox |
| Colour-blind safe default palette for truecolor themes | Accessibility | Use a published palette (for example Okabe-Ito) and check contrast with a simulator |
| Neovim which-key popup delay and yazi popup details were not verified | Popup timing | Read their docs; test 0 ms vs 300 ms with users |
