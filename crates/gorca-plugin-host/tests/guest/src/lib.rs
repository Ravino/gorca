// SPDX-License-Identifier: MIT OR Apache-2.0
//! Test guest for `gorca-plugin-host`: the behaviour of each `derive` call is chosen by the
//! `cause` code of the first dirty node, so one component serves every test.
//!
//! 0 ok: assert one number fact. 1 spin forever. 2 trap (`unreachable`). 3 read a forged handle.
//! 4 read the node name (needs `read-names`). 5 grow memory past any cap. 6 bump an in-guest
//! counter and assert it (instance state). 7 read the role through the real handle. 8 forge 16
//! handles. 9 log 1 MiB (over the hostcall cap). 10 log 1 KiB (over the log cap, under hostcall).
//! 11 speak (needs `speak`).
wit_bindgen::generate!({ path: "../../../../wit/gorca-ext-0.10", world: "app-module" });

use exports::gorca::ext::{commands, reactor, rules};
use gorca::ext::{diag, facts, model, output, types};
use std::cell::Cell;

thread_local! { static COUNTER: Cell<u32> = const { Cell::new(0) }; }

struct G;

fn assert_num(n: u64, group: u16, v: u32) {
    facts::assert(
        facts::FactTarget::Node(n),
        group,
        &types::PropValue::Number(v),
        facts::Layer::Inferred,
        900,
        None,
    );
}

impl rules::Guest for G {
    fn attach(_ctx: rules::RuleCtx) {}

    fn derive(_ctx: rules::RuleCtx, batch: Vec<rules::Dirty>) {
        let Some(d) = batch.first() else { return };
        let n = d.node;
        match d.cause {
            0 => assert_num(n, 1, 1),
            1 => {
                let mut x = 0u64;
                loop {
                    x = x.wrapping_add(1);
                    std::hint::black_box(x);
                }
            }
            2 => core::arch::wasm32::unreachable(),
            3 => {
                let r = model::role(n ^ 1);
                assert_num(n, 3, u32::from(r.is_some()));
            }
            4 => {
                let r = model::name(n);
                assert_num(n, 4, u32::from(r.is_some()));
            }
            5 => {
                let mut v: Vec<u8> = Vec::new();
                v.resize(64 << 20, 1);
                std::hint::black_box(&v);
            }
            6 => {
                let c = COUNTER.with(|c| {
                    c.set(c.get() + 1);
                    c.get()
                });
                assert_num(n, 6, c);
            }
            7 => {
                let r = model::role(n);
                assert_num(n, 7, u32::from(r.unwrap_or(0)));
            }
            8 => {
                for i in 0..16u64 {
                    let _ = model::role(n ^ (i + 1));
                }
            }
            9 => diag::log(1, &"x".repeat(1 << 20)),
            10 => diag::log(1, &"x".repeat(1024)),
            11 => output::say(
                &[output::Part::Text(types::Text::Literal("hi".to_string()))],
                output::Urgency::Message,
            ),
            _ => {}
        }
    }

    fn removed(_nodes: Vec<u64>) {}
}

impl commands::Guest for G {
    fn run(_id: u32, _ctx: commands::CmdCtx) {}
}

impl reactor::Guest for G {
    fn deliver(_id: u32, _r: reactor::Reply) {}
    fn tick(_token: u32) {}
    fn settings_changed(_keys: Vec<String>) {}
}

export!(G);
