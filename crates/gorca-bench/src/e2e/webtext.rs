//! M3b and M7b end-to-end gates: gorca (the real daemon) with a PRIVATE Firefox, the emulated
//! Mutter delivering the keys to gorca and the compositor's virtual keyboard delivering them to
//! the page (`tests/web/m7/rd.py`), the mock speech server scoring the answers.
//!
//! The M3 gates ran on a fake GTK-like app; these run the same questions against the real
//! Gecko accessibility server: UTF-16 offsets with phantom U+FEFF after every astral
//! character, text events with no phantoms, carets on the paragraphs of a contenteditable.
//!
//! | id | stimulus | metrics |
//! |---|---|---|
//! | FC1h | Right and Left through `a😀b😀😀c` (window hits); what each key must say follows from the page's own caret events | `fc1h_ms` Firefox caret event arrival to SPEAK (gate p99 3 ms), `lost`, `fc_double`, `fc_extra`, `fc_late`, `fc_swallowed` (all gate 0), `page_dropped` (gate at most 2 per repeat) |
//! | FC1 | Down through 300 distinct lines of a text area (one `GetStringAtOffset` per key) | `fc1_ms` (gate p99 15 ms), `lost`, `fc_double` |
//! | FE1 | printable keys typed into an input | `fe1_ms` key emit to SPEAK (gate p99 3 ms), `lost`, `fc_double` |
//! | FCE2 | printable keys typed into an empty contenteditable region | `fce2_ms` key emit to SPEAK (gate p99 3 ms), `lost`, `fc_double`, `fc_extra`, `fc_late` |
//! | FE2 | randomised session: letters, space, BackSpace, input-method commits of an emoji and a Cyrillic letter, arrows | `e2_missing`, `e2_double`, `e2_keyname`, `e2_wrong`, `e2_swallowed`, `ft_failures` (all gate 0), `e2_resync` (actions the page did not apply, not scored; gate at most 3 per repeat) |
//! | FS2 | Shift+Right and Shift+Left over a word | `fs2_ms` caret event arrival to SPEAK (gate p99 15 ms), `lost` |
//! | FSA1 | gorca+Down in a text area | `fsa1_ms` key emit to the first chunk's SPEAK (gate p99 15 ms), `lost` |
//! | FSA3 | any key during say-all | `fsa3_stop_ms` key emit to the cancel (gate p99 2 ms), `sa3_caret_wrong` (the caret must sit at the start of the chunk being read), `sa3_not_silenced` |
//! | FCE1 | caret, typing and BackSpace in a contenteditable region | `fce1_ms`, `ft_failures` (gate 0) |
//! | FT1 | browse to focus mode and back with a field: typing echo after each switch | `ft_failures` |
//! | FP1 | Escape while the field's popup is open | `ft_failures` |
//! | BSA1 | gorca+Down in browse mode: the page is read, the cursor follows, a key stops it | `bsa1_ms` (gate p99 5 ms), `bsa_failures` |
//! | BAR1 | a page loads: title, then it reads itself; a key stops it | `bar_failures`, `bar_start_ms` (information) |
//! | BL1 | quick navigation on the Russian layout, Shift+digit on both | `bl_failures` |
//! | FR1 | frame and article quick navigation on a page with an iframe | `fr_failures` |
//!
//! Everything is skipped unless `--reader-env GORCA_M7=1` and the private compositor are there
//! (`tests/web/m7/run_m7.sh`).

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use gorca_bench_b2::clock::Rng;

use super::apps::ReaderSpec;
use super::m1::K;
use super::m3::{Probe, lost_notes};
use super::mock::{Flavor, Kind};
use super::runner::HostEnv;
use super::scenarios::{RunOut, Size, ms};
use super::web::{CARET, DocRef, Web, file_url, gate_open, pages_dir, speaks_since};

/// The scenario ids of this module, in the order of a full run.
pub const IDS: &[&str] = &[
    "FC1h", "FC1", "FE1", "FCE2", "FE2", "FS2", "FSA1", "FSA3", "FCE1", "FT1", "FP1", "BSA1",
    "BAR1", "BL1", "FR1",
];

/// The reader does not read pages by itself (the M7a keys-by-line scenarios), or does (the
/// shipped default).
const CFG_QUIET: &str = "schema = 1\n[browse]\nsay_all_on_load = false\n";
const CFG_DEFAULT: &str = "schema = 1\n";

pub fn run(
    id: &str,
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, id, out)? {
        return Ok(());
    }
    match id {
        "FC1h" => fc1h(host, reader, flavor, size, out),
        "FC1" => fc1(host, reader, flavor, size, out),
        "FE1" => fe1(host, reader, flavor, size, seed, out, false),
        "FCE2" => fe1(host, reader, flavor, size, seed, out, true),
        "FE2" => fe2(host, reader, flavor, size, seed, out),
        "FS2" => fs2(host, reader, flavor, size, out),
        "FSA1" => fsa(host, reader, flavor, size, seed, out, false),
        "FSA3" => fsa(host, reader, flavor, size, seed, out, true),
        "FCE1" => fce1(host, reader, flavor, out),
        "FT1" => ft1(host, reader, flavor, out),
        "FP1" => fp1(host, reader, flavor, out),
        "BSA1" => bsa1(host, reader, flavor, size, out),
        "BAR1" => bar1(host, reader, flavor, out),
        "BL1" => bl1(host, reader, flavor, out),
        "FR1" => fr1(host, reader, flavor, out),
        other => bail!("unknown web text scenario {other}"),
    }
}

// ---------------------------------------------------------------------------------------------
// Keys. XKB keycode = evdev code + 8; the page gets the evdev code through the compositor.

const fn k(evdev: u16, sym: u32) -> K {
    K {
        code: evdev + 8,
        sym,
    }
}

const LEFT: K = k(105, 0xff51);
const RIGHT: K = k(106, 0xff53);
const UP: K = k(103, 0xff52);
const DOWN: K = k(108, 0xff54);
const HOME: K = k(102, 0xff50);
const END: K = k(107, 0xff57);
const BACKSPACE: K = k(14, 0xff08);
const ENTER: K = k(28, 0xff0d);
const ESCAPE: K = k(1, 0xff1b);
const SPACE: K = k(57, 0x20);
const F9: K = k(67, 0xffc6);
const F10: K = k(68, 0xffc7);
/// A key that does nothing in a page and is nobody's mode switch: it ends a reading.
const F8: K = k(66, 0xffc5);
const INSERT: K = k(110, 0xff63);
const CTRL_L: K = k(29, 0xffe3);
const SHIFT_L: K = k(42, 0xffe1);
const ALT_L: K = k(56, 0xffe9);
const SHIFT: u32 = 1;
const CONTROL: u32 = 4;
const ALT: u32 = 8;

/// The evdev code of a lower-case Latin letter or a digit.
fn evdev_of(c: char) -> Option<u16> {
    Some(match c {
        'q' => 16,
        'w' => 17,
        'e' => 18,
        'r' => 19,
        't' => 20,
        'y' => 21,
        'u' => 22,
        'i' => 23,
        'o' => 24,
        'p' => 25,
        'a' => 30,
        's' => 31,
        'd' => 32,
        'f' => 33,
        'g' => 34,
        'h' => 35,
        'j' => 36,
        'k' => 37,
        'l' => 38,
        'z' => 44,
        'x' => 45,
        'c' => 46,
        'v' => 47,
        'b' => 48,
        'n' => 49,
        'm' => 50,
        '1'..='9' => 2 + (c as u16 - '1' as u16),
        '0' => 11,
        _ => return None,
    })
}

fn char_key(c: char) -> K {
    match c {
        ' ' => SPACE,
        _ => k(evdev_of(c).unwrap_or(30), c as u32),
    }
}

fn pages(host: &HostEnv) -> std::path::PathBuf {
    host.harness_dir.join("../web/m3b/pages")
}

