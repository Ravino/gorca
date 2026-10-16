//! Reviewer probes (M4b r1). Each asserts the behaviour the spec/threat model promises.
#![allow(clippy::wildcard_enum_match_arm)]
use super::tests::{fixture, node, said};
use super::*;
use crate::reader::{FocusLaneMsg, ReaderMsg};
use gorca_model::LocalId;

fn other(n: u64) -> NodeKey {
    NodeKey {
        app: super::tests::app(2),
        local: LocalId(n),
    }
}

fn all_said(v: &[ReaderEffects]) -> Vec<String> {
    v.iter().flat_map(said).collect()
}

/// P1: a background app's activation that no focus follows (and whose window it never
/// answers for) must not silently cancel the user's speech.
#[test]
fn probe_p1_background_activation_does_not_silently_advance_epoch() {
    let mut f = fixture(&[
        node(1, Role::Window, "Files", &[2]),
        node(2, Role::Button, "Back", &[]),
    ]);
    let t = Instant::now();
    let fx = f.reader.handle(
        ReaderMsg::Focus(FocusLaneMsg::FocusChanged {
            node: super::tests::key(2),
        }),
        t,
    );
    assert_eq!(said(&fx), ["Back, button"]);
    let e0 = f.epoch.current();
    let fx1 = f.reader.handle(
        ReaderMsg::Focus(FocusLaneMsg::WindowActivated { window: other(500) }),
        t + Duration::from_millis(5),
    );
    let fx2 = f
        .reader
        .handle(ReaderMsg::Timer, t + Duration::from_millis(5) + ALONE_DELAY);
    let spoken = all_said(&[fx1, fx2]);
    assert!(
        f.epoch.current() == e0 || !spoken.is_empty(),
        "the user's focus speech was cancelled by another app's activation with nothing said: epoch {:?} -> {:?}, said {spoken:?}",
        e0,
        f.epoch.current()
    );
}

/// P2: a focus utterance held for its window read must not be dropped because another app's
/// pending activation fired its alone timer during the hold.
#[test]
fn probe_p2_held_focus_survives_another_apps_alone_timer() {
    let mut f = fixture(&[node(3, Role::Button, "Delete", &[])]);
    let base = Instant::now();
    let t = base + Duration::from_millis(115);
    let mut out = Vec::new();
    // App 2 activated a window 115 ms ago and no focus followed: its alone timer is due at t+5.
    out.push(f.reader.handle(
        ReaderMsg::Focus(FocusLaneMsg::WindowActivated { window: other(500) }),
        base,
    ));
    // App 1's dialog (unknown to the mirror) activates; its focus follows 2 ms later and is held.
    out.push(f.reader.handle(
        ReaderMsg::Focus(FocusLaneMsg::WindowActivated {
            window: super::tests::key(1),
        }),
        t,
    ));
    let fx = f.reader.handle(
        ReaderMsg::Focus(FocusLaneMsg::FocusChanged {
            node: super::tests::key(3),
        }),
        t + Duration::from_millis(2),
    );
    assert!(said(&fx).is_empty(), "held: {:?}", said(&fx));
    out.push(fx);
    out.push(
        f.reader
            .handle(ReaderMsg::Timer, t + Duration::from_millis(6)),
    );
    out.push(f.reader.handle(ReaderMsg::Timer, t + WINDOW_HOLD));
    out.push(
        f.reader
            .handle(ReaderMsg::Timer, t + Duration::from_millis(200)),
    );
    let spoken = all_said(&out);
    assert!(
        spoken.iter().any(|s| s.contains("Delete, button")),
        "the user's focus was never spoken: {spoken:?}"
    );
}

/// P3: five quick Alt+Tab switches between two windows of one process (one per 150 ms): the
/// last activation's title must lead its focus.
#[test]
fn probe_p3_last_alt_tab_title_is_spoken() {
    let mut f = fixture(&[
        node(1, Role::Window, "Files", &[2]),
        node(2, Role::Button, "Back", &[]),
        node(5, Role::Window, "Other", &[6]),
        node(6, Role::Button, "Go", &[]),
    ]);
    let t = Instant::now();
    let mut last = Vec::new();
    let mut all: Vec<Vec<String>> = Vec::new();
    for i in 0..9u64 {
        let (w, n) = if i % 2 == 0 { (1, 2) } else { (5, 6) };
        let at = t + Duration::from_millis(125 * i);
        f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::WindowActivated {
                window: super::tests::key(w),
            }),
            at,
        );
        let fx = f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::FocusChanged {
                node: super::tests::key(n),
            }),
            at + Duration::from_millis(3),
        );
        last = said(&fx);
        all.push(last.clone());
    }
    assert!(
        all.iter()
            .all(|s| s[0].starts_with("Files") || s[0].starts_with("Other")),
        "titles dropped: {all:?}"
    );
    assert_eq!(
        last,
        ["Files, Back, button"],
        "the final window's title was dropped by the activation limit"
    );
}
