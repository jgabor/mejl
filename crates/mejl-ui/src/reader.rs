//! The reader document: a conversation laid out as styled lines.
//!
//! `update` builds a [`ReaderDoc`] whenever the conversation, the expanded
//! messages, the folds, a body or the width change. `render` then only
//! copies the visible slice into the frame, so scrolling a long
//! conversation costs one slice per frame.
//!
//! Lines carry semantic [`StyleName`]s, not colours, and optional
//! [`HitTarget`]s for message headers, links, fold markers and attachments.

use std::collections::{HashMap, HashSet};

use jiff::Zoned;
use mejl_render::{FoldKind, RenderedBody, SpanKind, SpanStyle};
use ratatui::style::Modifier;

use crate::action::Action;
use crate::body::{BodyMeta, BodyState};
use crate::data::{ConversationView, EmailId, MaskedEmailView, MaskedState, Participant, ThreadId};
use crate::dates;
use crate::hit::HitTarget;
use crate::text::{fit, sanitize, width};
use crate::theme::{Glyphs, StyleName};

/// One run of text.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DocSpan {
    pub(crate) text: String,
    pub(crate) style: StyleName,
    pub(crate) add: Modifier,
    pub(crate) target: Option<HitTarget>,
}

impl DocSpan {
    pub(crate) fn new(text: impl Into<String>, style: StyleName) -> DocSpan {
        DocSpan {
            text: text.into(),
            style,
            add: Modifier::empty(),
            target: None,
        }
    }

    pub(crate) fn target(mut self, target: HitTarget) -> DocSpan {
        self.target = Some(target);
        self
    }
}

/// One line: left spans, optional right-aligned spans, optional target for
/// the whole line.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DocLine {
    pub(crate) spans: Vec<DocSpan>,
    pub(crate) right: Vec<DocSpan>,
    pub(crate) target: Option<HitTarget>,
}

impl DocLine {
    pub(crate) fn new(spans: Vec<DocSpan>) -> DocLine {
        DocLine {
            spans,
            ..DocLine::default()
        }
    }

    fn blank() -> DocLine {
        DocLine::default()
    }

    fn right(mut self, right: Vec<DocSpan>) -> DocLine {
        self.right = right;
        self
    }

    fn target(mut self, target: HitTarget) -> DocLine {
        self.target = Some(target);
        self
    }

    /// The plain text, with right-aligned parts after a tab.
    pub(crate) fn text(&self) -> String {
        let mut s: String = self.spans.iter().map(|s| s.text.as_str()).collect();
        if !self.right.is_empty() {
            s.push('\t');
            s.extend(self.right.iter().map(|s| s.text.as_str()));
        }
        s
    }
}

/// A titled block of the full reader's side panel.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SideSection {
    pub(crate) title: String,
    pub(crate) title_style: Option<StyleName>,
    pub(crate) lines: Vec<DocLine>,
}

/// A laid out conversation.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ReaderDoc {
    pub(crate) thread: Option<ThreadId>,
    pub(crate) width: usize,
    pub(crate) lines: Vec<DocLine>,
    /// First line of each message, in conversation order.
    pub(crate) message_lines: Vec<(EmailId, usize)>,
    pub(crate) side: Vec<SideSection>,
    /// The focused message was marked (the reader had focus).
    pub(crate) focus_marker: bool,
}

/// Pane preview or full screen reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DocMode {
    /// The reader pane next to the list.
    Pane,
    /// The reader fills the screen; `side` tells whether the side panel
    /// shows attachments and sender details.
    Full { side: bool },
}

impl DocMode {
    fn indent(self) -> usize {
        match self {
            DocMode::Pane => 3,
            DocMode::Full { .. } => 6,
        }
    }
}

/// The width bodies are laid out at for a reader pane `pane_width` wide.
pub(crate) fn body_width(mode: DocMode, pane_width: usize) -> usize {
    let indent = mode.indent();
    pane_width.saturating_sub(indent + 2).clamp(1, 100)
}

