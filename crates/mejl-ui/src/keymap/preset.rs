//! The built-in key presets.
//!
//! `fastmail` is the default and follows Fastmail's web shortcuts, so
//! Fastmail users relearn nothing. `gmail` follows Gmail's web shortcuts.
//! `vim` follows aerc and vim habits. All three use only non-overlapping prefixes and only keys legacy encoding can
//! deliver, which the tests check.
//!
//! Ctrl+Z suspends mejl in every context, as it does in a shell and in
//! vim, including while typing in compose, where it wins over the body
//! editor's own Ctrl+Z undo. The body undoes with Ctrl+_ or Ctrl+/ (the
//! Emacs and readline undo) and Ctrl+U instead, and redoes with Ctrl+Y or
//! Ctrl+R. `update` checks the global binding before overlays and text
//! inputs see the key, so a picker, prompt or confirm cannot swallow it.
//! Neither preset uses Ctrl+Z for anything else.

use super::Context;
use crate::action::Action as A;

/// A named set of default bindings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Preset {
    /// Fastmail's web shortcuts. The default.
    #[default]
    Fastmail,
    /// Gmail's web shortcuts: `e` archives, `#` trashes, `Shift-I` and
    /// `Shift-U` mark read and unread, `s` stars (pins).
    Gmail,
    /// vim and aerc style: `j`/`k` scroll in the reader, `a` archives,
    /// `u` undoes, counts before motions.
    Vim,
}

impl Preset {
    /// Every built-in preset.
    pub const ALL: [Preset; 3] = [Preset::Fastmail, Preset::Gmail, Preset::Vim];

    /// Config name.
    pub fn name(self) -> &'static str {
        match self {
            Preset::Fastmail => "fastmail",
            Preset::Gmail => "gmail",
            Preset::Vim => "vim",
        }
    }

    /// Looks a preset up by config name.
    pub fn from_name(name: &str) -> Option<Preset> {
        match name {
            "fastmail" => Some(Preset::Fastmail),
            "gmail" => Some(Preset::Gmail),
            "vim" => Some(Preset::Vim),
            _ => None,
        }
    }

    /// Whether digits before a motion are a repeat count.
    pub fn counts(self) -> bool {
        matches!(self, Preset::Vim)
    }

    /// The bindings, per context, in the order help and hints show them.
    pub(crate) fn bindings(self) -> Vec<(Context, &'static str, A)> {
        let (global, triage, list, reader) = match self {
            Preset::Fastmail => (FM_GLOBAL, FM_TRIAGE, FM_LIST, FM_READER),
            Preset::Gmail => (GM_GLOBAL, GM_TRIAGE, FM_LIST, GM_READER),
            Preset::Vim => (VIM_GLOBAL, VIM_TRIAGE, VIM_LIST, VIM_READER),
        };
        let mut out = Vec::new();
        let mut add = |ctx, table: &[(&'static str, A)]| {
            out.extend(table.iter().map(|&(k, a)| (ctx, k, a)));
        };
        add(Context::Global, global);
        add(Context::List, list);
        add(Context::List, triage);
        add(Context::Reader, reader);
        add(Context::Reader, triage);
        add(Context::Sidebar, SIDEBAR);
        add(Context::Help, HELP);
        add(Context::Compose, COMPOSE);
        add(Context::Search, SEARCH);
        add(Context::Masked, MASKED);
        add(Context::Rules, RULES);
        add(Context::Settings, SETTINGS);
        add(Context::Keys, KEYS);
        out
    }

    /// Labels for prefix keys, shown as the which-key title.
    pub(crate) fn prefix_labels(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Preset::Fastmail => &[
                ("g", "go to"),
                (".", "mark"),
                ("*", "select"),
                ("b", "snooze (web only)"),
                ("@", "masked email"),
                ("ctrl-o", "compose"),
            ],
            Preset::Gmail => &[
                ("g", "go to"),
                ("*", "select"),
                ("b", "snooze (web only)"),
                ("@", "masked email"),
                ("ctrl-o", "compose"),
            ],
            Preset::Vim => &[
                ("g", "go to"),
                ("*", "select"),
                ("r", "reply"),
                ("z", "view"),
                ("space", "mejl tools"),
                ("b", "snooze (web only)"),
                ("@", "masked email"),
                ("ctrl-o", "compose"),
            ],
        }
    }
}

