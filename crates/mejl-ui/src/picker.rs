//! Fuzzy pickers: the command palette, the move and label pickers, the
//! From picker in compose, the links overlay and the copy, actions and
//! Masked Email menus.
//!
//! A [`Picker`] is a filter line over a list of [`PickItem`]s. Typing
//! narrows the list with [`fuzzy::score`]; with an
//! empty filter the items keep the order they were given in, which is how
//! the palette shows recent actions first. What Enter does depends on the
//! item's [`PickValue`] and the picker's [`PickerKind`], and is handled in
//! `update`.

use crate::action::Action;
use crate::data::{DraftId, MailboxId, Participant};
use crate::event::Target;
use crate::fuzzy;
use crate::input::LineInput;

/// Which picker this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PickerKind {
    /// Every action, mailbox, saved search, contact, draft, masked
    /// address and rule.
    Palette,
    /// Move conversations to a mailbox.
    MoveTo,
    /// Add or remove labels.
    Label,
    /// The From address in compose.
    From,
    /// Links in the focused message.
    Links,
    /// Things to copy from the focused message.
    Copy,
    /// Actions for the conversation or message.
    Actions,
    /// Masked Email actions for the focused message.
    Mask,
}

/// What choosing an item does.
#[derive(Clone, Debug, PartialEq)]
pub enum PickValue {
    /// Run an action.
    Action(Action),
    /// Go to a mailbox, move to it, or toggle it as a label.
    Mailbox(MailboxId),
    /// Create a mailbox named by the filter text.
    CreateMailbox,
    /// Run a saved search.
    SavedSearch(String),
    /// Write to a contact.
    Contact(Participant),
    /// Resume a draft.
    Draft(DraftId),
    /// Open the Masked Email screen at this address.
    Masked(String),
    /// Open the rules screen at this rule.
    Rule(String),
    /// Send from this address.
    From {
        /// The JMAP identity.
        identity_id: Option<String>,
        /// The address.
        from: Participant,
    },
    /// Make a new masked address for this site and send from it.
    NewMask(String),
    /// Open this URL.
    Url(String),
    /// Copy this text.
    Copy(String),
    /// Set a Masked Email address's state.
    MaskState {
        /// Masked Email id.
        id: String,
        /// The state.
        state: crate::data::MaskedState,
    },
    /// An answer to the question asked when quitting while a sent message
    /// is still in its undo send window.
    Quit(QuitChoice),
    /// A line of text that cannot be chosen.
    Info,
}

/// What to do with held sends when quitting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuitChoice {
    /// Send them now, then quit. The default.
    SendNow,
    /// Quit and leave them queued; they go out on the next start.
    KeepQueued,
    /// Cancel them and go back to the message.
    CancelSend,
    /// Do not quit.
    Stay,
}

/// One row of a picker.
#[derive(Clone, Debug, PartialEq)]
pub struct PickItem {
    /// The text that is shown and matched.
    pub label: String,
    /// The key that runs it directly, if any.
    pub key: String,
    /// A short tag in the right column ("mailbox", "contact").
    pub tag: String,
    /// What it does.
    pub value: PickValue,
    /// A check box state, for labels.
    pub checked: Option<bool>,
}

impl PickItem {
    /// An item with no key, tag or check box.
    pub fn new(label: impl Into<String>, value: PickValue) -> PickItem {
        PickItem {
            label: label.into(),
            key: String::new(),
            tag: String::new(),
            value,
            checked: None,
        }
    }

    /// Sets the key column.
    pub fn key(mut self, key: impl Into<String>) -> PickItem {
        self.key = key.into();
        self
    }

    /// Sets the tag column.
    pub fn tag(mut self, tag: impl Into<String>) -> PickItem {
        self.tag = tag.into();
        self
    }

    fn selectable(&self) -> bool {
        self.value != PickValue::Info
    }
}

/// A shown row: index into the items, and matched char positions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    /// Index into [`Picker::items`].
    pub index: usize,
    /// Matched char positions in the label.
    pub positions: Vec<usize>,
}

/// A filterable list in a popup.
#[derive(Clone, Debug, PartialEq)]
pub struct Picker {
    /// Which picker.
    pub kind: PickerKind,
    /// Title on the border.
    pub title: String,
    /// Text on the right of the title border, such as the keys that open
    /// the picker.
    pub hint: String,
    /// The filter.
    pub filter: LineInput,
    /// All items.
    pub items: Vec<PickItem>,
    /// Items that match the filter, best first.
    pub shown: Vec<Shown>,
    /// Index into `shown`.
    pub cursor: usize,
    /// Lines under the list.
    pub footer: Vec<String>,
    /// Offer "create" for an unknown mailbox name (move and label).
    pub can_create: bool,
    /// Conversations a move or label picker acts on.
    pub target: Option<Target>,
}

impl Picker {
    /// A picker over `items`, unfiltered.
    pub fn new(kind: PickerKind, title: impl Into<String>, items: Vec<PickItem>) -> Picker {
        let mut p = Picker {
            kind,
            title: title.into(),
            hint: String::new(),
            filter: LineInput::default(),
            items,
            shown: Vec::new(),
            cursor: 0,
            footer: Vec::new(),
            can_create: false,
            target: None,
        };
        p.refilter();
        p
    }

