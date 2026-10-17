//! M6 end-to-end gates (docs/plan/phase-2.md 2.1 V1-V3, milestone M6): a terminal, through the
//! real connector, the real keyboard path and the `terminal` scenario of `fakeapp.py`.
//!
//! The fake application plays VTE: after every frame it rebuilds the visible screen, emits one
//! `text-changed` delete and insert covering what differs by common head and tail, then
//! `text-caret-moved`, and answers `GetText`, with `CharacterCount` that disagrees with the text
//! and the text right-trimmed (`--term-trim`). A negative offset to `GetText` is counted as the
//! `g_assert` that would abort a real `gnome-terminal-server` (`vte_assert`, gate 0 everywhere).
//! The harness plays the shell: after a key it writes what bash would write to the pty
//! (`term_feed`), escape sequences included.
//!
//! | id | stimulus | metrics |
//! |---|---|---|
//! | V1 | a command's output and the next prompt, one VTE frame | `v1_ms` last frame emit to SPEAK (settle 30 ms + gorca), `v1_over_ms` = `v1_ms` minus the settle, `lost` |
//! | VT | randomised typing session (letters, digits, space, BackSpace, Left, Right, Enter) | `vt_missing`, `vt_double`, `vt_extra` (all gate 0), `vt_ms` key to SPEAK |
//! | VH | scripted readline session: typing, Tab, arrows, word and line motions, Enter, history | `vh_failures` (gate 0) |
//! | V2 | `cat` of a huge file, `yes` and a `make -j` log flooding the focused terminal | `v2_*_cpu_pct` (gate 60), `v2_siocinq_p99_kib` (gate 1024), `v2_*_speaks`, `v2_key_ms`, `v2_alive` |
//! | V3 | Ctrl+C during a flood | `v3_stop_ms` key emit to the speech cancel (gate p99 2 ms), `v3_prompt_missing`, `v3_stale` (gate 0) |
//! | VP | a `sudo` password prompt, a password typed, with the presentation log and `RUST_LOG=trace` | `vp_prompt_missing`, `vp_echo_missing`, `vp_plaintext_in_logs` (gate 0) |
//! | VR | numpad review of the screen: lines, words, characters; the screen changes under the cursor | `vr_ms` (gate p99 3 ms), `vr_failures`, `vr_ipc_calls` (gate 0) |
//! | VF | full-screen programs: a pager page, a cursor row moved, a repaint, an edit in the middle | `vf_failures` (gate 0) |
//! | VB | a second, unfocused terminal floods while the first is typed in (ADR-0001 T-4) | `vb_key_ms` (gate p99 4 ms), `vb_stray_speaks` (gate 0) |
//!
//! Skipped unless `--reader-env GORCA_M6=1`; `GORCA_M6_STRICT=1` fails instead of skipping.
//! `GORCA_V2_SECS=<n>` sets the length of each V2 flood (default 30 s, 6 s with `--quick`; the
//! plan's 10 minutes is `GORCA_V2_SECS=200`, three floods).

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use gorca_bench_b2::clock::Rng;

use super::apps::{FakeApp, FakeMutter, ReaderSpec, json_num};
use super::m1::{K, Typist, flag, m1_env, m1_rig};
use super::mock::{Flavor, Kind, Rec};
use super::runner::{HostEnv, Rig};
use super::scenarios::{RunOut, Size, ms, pause, speaks};

const TERM: usize = 2;
const OTHER: usize = 3;
const PROMPT: &str = "me@box:~$ ";
/// The settle of ADR-0001 T-1, in milliseconds.
const SETTLE_MS: f64 = 30.0;

const ENTER: K = K {
    code: 36,
    sym: 0xff0d,
};
const BACKSPACE: K = K {
    code: 22,
    sym: 0xff08,
};
const TAB: K = K {
    code: 23,
    sym: 0xff09,
};
const LEFT: K = K {
    code: 113,
    sym: 0xff51,
};
const RIGHT: K = K {
    code: 114,
    sym: 0xff53,
};
const UP: K = K {
    code: 111,
    sym: 0xff52,
};
const DOWN: K = K {
    code: 116,
    sym: 0xff54,
};
const CTRL_L: K = K {
    code: 37,
    sym: 0xffe3,
};
const KEY_C: K = K {
    code: 54,
    sym: 0x63,
};
const KEY_A: K = K {
    code: 38,
    sym: 0x61,
};
const KEY_E: K = K {
    code: 26,
    sym: 0x65,
};
const CONTROL: u32 = 4;
const SHIFT: u32 = 1;
const KEY_U: K = K {
    code: 30,
    sym: 0x75,
};
const KEY_T: K = K {
    code: 28,
    sym: 0x54,
};
const PAGE_UP: K = K {
    code: 112,
    sym: 0xff55,
};
const SHIFT_L: K = K {
    code: 50,
    sym: 0xffe1,
};
/// Numpad review keys of the desktop layout, Num Lock off (ADR-0006 11).
const KP_PREV_LINE: K = K {
    code: 79,
    sym: 0xff95,
};
const KP_CUR_LINE: K = K {
    code: 80,
    sym: 0xff97,
};
const KP_NEXT_LINE: K = K {
    code: 81,
    sym: 0xff9a,
};
const KP_NEXT_WORD: K = K {
    code: 85,
    sym: 0xff98,
};
const KP_NEXT_CHAR: K = K {
    code: 89,
    sym: 0xff9b,
};

fn gate_open(host: &HostEnv, id: &str, out: &mut RunOut) -> Result<bool> {
    if flag(host, "GORCA_M6") {
        return Ok(true);
    }
    if flag(host, "GORCA_M6_STRICT") {
        bail!("{id}: GORCA_M6_STRICT=1 requires the M6 gates to run");
    }
    out.notes.push(format!(
        "{id} SKIPPED: set GORCA_M6=1 to run the M6 terminal gates"
    ));
    out.x("skipped", 1.0, "flag");
    Ok(false)
}

fn json_esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o
}

fn env_num(host: &HostEnv, key: &str) -> Option<u64> {
    host.reader_env
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.parse().ok())
}

/// A reader, a terminal application and a typist.
struct T {
    rig: Rig,
    app: FakeApp,
    ty: Typist,
}

fn setup(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    utter_ms: u64,
    extra: &[&str],
) -> Result<T> {
    let rig = m1_rig(host, reader, flavor)?;
    rig.mock
        .configure(Duration::from_millis(1), Duration::from_millis(utter_ms));
    let mut args = vec!["--term-trim"];
    args.extend_from_slice(extra);
    let app = FakeApp::spawn(
        &rig.env.harness_dir,
        "terminal",
        "gnome-terminal-fake",
        0,
        &args,
    )?;
    Ok(T {
        rig,
        app,
        ty: Typist::new(),
    })
}

/// What the program running in the terminal writes to the pty; the app then emits one VTE frame.
/// Returns the emit time of the frame's first signal.
fn feed(app: &mut FakeApp, text: &str) -> Result<u64> {
    let r = app.proc.request(
        "term_feed",
        &format!("\"text\":\"{}\"", json_esc(text)),
        Duration::from_secs(5),
    )?;
    Ok(json_num(&r, "t_emit_ns").unwrap_or(0.0) as u64)
}

/// Put the prompt on the screen and focus the terminal until the reader speaks for it.
fn focus_term(t: &mut T, screen: &str) -> Result<Vec<String>> {
    feed(&mut t.app, screen)?;
    let end = Instant::now() + Duration::from_secs(10);
    let mut mark;
    loop {
        if Instant::now() > end {
            bail!("the reader did not speak for the focus of the terminal within 10 s");
        }
        t.app.focus(OTHER)?;
        std::thread::sleep(Duration::from_millis(40));
        mark = t.rig.mock.mark();
        t.app.focus(TERM)?;
        if t.rig
            .mock
            .wait_for(mark, Duration::from_millis(400), |r| {
                matches!(r.kind, Kind::Speak { .. })
            })
            .is_some()
        {
            break;
        }
    }
    // The baseline read and the cursor line follow the focus speech.
    std::thread::sleep(Duration::from_millis(500));
    Ok(texts(&t.rig.mock.since(mark))
        .into_iter()
        .map(|(_, s)| s)
        .collect())
}

fn mutter(rig: &mut Rig) -> Result<&mut FakeMutter> {
    rig.mutter.as_mut().context("no mutter")
}

fn key_down(t: &mut T, k: K, uni: u32, state: u32) -> Result<u64> {
    t.ty.t_ms += 3;
    let r = mutter(&mut t.rig)?.key_full(k.code, k.sym, uni, state, false, t.ty.t_ms)?;
    Ok(r.t_emit_ns)
}

fn key_up(t: &mut T, k: K, state: u32) -> Result<()> {
    t.ty.t_ms += 3;
    mutter(&mut t.rig)?.key_full(k.code, k.sym, 0, state, true, t.ty.t_ms)?;
    Ok(())
}

/// Press and release; returns the emit time of the press.
fn tap(t: &mut T, k: K, uni: u32, state: u32) -> Result<u64> {
    let e = key_down(t, k, uni, state)?;
    key_up(t, k, state)?;
    Ok(e)
}

fn ctrl_tap(t: &mut T, k: K, uni: u32) -> Result<u64> {
    key_down(t, CTRL_L, 0, 0)?;
    let e = tap(t, k, uni, CONTROL)?;
    key_up(t, CTRL_L, 0)?;
    Ok(e)
}

/// Any letter key will do, the reader reads the unichar.
fn letter_key(c: char) -> K {
    K {
        code: 38 + u16::try_from(c as u32 % 9).unwrap_or(0),
        sym: 0x0100_0000 + c as u32,
    }
}

fn type_key(t: &mut T, c: char) -> Result<u64> {
    tap(t, letter_key(c), c as u32, 0)
}

fn texts(recs: &[Rec]) -> Vec<(u64, String)> {
    speaks(recs).map(|(r, t)| (r.t_ns, t.to_owned())).collect()
}

/// What the reader says during `wait_ms` after `act`.
fn said_during(
    t: &mut T,
    wait_ms: u64,
    act: impl FnOnce(&mut T) -> Result<()>,
) -> Result<Vec<String>> {
    let mark = t.rig.mock.mark();
    act(t)?;
    std::thread::sleep(Duration::from_millis(wait_ms));
    Ok(texts(&t.rig.mock.since(mark))
        .into_iter()
        .map(|(_, s)| s)
        .collect())
}

/// The character as the reader names it.
fn name_of(c: char) -> String {
    match c {
        ' ' => "space".to_owned(),
        '-' => "dash".to_owned(),
        '&' => "and".to_owned(),
        c => c.to_string(),
    }
}

/// Counters of the fake app that every scenario ends with: `vte_assert` must be 0 (T-3).
fn finish(t: &mut T, out: &mut RunOut) -> Result<()> {
    let r = t.app.proc.request("stats", "", Duration::from_secs(5))?;
    out.x(
        "vte_assert",
        json_num(&r, "vte_assert").unwrap_or(0.0),
        "calls",
    );
    out.x(
        "term_frames",
        json_num(&r, "term_frames").unwrap_or(0.0),
        "frames",
    );
    keep_reader_log(t.rig.reader_log.as_deref());
    if !t.rig.reader.alive() {
        bail!("the reader died during the scenario");
    }
    Ok(())
}

/// Copy the reader's log into `GORCA_VV_KEEP_DIR` (a diagnosis aid; the scenarios delete it).
fn keep_reader_log(log: Option<&std::path::Path>) {
    let (Some(log), Some(dir)) = (log, std::env::var_os("GORCA_VV_KEEP_DIR")) else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let _ = std::fs::create_dir_all(&dir);
    if let Some(name) = log.file_name() {
        let _ = std::fs::copy(log, dir.join(name));
    }
}

fn n_of(size: Size, cap: usize) -> usize {
    size.n.min(cap)
}

// ---- V1 -------------------------------------------------------------------------------------

/// V1: the last VTE frame of a command's output to the SPEAK of it (T-1 settle 30 ms plus gorca).
pub fn v1(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    _seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "V1", out)? {
        return Ok(());
    }
    let mut t = setup(host, reader, flavor, 1, &[])?;
    focus_term(&mut t, PROMPT)?;
    let n = n_of(size, 150);
    let (mut lat, mut lost) = (Vec::with_capacity(n), 0u64);
    for i in 0..n {
        // The output of the previous probe has been said (the reader paces program output).
        std::thread::sleep(Duration::from_millis(1100));
        let mark = t.rig.mock.mark();
        let tag = format!("r{i:04}");
        let t_emit = feed(&mut t.app, &format!("\r\n{tag}\r\n{PROMPT}"))?;
        let hit = t.rig.mock.wait_for(
            mark,
            Duration::from_secs(2),
            |r| matches!(&r.kind, Kind::Speak { text, .. } if text.contains(&tag)),
        );
        match hit {
            Some(r) => lat.push(ms(r.t_ns.saturating_sub(t_emit))),
            None => lost += 1,
        }
    }
    let over: Vec<f64> = lat.iter().map(|l| l - SETTLE_MS).collect();
    out.s("v1_ms", lat);
    out.s("v1_over_ms", over);
    out.x("lost", lost as f64, "events");
    out.notes.push(
        "V1: emit of the last VTE frame to the SPEAK of the output it carried; includes the 30 ms settle, one GetText of the python fake app and the speech lane (gate: settle <= 30 ms wall + 5 ms gorca)"
            .into(),
    );
    finish(&mut t, out)
}

// ---- VW: output after a window switch, before any key --------------------------------------

