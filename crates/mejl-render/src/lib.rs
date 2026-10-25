//! Message body rendering for the terminal.
//!
//! mejl-render turns a `text/plain` or `text/html` body into lines of
//! semantic spans. It knows nothing about ratatui or colours: the UI maps
//! each [`SpanStyle`] to its theme. Every rendered line fits the requested
//! width in display columns, measured per grapheme with `unicode-width` the
//! same way ratatui measures cells.
//!
//! The pipeline:
//!
//! - [`choose_part`] picks the part to show. Real `text/plain` wins; HTML is
//!   used when the plain part is missing, empty, only a URL, an "open the
//!   HTML version" placeholder, or much shorter than the HTML.
//! - Plain text is classified line by line (quote depth, attribution lines,
//!   the `-- ` signature delimiter), URLs become numbered links, and lines
//!   are wrapped by display width.
//! - HTML is parsed, cleaned (scripts, styles, hidden preheaders, comments
//!   and tracking pixels removed, remote images counted), converted with
//!   html2text and mapped to the same span styles.
//! - Both end with the same folding pass ([`Fold`]) and a footnote list of
//!   links ("[1] https://...").
//!
//! # Running it safely
//!
//! Rendering is synchronous and CPU bound. Callers run it on
//! `tokio::task::spawn_blocking` and wrap the join handle in
//! `tokio::time::timeout`. A timeout only stops the waiting: the blocking
//! task keeps running until it finishes, so the input size cap
//! ([`RenderOptions::max_input_bytes`]) is what bounds the work.
//!
//! html2text runs inside `std::panic::catch_unwind`. That relies on the
//! default `panic = "unwind"` profile setting. The default panic hook still
//! prints the message to stderr, which corrupts a TUI in raw mode, so the
//! UI should install a hook that logs instead. Stack overflow cannot be
//! caught at all, so the HTML cleaner flattens the document below a fixed
//! nesting depth before html2text sees it. Deep nesting is also cut before
//! parsing, because the HTML parser slows down quadratically with depth.
//! With the default 1 MiB cap, the worst shapes measured (one row with
//! 80 000 cells, 20 000 nested tables, 300 000 unclosed tags) prepare in
//! about 150 ms and render in about 90 ms in a release build.
//!
//! # Caching and rewrapping
//!
//! [`PreparedBody`] holds the width independent work (parsed and cleaned
//! HTML, classified plain lines). [`PreparedBody::render`] lays it out at a
//! width. Callers cache [`RenderedBody`] by `(email id, width, theme)` and
//! keep the `PreparedBody` around to rewrap on resize. Fold line numbers
//! refer to the lines of one rendering, so they change with the width.

mod classify;
mod html;
mod layout;
mod links;
mod nesting;
mod plain;
mod sanitize;
mod text;
mod width;

pub use sanitize::sanitize_for_browser;
use serde::{Deserialize, Serialize};

/// The semantic role of a span. The UI picks a theme style for each.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SpanKind {
    /// Body text.
    #[default]
    Normal,
    /// Heading text from HTML `<h1>` to `<h6>`. Themes usually show it bold.
    Heading,
    /// Link text or a footnote URL. The value is [`Link::index`].
    Link(usize),
    /// A footnote marker such as `[3]`. The value is [`Link::index`], so a
    /// click on the marker can open or copy the link.
    FootnoteMarker(usize),
    /// Quoted text, including the `>` prefix. The value is the quote depth,
    /// starting at 1.
    Quote(u8),
    /// Signature text, including the `-- ` delimiter line.
    Signature,
    /// Inline code or preformatted text.
    Code,
    /// Table borders and rules.
    TableRule,
}

/// The style of a span: one semantic kind plus emphasis flags.
///
/// The flags combine with any kind, so a bold link or an italic quote keeps
/// both pieces of information.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpanStyle {
    /// What the text is.
    pub kind: SpanKind,
    /// Strong emphasis.
    pub bold: bool,
    /// Emphasis.
    pub italic: bool,
    /// Struck out text.
    pub strikethrough: bool,
    /// Secondary text, such as an `[image: ...]` placeholder.
    pub dim: bool,
}

impl SpanStyle {
    /// A style with the given kind and no flags.
    pub const fn kind(kind: SpanKind) -> Self {
        SpanStyle {
            kind,
            bold: false,
            italic: false,
            strikethrough: false,
            dim: false,
        }
    }
}

