//! Themes: semantic style names mapped to ratatui styles, plus glyphs.
//!
//! Widgets never pick colours. They ask for a [`StyleName`] such as
//! `list.unread` or `status.error`, and the theme decides how it looks.
//! Because the names carry meaning, a theme cannot drop a state
//! distinction by accident, and state never depends on colour alone:
//! unread is bold plus a glyph, pinned is a glyph, errors carry a word.
//!
//! Built-in themes:
//!
//! - `terminal` (default): the 16 ANSI colours plus the terminal's own
//!   foreground and background, bold, and reverse video for the cursor row.
//!   No background fills, so it works on light and dark terminals.
//! - `mono`: no colour at all; bold, dim, underline and reverse only.
//!   `NO_COLOR` forces it.
//! - `high-contrast`: default colours only, no dim text, reverse video for
//!   selection.
//! - `dark` and `light`: truecolor palettes matching the mock-ups.
//!
//! A TOML theme starts from a built-in one and replaces styles by name:
//!
//! ```toml
//! base = "terminal"
//! ascii = false
//! [styles]
//! "list.unread" = { fg = "yellow", bold = true }
//! "list.cursor" = { bg = "#252932" }
//! ```

use std::fmt;

use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use thiserror::Error;

macro_rules! style_names {
    ($($variant:ident => $name:literal, $doc:literal;)*) => {
        /// A semantic style.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum StyleName {
            $(
                #[doc = $doc]
                $variant,
            )*
        }

        impl StyleName {
            /// Every style name.
            pub const ALL: &'static [StyleName] = &[$(StyleName::$variant),*];

            /// The name used in theme files.
            pub fn name(self) -> &'static str {
                match self { $(StyleName::$variant => $name,)* }
            }
        }
    };
}

style_names! {
    Text => "text", "Plain text.";
    Dim => "dim", "Secondary text.";
    Brand => "app.brand", "The mejl tag in the top line.";
    TopMailbox => "app.mailbox", "Mailbox name in the top line.";
    TopInfo => "app.info", "Account, counts and preset in the top line.";
    Border => "pane.border", "Pane borders.";
    PaneTitle => "pane.title", "Title of an unfocused pane.";
    PaneTitleFocused => "pane.title.focused", "Title of the focused pane.";
    ScrollThumb => "scrollbar.thumb", "Scrollbar thumb on a pane border.";
    SidebarItem => "sidebar.item", "A mailbox name.";
    SidebarActive => "sidebar.active", "The mailbox being viewed.";
    SidebarCount => "sidebar.count", "Unread count of a mailbox.";
    SidebarHeading => "sidebar.heading", "Folders, Saved searches, Tools.";
    SidebarCursor => "sidebar.cursor", "The sidebar cursor row.";
    ListRow => "list.row", "A read conversation.";
    ListUnread => "list.unread", "Sender and subject of an unread conversation.";
    ListUnreadMarker => "list.marker.unread", "The unread glyph.";
    ListPinnedMarker => "list.marker.pinned", "The pinned glyph.";
    ListMaskedMarker => "list.marker.masked", "The Masked Email glyph.";
    ListSelectedMarker => "list.marker.selected", "The selection glyph.";
    ListPreview => "list.preview", "Preview text after the subject.";
    ListDate => "list.date", "Date of a read conversation.";
    ListDateUnread => "list.date.unread", "Date of an unread conversation.";
    ListGroup => "list.group", "Today, Yesterday, This week, Earlier.";
    ListCursor => "list.cursor", "Patched over the cursor row.";
    ListSkeleton => "list.skeleton", "A row that is not loaded yet.";
    ReaderInfo => "reader.info", "Conversation summary line.";
    ReaderMarker => "reader.marker", "Expanded and collapsed message markers.";
    ReaderSender => "reader.sender", "Sender name of an expanded message.";
    ReaderAddress => "reader.address", "Addresses and recipient lines.";
    ReaderDate => "reader.date", "Date of an expanded message.";
    ReaderCollapsed => "reader.collapsed", "Sender and date of a collapsed message.";
    ReaderSnippet => "reader.snippet", "Snippet of a collapsed message.";
    ReaderFocus => "reader.focus", "Patched over the focused message's header.";
    ReaderBody => "reader.body", "Body text.";
    ReaderHeading => "reader.heading", "Headings in HTML mail.";
    ReaderLink => "reader.link", "Link text and footnote URLs.";
    ReaderFootnote => "reader.footnote", "Footnote markers such as `[1]`.";
    ReaderQuote1 => "reader.quote.1", "Quoted text, depth 1.";
    ReaderQuote2 => "reader.quote.2", "Quoted text, depth 2.";
    ReaderQuote3 => "reader.quote.3", "Quoted text, depth 3.";
    ReaderQuote4 => "reader.quote.4", "Quoted text, depth 4 and deeper.";
    ReaderSignature => "reader.signature", "Signatures.";
    ReaderCode => "reader.code", "Code and preformatted text.";
    ReaderRule => "reader.rule", "Table rules.";
    ReaderFold => "reader.fold", "Fold markers for quotes and signatures.";
    ReaderNotice => "reader.notice", "Remote content and tracker notices.";
    ReaderMasked => "reader.masked", "Masked Email addresses and badges.";
    ReaderAttachment => "reader.attachment", "Attachment names.";
    ReaderHeaderName => "reader.header.name", "Header names in the full headers view.";
    StatusText => "status.text", "Status line text.";
    StatusSelection => "status.selection", "The selection counter.";
    StatusOk => "status.ok", "Synced, saved, done.";
    StatusWarning => "status.warning", "Offline, pending, prompts.";
    StatusError => "status.error", "Failures.";
    HintKey => "hint.key", "A key; clickable in the hint bar.";
    HintLabel => "hint.label", "Text next to a key.";
    PopupBorder => "popup.border", "Popup borders.";
    PopupTitle => "popup.title", "Popup titles.";
    PopupGroup => "popup.group", "Group headings in popups.";
    PopupCursor => "popup.cursor", "Highlighted popup row.";
    HelpMatch => "help.match", "Filter matches in help.";
}

