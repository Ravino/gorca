//! M1 end-to-end gates K2, K3, K4 (docs/plan/phase-2.md 2.1, ADR-0005 10) plus K2s.
//!
//! The stimulus drives `fake_mutter.py`, which ports Mutter's grab semantics (exact-mask matching,
//! the a11y-modifier first-click rule, consumed vs passed keys). Chords are the ADR-0005 default
//! keymap (NVDA desktop preset) with the real XKB keycodes the reader matches physically (K-8).
//! Until M1 is switched on every scenario is **skipped** (`skipped = 1`); two switches change that:
//!
//! * `GORCA_M1=1`, given as `--reader-env GORCA_M1=1` (the process environment is never read, so
//!   the unit tests do not depend on it): run the scenario and enforce its gate.
//! * `GORCA_M1_STRICT=1`: the scenario fails with an error instead of skipping.
//!
//! | id | stimulus | metrics |
//! |---|---|---|
//! | K2 | `gorca+F12` (`info.time_date`, zero IPC), idle | `k2_ms` key-down emit to SPEAK, `lost`, `not_consumed` |
//! | K2s | the same chord while an L3-50k storm runs | `k2s_ms`, `lost`, `not_consumed`, storm rate |
//! | K3 | `gorca+Tab` (`info.where_am_i`) on a freshly focused node, fake app answering in about 1 ms | `k3_ms`, `lost`, `not_consumed` |
//! | K4 | 10k randomized grab-set switches (pass-next-key, learn mode) interleaved with typing | `app_lost`, `app_doubled`, `chord_leaked`, `forged_accepted` |
//!
//! Event times are virtual (`time_ms` of the emulated key event) so Mutter's first-click rule is
//! deterministic: every chord starts more than the repeat delay (500 ms) after the previous
//! modifier release, so the modifier press is always a fresh first click (swallowed).
//! The reader gets its desktop facts from `GORCA_*` overrides set by [`m1_env`], so it never
//! runs `gsettings` or reads a settings file in the harness.

use std::time::Duration;

use anyhow::{Result, bail};
use gorca_bench_b2::clock::Rng;

use super::apps::ReaderSpec;
use super::mock::Flavor;
use super::runner::{HostEnv, Rig};
use super::scenarios::{RunOut, Size, base_opts, ms, pause, speaks, start_storm};

/// A physical key: XKB keycode (evdev + 8) and the keysym it produces.
#[derive(Copy, Clone, Debug)]
pub(super) struct K {
    pub(super) code: u16,
    pub(super) sym: u32,
}

const INSERT: K = K {
    code: 118,
    sym: 0xff63,
};
const F12: K = K {
    code: 96,
    sym: 0xffc9,
};
const TAB: K = K {
    code: 23,
    sym: 0xff09,
};
const F2: K = K {
    code: 68,
    sym: 0xffbf,
};
const KEY_1: K = K {
    code: 10,
    sym: 0x31,
};
const ESCAPE: K = K {
    code: 9,
    sym: 0xff1b,
};
/// Virtual gap between chords: above Mutter's default repeat delay (500 ms).
const CHORD_GAP_MS: u64 = 700;
/// Longest wait for the reader to bring Mutter's grabs to the state a switch asks for.
const GRABS_TIMEOUT: Duration = Duration::from_secs(2);
/// Guard after Mutter applied a grab set: reply latency plus the reader's 1 ms settle.
const GUARD_MS: u64 = 8;
/// The normal grab set: Insert, KP_Insert and Caps_Lock.
const NORMAL_MODIFIERS: u32 = 3;
/// First node of the K3 series (warm-up uses 1..70).
const FIRST_NODE: usize = 100;

fn setting(host: &HostEnv, key: &str) -> Option<String> {
    host.reader_env
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
}

pub(super) fn flag(host: &HostEnv, key: &str) -> bool {
    setting(host, key).is_some_and(|v| v == "1")
}

/// Decide whether a gate runs. `Ok(false)` = skipped (already noted in `out`).
fn gate_open(host: &HostEnv, id: &str, out: &mut RunOut) -> Result<bool> {
    if flag(host, "GORCA_M1") {
        return Ok(true);
    }
    if flag(host, "GORCA_M1_STRICT") {
        bail!("{id}: M1 has not landed and GORCA_M1_STRICT=1 requires a red gate");
    }
    out.notes.push(format!(
        "{id} SKIPPED: set GORCA_M1=1 to run the M1 command-layer gates"
    ));
    out.x("skipped", 1.0, "flag");
    Ok(false)
}

pub(super) struct Chord {
    modifier: K,
    key: K,
}

/// Virtual clock plus keycode allocator of one scenario.
#[derive(Debug)]
pub(super) struct Typist {
    pub(super) t_ms: u64,
    next_keycode: u16,
}

impl Typist {
    pub(super) fn new() -> Self {
        Self {
            t_ms: 1_000_000,
            next_keycode: 200,
        }
    }

    /// Unique synthetic keycodes for typed letters, so `score_typing` can tell keys apart.
    fn keycode(&mut self) -> u16 {
        let k = self.next_keycode;
        self.next_keycode = if k >= 60_000 { 200 } else { k + 1 };
        k
    }
}

/// One chord: modifier down, key down, key up, modifier up. Returns the key-down emit time and
/// whether Mutter consumed the key-down.
pub(super) fn chord(rig: &mut Rig, ty: &mut Typist, c: &Chord) -> Result<(u64, bool)> {
    ty.t_ms += CHORD_GAP_MS;
    let m = rig
        .mutter
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("no mutter"))?;
    m.key_ex(c.modifier.code, c.modifier.sym, 0, false, ty.t_ms)?;
    let down = m.key_ex(c.key.code, c.key.sym, 0, false, ty.t_ms + 5)?;
    m.key_ex(c.key.code, c.key.sym, 0, true, ty.t_ms + 15)?;
    m.key_ex(c.modifier.code, c.modifier.sym, 0, true, ty.t_ms + 25)?;
    ty.t_ms += 25;
    Ok((down.t_emit_ns, down.consumed))
}

/// A plain key press and release (Mutter's verdict on both).
fn tap(rig: &mut Rig, ty: &mut Typist, k: K) -> Result<(bool, bool)> {
    let m = rig
        .mutter
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("no mutter"))?;
    ty.t_ms += 3;
    let down = m.key_ex(k.code, k.sym, 0, false, ty.t_ms)?;
    ty.t_ms += 3;
    let up = m.key_ex(k.code, k.sym, 0, true, ty.t_ms)?;
    Ok((down.consumed, up.consumed))
}

