//! M4a end-to-end gate R1 (docs/adr/0006 18): a review step on a valid line model to speech.
//!
//! The stimulus is the desktop layout's unmodified numpad review keys through `fake_mutter.py`,
//! so the run also proves the K-8a grab compilation: `KP_Prior` / `KP_Home` reach the reader only
//! if it registered the exact keysym and state Mutter will see (platform check V-1). The target
//! application is the `review` scenario of `fakeapp.py` with a window of 24 push buttons that
//! have window-relative extents (GTK4 flavour).
//!
//! | id | stimulus | metrics |
//! |---|---|---|
//! | R1 | after a cold build, 1200 alternating next-line / previous-line presses | `r1_ms` key-down emit to SPEAK, `lost`, `not_consumed`, `r1_ipc_calls` (application calls during the series, informational: RR-1 revalidates after 2 s), `r2_cold_ms` (first press, cold build, informational) |
//!
//! R1s repeats it under the L3-50k storm. Skipped unless `--reader-env GORCA_M4=1`; `GORCA_M4_STRICT=1` fails instead of skipping.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use gorca_bench_b2::clock::Rng;

use super::apps::{FakeApp, json_num};
use super::m1::{K, Typist, flag, m1_rig};
use super::mock::Flavor;
use super::runner::HostEnv;
use super::scenarios::{RunOut, Size, ms, pause, speaks, start_storm};

/// XKB keycode and keysym of `KP_Prior` (keypad 9) and `KP_Home` (keypad 7), Num Lock off.
const KP_NEXT_LINE: K = K {
    code: 81,
    sym: 0xff9a,
};
const KP_PREV_LINE: K = K {
    code: 79,
    sym: 0xff95,
};
/// Node ids of the `review` scenario tree: root 0, window 1, then the buttons in pre-order.
const FIRST_BUTTON: usize = 2;
const ROWS: usize = 24;

fn gate_open(host: &HostEnv, out: &mut RunOut) -> Result<bool> {
    if flag(host, "GORCA_M4") {
        return Ok(true);
    }
    if flag(host, "GORCA_M4_STRICT") {
        bail!("R1: GORCA_M4_STRICT=1 requires the M4a gate to run");
    }
    out.notes
        .push("R1 SKIPPED: set GORCA_M4=1 to run the M4a review gates".to_owned());
    out.x("skipped", 1.0, "flag");
    Ok(false)
}

/// The tree the fake application is built with: a framed window of push buttons, one per row.
fn tree_json() -> String {
    let mut kids = Vec::with_capacity(ROWS);
    for i in 0..ROWS {
        kids.push(format!(
            "{{\"name\":\"Row {i}\",\"role\":\"push button\",\"focusable\":true,\"extents\":[10,{},120,24]}}",
            10 + 30 * i
        ));
    }
    format!(
        "\"coord\":\"window\",\"tree\":{{\"name\":\"Review Form\",\"role\":\"frame\",\"extents\":[0,0,600,{}],\"children\":[{}]}}",
        20 + 30 * ROWS,
        kids.join(",")
    )
}

/// One key: down and up, Mutter's verdict on the down. Returns its emit time and `consumed`.
fn tap(rig: &mut super::runner::Rig, ty: &mut Typist, k: K) -> Result<(u64, bool)> {
    let m = rig.mutter.as_mut().context("no mutter")?;
    ty.t_ms += 12;
    let down = m.key_ex(k.code, k.sym, 0, false, ty.t_ms)?;
    ty.t_ms += 3;
    m.key_ex(k.code, k.sym, 0, true, ty.t_ms)?;
    Ok((down.t_emit_ns, down.consumed))
}

/// What key `i` of the series says: the series alternates previous-line and next-line presses
/// between the rows "Row 0" and "Row 1".
fn expected_text(i: usize) -> &'static str {
    if i.is_multiple_of(2) {
        "Row 0 button"
    } else {
        "Row 1 button"
    }
}

/// Credit every speech to the key that caused it, by its text (F-8). A speech that arrives after
/// the next key was already pressed is a **late** sample: it counts in the latency series and in
/// `late`, never as the next key's answer. `lost` are keys whose text never arrived (before the
/// key after next).
fn attribute(emits: &[u64], spoken: &[(u64, String)]) -> (Vec<f64>, u64, u64) {
    let mut used = vec![false; spoken.len()];
    let mut lat = Vec::with_capacity(emits.len());
    let (mut lost, mut late) = (0u64, 0u64);
    for (i, t) in emits.iter().enumerate() {
        let next = emits.get(i + 1).copied().unwrap_or(u64::MAX);
        let horizon = emits.get(i + 2).copied().unwrap_or(u64::MAX);
        let want = expected_text(i);
        let hit = spoken
            .iter()
            .enumerate()
            .find(|(j, (s, text))| !used[*j] && *s >= *t && *s < horizon && text == want);
        match hit {
            Some((j, (s, _))) => {
                used[j] = true;
                lat.push(ms(s - *t));
                if *s >= next {
                    late += 1;
                }
            }
            None => lost += 1,
        }
    }
    (lat, lost, late)
}

