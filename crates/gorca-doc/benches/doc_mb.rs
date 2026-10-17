// SPDX-License-Identifier: GPL-3.0-or-later
//! MB benches for the web document core (ADR-0003 MB-4/MB-5 style): nav queries, flat-text
//! queries, update batches and publish latency at 1k / 10k / 100k / 1M nodes.
//!
//! Run: `cargo bench -p gorca-doc --bench doc_mb -- [--sizes 1000,10000] [--out DIR] [--quick]`
//! Output: a markdown table on stdout; with `--out`, `results.md` and `results.json` in DIR.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::collapsible_if,
    clippy::too_many_lines,
    clippy::print_stdout,
    clippy::unwrap_used
)]

use std::fmt::Write as _;
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gorca_doc::web::synth::{self, Rng};
use gorca_doc::web::{
    Cat, ChildSpec, DocNodeId, DocOp, DocWriter, NodeFlags, NodeSpec, PropsPatch, ReadUnit,
    SubtreeSpec, WebDoc, channel_from,
};
use gorca_model::Role;

#[derive(Clone, Debug)]
struct Stat {
    name: String,
    n: usize,
    p50: f64,
    p99: f64,
    max: f64,
    mean_bulk: Option<f64>,
}

fn pct(sorted: &[u64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() as f64 - 1.0) * q).round() as usize;
    sorted[i] as f64
}

fn stat_from(name: &str, mut ns: Vec<u64>, overhead: u64, mean_bulk: Option<f64>) -> Stat {
    for v in &mut ns {
        *v = v.saturating_sub(overhead);
    }
    ns.sort_unstable();
    Stat {
        name: name.to_owned(),
        n: ns.len(),
        p50: pct(&ns, 0.5),
        p99: pct(&ns, 0.99),
        max: ns.last().copied().unwrap_or(0) as f64,
        mean_bulk,
    }
}

/// Median cost of one `Instant::now()` pair, subtracted from per-call samples.
fn timer_overhead() -> u64 {
    let mut v: Vec<u64> = (0..20_000)
        .map(|_| {
            let t = Instant::now();
            black_box(());
            t.elapsed().as_nanos() as u64
        })
        .collect();
    v.sort_unstable();
    v[v.len() / 2]
}

/// Time `f(i)` per call (timer overhead removed) and in bulk (no per-call timer).
fn bench_query<R>(name: &str, samples: usize, overhead: u64, f: impl Fn(usize) -> R) -> Stat {
    for i in 0..samples.min(2000) {
        black_box(f(i));
    }
    let mut ns = Vec::with_capacity(samples);
    for i in 0..samples {
        let t = Instant::now();
        black_box(f(i));
        ns.push(t.elapsed().as_nanos() as u64);
    }
    let t = Instant::now();
    for i in 0..samples {
        black_box(f(i));
    }
    let bulk = t.elapsed().as_nanos() as f64 / samples as f64;
    stat_from(name, ns, overhead, Some(bulk))
}

fn pick_nodes(doc: &WebDoc, rng: &mut Rng, count: usize) -> Vec<DocNodeId> {
    let n = doc.len() as u32;
    (0..count)
        .filter_map(|_| doc.at_position(rng.below(n as usize) as u32))
        .collect()
}

fn find_with(doc: &WebDoc, rng: &mut Rng, c: Cat) -> Option<DocNodeId> {
    let n = doc.len();
    for _ in 0..64 {
        let from = doc.at_position(rng.below(n) as u32)?;
        if let Some(x) = doc.next(from, c) {
            return Some(x);
        }
    }
    doc.first_of(c)
}

fn make_paragraph(rng: &mut Rng, sig: u64) -> SubtreeSpec {
    let words = ["alpha", "beta", "gamma", "delta", "reader"];
    let t = |rng: &mut Rng| {
        SubtreeSpec::leaf(NodeSpec::new(Role::StaticText).text(words[rng.below(words.len())]))
    };
    let link = SubtreeSpec::with_children(
        NodeSpec::new(Role::Link).sig(sig),
        vec![SubtreeSpec::leaf(
            NodeSpec::new(Role::StaticText).text("more"),
        )],
    );
    SubtreeSpec::with_children(
        NodeSpec::new(Role::Paragraph)
            .flags(NodeFlags::BREAK_AFTER)
            .sig(sig),
        vec![t(rng), link, t(rng)],
    )
}

