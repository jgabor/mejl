//! The last steps both renderers share: folds, quote and signature styles,
//! and the footnote list.

use crate::classify::{self, Attribution, Kind, Row};
use crate::links::footnote_lines;
use crate::{Fold, Link, LinkStyle, Settings, SpanKind, StyledLine};

/// A rendered line with the class of the logical line it came from.
pub(crate) struct Rendered<'a> {
    pub(crate) line: StyledLine,
    pub(crate) kind: Kind,
    pub(crate) depth: u8,
    /// Set on the first rendered line of a quote run.
    pub(crate) label: Option<&'a Attribution>,
}

/// The pieces of a [`crate::RenderedBody`] that depend on width.
pub(crate) struct Assembled {
    pub(crate) lines: Vec<StyledLine>,
    pub(crate) links: Vec<Link>,
    pub(crate) folds: Vec<Fold>,
}

pub(crate) fn assemble(
    rows: Vec<Rendered<'_>>,
    links: Vec<Link>,
    settings: Settings,
    width: usize,
) -> Assembled {
    let fold_rows: Vec<Row<'_>> = rows
        .iter()
        .map(|r| Row {
            kind: r.kind,
            label: r.label,
        })
        .collect();
    let folds = classify::folds(&fold_rows, settings.fold_quotes);
    let mut lines: Vec<StyledLine> = rows
        .into_iter()
        .map(|r| {
            let mut line = r.line;
            let role = match r.kind {
                Kind::Quote => Some(SpanKind::Quote(r.depth.max(1))),
                Kind::Signature | Kind::SigDelimiter => Some(SpanKind::Signature),
                _ => None,
            };
            if let Some(role) = role {
                for span in &mut line.spans {
                    if span.style.kind == SpanKind::Normal {
                        span.style.kind = role;
                    }
                }
            }
            line
        })
        .collect();
    if settings.link_style == LinkStyle::Footnotes {
        lines.extend(footnote_lines(&links, width));
    }
    Assembled {
        lines,
        links,
        folds,
    }
}
