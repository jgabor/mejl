//! `text/html` bodies: cleaning, html2text conversion and style mapping.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use html2text::render::{TaggedLine, TextDecorator};
use html2text::{Handle, RenderTree, config};

use crate::classify::{self, LineInfo};
use crate::layout::{self, Rendered};
use crate::links::{LinkTable, clean_url, marker};
use crate::nesting::limit_nesting;
use crate::plain::PlainDoc;
use crate::text::push_clean;
use crate::width::{LineBuilder, clamp};
use crate::{BodyNotices, Fold, Link, LinkStyle, Settings, SpanKind, SpanStyle, StyledLine};

/// Prefix html2text puts in front of heading lines, which it does not
/// annotate. A control character: the cleaner removes it from the input and
/// the post-pass removes it from output. html2text measures prefixes in
/// bytes in some places and columns in others, so a sentinel must be one
/// byte wide in both.
const HEADING_MARK: char = '\u{1E}';

/// Prefix html2text puts in front of each blockquote level. Replaced by
/// `>`, so author text that starts with `>` is not mistaken for a quote.
const QUOTE_MARK: char = '\u{1F}';

/// Elements deeper than this are flattened before conversion. html2text
/// copes with deep trees, but cloning its render tree recurses and a stack
/// overflow aborts the process; 256 levels stay far inside a 2 MiB stack
/// and far above what real mail uses.
const MAX_DEPTH: usize = 256;

/// Table and list parts keep their structure a little past [`MAX_DEPTH`],
/// so flattening happens inside a cell or item, where html2text renders
/// loose text, instead of directly under a `tr` or `ul`, where it drops it.
const STRUCTURAL_SLACK: usize = 8;

const STRUCTURAL: &[&str] = &[
    "table", "thead", "tbody", "tfoot", "tr", "colgroup", "ul", "ol", "dl",
];

/// Table parts that html2text only accepts inside a table.
const TABLE_PARTS: &[&str] = &[
    "table", "caption", "colgroup", "col", "thead", "tbody", "tfoot", "tr", "td", "th",
];

/// Blockquotes nested deeper than this are unwrapped. Each level costs two
/// columns of prefix, and real reply chains rarely pass a dozen.
const MAX_QUOTE_DEPTH: usize = 16;

/// More cells than this means the tables are page layout, not data.
const MAX_DATA_TABLE_CELLS: usize = 200;

/// Elements dropped with everything inside them.
const DROPPED_ELEMENTS: &[&str] = &[
    "script", "style", "head", "title", "template", "iframe", "frame", "frameset", "object",
    "embed", "applet", "noembed", "meta", "link", "base",
];

/// URL fragments typical of open-tracking pixels.
const TRACKER_HINTS: &[&str] = &[
    "pixel",
    "beacon",
    "/open?",
    "/open/",
    "/open.",
    "track/open",
    "/wf/open",
    "openrate",
    "/o.gif",
    "/t.gif",
];

/// A parsed and cleaned HTML body.
#[derive(Debug, Clone)]
pub(crate) struct HtmlDoc {
    tree: RenderTree,
    /// Draw table borders. Only when the tables look like data tables.
    borders: bool,
    /// The input, kept for the crude fallback if a later layout fails.
    source: String,
    remote_images: usize,
    trackers: usize,
}

/// The width dependent result of an HTML rendering.
pub(crate) struct HtmlOut {
    pub(crate) lines: Vec<StyledLine>,
    pub(crate) links: Vec<Link>,
    pub(crate) folds: Vec<Fold>,
    pub(crate) fallback: bool,
}

/// Runs `f`, turning a panic into `None`.
fn guarded<T>(f: impl FnOnce() -> Option<T>) -> Option<T> {
    catch_unwind(AssertUnwindSafe(f)).ok().flatten()
}

impl HtmlDoc {
    /// Parses and cleans `html`. Fails when html2text errors or panics.
    pub(crate) fn new(html: &str) -> Result<Self, ()> {
        let source = strip_controls(&limit_nesting(html, MAX_DEPTH));
        let built = guarded(|| {
            let cfg = config::with_decorator(MejlDecorator::new(false));
            let dom = cfg.parse_html(source.as_bytes()).ok()?;
            let report = preclean(&dom.document);
            let tree = cfg.dom_to_render_tree(&dom).ok()?;
            Some((tree, report))
        });
        let (tree, report) = built.ok_or(())?;
        Ok(HtmlDoc {
            tree,
            borders: report.data_tables,
            source,
            remote_images: report.remote_images,
            trackers: report.trackers,
        })
    }

