//! Starting and stopping the engine, and routing commands.

use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mejl_jmap::Client;
use mejl_store::{ReaderFactory, Store};
use tokio::sync::{Notify, mpsc};

use crate::bodies::{self, BodyMsg};
use crate::command::Command;
use crate::config::EngineConfig;
use crate::error::StartError;
use crate::event::{PushState, SyncEvent, SyncStatus};
use crate::shared::{Shared, SharedParts};
use crate::status::{self, Events, Sink, StatusCell};
use crate::store_thread::{self, StoreParts};
use crate::{drafts, flush, masked, mutate, push, search, send, sync, upload};

/// The sync engine. Start it once per account with [`Engine::start`].
#[derive(Debug)]
pub struct Engine;

impl Engine {
    /// Starts the engine and returns its handle.
    ///
    /// - `client` is a [`Client`] with the account's transport, session
    ///   URL and token. The engine fetches the session itself.
    /// - `store` moves into a dedicated thread named `mejl-store`, which
    ///   does every store write. Open readers for the UI thread with
    ///   [`SyncHandle::reader_factory`].
    /// - `sink` receives every [`SyncEvent`], from several threads. It
    ///   must return quickly; forward events into your own channel.
    ///
    /// The engine owns its tokio runtime, on a thread named `mejl-sync`;
    /// the caller needs none. Network work starts at once.
    pub fn start(
        config: EngineConfig,
        client: Client,
        store: Store,
        sink: impl Fn(SyncEvent) + Send + Sync + 'static,
    ) -> Result<SyncHandle, StartError> {
        let sink: Sink = Arc::new(sink);
        // The cached session lets the flusher and readers work before the
        // live one arrives, and offline.
        if client.session().is_none()
            && let Some(session) = store.session()?
        {
            client.set_session(session);
        }
        let readers = store.reader_factory();
        let initial = SyncStatus {
            push: if config.push {
                PushState::Connecting
            } else {
                PushState::Disabled
            },
            ..SyncStatus::default()
        };
        let status = Arc::new(StatusCell::new(sink.clone(), initial));
        let (events, data_rx) = Events::new(sink.clone());
        let flush_wake = Arc::new(Notify::new());
        let draft_wake = Arc::new(Notify::new());
        let (store_handle, store_thread, store_done) = store_thread::spawn(
            store,
            StoreParts {
                events: events.clone(),
                status: status.clone(),
                flush_wake: flush_wake.clone(),
                draft_wake: draft_wake.clone(),
                clock: config.clock.clone(),
            },
        )?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("mejl-sync-worker")
            .enable_all()
            .build();
        let runtime = match runtime {
            Ok(rt) => rt,
            Err(e) => {
                store_handle.stop();
                return Err(e.into());
            }
        };
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (body_tx, body_rx) = mpsc::unbounded_channel();
        let timeout = config.shutdown_timeout;
        let shared = Arc::new(Shared::new(SharedParts {
            cfg: config,
            client,
            store: store_handle.clone(),
            events,
            status,
            flush_wake,
            draft_wake,
            bodies: body_tx,
        }));
        let (engine_done_tx, engine_done) = std_mpsc::channel();
        let store_for_driver = store_handle.clone();
        let driver = std::thread::Builder::new()
            .name("mejl-sync".into())
            .spawn(move || {
                runtime.block_on(main(shared, cmd_rx, data_rx, body_rx));
                // Tasks still running are dropped here. A store job already
                // sent still runs to the end of its transaction.
                runtime.shutdown_timeout(timeout);
                store_for_driver.stop();
                let _ = engine_done_tx.send(());
            });
        let driver = match driver {
            Ok(d) => d,
            Err(e) => {
                store_handle.stop();
                return Err(e.into());
            }
        };
        Ok(SyncHandle {
            tx: cmd_tx,
            readers,
            joins: Arc::new(Mutex::new(Some(Joins {
                driver,
                engine_done,
                store_thread,
                store_done,
            }))),
            timeout,
        })
    }
}

struct Joins {
    driver: JoinHandle<()>,
    engine_done: std_mpsc::Receiver<()>,
    store_thread: JoinHandle<()>,
    store_done: std_mpsc::Receiver<()>,
}

/// The engine's handle: send commands, open readers, shut down. Cheap to
/// clone and safe to use from any thread. [`SyncHandle::send`] never
/// blocks and never awaits, so the UI thread can call it every frame.
#[derive(Clone)]
pub struct SyncHandle {
    tx: mpsc::UnboundedSender<Command>,
    readers: ReaderFactory,
    joins: Arc<Mutex<Option<Joins>>>,
    timeout: Duration,
}

impl std::fmt::Debug for SyncHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncHandle")
            .field("running", &!self.tx.is_closed())
            .finish_non_exhaustive()
    }
}

impl SyncHandle {
    /// Queues a command. Returns false when the engine has stopped.
    pub fn send(&self, command: Command) -> bool {
        self.tx.send(command).is_ok()
    }

    /// Opens read connections for another thread, such as the UI thread.
    pub fn reader_factory(&self) -> ReaderFactory {
        self.readers.clone()
    }

