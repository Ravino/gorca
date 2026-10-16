//! The terminal policy as a slot of the extension host (ADR-0001 T-2, ADR-0008 tier W3): the
//! session the Reader drives survives a fault of the policy (trap, loop, memory, panic) by
//! starting the next instance over (configure, then a `focus` that reads the screen again), and
//! a disabled slot is no terminal policy at all. `ext-test` plays the policy; it remembers what
//! it was told, so a rebuilt instance shows in its snapshot.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::time::{Duration, Instant};

use common::{BACKENDS, Backend, app, host, info, settle};
use gorca_core::ExtSource;
use gorca_ext_api::terminal::{RawKey, TermAction, TermConfig, TermInput, Unit, Urgency};

fn key(c: char) -> TermInput {
    TermInput::Raw(RawKey {
        keysym: c as u32,
        unichar: c as u32,
        phys: 0,
        shift: false,
        ctrl: false,
        alt: false,
        logo: false,
    })
}

fn said(c: char) -> [TermAction; 2] {
    [
        TermAction::ClaimKey,
        TermAction::Say {
            text: c.to_string(),
            unit: Unit::Char,
            urgency: Urgency::Key,
            sensitive: false,
        },
    ]
}

fn step(s: &mut Box<dyn gorca_core::TermSession>, now: u64, i: TermInput) -> Vec<TermAction> {
    let mut out = Vec::new();
    s.step(now, i, &mut out);
    out
}

fn snapshot_text(s: &dyn gorca_core::TermSession) -> String {
    s.snapshot().map(|x| x.text).unwrap_or_default()
}

fn session(b: Backend) -> (gorca_ext_host::ExtHost, Box<dyn gorca_core::TermSession>) {
    let (h, _sink, _dir) = host(b);
    let ae = h.attach(app(1), &info("gnome-terminal-server")).unwrap();
    let s = ae
        .terminal(&TermConfig::default())
        .expect("ext-test is a terminal policy");
    (h, s)
}

#[test]
fn a_session_drives_the_policy_and_reads_its_snapshot() {
    for b in BACKENDS {
        let (h, mut s) = session(b);
        assert_eq!(step(&mut s, 0, TermInput::Focus), [TermAction::FetchScreen]);
        assert_eq!(step(&mut s, 5, key('a')), said('a'));
        // Configured once, one focus, two steps.
        assert_eq!(snapshot_text(&*s), "steps=2 focuses=1 configs=1", "{b:?}");
        assert_eq!(s.counters().fetches, 1);
        assert_eq!(h.stats().faults, 0);
    }
}

#[test]
fn after_a_fault_the_next_instance_is_configured_and_re_baselined_before_the_key() {
    // The loop trigger only ends in the component (a native loop cannot be pre-empted).
    for b in BACKENDS {
        for crash_key in ['P', 'S', 'M'] {
            if b == Backend::Native && crash_key != 'P' {
                continue;
            }
            let (h, mut s) = session(b);
            step(&mut s, 0, TermInput::Focus);
            step(&mut s, 5, key('a'));
            let t = Instant::now();
            // The faulting key answers nothing (and the policy never reaches its next key).
            assert_eq!(step(&mut s, 10, key(crash_key)), [], "{b:?} {crash_key}");
            assert!(
                t.elapsed() < Duration::from_secs(2),
                "{b:?} {crash_key}: the fault is bounded"
            );
            assert_eq!(h.stats().faults, 1, "{b:?} {crash_key}");
            // Fail closed: the Reader is told the policy is down (typed keys count as secret).
            assert!(!s.alive(), "{b:?} {crash_key}");
            settle(&h);
            // The next key goes to a fresh instance: configure, focus (read the screen again),
            // then the key itself.
            let got = step(&mut s, 20, key('b'));
            let mut want = vec![TermAction::FetchScreen];
            want.extend(said('b'));
            assert_eq!(got, want, "{b:?} {crash_key}");
            assert!(s.alive(), "{b:?} {crash_key}");
            assert_eq!(snapshot_text(&*s), "steps=2 focuses=1 configs=1", "{b:?}");
            // And from then on it is steady.
            assert_eq!(step(&mut s, 30, key('c')), said('c'));
            assert_eq!(h.stats().faults, 1);
        }
    }
}

#[test]
fn three_faults_disable_the_policy_for_the_application() {
    for b in BACKENDS {
        let (h, mut s) = session(b);
        step(&mut s, 0, TermInput::Focus);
        for round in 0..3u64 {
            step(&mut s, 10 + round, key('P'));
            settle(&h);
            // A new instance each time: the next step re-baselines first.
            if round < 2 {
                let got = step(&mut s, 20 + round, key('x'));
                assert!(
                    got.contains(&TermAction::FetchScreen),
                    "{b:?} round {round}: {got:?}"
                );
            }
        }
        assert_eq!(h.stats().disabled, 1, "{b:?}");
        // A disabled slot is no policy: no new session, and the open one answers nothing.
        assert_eq!(step(&mut s, 99, key('y')), [], "{b:?}");
        let ae = h.attach(app(1), &info("gnome-terminal-server")).unwrap();
        assert!(ae.terminal(&TermConfig::default()).is_none(), "{b:?}");
    }
}

#[test]
fn a_blur_of_a_session_that_never_ran_costs_nothing() {
    for b in BACKENDS {
        let (h, mut s) = session(b);
        assert_eq!(step(&mut s, 0, TermInput::Blur), []);
        assert_eq!(h.stats().hook_calls, 0, "{b:?}: nothing was called");
        assert!(s.snapshot().is_none());
    }
}
