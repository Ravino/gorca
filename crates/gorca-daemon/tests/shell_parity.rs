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

#[test]
fn the_native_adapter_the_native_extension_and_the_component_agree_on_every_fact() {
    let native = NativeShellPolicy::default();
    let (_h1, ext_native) = host_with(true);
    let (_h2, ext_wasm) = host_with(false);
    let mut n = 0u64;
    let mut non_default = 0u64;
    for f in grid() {
        let want = native.decide(&f);
        if want != PolicyVerdict::Default {
            non_default += 1;
        }
        assert_eq!(in_host(&ext_native, &f), want, "native extension, {f:?}");
        assert_eq!(in_host(&ext_wasm, &f), want, "component, {f:?}");
        n += 1;
    }
    // The grid is dense and exercises every verdict, not only `Default`.
    assert!(n > 50_000, "{n}");
    assert!(non_default > 1_000, "{non_default}");
}

#[test]
fn the_component_never_faults_on_the_grid() {
    let (h, ae) = host_with(false);
    for f in grid().take(5_000) {
        let _ = in_host(&ae, &f);
    }
    let st = h.stats();
    assert_eq!(st.faults, 0, "{st:?}");
    assert_eq!(st.busy_skips, 0, "{st:?}");
}

#[test]
fn the_dialog_osd_banner_and_unnamed_slider_rules_speak_the_same_in_the_component() {
    let (_h, ae) = host_with(false);
    let f = |moment, role, name, range| PolicyFacts {
        moment,
        role,
        states: StateSet::empty(),
        name,
        description: None,
        range,
    };
    // Dialog that appears: read as a notice.
    assert_eq!(
        in_host(&ae, &f(PolicyMoment::Showing, Role::Dialog, None, None)),
        PolicyVerdict::Notice
    );
    // Window focus claim and the "Main stage" activation: silent.
    assert_eq!(
        in_host(&ae, &f(PolicyMoment::Focus, Role::Window, None, None)),
        PolicyVerdict::Silence
    );
    assert_eq!(
        in_host(
            &ae,
            &f(
                PolicyMoment::Activate,
                Role::Window,
                Some("Main stage"),
                None
            )
        ),
        PolicyVerdict::Silence
    );
    // The unit slider, named or not, is a percentage value.
    assert_eq!(
        in_host(
            &ae,
            &f(
                PolicyMoment::Focus,
                Role::Slider,
                None,
                Some((0.5, 0.0, 1.0))
            )
        ),
        PolicyVerdict::Value("50 percent".to_owned())
    );
    // The OSD level: name and percent.
    let PolicyVerdict::Say(segs) = in_host(
        &ae,
        &f(
            PolicyMoment::Value,
            Role::ProgressBar,
            Some("Volume"),
            Some((0.25, 0.0, 1.0)),
        ),
    ) else {
        panic!("the OSD level is said");
    };
    let text: Vec<&str> = segs.iter().map(|s| &*s.text).collect();
    assert_eq!(text, ["Volume", "25 percent"]);
    // A role the Shell has no rule for is the core's.
    assert_eq!(
        in_host(&ae, &f(PolicyMoment::Focus, Role::Button, Some("OK"), None)),
        PolicyVerdict::Default
    );
}

#[test]
fn a_window_that_is_not_the_verified_shell_gets_no_policy() {
    let dir = Arc::new(MirrorDirectory::new());
    let mut b = ExtHost::builder(dir);
    b.add_wasm(Manifest::parse(MANIFEST).expect("manifest"), &wasm_bytes())
        .expect("component");
    let host = b.build();
    let mut info = shell_info();
    info.verified_shell = false;
    let app = AppId {
        connector: ConnectorId(1),
        serial: 2,
    };
    assert!(
        host.attach(app, &info).is_none(),
        "a self-reported name grants nothing"
    );
}
