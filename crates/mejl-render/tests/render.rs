//! Public API tests: snapshots of real-looking (fictional) mail, hostile
//! input, and a width property test.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use mejl_render::{
    BodySource, FoldKind, LinkStyle, PreparedBody, RenderOptions, RenderedBody, SpanKind,
    SpanStyle, choose_part, render_auto, render_html, render_plain,
};

const ORDER: &str = include_str!("fixtures/order.html");
const NEWSLETTER: &str = include_str!("fixtures/newsletter.html");
const REPLY: &str = include_str!("fixtures/reply.txt");

fn opts(width: usize) -> RenderOptions {
    RenderOptions {
        width,
        ..RenderOptions::default()
    }
}

fn tag(style: SpanStyle) -> String {
    let mut t = match style.kind {
        SpanKind::Normal => String::new(),
        SpanKind::Heading => "h".into(),
        SpanKind::Link(i) => format!("l{i}"),
        SpanKind::FootnoteMarker(i) => format!("m{i}"),
        SpanKind::Quote(d) => format!("q{d}"),
        SpanKind::Signature => "s".into(),
        SpanKind::Code => "c".into(),
        SpanKind::TableRule => "r".into(),
    };
    if style.bold {
        t.push('*');
    }
    if style.italic {
        t.push('/');
    }
    if style.strikethrough {
        t.push('~');
    }
    if style.dim {
        t.push('.');
    }
    t
}

/// Checks the invariants every rendering must hold.
fn check(body: &RenderedBody) {
    for (i, line) in body.lines.iter().enumerate() {
        assert!(
            line.width() <= body.width,
            "line {i} is {} wide at width {}: {:?}",
            line.width(),
            body.width,
            line.text()
        );
        for span in &line.spans {
            assert!(!span.text.is_empty(), "empty span on line {i}");
            assert!(
                !span.text.chars().any(char::is_control),
                "control character on line {i}: {:?}",
                span.text
            );
            if let SpanKind::Link(n) | SpanKind::FootnoteMarker(n) = span.style.kind {
                assert!(n >= 1 && n <= body.links.len(), "bad link index {n}");
            }
        }
        for pair in line.spans.windows(2) {
            assert_ne!(pair[0].style, pair[1].style, "unmerged spans on line {i}");
        }
    }
    for (i, link) in body.links.iter().enumerate() {
        assert_eq!(link.index, i + 1);
    }
    let mut end = 0;
    for fold in &body.folds {
        assert!(fold.start_line >= end, "folds overlap");
        assert!(fold.line_count > 0);
        end = fold.start_line + fold.line_count;
        assert!(end <= body.lines.len(), "fold past the end");
    }
}

/// A compact, reviewable text form of a rendering. Styled spans show as
/// `{tag:text}`: h heading, lN link, mN marker, qN quote, s signature,
/// c code, r table rule; then * bold, / italic, ~ strike, . dim.
fn dump(body: &RenderedBody) -> String {
    check(body);
    let n = body.notices;
    let mut out = format!(
        "width {} | source {:?} | remote images {} | trackers {} | truncated {} | fallback {}\n",
        body.width,
        n.source,
        n.remote_images_blocked,
        n.trackers_blocked,
        n.truncated,
        n.html_fallback
    );
    out.push_str("--- lines\n");
    for (i, line) in body.lines.iter().enumerate() {
        let mut text = String::new();
        for span in &line.spans {
            let t = tag(span.style);
            if t.is_empty() {
                text.push_str(&span.text);
            } else {
                let _ = write!(text, "{{{t}:{}}}", span.text);
            }
        }
        let _ = writeln!(out, "{i:3} |{text}");
    }
    out.push_str("--- folds\n");
    for f in &body.folds {
        let _ = writeln!(
            out,
            "{:?} lines {}..{}: {}",
            f.kind,
            f.start_line,
            f.start_line + f.line_count,
            f.summary
        );
    }
    out.push_str("--- links\n");
    for l in &body.links {
        let _ = writeln!(out, "[{}] {} | {}", l.index, l.url, l.text);
    }
    out
}

