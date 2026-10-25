// SPDX-License-Identifier: GPL-3.0-or-later
//! Portable against every implementation, per input size (ADR-0027 CD-B0, CD-B2).
//!
//! Run: `cargo bench -p gorca-cpu --bench kernels [-- <filter>]`. Conditions (load, governor)
//! must be recorded with the numbers; see docs/reports/adr-0027-impl.md.
#![allow(
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::cast_possible_truncation
)]

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput};
use gorca_cpu::specs::{COMMON_PREFIX, UTF8_EXTEND, UTF8_SCALAR_COUNT};
use gorca_cpu::{Impl, KernelSpec, detect};

fn runnable<F: Copy>(spec: &KernelSpec<F>) -> Vec<(&'static str, F)> {
    let have = detect();
    let mut v = vec![("portable", spec.portable)];
    v.extend(
        spec.impls
            .iter()
            .filter_map(|i: &Impl<F>| i.checked(have).map(|f| (i.name, f))),
    );
    v
}

fn ascii(n: usize) -> String {
    "the quick brown fox jumps over the lazy dog 0123456789 "
        .chars()
        .cycle()
        .take(n)
        .collect()
}

fn cyrillic(n: usize) -> String {
    "быстрая коричневая лиса прыгает через ленивую собаку "
        .chars()
        .cycle()
        .take(n)
        .collect()
}

const SIZES: [usize; 6] = [16, 64, 256, 1024, 4096, 65536];

fn scalar_count(c: &mut Criterion) {
    let mut g = c.benchmark_group("utf8-scalar-count");
    for (kind, mk) in [
        ("ascii", ascii as fn(usize) -> String),
        ("cyrillic", cyrillic),
    ] {
        for n in SIZES {
            let s = mk(n);
            g.throughput(Throughput::Bytes(s.len() as u64));
            for (name, f) in runnable(&UTF8_SCALAR_COUNT) {
                g.bench_with_input(BenchmarkId::new(format!("{kind}/{name}"), n), &s, |b, s| {
                    b.iter(|| f(black_box(s)));
                });
            }
        }
    }
    g.finish();
}

fn prefix(c: &mut Criterion) {
    let mut g = c.benchmark_group("common-prefix-chars");
    for n in SIZES {
        let a: Vec<char> = ascii(n).chars().collect();
        let mut b = a.clone();
        // The difference sits at the end: the worst case for the scan.
        if let Some(last) = b.last_mut() {
            *last = '#';
        }
        g.throughput(Throughput::Elements(n as u64));
        // The code gorca-core runs today (`zip` + `take_while`), the real "before".
        g.bench_with_input(
            BenchmarkId::new("naive", n),
            &(a.clone(), b.clone()),
            |bch, (a, b)| {
                bch.iter(|| {
                    black_box(a)
                        .iter()
                        .zip(black_box(b))
                        .take_while(|(x, y)| x == y)
                        .count()
                });
            },
        );
        for (name, f) in runnable(&COMMON_PREFIX) {
            g.bench_with_input(
                BenchmarkId::new(name, n),
                &(a.clone(), b.clone()),
                |bch, (a, b)| {
                    bch.iter(|| f(black_box(a), black_box(b)));
                },
            );
        }
    }
    g.finish();
}

fn extend(c: &mut Criterion) {
    let mut g = c.benchmark_group("utf8-extend-chars");
    for (kind, mk) in [
        ("ascii", ascii as fn(usize) -> String),
        ("cyrillic", cyrillic),
    ] {
        for n in SIZES {
            let s = mk(n);
            g.throughput(Throughput::Bytes(s.len() as u64));
            for (name, f) in runnable(&UTF8_EXTEND) {
                g.bench_with_input(BenchmarkId::new(format!("{kind}/{name}"), n), &s, |b, s| {
                    let mut v: Vec<char> = Vec::with_capacity(s.len());
                    b.iter(|| {
                        v.clear();
                        f(black_box(s), &mut v);
                        black_box(&v);
                    });
                });
            }
        }
    }
    g.finish();
}

/// CD-B3 model of `gorca-core` `leaf_diff` (typing echo in a rebuilt text node): decode the new
/// text to scalars, then find the common prefix with the old scalars. "before" is the code in
/// the tree before the kernels (`chars().collect()` + `zip().take_while()`), "after" is the
/// wrapper pair through the table.
fn leaf_diff_model(c: &mut Criterion) {
    let mut g = c.benchmark_group("leaf-diff-model");
    for n in [40usize, 200, 1000, 5000] {
        let old_s = ascii(n);
        let old: Vec<char> = old_s.chars().collect();
        // One character typed in the middle.
        let mut new_s = old_s.clone();
        new_s.insert(n / 2, 'x');
        g.bench_function(BenchmarkId::new("before", n), |b| {
            b.iter(|| {
                let new: Vec<char> = black_box(&new_s).chars().collect();
                old.iter().zip(&new).take_while(|(x, y)| x == y).count()
            });
        });
        g.bench_function(BenchmarkId::new("after", n), |b| {
            b.iter(|| {
                let new = gorca_cpu::text::utf8_to_chars(black_box(&new_s));
                gorca_cpu::text::common_prefix_chars(&old, &new)
            });
        });
    }
    g.finish();
}

/// CD-B0: the cost of going through the table into a `#[target_feature]` trampoline, against the
/// portable function inline, on an input too short to matter.
fn dispatch_overhead(c: &mut Criterion) {
    let mut g = c.benchmark_group("dispatch-overhead");
    let s = ascii(8);
    g.bench_function("inline-portable-8B", |b| {
        b.iter(|| gorca_cpu::specs::utf8_scalar_count_portable(black_box(&s)));
    });
    let f = gorca_cpu::kernels().utf8_scalar_count;
    g.bench_function("table-fn-pointer-8B", |b| b.iter(|| f(black_box(&s))));
    g.bench_function("text-wrapper-8B", |b| {
        b.iter(|| gorca_cpu::text::utf8_scalar_count(black_box(&s)));
    });
    g.finish();
}

fn main() {
    let mut c = Criterion::default().configure_from_args();
    scalar_count(&mut c);
    prefix(&mut c);
    extend(&mut c);
    leaf_diff_model(&mut c);
    dispatch_overhead(&mut c);
    c.final_summary();
}
