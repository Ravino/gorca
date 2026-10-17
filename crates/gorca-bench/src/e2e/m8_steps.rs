//! The M8 scenario table: one corpus page plus its scripted steps per scenario.
//!
//! Expected speech is NVDA's, adapted (docs/plan/m8-spec.md names the rule or source line for
//! every row). Tokens are matched lower-cased and in any order, so "heading level 2 Products" and
//! "Products heading level 2" are the same expectation; the words are the contract.
//!
//! A step has a core (`say`: what must be spoken) and optional parity extras (`extra`: the words
//! NVDA also speaks that gorca does not yet, with the reason). Core failing = FAIL; extras missing
//! = GAP. `xf("...")` marks a step whose feature is declared but not built. Failures found by the
//! baseline run are marked in `m8_baseline.rs`, not here: this file states what NVDA does.
//!
//! Kept out of rustfmt on purpose: one step per line is what makes the table reviewable.
#![cfg_attr(rustfmt, rustfmt::skip)]

use super::{Act, Body, Gate, P0, P1, P2, Scenario, Step, role};
use crate::e2e::m1::K;

const SHIFT: u32 = 0x01;
const CTRL: u32 = 0x04;
const ALT: u32 = 0x08;
const CTRL_ALT: u32 = CTRL | ALT;

const fn key(code: u16, sym: u32) -> K {
    K { code, sym }
}

/// A letter key (US layout XKB codes).
const fn ch(c: char) -> K {
    let code = match c {
        'q' => 24, 'w' => 25, 'e' => 26, 'r' => 27, 't' => 28, 'y' => 29, 'u' => 30, 'i' => 31, 'o' => 32, 'p' => 33,
        'a' => 38, 's' => 39, 'd' => 40, 'f' => 41, 'g' => 42, 'h' => 43, 'j' => 44, 'k' => 45, 'l' => 46,
        'z' => 52, 'x' => 53, 'c' => 54, 'v' => 55, 'b' => 56, 'n' => 57, 'm' => 58, ',' => 59,
        _ => 0,
    };
    key(code, c as u32)
}

/// A letter pressed forward: `n('h')`.
fn n(c: char) -> Act {
    Act::Key(ch(c), 0)
}

/// A letter pressed backward (Shift): `p('h')` is Shift+H.
fn p(c: char) -> Act {
    let k = ch(c);
    Act::Key(key(k.code, c.to_ascii_uppercase() as u32), SHIFT)
}

/// A heading level key, forward: `lvl(2)`.
fn lvl(d: u32) -> Act {
    Act::Key(key(9 + d as u16, 0x30 + d), 0)
}

/// A heading level key, backward: Shift+digit produces the shifted symbol.
fn plvl(d: u32) -> Act {
    let sym = [0x21, 0x40, 0x23, 0x24, 0x25, 0x5e][(d - 1) as usize];
    Act::Key(key(9 + d as u16, sym), SHIFT)
}

fn rep(c: char, times: u32) -> Act {
    Act::Repeat(ch(c), 0, times)
}

const DOWN: K = key(116, 0xff54);
const UP: K = key(111, 0xff52);
const LEFT: K = key(113, 0xff51);
const RIGHT: K = key(114, 0xff53);
const HOME: K = key(110, 0xff50);
const END: K = key(115, 0xff57);
const PGUP: K = key(112, 0xff55);
const PGDN: K = key(117, 0xff56);
const ENTER: K = key(36, 0xff0d);
const ESC: K = key(9, 0xff1b);
const SPACE: K = key(65, 0x20);
const TAB: K = key(23, 0xff09);
const INSERT: K = key(118, 0xff63);
const F7: K = key(73, 0xffc4);
const KEY_A: K = ch('a');
const KEY_C: K = ch('c');
const KEY_F: K = ch('f');
const COMMA_SHIFT: K = key(59, 0x3c);

fn key0(k: K) -> Act {
    Act::Key(k, 0)
}

fn ctrl(k: K) -> Act {
    Act::Key(k, CTRL)
}

fn ctrl_alt(k: K) -> Act {
    Act::Key(k, CTRL_ALT)
}

fn gorca(k: K) -> Act {
    Act::Chord(INSERT, k, 0)
}

fn gorca_ctrl_alt(k: K) -> Act {
    Act::Chord(INSERT, k, CTRL_ALT)
}

fn s(id: &'static str, prio: u8, what: &'static str, act: Act, say: &'static [&'static str]) -> Step {
    Step::new(id, prio, what, act, say)
}

fn top(id: &'static str) -> Step {
    Step::setup(id, "cursor to the top of the page", Act::Top)
}

/// A positioning key that is not judged.
fn pos(id: &'static str, act: Act) -> Step {
    Step::setup(id, "position the cursor", act)
}

/// Escape as a setup step: back to browse mode before the next focus-mode target (a mode word is
/// only spoken on a change).
fn esc(id: &'static str) -> Step {
    pos(id, key0(ESC))
}

// Reasons repeated across rows.
const NOT_SELECTED: &str = "NVDA says not selected for an unselected tree item that takes the focus; Blink does not report SELECTABLE and FOCUSABLE together on it";
const NAME_OMITTED: &str = "the accessible name of a labelled form control is not spoken on quick nav: the label is separate text outside the control's range and the presenter speaks the range only (LP-1, LP-4)";
const NO_ROLE_WORD: &str = "the role word and count of a container are not spoken when the cursor lands on it (field diff, LP-2: M7b)";
const NO_LANDMARK_NAME: &str = "a landmark hit is presented as the content at its start; its name and role word are not spoken (BM-3/LP-2 field diff: M7b)";

// ---------------------------------------------------------------------------------------------
// Quick navigation

fn qn() -> Vec<Step> {
    vec![
        // headings
        top("QN.top"),
        s("QN.h.1", P0, "H: next heading, level and text", n('h'), &["heading level 2", "Heading two first"]).gate(Gate::Key),
        s("QN.h.2", P0, "H again: next level", n('h'), &["heading level 3", "Heading three first"]).gate(Gate::Key),
        s("QN.h.3", P0, "H x3 reaches level 6", rep('h', 3), &["heading level 6", "Heading six first"]),
        s("QN.h.4", P0, "Shift+H: previous heading", p('h'), &["heading level 5", "Heading five first"]).gate(Gate::Key),
        top("QN.top2"),
        s("QN.hl.2", P0, "2: next level-2 heading", lvl(2), &["heading level 2", "Heading two first"]).gate(Gate::Key),
        s("QN.hl.2b", P0, "2 again: skips levels 3 to 6", lvl(2), &["heading level 2", "Heading two second"]),
        s("QN.hl.2c", P0, "2 at the last level-2 heading: no wrap", lvl(2), &["no next heading", "2"]),
        top("QN.top3"),
        s("QN.hl.3", P1, "3: next level-3 heading", lvl(3), &["heading level 3", "Heading three first"]),
        s("QN.hl.4", P1, "4: next level-4 heading", lvl(4), &["heading level 4", "Heading four first"]),
        s("QN.hl.5", P1, "5: next level-5 heading", lvl(5), &["heading level 5", "Heading five first"]),
        s("QN.hl.6", P1, "6: next level-6 heading", lvl(6), &["heading level 6", "Heading six first"]),
        pos("QN.end", ctrl(END)),
        s("QN.hl.s1", P1, "Shift+1: previous level-1 heading", plvl(1), &["heading level 1", "Quick nav start"]),
        top("QN.top3b"),
        s("QN.h.edge.prev2", P0, "Shift+H before the first heading", p('h'), &["no previous heading"]),
        // links
        top("QN.top4"),
        s("QN.k.1", P0, "K: next link", n('k'), &["link", "Alpha link"]).gate(Gate::Key),
        s("QN.k.2", P0, "K x3: through a block of links", rep('k', 3), &["link", "inline link one"]),
        s("QN.k.3", P0, "Shift+K: previous link", p('k'), &["link", "Gamma link"]).gate(Gate::Key),
        top("QN.top5"),
        s("QN.u.1", P1, "U: next unvisited link", n('u'), &["link", "Alpha link"]),
        s("QN.v.1", P1, "V: no visited links in a fresh profile", n('v'), &["no next visited link"]),
        pos("QN.end2", ctrl(END)),
        s("QN.k.edge.next", P0, "K at the last link: no wrap", n('k'), &["no next link"]),
        top("QN.top6"),
        s("QN.k.edge.prev", P0, "Shift+K before the first link: no wrap", p('k'), &["no previous link"]),
        // form fields (a quick nav onto a field never switches to focus mode)
        top("QN.top7"),
        s("QN.f.1", P0, "F: next form field (edit)", n('f'), &["edit"]).extra(&["Search term"], NAME_OMITTED).not(&["focus mode"]).gate(Gate::Key),
        s("QN.f.2", P0, "F again", n('f'), &["edit"]).extra(&["Email address"], NAME_OMITTED).not(&["focus mode"]),
        s("QN.f.3", P0, "F: check box with state", n('f'), &["check box", "not checked"]).extra(&["Subscribe box"], NAME_OMITTED).not(&["focus mode"]),
        s("QN.f.4", P0, "F: next check box, checked", n('f'), &["check box", "checked"]).extra(&["Terms box"], NAME_OMITTED).not(&["focus mode", "not checked"]),
        top("QN.top8"),
        s("QN.e.1", P0, "E: next edit field skips buttons and boxes", n('e'), &["edit"]).extra(&["Search term"], NAME_OMITTED).not(&["focus mode"]),
        s("QN.e.2", P0, "E again", n('e'), &["edit"]).extra(&["Email address"], NAME_OMITTED),
        s("QN.e.3", P1, "E: check boxes and radios are skipped; the invalid field is next", n('e'), &["edit"]).extra(&["Invalid field"], NAME_OMITTED),
        top("QN.top9"),
        s("QN.x.1", P0, "X: next check box with state", n('x'), &["check box", "not checked"]).extra(&["Subscribe box"], NAME_OMITTED).not(&["focus mode"]),
        s("QN.x.2", P0, "X again", n('x'), &["check box", "checked"]).extra(&["Terms box"], NAME_OMITTED).not(&["not checked"]),
        s("QN.r.1", P0, "R: next radio button with state", n('r'), &["radio button", "not checked"]).extra(&["Small radio"], NAME_OMITTED).not(&["focus mode"]),
        s("QN.r.2", P0, "R again", n('r'), &["radio button", "checked"]).extra(&["Large radio"], NAME_OMITTED).not(&["not checked"]),
        s("QN.c.1", P0, "C: next combo box with value", n('c'), &["combo box"]).extra(&["Country", "Albania"], NAME_OMITTED).not(&["focus mode"]),
        s("QN.c.2", P0, "C again", n('c'), &["combo box"]).extra(&["Colour", "Red"], NAME_OMITTED),
        s("QN.b.1", P0, "B: next button", n('b'), &["button", "Go button"]).not(&["focus mode"]).gate(Gate::Key),
        s("QN.b.2", P0, "B again", n('b'), &["button", "Cancel button"]),
        s("QN.b.3", P2, "B: the toggle button counts as a button", n('b'), &["Toggle button one"]),
        // lists
        top("QN.top10"),
        s("QN.l.1", P0, "L: next list", n('l'), &["Apple item"]).extra(&["list", "3 items"], NO_ROLE_WORD),
        s("QN.l.2", P0, "L again", n('l'), &["First numbered"]).extra(&["list", "2 items"], NO_ROLE_WORD),
        top("QN.top11"),
        s("QN.i.1", P0, "I: next list item", n('i'), &["Apple item"]),
        s("QN.i.2", P0, "I again", n('i'), &["Banana item"]),
        // graphics, quotes, separators
        top("QN.top12"),
        s("QN.g.1", P0, "G: next graphic", n('g'), &["graphic", "Logo graphic"]),
        s("QN.g.2", P1, "G: skips the decorative image (alt empty)", n('g'), &["graphic", "Chart graphic"]),
        top("QN.top13"),
        s("QN.q.1", P0, "Q: next block quote", n('q'), &["Quoted wisdom one"]).extra(&["block quote"], NO_ROLE_WORD),
        s("QN.q.2", P1, "Q again", n('q'), &["Quoted wisdom two"]).extra(&["block quote"], NO_ROLE_WORD),
        top("QN.top14"),
        s("QN.s.1", P1, "S: next separator", n('s'), &["separator"]),
        s("QN.s.2", P1, "S again", n('s'), &["separator"]),
        s("QN.s.3", P1, "S: no third separator", n('s'), &["no next separator"]),
        // landmark, frame, embedded, table
        top("QN.top15"),
        s("QN.d.1", P0, "D: the named form is a landmark", n('d'), &["Search term"]).extra(&["Quick form"], NO_LANDMARK_NAME),
        s("QN.d.2", P0, "D: main", n('d'), &["Inside main"]).extra(&["Main content landmark"], NO_LANDMARK_NAME),
        s("QN.d.3", P0, "D: complementary", n('d'), &["Inside aside"]).extra(&["Side note landmark"], NO_LANDMARK_NAME),
        s("QN.d.4", P1, "D: no more landmarks", n('d'), &["no next landmark"]),
        top("QN.top16"),
        s("QN.m.1", P1, "M: next frame", n('m'), &["frame", "Embedded frame one"]),
        s("QN.m.2", P1, "M again", n('m'), &["frame", "Embedded frame two"]),
        top("QN.top17"),
        s("QN.o.1", P1, "O: next embedded object (application)", n('o'), &["Calculator widget"]),
        s("QN.o.2", P1, "O again (object element)", n('o'), &["Plain object"]).xf("the <object> with text/plain data is not found by O: Firefox gives it no embedded node in the mirror (not investigated)"),
        top("QN.top18"),
        s("QN.t.1", P0, "T: next table", n('t'), &["Fruit table"]).extra(&["table", "2 columns", "3 rows"], NO_ROLE_WORD).gate(Gate::Key),
        s("QN.t.2", P0, "T again", n('t'), &["Veg table"]).extra(&["table"], NO_ROLE_WORD),
        s("QN.t.3", P1, "T: no more tables", n('t'), &["no next table"]),
        // the keys gorca has not built yet
        top("QN.top19"),
        s("QN.n.1", P1, "N: skip past a block of links", n('n'), &["Text after the link block"]),
        top("QN.a.1.t"),
        s("QN.a.1", P2, "A: next annotation (inserted text)", n('a'), &["Inserted words"]),
        top("QN.w.1.t"),
        s("QN.w.1", P2, "W: next error", n('w'), &["misspeled"]).xf("aria-invalid=spelling is a text attribute in Firefox (invalid:spelling), the mirror carries node states only; W finds nodes in an invalid state, form fields excluded"),
        top("QN.p.1.t"),
        s("QN.p.1", P2, "P: next text paragraph", n('p'), &["Alpha link"]),
        // containers (comma / Shift+comma)
        top("QN.top20"),
        pos("QN.list.enter", n('l')),
        s("QN.comma.1", P1, "Comma: past the end of the list", n(','), &["First numbered"]),
        s("QN.comma.2", P1, "Shift+Comma: back to the start of the container", Act::Key(COMMA_SHIFT, SHIFT), &["First numbered"]),
        top("QN.top21"),
        s("QN.comma.3", P2, "Comma outside any container", n(','), &["Not in a container"]),
        // single-letter navigation toggle (NVDA+Shift+Space)
        s("QN.sl.off", P1, "gorca+Shift+Space: single letter navigation off", Act::Chord(INSERT, SPACE, SHIFT), &["single letter navigation off"]),
        s("QN.sl.key", P1, "H with it off reaches the page", n('h'), &[]).passes_through(),
        s("QN.sl.on", P1, "gorca+Shift+Space: back on", Act::Chord(INSERT, SPACE, SHIFT), &["single letter navigation on"]),
    ]
}