/// One SPA-style update batch (about 1k nodes touched): inserts, removals, a keyed child sync,
/// text and attribute changes. Targets are chosen on `doc` before timing starts.
fn make_batch(
    doc: &WebDoc,
    rng: &mut Rng,
    serial: u64,
    prev_inserted: &[DocNodeId],
) -> (Vec<DocOp>, usize) {
    let mut ops = Vec::new();
    let mut touched = 0usize;
    // The previous batch's insertions are removed again, so the document stays at its size
    // (route-swap churn) instead of growing during the measurement.
    for id in prev_inserted {
        if doc.is_attached(*id) {
            ops.push(DocOp::RemoveSubtree { id: *id });
        }
    }
    let regions: Vec<DocNodeId> = (0..24)
        .filter_map(|_| find_with(doc, rng, Cat::REGION))
        .collect();
    // 12 inserts of 12 paragraphs each (12 * 12 * 5 = 720 nodes).
    for k in 0..12usize {
        let Some(region) = regions.get(k % regions.len().max(1)).copied() else {
            continue;
        };
        let kids: Vec<DocNodeId> = doc.children(region).collect();
        let before = if kids.is_empty() {
            None
        } else {
            Some(kids[rng.below(kids.len())])
        };
        let paras: Vec<SubtreeSpec> = (0..12)
            .map(|i| make_paragraph(rng, serial * 1000 + (k * 12 + i) as u64))
            .collect();
        let sub = SubtreeSpec::with_children(
            NodeSpec::new(Role::Panel).flags(NodeFlags::BREAK_AFTER),
            paras,
        );
        touched += sub.count();
        ops.push(DocOp::InsertSubtree {
            parent: Some(region),
            before,
            subtree: Arc::new(sub),
        });
    }
    // 6 removals of paragraphs (detached, then reaped at the end of the batch).
    for _ in 0..6 {
        if let Some(p) = find_with(doc, rng, Cat::TEXT_BLOCK) {
            if doc.parent(p).is_some() {
                touched += 5;
                ops.push(DocOp::RemoveSubtree { id: p });
            }
        }
    }
    // Keyed sync of one region's children: swap two, drop one, add one (framework re-render).
    if let Some(region) = regions.first().copied() {
        let kids: Vec<DocNodeId> = doc.children(region).collect();
        if kids.len() >= 4
            && !ops
                .iter()
                .any(|o| matches!(o, DocOp::RemoveSubtree { id } if kids.contains(id)))
        {
            let mut desired: Vec<ChildSpec> =
                kids.iter().map(|k| ChildSpec::Existing(*k)).collect();
            desired.swap(0, 2);
            desired.remove(1);
            desired.push(ChildSpec::New(Arc::new(make_paragraph(rng, serial))));
            touched += 6;
            ops.push(DocOp::SyncChildren {
                parent: region,
                desired,
            });
        }
    }
    // 150 text changes, 100 attribute changes.
    for _ in 0..150 {
        if let Some(t) = find_with(doc, rng, Cat::TEXT_RUN) {
            touched += 1;
            ops.push(DocOp::SetText {
                id: t,
                text: Some(Arc::from("updated text")),
            });
        }
    }
    for _ in 0..100 {
        if let Some(l) = find_with(doc, rng, Cat::LINK) {
            touched += 1;
            ops.push(DocOp::SetProps {
                id: l,
                patch: PropsPatch {
                    flags: Some(NodeFlags::VISITED),
                    ..PropsPatch::default()
                },
            });
        }
    }
    ops.push(DocOp::Reap { id: None });
    (ops, touched)
}

struct BatchResult {
    stats: Vec<Stat>,
    touched: usize,
    ops: usize,
    fallbacks: u64,
    slow_waits: u64,
}