/// VW: another application has the focus, the terminal's window is activated (Alt+Tab) and the
/// terminal takes the focus; the program's output that arrives before any key press is spoken
/// within the V1 budget. Window activation and focus event in both orders, the node's FOCUSED
/// state with and without lag (a state read from a cache older than the focus), and the focus
/// bouncing between two nodes of the application (a tab switch). The reader sees no key.
pub fn vw(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    _seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "VW", out)? {
        return Ok(());
    }
    let mut t = setup(host, reader, flavor, 1, &[])?;
    let mut bg = FakeApp::spawn(
        &t.rig.env.harness_dir,
        "terminal",
        "gnome-terminal-bg",
        0,
        &["--term-trim"],
    )?;
    feed(&mut bg, "bg$ ")?;
    focus_term(&mut t, PROMPT)?;
    // (order, delay of the `focused=1` signal = ms GetState hides FOCUSED, bounce through the
    // second node of the application first). The real VTE is (focus_first, 8, 8): the repeated
    // event comes inside the reader's duplicate window and the first read is stale.
    const CASES: [(&str, u64, bool); 7] = [
        ("activate_first", 0, false),
        ("focus_first", 0, false),
        ("focus_first", 8, false),
        ("activate_first", 8, false),
        ("focus_first", 8, true),
        ("focus_first", 160, false),
        ("focus_first", 400, false),
    ];
    let rounds = n_of(size, 12) / CASES.len() + 1;
    let (mut lat, mut lost) = (Vec::new(), 0u64);
    let mut by_case = vec![0u64; CASES.len()];
    for r in 0..rounds {
        for (ci, (order, lag, bounce)) in CASES.iter().enumerate() {
            // The other application takes the focus (and with it the protection).
            bg.focus(TERM)?;
            std::thread::sleep(Duration::from_millis(500));
            if *bounce {
                // Two nodes of one application, as two tabs: the focus leaves and comes back.
                t.app.focus(OTHER)?;
                std::thread::sleep(Duration::from_millis(500));
                bg.focus(OTHER)?;
                std::thread::sleep(Duration::from_millis(300));
            }
            let mark = t.rig.mock.mark();
            t.app.focus_window(TERM, order, *lag, *lag)?;
            // The focus speech and the baseline read of the screen come first.
            std::thread::sleep(Duration::from_millis(1300));
            let tag = format!("w{r:02}c{ci}");
            let t_emit = feed(&mut t.app, &format!("\r\n{tag}\r\n{PROMPT}"))?;
            let hit = t.rig.mock.wait_for(
                mark,
                Duration::from_secs(2),
                |rec| matches!(&rec.kind, Kind::Speak { text, .. } if text.contains(&tag)),
            );
            match hit {
                Some(rec) => lat.push(ms(rec.t_ns.saturating_sub(t_emit))),
                None => {
                    lost += 1;
                    by_case[ci] += 1;
                    out.notes.push(format!(
                        "VW: output not spoken after {order}, FOCUSED lag {lag} ms, bounce {bounce}"
                    ));
                }
            }
        }
    }
    out.s("vw_ms", lat);
    out.x("vw_lost", lost as f64, "events");
    out.notes.push(format!(
        "VW: output after Alt+Tab into the terminal before any key; lost per case {by_case:?} (order, FOCUSED lag, bounce: {CASES:?}); latency is frame emit to SPEAK as in V1"
    ));
    finish(&mut t, out)
}

// ---- VT: typed characters exactly once -------------------------------------------------------

#[derive(Debug)]
struct Probe {
    t_key: u64,
    expect: String,
    /// What was done, for the notes of a failed probe.
    what: &'static str,
    /// The text must only contain `expect` (program output), not equal it.
    contains: bool,
}

fn matches_probe(p: &Probe, s: &str) -> bool {
    if p.contains {
        s.contains(&p.expect)
    } else {
        s == p.expect
    }
}

/// Score probes: each owns the SPEAKs in `[t_key, next t_key)`. Returns latencies, missing,
/// doubled and extra (speech nobody asked for).
fn score(probes: &[Probe], spoken: &[(u64, String)]) -> (Vec<f64>, u64, u64, u64, Vec<String>) {
    let (l, _, m, d, e, n) = score_split(probes, spoken);
    (l, m, d, e, n)
}

/// [`score`] with the latencies of program output (`contains` probes) apart from those of keys.
#[allow(clippy::type_complexity)]
fn score_split(
    probes: &[Probe],
    spoken: &[(u64, String)],
) -> (
    Vec<f64>,
    Vec<(&'static str, f64)>,
    u64,
    u64,
    u64,
    Vec<String>,
) {
    let (mut lat, mut lat_out) = (Vec::new(), Vec::new());
    let (mut missing, mut double, mut extra) = (0u64, 0u64, 0u64);
    let mut notes = Vec::new();
    // A caret-driven answer (Left, Right) can land after the next key: it is claimed by the
    // probe that asked for it, not counted as the next key's extra speech.
    let mut claimed_late: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let late_ns = 100_000_000u64;
    for (i, p) in probes.iter().enumerate() {
        if !matches!(p.what, "left" | "right") {
            continue;
        }
        let end = probes.get(i + 1).map_or(u64::MAX, |n| n.t_key);
        let own = spoken
            .iter()
            .any(|(t, s)| *t >= p.t_key && *t < end && matches_probe(p, s));
        if own {
            continue;
        }
        if let Some(j) = spoken.iter().enumerate().position(|(j, (t, s))| {
            *t >= end
                && *t < end.saturating_add(late_ns)
                && matches_probe(p, s)
                && !claimed_late.contains(&j)
        }) {
            claimed_late.insert(j);
        }
    }
    for (i, p) in probes.iter().enumerate() {
        let end = probes.get(i + 1).map_or(u64::MAX, |n| n.t_key);
        let own_hit = spoken
            .iter()
            .any(|(t, s)| *t >= p.t_key && *t < end && matches_probe(p, s));
        let late_hit = !own_hit
            && matches!(p.what, "left" | "right")
            && spoken.iter().enumerate().any(|(j, (t, s))| {
                claimed_late.contains(&j)
                    && *t >= end
                    && *t < end.saturating_add(late_ns)
                    && matches_probe(p, s)
            });
        let win: Vec<&(u64, String)> = spoken
            .iter()
            .enumerate()
            .filter(|(j, (t, _))| *t >= p.t_key && *t < end && !claimed_late.contains(j))
            .map(|(_, x)| x)
            .collect();
        let hits: Vec<&&(u64, String)> = win.iter().filter(|(_, s)| matches_probe(p, s)).collect();
        match hits.first() {
            None if late_hit => {}
            Some((t, _)) => {
                let l = ms(t.saturating_sub(p.t_key));
                match p.what {
                    // Spoken from the key itself.
                    "type" | "backspace" => lat.push(l),
                    // Spoken when the application's caret event arrives (after the echo delay of
                    // the harness) or when the program's output has settled.
                    _ => lat_out.push((p.what, l)),
                }
            }
            None => missing += 1,
        }
        if hits.len() > 1 {
            double += 1;
        }
        let others = win.len() - hits.len();
        extra += others as u64;
        if (hits.is_empty() || hits.len() > 1 || others > 0) && notes.len() < 6 {
            // A little context on both sides: a late answer shows up in the next window.
            let heard: Vec<String> = spoken
                .iter()
                .filter(|(t, _)| *t + 60_000_000 >= p.t_key && *t < end.saturating_add(120_000_000))
                .map(|(t, s)| format!("{:+.1}ms {s:?}", (*t as f64 - p.t_key as f64) / 1e6))
                .collect();
            notes.push(format!(
                "probe #{i} ({}; before: {}) wanted {:?}{}, heard [{}]",
                p.what,
                probes[i.saturating_sub(2)..i]
                    .iter()
                    .map(|q| format!("{} {:?}", q.what, q.expect))
                    .collect::<Vec<_>>()
                    .join(", "),
                p.expect,
                if p.contains { " (contained)" } else { "" },
                heard.join(", ")
            ));
        }
    }
    (lat, lat_out, missing, double, extra, notes)
}

/// The shell of VT: the typed line and the cursor in it.
struct Line {
    chars: Vec<char>,
    cur: usize,
}

/// VT: 10,000 randomised actions (1,000 with `--quick`). Every key must be spoken exactly once,
/// by the key, and the shell's echo of it must stay silent.
pub fn vt(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "VT", out)? {
        return Ok(());
    }
    // One row for the whole session: the harness models a single line, and the fake terminal
    // would wrap it at 80 columns and stop the cursor at the edge (no caret event for a Right
    // there), which is the fake's behaviour, not the reader's.
    let mut t = setup(host, reader, flavor, 1, &["--term-cols", "20000"])?;
    focus_term(&mut t, PROMPT)?;
    let n = if size.n >= 1200 { 10_000 } else { 1_000 };
    let mut rng = Rng::new(seed);
    let alphabet: Vec<char> = "abcdefghijklmnopqrstuvwxyz0123456789 ".chars().collect();
    let mut line = Line {
        chars: Vec::new(),
        cur: 0,
    };
    let mark = t.rig.mock.mark();
    let mut probes: Vec<Probe> = Vec::with_capacity(n);
    let mut outputs = 0u32;
    let mut mix = [0u64; 5];
    // VTE trims trailing blanks from the text it exposes, so a line that ends in a space cannot
    // be told from one that does not: the session never rests on one. A typed space is followed
    // by a letter, and BackSpace never leaves a space last.
    let mut must_letter = false;
    // A real terminal answers a key a frame later (pty, shell, VTE's frame clock: up to 16 ms).
    // `GORCA_VT_ECHO_MS=0` is the adversarial order: the screen changes before the key is seen.
    let echo_ms = env_num(host, "GORCA_VT_ECHO_MS").unwrap_or(10);
    let echo = |app: &mut FakeApp, text: &str| -> Result<u64> {
        std::thread::sleep(Duration::from_millis(echo_ms));
        feed(app, text)
    };
    for _ in 0..n {
        let roll = if must_letter { 999 } else { rng.below(1000) };
        let at_end = line.cur == line.chars.len();
        let leaves_space = line.chars.len() >= 2 && line.chars[line.chars.len() - 2] == ' ';
        if !at_end && roll < 500 {
            // Back to the end of the line, one Right at a time.
            let t_key = tap(&mut t, RIGHT, 0, 0)?;
            line.cur += 1;
            echo(&mut t.app, "\x1b[C")?;
            let expect = line
                .chars
                .get(line.cur)
                .map_or_else(|| "blank".to_owned(), |c| name_of(*c));
            probes.push(Probe {
                t_key,
                expect,
                what: "right",
                contains: false,
            });
            mix[3] += 1;
        } else if at_end && roll < 40 && !line.chars.is_empty() && !leaves_space {
            let c = line.chars.pop().unwrap_or(' ');
            line.cur -= 1;
            let t_key = tap(&mut t, BACKSPACE, 8, 0)?;
            echo(&mut t.app, "\x08 \x08")?;
            probes.push(Probe {
                t_key,
                expect: name_of(c),
                what: "backspace",
                contains: false,
            });
            mix[1] += 1;
        } else if at_end && roll < 80 && !line.chars.is_empty() {
            line.cur -= 1;
            let t_key = tap(&mut t, LEFT, 0, 0)?;
            echo(&mut t.app, "\x1b[D")?;
            probes.push(Probe {
                t_key,
                expect: name_of(line.chars[line.cur]),
                what: "left",
                contains: false,
            });
            mix[2] += 1;
        } else if at_end && roll < 90 && !line.chars.is_empty() {
            // Enter: the program prints a short line, the prompt returns. The reader paces
            // program output, so the next action waits for it.
            outputs += 1;
            let tag = format!("o{outputs}");
            let t_key = tap(&mut t, ENTER, 13, 0)?;
            echo(&mut t.app, &format!("\r\n{tag}\r\n{PROMPT}"))?;
            probes.push(Probe {
                t_key,
                expect: tag,
                what: "enter",
                contains: true,
            });
            line.chars.clear();
            line.cur = 0;
            mix[4] += 1;
            std::thread::sleep(Duration::from_millis(1000));
        } else if at_end {
            let mut c = alphabet[rng.below(alphabet.len() as u64) as usize];
            if c == ' ' && (must_letter || line.chars.is_empty()) {
                c = 'x';
            }
            must_letter = c == ' ';
            let t_key = type_key(&mut t, c)?;
            echo(&mut t.app, &c.to_string())?;
            line.chars.push(c);
            line.cur += 1;
            probes.push(Probe {
                t_key,
                expect: name_of(c),
                what: "type",
                contains: false,
            });
            mix[0] += 1;
        }
        pause(&mut rng, 20, 30);
    }
    std::thread::sleep(Duration::from_millis(1500));
    let spoken = texts(&t.rig.mock.since(mark));
    let (lat, lat_out, missing, double, extra, notes) = score_split(&probes, &spoken);
    out.notes.extend(notes);
    out.s("vt_ms", lat);
    for (name, metric) in [
        ("left", "vt_left_ms"),
        ("right", "vt_right_ms"),
        ("enter", "vt_output_ms"),
    ] {
        out.s(
            metric,
            lat_out
                .iter()
                .filter(|(w, _)| *w == name)
                .map(|(_, l)| *l)
                .collect(),
        );
    }
    out.x("vt_missing", missing as f64, "events");
    out.x("vt_double", double as f64, "events");
    out.x("vt_extra", extra as f64, "events");
    out.x("vt_keys", probes.len() as f64, "keys");
    out.notes.push(format!(
        "VT: {} keys (typed {}, BackSpace {}, Left {}, Right {}, Enter {}); every one spoken exactly once, nothing else",
        probes.len(),
        mix[0],
        mix[1],
        mix[2],
        mix[3],
        mix[4]
    ));
    finish(&mut t, out)
}

// ---- VH: readline session ---------------------------------------------------------------------

struct Check {
    failures: u64,
    notes: Vec<String>,
}

impl Check {
    /// Compare what was said with what the step expects. A `~` prefix means "contains".
    fn step(&mut self, name: &str, want: &[&str], got: &[String]) {
        let ok = want.len() == got.len()
            && want.iter().zip(got).all(|(w, g)| {
                w.strip_prefix('~')
                    .map_or_else(|| w == g, |part| g.contains(part))
            });
        if !ok {
            self.failures += 1;
            if self.notes.len() < 12 {
                self.notes
                    .push(format!("{name}: wanted {want:?}, heard {got:?}"));
            }
        }
    }
}

