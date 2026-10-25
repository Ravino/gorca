#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// SPDX-License-Identifier: GPL-3.0-or-later
//! Golden test of the mirror's structural node flags and live-region properties, the replay half
//! of the `docflags` cut-over gate (M22b4, kept after the legacy derivation was deleted). Every
//! node of every recorded web page (Firefox, Chromium, Electron, Telegram) goes through the
//! source's normaliser, the fusion stage and `flags_of`; every document node through
//! `Props::from_sem`. The records must equal `tests/fixtures/sem/golden/docflags.tsv`. The gate
//! compared 10 314 records per language against the legacy derivation: 0 unclassified, 5 under
//! dated deviation rows (`alertdialog-not-live`, `status-atomic`, `alert-live-flag`).
//!
//! `GORCA_BLESS=1 cargo test -p gorca-core --lib flags_golden` rewrites the file (review the diff:
//! every changed line is a change of what the browse mode groups and announces).
//!
//! Record format, tab separated and sorted: `corpus  node  case  value`; `case` is `flags` (node
//! flags and heading level) or `live` (region properties; only nodes that are region roots).

use std::fmt::Write as _;
use std::path::PathBuf;

use gorca_doc_source_atspi::WebQuirks;
use gorca_model::WebEngine;

use super::agree_tests::{build, pages, recorded_doc};
use super::flags_sem::flags_of;
use super::fuse::SemLookup;
use super::live::props_from_sem;

struct Family {
    dir: &'static str,
    engine: WebEngine,
    app: &'static str,
    quirks: WebQuirks,
}

const FAMILIES: &[Family] = &[
    Family {
        dir: "firefox",
        engine: WebEngine::Gecko,
        app: "Firefox",
        quirks: WebQuirks::GECKO,
    },
    Family {
        dir: "chromium",
        engine: WebEngine::Blink,
        app: "Chromium",
        quirks: WebQuirks::BLINK,
    },
    Family {
        dir: "electron",
        engine: WebEngine::Blink,
        app: "Signal",
        quirks: WebQuirks::BLINK,
    },
    Family {
        dir: "telegram",
        engine: WebEngine::Blink,
        app: "Telegram",
        quirks: WebQuirks::BLINK,
    },
];

fn emit(seed: u64) -> String {
    let mut lines: Vec<(String, u64, &'static str, String)> = Vec::new();
    for f in FAMILIES {
        for page in pages(f.dir) {
            let corpus = format!("{}/{page}", f.dir);
            let Ok(built) = build(f.engine, f.app, f.quirks, &page, f.dir, false, seed) else {
                continue;
            };
            for n in &built.nodes {
                let Some(e) = built.stage.sem(n.id) else {
                    continue;
                };
                let (fl, level) = flags_of(n, Some(&e), false);
                lines.push((
                    corpus.clone(),
                    n.id.0,
                    "flags",
                    format!("{fl:?} level {level}"),
                ));
            }
            let Some(doc) = recorded_doc(f.engine, f.app, f.quirks, &page, f.dir, seed) else {
                continue;
            };
            for id in doc.preorder() {
                let Some(v) = doc.node(id) else { continue };
                if let Some(p) = props_from_sem(&v) {
                    let src = v.data.and_then(|m| m.src).map_or(0, |s| s.0);
                    lines.push((corpus.clone(), src, "live", format!("{p:?}")));
                }
            }
        }
    }
    lines.sort();
    let mut text = String::new();
    for (c, n, case, v) in lines {
        let _ = writeln!(text, "{c}\t{n}\t{case}\t{v}");
    }
    text
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sem/golden/docflags.tsv")
}

#[test]
fn the_flags_and_live_properties_of_every_recorded_node_are_golden() {
    let got = emit(0);
    let path = golden_path();
    if std::env::var("GORCA_BLESS").is_ok_and(|v| v == "1") {
        std::fs::write(&path, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    if want != got {
        let (w, g): (Vec<&str>, Vec<&str>) = (want.lines().collect(), got.lines().collect());
        let first = w
            .iter()
            .zip(&g)
            .position(|(a, b)| a != b)
            .unwrap_or(w.len().min(g.len()));
        panic!(
            "docflags: the golden records differ ({} recorded, {} now); first difference at line {}:\n recorded: {:?}\n now:      {:?}\n(GORCA_BLESS=1 rewrites them)",
            w.len(),
            g.len(),
            first + 1,
            w.get(first),
            g.get(first)
        );
    }
}

#[test]
fn the_replay_is_order_free_and_covers_the_corpus() {
    let a = emit(0);
    assert_eq!(a, emit(0x5eed), "shuffled arrival changed the records");
    assert!(a.lines().filter(|l| l.contains("\tflags\t")).count() > 4000);
    assert!(a.lines().filter(|l| l.contains("\tlive\t")).count() > 50);
}
