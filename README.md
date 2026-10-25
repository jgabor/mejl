# mejl

mejl is a fast, keyboard-first email client for Fastmail that runs in your terminal. It talks to Fastmail over JMAP, keeps a local cache so it opens on your mail at once, applies every action straight away and syncs in the background. It knows Fastmail's own features: Masked Email, push, and rules you can build and test before you paste them into Fastmail.

## Status

mejl is early. Everything runs end to end against a built-in fake server, and the test suite covers it, but mejl has not been checked against a live Fastmail account yet. A few things it relies on are still open questions, listed with the steps that settle them in [docs/README.md](docs/README.md#open-questions). Among them: the exact shape of Fastmail's push events, which headers stored mail carries for masked addresses, and whether sending from a masked address works over JMAP.

Use it on an account you can afford to experiment with, and keep Fastmail's web app at hand.

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

mejl uses a Fastmail API token.

1. Open the API tokens page in Fastmail's settings: <https://app.fastmail.com/settings/security/tokens>. API tokens need a plan that includes them; Basic plans have none.
2. Create a new token. Give it these scopes:
   - Email (needed: reading and organising mail)
   - Email submission (to send)
   - Masked Email (for the Masked Email screen)
   - Contacts (for address completion)
3. Copy the token, then run:

```sh
mejl login
```

Paste the token when asked. It is not shown as you paste. mejl checks it with Fastmail, tells you which scopes it found and what is missing, and stores it in your system keyring. Then start mejl:

```sh
mejl
```

Other commands:

| Command | What it does |
|---|---|
| `mejl doctor` | Prints mejl's paths, checks the config and shows what your terminal supports. Never prints the token |
| `mejl doctor --print-config` | Prints a commented example config |
| `mejl keys --check` | Checks your key overrides |
| `mejl logout` | Removes the token and the local cache. It asks before it removes queued changes, drafts and rules |

Without a keyring, set `token_command` in the config (for example `pass show fastmail/mejl`), or set `MEJL_TOKEN` in the environment.

## Config

The config file is `config.toml` in mejl's config folder. On Linux that is `~/.config/mejl/config.toml`. `mejl doctor` prints the path on your system. Every setting is optional.

```toml
[ui]
theme = "terminal"        # terminal, mono, high-contrast, dark or light
preset = "fastmail"       # key preset: "fastmail" or "vim"
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

[keys.list]               # key overrides, per context
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

Prefer vim keys? Set `preset = "vim"`.

## Rules

Fastmail has no public API for rules, so mejl cannot change the rules on your account. Instead, you build rules in mejl (`g r`), see which recent messages each one would catch, and mejl turns them into a block of Sieve code. Press `P` to copy the block, then in Fastmail open Settings, then Filters & Rules, edit your custom Sieve code and paste it in. mejl remembers what you copied, and `v` checks a pasted copy of your script against it, so you know whether Fastmail has the latest version.

The block only uses Sieve features Fastmail allows, never discards mail, and marks clearly where it starts and ends, so pasting a new version replaces the old one.

## Licence

mejl is dual licensed under the [MIT licence](LICENSE-MIT) and the [Apache License, Version 2.0](LICENSE-APACHE), at your option.
