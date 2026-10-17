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

fn quick(op: Op) -> Step {
    Step { dt: 1, op }
}

/// Found by the 50k run (seed 37737, shrunk): the policy slot is absent at the focus (the
/// stand-in tracks the terminal), busy at the first key (the upgrade finds no session), healthy
/// at the second key. That key must upgrade the stand-in to the real policy.
#[test]
fn a_degraded_terminal_is_upgraded_by_the_first_key_after_the_slot_recovers() {
    expect_clean(&[
        quick(Op::WasmDeployment),
        quick(Op::Ext(ExtState::Absent)),
        quick(Op::Focus {
            node: TERM,
            prelude: None,
            dups: 2,
        }),
        quick(Op::Ext(ExtState::Busy)),
        quick(Op::Type(KeyKind::Space, Modifiers::empty())),
        quick(Op::Ext(ExtState::Healthy)),
        quick(Op::Type(KeyKind::Space, Modifiers::empty())),
    ]);
}

/// Found by the 50k run (seed 47158, shrunk): the baseline read of the terminal fails, the
/// dialog closes and focus returns, reads work again, then the user types. The terminal must
/// have been tracked again with a baseline by that key.
#[test]
fn a_terminal_whose_baseline_read_failed_is_tracked_again_by_the_next_key() {
    expect_clean(&[
        quick(Op::FailFetches(true)),
        quick(Op::CloseDialog {
            refocus: true,
            to: TERM,
        }),
        quick(Op::TermScroll),
        quick(Op::FailFetches(false)),
        quick(Op::Type(KeyKind::Backspace, Modifiers::empty())),
    ]);
}

/// Found by the 50k run (seed 812856, shrunk): the first key retries the failed baseline read
/// while reads still fail; the next key, with reads back, must retry again, not wait out the
/// interval of the attempt that failed.
#[test]
fn a_failed_retry_of_the_baseline_does_not_hold_back_the_next_key() {
    expect_clean(&[
        quick(Op::FailFetches(true)),
        quick(Op::Focus {
            node: TERM,
            prelude: None,
            dups: 0,
        }),
        quick(Op::Type(KeyKind::Enter, Modifiers::empty())),
        quick(Op::FailFetches(false)),
        quick(Op::Type(KeyKind::Backspace, Modifiers::empty())),
    ]);
}

fn after(dt: u64, op: Op) -> Step {
    Step { dt, op }
}

/// Found by the 50k run (seed 47624, shrunk) while the key-time re-read was in development: keys
/// that re-read a baseline-less terminal while every read fails must not leave the extension
/// re-arming a timer that is already due (a timer storm).
#[test]
fn re_reading_a_baseline_less_terminal_while_reads_fail_arms_no_timer_storm() {
    expect_clean(&[
        quick(Op::Focus {
            node: TERM,
            prelude: None,
            dups: 0,
        }),
        quick(Op::Cmd(CoreCmd::ReviewCurLine, 1)),
        quick(Op::FailFetches(true)),
        quick(Op::Type(KeyKind::Backspace, Modifiers::empty())),
        quick(Op::Type(KeyKind::Down, Modifiers::empty())),
        after(2600, Op::FailFetches(false)),
        after(2600, Op::Type(KeyKind::Letter('z'), Modifiers::CONTROL)),
    ]);
}

/// Found by the 50k run (seed 2021185, shrunk): background output replaces the whole screen, and
/// the line the review cursor was on is gone. The invariant `review-drift` flagged the cursor
/// moving to the new prompt line, but a line the screen no longer has has nothing to stay on; the
/// invariant now asks that the old line is still there. (With the line present the cursor stays:
/// `review_by_character_goes_on_while_the_line_shifts_up`.)
#[test]
fn review_by_character_stays_on_its_line_while_the_terminal_answers_late() {
    expect_clean(&[
        quick(Op::TermAsync),
        quick(Op::Focus {
            node: TERM,
            prelude: None,
            dups: 2,
        }),
        quick(Op::Type(KeyKind::Letter('z'), Modifiers::SHIFT)),
        quick(Op::Cmd(CoreCmd::ReviewNextChar, 2)),
        quick(Op::TermAsync),
        quick(Op::Cmd(CoreCmd::ReviewNextChar, 2)),
    ]);
}

/// Found by the 50k run (seed 823517, shrunk; a known id before): Up in the terminal after review
/// must speak the recalled command.
#[test]
fn history_recall_after_review_is_spoken() {
    expect_clean(&[
        quick(Op::Focus {
            node: TERM,
            prelude: None,
            dups: 0,
        }),
        quick(Op::Type(KeyKind::Down, Modifiers::empty())),
        quick(Op::Type(KeyKind::Backspace, Modifiers::empty())),
        quick(Op::Type(KeyKind::Backspace, Modifiers::empty())),
        quick(Op::Type(KeyKind::Letter('b'), Modifiers::empty())),
        quick(Op::Cmd(CoreCmd::ObjectNext, 2)),
        quick(Op::Type(KeyKind::Up, Modifiers::empty())),
    ]);
}