/// R1. Gate: p99 <= 3 ms (target 1 ms), `lost` = 0, `not_consumed` = 0.
pub fn r1(
    host: &HostEnv,
    reader: &super::apps::ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    series(host, reader, flavor, size, seed, out, false)
}

/// R1s: the same series while a 50k events/s storm runs on the a11y bus (ADR-0006 18: the gate
/// must also hold under L3-50k with a p99 at most 1 ms above the idle one).
pub fn r1s(
    host: &HostEnv,
    reader: &super::apps::ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    series(host, reader, flavor, size, seed, out, true)
}

fn series(
    host: &HostEnv,
    reader: &super::apps::ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
    storm: bool,
) -> Result<()> {
    if !gate_open(host, out)? {
        return Ok(());
    }
    let metric = if storm { "r1s_ms" } else { "r1_ms" };
    let mut rig = m1_rig(host, reader, flavor)?;
    let mut app = FakeApp::spawn(&rig.env.harness_dir, "review", "review", 0, &[])?;
    app.proc
        .request("build", &tree_json(), Duration::from_secs(5))?;
    // The reader learns the application from its first event; the focus is the entry point.
    app.focus(FIRST_BUTTON)?;
    std::thread::sleep(Duration::from_millis(400));
    let mut ty = Typist::new();

    // Cold: the first review key builds the model (R2 territory, informational here).
    let mark = rig.mock.mark();
    let (t_cold, consumed) = tap(&mut rig, &mut ty, KP_NEXT_LINE)?;
    if !consumed {
        bail!("the first review key was not grabbed: the reader did not register the keypad chord");
    }
    let first = rig
        .mock
        .wait_for(mark, Duration::from_secs(3), |r| {
            matches!(r.kind, super::mock::Kind::Speak { .. })
        })
        .context("no speech for the first review key within 3 s (cold build)")?;
    out.x("r2_cold_ms", ms(first.t_ns.saturating_sub(t_cold)), "ms");
    std::thread::sleep(Duration::from_millis(300));

    // Warm series.
    const RATE: f64 = 50_000.0;
    let storm = if storm {
        let secs = size.n as f64 * 0.02 + 2.0;
        let s = start_storm(&rig.env.a11y_addr, RATE, secs)?;
        std::thread::sleep(Duration::from_millis(1000));
        Some(s)
    } else {
        None
    };
    app.proc
        .request("calls", "\"reset\":true", Duration::from_secs(2))?;
    let mut rng = Rng::new(seed);
    let n = size.n;
    let mark = rig.mock.mark();
    let mut emits = Vec::with_capacity(n);
    let mut not_consumed = 0u64;
    for i in 0..n {
        let k = if i % 2 == 0 {
            KP_PREV_LINE
        } else {
            KP_NEXT_LINE
        };
        let (t, consumed) = tap(&mut rig, &mut ty, k)?;
        emits.push(t);
        if !consumed {
            not_consumed += 1;
        }
        pause(&mut rng, 8, 16);
    }
    std::thread::sleep(Duration::from_millis(400));
    if let Some(st) = storm {
        st.stop.store(true, std::sync::atomic::Ordering::Release);
        let fr = st
            .thread
            .join()
            .map_err(|_| anyhow::anyhow!("storm thread panicked"))?;
        out.x("storm_target_eps", RATE, "ev/s");
        out.x("storm_achieved_eps", fr.achieved_rate(), "ev/s");
    }
    let recs = rig.mock.since(mark);
    let spoken: Vec<(u64, String)> = speaks(&recs)
        .map(|(r, t)| (r.t_ns, t.replace(", ", " ")))
        .collect();
    let (lat, lost, late) = attribute(&emits, &spoken);
    out.x("r1_late_keys", late as f64, "events");
    let calls = app
        .proc
        .request("calls", "\"reset\":false", Duration::from_secs(2))?;
    out.x(
        "r1_ipc_calls",
        json_num(&calls, "total").unwrap_or(0.0),
        "calls",
    );
    out.notes.push(format!(
        "{metric}: first SPEAK between a numpad review key and the next one ({} spoken for {n} presses); the application calls during the series are RR-1 revalidations (every 2 s), never on the key path",
        spoken.len()
    ));
    out.s(metric, lat);
    out.x("lost", lost as f64, "events");
    out.x("not_consumed", not_consumed as f64, "events");
    drop(app);
    Ok(())
}

