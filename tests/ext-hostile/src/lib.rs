// SPDX-License-Identifier: GPL-3.0-or-later
//! Hostile extension (from the m5 review probe, kept as a regression fixture): exercises host-side
//! work that guest fuel cannot see, the canonical-ABI lift of import arguments, and host-call loops.
//! Built by `crates/gorca-ext-host/tests/hostile.rs`; not a workspace member (raw wasm imports).
#![allow(unsafe_code)]

use gorca_ext_api::{Event, EventVerdict, Hooks, Node, PresentCtx, Segment, export_extension, host};

#[link(wasm_import_module = "gorca:ext/host@0.4.0")]
unsafe extern "C" {
    #[link_name = "speak"]
    fn raw_speak(ptr: *const u8, len: usize, prio: i32);
    #[link_name = "log"]
    fn raw_log(level: i32, ptr: *const u8, len: usize);
}

#[link(wasm_import_module = "wasi:random/random@0.2.0")]
unsafe extern "C" {
    #[link_name = "get-random-bytes"]
    fn raw_random_bytes(len: i64, ret: *mut u32);
}

/// WASI random bytes in a loop, until `us` microseconds have passed (`None`: forever). Few guest
/// instructions per call, so fuel never fires; only the host-work meter can stop it.
fn random_until(us: Option<u64>) {
    let t0 = std::time::Instant::now();
    let mut ret = [0u32; 2];
    while us.is_none_or(|us| t0.elapsed() < std::time::Duration::from_micros(us)) {
        unsafe { raw_random_bytes(1 << 16, ret.as_mut_ptr()) };
        let v = unsafe { Vec::from_raw_parts(ret[0] as *mut u8, ret[1] as usize, ret[1] as usize) };
        drop(std::hint::black_box(v));
    }
}

/// The WASI monotonic clock in a loop, until `us` microseconds have passed (`None`: forever).
fn clock_until(us: Option<u64>) {
    let t0 = std::time::Instant::now();
    while us.is_none_or(|us| t0.elapsed() < std::time::Duration::from_micros(us)) {
        std::hint::black_box(std::time::Instant::now());
    }
}

const BIG: usize = 16 << 20;

/// `n` segment records (12 bytes: string ptr, string len, kind u8) all aliasing one `BIG` string.
fn alias_speak(n: usize) {
    let s = vec![b'a'; BIG];
    let mut recs: Vec<u32> = Vec::with_capacity(n * 3);
    for _ in 0..n {
        recs.push(s.as_ptr() as u32);
        recs.push(BIG as u32);
        recs.push(0);
    }
    unsafe { raw_speak(recs.as_ptr().cast(), n, 1) };
    std::hint::black_box(&s);
}

/// The host clips `log` to 512 chars and 8 calls, but only after lifting the whole string.
fn log_loop(n: usize) {
    let s = vec![b'a'; BIG];
    for _ in 0..n {
        unsafe { raw_log(3, s.as_ptr(), BIG) };
    }
}

/// Calls the host `log` forever with the largest string the host accepts (so every lift
/// succeeds and only the host-work meter can stop it).
fn log_max_loop() {
    let s = vec![b'a'; 60 << 10];
    loop {
        unsafe { raw_log(3, s.as_ptr(), s.len()) };
    }
}

fn node_loop() {
    loop {
        std::hint::black_box(host::get_node((1, 1)));
    }
}

fn arg(text: &str, p: &str) -> Option<usize> {
    text.strip_prefix(p)?.parse().ok()
}

#[derive(Default)]
struct Hostile;

impl Hooks for Hostile {
    fn on_event(&mut self, ev: &Event) -> EventVerdict {
        if let Event::Announcement(t) = ev {
            if let Some(n) = arg(t, "alias:") {
                alias_speak(n);
            } else if let Some(n) = arg(t, "log:") {
                log_loop(n);
            } else if t == "logmax" {
                log_max_loop();
            } else if t == "nodeforever" {
                node_loop();
            } else if t == "logforever" {
                log_loop(usize::MAX);
            } else if t == "ok" {
                host::log(3, "ok");
            }
        }
        EventVerdict::Pass
    }

    fn present(&mut self, n: &Node, _ctx: PresentCtx) -> Option<Vec<Segment>> {
        match n.name.as_deref() {
            Some("logforever") => log_loop(usize::MAX),
            Some("logmax") => log_max_loop(),
            Some("nodeforever") => node_loop(),
            Some("randforever") => random_until(None),
            Some("clockforever") => clock_until(None),
            Some(t) if t.starts_with("randtime:") => random_until(arg(t, "randtime:").map(|n| n as u64)),
            Some(t) if t.starts_with("clocktime:") => clock_until(arg(t, "clocktime:").map(|n| n as u64)),
            Some(t) if t.starts_with("alias:") => alias_speak(arg(t, "alias:").unwrap_or(1)),
            _ => {}
        }
        None
    }
}

export_extension!(Hostile);