/// VH: a readline session scripted key by key: what the reader says to each.
pub fn vh(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    _size: Size,
    _seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "VH", out)? {
        return Ok(());
    }
    let mut t = setup(host, reader, flavor, 1, &[])?;
    focus_term(&mut t, PROMPT)?;
    let mut c = Check {
        failures: 0,
        notes: Vec::new(),
    };
    const W: u64 = 350;
    // Typing "gi", Tab completes to "git ".
    for ch in ['g', 'i'] {
        let got = said_during(&mut t, W, |t| {
            type_key(t, ch)?;
            feed(&mut t.app, &ch.to_string())?;
            Ok(())
        })?;
        c.step(&format!("type {ch}"), &[&ch.to_string()], &got);
    }
    let got = said_during(&mut t, W, |t| {
        tap(t, TAB, 9, 0)?;
        feed(&mut t.app, "t ")?;
        Ok(())
    })?;
    c.step("Tab completes git", &["git"], &got);
    for ch in "status".chars() {
        let got = said_during(&mut t, W, |t| {
            type_key(t, ch)?;
            feed(&mut t.app, &ch.to_string())?;
            Ok(())
        })?;
        c.step(&format!("type {ch}"), &[&ch.to_string()], &got);
    }
    // The line is "git status", the cursor at its end (column 10 of the line).
    let got = said_during(&mut t, W, |t| {
        tap(t, LEFT, 0, 0)?;
        feed(&mut t.app, "\x1b[D")?;
        Ok(())
    })?;
    c.step("Left: s", &["s"], &got);
    let got = said_during(&mut t, W, |t| {
        tap(t, LEFT, 0, 0)?;
        feed(&mut t.app, "\x1b[D")?;
        Ok(())
    })?;
    c.step("Left: u", &["u"], &got);
    let got = said_during(&mut t, W, |t| {
        key_down(t, CTRL_L, 0, 0)?;
        tap(t, LEFT, 0, CONTROL)?;
        key_up(t, CTRL_L, 0)?;
        feed(&mut t.app, "\x1b[4D")?;
        Ok(())
    })?;
    c.step("Ctrl+Left: the word", &["status"], &got);
    let got = said_during(&mut t, W, |t| {
        ctrl_tap(t, KEY_A, 1)?;
        feed(&mut t.app, "\r\x1b[10C")?;
        Ok(())
    })?;
    c.step("Ctrl+A: first character", &["g"], &got);
    let got = said_during(&mut t, W, |t| {
        ctrl_tap(t, KEY_E, 5)?;
        feed(&mut t.app, "\r\x1b[20C")?;
        Ok(())
    })?;
    c.step("Ctrl+E: past the end", &["blank"], &got);
    let got = said_during(&mut t, W, |t| {
        tap(t, BACKSPACE, 8, 0)?;
        feed(&mut t.app, "\x08 \x08")?;
        Ok(())
    })?;
    c.step("BackSpace: s", &["s"], &got);
    let got = said_during(&mut t, W, |t| {
        type_key(t, 's')?;
        feed(&mut t.app, "s")?;
        Ok(())
    })?;
    c.step("type s", &["s"], &got);
    let got = said_during(&mut t, 700, |t| {
        tap(t, ENTER, 13, 0)?;
        feed(&mut t.app, &format!("\r\nOn branch main\r\n{PROMPT}"))?;
        Ok(())
    })?;
    c.step("Enter: the output", &["~On branch main"], &got);
    std::thread::sleep(Duration::from_millis(1500));
    let recall = |cmd: &str| format!("\r{PROMPT}{cmd}\x1b[K");
    let got = said_during(&mut t, W, |t| {
        tap(t, UP, 0, 0)?;
        feed(&mut t.app, &recall("git status"))?;
        Ok(())
    })?;
    c.step("Up: history", &["git status"], &got);
    let got = said_during(&mut t, W, |t| {
        tap(t, UP, 0, 0)?;
        feed(&mut t.app, &recall("ls"))?;
        Ok(())
    })?;
    c.step("Up: older", &["ls"], &got);
    let got = said_during(&mut t, W, |t| {
        tap(t, DOWN, 0, 0)?;
        feed(&mut t.app, &recall("git status"))?;
        Ok(())
    })?;
    c.step("Down: newer", &["git status"], &got);
    let got = said_during(&mut t, W, |t| {
        tap(t, DOWN, 0, 0)?;
        feed(&mut t.app, &recall(""))?;
        Ok(())
    })?;
    c.step("Down: empty line", &["blank"], &got);
    let got = said_during(&mut t, 700, |t| {
        ctrl_tap(t, KEY_C, 3)?;
        feed(&mut t.app, &format!("^C\r\n{PROMPT}"))?;
        Ok(())
    })?;
    // bash echoes ^C after what is typed, then prints a new prompt; the verbalizer names the caret.
    c.step("Ctrl+C at the prompt", &["~caret C"], &got);
    std::thread::sleep(Duration::from_millis(1200));
    // A directory listing of twenty lines is read once, in order.
    let mut listing = String::from("\r\n");
    for i in 0..20 {
        let _ = write!(listing, "file{i:02}\r\n");
    }
    listing.push_str(PROMPT);
    let got = said_during(&mut t, 900, |t| {
        tap(t, ENTER, 13, 0)?;
        feed(&mut t.app, &listing)?;
        Ok(())
    })?;
    let one = got.len() == 1 && got[0].contains("file00") && got[0].contains("file19");
    if !one {
        c.failures += 1;
        c.notes.push(format!(
            "listing: wanted one utterance with file00..file19, heard {} utterance(s)",
            got.len()
        ));
    }
    out.x("vh_failures", c.failures as f64, "steps");
    out.notes.extend(c.notes);
    out.notes.push("VH: typing, Tab, Left, Ctrl+Left, Ctrl+A/E, BackSpace, Enter, Up/Down history, Ctrl+C, a 20-line listing".into());
    finish(&mut t, out)
}

// ---- V2, V3, VB: floods -----------------------------------------------------------------------

/// The largest unread byte count of the sockets of `pid`, by `ss -xpn` (the SIOCINQ the broker's
/// backlog shows as on the reader's side).
fn recv_q_of(pid: u32) -> Option<u64> {
    let o = std::process::Command::new("ss")
        .args(["-xpn"])
        .output()
        .ok()?;
    if !o.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&o.stdout);
    let tag = format!("pid={pid},");
    let mut best = 0u64;
    let mut any = false;
    for l in text.lines().filter(|l| l.contains(&tag)) {
        any = true;
        let cols: Vec<&str> = l.split_whitespace().collect();
        if let Some(q) = cols.get(2).and_then(|c| c.parse::<u64>().ok()) {
            best = best.max(q);
        }
    }
    any.then_some(best)
}

struct Sampler {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<Vec<u64>>>,
}

impl Sampler {
    fn start(pid: u32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let s = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let mut v = Vec::new();
            while !s.load(Ordering::Acquire) {
                if let Some(q) = recv_q_of(pid) {
                    v.push(q);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            v
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }

    fn finish(mut self) -> Vec<u64> {
        self.stop.store(true, Ordering::Release);
        self.thread
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default()
    }
}

fn p99_u64(v: &[u64]) -> u64 {
    if v.is_empty() {
        return 0;
    }
    let mut s = v.to_vec();
    s.sort_unstable();
    s[((s.len() - 1) as f64 * 0.99) as usize]
}

fn gen_start(t: &mut T, kind: &str, lines: u32) -> Result<()> {
    t.app.proc.request(
        "term_gen",
        &format!("\"fps\":60,\"lines\":{lines},\"kind\":\"{kind}\""),
        Duration::from_secs(5),
    )?;
    Ok(())
}

fn gen_stop(t: &mut T) -> Result<()> {
    t.app
        .proc
        .request("term_stop", "", Duration::from_secs(5))?;
    Ok(())
}

/// V2: `cat` of a huge file (screens of distinct lines), `yes` and a `make -j` log in the
/// focused terminal. The reader must not fall behind (the backlog on its bus socket stays
/// small), must stay well under a core, must not flood speech, and must stay responsive.
pub fn v2(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    _seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "V2", out)? {
        return Ok(());
    }
    let secs = env_num(host, "GORCA_V2_SECS").unwrap_or(if size.n >= 1200 { 30 } else { 6 });
    let mut t = setup(
        host,
        reader,
        flavor,
        5,
        &["--term-rows", "60", "--term-cols", "240"],
    )?;
    focus_term(&mut t, PROMPT)?;
    let pid = t.rig.reader.pid;
    let mut worst_cpu = 0.0f64;
    let mut worst_q = 0u64;
    let mut all_key = Vec::new();
    let mut key_lost = 0u64;
    let mut measured_q = false;
    let stats = |t: &mut T| -> Result<(f64, f64)> {
        let r = t.app.proc.request("stats", "", Duration::from_secs(5))?;
        Ok((
            json_num(&r, "term_frames").unwrap_or(0.0),
            json_num(&r, "term_bytes").unwrap_or(0.0),
        ))
    };
    for (name, kind, lines) in [
        ("cat", "seq", 150u32),
        ("yes", "yes", 600),
        ("make", "build", 40),
    ] {
        let sampler = Sampler::start(pid);
        std::thread::sleep(Duration::from_millis(200));
        let mark = t.rig.mock.mark();
        let (f0, b0) = stats(&mut t)?;
        let (c0, t0) = (t.rig.reader.cpu_ns(), super::mono::now_ns());
        gen_start(&mut t, kind, lines)?;
        // First 60 %: nobody types. How often does a flood make the reader speak?
        std::thread::sleep(Duration::from_millis(secs * 600));
        let t_quiet = super::mono::now_ns();
        // The rest: typed keys in the middle of the flood; the answer must stay immediate.
        let end = Instant::now() + Duration::from_millis(secs * 400);
        let mut rng = Rng::new(7);
        let mut probes = Vec::new();
        while Instant::now() < end {
            std::thread::sleep(Duration::from_millis(250 + rng.below(200)));
            let c = (b'a' + (rng.below(26) as u8)) as char;
            let t_key = type_key(&mut t, c)?;
            probes.push(Probe {
                t_key,
                expect: name_of(c),
                what: "type",
                contains: false,
            });
        }
        let (c1, t1) = (t.rig.reader.cpu_ns(), super::mono::now_ns());
        let (f1, b1) = stats(&mut t)?;
        gen_stop(&mut t)?;
        let q = sampler.finish();
        measured_q |= !q.is_empty();
        let wall = (t1 - t0) as f64;
        let cpu = c1.saturating_sub(c0) as f64 / wall * 100.0;
        std::thread::sleep(Duration::from_millis(1500));
        let recs = t.rig.mock.since(mark);
        let spoken = texts(&recs);
        let quiet_speaks = spoken
            .iter()
            .filter(|(ts, _)| *ts >= t0 && *ts < t_quiet)
            .count();
        let (lat, missing, _, _, _) = score(&probes, &spoken);
        key_lost += missing;
        all_key.extend(lat);
        out.x(&format!("v2_{name}_cpu_pct"), cpu, "% of one core");
        out.x(
            &format!("v2_{name}_speaks_per_s"),
            quiet_speaks as f64 / ((t_quiet - t0) as f64 / 1e9),
            "1/s",
        );
        out.x(
            &format!("v2_{name}_frames_per_s"),
            (f1 - f0) / (wall / 1e9),
            "frames/s",
        );
        out.x(
            &format!("v2_{name}_signal_mb_per_s"),
            (b1 - b0) / (wall / 1e9) / 1e6,
            "MB/s of text-changed payload",
        );
        out.x(
            &format!("v2_{name}_siocinq_p99_kib"),
            p99_u64(&q) as f64 / 1024.0,
            "KiB",
        );
        worst_cpu = worst_cpu.max(cpu);
        worst_q = worst_q.max(p99_u64(&q));
        // The reader is still there and still reads the terminal after the flood.
        let tag = format!("after{name}");
        let mark = t.rig.mock.mark();
        feed(&mut t.app, &format!("\r\n{tag}\r\n{PROMPT}"))?;
        let ok = t.rig.mock.wait_for(
            mark,
            Duration::from_secs(4),
            |r| matches!(&r.kind, Kind::Speak { text, .. } if text.contains(&tag)),
        );
        out.x(
            &format!("v2_{name}_after_missing"),
            f64::from(u8::from(ok.is_none())),
            "events",
        );
        if !t.rig.reader.alive() {
            bail!("the reader died during the {name} flood");
        }
        std::thread::sleep(Duration::from_millis(1500));
    }
    out.x("v2_worst_cpu_pct", worst_cpu, "% of one core");
    out.x("v2_siocinq_p99_kib", worst_q as f64 / 1024.0, "KiB");
    out.x(
        "v2_siocinq_measured",
        f64::from(u8::from(measured_q)),
        "flag",
    );
    out.s("v2_key_ms", all_key);
    out.x("v2_key_lost", key_lost as f64, "events");
    out.x("v2_alive", 1.0, "flag");
    out.notes.push(format!(
        "V2: three floods of {secs} s each (60 % unattended, then typed keys) at 60 frames/s on a 60x240 screen (cat: 150 distinct lines per frame, yes: 600, make: 40); gates: cpu < 60 % of a core, SIOCINQ p99 < 1 MiB (ss -xpn on the reader's sockets every 50 ms), no broker disconnect (the reader answers after every flood)"
    ));
    finish(&mut t, out)
}

/// V3: Ctrl+C during a flood silences speech at once (the L2 path) and the next prompt is said.
pub fn v3(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    _seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "V3", out)? {
        return Ok(());
    }
    let mut t = setup(
        host,
        reader,
        flavor,
        600,
        &["--term-rows", "40", "--term-cols", "120"],
    )?;
    focus_term(&mut t, PROMPT)?;
    let trials = if size.n >= 1200 { 30 } else { 10 };
    let (mut stop_lat, mut prompt_lat) = (Vec::new(), Vec::new());
    let (mut no_stop, mut prompt_missing, mut stale) = (0u64, 0u64, 0u64);
    for i in 0..trials {
        let started = t.rig.mock.mark();
        gen_start(&mut t, "seq", 40)?;
        // Let the flood be read and spoken: an utterance is in flight when Ctrl+C comes.
        t.rig
            .mock
            .wait_for(started, Duration::from_secs(5), |r| {
                matches!(&r.kind, Kind::Speak { text, .. } if text.chars().take(8).all(|c| c.is_ascii_digit()))
            })
            .context("the flood was never spoken")?;
        std::thread::sleep(Duration::from_millis(150));
        let mark = t.rig.mock.mark();
        let t_key = ctrl_tap(&mut t, KEY_C, 3)?;
        gen_stop(&mut t)?;
        let tag = format!("done{i}");
        let t_emit = feed(&mut t.app, &format!("^C\r\n{tag}\r\n{PROMPT}"))?;
        let cancel = t.rig.mock.wait_for(mark, Duration::from_millis(300), |r| {
            matches!(
                &r.kind,
                Kind::Cancel {
                    had_speech: true,
                    ..
                }
            )
        });
        match cancel {
            Some(r) => stop_lat.push(ms(r.t_ns.saturating_sub(t_key))),
            None => no_stop += 1,
        }
        let hit = t.rig.mock.wait_for(
            mark,
            Duration::from_millis(1500),
            |r| matches!(&r.kind, Kind::Speak { text, .. } if text.contains(&tag)),
        );
        match hit {
            Some(r) => prompt_lat.push(ms(r.t_ns.saturating_sub(t_emit))),
            None => prompt_missing += 1,
        }
        // Nothing of the flood is said after the key: flood lines start with an 8-digit counter.
        std::thread::sleep(Duration::from_millis(500));
        stale += speaks(&t.rig.mock.since(mark))
            .filter(|(r, text)| {
                r.t_ns >= t_key
                    && text.chars().take(8).all(|c| c.is_ascii_digit())
                    && text.len() >= 8
            })
            .count() as u64;
        std::thread::sleep(Duration::from_millis(800));
    }
    out.s("v3_stop_ms", stop_lat);
    out.s("v3_prompt_ms", prompt_lat);
    out.x("v3_no_stop", no_stop as f64, "events");
    out.x("v3_prompt_missing", prompt_missing as f64, "events");
    out.x("v3_stale", stale as f64, "events");
    out.notes.push(
        "V3: Ctrl+C key emit to the speech cancel at the server (the L2 path), then the app stops the flood and prints ^C and the prompt; v3_stale counts flood lines spoken after the key"
            .into(),
    );
    finish(&mut t, out)
}

