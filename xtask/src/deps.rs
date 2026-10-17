// SPDX-License-Identifier: GPL-3.0-or-later
//! Dependency-direction rules from ADR-0001 section 11, evaluated on the resolved dependency
//! graph (production edges only: normal and build dependencies; dev-dependencies are test-only
//! and may use anything).
//!
//! 1. Neutral crates must not depend, even transitively, on `atspi*`, `zbus*`, `accesskit*`,
//!    `wayland-*`, `wasmtime*` or `reis`/libei bindings.
//! 2. Only `gorca-dbus`, `gorca-connector-atspi`, `gorca-doc-source-atspi`, `gorca-input-mutter`,
//!    `gorca-input-inject`, `gorca-daemon` and the B-6 harness `gorca-bench-web` may depend on `zbus`. Only `gorca-connector-atspi` may depend on `atspi*`.
//! 3. `gorca-core` must not depend on any `gorca-connector-*`, `gorca-speech-*`, `gorca-input-*`
//!    (except `-api`) or `gorca-ext-host`. Only `gorca-daemon` wires concrete implementations.
//! 4. `gorca-model`, `gorca-doc` and `gorca-text` must not depend on `tokio`.
//! 5. Extensions depend only on `gorca-ext-api` (which re-exports the pure leaf `gorca-i18n`
//!    for message references, ADR-0010).
//! 9. The WASM runtime is confined (M5): only `gorca-ext-host` may depend on `wasmtime*`, only
//!    `gorca-ext-api` (for the `wasm32` component build) on `wit-bindgen*`, and the host must not
//!    reach `zbus*` or `atspi*` (an extension sees the core's types, never a connector's).
//!
//! Rules 6 and 7 (lints, release profile) are enforced by `Cargo.toml` and clippy.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use anyhow::{Context, Result};
use cargo_metadata::{DependencyKind, MetadataCommand};

/// Crates that must stay transport-neutral (rule 1).
const NEUTRAL: &[&str] = &[
    "gorca-model",
    "gorca-ingress",
    "gorca-mirror",
    "gorca-connector-api",
    "gorca-doc-source-api",
    "gorca-doc",
    "gorca-text",
    "gorca-output-api",
    "gorca-input-api",
    "gorca-core",
    "gorca-speech",
    "gorca-config",
    // The extension API is what a WASM extension links: no transport, no runtime.
    "gorca-ext-api",
];
const MAY_USE_ZBUS: &[&str] = &[
    "gorca-dbus",
    "gorca-connector-atspi",
    "gorca-input-mutter",
    "gorca-input-inject",
    "gorca-daemon",
    // Web document source over P2P (ADR-0003 section 6.2). It talks raw D-Bus to the browser's
    // own P2P socket and never links the `atspi` crate.
    "gorca-doc-source-atspi",
    // B-6 harness: hosts the AT-SPI arm and a bus listener, test-only.
    "gorca-bench-web",
    // B-1 harness fake application (tests/harness/b1/fakeapp): serves the AT-SPI interfaces.
    "b1-fakeapp",
];
const MAY_USE_ATSPI: &[&str] = &["gorca-connector-atspi"];
const TOKIO_FREE: &[&str] = &["gorca-model", "gorca-doc", "gorca-text", "gorca-config"];
/// Concrete implementations that only `gorca-daemon` wires (rule 3, second sentence).
const CONCRETE: &[&str] = &[
    "gorca-connector-atspi",
    "gorca-speech-ssip",
    "gorca-speech-voicego",
    "gorca-input-mutter",
    "gorca-input-inject",
    "gorca-input-xkb",
    "gorca-braille-brlapi",
    "gorca-ext-host",
];

/// One package of the resolved graph.
#[derive(Debug, Clone)]
pub struct Pkg {
    pub name: String,
    pub in_workspace: bool,
    /// Lives under `extensions/`.
    pub is_extension: bool,
    /// Production dependencies (normal or build), by package name.
    pub deps: BTreeSet<String>,
}

#[derive(Debug, Default)]
pub struct Graph {
    pub pkgs: BTreeMap<String, Pkg>,
}

#[derive(Debug, Eq, PartialEq)]
pub struct Violation {
    pub rule: u8,
    pub message: String,
}

