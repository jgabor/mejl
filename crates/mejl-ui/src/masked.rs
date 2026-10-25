//! The Masked Email screen: every masked address with its state and local
//! statistics, a filter, and actions to create, enable, disable, delete,
//! rename and copy addresses, or jump to their mail.
//!
//! The list comes from [`MailData::masked_emails`](crate::MailData) when
//! the screen opens and again whenever `Msg::DataChanged` reports
//! `masked: true`. Changes go out as [`Cmd::MaskedSetState`] and friends;
//! the store applies them locally at once, so the next read shows them.
//!
//! [`Cmd::MaskedSetState`]: crate::Cmd::MaskedSetState

use jiff::{SignedDuration, Timestamp, Zoned};

use crate::data::{MaskedEmailView, MaskedState};
use crate::input::LineInput;

/// Masked states in filter order, with their filter keys 1 to 4.
pub const STATES: [MaskedState; 4] = [
    MaskedState::Enabled,
    MaskedState::Disabled,
    MaskedState::Pending,
    MaskedState::Deleted,
];

/// The word for a state.
pub fn state_name(state: MaskedState) -> &'static str {
    match state {
        MaskedState::Enabled => "enabled",
        MaskedState::Disabled => "disabled",
        MaskedState::Pending => "pending",
        MaskedState::Deleted => "deleted",
    }
}

/// How long an address goes without mail before it counts as silent.
const SILENT_AFTER: SignedDuration = SignedDuration::from_hours(24 * 183);

/// The silent filter on the Masked Email screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SilentFilter {
    /// Every address.
    #[default]
    Any,
    /// Enabled addresses whose last mail is six months old or more, the
    /// ones the "silent for 6+ months" count counts.
    Silent,
    /// Addresses with mail in the last six months.
    Active,
}

impl SilentFilter {
    /// The word on the filter row.
    pub fn name(self) -> &'static str {
        match self {
            SilentFilter::Any => "any",
            SilentFilter::Silent => "silent",
            SilentFilter::Active => "active",
        }
    }

    /// The next filter in the cycle any, silent, active.
    pub fn next(self) -> SilentFilter {
        match self {
            SilentFilter::Any => SilentFilter::Silent,
            SilentFilter::Silent => SilentFilter::Active,
            SilentFilter::Active => SilentFilter::Any,
        }
    }
}

/// The time six months before `now`.
fn silent_cut(now: Timestamp) -> Timestamp {
    now.checked_sub(SILENT_AFTER).unwrap_or(now)
}

/// Whether `m` is silent: enabled, with its last mail before `cut`. An
/// address that never got mail is not silent (nor active).
fn is_silent(m: &MaskedEmailView, cut: Timestamp) -> bool {
    m.state == MaskedState::Enabled && m.last_message_at.is_some_and(|t| t < cut)
}

/// Whether `m` got mail at or after `cut`.
fn is_active(m: &MaskedEmailView, cut: Timestamp) -> bool {
    m.last_message_at.is_some_and(|t| t >= cut)
}

/// Senders of mail to one address, from the local index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Senders {
    pub(crate) id: String,
    pub(crate) senders: Vec<(String, usize)>,
}

/// The Masked Email screen.
#[derive(Clone, Debug)]
pub struct MaskedScreen {
    pub(crate) all: Vec<MaskedEmailView>,
    pub(crate) shown: Vec<usize>,
    pub(crate) cursor: usize,
    pub(crate) filter: LineInput,
    pub(crate) filtering: bool,
    pub(crate) show: [bool; 4],
    /// The silent filter.
    pub(crate) silent: SilentFilter,
    /// Only addresses made by this client (`created_by`, compared without
    /// case), or every address when `None`.
    pub(crate) created_by: Option<String>,
    /// The time the silent filter measures from: when the list was last
    /// read or a filter changed.
    pub(crate) now: Timestamp,
    pub(crate) senders: Option<Senders>,
}

