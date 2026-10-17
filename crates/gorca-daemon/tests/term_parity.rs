//! Parity of the terminal policy across its three runtimes (ADR-0008 amendment 1: one source, two
//! builds): the daemon's native provider (`--terminal-policy=native`, the exclusive fallback), the
//! native build behind the extension host, and the `wasm32-wasip2` component under wasmtime.
//!
//! The drivers are the ones of `extensions/ext-vte/tests` (`support/replay.rs`, `support/fuzz.rs`):
//!
//! * a recorded real gnome-terminal 3.56 / VTE 0.80 session, raw and after the T-1 collapse;
//! * 40 random typing sessions of 400 keys against a model of a real VTE (echo 2 to 25 ms behind
//!   its key, either order).
//!
//! Every action the extension answers, with its time, and its snapshot after every input must be
//! equal in the three runtimes, and so must the counters. No host fault, no skipped call.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::if_not_else,
    clippy::match_same_arms,
    dead_code
)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use ext_vte::VteExtension;
use gorca_core::{AppExt, ExtSource, MirrorDirectory, TermProvider, TermSession};
use gorca_daemon::NativeTerminalPolicy;
use gorca_ext_api::terminal::{
    Ms, RawKey, TermAction, TermConfig, TermCounters, TermInput, TermSnapshot, TerminalHooks,
};
use gorca_ext_host::{ExtHost, Manifest};
use gorca_model::{AppId, AppInfo, ConnectorId, ConnectorKind, TextUnit};

#[path = "../../../extensions/ext-vte/tests/support/fuzz.rs"]
mod fuzz;
#[path = "../../../extensions/ext-vte/tests/support/replay.rs"]
mod replay;

const MANIFEST: &str = include_str!("../../../extensions/ext-vte/ext.toml");

fn wasm_bytes() -> Vec<u8> {
    static BYTES: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    BYTES.get_or_init(build_wasm).clone()
}

fn build_wasm() -> Vec<u8> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let target = root.join("target/ext-wasm");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let status = Command::new(cargo)
        .current_dir(&root)
        .args([
            "build",
            "--quiet",
            "-p",
            "ext-vte",
            "--target",
            "wasm32-wasip2",
            "--release",
            "--target-dir",
        ])
        .arg(&target)
        // The same flags as `cargo xtask build-ext`, so the two share one build.
        .env("CARGO_PROFILE_RELEASE_DEBUG", "false")
        .env("CARGO_PROFILE_RELEASE_STRIP", "true")
        .status()
        .expect("run cargo");
    assert!(status.success(), "building ext-vte as a component");
    std::fs::read(target.join("wasm32-wasip2/release/ext_vte.wasm")).expect("component")
}

fn term_info() -> AppInfo {
    AppInfo {
        toolkit: Arc::from("gtk3"),
        app_name: Arc::from("gnome-terminal-server"),
        pid: None,
        connector_kind: ConnectorKind::Atspi,
        verified_shell: false,
        dump_complete: false,
        text_unit: TextUnit::Scalar,
        text_insert_slack: 0,
        edit_quirks: gorca_model::EditQuirks::default(),
        root: None,
    }
}

/// A `TermSession` as the `TerminalHooks` the drivers speak.
struct Hooks(Box<dyn TermSession>);