impl fmt::Display for StyleName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl StyleName {
    /// Looks a style up by theme file name.
    pub fn from_name(name: &str) -> Option<StyleName> {
        StyleName::ALL.iter().copied().find(|s| s.name() == name)
    }

    /// The quote style for a depth starting at 1.
    pub fn quote(depth: u8) -> StyleName {
        match depth {
            0 | 1 => StyleName::ReaderQuote1,
            2 => StyleName::ReaderQuote2,
            3 => StyleName::ReaderQuote3,
            _ => StyleName::ReaderQuote4,
        }
    }
}

/// Indicator glyphs. The Unicode set uses only the Neutral East Asian
/// width class (one cell in every locale); the ASCII set is the fallback
/// for terminals and fonts that disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyphs {
    /// Unread conversation.
    pub unread: &'static str,
    /// Pinned conversation.
    pub pinned: &'static str,
    /// Delivered to a Masked Email address.
    pub masked: &'static str,
    /// Selected row.
    pub selected: &'static str,
    /// Success.
    pub ok: &'static str,
    /// Failure or disabled.
    pub fail: &'static str,
    /// Collapsed item, cursor, current item.
    pub collapsed: &'static str,
    /// Expanded item, dropdown.
    pub expanded: &'static str,
    /// Truncated text and folded blocks.
    pub ellipsis: &'static str,
    /// Scrollbar thumb.
    pub thumb: &'static str,
    /// Breadcrumb separator.
    pub crumb: &'static str,
}

impl Glyphs {
    /// The safe Unicode set from the mock-ups.
    pub const UNICODE: Glyphs = Glyphs {
        unread: "\u{25E6}",
        pinned: "\u{2691}",
        masked: "@",
        selected: "\u{2713}",
        ok: "\u{2713}",
        fail: "\u{2717}",
        collapsed: "\u{25B8}",
        expanded: "\u{25BE}",
        ellipsis: "\u{22EF}",
        thumb: "\u{2503}",
        crumb: ">",
    };

