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
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum TextUnit {
    /// UTF-16 code units with a phantom U+FEFF after each astral character (Firefox).
    Utf16Bom,
    /// Unicode scalar values (Chromium; the core's own unit).
    #[default]
    Scalar,
    /// UTF-8 byte offsets (VTE attribute runs).
    Utf8,
}

impl TextUnit {
    /// Can the Reader's text window follow this app in the app's own offsets?
    ///
    /// Scalars: yes. Gecko's UTF-16 offsets: yes, because the string it hands out already holds
    /// a phantom U+FEFF after every astral scalar, so each offset is the index of a character
    /// of that very string; the window keeps the string as it is and the unit rules keep a
    /// phantom with its scalar (ADR-0007 changelog, M3b; no conversion, no run cache). Byte
    /// offsets (VTE): no, they need the terminal's own layer (M6).
    pub const fn window_follows(self) -> bool {
        match self {
            TextUnit::Scalar | TextUnit::Utf16Bom => true,
            TextUnit::Utf8 => false,
        }
    }
}

/// Where a word-wise caret move (Ctrl+Right) ends relative to the word it passed.
///
/// Measured in the editing matrix (docs/reports/unified-editing.md): every Linux toolkit stops at
/// the END of the word it passes (GTK/Pango, GtkSourceView, Gecko, Blink, readline), where the
/// word just passed is the one that ends at the caret. Windows-style toolkits stop at the START
/// of the next word, where the word at the caret is the one just reached.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum WordLanding {
    /// The caret ends behind the passed word.
    #[default]
    End,
    /// The caret ends in front of the next word.
    Start,
}

/// Per-toolkit facts the shared editing presenter needs (ADR-0008: connector data, never a
/// toolkit name tested in the core). One row per toolkit class, selected by the connector.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct EditQuirks {
    /// Where Ctrl+Right lands (see [`WordLanding`]).
    pub word_landing: WordLanding,
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
    /// The character formatting at `offset` (`info.formatting`, ADR-0007 M3b). Answered with
    /// `BackendReply::Format`.
    Format {
        node: NodeKey,
        offset: u32,
    },
    /// The text around `around` (the caret when `None`) for the Reader's text window (ADR-0007
    /// TW-1): `radius` scalars on each side, clamped to the text. Answered with
    /// `BackendReply::Window`.
    Window {
        node: NodeKey,
        around: Option<u32>,
        radius: u32,
    },
    /// The whole text of `node` and its caret, for a terminal's visible screen (M6, ADR-0001
    /// T-2): one `GetText(0, -1)` (the character count of a VTE is not reliable) and the caret.
    /// Answered with `BackendReply::Window` whose `base` is 0, `len` the scalars read and
    /// `caret` the caret when the app answered. A text longer than the reply cap keeps its end
    /// (the cursor line is at the bottom of a screen).
    Screen {
        node: NodeKey,
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

/// Where a run sits relative to the baseline.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum ScriptPosition {
    #[default]
    Baseline,
    Superscript,
    Subscript,
}

/// Reply of [`TextQuery::Format`]: the formatting of the run that holds the offset, in the
/// terms every toolkit's attribute names are mapped to by the connector (the names differ:
/// GTK `style` and Gecko `font-style`, GTK `underline` and Gecko `text-underline-style`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
// Four independent switches of a run's emphasis, as a toolkit reports them.
#[allow(clippy::struct_excessive_bools)]
pub struct TextFormat {
    /// The run, in the app's own offsets.
    pub range: Range<u32>,
    pub family: Option<Arc<str>>,
    /// Points, as the toolkit wrote them.
    pub size: Option<Arc<str>>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub fg: Option<[u8; 3]>,
    pub bg: Option<[u8; 3]>,
    pub lang: Option<Arc<str>>,
    pub position: ScriptPosition,
    /// `left`, `right`, `center`, `fill`.
    pub align: Option<Arc<str>>,
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

/// True for a code point an icon font draws a symbol with (a private-use area): it speaks as
/// nothing, so text made only of these (and of [`is_object_marker`]) says nothing.
pub fn is_icon_glyph(c: char) -> bool {
    matches!(c, '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{FFFFD}' | '\u{100000}'..='\u{10FFFD}')
}

/// True for the embedded-object replacement character (one per non-text child in AT-SPI text).
pub fn is_object_marker(c: char) -> bool {
    c == '\u{FFFC}'
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
