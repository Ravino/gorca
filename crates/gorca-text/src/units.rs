//! Offset conversion between an application's raw text offsets and the core's Unicode scalar
//! offsets (ADR-0001 TU-1 to TU-3, ADR-0007 TQ-1).
//!
//! The text passed in is the text as the application returns it. For
//! [`TextUnit::Utf16Bom`] (Firefox) that means every astral scalar is followed by a phantom
//! U+FEFF, and a raw offset counts scalars of that string. Offsets that land inside a character
//! (a UTF-8 continuation byte, a phantom U+FEFF) snap to the start of that character.

use gorca_model::TextUnit;

/// Scalars between two checkpoints of the [`SparseIndex`].
const STRIDE: usize = 64;
const PHANTOM: char = '\u{FEFF}';

fn is_astral(c: char) -> bool {
    u32::from(c) > 0xFFFF
}

/// Scalar offset of a raw offset, or `None` when it is past the end of the text.
pub fn raw_to_scalar(text: &str, raw: u32, unit: TextUnit) -> Option<u32> {
    match unit {
        TextUnit::Scalar => Some(raw),
        TextUnit::Utf8 => {
            let byte = raw as usize;
            if byte > text.len() {
                return None;
            }
            let mut b = byte;
            while !text.is_char_boundary(b) {
                b -= 1;
            }
            u32::try_from(text[..b].chars().count()).ok()
        }
        TextUnit::Utf16Bom => {
            let mut scalar = 0u32;
            let mut chars = text.chars();
            let mut seen = 0u32;
            let mut prev_astral = false;
            while seen < raw {
                let c = chars.next()?;
                seen += 1;
                if prev_astral && c == PHANTOM {
                    prev_astral = false;
                    continue;
                }
                prev_astral = is_astral(c);
                scalar += 1;
            }
            // Between an astral scalar and its phantom: snap back to the astral scalar.
            if prev_astral && chars.next() == Some(PHANTOM) {
                scalar -= 1;
            }
            Some(scalar)
        }
    }
}

/// Raw offset of a scalar offset, or `None` when it is past the end of the text.
pub fn scalar_to_raw(text: &str, scalar: u32, unit: TextUnit) -> Option<u32> {
    match unit {
        TextUnit::Scalar => Some(scalar),
        TextUnit::Utf8 => {
            let mut n = 0u32;
            for (b, _) in text.char_indices() {
                if n == scalar {
                    return u32::try_from(b).ok();
                }
                n += 1;
            }
            (n == scalar)
                .then(|| u32::try_from(text.len()).ok())
                .flatten()
        }
        TextUnit::Utf16Bom => {
            let mut raw = 0u32;
            let mut n = 0u32;
            let mut chars = text.chars().peekable();
            while n < scalar {
                let c = chars.next()?;
                raw += 1;
                n += 1;
                if is_astral(c) && chars.peek() == Some(&PHANTOM) {
                    chars.next();
                    raw += 1;
                }
            }
            Some(raw)
        }
    }
}

/// Scalar to UTF-8 byte index with a checkpoint every 64 scalars (TU-3), so a lookup scans at
/// most 63 scalars instead of the whole text. Rebuild after the text changes.
#[derive(Clone, Debug, Default)]
pub struct SparseIndex {
    /// Byte offset of scalar `64 * k`.
    marks: Vec<u32>,
    scalars: u32,
    bytes: u32,
}

impl SparseIndex {
    pub fn build(text: &str) -> Self {
        let mut marks = Vec::with_capacity(text.len() / STRIDE + 1);
        let mut n = 0usize;
        for (b, _) in text.char_indices() {
            if n.is_multiple_of(STRIDE) {
                marks.push(u32::try_from(b).unwrap_or(u32::MAX));
            }
            n += 1;
        }
        Self {
            marks,
            scalars: u32::try_from(n).unwrap_or(u32::MAX),
            bytes: u32::try_from(text.len()).unwrap_or(u32::MAX),
        }
    }

    pub fn scalars(&self) -> u32 {
        self.scalars
    }

