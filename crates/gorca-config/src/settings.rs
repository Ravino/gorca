//! The settings schema (v1): typed values, defaults, ranges, and the runtime overlay.
//!
//! One row of [`SETTINGS`] per key drives unknown-key detection, "did you mean" and the
//! `gorca config defaults` output; the typed [`Settings`] struct is what the rest of gorca reads.

use std::fmt::Write as _;

/// Current schema version (ADR-0005 7.3).
pub const SCHEMA: u32 = 1;

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum Preset {
    #[default]
    Nvda,
    Orca,
}

impl Preset {
    pub const NAMES: &'static str = "\"nvda\" or \"orca\"";

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "nvda" => Some(Preset::Nvda),
            "orca" => Some(Preset::Orca),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Preset::Nvda => "nvda",
            Preset::Orca => "orca",
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum Layout {
    #[default]
    Desktop,
    Laptop,
}

impl Layout {
    pub const NAMES: &'static str = "\"desktop\" or \"laptop\"";

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "desktop" => Some(Layout::Desktop),
            "laptop" => Some(Layout::Laptop),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Layout::Desktop => "desktop",
            Layout::Laptop => "laptop",
        }
    }
}

/// K-3 policy: which key presses cancel speech.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum InterruptPolicy {
    #[default]
    NonModifier,
    Never,
}

impl InterruptPolicy {
    pub const NAMES: &'static str = "\"non-modifier\" or \"never\"";

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "non-modifier" => Some(InterruptPolicy::NonModifier),
            "never" => Some(InterruptPolicy::Never),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            InterruptPolicy::NonModifier => "non-modifier",
            InterruptPolicy::Never => "never",
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum Verbosity {
    Brief,
    #[default]
    Normal,
    Verbose,
}

impl Verbosity {
    pub const NAMES: &'static str = "\"brief\", \"normal\" or \"verbose\"";

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "brief" => Some(Verbosity::Brief),
            "normal" => Some(Verbosity::Normal),
            "verbose" => Some(Verbosity::Verbose),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Verbosity::Brief => "brief",
            Verbosity::Normal => "normal",
            Verbosity::Verbose => "verbose",
        }
    }

    /// The next level in the cycle brief, normal, verbose.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Verbosity::Brief => Verbosity::Normal,
            Verbosity::Normal => Verbosity::Verbose,
            Verbosity::Verbose => Verbosity::Brief,
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum Confirmations {
    #[default]
    Full,
    Brief,
}

