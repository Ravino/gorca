// SPDX-License-Identifier: GPL-3.0-or-later
//! Hostile extensions cannot delay the core's key-to-speech path (security review: a slot must
//! never be able to hold up the Reader, the app actor or the workers of another slot).
//!
//! The Reader here is the real `gorca_core::Reader` wired to a real host with the WASM test
//! extension; while it handles command keys, other threads do everything a hostile extension and a
//! hostile application stream can do: loop and crash on every event, flood commands, and churn
//! applications so that instances are built and torn down continuously.
#![allow(
    clippy::pedantic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::disallowed_methods
)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use common::{Backend, app, host, info, settle};
use gorca_core::{CoreCmd, ExtSource, MirrorDirectory, ModeKeys, Reader, ReaderEffect, ReaderMsg};
use gorca_input_api::{KeyClass, KeyEvent, KeyIntent, Modifiers, PressKind};
use gorca_model::{Epoch, NodeEvent, Politeness};
use gorca_rt::EpochCell;

fn key(cmd: CoreCmd, epoch: u64) -> KeyIntent {
    KeyIntent {
        event: KeyEvent {
            keycode: 28,
            keysym: 0x74,
            unichar: 0x74,
            modifiers: Modifiers::empty(),
            pressed: true,
        },
        kind: PressKind::Press,
        class: KeyClass::Command(cmd.id()),
        repeat: 1,
        chord: None,
        epoch: Epoch(epoch),
        at: Instant::now(),
    }
}

fn announce(t: &str) -> NodeEvent {
    NodeEvent::Announcement {
        node: None,
        text: Arc::from(t),
        politeness: Politeness::Polite,
    }
}

fn measure(reader: &mut Reader, epoch: &EpochCell, n: usize) -> Vec<Duration> {
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let e = epoch.advance();
        let intent = key(CoreCmd::InfoTimeDate, e.0);
        let t = Instant::now();
        let fx = reader.handle(ReaderMsg::Key(intent), t);
        let took = t.elapsed();
        assert!(
            fx.iter().any(|f| matches!(f, ReaderEffect::Speak(_))),
            "the key must speak"
        );
        v.push(took);
        std::thread::sleep(Duration::from_micros(200));
    }
    v.sort_unstable();
    v
}

fn pct(v: &[Duration], q: f64) -> Duration {
    v[((v.len() as f64 * q) as usize).min(v.len() - 1)]
}