    pub(crate) fn notices(&self) -> BodyNotices {
        BodyNotices {
            remote_images_blocked: self.remote_images,
            trackers_blocked: self.trackers,
            ..BodyNotices::default()
        }
    }

    pub(crate) fn render(&self, width: usize, settings: Settings) -> HtmlOut {
        layout(
            self.tree.clone(),
            self.borders,
            &self.source,
            width,
            settings,
        )
    }

    /// Renders without copying the render tree.
    pub(crate) fn render_owned(self, width: usize, settings: Settings) -> HtmlOut {
        layout(self.tree, self.borders, &self.source, width, settings)
    }
}

fn layout(
    tree: RenderTree,
    borders: bool,
    source: &str,
    width: usize,
    settings: Settings,
) -> HtmlOut {
    let footnotes = settings.link_style == LinkStyle::Footnotes;
    let decorator = MejlDecorator::new(footnotes);
    let state = Rc::clone(&decorator.state);
    let lines = guarded(move || {
        // Strikeout is carried by the span flag, not combining characters.
        let mut cfg = config::with_decorator(decorator)
            .allow_width_overflow()
            .unicode_strikeout(false);
        if !borders {
            cfg = cfg.no_table_borders();
        }
        cfg.render_to_lines(tree, width).ok()
    });
    let Some(lines) = lines else {
        let doc = PlainDoc::new(&crude_text(source), settings);
        let (lines, links, folds) = doc.render(width, settings);
        return HtmlOut {
            lines,
            links,
            folds,
            fallback: true,
        };
    };
    let table = std::mem::take(&mut state.borrow_mut().links);
    let (lines, links, folds) = convert(lines, table, width, settings);
    HtmlOut {
        lines,
        links,
        folds,
        fallback: false,
    }
}

/// Removes control characters from the source, literal or written as
/// numeric character references, except tab and line breaks.
///
/// They would otherwise reach text nodes, where they could fake the prefix
/// sentinels, and html2text panics when it hard wraps a word holding one.
fn strip_controls(html: &str) -> String {
    let unwanted = |c: char| c.is_control() && !matches!(c, '\t' | '\n' | '\r');
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(p) = rest.find("&#") {
        out.extend(rest[..p].chars().filter(|c| !unwanted(*c)));
        rest = &rest[p..];
        let body = &rest[2..];
        let (digits, radix) = match body.strip_prefix(['x', 'X']) {
            Some(hex) => (hex, 16),
            None => (body, 10),
        };
        let len = digits
            .bytes()
            .take_while(|b| (*b as char).is_digit(radix))
            .count();
        let code = u32::from_str_radix(&digits[..len], radix).ok();
        let consumed = rest.len() - digits.len() + len;
        let consumed = if rest[consumed..].starts_with(';') {
            consumed + 1
        } else {
            consumed
        };
        match code.and_then(char::from_u32) {
            Some(c) if len > 0 && unwanted(c) => {}
            _ => out.push_str(&rest[..consumed.max(2)]),
        }
        rest = &rest[consumed.max(2)..];
    }
    out.extend(rest.chars().filter(|c| !unwanted(*c)));
    out
}

/// A converted html2text line before width clamping.
struct RawLine {
    line: StyledLine,
    depth: u8,
    /// Bytes of quote prefix at the start of the line text.
    prefix_len: usize,
}

