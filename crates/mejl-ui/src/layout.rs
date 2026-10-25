//! Responsive layout of the mail screen.
//!
//! The layout is a pure function of the terminal size and a few pieces of
//! state, so `update` (for page sizes and scrolling) and `render` (for
//! drawing) always agree without sharing mutable data.
//!
//! Breakpoints, from terminal-ux.md and the mock-ups:
//!
//! - 160 columns or more: three panes, sidebar, list and reader.
//! - 100 to 159: two panes, list and reader; the sidebar is an overlay.
//! - under 100: one pane; Enter opens the reader full width.
//!
//! Rows: a top line, one box with the panes, a status line and a hint bar.
//! The hint bar is dropped when hints are off or the terminal is shorter
//! than 20 rows.

use ratatui::layout::Rect;

use crate::event::PaneSizes;

/// Columns from which three panes are shown.
pub const THREE_PANE_MIN: u16 = 160;
/// Columns from which two panes are shown.
pub const TWO_PANE_MIN: u16 = 100;
/// Rows below which the hint bar is dropped.
pub const HINTS_MIN_ROWS: u16 = 20;
/// Width of the full reader's side panel.
pub const SIDE_PANEL_WIDTH: u16 = 35;
/// Width the full reader needs before it shows the side panel.
pub const SIDE_PANEL_MIN: u16 = 100;
/// Smallest sidebar width when dragged.
pub const SIDEBAR_MIN: u16 = 14;
/// Largest sidebar width when dragged.
pub const SIDEBAR_MAX: u16 = 40;
/// Smallest pane width when dragging the list and reader border.
pub const PANE_MIN: u16 = 24;
/// Default list share when no ratio was dragged, giving the mock-ups'
/// 73 and 61 column split at 160 columns.
const AUTO_LIST_SHARE: f32 = 0.545;
/// The reader keeps at least this many columns in the automatic split.
const AUTO_READER_MIN: u16 = 60;

/// The user's layout choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LayoutPref {
    /// Pick by width.
    #[default]
    Auto,
    /// Always three panes.
    Three,
    /// Always two panes.
    Two,
    /// Always one pane.
    Stack,
}

/// The pane arrangement in use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutMode {
    /// Sidebar, list, reader.
    Three,
    /// List and reader.
    Two,
    /// One pane at a time.
    Stack,
}

impl LayoutMode {
    /// The mode for a width and preference.
    pub fn for_width(width: u16, pref: LayoutPref) -> LayoutMode {
        match pref {
            LayoutPref::Three => LayoutMode::Three,
            LayoutPref::Two => LayoutMode::Two,
            LayoutPref::Stack => LayoutMode::Stack,
            LayoutPref::Auto if width >= THREE_PANE_MIN => LayoutMode::Three,
            LayoutPref::Auto if width >= TWO_PANE_MIN => LayoutMode::Two,
            LayoutPref::Auto => LayoutMode::Stack,
        }
    }
}

/// A draggable border between panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Splitter {
    /// Between sidebar and list.
    SidebarList,
    /// Between list and reader.
    ListReader,
}

/// Inputs to [`compute`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutInput {
    /// Terminal size.
    pub area: Rect,
    /// Layout preference.
    pub pref: LayoutPref,
    /// Show the hint bar if there is room.
    pub hints: bool,
    /// The reader fills the screen (after Enter in narrow layouts).
    pub reader_full: bool,
    /// The sidebar overlay is open (two pane and stack layouts).
    pub sidebar_overlay: bool,
    /// Dragged pane sizes.
    pub panes: PaneSizes,
}

/// Where everything goes. Pane rects are the inner content areas.
#[derive(Clone, Debug, PartialEq)]
pub struct MailLayout {
    /// The arrangement.
    pub mode: LayoutMode,
    /// Top line.
    pub top: Rect,
    /// The outer box, borders included.
    pub frame: Rect,
    /// Status line.
    pub status: Rect,
    /// Hint bar, if shown.
    pub hints: Option<Rect>,
    /// Sidebar pane (three pane layout).
    pub sidebar: Option<Rect>,
    /// Conversation list.
    pub list: Option<Rect>,
    /// Reader pane, or the full reader's main column.
    pub reader: Option<Rect>,
    /// Full reader's side panel.
    pub side: Option<Rect>,
    /// Sidebar overlay box, borders included.
    pub sidebar_overlay: Option<Rect>,
    /// Vertical separators: column and what it splits. Draggable ones
    /// carry a [`Splitter`].
    pub separators: Vec<(u16, Option<Splitter>)>,
    /// Column that carries the list's scrollbar.
    pub list_scrollbar: Option<u16>,
    /// The reader fills the screen.
    pub reader_full: bool,
}

impl MailLayout {
    /// Too small to draw anything useful.
    pub fn too_small(area: Rect) -> bool {
        area.width < 30 || area.height < 8
    }
}