const GO: [(&str, A); 12] = [
    ("g i", A::GoInbox),
    ("g a", A::GoArchive),
    ("g s", A::GoSent),
    ("g d", A::GoDrafts),
    ("g t", A::GoTrash),
    ("g j", A::GoSpam),
    ("g f", A::GoMailbox),
    ("g g", A::GoTop),
    ("g r", A::GoRules),
    ("g ,", A::GoSettings),
    ("g m", A::GoMasked),
    ("g p", A::Perf),
];

const FM_GLOBAL: &[(&str, A)] = &[
    ("?", A::Help),
    (":", A::Palette),
    ("ctrl-k", A::Palette),
    ("ctrl-p", A::Palette),
    ("/", A::Search),
    ("u", A::Back),
    ("esc", A::Back),
    ("q", A::Quit),
    ("Q", A::QuitNow),
    ("ctrl-z", A::Suspend),
    ("z", A::Undo),
    ("ctrl-y", A::Redo),
    ("c", A::Compose),
    ("tab", A::NextPane),
    ("shift-tab", A::PrevPane),
    ("ctrl-l", A::Refresh),
    ("G", A::GoBottom),
    ("home", A::GoTop),
    ("end", A::GoBottom),
    GO[0],
    GO[1],
    GO[2],
    GO[3],
    GO[4],
    GO[5],
    GO[6],
    GO[7],
    GO[8],
    GO[9],
    GO[10],
    GO[11],
];

const FM_TRIAGE: &[(&str, A)] = &[
    ("space", A::PageDown),
    ("pgdn", A::PageDown),
    ("pgup", A::PageUp),
    ("ctrl-d", A::HalfPageDown),
    ("ctrl-u", A::HalfPageUp),
    ("x", A::ToggleSelect),
    ("X", A::SelectRange),
    (". r", A::MarkRead),
    (". u", A::MarkUnread),
    (". p", A::Pin),
    (". n", A::Unpin),
    ("* a", A::SelectAll),
    ("* n", A::SelectNone),
    ("y", A::Archive),
    ("[", A::ArchivePrev),
    ("]", A::ArchiveNext),
    ("d", A::Trash),
    ("#", A::DeletePermanently),
    ("!", A::ReportSpam),
    ("m", A::MoveTo),
    ("v", A::MoveTo),
    ("l", A::Label),
    ("s", A::TogglePin),
    ("I", A::MarkRead),
    ("U", A::MarkUnread),
    ("M", A::Mute),
    ("b 1", A::Snooze),
    ("b 2", A::Snooze),
    ("b 3", A::Snooze),
    ("b 4", A::Snooze),
    ("b 5", A::Snooze),
    ("b 6", A::Snooze),
    ("b 7", A::Snooze),
    ("r", A::Reply),
    ("a", A::ReplyAll),
    ("f", A::Forward),
    (";", A::ActionsMenu),
    ("T", A::ToggleThreads),
    ("O", A::OpenInBrowser),
    ("Y", A::CopyMenu),
    ("F", A::NewRule),
    ("@ x", A::MaskedDisable),
    ("@ e", A::MaskedEnable),
    ("@ d", A::MaskedDelete),
    ("@ r", A::MaskedRule),
    ("@ m", A::MaskedMessages),
    ("@ y", A::MaskedCopy),
    ("@ n", A::MaskedNew),
    ("@ @", A::MaskActions),
];

const FM_LIST: &[(&str, A)] = &[
    ("j", A::Next),
    ("down", A::Next),
    ("k", A::Prev),
    ("up", A::Prev),
    ("o", A::Open),
    ("enter", A::Open),
];

const FM_READER: &[(&str, A)] = &[
    ("j", A::Next),
    ("k", A::Prev),
    ("down", A::ScrollDown),
    ("up", A::ScrollUp),
    ("n", A::NextMessage),
    ("p", A::PrevMessage),
    ("o", A::Open),
    ("enter", A::Open),
    ("e", A::Expand),
    ("E", A::ExpandAll),
    ("\"", A::ToggleQuotes),
    ("H", A::ToggleHeaders),
    ("L", A::Links),
    ("V", A::ToggleHtml),
];