#[test]
fn order_email_with_table_and_links() {
    let body = render_html(ORDER, &opts(72));
    insta::assert_snapshot!(dump(&body));
    assert_eq!(body.notices.trackers_blocked, 1);
    assert_eq!(body.notices.remote_images_blocked, 1);
    assert_eq!(body.links.len(), 3);
    let quote = body
        .folds
        .iter()
        .find(|f| f.kind == FoldKind::Quote)
        .expect("quote fold");
    assert!(
        quote.summary.ends_with("from Mira Holt, Oct 21"),
        "{}",
        quote.summary
    );
}

#[test]
fn order_email_narrow() {
    let body = render_html(ORDER, &opts(30));
    insta::assert_snapshot!(dump(&body));
}

#[test]
fn newsletter_with_preheader_and_trackers() {
    let body = render_html(NEWSLETTER, &opts(64));
    insta::assert_snapshot!(dump(&body));
    let text: String = body.lines.iter().map(|l| l.text() + "\n").collect();
    assert!(!text.contains("plus 20% off bulbs"), "preheader leaked");
    assert!(!text.contains("Outlook only"), "conditional comment leaked");
    assert!(!text.contains("javascript"), "unsafe link shown");
    assert_eq!(body.notices.trackers_blocked, 3);
    assert_eq!(body.notices.remote_images_blocked, 3);
    // The same URL twice gets one footnote.
    assert_eq!(body.links.len(), 2);
}

#[test]
fn plain_reply_chain_with_nested_quotes_and_signature() {
    let body = render_plain(REPLY, &opts(60));
    insta::assert_snapshot!(dump(&body));
    let kinds: Vec<FoldKind> = body.folds.iter().map(|f| f.kind).collect();
    assert_eq!(kinds, [FoldKind::Signature, FoldKind::Quote]);
    assert_eq!(body.folds[0].summary, "signature, 3 lines");
    assert!(body.folds[1].summary.ends_with("from Mira Holt, Oct 19"));
}

#[test]
fn plain_reply_with_inline_links_and_no_quote_folds() {
    let options = RenderOptions {
        width: 60,
        fold_quotes: false,
        link_style: LinkStyle::Inline,
        ..RenderOptions::default()
    };
    let body = render_plain(REPLY, &options);
    check(&body);
    assert!(body.folds.iter().all(|f| f.kind == FoldKind::Signature));
    assert!(!body.lines.iter().any(|l| l.text().contains("[1]")));
    assert_eq!(body.links.len(), 1);
    assert!(
        body.lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.style.kind == SpanKind::Link(1)))
    );
}

