# mejl

mejl is a fast, keyboard-first email client for Fastmail that runs in your terminal. It talks to Fastmail over JMAP, keeps a local cache so it opens on your mail at once, applies every action straight away and syncs in the background. It knows Fastmail's own features: Masked Email, push, and rules you can build and test before you paste them into Fastmail.

## Install from source

You need Rust 1.88 or newer.

```sh
git clone https://github.com/jgabor/mejl
cd mejl
cargo install --path crates/mejl
```

This puts a `mejl` binary in `~/.cargo/bin`.

## Try it without an account

```sh
mejl --demo
```

The demo runs mejl against a fictional mailbox served by a fake server on your own machine (127.0.0.1). Nothing goes to the internet, nothing is written to your real data, and the demo's files are deleted when you quit. A new message arrives about every 45 seconds, so you can see push working.

## Log in to Fastmail

```sh
mejl login
```

mejl asks how you want to log in: with your browser (OAuth), or by pasting an API token. `mejl login --oauth` and `mejl login --token` skip the question.

### With your browser

mejl opens Fastmail's login page in your browser and waits up to five minutes. If no browser opens, it prints the address to open yourself. After you allow access, the tab says you can close it, and mejl stores the login in your system keyring. It refreshes the login by itself while it runs.

Over SSH, the browser on your own machine cannot reach mejl. Forward a port and tell mejl to use it:

```sh
ssh -L 8765:127.0.0.1:8765 your-server
mejl login --port 8765
```

The first browser login registers mejl as an app with Fastmail, once per device. If Fastmail gave you a client id for mejl, set `oauth_client_id` under your account in the config and mejl uses that instead.

The browser login may not include Masked Email. If Fastmail does not grant it, mejl logs in without it and says so, and the Masked Email screen explains why. Log in with an API token to use masks. `mejl doctor` shows what the login was granted.

Browser login is new and has not been tried against every Fastmail plan yet. If it fails, use an API token.

### With an API token

1. Open the API tokens page in Fastmail's settings: <https://app.fastmail.com/settings/security/tokens>. API tokens need a plan that includes them; Basic plans have none.
2. Create a new token. Give it these scopes:
   - Email (needed: reading and organising mail)
   - Email submission (to send)
   - Masked Email (for the Masked Email screen)
   - Contacts (for address completion)
3. Copy the token, run `mejl login --token` and paste it when asked. It is not shown as you paste. mejl checks it with Fastmail, tells you which scopes it found and what is missing, and stores it in your system keyring.

Then start mejl:

```sh
mejl
```

Other commands:

| Command | What it does |
|---|---|
| `mejl doctor` | Prints mejl's paths, checks the config, shows how you are logged in and what your terminal supports. Never prints a token |
| `mejl doctor --print-config` | Prints a commented example config |
| `mejl keys --check` | Checks your key overrides |
| `mejl logout` | Ends the login (a browser login is revoked at Fastmail), removes it and the local cache. It asks before it removes queued changes, drafts and rules |

Without a keyring, set `token_command` in the config (for example `pass show fastmail/mejl`), or set `MEJL_TOKEN` in the environment. Both take an API token and win over a stored login.

## Config

The config file is `config.toml` in mejl's config folder. On Linux that is `~/.config/mejl/config.toml`. `mejl doctor` prints the path on your system. Every setting is optional.

```toml
[ui]
theme = "terminal"        # terminal, mono, high-contrast, dark or light
preset = "fastmail"       # key preset: "fastmail", "gmail", "vim" or one of your own
density = "comfortable"   # compact, comfortable or relaxed
undo_send_seconds = 10    # how long a sent message waits so you can undo it
mark_read_delay_ms = 0    # wait before marking an opened message read
mark_read_on_archive = false  # archiving a conversation also marks it read
prefer_html = false       # show the HTML part when a message has both
remote_images = "off"     # never load remote images unless you turn this on
mouse = true              # false keeps your terminal's own text selection

[sync]
push = true               # new mail arrives over push
poll_interval_secs = 300  # how often to check when push is down
prefetch = 3              # bodies to fetch ahead of the cursor

[keys.list]               # key overrides on top of any preset, per context
"ctrl-n" = "next"
```

The settings screen (`g ,`) changes the same `[ui]` values and writes them back to this file, keeping your comments. `NO_COLOR` in the environment turns colour off.

## Keys

The default keys follow Fastmail's web shortcuts, so they are what you know from the web app. Press `?` in mejl for every key in the current context, `:` for the command palette, and `g` to see where you can go. A few to start with:

| Key | Action |
|---|---|
| `j` / `k` | Next or previous conversation |
| `Enter` or `o` | Open |
| `y` | Archive |
| `d` | Trash |
| `s` | Pin |
| `z` | Undo, also for a message you just sent |
| `r` / `a` / `f` | Reply, reply all, forward |
| `c` | Compose |
| `/` | Search |
| `g m` | Masked Email |
| `g r` | Rules |
| `q` | Back, then quit |

Prefer Gmail's keys or vim keys? Set `preset = "gmail"` or `preset = "vim"`. Gmail's preset archives with `e`, trashes with `#`, marks read and unread with `Shift-I` and `Shift-U`, stars (pins) with `s`, and goes to Sent with `g t`. Gmail has no keys for the trash and spam folders, so they are `g #` and `g !`.

### Your own keys

Open settings (`g ,`), go to the Keys row and press Enter. The key editor works like the controls screen of a game:

1. A built-in preset cannot change, so press `d` to make your own copy of it and give it a name.
2. Pick a command, press Enter, then press the new key. `a` adds a second key, `x` removes one, `u` puts the preset's key back.
3. If the key already does something, mejl asks whether to swap the two keys, take the key, or cancel. Keys that would break a prefix such as `g` are refused with the reason.

The Mail section changes the list and the reader together. Every change is written to the config file at once and works without a restart. Your preset stores only what you changed, so new commands in later versions of mejl get their default keys:

```toml
[presets.mine]
base = "fastmail"
[presets.mine.keys.list]
"e" = "archive"
"y" = "none"
```

`R` renames, `D` deletes and `p` switches presets. `mejl keys --check` checks every preset and macro.

### Macros

A macro makes several changes to the conversations under the cursor, or the selected ones, with one key. All its steps go to the server as one change, so one `z` undoes the whole macro. In the key editor, go to the Macros section and press `n`. Add steps with `a` (mark read or unread, pin or unpin, mute, add or remove a label, move to a folder, archive, trash, spam), reorder them with `K` and `J`, and bind a key with `b`. A macro moves the mail at most once and never sends or deletes anything for good.

```toml
[macros.file-receipt]
label = "File receipt"
keys = ["f2"]
steps = ["mark-read", "unpin", { move = "Receipts" }]
```

Macro keys work in the list and the reader with every preset. A macro key that would break the active preset, such as `r` in vim, is skipped, and `mejl keys --check` says so.

## Rules

Fastmail has no public API for rules, so mejl cannot change the rules on your account. Instead, you build rules in mejl (`g r`), see which recent messages each one would catch, and mejl turns them into a block of Sieve code. Press `P` to copy the block, then in Fastmail open Settings, then Filters & Rules, edit your custom Sieve code and paste it in. mejl remembers what you copied, and `v` checks a pasted copy of your script against it, so you know whether Fastmail has the latest version.

The block only uses Sieve features Fastmail allows, never discards mail, and marks clearly where it starts and ends, so pasting a new version replaces the old one.

## Licence

mejl is dual licensed under the [MIT licence](LICENSE-MIT) and the [Apache License, Version 2.0](LICENSE-APACHE), at your option.