/// The form of the RS scenario (window-relative extents, GTK4 flavour): a label and an editable
/// entry in one row, two buttons, a wrapped two-line label, and a status bar with a label.
fn form_json() -> String {
    let n = |name: &str, role: &str, ext: [i32; 4], extra: &str| {
        format!(
            "{{\"name\":\"{name}\",\"role\":\"{role}\",\"extents\":[{},{},{},{}]{extra}}}",
            ext[0], ext[1], ext[2], ext[3]
        )
    };
    let panel_kids = [
        n("Name", "label", [10, 10, 60, 20], ",\"text\":\"Name\""),
        n(
            "Name",
            "entry",
            [80, 10, 200, 20],
            ",\"text\":\"Alice\",\"editable\":true,\"focusable\":true",
        ),
        n(
            "Apply",
            "push button",
            [10, 50, 80, 24],
            ",\"focusable\":true",
        ),
        n(
            "Cancel",
            "push button",
            [100, 51, 80, 24],
            ",\"focusable\":true",
        ),
        n(
            "",
            "label",
            [10, 100, 300, 40],
            ",\"text\":\"first line\\nsecond line\"",
        ),
    ]
    .join(",");
    let bar = format!(
        "{{\"name\":\"\",\"role\":\"status bar\",\"extents\":[0,370,600,30],\"children\":[{}]}}",
        n("Ready", "label", [4, 375, 60, 20], ",\"text\":\"Ready\"")
    );
    format!(
        "\"coord\":\"window\",\"tree\":{{\"name\":\"Settings\",\"role\":\"frame\",\"extents\":[0,0,600,400],\"children\":[{{\"name\":\"\",\"role\":\"panel\",\"extents\":[0,0,600,300],\"children\":[{panel_kids}]}},{bar}]}}"
    )
}

/// A gorca chord: the gorca modifier (Insert), then the key.
fn gorca_chord(rig: &mut super::runner::Rig, ty: &mut Typist, key: K) -> Result<()> {
    const INSERT: K = K {
        code: 118,
        sym: 0xff63,
    };
    let m = rig.mutter.as_mut().context("no mutter")?;
    ty.t_ms += 700;
    m.key_ex(INSERT.code, INSERT.sym, 0, false, ty.t_ms)?;
    m.key_ex(key.code, key.sym, 0, false, ty.t_ms + 5)?;
    m.key_ex(key.code, key.sym, 0, true, ty.t_ms + 15)?;
    m.key_ex(INSERT.code, INSERT.sym, 0, true, ty.t_ms + 25)?;
    ty.t_ms += 25;
    Ok(())
}

/// What the reader says to the next key, within 3 s.
fn said_after(rig: &super::runner::Rig, mark: usize) -> Result<String> {
    let rec = rig
        .mock
        .wait_for(mark, Duration::from_secs(3), |r| {
            matches!(r.kind, super::mock::Kind::Speak { .. })
        })
        .context("no speech within 3 s")?;
    match rec.kind {
        super::mock::Kind::Speak { text, .. } => Ok(text),
        _ => bail!("not a speech record"),
    }
}

