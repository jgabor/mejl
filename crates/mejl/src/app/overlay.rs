//! The short-lived optimistic overlay that makes an action show in the
//! same frame as its key press.
//!
//! The UI's contract is that a mutation is visible in the frame drawn right
//! after the key. mejl-store's own overlay is written by the store thread,
//! so it lands one store-thread round trip later. To close that gap the UI
//! thread keeps its own overlay, keyed by the [`mejl_sync::Command`] token:
//!
//! 1. Before the command is sent, the adapter reads the affected
//!    conversations (their cached messages with keywords and mailboxes, with
//!    any older pending entries already applied) and stores a before and
//!    after snapshot under the token.
//! 2. Every read through [`StoreData`](super::data::StoreData) applies the
//!    newest snapshot of each conversation over the rows the Reader
//!    returns: flags change, rows whose messages all left the mailbox are
//!    hidden (with the page offsets, totals and positions shifted to
//!    match), and mailbox counts move by the snapshot's deltas.
//! 3. `SyncEvent::Applied` (the store's overlay is now committed) or
//!    `SyncEvent::Rejected` (nothing was queued) for the token drops the
//!    entry. The reads that follow see the store's own overlay, or server
//!    truth again.
//!
//! Entries live for one store round trip, usually a millisecond or two, so
//! a snapshot never goes stale in practice. Masked Email state and
//! description changes and actions on a whole mailbox (`* a`) get simpler
//! entries of their own.
//!
//! Every read checks the Reader first, so an entry whose change the store
//! has already written does nothing twice: a hidden row is gone from the
//! Reader anyway, a row an entry adds is only added while the Reader lacks
//! it, and mailbox counts are the entries' flags minus the Reader's.
//!
//! # Undo and redo
//!
//! The store's undo stack is on the store thread, so the UI thread cannot
//! read what an undo will revert before the store has done it. A
//! [`Journal`] keeps the snapshots of this run's undoable actions instead.
//! On undo the newest one is played backwards over the conversations as
//! they look now and shown as an entry under the undo's token, which
//! `SyncEvent::Undone` drops. Redo plays it forwards again. The journal is
//! only trusted when its newest entry is the store's: either the store has
//! not taken it yet (it takes commands in order, so the undo will find it
//! on top), or the label on top of the store's stack is its label. When in
//! doubt nothing is shown early, and the change shows when the store
//! answers, one round trip later.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use mejl_store::{EmailFlags, Seq, UNDO_DEPTH};
use mejl_ui as ui;

/// One conversation before and after a pending change. Both lists hold
/// every cached message of the thread, in the same order.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadSnapshot {
    /// The thread.
    pub thread: String,
    /// Each message's id and flags before the change.
    pub before: Vec<(String, EmailFlags)>,
    /// Each message's id and flags after it. A destroyed message keeps its
    /// id with no mailboxes, so it leaves every list.
    pub after: Vec<(String, EmailFlags)>,
}

impl ThreadSnapshot {
    /// Whether any message is in `mailbox` after the change.
    pub fn in_mailbox_after(&self, mailbox: &str) -> bool {
        self.after.iter().any(|(_, f)| f.in_mailbox(mailbox))
    }

    /// This change undone over `now`, a thread's messages with their flags
    /// as the UI shows them: each keyword and mailbox the change set or
    /// cleared goes back, and everything else stays as it is now.
    pub fn reverted(&self, now: Vec<(String, EmailFlags)>) -> ThreadSnapshot {
        self.replay(now, &self.after, &self.before)
    }

    /// This change made again over `now`.
    pub fn reapplied(&self, now: Vec<(String, EmailFlags)>) -> ThreadSnapshot {
        self.replay(now, &self.before, &self.after)
    }