fn sleep(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

/// One scenario's Firefox with its document.
struct Ft {
    w: Web,
    doc: DocRef,
    /// The window has been woken by a key: Firefox's first keyboard input moves the focus
    /// through a panel once, which a field focused before it would lose.
    woken: bool,
}

impl Ft {
    fn open(
        host: &HostEnv,
        reader: &ReaderSpec,
        flavor: Flavor,
        page: &str,
        title: &str,
        cfg: &str,
    ) -> Result<Self> {
        let url = file_url(&pages(host).join(page))?;
        let (mut w, doc) = Web::start_cfg(host, reader, flavor, &[url], title, cfg)?;
        w.prime(&doc)?;
        Ok(Self {
            w,
            doc,
            woken: false,
        })
    }

    /// The entry named `label`, focused: "focus mode" was heard and the text window had time to
    /// be fetched.
    fn enter(&mut self, label: &str) -> Result<String> {
        self.enter_role("entry", label)
    }

    fn enter_role(&mut self, role: &str, label: &str) -> Result<String> {
        let path = self.w.find(&self.doc.path, role, label)?;
        self.focus_field(&path)?;
        Ok(path)
    }

    fn focus_field(&mut self, path: &str) -> Result<()> {
        if !self.woken {
            self.woken = true;
            self.w.ff.keys(&[42]);
            sleep(900);
            self.w.focus(&self.doc.path)?;
            sleep(500);
        }
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            let mark = self.w.mark();
            self.w.focus(path)?;
            if self
                .w
                .speech(mark, Duration::from_secs(3), |t| t.contains("focus mode"))
                .is_some()
            {
                break;
            }
            if Instant::now() > end {
                bail!(
                    "no \"focus mode\" for {path}; heard {:?}",
                    speaks_since(&self.w, 0)
                );
            }
            // Back to the document, then onto the field again.
            self.w.focus(&self.doc.path)?;
            sleep(500);
        }
        sleep(450);
        self.w.settle_grabs()?;
        Ok(())
    }

    /// A key for gorca (the emulated Mutter, with the modifier keys around it) and, unless gorca
    /// swallowed it, for the page. Returns the emit time of the press and whether gorca took it.
    fn key(&mut self, key: K, uni: u32, state: u32) -> Result<(u64, bool)> {
        if self.w.unsettled {
            self.w.settle_grabs()?;
        }
        let mods = [
            (CONTROL, CTRL_L, 29u32),
            (SHIFT, SHIFT_L, 42),
            (ALT, ALT_L, 56),
        ];
        let w = &mut self.w;
        let m = w.rig.mutter.as_mut().context("no mutter")?;
        let mut held = 0u32;
        for (bit, mk, _) in mods {
            if state & bit != 0 {
                w.ty.t_ms += 4;
                m.key_ex(mk.code, mk.sym, held, false, w.ty.t_ms)?;
                held |= bit;
            }
        }
        w.ty.t_ms += 12;
        let down = m.key_full(key.code, key.sym, uni, state, false, w.ty.t_ms)?;
        w.ty.t_ms += 3;
        m.key_full(key.code, key.sym, 0, state, true, w.ty.t_ms)?;
        for (bit, mk, _) in mods.iter().rev() {
            if state & bit != 0 {
                w.ty.t_ms += 3;
                held &= !bit;
                m.key_ex(mk.code, mk.sym, held, true, w.ty.t_ms)?;
            }
        }
        if !down.consumed {
            let mut codes: Vec<u32> = mods
                .iter()
                .filter(|(bit, ..)| state & bit != 0)
                .map(|(_, _, ev)| *ev)
                .collect();
            codes.push(u32::from(key.code) - 8);
            w.ff.keys(&codes);
        }
        Ok((down.t_emit_ns, down.consumed))
    }

    /// A gorca chord (`gorca+key`): the Insert modifier held around the key. Only gorca sees it.
    fn chord(&mut self, key: K) -> Result<(u64, bool)> {
        if self.w.unsettled {
            self.w.settle_grabs()?;
        }
        let w = &mut self.w;
        let m = w.rig.mutter.as_mut().context("no mutter")?;
        w.ty.t_ms += 700;
        let t = w.ty.t_ms;
        m.key_ex(INSERT.code, INSERT.sym, 0, false, t)?;
        let down = m.key_ex(key.code, key.sym, 0, false, t + 5)?;
        m.key_ex(key.code, key.sym, 0, true, t + 15)?;
        m.key_ex(INSERT.code, INSERT.sym, 0, true, t + 25)?;
        w.ty.t_ms += 25;
        Ok((down.t_emit_ns, down.consumed))
    }

    /// Every spoken text since `mark`, with its arrival time.
    fn heard(&self, mark: usize) -> Vec<(u64, String)> {
        speaks_since(&self.w, mark)
    }

    /// Wait for a speech that satisfies `pred`; false when none came within `wait`.
    fn hears(&self, mark: usize, wait: Duration, pred: impl Fn(&str) -> bool) -> bool {
        self.w.speech(mark, wait, pred).is_some()
    }

    fn caret_event_after(&self, t_key: u64) -> u64 {
        self.w
            .tap
            .first_after(CARET, &self.w.bus, None, t_key)
            .unwrap_or(t_key)
    }

    /// A reply field of the AT-SPI driver.
    fn ax_num(&mut self, cmd: &str, path: &str, field: &str) -> Result<f64> {
        let r = self.w.ax.request(
            cmd,
            &format!("\"bus\":\"{}\",\"path\":\"{path}\"", self.w.bus),
            Duration::from_secs(5),
        )?;
        super::apps::json_num(&r, field).with_context(|| format!("no {field} in {r}"))
    }
}

/// Score `probes` against what was said ([`score_aligned`]); latencies are from the Firefox
/// caret event (`from_event`) or from the key. Records the gated `fc_extra` (speech no key
/// accounts for). Failed trials leave their evidence in `out.notes`.
fn finish(
    f: &mut Ft,
    id: &str,
    probes: &mut [Probe],
    from_event: bool,
    out: &mut RunOut,
) -> (Vec<f64>, u64, u64) {
    finish_with(f, id, probes, from_event, &[], out)
}

/// [`finish`] with, for each unscored key, the texts that may be said after it (the character
/// at the edge); anything else said after an unscored key counts as `fc_extra`. `allowed` is
/// indexed like `probes` (shorter: no allowance). Also records the gated `fc_late`.
fn finish_with(
    f: &mut Ft,
    id: &str,
    probes: &mut [Probe],
    from_event: bool,
    allowed: &[Vec<String>],
    out: &mut RunOut,
) -> (Vec<f64>, u64, u64) {
    for p in probes.iter_mut() {
        p.t_ev = f.caret_event_after(p.t_key);
    }
    let spoken = speaks_since(&f.w, 0);
    let sc = score_aligned(probes, &spoken, from_event, allowed);
    out.x("fc_extra", sc.extra as f64, "speeches");
    out.x("fc_late", sc.late as f64, "speeches");
    for &j in sc.stray.iter().take(5) {
        let (t, s) = &spoken[j];
        let near = probes.iter().rposition(|p| p.t_key <= *t);
        out.notes.push(format!(
            "{id} stray speech {s:?} at {:+.1} ms after key #{}",
            near.map_or(0.0, |k| (*t as f64 - probes[k].t_key as f64) / 1e6),
            near.map_or(-1, |k| k as i64),
        ));
    }
    let (lat, lost, doubled) = (sc.lat, sc.lost, sc.doubled);
    if lost > 0 {
        out.notes.extend(lost_notes(id, probes, &spoken));
        // Which objects sent the events: the evidence a wrong route needs.
        let mut seen: Vec<String> = Vec::new();
        for e in f.w.tap.tail(12) {
            if let Some(path) = e.split_whitespace().nth(1)
                && !seen.iter().any(|p| p == path)
            {
                seen.push(path.to_owned());
            }
        }
        for path in seen {
            if let Ok(r) = f.w.ax.request(
                "describe",
                &format!("\"bus\":\"{}\",\"path\":\"{path}\"", f.w.bus),
                Duration::from_secs(3),
            ) {
                out.notes.push(format!("{id} event source {path}: {r}"));
            }
        }
        f.w.dump_loss(&id.to_lowercase());
    }
    (lat, lost, doubled)
}

/// A speech this long after its key is lost (a loaded Firefox answers late; a speech that
/// never comes is what the gate is about).
const LATE_NS: u64 = 1_500_000_000;

