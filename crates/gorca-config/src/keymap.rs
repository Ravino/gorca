// SPDX-License-Identifier: GPL-3.0-or-later
//! Keymaps as data: chord strings, contexts, layering and the conflict rules (ADR-0005 7.5).
//!
//! The config crate knows chords and command **names** only. Mapping names to commands and
//! checking that a command may be bound in a context (KC-8, KC-9) is `gorca-core`'s job; turning
//! key names into physical keys is the input crate's.
//!
//! Chord syntax: `+`-separated tokens, the last one is the key. Modifier tokens (case-insensitive):
//! `gorca` (or `Insert`, `KP_Insert`, `Caps_Lock`, which normalise to it), `ctrl`/`control`, `alt`,
//! `shift`, `super`/`meta`. A press count is appended as `*N` (`gorca+Tab*2`, N from 2 to 3).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::diag::{Diag, DiagMsg};

/// Where a binding applies (ADR-0005 5.2). App contexts arrive with extensions (M5).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum ContextName {
    Global,
    Focus,
    Browse,
    Review,
}

impl ContextName {
    pub const ALL: [ContextName; 4] = [
        ContextName::Global,
        ContextName::Focus,
        ContextName::Browse,
        ContextName::Review,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ContextName::Global => "global",
            ContextName::Focus => "focus",
            ContextName::Browse => "browse",
            ContextName::Review => "review",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "global" => Some(ContextName::Global),
            "focus" => Some(ContextName::Focus),
            "browse" => Some(ContextName::Browse),
            "review" => Some(ContextName::Review),
            _ => None,
        }
    }
}

/// Non-gorca modifiers of a chord.
#[allow(clippy::struct_excessive_bools)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ChordMods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_: bool,
}

impl ChordMods {
    fn count_ctrl_alt_super(self) -> u8 {
        u8::from(self.ctrl) + u8::from(self.alt) + u8::from(self.super_)
    }
}

/// A normalised chord. Equality of two spellings (`Insert+t`, `gorca+T`) is equality of values.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ChordSpec {
    /// A gorca modifier is held.
    pub gorca: bool,
    pub mods: ChordMods,
    /// XKB keysym name, lower-cased (`t`, `f12`, `kp_home`, `prior`).
    pub key: String,
    /// 1 to 3: the press that triggers it.
    pub presses: u8,
}

impl fmt::Display for ChordSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.gorca {
            f.write_str("gorca+")?;
        }
        if self.mods.ctrl {
            f.write_str("ctrl+")?;
        }
        if self.mods.alt {
            f.write_str("alt+")?;
        }
        if self.mods.shift {
            f.write_str("shift+")?;
        }
        if self.mods.super_ {
            f.write_str("super+")?;
        }
        f.write_str(&self.key)?;
        if self.presses > 1 {
            write!(f, "*{}", self.presses)?;
        }
        Ok(())
    }
}

const GORCA_TOKENS: &[&str] = &["gorca", "insert", "kp_insert", "caps_lock"];
const MODIFIER_KEYS: &[&str] = &[
    "shift_l",
    "shift_r",
    "control_l",
    "control_r",
    "alt_l",
    "alt_r",
    "super_l",
    "super_r",
    "meta_l",
    "meta_r",
    "iso_level3_shift",
    "num_lock",
    "scroll_lock",
];

impl ChordSpec {
    /// Parse and normalise. The error is a short reason for the diagnostic.
    pub fn parse(s: &str) -> Result<ChordSpec, &'static str> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty chord");
        }
        let (body, presses) = match s.rsplit_once('*') {
            Some((b, n)) if !b.is_empty() => match n.trim() {
                "2" => (b, 2),
                "3" => (b, 3),
                _ => return Err("the press count must be 2 or 3"),
            },
            _ => (s, 1),
        };
        let mut tokens: Vec<&str> = body.split('+').map(str::trim).collect();
        let Some(key) = tokens.pop() else {
            return Err("empty chord");
        };
        if key.is_empty() {
            // "gorca++" style: a literal plus key is spelled `plus`.
            return Err("missing key; spell the plus key as `plus`");
        }
        let mut spec = ChordSpec {
            gorca: false,
            mods: ChordMods::default(),
            key: normalise_key(key),
            presses,
        };
        for t in tokens {
            let l = t.to_ascii_lowercase();
            if GORCA_TOKENS.contains(&l.as_str()) {
                spec.gorca = true;
                continue;
            }
            match l.as_str() {
                "ctrl" | "control" => spec.mods.ctrl = true,
                "alt" => spec.mods.alt = true,
                "shift" => spec.mods.shift = true,
                "super" | "meta" => spec.mods.super_ = true,
                _ => return Err("unknown modifier"),
            }
        }
        if MODIFIER_KEYS.contains(&spec.key.as_str()) {
            return Err("a modifier cannot be the key of a chord");
        }
        if GORCA_TOKENS.contains(&spec.key.as_str())
            && !spec.gorca
            && spec.mods == ChordMods::default()
        {
            return Err("the gorca modifier cannot be bound");
        }
        Ok(spec)
    }

    fn is_keypad(&self) -> bool {
        self.key.starts_with("kp_")
    }

    fn is_high_function_key(&self) -> bool {
        self.key
            .strip_prefix('f')
            .and_then(|n| n.parse::<u8>().ok())
            .is_some_and(|n| (13..=24).contains(&n))
    }

    fn is_single_letter_or_digit(&self) -> bool {
        let mut it = self.key.chars();
        matches!((it.next(), it.next()), (Some(c), None) if c.is_ascii_alphanumeric() || c == ',')
            || self.key == "comma"
    }

    /// KC-5: may this chord, which has no gorca modifier, be bound in `ctx`?
    pub fn allowed_without_gorca(&self, ctx: ContextName) -> bool {
        if self.gorca {
            return true;
        }
        match ctx {
            ContextName::Global | ContextName::Focus => {
                self.is_keypad()
                    || self.mods.count_ctrl_alt_super() >= 2
                    || self.is_high_function_key()
            }
            ContextName::Browse | ContextName::Review => true,
        }
    }

    /// Single letters and digits without any modifier would break typing outside browse/review.
    pub fn is_plain_text_key(&self) -> bool {
        !self.gorca && self.mods == ChordMods::default() && self.is_single_letter_or_digit()
    }
}

