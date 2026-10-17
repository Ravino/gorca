// SPDX-License-Identifier: GPL-3.0-or-later
//! MB-1: mirror store microbenchmark (decision MS-3). See docs/reports/2026-10-13-mb1-mirror-store.md.
//!
//! ```text
//! mb1 run --out DIR [--reps 3] [--sizes 1000,10000,100000,1000000]
//!         [--rates 0,1000,10000,50000,max] [--stores a,b] [--secs 2] [--readers 4]
//!         [--skip-steady] [--skip-removal] [--skip-rss]
//! mb1 summary DIR
//! ```
//!
//! Interleaved repeats: every repeat runs every (size, rate) cell once, with the store order
//! rotated per cell and repeat, so host load drift hits all stores alike. Results are gated on
//! the median over repeats.
//!
//! Dedicated OS threads that sleep and spin on purpose: sleeping, casts and unwraps in a bench
//! harness are fine, so the matching lints are relaxed crate-wide.
#![allow(
    clippy::disallowed_methods,
    clippy::collapsible_if,
    clippy::unnecessary_wraps,
    clippy::redundant_closure_for_method_calls,
    clippy::manual_ilog2,
    clippy::unchecked_time_subtraction,
    clippy::struct_field_names,
    clippy::manual_midpoint,
    clippy::semicolon_if_nothing_returned,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_lossless,
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::items_after_statements,
    clippy::unreadable_literal,
    clippy::manual_let_else,
    clippy::if_not_else,
    clippy::range_plus_one,
    clippy::uninlined_format_args
)]

mod hist;
mod scen;
mod store;
mod summary;
mod wl;

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use scen::{Rate, run_cell, run_removal};
use store::{
    ArcSwapStore, DashStore, LrStore, PapayaPinStore, PapayaStore, SccIndexStore, SccMapStore,
    Store,
};

pub const ALL_STORES: &[&str] = &[
    "papaya",
    "papaya-pin",
    "scc-map",
    "scc-index",
    "left-right",
    "arcswap-sharded",
    "dashmap",
];

macro_rules! dispatch {
    ($name:expr, $f:ident($($arg:expr),*)) => {
        match $name {
            "papaya" => $f::<PapayaStore>($($arg),*),
            "papaya-pin" => $f::<PapayaPinStore>($($arg),*),
            "scc-map" => $f::<SccMapStore>($($arg),*),
            "scc-index" => $f::<SccIndexStore>($($arg),*),
            "left-right" => $f::<LrStore>($($arg),*),
            "arcswap-sharded" => $f::<ArcSwapStore>($($arg),*),
            "dashmap" => $f::<DashStore>($($arg),*),
            other => Err(format!("unknown store {other}")),
        }
    };
}

struct Opts {
    out: PathBuf,
    reps: usize,
    sizes: Vec<u64>,
    rates: Vec<Rate>,
    stores: Vec<String>,
    secs: f64,
    readers: usize,
    steady: bool,
    removal: bool,
    rss: bool,
}

fn parse_rate(s: &str) -> Result<Rate, String> {
    match s {
        "0" | "off" => Ok(Rate::Off),
        "max" => Ok(Rate::Max),
        n => n
            .parse()
            .map(Rate::Hz)
            .map_err(|e| format!("rate {n}: {e}")),
    }
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        out: PathBuf::from("bench/results/mirror-mb1/run"),
        reps: 3,
        sizes: vec![1_000, 10_000, 100_000, 1_000_000],
        rates: vec![
            Rate::Off,
            Rate::Hz(1_000),
            Rate::Hz(10_000),
            Rate::Hz(50_000),
            Rate::Max,
        ],
        stores: ALL_STORES.iter().map(|s| (*s).to_string()).collect(),
        secs: 2.0,
        readers: 4,
        steady: true,
        removal: true,
        rss: true,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--out" => o.out = PathBuf::from(val()?),
            "--reps" => o.reps = val()?.parse().map_err(|e| format!("{e}"))?,
            "--secs" => o.secs = val()?.parse().map_err(|e| format!("{e}"))?,
            "--readers" => o.readers = val()?.parse().map_err(|e| format!("{e}"))?,
            "--sizes" => {
                o.sizes = val()?
                    .split(',')
                    .map(|v| v.parse().map_err(|e| format!("{e}")))
                    .collect::<Result<_, _>>()?;
            }
            "--rates" => {
                o.rates = val()?
                    .split(',')
                    .map(parse_rate)
                    .collect::<Result<_, _>>()?
            }
            "--stores" => o.stores = val()?.split(',').map(str::to_string).collect(),
            "--skip-steady" => o.steady = false,
            "--skip-removal" => o.removal = false,
            "--skip-rss" => o.rss = false,
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(o)
}

