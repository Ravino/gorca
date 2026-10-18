// SPDX-License-Identifier: MIT OR Apache-2.0
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

pub use crate::grid::{Rows, unit_at, word_at};

// A typed key may be a password character and a screen may show one: the `Debug` output of the
// input types never contains text (VP gate: no plaintext in any log).
impl std::fmt::Debug for TermKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            TermKey::Typed(_) => "Typed(..)",
            TermKey::Enter => "Enter",
            TermKey::Backspace => "Backspace",
            TermKey::Delete => "Delete",
            TermKey::Tab => "Tab",
            TermKey::CharLeft => "CharLeft",
            TermKey::CharRight => "CharRight",
            TermKey::WordLeft => "WordLeft",
            TermKey::WordRight => "WordRight",
            TermKey::LineStart => "LineStart",
            TermKey::LineEnd => "LineEnd",
            TermKey::LineUp => "LineUp",
            TermKey::LineDown => "LineDown",
            TermKey::Interrupt => "Interrupt",
            TermKey::Other => "Other",
        };
        f.write_str(name)
    }
}

impl std::fmt::Debug for TermEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TermEdit::Insert { offset, len, text } => f
                .debug_struct("Insert")
                .field("offset", offset)
                .field("len", len)
                .field("payload", &text.is_some())
                .finish(),
            TermEdit::Delete { offset, len } => f
                .debug_struct("Delete")
                .field("offset", offset)
                .field("len", len)
                .finish(),
            TermEdit::Replaced => f.write_str("Replaced"),
        }
    }
}

impl std::fmt::Debug for TermInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TermInput::Key(k) => f.debug_tuple("Key").field(k).finish(),
            TermInput::Raw(k) => f.debug_tuple("Raw").field(k).finish(),
            TermInput::Edit(e) => f.debug_tuple("Edit").field(e).finish(),
            TermInput::Caret(o) => f.debug_tuple("Caret").field(o).finish(),
            TermInput::Screen { text, caret } => f
                .debug_struct("Screen")
                .field("chars", &text.chars().count())
                .field("caret", caret)
                .finish(),
            TermInput::ScreenFailed => f.write_str("ScreenFailed"),
            TermInput::Tick => f.write_str("Tick"),
            TermInput::Focus => f.write_str("Focus"),
            TermInput::Blur => f.write_str("Blur"),
        }
    }
}

/// A key press that reaches the terminal, as the host classified it.
#[derive(Copy, Clone, Eq, PartialEq)]
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
#[derive(Clone, Eq, PartialEq)]
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

/// A key press as the core saw it, unclassified. Which keys are Enter, motion or shell shortcuts
/// is shell policy (readline and emacs bindings), so the extension decides (WIT `raw-key`).
///
/// The Control and Alt shortcuts are matched on the physical key (`phys`) so they work in every
/// keyboard layout. Lock modifiers are already removed.
#[derive(Copy, Clone, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)] // mirrors the WIT record: four independent modifier flags
pub struct RawKey {
    /// X11 keysym of the pressed key in the active layout.
    pub keysym: u32,
    /// The Unicode scalar the key types, 0 when it types none.
    pub unichar: u32,
    /// The lowercase ASCII letter of the physical (layout independent) key, 0 for a key that is
    /// not one of the 26 letter keys.
    pub phys: u8,
    pub shift: bool,
    pub ctrl: bool,
    /// Mod1 / Alt.
    pub alt: bool,
    /// Mod4 / Super.
    pub logo: bool,
}

// A typed key may be a password character: only the modifiers are shown.
impl std::fmt::Debug for RawKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawKey")
            .field("shift", &self.shift)
            .field("ctrl", &self.ctrl)
            .field("alt", &self.alt)
            .field("logo", &self.logo)
            .finish_non_exhaustive()
    }
}

impl RawKey {
    /// Does the key type a character on its own (no Control, Alt or Super held)? The generic
    /// definition of "a typed character"; the extension still decides what the key means.
    pub fn types_a_character(&self) -> bool {
        !(self.ctrl || self.alt || self.logo)
            && self.unichar != 0
            && char::from_u32(self.unichar).is_some_and(|c| !c.is_control())
    }
}