/// A run of text with one style.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    /// The text. It never contains control characters or newlines.
    pub text: String,
    /// How the text should look.
    pub style: SpanStyle,
}

/// One rendered line.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyledLine {
    /// The spans, left to right. Adjacent spans have different styles.
    pub spans: Vec<Span>,
}

impl StyledLine {
    /// The text of the line without styles.
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }

    /// The width of the line in terminal columns, measured per grapheme.
    pub fn width(&self) -> usize {
        self.spans.iter().map(|s| width::str_width(&s.text)).sum()
    }

    /// True when the line has no visible text.
    pub fn is_blank(&self) -> bool {
        self.spans.iter().all(|s| s.text.trim().is_empty())
    }
}

/// A link found in the body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// The 1-based footnote number. Identical URLs share one number.
    pub index: usize,
    /// The target URL as written in the message, with control characters
    /// removed. Only `http`, `https`, `ftp`, `mailto` and `tel` links are
    /// kept.
    pub url: String,
    /// The visible link text of the first occurrence, whitespace collapsed
    /// and cut to a few hundred characters. For plain text it is the URL.
    pub text: String,
}

/// What a fold hides.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FoldKind {
    /// A quoted block, with its attribution line when there is one.
    Quote,
    /// A signature, from the `-- ` delimiter to its last line.
    Signature,
}

/// A range of lines the UI can collapse to a one-line marker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fold {
    /// What the range holds.
    pub kind: FoldKind,
    /// Index of the first hidden line in [`RenderedBody::lines`].
    pub start_line: usize,
    /// Number of hidden lines.
    pub line_count: usize,
    /// Marker text, for example "18 quoted lines from Mira Holt, Oct 21" or
    /// "signature, 3 lines". The counts are content lines: quoted lines
    /// without the attribution, signature lines without the delimiter.
    pub summary: String,
}

/// Which body part a rendering came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BodySource {
    /// The `text/plain` part.
    #[default]
    Plain,
    /// The `text/html` part.
    Html,
}

/// Facts about the rendering the reader shows as a notice line, such as
/// "remote images off, 2 trackers blocked".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyNotices {
    /// Remote images (and remote CSS backgrounds) that were not loaded.
    pub remote_images_blocked: usize,
    /// Tracking pixels removed: tiny or tracker-looking remote images, and
    /// remote images inside hidden elements.
    pub trackers_blocked: usize,
    /// The input was longer than [`RenderOptions::max_input_bytes`] and was
    /// cut.
    pub truncated: bool,
    /// The part that was rendered.
    pub source: BodySource,
    /// html2text failed or panicked, so the HTML was shown as crudely
    /// extracted text.
    pub html_fallback: bool,
}

/// A body ready for display.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedBody {
    /// The width the body was laid out for. No line is wider.
    pub width: usize,
    /// Body lines, followed by the footnote list when links are shown as
    /// footnotes.
    pub lines: Vec<StyledLine>,
    /// Links in footnote order. `links[i].index == i + 1`.
    pub links: Vec<Link>,
    /// Foldable ranges in line order. They never overlap.
    pub folds: Vec<Fold>,
    /// Notices for the reader.
    pub notices: BodyNotices,
}

/// How links appear in the text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LinkStyle {
    /// A `[n]` marker after the link text and a numbered URL list at the
    /// end. Works in every terminal and shows the real target.
    #[default]
    Footnotes,
    /// No markers and no list. Link text still carries [`SpanKind::Link`],
    /// for terminals where the UI emits OSC 8 hyperlinks.
    Inline,
}

/// Options for one rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderOptions {
    /// Target width in terminal columns. A width of 0 is treated as 1. The
    /// UI usually caps this near 100 columns for readability.
    pub width: usize,
    /// Render HTML whenever an HTML part exists (the reader's toggle key).
    pub prefer_html: bool,
    /// Produce folds for long quoted blocks. Signature folds are always
    /// produced.
    pub fold_quotes: bool,
    /// Bodies longer than this are cut at a character boundary and
    /// [`BodyNotices::truncated`] is set.
    pub max_input_bytes: usize,
    /// How links are shown.
    pub link_style: LinkStyle,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            width: 80,
            prefer_html: false,
            fold_quotes: true,
            max_input_bytes: DEFAULT_MAX_INPUT_BYTES,
            link_style: LinkStyle::Footnotes,
        }
    }
}

