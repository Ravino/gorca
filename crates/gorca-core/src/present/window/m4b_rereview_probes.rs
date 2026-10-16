//! Reviewer probes (M4b r2). Each asserts what the review fix promises or what the user should hear.
#![allow(clippy::wildcard_enum_match_arm)]
use super::tests::{Fx, app, fixture, key, node, prios, said, snap};
use super::*;
use crate::reader::{BackgroundMsg, FocusLaneMsg, ReaderMsg};
use gorca_model::LocalId;

fn k(a: u32, n: u64) -> NodeKey {
    NodeKey {
        app: app(a),
        local: LocalId(n),
    }
}
fn act(f: &mut Fx, w: NodeKey, at: Instant) -> ReaderEffects {
    f.reader.handle(
        ReaderMsg::Focus(FocusLaneMsg::WindowActivated { window: w }),
        at,
    )
}
fn foc(f: &mut Fx, n: NodeKey, at: Instant) -> ReaderEffects {
    f.reader
        .handle(ReaderMsg::Focus(FocusLaneMsg::FocusChanged { node: n }), at)
}
fn info(w: NodeKey, role: Role, title: &'static str, texts: &[&'static str]) -> ReaderMsg {
    ReaderMsg::Background(BackgroundMsg::Window(WindowInfo {
        window: w,
        snap: Some(Arc::new(NodeSnapshot {
            key: w,
            ..(*snap(&node(w.local.0, role, title, &[]))).clone()
        })),
        texts: texts.iter().map(|t| Arc::from(*t)).collect(),
    }))
}
fn count(v: &[String], needle: &str) -> usize {
    v.iter().map(|s| s.matches(needle).count()).sum()
}

/// Q1 (M-2): a fresh plain window whose read misses the 10 ms hold. Before the fix the user heard
/// "Back, button" then "Files"; now the focus name is said twice.
#[test]
fn q1_hold_miss_repeats_the_focus() {
    let mut f = fixture(&[node(2, Role::Button, "Back", &[])]);
    let t = Instant::now();
    let mut all = Vec::new();
    all.extend(said(&act(&mut f, key(1), t)));
    all.extend(said(&foc(&mut f, key(2), t + Duration::from_millis(2))));
    all.extend(said(&f.reader.handle(ReaderMsg::Timer, t + WINDOW_HOLD)));
    all.extend(said(&f.reader.handle(
        info(key(1), Role::Window, "Files", &[]),
        t + Duration::from_millis(14),
    )));
    // One complete line replaces the focus still playing (Interrupt); the focus is not heard
    // again after it (N-1).
    assert_eq!(all.len(), 2, "{all:?}");
    assert_eq!(all[1], "Files, Back, button");
    let late = f.reader.handle(
        info(key(1), Role::Window, "Files", &["More"]),
        t + Duration::from_millis(400),
    );
    assert_eq!(
        count(&said(&late), "Back"),
        0,
        "focus repeated: {:?}",
        said(&late)
    );
}

/// Q1b: the first answer comes too late to replace the focus: only what is missing is spoken.
#[test]
fn q1b_late_answer_never_repeats_the_focus() {
    let mut f = fixture(&[node(2, Role::Button, "Back", &[])]);
    let t = Instant::now();
    act(&mut f, key(1), t);
    foc(&mut f, key(2), t + Duration::from_millis(2));
    f.reader.handle(ReaderMsg::Timer, t + WINDOW_HOLD);
    let fx = f.reader.handle(
        info(key(1), Role::Window, "Files", &[]),
        t + Duration::from_millis(400),
    );
    assert_eq!(said(&fx), ["Files"]);
    assert_eq!(prios(&fx), [Prio::Notification]);
}

/// Q2 (M-2): the title is in the mirror but the dialog text is not, and the focus arrives after
/// the hold window (no hold at all): title and button are both said twice.
#[test]
fn q2_late_focus_without_hold_repeats_title_and_focus() {
    let mut f = fixture(&[
        node(1, Role::Dialog, "Delete file", &[2, 3]),
        node(3, Role::Button, "Delete", &[]),
    ]);
    let t = Instant::now();
    let mut all = Vec::new();
    all.extend(said(&act(&mut f, key(1), t)));
    all.extend(said(&foc(&mut f, key(3), t + Duration::from_millis(12))));
    all.extend(said(&f.reader.handle(
        info(
            key(1),
            Role::Dialog,
            "Delete file",
            &["This cannot be undone."],
        ),
        t + Duration::from_millis(16),
    )));
    // The late text replaces the focus utterance with the complete line, once (N-1).
    assert_eq!(all.len(), 2, "{all:?}");
    assert_eq!(
        all[1],
        "Delete file, dialog, This cannot be undone., Delete, button"
    );
}

/// Q3 (H-1): the user switches to another application's window that sends no focus event. Its
/// title is a polite notice: it neither cancels the previous app's speech nor interrupts.
#[test]
fn q3_user_switch_to_app_without_focus_is_only_a_notice() {
    let mut f = fixture(&[node(2, Role::Button, "Back", &[])]);
    let t = Instant::now();
    foc(&mut f, key(2), t);
    let e0 = f.epoch.current();
    let at = t + Duration::from_millis(500);
    act(&mut f, k(2, 40), at);
    f.reader.handle(
        info(k(2, 40), Role::Window, "Image Viewer", &[]),
        at + Duration::from_millis(3),
    );
    let fx = f.reader.handle(ReaderMsg::Timer, at + ALONE_DELAY);
    assert_eq!(said(&fx), ["Image Viewer"]);
    assert_eq!(
        prios(&fx),
        [Prio::Notification],
        "no user signal: polite notice"
    );
    assert_eq!(f.epoch.current(), e0);
}