/// What the host tells the extension.
#[derive(Clone, Eq, PartialEq)]
pub enum TermInput {
    Key(TermKey),
    /// A key the extension classifies itself (the host does not know readline).
    Raw(RawKey),
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
    /// A movement key could not move the caret: the host plays the boundary cue.
    Cue,
}

/// What a key that cannot move the caret does (`[editing] boundary`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum BoundaryMode {
    /// Ask the host for the boundary cue ([`TermAction::Cue`]).
    #[default]
    Cue,
    /// Speak the unit at the caret again.
    Repeat,
    /// Nothing.
    Silent,
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
            TermAction::Cue => f.write_str("Cue"),
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
    /// How coarse a word is when the terminal reads one; `None` lets the extension choose
    /// ([`DEFAULT_WORD_LEVEL`]).
    pub word_level: Option<WordLevel>,
    /// What Left, Right, Home and End do when they cannot move the caret.
    pub boundary: BoundaryMode,
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
            word_level: None,
            boundary: BoundaryMode::Cue,
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

// ---- the word a readline word motion speaks -----------------------------------------------------

/// What a word motion (Ctrl+Left / Ctrl+Right in readline) speaks on `line` (one screen row,
/// trailing blanks trimmed), given the caret `from` before the move (when it was on this row) and
/// `at` after it: the word just passed when a forward move ended at the end of a word (the span
/// walked back to its last word with a letter or a digit), otherwise the word starting at the
/// caret; `None` means `blank` (the caret is behind the last character). The returned range is in
/// scalars of `line` and may carry trailing blanks: the caller trims them.
///
/// This is the same model the editing presenter (`gorca-text`, `caret::present_word` with
/// `WordLanding::End`) applies to every other editor, in slice form so that an extension (which
/// links only this crate) needs nothing else. `WORD_MOTION_VECTORS` is run by both crates, so the
/// two cannot drift apart.
pub fn word_motion(
    line: &[char],
    from: Option<u32>,
    at: u32,
    level: WordLevel,
) -> Option<std::ops::Range<usize>> {
    let at = at as usize;
    if let Some(f) = from.map(|f| f as usize).filter(|f| *f < at)
        && at > 0
        && let Some(r) = word_passed(line, f, at, level)
    {
        return Some(r);
    }
    (at < line.len()).then(|| word_range_level(line, at, level))
}

/// The word passed by a forward move from `from` to `at`, if the move ended at the end of one.
fn word_passed(
    line: &[char],
    from: usize,
    at: usize,
    level: WordLevel,
) -> Option<std::ops::Range<usize>> {
    let mut end = at;
    let mut first = true;
    loop {
        if end == 0 || end > line.len() {
            return None;
        }
        let w = word_range_level(line, end - 1, level);
        let kept = line[w.clone()]
            .iter()
            .collect::<String>()
            .trim_end_matches(char::is_whitespace)
            .chars()
            .count();
        let stop = w.start + kept;
        // Only the first word has to END where the caret is; behind it the span is walked back.
        if first && (kept == 0 || stop != at) {
            return None;
        }
        first = false;
        let lettered = line[w.start..stop].iter().any(|c| c.is_alphanumeric());
        if lettered || w.start <= from || w.start == 0 {
            return Some(w.start..stop);
        }
        end = w.start;
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum Class {
    Space,
    Word,
    Punct,
}

fn is_mark(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' || is_mark(c) {
        Class::Word
    } else {
        Class::Punct
    }
}

/// How coarse a word is (the SDK's twin of `gorca_model::WordLevel`; `gorca-text` runs the same
/// [`WORD_LEVEL_VECTORS`] against its own implementation, so the two cannot drift apart).
///
/// - `Fine`: a run of letters, digits and `_` is a word, every other character is a word of its
///   own (`--color=auto` has five stops).
/// - `Medium`: option tokens, identifiers and file names stay whole: the joiners `- . = : @ ~`
///   keep the token characters on both sides together when a letter or digit follows them
///   (`--color=auto`, `foo_bar`, `file.tar.gz`, `user@host`, `10:30`); a path separator is a
///   word of its own (`/`, `usr`, `/`, `local`: the segments of a path are the stops); other
///   punctuation is a word of its own, and so is a joiner with no letter after it (`end.`,
///   `a - b`).
/// - `Coarse`: everything between blanks is one word (Orca-like).
///
/// Every word keeps its trailing blanks, whatever the level.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum WordLevel {
    #[default]
    Fine,
    Medium,
    Coarse,
}

