//! The input lane's classifier (ADR-0005 4.2): one `KeyEvent` in, one [`KeyIntent`] out, O(1),
//! synchronous, no I/O. The daemon's pump task owns an [`InputLane`] and calls
//! [`InputLane::on_key`] for every event the key source delivers.
//!
//! Order: press kind (autorepeat), Mutter's verdict from the replica worlds, chord lookup with
//! multi-press counting, watched gestures, then the K-3 epoch rule.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use gorca_rt::EpochCell;
use smallvec::SmallVec;

use crate::event::{CmdId, KeyClass, KeyEvent, KeyIntent, Modifiers, PressKind};
use crate::keymap::{
    Bound, ChordKey, CompiledKeymap, GrabView, InputContext, KeyRef, LaneLinks, LaneSettings,
    ModeKind,
};
use crate::keys::{is_control, is_modifier_keysym, is_shift, sym};
use crate::mutter_model::{Ev, Replica, Timing, Verdict};

/// A Shift press and release with nothing between, released within this time, is a tap.
pub const SHIFT_TAP_MAX: Duration = Duration::from_millis(400);
/// Learn mode ends by itself after this long without a key (LM-3).
pub const HELP_IDLE: Duration = Duration::from_secs(60);
/// Learn mode ends if the Reader has not answered for this long (LM-3).
pub const HELP_STALL: Duration = Duration::from_secs(1);
/// Replica worlds kept at once (each guard-band event can double them).
const MAX_WORLDS: usize = 8;
const MAX_PRESS: u8 = 3;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Sel {
    Committed,
    Wanted,
}

#[derive(Clone, Debug)]
struct World {
    r: Replica,
    sel: Sel,
}

/// Safety actions the lane takes by itself when the Reader cannot (LM-3).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LaneAlert {
    /// Learn mode has been idle for [`HELP_IDLE`].
    HelpIdle,
    /// Learn mode, and the Reader did not answer within [`HELP_STALL`].
    HelpStall,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct LaneStats {
    pub keys: u64,
    /// Keys classified `Ambiguous` (M-16).
    pub ambiguous: u64,
    /// Keys whose verdict forked on the double-tap guard band.
    pub band_forks: u64,
}

/// What [`InputLane::on_key`] produced.
#[derive(Copy, Clone, Debug)]
pub struct LaneOut {
    pub intent: KeyIntent,
    /// The lane advanced the epoch: wake the speech lane (E-2).
    pub epoch_advanced: bool,
}

#[derive(Copy, Clone, Debug)]
struct Multi {
    key: ChordKey,
    count: u8,
    at: Instant,
}

#[derive(Debug)]
pub struct InputLane {
    links: Arc<LaneLinks>,
    epoch: Arc<EpochCell>,
    start: Instant,
    down: SmallVec<[u16; 16]>,
    worlds: SmallVec<[World; 4]>,
    seen: (u32, Option<u32>),
    multi: Option<Multi>,
    shift_armed: Option<Instant>,
    last_key_at: Instant,
    sent: u64,
    stall_since: Option<Instant>,
    stats: LaneStats,
}

impl InputLane {
    pub fn new(links: Arc<LaneLinks>, epoch: Arc<EpochCell>, now: Instant) -> Self {
        let view = links.view.load_full();
        let mut worlds = SmallVec::new();
        worlds.push(World {
            r: Replica::default(),
            sel: Sel::Committed,
        });
        let seen = (view.committed_gen, view.wanted_gen);
        Self {
            links,
            epoch,
            start: now,
            down: SmallVec::new(),
            worlds,
            seen,
            multi: None,
            shift_armed: None,
            last_key_at: now,
            sent: 0,
            stall_since: None,
            stats: LaneStats::default(),
        }
    }

    pub fn stats(&self) -> LaneStats {
        self.stats
    }

    /// Forget the keyboard state: the monitor was lost or restarted, so Mutter's state is
    /// unknown and keys that were down will never be released on this connection.
    pub fn reset(&mut self) {
        self.down.clear();
        self.worlds.clear();
        self.worlds.push(World {
            r: Replica::default(),
            sel: Sel::Committed,
        });
        self.multi = None;
        self.shift_armed = None;
    }

