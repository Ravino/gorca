//! Shared helpers for the conformance tests.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::disallowed_methods
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_bench_b2::bus::{Bus, BusKind};
use gorca_bench_b2::clock::Rng;
use gorca_bench_b2::mockbus::{Link, MockOpts};
use gorca_bench_b2::model::Ev;
use gorca_bench_b2::rig::{ConsumerCfg, ConsumerResult, HarnessRouter};
use gorca_dbus::drain::{DrainConfig, DrainHandle, spawn_drain};
use gorca_model::AppId;

/// A mock bus with a drain connected to it and one registered app.
pub struct Fixture {
    pub bus: Bus,
    pub router: Arc<HarnessRouter>,
    pub drain: DrainHandle,
    pub link: Arc<Link>,
    pub app: AppId,
    pub sender: String,
}

impl Fixture {
    pub fn start(opts: MockOpts, cons: &ConsumerCfg) -> Self {
        gorca_bench_b2::clock::init();
        let bus = Bus::start(BusKind::Mock(opts)).unwrap();
        let router = HarnessRouter::new();
        let sender = ":1.200".to_owned();
        let app = router.register(&sender, "app", cons);
        let drain = spawn_drain(
            DrainConfig::new(bus.address.clone()),
            Arc::clone(&router) as _,
        )
        .unwrap();
        let link = bus
            .mock()
            .unwrap()
            .next_link(Duration::from_secs(5))
            .unwrap();
        Self {
            bus,
            router,
            drain,
            link,
            app,
            sender,
        }
    }

    /// Wait for the drain to have framed `n` messages in total.
    pub fn wait_messages(&self, n: u64, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if self.drain.stats().messages >= n {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }

    pub fn finish(self) -> (ConsumerResult, gorca_dbus::drain::DrainStats) {
        self.link.flush_all(Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(100));
        let stats = self.drain.stop();
        let cons = self.router.finish("app").unwrap();
        (cons, stats)
    }
}

/// Random canonical events over a small node space (many collisions, so coalescing happens).
pub fn random_script(rng: &mut Rng, n: usize, nodes: u64) -> Vec<Ev> {
    let mut out = Vec::with_capacity(n);
    for i in 0..n as u32 {
        let node = rng.below(nodes);
        let ev = match rng.below(13) {
            0 => Ev::Focus(node),
            1 => Ev::Active(
                node,
                if rng.below(2) == 0 {
                    Some(rng.below(nodes))
                } else {
                    None
                },
            ),
            2 => Ev::Props(node, 1 << rng.below(9)),
            3 => Ev::Children(node),
            4 => Ev::TextIns(node, i, 1),
            5 => Ev::TextDel(node, i, 1),
            6 => Ev::TextReplaced(node),
            7 => Ev::Caret(node, i),
            8 => Ev::Selection(node),
            9 => Ev::Announce(Some(node), format!("a{i}"), rng.below(2) == 0),
            10 => Ev::Window(node, rng.below(4) as u8),
            11 => Ev::Doc(node, rng.below(4) as u8),
            _ => Ev::Removed(node),
        };
        out.push(ev);
    }
    out
}
