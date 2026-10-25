//! Mouse hit testing.
//!
//! ratatui is immediate mode and has no widget tree, so every interactive
//! element records its rectangle and a [`HitTarget`] while it is drawn.
//! The frame's [`HitMap`] is kept until the next frame; a mouse event is
//! tested against it from the top-most (last drawn) region down, and the
//! target is turned into the same action a key would run.

use ratatui::layout::{Position, Rect};

use crate::action::Action;
use crate::data::{EmailId, MailboxId};
use crate::layout::Splitter;

/// A pane, for wheel scrolling and focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pane {
    /// The mailbox list.
    Sidebar,
    /// The conversation list.
    List,
    /// The reader.
    Reader,
}

/// What a region of the screen does when clicked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HitTarget {
    /// A pane's area, for wheel scrolling and click to focus.
    Pane(Pane),
    /// A conversation row, by list index.
    ListRow(usize),
    /// The marker column of a row: click toggles selection.
    ListMarker(usize),
    /// A mailbox in the sidebar, by sidebar row.
    SidebarRow(usize),
    /// A draggable pane border.
    Splitter(Splitter),
    /// A scrollbar track. Clicking jumps to that position.
    Scrollbar {
        /// The pane it scrolls.
        pane: Pane,
        /// The track.
        track: Rect,
    },
    /// Runs an action: hint bar entries, which-key entries, panel hints.
    Action(Action),
    /// Starts a prefix, as if its keys were typed (hint bar `g go`).
    Prefix(crate::keymap::KeySeq),
    /// Opens a link.
    Link(String),
    /// Toggles a fold in a message body.
    Fold {
        /// The message.
        email_id: EmailId,
        /// Index into the rendering's folds.
        fold: usize,
    },
    /// Opens an attachment.
    Attachment {
        /// The message.
        email_id: EmailId,
        /// Index into the message's attachments.
        index: usize,
    },
    /// A message header in the reader: click expands or collapses it.
    Message(EmailId),
    /// The mailbox name in the top line: toggles the sidebar overlay.
    MailboxTitle,
    /// A mailbox, for example in an overlay.
    Mailbox(MailboxId),
    /// A row of an open picker, by its index among the shown rows.
    PickerRow(usize),
    /// A field of the current screen's form (compose fields, settings
    /// rows), by index.
    Field(usize),
    /// A row of the current screen's main list (search results, masked
    /// addresses, rules), by index.
    Row(usize),
    /// Inside a popup but not on an entry: swallows the click.
    Popup,
    /// Everything outside a modal popup: a click closes it.
    Backdrop,
}

/// The regions recorded while drawing one frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HitMap {
    regions: Vec<(Rect, HitTarget)>,
}

impl HitMap {
    /// An empty map.
    pub fn new() -> HitMap {
        HitMap::default()
    }

    /// Records a region. Later regions are on top of earlier ones.
    pub fn push(&mut self, area: Rect, target: HitTarget) {
        if area.width > 0 && area.height > 0 {
            self.regions.push((area, target));
        }
    }

    /// The top-most target at a cell.
    pub fn hit(&self, x: u16, y: u16) -> Option<&HitTarget> {
        let p = Position::new(x, y);
        self.regions
            .iter()
            .rev()
            .find(|(r, _)| r.contains(p))
            .map(|(_, t)| t)
    }

    /// Every target at a cell, top-most first.
    pub fn hits(&self, x: u16, y: u16) -> impl Iterator<Item = &HitTarget> {
        let p = Position::new(x, y);
        self.regions
            .iter()
            .rev()
            .filter(move |(r, _)| r.contains(p))
            .map(|(_, t)| t)
    }

    /// The rectangle of the first region with this target.
    pub fn find(&self, target: &HitTarget) -> Option<Rect> {
        self.regions
            .iter()
            .find(|(_, t)| t == target)
            .map(|(r, _)| *r)
    }

    /// All recorded regions, bottom first.
    pub fn regions(&self) -> &[(Rect, HitTarget)] {
        &self.regions
    }

    /// Number of regions.
    pub fn len(&self) -> usize {
        self.regions.len()
    }

    /// True when nothing was recorded.
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_most_region_wins() {
        let mut map = HitMap::new();
        map.push(Rect::new(0, 0, 10, 10), HitTarget::Pane(Pane::List));
        map.push(Rect::new(0, 2, 10, 1), HitTarget::ListRow(2));
        map.push(Rect::new(0, 0, 0, 5), HitTarget::Popup);
        assert_eq!(map.len(), 2);
        assert_eq!(map.hit(3, 2), Some(&HitTarget::ListRow(2)));
        assert_eq!(map.hit(3, 3), Some(&HitTarget::Pane(Pane::List)));
        assert_eq!(map.hit(10, 3), None);
        assert_eq!(map.hits(3, 2).count(), 2);
        assert_eq!(
            map.find(&HitTarget::ListRow(2)),
            Some(Rect::new(0, 2, 10, 1))
        );
    }
}
