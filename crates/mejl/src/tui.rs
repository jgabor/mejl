//! Running the app on the real terminal, for an account or the demo.
//!
//! The order matters and is the same for both:
//!
//! 1. Probe the terminal, before anything else reads from it. With a
//!    capability cache path, answers a slow terminal did not give come from
//!    `caps.toml`.
//! 2. Build the UI settings. Key override errors stop here, before the
//!    terminal changes, so the user sees them.
//! 3. Open the store and start the engine.
//! 4. Enter TUI mode, then start the input thread, the signal watcher and
//!    the app, and draw the first frame.
//! 5. On quit: stop the app's own threads, restore the terminal, then shut
//!    the engine down, so a slow shutdown never leaves a raw terminal.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use jiff::tz::TimeZone;
use mejl_jmap::Client;
use mejl_store::{Store, StoreOptions};
use mejl_sync::{Engine, EngineConfig};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::app::host::TerminalHost;
use crate::app::settings::Settings;
use crate::app::{App, AppEvent, AppParts};
use crate::cli::{CliError, exit};
use crate::config::Config;
use crate::env::{Env, var};
use crate::events::{self, LoopEvent};
use crate::input::InputThread;
use crate::oauth::OAuthSession;
use crate::terminal::editor::EditorCommand;
use crate::terminal::probe::{self, Support};
use crate::terminal::signals::SignalWatcher;
use crate::terminal::{self, TerminalOptions};

static STARTED: OnceLock<Instant> = OnceLock::new();

/// Records when the process started, for the time to first frame.
pub fn mark_start() {
    let _ = STARTED.set(Instant::now());
}

fn since_start() -> Duration {
    STARTED.get().map_or(Duration::ZERO, Instant::elapsed)
}

/// What [`run`] needs.
pub struct Launch {
    /// The account name, shown until the session is cached.
    pub account: String,
    /// The account's store directory.
    pub store_dir: PathBuf,
    /// The JMAP client with the token.
    pub client: Client,
    /// The OAuth login inside `client`, when it has one.
    pub oauth: Option<OAuthSession>,
    /// The loaded config.
    pub config: Config,
    /// Where setting changes are written.
    pub settings_path: PathBuf,
    /// The environment.
    pub env: Env,
    /// Engine settings.
    pub engine: EngineConfig,
}

/// What happened, for lines printed after the terminal is restored.
#[derive(Debug, Default)]
pub struct Report {
    /// Sends still held when the user quit; they go out on the next start.
    pub held_sends: usize,
    /// Why the app stopped on its own, if it did.
    pub note: Option<String>,
    /// Time from process start to the first frame on screen.
    pub first_frame: Duration,
}

/// Engine settings from the config.
pub fn engine_config(config: &Config) -> EngineConfig {
    EngineConfig {
        push: config.sync.push,
        poll_interval: Duration::from_secs(u64::from(config.sync.poll_interval_secs)),
        time_zone: TimeZone::system(),
        ..EngineConfig::default()
    }
}

/// Store settings from the config.
pub fn store_options(config: &Config) -> StoreOptions {
    StoreOptions {
        body_cache_bytes: config.sync.body_cache_bytes(),
        ..StoreOptions::default()
    }
}

fn failure(what: &str, e: impl std::fmt::Display) -> CliError {
    CliError::new(exit::FAILURE, format!("{what}: {e}"))
}

/// Runs the app until the user quits, without the capability cache.
pub fn run(launch: Launch) -> Result<Report, CliError> {
    run_with_caps_cache(launch, None)
}

