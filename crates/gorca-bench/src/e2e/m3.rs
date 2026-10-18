// SPDX-License-Identifier: GPL-3.0-or-later
//! M3 end-to-end gates (docs/adr/0007 16): caret echo, typing echo, selection, say-all and
//! password handling, through the real connector and the real keyboard path.
//!
//! The stimulus is `fake_mutter.py` (the reader sees `KeyEvent`s exactly as Mutter would send
//! them) and the `text` scenario of `fakeapp.py` (a GTK-like entry, text view and password
//! entry). The fake app plays the toolkit: after a key press the harness tells it to move the
//! caret, insert or delete, and it emits `text-caret-moved` / `text-changed` / selection
//! signals the way GTK does and answers `GetText`, `GetStringAtOffset` and `GetSelection`.
//! Latencies are CLOCK_MONOTONIC differences taken inside the stimulus processes.
//!
//! | id | stimulus | metrics |
//! |---|---|---|
//! | C1 | Down in a text view (toolkit line, one fetch per key) | `c1_ms` app event emit to SPEAK (gate p99 15 ms), `lost`, `c1_extra_fetches` |
//! | C1h | Right in a long entry (window hit, no IPC) | `c1h_ms` (gate p99 3 ms), `lost`, `c1h_ipc_calls` (gate 0) |
//! | C1hs, E1s | C1h and E1 under the L3-50k storm | `c1hs_ms`, `e1s_ms` (gate 4 ms) |
//! | C2 | Ctrl+Right held at 30 Hz for 3 s, slow speech | `stale_not_cancelled` (gate 0), `lost` (the last position is always spoken) |
//! | C3 | Ctrl+Right/Left through a Cyrillic sentence ending in punctuation, GTK landings (word end going right, word start going left) | `c3_wrong` (gate 0) |
//! | E1 | printable keys | `e1_ms` key emit to SPEAK (gate p99 3 ms), `lost` |
//! | E2 | randomised typing session (Latin, Cyrillic, BackSpace, paste, input-method commit) | `e2_missing`, `e2_double`, `e2_keyname`, `e2_wrong` (all gate 0) |
//! | E2f | the first typed key after the focus reached an entry, 0-300 ms after the focus speech | `e2f_missing`, `e2f_double` (gate 0) |
//! | S2 | Shift+Right (selection delta, one fetch) | `s2_ms` (gate p99 15 ms), `lost` |
//! | SA1 | `text.say_all` chord | `sa1_ms` key emit to first SPEAK (gate p99 15 ms) |
//! | SA2 | say-all over 100 KB | `sa2_ahead_ratio` (gate 0.99: the next chunk reaches the server within 5 ms of the previous END), `sa2_gap_ms` |
//! | SA3 | a key press during say-all | `sa3_stop_ms` key emit to the speech cancel (gate p99 2 ms), `sa3_caret_ms` key emit to the caret being placed (gate p99 50 ms) |
//! | PW | typing and moving in a password field with the presentation log on | `pw_plaintext_in_logs` (gate 0), `pw_echo_missing` (gate 0), `pw_log_lines` |
//!
//! Skipped unless `--reader-env GORCA_M3=1`; `GORCA_M3_STRICT=1` fails instead of skipping.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use gorca_bench_b2::clock::Rng;

use super::apps::{FakeApp, FakeMutter, ReaderSpec, json_num};
use super::m1::{K, Typist, flag, m1_rig};
use super::mock::{Flavor, Kind, Rec};
use super::runner::{HostEnv, Rig};
use super::scenarios::{RunOut, Size, ms, pause, speaks, start_storm};

const LEFT: K = K {
    code: 113,
    sym: 0xff51,
};
const RIGHT: K = K {
    code: 114,
    sym: 0xff53,
};
const DOWN: K = K {
    code: 116,
    sym: 0xff54,
};
const BACKSPACE: K = K {
    code: 22,
    sym: 0xff08,
};
const SPACE: K = K {
    code: 65,
    sym: 0x20,
};
const ESCAPE: K = K {
    code: 9,
    sym: 0xff1b,
};
const CTRL_L: K = K {
    code: 37,
    sym: 0xffe3,
};
const SHIFT_L: K = K {
    code: 50,
    sym: 0xffe1,
};
const KEY_V: K = K {
    code: 55,
    sym: 0x76,
};
const INSERT: K = K {
    code: 118,
    sym: 0xff63,
};
/// Modifier state bits of a Mutter key event.
const SHIFT: u32 = 1;
const CONTROL: u32 = 4;

/// SA2: the next chunk reaching the server this soon after the previous one ended is no gap.
const SA2_GAP_OK_NS: i64 = 5_000_000;

/// Node ids of the `text` scenario of fakeapp.py.
const NAME: usize = 2;
const NOTES: usize = 3;
const SECRET: usize = 4;
const LONG: usize = 5;
/// Characters per line of the notes text (`Line 0000 text\n`).
const LINE_LEN: usize = 15;
/// Lines of the notes text.
const LINES: usize = 1500;

fn gate_open(host: &HostEnv, id: &str, out: &mut RunOut) -> Result<bool> {
    if flag(host, "GORCA_M3") {
        return Ok(true);
    }
    if flag(host, "GORCA_M3_STRICT") {
        bail!("{id}: GORCA_M3_STRICT=1 requires the M3 gates to run");
    }
    out.notes.push(format!(
        "{id} SKIPPED: set GORCA_M3=1 to run the M3 text gates"
    ));
    out.x("skipped", 1.0, "flag");
    Ok(false)
}

/// The text of `LONG`: letters cycling a..z.
fn long_char(i: usize) -> char {
    char::from(b'a' + u8::try_from(i % 26).unwrap_or(0))
}

fn json_esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o
}

struct Setup {
    rig: Rig,
    app: FakeApp,
    ty: Typist,
}

/// A reader under test with the text app, speech utterances of `utter_ms`.
fn setup(host: &HostEnv, reader: &ReaderSpec, flavor: Flavor, utter_ms: u64) -> Result<Setup> {
    let rig = m1_rig(host, reader, flavor)?;
    rig.mock
        .configure(Duration::from_millis(1), Duration::from_millis(utter_ms));
    let app = FakeApp::spawn(&rig.env.harness_dir, "text", "textapp", 0, &[])?;
    Ok(Setup {
        rig,
        app,
        ty: Typist::new(),
    })
}

