//! Randomness behind a trait, so that code that needs it can be tested with
//! a generator whose output is known.
//!
//! [`SystemRng`] is what the harness runs with. [`SeededRng`] is the fake: the
//! same seed gives the same numbers on every machine, which is what makes
//! retry jitter, generated ids and property tests repeatable.
//!
//! **Not for secrets.** These generators are for ids, jitter and temporary
//! file names. Neither is a cryptographic generator. A token that must be
//! unguessable needs the operating system's generator, which arrives with the
//! step that first needs one.

use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

/// A source of random numbers.
pub trait Rng: Send + std::fmt::Debug {
    /// The next 64 random bits.
    fn next_u64(&mut self) -> u64;

    /// A number in `0..bound`, each equally likely. `0` when `bound` is 0.
    fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            return 0;
        }
        // Numbers under `threshold` are the ones that would make the low
        // remainders more likely than the high ones. Draw again past them.
        let threshold = bound.wrapping_neg() % bound;
        loop {
            let value = self.next_u64();
            if value >= threshold {
                return value % bound;
            }
        }
    }

    /// A number in `[0, 1)`, for jitter.
    fn unit(&mut self) -> f64 {
        // 53 bits fill an f64's mantissa exactly.
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Fills `bytes` with random bytes.
    fn fill(&mut self, bytes: &mut [u8]) {
        for chunk in bytes.chunks_mut(8) {
            let value = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&value[..chunk.len()]);
        }
    }
}

/// The generator both implementations run: xoshiro256**, a small, fast
/// generator with 256 bits of state. Its state is filled from a 64-bit seed
/// by SplitMix64, as its authors recommend.
#[derive(Clone)]
struct Xoshiro256 {
    state: [u64; 4],
}

impl Xoshiro256 {
    fn from_seed(seed: u64) -> Self {
        let mut mixer = seed;
        let mut state = [0u64; 4];
        for word in &mut state {
            *word = split_mix_64(&mut mixer);
        }
        Self { state }
    }

    fn next(&mut self) -> u64 {
        let [s0, s1, s2, s3] = &mut self.state;
        let result = s1.wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let shifted = *s1 << 17;
        *s2 ^= *s0;
        *s3 ^= *s1;
        *s1 ^= *s2;
        *s0 ^= *s3;
        *s2 ^= shifted;
        *s3 = s3.rotate_left(45);
        result
    }
}

/// One step of SplitMix64: advances `state` and returns the next output.
fn split_mix_64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut mixed = *state;
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^ (mixed >> 31)
}

/// A generator whose output is decided by its seed: the fake.
///
/// The same seed gives the same sequence in every build and on every OS, so
/// a test can assert on what code did with "random" numbers.
#[derive(Clone)]
pub struct SeededRng(Xoshiro256);

impl SeededRng {
    /// A generator that repeats the sequence of every other generator made
    /// with this seed.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(Xoshiro256::from_seed(seed))
    }
}

impl Rng for SeededRng {
    fn next_u64(&mut self) -> u64 {
        self.0.next()
    }
}

impl std::fmt::Debug for SeededRng {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SeededRng")
    }
}

/// A generator seeded differently in every process and for every instance.
///
/// The seed comes from the keys the standard library gives its hash maps,
/// which the operating system's generator provides, mixed with a counter.
/// That makes two instances differ and makes the output unpredictable enough
/// for ids, jitter and file names. It is not for secrets.
#[derive(Clone)]
pub struct SystemRng(Xoshiro256);

impl SystemRng {
    /// A freshly seeded generator.
    #[must_use]
    pub fn new() -> Self {
        static INSTANCES: AtomicU64 = AtomicU64::new(0);
        let instance = INSTANCES.fetch_add(1, Ordering::Relaxed);
        let mut state = [0u64; 4];
        for (index, word) in (0u64..).zip(&mut state) {
            let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
            hasher.write_u64(instance);
            hasher.write_u64(index);
            *word = hasher.finish();
        }
        // xoshiro's one forbidden state is all zeros.
        if state == [0; 4] {
            state = Xoshiro256::from_seed(instance).state;
        }
        Self(Xoshiro256 { state })
    }
}

impl Default for SystemRng {
    fn default() -> Self {
        Self::new()
    }
}

