//! Conversions between the store's and engine's types and the UI's view
//! models. Everything here is pure, so it is tested without a store.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;
use mejl_jmap::Id;
use mejl_jmap::types::{EmailAddress, Identity, MaskedEmailState, Role};
use mejl_store::{
    Draft, DraftHeaders, EmailDetail, EmailFlags, IssueKind, MailboxRow, MaskedEmailRow,
    MessageHeaders, RuleRender,
};
use mejl_sync::{DataChanges, PushState, SyncStatus};
use mejl_ui as ui;
use serde_json::{Value, json};

/// A UI participant from a JMAP address. Blank names count as none.
pub fn participant(a: &EmailAddress) -> ui::Participant {
    ui::Participant {
        name: a.name.clone().filter(|n| !n.trim().is_empty()),
        email: a.email.clone(),
    }
}

/// A JMAP address from a UI participant.
pub fn address(p: &ui::Participant) -> EmailAddress {
    EmailAddress {
        name: p.name.clone().filter(|n| !n.trim().is_empty()),
        email: p.email.trim().to_owned(),
    }
}

fn participants(list: &[EmailAddress]) -> Vec<ui::Participant> {
    list.iter().map(participant).collect()
}

fn addresses(list: &[ui::Participant]) -> Vec<EmailAddress> {
    list.iter().map(address).collect()
}

/// The UI's role for a JMAP mailbox role, when the UI has one.
pub fn role(r: &Role) -> Option<ui::MailboxRole> {
    Some(match r {
        Role::Inbox => ui::MailboxRole::Inbox,
        Role::Drafts => ui::MailboxRole::Drafts,
        Role::Sent => ui::MailboxRole::Sent,
        Role::Archive => ui::MailboxRole::Archive,
        Role::Junk => ui::MailboxRole::Junk,
        Role::Trash => ui::MailboxRole::Trash,
        _ => return None,
    })
}

