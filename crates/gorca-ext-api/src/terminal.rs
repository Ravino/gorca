//! Terminal extension interface (ADR-0001 section 5.6 T-2, ADR-0008 tier W3, milestone M6).
//!
//! The terminal presenter is product policy: what counts as new output, where the prompt ends,
//! which change a key explains, when a screen is a repaint. The host (the core's terminal arm)
//! owns everything that touches an application: it feeds the extension plain data and executes
//! the plain actions it answers with. The extension owns no clock, no IPC and no thread, so the
//! same crate compiles natively (linked into the core) and as a WASM component (`wit/gorca-ext.wit`,
//! interface `terminal`; M5 adds it to the world).
//!
//! The contract is one pure function per terminal, [`TerminalHooks::step`]:
//!
//! * time is a monotonic millisecond count the host supplies with every call;
//! * input is one [`TermInput`] (a key, a `text-changed`, a caret move, a screen the host fetched,
//!   a timer tick, a focus change);
//! * output is a list of [`TermAction`]s (speak, fetch the screen, arm a timer, claim the key).
//!
//! Text offsets are Unicode scalars of the snapshot VTE exposes (the visible screen only, rows
//! joined by `'\n'`). Everything a terminal application sends is untrusted: an implementation
//! never panics on any input and bounds every buffer it keeps.

/// Monotonic milliseconds since an origin the host chooses. Only differences matter.
pub type Ms = u64;

/// A key press that reaches the terminal, as the host classified it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TermKey {
    /// A printable character, a space included. Combined with Control or Alt it is `Other`.
    Typed(char),
    Enter,
    Backspace,
    Delete,
    Tab,
    /// Left / Ctrl+B.
    CharLeft,
    /// Right / Ctrl+F.
    CharRight,
    /// Ctrl+Left / Alt+B.
    WordLeft,
    /// Ctrl+Right / Alt+F.
    WordRight,
    /// Home / Ctrl+A.
    LineStart,
    /// End / Ctrl+E.
    LineEnd,
    /// Up / Ctrl+P: shell history, or a vertical move in a full-screen program.
    LineUp,
    /// Down / Ctrl+N.
    LineDown,
    /// Ctrl+C.
    Interrupt,
    /// Anything else that reaches the application (Ctrl+R, Ctrl+L, Escape, Page keys, F-keys).
    Other,
}

/// One `text-changed` of the terminal, as the host decoded it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TermEdit {
    /// `offset`/`len` in scalars; `text` when the payload arrived and has `len` scalars.
    Insert {
        offset: u32,
        len: u32,
        text: Option<String>,
    },
    Delete {
        offset: u32,
        len: u32,
    },
    /// The ingress collapsed a delete+insert pair or a flood (T-1): the screen must be read again.
    Replaced,
}

/// What the host tells the extension.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TermInput {
    Key(TermKey),
    Edit(TermEdit),
    /// `text-caret-moved`.
    Caret(u32),
    /// The reply to [`TermAction::FetchScreen`]: the whole visible screen and the caret.
    Screen {
        text: String,
        caret: Option<u32>,
    },
    /// The fetch failed or timed out.
    ScreenFailed,
    /// A timer armed with [`TermAction::ArmTimer`] (or any other wake-up) fired.
    Tick,
    /// The terminal became the focused object: forget the screen and read it again.
    Focus,
    /// The terminal lost focus or went away: drop everything, secrets included.
    Blur,
}

/// How a spoken piece is verbalised (punctuation level, spelling of symbols).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Unit {
    Char,
    Word,
    Line,
    Text,
}

/// How urgent a spoken piece is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Urgency {
    /// Answers the key the user just pressed: interrupts, carries that key's epoch.
    Key,
    /// A reading the user asked for or a prompt after focus: queued behind what is speaking.
    Queued,
    /// New program output: queued, superseded by any later key press.
    Output,
}

/// What the extension asks the host to do.
#[derive(Clone, Eq, PartialEq)]
pub enum TermAction {
    Say {
        text: String,
        unit: Unit,
        urgency: Urgency,
        /// The text may be a secret (a password prompt is active): never log it, never repeat it.
        sensitive: bool,
    },
    /// Read the whole screen (one `GetText(0, -1)` and the caret) and answer with
    /// [`TermInput::Screen`] or [`TermInput::ScreenFailed`].
    FetchScreen,
    /// Call [`TerminalHooks::step`] with [`TermInput::Tick`] at this time or soon after.
    ArmTimer(Ms),
    /// The key was handled: the generic key echo stays silent.
    ClaimKey,
}

