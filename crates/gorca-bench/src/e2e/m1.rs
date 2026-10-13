//! M1 end-to-end gates K2, K3, K4 (docs/plan/phase-2.md 2.1, ADR-0001 7.3 K-1/K-2/K-6, M-16).
//!
//! These are harness skeletons: the stimulus side is complete and drives `fake_mutter.py` with
//! the real grab semantics (exact-mask matching, a11y-modifier first-click rule, consumed vs
//! passed keys), but the reader side (gorca modifier, command registry, `SetKeyGrabs`) does not
//! exist until M1 lands. Until then every scenario is **skipped**: it reports `skipped = 1` and a
//! note, and does not start a rig. Two switches change that:
//!
//! * `GORCA_M1=1` (as `--reader-env GORCA_M1=1` or in the process environment): M1 has landed,
//!   run the scenario and enforce its gate.
//! * `GORCA_M1_STRICT=1`: M1 has not landed and the gate must be **red**: the scenario fails
//!   with an error instead of skipping (use it to prove the gate is wired and can fail).
//!
//! | id | stimulus | metrics |
//! |---|---|---|
//! | K2 | gorca modifier + command key (zero-IPC command), idle | `k2_ms` command-key emit to SPEAK, `lost`, `not_consumed` |
//! | K3 | the same chord while an L3-50k storm runs | `k3_ms`, `lost`, `not_consumed`, storm rate |
//! | K4 | 10k randomized mode toggles (chords) interleaved with plain typing | `app_lost`, `app_doubled`, `chord_leaked` |
//!
//! The chord keys are placeholders until ADR-0005 fixes the modifier and keymap (phase-2.md
//! section 8 Q1/Q2). Override with `--reader-env` (or the process environment):
//! `GORCA_M1_MODIFIER` (keysym, default Insert 0xff63), `GORCA_M1_CMD_KEYSYM` (default `t`),
//! `GORCA_M1_MODE_KEYSYM` (default `m`, the key that switches mode / grab set in K4). The values
//! are read by the harness only; the reader must be configured with the same keys.
//!
//! Event times are virtual (`time_ms` of the emulated key event) so Mutter's first-click rule
//! is deterministic: every chord starts more than the repeat delay (500 ms) after the previous
//! modifier release, so the modifier press is always a fresh first click (swallowed).

use std::time::Duration;

use anyhow::{Result, bail};
use gorca_bench_b2::clock::Rng;

use super::apps::ReaderSpec;
use super::mock::Flavor;
use super::runner::{HostEnv, Rig};
use super::scenarios::{RunOut, Size, base_opts, ms, pause, speaks, start_storm};

const INSERT: u32 = 0xff63;
const KEY_T: u32 = 0x74;
const KEY_M: u32 = 0x6d;
/// Virtual gap between chords: above Mutter's default repeat delay (500 ms).
const CHORD_GAP_MS: u64 = 700;

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

fn keysym(host: &HostEnv, key: &str, default: u32) -> u32 {
    setting(host, key)
        .and_then(|v| {
            v.strip_prefix("0x")
                .map_or_else(|| v.parse().ok(), |h| u32::from_str_radix(h, 16).ok())
        })
        .unwrap_or(default)
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
        "{id} SKIPPED: M1 (command layer, SetKeyGrabs) has not landed; set GORCA_M1=1 to run it"
    ));
    out.x("skipped", 1.0, "flag");
    Ok(false)
}

struct Chord {
    modifier: u32,
    key: u32,
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
            next_keycode: 10,
        }
    }

    fn keycode(&mut self) -> u16 {
        let k = self.next_keycode;
        self.next_keycode = if k >= 60_000 { 10 } else { k + 1 };
        k
    }
}

/// One chord: modifier down, key down, key up, modifier up. Returns the key-down emit time and
/// whether Mutter consumed the key-down.
fn chord(rig: &mut Rig, ty: &mut Typist, c: &Chord) -> Result<(u64, bool)> {
    ty.t_ms += CHORD_GAP_MS;
    let (mkc, ckc) = (ty.keycode(), ty.keycode());
    let m = rig
        .mutter
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("no mutter"))?;
    m.key_ex(mkc, c.modifier, 0, false, ty.t_ms)?;
    let down = m.key_ex(ckc, c.key, 0, false, ty.t_ms + 5)?;
    m.key_ex(ckc, c.key, 0, true, ty.t_ms + 15)?;
    m.key_ex(mkc, c.modifier, 0, true, ty.t_ms + 25)?;
    ty.t_ms += 25;
    Ok((down.t_emit_ns, down.consumed))
}

