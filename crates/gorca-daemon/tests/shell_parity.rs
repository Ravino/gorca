// SPDX-License-Identifier: GPL-3.0-or-later
//! Parity of the GNOME Shell policy across its three runtimes (ADR-0008 amendment 1: one source,
//! two builds): the daemon's native adapter (`AppPolicy`), the native build behind the extension
//! host, and the `wasm32-wasip2` component under wasmtime. Every combination of moment, role,
//! name, description, state bits and value range in a dense grid must give the same verdict.
//! The unit tests of `ext-gnome-shell` pin what the verdicts are; this pins that no runtime
//! changes them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods
)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use gorca_core::{
    AppExt, AppPolicy, ExtSource, MirrorDirectory, PolicyFacts, PolicyMoment, PolicyVerdict,
};
use gorca_daemon::NativeShellPolicy;
use gorca_ext_host::{ExtHost, Manifest};
use gorca_model::{AppId, AppInfo, ConnectorId, ConnectorKind, Role, StateSet, TextUnit};

const MANIFEST: &str = include_str!("../../../extensions/ext-gnome-shell/ext.toml");

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
            "ext-gnome-shell",
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
    assert!(status.success(), "building ext-gnome-shell as a component");
    std::fs::read(target.join("wasm32-wasip2/release/ext_gnome_shell.wasm")).expect("component")
}

fn shell_info() -> AppInfo {
    AppInfo {
        toolkit: Arc::from("clutter"),
        app_name: Arc::from("gnome-shell"),
        pid: None,
        connector_kind: ConnectorKind::Atspi,
        verified_shell: true,
        dump_complete: false,
        text_unit: TextUnit::Scalar,
        text_insert_slack: 0,
        root: None,
    }
}

fn host_with(native: bool) -> (ExtHost, Arc<dyn AppExt>) {
    let dir = Arc::new(MirrorDirectory::new());
    let mut b = ExtHost::builder(dir);
    if native {
        let m = Manifest::parse(&MANIFEST.replace("backend = \"wasm\"", "backend = \"native\""))
            .expect("manifest");
        b.add_native(m, Arc::new(|| Box::new(ext_gnome_shell::ShellExtension)))
            .expect("native");
    } else {
        let m = Manifest::parse(MANIFEST).expect("manifest");
        b.add_wasm(m, &wasm_bytes()).expect("component");
    }
    let host = b.build();
    let app = AppId {
        connector: ConnectorId(1),
        serial: 1,
    };
    let ae = host.attach(app, &shell_info()).expect("the Shell matches");
    (host, ae)
}

const ROLES: [Role; 44] = [
    Role::Unknown,
    Role::Application,
    Role::Window,
    Role::Dialog,
    Role::Panel,
    Role::Document,
    Role::DocumentWeb,
    Role::Heading,
    Role::Paragraph,
    Role::Link,
    Role::Image,
    Role::List,
    Role::ListItem,
    Role::Table,
    Role::Row,
    Role::Cell,
    Role::ColumnHeader,
    Role::RowHeader,
    Role::Button,
    Role::ToggleButton,
    Role::CheckBox,
    Role::RadioButton,
    Role::ComboBox,
    Role::TextEntry,
    Role::PasswordEntry,
    Role::SpinButton,
    Role::Slider,
    Role::ProgressBar,
    Role::Menu,
    Role::MenuBar,
    Role::MenuItem,
    Role::Popover,
    Role::Tab,
    Role::TabList,
    Role::Tree,
    Role::TreeItem,
    Role::Toolbar,
    Role::StatusBar,
    Role::Separator,
    Role::Label,
    Role::StaticText,
    Role::Terminal,
    Role::Landmark,
    Role::Alert,
];

const MOMENTS: [PolicyMoment; 4] = [
    PolicyMoment::Focus,
    PolicyMoment::Value,
    PolicyMoment::Activate,
    PolicyMoment::Showing,
];

const NAMES: [Option<&str>; 6] = [
    None,
    Some(""),
    Some("  "),
    Some("Volume"),
    Some("Lautst\u{e4}rke \u{1f50a}"),
    Some("Brightness"),
];

const RANGES: [Option<(f64, f64, f64)>; 13] = [
    None,
    Some((0.5, 0.0, 1.0)),
    Some((0.0, 0.0, 1.0)),
    Some((1.0, 0.0, 1.0)),
    Some((0.334, 0.0, 1.0)),
    Some((1.7, 0.0, 1.0)),
    Some((-0.2, 0.0, 1.0)),
    Some((50.0, 0.0, 100.0)),
    Some((0.5, 0.0, 1.000_000_000_1)),
    Some((0.5, 0.1, 1.0)),
    Some((0.5, 0.0, 0.0)),
    Some((f64::NAN, 0.0, 1.0)),
    Some((f64::INFINITY, 0.0, 1.0)),
];