/// VB: a second terminal that is not focused floods; typing in the focused one stays immediate
/// and the flood says nothing (ADR-0001 T-4).
pub fn vb(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "VB", out)? {
        return Ok(());
    }
    let mut t = setup(host, reader, flavor, 1, &[])?;
    let mut bg = FakeApp::spawn(
        &t.rig.env.harness_dir,
        "terminal",
        "gnome-terminal-bg",
        0,
        &["--term-trim", "--term-rows", "50", "--term-cols", "200"],
    )?;
    // The reader learns the background terminal by focusing it once.
    feed(&mut bg, "bg$ ")?;
    bg.focus(TERM)?;
    std::thread::sleep(Duration::from_millis(500));
    focus_term(&mut t, PROMPT)?;
    bg.proc.request(
        "term_gen",
        "\"fps\":60,\"lines\":90,\"kind\":\"seq\"",
        Duration::from_secs(5),
    )?;
    std::thread::sleep(Duration::from_millis(1000));
    let n = n_of(size, 400);
    let mut rng = Rng::new(seed);
    let mark = t.rig.mock.mark();
    let (c0, t0) = (t.rig.reader.cpu_ns(), super::mono::now_ns());
    let mut probes = Vec::new();
    for i in 0..n {
        let c = (b'a' + (i % 26) as u8) as char;
        let t_key = type_key(&mut t, c)?;
        feed(&mut t.app, &c.to_string())?;
        probes.push(Probe {
            t_key,
            expect: name_of(c),
            what: "type",
            contains: false,
        });
        pause(&mut rng, 20, 40);
    }
    let (c1, t1) = (t.rig.reader.cpu_ns(), super::mono::now_ns());
    std::thread::sleep(Duration::from_millis(500));
    bg.proc.request("term_stop", "", Duration::from_secs(5))?;
    let spoken = texts(&t.rig.mock.since(mark));
    let (lat, missing, double, extra, notes) = score(&probes, &spoken);
    out.notes.extend(notes);
    out.s("vb_key_ms", lat);
    out.x("vb_missing", missing as f64, "events");
    out.x("vb_double", double as f64, "events");
    out.x("vb_stray_speaks", extra as f64, "utterances");
    out.x(
        "vb_cpu_pct",
        c1.saturating_sub(c0) as f64 / (t1 - t0) as f64 * 100.0,
        "% of one core",
    );
    out.notes.push(
        "VB: a second terminal floods at 60 frames/s while the focused one is typed in; the key echo and the shell echo of the focused one are the only speech".into(),
    );
    finish(&mut t, out)
}

// ---- VP: password prompt ---------------------------------------------------------------------

/// VP: a `sudo` prompt is spoken, a password typed at it is spoken and never logged.
pub fn vp(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    _size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "VP", out)? {
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("gorca-h.vp{}", std::process::id()));
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
    let app = FakeApp::spawn(
        &rig.env.harness_dir,
        "terminal",
        "gnome-terminal-fake",
        0,
        &["--term-trim"],
    )?;
    let mut t = T {
        rig,
        app,
        ty: Typist::new(),
    };
    focus_term(&mut t, PROMPT)?;
    let mut rng = Rng::new(seed);
    for c in "sudo ls".chars() {
        type_key(&mut t, c)?;
        feed(&mut t.app, &c.to_string())?;
        pause(&mut rng, 20, 30);
    }
    let mark = t.rig.mock.mark();
    tap(&mut t, ENTER, 13, 0)?;
    feed(&mut t.app, "\r\n[sudo] password for me: ")?;
    let prompt = t.rig.mock.wait_for(
        mark,
        Duration::from_secs(2),
        |r| matches!(&r.kind, Kind::Speak { text, .. } if text.contains("password for me")),
    );
    out.x(
        "vp_prompt_missing",
        f64::from(u8::from(prompt.is_none())),
        "events",
    );
    std::thread::sleep(Duration::from_millis(400));
    // The password is typed; the terminal shows nothing.
    let secret = "Jq7Xk0Wz4Vy3";
    let read = |p: &std::path::Path| std::fs::read_to_string(p).unwrap_or_default();
    let (present_from, stderr_from) = (read(&log).len(), read(&err).len());
    let mark = t.rig.mock.mark();
    let mut probes = Vec::new();
    for c in secret.chars() {
        let t_key = type_key(&mut t, c)?;
        probes.push(Probe {
            t_key,
            expect: name_of(c),
            what: "type",
            contains: false,
        });
        pause(&mut rng, 20, 30);
    }
    tap(&mut t, ENTER, 13, 0)?;
    feed(
        &mut t.app,
        "\r\nSorry, try again.\r\n[sudo] password for me: ",
    )?;
    std::thread::sleep(Duration::from_millis(900));
    let spoken = texts(&t.rig.mock.since(mark));
    let (_, missing, double, _, notes) = score(&probes, &spoken);
    out.notes.extend(notes);
    out.x("vp_echo_missing", missing as f64, "events");
    out.x("vp_echo_double", double as f64, "events");
    std::thread::sleep(Duration::from_millis(500));
    let (present, stderr) = (read(&log), read(&err));
    let mut hits = 0u64;
    for (hay, from) in [(&present, present_from), (&stderr, stderr_from)] {
        hits += secret_leaks(hay, hay.get(from..).unwrap_or(""), secret);
    }
    out.x("vp_plaintext_in_logs", hits as f64, "occurrences");
    out.x("vp_log_lines", present.lines().count() as f64, "lines");
    out.x("vp_stderr_bytes", stderr.len() as f64, "bytes");
    let r = finish(&mut t, out);
    if hits == 0 {
        let _ = std::fs::remove_dir_all(&dir);
    } else {
        // A failure keeps its evidence.
        out.notes
            .push(format!("VP: logs with the leak kept in {}", dir.display()));
    }
    r
}

/// Occurrences of a typed secret in a log: the whole secret, its two halves, and (m6-review
/// finding 7) every secret character that shows up as a value of its own: quoted (`'q'`, `"q"`,
/// `Typed('q')`) or after a key-like field name (`key=q`, `char: q`, `text=q`). `phase` is the
/// part of the log written while the secret was typed; the per-character check looks only at it.
/// Use secrets made of rare characters so ordinary log words cannot match.
fn secret_leaks(whole: &str, phase: &str, secret: &str) -> u64 {
    let n = secret.chars().count();
    let half = |a: usize, b: usize| secret.chars().skip(a).take(b - a).collect::<String>();
    let mut hits = 0u64;
    for needle in [secret.to_owned(), half(0, n / 2 + 1), half(n / 2, n)] {
        hits += whole.matches(needle.as_str()).count() as u64;
    }
    let mut seen: Vec<char> = Vec::new();
    for c in secret.chars() {
        if seen.contains(&c) {
            continue;
        }
        seen.push(c);
        for line in phase.lines() {
            if char_field(line, c) {
                hits += 1;
            }
        }
    }
    hits
}

/// Does `line` carry `c` as a value of its own?
fn char_field(line: &str, c: char) -> bool {
    for q in ['\'', '"'] {
        let quoted = format!("{q}{c}{q}");
        if line.contains(&quoted) {
            return true;
        }
    }
    for field in [
        "key", "char", "chars", "ch", "text", "typed", "keyval", "unichar", "echo",
    ] {
        for sep in ["=", ": ", ":"] {
            let pat = format!("{field}{sep}{c}");
            let mut from = 0;
            while let Some(i) = line.get(from..).and_then(|l| l.find(&pat)) {
                let end = from + i + pat.len();
                let before = line.get(..from + i).and_then(|b| b.chars().next_back());
                let after = line.get(end..).and_then(|a| a.chars().next());
                if !before.is_some_and(char::is_alphanumeric)
                    && !after.is_some_and(char::is_alphanumeric)
                {
                    return true;
                }
                from = end;
            }
        }
    }
    false
}

// ---- VR: review ------------------------------------------------------------------------------

fn calls_total(app: &mut FakeApp, reset: bool) -> Result<f64> {
    let r = app.proc.request(
        "calls",
        if reset { "\"reset\":true" } else { "" },
        Duration::from_secs(5),
    )?;
    Ok(json_num(&r, "total").unwrap_or(0.0))
}

/// VR: review the terminal screen with the numpad keys: the lines, words and characters of the
/// held screen, with no call to the application, and a screen that changes under the cursor.
pub fn vr(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "VR", out)? {
        return Ok(());
    }
    let mut t = setup(host, reader, flavor, 1, &[])?;
    let mut screen = String::new();
    for i in 0..8 {
        let _ = write!(screen, "row {i} alpha beta\r\n");
    }
    screen.push_str(PROMPT);
    focus_term(&mut t, &screen)?;
    let mut fails = 0u64;
    let mut notes = Vec::new();
    let mut expect = |name: &str, want: &str, got: &[String]| {
        if got.len() != 1 || got[0] != want {
            fails += 1;
            if notes.len() < 8 {
                notes.push(format!("{name}: wanted {want:?}, heard {got:?}"));
            }
        }
    };
    // Review starts on the line the caret is on (the prompt).
    let got = said_during(&mut t, 300, |t| {
        tap(t, KP_CUR_LINE, 0, 0)?;
        Ok(())
    })?;
    expect("current line", "me at box: $", &got);
    for (i, row) in (0..8).rev().take(3).enumerate() {
        let got = said_during(&mut t, 250, |t| {
            tap(t, KP_PREV_LINE, 0, 0)?;
            Ok(())
        })?;
        expect(
            &format!("previous line {i}"),
            &format!("row {row} alpha beta"),
            &got,
        );
    }
    let got = said_during(&mut t, 250, |t| {
        tap(t, KP_NEXT_WORD, 0, 0)?;
        Ok(())
    })?;
    expect("next word", "5", &got);
    let got = said_during(&mut t, 250, |t| {
        tap(t, KP_NEXT_CHAR, 0, 0)?;
        Ok(())
    })?;
    let _ = got;
    // The screen changes under the cursor: row 5 is rewritten in place (cursor addressing).
    feed(&mut t.app, "\x1b[6;1Hchanged line five\x1b[K\x1b[10;11H")?;
    std::thread::sleep(Duration::from_millis(300));
    let got = said_during(&mut t, 300, |t| {
        tap(t, KP_CUR_LINE, 0, 0)?;
        Ok(())
    })?;
    expect("changed under the cursor", "changed line five", &got);
    // A series of steps: latency and no IPC.
    let n = n_of(size, 400);
    let mut rng = Rng::new(seed);
    calls_total(&mut t.app, true)?;
    let mark = t.rig.mock.mark();
    let mut emits = Vec::with_capacity(n);
    for i in 0..n {
        let k = if i % 2 == 0 {
            KP_PREV_LINE
        } else {
            KP_NEXT_LINE
        };
        emits.push(tap(&mut t, k, 0, 0)?);
        pause(&mut rng, 8, 16);
    }
    std::thread::sleep(Duration::from_millis(500));
    let calls = calls_total(&mut t.app, false)?;
    let spoken = texts(&t.rig.mock.since(mark));
    let mut lat = Vec::with_capacity(n);
    let mut lost = 0u64;
    for (i, te) in emits.iter().enumerate() {
        let next = emits.get(i + 1).copied().unwrap_or(u64::MAX);
        match spoken
            .iter()
            .find(|(s, _)| *s >= *te && *s < next.saturating_add(50_000_000))
        {
            Some((s, _)) => lat.push(ms(s - te)),
            None => lost += 1,
        }
    }
    out.s("vr_ms", lat);
    out.x("vr_lost", lost as f64, "events");
    out.x("vr_failures", fails as f64, "steps");
    out.x("vr_ipc_calls", calls, "calls");
    out.notes.extend(notes);
    out.notes.push("VR: numpad review of a held terminal screen; vr_ipc_calls are application calls during the series (gate 0)".into());
    finish(&mut t, out)
}

// ---- VF: full-screen programs -----------------------------------------------------------------

/// VF: full-screen programs at a sensible level: a pager page is read, a cursor row moved by
/// Down is read, a repaint says nothing, an edit in the middle of the screen is echoed once.
pub fn vf(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    _size: Size,
    _seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "VF", out)? {
        return Ok(());
    }
    let mut t = setup(host, reader, flavor, 1, &[])?;
    focus_term(&mut t, PROMPT)?;
    let mut fails = 0u64;
    let mut notes: Vec<String> = Vec::new();
    // less: the program clears the screen and draws a page, a status line at the bottom.
    let page = |first: usize| {
        let mut s = String::from("\x1b[2J\x1b[H");
        for i in first..first + 22 {
            let _ = write!(s, "doc line {i:03} lorem ipsum\r\n");
        }
        s.push_str(":\x1b[K");
        s
    };
    feed(&mut t.app, &page(0))?;
    std::thread::sleep(Duration::from_millis(1500));
    let got = said_during(&mut t, 900, |t| {
        type_key(t, ' ')?;
        feed(&mut t.app, &page(22))?;
        Ok(())
    })?;
    // The key says "space"; the new page is read once.
    let pages: Vec<&String> = got.iter().filter(|s| s.contains("doc line 022")).collect();
    if pages.len() != 1 {
        fails += 1;
        notes.push(format!(
            "pager page: wanted the new page once, heard {got:?}"
        ));
    }
    std::thread::sleep(Duration::from_millis(2600));
    // A cursor row moved with Down (vim j, mc, less in a menu): the row is read.
    feed(&mut t.app, "\x1b[3;1H")?;
    std::thread::sleep(Duration::from_millis(300));
    let got = said_during(&mut t, 400, |t| {
        tap(t, DOWN, 0, 0)?;
        feed(&mut t.app, "\x1b[B")?;
        Ok(())
    })?;
    if got.len() != 1 || !got[0].contains("doc line 025") {
        fails += 1;
        notes.push(format!(
            "Down moves the cursor row: wanted row 4, heard {got:?}"
        ));
    }
    // htop: the numbers change in place on every refresh; the lines around stay.
    let top = |a: u32, b: u32, c: u32| {
        format!(
            "\x1b[2J\x1b[HCPU {a}%\r\nTasks: fixed one\r\nMEM {b}%\r\nTasks: fixed two\r\nSWP {c}%\r\nPID USER fixed three\r\n"
        )
    };
    feed(&mut t.app, &top(1, 2, 3))?;
    std::thread::sleep(Duration::from_millis(3000));
    let mark = t.rig.mock.mark();
    for i in 0..8u32 {
        feed(&mut t.app, &top(10 + i, 20 + i, 30 + i))?;
        std::thread::sleep(Duration::from_millis(400));
    }
    let said = texts(&t.rig.mock.since(mark));
    if !said.is_empty() {
        fails += 1;
        notes.push(format!("repaint: wanted silence, heard {said:?}"));
    }
    // nano: typing in the middle of the screen is echoed once.
    feed(
        &mut t.app,
        "\x1b[2J\x1b[Hfirst line\r\nsecond line\r\nthird",
    )?;
    std::thread::sleep(Duration::from_millis(3000));
    let mark = t.rig.mock.mark();
    for c in "xy".chars() {
        type_key(&mut t, c)?;
        feed(&mut t.app, &c.to_string())?;
        std::thread::sleep(Duration::from_millis(150));
    }
    std::thread::sleep(Duration::from_millis(400));
    let said: Vec<String> = texts(&t.rig.mock.since(mark))
        .into_iter()
        .map(|(_, s)| s)
        .collect();
    if said != ["x", "y"] {
        fails += 1;
        notes.push(format!(
            "edit in the middle: wanted x, y once each, heard {said:?}"
        ));
    }
    finish(&mut t, out)?;
    drop(t);
    // A maximised window: 60 rows of 200 columns, ASCII and Cyrillic (two bytes per scalar).
    let (big_fails, big_notes) = vf_big(host, reader, flavor, out)?;
    fails += big_fails;
    notes.extend(big_notes);
    out.x("vf_failures", fails as f64, "steps");
    out.notes.extend(notes);
    out.notes.push("VF: a pager page, a cursor row moved by Down, an htop-style repaint, an edit on a full screen, a 60x200 screen in ASCII and Cyrillic (caret mid-screen, Left)".into());
    Ok(())
}