    fn replay(
        &self,
        now: Vec<(String, EmailFlags)>,
        from: &[(String, EmailFlags)],
        to: &[(String, EmailFlags)],
    ) -> ThreadSnapshot {
        let find = |list: &'_ [(String, EmailFlags)], id: &str| {
            list.iter().find(|(e, _)| e == id).map(|(_, f)| f.clone())
        };
        let mut after = now.clone();
        for (id, flags) in &mut after {
            let (Some(f), Some(t)) = (find(from, id), find(to, id)) else {
                continue;
            };
            for k in f.keywords.symmetric_difference(&t.keywords) {
                if t.keywords.contains(k) {
                    flags.keywords.insert(k.clone());
                } else {
                    flags.keywords.remove(k);
                }
            }
            for m in f.mailbox_ids.symmetric_difference(&t.mailbox_ids) {
                if t.mailbox_ids.contains(m) {
                    flags.mailbox_ids.insert(m.clone());
                } else {
                    flags.mailbox_ids.remove(m);
                }
            }
        }
        ThreadSnapshot {
            thread: self.thread.clone(),
            before: now,
            after,
        }
    }

    /// Unread and pinned state of the conversation in `mailbox` after the
    /// change, or `None` when it has left the mailbox.
    pub fn row_flags(&self, mailbox: &str) -> Option<(bool, bool)> {
        let mut any = false;
        let mut unread = false;
        let mut flagged = false;
        for (_, f) in self.after.iter().filter(|(_, f)| f.in_mailbox(mailbox)) {
            any = true;
            unread |= !f.has_keyword("$seen");
            flagged |= f.has_keyword("$flagged");
        }
        any.then_some((unread, flagged))
    }
}

/// An action on every conversation of a mailbox, from `* a`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkOverlay {
    /// The mailbox.
    pub mailbox: String,
    /// Conversations left out of the selection.
    pub except: BTreeSet<String>,
    /// The action moves mail out of the mailbox.
    pub hide: bool,
    /// Where a move takes the mail, for that mailbox's counts.
    pub target: Option<String>,
    /// Marks read (`Some(true)`) or unread.
    pub seen: Option<bool>,
    /// Pins or unpins.
    pub flagged: Option<bool>,
}

/// A pending Masked Email change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaskedOverlay {
    /// The mask.
    pub id: String,
    /// Its new state, if this changes it.
    pub state: Option<ui::MaskedState>,
    /// Its new description, if this changes it.
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum Entry {
    Threads(Vec<ThreadSnapshot>),
    Bulk(BulkOverlay),
    Masked(MaskedOverlay),
}

/// Mailbox count changes: total emails, unread emails, total threads,
/// unread threads.
pub type CountDelta = [i64; 4];

/// The pending entries, oldest token first.
#[derive(Debug, Default)]
pub struct Pending {
    entries: BTreeMap<u64, Entry>,
}

impl Pending {
    /// Records conversation snapshots for a token.
    pub fn add_threads(&mut self, token: u64, snapshots: Vec<ThreadSnapshot>) {
        if !snapshots.is_empty() {
            self.entries.insert(token, Entry::Threads(snapshots));
        }
    }

    /// Records a whole-mailbox action for a token.
    pub fn add_bulk(&mut self, token: u64, bulk: BulkOverlay) {
        self.entries.insert(token, Entry::Bulk(bulk));
    }

    /// Records a Masked Email change for a token.
    pub fn add_masked(&mut self, token: u64, masked: MaskedOverlay) {
        self.entries.insert(token, Entry::Masked(masked));
    }

    /// Drops a token's entry. Returns the threads it covered, so the caller
    /// can re-read them.
    pub fn remove(&mut self, token: u64) -> Option<Vec<String>> {
        Some(match self.entries.remove(&token)? {
            Entry::Threads(s) => s.into_iter().map(|t| t.thread).collect(),
            Entry::Bulk(_) | Entry::Masked(_) => Vec::new(),
        })
    }