/// Move the focus to `node` and wait until the reader speaks for it and has had time to fetch
/// the text window (the focus event is only emitted when focus really moves).
fn focus_node(s: &mut Setup, node: usize) -> Result<()> {
    focus_node_settled(s, node, 300)
}

/// [`focus_node`] with a chosen settle time after the first focus speech (0 = type at once).
fn focus_node_settled(s: &mut Setup, node: usize, settle_ms: u64) -> Result<()> {
    let other = if node == NAME { NOTES } else { NAME };
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        if Instant::now() > end {
            bail!("the reader did not speak for the focus of node {node} within 10 s");
        }
        s.app.focus(other)?;
        std::thread::sleep(Duration::from_millis(40));
        let mark = s.rig.mock.mark();
        s.app.focus(node)?;
        if s.rig
            .mock
            .wait_for(mark, Duration::from_millis(400), |r| {
                matches!(r.kind, Kind::Speak { .. })
            })
            .is_some()
        {
            break;
        }
    }
    std::thread::sleep(Duration::from_millis(settle_ms));
    Ok(())
}

fn load_text(s: &mut Setup, node: usize, text: &str, caret: usize) -> Result<()> {
    s.app.proc.request(
        "load_text",
        &format!(
            "\"node\":{node},\"text\":\"{}\",\"caret\":{caret}",
            json_esc(text)
        ),
        Duration::from_secs(10),
    )?;
    Ok(())
}

fn mutter(rig: &mut Rig) -> Result<&mut FakeMutter> {
    rig.mutter.as_mut().context("no mutter")
}

/// Key down; returns (emit time, consumed).
fn key_down(rig: &mut Rig, ty: &mut Typist, k: K, uni: u32, state: u32) -> Result<(u64, bool)> {
    ty.t_ms += 3;
    let r = mutter(rig)?.key_full(k.code, k.sym, uni, state, false, ty.t_ms)?;
    Ok((r.t_emit_ns, r.consumed))
}

fn key_up(rig: &mut Rig, ty: &mut Typist, k: K, state: u32) -> Result<()> {
    ty.t_ms += 3;
    mutter(rig)?.key_full(k.code, k.sym, 0, state, true, ty.t_ms)?;
    Ok(())
}

/// Press and release a key; returns the emit time of the press.
fn tap(rig: &mut Rig, ty: &mut Typist, k: K, uni: u32, state: u32) -> Result<u64> {
    let (t, _) = key_down(rig, ty, k, uni, state)?;
    key_up(rig, ty, k, state)?;
    Ok(t)
}

/// A command to the fake app that emits signals; returns the emit time of the first.
fn app_ev(app: &mut FakeApp, cmd: &str, fields: &str) -> Result<u64> {
    let r = app.proc.request(cmd, fields, Duration::from_secs(5))?;
    Ok(json_num(&r, "t_emit_ns").unwrap_or(0.0) as u64)
}

