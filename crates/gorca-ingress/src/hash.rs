// SPDX-License-Identifier: GPL-3.0-or-later
//! A small multiplicative hasher for the node tables.
//!
//! The keys are 64-bit object-path hashes the application controls, so the state carries a
//! per-table random seed: an application cannot pre-compute a set of colliding keys. The cost
//! per lookup is a few multiplications instead of a SipHash round.

use std::hash::{BuildHasher, Hasher, RandomState};

const K: u64 = 0xf135_7aea_2e62_a9c5;

#[derive(Copy, Clone, Debug)]
pub struct FastState(u64);

impl Default for FastState {
    fn default() -> Self {
        // `RandomState` is seeded from the OS once per thread; hashing a constant with it gives
        // a fresh value per table.
        Self(RandomState::new().hash_one(0x5eed_u64) | 1)
    }
}

impl BuildHasher for FastState {
    type Hasher = FastHasher;

    fn build_hasher(&self) -> FastHasher {
        FastHasher(self.0)
    }
}

#[derive(Copy, Clone, Debug)]
pub struct FastHasher(u64);

impl FastHasher {
    #[inline]
    fn add(&mut self, x: u64) {
        self.0 = (self.0.rotate_left(5) ^ x).wrapping_mul(K);
    }
}

impl Hasher for FastHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let (chunks, rest) = bytes.as_chunks::<8>();
        for c in chunks {
            self.add(u64::from_le_bytes(*c));
        }
        if !rest.is_empty() {
            let mut w = [0u8; 8];
            w[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(w));
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.0.rotate_left(26)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_keys_hash_differently_and_seeds_differ() {
        let a = FastState::default();
        let b = FastState::default();
        assert_ne!(a.hash_one(1u64), a.hash_one(2u64));
        assert_ne!(a.hash_one(1u64), b.hash_one(1u64));
        let long: &[u8] = b"abcdefghij";
        assert_eq!(a.hash_one(long), a.hash_one(long));
    }
}