    /// Whether nothing is pending.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn snapshots(&self) -> impl DoubleEndedIterator<Item = &ThreadSnapshot> {
        self.entries.values().flat_map(|e| match e {
            Entry::Threads(s) => s.as_slice(),
            _ => &[],
        })
    }

    /// Every thread with a pending snapshot.
    pub fn threads(&self) -> BTreeSet<&str> {
        self.snapshots().map(|s| s.thread.as_str()).collect()
    }

    /// The newest snapshot of a thread.
    pub fn thread(&self, thread: &str) -> Option<&ThreadSnapshot> {
        self.snapshots().rev().find(|s| s.thread == thread)
    }

    /// The newest pending flags of a message.
    pub fn flags(&self, email: &str) -> Option<&EmailFlags> {
        self.snapshots()
            .rev()
            .find_map(|s| s.after.iter().find(|(id, _)| id == email).map(|(_, f)| f))
    }

    /// Threads whose newest snapshot has no message left in `mailbox`. The
    /// caller hides their rows when the Reader still has them.
    pub fn left(&self, mailbox: &str) -> Vec<&str> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for s in self.snapshots().rev() {
            if seen.insert(s.thread.as_str()) && !s.in_mailbox_after(mailbox) {
                out.push(s.thread.as_str());
            }
        }
        out
    }

    /// The newest snapshot of each thread a pending change brings into
    /// `mailbox`: none of its messages were there before, some are after.
    /// The caller shows rows for them, such as a conversation an undo
    /// brings back.
    pub fn entered(&self, mailbox: &str) -> Vec<&ThreadSnapshot> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for s in self.snapshots().rev() {
            if seen.insert(s.thread.as_str())
                && s.in_mailbox_after(mailbox)
                && !s.before.iter().any(|(_, f)| f.in_mailbox(mailbox))
            {
                out.push(s);
            }
        }
        out
    }

    /// The newest whole-mailbox action on each mailbox.
    pub fn bulks(&self) -> Vec<&BulkOverlay> {
        let mut seen = BTreeSet::new();
        self.entries
            .values()
            .rev()
            .filter_map(|e| match e {
                Entry::Bulk(b) if seen.insert(b.mailbox.as_str()) => Some(b),
                _ => None,
            })
            .collect()
    }

    /// The newest whole-mailbox action on `mailbox`.
    pub fn bulk(&self, mailbox: &str) -> Option<&BulkOverlay> {
        self.entries.values().rev().find_map(|e| match e {
            Entry::Bulk(b) if b.mailbox == mailbox => Some(b),
            _ => None,
        })
    }

    /// How pending changes move each mailbox's counts, measured against
    /// what the Reader shows now. `reader` gives a thread's cached messages
    /// with the Reader's flags. Each thread counts once, as its newest
    /// pending flags minus the Reader's. The store writes a change before
    /// its `Applied` reaches this thread, so for a moment the Reader
    /// already shows what an entry here says; measuring against the Reader
    /// keeps that change from being counted twice.
    pub fn count_deltas(
        &self,
        reader: impl Fn(&str) -> Vec<(String, EmailFlags)>,
    ) -> HashMap<String, CountDelta> {
        let mut out: HashMap<String, CountDelta> = HashMap::new();
        for thread in self.threads() {
            let now = reader(thread);
            let pending: Vec<(String, EmailFlags)> = now
                .iter()
                .map(|(id, f)| (id.clone(), self.flags(id).unwrap_or(f).clone()))
                .collect();
            let mailboxes: BTreeSet<&String> = now
                .iter()
                .chain(pending.iter())
                .flat_map(|(_, f)| f.mailbox_ids.iter())
                .collect();
            for m in mailboxes {
                let tally = |list: &[(String, EmailFlags)]| {
                    let inside: Vec<&EmailFlags> = list
                        .iter()
                        .map(|(_, f)| f)
                        .filter(|f| f.in_mailbox(m))
                        .collect();
                    let unread = inside.iter().filter(|f| !f.has_keyword("$seen")).count();
                    (inside.len() as i64, unread as i64)
                };
                let (b_total, b_unread) = tally(&now);
                let (a_total, a_unread) = tally(&pending);
                if (a_total, a_unread) == (b_total, b_unread) {
                    continue;
                }
                let d = out.entry(m.clone()).or_default();
                d[0] += a_total - b_total;
                d[1] += a_unread - b_unread;
                d[2] += i64::from(a_total > 0) - i64::from(b_total > 0);
                d[3] += i64::from(a_unread > 0) - i64::from(b_unread > 0);
            }
        }
        out
    }

    /// The newest pending state and description of a mask.
    pub fn masked(&self, id: &str) -> (Option<ui::MaskedState>, Option<&str>) {
        let mut state = None;
        let mut description = None;
        for e in self.entries.values().rev() {
            if let Entry::Masked(m) = e
                && m.id == id
            {
                state = state.or(m.state);
                description = description.or(m.description.as_deref());
            }
        }
        (state, description)
    }
}

