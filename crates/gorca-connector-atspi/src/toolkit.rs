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
    ///
    /// Chromium (M9, found by FE2 on Chrome for Testing 153 with `gorca_text=debug`): in 24 of
    /// 446 inserts into an `<input>` (5.4 %) the named offset is 1 or 2 past the caret event that
    /// follows (`docs/reports/m9-baseline.md` section 4), and the doubled echoes of the failing
    /// run are exactly those keys; the same one-key tolerance as Gecko absorbs them.
    pub fn text_insert_slack(self) -> u32 {
        match self {
            Toolkit::Firefox | Toolkit::Chromium => 2,
            Toolkit::Gtk3 | Toolkit::Gtk4 | Toolkit::Qt | Toolkit::Vte | Toolkit::Other => 0,
        }
    }
}

impl Toolkit {
    /// Quirk row `blink-wake-call` (M9, `docs/reports/m9-baseline.md` section 2): Chromium builds
    /// the accessibility tree of web content only after an AT used `GetAttributes` or
    /// `GetRelationSet` on one of its objects (`AtkGetAttributes` / `AtkRefRelationSet` in
    /// `ui/accessibility/platform/ax_platform_node_auralinux.cc`); `IsEnabled` alone leaves the
    /// application with its window frames and no document. The connector sends one such call on
    /// the application root when it first sees an application of this toolkit.
    pub fn needs_wake_call(self) -> bool {
        matches!(self, Toolkit::Chromium)
    }

    /// Quirk row `blink-static-leaves` (M9): Chromium exposes every text run as a `static` child
    /// whose own `Text` repeats the text the parent's hypertext already holds (and the parent has
    /// no U+FFFC for it), so a text-leaf rebuild that appends children after the parent's runs
    /// reads every heading, link and label twice. Gecko keeps text only in the container.
    pub fn text_leaf_children_repeat_parent_text(self) -> bool {
        matches!(self, Toolkit::Chromium)
    }

    /// Quirk row `blink-name-from` (M9): the object attribute that says where an accessible name
    /// came from (`contents`, `attribute`, `relatedElement`, ...). A name that came from content
    /// repeats the text of the node and is not spoken separately. Gecko has no such attribute
    /// (it marks author-provided names with `explicit-name=true`).
    pub fn name_source_attribute(self) -> Option<&'static str> {
        match self {
            Toolkit::Chromium => Some("name-from"),
            Toolkit::Firefox
            | Toolkit::Gtk3
            | Toolkit::Gtk4
            | Toolkit::Qt
            | Toolkit::Vte
            | Toolkit::Other => None,
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
    fn only_the_browser_engines_have_the_insert_offset_quirk() {
        assert_eq!(Toolkit::Firefox.text_insert_slack(), 2);
        assert_eq!(Toolkit::Chromium.text_insert_slack(), 2);
        for t in [
            Toolkit::Gtk3,
            Toolkit::Gtk4,
            Toolkit::Qt,
            Toolkit::Vte,
            Toolkit::Other,
        ] {
            assert_eq!(t.text_insert_slack(), 0);
        }
    }

    #[test]
    fn blink_rows_belong_to_chromium_only() {
        for t in [
            Toolkit::Firefox,
            Toolkit::Gtk3,
            Toolkit::Gtk4,
            Toolkit::Qt,
            Toolkit::Vte,
            Toolkit::Other,
        ] {
            assert!(!t.needs_wake_call());
            assert!(!t.text_leaf_children_repeat_parent_text());
            assert_eq!(t.name_source_attribute(), None);
        }
        assert!(Toolkit::Chromium.needs_wake_call());
        assert!(Toolkit::Chromium.text_leaf_children_repeat_parent_text());
        assert_eq!(Toolkit::Chromium.name_source_attribute(), Some("name-from"));
        // Chrome and Electron classify as Chromium, so they get the rows.
        assert!(Toolkit::classify("", "Google Chrome for Testing").needs_wake_call());
        assert!(Toolkit::classify("Chromium", "slack").needs_wake_call());
    }
}
