//! The command line.
//!
//! ```text
//! mejl                  run the TUI for the selected account
//! mejl --demo           run against a built-in fictional mailbox, no account
//! mejl login            log in with the browser or an API token, and store it
//! mejl logout           forget the login and the local cache
//! mejl doctor           print paths, config check, stores and terminal support
//! mejl keys --check     check key overrides in the config
//! ```
//!
//! Global options: `--config <path>`, `--account <name>`, `--log-level
//! <level>`, `--version`, `--help`.
//!
//! Exit codes ([`exit`]): 0 success, 1 failure, 2 usage error, 3 config
//! error, 4 not logged in or token refused, 5 the account is open in
//! another mejl.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use mejl_jmap::transport::Transport;
use mejl_jmap::{Client, HttpTransport};

use crate::config::{self, AccountConfig, Config, ConfigError, Loaded};
use crate::demo::{DEMO_ACCOUNT, DemoBackend, DemoOptions};
use crate::doctor::{self, DoctorInput};
use crate::env::Env;
use crate::keys;
use crate::lock::{InstanceLock, LockError};
use crate::login::{self, Capability, LoginError, LoginMethod, TOKEN_SETTINGS_URL};
use crate::oauth_login::{self, OAuthLogin};
use crate::paths::Paths;
use crate::secrets::{KeyringStore, Login, SecretError, SecretStore, resolve_login};
use crate::terminal::probe;

/// Exit codes.
pub mod exit {
    /// Success.
    pub const OK: u8 = 0;
    /// Any other failure.
    pub const FAILURE: u8 = 1;
    /// Bad command line (clap's own code).
    pub const USAGE: u8 = 2;
    /// The config file is invalid.
    pub const CONFIG: u8 = 3;
    /// No token, or the server refused it.
    pub const AUTH: u8 = 4;
    /// Another mejl has the account open.
    pub const LOCKED: u8 = 5;
}

const EXIT_HELP: &str = "\
Exit codes:
  0  success
  1  failure
  2  usage error
  3  config error
  4  not logged in, or the token was refused
  5  the account is open in another mejl

Environment:
  MEJL_TOKEN  API token to use instead of the keyring, for scripts
  MEJL_LOG    log filter, such as debug or mejl=trace
  NO_COLOR    turn colour off";

/// mejl's arguments.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
#[command(
    name = "mejl",
    version,
    about = "A fast, keyboard-first terminal email client for Fastmail",
    after_help = EXIT_HELP,
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    /// Run against a built-in fictional mailbox. No account or network
    /// needed.
    #[arg(long)]
    pub demo: bool,

    /// Read this config file instead of the default one.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Use this account from the config.
    #[arg(long, global = true, value_name = "NAME")]
    pub account: Option<String>,

    /// Log level for the log file. Overrides MEJL_LOG.
    #[arg(
        long,
        global = true,
        value_name = "LEVEL",
        value_parser = ["off", "error", "warn", "info", "debug", "trace"]
    )]
    pub log_level: Option<String>,

    /// What to do. Without one, mejl starts.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Subcommands.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Log in with the browser (OAuth) or paste an API token, check the
    /// login with the server and store it in the OS keyring. With a
    /// terminal mejl asks which; without one it reads a token from stdin.
    Login {
        /// Log in with the browser, without asking.
        #[arg(long, conflicts_with = "token")]
        oauth: bool,
        /// Paste an API token, without asking.
        #[arg(long)]
        token: bool,
        /// Wait for the browser on this port of 127.0.0.1, for SSH port
        /// forwarding. Implies --oauth.
        #[arg(
            long,
            value_name = "PORT",
            conflicts_with = "token",
            value_parser = clap::value_parser!(u16).range(1..)
        )]
        port: Option<u16>,
    },
    /// Remove the stored login and the local cache. An OAuth login is
    /// revoked at the server first. The outbox and drafts are removed too
    /// after a confirmation.
    Logout {
        /// Also remove the outbox, drafts and rules without asking.
        #[arg(long)]
        yes: bool,
    },
    /// Print paths, the config check, the login, stores and terminal
    /// support. Never prints a token.
    Doctor {
        /// Print a commented example config and exit.
        #[arg(long)]
        print_config: bool,
        /// Do not query the terminal.
        #[arg(long)]
        no_probe: bool,
    },
    /// Key binding tools.
    Keys {
        /// Check the [keys] overrides in the config.
        #[arg(long)]
        check: bool,
    },
}

/// A failure with its exit code. The message is for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError {
    /// The exit code.
    pub code: u8,
    /// What went wrong, in plain words.
    pub message: String,
}

impl CliError {
    /// A failure with a code.
    pub fn new(code: u8, message: impl Into<String>) -> Self {
        CliError {
            code,
            message: message.into(),
        }
    }
}

impl From<ConfigError> for CliError {
    fn from(e: ConfigError) -> Self {
        CliError::new(exit::CONFIG, e.to_string())
    }
}

impl From<LockError> for CliError {
    fn from(e: LockError) -> Self {
        let code = match e {
            LockError::Held { .. } => exit::LOCKED,
            LockError::Io { .. } => exit::FAILURE,
        };
        CliError::new(code, e.to_string())
    }
}

impl From<SecretError> for CliError {
    fn from(e: SecretError) -> Self {
        CliError::new(exit::AUTH, e.to_string())
    }
}

impl From<LoginError> for CliError {
    fn from(e: LoginError) -> Self {
        let code = match e {
            LoginError::Unauthorized
            | LoginError::NoMail(_)
            | LoginError::OAuthNoMail(_)
            | LoginError::BadToken(_)
            | LoginError::Refused(_)
            | LoginError::WrongIssuer { .. }
            | LoginError::MissingIssuer(_) => exit::AUTH,
            _ => exit::FAILURE,
        };
        CliError::new(code, e.to_string())
    }
}

impl From<io::Error> for CliError {
    fn from(e: io::Error) -> Self {
        CliError::new(exit::FAILURE, e.to_string())
    }
}

/// What a command needs from the outside world. Tests build one over
/// temporary directories, an in-memory secret store and fake input.
pub struct Context<'a> {
    /// mejl's directories, with `--config` already applied.
    pub paths: Paths,
    /// The environment.
    pub env: Env,
    /// Where logins are kept. Shared, because the TUI's OAuth client saves
    /// refreshed tokens from other threads.
    pub store: Arc<dyn SecretStore>,
    /// The HTTP transport for login. `None` builds the real one when needed.
    pub transport: Option<Arc<dyn Transport>>,
    /// Standard input.
    pub stdin: &'a mut dyn BufRead,
    /// Whether standard input is a terminal (prompts in raw mode).
    pub stdin_is_tty: bool,
    /// Whether standard output is a terminal (the probe runs).
    pub stdout_is_tty: bool,
    /// Opens a URL in the browser, for the OAuth login.
    pub open_url: &'a dyn Fn(&str) -> Result<(), String>,
    /// How long the OAuth login waits for the browser.
    pub oauth_timeout: Duration,
}

/// Opens a URL with the system's browser without waiting for it.
pub fn open_in_browser(url: &str) -> Result<(), String> {
    open::that_detached(url).map_err(|e| e.to_string())
}

impl Context<'_> {
    fn transport(&self) -> Result<Arc<dyn Transport>, CliError> {
        match &self.transport {
            Some(t) => Ok(t.clone()),
            None => HttpTransport::new()
                .map(|t| Arc::new(t) as Arc<dyn Transport>)
                .map_err(|e| CliError::new(exit::FAILURE, e.to_string())),
        }
    }
}

