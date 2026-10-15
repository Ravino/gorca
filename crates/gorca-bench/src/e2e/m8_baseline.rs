//! Baseline marks: steps that fail on `main` at the time M8 was prepared, with the root cause.
//!
//! The scenario table (`m8_steps.rs`) states what NVDA does. This table records, per step id,
//! why `main` does not do it yet, so that the gate is green at the baseline and goes red on a
//! regression. To promote a feature, delete the lines of the steps it fixes in the same commit
//! (an expected-fail step that starts to pass is reported as XPASS and must not stay marked).
//! Rows starting with `rig:` are not gorca faults but limits of the test rig; fix the rig and
//! delete them. Source: `docs/reports/m8-baseline.md` (run `m8-baseline-raw2`, 2026-10-15).

const NO_EMBEDDED: &str = "O finds no embedded object: role=application and <object> are not in the embedded category on this page (to investigate: Cat::EMBEDDED vs the role Firefox exposes)";
const GRID_FOCUS: &str = "focus on an ARIA grid cell does not switch to focus mode: the pass-through predicate lacks interactive grid cells (NVDA: TABLECELL/TABLEROW in ALWAYS_SWITCH_TO_PASS_THROUGH_ROLES)";
const LISTBOX_FOCUS: &str = "focus on a list box lands on its selected option (list item); list items do not switch to focus mode (NVDA: LISTITEM switches on focus, D-1)";
const APP_FOCUS: &str = "focus on role=application does not switch to focus mode";
const NESTED_LANDMARK: &str = "Shift+D from inside a nested landmark skips the enclosing landmark (the cursor start is compared, not the node)";
const CONTAINER_JUMP: &str = "focus moving to a container (main, a tabindex=-1 div) leaves the cursor on the container: its first text line is not spoken and Down then skips it";
const LINE_START_PREFIX: &str = "the role prefix at the start of a line is spoken again on a character, word or Home move that stays inside the same heading (LP-1 repeats it)";
const ARIA_COMBO: &str = "a div with role=combobox and text content does not switch to focus mode here, while the one on forms.html (aria-controls) does: to investigate";
const NO_RESTORE: &str = "position restore after an in-place re-render (ADR-0003 10.3) is not implemented: the cursor goes back to the first item";
const RIG_KEYS: &str = "rig: a key typed into Firefox through the compositor lands on an unnamed browser 'panel', not in the page (probe m8-probe-tab, m8-probe-tab2); the page needs keyboard focus, which a click and AT-SPI GrabFocus did not give";

/// Step ids per root cause.
pub(super) const MARKS: &[(&[&str], &str)] = &[
    (&["QN.o.1", "QN.o.2"], NO_EMBEDDED),
    (&["TB.grid.focus"], GRID_FOCUS),
    (&["FM.listbox"], LISTBOX_FOCUS),
    (&["FM.app", "FM.app.toggle"], APP_FOCUS),
    (&["LM.d.prev", "LM.d.prev2"], NESTED_LANDMARK),
    (
        &["LM.skip.enter", "AN.skip", "AN.after", "AN.foc.go"],
        CONTAINER_JUMP,
    ),
    (
        &["RD.char.left", "RD.word.left", "RD.home"],
        LINE_START_PREFIX,
    ),
    (&["APP.combo"], ARIA_COMBO),
    (&["SP.restore"], NO_RESTORE),
    (&["FM.tab.real", "SY.tab", "SP.back"], RIG_KEYS),
];

/// Why this step is expected to fail today; `None` = it must pass.
pub(super) fn reason(id: &str) -> Option<&'static str> {
    MARKS
        .iter()
        .find(|(ids, _)| ids.contains(&id))
        .map(|(_, why)| *why)
}

#[cfg(test)]
mod tests {
    #[test]
    fn reasons_are_non_empty() {
        for (ids, why) in super::MARKS {
            assert!(!ids.is_empty() && !why.is_empty());
        }
        assert!(super::reason("SP.restore").is_some());
        assert!(super::reason("QN.h.1").is_none());
    }
}