impl Confirmations {
    pub const NAMES: &'static str = "\"full\" or \"brief\"";

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "full" => Some(Confirmations::Full),
            "brief" => Some(Confirmations::Brief),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Confirmations::Full => "full",
            Confirmations::Brief => "brief",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
// Two independent boolean switches that are separate settings in the file; a state enum
// would not be clearer.
#[allow(clippy::struct_excessive_bools)]
pub struct KeyboardSettings {
    pub preset: Preset,
    pub layout: Layout,
    /// Key names of the gorca modifier: `Insert`, `KP_Insert`, `Caps_Lock`.
    pub modifiers: Vec<String>,
    pub caps_lock_layout_guard: bool,
    pub multi_press_ms: u32,
    pub double_tap_guard_ms: u32,
    pub interrupt: InterruptPolicy,
    pub stop_speech_on_control: bool,
    pub pause_speech_on_shift: bool,
    /// KC-4: start from an empty keymap instead of the preset.
    pub replace_preset: bool,
}

impl Default for KeyboardSettings {
    fn default() -> Self {
        Self {
            preset: Preset::Nvda,
            layout: Layout::Desktop,
            modifiers: ["Insert", "KP_Insert", "Caps_Lock"]
                .map(str::to_owned)
                .to_vec(),
            caps_lock_layout_guard: true,
            multi_press_ms: 500,
            double_tap_guard_ms: 15,
            interrupt: InterruptPolicy::NonModifier,
            stop_speech_on_control: true,
            pause_speech_on_shift: true,
            replace_preset: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpeechSettings {
    pub rate: u8,
    pub pitch: u8,
    pub volume: u8,
    pub rate_step: u8,
    pub voice: String,
}

impl Default for SpeechSettings {
    fn default() -> Self {
        Self {
            rate: 50,
            pitch: 50,
            volume: 100,
            rate_step: 5,
            voice: String::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EchoSettings {
    pub typed_characters: bool,
    pub typed_words: bool,
    pub command_keys: bool,
}

impl Default for EchoSettings {
    fn default() -> Self {
        Self {
            typed_characters: true,
            typed_words: false,
            command_keys: false,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VerbositySettings {
    pub level: Verbosity,
    pub confirmations: Confirmations,
}

/// Validated settings: the file layered over the defaults, and (when it is the effective
/// snapshot) the runtime overlay on top.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Settings {
    pub keyboard: KeyboardSettings,
    pub speech: SpeechSettings,
    pub echo: EchoSettings,
    pub verbosity: VerbositySettings,
}

/// Runtime changes made by commands. Never written to disk except by `settings.save` (Q-1).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Overlay {
    pub rate: Option<u8>,
    pub pitch: Option<u8>,
    pub volume: Option<u8>,
    pub typed_characters: Option<bool>,
    pub typed_words: Option<bool>,
    pub command_keys: Option<bool>,
    pub verbosity: Option<Verbosity>,
}

impl Overlay {
    pub fn is_empty(&self) -> bool {
        *self == Overlay::default()
    }

    /// `base` with the overlay's entries applied.
    #[must_use]
    pub fn apply(&self, base: &Settings) -> Settings {
        let mut s = base.clone();
        if let Some(v) = self.rate {
            s.speech.rate = v;
        }
        if let Some(v) = self.pitch {
            s.speech.pitch = v;
        }
        if let Some(v) = self.volume {
            s.speech.volume = v;
        }
        if let Some(v) = self.typed_characters {
            s.echo.typed_characters = v;
        }
        if let Some(v) = self.typed_words {
            s.echo.typed_words = v;
        }
        if let Some(v) = self.command_keys {
            s.echo.command_keys = v;
        }
        if let Some(v) = self.verbosity {
            s.verbosity.level = v;
        }
        s
    }

    /// HR-4: after a file reload, drop the overlay entries for keys whose file value changed
    /// (the user's edit wins); the others survive.
    pub fn drop_changed(&mut self, old: &Settings, new: &Settings) {
        fn keep<T: PartialEq>(slot: &mut Option<T>, old: &T, new: &T) {
            if old != new {
                *slot = None;
            }
        }
        keep(&mut self.rate, &old.speech.rate, &new.speech.rate);
        keep(&mut self.pitch, &old.speech.pitch, &new.speech.pitch);
        keep(&mut self.volume, &old.speech.volume, &new.speech.volume);
        keep(
            &mut self.typed_characters,
            &old.echo.typed_characters,
            &new.echo.typed_characters,
        );
        keep(
            &mut self.typed_words,
            &old.echo.typed_words,
            &new.echo.typed_words,
        );
        keep(
            &mut self.command_keys,
            &old.echo.command_keys,
            &new.echo.command_keys,
        );
        keep(
            &mut self.verbosity,
            &old.verbosity.level,
            &new.verbosity.level,
        );
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SettingType {
    Bool,
    Int { min: i64, max: i64 },
    Enum(&'static str),
    Str,
    StrList,
}

/// Whether a change takes effect while gorca runs (HR-6).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Apply {
    Live,
    Restart,
}

#[derive(Copy, Clone, Debug)]
pub struct SettingDef {
    pub section: &'static str,
    pub key: &'static str,
    pub ty: SettingType,
    /// The default as TOML text.
    pub default: &'static str,
    pub since: u32,
    pub apply: Apply,
}

impl SettingDef {
    pub fn path(&self) -> String {
        format!("{}.{}", self.section, self.key)
    }
}

const fn row(
    section: &'static str,
    key: &'static str,
    ty: SettingType,
    default: &'static str,
) -> SettingDef {
    SettingDef {
        section,
        key,
        ty,
        default,
        since: 1,
        apply: Apply::Live,
    }
}

/// One row per key (ADR-0005 7.1).
pub static SETTINGS: &[SettingDef] = &[
    row(
        "keyboard",
        "preset",
        SettingType::Enum("\"nvda\" or \"orca\""),
        "\"nvda\"",
    ),
    row(
        "keyboard",
        "layout",
        SettingType::Enum("\"desktop\" or \"laptop\""),
        "\"desktop\"",
    ),
    row(
        "keyboard",
        "modifiers",
        SettingType::StrList,
        "[\"Insert\", \"KP_Insert\", \"Caps_Lock\"]",
    ),
    row(
        "keyboard",
        "caps_lock_layout_guard",
        SettingType::Bool,
        "true",
    ),
    row(
        "keyboard",
        "multi_press_ms",
        SettingType::Int {
            min: 100,
            max: 2000,
        },
        "500",
    ),
    row(
        "keyboard",
        "double_tap_guard_ms",
        SettingType::Int { min: 0, max: 100 },
        "15",
    ),
    row(
        "keyboard",
        "interrupt",
        SettingType::Enum("\"non-modifier\" or \"never\""),
        "\"non-modifier\"",
    ),
    row(
        "keyboard",
        "stop_speech_on_control",
        SettingType::Bool,
        "true",
    ),
    row(
        "keyboard",
        "pause_speech_on_shift",
        SettingType::Bool,
        "true",
    ),
    row("keymap", "replace_preset", SettingType::Bool, "false"),
    row(
        "speech",
        "rate",
        SettingType::Int { min: 0, max: 100 },
        "50",
    ),
    row(
        "speech",
        "pitch",
        SettingType::Int { min: 0, max: 100 },
        "50",
    ),
    row(
        "speech",
        "volume",
        SettingType::Int { min: 0, max: 100 },
        "100",
    ),
    row(
        "speech",
        "rate_step",
        SettingType::Int { min: 1, max: 25 },
        "5",
    ),
    row("speech", "voice", SettingType::Str, "\"\""),
    row("echo", "typed_characters", SettingType::Bool, "true"),
    row("echo", "typed_words", SettingType::Bool, "false"),
    row("echo", "command_keys", SettingType::Bool, "false"),
    row(
        "verbosity",
        "level",
        SettingType::Enum("\"brief\", \"normal\" or \"verbose\""),
        "\"normal\"",
    ),
    row(
        "verbosity",
        "confirmations",
        SettingType::Enum("\"full\" or \"brief\""),
        "\"full\"",
    ),
];

/// Section names of the schema, for unknown-section detection.
pub fn sections() -> impl Iterator<Item = &'static str> {
    let mut last = "";
    SETTINGS.iter().filter_map(move |d| {
        if d.section == last {
            None
        } else {
            last = d.section;
            Some(d.section)
        }
    })
}

/// The full default configuration as TOML text (`gorca config defaults`).
pub fn defaults_toml() -> String {
    let mut out = String::new();
    let _ = writeln!(out, "schema = {SCHEMA}");
    let mut section = "";
    for d in SETTINGS {
        if d.section != section {
            section = d.section;
            let _ = write!(out, "\n[{section}]\n");
        }
        let _ = writeln!(out, "{} = {}", d.key, d.default);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_adr() {
        let s = Settings::default();
        assert_eq!(s.speech.rate, 50);
        assert_eq!(s.speech.volume, 100);
        assert!(s.echo.typed_characters);
        assert!(!s.echo.typed_words);
        assert_eq!(s.keyboard.modifiers, ["Insert", "KP_Insert", "Caps_Lock"]);
        assert_eq!(s.keyboard.multi_press_ms, 500);
    }

    #[test]
    fn overlay_applies_and_drops_changed_keys() {
        let base = Settings::default();
        let o = Overlay {
            rate: Some(60),
            typed_words: Some(true),
            ..Overlay::default()
        };
        let eff = o.apply(&base);
        assert_eq!(eff.speech.rate, 60);
        assert!(eff.echo.typed_words);

        // The file edit changes the rate only: the rate entry goes, the toggle stays (HR-4).
        let mut new = base.clone();
        new.speech.rate = 40;
        let mut o2 = o.clone();
        o2.drop_changed(&base, &new);
        assert_eq!(o2.rate, None);
        assert_eq!(o2.typed_words, Some(true));
        assert!(Overlay::default().is_empty());
        assert!(!o2.is_empty());
    }

    #[test]
    fn defaults_table_lists_every_section_once() {
        let s: Vec<_> = sections().collect();
        assert_eq!(s, ["keyboard", "keymap", "speech", "echo", "verbosity"]);
        let text = defaults_toml();
        assert!(text.starts_with("schema = 1\n"));
        assert!(text.contains("[speech]\nrate = 50\n"));
    }

    #[test]
    fn verbosity_cycles() {
        assert_eq!(Verbosity::Brief.next(), Verbosity::Normal);
        assert_eq!(Verbosity::Verbose.next(), Verbosity::Brief);
    }
}