/// Runs mejl as the process: parses arguments, sets up logging, the panic
/// hook and the real keyring, and maps the result to an exit code.
pub fn main() -> ExitCode {
    crate::tui::mark_start();
    let cli = Cli::parse();
    let env = Env::from_process();
    let paths = match Paths::from_system() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("mejl: {e}");
            return ExitCode::from(exit::FAILURE);
        }
    };
    let paths = match &cli.config {
        Some(file) => paths.with_config_file(file.clone()),
        None => paths,
    };
    let _log = start_logging(&cli, &env, &paths);
    crate::terminal::install_panic_hook();
    let stdin = io::stdin();
    let stdin_is_tty = stdin.is_terminal();
    let mut stdin = stdin.lock();
    let mut ctx = Context {
        paths,
        env,
        store: Arc::new(KeyringStore::new()),
        transport: None,
        stdin: &mut stdin,
        stdin_is_tty,
        stdout_is_tty: io::stdout().is_terminal(),
        open_url: &open_in_browser,
        oauth_timeout: oauth_login::WAIT_TIMEOUT,
    };
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    match run(&cli, &mut ctx, &mut stdout, &mut stderr) {
        Ok(()) => ExitCode::from(exit::OK),
        Err(e) => {
            tracing::error!(code = e.code, "{}", e.message);
            let _ = writeln!(stderr, "mejl: {}", e.message);
            ExitCode::from(e.code)
        }
    }
}

fn start_logging(cli: &Cli, env: &Env, paths: &Paths) -> Option<crate::logging::LogGuard> {
    let filter = match crate::logging::build_filter(
        cli.log_level.as_deref(),
        env.get(crate::env::var::MEJL_LOG),
    ) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("mejl: {e}; logging is off");
            return None;
        }
    };
    match crate::logging::init(&paths.log_dir(), filter) {
        Ok(guard) => Some(guard),
        Err(e) => {
            eprintln!("mejl: {e}; logging is off");
            None
        }
    }
}

/// Runs one command. `out` gets results, `err` gets warnings and prompts.
pub fn run(
    cli: &Cli,
    ctx: &mut Context<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), CliError> {
    tracing::info!(version = env!("CARGO_PKG_VERSION"), command = ?cli.command, demo = cli.demo, "mejl starting");
    match &cli.command {
        None if cli.demo => run_demo(ctx, out, err),
        None => run_tui(cli, ctx, out, err),
        Some(Command::Login { oauth, token, port }) => {
            let method = if *oauth || port.is_some() {
                Some(LoginMethod::OAuth)
            } else if *token {
                Some(LoginMethod::ApiToken)
            } else {
                None
            };
            run_login(cli, ctx, method, *port, out, err)
        }
        Some(Command::Logout { yes }) => run_logout(cli, ctx, *yes, out, err),
        Some(Command::Doctor {
            print_config,
            no_probe,
        }) => run_doctor(cli, ctx, *print_config, *no_probe, out),
        Some(Command::Keys { check }) => run_keys(ctx, *check, out, err),
    }
}

/// Loads the config and prints its warnings.
fn load_config(ctx: &Context<'_>, err: &mut dyn Write) -> Result<Loaded, CliError> {
    let loaded = config::load(&ctx.paths.config_file)?;
    for w in &loaded.warnings {
        let _ = writeln!(
            err,
            "mejl: warning in {}: {w}",
            ctx.paths.config_file.display()
        );
    }
    Ok(loaded)
}

fn select_account(cli: &Cli, config: &Config) -> Result<AccountConfig, CliError> {
    Ok(config.account(cli.account.as_deref())?)
}

/// The message for `mejl` and `mejl --demo` when there is no terminal to
/// draw on.
pub const NEEDS_TERMINAL: &str =
    "mejl needs an interactive terminal; run it in one, or use a subcommand such as `mejl doctor`";

fn require_terminal(ctx: &Context<'_>) -> Result<(), CliError> {
    if ctx.stdin_is_tty && ctx.stdout_is_tty {
        Ok(())
    } else {
        Err(CliError::new(exit::FAILURE, NEEDS_TERMINAL))
    }
}

fn print_report(report: &crate::tui::Report, out: &mut dyn Write) -> Result<(), CliError> {
    if let Some(note) = &report.note {
        writeln!(out, "mejl stopped: {note}")?;
    }
    match report.held_sends {
        0 => {}
        1 => writeln!(
            out,
            "1 message is still queued to send. It goes out the next time mejl runs."
        )?,
        n => writeln!(
            out,
            "{n} messages are still queued to send. They go out the next time mejl runs."
        )?,
    }
    Ok(())
}

fn run_tui(
    cli: &Cli,
    ctx: &mut Context<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), CliError> {
    let loaded = load_config(ctx, err)?;
    let account = select_account(cli, &loaded.config)?;
    let dir = ctx.paths.ensure_account_dir(&account.name)?;
    let _lock = InstanceLock::acquire(&dir)?;
    let Some(found) = resolve_login(&account, &ctx.env, &*ctx.store)? else {
        return Err(CliError::new(
            exit::AUTH,
            format!(
                "no login for account {:?}. Run `mejl login{}`, or set MEJL_TOKEN",
                account.name,
                account_flag(cli)
            ),
        ));
    };
    // Checked after the setup, so a broken config or a missing token is
    // reported the same way with or without a terminal.
    require_terminal(ctx)?;
    let transport = ctx.transport()?;
    let (client, oauth) = match found {
        Login::Token(token, source) => {
            tracing::info!(account = %account.name, token = source.describe(), "starting");
            (Client::new(transport, &account.session_url, token), None)
        }
        Login::OAuth(record) => {
            tracing::info!(account = %account.name, "starting with an OAuth login");
            let (client, session) = crate::oauth::oauth_client(
                transport,
                &account.session_url,
                ctx.store.clone(),
                &account.name,
                *record,
            )
            .map_err(|e| {
                CliError::new(
                    exit::AUTH,
                    format!(
                        "the stored OAuth login cannot be used: {e}. Run `mejl login{}`",
                        account_flag(cli)
                    ),
                )
            })?;
            (client, Some(session))
        }
    };
    let caps_file = ctx.paths.caps_file();
    let report = crate::tui::run_with_caps_cache(
        crate::tui::Launch {
            account: account.name.clone(),
            store_dir: dir,
            client,
            oauth,
            engine: crate::tui::engine_config(&loaded.config),
            config: loaded.config,
            settings_path: ctx.paths.config_file.clone(),
            env: ctx.env.clone(),
        },
        Some(&caps_file),
    )?;
    print_report(&report, out)
}

fn account_flag(cli: &Cli) -> String {
    cli.account
        .as_deref()
        .map(|a| format!(" --account {a}"))
        .unwrap_or_default()
}

