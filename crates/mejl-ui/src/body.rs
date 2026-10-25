//! Body rendering and its cache.
//!
//! Small plain text bodies are laid out on the UI thread, because that
//! costs microseconds. Everything else (HTML, and plain text over
//! [`INLINE_PLAIN_LIMIT`]) goes through [`Cmd::RenderBody`]: the shell runs
//! [`render_body`](crate::render_body) on a blocking worker and answers
//! with `Msg::BodyRendered`. Until then the reader shows the previous
//! rendering at the old width, or a placeholder line.
//!
//! Renderings are cached per message with the width they were made for,
//! and the cache is bounded.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use mejl_render::{BodySource, RenderOptions, RenderedBody};

use crate::data::{AttachmentView, EmailId, MailData};
use crate::event::{Cmd, RenderRequest};

/// Plain text bodies up to this size render on the UI thread.
pub const INLINE_PLAIN_LIMIT: usize = 16 * 1024;
/// Messages whose renderings are kept.
const CACHE_CAP: usize = 64;

/// Render options the UI uses.
pub(crate) fn render_options(width: usize, prefer_html: bool) -> RenderOptions {
    RenderOptions {
        width: width.max(1),
        prefer_html,
        ..RenderOptions::default()
    }
}

/// Parts of a body view the reader needs besides the text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BodyMeta {
    pub(crate) attachments: Vec<AttachmentView>,
    pub(crate) headers: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default)]
struct Entry {
    rendered: Option<Arc<RenderedBody>>,
    prefer_html: bool,
    meta: Option<Arc<BodyMeta>>,
    /// Width of a render request in flight.
    pending: Option<usize>,
}

/// What the reader can show for a message right now.
#[derive(Clone, Debug)]
pub(crate) enum BodyState {
    /// Not cached locally; a fetch was requested.
    Loading,
    /// The fetch failed.
    Failed(String),
    /// A rendering, possibly at an old width while a new one is made.
    Ready {
        body: Arc<RenderedBody>,
        meta: Arc<BodyMeta>,
        stale: bool,
    },
    /// Cached but not rendered yet.
    Rendering,
}

/// Renderings by message.
#[derive(Debug, Default)]
pub(crate) struct BodyCache {
    entries: HashMap<EmailId, Entry>,
    order: VecDeque<EmailId>,
    fetching: HashSet<EmailId>,
    failed: HashMap<EmailId, String>,
}

impl BodyCache {
    fn touch(&mut self, id: &EmailId) {
        if let Some(pos) = self.order.iter().position(|e| e == id) {
            self.order.remove(pos);
        }
        self.order.push_back(id.clone());
        while self.order.len() > CACHE_CAP {
            if let Some(old) = self.order.pop_front() {
                self.entries.remove(&old);
            }
        }
    }

    /// Makes sure a rendering at `width` exists or is on its way.
    pub(crate) fn ensure(
        &mut self,
        data: &dyn MailData,
        id: &EmailId,
        width: usize,
        prefer_html: bool,
        cmds: &mut Vec<Cmd>,
    ) -> BodyState {
        if let Some(err) = self.failed.get(id) {
            return BodyState::Failed(err.clone());
        }
        let entry = self.entries.get(id).cloned().unwrap_or_default();
        let fresh = entry
            .rendered
            .as_ref()
            .is_some_and(|r| r.width == width && entry.prefer_html == prefer_html);
        if let (true, Some(body), Some(meta)) = (fresh, &entry.rendered, &entry.meta) {
            return BodyState::Ready {
                body: body.clone(),
                meta: meta.clone(),
                stale: false,
            };
        }
        let stale_view = || match (&entry.rendered, &entry.meta) {
            (Some(body), Some(meta)) => BodyState::Ready {
                body: body.clone(),
                meta: meta.clone(),
                stale: true,
            },
            _ => BodyState::Rendering,
        };
        if entry.pending == Some(width) && entry.meta.is_some() {
            return stale_view();
        }
        if self.fetching.contains(id) {
            return BodyState::Loading;
        }
        let Some(view) = data.message_body(id) else {
            self.fetching.insert(id.clone());
            cmds.push(Cmd::FetchBody(id.clone()));
            return BodyState::Loading;
        };
        let meta = Arc::new(BodyMeta {
            attachments: view.attachments.clone(),
            headers: view.headers.clone(),
        });
        if let (true, Some(body)) = (fresh, &entry.rendered) {
            // Rendered by a prefetch before the reader asked: only the
            // attachment list and headers were missing.
            let body = body.clone();
            let mut entry = entry;
            entry.meta = Some(meta.clone());
            self.entries.insert(id.clone(), entry);
            self.touch(id);
            return BodyState::Ready {
                body,
                meta,
                stale: false,
            };
        }
        let source = if prefer_html && view.html.is_some() {
            Some(BodySource::Html)
        } else {
            mejl_render::choose_part(view.text.as_deref(), view.html.as_deref())
        };
        let inline = match source {
            Some(BodySource::Plain) => view
                .text
                .as_ref()
                .is_some_and(|t| t.len() <= INLINE_PLAIN_LIMIT),
            Some(BodySource::Html) => false,
            None => true,
        };
        let mut entry = entry;
        entry.meta = Some(meta.clone());
        let state = if inline {
            let body = Arc::new(crate::render_body(&view, width, prefer_html));
            entry.rendered = Some(body.clone());
            entry.prefer_html = prefer_html;
            entry.pending = None;
            BodyState::Ready {
                body,
                meta,
                stale: false,
            }
        } else {
            entry.pending = Some(width);
            cmds.push(Cmd::RenderBody(RenderRequest {
                email_id: id.clone(),
                width,
                prefer_html,
            }));
            match &entry.rendered {
                Some(body) => BodyState::Ready {
                    body: body.clone(),
                    meta,
                    stale: true,
                },
                None => BodyState::Rendering,
            }
        };
        self.entries.insert(id.clone(), entry);
        self.touch(id);
        state
    }

    /// Stores a rendering from the worker.
    pub(crate) fn rendered(&mut self, id: &EmailId, prefer_html: bool, body: RenderedBody) {
        let entry = self.entries.entry(id.clone()).or_default();
        if entry.pending == Some(body.width) {
            entry.pending = None;
        }
        entry.rendered = Some(Arc::new(body));
        entry.prefer_html = prefer_html;
        self.touch(id);
    }

    /// A fetch finished: the next `ensure` reads the body again.
    pub(crate) fn fetched(&mut self, id: &EmailId) {
        self.fetching.remove(id);
        self.failed.remove(id);
    }

    /// A fetch failed.
    pub(crate) fn fetch_failed(&mut self, id: &EmailId, error: String) {
        self.fetching.remove(id);
        self.failed.insert(id.clone(), error);
    }

    /// Links of the newest rendering of a message, for the links overlay.
    pub(crate) fn links(&self, id: &EmailId) -> Vec<mejl_render::Link> {
        self.entries
            .get(id)
            .and_then(|e| e.rendered.as_ref())
            .map(|r| r.links.clone())
            .unwrap_or_default()
    }

    /// Drops renderings, for example after the HTML preference changed.
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    /// Forgets failures, for example after the connection came back.
    pub(crate) fn clear_failures(&mut self) {
        self.failed.clear();
    }
}
