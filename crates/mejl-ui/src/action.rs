//! Named commands.
//!
//! Every key binding, hint bar entry, which-key entry, help row and mouse
//! target resolves to an [`Action`], and one function applies actions. The
//! stable [`Action::name`] is what users write in the `[keys]` config table
//! and what the command palette will search.

use std::fmt;

/// Groups for the help overlay and the which-key popup.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    /// Moving the cursor and scrolling.
    Navigate,
    /// Going to a mailbox.
    Go,
    /// mejl's own screens: rules, masked email, settings.
    Tools,
    /// Selecting conversations.
    Select,
    /// Archive, trash, move, mark.
    Triage,
    /// Reading and replying.
    Read,
    /// Panes, overlays, undo, quit.
    App,
    /// Writing a message.
    Compose,
    /// The search bar.
    Search,
    /// The Masked Email screen.
    Masked,
    /// The rules screen.
    Rules,
    /// The settings screen.
    Settings,
}

impl Category {
    /// Heading text.
    pub fn label(self) -> &'static str {
        match self {
            Category::Navigate => "Move",
            Category::Go => "Go to",
            Category::Tools => "mejl tools",
            Category::Select => "Select",
            Category::Triage => "Triage",
            Category::Read => "Read and reply",
            Category::App => "App",
            Category::Compose => "Compose",
            Category::Search => "Search",
            Category::Masked => "Masked Email",
            Category::Rules => "Rules",
            Category::Settings => "Settings",
        }
    }
}

macro_rules! actions {
    ($($variant:ident => $name:literal, $label:literal, $cat:ident, $desc:literal;)*) => {
        /// A named command.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Action {
            $(
                #[doc = $desc]
                $variant,
            )*
        }

        impl Action {
            /// Every action, in definition order.
            pub const ALL: &'static [Action] = &[$(Action::$variant),*];

            /// Stable kebab-case name used in config and the palette.
            pub fn name(self) -> &'static str {
                match self { $(Action::$variant => $name,)* }
            }

            /// Short label for the hint bar and which-key popup.
            pub fn label(self) -> &'static str {
                match self { $(Action::$variant => $label,)* }
            }

            /// One-line description for help.
            pub fn description(self) -> &'static str {
                match self { $(Action::$variant => $desc,)* }
            }

            /// Group for help and which-key.
            pub fn category(self) -> Category {
                match self { $(Action::$variant => Category::$cat,)* }
            }
        }
    };
}

