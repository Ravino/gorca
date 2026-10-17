// SPDX-License-Identifier: GPL-3.0-or-later
//! Representative per-engine quirk normalisation, shared verbatim by the native baseline and the
//! WASM guest so both arms run the same code. Engine 1 = Gecko, 2 = Blink.
#![no_std]
extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    ChildrenAdded,
    ChildrenRemoved,
    TextInserted,
    TextRemoved,
    CaretMoved,
    AttrsChanged,
    StateChanged,
    Reorder,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delta {
    pub kind: Kind,
    pub node: (u64, u64),
    pub parent: (u64, u64),
    pub a: i32,
    pub b: i32,
    pub flags: u32,
    pub text: Option<String>,
}

pub const F_FOCUSED: u32 = 1;
pub const F_UTF16: u32 = 2;
pub const F_STATIC_TEXT: u32 = 4;
pub const F_REVALIDATE: u32 = 8;

pub const ENGINE_GECKO: u8 = 1;
pub const ENGINE_BLINK: u8 = 2;

pub const V_PASS: u8 = 0;
pub const V_DROP: u8 = 1;
pub const V_REVALIDATE: u8 = 2;
pub const V_FIXED: u8 = 3;

/// Fixes one delta in place and returns its verdict.
#[inline]
pub fn fix(engine: u8, d: &mut Delta) -> u8 {
    // Generic: index -1 / null child, inverted ranges.
    if d.a < 0 && matches!(d.kind, Kind::ChildrenAdded | Kind::ChildrenRemoved) {
        return V_DROP;
    }
    if d.b < d.a {
        core::mem::swap(&mut d.a, &mut d.b);
    }
    match engine {
        ENGINE_GECKO => {
            // UTF-16 offsets on text deltas: recompute the end from the payload in code points.
            if d.flags & F_UTF16 != 0
                && let (Kind::TextInserted, Some(t)) = (d.kind, d.text.as_ref())
            {
                let cps = t.chars().count() as i32;
                d.b = d.a + cps;
                d.flags &= !F_UTF16;
                return V_FIXED;
            }
            V_PASS
        }
        ENGINE_BLINK => match d.kind {
            // caret(0) on a non-focused node is scroll-to-anchor, not a caret move.
            Kind::CaretMoved if d.a == 0 && d.flags & F_FOCUSED == 0 => V_DROP,
            // Static text arrives as children-changed: treat as a text change of the parent.
            Kind::ChildrenAdded if d.flags & F_STATIC_TEXT != 0 => {
                d.kind = Kind::TextInserted;
                d.node = d.parent;
                V_FIXED
            }
            // Silent reorders: ask for parent revalidation.
            Kind::Reorder => {
                d.flags |= F_REVALIDATE;
                V_REVALIDATE
            }
            _ => V_PASS,
        },
        _ => V_PASS,
    }
}

pub fn normalize(engine: u8, ds: Vec<Delta>) -> Vec<Delta> {
    let mut out = Vec::with_capacity(ds.len());
    for mut d in ds {
        if fix(engine, &mut d) != V_DROP {
            out.push(d);
        }
    }
    out
}

/// Per-key verdict: a pass-through predicate over a node (role/state bits).
#[inline]
pub fn on_key(key: u32, role: u8, states: u64) -> Option<bool> {
    if role == 0x2a && states & 0x10 != 0 && key == 0x41 {
        Some(true)
    } else {
        None
    }
}