impl TerminalHooks for Hooks {
    fn configure(&mut self, _cfg: &TermConfig) {}
    fn step(&mut self, now: Ms, input: TermInput, out: &mut Vec<TermAction>) {
        self.0.step(now, input, out);
    }
    fn counters(&self) -> TermCounters {
        self.0.counters()
    }
    fn snapshot(&self) -> Option<TermSnapshot> {
        self.0.snapshot()
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Runtime {
    /// The daemon's provider (native fallback, no extension host).
    Provider,
    /// The native build as a slot of the extension host.
    HostNative,
    /// The component.
    Wasm,
}

const RUNTIMES: [Runtime; 3] = [Runtime::Provider, Runtime::HostNative, Runtime::Wasm];

struct Rt {
    // Kept alive: the sessions hold slots of this host.
    host: Option<ExtHost>,
    app: Option<Arc<dyn AppExt>>,
    kind: Runtime,
}

impl Rt {
    fn new(kind: Runtime) -> Self {
        if kind == Runtime::Provider {
            return Self {
                host: None,
                app: None,
                kind,
            };
        }
        let dir = Arc::new(MirrorDirectory::new());
        let mut b = ExtHost::builder(dir);
        if kind == Runtime::HostNative {
            let m =
                Manifest::parse(&MANIFEST.replace("backend = \"wasm\"", "backend = \"native\""))
                    .expect("manifest");
            b.add_native(m, Arc::new(|| Box::new(VteExtension::default())))
                .expect("native");
        } else {
            let m = Manifest::parse(MANIFEST).expect("manifest");
            b.add_wasm(m, &wasm_bytes()).expect("component");
        }
        let host = b.build();
        let app = host
            .attach(
                AppId {
                    connector: ConnectorId(1),
                    serial: 1,
                },
                &term_info(),
            )
            .expect("a gnome-terminal matches ext-vte");
        Self {
            host: Some(host),
            app: Some(app),
            kind,
        }
    }

    fn session(&self) -> Box<dyn TermSession> {
        let cfg = TermConfig::default();
        match self.kind {
            Runtime::Provider => NativeTerminalPolicy::default().open(&cfg),
            Runtime::HostNative | Runtime::Wasm => self
                .app
                .as_ref()
                .and_then(|a| a.terminal(&cfg))
                .expect("a terminal session"),
        }
    }

    fn make(&self) -> Box<dyn TerminalHooks> {
        Box::new(Hooks(self.session()))
    }

    /// The host saw no fault and skipped no call.
    fn assert_clean(&self) {
        if let Some(h) = &self.host {
            let st = h.stats();
            assert_eq!(st.faults, 0, "{:?}: {st:?}", self.kind);
            assert_eq!(st.busy_skips, 0, "{:?}: {st:?}", self.kind);
            assert_eq!(st.resets, 0, "{:?}: {st:?}", self.kind);
        }
    }
}

#[test]
fn the_manifest_matches_terminals_and_nothing_else() {
    let _ = gorca_daemon::init_tracing();
    let dir = Arc::new(MirrorDirectory::new());
    let mut b = ExtHost::builder(dir);
    b.add_wasm(Manifest::parse(MANIFEST).expect("manifest"), &wasm_bytes())
        .expect("component");
    let host = b.build();
    let mut other = term_info();
    other.app_name = Arc::from("firefox");
    assert!(
        host.attach(
            AppId {
                connector: ConnectorId(1),
                serial: 2
            },
            &other
        )
        .is_none()
    );
    let mut names = Vec::new();
    for n in [
        "gnome-terminal-server",
        "ptyxis",
        "kgx",
        "tilix",
        "xfce4-terminal",
    ] {
        let mut i = term_info();
        i.app_name = Arc::from(n);
        let id = AppId {
            connector: ConnectorId(1),
            serial: 10 + names.len() as u32,
        };
        names.push(host.attach(id, &i).is_some());
    }
    assert_eq!(names, [true; 5]);
    // The component exports no other hook than the terminal.
    let ae = host
        .attach(
            AppId {
                connector: ConnectorId(1),
                serial: 3,
            },
            &term_info(),
        )
        .unwrap();
    assert!(ae.terminal(&TermConfig::default()).is_some());
}

#[test]
fn the_recorded_real_session_is_identical_in_all_three_runtimes() {
    for collapse in [false, true] {
        let mut runs = Vec::new();
        for k in RUNTIMES {
            let rt = Rt::new(k);
            let r = replay::run(replay::SESSION, collapse, &|| rt.make());
            rt.assert_clean();
            runs.push((k, r));
        }
        let (_, base) = &runs[0];
        assert!(!base.actions.is_empty() && !base.snaps.is_empty());
        for (k, r) in &runs[1..] {
            assert_eq!(base.said, r.said, "{k:?} collapse {collapse}: speech");
            assert_eq!(
                base.actions, r.actions,
                "{k:?} collapse {collapse}: actions"
            );
            assert_eq!(base.snaps, r.snaps, "{k:?} collapse {collapse}: snapshots");
            assert_eq!(base.ext.counters(), r.ext.counters(), "{k:?}: counters");
        }
    }
}

#[test]
fn forty_random_sessions_are_identical_in_all_three_runtimes() {
    // A native slot is timed after the fact against the CPU limit, and an unoptimised build
    // overruns it: the native host runs in release builds only (the provider and the component
    // cover the debug build).
    let kinds: Vec<Runtime> = RUNTIMES
        .iter()
        .copied()
        .filter(|k| !(*k == Runtime::HostNative && cfg!(debug_assertions)))
        .collect();
    let rts: Vec<Rt> = kinds.iter().map(|k| Rt::new(*k)).collect();
    for seed in 1..=40u64 {
        let outs: Vec<fuzz::Outcome> = rts
            .iter()
            .map(|rt| fuzz::run(seed * 7919, 400, &|| rt.make()))
            .collect();
        for (i, o) in outs.iter().enumerate() {
            assert!(
                o.problems.is_empty(),
                "seed {seed} {:?}: {:?}",
                kinds[i],
                o.problems
            );
        }
        for (i, o) in outs.iter().enumerate().skip(1) {
            assert_eq!(outs[0].said, o.said, "seed {seed} {:?}: speech", kinds[i]);
            assert_eq!(
                outs[0].actions, o.actions,
                "seed {seed} {:?}: actions",
                kinds[i]
            );
            assert_eq!(
                rel(&outs[0].snaps),
                rel(&o.snaps),
                "seed {seed} {:?}: snapshots",
                kinds[i]
            );
        }
    }
    for rt in &rts {
        rt.assert_clean();
    }
}

/// Snapshots with the generation counted from the first one: a long-lived instance keeps counting
/// across sessions, a fresh one starts over, and only changes of it mean anything.
fn rel(v: &[Option<TermSnapshot>]) -> Vec<Option<TermSnapshot>> {
    let base = v.iter().flatten().map(|s| s.generation).next().unwrap_or(0);
    v.iter()
        .map(|s| {
            s.clone().map(|mut s| {
                s.generation = s.generation.wrapping_sub(base);
                s
            })
        })
        .collect()
}

fn raw(keysym: u32, unichar: u32, phys: char, ctrl: bool) -> TermInput {
    TermInput::Raw(RawKey {
        keysym,
        unichar,
        phys: if phys == '-' { 0 } else { phys as u8 },
        shift: false,
        ctrl,
        alt: false,
        logo: false,
    })
}

/// Drive one session by hand and collect everything it answered.
fn drive(rt: &Rt, script: &[(Ms, TermInput)]) -> Vec<(Ms, TermAction)> {
    let mut s = rt.session();
    let mut all = Vec::new();
    for (t, i) in script {
        let mut out = Vec::new();
        s.step(*t, i.clone(), &mut out);
        all.extend(out.into_iter().map(|a| (*t, a)));
    }
    all
}

#[test]
fn raw_keys_and_a_password_prompt_behave_the_same_and_leak_nothing() {
    const SECRET: &str = "Zq9#vX7";
    let prompt = "[sudo] password for me: ";
    let n = prompt.chars().count() as u32;
    let mut script: Vec<(Ms, TermInput)> = vec![
        (0, TermInput::Focus),
        (
            2,
            TermInput::Screen {
                text: prompt.to_owned(),
                caret: Some(n),
            },
        ),
        (40, TermInput::Tick),
    ];
    let mut t = 100;
    for c in SECRET.chars() {
        script.push((t, raw(c as u32, c as u32, '-', false)));
        t += 25;
    }
    script.push((t, raw(0xff0d, 13, '-', false)));
    script.push((t + 10, raw('c' as u32, 3, 'c', true)));
    let mut got = Vec::new();
    for k in RUNTIMES {
        let rt = Rt::new(k);
        let a = drive(&rt, &script);
        rt.assert_clean();
        // Nothing the extension answered, nor any debug text of it, contains the secret.
        let shown = format!("{a:?} {script:?}");
        for c in SECRET
            .chars()
            .filter(|c| !c.is_alphanumeric() || c.is_uppercase())
        {
            assert!(!shown.contains(c), "{k:?}: {c:?} leaked in debug output");
        }
        got.push(a);
    }
    assert_eq!(got[0], got[1]);
    assert_eq!(got[0], got[2]);
    // The prompt is spoken, as a possible secret, and the typed characters are not echoed aloud.
    let says: Vec<_> = got[0]
        .iter()
        .filter_map(|(_, a)| match a {
            TermAction::Say {
                text, sensitive, ..
            } => Some((text.clone(), *sensitive)),
            _ => None,
        })
        .collect();
    assert!(says.iter().any(|(t, _)| t.contains("password")), "{says:?}");
    assert!(
        says.iter().all(|(t, _)| !t.contains(SECRET)),
        "the secret was spoken as a whole"
    );
}

#[test]
fn a_big_screen_goes_through_the_component_whole() {
    let _ = gorca_daemon::init_tracing();
    // 120 x 300 of Cyrillic (72 KB, above the 64 KiB a plain hook result may lift) and a screen
    // beyond the 256 KiB cap: same snapshot and no fault in every runtime.
    let row: String = "\u{43f}\u{440}\u{438}\u{432}\u{435}\u{442} ".repeat(20);
    let line: String = row.chars().take(120).collect();
    let rows120: String = std::iter::repeat_n(format!("{line}\n"), 300).collect();
    let huge: String = "x".repeat(299) + "\n";
    let huge = huge.repeat(1100);
    for text in [rows120, huge] {
        let n = text.chars().count() as u32;
        let script = vec![
            (0, TermInput::Focus),
            (
                2,
                TermInput::Screen {
                    text: text.clone(),
                    caret: Some(n.saturating_sub(5)),
                },
            ),
            (50, TermInput::Tick),
            (60, TermInput::Caret(n.saturating_sub(10))),
        ];
        let mut snaps: Vec<Option<TermSnapshot>> = Vec::new();
        let mut acts: Vec<Vec<TermAction>> = Vec::new();
        for k in RUNTIMES {
            // A native slot is timed after the fact against the 8 ms limit, and this build is
            // unoptimised: the native host is only checked where it is built with optimisations.
            if k == Runtime::HostNative && cfg!(debug_assertions) {
                snaps.push(snaps[0].clone());
                acts.push(acts[0].clone());
                continue;
            }
            let rt = Rt::new(k);
            let mut s = rt.session();
            let mut out = Vec::new();
            for (t, i) in &script {
                s.step(*t, i.clone(), &mut out);
            }
            rt.assert_clean();
            snaps.push(s.snapshot());
            acts.push(out);
        }
        assert!(snaps[0].is_some());
        assert_eq!(snaps[0], snaps[1]);
        assert_eq!(snaps[0], snaps[2]);
        assert_eq!(acts[0], acts[1]);
        assert_eq!(acts[0], acts[2]);
    }
}
