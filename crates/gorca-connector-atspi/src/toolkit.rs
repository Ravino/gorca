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
    /// (`refs/firefox/accessible/atk/DOMtoATK.h`). Chromium: code points. VTE: the `Text`
    /// offsets (caret, `GetText`, `text-changed`) are characters of the screen snapshot; only
    /// its attribute runs are UTF-8 bytes (`docs/platform/vte-terminal-accessibility.md`), and
    /// gorca never reads attributes, so the table says scalar. The rest are scalar values too.
    pub fn text_unit(self) -> TextUnit {
        match self {
            Toolkit::Firefox => TextUnit::Utf16Bom,
            Toolkit::Chromium
            | Toolkit::Vte
            | Toolkit::Gtk3
            | Toolkit::Gtk4
            | Toolkit::Qt
            | Toolkit::Other => TextUnit::Scalar,
        }
    }
}

impl Toolkit {
    /// Quirk row `gecko-insert-offset`: Gecko now and then names a text-inserted offset other
    /// than where the text went, followed by a caret event that moves by exactly the insert:
    /// one or two units after the caret for scripted or committed text (13 of 461 and 24 of
    /// 1050 inserts in Firefox inputs, none in a contenteditable), and up to four off for a
    /// typed key's own insert in an input holding astral characters (found by FE2). The core
    /// folds that caret move into the edit when the slack is non-zero. Every other engine
    /// reports the real offset.
    pub fn text_insert_slack(self) -> u32 {
        match self {
            Toolkit::Firefox => 2,
            Toolkit::Chromium
            | Toolkit::Gtk3
            | Toolkit::Gtk4
            | Toolkit::Qt
            | Toolkit::Vte
            | Toolkit::Other => 0,
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
        // The text offsets of a VTE are characters; only its attribute runs are bytes.
        assert_eq!(Toolkit::Vte.text_unit(), TextUnit::Scalar);
        assert_eq!(Toolkit::Gtk4.text_unit(), TextUnit::Scalar);
    }

    #[test]
    fn only_gecko_has_the_insert_offset_quirk() {
        assert_eq!(Toolkit::Firefox.text_insert_slack(), 2);
        for t in [
            Toolkit::Chromium,
            Toolkit::Gtk3,
            Toolkit::Gtk4,
            Toolkit::Qt,
            Toolkit::Vte,
            Toolkit::Other,
        ] {
            assert_eq!(t.text_insert_slack(), 0);
        }
    }
}
