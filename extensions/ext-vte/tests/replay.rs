//! Replay of a REAL gnome-terminal 3.56 / VTE 0.80 session through the terminal policy.
//!
//! `tests/fixtures/gnome-terminal-3.56-vte-0.80-session.jsonl` was recorded by
//! `tests/term/vte_probe.py` in a private headless session (see `tests/term/README.md`): the
//! AT-SPI events of a real `gnome-terminal-server`, the keys the probe pressed through a Mutter
//! RemoteDesktop session, and `GetText(0, -1)` snapshots after each phase. The test rebuilds the
//! screen from the events (and checks it against every recorded snapshot, so the replay is
//! faithful), feeds the extension exactly what the host would, answers its screen reads with the
//! rebuilt screen, and compares what it says with what a user should hear.
//!
//! Two deliveries are replayed: the raw events, and the events as the ingress T-1 collapse hands
//! them to the Reader (a delete followed at once by an insert becomes one `Replaced`).
//!
//! The driver is `support/replay.rs`, shared with the parity test of the daemon, which replays the
//! same session through the native build and the WASM component.

#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

#[path = "support/replay.rs"]
mod driver;

use driver::{SESSION, during, run, texts};
use ext_vte::VteExtension;
use gorca_ext_api::terminal::TerminalHooks;

fn native() -> Box<dyn TerminalHooks> {
    Box::new(VteExtension::default())
}

fn check(collapse: bool) {
    let r = run(SESSION, collapse, &native);
    let said = |m: &str| texts(&during(&r, m));
    // Six typed keys: the key says each character once, the shell echo says nothing.
    assert_eq!(said("type echo hi"), ["e", "c", "h", "o", " ", "h", "i"]);
    // BackSpace says the character it removes.
    assert_eq!(said("backspace x2"), ["i", "h"]);
    // Left, Left, Home, End: the character at the caret each time (the first is the blank that
    // VTE keeps at the end of the line).
    assert_eq!(said("left x2, home, end"), [" ", "o", "e", "blank"]);
    // Enter runs `echo` (an empty line) and shows the next prompt: the prompt is said, once.
    assert_eq!(said("enter"), ["$"]);
    // "ec" and Tab: the typed keys, then the completed word.
    assert_eq!(said("ec tab"), ["e", "c", "echo"]);
    // Ctrl+U deletes the line: what went is read.
    assert_eq!(said("ctrl-u"), ["echo"]);
    // Up recalls the previous command, Down goes back to the empty line.
    assert_eq!(said("history up"), ["echo", "blank"]);
    // `seq 1 40`: eight typed keys, then the numbers that scrolled in, in order, with the prompt.
    let seq = said("seq 1 40");
    assert_eq!(&seq[..8], ["s", "e", "q", " ", "1", " ", "4", "0"]);
    let out = seq[8..].join("\n");
    assert!(out.contains("40") && out.contains('$'), "{out}");
    let ev = r.ext.counters();
    assert!(ev.typed_explained >= 10, "{ev:?}");
}

#[test]
fn a_real_vte_session_is_read_right_from_the_raw_events() {
    check(false);
}

#[test]
fn a_real_vte_session_is_read_right_after_the_t1_collapse() {
    check(true);
}