fn count(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Mailbox views with their tree depth. A parent that is missing (not
/// synced yet, or a cycle in bad data) ends the walk, so depth is bounded.
pub fn mailbox_views(rows: &[MailboxRow]) -> Vec<ui::MailboxView> {
    let parents: BTreeMap<&str, Option<&str>> = rows
        .iter()
        .map(|m| (m.id.as_str(), m.parent_id.as_ref().map(Id::as_str)))
        .collect();
    let depth = |id: &str| -> u8 {
        let mut d = 0u8;
        let mut at = parents.get(id).copied().flatten();
        while let Some(p) = at {
            if d >= 16 {
                break;
            }
            d += 1;
            at = parents.get(p).copied().flatten();
        }
        d
    };
    rows.iter()
        .map(|m| ui::MailboxView {
            id: ui::MailboxId::new(m.id.as_str()),
            parent: m.parent_id.as_ref().map(|p| ui::MailboxId::new(p.as_str())),
            name: m.name.clone(),
            role: m.role.as_ref().and_then(role),
            depth: depth(m.id.as_str()),
            sort_order: m.sort_order,
            total_threads: count(m.total_threads),
            unread_threads: count(m.unread_threads),
            total_emails: count(m.total_emails),
            unread_emails: count(m.unread_emails),
        })
        .collect()
}

/// A list row from the store's conversation row.
pub fn conversation_row(r: &mejl_store::ConversationRow) -> ui::ConversationRow {
    let mut people: Vec<ui::Participant> = participants(&r.participants);
    // The store keeps them oldest first; the UI shows newest first.
    people.reverse();
    ui::ConversationRow {
        thread_id: ui::ThreadId::new(r.thread_id.as_str()),
        latest_email_id: ui::EmailId::new(r.email_id.as_str()),
        from: r.from.first().map(participant).unwrap_or_default(),
        participants: people,
        subject: r.subject.clone(),
        preview: r.preview.clone(),
        date: r.received_at,
        message_count: r.thread_count.max(r.count_in_mailbox),
        unread: r.is_unread(),
        flagged: r.flagged,
        has_attachment: r.has_attachment,
        masked: r.masked,
    }
}

/// The flags the overlay rules work on, from cached headers.
pub fn flags_of(h: &MessageHeaders) -> EmailFlags {
    EmailFlags {
        keywords: h.keywords.iter().map(|k| k.to_lowercase()).collect(),
        mailbox_ids: h
            .mailbox_ids
            .iter()
            .map(|m| m.as_str().to_owned())
            .collect(),
    }
}

/// One message of a conversation for the reader.
pub fn message_summary(h: &MessageHeaders) -> ui::MessageSummary {
    let r = &h.row;
    ui::MessageSummary {
        email_id: ui::EmailId::new(r.email_id.as_str()),
        from: r.from.first().map(participant).unwrap_or_default(),
        to: participants(&r.to),
        cc: participants(&r.cc),
        date: r.received_at,
        preview: r.preview.clone(),
        unread: !r.seen,
        flagged: r.flagged,
        has_attachment: r.has_attachment,
        mailboxes: h
            .mailbox_ids
            .iter()
            .map(|m| ui::MailboxId::new(m.as_str()))
            .collect(),
        reply_to: participants(&h.reply_to),
        message_id: h.message_id.first().cloned(),
        references: h.references.clone(),
    }
}

/// Applies overlay flags to a message summary.
pub fn apply_flags(m: &mut ui::MessageSummary, flags: &EmailFlags) {
    m.unread = !flags.has_keyword("$seen");
    m.flagged = flags.has_keyword("$flagged");
    m.mailboxes = flags
        .mailbox_ids
        .iter()
        .map(|id| ui::MailboxId::new(id.as_str()))
        .collect();
}

fn is_html(p: &mejl_store::BodyPart) -> bool {
    p.mime_type
        .as_deref()
        .is_some_and(|m| m.trim().eq_ignore_ascii_case("text/html"))
}

/// Joins the values of the parts that are (`html`) or are not HTML. JMAP
/// fills `textBody` with the HTML part when a message has no plain one,
/// and `htmlBody` with the plain part when it has no HTML, so the type of
/// each part decides, not the list it came in.
fn join_parts(parts: &[mejl_store::BodyPart], html: bool) -> Option<String> {
    let texts: Vec<&str> = parts
        .iter()
        .filter(|p| is_html(p) == html)
        .map(|p| p.value.as_str())
        .filter(|v| !v.is_empty())
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

/// A body for the reader, when it is cached.
pub fn body_view(id: &str, detail: &EmailDetail) -> Option<ui::BodyView> {
    let body = detail.body.as_ref()?;
    Some(ui::BodyView {
        email_id: ui::EmailId::new(id),
        text: join_parts(&body.text_parts, false).or_else(|| join_parts(&body.html_parts, false)),
        html: join_parts(&body.html_parts, true).or_else(|| join_parts(&body.text_parts, true)),
        attachments: body
            .attachments
            .iter()
            .filter(|a| a.disposition.as_deref() != Some("inline") || a.cid.is_none())
            .filter_map(|a| {
                Some(ui::AttachmentView {
                    blob_id: ui::BlobId::new(a.blob_id.as_ref()?.as_str()),
                    name: a.name.clone().unwrap_or_else(|| "attachment".to_owned()),
                    mime_type: a
                        .mime_type
                        .clone()
                        .unwrap_or_else(|| "application/octet-stream".to_owned()),
                    size: a.size,
                })
            })
            .collect(),
        headers: body
            .headers
            .iter()
            .map(|h| (h.name.clone(), h.value.trim().to_owned()))
            .collect(),
    })
}

/// A sending identity.
pub fn identity_view(i: &Identity) -> ui::IdentityView {
    ui::IdentityView {
        id: i.id.as_str().to_owned(),
        name: i.name.clone().unwrap_or_default(),
        email: i.email.clone().unwrap_or_default(),
        signature: i.text_signature.clone().unwrap_or_default(),
    }
}

/// The UI's masked state. An unknown or missing state reads as enabled,
/// which is what Fastmail does with mail to it.
pub fn masked_state(s: Option<&MaskedEmailState>) -> ui::MaskedState {
    match s {
        Some(MaskedEmailState::Pending) => ui::MaskedState::Pending,
        Some(MaskedEmailState::Disabled) => ui::MaskedState::Disabled,
        Some(MaskedEmailState::Deleted) => ui::MaskedState::Deleted,
        _ => ui::MaskedState::Enabled,
    }
}

/// The engine's masked state for one the user picked. Pending cannot be
/// chosen; the server sets it.
pub fn masked_state_for_sync(s: ui::MaskedState) -> Option<mejl_sync::MaskedState> {
    match s {
        ui::MaskedState::Enabled => Some(mejl_sync::MaskedState::Enabled),
        ui::MaskedState::Disabled => Some(mejl_sync::MaskedState::Disabled),
        ui::MaskedState::Deleted => Some(mejl_sync::MaskedState::Deleted),
        ui::MaskedState::Pending => None,
    }
}

/// The site of a mask as the UI shows it: JMAP's `forDomain` is an origin
/// such as `https://shop.example.com`, the UI works with the host.
pub fn site_host(for_domain: &str) -> String {
    let t = for_domain.trim();
    let t = t
        .strip_prefix("https://")
        .or_else(|| t.strip_prefix("http://"))
        .unwrap_or(t);
    t.trim_end_matches('/').to_owned()
}

/// The `forDomain` to send for a site the user typed: an origin, with
/// `https://` added when no scheme was given.
pub fn site_origin(site: &str) -> String {
    let t = site.trim().trim_end_matches('/');
    if t.is_empty() || t.contains("://") {
        t.to_owned()
    } else {
        format!("https://{t}")
    }
}

/// A Masked Email address with local statistics.
pub fn masked_view(m: &MaskedEmailRow) -> ui::MaskedEmailView {
    let w = &m.masked;
    ui::MaskedEmailView {
        id: w.id.as_str().to_owned(),
        email: w.email.clone().unwrap_or_default(),
        state: masked_state(w.state.as_ref()),
        for_domain: site_host(w.for_domain.as_deref().unwrap_or_default()),
        description: w.description.clone().unwrap_or_default(),
        created_at: w.created_at,
        last_message_at: m.last_seen.max(w.last_message_at),
        created_by: w.created_by.clone().unwrap_or_default(),
        local_messages: count(m.local_messages),
        local_in_spam: count(m.local_in_spam),
    }
}

/// The UI's search issue kind.
pub fn issue_kind(k: &IssueKind) -> ui::SearchIssueKind {
    match k {
        IssueKind::UnsupportedOperator => ui::SearchIssueKind::UnsupportedOperator,
        IssueKind::UnsupportedValue => ui::SearchIssueKind::UnsupportedValue,
        IssueKind::UnknownMailbox => ui::SearchIssueKind::UnknownMailbox,
        IssueKind::BadDate => ui::SearchIssueKind::BadDate,
        IssueKind::BadSize => ui::SearchIssueKind::BadSize,
        IssueKind::EmptyValue => ui::SearchIssueKind::EmptyValue,
        IssueKind::MisplacedOr => ui::SearchIssueKind::MisplacedOr,
        IssueKind::Grouping => ui::SearchIssueKind::Grouping,
    }
}

/// A highlight from a store highlight.
pub fn highlight(h: &mejl_store::Highlighted) -> ui::Highlight {
    ui::Highlight {
        text: h.text.clone(),
        matches: h.matches.clone(),
    }
}

/// A copy of the rule block from the history.
pub fn rule_render_view(r: &RuleRender) -> ui::RuleRenderView {
    ui::RuleRenderView {
        id: r.id,
        rendered_at: r.rendered_at,
        sieve: r.sieve.clone(),
        body_sha256: r.body_sha256.clone(),
        confirmed_at: r.user_confirmed_paste_at,
    }
}

fn sieve_address(a: &EmailAddress) -> mejl_sieve::EmailAddress {
    mejl_sieve::EmailAddress {
        name: a.name.clone(),
        email: a.email.clone(),
    }
}

/// A cached message as the rule dry run sees it. Only list properties are
/// cached for most mail, so header tests other than the masked address see
/// no headers unless the body was fetched.
pub fn dry_run_message(h: &MessageHeaders) -> ui::DryRunMessage {
    let r = &h.row;
    let mut headers = Vec::new();
    if let Some(id) = h.message_id.first() {
        headers.push(("Message-ID".to_owned(), format!("<{id}>")));
    }
    ui::DryRunMessage {
        email_id: ui::EmailId::new(r.email_id.as_str()),
        thread_id: ui::ThreadId::new(r.thread_id.as_str()),
        date: r.received_at,
        mailboxes: h
            .mailbox_ids
            .iter()
            .map(|m| ui::MailboxId::new(m.as_str()))
            .collect(),
        facts: mejl_sieve::MessageFacts {
            from: r.from.iter().map(sieve_address).collect(),
            to: r.to.iter().map(sieve_address).collect(),
            cc: r.cc.iter().map(sieve_address).collect(),
            subject: Some(r.subject.clone()),
            headers,
            size: r.size,
            delivered_to: h.masked_address.iter().cloned().collect(),
        },
    }
}

/// The status line's sync state. Push syncs happen often, so the line says
/// "syncing" only until the first pass finished; after that it keeps the
/// last good state, as Fastmail's own clients do.
pub fn sync_status(s: &SyncStatus) -> ui::SyncStatus {
    if !s.online {
        if s.last_error.is_none() && s.last_sync.is_none() {
            return if s.activity.syncing.is_some() {
                ui::SyncStatus::Syncing
            } else {
                ui::SyncStatus::Starting
            };
        }
        return ui::SyncStatus::Offline { pending: s.pending };
    }
    if s.last_sync.is_none() {
        return ui::SyncStatus::Syncing;
    }
    ui::SyncStatus::Synced
}

/// Where drafts are kept, for compose. `held` names drafts whose server
/// copy the app is still holding back (see the app's draft copy debounce),
/// which the engine does not know about yet.
pub fn draft_sync<'a>(
    s: &SyncStatus,
    held: impl Iterator<Item = &'a ui::DraftId>,
) -> ui::DraftSync {
    let mut waiting: BTreeSet<ui::DraftId> = s
        .drafts_waiting
        .iter()
        .map(|id| ui::DraftId::from(id.as_str()))
        .collect();
    waiting.extend(held.cloned());
    ui::DraftSync {
        waiting,
        refused: s
            .drafts_refused
            .iter()
            .map(|(id, why)| (ui::DraftId::from(id.as_str()), why.clone()))
            .collect(),
        local_only: s.drafts_local_only,
        online: s.online,
    }
}

/// The status line's push state.
pub fn push_state(p: PushState) -> ui::PushState {
    match p {
        PushState::Connected => ui::PushState::Connected,
        PushState::Connecting | PushState::Reconnecting { .. } => ui::PushState::Connecting,
        PushState::Polling | PushState::Offline | PushState::Disabled => ui::PushState::Off,
    }
}

/// What the UI re-reads for an engine data change.
pub fn data_change(c: &DataChanges) -> ui::DataChange {
    ui::DataChange {
        mailboxes: c.mailboxes || c.identities || !c.mailbox_lists.is_empty(),
        threads: c
            .threads
            .iter()
            .map(|t| ui::ThreadId::new(t.as_str()))
            .collect(),
        emails: c
            .emails
            .iter()
            .map(|e| ui::EmailId::new(e.as_str()))
            .collect(),
        all: c.everything,
        masked: c.masked_emails || c.everything,
        rules: false,
        // A sent message deletes its local draft.
        drafts: c.outbox || c.everything,
    }
}

fn kind_name(k: ui::ComposeKind) -> &'static str {
    match k {
        ui::ComposeKind::New => "new",
        ui::ComposeKind::Reply => "reply",
        ui::ComposeKind::ReplyAll => "reply-all",
        ui::ComposeKind::Forward => "forward",
    }
}

fn kind_from(name: Option<&str>) -> ui::ComposeKind {
    match name {
        Some("reply") => ui::ComposeKind::Reply,
        Some("reply-all") => ui::ComposeKind::ReplyAll,
        Some("forward") => ui::ComposeKind::Forward,
        _ => ui::ComposeKind::New,
    }
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_owned))
        .collect()
}

