//! `ext-test`: the host's fault-injection and acceptance extension.
//!
//! Benign unless triggered, so it can stay loaded while the latency gates run (M5 acceptance:
//! "M1 latency gates unchanged with extensions loaded"). Triggers:
//!
//! | input | effect |
//! |---|---|
//! | `Announcement("crash")`, node named `gorca-ext-crash`, command 2 | panic (a trap in the component build) |
//! | `Announcement("spin")`, node named `gorca-ext-spin`, command 3 | infinite loop (epoch interruption in the component build) |
//! | `Announcement("oom")`, command 6 | allocate 256 MiB (over the 32 MiB cap) |
//! | `Announcement("spam")`, command 7 | floods the host functions: 1000 `log`, 200 `speak`, 200 `set_setting`, 500 `get_node` |
//! | `Announcement("suppress")` | `Suppress` verdict |
//! | `Announcement("std")` | uses `HashMap`, `Instant` and `SystemTime`; `Rewrite("std ok")` |
//! | `Announcement("rewrite:<text>")` | `Rewrite` verdict speaking `<text>` |
//! | node named `gorca-ext-present` | `present` answers `ext presented` |
//! | command 1 | speaks `ext hello` |
//! | command 4 | counts calls in the settings KV, speaks the count |
//! | command 5 | speaks through the `host` import (`host::speak`) |
//!
//! Depends only on `gorca-ext-api`; the same source builds natively and as a component.

#![allow(clippy::panic)]

use gorca_ext_api::{
    CommandResult, Event, EventVerdict, Hooks, Node, PresentCtx, Prio, Segment, export_extension,
    host,
};

#[derive(Debug, Default)]
pub struct TestExtension {
    events: u64,
    calls: u64,
}

/// Burn CPU until the host interrupts (or forever, natively).
fn spin() -> ! {
    let mut x: u64 = 0;
    loop {
        x = std::hint::black_box(x.wrapping_add(1));
    }
}

fn crash() -> ! {
    panic!("ext-test: deliberate crash");
}

/// A hostile flood of host calls, to check the host's per-call budgets.
fn spam() {
    for i in 0..1000u32 {
        host::log(2, "log flood");
        if i < 200 {
            host::speak(vec![Segment::text("spam")], Prio::Message);
            host::set_setting(&format!("k{i}"), "v");
        }
        if i < 500 {
            let _ = host::get_node((1, u64::from(i)));
        }
    }
}

fn oom() {
    let mut v: Vec<u8> = Vec::new();
    // Touch the pages so the allocation is real and the memory cap is hit.
    v.resize(256 << 20, 1);
    std::hint::black_box(&v);
}

impl Hooks for TestExtension {
    fn on_event(&mut self, ev: &Event) -> EventVerdict {
        self.events += 1;
        let Event::Announcement(text) = ev else {
            return EventVerdict::Pass;
        };
        match text.as_str() {
            "crash" => crash(),
            "spin" => spin(),
            "oom" => {
                oom();
                EventVerdict::Pass
            }
            "spam" => {
                spam();
                EventVerdict::Pass
            }
            "suppress" => EventVerdict::Suppress,
            "std" => {
                // Standard-library features that reach WASI: a randomly seeded HashMap and the
                // clocks. The host answers these (and nothing else ambient).
                let mut m = std::collections::HashMap::new();
                for i in 0..100u32 {
                    m.insert(i, i * 2);
                }
                let t = std::time::Instant::now();
                let wall = std::time::SystemTime::now();
                let ok = m.len() == 100
                    && m.get(&7) == Some(&14)
                    && t.elapsed() < std::time::Duration::from_secs(5)
                    && wall.elapsed().is_ok();
                EventVerdict::Rewrite(vec![Segment::text(if ok { "std ok" } else { "std wrong" })])
            }
            t => match t.strip_prefix("rewrite:") {
                Some(rest) => EventVerdict::Rewrite(vec![Segment::text(rest)]),
                None => EventVerdict::Pass,
            },
        }
    }

    fn present(&mut self, n: &Node, _ctx: PresentCtx) -> Option<Vec<Segment>> {
        match n.name.as_deref() {
            Some("gorca-ext-present") => Some(vec![Segment::text("ext presented")]),
            Some("gorca-ext-crash") => crash(),
            Some("gorca-ext-spin") => spin(),
            _ => None,
        }
    }

    fn command(&mut self, id: u32) -> CommandResult {
        match id {
            1 => CommandResult::Speak(vec![Segment::text("ext hello")]),
            2 => crash(),
            3 => spin(),
            4 => {
                self.calls += 1;
                let prev = host::get_setting("calls")
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                let n = prev.max(self.calls - 1) + 1;
                host::set_setting("calls", &n.to_string());
                CommandResult::Speak(vec![Segment::text(format!("call {n}"))])
            }
            5 => {
                host::speak(vec![Segment::text("ext via host")], Prio::Message);
                CommandResult::Handled
            }
            6 => {
                oom();
                CommandResult::Handled
            }
            7 => {
                spam();
                CommandResult::Handled
            }
            _ => CommandResult::NotHandled,
        }
    }
}

export_extension!(TestExtension);