fn convert(
    tagged: Vec<TaggedLine<Vec<Ann>>>,
    mut table: LinkTable,
    width: usize,
    settings: Settings,
) -> (Vec<StyledLine>, Vec<Link>, Vec<Fold>) {
    let footnotes = settings.link_style == LinkStyle::Footnotes;
    let mut link_text: BTreeMap<usize, String> = BTreeMap::new();
    // Link text is taken from the first occurrence of each URL only.
    let mut finished: BTreeSet<usize> = BTreeSet::new();
    let mut current: Option<usize> = None;
    let mut raw: Vec<RawLine> = Vec::with_capacity(tagged.len());
    for t in &tagged {
        let mut r = convert_line(t);
        if footnotes {
            split_markers(&mut r.line);
        }
        for span in &r.line.spans {
            match span.style.kind {
                SpanKind::Link(id) => {
                    if current.is_some_and(|c| c != id) {
                        finished.extend(current);
                    }
                    if !finished.contains(&id) {
                        link_text.entry(id).or_default().push_str(&span.text);
                    }
                    current = Some(id);
                }
                _ => {
                    finished.extend(current.take());
                }
            }
        }
        if let Some(id) = current
            && let Some(t) = link_text.get_mut(&id)
        {
            t.push(' ');
        }
        // Collapse runs of blank lines left behind by layout tables and
        // stripped filler.
        let blank = r.depth == 0 && r.line.is_blank();
        if blank {
            r.line = StyledLine::default();
            if raw.last().is_none_or(|l| l.depth == 0 && l.line.is_blank()) {
                continue;
            }
        }
        raw.push(r);
    }
    while raw
        .last()
        .is_some_and(|l| l.depth == 0 && l.line.is_blank())
    {
        raw.pop();
    }
    for (id, text) in link_text {
        table.add_text(id, &text);
    }

    // Clamp to the width, then classify the resulting lines.
    let mut lines = Vec::with_capacity(raw.len());
    let mut depths = Vec::with_capacity(raw.len());
    let mut content_start = Vec::with_capacity(raw.len());
    for r in raw {
        for (n, piece) in clamp(r.line, width).into_iter().enumerate() {
            lines.push(piece);
            depths.push(r.depth);
            content_start.push(if n == 0 { r.prefix_len } else { 0 });
        }
    }
    let texts: Vec<String> = lines.iter().map(StyledLine::text).collect();
    let infos: Vec<LineInfo<'_>> = texts
        .iter()
        .zip(&depths)
        .zip(&content_start)
        .map(|((text, &depth), &start)| LineInfo {
            depth,
            content: text.get(start.min(text.len())..).unwrap_or(""),
        })
        .collect();
    let classified = classify::classify(&infos);
    let rows: Vec<Rendered<'_>> = lines
        .into_iter()
        .enumerate()
        .map(|(i, line)| Rendered {
            line,
            kind: classified.kinds[i],
            depth: classified.depths[i],
            label: classified.labels[i].as_ref(),
        })
        .collect();
    let out = layout::assemble(rows, table.into_links(), settings, width);
    (out.lines, out.links, out.folds)
}

fn is_box_drawing(c: char) -> bool {
    ('\u{2500}'..='\u{257F}').contains(&c)
}

/// Maps one html2text line to spans, resolving the prefix sentinels.
fn convert_line(tagged: &TaggedLine<Vec<Ann>>) -> RawLine {
    let mut b = LineBuilder::default();
    let mut col = 0usize;
    let mut in_prefix = true;
    let mut depth = 0u8;
    let mut heading = false;
    let mut prefix_len = 0usize;
    let mut buf = String::new();
    let mut buf_style = SpanStyle::default();
    let mut ch = [0u8; 4];
    // html2text writes "Q " per quote level; mejl shows ">> " like plain
    // text, so spaces between marks are held back until the prefix ends.
    let mut pending_spaces = 0usize;
    let end_prefix = |b: &mut LineBuilder, pending: &mut usize, len: &mut usize| {
        if *pending > 0 {
            b.push(&" ".repeat(*pending), SpanStyle::default());
            *len += *pending;
            *pending = 0;
        }
    };
    for ts in tagged.tagged_strings() {
        let base = style_for(&ts.tag);
        for c in ts.s.chars() {
            if in_prefix {
                match c {
                    QUOTE_MARK => {
                        depth = depth.saturating_add(1);
                        pending_spaces = pending_spaces.saturating_sub(1);
                        end_prefix(&mut b, &mut pending_spaces, &mut prefix_len);
                        b.push(">", SpanStyle::default());
                        prefix_len += 1;
                        col += 1;
                        continue;
                    }
                    ' ' if depth > 0 => {
                        pending_spaces += 1;
                        col += 1;
                        continue;
                    }
                    HEADING_MARK => {
                        end_prefix(&mut b, &mut pending_spaces, &mut prefix_len);
                        heading = true;
                        in_prefix = false;
                        continue;
                    }
                    _ => {
                        end_prefix(&mut b, &mut pending_spaces, &mut prefix_len);
                        in_prefix = false;
                    }
                }
            } else if c == QUOTE_MARK || c == HEADING_MARK {
                continue;
            }
            let mut style = base;
            if is_box_drawing(c) {
                style = SpanStyle::kind(SpanKind::TableRule);
            } else if heading && style.kind == SpanKind::Normal {
                style.kind = SpanKind::Heading;
            }
            if style != buf_style {
                b.push(&std::mem::take(&mut buf), buf_style);
                buf_style = style;
            }
            push_clean(&mut buf, c.encode_utf8(&mut ch), &mut col, true);
        }
    }
    b.push(&buf, buf_style);
    let line = b.finish();
    RawLine {
        line,
        depth,
        prefix_len,
    }
}