/// Computes the layout.
pub fn compute(input: &LayoutInput) -> MailLayout {
    let a = input.area;
    let mut mode = LayoutMode::for_width(a.width, input.pref);
    // A forced layout that does not fit degrades instead of squeezing
    // panes to nothing.
    let inner_width = a.width.saturating_sub(2);
    if mode == LayoutMode::Three && inner_width < SIDEBAR_MIN + 1 + 2 * PANE_MIN + 1 {
        mode = LayoutMode::Two;
    }
    if mode == LayoutMode::Two && inner_width < 2 * PANE_MIN + 1 {
        mode = LayoutMode::Stack;
    }
    let show_hints = input.hints && a.height >= HINTS_MIN_ROWS;
    let bottom_rows = if show_hints { 2 } else { 1 };
    let top = Rect::new(a.x, a.y, a.width, 1.min(a.height));
    let frame_h = a.height.saturating_sub(1 + bottom_rows);
    let frame = Rect::new(a.x, a.y + 1, a.width, frame_h);
    let status = Rect::new(a.x, a.y + 1 + frame_h, a.width, 1);
    let hints = show_hints.then(|| Rect::new(a.x, a.y + 2 + frame_h, a.width, 1));
    let inner_y = frame.y + 1;
    let inner_h = frame.height.saturating_sub(2);
    let inner_x = frame.x + 1;
    let inner_w = frame.width.saturating_sub(2);
    let mut out = MailLayout {
        mode,
        top,
        frame,
        status,
        hints,
        sidebar: None,
        list: None,
        reader: None,
        side: None,
        sidebar_overlay: None,
        separators: Vec::new(),
        list_scrollbar: None,
        reader_full: input.reader_full,
    };
    if input.reader_full {
        if a.width >= SIDE_PANEL_MIN {
            let main_w = inner_w.saturating_sub(SIDE_PANEL_WIDTH + 1);
            let sep = inner_x + main_w;
            out.reader = Some(Rect::new(inner_x, inner_y, main_w, inner_h));
            out.side = Some(Rect::new(sep + 1, inner_y, SIDE_PANEL_WIDTH, inner_h));
            out.separators.push((sep, None));
        } else {
            out.reader = Some(Rect::new(inner_x, inner_y, inner_w, inner_h));
        }
        return out;
    }
    let mut x = inner_x;
    let mut avail = inner_w;
    if mode == LayoutMode::Three {
        let max = SIDEBAR_MAX.min(inner_w.saturating_sub(2 * PANE_MIN + 2));
        let side_w = input
            .panes
            .sidebar
            .clamp(SIDEBAR_MIN.min(max), max.max(SIDEBAR_MIN.min(max)));
        out.sidebar = Some(Rect::new(x, inner_y, side_w, inner_h));
        out.separators
            .push((x + side_w, Some(Splitter::SidebarList)));
        x += side_w + 1;
        avail = avail.saturating_sub(side_w + 1);
    }
    if mode == LayoutMode::Stack {
        out.list = Some(Rect::new(x, inner_y, avail, inner_h));
        out.list_scrollbar = Some(frame.x + frame.width.saturating_sub(1));
    } else {
        let shared = avail.saturating_sub(1);
        let list_w = list_width(shared, input.panes.list_ratio);
        let sep = x + list_w;
        out.list = Some(Rect::new(x, inner_y, list_w, inner_h));
        out.reader = Some(Rect::new(sep + 1, inner_y, shared - list_w, inner_h));
        out.separators.push((sep, Some(Splitter::ListReader)));
        out.list_scrollbar = Some(sep);
    }
    if input.sidebar_overlay && mode != LayoutMode::Three {
        let side = input.panes.sidebar.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
        let w = side.saturating_add(2).min(frame.width);
        out.sidebar_overlay = Some(Rect::new(frame.x, frame.y, w, frame.height));
    }
    out
}

/// Width of the list in the space shared with the reader.
fn list_width(shared: u16, ratio: Option<f32>) -> u16 {
    if shared < 2 * PANE_MIN {
        return shared / 2;
    }
    match ratio {
        Some(r) if r.is_finite() => {
            let w = (f32::from(shared) * r.clamp(0.0, 1.0)).round() as u16;
            w.clamp(PANE_MIN, shared - PANE_MIN)
        }
        _ => {
            let w = (f32::from(shared) * AUTO_LIST_SHARE).round() as u16;
            let reader = shared - w.min(shared);
            if reader < AUTO_READER_MIN {
                shared.saturating_sub(AUTO_READER_MIN).max(PANE_MIN)
            } else {
                w
            }
        }
    }
}

/// The list ratio that puts the list and reader border at column `x`.
pub fn ratio_for_drag(layout: &MailLayout, x: u16) -> Option<f32> {
    let list = layout.list?;
    let reader = layout.reader?;
    let shared = list.width + reader.width;
    if shared < 2 * PANE_MIN {
        return None;
    }
    let w = x.saturating_sub(list.x).clamp(PANE_MIN, shared - PANE_MIN);
    Some(f32::from(w) / f32::from(shared))
}

