// SPDX-License-Identifier: MIT OR Apache-2.0
//! All M18a-0 guests in one source file. The behaviour is chosen at build time by the `GUEST`
//! environment variable (`build.sh` builds one component per name), so every guest is a distinct
//! `.wasm` with its own sha256 and both arms run the same bytes.
//!
//! Group codes (fact groups) used by the harness: 100 author, 101 text, 102 time, 110..119 dummy
//! `k` writes group 110+k, 900 = refresh-like request (the subset has no `requests` interface, so
//! the host reads a fact in group 900 as "refresh this node"), 901 = adversarial patch-match.
wit_bindgen::generate!({ path: "../wit", world: "bakeoff-rules" });

use exports::gorca::ext::rules::{Dirty, Guest, RuleCtx};
use gorca::ext::diag;
use gorca::ext::facts::{self, FactTarget, Layer};
use gorca::ext::model;
use gorca::ext::types::PropValue;
use std::cell::RefCell;

const KIND: &str = match option_env!("GUEST") {
    Some(s) => s,
    None => "signal-p2",
};

thread_local! {
    static STATE: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
    static CALLS: RefCell<u32> = const { RefCell::new(0) };
}

struct G;

static PAD: [u8; 1 << 20] = [7; 1 << 20];

fn assert_text(n: u64, group: u16, s: String) {
    facts::assert(FactTarget::Node(n), group, &PropValue::Text(s), Layer::Inferred, 900, Some(300_000));
}
fn assert_num(n: u64, group: u16, v: u32) {
    facts::assert(FactTarget::Node(n), group, &PropValue::Number(v), Layer::Inferred, 900, Some(300_000));
}

fn signal_p2(batch: &[Dirty]) {
    for d in batch {
        let n = d.node;
        let name = model::name(n).unwrap_or_default();
        let kids = model::children(n, 2);
        let k0 = kids.first().and_then(|k| model::name(*k)).unwrap_or_default();
        assert_text(n, 100, name);
        assert_text(n, 101, k0);
        assert_num(n, 102, kids.len() as u32);
    }
}

/// Dummy `k` (1..=9). Guest time on the fast core is roughly 2..60 us for a 1..8 node batch.
fn dummy(k: u32, batch: &[Dirty]) {
    let g = 110 + k as u16;
    for d in batch {
        let n = d.node;
        match k {
            1 => assert_num(n, g, 1),
            2 => assert_text(n, g, model::name(n).unwrap_or_default()),
            3 => {
                let nm = model::name(n).unwrap_or_default();
                let mut s = String::new();
                for i in 0..6 {
                    s.push_str(&format!("{}:{}|", i, nm));
                }
                s.truncate(900);
                assert_text(n, g, s);
            }
            4 => {
                let mut acc = 0u64;
                for _ in 0..5 {
                    acc += model::role(n).unwrap_or(0) as u64;
                    acc += model::states(n);
                    acc += model::parent(n).unwrap_or(0);
                    acc += model::prop(n, 1).is_some() as u64;
                }
                assert_num(n, g, acc as u32);
            }
            5 => {
                let sub = model::subtree(n, 2, 16);
                let mut c = 0u32;
                for v in &sub {
                    c = c.wrapping_add(v.role as u32 + v.child_count);
                }
                assert_num(n, g, c);
            }
            6 => {
                let t = model::text(n, 0, 64).unwrap_or_default();
                let h = t.bytes().fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
                assert_num(n, g, h);
            }
            7 => {
                let c = model::attr(n, "class").unwrap_or_default();
                assert_text(n, g, c);
            }
            8 => {
                let s = model::states(n);
                let r = model::role(n).unwrap_or(0);
                assert_num(n, g, (s as u32) ^ r as u32);
            }
            _ => {
                let m = model::matches(n, 1);
                let kids = model::select(n, 2, 8);
                assert_num(n, g, m as u32 + kids.len() as u32);
            }
        }
    }
}