    /// Reconcile the worlds with the grab sets the source reports (ADR-0005 4.3).
    fn sync(&mut self, view: &GrabView) {
        let now = (view.committed_gen, view.wanted_gen);
        if now == self.seen {
            return;
        }
        let (old_c, old_w) = self.seen;
        // The wanted set became the committed one: its worlds are the new committed worlds.
        let promoted = old_w == Some(view.committed_gen) && view.committed_gen != old_c;
        let mut next: SmallVec<[World; 4]> = SmallVec::new();
        let keep = if promoted {
            Sel::Wanted
        } else {
            Sel::Committed
        };
        for w in self.worlds.iter().filter(|w| w.sel == keep) {
            next.push(World {
                r: w.r.clone(),
                sel: Sel::Committed,
            });
        }
        if next.is_empty() {
            // Should not happen; keep whatever there is rather than losing all state.
            for w in &self.worlds {
                next.push(World {
                    r: w.r.clone(),
                    sel: Sel::Committed,
                });
            }
        }
        if view.wanted.is_some() {
            let forks: SmallVec<[World; 4]> = next
                .iter()
                .map(|w| World {
                    r: w.r.clone(),
                    sel: Sel::Wanted,
                })
                .collect();
            next.extend(forks);
        }
        dedupe(&mut next);
        self.worlds = next;
        self.seen = now;
    }

    /// Classify one key event received at `at`.
    pub fn on_key(&mut self, ev: KeyEvent, at: Instant) -> LaneOut {
        self.stats.keys += 1;
        let settings = self.links.settings.load_full();
        let keymap = self.links.keymap.load_full();
        let view = self.links.view.load_full();
        self.sync(&view);

        let kind = self.press_kind(&ev);
        let time_ms =
            u32::try_from(at.saturating_duration_since(self.start).as_millis()).unwrap_or(u32::MAX);
        let verdict = self.verdict(&ev, kind, time_ms, &view, &settings);
        let ctx = view.committed.ctx;
        let is_gorca_mod = view.committed.modifiers.contains(&ev.keysym);
        let is_mod_key = is_gorca_mod || is_modifier_keysym(ev.keysym);

        let chord = (!is_mod_key && ev.pressed).then(|| ChordKey {
            mods: if matches!(verdict, Some(v) if v.gorca_held) {
                ev.modifiers.without_locks() | Modifiers::GORCA
            } else {
                ev.modifiers.without_locks()
            },
            key: KeyRef::Code(ev.keycode),
        });

        let count = self.press_count(kind, chord, at, &settings);
        let class = match verdict {
            None => {
                self.stats.ambiguous += 1;
                let committed = chord.and_then(|c| {
                    keymap
                        .lookup(lookup_mode(ctx), c, 1)
                        .filter(|b| b.active)
                        .map(|b| b.cmd)
                });
                (KeyClass::Ambiguous { committed }, None)
            }
            Some(v) => self.classify(
                &ev,
                kind,
                v,
                chord,
                count,
                (is_gorca_mod, ctx),
                &keymap,
                &settings,
                at,
            ),
        };
        let (class, bound) = class;
        self.track_shift(&ev, kind, at);
        self.last_key_at = at;
        if ctx.mode == ModeKind::InputHelp
            && matches!(class, KeyClass::Command(_) | KeyClass::Described(_))
        {
            self.sent += 1;
            self.stall_since.get_or_insert(at);
        }
        let advanced = self.advance_epoch(kind, class, bound, is_mod_key, &settings);
        let repeat = match class {
            KeyClass::Command(_) | KeyClass::Described(_) => {
                bound.map_or(1, |b| count.min(b.max_press.max(1)))
            }
            KeyClass::Swallowed
            | KeyClass::Watched(_)
            | KeyClass::PassToApp
            | KeyClass::ModifierPassThrough
            | KeyClass::Ambiguous { .. }
            | KeyClass::Injected => 1,
        };
        LaneOut {
            intent: KeyIntent {
                event: ev,
                kind,
                class,
                repeat,
                chord,
                epoch: self.epoch.current(),
                at,
            },
            epoch_advanced: advanced,
        }
    }