/// Everything the builder needs.
pub(crate) struct DocInput<'a> {
    /// Index of the focused message.
    pub(crate) focused: usize,
    /// Mark the focused message (the reader has focus).
    pub(crate) focus_marker: bool,
    /// Indent messages as a reply tree.
    pub(crate) tree: bool,
    pub(crate) conv: &'a ConversationView,
    pub(crate) mode: DocMode,
    pub(crate) width: usize,
    pub(crate) expanded: &'a HashSet<EmailId>,
    pub(crate) quotes_shown: bool,
    pub(crate) fold_overrides: &'a HashSet<(EmailId, usize)>,
    pub(crate) headers_full: bool,
    pub(crate) bodies: &'a HashMap<EmailId, BodyState>,
    pub(crate) now: &'a Zoned,
    pub(crate) me: &'a [String],
    pub(crate) labels: &'a [String],
    pub(crate) masked: &'a HashMap<String, MaskedEmailView>,
    pub(crate) in_contacts: bool,
    pub(crate) keys: &'a dyn Fn(Action) -> Option<String>,
    pub(crate) glyphs: &'a Glyphs,
}

impl DocInput<'_> {
    fn is_me(&self, p: &Participant) -> bool {
        self.me.iter().any(|m| m.eq_ignore_ascii_case(&p.email))
    }

    fn name(&self, p: &Participant) -> String {
        let n = sanitize(p.display()).into_owned();
        if self.is_me(p) {
            format!("{n} (you)")
        } else {
            n
        }
    }

    fn masked(&self, p: &Participant) -> Option<&MaskedEmailView> {
        self.masked.get(&p.email.to_lowercase())
    }

    fn key_hint(&self, action: Action, label: &str) -> Vec<DocSpan> {
        match (self.keys)(action) {
            Some(k) => vec![
                DocSpan::new(k, StyleName::HintKey),
                DocSpan::new(format!(" {label}"), StyleName::HintLabel),
            ],
            None => Vec::new(),
        }
    }
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

fn kb(size: u64) -> String {
    if size >= 1024 * 1024 {
        format!("{:.1} MB", size as f64 / (1024.0 * 1024.0))
    } else {
        format!("{} KB", size.div_ceil(1024).max(1))
    }
}

fn span_style(style: &SpanStyle) -> (StyleName, Modifier) {
    let name = match style.kind {
        SpanKind::Normal => StyleName::ReaderBody,
        SpanKind::Heading => StyleName::ReaderHeading,
        SpanKind::Link(_) => StyleName::ReaderLink,
        SpanKind::FootnoteMarker(_) => StyleName::ReaderFootnote,
        SpanKind::Quote(d) => StyleName::quote(d),
        SpanKind::Signature => StyleName::ReaderSignature,
        SpanKind::Code => StyleName::ReaderCode,
        SpanKind::TableRule => StyleName::ReaderRule,
    };
    let mut m = Modifier::empty();
    if style.bold {
        m |= Modifier::BOLD;
    }
    if style.italic {
        m |= Modifier::ITALIC;
    }
    if style.strikethrough {
        m |= Modifier::CROSSED_OUT;
    }
    if style.dim {
        m |= Modifier::DIM;
    }
    (name, m)
}