/// Calls the fake app served since the last reset: total, and of the Text interface.
fn app_calls(app: &mut FakeApp, reset: bool) -> Result<(u64, u64)> {
    let r = app.proc.request(
        "calls",
        if reset { "\"reset\":true" } else { "" },
        Duration::from_secs(5),
    )?;
    let total = json_num(&r, "total").unwrap_or(0.0) as u64;
    let mut text = 0u64;
    let mut rest = r.as_str();
    while let Some(i) = rest.find("\"Text.") {
        rest = &rest[i + 1..];
        if let Some(j) = rest.find("\":") {
            let n: String = rest[j + 2..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            text += n.parse::<u64>().unwrap_or(0);
        }
    }
    Ok((total, text))
}

/// One expected speech: the key that caused it, the signal that completes it, the text.
pub(super) struct Probe {
    pub(super) t_key: u64,
    pub(super) t_ev: u64,
    pub(super) expect: String,
}

fn texts(recs: &[Rec]) -> Vec<(u64, String)> {
    speaks(recs).map(|(r, t)| (r.t_ns, t.to_owned())).collect()
}

/// Credit each probe with the SPEAK in `[t_key_i, t_key_{i+1})` that has the expected text.
/// Returns (latencies from `from_event` or the key, lost, doubled).
pub(super) fn score(
    probes: &[Probe],
    spoken: &[(u64, String)],
    from_event: bool,
) -> (Vec<f64>, u64, u64) {
    let (mut lat, mut lost, mut doubled) = (Vec::new(), 0u64, 0u64);
    for (i, p) in probes.iter().enumerate() {
        let end = probes.get(i + 1).map_or(u64::MAX, |n| n.t_key);
        let hits: Vec<&(u64, String)> = spoken
            .iter()
            .filter(|(t, s)| *t >= p.t_key && *t < end && *s == p.expect)
            .collect();
        match hits.first() {
            Some((t, _)) => {
                let t0 = if from_event { p.t_ev } else { p.t_key };
                lat.push(ms(t.saturating_sub(t0)));
                if hits.len() > 1 {
                    doubled += 1;
                }
            }
            None => lost += 1,
        }
    }
    (lat, lost, doubled)
}

/// What was heard around the first few probes that were not spoken as expected: the evidence a
/// failed gate needs (the expected text, and every SPEAK from the key to the key after next).
pub(super) fn lost_notes(id: &str, probes: &[Probe], spoken: &[(u64, String)]) -> Vec<String> {
    let mut notes = Vec::new();
    for (i, p) in probes.iter().enumerate() {
        let end = probes.get(i + 1).map_or(u64::MAX, |n| n.t_key);
        if spoken
            .iter()
            .any(|(t, s)| *t >= p.t_key && *t < end && *s == p.expect)
        {
            continue;
        }
        let far = probes.get(i + 2).map_or(u64::MAX, |n| n.t_key);
        let heard: Vec<String> = spoken
            .iter()
            .filter(|(t, _)| *t >= p.t_key && *t < far)
            .map(|(t, s)| format!("{:+.1}ms {s:?}", (*t as f64 - p.t_key as f64) / 1e6))
            .collect();
        notes.push(format!(
            "{id} lost #{i}: wanted {:?}, key to next key {:.1} ms, heard [{}]",
            p.expect,
            probes
                .get(i + 1)
                .map_or(0.0, |n| (n.t_key as f64 - p.t_key as f64) / 1e6),
            heard.join(", ")
        ));
        if notes.len() >= 5 {
            break;
        }
    }
    notes
}

/// Every speech-server record (SPEAK, CANCEL, END) around the first lost probe, relative to that
/// probe's key, plus each neighbouring probe's emit time. A lost character is either never
/// presented or presented and cancelled before it was sent; this tells the two apart.
pub(super) fn lost_context(id: &str, probes: &[Probe], recs: &[Rec]) -> Option<String> {
    let spoken: Vec<(u64, String)> = texts(recs);
    let first = probes.iter().enumerate().find_map(|(i, p)| {
        let end = probes.get(i + 1).map_or(u64::MAX, |n| n.t_key);
        let hit = spoken
            .iter()
            .any(|(t, s)| *t >= p.t_key && *t < end && *s == p.expect);
        (!hit).then_some(i)
    })?;
    let t0 = probes[first].t_key;
    let rel = |t: u64| (t as f64 - t0 as f64) / 1e6;
    let lo = first.saturating_sub(2);
    let hi = (first + 8).min(probes.len());
    let mut events: Vec<(u64, String)> = Vec::new();
    for (i, p) in probes.iter().enumerate().take(hi).skip(lo) {
        events.push((p.t_key, format!("key#{i}")));
        events.push((p.t_ev, format!("emit#{i}({})", p.expect)));
    }
    let (t_lo, t_hi) = (probes[lo].t_key, probes[hi - 1].t_key + 40_000_000);
    for r in recs.iter().filter(|r| r.t_ns >= t_lo && r.t_ns <= t_hi) {
        let what = match &r.kind {
            Kind::Connected => "connected".to_owned(),
            Kind::Speak { text, .. } => format!("SPEAK({text})"),
            Kind::Cancel { verb, had_speech } => format!("CANCEL({verb},had={had_speech})"),
            Kind::End => "END".to_owned(),
        };
        events.push((r.t_ns, what));
    }
    events.sort_by_key(|e| e.0);
    let line: Vec<String> = events
        .iter()
        .map(|(t, w)| format!("{:+.2}ms {w}", rel(*t)))
        .collect();
    Some(format!(
        "{id} timeline around first lost #{first}: {}",
        line.join(" | ")
    ))
}

fn n_of(size: Size, cap: usize) -> usize {
    size.n.min(cap)
}

/// C1: Down in a multi-line text view. Each key moves the caret to a new line the Reader has
/// not seen: one `GetStringAtOffset` per key. Gate p99 15 ms (target 5 ms).
pub fn c1(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "C1", out)? {
        return Ok(());
    }
    let mut s = setup(host, reader, flavor, 1)?;
    focus_node(&mut s, NOTES)?;
    let n = n_of(size, LINES - 2);
    let mut rng = Rng::new(seed);
    app_calls(&mut s.app, true)?;
    let mark = s.rig.mock.mark();
    let mut probes = Vec::with_capacity(n);
    for i in 0..n {
        let line = i + 1;
        let t_key = tap(&mut s.rig, &mut s.ty, DOWN, 0, 0)?;
        let t_ev = app_ev(
            &mut s.app,
            "caret",
            &format!("\"node\":{NOTES},\"offset\":{}", line * LINE_LEN),
        )?;
        probes.push(Probe {
            t_key,
            t_ev,
            expect: format!("Line {line:04} text"),
        });
        pause(&mut rng, 8, 16);
    }
    std::thread::sleep(Duration::from_millis(300));
    let (_, text_calls) = app_calls(&mut s.app, false)?;
    let spoken = texts(&s.rig.mock.since(mark));
    let (lat, lost, _) = score(&probes, &spoken, true);
    out.notes.extend(lost_notes("C1", &probes, &spoken));
    out.s("c1_ms", lat);
    out.x("lost", lost as f64, "events");
    out.x(
        "c1_fetches_per_key",
        text_calls as f64 / n.max(1) as f64,
        "calls",
    );
    out.notes
        .push("C1: app caret-event emit to SPEAK of the new line (toolkit line, one fetch)".into());
    Ok(())
}

/// C1h: Right in a long single-line entry; the window answers, no application call at all.
/// Gate p99 3 ms (target 1 ms), 0 IPC.
pub fn c1h(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    c1h_run(host, reader, flavor, size, seed, out, false)
}

/// C1hs: C1h while the L3-50k storm runs on another application (ADR-0007 16: p99 at most
/// 1 ms above the idle gate).
pub fn c1hs(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    c1h_run(host, reader, flavor, size, seed, out, true)
}

/// The storm of the `s` variants: 50k events/s from another application for `secs`.
fn storm_for(s: &Setup, n: usize) -> Result<super::scenarios::Storm> {
    let st = start_storm(&s.rig.env.a11y_addr, 50_000.0, n as f64 * 0.02 + 3.0)?;
    std::thread::sleep(Duration::from_millis(1000));
    Ok(st)
}

fn c1h_run(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
    storm: bool,
) -> Result<()> {
    if !gate_open(host, if storm { "C1hs" } else { "C1h" }, out)? {
        return Ok(());
    }
    let mut s = setup(host, reader, flavor, 1)?;
    focus_node(&mut s, LONG)?;
    let n = n_of(size, 1400);
    let _storm = if storm { Some(storm_for(&s, n)?) } else { None };
    let mut rng = Rng::new(seed);
    app_calls(&mut s.app, true)?;
    let mark = s.rig.mock.mark();
    let mut probes = Vec::with_capacity(n);
    for i in 0..n {
        let t_key = tap(&mut s.rig, &mut s.ty, RIGHT, 0, 0)?;
        let t_ev = app_ev(
            &mut s.app,
            "caret",
            &format!("\"node\":{LONG},\"offset\":{}", i + 1),
        )?;
        probes.push(Probe {
            t_key,
            t_ev,
            expect: long_char(i + 1).to_string(),
        });
        pause(&mut rng, 8, 16);
    }
    std::thread::sleep(Duration::from_millis(300));
    let (total, _) = app_calls(&mut s.app, false)?;
    let recs = s.rig.mock.since(mark);
    let spoken = texts(&recs);
    let (lat, lost, _) = score(&probes, &spoken, true);
    out.notes.extend(lost_notes("C1h", &probes, &spoken));
    if lost > 0 {
        out.notes.extend(lost_context("C1h", &probes, &recs));
    }
    out.s(if storm { "c1hs_ms" } else { "c1h_ms" }, lat);
    out.x("lost", lost as f64, "events");
    out.x("c1h_ipc_calls", total as f64, "calls");
    out.notes
        .push("C1h: app caret-event emit to SPEAK of the character, from the window".into());
    Ok(())
}