/// Gmail has no key for the trash or spam folders, so they follow the keys
/// that file mail there: `g #` and `g !`.
const GM_GO: [(&str, A); 12] = [
    GO[0],
    GO[1],
    ("g t", A::GoSent),
    GO[3],
    ("g #", A::GoTrash),
    ("g !", A::GoSpam),
    ("g l", A::GoMailbox),
    GO[7],
    GO[8],
    GO[9],
    GO[10],
    GO[11],
];

const GM_GLOBAL: &[(&str, A)] = &[
    ("?", A::Help),
    (":", A::Palette),
    ("ctrl-k", A::Palette),
    ("ctrl-p", A::Palette),
    ("/", A::Search),
    ("u", A::Back),
    ("esc", A::Back),
    ("q", A::Quit),
    ("Q", A::QuitNow),
    ("ctrl-z", A::Suspend),
    ("z", A::Undo),
    ("ctrl-y", A::Redo),
    ("c", A::Compose),
    ("tab", A::NextPane),
    ("shift-tab", A::PrevPane),
    ("ctrl-l", A::Refresh),
    ("G", A::GoBottom),
    ("home", A::GoTop),
    ("end", A::GoBottom),
    GM_GO[0],
    GM_GO[1],
    GM_GO[2],
    GM_GO[3],
    GM_GO[4],
    GM_GO[5],
    GM_GO[6],
    GM_GO[7],
    GM_GO[8],
    GM_GO[9],
    GM_GO[10],
    GM_GO[11],
];

/// Permanent delete has no Gmail key; the palette still has it.
const GM_TRIAGE: &[(&str, A)] = &[
    ("space", A::PageDown),
    ("pgdn", A::PageDown),
    ("pgup", A::PageUp),
    ("ctrl-d", A::HalfPageDown),
    ("ctrl-u", A::HalfPageUp),
    ("x", A::ToggleSelect),
    ("X", A::SelectRange),
    ("* a", A::SelectAll),
    ("* n", A::SelectNone),
    ("e", A::Archive),
    ("y", A::Archive),
    ("[", A::ArchivePrev),
    ("]", A::ArchiveNext),
    ("#", A::Trash),
    ("!", A::ReportSpam),
    ("v", A::MoveTo),
    ("l", A::Label),
    ("s", A::TogglePin),
    ("I", A::MarkRead),
    ("U", A::MarkUnread),
    ("m", A::Mute),
    ("b 1", A::Snooze),
    ("b 2", A::Snooze),
    ("b 3", A::Snooze),
    ("b 4", A::Snooze),
    ("b 5", A::Snooze),
    ("b 6", A::Snooze),
    ("b 7", A::Snooze),
    ("r", A::Reply),
    ("a", A::ReplyAll),
    ("f", A::Forward),
    (".", A::ActionsMenu),
    ("T", A::ToggleThreads),
    ("O", A::OpenInBrowser),
    ("Y", A::CopyMenu),
    ("F", A::NewRule),
    ("@ x", A::MaskedDisable),
    ("@ e", A::MaskedEnable),
    ("@ d", A::MaskedDelete),
    ("@ r", A::MaskedRule),
    ("@ m", A::MaskedMessages),
    ("@ y", A::MaskedCopy),
    ("@ n", A::MaskedNew),
    ("@ @", A::MaskActions),
];

/// `e` archives in Gmail, so expanding one message has no key here; `;`
/// expands them all.
const GM_READER: &[(&str, A)] = &[
    ("j", A::Next),
    ("k", A::Prev),
    ("down", A::ScrollDown),
    ("up", A::ScrollUp),
    ("n", A::NextMessage),
    ("p", A::PrevMessage),
    ("o", A::Open),
    ("enter", A::Open),
    (";", A::ExpandAll),
    ("\"", A::ToggleQuotes),
    ("H", A::ToggleHeaders),
    ("L", A::Links),
    ("V", A::ToggleHtml),
];