impl Graph {
    /// Shortest dependency chain `from -> ... -> to` where the last element satisfies `hit`.
    fn path_to(&self, from: &str, hit: impl Fn(&str) -> bool) -> Option<Vec<String>> {
        let mut prev: BTreeMap<&str, &str> = BTreeMap::new();
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut q: VecDeque<&str> = VecDeque::new();
        let start = self.pkgs.get_key_value(from)?.0.as_str();
        seen.insert(start);
        q.push_back(start);
        while let Some(cur) = q.pop_front() {
            let Some(p) = self.pkgs.get(cur) else {
                continue;
            };
            for d in &p.deps {
                let d = d.as_str();
                if !seen.insert(d) {
                    continue;
                }
                prev.insert(d, cur);
                if hit(d) {
                    let mut chain = vec![d.to_owned()];
                    let mut at = d;
                    while let Some(&p) = prev.get(at) {
                        chain.push(p.to_owned());
                        at = p;
                    }
                    chain.reverse();
                    return Some(chain);
                }
                q.push_back(d);
            }
        }
        None
    }

    fn depends_directly(&self, from: &str, hit: impl Fn(&str) -> bool) -> Option<String> {
        self.pkgs.get(from)?.deps.iter().find(|d| hit(d)).cloned()
    }
}

fn is_transport(name: &str) -> bool {
    name == "atspi"
        || name.starts_with("atspi-")
        || name.starts_with("atspi_")
        || name == "zbus"
        || name.starts_with("zbus-")
        || name.starts_with("zbus_")
        || name == "accesskit"
        || name.starts_with("accesskit-")
        || name.starts_with("accesskit_")
        || name.starts_with("wayland-")
        || name == "wasmtime"
        || name.starts_with("wasmtime-")
        || name.starts_with("wasmtime_")
        || name == "reis"
        || name.starts_with("libei")
}

fn is_zbus(name: &str) -> bool {
    name == "zbus" || name.starts_with("zbus-") || name.starts_with("zbus_")
}

fn is_wasmtime(name: &str) -> bool {
    name == "wasmtime" || name.starts_with("wasmtime-")
}

fn is_wit_bindgen(name: &str) -> bool {
    name == "wit-bindgen" || name.starts_with("wit-bindgen-")
}

fn is_atspi(name: &str) -> bool {
    name == "atspi" || name.starts_with("atspi-") || name.starts_with("atspi_")
}

fn core_forbidden(name: &str) -> bool {
    name.starts_with("gorca-connector-") && name != "gorca-connector-api"
        || name.starts_with("gorca-speech-")
        || (name.starts_with("gorca-input-") && name != "gorca-input-api")
        || name == "gorca-ext-host"
        || (name.starts_with("gorca-doc-source-") && name != "gorca-doc-source-api")
}

/// Evaluate every rule. An empty result means the graph is clean.
pub fn check(g: &Graph) -> Vec<Violation> {
    let mut v = Vec::new();
    let mut report = |rule: u8, message: String| v.push(Violation { rule, message });

    // Rule 1.
    for name in NEUTRAL {
        if let Some(chain) = g.path_to(name, is_transport) {
            report(
                1,
                format!(
                    "`{name}` must stay transport-neutral: {}",
                    chain.join(" -> ")
                ),
            );
        }
    }
    // Rule 2 (direct edges of workspace crates).
    for p in g.pkgs.values().filter(|p| p.in_workspace) {
        if !MAY_USE_ZBUS.contains(&p.name.as_str())
            && let Some(d) = g.depends_directly(&p.name, is_zbus)
        {
            report(2, format!("`{}` may not depend on `{d}`", p.name));
        }
        if !MAY_USE_ATSPI.contains(&p.name.as_str())
            && let Some(d) = g.depends_directly(&p.name, is_atspi)
        {
            report(2, format!("`{}` may not depend on `{d}`", p.name));
        }
    }
    // Rule 3.
    if let Some(chain) = g.path_to("gorca-core", core_forbidden) {
        report(
            3,
            format!(
                "`gorca-core` reaches a concrete implementation: {}",
                chain.join(" -> ")
            ),
        );
    }
    for p in g
        .pkgs
        .values()
        .filter(|p| p.in_workspace && p.name != "gorca-daemon")
    {
        if let Some(d) = g.depends_directly(&p.name, |d| CONCRETE.contains(&d)) {
            // Concrete crates may of course depend on each other's lower layers (for example
            // the AT-SPI connector on gorca-dbus); only the wiring targets are protected.
            report(
                3,
                format!(
                    "`{}` depends on `{d}`; only `gorca-daemon` wires implementations",
                    p.name
                ),
            );
        }
    }
    // Rule 4.
    for name in TOKIO_FREE {
        if let Some(chain) = g.path_to(name, |d| d == "tokio") {
            report(
                4,
                format!("`{name}` must not depend on tokio: {}", chain.join(" -> ")),
            );
        }
    }
    // Rule 5.
    for p in g.pkgs.values().filter(|p| p.is_extension) {
        let forbidden = |d: &str| {
            g.pkgs.get(d).is_some_and(|x| x.in_workspace)
                && d != "gorca-ext-api"
                && d != "gorca-i18n"
        };
        if let Some(chain) = g.path_to(&p.name, forbidden) {
            report(
                5,
                format!(
                    "extension `{}` may depend only on `gorca-ext-api`: {}",
                    p.name,
                    chain.join(" -> ")
                ),
            );
        }
    }
    // Rule 9.
    for p in g.pkgs.values().filter(|p| p.in_workspace) {
        if p.name != "gorca-ext-host"
            && let Some(d) = g.depends_directly(&p.name, is_wasmtime)
        {
            report(
                9,
                format!(
                    "`{}` may not depend on `{d}`; only `gorca-ext-host` runs WASM",
                    p.name
                ),
            );
        }
        if p.name != "gorca-ext-api"
            && let Some(d) = g.depends_directly(&p.name, is_wit_bindgen)
        {
            report(
                9,
                format!(
                    "`{}` may not depend on `{d}`; only `gorca-ext-api` generates guest bindings",
                    p.name
                ),
            );
        }
    }
    for banned in [is_zbus as fn(&str) -> bool, is_atspi] {
        if let Some(chain) = g.path_to("gorca-ext-host", banned) {
            report(
                9,
                format!(
                    "`gorca-ext-host` must not reach a connector transport: {}",
                    chain.join(" -> ")
                ),
            );
        }
    }
    v
}