/// Default input cap: 1 MiB, far above normal mail and small enough that
/// html2text finishes well inside a reader frame budget on hostile input.
pub const DEFAULT_MAX_INPUT_BYTES: usize = 1 << 20;

/// Picks the part to render.
///
/// Returns `None` when there is neither part. Real `text/plain` wins. HTML
/// is chosen when the plain part is empty, consists only of URLs, is an
/// "open the HTML version" placeholder, or is very short while the HTML has
/// much more text. [`render_auto`] additionally honours
/// [`RenderOptions::prefer_html`].
pub fn choose_part(plain: Option<&str>, html: Option<&str>) -> Option<BodySource> {
    match (plain, html) {
        (None, None) => None,
        (Some(_), None) => Some(BodySource::Plain),
        (None, Some(_)) => Some(BodySource::Html),
        (Some(p), Some(h)) => {
            if plain::is_degenerate(p, h) {
                Some(BodySource::Html)
            } else {
                Some(BodySource::Plain)
            }
        }
    }
}

/// Renders a `text/plain` body at `options.width`.
pub fn render_plain(text: &str, options: &RenderOptions) -> RenderedBody {
    PreparedBody::plain(text, options).render(options.width)
}

/// Renders a `text/html` body at `options.width`.
pub fn render_html(html: &str, options: &RenderOptions) -> RenderedBody {
    PreparedBody::html(html, options).into_render(options.width)
}

/// Picks a part with [`choose_part`] (or HTML when
/// [`RenderOptions::prefer_html`] is set and HTML exists) and renders it.
/// With neither part the result is an empty body.
pub fn render_auto(
    plain: Option<&str>,
    html: Option<&str>,
    options: &RenderOptions,
) -> RenderedBody {
    PreparedBody::auto(plain, html, options).into_render(options.width)
}

/// The width independent part of a rendering, kept to rewrap cheaply.
///
/// For plain text this holds the classified, linkified lines, so a rewrap
/// only wraps. For HTML it holds the cleaned html2text render tree, so a
/// rewrap skips parsing and cleaning and only lays the tree out again.
/// `PreparedBody` is `Send`, so it can be built and rendered on a blocking
/// thread and moved back.
#[derive(Debug, Clone)]
pub struct PreparedBody {
    content: Content,
    notices: BodyNotices,
    settings: Settings,
}

#[derive(Debug, Clone)]
enum Content {
    Empty,
    Plain(plain::PlainDoc),
    Html(Box<html::HtmlDoc>),
}

/// The options that matter after preparation.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Settings {
    pub(crate) fold_quotes: bool,
    pub(crate) link_style: LinkStyle,
}

impl PreparedBody {
    /// Prepares a `text/plain` body.
    pub fn plain(text: &str, options: &RenderOptions) -> Self {
        let (text, truncated) = text::truncate(text, options.max_input_bytes);
        let settings = settings(options);
        PreparedBody {
            content: Content::Plain(plain::PlainDoc::new(text, settings)),
            notices: BodyNotices {
                truncated,
                source: BodySource::Plain,
                ..BodyNotices::default()
            },
            settings,
        }
    }

    /// Prepares a `text/html` body: size cap, parse, clean, build the
    /// render tree. If html2text fails or panics, the body falls back to
    /// crudely extracted text and [`BodyNotices::html_fallback`] is set.
    pub fn html(html: &str, options: &RenderOptions) -> Self {
        let (html, truncated) = text::truncate(html, options.max_input_bytes);
        let settings = settings(options);
        let (content, mut notices) = match html::HtmlDoc::new(html) {
            Ok(doc) => {
                let notices = doc.notices();
                (Content::Html(Box::new(doc)), notices)
            }
            Err(()) => {
                let fallback = html::crude_text(html);
                let notices = BodyNotices {
                    html_fallback: true,
                    ..BodyNotices::default()
                };
                (
                    Content::Plain(plain::PlainDoc::new(&fallback, settings)),
                    notices,
                )
            }
        };
        notices.truncated = truncated;
        notices.source = BodySource::Html;
        PreparedBody {
            content,
            notices,
            settings,
        }
    }

