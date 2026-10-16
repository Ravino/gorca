//! m5-rereview probe extension: `present` attacks that stay inside every per-call meter
//! (self-timed against the WASI monotonic clock), unmetered WASI host calls, settings reads
//! with a large value, and an oversize hook result.
#![allow(unsafe_code)]

use std::time::{Duration, Instant};

use gorca_ext_api::{Event, EventVerdict, Hooks, Node, PresentCtx, Segment, SegmentKind, export_extension, host};

#[link(wasm_import_module = "wasi:random/random@0.2.0")]
unsafe extern "C" {
    #[link_name = "get-random-bytes"]
    fn raw_random_bytes(len: i64, ret: *mut u32);
}

fn arg(text: &str, p: &str) -> Option<u64> {
    text.strip_prefix(p)?.parse().ok()
}

fn spin_until(us: u64) {
    let t0 = Instant::now();
    let d = Duration::from_micros(us);
    let mut x = 0u64;
    while t0.elapsed() < d {
        x = std::hint::black_box(x.wrapping_add(1));
    }
}

fn random_forever() {
    let mut ret = [0u32; 2];
    loop {
        unsafe { raw_random_bytes(1 << 16, ret.as_mut_ptr()) };
        // free the returned list so guest memory does not grow
        let v = unsafe { Vec::from_raw_parts(ret[0] as *mut u8, ret[1] as usize, ret[1] as usize) };
        drop(std::hint::black_box(v));
    }
}

/// Unmetered host work (WASI random) until `us` have passed: never out of fuel or host units.
fn random_until(us: u64) {
    let t0 = Instant::now();
    let d = Duration::from_micros(us);
    let mut ret = [0u32; 2];
    while t0.elapsed() < d {
        unsafe { raw_random_bytes(1 << 16, ret.as_mut_ptr()) };
        let v = unsafe { Vec::from_raw_parts(ret[0] as *mut u8, ret[1] as usize, ret[1] as usize) };
        drop(std::hint::black_box(v));
    }
}

fn clock_forever() {
    loop {
        std::hint::black_box(Instant::now());
    }
}

fn settings_forever() {
    host::set_setting("big", &"a".repeat(4096));
    loop {
        std::hint::black_box(host::get_setting("big"));
    }
}

#[derive(Default)]
struct Hostile2;

impl Hooks for Hostile2 {
    fn on_event(&mut self, _ev: &Event) -> EventVerdict {
        EventVerdict::Pass
    }

    fn present(&mut self, n: &Node, _ctx: PresentCtx) -> Option<Vec<Segment>> {
        match n.name.as_deref() {
            Some(t) if t.starts_with("selftime:") => spin_until(arg(t, "selftime:").unwrap_or(0)),
            Some("random") => random_forever(),
            Some(t) if t.starts_with("randtime:") => random_until(arg(t, "randtime:").unwrap_or(0)),
            Some("clock") => clock_forever(),
            Some("settings") => settings_forever(),
            Some(t) if t.starts_with("bigresult:") => {
                let n = arg(t, "bigresult:").unwrap_or(1) as usize;
                let s = "a".repeat(1000);
                return Some((0..n).map(|_| Segment { text: s.clone(), kind: SegmentKind::Text }).collect());
            }
            _ => {}
        }
        None
    }
}

export_extension!(Hostile2);