/// Build the graph from `cargo metadata`.
pub fn load() -> Result<Graph> {
    let md = MetadataCommand::new()
        .exec()
        .context("running cargo metadata")?;
    let resolve = md
        .resolve
        .as_ref()
        .context("cargo metadata returned no resolve graph")?;
    let ws: BTreeSet<_> = md.workspace_members.iter().collect();
    let by_id: BTreeMap<_, _> = md.packages.iter().map(|p| (&p.id, p)).collect();

    let mut g = Graph::default();
    for node in &resolve.nodes {
        let Some(pkg) = by_id.get(&node.id) else {
            continue;
        };
        let name = pkg.name.to_string();
        let deps = node
            .deps
            .iter()
            .filter(|d| {
                d.dep_kinds
                    .iter()
                    .any(|k| matches!(k.kind, DependencyKind::Normal | DependencyKind::Build))
            })
            .filter_map(|d| by_id.get(&d.pkg).map(|p| p.name.to_string()))
            .collect();
        let manifest = pkg.manifest_path.as_str();
        g.pkgs.insert(
            name.clone(),
            Pkg {
                name,
                in_workspace: ws.contains(&node.id),
                is_extension: ws.contains(&node.id) && manifest.contains("/extensions/"),
                deps,
            },
        );
    }
    Ok(g)
}

