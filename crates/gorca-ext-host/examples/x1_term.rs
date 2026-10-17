// SPDX-License-Identifier: GPL-3.0-or-later
//! X1 for the terminal policy (`ext-vte`): the whole host path of `TermSession::step` as the
//! Reader calls it (slot `try_lock`, input lowering, the call, lifting the actions), on the
//! component, on the native build behind the host and on the native build called directly (what
//! `--terminal-policy=native` costs without the host), for the inputs of the M6 gates.
//!
//! ```sh
//! cargo xtask build-ext
//! cargo run --release -p gorca-ext-host --example x1_term -- [iterations] [wasm|native-host|direct] > out.tsv
//! ```
//!
//! Output (TSV, nanoseconds): `case n p50 p99 p999 max mean`. Rows:
//!
//! * `key_typed_backspace` a typed key and a BackSpace, alternating (the key echo of V/VT);
//! * `edit_insert_delete` the shell's echo as `text-changed` payloads, alternating;
//! * `caret` a caret move;
//! * `screen_<size>` the screen reply of the settle (diff against the previous screen, one new
//!   line scrolled in): 24x80, 60x240 and 300x120 Cyrillic (72 KB);
//! * `tick_idle` a timer tick with nothing to do.
//!
//! Gate: the V1 settle (30 ms) plus gorca's 5 ms; the per-input cost should stay in the tens of
//! microseconds, the screen reply is bounded by the 8 ms limit of the hook.
#![allow(
    clippy::pedantic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::disallowed_methods
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gorca_core::{ExtSource, MirrorDirectory, TermSession};
use gorca_ext_api::terminal::{
    RawKey, TermAction, TermConfig, TermCounters, TermEdit, TermInput, TermSnapshot, TerminalHooks,
};
use gorca_ext_host::{ExtHost, Manifest};
use gorca_model::{AppId, AppInfo, ConnectorId, ConnectorKind};

const MANIFEST: &str = include_str!("../../../extensions/ext-vte/ext.toml");

fn info() -> AppInfo {
    AppInfo {
        toolkit: Arc::from("gtk3"),
        app_name: Arc::from("gnome-terminal-server"),
        pid: None,
        connector_kind: ConnectorKind::Atspi,
        verified_shell: false,
        dump_complete: false,
        text_unit: gorca_model::TextUnit::Scalar,
        text_insert_slack: 0,
        page_events_settle_ms: 0,
        web_engine: gorca_model::WebEngine::None,
        root: None,
    }
}

/// The native build called directly, as a `TermSession`.
struct Direct(ext_vte::VteExtension);

impl TermSession for Direct {
    fn step(&mut self, now: u64, input: TermInput, out: &mut Vec<TermAction>) {
        self.0.step(now, input, out);
    }
    fn counters(&self) -> TermCounters {
        self.0.counters()
    }
    fn snapshot(&self) -> Option<TermSnapshot> {
        self.0.snapshot()
    }
}

impl std::fmt::Debug for Direct {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Direct")
    }
}

fn session(label: &str, wasm: &[u8]) -> (Option<ExtHost>, Box<dyn TermSession>) {
    let cfg = TermConfig::default();
    if label == "direct" {
        return (None, Box::new(Direct(ext_vte::VteExtension::new(&cfg))));
    }
    let mut b = ExtHost::builder(Arc::new(MirrorDirectory::new()));
    if label == "wasm" {
        b.add_wasm(Manifest::parse(MANIFEST).unwrap(), wasm)
            .unwrap();
    } else {
        let m = Manifest::parse(&MANIFEST.replace("backend = \"wasm\"", "backend = \"native\""))
            .unwrap();
        b.add_native(m, Arc::new(|| Box::new(ext_vte::VteExtension::default())))
            .unwrap();
    }
    let h = b.build();
    let ae = h
        .attach(
            AppId {
                connector: ConnectorId(1),
                serial: 1,
            },
            &info(),
        )
        .expect("matches");
    let s = ae.terminal(&cfg).expect("a terminal policy");
    (Some(h), s)
}

fn key(keysym: u32, unichar: u32) -> TermInput {
    TermInput::Raw(RawKey {
        keysym,
        unichar,
        phys: 0,
        shift: false,
        ctrl: false,
        alt: false,
        logo: false,
    })
}

fn row(case: &str, mut v: Vec<u64>) {
    v.sort_unstable();
    let n = v.len();
    let at = |q: f64| v[((n as f64 * q) as usize).min(n - 1)];
    println!(
        "{case}\t{n}\t{}\t{}\t{}\t{}\t{:.0}",
        at(0.5),
        at(0.99),
        at(0.999),
        v[n - 1],
        v.iter().sum::<u64>() as f64 / n as f64
    );
}