/// C2: Ctrl held with Right autorepeating at 30 Hz for 3 s over words; speech is slow, so every
/// utterance but the last is cancelled by the next key. Nothing stale may reach the server and
/// the last position must be spoken.
pub fn c2(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    _seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "C2", out)? {
        return Ok(());
    }
    let mut s = setup(host, reader, flavor, 400)?;
    let words = (0..300).fold(String::new(), |mut s, i| {
        let _ = write!(s, "w{i:03} ");
        s
    });
    load_text(&mut s, NAME, &words, 0)?;
    focus_node(&mut s, NAME)?;
    let n = if size.n >= 1000 { 90 } else { 30 };
    let mark = s.rig.mock.mark();
    key_down(&mut s.rig, &mut s.ty, CTRL_L, 0, 0)?;
    let mut emits = Vec::with_capacity(n);
    for i in 0..n {
        let (t_key, _) = key_down(&mut s.rig, &mut s.ty, RIGHT, 0, CONTROL)?;
        app_ev(
            &mut s.app,
            "caret",
            &format!("\"node\":{NAME},\"offset\":{}", 5 * (i + 1)),
        )?;
        emits.push(t_key);
        std::thread::sleep(Duration::from_millis(33));
    }
    key_up(&mut s.rig, &mut s.ty, RIGHT, CONTROL)?;
    key_up(&mut s.rig, &mut s.ty, CTRL_L, 0)?;
    std::thread::sleep(Duration::from_millis(900));
    let spoken = texts(&s.rig.mock.since(mark));
    // Word i (0-based) is what key i speaks: "w{i+1:03}".
    let mut stale = 0u64;
    let mut last_spoken = false;
    for (t, text) in &spoken {
        let Some(k) = text
            .strip_prefix('w')
            .and_then(|d| d.parse::<usize>().ok())
            .and_then(|w| w.checked_sub(1))
        else {
            continue;
        };
        if k + 1 == n {
            last_spoken = true;
        }
        if emits.get(k + 1).is_some_and(|next| *t >= *next) {
            stale += 1;
        }
    }
    out.x("stale_not_cancelled", stale as f64, "utterances");
    out.x("lost", if last_spoken { 0.0 } else { 1.0 }, "events");
    out.x(
        "c2_spoken",
        spoken.iter().filter(|(_, t)| t.starts_with('w')).count() as f64,
        "utterances",
    );
    out.notes.push(format!(
        "C2: {n} autorepeated word moves at 30 Hz; {} reached the server, {stale} after a newer key",
        spoken.len()
    ));
    Ok(())
}

/// C3: Ctrl+Right and Ctrl+Left through a Cyrillic sentence that ends in punctuation, with the
/// caret landing the way GTK lands it: at the END of each word going right (so the last stop
/// is the end of the text) and at the START going left. Every move must speak the word just
/// reached, including the last one ("?" behind "твои") and the first when coming back.
pub fn c3(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    _size: Size,
    _seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "C3", out)? {
        return Ok(());
    }
    let mut s = setup(host, reader, flavor, 20)?;
    load_text(&mut s, NAME, "Привет, как дела твои?", 0)?;
    focus_node(&mut s, NAME)?;
    let right: [(usize, &str); 5] = [
        (6, "Привет"),
        (11, "как"),
        (16, "дела"),
        (21, "твои"),
        (22, "question"),
    ];
    let left: [(usize, &str); 4] = [(17, "твои"), (12, "дела"), (8, "как"), (0, "Привет")];
    let mut wrong = 0u64;
    key_down(&mut s.rig, &mut s.ty, CTRL_L, 0, 0)?;
    for (key, stops) in [(RIGHT, &right[..]), (LEFT, &left[..])] {
        for (off, want) in stops {
            let mark = s.rig.mock.mark();
            key_down(&mut s.rig, &mut s.ty, key, 0, CONTROL)?;
            app_ev(
                &mut s.app,
                "caret",
                &format!("\"node\":{NAME},\"offset\":{off}"),
            )?;
            key_up(&mut s.rig, &mut s.ty, key, CONTROL)?;
            std::thread::sleep(Duration::from_millis(250));
            let spoken = texts(&s.rig.mock.since(mark));
            if !spoken.iter().any(|(_, t)| t == want) {
                wrong += 1;
                out.notes.push(format!(
                    "C3: offset {off} should speak {want:?}, the server got {:?}",
                    spoken.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>()
                ));
            }
        }
    }
    key_up(&mut s.rig, &mut s.ty, CTRL_L, 0)?;
    out.x("c3_wrong", wrong as f64, "moves");
    Ok(())
}

/// A letter for typing: Latin or Cyrillic.
fn letter(rng: &mut Rng) -> char {
    if rng.below(4) == 0 {
        char::from_u32(0x430 + rng.below(32) as u32).unwrap_or('а')
    } else {
        char::from(b'a' + rng.below(26) as u8)
    }
}

/// Physical key of a letter for the synthetic keyboard: any letter key will do, the reader reads
/// the unichar.
fn letter_key(c: char) -> K {
    let code = 38 + u16::try_from(c as u32 % 9).unwrap_or(0);
    K {
        code,
        sym: 0x0100_0000 + c as u32,
    }
}

/// E1: printable keys in an entry; the app answers each with the insert and caret events.
/// Gate p99 3 ms (target 1 ms).
pub fn e1(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    e1_run(host, reader, flavor, size, seed, out, false)
}

/// E1s: E1 while the L3-50k storm runs on another application.
pub fn e1s(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    e1_run(host, reader, flavor, size, seed, out, true)
}

fn e1_run(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
    storm: bool,
) -> Result<()> {
    if !gate_open(host, if storm { "E1s" } else { "E1" }, out)? {
        return Ok(());
    }
    let mut s = setup(host, reader, flavor, 1)?;
    load_text(&mut s, NAME, "", 0)?;
    focus_node(&mut s, NAME)?;
    let n = n_of(size, 1200);
    let _storm = if storm { Some(storm_for(&s, n)?) } else { None };
    let mut rng = Rng::new(seed);
    let mark = s.rig.mock.mark();
    let mut probes = Vec::with_capacity(n);
    for _ in 0..n {
        let c = letter(&mut rng);
        let k = letter_key(c);
        let t_key = tap(&mut s.rig, &mut s.ty, k, c as u32, 0)?;
        let t_ev = app_ev(
            &mut s.app,
            "type",
            &format!("\"node\":{NAME},\"text\":\"{c}\""),
        )?;
        probes.push(Probe {
            t_key,
            t_ev,
            expect: c.to_string(),
        });
        pause(&mut rng, 8, 16);
    }
    std::thread::sleep(Duration::from_millis(300));
    let spoken = texts(&s.rig.mock.since(mark));
    let (lat, lost, doubled) = score(&probes, &spoken, false);
    out.notes.extend(lost_notes("E1", &probes, &spoken));
    out.s(if storm { "e1s_ms" } else { "e1_ms" }, lat);
    out.x("lost", lost as f64, "events");
    out.x("e2_double", doubled as f64, "events");
    Ok(())
}

