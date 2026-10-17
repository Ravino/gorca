// SPDX-License-Identifier: GPL-3.0-or-later
//! X1 for the GNOME Shell policy (`ext-gnome-shell`): the whole host path of the `policy` hook as
//! the Reader calls it (`AppExt::policy`: fact conversion, slot `try_lock`, the call, lifting and
//! clamping the verdict), on both builds of the one source, against the same rows through the
//! daemon's native adapter (no host at all) for reference.
//!
//! ```sh
//! cargo xtask build-ext
//! cargo run --release -p gorca-ext-host --example x1_shell -- [iterations] > out.tsv
//! ```
//!
//! Output (TSV, nanoseconds): `case n p50 p99 p999 max mean`. Gate: p99 <= 100 us (X1).
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

use gorca_core::{AppExt, ExtSource, MirrorDirectory, PolicyFacts, PolicyMoment, PolicyVerdict};
use gorca_ext_host::{ExtHost, Manifest};
use gorca_model::{AppId, AppInfo, ConnectorId, ConnectorKind, Role, StateSet};

const MANIFEST: &str = include_str!("../../../extensions/ext-gnome-shell/ext.toml");

fn info() -> AppInfo {
    AppInfo {
        toolkit: Arc::from("clutter"),
        app_name: Arc::from("gnome-shell"),
        pid: None,
        connector_kind: ConnectorKind::Atspi,
        verified_shell: true,
        dump_complete: false,
        text_unit: gorca_model::TextUnit::Scalar,
        text_insert_slack: 0,
        page_events_settle_ms: 0,
        web_engine: gorca_model::WebEngine::None,
        root: None,
    }
}

fn host(wasm: Option<&[u8]>) -> (ExtHost, Arc<dyn AppExt>) {
    let mut b = ExtHost::builder(Arc::new(MirrorDirectory::new()));
    match wasm {
        Some(bytes) => b
            .add_wasm(Manifest::parse(MANIFEST).unwrap(), bytes)
            .unwrap(),
        None => {
            let m =
                Manifest::parse(&MANIFEST.replace("backend = \"wasm\"", "backend = \"native\""))
                    .unwrap();
            b.add_native(m, Arc::new(|| Box::new(ext_gnome_shell::ShellExtension)))
                .unwrap();
        }
    }
    let h = b.build();
    let app = AppId {
        connector: ConnectorId(1),
        serial: 1,
    };
    let ae = h.attach(app, &info()).expect("the Shell matches");
    (h, ae)
}

fn time(n: usize, mut f: impl FnMut()) -> Vec<u64> {
    for _ in 0..n / 10 {
        f();
    }
    (0..n)
        .map(|_| {
            let t = Instant::now();
            f();
            u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX)
        })
        .collect()
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

fn facts(
    moment: PolicyMoment,
    role: Role,
    name: Option<&'static str>,
    range: Option<(f64, f64, f64)>,
) -> PolicyFacts<'static> {
    PolicyFacts {
        moment,
        role,
        states: StateSet::FOCUSED,
        name,
        description: None,
        range,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(200_000);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let wasm =
        std::fs::read(root.join("target/ext-wasm/wasm32-wasip2/release/ext_gnome_shell.wasm"))
            .expect("build the component first: cargo xtask build-ext");

    let cases: [(&str, PolicyFacts<'static>); 6] = [
        (
            "slider_focus_value",
            facts(
                PolicyMoment::Focus,
                Role::Slider,
                Some("Volume"),
                Some((0.5, 0.0, 1.0)),
            ),
        ),
        (
            "window_focus_silence",
            facts(PolicyMoment::Focus, Role::Window, Some("Main"), None),
        ),
        (
            "stage_activate_silence",
            facts(
                PolicyMoment::Activate,
                Role::Window,
                Some("Main stage"),
                None,
            ),
        ),
        (
            "dialog_showing_notice",
            facts(PolicyMoment::Showing, Role::Dialog, None, None),
        ),
        (
            "osd_level_say",
            facts(
                PolicyMoment::Value,
                Role::ProgressBar,
                Some("Volume"),
                Some((0.25, 0.0, 1.0)),
            ),
        ),
        (
            "button_focus_defer",
            facts(PolicyMoment::Focus, Role::Button, Some("OK"), None),
        ),
    ];
    for (label, bytes) in [("wasm", Some(&wasm[..])), ("native", None)] {
        let (_h, ae) = host(bytes);
        for (name, f) in &cases {
            row(
                &format!("{label}-policy_{name}"),
                time(n, || {
                    std::hint::black_box(ae.policy(std::hint::black_box(f)));
                }),
            );
        }
        let st = _h.stats();
        eprintln!(
            "{label}: faults {} busy_skips {} hook_calls {}",
            st.faults, st.busy_skips, st.hook_calls
        );
    }
    let _ = PolicyVerdict::Default;
}