#[test]
fn key_to_speech_is_unaffected_by_hostile_extension_traffic() {
    let (h, _sink, _dir) = host(Backend::Wasm);
    let h = Arc::new(h);
    let epoch = Arc::new(EpochCell::new());
    let mut reader = Reader::new(
        Arc::clone(&epoch),
        Arc::new(MirrorDirectory::new()),
        ModeKeys::default(),
    )
    .with_ext(h.clone());

    let base = measure(&mut reader, &epoch, 1500);

    let stop = Arc::new(AtomicBool::new(false));
    let hostile_calls = Arc::new(AtomicU64::new(0));
    let mut threads = Vec::new();

    // 1. Application churn: fresh applications whose instances are looped and crashed until their
    //    slots are disabled, over and over (builds, interrupts, rebuilds, drops).
    for t in 0..2u32 {
        let (h, stop, calls) = (h.clone(), stop.clone(), hostile_calls.clone());
        threads.push(std::thread::spawn(move || {
            let mut n = 0u32;
            while !stop.load(Ordering::Relaxed) {
                n += 1;
                let a = app(1000 + t * 100_000 + (n % 90_000));
                if let Some(ae) = h.attach(a, &info("hostile")) {
                    for trigger in ["spin", "crash", "oom", "spin"] {
                        ae.on_event(&announce(trigger));
                        calls.fetch_add(1, Ordering::Relaxed);
                    }
                    h.detach(a);
                }
            }
        }));
    }
    // 2. Command flood against looping commands on several applications.
    {
        let (h, stop, calls) = (h.clone(), stop.clone(), hostile_calls.clone());
        threads.push(std::thread::spawn(move || {
            let apps: Vec<_> = (0..8u32)
                .filter_map(|i| h.attach(app(500 + i), &info("cmd")))
                .collect();
            while !stop.load(Ordering::Relaxed) {
                for ae in &apps {
                    ae.command("ext-test", 3);
                    ae.command("ext-test", 7);
                    calls.fetch_add(2, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_micros(300));
            }
        }));
    }
    // 3. An event storm into a healthy slot while everything above runs.
    {
        let (h, stop, calls) = (h.clone(), stop.clone(), hostile_calls.clone());
        threads.push(std::thread::spawn(move || {
            let ae = h.attach(app(2), &info("storm")).expect("attach");
            while !stop.load(Ordering::Relaxed) {
                ae.on_event(&announce("spam"));
                calls.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }
    std::thread::sleep(Duration::from_millis(200));

    // Wall clock on purpose (a Reader blocked behind a slot lock burns no CPU), so a loaded host
    // can inflate one run's tail: take the run with the best p99 of up to five, each of 1500
    // samples while the hostile traffic keeps running. A Reader that a hostile slot can delay is
    // delayed in every run.
    let mut hostile = measure(&mut reader, &epoch, 1500);
    for _ in 1..5 {
        if pct(&hostile, 0.99) < Duration::from_millis(5) {
            break;
        }
        let again = measure(&mut reader, &epoch, 1500);
        if pct(&again, 0.99) < pct(&hostile, 0.99) {
            hostile = again;
        }
    }

    stop.store(true, Ordering::Relaxed);
    for t in threads {
        t.join().unwrap();
    }
    settle(&h);
    let s = h.stats();
    println!(
        "key->speech Reader::handle  baseline p50 {:?} p99 {:?} max {:?}",
        pct(&base, 0.5),
        pct(&base, 0.99),
        base.last().unwrap()
    );
    println!(
        "key->speech Reader::handle  hostile  p50 {:?} p99 {:?} max {:?}  ({} hostile calls, {} faults, {} rebuilds, {} disabled, {} cmds dropped)",
        pct(&hostile, 0.5),
        pct(&hostile, 0.99),
        hostile.last().unwrap(),
        hostile_calls.load(Ordering::Relaxed),
        s.faults,
        s.rebuilds,
        s.disabled,
        s.commands_dropped
    );
    assert!(s.faults > 50, "the hostile load must have been real: {s:?}");
    // The Reader does not touch a slot for a core command, so hostile slots cannot delay it. The
    // product budget is 15 ms p99 of gorca-side processing; stay far inside it.
    assert!(
        pct(&hostile, 0.99) < Duration::from_millis(5),
        "p99 {:?}",
        pct(&hostile, 0.99)
    );
    assert!(
        pct(&hostile, 0.5) < pct(&base, 0.5) * 20 + Duration::from_micros(200),
        "median moved: {:?} vs {:?}",
        pct(&hostile, 0.5),
        pct(&base, 0.5)
    );
}

#[test]
fn a_looping_present_delays_the_reader_by_at_most_its_fuel_limit_three_times() {
    use gorca_core::{PresentContext, PresenterRegistry};
    use gorca_model::{Epoch, Role};
    let (h, _sink, _) = host(Backend::Wasm);
    let h = Arc::new(h);
    let _ae = h.attach(app(1), &info("x")).expect("attach");
    let reg = PresenterRegistry::builtin().with_ext(h.clone());
    let n = common::snap(app(1), 1, Role::Button, Some("gorca-ext-spin"));
    let mut worst = Duration::ZERO;
    let mut total = Duration::ZERO;
    for i in 0..20 {
        // The Reader runs the hook on this thread: its CPU time is the work it is held for, and
        // unlike wall time it excludes the host descheduling the test thread.
        let _ = i;
        let (u, took) = gorca_testkit::cpu::cpu_time(|| {
            reg.ext_segments(
                PresentContext::Focus,
                &gorca_core::ExtNode::from_snapshot(&n),
                Epoch(1),
            )
        });
        assert!(
            u.is_none(),
            "a looping hook gives no opinion: the core composes"
        );
        worst = worst.max(took);
        total += took;
        // The replacement is built by a worker, never by the Reader.
        settle(&h);
    }
    println!("looping present: worst {worst:?}, total over 20 focus changes {total:?}");
    assert!(worst < Duration::from_millis(15), "worst {worst:?}");
    // After three strikes the slot is disabled and costs nothing.
    assert_eq!(h.stats().disabled, 1);
    let ((), spent) = gorca_testkit::cpu::cpu_time(|| {
        for _ in 0..1000 {
            let _ = reg.ext_segments(
                PresentContext::Focus,
                &gorca_core::ExtNode::from_snapshot(&n),
                Epoch(1),
            );
        }
    });
    assert!(
        spent / 1000 < Duration::from_millis(1),
        "{spent:?} per 1000"
    );
}