/// What an E2 action expects to hear.
struct Action {
    t_key: u64,
    expect: String,
}

/// E2: a randomised typing session. Every typed key is echoed exactly once; BackSpace speaks
/// the deleted character and never its own name; a paste and an input-method commit are spoken
/// once from the text.
pub fn e2(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "E2", out)? {
        return Ok(());
    }
    let mut s = setup(host, reader, flavor, 1)?;
    load_text(&mut s, NAME, "", 0)?;
    focus_node(&mut s, NAME)?;
    let n = if size.n >= 1000 { 10_000 } else { 1_000 };
    let mut rng = Rng::new(seed);
    let mark = s.rig.mock.mark();
    let mut model: Vec<char> = Vec::new();
    let (mut typed_keys, mut erase_keys, mut paste_keys, mut ime_keys) = (0u64, 0u64, 0u64, 0u64);
    let mut actions: Vec<Action> = Vec::with_capacity(n);
    let words = ["pasted words", "alpha beta", "gamma", "delta epsilon zeta"];
    for _ in 0..n {
        let roll = rng.below(1000);
        if roll < 700 || (roll < 880 && model.is_empty()) {
            let c = letter(&mut rng);
            let t = tap(&mut s.rig, &mut s.ty, letter_key(c), c as u32, 0)?;
            app_ev(
                &mut s.app,
                "type",
                &format!("\"node\":{NAME},\"text\":\"{c}\""),
            )?;
            model.push(c);
            typed_keys += 1;
            actions.push(Action {
                t_key: t,
                expect: c.to_string(),
            });
        } else if roll < 800 {
            let t = tap(&mut s.rig, &mut s.ty, SPACE, 0x20, 0)?;
            app_ev(
                &mut s.app,
                "type",
                &format!("\"node\":{NAME},\"text\":\" \""),
            )?;
            model.push(' ');
            typed_keys += 1;
            actions.push(Action {
                t_key: t,
                expect: "space".into(),
            });
        } else if roll < 970 {
            let gone = model.pop().unwrap_or('x');
            erase_keys += 1;
            let t = tap(&mut s.rig, &mut s.ty, BACKSPACE, 0, 0)?;
            app_ev(&mut s.app, "backspace", &format!("\"node\":{NAME}"))?;
            actions.push(Action {
                t_key: t,
                expect: if gone == ' ' {
                    "space".into()
                } else {
                    gone.to_string()
                },
            });
        } else if roll < 995 {
            // Paste: Ctrl+V reaches the app, which inserts the clipboard.
            let w = words[rng.below(words.len() as u64) as usize];
            key_down(&mut s.rig, &mut s.ty, CTRL_L, 0, 0)?;
            let t = tap(&mut s.rig, &mut s.ty, KEY_V, 0x76, CONTROL)?;
            key_up(&mut s.rig, &mut s.ty, CTRL_L, 0)?;
            app_ev(
                &mut s.app,
                "insert_text",
                &format!("\"node\":{NAME},\"text\":\"{w}\""),
            )?;
            model.extend(w.chars());
            paste_keys += 1;
            actions.push(Action {
                t_key: t,
                expect: format!("pasted, {w}"),
            });
            // The paste echo is built from the insert and its read-back, so it lands about one key gap
            // after the key. A newer key inside that gap makes it stale (E-2), which is right and
            // not a missing echo (ADR-0007 TI-3): pace like a person after a paste, as after a commit.
            pause(&mut rng, 60, 80);
        } else {
            // An input-method commit: text arrives with no key. Spaced out like a person
            // composing: the reader speaks unexplained inserts at most every 250 ms (EV-5).
            std::thread::sleep(Duration::from_millis(320));
            let t = app_ev(
                &mut s.app,
                "insert_text",
                &format!("\"node\":{NAME},\"text\":\"ключ\""),
            )?;
            model.extend("ключ".chars());
            ime_keys += 1;
            actions.push(Action {
                t_key: t,
                expect: "ключ".into(),
            });
            std::thread::sleep(Duration::from_millis(320));
        }
        // 30-50 keys/s, several times a fast typist: a stall longer than the gap lets the speech lane
        // keep only the newer of two echoes (E-1), which is right and not a missing echo.
        pause(&mut rng, 20, 30);
    }
    std::thread::sleep(Duration::from_millis(500));
    let spoken = texts(&s.rig.mock.since(mark));
    let (mut missing, mut double, mut keyname, mut wrong) = (0u64, 0u64, 0u64, 0u64);
    let mut notes: Vec<String> = Vec::new();
    for (i, a) in actions.iter().enumerate() {
        let end = actions.get(i + 1).map_or(u64::MAX, |x| x.t_key);
        let heard: Vec<&str> = spoken
            .iter()
            .filter(|(t, _)| *t >= a.t_key && *t < end)
            .map(|(_, s)| s.as_str())
            .collect();
        keyname += heard
            .iter()
            .filter(|s| matches!(**s, "backspace" | "delete"))
            .count() as u64;
        if notes.len() < 8 && (!heard.contains(&a.expect.as_str()) || heard.len() > 1) {
            let far = actions.get(i + 2).map_or(u64::MAX, |x| x.t_key);
            let around: Vec<String> = spoken
                .iter()
                .filter(|(t, _)| *t >= a.t_key && *t < far)
                .map(|(t, s)| format!("{:+.1}ms {s:?}", (*t as f64 - a.t_key as f64) / 1e6))
                .collect();
            notes.push(format!(
                "E2 action #{i}: wanted {:?}, key to next key {:.1} ms, heard [{}]",
                a.expect,
                (end.min(far) as f64 - a.t_key as f64) / 1e6,
                around.join(", ")
            ));
        }
        match heard.as_slice() {
            [] => missing += 1,
            [one] if *one == a.expect => {}
            [one] if matches!(*one, "backspace" | "delete") => {}
            [_] => wrong += 1,
            more if more.iter().filter(|s| **s == a.expect).count() == 1 => {
                double += (more.len() - 1) as u64;
            }
            more if more.contains(&a.expect.as_str()) => double += 1,
            _ => wrong += 1,
        }
    }
    out.notes.extend(notes);
    out.notes.push(format!(
        "E2 sent {typed_keys} typed keys ({erase_keys} BackSpace, {paste_keys} paste, {ime_keys} keyless commits)"
    ));
    out.x("e2_missing", missing as f64, "events");
    out.x("e2_double", double as f64, "events");
    out.x("e2_keyname", keyname as f64, "events");
    out.x("e2_wrong", wrong as f64, "events");
    out.notes
        .push(format!("E2: {n} actions, {} speeches heard", spoken.len()));
    Ok(())
}

