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

/// How much punctuation is spoken (ADR-0007 VB-1).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum Punct {
    None,
    #[default]
    Some,
    Most,
    All,
}

impl Punct {
    pub const NAMES: &'static str = "\"none\", \"some\", \"most\" or \"all\"";

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Punct::None),
            "some" => Some(Punct::Some),
            "most" => Some(Punct::Most),
            "all" => Some(Punct::All),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Punct::None => "none",
            Punct::Some => "some",
            Punct::Most => "most",
            Punct::All => "all",
        }
    }

    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Punct::None => Punct::Some,
            Punct::Some => Punct::Most,
            Punct::Most => Punct::All,
            Punct::All => Punct::None,
        }
    }
}

/// How a capital letter is announced (ADR-0007 VB-1).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum Caps {
    /// A higher pitch (a server that cannot do that says "cap").
    #[default]
    Pitch,
    /// The word "cap" before the letter.
    Say,
    Off,
}

impl Caps {
    pub const NAMES: &'static str = "\"pitch\", \"say\" or \"off\"";

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pitch" => Some(Caps::Pitch),
            "say" => Some(Caps::Say),
            "off" => Some(Caps::Off),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Caps::Pitch => "pitch",
            Caps::Say => "say",
            Caps::Off => "off",
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
    pub punctuation: Punct,
    pub caps: Caps,
}

impl Default for SpeechSettings {
    fn default() -> Self {
        Self {
            rate: 50,
            pitch: 50,
            volume: 100,
            rate_step: 5,
            voice: String::new(),
            punctuation: Punct::Some,
            caps: Caps::Pitch,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
// Four independent switches that are separate settings in the file.
#[allow(clippy::struct_excessive_bools)]
pub struct EchoSettings {
    pub typed_characters: bool,
    pub typed_words: bool,
    pub command_keys: bool,
    /// Text inserted without a key press (paste, input method, script) is spoken (ADR-0007 TE-3).
    pub inserted_text: bool,
}

impl Default for EchoSettings {
    fn default() -> Self {
        Self {
            typed_characters: true,
            typed_words: false,
            command_keys: false,
            inserted_text: true,
        }
    }
}

/// Caret and text presentation (ADR-0007 section 12).
#[derive(Clone, Debug, Eq, PartialEq)]
// One switch and two durations; a state enum would not be clearer.
#[allow(clippy::struct_excessive_bools)]
pub struct TextSettings {
    /// Say-all leaves the caret where it stopped (a `SetCaret` when it ends).
    pub say_all_moves_caret: bool,
    /// A movement key with no caret event after this long did not move the caret (CR-3).
    pub caret_edge_ms: u32,
    /// A caret event up to this long after its key still belongs to it (CR-3b).
    pub caret_late_ms: u32,
}

impl Default for TextSettings {
    fn default() -> Self {
        Self {
            say_all_moves_caret: true,
            caret_edge_ms: 100,
            caret_late_ms: 400,
        }
    }
}

/// Browse mode (docs/design/browse-mode.md, Q-W2).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowseSettings {
    /// Read a page from its top as soon as it has loaded (NVDA `autoSayAllOnPageLoad`, on by
    /// default).
    pub say_all_on_load: bool,
}

impl Default for BrowseSettings {
    fn default() -> Self {
        Self {
            say_all_on_load: true,
        }
    }
}

/// Terminal presentation (milestone M6, `ext-vte`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalSettings {
    /// Quiet time after the last change before the screen is read (ADR-0001 T-1).
    pub settle_ms: u32,
    /// More new lines than this in one reading are a burst: only the last few are spoken.
    pub burst_lines: u32,
    /// Prompt ends: the first of these found in a line ends the prompt.
    pub prompt_markers: Vec<String>,
    /// Regular expression whose match ends the prompt; empty = use the markers only.
    pub prompt_regex: String,
    /// Regular expression for password prompts; empty = the built-in list.
    pub secret_regex: String,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            settle_ms: 30,
            burst_lines: 40,
            prompt_markers: ["$ ", "# ", "% ", "> ", "\u{276f} ", "\u{bb} "]
                .map(str::to_owned)
                .to_vec(),
            prompt_regex: String::new(),
            secret_regex: String::new(),
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
    pub text: TextSettings,
    pub browse: BrowseSettings,
    pub terminal: TerminalSettings,
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
    pub punctuation: Option<Punct>,
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
        if let Some(v) = self.punctuation {
            s.speech.punctuation = v;
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
        keep(
            &mut self.punctuation,
            &old.speech.punctuation,
            &new.speech.punctuation,
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
    row(
        "speech",
        "punctuation",
        SettingType::Enum("\"none\", \"some\", \"most\" or \"all\""),
        "\"some\"",
    ),
    row(
        "speech",
        "caps",
        SettingType::Enum("\"pitch\", \"say\" or \"off\""),
        "\"pitch\"",
    ),
    row("echo", "typed_characters", SettingType::Bool, "true"),
    row("echo", "typed_words", SettingType::Bool, "false"),
    row("echo", "command_keys", SettingType::Bool, "false"),
    row("echo", "inserted_text", SettingType::Bool, "true"),
    row("text", "say_all_moves_caret", SettingType::Bool, "true"),
    row(
        "text",
        "caret_edge_ms",
        SettingType::Int { min: 20, max: 1000 },
        "100",
    ),
    row(
        "text",
        "caret_late_ms",
        SettingType::Int {
            min: 100,
            max: 2000,
        },
        "400",
    ),
    row("browse", "say_all_on_load", SettingType::Bool, "true"),
    row(
        "terminal",
        "settle_ms",
        SettingType::Int { min: 10, max: 200 },
        "30",
    ),
    row(
        "terminal",
        "burst_lines",
        SettingType::Int { min: 4, max: 400 },
        "40",
    ),
    row(
        "terminal",
        "prompt_markers",
        SettingType::StrList,
        "[\"$ \", \"# \", \"% \", \"> \", \"\u{276f} \", \"\u{bb} \"]",
    ),
    row("terminal", "prompt_regex", SettingType::Str, "\"\""),
    row("terminal", "secret_regex", SettingType::Str, "\"\""),
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
        assert_eq!(
            s,
            [
                "keyboard",
                "keymap",
                "speech",
                "echo",
                "text",
                "browse",
                "terminal",
                "verbosity"
            ]
        );
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
