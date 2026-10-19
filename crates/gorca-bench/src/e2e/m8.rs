// SPDX-License-Identifier: GPL-3.0-or-later
//! M8 gate harness: NVDA browse-mode parity scenarios (docs/plan/m8-spec.md).
//!
//! The runner is data driven. A [`Scenario`] is one corpus page (`tests/web/m8/pages/`) plus an
//! ordered list of [`Step`]s. Each step does one thing (a key, a chord, an AT-SPI focus or
//! action, keys sent to Firefox itself), collects the speech that follows for a short window and
//! compares it with what NVDA's rules say it should be (wording adapted: matching is on
//! lower-cased tokens, so the word order is not part of the contract, the words are).
//!
//! ```text
//!   step.act ──► emulated Mutter / AT-SPI driver / compositor keys ──► Firefox + gorca daemon
//!        │                                                                   │
//!        └──────── t_emit ──────────────  mock speech server  ◄──────────────┘
//!                                          first SPEAK, all SPEAKs in the window
//! ```
//!
//! A step ends in one [`Status`]:
//!
//! | status | meaning | counts against the gate |
//! |---|---|---|
//! | `Pass` | every token present, nothing forbidden | no |
//! | `Gap` | the core tokens are present, the `extra` tokens (NVDA parity extras) are not | no (reported) |
//! | `XFail` | wrong, and the step is marked expected-fail (feature not built yet) | no (reported) |
//! | `XPass` | marked expected-fail, but it passes: the mark must be removed | no (reported; promote) |
//! | `Fail` | wrong and not marked | **yes** (`fail` must be 0 in every run) |
//!
//! Latency is the emit time of the stimulus to the first SPEAK of the step, recorded only for
//! passing steps whose [`Gate`] names a budget (ADR-0003 14): `key_ms` p99 5 ms, `mode_ms` p99
//! 15 ms, `live_ms` p99 65 ms plus the rig allowance (see `bin/e2e_m8.rs`).
//!
//! Skipped unless `--reader-env GORCA_M8=1` and the private compositor is up
//! (`tests/web/m8/run_m8.sh` sets both); `GORCA_M8_STRICT=1` fails instead of skipping.

#[path = "m8_baseline.rs"]
mod baseline;
#[path = "m8_steps.rs"]
mod steps;

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::{DocRef, FOCUS, Web, file_url, key_series, latencies, speaks_since};
use crate::e2e::apps::{PYTHON, json_str};
use crate::e2e::m1::{K, flag};
use crate::e2e::mock::Flavor;
use crate::e2e::mono::now_ns;
use crate::e2e::runner::HostEnv;
use crate::e2e::scenarios::{RunOut, Size, ms};

pub use steps::scenarios;

/// Priority of a step: P0 is part of the M8 acceptance checklist, P1 is expected from NVDA users
/// every day, P2 is polish.
pub const P0: u8 = 0;
pub const P1: u8 = 1;
pub const P2: u8 = 2;

/// AT-SPI role numbers (`AtspiRole`, stable ABI) used to find controls in the corpus pages.
pub mod role {
    pub const CHECK_BOX: i32 = 7;
    pub const CHECK_MENU_ITEM: i32 = 8;
    pub const COMBO_BOX: i32 = 11;
    pub const LIST: i32 = 31;
    pub const MENU_ITEM: i32 = 35;
    pub const PAGE_TAB: i32 = 37;
    pub const PASSWORD_TEXT: i32 = 40;
    pub const PUSH_BUTTON: i32 = 43;
    pub const RADIO_BUTTON: i32 = 44;
    pub const RADIO_MENU_ITEM: i32 = 45;
    pub const SLIDER: i32 = 51;
    pub const SPIN_BUTTON: i32 = 52;
    pub const TABLE_CELL: i32 = 56;
    pub const TOGGLE_BUTTON: i32 = 62;
    pub const TREE: i32 = 65;
    pub const TREE_ITEM: i32 = 91;
    pub const APPLICATION: i32 = 75;
    pub const ENTRY: i32 = 79;
    pub const LINK: i32 = 88;
    pub const ANY: i32 = -1;
}

