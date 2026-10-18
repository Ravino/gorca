// SPDX-License-Identifier: GPL-3.0-or-later
//! The editing model: what every step of the matrix script (`edit.rs`) must speak, and where a
//! target deliberately differs (docs/reports/unified-editing.md section 3).
//!
//! The model is the user's terminal model, applied everywhere:
//!
//! * a typed character is spoken by name (`comma`, `space`), once;
//! * Left/Right speak the character the caret landed on (`blank` behind the last one), Home and
//!   End the character at the caret; a key that moves nothing speaks the unit at the caret;
//! * Ctrl+Left/Right speak the word just passed or reached, without its punctuation;
//! * BackSpace and Delete speak the removed character, Ctrl+BackSpace/Delete the removed word;
//! * Shift moves speak `X selected` / `X unselected` (the text that changed), Ctrl+A
//!   `all selected, N characters`;
//! * Up/Down speak the visual line, Ctrl+Up/Down the paragraph, Home/End the character, Ctrl+Home/End
//!   and PageUp/PageDown the line at the caret;
//! * Return speaks `new line`, once.
//!
//! Deliberate differences, each one a fact of the toolkit and not of the reader:
//!
//! * [`Stops::Punct`]: GtkSourceView (gedit, Text Editor) and Blink stop a Ctrl+arrow between a word
//!   and the comma behind it, so one more stop (`,`) is spoken; the other toolkits skip it.
//! * Gecko lands behind the comma, so a Ctrl+Delete or a Ctrl+Shift+Right takes it along
//!   (`ello,` where the others say `ello`).
//! * A wrapped line is whatever the widget's width makes it: `Line` only checks that one visual
//!   line of the paragraph is spoken.
//! * Ctrl+Up/Down land where the toolkit puts them (paragraph start or the neighbour): `Para`
//!   only checks that one whole paragraph is spoken.
//! * In a contenteditable a selection that crosses blocks is not reported on the block the caret
//!   is in (Gecko and Blink send no selection ranges for it): the line is spoken without
//!   `selected`; a selection inside one block is announced like everywhere else.

use super::edit::{LINE, PARA, Target, WebField};

/// Does a Ctrl+arrow stop between a word and its trailing punctuation?
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Stops {
    Words,
    Punct,
}

/// What a step must speak.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Exp {
    /// Exactly these utterances.
    Exact(Vec<String>),
    /// Nothing at all.
    Silent,
    /// One utterance that is a visual line of the paragraph (a part of it, no line break).
    Line,
    /// One utterance that is a whole paragraph of the document, or `blank`.
    Para,
    /// One utterance that starts with `all selected, ` and ends with ` characters`.
    SelectAll,
    /// One utterance ending in `selected` (a line or text, then the word).
    Selected,
    /// One utterance ending in `unselected`.
    Unselected,
    /// A selection across blocks of a contenteditable: one utterance, `selected` is not required.
    BlockSelection,
    /// Silence (the boundary cue: the key could not move the caret on this toolkit) or the
    /// inner expectation (the caret moved).
    OrSilent(Box<Exp>),
    /// One utterance that starts with this (`cut, ` or `pasted, ` and the text).
    Prefixed(&'static str),
    /// Any non-empty speech.
    Some,
    /// The text that was removed (checklist row 25): the whole document, which the script selected
    /// with Ctrl+Home, Shift+Down ... Ctrl+Shift+End; compared with line breaks as blanks.
    Removed,
}

fn ex(v: &[&str]) -> Exp {
    Exp::Exact(v.iter().map(|s| (*s).to_owned()).collect())
}

/// Names the way the verbalizer speaks the characters the script types.
fn names(text: &str) -> Exp {
    Exp::Exact(
        text.chars()
            .map(|c| match c {
                ' ' => "space".to_owned(),
                ',' => "comma".to_owned(),
                other => other.to_string(),
            })
            .collect(),
    )
}

pub fn stops(t: Target, chromium: bool) -> Stops {
    match t {
        Target::Gedit | Target::TextEditor => Stops::Punct,
        Target::Web(_) if chromium => Stops::Punct,
        Target::Web(_)
        | Target::Gtk3Entry
        | Target::Gtk3Text
        | Target::Gtk4Entry
        | Target::Gtk4Text
        | Target::Terminal => Stops::Words,
    }
}