#[test]
fn very_long_urls() {
    let url = format!(
        "https://click.example.com/ls/click?upn={}&data={}",
        "aB3dE6gH9".repeat(30),
        "x".repeat(150)
    );
    let text = format!("Confirm here: {url}\nThanks.");
    let body = render_plain(&text, &opts(40));
    insta::assert_snapshot!(dump(&body));
    assert_eq!(body.links[0].url, url);

    let html = format!(r#"<p>Confirm <a href="{url}">{url}</a> today</p>"#);
    let body = render_html(&html, &opts(40));
    check(&body);
    assert_eq!(body.links[0].url, url);
}

#[test]
fn cjk_and_emoji_width() {
    let text = "日本語のテキストは全角で表示されます。折り返しは二桁の幅を守ります。\n\
                Family 👨‍👩‍👧 flags 🇸🇪🇯🇵 thumbs 👍🏽 heart ❤️ accents e\u{301}e\u{301}e\u{301}\n\
                中文混合English文本测试";
    for width in [1, 2, 3, 7, 10, 25] {
        let body = render_plain(text, &opts(width));
        check(&body);
        // No grapheme lost or split: the text survives the wrap, apart
        // from spaces at wrap points, unless a grapheme is wider than the
        // whole line.
        if width >= 2 {
            let joined: String = body.lines.iter().map(|l| l.text()).collect();
            let expected: String = text.chars().filter(|c| *c != ' ' && *c != '\n').collect();
            let got: String = joined.chars().filter(|c| *c != ' ').collect();
            assert_eq!(got, expected, "width {width}");
        }
    }
    insta::assert_snapshot!(dump(&render_plain(text, &opts(12))));
    let html = format!("<p>{}</p>", text.replace('\n', "<br>"));
    for width in [1, 2, 5, 12, 40] {
        check(&render_html(&html, &opts(width)));
    }
}

#[test]
fn rtl_text_does_not_crash() {
    let text = "שלום עולם, זהו טקסט בעברית.\nمرحبا بالعالم https://example.com/ar هذا نص عربي\n\u{202E}reversed\u{202C} and \u{2067}isolated\u{2069}";
    for width in [1, 4, 9, 30, 80] {
        let body = render_plain(text, &opts(width));
        check(&body);
        let all: String = body.lines.iter().map(|l| l.text()).collect();
        assert!(!all.contains('\u{202E}'), "bidi override kept");
        let html = format!("<p dir=\"rtl\">{}</p>", text.replace('\n', "<br>"));
        check(&render_html(&html, &opts(width)));
    }
    insta::assert_snapshot!(dump(&render_plain(text, &opts(30))));
}

/// Runs `f` on a thread with the stack size tokio gives blocking tasks.
fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(f)
        .expect("spawn")
        .join()
        .expect("render thread panicked")
}

#[test]
fn deeply_nested_html_does_not_overflow_or_hang() {
    let started = Instant::now();
    let shapes = [
        ("<div>", "</div>"),
        ("<blockquote>", "</blockquote>"),
        ("<table><tr><td>", "</td></tr></table>"),
        ("<ul><li>", "</li></ul>"),
        ("<b><i>", "</i></b>"),
        ("<a href=\"https://example.com/n\">", "</a>"),
    ];
    for (open, close) in shapes {
        // As deep as fits under the default input cap, so the text survives.
        let n = (900_000 / open.len()).min(50_000);
        let html = format!("{}inner text{}", open.repeat(n), close.repeat(n));
        let lines = on_small_stack(move || {
            let prepared = PreparedBody::html(&html, &opts(80));
            let first = prepared.render(80);
            let again = prepared.render(33);
            check(&first);
            check(&again);
            first.lines.len()
        });
        assert!(lines > 0, "{open}: nothing rendered");
    }
    assert!(started.elapsed() < Duration::from_secs(30));
}

#[test]
fn malformed_html_does_not_panic() {
    let samples = [
        "<p>unclosed <b>bold <i>italic",
        "</div></div></p>stray closers",
        "<table><tr><td rowspan=\"999999\" colspan=\"999999\">huge span</td></tr></table>",
        "<table><td>no rows<td>still<tr><th>",
        "<a href=\"https://example.com\"><a href=\"https://example.org\">nested</a></a>",
        "<<<>>><p>>>> &amp &#xFFFFFFFF; &#0; &#55296; &bogus</p>",
        "<img src=x onerror=alert(1)><svg><title>t</title></svg>",
        "<style>p { color: red; ;;; {{{ </style><p style=\"color:\">x</p>",
        "<p>\u{0}\u{1b}[31mred\u{1b}[0m &#27;[2J</p>",
        "<pre>\tcode\twith\ttabs</pre>",
        "<h1>a<h2>b<h3>c</h1>",
        "<blockquote><h1>Quoted heading</h1><pre>pre</pre></blockquote>",
        "",
        "<!-- only a comment -->",
        "<html><head></head><body></body></html>",
    ];
    for html in samples {
        for width in [1, 2, 5, 40, 120] {
            let body = render_html(html, &opts(width));
            check(&body);
            assert!(
                !body.lines.iter().any(|l| l.text().contains('\u{1b}')),
                "escape leaked for {html:?}"
            );
        }
    }
}