impl Rng for SystemRng {
    fn next_u64(&mut self) -> u64 {
        self.0.next()
    }
}

impl std::fmt::Debug for SystemRng {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SystemRng")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_seeding_step_matches_its_published_outputs() {
        // The first outputs of SplitMix64 from a state of zero.
        let mut state = 0;
        assert_eq!(split_mix_64(&mut state), 0xe220_a839_7b1d_cdaf);
        assert_eq!(split_mix_64(&mut state), 0x6e78_9e6a_a1b9_65f4);
        assert_eq!(split_mix_64(&mut state), 0x06c4_5d18_8009_454f);
    }

    #[test]
    fn the_generator_matches_its_reference_from_a_known_state() {
        // xoshiro256** from the state 1, 2, 3, 4: the first output is
        // rotl(2 * 5, 7) * 9, and the rest follow from the state update.
        let mut generator = Xoshiro256 {
            state: [1, 2, 3, 4],
        };
        assert_eq!(generator.next(), 11_520);
        assert_eq!(generator.next(), 0);
        assert_eq!(generator.next(), 1_509_978_240);
        assert_eq!(generator.next(), 1_215_971_899_390_074_240);
    }

    #[test]
    fn the_same_seed_gives_the_same_numbers() {
        let mut first = SeededRng::new(42);
        let mut second = SeededRng::new(42);
        let mut other = SeededRng::new(43);
        let a: Vec<u64> = (0..8).map(|_| first.next_u64()).collect();
        let b: Vec<u64> = (0..8).map(|_| second.next_u64()).collect();
        let c: Vec<u64> = (0..8).map(|_| other.next_u64()).collect();
        assert_eq!(a, b);
        assert_ne!(a, c);
        // Pinned, so that a change to the generator is noticed: ids and
        // property-test inputs depend on it.
        assert_eq!(SeededRng::new(0).next_u64(), 0x99ec_5f36_cb75_f2b4);
    }

    #[test]
    fn below_stays_under_its_bound_and_reaches_every_value() {
        let mut rng = SeededRng::new(1);
        assert_eq!(rng.below(0), 0);
        assert_eq!(rng.below(1), 0);
        for bound in [2u64, 3, 7, 10, 1 << 40, u64::MAX] {
            for _ in 0..200 {
                assert!(rng.below(bound) < bound);
            }
        }
        let mut seen = [0u32; 6];
        for _ in 0..6000 {
            seen[rng.below(6) as usize] += 1;
        }
        for count in seen {
            assert!((800..1200).contains(&count), "uneven: {seen:?}");
        }
    }

    #[test]
    fn unit_is_in_the_half_open_interval() {
        let mut rng = SeededRng::new(9);
        let mut sum = 0.0;
        for _ in 0..4000 {
            let value = rng.unit();
            assert!((0.0..1.0).contains(&value));
            sum += value;
        }
        assert!((1900.0..2100.0).contains(&sum), "mean is off: {sum}");
    }

    #[test]
    fn fill_writes_every_byte_of_any_length() {
        let mut rng = SeededRng::new(3);
        for length in [0usize, 1, 7, 8, 9, 31] {
            let mut bytes = vec![0u8; length];
            rng.fill(&mut bytes);
            if length >= 8 {
                assert!(bytes.iter().any(|&byte| byte != 0));
            }
        }
        let mut a = [0u8; 20];
        let mut b = [0u8; 20];
        SeededRng::new(5).fill(&mut a);
        SeededRng::new(5).fill(&mut b);
        assert_eq!(a, b);
    }

    #[test]
    fn system_generators_differ_from_each_other() {
        let mut first = SystemRng::new();
        let mut second = SystemRng::default();
        let a: Vec<u64> = (0..4).map(|_| first.next_u64()).collect();
        let b: Vec<u64> = (0..4).map(|_| second.next_u64()).collect();
        assert_ne!(a, b);
        assert_ne!(a[0], a[1]);
        assert_eq!(format!("{first:?}"), "SystemRng");
    }

    #[test]
    fn a_generator_can_be_used_through_the_trait() {
        let mut boxed: Box<dyn Rng> = Box::new(SeededRng::new(11));
        assert!(boxed.below(100) < 100);
        assert_eq!(format!("{boxed:?}"), "SeededRng");
    }
}
