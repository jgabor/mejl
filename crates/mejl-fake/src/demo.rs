//! The fictional demo account behind `mejl --demo`, matching the mock-ups in
//! `docs/mockups.html`.
//!
//! Every person, company and address here is invented, and every domain is
//! under example.com, example.org or example.net. Times are written as they
//! appear in the mock-ups, relative to Friday 2026-10-23 14:27 UTC, and are
//! shifted so that moment becomes the `now` passed in.

use std::collections::BTreeMap;

use jiff::{SignedDuration, Timestamp};

use crate::model::{
    Address, Attachment, Body, ContactCard, Identity, MaskedEmail, MaskedEmailState,
};
use crate::store::{NewEmail, Store};

/// The moment the mock-ups were drawn: Friday 2026-10-23 14:27 UTC.
const REF_NOW: Timestamp = Timestamp::constant(1_792_765_620, 0);

pub(crate) const USERNAME: &str = "mira@example.org";
pub(crate) const DISPLAY_NAME: &str = "Mira Holt";

fn p(name: &str, email: &str) -> Address {
    Address::new(name, email)
}

struct People {
    me: Address,
    work: Address,
    juniper: Address,
    owen: Address,
    lena: Address,
    priya: Address,
    marco: Address,
    dana: Address,
    sam: Address,
    ravi: Address,
    grace: Address,
    felix: Address,
    noor: Address,
    theo: Address,
    ines: Address,
    bea: Address,
    pia: Address,
    hugo: Address,
}

fn people() -> People {
    People {
        me: p("Mira Holt", USERNAME),
        work: p("Mira at work", "mira.holt@work.example.com"),
        juniper: p("Juniper Tran", "juniper@garden.example.com"),
        owen: p("Owen Castillo", "owen@castillo.example.net"),
        lena: p("Lena Okafor", "lena.okafor@work.example.com"),
        priya: p("Priya Raman", "priya@raman.example.org"),
        marco: p("Marco Bellini", "marco@bellini.example.com"),
        dana: p("Dana Whitfield", "dana.whitfield@example.net"),
        sam: p("Sam Ito", "sam.ito@example.com"),
        ravi: p("Ravi Menon", "ravi@menon.example.org"),
        grace: p("Grace Lindqvist", "grace@lindqvist.example.net"),
        felix: p("Felix Moreau", "felix.moreau@example.org"),
        noor: p("Noor Haddad", "noor@haddad.example.com"),
        theo: p("Theo Park", "theo.park@example.net"),
        ines: p("Ines Duarte", "ines@duarte.example.org"),
        bea: p("Bea Kowalski", "bea@kowalski.example.com"),
        pia: p("Pia Sorensen", "pia.sorensen@example.org"),
        hugo: p("Hugo Brandt", "hugo.brandt@example.net"),
    }
}

/// Mailbox ids by short name.
struct Boxes(BTreeMap<&'static str, String>);

impl Boxes {
    fn ids(&self, names: &[&str]) -> Vec<String> {
        names
            .iter()
            .filter_map(|n| self.0.get(n).cloned())
            .collect()
    }
}

struct Demo<'a> {
    store: &'a mut Store,
    now: Timestamp,
    boxes: Boxes,
    /// References chain per message id, for threading replies.
    refs: BTreeMap<String, Vec<String>>,
    seq: u32,
}

struct Msg<'d, 'a> {
    demo: &'d mut Demo<'a>,
    new: NewEmail,
    recipients: Vec<String>,
    outgoing: bool,
}

impl<'a> Demo<'a> {
    fn at(&self, reference: &str) -> Timestamp {
        let t: Timestamp = reference.parse().unwrap_or(REF_NOW);
        let back = REF_NOW.duration_since(t);
        self.now.checked_sub(back).unwrap_or(self.now)
    }

    fn msg<'d>(&'d mut self, at: &str, from: &Address, subject: &str) -> Msg<'d, 'a> {
        self.seq += 1;
        let received = self.at(at);
        let domain = from
            .email
            .rsplit_once('@')
            .map(|(_, d)| d)
            .unwrap_or("example.org");
        let new = NewEmail {
            received_at: Some(received),
            sent_at: Some(
                received
                    .checked_sub(SignedDuration::from_secs(42))
                    .unwrap_or(received),
            ),
            message_id: Some(format!(
                "demo{}.{}@{domain}",
                self.seq,
                received.as_second()
            )),
            from: Some(vec![from.clone()]),
            subject: Some(subject.to_owned()),
            ..NewEmail::default()
        };
        let outgoing = from.email == USERNAME
            || from.email.ends_with("@work.example.com")
                && from.name.as_deref() == Some("Mira at work");
        Msg {
            demo: self,
            new,
            recipients: Vec::new(),
            outgoing,
        }
    }
}

impl Msg<'_, '_> {
    fn to(mut self, list: &[&Address]) -> Self {
        self.recipients.extend(list.iter().map(|a| a.email.clone()));
        self.new.to = Some(list.iter().map(|a| (*a).clone()).collect());
        self
    }

    fn cc(mut self, list: &[&Address]) -> Self {
        self.recipients.extend(list.iter().map(|a| a.email.clone()));
        self.new.cc = Some(list.iter().map(|a| (*a).clone()).collect());
        self
    }

    fn mailboxes(mut self, names: &[&str]) -> Self {
        self.new.mailbox_ids.extend(self.demo.boxes.ids(names));
        self
    }

    fn seen(mut self) -> Self {
        self.new.keywords.insert("$seen".to_owned());
        self
    }

    fn flagged(mut self) -> Self {
        self.new.keywords.insert("$flagged".to_owned());
        self
    }

    fn answered(mut self) -> Self {
        self.new.keywords.insert("$answered".to_owned());
        self
    }

    fn draft(mut self) -> Self {
        self.new.keywords.insert("$draft".to_owned());
        self.new.keywords.insert("$seen".to_owned());
        self
    }

    fn text(mut self, body: &str) -> Self {
        self.new.text = Some(Body::Text(body.to_owned()));
        self
    }

    fn html(mut self, body: &str) -> Self {
        self.new.html = Some(body.to_owned());
        self
    }

    fn attach(mut self, name: &str, mime: &str, size: u64) -> Self {
        self.new.attachments.push(Attachment {
            name: name.to_owned(),
            mime: mime.to_owned(),
            size,
            inline: false,
            cid: None,
            upload: None,
        });
        self
    }

    fn header(mut self, name: &str, value: &str) -> Self {
        self.new
            .extra_headers
            .push((name.to_owned(), value.to_owned()));
        self
    }

    fn reply_to(mut self, parent: &str) -> Self {
        let mut refs = self.demo.refs.get(parent).cloned().unwrap_or_default();
        refs.push(parent.to_owned());
        self.new.in_reply_to = Some(vec![parent.to_owned()]);
        self.new.references = Some(refs);
        self
    }

    /// Files the message and returns its Message-ID for replies.
    fn send(self) -> String {
        let msgid = self.new.message_id.clone().unwrap_or_default();
        let refs = self.new.references.clone().unwrap_or_default();
        self.demo.refs.insert(msgid.clone(), refs);
        if self.outgoing {
            self.demo.store.insert_email(self.new);
        } else if self.demo.store.deliver(self.new, &self.recipients).is_err() {
            tracing::warn!("demo message to a deleted mask was dropped");
        }
        msgid
    }
}