/// RS (ADR-0006 18): correctness of review through the real connector and a real application
/// process. Lines read in visual order with no zone lost or doubled, the label elided because the
/// entry carries its name, the wrapped label as two lines, object navigation over the real tree,
/// and the status bar found by `Collection`. `rs_failures` must be 0.
pub fn rs(
    host: &HostEnv,
    reader: &super::apps::ReaderSpec,
    flavor: Flavor,
    _size: Size,
    _seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, out)? {
        return Ok(());
    }
    const KP_BEGIN: K = K {
        code: 84,
        sym: 0xff9d,
    };
    const KP_LEFT: K = K {
        code: 83,
        sym: 0xff96,
    };
    const KP_UP: K = K {
        code: 80,
        sym: 0xff97,
    };
    const END: K = K {
        code: 115,
        sym: 0xff57,
    };
    let mut rig = m1_rig(host, reader, flavor)?;
    let mut app = FakeApp::spawn(&rig.env.harness_dir, "review", "review", 0, &[])?;
    app.proc
        .request("build", &form_json(), Duration::from_secs(5))?;
    // Pre-order ids: root 0, window 1, panel 2, label 3, entry 4, Apply 5, Cancel 6, wrapped 7,
    // status bar 8, its label 9.
    app.focus(4)?;
    std::thread::sleep(Duration::from_millis(500));
    let mut ty = Typist::new();
    let mut failures = 0u64;
    let mut transcript: Vec<String> = Vec::new();
    let mut expect = |rig: &mut super::runner::Rig,
                      ty: &mut Typist,
                      what: &str,
                      want: &[&str],
                      press: &dyn Fn(&mut super::runner::Rig, &mut Typist) -> Result<()>|
     -> Result<()> {
        let mark = rig.mock.mark();
        press(rig, ty)?;
        let got = said_after(rig, mark)?.replace(", ", " ");
        transcript.push(format!("{what}: {got:?}"));
        if !want.iter().any(|w| got.contains(w)) {
            failures += 1;
            transcript.push(format!("  MISMATCH, wanted one of {want:?}"));
        }
        std::thread::sleep(Duration::from_millis(60));
        Ok(())
    };
    let key =
        |k: K| move |rig: &mut super::runner::Rig, ty: &mut Typist| tap(rig, ty, k).map(|_| ());
    let chord = |k: K| move |rig: &mut super::runner::Rig, ty: &mut Typist| gorca_chord(rig, ty, k);

    // Flat review, by line: the first key builds the model (cold), then every step is warm.
    expect(
        &mut rig,
        &mut ty,
        "next line",
        &["Apply button Cancel button"],
        &key(KP_NEXT_LINE),
    )?;
    expect(
        &mut rig,
        &mut ty,
        "next line",
        &["first line"],
        &key(KP_NEXT_LINE),
    )?;
    expect(
        &mut rig,
        &mut ty,
        "next line",
        &["second line"],
        &key(KP_NEXT_LINE),
    )?;
    expect(
        &mut rig,
        &mut ty,
        "next line",
        &["Ready"],
        &key(KP_NEXT_LINE),
    )?;
    expect(
        &mut rig,
        &mut ty,
        "next line at the end",
        &["Bottom"],
        &key(KP_NEXT_LINE),
    )?;
    expect(
        &mut rig,
        &mut ty,
        "previous line",
        &["second line"],
        &key(KP_PREV_LINE),
    )?;
    // Object navigation over the real tree: next object from the review position.
    expect(
        &mut rig,
        &mut ty,
        "object.current",
        &["label"],
        &chord(KP_BEGIN),
    )?;
    expect(
        &mut rig,
        &mut ty,
        "object.prev",
        &["Cancel button"],
        &chord(KP_LEFT),
    )?;
    expect(
        &mut rig,
        &mut ty,
        "object.parent",
        &["Settings window"],
        &chord(KP_UP),
    )?;
    expect(
        &mut rig,
        &mut ty,
        "info.status_bar",
        &["Ready"],
        &chord(END),
    )?;
    out.x("rs_failures", failures as f64, "events");
    out.notes
        .push(format!("RS transcript: {}", transcript.join(" | ")));
    drop(app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tree_has_one_button_per_row_and_balanced_json() {
        let t = format!("{{{}}}", tree_json());
        assert_eq!(t.matches("push button").count(), ROWS);
        assert_eq!(t.matches('{').count(), t.matches('}').count());
        assert_eq!(t.matches('[').count(), t.matches(']').count());
        assert!(t.contains("\"coord\":\"window\""));
    }

    #[test]
    fn a_late_speech_is_credited_to_its_own_key_and_reported_as_late() {
        // The incident of review item 4: key 0's answer arrives after key 1 was pressed.
        let emits = [0, 13_000_000, 27_000_000, 40_000_000];
        let spoken = vec![
            (19_500_000, "Row 0 button".to_owned()),
            (19_600_000, "Row 1 button".to_owned()),
            (28_000_000, "Row 0 button".to_owned()),
            (41_000_000, "Row 1 button".to_owned()),
        ];
        let (lat, lost, late) = attribute(&emits, &spoken);
        assert_eq!((lost, late), (0, 1), "{lat:?}");
        assert!(
            (lat[0] - 19.5).abs() < 1e-9,
            "the worst sample is kept: {lat:?}"
        );
        // A speech that never comes is lost, and the next key does not inherit its neighbour's.
        let spoken = vec![(28_000_000, "Row 0 button".to_owned())];
        let (lat, lost, _) = attribute(&emits, &spoken);
        assert_eq!((lat.len(), lost), (1, 3));
    }

    #[test]
    fn r1_skips_without_the_flag_and_fails_when_strict() {
        let mut host = HostEnv {
            harness_dir: std::path::PathBuf::new(),
            a11y_addr: String::new(),
            session_addr: String::new(),
            sock_dir: std::path::PathBuf::new(),
            reader_env: Vec::new(),
        };
        let mut out = RunOut::default();
        assert!(!gate_open(&host, &mut out).unwrap());
        assert!(
            out.scalars
                .iter()
                .any(|(k, v, _)| k == "skipped" && (*v - 1.0).abs() < f64::EPSILON)
        );
        host.reader_env.push(("GORCA_M4_STRICT".into(), "1".into()));
        assert!(gate_open(&host, &mut RunOut::default()).is_err());
        host.reader_env.push(("GORCA_M4".into(), "1".into()));
        assert!(gate_open(&host, &mut RunOut::default()).unwrap());
    }
}