fn normalise_key(k: &str) -> String {
    k.to_ascii_lowercase()
}

/// A binding value. `None` is `"none"`: unbind, and block fall-through (KC-3).
pub type Binding = Option<String>;

/// Chord tables per context after layering. Deterministic order (`BTreeMap`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolvedKeymap {
    pub contexts: BTreeMap<ContextName, BTreeMap<ChordSpec, Binding>>,
}

impl ResolvedKeymap {
    pub fn table(&self, ctx: ContextName) -> Option<&BTreeMap<ChordSpec, Binding>> {
        self.contexts.get(&ctx)
    }

    /// Every `(context, chord, command)` triple that binds a command.
    pub fn bound(&self) -> impl Iterator<Item = (ContextName, &ChordSpec, &str)> {
        self.contexts.iter().flat_map(|(ctx, t)| {
            t.iter()
                .filter_map(move |(c, b)| b.as_deref().map(|b| (*ctx, c, b)))
        })
    }

    /// Chords bound to `command` in `ctx`.
    pub fn chords_of<'a>(
        &'a self,
        ctx: ContextName,
        command: &'a str,
    ) -> impl Iterator<Item = &'a ChordSpec> {
        self.contexts.get(&ctx).into_iter().flat_map(move |t| {
            t.iter()
                .filter(move |(_, b)| b.as_deref() == Some(command))
                .map(|(c, _)| c)
        })
    }
}

/// One layer's entries in file order: `(context, raw chord text, command, span)`.
pub(crate) struct LayerEntry {
    pub ctx: ContextName,
    pub chord_text: String,
    pub command: String,
    pub span: Option<(u32, u32)>,
}

/// Merge one layer into `map` with KC-1 (later entry wins within a layer, with a diagnostic),
/// KC-2 (a higher layer overrides a lower one silently), KC-3 (`none`), KC-5/6 (shape rules).
pub(crate) fn merge_layer(
    map: &mut ResolvedKeymap,
    entries: Vec<LayerEntry>,
    layer_name: &str,
    diags: &mut Vec<Diag>,
) {
    let mut seen: BTreeSet<(ContextName, ChordSpec)> = BTreeSet::new();
    for e in entries {
        let path = format!("keymap.{}.{}", e.ctx.name(), quote_if_needed(&e.chord_text));
        let chord = match ChordSpec::parse(&e.chord_text) {
            Ok(c) => c,
            Err(reason) => {
                diags.push(Diag::error(
                    path,
                    e.span,
                    DiagMsg::BadChord {
                        chord: e.chord_text,
                        reason,
                    },
                ));
                continue;
            }
        };
        if !chord.allowed_without_gorca(e.ctx) {
            diags.push(Diag::error(
                path,
                e.span,
                DiagMsg::BadChord {
                    chord: e.chord_text,
                    reason: "needs the gorca modifier here, or a keypad key, two of Ctrl/Alt/Super, or F13 to F24",
                },
            ));
            continue;
        }
        if chord.is_plain_text_key() && matches!(e.ctx, ContextName::Global | ContextName::Focus) {
            diags.push(Diag::error(
                path,
                e.span,
                DiagMsg::BadChord {
                    chord: e.chord_text,
                    reason: "a plain letter or digit would break typing",
                },
            ));
            continue;
        }
        if !seen.insert((e.ctx, chord.clone())) {
            diags.push(Diag::warning(
                path,
                e.span,
                DiagMsg::DuplicateChord {
                    chord: chord.to_string(),
                },
            ));
        }
        let _ = layer_name;
        let binding = if e.command == "none" {
            None
        } else {
            Some(e.command)
        };
        map.contexts
            .entry(e.ctx)
            .or_default()
            .insert(chord, binding);
    }
}