    fn press_kind(&mut self, ev: &KeyEvent) -> PressKind {
        if ev.pressed {
            if self.down.contains(&ev.keycode) {
                PressKind::Repeat
            } else {
                if self.down.len() < 64 {
                    self.down.push(ev.keycode);
                }
                PressKind::Press
            }
        } else {
            self.down.retain(|k| *k != ev.keycode);
            PressKind::Release
        }
    }

    /// Step every world; `Some` when they all agree.
    fn verdict(
        &mut self,
        ev: &KeyEvent,
        kind: PressKind,
        time_ms: u32,
        view: &GrabView,
        settings: &LaneSettings,
    ) -> Option<Verdict> {
        let e = Ev {
            keysym: ev.keysym,
            pressed: ev.pressed,
            repeat: kind == PressKind::Repeat,
            state: ev.modifiers,
            time_ms,
        };
        let mut next: SmallVec<[World; 4]> = SmallVec::new();
        let mut verdicts: SmallVec<[(Sel, Verdict); 4]> = SmallVec::new();
        for w in std::mem::take(&mut self.worlds) {
            let grabs: &crate::grab::GrabSet = match w.sel {
                Sel::Committed => &view.committed,
                Sel::Wanted => view.wanted.as_deref().unwrap_or(&view.committed),
            };
            let timing = w.r.timing(
                &e,
                grabs,
                settings.repeat_delay_ms,
                settings.double_tap_guard_ms,
            );
            let outcomes: &[bool] = match timing {
                Timing::Soon => &[true],
                Timing::Late | Timing::Irrelevant => &[false],
                Timing::Band => {
                    self.stats.band_forks += 1;
                    &[true, false]
                }
            };
            for soon in outcomes {
                let mut r = w.r.clone();
                let v = r.step(&e, grabs, *soon);
                verdicts.push((w.sel, v));
                next.push(World { r, sel: w.sel });
            }
        }
        dedupe(&mut next);
        next.truncate(MAX_WORLDS);
        self.worlds = next;
        let first = verdicts.first().map(|(_, v)| *v)?;
        verdicts.iter().all(|(_, v)| *v == first).then_some(first)
    }

    fn press_count(
        &mut self,
        kind: PressKind,
        chord: Option<ChordKey>,
        at: Instant,
        settings: &LaneSettings,
    ) -> u8 {
        let Some(key) = chord else { return 1 };
        if kind != PressKind::Press {
            return self.multi.map_or(1, |m| m.count);
        }
        let window = Duration::from_millis(u64::from(settings.multi_press_ms));
        let count = match self.multi {
            Some(m) if m.key == key && at.saturating_duration_since(m.at) <= window => {
                (m.count + 1).min(MAX_PRESS)
            }
            Some(_) | None => 1,
        };
        self.multi = Some(Multi { key, count, at });
        count
    }

    #[allow(clippy::too_many_arguments)]
    fn classify(
        &self,
        ev: &KeyEvent,
        kind: PressKind,
        v: Verdict,
        chord: Option<ChordKey>,
        count: u8,
        (is_gorca_mod, ctx): (bool, InputContext),
        keymap: &CompiledKeymap,
        settings: &LaneSettings,
        at: Instant,
    ) -> (KeyClass, Option<Bound>) {
        if is_gorca_mod {
            return (
                if v.passed_modifier {
                    KeyClass::ModifierPassThrough
                } else if v.swallowed {
                    KeyClass::Swallowed
                } else {
                    KeyClass::PassToApp
                },
                None,
            );
        }
        if v.swallowed {
            if ctx.mode == ModeKind::InputHelp {
                return Self::classify_help(ev, kind, chord, count, ctx, keymap);
            }
            let Some(chord) = chord else {
                return (KeyClass::Swallowed, None);
            };
            let bound = lookup_bound(keymap, ctx.mode, chord, count);
            return match bound {
                Some(b)
                    if b.active
                        && (kind == PressKind::Press
                            || b.repeatable && kind == PressKind::Repeat) =>
                {
                    (KeyClass::Command(b.cmd), Some(b))
                }
                Some(_) | None => (KeyClass::Swallowed, bound),
            };
        }
        // Reached the application.
        if kind == PressKind::Press
            && is_control(ev.keysym)
            && settings.stop_speech_on_control
            && let Some(stop) = keymap.stop
        {
            return (KeyClass::Watched(stop), None);
        }
        if kind == PressKind::Release
            && is_shift(ev.keysym)
            && settings.pause_speech_on_shift
            && let Some(armed) = self.shift_armed
            && at.saturating_duration_since(armed) <= SHIFT_TAP_MAX
            && let Some(pause) = keymap.pause
        {
            return (KeyClass::Watched(pause), None);
        }
        (KeyClass::PassToApp, None)
    }

