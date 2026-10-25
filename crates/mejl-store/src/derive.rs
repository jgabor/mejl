//! Derived cache state: effective flags and membership, conversation rows,
//! local counters and overlay count deltas.
//!
//! Everything here is a function of server truth plus the overlay mirror,
//! and runs inside the caller's cache transaction.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use mejl_jmap::Patch;
use mejl_jmap::types::EmailAddress;
use mejl_jmap::types::keyword;
use rusqlite::{Connection, OptionalExtension, params};

use crate::codec::{from_json, from_opt_json, json};
use crate::error::Result;
use crate::overlay::EmailFlags;

/// Overlay mirror kinds.
pub(crate) mod kind {
    pub const EMAIL_PATCH: &str = "email_patch";
    pub const EMAIL_DESTROY: &str = "email_destroy";
    pub const MAILBOX_CREATE: &str = "mailbox_create";
    pub const MASKED_CREATE: &str = "masked_create";
    pub const MASKED_UPDATE: &str = "masked_update";
    pub const CONTACT_CREATE: &str = "contact_create";
}

/// Threads whose conversation rows must be rebuilt before commit.
#[derive(Debug, Default)]
pub(crate) struct Dirty {
    pub threads: BTreeSet<String>,
}

/// One pending change to an email from the overlay mirror.
#[derive(Debug, Clone)]
pub(crate) enum EmailOp {
    Patch(Patch),
    Destroy,
}

/// The parts of an email row the derived state needs.
#[derive(Debug, Clone)]
pub(crate) struct EmailCore {
    pub rid: i64,
    pub thread_id: String,
    pub received_at: i64,
    pub srv: EmailFlags,
}

pub(crate) fn flags_from_json(keywords: &str, mailboxes: &str) -> Result<EmailFlags> {
    let keywords: BTreeMap<String, bool> = from_json(keywords)?;
    let mailboxes: BTreeMap<mejl_jmap::Id, bool> = from_json(mailboxes)?;
    Ok(EmailFlags::from_maps(Some(&keywords), Some(&mailboxes)))
}

pub(crate) fn set_json(set: &BTreeSet<String>) -> Result<String> {
    let map: BTreeMap<&str, bool> = set.iter().map(|k| (k.as_str(), true)).collect();
    json(&map)
}

