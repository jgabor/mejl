//! Opaque id generation and a tiny deterministic random number generator.

/// A xorshift64* generator.
///
/// Deterministic for a given seed, which keeps demo and synthetic data stable
/// across runs. It is not suitable for anything secret.
#[derive(Debug, Clone)]
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        // xorshift has a fixed point at zero, so the seed is mixed first.
        let mixed = splitmix(seed);
        Rng(if mixed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            mixed
        })
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A value in `0..n`, or 0 when `n` is 0.
    pub(crate) fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }

    /// A value in `lo..=hi`.
    pub(crate) fn between(&mut self, lo: u64, hi: u64) -> u64 {
        if hi <= lo {
            lo
        } else {
            lo + self.below(hi - lo + 1)
        }
    }

    /// True with the given chance in percent.
    pub(crate) fn percent(&mut self, chance: u64) -> bool {
        self.below(100) < chance
    }

    /// One item of a non-empty slice. Returns the first item's default when
    /// the slice is empty, so callers never panic.
    pub(crate) fn pick<'a>(&mut self, items: &'a [&'a str]) -> &'a str {
        if items.is_empty() {
            return "";
        }
        let idx = self.below(items.len() as u64) as usize;
        items.get(idx).copied().unwrap_or("")
    }
}

/// The splitmix64 finaliser. It is a bijection on u64, so distinct inputs
/// always give distinct outputs.
pub(crate) fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Hands out ids that look opaque but are unique and deterministic.
///
/// Each id is a type prefix followed by the hex form of a bijective mix of a
/// counter. Clients must not read meaning into them, and the varying length
/// makes fixed-width assumptions fail early.
#[derive(Debug, Clone, Default)]
pub(crate) struct IdGen {
    next: u64,
}

impl IdGen {
    pub(crate) fn make(&mut self, prefix: char) -> String {
        self.next += 1;
        format!("{prefix}{:x}", splitmix(self.next))
    }
}

/// A 64-bit FNV-1a hash, used for stable query state fingerprints.
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn rng_is_deterministic_per_seed() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        let mut c = Rng::new(8);
        let xs: Vec<u64> = (0..16).map(|_| a.next_u64()).collect();
        let ys: Vec<u64> = (0..16).map(|_| b.next_u64()).collect();
        let zs: Vec<u64> = (0..16).map(|_| c.next_u64()).collect();
        assert_eq!(xs, ys);
        assert_ne!(xs, zs);
    }

    #[test]
    fn zero_seed_still_produces_values() {
        let mut r = Rng::new(0);
        assert!((0..8).map(|_| r.next_u64()).any(|v| v != 0));
    }

    #[test]
    fn ranges_hold() {
        let mut r = Rng::new(1);
        for _ in 0..1000 {
            let v = r.between(3, 9);
            assert!((3..=9).contains(&v));
            assert!(r.below(5) < 5);
        }
        assert_eq!(r.below(0), 0);
        assert_eq!(r.between(4, 4), 4);
        assert_eq!(r.between(9, 4), 9);
        assert_eq!(r.pick(&[]), "");
    }

    #[test]
    fn ids_are_unique() {
        let mut ids = IdGen::default();
        let set: BTreeSet<String> = (0..10_000).map(|_| ids.make('E')).collect();
        assert_eq!(set.len(), 10_000);
        assert!(set.iter().all(|id| id.starts_with('E')));
    }
}