fn ns(t: Instant) -> u64 {
    u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// A screen of `rows` x `cols` where line `shift` is the first: scrolling by one line changes
/// every row's position, like program output scrolling in.
fn screen(rows: usize, cols: usize, shift: usize, cyr: bool) -> String {
    let mut s = String::new();
    for r in 0..rows {
        let n = shift + r;
        let word = if cyr {
            "\u{43f}\u{440}\u{438}\u{432}\u{435}\u{442}"
        } else {
            "line"
        };
        let mut line = format!("{word} {n} ");
        while line.chars().count() < cols - 8 {
            line.push_str(&format!("{word}{} ", n % 7));
        }
        s.push_str(line.trim_end());
        s.push('\n');
    }
    s
}

fn baseline(s: &mut dyn TermSession, text: &str) {
    let mut out = Vec::new();
    s.step(0, TermInput::Focus, &mut out);
    s.step(
        1,
        TermInput::Screen {
            text: text.to_owned(),
            caret: Some(0),
        },
        &mut out,
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(100_000);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let wasm = std::fs::read(root.join("target/ext-wasm/wasm32-wasip2/release/ext_vte.wasm"))
        .expect("build the component first: cargo xtask build-ext");

    let only = args.get(2).cloned();
    for label in ["wasm", "native-host", "direct"] {
        if only.as_deref().is_some_and(|o| o != label) {
            continue;
        }
        let (h, mut s) = session(label, &wasm);
        let mut out: Vec<TermAction> = Vec::new();
        baseline(&mut *s, "me@box:~$ \n");
        let mut t: u64 = 100;

        // Keys: a typed key and BackSpace, alternating.
        let mut v = Vec::with_capacity(n);
        for i in 0..n + n / 10 {
            out.clear();
            t += 30;
            let k = if i % 2 == 0 {
                key('a' as u32, 'a' as u32)
            } else {
                key(0xff08, 8)
            };
            let t0 = Instant::now();
            s.step(t, k, &mut out);
            if i >= n / 10 {
                v.push(ns(t0));
            }
        }
        row(&format!("{label}-key_typed_backspace"), v);

        // The echo as payloads.
        let mut v = Vec::with_capacity(n);
        for i in 0..n + n / 10 {
            out.clear();
            t += 30;
            let e = if i % 2 == 0 {
                TermEdit::Insert {
                    offset: 10,
                    len: 1,
                    text: Some("a".to_owned()),
                }
            } else {
                TermEdit::Delete { offset: 10, len: 1 }
            };
            let t0 = Instant::now();
            s.step(t, TermInput::Edit(e), &mut out);
            if i >= n / 10 {
                v.push(ns(t0));
            }
        }
        row(&format!("{label}-edit_insert_delete"), v);

        let mut v = Vec::with_capacity(n);
        for i in 0..n + n / 10 {
            out.clear();
            t += 30;
            let t0 = Instant::now();
            s.step(t, TermInput::Caret(10 + (i % 3) as u32), &mut out);
            if i >= n / 10 {
                v.push(ns(t0));
            }
        }
        row(&format!("{label}-caret"), v);

        let mut v = Vec::with_capacity(n);
        for i in 0..n + n / 10 {
            out.clear();
            t += 30;
            let t0 = Instant::now();
            s.step(t, TermInput::Tick, &mut out);
            if i >= n / 10 {
                v.push(ns(t0));
            }
        }
        row(&format!("{label}-tick_idle"), v);

        // The screen reply of a settle, for three screen sizes.
        for (name, rows, cols, cyr, iters) in [
            ("screen_24x80", 24usize, 80usize, false, n / 10),
            ("screen_60x240", 60, 240, false, n / 50),
            ("screen_300x120_cyr", 300, 120, true, n / 500),
        ] {
            let iters = iters.max(50);
            let (h2, mut s2) = session(label, &wasm);
            baseline(&mut *s2, &screen(rows, cols, 0, cyr));
            let mut v = Vec::with_capacity(iters);
            let mut total = Vec::with_capacity(iters);
            let mut tt: u64 = 100;
            for i in 0..iters + 20 {
                let text = screen(rows, cols, i + 1, cyr);
                out.clear();
                let c0 = Instant::now();
                // The ingress collapsed an update; the settle passes; the extension asks for the
                // screen; the host answers it (the timed step).
                s2.step(tt, TermInput::Edit(TermEdit::Replaced), &mut out);
                tt += 31;
                s2.step(tt, TermInput::Tick, &mut out);
                tt += 2;
                let t0 = Instant::now();
                s2.step(
                    tt,
                    TermInput::Screen {
                        text,
                        caret: Some(0),
                    },
                    &mut out,
                );
                let d = ns(t0);
                let all = ns(c0);
                tt += 400;
                if i >= 20 {
                    v.push(d);
                    total.push(all);
                }
            }
            row(&format!("{label}-{name}"), v);
            row(&format!("{label}-{name}_cycle"), total);
            if let Some(h2) = h2 {
                let st = h2.stats();
                eprintln!(
                    "{label} {name}: faults {} busy_skips {} hook_calls {}",
                    st.faults, st.busy_skips, st.hook_calls
                );
            }
        }
        if let Some(h) = h {
            let st = h.stats();
            eprintln!(
                "{label}: faults {} busy_skips {} hook_calls {}",
                st.faults, st.busy_skips, st.hook_calls
            );
        }
    }
}