/// Splits `[n]` footnote markers out of the link spans that end with them.
fn split_markers(line: &mut StyledLine) {
    let mut out: Vec<crate::Span> = Vec::with_capacity(line.spans.len());
    let push = |out: &mut Vec<crate::Span>, text: &str, style: SpanStyle| match out.last_mut() {
        Some(last) if last.style == style => last.text.push_str(text),
        _ => out.push(crate::Span {
            text: text.to_owned(),
            style,
        }),
    };
    for span in std::mem::take(&mut line.spans) {
        let SpanKind::Link(id) = span.style.kind else {
            push(&mut out, &span.text, span.style);
            continue;
        };
        let m = marker(id);
        let mut rest = span.text.as_str();
        while let Some(p) = rest.find(&m) {
            if p > 0 {
                push(&mut out, &rest[..p], span.style);
            }
            let mut style = span.style;
            style.kind = SpanKind::FootnoteMarker(id);
            push(&mut out, &m, style);
            rest = &rest[p + m.len()..];
        }
        if !rest.is_empty() {
            push(&mut out, rest, span.style);
        }
    }
    line.spans = out;
}

/// html2text annotations, numbered links included.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Ann {
    #[default]
    Plain,
    Link(usize),
    Image,
    Emphasis,
    Strong,
    Strikeout,
    Code,
    Preformat,
}

fn style_for(tags: &[Ann]) -> SpanStyle {
    let mut s = SpanStyle::default();
    for t in tags {
        match t {
            Ann::Plain => {}
            Ann::Link(id) => s.kind = SpanKind::Link(*id),
            Ann::Image => s.dim = true,
            Ann::Emphasis => s.italic = true,
            Ann::Strong => s.bold = true,
            Ann::Strikeout => s.strikethrough = true,
            Ann::Code | Ann::Preformat => {
                if !matches!(s.kind, SpanKind::Link(_)) {
                    s.kind = SpanKind::Code;
                }
            }
        }
    }
    s
}

#[derive(Debug, Default)]
struct DecoState {
    links: LinkTable,
    /// One entry per open link: its index, or `None` for a target mejl does
    /// not open.
    open: Vec<Option<usize>>,
}

/// The decorator: numbers links (shared across table cells and quotes,
/// which html2text renders with cloned sub-decorators), emits footnote
/// markers inside the link annotation so wrapping counts them, renders
/// images as `[image: alt]`, and marks headings and quotes with sentinels.
/// Colour hooks keep their defaults, which drops author colours.
#[derive(Clone)]
struct MejlDecorator {
    state: Rc<RefCell<DecoState>>,
    footnotes: bool,
}

impl MejlDecorator {
    fn new(footnotes: bool) -> Self {
        MejlDecorator {
            state: Rc::new(RefCell::new(DecoState::default())),
            footnotes,
        }
    }
}

impl TextDecorator for MejlDecorator {
    type Annotation = Ann;

    fn decorate_link_start(&mut self, url: &str) -> (String, Ann) {
        let mut state = self.state.borrow_mut();
        match clean_url(url) {
            Some(url) => {
                let index = state.links.intern(&url);
                state.open.push(Some(index));
                (String::new(), Ann::Link(index))
            }
            None => {
                state.open.push(None);
                (String::new(), Ann::Plain)
            }
        }
    }

    fn decorate_link_end(&mut self) -> String {
        let index = self.state.borrow_mut().open.pop().flatten();
        match index {
            Some(index) if self.footnotes => marker(index),
            _ => String::new(),
        }
    }

    fn decorate_em_start(&self) -> (String, Ann) {
        (String::new(), Ann::Emphasis)
    }

    fn decorate_em_end(&self) -> String {
        String::new()
    }

    fn decorate_strong_start(&self) -> (String, Ann) {
        (String::new(), Ann::Strong)
    }

    fn decorate_strong_end(&self) -> String {
        String::new()
    }

    fn decorate_strikeout_start(&self) -> (String, Ann) {
        (String::new(), Ann::Strikeout)
    }

    fn decorate_strikeout_end(&self) -> String {
        String::new()
    }

    fn decorate_code_start(&self) -> (String, Ann) {
        (String::new(), Ann::Code)
    }

    fn decorate_code_end(&self) -> String {
        String::new()
    }

    fn decorate_preformat_first(&self) -> Ann {
        Ann::Preformat
    }

    fn decorate_preformat_cont(&self) -> Ann {
        Ann::Preformat
    }

    fn decorate_image(&mut self, _src: &str, title: &str) -> (String, Ann) {
        let alt = title.split_whitespace().collect::<Vec<_>>().join(" ");
        if alt.is_empty() {
            (String::new(), Ann::Image)
        } else {
            (format!("[image: {alt}]"), Ann::Image)
        }
    }