// ---------------------------------------------------------------------------------------------
// Tables

fn tb() -> Vec<Step> {
    vec![
        top("TB.top"),
        s("TB.t.1", P0, "T: next table, named by its caption", n('t'), &["Quarterly sales"]).extra(&["table", "4 columns", "4 rows"], NO_ROLE_WORD),
        s("TB.into", P0, "Down: T lands on the caption line (the first line of the table, as in NVDA); the next line is the header row, one line (screen layout on, NVDA default)", Act::Repeat(DOWN, 0, 1), &["Region"]).extra(&["Q1", "Q2", "Q3"], "every table cell is its own line; NVDA reads a table row as one line with screen layout on"),
        s("TB.right.1", P0, "Ctrl+Alt+Right: next cell", ctrl_alt(RIGHT), &["Q1"]).gate(Gate::Key),
        s("TB.right.2", P0, "Ctrl+Alt+Right again", ctrl_alt(RIGHT), &["Q2"]),
        s("TB.down.1", P0, "Ctrl+Alt+Down: next row, the row header changes and is spoken", ctrl_alt(DOWN), &["North", "20"]).gate(Gate::Key),
        s("TB.down.2", P0, "Ctrl+Alt+Down again", ctrl_alt(DOWN), &["South", "21"]),
        s("TB.up.1", P0, "Ctrl+Alt+Up: previous row", ctrl_alt(UP), &["North", "20"]),
        s("TB.left.1", P0, "Ctrl+Alt+Left: previous column, the column header changes", ctrl_alt(LEFT), &["Q1", "10"]),
        s("TB.left.2", P0, "Ctrl+Alt+Left onto the row header cell", ctrl_alt(LEFT), &["North"]),
        s("TB.edge.left", P0, "Ctrl+Alt+Left at the first column", ctrl_alt(LEFT), &["Edge of table"]),
        s("TB.last.col", P1, "Ctrl+Alt+End: last column", ctrl_alt(END), &["30"]),
        s("TB.edge.right", P1, "Ctrl+Alt+Right at the last column", ctrl_alt(RIGHT), &["Edge of table"]),
        s("TB.last.row", P1, "Ctrl+Alt+PageDown: last row", ctrl_alt(PGDN), &["East", "32"]),
        s("TB.edge.down", P1, "Ctrl+Alt+Down at the last row", ctrl_alt(DOWN), &["Edge of table"]),
        s("TB.first.row", P1, "Ctrl+Alt+PageUp: first row", ctrl_alt(PGUP), &["Q3"]),
        s("TB.first.col", P1, "Ctrl+Alt+Home: first column", ctrl_alt(HOME), &["Region"]),
        s("TB.say.row", P2, "gorca+Ctrl+Alt+Left: speak the whole row", gorca_ctrl_alt(LEFT), &["Region", "Q1", "Q2", "Q3"]),
        s("TB.say.col", P2, "gorca+Ctrl+Alt+Up: speak the whole column", gorca_ctrl_alt(UP), &["Region", "North", "South", "East"]),
        top("TB.top2"),
        s("TB.notcell", P1, "Table keys outside a table", ctrl_alt(DOWN), &["Not in a table cell"]),
        // spans, layout tables, grids, sparse tables
        s("TB.t.2", P0, "T twice from the top: the span table", rep('t', 2), &["Span table"]).extra(&["table"], NO_ROLE_WORD),
        s("TB.span.hdr", P2, "Down: from the caption line to the first header row", Act::Repeat(DOWN, 0, 1), &["Item"]).extra(&["Price"], "every table cell is its own line"),
        s("TB.t.3", P0, "T: the layout table is skipped, the grid is next", n('t'), &["Seat"]).extra(&["Seat grid"], NO_ROLE_WORD).not(&["Layout cell"]),
        s("TB.t.4", P1, "T: sparse table", n('t'), &["Sparse table"]),
        s("TB.t.5", P1, "T: one-cell table", n('t'), &["One cell"]),
        top("TB.top3"),
        s("TB.layout.walk", P0, "A layout table is read as plain text", Act::Repeat(DOWN, 0, 45), &["Layout cell left", "Layout cell right"]).win(6500),
        s("TB.grid.focus", P1, "Focus on an ARIA grid cell switches to focus mode", Act::Focus(role::TABLE_CELL, "free"), &["focus mode"]),
    ]
}

// ---------------------------------------------------------------------------------------------
// Forms and mode switching