fn a1_page_stride() {
    // 3 MiB leaked per call: touch one byte per 4 KiB page of fresh memory (page faults are not
    // fuel); after a few calls memory.grow hits the 16 MiB instance cap and the allocator aborts.
    let mut v: Vec<u8> = Vec::with_capacity(3 << 20);
    let p = v.as_mut_ptr();
    let mut i = 0usize;
    while i < (3 << 20) {
        // SAFETY: i < capacity, the allocation is live.
        unsafe { p.add(i).write_volatile(1) };
        i += 4096;
    }
    std::mem::forget(v);
}

fn a2_pointer_chase() {
    // 12 MiB table, content-dependent random index: every step is a dependent cache miss.
    const N: usize = 3 << 20;
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.is_empty() {
            *s = vec![0x9E37_79B9u32; N];
        }
        let mut idx = 12345usize;
        let mut acc = 0u32;
        for i in 0..150_000u32 {
            let x = s[idx];
            acc = acc.wrapping_add(x);
            idx = (x.wrapping_mul(2_654_435_761).wrapping_add(i.wrapping_mul(40503)) as usize) & (N - 1);
            if (i & 7) == 0 {
                s[idx] = acc;
            }
        }
        std::hint::black_box(acc);
    });
}

fn big_string() -> String {
    // 14 MiB: about the largest string a guest with a 16 MiB memory cap can build (SPEC says 16 MiB; the cap itself forbids one).
    // SAFETY: all bytes are ASCII 'x'.
    unsafe { String::from_utf8_unchecked(vec![b'x'; 14 << 20]) }
}

impl Guest for G {
    fn attach(_ctx: RuleCtx) {}

    fn derive(_ctx: RuleCtx, batch: Vec<Dirty>) {
        let b = &batch[..];
        match KIND {
            "signal-p2" => signal_p2(b),
            "dummy-1" => dummy(1, b),
            "dummy-2" => dummy(2, b),
            "dummy-3" => dummy(3, b),
            "dummy-4" => dummy(4, b),
            "dummy-5" => dummy(5, b),
            "dummy-6" => dummy(6, b),
            "dummy-7" => dummy(7, b),
            "dummy-8" => dummy(8, b),
            "dummy-9" => dummy(9, b),
            "a1-page-stride" => a1_page_stride(),
            "a2-pointer-chase" => a2_pointer_chase(),
            "a3-huge-string" => diag::log(1, &big_string()),
            "a3b-huge-assert" => {
                if let Some(d) = b.first() {
                    assert_text(d.node, 100, big_string());
                }
            }
            "a3c-huge-attr" => {
                if let Some(d) = b.first() {
                    let _ = model::attr(d.node, &big_string());
                }
            }
            "a4-huge-return" => {
                if let Some(d) = b.first() {
                    for i in 0..100_000u32 {
                        assert_num(d.node, 100, i);
                    }
                }
            }
            "a5-cold-load" => {
                // Benign; a 1 MiB data segment makes its .cwasm big enough for a cold-cache test.
                for d in b {
                    assert_num(d.node, 111, PAD[(d.node as usize) & 0xF_FFFF] as u32);
                }
            }
            "a6-full-grant" => {
                // Spend whatever grant is given: never returns on its own.
                let mut x = 1u64;
                loop {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    std::hint::black_box(x);
                }
            }
            "a13-subtree-loop" => {
                if let Some(d) = b.first() {
                    for _ in 0..100_000u32 {
                        std::hint::black_box(model::subtree(d.node, 8, 255));
                    }
                }
            }
            "a7-patch-match" => {
                for d in b {
                    // Facts on groups other plugins' selectors might read: must re-trigger nobody.
                    assert_num(d.node, 901, 1);
                    assert_num(d.node, 100, 2);
                }
            }
            "a8-refresh-each" => {
                for d in b {
                    assert_num(d.node, 900, 1);
                }
            }
            "a12-self" => {
                for d in b {
                    let c = CALLS.with(|c| {
                        *c.borrow_mut() += 1;
                        *c.borrow()
                    });
                    assert_num(d.node, 950, c);
                }
            }
            _ => {}
        }
    }

    fn removed(_nodes: Vec<u64>) {}
}

export!(G);
