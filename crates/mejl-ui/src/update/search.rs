//! Search in `update`: the search bar, results, and results as a list.

use std::sync::Arc;

use crate::action::Action;
use crate::data::{ConversationRow, MailData};
use crate::event::{Cmd, ToastLevel};
use crate::input::{InputResult, LineInput};
use crate::keymap::{Context, Key, Pending, Resolution};
use crate::search::{
    LOCAL_LIMIT, ResultsList, SERVER_LIMIT, Saved, SearchState, ServerSearch, merged,
};
use crate::state::{AppState, Focus, ListState, Overlay, Prompt, PromptAction, Screen};

impl AppState {
    fn saved_view(&self) -> Saved {
        match &self.results {
            // Searching again from a results list keeps the mailbox from
            // before the first search.
            Some(r) => r.prev.clone(),
            None => Saved {
                mailbox: self.mailbox.clone(),
                list: self.list.clone(),
                focus: self.focus,
                reader_full: self.reader_full,
            },
        }
    }

    /// Opens the search bar, starting from `query`.
    pub(crate) fn open_search(&mut self, data: &dyn MailData, query: &str) {
        let mut s = SearchState::new(query, self.saved_view());
        if let Some(r) = &self.results
            && r.query == query
        {
            s.server = r.server.clone();
        }
        self.search = Some(Box::new(s));
        self.screen = Screen::Search;
        self.pending.clear();
        self.run_local(data);
    }

    fn run_local(&mut self, data: &dyn MailData) {
        let Some(s) = self.search.as_mut() else {
            return;
        };
        let q = s.input.text().trim().to_owned();
        s.local = if q.is_empty() {
            Default::default()
        } else {
            data.search_local(&q, LOCAL_LIMIT)
        };
        // Asked for an empty query too, so the screen keeps the same rows
        // whether or not anything is typed.
        s.jmap = data.search_filter_preview(&q);
        s.cursor = 0;
        s.scroll = 0;
        s.merge();
    }

    /// Keys on the search screen. Returns false to let the keymap have
    /// the key.
    pub(crate) fn search_key(
        &mut self,
        data: &dyn MailData,
        key: Key,
        cmds: &mut Vec<Cmd>,
    ) -> bool {
        if !self.pending.is_empty() {
            return false;
        }
        let mut scratch = Pending::default();
        let res = self
            .config
            .keymap
            .feed(&mut scratch, &[Context::Search], key);
        if matches!(res, Resolution::Action { .. } | Resolution::Pending) {
            return false;
        }
        let Some(s) = self.search.as_mut() else {
            return false;
        };
        if s.input.handle(&key) == InputResult::Changed {
            s.results_focused = false;
            self.run_local(data);
        }
        let _ = cmds;
        true
    }

    /// Pasted text into the search bar.
    pub(crate) fn search_paste(&mut self, data: &dyn MailData, text: &str) {
        if let Some(s) = self.search.as_mut() {
            s.input.insert_str(text);
            s.results_focused = false;
            self.run_local(data);
        }
    }

    /// Runs a search screen action. Returns false for other actions.
    pub(crate) fn search_action(
        &mut self,
        data: &dyn MailData,
        action: Action,
        cmds: &mut Vec<Cmd>,
    ) -> bool {
        if self.screen != Screen::Search {
            return false;
        }
        let Some(s) = self.search.as_mut() else {
            return false;
        };
        match action {
            Action::Next | Action::Prev => {
                if !s.rows.is_empty() {
                    let len = s.rows.len();
                    s.cursor = if action == Action::Next {
                        (s.cursor + 1).min(len - 1)
                    } else {
                        s.cursor.saturating_sub(1)
                    };
                    s.results_focused = true;
                }
            }
            Action::PageDown | Action::PageUp => {
                if !s.rows.is_empty() {
                    let len = s.rows.len();
                    s.cursor = if action == Action::PageDown {
                        (s.cursor + 10).min(len - 1)
                    } else {
                        s.cursor.saturating_sub(10)
                    };
                    s.results_focused = true;
                }
            }
            Action::NextField | Action::PrevField => {
                s.results_focused = !s.results_focused && !s.rows.is_empty();
            }
            Action::SearchServer => self.server_search(cmds),
            Action::Open => {
                let q = s.input.text().trim().to_owned();
                if q.is_empty() {
                    return true;
                }
                let asked = s.server.as_ref().is_some_and(|x| x.query() == q);
                if !s.results_focused && !asked {
                    self.server_search(cmds);
                    if let Some(s) = self.search.as_mut() {
                        s.results_focused = !s.rows.is_empty();
                    }
                } else if !s.rows.is_empty() {
                    self.open_results_list(data, cmds);
                }
            }
            Action::SaveSearch => {
                let q = s.input.text().trim().to_owned();
                if q.is_empty() {
                    self.notify("Type a search to save", ToastLevel::Info, false, cmds);
                } else {
                    self.overlays.push(Overlay::Prompt(Prompt {
                        label: "Save search as:".into(),
                        input: LineInput::new(q.clone()),
                        on_enter: PromptAction::SaveSearchName { query: q },
                    }));
                }
            }
            Action::Back => self.close_search(),
            _ => return false,
        }
        true
    }

    fn server_search(&mut self, cmds: &mut Vec<Cmd>) {
        let Some(s) = self.search.as_mut() else {
            return;
        };
        let q = s.input.text().trim().to_owned();
        if q.is_empty() {
            return;
        }
        cmds.push(Cmd::ServerSearch {
            query: q.clone(),
            limit: SERVER_LIMIT,
        });
        s.server = Some(ServerSearch::Searching { query: q });
    }