    /// Plain ASCII.
    pub const ASCII: Glyphs = Glyphs {
        unread: "*",
        pinned: "!",
        masked: "@",
        selected: "+",
        ok: "+",
        fail: "x",
        collapsed: ">",
        expanded: "v",
        ellipsis: "~",
        thumb: "#",
        crumb: ">",
    };
}

/// Line drawing characters for borders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lines {
    /// Horizontal line.
    pub horizontal: &'static str,
    /// Vertical line.
    pub vertical: &'static str,
    /// Corners: top left, top right, bottom left, bottom right.
    pub corners: [&'static str; 4],
    /// Tees: down (top edge), up (bottom edge), right (left edge), left
    /// (right edge).
    pub tees: [&'static str; 4],
    /// Can ratatui merge these where borders meet?
    pub mergeable: bool,
}

impl Lines {
    /// Box drawing characters.
    pub const BOX: Lines = Lines {
        horizontal: "\u{2500}",
        vertical: "\u{2502}",
        corners: ["\u{250C}", "\u{2510}", "\u{2514}", "\u{2518}"],
        tees: ["\u{252C}", "\u{2534}", "\u{251C}", "\u{2524}"],
        mergeable: true,
    };

    /// ASCII lines.
    pub const ASCII: Lines = Lines {
        horizontal: "-",
        vertical: "|",
        corners: ["+", "+", "+", "+"],
        tees: ["+", "+", "+", "+"],
        mergeable: false,
    };

    /// As a ratatui border set, for popups drawn with `Block`.
    pub fn border_set(&self) -> border::Set<'static> {
        border::Set {
            top_left: self.corners[0],
            top_right: self.corners[1],
            bottom_left: self.corners[2],
            bottom_right: self.corners[3],
            vertical_left: self.vertical,
            vertical_right: self.vertical,
            horizontal_top: self.horizontal,
            horizontal_bottom: self.horizontal,
        }
    }
}

/// A complete theme.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    name: String,
    styles: Vec<Style>,
    /// Indicator glyphs.
    pub glyphs: Glyphs,
    /// Border characters.
    pub lines: Lines,
}

/// A problem in a theme file.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ThemeError {
    /// Not valid TOML.
    #[error("the theme is not valid TOML: {0}")]
    Toml(String),
    /// Unknown built-in theme.
    #[error("unknown theme \"{0}\"; built in are terminal, mono, high-contrast, dark and light")]
    UnknownTheme(String),
    /// Unknown style name.
    #[error("unknown style \"{0}\"")]
    UnknownStyle(String),
    /// A style value that is not a table of attributes.
    #[error("style \"{name}\": {reason}")]
    BadStyle {
        /// Style name.
        name: String,
        /// What is wrong.
        reason: String,
    },
}

impl Default for Theme {
    fn default() -> Self {
        Theme::terminal()
    }
}

fn s() -> Style {
    Style::new()
}

