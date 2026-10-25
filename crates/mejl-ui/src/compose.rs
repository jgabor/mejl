//! Compose: the header form, the body editor, quoting and drafts.
//!
//! The headers are a form mejl draws itself, because address completion
//! and the From picker (identities and Masked Email addresses) need mejl's
//! data. The body is mejl's own editor (see `editor.rs`), with the
//! Emacs-like keys of `ratatui-textarea`, soft wrap by display width and
//! its own undo, at a cost per key that does not grow with the body.
//! `Ctrl-o e` hands the body to `$EDITOR` through
//! [`Cmd::OpenEditor`](crate::Cmd::OpenEditor).
//!
//! Drafts save themselves: every edit restarts a short timer
//! ([`AUTOSAVE_DELAY`]), the UI asks the shell to wake it with
//! [`Cmd::WakeAt`](crate::Cmd::WakeAt), and on the wake-up a
//! [`Cmd::SaveDraft`](crate::Cmd::SaveDraft) goes out. Leaving compose
//! saves too, so text is never lost by closing the screen.

mod editor;

use jiff::{SignedDuration, Timestamp, Zoned};
use ratatui::style::{Modifier, Style};

use self::editor::{Editor, Move};
use crate::address;
use crate::data::{
    BodyView, ComposeKind, DraftId, EmailId, IdentityView, MessageSummary, OutgoingAttachment,
    OutgoingMessage, Participant,
};
use crate::input::{InputResult, LineInput};
use crate::keymap::{Key, KeyName};

/// Idle time after the last edit before the draft saves itself.
pub const AUTOSAVE_DELAY: SignedDuration = SignedDuration::from_secs(2);
/// Most completion suggestions shown under an address field.
pub(crate) const SUGGESTIONS: usize = 6;

/// Undo and redo in the body, for the compose help. Ctrl-z is not here:
/// it suspends mejl in every context (see `keymap/preset.rs`), so the
/// body's Ctrl-z undo only runs when `suspend` is unbound.
pub(crate) const BODY_UNDO_HELP: [(&str, &str); 2] = [
    ("^_, ^/, ^u", "Undo the last edit in the body"),
    ("^y, ^r", "Redo the last undone edit in the body"),
];

/// Whether `key` undoes in the body: Ctrl-_ or Ctrl-/ (the Emacs and
/// readline undo), or Ctrl-z when nothing else takes it. Ctrl-u, the
/// editor's own undo, is handled by the editor.
///
/// In legacy encoding Ctrl-_ and Ctrl-/ are both byte 0x1F, which
/// crossterm reports as Ctrl-7. With the kitty protocol and alternate keys
/// they arrive as Ctrl-_ and Ctrl-/. All three are accepted.
fn is_body_undo(key: &Key) -> bool {
    key.ctrl && !key.alt && matches!(key.name, KeyName::Char('_' | '/' | '7' | 'z'))
}

/// A compose form field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    /// From, picked from identities and masked addresses.
    From,
    /// To recipients.
    To,
    /// Cc recipients.
    Cc,
    /// Bcc recipients, shown on request.
    Bcc,
    /// The subject line.
    Subject,
    /// The body editor.
    Body,
    /// The attachment row.
    Attachments,
}

impl Field {
    /// The label in the form.
    pub fn label(self) -> &'static str {
        match self {
            Field::From => "From",
            Field::To => "To",
            Field::Cc => "Cc",
            Field::Bcc => "Bcc",
            Field::Subject => "Subject",
            Field::Body => "Body",
            Field::Attachments => "Attach",
        }
    }
}

/// An address field: entered recipients as chips plus the text being
/// typed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AddressField {
    /// Checked recipients.
    pub chips: Vec<Participant>,
    /// Text not yet turned into a chip.
    pub input: LineInput,
    /// Why the typed text was refused.
    pub error: Option<String>,
}

impl AddressField {
    fn with(chips: Vec<Participant>) -> AddressField {
        AddressField {
            chips,
            ..AddressField::default()
        }
    }

    fn push(&mut self, p: Participant) {
        if !self
            .chips
            .iter()
            .any(|c| c.email.eq_ignore_ascii_case(&p.email))
        {
            self.chips.push(p);
        }
    }

    /// Turns the typed text into chips. Addresses that do not parse stay
    /// in the input with an error. Returns false when something was
    /// refused.
    pub fn commit(&mut self) -> bool {
        let text = self.input.take();
        self.add_text(&text)
    }

    /// Adds pasted or typed text, which may hold several addresses.
    pub fn add_text(&mut self, text: &str) -> bool {
        let mut bad = Vec::new();
        let mut reason = None;
        for part in address::split(text) {
            match address::parse(&part) {
                Ok(p) => self.push(p),
                Err(e) => {
                    reason.get_or_insert_with(|| format!("{part}: {e}"));
                    bad.push(part);
                }
            }
        }
        self.error = reason;
        if !bad.is_empty() {
            let mut keep = bad.join(", ");
            if !self.input.is_empty() {
                keep = format!("{keep}, {}", self.input.text());
            }
            self.input.set(keep);
            return false;
        }
        true
    }