pub(crate) fn load_core(conn: &Connection, email_id: &str) -> Result<Option<EmailCore>> {
    let mut stmt = conn.prepare_cached(
        "SELECT rid, thread_id, received_at, srv_keywords_json, srv_mailbox_ids_json
         FROM email WHERE id = ?1",
    )?;
    let row = stmt
        .query_row([email_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .optional()?;
    let Some((rid, thread_id, received_at, kw, mb)) = row else {
        return Ok(None);
    };
    Ok(Some(EmailCore {
        rid,
        thread_id,
        received_at,
        srv: flags_from_json(&kw, &mb)?,
    }))
}

/// The effective flags of a cached email as the UI sees them.
pub(crate) fn effective_flags(conn: &Connection, email_id: &str) -> Result<Option<EmailFlags>> {
    let mut stmt = conn.prepare_cached("SELECT rid, keywords_json FROM email WHERE id = ?1")?;
    let row = stmt
        .query_row([email_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })
        .optional()?;
    let Some((rid, kw)) = row else {
        return Ok(None);
    };
    let keywords: BTreeMap<String, bool> = from_json(&kw)?;
    let mut flags = EmailFlags::from_maps(Some(&keywords), None);
    let mut stmt =
        conn.prepare_cached("SELECT mailbox_id FROM email_mailbox WHERE email_rid = ?1")?;
    for m in stmt.query_map([rid], |r| r.get::<_, String>(0))? {
        flags.mailbox_ids.insert(m?);
    }
    Ok(Some(flags))
}

/// Pending and done-but-unconfirmed changes to an email, oldest first.
pub(crate) fn email_ops(conn: &Connection, email_id: &str) -> Result<Vec<(i64, bool, EmailOp)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT seq, kind, payload_json, done FROM overlay
         WHERE target_id = ?1 AND kind IN ('email_patch', 'email_destroy') ORDER BY seq",
    )?;
    let rows = stmt.query_map([email_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, bool>(3)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (seq, kind, payload, done) = row?;
        let op = if kind == kind::EMAIL_DESTROY {
            EmailOp::Destroy
        } else {
            EmailOp::Patch(from_opt_json(payload.as_deref())?)
        };
        out.push((seq, done, op));
    }
    Ok(out)
}

/// Re-derives an email's effective state from server truth and its overlay
/// entries.
pub(crate) fn refresh_email(conn: &Connection, email_id: &str, dirty: &mut Dirty) -> Result<()> {
    let Some(core) = load_core(conn, email_id)? else {
        return Ok(());
    };
    let ops: Vec<EmailOp> = email_ops(conn, email_id)?
        .into_iter()
        .map(|(_, _, op)| op)
        .collect();
    write_effective(conn, &core, &ops, dirty)
}

pub(crate) fn write_effective(
    conn: &Connection,
    core: &EmailCore,
    ops: &[EmailOp],
    dirty: &mut Dirty,
) -> Result<()> {
    let mut eff = core.srv.clone();
    let mut hidden = false;
    for op in ops {
        match op {
            EmailOp::Patch(p) => eff.apply(p),
            EmailOp::Destroy => hidden = true,
        }
    }
    conn.prepare_cached(
        "UPDATE email SET keywords_json = ?2, is_seen = ?3, is_flagged = ?4, is_answered = ?5,
             is_draft = ?6, has_overlay = ?7, is_hidden = ?8
         WHERE rid = ?1",
    )?
    .execute(params![
        core.rid,
        set_json(&eff.keywords)?,
        eff.has_keyword(keyword::SEEN),
        eff.has_keyword(keyword::FLAGGED),
        eff.has_keyword(keyword::ANSWERED),
        eff.has_keyword(keyword::DRAFT),
        !ops.is_empty(),
        hidden,
    ])?;
    let members = if hidden {
        BTreeSet::new()
    } else {
        eff.mailbox_ids
    };
    set_membership(conn, core.rid, core.received_at, &members)?;
    dirty.threads.insert(core.thread_id.clone());
    Ok(())
}

/// Makes the email's `email_mailbox` rows match `members`, keeping the
/// per-mailbox message counters exact.
pub(crate) fn set_membership(
    conn: &Connection,
    rid: i64,
    received_at: i64,
    members: &BTreeSet<String>,
) -> Result<()> {
    let mut existing: Vec<(String, i64)> = Vec::new();
    {
        let mut stmt = conn.prepare_cached(
            "SELECT mailbox_id, received_at FROM email_mailbox WHERE email_rid = ?1",
        )?;
        for row in stmt.query_map([rid], |r| Ok((r.get(0)?, r.get(1)?)))? {
            existing.push(row?);
        }
    }
    for (mailbox, at) in &existing {
        if !members.contains(mailbox) || *at != received_at {
            conn.prepare_cached(
                "DELETE FROM email_mailbox WHERE mailbox_id = ?1 AND received_at = ?2 AND email_rid = ?3",
            )?
            .execute(params![mailbox, at, rid])?;
            bump_local(conn, mailbox, -1, 0)?;
        }
    }
    for mailbox in members {
        if existing
            .iter()
            .any(|(m, at)| m == mailbox && *at == received_at)
        {
            continue;
        }
        conn.prepare_cached(
            "INSERT INTO email_mailbox (mailbox_id, received_at, email_rid) VALUES (?1, ?2, ?3)",
        )?
        .execute(params![mailbox, received_at, rid])?;
        bump_local(conn, mailbox, 1, 0)?;
    }
    Ok(())
}

fn bump_local(conn: &Connection, mailbox: &str, messages: i64, conversations: i64) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO mailbox_local (mailbox_id, messages, conversations) VALUES (?1, ?2, ?3)
         ON CONFLICT (mailbox_id) DO UPDATE SET
             messages = messages + excluded.messages,
             conversations = conversations + excluded.conversations",
    )?
    .execute(params![mailbox, messages, conversations])?;
    Ok(())
}

struct ThreadEmail {
    rid: i64,
    received_at: i64,
    from: Vec<EmailAddress>,
    seen: bool,
    flagged: bool,
    attachment: bool,
    masked: bool,
    overlay: bool,
    hidden: bool,
    srv_keywords: String,
    srv_mailboxes: String,
}

#[derive(Default)]
struct Agg {
    latest_at: i64,
    latest_rid: i64,
    count: i64,
    unread: i64,
    flagged: bool,
    attachment: bool,
    masked: bool,
}

#[derive(Default, Clone, Copy)]
struct Counts {
    emails: i64,
    unread: i64,
}

const MAX_PARTICIPANTS: usize = 8;

/// Rebuilds the conversation rows and count deltas of the given threads.
pub(crate) fn refresh_threads(conn: &Connection, threads: &BTreeSet<String>) -> Result<()> {
    for thread in threads {
        refresh_thread(conn, thread)?;
    }
    Ok(())
}