    /// Byte offset of the `scalar`-th scalar; `scalar == scalars()` gives `text.len()`.
    pub fn scalar_to_byte(&self, text: &str, scalar: u32) -> Option<usize> {
        if scalar > self.scalars {
            return None;
        }
        if scalar == self.scalars {
            return Some(self.bytes as usize);
        }
        let k = scalar as usize / STRIDE;
        let from = *self.marks.get(k)? as usize;
        let skip = scalar as usize % STRIDE;
        text.get(from..)?
            .char_indices()
            .nth(skip)
            .map(|(b, _)| from + b)
    }

    /// Scalar offset of a byte offset on a character boundary.
    pub fn byte_to_scalar(&self, text: &str, byte: usize) -> Option<u32> {
        if byte > self.bytes as usize || !text.is_char_boundary(byte) {
            return None;
        }
        let byte32 = u32::try_from(byte).ok()?;
        let k = self
            .marks
            .partition_point(|m| *m <= byte32)
            .saturating_sub(1);
        let from = *self.marks.get(k).unwrap_or(&0) as usize;
        let within = text.get(from..byte)?.chars().count();
        u32::try_from(k * STRIDE + within).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_offsets() {
        let t = "аб😀в";
        assert_eq!(raw_to_scalar(t, 0, TextUnit::Utf8), Some(0));
        assert_eq!(raw_to_scalar(t, 2, TextUnit::Utf8), Some(1));
        assert_eq!(
            raw_to_scalar(t, 3, TextUnit::Utf8),
            Some(1),
            "mid-char snaps back"
        );
        assert_eq!(raw_to_scalar(t, 8, TextUnit::Utf8), Some(3));
        assert_eq!(raw_to_scalar(t, 10, TextUnit::Utf8), Some(4));
        assert_eq!(raw_to_scalar(t, 11, TextUnit::Utf8), None);
        assert_eq!(scalar_to_raw(t, 3, TextUnit::Utf8), Some(8));
        assert_eq!(scalar_to_raw(t, 4, TextUnit::Utf8), Some(10));
        assert_eq!(scalar_to_raw(t, 5, TextUnit::Utf8), None);
    }

    #[test]
    fn utf16_bom_phantom_after_astral() {
        // Text as Firefox returns it: the astral scalar is followed by U+FEFF.
        let t = "a\u{1F600}\u{FEFF}b";
        assert_eq!(raw_to_scalar(t, 1, TextUnit::Utf16Bom), Some(1));
        assert_eq!(
            raw_to_scalar(t, 2, TextUnit::Utf16Bom),
            Some(1),
            "inside the pair"
        );
        assert_eq!(raw_to_scalar(t, 3, TextUnit::Utf16Bom), Some(2));
        assert_eq!(raw_to_scalar(t, 4, TextUnit::Utf16Bom), Some(3));
        assert_eq!(raw_to_scalar(t, 5, TextUnit::Utf16Bom), None);
        assert_eq!(scalar_to_raw(t, 2, TextUnit::Utf16Bom), Some(3));
        assert_eq!(scalar_to_raw(t, 3, TextUnit::Utf16Bom), Some(4));
        assert_eq!(scalar_to_raw(t, 4, TextUnit::Utf16Bom), None);
    }

    #[test]
    fn sparse_index_matches_the_linear_helpers() {
        let mut t = String::new();
        for i in 0..500u32 {
            t.push(match i % 5 {
                0 => 'a',
                1 => 'ж',
                2 => '\u{1F600}',
                3 => 'é',
                _ => ' ',
            });
        }
        let idx = SparseIndex::build(&t);
        assert_eq!(idx.scalars(), 500);
        for s in 0..=500u32 {
            let b = gorca_model::text::scalar_to_byte(&t, s as usize);
            assert_eq!(idx.scalar_to_byte(&t, s), b, "scalar {s}");
            assert_eq!(idx.byte_to_scalar(&t, b.unwrap_or(0)), Some(s));
        }
        assert_eq!(idx.scalar_to_byte(&t, 501), None);
        assert_eq!(idx.byte_to_scalar(&t, t.len() + 1), None);
        let empty = SparseIndex::build("");
        assert_eq!(empty.scalar_to_byte("", 0), Some(0));
    }
}
