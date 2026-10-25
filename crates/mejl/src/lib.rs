//! The mejl binary's building blocks.
//!
//! Everything the `mejl` executable does lives here as a library, so each
//! piece can be tested without a terminal, a network or the user's real
//! directories. `main.rs` only calls [`cli::main`].
//!
//! | Module | Role |
//! |---|---|
//! | [`cli`] | Arguments, commands and exit codes |
//! | [`paths`] | Config, data, cache and state directories |
//! | [`config`] | The TOML config, its defaults and checks |
//! | [`secrets`] | Token storage: keyring, `token_command`, `MEJL_TOKEN` |
//! | [`login`] | Token paste, check against the session, report |
//! | [`lock`] | One mejl per account |
//! | [`terminal`] | Setup, restore, suspend, probe, OSC 52 and OSC 8, `$EDITOR`, signals |
//! | [`events`] | The main loop's channel |
//! | [`input`] | The input thread and event coalescing |
//! | [`timer`] | Clock ticks and wake-ups |
//! | [`render_pool`] | Body render workers |
//! | [`demo`] | `mejl --demo` backend |
//! | [`logging`] | File logging with token redaction |
//! | [`doctor`] | `mejl doctor` |
//! | [`keys`] | `mejl keys --check` |
//! | [`app`] | The interactive app: main loop, read model, optimistic overlay, commands |
//! | [`tui`] | Running the app on the real terminal |
//!
//! Threads, as in `docs/architecture.md`: the UI thread owns the terminal
//! and blocks on [`events::LoopReceiver`]; the input thread, the timer, the
//! render workers, the signal watcher and the sync runtime all send to it
//! through [`events::LoopSender`]. The UI thread never awaits.

#![warn(missing_docs)]

pub mod app;
pub mod cli;
pub mod config;
pub mod demo;
pub mod doctor;
pub mod env;
pub mod events;
pub mod input;
pub mod keys;
pub mod lock;
pub mod logging;
pub mod login;
pub mod paths;
pub mod render_pool;
pub mod secrets;
pub mod terminal;
pub mod timer;
pub mod tui;
