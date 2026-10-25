// SPDX-License-Identifier: GPL-3.0-or-later
//! Redaction of secret text for diagnostics (the presentation log).
//!
//! Password text is spoken but never logged (docs/decisions.md). A diagnostic that must still
//! tell "the same text" from "other text" records [`RedactKey::tag`]: the length in characters and
//! a keyed hash (SipHash-2-4 under a per-run random key), never the text. The key is for
//! comparing a log against a truth source of the same run; without it a hash cannot be tested
//! against guesses offline, and the log file itself is created readable by its owner only.
//!
//! Both sides of a comparison must use this function: the daemon's presentation log and the
//! harness' truth source.

use crate::{NodeSnapshot, Role};

/// Does text of this node need redaction? Only the role says so: the AT-SPI state set has no
/// "protected" or "secret" state, and a password field announces itself by role.
pub fn is_sensitive(role: Role) -> bool {
    matches!(role, Role::PasswordEntry)
}

/// Is a node with this native role and (when it has one) fused entry a secret field? The native
/// role and the fused role are two signals; a sink that asks only one leaks when the other
/// disagrees (an engine reporting a password over a text-box token).
pub fn is_secret_node(role: Role, entry: Option<&crate::SemEntry>) -> bool {
    is_sensitive(role) || entry.is_some_and(|e| e.sem.role == crate::sem::SemRole::PasswordBox)
}

impl NodeSnapshot {
    /// The name, value, contents and every word made of them must not reach a log, the speech
    /// lane, an extension or the clipboard: the native role, the fused entry and the source record
    /// are asked, any one of them says it is a password.
    pub fn is_sensitive(&self) -> bool {
        is_secret_node(self.role, self.sem.entry.as_deref())
            || self
                .sem
                .rec
                .as_ref()
                .is_some_and(|r| r.sem.role == crate::sem::SemRole::PasswordBox)
    }
}

/// A 128-bit key, fresh for every run.
#[derive(Copy, Clone, Eq, PartialEq)]
pub struct RedactKey([u8; 16]);

impl std::fmt::Debug for RedactKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RedactKey(..)")
    }
}

impl RedactKey {
    pub fn from_bytes(b: [u8; 16]) -> Self {
        Self(b)
    }

    /// A fresh key from the operating system. If `/dev/urandom` cannot be read the key is derived
    /// from the clock and the address space, which is weaker but still per-run.
    pub fn random() -> Self {
        use std::io::Read;
        let mut b = [0u8; 16];
        let read = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b));
        if read.is_err() {
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let local = 0u8;
            let a = std::ptr::addr_of!(local) as usize as u128;
            let x = t ^ a.rotate_left(64) ^ u128::from(std::process::id());
            b = x.to_le_bytes();
        }
        Self(b)
    }

    pub fn to_hex(self) -> String {
        use std::fmt::Write;
        self.0.iter().fold(String::with_capacity(32), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != 32 || !s.is_ascii() {
            return None;
        }
        let mut b = [0u8; 16];
        for (i, byte) in b.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
        }
        Some(Self(b))
    }

    /// `redacted(len=<chars>,h=<16 hex digits>)` for `text`.
    pub fn tag(&self, text: &str) -> String {
        let h = siphash24(&self.0, text.as_bytes());
        format!("redacted(len={},h={h:016x})", text.chars().count())
    }
}

fn round(v: &mut [u64; 4]) {
    v[0] = v[0].wrapping_add(v[1]);
    v[1] = v[1].rotate_left(13) ^ v[0];
    v[0] = v[0].rotate_left(32);
    v[2] = v[2].wrapping_add(v[3]);
    v[3] = v[3].rotate_left(16) ^ v[2];
    v[0] = v[0].wrapping_add(v[3]);
    v[3] = v[3].rotate_left(21) ^ v[0];
    v[2] = v[2].wrapping_add(v[1]);
    v[1] = v[1].rotate_left(17) ^ v[2];
    v[2] = v[2].rotate_left(32);
}

fn siphash24(key: &[u8; 16], data: &[u8]) -> u64 {
    let word = |b: &[u8]| {
        let mut w = [0u8; 8];
        w.copy_from_slice(b);
        u64::from_le_bytes(w)
    };
    let (k0, k1) = (word(&key[..8]), word(&key[8..]));
    let mut v = [
        k0 ^ 0x736f_6d65_7073_6575,
        k1 ^ 0x646f_7261_6e64_6f6d,
        k0 ^ 0x6c79_6765_6e65_7261,
        k1 ^ 0x7465_6462_7974_6573,
    ];
    let (chunks, rest) = data.as_chunks::<8>();
    for c in chunks {
        let m = u64::from_le_bytes(*c);
        v[3] ^= m;
        round(&mut v);
        round(&mut v);
        v[0] ^= m;
    }
    let mut last = [0u8; 8];
    last[..rest.len()].copy_from_slice(rest);
    last[7] = u8::try_from(data.len() & 0xff).unwrap_or(0);
    let m = u64::from_le_bytes(last);
    v[3] ^= m;
    round(&mut v);
    round(&mut v);
    v[0] ^= m;
    v[2] ^= 0xff;
    for _ in 0..4 {
        round(&mut v);
    }
    v[0] ^ v[1] ^ v[2] ^ v[3]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn siphash24_matches_the_reference_vectors() {
        // Reference: key 00..0f, input 00..(n-1) (Aumasson and Bernstein, `vectors.h`).
        let key: [u8; 16] = std::array::from_fn(|i| u8::try_from(i).unwrap_or(0));
        let input: Vec<u8> = (0u8..15).collect();
        assert_eq!(siphash24(&key, &input[..0]), 0x726f_db47_dd0e_0e31);
        assert_eq!(siphash24(&key, &input[..1]), 0x74f8_39c5_93dc_67fd);
        assert_eq!(siphash24(&key, &input[..8]), 0x93f5_f579_9a93_2462);
        assert_eq!(siphash24(&key, &input[..15]), 0xa129_ca61_49be_45e5);
    }

    #[test]
    fn a_tag_has_the_length_and_no_text() {
        let k = RedactKey::from_bytes([7; 16]);
        let t = k.tag("hunter2");
        assert!(t.starts_with("redacted(len=7,h="), "{t}");
        assert!(!t.contains("hunter"));
        assert_eq!(t, k.tag("hunter2"));
        assert_ne!(t, k.tag("hunter3"));
        assert_ne!(t, RedactKey::from_bytes([8; 16]).tag("hunter2"));
        assert!(k.tag("пароль").starts_with("redacted(len=6,h="));
    }

    #[test]
    fn keys_round_trip_through_hex_and_are_fresh() {
        let k = RedactKey::random();
        assert_eq!(RedactKey::from_hex(&k.to_hex()), Some(k));
        assert_ne!(k, RedactKey::random());
        assert_eq!(RedactKey::from_hex("zz"), None);
        assert_eq!(RedactKey::from_hex(&"g".repeat(32)), None);
    }

    #[test]
    fn only_the_password_role_is_sensitive() {
        assert!(is_sensitive(Role::PasswordEntry));
        assert!(!is_sensitive(Role::CheckBox));
    }
}