/// Q3b (N-2): the same switch right after the user's input is the user's own: it cuts the stale
/// speech of the previous app.
#[test]
fn q3b_user_switch_after_input_interrupts() {
    let mut f = fixture(&[node(2, Role::Button, "Back", &[])]);
    let t = Instant::now();
    foc(&mut f, key(2), t);
    let e0 = f.epoch.current();
    let at = t + Duration::from_millis(500);
    f.reader.win.last_input = Some(t + Duration::from_millis(470));
    act(&mut f, k(2, 40), at);
    f.reader.handle(
        info(k(2, 40), Role::Window, "Image Viewer", &[]),
        at + Duration::from_millis(3),
    );
    let fx = f.reader.handle(ReaderMsg::Timer, at + ALONE_DELAY);
    assert_eq!(said(&fx), ["Image Viewer"]);
    assert_eq!(prios(&fx), [Prio::Message]);
    assert!(f.epoch.current() > e0, "stale speech not cancelled");
}

/// Q3c (N-2): a window the mirror knows as `active` is the user's too (a click, no key).
#[test]
fn q3c_active_window_is_the_users() {
    let mut f = fixture(&[node(2, Role::Button, "Back", &[])]);
    let t = Instant::now();
    foc(&mut f, key(2), t);
    let e0 = f.epoch.current();
    let at = t + Duration::from_millis(500);
    let (mut w2, h2) = gorca_mirror::mirror();
    f.reader.dir.insert(app(2), &h2);
    let mut win = (*snap(&node(40, Role::Window, "Image Viewer", &[]))).clone();
    win.key = k(2, 40);
    win.states |= gorca_model::StateSet::ACTIVE;
    w2.apply(
        gorca_model::SnapshotBatch {
            nodes: vec![Arc::new(win)],
            ..Default::default()
        },
        gorca_mirror::ApplyOrigin::Push,
    );
    act(&mut f, k(2, 40), at);
    let fx = f.reader.handle(ReaderMsg::Timer, at + ALONE_DELAY);
    assert_eq!(said(&fx), ["Image Viewer"]);
    assert!(f.epoch.current() > e0);
}

/// Q4 (remaining risk): eight other apps activate within 120 ms of the user's dialog; the global
/// oldest pending activation (the user's) is evicted and the dialog title never leads its focus.
#[test]
fn q4_eight_background_activations_evict_the_users_dialog_title() {
    let mut f = fixture(&[
        node(1, Role::Dialog, "Delete file", &[2, 3]),
        node(2, Role::Label, "This cannot be undone.", &[]),
        node(3, Role::Button, "Delete", &[]),
    ]);
    let t = Instant::now();
    // The user is working in the dialog's application (N-3: its activation is never evicted).
    foc(&mut f, key(2), t);
    let t = t + Duration::from_secs(1);
    act(&mut f, key(1), t);
    for a in 2..10u32 {
        act(&mut f, k(a, 1), t + Duration::from_millis(1));
    }
    let mut all = said(&foc(&mut f, key(3), t + Duration::from_millis(3)));
    all.extend(said(&f.reader.handle(ReaderMsg::Timer, t + WINDOW_HOLD)));
    assert!(
        all.iter().any(|s| s.contains("Delete file")),
        "dialog title evicted: {all:?}"
    );
}

/// Q5 (H-1): the focused app floods activations of its own two windows with no focus.
#[test]
fn q5_focused_app_activation_flood_epoch_rate() {
    let mut f = fixture(&[
        node(1, Role::Window, "A", &[]),
        node(5, Role::Window, "B", &[]),
        node(2, Role::Button, "Back", &[]),
    ]);
    let t = Instant::now();
    foc(&mut f, key(2), t);
    let e0 = f.epoch.current().0;
    let mut spoken = 0;
    for i in 0..100u64 {
        let at = t + Duration::from_millis(1000 + 101 * i);
        spoken += said(&act(&mut f, key(if i % 2 == 0 { 1 } else { 5 }), at)).len();
        spoken += said(&f.reader.handle(ReaderMsg::Timer, at + ALONE_DELAY)).len();
    }
    let adv = f.epoch.current().0 - e0;
    eprintln!("Q5: {adv} epoch advances, {spoken} utterances in ~10 s");
    assert!(adv <= 33, "{adv} epoch advances in 10 s");
}

/// Q6 (H-1): another window of the focused app comes due exactly at the hold release; the held
/// focus is spoken once and not cancelled by that title.
#[test]
fn q6_same_app_alone_timer_at_hold_release() {
    let mut f = fixture(&[
        node(3, Role::Button, "Delete", &[]),
        node(5, Role::Window, "Other", &[]),
    ]);
    let t = Instant::now();
    foc(&mut f, key(3), t);
    let base = t + Duration::from_millis(500);
    let mut out = vec![act(&mut f, key(5), t + Duration::from_millis(390))];
    out.push(act(&mut f, key(1), base));
    out.push(foc(&mut f, key(3), base + Duration::from_millis(2)));
    let e_before = f.epoch.current();
    out.push(f.reader.handle(ReaderMsg::Timer, base + WINDOW_HOLD));
    let all: Vec<String> = out.iter().flat_map(said).collect();
    assert_eq!(count(&all, "Delete, button"), 1, "{all:?}");
    assert_eq!(f.epoch.current(), e_before, "held focus cancelled: {all:?}");
}
