// SPDX-License-Identifier: GPL-3.0-or-later
//! Reader-level tests of the terminal arm: keys and `text-changed` in, speech and screen reads
//! out, with a fake clock, the way the lanes drive the Reader.

#![allow(clippy::wildcard_enum_match_arm)]

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

use gorca_input_api::{KeyClass, KeyEvent, KeyIntent, Modifiers, PressKind, static_keycode};
use gorca_model::{
    AppId, ChildList, ConnectorId, Epoch, Freshness, Ifaces, LocalId, NodeKey, NodeSnapshot, Prio,
    PropMask, Role, StateSet, TextDelta, TextWindowFill,
};
use gorca_rt::EpochCell;

use gorca_ext_api::terminal::{
    Ms, TermAction, TermConfig, TermCounters, TermInput, TermSnapshot, TerminalHooks,
};

use super::raw_key;
use crate::directory::MirrorDirectory;
use crate::mode::ModeKeys;
use crate::reader::{Reader, ReaderEffect, ReaderEffects, ReaderMsg};
use crate::text::caret::Reply;
use crate::text::{TextMsg, TextOp, TextReqId};

/// The native build of `ext-vte` behind the core's seam (what the daemon installs with
/// `--terminal-policy=native`).
#[derive(Debug)]
pub(crate) struct NativeTerm;

#[derive(Debug)]
struct NativeSess(ext_vte::VteExtension);

impl crate::TermSession for NativeSess {
    fn step(&mut self, now: Ms, input: TermInput, out: &mut Vec<TermAction>) {
        self.0.step(now, input, out);
    }
    fn counters(&self) -> TermCounters {
        self.0.counters()
    }
    fn snapshot(&self) -> Option<TermSnapshot> {
        self.0.snapshot()
    }
}

impl crate::TermProvider for NativeTerm {
    fn open(&self, cfg: &TermConfig) -> Box<dyn crate::TermSession> {
        Box::new(NativeSess(ext_vte::VteExtension::new(cfg)))
    }
}

fn node() -> NodeKey {
    NodeKey {
        app: AppId {
            connector: ConnectorId(1),
            serial: 1,
        },
        local: LocalId(9),
    }
}

fn term_snap() -> Arc<NodeSnapshot> {
    Arc::new(NodeSnapshot {
        key: node(),
        role: Role::Terminal,
        states: StateSet::FOCUSED | StateSet::SHOWING | StateSet::VISIBLE,
        name: Some(Arc::from("Terminal")),
        description: None,
        value: None,
        parent: None,
        children: ChildList::Unknown,
        interfaces: Ifaces::empty(),
        attrs: gorca_model::AttrMap::default(),
        relations: gorca_model::Relations::default(),
        bounds: None,
        have: PropMask::PRESENT,
        confirmed_seq: 0,
        fresh: Freshness::Fresh { at: Instant::now() },
    })
}

struct Rig {
    r: Reader,
    epoch: Arc<EpochCell>,
    t0: Instant,
    at: u64,
    dropped: Arc<AtomicU64>,
}

fn rig() -> Rig {
    let epoch = Arc::new(EpochCell::new());
    let dir = Arc::new(MirrorDirectory::new());
    let dropped = Arc::new(AtomicU64::new(0));
    let r = Reader::new(Arc::clone(&epoch), dir, ModeKeys::default())
        .with_term_provider(Arc::new(NativeTerm))
        .with_text_dropped(Arc::clone(&dropped));
    Rig {
        r,
        epoch,
        t0: Instant::now(),
        at: 100,
        dropped,
    }
}