fn fm() -> Vec<Step> {
    vec![
        s("FM.link", P0, "Focus on a link: stays in browse mode", Act::Focus(role::LINK, "First link"), &["link", "First link"]).not(&["focus mode"]),
        // the first focus into an edit of a fresh page is a mirror miss (34 to 121 ms measured on a loaded host):
        // reported as FM.edit.cold, not gated; the warm repeat is the FM1 case
        s("FM.edit.cold", P0, "First focus on an edit of a fresh page: auto focus mode (cold mirror)", Act::Focus(role::ENTRY, "Full name"), &["focus mode", "Full name"]),
        s("FM.esc.cold", P0, "Escape: back to browse mode", key0(ESC), &["browse mode"]),
        pos("FM.warm.link", Act::Focus(role::LINK, "First link")),
        s("FM.edit", P0, "Focus on the same edit again: auto focus mode (warm)", Act::Focus(role::ENTRY, "Full name"), &["focus mode", "Full name"]).gate(Gate::Mode),
        s("FM.esc", P0, "Escape: back to browse mode", key0(ESC), &["browse mode"]).gate(Gate::Mode),
        s("FM.toggle.on", P0, "gorca+Space in browse mode: sticky focus mode", Act::Chord(INSERT, SPACE, 0), &["focus mode"]).gate(Gate::Mode),
        s("FM.toggle.off", P0, "gorca+Space: back to browse mode", Act::Chord(INSERT, SPACE, 0), &["browse mode"]).gate(Gate::Mode),
        // every focus-mode target is entered from browse mode: a mode word is only spoken on a change
        s("FM.pw", P0, "Password field: focus mode, the value is never spoken", Act::Focus(role::PASSWORD_TEXT, "Secret"), &["focus mode", "Secret"]).not(&["hunter2"]),
        esc("FM.e1"),
        s("FM.textarea", P0, "Multi-line edit: focus mode", Act::Focus(role::ENTRY, "Notes"), &["focus mode", "Notes"]),
        esc("FM.e2"),
        s("FM.readonly", P1, "Read-only edit: focus mode and the state", Act::Focus(role::ENTRY, "Read only"), &["focus mode"]).extra(&["read only"], "the read-only state word is not spoken yet"),
        esc("FM.e3"),
        s("FM.required", P1, "Required field: state and description", Act::Focus(role::ENTRY, "Required"), &["Required"]).extra(&["required", "Help text for required"], "states and descriptions are M3/M7b polish"),
        esc("FM.e4"),
        s("FM.invalid", P1, "Invalid field: state and error message", Act::Focus(role::ENTRY, "Invalid"), &["Invalid"]).extra(&["invalid", "Error text here"], "states and descriptions are M3/M7b polish"),
        esc("FM.e5"),
        s("FM.cb", P0, "Check box focus: stays in browse mode, state spoken", Act::Focus(role::CHECK_BOX, "Gift wrap"), &["check box", "Gift wrap"]).extra(&["not checked"], "the unchecked state is not spoken on focus").not(&["focus mode"]),
        s("FM.btn", P0, "Button focus: stays in browse mode", Act::Focus(role::PUSH_BUTTON, "Save profile"), &["button", "Save profile"]).not(&["focus mode"]),
        s("FM.radio", P1, "Radio focus: NVDA switches to focus mode (SWITCH_TO_PASS_THROUGH_ON_FOCUS_ROLES); ADR-0003 P(n) says browse", Act::Focus(role::RADIO_BUTTON, "Post"), &["radio button", "Post", "focus mode"]),
        esc("FM.e6"),
        s("FM.select", P0, "Combo box (select) focus: focus mode", Act::Focus(role::COMBO_BOX, "Size"), &["focus mode", "Size"]),
        esc("FM.e7"),
        s("FM.listbox", P1, "List box focus: focus mode", Act::Focus(role::ANY, "Toppings choices"), &["focus mode", "Toppings"]),
        esc("FM.e8"),
        s("FM.slider", P1, "Slider focus: focus mode", Act::Focus(role::SLIDER, "Level"), &["focus mode", "Level"]),
        esc("FM.e9"),
        s("FM.spin", P1, "Spin button focus: focus mode", Act::Focus(role::SPIN_BUTTON, "Quantity"), &["focus mode", "Quantity"]),
        esc("FM.e10"),
        s("FM.aria.combo", P1, "ARIA combo box focus: focus mode", Act::Focus(role::COMBO_BOX, "Fruit combobox"), &["focus mode", "Fruit combobox"]),
        esc("FM.e11"),
        s("FM.aria.slider", P2, "ARIA slider focus: focus mode", Act::Focus(role::SLIDER, "Aria slider"), &["focus mode"]),
        esc("FM.e12"),
        s("FM.tree", P1, "Tree focus: focus mode", Act::Focus(role::TREE, "File tree"), &["focus mode", "File tree"]),
        esc("FM.e13"),
        s("FM.ce", P1, "Rich editor (contenteditable): focus mode", Act::Focus(role::ENTRY, "Rich editor"), &["focus mode", "Rich editor"]),
        // focus mode: letters reach the control
        esc("FM.e14b"),
        s("FM.letters", P0, "In focus mode letters are not grabbed", Act::Focus(role::ENTRY, "Full name"), &["focus mode"]),
        s("FM.letters.key", P0, "H is not consumed: it reaches the field", n('h'), &[]).passes_through(),
        s("FM.letters.tab", P0, "Tab is not consumed in focus mode: the page's own tab order (R-6)", key0(TAB), &[]).passes_through(),
        s("FM.letters.stab", P0, "Shift+Tab is not consumed in focus mode", Act::Key(TAB, SHIFT), &[]).passes_through(),
        s("FM.esc2", P0, "Escape again", key0(ESC), &["browse mode"]).gate(Gate::Mode),
        // keys that must reach the page in browse mode (NVDA's passThrough gesture list)
        // Tab is no longer in this list: D-2 grabs it in browse mode while a focusable follows the
        // cursor (SY.tab; SY.tab.out covers the released case); Shift+Tab likewise (SY.stab).
        s("FM.pass.ctrl_enter", P0, "Ctrl+Enter is not grabbed", Act::Key(ENTER, CTRL), &[]).passes_through(),
        s("FM.pass.shift_f10", P1, "Shift+F10 is not grabbed", Act::Key(key(76, 0xffc7), SHIFT), &[]).passes_through(),
        // a real Tab through the compositor (not the emulated Mutter): focus follows
        pos("FM.click", Act::Click),
        s("FM.tab.real.0", P1, "Focus on the first link", Act::Focus(role::LINK, "First link"), &["First link"]),
        s("FM.tab.real", P1, "Tab from a link onto the first edit: focus mode", Act::AppKeys(&[15]), &["focus mode", "Full name"]).win(1500),
        esc("FM.e15"),
        // browse-mode activation
        top("FM.top"),
        s("FM.x", P0, "X: the check box", n('x'), &["check box", "not checked"]).extra(&["Gift wrap"], NAME_OMITTED),
        s("FM.space", P0, "Space on a check box toggles it, the new state is spoken", key0(SPACE), &["checked"]).not(&["not checked"]),
        s("FM.space2", P0, "Space again toggles back", key0(SPACE), &["not checked"]),
        top("FM.top2"),
        s("FM.e", P0, "E: next edit", n('e'), &["edit", "Ada"]).extra(&["Full name"], NAME_OMITTED).not(&["focus mode"]),
        s("FM.enter", P0, "Enter on the edit: focus mode", key0(ENTER), &["focus mode"]).gate(Gate::Mode),
        s("FM.esc4", P0, "Escape", key0(ESC), &["browse mode"]),
        s("FM.btn.key", P0, "B: next button", n('b'), &["button", "Save profile"]),
        // last, because a sticky focus mode would hide the mode words of any step after it
        esc("FM.e14"),
        s("FM.app", P1, "role=application: focus mode that Escape does not leave", Act::Focus(role::ANY, "Whiteboard app"), &["focus mode"]),
        s("FM.app.esc", P1, "Escape inside an application passes through; only the toggle leaves", key0(ESC), &[]).not(&["browse mode"]),
        s("FM.app.toggle", P1, "gorca+Space leaves an application", Act::Chord(INSERT, SPACE, 0), &["browse mode"]),
    ]
}

fn af() -> Vec<Step> {
    vec![
        s("AF.field", P0, "Focus on the field: focus mode", Act::Focus(role::ENTRY, "Search"), &["focus mode", "Search"]),
        s("AF.script", P0, "A page script moves focus to a link: back to browse mode", Act::Do(role::PUSH_BUTTON, "Move focus to the link"), &["browse mode", "Scripted target link"]).win(1200),
        s("AF.script.again", P1, "Focus back to the field by AT-SPI: focus mode again", Act::Focus(role::ENTRY, "Search"), &["focus mode"]).gate(Gate::Mode),
    ]
}

// ---------------------------------------------------------------------------------------------
// Landmarks

fn lm() -> Vec<Step> {
    vec![
        top("LM.top"),
        s("LM.d.1", P0, "D: banner", n('d'), &["banner"]).gate(Gate::Key),
        s("LM.d.2", P0, "D: named navigation", n('d'), &["Home link"]).extra(&["navigation", "Primary navigation"], NO_LANDMARK_NAME),
        s("LM.d.3", P0, "D: search", n('d'), &["Search"]).extra(&["search", "Site search"], NO_LANDMARK_NAME),
        s("LM.d.4", P0, "D: second navigation", n('d'), &["Top crumb"]).extra(&["navigation", "Breadcrumb"], NO_LANDMARK_NAME),
        s("LM.d.5", P0, "D: main", n('d'), &["main"]),
        s("LM.d.6", P1, "D: named region", n('d'), &["Region heading"]).extra(&["region", "Named region one"], NO_LANDMARK_NAME),
        s("LM.d.7", P0, "D: an unnamed section is skipped; the nested aside is next", n('d'), &["Nested complementary"]).extra(&["Pull quote"], NO_LANDMARK_NAME).not(&["Unnamed section"]),
        s("LM.d.8", P1, "D: region labelled by a heading", n('d'), &["Labelled by heading"]),
        s("LM.d.9", P1, "D: named form", n('d'), &["Comment"]).extra(&["Feedback form"], NO_LANDMARK_NAME),
        s("LM.d.10", P1, "D: an unnamed form is skipped; the aside is next", n('d'), &["Related heading"]).extra(&["Related links"], NO_LANDMARK_NAME).not(&["Unnamed form"]),
        s("LM.d.11", P1, "D: contentinfo (the footer)", n('d'), &["Footer text"]),
        s("LM.d.12", P1, "D: footer navigation", n('d'), &["Privacy link"]).extra(&["Footer navigation"], NO_LANDMARK_NAME),
        s("LM.d.13", P0, "D: no wrap", n('d'), &["no next landmark"]),
        s("LM.d.prev", P0, "Shift+D from inside the footer navigation: the enclosing contentinfo", p('d'), &["Footer text"]).win(900),
        s("LM.d.prev2", P1, "Shift+D again: the aside before the footer", p('d'), &["Related heading"]),
        top("LM.top2"),
        s("LM.d.edge", P1, "Shift+D before the first landmark", p('d'), &["no previous landmark"]),
        top("LM.top3"),
        s("LM.skip.enter", P1, "Enter on the skip link (the first line): main takes focus and the cursor lands in it", key0(ENTER), &["Main heading"]).win(1200),
    ]
}

// ---------------------------------------------------------------------------------------------
// Live regions