const STATES: [StateSet; 3] = [
    StateSet::empty(),
    StateSet::FOCUSED.union(StateSet::SHOWING),
    StateSet::all(),
];

fn grid() -> impl Iterator<Item = PolicyFacts<'static>> {
    MOMENTS.into_iter().flat_map(|moment| {
        ROLES.into_iter().flat_map(move |role| {
            STATES.into_iter().flat_map(move |states| {
                NAMES.into_iter().flat_map(move |name| {
                    [None, Some("a description")]
                        .into_iter()
                        .flat_map(move |description| {
                            RANGES.into_iter().map(move |range| PolicyFacts {
                                moment,
                                role,
                                states,
                                name,
                                description,
                                range,
                            })
                        })
                })
            })
        })
    })
}

fn in_host(ae: &Arc<dyn AppExt>, f: &PolicyFacts<'_>) -> PolicyVerdict {
    // `None` is "no extension has an opinion": the core presents the node as usual.
    ae.policy(f).unwrap_or(PolicyVerdict::Default)
}

/// A host-backed runtime (native extension or component), driven so that a fault caused by the *host* cannot be mistaken for
/// a wrong verdict. A `policy` call runs under a wall-clock backstop of 6 ms (three times its
/// 2 ms CPU limit, wasm.rs); on a loaded host a thread descheduled inside a call is interrupted,
/// the slot answers "no opinion" and rebuilds, and three such strikes disable the extension.
/// That is the designed containment, not a parity bug, but it makes one call's verdict
/// meaningless. So a call during which the host counted a fault is retried (after the rebuild,
/// on a fresh host when the slot got disabled). A fact that faults on every attempt is a real
/// defect and fails; so does any verdict that differs from the expected one without a fault.
struct Component {
    native: bool,
    host: ExtHost,
    ae: Arc<dyn AppExt>,
    /// Calls the host interrupted and that were repeated (printed, never hidden).
    retried: u64,
}

impl Component {
    const ATTEMPTS: u32 = 10;

    fn new(native: bool) -> Self {
        let (host, ae) = host_with(native);
        Self {
            native,
            host,
            ae,
            retried: 0,
        }
    }

    fn verdict(&mut self, f: &PolicyFacts<'_>) -> PolicyVerdict {
        for _ in 0..Self::ATTEMPTS {
            while !self.host.settled() {
                std::thread::yield_now();
            }
            let before = self.host.stats();
            let v = in_host(&self.ae, f);
            let after = self.host.stats();
            if after.faults == before.faults && after.busy_skips == before.busy_skips {
                return v;
            }
            self.retried += 1;
            if after.disabled != 0 {
                let retried = self.retried;
                *self = Self::new(self.native);
                self.retried = retried;
            }
        }
        panic!("the host faults on every attempt for {f:?}");
    }
}

#[test]
fn the_native_adapter_the_native_extension_and_the_component_agree_on_every_fact() {
    let native = NativeShellPolicy::default();
    let mut ext_native = Component::new(true);
    let mut wasm = Component::new(false);
    let mut n = 0u64;
    let mut non_default = 0u64;
    for f in grid() {
        let want = native.decide(&f);
        if want != PolicyVerdict::Default {
            non_default += 1;
        }
        assert_eq!(ext_native.verdict(&f), want, "native extension, {f:?}");
        assert_eq!(wasm.verdict(&f), want, "component, {f:?}");
        n += 1;
    }
    eprintln!(
        "parity: {n} facts, {} interrupted calls retried",
        wasm.retried
    );
    // The grid is dense and exercises every verdict, not only `Default`.
    assert!(n > 50_000, "{n}");
    assert!(non_default > 1_000, "{non_default}");
}

#[test]
fn the_component_never_faults_on_the_grid() {
    // No input may fault the component by itself: every fact must get an uninterrupted answer
    // within `Component::ATTEMPTS` tries (`verdict` panics otherwise). Interruptions by the
    // loaded host are retried and reported.
    let mut wasm = Component::new(false);
    for f in grid().take(5_000) {
        let _ = wasm.verdict(&f);
    }
    eprintln!("grid: {} interrupted calls retried", wasm.retried);
}