fn append(path: &Path, header: &str, row: &str) {
    let new = !path.exists();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        if new {
            let _ = writeln!(f, "{header}");
        }
        let _ = writeln!(f, "{row}");
    }
}

const STEADY_HDR: &str = "store,size,rate,rep,load1,cmds_per_s,r_p50,r_p95,r_p99,r_p999,r_max,writes_per_s,b_p50,b_p99,vis_p50,vis_p99,vis_p999,proc_cpu,writer_cpu";

fn steady_cell<S: Store>(
    size: u64,
    rate: Rate,
    rep: usize,
    readers: usize,
    secs: f64,
) -> Result<String, String> {
    let m = run_cell::<S>(
        size,
        rate,
        readers,
        Duration::from_millis(400),
        Duration::from_secs_f64(secs),
    );
    Ok(format!(
        "{},{},{},{},{:.2},{:.0},{},{},{},{},{},{:.0},{},{},{},{},{},{:.3},{:.3}",
        S::NAME,
        size,
        rate.label(),
        rep,
        m.load1,
        m.cmds_per_s,
        m.read.q(0.5),
        m.read.q(0.95),
        m.read.q(0.99),
        m.read.q(0.999),
        m.read.max(),
        m.writes_per_s,
        m.batch.q(0.5),
        m.batch.q(0.99),
        m.vis.q(0.5),
        m.vis.q(0.99),
        m.vis.q(0.999),
        m.proc_cpu,
        m.writer_cpu
    ))
}

const REMOVAL_HDR: &str = "store,variant,rep,load1,removal_ms,survivors,base_cmds_per_s,during_cmds_per_s,base_p50,base_p99,base_p999,base_max,during_p50,during_p99,during_p999,during_max,after_p99,after_max";

fn removal_cell<S: Store>(
    doomed: u64,
    chunk: usize,
    label: &str,
    rep: usize,
    readers: usize,
) -> Result<String, String> {
    let r = run_removal::<S>(doomed, chunk, readers);
    Ok(format!(
        "{},{},{},{:.2},{:.2},{},{:.0},{:.0},{},{},{},{},{},{},{},{},{},{}",
        S::NAME,
        label,
        rep,
        r.load1,
        r.removal_ms,
        r.survivors,
        r.base_cmds_per_s,
        r.during_cmds_per_s,
        r.base.q(0.5),
        r.base.q(0.99),
        r.base.q(0.999),
        r.base.max(),
        r.during.q(0.5),
        r.during.q(0.99),
        r.during.q(0.999),
        r.during.max(),
        r.after.q(0.99),
        r.after.max()
    ))
}

/// Child-process mode: build the tree, print RSS growth in bytes.
fn rss_child<S: Store>(n: u64) -> Result<String, String> {
    let before = wl::rss_bytes();
    let s = S::new();
    wl::build(&mut s.writer(), n);
    let after = wl::rss_bytes();
    std::hint::black_box(&s);
    Ok(format!("{}", after.saturating_sub(before)))
}