/// E2f: the first key typed after the focus reached an entry is echoed, whatever the time since
/// the focus speech (0, 10, 30, 100, 300 ms): text tracking arms only after the focus
/// presentation, and the generic key echo covers the gap, without a double (ADR-0007 TI-3). Gates 0.
pub fn e2f(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "E2f", out)? {
        return Ok(());
    }
    let mut s = setup(host, reader, flavor, 1)?;
    load_text(&mut s, NAME, "", 0)?;
    let settles = [0u64, 10, 30, 100, 300];
    let trials = (n_of(size, 400) / settles.len()).max(1);
    let mut rng = Rng::new(seed);
    let mut missing = [0u64; 5];
    let mut doubled = 0u64;
    let mut notes: Vec<String> = Vec::new();
    for _ in 0..trials {
        for (si, settle) in settles.iter().enumerate() {
            focus_node_settled(&mut s, NAME, *settle)?;
            let c = char::from(b'a' + rng.below(26) as u8);
            let mark = s.rig.mock.mark();
            tap(&mut s.rig, &mut s.ty, letter_key(c), c as u32, 0)?;
            app_ev(
                &mut s.app,
                "type",
                &format!("\"node\":{NAME},\"text\":\"{c}\""),
            )?;
            std::thread::sleep(Duration::from_millis(150));
            let heard = texts(&s.rig.mock.since(mark));
            let times = heard.iter().filter(|(_, t)| *t == c.to_string()).count();
            if times > 1 {
                doubled += 1;
            }
            if times == 0 {
                missing[si] += 1;
                if notes.len() < 8 {
                    notes.push(format!(
                        "E2f: key {c:?} {settle} ms after the focus speech, heard {:?}",
                        heard.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>()
                    ));
                }
            }
        }
    }
    out.notes.extend(notes);
    for (si, settle) in settles.iter().enumerate() {
        out.notes.push(format!(
            "E2f: {} of {trials} first keys missed at {settle} ms",
            missing[si]
        ));
    }
    out.x("e2f_missing", missing.iter().sum::<u64>() as f64, "events");
    out.x("e2f_double", doubled as f64, "events");
    Ok(())
}

/// S2: Shift+Right in a long entry; the app signals the caret, then the selection (GTK4
/// order). The Reader fetches the selection and speaks only what changed.
pub fn s2(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "S2", out)? {
        return Ok(());
    }
    let mut s = setup(host, reader, flavor, 1)?;
    focus_node(&mut s, LONG)?;
    let n = n_of(size, 1400);
    let mut rng = Rng::new(seed);
    key_down(&mut s.rig, &mut s.ty, SHIFT_L, 0, 0)?;
    let mark = s.rig.mock.mark();
    let mut probes = Vec::with_capacity(n);
    for i in 0..n {
        let (t_key, _) = key_down(&mut s.rig, &mut s.ty, RIGHT, 0, SHIFT)?;
        key_up(&mut s.rig, &mut s.ty, RIGHT, SHIFT)?;
        let t_ev = app_ev(
            &mut s.app,
            "caret",
            &format!(
                "\"node\":{LONG},\"offset\":{},\"sel\":[[0,{}]]",
                i + 1,
                i + 1
            ),
        )?;
        probes.push(Probe {
            t_key,
            t_ev,
            expect: format!("{} selected", long_char(i)),
        });
        // 40-80 keys/s: a scheduling stall of the fake processes longer than the gap lets the ingress
        // fold two caret events into one (R-1), which is right and not a lost echo.
        pause(&mut rng, 12, 24);
    }
    key_up(&mut s.rig, &mut s.ty, SHIFT_L, 0)?;
    std::thread::sleep(Duration::from_millis(300));
    let spoken = texts(&s.rig.mock.since(mark));
    let (lat, lost, _) = score(&probes, &spoken, true);
    out.notes.extend(lost_notes("S2", &probes, &spoken));
    out.s("s2_ms", lat);
    out.x("lost", lost as f64, "events");
    Ok(())
}

/// The `text.say_all` chord of the NVDA desktop preset: gorca (Insert) + Down.
fn say_all_chord(s: &mut Setup) -> Result<u64> {
    s.ty.t_ms += 700;
    let t = s.ty.t_ms;
    let m = mutter(&mut s.rig)?;
    m.key_ex(INSERT.code, INSERT.sym, 0, false, t)?;
    let down = m.key_ex(DOWN.code, DOWN.sym, 0, false, t + 5)?;
    m.key_ex(DOWN.code, DOWN.sym, 0, true, t + 15)?;
    m.key_ex(INSERT.code, INSERT.sym, 0, true, t + 25)?;
    s.ty.t_ms += 25;
    if !down.consumed {
        bail!("the say-all chord (gorca+Down) was not grabbed: is text.say_all bound?");
    }
    Ok(down.t_emit_ns)
}

