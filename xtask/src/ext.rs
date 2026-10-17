//! `cargo xtask build-ext`: build the shipped extensions as `wasm32-wasip2` components and lay
//! them out where the daemon looks (ADR-0008 amendment 1: extensions are built once for WASM,
//! the default, and natively; this is the WASM half).
//!
//! Layout written, once per cargo profile directory:
//!
//! ```text
//! <target>/<debug|release>/extensions/<name>/ext.toml
//! <target>/<debug|release>/extensions/<name>/<module>.wasm
//! <target>/<debug|release>/extensions/<name>/i18n/<lang>.msg   (the extension's message catalogs)
//! ```
//!
//! which is `<exe dir>/extensions` for `target/<profile>/gorca`, the first place the daemon
//! looks when no `--ext-dir` is named. `--dest DIR` writes `DIR/<name>/...` instead. The
//! component is always a release build: the wasm is compiled once more by wasmtime at load time,
//! and a debug component only costs fuel.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// The extensions shipped as components.
const SHIPPED: &[&str] = &["ext-gnome-shell", "ext-vte"];

pub fn run(args: impl Iterator<Item = String>) -> Result<bool> {
    let mut dest: Option<PathBuf> = None;
    let mut args = args;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dest" => dest = Some(PathBuf::from(args.next().context("--dest needs a value")?)),
            other => bail!("unknown build-ext argument `{other}`"),
        }
    }
    let meta = cargo_metadata::MetadataCommand::new()
        .no_deps()
        .exec()
        .context("cargo metadata")?;
    let root = PathBuf::from(&meta.workspace_root);
    let target = PathBuf::from(&meta.target_directory);
    let wasm_target = target.join("ext-wasm");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    for name in SHIPPED {
        let status = Command::new(&cargo)
            .current_dir(&root)
            .args([
                "build",
                "--quiet",
                "-p",
                name,
                "--target",
                "wasm32-wasip2",
                "--release",
            ])
            .arg("--target-dir")
            .arg(&wasm_target)
            // Line tables and symbols are of no use in a component (a trap shows no source
            // lines either way) and make it ten times larger.
            .env("CARGO_PROFILE_RELEASE_DEBUG", "false")
            .env("CARGO_PROFILE_RELEASE_STRIP", "true")
            .status()
            .with_context(|| format!("run cargo for {name}"))?;
        if !status.success() {
            bail!("building {name} for wasm32-wasip2 failed");
        }
        let manifest = root.join("extensions").join(name).join("ext.toml");
        let module = module_name(&manifest)?;
        let built = wasm_target
            .join("wasm32-wasip2/release")
            .join(name.replace('-', "_"))
            .with_extension("wasm");
        let dests: Vec<PathBuf> = match &dest {
            Some(d) => vec![d.join(name)],
            None => ["debug", "release"]
                .iter()
                .map(|p| target.join(p).join("extensions").join(name))
                .collect(),
        };
        for d in dests {
            std::fs::create_dir_all(&d).with_context(|| format!("create {}", d.display()))?;
            copy(&manifest, &d.join("ext.toml"))?;
            copy(&built, &d.join(&module))?;
            copy_catalogs(
                &root.join("extensions").join(name).join("i18n"),
                &d.join("i18n"),
            )?;
            println!("installed {}", d.join(&module).display());
        }
    }
    Ok(true)
}

/// The `module = "..."` of a manifest: the file name the daemon will look for.
fn module_name(manifest: &Path) -> Result<String> {
    let text = std::fs::read_to_string(manifest)
        .with_context(|| format!("read {}", manifest.display()))?;
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("module")
            && let Some(v) = rest.trim_start().strip_prefix('=')
        {
            let v = v.trim().trim_matches('"');
            if !v.is_empty() && !v.contains('/') && !v.contains("..") {
                return Ok(v.to_owned());
            }
        }
    }
    bail!(
        "{} has no plain `module = \"file.wasm\"`",
        manifest.display()
    )
}

fn copy(from: &Path, to: &Path) -> Result<()> {
    // Write beside, then rename: a daemon starting now never reads a half-written component.
    let tmp = to.with_extension("tmp");
    std::fs::copy(from, &tmp)
        .with_context(|| format!("copy {} to {}", from.display(), tmp.display()))?;
    std::fs::rename(&tmp, to).with_context(|| format!("rename to {}", to.display()))?;
    Ok(())
}

/// Copy an extension's `i18n/*.msg` catalogs next to its manifest (none is fine).
fn copy_catalogs(from: &Path, to: &Path) -> Result<()> {
    let Ok(rd) = std::fs::read_dir(from) else {
        return Ok(());
    };
    std::fs::create_dir_all(to).with_context(|| format!("create {}", to.display()))?;
    for e in rd {
        let path = e?.path();
        if path.extension().is_some_and(|x| x == "msg")
            && let Some(name) = path.file_name()
        {
            copy(&path, &to.join(name))?;
        }
    }
    Ok(())
}