/// m6-review finding 1: the caret and the rows of a screen larger than a few KiB. The terminal
/// shows 60 rows of 200 scalars with the cursor on row 20; focus must read that row, and Left
/// must read the character before the cursor in it. Returns the failed steps.
fn vf_big(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    out: &mut RunOut,
) -> Result<(u64, Vec<String>)> {
    let mut fails = 0u64;
    let mut notes = Vec::new();
    // (rows, columns, word of the row tag, fill, 0-based cursor column, character Left must read).
    // The third screen is 72 KB of UTF-8, more than the 64 KiB the connector used to keep.
    let cyr = "\u{441}\u{442}\u{440}\u{43e}\u{43a}\u{430}";
    for (rows, cols, word, fill, col, left) in [
        (60usize, 200usize, "row", 'x', 5usize, "2"),
        (60, 200, cyr, '\u{44f}', 8, "2"),
        (120, 300, cyr, '\u{44f}', 8, "2"),
    ] {
        let mut t = setup(
            host,
            reader,
            flavor,
            1,
            &[
                "--term-rows",
                &rows.to_string(),
                "--term-cols",
                &cols.to_string(),
            ],
        )?;
        let mut s = String::from("\x1b[2J\x1b[H");
        for r in 0..rows {
            let tag = format!("{word} {r:02} ");
            let pad = cols - tag.chars().count();
            let _ = write!(s, "{tag}{}", fill.to_string().repeat(pad));
            if r + 1 < rows {
                s.push_str("\r\n");
            }
        }
        let _ = write!(s, "\x1b[21;{}H", col + 1);
        out.x("vf_big_screen_bytes", s.len() as f64, "bytes");
        let said = focus_term(&mut t, &s)?;
        let want = format!("{word} 20 ");
        let others = (0..rows)
            .filter(|r| *r != 20)
            .any(|r| said.iter().any(|x| x.contains(&format!("{word} {r:02} "))));
        if !said.iter().any(|x| x.contains(&want)) || others {
            fails += 1;
            notes.push(format!(
                "big screen focus ({word}): wanted only the cursor row 20, heard {:?}",
                said.iter()
                    .map(|x| x.chars().take(24).collect::<String>())
                    .collect::<Vec<_>>()
            ));
        }
        let got = said_during(&mut t, 400, |t| {
            tap(t, LEFT, 0, 0)?;
            feed(&mut t.app, "\x1b[D")?;
            Ok(())
        })?;
        if got != [left] {
            fails += 1;
            notes.push(format!(
                "big screen Left ({word}): wanted {left:?}, heard {got:?}"
            ));
        }
        finish(&mut t, out)?;
    }
    Ok((fails, notes))
}

// ---- VV: a real gnome-terminal ----------------------------------------------------------------

/// A real `gnome-terminal-server` and its window, started in the private AT-SPI session by
/// `tests/term/run_real_vte.sh` (the compositor world and its RemoteDesktop keyboard are the
/// script's). Own process groups, signalled only through them.
struct RealTerm {
    server: std::process::Child,
    client: std::process::Child,
    rd: std::process::Child,
    /// A second application (its own server and window), started by `open_second_app`.
    extra: Vec<std::process::Child>,
}

impl RealTerm {
    fn start(host: &HostEnv) -> Result<Self> {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        let wayland = std::env::var("GORCA_VTE_WAYLAND").context("GORCA_VTE_WAYLAND not set")?;
        let cbus = std::env::var("GORCA_VTE_CBUS").context("GORCA_VTE_CBUS not set")?;
        let tools = std::env::var("GORCA_VTE_TOOLS").context("GORCA_VTE_TOOLS not set")?;
        if !host.a11y_addr.contains("/gorca-h.") {
            bail!("refusing to start a terminal outside a harness session");
        }
        let root = std::env::var("GORCA_HARNESS_DIR").context("GORCA_HARNESS_DIR not set")?;
        let log = |n: &str| std::fs::File::create(std::path::Path::new(&root).join(n));
        // A headless seat has no keyboard until a virtual one is used, and a window is told it
        // has the keyboard focus only then: the first key press comes before the window exists, so
        // the terminal is focused from the moment it is mapped, as on a real desktop.
        let mut rd = Command::new("/usr/bin/python3")
            .arg(std::path::Path::new(&tools).join("../web/m7/rd.py"))
            .env("GORCA_M7_CBUS", &cbus)
            .stdin(Stdio::piped())
            .stdout(Stdio::from(log("vte-rd.log")?))
            .stderr(Stdio::from(log("vte-rd.err")?))
            .process_group(0)
            .spawn()
            .context("starting rd.py")?;
        std::thread::sleep(Duration::from_millis(1500));
        {
            use std::io::Write;
            if let Some(stdin) = rd.stdin.as_mut() {
                let _ = writeln!(stdin, "key 42");
                let _ = stdin.flush();
            }
        }
        std::thread::sleep(Duration::from_millis(500));
        let server = Command::new("/usr/libexec/gnome-terminal-server")
            .args(["--app-id", "org.gnome.Terminal"])
            .env_remove("DISPLAY")
            .env("GDK_BACKEND", "wayland")
            .env("WAYLAND_DISPLAY", &wayland)
            .env("LANG", "C.UTF-8")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log("vte-server.log")?))
            .stderr(Stdio::from(log("vte-server.err")?))
            .process_group(0)
            .spawn()
            .context("starting gnome-terminal-server")?;
        // The server owns its bus name when the client can reach it.
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            let names = Command::new("busctl")
                .args([
                    "--address",
                    &host.session_addr,
                    "--no-pager",
                    "--acquired",
                    "list",
                ])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            if names.lines().any(|l| l.starts_with("org.gnome.Terminal ")) {
                break;
            }
            if Instant::now() > end {
                bail!("gnome-terminal-server never took org.gnome.Terminal");
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let client = Command::new("gnome-terminal")
            .args([
                "--app-id",
                "org.gnome.Terminal",
                "--window",
                "--geometry=100x30",
            ])
            .args(["--", "env", "PS1=$ ", "bash", "--norc", "--noprofile", "-i"])
            .env_remove("DISPLAY")
            .env("GDK_BACKEND", "wayland")
            .env("WAYLAND_DISPLAY", &wayland)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log("vte-client.log")?))
            .stderr(Stdio::from(log("vte-client.err")?))
            .process_group(0)
            .spawn()
            .context("starting gnome-terminal")?;
        Ok(Self {
            server,
            client,
            rd,
            extra: Vec::new(),
        })
    }

    fn rd_line(&mut self, line: &str) {
        use std::io::Write;
        if let Some(stdin) = self.rd.stdin.as_mut() {
            let _ = writeln!(stdin, "{line}");
            let _ = stdin.flush();
        }
    }

    /// A second terminal application (its own `gnome-terminal-server` under another application
    /// id, so another AT-SPI application) with one window running `script`. The new window takes
    /// the focus.
    fn open_second_app(&mut self, script: &str) -> Result<()> {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        const ID: &str = "org.gnome.TerminalTwo";
        let wayland = std::env::var("GORCA_VTE_WAYLAND").context("GORCA_VTE_WAYLAND not set")?;
        let root = std::env::var("GORCA_HARNESS_DIR").context("GORCA_HARNESS_DIR not set")?;
        let log = |n: &str| std::fs::File::create(std::path::Path::new(&root).join(n));
        let server = Command::new("/usr/libexec/gnome-terminal-server")
            .args(["--app-id", ID])
            .env_remove("DISPLAY")
            .env("GDK_BACKEND", "wayland")
            .env("WAYLAND_DISPLAY", &wayland)
            .env("LANG", "C.UTF-8")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log("vte2-server.log")?))
            .stderr(Stdio::from(log("vte2-server.err")?))
            .process_group(0)
            .spawn()
            .context("starting the second gnome-terminal-server")?;
        self.extra.push(server);
        std::thread::sleep(Duration::from_millis(1500));
        let client = Command::new("gnome-terminal")
            .args(["--app-id", ID, "--window", "--geometry=60x15"])
            .args([
                "--",
                "env",
                "PS1=$ ",
                "bash",
                "--norc",
                "--noprofile",
                "-c",
                script,
            ])
            .env_remove("DISPLAY")
            .env("GDK_BACKEND", "wayland")
            .env("WAYLAND_DISPLAY", &wayland)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log("vte2-client.log")?))
            .stderr(Stdio::from(log("vte2-client.err")?))
            .process_group(0)
            .spawn()
            .context("starting the second gnome-terminal")?;
        self.extra.push(client);
        Ok(())
    }

    /// A new tab of the first application's window running `script`. It takes the focus.
    fn open_tab(&mut self, script: &str) -> Result<()> {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        let wayland = std::env::var("GORCA_VTE_WAYLAND").context("GORCA_VTE_WAYLAND not set")?;
        let root = std::env::var("GORCA_HARNESS_DIR").context("GORCA_HARNESS_DIR not set")?;
        let log = |n: &str| std::fs::File::create(std::path::Path::new(&root).join(n));
        let client = Command::new("gnome-terminal")
            .args(["--app-id", "org.gnome.Terminal", "--tab"])
            .args([
                "--",
                "env",
                "PS1=$ ",
                "bash",
                "--norc",
                "--noprofile",
                "-c",
                script,
            ])
            .env_remove("DISPLAY")
            .env("GDK_BACKEND", "wayland")
            .env("WAYLAND_DISPLAY", &wayland)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log("vte-tab-client.log")?))
            .stderr(Stdio::from(log("vte-tab-client.err")?))
            .process_group(0)
            .spawn()
            .context("starting gnome-terminal for a tab")?;
        self.extra.push(client);
        Ok(())
    }

    fn server_pid(&self) -> u32 {
        self.server.id()
    }
}

