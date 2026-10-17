// SPDX-License-Identifier: GPL-3.0-or-later
//! M4b end-to-end gates (docs/plan/phase-2.md M4, Desktop and Shell polish).
//!
//! | id | stimulus | metrics |
//! |---|---|---|
//! | A1 | a window is activated and, 2 ms later (GTK order), its widget takes focus: fresh dialogs (title, a text label, two buttons) and fresh plain windows (title, one button), mirror cold each time | `a1_ms` / `a1w_ms` activation emit to the SPEAK that carries the focus (gate p99 15 ms), `a1f_ms` focus emit to that SPEAK, `a1_wrong` (neither one utterance in reading order title, role, label, focus nor the focus followed by that complete line within 400 ms; gate 0), `a1_split` (the read missed the hold: the focus went first and the complete line, in reading order, followed in a later utterance that replaces it; informational), `a1_full_ms` (activation to the complete line, led or split; informational), `lost` |
//! | N1 | a focus speech of 400 ms is in progress; an announcement (polite, assertive) or a Shell banner arrives; in half the trials a newer focus follows at once | `n1_interrupted` (the notification cut the focus speech or spoke before it ended; gate 0), `n1_focus_ms` (the newer focus is not held back by the queued notification; gate p99 15 ms), `n1_text_wrong`, `lost` (a notification that never spoke; gate 0) |
//!
//! Skipped unless `--reader-env GORCA_M4B=1`; `GORCA_M4B_STRICT=1` fails instead of skipping.
//! The application is `fakeapp.py --scenario review` driven by `build`, `activate`, `popup` and
//! `announce` control commands; N1 runs it as `--own-shell-name` so the daemon verifies it as the
//! Shell (banners are only watched there).

use std::time::Duration;

use anyhow::{Result, bail};
use gorca_bench_b2::clock::Rng;

use super::apps::{FakeApp, json_num};
use super::m1::{flag, m1_env};
use super::mock::{Flavor, Kind, Rec};
use super::runner::{HostEnv, Rig};
use super::scenarios::{RunOut, Size, base_opts, ms, pause, speaks};

fn gate_open(host: &HostEnv, out: &mut RunOut, id: &str) -> Result<bool> {
    if flag(host, "GORCA_M4B") {
        return Ok(true);
    }
    if flag(host, "GORCA_M4B_STRICT") {
        bail!("{id}: GORCA_M4B_STRICT=1 requires the M4b gates to run");
    }
    out.notes.push(format!(
        "{id} SKIPPED: set GORCA_M4B=1 to run the M4b gates"
    ));
    out.x("skipped", 1.0, "flag");
    Ok(false)
}

/// A cancel of live speech counts as the notification's only when it comes this close before
/// its SPEAK (a replace is a cancel plus a speak; the focus change that cancelled the focus
/// speech earlier is not the notification's doing).
const CUT_WINDOW_NS: u64 = 50_000_000;
/// Gap between A1 probes: the reader rate-limits window activations per process (4 per second),
/// so a probe faster than that would measure the limiter, not the path.
const A1_PACE_MS: u64 = 260;

fn desktop_rig(
    host: &HostEnv,
    reader: &super::apps::ReaderSpec,
    flavor: Flavor,
    utterance: Duration,
) -> Result<Rig> {
    let mut o = base_opts(reader, flavor);
    o.apps = Vec::new();
    let rig = Rig::start(&m1_env(host), &o)?;
    rig.mock.configure(Duration::from_millis(1), utterance);
    Ok(rig)
}

/// Focus the two prime buttons alternately until the reader speaks: events sent before the
/// drain's match rules are installed are lost by design, so a fixed sleep would be a race.
fn prime(rig: &Rig, app: &mut FakeApp, a: usize, b: usize) -> Result<()> {
    let mut flip = false;
    for _ in 0..80 {
        flip = !flip;
        let mark = rig.mock.mark();
        app.focus(if flip { a } else { b })?;
        if rig
            .mock
            .wait_for(mark, Duration::from_millis(400), |r| {
                matches!(r.kind, Kind::Speak { .. })
            })
            .is_some()
        {
            std::thread::sleep(Duration::from_millis(400));
            return Ok(());
        }
    }
    bail!("the reader never spoke the prime focus")
}