/// What a step does.
#[derive(Clone, Copy, Debug)]
pub(super) enum Act {
    /// A key press and release through the emulated Mutter (the reader's key path).
    Key(K, u32),
    /// The same key `n` times, 70 ms apart; the speech of all presses is judged together.
    Repeat(K, u32, u32),
    /// The gorca modifier (Insert) held while a key is pressed: modifier, key, modifier state.
    Chord(K, K, u32),
    /// evdev key codes typed into Firefox itself through the compositor (Tab, Alt+Left, Ctrl+C):
    /// the reader does not see them as key events, only their effects (focus, caret, selection).
    AppKeys(&'static [u32]),
    /// AT-SPI `GrabFocus` on the first node with this role and name fragment: what Tab ends in.
    Focus(i32, &'static str),
    /// `GrabFocus` on the n-th (0-based) focusable node of the document in document order: what
    /// the n-th Tab press ends in, whatever the node's name is (label-source tests: the name may
    /// be empty, so it cannot be the key).
    FocusNth(usize),
    /// AT-SPI `DoAction(0)` on that node: a click without moving the pointer.
    Do(i32, &'static str),
    /// Focus the document and press Ctrl+Home: a known cursor position for the next block.
    Top,
    /// A click on the page area through the compositor (gives the content the keyboard focus,
    /// which `AppKeys` needs: without it Tab walks the browser chrome).
    Click,
    /// Sleep; nothing is scored.
    Wait(u64),
    /// Set the mock speech server's utterance length (ms); say-all needs speech that takes time.
    Speak(u64),
    /// A key press only (no release), a Mutter autorepeat of it, its release.
    PrbDown(K, u32),
    PrbRep(K, u32),
    PrbUp(K, u32),
}

/// The latency budget a passing step is scored against.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Gate {
    /// Not scored.
    Info,
    /// Key to speech: ADR-0003 14, p99 5 ms gorca side.
    Key,
    /// Mode switch to speech: p99 15 ms.
    Mode,
    /// Last mutation to speech: LV1.
    Live,
}

/// One scripted check.
#[derive(Clone, Debug)]
pub(super) struct Step {
    /// Stable id, `AREA.topic.n`; used in reports and in `--steps`.
    pub id: &'static str,
    pub prio: u8,
    /// What NVDA does, in one line (also the report text).
    pub what: &'static str,
    pub act: Act,
    /// Tokens that must all be in the speech of the window (case and spacing insensitive).
    pub say: &'static [&'static str],
    /// Tokens that must not be.
    pub not: &'static [&'static str],
    /// Each of these occurs exactly once in what was heard (the role word is said once).
    pub once: &'static [&'static str],
    /// NVDA parity extras (field-diff words, counts): missing extras make the step a `Gap`.
    pub extra: &'static [&'static str],
    pub extra_why: &'static str,
    /// The whole speech of the window must equal this (after normalising).
    pub exact: Option<&'static str>,
    /// At most this many utterances in the window.
    pub max_say: Option<usize>,
    /// At most this many characters spoken in the whole step (a hostile page's megabyte name
    /// must be clipped, never read whole).
    pub max_chars: Option<usize>,
    /// At least this many (the reader must answer).
    pub min_say: Option<usize>,
    /// `Some(false)`: the key must reach the application (not consumed).
    pub consumed: Option<bool>,
    /// No focus event may reach the bus during the step (the browse cursor never moves focus).
    pub no_focus_events: bool,
    pub window_ms: u64,
    pub gate: Gate,
    /// Expected to fail today; the text says why.
    pub xfail: Option<&'static str>,
    /// A positioning or configuration step: it runs, but is neither judged nor reported.
    pub setup: bool,
}

impl Step {
    pub fn new(
        id: &'static str,
        prio: u8,
        what: &'static str,
        act: Act,
        say: &'static [&'static str],
    ) -> Self {
        Self {
            id,
            prio,
            what,
            act,
            say,
            not: &[],
            once: &[],
            extra: &[],
            extra_why: "",
            exact: None,
            max_say: None,
            max_chars: None,
            min_say: None,
            consumed: None,
            no_focus_events: false,
            window_ms: 700,
            gate: Gate::Info,
            xfail: None,
            setup: false,
        }
    }

    /// A step that only positions the cursor or sets the rig up.
    pub fn setup(id: &'static str, what: &'static str, act: Act) -> Self {
        let mut s = Self::new(id, P2, what, act, &[]);
        s.setup = true;
        s
    }

    #[must_use]
    pub fn not(mut self, t: &'static [&'static str]) -> Self {
        self.not = t;
        self
    }

    #[must_use]
    pub fn once(mut self, t: &'static [&'static str]) -> Self {
        self.once = t;
        self
    }

    #[must_use]
    pub fn extra(mut self, t: &'static [&'static str], why: &'static str) -> Self {
        self.extra = t;
        self.extra_why = why;
        self
    }

    #[must_use]
    pub fn exact(mut self, t: &'static str) -> Self {
        self.exact = Some(t);
        self
    }

    #[must_use]
    pub fn max(mut self, n: usize) -> Self {
        self.max_say = Some(n);
        self
    }

    #[must_use]
    pub fn chars(mut self, n: usize) -> Self {
        self.max_chars = Some(n);
        self
    }

    #[must_use]
    pub fn min(mut self, n: usize) -> Self {
        self.min_say = Some(n);
        self
    }

    #[must_use]
    pub fn passes_through(mut self) -> Self {
        self.consumed = Some(false);
        self
    }

    #[must_use]
    pub fn no_focus(mut self) -> Self {
        self.no_focus_events = true;
        self
    }

    #[must_use]
    pub fn win(mut self, ms: u64) -> Self {
        self.window_ms = ms;
        self
    }

    #[must_use]
    pub fn gate(mut self, g: Gate) -> Self {
        self.gate = g;
        self
    }

    #[must_use]
    pub fn xf(mut self, why: &'static str) -> Self {
        self.xfail = Some(why);
        self
    }
}