/// Lays out a conversation.
pub(crate) fn build(input: &DocInput<'_>) -> ReaderDoc {
    let conv = input.conv;
    let g = input.glyphs;
    let ell = g.ellipsis;
    let w = input.width;
    let mut lines = Vec::new();
    let mut message_lines = Vec::new();

    let attachments: usize = conv
        .messages
        .iter()
        .filter(|m| m.has_attachment)
        .map(|m| match input.bodies.get(&m.email_id) {
            Some(BodyState::Ready { meta, .. }) => meta.attachments.len(),
            _ => 1,
        })
        .sum();
    let count = plural(conv.messages.len(), "message");
    let info = match input.mode {
        DocMode::Pane if input.labels.is_empty() => format!(" {count}"),
        DocMode::Pane => format!(" {count} in {}", input.labels.join(", ")),
        DocMode::Full { .. } => {
            let mut s = format!(" {count}");
            if attachments > 0 {
                s.push_str(&format!(", {}", plural(attachments, "attachment")));
            }
            match input.labels.len() {
                0 => {}
                1 => s.push_str(&format!("   label: {}", input.labels[0])),
                _ => s.push_str(&format!("   labels: {}", input.labels.join(", "))),
            }
            s
        }
    };
    lines.push(DocLine::new(vec![DocSpan::new(
        fit(&info, w.saturating_sub(1), ell).trim_end().to_owned(),
        StyleName::ReaderInfo,
    )]));
    if input.mode == DocMode::Pane {
        lines.push(DocLine::blank());
    }

    let (name_w, date_w): (usize, usize) = match input.mode {
        DocMode::Pane => (16, 9),
        DocMode::Full { .. } => (20, 13),
    };
    let indent = " ".repeat(input.mode.indent());
    let side_panel = matches!(input.mode, DocMode::Full { side: true });

    let depths = if input.tree {
        tree_depths(conv)
    } else {
        vec![0; conv.messages.len()]
    };
    let marked = input.focus_marker && conv.messages.len() > 1;
    for (mi, msg) in conv.messages.iter().enumerate() {
        let id = &msg.email_id;
        message_lines.push((id.clone(), lines.len()));
        let depth = depths.get(mi).copied().unwrap_or(0).min(8);
        let branch = if depth == 0 {
            String::new()
        } else {
            format!("{}{} ", "  ".repeat(depth - 1), g.crumb)
        };
        let marker_style = if marked && mi == input.focused {
            StyleName::ReaderFocus
        } else {
            StyleName::ReaderMarker
        };
        if !input.expanded.contains(id) {
            let marker = format!(" {} ", g.collapsed);
            let date = match input.mode {
                DocMode::Pane => dates::pane_date(msg.date, input.now),
                DocMode::Full { .. } => dates::full_date(msg.date, input.now),
            };
            let head = format!(
                "{branch}{} {} ",
                fit(
                    &input.name(&msg.from),
                    name_w.saturating_sub(width(&branch)),
                    ell
                ),
                fit(&date, date_w, ell)
            );
            let rest = w.saturating_sub(width(&head) + width(&marker) + 1);
            let snippet = fit(&sanitize(&msg.preview), rest, ell)
                .trim_end()
                .to_owned();
            let name_style = if msg.unread {
                StyleName::ListUnread
            } else {
                StyleName::ReaderCollapsed
            };
            let marker_span = if marker_style == StyleName::ReaderFocus {
                DocSpan::new(marker, marker_style)
            } else {
                DocSpan::new(marker, name_style)
            };
            lines.push(
                DocLine::new(vec![
                    marker_span,
                    DocSpan::new(head, name_style),
                    DocSpan::new(snippet, StyleName::ReaderSnippet),
                ])
                .target(HitTarget::Message(id.clone())),
            );
            continue;
        }

        // Header block.
        let sender = input.name(&msg.from);
        let addr = format!(" <{}>", sanitize(&msg.from.email));
        let date = match input.mode {
            DocMode::Pane => dates::list_date(msg.date, input.now),
            DocMode::Full { .. } => dates::header_date(msg.date, input.now),
        };
        let date_w = width(&date) + 2;
        let room = w.saturating_sub(3 + date_w + width(&branch));
        let sender_fit = crate::text::truncate(&sender, room, ell).0;
        let addr_room = room.saturating_sub(width(&sender_fit));
        let addr_fit = crate::text::truncate(&addr, addr_room, ell).0;
        lines.push(
            DocLine::new(vec![
                DocSpan::new(format!(" {} ", g.expanded), marker_style),
                DocSpan::new(branch.clone(), StyleName::ReaderMarker),
                DocSpan::new(sender_fit, StyleName::ReaderSender),
                DocSpan::new(addr_fit, StyleName::ReaderAddress),
            ])
            .right(vec![DocSpan::new(date, StyleName::ReaderDate)])
            .target(HitTarget::Message(id.clone())),
        );
        let recipients: Vec<&Participant> = msg.to.iter().chain(&msg.cc).collect();
        let masked = recipients
            .iter()
            .find_map(|p| input.masked(p).map(|m| (*p, m)));
        if let Some((p, mask)) = masked {
            let mut spans = vec![
                DocSpan::new("   to ", StyleName::ReaderAddress),
                DocSpan::new(sanitize(&p.email).into_owned(), StyleName::ReaderMasked),
                DocSpan::new("  ", StyleName::ReaderAddress),
            ];
            let mut badge = DocSpan::new(g.masked, StyleName::ReaderMasked);
            badge.add = Modifier::BOLD;
            spans.push(badge);
            let label = if mask.description.trim().is_empty() {
                mask.for_domain.clone()
            } else {
                mask.description.clone()
            };
            spans.push(DocSpan::new(
                format!(" masked: {}", sanitize(&label)),
                StyleName::ReaderMasked,
            ));
            lines.push(DocLine::new(spans));
        } else if !recipients.is_empty() {
            let names: Vec<String> = recipients
                .iter()
                .map(|p| {
                    if input.is_me(p) {
                        "me".to_owned()
                    } else {
                        sanitize(&p.email).into_owned()
                    }
                })
                .collect();
            let text = format!("   to {}", names.join(", "));
            lines.push(DocLine::new(vec![DocSpan::new(
                crate::text::truncate(&text, w.saturating_sub(1), ell).0,
                StyleName::ReaderAddress,
            )]));
        }
        let state = input.bodies.get(id);
        if input.headers_full {
            let meta = match state {
                Some(BodyState::Ready { meta, .. }) => Some(meta.as_ref()),
                _ => None,
            };
            header_lines(&mut lines, msg, meta, input, w);
        }
        match state {
            Some(BodyState::Ready { body, meta, stale }) => {
                notices(&mut lines, body, input, *stale);
                lines.push(DocLine::blank());
                body_lines(&mut lines, id, body, input, &indent);
                if !side_panel && !meta.attachments.is_empty() {
                    lines.push(DocLine::blank());
                    attachment_lines(&mut lines, id, meta, &indent, w, ell);
                }
            }
            Some(BodyState::Failed(err)) => {
                lines.push(DocLine::blank());
                lines.push(DocLine::new(vec![DocSpan::new(
                    format!(
                        "{indent}{} could not load this message: {}",
                        g.fail,
                        sanitize(err)
                    ),
                    StyleName::StatusError,
                )]));
            }
            Some(BodyState::Rendering) => {
                lines.push(DocLine::blank());
                lines.push(DocLine::new(vec![DocSpan::new(
                    format!("{indent}{ell} preparing the message"),
                    StyleName::Dim,
                )]));
            }
            Some(BodyState::Loading) | None => {
                lines.push(DocLine::blank());
                lines.push(DocLine::new(vec![DocSpan::new(
                    format!("{indent}{ell} loading the message"),
                    StyleName::Dim,
                )]));
            }
        }
        lines.push(DocLine::blank());
    }

    if input.mode == DocMode::Pane {
        let mut hint = vec![DocSpan::new("   ", StyleName::Text)];
        for (i, (action, label)) in [
            (Action::Reply, "reply"),
            (Action::ReplyAll, "reply all"),
            (Action::Forward, "forward"),
            (Action::ExpandAll, "expand all"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut part = input.key_hint(action, label);
            if part.is_empty() {
                continue;
            }
            if i > 0 && hint.len() > 1 {
                hint.push(DocSpan::new("  ", StyleName::HintLabel));
            }
            for s in &mut part {
                s.target = Some(HitTarget::Action(action));
            }
            hint.extend(part);
        }
        if hint.len() > 1 {
            lines.push(DocLine::new(hint));
        }
    }

    let side = if side_panel {
        side_sections(input)
    } else {
        Vec::new()
    };
    ReaderDoc {
        thread: Some(conv.thread_id.clone()),
        width: w,
        lines,
        message_lines,
        side,
        focus_marker: input.focus_marker,
    }
}

/// Reply depth of each message: its parent is the newest earlier message
/// whose Message-ID appears in its References.
fn tree_depths(conv: &ConversationView) -> Vec<usize> {
    let mut depths: Vec<usize> = Vec::with_capacity(conv.messages.len());
    for (i, m) in conv.messages.iter().enumerate() {
        let parent = conv.messages[..i].iter().enumerate().rev().find(|(_, p)| {
            p.message_id
                .as_ref()
                .is_some_and(|id| m.references.iter().any(|r| r == id))
        });
        depths.push(parent.map_or(0, |(pi, _)| depths[pi] + 1));
    }
    depths
}

fn header_lines(
    lines: &mut Vec<DocLine>,
    msg: &crate::data::MessageSummary,
    meta: Option<&BodyMeta>,
    input: &DocInput<'_>,
    w: usize,
) {
    let ell = input.glyphs.ellipsis;
    let fmt = |ps: &[Participant]| -> String {
        ps.iter()
            .map(|p| match &p.name {
                Some(n) if !n.trim().is_empty() => {
                    format!("{} <{}>", sanitize(n), sanitize(&p.email))
                }
                _ => sanitize(&p.email).into_owned(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut rows: Vec<(String, String)> = vec![
        ("From".into(), fmt(std::slice::from_ref(&msg.from))),
        ("To".into(), fmt(&msg.to)),
    ];
    if !msg.cc.is_empty() {
        rows.push(("Cc".into(), fmt(&msg.cc)));
    }
    rows.push((
        "Date".into(),
        msg.date
            .to_zoned(input.now.time_zone().clone())
            .strftime("%a, %-d %b %Y %H:%M:%S %z")
            .to_string(),
    ));
    if let Some(meta) = meta {
        for (name, value) in &meta.headers {
            if !rows.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)) {
                rows.push((name.clone(), value.clone()));
            }
        }
    }
    for (name, value) in rows {
        let name = format!("   {}: ", sanitize(&name));
        let room = w.saturating_sub(width(&name) + 1);
        lines.push(DocLine::new(vec![
            DocSpan::new(name, StyleName::ReaderHeaderName),
            DocSpan::new(
                crate::text::truncate(&sanitize(&value), room, ell).0,
                StyleName::ReaderBody,
            ),
        ]));
    }
}

fn notices(lines: &mut Vec<DocLine>, body: &RenderedBody, input: &DocInput<'_>, stale: bool) {
    let n = body.notices;
    let mut parts = Vec::new();
    if n.remote_images_blocked > 0 {
        parts.push("remote images off".to_owned());
    }
    if n.trackers_blocked > 0 {
        parts.push(plural(n.trackers_blocked, "tracker") + " blocked");
    }
    if n.truncated {
        parts.push("long message cut short".to_owned());
    }
    if n.html_fallback {
        parts.push("HTML shown as plain text".to_owned());
    }
    if stale {
        parts.push("rewrapping".to_owned());
    }
    if parts.is_empty() {
        return;
    }
    let mut line = DocLine::new(vec![DocSpan::new(
        format!("   {}", parts.join(", ")),
        StyleName::ReaderNotice,
    )]);
    if n.remote_images_blocked > 0 || n.trackers_blocked > 0 {
        let mut hint = input.key_hint(Action::OpenInBrowser, "open in browser");
        for s in &mut hint {
            s.target = Some(HitTarget::Action(Action::OpenInBrowser));
        }
        line = line.right(hint);
    }
    lines.push(line);
}

fn body_lines(
    lines: &mut Vec<DocLine>,
    id: &EmailId,
    body: &RenderedBody,
    input: &DocInput<'_>,
    indent: &str,
) {
    let g = input.glyphs;
    let full = matches!(input.mode, DocMode::Full { .. });
    let mut i = 0;
    while i < body.lines.len() {
        if let Some((fi, fold)) = body
            .folds
            .iter()
            .enumerate()
            .find(|(_, f)| f.start_line == i && f.line_count > 0)
        {
            let default_folded = match fold.kind {
                FoldKind::Quote => !input.quotes_shown,
                FoldKind::Signature => true,
            };
            let overridden = input.fold_overrides.contains(&(id.clone(), fi));
            let folded = default_folded != overridden;
            let target = HitTarget::Fold {
                email_id: id.clone(),
                fold: fi,
            };
            if folded {
                let mut spans = vec![DocSpan::new(
                    format!("{indent}{} {}", g.ellipsis, sanitize(&fold.summary)),
                    StyleName::ReaderFold,
                )];
                if fold.kind == FoldKind::Quote
                    && let Some(k) = (input.keys)(Action::ToggleQuotes)
                {
                    spans.push(DocSpan::new("  (", StyleName::ReaderFold));
                    spans.push(DocSpan::new(k, StyleName::HintKey));
                    spans.push(DocSpan::new(
                        if full { " to show)" } else { ")" },
                        StyleName::ReaderFold,
                    ));
                }
                lines.push(DocLine::new(spans).target(target));
                i += fold.line_count;
                continue;
            }
            for line in &body.lines[i..(i + fold.line_count).min(body.lines.len())] {
                lines.push(styled(line, body, indent).target(target.clone()));
            }
            i += fold.line_count;
            continue;
        }
        lines.push(styled(&body.lines[i], body, indent));
        i += 1;
    }
}

fn styled(line: &mejl_render::StyledLine, body: &RenderedBody, indent: &str) -> DocLine {
    let mut spans = vec![DocSpan::new(indent, StyleName::ReaderBody)];
    for s in &line.spans {
        let (style, add) = span_style(&s.style);
        let target = match s.style.kind {
            SpanKind::Link(n) | SpanKind::FootnoteMarker(n) => body
                .links
                .get(n.wrapping_sub(1))
                .map(|l| HitTarget::Link(l.url.clone())),
            _ => None,
        };
        spans.push(DocSpan {
            text: s.text.clone(),
            style,
            add,
            target,
        });
    }
    DocLine::new(spans)
}

fn attachment_lines(
    lines: &mut Vec<DocLine>,
    id: &EmailId,
    meta: &BodyMeta,
    indent: &str,
    w: usize,
    ell: &str,
) {
    for (index, a) in meta.attachments.iter().enumerate() {
        let size = kb(a.size);
        let room = w.saturating_sub(width(indent) + width(&size) + 3);
        let name = crate::text::truncate(&sanitize(&a.name), room, ell).0;
        let target = HitTarget::Attachment {
            email_id: id.clone(),
            index,
        };
        lines.push(DocLine::new(vec![
            DocSpan::new(indent, StyleName::Text),
            DocSpan::new(name, StyleName::ReaderAttachment).target(target.clone()),
            DocSpan::new(format!("  {size}"), StyleName::Dim),
        ]));
    }
}

fn side_sections(input: &DocInput<'_>) -> Vec<SideSection> {
    let g = input.glyphs;
    let ell = g.ellipsis;
    let Some(latest) = input
        .conv
        .messages
        .iter()
        .rev()
        .find(|m| input.expanded.contains(&m.email_id))
        .or(input.conv.messages.last())
    else {
        return Vec::new();
    };
    let w = crate::layout::SIDE_PANEL_WIDTH as usize;
    let line = |label: &str, value: String, style: StyleName| {
        DocLine::new(vec![
            DocSpan::new(format!("   {}", fit(label, 9, ell)), StyleName::Dim),
            DocSpan::new(
                crate::text::truncate(&value, w.saturating_sub(13), ell).0,
                style,
            ),
        ])
    };
    let mut out = Vec::new();
    let recipients = latest.to.iter().chain(&latest.cc);
    if let Some(mask) = recipients.filter_map(|p| input.masked(p)).next() {
        let mut lines = Vec::new();
        let mut addr = DocSpan::new(
            crate::text::truncate(&sanitize(&mask.email), w - 4, ell).0,
            StyleName::ReaderMasked,
        );
        addr.add = Modifier::BOLD;
        let mut at = DocSpan::new(format!(" {} ", g.masked), StyleName::ReaderMasked);
        at.add = Modifier::BOLD;
        lines.push(DocLine::new(vec![at, addr]));
        if !mask.description.trim().is_empty() {
            lines.push(DocLine::new(vec![DocSpan::new(
                format!("   {}", sanitize(&mask.description)),
                StyleName::ReaderSender,
            )]));
        }
        lines.push(line(
            "site",
            sanitize(&mask.for_domain).into_owned(),
            StyleName::Text,
        ));
        let (state, style) = match mask.state {
            MaskedState::Enabled => (format!("{} enabled", g.ok), StyleName::StatusOk),
            MaskedState::Disabled => (format!("{} disabled", g.fail), StyleName::StatusError),
            MaskedState::Pending => (format!("{} pending", g.unread), StyleName::Dim),
            MaskedState::Deleted => ("- deleted".to_owned(), StyleName::Dim),
        };
        lines.push(line("state", state, style));
        if let Some(created) = mask.created_at {
            let date = created
                .to_zoned(input.now.time_zone().clone())
                .strftime("%Y-%m-%d")
                .to_string();
            let by = if mask.created_by.trim().is_empty() {
                date
            } else {
                format!("{date} by {}", sanitize(&mask.created_by))
            };
            lines.push(line("created", by, StyleName::Text));
        }
        // When mail last came in, and how much of it is cached, as in the
        // mock-up's side panel.
        let last = match mask.last_message_at {
            Some(t) if t.to_zoned(input.now.time_zone().clone()).date() == input.now.date() => {
                format!("today {}", crate::masked::last_seen(Some(t), input.now))
            }
            other => crate::masked::last_seen(other, input.now),
        };
        lines.push(line("last", last, StyleName::Text));
        lines.push(line(
            "mail",
            format!(
                "{}, {} in Spam",
                plural(mask.local_messages as usize, "message"),
                mask.local_in_spam
            ),
            StyleName::Text,
        ));
        let domain = mask.for_domain.to_lowercase();
        let sender_domain = latest
            .from
            .email
            .rsplit_once('@')
            .map(|(_, d)| d.to_lowercase())
            .unwrap_or_default();
        let matches = !domain.is_empty()
            && (sender_domain == domain || sender_domain.ends_with(&format!(".{domain}")));
        lines.push(if matches {
            DocLine::new(vec![
                DocSpan::new(format!("   {} ", g.ok), StyleName::StatusOk),
                DocSpan::new("sender matches the site", StyleName::Dim),
            ])
        } else {
            DocLine::new(vec![
                DocSpan::new("   ! ", StyleName::StatusWarning),
                DocSpan::new("sender is not from the site", StyleName::StatusWarning),
            ])
        });
        lines.push(DocLine::blank());
        for (action, label) in [
            (Action::MaskedDisable, "disable: mail goes to Trash"),
            (Action::MaskedDelete, "delete: mail bounces"),
            (Action::MaskedRule, "rule for this address"),
            (Action::MaskedMessages, "all mail to this address"),
            (Action::MaskedCopy, "copy address"),
        ] {
            if let Some(k) = (input.keys)(action) {
                lines.push(DocLine::new(vec![
                    DocSpan::new("   ", StyleName::Text),
                    DocSpan::new(fit(&k, 4, ell), StyleName::HintKey)
                        .target(HitTarget::Action(action)),
                    DocSpan::new(format!(" {label}"), StyleName::Text)
                        .target(HitTarget::Action(action)),
                ]));
            }
        }
        out.push(SideSection {
            title: "Delivered to a masked address".into(),
            title_style: Some(StyleName::ReaderMasked),
            lines,
        });
    }
    let mut attachments = Vec::new();
    for m in &input.conv.messages {
        if let Some(BodyState::Ready { meta, .. }) = input.bodies.get(&m.email_id)
            && !meta.attachments.is_empty()
        {
            attachment_lines(&mut attachments, &m.email_id, meta, "   ", w, ell);
        }
    }
    if !attachments.is_empty() {
        out.push(SideSection {
            title: "Attachments".into(),
            title_style: None,
            lines: attachments,
        });
    }
    let mut sender = vec![
        DocLine::new(vec![DocSpan::new(
            format!(
                "   {}",
                crate::text::truncate(&sanitize(&latest.from.email), w - 4, ell).0
            ),
            StyleName::Text,
        )]),
        DocLine::new(vec![DocSpan::new(
            if input.in_contacts {
                "   in contacts"
            } else {
                "   not in contacts"
            },
            StyleName::Dim,
        )]),
    ];
    for (action, label) in [
        (Action::NewRule, "new rule from this message"),
        (Action::Mute, "mute conversation"),
    ] {
        if let Some(k) = (input.keys)(action) {
            sender.push(DocLine::new(vec![
                DocSpan::new("   ", StyleName::Text),
                DocSpan::new(k, StyleName::HintKey).target(HitTarget::Action(action)),
                DocSpan::new(format!("  {label}"), StyleName::Text)
                    .target(HitTarget::Action(action)),
            ]));
        }
    }
    out.push(SideSection {
        title: "Sender".into(),
        title_style: None,
        lines: sender,
    });
    out
}