fn lv() -> Vec<Step> {
    vec![
        s("LV.polite", P0, "aria-live=polite: the new text is spoken", Act::Do(role::PUSH_BUTTON, "Run polite"), &["Polite message"]).gate(Gate::Live).win(1500),
        s("LV.assertive", P0, "aria-live=assertive: spoken", Act::Do(role::PUSH_BUTTON, "Run assertive"), &["Assertive message"]).gate(Gate::Live).win(1500),
        s("LV.alert", P0, "role=alert added: spoken at once", Act::Do(role::PUSH_BUTTON, "Run alert"), &["Alert: disk almost full"]).gate(Gate::Live).win(1500),
        s("LV.status", P0, "role=status text change", Act::Do(role::PUSH_BUTTON, "Run status"), &["Saved at step"]).gate(Gate::Live).win(1500),
        s("LV.log", P0, "role=log: only the appended entry", Act::Do(role::PUSH_BUTTON, "Run log"), &["Log entry"]).gate(Gate::Live).win(1500),
        s("LV.log2", P1, "role=log again: one utterance, not the whole log", Act::Do(role::PUSH_BUTTON, "Run log"), &["Log entry"]).max(1).win(1500),
        s("LV.atomic", P1, "aria-atomic: the whole region is spoken for one changed part", Act::Do(role::PUSH_BUTTON, "Run atomic"), &["Ticker", "points"]).win(1500),
        s("LV.busy", P1, "aria-busy: only the final text, once", Act::Do(role::PUSH_BUTTON, "Run busy"), &["Busy final text"]).not(&["Busy part one"]).win(2200),
        s("LV.off", P0, "aria-live=off: nothing", Act::Do(role::PUSH_BUTTON, "Run off"), &[]).not(&["Silent update"]).win(900),
        s("LV.remove", P2, "aria-relevant=all: a removal is spoken (ADR-0003 LR-2; NVDA ignores removals)", Act::Do(role::PUSH_BUTTON, "Run remove"), &["Removable text A"]).win(1200),
        s("LV.same", P1, "Same text again: spoken once", Act::Do(role::PUSH_BUTTON, "Run same"), &["Same text again"]).win(1200),
        s("LV.same2", P1, "Same text a second time within 10 s: suppressed (dedupe)", Act::Do(role::PUSH_BUTTON, "Run same"), &[]).not(&["Same text again"]).win(1200),
        s("LV.new", P1, "A region that appears with text: the existing text is not spoken, a later change is", Act::Do(role::PUSH_BUTTON, "Run new region"), &["Text added after the region exists"]).not(&["Text present when the region appears"]).win(1800),
        s("LV.hidden", P2, "A hidden region changes: nothing", Act::Do(role::PUSH_BUTTON, "Run hidden"), &[]).not(&["Hidden region changed"]).win(900),
        s("LV.flood", P0, "200 polite updates in 1 s: bounded speech (bucket 1/s burst 3, 4/s per document)", Act::Do(role::PUSH_BUTTON, "Run flood polite"), &["Flood update"]).max(8).win(2600),
        s("LV.flood.a", P1, "200 assertive updates in 1 s: bounded speech (2/s burst 3)", Act::Do(role::PUSH_BUTTON, "Run flood assertive"), &["Assertive flood"]).max(10).win(2600),
        Step::setup("LV.after", "let the flood end", Act::Wait(1500)),
        s("LV.key", P0, "Down after a flood: the reader still answers", key0(DOWN), &[]).min(1).gate(Gate::Key).win(600),
    ]
}

// ---------------------------------------------------------------------------------------------
// iframes

fn iframes() -> Vec<Step> {
    vec![
        top("IF.top"),
        s("IF.walk", P0, "Down through the outer page into the first frame: its text is in the same reading flow", Act::Repeat(DOWN, 0, 6), &["Level one heading"]).win(3000),
        top("IF.top2"),
        s("IF.h.deep", P0, "H x4 reaches the heading of the fourth nested frame", rep('h', 4), &["heading level 1", "Level four heading"]).win(2500),
        s("IF.h.form", P1, "H: the form frame's heading follows", n('h'), &["Frame form heading"]),
        s("IF.h.srcdoc", P1, "H: the srcdoc frame's heading", n('h'), &["Srcdoc heading"]),
        top("IF.top3"),
        s("IF.f.1", P0, "F x2: the deepest frame's button, then a field in another frame", rep('f', 2), &["edit"]).extra(&["Frame field"], NAME_OMITTED).not(&["focus mode"]).win(1500),
        s("IF.b.1", P1, "B: the frame's button", n('b'), &["Frame submit"]),
        s("IF.focus", P0, "Focus on the field in the frame: focus mode", Act::Focus(role::ENTRY, "Frame field"), &["focus mode", "Frame field"]),
        s("IF.esc", P1, "Escape in the frame: browse mode", key0(ESC), &["browse mode"]),
        top("IF.top4"),
        s("IF.m.1", P1, "M: next frame, named by its title", n('m'), &["frame", "Nested level one"]),
        s("IF.m.2", P1, "M again", n('m'), &["frame", "Level two frame"]),
        s("IF.end", P1, "Ctrl+End: the last line of the outer page after all frames", ctrl(END), &["Outer paragraph after the frames"]),
        s("IF.late", P2, "A frame reloads its content: no speech", Act::Do(role::PUSH_BUTTON, "Reload late frame"), &[]).not(&["After reload text"]).win(900),
        s("IF.late2", P2, "The page is still navigable", ctrl(HOME), &["Outer page heading"]),
        // Tab from the top of a page with nested frames (R-4, R-5): our first Tab lands on a real
        // control, never on a frame document; after that focus moved the cursor, so the
        // browser's own tab order takes over (NVDA #5227).
        top("IF.tab.top"),
        s("IF.tab.1", P1, "Tab from the top: the first link, not a frame document", key0(TAB), &["link"]).not(&["document"]).win(1500),
        s("IF.tab.2", P1, "Tab after a focus-driven move is the browser's (not consumed)", key0(TAB), &[]).passes_through().win(1500),
        s("IF.tab.3", P1, "Tab again: still the browser's", key0(TAB), &[]).passes_through().win(1500),
        s("IF.stab", P1, "Shift+Tab after a focus-driven move is the browser's", Act::Key(TAB, SHIFT), &[]).passes_through().win(1500),
    ]
}

// ---------------------------------------------------------------------------------------------
// aria-hidden and presentational content

fn hd() -> Vec<Step> {
    vec![
        top("HD.top"),
        s("HD.walk", P0, "Reading the whole page: hidden content never appears, hidden-from-view-but-exposed content does", Act::Repeat(DOWN, 0, 30), &["START marker", "Visible after hidden", "SCREEN-READER-ONLY", "OFFSCREEN-TEXT", "Presentational list item one", "None-role list item", "Presentational heading text", "DECOR-IMG-SHOULD-NOT-BE-SKIPPED", "END marker"])
            .not(&["ARIA-HIDDEN-TEXT", "DISPLAY-NONE-TEXT", "VISIBILITY-HIDDEN-TEXT", "HIDDEN-ATTR-TEXT", "HIDDEN-LINK", "HIDDEN-BUTTON", "ARIA-HIDDEN-BUTTON", "HIDDEN-INLINE", "list with"])
            .win(4500),
        top("HD.top2"),
        s("HD.k", P0, "K: the link inside aria-hidden is not reachable", n('k'), &["no next link"]),
        s("HD.b", P0, "B: the aria-hidden button is skipped", n('b'), &["Toggle late paragraph"]).not(&["ARIA-HIDDEN-BUTTON", "HIDDEN-BUTTON"]),
        s("HD.h", P1, "H: role=presentation on a heading removes it from navigation", n('h'), &["no next heading"]),
        s("HD.l", P1, "L: role=presentation lists are not lists", n('l'), &["no next list"]),
        s("HD.toggle", P1, "A hidden paragraph is revealed by script", Act::Do(role::PUSH_BUTTON, "Toggle late paragraph"), &[]).win(500),
        top("HD.top3"),
        s("HD.late", P1, "The revealed paragraph is read", Act::Repeat(DOWN, 0, 34), &["LATE-PARAGRAPH"]).win(5000),
    ]
}

// ---------------------------------------------------------------------------------------------
// Same-page anchors

fn an() -> Vec<Step> {
    vec![
        top("AN.top"),
        s("AN.skip", P0, "Enter on the skip link (the first line): main takes focus, the cursor lands in it", key0(ENTER), &["Section one"]).win(1500),
        s("AN.after", P0, "Down after the jump continues from the target, not from the link", key0(DOWN), &["Text of section one"]),
        top("AN.top2"),
        s("AN.toc.1", P0, "K x2: the second table-of-contents link", rep('k', 2), &["Go to section two"]),
        s("AN.toc.go", P0, "Enter: the cursor lands on the heading", key0(ENTER), &["Section two"]).win(1500),
        top("AN.top3"),
        s("AN.para", P0, "Link to a paragraph id", rep('k', 3), &["Go to paragraph three"]),
        s("AN.para.go", P0, "Enter: the paragraph is spoken", key0(ENTER), &["Paragraph three is the target"]).win(1500),
        top("AN.top4"),
        s("AN.named", P1, "Link to a named anchor", rep('k', 4), &["Go to named anchor"]),
        s("AN.named.go", P1, "Enter: the text after the anchor is spoken", key0(ENTER), &["Text after a named anchor"]).win(1500),
        top("AN.top5"),
        s("AN.foc", P0, "Link to a focusable div", rep('k', 5), &["Go to focusable target"]),
        s("AN.foc.go", P0, "Enter: focus moves to the div, the cursor follows", key0(ENTER), &["focusable div target"]).win(1500),
        top("AN.top6"),
        s("AN.miss", P1, "Link to an id that does not exist", rep('k', 6), &["Go to missing target"]),
        s("AN.miss.go", P1, "Enter: nothing breaks, no error is spoken", key0(ENTER), &[]).not(&["error", "failed", "exception"]).win(900),
        s("AN.alive", P1, "The cursor still works", key0(DOWN), &[]).min(1).gate(Gate::Key).win(700),
    ]
}

// ---------------------------------------------------------------------------------------------
// Focus and caret sync