/// One top-level window of the A1 tree.
struct Win {
    node: usize,
    focus: usize,
    /// What the first utterance must be, exactly (`title, dialog, label, button, button`).
    full: String,
    button: String,
}

/// The tree: a prime window (ids 1..3), then `n` dialogs of 4 nodes and `n` plain windows of 2.
fn a1_tree(n: usize) -> (String, Vec<Win>, Vec<Win>) {
    let mut tops = vec![
        "{\"name\":\"Main\",\"role\":\"frame\",\"children\":[{\"name\":\"Prime A\",\"role\":\"push button\",\"focusable\":true},{\"name\":\"Prime B\",\"role\":\"push button\",\"focusable\":true}]}"
            .to_owned(),
    ];
    let mut id = 4;
    let mut dialogs = Vec::new();
    for k in 0..n {
        let (title, label, button) = (
            format!("Delete item {k}?"),
            format!("Item {k} will be lost."),
            format!("Confirm {k}"),
        );
        tops.push(format!(
            "{{\"name\":\"{title}\",\"role\":\"dialog\",\"children\":[{{\"name\":\"{label}\",\"role\":\"label\"}},{{\"name\":\"{button}\",\"role\":\"push button\",\"focusable\":true}},{{\"name\":\"Cancel {k}\",\"role\":\"push button\",\"focusable\":true}}]}}"
        ));
        dialogs.push(Win {
            node: id,
            focus: id + 2,
            full: format!("{title}, dialog, {label}, {button}, button"),
            button,
        });
        id += 4;
    }
    let mut plain = Vec::new();
    for k in 0..n {
        let (title, button) = (format!("Folder {k}"), format!("Open {k}"));
        tops.push(format!(
            "{{\"name\":\"{title}\",\"role\":\"frame\",\"children\":[{{\"name\":\"{button}\",\"role\":\"push button\",\"focusable\":true}}]}}"
        ));
        plain.push(Win {
            node: id,
            focus: id + 1,
            full: format!("{title}, {button}, button"),
            button,
        });
        id += 2;
    }
    (
        format!("\"coord\":\"window\",\"tree\":[{}]", tops.join(",")),
        dialogs,
        plain,
    )
}

/// What one A1 probe found.
struct Probe {
    /// Activation emit to the SPEAK that carries the focus.
    act_ms: f64,
    /// Focus emit to that SPEAK.
    focus_ms: f64,
    /// `full` was the first utterance.
    exact: bool,
    /// The text of the first utterance (for the first mismatch note).
    got: String,
    /// The focus came first and the complete line followed in a later utterance.
    split: bool,
    /// Activation emit to the first utterance that is the complete line (`None`: never came).
    full_ms: Option<f64>,
}