fn refresh_thread(conn: &Connection, thread_id: &str) -> Result<()> {
    {
        let mut stmt = conn
            .prepare_cached("DELETE FROM conversation WHERE thread_id = ?1 RETURNING mailbox_id")?;
        let removed: Vec<String> = stmt
            .query_map([thread_id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for mailbox in removed {
            bump_local(conn, &mailbox, 0, -1)?;
        }
    }
    conn.prepare_cached("DELETE FROM count_delta WHERE thread_id = ?1")?
        .execute([thread_id])?;

    let mut emails: Vec<ThreadEmail> = Vec::new();
    {
        let mut stmt = conn.prepare_cached(
            "SELECT rid, received_at, from_json, is_seen, is_flagged, has_attachment,
                    masked_header IS NOT NULL, has_overlay, is_hidden,
                    srv_keywords_json, srv_mailbox_ids_json
             FROM email WHERE thread_id = ?1 ORDER BY received_at, rid",
        )?;
        let rows = stmt.query_map([thread_id], |r| {
            Ok((
                ThreadEmail {
                    rid: r.get(0)?,
                    received_at: r.get(1)?,
                    from: Vec::new(),
                    seen: r.get(3)?,
                    flagged: r.get(4)?,
                    attachment: r.get(5)?,
                    masked: r.get(6)?,
                    overlay: r.get(7)?,
                    hidden: r.get(8)?,
                    srv_keywords: r.get(9)?,
                    srv_mailboxes: r.get(10)?,
                },
                r.get::<_, Option<String>>(2)?,
            ))
        })?;
        for row in rows {
            let (mut email, from) = row?;
            email.from = from_opt_json(from.as_deref())?;
            emails.push(email);
        }
    }
    if emails.is_empty() {
        return Ok(());
    }
    let by_rid: HashMap<i64, usize> = emails.iter().enumerate().map(|(i, e)| (e.rid, i)).collect();

    let mut members: Vec<(String, i64)> = Vec::new();
    {
        let mut stmt = conn.prepare_cached(
            "SELECT em.mailbox_id, em.email_rid FROM email e
             JOIN email_mailbox em ON em.email_rid = e.rid WHERE e.thread_id = ?1",
        )?;
        for row in stmt.query_map([thread_id], |r| Ok((r.get(0)?, r.get(1)?)))? {
            members.push(row?);
        }
    }

    let mut participants: Vec<EmailAddress> = Vec::new();
    let mut seen_addresses: BTreeSet<String> = BTreeSet::new();
    for email in emails.iter().filter(|e| !e.hidden) {
        for addr in &email.from {
            if participants.len() >= MAX_PARTICIPANTS {
                break;
            }
            if seen_addresses.insert(addr.email.to_lowercase()) {
                participants.push(addr.clone());
            }
        }
    }
    let participants_json = json(&participants)?;
    let thread_count = emails.iter().filter(|e| !e.hidden).count();

    let mut aggs: BTreeMap<&str, Agg> = BTreeMap::new();
    for (mailbox, rid) in &members {
        let Some(email) = by_rid.get(rid).map(|&i| &emails[i]) else {
            continue;
        };
        let agg = aggs.entry(mailbox.as_str()).or_default();
        if agg.count == 0 || (email.received_at, email.rid) > (agg.latest_at, agg.latest_rid) {
            agg.latest_at = email.received_at;
            agg.latest_rid = email.rid;
        }
        agg.count += 1;
        agg.unread += i64::from(!email.seen);
        agg.flagged |= email.flagged;
        agg.attachment |= email.attachment;
        agg.masked |= email.masked;
    }
    for (mailbox, agg) in &aggs {
        conn.prepare_cached(
            "INSERT INTO conversation (mailbox_id, latest_at, thread_id, latest_rid, count_in_mailbox,
                 thread_count, unread_count, any_flagged, any_attachment, any_masked, participants_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )?
        .execute(params![
            mailbox,
            agg.latest_at,
            thread_id,
            agg.latest_rid,
            agg.count,
            i64::try_from(thread_count).unwrap_or(i64::MAX),
            agg.unread,
            agg.flagged,
            agg.attachment,
            agg.masked,
            participants_json,
        ])?;
        bump_local(conn, mailbox, 0, 1)?;
    }

    if emails.iter().any(|e| e.overlay) {
        write_count_deltas(conn, thread_id, &emails, &aggs)?;
    }
    Ok(())
}

/// Records how the effective state of a thread differs from server truth,
/// so mailbox counts can be adjusted at read time.
fn write_count_deltas(
    conn: &Connection,
    thread_id: &str,
    emails: &[ThreadEmail],
    eff: &BTreeMap<&str, Agg>,
) -> Result<()> {
    let mut srv: BTreeMap<String, Counts> = BTreeMap::new();
    for email in emails {
        let flags = flags_from_json(&email.srv_keywords, &email.srv_mailboxes)?;
        let unread = !flags.has_keyword(keyword::SEEN);
        for mailbox in flags.mailbox_ids {
            let c = srv.entry(mailbox).or_default();
            c.emails += 1;
            c.unread += i64::from(unread);
        }
    }
    let mailboxes: BTreeSet<&str> = srv
        .keys()
        .map(String::as_str)
        .chain(eff.keys().copied())
        .collect();
    for mailbox in mailboxes {
        let s = srv.get(mailbox).copied().unwrap_or_default();
        let e = eff
            .get(mailbox)
            .map(|a| Counts {
                emails: a.count,
                unread: a.unread,
            })
            .unwrap_or_default();
        let d_te = e.emails - s.emails;
        let d_ue = e.unread - s.unread;
        let d_tt = i64::from(e.emails > 0) - i64::from(s.emails > 0);
        let d_ut = i64::from(e.unread > 0) - i64::from(s.unread > 0);
        if d_te == 0 && d_ue == 0 && d_tt == 0 && d_ut == 0 {
            continue;
        }
        conn.prepare_cached(
            "INSERT INTO count_delta (thread_id, mailbox_id, d_total_emails, d_unread_emails,
                 d_total_threads, d_unread_threads) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?
        .execute(params![thread_id, mailbox, d_te, d_ue, d_tt, d_ut])?;
    }
    Ok(())
}
