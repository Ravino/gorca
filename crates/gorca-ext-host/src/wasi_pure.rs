//! The pure WASI imports a Rust `wasm32-wasip2` extension may pull in: random numbers and
//! clocks. Everything else WASI (files, sockets, environment, stdio, process) stays a trap, so an
//! extension has no ambient capability. These are implemented from the host's own state and make
//! no system calls beyond reading the clock.
//!
//! Random numbers are *not* cryptographic (a splitmix64 stream seeded from the clock). The
//! standard library uses them to seed `HashMap`s, where that is enough.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use wasmtime::component::{Component, Linker, Val};
use wasmtime::{Engine, StoreContextMut};

static STATE: AtomicU64 = AtomicU64::new(0x9e37_79b9_7f4a_7c15);

fn next_u64() -> u64 {
    // splitmix64 over an atomic counter: lock-free, and every caller gets a distinct value.
    let seed = STATE.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed);
    let mut z = seed
        ^ u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        )
        .unwrap_or(0);
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
pub fn add<T: 'static>(
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
                i.func_wrap("get-random-u64", |_: StoreContextMut<'_, T>, (): ()| {
                    Ok((next_u64(),))
                })?;
                i.func_wrap(
                    "get-random-bytes",
                    |_: StoreContextMut<'_, T>, (len,): (u64,)| {
                        let len = usize::try_from(len.min(MAX_RANDOM_BYTES)).unwrap_or(0);
                        let mut out = Vec::with_capacity(len);
                        while out.len() < len {
                            out.extend_from_slice(&next_u64().to_le_bytes());
                        }
                        out.truncate(len);
                        Ok((out,))
                    },
                )?;
            }
            "wasi:random/insecure-seed" => {
                linker
                    .instance(name)?
                    .func_wrap("insecure-seed", |_: StoreContextMut<'_, T>, (): ()| {
                        Ok(((next_u64(), next_u64()),))
                    })?;
            }
            "wasi:clocks/monotonic-clock" => {
                let mut i = linker.instance(name)?;
                i.func_wrap("now", |_: StoreContextMut<'_, T>, (): ()| {
                    Ok((u64::try_from(start().elapsed().as_nanos()).unwrap_or(u64::MAX),))
                })?;
                i.func_wrap("resolution", |_: StoreContextMut<'_, T>, (): ()| {
                    Ok((1u64,))
                })?;
            }
            "wasi:clocks/wall-clock" => {
                let mut i = linker.instance(name)?;
                i.func_new("now", |_, _, _, results| {
                    let d = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default();
                    results[0] = datetime(d.as_secs(), d.subsec_nanos());
                    Ok(())
                })?;
                i.func_new("resolution", |_, _, _, results| {
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