/// A matched speech this much later than its own key's page caret event (`from_event`) or its
/// own key is not that key's echo: gorca-side echo is well under 1 ms. Gated at 0.
const LATE_GATE_MS: f64 = 50.0;

/// The result of scoring a series of keys against what was said.
#[derive(Debug, Default, PartialEq)]
struct Score {
    lat: Vec<f64>,
    /// Scored keys with no speech of their text, in order, within [`LATE_NS`].
    lost: u64,
    /// Speeches that repeat the text of the speech matched just before them.
    doubled: u64,
    /// Every other speech no key accounts for (a wrong text, a stray word).
    extra: u64,
    /// Matched speeches later than [`LATE_GATE_MS`] after their own key's event (or the key):
    /// a speech borrowed by a silent key, or a real latency failure. Gated at 0.
    late: u64,
    /// Indices of the unmatched speeches that count (doubled or extra), for the notes.
    stray: Vec<usize>,
}

/// Score in order, strictly (the speech of a loaded Firefox may come after the next key, so a
/// window around each key would blame gorca for Gecko's lag). Every speech from the first key
/// on is aligned with the keys by a longest common subsequence: speech `j` may stand for key
/// `i` when its text is the key's, it came after the key and within [`LATE_NS`] of it, and the
/// order is kept. Nothing is skipped silently: a key without its speech is `lost`; a speech
/// left over is `doubled` when it repeats the matched speech before it, else `extra`. A probe
/// with an empty `expect` is a key that is not scored: whatever is said after it and before
/// the next key is neither matched nor counted. Latency is from the last Firefox caret event
/// between the key and its speech (`from_event`) or from the key.
fn score_aligned(
    probes: &[Probe],
    spoken: &[(u64, String)],
    from_event: bool,
    allowed: &[Vec<String>],
) -> Score {
    // From the matched key's OWN page event: a silent key whose speech is borrowed by the next
    // equal key would otherwise get that key's (later) event and look instantaneous.
    let lat_of = |i: usize, t: u64| {
        let t0 = if from_event {
            probes[i].t_ev.max(probes[i].t_key)
        } else {
            probes[i].t_key
        };
        ms(t.saturating_sub(t0))
    };
    align(probes, spoken, allowed, lat_of)
}

/// The alignment of [`score_aligned`], apart from the bus (testable).
fn align(
    probes: &[Probe],
    spoken: &[(u64, String)],
    allowed: &[Vec<String>],
    lat_of: impl Fn(usize, u64) -> f64,
) -> Score {
    let Some(first) = probes.first() else {
        return Score::default();
    };
    // Every speech from the first key on.
    let sp: Vec<(usize, &(u64, String))> = spoken
        .iter()
        .enumerate()
        .filter(|(_, (t, _))| *t >= first.t_key)
        .collect();
    let (n, m) = (probes.len(), sp.len());
    let ok = |i: usize, j: usize| {
        let p = &probes[i];
        let (t, s) = sp[j].1;
        !p.expect.is_empty() && *t >= p.t_key && *t <= p.t_key + LATE_NS && *s == p.expect
    };
    // dp[i][j]: most matches of probes[i..] with sp[j..].
    let w = m + 1;
    let mut dp = vec![0u32; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let mut best = dp[(i + 1) * w + j].max(dp[i * w + j + 1]);
            if ok(i, j) {
                best = best.max(1 + dp[(i + 1) * w + j + 1]);
            }
            dp[i * w + j] = best;
        }
    }
    let mut matched_sp = vec![None::<usize>; m];
    let mut probe_hit = vec![false; n];
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if ok(i, j) && dp[i * w + j] == 1 + dp[(i + 1) * w + j + 1] {
            matched_sp[j] = Some(i);
            probe_hit[i] = true;
            i += 1;
            j += 1;
        } else if dp[i * w + j] == dp[(i + 1) * w + j] {
            i += 1;
        } else {
            j += 1;
        }
    }
    let mut sc = Score::default();
    for (i, p) in probes.iter().enumerate() {
        if p.expect.is_empty() {
            continue;
        }
        if !probe_hit[i] {
            sc.lost += 1;
        }
    }
    for (j, hit) in matched_sp.iter().enumerate() {
        if let Some(i) = hit {
            let l = lat_of(*i, sp[j].1.0);
            if l > LATE_GATE_MS {
                sc.late += 1;
            }
            sc.lat.push(l);
        }
    }
    // The speeches no key accounts for.
    let mut last_text: Option<&str> = None;
    for (j, hit) in matched_sp.iter().enumerate() {
        let (t, s) = sp[j].1;
        if hit.is_some() {
            last_text = Some(s.as_str());
            continue;
        }
        // Said after an unscored key (and before the next key): not ours to judge.
        let owner = probes.iter().rposition(|p| p.t_key <= *t);
        if let Some(k) = owner
            && probes[k].expect.is_empty()
            && allowed.get(k).is_none_or(|a| a.is_empty() || a.contains(s))
        {
            continue;
        }
        if last_text == Some(s.as_str()) {
            sc.doubled += 1;
        } else {
            sc.extra += 1;
        }
        sc.stray.push(sp[j].0);
    }
    sc
}

#[cfg(test)]
mod align_tests {
    use super::*;

    fn p(t: u64, e: &str) -> Probe {
        Probe {
            t_key: t * 1_000_000,
            t_ev: 0,
            expect: e.to_owned(),
        }
    }
    fn s(t: u64, e: &str) -> (u64, String) {
        (t * 1_000_000, e.to_owned())
    }
    fn run(probes: &[Probe], spoken: &[(u64, String)]) -> (usize, u64, u64, u64) {
        let sc = align(probes, spoken, &[], |_, _| 0.0);
        (sc.lat.len(), sc.lost, sc.doubled, sc.extra)
    }

    #[test]
    fn late_speech_in_order_is_heard() {
        let pr = [p(0, "a"), p(140, "b"), p(280, "a")];
        let sp = [s(150, "a"), s(290, "b"), s(430, "a")];
        assert_eq!(run(&pr, &sp), (3, 0, 0, 0));
    }

    #[test]
    fn a_missing_speech_is_lost_not_borrowed_from_a_later_key() {
        // Key 0 wants "a" and gets nothing; key 2's "a" must not stand for it.
        let pr = [p(0, "a"), p(140, "b"), p(280, "a")];
        let sp = [s(150, "b"), s(290, "a")];
        assert_eq!(run(&pr, &sp), (2, 1, 0, 0));
    }

    #[test]
    fn a_repeat_is_doubled_and_a_wrong_text_is_extra() {
        let pr = [p(0, "a"), p(140, "b")];
        let sp = [s(5, "a"), s(10, "a"), s(145, "c"), s(150, "b")];
        assert_eq!(run(&pr, &sp), (2, 0, 1, 1));
    }

    #[test]
    fn a_wrong_text_instead_of_the_right_one_is_lost_and_extra() {
        let pr = [p(0, "a")];
        let sp = [s(5, "x")];
        assert_eq!(run(&pr, &sp), (0, 1, 0, 1));
    }

    #[test]
    fn too_late_is_lost() {
        let pr = [p(0, "a")];
        let sp = [s(1600, "a")];
        assert_eq!(run(&pr, &sp), (0, 1, 0, 1));
    }

    /// Review F-2: a silent key, then the next key (same text) spoken twice. With latency from
    /// each match's OWN key event the borrowed first copy is 140 ms late and gated.
    #[test]
    fn a_silent_key_then_a_doubled_next_key_is_late_not_clean() {
        let pr = [p(0, "x"), p(140, "x")];
        let sp = [s(141, "x"), s(142, "x")];
        // Page caret events: key 0's at 0.2 ms, key 1's at 140.2 ms.
        let ev = [200_000u64, 140_200_000];
        let own = |i: usize, t: u64| ms(t.saturating_sub(ev[i]));
        let sc = align(&pr, &sp, &[], own);
        assert_eq!((sc.lost, sc.late), (0, 1), "{sc:?}");
        // The old measure (last page event between the key and the speech) saw only key 1's
        // event for both copies and scored this clean: the blind spot.
        let old = |i: usize, t: u64| {
            let t0 = ev
                .iter()
                .copied()
                .filter(|e| *e >= pr[i].t_key && *e <= t)
                .max()
                .unwrap_or(pr[i].t_key);
            ms(t.saturating_sub(t0))
        };
        assert_eq!(align(&pr, &sp, &[], old).late, 0);
    }

