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

/// Which speech server gorca talks to (`[speech] backend`; the `--speech` flag overrides it).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum Backend {
    /// voicego first, speech-dispatcher when voicego is unreachable.
    #[default]
    Auto,
    Voicego,
    Ssip,
}

impl Backend {
    pub const NAMES: &'static str = "\"auto\", \"voicego\" or \"ssip\"";

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Backend::Auto),
            "voicego" => Some(Backend::Voicego),
            "ssip" => Some(Backend::Ssip),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Backend::Auto => "auto",
            Backend::Voicego => "voicego",
            Backend::Ssip => "ssip",
        }
    }
}

/// The speech language: the configured one, else the language of the first set of `LC_ALL`,
/// `LC_MESSAGES`, `LANG` (`ru_RU.UTF-8` gives `ru`), else `en`. `C` and `POSIX` name no
/// language and are skipped. `get` looks up an environment variable.
pub fn resolve_language(configured: &str, get: impl Fn(&str) -> Option<String>) -> String {
    let configured = configured.trim();
    if !configured.is_empty() {
        return configured.to_owned();
    }
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        let Some(v) = get(var) else { continue };
        let base = v.trim().split(['.', '@']).next().unwrap_or("");
        let lang = base.split('_').next().unwrap_or("").to_ascii_lowercase();
        if !lang.is_empty() && lang != "c" && lang != "posix" {
            return lang;
        }
    }
    "en".to_owned()
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
    /// BCP-47 or ISO 639 code. Empty: derived from the user's locale at run time.
    pub language: String,
    /// Speech-dispatcher output module. Empty: the server default.
    pub output_module: String,
    pub backend: Backend,
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
            language: String::new(),
            output_module: String::new(),
            backend: Backend::Auto,
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
    /// Say the description of a control (`aria-describedby`, a tooltip title) after a short
    /// pause, as its own sentence, instead of straight after the other words (M8b).
    pub description_pause: bool,
}