/// [`run`], reading and updating the capability cache at `caps_cache`
/// (normally [`crate::paths::Paths::caps_file`]). Demo mode passes `None`,
/// so it never writes the user's real cache directory.
pub fn run_with_caps_cache(
    launch: Launch,
    caps_cache: Option<&std::path::Path>,
) -> Result<Report, CliError> {
    let caps = probe::detect_with(&launch.env, Some(probe::DEFAULT_TIMEOUT), caps_cache);
    let no_color = caps.no_color || launch.env.is_set(var::NO_COLOR);
    let settings = Settings::new(
        launch.settings_path.clone(),
        launch.config.clone(),
        no_color,
        TimeZone::system(),
    );
    let (ui, warnings) = settings.ui_config().map_err(|errors| {
        CliError::new(
            exit::CONFIG,
            format!(
                "the [keys] overrides in the config have errors:\n  {}\nRun `mejl keys --check` for details.",
                errors.join("\n  ")
            ),
        )
    })?;
    let store = Store::open(&launch.store_dir, store_options(&launch.config))
        .map_err(|e| failure("cannot open the local store", e))?;
    let (tx, rx) = events::channel::<AppEvent>();
    if let Some(oauth) = &launch.oauth {
        // Hooked up before the engine starts, so its first refresh is
        // covered.
        let save_tx = tx.clone();
        oauth.store.on_save_error(move |why| {
            let _ = save_tx.msg(AppEvent::LoginNotSaved(why));
        });
    }
    let sink_tx = tx.clone();
    let handle = Engine::start(launch.engine, launch.client.clone(), store, move |e| {
        let _ = sink_tx.msg(AppEvent::Sync(e));
    })
    .map_err(|e| failure("cannot start the sync engine", e))?;

    let guard = match terminal::enter(TerminalOptions {
        mouse: launch.config.ui.mouse,
        kitty_keyboard: caps.kitty_keyboard == Support::Yes,
        // Off even when supported: crossterm stalls on the reports. See
        // `terminal::theme`.
        theme_reports: false,
        ..TerminalOptions::default()
    }) {
        Ok(g) => g,
        Err(e) => {
            handle.shutdown();
            return Err(failure("cannot set up the terminal", e));
        }
    };
    let result = run_inside(
        Inside {
            account: launch.account,
            env: &launch.env,
            caps: &caps,
            settings,
            ui,
            warnings: &warnings,
            tx,
            handle: handle.clone(),
            client: launch.client,
            oauth: launch.oauth.map(|o| o.credentials),
        },
        &rx,
    );
    drop(guard);
    let clean = handle.shutdown();
    if !clean {
        tracing::warn!("the sync engine did not stop in time; queued work resumes next start");
    }
    result
}

/// What the app needs once the terminal is in TUI mode.
struct Inside<'a> {
    account: String,
    env: &'a Env,
    caps: &'a probe::Capabilities,
    settings: Settings,
    ui: mejl_ui::UiConfig,
    warnings: &'a [String],
    tx: events::LoopSender<AppEvent>,
    handle: mejl_sync::SyncHandle,
    // The engine has its own clone; they share the session, which the app
    // needs for attachment downloads.
    client: Client,
    oauth: Option<mejl_jmap::OAuthCredentials>,
}

fn run_inside(inside: Inside<'_>, rx: &events::LoopReceiver<AppEvent>) -> Result<Report, CliError> {
    let Inside {
        account,
        env,
        caps,
        settings,
        ui,
        warnings,
        tx,
        handle,
        client,
        oauth,
    } = inside;
    let input =
        InputThread::spawn(tx.clone()).map_err(|e| failure("cannot read the terminal", e))?;
    let signal_tx = tx.clone();
    let _signals = SignalWatcher::spawn(move |kind| {
        let _ = signal_tx.send(LoopEvent::Signal(kind));
    })
    .map_err(|e| failure("cannot watch for signals", e))?;
    let host = TerminalHost::new(
        Some(input),
        EditorCommand::from_env(env),
        caps.synchronized_output == Support::Yes,
    );
    let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
        .map_err(|e| failure("cannot draw on the terminal", e))?;
    let mut app = App::new(AppParts {
        terminal,
        host: Box::new(host),
        handle,
        client,
        oauth,
        lock_retry: crate::app::LOCK_RETRY,
        tx,
        settings,
        ui,
        account,
        clock: Arc::new(mejl_ui::SystemClock),
        render_threads: crate::render_pool::default_threads(),
    })
    .map_err(|e| failure("cannot start", e))?;
    app.start(warnings)
        .map_err(|e| failure("cannot draw the first frame", e))?;
    let first_frame = since_start();
    tracing::info!(ms = first_frame.as_millis() as u64, "first frame drawn");
    let outcome = app.run(rx);
    let report = Report {
        held_sends: app.held_sends(),
        note: app.exit_note().map(str::to_owned),
        first_frame,
    };
    app.close();
    outcome.map_err(|e| failure("the terminal failed", e))?;
    Ok(report)
}
