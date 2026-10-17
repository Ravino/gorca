// SPDX-License-Identifier: GPL-3.0-or-later
//! `b1-fakeapp` command line.
//!
//! ```text
//! b1-fakeapp --model <name|path.json> [--label NAME] [--bus ADDR] [--truth FILE]
//!            [--no-register] [--seed N] [--p2p peer|same_bus|none] [--quirks pessimistic|none]
//!            [--dir DIR] [--allow-any-bus]
//! ```
//!
//! Control channel: one JSON object per stdin line (`{"id":1,"cmd":"rename",...}`, see
//! `gorca_testkit::b1::proto::Cmd`); one JSON line per answer on stdout
//! (`{"event":"reply","id":1,"ok":true,"data":{...}}`). The first stdout line is
//! `{"event":"ready","bus_name":...,"root":...,"p2p_address":...,"nodes":N,"landmarks":{...}}`.
//! EOF on stdin ends the process.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use b1_fakeapp::{FakeApp, Options};
use gorca_testkit::b1::proto::{Cmd, ModelSpec, P2pMode, Quirks};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};

fn models_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../models")
}

fn load_model(arg: &str) -> Result<ModelSpec> {
    let path = if Path::new(arg).extension().is_some_and(|e| e == "json") || arg.contains('/') {
        PathBuf::from(arg)
    } else {
        models_dir().join(format!("{arg}.json"))
    };
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

fn emit(v: &serde_json::Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut model = None;
    let mut label = None;
    let mut bus = std::env::var("AT_SPI_BUS_ADDRESS").ok();
    let mut truth = None;
    let mut register = true;
    let mut seed = None;
    let mut p2p = None;
    let mut quirks = None;
    let mut dir =
        std::env::var("GORCA_HARNESS_DIR").map_or_else(|_| std::env::temp_dir(), PathBuf::from);
    let mut allow_any = false;
    let mut session_bus = None;
    let mut own_name = None;
    while let Some(a) = args.next() {
        let mut val = || args.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--model" => model = Some(val()?),
            "--label" => label = Some(val()?),
            "--bus" => bus = Some(val()?),
            "--truth" => truth = Some(PathBuf::from(val()?)),
            "--no-register" => register = false,
            "--seed" => seed = Some(val()?.parse::<u64>()?),
            "--p2p" => p2p = Some(val()?),
            "--quirks" => quirks = Some(val()?),
            "--dir" => dir = PathBuf::from(val()?),
            "--allow-any-bus" => allow_any = true,
            "--session-bus" => session_bus = Some(val()?),
            "--own-name" => own_name = Some(val()?),
            other => bail!("unknown argument {other}"),
        }
    }
    let model = model.context("--model is required")?;
    let mut spec = load_model(&model)?;
    if let Some(s) = seed {
        spec.seed = s;
    }
    match p2p.as_deref() {
        Some("peer") => spec.p2p = P2pMode::Peer,
        Some("same_bus") => spec.p2p = P2pMode::SameBus,
        Some("none") => spec.p2p = P2pMode::None,
        Some(o) => bail!("bad --p2p {o}"),
        None => {}
    }
    match quirks.as_deref() {
        Some("pessimistic") => spec.quirks = Quirks::pessimistic_gtk4(),
        Some("none") => spec.quirks = Quirks::default(),
        Some(o) => bail!("bad --quirks {o}"),
        None => {}
    }
    let bus_address = bus.context("no --bus and no AT_SPI_BUS_ADDRESS")?;
    // Isolation: never touch an a11y bus that is not a harness session's.
    if !allow_any && !bus_address.contains("/gorca-h.") {
        bail!(
            "refusing to connect to {bus_address}: not a harness session bus (use --allow-any-bus for a test dbus-daemon)"
        );
    }
    if let Some(sb) = &session_bus
        && !allow_any
        && !sb.contains("/gorca-h.")
    {
        bail!("refusing to connect to {sb}: not a harness session bus");
    }
    if own_name.is_none() {
        own_name.clone_from(&spec.owns_name);
        if own_name.is_some() && session_bus.is_none() {
            session_bus = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok();
        }
    }
    let label = label.unwrap_or_else(|| spec.name.clone());
    let app = FakeApp::start(Options {
        spec,
        label: label.clone(),
        bus_address,
        truth_path: truth,
        register,
        peer_dir: dir,
        session_bus,
        own_name,
    })
    .await?;
    let r = &app.ready;
    emit(&json!({
        "event": "ready", "label": label, "bus_name": r.bus_name, "root": r.root,
        "p2p_address": r.p2p_address, "nodes": r.nodes, "landmarks": r.landmarks,
    }));
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                emit(&json!({"event": "reply", "ok": false, "error": format!("bad json: {e}")}));
                continue;
            }
        };
        let id = v.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let cmd: Cmd = match serde_json::from_value(v) {
            Ok(c) => c,
            Err(e) => {
                emit(
                    &json!({"event": "reply", "id": id, "ok": false, "error": format!("bad command: {e}")}),
                );
                continue;
            }
        };
        let quit = matches!(cmd, Cmd::Quit);
        let reply = app.control(cmd).await;
        emit(
            &json!({"event": "reply", "id": id, "ok": reply.ok, "error": reply.error, "data": reply.data}),
        );
        if quit {
            break;
        }
    }
    Ok(())
}
