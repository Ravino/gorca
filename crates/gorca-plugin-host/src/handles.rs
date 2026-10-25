// SPDX-License-Identifier: GPL-3.0-or-later
//! Opaque per-instance handles (ADR-0026 §2.3.3).
//!
//! A handle is a `u64`: `index (24) ‖ generation (16) ‖ tag (24)`. The tag is SipHash-2-4 under a
//! per-instance CSPRNG key over `(instance, index, generation)`; a forgery succeeds with
//! p = 2^-24 and every failure is counted. Handles are minted only by the host. A forged,
//! foreign-instance or dead handle is rejected and never reads another node.

use crate::mac::{SipKey, siphash24};

const INDEX_BITS: u32 = 24;
const GEN_BITS: u32 = 16;
const TAG_BITS: u32 = 24;
const TAG_MASK: u64 = (1 << TAG_BITS) - 1;

/// Why a handle was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HandleError {
    /// The tag does not verify under this instance's key (forged or from another instance).
    #[error("handle tag mismatch")]
    Forged,
    /// The tag verifies but the generation is not the current one for that index.
    #[error("handle generation is dead")]
    Dead,
}

/// A verified handle's parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verified {
    /// Host-side slot index.
    pub index: u32,
    /// Generation at mint time.
    pub generation: u16,
}

/// Mints and verifies the handles of one plugin instance.
#[derive(Debug)]
pub struct HandleMinter {
    key: SipKey,
    instance: u64,
}

impl HandleMinter {
    /// A minter with a fresh random key (a rebuilt instance gets a new one, so its old handles
    /// are all dead).
    pub fn new(instance: u64) -> Result<Self, getrandom::Error> {
        Ok(Self::with_key(instance, SipKey::random()?))
    }

    /// A minter with an explicit key (tests).
    #[must_use]
    pub fn with_key(instance: u64, key: SipKey) -> Self {
        Self { key, instance }
    }

    fn tag(&self, index: u32, generation: u16) -> u64 {
        let mut m = [0u8; 14];
        m[..8].copy_from_slice(&self.instance.to_le_bytes());
        m[8..12].copy_from_slice(&index.to_le_bytes());
        m[12..].copy_from_slice(&generation.to_le_bytes());
        siphash24(self.key, &m) & TAG_MASK
    }

    /// Mint the handle of `(index, generation)`. `index` is masked to 24 bits.
    #[must_use]
    pub fn mint(&self, index: u32, generation: u16) -> u64 {
        let index = index & ((1 << INDEX_BITS) - 1);
        (u64::from(index) << (GEN_BITS + TAG_BITS))
            | (u64::from(generation) << TAG_BITS)
            | self.tag(index, generation)
    }

    /// Check a guest-supplied handle. `current_generation` maps an index to its live generation
    /// (`None` = no such slot).
    pub fn verify(
        &self,
        handle: u64,
        current_generation: impl FnOnce(u32) -> Option<u16>,
    ) -> Result<Verified, HandleError> {
        let tag = handle & TAG_MASK;
        let generation = ((handle >> TAG_BITS) & 0xffff) as u16;
        let index = (handle >> (GEN_BITS + TAG_BITS)) as u32;
        let want = self.tag(index, generation);
        // A 24-bit tag: plain comparison leaks nothing the failure counter does not already bound.
        if tag != want {
            return Err(HandleError::Forged);
        }
        match current_generation(index) {
            Some(g) if g == generation => Ok(Verified { index, generation }),
            Some(_) | None => Err(HandleError::Dead),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minter(instance: u64, k: u64) -> HandleMinter {
        HandleMinter::with_key(instance, SipKey(k, !k))
    }

    #[test]
    fn minted_handle_verifies() {
        let m = minter(1, 7);
        let h = m.mint(42, 3);
        assert_eq!(
            m.verify(h, |i| (i == 42).then_some(3)),
            Ok(Verified {
                index: 42,
                generation: 3
            })
        );
    }

    #[test]
    fn flipped_bits_are_forged() {
        let m = minter(1, 7);
        let h = m.mint(42, 3);
        for bit in 0..64 {
            let bad = h ^ (1u64 << bit);
            assert!(m.verify(bad, |_| Some(3)).is_err(), "bit {bit}");
        }
    }

    #[test]
    fn foreign_instance_handle_is_rejected() {
        let a = minter(1, 7);
        let b = minter(2, 9);
        let h = a.mint(5, 1);
        assert_eq!(b.verify(h, |_| Some(1)), Err(HandleError::Forged));
        // Same key, different instance id: still foreign.
        let c = minter(2, 7);
        assert_eq!(c.verify(h, |_| Some(1)), Err(HandleError::Forged));
    }

    #[test]
    fn stale_generation_is_dead_not_forged() {
        let m = minter(1, 7);
        let h = m.mint(5, 1);
        assert_eq!(m.verify(h, |_| Some(2)), Err(HandleError::Dead));
        assert_eq!(m.verify(h, |_| None), Err(HandleError::Dead));
    }

    #[test]
    fn fresh_random_keys_differ() {
        let a = HandleMinter::new(1).expect("csprng");
        let b = HandleMinter::new(1).expect("csprng");
        assert_ne!(a.mint(1, 1), b.mint(1, 1));
    }
}
