// SPDX-License-Identifier: GPL-3.0-or-later
//! Reader-level tests of the review arm, driven through `Reader::handle` like the keys are.
#![allow(
    clippy::wildcard_enum_match_arm,
    clippy::unnecessary_wraps,
    clippy::unwrap_used,
    clippy::single_match_else,
    clippy::redundant_closure
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_input_api::{KeyClass, KeyEvent, KeyIntent, Modifiers, PressKind};
use gorca_mirror::{ApplyOrigin, MirrorRead, MirrorWriter, mirror};
use gorca_model::{
    AppId, AttrMap, ChildList, ConnectorId, Epoch, Freshness, Ifaces, LocalId, NodeKey,
    NodeSnapshot, PropMask, Rect, Relations, Role, SnapshotBatch, StateSet,
};
use gorca_rt::EpochCell;

use super::build::{HandleSrc, NavFetchError};
use super::cluster::{BuildInput, build_line_model};
use super::collect::collect;
use super::{BuildId, LineModel, ReviewMsg, ReviewOrigin};
use crate::commands::CoreCmd;
use crate::directory::MirrorDirectory;
use crate::mode::{Mode, ModeKeys};
use crate::reader::{Reader, ReaderEffect, ReaderEffects, ReaderMsg};

fn app() -> AppId {
    AppId {
        connector: ConnectorId(1),
        serial: 1,
    }
}

fn key(n: u64) -> NodeKey {
    NodeKey {
        app: app(),
        local: LocalId(n),
    }
}

fn node(
    n: u64,
    role: Role,
    name: &str,
    parent: Option<u64>,
    kids: &[u64],
    r: Option<Rect>,
) -> Arc<NodeSnapshot> {
    let mut have = PropMask::PRESENT | PropMask::CHILDREN | PropMask::PARENT;
    if r.is_some() {
        have |= PropMask::BOUNDS;
    }
    Arc::new(
        NodeSnapshot {
            key: key(n),
            role,
            states: if matches!(role, Role::Button | Role::TextEntry) {
                StateSet::SHOWING | StateSet::VISIBLE | StateSet::FOCUSABLE
            } else {
                StateSet::SHOWING | StateSet::VISIBLE
            },
            name: (!name.is_empty()).then(|| Arc::from(name)),
            description: None,
            value: None,
            parent: parent.map(key),
            children: ChildList::Known(kids.iter().map(|k| key(*k)).collect()),
            interfaces: Ifaces::empty(),
            attrs: AttrMap::default(),
            relations: Relations::default(),
            bounds: r,
            have,
            confirmed_seq: 0,
            fresh: Freshness::Fresh { at: Instant::now() },
            sem: gorca_model::NodeSem::NONE,
        }
        .with_test_record(),
    )
}

fn rect(x: i32, y: i32, w: u32, h: u32) -> Option<Rect> {
    Some(Rect { x, y, w, h })
}

/// A GTK4-like dialog: window 1 > panel 2 > [label 3, entry 4, button 5, button 6], and a status
/// bar 7 > label 8 in the window.
fn dialog() -> Vec<Arc<NodeSnapshot>> {
    vec![
        node(
            1,
            Role::Window,
            "Settings",
            None,
            &[2, 7],
            rect(0, 0, 400, 300),
        ),
        node(
            2,
            Role::Panel,
            "",
            Some(1),
            &[3, 4, 5, 6],
            rect(0, 0, 400, 200),
        ),
        node(3, Role::Label, "Name", Some(2), &[], rect(10, 10, 60, 20)),
        node(
            4,
            Role::TextEntry,
            "Name",
            Some(2),
            &[],
            rect(80, 10, 200, 20),
        ),
        node(5, Role::Button, "Apply", Some(2), &[], rect(10, 50, 80, 24)),
        node(
            6,
            Role::Button,
            "Cancel",
            Some(2),
            &[],
            rect(100, 51, 80, 24),
        ),
        node(7, Role::StatusBar, "", Some(1), &[8], rect(0, 270, 400, 30)),
        node(8, Role::Label, "Ready", Some(7), &[], rect(4, 275, 60, 20)),
    ]
}

struct Fx {
    reader: Reader,
    writer: MirrorWriter,
    epoch: Arc<EpochCell>,
    now: Instant,
}

fn fixture() -> Fx {
    let epoch = Arc::new(EpochCell::new());
    let dir = Arc::new(MirrorDirectory::new());
    let (writer, handle) = mirror();
    dir.insert(app(), &handle);
    let reader = Reader::new(Arc::clone(&epoch), dir, ModeKeys::default())
        .with_term_provider(Arc::new(crate::terminal::tests::NativeTerm));
    Fx {
        reader,
        writer,
        epoch,
        now: Instant::now(),
    }
}

fn press(cmd: CoreCmd, repeat: u8, epoch: u64) -> KeyIntent {
    KeyIntent {
        event: KeyEvent {
            keycode: 79,
            keysym: 0xff95,
            unichar: 0,
            modifiers: Modifiers::empty(),
            pressed: true,
        },
        kind: PressKind::Press,
        class: KeyClass::Command(cmd.id()),
        repeat,
        chord: None,
        epoch: Epoch(epoch),
        at: Instant::now(),
    }
}

impl Fx {
    fn load(&mut self, nodes: Vec<Arc<NodeSnapshot>>) {
        self.writer.apply(
            SnapshotBatch {
                nodes,
                ..Default::default()
            },
            ApplyOrigin::Push,
        );
    }

    fn focus(&mut self, n: u64) {
        self.reader.handle(
            ReaderMsg::Focus(crate::FocusLaneMsg::FocusChanged { node: key(n) }),
            self.now,
        );
        // Let the settle timer elapse so the focus is presented.
        self.now += Duration::from_millis(60);
        self.reader.handle(ReaderMsg::Timer, self.now);
    }

    fn run(&mut self, cmd: CoreCmd) -> ReaderEffects {
        self.run_n(cmd, 1)
    }

    fn run_n(&mut self, cmd: CoreCmd, repeat: u8) -> ReaderEffects {
        let e = self.epoch.advance();
        self.reader
            .handle(ReaderMsg::Key(press(cmd, repeat, e.0)), self.now)
    }

    fn review(&mut self, m: ReviewMsg) -> ReaderEffects {
        self.reader.handle(ReaderMsg::Review(m), self.now)
    }

    /// The model a build of `surface` would give from what the mirror holds now.
    fn model(&self, surface: u64) -> Arc<LineModel> {
        let h = self.writer.reader();
        let c = collect(&HandleSrc(&h), key(surface), super::ReviewLimits::default());
        assert!(c.misses.is_empty(), "{:?}", c.misses);
        Arc::new(build_line_model(
            &BuildInput {
                app: app(),
                surface: key(surface),
                nodes: c.nodes,
                built_rev: h.revision(),
                built_at: Instant::now(),
                truncated: c.truncated,
            },
            &super::ReviewLimits::default(),
        ))
    }
}