impl Theme {
    /// Built-in theme names.
    pub const BUILT_IN: [&'static str; 5] = ["terminal", "mono", "high-contrast", "dark", "light"];

    /// A built-in theme by name. With `no_color` (the `NO_COLOR`
    /// convention) every name gives `mono`.
    pub fn named(name: &str, no_color: bool) -> Result<Theme, ThemeError> {
        let theme = match name {
            "terminal" => Theme::terminal(),
            "mono" => Theme::mono(),
            "high-contrast" => Theme::high_contrast(),
            "dark" => Theme::truecolor(&DARK, "dark"),
            "light" => Theme::truecolor(&LIGHT, "light"),
            other => return Err(ThemeError::UnknownTheme(other.to_owned())),
        };
        Ok(if no_color { theme.into_mono() } else { theme })
    }

    /// The theme's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The style for a semantic name.
    pub fn style(&self, name: StyleName) -> Style {
        self.styles.get(name as usize).copied().unwrap_or_default()
    }

    /// Replaces one style.
    pub fn set(&mut self, name: StyleName, style: Style) {
        if let Some(slot) = self.styles.get_mut(name as usize) {
            *slot = style;
        }
    }

    /// Switches to the ASCII glyph and line sets.
    pub fn with_ascii(mut self, ascii: bool) -> Theme {
        if ascii {
            self.glyphs = Glyphs::ASCII;
            self.lines = Lines::ASCII;
        } else {
            self.glyphs = Glyphs::UNICODE;
            self.lines = Lines::BOX;
        }
        self
    }

    fn build(name: &str, f: impl Fn(StyleName) -> Style) -> Theme {
        Theme {
            name: name.to_owned(),
            styles: StyleName::ALL.iter().map(|&n| f(n)).collect(),
            glyphs: Glyphs::UNICODE,
            lines: Lines::BOX,
        }
    }

    /// The default theme: ANSI colours and the terminal's own fg and bg.
    pub fn terminal() -> Theme {
        use Color::*;
        use StyleName as N;
        let dim = s().fg(DarkGray);
        let acc = s().fg(Blue);
        Theme::build("terminal", |n| match n {
            N::Text | N::ListRow | N::SidebarItem | N::ReaderBody | N::StatusText => s(),
            N::Dim
            | N::TopInfo
            | N::Border
            | N::PaneTitle
            | N::SidebarHeading
            | N::ListPreview
            | N::ListDate
            | N::ListGroup
            | N::ListSkeleton
            | N::ReaderInfo
            | N::ReaderAddress
            | N::ReaderSnippet
            | N::ReaderSignature
            | N::ReaderRule
            | N::ReaderFold
            | N::ReaderHeaderName
            | N::HintLabel
            | N::PopupGroup => dim,
            N::Brand => s().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            N::TopMailbox | N::ListUnread | N::ListDateUnread | N::ReaderSender | N::ReaderDate => {
                s().add_modifier(Modifier::BOLD)
            }
            N::PaneTitleFocused | N::SidebarActive | N::PopupTitle => {
                acc.add_modifier(Modifier::BOLD)
            }
            N::ScrollThumb | N::ReaderCollapsed => s(),
            N::SidebarCount => s().add_modifier(Modifier::BOLD),
            N::SidebarCursor | N::ListCursor | N::PopupCursor => {
                s().add_modifier(Modifier::REVERSED)
            }
            N::ListUnreadMarker | N::ReaderMarker => acc.add_modifier(Modifier::BOLD),
            N::ListPinnedMarker => s().fg(Red),
            N::ListMaskedMarker | N::ReaderMasked => s().fg(Magenta),
            N::ListSelectedMarker | N::StatusSelection => {
                s().fg(Yellow).add_modifier(Modifier::BOLD)
            }
            N::ReaderFocus => acc.add_modifier(Modifier::BOLD | Modifier::REVERSED),
            N::ReaderHeading => s().add_modifier(Modifier::BOLD),
            N::ReaderLink | N::ReaderAttachment => s().fg(Cyan).add_modifier(Modifier::UNDERLINED),
            N::ReaderFootnote => s().fg(Cyan),
            N::ReaderQuote1 => s().fg(Green),
            N::ReaderQuote2 => s().fg(Magenta),
            N::ReaderQuote3 => s().fg(Cyan),
            N::ReaderQuote4 => dim,
            N::ReaderCode => s().fg(Yellow),
            N::ReaderNotice | N::StatusWarning => s().fg(Yellow),
            N::StatusOk => s().fg(Green),
            N::StatusError => s().fg(Red).add_modifier(Modifier::BOLD),
            N::HintKey => acc.add_modifier(Modifier::BOLD),
            N::PopupBorder => acc,
            N::HelpMatch => s().fg(Yellow).add_modifier(Modifier::BOLD),
        })
    }

    /// No colour: attributes only.
    pub fn mono() -> Theme {
        Theme::terminal().into_mono()
    }

    fn into_mono(self) -> Theme {
        use StyleName as N;
        let mut t = Theme::build("mono", |n| {
            let base = self.style(n);
            let mut out = s().add_modifier(base.add_modifier);
            if base.fg == Some(Color::DarkGray) {
                out = out.add_modifier(Modifier::DIM);
            }
            out
        });
        t.set(
            N::PaneTitleFocused,
            s().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        );
        t.set(N::HintKey, s().add_modifier(Modifier::BOLD));
        t.set(N::ReaderQuote1, s().add_modifier(Modifier::DIM));
        t.set(
            N::ReaderQuote2,
            s().add_modifier(Modifier::DIM | Modifier::ITALIC),
        );
        t.set(N::ReaderQuote3, s().add_modifier(Modifier::DIM));
        t.set(
            N::ReaderQuote4,
            s().add_modifier(Modifier::DIM | Modifier::ITALIC),
        );
        t.set(N::ReaderNotice, s().add_modifier(Modifier::ITALIC));
        t.set(N::StatusWarning, s().add_modifier(Modifier::BOLD));
        t.glyphs = self.glyphs;
        t.lines = self.lines;
        t
    }

    /// Default colours only, no dim text, reverse video for selection.
    pub fn high_contrast() -> Theme {
        use StyleName as N;
        let base = Theme::mono();
        let mut t = Theme::build("high-contrast", |n| {
            base.style(n).remove_modifier(Modifier::DIM)
        });
        t.set(
            N::HintKey,
            s().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        );
        t.set(N::ListPreview, s());
        t.set(N::ListUnreadMarker, s().add_modifier(Modifier::BOLD));
        t.set(
            N::ListSelectedMarker,
            s().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        );
        t
    }

    fn truecolor(p: &Palette, name: &str) -> Theme {
        let base = Theme::terminal();
        let map = |c: Option<Color>| -> Option<Color> {
            Some(match c? {
                Color::DarkGray => p.dim,
                Color::Red => p.red,
                Color::Green => p.green,
                Color::Yellow => p.yellow,
                Color::Blue => p.blue,
                Color::Magenta => p.magenta,
                Color::Cyan => p.cyan,
                other => other,
            })
        };
        let mut t = Theme::build(name, |n| {
            let b = base.style(n);
            let mut out = b;
            out.fg = map(b.fg);
            out.bg = map(b.bg);
            out
        });
        let cursor = s().bg(p.cursor);
        t.set(StyleName::ListCursor, cursor);
        t.set(StyleName::SidebarCursor, cursor);
        t.set(StyleName::PopupCursor, cursor);
        t.set(
            StyleName::Brand,
            s().fg(p.bg).bg(p.blue).add_modifier(Modifier::BOLD),
        );
        for n in [
            StyleName::TopMailbox,
            StyleName::ListUnread,
            StyleName::ListDateUnread,
            StyleName::ReaderSender,
            StyleName::ReaderDate,
            StyleName::SidebarCount,
            StyleName::ReaderHeading,
        ] {
            t.set(n, s().fg(p.bold).add_modifier(Modifier::BOLD));
        }
        t
    }

    /// Parses a theme file. `no_color` forces the result to mono after the
    /// file is applied, so `NO_COLOR` always wins.
    pub fn from_toml(document: &str, no_color: bool) -> Result<Theme, ThemeError> {
        let doc: toml::Table = document
            .parse()
            .map_err(|e: toml::de::Error| ThemeError::Toml(e.message().to_owned()))?;
        let base = doc
            .get("base")
            .and_then(|v| v.as_str())
            .unwrap_or("terminal");
        let mut theme = Theme::named(base, false)?;
        if let Some(name) = doc.get("name").and_then(|v| v.as_str()) {
            theme.name = name.to_owned();
        }
        if let Some(ascii) = doc.get("ascii").and_then(|v| v.as_bool()) {
            theme = theme.with_ascii(ascii);
        }
        if let Some(styles) = doc.get("styles") {
            let Some(styles) = styles.as_table() else {
                return Err(ThemeError::BadStyle {
                    name: "styles".into(),
                    reason: "must be a table".into(),
                });
            };
            for (name, value) in styles {
                let style_name = StyleName::from_name(name)
                    .ok_or_else(|| ThemeError::UnknownStyle(name.clone()))?;
                let style = parse_style(name, value)?;
                theme.set(style_name, style);
            }
        }
        Ok(if no_color { theme.into_mono() } else { theme })
    }
}

fn parse_style(name: &str, value: &toml::Value) -> Result<Style, ThemeError> {
    let bad = |reason: String| ThemeError::BadStyle {
        name: name.to_owned(),
        reason,
    };
    let table = value
        .as_table()
        .ok_or_else(|| bad("must be a table such as { fg = \"blue\", bold = true }".into()))?;
    let mut style = Style::new();
    for (attr, v) in table {
        match attr.as_str() {
            "fg" | "bg" => {
                let text = v
                    .as_str()
                    .ok_or_else(|| bad(format!("{attr} must be a colour string")))?;
                let color = parse_color(text).ok_or_else(|| {
                    bad(format!(
                        "unknown colour \"{text}\"; use an ANSI name such as blue or \
                         bright-red, \"default\", \"#rrggbb\" or an index 0 to 255"
                    ))
                })?;
                if attr == "fg" {
                    style = style.fg(color);
                } else {
                    style = style.bg(color);
                }
            }
            flag => {
                let m = match flag {
                    "bold" => Modifier::BOLD,
                    "dim" => Modifier::DIM,
                    "italic" => Modifier::ITALIC,
                    "underline" | "underlined" => Modifier::UNDERLINED,
                    "reverse" | "reversed" => Modifier::REVERSED,
                    "strikethrough" | "crossed" => Modifier::CROSSED_OUT,
                    other => return Err(bad(format!("unknown attribute \"{other}\""))),
                };
                let on = v
                    .as_bool()
                    .ok_or_else(|| bad(format!("{flag} must be true or false")))?;
                if on {
                    style = style.add_modifier(m);
                }
            }
        }
    }
    Ok(style)
}

fn parse_color(text: &str) -> Option<Color> {
    let t = text.trim().to_ascii_lowercase().replace('_', "-");
    if let Some(hex) = t.strip_prefix('#') {
        if hex.len() == 6 {
            let v = u32::from_str_radix(hex, 16).ok()?;
            return Some(Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8));
        }
        return None;
    }
    if let Ok(i) = t.parse::<u8>() {
        return Some(Color::Indexed(i));
    }
    Some(match t.as_str() {
        "default" | "reset" => Color::Reset,
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "white" | "gray" | "grey" => Color::Gray,
        "bright-black" | "dark-gray" | "dark-grey" => Color::DarkGray,
        "bright-red" => Color::LightRed,
        "bright-green" => Color::LightGreen,
        "bright-yellow" => Color::LightYellow,
        "bright-blue" => Color::LightBlue,
        "bright-magenta" => Color::LightMagenta,
        "bright-cyan" => Color::LightCyan,
        "bright-white" => Color::White,
        _ => return None,
    })
}

