// SPDX-License-Identifier: GPL-3.0-or-later
//! Differential test against the A3 reference corpus (M22a-B1a/B1b): every case, under every
//! delivery order (canonical, reversed, shuffled with duplicates and batch cuts), must give the
//! oracle's values, `ProvCode`s, hedge, taint, `known`/`stale`/`fallback` masks and, in trace
//! mode, the full `explain` trail, evidence list and parked facts.
//!
//! The non-primary side is built from the **raw fact events**: a port of the oracle's pure
//! admission (checked against the corpus' admission column), the side table with its validity
//! filter and ranking, and a toy anchor index. The expected trails are only compared against.
//!
//! `fuse-x32.json` is the target layout (full bit encodings are compared); `fuse-r2-16.json` is
//! the older 16-group layout with the same semantics (per-group maps and the low words of the
//! encodings are compared).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{ToyQuirks, compare, evaluate_full, fixture, ingest, load_cases};

fn run(fixture_name: &str, min_cases: usize) {
    let doc = fixture(fixture_name);
    let cases = load_cases(&doc);
    assert!(cases.len() >= min_cases, "corpus shrank: {}", cases.len());
    let mut all = Vec::new();
    let mut runs = 0usize;
    for case in &cases {
        for (name, batches) in &case.deliveries {
            let order: Vec<usize> = batches.iter().flatten().copied().collect();
            let (world, labels) = ingest(case, &order);
            // Admission is per event, so duplicates and reordering must not change a label.
            let labels = labels_by_event(case, &order, labels);
            let traced = evaluate_full(case, &world, &ToyQuirks, true);
            for d in compare(case, &traced, &labels, true) {
                all.push(format!("[{name}] {d}"));
            }
            // The hot path (no tracer) must produce the identical entries.
            let plain = evaluate_full(case, &world, &ToyQuirks, false);
            if plain.entries() != traced.entries() {
                all.push(format!("[{name}] {}: tracer changed the result", case.id));
            }
            runs += 1;
        }
    }
    assert!(runs >= cases.len() * 4, "every case has four deliveries");
    assert!(
        all.is_empty(),
        "{} mismatches over {runs} runs; first 12:\n{}",
        all.len(),
        all.iter().take(12).cloned().collect::<Vec<_>>().join("\n")
    );
}

/// Labels of events that were never delivered stay `-`; restore the canonical label for the
/// events that were, so a delivery with all events compares equal to the corpus column.
fn labels_by_event(case: &support::Case, order: &[usize], labels: Vec<String>) -> Vec<String> {
    let delivered: std::collections::HashSet<usize> = order.iter().copied().collect();
    labels
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            if delivered.contains(&i) {
                l
            } else {
                case.admission[i].clone()
            }
        })
        .collect()
}

#[test]
fn x32_corpus_agrees_with_the_oracle() {
    run("fuse-x32.json", 30);
}

#[test]
fn r2_16_corpus_agrees_with_the_oracle() {
    run("fuse-r2-16.json", 30);
}
