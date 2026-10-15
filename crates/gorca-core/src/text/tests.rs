//! Reader-level tests of the text layer: keys and text events in, speech and fetches out, with
//! a fake clock. The machine itself is exercised through [`Reader::handle`], the way the
//! lanes drive it.

#![allow(clippy::wildcard_enum_match_arm)]

use std::sync::Arc;

use crate::SpeechIntent;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gorca_input_api::{KeyClass, KeyEvent, KeyIntent, Modifiers, PressKind, static_keycode};
use gorca_model::{
    AppId, ChildList, ConnectorId, Epoch, Freshness, Ifaces, LocalId, NodeKey, NodeSnapshot, Prio,
    PropMask, Role, StateSet, TextGranularity, TextSelections, TextUnitAt, TextWindowFill,
};
use gorca_rt::EpochCell;

use super::caret::Reply;
use super::caret::Spell;
use super::{TextMsg, TextOp, TextReqId};
use super::{line_spell_for, spell_for};
use crate::directory::MirrorDirectory;
use crate::mode::ModeKeys;
use crate::reader::{Reader, ReaderEffect, ReaderEffects, ReaderMsg};
use gorca_text::caret::Say;

fn node() -> NodeKey {
    NodeKey {
        app: AppId {
            connector: ConnectorId(1),
            serial: 1,
        },
        local: LocalId(7),
    }
}