/// One undoable action of this run.
#[derive(Debug, Clone)]
struct Done {
    /// The command's token.
    token: u64,
    /// Its outbox entry, once the store has it.
    seq: Option<Seq>,
    /// The label the store keeps it under; empty when the store picks it.
    label: String,
    /// What it changed. Empty for actions with nothing to show early (a
    /// whole mailbox, a send, a mask, a new mailbox).
    snapshots: Vec<ThreadSnapshot>,
}

/// This run's undoable actions, mirroring the top of the store's undo and
/// redo stacks. See the module docs.
#[derive(Debug, Default)]
pub struct Journal {
    undo: Vec<Done>,
    redo: Vec<Done>,
}

impl Journal {
    /// An undoable command was sent. It clears the store's redo stack, so
    /// it clears this one.
    pub fn record(&mut self, token: u64, label: String, snapshots: Vec<ThreadSnapshot>) {
        self.redo.clear();
        self.undo.push(Done {
            token,
            seq: None,
            label,
            snapshots,
        });
    }

    /// Any other change to the outbox, which also clears the redo stack.
    pub fn clear_redo(&mut self) {
        self.redo.clear();
    }

    /// The store took the command for `token` as entry `seq`.
    pub fn applied(&mut self, token: u64, seq: Seq) {
        if let Some(d) = self.undo.iter_mut().find(|d| d.token == token) {
            d.seq = Some(seq);
        }
        // The store keeps this many; older ones can no longer be undone.
        while self.undo.iter().filter(|d| d.seq.is_some()).count() > UNDO_DEPTH {
            if let Some(i) = self.undo.iter().position(|d| d.seq.is_some()) {
                self.undo.remove(i);
            }
        }
    }

    /// The store refused the command for `token`.
    pub fn rejected(&mut self, token: u64) {
        self.undo.retain(|d| d.token != token);
    }

    /// An undo popped entry `original` off the store's stack. With
    /// `inverse`, the store queued that entry to revert it, which is what a
    /// redo replays.
    pub fn undone(&mut self, original: Seq, inverse: Option<Seq>) {
        let Some(i) = self.undo.iter().position(|d| d.seq == Some(original)) else {
            return;
        };
        let mut done = self.undo.remove(i);
        if let Some(seq) = inverse {
            done.seq = Some(seq);
            self.redo.push(done);
        }
    }

    /// The store's undo stack was empty.
    pub fn nothing_to_undo(&mut self) {
        self.undo.retain(|d| d.seq.is_none());
    }

    /// A redo replayed the undo entry `undone` as the new entry `seq`, which
    /// is back on the undo stack. With `seq` `None` the store dropped it.
    pub fn redone(&mut self, undone: Seq, seq: Option<Seq>) {
        let Some(i) = self.redo.iter().position(|d| d.seq == Some(undone)) else {
            return;
        };
        let mut done = self.redo.remove(i);
        if let Some(seq) = seq {
            done.seq = Some(seq);
            self.undo.push(done);
        }
    }

    /// The store's redo stack was empty.
    pub fn nothing_to_redo(&mut self) {
        self.redo.clear();
    }

    /// What the next undo shows at once: the newest action played
    /// backwards over `now` (a thread's messages with the flags the UI
    /// shows). Empty unless the newest action is surely the one the store
    /// will undo. `store_top` is the label on top of the store's stack.
    pub fn undo_preview(
        &self,
        store_top: Option<&str>,
        now: impl Fn(&str) -> Vec<(String, EmailFlags)>,
    ) -> Vec<ThreadSnapshot> {
        match Self::trusted(&self.undo, store_top) {
            Some(d) => d
                .snapshots
                .iter()
                .map(|s| s.reverted(now(&s.thread)))
                .collect(),
            None => Vec::new(),
        }
    }

    /// What the next redo shows at once, like [`Journal::undo_preview`].
    pub fn redo_preview(
        &self,
        store_top: Option<&str>,
        now: impl Fn(&str) -> Vec<(String, EmailFlags)>,
    ) -> Vec<ThreadSnapshot> {
        match Self::trusted(&self.redo, store_top) {
            Some(d) => d
                .snapshots
                .iter()
                .map(|s| s.reapplied(now(&s.thread)))
                .collect(),
            None => Vec::new(),
        }
    }