fn a1_probe(
    rig: &Rig,
    app: &mut FakeApp,
    w: &Win,
    dialog: bool,
    rng: &mut Rng,
) -> Result<Option<Probe>> {
    let mark = rig.mock.mark();
    let r = app.proc.request(
        "activate",
        &format!("\"node\":{},\"focus\":{},\"gap_ms\":2", w.node, w.focus),
        Duration::from_secs(5),
    )?;
    let t_act = json_num(&r, "t_act_ns").unwrap_or(0.0) as u64;
    let t_focus = json_num(&r, "t_focus_ns").unwrap_or(0.0) as u64;
    let Some(first) = rig.mock.wait_for(
        mark,
        Duration::from_secs(1),
        |rec| matches!(&rec.kind, Kind::Speak { text, .. } if text.contains(&w.button)),
    ) else {
        return Ok(None);
    };
    let Kind::Speak { text, .. } = &first.kind else {
        return Ok(None);
    };
    let exact = *text == w.full;
    let mut split = false;
    let mut full_ns = exact.then_some(first.t_ns);
    if !exact {
        // On a loaded host the window read can miss its 10 ms bound: the focus (with whatever was
        // known) goes first and the whole line follows once as a queued utterance, in reading
        // order (title, role, text, focus). That is the designed fallback, counted as a split;
        // wrong is when the complete line never comes (the text after the button is wrong).
        let later = rig
            .mock
            .wait_for(mark, Duration::from_millis(400), |rec| {
                matches!(&rec.kind, Kind::Speak { text, .. } if *text == w.full && rec.t_ns > first.t_ns)
            });
        split = later.is_some();
        full_ns = later.map(|rec| rec.t_ns);
    }
    let _ = dialog;
    pause(rng, A1_PACE_MS, A1_PACE_MS + 40);
    Ok(Some(Probe {
        act_ms: ms(first.t_ns.saturating_sub(t_act)),
        focus_ms: ms(first.t_ns.saturating_sub(t_focus)),
        exact,
        got: text.clone(),
        split,
        full_ms: full_ns.map(|ns| ms(ns.saturating_sub(t_act))),
    }))
}

/// A1. Gate: `a1_ms` p99 <= 15 ms, `a1_wrong` = 0, `lost` = 0.
pub fn a1(
    host: &HostEnv,
    reader: &super::apps::ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, out, "A1")? {
        return Ok(());
    }
    const WARM: usize = 6;
    let n = size.n.clamp(20, 150);
    let rig = desktop_rig(host, reader, flavor, Duration::from_millis(1))?;
    let mut app = FakeApp::spawn(&rig.env.harness_dir, "review", "dialogs", 0, &[])?;
    let (tree, dialogs, plain) = a1_tree(n + WARM);
    app.proc.request("build", &tree, Duration::from_secs(10))?;
    prime(&rig, &mut app, 2, 3)?;
    let mut rng = Rng::new(seed);
    let (mut d_ms, mut f_ms, mut w_ms) = (Vec::new(), Vec::new(), Vec::new());
    let mut full_ms = Vec::new();
    let (mut wrong, mut split, mut lost) = (0u64, 0u64, 0u64);
    for (windows, is_dialog) in [(&dialogs, true), (&plain, false)] {
        for (i, w) in windows.iter().enumerate() {
            let p = a1_probe(&rig, &mut app, w, is_dialog, &mut rng)?;
            if i < WARM {
                continue;
            }
            match p {
                None => lost += 1,
                Some(p) => {
                    if is_dialog {
                        d_ms.push(p.act_ms);
                        f_ms.push(p.focus_ms);
                    } else {
                        w_ms.push(p.act_ms);
                    }
                    if p.split {
                        split += 1;
                    }
                    full_ms.extend(p.full_ms);
                    if !p.exact && !p.split {
                        wrong += 1;
                        if wrong == 1 {
                            out.notes.push(format!(
                                "A1 first mismatch: wanted `{}`, got `{}`",
                                w.full, p.got
                            ));
                        }
                    }
                }
            }
        }
    }
    out.notes.push(format!(
        "A1: {n} fresh dialogs and {n} fresh plain windows after {WARM} warm-up ones; the focus follows the activation by 2 ms; a1_ms is the activation emit to the first SPEAK carrying the focus button (the title, role and dialog text lead it in one utterance)"
    ));
    out.s("a1_ms", d_ms);
    out.s("a1f_ms", f_ms);
    out.s("a1w_ms", w_ms);
    // Informational (N-5): activation to the complete line, whether it led or followed a split.
    out.s("a1_full_ms", full_ms);
    out.x("a1_wrong", wrong as f64, "events");
    out.x("a1_split", split as f64, "events");
    out.x("lost", lost as f64, "events");
    drop(app);
    Ok(())
}

/// What kind of notification a trial sends, and whether a newer focus follows it at once.
#[derive(Copy, Clone, Debug)]
enum Note {
    Polite,
    Assertive,
    Banner,
}