fn sy() -> Vec<Step> {
    vec![
        top("SY.top"),
        s("SY.focus", P0, "Focus on a link by AT-SPI (what Tab ends in): spoken", Act::Focus(role::LINK, "link one"), &["link", "link one"]),
        s("SY.follow", P0, "The browse cursor follows focus: Down continues after the link's line", key0(DOWN), &["Second paragraph"]),
        s("SY.script", P0, "A script moves focus to a link: the cursor follows and it is spoken", Act::Do(role::PUSH_BUTTON, "Script focus link three"), &["Link three"]).win(1200),
        s("SY.script.field", P0, "A script moves focus into a field: focus mode", Act::Do(role::PUSH_BUTTON, "Script focus field"), &["focus mode", "Sync field"]).win(1200),
        s("SY.esc", P0, "Escape", key0(ESC), &["browse mode"]),
        top("SY.top2"),
        s("SY.nomove", P1, "Moving the browse cursor never moves system focus (BM-2)", Act::Repeat(DOWN, 0, 6), &[]).no_focus().win(1200),
        top("SY.top5"),
        s("SY.nomove.k", P1, "K onto a link: reading never focuses it or sets the page caret (BM-2, review H-1)", n('k'), &["link one"]).no_focus().win(1500),
        s("SY.nomove.sel", P1, "Shift+Right inside the link: the virtual selection never touches the page", Act::Key(RIGHT, SHIFT), &["selected"]).no_focus().win(1500),
        top("SY.top4"),
        s("SY.far", P2, "Ctrl+End: the far paragraph after a long spacer", ctrl(END), &["Far paragraph at the bottom"]),
        s("SY.back", P2, "Ctrl+Home back", ctrl(HOME), &["Focus and caret sync"]),
        // last: the real Tab leaves the page in this rig and the reader is not in the document afterwards
        // Tab starts from the browse cursor (NVDA _tabOverride, browseMode.py:2121)
        top("SY.top3"),
        s("SY.down2", P1, "Cursor onto the second paragraph", Act::Repeat(DOWN, 0, 2), &["Second paragraph"]),
        s("SY.tab", P1, "Tab from the cursor: the next focusable after it, not the first on the page (D-2)", key0(TAB), &["link two"]).win(1500),
        s("SY.tab2", P1, "Tab again after the focus-driven move is the browser's, not consumed (R-5, NVDA #5227)", key0(TAB), &[]).passes_through().win(800),
        s("SY.btn", P1, "Firefox's own Tab ended on the button: the cursor follows focus", Act::Focus(role::PUSH_BUTTON, "Plain button"), &["Plain button"]).win(800),
        s("SY.up3", P1, "Up: the cursor moves off the focused button, Tab is ours again", key0(UP), &[]).win(800),
        Step::setup("SY.up3.wait", "the grab set follows the settled cursor", Act::Wait(500)),
        s("SY.stab", P1, "Shift+Tab: the previous focusable before the cursor", Act::Key(TAB, SHIFT), &["link"]).not(&["Plain button"]).win(1500),
        // last: with nothing focusable after the cursor Tab is not grabbed and reaches the page,
        // which in this rig leaves the document
        s("SY.end", P1, "Ctrl+End: past the last focusable", ctrl(END), &["Far paragraph at the bottom"]),
        Step::setup("SY.end.wait", "the grab set follows the settled cursor", Act::Wait(500)),
        s("SY.tab.out", P1, "Tab with no focusable after the cursor is not consumed (D-2 pass-through)", key0(TAB), &[]).passes_through(),
        // A held Tab across the grab flip its own focus move causes (R-1): Mutter swallows the
        // release of a key whose grab was added while it was down.
        s("SY.hold.down", P1, "Tab down past the last focusable (passes)", Act::PrbDown(TAB, 0), &[]).passes_through().win(300),
        s("SY.hold.ffocus", P1, "Firefox's native Tab focuses the button", Act::Focus(role::PUSH_BUTTON, "Plain button"), &["Plain button"]).win(500),
        s("SY.hold.rep", P1, "Mutter autorepeat of the held Tab is not consumed", Act::PrbRep(TAB, 0), &[]).passes_through().win(800),
        s("SY.hold.up", P1, "Tab released: reaches the app", Act::PrbUp(TAB, 0), &[]).passes_through().win(800),
        s("SY.hold.field", P1, "Focus the field: focus mode", Act::Focus(role::ENTRY, "Sync field"), &["focus mode"]).win(1000),
        s("SY.hold.f.down", P1, "Tab down in focus mode (passes)", Act::PrbDown(TAB, 0), &[]).passes_through().win(300),
        s("SY.hold.f.link", P1, "Firefox's native Tab lands on Link three: browse mode", Act::Focus(role::LINK, "Link three"), &["Link three"]).win(1000),
        s("SY.hold.f.rep", P1, "Mutter autorepeat of the still-held Tab is not consumed", Act::PrbRep(TAB, 0), &[]).passes_through().win(800),
        s("SY.hold.f.up", P1, "Tab released: reaches the app (it got the press)", Act::PrbUp(TAB, 0), &[]).passes_through().win(800),
    ]
}

// ---------------------------------------------------------------------------------------------
// Reading, selection and say-all

fn rd() -> Vec<Step> {
    vec![
        top("RD.top"),
        s("RD.line", P0, "Down: next line", key0(DOWN), &["Alpha bravo charlie"]).gate(Gate::Key),
        s("RD.line.up", P0, "Up: previous line, with its role", key0(UP), &["heading level 1", "Selection and say-all"]).gate(Gate::Key),
        s("RD.char", P0, "Right: next character", key0(RIGHT), &[]).exact("e").gate(Gate::Key),
        s("RD.char.left", P0, "Left: previous character", key0(LEFT), &[]).exact("S"),
        s("RD.word", P0, "Ctrl+Right: next word", Act::Key(RIGHT, CTRL), &[]).exact("and").gate(Gate::Key),
        s("RD.word.left", P0, "Ctrl+Left: previous word", Act::Key(LEFT, CTRL), &[]).exact("Selection"),
        s("RD.end", P1, "End: the last character of the line", key0(END), &[]).exact("l"),
        s("RD.home", P1, "Home: the first character of the line", key0(HOME), &[]).exact("S"),
        s("RD.bottom", P0, "Ctrl+End: the last line", ctrl(END), &["Final paragraph of the say-all page"]),
        s("RD.bottom.edge", P0, "Down at the last line: bottom", key0(DOWN), &["bottom"]),
        s("RD.topkey", P0, "Ctrl+Home: the first line", ctrl(HOME), &["Selection and say-all"]),
        s("RD.top.edge", P0, "Up at the first line: top", key0(UP), &["top"]),
        s("RD.pgdn", P1, "Page Down on a short page lands on the last line", key0(PGDN), &["Final paragraph"]).gate(Gate::Key),
        s("RD.pgup", P1, "Page Up back to the first line", key0(PGUP), &["Selection and say-all"]),
        top("RD.top2"),
        pos("RD.p1", key0(DOWN)),
        s("RD.para", P1, "Ctrl+Down: next paragraph", ctrl(DOWN), &["Kilo lima"]),
        top("RD.top2b"),
        pos("RD.p1b", key0(DOWN)),
        s("RD.sent", P1, "Alt+Down: next sentence (only the sentence is spoken)", Act::Key(DOWN, ALT), &[]).exact("Foxtrot golf hotel india juliet."),
        // selection
        top("RD.top3"),
        pos("RD.sel.0", key0(DOWN)),
        s("RD.sel.right", P1, "Shift+Right: the character is spoken as selected", Act::Key(RIGHT, SHIFT), &["selected"]),
        s("RD.sel.left", P1, "Shift+Left: unselected", Act::Key(LEFT, SHIFT), &["unselected"]),
        s("RD.sel.word", P1, "Ctrl+Shift+Right: the word, selected", Act::Key(RIGHT, CTRL | SHIFT), &["selected"]),
        s("RD.sel.down", P1, "Shift+Down: the lines, selected", Act::Key(DOWN, SHIFT), &["selected"]),
        s("RD.sel.all", P1, "Ctrl+A: select all", ctrl(KEY_A), &["selected"]),
        pos("RD.copy", ctrl(KEY_C)),
        s("RD.copy.say", P1, "Ctrl+C then gorca+C reads the clipboard: the selected text", gorca(KEY_C), &["Alpha"]).xf("system clipboard read (gorca+C) is not implemented; Ctrl+C reaches the page, which copies the native selection mirrored for single-node spans"),
        // say-all
        top("RD.top4"),
        Step::setup("RD.sa.slow", "speech takes 120 ms per utterance from here", Act::Speak(120)),
        s("RD.sa.start", P0, "gorca+Down: say-all reads on through paragraphs, list and heading", gorca(DOWN), &["Alpha bravo", "Kilo lima", "Uniform victor", "List item one", "Second part"]).win(9000),
        top("RD.top5"),
        pos("RD.sa.h", n('h')),
        s("RD.sa.inline", P1, "Say-all reads links and tables inline", gorca(DOWN), &["link inside the sentence", "Say-all table", "Final paragraph"]).win(9000),
        top("RD.top6"),
        s("RD.sa.start2", P0, "Say-all again", gorca(DOWN), &["Alpha bravo"]).win(900),
        s("RD.sa.stop", P0, "Any key stops it at once: the next line only, nothing from later paragraphs", key0(DOWN), &["Kilo lima"]).not(&["Final paragraph", "Uniform victor"]).win(1500),
        Step::setup("RD.sa.fast", "speech back to instant", Act::Speak(1)),
    ]
}

fn el() -> Vec<Step> {
    vec![
        top("EL.top"),
        s("EL.open", P0, "gorca+F7: the elements list opens", gorca(F7), &["elements list"]).xf("browse.elements_list is Planned(M7): not implemented on main").win(1500),
        s("EL.find", P1, "gorca+Ctrl+F: find in page", Act::Chord(INSERT, KEY_F, CTRL), &["find"]).xf("browse.find is Planned(M7): not implemented on main").win(1500),
        s("EL.h", P1, "H still works after the dialogs", n('h'), &["heading"]),
    ]
}

// ---------------------------------------------------------------------------------------------
// Shadow DOM

fn sh() -> Vec<Step> {
    vec![
        top("SH.top"),
        s("SH.h.1", P0, "H: a heading in an open shadow root, text from a slot", n('h'), &["heading level 2", "Slotted title text"]),
        s("SH.h.2", P0, "H: the second component", n('h'), &["Second card title"]),
        s("SH.h.3", P0, "H: a heading in a nested shadow root", n('h'), &["heading level 3", "Nested outer heading"]),
        s("SH.h.4", P0, "H: a component inside a shadow root", n('h'), &["Nested inner title"]),
        top("SH.top2"),
        s("SH.k", P0, "K: a link inside a shadow root", n('k'), &["link", "Shadow card link"]),
        s("SH.b", P0, "B: a button inside a shadow root", n('b'), &["button", "Shadow card button"]),
        s("SH.e", P0, "E: an edit inside a shadow root", n('e'), &["edit"]).extra(&["Shadow field"], NAME_OMITTED),
        s("SH.focus", P1, "Focus on the field in the shadow root: focus mode", Act::Focus(role::ENTRY, "Shadow field"), &["focus mode", "Shadow field"]),
        s("SH.esc", P1, "Escape", key0(ESC), &["browse mode"]),
        s("SH.end", P0, "Ctrl+End: light DOM text after the components", ctrl(END), &["Light paragraph after the components"]),
    ]
}

// ---------------------------------------------------------------------------------------------
// Real-world shaped pages