/// What a terminal reads as a word unless the user chose a level (`[editing] word = "auto"`):
/// shell words are option tokens, paths and identifiers, which fine words chop into pieces.
pub const DEFAULT_WORD_LEVEL: WordLevel = WordLevel::Medium;

fn is_joiner(c: char) -> bool {
    matches!(c, '-' | '.' | '=' | ':' | '@' | '~')
}

/// Does `s[p]` continue the cluster that `s[p - 1]` is in? The SDK has no segmentation crate, so
/// this is the part of UAX #29 that matters on a terminal line: combining marks, variation
/// selectors, skin-tone modifiers, the zero-width joiner and the scalar it joins, a pair of
/// regional indicators, and Gecko's phantom U+FEFF. `gorca-text` runs the real algorithm; the
/// shared vectors keep the two together.
fn joins_prev(s: &[char], p: usize) -> bool {
    if p == 0 {
        return false;
    }
    let c = s[p];
    let u = c as u32;
    if is_mark(c)
        || matches!(u, 0x200C | 0x200D | 0xFE00..=0xFE0F | 0x1F3FB..=0x1F3FF | 0xE0100..=0xE01EF | 0xFEFF)
    {
        return true;
    }
    if s[p - 1] == '\u{200D}' && !c.is_whitespace() && u > 0x7F {
        return true;
    }
    if (0x1F1E6..=0x1F1FF).contains(&u) {
        // The second of a pair: an odd number of regional indicators in front, with the
        // cluster start of the run at an even distance.
        let n = s[..p]
            .iter()
            .rev()
            .take_while(|x| (0x1F1E6..=0x1F1FF).contains(&(**x as u32)))
            .count();
        return n % 2 == 1;
    }
    false
}

/// The word containing `s[i]`, with its trailing blanks; leading blanks belong to the word before
/// them (see [`WordLevel`] for what stays together). A word never ends inside a cluster.
pub fn word_range_level(s: &[char], i: usize, level: WordLevel) -> std::ops::Range<usize> {
    if s.is_empty() {
        return 0..0;
    }
    // Everything below works on cluster starts, like `gorca-text`.
    let next = |p: usize| {
        let mut e = p + 1;
        while e < s.len() && joins_prev(s, e) {
            e += 1;
        }
        e
    };
    let prev = |p: usize| {
        let mut q = p - 1;
        while q > 0 && joins_prev(s, q) {
            q -= 1;
        }
        q
    };
    let cls = |p: usize| class(s[p]);
    let word_at = |p: usize| p < s.len() && cls(p) == Class::Word;
    // Are the clusters starting at `a` and at `b = next(a)` in the same run?
    let same = |a: usize, b: usize| -> bool {
        let (ca, cb) = (cls(a), cls(b));
        match level {
            WordLevel::Fine => ca == cb && ca != Class::Punct,
            WordLevel::Coarse => (ca == Class::Space) == (cb == Class::Space),
            WordLevel::Medium => {
                if ca == Class::Space || cb == Class::Space {
                    return ca == cb;
                }
                let (x, y) = (s[a], s[b]);
                if ca == Class::Word && cb == Class::Word {
                    true
                } else if ca == Class::Word && is_joiner(y) {
                    word_at(next(b))
                } else if is_joiner(x) && cb == Class::Word {
                    true
                } else {
                    x == '-'
                        && y == '-'
                        && !(a > 0 && (word_at(prev(a)) || s[prev(a)] == '-'))
                        && word_at(next(b))
                }
            }
        }
    };
    let run_start = |p: usize| {
        let mut q = p;
        while q > 0 && same(prev(q), q) {
            q = prev(q);
        }
        q
    };
    let run_end = |p: usize| {
        let mut e = p;
        loop {
            let n = next(e);
            if n >= s.len() || !same(e, n) {
                return n;
            }
            e = n;
        }
    };
    let mut i = i.min(s.len() - 1);
    while i > 0 && joins_prev(s, i) {
        i -= 1;
    }
    match cls(i) {
        Class::Space => {
            let a = run_start(i);
            let b = run_end(i);
            if a > 0 { run_start(prev(a))..b } else { a..b }
        }
        Class::Word | Class::Punct => {
            let a = run_start(i);
            let mut b = run_end(i);
            while b < s.len() && cls(b) == Class::Space {
                b = next(b);
            }
            a..b
        }
    }
}