    fn trusted<'a>(stack: &'a [Done], store_top: Option<&str>) -> Option<&'a Done> {
        let top = stack.last()?;
        (top.seq.is_none() || store_top == Some(top.label.as_str())).then_some(top)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(mailboxes: &[&str], keywords: &[&str]) -> EmailFlags {
        EmailFlags {
            keywords: keywords.iter().map(|k| (*k).to_owned()).collect(),
            mailbox_ids: mailboxes.iter().map(|m| (*m).to_owned()).collect(),
        }
    }

    fn archive(thread: &str, email: &str) -> ThreadSnapshot {
        ThreadSnapshot {
            thread: thread.into(),
            before: vec![(email.into(), flags(&["inbox"], &[]))],
            after: vec![(email.into(), flags(&["archive"], &[]))],
        }
    }

    #[test]
    fn archived_threads_leave_the_mailbox_until_dropped() {
        let mut p = Pending::default();
        p.add_threads(1, vec![archive("t1", "e1")]);
        assert_eq!(p.left("inbox"), ["t1"]);
        assert!(p.left("archive").is_empty());
        assert_eq!(
            p.thread("t1").unwrap().row_flags("archive"),
            Some((true, false))
        );
        let d = p.count_deltas(|_| vec![("e1".into(), flags(&["inbox"], &[]))]);
        assert_eq!(d["inbox"], [-1, -1, -1, -1]);
        assert_eq!(d["archive"], [1, 1, 1, 1]);
        // The store has written it, but Applied is not here yet: the
        // Reader already shows it, so nothing moves twice.
        let d = p.count_deltas(|_| vec![("e1".into(), flags(&["archive"], &[]))]);
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(p.remove(1), Some(vec!["t1".to_owned()]));
        assert!(p.is_empty());
        assert!(p.left("inbox").is_empty());
        assert_eq!(p.remove(1), None);
    }

    #[test]
    fn the_newest_snapshot_wins_and_deltas_add_up() {
        let mut p = Pending::default();
        // Read, then pin, the same one-message thread.
        p.add_threads(
            1,
            vec![ThreadSnapshot {
                thread: "t".into(),
                before: vec![("e".into(), flags(&["inbox"], &[]))],
                after: vec![("e".into(), flags(&["inbox"], &["$seen"]))],
            }],
        );
        p.add_threads(
            2,
            vec![ThreadSnapshot {
                thread: "t".into(),
                before: vec![("e".into(), flags(&["inbox"], &["$seen"]))],
                after: vec![("e".into(), flags(&["inbox"], &["$seen", "$flagged"]))],
            }],
        );
        assert_eq!(
            p.thread("t").unwrap().row_flags("inbox"),
            Some((false, true))
        );
        assert!(p.flags("e").unwrap().has_keyword("$flagged"));
        let unread = |_: &str| vec![("e".into(), flags(&["inbox"], &[]))];
        assert_eq!(p.count_deltas(unread)["inbox"], [0, -1, 0, -1]);
        // The store applied the first one; pinning moves no count.
        p.remove(1);
        let read = |_: &str| vec![("e".into(), flags(&["inbox"], &["$seen"]))];
        assert!(p.count_deltas(read).is_empty());
        assert!(p.flags("e").unwrap().has_keyword("$seen"));
    }

    #[test]
    fn a_thread_moved_back_is_not_hidden() {
        let mut p = Pending::default();
        p.add_threads(1, vec![archive("t1", "e1")]);
        p.add_threads(
            2,
            vec![ThreadSnapshot {
                thread: "t1".into(),
                before: vec![("e1".into(), flags(&["archive"], &[]))],
                after: vec![("e1".into(), flags(&["inbox"], &[]))],
            }],
        );
        assert!(p.left("inbox").is_empty());
    }

    fn reader(list: &[(&str, &[&str], &[&str])]) -> Vec<(String, EmailFlags)> {
        list.iter()
            .map(|(id, m, k)| ((*id).to_owned(), flags(m, k)))
            .collect()
    }

    #[test]
    fn undo_plays_the_change_backwards_over_what_is_shown_now() {
        // Archived and marked read; since then someone pinned it.
        let s = ThreadSnapshot {
            thread: "t".into(),
            before: reader(&[("e", &["inbox"], &[])]),
            after: reader(&[("e", &["archive"], &["$seen"])]),
        };
        let now = reader(&[("e", &["archive"], &["$seen", "$flagged"])]);
        let back = s.reverted(now.clone());
        assert_eq!(back.before, now);
        assert_eq!(back.after, reader(&[("e", &["inbox"], &["$flagged"])]));
        assert_eq!(
            s.reapplied(back.after.clone()).after,
            reader(&[("e", &["archive"], &["$flagged", "$seen"])])
        );
        // A message that came later is left alone.
        let later = s.reverted(reader(&[
            ("e", &["archive"], &["$seen"]),
            ("f", &["inbox"], &[]),
        ]));
        assert_eq!(later.after[1], ("f".into(), flags(&["inbox"], &[])));
    }

    #[test]
    fn the_journal_is_used_only_when_it_matches_the_store() {
        let s = archive("t1", "e1");
        let now = |_: &str| reader(&[("e1", &["archive"], &[])]);
        let mut j = Journal::default();
        assert!(j.undo_preview(None, now).is_empty(), "nothing done yet");
        j.record(1, "Archive 1 conversation".into(), vec![s]);
        // Not in the store yet: the undo will find it on top.
        let preview = j.undo_preview(Some("Something older"), now);
        assert!(preview[0].in_mailbox_after("inbox"));
        j.applied(1, Seq(7));
        assert!(
            !j.undo_preview(Some("Archive 1 conversation"), now)
                .is_empty()
        );
        // Something else is on top of the store's stack.
        assert!(j.undo_preview(Some("Archive 3"), now).is_empty());
        assert!(j.undo_preview(None, now).is_empty());
        // A whole-mailbox action has nothing to show early, and covers the
        // older one until it is undone.
        j.record(2, String::new(), Vec::new());
        assert!(
            j.undo_preview(Some("Archive 1 conversation"), now)
                .is_empty()
        );
        j.applied(2, Seq(8));
        j.undone(Seq(8), Some(Seq(9)));
        assert!(
            !j.undo_preview(Some("Archive 1 conversation"), now)
                .is_empty()
        );
        // Undo and redo the archive.
        j.undone(Seq(7), Some(Seq(10)));
        assert!(
            j.undo_preview(Some("Archive 1 conversation"), now)
                .is_empty()
        );
        let back = |_: &str| reader(&[("e1", &["inbox"], &[])]);
        assert!(
            j.redo_preview(Some("Archive 1 conversation"), back)[0].in_mailbox_after("archive")
        );
        j.redone(Seq(10), Some(Seq(11)));
        assert!(
            j.redo_preview(Some("Archive 1 conversation"), back)
                .is_empty()
        );
        assert!(
            !j.undo_preview(Some("Archive 1 conversation"), now)
                .is_empty()
        );
        // A new action clears the redo side; a refused one leaves no trace.
        j.undone(Seq(11), Some(Seq(12)));
        j.record(3, "Pin 1 conversation".into(), Vec::new());
        assert!(
            j.redo_preview(Some("Archive 1 conversation"), back)
                .is_empty()
        );
        j.rejected(3);
        j.nothing_to_undo();
        assert!(
            j.undo_preview(Some("Archive 1 conversation"), now)
                .is_empty()
        );
    }

    #[test]
    fn bulk_and_masked_entries() {
        let mut p = Pending::default();
        p.add_bulk(
            1,
            BulkOverlay {
                mailbox: "inbox".into(),
                except: ["t9".to_owned()].into(),
                hide: true,
                target: Some("archive".into()),
                seen: None,
                flagged: None,
            },
        );
        assert_eq!(p.bulks().len(), 1);
        assert!(p.bulk("inbox").is_some_and(|b| b.hide));
        assert!(p.bulk("archive").is_none());
        p.add_masked(
            2,
            MaskedOverlay {
                id: "m1".into(),
                state: Some(ui::MaskedState::Disabled),
                description: None,
            },
        );
        p.add_masked(
            3,
            MaskedOverlay {
                id: "m1".into(),
                state: None,
                description: Some("Bike shop".into()),
            },
        );
        assert_eq!(
            p.masked("m1"),
            (Some(ui::MaskedState::Disabled), Some("Bike shop"))
        );
        assert_eq!(p.masked("m2"), (None, None));
        p.remove(1);
        assert!(p.bulk("inbox").is_none());
    }
}
