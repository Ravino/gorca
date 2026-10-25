// SPDX-License-Identifier: MIT OR Apache-2.0
//! `meta.json`: host facts, condition label, loadavg and PSI before and after, governors, versions,
//! build flags, seed, scenario hash, guest hashes (SPEC 4.2).

use m18a0_common::timing::*;
use serde_json::{json, Value};
use std::process::Command;

#[derive(Clone, Debug)]
pub struct HostSnap {
    pub loadavg1: f64,
    pub psi_cpu: f64,
    pub psi_mem: f64,
    pub governors: String,
}

pub fn host_snap() -> HostSnap {
    HostSnap {
        loadavg1: loadavg1(),
        psi_cpu: psi_some_avg10("cpu"),
        psi_mem: psi_some_avg10("memory"),
        governors: governors(),
    }
}

impl HostSnap {
    pub fn to_json(&self) -> Value {
        json!({"loadavg1": self.loadavg1, "psi_cpu_some_avg10": self.psi_cpu, "psi_mem_some_avg10": self.psi_mem, "governors": self.governors})
    }

    /// Quiet limits (SPEC 4.1 as clarified by C-4): loadavg1 < 1.5 (the busy-polling writer alone
    /// contributes about 1.0 by the end of a run), PSI cpu some avg10 < 1.0, governor `performance`.
    pub fn is_quiet(&self) -> bool {
        self.loadavg1 < 1.5 && self.psi_cpu < 1.0 && self.governors == "performance"
    }
}

fn cmd(c: &str, args: &[&str]) -> String {
    Command::new(c)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

pub fn utc_stamp() -> String {
    // SAFETY: time(NULL) and gmtime_r with a valid out struct.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::gmtime_r(&t, &mut tm);
        format!(
            "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

pub fn host_facts() -> Value {
    let lscpu = cmd("lscpu", &["-e=CPU,CORE,MAXMHZ,MINMHZ"]);
    let model = cmd("sh", &["-c", "lscpu | grep -m1 'Model name' | cut -d: -f2"]);
    json!({
        "uname": cmd("uname", &["-r"]),
        "cpu_model": model.trim(),
        "core_class_map_lscpu_e": lscpu,
        "nproc": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        "mem_available_kb": read_trim("/proc/meminfo").lines().find(|l| l.starts_with("MemAvailable")).unwrap_or(""),
        "top_snapshot": cmd("sh", &["-c", "top -bn1 | head -15"]),
        "proc_self_timerslack_ns": read_trim("/proc/self/timerslack_ns"),
    })
}

pub fn versions() -> Value {
    json!({
        "wasmtime": "=49.0.3",
        "rustc": cmd("rustc", &["--version"]),
        "kernel": cmd("uname", &["-r"]),
        "build": {
            "profile": if cfg!(debug_assertions) { "debug (NOT a gating build)" } else { "release" },
            "lto": "fat", "codegen_units": 1,
            "RUSTFLAGS": std::env::var("RUSTFLAGS").unwrap_or_default(),
            "CARGO_ENCODED_RUSTFLAGS_note": "see .cargo/config.toml of the main repo for linker flags",
        },
    })
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    m18a0_common::host::hex(&Sha256::digest(bytes))
}