/// Desktop facts for the reader, so it never asks `gsettings` in the harness: GNOME's default
/// repeat delay, no XKB options, a 24-hour clock.
pub(super) fn m1_env(host: &HostEnv) -> HostEnv {
    let mut h = host.clone();
    for (k, v) in [
        ("GORCA_REPEAT_DELAY_MS", "500"),
        ("GORCA_XKB_OPTIONS", ""),
        ("GORCA_CLOCK_24H", "1"),
    ] {
        if !h.reader_env.iter().any(|(e, _)| e == k) {
            h.reader_env.push((k.to_owned(), v.to_owned()));
        }
    }
    h
}

pub(super) fn m1_rig(host: &HostEnv, reader: &ReaderSpec, flavor: Flavor) -> Result<Rig> {
    let mut o = base_opts(reader, flavor);
    o.keys = true;
    if matches!(reader, ReaderSpec::Daemon { .. }) {
        // The shutdown counters of the reader's speech lane: see `account`.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        o.reader_stderr = Some(
            host.sock_dir
                .join(format!("reader-{}-{nanos}.log", std::process::id())),
        );
    }
    let rig = Rig::start(&m1_env(host), &o)?;
    rig.mock
        .configure(Duration::from_millis(1), Duration::from_millis(1));
    if !rig.keys_ok {
        bail!("the reader did not register with the emulated keyboard monitor (no SetKeyGrabs)");
    }
    Ok(rig)
}

/// What a series does before each chord.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Before {
    Nothing,
    /// Move focus to a fresh node and press at once: the answer needs a fetch (cache cold).
    FocusFresh,
}

/// Run `n` chords, score key-down emit to the first SPEAK after it (the answer; the speech of
/// the previous action is cancelled by the epoch of the key).
pub(super) fn chord_series(
    rig: &mut Rig,
    out: &mut RunOut,
    metric: &str,
    c: &Chord,
    n: usize,
    seed: u64,
    before: Before,
    hook: Option<&mut dyn FnMut(&mut Rig)>,
) -> Result<()> {
    let mut rng = Rng::new(seed);
    let mut ty = Typist::new();
    let mark = rig.mock.mark();
    let mut emits = Vec::with_capacity(n);
    let mut not_consumed = 0u64;
    for i in 0..n {
        if before == Before::FocusFresh {
            let app = rig
                .apps
                .get_mut("app")
                .ok_or_else(|| anyhow::anyhow!("no app"))?;
            app.focus(FIRST_NODE + i)?;
        }
        let (t, consumed) = chord(rig, &mut ty, c)?;
        emits.push(t);
        if !consumed {
            not_consumed += 1;
        }
        pause(&mut rng, 8, 16);
    }
    std::thread::sleep(Duration::from_millis(400));
    if let Some(h) = hook {
        h(rig);
    }
    let recs = rig.mock.since(mark);
    let spoken: Vec<u64> = speaks(&recs).map(|(r, _)| r.t_ns).collect();
    let mut lat = Vec::with_capacity(n);
    let mut window_miss = 0u64;
    // The first SPEAK in [emit_i, emit_i+1) belongs to chord i; none in the window means the
    // answer is missing from it (late, superseded by the next chord, or lost: see below).
    for (i, t) in emits.iter().enumerate() {
        let next = emits.get(i + 1).copied().unwrap_or(u64::MAX);
        match spoken.iter().find(|s| **s >= *t && **s < next) {
            Some(s) => lat.push(ms(s - *t)),
            None => window_miss += 1,
        }
    }
    out.notes.push(format!(
        "{metric}: first SPEAK between a command key and the next one ({} spoken for {n} chords)",
        spoken.len()
    ));
    out.s(metric, lat);
    // `lost` is a silent loss. A chord with no SPEAK in its window is not one by itself: its
    // answer may have reached the server after the next key (late), or the next command's
    // answer may have overwritten it in the lane's interrupt slot before the lane thread woke
    // (`superseded`, latest wins, counted by the reader). The reader's own counters decide.
    match rig.account_speech() {
        Some(a) => {
            out.x("lost", a.silent_loss() as f64, "events");
            out.x("superseded", a.superseded as f64, "events");
            out.x(
                "late",
                window_miss.saturating_sub(a.counted_drops()) as f64,
                "events",
            );
            out.notes.push(format!(
                "{metric}: {window_miss} chords without a SPEAK in their window; reader counters: submitted {}, spoken {}, superseded {}, stale {}, inbox {}, received by the server {}",
                a.submitted, a.spoken, a.superseded, a.stale_dropped, a.inbox_dropped, a.received
            ));
        }
        None => out.x("lost", window_miss as f64, "events"),
    }
    out.x("not_consumed", not_consumed as f64, "events");
    Ok(())
}

/// `info.time_date`: zero IPC, always speaks.
pub(super) fn time_chord() -> Chord {
    Chord {
        modifier: INSERT,
        key: F12,
    }
}

/// K2: command key to speech, idle host, zero-IPC command. Gate p99 5 ms (target 2 ms).
pub fn k2(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "K2", out)? {
        return Ok(());
    }
    let mut rig = m1_rig(host, reader, flavor)?;
    chord_series(
        &mut rig,
        out,
        "k2_ms",
        &time_chord(),
        size.n,
        seed,
        Before::Nothing,
        None,
    )
}

/// K2s: the same chord while a 50k events/s storm runs on the a11y bus. Gate: K2's ceiling plus
/// 1 ms, `lost = 0`; the modifier must keep being swallowed under the storm.
pub fn k2s(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "K2s", out)? {
        return Ok(());
    }
    const RATE: f64 = 50_000.0;
    let mut rig = m1_rig(host, reader, flavor)?;
    let series_s = size.n as f64 * 0.012 + 1.0;
    let storm = start_storm(&rig.env.a11y_addr, RATE, series_s + 3.0)?;
    std::thread::sleep(Duration::from_millis(1000));
    let r = chord_series(
        &mut rig,
        out,
        "k2s_ms",
        &time_chord(),
        size.n,
        seed,
        Before::Nothing,
        None,
    );
    storm.stop.store(true, std::sync::atomic::Ordering::Release);
    let fr = storm
        .thread
        .join()
        .map_err(|_| anyhow::anyhow!("storm thread panicked"))?;
    r?;
    out.x("storm_target_eps", RATE, "ev/s");
    out.x("storm_achieved_eps", fr.achieved_rate(), "ev/s");
    Ok(())
}