    /// True when nothing is entered or typed.
    pub fn is_empty(&self) -> bool {
        self.chips.is_empty() && self.input.text().trim().is_empty()
    }
}

/// What a key did in compose, for the caller to act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum KeyOutcome {
    /// The key was used; the draft may be dirty.
    Used {
        /// The text changed.
        changed: bool,
    },
    /// Open the From picker.
    PickFrom,
    /// Ask for a file to attach.
    AttachPrompt,
    /// Let the keymap have the key.
    Pass,
}

/// The compose screen.
#[derive(Clone, Debug)]
pub struct ComposeState {
    pub(crate) draft_id: DraftId,
    pub(crate) kind: ComposeKind,
    pub(crate) identity_id: Option<String>,
    pub(crate) from: Participant,
    pub(crate) to: AddressField,
    pub(crate) cc: AddressField,
    pub(crate) bcc: AddressField,
    pub(crate) show_bcc: bool,
    pub(crate) subject: LineInput,
    pub(crate) body: Editor,
    pub(crate) attachments: Vec<OutgoingAttachment>,
    pub(crate) uploading: Vec<String>,
    pub(crate) attach_cursor: usize,
    pub(crate) in_reply_to: Vec<String>,
    pub(crate) references: Vec<String>,
    pub(crate) original: Option<EmailId>,
    pub(crate) field: Field,
    pub(crate) editing: bool,
    pub(crate) dirty: bool,
    pub(crate) last_edit: Option<Timestamp>,
    pub(crate) saved_at: Option<Timestamp>,
    pub(crate) save_error: Option<String>,
    pub(crate) saving: bool,
    pub(crate) ever_saved: bool,
    pub(crate) suggest: Vec<Participant>,
    pub(crate) suggest_cursor: usize,
    pub(crate) quote_wait: Option<(EmailId, QuoteKind)>,
    /// The body as compose opened it, so an untouched message counts as
    /// blank.
    pub(crate) initial_body: String,
}

/// How a message is quoted into a reply or forward.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum QuoteKind {
    /// `On <date>, <name> wrote:` and `> ` lines.
    Reply {
        /// The attribution line.
        attribution: String,
    },
    /// A forwarded message header block and the text unquoted.
    Forward {
        /// The header block.
        header: String,
    },
}

fn new_body(text: &str) -> Editor {
    Editor::new(text)
}