fn news() -> Vec<Step> {
    vec![
        top("NW.top"),
        s("NW.h1", P0, "1: the article title", lvl(1), &["heading level 1", "Screen readers learn to keep up"]).gate(Gate::Key),
        s("NW.h2.1", P0, "2: first section heading", lvl(2), &["heading level 2", "The latency problem"]),
        s("NW.h2.2", P0, "2: the advertisement heading", lvl(2), &["Sponsored"]),
        s("NW.h2.3", P0, "2: next section", lvl(2), &["What changed"]),
        s("NW.h3", P1, "3: sub-heading", lvl(3), &["heading level 3", "Tables of results"]),
        top("NW.top2"),
        s("NW.f.1", P0, "F: the cookie banner button is the first form field", n('f'), &["Accept cookies"]),
        s("NW.cookie.enter", P0, "Enter on it removes the banner", key0(ENTER), &[]).win(700),
        top("NW.top3"),
        s("NW.f.2", P0, "F: the removed banner's buttons are gone; the search box is first", n('f'), &["edit"]).extra(&["Search"], NAME_OMITTED).not(&["cookies"]),
        top("NW.top4"),
        s("NW.d.1", P0, "D: banner", n('d'), &["The Daily Gorca"]).extra(&["banner"], NO_LANDMARK_NAME),
        s("NW.d.2", P0, "D: sections navigation", n('d'), &["World"]).extra(&["Sections"], NO_LANDMARK_NAME),
        s("NW.d.3", P0, "D: search", n('d'), &["Search"]).extra(&["Search the site"], NO_LANDMARK_NAME),
        top("NW.top5"),
        s("NW.g", P1, "G: the photograph, not the decorative logo", n('g'), &["graphic", "braille display"]),
        s("NW.q", P1, "Q: the block quote", n('q'), &["Every keystroke"]).extra(&["block quote"], NO_ROLE_WORD),
        s("NW.t", P0, "T: the results table", n('t'), &["Median key-to-speech latency"]).extra(&["table"], NO_ROLE_WORD),
        top("NW.top6"),
        s("NW.l", P1, "L: the section list", n('l'), &["World"]).extra(&["list", "6 items"], NO_ROLE_WORD),
        s("NW.end", P1, "Ctrl+End: the footer line", ctrl(END), &["2026 The Daily Gorca"]),
    ]
}

fn wiki() -> Vec<Step> {
    vec![
        top("WK.top"),
        s("WK.h1", P0, "1: the title", lvl(1), &["heading level 1", "Screen reader"]),
        s("WK.h2.1", P0, "2: contents", lvl(2), &["heading level 2", "Contents"]),
        s("WK.h2.2", P0, "2: History; the edit link is part of the heading line", lvl(2), &["History"]),
        s("WK.h3", P0, "3: Early years", lvl(3), &["heading level 3", "Early years"]),
        top("WK.top2"),
        s("WK.t", P0, "T: the presentational infobox table is skipped; the data table is found", n('t'), &["Comparison of screen readers"]).not(&["Infobox"]),
        s("WK.t.2", P1, "T: the navigation box is a data table too", n('t'), &["Assistive technology"]),
        s("WK.t.3", P1, "T: no more tables", n('t'), &["no next table"]),
        top("WK.top3"),
        s("WK.d", P1, "D: first navigation landmark", n('d'), &["Log in"]).extra(&["Personal tools"], NO_LANDMARK_NAME),
        s("WK.g", P1, "G: the figure image", n('g'), &["graphic", "An early speech synthesiser"]),
        top("WK.top4"),
        s("WK.i", P1, "I: first list item of the page", n('i'), &["Log in"]).win(900),
        top("WK.top5"),
        s("WK.k", P0, "K x3: the skip link, log in, create account", rep('k', 3), &["link", "Create account"]),
        s("WK.end", P1, "Ctrl+End: the footer", ctrl(END), &["About Wikipedia"]),
    ]
}

fn forge() -> Vec<Step> {
    vec![
        top("FG.top"),
        s("FG.h.1", P0, "1: repository heading", lvl(1), &["heading level 1", "gorca-dev"]),
        top("FG.top1b"),
        s("FG.b.1", P0, "B: the first button, named by aria-label, with its state", n('b'), &["button"]).extra(&["Open global navigation menu", "collapsed"], "the aria-label name and the collapsed state of the menu button"),
        s("FG.toggle", P1, "Toggle button focus: pressed state", Act::Focus(role::TOGGLE_BUTTON, "Watch"), &["Watch 12"]).extra(&["not pressed"], "the pressed state is not spoken"),
        top("FG.top2"),
        s("FG.t", P0, "T: the file table, named by its sr-only caption", n('t'), &["Folders and files"]).extra(&["table"], NO_ROLE_WORD),
        s("FG.t.2", P1, "T: the diff table", n('t'), &["Diff of src slash lib dot rs"]),
        top("FG.top3"),
        s("FG.d", P1, "D x3: banner, search, then the global navigation", rep('d', 3), &["Pull requests"]).extra(&["Global"], NO_LANDMARK_NAME),
        top("FG.top4"),
        s("FG.h2", P1, "2: the files heading", lvl(2), &["Files"]),
        s("FG.star", P0, "Activating the star button updates a status region: spoken", Act::Do(role::PUSH_BUTTON, "Star this repository"), &["You starred this repository"]).gate(Gate::Live).win(1500),
    ]
}

fn app() -> Vec<Step> {
    vec![
        top("APP.top"),
        s("APP.b", P0, "B: the first toolbar button", n('b'), &["button", "Compose"]),
        s("APP.compose", P0, "Compose opens a modal dialog; focus moves into it: name, and the first field", Act::Do(role::PUSH_BUTTON, "Compose"), &["To"]).extra(&["New message"], "the dialog's name is not spoken when focus enters it").win(1500),
        s("APP.esc", P0, "Escape in the dialog's field: browse mode, the dialog stays", key0(ESC), &["browse mode"]),
        s("APP.h", P0, "Shift+H: the dialog's own heading", p('h'), &["New message"]),
        s("APP.close", P0, "Discard closes it; focus returns to the opener and the cursor is there; the toast 'Dialog closed' (a status region) is spoken after the focus speech", Act::Do(role::PUSH_BUTTON, "Discard"), &["Compose", "Dialog closed"]).win(3000),
        // alert dialog without aria-hidden on the app: modal scope must confine navigation
        s("APP.alert", P0, "The alert dialog opens: name and focus on Cancel", Act::Do(role::PUSH_BUTTON, "Empty trash"), &["Cancel"]).extra(&["Empty trash"], "the dialog's name is not spoken when focus enters it").win(1500),
        s("APP.alert.h", P0, "Shift+H: the dialog heading", p('h'), &["Empty trash"]),
        s("APP.alert.h2", P0, "Shift+H again: confined to the dialog, nothing from the page behind it", p('h'), &["no previous heading"]).not(&["Inbox", "Mail app"]),
        s("APP.alert.k", P0, "Shift+K: links behind the dialog are not reachable", p('k'), &["no previous link"]).not(&["Inbox", "Quarterly report"]),
        s("APP.alert.close", P0, "Cancel: focus returns to the opener", Act::Do(role::PUSH_BUTTON, "Cancel"), &["Empty trash"]).win(1500),
        top("APP.top2"),
        s("APP.tab", P1, "Focus on a tab: tabs switch to focus mode on focus", Act::Focus(role::PAGE_TAB, "Social"), &["tab", "Social"]).extra(&["focus mode"], "NVDA SWITCH_TO_PASS_THROUGH_ON_FOCUS_ROLES includes tabs (D-1)"),
        s("APP.tab.sel", P1, "Activating a tab shows its panel", Act::Do(role::PAGE_TAB, "Social"), &[]).win(700),
        s("APP.delete", P0, "Delete shows a toast: spoken", Act::Do(role::PUSH_BUTTON, "Delete"), &["Conversation deleted"]).win(1500),
        s("APP.menu", P1, "A menu button opens a menu; focus on a menu item: focus mode", Act::Do(role::PUSH_BUTTON, "More actions"), &[]).win(700),
        esc("APP.menu.esc"),
        s("APP.menu.item", P1, "Focus on a menu item", Act::Focus(role::MENU_ITEM, "Settings"), &["Settings"]).extra(&["focus mode"], "menu items pass through on focus in NVDA (D-1)"),
        esc("APP.combo.esc"),
        s("APP.combo", P1, "The address combo box: focus mode", Act::Focus(role::COMBO_BOX, "To address"), &["focus mode"]),
    ]
}

fn spa() -> Vec<Step> {
    vec![
        top("SP.top"),
        s("SP.h1", P0, "1: the initial route's heading", lvl(1), &["heading level 1", "Home view"]),
        s("SP.route", P0, "Route change: focus moves to the new heading; the new view is spoken, the old one is not", Act::Do(role::LINK, "List route"), &["List view"]).not(&["Home view"]).win(1500),
        s("SP.after", P0, "Down from the new heading: the first row", key0(DOWN), &["Row 1"]),
        s("SP.more", P1, "Loading more rows (infinite scroll) is silent", Act::Do(role::PUSH_BUTTON, "Load more rows"), &[]).not(&["Extra"]).win(900),
        pos("SP.bottom", ctrl(END)),
        s("SP.lastlink", P0, "The last link is one of the appended rows", p('k'), &["Extra 10"]),
        s("SP.route2", P0, "Back to the first route", Act::Do(role::LINK, "Home route"), &["Home view"]).not(&["List view", "Extra"]).win(1500),
        top("SP.top2"),
        s("SP.items", P0, "I x3: the third list item", rep('i', 3), &["Row 3"]),
        // The page re-renders the list 1200 ms after the click, so the cursor can be put in the
        // list first: a click on the button would otherwise move focus (and the cursor) to it.
        Step::setup("SP.rr.click", "press Re-render list; the page re-renders 1200 ms later", Act::Do(role::PUSH_BUTTON, "Re-render list")),
        s("SP.items2", P0, "I x3 from the button: the third list item", rep('i', 3), &["Row 3"]),
        s("SP.rerender", P0, "The list is re-rendered with the same text: only the toast speaks", Act::Wait(1500), &["List refreshed"]).not(&["Row 1", "Row 2"]).win(1800),
        s("SP.restore", P0, "Down: the cursor is still on Row 3 (position restore), so Row 4 is next", key0(DOWN), &["Row 4"]).not(&["Row 1"]),
        s("SP.back", P1, "Browser Back (Alt+Left) pops the route: the list view returns", Act::AppKeys(&[56, 105]), &["List view"]).win(1500),
    ]
}

// ---------------------------------------------------------------------------------------------
// M8b: ARIA names, roles, states and descriptions (tests/web/m8b/pages). The words are NVDA's
// (`refs/nvda/source/controlTypes/role.py` and `state.py` labels, `speech/speech.py` order); a
// phrase token such as "close dialog, button" also checks the order the segments are joined in.


