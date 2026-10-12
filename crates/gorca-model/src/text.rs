//! Text units and queries (ADR-0001 section 4.3).
//!
//! - **TU-1** The core API uses Unicode scalar value offsets.
//! - **TU-2** Connectors and sources convert at the edge (Firefox UTF-16 with phantom U+FEFF,
//!   VTE UTF-8 byte offsets).
//! - **TU-3** Storage is UTF-8 with a sparse scalar-offset index every 64 scalars.
//!   TBD: the sparse index is not implemented yet; the helpers below are linear.

use std::ops::Range;

use crate::ids::NodeKey;

/// Offset unit used by a source or toolkit.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum TextUnit {
    /// UTF-16 code units with a phantom U+FEFF after each astral character (Firefox).
    Utf16Bom,
    /// Unicode scalar values (Chromium; the core's own unit).
    Scalar,
    /// UTF-8 byte offsets (VTE attribute runs).
    Utf8,
}

/// Core text queries. Text is not part of the node snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TextQuery {
    GetText { node: NodeKey, range: Range<u32> },
    CaretOffset { node: NodeKey },
    Attributes { node: NodeKey, range: Range<u32> },
}

/// Byte offset of the `scalar`-th scalar value, or `None` when out of range.
/// `scalar == char count` yields `text.len()`.
pub fn scalar_to_byte(text: &str, scalar: usize) -> Option<usize> {
    if scalar == 0 {
        return Some(0);
    }
    let mut count = 0usize;
    for (byte, _) in text.char_indices() {
        if count == scalar {
            return Some(byte);
        }
        count += 1;
    }
    (count == scalar).then_some(text.len())
}

/// Scalar offset of a byte offset. `None` when the offset is not on a char boundary.
pub fn byte_to_scalar(text: &str, byte: usize) -> Option<usize> {
    if byte > text.len() || !text.is_char_boundary(byte) {
        return None;
    }
    Some(text[..byte].chars().count())
}

/// Clamp a requested range into `[0, last_known_len]` (T-3: never send out-of-range values to
/// an application).
pub fn clamp_range(range: Range<u32>, last_known_len: u32) -> Range<u32> {
    let end = range.end.min(last_known_len);
    let start = range.start.min(end);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_byte_roundtrip_with_astral() {
        let s = "a\u{1F600}b\u{e9}";
        assert_eq!(scalar_to_byte(s, 0), Some(0));
        assert_eq!(scalar_to_byte(s, 1), Some(1));
        assert_eq!(scalar_to_byte(s, 2), Some(5));
        assert_eq!(scalar_to_byte(s, 4), Some(s.len()));
        assert_eq!(scalar_to_byte(s, 5), None);
        for scalar in 0..=4 {
            let byte = scalar_to_byte(s, scalar).unwrap();
            assert_eq!(byte_to_scalar(s, byte), Some(scalar));
        }
    }

    #[test]
    fn byte_to_scalar_rejects_mid_char() {
        assert_eq!(byte_to_scalar("\u{e9}", 1), None);
        assert_eq!(byte_to_scalar("abc", 4), None);
    }

    #[test]
    fn clamp_never_exceeds_len_and_never_inverts() {
        assert_eq!(clamp_range(2..100, 10), 2..10);
        assert_eq!(clamp_range(50..100, 10), 10..10);
        assert_eq!(clamp_range(0..0, 0), 0..0);
    }
}