const VIM_GLOBAL: &[(&str, A)] = &[
    ("?", A::Help),
    (":", A::Palette),
    ("ctrl-k", A::Palette),
    ("/", A::Search),
    ("esc", A::Back),
    ("q", A::Quit),
    ("Q", A::QuitNow),
    ("ctrl-z", A::Suspend),
    ("u", A::Undo),
    ("ctrl-r", A::Redo),
    (".", A::Repeat),
    ("c", A::Compose),
    ("tab", A::NextPane),
    ("shift-tab", A::PrevPane),
    ("ctrl-l", A::Refresh),
    ("G", A::GoBottom),
    GO[0],
    GO[1],
    GO[2],
    GO[3],
    GO[4],
    GO[5],
    GO[6],
    GO[7],
    GO[8],
    GO[9],
    GO[10],
    GO[11],
    ("space r", A::GoRules),
    ("space m", A::GoMasked),
    ("space s", A::GoSettings),
    ("space p", A::Perf),
];

const VIM_TRIAGE: &[(&str, A)] = &[
    ("ctrl-f", A::PageDown),
    ("pgdn", A::PageDown),
    ("ctrl-b", A::PageUp),
    ("pgup", A::PageUp),
    ("ctrl-d", A::HalfPageDown),
    ("ctrl-u", A::HalfPageUp),
    ("x", A::ToggleSelect),
    ("X", A::SelectRange),
    ("* a", A::SelectAll),
    ("* n", A::SelectNone),
    ("a", A::Archive),
    ("[", A::ArchivePrev),
    ("]", A::ArchiveNext),
    ("d", A::Trash),
    ("#", A::DeletePermanently),
    ("!", A::ReportSpam),
    ("m", A::MoveTo),
    ("l", A::Label),
    ("s", A::TogglePin),
    ("I", A::MarkRead),
    ("U", A::MarkUnread),
    ("M", A::Mute),
    ("b 1", A::Snooze),
    ("b 2", A::Snooze),
    ("b 3", A::Snooze),
    ("b 4", A::Snooze),
    ("b 5", A::Snooze),
    ("b 6", A::Snooze),
    ("b 7", A::Snooze),
    ("r r", A::ReplyAll),
    ("r s", A::Reply),
    ("r q", A::Reply),
    ("f", A::Forward),
    (";", A::ActionsMenu),
    ("T", A::ToggleThreads),
    ("O", A::OpenInBrowser),
    ("y", A::CopyMenu),
    ("Y", A::CopyMenu),
    ("F", A::NewRule),
    ("@ x", A::MaskedDisable),
    ("@ e", A::MaskedEnable),
    ("@ d", A::MaskedDelete),
    ("@ r", A::MaskedRule),
    ("@ m", A::MaskedMessages),
    ("@ y", A::MaskedCopy),
    ("@ n", A::MaskedNew),
    ("@ @", A::MaskActions),
];

const VIM_LIST: &[(&str, A)] = &[
    ("j", A::Next),
    ("down", A::Next),
    ("k", A::Prev),
    ("up", A::Prev),
    ("o", A::Open),
    ("enter", A::Open),
];

const VIM_READER: &[(&str, A)] = &[
    ("j", A::ScrollDown),
    ("down", A::ScrollDown),
    ("k", A::ScrollUp),
    ("up", A::ScrollUp),
    ("J", A::Next),
    ("K", A::Prev),
    ("n", A::NextMessage),
    ("N", A::PrevMessage),
    ("enter", A::Open),
    ("z a", A::ToggleQuotes),
    ("z o", A::Expand),
    ("z c", A::Expand),
    ("z R", A::ExpandAll),
    ("E", A::ExpandAll),
    ("\"", A::ToggleQuotes),
    ("H", A::ToggleHeaders),
    ("L", A::Links),
    ("V", A::ToggleHtml),
];

const SIDEBAR: &[(&str, A)] = &[
    ("j", A::Next),
    ("down", A::Next),
    ("k", A::Prev),
    ("up", A::Prev),
    ("o", A::Open),
    ("enter", A::Open),
];

const HELP: &[(&str, A)] = &[
    ("down", A::ScrollDown),
    ("up", A::ScrollUp),
    ("pgdn", A::PageDown),
    ("pgup", A::PageUp),
    ("esc", A::Back),
];