/// The sidebar width that puts the sidebar and list border at column `x`.
pub fn sidebar_for_drag(layout: &MailLayout, x: u16) -> Option<u16> {
    let side = layout.sidebar?;
    Some(x.saturating_sub(side.x).clamp(SIDEBAR_MIN, SIDEBAR_MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(w: u16, h: u16) -> LayoutInput {
        LayoutInput {
            area: Rect::new(0, 0, w, h),
            pref: LayoutPref::Auto,
            hints: true,
            reader_full: false,
            sidebar_overlay: false,
            panes: PaneSizes::default(),
        }
    }

    #[test]
    fn breakpoints() {
        assert_eq!(compute(&input(200, 50)).mode, LayoutMode::Three);
        assert_eq!(compute(&input(160, 40)).mode, LayoutMode::Three);
        assert_eq!(compute(&input(159, 40)).mode, LayoutMode::Two);
        assert_eq!(compute(&input(100, 40)).mode, LayoutMode::Two);
        assert_eq!(compute(&input(99, 40)).mode, LayoutMode::Stack);
        assert_eq!(compute(&input(80, 24)).mode, LayoutMode::Stack);
        let mut forced = input(80, 24);
        forced.pref = LayoutPref::Three;
        assert_eq!(compute(&forced).mode, LayoutMode::Three);
        // Forced layouts that cannot fit fall back.
        forced.area = Rect::new(0, 0, 60, 24);
        assert_eq!(compute(&forced).mode, LayoutMode::Two);
        forced.area = Rect::new(0, 0, 40, 24);
        assert_eq!(compute(&forced).mode, LayoutMode::Stack);
    }

    #[test]
    fn matches_the_mockup_columns() {
        let l = compute(&input(160, 40));
        assert_eq!(l.sidebar, Some(Rect::new(1, 2, 22, 35)));
        assert_eq!(l.list, Some(Rect::new(24, 2, 73, 35)));
        assert_eq!(l.reader, Some(Rect::new(98, 2, 61, 35)));
        assert_eq!(l.separators[0].0, 23);
        assert_eq!(l.separators[1].0, 97);
        assert_eq!(l.status.y, 38);
        assert_eq!(l.hints.map(|h| h.y), Some(39));

        let l = compute(&input(120, 36));
        assert_eq!(l.list, Some(Rect::new(1, 2, 57, 31)));
        assert_eq!(l.reader, Some(Rect::new(59, 2, 60, 31)));
        assert_eq!(l.separators[0].0, 58);

        let l = compute(&input(80, 24));
        assert_eq!(l.list, Some(Rect::new(1, 2, 78, 19)));
        assert_eq!(l.reader, None);
        assert_eq!(l.list_scrollbar, Some(79));
    }

    #[test]
    fn full_reader_side_panel() {
        let mut i = input(120, 36);
        i.reader_full = true;
        let l = compute(&i);
        assert_eq!(l.reader, Some(Rect::new(1, 2, 82, 31)));
        assert_eq!(l.side, Some(Rect::new(84, 2, 35, 31)));
        let mut i = input(80, 24);
        i.reader_full = true;
        let l = compute(&i);
        assert_eq!(l.reader, Some(Rect::new(1, 2, 78, 19)));
        assert_eq!(l.side, None);
    }

    #[test]
    fn short_terminals_drop_hints() {
        let l = compute(&input(80, 19));
        assert_eq!(l.hints, None);
        assert_eq!(l.status.y, 18);
        let mut i = input(80, 30);
        i.hints = false;
        assert_eq!(compute(&i).hints, None);
    }

    #[test]
    fn dragged_ratios_are_clamped() {
        let mut i = input(160, 40);
        i.panes.list_ratio = Some(0.0);
        let l = compute(&i);
        assert_eq!(l.list.map(|r| r.width), Some(PANE_MIN));
        i.panes.list_ratio = Some(1.0);
        let l = compute(&i);
        assert_eq!(l.reader.map(|r| r.width), Some(PANE_MIN));
        i.panes.list_ratio = Some(f32::NAN);
        assert_eq!(compute(&i).list.map(|r| r.width), Some(73));
        i.panes.sidebar = 200;
        assert_eq!(compute(&i).sidebar.map(|r| r.width), Some(SIDEBAR_MAX));
        let l = compute(&input(160, 40));
        let r = ratio_for_drag(&l, 70).unwrap();
        let mut j = input(160, 40);
        j.panes.list_ratio = Some(r);
        assert_eq!(compute(&j).separators[1].0, 70);
        assert_eq!(sidebar_for_drag(&l, 31), Some(30));
    }

    #[test]
    fn tiny_sizes_do_not_panic() {
        for w in 0..40 {
            for h in 0..12 {
                let l = compute(&input(w, h));
                assert!(l.status.y <= h.max(1));
            }
        }
    }
}
