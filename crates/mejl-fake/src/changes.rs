//! Per-type state strings and change logs, for `/changes` (RFC 8620 5.2).
//!
//! Every single object change advances the state by one. That way any state
//! a `/changes` page hands out as `newState` is a real point in history, and
//! paging with `maxChanges` can stop between any two changes.

use std::collections::{BTreeMap, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChangeKind {
    Created,
    Updated,
    Destroyed,
}

#[derive(Debug, Clone)]
struct Entry {
    seq: u64,
    id: String,
    kind: ChangeKind,
    /// For mailboxes: only the count properties changed.
    counts_only: bool,
    /// For emails: the thread the email belonged to. Used to widen
    /// `Email/queryChanges` when threads matter.
    thread_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ChangeLog {
    current: u64,
    /// The oldest state `/changes` can still answer from.
    oldest: u64,
    entries: VecDeque<Entry>,
    limit: usize,
}

/// One page of `/changes` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangesPage {
    pub old_state: String,
    pub new_state: String,
    pub has_more_changes: bool,
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub destroyed: Vec<String>,
    /// True when every id in `updated` changed only its counts.
    pub updated_counts_only: bool,
}

/// What changed for one id within a range of history.
#[derive(Debug, Clone, Copy)]
struct Seen {
    first: ChangeKind,
    last: ChangeKind,
    counts_only: bool,
}

impl ChangeLog {
    pub(crate) fn new(limit: usize) -> Self {
        ChangeLog {
            current: 0,
            oldest: 0,
            entries: VecDeque::new(),
            limit: limit.max(1),
        }
    }

    pub(crate) fn state(&self) -> String {
        self.current.to_string()
    }

    pub(crate) fn record(&mut self, id: &str, kind: ChangeKind) {
        self.push(id, kind, false, None);
    }

    pub(crate) fn record_counts(&mut self, id: &str) {
        self.push(id, ChangeKind::Updated, true, None);
    }

    pub(crate) fn record_email(&mut self, id: &str, kind: ChangeKind, thread_id: &str) {
        self.push(id, kind, false, Some(thread_id.to_owned()));
    }

    fn push(&mut self, id: &str, kind: ChangeKind, counts_only: bool, thread_id: Option<String>) {
        self.current += 1;
        self.entries.push_back(Entry {
            seq: self.current,
            id: id.to_owned(),
            kind,
            counts_only,
            thread_id,
        });
        while self.entries.len() > self.limit {
            if let Some(dropped) = self.entries.pop_front() {
                self.oldest = dropped.seq;
            }
        }
    }

    /// Forgets all history. The current state stays, so existing clients get
    /// `cannotCalculateChanges` for any older state.
    pub(crate) fn expire(&mut self) {
        self.entries.clear();
        self.oldest = self.current;
    }

    /// Parses a state string this log can answer from.
    pub(crate) fn parse_since(&self, since: &str) -> Option<u64> {
        // Reject "+1", "01" and the like, which `parse` would accept or
        // which never came from this server.
        if since.is_empty() || !since.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if since.len() > 1 && since.starts_with('0') {
            return None;
        }
        let n: u64 = since.parse().ok()?;
        (self.oldest..=self.current).contains(&n).then_some(n)
    }

    /// A page of changes since `since`. `None` means the state is unknown or
    /// too old, which callers report as `cannotCalculateChanges`.
    pub(crate) fn changes(&self, since: &str, max_changes: Option<usize>) -> Option<ChangesPage> {
        let since_n = self.parse_since(since)?;
        let mut order: Vec<String> = Vec::new();
        let mut seen: BTreeMap<String, Seen> = BTreeMap::new();
        let mut last_seq = since_n;
        let mut has_more = false;
        for entry in self.entries.iter().filter(|e| e.seq > since_n) {
            match seen.get_mut(&entry.id) {
                Some(s) => {
                    s.last = entry.kind;
                    s.counts_only &= entry.counts_only;
                }
                None => {
                    if max_changes.is_some_and(|max| order.len() >= max) {
                        has_more = true;
                        break;
                    }
                    order.push(entry.id.clone());
                    seen.insert(
                        entry.id.clone(),
                        Seen {
                            first: entry.kind,
                            last: entry.kind,
                            counts_only: entry.counts_only,
                        },
                    );
                }
            }
            last_seq = entry.seq;
        }
        let mut page = ChangesPage {
            old_state: since_n.to_string(),
            new_state: last_seq.to_string(),
            has_more_changes: has_more,
            created: Vec::new(),
            updated: Vec::new(),
            destroyed: Vec::new(),
            updated_counts_only: true,
        };
        for id in order {
            let Some(s) = seen.get(&id) else { continue };
            match (s.first, s.last) {
                // Created and destroyed within the range: the client never
                // needs to hear about it.
                (ChangeKind::Created, ChangeKind::Destroyed) => {}
                (ChangeKind::Created, _) => page.created.push(id),
                (_, ChangeKind::Destroyed) => page.destroyed.push(id),
                _ => {
                    page.updated_counts_only &= s.counts_only;
                    page.updated.push(id);
                }
            }
        }
        if page.updated.is_empty() {
            page.updated_counts_only = false;
        }
        Some(page)
    }

    /// Ids changed since `since`, with the threads they were in, and the
    /// ids that were created after `since`. `None` for an unknown state.
    pub(crate) fn touched_since(&self, since: u64) -> Option<Touched> {
        if since < self.oldest || since > self.current {
            return None;
        }
        let mut touched = Touched::default();
        for entry in self.entries.iter().filter(|e| e.seq > since) {
            if entry.kind == ChangeKind::Created {
                touched.created.insert(entry.id.clone());
            }
            touched.ids.insert(entry.id.clone());
            if let Some(t) = &entry.thread_id {
                touched.threads.insert(t.clone());
            }
        }
        Some(touched)
    }
}

#[derive(Debug, Default)]
pub(crate) struct Touched {
    pub ids: std::collections::BTreeSet<String>,
    pub created: std::collections::BTreeSet<String>,
    pub threads: std::collections::BTreeSet<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log_with(changes: &[(&str, ChangeKind)]) -> ChangeLog {
        let mut log = ChangeLog::new(100);
        for (id, kind) in changes {
            log.record(id, *kind);
        }
        log
    }

    #[test]
    fn collapses_per_id() {
        use ChangeKind::*;
        let log = log_with(&[
            ("a", Created),
            ("a", Updated),
            ("b", Updated),
            ("c", Created),
            ("c", Destroyed),
            ("d", Updated),
            ("d", Destroyed),
        ]);
        let p = log.changes("0", None).unwrap();
        assert_eq!(p.created, vec!["a"]);
        assert_eq!(p.updated, vec!["b"]);
        assert_eq!(p.destroyed, vec!["d"]);
        assert_eq!(p.new_state, "7");
        assert!(!p.has_more_changes);
        assert!(!p.updated_counts_only);
    }

    #[test]
    fn pages_with_max_changes_and_resumes() {
        use ChangeKind::*;
        let log = log_with(&[
            ("a", Created),
            ("b", Created),
            ("a", Updated),
            ("c", Created),
            ("d", Created),
        ]);
        let p1 = log.changes("0", Some(2)).unwrap();
        assert_eq!(p1.created, vec!["a", "b"]);
        assert!(p1.has_more_changes);
        assert_eq!(p1.new_state, "3");
        let p2 = log.changes(&p1.new_state, Some(2)).unwrap();
        assert_eq!(p2.created, vec!["c", "d"]);
        assert!(!p2.has_more_changes);
        let p3 = log.changes(&p2.new_state, Some(2)).unwrap();
        assert!(p3.created.is_empty() && p3.updated.is_empty() && p3.destroyed.is_empty());
        assert_eq!(p3.new_state, p3.old_state);
    }

    #[test]
    fn rejects_unknown_and_expired_states() {
        let mut log = ChangeLog::new(2);
        for id in ["a", "b", "c"] {
            log.record(id, ChangeKind::Created);
        }
        assert!(log.changes("0", None).is_none(), "expired");
        assert!(log.changes("1", None).is_some());
        assert!(log.changes("4", None).is_none(), "from the future");
        assert!(log.changes("x", None).is_none());
        assert!(log.changes("", None).is_none());
        assert!(log.changes("01", None).is_none());
        assert!(log.changes("+1", None).is_none());
        assert!(log.changes("-1", None).is_none());
        log.expire();
        assert!(log.changes("2", None).is_none());
        let p = log.changes("3", None).unwrap();
        assert!(p.created.is_empty());
    }

    #[test]
    fn counts_only_updates_are_flagged() {
        let mut log = ChangeLog::new(10);
        log.record_counts("m1");
        log.record_counts("m2");
        let p = log.changes("0", None).unwrap();
        assert!(p.updated_counts_only);
        log.record("m1", ChangeKind::Updated);
        let p = log.changes("0", None).unwrap();
        assert!(!p.updated_counts_only);
        let p = log.changes("2", None).unwrap();
        assert!(!p.updated_counts_only);
    }

    #[test]
    fn touched_reports_threads_and_creations() {
        let mut log = ChangeLog::new(10);
        log.record_email("e1", ChangeKind::Created, "t1");
        log.record_email("e2", ChangeKind::Updated, "t2");
        let t = log.touched_since(0).unwrap();
        assert_eq!(t.ids.len(), 2);
        assert!(t.created.contains("e1"));
        assert!(t.threads.contains("t2"));
        assert!(log.touched_since(5).is_none());
    }
}