/// Compose binds only keys a text field never needs, so every other key
/// types. `Ctrl-o` is the compose prefix; Ctrl-Enter is left out because
/// legacy encoding cannot deliver it (users can bind it themselves).
const COMPOSE: &[(&str, A)] = &[
    ("ctrl-o s", A::Send),
    ("ctrl-s", A::SaveDraft),
    ("ctrl-o a", A::Attach),
    ("ctrl-o m", A::NewMaskFrom),
    ("ctrl-o e", A::ExternalEditor),
    ("ctrl-o d", A::DiscardDraft),
    ("ctrl-o f", A::PickFrom),
    ("ctrl-o b", A::ToggleBcc),
    ("ctrl-o :", A::Palette),
    ("ctrl-o ?", A::Help),
    ("tab", A::NextField),
    ("shift-tab", A::PrevField),
    ("esc", A::LeaveField),
];

/// The search bar owns typing; these are the keys around it.
const SEARCH: &[(&str, A)] = &[
    ("enter", A::Open),
    ("down", A::Next),
    ("up", A::Prev),
    ("ctrl-n", A::Next),
    ("ctrl-p", A::Prev),
    ("pgdn", A::PageDown),
    ("pgup", A::PageUp),
    ("tab", A::NextField),
    ("shift-tab", A::PrevField),
    ("ctrl-r", A::SearchServer),
    ("ctrl-s", A::SaveSearch),
    ("esc", A::Back),
];

const MASKED: &[(&str, A)] = &[
    ("j", A::Next),
    ("down", A::Next),
    ("k", A::Prev),
    ("up", A::Prev),
    ("pgdn", A::PageDown),
    ("pgup", A::PageUp),
    ("/", A::Filter),
    ("1", A::MaskedShowEnabled),
    ("2", A::MaskedShowDisabled),
    ("3", A::MaskedShowPending),
    ("4", A::MaskedShowDeleted),
    ("5", A::MaskedCycleSilent),
    ("6", A::MaskedCycleCreatedBy),
    ("n", A::MaskedNew),
    ("e", A::MaskedEnable),
    ("x", A::MaskedDisable),
    ("D", A::MaskedDelete),
    ("r", A::MaskedRename),
    ("enter", A::MaskedMessages),
    ("y", A::MaskedCopy),
    ("R", A::MaskedRule),
    ("esc", A::Back),
];

const RULES: &[(&str, A)] = &[
    ("j", A::Next),
    ("down", A::Next),
    ("k", A::Prev),
    ("up", A::Prev),
    ("n", A::RuleNew),
    ("enter", A::Open),
    ("space", A::RuleToggle),
    ("J", A::RuleDown),
    ("K", A::RuleUp),
    ("D", A::RuleDelete),
    ("t", A::RuleDryRun),
    ("a", A::RuleApply),
    ("P", A::RuleCopyBlock),
    ("v", A::RulePasteCheck),
    ("tab", A::NextPane),
    ("shift-tab", A::PrevPane),
    ("esc", A::Back),
];

const SETTINGS: &[(&str, A)] = &[
    ("j", A::Next),
    ("down", A::Next),
    ("k", A::Prev),
    ("up", A::Prev),
    ("enter", A::SettingOpen),
    ("space", A::SettingChange),
    ("right", A::SettingChange),
    ("l", A::SettingChange),
    ("left", A::SettingChangeBack),
    ("h", A::SettingChangeBack),
    ("esc", A::Back),
];

/// The key editor. A few keys mean different things by where the cursor
/// is: on a key row, on a macro, or in a macro's steps.
const KEYS: &[(&str, A)] = &[
    ("j", A::Next),
    ("down", A::Next),
    ("k", A::Prev),
    ("up", A::Prev),
    ("pgdn", A::PageDown),
    ("pgup", A::PageUp),
    ("tab", A::NextPane),
    ("shift-tab", A::PrevPane),
    ("enter", A::KeyReplace),
    ("a", A::KeyAdd),
    ("x", A::KeyRemove),
    ("u", A::KeyReset),
    ("d", A::PresetDuplicate),
    ("R", A::PresetRename),
    ("D", A::PresetDelete),
    ("p", A::PresetSwitch),
    ("/", A::Filter),
    ("n", A::MacroNew),
    ("r", A::MacroRename),
    ("b", A::MacroBind),
    ("K", A::MacroStepUp),
    ("J", A::MacroStepDown),
    ("esc", A::Back),
];
