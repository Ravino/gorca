//! Toolkit normalisation keys (ADR-0001 section 5.6). Quirks that more than one consumer needs
//! live in tables keyed by `AppInfo.toolkit`, never in ad-hoc code.

use gorca_model::TextUnit;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Toolkit {
    Gtk3,
    Gtk4,
    Qt,
    Firefox,
    Chromium,
    Vte,
    Other,
}

impl Toolkit {
    /// Classify from the `ToolkitName` and application name AT-SPI reports. Matching is
    /// case-insensitive. Untrusted input: anything unknown is `Other`.
    pub fn classify(toolkit_name: &str, app_name: &str) -> Toolkit {
        let t = toolkit_name.to_ascii_lowercase();
        let a = app_name.to_ascii_lowercase();
        if a.contains("firefox") || a.contains("thunderbird") || t == "gecko" {
            Toolkit::Firefox
        } else if a.contains("chrom") || a.contains("electron") || t == "chromium" {
            Toolkit::Chromium
        } else if a.contains("gnome-terminal") || a.contains("ptyxis") || a.contains("kgx") {
            Toolkit::Vte
        } else if t.starts_with("gtk") && t.chars().find(char::is_ascii_digit) == Some('4') {
            Toolkit::Gtk4
        } else if t.starts_with("gtk") {
            Toolkit::Gtk3
        } else if t.starts_with("qt") {
            Toolkit::Qt
        } else {
            Toolkit::Other
        }
    }

    /// TU-2: the offset unit this toolkit's AT-SPI text interface uses.
    ///
    /// Firefox: UTF-16 units with a phantom U+FEFF after each astral character
    /// (`refs/firefox/accessible/atk/DOMtoATK.h`). Chromium: code points. VTE attribute runs:
    /// UTF-8 bytes. The rest are treated as scalar values.
    pub fn text_unit(self) -> TextUnit {
        match self {
            Toolkit::Firefox => TextUnit::Utf16Bom,
            Toolkit::Vte => TextUnit::Utf8,
            Toolkit::Chromium | Toolkit::Gtk3 | Toolkit::Gtk4 | Toolkit::Qt | Toolkit::Other => {
                TextUnit::Scalar
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert_eq!(Toolkit::classify("gtk", "Firefox"), Toolkit::Firefox);
        assert_eq!(Toolkit::classify("Chromium", "chrome"), Toolkit::Chromium);
        assert_eq!(Toolkit::classify("GTK 4.14", "Settings"), Toolkit::Gtk4);
        assert_eq!(Toolkit::classify("gtk 3.24", "gedit"), Toolkit::Gtk3);
        assert_eq!(Toolkit::classify("Qt 6", "kate"), Toolkit::Qt);
        assert_eq!(Toolkit::classify("", ""), Toolkit::Other);
        assert_eq!(
            Toolkit::classify("gtk", "gnome-terminal-server"),
            Toolkit::Vte
        );
    }

    #[test]
    fn text_units_follow_tu2() {
        assert_eq!(Toolkit::Firefox.text_unit(), TextUnit::Utf16Bom);
        assert_eq!(Toolkit::Chromium.text_unit(), TextUnit::Scalar);
        assert_eq!(Toolkit::Vte.text_unit(), TextUnit::Utf8);
        assert_eq!(Toolkit::Gtk4.text_unit(), TextUnit::Scalar);
    }
}