#[test]
fn huge_input_is_truncated() {
    let mut html = String::with_capacity(5 << 20);
    let mut n = 0;
    while html.len() < 5 << 20 {
        let _ = write!(
            html,
            "<p>Paragraph {n} with a few words of filler text.</p>"
        );
        n += 1;
    }
    let started = Instant::now();
    let body = render_html(&html, &opts(80));
    assert!(body.notices.truncated);
    assert!(!body.lines.is_empty());
    check(&body);
    assert!(started.elapsed() < Duration::from_secs(60));

    let plain = "All work and no play makes a dull message. ".repeat(130_000);
    assert!(plain.len() > 5 << 20);
    let body = render_plain(&plain, &opts(80));
    assert!(body.notices.truncated);
    check(&body);

    let small = RenderOptions {
        max_input_bytes: 10,
        ..opts(80)
    };
    let body = render_plain("héllo wörld and more", &small);
    assert!(body.notices.truncated);
    assert_eq!(body.lines[0].text(), "héllo wö");
}

#[test]
fn empty_bodies() {
    for width in [0, 1, 80] {
        for body in [
            render_plain("", &opts(width)),
            render_plain("\n\n   \n", &opts(width)),
            render_html("", &opts(width)),
            render_html("<p></p><div>   </div>", &opts(width)),
            render_auto(None, None, &opts(width)),
        ] {
            check(&body);
            assert!(body.lines.is_empty(), "{:?}", body.lines);
            assert!(body.links.is_empty());
            assert!(body.folds.is_empty());
            assert!(body.width >= 1);
        }
    }
}

#[test]
fn choose_part_prefers_real_plain_text() {
    assert_eq!(
        choose_part(Some("Thanks!"), Some("<p>Thanks!</p>")),
        Some(BodySource::Plain)
    );
    assert_eq!(
        choose_part(Some("https://example.com/view"), Some(ORDER)),
        Some(BodySource::Html)
    );
    assert_eq!(choose_part(Some(""), Some(ORDER)), Some(BodySource::Html));
    assert_eq!(choose_part(Some("Hi"), Some(ORDER)), Some(BodySource::Html));
    assert_eq!(
        choose_part(Some(REPLY), Some(ORDER)),
        Some(BodySource::Plain)
    );
    let body = render_auto(Some("https://example.com/view"), Some(ORDER), &opts(72));
    assert_eq!(body.notices.source, BodySource::Html);
    let body = render_auto(Some(REPLY), Some(ORDER), &opts(72));
    assert_eq!(body.notices.source, BodySource::Plain);
}

#[test]
fn links_in_tables_and_quotes_number_in_order() {
    let html = r#"<p><a href="https://a.example.com">A</a></p>
        <table><tr><td><a href="https://b.example.com">B</a></td><td><a href="https://c.example.com">C</a></td></tr>
        <tr><td><a href="https://a.example.com">A again</a></td><td>x</td></tr></table>
        <blockquote><blockquote><a href="https://d.example.com">D</a></blockquote></blockquote>"#;
    let body = render_html(html, &opts(60));
    check(&body);
    let urls: Vec<&str> = body.links.iter().map(|l| l.url.as_str()).collect();
    assert_eq!(
        urls,
        [
            "https://a.example.com",
            "https://b.example.com",
            "https://c.example.com",
            "https://d.example.com"
        ]
    );
    assert_eq!(body.links[0].text, "A");
    let markers: Vec<usize> = body
        .lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .filter_map(|s| match s.style.kind {
            SpanKind::FootnoteMarker(i) => Some(i),
            _ => None,
        })
        .collect();
    // Inline markers in document order, then the footnote list.
    assert_eq!(markers, [1, 2, 3, 1, 4, 1, 2, 3, 4]);
    let quoted = body
        .lines
        .iter()
        .find(|l| l.text().contains("D[4]"))
        .expect("quoted link line");
    assert!(quoted.text().starts_with(">> "), "{:?}", quoted.text());
    assert_eq!(quoted.spans[0].style.kind, SpanKind::Quote(2));
}