    /// Prepares whichever part [`render_auto`] would pick.
    pub fn auto(plain: Option<&str>, html: Option<&str>, options: &RenderOptions) -> Self {
        let source = match html {
            Some(_) if options.prefer_html => Some(BodySource::Html),
            _ => choose_part(plain, html),
        };
        match (source, plain, html) {
            (Some(BodySource::Plain), Some(p), _) => PreparedBody::plain(p, options),
            (Some(BodySource::Html), _, Some(h)) => PreparedBody::html(h, options),
            _ => PreparedBody {
                content: Content::Empty,
                notices: BodyNotices::default(),
                settings: settings(options),
            },
        }
    }

    /// The notices that do not depend on width.
    pub fn notices(&self) -> BodyNotices {
        self.notices
    }

    /// Lays the body out at `width` columns.
    pub fn render(&self, width: usize) -> RenderedBody {
        let width = width.max(1);
        match &self.content {
            Content::Empty => self.finish(width, Vec::new(), Vec::new(), Vec::new(), false),
            Content::Plain(doc) => {
                let (lines, links, folds) = doc.render(width, self.settings);
                self.finish(width, lines, links, folds, false)
            }
            Content::Html(doc) => {
                let out = doc.render(width, self.settings);
                self.finish(width, out.lines, out.links, out.folds, out.fallback)
            }
        }
    }

    /// Like [`PreparedBody::render`], but consumes the body so the HTML
    /// render tree is not copied.
    fn into_render(self, width: usize) -> RenderedBody {
        match self.content {
            Content::Html(doc) => {
                let width = width.max(1);
                let out = doc.render_owned(width, self.settings);
                let mut notices = self.notices;
                notices.html_fallback |= out.fallback;
                RenderedBody {
                    width,
                    lines: out.lines,
                    links: out.links,
                    folds: out.folds,
                    notices,
                }
            }
            _ => self.render(width),
        }
    }

    fn finish(
        &self,
        width: usize,
        lines: Vec<StyledLine>,
        links: Vec<Link>,
        folds: Vec<Fold>,
        fallback: bool,
    ) -> RenderedBody {
        let mut notices = self.notices;
        notices.html_fallback |= fallback;
        RenderedBody {
            width,
            lines,
            links,
            folds,
            notices,
        }
    }
}

fn settings(options: &RenderOptions) -> Settings {
    Settings {
        fold_quotes: options.fold_quotes,
        link_style: options.link_style,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}

    #[test]
    fn prepared_body_and_output_are_send() {
        assert_send::<PreparedBody>();
        assert_send::<RenderedBody>();
    }

    #[test]
    fn choose_part_without_parts_is_none() {
        assert_eq!(choose_part(None, None), None);
        assert_eq!(choose_part(Some("hi"), None), Some(BodySource::Plain));
        assert_eq!(choose_part(None, Some("<p>hi</p>")), Some(BodySource::Html));
    }

    #[test]
    fn render_auto_without_parts_is_empty() {
        let body = render_auto(None, None, &RenderOptions::default());
        assert!(body.lines.is_empty());
        assert!(body.links.is_empty());
        assert!(body.folds.is_empty());
    }

    #[test]
    fn prefer_html_forces_html() {
        let options = RenderOptions {
            prefer_html: true,
            ..RenderOptions::default()
        };
        let body = render_auto(
            Some("A real plain text part with enough words in it."),
            Some("<p>HTML version</p>"),
            &options,
        );
        assert_eq!(body.notices.source, BodySource::Html);
        assert_eq!(body.lines[0].text(), "HTML version");
    }

    #[test]
    fn rewrap_matches_fresh_render() {
        let text = "> one two three four five six seven\n> eight nine ten\n\nSee https://example.com/a for more, thanks.";
        let options = RenderOptions::default();
        let prepared = PreparedBody::plain(text, &options);
        for width in [5, 20, 33, 80] {
            let fresh = render_plain(text, &RenderOptions { width, ..options });
            assert_eq!(prepared.render(width), fresh);
        }
    }

    #[test]
    fn html_rewrap_matches_fresh_render() {
        let html = "<h1>Hi</h1><p>Some <a href=\"https://example.com/x\">linked text</a> here.</p>";
        let options = RenderOptions::default();
        let prepared = PreparedBody::html(html, &options);
        for width in [3, 20, 80] {
            let fresh = render_html(html, &RenderOptions { width, ..options });
            assert_eq!(prepared.render(width), fresh);
        }
    }
}