/// Widgets page (`widgets.html`): names, tabs, switch, tree, combobox, menu button, disclosure,
/// accordion, toolbar, breadcrumb and aria-current, custom headings, landmark names.
fn wg() -> Vec<Step> {
    vec![
        // accessible names: aria-label, aria-labelledby, title, alt
        top("WG.top"),
        s("WG.btn.label", P0, "B: an icon button is its aria-label, the svg is not read", n('b'), &["close dialog, button"]).not(&["blank"]).gate(Gate::Key),
        s("WG.btn.label2", P0, "B: aria-label wins over the visible glyph", n('b'), &["button, search the site"]).not(&["\u{1F50D}"]),
        s("WG.btn.plain", P0, "B: a plain button is its content", n('b'), &["button, save draft"]),
        s("WG.btn.labelledby", P0, "B: aria-labelledby chain beats the visible text", n('b'), &["button, send message"]).not(&["ignored visible"]),
        s("WG.btn.title", P1, "B: a button named by title only", n('b'), &["title only, button"]),
        top("WG.top1"),
        s("WG.lnk.label", P0, "K x3: a link with aria-label says the label, not its text", rep('k', 3), &["link, read more about pandas"]).not(&["read more,"]),
        top("WG.top2"),
        s("WG.edit.chain", P0, "E: an edit named by an aria-labelledby chain of two spans", n('e'), &["shipping address, edit"]),
        s("WG.edit.label", P0, "E: an edit named by aria-label", n('e'), &["quantity, edit"]),
        // tabs
        top("WG.top3"),
        pos("WG.tabs.h", rep('h', 2)),
        s("WG.tabs.line", P0, "Down onto the tab list: the name and tab control, then each tab with its selection", Act::Key(DOWN, 0), &["entertainment, tab control", "tab, selected, nils frahm", "tab, agnes obel"]),
        s("WG.tab.focus.sel", P0, "Focus on the selected tab: name, tab, selected", Act::Focus(role::PAGE_TAB, "Nils Frahm"), &["nils frahm, tab, selected"]),
        esc("WG.e1"),
        s("WG.tab.focus", P0, "Focus on an unselected tab: no selected word", Act::Focus(role::PAGE_TAB, "Agnes Obel"), &["agnes obel, tab"]).not(&["selected"]),
        esc("WG.e2"),
        // switch
        top("WG.top4"),
        pos("WG.sw.h", rep('h', 3)),
        s("WG.sw.line", P0, "Down onto the switches: role switch with on and off", Act::Key(DOWN, 0), &["switch, on, notifications", "switch, off, dark mode"]).not(&["toggle button", "checked", "pressed"]),
        s("WG.sw.on", P0, "Focus on a switch that is on", Act::Focus(role::TOGGLE_BUTTON, "Notifications"), &["notifications, switch, on"]).not(&["checked"]),
        esc("WG.e3"),
        s("WG.sw.off", P0, "Focus on a switch that is off", Act::Focus(role::TOGGLE_BUTTON, "Dark mode"), &["dark mode, switch, off"]).not(&["not checked", "not pressed"]),
        esc("WG.e4"),
        // tree
        top("WG.top5"),
        pos("WG.tree.h", rep('h', 4)),
        s("WG.tree.line", P0, "Down onto the tree: name and tree view, then the first item with level and expansion", Act::Key(DOWN, 0), &["files, tree view", "tree view item, expanded, level 1", "documents"]),
        s("WG.tree.focus1", P0, "Focus on an expanded, unselected tree item: NVDA says not selected on focus", Act::Focus(role::TREE_ITEM, "Documents"), &["documents, tree view item", "expanded, level 1"]).extra(&["not selected, expanded"], NOT_SELECTED).not(&["\u{2022}"]),
        esc("WG.e5"),
        s("WG.tree.focus2", P0, "Focus on a collapsed tree item", Act::Focus(role::TREE_ITEM, "Pictures"), &["pictures, tree view item", "collapsed, level 1"]).extra(&["not selected, collapsed"], NOT_SELECTED),
        esc("WG.e6"),
        s("WG.tree.focus3", P0, "Focus on a nested item: its own level; selected is silent for tree items", Act::Focus(role::TREE_ITEM, "Report"), &["report, tree view item, level 2"]).not(&["selected"]),
        esc("WG.e7"),
        s("WG.tree.focus4", P1, "Focus on an unselected nested item", Act::Focus(role::TREE_ITEM, "Budget"), &["budget, tree view item", "level 2"]).extra(&["not selected, level 2"], NOT_SELECTED),
        esc("WG.e8"),
        // combo box, menu button, menu items
        s("WG.combo", P0, "Focus on a combo box with aria-haspopup=listbox: collapsed, no popup word", Act::Focus(role::COMBO_BOX, "Favourite fruit"), &["favourite fruit, combo box, collapsed"]).not(&["opens list", "submenu"]),
        esc("WG.e9"),
        s("WG.menubtn", P0, "Focus on a menu button: collapsed and submenu", Act::Focus(role::PUSH_BUTTON, "Actions"), &["actions, button, collapsed, submenu"]),
        esc("WG.e10"),
        s("WG.menu.item", P1, "Focus on a menu item", Act::Focus(role::MENU_ITEM, "Copy"), &["copy, menu item"]),
        esc("WG.e11"),
        s("WG.menu.check", P0, "Focus on a menu item check box", Act::Focus(role::ANY, "Word wrap"), &["word wrap, check menu item, checked"]),
        esc("WG.e12"),
        s("WG.menu.radio1", P0, "Focus on a checked menu item radio", Act::Focus(role::ANY, "Small"), &["small, radio menu item, checked"]),
        esc("WG.e13"),
        s("WG.menu.radio2", P0, "Focus on an unchecked menu item radio", Act::Focus(role::ANY, "Large"), &["large, radio menu item, not checked"]),
        esc("WG.e14"),
        // disclosure and accordion
        s("WG.disc.closed", P0, "Disclosure button, collapsed", Act::Focus(role::PUSH_BUTTON, "Show details"), &["show details, button, collapsed"]),
        esc("WG.e15"),
        s("WG.disc.open", P0, "Disclosure button, expanded", Act::Focus(role::PUSH_BUTTON, "Hide notes"), &["hide notes, button, expanded"]),
        esc("WG.e16"),
        s("WG.acc.open", P0, "Accordion header button, expanded", Act::Focus(role::PUSH_BUTTON, "Personal information"), &["personal information, button, expanded"]),
        esc("WG.e17"),
        s("WG.acc.closed", P0, "Accordion header button, collapsed", Act::Focus(role::PUSH_BUTTON, "Billing address"), &["billing address, button, collapsed"]),
        esc("WG.e18"),
        // toolbar
        s("WG.tb.pressed", P0, "Toggle button, pressed", Act::Focus(role::TOGGLE_BUTTON, "Bold"), &["bold, toggle button, pressed"]).not(&["not pressed"]),
        esc("WG.e19"),
        s("WG.tb.unpressed", P0, "Toggle button, not pressed", Act::Focus(role::TOGGLE_BUTTON, "Italic"), &["italic, toggle button, not pressed"]),
        esc("WG.e20"),
        s("WG.tb.mixed", P1, "Toggle button, aria-pressed=mixed", Act::Focus(role::TOGGLE_BUTTON, "Underline"), &["underline, toggle button, half pressed"]),
        esc("WG.e21"),
        s("WG.tb.popup", P1, "Icon button with aria-haspopup: the label and submenu", Act::Focus(role::PUSH_BUTTON, "Align left"), &["align left, button, submenu"]),
        esc("WG.e22"),
        // aria-current
        s("WG.cur.page", P0, "Breadcrumb link, aria-current=page", Act::Focus(role::LINK, "Gadgets crumb"), &["gadgets crumb, link, current page"]),
        s("WG.cur.step", P0, "aria-current=step", Act::Focus(role::LINK, "Step one link"), &["step one link, link, current step"]),
        s("WG.cur.location", P1, "aria-current=location", Act::Focus(role::LINK, "Location link"), &["location link, link, current location"]),
        s("WG.cur.date", P1, "aria-current=date", Act::Focus(role::LINK, "Date link"), &["date link, link, current date"]),
        s("WG.cur.time", P1, "aria-current=time", Act::Focus(role::LINK, "Time link"), &["time link, link, current time"]),
        s("WG.cur.true", P1, "aria-current=true", Act::Focus(role::LINK, "True link"), &["true link, link, current"]).not(&["current page", "current step"]),
        s("WG.cur.false", P1, "aria-current=false says nothing", Act::Focus(role::LINK, "False link"), &["false link, link"]).not(&["current"]),
        top("WG.top6"),
        pos("WG.crumb.k", rep('k', 5)),
        s("WG.crumb.line", P0, "K onto the current breadcrumb link", n('k'), &["link, current page, gadgets crumb"]),
        // headings by role and level
        top("WG.top7"),
        pos("WG.h.pos", rep('h', 13)),
        s("WG.h.role4", P1, "H: a div with role=heading aria-level=4", n('h'), &["heading level 4, custom heading four"]),
        s("WG.h.role2", P1, "H: role=heading aria-level=2", n('h'), &["heading level 2, custom heading two"]),
        // landmarks with aria-label / aria-labelledby names
        top("WG.top8"),
        s("WG.d.nav", P0, "D: a nav named by aria-label", n('d'), &["primary, navigation landmark"]),
        s("WG.d.region", P0, "D: a section named by aria-labelledby", n('d'), &["region", "names"]),
        s("WG.d.region2", P1, "D: a section named by aria-label", n('d'), &["tabs demo, region"]),
    ]
}

/// Hostile page (`hostile.html`): describedby / labelledby / errormessage cycles, a 2,000 hop
/// chain, 10,000 targets, megabyte strings. The reader must keep answering at once, say a
/// bounded amount, and the sentinel after all of it must still be reachable.
fn ho() -> Vec<Step> {
    vec![
        top("HO.top"),
        s("HO.h", P0, "H: the page is navigable at all", n('h'), &[]).chars(8000).win(1500),
        s("HO.cycle", P0, "E: a field in a describedby / errormessage cycle is named and spoken once", n('e'), &["cycle one, edit"]).max(2).win(1500),
        s("HO.fan", P0, "B: a button with 10,000 label targets speaks a bounded name", rep('b', 2), &["button"]).chars(16000).win(2500),
        s("HO.big", P0, "B: a megabyte aria-label is clipped, not spoken whole", n('b'), &["button"]).chars(8000).win(2500),
        s("HO.sentinel", P0, "B again: the sentinel after all of it", n('b'), &["button, sentinel button"]).win(2500),
        s("HO.key", P0, "Down still answers at once", Act::Key(DOWN, 0), &[]).max(2).win(1000),
    ]
}