/// A local draft record for a compose message. `created_at` is kept from
/// an earlier save of the same draft.
pub fn draft_record(m: &ui::OutgoingMessage, created_at: Timestamp, now: Timestamp) -> Draft {
    let mut extra = serde_json::Map::new();
    extra.insert("kind".into(), json!(kind_name(m.kind)));
    extra.insert("inReplyTo".into(), json!(m.in_reply_to));
    extra.insert("references".into(), json!(m.references));
    if let Some(o) = &m.original {
        extra.insert("original".into(), json!(o.as_str()));
    }
    let attachments: Vec<Value> = m
        .attachments
        .iter()
        .map(|a| {
            json!({
                "blobId": a.blob_id.as_str(),
                "name": a.name,
                "type": a.mime_type,
                "size": a.size,
            })
        })
        .collect();
    Draft {
        local_id: m.draft_id.as_str().to_owned(),
        server_email_id: None,
        identity_id: m.identity_id.as_deref().map(Id::from),
        headers: DraftHeaders {
            from: (!m.from.email.trim().is_empty()).then(|| address(&m.from)),
            to: addresses(&m.to),
            cc: addresses(&m.cc),
            bcc: addresses(&m.bcc),
            subject: m.subject.clone(),
            in_reply_to_email_id: m.original.as_ref().map(|o| Id::from(o.as_str())),
            extra,
        },
        body: m.text_body.clone(),
        format: "plain".into(),
        attachments: Value::Array(attachments),
        created_at,
        updated_at: now,
    }
}

