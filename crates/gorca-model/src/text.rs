//! Text units and queries (ADR-0001 section 4.3).
//!
//! - **TU-1** The core API uses Unicode scalar value offsets.
//! - **TU-2** Connectors and sources convert at the edge (Firefox UTF-16 with phantom U+FEFF,
//!   VTE UTF-8 byte offsets).
//! - **TU-3** Storage is UTF-8 with a sparse scalar-offset index every 64 scalars.
//!   TBD: the sparse index is not implemented yet; the helpers below are linear.

use std::ops::Range;
use std::sync::Arc;

use crate::geometry::Rect;
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
    GetText {
        node: NodeKey,
        range: Range<u32>,
    },
    CaretOffset {
        node: NodeKey,
    },
    Attributes {
        node: NodeKey,
        range: Range<u32>,
    },
    /// Lines of `node` that intersect `clip` (coordinates relative to the node's surface, ADR-0006
    /// 3), top to bottom. Answered with `BackendReply::Lines`. The connector truncates at
    /// `max_lines` lines or `max_bytes` bytes of text and says so in [`TextLines::truncated`].
    VisibleLines {
        node: NodeKey,
        clip: Rect,
        max_lines: u16,
        max_bytes: u32,
    },
    /// One `GetStringAtOffset(granularity)` call (ADR-0007 4.2). `offset` is in the app's text
    /// unit; answered with `BackendReply::Unit`, whose range is in scalars of the same text.
    AtOffset {
        node: NodeKey,
        offset: u32,
        unit: TextGranularity,
    },
    /// `GetNSelections` plus `GetSelection(i)` for `i < 8`, each with its text. Answered with
    /// `BackendReply::Selections`.
    Selections {
        node: NodeKey,
    },
    /// The text around `around` (the caret when `None`) for the Reader's text window (ADR-0007
    /// TW-1): `radius` scalars on each side, clamped to the text. Answered with
    /// `BackendReply::Window`.
    Window {
        node: NodeKey,
        around: Option<u32>,
        radius: u32,
    },
}

/// Granularity of a `GetStringAtOffset` call (the values of `AtspiTextGranularity`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum TextGranularity {
    Char,
    Word,
    Sentence,
    Line,
    Paragraph,
}

impl TextGranularity {
    /// The wire value of `AtspiTextGranularity`.
    pub const fn wire(self) -> u32 {
        match self {
            TextGranularity::Char => 0,
            TextGranularity::Word => 1,
            TextGranularity::Sentence => 2,
            TextGranularity::Line => 3,
            TextGranularity::Paragraph => 4,
        }
    }
}

/// Reply of [`TextQuery::AtOffset`].
#[derive(Clone, Eq, PartialEq)]
pub struct TextUnitAt {
    pub range: Range<u32>,
    pub text: Arc<str>,
}

/// Reply of [`TextQuery::Window`].
#[derive(Clone, Eq, PartialEq)]
pub struct TextWindowFill {
    /// Scalar offset of the first character of `text`.
    pub base: u32,
    pub text: Arc<str>,
    /// Length of the whole text in scalars.
    pub len: u32,
    /// The caret, when the query asked for it (`around == None`) and the app answered.
    pub caret: Option<u32>,
}

/// Longest selection whose text travels with [`TextSelections`]; longer ones are counted only.
pub const MAX_SELECTION_TEXT: u32 = 1024;

/// Reply of [`TextQuery::Selections`]: each range with its text, or an empty text when the range
/// is longer than [`MAX_SELECTION_TEXT`] scalars.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct TextSelections {
    pub ranges: Vec<(Range<u32>, Arc<str>)>,
}

// Text read from an application may be a password: these show lengths only.
impl std::fmt::Debug for TextUnitAt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextUnitAt")
            .field("range", &self.range)
            .field("chars", &self.text.chars().count())
            .finish()
    }
}

impl std::fmt::Debug for TextWindowFill {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextWindowFill")
            .field("base", &self.base)
            .field("chars", &self.text.chars().count())
            .field("len", &self.len)
            .field("caret", &self.caret)
            .finish()
    }
}

impl std::fmt::Debug for TextSelections {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let spans: Vec<&Range<u32>> = self.ranges.iter().map(|(r, _)| r).collect();
        f.debug_struct("TextSelections")
            .field("ranges", &spans)
            .finish_non_exhaustive()
    }
}

/// One visual line of a text object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextLine {
    /// Scalar offsets (TU-1) of the line in the node's text.
    pub range: Range<u32>,
    pub text: Arc<str>,
    /// Extents relative to the surface; `None` when the toolkit could not give them.
    pub rect: Option<Rect>,
}

/// Reply of [`TextQuery::VisibleLines`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TextLines {
    pub lines: Vec<TextLine>,
    /// The connector stopped at a bound.
    pub truncated: bool,
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