fn bench_updates(
    w: &mut DocWriter,
    rng: &mut Rng,
    batches: usize,
    prev_inserted: &mut Vec<DocNodeId>,
) -> BatchResult {
    let (mut catchup, mut apply, mut publish, mut total_crit, mut total_steady) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut touched, mut nops) = (0, 0);
    for b in 0..batches + 5 {
        // Catch-up (replay of the previous batch on the back copy) timed separately.
        let t0 = Instant::now();
        let doc_len = w.doc().len();
        let tc = t0.elapsed().as_nanos() as u64;
        black_box(doc_len);
        let (ops, tch) = make_batch(w.doc(), rng, b as u64, prev_inserted);
        let n = ops.len();
        let t1 = Instant::now();
        prev_inserted.clear();
        for op in ops {
            // Targets can go stale inside one batch (a removed paragraph was also a sync
            // child); a rejected op is part of real churn and is simply skipped.
            let is_insert = matches!(op, DocOp::InsertSubtree { .. });
            if let Ok(out) = w.apply(op) {
                if is_insert {
                    if let Some(first) = out.created.first() {
                        prev_inserted.push(*first);
                    }
                }
            }
        }
        let ta = t1.elapsed().as_nanos() as u64;
        let t2 = Instant::now();
        w.publish();
        let tp = t2.elapsed().as_nanos() as u64;
        if b >= 5 {
            catchup.push(tc);
            apply.push(ta);
            publish.push(tp);
            total_crit.push(ta + tp);
            total_steady.push(tc + ta + tp);
            touched = tch;
            nops = n;
        }
    }
    let st = w.stats();
    BatchResult {
        stats: vec![
            stat_from("batch: apply (writer, back copy)", apply, 0, None),
            stat_from("batch: publish() only", publish, 0, None),
            stat_from("batch: apply + publish", total_crit, 0, None),
            stat_from("batch: replay on other copy (deferred)", catchup, 0, None),
            stat_from(
                "batch: replay + apply + publish (steady state)",
                total_steady,
                0,
                None,
            ),
        ],
        touched,
        ops: nops,
        fallbacks: st.fallback_clones,
        slow_waits: st.slow_waits,
    }
}

fn fmt_ns(v: f64) -> String {
    if v >= 1_000_000.0 {
        format!("{:.2} ms", v / 1e6)
    } else if v >= 1_000.0 {
        format!("{:.2} us", v / 1e3)
    } else {
        format!("{v:.0} ns")
    }
}