impl MaskedScreen {
    /// A screen over `all` as of `now`. Pending and deleted addresses start
    /// hidden, as in Fastmail's own list.
    pub(crate) fn new(all: Vec<MaskedEmailView>, now: Timestamp) -> MaskedScreen {
        let mut s = MaskedScreen {
            all,
            shown: Vec::new(),
            cursor: 0,
            filter: LineInput::default(),
            filtering: false,
            show: [true, true, false, false],
            silent: SilentFilter::Any,
            created_by: None,
            now,
            senders: None,
        };
        s.refilter(None);
        s
    }

    /// The address under the cursor.
    pub(crate) fn selected(&self) -> Option<&MaskedEmailView> {
        self.shown.get(self.cursor).and_then(|&i| self.all.get(i))
    }

    /// Replaces the list as of `now`, keeping the cursor on the same
    /// address.
    pub(crate) fn reload(&mut self, all: Vec<MaskedEmailView>, now: Timestamp) {
        let keep = self.selected().map(|m| m.id.clone());
        self.all = all;
        self.now = now;
        self.refilter(keep.as_deref());
    }

    /// The distinct `created_by` values, without case, sorted. Empty
    /// values are left out.
    pub(crate) fn creators(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for m in &self.all {
            let by = m.created_by.trim();
            if by.is_empty() || out.iter().any(|x| x.eq_ignore_ascii_case(by)) {
                continue;
            }
            out.push(by.to_owned());
        }
        out.sort_by_key(|x| x.to_lowercase());
        out
    }

    /// Moves the created-by filter on: any, then each value in
    /// [`MaskedScreen::creators`], then any again. A value no address has
    /// any more starts the cycle over at the first one.
    pub(crate) fn cycle_created_by(&mut self) {
        let values = self.creators();
        let at = self
            .created_by
            .as_ref()
            .and_then(|c| values.iter().position(|v| v.eq_ignore_ascii_case(c)));
        self.created_by = match (self.created_by.is_some(), at) {
            (false, _) | (true, None) => values.first().cloned(),
            (true, Some(i)) => values.get(i + 1).cloned(),
        };
    }

    /// Applies the state, silent, created-by and text filters, newest mail
    /// first.
    pub(crate) fn refilter(&mut self, keep: Option<&str>) {
        let keep = keep
            .map(str::to_owned)
            .or_else(|| self.selected().map(|m| m.id.clone()));
        let q = self.filter.text().trim().to_lowercase();
        let cut = silent_cut(self.now);
        let mut shown: Vec<usize> = self
            .all
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                let i = STATES.iter().position(|s| *s == m.state).unwrap_or(0);
                self.show[i]
            })
            .filter(|(_, m)| match self.silent {
                SilentFilter::Any => true,
                SilentFilter::Silent => is_silent(m, cut),
                SilentFilter::Active => is_active(m, cut),
            })
            .filter(|(_, m)| {
                self.created_by
                    .as_ref()
                    .is_none_or(|c| m.created_by.trim().eq_ignore_ascii_case(c))
            })
            .filter(|(_, m)| {
                q.is_empty()
                    || m.email.to_lowercase().contains(&q)
                    || m.description.to_lowercase().contains(&q)
                    || m.for_domain.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect();
        let all = &self.all;
        shown.sort_by(|&a, &b| {
            let (x, y) = (&all[a], &all[b]);
            y.last_message_at
                .cmp(&x.last_message_at)
                .then_with(|| y.created_at.cmp(&x.created_at))
                .then_with(|| x.email.cmp(&y.email))
        });
        self.shown = shown;
        self.cursor = keep
            .and_then(|id| {
                self.shown
                    .iter()
                    .position(|&i| self.all.get(i).is_some_and(|m| m.id == id))
            })
            .unwrap_or(self.cursor)
            .min(self.shown.len().saturating_sub(1));
    }

    /// Moves the cursor to an address, showing its state if it was hidden
    /// and clearing the filters that could hide it.
    pub(crate) fn focus(&mut self, id: &str) {
        if let Some(m) = self.all.iter().find(|m| m.id == id) {
            let i = STATES.iter().position(|s| *s == m.state).unwrap_or(0);
            self.show[i] = true;
        }
        self.silent = SilentFilter::Any;
        self.created_by = None;
        self.filter.clear();
        self.refilter(Some(id));
    }

    /// Moves the cursor by `delta` rows.
    pub(crate) fn step(&mut self, delta: isize) {
        let len = self.shown.len();
        if len == 0 {
            return;
        }
        let next = (self.cursor as isize + delta).clamp(0, len as isize - 1);
        self.cursor = next as usize;
    }

    /// Counts per state, over every address.
    pub(crate) fn counts(&self) -> [usize; 4] {
        let mut c = [0usize; 4];
        for m in &self.all {
            if let Some(i) = STATES.iter().position(|s| *s == m.state) {
                c[i] += 1;
            }
        }
        c
    }

    /// Enabled addresses with no mail for six months or more.
    pub(crate) fn silent(&self, now: Timestamp) -> usize {
        let cut = silent_cut(now);
        self.all.iter().filter(|m| is_silent(m, cut)).count()
    }

    /// Addresses with mail in Spam.
    pub(crate) fn with_spam(&self) -> usize {
        self.all.iter().filter(|m| m.local_in_spam > 0).count()
    }
}