actions! {
    Help => "help", "help", App, "Show the keys for this screen";
    Palette => "palette", "commands", App, "Open the command palette";
    Search => "search", "search", App, "Search mail";
    Back => "back", "back", App, "Go back one level: close an overlay, leave the reader";
    Quit => "quit", "quit", App, "Go back, or quit at the top level";
    QuitNow => "quit-now", "quit now", App, "Quit at once";
    Suspend => "suspend", "suspend", App, "Suspend mejl; the shell's fg brings it back";
    Undo => "undo", "undo", App, "Undo the last change";
    Redo => "redo", "redo", App, "Redo the last undone change";
    Compose => "compose", "compose", Read, "Write a new message";
    NextPane => "next-pane", "next pane", App, "Focus the next pane";
    PrevPane => "prev-pane", "previous pane", App, "Focus the previous pane";
    ToggleSidebar => "toggle-sidebar", "mailboxes", App, "Show or hide the mailbox list";
    Refresh => "refresh", "refresh", App, "Sync with the server now";
    Perf => "perf", "performance", Tools, "Show frame timings";
    GoInbox => "go-inbox", "Inbox", Go, "Go to the inbox";
    GoArchive => "go-archive", "Archive", Go, "Go to the archive";
    GoSent => "go-sent", "Sent", Go, "Go to sent mail";
    GoDrafts => "go-drafts", "Drafts", Go, "Go to drafts";
    GoTrash => "go-trash", "Trash", Go, "Go to trash";
    GoSpam => "go-spam", "Spam", Go, "Go to spam";
    GoMailbox => "go-mailbox", "pick a mailbox", Go, "Pick a mailbox from the list";
    GoTop => "go-top", "top of list", Go, "Jump to the first conversation";
    GoBottom => "go-bottom", "bottom of list", Navigate, "Jump to the last conversation";
    GoRules => "go-rules", "Rules (Sieve)", Tools, "Open the rules manager";
    GoMasked => "go-masked", "Masked Email", Tools, "Open the Masked Email manager";
    GoSettings => "go-settings", "Settings", Tools, "Open settings";
    Next => "next", "next", Navigate, "Next conversation, or next item";
    Prev => "prev", "previous", Navigate, "Previous conversation, or previous item";
    Open => "open", "open", Navigate, "Open the conversation or mailbox under the cursor";
    PageDown => "page-down", "page down", Navigate, "Scroll one page down";
    PageUp => "page-up", "page up", Navigate, "Scroll one page up";
    HalfPageDown => "half-page-down", "half page down", Navigate, "Scroll half a page down";
    HalfPageUp => "half-page-up", "half page up", Navigate, "Scroll half a page up";
    ScrollDown => "scroll-down", "scroll down", Navigate, "Scroll the reader one line down";
    ScrollUp => "scroll-up", "scroll up", Navigate, "Scroll the reader one line up";
    NextMessage => "next-message", "next message", Navigate, "Next message in the conversation";
    PrevMessage => "prev-message", "previous message", Navigate, "Previous message in the conversation";
    ToggleSelect => "select", "select", Select, "Select or deselect the conversation";
    SelectRange => "select-range", "select range", Select, "Select from the last selected row to the cursor";
    SelectAll => "select-all", "all", Select, "Select every conversation in the mailbox";
    SelectNone => "select-none", "none", Select, "Clear the selection";
    Archive => "archive", "archive", Triage, "Archive";
    ArchivePrev => "archive-prev", "archive, previous", Triage, "Archive, then go to the previous conversation";
    ArchiveNext => "archive-next", "archive, next", Triage, "Archive, then go to the next conversation";
    Trash => "trash", "trash", Triage, "Move to trash (undo with z)";
    DeletePermanently => "delete-permanently", "delete", Triage, "Delete permanently, in Trash and Spam only";
    ReportSpam => "report-spam", "spam", Triage, "Report spam";
    MoveTo => "move", "move to", Triage, "Move to a mailbox";
    Label => "label", "label", Triage, "Add or remove labels";
    TogglePin => "toggle-pin", "pin", Triage, "Pin or unpin";
    MarkRead => "mark-read", "read", Triage, "Mark as read";
    MarkUnread => "mark-unread", "unread", Triage, "Mark as unread";
    Pin => "pin", "pin", Triage, "Pin";
    Unpin => "unpin", "unpin", Triage, "Unpin";
    Mute => "mute", "mute", Triage, "Mute the conversation";
    Snooze => "snooze", "snooze", Triage, "Snooze (web only in Fastmail)";
    Reply => "reply", "reply", Read, "Reply to the sender";
    ReplyAll => "reply-all", "reply all", Read, "Reply to everyone";
    Forward => "forward", "forward", Read, "Forward";
    Expand => "expand", "expand", Read, "Expand or collapse the focused message";
    ExpandAll => "expand-all", "expand all", Read, "Expand or collapse every message";
    ToggleQuotes => "toggle-quotes", "quotes", Read, "Show or hide quoted text";
    ToggleHeaders => "toggle-headers", "headers", Read, "Show or hide full headers";
    ToggleThreads => "toggle-threads", "thread view", Read, "Switch between conversations and a thread tree";
    Links => "links", "links", Read, "List links to open or copy";
    OpenInBrowser => "open-in-browser", "open in browser", Read, "Open a sanitised copy in the browser";
    CopyMenu => "copy", "copy", Read, "Copy an address, subject, message id or link";
    ActionsMenu => "actions", "actions", Read, "Actions for this message";
    NewRule => "new-rule", "new rule", Read, "New rule from this message";
    MaskActions => "mask-actions", "mask actions", Read, "Masked Email actions for this message";
    Repeat => "repeat", "repeat", App, "Repeat the last change";
    ToggleHtml => "toggle-html", "plain/HTML", Read, "Switch the message between plain text and HTML";
    Send => "send", "send", Compose, "Send the message, with a few seconds to undo";
    SaveDraft => "save-draft", "save", Compose, "Save the draft now";
    Attach => "attach", "attach", Compose, "Attach a file";
    ExternalEditor => "edit-in-editor", "$EDITOR", Compose, "Edit the body in $VISUAL or $EDITOR";
    DiscardDraft => "discard-draft", "discard", Compose, "Throw the draft away";
    ToggleBcc => "toggle-bcc", "bcc", Compose, "Show or hide the Bcc field";
    PickFrom => "pick-from", "send as", Compose, "Pick the From address, masked addresses included";
    NewMaskFrom => "new-mask-from", "new mask", Compose, "Make a masked address for this recipient and send from it";
    NextField => "next-field", "next field", Compose, "Go to the next field";
    PrevField => "prev-field", "previous field", Compose, "Go to the previous field";
    LeaveField => "leave-field", "leave field", Compose, "Leave the field; again closes compose and keeps the draft";
    SearchServer => "search-server", "search server", Search, "Search on the server as well as locally";
    SaveSearch => "save-search", "save search", Search, "Save this search in the sidebar";
    Filter => "filter", "search", Masked, "Filter the list by typing";
    MaskedNew => "masked-new", "new", Masked, "Make a new masked address for a site";
    MaskedEnable => "masked-enable", "enable", Masked, "Enable the masked address";
    MaskedDisable => "masked-disable", "disable", Masked, "Disable the masked address: its mail goes to Trash";
    MaskedDelete => "masked-delete", "delete", Masked, "Delete the masked address: its mail bounces";
    MaskedRename => "masked-rename", "rename", Masked, "Change the description";
    MaskedCopy => "masked-copy", "copy", Masked, "Copy the address";
    MaskedMessages => "masked-messages", "messages", Masked, "Show mail sent to the address";
    MaskedRule => "masked-rule", "rule", Masked, "New rule for mail to this address";
    MaskedShowEnabled => "masked-show-enabled", "enabled", Masked, "Show or hide enabled addresses";
    MaskedShowDisabled => "masked-show-disabled", "disabled", Masked, "Show or hide disabled addresses";
    MaskedShowPending => "masked-show-pending", "pending", Masked, "Show or hide pending addresses";
    MaskedShowDeleted => "masked-show-deleted", "deleted", Masked, "Show or hide deleted addresses";
    MaskedCycleSilent => "masked-cycle-silent", "silent", Masked, "Cycle the silent filter: any, silent (enabled, no mail for 6+ months), active (mail within 6 months)";
    MaskedCycleCreatedBy => "masked-cycle-created-by", "created by", Masked, "Cycle the created by filter: any, then each client in the list";
    RuleNew => "rule-new", "new", Rules, "Make a new rule";
    RuleToggle => "rule-toggle", "on/off", Rules, "Turn the rule on or off";
    RuleUp => "rule-up", "move up", Rules, "Move the rule up, so it runs earlier";
    RuleDown => "rule-down", "move down", Rules, "Move the rule down, so it runs later";
    RuleDelete => "rule-delete", "delete", Rules, "Delete the rule";
    RuleDryRun => "rule-dry-run", "dry run", Rules, "Try the rule against cached mail";
    RuleApply => "rule-apply", "apply to existing", Rules, "Apply the rule to the cached messages it matches";
    RuleCopyBlock => "rule-copy-block", "copy block", Rules, "Copy the Sieve block to paste into Fastmail";
    RulePasteCheck => "rule-paste-check", "paste back", Rules, "Paste your Fastmail script back to check it";
    SettingChange => "setting-change", "change", Settings, "Change the setting to its next value";
    SettingChangeBack => "setting-change-back", "change back", Settings, "Change the setting to its previous value";
}

