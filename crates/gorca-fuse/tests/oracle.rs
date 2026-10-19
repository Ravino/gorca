// SPDX-License-Identifier: GPL-3.0-or-later
//! Differential test against the A3 reference corpus (M22a-B1a): every case, under every
//! delivery order (canonical, reversed, shuffled with duplicates and batch cuts), must give the
//! oracle's values, `ProvCode`s, hedge, taint and `known`/`stale`/`fallback` masks.
//!
//! `fuse-x32.json` is the target layout (full bit encodings are compared); `fuse-r2-16.json` is
//! the older 16-group layout with the same semantics (per-group maps and the low words of the
//! encodings are compared). Explain trails and the non-primary status computation are B1b.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{cands_from_expected, compare, evaluate, fixture, ingest, load_cases};

fn run(fixture_name: &str, min_cases: usize) {
    let doc = fixture(fixture_name);
    let cases = load_cases(&doc);
    assert!(cases.len() >= min_cases, "corpus shrank: {}", cases.len());
    let mut all = Vec::new();
    let mut runs = 0usize;
    for case in &cases {
        let cands = cands_from_expected(case);
        for (name, batches) in &case.deliveries {
            let order: Vec<usize> = batches.iter().flatten().copied().collect();
            let world = ingest(case, &order);
            let finals = evaluate(case, &world, &cands);
            for d in compare(case, &finals) {
                all.push(format!("[{name}] {d}"));
            }
            runs += 1;
        }
    }
    assert!(runs >= cases.len() * 4, "every case has four deliveries");
    assert!(
        all.is_empty(),
        "{} mismatches over {runs} runs; first 25:\n{}",
        all.len(),
        all.iter().take(25).cloned().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn x32_corpus_agrees_with_the_oracle() {
    run("fuse-x32.json", 30);
}

#[test]
fn r2_16_corpus_agrees_with_the_oracle() {
    run("fuse-r2-16.json", 30);
}