/// Truecolor palette, from the mock-up's colour tokens.
struct Palette {
    bg: Color,
    bold: Color,
    dim: Color,
    red: Color,
    green: Color,
    yellow: Color,
    blue: Color,
    magenta: Color,
    cyan: Color,
    cursor: Color,
}

const fn rgb(v: u32) -> Color {
    Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

const DARK: Palette = Palette {
    bg: rgb(0x15171c),
    bold: rgb(0xeef0f3),
    dim: rgb(0x666d79),
    red: rgb(0xd98080),
    green: rgb(0x93c393),
    yellow: rgb(0xd8bb7e),
    blue: rgb(0x86aee0),
    magenta: rgb(0xbb9fdc),
    cyan: rgb(0x80c4c4),
    cursor: rgb(0x252932),
};

const LIGHT: Palette = Palette {
    bg: rgb(0xfbfaf7),
    bold: rgb(0x101318),
    dim: rgb(0x8b9099),
    red: rgb(0xb04848),
    green: rgb(0x3b7a3b),
    yellow: rgb(0x8a6716),
    blue: rgb(0x2c5f9e),
    magenta: rgb(0x7a4ca6),
    cyan: rgb(0x2a7a7a),
    cursor: rgb(0xebe9e2),
};

/// True when the `NO_COLOR` environment variable is set and not empty
/// (no-color.org). The shell calls this once and passes the answer on;
/// library code and tests never read the environment.
pub fn no_color_from_env() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_style_has_a_unique_name() {
        let mut names: Vec<&str> = StyleName::ALL.iter().map(|s| s.name()).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), before);
        for &n in StyleName::ALL {
            assert_eq!(StyleName::from_name(n.name()), Some(n));
        }
    }

    #[test]
    fn built_in_themes() {
        for name in Theme::BUILT_IN {
            let t = Theme::named(name, false).unwrap();
            assert_eq!(t.name(), name);
            // Unread and the cursor row must always be distinguishable
            // without colour.
            assert!(
                t.style(StyleName::ListUnread)
                    .add_modifier
                    .contains(Modifier::BOLD),
                "{name}"
            );
            let cursor = t.style(StyleName::ListCursor);
            assert!(
                cursor.add_modifier.contains(Modifier::REVERSED) || cursor.bg.is_some(),
                "{name}"
            );
        }
        assert!(Theme::named("nope", false).is_err());
    }

    #[test]
    fn no_color_forces_mono() {
        for name in Theme::BUILT_IN {
            let t = Theme::named(name, true).unwrap();
            for &n in StyleName::ALL {
                let st = t.style(n);
                assert!(st.fg.is_none() && st.bg.is_none(), "{name} {n}");
            }
        }
        let t = Theme::mono();
        assert!(
            t.style(StyleName::ListPreview)
                .add_modifier
                .contains(Modifier::DIM)
        );
        assert!(
            t.style(StyleName::ListCursor)
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn high_contrast_has_no_dim() {
        let t = Theme::high_contrast();
        for &n in StyleName::ALL {
            assert!(!t.style(n).add_modifier.contains(Modifier::DIM), "{n}");
        }
    }

    #[test]
    fn theme_files() {
        let doc = r##"
            base = "dark"
            name = "mine"
            ascii = true
            [styles]
            "list.unread" = { fg = "bright-yellow", bold = true, underline = true }
            "list.cursor" = { bg = "#102030" }
            "status.error" = { fg = "196" }
        "##;
        let t = Theme::from_toml(doc, false).unwrap();
        assert_eq!(t.name(), "mine");
        assert_eq!(t.glyphs, Glyphs::ASCII);
        assert_eq!(t.style(StyleName::ListUnread).fg, Some(Color::LightYellow));
        assert_eq!(
            t.style(StyleName::ListCursor).bg,
            Some(Color::Rgb(0x10, 0x20, 0x30))
        );
        assert_eq!(
            t.style(StyleName::StatusError).fg,
            Some(Color::Indexed(196))
        );
        let mono = Theme::from_toml(doc, true).unwrap();
        assert_eq!(mono.style(StyleName::ListUnread).fg, None);
        assert_eq!(mono.glyphs, Glyphs::ASCII);
    }

    #[test]
    fn theme_file_errors() {
        assert!(matches!(
            Theme::from_toml("[styles]\n\"list.nope\" = {}", false),
            Err(ThemeError::UnknownStyle(_))
        ));
        assert!(matches!(
            Theme::from_toml("[styles]\n\"list.unread\" = { fg = \"plaid\" }", false),
            Err(ThemeError::BadStyle { .. })
        ));
        assert!(matches!(
            Theme::from_toml("[styles]\n\"list.unread\" = { blink = true }", false),
            Err(ThemeError::BadStyle { .. })
        ));
        assert!(matches!(
            Theme::from_toml("[styles]\n\"list.unread\" = \"bold\"", false),
            Err(ThemeError::BadStyle { .. })
        ));
        assert!(matches!(
            Theme::from_toml("base = \"neon\"", false),
            Err(ThemeError::UnknownTheme(_))
        ));
        assert!(matches!(
            Theme::from_toml("=", false),
            Err(ThemeError::Toml(_))
        ));
    }

    #[test]
    fn glyph_sets_are_one_cell() {
        for g in [Glyphs::UNICODE, Glyphs::ASCII] {
            for s in [
                g.unread,
                g.pinned,
                g.masked,
                g.selected,
                g.ok,
                g.fail,
                g.collapsed,
                g.expanded,
                g.ellipsis,
                g.thumb,
                g.crumb,
            ] {
                assert_eq!(crate::text::width(s), 1, "{s:?}");
            }
        }
    }
}
