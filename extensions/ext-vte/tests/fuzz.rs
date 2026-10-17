//! Randomised typing against a model of a real VTE (rows ending in `\n`, written blanks kept,
//! payloads on inserts and deletes, caret events after the text), with the shell's echo
//! arriving 2 to 25 ms after the key, before or after the key is seen. Every key must be spoken
//! once, by itself, and nothing else may be said (the property of the VT e2e gate, here with
//! many more keys and a controlled clock).
//!
//! The driver is `support/fuzz.rs`, shared with the parity test of the daemon, which runs the
//! same sessions through the native build and the WASM component.
#![allow(clippy::unwrap_used, dead_code)]

#[path = "support/fuzz.rs"]
mod driver;

use ext_vte::VteExtension;
use gorca_ext_api::terminal::TerminalHooks;

fn native() -> Box<dyn TerminalHooks> {
    Box::new(VteExtension::default())
}

#[test]
fn many_random_sessions_say_every_key_once_and_nothing_else() {
    let mut all = Vec::new();
    let only: Option<u64> = std::env::var("FUZZ_SEED").ok().and_then(|v| v.parse().ok());
    for seed in 1..=40u64 {
        if only.is_some_and(|o| o != seed) {
            continue;
        }
        for p in driver::run(seed * 7919, 400, &native).problems {
            all.push(format!("seed {seed}: {p}"));
        }
    }
    assert!(
        all.is_empty(),
        "{} problems, first: {:#?}",
        all.len(),
        &all[..all.len().min(8)]
    );
}
