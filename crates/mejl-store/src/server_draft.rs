//! Server copies of local drafts: what the engine still has to upload and
//! which copies it has to destroy.
//!
//! JMAP emails are immutable, so every version of a draft is a new email.
//! The engine stores the newest message on the draft row with a revision
//! number ([`Store::queue_draft_upload`]), uploads the newest revision
//! ([`Store::draft_uploads`]), and records which revision the server has
//! ([`Store::finish_draft_upload`]). Copies nobody needs any more go to the
//! draft trash, which survives restarts, until they are destroyed.

use jiff::Timestamp;
use mejl_jmap::Id;
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use crate::codec::{from_json, json, ts_ms};
use crate::error::{Error, Result};
use crate::store::Store;

/// A draft whose newest revision is not on the server yet.
#[derive(Debug, Clone, PartialEq)]
pub struct DraftUpload {
    /// The local draft.
    pub local_id: String,
    /// The revision to upload. Pass it back in [`DraftUploadDone::rev`].
    pub rev: i64,
    /// The message, as the engine queued it.
    pub payload: Value,
    /// The current server copy, which the upload replaces.
    pub server_email_id: Option<Id>,
    /// The Message-ID every server copy of this draft carries, once the
    /// first upload chose one.
    pub message_id: Option<String>,
    /// An upload was sent but its answer never recorded (a crash or a lost
    /// response), so a copy the store does not know about may exist.
    pub uncertain: bool,
}

/// What one upload did, for [`Store::finish_draft_upload`].
#[derive(Debug, Clone, PartialEq)]
pub struct DraftUploadDone {
    /// The local draft.
    pub local_id: String,
    /// The revision that was sent.
    pub rev: i64,
    /// Whether the server took that revision.
    pub uploaded: bool,
    /// The draft's server copy now, if any.
    pub server_email_id: Option<Id>,
    /// Server copies that are not wanted but may still exist.
    pub trash: Vec<Id>,
}

/// A server copy of a draft to destroy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftTrash {
    /// The trash entry, for [`Store::clear_draft_trash`].
    pub id: i64,
    /// The copy's email id.
    pub email_id: Option<Id>,
    /// Copies with this Message-ID, for copies whose id was never learned.
    pub message_id: Option<String>,
}

impl Store {
    /// Stores `payload` as the newest version of a draft's server copy and
    /// bumps its revision. Returns false when the local draft does not
    /// exist; a server copy is only made for a saved local draft.
    pub fn queue_draft_upload(&mut self, local_id: &str, payload: &Value) -> Result<bool> {
        let n = self.state.execute(
            "UPDATE draft SET upload_json = ?2, upload_rev = upload_rev + 1 WHERE local_id = ?1",
            params![local_id, json(payload)?],
        )?;
        Ok(n > 0)
    }

    /// Drafts whose newest revision is not on the server yet, oldest
    /// change first.
    pub fn draft_uploads(&self) -> Result<Vec<DraftUpload>> {
        let mut stmt = self.state.prepare_cached(
            "SELECT local_id, upload_rev, upload_json, server_email_id, server_message_id,
                 upload_in_flight
             FROM draft
             WHERE upload_json IS NOT NULL AND upload_rev > uploaded_rev
             ORDER BY updated_at, local_id",
        )?;
        type Raw = (String, i64, String, Option<String>, Option<String>, bool);
        let rows: Vec<Raw> = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        rows.into_iter()
            .map(|(local_id, rev, payload, server, message_id, uncertain)| {
                Ok(DraftUpload {
                    local_id,
                    rev,
                    payload: from_json(&payload)?,
                    server_email_id: server.map(Id::from),
                    message_id,
                    uncertain,
                })
            })
            .collect()
    }

    /// Marks an upload of a draft as sent, before the request goes out, so
    /// a crash or a lost answer is noticed later. Keeps the draft's
    /// Message-ID, or sets it to `message_id` if it has none yet, and
    /// returns the one to use. `None` when the draft is gone.
    pub fn begin_draft_upload(
        &mut self,
        local_id: &str,
        message_id: &str,
    ) -> Result<Option<String>> {
        if message_id.is_empty() {
            return Err(Error::Invalid("a draft copy needs a Message-ID".into()));
        }
        Ok(self
            .state
            .query_row(
                "UPDATE draft SET upload_in_flight = 1,
                     server_message_id = coalesce(server_message_id, ?2)
                 WHERE local_id = ?1 RETURNING server_message_id",
                params![local_id, message_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    }

    /// Records the answer to an upload. When the draft is still there, its
    /// server copy becomes `done.server_email_id` and, if the revision was
    /// uploaded, the draft is clean unless a newer revision came in
    /// meanwhile. When the draft was deleted in the meantime, the new copy
    /// goes to the trash too. Returns whether the draft still exists.
    pub fn finish_draft_upload(&mut self, done: &DraftUploadDone, now: Timestamp) -> Result<bool> {
        let tx = self.state.transaction()?;
        let n = tx.execute(
            "UPDATE draft SET upload_in_flight = 0, server_email_id = ?2,
                 uploaded_rev = CASE WHEN ?3 THEN max(uploaded_rev, ?4) ELSE uploaded_rev END
             WHERE local_id = ?1",
            params![
                done.local_id,
                done.server_email_id.as_ref().map(Id::as_str),
                done.uploaded,
                done.rev
            ],
        )?;
        let orphan = if n == 0 {
            done.server_email_id.as_ref()
        } else {
            None
        };
        for id in done.trash.iter().chain(orphan) {
            tx.execute(
                "INSERT INTO draft_trash (email_id, created_at) VALUES (?1, ?2)",
                params![id.as_str(), ts_ms(now)],
            )?;
        }
        tx.commit()?;
        Ok(n > 0)
    }

    /// Puts server copies of drafts in the trash: by email id, and every
    /// draft copy with one of `message_ids`.
    pub fn trash_draft_copies(
        &mut self,
        email_ids: &[Id],
        message_ids: &[&str],
        now: Timestamp,
    ) -> Result<()> {
        let tx = self.state.transaction()?;
        for id in email_ids {
            tx.execute(
                "INSERT INTO draft_trash (email_id, created_at) VALUES (?1, ?2)",
                params![id.as_str(), ts_ms(now)],
            )?;
        }
        for mid in message_ids.iter().filter(|m| !m.is_empty()) {
            tx.execute(
                "INSERT INTO draft_trash (message_id, created_at) VALUES (?1, ?2)",
                params![mid, ts_ms(now)],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Up to `limit` server copies to destroy, oldest first.
    pub fn draft_trash(&self, limit: usize) -> Result<Vec<DraftTrash>> {
        let mut stmt = self.state.prepare_cached(
            "SELECT id, email_id, message_id FROM draft_trash ORDER BY id LIMIT ?1",
        )?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = stmt.query_map([limit], |r| {
            Ok(DraftTrash {
                id: r.get(0)?,
                email_id: r.get::<_, Option<String>>(1)?.map(Id::from),
                message_id: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Removes trash entries whose copies are gone.
    pub fn clear_draft_trash(&mut self, ids: &[i64]) -> Result<()> {
        let tx = self.state.transaction()?;
        for id in ids {
            tx.execute("DELETE FROM draft_trash WHERE id = ?1", [id])?;
        }
        tx.commit()?;
        Ok(())
    }
}