    /// Learn mode: describe, do not execute; only bypass commands run (LM-2, LM-3).
    fn classify_help(
        ev: &KeyEvent,
        kind: PressKind,
        chord: Option<ChordKey>,
        count: u8,
        ctx: InputContext,
        keymap: &CompiledKeymap,
    ) -> (KeyClass, Option<Bound>) {
        if kind != PressKind::Press {
            return (KeyClass::Swallowed, None);
        }
        let plain_escape = ev.keysym == sym::ESCAPE
            && ev
                .modifiers
                .without_locks()
                .difference(Modifiers::SHIFT)
                .is_empty();
        if plain_escape && let Some(t) = keymap.learn_toggle {
            return (KeyClass::Command(t), None);
        }
        let Some(chord) = chord else {
            return (KeyClass::Swallowed, None);
        };
        let bound = lookup_bound(keymap, ctx.resume, chord, count);
        match bound {
            Some(b) if b.bypass && b.active => (KeyClass::Command(b.cmd), Some(b)),
            Some(b) => (KeyClass::Described(Some(b.cmd)), Some(b)),
            None => (KeyClass::Described(None), None),
        }
    }

    fn track_shift(&mut self, ev: &KeyEvent, kind: PressKind, at: Instant) {
        if is_shift(ev.keysym) {
            match kind {
                PressKind::Press if self.down.len() == 1 => self.shift_armed = Some(at),
                PressKind::Press | PressKind::Repeat => {}
                PressKind::Release => self.shift_armed = None,
            }
        } else if kind == PressKind::Press {
            self.shift_armed = None;
        }
    }

    /// K-3 (ADR-0005 4.2 step 7).
    fn advance_epoch(
        &self,
        kind: PressKind,
        class: KeyClass,
        bound: Option<Bound>,
        is_mod_key: bool,
        settings: &LaneSettings,
    ) -> bool {
        if kind != PressKind::Press {
            return false;
        }
        let advance = match class {
            KeyClass::Command(_) => bound.is_none_or(|b| b.interrupts),
            KeyClass::Watched(cmd) => {
                // Only the stop gesture cancels speech; the pause tap fires on a release.
                self.links.keymap.load().stop == Some(cmd)
            }
            KeyClass::Swallowed
            | KeyClass::Described(_)
            | KeyClass::PassToApp
            | KeyClass::Ambiguous { .. } => !is_mod_key && settings.interrupt_on_keys,
            KeyClass::ModifierPassThrough | KeyClass::Injected => false,
        };
        if advance {
            self.epoch.advance();
        }
        advance
    }

    /// Periodic safety check (call about once a second). LM-3.
    pub fn tick(&mut self, now: Instant) -> Option<LaneAlert> {
        let view = self.links.view.load();
        if view.committed.ctx.mode != ModeKind::InputHelp {
            self.stall_since = None;
            return None;
        }
        let ack = self.links.reader_ack.load(Ordering::Acquire);
        if ack >= self.sent {
            self.stall_since = None;
        } else if let Some(since) = self.stall_since
            && now.saturating_duration_since(since) >= HELP_STALL
        {
            self.stall_since = None;
            return Some(LaneAlert::HelpStall);
        }
        if now.saturating_duration_since(self.last_key_at) >= HELP_IDLE {
            self.last_key_at = now;
            return Some(LaneAlert::HelpIdle);
        }
        None
    }

    /// The command to run for a lane alert, if the keymap has one.
    pub fn learn_toggle(&self) -> Option<CmdId> {
        self.links.keymap.load().learn_toggle
    }
}

/// Explicit `(chord, n)` binding first, then `(chord, 1)` with a repeat count (CR-4).
fn lookup_bound(
    keymap: &CompiledKeymap,
    mode: ModeKind,
    chord: ChordKey,
    count: u8,
) -> Option<Bound> {
    if count > 1
        && let Some(b) = keymap.lookup(mode, chord, count)
    {
        return Some(b);
    }
    keymap.lookup(mode, chord, 1)
}