    fn header_prefix(&self, _level: usize) -> String {
        HEADING_MARK.to_string()
    }

    fn quote_prefix(&self) -> String {
        format!("{QUOTE_MARK} ")
    }

    fn unordered_item_prefix(&self) -> String {
        "* ".to_owned()
    }

    fn ordered_item_prefix(&self, i: i64) -> String {
        format!("{i}. ")
    }

    fn make_subblock_decorator(&self) -> Self {
        self.clone()
    }

    fn decorate_superscript_start(&self) -> (String, Ann) {
        ("^".to_owned(), Ann::Plain)
    }

    fn decorate_superscript_end(&self) -> String {
        String::new()
    }
}

/// What the cleaner found.
#[derive(Debug, Default)]
struct CleanReport {
    remote_images: usize,
    trackers: usize,
    data_tables: bool,
}

#[derive(Debug, Default)]
struct Stats {
    remote_images: usize,
    trackers: usize,
    tables: usize,
    nested_tables: bool,
    presentation_tables: bool,
    cells: usize,
    max_row_cells: usize,
}

enum Verdict {
    Keep,
    Drop,
}

/// Cleans the DOM in place: drops scripts, styles, head, comments, hidden
/// elements and tracking pixels, counts remote images, and flattens
/// anything nested deeper than [`MAX_DEPTH`]. The walk uses an explicit
/// stack, so hostile nesting cannot overflow it.
fn preclean(root: &Handle) -> CleanReport {
    let mut stats = Stats::default();
    let mut stack: Vec<(Handle, usize, bool, usize)> = vec![(Rc::clone(root), 0, false, 0)];
    while let Some((node, depth, in_table, quotes)) = stack.pop() {
        let name = element_name(&node);
        let structural = name.is_some_and(|n| STRUCTURAL.contains(&n));
        if depth >= MAX_DEPTH && !(structural && depth < MAX_DEPTH + STRUCTURAL_SLACK) {
            flatten(&node, &mut stats);
            continue;
        }
        let mut queue: VecDeque<Handle> = node.children.borrow().iter().cloned().collect();
        let mut kept = Vec::with_capacity(queue.len());
        while let Some(child) = queue.pop_front() {
            if quotes >= MAX_QUOTE_DEPTH && element_name(&child) == Some("blockquote") {
                let kids: Vec<Handle> = child.children.borrow().clone();
                for kid in kids.into_iter().rev() {
                    queue.push_front(kid);
                }
                continue;
            }
            if let Verdict::Keep = judge(&child, &mut stats) {
                kept.push(child);
            }
        }
        for child in &kept {
            child.parent.set(Some(Rc::downgrade(&node)));
        }
        node.children.borrow_mut().clone_from(&kept);

        let is_table = name == Some("table");
        if is_table {
            stats.tables += 1;
            stats.nested_tables |= in_table;
            let role = attr(&node, "role").unwrap_or_default().to_ascii_lowercase();
            stats.presentation_tables |= role == "presentation" || role == "none";
        }
        if name == Some("tr") {
            let cells = kept
                .iter()
                .filter(|c| element_name(c).is_some_and(|n| n == "td" || n == "th"))
                .count();
            stats.cells += cells;
            stats.max_row_cells = stats.max_row_cells.max(cells);
        }
        for child in kept.into_iter().rev() {
            let q = quotes + usize::from(element_name(&child) == Some("blockquote"));
            stack.push((child, depth + 1, in_table || is_table, q));
        }
    }
    CleanReport {
        remote_images: stats.remote_images,
        trackers: stats.trackers,
        data_tables: stats.tables > 0
            && !stats.nested_tables
            && !stats.presentation_tables
            && stats.cells <= MAX_DATA_TABLE_CELLS
            && stats.max_row_cells >= 2,
    }
}

/// Replaces the children of `node` with the leaves of its subtree, in
/// document order, dropping the wrappers in between.
fn flatten(node: &Handle, stats: &mut Stats) {
    let mut leaves = Vec::new();
    let mut stack: Vec<Handle> = node.children.borrow().iter().rev().cloned().collect();
    while let Some(n) = stack.pop() {
        if let Verdict::Drop = judge(&n, stats) {
            continue;
        }
        let kids: Vec<Handle> = n.children.borrow().iter().rev().cloned().collect();
        if kids.is_empty() {
            // An empty cell or row hoisted out of its table makes html2text
            // hit `unimplemented!()`, so table parts never become leaves.
            if element_name(&n).is_some_and(|name| TABLE_PARTS.contains(&name)) {
                continue;
            }
            leaves.push(n);
        } else {
            stack.extend(kids);
        }
    }
    for leaf in &leaves {
        leaf.parent.set(Some(Rc::downgrade(node)));
    }
    *node.children.borrow_mut() = leaves;
}