fn quote(intro: &str, text: &str) -> String {
    let mut out = format!("{intro}\n");
    for line in text.lines() {
        if line.is_empty() {
            out.push_str(">\n");
        } else {
            out.push_str("> ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

const MIRA_SIG: &str = "-- \nMira Holt\nPlot 7, Northgate Garden Club";

/// Builds the demo account in `store`, which must be fresh.
pub(crate) fn fill(store: &mut Store, now: Timestamp) {
    store.begin_bulk();
    let mut boxes = BTreeMap::new();
    for (key, role) in [
        ("inbox", "inbox"),
        ("drafts", "drafts"),
        ("sent", "sent"),
        ("archive", "archive"),
        ("spam", "junk"),
        ("trash", "trash"),
    ] {
        if let Some(id) = store.role_id(role) {
            boxes.insert(key, id);
        }
    }
    boxes.insert(
        "newsletters",
        store.create_mailbox("Newsletters", None, None, 10),
    );
    boxes.insert("receipts", store.create_mailbox("Receipts", None, None, 11));
    let projects = store.create_mailbox("Projects", None, None, 12);
    boxes.insert(
        "garden",
        store.create_mailbox("Garden club", Some(projects.clone()), None, 1),
    );
    boxes.insert(
        "hall",
        store.create_mailbox("Hall repairs", Some(projects.clone()), None, 2),
    );
    boxes.insert("projects", projects);
    boxes.insert("travel", store.create_mailbox("Travel", None, None, 13));

    add_identities(store);
    add_contacts(store, now);
    add_masks(store, now);

    let mut d = Demo {
        store,
        now,
        boxes: Boxes(boxes),
        refs: BTreeMap::new(),
        seq: 0,
    };
    let pp = people();
    inbox_today(&mut d, &pp);
    inbox_this_week(&mut d, &pp);
    inbox_earlier(&mut d, &pp);
    folders(&mut d, &pp);
    set_mask_activity(d.store, now);
    d.store.end_bulk();
}

fn add_identities(store: &mut Store) {
    if let Some(main) = store.identities.values_mut().next() {
        main.name = DISPLAY_NAME.to_owned();
        main.text_signature = "Mira\n\n-- \nMira Holt\nPlot 7, Northgate Garden Club".to_owned();
        main.html_signature =
            "<p>Mira</p><p>-- <br>Mira Holt<br>Plot 7, Northgate Garden Club</p>".to_owned();
    }
    for (name, email, sig) in [
        ("Mira Holt", "hello@example.org", ""),
        (
            "Mira at work",
            "mira.holt@work.example.com",
            "Mira Holt\nProject lead, Quay Road office",
        ),
    ] {
        let id = store.ids.make('I');
        store.identities.insert(
            id.clone(),
            Identity {
                id,
                name: name.to_owned(),
                email: email.to_owned(),
                reply_to: None,
                bcc: None,
                text_signature: sig.to_owned(),
                html_signature: String::new(),
                may_delete: true,
            },
        );
    }
}

fn add_contacts(store: &mut Store, now: Timestamp) {
    let contacts: &[(&str, &str, &[&str])] = &[
        ("Juniper", "Tran", &["juniper@garden.example.com"]),
        ("Owen", "Castillo", &["owen@castillo.example.net"]),
        ("Lena", "Okafor", &["lena.okafor@work.example.com"]),
        ("Priya", "Raman", &["priya@raman.example.org"]),
        ("Marco", "Bellini", &["marco@bellini.example.com"]),
        ("Dana", "Whitfield", &["dana.whitfield@example.net"]),
        ("Sam", "Ito", &["sam.ito@example.com"]),
        ("Ravi", "Menon", &["ravi@menon.example.org"]),
        ("Grace", "Lindqvist", &["grace@lindqvist.example.net"]),
        ("Felix", "Moreau", &["felix.moreau@example.org"]),
        (
            "Noor",
            "Haddad",
            &["noor@haddad.example.com", "noor.haddad@work.example.com"],
        ),
        ("Theo", "Park", &["theo.park@example.net"]),
        ("Ines", "Duarte", &["ines@duarte.example.org"]),
        ("Bea", "Kowalski", &["bea@kowalski.example.com"]),
        ("Nia", "Newell", &["nia@example.net"]),
    ];
    for (i, (given, surname, emails)) in contacts.iter().enumerate() {
        let id = store.ids.make('C');
        let created = now
            .checked_sub(SignedDuration::from_hours(24 * (400 + 17 * i as i64)))
            .unwrap_or(now);
        store.contacts.insert(
            id.clone(),
            ContactCard {
                id,
                uid: format!(
                    "urn:uuid:6d1c{:04x}-8e2f-4a7b-9c3d-0a1b2c3d4e{:02x}",
                    i * 7919,
                    i
                ),
                given: (*given).to_owned(),
                surname: (*surname).to_owned(),
                emails: emails
                    .iter()
                    .enumerate()
                    .map(|(j, e)| {
                        (
                            (*e).to_owned(),
                            Some(if j == 0 { "private" } else { "work" }),
                        )
                    })
                    .collect(),
                created,
            },
        );
    }
}

type MaskRow = (
    &'static str,
    &'static str,
    MaskedEmailState,
    &'static str,
    &'static str,
    &'static str,
);

const MASKS: &[MaskRow] = &[
    (
        "masked-48210",
        "kettle.fern4821@example.org",
        MaskedEmailState::Enabled,
        "Larkspur shop",
        "https://shop.example.com",
        "mejl",
    ),
    (
        "masked-22071",
        "bright.otter2207@example.org",
        MaskedEmailState::Enabled,
        "Northgate Library",
        "https://library.example.org",
        "web",
    ),
    (
        "masked-55130",
        "maple.drift5513@example.org",
        MaskedEmailState::Enabled,
        "Kestrel Bank",
        "https://bank.example.com",
        "Password manager",
    ),
    (
        "masked-77310",
        "quiet.heron7731@example.org",
        MaskedEmailState::Disabled,
        "Promo contest",
        "https://win.example.net",
        "web",
    ),
    (
        "masked-33901",
        "amber.loom3390@example.org",
        MaskedEmailState::Enabled,
        "Harbor Dental",
        "https://dental.example.com",
        "mejl",
    ),
    (
        "masked-04081",
        "cedar.pixel0408@example.org",
        MaskedEmailState::Pending,
        "",
        "https://cabins.example.net",
        "Password manager",
    ),
    (
        "masked-66411",
        "violet.quay6641@example.org",
        MaskedEmailState::Enabled,
        "Calder Insurance",
        "https://insure.example.com",
        "web",
    ),
    (
        "masked-11891",
        "sunny.flax1189@example.org",
        MaskedEmailState::Enabled,
        "Tessellate Weekly",
        "https://news.example.net",
        "mejl",
    ),
    (
        "masked-90241",
        "rusty.gale9024@example.org",
        MaskedEmailState::Disabled,
        "Old forum",
        "https://forum.example.net",
        "web",
    ),
    (
        "masked-33171",
        "minty.cove3317@example.org",
        MaskedEmailState::Enabled,
        "Wren Analytics",
        "https://wren.example.com",
        "mejl",
    ),
    (
        "masked-51501",
        "hazel.brook5150@example.org",
        MaskedEmailState::Deleted,
        "Breached shop",
        "https://leaky.example.com",
        "web",
    ),
    (
        "masked-77201",
        "pearl.moss7720@example.org",
        MaskedEmailState::Enabled,
        "Conference 2025",
        "https://conf.example.org",
        "web",
    ),
    (
        "masked-44021",
        "tidal.fig4402@example.org",
        MaskedEmailState::Enabled,
        "Maple Street Co-op",
        "https://coop.example.org",
        "web",
    ),
    (
        "masked-28151",
        "stony.wick2815@example.org",
        MaskedEmailState::Deleted,
        "Old phone plan",
        "https://mobile.example.net",
        "web",
    ),
    (
        "masked-10481",
        "juniper.sail1048@example.org",
        MaskedEmailState::Enabled,
        "Seed catalogue",
        "https://seeds.example.com",
        "mejl",
    ),
    (
        "masked-55321",
        "copper.wren5532@example.org",
        MaskedEmailState::Pending,
        "",
        "https://tickets.example.org",
        "Password manager",
    ),
    (
        "masked-88901",
        "misty.vale8890@example.org",
        MaskedEmailState::Enabled,
        "Hosting status page",
        "https://status.example.org",
        "web",
    ),
    (
        "masked-27641",
        "olive.crest2764@example.org",
        MaskedEmailState::Disabled,
        "Survey site",
        "https://survey.example.net",
        "web",
    ),
    (
        "masked-33051",
        "lunar.pine3305@example.org",
        MaskedEmailState::Deleted,
        "Old gym",
        "https://gym.example.com",
        "web",
    ),
    (
        "masked-99171",
        "rapid.harbor9917@example.org",
        MaskedEmailState::Enabled,
        "Rail tickets",
        "https://rail.example.net",
        "mejl",
    ),
];

fn add_masks(store: &mut Store, now: Timestamp) {
    for (i, (id, email, state, description, site, by)) in MASKS.iter().enumerate() {
        // Spread creation dates over the last three years; the Larkspur
        // mask is dated 2025-03-14 as in the mock-up.
        let days = if i == 0 {
            588
        } else {
            60 + (i as i64 * 53) % 1000
        };
        let created_at = now
            .checked_sub(SignedDuration::from_hours(24 * days))
            .unwrap_or(now);
        store.masked.insert(
            (*id).to_owned(),
            MaskedEmail {
                id: (*id).to_owned(),
                email: (*email).to_owned(),
                state: *state,
                for_domain: (*site).to_owned(),
                description: (*description).to_owned(),
                url: None,
                created_by: (*by).to_owned(),
                created_at,
                last_message_at: None,
            },
        );
    }
}

/// Last-message times for masks that get no mail in the demo, as the
/// masked email screen in the mock-ups shows them.
fn set_mask_activity(store: &mut Store, now: Timestamp) {
    for (id, days) in [
        ("masked-90241", 120),
        ("masked-51501", 365),
        ("masked-77201", 330),
        ("masked-28151", 730),
        ("masked-10481", 45),
        ("masked-88901", 21),
        ("masked-27641", 200),
        ("masked-33051", 500),
    ] {
        if let Some(m) = store.masked.get_mut(id)
            && m.last_message_at.is_none()
        {
            m.last_message_at = now.checked_sub(SignedDuration::from_hours(24 * days)).ok();
        }
    }
}

fn mask(id: &str) -> Address {
    let email = MASKS
        .iter()
        .find(|m| m.0 == id)
        .map(|m| m.1)
        .unwrap_or(USERNAME);
    Address::bare(email)
}

const LARKSPUR_HTML: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Order update</title>
<style>body{font-family:Helvetica,Arial,sans-serif;color:#222}.btn{background:#2c5f9e;color:#fff;padding:8px 14px;text-decoration:none}td,th{padding:4px 10px;text-align:left}</style>
</head><body>
<div style="display:none;max-height:0;overflow:hidden">Your parcel is on its way. Track it any time.</div>
<h1>ORDER UPDATE</h1>
<p>Hi Mira,</p>
<p>Good news: we changed the delivery address and your order has shipped. You can follow the parcel with the link below.</p>
<p><a class="btn" href="https://shop.example.com/track/48213">Track your parcel</a></p>
<table role="presentation">
<thead><tr><th>Item</th><th>Qty</th><th>Price</th></tr></thead>
<tbody>
<tr><td>Merino hiking socks, 2-pack</td><td>1</td><td>18.00</td></tr>
<tr><td>Trail bottle, 750 ml</td><td>2</td><td>24.00</td></tr>
<tr><td>Delivery</td><td></td><td>0.00</td></tr>
<tr><td><b>Total</b></td><td></td><td><b>42.00</b></td></tr>
</tbody></table>
<p>Questions? Reply to this email or visit our <a href="https://shop.example.com/help">help centre</a>.</p>
<p><a href="https://shop.example.com/preferences">Manage your email preferences</a>.</p>
<img src="https://track.shop.example.com/open/48213.gif" width="1" height="1" alt="">
<img src="https://pixel.example.net/p.gif?u=48213&amp;c=ship" width="1" height="1" alt="">
<blockquote type="cite">
<p>On Wed, 21 Oct 2026 at 08:31, Mira Holt &lt;kettle.fern4821@example.org&gt; wrote:</p>
<p>Hi,</p>
<p>could you ship to my work address instead? It is 4 Quay Road,<br>
second floor, and the front desk takes parcels between 8 and 6.</p>
<p>The order number is 48213. I placed it on Tuesday afternoon<br>
and the confirmation still shows my home address.</p>
<p>If the parcel has already left, no problem, I will be at home<br>
on Saturday morning as well.</p>
<p>Thanks,<br>Mira</p>
<p>On Tue, 20 Oct 2026 at 16:05, Larkspur Outfitters wrote:<br>
&gt; Thanks for your order. Order 48213 is confirmed and will ship soon.<br>
&gt; You will get another message with tracking details.</p>
</blockquote>
</body></html>"#;

const TESSELLATE_HTML: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Tessellate Weekly</title></head>
<body style="font-family:Georgia,serif">
<h1>Tessellate Weekly</h1>
<h2>Issue 112: tiling window managers</h2>
<p>This week: layouts, gaps and a short history of the tiling window manager, from the first experiments to today's scrollable workspaces.</p>
<h3>In this issue</h3>
<ul>
<li><a href="https://news.example.net/112/layouts">Master and stack, explained</a></li>
<li><a href="https://news.example.net/112/gaps">Why gaps are not just decoration</a></li>
<li><a href="https://news.example.net/112/history">A short history, 1988 to now</a></li>
</ul>
<p>Reader question of the week: <em>how do I keep a scratchpad terminal one key away?</em> Three answers inside.</p>
<hr>
<p style="font-size:small">You get this because you subscribed at news.example.net.
<a href="https://news.example.net/unsubscribe/7f3a">Unsubscribe</a> or
<a href="https://news.example.net/preferences/7f3a">change your preferences</a>.</p>
</body></html>"#;

fn newsletter_html(issue: u32, topic: &str, blurb: &str) -> String {
    format!(
        "<!doctype html><html><body style=\"font-family:Georgia,serif\">\
<h1>Tessellate Weekly</h1><h2>Issue {issue}: {topic}</h2><p>{blurb}</p>\
<p><a href=\"https://news.example.net/{issue}\">Read the full issue</a></p><hr>\
<p style=\"font-size:small\"><a href=\"https://news.example.net/unsubscribe/7f3a\">Unsubscribe</a></p>\
</body></html>"
    )
}

fn inbox_today(d: &mut Demo<'_>, pp: &People) {
    let garden_list = p("Northgate Garden Club", "garden-club@example.org");

    // Garden club plot rota: three messages, labelled Garden club.
    let rota_text = "Hi all,\n\nHere is the rota for November, plots 1 to 12 are listed below. Each week has one \
person on watering and one on the compost bays.\n\n  Nov 7   Mira (watering), Owen (compost)\n  Nov 14  Noor \
(watering), Juniper (compost)\n  Nov 21  Juniper (watering), Mira (compost)\n  Nov 28  Owen (watering), Noor \
(compost)\n\nShout by Wednesday if a date does not work, and swap among yourselves if you can.\n\nJuniper\n\n-- \n\
Juniper Tran\nSecretary, Northgate Garden Club\nhttps://garden.example.com";
    let rota = d
        .msg("2026-10-19T10:14:00Z", &pp.juniper, "Garden club plot rota")
        .to(&[&garden_list])
        .mailboxes(&["inbox", "garden"])
        .header("List-Id", "Northgate Garden Club <garden-club.example.org>")
        .seen()
        .answered()
        .text(rota_text)
        .send();
    let mira_reply = "Thanks! I cannot do the 7th, could someone swap with me? I am away that weekend.\n\nHappy \
to take the 14th or the 28th instead.\n\nMira\n\n";
    let reply = d
        .msg("2026-10-19T18:40:00Z", &pp.me, "Re: Garden club plot rota")
        .to(&[&garden_list])
        .mailboxes(&["sent"])
        .seen()
        .reply_to(&rota)
        .text(&format!(
            "{mira_reply}{MIRA_SIG}\n\n{}",
            quote(
                "On Mon, 19 Oct 2026 at 10:14, Juniper Tran <juniper@garden.example.com> wrote:",
                rota_text
            )
        ))
        .send();
    let quoted = quote(
        "On Mon, 19 Oct 2026 at 18:40, Mira Holt <mira@example.org> wrote:",
        "Thanks! I cannot do the 7th, could someone swap with me? I am away that weekend.\n\nHappy to take the 14th \
or the 28th instead.\n\nMira\n\n-- \nMira Holt\nPlot 7, Northgate Garden Club\n\nOn Mon, 19 Oct 2026 at 10:14, \
Juniper Tran wrote:\n> Here is the rota for November, plots 1 to 12 are listed below.",
    );
    d.msg("2026-10-23T14:02:00Z", &pp.juniper, "Re: Garden club plot rota")
        .to(&[&pp.me])
        .cc(&[&garden_list])
        .mailboxes(&["inbox", "garden"])
        .header("List-Id", "Northgate Garden Club <garden-club.example.org>")
        .reply_to(&reply)
        .text(&format!(
            "Swapping Saturday works for me, can you take the 14th instead? Then Owen covers the 21st and we are \
set.\n\nThe compost delivery is on the 9th, so whoever is on that week please be there by 8. I will leave the shed \
key with Noor.\n\nI have updated the shared rota: https://garden.example.com/rota\n\n{quoted}\nJuniper\n\n-- \n\
Juniper Tran\nSecretary, Northgate Garden Club\nhttps://garden.example.com"
        ))
        .send();

    d.msg("2026-10-23T13:47:00Z", &pp.owen, "Draft budget for the hall repairs")
        .to(&[&pp.me])
        .cc(&[&pp.grace])
        .mailboxes(&["inbox", "hall"])
        .flagged()
        .attach("hall-repairs-budget.xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", 38_912)
        .text(
            "Hi Mira,\n\nAttached the spreadsheet. The roof line is the big one: the two quotes came in at 4,200 \
and 5,050, and I have used the lower one for now. Gutters and the side door are small by comparison.\n\nCan we go \
through it before the committee meets on the 2nd? Grace has seen an earlier version.\n\nOwen\n\n-- \nOwen \
Castillo\nTreasurer, Northgate Community Hall",
        )
        .send();

    // Larkspur: confirmation, Mira's reply, then the HTML shipping notice to
    // the masked address.
    let larkspur = p("Larkspur Outfitters", "orders@shop.example.com");
    let support = p("Larkspur Support", "help@shop.example.com");
    let kettle = mask("masked-48210");
    let confirm = d
        .msg("2026-10-20T16:05:00Z", &larkspur, "Order 48213 confirmed")
        .to(&[&kettle])
        .mailboxes(&["receipts"])
        .seen()
        .header("List-Unsubscribe", "<https://shop.example.com/unsubscribe/kf4821>")
        .attach("order-48213.pdf", "application/pdf", 62_464)
        .text(
            "Thanks for your order. Order 48213 is confirmed and will ship soon.\n\nYou will get another message \
with tracking details.\n\n  Merino hiking socks, 2-pack   1   18.00\n  Trail bottle, 750 ml          2   24.00\n\
  Delivery                          0.00\n  Total                            42.00\n\nLarkspur Outfitters\n\
https://shop.example.com",
        )
        .html(
            "<html><body><h1>Thanks for your order</h1><p>Order 48213 is confirmed and will ship soon.</p>\
<p>You will get another message with tracking details.</p><p><a href=\"https://shop.example.com/orders/48213\">\
View your order</a></p></body></html>",
        )
        .send();
    let ask = d
        .msg("2026-10-21T08:31:00Z", &pp.me, "Re: Order 48213 confirmed")
        .to(&[&support])
        .mailboxes(&["sent"])
        .seen()
        .reply_to(&confirm)
        .text(&format!(
            "Hi,\n\ncould you ship to my work address instead? It is 4 Quay Road, second floor, and the front desk \
takes parcels between 8 and 6.\n\nThanks,\nMira\n\n{}",
            quote(
                "On Tue, 20 Oct 2026 at 16:05, Larkspur Outfitters <orders@shop.example.com> wrote:",
                "Thanks for your order. Order 48213 is confirmed and will ship soon.",
            )
        ))
        .send();
    d.msg(
        "2026-10-23T09:12:00Z",
        &support,
        "Re: Your order 48213 has shipped",
    )
    .to(&[&kettle])
    .mailboxes(&["inbox", "receipts"])
    .reply_to(&ask)
    .header(
        "List-Unsubscribe",
        "<https://shop.example.com/unsubscribe/kf4821>, <mailto:unsubscribe@shop.example.com>",
    )
    .attach("invoice-48213.pdf", "application/pdf", 86_016)
    .html(LARKSPUR_HTML)
    .send();

    let standup = d
        .msg("2026-10-22T16:30:00Z", &pp.work, "Thursday standup notes")
        .to(&[&pp.lena])
        .mailboxes(&["sent"])
        .seen()
        .text(
            "Hi Lena,\n\nNotes from today:\n\n- Release candidate is green apart from the export bug.\n- Ines \
needs two more days on the translations.\n- Proposal: move the release from the 27th to the 30th.\n\nMira\n\n\
Mira Holt\nProject lead, Quay Road office",
        )
        .send();
    d.msg("2026-10-23T08:55:00Z", &pp.lena, "Re: Thursday standup notes")
        .to(&[&pp.work])
        .mailboxes(&["inbox"])
        .seen()
        .reply_to(&standup)
        .text(&format!(
            "Agreed on moving the release to the 30th. I will tell the support team today.\n\nLena\n\n{}",
            quote(
                "On Thu, 22 Oct 2026 at 16:30, Mira at work <mira.holt@work.example.com> wrote:",
                "- Proposal: move the release from the 27th to the 30th.",
            )
        ))
        .send();

    d.msg("2026-10-23T07:30:00Z", &p("Northgate Library", "noreply@library.example.org"), "Your reservation is ready to collect")
        .to(&[&mask("masked-22071")])
        .mailboxes(&["inbox"])
        .seen()
        .text(
            "The book you reserved is waiting at the front desk.\n\n  Title: A Field Guide to Lichens\n  \
Collect by: 30 October\n\nOpening hours: Monday to Saturday, 9 to 7.\n\nNorthgate Library\n\
https://library.example.org/account",
        )
        .send();

    // The coast trip: five messages, labelled Travel.
    let trip = d
        .msg("2026-10-18T19:05:00Z", &pp.priya, "Trip to the coast in November")
        .to(&[&pp.me, &pp.dana, &pp.sam])
        .mailboxes(&["inbox", "travel"])
        .seen()
        .text(
            "Hi all,\n\nHow about a long weekend at the coast, 14 to 16 November? I found two cabins that sleep \
four, both near the cliff path. I can book whichever we pick.\n\nPriya",
        )
        .send();
    let sam = d
        .msg(
            "2026-10-19T08:12:00Z",
            &pp.sam,
            "Re: Trip to the coast in November",
        )
        .to(&[&pp.priya])
        .cc(&[&pp.me, &pp.dana])
        .mailboxes(&["inbox", "travel"])
        .seen()
        .reply_to(&trip)
        .text("In! The second cabin has a wood stove, so that one gets my vote.\n\nSam")
        .send();
    let mira = d
        .msg(
            "2026-10-19T12:40:00Z",
            &pp.me,
            "Re: Trip to the coast in November",
        )
        .to(&[&pp.priya])
        .cc(&[&pp.dana, &pp.sam])
        .mailboxes(&["sent", "travel"])
        .seen()
        .reply_to(&sam)
        .text("Count me in too. I can drive, the car takes four with bags.\n\nMira")
        .send();
    let dana = d
        .msg(
            "2026-10-20T21:03:00Z",
            &pp.dana,
            "Re: Trip to the coast in November",
        )
        .to(&[&pp.priya])
        .cc(&[&pp.me, &pp.sam])
        .mailboxes(&["inbox", "travel"])
        .seen()
        .reply_to(&mira)
        .text("Yes please. Wood stove cabin it is. I will bring the plum cake.\n\nDana")
        .send();
    d.msg("2026-10-22T21:15:00Z", &pp.priya, "Re: Trip to the coast in November")
        .to(&[&pp.me, &pp.dana, &pp.sam])
        .mailboxes(&["inbox", "travel"])
        .seen()
        .reply_to(&dana)
        .text(&format!(
            "Booked the cabin, link below. Can everyone confirm by Sunday?\n\nhttps://cabins.example.net/booking/7731\n\n\
Check-in is from 4. I asked about late arrival and a dog, no answer yet.\n\nPriya\n\n{}",
            quote(
                "On Tue, 20 Oct 2026 at 21:03, Dana Whitfield <dana.whitfield@example.net> wrote:",
                "Yes please. Wood stove cabin it is. I will bring the plum cake.",
            )
        ))
        .send();

    d.msg("2026-10-22T06:00:00Z", &p("Tessellate Weekly", "issue@news.example.net"), "Issue 112: tiling window managers")
        .to(&[&mask("masked-11891")])
        .mailboxes(&["inbox"])
        .seen()
        .header("List-Id", "Tessellate Weekly <weekly.news.example.net>")
        .header("List-Unsubscribe", "<https://news.example.net/unsubscribe/7f3a>, <mailto:leave@news.example.net>")
        .text(
            "Tessellate Weekly, issue 112: tiling window managers\n\nThis week: layouts, gaps and a short history \
of the tiling window manager.\n\n- Master and stack, explained: https://news.example.net/112/layouts\n- Why gaps \
are not just decoration: https://news.example.net/112/gaps\n- A short history, 1988 to now: \
https://news.example.net/112/history\n\nUnsubscribe: https://news.example.net/unsubscribe/7f3a",
        )
        .html(TESSELLATE_HTML)
        .send();

    d.msg("2026-10-22T15:20:00Z", &pp.marco, "Invoice 2026-118")
        .to(&[&pp.me])
        .mailboxes(&["inbox"])
        .seen()
        .flagged()
        .attach("invoice-2026-118.pdf", "application/pdf", 122_880)
        .text(
            "Hello Mira,\n\nPlease find the invoice for October attached: garden design consultation, two visits. \
Payment within 30 days to the account on the invoice.\n\nThank you,\nMarco Bellini\nBellini Landscapes",
        )
        .send();
}

fn inbox_this_week(d: &mut Demo<'_>, pp: &People) {
    d.msg("2026-10-21T11:00:00Z", &p("City Notices", "notices@city.example.org"), "Road closure on Elm Street, Oct 28")
        .to(&[&pp.me])
        .mailboxes(&["inbox"])
        .seen()
        .text(
            "Works start at 07:00 and run until late afternoon on Wednesday 28 October. Elm Street will be \
closed between the library and the bridge. Bus 12 will use Mill Lane.\n\nCity Notices\n\
https://city.example.org/roadworks",
        )
        .send();

    let cake = d
        .msg("2026-10-20T19:30:00Z", &pp.me, "Recipe for the plum cake")
        .to(&[&pp.dana])
        .mailboxes(&["sent"])
        .seen()
        .text("Dana, could you send me the plum cake recipe? The one from the harvest supper.\n\nMira")
        .send();
    d.msg("2026-10-21T09:45:00Z", &pp.dana, "Re: Recipe for the plum cake")
        .to(&[&pp.me])
        .mailboxes(&["inbox"])
        .seen()
        .reply_to(&cake)
        .text(&format!(
            "Use less sugar than it says, the plums are sweet this year.\n\n  250 g flour\n  150 g butter\n  \
120 g sugar (recipe says 180)\n  3 eggs\n  600 g plums, halved\n\n45 minutes at 180, then cool in the tin.\n\nDana\n\n{}",
            quote(
                "On Tue, 20 Oct 2026 at 19:30, Mira Holt <mira@example.org> wrote:",
                "Dana, could you send me the plum cake recipe? The one from the harvest supper.",
            )
        ))
        .send();

    d.msg("2026-10-20T07:15:00Z", &p("Kestrel Bank", "statements@bank.example.com"), "Your October statement is ready")
        .to(&[&mask("masked-55130")])
        .mailboxes(&["inbox"])
        .seen()
        .text(
            "Your October statement is ready.\n\nSign in to view your statement. We will never ask for your \
password or a code by email.\n\nKestrel Bank\nhttps://bank.example.com",
        )
        .html(
            "<html><body><h2>Your October statement is ready</h2><p>Sign in to view your statement. We will never \
ask for your password or a code by email.</p><p><a href=\"https://bank.example.com/statements\">View statement</a>\
</p><img src=\"https://bank.example.com/t/open.gif\" width=\"1\" height=\"1\" alt=\"\"></body></html>",
        )
        .send();

    d.msg("2026-10-20T20:10:00Z", &pp.sam, "Photos from the climbing trip")
        .to(&[&pp.me, &pp.priya])
        .mailboxes(&["inbox"])
        .seen()
        .attach("crag-01.jpg", "image/jpeg", 2_201_600)
        .attach("crag-02.jpg", "image/jpeg", 1_904_640)
        .attach("summit.jpg", "image/jpeg", 2_457_600)
        .text("Here are the good ones, the rest are mostly blurry. The summit one is my favourite.\n\nSam")
        .send();

    let games = d
        .msg("2026-10-15T17:20:00Z", &pp.ravi, "Board game night")
        .to(&[&pp.me, &pp.theo, &pp.noor])
        .mailboxes(&["inbox"])
        .seen()
        .text("Board games at mine next Friday, from 7? I will make chilli.\n\nRavi")
        .send();
    let theo = d
        .msg("2026-10-16T09:02:00Z", &pp.theo, "Re: Board game night")
        .to(&[&pp.ravi])
        .cc(&[&pp.me, &pp.noor])
        .mailboxes(&["inbox"])
        .seen()
        .reply_to(&games)
        .text("Yes! Can we finally finish the railway campaign?\n\nTheo")
        .send();
    let mira = d
        .msg("2026-10-17T10:30:00Z", &pp.me, "Re: Board game night")
        .to(&[&pp.ravi])
        .cc(&[&pp.theo, &pp.noor])
        .mailboxes(&["sent"])
        .seen()
        .reply_to(&theo)
        .text("I will be there, a bit after 7. Shall I bring bread?\n\nMira")
        .send();
    d.msg("2026-10-19T12:05:00Z", &pp.ravi, "Re: Board game night")
        .to(&[&pp.me, &pp.theo, &pp.noor])
        .mailboxes(&["inbox"])
        .seen()
        .reply_to(&mira)
        .text("I can bring the new expansion if people want. Bread would be great, thanks Mira.\n\nRavi")
        .send();

    d.msg("2026-10-19T06:30:00Z", &p("Example Hosting", "billing@hosting.example.com"), "Renewal notice for example.org")
        .to(&[&pp.me])
        .mailboxes(&["inbox"])
        .seen()
        .text(
            "Your domain renews automatically on 12 November for 14.00 a year. Nothing to do if your card on file \
is current.\n\nManage your domains: https://hosting.example.com/domains\n\nExample Hosting",
        )
        .send();
}

fn inbox_earlier(d: &mut Demo<'_>, pp: &People) {
    d.msg(
        "2026-10-17T15:40:00Z",
        &pp.grace,
        "Quarterly newsletter draft",
    )
    .to(&[&pp.me, &pp.owen])
    .mailboxes(&["inbox"])
    .seen()
    .attach(
        "newsletter-q4-draft.docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        48_128,
    )
    .text("Comments welcome before Friday, especially on the hall repairs section.\n\nGrace")
    .send();

    d.msg("2026-10-16T10:00:00Z", &p("Harbor Dental", "reminders@dental.example.com"), "Appointment reminder: 3 November")
        .to(&[&mask("masked-33901")])
        .mailboxes(&["inbox"])
        .seen()
        .text("Reply C to confirm or call us to rearrange.\n\n  Tuesday 3 November, 09:40\n  Dr. Alder, room 2\n\nHarbor Dental")
        .send();

    let club = d
        .msg("2026-10-09T18:00:00Z", &pp.felix, "Moving the book club")
        .to(&[&pp.me, &pp.ines, &pp.noor, &pp.bea])
        .mailboxes(&["inbox"])
        .seen()
        .text("The café is closing for refurbishment. Shall we move to the library meeting room, and maybe to a different evening?\n\nFelix")
        .send();
    let ines = d
        .msg("2026-10-10T08:20:00Z", &pp.ines, "Re: Moving the book club")
        .to(&[&pp.felix])
        .cc(&[&pp.me, &pp.noor, &pp.bea])
        .mailboxes(&["inbox"])
        .seen()
        .reply_to(&club)
        .text("Library works for me. Thursdays are hard though.\n\nInes")
        .send();
    let mira = d
        .msg("2026-10-10T12:10:00Z", &pp.me, "Re: Moving the book club")
        .to(&[&pp.felix])
        .cc(&[&pp.ines, &pp.noor, &pp.bea])
        .mailboxes(&["sent"])
        .seen()
        .reply_to(&ines)
        .text("The library room is free on second Tuesdays, I checked.\n\nMira")
        .send();
    let noor = d
        .msg("2026-10-12T19:45:00Z", &pp.noor, "Re: Moving the book club")
        .to(&[&pp.felix])
        .cc(&[&pp.me, &pp.ines, &pp.bea])
        .mailboxes(&["inbox"])
        .seen()
        .reply_to(&mira)
        .text("Second Tuesday is fine by me.\n\nNoor")
        .send();
    let bea = d
        .msg("2026-10-13T07:55:00Z", &pp.bea, "Re: Moving the book club")
        .to(&[&pp.felix])
        .cc(&[&pp.me, &pp.ines, &pp.noor])
        .mailboxes(&["inbox"])
        .seen()
        .reply_to(&noor)
        .text("Works for me too, as long as we still get cake.\n\nBea")
        .send();
    d.msg("2026-10-15T21:30:00Z", &pp.felix, "Re: Moving the book club")
        .to(&[&pp.me, &pp.ines, &pp.noor, &pp.bea])
        .mailboxes(&["inbox"])
        .seen()
        .reply_to(&bea)
        .text("Second Tuesday works better for most of us, so that is settled. First one is 10 November in the library.\n\nFelix")
        .send();

    d.msg("2026-10-14T08:00:00Z", &p("Wren Analytics", "reports@wren.example.com"), "Weekly report: 3 new signups")
        .to(&[&mask("masked-33171")])
        .mailboxes(&["inbox"])
        .seen()
        .html(
            "<html><body><h2>Weekly report</h2><p>Traffic is up 4 percent on last week.</p><table>\
<tr><th>Metric</th><th>This week</th><th>Last week</th></tr><tr><td>Visits</td><td>1,204</td><td>1,158</td></tr>\
<tr><td>Signups</td><td>3</td><td>1</td></tr></table><p><a href=\"https://wren.example.com/dashboard\">Open the \
dashboard</a></p></body></html>",
        )
        .send();

    d.msg("2026-10-12T16:25:00Z", &pp.juniper, "Seed order list")
        .to(&[&pp.me, &pp.owen, &pp.noor])
        .mailboxes(&["inbox", "garden"])
        .seen()
        .attach("seed-order-2027.xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", 21_504)
        .text("Final list attached. Shout if I missed anything before I send it on Friday.\n\nJuniper")
        .send();

    let room = d
        .msg("2026-10-09T20:00:00Z", &pp.me, "Spare room this weekend?")
        .to(&[&pp.noor])
        .mailboxes(&["sent"])
        .seen()
        .text("Hi Noor, my cousin is in town Friday to Sunday. Is your spare room free by any chance?\n\nMira")
        .send();
    d.msg(
        "2026-10-10T07:40:00Z",
        &pp.noor,
        "Re: Spare room this weekend?",
    )
    .to(&[&pp.me])
    .mailboxes(&["inbox"])
    .seen()
    .reply_to(&room)
    .text("Yes, the spare room is free, come Friday. Key is under the blue pot as usual.\n\nNoor")
    .send();

    d.msg("2026-10-08T12:00:00Z", &p("Larkspur Outfitters", "orders@shop.example.com"), "Receipt for order 47720")
        .to(&[&mask("masked-48210")])
        .mailboxes(&["inbox", "receipts"])
        .seen()
        .attach("receipt-47720.pdf", "application/pdf", 59_392)
        .text("Thanks for shopping with us. Your receipt is attached.\n\n  Order 47720\n  Rain shell, size M   1   89.00\n\nLarkspur Outfitters")
        .send();

    d.msg("2026-10-06T18:30:00Z", &pp.theo, "Slides from the meetup")
        .to(&[&pp.me, &pp.ravi])
        .mailboxes(&["inbox"])
        .seen()
        .text(
            "Slides and the recording link are below.\n\nSlides: https://talks.example.net/theo/terminal-mail\n\
Recording: https://video.example.net/meetup/41\n\nThanks for the questions!\nTheo",
        )
        .send();

    d.msg("2026-10-03T09:00:00Z", &p("Maple Street Co-op", "members@coop.example.org"), "Membership renewal")
        .to(&[&mask("masked-44021")])
        .mailboxes(&["inbox"])
        .seen()
        .text("Your membership is due for renewal this month. Renew at the till or online.\n\nhttps://coop.example.org/renew\n\nMaple Street Co-op")
        .send();

    let review = d
        .msg("2026-09-28T10:15:00Z", &pp.work, "Translation review")
        .to(&[&pp.ines])
        .mailboxes(&["sent"])
        .seen()
        .attach("release-notes-pt.docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document", 30_720)
        .text("Hi Ines, could you review the Portuguese release notes when you have a moment?\n\nMira")
        .send();
    let first = d
        .msg("2026-09-30T14:00:00Z", &pp.ines, "Re: Translation review")
        .to(&[&pp.work])
        .mailboxes(&["inbox"])
        .seen()
        .reply_to(&review)
        .text("On it, I will send comments tomorrow.\n\nInes")
        .send();
    d.msg("2026-10-01T16:50:00Z", &pp.ines, "Re: Translation review")
        .to(&[&pp.work])
        .mailboxes(&["inbox"])
        .seen()
        .reply_to(&first)
        .text(
            "Two small fixes in section 4, otherwise good. I marked them in the document.\n\nInes",
        )
        .send();

    d.msg("2026-09-29T11:30:00Z", &pp.owen, "Hall booking calendar")
        .to(&[&pp.me])
        .mailboxes(&["inbox", "hall"])
        .seen()
        .text("Shared the calendar for the winter term: https://calendar.example.net/hall/winter\n\nOwen")
        .send();

    let route = d
        .msg("2026-09-26T07:10:00Z", &pp.me, "Running club route")
        .to(&[&pp.bea])
        .mailboxes(&["sent"])
        .seen()
        .text("Which route on Sunday, river or hill?\n\nMira")
        .send();
    d.msg("2026-09-27T06:45:00Z", &pp.bea, "Re: Running club route")
        .to(&[&pp.me])
        .mailboxes(&["inbox"])
        .seen()
        .reply_to(&route)
        .text("The river loop is flooded, take the hill route. Meet at the gate at 8.\n\nBea")
        .send();

    d.msg("2026-09-25T13:00:00Z", &p("Calder Insurance", "documents@insure.example.com"), "Policy documents")
        .to(&[&mask("masked-66411")])
        .mailboxes(&["inbox"])
        .seen()
        .attach("home-policy-2026.pdf", "application/pdf", 348_160)
        .text("Your renewed policy documents are attached. Please keep them somewhere safe.\n\nCalder Insurance")
        .send();

    d.msg("2026-09-22T09:30:00Z", &pp.lena, "Offsite agenda")
        .to(&[&pp.work])
        .mailboxes(&["inbox"])
        .seen()
        .text("Draft agenda for the offsite, comments welcome.\n\n  09:30  Roadmap\n  11:00  Support review\n  13:30  Walk\n\nLena")
        .send();

    d.msg("2026-09-20T17:00:00Z", &pp.pia, "Choir rehearsal moved")
        .to(&[&pp.me])
        .mailboxes(&["inbox"])
        .seen()
        .text("We are in the small hall this week, same time.\n\nPia")
        .send();

    d.msg("2026-09-18T12:20:00Z", &pp.hugo, "Old flat deposit")
        .to(&[&pp.me])
        .mailboxes(&["inbox"])
        .seen()
        .text("The deposit came back in full, finally. Your half is on its way.\n\nHugo")
        .send();
}

fn folders(d: &mut Demo<'_>, pp: &People) {
    let tess = p("Tessellate Weekly", "issue@news.example.net");
    let sunny = mask("masked-11891");
    for (at, issue, topic, blurb, seen) in [
        (
            "2026-09-24T06:00:00Z",
            108u32,
            "status bars",
            "What belongs in a status bar, and what does not.",
            true,
        ),
        (
            "2026-10-01T06:00:00Z",
            109,
            "keyboard layers",
            "Layers, chords and the keys you never use.",
            false,
        ),
        (
            "2026-10-08T06:00:00Z",
            110,
            "terminal colours",
            "Sixteen colours, used well.",
            false,
        ),
        (
            "2026-10-15T06:00:00Z",
            111,
            "dotfiles",
            "Keeping your setup in one place without losing a weekend.",
            false,
        ),
    ] {
        let subject = format!("Issue {issue}: {topic}");
        let mut m = d
            .msg(at, &tess, &subject)
            .to(&[&sunny])
            .mailboxes(&["newsletters"])
            .header("List-Id", "Tessellate Weekly <weekly.news.example.net>")
            .header(
                "List-Unsubscribe",
                "<https://news.example.net/unsubscribe/7f3a>",
            )
            .html(&newsletter_html(issue, topic, blurb));
        if seen {
            m = m.seen();
        }
        m.send();
    }
    let almanac = p("The Allotment Almanac", "letters@almanac.example.org");
    for (at, subject, seen) in [
        (
            "2026-10-05T07:00:00Z",
            "October: garlic, broad beans and the first frost",
            false,
        ),
        (
            "2026-09-07T07:00:00Z",
            "September: saving seed and lifting potatoes",
            true,
        ),
    ] {
        let mut m = d
            .msg(at, &almanac, subject)
            .to(&[&pp.me])
            .mailboxes(&["newsletters"])
            .header("List-Id", "The Allotment Almanac <letters.almanac.example.org>")
            .html(&format!(
                "<html><body><h1>The Allotment Almanac</h1><h2>{subject}</h2><p>Jobs for the month, a recipe and \
a reader's plot.</p><p><a href=\"https://almanac.example.org/latest\">Read online</a></p></body></html>"
            ));
        if seen {
            m = m.seen();
        }
        m.send();
    }
    d.msg("2026-10-20T10:00:00Z", &p("Northgate Library", "events@library.example.org"), "Events this autumn")
        .to(&[&mask("masked-22071")])
        .mailboxes(&["newsletters"])
        .html("<html><body><h2>Events this autumn</h2><ul><li>Lichen walk, 7 November</li><li>Repair café, 21 November</li></ul></body></html>")
        .send();

    // Receipts, matching the rules dry run in the mock-ups.
    let larkspur = p("Larkspur Outfitters", "orders@shop.example.com");
    let kettle = mask("masked-48210");
    d.msg("2026-09-14T10:00:00Z", &larkspur, "Returns label for order 47188")
        .to(&[&kettle])
        .mailboxes(&["receipts"])
        .seen()
        .attach("label-47188.pdf", "application/pdf", 104_448)
        .text("Your returns label is attached. Drop the parcel at any post office.\n\nLarkspur Outfitters")
        .send();
    d.msg("2026-08-30T15:00:00Z", &larkspur, "Receipt for order 47188")
        .to(&[&kettle])
        .mailboxes(&["receipts"])
        .seen()
        .attach("receipt-47188.pdf", "application/pdf", 58_368)
        .text("Thanks for shopping with us. Your receipt is attached.\n\nLarkspur Outfitters")
        .send();
    d.msg(
        "2026-10-03T18:00:00Z",
        &p("Maple Street Co-op", "members@coop.example.org"),
        "Your receipt for October",
    )
    .to(&[&mask("masked-44021")])
    .mailboxes(&["receipts"])
    .seen()
    .text("Membership, October: 6.00. Thank you for being a member.\n\nMaple Street Co-op")
    .send();
    d.msg(
        "2026-09-29T09:00:00Z",
        &p("Example Hosting", "billing@hosting.example.com"),
        "Receipt: example.org renewal",
    )
    .to(&[&pp.me])
    .mailboxes(&["receipts"])
    .seen()
    .text("Receipt for the renewal of example.org, one year: 14.00.\n\nExample Hosting")
    .send();
    d.msg(
        "2026-08-26T12:00:00Z",
        &p("Northgate Library", "noreply@library.example.org"),
        "Receipt for your late fee",
    )
    .to(&[&mask("masked-22071")])
    .mailboxes(&["receipts"])
    .seen()
    .text("Late fee paid: 1.20. Thank you.\n\nNorthgate Library")
    .send();

    // Spam, including a lookalike bank sender on the bank's mask.
    d.msg("2026-10-22T03:12:00Z", &p("Prize Desk", "prizes@win.example.net"), "You have won a cruise!!!")
        .to(&[&pp.me])
        .mailboxes(&["spam"])
        .header("X-Spam-Score", "9.4")
        .html("<html><body><h1>CONGRATULATIONS</h1><p><a href=\"https://win.example.net/claim\">Claim now</a></p></body></html>")
        .send();
    d.msg("2026-10-21T23:40:00Z", &p("Kestrel Bank Security", "alerts@pay.example.org"), "Action needed: verify your account")
        .to(&[&mask("masked-55130")])
        .mailboxes(&["spam"])
        .header("X-Spam-Score", "7.8")
        .text("We noticed unusual activity. Verify your account within 24 hours: https://pay.example.org/verify")
        .send();
    d.msg(
        "2026-10-20T02:05:00Z",
        &p("Watch Deals", "deals@promo.example.com"),
        "Luxury watches at 90% off",
    )
    .to(&[&pp.me])
    .mailboxes(&["spam"])
    .header("X-Spam-Score", "11.2")
    .text("Today only. https://promo.example.com/watches")
    .send();

    // Mail to the disabled mask lands in Trash by itself.
    let heron = mask("masked-77310");
    for (at, subject) in [
        ("2026-10-21T08:00:00Z", "Final chance: claim your prize"),
        ("2026-10-14T08:00:00Z", "You are a finalist"),
    ] {
        d.msg(at, &p("Promo Contest", "contest@win.example.net"), subject)
            .to(&[&heron])
            .mailboxes(&["inbox"])
            .text("Click to claim: https://win.example.net/claim")
            .send();
    }
    d.msg(
        "2026-09-10T09:00:00Z",
        &p("Survey Site", "panel@survey.example.net"),
        "Tell us what you think",
    )
    .to(&[&mask("masked-27641")])
    .mailboxes(&["inbox"])
    .seen()
    .text("Five minutes of your time: https://survey.example.net/s/41")
    .send();

    d.msg("2026-10-23T11:30:00Z", &pp.me, "Booking for 14 to 16 November")
        .to(&[&p("Lakeside Cabins", "bookings@cabins.example.net")])
        .mailboxes(&["drafts"])
        .draft()
        .attach("floorplan-2025.pdf", "application/pdf", 217_088)
        .text(
            "Hi,\n\nWe would like to book the lakeside cabin for two nights, Friday 14 to\nSunday 16 November, for \
two adults.\n\nA few questions:\n\n- Is late check-in after 21:00 possible?\n- Can we bring a small dog?\n\nThe \
floor plan you sent last year is attached. We would like the same\nlayout if it is free.\n\nThanks,\nMira",
        )
        .send();

    d.msg("2026-09-30T18:00:00Z", &pp.me, "Hall key handover")
        .to(&[&pp.owen])
        .mailboxes(&["sent"])
        .seen()
        .text("Owen, I left the hall key with the café. Ask for Ana.\n\nMira")
        .send();
    d.msg("2026-10-02T10:00:00Z", &pp.owen, "Roofer quote")
        .to(&[&pp.me])
        .mailboxes(&["hall"])
        .seen()
        .attach("roof-quote.pdf", "application/pdf", 143_360)
        .text("Second quote attached. Cheaper, but they cannot start until January.\n\nOwen")
        .send();

    d.msg("2026-10-05T08:30:00Z", &pp.juniper, "Compost delivery on the 9th")
        .to(&[&p("Northgate Garden Club", "garden-club@example.org")])
        .mailboxes(&["garden"])
        .seen()
        .header("List-Id", "Northgate Garden Club <garden-club.example.org>")
        .text("Six bags of compost arrive on Monday the 9th. Please help carry them to the bays if you are around.\n\nJuniper")
        .send();
    d.msg("2026-10-22T17:45:00Z", &pp.noor, "Plot 7 water tap")
        .to(&[&pp.me])
        .cc(&[&pp.juniper])
        .mailboxes(&["garden"])
        .text("Mira, the tap by plot 7 is dripping again. I tightened it but it needs a new washer.\n\nNoor")
        .send();

    // Archive.
    let archive: &[(&str, &Address, &str, &str)] = &[
        (
            "2026-09-15T10:00:00Z",
            &pp.grace,
            "Summer fete thank you",
            "Thank you all, we raised 1,320 for the hall roof.",
        ),
        (
            "2026-09-12T16:00:00Z",
            &pp.juniper,
            "Allotment water butts",
            "Two new water butts are in, by plots 3 and 9.",
        ),
        (
            "2026-09-08T09:00:00Z",
            &pp.ravi,
            "Bike for sale",
            "Selling my old touring bike if you know anyone.",
        ),
        (
            "2026-09-03T12:30:00Z",
            &pp.lena,
            "Q3 retro notes",
            "Notes from the retro are in the shared folder.",
        ),
        (
            "2026-08-29T08:00:00Z",
            &pp.priya,
            "Photos from the lake",
            "A few from the lake day, more to come.",
        ),
        (
            "2026-08-27T19:00:00Z",
            &pp.felix,
            "September book",
            "We are reading The Salt Path for September.",
        ),
    ];
    for (at, from, subject, body) in archive {
        d.msg(at, from, subject)
            .to(&[&pp.me])
            .mailboxes(&["archive"])
            .seen()
            .text(&format!("{body}\n\n{}", from.name.as_deref().unwrap_or("")))
            .send();
    }
    d.msg("2026-08-25T14:00:00Z", &p("Rail Tickets", "tickets@rail.example.net"), "Your tickets for 14 November")
        .to(&[&mask("masked-99171")])
        .mailboxes(&["archive", "travel"])
        .seen()
        .attach("tickets-14-nov.pdf", "application/pdf", 77_824)
        .text("Booking reference K7Q2M. Outbound 14 November 15:10, return 16 November 17:40.\n\nRail Tickets")
        .send();
    d.msg("2026-09-05T11:00:00Z", &pp.hugo, "Old newsletter")
        .to(&[&pp.me])
        .mailboxes(&["trash"])
        .seen()
        .text("Forwarding this in case you want it. Probably not.\n\nHugo")
        .send();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FakeConfig;

    fn demo_store() -> Store {
        let now: Timestamp = "2026-10-23T14:27:00Z".parse().unwrap();
        let cfg = FakeConfig {
            username: USERNAME.to_owned(),
            display_name: DISPLAY_NAME.to_owned(),
            now: Some(now),
            ..FakeConfig::default()
        };
        let mut store = Store::new(&cfg, now);
        fill(&mut store, now);
        store
    }

    #[test]
    fn demo_has_expected_shape() {
        let s = demo_store();
        assert!(
            (60..=120).contains(&s.emails.len()),
            "{} emails",
            s.emails.len()
        );
        assert_eq!(s.masked.len(), 20);
        assert_eq!(s.identities.len(), 3);
        assert!(s.contacts.len() >= 12);
        for path in [
            "Newsletters",
            "Receipts",
            "Projects/Garden club",
            "Projects/Hall repairs",
            "Travel",
        ] {
            assert!(s.find_mailbox(path).is_some(), "{path}");
        }
        let states: std::collections::BTreeSet<_> = s.masked.values().map(|m| m.state).collect();
        assert_eq!(states.len(), 4, "every mask state is present");
    }

    #[test]
    fn demo_threads_and_masks() {
        let s = demo_store();
        let rota: Vec<_> = s
            .emails
            .values()
            .filter(|e| {
                e.subject
                    .as_deref()
                    .is_some_and(|x| x.contains("Garden club plot rota"))
            })
            .collect();
        assert_eq!(rota.len(), 3);
        assert!(rota.iter().all(|e| e.thread_id == rota[0].thread_id));
        let shipped = s
            .emails
            .values()
            .find(|e| e.subject.as_deref() == Some("Re: Your order 48213 has shipped"))
            .unwrap();
        assert!(shipped.html.as_deref().unwrap().contains("<table"));
        assert!(
            shipped
                .extra_headers
                .iter()
                .any(|(n, v)| n == crate::store::MASKED_EMAIL_HEADER
                    && v == "kettle.fern4821@example.org")
        );
        assert_eq!(s.threads[&shipped.thread_id].email_ids.len(), 3);
        let trash = s.role_id("trash").unwrap();
        let promo: Vec<_> = s
            .emails
            .values()
            .filter(|e| {
                e.to.iter()
                    .flatten()
                    .any(|a| a.email == "quiet.heron7731@example.org")
            })
            .collect();
        assert_eq!(promo.len(), 2);
        assert!(promo.iter().all(|e| e.mailbox_ids.contains(&trash)));
        let kettle = &s.masked["masked-48210"];
        assert!(kettle.last_message_at.is_some());
        assert!(s.masked.values().all(|m| m.email.ends_with("@example.org")));
    }

    #[test]
    fn demo_uses_reserved_domains_only() {
        let s = demo_store();
        let ok = |a: &str| {
            let d = a.rsplit_once('@').map(|(_, d)| d).unwrap_or("");
            d == "example.com"
                || d == "example.org"
                || d == "example.net"
                || d.ends_with(".example.com")
                || d.ends_with(".example.org")
                || d.ends_with(".example.net")
        };
        for e in s.emails.values() {
            for a in [&e.from, &e.to, &e.cc].into_iter().flatten().flatten() {
                assert!(ok(&a.email), "{}", a.email);
            }
        }
        for c in s.contacts.values() {
            assert!(c.emails.iter().all(|(e, _)| ok(e)));
        }
    }

    #[test]
    fn demo_times_follow_now() {
        let now: Timestamp = "2027-01-01T00:00:00Z".parse().unwrap();
        let cfg = FakeConfig {
            now: Some(now),
            ..FakeConfig::default()
        };
        let mut store = Store::new(&cfg, now);
        fill(&mut store, now);
        let newest = store.emails.values().map(|e| e.received_at).max().unwrap();
        assert!(newest <= now);
        assert!(now.duration_since(newest) < SignedDuration::from_hours(1));
        let oldest = store.emails.values().map(|e| e.received_at).min().unwrap();
        assert!(now.duration_since(oldest) < SignedDuration::from_hours(24 * 62));
    }
}