/// The word containing `line[col]`, or the one before when `col` is on a blank or behind the end,
/// without its blanks; `None` for a blank line.
pub fn word_at_level(line: &str, col: u32, level: WordLevel) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut i = col as usize;
    if i >= chars.len() || chars[i].is_whitespace() {
        let mut j = i.min(chars.len());
        while j > 0 && chars[j - 1].is_whitespace() {
            j -= 1;
        }
        if j == 0 {
            return None;
        }
        i = j - 1;
    }
    let r = word_range_level(&chars, i, level);
    let w: String = chars[r].iter().collect();
    let w = w.trim();
    (!w.is_empty()).then(|| w.to_owned())
}

/// Segment `line` into the words [`word_range_level`] gives, each with its trailing blanks:
/// the segments tile the line.
pub fn words_of(line: &[char], level: WordLevel) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < line.len() {
        let r = word_range_level(line, i, level);
        if r.end <= i {
            break;
        }
        i = r.end;
        out.push(r);
    }
    out
}

/// Shared test vectors of the word levels: `(line, level, the words joined by "|")`. Each word
/// keeps its trailing blanks, so removing the bars gives back the line. Run by this crate and by
/// `gorca-text` against `seg::word_range_level`, which must agree.
#[doc(hidden)]
pub const WORD_LEVEL_VECTORS: &[(&str, WordLevel, &str)] = &[
    ("--color=auto", WordLevel::Fine, "-|-|color|=|auto"),
    ("--color=auto", WordLevel::Medium, "--color=auto"),
    ("--color=auto", WordLevel::Coarse, "--color=auto"),
    (
        "ls -l --color=auto /usr/local/bin",
        WordLevel::Medium,
        "ls |-l |--color=auto |/|usr|/|local|/|bin",
    ),
    (
        "ls -l --color=auto /usr/local/bin",
        WordLevel::Coarse,
        "ls |-l |--color=auto |/usr/local/bin",
    ),
    ("foo_bar baz", WordLevel::Fine, "foo_bar |baz"),
    ("foo_bar baz", WordLevel::Medium, "foo_bar |baz"),
    ("foo-bar.baz qux", WordLevel::Fine, "foo|-|bar|.|baz |qux"),
    ("foo-bar.baz qux", WordLevel::Medium, "foo-bar.baz |qux"),
    ("foo-bar.baz qux", WordLevel::Coarse, "foo-bar.baz |qux"),
    (
        "tar xzf file.tar.gz",
        WordLevel::Medium,
        "tar |xzf |file.tar.gz",
    ),
    ("user@host:~/dir", WordLevel::Medium, "user@host|:|~|/|dir"),
    (
        "wait... then end.",
        WordLevel::Medium,
        "wait|.|.|. |then |end|.",
    ),
    ("a - b -- c", WordLevel::Medium, "a |- |b |-|- |c"),
    ("x = -1", WordLevel::Medium, "x |= |-1"),
    ("10:30 and 3.14", WordLevel::Medium, "10:30 |and |3.14"),
    ("echo 'hi there'", WordLevel::Medium, "echo |'|hi |there|'"),
    ("echo 'hi there'", WordLevel::Coarse, "echo |'hi |there'"),
    ("  lead", WordLevel::Medium, "  |lead"),
    ("  lead", WordLevel::Coarse, "  |lead"),
    (
        "Привет, как-дела?",
        WordLevel::Fine,
        "Привет|, |как|-|дела|?",
    ),
    (
        "Привет, как-дела?",
        WordLevel::Medium,
        "Привет|, |как-дела|?",
    ),
    ("Привет, как-дела?", WordLevel::Coarse, "Привет, |как-дела?"),
    ("", WordLevel::Medium, ""),
    // Clusters stay whole: a ZWJ sequence, a flag, a skin tone, a combining mark.
    (
        "\u{1F468}\u{200D}\u{1F469} x-y",
        WordLevel::Fine,
        "\u{1F468}\u{200D}\u{1F469} |x|-|y",
    ),
    (
        "\u{1F468}\u{200D}\u{1F469} x-y",
        WordLevel::Medium,
        "\u{1F468}\u{200D}\u{1F469} |x-y",
    ),
    (
        "\u{1F1F7}\u{1F1FA}\u{1F1E9}\u{1F1EA} a",
        WordLevel::Fine,
        "\u{1F1F7}\u{1F1FA}|\u{1F1E9}\u{1F1EA} |a",
    ),
    (
        "\u{1F44D}\u{1F3FD} ok",
        WordLevel::Medium,
        "\u{1F44D}\u{1F3FD} |ok",
    ),
    ("e\u{301}x-y", WordLevel::Medium, "e\u{301}x-y"),
];