fn m1_rig(host: &HostEnv, reader: &ReaderSpec, flavor: Flavor) -> Result<Rig> {
    let mut o = base_opts(reader, flavor);
    o.keys = true;
    let rig = Rig::start(host, &o)?;
    rig.mock
        .configure(Duration::from_millis(1), Duration::from_millis(1));
    if !rig.keys_ok {
        bail!("the reader did not register with the emulated keyboard monitor (no SetKeyGrabs)");
    }
    Ok(rig)
}

/// Run `n` chords, score command-key emit to the matching SPEAK by order.
fn chord_series(
    rig: &mut Rig,
    out: &mut RunOut,
    metric: &str,
    c: &Chord,
    n: usize,
    seed: u64,
) -> Result<()> {
    let mut rng = Rng::new(seed);
    let mut ty = Typist::new();
    let mark = rig.mock.mark();
    let mut emits = Vec::with_capacity(n);
    let mut not_consumed = 0u64;
    for _ in 0..n {
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
    // The first SPEAK in [emit_i, emit_i+1) belongs to chord i (other speech in the window, for
    // example an echo of the modifier, is tolerated); none in the window means lost.
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

fn command_chord(host: &HostEnv) -> Chord {
    Chord {
        modifier: keysym(host, "GORCA_M1_MODIFIER", INSERT),
        key: keysym(host, "GORCA_M1_CMD_KEYSYM", KEY_T),
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
    chord_series(&mut rig, out, "k2_ms", &command_chord(host), size.n, seed)
}

/// K3: the same modifier chord while a 50k events/s storm runs on the a11y bus. Gate p99 15 ms
/// (target 5 ms), `lost = 0`; the modifier must keep being swallowed under the storm.
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
    const RATE: f64 = 50_000.0;
    let mut rig = m1_rig(host, reader, flavor)?;
    let series_s = size.n as f64 * 0.012 + 1.0;
    let storm = start_storm(&rig.env.a11y_addr, RATE, series_s + 3.0)?;
    std::thread::sleep(Duration::from_millis(1000));
    let r = chord_series(&mut rig, out, "k3_ms", &command_chord(host), size.n, seed);
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

/// K4: no lost or doubled keys across randomized mode / grab-set switches (ADR-0001 M-16).
///
/// Plain letters (state 0, never the modifier, command or mode key) are typed between mode
/// toggles; in no mode may the reader swallow them (`app_lost`) and the app must see each
/// exactly once (`app_doubled`). The toggle chord itself must always be consumed
/// (`chord_leaked` counts chords whose key reached the app). Ground truth is Mutter's own
/// verdict (`consumed`) and the app log of `fake_mutter.py`, never the reader's speech.
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
    let mode = Chord {
        modifier: keysym(host, "GORCA_M1_MODIFIER", INSERT),
        key: keysym(host, "GORCA_M1_MODE_KEYSYM", KEY_M),
    };
    let reserved = [mode.modifier, mode.key, command_chord(host).key];
    let letters: Vec<u32> = (u32::from(b'a')..=u32::from(b'z'))
        .filter(|l| !reserved.contains(l))
        .collect();
    let mut rig = m1_rig(host, reader, flavor)?;
    rig.mutter
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("no mutter"))?
        .app_log(true)?;
    let mut rng = Rng::new(seed);
    let mut ty = Typist::new();
    let (mut typed, mut leaked, mut swallowed_live) = (Vec::new(), 0u64, 0u64);
    for _ in 0..switches {
        let (_, consumed) = chord(&mut rig, &mut ty, &mode)?;
        if !consumed {
            leaked += 1;
        }
        for _ in 0..=rng.below(4) {
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
        }
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
    out.x("app_lost", (lost + swallowed_live) as f64, "keys");
    out.x("app_doubled", doubled as f64, "keys");
    out.x("chord_leaked", leaked as f64, "chords");
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
    fn keysym_overrides_parse_hex_and_decimal() {
        assert_eq!(keysym(&host(&[]), "K", 7), 7);
        assert_eq!(keysym(&host(&[("K", "0xff63")]), "K", 7), 0xff63);
        assert_eq!(keysym(&host(&[("K", "116")]), "K", 7), 116);
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