fn send_note(app: &mut FakeApp, kind: Note, i: usize) -> Result<String> {
    Ok(match kind {
        Note::Polite | Note::Assertive => {
            let (text, pol) = if matches!(kind, Note::Polite) {
                (format!("Download {i} complete"), 1)
            } else {
                (format!("Battery low {i}"), 2)
            };
            app.proc.request(
                "announce",
                &format!("\"node\":1,\"text\":\"{text}\",\"politeness\":{pol}"),
                Duration::from_secs(5),
            )?;
            text
        }
        Note::Banner => {
            app.proc.request(
                "popup",
                &format!(
                    "\"parent\":1,\"tree\":{{\"name\":\"Alice {i}\",\"role\":\"notification\",\"children\":[{{\"name\":\"Alice {i}\",\"role\":\"label\"}},{{\"name\":\"Are you coming tonight {i}?\",\"role\":\"label\"}},{{\"name\":\"Reply\",\"role\":\"push button\",\"focusable\":true}}]}}"
                ),
                Duration::from_secs(5),
            )?;
            format!("alert, Alice {i}, Are you coming tonight {i}?")
        }
    })
}

fn n1_wait_end(rig: &Rig, mark: usize, after_t: u64) -> Option<Rec> {
    rig.mock.wait_for(mark, Duration::from_secs(3), |r| {
        matches!(r.kind, Kind::End) && r.t_ns > after_t
    })
}