    /// Stops the engine and waits up to the configured shutdown timeout
    /// for its threads. Network tasks stop at once and no new outbox work
    /// starts; the store thread finishes the jobs already queued, so
    /// queued mutations are not lost. Returns true when both threads ended
    /// in time. Later calls return true at once.
    ///
    /// This blocks for at most the timeout. Call it when quitting.
    pub fn shutdown(&self) -> bool {
        let _ = self.tx.send(Command::Shutdown);
        let joins = self
            .joins
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(joins) = joins else {
            return true;
        };
        let deadline = Instant::now() + self.timeout + self.timeout;
        let left = || deadline.saturating_duration_since(Instant::now());
        let engine_ok = joins.engine_done.recv_timeout(left()).is_ok();
        let store_ok = engine_ok && joins.store_done.recv_timeout(left()).is_ok();
        if engine_ok {
            let _ = joins.driver.join();
        }
        if store_ok {
            let _ = joins.store_thread.join();
        } else {
            tracing::warn!("the sync engine did not stop in time");
        }
        engine_ok && store_ok
    }
}

async fn main(
    sh: Arc<Shared>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    data_rx: mpsc::UnboundedReceiver<crate::event::DataChanges>,
    body_rx: mpsc::UnboundedReceiver<BodyMsg>,
) {
    tokio::spawn(status::coalesce(
        data_rx,
        sh.events.sink().clone(),
        sh.cfg.data_coalesce,
        sh.cancel.clone(),
    ));
    tokio::spawn(sync::sync_loop(sh.clone()));
    if sh.cfg.push {
        tokio::spawn(push::push_loop(sh.clone()));
    }
    tokio::spawn(flush::flush_loop(sh.clone()));
    tokio::spawn(drafts::draft_loop(sh.clone()));
    tokio::spawn(bodies::body_loop(sh.clone(), body_rx));
    tokio::spawn(masked::masked_loop(sh.clone()));
    sh.store.post(|ctx| {
        ctx.refresh_outbox_status();
        drafts::refresh_pending(ctx);
    });
    sh.sync.fire();
    sh.masked.fire();
    loop {
        let command = tokio::select! {
            _ = sh.cancel.cancelled() => break,
            c = commands.recv() => c,
        };
        match command {
            None | Some(Command::Shutdown) => break,
            Some(c) => dispatch(&sh, c),
        }
    }
    tracing::debug!("sync engine stopping");
    sh.cancel.cancel();
}

fn dispatch(sh: &Arc<Shared>, command: Command) {
    match command {
        Command::RefreshNow => {
            sh.sync.fire();
            sh.masked.fire();
            sh.flush_wake.notify_one();
            sh.draft_wake.notify_one();
        }
        Command::SetToken(token) => {
            sh.client.set_token(token);
            sh.resume_auth();
        }
        Command::ResumeAuth => sh.resume_auth(),
        Command::Mutate {
            token,
            mutation,
            label,
            undoable,
        } => mutate::enqueue(sh, token, mutation, label, undoable),
        Command::Bulk {
            token,
            mailbox,
            except_threads,
            action,
            label,
        } => mutate::bulk(sh, token, mailbox, except_threads, action, label),
        Command::Undo { token } => mutate::undo_redo(sh, token, false),
        Command::Redo { token } => mutate::undo_redo(sh, token, true),
        Command::Send {
            token,
            message,
            undo_window,
        } => send::queue(sh, token, *message, undo_window),
        Command::CancelSend { token, seq } => send::cancel(sh, token, seq),
        Command::SendNow { seq } => send::send_now(sh, seq),
        Command::SaveDraftToServer { local_id, message } => drafts::queue(sh, local_id, *message),
        Command::UploadAttachment {
            token,
            path,
            content_type,
        } => {
            tokio::spawn(upload::upload(sh.clone(), token, path, content_type));
        }
        Command::FetchBody { email_id } => {
            let _ = sh.bodies.send(BodyMsg::Fetch(email_id));
        }
        Command::Prefetch { email_ids } => {
            let _ = sh.bodies.send(BodyMsg::Prefetch(email_ids));
        }
        Command::TouchBody { email_id } => {
            sh.store.post(move |ctx| {
                let now = ctx.now();
                if let Err(e) = ctx.store.touch_body(email_id.as_str(), now) {
                    tracing::debug!(error = %e, "cannot mark a body as opened");
                }
            });
        }
        Command::LoadMore { mailbox } => {
            tokio::spawn(sync::load_more(sh.clone(), mailbox));
        }
        Command::Search { token, request } => {
            tokio::spawn(search::run(sh.clone(), token, request));
        }
        Command::MaskedCreate {
            token,
            for_domain,
            description,
            email_prefix,
        } => masked::create(sh, token, for_domain, description, email_prefix),
        Command::MaskedSetState { token, id, state } => masked::set_state(sh, token, id, state),
        Command::MaskedSetDescription {
            token,
            id,
            description,
        } => masked::set_description(sh, token, id, description),
        Command::Local { token, write } => crate::local::write(sh, token, write),
        Command::Shutdown => sh.cancel.cancel(),
    }
}