fn quote_if_needed(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        s.to_owned()
    } else {
        format!("\"{s}\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> ChordSpec {
        ChordSpec::parse(s).unwrap()
    }

    #[test]
    fn spellings_normalise_to_one_chord() {
        assert_eq!(p("Insert+t"), p("gorca+T"));
        assert_eq!(p("KP_Insert+Ctrl+Up"), p("ctrl+gorca+up"));
        assert_eq!(p("gorca+Tab*2").presses, 2);
        assert_eq!(p("gorca+Tab*2").to_string(), "gorca+tab*2");
        assert_eq!(p("Super+Alt+x").to_string(), "alt+super+x");
    }

    #[test]
    fn bad_chords_are_rejected_with_a_reason() {
        for bad in [
            "",
            "gorca+",
            "gorca+Hyper+t",
            "gorca+Shift_L",
            "gorca+t*9",
            "Insert",
        ] {
            assert!(ChordSpec::parse(bad).is_err(), "{bad}");
        }
        assert!(ChordSpec::parse("gorca+Escape").is_ok());
        assert!(
            ChordSpec::parse("Escape").is_ok(),
            "allowed in browse; KC-5 guards the rest"
        );
    }

    #[test]
    fn kc5_plain_chords_in_global_and_focus() {
        assert!(!p("t").allowed_without_gorca(ContextName::Global));
        assert!(!p("ctrl+t").allowed_without_gorca(ContextName::Focus));
        assert!(p("ctrl+alt+t").allowed_without_gorca(ContextName::Global));
        assert!(p("kp_home").allowed_without_gorca(ContextName::Focus));
        assert!(p("f13").allowed_without_gorca(ContextName::Global));
        assert!(!p("f12").allowed_without_gorca(ContextName::Global));
        assert!(p("t").allowed_without_gorca(ContextName::Browse));
        assert!(p("gorca+t").allowed_without_gorca(ContextName::Global));
    }

    fn entry(ctx: ContextName, chord: &str, cmd: &str) -> LayerEntry {
        LayerEntry {
            ctx,
            chord_text: chord.to_owned(),
            command: cmd.to_owned(),
            span: None,
        }
    }

    #[test]
    fn later_layer_wins_silently_and_none_unbinds() {
        let mut m = ResolvedKeymap::default();
        let mut d = Vec::new();
        merge_layer(
            &mut m,
            vec![
                entry(ContextName::Global, "gorca+t", "info.window_title"),
                entry(ContextName::Global, "gorca+x", "speech.repeat_last"),
            ],
            "preset",
            &mut d,
        );
        merge_layer(
            &mut m,
            vec![
                entry(ContextName::Global, "gorca+T", "info.time_date"),
                entry(ContextName::Global, "gorca+x", "none"),
            ],
            "user",
            &mut d,
        );
        assert!(d.is_empty(), "{d:?}");
        let g = m.table(ContextName::Global).unwrap();
        assert_eq!(g[&p("gorca+t")].as_deref(), Some("info.time_date"));
        assert_eq!(g[&p("gorca+x")], None);
    }

    #[test]
    fn duplicates_in_one_layer_warn_and_the_later_wins() {
        let mut m = ResolvedKeymap::default();
        let mut d = Vec::new();
        merge_layer(
            &mut m,
            vec![
                entry(ContextName::Global, "Insert+t", "a"),
                entry(ContextName::Global, "gorca+T", "b"),
            ],
            "user",
            &mut d,
        );
        assert_eq!(d.len(), 1);
        assert!(matches!(d[0].msg, DiagMsg::DuplicateChord { .. }));
        assert_eq!(
            m.table(ContextName::Global).unwrap()[&p("gorca+t")].as_deref(),
            Some("b")
        );
    }

    #[test]
    fn kc5_violations_are_dropped_with_a_diagnostic() {
        let mut m = ResolvedKeymap::default();
        let mut d = Vec::new();
        merge_layer(
            &mut m,
            vec![
                entry(ContextName::Global, "t", "a"),
                entry(ContextName::Focus, "ctrl+t", "a"),
                entry(ContextName::Browse, "t", "a"),
            ],
            "user",
            &mut d,
        );
        assert_eq!(d.len(), 2);
        assert!(m.table(ContextName::Global).is_none());
        assert!(m.table(ContextName::Browse).is_some());
    }

    #[test]
    fn chords_of_lists_bindings_of_a_command() {
        let mut m = ResolvedKeymap::default();
        let mut d = Vec::new();
        merge_layer(
            &mut m,
            vec![
                entry(ContextName::Global, "gorca+t", "x"),
                entry(ContextName::Global, "gorca+u", "x"),
                entry(ContextName::Global, "gorca+v", "y"),
            ],
            "preset",
            &mut d,
        );
        assert_eq!(m.chords_of(ContextName::Global, "x").count(), 2);
        assert_eq!(m.bound().count(), 3);
    }
}
