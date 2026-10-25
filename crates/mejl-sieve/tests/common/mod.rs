//! Shared fixtures for the integration tests. All names and addresses are
//! fictional.

#![allow(dead_code)]

use mejl_sieve::{
    Action, AddressHeader, AddressPart, Condition, MailboxRef, MatchMode, Rule, Test, TextOp,
};

pub fn from_domain_is(domain: &str) -> Condition {
    Condition::new(Test::Address {
        header: AddressHeader::From,
        part: AddressPart::Domain,
        op: TextOp::Is,
        value: domain.into(),
    })
}

/// The four rules shown on the rules screen in docs/mockups.html.
pub fn mockup_rules() -> Vec<Rule> {
    let mut newsletters = Rule::new("r_18aa", "Newsletters");
    newsletters
        .conditions
        .push(from_domain_is("news.example.net"));
    newsletters.actions.push(Action::MoveTo {
        mailbox: MailboxRef::new("M2a90", ["Newsletters"]),
    });

    let mut receipts = Rule::new("r_2c41", "Receipts to folder");
    receipts.match_mode = MatchMode::Any;
    receipts.conditions.push(from_domain_is("shop.example.com"));
    receipts.conditions.push(Condition::new(Test::Subject {
        op: TextOp::Contains,
        value: "receipt".into(),
    }));
    receipts.actions.push(Action::MoveTo {
        mailbox: MailboxRef::new("M7d1e", ["Receipts"]),
    });
    receipts.actions.push(Action::MarkRead);

    let mut garden = Rule::new("r_5d07", "Garden club label");
    garden.conditions.push(Condition::new(Test::ListId {
        op: TextOp::Contains,
        value: "garden-club.example.org".into(),
    }));
    garden.actions.push(Action::AddLabel {
        mailbox: MailboxRef::new("M4c3b", ["Garden club"]),
    });

    let mut promo = Rule::new("r_77e2", "Old promo");
    promo.enabled = false;
    promo.conditions.push(from_domain_is("promo.example.com"));
    promo.actions.push(Action::MoveToTrash);

    vec![newsletters, receipts, garden, promo]
}

/// A tiny deterministic PRNG (SplitMix64), so property tests need no rand
/// crate and fail the same way every run.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform-ish in `0..n`. `n` must not be zero.
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    pub fn chance(&mut self, percent: u64) -> bool {
        self.next_u64() % 100 < percent
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}
