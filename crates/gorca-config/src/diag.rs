// SPDX-License-Identifier: GPL-3.0-or-later
//! Diagnostics: every problem found while loading is reported, none is fatal (ADR-0005 7.4).

use std::fmt;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Severity {
    /// The file is honoured, but something deserves attention (KC-7, obsolete keys).
    Warning,
    /// A value was replaced by its default or an entry was dropped.
    Error,
}

/// What went wrong. `Display` is the sentence that `settings.report_problems` speaks.
#[derive(Clone, Debug, Eq, PartialEq, Hash, thiserror::Error)]
pub enum DiagMsg {
    #[error("file is larger than {limit} bytes, ignored")]
    TooLarge { limit: usize },
    #[error("{0}")]
    Syntax(String),
    #[error("expected {expected}, using the default")]
    WrongType { expected: &'static str },
    #[error("{value} is out of range {min} to {max}, using {used}")]
    OutOfRange {
        value: i64,
        min: i64,
        max: i64,
        used: i64,
    },
    #[error("unknown value `{value}`, expected {allowed}, using the default")]
    UnknownEnum {
        value: String,
        allowed: &'static str,
    },
    #[error("unknown key{}", suggestion_suffix(.suggestion.as_deref()))]
    UnknownKey { suggestion: Option<String> },
    #[error("obsolete key, ignored")]
    ObsoleteKey,
    #[error("schema {found} is newer than this version ({supported}); unknown keys are ignored")]
    NewerSchema { found: i64, supported: u32 },
    #[error("schema must be a positive integer")]
    BadSchema,
    #[error("invalid chord `{chord}`: {reason}")]
    BadChord { chord: String, reason: &'static str },
    #[error("chord `{chord}` is bound twice in this context, the later entry wins")]
    DuplicateChord { chord: String },
    #[error("unknown command `{command}`{}", suggestion_suffix(.suggestion.as_deref()))]
    UnknownCommand {
        command: String,
        suggestion: Option<String>,
    },
    #[error("command `{command}` cannot be bound in the {context} context")]
    WrongContext {
        command: String,
        context: &'static str,
    },
    #[error("unknown keymap context")]
    UnknownContext,
    #[error("chord `{chord}` is already used by the system shortcut `{shortcut}`")]
    SystemShortcut { chord: String, shortcut: String },
    /// ADR-0016 PK-1/NS-1/PF-1: a profile or row key that its trigger or origin may not set.
    #[error("key not allowed here: {why}")]
    ProfileKey { why: &'static str },
    /// ADR-0016 PK-1: keys of an extension that is not installed are kept, inactive.
    #[error("extension `{ext}` is not installed, its keys are kept but inactive")]
    InactiveExtension { ext: String },
    /// ADR-0016 PC-1.
    #[error("volume {value} is below the floor {floor}, ignored")]
    VolumeFloor { value: i64, floor: i64 },
    /// A malformed trigger, row or profile table; the entry is skipped.
    #[error("invalid {what}: {why}")]
    ProfileInvalid { what: &'static str, why: String },
    /// ADR-0016 PF-6/PC-3: a limit was exceeded and the excess dropped.
    #[error("too many {what}: the limit is {limit}")]
    ProfileLimit { what: &'static str, limit: usize },
    /// ADR-0016 PM-1: two profiles of one layer have the same trigger.
    #[error("profile `{other}` has the same trigger, the first by id wins ties")]
    SameTrigger { other: String },
    /// ADR-0016 PC-0: the file does not parse, the last good version is kept.
    #[error("{0}; the last good version of this profile stays in effect")]
    KeptLastGood(String),
    #[error("{0}")]
    Other(String),
}

fn suggestion_suffix(s: Option<&str>) -> String {
    match s {
        Some(s) => format!(", did you mean `{s}`"),
        None => String::new(),
    }
}

/// One problem: where, how bad, what.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Diag {
    pub severity: Severity,
    /// Dotted path of the key, for example `speech.rate` or `keymap.global."gorca+t"`.
    pub path: String,
    /// Byte span in the file, when known.
    pub span: Option<(u32, u32)>,
    pub msg: DiagMsg,
}

impl Diag {
    pub fn error(path: impl Into<String>, span: Option<(u32, u32)>, msg: DiagMsg) -> Self {
        Self {
            severity: Severity::Error,
            path: path.into(),
            span,
            msg,
        }
    }

    pub fn warning(path: impl Into<String>, span: Option<(u32, u32)>, msg: DiagMsg) -> Self {
        Self {
            severity: Severity::Warning,
            path: path.into(),
            span,
            msg,
        }
    }

    /// 1-based line and column of the span start in `text`.
    pub fn line_col(&self, text: &str) -> Option<(usize, usize)> {
        let at = self.span?.0 as usize;
        let upto = text.get(..at)?;
        let line = upto.bytes().filter(|b| *b == b'\n').count() + 1;
        let col = upto.rfind('\n').map_or(at, |nl| at - nl - 1) + 1;
        Some((line, col))
    }
}

impl fmt::Display for Diag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(f, "{}", self.msg)
        } else {
            write!(f, "{}: {}", self.path, self.msg)
        }
    }
}

/// Order-independent hash of a diagnostic set: "spoken once per distinct set" (CV-2).
pub fn diags_hash(diags: &[Diag]) -> u64 {
    // Sum of per-diagnostic FNV hashes: independent of order, deterministic across runs.
    diags
        .iter()
        .map(|d| fnv1a(format!("{d:?}").as_bytes()))
        .fold(0u64, u64::wrapping_add)
}

/// FNV-1a, 64 bit. Stable across runs and platforms (the std hasher is not).
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Edit distance, capped: enough for "did you mean" on short key and command names.
pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            let v = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
            cur.push(v);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The closest candidate within edit distance 2, if any.
pub fn did_you_mean<'a>(
    input: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    let mut best: Option<(usize, &str)> = None;
    for c in candidates {
        let d = edit_distance(input, c);
        if d <= 2 && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, c));
        }
    }
    best.map(|(_, c)| c.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_distance_basics() {
        assert_eq!(edit_distance("rate", "rate"), 0);
        assert_eq!(edit_distance("rate", "rat"), 1);
        assert_eq!(edit_distance("pitch", "ptch"), 1);
        assert_eq!(edit_distance("", "abc"), 3);
    }

    #[test]
    fn suggestions_stay_within_distance_two() {
        assert_eq!(
            did_you_mean("volum", ["rate", "pitch", "volume"]),
            Some("volume".to_owned())
        );
        assert_eq!(did_you_mean("zzzzzz", ["rate", "pitch"]), None);
    }

    #[test]
    fn line_and_column_are_one_based() {
        let text = "a = 1\nbb = 2\n";
        let d = Diag::error("bb", Some((6, 8)), DiagMsg::BadSchema);
        assert_eq!(d.line_col(text), Some((2, 1)));
        let d = Diag::error("x", Some((2, 3)), DiagMsg::BadSchema);
        assert_eq!(d.line_col(text), Some((1, 3)));
    }

    #[test]
    fn hash_ignores_order_but_not_content() {
        let a = Diag::error("a", None, DiagMsg::BadSchema);
        let b = Diag::error("b", None, DiagMsg::BadSchema);
        assert_eq!(
            diags_hash(&[a.clone(), b.clone()]),
            diags_hash(&[b.clone(), a.clone()])
        );
        assert_ne!(diags_hash(std::slice::from_ref(&a)), diags_hash(&[b]));
        assert_ne!(diags_hash(&[a]), diags_hash(&[]));
    }
}