/// `cargo xtask deps`. Returns `Ok(true)` when the graph is clean.
pub fn run() -> Result<bool> {
    let g = load()?;
    let violations = check(&g);
    let ws = g.pkgs.values().filter(|p| p.in_workspace).count();
    if violations.is_empty() {
        println!(
            "dependency rules: ok ({ws} workspace crates, {} packages)",
            g.pkgs.len()
        );
        return Ok(true);
    }
    for x in &violations {
        eprintln!("rule {}: {}", x.rule, x.message);
    }
    eprintln!("{} dependency rule violation(s)", violations.len());
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a graph from `(name, in_workspace, deps)`.
    fn graph(items: &[(&str, bool, &[&str])]) -> Graph {
        let mut g = Graph::default();
        for (name, ws, deps) in items {
            g.pkgs.insert(
                (*name).to_owned(),
                Pkg {
                    name: (*name).to_owned(),
                    in_workspace: *ws,
                    is_extension: name.starts_with("ext-"),
                    deps: deps.iter().map(|d| (*d).to_owned()).collect(),
                },
            );
        }
        // Every dependency must exist as a node.
        let missing: Vec<String> = g
            .pkgs
            .values()
            .flat_map(|p| p.deps.iter().cloned())
            .filter(|d| !g.pkgs.contains_key(d))
            .collect();
        for m in missing {
            g.pkgs.insert(
                m.clone(),
                Pkg {
                    name: m,
                    in_workspace: false,
                    is_extension: false,
                    deps: BTreeSet::new(),
                },
            );
        }
        g
    }

    fn rules(v: &[Violation]) -> Vec<u8> {
        v.iter().map(|x| x.rule).collect()
    }

    #[test]
    fn clean_graph_passes() {
        let g = graph(&[
            ("gorca-model", true, &[]),
            ("gorca-core", true, &["gorca-model", "gorca-speech"]),
            ("gorca-speech", true, &["gorca-model"]),
            ("gorca-dbus", true, &["zbus"]),
            (
                "gorca-connector-atspi",
                true,
                &["gorca-dbus", "atspi", "zbus"],
            ),
            (
                "gorca-daemon",
                true,
                &["gorca-core", "gorca-connector-atspi"],
            ),
            ("zbus", false, &[]),
            ("atspi", false, &["zbus"]),
        ]);
        assert_eq!(check(&g), vec![]);
    }

    #[test]
    fn core_depending_on_a_connector_is_rejected_with_the_chain() {
        let g = graph(&[
            ("gorca-core", true, &["gorca-connector-atspi"]),
            ("gorca-connector-atspi", true, &["atspi", "zbus"]),
            ("atspi", false, &["zbus"]),
            ("zbus", false, &[]),
        ]);
        let v = check(&g);
        assert!(rules(&v).contains(&1), "{v:?}");
        assert!(rules(&v).contains(&3), "{v:?}");
        assert!(
            v.iter()
                .any(|x| x.message.contains("gorca-core -> gorca-connector-atspi"))
        );
    }

    #[test]
    fn transitive_transport_leak_through_a_neutral_crate_is_caught() {
        let g = graph(&[
            ("gorca-mirror", true, &["helper"]),
            ("helper", false, &["wasmtime-environ"]),
            ("wasmtime-environ", false, &[]),
        ]);
        let v = check(&g);
        assert_eq!(rules(&v), vec![1]);
        assert!(
            v[0].message
                .contains("gorca-mirror -> helper -> wasmtime-environ")
        );
    }

    #[test]
    fn zbus_and_atspi_are_limited_to_their_crates() {
        let g = graph(&[
            ("gorca-rt", true, &["zbus"]),
            ("gorca-dbus", true, &["atspi"]),
            ("zbus", false, &[]),
            ("atspi", false, &[]),
        ]);
        let v = check(&g);
        assert_eq!(rules(&v), vec![2, 2]);
    }

    #[test]
    fn tokio_is_forbidden_in_model_and_doc_even_transitively() {
        let g = graph(&[
            ("gorca-model", true, &["gorca-rt"]),
            ("gorca-doc", true, &["gorca-model"]),
            ("gorca-rt", true, &["tokio"]),
            ("tokio", false, &[]),
        ]);
        let v = check(&g);
        assert_eq!(rules(&v), vec![4, 4]);
    }

    #[test]
    fn extensions_may_only_use_ext_api() {
        let ok = graph(&[
            ("ext-vte", true, &["gorca-ext-api", "serde"]),
            ("gorca-ext-api", true, &[]),
        ]);
        assert_eq!(check(&ok), vec![]);
        let bad = graph(&[
            ("ext-vte", true, &["gorca-ext-api", "gorca-model"]),
            ("gorca-ext-api", true, &[]),
            ("gorca-model", true, &[]),
        ]);
        assert_eq!(rules(&check(&bad)), vec![5]);
    }

    #[test]
    fn the_wasm_runtime_is_confined_to_the_host() {
        let ok = graph(&[
            ("gorca-ext-host", true, &["wasmtime", "gorca-core"]),
            ("gorca-core", true, &[]),
            ("gorca-ext-api", true, &["wit-bindgen"]),
            ("wasmtime", false, &[]),
            ("wit-bindgen", false, &[]),
        ]);
        assert_eq!(check(&ok), vec![]);
        let bad = graph(&[
            ("gorca-daemon", true, &["wasmtime"]),
            ("ext-vte", true, &["gorca-ext-api", "wit-bindgen"]),
            ("gorca-ext-host", true, &["zbus"]),
            ("gorca-ext-api", true, &[]),
            ("wasmtime", false, &[]),
            ("wit-bindgen", false, &[]),
            ("zbus", false, &[]),
        ]);
        assert_eq!(rules(&check(&bad)), vec![2, 9, 9, 9]);
    }

    #[test]
    fn only_the_daemon_wires_concrete_implementations() {
        let g = graph(&[
            ("gorca-testkit", true, &["gorca-speech-ssip"]),
            ("gorca-speech-ssip", true, &[]),
            (
                "gorca-daemon",
                true,
                &["gorca-speech-ssip", "gorca-ext-host"],
            ),
            ("gorca-ext-host", true, &[]),
        ]);
        let v = check(&g);
        assert_eq!(rules(&v), vec![3]);
        assert!(v[0].message.contains("gorca-testkit"));
    }

    /// The real workspace must satisfy every rule. This is the CI gate: a forbidden edge added
    /// to any `Cargo.toml` fails `cargo test --workspace`.
    #[test]
    fn the_real_workspace_satisfies_the_adr_rules() {
        let g = load().unwrap();
        let v = check(&g);
        assert!(v.is_empty(), "dependency rule violations: {v:#?}");
        assert!(
            g.pkgs.contains_key("gorca-core"),
            "metadata must include the workspace"
        );
    }
}