/// Copies the user's config into the demo's directory, so the demo uses
/// their theme and keys, and settings changed in the demo never touch the
/// real file.
fn demo_config(ctx: &Context<'_>, demo: &DemoBackend) -> Result<std::path::PathBuf, CliError> {
    let paths = demo.paths();
    crate::paths::create_private_dir(&paths.config_dir)?;
    match std::fs::read_to_string(&ctx.paths.config_file) {
        Ok(text) => std::fs::write(&paths.config_file, text)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(paths.config_file)
}

fn run_demo(
    ctx: &mut Context<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), CliError> {
    let loaded = load_config(ctx, err)?;
    require_terminal(ctx)?;
    let demo = DemoBackend::start(DemoOptions::interactive())?;
    let settings_path = demo_config(ctx, &demo)?;
    let dir = demo.paths().ensure_account_dir(DEMO_ACCOUNT)?;
    {
        // Rules and saved searches are local only, so the demo brings its
        // own, as in the mock-ups. The store closes before the engine opens
        // it.
        let mut store = mejl_store::Store::open(&dir, crate::tui::store_options(&loaded.config))
            .map_err(|e| {
                CliError::new(exit::FAILURE, format!("cannot open the demo store: {e}"))
            })?;
        crate::demo::seed_local(&mut store, demo.server(), jiff::Timestamp::now()).map_err(
            |e| CliError::new(exit::FAILURE, format!("cannot fill the demo store: {e}")),
        )?;
    }
    let transport: Arc<dyn Transport> =
        Arc::new(HttpTransport::new().map_err(|e| CliError::new(exit::FAILURE, e.to_string()))?);
    let client = Client::new(transport, demo.session_url(), demo.token());
    let mut engine = crate::tui::engine_config(&loaded.config);
    engine.push = true;
    let report = crate::tui::run(crate::tui::Launch {
        account: DEMO_ACCOUNT.to_owned(),
        store_dir: dir,
        client,
        oauth: None,
        engine,
        config: loaded.config,
        settings_path,
        env: ctx.env.clone(),
    })?;
    // The demo's stores are removed with its directory, so a held send
    // never goes anywhere; say nothing about it.
    let report = crate::tui::Report {
        held_sends: 0,
        ..report
    };
    print_report(&report, out)?;
    drop(demo);
    Ok(())
}

fn run_login(
    cli: &Cli,
    ctx: &mut Context<'_>,
    method: Option<LoginMethod>,
    port: Option<u16>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), CliError> {
    let loaded = load_config(ctx, err)?;
    let account = select_account(cli, &loaded.config)?;
    // A running mejl would keep using the old login, and an OAuth login
    // there holds a refresh token this login makes useless.
    let dir = ctx.paths.ensure_account_dir(&account.name)?;
    let _lock = InstanceLock::acquire(&dir)?;
    let method = match method {
        Some(m) => m,
        None if ctx.stdin_is_tty => ask_method(ctx, &account, err)?,
        // Scripts pipe a token in.
        None => LoginMethod::ApiToken,
    };
    match method {
        LoginMethod::OAuth => login_oauth(ctx, &account, port, out, err)?,
        LoginMethod::ApiToken => login_token(ctx, &account, out, err)?,
    }
    if ctx.env.non_empty(crate::env::var::MEJL_TOKEN).is_some() {
        writeln!(
            err,
            "Note: MEJL_TOKEN is set in this environment, and mejl uses it instead of the stored login."
        )?;
    } else if account.token_command.is_some() {
        writeln!(
            err,
            "Note: account {:?} has a token_command, and mejl uses it instead of the stored login.",
            account.name
        )?;
    }
    Ok(())
}

/// Asks which way to log in. An empty answer picks the browser.
fn ask_method(
    ctx: &mut Context<'_>,
    account: &AccountConfig,
    err: &mut dyn Write,
) -> Result<LoginMethod, CliError> {
    writeln!(
        err,
        "How do you want to log in to account {:?}?\n  1  Log in with your browser (OAuth)\n  2  Paste an API token",
        account.name
    )?;
    loop {
        write!(err, "Choose 1 or 2 [1]: ")?;
        err.flush()?;
        let mut answer = String::new();
        if ctx.stdin.read_line(&mut answer)? == 0 {
            return Err(LoginError::Cancelled.into());
        }
        match answer.trim() {
            "" | "1" => return Ok(LoginMethod::OAuth),
            "2" => return Ok(LoginMethod::ApiToken),
            _ => writeln!(err, "Please answer 1 or 2.")?,
        }
    }
}

fn login_oauth(
    ctx: &mut Context<'_>,
    account: &AccountConfig,
    port: Option<u16>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), CliError> {
    let opts = OAuthLogin {
        transport: ctx.transport()?,
        session_url: &account.session_url,
        account: &account.name,
        client_id: account.oauth_client_id.as_deref(),
        port,
        timeout: ctx.oauth_timeout,
        open: ctx.open_url,
    };
    let outcome = oauth_login::login_with_oauth(&opts, &*ctx.store, err)?;
    for line in outcome.report.oauth_lines(&outcome.record.tokens.scope) {
        writeln!(out, "{line}")?;
    }
    writeln!(
        out,
        "Login saved in the {} for account {:?}.",
        ctx.store.describe(),
        account.name
    )?;
    if outcome.replaced_token {
        writeln!(
            out,
            "The API token stored for this account was removed; this login replaces it."
        )?;
    }
    Ok(())
}

fn login_token(
    ctx: &mut Context<'_>,
    account: &AccountConfig,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), CliError> {
    let scopes: Vec<&str> = Capability::ALL.iter().map(|c| c.scope_name()).collect();
    let token = if ctx.stdin_is_tty {
        writeln!(
            err,
            "Log in to account {:?} at {}.\nCreate an API token at {TOKEN_SETTINGS_URL}\nwith the scopes {}. The token is not shown as you paste it.",
            account.name,
            account.session_url,
            scopes.join(", ")
        )?;
        login::prompt_token("API token: ")?
    } else {
        login::read_token_line(ctx.stdin)?
    };
    let token = token.ok_or_else(|| CliError::from(LoginError::Cancelled))?;
    let token = login::checked(token)?;
    let outcome = login::login_with_token(
        ctx.transport()?,
        &account.session_url,
        &account.name,
        token,
        &*ctx.store,
    )?;
    for line in outcome.report.lines() {
        writeln!(out, "{line}")?;
    }
    writeln!(
        out,
        "Token saved in the {} for account {:?}.",
        ctx.store.describe(),
        account.name
    )?;
    match outcome.replaced_oauth {
        None => {}
        Some(Ok(())) => writeln!(
            out,
            "The OAuth login stored for this account was revoked; this token replaces it."
        )?,
        Some(Err(why)) => writeln!(
            out,
            "The OAuth login stored for this account was removed, but revoking it at the server failed ({why}); this token replaces it."
        )?,
    }
    Ok(())
}

fn run_logout(
    cli: &Cli,
    ctx: &mut Context<'_>,
    yes: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), CliError> {
    let loaded = load_config(ctx, err)?;
    let account = select_account(cli, &loaded.config)?;
    let dir = ctx.paths.account_dir(&account.name);
    // Never pull files out from under a running mejl.
    let lock = if dir.is_dir() {
        Some(InstanceLock::acquire(&dir)?)
    } else {
        None
    };
    // A damaged record cannot be revoked, but it is removed below.
    let record = match ctx.store.get_oauth(&account.name) {
        Ok(record) => record,
        Err(SecretError::Corrupt(_)) => None,
        Err(e) => return Err(e.into()),
    };
    if let Some(record) = &record {
        // Revoking needs no refresh, so this never builds an OAuth client.
        match revoke_login(ctx, record) {
            Ok(()) => writeln!(out, "Revoked the OAuth login at the server.")?,
            Err(why) => writeln!(
                err,
                "Could not revoke the OAuth login at the server ({why}). It is removed here anyway; to end it at the server too, remove mejl from the connected apps in your Fastmail settings."
            )?,
        }
    }
    let had_oauth = ctx.store.delete_oauth(&account.name)?;
    let had_token = ctx.store.delete(&account.name)?;
    if had_oauth {
        writeln!(
            out,
            "Removed the OAuth login for {:?} from the {}.",
            account.name,
            ctx.store.describe()
        )?;
    }
    if had_token {
        writeln!(
            out,
            "Removed the token for {:?} from the {}.",
            account.name,
            ctx.store.describe()
        )?;
    }
    if !had_oauth && !had_token {
        writeln!(out, "No stored login for {:?}.", account.name)?;
    }
    if lock.is_some() {
        let removed = remove_store(&dir, mejl_store::CACHE_FILE)?;
        if removed {
            writeln!(out, "Removed the local cache (cache.db).")?;
        }
        let has_state = store_exists(&dir, mejl_store::STATE_FILE);
        if has_state {
            let confirmed = yes || (ctx.stdin_is_tty && confirm(ctx, err)?);
            if confirmed {
                remove_store(&dir, mejl_store::STATE_FILE)?;
                writeln!(out, "Removed the outbox, drafts and rules (state.db).")?;
            } else {
                writeln!(
                    out,
                    "Kept state.db (outbox, drafts and rules) in {}. Run `mejl logout --yes` to remove it.",
                    dir.display()
                )?;
            }
        }
    }
    if ctx.env.non_empty(crate::env::var::MEJL_TOKEN).is_some() {
        writeln!(err, "Note: MEJL_TOKEN is still set in this environment.")?;
    }
    drop(lock);
    Ok(())
}

/// Revokes a stored OAuth login on a small runtime of its own.
fn revoke_login(ctx: &Context<'_>, record: &crate::oauth::OAuthRecord) -> Result<(), String> {
    let transport = ctx.transport().map_err(|e| e.message)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(crate::oauth::revoke(&*transport, record))
}

fn confirm(ctx: &mut Context<'_>, err: &mut dyn Write) -> Result<bool, CliError> {
    write!(
        err,
        "Also remove the outbox, unsent drafts and rules for this account? [y/N] "
    )?;
    err.flush()?;
    let mut answer = String::new();
    ctx.stdin.read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn store_exists(dir: &std::path::Path, file: &str) -> bool {
    dir.join(file).exists()
}

/// Removes a SQLite file and its write-ahead log files. Returns whether the
/// main file existed.
fn remove_store(dir: &std::path::Path, file: &str) -> io::Result<bool> {
    let existed = dir.join(file).exists();
    for suffix in ["", "-wal", "-shm", "-journal"] {
        match std::fs::remove_file(dir.join(format!("{file}{suffix}"))) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(existed)
}

fn run_doctor(
    cli: &Cli,
    ctx: &mut Context<'_>,
    print_config: bool,
    no_probe: bool,
    out: &mut dyn Write,
) -> Result<(), CliError> {
    if print_config {
        write!(out, "{}", config::example_config())?;
        return Ok(());
    }
    let loaded = config::load(&ctx.paths.config_file);
    let probe_timeout =
        (!no_probe && ctx.stdin_is_tty && ctx.stdout_is_tty).then_some(probe::DEFAULT_TIMEOUT);
    let caps = probe::detect(&ctx.env, probe_timeout);
    let input = DoctorInput {
        paths: &ctx.paths,
        env: &ctx.env,
        config: loaded.as_ref(),
        account: cli.account.as_deref(),
        store: &*ctx.store,
        caps: &caps,
    };
    let outcome = doctor::report(&input, out)?;
    if outcome.config_errors {
        return Err(CliError::new(
            exit::CONFIG,
            "the config file has errors (see above)",
        ));
    }
    Ok(())
}

fn run_keys(
    ctx: &mut Context<'_>,
    check: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), CliError> {
    if !check {
        writeln!(out, "Use `mejl keys --check` to check your key overrides.")?;
        return Ok(());
    }
    let loaded = load_config(ctx, err)?;
    let report = keys::check_keys(&loaded.config);
    for line in &report.lines {
        writeln!(out, "{line}")?;
    }
    if report.has_errors {
        return Err(CliError::new(exit::CONFIG, "the key overrides have errors"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemoryStore;
    use mejl_jmap::Secret;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("mejl").chain(args.iter().copied()))
    }

    #[test]
    fn parses_every_form() {
        assert_eq!(parse(&[]).unwrap().command, None);
        assert!(parse(&["--demo"]).unwrap().demo);
        let login = |oauth, token, port| Some(Command::Login { oauth, token, port });
        assert_eq!(
            parse(&["login"]).unwrap().command,
            login(false, false, None)
        );
        assert_eq!(
            parse(&["login", "--oauth"]).unwrap().command,
            login(true, false, None)
        );
        assert_eq!(
            parse(&["login", "--token"]).unwrap().command,
            login(false, true, None)
        );
        assert_eq!(
            parse(&["login", "--port", "8765"]).unwrap().command,
            login(false, false, Some(8765))
        );
        assert!(parse(&["login", "--oauth", "--token"]).is_err());
        assert!(parse(&["login", "--token", "--port", "8765"]).is_err());
        assert!(parse(&["login", "--port", "0"]).is_err());
        assert_eq!(
            parse(&["logout", "--yes"]).unwrap().command,
            Some(Command::Logout { yes: true })
        );
        assert_eq!(
            parse(&["doctor", "--print-config"]).unwrap().command,
            Some(Command::Doctor {
                print_config: true,
                no_probe: false
            })
        );
        assert_eq!(
            parse(&["keys", "--check"]).unwrap().command,
            Some(Command::Keys { check: true })
        );
        let cli = parse(&[
            "--config",
            "/x/c.toml",
            "--account",
            "work",
            "--log-level",
            "debug",
        ])
        .unwrap();
        assert_eq!(cli.config, Some(PathBuf::from("/x/c.toml")));
        assert_eq!(cli.account.as_deref(), Some("work"));
        assert_eq!(cli.log_level.as_deref(), Some("debug"));
        // Global options also work after the subcommand.
        let cli = parse(&["doctor", "--account", "work"]).unwrap();
        assert_eq!(cli.account.as_deref(), Some("work"));
    }

    #[test]
    fn usage_errors() {
        use clap::error::ErrorKind;
        assert_eq!(
            parse(&["--log-level", "loud"]).unwrap_err().kind(),
            ErrorKind::InvalidValue
        );
        assert_eq!(
            parse(&["frobnicate"]).unwrap_err().kind(),
            ErrorKind::InvalidSubcommand
        );
        assert!(parse(&["--demo", "login"]).is_err());
        assert_eq!(
            parse(&["--version"]).unwrap_err().kind(),
            ErrorKind::DisplayVersion
        );
        let help = parse(&["--help"]).unwrap_err().to_string();
        assert!(help.contains("Exit codes"), "{help}");
        assert!(help.contains("MEJL_TOKEN"));
        assert_eq!(
            parse(&["--log-level", "loud"]).unwrap_err().exit_code(),
            i32::from(exit::USAGE)
        );
    }

    #[test]
    fn verify_cli_definition() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    type Opener = Box<dyn Fn(&str) -> Result<(), String>>;

    struct Harness {
        _base: tempfile::TempDir,
        paths: Paths,
        store: Arc<MemoryStore>,
        env: Env,
        transport: Option<Arc<dyn Transport>>,
        stdin: io::Cursor<Vec<u8>>,
        stdin_is_tty: bool,
        opener: Opener,
        oauth_timeout: Duration,
    }

    impl Harness {
        fn new() -> Self {
            let base = tempfile::tempdir().unwrap();
            let paths = Paths::under(base.path());
            Harness {
                _base: base,
                paths,
                store: Arc::new(MemoryStore::new()),
                env: Env::default(),
                transport: None,
                stdin: io::Cursor::new(Vec::new()),
                stdin_is_tty: false,
                opener: Box::new(|_| panic!("no browser in this test")),
                oauth_timeout: Duration::from_secs(20),
            }
        }

        fn write_config(&self, text: &str) {
            std::fs::create_dir_all(&self.paths.config_dir).unwrap();
            std::fs::write(&self.paths.config_file, text).unwrap();
        }

        fn run(&mut self, args: &[&str]) -> (Result<(), CliError>, String, String) {
            let cli = parse(args).unwrap();
            let mut ctx = Context {
                paths: self.paths.clone(),
                env: self.env.clone(),
                store: self.store.clone(),
                transport: self.transport.clone(),
                stdin: &mut self.stdin,
                stdin_is_tty: self.stdin_is_tty,
                stdout_is_tty: false,
                open_url: &*self.opener,
                oauth_timeout: self.oauth_timeout,
            };
            let mut out = Vec::new();
            let mut err = Vec::new();
            let result = run(&cli, &mut ctx, &mut out, &mut err);
            (
                result,
                String::from_utf8(out).unwrap(),
                String::from_utf8(err).unwrap(),
            )
        }
    }

    #[test]
    fn run_without_token_says_how_to_log_in() {
        let mut h = Harness::new();
        let (result, _, _) = h.run(&["--account", "work"]);
        let e = result.unwrap_err();
        assert_eq!(e.code, exit::AUTH);
        assert!(
            e.message.contains("mejl login --account work"),
            "{}",
            e.message
        );
    }

    #[test]
    fn run_with_token_checks_setup_and_locks() {
        let mut h = Harness::new();
        h.store
            .set("default", &Secret::new("fmu1-test-token-0001"))
            .unwrap();
        // Without a terminal the setup is checked, then mejl stops before
        // it builds a client, so nothing is contacted.
        let (result, out, _) = h.run(&[]);
        let e = result.unwrap_err();
        assert_eq!(e.code, exit::FAILURE);
        assert_eq!(e.message, NEEDS_TERMINAL);
        assert!(!out.contains("fmu1-test-token"));
        // A second mejl on the same account is refused first.
        let dir = h.paths.account_dir("default");
        let _held = InstanceLock::acquire(&dir).unwrap();
        let (result, _, _) = h.run(&[]);
        assert_eq!(result.unwrap_err().code, exit::LOCKED);
    }

    #[test]
    fn config_errors_exit_with_the_config_code() {
        let mut h = Harness::new();
        h.write_config("[ui]\nmouse = \"no\"\n");
        let (result, _, _) = h.run(&[]);
        let e = result.unwrap_err();
        assert_eq!(e.code, exit::CONFIG);
        assert!(e.message.contains("line 2"), "{}", e.message);
        let (result, out, _) = h.run(&["doctor", "--no-probe"]);
        assert_eq!(result.unwrap_err().code, exit::CONFIG);
        assert!(out.contains("Paths"));
    }

    #[test]
    fn config_warnings_go_to_stderr() {
        let mut h = Harness::new();
        h.write_config("[ui]\nmouce = true\n");
        let (result, out, err) = h.run(&["keys", "--check"]);
        result.unwrap();
        assert!(err.contains("did you mean `ui.mouse`"), "{err}");
        assert!(out.contains("no [keys] overrides"), "{out}");
    }

    #[test]
    fn config_flag_is_honoured() {
        let mut h = Harness::new();
        let other = h._base.path().join("elsewhere.toml");
        std::fs::write(&other, "[keys.list]\nj = \"next\"\n").unwrap();
        h.paths = h.paths.clone().with_config_file(other);
        let (result, out, _) = h.run(&["keys", "--check"]);
        result.unwrap();
        assert!(out.contains("1 [keys] override"), "{out}");
    }

    #[test]
    fn doctor_print_config_and_report() {
        let mut h = Harness::new();
        let (result, out, _) = h.run(&["doctor", "--print-config"]);
        result.unwrap();
        assert_eq!(out, config::example_config());
        h.env = Env::default().with(crate::env::var::MEJL_TOKEN, "fmu1-very-secret-doctor-0001");
        let (result, out, _) = h.run(&["doctor"]);
        result.unwrap();
        assert!(out.contains("Terminal"));
        assert!(!out.contains("very-secret"), "{out}");
    }

    #[test]
    fn keys_without_check_explains() {
        let mut h = Harness::new();
        let (result, out, _) = h.run(&["keys"]);
        result.unwrap();
        assert!(out.contains("--check"));
    }

    fn serve() -> (
        tokio::runtime::Runtime,
        mejl_fake::HttpHandle,
        mejl_fake::FakeServer,
    ) {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let server = mejl_fake::FakeServer::demo("2026-10-23T14:27:00Z".parse().unwrap());
        let http = rt
            .block_on(server.serve_http("127.0.0.1:0".parse().unwrap()))
            .unwrap();
        (rt, http, server)
    }

    #[test]
    fn login_from_piped_stdin_against_loopback() {
        let (_rt, http, server) = serve();
        let mut h = Harness::new();
        h.write_config(&format!(
            "[accounts.demo]\nsession_url = \"{}\"\n",
            http.session_url()
        ));
        h.stdin = io::Cursor::new(format!("{}\n", server.token()).into_bytes());
        let (result, out, _) = h.run(&["login"]);
        result.unwrap();
        assert!(out.contains("Signed in as mira@example.org"), "{out}");
        assert!(out.contains("Masked Email: yes"));
        assert!(out.contains("Token saved in the memory store for account \"demo\""));
        assert_eq!(
            h.store.get("demo").unwrap().unwrap().expose(),
            server.token()
        );
        // The address is printed, never written into the config.
        let config = std::fs::read_to_string(&h.paths.config_file).unwrap();
        assert!(!config.contains("mira@"));
    }

    #[test]
    fn login_with_a_bad_token_exits_with_auth() {
        let (_rt, http, _server) = serve();
        let mut h = Harness::new();
        h.write_config(&format!(
            "[accounts.demo]\nsession_url = \"{}\"\n",
            http.session_url()
        ));
        h.stdin = io::Cursor::new(b"fmu1-not-the-right-token\n".to_vec());
        let (result, _, _) = h.run(&["login"]);
        let e = result.unwrap_err();
        assert_eq!(e.code, exit::AUTH);
        assert!(e.message.contains("401"));
        assert!(h.store.get("demo").unwrap().is_none());
    }

    #[test]
    fn login_with_empty_input_is_cancelled_or_refused() {
        let mut h = Harness::new();
        let (result, _, _) = h.run(&["login"]);
        assert_eq!(result.unwrap_err().code, exit::FAILURE);
        h.stdin = io::Cursor::new(b"two words\n".to_vec());
        let (result, _, _) = h.run(&["login"]);
        let e = result.unwrap_err();
        assert_eq!(e.code, exit::AUTH);
        assert!(!e.message.contains("words"));
    }

    #[test]
    fn logout_removes_token_and_cache_and_asks_about_state() {
        let mut h = Harness::new();
        h.store
            .set("default", &Secret::new("fmu1-test-token-0002"))
            .unwrap();
        let dir = h.paths.ensure_account_dir("default").unwrap();
        for f in ["cache.db", "cache.db-wal", "state.db", "state.db-wal"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        // Not a terminal and no --yes: state.db stays.
        let (result, out, _) = h.run(&["logout"]);
        result.unwrap();
        assert!(out.contains("Removed the token"), "{out}");
        assert!(out.contains("Removed the local cache"));
        assert!(out.contains("Kept state.db"));
        assert!(!dir.join("cache.db").exists());
        assert!(!dir.join("cache.db-wal").exists());
        assert!(dir.join("state.db").exists());
        assert!(h.store.get("default").unwrap().is_none());

        // A terminal that answers "n" keeps it; "y" removes it.
        h.stdin_is_tty = true;
        h.stdin = io::Cursor::new(b"n\n".to_vec());
        let (result, _, err) = h.run(&["logout"]);
        result.unwrap();
        assert!(err.contains("[y/N]"));
        assert!(dir.join("state.db").exists());
        h.stdin = io::Cursor::new(b"y\n".to_vec());
        let (result, out, _) = h.run(&["logout"]);
        result.unwrap();
        assert!(out.contains("Removed the outbox"), "{out}");
        assert!(!dir.join("state.db").exists());
        assert!(!dir.join("state.db-wal").exists());
    }

    #[test]
    fn logout_refuses_while_mejl_runs() {
        let mut h = Harness::new();
        let dir = h.paths.ensure_account_dir("default").unwrap();
        std::fs::write(dir.join("cache.db"), b"x").unwrap();
        let _held = InstanceLock::acquire(&dir).unwrap();
        let (result, _, _) = h.run(&["logout", "--yes"]);
        assert_eq!(result.unwrap_err().code, exit::LOCKED);
        assert!(dir.join("cache.db").exists());
    }

    #[test]
    fn logout_reports_a_locked_keyring() {
        let mut h = Harness::new();
        h.store.fail_with(Some("locked"));
        let (result, _, _) = h.run(&["logout"]);
        assert_eq!(result.unwrap_err().code, exit::AUTH);
    }

    #[test]
    fn demo_needs_a_terminal_and_starts_nothing_without_one() {
        let mut h = Harness::new();
        let (result, out, _) = h.run(&["--demo"]);
        let e = result.unwrap_err();
        assert_eq!(e.message, NEEDS_TERMINAL);
        assert!(out.is_empty());
    }

    #[test]
    fn keys_check_reports_bad_overrides() {
        let mut h = Harness::new();
        h.write_config("[keys.list]\nj = \"nxet\"\n");
        let (result, out, _) = h.run(&["keys", "--check"]);
        assert_eq!(result.unwrap_err().code, exit::CONFIG);
        assert!(out.contains("nxet"), "{out}");
    }

    #[test]
    fn demo_config_is_a_copy_in_the_demo_dir() {
        let h = Harness::new();
        h.write_config("[ui]\ntheme = \"mono\"\n");
        let demo = DemoBackend::start(DemoOptions::default()).unwrap();
        let mut stdin = io::Cursor::new(Vec::new());
        let ctx = Context {
            paths: h.paths.clone(),
            env: Env::default(),
            store: h.store.clone(),
            transport: None,
            stdin: &mut stdin,
            stdin_is_tty: false,
            stdout_is_tty: false,
            open_url: &|_| Ok(()),
            oauth_timeout: Duration::from_secs(1),
        };
        let copy = demo_config(&ctx, &demo).unwrap();
        assert!(copy.starts_with(demo.base_dir()));
        assert_ne!(copy, h.paths.config_file);
        assert!(std::fs::read_to_string(&copy).unwrap().contains("mono"));
    }

    // ----- OAuth, against the fake on loopback HTTP only -----

    use std::io::Read as _;
    use std::net::TcpStream;
    use std::sync::Mutex;

    /// A plain HTTP/1.1 GET that never follows a redirect: the status, the
    /// headers and the body.
    fn http_get(url: &str) -> (u16, Vec<(String, String)>, String) {
        let rest = url.strip_prefix("http://").expect("loopback http only");
        let split = rest.find('/').unwrap_or(rest.len());
        let (authority, path) = rest.split_at(split);
        assert!(authority.starts_with("127.0.0.1:"), "{url}");
        let path = if path.is_empty() { "/" } else { path };
        let mut stream = TcpStream::connect(authority).unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut text = String::new();
        stream.read_to_string(&mut text).unwrap();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let mut lines = head.lines();
        let status = lines
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
            .collect();
        (status, headers, body.to_owned())
    }

    /// `url` with query parameter `key` set to `value`, or removed.
    fn with_param(url: &str, key: &str, value: Option<&str>) -> String {
        let (base, query) = url.split_once('?').unwrap();
        let mut pairs: Vec<String> = query
            .split('&')
            .filter(|p| p.split('=').next() != Some(key))
            .map(str::to_owned)
            .collect();
        if let Some(v) = value {
            pairs.push(format!("{key}={v}"));
        }
        format!("{base}?{}", pairs.join("&"))
    }

    /// How the test browser comes back to mejl.
    #[derive(Clone, Copy, PartialEq)]
    enum Visit {
        Normal,
        WrongStateFirst,
        WrongIss,
        NoIss,
    }

    /// A browser: it loads the authorization URL without following the
    /// redirect, opens an empty connection and asks for the favicon like a
    /// real one, then follows the redirect to mejl's listener. It runs on
    /// its own thread, as a real browser would. Returns the URLs opened and
    /// the pages mejl answered.
    /// Strings collected by the test browser's thread.
    type Seen = Arc<Mutex<Vec<String>>>;

    /// The first `n` pages the test browser got. Its thread may still be
    /// reading the last one when mejl has moved on.
    fn pages_seen(pages: &Seen, n: usize) -> Vec<String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let got = pages.lock().unwrap().clone();
            if got.len() >= n || std::time::Instant::now() > deadline {
                return got;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn browser(visit: Visit) -> (Opener, Seen, Seen) {
        let opened = Arc::new(Mutex::new(Vec::new()));
        let pages = Arc::new(Mutex::new(Vec::new()));
        let (log, shown) = (opened.clone(), pages.clone());
        let opener: Opener = Box::new(move |url: &str| {
            log.lock().unwrap().push(url.to_owned());
            let url = url.to_owned();
            let shown = shown.clone();
            std::thread::spawn(move || {
                let (status, headers, _) = http_get(&url);
                assert_eq!(status, 302, "{url}");
                let location = headers
                    .iter()
                    .find(|(k, _)| k == "location")
                    .map(|(_, v)| v.clone())
                    .unwrap();
                let authority = location
                    .strip_prefix("http://")
                    .and_then(|r| r.split('/').next())
                    .unwrap()
                    .to_owned();
                drop(TcpStream::connect(&authority).unwrap());
                let (status, _, _) = http_get(&format!("http://{authority}/favicon.ico"));
                assert_eq!(status, 404);
                let target = match visit {
                    Visit::Normal => location.clone(),
                    Visit::WrongStateFirst => {
                        let forged = with_param(&location, "state", Some("not-this-one"));
                        let (status, _, body) = http_get(&forged);
                        assert_eq!(status, 400);
                        shown.lock().unwrap().push(body);
                        location.clone()
                    }
                    Visit::WrongIss => {
                        with_param(&location, "iss", Some("https%3A%2F%2Fevil.example.org"))
                    }
                    Visit::NoIss => with_param(&location, "iss", None),
                };
                let (_, _, body) = http_get(&target);
                shown.lock().unwrap().push(body);
            });
            Ok(())
        });
        (opener, opened, pages)
    }

    /// The harness with the fake's session URL in the config and a real
    /// HTTP transport to it.
    fn oauth_harness(http: &mejl_fake::HttpHandle, extra: &str) -> Harness {
        let mut h = Harness::new();
        h.write_config(&format!(
            "[accounts.demo]\nsession_url = \"{}\"\n{extra}",
            http.session_url()
        ));
        h.transport = Some(Arc::new(HttpTransport::new().unwrap()));
        h
    }

    #[test]
    fn oauth_login_end_to_end_stores_the_login() {
        let (_rt, http, server) = serve();
        let mut h = oauth_harness(&http, "");
        h.store
            .set("demo", &Secret::new("fmu1-old-api-token-0001"))
            .unwrap();
        let (opener, opened, pages) = browser(Visit::Normal);
        h.opener = opener;
        let (result, out, err) = h.run(&["login", "--oauth"]);
        result.unwrap();
        assert!(out.contains("Signed in as mira@example.org"), "{out}");
        assert!(out.contains("Masked Email: yes"), "{out}");
        assert!(
            out.contains("Granted scope: urn:ietf:params:oauth:scope:mail"),
            "{out}"
        );
        assert!(
            out.contains("Login saved in the memory store for account \"demo\""),
            "{out}"
        );
        assert!(
            out.contains("API token stored for this account was removed"),
            "{out}"
        );
        let url = opened.lock().unwrap()[0].clone();
        assert!(err.contains(&url), "{err}");
        assert!(err.contains("mejl login --port 8765"), "{err}");
        assert!(err.contains("ssh -L 8765:127.0.0.1:8765 <host>"), "{err}");
        assert!(err.contains("Press Ctrl-C to cancel"), "{err}");
        assert!(pages_seen(&pages, 1)[0].contains("You can close this tab"));

        let record = h.store.get_oauth("demo").unwrap().unwrap();
        assert_eq!(record.client_source, crate::oauth::ClientSource::Registered);
        assert_eq!(record.issuer, http.base_url());
        assert!(record.token_endpoint.ends_with("/oauth/refresh"));
        assert!(
            record
                .tokens
                .has_scope(mejl_jmap::oauth::scope::MASKED_EMAIL)
        );
        assert!(
            h.store.get("demo").unwrap().is_none(),
            "one login per account"
        );
        assert_eq!(server.oauth_registrations(), 1);
        let grants = server.oauth_grants();
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].client_id, record.client_id);
        for secret in [
            record.tokens.access.expose(),
            record.tokens.refresh.expose(),
        ] {
            assert!(!out.contains(secret) && !err.contains(secret));
        }

        // A second login reuses the stored client id.
        let (opener, _, _) = browser(Visit::Normal);
        h.opener = opener;
        let (result, _, err) = h.run(&["login", "--oauth"]);
        result.unwrap();
        assert_eq!(server.oauth_registrations(), 1, "{err}");
        assert_eq!(
            h.store.get_oauth("demo").unwrap().unwrap().client_id,
            record.client_id
        );
    }

    #[test]
    fn oauth_login_ignores_a_wrong_state_and_keeps_waiting() {
        let (_rt, http, _server) = serve();
        let mut h = oauth_harness(&http, "");
        let (opener, opened, pages) = browser(Visit::WrongStateFirst);
        h.opener = opener;
        // --port, as for SSH forwarding, with a port that was just free.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let (result, _, err) = h.run(&["login", "--port", &port.to_string()]);
        result.unwrap();
        assert!(
            err.contains(&format!("ssh -L {port}:127.0.0.1:{port} <host>")),
            "{err}"
        );
        let redirect = format!("127.0.0.1%3A{port}");
        assert!(opened.lock().unwrap()[0].contains(&redirect));
        let pages = pages_seen(&pages, 2);
        assert!(pages[0].contains("not part of the mejl login"), "{pages:?}");
        assert!(pages[1].contains("You can close this tab"), "{pages:?}");
        assert!(h.store.get_oauth("demo").unwrap().is_some());
    }

    #[test]
    fn oauth_login_times_out_and_stores_nothing() {
        let (_rt, http, _server) = serve();
        let mut h = oauth_harness(&http, "");
        h.opener = Box::new(|_| Err("no browser here".into()));
        h.oauth_timeout = Duration::from_millis(300);
        let (result, _, err) = h.run(&["login", "--oauth"]);
        let e = result.unwrap_err();
        assert_eq!(e.code, exit::FAILURE);
        assert!(
            e.message.contains("no answer from the browser"),
            "{}",
            e.message
        );
        assert!(
            err.contains("Could not open a browser (no browser here)"),
            "{err}"
        );
        assert!(h.store.get_oauth("demo").unwrap().is_none());
    }

    #[test]
    fn oauth_login_refuses_a_wrong_or_missing_issuer() {
        for (visit, want) in [
            (Visit::WrongIss, "evil.example.org"),
            (Visit::NoIss, "did not say which server"),
        ] {
            let (_rt, http, server) = serve();
            let mut h = oauth_harness(&http, "");
            let (opener, _, pages) = browser(visit);
            h.opener = opener;
            let (result, _, _) = h.run(&["login", "--oauth"]);
            let e = result.unwrap_err();
            assert_eq!(e.code, exit::AUTH);
            assert!(e.message.contains(want), "{}", e.message);
            assert!(pages_seen(&pages, 1)[0].contains("could not log in"));
            assert!(h.store.get_oauth("demo").unwrap().is_none());
            assert!(
                server.oauth_grants().is_empty(),
                "the code was never exchanged"
            );
        }
    }

    #[test]
    fn a_refused_masked_scope_leads_to_a_second_round_without_it() {
        let (_rt, http, server) = serve();
        server.quirks_mut().refuse_masked_scope = true;
        let mut h = oauth_harness(&http, "");
        let (opener, opened, pages) = browser(Visit::Normal);
        h.opener = opener;
        let (result, out, err) = h.run(&["login", "--oauth"]);
        result.unwrap();
        assert!(
            err.contains("Fastmail did not grant the Masked Email scope"),
            "{err}"
        );
        assert!(err.contains("Asking again without it"), "{err}");
        let opened = opened.lock().unwrap();
        assert_eq!(opened.len(), 2);
        assert!(opened[0].contains("maskedemail"));
        assert!(!opened[1].contains("maskedemail"));
        assert!(!opened[1].contains("jmap%3Acore"), "{}", opened[1]);
        // Two browser threads, so either page may come first.
        let pages = pages_seen(&pages, 2);
        assert!(
            pages.iter().any(|p| p.contains("asks again with fewer")),
            "{pages:?}"
        );
        assert!(
            pages.iter().any(|p| p.contains("You can close this tab")),
            "{pages:?}"
        );
        assert!(out.contains("Masked Email: not granted"), "{out}");
        assert!(out.contains("log in with an API token instead"), "{out}");
        let record = h.store.get_oauth("demo").unwrap().unwrap();
        assert!(
            !record
                .tokens
                .has_scope(mejl_jmap::oauth::scope::MASKED_EMAIL)
        );
        let grants = server.oauth_grants();
        assert_eq!(grants.len(), 1);
        assert!(
            !grants[0]
                .scope
                .iter()
                .any(|s| s.contains("maskedemail") || s.contains("jmap:core"))
        );
    }

    #[test]
    fn a_configured_client_id_skips_registration_and_uses_the_jmap_scopes() {
        let (rt, http, server) = serve();
        let transport = HttpTransport::new().unwrap();
        let id = rt
            .block_on(mejl_jmap::oauth::register(
                &transport,
                &format!("{}/oauth/register", http.base_url()),
            ))
            .unwrap();
        assert_eq!(server.oauth_registrations(), 1);
        let mut h = oauth_harness(&http, &format!("oauth_client_id = \"{id}\"\n"));
        let (opener, opened, _) = browser(Visit::Normal);
        h.opener = opener;
        let (result, out, _) = h.run(&["login", "--oauth"]);
        result.unwrap();
        assert_eq!(server.oauth_registrations(), 1, "no second registration");
        let record = h.store.get_oauth("demo").unwrap().unwrap();
        assert_eq!(record.client_id, id);
        assert_eq!(record.client_source, crate::oauth::ClientSource::Config);
        let grants = server.oauth_grants();
        for scope in [
            "urn:ietf:params:jmap:core",
            "urn:ietf:params:jmap:mail",
            "urn:ietf:params:jmap:submission",
            "urn:ietf:params:jmap:contacts",
            "https://www.fastmail.com/dev/maskedemail",
        ] {
            assert!(grants[0].scope.iter().any(|s| s == scope), "{scope}");
        }
        assert!(!opened.lock().unwrap()[0].contains("oauth%3Ascope"));
        assert!(out.contains("Masked Email: yes"), "{out}");
    }

    fn stale_record(http: &mejl_fake::HttpHandle) -> crate::oauth::OAuthRecord {
        crate::oauth::OAuthRecord {
            client_id: "mejl-fake-client-forgotten".into(),
            client_source: crate::oauth::ClientSource::Registered,
            issuer: http.base_url(),
            token_endpoint: format!("{}/oauth/refresh", http.base_url()),
            revocation_endpoint: Some(format!("{}/oauth/revoke", http.base_url())),
            tokens: mejl_jmap::TokenSet {
                access: Secret::new("stale-access-token-0001"),
                refresh: Secret::new("stale-refresh-token-0001"),
                expires_at: jiff::Timestamp::UNIX_EPOCH,
                scope: Vec::new(),
            },
        }
    }

    #[test]
    fn a_stale_stored_client_id_is_registered_again() {
        let (_rt, http, server) = serve();
        let mut h = oauth_harness(&http, "");
        h.store.set_oauth("demo", &stale_record(&http)).unwrap();
        let (opener, _, _) = browser(Visit::Normal);
        h.opener = opener;
        let (result, _, err) = h.run(&["login", "--oauth"]);
        result.unwrap();
        assert!(err.contains("registering mejl again"), "{err}");
        assert_eq!(server.oauth_registrations(), 1);
        let record = h.store.get_oauth("demo").unwrap().unwrap();
        assert_ne!(record.client_id, "mejl-fake-client-forgotten");
    }

    #[test]
    fn login_is_refused_while_mejl_runs() {
        let (_rt, http, server) = serve();
        let mut h = oauth_harness(&http, "");
        let dir = h.paths.ensure_account_dir("demo").unwrap();
        let _held = InstanceLock::acquire(&dir).unwrap();
        // The default opener panics, so reaching the browser would fail.
        let (result, _, _) = h.run(&["login", "--oauth"]);
        assert_eq!(result.unwrap_err().code, exit::LOCKED);
        h.stdin = io::Cursor::new(format!("{}\n", server.token()).into_bytes());
        let (result, _, _) = h.run(&["login", "--token"]);
        assert_eq!(result.unwrap_err().code, exit::LOCKED);
        assert!(h.store.get("demo").unwrap().is_none());
        assert_eq!(server.oauth_registrations(), 0);
    }

    #[test]
    fn login_on_a_terminal_asks_which_method() {
        let (_rt, http, _server) = serve();
        let mut h = oauth_harness(&http, "");
        h.stdin_is_tty = true;
        h.stdin = io::Cursor::new(b"3\n1\n".to_vec());
        let (opener, _, _) = browser(Visit::Normal);
        h.opener = opener;
        let (result, _, err) = h.run(&["login"]);
        result.unwrap();
        assert!(err.contains("1  Log in with your browser (OAuth)"), "{err}");
        assert!(err.contains("2  Paste an API token"), "{err}");
        assert!(err.contains("Please answer 1 or 2."), "{err}");
        assert!(h.store.get_oauth("demo").unwrap().is_some());
        // End of input cancels.
        h.stdin = io::Cursor::new(Vec::new());
        let (result, _, _) = h.run(&["login"]);
        assert!(result.unwrap_err().message.contains("cancelled"));
    }

    /// An OAuth login made through the CLI, for the tests after it.
    fn logged_in(http: &mejl_fake::HttpHandle) -> Harness {
        let mut h = oauth_harness(http, "");
        let (opener, _, _) = browser(Visit::Normal);
        h.opener = opener;
        let (result, _, err) = h.run(&["login", "--oauth"]);
        result.expect(&err);
        h
    }

    #[test]
    fn logout_revokes_the_oauth_login() {
        let (_rt, http, server) = serve();
        let mut h = logged_in(&http);
        assert_eq!(server.oauth_grants().len(), 1);
        let (result, out, err) = h.run(&["logout"]);
        result.unwrap();
        assert!(
            out.contains("Revoked the OAuth login at the server."),
            "{out}{err}"
        );
        assert!(
            out.contains("Removed the OAuth login for \"demo\""),
            "{out}"
        );
        assert!(server.oauth_grants().is_empty(), "the grant is gone");
        assert!(h.store.get_oauth("demo").unwrap().is_none());
        assert_eq!(server.oauth_refreshes(), 0, "logout never refreshes");
        let (result, out, _) = h.run(&["logout"]);
        result.unwrap();
        assert!(out.contains("No stored login for \"demo\""), "{out}");
    }

    #[test]
    fn logout_says_when_revoking_fails_and_removes_the_login_anyway() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let mut h = Harness::new();
        h.transport = Some(Arc::new(HttpTransport::new().unwrap()));
        let mut record = crate::oauth::OAuthRecord {
            client_id: "c".into(),
            client_source: crate::oauth::ClientSource::Registered,
            issuer: format!("http://127.0.0.1:{port}"),
            token_endpoint: format!("http://127.0.0.1:{port}/oauth/refresh"),
            revocation_endpoint: Some(format!("http://127.0.0.1:{port}/oauth/revoke")),
            tokens: mejl_jmap::TokenSet {
                access: Secret::new("closed-port-access-0001"),
                refresh: Secret::new("closed-port-refresh-0001"),
                expires_at: jiff::Timestamp::UNIX_EPOCH,
                scope: Vec::new(),
            },
        };
        h.store.set_oauth("default", &record).unwrap();
        let (result, out, err) = h.run(&["logout"]);
        result.unwrap();
        assert!(err.contains("Could not revoke the OAuth login"), "{err}");
        assert!(out.contains("Removed the OAuth login"), "{out}");
        assert!(h.store.get_oauth("default").unwrap().is_none());
        // Without a revocation endpoint it says so too.
        record.revocation_endpoint = None;
        h.store.set_oauth("default", &record).unwrap();
        let (result, _, err) = h.run(&["logout"]);
        result.unwrap();
        assert!(err.contains("no way to revoke"), "{err}");
    }

    #[test]
    fn doctor_shows_the_oauth_login_and_never_a_token() {
        let (_rt, http, server) = serve();
        let mut h = logged_in(&http);
        let refreshes = server.oauth_refresh_requests();
        let (result, out, _) = h.run(&["doctor", "--no-probe"]);
        result.unwrap();
        assert!(out.contains("OAuth, found in the memory store"), "{out}");
        assert!(out.contains("registered by `mejl login`"), "{out}");
        assert!(out.contains("urn:ietf:params:oauth:scope:mail"), "{out}");
        assert!(out.contains("refreshes it before then"), "{out}");
        let record = h.store.get_oauth("demo").unwrap().unwrap();
        assert!(!out.contains(record.tokens.access.expose()), "{out}");
        assert!(!out.contains(record.tokens.refresh.expose()), "{out}");
        assert!(!out.contains("mejl-fake-at-"), "{out}");
        assert!(!out.contains("mejl-fake-rt-"), "{out}");
        assert_eq!(
            server.oauth_refresh_requests(),
            refreshes,
            "doctor never refreshes"
        );
    }

    #[test]
    fn a_token_login_replaces_and_revokes_the_oauth_login() {
        let (_rt, http, server) = serve();
        let mut h = logged_in(&http);
        h.stdin = io::Cursor::new(format!("{}\n", server.token()).into_bytes());
        let (result, out, _) = h.run(&["login", "--token"]);
        result.unwrap();
        assert!(out.contains("Token saved"), "{out}");
        assert!(
            out.contains("OAuth login stored for this account was revoked"),
            "{out}"
        );
        assert!(h.store.get_oauth("demo").unwrap().is_none());
        assert_eq!(
            h.store.get("demo").unwrap().unwrap().expose(),
            server.token()
        );
        assert!(server.oauth_grants().is_empty());
    }

    #[test]
    fn run_with_an_oauth_login_checks_setup_without_contacting_anything() {
        let (_rt, http, server) = serve();
        let mut h = logged_in(&http);
        let refreshes = server.oauth_refresh_requests();
        let (result, _, _) = h.run(&["--account", "demo"]);
        assert_eq!(result.unwrap_err().message, NEEDS_TERMINAL);
        assert_eq!(server.oauth_refresh_requests(), refreshes);
    }

    #[test]
    fn unknown_account_with_configured_accounts_fails() {
        let mut h = Harness::new();
        h.write_config("[accounts.home]\n");
        let (result, _, _) = h.run(&["--account", "hmoe"]);
        let e = result.unwrap_err();
        assert_eq!(e.code, exit::CONFIG);
        assert!(e.message.contains("known accounts: home"));
    }
}