impl Action {
    /// Looks an action up by its config name.
    pub fn from_name(name: &str) -> Option<Action> {
        Action::ALL.iter().copied().find(|a| a.name() == name)
    }

    /// The closest action name, for "did you mean" hints.
    pub fn suggest(name: &str) -> Option<&'static str> {
        Action::ALL
            .iter()
            .map(|a| (edit_distance(name, a.name()), a.name()))
            .filter(|(d, n)| *d <= 2.max(n.len() / 3))
            .min_by_key(|(d, _)| *d)
            .map(|(_, n)| n)
    }

    /// Movement actions repeat with a count prefix (vim preset).
    pub fn takes_count(self) -> bool {
        matches!(
            self,
            Action::Next
                | Action::Prev
                | Action::ScrollDown
                | Action::ScrollUp
                | Action::PageDown
                | Action::PageUp
                | Action::HalfPageDown
                | Action::HalfPageUp
                | Action::NextMessage
                | Action::PrevMessage
                | Action::GoTop
                | Action::GoBottom
        )
    }

    /// Changes that `.` repeats in the vim preset.
    pub fn is_repeatable(self) -> bool {
        matches!(
            self,
            Action::Archive
                | Action::ArchiveNext
                | Action::ArchivePrev
                | Action::Trash
                | Action::ReportSpam
                | Action::TogglePin
                | Action::MarkRead
                | Action::MarkUnread
                | Action::Pin
                | Action::Unpin
                | Action::ToggleSelect
        )
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn names_are_unique_kebab_case_and_round_trip() {
        let mut seen = HashSet::new();
        for &a in Action::ALL {
            assert!(seen.insert(a.name()), "duplicate {}", a.name());
            assert!(
                a.name().chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "{}",
                a.name()
            );
            assert_eq!(Action::from_name(a.name()), Some(a));
            assert!(!a.label().is_empty() && !a.description().is_empty());
        }
        assert_eq!(Action::from_name("nope"), None);
    }

    #[test]
    fn suggestions() {
        assert_eq!(Action::suggest("archve"), Some("archive"));
        assert_eq!(Action::suggest("go-inbx"), Some("go-inbox"));
        assert_eq!(Action::suggest("zzzzzzzzzzzz"), None);
    }
}