// The text may be a typed password: only its length is shown.
impl std::fmt::Debug for TermAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TermAction::Say {
                text,
                unit,
                urgency,
                sensitive,
            } => f
                .debug_struct("Say")
                .field("chars", &text.chars().count())
                .field("unit", unit)
                .field("urgency", urgency)
                .field("sensitive", sensitive)
                .finish(),
            TermAction::FetchScreen => f.write_str("FetchScreen"),
            TermAction::ArmTimer(t) => f.debug_tuple("ArmTimer").field(t).finish(),
            TermAction::ClaimKey => f.write_str("ClaimKey"),
        }
    }
}

/// Settings of the terminal policy (WIT record `term-config`). Defaults suit bash and zsh.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TermConfig {
    /// Quiet time after the last `text-changed` before the screen is read (T-1 settle).
    pub settle_ms: u32,
    /// A continuous flood is read at least this often.
    pub max_wait_ms: u32,
    /// Prompt ends: the first of these found in a line ends the prompt (`"$ "`, `"# "`, ...).
    pub prompt_markers: Vec<String>,
    /// Optional regular expression; when it matches a line, its match end is the prompt end
    /// (it wins over the markers). Empty = unused. An invalid expression is ignored.
    pub prompt_regex: String,
    /// Optional regular expression for password prompts; empty = the built-in list.
    pub secret_regex: String,
    /// More new lines than this in one reading is a burst: only its tail is spoken.
    pub burst_lines: u32,
    /// Lines of a burst that are still spoken.
    pub burst_tail: u32,
    /// Longest utterance of program output, in characters.
    pub max_output_chars: u32,
}

impl Default for TermConfig {
    fn default() -> Self {
        Self {
            settle_ms: 30,
            max_wait_ms: 200,
            prompt_markers: ["$ ", "# ", "% ", "> ", "\u{276f} ", "\u{bb} "]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            prompt_regex: String::new(),
            secret_regex: String::new(),
            burst_lines: 40,
            burst_tail: 3,
            max_output_chars: 600,
        }
    }
}

/// Counters an implementation keeps for tests and the e2e gates.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct TermCounters {
    /// Screens read (baseline and settle).
    pub fetches: u64,
    pub fetch_failed: u64,
    /// Settles answered from the locally maintained screen, with no fetch.
    pub settled_local: u64,
    /// A fetched screen differed from the locally maintained one.
    pub resync_mismatch: u64,
    pub output_spoken: u64,
    pub output_lines_spoken: u64,
    /// Lines of program output that were not spoken (burst tail, pacing, repaint).
    pub output_lines_skipped: u64,
    pub repaints: u64,
    pub typed_echoed: u64,
    /// Typed characters that the shell's own echo explained and that were therefore not spoken twice.
    pub typed_explained: u64,
    pub history_read: u64,
    pub secret_prompts: u64,
}

/// The extension's current idea of the screen, for review (M4 line model, ADR-0006 LS-1): no IPC
/// per review key. `generation` changes whenever `text` or `caret` does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TermSnapshot {
    pub text: String,
    pub caret: u32,
    pub generation: u64,
}

/// The terminal extension (WIT interface `terminal`, functions `step` and `snapshot`).
pub trait TerminalHooks {
    /// Replace the settings.
    fn configure(&mut self, cfg: &TermConfig);
    /// One input at `now`; actions are appended to `out`.
    fn step(&mut self, now: Ms, input: TermInput, out: &mut Vec<TermAction>);
    fn counters(&self) -> TermCounters;
    /// The screen as last known (the live one when it is trusted, else the last reading), or
    /// `None` before the first reading.
    fn snapshot(&self) -> Option<TermSnapshot>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spoken_secret_never_shows_in_debug() {
        let a = TermAction::Say {
            text: "hunter2".to_owned(),
            unit: Unit::Char,
            urgency: Urgency::Key,
            sensitive: true,
        };
        let shown = format!("{a:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("chars: 7"), "{shown}");
    }

    #[test]
    fn defaults_follow_the_adr() {
        let c = TermConfig::default();
        assert_eq!(c.settle_ms, 30);
        assert!(c.prompt_markers.iter().any(|m| m == "$ "));
    }
}