/// Shared test vectors of the word model: `(line, caret before, caret after, spoken)`, where an
/// empty `spoken` is `blank`. Run by this crate and, as a dev-dependency check, by `gorca-text`
/// against `present_word`, which must agree.
#[doc(hidden)]
pub const WORD_MOTION_VECTORS: &[(&str, Option<u32>, u32, &str)] = &[
    ("hello, brave world", Some(0), 5, "hello"),
    ("hello, brave world", Some(0), 6, "hello"),
    ("hello, brave world", Some(5), 6, ","),
    ("hello, brave world", Some(5), 12, "brave"),
    ("hello, brave world", Some(12), 18, "world"),
    ("hello, brave world", Some(18), 18, ""),
    ("hello, brave world", Some(18), 12, "brave"),
    ("hello, brave world", Some(12), 7, "brave"),
    ("hello, brave world", None, 0, "hello"),
    ("/usr/local/bin", Some(0), 4, "usr"),
    ("/usr/local/bin", Some(4), 10, "local"),
    ("/usr/local/bin", Some(10), 14, "bin"),
    ("foo,bar baz", Some(0), 4, "foo"),
    ("echo hello, world", Some(5), 10, "hello"),
    ("ls -l", Some(0), 2, "ls"),
    ("", None, 0, ""),
];

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

    #[test]
    fn word_level_vectors() {
        for &(text, level, want) in WORD_LEVEL_VECTORS {
            let chars: Vec<char> = text.chars().collect();
            let got = words_of(&chars, level)
                .into_iter()
                .map(|r| chars[r].iter().collect::<String>())
                .collect::<Vec<_>>()
                .join("|");
            assert_eq!(got, want, "{text:?} {level:?}");
            assert_eq!(got.replace('|', ""), text, "the words tile {text:?}");
        }
    }

    #[test]
    fn word_at_by_level() {
        let l = "ls --color=auto /usr";
        assert_eq!(
            word_at_level(l, 8, WordLevel::Fine).as_deref(),
            Some("color")
        );
        assert_eq!(
            word_at_level(l, 8, WordLevel::Medium).as_deref(),
            Some("--color=auto")
        );
        assert_eq!(
            word_at_level(l, 2, WordLevel::Medium).as_deref(),
            Some("ls")
        );
        assert_eq!(
            word_at_level(l, 40, WordLevel::Medium).as_deref(),
            Some("usr")
        );
        assert_eq!(word_at_level("  ", 0, WordLevel::Medium), None);
    }

    #[test]
    fn word_motion_vectors() {
        for &(text, from, at, want) in WORD_MOTION_VECTORS {
            let chars: Vec<char> = text.chars().collect();
            let got = word_motion(&chars, from, at, WordLevel::Fine)
                .map(|r| chars[r].iter().collect::<String>().trim().to_owned())
                .unwrap_or_default();
            assert_eq!(got, want, "{text:?} {from:?} -> {at}");
        }
    }
}