fn rss_floor(n: u64) -> String {
    let before = wl::rss_bytes();
    let v: Vec<Arc<store::Node>> = (0..n).map(|i| wl::mk_node(n, i, 0)).collect();
    let after = wl::rss_bytes();
    std::hint::black_box(&v);
    format!("{}", after.saturating_sub(before))
}

fn rss_run(store: &str, n: u64) -> Result<u64, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let out = Command::new(exe)
        .args(["rss-child", store, &n.to_string()])
        .output()
        .map_err(|e| e.to_string())?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .map_err(|e| {
            format!(
                "rss child {store}/{n}: {e} ({})",
                String::from_utf8_lossy(&out.stderr)
            )
        })
}

fn timer_overhead_ns() -> f64 {
    let mut best = f64::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        let mut acc = 0u128;
        for _ in 0..1_000_000 {
            let a = Instant::now();
            acc += a.elapsed().as_nanos();
        }
        std::hint::black_box(acc);
        best = best.min(t.elapsed().as_nanos() as f64 / 1e6);
    }
    best
}

fn meta(o: &Opts) -> String {
    let mut m = String::new();
    let sh = |c: &str, a: &[&str]| {
        Command::new(c)
            .args(a)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let _ = writeln!(m, "date: {}", sh("date", &["-Is"]));
    let _ = writeln!(m, "kernel: {}", sh("uname", &["-r"]));
    let _ = writeln!(
        m,
        "cpus: {}",
        std::thread::available_parallelism().map_or(0, |n| n.get())
    );
    let _ = writeln!(
        m,
        "cpu_model: {}",
        wl::read_file("/proc/cpuinfo")
            .lines()
            .find(|l| l.starts_with("model name"))
            .unwrap_or("")
    );
    let _ = writeln!(
        m,
        "governor: {}",
        wl::read_file("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor").trim()
    );
    let _ = writeln!(
        m,
        "loadavg_start: {}",
        wl::read_file("/proc/loadavg").trim()
    );
    let _ = writeln!(m, "rustc: {}", sh("rustc", &["--version"]));
    let _ = writeln!(
        m,
        "timer_overhead_ns (Instant pair): {:.1}",
        timer_overhead_ns()
    );
    let _ = writeln!(
        m,
        "reps: {}  secs/cell: {}  readers: {}",
        o.reps, o.secs, o.readers
    );
    let _ = writeln!(m, "sizes: {:?}", o.sizes);
    let _ = writeln!(
        m,
        "rates: {:?}",
        o.rates.iter().map(|r| r.label()).collect::<Vec<_>>()
    );
    let _ = writeln!(m, "stores: {:?}", o.stores);
    m
}

fn run(o: &Opts) -> Result<(), String> {
    std::fs::create_dir_all(&o.out).map_err(|e| e.to_string())?;
    let mut m = meta(o);
    std::fs::write(o.out.join("meta.txt"), &m).map_err(|e| e.to_string())?;
    eprint!("{m}");

    if o.steady {
        let cells: Vec<(u64, Rate)> = o
            .sizes
            .iter()
            .flat_map(|s| o.rates.iter().map(move |r| (*s, *r)))
            .collect();
        for rep in 0..o.reps {
            for (ci, (size, rate)) in cells.iter().enumerate() {
                for k in 0..o.stores.len() {
                    let name = &o.stores[(k + rep + ci) % o.stores.len()];
                    eprintln!(
                        "[steady] rep {rep} size {size} rate {} {name}",
                        rate.label()
                    );
                    let row = dispatch!(
                        name.as_str(),
                        steady_cell(*size, *rate, rep, o.readers, o.secs)
                    )?;
                    append(&o.out.join("steady.csv"), STEADY_HDR, &row);
                }
            }
        }
    }

    if o.removal {
        let mut variants: Vec<(String, String, usize)> = o
            .stores
            .iter()
            .map(|s| (s.clone(), "chunk1024".to_string(), 1024))
            .collect();
        if o.stores.iter().any(|s| s == "left-right") {
            variants.push(("left-right".into(), "oneshot".into(), usize::MAX));
        }
        for rep in 0..o.reps.max(3) {
            for k in 0..variants.len() {
                let (name, label, chunk) = &variants[(k + rep) % variants.len()];
                eprintln!("[removal] rep {rep} {name} {label}");
                let row = dispatch!(
                    name.as_str(),
                    removal_cell(100_000, *chunk, label, rep, o.readers)
                )?;
                append(&o.out.join("removal.csv"), REMOVAL_HDR, &row);
            }
        }
    }

    if o.rss {
        for size in &o.sizes {
            for rep in 0..o.reps {
                let floor = Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
                    .args(["rss-floor", &size.to_string()])
                    .output()
                    .map_err(|e| e.to_string())?;
                let floor = String::from_utf8_lossy(&floor.stdout).trim().to_string();
                append(
                    &o.out.join("rss.csv"),
                    "store,size,rep,rss_bytes",
                    &format!("floor,{size},{rep},{floor}"),
                );
                for name in &o.stores {
                    eprintln!("[rss] rep {rep} size {size} {name}");
                    let b = rss_run(name, *size)?;
                    append(
                        &o.out.join("rss.csv"),
                        "store,size,rep,rss_bytes",
                        &format!("{name},{size},{rep},{b}"),
                    );
                }
            }
        }
    }
    let _ = writeln!(m, "loadavg_end: {}", wl::read_file("/proc/loadavg").trim());
    std::fs::write(o.out.join("meta.txt"), &m).map_err(|e| e.to_string())?;
    summary::write(&o.out)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let res = match args.first().map(String::as_str) {
        Some("run") => parse_opts(&args[1..]).and_then(|o| run(&o)),
        Some("summary") => args
            .get(1)
            .ok_or("summary DIR".to_string())
            .and_then(|d| summary::write(Path::new(d))),
        Some("rss-child") => {
            let n = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(1000);
            let name = args.get(1).cloned().unwrap_or_default();
            dispatch!(name.as_str(), rss_child(n)).map(|s| println!("{s}"))
        }
        Some("rss-floor") => {
            println!(
                "{}",
                rss_floor(args.get(1).and_then(|v| v.parse().ok()).unwrap_or(1000))
            );
            Ok(())
        }
        _ => Err("usage: mb1 run|summary|rss-child ...".to_string()),
    };
    if let Err(e) = res {
        eprintln!("error: {e}");
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Reader, Session, Writer};

    fn smoke<S: Store>() {
        let s = S::new();
        {
            let mut w = s.writer();
            wl::build(&mut w, 100);
            w.remove(5);
            w.put(7, wl::mk_leaf(7, 0, 9));
            w.publish();
        }
        let mut r = s.reader();
        let sess = r.session();
        assert!(sess.get(5).is_none());
        assert_eq!(sess.get(7).map(|n| n.version), Some(9));
        assert!(sess.get(99).is_some());
    }

    #[test]
    fn every_store_round_trips() {
        smoke::<PapayaStore>();
        smoke::<PapayaPinStore>();
        smoke::<SccMapStore>();
        smoke::<SccIndexStore>();
        smoke::<LrStore>();
        smoke::<ArcSwapStore>();
        smoke::<DashStore>();
    }

    #[test]
    fn short_cell_and_removal_complete() {
        let m = run_cell::<LrStore>(
            1000,
            Rate::Hz(10_000),
            2,
            Duration::from_millis(50),
            Duration::from_millis(200),
        );
        assert!(m.read.count() > 0, "reads {:?}", m.read);
        assert!(m.writes_per_s > 0.0, "writes");
        assert!(m.vis.count() > 0, "vis {:?}", m.vis);
        let r = run_removal::<PapayaStore>(2000, 512, 2);
        assert_eq!(r.survivors, 0);
    }
}