fn lookup_mode(ctx: InputContext) -> ModeKind {
    if ctx.mode == ModeKind::InputHelp {
        ctx.resume
    } else {
        ctx.mode
    }
}

fn dedupe(worlds: &mut SmallVec<[World; 4]>) {
    let mut out: SmallVec<[World; 4]> = SmallVec::new();
    for w in worlds.drain(..) {
        if !out.iter().any(|o| o.sel == w.sel && o.r == w.r) {
            out.push(w);
        }
    }
    *worlds = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grab::GrabSet;
    use crate::keymap::TableKind;

    const INS: u32 = sym::INSERT;
    const KC_INS: u16 = 118;
    const KC_T: u16 = 28;
    const KC_A: u16 = 38;
    const KC_CTRL: u16 = 37;
    const KC_SHIFT: u16 = 50;
    const T: u32 = 0x74;
    const A: u32 = 0x61;

    struct Rig {
        lane: InputLane,
        links: Arc<LaneLinks>,
        epoch: Arc<EpochCell>,
        t0: Instant,
    }

    fn ck(code: u16, gorca: bool) -> ChordKey {
        ChordKey {
            mods: if gorca {
                Modifiers::GORCA
            } else {
                Modifiers::empty()
            },
            key: KeyRef::Code(code),
        }
    }

    #[allow(clippy::unnecessary_wraps)]
    fn bound(n: u32) -> Option<Bound> {
        Some(Bound {
            cmd: CmdId(n),
            active: true,
            repeatable: false,
            max_press: 3,
            bypass: false,
            interrupts: true,
        })
    }

    fn rig() -> Rig {
        let links = LaneLinks::new();
        let mut km = CompiledKeymap::new(vec![INS]);
        km.insert(TableKind::Global, ck(KC_T, true), 1, bound(1));
        km.insert(TableKind::Global, ck(KC_T, true), 3, bound(33));
        km.stop = Some(CmdId(100));
        km.pause = Some(CmdId(101));
        km.learn_toggle = Some(CmdId(102));
        links.keymap.store(Arc::new(km));
        let mut g = GrabSet::default();
        g.modifiers.insert(INS);
        links.view.store(Arc::new(GrabView {
            committed_gen: 1,
            committed: Arc::new(g),
            wanted_gen: None,
            wanted: None,
        }));
        let epoch = Arc::new(EpochCell::new());
        let t0 = Instant::now();
        Rig {
            lane: InputLane::new(Arc::clone(&links), Arc::clone(&epoch), t0),
            links,
            epoch,
            t0,
        }
    }

    impl Rig {
        fn key(&mut self, code: u16, sym: u32, pressed: bool, ms: u64, mods: Modifiers) -> LaneOut {
            let ev = KeyEvent {
                keycode: code,
                keysym: sym,
                unichar: if sym < 0x7f { sym } else { 0 },
                modifiers: mods,
                pressed,
            };
            self.lane.on_key(ev, self.t0 + Duration::from_millis(ms))
        }

        fn class(&mut self, code: u16, sym: u32, pressed: bool, ms: u64) -> KeyClass {
            self.key(code, sym, pressed, ms, Modifiers::empty())
                .intent
                .class
        }
    }

    #[test]
    fn modifier_chord_is_a_command_and_interrupts() {
        let mut r = rig();
        assert_eq!(r.class(KC_INS, INS, true, 1000), KeyClass::Swallowed);
        let before = r.epoch.current();
        let o = r.key(KC_T, T, true, 1100, Modifiers::empty());
        assert_eq!(o.intent.class, KeyClass::Command(CmdId(1)));
        assert!(o.epoch_advanced && r.epoch.current() > before);
        assert_eq!(o.intent.epoch, r.epoch.current());
        assert_eq!(r.class(KC_T, T, false, 1150), KeyClass::Swallowed);
        assert_eq!(r.class(KC_INS, INS, false, 1200), KeyClass::Swallowed);
        // The lone modifier never advanced the epoch beyond the command.
        assert_eq!(r.epoch.current().0, before.0 + 1);
    }

    #[test]
    fn plain_keys_pass_and_non_modifiers_interrupt() {
        let mut r = rig();
        let before = r.epoch.current();
        let o = r.key(KC_A, A, true, 100, Modifiers::empty());
        assert_eq!(o.intent.class, KeyClass::PassToApp);
        assert!(o.epoch_advanced);
        assert!(r.epoch.current() > before);
        // Shift alone does not interrupt.
        let e = r.epoch.current();
        let o = r.key(KC_SHIFT, sym::SHIFT_L, true, 200, Modifiers::SHIFT);
        assert!(!o.epoch_advanced);
        assert_eq!(r.epoch.current(), e);
    }

    #[test]
    fn double_tap_is_passed_through_and_keys_under_it_are_not_commands() {
        let mut r = rig();
        r.class(KC_INS, INS, true, 1000);
        r.class(KC_INS, INS, false, 1050);
        assert_eq!(
            r.class(KC_INS, INS, true, 1200),
            KeyClass::ModifierPassThrough
        );
        assert_eq!(r.class(KC_T, T, true, 1250), KeyClass::PassToApp);
        r.class(KC_T, T, false, 1260);
        assert_eq!(r.class(KC_INS, INS, false, 1300), KeyClass::PassToApp);
        assert_eq!(r.class(KC_INS, INS, true, 3000), KeyClass::Swallowed);
    }

    #[test]
    fn the_guard_band_makes_the_second_press_and_its_chord_ambiguous() {
        let mut r = rig();
        r.class(KC_INS, INS, true, 1000);
        r.class(KC_INS, INS, false, 1050);
        // 495 ms after the release: |dt - 500| <= 15.
        let c = r.class(KC_INS, INS, true, 1050 + 495);
        assert!(matches!(c, KeyClass::Ambiguous { .. }), "{c:?}");
        let c = r.class(KC_T, T, true, 1560);
        assert!(matches!(c, KeyClass::Ambiguous { .. }), "{c:?}");
        assert!(r.lane.stats().ambiguous >= 2);
        // After a different key event the worlds agree again.
        r.class(KC_T, T, false, 1580);
        r.class(KC_INS, INS, false, 1600);
        r.class(KC_A, A, true, 1700);
        assert_eq!(r.class(KC_A, A, false, 1710), KeyClass::PassToApp);
    }

    #[test]
    fn multi_press_picks_the_explicit_binding_then_the_repeat_count() {
        let mut r = rig();
        r.class(KC_INS, INS, true, 1000);
        let o = r.key(KC_T, T, true, 1050, Modifiers::empty());
        assert_eq!(
            (o.intent.class, o.intent.repeat),
            (KeyClass::Command(CmdId(1)), 1)
        );
        r.class(KC_T, T, false, 1060);
        let o = r.key(KC_T, T, true, 1200, Modifiers::empty());
        assert_eq!(
            (o.intent.class, o.intent.repeat),
            (KeyClass::Command(CmdId(1)), 2)
        );
        r.class(KC_T, T, false, 1210);
        let o = r.key(KC_T, T, true, 1300, Modifiers::empty());
        assert_eq!(
            o.intent.class,
            KeyClass::Command(CmdId(33)),
            "explicit x3 binding"
        );
        r.class(KC_T, T, false, 1310);
        // A pause longer than multi_press_ms starts over.
        let o = r.key(KC_T, T, true, 2000, Modifiers::empty());
        assert_eq!(
            (o.intent.class, o.intent.repeat),
            (KeyClass::Command(CmdId(1)), 1)
        );
    }

    #[test]
    fn autorepeat_of_a_chord_key_is_not_a_second_press() {
        let mut r = rig();
        r.class(KC_INS, INS, true, 1000);
        r.class(KC_T, T, true, 1010);
        let o = r.key(KC_T, T, true, 1020, Modifiers::empty());
        assert_eq!(o.intent.kind, PressKind::Repeat);
        assert_eq!(o.intent.class, KeyClass::Swallowed, "not repeatable");
        assert!(!o.epoch_advanced);
    }

    #[test]
    fn control_press_is_a_watched_stop_and_shift_tap_pauses() {
        let mut r = rig();
        let before = r.epoch.current();
        let o = r.key(KC_CTRL, sym::CONTROL_L, true, 100, Modifiers::CONTROL);
        assert_eq!(o.intent.class, KeyClass::Watched(CmdId(100)));
        assert!(o.epoch_advanced && r.epoch.current() > before);
        r.class(KC_CTRL, sym::CONTROL_L, false, 150);
        r.class(KC_SHIFT, sym::SHIFT_L, true, 1000);
        let o = r.key(KC_SHIFT, sym::SHIFT_L, false, 1100, Modifiers::empty());
        assert_eq!(o.intent.class, KeyClass::Watched(CmdId(101)));
        assert!(!o.epoch_advanced);
        // A key between press and release cancels the tap.
        r.class(KC_SHIFT, sym::SHIFT_L, true, 2000);
        r.class(KC_A, 0x41, true, 2050);
        r.class(KC_A, 0x41, false, 2060);
        assert_eq!(
            r.class(KC_SHIFT, sym::SHIFT_L, false, 2100),
            KeyClass::PassToApp
        );
        // So does holding it too long.
        r.class(KC_SHIFT, sym::SHIFT_L, true, 3000);
        assert_eq!(
            r.class(KC_SHIFT, sym::SHIFT_L, false, 3600),
            KeyClass::PassToApp
        );
    }

    #[test]
    fn settings_switch_the_watched_gestures_off() {
        let mut r = rig();
        r.links.settings.store(Arc::new(LaneSettings {
            stop_speech_on_control: false,
            ..LaneSettings::default()
        }));
        assert_eq!(
            r.class(KC_CTRL, sym::CONTROL_L, true, 100),
            KeyClass::PassToApp
        );
    }

    fn pending_view(r: &Rig, wanted: GrabSet) {
        let cur = r.links.view.load_full();
        r.links.view.store(Arc::new(GrabView {
            committed_gen: cur.committed_gen,
            committed: Arc::clone(&cur.committed),
            wanted_gen: Some(cur.committed_gen + 1),
            wanted: Some(Arc::new(wanted)),
        }));
    }

    #[test]
    fn keys_in_the_difference_of_committed_and_wanted_are_ambiguous() {
        let mut r = rig();
        // Pending: the modifier is being removed (pass-next-key).
        pending_view(&r, GrabSet::default());
        let c = r.class(KC_INS, INS, true, 1000);
        assert!(matches!(c, KeyClass::Ambiguous { .. }), "{c:?}");
        // Plain keys are the same in both worlds.
        r.class(KC_INS, INS, false, 1010);
        assert_eq!(r.class(KC_A, A, true, 1100), KeyClass::PassToApp);
        // Commit: the wanted set is now the committed one (empty, so Insert passes).
        let cur = r.links.view.load_full();
        r.links.view.store(Arc::new(GrabView {
            committed_gen: cur.wanted_gen.unwrap_or(0),
            committed: Arc::clone(cur.wanted.as_ref().unwrap_or(&cur.committed)),
            wanted_gen: None,
            wanted: None,
        }));
        r.class(KC_A, A, false, 1200);
        assert_eq!(r.class(KC_INS, INS, true, 5000), KeyClass::PassToApp);
    }

    #[test]
    fn learn_mode_describes_keys_and_runs_only_bypass_commands() {
        let mut r = rig();
        let mut km = (*r.links.keymap.load_full()).clone();
        km.insert(
            TableKind::Global,
            ck(KC_A, true),
            1,
            Some(Bound {
                bypass: true,
                ..bound(7).unwrap()
            }),
        );
        r.links.keymap.store(Arc::new(km));
        let help = GrabSet {
            grab_all: true,
            modifiers: [INS].into_iter().collect(),
            ctx: InputContext {
                mode: ModeKind::InputHelp,
                resume: ModeKind::Focus,
            },
            ..GrabSet::default()
        };
        r.links.view.store(Arc::new(GrabView {
            committed_gen: 2,
            committed: Arc::new(help),
            wanted_gen: None,
            wanted: None,
        }));
        r.class(KC_INS, INS, true, 1000);
        assert_eq!(
            r.class(KC_T, T, true, 1100),
            KeyClass::Described(Some(CmdId(1)))
        );
        assert_eq!(r.class(KC_T, T, false, 1110), KeyClass::Swallowed);
        assert_eq!(r.class(KC_A, A, true, 1200), KeyClass::Command(CmdId(7)));
        r.class(KC_A, A, false, 1210);
        r.class(KC_INS, INS, false, 1220);
        // An unbound key is described as such; Escape leaves.
        assert_eq!(
            r.class(KC_SHIFT + 5, 0x7a, true, 1300),
            KeyClass::Described(None)
        );
        assert_eq!(
            r.class(9, sym::ESCAPE, true, 1400),
            KeyClass::Command(CmdId(102))
        );
    }

    #[test]
    fn learn_mode_alerts_on_idle_and_on_a_silent_reader() {
        let mut r = rig();
        let help = GrabSet {
            grab_all: true,
            ctx: InputContext {
                mode: ModeKind::InputHelp,
                resume: ModeKind::Focus,
            },
            ..GrabSet::default()
        };
        r.links.view.store(Arc::new(GrabView {
            committed_gen: 2,
            committed: Arc::new(help),
            wanted_gen: None,
            wanted: None,
        }));
        let at = |ms| r.t0 + Duration::from_millis(ms);
        let _ = at(0);
        r.class(KC_T, T, true, 100);
        // The Reader never acknowledges.
        assert_eq!(r.lane.tick(r.t0 + Duration::from_millis(500)), None);
        assert_eq!(
            r.lane.tick(r.t0 + Duration::from_millis(1200)),
            Some(LaneAlert::HelpStall)
        );
        // Acknowledged keys never stall; silence for a minute is idle.
        r.links.reader_ack.store(1, Ordering::Release);
        assert_eq!(r.lane.tick(r.t0 + Duration::from_secs(30)), None);
        assert_eq!(
            r.lane.tick(r.t0 + Duration::from_secs(61)),
            Some(LaneAlert::HelpIdle)
        );
    }

    #[test]
    fn planned_commands_are_swallowed_not_executed() {
        let mut r = rig();
        let mut km = (*r.links.keymap.load_full()).clone();
        km.insert(
            TableKind::Global,
            ck(KC_A, true),
            1,
            Some(Bound {
                active: false,
                ..bound(9).unwrap()
            }),
        );
        r.links.keymap.store(Arc::new(km));
        r.class(KC_INS, INS, true, 1000);
        assert_eq!(r.class(KC_A, A, true, 1050), KeyClass::Swallowed);
    }

    /// K7 in miniature: double taps with intervals uniform in 300 to 700 ms, receipt times
    /// jittered by up to 5 ms. Outside the guard band the lane must match Mutter exactly; inside
    /// it may say `Ambiguous` but never the wrong thing.
    #[test]
    fn double_tap_classification_is_exact_outside_the_guard_band() {
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let mut r = rig();
        let (mut passed, mut swallowed, mut ambiguous) = (0, 0, 0);
        let mut t: u64 = 10_000;
        for _ in 0..2000 {
            t += 1000;
            let gap = 300 + next(401);
            let j = |base: u64, d: u64| base + d;
            let (d1, d2, d3) = (next(11), next(11), next(11));
            // Receipt times are the true times plus 0..10 ms of delay (jitter of +-5 ms around a
            // constant 5 ms latency, which cancels in the intervals).
            r.class(KC_INS, INS, true, j(t, d1));
            r.class(KC_INS, INS, false, j(t + 40, d2));
            let c = r.class(KC_INS, INS, true, j(t + 40 + gap, d3));
            r.class(KC_INS, INS, false, t + 40 + gap + 30);
            // The interval Mutter saw is `gap`; the lane saw `gap + d3 - d2`.
            match c {
                KeyClass::ModifierPassThrough => {
                    passed += 1;
                    assert!(gap < 500, "gap {gap} passed");
                }
                KeyClass::Swallowed => {
                    swallowed += 1;
                    assert!(gap >= 500, "gap {gap} swallowed");
                }
                KeyClass::Ambiguous { .. } => {
                    ambiguous += 1;
                    assert!((470..=530).contains(&gap), "gap {gap} ambiguous");
                }
                other => panic!("{other:?}"),
            }
            // Return to a known state for the next round.
            r.class(KC_A, A, true, t + 40 + gap + 100);
            r.class(KC_A, A, false, t + 40 + gap + 110);
        }
        assert!(
            passed > 500 && swallowed > 500,
            "{passed} {swallowed} {ambiguous}"
        );
        assert!(ambiguous < 400, "the band is narrow: {ambiguous}");
    }
}