/// How a scenario runs.
#[derive(Clone, Copy, Debug)]
pub(super) enum Body {
    /// The scripted steps.
    Steps(fn() -> Vec<Step>),
    /// W1 at 100k nodes: alternating `H` / `Shift+H`, scored like W1 (gate 5 ms).
    W1Big,
    /// Table navigation at 12k cells: `T`, `Down`, then Ctrl+Alt+Down / Up alternating.
    TableBig,
}

#[derive(Clone, Copy, Debug)]
pub struct Scenario {
    /// `M8-QN`...
    id: &'static str,
    /// What it covers (matrix section).
    area: &'static str,
    /// Path under `tests/web/m8/pages/`.
    page: &'static str,
    /// The document title Firefox exposes.
    title: &'static str,
    body: Body,
    /// Expected-fail for the whole scenario (perf bodies); the text says why.
    xfail: Option<&'static str>,
}

impl Scenario {
    pub fn id(&self) -> &'static str {
        self.id
    }

    pub fn area(&self) -> &'static str {
        self.area
    }

    pub fn xfail(&self) -> Option<&'static str> {
        self.xfail
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Pass,
    Gap,
    XFail,
    XPass,
    Fail,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Gap => "GAP",
            Self::XFail => "XFAIL",
            Self::XPass => "XPASS",
            Self::Fail => "FAIL",
        }
    }
}

/// The outcome of one step, for the per-step report.
#[derive(Clone, Debug)]
pub struct StepRes {
    pub scenario: &'static str,
    pub id: &'static str,
    pub prio: u8,
    pub what: &'static str,
    pub status: Status,
    pub lat_ms: Option<f64>,
    pub heard: String,
    /// The expectation, as the tokens that were wanted.
    pub wanted: String,
    /// The expected-fail reason, the extras' reason, or the error text.
    pub why: String,
}

/// The scripted steps of a scenario with the baseline marks applied: a step that fails on `main`
/// for a reason recorded in `m8_baseline.rs` is expected-fail until that line is deleted.
fn steps_of(f: fn() -> Vec<Step>) -> Vec<Step> {
    let mut v = f();
    for s in &mut v {
        if s.xfail.is_none()
            && let Some(why) = baseline::reason(s.id)
        {
            s.xfail = Some(why);
        }
    }
    v
}