/// SA1: `text.say_all` from the start of the notes: key emit to the first SPEAK (one fetch).
pub fn sa1(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "SA1", out)? {
        return Ok(());
    }
    let mut s = setup(host, reader, flavor, 30)?;
    focus_node(&mut s, NOTES)?;
    let trials = size.trials.min(200);
    let mut rng = Rng::new(seed);
    let (mut lat, mut lost) = (Vec::new(), 0u64);
    for _ in 0..trials {
        // Back to the top: the user did it, so the Reader speaks it; then quiet.
        app_ev(
            &mut s.app,
            "caret",
            &format!("\"node\":{NOTES},\"offset\":0"),
        )?;
        std::thread::sleep(Duration::from_millis(60));
        let mark = s.rig.mock.mark();
        let t = say_all_chord(&mut s)?;
        match s.rig.mock.wait_for(
            mark,
            Duration::from_secs(2),
            |r| matches!(&r.kind, Kind::Speak { text, .. } if text == "Line 0000 text"),
        ) {
            Some(r) => lat.push(ms(r.t_ns.saturating_sub(t))),
            None => lost += 1,
        }
        // Any key ends the reading; give the Reader time to put the caret back.
        tap(&mut s.rig, &mut s.ty, ESCAPE, 0, 0)?;
        std::thread::sleep(Duration::from_millis(120));
        pause(&mut rng, 4, 12);
    }
    out.s("sa1_ms", lat);
    out.x("lost", lost as f64, "events");
    Ok(())
}

/// SA2: say-all over 100 KB. The next chunk must be at the server before the previous one ends.
pub fn sa2(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    _seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "SA2", out)? {
        return Ok(());
    }
    const UTTER_MS: u64 = 10;
    let mut s = setup(host, reader, flavor, UTTER_MS)?;
    let sentences = if size.n >= 1000 { 2500 } else { 500 };
    let text = (0..sentences).fold(String::new(), |mut s, i| {
        let _ = write!(s, "Sentence number {i:04} of the long document. ");
        s
    });
    load_text(&mut s, LONG, &text, 0)?;
    focus_node(&mut s, LONG)?;
    let mark = s.rig.mock.mark();
    say_all_chord(&mut s)?;
    let want = sentences;
    let end = Instant::now() + Duration::from_secs(120);
    let mut recs: Vec<Rec>;
    loop {
        std::thread::sleep(Duration::from_millis(250));
        recs = s.rig.mock.since(mark);
        let n = speaks(&recs)
            .filter(|(_, t)| t.starts_with("Sentence number"))
            .count();
        if n >= want || Instant::now() > end {
            break;
        }
    }
    let sp: Vec<u64> = speaks(&recs)
        .filter(|(_, t)| t.starts_with("Sentence number"))
        .map(|(r, _)| r.t_ns)
        .collect();
    let ends: Vec<u64> = recs
        .iter()
        .filter_map(|r| matches!(r.kind, Kind::End).then_some(r.t_ns))
        .collect();
    let (mut ok, mut transitions) = (0u64, 0u64);
    // The voicego client holds one utterance at the server (`max_in_flight = 1`, TBD-V), so
    // "submitted before the previous one ended" shows here as the next SPEAK following the END
    // within a few milliseconds; the Reader's own look-ahead is unit-tested.
    let mut gaps = Vec::with_capacity(sp.len());
    for k in 0..sp.len().saturating_sub(1).min(ends.len()) {
        transitions += 1;
        let gap = sp[k + 1] as i64 - ends[k] as i64;
        gaps.push(gap.max(0) as f64 / 1e6);
        if gap <= SA2_GAP_OK_NS {
            ok += 1;
        }
    }
    out.s("sa2_gap_ms", gaps);
    out.x(
        "sa2_ahead_ratio",
        if transitions == 0 {
            0.0
        } else {
            ok as f64 / transitions as f64
        },
        "ratio",
    );
    out.x("sa2_chunks", sp.len() as f64, "chunks");
    out.x("lost", (want.saturating_sub(sp.len())) as f64, "chunks");
    Ok(())
}

/// SA3: a key press during say-all stops it at once and puts the caret where it stood.
pub fn sa3(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "SA3", out)? {
        return Ok(());
    }
    let mut s = setup(host, reader, flavor, 400)?;
    focus_node(&mut s, NOTES)?;
    let trials = if size.n >= 1000 { 40 } else { 10 };
    let mut rng = Rng::new(seed);
    let mut stop = Vec::new();
    let (mut wrong, mut not_silenced) = (0u64, 0u64);
    for _ in 0..trials {
        app_ev(
            &mut s.app,
            "caret",
            &format!("\"node\":{NOTES},\"offset\":0"),
        )?;
        std::thread::sleep(Duration::from_millis(80));
        let mark = s.rig.mock.mark();
        say_all_chord(&mut s)?;
        // Let a few chunks go by (400 ms each) so the position is somewhere in the text.
        let first = s.rig.mock.wait_for(mark, Duration::from_secs(2), |r| {
            matches!(r.kind, Kind::Speak { .. })
        });
        if first.is_none() {
            not_silenced += 1;
            continue;
        }
        std::thread::sleep(Duration::from_millis(1000 + rng.below(600)));
        let before = json_num(
            &s.app.proc.request(
                "get_caret",
                &format!("\"node\":{NOTES}"),
                Duration::from_secs(5),
            )?,
            "sets",
        )
        .unwrap_or(0.0);
        let mark2 = s.rig.mock.mark();
        let t = tap(&mut s.rig, &mut s.ty, ESCAPE, 0, 0)?;
        match s.rig.mock.wait_for(mark2, Duration::from_millis(500), |r| {
            matches!(r.kind, Kind::Cancel { .. })
        }) {
            Some(r) => stop.push(ms(r.t_ns.saturating_sub(t))),
            None => not_silenced += 1,
        }
        // The caret was put at the start of the chunk being spoken as that chunk started, so
        // nothing is left to move: gorca must not touch it once the key has arrived.
        std::thread::sleep(Duration::from_millis(150));
        let r = s.app.proc.request(
            "get_caret",
            &format!("\"node\":{NOTES}"),
            Duration::from_secs(5),
        )?;
        let caret = json_num(&r, "caret").unwrap_or(-1.0);
        let sets = json_num(&r, "sets").unwrap_or(0.0);
        if sets > before
            || caret < 0.0
            || !(caret as usize).is_multiple_of(LINE_LEN)
            || (caret as usize) == 0
        {
            wrong += 1;
        }
    }
    out.s("sa3_stop_ms", stop);
    out.x("sa3_caret_wrong", wrong as f64, "events");
    out.x("sa3_not_silenced", not_silenced as f64, "events");
    Ok(())
}