/// A compose message from a local draft.
pub fn draft_message(d: &Draft) -> ui::OutgoingMessage {
    let extra = &d.headers.extra;
    let attachments = d
        .attachments
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            Some(ui::OutgoingAttachment {
                blob_id: ui::BlobId::new(a.get("blobId")?.as_str()?),
                name: a.get("name")?.as_str()?.to_owned(),
                mime_type: a
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream")
                    .to_owned(),
                size: a.get("size").and_then(Value::as_u64).unwrap_or(0),
            })
        })
        .collect();
    ui::OutgoingMessage {
        draft_id: ui::DraftId::new(d.local_id.clone()),
        kind: kind_from(extra.get("kind").and_then(Value::as_str)),
        identity_id: d.identity_id.as_ref().map(|i| i.as_str().to_owned()),
        from: d.headers.from.as_ref().map(participant).unwrap_or_default(),
        to: participants(&d.headers.to),
        cc: participants(&d.headers.cc),
        bcc: participants(&d.headers.bcc),
        subject: d.headers.subject.clone(),
        text_body: d.body.clone(),
        in_reply_to: strings(extra.get("inReplyTo")),
        references: strings(extra.get("references")),
        original: extra
            .get("original")
            .and_then(Value::as_str)
            .map(ui::EmailId::new)
            .or_else(|| {
                d.headers
                    .in_reply_to_email_id
                    .as_ref()
                    .map(|i| ui::EmailId::new(i.as_str()))
            }),
        attachments,
    }
}

