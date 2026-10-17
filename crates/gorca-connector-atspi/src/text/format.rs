// SPDX-License-Identifier: GPL-3.0-or-later
//! Character formatting for `info.formatting` (ADR-0007 M3b): the attribute run at an offset,
//! mapped from each toolkit's attribute names to [`TextFormat`].
//!
//! The names are the toolkits' to choose and they differ, so this is the connector's table
//! (ADR-0008 W1), not the core's:
//!
//! | meaning | GTK / ATK | Gecko |
//! |---|---|---|
//! | family | `family-name` | `family-name` |
//! | size | `size` (points) | `size` (points) |
//! | bold | `weight` >= 700 | `weight` >= 700 |
//! | italic | `style` = italic or oblique | `font-style` = italic or oblique |
//! | underline | `underline` != none/false | `text-underline-style` set and not none |
//! | strike | `strikethrough` = true | `text-line-through-style` set and not none |
//! | colours | `fg-color`, `bg-color` as `r,g,b` (8 or 16 bits per channel) | the same, 8 bits |
//! | script | `rise` > 0 / < 0 | `text-position` = super / sub |
//! | alignment | `justification` | `text-align`, `justification` |
//!
//! Everything here is app-supplied: values are length-capped, and a value that does not parse is
//! left out, never an error.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use gorca_connector_api::{BackendError, BackendReply};
use gorca_model::{NodeKey, ScriptPosition, TextFormat};
use gorca_rt::Deadline;

use crate::fetch::{CallCx, backend_error};

/// Attribute values are cut here (a hostile app must not make us copy megabytes).
const MAX_VALUE: usize = 128;

fn short(v: &str) -> Arc<str> {
    match v.char_indices().nth(MAX_VALUE) {
        Some((cut, _)) => Arc::from(&v[..cut]),
        None => Arc::from(v),
    }
}

/// `r,g,b` or `rgb(r,g,b)`; channels above 255 are 16 bits (GTK3).
fn colour(v: &str) -> Option<[u8; 3]> {
    let inner = v
        .trim()
        .strip_prefix("rgb(")
        .map_or(v.trim(), |r| r.trim_end_matches(')'));
    let mut it = inner.split(',').map(|p| p.trim().parse::<u32>().ok());
    let (r, g, b) = (it.next()??, it.next()??, it.next()??);
    if it.next().is_some() {
        return None;
    }
    let wide = r.max(g).max(b) > 255;
    let ch = |c: u32| u8::try_from(if wide { c / 257 } else { c }).unwrap_or(u8::MAX);
    Some([ch(r), ch(g), ch(b)])
}

fn set(attrs: &HashMap<String, String>, key: &str) -> Option<String> {
    attrs
        .get(key)
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !matches!(v.as_str(), "" | "none" | "false" | "0" | "normal"))
}

/// Map one attribute run to the core's terms.
pub fn parse(attrs: &HashMap<String, String>, range: Range<u32>) -> TextFormat {
    let get = |k: &str| attrs.get(k).map(|v| v.trim()).filter(|v| !v.is_empty());
    let weight = get("weight").and_then(|w| w.parse::<u32>().ok());
    let italic = ["style", "font-style"]
        .iter()
        .filter_map(|k| set(attrs, k))
        .any(|v| v == "italic" || v == "oblique");
    let underline = ["underline", "text-underline-style"]
        .iter()
        .any(|k| set(attrs, k).is_some());
    let strike = ["strikethrough", "text-line-through-style"]
        .iter()
        .any(|k| set(attrs, k).is_some());
    let position = match get("text-position") {
        Some("super") => ScriptPosition::Superscript,
        Some("sub") => ScriptPosition::Subscript,
        Some(_) | None => match get("rise").and_then(|r| r.parse::<i32>().ok()) {
            Some(r) if r > 0 => ScriptPosition::Superscript,
            Some(r) if r < 0 => ScriptPosition::Subscript,
            Some(_) | None => ScriptPosition::Baseline,
        },
    };
    let align = get("text-align")
        .or_else(|| get("justification"))
        .map(|a| match a {
            "start" => "left",
            "end" => "right",
            other => other,
        })
        .map(short);
    TextFormat {
        range,
        family: get("family-name").map(short),
        size: get("size").map(short),
        bold: weight.is_some_and(|w| w >= 700) || get("weight") == Some("bold"),
        italic,
        underline,
        strike,
        fg: get("fg-color").and_then(colour),
        bg: get("bg-color").and_then(colour),
        lang: get("language").map(short),
        position,
        align,
    }
}