    #[test]
    fn speech_after_a_dropped_key_is_nothing_or_the_edge_character() {
        let pr = [p(0, "a"), p(140, ""), p(400, "b")];
        let allowed = [vec![], vec!["a".to_owned()], vec![]];
        let ok = [s(5, "a"), s(150, "a"), s(405, "b")];
        assert_eq!(align(&pr, &ok, &allowed, |_, _| 0.0).extra, 0);
        let bad = [s(5, "a"), s(150, "zzz"), s(405, "b")];
        assert_eq!(align(&pr, &bad, &allowed, |_, _| 0.0).extra, 1);
    }

    #[test]
    fn speech_after_an_unscored_key_is_ignored() {
        let pr = [p(0, "a"), p(140, ""), p(400, "b")];
        let sp = [s(5, "a"), s(150, "whatever"), s(405, "b")];
        assert_eq!(run(&pr, &sp), (2, 0, 0, 0));
    }
}

// ---------------------------------------------------------------------------------------------
// Caret echo

/// FC1h: Right and Left over a mixed Latin and emoji line; the window answers (no IPC).
fn fc1h(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    out: &mut RunOut,
) -> Result<()> {
    let mut f = Ft::open(host, reader, flavor, "fields.html", "M3b Fields", CFG_QUIET)?;
    let path = f.enter("Emoji line")?;
    // Home: the caret is at 0 (the field is focused with it there).
    let mut probes: Vec<Probe> = Vec::new();
    let mut swallowed = 0u64;
    let cycles = (size.n / 12).clamp(4, 40);
    for _ in 0..cycles {
        // Right over six positions (😀 b 😀 😀 c and the end), then back (c 😀 😀 b 😀 a).
        for key in [RIGHT; 6].into_iter().chain([LEFT; 6]) {
            let (t, consumed) = f.key(key, 0, 0)?;
            if consumed {
                swallowed += 1;
            }
            probes.push(Probe {
                t_key: t,
                t_ev: 0,
                expect: String::new(),
            });
            sleep(140);
        }
    }
    sleep(400);
    // What each key must say follows from where the page put the caret (its own caret
    // events, in UTF-16 units), not from a model: a loaded Firefox now and then drops a key
    // of the virtual keyboard, and every later expectation of a model would be off by one.
    // A key that moved nothing is not scored (the reader then names the character at the
    // edge) and is counted in `page_dropped`.
    let first = probes.first().map_or(0, |p| p.t_key);
    let carets = f.w.tap.events_since(CARET, &f.w.bus, &path, first);
    let mut dropped = 0u64;
    let keys: Vec<u64> = probes.iter().map(|p| p.t_key).collect();
    // After a key the page dropped the reader may say nothing, or name the character at the
    // edge the caret still stands at (or, when the page's event is only late, the one of an
    // event that arrives before the key after next); any other speech is `fc_extra`.
    let mut allowed: Vec<Vec<String>> = vec![Vec::new(); probes.len()];
    let mut prev_at = 0i32;
    for (i, p) in probes.iter_mut().enumerate() {
        let next = keys.get(i + 1).copied().unwrap_or(u64::MAX);
        match carets
            .iter()
            .rev()
            .find(|(t, _)| *t >= p.t_key && *t < next)
        {
            Some((_, at)) => {
                p.expect = emoji_line_at(*at);
                prev_at = *at;
            }
            None => {
                dropped += 1;
                let until = keys.get(i + 2).copied().unwrap_or(u64::MAX);
                let mut a = vec![emoji_line_at(prev_at)];
                a.extend(
                    carets
                        .iter()
                        .filter(|(t, _)| *t >= p.t_key && *t < until)
                        .map(|(_, at)| emoji_line_at(*at)),
                );
                allowed[i] = a;
            }
        }
    }
    if dropped > 0 {
        out.notes.push(format!(
            "FC1h: {dropped} of {} keys moved no caret in the page (not scored)",
            probes.len()
        ));
    }
    let (lat, lost, doubled) = finish_with(&mut f, "FC1h", &mut probes, true, &allowed, out);
    out.s("fc1h_ms", lat);
    out.x("lost", lost as f64, "keys");
    out.x("fc_double", doubled as f64, "keys");
    out.x("fc_swallowed", swallowed as f64, "keys");
    out.x("page_dropped", dropped as f64, "keys");
    Ok(())
}

/// What is said for the caret at UTF-16 offset `at` of `a😀b😀😀c` (a phantom U+FEFF follows
/// each emoji in Gecko's string, so 2, 5 and 7 are never a caret position).
fn emoji_line_at(at: i32) -> String {
    match at {
        0 => "a",
        1 | 4 | 6 => "\u{1F600}",
        3 => "b",
        8 => "c",
        9 => "blank",
        _ => "<caret inside a surrogate pair>",
    }
    .to_owned()
}

