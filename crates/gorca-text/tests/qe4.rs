//! ADR-0008 Q-E4: does Gecko's UTF-16 offset handling for native text fields need a TX-1 run
//! cache (a per-node map between the app's offsets and Unicode scalars), or does the text
//! window in the app's own offsets do?
//!
//! The M3b answer (docs/reports/m3b-m7b-impl.md): the window keeps the string Gecko hands out
//! (a phantom U+FEFF after each astral scalar, so every UTF-16 offset is the index of a
//! character of that very string) and the unit rules keep a phantom with the scalar before
//! it. No offset is ever converted, so nothing needs a run cache and nothing needs the text in
//! front of the caret. This file measures what each alternative would have cost.
//!
//! Run: `cargo test --release -p gorca-text --test qe4 -- --ignored --nocapture`

#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use std::hint::black_box;
use std::time::Instant;

use gorca_model::TextUnit;
use gorca_text::{seg, units};

const PHANTOM: char = '\u{FEFF}';

/// A text of `n` scalars: Latin and Cyrillic words, an emoji every `every` scalars.
fn scalars(n: usize, every: usize) -> Vec<char> {
    let words = ["hello ", "world ", "привет ", "мир ", "text "];
    let mut out: Vec<char> = Vec::with_capacity(n);
    let mut w = 0usize;
    while out.len() < n {
        if every > 0 && out.len() % every == every - 1 {
            out.push('\u{1F600}');
            continue;
        }
        out.extend(words[w % words.len()].chars());
        w += 1;
    }
    out.truncate(n);
    out
}

/// The string the application hands out: a phantom after each astral scalar.
fn with_phantoms(s: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(s.len() + s.len() / 8);
    for c in s {
        out.push(*c);
        if u32::from(*c) > 0xFFFF {
            out.push(PHANTOM);
        }
    }
    out
}

fn per_call_ns<F: FnMut(usize) -> usize>(calls: usize, mut f: F) -> f64 {
    let mut sink = 0usize;
    // Warm.
    for i in 0..calls / 10 {
        sink = sink.wrapping_add(f(i));
    }
    let t = Instant::now();
    for i in 0..calls {
        sink = sink.wrapping_add(f(i));
    }
    let ns = t.elapsed().as_nanos() as f64 / calls as f64;
    black_box(sink);
    ns
}

#[test]
#[ignore = "measurement: cargo test --release -p gorca-text --test qe4 -- --ignored --nocapture"]
fn q_e4_what_each_way_of_following_gecko_offsets_costs() {
    println!("== Q-E4: Gecko UTF-16 offsets in a native text window ==");
    println!(
        "host: {} CPUs",
        std::thread::available_parallelism().map_or(1, usize::from)
    );

    // A. The way taken: the window is the app's string; units are phantom-aware.
    for (label, every) in [
        ("no emoji", 0usize),
        ("emoji every 10th", 10),
        ("emoji every 2nd", 2),
    ] {
        let plain = scalars(16 * 1024, every);
        let app = with_phantoms(&plain);
        let n = app.len();
        let char_ns = per_call_ns(2_000_000, |i| seg::char_range(&app, (i * 7919) % n).end);
        let word_ns = per_call_ns(500_000, |i| seg::word_range(&app, (i * 7919) % n).end);
        println!(
            "A  window of {n} chars, {label}: char unit {char_ns:.0} ns, word unit {word_ns:.0} ns per call"
        );
    }

    // B. Converting the caret offset of each event to scalars (raw_to_scalar scans the text in
    // front of the offset) for texts of growing size: what a Reader that wanted scalars would pay
    // per caret event with the whole text at hand.
    for len in [1_000usize, 16_000, 100_000, 1_000_000] {
        let plain = scalars(len, 10);
        let app: String = with_phantoms(&plain).into_iter().collect();
        let raw_len = app.chars().count();
        let mid = raw_len / 2;
        let calls = if len >= 100_000 { 200 } else { 5_000 };
        let r2s = per_call_ns(calls, |i| {
            units::raw_to_scalar(&app, (mid + i % 64) as u32, TextUnit::Utf16Bom).unwrap_or(0)
                as usize
        });
        let s2r = per_call_ns(calls, |i| {
            units::scalar_to_raw(&app, (len / 2 + i % 64) as u32, TextUnit::Utf16Bom).unwrap_or(0)
                as usize
        });
        println!(
            "B  raw_to_scalar at the middle of {len} scalars: {:.1} us; scalar_to_raw {:.1} us per call (needs the {raw_len} chars in front of the caret)",
            r2s / 1000.0,
            s2r / 1000.0
        );
    }

    // C. A TX-1 style run cache: the raw offsets of every phantom, kept sorted; raw to scalar
    // is a binary search (offset minus the phantoms before it). It must be rebuilt or shifted
    // on every edit and it needs the whole text once.
    for len in [16_000usize, 100_000, 1_000_000] {
        let plain = scalars(len, 10);
        let app = with_phantoms(&plain);
        let t = Instant::now();
        let phantoms: Vec<u32> = app
            .iter()
            .enumerate()
            .filter(|(_, c)| **c == PHANTOM)
            .map(|(i, _)| i as u32)
            .collect();
        let build_us = t.elapsed().as_nanos() as f64 / 1000.0;
        let mid = app.len() / 2;
        let look = per_call_ns(2_000_000, |i| {
            let raw = (mid + i % 64) as u32;
            raw as usize - phantoms.partition_point(|p| *p < raw)
        });
        println!(
            "C  run cache over {len} scalars ({} phantoms): build {build_us:.0} us, lookup {look:.0} ns; an edit at the front shifts {} entries",
            phantoms.len(),
            phantoms.len()
        );
    }
}
