//! The fictional demo mailbox from docs/mockups.html.
//!
//! All names, addresses and companies are invented, and every address uses
//! an example domain. Dates are written as they appear in the mock-ups
//! (Friday 2026-10-23, 14:02:31 UTC) and shifted by the distance between
//! that instant and the requested "now", so the list reads the same way at
//! any time.

use jiff::{SignedDuration, Timestamp};

use super::memory::{MemoryData, NewEmail};
use super::{
    AccountView, AttachmentView, BodyView, ContactView, IdentityView, MailboxId, MailboxRole,
    MaskedEmailView, MaskedState, Participant, SavedSearchView,
};

/// The instant the mock-ups were drawn at.
pub(crate) const MOCKUP_NOW: &str = "2026-10-23T14:02:31Z";

const INBOX: &str = "mb-inbox";
const DRAFTS: &str = "mb-drafts";
const SENT: &str = "mb-sent";
const ARCHIVE: &str = "mb-archive";
const JUNK: &str = "mb-junk";
const TRASH: &str = "mb-trash";
const NEWSLETTERS: &str = "mb-newsletters";
const RECEIPTS: &str = "mb-receipts";
const PROJECTS: &str = "mb-projects";
const GARDEN: &str = "mb-garden";
const HALL: &str = "mb-hall";
const TRAVEL: &str = "mb-travel";

const MIRA: (&str, &str) = ("Mira Holt", "mira@example.org");

/// A name and an address.
type Person = (&'static str, &'static str);

/// An older inbox conversation: subject, sender, date, preview, the masked
/// address it went to, and how many messages it has.
type Older = (
    &'static str,
    Person,
    &'static str,
    &'static str,
    Option<Person>,
    usize,
);

struct Builder {
    data: MemoryData,
    shift: SignedDuration,
    next: usize,
}

/// One message of a conversation in the fixture.
struct Msg<'a> {
    from: (&'a str, &'a str),
    to: &'a [(&'a str, &'a str)],
    at: &'a str,
    preview: &'a str,
    mailboxes: &'a [&'a str],
    unread: bool,
    flagged: bool,
}

impl<'a> Msg<'a> {
    fn new(from: (&'a str, &'a str), at: &'a str, preview: &'a str) -> Self {
        Msg {
            from,
            to: &[MIRA],
            at,
            preview,
            mailboxes: &[INBOX],
            unread: false,
            flagged: false,
        }
    }

    fn unread(mut self) -> Self {
        self.unread = true;
        self
    }

    fn flagged(mut self) -> Self {
        self.flagged = true;
        self
    }

    fn to(mut self, to: &'a [(&'a str, &'a str)]) -> Self {
        self.to = to;
        self
    }

    fn mine(mut self, to: &'a [(&'a str, &'a str)]) -> Self {
        self.from = MIRA;
        self.to = to;
        self.mailboxes = &[SENT];
        self
    }

    fn in_boxes(mut self, mailboxes: &'a [&'a str]) -> Self {
        self.mailboxes = mailboxes;
        self
    }
}

impl Builder {
    fn at(&self, canonical: &str) -> Timestamp {
        let base: Timestamp = canonical.parse().unwrap_or(Timestamp::UNIX_EPOCH);
        base.checked_add(self.shift).unwrap_or(base)
    }