/// K3: `info.where_am_i` on a freshly focused node, the fake app answering in about 1 ms.
/// Gate p99 15 ms (target 5 ms), `lost = 0`.
pub fn k3(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "K3", out)? {
        return Ok(());
    }
    let mut rig = m1_rig(host, reader, flavor)?;
    let c = Chord {
        modifier: INSERT,
        key: TAB,
    };
    chord_series(
        &mut rig,
        out,
        "k3_ms",
        &c,
        size.n,
        seed,
        Before::FocusFresh,
        None,
    )
}

fn forger_command(rig: &Rig, args: &[&str]) -> std::process::Command {
    let mut c = std::process::Command::new("/usr/bin/python3");
    // The system python has the D-Bus bindings (the project venv does not), like fake_mutter.py.
    c.arg("-I")
        .arg(rig.env.harness_dir.join("key_forger.py"))
        .arg(&rig.env.session_addr)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    c
}

/// Run the forger: a rogue client addressing every kind of forged manager and bus signal to the
/// reader (forged `KeyEvent`s, a forged chord, wrong-shaped signals, forged `NameOwnerChanged`).
/// Returns how much they achieved: SPEAKs they caused, plus grab calls the reader made afterwards
/// (a forged owner change would restart its keyboard session and re-send its grabs). Must be 0.
fn forged_keys(rig: &mut Rig) -> Result<u64> {
    let applied0 = mutter(rig)?.grab_state()?.applied;
    let mark = rig.mock.mark();
    // The reader may be reconnecting right now (an owner change just happened): wait, checked,
    // until it owns its name again.
    let mut out = forger_command(rig, &["5", "all"]).output()?;
    for _ in 0..20 {
        if out.status.success() {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
        out = forger_command(rig, &["5", "all"]).output()?;
    }
    if !out.status.success() {
        bail!(
            "key_forger.py failed: {} {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    std::thread::sleep(Duration::from_millis(300));
    let speeches = speaks(&rig.mock.since(mark)).count() as u64;
    let regrabbed = mutter(rig)?.grab_state()?.applied.saturating_sub(applied0);
    Ok(speeches + regrabbed)
}

/// Like [`restart_mutter`], with a forger flooding every kind of forged signal the whole time:
/// during the owner transition a forged signal must neither get through nor stop the genuine
/// restart. Returns `(restore ms, speeches the forgeries caused)`.
fn restart_mutter_under_flood(rig: &mut Rig) -> Result<(f64, u64)> {
    let mark = rig.mock.mark();
    let mut child = forger_command(rig, &["1", "flood:2500"]).spawn()?;
    std::thread::sleep(Duration::from_millis(150));
    let restored = restart_mutter(rig);
    // Reap on every path: no stray process, no leftover pipes.
    let waited = child.wait();
    let restored = restored?;
    waited?;
    std::thread::sleep(Duration::from_millis(300));
    // The genuine restart is announced ("keyboard commands unavailable", "... restored"); any
    // other speech was caused by a forged signal.
    let n = speaks(&rig.mock.since(mark))
        .filter(|(_, t)| !t.starts_with("keyboard commands"))
        .count() as u64;
    Ok((restored, n))
}

/// Replace the emulated manager by a new process (a new unique name) and time how long the
/// reader needs to register its grabs with it again.
fn restart_mutter(rig: &mut Rig) -> Result<f64> {
    rig.mutter = None;
    let t0 = std::time::Instant::now();
    let mut m = super::apps::FakeMutter::spawn(&rig.env.harness_dir)?;
    let end = t0 + Duration::from_secs(10);
    while m.clients() == 0 {
        if std::time::Instant::now() > end {
            bail!("the reader did not re-register with the restarted manager within 10 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    rig.mutter = Some(m);
    Ok(ms)
}

/// Rounds of a K4 walk: 500 with `--quick`, 10k otherwise, or what `--reader-env
/// GORCA_M1_ROUNDS=N` asks for (the review runs 30k).
fn rounds_of(host: &HostEnv, size: Size) -> usize {
    setting(host, "GORCA_M1_ROUNDS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(if size.n >= 1000 { 10_000 } else { 500 })
}

/// A plain typed letter with a unique synthetic keycode; returns Mutter's verdict
/// (`down.consumed || up.consumed`).
fn type_letter(
    rig: &mut Rig,
    ty: &mut Typist,
    rng: &mut Rng,
    typed: &mut Vec<(u16, u32)>,
    letters: &[u32],
) -> Result<bool> {
    let sym = letters[rng.below(letters.len() as u64) as usize];
    let kc = ty.keycode();
    let m = rig
        .mutter
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("no mutter"))?;
    ty.t_ms += 3;
    let down = m.key_ex(kc, sym, 0, false, ty.t_ms)?;
    ty.t_ms += 3;
    let up = m.key_ex(kc, sym, 0, true, ty.t_ms)?;
    typed.push((kc, sym));
    Ok(down.consumed || up.consumed)
}

/// Wait until Mutter holds the grab state `ok`, then give the reader the time it needs to learn
/// that: the `SetKeyGrabs` reply travels back and the machine settles for 1 ms (P-7). Keys typed
/// earlier would meet a grab set the reader still sees as pending and be `Ambiguous` by design.
fn settle_grabs(rig: &mut Rig, ok: impl Fn(&super::apps::GrabState) -> bool) -> Result<bool> {
    let (_, ok) = mutter(rig)?.wait_grabs(GRABS_TIMEOUT, ok)?;
    std::thread::sleep(Duration::from_millis(GUARD_MS));
    Ok(ok)
}

/// Send `c` until Mutter holds the grab state `ok`. A chord that arrives while the reader still
/// sees the previous grab change as pending is `Ambiguous` by design (counted, never run, never
/// re-executed); like a user who heard nothing, the harness presses it again. Returns whether the
/// state was reached, the first attempt's `consumed`, and how many chords had to be repeated.
fn chord_until(
    rig: &mut Rig,
    ty: &mut Typist,
    c: &Chord,
    ok: impl Fn(&super::apps::GrabState) -> bool + Copy,
) -> Result<(bool, bool, u64)> {
    let mut resent = 0;
    let mut first = true;
    let mut first_consumed = true;
    for _ in 0..4 {
        let (_, consumed) = chord(rig, ty, c)?;
        if first {
            first_consumed = consumed;
            first = false;
        }
        let (_, reached) = mutter(rig)?.wait_grabs(Duration::from_millis(300), ok)?;
        std::thread::sleep(Duration::from_millis(GUARD_MS));
        if reached {
            return Ok((true, first_consumed, resent));
        }
        resent += 1;
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok((false, first_consumed, resent))
}

/// Score the letters typed so far against the app log and start over. Synthetic keycodes wrap at
/// 16 bits, so a long walk is scored in chunks (a wrapped keycode would look doubled).
fn flush_scores(
    rig: &mut Rig,
    typed: &mut Vec<(u16, u32)>,
    swallowed: &mut u64,
    totals: &mut (u64, u64, u64, Vec<u16>),
) -> Result<()> {
    let log = mutter(rig)?.app_log(true)?;
    let (lost, doubled) = score_typing(typed, &log);
    totals.0 += lost + std::mem::take(swallowed);
    // A duplicate delivered after the previous flush lands in this chunk's log under a keycode
    // this chunk did not type: the last few codes of the previous chunk are carried over.
    totals.1 += doubled + straddling_doubles(&log, &totals.3);
    totals.2 += unbalanced_keys(&log);
    totals.3 = typed.iter().rev().take(CARRY).map(|(kc, _)| *kc).collect();
    typed.clear();
    Ok(())
}

/// Keycodes carried across a chunk boundary.
const CARRY: usize = 64;

/// Entries of `log` under a keycode that only the previous chunk typed (and whose presses and
/// releases that chunk's log already accounted for): each one is a duplicate.
fn straddling_doubles(log: &[(u16, u32, bool)], carry: &[u16]) -> u64 {
    log.iter().filter(|(kc, _, _)| carry.contains(kc)).count() as u64
}

/// Letters per scoring chunk: well under the 60k synthetic keycodes before they wrap.
const SCORE_CHUNK: usize = 20_000;

fn mutter(rig: &mut Rig) -> Result<&mut super::apps::FakeMutter> {
    rig.mutter
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("no mutter"))
}

/// F4: the manager's name flaps (a hundred owner changes at 0 to 5 ms) while the reader connects
/// and while a new emulated manager starts. The reader must end up trusting the real owner: a
/// chord is accepted afterwards. Returns whether it recovered.
fn flap_and_restart(rig: &mut Rig, seed: u64) -> Result<bool> {
    rig.mutter = None;
    let script = rig.env.harness_dir.join("name_flapper.py");
    let mut child = std::process::Command::new("/usr/bin/python3")
        .arg("-I")
        .arg(&script)
        .arg(&rig.env.session_addr)
        .arg("150")
        .arg(seed.to_string())
        .stdout(std::process::Stdio::null())
        .spawn()?;
    // The new manager starts while the name is still flapping (it retries until it owns it).
    std::thread::sleep(Duration::from_millis(120));
    let spawned = super::apps::FakeMutter::spawn(&rig.env.harness_dir);
    // Reap on every path.
    let waited = child.wait();
    let mut m = spawned?;
    waited?;
    let end = std::time::Instant::now() + Duration::from_secs(8);
    while m.clients() == 0 {
        if std::time::Instant::now() > end {
            rig.mutter = Some(m);
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    rig.mutter = Some(m);
    let mut ty = Typist::new();
    let before = rig.mock.mark();
    let (_, consumed) = chord(rig, &mut ty, &time_chord())?;
    std::thread::sleep(Duration::from_millis(300));
    Ok(consumed && speaks(&rig.mock.since(before)).count() > 0)
}

/// K4: no lost or doubled keys across randomized grab-set switches (ADR-0001 M-16).
///
/// Each round switches the grab set with a chord (`input.pass_next_key`, which empties it and
/// restores it on the next key, or `input.learn_mode_toggle` followed by Escape, which grabs
/// everything and releases it), waits **on the emulated manager's own state** until the reader's
/// grab calls have been applied (no fixed sleeps, a few ms per switch), then types plain
/// letters. In no round may the reader swallow a letter (`app_lost`), and the app must see
/// each exactly once (`app_doubled`). A chord must be consumed whenever it is sent in the
/// normal mode (`chord_leaked`). Ground truth is Mutter's own verdict (`consumed`) and the app
/// log of `fake_mutter.py`, never the reader's speech. A forged `KeyEvent` from another client
/// must do nothing (`forged_accepted`), also after the manager restarted under a new name.
pub fn k4(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "K4", out)? {
        return Ok(());
    }
    let switches = rounds_of(host, size);
    let pass = Chord {
        modifier: INSERT,
        key: F2,
    };
    let learn = Chord {
        modifier: INSERT,
        key: KEY_1,
    };
    let letters: Vec<u32> = (u32::from(b'a')..=u32::from(b'z')).collect();
    let mut rig = m1_rig(host, reader, flavor)?;
    mutter(&mut rig)?.app_log(true)?;
    let flap_ok = flap_and_restart(&mut rig, seed)?;
    let mut forged = forged_keys(&mut rig)?;
    // The shell restarts: the manager comes back under a new unique name. The reader must
    // re-authorise and restore its grabs within 2 s, accept the new owner, and still ignore
    // forgeries (ADR-0005 8.2).
    let (restored_ms, flooded) = restart_mutter_under_flood(&mut rig)?;
    forged += flooded;
    forged += forged_keys(&mut rig)?;
    let mut ty0 = Typist::new();
    let before = rig.mock.mark();
    let (_, consumed) = chord(&mut rig, &mut ty0, &time_chord())?;
    std::thread::sleep(Duration::from_millis(300));
    let after_restart_ok = consumed && speaks(&rig.mock.since(before)).count() > 0;
    mutter(&mut rig)?.app_log(true)?;
    let mut rng = Rng::new(seed);
    let mut ty = Typist::new();
    let (mut typed, mut leaked, mut swallowed_live) = (Vec::new(), 0u64, 0u64);
    let (mut escape_unconsumed, mut never_settled, mut resent) = (0u64, 0u64, 0u64);
    let started = std::time::Instant::now();
    let mut totals = (0u64, 0u64, 0u64, Vec::<u16>::new());
    let mut typed_total = 0usize;
    for round in 0..switches {
        if typed.len() >= SCORE_CHUNK {
            typed_total += typed.len();
            flush_scores(&mut rig, &mut typed, &mut swallowed_live, &mut totals)?;
        }
        let use_learn = rng.below(4) == 0;
        if use_learn {
            let (ok, consumed, again) = chord_until(&mut rig, &mut ty, &learn, |g| g.grab_all)?;
            resent += again;
            if !consumed {
                leaked += 1;
            }
            never_settled += u64::from(!ok);
            if !ok {
                eprintln!(
                    "K4 DEBUG wait 1 timed out at switch {round}: {:?}",
                    mutter(&mut rig)?.grab_state()?
                );
            }
            // Learn mode holds the keyboard: Escape is delivered to the reader, not the app. An
            // Escape that raced the grab change is Ambiguous by design ("again"); press it again
            // while Mutter still holds the keyboard.
            let mut left = false;
            for attempt in 0..4 {
                let (d, u) = tap(&mut rig, &mut ty, ESCAPE)?;
                if !(d && u) {
                    escape_unconsumed += 1;
                }
                let (_, reached) = mutter(&mut rig)?
                    .wait_grabs(Duration::from_millis(300), |g| {
                        !g.grab_all && g.modifiers == NORMAL_MODIFIERS
                    })?;
                std::thread::sleep(Duration::from_millis(GUARD_MS));
                if reached {
                    left = true;
                    break;
                }
                resent += u64::from(attempt < 3);
                std::thread::sleep(Duration::from_millis(20));
            }
            never_settled += u64::from(!left);
        } else {
            // The modifier grab is emptied once Insert is up (never while it is held).
            let (ok, consumed, again) = chord_until(&mut rig, &mut ty, &pass, |g| {
                g.modifiers == 0 && !g.grab_all
            })?;
            resent += again;
            if !consumed {
                leaked += 1;
            }
            never_settled += u64::from(!ok);
            if !ok {
                eprintln!(
                    "K4 DEBUG wait 3 timed out at switch {round}: {:?}",
                    mutter(&mut rig)?.grab_state()?
                );
            }
        }
        for i in 0..=rng.below(3) {
            if type_letter(&mut rig, &mut ty, &mut rng, &mut typed, &letters)? {
                swallowed_live += 1;
            }
            if i == 0 && !use_learn {
                // The first letter ended pass-next-key: the modifiers come back.
                let ok = settle_grabs(&mut rig, |g| g.modifiers == NORMAL_MODIFIERS)?;
                never_settled += u64::from(!ok);
                if !ok {
                    eprintln!(
                        "K4 DEBUG wait 4 timed out at switch {round}: {:?}",
                        mutter(&mut rig)?.grab_state()?
                    );
                }
            }
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    std::thread::sleep(Duration::from_millis(200));
    if flag(host, "GORCA_M1_INJECT_DOUBLE") {
        // Negative control for the scoring: a real double, delivered to the application by
        // Mutter, must be counted. The last typed letter is pressed and released once more.
        if let Some(&(kc, sym)) = typed.last() {
            let m = mutter(&mut rig)?;
            ty.t_ms += 3;
            m.key_ex(kc, sym, 0, false, ty.t_ms)?;
            m.key_ex(kc, sym, 0, true, ty.t_ms + 3)?;
        }
    }
    typed_total += typed.len();
    let last_typed = typed.last().copied();
    flush_scores(&mut rig, &mut typed, &mut swallowed_live, &mut totals)?;
    if setting(host, "GORCA_M1_INJECT_DOUBLE").as_deref() == Some("straddle")
        && let Some((kc, sym)) = last_typed
    {
        // Negative control for the chunk boundary: the duplicate arrives after the flush.
        let m = mutter(&mut rig)?;
        ty.t_ms += 3;
        m.key_ex(kc, sym, 0, false, ty.t_ms)?;
        m.key_ex(kc, sym, 0, true, ty.t_ms + 3)?;
        flush_scores(&mut rig, &mut typed, &mut swallowed_live, &mut totals)?;
    }
    let (lost, doubled, _, _) = totals;
    out.notes.push(format!(
        "K4 app_lost parts: lost or swallowed letters {lost}, escape not consumed {escape_unconsumed}; {switches} switches in {elapsed:.1} s"
    ));
    out.x("switches", switches as f64, "count");
    out.x("typed_keys", typed_total as f64, "count");
    out.x("app_lost", (lost + escape_unconsumed) as f64, "keys");
    out.x("app_doubled", doubled as f64, "keys");
    out.x("chord_leaked", leaked as f64, "chords");
    out.x("wait_timeouts", never_settled as f64, "waits");
    out.x("forged_accepted", forged as f64, "speeches");
    out.x("flap_failed", f64::from(u8::from(!flap_ok)), "runs");
    out.x("restore_ms", restored_ms, "ms");
    out.x(
        "restore_failed",
        f64::from(u8::from(!(after_restart_ok && restored_ms <= 2000.0))),
        "runs",
    );
    out.x("chord_resent", resent as f64, "chords");
    out.x(
        "chord_resent_per_10k",
        resent as f64 * 10_000.0 / switches as f64,
        "chords",
    );
    out.x("k4_wall_s", elapsed, "s");
    Ok(())
}

/// Keys per keycode whose presses and releases do not pair up in the app log: a modifier whose
/// release was eaten (or whose press was) is the signature of changing a modifier grab while the
/// modifier was held, which leaves XKB with a stuck key (P-7).
fn unbalanced_keys(log: &[(u16, u32, bool)]) -> u64 {
    use std::collections::HashMap;
    let mut d: HashMap<u16, i64> = HashMap::new();
    for (kc, _, rel) in log {
        *d.entry(*kc).or_default() += if *rel { -1 } else { 1 };
    }
    d.values().filter(|v| **v != 0).count() as u64
}

/// K4r: the race variant. The same kind of random walk with **no waiting**: after a
/// pass-next-key chord the next chord or letters follow at once, while the emulated manager
/// applies grab calls after a random 0 to 8 ms delay and may reorder concurrent ones. Plain
/// letters are never swallowed in any grab state, so `app_lost` and `app_doubled` stay 0 on
/// Mutter's own verdict. Once all keys are up and the system is quiet, Mutter must hold exactly
/// the grab set the reader's mode asks for (`grab_state_wrong`: a reordered grab call leaves the
/// whole keyboard grabbed or the modifiers missing), nothing may be left held (`held_keys`), and
/// no key may be left down in the emulated XKB state (`xkb_stuck`: a modifier grab changed while the modifier was held eats its release), every key's presses and releases must pair up in the app log (`key_unbalanced`: a modifier
/// grab changed while the modifier was held).
pub fn k4r(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "K4r", out)? {
        return Ok(());
    }
    let rounds = rounds_of(host, size);
    let pass = Chord {
        modifier: INSERT,
        key: F2,
    };
    // Insert+T straight after pass-next-key: the T ends pass mode while Insert is still down.
    let chord_t = Chord {
        modifier: INSERT,
        key: K {
            code: 28,
            sym: 0x74,
        },
    };
    let letters: Vec<u32> = (u32::from(b'a')..=u32::from(b'z')).collect();
    let mut rig = m1_rig(host, reader, flavor)?;
    mutter(&mut rig)?.set_latency(8, seed)?;
    mutter(&mut rig)?.app_log(true)?;
    let mut rng = Rng::new(seed);
    let mut ty = Typist::new();
    let (mut typed, mut swallowed_live, mut wrong_state) = (Vec::new(), 0u64, 0u64);
    let (mut stuck, mut wait_timeouts, mut precondition_dirty) = (0u64, 0u64, 0u64);
    let mut continue_round = true;
    let mut totals = (0u64, 0u64, 0u64, Vec::<u16>::new());
    let mut typed_total = 0usize;
    let started = std::time::Instant::now();
    for _ in 0..rounds {
        if typed.len() >= SCORE_CHUNK {
            typed_total += typed.len();
            flush_scores(&mut rig, &mut typed, &mut swallowed_live, &mut totals)?;
        }
        chord(&mut rig, &mut ty, &pass)?;
        let round_type = rng.below(4);
        match round_type {
            0 => {
                for _ in 0..=rng.below(3) {
                    if type_letter(&mut rig, &mut ty, &mut rng, &mut typed, &letters)? {
                        swallowed_live += 1;
                    }
                }
            }
            1 => {
                chord(&mut rig, &mut ty, &chord_t)?;
            }
            2 => {
                // The grabs are really empty when Insert goes down: it reaches the application,
                // and the T that follows asks for the modifier grabs back while Insert is held.
                // Changing the grab then would eat Insert's release (a stuck key).
                // Start from a quiet state: end the pass mode just entered, let every in-flight
                // grab call land, then enter pass mode again and wait for the empty grab set.
                // Every wait is checked: a timeout is reported as `wait_timeouts` and the round
                // is not judged, so a harness failure is never charged to gorca.
                if type_letter(&mut rig, &mut ty, &mut rng, &mut typed, &letters)? {
                    swallowed_live += 1;
                }
                let quiet = settle_grabs(&mut rig, |g| g.modifiers == NORMAL_MODIFIERS)?;
                std::thread::sleep(Duration::from_millis(30));
                chord(&mut rig, &mut ty, &pass)?;
                let emptied = settle_grabs(&mut rig, |g| g.modifiers == 0)?;
                if !(quiet && emptied) {
                    wait_timeouts += 1;
                    continue_round = false;
                }
                if continue_round {
                    // A plain Insert tap while nothing is grabbed clears any key an earlier,
                    // deliberately racy round left down in the emulated XKB state. The round is
                    // judged only if XKB is clean afterwards.
                    tap(&mut rig, &mut ty, INSERT)?;
                    if mutter(&mut rig)?.grab_state()?.xkb_down > 0 {
                        precondition_dirty += 1;
                        continue_round = false;
                    }
                }
                if continue_round {
                    ty.t_ms += CHORD_GAP_MS;
                    let m = mutter(&mut rig)?;
                    m.key_ex(INSERT.code, INSERT.sym, 0, false, ty.t_ms)?;
                    m.key_ex(28, 0x74, 0, false, ty.t_ms + 5)?;
                    m.key_ex(28, 0x74, 0, true, ty.t_ms + 15)?;
                    // Insert stays down while the reader asks for the modifier grabs back and
                    // Mutter (up to 8 ms of latency) applies them.
                    std::thread::sleep(Duration::from_millis(20));
                    mutter(&mut rig)?.key_ex(INSERT.code, INSERT.sym, 0, true, ty.t_ms + 25)?;
                    ty.t_ms += 25;
                    // Every key is up again: XKB must agree (it heals on later events, so look
                    // now).
                    let (st, landed) = mutter(&mut rig)?
                        .wait_grabs(Duration::from_millis(500), |g| {
                            g.modifiers == NORMAL_MODIFIERS
                        })?;
                    wait_timeouts += u64::from(!landed);
                    std::thread::sleep(Duration::from_millis(5));
                    let st = if landed {
                        mutter(&mut rig)?.grab_state()?
                    } else {
                        st
                    };
                    if landed && st.xkb_down > 0 {
                        eprintln!("K4r DEBUG stuck code {} state {:?}", st.xkb_first, st);
                        stuck += 1;
                    }
                }
                continue_round = true;
            }
            _ => {
                // Insert goes down and stays down while the letters are typed (a chord).
                ty.t_ms += CHORD_GAP_MS;
                let m = mutter(&mut rig)?;
                m.key_ex(INSERT.code, INSERT.sym, 0, false, ty.t_ms)?;
                ty.t_ms += 5;
                m.key_ex(28, 0x74, 0, false, ty.t_ms)?;
                m.key_ex(28, 0x74, 0, true, ty.t_ms + 5)?;
                m.key_ex(INSERT.code, INSERT.sym, 0, true, ty.t_ms + 10)?;
            }
        }
        // Checkpoint: after a plain key the reader's mode is normal again. Let in-flight grab calls finish (latency is at most 8 ms), then
        // Mutter must hold exactly the normal grab set. A reordered pair of calls shows up here.
        if rng.below(4) == 0 {
            // A chord that raced the grab change may have been swallowed, leaving the reader in
            // pass mode with the grabs emptied (legitimate). One plain key ends it.
            if type_letter(&mut rig, &mut ty, &mut rng, &mut typed, &letters)? {
                swallowed_live += 1;
            }
            // Up to four sequential calls with up to 8 ms of latency each can still be in flight:
            // poll for the state instead of guessing a sleep.
            let (st, reached) = mutter(&mut rig)?.wait_grabs(Duration::from_millis(500), |g| {
                g.modifiers == NORMAL_MODIFIERS && !g.grab_all
            })?;
            if !reached {
                wrong_state += 1;
                eprintln!("K4r DEBUG wrong checkpoint after round type {round_type}: {st:?}");
            }
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    // Quiet: every key is up. The reader's mode is back to normal after any key, so settle with
    // one plain letter (it ends a pending pass-next-key) and look at Mutter's state.
    if type_letter(&mut rig, &mut ty, &mut rng, &mut typed, &letters)? {
        swallowed_live += 1;
    }
    let (state, settled) = mutter(&mut rig)?.wait_grabs(Duration::from_secs(3), |g| {
        g.modifiers == NORMAL_MODIFIERS && !g.grab_all
    })?;
    typed_total += typed.len();
    flush_scores(&mut rig, &mut typed, &mut swallowed_live, &mut totals)?;
    let (lost, doubled, unbalanced, _) = totals;
    out.x("rounds", rounds as f64, "count");
    out.x("typed_keys", typed_total as f64, "count");
    out.x("app_lost", lost as f64, "keys");
    out.x("app_doubled", doubled as f64, "keys");
    out.x(
        "grab_state_wrong",
        (wrong_state + u64::from(!settled)) as f64,
        "checkpoints",
    );
    out.x("held_keys", f64::from(state.held), "keys");
    // Judged rounds only: the deliberately racy rounds may leave a key down in the emulated XKB
    // state by construction (a human cannot press a key within milliseconds of a grab change).
    out.x("xkb_stuck", stuck as f64, "keys");
    out.x("xkb_down_at_end", f64::from(state.xkb_down), "keys");
    out.x("wait_timeouts", wait_timeouts as f64, "waits");
    out.x("precondition_dirty", precondition_dirty as f64, "rounds");
    out.x("key_unbalanced", unbalanced as f64, "keys");
    out.x("k4r_wall_s", elapsed, "s");
    Ok(())
}

/// Compare what was typed `(keycode, keysym)` with what the app saw. A key is lost when its
/// press or its release is missing, doubled when either shows up more than once. Entries in the
/// log that were never typed (leaked chord keys) are ignored here (`chord_leaked` covers them).
fn score_typing(typed: &[(u16, u32)], log: &[(u16, u32, bool)]) -> (u64, u64) {
    use std::collections::HashMap;
    let mut seen: HashMap<(u16, bool), u32> = HashMap::new();
    for (kc, _, rel) in log {
        *seen.entry((*kc, *rel)).or_default() += 1;
    }
    let (mut lost, mut doubled) = (0u64, 0u64);
    for (kc, _) in typed {
        for rel in [false, true] {
            match seen.get(&(*kc, rel)).copied().unwrap_or(0) {
                0 => lost += 1,
                1 => {}
                _ => doubled += 1,
            }
        }
    }
    (lost, doubled)
}

const KEY_A: K = K {
    code: 38,
    sym: 0x61,
};
/// What the reader says when it predicts Mutter 48.7's stuck modifier.
const STUCK_HINT: &str = "Insert stuck, tap Insert";

/// K4h: the Mutter 48.7 stuck-modifier bug (docs/reports/live-keys-swallowed.md). Holding a grabbed
/// modifier that autorepeats (Insert) for 0.57 s or more leaves it in `grabbed_keypresses`; from
/// then on Mutter swallows every key until the modifier is tapped once. `fake_mutter.py` emulates
/// the 48.7 autorepeat logic, so the stale state is real here. The reader must know it: it speaks
/// the hint once per episode and never echoes keys the application does not receive; a short hold
/// must stay silent; one tap of Insert ends the episode.
///
/// Metrics (all gated at 0 in every run): `hold_not_stale` (the emulation did not reproduce the
/// bug, so the scenario proved nothing), `stuck_not_swallowed` (Mutter passed a key while stale),
/// `stuck_echoed` (the reader spoke something other than the hint while keys were swallowed),
/// `stuck_hint_wrong` (hint count per episode is not exactly one, or a short hold produced one),
/// `stuck_not_healed` (a key was still swallowed after the tap of Insert).
pub fn k4h(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    _seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "K4h", out)? {
        return Ok(());
    }
    let episodes = size.n.clamp(2, 5);
    let mut rig = m1_rig(host, reader, flavor)?;
    let mut ty = Typist::new();
    std::thread::sleep(Duration::from_millis(500));
    let (mut hold_not_stale, mut not_swallowed, mut echoed) = (0u64, 0u64, 0u64);
    let (mut hint_wrong, mut not_healed) = (0u64, 0u64);
    for round in 0..episodes {
        let say = |rig: &Rig, mark: usize| -> Vec<String> {
            speaks(&rig.mock.since(mark))
                .map(|(_, t)| t.to_owned())
                .collect()
        };
        // A short hold (under the 0.57 s threshold) is no episode and must be silent.
        ty.t_ms += CHORD_GAP_MS;
        let mark = rig.mock.mark();
        let h = mutter(&mut rig)?.hold(INSERT.code, INSERT.sym, ty.t_ms, 450)?;
        ty.t_ms += 450;
        let (_, _) = tap(&mut rig, &mut ty, KEY_A)?;
        std::thread::sleep(Duration::from_millis(150));
        if h.stale || say(&rig, mark).iter().any(|t| t == STUCK_HINT) {
            hint_wrong += 1;
        }
        // The long hold: the episode.
        ty.t_ms += CHORD_GAP_MS;
        let mark = rig.mock.mark();
        let hold_ms = 900 + 100 * round as u64;
        let h = mutter(&mut rig)?.hold(INSERT.code, INSERT.sym, ty.t_ms, hold_ms)?;
        ty.t_ms += hold_ms;
        if !h.stale {
            hold_not_stale += 1;
        }
        std::thread::sleep(Duration::from_millis(150));
        for k in [TAB, KEY_A, TAB] {
            ty.t_ms += 120;
            let (down, up) = tap(&mut rig, &mut ty, k)?;
            if !(down && up) {
                not_swallowed += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(300));
        let said = say(&rig, mark);
        let hints = said.iter().filter(|t| *t == STUCK_HINT).count();
        if hints != 1 {
            hint_wrong += 1;
        }
        echoed += (said.len() - hints) as u64;
        // One tap of Insert heals Mutter; the next key must get through again.
        ty.t_ms += CHORD_GAP_MS;
        tap(&mut rig, &mut ty, INSERT)?;
        ty.t_ms += 120;
        let (down, _) = tap(&mut rig, &mut ty, KEY_A)?;
        if down {
            not_healed += 1;
        }
    }
    out.notes.push(format!(
        "K4h: {episodes} episodes of a 0.9 s+ Insert hold with the 48.7 autorepeat logic emulated"
    ));
    out.x("hold_not_stale", hold_not_stale as f64, "events");
    out.x("stuck_not_swallowed", not_swallowed as f64, "events");
    out.x("stuck_echoed", echoed as f64, "events");
    out.x("stuck_hint_wrong", hint_wrong as f64, "events");
    out.x("stuck_not_healed", not_healed as f64, "events");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::e2e::apps::{KeyReply, parse_app_log};
    use std::path::PathBuf;

    fn host(env: &[(&str, &str)]) -> HostEnv {
        HostEnv {
            harness_dir: PathBuf::new(),
            a11y_addr: String::new(),
            session_addr: String::new(),
            sock_dir: PathBuf::new(),
            reader_env: env
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    #[test]
    fn gates_skip_until_m1_lands() {
        let mut out = RunOut::default();
        assert!(!gate_open(&host(&[]), "K2", &mut out).unwrap());
        assert_eq!(out.scalars[0].0, "skipped");
        assert!(out.notes[0].contains("K2 SKIPPED"));
    }

    #[test]
    fn strict_mode_makes_the_gate_red() {
        let mut out = RunOut::default();
        let e = gate_open(&host(&[("GORCA_M1_STRICT", "1")]), "K4", &mut out).unwrap_err();
        assert!(e.to_string().contains("K4"));
    }

    #[test]
    fn m1_flag_opens_the_gate() {
        let mut out = RunOut::default();
        assert!(gate_open(&host(&[("GORCA_M1", "1")]), "K3", &mut out).unwrap());
        assert!(out.scalars.is_empty());
    }

    #[test]
    fn the_reader_gets_desktop_facts_so_it_never_asks_gsettings() {
        let h = m1_env(&host(&[("GORCA_REPEAT_DELAY_MS", "300")]));
        let get = |k: &str| {
            h.reader_env
                .iter()
                .find(|(e, _)| e == k)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(
            get("GORCA_REPEAT_DELAY_MS").as_deref(),
            Some("300"),
            "explicit wins"
        );
        assert_eq!(get("GORCA_XKB_OPTIONS").as_deref(), Some(""));
        assert_eq!(get("GORCA_CLOCK_24H").as_deref(), Some("1"));
    }

    /// The 16-bit synthetic keycodes wrap during a long walk. Scored in one piece, a wrapped
    /// keycode looks doubled (this is what m1-r6 saw at 30k switches); scored per chunk it does
    /// not, and a real duplicate inside a chunk is still found.
    #[test]
    fn wrapped_keycodes_are_not_doubles_but_real_doubles_are() {
        let key = |kc: u16| [(kc, 97, false), (kc, 97, true)];
        // Keycode 300 typed in chunk one and again in chunk two (after the wrap).
        let (chunk1, chunk2) = ([(300, 97)], [(300, 97)]);
        let log1: Vec<_> = key(300).to_vec();
        let log2: Vec<_> = key(300).to_vec();
        let whole: Vec<_> = [log1.clone(), log2.clone()].concat();
        assert_eq!(
            score_typing(&[chunk1[0], chunk2[0]], &whole),
            (0, 4),
            "one piece: false doubles"
        );
        assert_eq!(score_typing(&chunk1, &log1), (0, 0));
        assert_eq!(score_typing(&chunk2, &log2), (0, 0));
        // A real double inside a chunk.
        let real: Vec<_> = [log1.clone(), key(300).to_vec()].concat();
        assert_eq!(score_typing(&chunk1, &real), (0, 2));
    }

    /// A duplicate that lands after a chunk was scored shows up in the next log under a keycode
    /// that chunk did not type; the carried codes catch it.
    #[test]
    fn a_double_that_straddles_a_chunk_boundary_is_found() {
        let carry = [300u16, 301];
        let next_log = [
            (500, 97, false),
            (500, 97, true),
            (301, 98, false),
            (301, 98, true),
        ];
        assert_eq!(straddling_doubles(&next_log, &carry), 2);
        assert_eq!(straddling_doubles(&next_log[..2], &carry), 0);
        assert_eq!(straddling_doubles(&[], &carry), 0);
    }

    #[test]
    fn unpaired_presses_and_releases_are_counted() {
        let log = [(10, 1, false), (10, 1, true), (11, 2, false), (12, 3, true)];
        assert_eq!(unbalanced_keys(&log), 2);
        assert_eq!(unbalanced_keys(&[]), 0);
    }

    #[test]
    fn the_process_environment_never_opens_a_gate() {
        // Whatever the test runner exports, a gate opens only through --reader-env.
        let mut out = RunOut::default();
        assert!(!gate_open(&host(&[]), "K2", &mut out).unwrap() || flag(&host(&[]), "GORCA_M1"));
        assert!(!flag(&host(&[]), "GORCA_M1"));
    }

    #[test]
    fn default_chords_use_real_keycodes() {
        // XKB keycode = evdev + 8: Insert 110, F12 88, Tab 15, F2 60, 1 is 2, Escape 1.
        assert_eq!(
            [
                INSERT.code,
                F12.code,
                TAB.code,
                F2.code,
                KEY_1.code,
                ESCAPE.code
            ],
            [118, 96, 23, 68, 10, 9]
        );
    }

    #[test]
    fn key_reply_and_app_log_parse() {
        let r = KeyReply::parse(
            r#"{"t_emit_ns":123,"sent":1,"notified":1,"consumed":true,"first_click":false,"passed":false,"event":"reply","id":4,"ok":true}"#,
        );
        assert_eq!(
            r,
            KeyReply {
                t_emit_ns: 123,
                notified: 1,
                consumed: true
            }
        );
        let l = r#"{"keys":[[38,97,false],[38,97,true],[40,98,false]],"event":"reply","id":9,"ok":true}"#;
        assert_eq!(
            parse_app_log(l),
            vec![(38, 97, false), (38, 97, true), (40, 98, false)]
        );
        assert!(parse_app_log(r#"{"keys":[],"event":"reply"}"#).is_empty());
    }

    #[test]
    fn typing_score_detects_lost_and_doubled() {
        let typed = [(10, 97), (11, 98), (12, 99)];
        let log = [
            (10, 97, false),
            (10, 97, true),
            (11, 98, false),
            (11, 98, false), // doubled press
            (11, 98, true),
            (12, 99, false),  // release lost
            (99, 120, false), // never typed: ignored
        ];
        assert_eq!(score_typing(&typed, &log), (1, 1));
    }
}