fn gecko(t: Target, chromium: bool) -> bool {
    matches!(t, Target::Web(_)) && !chromium
}

/// Does the toolkit's text widget undo and redo?
fn has_undo(t: Target) -> bool {
    !matches!(t, Target::Gtk3Entry | Target::Gtk3Text)
}

/// Is Ctrl+Y bound to redo? gedit only has Ctrl+Shift+Z; the key does nothing there.
fn has_redo_key(t: Target) -> bool {
    has_undo(t) && t != Target::Gedit
}

fn editable(t: Target) -> bool {
    t == Target::Web(WebField::Editable)
}

/// The expectation of `step` for `t`, `None` when the step is recorded and not judged.
pub fn expect(t: Target, chromium: bool, step: &str) -> Option<Exp> {
    let punct = stops(t, chromium) == Stops::Punct;
    let comma_along = gecko(t, chromium);
    Some(match step {
        "type" => names(LINE),
        "Left" => ex(&["d"]),
        // A key that cannot move the caret gives the boundary cue, which is silent speech.
        "Right at end" | "Left at start" => Exp::Silent,
        // After a select-all the caret may already be at the end: the key then collapses the
        // selection and moves nothing.
        "Right collapses" | "End 2" => Exp::OrSilent(Box::new(ex(&["blank"]))),
        "Right" | "End" => ex(&["blank"]),
        "Home" | "Home 2" => ex(&["h"]),
        "Ctrl+Left 1" => ex(&["world"]),
        "Ctrl+Left 2" => ex(&["brave"]),
        "Ctrl+Left 3" if punct => ex(&[","]),
        "Ctrl+Left 3" => ex(&["hello"]),
        "Ctrl+Left 4 at start" if punct => ex(&["hello"]),
        "Ctrl+Left 4 at start" => Exp::Silent,
        "Ctrl+Right 1" => ex(&["hello"]),
        "Ctrl+Right 2" if punct => ex(&[","]),
        "Ctrl+Right 2" => ex(&["brave"]),
        "Ctrl+Right 3" if punct => ex(&["brave"]),
        "Ctrl+Right 3" => ex(&["world"]),
        "Ctrl+Right 4 at end" if punct => ex(&["world"]),
        "Ctrl+Right 4 at end" => Exp::Silent,
        "BackSpace" => ex(&["d"]),
        "Delete at end" | "Ctrl+Delete at end" => Exp::Silent,
        "Delete" => ex(&["h"]),
        "Shift+Right" => ex(&["e selected"]),
        "Shift+Right 2" => ex(&["l selected"]),
        "Shift+Left" => ex(&["l unselected"]),
        "Shift+Left 2" => ex(&["e unselected"]),
        "Ctrl+Shift+Right" if comma_along => ex(&["ello, selected"]),
        "Ctrl+Shift+Right" => ex(&["ello selected"]),
        "Ctrl+Shift+Left" if comma_along => ex(&["ello, unselected"]),
        "Ctrl+Shift+Left" => ex(&["ello unselected"]),
        "Shift+End" | "Ctrl+A" => ex(&["all selected, 16 characters"]),
        "Shift+Home" => ex(&["ello, brave worl unselected"]),
        "Ctrl+BackSpace" => ex(&["worl"]),
        "Home 3" => ex(&["e"]),
        "Ctrl+Delete" if comma_along => ex(&["ello,"]),
        "Ctrl+Delete" => ex(&["ello"]),
        "clip: select all" => Exp::SelectAll,
        "clip: Ctrl+C" => ex(&["copied"]),
        "clip: Ctrl+X" => Exp::Prefixed("cut, "),
        "clip: Ctrl+V" => Exp::Prefixed("pasted, "),
        // GTK 3 entries and text views have no undo: nothing changes, nothing is claimed.
        "clip: Ctrl+Z" if has_undo(t) => ex(&["undo"]),
        "clip: Ctrl+Y" if has_redo_key(t) => ex(&["redo"]),
        "clip: Ctrl+Z" | "clip: Ctrl+Y" => Exp::Silent,
        "clip: restore" => Exp::Some,
        // The multi-line phase.
        "select all" => Exp::SelectAll,
        "type line 1" => names("one two"),
        "type paragraph" => names(PARA),
        "type last" => names("last line"),
        "Enter" | "Enter 2" => ex(&["new line"]),
        "Up" | "Up 2" | "Down" => Exp::Line,
        "Ctrl+Up 2" => Exp::OrSilent(Box::new(Exp::Para)),
        "Ctrl+Up" | "Ctrl+Down" => Exp::Para,
        "Ctrl+Home" | "Ctrl+Home 2" | "PageUp" => ex(&["one two"]),
        "Ctrl+End" | "PageDown" => ex(&["last line"]),
        "Shift+Down" | "Shift+Down 2" | "Ctrl+Shift+Down" if editable(t) => Exp::BlockSelection,
        "Shift+Down" | "Shift+Down 2" | "Ctrl+Shift+Down" => Exp::Selected,
        "Shift+Up" if editable(t) => Exp::BlockSelection,
        "Shift+Up" => Exp::Unselected,
        "BackSpace selection" => Exp::Removed,
        // The editor applications: find, replace, tabs and the save dialog speak something.
        "Ctrl+H" | "Ctrl+S" | "save: Escape" => Exp::Some,
        _ => return None,
    })
}

