//! Minimal regressions for the bugs the random sequences found, and sanity checks of the
//! harness itself. The `#[ignore]`d ones fail on the current tree; each names the fix it waits
//! for and is run with `cargo test -p gorca-core statemodel -- --ignored`.
use super::*;

fn st(op: Op) -> Step {
    Step { dt: 700, op }
}

fn focus(n: u64) -> Step {
    st(Op::Focus {
        node: n,
        prelude: None,
        dups: 0,
    })
}

fn typed(k: KeyKind) -> Step {
    st(Op::Type(k, Modifiers::empty()))
}

fn expect_clean(steps: &[Step]) {
    if let Some(v) = trace(steps).or_else(|| run(steps)) {
        panic!("{v:?}");
    }
}

#[test]
fn a_plain_typing_session_violates_nothing() {
    expect_clean(&[
        focus(ENTRY),
        typed(KeyKind::Letter('a')),
        typed(KeyKind::Space),
        typed(KeyKind::Punct('!')),
        typed(KeyKind::Backspace),
        focus(TERM),
        typed(KeyKind::Letter('l')),
        typed(KeyKind::Enter),
        typed(KeyKind::Up),
    ]);
}

#[test]
fn the_same_seed_gives_the_same_sequence_and_verdict() {
    let a = gen_sequence(42, 40);
    let b = gen_sequence(42, 40);
    assert_eq!(format!("{a:?}"), format!("{b:?}"));
    assert_eq!(run(&a).map(|v| (v.id, v.at)), run(&b).map(|v| (v.id, v.at)));
}

/// Typing into the focused object while review is on: the user is typing, not reviewing. After
/// numpad review the entry echo degraded (generic echo: "space" by name, no deleted character)
/// because the text layer is switched off in review and nothing switched review off.
/// Fixed by commit 4652684 (branch worktree-agent-a81984edf8db88784).
#[test]
fn typing_into_the_focused_entry_leaves_review() {
    expect_clean(&[
        focus(ENTRY),
        st(Op::Cmd(CoreCmd::ReviewCurWord, 2)),
        typed(KeyKind::Backspace),
    ]);
    expect_clean(&[
        focus(TERM),
        st(Op::Cmd(CoreCmd::ReviewCurWord, 1)),
        typed(KeyKind::Letter('b')),
    ]);
}

/// WASM deployment (no native terminal build): the policy slot is busy when the terminal gets
/// the focus, so the core tracks it with the fail-closed `WatchOnly` session (reads nothing
/// aloud, claims no keys). The slot recovers, but `focus_audit_on_key` only re-tracks an
/// *untracked* terminal: the terminal stays degraded until the next focus event.
#[test]

fn a_terminal_tracked_while_the_policy_was_busy_is_upgraded_by_a_key() {
    expect_clean(&[
        st(Op::WasmDeployment),
        st(Op::Ext(ExtState::Busy)),
        focus(TERM),
        st(Op::Ext(ExtState::Healthy)),
        typed(KeyKind::Letter('a')),
    ]);
}

/// The screen read behind the focus of a terminal fails (a busy application) and no later read
/// is scheduled while it is idle: the first successful read after the user presses Enter becomes
/// the baseline, so that command's output is swallowed (and the extension has no snapshot).
#[test]

fn a_failed_baseline_read_is_retried_before_the_first_command_output() {
    expect_clean(&[
        st(Op::FailFetches(true)),
        focus(TERM),
        st(Op::FailFetches(false)),
        typed(KeyKind::Enter),
    ]);
}

/// ADR-0018: output that scrolls the terminal screen while the user reviews a line by word. The
/// cursor stays on the line and goes on from its offset (it used to start again at the first
/// word of the row).
#[test]
fn review_by_word_goes_on_while_the_terminal_scrolls() {
    let mut steps = vec![focus(TERM)];
    for k in [
        KeyKind::Letter('l'),
        KeyKind::Letter('s'),
        KeyKind::Space,
        KeyKind::Punct('-'),
        KeyKind::Letter('l'),
        KeyKind::Enter,
    ] {
        steps.push(typed(k));
    }
    steps.extend([
        st(Op::Cmd(CoreCmd::ReviewToggle, 1)),
        st(Op::Cmd(CoreCmd::ReviewPrevLine, 1)),
        st(Op::Cmd(CoreCmd::ReviewPrevLine, 1)),
        st(Op::Cmd(CoreCmd::ReviewNextWord, 1)),
        st(Op::TermScroll),
        st(Op::Cmd(CoreCmd::ReviewNextWord, 1)),
        st(Op::TermScroll),
        st(Op::Cmd(CoreCmd::ReviewNextWord, 1)),
        st(Op::LoseEvents(true)),
        st(Op::Cmd(CoreCmd::ReviewNextChar, 1)),
        st(Op::Cmd(CoreCmd::ReviewNextWord, 1)),
    ]);
    expect_clean(&steps);
}

/// The same with a line that stays on the screen while it shifts up: the offset is carried.
#[test]
fn review_by_character_goes_on_while_the_line_shifts_up() {
    let mut steps = vec![focus(TERM)];
    for _ in 0..3 {
        steps.push(typed(KeyKind::Enter));
    }
    steps.push(st(Op::Cmd(CoreCmd::ReviewToggle, 1)));
    for _ in 0..3 {
        steps.push(st(Op::Cmd(CoreCmd::ReviewPrevLine, 1)));
    }
    steps.extend([
        st(Op::Cmd(CoreCmd::ReviewNextChar, 1)),
        st(Op::TermScroll),
        st(Op::Cmd(CoreCmd::ReviewNextChar, 1)),
        st(Op::TermScroll),
        st(Op::Cmd(CoreCmd::ReviewNextChar, 1)),
        st(Op::Cmd(CoreCmd::ReviewNextChar, 1)),
    ]);
    expect_clean(&steps);
}
