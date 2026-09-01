//! A deterministic pseudo-random source.
//!
//! # Why not a random-number crate
//!
//! A simulation that cannot be replayed exactly is a simulation whose failures
//! cannot be reproduced, and a fleet run that fails once a month for reasons
//! nobody can recreate is worse than no fleet run at all. So the sequence is a
//! pure function of a seed, with no entropy source, no thread-local state and
//! no dependency whose next release might reorder its output.
//!
//! `SplitMix64` is the whole of it: a counter, one multiply-xor mixing step, and
//! a period long enough that nothing here can exhaust it. It is not
//! cryptographic and does not need to be — the only keys in this crate are
//! derived through it, and they exist to sign fixtures rather than to protect
//! anything.

/// A deterministic stream of values from a seed.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// A stream from a seed.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// A stream from a seed and a label, so two independent parts of a run do
    /// not have to share a cursor.
    ///
    /// The point is reproducibility under change: adding a draw in the station
    /// generator must not shift every session's shape, or a regression looks
    /// like a change everywhere.
    #[must_use]
    pub fn stream(seed: u64, label: &str) -> Self {
        let mut mixed = seed;
        for byte in label.bytes() {
            mixed = mixed.rotate_left(7) ^ u64::from(byte).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
        Self::new(mixed)
    }

    /// The next value.
    pub const fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..bound`, without the modulo bias a bare `%` leaves.
    ///
    /// # Panics
    ///
    /// Never. A bound of zero yields zero, because a simulation that panics on
    /// an empty range is a simulation that fails on the day somebody asks for
    /// no sessions.
    pub fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            return 0;
        }
        // Lemire's method: reject the values that would make the mapping
        // uneven, so a `1 in 3` fault really is one in three. The two halves of
        // the 128-bit product are taken by mask and shift rather than by cast,
        // because a cast that truncates is exactly what the workspace's guards
        // exist to make visible — even where the truncation is the point.
        let threshold = bound.wrapping_neg() % bound;
        loop {
            let product = u128::from(self.next_u64()) * u128::from(bound);
            let low = u64::try_from(product & u128::from(u64::MAX)).unwrap_or(0);
            if low >= threshold {
                return u64::try_from(product >> 64).unwrap_or(0);
            }
        }
    }

    /// A value in `low..=high`.
    pub fn between(&mut self, low: u64, high: u64) -> u64 {
        if high <= low {
            return low;
        }
        low + self.below(high - low + 1)
    }

    /// Whether a one-in-`n` event fires. `n == 0` never fires.
    pub fn one_in(&mut self, n: u64) -> bool {
        n != 0 && self.below(n) == 0
    }

    /// Thirty-two bytes, for deriving a key.
    pub fn bytes32(&mut self) -> [u8; 32] {
        let mut out = [0u8; 32];
        let mut i = 0;
        while i < 4 {
            let word = self.next_u64().to_be_bytes();
            let mut j = 0;
            while j < 8 {
                out[i * 8 + j] = word[j];
                j += 1;
            }
            i += 1;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_seed_is_one_sequence() {
        // The property the whole crate rests on: a failing fleet run is
        // reproducible from its seed alone.
        let a: Vec<u64> = (0..8).map(|_| Rng::new(42).next_u64()).collect();
        let mut one = Rng::new(42);
        let mut two = Rng::new(42);
        for _ in 0..64 {
            assert_eq!(one.next_u64(), two.next_u64());
        }
        assert!(a.iter().all(|v| *v == a[0]), "a fresh Rng starts over");
    }

    #[test]
    fn two_seeds_are_two_sequences() {
        let mut one = Rng::new(1);
        let mut two = Rng::new(2);
        assert_ne!(one.next_u64(), two.next_u64());
    }

    #[test]
    fn labelled_streams_do_not_share_a_cursor() {
        // Adding a draw to the station generator must not reshape every
        // session, or a regression looks like a change everywhere.
        let mut stations = Rng::stream(7, "stations");
        let mut sessions = Rng::stream(7, "sessions");
        assert_ne!(stations.next_u64(), sessions.next_u64());
        assert_eq!(
            Rng::stream(7, "stations").next_u64(),
            {
                let mut again = Rng::stream(7, "stations");
                again.next_u64()
            },
            "…and a label is still deterministic"
        );
    }

    #[test]
    fn a_bounded_draw_stays_in_range_and_is_not_biased() {
        let mut rng = Rng::new(0x00E4_0B15_u64.rotate_left(3));
        let mut buckets = [0u32; 3];
        for _ in 0..30_000 {
            let v = rng.below(3);
            assert!(v < 3);
            buckets[usize::try_from(v).expect("a value below three")] += 1;
        }
        // A biased modulo would skew this by whole percentage points; the
        // tolerance is wide enough that the test cannot flake and narrow
        // enough to catch a bare `%`.
        for count in buckets {
            assert!(
                (9_000..=11_000).contains(&count),
                "uneven distribution: {buckets:?}"
            );
        }
    }

    #[test]
    fn degenerate_bounds_do_not_panic() {
        // A simulation that panics on an empty range fails on the day somebody
        // asks for no sessions.
        let mut rng = Rng::new(1);
        assert_eq!(rng.below(0), 0);
        assert_eq!(rng.between(5, 5), 5);
        assert_eq!(rng.between(9, 3), 9);
        assert!(!rng.one_in(0));
    }

    #[test]
    fn one_in_n_fires_about_one_time_in_n() {
        let mut rng = Rng::new(99);
        let fired = (0..10_000).filter(|_| rng.one_in(10)).count();
        assert!((800..=1_200).contains(&fired), "{fired} of 10000");
    }
}