/// The "last mail" column: `09:12` today, `Tue` this week, `Oct 16`
/// within two months, then `4 mo` and `2 yr`, or `never`.
pub fn last_seen(ts: Option<Timestamp>, now: &Zoned) -> String {
    let Some(ts) = ts else {
        return "never".into();
    };
    let z = ts.to_zoned(now.time_zone().clone());
    let days = now.date().since(z.date()).map_or(0, |s| s.get_days());
    if days <= 0 {
        z.strftime("%H:%M").to_string()
    } else if days < 7 {
        z.strftime("%a").to_string()
    } else if days < 60 {
        z.strftime("%b %-d").to_string()
    } else if days < 365 {
        format!("{} mo", days / 30)
    } else {
        format!("{} yr", days / 365)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask(id: &str, state: MaskedState, last: Option<&str>) -> MaskedEmailView {
        MaskedEmailView {
            id: id.into(),
            email: format!("{id}@example.org"),
            state,
            for_domain: format!("{id}.example.com"),
            description: format!("About {id}"),
            created_at: None,
            last_message_at: last.and_then(|s| s.parse().ok()),
            created_by: "web".into(),
            local_messages: 1,
            local_in_spam: u32::from(id == "b"),
        }
    }

    fn now() -> Timestamp {
        "2026-10-23T14:00:00Z".parse().unwrap()
    }

    fn screen() -> MaskedScreen {
        MaskedScreen::new(
            vec![
                mask("a", MaskedState::Enabled, Some("2026-10-20T10:00:00Z")),
                mask("b", MaskedState::Enabled, Some("2026-10-23T09:12:00Z")),
                mask("c", MaskedState::Pending, None),
                mask("d", MaskedState::Deleted, Some("2024-01-01T00:00:00Z")),
                mask("e", MaskedState::Enabled, Some("2026-01-01T00:00:00Z")),
            ],
            now(),
        )
    }

    fn ids(s: &MaskedScreen) -> Vec<&str> {
        s.shown.iter().map(|&i| s.all[i].id.as_str()).collect()
    }

    #[test]
    fn the_silent_filter_shows_what_the_count_counts() {
        let mut s = screen();
        s.show = [true; 4];
        s.silent = SilentFilter::Silent;
        s.refilter(None);
        // "d" has old mail but is deleted, so it is not silent.
        assert_eq!(ids(&s), ["e"]);
        assert_eq!(s.shown.len(), s.silent(now()));
        s.silent = s.silent.next();
        assert_eq!(s.silent, SilentFilter::Active);
        s.refilter(None);
        // "c" never got mail: neither silent nor active.
        assert_eq!(ids(&s), ["b", "a"]);
        s.silent = s.silent.next();
        assert_eq!(s.silent, SilentFilter::Any);
        s.refilter(None);
        assert_eq!(s.shown.len(), 5);
        assert_eq!(SilentFilter::Silent.name(), "silent");
    }

    #[test]
    fn created_by_cycles_through_the_values_without_case() {
        let mut all = screen().all;
        all[0].created_by = "mejl".into();
        all[1].created_by = "Web".into();
        all[3].created_by = "pw manager".into();
        all[4].created_by = "  ".into();
        let mut s = MaskedScreen::new(all, now());
        s.show = [true; 4];
        // The first spelling seen is kept.
        assert_eq!(s.creators(), ["mejl", "pw manager", "Web"]);
        s.cycle_created_by();
        assert_eq!(s.created_by.as_deref(), Some("mejl"));
        s.refilter(None);
        assert_eq!(ids(&s), ["a"]);
        s.cycle_created_by();
        s.cycle_created_by();
        assert_eq!(s.created_by.as_deref(), Some("Web"));
        s.refilter(None);
        // "c" says "web" and "b" says "Web".
        assert_eq!(ids(&s), ["b", "c"]);
        s.cycle_created_by();
        assert_eq!(s.created_by, None);
        // A value no address has any more starts over at the first one.
        s.created_by = Some("gone".into());
        s.cycle_created_by();
        assert_eq!(s.created_by.as_deref(), Some("mejl"));
    }

    #[test]
    fn focus_clears_the_filters_that_hide_the_address() {
        let mut s = screen();
        s.silent = SilentFilter::Silent;
        s.created_by = Some("mejl".into());
        s.refilter(None);
        assert!(s.shown.is_empty());
        s.focus("a");
        assert_eq!(s.selected().unwrap().id, "a");
        assert_eq!(s.silent, SilentFilter::Any);
        assert_eq!(s.created_by, None);
    }

    #[test]
    fn hides_pending_and_deleted_and_sorts_by_last_mail() {
        let s = screen();
        assert_eq!(ids(&s), ["b", "a", "e"]);
        assert_eq!(s.counts(), [3, 0, 1, 1]);
        assert_eq!(s.silent(now()), 1);
        assert_eq!(s.with_spam(), 1);
    }

    #[test]
    fn filters_and_keeps_the_cursor() {
        let mut s = screen();
        s.step(1);
        assert_eq!(s.selected().unwrap().id, "a");
        s.show[2] = true;
        s.refilter(None);
        assert_eq!(
            s.selected().unwrap().id,
            "a",
            "the cursor stays on its address"
        );
        s.filter.set("C.EXAMPLE");
        s.refilter(None);
        assert_eq!(s.shown.len(), 1);
        assert_eq!(s.selected().unwrap().id, "c");
        s.focus("d");
        assert_eq!(s.selected().unwrap().id, "d");
        assert!(s.show[3]);
        s.reload(Vec::new(), now());
        assert!(s.selected().is_none());
        s.step(1);
    }

    #[test]
    fn last_seen_formats() {
        let now: Timestamp = "2026-10-23T14:02:31Z".parse().unwrap();
        let now = now.to_zoned(jiff::tz::TimeZone::UTC);
        let at = |s: &str| Some(s.parse::<Timestamp>().unwrap());
        assert_eq!(last_seen(at("2026-10-23T09:12:00Z"), &now), "09:12");
        assert_eq!(last_seen(at("2026-10-20T09:12:00Z"), &now), "Tue");
        assert_eq!(last_seen(at("2026-10-01T09:12:00Z"), &now), "Oct 1");
        assert_eq!(last_seen(at("2026-06-01T09:12:00Z"), &now), "4 mo");
        assert_eq!(last_seen(at("2024-06-01T09:12:00Z"), &now), "2 yr");
        assert_eq!(last_seen(None, &now), "never");
        assert_eq!(state_name(MaskedState::Pending), "pending");
    }
}