impl Drop for RealTerm {
    fn drop(&mut self) {
        drop(self.rd.stdin.take());
        for c in [&mut self.rd, &mut self.client, &mut self.server]
            .into_iter()
            .chain(self.extra.iter_mut())
        {
            let group = format!("-{}", c.id());
            let _ = std::process::Command::new("kill")
                .args(["-TERM", "--", &group])
                .status();
        }
        std::thread::sleep(Duration::from_millis(500));
        for c in [&mut self.rd, &mut self.client, &mut self.server]
            .into_iter()
            .chain(self.extra.iter_mut())
        {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

struct V {
    rig: Rig,
    term: RealTerm,
    ty: Typist,
}

/// evdev code of a key the real terminal is typed with.
fn evdev_of(c: char) -> Option<u32> {
    const ROWS: [(&str, u32); 4] = [
        ("qwertyuiop", 16),
        ("asdfghjkl", 30),
        ("zxcvbnm", 44),
        ("1234567890", 2),
    ];
    for (row, base) in ROWS {
        if let Some(i) = row.find(c) {
            return Some(base + u32::try_from(i).ok()?);
        }
    }
    match c {
        ' ' => Some(57),
        '-' => Some(12),
        '.' => Some(52),
        '/' => Some(53),
        ';' => Some(39),
        _ => None,
    }
}

/// The evdev code of the key that types `c` with Shift held (US layout).
fn shifted_of(c: char) -> Option<u32> {
    match c {
        ':' => Some(39),
        '|' => Some(43),
        _ => None,
    }
}

impl V {
    /// A key the reader sees (the emulated Mutter) and the terminal gets (the compositor's
    /// RemoteDesktop keyboard), the reader's first, as in a real compositor. Returns the emit time.
    fn press(&mut self, k: K, uni: u32, state: u32, evdev: &[u32]) -> Result<u64> {
        self.ty.t_ms += 3;
        let m = self.rig.mutter.as_mut().context("no mutter")?;
        let down = m.key_full(k.code, k.sym, uni, state, false, self.ty.t_ms)?;
        self.ty.t_ms += 3;
        m.key_full(k.code, k.sym, 0, state, true, self.ty.t_ms)?;
        let list: Vec<String> = evdev.iter().map(u32::to_string).collect();
        self.term.rd_line(&format!("key {}", list.join(" ")));
        Ok(down.t_emit_ns)
    }

    fn type_char(&mut self, c: char) -> Result<u64> {
        let e = evdev_of(c).with_context(|| format!("no evdev code for {c:?}"))?;
        self.press(letter_key(c), c as u32, 0, &[e])
    }

    fn ctrl_c(&mut self) -> Result<u64> {
        self.chord(&[CTRL_L], KEY_C, 3, CONTROL, &[29, 46])
    }

    /// `k` with the modifiers `mods` held (pressed before, released after), as the reader and the
    /// terminal see it; `evdev` lists every key of the chord for the compositor's keyboard.
    fn chord(&mut self, mods: &[K], k: K, uni: u32, state: u32, evdev: &[u32]) -> Result<u64> {
        for m in mods {
            self.ty.t_ms += 3;
            let mutter = self.rig.mutter.as_mut().context("no mutter")?;
            mutter.key_full(m.code, m.sym, 0, 0, false, self.ty.t_ms)?;
        }
        let r = self.press(k, uni, state, evdev)?;
        for m in mods.iter().rev() {
            self.ty.t_ms += 3;
            let mutter = self.rig.mutter.as_mut().context("no mutter")?;
            mutter.key_full(m.code, m.sym, 0, 0, true, self.ty.t_ms)?;
        }
        Ok(r)
    }

    /// Type `text` (lower case, digits and the few symbols `evdev_of` and `shifted_of` know), one
    /// key every 60 ms.
    fn type_text(&mut self, text: &str) -> Result<Vec<u64>> {
        let mut at = Vec::with_capacity(text.len());
        for ch in text.chars() {
            let t = if let Some(e) = shifted_of(ch) {
                self.chord(&[SHIFT_L], letter_key(ch), ch as u32, SHIFT, &[42, e])?
            } else {
                self.type_char(ch)?
            };
            at.push(t);
            std::thread::sleep(Duration::from_millis(60));
        }
        Ok(at)
    }

    fn said_during(
        &mut self,
        wait_ms: u64,
        act: impl FnOnce(&mut V) -> Result<()>,
    ) -> Result<Vec<String>> {
        let mark = self.rig.mock.mark();
        act(self)?;
        std::thread::sleep(Duration::from_millis(wait_ms));
        Ok(texts(&self.rig.mock.since(mark))
            .into_iter()
            .map(|(_, s)| s)
            .collect())
    }
}

/// VV: the reader against a REAL gnome-terminal-server (VTE 0.80): scripted readline session,
/// then `yes` and `seq` floods and Ctrl+C, in the private headless session
/// (`tests/term/run_real_vte.sh target/release/e2e run --only VV ...`, `GORCA_M6_REAL=1`).
pub fn vv(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    _seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "VV", out)? {
        return Ok(());
    }
    if !(flag(host, "GORCA_M6_REAL") && std::env::var_os("GORCA_VTE_WAYLAND").is_some()) {
        if flag(host, "GORCA_M6_STRICT") {
            bail!(
                "VV: GORCA_M6_REAL=1 and the private compositor (tests/term/run_real_vte.sh) are required"
            );
        }
        out.notes
            .push("VV SKIPPED: needs GORCA_M6_REAL=1 and tests/term/run_real_vte.sh".to_owned());
        out.x("skipped", 1.0, "flag");
        return Ok(());
    }
    // The reader logs at trace level into a file of its own: the password must not appear in it.
    let root = std::env::var("GORCA_HARNESS_DIR").context("GORCA_HARNESS_DIR not set")?;
    let err_path = std::path::Path::new(&root).join("vv-reader.stderr");
    let present_path = std::path::Path::new(&root).join("vv-present.jsonl");
    let mut h = m1_env(host);
    h.reader_env.retain(|(k, _)| k != "RUST_LOG");
    h.reader_env
        .push(("RUST_LOG".to_owned(), "trace".to_owned()));
    let mut o = super::scenarios::base_opts(reader, flavor);
    o.keys = true;
    o.reader_stderr = Some(err_path.clone());
    o.extra_reader_args = vec![format!("--bench-present-log={}", present_path.display())];
    let rig = Rig::start(&h, &o)?;
    if !rig.keys_ok {
        bail!("the reader did not register with the emulated keyboard monitor (no SetKeyGrabs)");
    }
    rig.mock
        .configure(Duration::from_millis(1), Duration::from_millis(5));
    let term = RealTerm::start(host)?;
    let mut v = V {
        rig,
        term,
        ty: Typist::new(),
    };
    // The window needs a click to be active, and the first key after it can be lost.
    std::thread::sleep(Duration::from_secs(4));
    v.term.rd_line("click");
    std::thread::sleep(Duration::from_secs(1));
    let mark = v.rig.mock.mark();
    v.press(
        K {
            code: 110,
            sym: 0xff50,
        },
        0,
        0,
        &[102],
    )?;
    std::thread::sleep(Duration::from_millis(1500));
    if texts(&v.rig.mock.since(mark)).is_empty() && v.rig.mock.since(0).is_empty() {
        bail!("the reader never spoke: the terminal did not come up");
    }
    let mut c = Check {
        failures: 0,
        notes: Vec::new(),
    };
    const W: u64 = 400;
    let mut probes: Vec<Probe> = Vec::new();
    let mark_all = v.rig.mock.mark();
    // Typing: each key exactly once.
    for ch in "echo hello".chars() {
        let t_key = v.type_char(ch)?;
        probes.push(Probe {
            t_key,
            expect: name_of(ch),
            what: "type",
            contains: false,
        });
        std::thread::sleep(Duration::from_millis(70));
    }
    std::thread::sleep(Duration::from_millis(300));
    let spoken = texts(&v.rig.mock.since(mark_all));
    let (lat, missing, double, extra, notes) = score(&probes, &spoken);
    out.notes.extend(notes);
    out.s("vv_key_ms", lat);
    out.x("vv_type_missing", missing as f64, "events");
    out.x("vv_type_double", double as f64, "events");
    out.x("vv_type_extra", extra as f64, "events");
    let got = v.said_during(W, |v| {
        v.press(LEFT, 0, 0, &[105])?;
        Ok(())
    })?;
    c.step("Left", &["o"], &got);
    let got = v.said_during(W, |v| {
        v.press(LEFT, 0, 0, &[105])?;
        Ok(())
    })?;
    c.step("Left again", &["l"], &got);
    let got = v.said_during(W, |v| {
        v.press(
            K {
                code: 110,
                sym: 0xff50,
            },
            0,
            0,
            &[102],
        )?;
        Ok(())
    })?;
    c.step("Home", &["e"], &got);
    let got = v.said_during(W, |v| {
        v.press(
            K {
                code: 115,
                sym: 0xff57,
            },
            0,
            0,
            &[107],
        )?;
        Ok(())
    })?;
    c.step("End", &["blank"], &got);
    let got = v.said_during(900, |v| {
        v.press(ENTER, 13, 0, &[28])?;
        Ok(())
    })?;
    c.step(
        "Enter",
        &["~hello"],
        &got.iter()
            .map(|s| s.lines().next().unwrap_or("").to_owned())
            .collect::<Vec<_>>(),
    );
    std::thread::sleep(Duration::from_millis(1200));
    let got = v.said_during(W, |v| {
        v.press(UP, 0, 0, &[103])?;
        Ok(())
    })?;
    c.step("Up: history", &["echo hello"], &got);
    let got = v.said_during(W, |v| {
        v.press(DOWN, 0, 0, &[108])?;
        Ok(())
    })?;
    c.step("Down: empty line", &["blank"], &got);
    for ch in "ec".chars() {
        v.type_char(ch)?;
        std::thread::sleep(Duration::from_millis(70));
    }
    std::thread::sleep(Duration::from_millis(200));
    let got = v.said_during(W, |v| {
        v.press(TAB, 9, 0, &[15])?;
        Ok(())
    })?;
    c.step("Tab completes", &["echo"], &got);
    let got = v.said_during(W, |v| {
        v.press(BACKSPACE, 8, 0, &[14])?;
        Ok(())
    })?;
    c.step("BackSpace removes the completion's blank", &["space"], &got);
    let got = v.said_during(W, |v| {
        v.chord(&[CTRL_L], KEY_U, 21, CONTROL, &[29, 22])?;
        Ok(())
    })?;
    c.step("Ctrl+U: the deleted text", &["echo"], &got);
    out.x("vv_failures", c.failures as f64, "steps");
    out.notes.extend(std::mem::take(&mut c.notes));
    let failures_before = c.failures;
    vv_password(&mut v, &mut c, out, &[&err_path, &present_path])?;
    vv_pager(&mut v, &mut c)?;
    vv_tabs(&mut v, &mut c)?;
    let before_focus = c.failures;
    vv_focus(&mut v, &mut c)?;
    out.x(
        "vv_focus_failures",
        (c.failures - before_focus) as f64,
        "steps",
    );
    out.x(
        "vv_screen_failures",
        (c.failures - failures_before) as f64,
        "steps",
    );
    out.notes.extend(std::mem::take(&mut c.notes));
    // Keep the reader's trace and the presentation log for a diagnosis (`GORCA_VV_KEEP_DIR`).
    if let Some(dir) = std::env::var_os("GORCA_VV_KEEP_DIR") {
        let dir = std::path::PathBuf::from(dir);
        let _ = std::fs::create_dir_all(&dir);
        for f in [&err_path, &present_path] {
            if let Some(name) = f.file_name() {
                let _ = std::fs::copy(f, dir.join(name));
            }
        }
    }
    if flag(host, "GORCA_VV_NO_FLOOD") {
        out.notes
            .push("VV: floods skipped (GORCA_VV_NO_FLOOD)".into());
        return Ok(());
    }

    // Floods in the real terminal.
    let pid = v.term.server_pid();
    let secs = env_num(host, "GORCA_V2_SECS").unwrap_or(if size.n >= 1200 { 20 } else { 6 });
    // Utterances long enough to be in flight when Ctrl+C comes.
    v.rig
        .mock
        .configure(Duration::from_millis(1), Duration::from_millis(600));
    let _ = pid;
    let reader_pid = v.rig.reader.pid;
    let mut worst_cpu = 0.0f64;
    let mut worst_q = 0u64;
    let mut stop_lat = Vec::new();
    let mut key_lat = Vec::new();
    let mut retries = 0u32;
    for (name, cmd) in [("yes", "yes"), ("seq", "seq 1 100000000")] {
        // ctrl-u clears whatever is on the line
        v.term.rd_line("key 29 22");
        std::thread::sleep(Duration::from_millis(300));
        for ch in cmd.chars() {
            if ch == ' ' || ch.is_ascii_alphanumeric() {
                v.type_char(ch)?;
                std::thread::sleep(Duration::from_millis(60));
            }
        }
        let sampler = Sampler::start(reader_pid);
        let mark = v.rig.mock.mark();
        let (c0, t0) = (v.rig.reader.cpu_ns(), super::mono::now_ns());
        v.press(ENTER, 13, 0, &[28])?;
        std::thread::sleep(Duration::from_millis(secs * 600));
        let t_quiet = super::mono::now_ns();
        // Keys typed into the flood.
        let end = Instant::now() + Duration::from_millis(secs * 400);
        let mut rng = Rng::new(11);
        let mut keyed = Vec::new();
        while Instant::now() < end {
            std::thread::sleep(Duration::from_millis(300 + rng.below(200)));
            let ch = (b'a' + rng.below(26) as u8) as char;
            let t_key = v.type_char(ch)?;
            keyed.push(Probe {
                t_key,
                expect: name_of(ch),
                what: "type",
                contains: false,
            });
        }
        let (c1, t1) = (v.rig.reader.cpu_ns(), super::mono::now_ns());
        let q = sampler.finish();
        let wall = (t1 - t0) as f64;
        let cpu = c1.saturating_sub(c0) as f64 / wall * 100.0;
        // Ctrl+C with an utterance in flight (the flood is spoken now and then).
        let m2 = v.rig.mock.mark();
        let t_key = v.ctrl_c()?;
        let cancel = v.rig.mock.wait_for(m2, Duration::from_millis(400), |r| {
            matches!(
                &r.kind,
                Kind::Cancel {
                    had_speech: true,
                    ..
                }
            )
        });
        if let Some(r) = cancel {
            stop_lat.push(ms(r.t_ns.saturating_sub(t_key)));
        }
        std::thread::sleep(Duration::from_millis(2000));
        // The compositor's virtual keyboard now and then drops a key (seen with `yes`, `seq`):
        // if the flood is still going the reader is rightly silent after Ctrl+C, so press again once.
        let said_dollar = |v: &V| {
            texts(&v.rig.mock.since(mark))
                .iter()
                .any(|(ts, s)| *ts > t_key && (s.contains("dollar") || s.contains('$')))
        };
        let mut t_key = t_key;
        if !said_dollar(&v) {
            retries += 1;
            t_key = v.ctrl_c()?;
            std::thread::sleep(Duration::from_millis(2000));
        }
        let recs = v.rig.mock.since(mark);
        let spoken = texts(&recs);
        let quiet = spoken
            .iter()
            .filter(|(ts, _)| *ts >= t0 && *ts < t_quiet)
            .count();
        let (lat, missing, _, _, _) = score(&keyed, &spoken);
        key_lat.extend(lat);
        let prompt_said = spoken
            .iter()
            .any(|(ts, s)| *ts > t_key && (s.contains("dollar") || s.contains('$')));
        if !prompt_said {
            let after: Vec<String> = spoken
                .iter()
                .filter(|(ts, _)| *ts > t_key)
                .take(6)
                .map(|(_, s)| s.chars().take(60).collect())
                .collect();
            out.notes.push(format!(
                "VV {name}: no prompt after Ctrl+C, heard {after:?}"
            ));
        }
        out.x(&format!("vv_{name}_cpu_pct"), cpu, "% of one core");
        out.x(
            &format!("vv_{name}_speaks_per_s"),
            quiet as f64 / ((t_quiet - t0) as f64 / 1e9),
            "1/s",
        );
        out.x(
            &format!("vv_{name}_siocinq_p99_kib"),
            p99_u64(&q) as f64 / 1024.0,
            "KiB",
        );
        out.x(&format!("vv_{name}_key_lost"), missing as f64, "events");
        out.x(
            &format!("vv_{name}_prompt_missing"),
            f64::from(u8::from(!prompt_said)),
            "events",
        );
        out.x(
            &format!("vv_{name}_flood_unspoken"),
            f64::from(u8::from(quiet == 0)),
            "events",
        );
        worst_cpu = worst_cpu.max(cpu);
        worst_q = worst_q.max(p99_u64(&q));
        if !v.rig.reader.alive() {
            bail!("the reader died during the {name} flood");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    out.x("vv_ctrlc_retries", f64::from(retries), "presses");
    out.x("vv_worst_cpu_pct", worst_cpu, "% of one core");
    out.x("vv_siocinq_p99_kib", worst_q as f64 / 1024.0, "KiB");
    out.s("vv_flood_key_ms", key_lat);
    out.s("vv_stop_ms", stop_lat);
    out.notes.push(
        "VV: a real gnome-terminal-server (VTE 0.80) in a private headless session; keys go to the reader through the emulated Mutter and to the terminal through the compositor's RemoteDesktop keyboard; floods are `yes` and `seq 1 100000000`".into(),
    );
    // T-3: the reader never made the real server abort (a negative offset is a g_assert there).
    let aborted = !matches!(v.term.server.try_wait(), Ok(None));
    out.x("vv_server_aborts", f64::from(u8::from(aborted)), "events");
    let server_err = std::fs::read_to_string(std::path::Path::new(&root).join("vte-server.err"))
        .unwrap_or_default();
    out.x(
        "vv_server_criticals",
        server_err.matches("CRITICAL").count() as f64,
        "lines",
    );
    if !v.rig.reader.alive() {
        bail!("the reader died");
    }
    Ok(())
}

/// Any utterance contains `needle`.
fn heard(got: &[String], needle: &str) -> bool {
    got.iter().any(|g| g.contains(needle))
}

impl Check {
    fn expect(&mut self, name: &str, ok: bool, got: &[String]) {
        if !ok {
            self.failures += 1;
            if self.notes.len() < 12 {
                let short: Vec<String> = got.iter().map(|s| s.chars().take(80).collect()).collect();
                self.notes.push(format!("VV {name}: heard {short:?}"));
            }
        }
    }
}

/// VV password: a real no-echo prompt (`read -s`, the shape of sudo's) is spoken, every typed
/// key is spoken once, and the secret is in neither the presentation log nor the reader's
/// trace-level stderr.
fn vv_password(
    v: &mut V,
    c: &mut Check,
    out: &mut RunOut,
    logs: &[&std::path::Path],
) -> Result<()> {
    v.type_text("read -s -p password: x")?;
    std::thread::sleep(Duration::from_millis(300));
    let got = v.said_during(900, |v| {
        v.press(ENTER, 13, 0, &[28])?;
        Ok(())
    })?;
    let prompt_ok = heard(&got, "password");
    c.expect("password prompt", prompt_ok, &got);
    out.x(
        "vv_password_prompt_missing",
        f64::from(u8::from(!prompt_ok)),
        "events",
    );
    let secret = "jq7xk0wz4vy3";
    let from: Vec<usize> = logs
        .iter()
        .map(|p| std::fs::read_to_string(p).map_or(0, |h| h.len()))
        .collect();
    let mark = v.rig.mock.mark();
    let mut probes = Vec::new();
    for ch in secret.chars() {
        let t_key = v.type_char(ch)?;
        probes.push(Probe {
            t_key,
            expect: name_of(ch),
            what: "type",
            contains: false,
        });
        std::thread::sleep(Duration::from_millis(80));
    }
    std::thread::sleep(Duration::from_millis(300));
    let spoken = texts(&v.rig.mock.since(mark));
    let (_, missing, double, extra, notes) = score(&probes, &spoken);
    out.notes.extend(notes);
    out.x("vv_password_echo_missing", missing as f64, "events");
    out.x("vv_password_echo_double", double as f64, "events");
    out.x("vv_password_echo_extra", extra as f64, "events");
    let got = v.said_during(900, |v| {
        v.press(ENTER, 13, 0, &[28])?;
        Ok(())
    })?;
    c.expect(
        "prompt after the password",
        heard(&got, "dollar") || heard(&got, "$"),
        &got,
    );
    std::thread::sleep(Duration::from_millis(500));
    let mut hits = 0u64;
    let mut empty = 0u64;
    for (p, from) in logs.iter().zip(from) {
        let hay = std::fs::read_to_string(p).unwrap_or_default();
        empty += u64::from(hay.is_empty());
        hits += secret_leaks(&hay, hay.get(from..).unwrap_or(""), secret);
    }
    out.x("vv_password_plaintext_in_logs", hits as f64, "occurrences");
    // A log that was never written would make the plaintext check vacuous.
    out.x("vv_password_log_empty", empty as f64, "files");
    Ok(())
}

/// VV pager: `less` over numbered lines in the real terminal (alternate screen, full repaints):
/// a page forward reads the new page, a page back the old one, `q` returns to the prompt.
fn vv_pager(v: &mut V, c: &mut Check) -> Result<()> {
    v.type_text("seq 1000 1999 | less")?;
    std::thread::sleep(Duration::from_millis(300));
    let got = v.said_during(1200, |v| {
        v.press(ENTER, 13, 0, &[28])?;
        Ok(())
    })?;
    c.expect("less opens on the first page", heard(&got, "1000"), &got);
    let got = v.said_during(1200, |v| {
        v.type_char(' ')?;
        Ok(())
    })?;
    c.expect(
        "Space: the next page",
        heard(&got, "1040") && !heard(&got, "1000"),
        &got,
    );
    let got = v.said_during(1200, |v| {
        v.type_char('b')?;
        Ok(())
    })?;
    c.expect("b: the page before", heard(&got, "1010"), &got);
    let got = v.said_during(1200, |v| {
        v.press(DOWN, 0, 0, &[108])?;
        Ok(())
    })?;
    c.expect(
        "Down: the new bottom row",
        got.len() == 1 && heard(&got, "10"),
        &got,
    );
    let got = v.said_during(1200, |v| {
        v.type_char('q')?;
        Ok(())
    })?;
    c.expect(
        "q: back at the prompt",
        heard(&got, "dollar") || heard(&got, "$"),
        &got,
    );
    Ok(())
}

/// VV tabs: a new tab and a switch back each read the tab's cursor line.
fn vv_tabs(v: &mut V, c: &mut Check) -> Result<()> {
    let got = v.said_during(2500, |v| {
        v.chord(&[CTRL_L, SHIFT_L], KEY_T, 0, CONTROL | SHIFT, &[29, 42, 20])?;
        Ok(())
    })?;
    c.expect(
        "Ctrl+Shift+T: the new tab's prompt",
        heard(&got, "dollar") || heard(&got, "$"),
        &got,
    );
    let got = v.said_during(1500, |v| {
        v.chord(&[CTRL_L], PAGE_UP, 0, CONTROL, &[29, 104])?;
        Ok(())
    })?;
    c.expect(
        "Ctrl+PgUp: the first tab's prompt",
        heard(&got, "dollar") || heard(&got, "$"),
        &got,
    );
    Ok(())
}

/// VV focus: output that arrives after a terminal got the focus and before any key is spoken.
/// Nothing in these steps reaches the reader as a key (the compositor's keyboard only, or no key
/// at all), so only the focus and window events of the application can route the terminal's text
/// events to it. The terminals are focused for the first time (a new window of another
/// application, a new tab), the first presentation of such a node is read from the application
/// and a toolkit can still answer without `FOCUSED` there (the real VTE does).
fn vv_focus(v: &mut V, c: &mut Check) -> Result<()> {
    const ALT_TAB: &str = "key 56 15";
    // A new window of another application takes the focus and prints after a while.
    let got = v.said_during(11_000, |v| {
        v.term
            .open_second_app("sleep 9 ; echo focusa ; exec bash --norc -i")
    })?;
    c.expect(
        "a new window: its output before any key",
        heard(&got, "focusa"),
        &got,
    );
    // A new tab of the first window: the same, in the same application as the old tab.
    let got = v.said_during(11_000, |v| {
        v.term
            .open_tab("sleep 9 ; echo focusb ; exec bash --norc -i")
    })?;
    c.expect(
        "a new tab: its output before any key",
        heard(&got, "focusb"),
        &got,
    );
    // Back to the other application with Alt+Tab, and the old tab of the first window with
    // Ctrl+PgUp: both are cached nodes now. The shell prints when told to by a key typed to the
    // compositor only.
    let quiet = |v: &mut V, text: &str| {
        for ch in text.chars() {
            if let Some(e) = evdev_of(ch) {
                v.term.rd_line(&format!("key {e}"));
                std::thread::sleep(Duration::from_millis(60));
            }
        }
        v.term.rd_line("key 28");
    };
    let got = v.said_during(2500, |v| {
        v.term.rd_line(ALT_TAB);
        Ok(())
    })?;
    c.expect("Alt+Tab is spoken", !got.is_empty(), &got);
    quiet(v, "sleep 8 ; echo focusc");
    std::thread::sleep(Duration::from_millis(500));
    v.term.rd_line(ALT_TAB);
    std::thread::sleep(Duration::from_millis(2000));
    let got = v.said_during(9000, |v| {
        v.term.rd_line(ALT_TAB);
        Ok(())
    })?;
    c.expect(
        "Alt+Tab back: output before any key",
        heard(&got, "focusc"),
        &got,
    );
    Ok(())
}

// ---- VS: flat review of a terminal that keeps printing ----------------------------------------

/// The number a `tick N` or `burst N j` line of the VS loop carries.
fn vs_count(line: &str) -> Option<u64> {
    let mut words = line.split_whitespace();
    match words.next()? {
        "tick" | "burst" => words.next()?.parse().ok(),
        _ => None,
    }
}

/// How many ticks the VS reading may trail the loop by: the settle, the verification period of
/// the screen and the review key itself, with room to spare. A stuck snapshot falls behind by
/// ten ticks a second, without bound.
const VS_SLACK_TICKS: u64 = 40;

impl V {
    /// A key only the reader sees (the emulated Mutter): a review key must not reach the terminal.
    fn reader_key(&mut self, k: K) -> Result<u64> {
        self.ty.t_ms += 3;
        let m = self.rig.mutter.as_mut().context("no mutter")?;
        let down = m.key_full(k.code, k.sym, 0, 0, false, self.ty.t_ms)?;
        self.ty.t_ms += 3;
        m.key_full(k.code, k.sym, 0, 0, true, self.ty.t_ms)?;
        Ok(down.t_emit_ns)
    }
}

/// VS: a real gnome-terminal runs a loop that prints a numbered line every 100 ms and a burst of
/// lines every few seconds; the user presses Alt+Tab away and back and reads the screen with the
/// numpad review keys, again and again. The line review speaks is always the screen's current
/// one: its number is never more than `VS_SLACK_TICKS` behind the loop's own counter.
/// `GORCA_VS_SECS` sets the length (default 240 s, 45 s with `--quick`).
pub fn vs(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    _seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "VS", out)? {
        return Ok(());
    }
    if !(flag(host, "GORCA_M6_REAL") && std::env::var_os("GORCA_VTE_WAYLAND").is_some()) {
        if flag(host, "GORCA_M6_STRICT") {
            bail!(
                "VS: GORCA_M6_REAL=1 and the private compositor (tests/term/run_real_vte.sh) are required"
            );
        }
        out.notes
            .push("VS SKIPPED: needs GORCA_M6_REAL=1 and tests/term/run_real_vte.sh".to_owned());
        out.x("skipped", 1.0, "flag");
        return Ok(());
    }
    const ALT_TAB: &str = "key 56 15";
    let root = std::env::var("GORCA_HARNESS_DIR").context("GORCA_HARNESS_DIR not set")?;
    let counter = std::path::Path::new(&root).join("vs-n");
    let script = std::path::Path::new(&root).join("vs-loop.sh");
    std::fs::write(
        &script,
        format!(
            "i=0\nwhile :; do\n  i=$((i+1))\n  echo \"tick $i\"\n  echo $i > '{c}'\n  if [ $((i % 37)) = 0 ]; then\n    for j in $(seq 1 120); do echo \"burst $i $j\"; done\n  fi\n  sleep 0.1\ndone\n",
            c = counter.display()
        ),
    )?;
    let mut h = m1_env(host);
    h.reader_env.retain(|(k, _)| k != "RUST_LOG");
    h.reader_env
        .push(("RUST_LOG".to_owned(), "gorca_core=debug".to_owned()));
    let err_path = std::path::Path::new(&root).join("vs-reader.stderr");
    let mut o = super::scenarios::base_opts(reader, flavor);
    o.keys = true;
    o.reader_stderr = Some(err_path.clone());
    let rig = Rig::start(&h, &o)?;
    if !rig.keys_ok {
        bail!("the reader did not register with the emulated keyboard monitor (no SetKeyGrabs)");
    }
    rig.mock
        .configure(Duration::from_millis(1), Duration::from_millis(5));
    let term = RealTerm::start(host)?;
    let mut v = V {
        rig,
        term,
        ty: Typist::new(),
    };
    std::thread::sleep(Duration::from_secs(4));
    v.term.rd_line("click");
    std::thread::sleep(Duration::from_secs(1));
    // The loop runs in a second application: Alt+Tab goes between it and the first one.
    v.term
        .open_second_app(&format!("bash {}", script.display()))?;
    std::thread::sleep(Duration::from_secs(5));
    let secs = env_num(host, "GORCA_VS_SECS").unwrap_or(if size.n >= 1200 { 240 } else { 45 });
    let truth = || -> u64 {
        std::fs::read_to_string(&counter)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    };
    let mut failures = 0u64;
    let mut reads = 0u64;
    let mut silent = 0u64;
    let mut worst_lag = 0u64;
    let mut notes: Vec<String> = Vec::new();
    // One review read: the line the key speaks and how far behind the loop it is.
    let mut probe = |v: &mut V, what: &str, k: K, numbered: bool| -> Result<()> {
        let before = truth();
        let mut got = v.said_during(700, |v| v.reader_key(k).map(|_| ()))?;
        if got.is_empty() {
            // The first key after a focus change can be lost: once more.
            got = v.said_during(700, |v| v.reader_key(k).map(|_| ()))?;
        }
        reads += 1;
        let n = got.iter().filter_map(|g| vs_count(g)).max();
        match n {
            None if !numbered => {}
            None => {
                silent += 1;
                if notes.len() < 10 {
                    notes.push(format!("VS {what}: no numbered line heard: {got:?}"));
                }
            }
            Some(n) => {
                let lag = before.saturating_sub(n);
                worst_lag = worst_lag.max(lag);
                if lag > VS_SLACK_TICKS {
                    failures += 1;
                    if notes.len() < 10 {
                        notes.push(format!(
                            "VS {what}: read line {n}, the loop was at {before} ({lag} ticks behind): {got:?}"
                        ));
                    }
                }
            }
        }
        Ok(())
    };
    let end = Instant::now() + Duration::from_secs(secs);
    let mut cycles = 0u64;
    while Instant::now() < end {
        cycles += 1;
        // Away and back.
        v.term.rd_line(ALT_TAB);
        std::thread::sleep(Duration::from_millis(1500));
        v.term.rd_line(ALT_TAB);
        std::thread::sleep(Duration::from_millis(2000));
        // Review starts at the caret line; the line above it is the newest one printed.
        probe(&mut v, "current line", KP_CUR_LINE, false)?;
        probe(&mut v, "previous line", KP_PREV_LINE, true)?;
        // Still in review while the loop prints on: the same line index reads newer text.
        std::thread::sleep(Duration::from_secs(4));
        probe(&mut v, "same line later", KP_CUR_LINE, true)?;
        std::thread::sleep(Duration::from_secs(3));
        probe(&mut v, "line above later", KP_PREV_LINE, true)?;
        if !v.rig.reader.alive() {
            bail!("the reader died during VS");
        }
    }
    out.x("vs_stale", failures as f64, "reads");
    out.x("vs_silent", silent as f64, "reads");
    out.x("vs_reads", reads as f64, "reads");
    out.x("vs_cycles", cycles as f64, "cycles");
    out.x("vs_worst_lag_ticks", worst_lag as f64, "ticks");
    out.notes.extend(notes);
    out.notes.push(
        "VS: flat review of a real gnome-terminal that keeps printing, Alt+Tab away and back between reads (vs_stale gate 0)".into(),
    );
    if let Some(dir) = std::env::var_os("GORCA_VV_KEEP_DIR") {
        let dir = std::path::PathBuf::from(dir);
        let _ = std::fs::create_dir_all(&dir);
        if let Some(name) = err_path.file_name() {
            let _ = std::fs::copy(&err_path, dir.join(name));
        }
    }
    Ok(())
}

// ---- VD: the terminal must not degrade over a long session -------------------------------------

const ALT_L: K = K {
    code: 64,
    sym: 0xffe9,
};
const ALT: u32 = 8;
const ESCAPE: K = K {
    code: 9,
    sym: 0xff1b,
};
const PAGE_DOWN: K = K {
    code: 117,
    sym: 0xff56,
};

/// Key names the generic echo speaks for a key the terminal layer must own (Tab, BackSpace, Enter
/// in a tracked terminal are never spoken as names: a name means the terminal is not tracked).
fn key_name_spoken(got: &[String]) -> Option<&str> {
    got.iter().map(String::as_str).find(|s| {
        ["tab", "backspace", "enter", "return"].contains(&s.to_ascii_lowercase().as_str())
    })
}

impl Check {
    /// A VD step: `ok` and no key name among what was said.
    fn vd(&mut self, name: &str, ok: bool, got: &[String]) {
        let leak = key_name_spoken(got).is_some();
        let label = if leak {
            format!("{name} (a key NAME was spoken)")
        } else {
            name.to_owned()
        };
        self.expect(&label, ok && !leak, got);
    }
}

/// One cycle of ordinary bash use. Every step must read what it did and none may speak a key name.
fn vd_cycle(v: &mut V, c: &mut Check, tag: &str) -> Result<()> {
    let typed = format!("echo {tag}");
    let got = v.said_during(300, |v| v.type_text(&typed).map(|_| ()))?;
    c.vd(
        &format!("{tag} typing is read"),
        got.len() + 2 >= typed.len(),
        &got,
    );
    let got = v.said_during(900, |v| v.press(ENTER, 13, 0, &[28]).map(|_| ()))?;
    c.vd(&format!("{tag} Enter: the output"), heard(&got, tag), &got);
    std::thread::sleep(Duration::from_millis(600));
    let got = v.said_during(500, |v| v.press(UP, 0, 0, &[103]).map(|_| ()))?;
    c.vd(&format!("{tag} Up: history"), heard(&got, &typed), &got);
    let got = v.said_during(500, |v| v.press(DOWN, 0, 0, &[108]).map(|_| ()))?;
    c.vd(&format!("{tag} Down: empty line"), !got.is_empty(), &got);
    let got = v.said_during(300, |v| v.type_text("ec").map(|_| ()))?;
    c.vd(&format!("{tag} typing ec"), !got.is_empty(), &got);
    let got = v.said_during(500, |v| v.press(TAB, 9, 0, &[15]).map(|_| ()))?;
    c.vd(&format!("{tag} Tab completes"), heard(&got, "echo"), &got);
    let got = v.said_during(500, |v| v.press(BACKSPACE, 8, 0, &[14]).map(|_| ()))?;
    c.vd(&format!("{tag} BackSpace"), !got.is_empty(), &got);
    let got = v.said_during(500, |v| {
        v.chord(&[CTRL_L], KEY_U, 21, CONTROL, &[29, 22])
            .map(|_| ())
    })?;
    c.vd(&format!("{tag} Ctrl+U"), !got.is_empty(), &got);
    // Several candidates: the common prefix completes, the second Tab pair lists them.
    let got = v.said_during(300, |v| v.type_text("ls /usr/bin/gnome-se").map(|_| ()))?;
    c.vd(&format!("{tag} typing a path"), !got.is_empty(), &got);
    let got = v.said_during(600, |v| v.press(TAB, 9, 0, &[15]).map(|_| ()))?;
    c.vd(
        &format!("{tag} Tab: the prefix"),
        heard(&got, "ession"),
        &got,
    );
    let got = v.said_during(1500, |v| {
        v.press(TAB, 9, 0, &[15])?;
        std::thread::sleep(Duration::from_millis(150));
        v.press(TAB, 9, 0, &[15]).map(|_| ())
    })?;
    c.vd(
        &format!("{tag} Tab Tab: the candidates"),
        heard(&got, "gnome-session-quit") || heard(&got, "gnome-session-inhibit"),
        &got,
    );
    let got = v.said_during(500, |v| {
        v.chord(&[CTRL_L], KEY_U, 21, CONTROL, &[29, 22])
            .map(|_| ())
    })?;
    c.vd(&format!("{tag} Ctrl+U again"), !got.is_empty(), &got);
    Ok(())
}

/// The disturbance `n` of a VD round. The terminal keeps the keyboard focus through all of them.
fn vd_perturb(v: &mut V, n: usize, cbus: &str) -> Result<&'static str> {
    use std::process::Command;
    let alt_tab = |v: &mut V| v.chord(&[ALT_L], TAB, 9, ALT, &[56, 15]).map(|_| ());
    Ok(match n % 10 {
        0 => {
            std::thread::sleep(Duration::from_secs(20));
            "idle 20 s"
        }
        1 => {
            v.term
                .open_second_app("exec bash --norc -i")
                .context("second app")?;
            std::thread::sleep(Duration::from_secs(3));
            alt_tab(v)?;
            std::thread::sleep(Duration::from_millis(1500));
            alt_tab(v)?;
            std::thread::sleep(Duration::from_millis(1500));
            "second app window, Alt+Tab back"
        }
        2 => {
            v.term.open_tab("exec bash --norc -i")?;
            std::thread::sleep(Duration::from_secs(3));
            v.chord(&[CTRL_L], PAGE_UP, 0, CONTROL, &[29, 104])?;
            std::thread::sleep(Duration::from_millis(1200));
            v.chord(&[CTRL_L], PAGE_DOWN, 0, CONTROL, &[29, 109])?;
            std::thread::sleep(Duration::from_millis(1200));
            v.chord(&[CTRL_L], PAGE_UP, 0, CONTROL, &[29, 104])?;
            std::thread::sleep(Duration::from_millis(1200));
            "tab switch"
        }
        3 => {
            for i in 0..3 {
                let _ = Command::new("notify-send")
                    .args(["gorca soak", &format!("notification {i}")])
                    .env("DBUS_SESSION_BUS_ADDRESS", cbus)
                    .status();
                std::thread::sleep(Duration::from_millis(900));
            }
            std::thread::sleep(Duration::from_secs(5));
            "notifications"
        }
        4 => {
            // Ctrl+G rings the readline bell; so does a Tab with nothing to add.
            v.chord(&[CTRL_L], letter_key('g'), 7, CONTROL, &[29, 34])?;
            std::thread::sleep(Duration::from_millis(800));
            v.type_text("ls /usr/bin/gnome-")?;
            v.press(TAB, 9, 0, &[15])?;
            std::thread::sleep(Duration::from_millis(800));
            v.chord(&[CTRL_L], KEY_U, 21, CONTROL, &[29, 22])?;
            std::thread::sleep(Duration::from_millis(500));
            "bell"
        }
        5 => {
            v.term.rd_line("key 125");
            std::thread::sleep(Duration::from_millis(1500));
            v.term.rd_line("key 1");
            std::thread::sleep(Duration::from_millis(1500));
            "overview and back"
        }
        6 => {
            alt_tab(v)?;
            std::thread::sleep(Duration::from_millis(1200));
            alt_tab(v)?;
            std::thread::sleep(Duration::from_millis(1200));
            alt_tab(v)?;
            std::thread::sleep(Duration::from_millis(300));
            alt_tab(v)?;
            std::thread::sleep(Duration::from_millis(1500));
            "Alt+Tab storm"
        }
        7 => {
            // The search bar: the focus goes to its entry inside the window and comes back.
            v.chord(
                &[CTRL_L, SHIFT_L],
                letter_key('f'),
                0,
                CONTROL | SHIFT,
                &[29, 42, 33],
            )?;
            std::thread::sleep(Duration::from_millis(1500));
            v.press(ESCAPE, 27, 0, &[1])?;
            std::thread::sleep(Duration::from_millis(1500));
            "search bar and Escape"
        }
        8 => {
            // The context menu (Menu key) and Escape: a popup that takes the focus and returns it.
            v.term.rd_line("key 127");
            std::thread::sleep(Duration::from_millis(1500));
            v.press(ESCAPE, 27, 0, &[1])?;
            std::thread::sleep(Duration::from_millis(1500));
            "context menu and Escape"
        }
        _ => {
            std::thread::sleep(Duration::from_secs(60));
            "idle 60 s"
        }
    })
}

/// VD (real terminal, `GORCA_M6_REAL=1`, `tests/term/run_real_vte.sh`): a long session of ordinary
/// bash use (typing, Enter, history, Tab completion with several candidates, BackSpace) between
/// disturbances (idle, another application's window and Alt+Tab, tab switches, notifications, the
/// bell, the overview). After every disturbance a full cycle must read what it did, and the
/// generic key echo must never speak Tab, BackSpace or Enter by name: that is the signature of a
/// terminal that is no longer tracked. `GORCA_VD_ROUNDS` (default 8) sets the length.
pub fn vd(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    _size: Size,
    _seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "VD", out)? {
        return Ok(());
    }
    if !(flag(host, "GORCA_M6_REAL") && std::env::var_os("GORCA_VTE_WAYLAND").is_some()) {
        if flag(host, "GORCA_M6_STRICT") {
            bail!("VD: GORCA_M6_REAL=1 and the private compositor are required");
        }
        out.notes
            .push("VD SKIPPED: needs GORCA_M6_REAL=1 and tests/term/run_real_vte.sh".to_owned());
        out.x("skipped", 1.0, "flag");
        return Ok(());
    }
    let root = std::env::var("GORCA_HARNESS_DIR").context("GORCA_HARNESS_DIR not set")?;
    let cbus = std::env::var("GORCA_VTE_CBUS").context("GORCA_VTE_CBUS not set")?;
    let err_path = std::path::Path::new(&root).join("vd-reader.stderr");
    let mut h = m1_env(host);
    h.reader_env.retain(|(k, _)| k != "RUST_LOG");
    h.reader_env
        .push(("RUST_LOG".to_owned(), "debug".to_owned()));
    let mut o = super::scenarios::base_opts(reader, flavor);
    o.keys = true;
    o.reader_stderr = Some(err_path.clone());
    let rig = Rig::start(&h, &o)?;
    if !rig.keys_ok {
        bail!("the reader did not register with the emulated keyboard monitor (no SetKeyGrabs)");
    }
    rig.mock
        .configure(Duration::from_millis(1), Duration::from_millis(5));
    let term = RealTerm::start(host)?;
    let mut v = V {
        rig,
        term,
        ty: Typist::new(),
    };
    std::thread::sleep(Duration::from_secs(4));
    v.term.rd_line("click");
    std::thread::sleep(Duration::from_secs(1));
    v.press(
        K {
            code: 110,
            sym: 0xff50,
        },
        0,
        0,
        &[102],
    )?;
    std::thread::sleep(Duration::from_millis(1500));
    if v.rig.mock.since(0).is_empty() {
        bail!("the reader never spoke: the terminal did not come up");
    }
    let mut c = Check {
        failures: 0,
        notes: Vec::new(),
    };
    let rounds: usize = std::env::var("GORCA_VD_ROUNDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);
    vd_cycle(&mut v, &mut c, "base0")?;
    out.x("vd_base_failures", c.failures as f64, "steps");
    let mut per_round = Vec::new();
    for r in 0..rounds {
        let before = c.failures;
        let what = vd_perturb(&mut v, r, &cbus)?;
        vd_cycle(&mut v, &mut c, &format!("r{r}"))?;
        let bad = c.failures - before;
        per_round.push(bad);
        out.notes
            .push(format!("VD round {r} ({what}): {bad} failing steps"));
        if bad > 0 && flag(host, "GORCA_VD_RECOVER") {
            // Does Alt+Tab away and back repair it, as the user sees?
            v.chord(&[ALT_L], TAB, 9, ALT, &[56, 15]).ok();
            std::thread::sleep(Duration::from_millis(1500));
            v.chord(&[ALT_L], TAB, 9, ALT, &[56, 15]).ok();
            std::thread::sleep(Duration::from_millis(1500));
            let b2 = c.failures;
            vd_cycle(&mut v, &mut c, &format!("r{r}re"))?;
            out.notes.push(format!(
                "VD round {r}: after Alt+Tab away and back {} failing steps",
                c.failures - b2
            ));
        }
    }
    out.x("vd_failures", c.failures as f64, "steps");
    out.x(
        "vd_bad_rounds",
        per_round.iter().filter(|b| **b > 0).count() as f64,
        "rounds",
    );
    out.notes.extend(std::mem::take(&mut c.notes));
    if let Some(dir) = std::env::var_os("GORCA_VV_KEEP_DIR") {
        let dir = std::path::PathBuf::from(dir);
        let _ = std::fs::create_dir_all(&dir);
        if let Some(name) = err_path.file_name() {
            let _ = std::fs::copy(&err_path, dir.join(name));
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    fn p(t_key: u64, expect: &str) -> Probe {
        Probe {
            t_key,
            expect: expect.to_owned(),
            what: "type",
            contains: false,
        }
    }

    #[test]
    fn a_per_character_leak_is_found_and_ordinary_log_lines_are_not() {
        let secret = "Jq7Xk0Wz4Vy3";
        let clean = "2026-10-15T10:00:00Z INFO key event key=65 sym=0x71 handled\n\
                     DEBUG terminal actions Say { chars: 1, unit: Char }\n\
                     TRACE zbus::connection sent call q-string-with-J-inside\n";
        assert_eq!(secret_leaks(clean, clean, secret), 0);
        // Whole, half and per-character forms.
        assert!(secret_leaks("typed Jq7Xk0Wz4Vy3", "", secret) > 0);
        assert!(secret_leaks("x Jq7Xk0W y", "", secret) > 0);
        for leak in [
            "DEBUG key event Typed('q')\n",
            "TRACE text=X\n",
            "TRACE echo: W done\n",
            "TRACE {\"char\":\"k\"}\n",
        ] {
            assert!(secret_leaks("", leak, secret) > 0, "{leak}");
        }
        // The same line before the password phase does not count.
        assert_eq!(secret_leaks("", "", secret), 0);
        // A character that is part of a longer word is not a value of its own.
        assert!(!char_field("TRACE text=Xylophone", 'X'));
    }

    #[test]
    fn scoring_counts_missing_double_and_extra_speech() {
        let probes = vec![p(100, "a"), p(200, "b"), p(300, "c")];
        let spoken = vec![
            (110u64, "a".to_owned()),
            (210, "b".to_owned()),
            (215, "b".to_owned()),
            (220, "oops".to_owned()),
        ];
        let (lat, missing, double, extra, _) = score(&probes, &spoken);
        assert_eq!(lat.len(), 2);
        assert_eq!((missing, double, extra), (1, 1, 1));
    }

    #[test]
    fn a_contained_probe_matches_a_longer_utterance() {
        let probes = vec![Probe {
            t_key: 0,
            expect: "o7".to_owned(),
            what: "enter",
            contains: true,
        }];
        let spoken = vec![(5u64, "o7\nme at box".to_owned())];
        let (_, missing, double, extra, _) = score(&probes, &spoken);
        assert_eq!((missing, double, extra), (0, 0, 0));
    }

    #[test]
    fn json_is_escaped_for_control_characters() {
        assert_eq!(
            json_esc("a\x1b[D\r\n\"\\"),
            "a\\u001b[D\\u000d\\u000a\\\"\\\\"
        );
    }

    #[test]
    fn names_follow_the_verbalizer() {
        assert_eq!(name_of(' '), "space");
        assert_eq!(name_of('-'), "dash");
        assert_eq!(name_of('x'), "x");
    }

    #[test]
    fn every_character_of_the_real_terminal_script_has_a_key() {
        for text in [
            "read -s -p password: x",
            "seq 1000 1999 | less",
            "zq7tr0ub4dor3",
        ] {
            for c in text.chars() {
                assert!(
                    evdev_of(c).is_some() ^ shifted_of(c).is_some(),
                    "{c:?} in {text:?}"
                );
            }
        }
        assert_eq!(shifted_of(':'), Some(39));
        assert_eq!(evdev_of('/'), Some(53));
    }

    #[test]
    fn heard_and_expect_count_a_missing_phrase_once() {
        let got = vec!["1040\n1041".to_owned()];
        assert!(heard(&got, "1040"));
        let mut c = Check {
            failures: 0,
            notes: Vec::new(),
        };
        c.expect("a", heard(&got, "1000"), &got);
        c.expect("b", heard(&got, "1041"), &got);
        assert_eq!(c.failures, 1);
        assert_eq!(c.notes.len(), 1);
    }

    #[test]
    fn the_vs_loop_lines_carry_their_count() {
        assert_eq!(vs_count("tick 41"), Some(41));
        assert_eq!(vs_count("burst 37 12"), Some(37));
        assert_eq!(vs_count("blank"), None);
        assert_eq!(vs_count("tick"), None);
    }

    #[test]
    fn the_p99_of_nothing_is_zero() {
        assert_eq!(p99_u64(&[]), 0);
        assert_eq!(p99_u64(&[1, 2, 3, 100]), 3);
    }
}
