// SPDX-License-Identifier: GPL-3.0-or-later
//! Process-local monotonic nanosecond clock shared by feeders and consumers.

use std::sync::OnceLock;
use std::time::Instant;

fn epoch() -> Instant {
    static T0: OnceLock<Instant> = OnceLock::new();
    *T0.get_or_init(Instant::now)
}

/// Pin the epoch. Call once at start-up so the first reading is not slow.
pub fn init() {
    let _ = epoch();
}

/// Nanoseconds since [`init`] (or the first call). Monotonic, comparable across threads.
#[inline]
pub fn now_ns() -> u64 {
    u64::try_from(epoch().elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Deterministic xorshift64* generator. Seeded, so workloads are reproducible.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0..n` (n > 0), slightly biased, which is fine for workloads.
    #[inline]
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }

    /// Uniform in `[0, 1)`.
    #[inline]
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}