/// Forms page (`forms.html`): invalid with error message, describedby hints, required, read
/// only, disabled; every control is focused (the description and error words are part of the
/// focus speech) and quick-navigated (a hit speaks them too).
fn fb() -> Vec<Step> {
    vec![
        s("FB.invalid.focus", P0, "Focus on a field with aria-invalid and aria-errormessage: invalid entry, then the message", Act::Focus(role::ENTRY, "Email address"), &["email address, edit", "invalid entry", "enter an address like name at example dot com"]),
        esc("FB.e1"),
        s("FB.describedby", P0, "Focus on a field with aria-describedby: the hint after a short pause", Act::Focus(role::ENTRY, "User name"), &["user name, edit", ". at least six characters"]).not(&["invalid"]),
        esc("FB.e2"),
        s("FB.invalid.described", P0, "An invalid field with a hint: invalid entry, then the hint", Act::Focus(role::ENTRY, "Password field"), &["password field, edit", "invalid entry", ". use a long password"]),
        esc("FB.e3"),
        s("FB.required", P0, "A field with aria-required: the word required", Act::Focus(role::ENTRY, "Aria required name"), &["aria required name, edit, required"]),
        esc("FB.e4"),
        s("FB.required.native", P0, "A native required field", Act::Focus(role::ENTRY, "Required name"), &["required name, edit", "required"]),
        esc("FB.e5"),
        s("FB.readonly.native", P0, "A native read-only field", Act::Focus(role::ENTRY, "Native read only"), &["native read only, edit", "read only"]),
        esc("FB.e6"),
        s("FB.readonly.aria", P1, "An aria-readonly field", Act::Focus(role::ENTRY, "Read only name"), &["read only name, edit"]).extra(&["read only"], "Gecko does not expose aria-readonly on an <input> (Blink sets read-only)"),
        esc("FB.e7"),
        s("FB.checkbox", P0, "An invalid check box with a hint", Act::Focus(role::CHECK_BOX, "Accept terms"), &["accept terms, check box, not checked", "invalid entry", ". you must accept to continue"]),
        esc("FB.e8"),
        s("FB.select", P1, "A select with a hint", Act::Focus(role::COMBO_BOX, "Colour pick"), &["colour pick, combo box", ". pick one"]),
        esc("FB.e9"),
        s("FB.textarea", P1, "A textarea with a hint", Act::Focus(role::ENTRY, "Notes area"), &["notes area, edit", ". optional notes"]),
        esc("FB.e10"),
        s("FB.button", P0, "A button with aria-describedby", Act::Focus(role::PUSH_BUTTON, "Submit form"), &["submit form, button", ". sends the signup"]).not(&["focus mode"]),
        s("FB.disabled.aria", P1, "An aria-disabled button", Act::Focus(role::PUSH_BUTTON, "Aria disabled button"), &["aria disabled button, button"]).extra(&["unavailable"], "Gecko leaves aria-disabled buttons enabled in the state set (to be verified per engine)"),
        // quick navigation hits speak the same words
        top("FB.top"),
        s("FB.qn.invalid", P0, "E: the invalid field is announced with its error message", n('e'), &["email address, edit", "invalid entry", "enter an address like name at example dot com"]),
        s("FB.qn.described", P0, "E: a described field says its hint after a pause", n('e'), &["user name, edit", ". at least six characters"]),
        s("FB.qn.button.disabled", P0, "B: a disabled button is unavailable", rep('b', 2), &["button, unavailable, native disabled button"]),
    ]
}

// ---- reviewer probes (M8b review, not for merge) ----
fn rv_desc() -> Vec<Step> {
    vec![
        top("RVD.top"),
        s("RVD.h", P0, "H x2: the page after 300 megabyte descriptions", rep('h', 2), &["after desc"]).win(4000),
        s("RVD.b", P0, "B: sentinel", n('b'), &["sentinel button"]).win(3000),
        s("RVD.focus", P0, "Focus a described button: bounded description", Act::Focus(role::PUSH_BUTTON, "Btn 5"), &["btn 5, button"]).win(3000),
    ]
}
fn rv_name() -> Vec<Step> {
    vec![
        top("RVN.top"),
        s("RVN.h", P0, "H x2: the page after 300 megabyte names", rep('h', 2), &["after name"]).win(4000),
        s("RVN.b", P0, "B: sentinel", n('b'), &["sentinel button"]).win(3000),
    ]
}
fn rv_inv() -> Vec<Step> {
    vec![
        top("RI.top"),
        s("RI.h", P0, "H x2 with 500 invalid fields and 100 toggling", rep('h', 2), &["after invalid"]).win(4000),
        s("RI.focus1", P0, "Focus probe field past the 64-field cap: its error text is read on focus", Act::Focus(role::ENTRY, "Probe field"), &["probe field, edit", "invalid entry", "error"]).win(3000),
        esc("RI.e1"),
        pos("RI.wait", Act::Wait(3500)),
        s("RI.focus2", P0, "Focus sentinel then probe again: the changed error text", Act::Focus(role::PUSH_BUTTON, "Sentinel"), &["sentinel"]).win(3000),
        esc("RI.e2"),
        s("RI.focus3", P0, "Probe again: second error", Act::Focus(role::ENTRY, "Probe field"), &["second error"]).win(3000),
        esc("RI.e3"),
        s("RI.key", P0, "Down answers at once under the toggle storm", Act::Key(DOWN, 0), &[]).win(1000),
    ]
}
fn rv_names() -> Vec<Step> {
    vec![
        top("RM.top"),
        s("RM.hid", P0, "B: icon button labelled by a hidden span", n('b'), &["button, hidden label text"]),
        s("RM.cb", P0, "X: aria-label check box (no visible label)", n('x'), &["subscribe box"]),
        s("RM.cb.up", P0, "Up: the line above the check box", Act::Key(UP, 0), &["hidden label text"]),
        s("RM.cb.line", P0, "Down: the line of the check box says its aria-label name", Act::Key(DOWN, 0), &["subscribe box", "check box", "not checked"]),
        top("RM.top2"),
        s("RM.h", P0, "H: heading with aria-label (NVDA: label replaces content)", n('h'), &["short heading label"]),
        s("RM.h2", P0, "H: next heading", n('h'), &["after names"]),
        top("RM.top3"),
        s("RM.k", P0, "K: card link with aria-label", n('k'), &["link, read article card title"]).not(&["card teaser text"]),
        s("RM.down1", P0, "Down from the card link: its second line", Act::Key(DOWN, 0), &[]).extra(&["card teaser text"], "Blink repeats static text runs (M9), so its lines differ"),
        s("RM.down2", P0, "Down again: the tree item keeps its content (NVDA)", Act::Key(DOWN, 0), &[]).extra(&["tree view item", "item content"], "Blink repeats static text runs (M9), so its lines differ"),
        s("RM.down3", P0, "Down again: the leading '. ' of page text is kept", Act::Key(DOWN, 0), &[]).extra(&[". dot leading paragraph"], "Blink repeats static text runs (M9), so its lines differ"),
        s("RM.down4", P0, "Down again", Act::Key(DOWN, 0), &[]).extra(&["after names"], "Blink repeats static text runs (M9), so its lines differ"),
        s("RM.tree", P0, "Focus tree item with aria-label", Act::Focus(role::TREE_ITEM, "Item label"), &["item label, tree view item"]),
    ]
}

/// Every scenario, in run order.
pub fn scenarios() -> Vec<Scenario> {
    let sc = |id, area, page, title, body| Scenario { id, area, page, title, body, xfail: None };
    vec![
        sc("M8-QN", "quick navigation keys", "quicknav.html", "M8 Quick navigation", Body::Steps(qn)),
        sc("M8-TB", "tables", "tables.html", "M8 Tables", Body::Steps(tb)),
        sc("M8-FM", "forms, focus and browse mode", "forms.html", "M8 Forms", Body::Steps(fm)),
        sc("M8-AF", "script focus and mode", "autofocus.html", "M8 Autofocus", Body::Steps(af)),
        sc("M8-LM", "landmarks and regions", "landmarks.html", "M8 Landmarks", Body::Steps(lm)),
        sc("M8-LV", "live regions", "live.html", "M8 Live regions", Body::Steps(lv)),
        sc("M8-IF", "iframes", "iframes.html", "M8 Iframes", Body::Steps(iframes)),
        sc("M8-HD", "hidden and presentational content", "hidden.html", "M8 Hidden and presentational", Body::Steps(hd)),
        sc("M8-AN", "same-page anchors", "anchors.html", "M8 Anchors", Body::Steps(an)),
        sc("M8-SY", "focus and caret sync", "sync.html", "M8 Focus and caret sync", Body::Steps(sy)),
        sc("M8-RD", "reading, selection, say-all", "selection.html", "M8 Selection and say-all", Body::Steps(rd)),
        sc("M8-SH", "shadow DOM", "shadow.html", "M8 Shadow DOM", Body::Steps(sh)),
        sc("M8-EL", "elements list and find", "news.html", "M8 News article", Body::Steps(el)),
        sc("M8-NEWS", "real-world: news article", "news.html", "M8 News article", Body::Steps(news)),
        sc("M8-WIKI", "real-world: encyclopedia article", "wiki.html", "M8 Wiki article", Body::Steps(wiki)),
        sc("M8-FORGE", "real-world: code forge repository page", "forge.html", "M8 Forge repository", Body::Steps(forge)),
        sc("M8-APP", "real-world: web app with dialogs", "webapp.html", "M8 Web app dialogs", Body::Steps(app)),
        sc("M8-SPA", "dynamic SPA updates", "spa.html", "M8 SPA", Body::Steps(spa)),
        sc("M8-BWG", "ARIA widgets: names, roles, states", "../../m8b/pages/widgets.html", "M8b Widgets", Body::Steps(wg)),
        sc("M8-BFB", "ARIA forms: invalid, described, required, read only", "../../m8b/pages/forms.html", "M8b Forms", Body::Steps(fb)),
        sc("M8-BHO", "hostile ARIA: cycles, fan-out, megabyte strings", "../../m8b/pages/hostile.html", "M8b Hostile", Body::Steps(ho)),
        sc("M8-RDESC", "review: description flood", "../../m8b/review/r_desc.html", "M8b Review Desc", Body::Steps(rv_desc)),
        sc("M8-RNAME", "review: name flood", "../../m8b/review/r_name.html", "M8b Review Name", Body::Steps(rv_name)),
        sc("M8-RINV", "review: invalid flood", "../../m8b/review/r_invalid.html", "M8b Review Invalid", Body::Steps(rv_inv)),
        sc("M8-RNAMES", "review: names", "../../m8b/review/r_names.html", "M8b Review Names", Body::Steps(rv_names)),
        Scenario {
            id: "M8-PF1",
            area: "W1 at 100k nodes: H / Shift+H p99 5 ms",
            page: "generated/large-100k.html",
            title: "M8 Large 100k",
            body: Body::W1Big,
            xfail: None,
        },
        Scenario {
            id: "M8-PF2",
            area: "table navigation at 12k cells: Ctrl+Alt+Down p99 5 ms",
            page: "generated/large-table.html",
            title: "M8 Large table",
            body: Body::TableBig,
            xfail: None,
        },
    ]
}