#[test]
fn author_text_starting_with_gt_is_not_a_quote() {
    let body = render_html("<p>&gt; not a quote</p>", &opts(40));
    assert_eq!(body.lines[0].spans[0].style.kind, SpanKind::Normal);
}

#[test]
fn headings_and_emphasis_map_to_styles() {
    let body = render_html(
        "<h2>Big <em>news</em></h2><p><strong>bold</strong> <s>gone</s> <code>x = 1</code></p>",
        &opts(40),
    );
    insta::assert_snapshot!(dump(&body));
}

/// A small deterministic xorshift generator, so failures reproduce.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

const PIECES: &[&str] = &[
    "word",
    "other",
    "a",
    " ",
    " ",
    " ",
    "  ",
    "\n",
    "\n",
    "\n\n",
    "> ",
    ">> ",
    "> > ",
    "\t",
    "\r\n",
    "\r",
    "-- \n",
    "--\n",
    "https://example.com/some/long/path?q=1&r=2",
    "http://x.example.org",
    "日本語",
    "中文字符",
    "👨‍👩‍👧",
    "🇸🇪",
    "👍🏽",
    "❤️",
    "e\u{301}",
    "שלום",
    "مرحبا",
    "\u{202E}",
    "\u{1b}[31m",
    "\u{0}",
    "Ａ",
    "supercalifragilisticexpialidocious",
    "On Mon, Oct 19, 2026 at 6:40 PM Ann Example <ann@example.com> wrote:\n",
    "From: Ann Example\nSent: Oct 19\nSubject: x\n",
    "[1]",
    "\u{FFFD}",
    "\u{200D}",
    "\u{AD}",
];

const TAGS: &[&str] = &[
    "<p>",
    "</p>",
    "<div>",
    "</div>",
    "<br>",
    "<b>",
    "</b>",
    "<i>",
    "</i>",
    "<blockquote>",
    "</blockquote>",
    "<table><tr><td>",
    "</td><td>",
    "</td></tr></table>",
    "<th>",
    "<ul><li>",
    "</li></ul>",
    "<pre>",
    "</pre>",
    "<h1>",
    "</h1>",
    "<a href=\"https://example.com/l\">",
    "</a>",
    "<img src=\"https://t.example.com/p.gif\" width=1 height=1>",
    "<img src=\"https://i.example.com/x.png\" alt=\"pic\">",
    "<span style=\"display:none\">",
    "</span>",
    "<code>",
    "</code>",
    "&amp;",
    "&#27;",
    "<!--",
    "-->",
    "<style>",
    "</style>",
];

#[test]
fn no_line_exceeds_the_width() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    // html2text should not need the crude fallback at normal widths.
    let mut fallbacks = Vec::new();
    for round in 0..400 {
        let mut text = String::new();
        for _ in 0..rng.below(60) {
            text.push_str(rng.pick(PIECES));
        }
        let width = match rng.below(4) {
            0 => rng.below(4),
            1 => 4 + rng.below(12),
            _ => 16 + rng.below(110),
        };
        let options = RenderOptions {
            width,
            fold_quotes: rng.below(2) == 0,
            link_style: if rng.below(3) == 0 {
                LinkStyle::Inline
            } else {
                LinkStyle::Footnotes
            },
            ..RenderOptions::default()
        };
        let body = render_plain(&text, &options);
        check(&body);

        let mut html = String::new();
        for _ in 0..rng.below(60) {
            if rng.below(2) == 0 {
                html.push_str(rng.pick(TAGS));
            } else {
                html.push_str(rng.pick(PIECES));
            }
        }
        let body = render_html(&html, &options);
        check(&body);
        if width >= 40 && body.notices.html_fallback {
            fallbacks.push(html.clone());
        }

        // Rewrapping a prepared body at another width keeps the invariant.
        if round % 10 == 0 {
            let prepared = PreparedBody::html(&html, &options);
            for w in [1, 3, 17, 61] {
                check(&prepared.render(w));
            }
        }
    }
    assert!(fallbacks.is_empty(), "html2text failed on {fallbacks:?}");
}
