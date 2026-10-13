//! M1 end-to-end gates K2, K3, K4 (docs/plan/phase-2.md 2.1, ADR-0005 10) plus K2s.
//!
//! The stimulus drives `fake_mutter.py`, which ports Mutter's grab semantics (exact-mask matching,
//! the a11y-modifier first-click rule, consumed vs passed keys). Chords are the ADR-0005 default
//! keymap (NVDA desktop preset) with the real XKB keycodes the reader matches physically (K-8).
//! Until M1 is switched on every scenario is **skipped** (`skipped = 1`); two switches change that:
//!
//! * `GORCA_M1=1` (as `--reader-env GORCA_M1=1` or in the process environment): run the scenario
//!   and enforce its gate.
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
struct K {
    code: u16,
    sym: u32,
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
/// Real time for the reader to restore the grab set after a switch (P-7 settle plus a margin).
const SETTLE_REAL_MS: u64 = 40;
/// First node of the K3 series (warm-up uses 1..70).
const FIRST_NODE: usize = 100;

fn setting(host: &HostEnv, key: &str) -> Option<String> {
    host.reader_env
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
        .or_else(|| std::env::var(key).ok())
}

fn flag(host: &HostEnv, key: &str) -> bool {
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

struct Chord {
    modifier: K,
    key: K,
}

/// Virtual clock plus keycode allocator of one scenario.
struct Typist {
    t_ms: u64,
    next_keycode: u16,
}

impl Typist {
    fn new() -> Self {
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
fn chord(rig: &mut Rig, ty: &mut Typist, c: &Chord) -> Result<(u64, bool)> {
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
fn m1_env(host: &HostEnv) -> HostEnv {
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

fn m1_rig(host: &HostEnv, reader: &ReaderSpec, flavor: Flavor) -> Result<Rig> {
    let mut o = base_opts(reader, flavor);
    o.keys = true;
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
enum Before {
    Nothing,
    /// Move focus to a fresh node and press at once: the answer needs a fetch (cache cold).
    FocusFresh,
}

/// Run `n` chords, score key-down emit to the first SPEAK after it (the answer; the speech of
/// the previous action is cancelled by the epoch of the key).
fn chord_series(
    rig: &mut Rig,
    out: &mut RunOut,
    metric: &str,
    c: &Chord,
    n: usize,
    seed: u64,
    before: Before,
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
    let recs = rig.mock.since(mark);
    let spoken: Vec<u64> = speaks(&recs).map(|(r, _)| r.t_ns).collect();
    let mut lat = Vec::with_capacity(n);
    let mut lost = 0u64;
    // The first SPEAK in [emit_i, emit_i+1) belongs to chord i; none in the window means lost.
    for (i, t) in emits.iter().enumerate() {
        let next = emits.get(i + 1).copied().unwrap_or(u64::MAX);
        match spoken.iter().find(|s| **s >= *t && **s < next) {
            Some(s) => lat.push(ms(s - *t)),
            None => lost += 1,
        }
    }
    out.notes.push(format!(
        "{metric}: first SPEAK between a command key and the next one ({} spoken for {n} chords)",
        spoken.len()
    ));
    out.s(metric, lat);
    out.x("lost", lost as f64, "events");
    out.x("not_consumed", not_consumed as f64, "events");
    Ok(())
}

/// `info.time_date`: zero IPC, always speaks.
fn time_chord() -> Chord {
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
    chord_series(&mut rig, out, "k3_ms", &c, size.n, seed, Before::FocusFresh)
}

/// Run the forger: a rogue client addressing `KeyEvent` signals to the reader. Returns how many
/// SPEAKs they caused (must be 0).
fn forged_keys(rig: &mut Rig) -> Result<u64> {
    let script = rig.env.harness_dir.join("key_forger.py");
    let mark = rig.mock.mark();
    // The system python has the D-Bus bindings (the project venv does not), like fake_mutter.py.
    let out = std::process::Command::new("/usr/bin/python3")
        .arg("-I")
        .arg(&script)
        .arg(&rig.env.session_addr)
        .arg("5")
        .output()?;
    if !out.status.success() {
        bail!(
            "key_forger.py failed: {} {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    std::thread::sleep(Duration::from_millis(300));
    Ok(speaks(&rig.mock.since(mark)).count() as u64)
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

/// K4: no lost or doubled keys across randomized grab-set switches (ADR-0001 M-16).
///
/// Each round switches the grab set with a chord (`input.pass_next_key`, which empties it and
/// restores it on the next key, or `input.learn_mode_toggle` followed by Escape, which grabs
/// everything and releases it), then types plain letters. Letters are typed only while the
/// grabs are restored, after [`SETTLE_REAL_MS`]; in no round may the reader swallow a letter
/// (`app_lost`), and the app must see each exactly once (`app_doubled`). A chord must be
/// consumed whenever it is sent in the normal mode (`chord_leaked` counts those that reached the
/// app). Ground truth is Mutter's own verdict (`consumed`) and the app log of `fake_mutter.py`,
/// never the reader's speech. A forged `KeyEvent` from another client must do nothing
/// (`forged_accepted`).
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
    let switches: usize = if size.n >= 1000 { 10_000 } else { 500 };
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
    rig.mutter
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("no mutter"))?
        .app_log(true)?;
    let mut forged = forged_keys(&mut rig)?;
    // The shell restarts: the manager comes back under a new unique name. The reader must
    // re-authorise and restore its grabs within 2 s, accept the new owner, and still ignore
    // forgeries (ADR-0005 8.2).
    let restored_ms = restart_mutter(&mut rig)?;
    forged += forged_keys(&mut rig)?;
    let mut ty0 = Typist::new();
    let before = rig.mock.mark();
    let (_, consumed) = chord(&mut rig, &mut ty0, &time_chord())?;
    std::thread::sleep(Duration::from_millis(300));
    let after_restart_ok = consumed && speaks(&rig.mock.since(before)).count() > 0;
    rig.mutter
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("no mutter"))?
        .app_log(true)?;
    let mut rng = Rng::new(seed);
    let mut ty = Typist::new();
    let (mut typed, mut leaked, mut swallowed_live) = (Vec::new(), 0u64, 0u64);
    let mut escape_unconsumed = 0u64;
    for _ in 0..switches {
        let use_learn = rng.below(4) == 0;
        if use_learn {
            let (_, consumed) = chord(&mut rig, &mut ty, &learn)?;
            if !consumed {
                leaked += 1;
            }
            std::thread::sleep(Duration::from_millis(SETTLE_REAL_MS));
            // Learn mode holds the keyboard: Escape is delivered to the reader, not the app.
            let (d, u) = tap(&mut rig, &mut ty, ESCAPE)?;
            if !(d && u) {
                escape_unconsumed += 1;
            }
        } else {
            let (_, consumed) = chord(&mut rig, &mut ty, &pass)?;
            if !consumed {
                leaked += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(SETTLE_REAL_MS));
        for _ in 0..=rng.below(3) {
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
            if down.consumed || up.consumed {
                swallowed_live += 1;
            }
            typed.push((kc, sym));
            if !use_learn {
                // The first letter ended pass-next-key; give the restore time before the next.
                std::thread::sleep(Duration::from_millis(SETTLE_REAL_MS));
            }
        }
        std::thread::sleep(Duration::from_millis(SETTLE_REAL_MS));
    }
    std::thread::sleep(Duration::from_millis(200));
    let log = rig
        .mutter
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("no mutter"))?
        .app_log(true)?;
    let (lost, doubled) = score_typing(&typed, &log);
    out.x("switches", switches as f64, "count");
    out.x("typed_keys", typed.len() as f64, "count");
    out.notes.push(format!(
        "K4 app_lost parts: missing in app log {lost}, swallowed live {swallowed_live}, escape not consumed {escape_unconsumed}"
    ));
    out.x(
        "app_lost",
        (lost + swallowed_live + escape_unconsumed) as f64,
        "keys",
    );
    out.x("app_doubled", doubled as f64, "keys");
    out.x("chord_leaked", leaked as f64, "chords");
    out.x("forged_accepted", forged as f64, "speeches");
    out.x("restore_ms", restored_ms, "ms");
    out.x(
        "restore_failed",
        f64::from(u8::from(!(after_restart_ok && restored_ms <= 2000.0))),
        "runs",
    );
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
