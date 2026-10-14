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

use crate::decode::clip;
use crate::fetch::{CallCx, Resolved, backend_error};

/// A reply that is app text is cut here (a hostile app must not make us copy megabytes).
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
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
        TextQuery::Window {
            node,
            around,
            radius,
        } => Some(window(cx, *node, *around, *radius, dl).await),
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

    #[test]
    fn granularity_wire_values_are_the_atspi_ones() {
        assert_eq!(TextGranularity::Char.wire(), 0);
        assert_eq!(TextGranularity::Word.wire(), 1);
        assert_eq!(TextGranularity::Sentence.wire(), 2);
        assert_eq!(TextGranularity::Line.wire(), 3);
        assert_eq!(TextGranularity::Paragraph.wire(), 4);
    }
}
