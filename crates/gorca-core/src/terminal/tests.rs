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

use super::classify_term_key;
use crate::directory::MirrorDirectory;
use crate::mode::ModeKeys;
use crate::reader::{Reader, ReaderEffect, ReaderEffects, ReaderMsg};
use crate::text::caret::Reply;
use crate::text::{TextMsg, TextOp, TextReqId};
use gorca_ext_api::terminal::TermKey;

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

#[test]
fn keys_are_classified_for_a_terminal() {
    let code = |n: &str| static_keycode(n).unwrap_or(0);
    assert_eq!(
        classify_term_key(&ev(0, 'a' as u32, 'a' as u32, NONE)),
        TermKey::Typed('a')
    );
    assert_eq!(
        classify_term_key(&ev(0, 0x20, 0x20, NONE)),
        TermKey::Typed(' ')
    );
    assert_eq!(classify_term_key(&ev(0, 0xff0d, 13, NONE)), TermKey::Enter);
    assert_eq!(
        classify_term_key(&ev(0, 0xff08, 8, NONE)),
        TermKey::Backspace
    );
    assert_eq!(classify_term_key(&ev(0, 0xff09, 9, NONE)), TermKey::Tab);
    assert_eq!(classify_term_key(&ev(0, 0xff52, 0, NONE)), TermKey::LineUp);
    assert_eq!(
        classify_term_key(&ev(0, 0xff51, 0, NONE)),
        TermKey::CharLeft
    );
    assert_eq!(
        classify_term_key(&ev(0, 0xff51, 0, Modifiers::CONTROL)),
        TermKey::WordLeft
    );
    assert_eq!(
        classify_term_key(&ev(code("c"), 'c' as u32, 3, Modifiers::CONTROL)),
        TermKey::Interrupt
    );
    assert_eq!(
        classify_term_key(&ev(code("a"), 'a' as u32, 1, Modifiers::CONTROL)),
        TermKey::LineStart
    );
    assert_eq!(
        classify_term_key(&ev(code("e"), 'e' as u32, 5, Modifiers::CONTROL)),
        TermKey::LineEnd
    );
    assert_eq!(
        classify_term_key(&ev(code("b"), 'b' as u32, 0, Modifiers::MOD1)),
        TermKey::WordLeft
    );
    // Shift keeps the character; a lone modifier, Escape and F-keys are not characters.
    assert_eq!(
        classify_term_key(&ev(0, 'A' as u32, 'A' as u32, Modifiers::SHIFT)),
        TermKey::Typed('A')
    );
    assert_eq!(classify_term_key(&ev(0, 0xffe1, 0, NONE)), TermKey::Other);
    assert_eq!(classify_term_key(&ev(0, 0xff1b, 27, NONE)), TermKey::Other);
    assert_eq!(classify_term_key(&ev(0, 0xffbe, 0, NONE)), TermKey::Other);
    // Super and Ctrl+Alt belong to the desktop.
    assert_eq!(
        classify_term_key(&ev(0, 'x' as u32, 'x' as u32, Modifiers::MOD4)),
        TermKey::Other
    );
    // Num Lock does not change the meaning of a key.
    assert_eq!(
        classify_term_key(&ev(0, 0xff51, 0, Modifiers::MOD2)),
        TermKey::CharLeft
    );
}

#[test]
fn focusing_a_terminal_reads_the_screen_and_speaks_the_cursor_line_after_the_focus_speech() {
    let mut g = rig();
    let fx = g.focus("out\nme@box:~$ ls", 16);
    assert_eq!(
        spoken(&fx),
        ["me at box: tilde dollar ls"],
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
    assert_eq!(spoken(&fx), ["alpha beta\n dollar"]);
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
    assert_eq!(spoken(&fx), ["me at box: tilde dollar git status"]);
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