/// SA3a: stopping say-all with an arrow key. The caret follows the reading, so the application
/// handles the key from where the reading is: the line the user hears is the one after the
/// chunk being spoken, and gorca never moves the caret back afterwards (review M1).
pub fn sa3a(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "SA3a", out)? {
        return Ok(());
    }
    // Long chunks (1.5 s each): the caret does not move between our read and the key.
    let mut s = setup(host, reader, flavor, 1500)?;
    focus_node(&mut s, NOTES)?;
    let trials = if size.n >= 1000 { 20 } else { 6 };
    let mut rng = Rng::new(seed);
    let (mut lat, mut wrong, mut moved_back) = (Vec::new(), 0u64, 0u64);
    let get = |s: &mut Setup| -> Result<(usize, f64)> {
        let r = s.app.proc.request(
            "get_caret",
            &format!("\"node\":{NOTES}"),
            Duration::from_secs(5),
        )?;
        Ok((
            json_num(&r, "caret").unwrap_or(-1.0).max(0.0) as usize,
            json_num(&r, "sets").unwrap_or(0.0),
        ))
    };
    for _ in 0..trials {
        app_ev(
            &mut s.app,
            "caret",
            &format!("\"node\":{NOTES},\"offset\":0"),
        )?;
        std::thread::sleep(Duration::from_millis(80));
        let mark = s.rig.mock.mark();
        say_all_chord(&mut s)?;
        if s.rig
            .mock
            .wait_for(mark, Duration::from_secs(2), |r| {
                matches!(r.kind, Kind::Speak { .. })
            })
            .is_none()
        {
            wrong += 1;
            continue;
        }
        // Into the second or third chunk, away from its edges.
        std::thread::sleep(Duration::from_millis(2200 + rng.below(400)));
        let (reading_at, sets) = get(&mut s)?;
        let mark2 = s.rig.mock.mark();
        let t_key = tap(&mut s.rig, &mut s.ty, DOWN, 0, 0)?;
        // The application handles Down from where the caret is now.
        let (app_caret, _) = get(&mut s)?;
        let target = app_caret + LINE_LEN;
        let t_ev = app_ev(
            &mut s.app,
            "caret",
            &format!("\"node\":{NOTES},\"offset\":{target}"),
        )?;
        let want = format!("Line {:04} text", target / LINE_LEN);
        match s.rig.mock.wait_for(
            mark2,
            Duration::from_millis(500),
            |r| matches!(&r.kind, Kind::Speak { text, .. } if *text == want),
        ) {
            Some(r) => lat.push(ms(r.t_ns.saturating_sub(t_ev.max(t_key)))),
            None => wrong += 1,
        }
        if app_caret != reading_at {
            // The caret was not where the reading was.
            wrong += 1;
        }
        std::thread::sleep(Duration::from_millis(400));
        let (end_caret, end_sets) = get(&mut s)?;
        if end_caret != target || end_sets > sets {
            moved_back += 1;
        }
    }
    out.s("sa3a_ms", lat);
    out.x("sa3a_wrong", wrong as f64, "events");
    out.x("sa3a_caret_moved_back", moved_back as f64, "events");
    Ok(())
}

/// PW: a scripted session in a password entry with the presentation log and trace logging on.
/// The user hears everything; no log holds the typed string.
pub fn pw(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    _size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "PW", out)? {
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("gorca-h.pw{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let log = dir.join("present.jsonl");
    let err = dir.join("reader.stderr");
    let mut h = host.clone();
    h.reader_env.retain(|(k, _)| k != "RUST_LOG");
    h.reader_env
        .push(("RUST_LOG".to_owned(), "trace".to_owned()));
    let mut o = super::scenarios::base_opts(reader, flavor);
    o.keys = true;
    o.extra_reader_args = vec![format!("--bench-present-log={}", log.display())];
    o.reader_stderr = Some(err.clone());
    for (k, v) in [
        ("GORCA_REPEAT_DELAY_MS", "500"),
        ("GORCA_XKB_OPTIONS", ""),
        ("GORCA_CLOCK_24H", "1"),
    ] {
        if !h.reader_env.iter().any(|(e, _)| e == k) {
            h.reader_env.push((k.to_owned(), v.to_owned()));
        }
    }
    let rig = Rig::start(&h, &o)?;
    rig.mock
        .configure(Duration::from_millis(1), Duration::from_millis(1));
    let app = FakeApp::spawn(&rig.env.harness_dir, "text", "textapp", 0, &[])?;
    let mut s = Setup {
        rig,
        app,
        ty: Typist::new(),
    };
    focus_node(&mut s, SECRET)?;
    let secret = "Zq7-tr0ub4dor&3";
    let mut rng = Rng::new(seed);
    let mark = s.rig.mock.mark();
    let mut probes = Vec::new();
    for c in secret.chars() {
        let k = letter_key(c);
        let t_key = tap(&mut s.rig, &mut s.ty, k, c as u32, 0)?;
        let t_ev = app_ev(
            &mut s.app,
            "type",
            &format!(
                "\"node\":{SECRET},\"text\":\"{}\"",
                json_esc(&c.to_string())
            ),
        )?;
        probes.push(Probe {
            t_key,
            t_ev,
            expect: match c {
                '-' => "dash".to_owned(),
                '&' => "and".to_owned(),
                c => c.to_string(),
            },
        });
        pause(&mut rng, 20, 30);
    }
    // Move around, select all, read the line: all of it must stay out of the logs.
    let n = secret.chars().count();
    for i in 0..4 {
        tap(&mut s.rig, &mut s.ty, LEFT, 0, 0)?;
        app_ev(
            &mut s.app,
            "caret",
            &format!("\"node\":{SECRET},\"offset\":{}", n - 1 - i),
        )?;
        std::thread::sleep(Duration::from_millis(40));
    }
    app_ev(&mut s.app, "select_all", &format!("\"node\":{SECRET}"))?;
    std::thread::sleep(Duration::from_millis(500));
    let spoken = texts(&s.rig.mock.since(mark));
    let (_, lost, _) = score(&probes, &spoken, false);
    out.x("pw_echo_missing", lost as f64, "events");
    // Give the log thread its moment, then grep.
    std::thread::sleep(Duration::from_millis(500));
    let read = |p: &std::path::Path| std::fs::read_to_string(p).unwrap_or_default();
    let (present, stderr) = (read(&log), read(&err));
    let mut hits = 0u64;
    for hay in [&present, &stderr] {
        hits += hay.matches(secret).count() as u64;
        hits += hay.matches(&secret[..8]).count() as u64;
        hits += hay.matches(&secret[7..]).count() as u64;
    }
    out.x("pw_plaintext_in_logs", hits as f64, "occurrences");
    out.x("pw_log_lines", present.lines().count() as f64, "lines");
    out.x("pw_stderr_bytes", stderr.len() as f64, "bytes");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
