// SPDX-License-Identifier: GPL-3.0-or-later
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
/// P-13: a held key produces about 35 `KeyEvent`s in 1.5 s with no repeat flag. Repeats of a
/// command run at most this often (voice settings repeat; everything else does not repeat).
pub const REPEAT_MIN: Duration = Duration::from_millis(120);

/// Mutter hands a key that goes through the input method (a Wayland text-input client such as a
/// browser's editable) to the monitor twice: once as it arrives, once when the input method
/// forwards it back. The copies are identical and about 1 ms apart. No keyboard autorepeats at
/// this rate, so an identical event this soon after the same one is that copy.
pub const MIRROR_WINDOW: Duration = Duration::from_millis(8);
/// How many recent events are compared for a mirrored copy (copies of fast typing interleave).
const RECENT_KEYS: usize = 4;

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
    /// Identical copies of an event Mutter delivered twice, dropped before classification.
    pub mirrored: u64,
}

/// What [`InputLane::on_key`] produced.
#[derive(Copy, Clone, Debug)]
pub struct LaneOut {
    pub intent: KeyIntent,
    /// The lane advanced the epoch: wake the speech lane (E-2).
    pub epoch_advanced: bool,
    /// The replica predicts, for the first time since it last was clean, that Mutter 48.7 holds
    /// a grabbed modifier stale and swallows every key (see [`Verdict::stale`]). Tell the user
    /// once; it re-arms when a tap of the modifier heals the state.
    pub stale_modifier: Option<u32>,
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
    recent: SmallVec<[(KeyEvent, Instant); RECENT_KEYS]>,
    sent: u64,
    last_repeat_run: Option<Instant>,
    session: u64,
    stall_since: Option<Instant>,
    stats: LaneStats,
    /// The stale-modifier prediction was already reported and has not healed since.
    stale_told: bool,
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
            recent: SmallVec::new(),
            sent: 0,
            last_repeat_run: None,
            session: 0,
            stall_since: None,
            stats: LaneStats::default(),
            stale_told: false,
        }
    }

    pub fn stats(&self) -> LaneStats {
        self.stats
    }

    /// Forget the keyboard state: the monitor was lost or restarted, so Mutter's state is
    /// unknown and keys that were down will never be released on this connection.
    pub fn reset(&mut self) {
        self.down.clear();
        self.recent.clear();
        self.worlds.clear();
        self.worlds.push(World {
            r: Replica::default(),
            sel: Sel::Committed,
        });
        self.multi = None;
        self.shift_armed = None;
        self.stale_told = false;
    }

    /// Forget what is known of the keyboard because events were lost at this exact point of the
    /// stream (a full key channel): keys that were down may never be seen released.
    pub fn resync(&mut self) {
        self.reset();
        let view = self.links.view.load_full();
        self.seen = (view.committed_gen, view.wanted_gen);
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
        let session = self.links.session.load(Ordering::Acquire);
        if session != self.session {
            // The monitor connection changed: keys that were down will never be released on the
            // new one, and Mutter's replica state is gone.
            self.session = session;
            self.reset();
            self.seen = (view.committed_gen, view.wanted_gen);
        }
        if self.is_mirrored(&ev, at) {
            self.stats.mirrored += 1;
            return LaneOut {
                intent: KeyIntent {
                    event: ev,
                    kind: if ev.pressed {
                        PressKind::Repeat
                    } else {
                        PressKind::Release
                    },
                    class: KeyClass::Swallowed,
                    repeat: 1,
                    chord: None,
                    epoch: self.epoch.current(),
                    at,
                },
                epoch_advanced: false,
                stale_modifier: None,
            };
        }
        self.sync(&view);

        let kind = self.press_kind(&ev);
        let time_ms =
            u32::try_from(at.saturating_duration_since(self.start).as_millis()).unwrap_or(u32::MAX);
        let (verdict, nominal) = self.verdict(&ev, kind, time_ms, &view, &settings);
        let stale_modifier = self.note_stale(verdict);
        let ctx = view.committed.ctx;
        let is_gorca_mod = view.committed.modifiers.contains(&ev.keysym);
        let is_mod_key = is_gorca_mod || is_modifier_keysym(ev.keysym);

        let chord = (!is_mod_key && ev.pressed).then(|| ChordKey {
            mods: if verdict.unwrap_or(nominal).gorca_held {
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
                let in_help = ctx.mode == ModeKind::InputHelp
                    || view
                        .wanted
                        .as_ref()
                        .is_some_and(|w| w.ctx.mode == ModeKind::InputHelp);
                let plain_escape = ev.pressed
                    && ev.keysym == sym::ESCAPE
                    && ev
                        .modifiers
                        .without_locks()
                        .difference(Modifiers::SHIFT)
                        .is_empty();
                let committed = if in_help && plain_escape {
                    // Escape leaves learn mode: if it raced the grab change, say "again".
                    keymap.learn_toggle
                } else {
                    chord.and_then(|c| {
                        keymap
                            .lookup(lookup_mode(ctx), c, 1)
                            .filter(|b| b.active)
                            .map(|b| b.cmd)
                    })
                };
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
        let (mut class, bound) = class;
        if kind == PressKind::Repeat
            && matches!(class, KeyClass::Command(_))
            && self
                .last_repeat_run
                .is_some_and(|t| at.saturating_duration_since(t) < REPEAT_MIN)
        {
            // Debounce: a held key must not flood the Reader and the speech lane.
            class = KeyClass::Swallowed;
        } else if kind == PressKind::Repeat && matches!(class, KeyClass::Command(_)) {
            self.last_repeat_run = Some(at);
        }
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
            stale_modifier,
        }
    }

    /// Edge detector for [`LaneOut::stale_modifier`]. Only a verdict all worlds agree on counts.
    fn note_stale(&mut self, verdict: Option<Verdict>) -> Option<u32> {
        match verdict {
            Some(v) => match v.stale {
                Some(_) if self.stale_told => None,
                Some(m) => {
                    self.stale_told = true;
                    Some(m)
                }
                None => {
                    self.stale_told = false;
                    None
                }
            },
            None => None,
        }
    }

    /// Is `ev` the second copy of an event seen a moment ago ([`MIRROR_WINDOW`])? Records `ev`
    /// otherwise. A copy changes no lane state: it is as if it never arrived.
    fn is_mirrored(&mut self, ev: &KeyEvent, at: Instant) -> bool {
        self.recent
            .retain(|(_, t)| at.saturating_duration_since(*t) < MIRROR_WINDOW);
        if let Some(i) = self
            .recent
            .iter()
            .rposition(|(e, _)| e.keycode == ev.keycode)
            && self.recent[i].0 == *ev
        {
            // One copy per original: a third identical event is a real one.
            self.recent.remove(i);
            return true;
        }
        if self.recent.len() == RECENT_KEYS {
            self.recent.remove(0);
        }
        self.recent.push((*ev, at));
        false
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
    ) -> (Option<Verdict>, Verdict) {
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
        let Some(first) = verdicts.first().map(|(_, v)| *v) else {
            return (None, Verdict::default());
        };
        // `first` is the committed grab set's most likely world: what to look the chord up with
        // when the worlds disagree.
        let mut nominal = first;
        // If any world has the gorca modifier held, the key may be a command chord: look it up
        // so the Reader can ask the user to press again.
        nominal.gorca_held = verdicts.iter().any(|(_, v)| v.gorca_held);
        (
            verdicts.iter().all(|(_, v)| *v == first).then_some(first),
            nominal,
        )
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
                // Mutter 48.7 runs the double-tap rule for autorepeat too: some repeats leak to
                // the application (PF-5, P-13), the others are swallowed. Neither is a double
                // tap, and it must not look like one, 35 times.
                if kind == PressKind::Repeat {
                    if v.swallowed {
                        KeyClass::Swallowed
                    } else {
                        KeyClass::PassToApp
                    }
                } else if v.passed_modifier {
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
    fn a_copy_of_an_event_mutter_delivered_twice_is_swallowed() {
        // Input-method forwarding makes Mutter notify the monitor twice, ~1 ms apart.
        let mut r = rig();
        let first = r.key(KC_T, 0x68, true, 100, Modifiers::empty());
        assert_eq!(first.intent.class, KeyClass::PassToApp);
        let copy = r.key(KC_T, 0x68, true, 101, Modifiers::empty());
        assert_eq!(copy.intent.class, KeyClass::Swallowed);
        assert!(!copy.epoch_advanced);
        let up = r.key(KC_T, 0x68, false, 140, Modifiers::empty());
        assert_eq!(up.intent.kind, PressKind::Release);
        let up_copy = r.key(KC_T, 0x68, false, 141, Modifiers::empty());
        assert_eq!(up_copy.intent.class, KeyClass::Swallowed);
        assert_eq!(r.lane.stats().mirrored, 2);
        // The copy left no trace: the next press is a fresh press, not an autorepeat.
        let next = r.key(KC_T, 0x68, true, 300, Modifiers::empty());
        assert_eq!(next.intent.kind, PressKind::Press);
    }

    #[test]
    fn autorepeat_and_fast_retyping_are_not_copies() {
        let mut r = rig();
        let a = r.key(KC_T, 0x68, true, 100, Modifiers::empty());
        assert_eq!(a.intent.kind, PressKind::Press);
        // Autorepeat starts far later than the mirror window and repeats every 30 ms or more.
        let b = r.key(KC_T, 0x68, true, 600, Modifiers::empty());
        assert_eq!(b.intent.kind, PressKind::Repeat);
        assert_eq!(b.intent.class, KeyClass::PassToApp);
        let c = r.key(KC_T, 0x68, true, 630, Modifiers::empty());
        assert_eq!(c.intent.class, KeyClass::PassToApp);
        // Press, release, press of one key inside a few ms: the events differ.
        let mut r = rig();
        r.key(KC_T, 0x68, true, 0, Modifiers::empty());
        r.key(KC_T, 0x68, false, 3, Modifiers::empty());
        let again = r.key(KC_T, 0x68, true, 6, Modifiers::empty());
        assert_eq!(again.intent.kind, PressKind::Press);
        assert_eq!(r.lane.stats().mirrored, 0);
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

    /// Hold Insert from `t0` for `hold_ms` with autorepeat (first repeat after 500 ms, then
    /// every 30 ms), release it, and return every `stale_modifier` the lane reported.
    fn hold_insert(r: &mut Rig, t0: u64, hold_ms: u64) -> Vec<u32> {
        let mut told = Vec::new();
        told.extend(
            r.key(KC_INS, INS, true, t0, Modifiers::empty())
                .stale_modifier,
        );
        let mut t = 500;
        while t < hold_ms {
            told.extend(
                r.key(KC_INS, INS, true, t0 + t, Modifiers::empty())
                    .stale_modifier,
            );
            t += 30;
        }
        told.extend(
            r.key(KC_INS, INS, false, t0 + hold_ms, Modifiers::empty())
                .stale_modifier,
        );
        told
    }

    #[test]
    fn a_long_hold_of_the_modifier_is_predicted_stuck_and_keys_are_not_echoed() {
        let mut r = rig();
        assert_eq!(hold_insert(&mut r, 1000, 900), [INS], "told exactly once");
        // Mutter swallows Tab and a: never PassToApp (that would echo "tab"), never a command.
        let o = r.key(KC_A, A, true, 3000, Modifiers::empty());
        assert_eq!(o.intent.class, KeyClass::Swallowed);
        assert!(o.stale_modifier.is_none(), "not told again");
        assert_eq!(o.intent.chord.map(|c| c.mods), Some(Modifiers::empty()));
        // T is bound as gorca+T: without the modifier down it must not run that command.
        let o = r.key(KC_T, T, true, 3100, Modifiers::empty());
        assert_eq!(o.intent.class, KeyClass::Swallowed);
        r.class(KC_T, T, false, 3140);
        r.class(KC_A, A, false, 3150);
        // One tap heals it: keys reach the application again, and a new episode is told again.
        r.class(KC_INS, INS, true, 5000);
        r.class(KC_INS, INS, false, 5050);
        assert_eq!(r.class(KC_A, A, true, 5200), KeyClass::PassToApp);
        assert_eq!(hold_insert(&mut r, 8000, 900), [INS]);
    }

    /// A rig whose grab set holds Insert, the keypad Insert and Caps Lock.
    fn rig_all_modifiers() -> Rig {
        let r = rig();
        let mut g = GrabSet::default();
        for m in crate::keys::MODIFIER_KEYS {
            g.modifiers.insert(m.keysym);
        }
        r.links.view.store(Arc::new(GrabView {
            committed_gen: 2,
            committed: Arc::new(g),
            wanted_gen: None,
            wanted: None,
        }));
        r
    }

    /// Keyboard report, items 2 and 3: autorepeat of a held modifier is never a double tap, never
    /// arms the pass-through, and the modifier stays held for a chord pressed during the repeats.
    #[test]
    fn autorepeat_of_a_held_modifier_is_no_double_tap_and_the_chord_still_runs() {
        for m in crate::keys::MODIFIER_KEYS {
            if !m.autorepeats {
                continue;
            }
            let mut r = rig_all_modifiers();
            let none = Modifiers::empty();
            let t0 = 1000;
            assert_eq!(
                r.class(m.keycode, m.keysym, true, t0),
                KeyClass::Swallowed,
                "{}: the first press",
                m.name
            );
            // First repeat after the repeat delay, then every 30 ms, like the keyboard.
            let mut t = 500;
            while t < 700 {
                let c = r.class(m.keycode, m.keysym, true, t0 + t);
                assert!(
                    !matches!(c, KeyClass::ModifierPassThrough | KeyClass::Command(_)),
                    "{}: a repeat at {t} ms is {c:?}",
                    m.name
                );
                t += 30;
            }
            // The chord key pressed while the modifier is still down (and repeating).
            let o = r.key(KC_T, T, true, t0 + 720, none);
            assert!(
                matches!(o.intent.class, KeyClass::Command(_) | KeyClass::Swallowed),
                "{}: chord during repeats is {:?}",
                m.name,
                o.intent.class
            );
            assert!(
                o.intent
                    .chord
                    .is_some_and(|c| c.mods.contains(Modifiers::GORCA)),
                "{}: the modifier is still held during repeats",
                m.name
            );
            // Nothing the lane reports is ever a pass-through of another key.
            assert_ne!(o.intent.event.keysym, 0xffb0);
        }
    }

    /// A fast tap followed by a hold: the second press is the real double tap, its repeats are not.
    #[test]
    fn a_double_tap_then_hold_passes_once_and_repeats_do_not_pass_again() {
        let mut r = rig_all_modifiers();
        let (code, sym) = (90, crate::keys::sym::KP_INSERT);
        assert_eq!(r.class(code, sym, true, 1000), KeyClass::Swallowed);
        assert_eq!(r.class(code, sym, false, 1040), KeyClass::Swallowed);
        assert_eq!(
            r.class(code, sym, true, 1160),
            KeyClass::ModifierPassThrough
        );
        let mut passes = 0;
        for i in 0..20 {
            if r.class(code, sym, true, 1160 + 500 + 30 * i) == KeyClass::ModifierPassThrough {
                passes += 1;
            }
        }
        assert_eq!(passes, 0, "repeats never count as a pass-through");
    }

    #[test]
    fn a_short_hold_is_not_stuck() {
        let mut r = rig();
        assert!(hold_insert(&mut r, 1000, 540).is_empty());
        assert_eq!(r.class(KC_A, A, true, 3000), KeyClass::PassToApp);
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
        // The chord key matches a command: the Reader is told which one, to say "again".
        assert_eq!(
            c,
            KeyClass::Ambiguous {
                committed: Some(CmdId(1))
            }
        );
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

    /// Reviewer probe audit_m1_overflow, as a test: the resync happens at the gap. The queue holds
    /// [Insert press]; the Insert release does not fit and is dropped; the next event that gets
    /// through is marked. A plain T after the gap is a plain T.
    #[test]
    fn a_release_dropped_on_overflow_cannot_make_a_later_t_a_command() {
        let mut r = rig();
        r.class(KC_INS, INS, true, 1000);
        // (the release is lost; the marked event is handled after resync())
        r.lane.resync();
        assert_eq!(r.class(KC_T, T, true, 3000), KeyClass::PassToApp);
        // Without the resync the same sequence is the bug the reviewer found.
        let mut r = rig();
        r.class(KC_INS, INS, true, 1000);
        assert_eq!(r.class(KC_T, T, true, 3000), KeyClass::Command(CmdId(1)));
    }

    #[test]
    fn a_new_monitor_session_forgets_held_keys() {
        let mut r = rig();
        r.class(KC_INS, INS, true, 1000);
        // The shell restarts: Insert is still "down" in the lane, but the manager is new.
        r.links.session.fetch_add(1, Ordering::AcqRel);
        assert_eq!(
            r.class(KC_T, T, true, 1500),
            KeyClass::PassToApp,
            "a plain T is not Insert+T after a restart"
        );
        r.class(KC_T, T, false, 1510);
        assert_eq!(r.class(KC_INS, INS, true, 3000), KeyClass::Swallowed);
        assert_eq!(r.class(KC_T, T, true, 3050), KeyClass::Command(CmdId(1)));
    }

    #[test]
    fn a_held_modifier_and_a_held_command_key_are_debounced() {
        let mut r = rig();
        let mut km = (*r.links.keymap.load_full()).clone();
        km.insert(
            TableKind::Global,
            ck(KC_A, true),
            1,
            Some(Bound {
                repeatable: true,
                ..bound(5).unwrap()
            }),
        );
        r.links.keymap.store(Arc::new(km));
        r.class(KC_INS, INS, true, 1000);
        // About 35 repeats of Insert in 1.5 s: never a double tap, never an interrupt. Mutter
        // 48.7 swallows some of them and passes the others (the double-tap rule runs for
        // autorepeat), so either class is right, a pass-through marker or a command is not.
        let e = r.epoch.current();
        for i in 0..35 {
            let o = r.key(KC_INS, INS, true, 1500 + i * 40, Modifiers::empty());
            assert_eq!(o.intent.kind, PressKind::Repeat);
            assert!(
                matches!(o.intent.class, KeyClass::PassToApp | KeyClass::Swallowed),
                "{:?}",
                o.intent.class
            );
            assert!(!o.epoch_advanced);
        }
        assert_eq!(r.epoch.current(), e);
        // A repeatable command held at 30 Hz runs about every 120 ms, not on every event.
        assert_eq!(r.class(KC_A, A, true, 3000), KeyClass::Command(CmdId(5)));
        let runs = (1..=30)
            .filter(|i| matches!(r.class(KC_A, A, true, 3000 + i * 33), KeyClass::Command(_)))
            .count();
        assert!((6..=9).contains(&runs), "{runs}");
        // A command that does not repeat is never repeated.
        r.class(KC_A, A, false, 4100);
        r.class(KC_T, T, true, 4200);
        assert!((1..=10).all(|i| r.class(KC_T, T, true, 4200 + i * 33) == KeyClass::Swallowed));
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
    fn escape_that_races_entering_learn_mode_asks_for_another_press() {
        let mut r = rig();
        let help = GrabSet {
            grab_all: true,
            modifiers: [INS].into_iter().collect(),
            ctx: InputContext {
                mode: ModeKind::InputHelp,
                resume: ModeKind::Focus,
            },
            ..GrabSet::default()
        };
        // Learn mode is wanted but not committed yet: the Escape could go either way.
        pending_view(&r, help);
        let c = r.class(9, sym::ESCAPE, true, 1000);
        assert_eq!(
            c,
            KeyClass::Ambiguous {
                committed: Some(CmdId(102))
            },
            "never run, but the Reader says \"again\""
        );
        // A plain key in the same window gets no cue.
        r.class(9, sym::ESCAPE, false, 1010);
        assert_eq!(
            r.class(KC_A, A, true, 1100),
            KeyClass::Ambiguous { committed: None },
            "an unbound key in the window is counted but gets no cue"
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