fn element_name(node: &Handle) -> Option<&str> {
    match &node.data {
        html2text::Element { name, .. } => Some(&*name.local),
        _ => None,
    }
}

fn attr(node: &Handle, key: &str) -> Option<String> {
    match &node.data {
        html2text::Element { attrs, .. } => attrs
            .borrow()
            .iter()
            .find(|a| &*a.name.local == key)
            .map(|a| a.value.to_string()),
        _ => None,
    }
}

/// Decides whether a child stays, counting what it removes.
fn judge(node: &Handle, stats: &mut Stats) -> Verdict {
    let name = match &node.data {
        html2text::Comment { .. } => return Verdict::Drop,
        html2text::Element { name, .. } => &*name.local,
        _ => return Verdict::Keep,
    };
    if DROPPED_ELEMENTS.contains(&name) {
        return Verdict::Drop;
    }
    if is_hidden(node) {
        // Images in hidden elements still load in a browser, which is
        // exactly what a tracker wants.
        stats.trackers += count_remote_images(node);
        return Verdict::Drop;
    }
    if name == "img" {
        match classify_image(node) {
            Image::Tracker => {
                stats.trackers += 1;
                return Verdict::Drop;
            }
            Image::Invisible => return Verdict::Drop,
            Image::Remote => stats.remote_images += 1,
            Image::Local => {}
        }
    }
    if attr(node, "background").is_some_and(|b| is_remote(&b)) {
        stats.remote_images += 1;
    }
    if attr(node, "style").is_some_and(|s| style_has_remote_url(&s)) {
        stats.remote_images += 1;
    }
    Verdict::Keep
}

fn count_remote_images(node: &Handle) -> usize {
    let mut count = 0;
    let mut stack = vec![Rc::clone(node)];
    while let Some(n) = stack.pop() {
        if element_name(&n) == Some("img") && attr(&n, "src").is_some_and(|s| is_remote(&s)) {
            count += 1;
        }
        stack.extend(n.children.borrow().iter().cloned());
    }
    count
}

fn is_remote(url: &str) -> bool {
    let u = url.trim();
    let starts =
        |p: &str| u.len() >= p.len() && u.as_bytes()[..p.len()].eq_ignore_ascii_case(p.as_bytes());
    starts("http://") || starts("https://") || starts("//")
}

fn style_has_remote_url(style: &str) -> bool {
    let lower = style.to_ascii_lowercase();
    lower.match_indices("url(").any(|(p, _)| {
        let rest = lower[p + 4..].trim_start_matches([' ', '"', '\'']);
        is_remote(rest)
    })
}

/// Parsed CSS declarations of a `style` attribute, lowercased, without
/// whitespace or `!important`.
fn declarations(style: &str) -> Vec<(String, String)> {
    let compact: String = style
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase()
        .replace("!important", "");
    compact
        .split(';')
        .filter_map(|d| {
            let (k, v) = d.split_once(':')?;
            Some((k.to_owned(), v.to_owned()))
        })
        .collect()
}

/// True for a CSS length or number that is zero ("0", "0px", "0.0em").
fn is_zero(value: &str) -> bool {
    let num: String = value
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    !num.is_empty() && num.parse::<f64>().is_ok_and(|n| n == 0.0)
}