/// Lower-case, trim and collapse whitespace.
fn norm(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn has_all(heard: &str, toks: &[&str]) -> bool {
    toks.iter().all(|t| heard.contains(&norm(t)))
}

/// Pure judgement of one step from what was heard. `utterances` are the speech texts in order.
fn judge(st: &Step, utterances: &[String], consumed: bool, focus_events: usize) -> Status {
    let heard = norm(&utterances.join(" "));
    let core = has_all(&heard, st.say)
        && st.not.iter().all(|t| !heard.contains(&norm(t)))
        && st.once.iter().all(|t| heard.matches(&norm(t)).count() == 1)
        && st.exact.is_none_or(|e| heard == norm(e))
        && st.max_say.is_none_or(|m| utterances.len() <= m)
        && st
            .max_chars
            .is_none_or(|m| utterances.iter().map(|u| u.chars().count()).sum::<usize>() <= m)
        && st.min_say.is_none_or(|m| utterances.len() >= m)
        && st.consumed.is_none_or(|c| c == consumed)
        && (!st.no_focus_events || focus_events == 0);
    let extras = has_all(&heard, st.extra);
    match (core && extras, core, st.xfail.is_some()) {
        (true, _, false) => Status::Pass,
        (true, _, true) => Status::XPass,
        (false, true, false) => Status::Gap,
        (false, _, true) => Status::XFail,
        (false, false, false) => Status::Fail,
    }
}

// ---------------------------------------------------------------------------------------------
// Running

fn gate_open(host: &HostEnv, id: &str, out: &mut RunOut) -> Result<bool> {
    if flag(host, "GORCA_M8") && std::env::var_os("GORCA_M7_WAYLAND").is_some() {
        return Ok(true);
    }
    if flag(host, "GORCA_M8_STRICT") {
        bail!(
            "{id}: GORCA_M8_STRICT=1 requires GORCA_M8=1 and the private compositor (tests/web/m8/run_m8.sh)"
        );
    }
    out.notes.push(format!(
        "{id} SKIPPED: run tests/web/m8/run_m8.sh (sets GORCA_M8=1 and the private compositor)"
    ));
    out.x("skipped", 1.0, "flag");
    Ok(false)
}

fn pages_dir(host: &HostEnv) -> PathBuf {
    host.harness_dir.join("../web/m8/pages")
}

/// The URL of a corpus page; generated pages (`generated/...`) are produced on first use.
fn page_url(host: &HostEnv, page: &str) -> Result<String> {
    let p = pages_dir(host).join(page);
    if page.starts_with("generated/") && !p.exists() {
        let gen_script = host.harness_dir.join("../web/m8/gen_m8.py");
        let st = Command::new(PYTHON)
            .arg(gen_script)
            .arg(pages_dir(host).join("generated"))
            .status()?;
        if !st.success() {
            bail!("gen_m8.py failed");
        }
    }
    file_url(&p)
}

fn find_role(w: &mut Web, root: &str, role: i32, name: &str) -> Result<String> {
    let r = w.ax.request(
        "find",
        &format!(
            "\"bus\":\"{}\",\"root\":\"{root}\",\"role\":{role},\"name\":\"{name}\",\"partial\":true,\"timeout_ms\":6000",
            w.bus
        ),
        Duration::from_secs(10),
    )?;
    json_str(&r, "path").with_context(|| format!("no node role {role} name {name:?}: {r}"))
}

/// The gorca modifier held around one key. Returns (emit time of the key-down, consumed).
fn chord(w: &mut Web, modifier: K, key: K, state: u32) -> Result<(u64, bool)> {
    if w.unsettled {
        w.settle_grabs()?;
    }
    // Above Mutter's repeat delay, so the chord is never taken for a repeat.
    w.ty.t_ms += 520;
    let t = w.ty.t_ms;
    let m = w.rig.mutter.as_mut().context("no mutter")?;
    m.key_ex(modifier.code, modifier.sym, 0, false, t)?;
    let down = m.key_ex(key.code, key.sym, state, false, t + 5)?;
    m.key_ex(key.code, key.sym, state, true, t + 15)?;
    m.key_ex(modifier.code, modifier.sym, 0, true, t + 25)?;
    w.ty.t_ms += 25;
    Ok((down.t_emit_ns, down.consumed))
}

const CTRL_HOME: K = K {
    code: 110,
    sym: 0xff50,
};

/// Do the stimulus; returns (emit time, consumed). `consumed` is true for non-key stimuli.
fn act(w: &mut Web, doc: &DocRef, a: Act, node: &mut Option<String>) -> Result<(u64, bool)> {
    match a {
        Act::Key(k, st) => w.press(k, st),
        Act::Repeat(k, st, n) => {
            let (t0, mut all) = (now_ns(), true);
            let mut first = 0;
            for i in 0..n {
                let (t, c) = w.press(k, st)?;
                if i == 0 {
                    first = t;
                }
                all &= c;
                std::thread::sleep(Duration::from_millis(70));
            }
            Ok((if first == 0 { t0 } else { first }, all))
        }
        Act::Chord(m, k, st) => chord(w, m, k, st),
        Act::AppKeys(codes) => {
            w.unsettled = true;
            let t = now_ns();
            w.ff.keys(codes);
            Ok((t, true))
        }
        Act::Focus(r, name) => {
            let path = find_role(w, &doc.path, r, name)?;
            let t = w.focus(&path)?;
            *node = Some(path);
            Ok((t, true))
        }
        Act::FocusNth(i) => {
            let r = w.ax.request(
                "focusables",
                &format!("\"bus\":\"{}\",\"root\":\"{}\"", w.bus, doc.path),
                Duration::from_secs(30),
            )?;
            if i == 0 {
                // What the browser exposes at every Tab stop (name, description, attributes,
                // relations), kept next to the speech for the label-source investigation.
                let dir = "/var/tmp/gorca-fl-dump";
                let engine = if super::chromium_selected() {
                    "chromium"
                } else {
                    "firefox"
                };
                let _ = std::fs::create_dir_all(dir);
                let _ = std::fs::write(
                    format!("{dir}/{engine}-{}.json", doc.name.replace(' ', "_")),
                    &r,
                );
            }
            let path = r
                .split("\"path\":\"")
                .skip(1)
                .nth(i)
                .and_then(|t| t.split('"').next())
                .map(str::to_owned)
                .with_context(|| format!("focusable #{i} not found"))?;
            let t = w.focus(&path)?;
            *node = Some(path);
            Ok((t, true))
        }
        Act::Do(r, name) => {
            let path = find_role(w, &doc.path, r, name)?;
            w.unsettled = true;
            Ok((w.action(&path)?, true))
        }
        Act::Top => {
            w.focus(&doc.path)?;
            std::thread::sleep(Duration::from_millis(350));
            let r = w.press(CTRL_HOME, super::CTRL)?;
            std::thread::sleep(Duration::from_millis(300));
            Ok(r)
        }
        Act::Click => {
            w.ff.click();
            Ok((now_ns(), true))
        }
        Act::Wait(ms) => {
            std::thread::sleep(Duration::from_millis(ms));
            Ok((now_ns(), true))
        }
        Act::PrbDown(k, st) | Act::PrbRep(k, st) | Act::PrbUp(k, st) => {
            let m = w.rig.mutter.as_mut().context("no mutter")?;
            w.ty.t_ms += 12;
            let r = match a {
                Act::PrbDown(..) => m.key_ex(k.code, k.sym, st, false, w.ty.t_ms)?,
                Act::PrbUp(..) => m.key_ex(k.code, k.sym, st, true, w.ty.t_ms)?,
                _ => m.key_repeat(k.code, k.sym, st, w.ty.t_ms)?,
            };
            Ok((r.t_emit_ns, r.consumed))
        }
        Act::Speak(len) => {
            w.rig
                .mock
                .configure(Duration::from_millis(1), Duration::from_millis(len));
            Ok((now_ns(), true))
        }
    }
}

fn run_step(w: &mut Web, doc: &DocRef, sc: &'static str, st: &Step) -> StepRes {
    let wanted = [st.say, st.extra].concat().join(" + ");
    let mut res = StepRes {
        scenario: sc,
        id: st.id,
        prio: st.prio,
        what: st.what,
        status: Status::Fail,
        lat_ms: None,
        heard: String::new(),
        wanted,
        why: st.xfail.unwrap_or_default().to_owned(),
    };
    let focus0 = w.tap.count(FOCUS);
    let caret0 = w.tap.count(super::CARET);
    let mark_before = w.mark();
    let mut node = None;
    let (mut t_emit, consumed) = match act(w, doc, st.act, &mut node) {
        Ok(v) => v,
        Err(e) => {
            res.why = format!("{e:#}");
            res.status = if st.xfail.is_some() {
                Status::XFail
            } else {
                Status::Fail
            };
            return res;
        }
    };
    if st.setup {
        // The speech a positioning step causes (a mode word, a line) must not leak into the next
        // step's window.
        if !matches!(st.act, Act::Top | Act::Wait(_) | Act::Speak(_)) {
            std::thread::sleep(Duration::from_millis(350));
        }
        res.status = Status::Pass;
        return res;
    }
    // A focus step is timed from the moment the focus event reached the bus, not from the
    // `GrabFocus` call: the browser's own work is not gorca's (as FM1).
    if let (Act::Focus(..) | Act::FocusNth(_), Some(n)) = (st.act, node.as_deref())
        && let Some(arrival) = w.tap.first_after(FOCUS, &w.bus, Some(n), t_emit)
    {
        t_emit = arrival;
    }
    let mark = mark_before;
    let early =
        st.not.is_empty() && st.max_say.is_none() && st.max_chars.is_none() && !st.say.is_empty();
    let end = Instant::now() + Duration::from_millis(st.window_ms);
    let mut quiet_since = Instant::now();
    let mut seen = 0usize;
    let texts: Vec<String> = loop {
        let sp = speaks_since(w, mark);
        if sp.len() != seen {
            seen = sp.len();
            quiet_since = Instant::now();
        }
        let texts: Vec<String> = sp.into_iter().map(|s| s.1).collect();
        let heard = norm(&texts.join(" "));
        let settled = early
            && has_all(&heard, st.say)
            && st.exact.is_none_or(|e| heard == norm(e))
            && quiet_since.elapsed() >= Duration::from_millis(120);
        if settled || Instant::now() >= end {
            break texts;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let sp = speaks_since(w, mark);
    // BM-2: a step that must not move focus must not move the page caret either (a caret in a
    // focusable node is a focus in some engines), so caret events count like focus events.
    let focus_events = w.tap.count(FOCUS).saturating_sub(focus0)
        + if st.no_focus_events {
            w.tap.count(super::CARET).saturating_sub(caret0)
        } else {
            0
        };
    res.status = judge(st, &texts, consumed, focus_events);
    let first = st
        .say
        .first()
        .and_then(|tok| {
            let want = norm(tok);
            sp.iter()
                .find(|(t, text)| *t >= t_emit && norm(text).contains(&want))
        })
        .or_else(|| sp.iter().find(|(t, _)| *t >= t_emit));
    if let Some((t, _)) = first {
        res.lat_ms = Some(ms(t.saturating_sub(t_emit)));
    }
    let mut heard = texts.join(" | ");
    if st.id.starts_with("SY.hold") {
        heard = format!(
            "[consumed {consumed} focus {} caret {}] {heard}",
            w.tap.count(FOCUS).saturating_sub(focus0),
            w.tap.count(super::CARET).saturating_sub(caret0)
        );
    }
    if !consumed && matches!(st.act, Act::Key(..) | Act::Repeat(..) | Act::Chord(..)) {
        heard.insert_str(0, "[key passed to the app] ");
    }
    if heard.len() > 240 {
        let mut cut = 240;
        while !heard.is_char_boundary(cut) {
            cut -= 1;
        }
        heard.truncate(cut);
        heard.push('~');
    }
    res.heard = heard;
    if matches!(res.status, Status::Gap) {
        st.extra_why.clone_into(&mut res.why);
    }
    res
}

/// Run one scenario. `GORCA_M8_STEPS=id,id` restricts the steps of a scripted scenario (the
/// rest are skipped, not failed), for debugging.
pub fn run(
    host: &HostEnv,
    reader: &crate::e2e::apps::ReaderSpec,
    flavor: Flavor,
    size: Size,
    sc: &Scenario,
) -> Result<(RunOut, Vec<StepRes>)> {
    let mut out = RunOut::default();
    if !gate_open(host, sc.id, &mut out)? {
        return Ok((out, Vec::new()));
    }
    let url = page_url(host, sc.page)?;
    let (mut w, doc) = Web::start(host, reader, flavor, &[url], sc.title)?;
    w.prime(&doc)?;
    let mut results = Vec::new();
    match sc.body {
        Body::Steps(list) => {
            let only: Option<Vec<String>> = std::env::var("GORCA_M8_STEPS")
                .ok()
                .map(|s| s.split(',').map(str::to_owned).collect());
            for st in steps_of(list) {
                if let Some(o) = &only
                    && !o.iter().any(|x| st.id.starts_with(x.as_str()))
                {
                    continue;
                }
                let r = run_step(&mut w, &doc, sc.id, &st);
                if st.setup {
                    continue;
                }
                if r.status == Status::Fail
                    && (r.heard.is_empty() || std::env::var_os("GORCA_M8_DUMP").is_some())
                {
                    w.dump_loss(&r.id.replace('.', "-"));
                }
                results.push(r);
            }
        }
        Body::W1Big => perf_w1_big(&mut w, size, &mut out)?,
        Body::TableBig => perf_table_big(&mut w, size, &mut out)?,
    }
    score(&results, &mut out);
    Ok((out, results))
}

/// Fold step results into the scalars and latency samples the runner gates on.
fn score(results: &[StepRes], out: &mut RunOut) {
    let count = |s: Status| results.iter().filter(|r| r.status == s).count() as f64;
    if !results.is_empty() {
        out.x("steps", results.len() as f64, "steps");
        out.x("pass", count(Status::Pass), "steps");
        out.x("gap", count(Status::Gap), "steps");
        out.x("xfail", count(Status::XFail), "steps");
        out.x("xpass", count(Status::XPass), "steps");
        out.x("fail", count(Status::Fail), "steps");
        out.x(
            "p0_not_pass",
            results
                .iter()
                .filter(|r| r.prio == P0 && matches!(r.status, Status::Fail | Status::XFail))
                .count() as f64,
            "steps",
        );
    }
    // Only what needs a human goes into the run notes; the full table is the steps report.
    for r in results
        .iter()
        .filter(|r| matches!(r.status, Status::Fail | Status::XPass))
    {
        out.notes.push(format!(
            "{} {} P{} {}: heard {:?} {}",
            r.scenario,
            r.id,
            r.prio,
            r.status.label(),
            r.heard,
            r.why
        ));
    }
}

/// The latency series of passing steps, by gate, for one scenario's results and its step table.
pub fn latency_series(sc: &Scenario, results: &[StepRes], out: &mut RunOut) {
    let Body::Steps(list) = sc.body else { return };
    let table = steps_of(list);
    for gate in [Gate::Key, Gate::Mode, Gate::Live] {
        let v: Vec<f64> = results
            .iter()
            .filter(|r| matches!(r.status, Status::Pass | Status::Gap | Status::XPass))
            .filter(|r| table.iter().any(|s| s.id == r.id && s.gate == gate))
            .filter_map(|r| r.lat_ms)
            .collect();
        if !v.is_empty() {
            out.s(
                match gate {
                    Gate::Key => "key_ms",
                    Gate::Mode => "mode_ms",
                    Gate::Live => "live_ms",
                    Gate::Info => continue,
                },
                v,
            );
        }
    }
}

fn perf_w1_big(w: &mut Web, size: Size, out: &mut RunOut) -> Result<()> {
    use super::{KEY_H, KEY_H_CAP, SHIFT};
    // The full mirror of 100k nodes is built in the background (phase-2 M7: under 5 s). Until it
    // is, a quick-nav key may fall back to the source (ADR-0003 8.4, up to 50 ms): measure the
    // steady state, and say how long it took to get there.
    let t0 = Instant::now();
    let mut fast = 0;
    while fast < 12 && t0.elapsed() < Duration::from_secs(40) {
        let mark = w.mark();
        let (t, _) = w.press(KEY_H, 0)?;
        std::thread::sleep(Duration::from_millis(120));
        let said = speaks_since(w, mark);
        match said.first() {
            Some((ts, _)) if ms(ts.saturating_sub(t)) < 3.0 => fast += 1,
            _ => fast = 0,
        }
    }
    out.x("w1_100k_settle_s", t0.elapsed().as_secs_f64(), "s");
    key_series(
        w,
        out,
        "w1_100k_ms",
        &[(KEY_H, 0), (KEY_H_CAP, SHIFT)],
        size.n,
        |_, t| t.contains("heading level"),
    )
}

const ALT_CTRL: u32 = 0x04 | 0x08;
const KEY_T: K = K {
    code: 28,
    sym: 0x74,
};
const DOWN: K = K {
    code: 116,
    sym: 0xff54,
};
const UP: K = K {
    code: 111,
    sym: 0xff52,
};

fn perf_table_big(w: &mut Web, size: Size, out: &mut RunOut) -> Result<()> {
    // Into the table: T, then Down onto the first header row.
    w.press(KEY_T, 0)?;
    std::thread::sleep(Duration::from_millis(150));
    w.press(DOWN, 0)?;
    std::thread::sleep(Duration::from_millis(150));
    let n = size.n.min(400);
    let mark = w.mark();
    let mut emits = Vec::with_capacity(n);
    for i in 0..n {
        // Down 200 rows, then back up: the cell under the cursor changes on every press.
        let k = if (i / 200) % 2 == 0 { DOWN } else { UP };
        let (t, _) = w.press(k, ALT_CTRL)?;
        emits.push(t);
        std::thread::sleep(Duration::from_millis(45));
    }
    std::thread::sleep(Duration::from_millis(500));
    let spoken = speaks_since(w, mark);
    let (lat, lost, late) = latencies(&emits, &spoken, |_, t| !t.is_empty());
    out.s("tb1k_ms", lat);
    out.x("lost_tb1k_ms", lost as f64, "keys");
    out.x("late_tb1k_ms", late as f64, "keys");
    Ok(())
}

/// A printable list of the scenarios and their steps, for `e2e-m8 list`.
pub fn list() -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    for sc in scenarios() {
        let n = match sc.body {
            Body::Steps(f) => steps_of(f).iter().filter(|s| !s.setup).count(),
            Body::W1Big | Body::TableBig => 0,
        };
        let _ = writeln!(
            s,
            "{:<10} {:>3} steps  {:<22} {}",
            sc.id, n, sc.page, sc.area
        );
        if let Body::Steps(f) = sc.body {
            for st in steps_of(f).iter().filter(|s| !s.setup) {
                let _ = writeln!(
                    s,
                    "    {:<22} P{} {:<5} {}{}",
                    st.id,
                    st.prio,
                    if st.xfail.is_some() { "xfail" } else { "" },
                    st.what,
                    if st.xfail.is_some() {
                        " (expected fail)"
                    } else {
                        ""
                    }
                );
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step() -> Step {
        Step::new(
            "T.x",
            P0,
            "w",
            Act::Wait(0),
            &["heading level 2", "Products"],
        )
    }

    fn said(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn tokens_match_case_and_spacing_insensitively() {
        assert_eq!(
            judge(&step(), &said(&["Heading  Level 2 products"]), true, 0),
            Status::Pass
        );
    }

    #[test]
    fn missing_token_fails_unless_marked() {
        assert_eq!(
            judge(&step(), &said(&["heading level 3 Products"]), true, 0),
            Status::Fail
        );
        assert_eq!(
            judge(&step().xf("later"), &said(&["nothing"]), true, 0),
            Status::XFail
        );
        assert_eq!(
            judge(
                &step().xf("later"),
                &said(&["heading level 2 Products"]),
                true,
                0
            ),
            Status::XPass
        );
    }

    #[test]
    fn a_character_cap_fails_a_name_read_whole() {
        let s = Step::new("T.c", P0, "w", Act::Wait(0), &["button"]).chars(100);
        assert_eq!(judge(&s, &said(&["button"]), true, 0), Status::Pass);
        let whole = "A".repeat(1 << 20) + " button";
        assert_eq!(judge(&s, &[whole], true, 0), Status::Fail);
    }

    #[test]
    fn extras_make_a_gap_not_a_failure() {
        let s = step().extra(&["3 items"], "field diff");
        assert_eq!(
            judge(&s, &said(&["heading level 2 Products"]), true, 0),
            Status::Gap
        );
        assert_eq!(
            judge(&s, &said(&["heading level 2 Products 3 items"]), true, 0),
            Status::Pass
        );
    }

    #[test]
    fn forbidden_tokens_utterance_caps_and_key_verdicts() {
        let s = step().not(&["hidden"]);
        assert_eq!(
            judge(&s, &said(&["heading level 2 Products hidden"]), true, 0),
            Status::Fail
        );
        let s = Step::new("T.y", P0, "w", Act::Wait(0), &[]).max(1);
        assert_eq!(judge(&s, &said(&["a", "b"]), true, 0), Status::Fail);
        assert_eq!(judge(&s, &said(&["a"]), true, 0), Status::Pass);
        let s = Step::new("T.z", P0, "w", Act::Wait(0), &[]).passes_through();
        assert_eq!(judge(&s, &[], true, 0), Status::Fail);
        assert_eq!(judge(&s, &[], false, 0), Status::Pass);
        let s = Step::new("T.f", P1, "w", Act::Wait(0), &[]).no_focus();
        assert_eq!(judge(&s, &[], true, 1), Status::Fail);
    }

    #[test]
    fn exact_compares_the_whole_window() {
        let s = Step::new("T.e", P0, "w", Act::Wait(0), &[]).exact("l");
        assert_eq!(judge(&s, &said(&["L"]), true, 0), Status::Pass);
        assert_eq!(judge(&s, &said(&["l", "m"]), true, 0), Status::Fail);
    }

    #[test]
    fn every_baseline_mark_names_a_real_step() {
        let mut all = std::collections::BTreeSet::new();
        for sc in scenarios() {
            if let Body::Steps(f) = sc.body {
                all.extend(f().into_iter().map(|s| s.id));
            }
        }
        for (ids, _) in baseline::MARKS {
            for id in *ids {
                assert!(
                    all.contains(id),
                    "baseline mark for a step that does not exist: {id}"
                );
            }
        }
    }

    #[test]
    fn setup_steps_are_not_judged() {
        let s = Step::setup("T.s", "pos", Act::Top);
        assert!(s.setup && s.say.is_empty());
    }

    #[test]
    fn step_ids_are_unique_and_scenarios_are_consistent() {
        let mut ids = std::collections::BTreeSet::new();
        for sc in scenarios() {
            assert!(sc.id.starts_with("M8-"), "{}", sc.id);
            if let Body::Steps(f) = sc.body {
                for st in steps_of(f) {
                    assert!(ids.insert(st.id), "duplicate step id {}", st.id);
                    assert!(st.prio <= P2);
                    if st.xfail.is_some() {
                        assert!(!st.xfail.unwrap_or_default().is_empty());
                    }
                    if !st.extra.is_empty() {
                        assert!(!st.extra_why.is_empty(), "{}: extras need a reason", st.id);
                    }
                }
            }
        }
    }
}
