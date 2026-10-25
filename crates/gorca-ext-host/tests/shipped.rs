// SPDX-License-Identifier: GPL-3.0-or-later
//! Every first-party extension that `cargo xtask build-ext` ships is built for the host's
//! extension world and loads (ADR-0019 V-1: pre-1.0 versions carry no ABI promise, a component of
//! another version is refused, and a refused Shell policy would be silent). The list is the
//! `SHIPPED` list of `xtask/src/ext.rs`, read from the source so that the two cannot drift.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use gorca_core::MirrorDirectory;
use gorca_ext_host::{ExtHost, Manifest};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The extensions `build-ext` ships, from `xtask/src/ext.rs`.
fn shipped() -> Vec<String> {
    let src = std::fs::read_to_string(root().join("xtask/src/ext.rs")).expect("xtask/src/ext.rs");
    let line = src
        .lines()
        .find(|l| l.contains("const SHIPPED: &[&str]"))
        .expect("the SHIPPED list");
    let names: Vec<String> = line
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect();
    assert!(!names.is_empty(), "{line}");
    names
}

fn build(name: &str) -> Vec<u8> {
    let target = root().join("target/ext-wasm");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let status = Command::new(cargo)
        .current_dir(root())
        .args([
            "build",
            "--quiet",
            "-p",
            name,
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
    assert!(status.success(), "building {name} as a component");
    std::fs::read(
        target
            .join("wasm32-wasip2/release")
            .join(name.replace('-', "_"))
            .with_extension("wasm"),
    )
    .expect("the component")
}

#[test]
fn every_shipped_component_is_built_for_this_host_and_loads() {
    let names = shipped();
    assert!(names.contains(&"ext-gnome-shell".to_owned()), "{names:?}");
    for name in names {
        let manifest_text =
            std::fs::read_to_string(root().join("extensions").join(&name).join("ext.toml"))
                .expect("the manifest");
        let manifest = Manifest::parse(&manifest_text).expect("the manifest parses");
        let mut b = ExtHost::builder(Arc::new(MirrorDirectory::new()));
        b.add_wasm(manifest, &build(&name))
            .unwrap_or_else(|e| panic!("{name} does not load: {e}"));
    }
}