/// Removes characters that must not reach the body: control characters
/// other than line breaks and tabs, and terminal escape sequences, which
/// pasted text can carry.
pub(crate) fn clean_text(s: &str) -> String {
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Skip a CSI or OSC sequence.
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    for d in chars.by_ref() {
                        if ('@'..='~').contains(&d) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(d) = chars.next() {
                        if d == '\u{7}' {
                            break;
                        }
                        if d == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        if c == '\n' || c == '\t' || !c.is_control() {
            out.push(c);
        }
    }
    out
}

/// Adds `Re: ` or `Fwd: ` unless the subject already starts with it.
pub(crate) fn prefixed_subject(subject: &str, prefix: &str) -> String {
    let s = subject.trim();
    let lower = s.to_lowercase();
    let known: &[&str] = if prefix == "Re:" {
        &["re:", "aw:", "sv:"]
    } else {
        &["fwd:", "fw:"]
    };
    if known.iter().any(|k| lower.starts_with(k)) {
        s.to_owned()
    } else if s.is_empty() {
        prefix.to_owned()
    } else {
        format!("{prefix} {s}")
    }
}

/// The plain text of a body for quoting: the text part, or the HTML part
/// rendered to plain lines.
pub(crate) fn plain_text(body: &BodyView) -> String {
    if let Some(t) = &body.text
        && !t.trim().is_empty()
    {
        return t.replace("\r\n", "\n");
    }
    let options = crate::body::render_options(72, false);
    let rendered = mejl_render::render_auto(body.text.as_deref(), body.html.as_deref(), &options);
    rendered
        .lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.text.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Quotes text with `> `, keeping deeper quotes as `>>`.
pub(crate) fn quote(text: &str) -> String {
    let trimmed = text.trim_end();
    trimmed
        .lines()
        .map(|l| {
            let l = l.trim_end();
            if l.is_empty() {
                ">".to_owned()
            } else if l.starts_with('>') {
                format!(">{l}")
            } else {
                format!("> {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn signature_block(identity: Option<&IdentityView>) -> String {
    match identity {
        Some(i) if !i.signature.trim().is_empty() => {
            format!("-- \n{}\n", clean_text(i.signature.trim_end()))
        }
        _ => String::new(),
    }
}

fn date_line(ts: Timestamp, now: &Zoned) -> String {
    ts.to_zoned(now.time_zone().clone())
        .strftime("%a, %-d %b %Y at %H:%M")
        .to_string()
}

fn people(ps: &[Participant]) -> String {
    ps.iter()
        .map(address::format)
        .collect::<Vec<_>>()
        .join(", ")
}

/// What a new compose screen starts from.
pub(crate) struct Start<'a> {
    pub(crate) draft_id: DraftId,
    pub(crate) kind: ComposeKind,
    pub(crate) identities: &'a [IdentityView],
    /// The account address, used when there is no identity.
    pub(crate) account: Participant,
    /// The message replied to or forwarded.
    pub(crate) original: Option<&'a MessageSummary>,
    /// Its subject.
    pub(crate) subject: &'a str,
    /// Its body, if cached.
    pub(crate) body: Option<&'a BodyView>,
    /// Addresses that are the user's own (identities and masked).
    pub(crate) mine: &'a dyn Fn(&str) -> bool,
    /// A masked address among the original's recipients, if any.
    pub(crate) masked_to: Option<Participant>,
    pub(crate) now: &'a Zoned,
}

impl ComposeState {
    /// A compose screen for a new message, reply or forward.
    pub(crate) fn start(s: &Start<'_>) -> ComposeState {
        let default = s.identities.first();
        let mut identity = default;
        let mut from = default
            .map(|i| Participant {
                name: (!i.name.trim().is_empty()).then(|| i.name.clone()),
                email: i.email.clone(),
            })
            .unwrap_or_else(|| s.account.clone());
        let mut to = Vec::new();
        let mut cc = Vec::new();
        let mut subject = String::new();
        let mut in_reply_to = Vec::new();
        let mut references = Vec::new();
        let mut quote_kind = None;
        let mut attachments = Vec::new();
        if let Some(m) = s.original {
            // Answer from the address the message was sent to.
            let recipients: Vec<&Participant> = m.to.iter().chain(&m.cc).collect();
            if let Some(i) = s.identities.iter().find(|i| {
                recipients
                    .iter()
                    .any(|p| p.email.eq_ignore_ascii_case(&i.email))
            }) {
                identity = Some(i);
                from = Participant {
                    name: (!i.name.trim().is_empty()).then(|| i.name.clone()),
                    email: i.email.clone(),
                };
            } else if let Some(mask) = &s.masked_to {
                from = Participant {
                    name: from.name.clone(),
                    email: mask.email.clone(),
                };
            }
            let me =
                |p: &Participant| (s.mine)(&p.email) || p.email.eq_ignore_ascii_case(&from.email);
            let sent_by_me = me(&m.from);
            let sender: Vec<Participant> = if !m.reply_to.is_empty() {
                m.reply_to.clone()
            } else {
                vec![m.from.clone()]
            };
            match s.kind {
                ComposeKind::Reply => {
                    to = if sent_by_me { m.to.clone() } else { sender };
                }
                ComposeKind::ReplyAll => {
                    if sent_by_me {
                        to = m.to.clone();
                    } else {
                        to = sender;
                        to.extend(m.to.iter().filter(|p| !me(p)).cloned());
                    }
                    cc = m.cc.iter().filter(|p| !me(p)).cloned().collect();
                }
                ComposeKind::Forward | ComposeKind::New => {}
            }
            let mut seen = std::collections::HashSet::new();
            to.retain(|p| seen.insert(p.email.to_lowercase()));
            cc.retain(|p| seen.insert(p.email.to_lowercase()));
            match s.kind {
                ComposeKind::Forward => {
                    subject = prefixed_subject(s.subject, "Fwd:");
                    let mut header = String::from("---------- Forwarded message ----------\n");
                    header.push_str(&format!("From: {}\n", address::format(&m.from)));
                    header.push_str(&format!("Date: {}\n", date_line(m.date, s.now)));
                    header.push_str(&format!("Subject: {}\n", s.subject.trim()));
                    if !m.to.is_empty() {
                        header.push_str(&format!("To: {}\n", people(&m.to)));
                    }
                    if !m.cc.is_empty() {
                        header.push_str(&format!("Cc: {}\n", people(&m.cc)));
                    }
                    quote_kind = Some(QuoteKind::Forward { header });
                    if let Some(b) = s.body {
                        attachments = b
                            .attachments
                            .iter()
                            .map(|a| OutgoingAttachment {
                                blob_id: a.blob_id.clone(),
                                name: a.name.clone(),
                                mime_type: a.mime_type.clone(),
                                size: a.size,
                            })
                            .collect();
                    }
                }
                _ => {
                    subject = prefixed_subject(s.subject, "Re:");
                    quote_kind = Some(QuoteKind::Reply {
                        attribution: format!(
                            "On {}, {} wrote:",
                            date_line(m.date, s.now),
                            m.from.display()
                        ),
                    });
                    references = m.references.clone();
                    if let Some(id) = &m.message_id {
                        in_reply_to.push(id.clone());
                        if !references.contains(id) {
                            references.push(id.clone());
                        }
                    }
                }
            }
        }
        let mut text = String::new();
        let sig = signature_block(identity);
        if s.kind != ComposeKind::New || !sig.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&sig);
        let mut quote_wait = None;
        if let (Some(m), Some(q)) = (s.original, quote_kind) {
            match s.body {
                Some(b) => {
                    if !sig.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&quoted(&q, &plain_text(b)));
                }
                None => quote_wait = Some((m.email_id.clone(), q)),
            }
        }
        let first = match s.kind {
            ComposeKind::New | ComposeKind::Forward => Field::To,
            ComposeKind::Reply | ComposeKind::ReplyAll => Field::Body,
        };
        let mut body = new_body(&text);
        body.move_cursor(Move::Top);
        body.move_cursor(Move::Head);
        let mut c = ComposeState {
            draft_id: s.draft_id.clone(),
            kind: s.kind,
            identity_id: identity.map(|i| i.id.clone()),
            from,
            to: AddressField::with(to),
            cc: AddressField::with(cc.clone()),
            bcc: AddressField::default(),
            show_bcc: false,
            subject: LineInput::new(subject),
            body,
            attachments,
            uploading: Vec::new(),
            attach_cursor: 0,
            in_reply_to,
            references,
            original: s.original.map(|m| m.email_id.clone()),
            field: first,
            editing: true,
            dirty: false,
            last_edit: None,
            saved_at: None,
            save_error: None,
            saving: false,
            ever_saved: false,
            suggest: Vec::new(),
            suggest_cursor: 0,
            quote_wait,
            initial_body: text.clone(),
        };
        if !cc.is_empty() && c.field == Field::Body {
            c.field = Field::Body;
        }
        c
    }

    /// Reopens a saved or cancelled message.
    pub(crate) fn resume(m: &OutgoingMessage, saved: bool) -> ComposeState {
        let mut body = new_body(&m.text_body);
        body.move_cursor(Move::Top);
        ComposeState {
            draft_id: m.draft_id.clone(),
            kind: m.kind,
            identity_id: m.identity_id.clone(),
            from: m.from.clone(),
            to: AddressField::with(m.to.clone()),
            cc: AddressField::with(m.cc.clone()),
            show_bcc: !m.bcc.is_empty(),
            bcc: AddressField::with(m.bcc.clone()),
            subject: LineInput::new(m.subject.clone()),
            body,
            attachments: m.attachments.clone(),
            uploading: Vec::new(),
            attach_cursor: 0,
            in_reply_to: m.in_reply_to.clone(),
            references: m.references.clone(),
            original: m.original.clone(),
            field: if m.to.is_empty() {
                Field::To
            } else {
                Field::Body
            },
            editing: true,
            dirty: false,
            last_edit: None,
            saved_at: None,
            save_error: None,
            saving: false,
            ever_saved: saved,
            suggest: Vec::new(),
            suggest_cursor: 0,
            quote_wait: None,
            // A resumed draft is never "blank" because of its own body;
            // only emptying it makes it so.
            initial_body: String::new(),
        }
    }

    /// Adds the quote once the original body arrives.
    pub(crate) fn fill_quote(&mut self, body: &BodyView) -> bool {
        let Some((id, q)) = self.quote_wait.take() else {
            return false;
        };
        if id != body.email_id {
            self.quote_wait = Some((id, q));
            return false;
        }
        let text = quoted(&q, &plain_text(body));
        let cursor = self.body.cursor();
        self.body.move_cursor(Move::Bottom);
        self.body.move_cursor(Move::End);
        let last_empty = self.body.lines().last().is_none_or(|l| l.is_empty());
        if !last_empty {
            self.body.insert_newline();
        }
        self.body.insert_str(&text);
        self.body.move_cursor(Move::Jump(cursor.0, cursor.1));
        if self.kind == ComposeKind::Forward && self.attachments.is_empty() {
            self.attachments = body
                .attachments
                .iter()
                .map(|a| OutgoingAttachment {
                    blob_id: a.blob_id.clone(),
                    name: a.name.clone(),
                    mime_type: a.mime_type.clone(),
                    size: a.size,
                })
                .collect();
        }
        true
    }

    /// The fields in tab order.
    pub(crate) fn fields(&self) -> Vec<Field> {
        let mut v = vec![Field::From, Field::To, Field::Cc];
        if self.show_bcc {
            v.push(Field::Bcc);
        }
        v.extend([Field::Subject, Field::Body, Field::Attachments]);
        v
    }

    pub(crate) fn address_mut(&mut self, f: Field) -> Option<&mut AddressField> {
        match f {
            Field::To => Some(&mut self.to),
            Field::Cc => Some(&mut self.cc),
            Field::Bcc => Some(&mut self.bcc),
            _ => None,
        }
    }

    pub(crate) fn address(&self, f: Field) -> Option<&AddressField> {
        match f {
            Field::To => Some(&self.to),
            Field::Cc => Some(&self.cc),
            Field::Bcc => Some(&self.bcc),
            _ => None,
        }
    }

    /// Commits typed addresses in every field. False when one was refused.
    pub(crate) fn commit_all(&mut self) -> bool {
        let a = self.to.commit();
        let b = self.cc.commit();
        let c = self.bcc.commit();
        a && b && c
    }

    /// Moves to the next (or previous) field and starts editing it.
    pub(crate) fn step_field(&mut self, forward: bool) {
        if let Some(a) = self.address_mut(self.field) {
            a.commit();
        }
        self.suggest.clear();
        let fields = self.fields();
        let i = fields.iter().position(|f| *f == self.field).unwrap_or(0);
        let next = if forward {
            (i + 1) % fields.len()
        } else {
            (i + fields.len() - 1) % fields.len()
        };
        self.field = fields[next];
        self.editing = true;
    }

    /// Focuses a field by its index in [`ComposeState::fields`].
    pub(crate) fn focus_index(&mut self, index: usize) {
        if let Some(a) = self.address_mut(self.field) {
            a.commit();
        }
        self.suggest.clear();
        if let Some(f) = self.fields().get(index) {
            self.field = *f;
            self.editing = true;
        }
    }

    /// Shows or hides Bcc. Hiding keeps nothing typed there hidden: a
    /// Bcc with recipients stays visible.
    pub(crate) fn toggle_bcc(&mut self) {
        if self.show_bcc && self.bcc.is_empty() {
            self.show_bcc = false;
            if self.field == Field::Bcc {
                self.field = Field::Subject;
            }
        } else {
            self.show_bcc = true;
            self.field = Field::Bcc;
            self.editing = true;
        }
    }

    /// Records an edit for autosave.
    pub(crate) fn touch(&mut self, now: Timestamp) -> Timestamp {
        self.dirty = true;
        self.last_edit = Some(now);
        now.checked_add(AUTOSAVE_DELAY).unwrap_or(now)
    }

    /// When the next autosave is due, if the draft has unsaved edits.
    pub fn autosave_at(&self) -> Option<Timestamp> {
        if !self.dirty || self.saving {
            return None;
        }
        self.last_edit
            .map(|t| t.checked_add(AUTOSAVE_DELAY).unwrap_or(t))
    }

    /// The body as text.
    pub fn body_text(&self) -> String {
        self.body.lines().join("\n")
    }

    /// Words in the body, for the status line.
    pub(crate) fn words(&self) -> usize {
        // The editor keeps a count per line, so this is a walk over the
        // lines without splitting any text.
        self.body
            .lines()
            .iter()
            .zip(self.body.line_words())
            .take_while(|(l, _)| l.as_str() != "-- ")
            .filter(|(l, _)| !l.starts_with('>'))
            .map(|(_, n)| *n as usize)
            .sum()
    }

    /// True when nothing was entered.
    pub(crate) fn is_blank(&self) -> bool {
        self.to.is_empty()
            && self.cc.is_empty()
            && self.bcc.is_empty()
            && self.subject.text().trim().is_empty()
            && (self.body_text() == self.initial_body
                || self.body.lines().iter().all(|l| l.trim().is_empty()))
            && self.attachments.is_empty()
    }

    /// The message as it stands, for saving or sending. Typed text that is
    /// not a chip yet is included when it parses.
    pub(crate) fn message(&self) -> OutgoingMessage {
        let field = |a: &AddressField| {
            let mut v = a.chips.clone();
            for part in address::split(a.input.text()) {
                if let Ok(p) = address::parse(&part)
                    && !v.iter().any(|c| c.email.eq_ignore_ascii_case(&p.email))
                {
                    v.push(p);
                }
            }
            v
        };
        OutgoingMessage {
            draft_id: self.draft_id.clone(),
            kind: self.kind,
            identity_id: self.identity_id.clone(),
            from: self.from.clone(),
            to: field(&self.to),
            cc: field(&self.cc),
            bcc: field(&self.bcc),
            subject: self.subject.text().trim().to_owned(),
            text_body: self.body_text(),
            in_reply_to: self.in_reply_to.clone(),
            references: self.references.clone(),
            original: self.original.clone(),
            attachments: self.attachments.clone(),
        }
    }

    /// True when the body mentions an attachment but none is attached.
    pub(crate) fn missing_attachment(&self) -> bool {
        if !self.attachments.is_empty() || !self.uploading.is_empty() {
            return false;
        }
        self.body
            .lines()
            .iter()
            .filter(|l| !l.starts_with('>'))
            .any(|l| {
                let l = l.to_lowercase();
                l.contains("attached") || l.contains("attachment") || l.contains("enclosed")
            })
    }

    /// Sets the body editor's cursor style for the editing state.
    pub(crate) fn sync_cursor(&mut self) {
        let style = if self.editing && self.field == Field::Body {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        self.body.set_cursor_style(style);
    }

    /// Replaces the body (after `$EDITOR`), keeping undo history.
    pub(crate) fn replace_body(&mut self, text: &str) {
        self.body.select_all();
        self.body.cut();
        self.body.insert_str(&clean_text(text));
        self.body.move_cursor(Move::Top);
    }

    /// Pastes text into the focused field.
    pub(crate) fn paste(&mut self, text: &str) -> bool {
        match self.field {
            Field::To | Field::Cc | Field::Bcc => {
                let f = self.field;
                if let Some(a) = self.address_mut(f) {
                    let typed = a.input.take();
                    a.add_text(&format!("{typed}, {text}"));
                }
                true
            }
            Field::Subject => {
                self.subject.insert_str(text);
                true
            }
            Field::Body => {
                self.editing = true;
                self.body.insert_str(&clean_text(text))
            }
            Field::From | Field::Attachments => false,
        }
    }

    /// Handles a key while a field is focused. Keys the keymap reserves
    /// (Esc, Tab, `Ctrl-o`, `Ctrl-s`) never reach here.
    pub(crate) fn key(&mut self, key: &Key) -> KeyOutcome {
        if !self.editing {
            return match key.name {
                KeyName::Up | KeyName::Char('k') if !key.ctrl => {
                    self.step_field(false);
                    self.editing = false;
                    KeyOutcome::Used { changed: false }
                }
                KeyName::Down | KeyName::Char('j') if !key.ctrl => {
                    self.step_field(true);
                    self.editing = false;
                    KeyOutcome::Used { changed: false }
                }
                KeyName::Enter | KeyName::Char('i') if !key.ctrl => {
                    self.editing = true;
                    match self.field {
                        Field::From => KeyOutcome::PickFrom,
                        _ => KeyOutcome::Used { changed: false },
                    }
                }
                _ => KeyOutcome::Pass,
            };
        }
        match self.field {
            Field::From => match key.name {
                KeyName::Enter | KeyName::Char(' ') => KeyOutcome::PickFrom,
                KeyName::Down => {
                    self.step_field(true);
                    KeyOutcome::Used { changed: false }
                }
                _ => KeyOutcome::Used { changed: false },
            },
            Field::To | Field::Cc | Field::Bcc => self.address_key(key),
            Field::Subject => match key.name {
                KeyName::Enter | KeyName::Down => {
                    self.step_field(true);
                    KeyOutcome::Used { changed: false }
                }
                KeyName::Up => {
                    self.step_field(false);
                    KeyOutcome::Used { changed: false }
                }
                _ => KeyOutcome::Used {
                    changed: self.subject.handle(key) == InputResult::Changed,
                },
            },
            Field::Body => {
                if is_body_undo(key) {
                    return KeyOutcome::Used {
                        changed: self.body.undo(),
                    };
                }
                if key.ctrl && key.name == KeyName::Char('y') {
                    return KeyOutcome::Used {
                        changed: self.body.redo(),
                    };
                }
                KeyOutcome::Used {
                    changed: self.body.input(key),
                }
            }
            Field::Attachments => match key.name {
                KeyName::Left => {
                    self.attach_cursor = self.attach_cursor.saturating_sub(1);
                    KeyOutcome::Used { changed: false }
                }
                KeyName::Right => {
                    self.attach_cursor =
                        (self.attach_cursor + 1).min(self.attachments.len().saturating_sub(1));
                    KeyOutcome::Used { changed: false }
                }
                KeyName::Delete | KeyName::Backspace => {
                    if self.attach_cursor < self.attachments.len() {
                        self.attachments.remove(self.attach_cursor);
                        self.attach_cursor = self
                            .attach_cursor
                            .min(self.attachments.len().saturating_sub(1));
                        KeyOutcome::Used { changed: true }
                    } else {
                        KeyOutcome::Used { changed: false }
                    }
                }
                KeyName::Enter | KeyName::Char('a') => KeyOutcome::AttachPrompt,
                KeyName::Up => {
                    self.step_field(false);
                    KeyOutcome::Used { changed: false }
                }
                _ => KeyOutcome::Used { changed: false },
            },
        }
    }

    fn address_key(&mut self, key: &Key) -> KeyOutcome {
        let field = self.field;
        let has_suggestions = !self.suggest.is_empty();
        match key.name {
            KeyName::Down if has_suggestions => {
                self.suggest_cursor = (self.suggest_cursor + 1) % self.suggest.len();
                return KeyOutcome::Used { changed: false };
            }
            KeyName::Up if has_suggestions => {
                self.suggest_cursor =
                    (self.suggest_cursor + self.suggest.len() - 1) % self.suggest.len();
                return KeyOutcome::Used { changed: false };
            }
            KeyName::Enter if has_suggestions => {
                self.accept_suggestion();
                return KeyOutcome::Used { changed: true };
            }
            KeyName::Down => {
                self.step_field(true);
                return KeyOutcome::Used { changed: false };
            }
            KeyName::Up => {
                self.step_field(false);
                return KeyOutcome::Used { changed: false };
            }
            _ => {}
        }
        let Some(a) = self.address_mut(field) else {
            return KeyOutcome::Pass;
        };
        match key.name {
            KeyName::Enter => {
                let changed = !a.input.is_empty();
                if a.input.is_empty() {
                    self.step_field(true);
                } else {
                    a.commit();
                }
                KeyOutcome::Used { changed }
            }
            KeyName::Char(',' | ';') if !key.ctrl && !key.alt => {
                a.commit();
                KeyOutcome::Used { changed: true }
            }
            KeyName::Backspace if a.input.is_empty() => {
                let changed = a.chips.pop().is_some();
                a.error = None;
                KeyOutcome::Used { changed }
            }
            _ => {
                let changed = a.input.handle(key) == InputResult::Changed;
                if changed {
                    a.error = None;
                }
                KeyOutcome::Used { changed }
            }
        }
    }

    /// The word being typed in the focused address field, for completion.
    pub(crate) fn completion_prefix(&self) -> Option<String> {
        let a = self.address(self.field)?;
        let t = a.input.text().trim();
        (t.chars().count() >= 1).then(|| t.to_owned())
    }

    /// Takes the highlighted suggestion as a chip.
    pub(crate) fn accept_suggestion(&mut self) {
        let Some(p) = self.suggest.get(self.suggest_cursor).cloned() else {
            return;
        };
        let field = self.field;
        if let Some(a) = self.address_mut(field) {
            a.input.clear();
            a.error = None;
            a.push(p);
        }
        self.suggest.clear();
        self.suggest_cursor = 0;
    }
}

fn quoted(q: &QuoteKind, text: &str) -> String {
    match q {
        QuoteKind::Reply { attribution } => format!("{attribution}\n{}\n", quote(text)),
        QuoteKind::Forward { header } => format!("{header}\n{}\n", text.trim_end()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Zoned {
        "2026-10-23T14:02:31Z"
            .parse::<Timestamp>()
            .unwrap()
            .to_zoned(jiff::tz::TimeZone::UTC)
    }

    fn ids() -> Vec<IdentityView> {
        vec![
            IdentityView {
                id: "i1".into(),
                name: "Mira Holt".into(),
                email: "mira@example.org".into(),
                signature: "Mira".into(),
            },
            IdentityView {
                id: "i3".into(),
                name: "Mira at work".into(),
                email: "mira.holt@work.example.com".into(),
                signature: String::new(),
            },
        ]
    }

    fn original() -> MessageSummary {
        MessageSummary {
            email_id: "e1".into(),
            from: Participant::new("Juniper Tran", "juniper@garden.example.com"),
            to: vec![
                Participant::new("Mira at work", "mira.holt@work.example.com"),
                Participant::new("Owen Castillo", "owen@example.net"),
            ],
            cc: vec![Participant::new("Lena Okafor", "lena@example.net")],
            date: "2026-10-23T14:02:00Z".parse().unwrap(),
            preview: String::new(),
            unread: false,
            flagged: false,
            has_attachment: false,
            mailboxes: Vec::new(),
            reply_to: Vec::new(),
            message_id: Some("m1@garden.example.com".into()),
            references: vec!["m0@garden.example.com".into()],
        }
    }

    fn body() -> BodyView {
        BodyView {
            email_id: "e1".into(),
            text: Some("Hi all,\n\n> earlier\nSee you.\n".into()),
            html: None,
            attachments: Vec::new(),
            headers: Vec::new(),
        }
    }

    fn start(kind: ComposeKind, body: Option<&BodyView>) -> ComposeState {
        let ids = ids();
        let m = original();
        let mine = |e: &str| ids.iter().any(|i| i.email.eq_ignore_ascii_case(e));
        let now = now();
        ComposeState::start(&Start {
            draft_id: "d1".into(),
            kind,
            identities: &ids,
            account: Participant::new("Mira Holt", "mira@example.org"),
            original: (kind != ComposeKind::New).then_some(&m),
            subject: "Garden club plot rota",
            body,
            mine: &mine,
            masked_to: None,
            now: &now,
        })
    }

    #[test]
    fn reply_quotes_and_threads() {
        let b = body();
        let c = start(ComposeKind::Reply, Some(&b));
        assert_eq!(c.subject.text(), "Re: Garden club plot rota");
        assert_eq!(
            c.from.email, "mira.holt@work.example.com",
            "answers from the address used"
        );
        assert_eq!(c.identity_id.as_deref(), Some("i3"));
        assert_eq!(c.to.chips.len(), 1);
        assert_eq!(c.to.chips[0].email, "juniper@garden.example.com");
        assert!(c.cc.chips.is_empty());
        assert_eq!(c.in_reply_to, vec!["m1@garden.example.com"]);
        assert_eq!(
            c.references,
            vec!["m0@garden.example.com", "m1@garden.example.com"]
        );
        let text = c.body_text();
        assert!(text.starts_with("\n\nOn Fri, 23 Oct 2026 at 14:02, Juniper Tran wrote:\n> Hi all,\n>\n>> earlier\n> See you."), "{text:?}");
        assert_eq!(c.field, Field::Body);
        assert_eq!(c.body.cursor(), (0, 0));
    }

    #[test]
    fn reply_all_drops_me_and_keeps_everyone_else() {
        let c = start(ComposeKind::ReplyAll, Some(&body()));
        let to: Vec<&str> = c.to.chips.iter().map(|p| p.email.as_str()).collect();
        assert_eq!(to, ["juniper@garden.example.com", "owen@example.net"]);
        let cc: Vec<&str> = c.cc.chips.iter().map(|p| p.email.as_str()).collect();
        assert_eq!(cc, ["lena@example.net"]);
    }

    #[test]
    fn forward_and_waiting_for_the_body() {
        let mut c = start(ComposeKind::Forward, None);
        assert_eq!(c.subject.text(), "Fwd: Garden club plot rota");
        assert!(c.to.chips.is_empty());
        assert!(c.quote_wait.is_some());
        assert!(!c.fill_quote(&BodyView {
            email_id: "other".into(),
            ..body()
        }));
        assert!(c.fill_quote(&body()));
        let text = c.body_text();
        assert!(text.contains("---------- Forwarded message ----------\nFrom: Juniper Tran <juniper@garden.example.com>"), "{text}");
        assert!(
            text.contains("\nHi all,\n"),
            "forwarded text is not quoted: {text}"
        );
        assert!(c.in_reply_to.is_empty());
    }

    #[test]
    fn new_message_has_signature_and_default_identity() {
        let c = start(ComposeKind::New, None);
        assert_eq!(c.from.email, "mira@example.org");
        assert_eq!(c.body_text(), "\n\n-- \nMira\n");
        assert_eq!(c.field, Field::To);
        assert!(c.is_blank() || !c.body_text().is_empty());
    }

    #[test]
    fn subjects_are_prefixed_once() {
        assert_eq!(prefixed_subject("Re: x", "Re:"), "Re: x");
        assert_eq!(prefixed_subject("AW: x", "Re:"), "AW: x");
        assert_eq!(prefixed_subject("x", "Re:"), "Re: x");
        assert_eq!(prefixed_subject("", "Fwd:"), "Fwd:");
        assert_eq!(prefixed_subject("FW: x", "Fwd:"), "FW: x");
    }

    #[test]
    fn address_field_commits_and_keeps_typos() {
        let mut a = AddressField::default();
        assert!(a.add_text("a@example.com, b@example.org"));
        assert!(!a.add_text("bad, c@example.net"));
        assert_eq!(a.chips.len(), 3);
        assert_eq!(a.input.text(), "bad");
        assert!(a.error.as_deref().unwrap().contains("needs an @"));
        assert!(
            a.add_text("A@EXAMPLE.COM"),
            "duplicates are dropped quietly"
        );
        assert_eq!(a.chips.len(), 3);
    }

    #[test]
    fn keys_edit_fields() {
        let mut c = start(ComposeKind::New, None);
        let k = |s: &str| -> Key { s.parse().unwrap() };
        for ch in "ada@example.com".chars() {
            c.key(&Key::char(ch));
        }
        c.key(&k("enter"));
        assert_eq!(c.to.chips.len(), 1);
        c.key(&k("backspace"));
        assert!(c.to.chips.is_empty());
        c.key(&k("enter"));
        assert_eq!(c.field, Field::Cc, "Enter on an empty field moves on");
        c.step_field(true);
        assert_eq!(c.field, Field::Subject);
        c.key(&Key::char('H'));
        c.key(&k("enter"));
        assert_eq!(c.field, Field::Body);
        c.key(&Key::char('x'));
        assert!(c.body_text().starts_with('x'));
        c.key(&k("ctrl-z"));
        assert!(!c.body_text().starts_with('x'), "undo in the editor");
        c.key(&k("ctrl-y"));
        assert!(c.body_text().starts_with('x'), "redo in the editor");
        c.step_field(true);
        assert_eq!(c.field, Field::Attachments);
        assert_eq!(c.key(&k("enter")), KeyOutcome::AttachPrompt);
        c.step_field(true);
        assert_eq!(c.field, Field::From);
        assert_eq!(c.key(&k("enter")), KeyOutcome::PickFrom);
        c.editing = false;
        assert_eq!(c.key(&k("j")), KeyOutcome::Used { changed: false });
        assert_eq!(c.field, Field::To);
        assert_eq!(c.key(&k("x")), KeyOutcome::Pass);
    }

    #[test]
    fn bcc_toggles_and_paste_cleans() {
        let mut c = start(ComposeKind::New, None);
        c.toggle_bcc();
        assert!(c.show_bcc && c.field == Field::Bcc);
        assert!(c.fields().contains(&Field::Bcc));
        c.paste("x@example.com\ny@example.com");
        assert_eq!(c.bcc.chips.len(), 2);
        c.toggle_bcc();
        assert!(c.show_bcc, "Bcc with recipients stays visible");
        c.field = Field::Body;
        c.paste("a\u{1b}[31mred\u{1b}[0m\r\nb\u{7}");
        assert!(c.body_text().starts_with("ared\nb"), "{:?}", c.body_text());
    }

    #[test]
    fn autosave_timer_and_message() {
        let mut c = start(ComposeKind::New, None);
        assert_eq!(c.autosave_at(), None);
        let t: Timestamp = "2026-10-23T14:00:00Z".parse().unwrap();
        let due = c.touch(t);
        assert_eq!(c.autosave_at(), Some(due));
        c.to.input.set("ada@example.com");
        let m = c.message();
        assert_eq!(m.to.len(), 1, "typed text that parses is sent");
        assert_eq!(m.draft_id.as_str(), "d1");
        assert!(!c.missing_attachment());
        c.body.insert_str("The plan is attached.");
        assert!(c.missing_attachment());
        assert_eq!(c.words(), 4, "quotes and the signature are not counted");
    }

    #[test]
    fn clean_text_strips_escapes() {
        assert_eq!(
            clean_text("a\u{1b}]8;;http://x\u{7}b\u{1b}]8;;\u{1b}\\c"),
            "abc"
        );
        assert_eq!(clean_text("tab\tok\r\n"), "tab\tok\n");
    }
}