/// N1. Gates: `n1_interrupted` = 0, `lost` = 0, `n1_focus_ms` p99 <= 15 ms.
pub fn n1(
    host: &HostEnv,
    reader: &super::apps::ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, out, "N1")? {
        return Ok(());
    }
    let trials = (size.trials / 6).clamp(12, 60);
    let rig = desktop_rig(host, reader, flavor, Duration::from_millis(400))?;
    let mut app = FakeApp::spawn(
        &rig.env.harness_dir,
        "review",
        "gnome-shell",
        0,
        &["--own-shell-name"],
    )?;
    // The Shell's top bar: one window with focusable items. Node 1 is the window.
    let items: Vec<String> = (0..14)
        .map(|i| format!("{{\"name\":\"Item {i}\",\"role\":\"push button\",\"focusable\":true}}"))
        .collect();
    app.proc.request(
        "build",
        &format!(
            "\"coord\":\"window\",\"tree\":{{\"name\":\"Top Bar\",\"role\":\"frame\",\"children\":[{}]}}",
            items.join(",")
        ),
        Duration::from_secs(10),
    )?;
    // Items are nodes 2..=15.
    prime(&rig, &mut app, 2, 3)?;
    std::thread::sleep(Duration::from_millis(500));
    let mut rng = Rng::new(seed);
    let (mut interrupted, mut lost, mut text_wrong) = (0u64, 0u64, 0u64);
    let mut focus_ms = Vec::new();
    let mut spoken_notes = 0u64;
    for t in 0..trials {
        let (kind, follow) = match t % 4 {
            0 => (Note::Polite, false),
            1 => (Note::Assertive, true),
            2 => (Note::Banner, false),
            _ => (Note::Banner, true),
        };
        let item = 4 + (t * 2) % 12;
        let mark = rig.mock.mark();
        app.focus(item)?;
        let f_name = format!("Item {}, button", item - 2);
        let Some(f) = rig.mock.wait_for(
            mark,
            Duration::from_secs(2),
            |r| matches!(&r.kind, Kind::Speak { text, .. } if *text == f_name),
        ) else {
            lost += 1;
            out.notes.push(format!(
                "N1 trial {t} {kind:?} follow={follow}: {} never spoke",
                "focus"
            ));
            continue;
        };
        pause(&mut rng, 60, 120);
        let want = send_note(&mut app, kind, t)?;
        let mut g_speak = None;
        if follow {
            pause(&mut rng, 30, 60);
            let g_item = item + 1;
            let t_emit = app.focus(g_item)?;
            let g_name = format!("Item {}, button", g_item - 2);
            if let Some(g) = rig.mock.wait_for(mark, Duration::from_secs(2), |r| {
                matches!(&r.kind, Kind::Speak { text, .. } if *text == g_name) && r.t_ns >= t_emit
            }) {
                focus_ms.push(ms(g.t_ns.saturating_sub(t_emit)));
                g_speak = Some(g);
            } else {
                lost += 1;
                out.notes.push(format!(
                    "N1 trial {t} {kind:?} follow={follow}: {} never spoke",
                    "newer-focus"
                ));
            }
        }
        // The notification speaks (it is never dropped: every kind here survives a focus change
        // or has no focus change at all).
        let Some(n) = rig.mock.wait_for(mark, Duration::from_secs(4), |r| {
            matches!(&r.kind, Kind::Speak { text, .. } if text.contains(want.rsplit(',').next().unwrap_or(&want).trim()))
        }) else {
            lost += 1;
 out.notes.push(format!("N1 trial {t} {kind:?} follow={follow}: {} never spoke", "notification"));
            continue;
        };
        spoken_notes += 1;
        let Kind::Speak { text, .. } = &n.kind else {
            continue;
        };
        if *text != want {
            text_wrong += 1;
        }
        // It must have waited for the speech it found in progress: with no newer focus that is
        // the focus speech of this trial; with one, the newer focus utterance.
        let held_back_by = g_speak.as_ref().unwrap_or(&f);
        match n1_wait_end(&rig, mark, held_back_by.t_ns) {
            Some(end) if n.t_ns >= end.t_ns => {}
            other => {
                interrupted += 1;
                out.notes.push(format!(
                    "N1 trial {t} {kind:?} follow={follow}: spoke {:.1} ms vs end {:?}",
                    ms(n.t_ns.saturating_sub(held_back_by.t_ns)),
                    other.map(|e| ms(e.t_ns.saturating_sub(held_back_by.t_ns)))
                ));
            }
        }
        // Nothing was cut off by it: no cancel of live speech between the focus speech and the
        // notification, except the newer focus's own (it comes before the notification).
        let recs = rig.mock.since(mark);
        let notif_t = n.t_ns;
        let cut = recs.iter().any(|r| {
            matches!(
                r.kind,
                Kind::Cancel {
                    had_speech: true,
                    ..
                }
            ) && r.t_ns + CUT_WINDOW_NS >= notif_t
                && r.t_ns <= notif_t
        });
        if cut {
            interrupted += 1;
            out.notes.push(format!("N1 trial {t} {kind:?} follow={follow}: a live speech was cancelled right before the notification"));
        }
        // Let its speech finish before the next trial.
        let _ = rig.mock.wait_for(mark, Duration::from_secs(3), |r| {
            matches!(r.kind, Kind::End) && r.t_ns > notif_t
        });
        std::thread::sleep(Duration::from_millis(80));
    }
    let _ = speaks;
    out.notes.push(format!(
        "N1: {trials} trials of 400 ms focus speech, cycling polite announcement, assertive announcement + newer focus, Shell banner, banner + newer focus; {spoken_notes} notifications spoken"
    ));
    out.s("n1_focus_ms", focus_ms);
    out.x("n1_interrupted", interrupted as f64, "events");
    out.x("n1_text_wrong", text_wrong as f64, "events");
    out.x("lost", lost as f64, "events");
    drop(app);
    let _ = rig.mock.mark();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_a1_tree_numbers_nodes_in_pre_order() {
        let (_, d, p) = a1_tree(3);
        // Root 0, Main 1 with two buttons (2, 3), then windows from 4.
        assert_eq!(d[0].node, 4);
        assert_eq!(
            d[0].focus, 6,
            "the confirm button is the dialog's second child"
        );
        assert_eq!(d[1].node, 8);
        assert_eq!(p[0].node, 16, "three dialogs of four nodes come first");
        assert_eq!(p[0].focus, 17);
        assert_eq!(
            d[0].full,
            "Delete item 0?, dialog, Item 0 will be lost., Confirm 0, button"
        );
        assert_eq!(p[2].full, "Folder 2, Open 2, button");
    }
}