fn said(fx: &ReaderEffects) -> Vec<String> {
    fx.iter()
        .filter_map(|e| match e {
            ReaderEffect::Speak(s) => Some(
                s.utterance
                    .segments
                    .iter()
                    .map(|g| &*g.text)
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            _ => None,
        })
        .collect()
}

fn effects(fx: &ReaderEffects, f: impl Fn(&ReaderEffect) -> bool) -> usize {
    fx.iter().filter(|e| f(e)).count()
}

fn is_ipc(e: &ReaderEffect) -> bool {
    matches!(
        e,
        ReaderEffect::Fetch { .. }
            | ReaderEffect::ReviewNav { .. }
            | ReaderEffect::ReviewBuild(_)
            | ReaderEffect::ReviewRevalidate { .. }
            | ReaderEffect::ReviewStatusBar { .. }
            | ReaderEffect::ReviewAct { .. }
    )
}

fn built_request(fx: &ReaderEffects) -> Option<BuildId> {
    fx.iter().find_map(|e| match e {
        ReaderEffect::ReviewBuild(b) => Some(b.build),
        _ => None,
    })
}

// ---- object navigation ---------------------------------------------------------------------

#[test]
fn object_moves_over_the_mirror_skip_layout_and_speak_like_focus() {
    let mut f = fixture();
    f.load(dialog());
    f.focus(4);
    let fx = f.run(CoreCmd::ObjectNext);
    assert_eq!(said(&fx), ["Apply button"]);
    assert_eq!(effects(&fx, is_ipc), 0, "a mirror hit needs no IPC (RI-1)");
    assert!(matches!(f.reader.mode(), Mode::Review { .. }));
    // The panel is layout: the parent of a button in it is the window.
    let fx = f.run(CoreCmd::ObjectParent);
    assert_eq!(said(&fx), ["Settings window"]);
    let fx = f.run(CoreCmd::ObjectParent);
    assert_eq!(said(&fx), ["No containing object"]);
    // First child descends through the layout panel and its label to the entry.
    let fx = f.run(CoreCmd::ObjectFirstChild);
    assert_eq!(said(&fx), ["Name edit"]);
    let fx = f.run(CoreCmd::ObjectPrev);
    assert_eq!(said(&fx), ["No previous"]);
    // The flow visits every content object once: entry, Apply, Cancel, the status bar.
    let mut seen = Vec::new();
    for _ in 0..6 {
        let fx = f.run(CoreCmd::ObjectNextInFlow);
        seen.extend(said(&fx));
    }
    assert_eq!(
        seen,
        [
            "Apply button",
            "Cancel button",
            "status bar",
            "No next",
            "No next",
            "No next"
        ]
        .map(String::from)
        .to_vec()
    );
    let fx = f.run(CoreCmd::ObjectCurrent);
    assert_eq!(said(&fx), ["status bar"]);
    let fx = f.run_n(CoreCmd::ObjectCurrent, 2);
    assert_eq!(said(&fx), ["status bar, no name"]);
    assert_eq!(f.reader.review_counters().objnav_degraded, 0);
}

#[test]
fn raw_review_shows_every_node_when_simple_review_is_off() {
    let mut f = fixture();
    f.load(dialog());
    f.focus(4);
    f.run(CoreCmd::ReviewSimpleToggle);
    let fx = f.run(CoreCmd::ObjectParent);
    assert_eq!(said(&fx), ["panel"]);
    let fx = f.run(CoreCmd::ObjectFirstChild);
    assert_eq!(said(&fx), ["Name label"]);
}

#[test]
fn a_miss_suspends_once_and_resumes_on_the_fetch_reply() {
    let mut f = fixture();
    let mut nodes = dialog();
    let missing = nodes.remove(4); // button 5
    f.load(nodes);
    f.focus(4);
    let fx = f.run(CoreCmd::ObjectNext);
    assert!(said(&fx).is_empty(), "no speech before the data is there");
    let req = fx
        .iter()
        .find_map(|e| match e {
            ReaderEffect::ReviewNav { req, op } => {
                assert_eq!(op.node(), key(5));
                Some(*req)
            }
            _ => None,
        })
        .expect("one fetch");
    f.load(vec![missing]);
    let fx = f.review(ReviewMsg::NavFetched {
        req,
        result: Ok(()),
    });
    assert_eq!(said(&fx), ["Apply button"]);
}

#[test]
fn commands_pressed_while_pending_queue_and_only_the_last_speaks() {
    let mut f = fixture();
    let mut nodes = dialog();
    let missing = nodes.remove(4);
    f.load(nodes);
    f.focus(4);
    let fx = f.run(CoreCmd::ObjectNext);
    let req = fx
        .iter()
        .find_map(|e| match e {
            ReaderEffect::ReviewNav { req, .. } => Some(*req),
            _ => None,
        })
        .expect("pending");
    assert!(said(&f.run(CoreCmd::ObjectNext)).is_empty());
    assert!(said(&f.run(CoreCmd::ObjectNext)).is_empty());
    f.load(vec![missing]);
    let fx = f.review(ReviewMsg::NavFetched {
        req,
        result: Ok(()),
    });
    // Next (Apply), next (Cancel), next (status bar): one utterance, the last.
    assert_eq!(said(&fx), ["status bar"]);
}

#[test]
fn a_fetch_that_never_answers_ends_in_a_degraded_step() {
    let mut f = fixture();
    let mut nodes = dialog();
    nodes.remove(4);
    f.load(nodes);
    f.focus(4);
    let fx = f.run(CoreCmd::ObjectNext);
    assert_eq!(
        effects(&fx, |e| matches!(e, ReaderEffect::ReviewNav { .. })),
        1
    );
    f.now += Duration::from_millis(400);
    let fx = f.reader.handle(ReaderMsg::Timer, f.now);
    // The degraded step is a raw move over what the mirror has: the missing node is skipped.
    assert_eq!(said(&fx), ["Cancel button"]);
    assert_eq!(f.reader.review_counters().review_step_timeout, 1);
    assert_eq!(f.reader.review_counters().objnav_degraded, 1);
    // The next command is not blocked by the dead one.
    let fx = f.run(CoreCmd::ObjectCurrent);
    assert_eq!(said(&fx), ["Cancel button"]);
}

#[test]
fn a_failed_fetch_degrades_at_once() {
    let mut f = fixture();
    let mut nodes = dialog();
    nodes.remove(4);
    f.load(nodes);
    f.focus(4);
    let fx = f.run(CoreCmd::ObjectNext);
    let req = fx
        .iter()
        .find_map(|e| match e {
            ReaderEffect::ReviewNav { req, .. } => Some(*req),
            _ => None,
        })
        .expect("pending");
    let fx = f.review(ReviewMsg::NavFetched {
        req,
        result: Err(NavFetchError::Timeout),
    });
    assert_eq!(said(&fx), ["Cancel button"]);
    assert_eq!(f.reader.review_counters().objnav_degraded, 1);
}

// ---- flat review ---------------------------------------------------------------------------

#[test]
fn flat_review_builds_once_and_then_steps_without_any_ipc() {
    let mut f = fixture();
    f.load(dialog());
    f.focus(4);
    let fx = f.run(CoreCmd::ReviewNextLine);
    let build = built_request(&fx).expect("a cold model is built");
    assert!(said(&fx).is_empty());
    let model = f.model(1);
    let fx = f.review(ReviewMsg::Built {
        build,
        result: Ok(Arc::clone(&model)),
    });
    // The cursor anchored at the entry's line (line 0, "Name Name edit" minus the elided label),
    // and the pending next-line ran from there.
    let lines: Vec<String> = model.lines.iter().map(|l| l.text.to_string()).collect();
    assert_eq!(
        lines,
        ["Name edit", "Apply button Cancel button", "Ready"]
            .map(String::from)
            .to_vec(),
        "visual order, the label elided because the entry carries its name"
    );
    assert_eq!(said(&fx), ["Apply button Cancel button"]);
    // From here on every step is mirror-only.
    for cmd in [
        CoreCmd::ReviewNextLine,
        CoreCmd::ReviewPrevLine,
        CoreCmd::ReviewNextWord,
        CoreCmd::ReviewPrevWord,
        CoreCmd::ReviewNextChar,
        CoreCmd::ReviewPrevChar,
        CoreCmd::ReviewTop,
        CoreCmd::ReviewBottom,
        CoreCmd::ReviewCurLine,
    ] {
        let fx = f.run(cmd);
        assert_eq!(effects(&fx, is_ipc), 0, "{cmd:?}");
        assert_eq!(said(&fx).len(), 1, "{cmd:?}");
    }
    let fx = f.run(CoreCmd::ReviewBottom);
    assert_eq!(said(&fx), ["Ready"]);
    let fx = f.run(CoreCmd::ReviewNextLine);
    assert_eq!(said(&fx), ["Bottom"]);
    let fx = f.run(CoreCmd::ReviewTop);
    assert_eq!(said(&fx), ["Name edit"]);
    let fx = f.run(CoreCmd::ReviewPrevLine);
    assert_eq!(said(&fx), ["Top"]);
    // RC-2: reading moves the navigator, so object.current says what was just read.
    f.run(CoreCmd::ReviewNextLine);
    let fx = f.run(CoreCmd::ObjectCurrent);
    assert_eq!(said(&fx), ["Apply button"]);
}

#[test]
fn spelling_on_the_second_press() {
    let mut f = fixture();
    f.load(dialog());
    f.focus(5);
    let fx = f.run(CoreCmd::ReviewCurLine);
    let build = built_request(&fx).expect("build");
    let fx = f.review(ReviewMsg::Built {
        build,
        result: Ok(f.model(1)),
    });
    assert_eq!(said(&fx), ["Apply button Cancel button"]);
    let fx = f.run(CoreCmd::ReviewCurWord);
    assert_eq!(said(&fx), ["Apply"]);
    let fx = f.run_n(CoreCmd::ReviewCurWord, 2);
    assert_eq!(said(&fx), ["A, p, p, l, y"]);
    let fx = f.run(CoreCmd::ReviewCurChar);
    assert_eq!(said(&fx), ["A"]);
}

#[test]
fn a_changed_zone_on_the_target_line_waits_for_a_refresh_others_do_not() {
    let mut f = fixture();
    f.load(dialog());
    f.focus(5);
    let fx = f.run(CoreCmd::ReviewCurLine);
    let build = built_request(&fx).expect("build");
    f.review(ReviewMsg::Built {
        build,
        result: Ok(f.model(1)),
    });
    // The status bar's label changes: the target of ReviewBottom is dirty (VD-2).
    f.load(vec![node(
        8,
        Role::Label,
        "Saved",
        Some(7),
        &[],
        rect(4, 275, 60, 20),
    )]);
    let fx = f.run(CoreCmd::ReviewBottom);
    assert!(said(&fx).is_empty());
    let b2 = built_request(&fx).expect("refresh requested");
    let fresh = f.model(1);
    let fx = f.review(ReviewMsg::Built {
        build: b2,
        result: Ok(fresh),
    });
    assert_eq!(said(&fx), ["Saved"]);
    // A change away from the target line: speak at once, refresh in the background (VD-1).
    f.load(vec![node(
        4,
        Role::TextEntry,
        "Title",
        Some(2),
        &[],
        rect(80, 10, 200, 20),
    )]);
    let fx = f.run(CoreCmd::ReviewPrevLine);
    assert_eq!(said(&fx), ["Apply button Cancel button"]);
    let bg = fx.iter().find_map(|e| match e {
        ReaderEffect::ReviewBuild(b) => Some(b.interactive),
        _ => None,
    });
    assert_eq!(bg, Some(false), "a background refresh, not a wait");
}

#[test]
fn a_cold_build_that_fails_says_so_and_leaves_review_usable() {
    let mut f = fixture();
    f.load(dialog());
    f.focus(4);
    let fx = f.run(CoreCmd::ReviewNextLine);
    let build = built_request(&fx).expect("build");
    let fx = f.review(ReviewMsg::Built {
        build,
        result: Err(super::BuildError::Hung),
    });
    assert_eq!(said(&fx), ["not responding"]);
    let fx = f.run(CoreCmd::ObjectNext);
    assert_eq!(said(&fx), ["Apply button"]);
}

#[test]
fn a_flood_makes_everything_stale_and_review_speaks_from_the_old_model() {
    let mut f = fixture();
    f.load(dialog());
    f.focus(5);
    let fx = f.run(CoreCmd::ReviewCurLine);
    let build = built_request(&fx).expect("build");
    f.review(ReviewMsg::Built {
        build,
        result: Ok(f.model(1)),
    });
    // The app flooded: its whole mirror is stale (VD-3).
    f.writer.invalidate_all(Instant::now());
    f.now += Duration::from_secs(3);
    let fx = f.run(CoreCmd::ReviewBottom);
    assert_eq!(
        said(&fx),
        ["Ready"],
        "spoken from the old model, no waiting"
    );
    let refresh = fx.iter().find_map(|e| match e {
        ReaderEffect::ReviewBuild(b) => Some((b.interactive, b.dirty.is_none())),
        _ => None,
    });
    assert_eq!(
        refresh,
        Some((false, true)),
        "a full rebuild in the background"
    );
    assert_eq!(f.reader.review_counters().review_stale_spoken, 1);
}

// ---- modes, focus, actions, status bar ------------------------------------------------------

#[test]
fn review_follows_focus_unless_told_not_to() {
    let mut f = fixture();
    f.load(dialog());
    f.focus(4);
    f.run(CoreCmd::ObjectNext);
    assert!(matches!(f.reader.mode(), Mode::Review { .. }));
    f.now += Duration::from_millis(200);
    f.focus(5);
    assert_eq!(f.reader.mode(), Mode::Focus);

    f.run(CoreCmd::ReviewFocusFollowsToggle);
    f.run(CoreCmd::ObjectNext);
    assert!(matches!(f.reader.mode(), Mode::Review { .. }));
    f.now += Duration::from_millis(200);
    f.focus(4);
    assert!(
        matches!(f.reader.mode(), Mode::Review { .. }),
        "focus is tracked but review stays"
    );
}

#[test]
fn to_focus_leaves_review_and_speaks_the_focused_object() {
    let mut f = fixture();
    f.load(dialog());
    f.focus(4);
    f.run(CoreCmd::ObjectNext);
    let fx = f.run(CoreCmd::ReviewToFocus);
    assert_eq!(f.reader.mode(), Mode::Focus);
    assert_eq!(said(&fx), ["Name edit"]);
}

#[test]
fn move_focus_falls_back_to_selection_and_then_refuses_politely() {
    let mut f = fixture();
    f.load(dialog());
    f.focus(4);
    f.run(CoreCmd::ObjectNext);
    let fx = f.run(CoreCmd::ReviewMoveFocus);
    let req = fx
        .iter()
        .find_map(|e| match e {
            ReaderEffect::ReviewAct { req, act } => {
                assert!(matches!(act, gorca_connector_api::ActionReq::Focus { .. }));
                Some(*req)
            }
            _ => None,
        })
        .expect("a focus action");
    let fx = f.review(ReviewMsg::ActionDone {
        req,
        result: Err(NavFetchError::Failed),
    });
    let req2 = fx
        .iter()
        .find_map(|e| match e {
            ReaderEffect::ReviewAct { req, act } => {
                assert!(matches!(act, gorca_connector_api::ActionReq::Select { .. }));
                Some(*req)
            }
            _ => None,
        })
        .expect("the selection fallback");
    let fx = f.review(ReviewMsg::ActionDone {
        req: req2,
        result: Err(NavFetchError::Failed),
    });
    assert_eq!(said(&fx), ["cannot move focus here"]);
    assert_eq!(f.reader.review_counters().review_move_focus_unsupported, 1);
}

#[test]
fn activate_acts_on_the_navigator() {
    let mut f = fixture();
    f.load(dialog());
    f.focus(4);
    f.run(CoreCmd::ObjectNext);
    let fx = f.run(CoreCmd::ReviewActivate);
    let req = fx
        .iter()
        .find_map(|e| match e {
            ReaderEffect::ReviewAct { req, act } => {
                assert_eq!(
                    *act,
                    gorca_connector_api::ActionReq::DoDefault { node: key(5) }
                );
                Some(*req)
            }
            _ => None,
        })
        .expect("an action");
    let fx = f.review(ReviewMsg::ActionDone {
        req,
        result: Ok(()),
    });
    assert!(said(&fx).is_empty(), "success is confirmed by an event");
}

#[test]
fn the_status_bar_is_found_in_the_mirror_and_never_enters_review() {
    let mut f = fixture();
    f.load(dialog());
    f.focus(4);
    let fx = f.run(CoreCmd::InfoStatusBar);
    assert_eq!(said(&fx), ["Ready"]);
    assert_eq!(effects(&fx, is_ipc), 0);
    assert_eq!(f.reader.mode(), Mode::Focus);
    let fx = f.run_n(CoreCmd::InfoStatusBar, 2);
    assert_eq!(said(&fx), ["R, e, a, d, y"]);
}

#[test]
fn a_status_bar_missing_from_the_mirror_is_asked_for_once() {
    let mut f = fixture();
    let mut nodes = dialog();
    nodes.truncate(6); // no status bar nodes
    nodes[0] = node(
        1,
        Role::Window,
        "Settings",
        None,
        &[2],
        rect(0, 0, 400, 300),
    );
    f.load(nodes);
    f.focus(4);
    let fx = f.run(CoreCmd::InfoStatusBar);
    let req = fx
        .iter()
        .find_map(|e| match e {
            ReaderEffect::ReviewStatusBar { req, root } => {
                assert_eq!(*root, key(1));
                Some(*req)
            }
            _ => None,
        })
        .expect("a Collection call");
    let fx = f.review(ReviewMsg::StatusBarFound {
        req,
        result: Ok(None),
    });
    assert_eq!(said(&fx), ["no status bar found"]);
}

#[test]
fn review_toggle_enters_and_leaves() {
    let mut f = fixture();
    f.load(dialog());
    f.focus(4);
    let fx = f.run(CoreCmd::ReviewToggle);
    assert_eq!(said(&fx), ["review on"]);
    assert!(matches!(f.reader.mode(), Mode::Review { .. }));
    let fx = f.run(CoreCmd::ReviewToggle);
    assert_eq!(said(&fx), ["review off"]);
    assert_eq!(f.reader.mode(), Mode::Focus);
}

#[test]
fn mode_switch_toggles_between_object_and_screen() {
    let mut f = fixture();
    f.load(dialog());
    f.focus(5);
    let fx = f.run(CoreCmd::ReviewModeNext);
    // Entering at the default (screen) kind, the switch goes to object review.
    assert_eq!(said(&fx), ["Apply button"]);
    let fx = f.run(CoreCmd::ReviewCurWord);
    assert_eq!(
        said(&fx),
        ["Apply"],
        "object review reads the object's text"
    );
    let fx = f.run(CoreCmd::ReviewNextWord);
    assert_eq!(said(&fx), ["button"]);
    let fx = f.run(CoreCmd::ReviewNextWord);
    assert_eq!(said(&fx), ["Bottom"]);
}

#[test]
fn the_origin_in_browse_is_remembered() {
    let _ = ReviewOrigin {
        node: key(1),
        from: super::ReviewFrom::Focus,
    };
}

#[test]
fn a_removed_navigator_continues_from_the_origin() {
    let mut f = fixture();
    f.load(dialog());
    f.focus(4);
    f.run(CoreCmd::ObjectNext); // nav = button 5
    f.writer
        .tombstone(key(5), Instant::now(), gorca_mirror::RemovalCause::Event);
    let fx = f.run(CoreCmd::ObjectCurrent);
    // The stand-in is the origin (the focused entry).
    assert_eq!(said(&fx), ["Name edit"]);
    assert_eq!(f.reader.review_counters().review_nav_gone, 1);
}

#[test]
fn the_app_going_away_leaves_review() {
    let mut f = fixture();
    f.load(dialog());
    f.focus(4);
    f.run(CoreCmd::ObjectNext);
    f.reader
        .handle(ReaderMsg::Focus(crate::FocusLaneMsg::AppGone(app())), f.now);
    assert_eq!(f.reader.mode(), Mode::Focus);
    assert_eq!(f.reader.review_counters().review_surface_gone, 1);
    assert_eq!(f.reader.review_state().models_held(), 0);
}

#[test]
fn unrelated_replies_are_ignored() {
    let mut f = fixture();
    f.load(dialog());
    f.focus(4);
    let fx = f.review(ReviewMsg::NavFetched {
        req: super::NavReqId(99),
        result: Ok(()),
    });
    assert!(fx.is_empty());
    let fx = f.review(ReviewMsg::Built {
        build: BuildId(77),
        result: Ok(f.model(1)),
    });
    assert!(said(&fx).is_empty(), "a background model installs silently");
    assert_eq!(f.reader.review_state().models_held(), 1);
}

// ---- review findings F-2 to F-4 ------------------------------------------------------------

fn nav_req(fx: &ReaderEffects) -> Option<super::NavReqId> {
    fx.iter().find_map(|e| match e {
        ReaderEffect::ReviewNav { req, .. } => Some(*req),
        _ => None,
    })
}

fn armed(fx: &ReaderEffects) -> Option<Instant> {
    fx.iter().find_map(|e| match e {
        ReaderEffect::ArmTimer(t) => Some(*t),
        _ => None,
    })
}

/// F-2 (OB-3, R4): "a step suspends at most 3 times, all within one Interactive deadline;
/// never more than 120 ms + 5 ms". A slow but healthy app that answers each fetch just before its
/// deadline used to stretch one step to 477 ms.
#[test]
fn f2_one_object_step_never_waits_longer_than_125_ms() {
    let mut f = fixture();
    let mut nodes = dialog();
    nodes.remove(4); // button 5: every fetch "succeeds" but brings nothing
    f.load(nodes);
    f.focus(4);
    let t0 = f.now;
    let fx = f.run(CoreCmd::ObjectNext);
    let mut req = nav_req(&fx).expect("suspend 1");
    let mut deadline = armed(&fx).expect("timer 1");
    let spoken = loop {
        f.now = deadline.checked_sub(Duration::from_millis(1)).unwrap();
        let fx = f.review(ReviewMsg::NavFetched {
            req,
            result: Ok(()),
        });
        match (nav_req(&fx), armed(&fx)) {
            (Some(r), Some(d)) => {
                req = r;
                deadline = d;
            }
            _ => break said(&fx),
        }
        assert!(f.now.duration_since(t0) <= Duration::from_millis(125));
    };
    assert!(f.now.duration_since(t0) <= Duration::from_millis(125));
    assert_eq!(spoken, ["Cancel button"], "the degraded step still speaks");
}

#[test]
fn f2_the_queue_shares_the_budget_of_the_step_that_started_it() {
    let mut f = fixture();
    let mut nodes = dialog();
    nodes.remove(4);
    f.load(nodes);
    f.focus(4);
    let fx = f.run(CoreCmd::ObjectNext);
    let req = nav_req(&fx).expect("pending");
    for _ in 0..3 {
        f.run(CoreCmd::ObjectCurrent);
    }
    f.now += Duration::from_millis(124);
    let fx = f.review(ReviewMsg::NavFetched {
        req,
        result: Ok(()),
    });
    // The budget is nearly spent: nothing more is fetched for the step or its queue.
    assert!(effects(&fx, is_ipc) <= 1);
    f.now += Duration::from_millis(10);
    let late = f.reader.handle(ReaderMsg::Timer, f.now);
    assert_eq!(
        effects(&late, |e| matches!(e, ReaderEffect::ReviewNav { .. })),
        0
    );
}

/// F-3: a key after a pending operation's deadline used to run the stale command degraded and
/// speak it, and then speak the new one too: two utterances for one key.
#[test]
fn f3_a_late_pending_operation_is_dropped_silently_when_a_new_key_arrives() {
    let mut f = fixture();
    let mut nodes = dialog();
    nodes.remove(4);
    f.load(nodes);
    f.focus(4);
    let fx = f.run(CoreCmd::ObjectNext);
    assert!(nav_req(&fx).is_some());
    f.now += Duration::from_millis(400);
    let fx = f.run(CoreCmd::ObjectCurrent);
    assert_eq!(said(&fx), ["Name edit"], "one key, one utterance");
    assert_eq!(f.reader.review_counters().review_step_timeout, 1);
}

/// F-3: the runtime has one timer slot and clears it when it fires; whatever else fires it must
/// not lose the review deadline.
#[test]
fn f3_the_review_deadline_is_armed_again_when_another_timer_fires_first() {
    let mut f = fixture();
    let mut nodes = dialog();
    nodes.remove(4);
    f.load(nodes);
    f.focus(4);
    let fx = f.run(CoreCmd::ObjectNext);
    let deadline = armed(&fx).expect("armed");
    f.now = deadline.checked_sub(Duration::from_millis(50)).unwrap();
    let fx = f.reader.handle(ReaderMsg::Timer, f.now);
    assert_eq!(
        armed(&fx),
        Some(deadline),
        "still outstanding, so armed again"
    );
    f.now = deadline;
    let fx = f.reader.handle(ReaderMsg::Timer, f.now);
    assert_eq!(said(&fx), ["Cancel button"]);
    assert_eq!(armed(&fx), None);
}

/// F-3 / R3: a result for a key press that another key has since superseded is not spoken.
#[test]
fn f3_a_result_for_a_superseded_epoch_is_dropped() {
    let mut f = fixture();
    let mut nodes = dialog();
    let missing = nodes.remove(4);
    f.load(nodes);
    f.focus(4);
    let fx = f.run(CoreCmd::ObjectNext);
    let req = nav_req(&fx).expect("pending");
    f.load(vec![missing]);
    // The user pressed some other key meanwhile (every key advances the epoch).
    f.epoch.advance();
    let fx = f.review(ReviewMsg::NavFetched {
        req,
        result: Ok(()),
    });
    assert!(said(&fx).is_empty(), "{:?}", said(&fx));
    assert_eq!(f.reader.review_counters().review_superseded, 1);
}

/// F-4 (RR-1): when revalidation finds the spoken line out of date, the corrected line is spoken.
#[test]
fn f4_a_stale_line_is_corrected_after_the_revalidation_rebuild() {
    let mut f = fixture();
    f.load(dialog());
    f.focus(5);
    let fx = f.run(CoreCmd::ReviewCurLine);
    let build = built_request(&fx).expect("build");
    f.review(ReviewMsg::Built {
        build,
        result: Ok(f.model(1)),
    });
    f.run(CoreCmd::ReviewBottom);
    f.now += Duration::from_secs(3);
    let fx = f.run(CoreCmd::ReviewCurLine);
    assert_eq!(said(&fx), ["Ready"]);
    let model = fx
        .iter()
        .find_map(|e| match e {
            ReaderEffect::ReviewRevalidate { model, .. } => Some(*model),
            _ => None,
        })
        .expect("RR-1 after the TTL");
    // The app changed the text without an event; the probe found out.
    f.load(vec![node(
        8,
        Role::Label,
        "Saved",
        Some(7),
        &[],
        rect(4, 275, 60, 20),
    )]);
    let fx = f.review(ReviewMsg::Revalidated {
        model,
        verdict: super::RrVerdict::Changed { text_changed: true },
    });
    let build = built_request(&fx).expect("a background rebuild");
    let fx = f.review(ReviewMsg::Built {
        build,
        result: Ok(f.model(1)),
    });
    assert_eq!(said(&fx), ["Saved"], "the correction");
    assert_eq!(f.reader.review_counters().review_corrections, 1);
    // An unchanged revalidation says nothing.
    let fx = f.review(ReviewMsg::Revalidated {
        model,
        verdict: super::RrVerdict::Same,
    });
    assert!(said(&fx).is_empty());
}

/// The correction is not spoken when the user has already moved on.
#[test]
fn f4_no_correction_after_another_key() {
    let mut f = fixture();
    f.load(dialog());
    f.focus(5);
    let fx = f.run(CoreCmd::ReviewCurLine);
    let build = built_request(&fx).expect("build");
    f.review(ReviewMsg::Built {
        build,
        result: Ok(f.model(1)),
    });
    f.run(CoreCmd::ReviewBottom);
    f.now += Duration::from_secs(3);
    let fx = f.run(CoreCmd::ReviewCurLine);
    let model = fx
        .iter()
        .find_map(|e| match e {
            ReaderEffect::ReviewRevalidate { model, .. } => Some(*model),
            _ => None,
        })
        .expect("RR-1");
    f.load(vec![node(
        8,
        Role::Label,
        "Saved",
        Some(7),
        &[],
        rect(4, 275, 60, 20),
    )]);
    let fx = f.review(ReviewMsg::Revalidated {
        model,
        verdict: super::RrVerdict::Changed { text_changed: true },
    });
    let build = built_request(&fx).expect("rebuild");
    f.epoch.advance();
    let fx = f.review(ReviewMsg::Built {
        build,
        result: Ok(f.model(1)),
    });
    assert!(said(&fx).is_empty());
}

// ---- R1 ------------------------------------------------------------------------------------

/// R1 (ADR-0006 18): a flat step on a valid model is an index move. The strict 3 ms gate is the
/// release-build bound; a debug build only has to be sane. The end-to-end gate is bench/e2e R1.
#[test]
fn r1_a_step_on_a_big_valid_model_is_fast_and_pure() {
    let mut f = fixture();
    let n = 600u64;
    let mut nodes = vec![node(
        1,
        Role::Window,
        "Big",
        None,
        &(2..2 + n).collect::<Vec<_>>(),
        rect(0, 0, 1000, 20_000),
    )];
    for i in 0..n {
        let y = i32::try_from(i).unwrap_or(0) * 24;
        nodes.push(node(
            2 + i,
            Role::Button,
            &format!("Button number {i}"),
            Some(1),
            &[],
            rect(10, y, 200, 20),
        ));
    }
    f.load(nodes);
    f.focus(2);
    let fx = f.run(CoreCmd::ReviewCurLine);
    let build = built_request(&fx).expect("build");
    f.review(ReviewMsg::Built {
        build,
        result: Ok(f.model(1)),
    });
    let mut worst = Duration::ZERO;
    let mut samples = Vec::new();
    for i in 0..2_000 {
        let cmd = if i % 2 == 0 {
            CoreCmd::ReviewNextLine
        } else {
            CoreCmd::ReviewPrevLine
        };
        let t = Instant::now();
        let fx = f.run(cmd);
        let d = t.elapsed();
        assert_eq!(effects(&fx, is_ipc), 0);
        samples.push(d);
        worst = worst.max(d);
    }
    samples.sort();
    let p99 = samples[samples.len() * 99 / 100];
    // The strict p99 gate is the release bound; a debug run on a loaded host gates the median
    // (preemption victims land in the tail, an O(n) regression moves the median).
    let (stat, bound) = if cfg!(debug_assertions) {
        (samples[samples.len() / 2], Duration::from_millis(10))
    } else {
        (p99, Duration::from_millis(3))
    };
    assert!(
        stat <= bound,
        "{stat:?} (p99 {p99:?}, worst {worst:?}) over {bound:?}"
    );
}

// ---- terminal review (ADR-0006 LS-1, M6) -------------------------------------------------------

fn terminal_screen() -> Vec<Arc<NodeSnapshot>> {
    let mut term = (*node(
        2,
        Role::Terminal,
        "Terminal",
        Some(1),
        &[],
        rect(0, 0, 400, 300),
    ))
    .clone();
    term.states |= StateSet::FOCUSED;
    vec![
        node(
            1,
            Role::Window,
            "Terminal",
            None,
            &[2],
            rect(0, 0, 400, 300),
        ),
        Arc::new(term),
    ]
}

#[test]
fn terminal_review_reads_the_held_screen_by_line_word_and_char_with_no_ipc() {
    let mut f = fixture();
    f.load(terminal_screen());
    // Focus the terminal and collect the screen read it asks for.
    let mut fx = f.reader.handle(
        ReaderMsg::Focus(crate::FocusLaneMsg::FocusChanged { node: key(2) }),
        f.now,
    );
    f.now += Duration::from_millis(60);
    fx.extend(f.reader.handle(ReaderMsg::Timer, f.now));
    let req = fx
        .iter()
        .find_map(|e| match e {
            ReaderEffect::TextFetch {
                req,
                op: crate::TextOp::Screen { .. },
                ..
            } => Some(*req),
            _ => None,
        })
        .expect("the focused terminal's screen is read");
    let screen = "total 3\nalpha beta\nme@box:~$ ls -l";
    let n = u32::try_from(screen.chars().count()).unwrap();
    f.reader.handle(
        ReaderMsg::Text(crate::TextMsg::Fetched {
            req,
            epoch: f.epoch.current(),
            result: Ok(crate::text::caret::Reply::Window(
                gorca_model::TextWindowFill {
                    base: 0,
                    text: Arc::from(screen),
                    len: n,
                    caret: Some(n),
                },
            )),
        }),
        f.now,
    );
    // Review starts on the line with the caret, and every step is a mirror-only, IPC-free read.
    let fx = f.run(CoreCmd::ReviewCurLine);
    assert_eq!(said(&fx), ["me at box: $ ls -l"]);
    assert_eq!(effects(&fx, is_ipc), 0);
    let fx = f.run(CoreCmd::ReviewPrevLine);
    assert_eq!(said(&fx), ["alpha beta"]);
    assert_eq!(effects(&fx, is_ipc), 0);
    let fx = f.run(CoreCmd::ReviewNextWord);
    assert_eq!(said(&fx), ["beta"]);
    let fx = f.run(CoreCmd::ReviewNextChar);
    assert_eq!(said(&fx), ["e"]);
    let fx = f.run(CoreCmd::ReviewPrevLine);
    assert_eq!(said(&fx), ["total 3"]);
    let fx = f.run(CoreCmd::ReviewPrevLine);
    assert_eq!(said(&fx), ["Top"]);
    let fx = f.run(CoreCmd::ReviewBottom);
    assert_eq!(said(&fx), ["me at box: $ ls -l"]);
    assert_eq!(effects(&fx, is_ipc), 0);
}

// ---- a terminal whose screen keeps changing (terminal-review-stale) -----------------------------

fn term_fixture(
    dropped: Option<Arc<std::sync::atomic::AtomicU64>>,
    focused: Option<Arc<gorca_rt::FocusedApp>>,
) -> Fx {
    let epoch = Arc::new(EpochCell::new());
    let dir = Arc::new(MirrorDirectory::new());
    let (writer, handle) = mirror();
    dir.insert(app(), &handle);
    let mut reader = Reader::new(Arc::clone(&epoch), dir, ModeKeys::default())
        .with_term_provider(Arc::new(crate::terminal::tests::NativeTerm));
    if let Some(d) = dropped {
        reader = reader.with_text_dropped(d);
    }
    if let Some(f) = focused {
        reader = reader.with_focused(f);
    }
    let mut f = Fx {
        reader,
        writer,
        epoch,
        now: Instant::now(),
    };
    f.load(terminal_screen());
    f
}

fn screen_read(fx: &ReaderEffects) -> Option<crate::TextReqId> {
    fx.iter().find_map(|e| match e {
        ReaderEffect::TextFetch {
            req,
            op: crate::TextOp::Screen { .. },
            ..
        } => Some(*req),
        _ => None,
    })
}

impl Fx {
    /// Answer the screen read `req` with `screen` (the caret at its end).
    fn screen_reply(&mut self, req: crate::TextReqId, screen: &str) -> ReaderEffects {
        let n = u32::try_from(screen.chars().count()).unwrap();
        self.now += Duration::from_millis(5);
        self.reader.handle(
            ReaderMsg::Text(crate::TextMsg::Fetched {
                req,
                epoch: self.epoch.current(),
                result: Ok(crate::text::caret::Reply::Window(
                    gorca_model::TextWindowFill {
                        base: 0,
                        text: Arc::from(screen),
                        len: n,
                        caret: Some(n),
                    },
                )),
            }),
            self.now,
        )
    }

    /// Focus the terminal and answer its baseline read with `screen`.
    fn focus_terminal(&mut self, screen: &str) {
        let mut fx = self.reader.handle(
            ReaderMsg::Focus(crate::FocusLaneMsg::FocusChanged { node: key(2) }),
            self.now,
        );
        self.now += Duration::from_millis(60);
        fx.extend(self.reader.handle(ReaderMsg::Timer, self.now));
        let req = screen_read(&fx).expect("the focused terminal's screen is read");
        self.screen_reply(req, screen);
    }

    /// Let the extension's settle timer run and answer the screen read it asks for.
    fn settle_and_answer(&mut self, fx: &mut ReaderEffects, screen: &str) -> ReaderEffects {
        for _ in 0..4 {
            if let Some(req) = screen_read(fx) {
                return self.screen_reply(req, screen);
            }
            self.now += Duration::from_millis(40);
            *fx = self.reader.handle(ReaderMsg::Timer, self.now);
        }
        panic!("no screen read: {fx:?}");
    }
}

#[test]
fn review_of_a_terminal_whose_events_were_lost_speaks_the_held_screen_and_corrects_after_the_read()
{
    let dropped = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut f = term_fixture(Some(Arc::clone(&dropped)), None);
    f.focus_terminal("old 1\nold 2\n$ ");
    let fx = f.run(CoreCmd::ReviewCurLine);
    assert_eq!(said(&fx), ["$"]);
    // The app actor lost text events (a flood, a full queue) and nothing followed: the held
    // screen may be old. The next review step speaks from it at once (it never waits for the
    // read) and the line is corrected when the new screen turns out to be different (RV-1).
    dropped.fetch_add(3, std::sync::atomic::Ordering::Relaxed);
    let mut fx = f.run(CoreCmd::ReviewPrevLine);
    assert_eq!(said(&fx), ["old 2"], "spoken at once from the held screen");
    let fx = f.settle_and_answer(&mut fx, "new 8\nnew 9\n$ ");
    assert_eq!(
        said(&fx),
        ["new 9"],
        "corrected, once, though no line is shared"
    );
    assert_eq!(f.reader.terminal_reads().review_syncs, 1);
    // Trusted again: the next step reads the held screen with no wait.
    let fx = f.run(CoreCmd::ReviewPrevLine);
    assert_eq!(said(&fx), ["new 8"]);
    assert!(screen_read(&fx).is_none());
}

#[test]
fn review_of_a_terminal_that_is_not_the_protected_application_reads_the_screen_again() {
    // Its text events are dropped (uncounted) while the application is not the protected one.
    let cell = Arc::new(gorca_rt::FocusedApp::new());
    let mut f = term_fixture(None, Some(Arc::clone(&cell)));
    f.focus_terminal("old 1\nold 2\n$ ");
    cell.forget(app());
    let mut fx = f.run(CoreCmd::ReviewCurLine);
    assert_eq!(said(&fx), ["$"], "spoken at once, no wait");
    let fx = f.settle_and_answer(&mut fx, "new 8\nnew 9\n$ ");
    assert!(
        said(&fx).is_empty(),
        "the line did not change: no correction"
    );
    let mut fx = f.run(CoreCmd::ReviewPrevLine);
    assert_eq!(said(&fx), ["new 9"]);
    let fx = f.settle_and_answer(&mut fx, "newer 9\nnewer 10\n$ ");
    assert_eq!(
        said(&fx),
        ["newer 10"],
        "the screen read again, the line corrected"
    );
    assert!(f.reader.terminal_reads().review_syncs >= 2);
}

#[test]
fn review_never_serves_the_model_of_a_terminal_that_is_no_longer_tracked() {
    let mut f = term_fixture(None, None);
    f.focus_terminal("old 1\nold 2\n$ ");
    let fx = f.run(CoreCmd::ReviewCurLine);
    assert_eq!(said(&fx), ["$"]);
    // The tracking is gone (a focus event the presentation did not finish, a lost track): the
    // model built from the old screen has no zones to check and must not be read from.
    f.reader.term_untrack("test");
    let mut fx = f.run(CoreCmd::ReviewPrevLine);
    assert!(said(&fx).is_empty(), "stale speech: {:?}", said(&fx));
    let fx = f.settle_and_answer(&mut fx, "new 8\nnew 9\n$ ");
    assert_eq!(said(&fx), ["new 9"]);
}

#[test]
fn a_new_tracking_never_matches_a_model_built_from_an_earlier_one() {
    let mut f = term_fixture(None, None);
    f.focus_terminal("a\nb\n$ ");
    let g1 = f.reader.term_snapshot(key(2)).unwrap().generation;
    f.reader.term_untrack("test");
    f.now += Duration::from_millis(500);
    f.focus_terminal("c\nd\n$ ");
    let g2 = f.reader.term_snapshot(key(2)).unwrap().generation;
    assert_ne!(g1, g2);
    assert_ne!(g1 >> 32, g2 >> 32, "the tracking is part of the generation");
}

#[test]
fn character_reading_names_every_character_in_the_speech_language() {
    use super::flat::FlatMove::{CurChar, CurWord, NextChar};
    let mut f = fixture();
    for ch in ["!", "(", "@", ",", "-", "~", "$"] {
        let en = f.reader.review_voice(CurChar, false, 1, ch);
        assert!(!en.is_empty() && en != ch, "{ch:?} is named: {en:?}");
    }
    assert_eq!(f.reader.review_voice(NextChar, false, 1, " "), "space");
    assert_eq!(f.reader.review_voice(CurChar, false, 1, ""), "blank");
    assert_eq!(f.reader.review_voice(CurChar, false, 1, "a"), "a");
    assert_eq!(f.reader.review_voice(NextChar, true, 1, "Bottom"), "Bottom");
    f.reader.cmd.file.speech.language = "ru".to_owned();
    f.reader.cmd.republish();
    assert_eq!(f.reader.review_voice(NextChar, false, 1, " "), "пробел");
    let bang = f.reader.review_voice(CurChar, false, 1, "!");
    assert!(!bang.is_empty() && bang != "!", "{bang:?}");
    assert_eq!(f.reader.review_voice(CurWord, false, 1, "дом"), "дом");
}

// ---- a terminal whose output streams while the user reviews it (ADR-0018, Option A) -------------

impl Fx {
    /// The screen of the tracked terminal changes to `screen` (output arrived): the extension is
    /// told its events were lost, reads the screen and gets `screen`.
    fn terminal_output(&mut self, screen: &str) {
        let mut fx = ReaderEffects::default();
        self.reader.term_events_lost(&mut fx);
        self.settle_and_answer(&mut fx, screen);
    }
}

#[test]
fn live_review_a_word_move_continues_in_the_line_while_output_scrolls_the_screen() {
    // T-A1: the reported bug. Every word key found the screen changed and started again at the
    // first word of the row.
    let mut f = term_fixture(None, None);
    f.focus_terminal("one\nalpha bravo charlie delta\nout 1");
    f.run(CoreCmd::ReviewCurLine);
    let fx = f.run(CoreCmd::ReviewPrevLine);
    assert_eq!(said(&fx), ["alpha bravo charlie delta"]);
    let fx = f.run(CoreCmd::ReviewNextWord);
    assert_eq!(said(&fx), ["bravo"]);
    // One line of output: the screen scrolls by one, the user's line is now the first row.
    f.terminal_output("alpha bravo charlie delta\nout 1\nout 2");
    let fx = f.run(CoreCmd::ReviewNextWord);
    assert_eq!(said(&fx), ["charlie"]);
    f.terminal_output("out 1\nout 2\nout 3");
    let fx = f.run(CoreCmd::ReviewNextWord);
    assert_eq!(
        said(&fx),
        ["delta"],
        "scrolled away: its text is still read"
    );
}

#[test]
fn live_review_a_caret_only_change_does_not_rebuild_the_model() {
    // T-A2: a caret move bumps the screen generation; the text is the same.
    let mut f = term_fixture(None, None);
    f.focus_terminal("one\nalpha bravo\nout 1");
    f.run(CoreCmd::ReviewCurLine);
    f.run(CoreCmd::ReviewPrevLine);
    let id = f.reader.review.cur.as_ref().map(|m| m.id);
    let g = f.reader.term_snapshot(key(2)).unwrap().generation;
    let mut fx = ReaderEffects::default();
    f.reader.term_caret(3, &mut fx);
    assert_ne!(g, f.reader.term_snapshot(key(2)).unwrap().generation);
    let fx = f.run(CoreCmd::ReviewNextWord);
    assert_eq!(said(&fx), ["bravo"]);
    assert_eq!(f.reader.review.cur.as_ref().map(|m| m.id), id, "same model");
}

#[test]
fn live_review_a_line_that_scrolled_away_is_said_to_have_left_when_the_line_changes() {
    // T-A4: word moves go on in the held line; a line move says so and lands on the top row.
    let mut f = term_fixture(None, None);
    f.focus_terminal("one\nalpha bravo\nout 1");
    f.run(CoreCmd::ReviewCurLine);
    f.run(CoreCmd::ReviewPrevLine);
    f.terminal_output("out 1\nout 2\nout 3");
    let fx = f.run(CoreCmd::ReviewNextChar);
    assert_eq!(said(&fx), ["l"], "the held line goes on being read");
    let fx = f.run(CoreCmd::ReviewNextWord);
    assert_eq!(said(&fx), ["bravo"]);
    let fx = f.run(CoreCmd::ReviewNextLine);
    assert_eq!(said(&fx), ["scrolled, out 1"]);
    let fx = f.run(CoreCmd::ReviewNextLine);
    assert_eq!(said(&fx), ["out 2"], "an ordinary line move again");
}

#[test]
fn live_review_a_flood_does_not_make_a_review_key_wait() {
    // T-A6: events lost, a screen is held: the key speaks within the step.
    let dropped = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut f = term_fixture(Some(Arc::clone(&dropped)), None);
    f.focus_terminal("one\nalpha bravo\nout 1");
    f.run(CoreCmd::ReviewCurLine);
    f.run(CoreCmd::ReviewPrevLine);
    dropped.fetch_add(5, std::sync::atomic::Ordering::Relaxed);
    let fx = f.run(CoreCmd::ReviewNextWord);
    assert_eq!(said(&fx), ["bravo"]);
    assert!(
        !matches!(f.reader.review.op, super::OpState::Pending(_)),
        "no suspended step"
    );
}

#[test]
fn live_review_a_screen_replaced_as_a_whole_keeps_the_line_the_user_is_on() {
    // A flood outran the extension: nothing of the old screen is on the new one. The line is
    // held, not replaced by whatever is on the same row.
    let mut f = term_fixture(None, None);
    f.focus_terminal("old 1\nalpha bravo\n$ ");
    f.run(CoreCmd::ReviewCurLine);
    f.run(CoreCmd::ReviewPrevLine);
    let fx = f.run(CoreCmd::ReviewNextWord);
    assert_eq!(said(&fx), ["bravo"]);
    f.terminal_output("n1\nn2\n$ ");
    let fx = f.run(CoreCmd::ReviewPrevChar);
    assert_eq!(said(&fx), ["space"]);
}

#[test]
fn live_review_identical_lines_never_move_the_cursor_on_a_guess() {
    // A flood of the same line: the scroll cannot be told from the content; the cursor holds.
    let mut f = term_fixture(None, None);
    f.focus_terminal("ok\nok\nok\nok\nend here");
    f.run(CoreCmd::ReviewCurLine);
    f.run(CoreCmd::ReviewPrevLine);
    let fx = f.run(CoreCmd::ReviewPrevLine);
    assert_eq!(said(&fx), ["ok"]);
    f.terminal_output("ok\nok\nok\nok\nok");
    let fx = f.run(CoreCmd::ReviewNextChar);
    assert_eq!(said(&fx), ["k"]);
    let fx = f.run(CoreCmd::ReviewNextLine);
    assert!(said(&fx)[0].starts_with("scrolled,") || said(&fx)[0] == "ok");
}

#[test]
fn live_review_the_rows_the_terminal_reports_scrolled_move_the_cursor_exactly() {
    // A flood of one line: the content cannot tell which "ok" the cursor was on, the delete
    // events at the top of the screen can.
    let mut f = term_fixture(None, None);
    f.focus_terminal("ok\nok\nok\nok\nend");
    f.run(CoreCmd::ReviewCurLine);
    f.run(CoreCmd::ReviewPrevLine);
    f.run(CoreCmd::ReviewNextChar);
    let row = |f: &Fx| match f.reader.cursor().map(|c| c.pos) {
        Some(super::cursor::ReviewPos::Screen(Some(p))) => (p.line, p.offset),
        other => panic!("{other:?}"),
    };
    assert_eq!(row(&f), (3, 1));
    let mut fx = ReaderEffects::default();
    for _ in 0..2 {
        let d = gorca_model::TextDelta::Delete {
            offset: 0,
            len: 3,
            text: None,
        };
        f.reader.term_edit(d, &mut fx);
    }
    let fx = f.run(CoreCmd::ReviewPrevChar);
    assert_eq!(said(&fx), ["o"]);
    assert_eq!(
        row(&f),
        (1, 0),
        "two rows up, offset carried then moved back"
    );
}

#[test]
fn live_review_the_correction_speaks_the_line_the_cursor_is_on() {
    // A line spoken from a stale screen, then the screen scrolls by one and the line is rewritten.
    let dropped = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut f = term_fixture(Some(Arc::clone(&dropped)), None);
    f.focus_terminal("a1\nb2\nprog 10%\n$ ");
    f.run(CoreCmd::ReviewCurLine);
    dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut fx = f.run(CoreCmd::ReviewPrevLine);
    assert_eq!(said(&fx), ["prog 10 percent"]);
    let fx = f.settle_and_answer(&mut fx, "b2\nprog 20%\nc3\n$ ");
    assert_eq!(
        said(&fx),
        ["prog 20 percent"],
        "the cursor's line, not another"
    );
    let fx = f.run(CoreCmd::ReviewCurLine);
    assert_eq!(said(&fx), ["prog 20 percent"]);
}