fn snap(role: Role, states: StateSet) -> Arc<NodeSnapshot> {
    Arc::new(NodeSnapshot {
        key: node(),
        role,
        states,
        name: Some(Arc::from("field")),
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
    /// Milliseconds since `t0`.
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
        at: 0,
        dropped,
    }
}

fn intent_of(e: &ReaderEffect) -> Option<&SpeechIntent> {
    match e {
        ReaderEffect::Speak(s) => Some(s),
        ReaderEffect::SayAllChunk { intent, .. } => Some(intent),
        _ => None,
    }
}

fn spoken(fx: &ReaderEffects) -> Vec<String> {
    fx.iter()
        .filter_map(intent_of)
        .map(|s| {
            s.utterance
                .segments
                .iter()
                .map(|g| g.text.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

fn fetches(fx: &ReaderEffects) -> Vec<(TextReqId, TextOp)> {
    fx.iter()
        .filter_map(|e| match e {
            ReaderEffect::TextFetch { req, op, .. } => Some((*req, *op)),
            _ => None,
        })
        .collect()
}

impl Rig {
    fn now(&self) -> Instant {
        self.t0 + Duration::from_millis(self.at)
    }

    fn advance(&mut self, ms: u64) {
        self.at += ms;
    }

    fn msg(&mut self, m: ReaderMsg) -> ReaderEffects {
        let now = self.now();
        self.r.handle(m, now)
    }

    /// Track a text object and answer the prefetch with `text` (caret at `caret`).
    fn track(&mut self, role: Role, states: StateSet, text: &str, caret: u32) {
        let s = snap(role, states);
        let mut fx = ReaderEffects::new();
        self.r.now = self.now();
        self.r.text_track(&s, &mut fx);
        let f = fetches(&fx);
        assert_eq!(f.len(), 1, "one prefetch on focus: {fx:?}");
        let (req, op) = &f[0];
        assert!(matches!(
            op,
            TextOp::Window {
                around: None,
                radius: 2048,
                ..
            }
        ));
        let len = u32::try_from(text.chars().count()).unwrap_or(0);
        let reply = Reply::Window(TextWindowFill {
            base: 0,
            text: Arc::from(text),
            len,
            caret: Some(caret),
        });
        let _ = self.msg(ReaderMsg::Text(TextMsg::Fetched {
            req: *req,
            epoch: Epoch(0),
            result: Ok(reply),
        }));
    }

    fn entry(&mut self, text: &str, caret: u32) {
        self.track(
            Role::TextEntry,
            StateSet::EDITABLE | StateSet::FOCUSED,
            text,
            caret,
        );
    }

    fn area(&mut self, text: &str, caret: u32) {
        self.track(
            Role::TextEntry,
            StateSet::EDITABLE | StateSet::FOCUSED | StateSet::MULTILINE,
            text,
            caret,
        );
    }

    fn key(&mut self, keysym: u32, unichar: u32, mods: Modifiers) -> ReaderEffects {
        self.key_code(0, keysym, unichar, mods)
    }

    fn key_code(&mut self, code: u16, keysym: u32, unichar: u32, mods: Modifiers) -> ReaderEffects {
        let epoch = self.epoch.advance();
        self.key_with_epoch(code, keysym, unichar, mods, epoch)
    }

    /// A key whose epoch the input lane already advanced to (possibly before this call).
    fn key_with_epoch(
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

    fn caret(&mut self, off: u32) -> ReaderEffects {
        self.msg(ReaderMsg::Text(TextMsg::Caret {
            node: node(),
            offset: off,
        }))
    }

    fn insert(&mut self, offset: u32, text: &str) -> ReaderEffects {
        self.msg(ReaderMsg::Text(TextMsg::Edit {
            node: node(),
            delta: gorca_model::TextDelta::Insert {
                offset,
                len: u32::try_from(text.chars().count()).unwrap_or(0),
                text: Some(gorca_model::LazyText {
                    msg: Arc::from(text.as_bytes()),
                    field: 0,
                }),
            },
        }))
    }

    fn delete(&mut self, offset: u32, text: Option<&str>, len: u32) -> ReaderEffects {
        self.msg(ReaderMsg::Text(TextMsg::Edit {
            node: node(),
            delta: gorca_model::TextDelta::Delete {
                offset,
                len,
                text: text.map(|t| gorca_model::LazyText {
                    msg: Arc::from(t.as_bytes()),
                    field: 0,
                }),
            },
        }))
    }

    fn selection_event(&mut self) -> ReaderEffects {
        self.msg(ReaderMsg::Text(TextMsg::Selection { node: node() }))
    }

    fn tick(&mut self, ms: u64) -> ReaderEffects {
        self.advance(ms);
        self.msg(ReaderMsg::Timer)
    }

    fn reply(&mut self, req: TextReqId, r: Reply) -> ReaderEffects {
        self.msg(ReaderMsg::Text(TextMsg::Fetched {
            req,
            epoch: self.epoch.current(),
            result: Ok(r),
        }))
    }
}

const NONE: Modifiers = Modifiers::empty();
const LEFT: u32 = 0xff51;
const UP: u32 = 0xff52;
const RIGHT: u32 = 0xff53;
const DOWN: u32 = 0xff54;
const BACKSPACE: u32 = 0xff08;

#[test]
fn right_arrow_speaks_the_character_from_the_window_with_no_fetch() {
    let mut g = rig();
    g.entry("hello world", 0);
    let fx = g.key(RIGHT, 0, NONE);
    assert!(spoken(&fx).is_empty(), "the key itself is not echoed");
    let fx = g.caret(1);
    assert_eq!(spoken(&fx), ["e"]);
    assert!(fetches(&fx).is_empty(), "C1h: the window answers");
    let ReaderEffect::Speak(s) = &fx[0] else {
        panic!("{fx:?}")
    };
    assert_eq!(s.utterance.prio, Prio::Interrupt);
    assert_eq!(s.utterance.epoch, g.epoch.current(), "the key's epoch");
    assert_eq!(g.r.text_counters().caret_correlated, 1);
}

#[test]
fn ctrl_right_speaks_the_word_and_home_the_character() {
    let mut g = rig();
    g.entry("hello big world", 0);
    let _ = g.key(RIGHT, 0, Modifiers::CONTROL);
    let fx = g.caret(6);
    assert_eq!(spoken(&fx), ["big"], "a word, without its trailing space");
    let _ = g.key(0xff50, 0, NONE);
    let fx = g.caret(0);
    assert_eq!(spoken(&fx), ["h"], "Home speaks the character at the caret");
}

#[test]
fn a_space_and_the_end_of_text_have_names() {
    let mut g = rig();
    g.entry("a b", 0);
    let _ = g.key(RIGHT, 0, NONE);
    assert_eq!(spoken(&g.caret(1)), ["space"]);
    let _ = g.key(RIGHT, 0, NONE);
    let _ = g.caret(2);
    let _ = g.key(RIGHT, 0, NONE);
    assert_eq!(spoken(&g.caret(3)), ["blank"], "behind the last character");
}

#[test]
fn down_arrow_in_a_text_view_asks_the_toolkit_for_the_line_once() {
    let mut g = rig();
    g.area("ab\ncd\nef", 0);
    let _ = g.key(DOWN, 0, NONE);
    let fx = g.caret(3);
    assert!(spoken(&fx).is_empty(), "waits for the line");
    let f = fetches(&fx);
    assert_eq!(f.len(), 1, "C1: exactly one fetch");
    assert_eq!(
        f[0].1,
        TextOp::AtOffset {
            node: node(),
            offset: 3,
            unit: TextGranularity::Line
        }
    );
    let fx = g.reply(
        f[0].0,
        Reply::Unit(TextUnitAt {
            range: 3..6,
            text: Arc::from("cd\n"),
        }),
    );
    assert_eq!(spoken(&fx), ["cd"]);
    // The line is cached: stepping into it again needs no fetch (TW-3).
    let _ = g.key(UP, 0, NONE);
    let fx = g.caret(0);
    assert_eq!(fetches(&fx).len(), 1, "line 0 is new");
}

#[test]
fn a_single_line_entry_is_one_line_from_the_window() {
    let mut g = rig();
    g.entry("just one line", 3);
    let _ = g.key(DOWN, 0, NONE);
    let fx = g.caret(13);
    assert_eq!(spoken(&fx), ["just one line"]);
    assert!(fetches(&fx).is_empty());
}

#[test]
fn a_key_that_moves_nothing_speaks_the_unit_at_the_caret_after_the_edge_time() {
    let mut g = rig();
    g.entry("hello", 0);
    let fx = g.key(LEFT, 0, NONE);
    assert!(spoken(&fx).is_empty());
    assert!(
        fx.iter().any(|e| matches!(e, ReaderEffect::ArmTimer(_))),
        "the edge timer is armed"
    );
    let fx = g.tick(50);
    assert!(spoken(&fx).is_empty(), "not yet");
    let fx = g.tick(60);
    assert_eq!(spoken(&fx), ["h"], "CR-3: the boundary speaks the unit");
    assert_eq!(g.r.text_counters().caret_edge, 1);
    // CR-3b: the slow app's caret event still arrives and is spoken with the key's unit.
    let fx = g.caret(1);
    assert_eq!(spoken(&fx), ["e"]);
    assert_eq!(g.r.text_counters().caret_late, 1);
}

#[test]
fn an_unexplained_caret_move_is_held_briefly_then_spoken_by_distance() {
    let mut g = rig();
    g.entry("hello big world", 0);
    let fx = g.caret(1);
    assert!(spoken(&fx).is_empty(), "held for the key that may follow");
    let fx = g.tick(6);
    assert_eq!(
        spoken(&fx),
        ["e"],
        "CR-4: one scalar, same line: a character"
    );
    let _ = g.caret(12);
    let fx = g.tick(6);
    assert_eq!(
        spoken(&fx),
        ["hello big world"],
        "CR-4: a jump speaks the line"
    );
    assert_eq!(g.r.text_counters().caret_uncorrelated, 2);
}

#[test]
fn a_caret_event_that_beats_its_key_is_still_correlated() {
    let mut g = rig();
    g.entry("hello big world", 0);
    let fx = g.caret(6);
    assert!(spoken(&fx).is_empty());
    g.advance(2);
    let fx = g.key(RIGHT, 0, Modifiers::CONTROL);
    assert_eq!(spoken(&fx), ["big"], "EV-4");
    assert_eq!(g.r.text_counters().caret_held_correlated, 1);
    let fx = g.tick(10);
    assert!(spoken(&fx).is_empty(), "nothing is spoken twice (CS-3)");
}

#[test]
fn a_held_arrow_speaks_each_position_with_its_own_epoch() {
    let mut g = rig();
    g.entry("abcdefghij", 0);
    let mut last = Epoch(0);
    for i in 1..=5u32 {
        let _ = g.key(RIGHT, 0, NONE);
        g.advance(30);
        let fx = g.caret(i);
        let ReaderEffect::Speak(s) = &fx[0] else {
            panic!("{fx:?}")
        };
        assert!(s.utterance.epoch > last, "newer than the previous key");
        last = s.utterance.epoch;
    }
}

#[test]
fn typed_characters_are_spoken_once_from_the_key_and_not_again_from_the_event() {
    let mut g = rig();
    g.entry("", 0);
    let fx = g.typed('a');
    assert_eq!(spoken(&fx), ["a"], "exactly one echo, no generic key echo");
    let fx = g.insert(0, "a");
    assert!(
        spoken(&fx).is_empty(),
        "TE-2: the insert is the echo's twin"
    );
    let fx = g.caret(1);
    assert!(
        spoken(&fx).is_empty(),
        "CS-3: the caret move of the insert is absorbed"
    );
    assert_eq!(g.r.text_counters().typed_echoed, 1);
}

#[test]
fn a_caret_event_ahead_of_its_insert_is_absorbed_too() {
    let mut g = rig();
    g.entry("", 0);
    let _ = g.typed('a');
    let fx = g.caret(1);
    assert!(spoken(&fx).is_empty());
    let fx = g.tick(10);
    assert!(spoken(&fx).is_empty());
    assert!(spoken(&g.insert(0, "a")).is_empty());
}

#[test]
fn fast_typing_coalesced_into_one_insert_is_not_spoken_again() {
    let mut g = rig();
    g.entry("", 0);
    let mut said = 0;
    for c in "при".chars() {
        said += spoken(&g.typed(c)).len();
    }
    assert_eq!(said, 3, "every character is echoed (Cyrillic too)");
    assert!(spoken(&g.insert(0, "при")).is_empty());
}

#[test]
fn typed_characters_can_be_switched_off_without_breaking_dedupe() {
    let mut g = rig();
    g.entry("", 0);
    g.r.cmd.overlay.typed_characters = Some(false);
    g.r.cmd.republish();
    let fx = g.typed('x');
    assert!(spoken(&fx).is_empty(), "the setting is honoured");
    assert!(
        spoken(&g.insert(0, "x")).is_empty(),
        "and the insert is still recognised as typed"
    );
}

#[test]
fn typed_words_speak_the_word_when_its_separator_arrives() {
    let mut g = rig();
    g.r.cmd.overlay.typed_words = Some(true);
    g.r.cmd.republish();
    g.entry("", 0);
    for (i, c) in "hi".chars().enumerate() {
        let _ = g.typed(c);
        let _ = g.insert(u32::try_from(i).unwrap_or(0), &c.to_string());
    }
    let fx = g.typed(' ');
    assert_eq!(spoken(&fx), ["space"]);
    let fx = g.insert(2, " ");
    assert_eq!(spoken(&fx), ["hi"], "TE-4: the word, read from the window");
}

#[test]
fn space_and_return_have_names_when_typed() {
    let mut g = rig();
    g.area("", 0);
    assert_eq!(spoken(&g.typed(' ')), ["space"]);
    let fx = g.key(0xff0d, 0x0d, NONE);
    assert_eq!(spoken(&fx), ["new line"], "TE-6");
}

#[test]
fn return_in_a_single_line_entry_is_the_generic_echo() {
    let mut g = rig();
    g.entry("x", 1);
    let fx = g.key(0xff0d, 0x0d, NONE);
    assert_eq!(spoken(&fx), ["enter"], "an entry's Return is activation");
}

#[test]
fn paste_is_spoken_as_the_inserted_text() {
    let mut g = rig();
    g.entry("ab", 2);
    let v = static_keycode("v").unwrap_or(0);
    let fx = g.key_code(v, 0x76, 0, Modifiers::CONTROL);
    assert!(spoken(&fx).is_empty());
    let fx = g.insert(2, "pasted words");
    assert_eq!(spoken(&fx), ["pasted words"], "TE-3");
    assert!(spoken(&g.caret(14)).is_empty());
}

#[test]
fn an_insert_nobody_typed_is_spoken_but_a_flood_is_thinned() {
    let mut g = rig();
    g.entry("", 0);
    let fx = g.insert(0, "from an input method");
    assert_eq!(spoken(&fx), ["from an input method"]);
    g.advance(10);
    let _ = g.insert(20, "second");
    assert!(spoken(&g.tick(6)).is_empty(), "EV-5: it waits for the gap");
    assert_eq!(
        spoken(&g.tick(300)),
        ["second"],
        "and is spoken once it has passed"
    );
    // Burst of inserts inside one gap: only what is held when the gap ends is spoken.
    let _ = g.insert(26, "third");
    assert!(spoken(&g.tick(6)).is_empty());
    assert_eq!(spoken(&g.tick(300)), ["third"]);
}

#[test]
fn touching_unexplained_inserts_are_spoken_as_one_net_insert() {
    let mut g = rig();
    g.entry("", 0);
    let _ = g.insert(0, "hel");
    let _ = g.insert(3, "lo ");
    assert!(spoken(&g.tick(2)).is_empty());
    assert_eq!(spoken(&g.tick(6)), ["hello"]);
    assert_eq!(g.r.text_counters().inserts_coalesced, 1);
    // An insert elsewhere is not folded in.
    g.advance(400);
    let _ = g.insert(0, "ab");
    let _ = g.insert(40, "cd");
    assert_eq!(g.r.text_counters().inserts_coalesced, 1);
}

#[test]
fn a_delete_that_beats_its_key_is_still_spoken() {
    let mut g = rig();
    g.entry("hello", 5);
    assert!(
        spoken(&g.delete(4, Some("o"), 1)).is_empty(),
        "held for its key"
    );
    g.advance(2);
    let fx = g.key(BACKSPACE, 8, NONE);
    assert_eq!(spoken(&fx), ["o"], "TE-5, once");
    assert_eq!(g.r.text_counters().delete_held_claimed, 1);
    assert!(spoken(&g.caret(4)).is_empty(), "the caret move is absorbed");
    assert!(spoken(&g.tick(300)).is_empty(), "no boundary speech either");
}

#[test]
fn a_delete_nobody_claims_is_dropped_silently() {
    let mut g = rig();
    g.entry("hello", 5);
    assert!(spoken(&g.delete(4, Some("o"), 1)).is_empty());
    assert!(spoken(&g.tick(6)).is_empty());
    // A BackSpace much later finds nothing held and waits for its own delete as usual.
    g.advance(100);
    let _ = g.key(BACKSPACE, 8, NONE);
    assert_eq!(spoken(&g.delete(3, Some("l"), 1)), ["l"]);
}

#[test]
fn a_key_that_trails_an_insert_spoken_in_its_own_epoch_stays_silent() {
    // The input lane advanced the epoch for the key and the insert overtook the key in the
    // Reader: the insert was spoken in the key's epoch, so it is not cancelled by the key.
    let mut g = rig();
    g.entry("", 0);
    let _ = g.insert(0, "a");
    let key_epoch = g.epoch.advance();
    assert_eq!(
        spoken(&g.tick(6)),
        ["a"],
        "unclaimed: spoken as inserted text"
    );
    g.advance(20);
    let fx = g.key_with_epoch(0, 'a' as u32, 'a' as u32, NONE, key_epoch);
    assert!(spoken(&fx).is_empty(), "the key's echo would be the second");
    // The next typed character is a new one.
    g.advance(400);
    assert_eq!(spoken(&g.typed('a')), ["a"]);
}

/// Bug A/B of the M3 E2 flake: the insert was spoken before its key reached the input lane, so
/// the key's epoch cancels that speech. The key must echo itself, not stay silent for a speech
/// that no longer exists.
#[test]
fn a_key_that_trails_an_insert_spoken_in_an_older_epoch_echoes_itself() {
    let mut g = rig();
    g.entry("", 0);
    let _ = g.insert(0, "a");
    assert_eq!(spoken(&g.tick(6)), ["a"]);
    let spoken_epoch = g.epoch.current();
    g.advance(20);
    let fx = g.typed('a');
    let echoes: Vec<_> = fx.iter().filter_map(intent_of).collect();
    assert_eq!(echoes.len(), 1, "the key echoes itself");
    assert!(echoes[0].utterance.epoch > spoken_epoch);
    // Nothing more is expected: the insert is already here, so it is not spoken again.
    assert!(spoken(&g.tick(20)).is_empty());
    assert!(spoken(&g.caret(1)).is_empty());
}

#[test]
fn an_insert_that_beats_its_key_is_the_keys_echo() {
    // The application's signal can overtake the keyboard monitor's (separate sockets).
    let mut g = rig();
    g.entry("", 0);
    assert!(spoken(&g.insert(0, "a")).is_empty(), "held for its key");
    g.advance(2);
    assert_eq!(
        spoken(&g.typed('a')),
        ["a"],
        "the key's echo is the only one"
    );
    assert_eq!(g.r.text_counters().insert_held_claimed, 1);
    assert!(spoken(&g.tick(10)).is_empty(), "nothing is spoken twice");
    assert!(spoken(&g.caret(1)).is_empty());
}

#[test]
fn a_fast_burst_that_beats_its_keys_is_claimed_key_by_key() {
    let mut g = rig();
    g.entry("", 0);
    assert!(spoken(&g.insert(0, "ab")).is_empty());
    assert_eq!(spoken(&g.typed('a')), ["a"]);
    assert_eq!(spoken(&g.typed('b')), ["b"]);
    assert!(spoken(&g.tick(10)).is_empty());
}

#[test]
fn a_short_insert_nobody_claims_is_spoken_when_the_hold_ends() {
    let mut g = rig();
    g.entry("", 0);
    assert!(spoken(&g.insert(0, "é")).is_empty());
    assert!(spoken(&g.tick(2)).is_empty(), "still waiting");
    assert_eq!(spoken(&g.tick(5)), ["é"]);
    // A key that is not a typed character does not explain an insert before it.
    let mut g = rig();
    g.entry("", 0);
    let _ = g.insert(0, "x");
    let fx = g.key(RIGHT, 0, NONE);
    assert_eq!(spoken(&fx), ["x"]);
}

#[test]
fn an_edit_does_not_cancel_a_presentation_in_flight() {
    let mut g = rig();
    g.area("ab\ncd\nef", 0);
    let _ = g.key(DOWN, 0, NONE);
    let f = fetches(&g.caret(3));
    assert_eq!(f.len(), 1);
    // A text event of the same object arrives while the line is being fetched.
    let _ = g.msg(ReaderMsg::Text(TextMsg::Edit {
        node: node(),
        delta: gorca_model::TextDelta::Replaced { text: None },
    }));
    let fx = g.reply(
        f[0].0,
        Reply::Unit(TextUnitAt {
            range: 3..6,
            text: Arc::from("cd\n"),
        }),
    );
    assert_eq!(spoken(&fx), ["cd"]);
}

#[test]
fn extending_a_long_selection_by_a_character_reads_that_character() {
    // The fetched text of a selection over 1,024 scalars is left out; the change is small and
    // the window has it.
    let mut g = rig();
    let text: String = (0..1500u32)
        .map(|i| char::from(b'a' + u8::try_from(i % 26).unwrap_or(0)))
        .collect();
    g.entry(&text, 0);
    let sel = |end: u32| {
        Reply::Selections(TextSelections {
            ranges: vec![(0..end, Arc::from(""))],
        })
    };
    let f = fetches(&g.selection_event());
    let _ = g.reply(f[0].0, sel(1100));
    let _ = g.key(RIGHT, 0, Modifiers::SHIFT);
    let f = fetches(&g.caret(1101));
    let fx = g.reply(f[0].0, sel(1101));
    let want = char::from(b'a' + u8::try_from(1100 % 26).unwrap_or(0));
    assert_eq!(spoken(&fx), [format!("{want} selected")]);
}

#[test]
fn inserted_text_can_be_switched_off() {
    let mut g = rig();
    g.entry("", 0);
    g.r.cmd.effective = {
        let mut s = (*g.r.cmd.effective).clone();
        s.echo.inserted_text = false;
        Arc::new(s)
    };
    assert!(spoken(&g.insert(0, "quiet")).is_empty());
}

#[test]
fn backspace_speaks_the_deleted_character_and_not_its_own_name() {
    let mut g = rig();
    g.entry("hello", 5);
    let fx = g.key(BACKSPACE, 0x08, NONE);
    assert!(
        spoken(&fx).is_empty(),
        "TE-5: no key-name echo in an editable"
    );
    let fx = g.delete(4, Some("o"), 1);
    assert_eq!(spoken(&fx), ["o"]);
    assert!(
        spoken(&g.caret(4)).is_empty(),
        "the caret move is not spoken"
    );
}

#[test]
fn delete_without_a_payload_reads_the_window_before_the_splice() {
    let mut g = rig();
    g.entry("hello", 1);
    let fx = g.key(0xffff, 0x7f, NONE);
    assert!(spoken(&fx).is_empty());
    let fx = g.delete(1, None, 1);
    assert_eq!(spoken(&fx), ["e"]);
}

#[test]
fn ctrl_backspace_speaks_the_deleted_word() {
    let mut g = rig();
    g.entry("say hello", 9);
    let _ = g.key(BACKSPACE, 0x08, Modifiers::CONTROL);
    assert_eq!(spoken(&g.delete(4, Some("hello"), 5)), ["hello"]);
}

#[test]
fn nothing_deleted_at_the_boundary_is_silent() {
    let mut g = rig();
    g.entry("", 0);
    let _ = g.key(BACKSPACE, 0x08, NONE);
    assert!(spoken(&g.tick(150)).is_empty());
}

#[test]
fn backspace_outside_a_text_field_keeps_the_generic_echo() {
    let mut g = rig();
    let fx = g.key(BACKSPACE, 0x08, NONE);
    assert_eq!(spoken(&fx), ["backspace"]);
}

#[test]
fn shift_arrows_announce_selected_and_unselected_text() {
    let mut g = rig();
    g.entry("hello", 0);
    let _ = g.key(RIGHT, 0, Modifiers::SHIFT);
    let fx = g.caret(1);
    let f = fetches(&fx);
    assert_eq!(f.len(), 1, "S2: one selection fetch");
    assert_eq!(f[0].1, TextOp::Selections { node: node() });
    // The selection event of the same keystroke brings nothing new.
    assert!(fetches(&g.selection_event()).is_empty());
    let sel = |r: std::ops::Range<u32>, t: &str| {
        Reply::Selections(TextSelections {
            ranges: vec![(r, Arc::from(t))],
        })
    };
    let fx = g.reply(f[0].0, sel(0..1, "h"));
    assert_eq!(spoken(&fx), ["h selected"]);

    let _ = g.key(RIGHT, 0, Modifiers::SHIFT);
    let f = fetches(&g.caret(2));
    let fx = g.reply(f[0].0, sel(0..2, "he"));
    assert_eq!(spoken(&fx), ["e selected"], "only the delta is spoken");

    let _ = g.key(LEFT, 0, Modifiers::SHIFT);
    let f = fetches(&g.caret(1));
    let fx = g.reply(f[0].0, sel(0..1, "h"));
    assert_eq!(spoken(&fx), ["e unselected"]);
}

#[test]
fn a_collapse_by_a_plain_move_is_not_announced() {
    let mut g = rig();
    g.entry("hello", 0);
    let _ = g.key(RIGHT, 0, Modifiers::SHIFT);
    let f = fetches(&g.caret(1));
    let _ = g.reply(
        f[0].0,
        Reply::Selections(TextSelections {
            ranges: vec![(0..1, Arc::from("h"))],
        }),
    );
    let _ = g.key(RIGHT, 0, NONE);
    let fx = g.caret(2);
    assert_eq!(spoken(&fx), ["l"], "the move's unit, not 'unselected'");
    assert!(
        fetches(&g.selection_event()).is_empty(),
        "the collapse event is ignored"
    );
}

#[test]
fn select_all_says_all_selected_with_the_count() {
    let mut g = rig();
    g.entry("hello", 0);
    let a = static_keycode("a").unwrap_or(0);
    let _ = g.key_code(a, 0x61, 0, Modifiers::CONTROL);
    let f = fetches(&g.selection_event());
    assert_eq!(f.len(), 1);
    let fx = g.reply(
        f[0].0,
        Reply::Selections(TextSelections {
            ranges: vec![(0..5, Arc::from("hello"))],
        }),
    );
    assert_eq!(spoken(&fx), ["all selected, 5 characters"]);
}

#[test]
fn a_mouse_selection_is_announced_from_the_selection_event() {
    let mut g = rig();
    g.entry("hello world", 0);
    let f = fetches(&g.selection_event());
    let fx = g.reply(
        f[0].0,
        Reply::Selections(TextSelections {
            ranges: vec![(6..11, Arc::from("world"))],
        }),
    );
    assert_eq!(spoken(&fx), ["world selected"]);
}

#[test]
fn a_long_selection_is_counted_not_read() {
    let mut g = rig();
    g.entry(&"x".repeat(2000), 0);
    let f = fetches(&g.selection_event());
    let fx = g.reply(
        f[0].0,
        Reply::Selections(TextSelections {
            ranges: vec![(0..1500, Arc::from(""))],
        }),
    );
    assert_eq!(spoken(&fx), ["1500 characters selected"]);
}

#[test]
fn a_failed_selection_fetch_falls_back_to_the_caret_unit() {
    let mut g = rig();
    g.entry("hello", 0);
    let _ = g.key(RIGHT, 0, Modifiers::SHIFT);
    let f = fetches(&g.caret(1));
    let fx = g.msg(ReaderMsg::Text(TextMsg::Fetched {
        req: f[0].0,
        epoch: g.epoch.current(),
        result: Err(gorca_connector_api::BackendError::Timeout),
    }));
    assert_eq!(spoken(&fx), ["e"]);
    assert_eq!(g.r.text_counters().sel_fetch_failed, 1);
}

#[test]
fn a_password_field_is_spoken_like_any_other_and_marked_sensitive() {
    let mut g = rig();
    g.track(
        Role::PasswordEntry,
        StateSet::EDITABLE | StateSet::FOCUSED,
        "",
        0,
    );
    let fx = g.typed('s');
    assert_eq!(spoken(&fx), ["s"], "the user reads everything");
    let ReaderEffect::Speak(s) = &fx[0] else {
        panic!("{fx:?}")
    };
    assert_eq!(g.r.text.sensitive_utt, Some(s.utterance.id));
    let _ = g.insert(0, "s");
    let dbg = format!("{:?}", g.r.text.caret);
    assert!(!dbg.contains("\"s\""), "Debug shows lengths only: {dbg}");
    // After other speech the flag is for that utterance only.
    g.r.cmd.last_spoken = Some(s.utterance.clone());
    assert!(g.r.last_spoken_is_sensitive());
    let other = {
        let mut u = s.utterance.clone();
        u.id = gorca_model::UttId(9999);
        u
    };
    g.r.cmd.last_spoken = Some(other);
    assert!(!g.r.last_spoken_is_sensitive());
}

#[test]
fn a_secret_window_is_dropped_with_the_focus() {
    let mut g = rig();
    g.track(Role::PasswordEntry, StateSet::EDITABLE, "hunter2", 7);
    assert!(g.r.text.caret.tracked().is_some());
    g.r.text_untrack();
    assert!(g.r.text.caret.is_off());
}

#[test]
fn non_text_roles_are_not_tracked() {
    let mut g = rig();
    let mut fx = ReaderEffects::new();
    g.r.now = g.now();
    g.r.text_track(&snap(Role::Button, StateSet::FOCUSED), &mut fx);
    assert!(g.r.text.caret.is_off());
    g.r.text_track(&snap(Role::Terminal, StateSet::EDITABLE), &mut fx);
    assert!(g.r.text.caret.is_off(), "the terminal belongs to M6");
    g.r.text_track(&snap(Role::Document, StateSet::EDITABLE), &mut fx);
    assert!(!g.r.text.caret.is_off());
}

#[test]
fn a_window_miss_fetches_once_and_then_speaks() {
    let mut g = rig();
    // A text longer than the window: the caret event lands outside it.
    g.entry("abcdef", 0);
    // Make the window smaller than the text: pretend it holds [0, 6) of 100.
    if let Some(t) = g.r.text.caret.tracked_mut() {
        t.win.fill(&TextWindowFill {
            base: 0,
            text: Arc::from("abcdef"),
            len: 100,
            caret: None,
        });
    }
    let _ = g.key(RIGHT, 0, NONE);
    let fx = g.caret(50);
    let f = fetches(&fx);
    assert_eq!(f.len(), 1);
    assert!(matches!(
        f[0].1,
        TextOp::Window {
            around: Some(50),
            ..
        }
    ));
    let fx = g.reply(
        f[0].0,
        Reply::Window(TextWindowFill {
            base: 0,
            text: Arc::from("x".repeat(100).as_str()),
            len: 100,
            caret: None,
        }),
    );
    assert_eq!(spoken(&fx), ["x"]);
}

#[test]
fn a_hung_fetch_gives_up_at_its_deadline_and_a_line_falls_back_to_the_window() {
    let mut g = rig();
    g.area("ab\ncd\n", 0);
    let _ = g.key(DOWN, 0, NONE);
    let fx = g.caret(3);
    assert_eq!(fetches(&fx).len(), 1);
    // No reply ever comes. The deadline timer ends the wait.
    let fx = g.tick(400);
    assert_eq!(spoken(&fx), ["cd"], "the window knows the hard line");
    assert_eq!(g.r.text_counters().fetch_timeout, 1);
    assert!(g.r.text.caret.tracked().is_some());
}

#[test]
fn a_failed_character_fetch_says_nothing() {
    let mut g = rig();
    g.entry("abcdef", 0);
    if let Some(t) = g.r.text.caret.tracked_mut() {
        t.win.fill(&TextWindowFill {
            base: 0,
            text: Arc::from("abcdef"),
            len: 100,
            caret: None,
        });
    }
    let _ = g.key(RIGHT, 0, NONE);
    let f = fetches(&g.caret(50));
    let fx = g.msg(ReaderMsg::Text(TextMsg::Fetched {
        req: f[0].0,
        epoch: g.epoch.current(),
        result: Err(gorca_connector_api::BackendError::Gone),
    }));
    assert!(spoken(&fx).is_empty());
}

#[test]
fn lost_events_make_the_window_untrusted() {
    let mut g = rig();
    g.entry("hello", 0);
    g.dropped.fetch_add(3, Ordering::Relaxed);
    let _ = g.key(RIGHT, 0, NONE);
    let fx = g.caret(1);
    assert!(
        spoken(&fx).is_empty(),
        "the window was emptied: fetch first"
    );
    assert_eq!(fetches(&fx).len(), 1);
    assert_eq!(g.r.text_counters().events_dropped, 3);
}

#[test]
fn a_reply_computed_before_an_edit_does_not_overwrite_the_window() {
    let mut g = rig();
    g.entry("hello", 0);
    if let Some(t) = g.r.text.caret.tracked_mut() {
        t.win.fill(&TextWindowFill {
            base: 0,
            text: Arc::from("hello"),
            len: 5,
            caret: None,
        });
    }
    // A refill is requested ...
    let mut fx = ReaderEffects::new();
    g.r.now = g.now();
    g.r.text_track(&snap(Role::TextEntry, StateSet::EDITABLE), &mut fx);
    let f = fetches(&fx);
    // ... then the text changes before the reply arrives.
    let _ = g.insert(0, "X");
    let _ = g.reply(
        f[0].0,
        Reply::Window(TextWindowFill {
            base: 0,
            text: Arc::from("hello"),
            len: 5,
            caret: Some(0),
        }),
    );
    let t = g.r.text.caret.tracked();
    assert_eq!(
        t.and_then(|t| t.win.read(0..6)).as_deref(),
        Some("Xhello"),
        "the stale fill was discarded"
    );
}

#[test]
fn text_events_of_another_node_are_ignored() {
    let mut g = rig();
    g.entry("hello", 0);
    let other = NodeKey {
        local: LocalId(99),
        ..node()
    };
    let fx = g.msg(ReaderMsg::Text(TextMsg::Caret {
        node: other,
        offset: 3,
    }));
    assert!(fx.is_empty());
}

#[test]
fn text_commands_speak_the_unit_at_the_caret_and_spell_it() {
    let mut g = rig();
    g.entry("hello world", 6);
    let mut fx = ReaderEffects::new();
    let e = g.epoch.advance();
    g.r.now = g.now();
    g.r.text_say_unit(Say::Char, Spell::No, e, &mut fx);
    g.r.text_say_unit(Say::Word, Spell::No, e, &mut fx);
    g.r.text_say_unit(Say::Word, spell_for(1), e, &mut fx);
    g.r.text_say_unit(Say::Char, spell_for(2), e, &mut fx);
    g.r.text_say_unit(Say::Line, line_spell_for(1), e, &mut fx);
    assert_eq!(
        spoken(&fx),
        ["w", "world", "w, o, r, l, d", "whiskey", "hello world"]
    );
}

#[test]
fn text_commands_outside_a_text_field_say_so() {
    let mut g = rig();
    let mut fx = ReaderEffects::new();
    let e = g.epoch.advance();
    g.r.text_say_unit(Say::Char, Spell::No, e, &mut fx);
    assert_eq!(spoken(&fx), ["not in a text field"]);
}

#[test]
fn info_selection_speaks_the_last_selection() {
    let mut g = rig();
    g.entry("hello world", 0);
    let mut fx = ReaderEffects::new();
    let e = g.epoch.advance();
    g.r.text_say_selection(e, &mut fx);
    assert_eq!(spoken(&fx), ["no selection"]);
    let f = fetches(&g.selection_event());
    let _ = g.reply(
        f[0].0,
        Reply::Selections(TextSelections {
            ranges: vec![(6..11, Arc::from("world"))],
        }),
    );
    let mut fx = ReaderEffects::new();
    g.r.text_say_selection(e, &mut fx);
    assert_eq!(spoken(&fx), ["world"]);
}

// ---- say-all -------------------------------------------------------------------------------

fn say_all_start(g: &mut Rig, text: &str) -> (ReaderEffects, Vec<(TextReqId, TextOp)>) {
    g.entry(text, 0);
    let e = g.epoch.advance();
    g.r.now = g.now();
    let mut fx = ReaderEffects::new();
    g.r.say_all_start(e, &mut fx);
    let f = fetches(&fx);
    (fx, f)
}

fn utt_ids(fx: &ReaderEffects) -> Vec<gorca_model::UttId> {
    fx.iter()
        .filter_map(intent_of)
        .map(|s| s.utterance.id)
        .collect()
}

fn firsts(fx: &ReaderEffects) -> Vec<bool> {
    fx.iter()
        .filter_map(|e| match e {
            ReaderEffect::SayAllChunk { first, .. } => Some(*first),
            _ => None,
        })
        .collect()
}

fn progress(g: &mut Rig, p: gorca_speech::LaneProgress) -> ReaderEffects {
    g.msg(ReaderMsg::Text(TextMsg::Progress(p)))
}

fn window(text: &str) -> Reply {
    Reply::Window(TextWindowFill {
        base: 0,
        text: Arc::from(text),
        len: u32::try_from(text.chars().count()).unwrap_or(0),
        caret: None,
    })
}

fn set_carets(fx: &ReaderEffects) -> Vec<u32> {
    fetches(fx)
        .into_iter()
        .filter_map(|(_, op)| match op {
            TextOp::SetCaret { offset, .. } => Some(offset),
            _ => None,
        })
        .collect()
}

#[test]
fn say_all_submits_three_chunks_at_once_and_only_the_first_interrupts() {
    use gorca_speech::LaneProgress as P;
    let mut g = rig();
    let text = "One. Two. Three. Four. Five.";
    let (fx, f) = say_all_start(&mut g, text);
    assert!(spoken(&fx).is_empty());
    assert_eq!(f.len(), 1, "SA1: one fetch for the first chunk");
    let fx = g.reply(f[0].0, window(text));
    assert_eq!(
        spoken(&fx),
        ["One.", "Two.", "Three."],
        "look-ahead of three"
    );
    assert_eq!(firsts(&fx), [true, false, false]);
    let ids = utt_ids(&fx);
    let fx = progress(&mut g, P::Started(ids[0]));
    assert!(
        spoken(&fx).is_empty() && set_carets(&fx).is_empty(),
        "the caret is already there"
    );
    let fx = progress(&mut g, P::Ended(ids[0]));
    assert_eq!(
        spoken(&fx),
        ["Four."],
        "SA2: the next chunk is queued before the previous ends"
    );
    assert_eq!(firsts(&fx), [false]);
    let fx = progress(&mut g, P::Started(ids[1]));
    assert_eq!(set_carets(&fx), [5], "the caret follows the reading");
}

#[test]
fn a_key_press_stops_say_all_and_the_caret_is_already_where_it_stopped() {
    use gorca_speech::LaneProgress as P;
    let mut g = rig();
    let text = "One. Two. Three.";
    let (_, f) = say_all_start(&mut g, text);
    let fx = g.reply(f[0].0, window(text));
    let ids = utt_ids(&fx);
    let _ = progress(&mut g, P::Started(ids[0]));
    let _ = progress(&mut g, P::Ended(ids[0]));
    let fx = progress(&mut g, P::Started(ids[1]));
    assert_eq!(set_carets(&fx), [5]);
    // The caret event of gorca's own move is not the user's.
    assert!(spoken(&g.caret(5)).is_empty());
    // Any key advances the epoch; the next message ends the reading (SA3) and moves nothing.
    let fx = g.key(0xff1b, 0, NONE);
    assert!(set_carets(&fx).is_empty(), "{fx:?}");
    assert!(
        !g.r.text.say_all.stopped_by(Epoch(0)),
        "the reading is over"
    );
    // A movement key then acts from the reading position: the app's caret event is spoken.
    let _ = g.key(DOWN, 0, NONE);
    assert!(set_carets(&g.tick(300)).is_empty());
}

#[test]
fn say_all_ends_at_the_end_of_the_text_and_leaves_the_caret_there() {
    use gorca_speech::LaneProgress as P;
    let mut g = rig();
    let text = "Only one.";
    let (_, f) = say_all_start(&mut g, text);
    let fx = g.reply(f[0].0, window(text));
    assert_eq!(spoken(&fx), ["Only one."]);
    let id = utt_ids(&fx)[0];
    let _ = progress(&mut g, P::Started(id));
    let fx = progress(&mut g, P::Ended(id));
    assert!(spoken(&fx).is_empty(), "nothing is said at the end (NVDA)");
    assert_eq!(set_carets(&fx), [9]);
}

/// Probe P2 of the M3 review: a lost `Ended` must neither keep the reading open past the end
/// of the text nor let a later key move the caret.
#[test]
fn a_lost_progress_report_cannot_keep_say_all_open() {
    use gorca_speech::LaneProgress as P;
    let mut g = rig();
    let text = "One. Two.";
    let (_, f) = say_all_start(&mut g, text);
    let fx = g.reply(f[0].0, window(text));
    let ids = utt_ids(&fx);
    // Started(1), Ended(1) and Started(2) are all lost; Ended(2) arrives.
    let fx = progress(&mut g, P::Ended(ids[1]));
    assert_eq!(set_carets(&fx), [9], "the reading finished naturally");
    g.advance(60_000);
    let fx = g.key(DOWN, 0, NONE);
    assert!(
        set_carets(&fx).is_empty(),
        "P2: a key long after the end moves no caret"
    );
}

#[test]
fn a_silent_speech_lane_ends_say_all_without_touching_the_caret() {
    use gorca_speech::LaneProgress as P;
    let mut g = rig();
    let text = "One. Two. Three.";
    let (_, f) = say_all_start(&mut g, text);
    let fx = g.reply(f[0].0, window(text));
    let ids = utt_ids(&fx);
    let _ = progress(&mut g, P::Started(ids[0]));
    assert!(g.r.text.say_all.stopped_by(Epoch(99)));
    // Nothing more is heard from the lane.
    let fx = g.tick(89_000);
    assert!(set_carets(&fx).is_empty());
    assert!(
        g.r.text.say_all.stopped_by(Epoch(99)),
        "still running just before the watchdog"
    );
    let fx = g.tick(2_000);
    assert!(
        set_carets(&fx).is_empty(),
        "the watchdog never moves the caret"
    );
    assert!(
        !g.r.text.say_all.stopped_by(Epoch(99)),
        "and it ends the reading"
    );
    assert_eq!(g.r.text_counters().say_all_watchdog, 1);
}

#[test]
fn say_all_and_say_selection_refuse_outside_focus_mode() {
    let mut g = rig();
    g.entry("hello", 0);
    g.r.mode = crate::mode::Mode::InputHelp {
        resume: crate::mode::BaseMode::Focus,
    };
    let e = g.epoch.advance();
    let mut fx = ReaderEffects::new();
    g.r.say_all_start(e, &mut fx);
    assert_eq!(spoken(&fx), ["not available in this mode"]);
    assert!(fetches(&fx).is_empty() && !g.r.text.say_all.stopped_by(Epoch(0)));
    let mut fx = ReaderEffects::new();
    g.r.text_say_selection(e, &mut fx);
    assert_eq!(spoken(&fx), ["not available in this mode"]);
}

#[test]
fn say_all_gives_up_when_the_text_does_not_come() {
    let mut g = rig();
    let (_, f) = say_all_start(&mut g, "text");
    let fx = g.msg(ReaderMsg::Text(TextMsg::Fetched {
        req: f[0].0,
        epoch: g.epoch.current(),
        result: Err(gorca_connector_api::BackendError::Timeout),
    }));
    assert!(spoken(&fx).is_empty());
    assert_eq!(g.r.text_counters().say_all_starved, 1);
    assert!(!g.r.text.say_all.stopped_by(Epoch(0)));
}

#[test]
fn modes_other_than_focus_leave_the_text_layer_idle() {
    let mut g = rig();
    g.entry("hello", 0);
    g.r.mode = crate::mode::Mode::InputHelp {
        resume: crate::mode::BaseMode::Focus,
    };
    let fx = g.caret(1);
    assert!(fx.is_empty());
}

#[test]
fn an_entry_that_does_not_say_editable_still_echoes_once() {
    // A toolkit that forgets the Editable state: the generic key echo speaks the key, and the
    // insert that follows is recognised as the typed key's, not spoken as foreign text.
    let mut g = rig();
    g.track(Role::TextEntry, StateSet::FOCUSED, "", 0);
    assert_eq!(spoken(&g.typed('a')), ["a"]);
    assert!(spoken(&g.insert(0, "a")).is_empty());
}

#[test]
fn tab_in_a_text_view_is_typed_once_and_in_an_entry_it_is_the_generic_echo() {
    let mut g = rig();
    g.area("", 0);
    assert_eq!(spoken(&g.key(0xff09, 0x09, NONE)), ["tab"]);
    assert!(
        spoken(&g.insert(0, "\t")).is_empty(),
        "the insert is its twin"
    );
    let mut h = rig();
    h.entry("x", 1);
    assert_eq!(
        spoken(&h.key(0xff09, 0x09, NONE)),
        ["tab"],
        "an entry's Tab moves the focus: the generic key echo"
    );
}

#[test]
fn a_huge_line_is_cut_before_it_is_spoken() {
    let mut g = rig();
    g.area("ab\ncd\n", 0);
    let _ = g.key(DOWN, 0, NONE);
    let f = fetches(&g.caret(3));
    let fx = g.reply(
        f[0].0,
        Reply::Unit(TextUnitAt {
            range: 3..6,
            text: Arc::from("x".repeat(60_000).as_str()),
        }),
    );
    let said = spoken(&fx);
    assert_eq!(said.len(), 1);
    assert!(
        said[0].chars().count() <= 4000,
        "{}",
        said[0].chars().count()
    );
}

/// Probe P1 of the M3 review: `selection-changed` before `text-caret-moved` for a Shift+Right.
#[test]
fn a_selection_event_ahead_of_its_caret_event_still_speaks_the_selection() {
    let mut g = rig();
    g.entry("hello", 0);
    let _ = g.key(RIGHT, 0, Modifiers::SHIFT);
    let f1 = fetches(&g.selection_event());
    assert_eq!(f1.len(), 1, "selection fetch issued");
    let fx_c = g.caret(1);
    assert!(fetches(&fx_c).is_empty(), "the fetch in flight is kept");
    let fx = g.reply(
        f1[0].0,
        Reply::Selections(TextSelections {
            ranges: vec![(0..1, Arc::from("h"))],
        }),
    );
    let mut all = spoken(&fx_c);
    all.extend(spoken(&fx));
    all.extend(spoken(&g.tick(10)));
    assert_eq!(all, ["h selected"]);
    assert_eq!(g.r.text.caret.tracked().and_then(|t| t.caret), Some(1));
}

/// Probe P3: a typed key whose insert never comes must not swallow a later move.
#[test]
fn an_unmatched_typed_key_does_not_absorb_a_later_unrelated_caret_move() {
    let mut g = rig();
    g.entry("hello world", 0);
    let _ = g.typed('x');
    g.advance(300);
    let mut all = spoken(&g.caret(1));
    all.extend(spoken(&g.tick(10)));
    assert!(
        !all.is_empty(),
        "an uncorrelated caret move is spoken: {all:?}"
    );
}

#[test]
fn text_that_matches_no_typed_key_forgets_the_expectations() {
    // IME: preedit keys, then a commit that is none of them.
    let mut g = rig();
    g.entry("", 0);
    for c in "nihao".chars() {
        let _ = g.typed(c);
    }
    let _ = g.insert(0, "你好");
    assert!(
        g.r.text
            .caret
            .tracked()
            .is_some_and(|t| t.expect_len() == 0)
    );
}

/// Probe P4: Left's caret event beats the Left key while Right's slot (a boundary) is open.
#[test]
fn a_caret_that_beats_its_key_while_the_previous_slot_is_open_is_the_new_keys() {
    let mut g = rig();
    g.entry("ab cd", 5);
    let _ = g.key(RIGHT, 0, NONE); // at the end: no caret event will come
    g.advance(20);
    let mut all = spoken(&g.caret(4)); // Left's caret beats the Left key
    all.extend(spoken(&g.key(LEFT, 0, NONE)));
    for _ in 0..50 {
        all.extend(spoken(&g.tick(10)));
    }
    assert_eq!(all, ["d"], "one utterance, for the Left key");
}

#[test]
fn a_held_caret_in_an_open_slot_that_no_key_claims_is_spoken_as_unexplained() {
    let mut g = rig();
    g.entry("ab cd", 5);
    let _ = g.key(RIGHT, 0, NONE);
    g.advance(20);
    assert!(spoken(&g.caret(4)).is_empty());
    assert_eq!(spoken(&g.tick(6)), ["d"]);
}

#[test]
fn a_second_unexplained_insert_in_the_flood_gap_does_not_drop_the_first() {
    let mut g = rig();
    g.entry("", 0);
    assert_eq!(
        spoken(&g.insert(0, "from an input method")),
        ["from an input method"]
    );
    let _ = g.insert(20, "one");
    let _ = g.insert(100, "two");
    assert!(spoken(&g.tick(6)).is_empty(), "both wait for the gap");
    let said = spoken(&g.tick(300));
    assert_eq!(said, ["one and 3 more"], "kept, with the rest counted");
}

#[test]
fn password_text_is_not_in_the_debug_output_of_anything_that_holds_it() {
    let mut g = rig();
    g.track(Role::PasswordEntry, StateSet::EDITABLE, "", 0);
    let secret = "hunter2x";
    let msgs = [
        TextMsg::Edit {
            node: node(),
            delta: gorca_model::TextDelta::Insert {
                offset: 0,
                len: 8,
                text: Some(gorca_model::LazyText {
                    msg: Arc::from(secret.as_bytes()),
                    field: 0,
                }),
            },
        },
        TextMsg::Fetched {
            req: TextReqId(1),
            epoch: Epoch(1),
            result: Ok(Reply::Window(TextWindowFill {
                base: 0,
                text: Arc::from(secret),
                len: 8,
                caret: None,
            })),
        },
        TextMsg::Fetched {
            req: TextReqId(2),
            epoch: Epoch(1),
            result: Ok(Reply::Unit(TextUnitAt {
                range: 0..8,
                text: Arc::from(secret),
            })),
        },
        TextMsg::Fetched {
            req: TextReqId(3),
            epoch: Epoch(1),
            result: Ok(Reply::Selections(TextSelections {
                ranges: vec![(0..8, Arc::from(secret))],
            })),
        },
    ];
    for m in &msgs {
        assert!(!format!("{m:?}").contains(secret), "{m:?}");
    }
    let edit = super::caret::Edit::Insert {
        offset: 0,
        len: 8,
        text: Some(secret.to_owned()),
    };
    assert!(!format!("{edit:?}").contains(secret));
    // Held text, the utterance and everything the machine holds.
    let _ = g.insert(0, secret);
    let _ = g.typed('z');
    let held = format!("{:?}", g.r.text.caret);
    assert!(!held.contains(secret) && !held.contains('z'), "{held}");
    let fx = g.tick(10);
    for e in &fx {
        assert!(!format!("{e:?}").contains(secret), "{e:?}");
    }
}

#[test]
fn an_app_with_byte_text_offsets_is_not_tracked_and_keeps_the_generic_echo() {
    let mut g = rig();
    // The connector reported UTF-8 byte offsets for this application (a capability, not a name).
    g.r.dir
        .set_text_unit(node().app, gorca_model::TextUnit::Utf8);
    let mut fx = ReaderEffects::new();
    g.r.now = g.now();
    g.r.text_track(&snap(Role::TextEntry, StateSet::EDITABLE), &mut fx);
    assert!(
        g.r.text.caret.is_off(),
        "byte offsets are the terminal layer's"
    );
    assert!(fetches(&fx).is_empty());
    assert_eq!(spoken(&g.typed('a')), ["a"], "the generic key echo");
    assert!(spoken(&g.caret(3)).is_empty());
    // Scalar apps are tracked as before, and the flag follows the app's announcement.
    g.r.dir
        .set_text_unit(node().app, gorca_model::TextUnit::Scalar);
    g.r.text_track(&snap(Role::TextEntry, StateSet::EDITABLE), &mut fx);
    assert!(!g.r.text.caret.is_off());
}

#[test]
fn a_gecko_field_is_tracked_in_the_apps_own_utf16_offsets() {
    let mut g = rig();
    g.r.dir
        .set_text_unit(node().app, gorca_model::TextUnit::Utf16Bom);
    let mut fx = ReaderEffects::new();
    g.r.now = g.now();
    g.r.text_track(&snap(Role::TextEntry, StateSet::EDITABLE), &mut fx);
    assert!(
        !g.r.text.caret.is_off(),
        "UTF-16 offsets are followed as they are"
    );
    assert!(!fetches(&fx).is_empty(), "the window is fetched");
}

#[test]
fn a_removed_node_or_a_gone_app_ends_tracking() {
    let mut g = rig();
    g.entry("hello", 0);
    let other = NodeKey {
        local: LocalId(99),
        ..node()
    };
    g.r.text_node_gone(Some(other), None);
    assert!(!g.r.text.caret.is_off(), "another node");
    g.r.text_node_gone(Some(node()), None);
    assert!(g.r.text.caret.is_off());
    g.entry("hello", 0);
    g.r.text_node_gone(None, Some(node().app));
    assert!(g.r.text.caret.is_off());
}

/// A small deterministic generator for the hostile-input test.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    /// Mostly small offsets, sometimes the extremes an application could send.
    fn offset(&mut self) -> u32 {
        match self.below(10) {
            0 => u32::MAX,
            1 => u32::MAX - u32::try_from(self.below(4)).unwrap_or(0),
            2 => 0,
            3 => 2048 + u32::try_from(self.below(8)).unwrap_or(0),
            _ => u32::try_from(self.below(40)).unwrap_or(0),
        }
    }
}

#[test]
fn hostile_event_streams_never_panic_and_stay_bounded() {
    for seed in 1..=40u64 {
        let mut rng = XorShift(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let mut g = rig();
        match seed % 3 {
            0 => g.area("ab\ncd\nef\n", 0),
            1 => g.track(Role::PasswordEntry, StateSet::EDITABLE, "secret", 3),
            _ => g.entry("hello world", 5),
        }
        let mut pending: Vec<TextReqId> = Vec::new();
        for _ in 0..300 {
            g.advance(rng.below(90));
            let fx = match rng.below(14) {
                0 => g.key(RIGHT, 0, NONE),
                1 => g.key(LEFT, 0, Modifiers::CONTROL),
                2 => g.key(DOWN, 0, Modifiers::SHIFT),
                3 => g.key(UP, 0, NONE),
                4 => g.key(BACKSPACE, 8, NONE),
                5 => g.typed(
                    char::from_u32(0x41 + u32::try_from(rng.below(60)).unwrap_or(0)).unwrap_or('a'),
                ),
                6 => g.caret(rng.offset()),
                7 => g.insert(rng.offset(), "xyz"),
                8 => {
                    let text = if rng.below(2) == 0 { Some("q") } else { None };
                    g.delete(rng.offset(), text, rng.offset())
                }
                9 => g.selection_event(),
                10 => g.tick(rng.below(300)),
                11 => g.msg(ReaderMsg::Text(TextMsg::Edit {
                    node: node(),
                    delta: gorca_model::TextDelta::Replaced { text: None },
                })),
                12 => {
                    // Answer a pending request with nonsense.
                    let Some(req) = pending.pop() else { continue };
                    let reply = match rng.below(4) {
                        0 => Reply::Window(TextWindowFill {
                            base: rng.offset(),
                            text: Arc::from("window text"),
                            len: rng.offset(),
                            caret: Some(rng.offset()),
                        }),
                        1 => Reply::Unit(TextUnitAt {
                            range: rng.offset()..rng.offset(),
                            text: Arc::from("a line"),
                        }),
                        2 => Reply::Selections(TextSelections {
                            ranges: vec![
                                (rng.offset()..rng.offset(), Arc::from("sel")),
                                (0..u32::MAX, Arc::from("")),
                            ],
                        }),
                        _ => Reply::Done,
                    };
                    g.reply(req, reply)
                }
                _ => {
                    let req = pending.pop().unwrap_or(TextReqId(1));
                    let epoch = g.epoch.current();
                    g.msg(ReaderMsg::Text(TextMsg::Fetched {
                        req,
                        epoch,
                        result: Err(gorca_connector_api::BackendError::Timeout),
                    }))
                }
            };
            pending.extend(fetches(&fx).into_iter().map(|(r, _)| r));
            for t in spoken(&fx) {
                assert!(t.chars().count() < 4_000, "a bounded utterance");
            }
            if let Some(t) = g.r.text.caret.tracked() {
                assert!(t.win.held() <= 16 * 1024);
            }
        }
    }
}

// ---- Gecko: UTF-16 offsets, phantom U+FEFF, rich editables (M3b) ------------------------------

/// The string Gecko hands out for "a", an emoji, "b", an emoji, "c": a phantom U+FEFF follows
/// each astral scalar, so every offset is the index of a character of this very string.
const GECKO: &str = "a\u{1F600}\u{FEFF}b\u{1F600}\u{FEFF}c";

fn gecko_rig() -> Rig {
    let g = rig();
    g.r.dir
        .set_text_unit(node().app, gorca_model::TextUnit::Utf16Bom);
    g.r.dir.set_text_insert_slack(node().app, 2);
    g
}

fn leaf(n: u64) -> NodeKey {
    NodeKey {
        local: LocalId(n),
        ..node()
    }
}

impl Rig {
    fn insert_raw(&mut self, offset: u32, len: u32, text: &str) -> ReaderEffects {
        self.insert_on(node(), offset, len, text)
    }

    fn caret_on(&mut self, n: NodeKey, off: u32) -> ReaderEffects {
        self.msg(ReaderMsg::Text(TextMsg::Caret {
            node: n,
            offset: off,
        }))
    }

    fn insert_on(&mut self, n: NodeKey, offset: u32, len: u32, text: &str) -> ReaderEffects {
        self.msg(ReaderMsg::Text(TextMsg::Edit {
            node: n,
            delta: gorca_model::TextDelta::Insert {
                offset,
                len,
                text: Some(gorca_model::LazyText {
                    msg: Arc::from(text.as_bytes()),
                    field: 0,
                }),
            },
        }))
    }

    fn delete_on(&mut self, n: NodeKey, offset: u32, len: u32, text: &str) -> ReaderEffects {
        self.msg(ReaderMsg::Text(TextMsg::Edit {
            node: n,
            delta: gorca_model::TextDelta::Delete {
                offset,
                len,
                text: Some(gorca_model::LazyText {
                    msg: Arc::from(text.as_bytes()),
                    field: 0,
                }),
            },
        }))
    }

    fn held(&self, r: std::ops::Range<u32>) -> String {
        self.r
            .text
            .caret
            .tracked()
            .and_then(|t| t.win.read(r))
            .unwrap_or_default()
    }

    /// Focus a contenteditable region (its text is two embedded objects) and let the browser
    /// place the caret in its first paragraph `p`, whose text is `text`.
    fn rich(&mut self, p: NodeKey, text: &str) {
        self.track(
            Role::TextEntry,
            StateSet::EDITABLE | StateSet::FOCUSED | StateSet::MULTILINE,
            "\u{FFFC}\u{FFFC}",
            0,
        );
        self.advance(20);
        let fx = self.caret_on(p, 0);
        assert!(
            spoken(&fx).is_empty(),
            "the browser placing the caret is silent: {fx:?}"
        );
        let f = fetches(&fx);
        let (req, op) = f.last().copied().expect("the paragraph's window");
        assert_eq!(
            op,
            TextOp::Window {
                node: p,
                around: None,
                radius: 2048
            }
        );
        let len = u32::try_from(text.chars().count()).unwrap_or(0);
        let _ = self.reply(
            req,
            Reply::Window(TextWindowFill {
                base: 0,
                text: Arc::from(text),
                len,
                caret: Some(0),
            }),
        );
    }
}

#[test]
fn gecko_arrows_over_an_emoji_speak_it_once_and_never_the_phantom() {
    let mut g = gecko_rig();
    g.entry(GECKO, 0);
    let _ = g.key(RIGHT, 0, NONE);
    assert_eq!(
        spoken(&g.caret(1)),
        ["\u{1F600}"],
        "the emoji, not its phantom"
    );
    let _ = g.key(RIGHT, 0, NONE);
    // Gecko's caret skips the phantom: 1 -> 3.
    assert_eq!(spoken(&g.caret(3)), ["b"]);
    let _ = g.key(LEFT, 0, NONE);
    assert_eq!(spoken(&g.caret(1)), ["\u{1F600}"]);
    // A caret event that lands on the phantom itself still speaks the emoji.
    let _ = g.key(RIGHT, 0, NONE);
    assert_eq!(spoken(&g.caret(2)), ["\u{1F600}"]);
}

#[test]
fn gecko_word_units_keep_a_phantom_with_its_emoji() {
    let mut g = gecko_rig();
    g.entry("hi \u{1F600}\u{FEFF} there", 0);
    let _ = g.key(RIGHT, 0, Modifiers::CONTROL);
    assert_eq!(
        spoken(&g.caret(3)),
        ["\u{1F600}"],
        "an emoji is a word of its own"
    );
    let _ = g.key(RIGHT, 0, Modifiers::CONTROL);
    assert_eq!(spoken(&g.caret(6)), ["there"]);
}

#[test]
fn gecko_inserts_come_without_phantoms_and_the_window_adds_them() {
    let mut g = gecko_rig();
    g.entry("ab", 2);
    // An input method commits an emoji: 2 UTF-16 units of text that has one scalar.
    let _ = g.insert_raw(2, 2, "\u{1F600}");
    assert_eq!(g.held(0..4), "ab\u{1F600}\u{FEFF}");
    let _ = g.caret(4);
    // Typing after it lands at the right offset.
    let _ = g.typed('c');
    let _ = g.insert(4, "c");
    assert_eq!(g.held(0..5), "ab\u{1F600}\u{FEFF}c");
}

#[test]
fn gecko_backspace_over_an_emoji_speaks_it() {
    let mut g = gecko_rig();
    g.entry(GECKO, 3);
    let _ = g.key(BACKSPACE, 0, NONE);
    let fx = g.delete(1, Some("\u{1F600}"), 2);
    assert_eq!(spoken(&fx), ["\u{1F600}"]);
    assert_eq!(g.held(0..5), "ab\u{1F600}\u{FEFF}c");
}

#[test]
fn gecko_an_unexplained_move_over_one_emoji_is_a_character_not_a_line() {
    let mut g = gecko_rig();
    g.entry(GECKO, 1);
    // A script or the mouse moves the caret from 1 to 3: one emoji.
    let _ = g.caret(3);
    let fx = g.tick(10);
    assert_eq!(spoken(&fx), ["b"], "one step on the line is a character");
}

#[test]
fn gecko_selection_of_an_emoji_is_spoken_as_the_emoji() {
    let mut g = gecko_rig();
    g.entry(GECKO, 1);
    let _ = g.key(RIGHT, 0, Modifiers::SHIFT);
    let f = fetches(&g.caret(3));
    let (req, op) = f.last().copied().expect("a selection fetch");
    assert!(matches!(op, TextOp::Selections { .. }), "{op:?}");
    let fx = g.reply(
        req,
        Reply::Selections(TextSelections {
            ranges: vec![(1..3, Arc::from("\u{1F600}"))],
        }),
    );
    assert_eq!(spoken(&fx), ["\u{1F600} selected"]);
}

#[test]
fn a_contenteditable_paragraph_takes_over_the_caret() {
    let mut g = gecko_rig();
    let p = leaf(96);
    g.rich(p, "One \u{1F600}\u{FEFF} two");
    assert_eq!(g.r.text.caret.tracked().map(|t| t.node), Some(p));
    assert_eq!(g.r.text.caret.tracked().map(|t| t.root), Some(node()));
    let _ = g.key(RIGHT, 0, NONE);
    assert_eq!(spoken(&g.caret_on(p, 1)), ["n"]);
    // Typing: the key speaks, the insert on the paragraph is the key's.
    let fx = g.typed('x');
    assert_eq!(spoken(&fx), ["x"]);
    let fx = g.insert_on(p, 2, 1, "x");
    assert!(spoken(&fx).is_empty(), "once: {fx:?}");
    // Backspace is spoken from the delete on the paragraph.
    let _ = g.key(BACKSPACE, 0, NONE);
    let fx = g.delete_on(p, 2, 1, "x");
    assert_eq!(spoken(&fx), ["x"]);
    assert_eq!(g.r.text_counters().retargets, 1);
}

#[test]
fn a_caret_into_the_next_paragraph_is_spoken_as_the_key_moved_it() {
    let mut g = gecko_rig();
    let (p, q) = (leaf(96), leaf(97));
    g.rich(p, "One two");
    g.advance(1000);
    let _ = g.key(DOWN, 0, NONE);
    let fx = g.caret_on(q, 0);
    // The new paragraph's window is not here yet: it is fetched, then the line is spoken.
    assert!(spoken(&fx).is_empty());
    let f = fetches(&fx);
    // The new paragraph is unknown to the window: it is filled (the paragraph the caret is in
    // now), then Down speaks the toolkit's line.
    assert_eq!(f.len(), 1, "one fetch, not a prefetch and a line: {f:?}");
    let (req, op) = f[0];
    assert!(
        matches!(op, TextOp::Window { node, .. } if node == q),
        "{op:?}"
    );
    let fx = g.reply(
        req,
        Reply::Window(TextWindowFill {
            base: 0,
            text: Arc::from("Tri chetyre"),
            len: 11,
            caret: Some(0),
        }),
    );
    let f = fetches(&fx);
    let (req, op) = f.last().copied().expect("the line");
    assert!(
        matches!(op, TextOp::AtOffset { node, .. } if node == q),
        "{op:?}"
    );
    let fx = g.reply(
        req,
        Reply::Unit(TextUnitAt {
            range: 0..11,
            text: Arc::from("Tri chetyre"),
        }),
    );
    assert_eq!(spoken(&fx), ["Tri chetyre"]);
    // Back at the region itself (Ctrl+A puts the caret on it): the region is read again.
    let fx = g.caret_on(node(), 2);
    assert!(spoken(&fx).is_empty());
    assert_eq!(g.r.text.caret.tracked().map(|t| t.node), Some(node()));
}

#[test]
fn a_plain_entry_ignores_events_of_other_nodes() {
    let mut g = gecko_rig();
    g.entry("hello", 0);
    let fx = g.caret_on(leaf(78), 0);
    assert!(spoken(&fx).is_empty());
    assert_eq!(g.r.text_counters().retargets, 0);
    assert_eq!(g.r.text.caret.tracked().map(|t| t.node), Some(node()));
}

#[test]
fn a_removed_paragraph_gives_the_caret_back_to_the_region() {
    let mut g = gecko_rig();
    let p = leaf(96);
    g.rich(p, "One two");
    g.r.text_node_gone(Some(p), None);
    assert_eq!(g.r.text.caret.tracked().map(|t| t.node), Some(node()));
    g.r.text_node_gone(Some(node()), None);
    assert!(g.r.text.caret.is_off());
}

#[test]
fn info_formatting_asks_for_the_run_under_the_caret_and_speaks_it() {
    let mut g = gecko_rig();
    g.entry("hello", 2);
    let e = g.epoch.advance();
    let mut fx = ReaderEffects::new();
    g.r.now = g.now();
    g.r.text_say_formatting(e, &mut fx);
    let f = fetches(&fx);
    assert_eq!(f.len(), 1, "{fx:?}");
    assert_eq!(
        f[0].1,
        TextOp::Format {
            node: node(),
            offset: 2
        }
    );
    let fx = g.reply(
        f[0].0,
        Reply::Format(gorca_model::TextFormat {
            family: Some(Arc::from("Noto Sans")),
            size: Some(Arc::from("12")),
            bold: true,
            fg: Some([200, 0, 0]),
            ..gorca_model::TextFormat::default()
        }),
    );
    assert_eq!(spoken(&fx), ["Noto Sans, 12 point, bold, red text"]);
}

#[test]
fn info_formatting_behind_the_last_character_asks_about_the_one_before_it() {
    let mut g = gecko_rig();
    g.entry("hello", 5);
    let mut fx = ReaderEffects::new();
    g.r.now = g.now();
    g.r.text_say_formatting(g.epoch.current(), &mut fx);
    assert_eq!(
        fetches(&fx)[0].1,
        TextOp::Format {
            node: node(),
            offset: 4
        }
    );
}

#[test]
fn info_formatting_outside_a_text_field_says_so_and_a_refusal_is_spoken() {
    let mut g = gecko_rig();
    let mut fx = ReaderEffects::new();
    g.r.text_say_formatting(g.epoch.current(), &mut fx);
    assert_eq!(spoken(&fx), ["not in a text field"]);
    g.entry("hello", 0);
    let mut fx = ReaderEffects::new();
    g.r.now = g.now();
    g.r.text_say_formatting(g.epoch.current(), &mut fx);
    let (req, _) = fetches(&fx)[0];
    let fx = g.msg(ReaderMsg::Text(TextMsg::Fetched {
        req,
        epoch: g.epoch.current(),
        result: Err(gorca_connector_api::BackendError::NotSupported),
    }));
    assert_eq!(spoken(&fx), ["formatting not available here"]);
}

#[test]
fn a_typed_letter_reported_one_offset_too_far_is_not_spoken_twice() {
    // Gecko sometimes reports the insert of a letter typed in the middle of a text one offset
    // too far, with the caret event where the caret really is (observed in the private
    // Firefox, FE2). The key speaks; the caret event is the insert's own; the window is
    // dropped because it can no longer be trusted.
    let mut g = gecko_rig();
    g.entry("abcdef", 5);
    let fx = g.typed('w');
    assert_eq!(spoken(&fx), ["w"]);
    let _ = g.insert(6, "w");
    let fx = g.caret(6);
    assert!(spoken(&fx).is_empty(), "once: {fx:?}");
    assert_eq!(g.r.text_counters().window_dirty, 1);
}

#[test]
fn a_commit_reported_one_past_the_caret_is_spoken_once() {
    // Gecko in an input, after scripted text: the insert says 4, the caret was at 3 and moves
    // by the insert's length to 4. The caret move is the insert's own, not a second speech.
    let mut g = gecko_rig();
    g.entry("abcdef", 3);
    let fx = g.insert_raw(4, 1, "\u{436}");
    let mut said = spoken(&fx);
    let fx = g.caret(4);
    said.extend(spoken(&fx));
    said.extend(spoken(&g.tick(400)));
    assert_eq!(said, ["\u{436}"]);
}

#[test]
fn a_backspace_reported_at_the_caret_is_spoken_once() {
    // BackSpace at 5: Gecko reports the delete at 5 (not 4), the caret then goes to 4.
    let mut g = gecko_rig();
    g.entry("abc\u{436}de", 5);
    let _ = g.key(BACKSPACE, 0, NONE);
    let fx = g.delete(5, Some("e"), 1);
    let mut said = spoken(&fx);
    let fx = g.caret(4);
    said.extend(spoken(&fx));
    said.extend(spoken(&g.tick(400)));
    assert_eq!(said, ["e"]);
}

/// Review F-1: a script updates an unrelated text object of the same page (a clock) while the
/// user is in a contenteditable: the machine stays on the paragraph and says nothing.
#[test]
fn unrelated_text_change_does_not_steal_the_rich_editable() {
    let mut g = gecko_rig();
    let p = leaf(96);
    g.rich(p, "One two");
    g.advance(1000);
    let clock = leaf(500); // not in the mirror: Lookup::Miss
    let fx = g.insert_on(clock, 0, 5, "12:01");
    let fx2 = g.tick(50);
    let tracked = g.r.text.caret.tracked().map(|t| t.node);
    assert_eq!(
        (spoken(&fx), spoken(&fx2), tracked),
        (Vec::<String>::new(), Vec::<String>::new(), Some(p)),
        "clock insert retargeted or spoken"
    );
}

/// Review F-4 (ADR-0008): a scripted insert one unit after the caret in a toolkit without the
/// Gecko quirk (a GTK entry), then a mouse click puts the caret right there within 50 ms: the
/// click's move is still spoken, because only an app whose connector declared the insert-offset
/// slack gets its following caret move folded into the edit.
#[test]
fn scripted_insert_after_caret_does_not_swallow_a_real_move_without_the_quirk() {
    let mut g = rig();
    g.entry("hello", 2);
    g.advance(1000);
    let _ = g.insert(3, "X");
    let _ = g.tick(5);
    g.advance(10);
    let fx = g.caret(3);
    let fx2 = g.tick(20);
    let mut s = spoken(&fx);
    s.extend(spoken(&fx2));
    assert!(!s.is_empty(), "uncorrelated move to 3 was swallowed");
}