/// FC1: Down through 300 distinct lines of a text area; every key needs the toolkit's line.
fn fc1(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    out: &mut RunOut,
) -> Result<()> {
    let mut f = Ft::open(host, reader, flavor, "fields.html", "M3b Fields", CFG_QUIET)?;
    f.enter("Lines")?;
    // A value set by script leaves the caret at its end: to the top first.
    f.key(HOME, 0, CONTROL)?;
    sleep(500);
    let n = size.n.clamp(40, 250);
    let mut probes: Vec<Probe> = Vec::new();
    for i in 1..=n {
        let (t, _) = f.key(DOWN, 0, 0)?;
        probes.push(Probe {
            t_key: t,
            t_ev: 0,
            expect: format!("Line {i:04} \u{432}\u{442}\u{43e}\u{440}\u{430}\u{44f} \u{441}\u{442}\u{440}\u{43e}\u{43a}\u{430} \u{1F600} end"),
        });
        sleep(120);
    }
    sleep(500);
    let (lat, lost, doubled) = finish(&mut f, "FC1", &mut probes, true, out);
    out.s("fc1_ms", lat);
    out.x("lost", lost as f64, "keys");
    out.x("fc_double", doubled as f64, "keys");
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Typing echo

/// FE1: letters typed into an empty input: each is spoken once, from the key. FCE2 (`rich`):
/// the same into an empty contenteditable region (its paragraph gets the text events).
fn fe1(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
    rich: bool,
) -> Result<()> {
    let id = if rich { "FCE2" } else { "FE1" };
    let mut f = Ft::open(host, reader, flavor, "fields.html", "M3b Fields", CFG_QUIET)?;
    f.enter(if rich { "Typing region:" } else { "Typing" })?;
    let mut rng = Rng::new(seed);
    let n = size.n.clamp(60, 600);
    let mut probes: Vec<Probe> = Vec::new();
    for _ in 0..n {
        let c = char::from(b'a' + u8::try_from(rng.below(26)).unwrap_or(0));
        let (t, _) = f.key(char_key(c), c as u32, 0)?;
        probes.push(Probe {
            t_key: t,
            t_ev: 0,
            expect: c.to_string(),
        });
        sleep(110);
    }
    sleep(500);
    let (lat, lost, doubled) = finish(&mut f, id, &mut probes, false, out);
    out.s(if rich { "fce2_ms" } else { "fe1_ms" }, lat);
    out.x("lost", lost as f64, "keys");
    out.x("fc_double", doubled as f64, "keys");
    Ok(())
}

/// What is said for one character of a text.
fn name_of(c: char) -> String {
    match c {
        ' ' => "space".to_owned(),
        c => c.to_string(),
    }
}

#[derive(Copy, Clone, Debug)]
enum Act {
    Letter(char),
    Space,
    Back,
    Emoji,
    Cyr,
    Left,
    Right,
    Home,
    End,
}

/// FE2: a randomised session in an input, checked against a model of its text.
fn fe2(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    let mut f = Ft::open(host, reader, flavor, "fields.html", "M3b Fields", CFG_QUIET)?;
    let path = f.enter("Session")?;
    // The field's own accessible: the label above is a sibling, the entry is what `enter` found.
    let mut rng = Rng::new(seed ^ 0xE2);
    let n = size.n.clamp(100, 700);
    let mut text: Vec<char> = Vec::new();
    let mut caret = 0usize;
    let (mut missing, mut double, mut keyname, mut wrong) = (0u64, 0u64, 0u64, 0u64);
    let mut history: Vec<String> = Vec::new();
    let mut drift_notes = 0u32;
    // Actions the page did not apply as the model says (a loaded Firefox now and then drops a
    // key of the virtual keyboard): the model is set to what the page holds, the action is
    // not scored, and the count is gated (`e2_resync`), so a stream of them cannot hide.
    let mut resync = 0u64;
    // Plain typing keys gorca itself took (they must always reach the page).
    let mut swallowed = 0u64;
    for i in 0..n {
        let act = match rng.below(100) {
            0..=44 => Act::Letter(char::from(b'a' + u8::try_from(rng.below(26)).unwrap_or(0))),
            45..=54 => Act::Space,
            55..=66 => Act::Back,
            67..=71 => Act::Emoji,
            72..=76 => Act::Cyr,
            77..=84 => Act::Left,
            85..=92 => Act::Right,
            93..=96 => Act::Home,
            _ => Act::End,
        };
        history.push(format!("{act:?} at caret {caret} of {}", text.len()));
        let recent = history[history.len().saturating_sub(4)..].join(" | ");
        // The model: what the key does and what must be said.
        let at_char = |text: &[char], c: usize| {
            text.get(c)
                .map_or_else(|| "blank".to_owned(), |x| name_of(*x))
        };
        let expect: Option<String> = match act {
            Act::Letter(c) => {
                text.insert(caret, c);
                caret += 1;
                Some(c.to_string())
            }
            Act::Space => {
                text.insert(caret, ' ');
                caret += 1;
                Some("space".to_owned())
            }
            Act::Back => {
                if caret > 0 {
                    caret -= 1;
                    let gone = text.remove(caret);
                    Some(name_of(gone))
                } else {
                    None
                }
            }
            Act::Emoji => {
                text.insert(caret, '\u{1F600}');
                caret += 1;
                Some("\u{1F600}".to_owned())
            }
            Act::Cyr => {
                text.insert(caret, '\u{436}');
                caret += 1;
                Some("\u{436}".to_owned())
            }
            Act::Left => {
                caret = caret.saturating_sub(1);
                Some(at_char(&text, caret))
            }
            Act::Right => {
                caret = (caret + 1).min(text.len());
                Some(at_char(&text, caret))
            }
            Act::Home => {
                caret = 0;
                Some(at_char(&text, caret))
            }
            Act::End => {
                caret = text.len();
                Some(at_char(&text, caret))
            }
        };
        let mark = f.w.mark();
        let (t_key, consumed) = match act {
            Act::Letter(c) => f.key(char_key(c), c as u32, 0)?,
            Act::Space => f.key(SPACE, 0x20, 0)?,
            Act::Back => f.key(BACKSPACE, 0, 0)?,
            Act::Emoji => f.key(F9, 0, 0)?,
            Act::Cyr => f.key(F10, 0, 0)?,
            Act::Left => f.key(LEFT, 0, 0)?,
            Act::Right => f.key(RIGHT, 0, 0)?,
            Act::Home => f.key(HOME, 0, 0)?,
            Act::End => f.key(END, 0, 0)?,
        };
        if consumed {
            swallowed += 1;
            out.notes.push(format!(
                "FE2 #{i} {act:?}: gorca took the key (not passed to the page)"
            ));
        }
        // Wait for the speech, then a little longer to catch a second one.
        let end = Instant::now() + Duration::from_millis(if expect.is_some() { 900 } else { 450 });
        while Instant::now() < end {
            sleep(15);
            if expect.is_some() && !f.heard(mark).is_empty() {
                break;
            }
        }
        sleep(if expect.is_some() { 130 } else { 0 });
        // Function keys are named by the generic key echo, which is not under test here.
        let heard: Vec<String> = f
            .heard(mark)
            .into_iter()
            .filter(|(t, _)| *t >= t_key)
            .map(|(_, s)| s)
            .filter(|s| !matches!(s.as_str(), "f 9" | "f 10"))
            .collect();
        // The page's own caret against the model's: the first divergence names the action.
        let raw = text[..caret.min(text.len())]
            .iter()
            .map(|c| c.len_utf16())
            .sum::<usize>();
        let mut page = f.ax_num("caret", &path, "caret").unwrap_or(-1.0);
        // A loaded Firefox applies the key a moment later.
        for _ in 0..4 {
            if page as i64 == raw as i64 {
                break;
            }
            sleep(60);
            page = f.ax_num("caret", &path, "caret").unwrap_or(-1.0);
        }
        if page as i64 != raw as i64 {
            resync += 1;
            if drift_notes < 6 {
                drift_notes += 1;
                out.notes.push(format!(
                    "FE2 #{i} {act:?}: the page caret is {page}, the model {raw} (not scored, model reset to the page); recent: {recent}"
                ));
            }
            if let Ok(pt) = field_text(&mut f, &path) {
                text = pt.chars().collect();
                let mut units = 0usize;
                caret = text.len();
                for (k, c) in text.iter().enumerate() {
                    if units as i64 >= page as i64 {
                        caret = k;
                        break;
                    }
                    units += c.len_utf16();
                }
            }
            sleep(40);
            continue;
        }
        if heard.iter().any(|s| {
            matches!(
                s.as_str(),
                "backspace" | "delete" | "left" | "right" | "home" | "end"
            )
        }) {
            keyname += 1;
        }
        match &expect {
            Some(e) => {
                let hits = heard.iter().filter(|s| *s == e).count();
                if hits == 0 && heard.is_empty() {
                    missing += 1;
                    out.notes
                        .push(format!("FE2 #{i} {act:?}: nothing said, wanted {e:?}"));
                } else if hits == 0 {
                    wrong += 1;
                    out.notes
                        .push(format!("FE2 #{i} {act:?}: wanted {e:?}, said {heard:?}"));
                } else if hits > 1 || heard.len() > 1 {
                    double += 1;
                    out.notes.push(format!(
                        "FE2 #{i} {act:?}: wanted {e:?} once, said {heard:?}; recent: {recent}"
                    ));
                }
            }
            None => {
                if !heard.is_empty() && !heard.iter().all(|s| s == "backspace") {
                    wrong += 1;
                    out.notes
                        .push(format!("FE2 #{i} {act:?}: wanted silence, said {heard:?}"));
                }
            }
        }
        // Commits closer than the insert gap (EV-5) are spoken as one: by design, so keep them
        // apart in time.
        sleep(if matches!(act, Act::Emoji | Act::Cyr) {
            320
        } else {
            40
        });
    }
    out.notes.push(format!(
        "FE2 first actions: {}",
        history
            .iter()
            .take(14)
            .cloned()
            .collect::<Vec<_>>()
            .join(" | ")
    ));
    // The model and the page must agree, or the harness itself drifted.
    let page = field_text(&mut f, &path)?;
    let model: String = text.iter().collect();
    let mut failures = 0u64;
    if page != model {
        failures += 1;
        out.notes
            .push(format!("FE2 the page holds {page:?}, the model {model:?}"));
    }
    out.x("e2_missing", missing as f64, "actions");
    out.x("e2_double", double as f64, "actions");
    out.x("e2_keyname", keyname as f64, "actions");
    out.x("e2_wrong", wrong as f64, "actions");
    out.x("ft_failures", failures as f64, "checks");
    out.x("e2_resync", resync as f64, "actions");
    out.x("e2_swallowed", swallowed as f64, "keys");
    Ok(())
}

/// The text of an entry, without the phantoms Gecko adds after astral characters.
fn field_text(f: &mut Ft, path: &str) -> Result<String> {
    let r = f.w.ax.request(
        "text",
        &format!("\"bus\":\"{}\",\"path\":\"{path}\"", f.w.bus),
        Duration::from_secs(5),
    )?;
    let hex = super::apps::json_str(&r, "hex").unwrap_or_default();
    let bytes: Vec<u8> = (0..hex.len() / 2)
        .filter_map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok())
        .collect();
    let t = String::from_utf8_lossy(&bytes).into_owned();
    Ok(t.chars().filter(|c| *c != '\u{FEFF}').collect())
}

