// SPDX-License-Identifier: GPL-3.0-or-later
//! Keyed primitives (ADR-0026 §2.3.3, harness-security decision of 2026-10-24).
//!
//! * [`hmac_sha256`] authenticates `.cwasm` artifacts with the per-install secret before
//!   `Component::deserialize`.
//! * [`SipKey`] / [`siphash24`] tag handles with a per-instance CSPRNG key.
//! * [`random_bytes`] is the one entropy source (the OS CSPRNG through `getrandom`).

use sha2::{Digest, Sha256};

/// Fill an array from the OS CSPRNG. A failure is reported, never papered over with weak data.
pub fn random_bytes<const N: usize>() -> Result<[u8; N], getrandom::Error> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b)?;
    Ok(b)
}

const BLOCK: usize = 64;

/// HMAC-SHA256 (RFC 2104).
#[must_use]
pub fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        let d = Sha256::digest(key);
        k[..32].copy_from_slice(&d);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    for p in parts {
        inner.update(p);
    }
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    outer.finalize().into()
}

/// Constant-time equality of two byte strings (length is not secret).
#[must_use]
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut d = 0u8;
    for (x, y) in a.iter().zip(b) {
        d |= x ^ y;
    }
    d == 0
}

/// A 128-bit SipHash key.
#[derive(Clone, Copy)]
pub struct SipKey(pub u64, pub u64);

impl std::fmt::Debug for SipKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SipKey(..)")
    }
}

impl SipKey {
    /// A fresh random key.
    pub fn random() -> Result<Self, getrandom::Error> {
        let b = random_bytes::<16>()?;
        let mut lo = [0u8; 8];
        let mut hi = [0u8; 8];
        lo.copy_from_slice(&b[..8]);
        hi.copy_from_slice(&b[8..]);
        Ok(Self(u64::from_le_bytes(lo), u64::from_le_bytes(hi)))
    }
}

macro_rules! sipround {
    ($v0:ident, $v1:ident, $v2:ident, $v3:ident) => {
        $v0 = $v0.wrapping_add($v1);
        $v1 = $v1.rotate_left(13);
        $v1 ^= $v0;
        $v0 = $v0.rotate_left(32);
        $v2 = $v2.wrapping_add($v3);
        $v3 = $v3.rotate_left(16);
        $v3 ^= $v2;
        $v0 = $v0.wrapping_add($v3);
        $v3 = $v3.rotate_left(21);
        $v3 ^= $v0;
        $v2 = $v2.wrapping_add($v1);
        $v1 = $v1.rotate_left(17);
        $v1 ^= $v2;
        $v2 = $v2.rotate_left(32);
    };
}

/// SipHash-2-4 of `msg`.
#[must_use]
pub fn siphash24(key: SipKey, msg: &[u8]) -> u64 {
    let SipKey(k0, k1) = key;
    let mut v0 = k0 ^ 0x736f_6d65_7073_6575;
    let mut v1 = k1 ^ 0x646f_7261_6e64_6f6d;
    let mut v2 = k0 ^ 0x6c79_6765_6e65_7261;
    let mut v3 = k1 ^ 0x7465_6462_7974_6573;
    let (words, rem) = msg.as_chunks::<8>();
    for w in words {
        let m = u64::from_le_bytes(*w);
        v3 ^= m;
        sipround!(v0, v1, v2, v3);
        sipround!(v0, v1, v2, v3);
        v0 ^= m;
    }
    let mut last = [0u8; 8];
    last[..rem.len()].copy_from_slice(rem);
    // The length byte is the message length mod 256 by definition.
    #[allow(clippy::cast_possible_truncation)]
    {
        last[7] = msg.len() as u8;
    }
    let m = u64::from_le_bytes(last);
    v3 ^= m;
    sipround!(v0, v1, v2, v3);
    sipround!(v0, v1, v2, v3);
    v0 ^= m;
    v2 ^= 0xff;
    sipround!(v0, v1, v2, v3);
    sipround!(v0, v1, v2, v3);
    sipround!(v0, v1, v2, v3);
    sipround!(v0, v1, v2, v3);
    v0 ^ v1 ^ v2 ^ v3
}

/// Lower-case hex.
#[must_use]
pub fn hex(b: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        let _ = write!(s, "{x:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn siphash24_reference_vectors() {
        // Reference key 00 01 .. 0f; vectors from the SipHash paper's test set.
        let key = SipKey(0x0706_0504_0302_0100, 0x0f0e_0d0c_0b0a_0908);
        assert_eq!(siphash24(key, &[]), 0x726f_db47_dd0e_0e31);
        assert_eq!(siphash24(key, &[0]), 0x74f8_39c5_93dc_67fd);
        let m: Vec<u8> = (0..15).collect();
        assert_eq!(siphash24(key, &m), 0xa129_ca61_49be_45e5);
    }

    #[test]
    fn hmac_sha256_rfc4231_case_2() {
        let mac = hmac_sha256(b"Jefe", &[b"what do ya want ", b"for nothing?"]);
        assert_eq!(
            hex(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn ct_eq_rejects_length_and_content() {
        assert!(ct_eq(b"ab", b"ab"));
        assert!(!ct_eq(b"ab", b"ac"));
        assert!(!ct_eq(b"ab", b"abc"));
    }
}
