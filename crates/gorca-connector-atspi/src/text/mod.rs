// SPDX-License-Identifier: GPL-3.0-or-later
//! Text-interface reads for the Reader's text layer (ADR-0007 sections 4.2 and 6): the window
//! around the caret, one unit at an offset, and the selections with their text.
//!
//! Everything here is app-supplied data and untrusted: offsets are clamped to the text length
//! (T-3), reply sizes are bounded, and an application that sends nonsense gets an error, never a
//! panic. Every call runs under the request deadline. Offsets in and out are the toolkit's
//! (scalars for GTK, St and the fake app); UTF-16 conversion for Gecko happens in the Reader
//! next to the text (TQ-1).

use std::sync::Arc;

use gorca_connector_api::{BackendError, BackendReply};
use gorca_model::{
    MAX_SELECTION_TEXT, NodeKey, TextGranularity, TextQuery, TextSelections, TextUnitAt,
    TextWindowFill,
};
use gorca_rt::Deadline;

mod format;

use crate::decode::clip;
use crate::fetch::{CallCx, Resolved, backend_error};

/// A reply that is app text is cut here (a hostile app must not make us copy megabytes).
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
/// The most bytes of a terminal screen kept (a `GetText(0, -1)` reply is refused above 256 KiB
/// anyway; the extension bounds a screen at the same size).
pub const MAX_SCREEN_BYTES: usize = 256 * 1024;
/// Most selection ranges one query reads (`GetSelection(i)`, `i < 8`).
pub const MAX_SELECTIONS: i32 = 8;
/// The most scalars a window query returns, whatever the radius asked for.
pub const MAX_WINDOW_RADIUS: u32 = 8 * 1024;

fn resolve(cx: &CallCx, key: NodeKey) -> Result<Resolved, BackendError> {
    cx.paths
        .get(key)
        .map(|path| Resolved { key, path })
        .ok_or(BackendError::Unresolved)
}

/// An offset or length from the wire as a non-negative scalar count.
fn nonneg(v: i32) -> u32 {
    u32::try_from(v).unwrap_or(0)
}

/// Serve the text queries of this module; `None` for the ones `fetch::review` serves.
pub(crate) async fn run(
    cx: &CallCx,
    q: &TextQuery,
    dl: Deadline,
) -> Option<Result<BackendReply, BackendError>> {
    match q {
        TextQuery::AtOffset { node, offset, unit } => {
            Some(at_offset(cx, *node, *offset, *unit, dl).await)
        }
        TextQuery::Selections { node } => Some(selections(cx, *node, dl).await),
        TextQuery::Format { node, offset } => Some(format::run(cx, *node, *offset, dl).await),
        TextQuery::Window {
            node,
            around,
            radius,
        } => Some(window(cx, *node, *around, *radius, dl).await),
        TextQuery::Screen { node } => Some(screen(cx, *node, dl).await),
        TextQuery::GetText { .. }
        | TextQuery::CaretOffset { .. }
        | TextQuery::Attributes { .. }
        | TextQuery::VisibleLines { .. } => None,
    }
}

async fn char_count(cx: &CallCx, n: &Resolved, dl: Deadline) -> Result<u32, BackendError> {
    cx.caller
        .text_char_count(&n.path, dl)
        .await
        .map(nonneg)
        .map_err(|e| backend_error(&e))
}

/// A terminal's visible screen: `GetText(0, -1)` and the caret. The end offset `-1` means "to the
/// end" for every implementation and never trips VTE's `g_assert(start_offset >= 0)` (T-3);
/// `CharacterCount` is not asked because a VTE reports a different number than the text it serves.
async fn screen(cx: &CallCx, node: NodeKey, dl: Deadline) -> Result<BackendReply, BackendError> {
    let n = resolve(cx, node)?;
    let t = cx
        .caller
        .text_range(&n.path, 0, -1, dl)
        .await
        .map_err(|e| backend_error(&e))?;
    let caret = cx.caller.text_caret(&n.path, dl).await.ok().map(nonneg);
    let (kept, caret) = clip_screen(&t, caret, MAX_SCREEN_BYTES);
    let text: Arc<str> = Arc::from(kept);
    let len = u32::try_from(gorca_cpu::text::utf8_scalar_count(&text)).unwrap_or(u32::MAX);
    Ok(BackendReply::Window(TextWindowFill {
        base: 0,
        text,
        len,
        caret,
    }))
}