fn spoken(fx: &ReaderEffects) -> Vec<String> {
    fx.iter()
        .filter_map(|e| match e {
            ReaderEffect::Speak(s) => Some(
                s.utterance
                    .segments
                    .iter()
                    .map(|g| g.text.to_string())
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            _ => None,
        })
        .collect()
}

fn utterances(fx: &ReaderEffects) -> Vec<&gorca_model::Utterance> {
    fx.iter()
        .filter_map(|e| match e {
            ReaderEffect::Speak(s) => Some(&s.utterance),
            _ => None,
        })
        .collect()
}

fn screen_reads(fx: &ReaderEffects) -> Vec<TextReqId> {
    fx.iter()
        .filter_map(|e| match e {
            ReaderEffect::TextFetch {
                req,
                op: TextOp::Screen { .. },
                ..
            } => Some(*req),
            _ => None,
        })
        .collect()
}

fn timers(fx: &ReaderEffects) -> Vec<Instant> {
    fx.iter()
        .filter_map(|e| match e {
            ReaderEffect::ArmTimer(t) => Some(*t),
            _ => None,
        })
        .collect()
}

impl Rig {
    fn now(&self) -> Instant {
        self.t0 + Duration::from_millis(self.at)
    }

    fn msg(&mut self, m: ReaderMsg) -> ReaderEffects {
        let now = self.now();
        self.r.handle(m, now)
    }

    fn answer(&mut self, req: TextReqId, text: &str, caret: u32) -> ReaderEffects {
        let len = u32::try_from(text.chars().count()).unwrap_or(0);
        self.msg(ReaderMsg::Text(TextMsg::Fetched {
            req,
            epoch: self.epoch.current(),
            result: Ok(Reply::Window(TextWindowFill {
                base: 0,
                text: Arc::from(text),
                len,
                caret: Some(caret),
            })),
        }))
    }

    /// Focus the terminal and answer its baseline read.
    fn focus(&mut self, text: &str, caret: u32) -> ReaderEffects {
        let s = term_snap();
        let mut fx = ReaderEffects::new();
        self.r.now = self.now();
        self.r.text_track(&s, &mut fx);
        let reads = screen_reads(&fx);
        assert_eq!(reads.len(), 1, "one baseline read: {fx:?}");
        self.answer(reads[0], text, caret)
    }

    fn key(&mut self, keysym: u32, unichar: u32, mods: Modifiers) -> ReaderEffects {
        let epoch = self.epoch.advance();
        self.key_code(0, keysym, unichar, mods, epoch)
    }

    fn key_code(
        &mut self,
        code: u16,
        keysym: u32,
        unichar: u32,
        mods: Modifiers,
        epoch: Epoch,
    ) -> ReaderEffects {
        let now = self.now();
        self.msg(ReaderMsg::Key(KeyIntent {
            event: KeyEvent {
                keycode: code,
                keysym,
                unichar,
                modifiers: mods,
                pressed: true,
            },
            kind: PressKind::Press,
            class: KeyClass::PassToApp,
            repeat: 1,
            chord: None,
            epoch,
            at: now,
        }))
    }

    fn typed(&mut self, c: char) -> ReaderEffects {
        self.key(c as u32, c as u32, Modifiers::empty())
    }

    fn insert(&mut self, offset: u32, text: &str) -> ReaderEffects {
        self.msg(ReaderMsg::Text(TextMsg::Edit {
            node: node(),
            delta: TextDelta::Insert {
                offset,
                len: u32::try_from(text.chars().count()).unwrap_or(0),
                text: Some(gorca_model::LazyText {
                    msg: Arc::from(text.as_bytes()),
                    field: 0,
                }),
            },
        }))
    }

    fn replaced(&mut self) -> ReaderEffects {
        self.msg(ReaderMsg::Text(TextMsg::Edit {
            node: node(),
            delta: TextDelta::Replaced { text: None },
        }))
    }

    fn caret(&mut self, off: u32) -> ReaderEffects {
        self.msg(ReaderMsg::Text(TextMsg::Caret {
            node: node(),
            offset: off,
        }))
    }

    fn tick(&mut self, ms: u64) -> ReaderEffects {
        self.at += ms;
        self.msg(ReaderMsg::Timer)
    }
}

const NONE: Modifiers = Modifiers::empty();

fn ev(code: u16, keysym: u32, unichar: u32, mods: Modifiers) -> KeyEvent {
    KeyEvent {
        keycode: code,
        keysym,
        unichar,
        modifiers: mods,
        pressed: true,
    }
}

/// The core hands the key over unclassified (which key is a readline motion is the extension's
/// policy, `ext-vte`'s `keys` tests): only the facts of the press.
#[test]
fn a_key_reaches_the_extension_unclassified_with_its_physical_letter() {
    let code = |n: &str| static_keycode(n).unwrap_or(0);
    let k = raw_key(&ev(code("c"), 'c' as u32, 3, Modifiers::CONTROL));
    assert_eq!(
        (k.phys, k.ctrl, k.alt, k.shift, k.logo),
        (b'c', true, false, false, false)
    );
    assert!(!k.types_a_character());
    let k = raw_key(&ev(0, 'A' as u32, 'A' as u32, Modifiers::SHIFT));
    assert_eq!(
        (k.phys, k.shift, k.keysym, k.unichar),
        (0, true, 'A' as u32, 'A' as u32)
    );
    assert!(k.types_a_character());
    // Num Lock is not a modifier of the key; Super is.
    let k = raw_key(&ev(0, 0xff51, 0, Modifiers::MOD2));
    assert_eq!(
        (k.ctrl, k.alt, k.shift, k.logo),
        (false, false, false, false)
    );
    assert!(raw_key(&ev(0, 'x' as u32, 'x' as u32, Modifiers::MOD4)).logo);
    // Enter types no character.
    assert!(!raw_key(&ev(0, 0xff0d, 13, NONE)).types_a_character());
    // A key never shows its character in debug output.
    assert!(!format!("{:?}", raw_key(&ev(0, 'q' as u32, 'q' as u32, NONE))).contains('q'));
}

#[test]
fn focusing_a_terminal_reads_the_screen_and_speaks_the_cursor_line_after_the_focus_speech() {
    let mut g = rig();
    let fx = g.focus("out\nme@box:~$ ls", 16);
    assert_eq!(
        spoken(&fx),
        ["me at box: $ ls"],
        "punctuation at the default level"
    );
    let u = utterances(&fx)[0];
    assert_eq!(u.prio, Prio::Text, "queued behind the focus speech");
    assert!(g.r.term_tracked());
    assert_eq!(g.r.terminal_reads().answered, 1);
}

#[test]
fn a_typed_character_is_spoken_once_by_the_key_with_the_key_epoch() {
    let mut g = rig();
    g.focus("me@box:~$ ", 10);
    let fx = g.typed('l');
    assert_eq!(spoken(&fx), ["l"]);
    let u = utterances(&fx)[0];
    assert_eq!(u.prio, Prio::Interrupt);
    assert_eq!(u.epoch, g.epoch.current(), "the key's own epoch");
    // The shell echoes it: an insert and a caret move, then the settle.
    g.at += 10;
    let a = g.insert(10, "l");
    assert!(spoken(&a).is_empty());
    assert!(!timers(&a).is_empty(), "the settle timer is armed");
    let b = g.caret(11);
    assert!(spoken(&b).is_empty());
    let c = g.tick(31);
    assert!(
        spoken(&c).is_empty(),
        "the echo is explained by the key: {c:?}"
    );
    assert!(
        screen_reads(&c).is_empty(),
        "the payload kept the screen true"
    );
    let n = g.r.terminal_counters();
    assert_eq!(
        (n.typed_echoed, n.typed_explained, n.settled_local),
        (1, 1, 1)
    );
}

#[test]
fn the_settle_timer_fires_at_the_settle_time_after_the_last_event() {
    let mut g = rig();
    g.focus("$ ", 2);
    let since = g
        .now()
        .saturating_duration_since(g.r.text.origin)
        .as_millis();
    let fx = g.replaced();
    let at = timers(&fx);
    assert_eq!(at.len(), 1, "{fx:?}");
    let due = at[0].saturating_duration_since(g.r.text.origin).as_millis();
    assert_eq!(due, since + 30);
}

#[test]
fn command_output_is_read_once_after_the_settle_through_one_screen_read() {
    let mut g = rig();
    g.focus("$ ls", 4);
    let k = g.key(0xff0d, 13, NONE);
    assert!(spoken(&k).is_empty(), "Enter is claimed and silent");
    g.replaced();
    g.caret(20);
    let fx = g.tick(31);
    let reads = screen_reads(&fx);
    assert_eq!(reads.len(), 1, "{fx:?}");
    let fx = g.answer(reads[0], "$ ls\nalpha beta\n$ ", 18);
    assert_eq!(spoken(&fx), ["alpha beta\n$"]);
    let u = utterances(&fx)[0];
    assert_eq!(u.prio, Prio::Text);
    assert_eq!(u.epoch, g.epoch.current());
    assert_eq!(g.r.terminal_reads().started, 2);
}

#[test]
fn a_newer_key_makes_pending_output_stale_by_epoch() {
    let mut g = rig();
    g.focus("$ ls", 4);
    g.key(0xff0d, 13, NONE);
    g.replaced();
    let fx = g.tick(31);
    let reads = screen_reads(&fx);
    let out = g.answer(reads[0], "$ ls\nresult\n$ ", 14);
    let u = utterances(&out)[0];
    // The user types before it is heard: the utterance is older than the epoch now.
    let e = g.epoch.advance();
    assert!(!u.is_valid(e));
}

#[test]
fn a_hung_screen_read_does_not_stall_and_is_retried() {
    let mut g = rig();
    g.focus("$ ", 2);
    g.replaced();
    let fx = g.tick(31);
    let first = screen_reads(&fx)[0];
    let fx = g.msg(ReaderMsg::Text(TextMsg::Fetched {
        req: first,
        epoch: g.epoch.current(),
        result: Err(gorca_connector_api::BackendError::Timeout),
    }));
    assert_eq!(g.r.terminal_reads().failed, 1);
    assert!(spoken(&fx).is_empty());
    // The retry follows after the settle.
    let fx = g.tick(31);
    assert_eq!(screen_reads(&fx).len(), 1, "{fx:?}");
}

#[test]
fn an_answer_to_an_old_read_is_ignored() {
    let mut g = rig();
    g.focus("$ ", 2);
    let fx = g.answer(TextReqId(9999), "junk", 0);
    assert!(spoken(&fx).is_empty());
}

#[test]
fn lost_events_make_the_extension_read_the_screen_again() {
    let mut g = rig();
    g.focus("$ ", 2);
    g.dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fx = g.caret(2);
    assert!(
        !timers(&fx).is_empty() || !screen_reads(&fx).is_empty(),
        "{fx:?}"
    );
    assert_eq!(g.r.terminal_reads().events_lost, 1);
}

#[test]
fn password_prompt_characters_are_marked_sensitive_and_not_repeatable() {
    let mut g = rig();
    let prompt = "$ sudo ls\n[sudo] password for me: ";
    g.focus(prompt, u32::try_from(prompt.chars().count()).unwrap_or(0));
    let fx = g.typed('h');
    assert_eq!(spoken(&fx), ["h"]);
    assert_eq!(g.r.text.sensitive_utt, Some(utterances(&fx)[0].id));
}

/// A session that stops answering when `dead` is set (a trapped, busy or disabled extension).
#[derive(Debug)]
struct Mortal {
    inner: NativeSess,
    dead: Arc<std::sync::atomic::AtomicBool>,
}

impl crate::TermSession for Mortal {
    fn alive(&self) -> bool {
        !self.dead.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn step(&mut self, now: Ms, input: TermInput, out: &mut Vec<TermAction>) {
        if self.dead.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        self.inner.step(now, input, out);
    }
    fn counters(&self) -> TermCounters {
        self.inner.counters()
    }
    fn snapshot(&self) -> Option<TermSnapshot> {
        self.inner.snapshot()
    }
}

#[derive(Debug)]
struct MortalTerm(Arc<std::sync::atomic::AtomicBool>);

impl crate::TermProvider for MortalTerm {
    fn open(&self, cfg: &TermConfig) -> Box<dyn crate::TermSession> {
        Box::new(Mortal {
            inner: NativeSess(ext_vte::VteExtension::new(cfg)),
            dead: Arc::clone(&self.0),
        })
    }
}

fn mortal_rig() -> (Rig, Arc<std::sync::atomic::AtomicBool>) {
    let dead = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut g = rig();
    g.r =
        g.r.with_term_provider(Arc::new(MortalTerm(Arc::clone(&dead))));
    (g, dead)
}

fn every_utterance_is_sensitive(g: &Rig, fx: &ReaderEffects) -> bool {
    utterances(fx)
        .iter()
        .all(|u| g.r.text.sensitive_utt == Some(u.id))
}

#[test]
fn a_policy_that_dies_during_a_password_prompt_keeps_every_typed_key_sensitive() {
    use std::sync::atomic::Ordering;
    let (mut g, dead) = mortal_rig();
    let prompt = "$ sudo ls\n[sudo] password for me: ";
    g.focus(prompt, u32::try_from(prompt.chars().count()).unwrap_or(0));
    // Alive: the first key is sensitive.
    let fx = g.typed('h');
    assert_eq!(spoken(&fx), ["h"]);
    assert!(every_utterance_is_sensitive(&g, &fx));
    // The extension traps / is busy / is disabled from now on: it answers nothing. The core
    // still claims and speaks the key, marked sensitive, and never leaves it to the generic echo.
    dead.store(true, Ordering::Relaxed);
    for c in ['q', 'Z', '7'] {
        let keyfx = g.typed(c);
        assert_eq!(spoken(&keyfx), [c.to_string()], "{c}");
        assert!(every_utterance_is_sensitive(&g, &keyfx), "{c}: sensitive");
    }
    // Enter ends the prompt: the hold goes with it.
    let _ = g.key(0xff0d, 13, NONE);
    assert!(!g.r.text.term.secret_hold);
}

#[test]
fn the_core_sees_a_password_prompt_even_when_the_policy_never_answers() {
    // Dead from the start: the extension never reports the prompt; the core's own check of the
    // screen it read does.
    let (mut g, dead) = mortal_rig();
    dead.store(true, std::sync::atomic::Ordering::Relaxed);
    let prompt = "Enter passphrase for key '/home/me/.ssh/id': ";
    g.focus(prompt, u32::try_from(prompt.chars().count()).unwrap_or(0));
    assert!(g.r.text.term.secret_hold);
    let fx = g.typed('x');
    assert_eq!(spoken(&fx), ["x"]);
    assert!(every_utterance_is_sensitive(&g, &fx));
    // An ordinary prompt does not hold.
    let (mut g2, dead2) = mortal_rig();
    dead2.store(true, std::sync::atomic::Ordering::Relaxed);
    g2.focus("me@box:~$ ", 10);
    assert!(!g2.r.text.term.secret_hold);
}

#[test]
fn the_secret_prompt_table_of_the_core() {
    for yes in [
        "[sudo] password for me:",
        "Password:",
        "Enter PIN:",
        "[sudo] \u{43f}\u{430}\u{440}\u{43e}\u{43b}\u{44c} \u{434}\u{43b}\u{44f} me:",
        "Verification code: ",
        "Contrase\u{f1}a:",
    ] {
        assert!(super::is_secret_prompt(yes), "{yes}");
    }
    for no in [
        "me@box:~$ ",
        "ping: unknown host",
        "password reset done",
        "pinned:x y",
        "ls",
    ] {
        assert!(!super::is_secret_prompt(no), "{no}");
    }
}

#[test]
fn a_terminal_whose_policy_was_disabled_is_still_watched_for_secret_prompts() {
    #[derive(Debug)]
    struct NoTerm;
    impl crate::AppExt for NoTerm {
        fn on_event(&self, _: &gorca_model::NodeEvent) -> crate::ExtVerdict {
            crate::ExtVerdict::Pass
        }
        fn present(
            &self,
            _: &NodeSnapshot,
            _: crate::present::PresentContext,
            _: u64,
        ) -> Option<Vec<gorca_model::Segment>> {
            None
        }
        fn command(&self, _: &str, _: u32) {}
    }
    #[derive(Debug)]
    struct Src;
    impl crate::ExtSource for Src {
        fn attach(&self, _: AppId, _: &gorca_model::AppInfo) -> Option<Arc<dyn crate::AppExt>> {
            None
        }
        fn detach(&self, _: AppId) {}
        fn for_app(&self, _: AppId) -> Option<Arc<dyn crate::AppExt>> {
            Some(Arc::new(NoTerm))
        }
        fn set_sink(&self, _: Arc<dyn crate::ExtSink>) {}
    }
    let mut g = rig();
    g.r = Reader::new(
        Arc::clone(&g.epoch),
        Arc::new(MirrorDirectory::new()),
        ModeKeys::default(),
    )
    .with_ext(Arc::new(Src));
    let prompt = "Password: ";
    g.focus(prompt, 10);
    assert!(g.r.term_tracked());
    let fx = g.typed('k');
    assert_eq!(spoken(&fx), ["k"]);
    assert!(every_utterance_is_sensitive(&g, &fx));
}

#[test]
fn moving_focus_off_the_terminal_wipes_it_and_ignores_its_late_events() {
    let mut g = rig();
    g.focus("$ ", 2);
    g.r.text_untrack("test");
    assert!(!g.r.term_tracked());
    let fx = g.insert(2, "x");
    assert!(spoken(&fx).is_empty());
    assert!(screen_reads(&fx).is_empty());
    // Its counters survive the wipe.
    let _ = g.r.terminal_counters();
}

#[test]
fn a_gone_terminal_node_is_untracked() {
    let mut g = rig();
    g.focus("$ ", 2);
    g.r.text_node_gone(Some(node()), None);
    assert!(!g.r.term_tracked());
}

#[test]
fn events_of_another_node_are_not_the_terminals() {
    let mut g = rig();
    g.focus("$ ", 2);
    let other = NodeKey {
        local: LocalId(77),
        ..node()
    };
    let fx = g.msg(ReaderMsg::Text(TextMsg::Edit {
        node: other,
        delta: TextDelta::Replaced { text: None },
    }));
    assert!(timers(&fx).is_empty());
}

#[test]
fn say_current_line_and_word_read_the_held_screen_with_no_read() {
    let mut g = rig();
    g.focus("out\nme@box:~$ git status", 15);
    let ep = g.epoch.advance();
    let mut fx = ReaderEffects::new();
    g.r.now = g.now();
    g.r.text_say_unit(
        gorca_text::caret::Say::Word,
        crate::text::caret::Spell::No,
        ep,
        &mut fx,
    );
    assert_eq!(spoken(&fx), ["git"]);
    let mut fx = ReaderEffects::new();
    g.r.text_say_unit(
        gorca_text::caret::Say::Line,
        crate::text::caret::Spell::No,
        ep,
        &mut fx,
    );
    assert_eq!(spoken(&fx), ["me at box: $ git status"]);
    assert!(screen_reads(&fx).is_empty());
}

#[test]
fn nothing_is_spoken_while_review_owns_reading_but_the_screen_is_kept_current() {
    let mut g = rig();
    g.focus("$ ls", 4);
    g.r.mode = crate::mode::Mode::Review {
        origin: crate::review::ReviewOrigin {
            node: node(),
            from: crate::review::ReviewFrom::Focus,
        },
        cursor: crate::review::ReviewCursor::at(node(), crate::review::ReviewKind::Screen),
    };
    g.replaced();
    let fx = g.tick(31);
    let reads = screen_reads(&fx);
    assert_eq!(reads.len(), 1, "the screen is still read");
    let fx = g.answer(reads[0], "$ ls\nout\n$ ", 11);
    assert!(spoken(&fx).is_empty(), "{fx:?}");
}

#[test]
fn echo_typed_characters_off_silences_the_typed_character_but_not_the_rest() {
    // Regression for m6-review finding 2: the setting and its toggle command reach the terminal.
    let mut g = rig();
    g.focus("me@box:~$ ", 10);
    g.r.cmd.overlay.typed_characters = Some(false);
    g.r.cmd.republish();
    let fx = g.typed('x');
    assert!(spoken(&fx).is_empty(), "{:?}", spoken(&fx));
    // The shell's own echo of that key is explained by it and stays silent too.
    g.at += 5;
    let fx = g.insert(10, "x");
    assert!(spoken(&fx).is_empty(), "{:?}", spoken(&fx));
    // Back on: the next character is spoken again.
    g.r.cmd.overlay.typed_characters = Some(true);
    g.r.cmd.republish();
    g.at += 100;
    let fx = g.typed('y');
    assert_eq!(spoken(&fx), ["y"]);
}

// ---- degraded state: the focused terminal lost its tracking (terminal-degraded-state.md) -----

impl Rig {
    /// The Reader's focus state as after a finished focus presentation of the terminal.
    fn hold_focus_on_terminal(&mut self) {
        let snap = term_snap();
        self.r.focus = crate::focus::FocusState::Focused {
            node: node(),
            epoch: self.epoch.current(),
            presented: true,
        };
        self.r.presented = Some(crate::present::Presented {
            node: node(),
            snap,
            utt: gorca_model::UttId(1),
            epoch: self.epoch.current(),
            at: self.now(),
            ctx: crate::present::PresentContext::Focus,
        });
    }
}

#[test]
fn a_key_in_a_focused_terminal_that_lost_its_tracking_tracks_it_again_and_reads_what_follows() {
    let mut g = rig();
    g.focus("me@box:~$ ", 10);
    g.hold_focus_on_terminal();
    // The tracking is lost with no focus change (the extension was not attached at the focus,
    // a node-removed event, a busy slot): the terminal is still the focus.
    g.r.term_untrack("test");
    assert!(!g.r.term_tracked());
    g.at += 700;
    let fx = g.typed('l');
    assert!(g.r.term_tracked(), "the key restarted the tracking");
    assert_eq!(g.r.terminal_reads().retracked_on_key, 1);
    let reads = screen_reads(&fx);
    assert_eq!(reads.len(), 1, "the screen is read again: {fx:?}");
    g.answer(reads[0], "me@box:~$ l", 11);
    // History recall: the recalled command is read, and Tab/Backspace/Enter are never names.
    g.at += 700;
    let fx = g.key(0xff52, 0, NONE);
    assert!(spoken(&fx).is_empty(), "the key itself says nothing");
    g.at += 10;
    g.replaced();
    let fx = g.tick(40);
    let reads = screen_reads(&fx);
    assert_eq!(reads.len(), 1, "{fx:?}");
    let fx = g.answer(reads[0], "me@box:~$ echo recalled", 23);
    assert_eq!(spoken(&fx), ["echo recalled"]);
    for (sym, uni) in [(0xff09, 9), (0xff08, 8), (0xff0d, 13)] {
        g.at += 700;
        let fx = g.key(sym, uni, NONE);
        let said = spoken(&fx);
        assert!(
            said.iter()
                .all(|s| !["tab", "backspace", "enter"].contains(&s.as_str())),
            "{sym:#x}: {said:?}"
        );
    }
}

#[test]
fn a_terminal_that_is_not_the_focus_is_not_tracked_by_a_key() {
    let mut g = rig();
    g.focus("me@box:~$ ", 10);
    g.hold_focus_on_terminal();
    g.r.term_untrack("test");
    // Focus moved to another node: the presented node is still the terminal's, the state is not.
    let other = NodeKey {
        local: LocalId(10),
        ..node()
    };
    g.r.focus = crate::focus::FocusState::Focused {
        node: other,
        epoch: g.epoch.current(),
        presented: false,
    };
    g.typed('l');
    assert!(!g.r.term_tracked());
    // An orphaned focus is not guessed either.
    g.r.focus = crate::focus::FocusState::Orphaned {
        last: node(),
        epoch: g.epoch.current(),
    };
    g.typed('l');
    assert!(!g.r.term_tracked());
    assert_eq!(g.r.terminal_reads().retracked_on_key, 0);
}

#[test]
fn the_retracking_on_a_key_is_rate_limited() {
    let mut g = rig();
    g.focus("me@box:~$ ", 10);
    g.hold_focus_on_terminal();
    // A policy that cannot be had (no session): every key asks again (the policy may be back),
    // nothing is tracked, so nothing uses up the interval.
    g.r.term_provider = None;
    g.r.term_untrack("test");
    g.at += 700;
    g.typed('a');
    g.at += 50;
    g.typed('b');
    assert_eq!(g.r.terminal_reads().retracked_on_key, 2);
    assert!(!g.r.term_tracked(), "no policy, no tracking");
    // The policy comes back: the next key tracks, and the interval then spaces the asks.
    g.r.term_provider = Some(Arc::new(NativeTerm));
    g.at += 50;
    g.typed('c');
    assert!(g.r.term_tracked());
    assert_eq!(g.r.terminal_reads().retracked_on_key, 3);
}

#[test]
fn key_names_are_silent_while_command_keys_are_off_and_spoken_when_on() {
    let mut g = rig();
    // Not a terminal, not a text field: the generic echo.
    for (sym, uni, name) in [
        (0xff09, 9, "tab"),
        (0xff0d, 13, "enter"),
        (0xff08, 8, "backspace"),
        (0xff1b, 27, "escape"),
    ] {
        g.at += 50;
        assert!(
            spoken(&g.key(sym, uni, NONE)).is_empty(),
            "{name} by default"
        );
    }
    // Typed characters and space are not names.
    assert_eq!(spoken(&g.typed('a')), ["a"]);
    assert_eq!(spoken(&g.key(0x20, 0x20, NONE)), ["space"]);
    g.r.cmd.overlay.command_keys = Some(true);
    g.r.cmd.republish();
    for (sym, uni, name) in [(0xff09, 9, "tab"), (0xff0d, 13, "enter")] {
        g.at += 50;
        assert_eq!(spoken(&g.key(sym, uni, NONE)), [name]);
    }
}

#[test]
fn a_key_after_a_focus_that_never_finished_presents_it_again_once() {
    let mut g = rig();
    // The focus event came 3 s ago and its fetch failed twice: never presented, never tracked.
    g.r.focus = crate::focus::FocusState::Focused {
        node: node(),
        epoch: g.epoch.current(),
        presented: false,
    };
    g.r.last_focus = Some((node(), g.t0));
    g.at = 3000;
    let fetches = |fx: &ReaderEffects| {
        fx.iter()
            .filter(|e| {
                matches!(e, ReaderEffect::Fetch { node: n, kind: crate::FetchKind::Focus, .. } if *n == node())
            })
            .count()
    };
    let fx = g.typed('l');
    assert_eq!(fetches(&fx), 1, "{fx:?}");
    assert_eq!(g.r.counters.focus_represented_on_key, 1);
    g.at += 100;
    let fx = g.typed('l');
    assert_eq!(fetches(&fx), 0, "rate limited");
    // A focus that is still young is simply in flight: not asked again.
    let mut h = rig();
    h.r.focus = crate::focus::FocusState::Focused {
        node: node(),
        epoch: h.epoch.current(),
        presented: false,
    };
    h.r.last_focus = Some((node(), h.t0));
    h.at = 300;
    let fx = h.typed('l');
    assert_eq!(fetches(&fx), 0);
}

// ---- protection: a tracked terminal always has a way to get its text events -------------------

fn is_state_read(e: &ReaderEffect) -> bool {
    matches!(
        e,
        ReaderEffect::Fetch {
            kind: crate::FetchKind::StateChange,
            ..
        }
    )
}

impl Rig {
    fn state_reply(&mut self, focused: bool) -> ReaderEffects {
        let mut s = (*term_snap()).clone();
        if !focused {
            s.states = StateSet::SHOWING | StateSet::VISIBLE;
        }
        self.msg(ReaderMsg::Reply(crate::QueryReply {
            node: node(),
            epoch: self.epoch.current(),
            kind: crate::FetchKind::StateChange,
            result: Ok(Arc::new(s)),
        }))
    }
}

/// The kept trace of the VV Alt+Tab flake (docs/reports/terminal-timer-rearm.md): the terminal is
/// tracked and presented, the application never earned the protection (the real VTE says
/// `FOCUSED` after it announced the focus), so the app actor drops every text event and the
/// terminal is silent until a key re-reads the state. A timer must re-read the state on a
/// backoff until a `FOCUSED` reply earns it.
#[test]
fn an_unprotected_tracked_terminal_rereads_its_focus_state_until_it_earns_the_protection() {
    let mut g = rig();
    let published = Arc::new(gorca_rt::FocusedApp::new());
    g.r.focused_app = Some(Arc::clone(&published));
    g.focus("me@box:~$ ", 10);
    g.hold_focus_on_terminal();
    assert!(g.r.term_tracked());
    // Any message arms the first re-read.
    let fx = g.tick(1);
    assert!(!timers(&fx).is_empty(), "a watch timer is armed: {fx:?}");
    // Not due yet: nothing is read.
    let fx = g.tick(50);
    assert!(!fx.iter().any(is_state_read));
    // Due: the state is read once; a reply without FOCUSED (the VTE is not there yet) leaves the
    // read open for the next attempt, which comes later.
    let fx = g.tick(120);
    assert_eq!(fx.iter().filter(|e| is_state_read(e)).count(), 1, "{fx:?}");
    assert!(!timers(&fx).is_empty(), "the next attempt is armed");
    let fx = g.state_reply(false);
    assert!(!fx.iter().any(is_state_read));
    assert_eq!(published.get(), None);
    let fx = g.tick(100);
    assert!(!fx.iter().any(is_state_read), "backed off: {fx:?}");
    let fx = g.tick(300);
    assert_eq!(fx.iter().filter(|e| is_state_read(e)).count(), 1, "{fx:?}");
    // One read in flight at a time: a timer while it is unanswered asks nothing.
    let fx = g.tick(3000);
    assert!(!fx.iter().any(is_state_read), "{fx:?}");
    // FOCUSED earns the protection; the watch is over.
    g.state_reply(true);
    assert_eq!(published.get(), Some(node().app));
    let fx = g.tick(5000);
    assert!(!fx.iter().any(is_state_read));
    assert!(timers(&fx).is_empty(), "nothing left to watch: {fx:?}");
    assert!(g.r.terminal_reads().protection_reads >= 2);
}

/// A protected terminal costs nothing, and neither does one nobody tracks.
#[test]
fn a_protected_or_untracked_terminal_arms_no_protection_watch() {
    let mut g = rig();
    let published = Arc::new(gorca_rt::FocusedApp::new());
    published.publish(node().app);
    g.r.focused_app = Some(Arc::clone(&published));
    g.focus("$ ", 2);
    g.hold_focus_on_terminal();
    let fx = g.tick(1);
    assert!(timers(&fx).is_empty(), "{fx:?}");
    g.r.text_untrack("test");
    published.demote();
    let fx = g.tick(1000);
    assert!(
        timers(&fx).is_empty() && !fx.iter().any(is_state_read),
        "{fx:?}"
    );
}
