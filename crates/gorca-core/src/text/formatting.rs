//! `info.formatting`: what is spoken for the formatting of the character under the caret
//! (ADR-0007 M3b). The connector has already mapped the toolkit's attribute names to
//! [`TextFormat`]; this is only wording.

use gorca_i18n::{MsgId as M, msg, t};
use gorca_model::{ScriptPosition, TextFormat};

/// The sixteen HTML colours plus orange, brown and pink: names a listener knows.
const COLOURS: &[(M, [u8; 3])] = &[
    (M::ColourBlack, [0, 0, 0]),
    (M::ColourWhite, [255, 255, 255]),
    (M::ColourRed, [255, 0, 0]),
    (M::ColourGreen, [0, 128, 0]),
    (M::ColourBlue, [0, 0, 255]),
    (M::ColourYellow, [255, 255, 0]),
    (M::ColourCyan, [0, 255, 255]),
    (M::ColourMagenta, [255, 0, 255]),
    (M::ColourGray, [128, 128, 128]),
    (M::ColourLightGray, [192, 192, 192]),
    (M::ColourDarkRed, [128, 0, 0]),
    (M::ColourOlive, [128, 128, 0]),
    (M::ColourNavy, [0, 0, 128]),
    (M::ColourPurple, [128, 0, 128]),
    (M::ColourTeal, [0, 128, 128]),
    (M::ColourLime, [0, 255, 0]),
    (M::ColourOrange, [255, 165, 0]),
    (M::ColourBrown, [139, 69, 19]),
    (M::ColourPink, [255, 192, 203]),
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
        .map_or(t(M::ColourUnknown), |(n, _)| t(*n))
}

/// The words for `f`, most useful first: font, size, emphasis, script, colours, alignment,
/// language. Empty when the toolkit said nothing.
pub fn describe(f: &TextFormat) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(n) = &f.family {
        parts.push(n.to_string());
    }
    if let Some(s) = &f.size {
        parts.push(msg!(FormatPoint, s = s.to_string().as_str()));
    }
    if f.bold {
        parts.push(t(M::FormatBold).to_owned());
    }
    if f.italic {
        parts.push(t(M::FormatItalic).to_owned());
    }
    if f.underline {
        parts.push(t(M::FormatUnderline).to_owned());
    }
    if f.strike {
        parts.push(t(M::FormatStrike).to_owned());
    }
    match f.position {
        ScriptPosition::Baseline => {}
        ScriptPosition::Superscript => parts.push(t(M::FormatSuperscript).to_owned()),
        ScriptPosition::Subscript => parts.push(t(M::FormatSubscript).to_owned()),
    }
    match (f.fg, f.bg) {
        (Some(fg), Some(bg)) => parts.push(msg!(
            FormatColourOn,
            fg = colour_name(fg),
            bg = colour_name(bg)
        )),
        (Some(fg), None) => parts.push(msg!(FormatTextColour, c = colour_name(fg))),
        (None, Some(bg)) => parts.push(msg!(FormatBgColour, c = colour_name(bg))),
        (None, None) => {}
    }
    if let Some(a) = &f.align {
        let a: &str = match &**a {
            "left" => t(M::FormatAlignLeft),
            "right" => t(M::FormatAlignRight),
            "center" => t(M::FormatAlignCenter),
            "justify" => t(M::FormatAlignJustify),
            other => other,
        };
        parts.push(msg!(FormatAligned, a = a));
    }
    if let Some(l) = &f.lang {
        parts.push(msg!(FormatLanguage, l = &**l));
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