    /// Adds a conversation and returns the email ids, oldest first.
    fn thread(&mut self, subject: &str, msgs: &[Msg<'_>]) -> Vec<String> {
        self.next += 1;
        let thread = format!("t{:03}", self.next);
        let mut ids = Vec::new();
        for (i, m) in msgs.iter().enumerate() {
            let id = format!("{thread}-{i}");
            let subject = if i == 0 || subject.starts_with("Re: ") {
                subject.to_owned()
            } else {
                format!("Re: {subject}")
            };
            let mut e = NewEmail::new(
                id.as_str(),
                thread.as_str(),
                Participant::new(m.from.0, m.from.1),
                subject,
                self.at(m.at),
                m.mailboxes[0],
            );
            e.mailboxes = m.mailboxes.iter().map(|b| MailboxId::from(*b)).collect();
            e.to = m.to.iter().map(|(n, a)| Participant::new(*n, *a)).collect();
            e.preview = m.preview.to_owned();
            e.unread = m.unread;
            e.flagged = m.flagged;
            e.message_id = Some(format!("{id}@mail.example.org"));
            e.references = ids
                .iter()
                .map(|p: &String| format!("{p}@mail.example.org"))
                .collect();
            self.data.push_email(e);
            self.data.add_body(BodyView {
                email_id: id.as_str().into(),
                text: Some(plain_body(m)),
                html: None,
                attachments: Vec::new(),
                headers: vec![
                    ("From".into(), format!("{} <{}>", m.from.0, m.from.1)),
                    ("Message-ID".into(), format!("<{id}@mail.example.org>")),
                ],
            });
            ids.push(id);
        }
        ids
    }
}

fn plain_body(m: &Msg<'_>) -> String {
    let first = m.from.0.split(' ').next().unwrap_or(m.from.0);
    format!(
        "Hi,\n\n{}.\n\nTalk soon,\n{first}\n",
        m.preview.trim_end_matches(['.', ',', ' '])
    )
}

const JUNIPER: (&str, &str) = ("Juniper Tran", "juniper@garden.example.com");
const OWEN: (&str, &str) = ("Owen Castillo", "owen.castillo@example.net");
const LARKSPUR_HELP: (&str, &str) = ("Larkspur Support", "help@shop.example.com");
const LARKSPUR_ORDERS: (&str, &str) = ("Larkspur Outfitters", "orders@shop.example.com");
const LENA: (&str, &str) = ("Lena Okafor", "lena.okafor@work.example.com");
const LIBRARY: (&str, &str) = ("Northgate Library", "noreply@library.example.org");
const PRIYA: (&str, &str) = ("Priya Raman", "priya.raman@example.net");
const TESSELLATE: (&str, &str) = ("Tessellate Weekly", "editor@news.example.net");
const MARCO: (&str, &str) = ("Marco Bellini", "marco@bellini.example.com");
const CITY: (&str, &str) = ("City Notices", "notices@city.example.org");
const DANA: (&str, &str) = ("Dana Whitfield", "dana.whitfield@example.com");
const KESTREL: (&str, &str) = ("Kestrel Bank", "statements@bank.example.com");
const SAM: (&str, &str) = ("Sam Ito", "sam.ito@example.net");
const RAVI: (&str, &str) = ("Ravi Menon", "ravi.menon@example.com");
const HOSTING: (&str, &str) = ("Example Hosting", "billing@hosting.example.com");
const GRACE: (&str, &str) = ("Grace Lindqvist", "grace.lindqvist@example.org");
const DENTAL: (&str, &str) = ("Harbor Dental", "reminders@dental.example.com");
const FELIX: (&str, &str) = ("Felix Moreau", "felix.moreau@example.net");
const WREN: (&str, &str) = ("Wren Analytics", "reports@wren.example.com");
const NOOR: (&str, &str) = ("Noor Haddad", "noor.haddad@example.com");
const THEO: (&str, &str) = ("Theo Park", "theo.park@example.net");
const COOP: (&str, &str) = ("Maple Street Co-op", "members@coop.example.org");
const INES: (&str, &str) = ("Ines Duarte", "ines.duarte@example.com");
const BEA: (&str, &str) = ("Bea Kowalski", "bea.kowalski@example.net");
const CALDER: (&str, &str) = ("Calder Insurance", "policies@insure.example.com");
const PIA: (&str, &str) = ("Pia Sorensen", "pia.sorensen@example.org");
const HUGO: (&str, &str) = ("Hugo Brandt", "hugo.brandt@example.com");

const MASK_LARKSPUR: (&str, &str) = ("", "kettle.fern4821@example.org");
const MASK_LIBRARY: (&str, &str) = ("", "bright.otter2207@example.org");
const MASK_BANK: (&str, &str) = ("", "maple.drift5513@example.org");
const MASK_DENTAL: (&str, &str) = ("", "amber.loom3390@example.org");
const MASK_CALDER: (&str, &str) = ("", "violet.quay6641@example.org");
const MASK_NEWS: (&str, &str) = ("", "sunny.flax1189@example.org");
const MASK_WREN: (&str, &str) = ("", "minty.cove3317@example.org");
const MASK_COOP: (&str, &str) = ("", "tidal.fig4402@example.org");

const JUNIPER_HTML: &str = r#"<p>Swapping Saturday works for me, can you take the 14th instead? Then Owen covers the 21st and we are set.</p>
<p>The compost delivery is on the 9th, so whoever is on that week please be there by 8. I will leave the shed key with Noor.</p>
<p>I have updated the shared <a href="https://garden.example.com/rota">rota</a>.</p>
<blockquote>
<p>On Mon, Mira Holt wrote:</p>
<p>Thanks! I cannot do the 7th, could someone swap?<br>
I am away that weekend visiting my sister,<br>
and back on the Monday evening.<br>
Happy to take any Saturday after that,<br>
or two if it helps balance things out.<br>
I can also bring the spare hose<br>
and the long handled shears<br>
for the hedge on plot 9.<br>
See you at the shed,<br>
Mira</p>
</blockquote>
<p>Juniper<br>-- <br>Juniper Tran<br>Garden club secretary<br>garden.example.com</p>"#;

const ORDER_HTML: &str = r#"<html><body>
<img src="https://track.example.com/open.gif" width="1" height="1">
<img src="https://shop.example.com/pixel.png" width="1" height="1">
<h1>ORDER UPDATE</h1>
<img src="https://shop.example.com/banner.jpg" width="600" height="120" alt="">
<p>Hi Mira,</p>
<p>Good news: we changed the delivery address and your order has shipped. You can follow the parcel with the link below.</p>
<p><a href="https://shop.example.com/track/48213">[ Track your parcel ]</a></p>
<table>
<tr><th>Item</th><th>Qty</th><th>Price</th></tr>
<tr><td>Merino hiking socks, 2-pack</td><td>1</td><td>18.00</td></tr>
<tr><td>Trail bottle, 750 ml</td><td>2</td><td>24.00</td></tr>
<tr><td>Delivery</td><td></td><td>0.00</td></tr>
<tr><td><b>Total</b></td><td></td><td><b>42.00</b></td></tr>
</table>
<p>Questions? Reply to this email or visit our <a href="https://shop.example.com/help">help centre</a>.</p>
<p>Manage your <a href="https://shop.example.com/preferences">email preferences</a>.</p>
<blockquote>
<p>On Oct 21, Mira Holt wrote:</p>
<p>Hi,<br>could you ship to my work address instead?<br>It is 4 Quay Road,<br>Harbourside,<br>second floor reception.<br>
The office is open from 8 to 6<br>on weekdays,<br>and someone at the desk<br>can sign for it.<br>
If the parcel has already left,<br>please let me know<br>and I will collect it<br>from the depot instead.<br>
Order number 48213,<br>placed on the 20th.<br>Thanks for your help,<br>Mira Holt<br>kettle.fern4821@example.org</p>
</blockquote>
</body></html>"#;

pub(crate) fn demo(now: Timestamp) -> MemoryData {
    let canonical: Timestamp = MOCKUP_NOW.parse().unwrap_or(now);
    let shift = now.duration_since(canonical);
    let mut b = Builder {
        data: MemoryData::new(AccountView {
            name: MIRA.0.into(),
            email: MIRA.1.into(),
        }),
        shift,
        next: 0,
    };
    mailboxes(&mut b.data);
    people(&mut b.data);
    inbox(&mut b);
    elsewhere(&mut b);
    b.data.reindex();
    b.data
}

fn mailboxes(d: &mut MemoryData) {
    let roles = [
        (INBOX, "Inbox", MailboxRole::Inbox),
        (DRAFTS, "Drafts", MailboxRole::Drafts),
        (SENT, "Sent", MailboxRole::Sent),
        (ARCHIVE, "Archive", MailboxRole::Archive),
        (JUNK, "Spam", MailboxRole::Junk),
        (TRASH, "Trash", MailboxRole::Trash),
    ];
    for (i, (id, name, role)) in roles.into_iter().enumerate() {
        d.add_mailbox(id, None, name, Some(role), i as u32);
    }
    d.add_mailbox(NEWSLETTERS, None, "Newsletters", None, 10);
    d.add_mailbox(RECEIPTS, None, "Receipts", None, 11);
    d.add_mailbox(PROJECTS, None, "Projects", None, 12);
    d.add_mailbox(GARDEN, Some(PROJECTS.into()), "Garden club", None, 1);
    d.add_mailbox(HALL, Some(PROJECTS.into()), "Hall repairs", None, 2);
    d.add_mailbox(TRAVEL, None, "Travel", None, 13);
}

fn people(d: &mut MemoryData) {
    for (id, name, email) in [
        ("i1", "Mira Holt", "mira@example.org"),
        ("i2", "Mira Holt", "hello@example.org"),
        ("i3", "Mira at work", "mira.holt@work.example.com"),
    ] {
        d.add_identity(IdentityView {
            id: id.into(),
            name: name.into(),
            email: email.into(),
            signature: if id == "i3" {
                String::new()
            } else {
                "Mira".into()
            },
        });
    }
    let canonical = |s: &str| s.parse::<Timestamp>().ok();
    let masks: [(&str, &str, &str, MaskedState, &str); 14] = [
        (
            "kettle.fern4821@example.org",
            "Larkspur shop",
            "shop.example.com",
            MaskedState::Enabled,
            "mejl",
        ),
        (
            "bright.otter2207@example.org",
            "Northgate Library",
            "library.example.org",
            MaskedState::Enabled,
            "web",
        ),
        (
            "maple.drift5513@example.org",
            "Kestrel Bank",
            "bank.example.com",
            MaskedState::Enabled,
            "pw manager",
        ),
        (
            "quiet.heron7731@example.org",
            "Promo contest",
            "win.example.net",
            MaskedState::Disabled,
            "web",
        ),
        (
            "amber.loom3390@example.org",
            "Harbor Dental",
            "dental.example.com",
            MaskedState::Enabled,
            "mejl",
        ),
        (
            "cedar.pixel0408@example.org",
            "",
            "cabins.example.net",
            MaskedState::Pending,
            "pw manager",
        ),
        (
            "violet.quay6641@example.org",
            "Calder Insurance",
            "insure.example.com",
            MaskedState::Enabled,
            "web",
        ),
        (
            "sunny.flax1189@example.org",
            "Tessellate Weekly",
            "news.example.net",
            MaskedState::Enabled,
            "mejl",
        ),
        (
            "rusty.gale9024@example.org",
            "Old forum",
            "forum.example.net",
            MaskedState::Disabled,
            "web",
        ),
        (
            "minty.cove3317@example.org",
            "Wren Analytics",
            "wren.example.com",
            MaskedState::Enabled,
            "mejl",
        ),
        (
            "hazel.brook5150@example.org",
            "Breached shop",
            "leaky.example.com",
            MaskedState::Deleted,
            "web",
        ),
        (
            "pearl.moss7720@example.org",
            "Conference 2025",
            "conf.example.org",
            MaskedState::Enabled,
            "web",
        ),
        (
            "tidal.fig4402@example.org",
            "Maple Street Co-op",
            "coop.example.org",
            MaskedState::Enabled,
            "web",
        ),
        (
            "stony.wick2815@example.org",
            "Old phone plan",
            "mobile.example.net",
            MaskedState::Deleted,
            "web",
        ),
    ];
    // Last mail and cached counts from the Masked Email mock-up.
    let stats: [(Option<&str>, u32, u32); 14] = [
        (Some("2026-10-23T09:12:00Z"), 23, 0),
        (Some("2026-10-23T07:30:00Z"), 41, 0),
        (Some("2026-10-20T11:00:00Z"), 64, 1),
        (Some("2026-10-21T08:00:00Z"), 312, 2),
        (Some("2026-10-16T08:00:00Z"), 9, 0),
        (None, 0, 0),
        (Some("2026-09-25T08:00:00Z"), 17, 0),
        (Some("2026-10-22T08:00:00Z"), 112, 0),
        (Some("2026-06-20T08:00:00Z"), 88, 0),
        (Some("2026-10-14T08:00:00Z"), 30, 0),
        (Some("2025-10-01T08:00:00Z"), 204, 0),
        (Some("2025-11-20T08:00:00Z"), 6, 0),
        (Some("2026-10-03T08:00:00Z"), 12, 0),
        (Some("2024-10-01T08:00:00Z"), 51, 0),
    ];
    for (i, ((email, description, domain, state, by), (last, mail, spam))) in
        masks.into_iter().zip(stats).enumerate()
    {
        d.add_masked(MaskedEmailView {
            id: format!("me{i}"),
            email: email.into(),
            state,
            for_domain: domain.into(),
            description: description.into(),
            created_at: canonical("2025-03-14T10:00:00Z"),
            last_message_at: last.and_then(canonical),
            created_by: by.into(),
            local_messages: mail,
            local_in_spam: spam,
        });
    }
    for (i, (name, email)) in [
        JUNIPER,
        OWEN,
        LENA,
        PRIYA,
        MARCO,
        DANA,
        SAM,
        RAVI,
        GRACE,
        FELIX,
        NOOR,
        THEO,
        INES,
        BEA,
        ("Nia Newell", "nia@example.net"),
    ]
    .into_iter()
    .enumerate()
    {
        d.add_contact(ContactView {
            id: format!("c{i}"),
            name: name.into(),
            emails: vec![email.into()],
        });
    }
    for (id, name, query, unread) in [
        ("s1", "VIPs, unread", "is:unread from:vip", Some(1)),
        (
            "s2",
            "Masked, 7 days",
            "deliveredto:masked newer_than:7d",
            Some(4),
        ),
        ("s3", "Has attachment", "has:attachment", None),
    ] {
        d.add_saved_search(SavedSearchView {
            id: id.into(),
            name: name.into(),
            query: query.into(),
            unread,
        });
    }
    rules(d);
}

/// The four rules from the rules mock-up, and one earlier copy of the
/// block (before "Receipts to folder" marked mail read and before the
/// Garden club rule existed).
fn rules(d: &mut MemoryData) {
    use mejl_sieve::{
        Action as A, AddressHeader, AddressPart, Condition, MailboxRef, MatchMode, Rule, Test,
        TextOp,
    };
    let from_domain = |domain: &str| {
        Condition::new(Test::Address {
            header: AddressHeader::From,
            part: AddressPart::Domain,
            op: TextOp::Is,
            value: domain.into(),
        })
    };
    let mut news = Rule::new("r_18aa", "Newsletters");
    news.conditions.push(from_domain("news.example.net"));
    news.actions.push(A::MoveTo {
        mailbox: MailboxRef::new(NEWSLETTERS, ["Newsletters"]),
    });
    let mut receipts = Rule::new("r_2c41", "Receipts to folder");
    receipts.match_mode = MatchMode::Any;
    receipts.conditions.push(from_domain("shop.example.com"));
    receipts.conditions.push(Condition::new(Test::Subject {
        op: TextOp::Contains,
        value: "receipt".into(),
    }));
    receipts.actions.push(A::MoveTo {
        mailbox: MailboxRef::new(RECEIPTS, ["Receipts"]),
    });
    let old_receipts = receipts.clone();
    receipts.actions.push(A::MarkRead);
    let mut garden = Rule::new("r_5d07", "Garden club label");
    garden.conditions.push(Condition::new(Test::ListId {
        op: TextOp::Contains,
        value: "garden-club.example.org".into(),
    }));
    garden.actions.push(A::AddLabel {
        mailbox: MailboxRef::new(GARDEN, ["Projects", "Garden club"]),
    });
    let mut promo = Rule::new("r_77e2", "Old promo");
    promo.enabled = false;
    promo.conditions.push(from_domain("promo.example.com"));
    promo.actions.push(A::MoveToTrash);
    for rule in [&news, &receipts, &garden, &promo] {
        d.add_rule(crate::rules::to_json(rule));
    }
    if let Ok(block) = mejl_sieve::render_block(&[news, old_receipts]) {
        d.add_rule_render(super::RuleRenderView {
            id: 1,
            rendered_at: "2026-10-20T18:02:00Z"
                .parse()
                .unwrap_or(Timestamp::UNIX_EPOCH),
            sieve: block.text,
            body_sha256: block.body_sha256,
            confirmed_at: None,
        });
    }
}

fn inbox(b: &mut Builder) {
    // Today.
    let juniper = b.thread(
        "Re: Garden club plot rota",
        &[
            Msg::new(
                JUNIPER,
                "2026-10-19T10:14:00Z",
                "Here is the rota for November, plots 1 to 12 are listed",
            )
            .to(&[MIRA, ("Garden club", "garden-club@example.org")])
            .in_boxes(&[INBOX, GARDEN]),
            Msg::new(
                MIRA,
                "2026-10-19T18:40:00Z",
                "Thanks! I cannot do the 7th, could someone swap with me",
            )
            .mine(&[JUNIPER, ("Garden club", "garden-club@example.org")]),
            Msg::new(
                JUNIPER,
                "2026-10-23T14:02:00Z",
                "Swapping Saturday works for me, can you take the 14th",
            )
            .to(&[MIRA, ("Garden club", "garden-club@example.org")])
            .in_boxes(&[INBOX, GARDEN])
            .unread(),
        ],
    );
    if let Some(latest) = juniper.last() {
        b.data.add_body(BodyView {
            email_id: latest.as_str().into(),
            text: None,
            html: Some(JUNIPER_HTML.into()),
            attachments: Vec::new(),
            headers: vec![
                (
                    "From".into(),
                    "Juniper Tran <juniper@garden.example.com>".into(),
                ),
                (
                    "To".into(),
                    "Mira Holt <mira@example.org>, garden-club@example.org".into(),
                ),
                ("Subject".into(), "Re: Garden club plot rota".into()),
                ("List-Id".into(), "<garden-club.example.org>".into()),
            ],
        });
    }
    b.thread(
        "Draft budget for the hall repairs",
        &[Msg::new(
            OWEN,
            "2026-10-23T13:47:00Z",
            "Attached the spreadsheet. The roof line is the big one",
        )
        .unread()
        .flagged()],
    );
    let order = b.thread(
        "Re: Your order 48213 has shipped",
        &[
            Msg::new(
                LARKSPUR_ORDERS,
                "2026-10-20T16:05:00Z",
                "Thanks for your order. Order 48213 is confirmed and will ship soon",
            )
            .to(&[MASK_LARKSPUR])
            .in_boxes(&[INBOX, RECEIPTS]),
            Msg::new(
                MIRA,
                "2026-10-21T08:31:00Z",
                "Hi, could you ship to my work address instead? It is 4 Quay Road",
            )
            .mine(&[LARKSPUR_HELP]),
            Msg::new(
                LARKSPUR_HELP,
                "2026-10-23T09:12:00Z",
                "Good news: we changed the delivery address and",
            )
            .to(&[MASK_LARKSPUR])
            .unread(),
        ],
    );
    if let Some(latest) = order.last() {
        b.data.add_body(BodyView {
            email_id: latest.as_str().into(),
            text: None,
            html: Some(ORDER_HTML.into()),
            attachments: vec![AttachmentView {
                blob_id: "blob-invoice-48213".into(),
                name: "invoice-48213.pdf".into(),
                mime_type: "application/pdf".into(),
                size: 84 * 1024,
            }],
            headers: vec![
                (
                    "From".into(),
                    "Larkspur Support <help@shop.example.com>".into(),
                ),
                ("To".into(), "kettle.fern4821@example.org".into()),
                ("Subject".into(), "Re: Your order 48213 has shipped".into()),
                (
                    "X-FM-Masked-Email".into(),
                    "kettle.fern4821@example.org".into(),
                ),
            ],
        });
        mark_attachment(&mut b.data, latest);
    }
    b.thread(
        "Re: Thursday standup notes",
        &[
            Msg::new(
                MIRA,
                "2026-10-22T17:10:00Z",
                "Notes from today, the release date is the open question",
            )
            .mine(&[LENA]),
            Msg::new(
                LENA,
                "2026-10-23T08:55:00Z",
                "Agreed on moving the release to the 30th",
            ),
        ],
    );
    b.thread(
        "Your reservation is ready to collect",
        &[Msg::new(
            LIBRARY,
            "2026-10-23T07:30:00Z",
            "The book you reserved is waiting at the front desk",
        )
        .to(&[MASK_LIBRARY])],
    );
    // Yesterday.
    b.thread(
        "Re: Trip to the coast in November",
        &[
            Msg::new(
                PRIYA,
                "2026-10-12T19:00:00Z",
                "Shall we do the coast trip again this year",
            ),
            Msg::new(
                MIRA,
                "2026-10-13T08:15:00Z",
                "Yes please, the second weekend works for me",
            )
            .mine(&[PRIYA]),
            Msg::new(
                PRIYA,
                "2026-10-16T21:30:00Z",
                "Found two cabins, one has a sea view",
            ),
            Msg::new(MIRA, "2026-10-17T09:05:00Z", "The sea view one, definitely").mine(&[PRIYA]),
            Msg::new(
                PRIYA,
                "2026-10-22T18:20:00Z",
                "Booked the cabin, link below. Can everyone confirm",
            ),
        ],
    );
    b.thread(
        "Issue 112: tiling window managers",
        &[Msg::new(
            TESSELLATE,
            "2026-10-22T11:05:00Z",
            "This week: layouts, gaps and a short history",
        )
        .to(&[MASK_NEWS])
        .in_boxes(&[INBOX, NEWSLETTERS])],
    );
    let invoice = b.thread(
        "Invoice 2026-118",
        &[Msg::new(
            MARCO,
            "2026-10-22T09:40:00Z",
            "Please find the invoice for October attached",
        )
        .flagged()],
    );
    if let Some(id) = invoice.last() {
        mark_attachment(&mut b.data, id);
    }
    // This week.
    b.thread(
        "Road closure on Elm Street, Oct 28",
        &[Msg::new(
            CITY,
            "2026-10-21T15:00:00Z",
            "Works start at 07:00 and run until late afternoon",
        )],
    );
    b.thread(
        "Re: Recipe for the plum cake",
        &[
            Msg::new(
                MIRA,
                "2026-10-20T20:00:00Z",
                "Could you send the plum cake recipe",
            )
            .mine(&[DANA]),
            Msg::new(
                DANA,
                "2026-10-21T09:30:00Z",
                "Use less sugar than it says, the plums are sweet",
            ),
        ],
    );
    b.thread(
        "Your October statement is ready",
        &[Msg::new(
            KESTREL,
            "2026-10-20T06:00:00Z",
            "Sign in to view your statement. We will never ask",
        )
        .to(&[MASK_BANK])],
    );
    b.thread(
        "Photos from the climbing trip",
        &[Msg::new(
            SAM,
            "2026-10-20T12:45:00Z",
            "Here are the good ones, the rest are mostly blurry",
        )],
    );
    b.thread(
        "Re: Board game night",
        &[
            Msg::new(
                RAVI,
                "2026-10-14T18:00:00Z",
                "Board game night on Friday, who is in",
            ),
            Msg::new(MIRA, "2026-10-14T19:10:00Z", "Count me in").mine(&[RAVI]),
            Msg::new(NOOR, "2026-10-15T08:00:00Z", "Me too, I will bring snacks"),
            Msg::new(
                RAVI,
                "2026-10-19T17:30:00Z",
                "I can bring the new expansion if people want",
            ),
        ],
    );
    b.thread(
        "Renewal notice for example.org",
        &[Msg::new(
            HOSTING,
            "2026-10-19T08:00:00Z",
            "Your domain renews automatically on 12 November",
        )],
    );
    // Earlier.
    let earlier: [Older; 16] = [
        (
            "Quarterly newsletter draft",
            GRACE,
            "2026-10-17T10:00:00Z",
            "Comments welcome before Friday, especially on",
            None,
            1,
        ),
        (
            "Appointment reminder: 3 November",
            DENTAL,
            "2026-10-16T10:00:00Z",
            "Reply C to confirm or call us to rearrange",
            Some(MASK_DENTAL),
            1,
        ),
        (
            "Re: Moving the book club",
            FELIX,
            "2026-10-15T10:00:00Z",
            "Second Tuesday works better for most of us",
            None,
            6,
        ),
        (
            "Weekly report: 3 new signups",
            WREN,
            "2026-10-14T10:00:00Z",
            "Traffic is up 4 percent on last week",
            Some(MASK_WREN),
            1,
        ),
        (
            "Seed order list",
            JUNIPER,
            "2026-10-12T10:00:00Z",
            "Final list attached. Shout if I missed anything",
            None,
            1,
        ),
        (
            "Re: Spare room this weekend?",
            NOOR,
            "2026-10-10T10:00:00Z",
            "Yes, the spare room is free, come Friday",
            None,
            2,
        ),
        (
            "Receipt for order 47720",
            LARKSPUR_ORDERS,
            "2026-10-08T10:00:00Z",
            "Thanks for shopping with us. Your receipt is",
            Some(MASK_LARKSPUR),
            1,
        ),
        (
            "Slides from the meetup",
            THEO,
            "2026-10-06T10:00:00Z",
            "Slides and the recording link are below",
            None,
            1,
        ),
        (
            "Membership renewal",
            COOP,
            "2026-10-03T10:00:00Z",
            "Your membership is due for renewal this month",
            Some(MASK_COOP),
            1,
        ),
        (
            "Re: Translation review",
            INES,
            "2026-10-01T10:00:00Z",
            "Two small fixes in section 4, otherwise good",
            None,
            3,
        ),
        (
            "Hall booking calendar",
            OWEN,
            "2026-09-29T10:00:00Z",
            "Shared the calendar for the winter term",
            None,
            1,
        ),
        (
            "Re: Running club route",
            BEA,
            "2026-09-27T10:00:00Z",
            "The river loop is flooded, take the hill route",
            None,
            2,
        ),
        (
            "Policy documents",
            CALDER,
            "2026-09-25T10:00:00Z",
            "Your renewed policy documents are attached",
            Some(MASK_CALDER),
            1,
        ),
        (
            "Offsite agenda",
            LENA,
            "2026-09-22T10:00:00Z",
            "Draft agenda for the offsite, comments welcome",
            None,
            1,
        ),
        (
            "Choir rehearsal moved",
            PIA,
            "2026-09-20T10:00:00Z",
            "We are in the small hall this week",
            None,
            1,
        ),
        (
            "Old flat deposit",
            HUGO,
            "2026-09-18T10:00:00Z",
            "The deposit came back in full, finally",
            None,
            1,
        ),
    ];
    for (subject, from, at, preview, mask, count) in earlier {
        let to: &[(&str, &str)] = match mask {
            Some(m) if m == MASK_DENTAL => &[MASK_DENTAL],
            Some(m) if m == MASK_WREN => &[MASK_WREN],
            Some(m) if m == MASK_LARKSPUR => &[MASK_LARKSPUR],
            Some(m) if m == MASK_COOP => &[MASK_COOP],
            Some(m) if m == MASK_CALDER => &[MASK_CALDER],
            _ => &[MIRA],
        };
        let mut msgs = Vec::new();
        // Earlier replies, alternating with Mira, a day apart.
        let base: Timestamp = at.parse().unwrap_or(Timestamp::UNIX_EPOCH);
        let stamps: Vec<String> = (1..count)
            .rev()
            .map(|k| {
                let hours = i64::try_from(k).unwrap_or(0) * 20;
                base.checked_sub(SignedDuration::from_hours(hours))
                    .unwrap_or(base)
                    .to_string()
            })
            .collect();
        let reply_to = [from];
        for (k, stamp) in stamps.iter().enumerate() {
            let m = if k % 2 == 0 {
                Msg::new(MIRA, stamp, "Sounds good, let me check and get back to you")
                    .mine(&reply_to)
            } else {
                Msg::new(from, stamp, "A quick follow up on the earlier note")
            };
            msgs.push(m);
        }
        msgs.push(Msg::new(from, at, preview).to(to));
        b.thread(subject, &msgs);
    }
    filler(b);
}

/// Older inbox conversations, so the inbox has the mock-ups' 214
/// conversations and 12 unread.
fn filler(b: &mut Builder) {
    const SENDERS: [(&str, &str); 10] = [GRACE, THEO, PIA, HUGO, SAM, DANA, INES, BEA, FELIX, RAVI];
    const TOPICS: [&str; 12] = [
        "Allotment fees for next year",
        "Bike repair quote",
        "Photos from the weekend",
        "Book club pick for December",
        "Quiz night results",
        "Plant sale on Sunday",
        "Notes from the residents meeting",
        "Cabin options for spring",
        "Volunteer rota",
        "Lost and found at the hall",
        "Recipe swap",
        "Carpool for the match",
    ];
    let start: Timestamp = "2026-09-17T09:00:00Z"
        .parse()
        .unwrap_or(Timestamp::UNIX_EPOCH);
    let unread_at = [3usize, 17, 29, 44, 58, 71, 96, 120, 151];
    for i in 0..184usize {
        let hours = i64::try_from(i).unwrap_or(0) * 37;
        let at = start
            .checked_sub(SignedDuration::from_hours(hours))
            .unwrap_or(start)
            .to_string();
        let from = SENDERS[i % SENDERS.len()];
        let topic = TOPICS[i % TOPICS.len()];
        let preview = "A short note about this, details inside";
        let mut m = Msg::new(from, &at, preview);
        if unread_at.contains(&i) {
            m = m.unread();
        }
        b.thread(topic, &[m]);
    }
}

/// Mail outside the inbox, so every sidebar count matches the mock-up.
fn elsewhere(b: &mut Builder) {
    b.thread(
        "Booking for 14 to 16 November",
        &[Msg::new(
            MIRA,
            "2026-10-23T11:20:00Z",
            "We would like to book the lakeside cabin for two nights",
        )
        .to(&[("", "bookings@cabins.example.net")])
        .in_boxes(&[DRAFTS])],
    );
    for (i, subject) in [
        "You have won a prize",
        "Urgent account notice",
        "Cheap watches",
    ]
    .into_iter()
    .enumerate()
    {
        let at = format!("2026-10-2{}T03:00:00Z", i + 1);
        b.thread(
            subject,
            &[Msg::new(
                ("Prize Desk", "desk@win.example.net"),
                &at,
                "Click here to claim",
            )
            .to(&[("", "quiet.heron7731@example.org")])
            .in_boxes(&[JUNK])
            .unread()],
        );
    }
    for i in 0..5 {
        let at = format!("2026-10-0{}T07:00:00Z", i + 1);
        let subject = format!("Issue {}: terminal colours", 107 + i);
        b.thread(
            &subject,
            &[
                Msg::new(TESSELLATE, &at, "This week in terminals and text tools")
                    .to(&[MASK_NEWS])
                    .in_boxes(&[NEWSLETTERS])
                    .unread(),
            ],
        );
    }
    b.thread(
        "Compost bins for plot 9",
        &[Msg::new(
            JUNIPER,
            "2026-10-21T07:45:00Z",
            "The new bins arrive on Saturday morning",
        )
        .in_boxes(&[GARDEN])
        .unread()],
    );
    b.thread(
        "Ferry times for the coast",
        &[Msg::new(
            PRIYA,
            "2026-10-02T12:00:00Z",
            "The 9:40 ferry is the one we want",
        )
        .in_boxes(&[TRAVEL])],
    );
    b.thread(
        "Old newsletter",
        &[Msg::new(TESSELLATE, "2026-08-01T07:00:00Z", "An older issue").in_boxes(&[TRASH])],
    );
    b.thread(
        "Gutter quote",
        &[Msg::new(
            OWEN,
            "2026-09-02T07:00:00Z",
            "Quote for the gutters attached",
        )
        .in_boxes(&[ARCHIVE, HALL])],
    );
}

fn mark_attachment(d: &mut MemoryData, email: &str) {
    d.set_has_attachment(&email.into());
}
