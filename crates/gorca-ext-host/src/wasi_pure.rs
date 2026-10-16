//! The pure WASI imports a Rust `wasm32-wasip2` extension may pull in: random numbers and
//! clocks. Everything else WASI (files, sockets, environment, stdio, process) stays a trap, so an
//! extension has no ambient capability. These are implemented from the host's own state and make
//! no system calls beyond reading the clock.
//!
//! Random numbers are *not* cryptographic (a splitmix64 stream seeded from the clock). The
//! standard library uses them to seed `HashMap`s, where that is enough.
//!
//! Every import here is charged to the running call's host-work meter ([`HostMeter`]) like any
//! other host function, so a guest that loops on a clock or the random generator is cut by the
//! meter instead of holding its caller until the wall-clock backstop. The other WASI imports
//! (`wasi:io/poll`, sockets, files, ...) are not linked and trap on first use.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use wasmtime::component::{Component, Linker, Val};
use wasmtime::{Engine, StoreContextMut};

/// The per-call host-work meter of a store (see `wasm.rs`).
pub trait HostMeter {
    /// Charge one host call that moves `bytes`; an error traps the guest.
    fn charge(&mut self, bytes: usize) -> wasmtime::Result<()>;
}

/// Meter units charged per random byte: the generator costs a few ns per byte.
const RANDOM_UNITS_PER_BYTE: usize = 8;

static STATE: AtomicU64 = AtomicU64::new(0x9e37_79b9_7f4a_7c15);

fn clock_nanos() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    )
    .unwrap_or(0)
}

fn next_u64() -> u64 {
    mix(STATE.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed) ^ clock_nanos())
}

/// splitmix64 finaliser.
fn mix(seed: u64) -> u64 {
    let mut z = seed;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn start() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

const MAX_RANDOM_BYTES: u64 = 1 << 16;

/// Link the pure WASI interfaces `component` imports.
pub fn add<T: HostMeter + 'static>(
    linker: &mut Linker<T>,
    component: &Component,
    engine: &Engine,
) -> wasmtime::Result<()> {
    let _ = start();
    for (name, _) in component.component_type().imports(engine) {
        let base = name.split('@').next().unwrap_or(name);
        match base {
            "wasi:random/random" | "wasi:random/insecure" => {
                let mut i = linker.instance(name)?;
                i.func_wrap("get-random-u64", |mut cx: StoreContextMut<'_, T>, (): ()| {
                    cx.data_mut().charge(8)?;
                    Ok((next_u64(),))
                })?;
                i.func_wrap(
                    "get-random-bytes",
                    |mut cx: StoreContextMut<'_, T>, (len,): (u64,)| {
                        let len = usize::try_from(len.min(MAX_RANDOM_BYTES)).unwrap_or(0);
                        cx.data_mut().charge(len * RANDOM_UNITS_PER_BYTE)?;
                        let mut out = Vec::with_capacity(len + 8);
                        // One clock read and one atomic step per call, then a counter stream.
                        let mut x = next_u64();
                        while out.len() < len {
                            x = mix(x.wrapping_add(0x9e37_79b9_7f4a_7c15));
                            out.extend_from_slice(&x.to_le_bytes());
                        }
                        out.truncate(len);
                        Ok((out,))
                    },
                )?;
            }
            "wasi:random/insecure-seed" => {
                linker
                    .instance(name)?
                    .func_wrap("insecure-seed", |mut cx: StoreContextMut<'_, T>, (): ()| {
                        cx.data_mut().charge(16)?;
                        Ok(((next_u64(), next_u64()),))
                    })?;
            }
            "wasi:clocks/monotonic-clock" => {
                let mut i = linker.instance(name)?;
                i.func_wrap("now", |mut cx: StoreContextMut<'_, T>, (): ()| {
                    cx.data_mut().charge(0)?;
                    Ok((u64::try_from(start().elapsed().as_nanos()).unwrap_or(u64::MAX),))
                })?;
                i.func_wrap("resolution", |mut cx: StoreContextMut<'_, T>, (): ()| {
                    cx.data_mut().charge(0)?;
                    Ok((1u64,))
                })?;
            }
            "wasi:clocks/wall-clock" => {
                let mut i = linker.instance(name)?;
                i.func_new("now", |mut cx, _, _, results| {
                    cx.data_mut().charge(0)?;
                    let d = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default();
                    results[0] = datetime(d.as_secs(), d.subsec_nanos());
                    Ok(())
                })?;
                i.func_new("resolution", |mut cx, _, _, results| {
                    cx.data_mut().charge(0)?;
                    results[0] = datetime(0, 1);
                    Ok(())
                })?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn datetime(seconds: u64, nanoseconds: u32) -> Val {
    Val::Record(vec![
        ("seconds".to_owned(), Val::U64(seconds)),
        ("nanoseconds".to_owned(), Val::U32(nanoseconds)),
    ])
}