fn is_hidden(node: &Handle) -> bool {
    let html2text::Element { attrs, .. } = &node.data else {
        return false;
    };
    for a in attrs.borrow().iter() {
        match &*a.name.local {
            "hidden" => return true,
            "class" => {
                if a.value.to_ascii_lowercase().contains("preheader") {
                    return true;
                }
            }
            "style" => {
                let decls = declarations(&a.value);
                let has = |k: &str, pred: &dyn Fn(&str) -> bool| {
                    decls.iter().any(|(dk, dv)| dk == k && pred(dv))
                };
                if has("display", &|v| v == "none")
                    || has("visibility", &|v| v == "hidden")
                    || has("mso-hide", &|v| v == "all")
                    || has("opacity", &is_zero)
                {
                    return true;
                }
                let collapsed = has("max-height", &is_zero) || has("height", &is_zero);
                if collapsed && has("overflow", &|v| v == "hidden") {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

enum Image {
    Tracker,
    Invisible,
    Remote,
    Local,
}

/// Pixels from a `width` or `height` attribute or style value. Percentages
/// and junk give `None`.
fn pixels(value: &str) -> Option<u32> {
    let v = value.trim();
    let digits: String = v.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || v[digits.len()..].trim_start().starts_with('%') {
        return None;
    }
    digits.parse().ok().or(Some(u32::MAX))
}

fn classify_image(node: &Handle) -> Image {
    let src = attr(node, "src").unwrap_or_default();
    let src = src.trim();
    if src.is_empty() {
        return Image::Local;
    }
    let remote = is_remote(src);
    let decls = attr(node, "style")
        .map(|s| declarations(&s))
        .unwrap_or_default();
    let dim = |key: &str| {
        attr(node, key).and_then(|v| pixels(&v)).or_else(|| {
            decls
                .iter()
                .find(|(k, _)| k == key)
                .and_then(|(_, v)| pixels(v))
        })
    };
    let tiny = match (dim("width"), dim("height")) {
        (Some(w), Some(h)) => w <= 1 && h <= 1,
        (Some(d), None) | (None, Some(d)) => d <= 1,
        (None, None) => false,
    };
    let lower = src.to_ascii_lowercase();
    let tracker_url = TRACKER_HINTS.iter().any(|h| lower.contains(h));
    match (remote, tiny || tracker_url, tiny) {
        (true, true, _) => Image::Tracker,
        (false, _, true) => Image::Invisible,
        (true, false, _) => Image::Remote,
        _ => Image::Local,
    }
}

/// Elements that start a new line in the crude fallback.
const BLOCK_TAGS: &[&str] = &[
    "p",
    "div",
    "br",
    "tr",
    "li",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "blockquote",
    "table",
    "ul",
    "ol",
    "pre",
    "hr",
    "section",
    "article",
    "header",
    "footer",
];

/// Text from HTML without a parser: tags stripped, a few entities decoded,
/// block tags turned into line breaks. Used when html2text fails and for
/// the "is the plain part much shorter" estimate.
pub(crate) fn crude_text(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        push_text(&mut out, &rest[..lt]);
        rest = &rest[lt..];
        if rest.starts_with("<!--") {
            rest = rest.find("-->").map_or("", |e| &rest[e + 3..]);
            continue;
        }
        let Some(end) = rest.find('>') else {
            rest = "";
            break;
        };
        let tag = &rest[1..end];
        let closing = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| !c.is_ascii_alphanumeric())
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        rest = &rest[end + 1..];
        if !closing && matches!(name.as_str(), "script" | "style" | "head" | "title") {
            let close = format!("</{name}");
            rest = find_ci(rest, &close).map_or("", |p| &rest[p..]);
        }
        if BLOCK_TAGS.contains(&name.as_str()) {
            out.push('\n');
        }
    }
    push_text(&mut out, rest);
    let mut text = String::with_capacity(out.len());
    let mut blank_run = 0;
    for line in out.split('\n') {
        let line = line.trim();
        if line.is_empty() {
            blank_run += 1;
            if blank_run > 1 || text.is_empty() {
                continue;
            }
        } else {
            blank_run = 0;
        }
        text.push_str(line);
        text.push('\n');
    }
    text.trim_end().to_owned()
}

fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    let n = needle.len();
    hay.as_bytes()
        .windows(n)
        .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

/// Appends HTML text with entities decoded and whitespace collapsed.
fn push_text(out: &mut String, s: &str) {
    let mut rest = s;
    let push_char = |out: &mut String, c: char| {
        if c.is_whitespace() && c != '\u{A0}' {
            if !out.is_empty() && !out.ends_with([' ', '\n']) {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    };
    while let Some(amp) = rest.find('&') {
        for c in rest[..amp].chars() {
            push_char(out, c);
        }
        rest = &rest[amp..];
        let semi = rest.bytes().take(12).position(|b| b == b';');
        let decoded = semi.and_then(|e| decode_entity(&rest[1..e]).map(|c| (c, e)));
        match decoded {
            Some((c, e)) => {
                push_char(out, c);
                rest = &rest[e + 1..];
            }
            None => {
                push_char(out, '&');
                rest = &rest[1..];
            }
        }
    }
    for c in rest.chars() {
        push_char(out, c);
    }
}

fn decode_entity(name: &str) -> Option<char> {
    let named = match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{A0}'),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    let num = name.strip_prefix('#')?;
    let code = match num.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => num.parse().ok()?,
    };
    char::from_u32(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(html: &str) -> (CleanReport, String) {
        let cfg = config::with_decorator(MejlDecorator::new(false));
        let dom = cfg.parse_html(html.as_bytes()).expect("parse");
        let report = preclean(&dom.document);
        let tree = cfg.dom_to_render_tree(&dom).expect("tree");
        let text = config::with_decorator(MejlDecorator::new(false))
            .allow_width_overflow()
            .render_to_lines(tree, 80)
            .expect("render")
            .iter()
            .map(|l| l.tagged_strings().map(|t| t.s.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        (report, text)
    }

    #[test]
    fn drops_hidden_and_scripted_content() {
        let (_, text) = clean(
            r#"<head><title>T</title><style>p{}</style></head><body>
            <div style="display: none !important">preheader</div>
            <div style="DISPLAY:NONE">x1</div>
            <span style="mso-hide:all">x2</span>
            <div style="max-height:0px; overflow:hidden">x3</div>
            <div style="opacity:0">x4</div>
            <div hidden>x5</div>
            <div class="email-preheader">x6</div>
            <script>alert(1)</script><!-- comment -->
            <p>visible</p></body>"#,
        );
        assert_eq!(text.trim(), "visible");
    }

    #[test]
    fn keeps_layout_hacks_that_are_not_hidden() {
        let (_, text) = clean(
            r#"<div style="font-size:0"><span style="font-size:14px">kept</span></div><div style="opacity:0.5">half</div>"#,
        );
        assert!(text.contains("kept"));
        assert!(text.contains("half"));
    }

    #[test]
    fn counts_and_removes_trackers() {
        let (report, text) = clean(
            r#"<img src="https://t.example.com/o.gif" width="1" height="1">
            <img src="https://t.example.com/open?id=3" alt="">
            <img src="https://img.example.com/logo.png" alt="Larkspur logo">
            <img src="cid:part1" alt="inline">
            <img src="https://img.example.com/spacer.png" style="width:1px;height:1px">
            <div style="display:none"><img src="https://t.example.com/x.png"></div>
            <table><tr><td background="https://img.example.com/bg.png">cell</td></tr></table>"#,
        );
        assert_eq!(report.trackers, 4);
        assert_eq!(report.remote_images, 2);
        assert!(text.contains("[image: Larkspur logo]"));
        assert!(text.contains("[image: inline]"));
    }

    #[test]
    fn flattens_deep_nesting() {
        let html = format!("{}deep{}", "<div>".repeat(5000), "</div>".repeat(5000));
        let (_, text) = clean(&html);
        assert_eq!(text.trim(), "deep");
    }

    #[test]
    fn flattening_never_strands_table_parts() {
        let html = format!(
            "{}x{}",
            "<table><tr><td>a</td><td></td><td>".repeat(400),
            "</td></tr></table>".repeat(400)
        );
        let doc = HtmlDoc::new(&html).expect("prepare");
        let settings = crate::Settings {
            fold_quotes: true,
            link_style: LinkStyle::Footnotes,
        };
        let out = doc.render(80, settings);
        assert!(!out.fallback);
    }

    #[test]
    fn detects_data_and_layout_tables() {
        let (r, _) =
            clean("<table><tr><th>a</th><th>b</th></tr><tr><td>1</td><td>2</td></tr></table>");
        assert!(r.data_tables);
        let (r, _) =
            clean("<table><tr><td><table><tr><td>a</td><td>b</td></tr></table></td></tr></table>");
        assert!(!r.data_tables);
        let (r, _) = clean(r#"<table role="presentation"><tr><td>a</td><td>b</td></tr></table>"#);
        assert!(!r.data_tables);
        let (r, _) = clean("<table><tr><td>only</td></tr></table>");
        assert!(!r.data_tables);
    }

    #[test]
    fn crude_text_strips_tags_and_decodes() {
        let text = crude_text(
            "<html><head><style>p{color:red}</style></head><body><p>Fish &amp; chips&nbsp;&#169; &#x263A;</p><!-- x --><SCRIPT>bad()</SCRIPT><div>two\n   words</div>&bogus; <b",
        );
        assert_eq!(text, "Fish & chips\u{A0}© ☺\n\ntwo words\n&bogus;");
    }

    #[test]
    fn guarded_turns_panics_into_none() {
        let r: Option<u8> = guarded(|| panic!("boom"));
        assert_eq!(r, None);
        assert_eq!(guarded(|| Some(3)), Some(3));
    }

    #[test]
    fn pixels_parses_lengths() {
        assert_eq!(pixels("1"), Some(1));
        assert_eq!(pixels(" 0px"), Some(0));
        assert_eq!(pixels("100%"), None);
        assert_eq!(pixels("auto"), None);
        assert_eq!(pixels("99999999999"), Some(u32::MAX));
    }
}