/// A resumable draft for compose.
pub fn draft_view(d: &Draft) -> ui::DraftView {
    ui::DraftView {
        message: draft_message(d),
        server_email_id: d
            .server_email_id
            .as_ref()
            .map(|i| ui::EmailId::new(i.as_str())),
        updated_at: d.updated_at,
    }
}

/// The engine's message for a compose message.
pub fn outgoing(m: &ui::OutgoingMessage, identity_id: Id) -> mejl_sync::OutgoingMessage {
    mejl_sync::OutgoingMessage {
        identity_id,
        from: address(&m.from),
        to: addresses(&m.to),
        cc: addresses(&m.cc),
        bcc: addresses(&m.bcc),
        reply_to: Vec::new(),
        subject: m.subject.clone(),
        text_body: m.text_body.clone(),
        html_body: None,
        in_reply_to: m.in_reply_to.clone(),
        references: m.references.clone(),
        attachments: m
            .attachments
            .iter()
            .map(|a| mejl_sync::OutgoingAttachment {
                blob_id: Id::from(a.blob_id.as_str()),
                name: a.name.clone(),
                content_type: a.mime_type.clone(),
            })
            .collect(),
        draft_local_id: Some(m.draft_id.as_str().to_owned()),
    }
}

/// A compose message back from the engine's, for a send cancelled after a
/// restart, when the original compose message is gone. What the engine
/// does not keep (the compose kind, the original message, attachment
/// sizes) comes back empty.
pub fn outgoing_back(m: &mejl_sync::OutgoingMessage) -> ui::OutgoingMessage {
    ui::OutgoingMessage {
        draft_id: ui::DraftId::new(m.draft_local_id.clone().unwrap_or_default()),
        kind: if m.in_reply_to.is_empty() {
            ui::ComposeKind::New
        } else {
            ui::ComposeKind::Reply
        },
        identity_id: Some(m.identity_id.as_str().to_owned()),
        from: participant(&m.from),
        to: participants(&m.to),
        cc: participants(&m.cc),
        bcc: participants(&m.bcc),
        subject: m.subject.clone(),
        text_body: m.text_body.clone(),
        in_reply_to: m.in_reply_to.clone(),
        references: m.references.clone(),
        original: None,
        attachments: m
            .attachments
            .iter()
            .map(|a| ui::OutgoingAttachment {
                blob_id: ui::BlobId::new(a.blob_id.as_str()),
                name: a.name.clone(),
                mime_type: a.content_type.clone(),
                size: 0,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts() -> Timestamp {
        "2026-10-23T14:27:00Z".parse().unwrap()
    }

    fn message() -> ui::OutgoingMessage {
        ui::OutgoingMessage {
            draft_id: ui::DraftId::new("d-1"),
            kind: ui::ComposeKind::Reply,
            identity_id: Some("I1".into()),
            from: ui::Participant::new("Mira Holt", "mira@example.org"),
            to: vec![ui::Participant::new("Juniper Vale", "juniper@example.net")],
            cc: vec![],
            bcc: vec![ui::Participant {
                name: None,
                email: "otto@example.com".into(),
            }],
            subject: "Re: Seed swap".into(),
            text_body: "See you there.\n".into(),
            in_reply_to: vec!["m1@example.net".into()],
            references: vec!["m0@example.net".into(), "m1@example.net".into()],
            original: Some(ui::EmailId::new("E7")),
            attachments: vec![ui::OutgoingAttachment {
                blob_id: ui::BlobId::new("B1"),
                name: "rota.pdf".into(),
                mime_type: "application/pdf".into(),
                size: 2048,
            }],
        }
    }

    #[test]
    fn drafts_round_trip_through_the_store_record() {
        let m = message();
        let d = draft_record(&m, ts(), ts());
        assert_eq!(d.local_id, "d-1");
        assert_eq!(d.headers.in_reply_to_email_id, Some(Id::from("E7")));
        let back = draft_message(&d);
        assert_eq!(back, m);
        let view = draft_view(&d);
        assert_eq!(view.updated_at, ts());
        assert_eq!(view.server_email_id, None);
    }

    #[test]
    fn outgoing_keeps_threading_and_attachments() {
        let o = outgoing(&message(), Id::from("I1"));
        assert_eq!(o.draft_local_id.as_deref(), Some("d-1"));
        assert_eq!(o.to[0].email, "juniper@example.net");
        assert_eq!(o.references.len(), 2);
        assert_eq!(o.attachments[0].content_type, "application/pdf");
        let back = outgoing_back(&o);
        assert_eq!(back.draft_id, ui::DraftId::new("d-1"));
        assert_eq!(back.kind, ui::ComposeKind::Reply);
        assert_eq!(back.subject, "Re: Seed swap");
    }

    #[test]
    fn blank_names_are_dropped() {
        let p = participant(&EmailAddress::with_name("  ", "a@example.org"));
        assert_eq!(p.name, None);
        assert_eq!(address(&p).email, "a@example.org");
    }

    #[test]
    fn mailbox_depth_follows_parents_and_stops_on_cycles() {
        let row = |id: &str, parent: Option<&str>| MailboxRow {
            id: Id::from(id),
            parent_id: parent.map(Id::from),
            name: id.into(),
            role: None,
            sort_order: 0,
            total_emails: 0,
            unread_emails: 0,
            total_threads: 0,
            unread_threads: 0,
            local_messages: 0,
            local_conversations: 0,
            is_subscribed: true,
            my_rights: None,
            pending: false,
        };
        let views = mailbox_views(&[
            row("a", None),
            row("b", Some("a")),
            row("c", Some("b")),
            row("x", Some("y")),
            row("y", Some("x")),
        ]);
        let depth: Vec<u8> = views.iter().map(|v| v.depth).collect();
        assert_eq!(&depth[..3], &[0, 1, 2]);
        assert!(depth[3] <= 16 && depth[4] <= 16);
    }

    #[test]
    fn status_mapping_keeps_the_line_calm() {
        let mut s = SyncStatus::default();
        assert_eq!(sync_status(&s), ui::SyncStatus::Starting);
        s.online = true;
        assert_eq!(sync_status(&s), ui::SyncStatus::Syncing);
        s.last_sync = Some(ts());
        assert_eq!(sync_status(&s), ui::SyncStatus::Synced);
        s.activity.syncing = Some(mejl_sync::SyncPhase::Changes);
        assert_eq!(sync_status(&s), ui::SyncStatus::Synced);
        s.online = false;
        s.pending = 3;
        assert_eq!(sync_status(&s), ui::SyncStatus::Offline { pending: 3 });
        assert_eq!(
            push_state(PushState::Reconnecting { attempt: 2 }),
            ui::PushState::Connecting
        );
        assert_eq!(push_state(PushState::Polling), ui::PushState::Off);
        assert_eq!(push_state(PushState::Connected), ui::PushState::Connected);
    }

    #[test]
    fn draft_status_adds_held_copies_and_keeps_refusals_per_draft() {
        let mut s = SyncStatus::default();
        assert_eq!(draft_sync(&s, [].iter()), ui::DraftSync::default());
        s.online = true;
        s.drafts_waiting.insert("d1".into());
        s.drafts_refused.insert("d1".into(), "too big".into());
        s.last_error = Some("the network is down".into());
        let held = [ui::DraftId::from("d2")];
        let d = draft_sync(&s, held.iter());
        let ids: Vec<&str> = d.waiting.iter().map(ui::DraftId::as_str).collect();
        assert_eq!(ids, ["d1", "d2"]);
        assert!(d.online && !d.local_only);
        assert_eq!(
            d.refused.get(&ui::DraftId::from("d1")).map(String::as_str),
            Some("too big")
        );
        assert!(!d.refused.contains_key(&ui::DraftId::from("d2")));
        s.drafts_local_only = true;
        assert!(draft_sync(&s, [].iter()).local_only);
    }

    #[test]
    fn data_change_scopes() {
        let mut c = DataChanges::default();
        c.threads.insert(Id::from("T1"));
        c.mailbox_lists.insert(Id::from("M1"));
        c.outbox = true;
        let d = data_change(&c);
        assert!(d.mailboxes && d.drafts && !d.all);
        assert_eq!(d.threads, vec![ui::ThreadId::new("T1")]);
        let everything = data_change(&DataChanges {
            everything: true,
            ..Default::default()
        });
        assert!(everything.all && everything.masked);
    }

    #[test]
    fn html_only_bodies_are_html_whatever_list_they_are_in() {
        let part = |mime: &str, value: &str| mejl_store::BodyPart {
            part_id: None,
            blob_id: None,
            mime_type: Some(mime.into()),
            charset: None,
            value: value.into(),
            is_truncated: false,
            is_encoding_problem: false,
        };
        let detail = |text: Vec<mejl_store::BodyPart>, html: Vec<mejl_store::BodyPart>| {
            let row = mejl_store::MessageRow {
                email_id: Id::from("E1"),
                thread_id: Id::from("T1"),
                received_at: ts(),
                subject: String::new(),
                preview: String::new(),
                from: vec![],
                to: vec![],
                cc: vec![],
                size: 0,
                seen: true,
                flagged: false,
                answered: false,
                draft: false,
                has_attachment: false,
                masked: false,
                masked_email_id: None,
            };
            EmailDetail {
                headers: MessageHeaders {
                    row,
                    bcc: vec![],
                    reply_to: vec![],
                    sender: vec![],
                    sent_at: None,
                    message_id: vec![],
                    in_reply_to: vec![],
                    references: vec![],
                    masked_address: None,
                    keywords: Default::default(),
                    mailbox_ids: Default::default(),
                    body_cached: true,
                },
                body: Some(mejl_store::Body {
                    text_parts: text,
                    html_parts: html,
                    attachments: vec![],
                    headers: vec![],
                    fetched_at: ts(),
                    opened_at: None,
                }),
            }
        };
        let html_only = detail(
            vec![part("text/html", "<p>Hi</p>")],
            vec![part("text/html", "<p>Hi</p>")],
        );
        let v = body_view("E1", &html_only).unwrap();
        assert_eq!(v.text, None);
        assert_eq!(v.html.as_deref(), Some("<p>Hi</p>"));
        let plain_only = detail(
            vec![part("text/plain", "Hi")],
            vec![part("text/plain", "Hi")],
        );
        let v = body_view("E1", &plain_only).unwrap();
        assert_eq!(v.text.as_deref(), Some("Hi"));
        assert_eq!(v.html, None);
        let both = detail(
            vec![part("text/plain", "Hi")],
            vec![part("text/html", "<b>Hi</b>")],
        );
        let v = body_view("E1", &both).unwrap();
        assert_eq!(v.text.as_deref(), Some("Hi"));
        assert_eq!(v.html.as_deref(), Some("<b>Hi</b>"));
    }

    #[test]
    fn mask_sites_are_hosts_in_the_ui_and_origins_on_the_wire() {
        assert_eq!(site_host("https://shop.example.com"), "shop.example.com");
        assert_eq!(site_host("http://shop.example.com/"), "shop.example.com");
        assert_eq!(site_host("shop.example.com"), "shop.example.com");
        assert_eq!(
            site_origin("cabins.example.net"),
            "https://cabins.example.net"
        );
        assert_eq!(
            site_origin("http://cabins.example.net/"),
            "http://cabins.example.net"
        );
        assert_eq!(site_origin(""), "");
    }

    #[test]
    fn masked_states() {
        assert_eq!(masked_state(None), ui::MaskedState::Enabled);
        assert_eq!(
            masked_state(Some(&MaskedEmailState::Deleted)),
            ui::MaskedState::Deleted
        );
        assert_eq!(masked_state_for_sync(ui::MaskedState::Pending), None);
    }
}