    /// Leaves search and puts the previous mailbox and cursor back.
    pub(crate) fn close_search(&mut self) {
        let Some(s) = self.search.take() else {
            return;
        };
        self.screen = Screen::Mail;
        if self.results.is_some() {
            // Back to the results list the search was opened from.
            return;
        }
        self.restore(s.prev);
    }

    fn restore(&mut self, prev: Saved) {
        self.results = None;
        self.mailbox = prev.mailbox;
        self.list = prev.list;
        self.list.pages.invalidate();
        self.list.relocate = true;
        self.focus = prev.focus;
        self.reader_full = prev.reader_full;
        self.reader.thread = None;
        self.reader.dirty = true;
    }

    /// Leaves a results list for the mailbox shown before the search.
    /// Returns false when no results list is shown.
    pub(crate) fn close_results(&mut self) -> bool {
        match self.results.take() {
            Some(r) => {
                self.restore(r.prev);
                true
            }
            None => false,
        }
    }

    /// Shows the search results as a list with the reader on the
    /// highlighted result.
    fn open_results_list(&mut self, data: &dyn MailData, cmds: &mut Vec<Cmd>) {
        let Some(s) = self.search.take() else {
            return;
        };
        let cursor = s.cursor;
        let query = s.input.text().trim().to_owned();
        self.show_results(query, s.rows, s.server, s.prev);
        self.list.cursor = cursor.min(self.list.total.saturating_sub(1));
        self.refresh(data, cmds);
        let layout = self.layout();
        self.open(data, &layout, cmds);
    }

    fn show_results(
        &mut self,
        query: String,
        rows: Vec<crate::search::ResultRow>,
        server: Option<ServerSearch>,
        prev: Saved,
    ) {
        let list: Vec<ConversationRow> = rows.iter().map(|r| r.row.clone()).collect();
        self.screen = Screen::Mail;
        self.list = ListState::default();
        self.list.total = list.len();
        self.list.pages.set_fixed(Arc::new(list));
        self.results = Some(Box::new(ResultsList {
            query,
            rows: Arc::new(rows),
            server,
            prev,
        }));
        self.focus = Focus::List;
        self.reader_full = false;
        self.reader.thread = None;
        self.reader.dirty = true;
    }

    /// Runs a saved search and shows its results as a list.
    pub(crate) fn open_results_for(
        &mut self,
        data: &dyn MailData,
        query: &str,
        cmds: &mut Vec<Cmd>,
    ) {
        let prev = self.saved_view();
        let local = data.search_local(query, LOCAL_LIMIT);
        let rows = merged(&local, None, query);
        cmds.push(Cmd::ServerSearch {
            query: query.to_owned(),
            limit: SERVER_LIMIT,
        });
        self.search = None;
        self.sidebar_overlay = false;
        self.show_results(
            query.to_owned(),
            rows,
            Some(ServerSearch::Searching {
                query: query.to_owned(),
            }),
            prev,
        );
    }

    /// Server results arrived.
    pub(crate) fn server_results(
        &mut self,
        query: String,
        result: Result<Vec<ConversationRow>, String>,
        cmds: &mut Vec<Cmd>,
    ) {
        let state = match result {
            Ok(rows) => ServerSearch::Done {
                query: query.clone(),
                rows,
            },
            Err(error) => {
                self.notify(
                    format!("Server search failed: {}", crate::text::sanitize(&error)),
                    ToastLevel::Error,
                    false,
                    cmds,
                );
                ServerSearch::Failed {
                    query: query.clone(),
                    error,
                }
            }
        };
        if let Some(s) = self.search.as_mut()
            && s.input.text().trim() == query
        {
            s.server = Some(state.clone());
            s.merge();
        }
        if let Some(r) = self.results.as_mut()
            && r.query == query
        {
            let mut rows: Vec<crate::search::ResultRow> = (*r.rows).clone();
            if let ServerSearch::Done { .. } = &state {
                let local = crate::data::SearchResults::default();
                for extra in merged(&local, Some(&state), &query) {
                    if !rows.iter().any(|x| x.row.thread_id == extra.row.thread_id) {
                        rows.push(extra);
                    }
                }
            }
            r.server = Some(state);
            let list: Vec<ConversationRow> = rows.iter().map(|x| x.row.clone()).collect();
            r.rows = Arc::new(rows);
            self.list.pages.set_fixed(Arc::new(list));
            self.list.relocate = true;
        }
    }

    /// Data changed while a results list is shown: read the local results
    /// again so flags and membership are fresh.
    pub(crate) fn refresh_results(&mut self, data: &dyn MailData) {
        let Some(r) = self.results.as_mut() else {
            return;
        };
        let local = data.search_local(&r.query, LOCAL_LIMIT);
        let rows = merged(&local, r.server.as_ref(), &r.query);
        let list: Vec<ConversationRow> = rows.iter().map(|x| x.row.clone()).collect();
        r.rows = Arc::new(rows);
        self.list.pages.set_fixed(Arc::new(list));
    }

    /// The mailbox the cursor row of a results list was found in.
    pub(crate) fn result_mailbox(&self) -> Option<crate::data::MailboxId> {
        let r = self.results.as_ref()?;
        let row = self.list.pages.row(self.list.cursor)?;
        r.rows
            .iter()
            .find(|x| x.row.thread_id == row.thread_id)
            .and_then(|x| x.mailbox.clone())
    }
}
