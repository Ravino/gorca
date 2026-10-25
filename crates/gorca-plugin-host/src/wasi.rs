// SPDX-License-Identifier: GPL-3.0-or-later
//! The pure WASI imports a Rust `wasm32-wasip2` plugin may pull in: random numbers and clocks.
//!
//! M18a-1 linked none of them (`define_unknown_imports_as_traps`), so a guest that used a
//! `HashMap` trapped on the first insert. These are the same two interface families the legacy
//! extension host links, with one difference that is the point of the W lane: **they are
//! deterministic** (ADR-0026 PF-1, view-functional S1).
//!
//! * Random numbers come from a fixed stream that restarts at the start of every call. A
//!   `HashMap` therefore iterates in the same order for the same insertions, and a plugin cannot
//!   obtain entropy to make a fact depend on anything but its view.
//! * Clocks read a constant (monotonic 0, wall clock 0): there is no clock in derivation
//!   (ADR-0026 section 2.3.2, `diag.now-ms` is `none` in W).
//!
//! Everything else WASI (files, sockets, environment, stdio, process, poll) stays a trap, so a
//! plugin has no ambient capability. Every import here is charged to the call's host-work meter
//! like any other host function.

use wasmtime::component::{Component, Linker, Val};
use wasmtime::{Engine, StoreContextMut};

use crate::host::HostState;

/// Units charged per random byte.
const RANDOM_UNITS_PER_BYTE: u64 = 8;
/// A guest asking for more random bytes than this in one call gets this many.
const MAX_RANDOM_BYTES: u64 = 1 << 16;
/// The seed the stream restarts from on every call.
pub const STREAM_SEED: u64 = 0x9e37_79b9_7f4a_7c15;

/// splitmix64 finaliser.
fn mix(seed: u64) -> u64 {
    let mut z = seed;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// The deterministic per-call stream.
#[derive(Debug, Clone, Copy)]
pub struct Stream(u64);

impl Default for Stream {
    fn default() -> Self {
        Self(STREAM_SEED)
    }
}

impl Stream {
    /// Restart from the fixed seed (called at the start of every guest call).
    pub fn restart(&mut self) {
        self.0 = STREAM_SEED;
    }

    /// The next 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(STREAM_SEED);
        mix(self.0)
    }
}

/// Link the pure WASI interfaces `component` imports.
pub fn add(
    linker: &mut Linker<HostState>,
    component: &Component,
    engine: &Engine,
) -> wasmtime::Result<()> {
    for (name, _) in component.component_type().imports(engine) {
        let base = name.split('@').next().unwrap_or(name);
        match base {
            "wasi:random/random" | "wasi:random/insecure" => {
                let mut i = linker.instance(name)?;
                i.func_wrap(
                    "get-random-u64",
                    |mut cx: StoreContextMut<'_, HostState>, (): ()| {
                        cx.data_mut().charge_wasi(8)?;
                        Ok((cx.data_mut().stream.next_u64(),))
                    },
                )?;
                i.func_wrap(
                    "get-random-bytes",
                    |mut cx: StoreContextMut<'_, HostState>, (len,): (u64,)| {
                        let len = usize::try_from(len.min(MAX_RANDOM_BYTES)).unwrap_or(0);
                        cx.data_mut()
                            .charge_wasi(len as u64 * RANDOM_UNITS_PER_BYTE)?;
                        let mut out = Vec::with_capacity(len + 8);
                        while out.len() < len {
                            out.extend_from_slice(&cx.data_mut().stream.next_u64().to_le_bytes());
                        }
                        out.truncate(len);
                        Ok((out,))
                    },
                )?;
            }
            "wasi:random/insecure-seed" => {
                linker.instance(name)?.func_wrap(
                    "insecure-seed",
                    |mut cx: StoreContextMut<'_, HostState>, (): ()| {
                        cx.data_mut().charge_wasi(16)?;
                        let a = cx.data_mut().stream.next_u64();
                        let b = cx.data_mut().stream.next_u64();
                        Ok(((a, b),))
                    },
                )?;
            }
            "wasi:clocks/monotonic-clock" => {
                let mut i = linker.instance(name)?;
                i.func_wrap("now", |mut cx: StoreContextMut<'_, HostState>, (): ()| {
                    cx.data_mut().charge_wasi(0)?;
                    Ok((0u64,))
                })?;
                i.func_wrap(
                    "resolution",
                    |mut cx: StoreContextMut<'_, HostState>, (): ()| {
                        cx.data_mut().charge_wasi(0)?;
                        Ok((1u64,))
                    },
                )?;
            }
            "wasi:clocks/wall-clock" => {
                let mut i = linker.instance(name)?;
                i.func_new("now", |mut cx, _, _, results| {
                    cx.data_mut().charge_wasi(0)?;
                    results[0] = datetime(0, 0);
                    Ok(())
                })?;
                i.func_new("resolution", |mut cx, _, _, results| {
                    cx.data_mut().charge_wasi(0)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stream_restarts_identically() {
        let mut a = Stream::default();
        let first: Vec<u64> = (0..4).map(|_| a.next_u64()).collect();
        a.restart();
        let second: Vec<u64> = (0..4).map(|_| a.next_u64()).collect();
        assert_eq!(first, second);
        assert_ne!(first[0], first[1]);
    }
}