// ---------------------------------------------------------------------------------------------
// Selection

/// FS2: Shift+Right then Shift+Left over the alphabet: only the change is spoken.
fn fs2(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    out: &mut RunOut,
) -> Result<()> {
    let mut f = Ft::open(host, reader, flavor, "fields.html", "M3b Fields", CFG_QUIET)?;
    f.enter("Select")?;
    let alphabet: Vec<char> = ('a'..='z').collect();
    let cycles = (size.n / 40).clamp(2, 12);
    let mut probes: Vec<Probe> = Vec::new();
    let mut failures = 0u64;
    for _ in 0..cycles {
        // To the start (Home speaks "a" or nothing at 0; not scored).
        let (t, _) = f.key(HOME, 0, 0)?;
        probes.push(Probe {
            t_key: t,
            t_ev: 0,
            expect: String::new(),
        });
        sleep(250);
        for c in alphabet.iter().take(20) {
            let (t, _) = f.key(RIGHT, 0, SHIFT)?;
            probes.push(Probe {
                t_key: t,
                t_ev: 0,
                expect: format!("{c} selected"),
            });
            sleep(200);
        }
        for (i, c) in alphabet.iter().take(20).rev().enumerate() {
            let (t, _) = f.key(LEFT, 0, SHIFT)?;
            // Releasing the last one collapses the selection onto the caret: the toolkit says
            // what it says there; only the changes before it are scored.
            probes.push(Probe {
                t_key: t,
                t_ev: 0,
                expect: if i < 19 {
                    format!("{c} unselected")
                } else {
                    String::new()
                },
            });
            sleep(200);
        }
        sleep(200);
    }
    sleep(400);
    let (lat, lost, _) = finish(&mut f, "FS2", &mut probes, true, out);
    if lost > 0 {
        failures += 1;
    }
    out.s("fs2_ms", lat);
    out.x("lost", lost as f64, "keys");
    let _ = failures;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Say-all in a field

/// One sentence of the say-all text is 30 scalars: `Sentence number 0000 is here. `.
const SENTENCE: usize = 30;

/// FSA1 (`stop == false`): `gorca+Down` from the top of a long text. FSA3 (`stop == true`): a
/// key during the reading silences it and leaves the caret at the start of the chunk being read.
fn fsa(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
    stop: bool,
) -> Result<()> {
    let id = if stop { "FSA3" } else { "FSA1" };
    let mut f = Ft::open(host, reader, flavor, "fields.html", "M3b Fields", CFG_QUIET)?;
    let path = f.enter("Say all text")?;
    f.w.rig.mock.configure(
        Duration::from_millis(1),
        Duration::from_millis(if stop { 400 } else { 30 }),
    );
    let trials = if stop { 8 } else { size.trials.clamp(8, 30) };
    let mut rng = Rng::new(seed);
    let (mut lat, mut cancel, mut lost, mut caret_wrong, mut not_silenced) =
        (Vec::new(), Vec::new(), 0u64, 0u64, 0u64);
    for _ in 0..trials {
        // The caret to the top: Ctrl+Home (the user did it, the reader says the line; then quiet).
        f.key(HOME, 0, CONTROL)?;
        sleep(450);
        let mark = f.w.mark();
        let (t, consumed) = f.chord(DOWN)?;
        if !consumed {
            bail!("{id}: gorca+Down was not grabbed: is text.say_all bound?");
        }
        let first = f.w.speech(mark, Duration::from_secs(3), |t| {
            t.starts_with("Sentence number 0000")
        });
        match first {
            Some(r) => lat.push(ms(r.t_ns.saturating_sub(t))),
            None => {
                lost += 1;
                if lost <= 2 {
                    f.w.dump_loss("fsa");
                }
                // End the reading, whatever it is doing.
                f.key(F8, 0, 0)?;
                sleep(300);
                continue;
            }
        }
        if stop {
            sleep(900 + rng.below(700));
            let mark2 = f.w.mark();
            let (tk, _) = f.key(F8, 0, 0)?;
            match f
                .w
                .rig
                .mock
                .wait_for(mark2, Duration::from_millis(500), |r| {
                    matches!(r.kind, Kind::Cancel { .. })
                }) {
                Some(r) => cancel.push(ms(r.t_ns.saturating_sub(tk))),
                None => not_silenced += 1,
            }
            sleep(400);
            let caret = f.ax_num("caret", &path, "caret")? as usize;
            if caret == 0 || !caret.is_multiple_of(SENTENCE) {
                caret_wrong += 1;
                out.notes
                    .push(format!("{id}: the caret is at {caret} after the stop"));
            }
        } else {
            f.key(F8, 0, 0)?;
            sleep(400);
        }
    }
    if stop {
        out.s("fsa3_stop_ms", cancel);
        out.x("sa3_caret_wrong", caret_wrong as f64, "events");
        out.x("sa3_not_silenced", not_silenced as f64, "events");
        out.x("lost", lost as f64, "events");
    } else {
        out.s("fsa1_ms", lat);
        out.x("lost", lost as f64, "events");
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// A contenteditable region

/// FCE1: the caret moves between the paragraphs of a contenteditable region, typing and
/// BackSpace are spoken from the paragraph's own events.
fn fce1(host: &HostEnv, reader: &ReaderSpec, flavor: Flavor, out: &mut RunOut) -> Result<()> {
    let mut f = Ft::open(host, reader, flavor, "fields.html", "M3b Fields", CFG_QUIET)?;
    f.enter("Editable region:")?;
    let mut probes: Vec<Probe> = Vec::new();
    let mut failures = 0u64;
    // "One 😀 two" then "Три четыре": Right across the first paragraph and into the second.
    let rights: [&str; 12] = [
        "n",
        "e",
        "space",
        "\u{1F600}",
        "space",
        "t",
        "w",
        "o",
        "blank",
        "\u{422}",
        "\u{440}",
        "\u{438}",
    ];
    // The caret starts before the "O" of the first paragraph; the first Right lands on "n".
    for e in rights {
        let (t, _) = f.key(RIGHT, 0, 0)?;
        probes.push(Probe {
            t_key: t,
            t_ev: 0,
            expect: e.to_owned(),
        });
        sleep(160);
    }
    // Typing at that place, then BackSpace.
    for c in ['x', 'y'] {
        let (t, _) = f.key(char_key(c), c as u32, 0)?;
        probes.push(Probe {
            t_key: t,
            t_ev: 0,
            expect: c.to_string(),
        });
        sleep(160);
    }
    for c in ['y', 'x'] {
        let (t, _) = f.key(BACKSPACE, 0, 0)?;
        probes.push(Probe {
            t_key: t,
            t_ev: 0,
            expect: c.to_string(),
        });
        sleep(160);
    }
    // And back up: Up speaks the first paragraph's line.
    let (t, _) = f.key(UP, 0, 0)?;
    probes.push(Probe {
        t_key: t,
        t_ev: 0,
        expect: "One \u{1F600} two".to_owned(),
    });
    sleep(500);
    let (lat, lost, doubled) = finish(&mut f, "FCE1", &mut probes, true, out);
    if lost > 0 || doubled > 0 {
        failures += 1;
    }
    out.s("fce1_ms", lat);
    out.x("lost", lost as f64, "keys");
    out.x("fc_double", doubled as f64, "keys");
    out.x("ft_failures", failures as f64, "checks");
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Browse <-> focus mode with a field

fn said_once(f: &Ft, mark: usize, want: &str, wait: Duration) -> bool {
    let end = Instant::now() + wait;
    while Instant::now() < end {
        if f.heard(mark).iter().any(|(_, s)| s == want) {
            sleep(150);
            return f.heard(mark).iter().filter(|(_, s)| s == want).count() == 1;
        }
        sleep(15);
    }
    false
}

/// FT1: into the field (focus mode), typing is echoed; Escape (browse mode); Enter (focus mode
/// again); typing and BackSpace are echoed once, so the layer was not left with a stale window.
fn ft1(host: &HostEnv, reader: &ReaderSpec, flavor: Flavor, out: &mut RunOut) -> Result<()> {
    let mut f = Ft::open(host, reader, flavor, "fields.html", "M3b Fields", CFG_QUIET)?;
    let mut fails: Vec<String> = Vec::new();
    f.enter("Typing")?;
    let mark = f.w.mark();
    f.key(char_key('x'), 'x' as u32, 0)?;
    if !said_once(&f, mark, "x", Duration::from_secs(2)) {
        fails.push("typing after the focus: no single \"x\"".into());
    }
    // Escape: browse mode.
    let mark = f.w.mark();
    let (_, took) = f.key(ESCAPE, 0, 0)?;
    if !took
        || !f.hears(mark, Duration::from_secs(2), |t| {
            t.starts_with("browse mode")
        })
    {
        fails.push(format!("Escape: took={took}, heard {:?}", f.heard(mark)));
    }
    sleep(300);
    // Enter in browse mode on the field: focus mode, the field gets the focus.
    let mark = f.w.mark();
    let (_, took) = f.key(ENTER, 0, 0)?;
    if !took || !f.hears(mark, Duration::from_secs(3), |t| t.contains("focus mode")) {
        fails.push(format!("Enter: took={took}, heard {:?}", f.heard(mark)));
    }
    sleep(700);
    let _ = f.w.settle_grabs();
    let mark = f.w.mark();
    f.key(char_key('y'), 'y' as u32, 0)?;
    if !said_once(&f, mark, "y", Duration::from_secs(2)) {
        fails.push(format!(
            "typing after Enter: no single \"y\"; heard {:?}",
            f.heard(mark)
        ));
    }
    sleep(200);
    let mark = f.w.mark();
    f.key(BACKSPACE, 0, 0)?;
    if !said_once(&f, mark, "y", Duration::from_secs(2)) {
        fails.push(format!(
            "BackSpace after Enter: no single \"y\"; heard {:?}",
            f.heard(mark)
        ));
    }
    sleep(200);
    // Left speaks the character before the caret: the "x" typed first.
    let mark = f.w.mark();
    f.key(LEFT, 0, 0)?;
    if !said_once(&f, mark, "x", Duration::from_secs(2)) {
        fails.push(format!(
            "Left after Enter: no single \"x\"; heard {:?}",
            f.heard(mark)
        ));
    }
    for m in &fails {
        out.notes.push(format!("FT1 FAIL {m}"));
    }
    out.x("ft_failures", fails.len() as f64, "checks");
    Ok(())
}

/// FP1: with a popup open in the field, the first Escape belongs to the page (it closes the
/// popup) and the second leaves focus mode.
fn fp1(host: &HostEnv, reader: &ReaderSpec, flavor: Flavor, out: &mut RunOut) -> Result<()> {
    let mut f = Ft::open(host, reader, flavor, "fields.html", "M3b Fields", CFG_QUIET)?;
    let mut fails: Vec<String> = Vec::new();
    let path = f.enter_role("combo box", "Combo")?;
    // Alt+Down opens the popup (the page's own widget).
    let (_, took) = f.key(DOWN, 0, ALT)?;
    if took {
        fails.push("Alt+Down was swallowed".into());
    }
    sleep(900);
    let expanded = f.ax_num("has_state", &path, "has").unwrap_or(0.0);
    if expanded < 0.5 {
        fails.push("the popup did not open (aria-expanded is false)".into());
    }
    let mark = f.w.mark();
    let (_, took) = f.key(ESCAPE, 0, 0)?;
    if took {
        fails.push("the first Escape was taken by gorca while the popup was open".into());
    }
    sleep(600);
    if f.hears(mark, Duration::from_millis(100), |t| {
        t.contains("browse mode")
    }) {
        fails.push("\"browse mode\" was said for the Escape that closes the popup".into());
    }
    let closed = f.ax_num("has_state", &path, "has").unwrap_or(1.0);
    if closed > 0.5 {
        fails.push("the page did not get the Escape (still expanded)".into());
    }
    let mark = f.w.mark();
    let (_, took) = f.key(ESCAPE, 0, 0)?;
    if !took
        || !f.hears(mark, Duration::from_secs(2), |t| {
            t.starts_with("browse mode")
        })
    {
        fails.push(format!(
            "the second Escape: took={took}, heard {:?}",
            f.heard(mark)
        ));
    }
    for m in &fails {
        out.notes.push(format!("FP1 FAIL {m}"));
    }
    out.x("ft_failures", fails.len() as f64, "checks");
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Browse mode: say-all, auto-read, layouts, frames

/// The number N of the first "Paragraph N" in a spoken text.
fn paragraph_no(t: &str) -> Option<u32> {
    let i = t.find("Paragraph ")? + "Paragraph ".len();
    let digits: String = t[i..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// BSA1: `gorca+Down` at the top of a long page reads it; the cursor follows the reading; a
/// browse key stops it and continues from the chunk being read.
fn bsa1(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    out: &mut RunOut,
) -> Result<()> {
    let mut f = Ft::open(
        host,
        reader,
        flavor,
        "long.html",
        "M7b Long article",
        CFG_QUIET,
    )?;
    f.w.rig
        .mock
        .configure(Duration::from_millis(1), Duration::from_millis(120));
    let mut fails: Vec<String> = Vec::new();
    let mut lat: Vec<f64> = Vec::new();
    let trials = size.trials.clamp(4, 12);
    for trial in 0..trials {
        // Top of the page: Ctrl+Home is a browse key.
        let (_, took) = f.key(HOME, 0, CONTROL)?;
        if !took {
            fails.push("Ctrl+Home was not a browse key".into());
        }
        sleep(500);
        let mark = f.w.mark();
        let (t, consumed) = f.chord(DOWN)?;
        if !consumed {
            fails.push("gorca+Down was not grabbed".into());
            continue;
        }
        let first = f.w.speech(mark, Duration::from_secs(3), |t| {
            t.contains("heading level 1") && t.contains("The long article")
        });
        match first {
            Some(r) => lat.push(ms(r.t_ns.saturating_sub(t))),
            None => {
                fails.push(format!(
                    "trial {trial}: the page did not start reading; heard {:?}",
                    f.heard(mark)
                ));
                f.key(ESCAPE, 0, 0)?;
                sleep(300);
                continue;
            }
        }
        // Let it read for a while, then stop it with a browse key.
        sleep(2200);
        let mark2 = f.w.mark();
        let chunks: Vec<String> = f.heard(mark).into_iter().map(|(_, s)| s).collect();
        let last = chunks.iter().rev().find_map(|c| paragraph_no(c));
        let (_, took) = f.key(DOWN, 0, 0)?;
        if !took {
            fails.push("Down was not a browse key".into());
        }
        let after =
            f.w.speech(mark2, Duration::from_secs(2), |t| paragraph_no(t).is_some());
        match (last, after) {
            (Some(l), Some(r)) => {
                let said = match &r.kind {
                    Kind::Speak { text, .. } => text.clone(),
                    _ => String::new(),
                };
                let n = paragraph_no(&said).unwrap_or(0);
                // The cursor was on the chunk being read; Down moves to the line after it.
                if n < l {
                    fails.push(format!(
                        "trial {trial}: stopped while reading paragraph {l}, Down said paragraph {n}"
                    ));
                }
                if n > l + 2 {
                    fails.push(format!(
                        "trial {trial}: the cursor ran ahead: reading {l}, Down said paragraph {n}"
                    ));
                }
            }
            (None, _) => fails.push(format!(
                "trial {trial}: no paragraph was read in 2.2 s: {chunks:?}"
            )),
            (Some(_), None) => fails.push(format!("trial {trial}: Down said nothing")),
        }
        // Silence afterwards: the reading is over.
        sleep(500);
        let mark3 = f.w.mark();
        sleep(900);
        let more = f.heard(mark3);
        if !more.is_empty() {
            fails.push(format!("trial {trial}: speech after the stop: {more:?}"));
        }
    }
    for m in &fails {
        out.notes.push(format!("BSA1 FAIL {m}"));
    }
    out.s("bsa1_ms", lat);
    out.x("bsa_failures", fails.len() as f64, "checks");
    Ok(())
}

/// BAR1: a page loads (default settings): its title, then it reads itself from the top; a key
/// stops it. A second load by a link behaves the same.
fn bar1(host: &HostEnv, reader: &ReaderSpec, flavor: Flavor, out: &mut RunOut) -> Result<()> {
    let url = file_url(&pages(host).join("long.html"))?;
    let (w, doc) = Web::start_cfg(
        host,
        reader,
        flavor,
        &[url],
        "M7b Long article",
        CFG_DEFAULT,
    )?;
    w.rig
        .mock
        .configure(Duration::from_millis(1), Duration::from_millis(120));
    let mut fails: Vec<String> = Vec::new();
    let mut f = Ft {
        w,
        doc: doc.clone(),
        woken: false,
    };
    // gorca attached to a page that was already open: no load happened, so nothing reads.
    f.w.prime(&doc)?;
    sleep(2500);
    let quiet: Vec<(u64, String)> = f.heard(0);
    if quiet.iter().any(|(_, t)| paragraph_no(t).is_some()) {
        fails.push(format!(
            "a page that was already open read itself: {quiet:?}"
        ));
    }
    // A load by a link: the last link of the page is "Back to the top" (this very page).
    let link = f.w.find(&doc.path, "link", "Back to the top")?;
    let t_load0 = super::mono::now_ns();
    let mark = f.w.mark();
    f.w.action(&link)?;
    let started =
        f.w.speech(mark, Duration::from_secs(15), |t| paragraph_no(t).is_some());
    match started {
        Some(r) => out.x("bar_start_ms", ms(r.t_ns.saturating_sub(t_load0)), "ms"),
        None => fails.push(format!(
            "the loaded page did not read itself; heard {:?}",
            f.heard(mark)
        )),
    }
    sleep(1800);
    let heard = f.heard(mark);
    let texts: Vec<&str> = heard.iter().map(|(_, s)| s.as_str()).collect();
    if !texts.iter().any(|t| t.contains("M7b Long article")) {
        fails.push(format!(
            "the title was not spoken after the load: {texts:?}"
        ));
    }
    let paragraphs: Vec<u32> = texts.iter().filter_map(|t| paragraph_no(t)).collect();
    if paragraphs.len() < 2 || paragraphs.windows(2).any(|p| p[1] < p[0]) {
        fails.push(format!(
            "the page did not read itself in order: {paragraphs:?}"
        ));
    }
    // A key stops it, and nothing starts again.
    let (_, took) = f.key(DOWN, 0, 0)?;
    if !took {
        fails.push("Down was not a browse key".into());
    }
    sleep(700);
    let mark2 = f.w.mark();
    sleep(1200);
    if !f.heard(mark2).is_empty() {
        fails.push(format!("speech after the key: {:?}", f.heard(mark2)));
    }
    for m in &fails {
        out.notes.push(format!("BAR1 FAIL {m}"));
    }
    out.x("bar_failures", fails.len() as f64, "checks");
    Ok(())
}

/// BL1: browse keys on the Russian layout (the physical key of `h` types `р`, `k` types `л`)
/// and Shift+digit on both layouts.
fn bl1(host: &HostEnv, reader: &ReaderSpec, flavor: Flavor, out: &mut RunOut) -> Result<()> {
    let mut h = host.clone();
    if !h.reader_env.iter().any(|(k, _)| k == "GORCA_LAYOUTS") {
        h.reader_env
            .push(("GORCA_LAYOUTS".to_owned(), "us,ru".to_owned()));
    }
    let url = file_url(&pages_dir(host).join("article.html"))?;
    let (mut w, doc) = Web::start_cfg(&h, reader, flavor, &[url], "M7 Article", CFG_QUIET)?;
    w.prime(&doc)?;
    let mut f = Ft {
        w,
        doc,
        woken: false,
    };
    let mut fails: Vec<String> = Vec::new();
    // (physical key, keysym on the layout, unichar, state, what must be said)
    let any = |_: &str| true;
    let _ = any;
    let steps: [(&str, K, u32, u32, &str); 7] = [
        // The US layout first: a key a US keyboard types must still be grabbed.
        ("us H", k(35, 0x68), 0x68, 0, "heading level 2"),
        // Shift+2 types @ on the US layout.
        (
            "us Shift+2",
            k(3, 0x40),
            0x40,
            SHIFT,
            "no previous heading level 2",
        ),
        // The physical H on the Russian layout: Cyrillic_er.
        ("ru H", k(35, 0x6d2), 0x440, 0, "heading level 2"),
        // Shift+2 types a double quote on the Russian layout.
        ("ru Shift+2", k(3, 0x22), 0x22, SHIFT, "Introduction"),
        ("ru 2", k(3, 0x32), 0x32, 0, "heading level 2"),
        // The physical K: Cyrillic_el.
        ("ru K", k(37, 0x6cc), 0x43b, 0, "link"),
        // Shift+H on the Russian layout: Cyrillic_ER.
        ("ru Shift+H", k(35, 0x6f2), 0x420, SHIFT, "heading level 2"),
    ];
    for (name, key, uni, state, want) in steps {
        f.w.settle_grabs()?;
        let mark = f.w.mark();
        let (_, took) = f.key(key, uni, state)?;
        if !took {
            fails.push(format!(
                "{name}: the key was not grabbed (it reached the page)"
            ));
            continue;
        }
        if !f.hears(mark, Duration::from_secs(3), |t| t.contains(want)) {
            fails.push(format!(
                "{name}: wanted {want:?}, heard {:?}",
                f.heard(mark)
            ));
        }
        sleep(120);
    }
    for m in &fails {
        out.notes.push(format!("BL1 FAIL {m}"));
    }
    if !fails.is_empty() {
        let said: Vec<String> = f.heard(0).into_iter().map(|(_, t)| t).collect();
        out.notes.push(format!("BL1 transcript: {said:?}"));
    }
    out.x("bl_failures", fails.len() as f64, "checks");
    Ok(())
}

/// FR1: frames and articles on a page with an iframe.
fn fr1(host: &HostEnv, reader: &ReaderSpec, flavor: Flavor, out: &mut RunOut) -> Result<()> {
    // Articles have no key in the shipped keymaps; bind one the way a user would.
    let cfg = format!(
        "{CFG_QUIET}[keymap.browse]\n\"w\" = \"quicknav.next.article\"\n\"shift+w\" = \"quicknav.prev.article\"\n"
    );
    let mut f = Ft::open(host, reader, flavor, "frames.html", "M7b Frames", &cfg)?;
    let mut fails: Vec<String> = Vec::new();
    let m_key = k(50, 0x6d);
    let w_key = k(17, 0x77);
    step(&mut f, &mut fails, "W", w_key, 0, "article")?;
    step(&mut f, &mut fails, "W again", w_key, 0, "article")?;
    step(&mut f, &mut fails, "M", m_key, 0, "frame")?;
    step(&mut f, &mut fails, "M again", m_key, 0, "no next frame")?;
    step(&mut f, &mut fails, "Shift+W", w_key, SHIFT, "article")?;
    step(
        &mut f,
        &mut fails,
        "Shift+M",
        m_key,
        SHIFT,
        "no previous frame",
    )?;
    for m in &fails {
        out.notes.push(format!("FR1 FAIL {m}"));
    }
    out.x("fr_failures", fails.len() as f64, "checks");
    Ok(())
}

fn step(
    f: &mut Ft,
    fails: &mut Vec<String>,
    name: &str,
    key: K,
    state: u32,
    want: &str,
) -> Result<()> {
    f.w.settle_grabs()?;
    let mark = f.w.mark();
    // Shift types the capital.
    let sym = if state & SHIFT != 0 && (0x61..=0x7a).contains(&key.sym) {
        key.sym - 0x20
    } else {
        key.sym
    };
    let (_, took) = f.key(
        K {
            code: key.code,
            sym,
        },
        sym,
        state,
    )?;
    if !took {
        fails.push(format!("{name}: not grabbed"));
        return Ok(());
    }
    if !f.hears(mark, Duration::from_secs(3), |t| t.contains(want)) {
        fails.push(format!(
            "{name}: wanted {want:?}, heard {:?}",
            f.heard(mark)
        ));
    }
    sleep(150);
    Ok(())
}