/// Keep the last rows of a screen that is longer than `max` bytes and move the caret with them.
///
/// The text starts at a row boundary (a partial first row would be read as a whole row), the
/// caret is a scalar offset into the kept text (`None` when it fell into the dropped head), and
/// nothing is cut when the screen fits. Offsets are scalars, never bytes (VTE's `Text`
/// interface counts characters). `GetText` and `GetCaretOffset` are two calls, so the caret can
/// belong to a slightly newer frame; the settle (T-1) makes that harmless.
fn clip_screen(s: &str, caret: Option<u32>, max: usize) -> (&str, Option<u32>) {
    if s.len() <= max {
        return (s, caret);
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    // Round up to the next row start, unless the cut already fell on one.
    if s.get(..start).is_some_and(|head| !head.ends_with('\n'))
        && let Some(nl) = s.get(start..).and_then(|t| t.find('\n'))
    {
        start += nl + 1;
    }
    let dropped = s.get(..start).map_or(0, gorca_cpu::text::utf8_scalar_count);
    let dropped = u32::try_from(dropped).unwrap_or(u32::MAX);
    let kept = s.get(start..).unwrap_or("");
    (kept, caret.and_then(|c| c.checked_sub(dropped)))
}

/// `GetText` around `around` (or the caret) in one pass: the character count, the caret when
/// asked, and the text of `[around - radius, around + radius]` clamped to the text.
async fn window(
    cx: &CallCx,
    node: NodeKey,
    around: Option<u32>,
    radius: u32,
    dl: Deadline,
) -> Result<BackendReply, BackendError> {
    let n = resolve(cx, node)?;
    let len = char_count(cx, &n, dl).await?;
    let (centre, caret) = if let Some(a) = around {
        (a.min(len), None)
    } else {
        let c = cx
            .caller
            .text_caret(&n.path, dl)
            .await
            .map(nonneg)
            .map_err(|e| backend_error(&e))?
            .min(len);
        (c, Some(c))
    };
    let radius = radius.min(MAX_WINDOW_RADIUS);
    let base = centre.saturating_sub(radius);
    let end = centre.saturating_add(radius).min(len);
    let text = if base < end {
        let t = cx
            .caller
            .text_range(
                &n.path,
                i32::try_from(base).unwrap_or(i32::MAX),
                i32::try_from(end).unwrap_or(i32::MAX),
                dl,
            )
            .await
            .map_err(|e| backend_error(&e))?;
        Arc::from(clip(&t, MAX_TEXT_BYTES))
    } else {
        Arc::from("")
    };
    Ok(BackendReply::Window(TextWindowFill {
        base,
        text,
        len,
        caret,
    }))
}

/// One `GetStringAtOffset`. A range that is inverted or does not contain the offset is the
/// application's mistake; it is reported as a protocol error so the Reader falls back to its
/// window.
async fn at_offset(
    cx: &CallCx,
    node: NodeKey,
    offset: u32,
    unit: TextGranularity,
    dl: Deadline,
) -> Result<BackendReply, BackendError> {
    let n = resolve(cx, node)?;
    let off = i32::try_from(offset).unwrap_or(i32::MAX);
    let (text, start, end) = cx
        .caller
        .string_at(&n.path, off, unit.wire(), dl)
        .await
        .map_err(|e| backend_error(&e))?;
    if start < 0 || end < start {
        return Err(BackendError::Protocol(
            "GetStringAtOffset returned an inverted range".to_owned(),
        ));
    }
    // A clipped text no longer spans its range: shorten the range to what is kept.
    let kept = clip(&text, MAX_TEXT_BYTES);
    let end = if kept.len() < text.len() {
        nonneg(start).saturating_add(u32::try_from(kept.chars().count()).unwrap_or(u32::MAX))
    } else {
        nonneg(end)
    };
    Ok(BackendReply::Unit(TextUnitAt {
        range: nonneg(start)..end,
        text: Arc::from(kept),
    }))
}

/// `GetNSelections` and `GetSelection(i)`, each range with its text (not for ranges longer than
/// [`MAX_SELECTION_TEXT`]: the Reader then only counts them).
async fn selections(
    cx: &CallCx,
    node: NodeKey,
    dl: Deadline,
) -> Result<BackendReply, BackendError> {
    let n = resolve(cx, node)?;
    let count = cx
        .caller
        .n_selections(&n.path, dl)
        .await
        .map_err(|e| backend_error(&e))?
        .clamp(0, MAX_SELECTIONS);
    let mut ranges = Vec::new();
    for i in 0..count {
        let (a, b) = cx
            .caller
            .selection_range(&n.path, i, dl)
            .await
            .map_err(|e| backend_error(&e))?;
        let (s, e) = (nonneg(a.min(b)), nonneg(a.max(b)));
        if s == e {
            continue;
        }
        let text: Arc<str> = if e - s <= MAX_SELECTION_TEXT {
            let t = cx
                .caller
                .text_range(
                    &n.path,
                    i32::try_from(s).unwrap_or(i32::MAX),
                    i32::try_from(e).unwrap_or(i32::MAX),
                    dl,
                )
                .await
                .map_err(|e| backend_error(&e))?;
            Arc::from(clip(&t, MAX_TEXT_BYTES))
        } else {
            Arc::from("")
        };
        ranges.push((s..e, text));
    }
    Ok(BackendReply::Selections(TextSelections { ranges }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_numbers_are_clamped_not_wrapped() {
        assert_eq!(nonneg(-1), 0);
        assert_eq!(nonneg(i32::MIN), 0);
        assert_eq!(nonneg(7), 7);
    }

    /// The caret row of `text` (rows split on `\n`) and the scalar column.
    fn row_col(text: &str, caret: u32) -> (usize, usize) {
        let head: String = text.chars().take(caret as usize).collect();
        let row = head.matches('\n').count();
        (
            row,
            head.rsplit('\n').next().map_or(0, |l| l.chars().count()),
        )
    }

    fn screen(rows: usize, cols: usize, fill: char) -> String {
        let mut s = String::new();
        for r in 0..rows {
            let tag = format!("row {r:02} ");
            s.push_str(&tag);
            s.extend(std::iter::repeat_n(fill, cols - tag.chars().count()));
            s.push('\n');
        }
        s
    }

    #[test]
    fn a_screen_that_fits_is_untouched() {
        let s = screen(60, 200, '.');
        assert!(s.len() < MAX_SCREEN_BYTES);
        assert_eq!(
            clip_screen(&s, Some(7), MAX_SCREEN_BYTES),
            (s.as_str(), Some(7))
        );
    }

    #[test]
    fn a_clipped_screen_starts_on_a_row_and_the_caret_moves_with_it() {
        // Reviewer probe (m6-review finding 1): 40x100, caret on row 10, cut at 2 KiB.
        let s = screen(40, 100, '.');
        let caret = u32::try_from(25 * 101 + 4).unwrap_or(0);
        let (kept, c) = clip_screen(&s, Some(caret), 2048);
        assert!(kept.len() <= 2048 && kept.starts_with("row "));
        let c = c.unwrap_or(u32::MAX);
        let (row, col) = row_col(kept, c);
        let line = kept.lines().nth(row).unwrap_or("");
        assert!(line.starts_with("row 25 "), "{line}");
        assert_eq!(col, 4);
        // A caret in the dropped head has no place in the kept text.
        assert_eq!(clip_screen(&s, Some(10), 2048).1, None);
        assert_eq!(clip_screen(&s, None, 2048).1, None);
    }

    #[test]
    fn offsets_count_scalars_not_bytes_for_multibyte_rows() {
        // Cyrillic is two bytes per scalar: a byte count would drop the caret far too early.
        let s = screen(60, 100, '\u{44f}');
        let caret = u32::try_from(40 * 101 + 9).unwrap_or(0);
        let (kept, c) = clip_screen(&s, Some(caret), 4096);
        assert!(kept.len() <= 4096);
        let (row, col) = row_col(kept, c.unwrap_or(u32::MAX));
        assert!(kept.lines().nth(row).unwrap_or("").starts_with("row 40 "));
        assert_eq!(col, 9);
        // Never inside a scalar, even for a cut that lands mid-character.
        for max in 1500..1510 {
            let (k, _) = clip_screen(&s, None, max);
            assert!(k.is_empty() || k.starts_with("row "));
        }
    }

    #[test]
    fn granularity_wire_values_are_the_atspi_ones() {
        assert_eq!(TextGranularity::Char.wire(), 0);
        assert_eq!(TextGranularity::Word.wire(), 1);
        assert_eq!(TextGranularity::Sentence.wire(), 2);
        assert_eq!(TextGranularity::Line.wire(), 3);
        assert_eq!(TextGranularity::Paragraph.wire(), 4);
    }
}