fn run_size(n: usize, quick: bool, md: &mut String, json: &mut Vec<String>) {
    let samples = if quick {
        2_000
    } else if n >= 1_000_000 {
        20_000
    } else {
        100_000
    };
    let overhead = timer_overhead();
    let mut rng = Rng::new(7 + n as u64);

    // ---- build
    let spec = synth::page(n, 1);
    let nodes = spec.count();
    let mut doc = WebDoc::new(0x5eed);
    let spec = Arc::new(spec);
    let t = Instant::now();
    let built = doc.apply(&DocOp::InsertSubtree {
        parent: None,
        before: None,
        subtree: Arc::clone(&spec),
    });
    let build_ns = t.elapsed().as_nanos() as f64;
    assert!(built.is_ok());
    drop(spec);
    let heap = doc.heap_bytes();
    let headings = doc.count(Cat::HEADING).unwrap_or(0);
    let links = doc.count(Cat::LINK).unwrap_or(0);
    let (_, lines) = (doc.text_len(), doc.line_count());
    println!(
        "== {n} nodes requested, {nodes} built, {headings} headings, {links} links, {lines} lines"
    );

    // ---- nav queries
    let from = pick_nodes(&doc, &mut rng, 4096);
    let m = from.len();
    let scopes: Vec<DocNodeId> = from
        .iter()
        .map(|f| doc.landmark_of(*f).unwrap_or(doc.root().unwrap_or(*f)))
        .collect();
    let hcount = headings.max(1) as usize;
    let mut stats: Vec<Stat> = Vec::new();
    stats.push(bench_query("next heading", samples, overhead, |i| {
        doc.next(from[i % m], Cat::HEADING)
    }));
    stats.push(bench_query("prev heading", samples, overhead, |i| {
        doc.prev(from[i % m], Cat::HEADING)
    }));
    stats.push(bench_query(
        "next heading level 2 (filter)",
        samples,
        overhead,
        |i| doc.next(from[i % m], Cat::HEADING_2),
    ));
    stats.push(bench_query(
        "next heading level 1-3 (filter)",
        samples,
        overhead,
        |i| doc.next(from[i % m], Cat::headings_up_to(3)),
    ));
    stats.push(bench_query("next link", samples, overhead, |i| {
        doc.next(from[i % m], Cat::LINK)
    }));
    stats.push(bench_query("next unvisited link", samples, overhead, |i| {
        doc.next(from[i % m], Cat::LINK_UNVISITED)
    }));
    stats.push(bench_query(
        "next landmark/region",
        samples,
        overhead,
        |i| doc.next(from[i % m], Cat::ANY_LANDMARK),
    ));
    stats.push(bench_query("next form field", samples, overhead, |i| {
        doc.next(from[i % m], Cat::FORM_FIELD)
    }));
    stats.push(bench_query("next table", samples, overhead, |i| {
        doc.next(from[i % m], Cat::TABLE)
    }));
    stats.push(bench_query("next list item", samples, overhead, |i| {
        doc.next(from[i % m], Cat::LIST_ITEM)
    }));
    stats.push(bench_query(
        "next text run (dense category)",
        samples,
        overhead,
        |i| doc.next(from[i % m], Cat::TEXT_RUN),
    ));
    stats.push(bench_query(
        "next link within landmark",
        samples,
        overhead,
        |i| doc.next_in(from[i % m], Cat::LINK, scopes[i % m]),
    ));
    stats.push(bench_query(
        "next_in_landmark(link)",
        samples,
        overhead,
        |i| doc.next_in_landmark(from[i % m], Cat::LINK),
    ));
    stats.push(bench_query("kth heading", samples, overhead, |i| {
        doc.kth(Cat::HEADING, (i * 7919 % hcount) as u32)
    }));
    stats.push(bench_query(
        "rank (heading k of N)",
        samples,
        overhead,
        |i| {
            doc.next(from[i % m], Cat::HEADING)
                .and_then(|h| doc.rank(h, Cat::HEADING))
        },
    ));
    stats.push(bench_query("cmp_order", samples, overhead, |i| {
        doc.cmp_order(from[i % m], from[(i + 1) % m])
    }));
    stats.push(bench_query("landmark_of", samples, overhead, |i| {
        doc.landmark_of(from[i % m])
    }));
    let nav_len = stats.len();

    // ---- flat text
    let total = doc.text_len().max(1);
    let offs: Vec<u32> = (0..4096)
        .map(|_| rng.below(total as usize) as u32)
        .collect();
    stats.push(bench_query(
        "text: offset_of(node)",
        samples,
        overhead,
        |i| doc.offset_of(from[i % m]),
    ));
    stats.push(bench_query("text: at_offset", samples, overhead, |i| {
        doc.at_offset(offs[i % m])
    }));
    stats.push(bench_query("text: line_start(k)", samples, overhead, |i| {
        doc.line_start((offs[i % m] / 40) % lines)
    }));
    stats.push(bench_query(
        "text: line_of(offset)",
        samples,
        overhead,
        |i| doc.line_of(offs[i % m]),
    ));
    stats.push(bench_query(
        "text: line_range + read line",
        samples,
        overhead,
        |i| {
            let (s, e) = doc.line_range(offs[i % m]);
            doc.text_in(s, e).len()
        },
    ));
    stats.push(bench_query(
        "text: word at offset",
        samples,
        overhead,
        |i| doc.unit_range(ReadUnit::Word, offs[i % m]),
    ));
    stats.push(bench_query("text: next line", samples, overhead, |i| {
        doc.next_unit(ReadUnit::Line, offs[i % m])
    }));

    // ---- updates and publication
    let (mut w, r) = channel_from(doc);
    let batches = if quick {
        20
    } else if n >= 1_000_000 {
        40
    } else {
        300
    };
    let mut prev_inserted: Vec<DocNodeId> = Vec::new();
    let res = bench_updates(&mut w, &mut rng, batches, &mut prev_inserted);
    let upd_len = w.doc().len();

    // Publish latency of a minimal change at this size.
    let probe = find_with(w.doc(), &mut rng, Cat::TEXT_RUN);
    let mut pub_ns = Vec::new();
    if let Some(p) = probe {
        for i in 0..(if quick { 200 } else { 2000 }) {
            let _ = w.apply(DocOp::SetText {
                id: p,
                text: Some(Arc::from(if i % 2 == 0 { "a" } else { "b" })),
            });
            let t = Instant::now();
            w.publish();
            pub_ns.push(t.elapsed().as_nanos() as u64);
        }
    }
    let pub_stat = stat_from("publish() after a 1-op change", pub_ns, 0, None);

    // ---- reader under writer load
    let stop = Arc::new(AtomicBool::new(false));
    let reader_stat = {
        let rd = r.clone();
        let stop2 = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let mut ns = Vec::new();
            let mut rng = Rng::new(99);
            let n = rd.with(WebDoc::len).max(1);
            while !stop2.load(Ordering::Relaxed) {
                let pos = rng.below(n) as u32;
                let t = Instant::now();
                let x = rd.with(|d| d.at_position(pos).and_then(|f| d.next(f, Cat::HEADING)));
                ns.push(t.elapsed().as_nanos() as u64);
                black_box(x);
            }
            ns
        });
        let end = Instant::now() + Duration::from_millis(if quick { 300 } else { 1500 });
        let mut k = 1_000_000u64;
        while Instant::now() < end {
            let (ops, _) = make_batch(w.doc(), &mut rng, k, &prev_inserted);
            k += 1;
            prev_inserted.clear();
            for op in ops {
                let is_insert = matches!(op, DocOp::InsertSubtree { .. });
                if let Ok(out) = w.apply(op) {
                    if is_insert {
                        if let Some(first) = out.created.first() {
                            prev_inserted.push(*first);
                        }
                    }
                }
            }
            w.publish();
        }
        stop.store(true, Ordering::Relaxed);
        let ns = handle.join().unwrap_or_default();
        stat_from(
            "reader: at_position + next heading, writer running batches",
            ns,
            overhead,
            None,
        )
    };
    let fin = w.stats();

    // ---- report
    let _ = writeln!(
        md,
        "\n### {n} nodes ({nodes} built, {upd_len} after update runs)\n"
    );
    let _ = writeln!(
        md,
        "build {:.1} ms ({:.0} ns/node), heap {:.1} MB ({:.0} B/node), headings {headings}, links {links}, lines {lines}, timer overhead {overhead} ns (subtracted)\n",
        build_ns / 1e6,
        build_ns / nodes as f64,
        heap as f64 / 1e6,
        heap as f64 / nodes as f64
    );
    let _ = writeln!(
        md,
        "| operation | samples | p50 | p99 | max | bulk mean |\n|---|---|---|---|---|---|"
    );
    let mut all: Vec<&Stat> = stats.iter().collect();
    all.extend(res.stats.iter());
    all.push(&pub_stat);
    all.push(&reader_stat);
    for s in &all {
        let _ = writeln!(
            md,
            "| {} | {} | {} | {} | {} | {} |",
            s.name,
            s.n,
            fmt_ns(s.p50),
            fmt_ns(s.p99),
            fmt_ns(s.max),
            s.mean_bulk.map_or_else(|| "-".to_owned(), fmt_ns)
        );
        json.push(format!(
            "{{\"nodes\":{n},\"op\":\"{}\",\"samples\":{},\"p50_ns\":{:.0},\"p99_ns\":{:.0},\"max_ns\":{:.0},\"bulk_mean_ns\":{}}}",
            s.name,
            s.n,
            s.p50,
            s.p99,
            s.max,
            s.mean_bulk.map_or_else(|| "null".to_owned(), |v| format!("{v:.1}"))
        ));
    }
    let _ = writeln!(
        md,
        "\nbatch: {} ops touching ~{} nodes; writer waits {} (slow >1 ms: {}), fallback clones {} (batch phase) / {} (total)\n",
        res.ops, res.touched, fin.waits, res.slow_waits, res.fallbacks, fin.fallback_clones
    );
    println!(
        "{}",
        md.lines()
            .rev()
            .take(all.len() + 6)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n")
    );
    let _ = nav_len;
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut sizes = vec![1_000usize, 10_000, 100_000, 1_000_000];
    let mut out: Option<String> = None;
    let mut quick = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--sizes" => {
                if let Some(v) = args.get(i + 1) {
                    sizes = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                }
                i += 1;
            }
            "--out" => {
                out = args.get(i + 1).cloned();
                i += 1;
            }
            "--quick" => quick = true,
            _ => {}
        }
        i += 1;
    }
    let load = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
    let gov = std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
        .unwrap_or_default();
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with("model name"))
        .unwrap_or("")
        .to_owned();
    let mut md = String::new();
    let _ = writeln!(
        md,
        "# gorca-doc MB benches\n\nconditions: loadavg `{}`, governor `{}`, {}, cores {}, quick={quick}",
        load.trim(),
        gov.trim(),
        cpu.trim(),
        std::thread::available_parallelism().map_or(0, std::num::NonZero::get)
    );
    let mut json = Vec::new();
    for n in sizes {
        run_size(n, quick, &mut md, &mut json);
    }
    if let Some(dir) = out {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(format!("{dir}/results.md"), &md);
        let _ = std::fs::write(
            format!("{dir}/results.json"),
            format!("[\n{}\n]\n", json.join(",\n")),
        );
        println!("wrote {dir}/results.md and results.json");
    }
}