impl Default for BrowseSettings {
    fn default() -> Self {
        Self {
            say_all_on_load: true,
            description_pause: true,
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
    row("speech", "language", SettingType::Str, "\"\""),
    row("speech", "output_module", SettingType::Str, "\"\""),
    row(
        "speech",
        "backend",
        SettingType::Enum(Backend::NAMES),
        "\"auto\"",
    ),
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
    row("browse", "description_pause", SettingType::Bool, "true"),
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

/// One-line English description per key, for `gorca config print-default`. The test
/// `every_setting_has_a_description_and_appears_in_the_commented_defaults` keeps this table in
/// step with [`SETTINGS`].
static DOCS: &[(&str, &str, &str)] = &[
    ("keyboard", "preset", "Key bindings preset."),
    (
        "keyboard",
        "layout",
        "Keyboard layout of the preset (keypad review keys on the desktop layout).",
    ),
    (
        "keyboard",
        "modifiers",
        "Key names that act as the gorca modifier.",
    ),
    (
        "keyboard",
        "caps_lock_layout_guard",
        "Drop Caps_Lock as a modifier when it switches keyboard layouts.",
    ),
    (
        "keyboard",
        "multi_press_ms",
        "Window in milliseconds for double and triple key presses.",
    ),
    (
        "keyboard",
        "double_tap_guard_ms",
        "Ignore a repeat of the same key within this many milliseconds.",
    ),
    ("keyboard", "interrupt", "Which key presses cancel speech."),
    (
        "keyboard",
        "stop_speech_on_control",
        "Control stops speech.",
    ),
    (
        "keyboard",
        "pause_speech_on_shift",
        "Shift pauses and resumes speech.",
    ),
    (
        "keymap",
        "replace_preset",
        "Use only the [keymap.*] tables instead of layering them over the preset.",
    ),
    ("speech", "rate", "Speech rate; 50 is the server default."),
    ("speech", "pitch", "Speech pitch; 50 is the server default."),
    (
        "speech",
        "volume",
        "Speech volume; 100 is the server volume.",
    ),
    (
        "speech",
        "rate_step",
        "How much one rate, pitch or volume command changes the level.",
    ),
    (
        "speech",
        "voice",
        "Synthesis voice name; empty is the server default (SSIP SYNTHESIS_VOICE).",
    ),
    (
        "speech",
        "language",
        "Speech language code such as \"ru\"; empty is derived from LC_ALL/LC_MESSAGES/LANG, else \"en\".",
    ),
    (
        "speech",
        "output_module",
        "Speech-dispatcher output module such as \"espeak-ng\" or \"rhvoice\"; empty is the server default.",
    ),
    (
        "speech",
        "backend",
        "Speech server: voicego with speech-dispatcher fallback (auto), voicego only, or speech-dispatcher only (ssip); the --speech flag wins; needs a restart.",
    ),
    ("speech", "punctuation", "How much punctuation is spoken."),
    ("speech", "caps", "How a capital letter is announced."),
    ("echo", "typed_characters", "Speak each typed character."),
    ("echo", "typed_words", "Speak each typed word."),
    ("echo", "command_keys", "Speak command keys such as Ctrl+S."),
    (
        "echo",
        "inserted_text",
        "Speak text inserted by paste or autocomplete.",
    ),
    (
        "text",
        "say_all_moves_caret",
        "Say all moves the caret as it reads.",
    ),
    (
        "text",
        "caret_edge_ms",
        "Caret events within this many milliseconds of an edit count as part of it.",
    ),
    (
        "text",
        "caret_late_ms",
        "Caret events later than this many milliseconds after a key are not announced as a move.",
    ),
    (
        "browse",
        "say_all_on_load",
        "Read a web page automatically when it finishes loading.",
    ),
    (
        "browse",
        "description_pause",
        "Say the description of a web control after a short pause.",
    ),
    (
        "terminal",
        "settle_ms",
        "Wait this many milliseconds for terminal output to settle before speaking.",
    ),
    (
        "terminal",
        "burst_lines",
        "Output longer than this many lines is summarised instead of read.",
    ),
    (
        "terminal",
        "prompt_markers",
        "Line endings that mark a shell prompt.",
    ),
    (
        "terminal",
        "prompt_regex",
        "Regular expression that matches a prompt; empty uses prompt_markers only.",
    ),
    (
        "terminal",
        "secret_regex",
        "Regular expression for lines never to speak, such as password prompts; empty is none.",
    ),
    ("verbosity", "level", "How much detail is spoken."),
    (
        "verbosity",
        "confirmations",
        "Full or brief confirmations of settings changes.",
    ),
];

/// The description of a key, if it has one.
pub fn describe(section: &str, key: &str) -> Option<&'static str> {
    DOCS.iter()
        .find(|(s, k, _)| *s == section && *k == key)
        .map(|(_, _, d)| *d)
}

fn type_text(t: SettingType) -> String {
    match t {
        SettingType::Bool => "true or false".to_owned(),
        SettingType::Int { min, max } => format!("integer {min} to {max}"),
        SettingType::Enum(names) => format!("one of {names}"),
        SettingType::Str => "string".to_owned(),
        SettingType::StrList => "list of strings".to_owned(),
    }
}

/// The complete configuration with every section and key at its default, one comment line per
/// key giving the description, type and allowed values (`gorca config print-default`). Built
/// from [`SETTINGS`], so it cannot drift from the schema.
pub fn defaults_toml_commented() -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# gorca settings: every key at its default. Delete what you do not change.\n\
         # Keymap overrides go in [keymap.<context>] tables (see docs/config-reference.md).\n\
         schema = {SCHEMA}"
    );
    let mut section = "";
    for d in SETTINGS {
        if d.section != section {
            section = d.section;
            let _ = write!(out, "\n[{section}]\n");
        }
        let what = describe(d.section, d.key).unwrap_or("");
        let _ = writeln!(out, "# {what} ({})", type_text(d.ty));
        let _ = writeln!(out, "{} = {}", d.key, d.default);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_has_a_description_and_appears_in_the_commented_defaults() {
        let text = defaults_toml_commented();
        for d in SETTINGS {
            assert!(
                describe(d.section, d.key).is_some_and(|x| !x.is_empty()),
                "{} has no description",
                d.path()
            );
            assert!(
                text.contains(&format!("\n{} = {}\n", d.key, d.default)),
                "{} is missing",
                d.path()
            );
        }
        for s in sections() {
            assert!(text.contains(&format!("\n[{s}]\n")), "[{s}] is missing");
        }
        assert_eq!(
            DOCS.len(),
            SETTINGS.len(),
            "a description without a setting"
        );
        // The output is valid settings text with no problems.
        let l = crate::load(Some(&text), &crate::Presets::builtin());
        assert!(l.diags.is_empty(), "{:?}", l.diags);
        assert_eq!(l.settings, Settings::default());
    }

    #[test]
    fn language_comes_from_the_setting_then_the_locale_then_english() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(
            resolve_language("de", env(&[("LANG", "ru_RU.UTF-8")])),
            "de"
        );
        assert_eq!(resolve_language("", env(&[("LANG", "ru_RU.UTF-8")])), "ru");
        assert_eq!(
            resolve_language("", env(&[("LC_ALL", "C"), ("LANG", "uk_UA@x")])),
            "uk"
        );
        assert_eq!(
            resolve_language("", env(&[("LC_MESSAGES", "pt_BR"), ("LANG", "ru_RU")])),
            "pt"
        );
        assert_eq!(resolve_language("", env(&[("LANG", "C.UTF-8")])), "en");
        assert_eq!(resolve_language(" ", env(&[])), "en");
    }

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