/// One `GetAttributeRun` at `offset`.
pub(super) async fn run(
    cx: &CallCx,
    node: NodeKey,
    offset: u32,
    dl: Deadline,
) -> Result<BackendReply, BackendError> {
    let n = super::resolve(cx, node)?;
    let (attrs, start, end) = cx
        .caller
        .attribute_run(&n.path, i32::try_from(offset).unwrap_or(i32::MAX), dl)
        .await
        .map_err(|e| backend_error(&e))?;
    let (start, end) = (
        u32::try_from(start.max(0)).unwrap_or(0),
        u32::try_from(end.max(0)).unwrap_or(0),
    );
    // Nonsense from the app is its own mistake, never a panic.
    let range = if end >= start {
        start..end
    } else {
        start..start
    };
    Ok(BackendReply::Format(parse(&attrs, range)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn gecko_plain_text() {
        let f = parse(
            &map(&[
                ("fg-color", "0,0,0"),
                ("text-position", "baseline"),
                ("size", "12"),
                ("family-name", "Noto Sans"),
                ("bg-color", "255,255,255"),
                ("weight", "400"),
                ("font-style", "normal"),
                ("language", "en"),
            ]),
            0..11,
        );
        assert_eq!(f.family.as_deref(), Some("Noto Sans"));
        assert_eq!(f.size.as_deref(), Some("12"));
        assert!(!f.bold && !f.italic && !f.underline && !f.strike);
        assert_eq!(f.fg, Some([0, 0, 0]));
        assert_eq!(f.bg, Some([255, 255, 255]));
        assert_eq!(f.position, ScriptPosition::Baseline);
        assert_eq!(f.range, 0..11);
    }

    #[test]
    fn gecko_styled_text() {
        let f = parse(
            &map(&[
                ("weight", "700"),
                ("font-style", "italic"),
                ("text-underline-style", "solid"),
                ("text-line-through-style", "solid"),
                ("text-position", "super"),
                ("fg-color", "rgb(255, 0, 0)"),
            ]),
            3..9,
        );
        assert!(f.bold && f.italic && f.underline && f.strike);
        assert_eq!(f.position, ScriptPosition::Superscript);
        assert_eq!(f.fg, Some([255, 0, 0]));
    }

    #[test]
    fn gtk_names_and_sixteen_bit_colours() {
        let f = parse(
            &map(&[
                ("style", "italic"),
                ("underline", "single"),
                ("strikethrough", "false"),
                ("fg-color", "65535,0,32896"),
                ("rise", "-3"),
                ("justification", "fill"),
            ]),
            0..1,
        );
        assert!(f.italic && f.underline && !f.strike);
        assert_eq!(f.fg, Some([255, 0, 128]));
        assert_eq!(f.position, ScriptPosition::Subscript);
        assert_eq!(f.align.as_deref(), Some("fill"));
    }

    #[test]
    fn nonsense_is_left_out() {
        let f = parse(
            &map(&[("fg-color", "red"), ("weight", "heavy"), ("size", "")]),
            0..0,
        );
        assert_eq!(f.fg, None);
        assert!(!f.bold);
        assert_eq!(f.size, None);
        let long = "x".repeat(10_000);
        let f = parse(&map(&[("family-name", long.as_str())]), 0..0);
        assert_eq!(f.family.map(|s| s.chars().count()), Some(MAX_VALUE));
    }
}