    /// Applies the filter again after it or the items changed.
    pub fn refilter(&mut self) {
        let pattern = self.filter.text().trim().to_owned();
        self.items.retain(|i| i.value != PickValue::CreateMailbox);
        if self.can_create
            && !pattern.is_empty()
            && !self
                .items
                .iter()
                .any(|i| i.label.eq_ignore_ascii_case(&pattern))
        {
            self.items.push(
                PickItem::new(
                    format!("Create mailbox \"{pattern}\""),
                    PickValue::CreateMailbox,
                )
                .tag("new"),
            );
        }
        if pattern.is_empty() {
            self.shown = (0..self.items.len())
                .map(|index| Shown {
                    index,
                    positions: Vec::new(),
                })
                .collect();
        } else {
            let mut scored: Vec<(i32, Shown)> = self
                .items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    if item.value == PickValue::CreateMailbox {
                        // Always offered last, never ranked.
                        return Some((
                            i32::MIN,
                            Shown {
                                index,
                                positions: Vec::new(),
                            },
                        ));
                    }
                    if !item.selectable() {
                        return None;
                    }
                    let label = fuzzy::score(&pattern, &item.label);
                    let tagged = fuzzy::score(&pattern, &format!("{} {}", item.label, item.tag));
                    match (label, tagged) {
                        (Some(m), _) => Some((
                            m.score,
                            Shown {
                                index,
                                positions: m.positions,
                            },
                        )),
                        (None, Some(m)) => Some((
                            m.score - 50,
                            Shown {
                                index,
                                positions: Vec::new(),
                            },
                        )),
                        (None, None) => None,
                    }
                })
                .collect();
            // Stable, so equal scores keep the given (recent first) order.
            scored.sort_by_key(|s| std::cmp::Reverse(s.0));
            self.shown = scored.into_iter().map(|(_, s)| s).collect();
        }
        self.cursor = self.cursor.min(self.shown.len().saturating_sub(1));
        self.skip_info(true);
    }

    fn skip_info(&mut self, down: bool) {
        let len = self.shown.len();
        for _ in 0..len {
            let ok = self
                .shown
                .get(self.cursor)
                .and_then(|s| self.items.get(s.index))
                .is_some_and(PickItem::selectable);
            if ok {
                return;
            }
            self.cursor = if down {
                (self.cursor + 1) % len
            } else {
                (self.cursor + len - 1) % len
            };
        }
    }

    /// Moves the cursor, wrapping at the ends.
    pub fn step(&mut self, delta: isize) {
        let len = self.shown.len() as isize;
        if len == 0 {
            return;
        }
        let next = (self.cursor as isize + delta).rem_euclid(len);
        self.cursor = next as usize;
        self.skip_info(delta >= 0);
    }

    /// Moves the cursor by a page without wrapping.
    pub fn page(&mut self, delta: isize) {
        let len = self.shown.len();
        if len == 0 {
            return;
        }
        let next = (self.cursor as isize + delta).clamp(0, len as isize - 1);
        self.cursor = next as usize;
        self.skip_info(delta >= 0);
    }

    /// The item under the cursor.
    pub fn selected(&self) -> Option<&PickItem> {
        self.shown
            .get(self.cursor)
            .and_then(|s| self.items.get(s.index))
            .filter(|i| i.selectable())
    }

    /// The item under the cursor, mutable (for label check boxes).
    pub fn selected_mut(&mut self) -> Option<&mut PickItem> {
        let index = self.shown.get(self.cursor)?.index;
        self.items.get_mut(index)
    }

    /// How many items can be chosen.
    pub fn choosable(&self) -> usize {
        self.items.iter().filter(|i| i.selectable()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<PickItem> {
        vec![
            PickItem::new("Compose: new message", PickValue::Action(Action::Compose)).key("c"),
            PickItem::new("Section", PickValue::Info),
            PickItem::new("Go to: Newsletters", PickValue::Mailbox("nl".into())).tag("mailbox"),
            PickItem::new("Go to: Receipts", PickValue::Mailbox("rc".into())).tag("mailbox"),
        ]
    }

    #[test]
    fn empty_filter_keeps_order_and_skips_info_rows() {
        let mut p = Picker::new(PickerKind::Palette, "Commands", items());
        assert_eq!(p.shown.len(), 4);
        assert_eq!(p.cursor, 0);
        p.step(1);
        assert_eq!(p.cursor, 2, "the info row is skipped");
        p.step(-1);
        assert_eq!(p.cursor, 0);
        p.step(-1);
        assert_eq!(p.cursor, 3, "wraps");
        p.page(-10);
        assert_eq!(p.cursor, 0);
        assert_eq!(p.choosable(), 3);
    }

    #[test]
    fn filter_ranks_and_hides_info() {
        let mut p = Picker::new(PickerKind::Palette, "Commands", items());
        p.filter.set("rec");
        p.refilter();
        assert_eq!(p.shown.len(), 1);
        assert_eq!(p.selected().unwrap().label, "Go to: Receipts");
        p.filter.set("mailbox");
        p.refilter();
        assert_eq!(p.shown.len(), 2, "tags match too");
        p.filter.set("zzzz");
        p.refilter();
        assert!(p.selected().is_none());
        p.step(1);
        p.page(1);
    }

    #[test]
    fn create_row_for_unknown_names() {
        let mut p = Picker::new(PickerKind::MoveTo, "Move to", items());
        p.can_create = true;
        p.filter.set("Taxes");
        p.refilter();
        assert_eq!(
            p.selected().map(|i| &i.value),
            Some(&PickValue::CreateMailbox)
        );
        p.filter.set("go to: receipts");
        p.refilter();
        assert!(p.items.iter().all(|i| i.value != PickValue::CreateMailbox));
        p.filter.clear();
        p.refilter();
        assert!(p.items.iter().all(|i| i.value != PickValue::CreateMailbox));
    }
}
