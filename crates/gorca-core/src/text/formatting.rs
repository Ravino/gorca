//! `info.formatting`: what is spoken for the formatting of the character under the caret
//! (ADR-0007 M3b). The connector has already mapped the toolkit's attribute names to
//! [`TextFormat`]; this is only wording.

use gorca_model::{ScriptPosition, TextFormat};

/// The sixteen HTML colours plus orange, brown and pink: names a listener knows.
const COLOURS: &[(&str, [u8; 3])] = &[
    ("black", [0, 0, 0]),
    ("white", [255, 255, 255]),
    ("red", [255, 0, 0]),
    ("green", [0, 128, 0]),
    ("blue", [0, 0, 255]),
    ("yellow", [255, 255, 0]),
    ("cyan", [0, 255, 255]),
    ("magenta", [255, 0, 255]),
    ("gray", [128, 128, 128]),
    ("light gray", [192, 192, 192]),
    ("dark red", [128, 0, 0]),
    ("olive", [128, 128, 0]),
    ("navy", [0, 0, 128]),
    ("purple", [128, 0, 128]),
    ("teal", [0, 128, 128]),
    ("lime", [0, 255, 0]),
    ("orange", [255, 165, 0]),
    ("brown", [139, 69, 19]),
    ("pink", [255, 192, 203]),
];

/// The name of the colour nearest to `rgb`.
pub fn colour_name(rgb: [u8; 3]) -> &'static str {
    let dist = |c: &[u8; 3]| -> u32 {
        c.iter()
            .zip(rgb)
            .map(|(a, b)| u32::from(a.abs_diff(b)).pow(2))
            .sum()
    };
    COLOURS
        .iter()
        .min_by_key(|(_, c)| dist(c))
        .map_or("unknown", |(n, _)| *n)
}

/// The words for `f`, most useful first: font, size, emphasis, script, colours, alignment,
/// language. Empty when the toolkit said nothing.
pub fn describe(f: &TextFormat) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(n) = &f.family {
        parts.push(n.to_string());
    }
    if let Some(s) = &f.size {
        parts.push(format!("{s} point"));
    }
    if f.bold {
        parts.push("bold".to_owned());
    }
    if f.italic {
        parts.push("italic".to_owned());
    }
    if f.underline {
        parts.push("underline".to_owned());
    }
    if f.strike {
        parts.push("strikethrough".to_owned());
    }
    match f.position {
        ScriptPosition::Baseline => {}
        ScriptPosition::Superscript => parts.push("superscript".to_owned()),
        ScriptPosition::Subscript => parts.push("subscript".to_owned()),
    }
    match (f.fg, f.bg) {
        (Some(fg), Some(bg)) => parts.push(format!("{} on {}", colour_name(fg), colour_name(bg))),
        (Some(fg), None) => parts.push(format!("{} text", colour_name(fg))),
        (None, Some(bg)) => parts.push(format!("{} background", colour_name(bg))),
        (None, None) => {}
    }
    if let Some(a) = &f.align {
        parts.push(format!("aligned {a}"));
    }
    if let Some(l) = &f.lang {
        parts.push(format!("language {l}"));
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn colours_round_to_a_known_name() {
        assert_eq!(colour_name([0, 0, 0]), "black");
        assert_eq!(colour_name([250, 250, 250]), "white");
        assert_eq!(colour_name([200, 10, 10]), "red");
        assert_eq!(colour_name([10, 10, 140]), "navy");
        assert_eq!(colour_name([255, 140, 0]), "orange");
    }

    #[test]
    fn a_run_is_described_in_a_listeners_order() {
        let f = TextFormat {
            family: Some(Arc::from("Noto Sans")),
            size: Some(Arc::from("12")),
            bold: true,
            italic: true,
            fg: Some([0, 0, 0]),
            bg: Some([255, 255, 255]),
            lang: Some(Arc::from("en")),
            ..TextFormat::default()
        };
        assert_eq!(
            describe(&f),
            "Noto Sans, 12 point, bold, italic, black on white, language en"
        );
        assert_eq!(describe(&TextFormat::default()), "");
    }
}