/// Steps that are known to fail on a target, with evidence in docs/reports/unified-editing.md
/// (verification F4): a contenteditable of Firefox and Chromium does not speak the removed text
/// when Backspace deletes a selection. They are recorded as failures and never gate.
pub fn known_failing(t: Target, step: &str) -> bool {
    editable(t) && step == "BackSpace selection"
}

fn is_line_of_paragraph(s: &str) -> bool {
    !s.is_empty() && !s.contains('\n') && PARA.contains(s.trim())
}

fn is_paragraph(s: &str) -> bool {
    s == "blank" || s == PARA || s == "one two" || s == "last line"
}

fn one(got: &[String]) -> Result<&str, String> {
    match got {
        [s] => Ok(s.as_str()),
        other => Err(format!("expected one utterance, got {}", other.len())),
    }
}

/// Judge `got` against the model: `None` when the step is not judged.
pub fn check(t: Target, chromium: bool, step: &str, got: &[String]) -> Option<Result<(), String>> {
    let e = expect(t, chromium, step)?;
    let r = match &e {
        Exp::OrSilent(inner) => {
            if got.is_empty() {
                Ok(())
            } else {
                check_exp(inner, got)
            }
        }
        _ => check_exp(&e, got),
    };
    Some(r)
}

fn check_exp(e: &Exp, got: &[String]) -> Result<(), String> {
    match e {
        Exp::OrSilent(inner) => check_exp(inner, got),
        Exp::Exact(v) => {
            if v == got {
                Ok(())
            } else {
                Err(format!("(want {v:?})"))
            }
        }
        Exp::Silent => {
            if got.is_empty() {
                Ok(())
            } else {
                Err("(want silence)".to_owned())
            }
        }
        Exp::Prefixed(p) => one(got).and_then(|s| {
            if s.starts_with(p) {
                Ok(())
            } else {
                Err(format!("(want {p:?} and the text)"))
            }
        }),
        Exp::Some => {
            if got.iter().any(|s| !s.trim().is_empty()) {
                Ok(())
            } else {
                Err("(want speech)".to_owned())
            }
        }
        Exp::Removed => {
            let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
            let doc = norm(&format!("one two {PARA} last line"));
            if norm(&got.join(" ")) == doc {
                Ok(())
            } else {
                Err("(want the removed text: the whole document)".to_owned())
            }
        }
        Exp::Line => one(got).and_then(|s| {
            if is_line_of_paragraph(s) || s == "one two" || s == "last line" {
                Ok(())
            } else {
                Err("(want one visual line)".to_owned())
            }
        }),
        Exp::Para => one(got).and_then(|s| {
            if is_paragraph(s) {
                Ok(())
            } else {
                Err("(want one whole paragraph)".to_owned())
            }
        }),
        Exp::SelectAll => one(got).and_then(|s| {
            if s.starts_with("all selected, ") && s.ends_with(" characters") {
                Ok(())
            } else {
                Err("(want all selected, N characters)".to_owned())
            }
        }),
        Exp::Selected => one(got).and_then(|s| {
            if s.ends_with(" selected") && !s.ends_with(" unselected") {
                Ok(())
            } else {
                Err("(want X selected)".to_owned())
            }
        }),
        Exp::Unselected => one(got).and_then(|s| {
            if s.ends_with(" unselected") {
                Ok(())
            } else {
                Err("(want X unselected)".to_owned())
            }
        }),
        Exp::BlockSelection => one(got).map(|_| ()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn backspace_over_a_selection_must_speak_the_removed_text() {
        let t = Target::Gtk4Text;
        let doc = format!("one two\n{PARA}\nlast line");
        assert_eq!(
            check(t, false, "BackSpace selection", &g(&[&doc])),
            Some(Ok(()))
        );
        assert!(
            check(t, false, "BackSpace selection", &g(&["one two"])).is_some_and(|r| r.is_err())
        );
        assert!(check(t, false, "BackSpace selection", &g(&["blank"])).is_some_and(|r| r.is_err()));
        assert!(check(t, false, "BackSpace selection", &g(&[])).is_some_and(|r| r.is_err()));
        assert!(known_failing(
            Target::Web(WebField::Editable),
            "BackSpace selection"
        ));
        assert!(!known_failing(t, "BackSpace selection"));
    }

    #[test]
    fn clipboard_steps_follow_the_toolkits_undo() {
        let c = |t, step, got: &[&str]| check(t, false, step, &g(got));
        assert_eq!(
            c(Target::Gtk4Text, "clip: Ctrl+C", &["copied"]),
            Some(Ok(()))
        );
        assert_eq!(
            c(Target::Gtk4Text, "clip: Ctrl+X", &["cut, hello"]),
            Some(Ok(()))
        );
        assert!(matches!(
            c(Target::Gtk4Text, "clip: Ctrl+V", &["hello"]),
            Some(Err(_))
        ));
        assert_eq!(
            c(Target::Gtk4Entry, "clip: Ctrl+Z", &["undo"]),
            Some(Ok(()))
        );
        assert_eq!(c(Target::Gtk3Entry, "clip: Ctrl+Z", &[]), Some(Ok(())));
        assert!(matches!(
            c(Target::Gtk3Entry, "clip: Ctrl+Y", &["redo"]),
            Some(Err(_))
        ));
    }

    #[test]
    fn typed_names_follow_the_verbalizer() {
        let Exp::Exact(v) = names("a, b") else {
            panic!("exact")
        };
        assert_eq!(v, ["a", "comma", "space", "b"]);
    }

    #[test]
    fn punct_stoppers_are_gtksourceview_and_blink_only() {
        assert_eq!(stops(Target::Gedit, false), Stops::Punct);
        assert_eq!(stops(Target::TextEditor, false), Stops::Punct);
        assert_eq!(stops(Target::Web(WebField::Textarea), true), Stops::Punct);
        assert_eq!(stops(Target::Web(WebField::Textarea), false), Stops::Words);
        assert_eq!(stops(Target::Gtk4Text, false), Stops::Words);
        assert_eq!(stops(Target::Terminal, false), Stops::Words);
    }

    #[test]
    fn the_same_word_step_has_one_expectation_per_stop_style() {
        let w = check(Target::Gtk3Entry, false, "Ctrl+Right 2", &g(&["brave"]));
        assert_eq!(w, Some(Ok(())));
        let p = check(Target::Gedit, false, "Ctrl+Right 2", &g(&[","]));
        assert_eq!(p, Some(Ok(())));
        let bad = check(Target::Gedit, false, "Ctrl+Right 2", &g(&["brave"]));
        assert!(matches!(bad, Some(Err(_))));
    }

    #[test]
    fn selection_and_line_predicates() {
        assert_eq!(
            check(
                Target::Gtk4Text,
                false,
                "Shift+Down",
                &g(&["one two selected"])
            ),
            Some(Ok(()))
        );
        assert!(matches!(
            check(Target::Gtk4Text, false, "Shift+Down", &g(&["one two"])),
            Some(Err(_))
        ));
        assert_eq!(
            check(
                Target::Web(WebField::Editable),
                false,
                "Shift+Down",
                &g(&["one two"])
            ),
            Some(Ok(())),
            "a selection across blocks is not announced in a contenteditable"
        );
        assert_eq!(
            check(Target::Gtk3Text, false, "Up", &g(&["lima mike november"])),
            Some(Ok(()))
        );
        assert!(matches!(
            check(Target::Gtk3Text, false, "Up", &g(&["not in the text"])),
            Some(Err(_))
        ));
    }
}
