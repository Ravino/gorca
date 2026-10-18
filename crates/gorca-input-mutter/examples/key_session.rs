// SPDX-License-Identifier: GPL-3.0-or-later
//! Test driver: gorca's real [`MutterKeySource`] alone, for private-shell regression runs
//! (`tools/platform-probes/shell-segfault/regress.py`). Nothing here touches the live session:
//! the session bus address is a required argument.
//!
//! `key_session <session-bus-address>`; commands on stdin, one per line:
//! `chord N` (a grab set with modifier Insert and Ctrl+key N), `all 0|1` (grab-all), `quit`.
//! Logs go to stderr; the source's own `keyboard monitor active` line marks a session start.

use std::io::BufRead;

use gorca_input_api::{Chord, GrabSet, KeySource, KeySourceCtx, Modifiers, key_channel};
use gorca_input_mutter::MutterKeySource;
use gorca_rt::{CancellationToken, Clock, watch};

fn set_for(n: u32, all: bool) -> GrabSet {
    let mut s = GrabSet::default();
    s.modifiers.insert(0xff63);
    s.chords.insert(Chord {
        keysym: 0x61 + (n % 20),
        modifiers: Modifiers::CONTROL,
    });
    s.grab_all = all;
    s
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    let Some(addr) = std::env::args().nth(1) else {
        eprintln!("usage: key_session <session-bus-address>");
        std::process::exit(64);
    };
    let (sink, mut keys) = key_channel();
    let (tx, rx) = watch::channel(set_for(0, false));
    let shutdown = CancellationToken::new();
    let ctx = KeySourceCtx {
        sink,
        grabs: rx,
        shutdown: shutdown.clone(),
        clock: Clock::System,
    };
    let source = Box::new(MutterKeySource::new(addr));
    let run = tokio::spawn(source.run(ctx));
    tokio::spawn(async move { while keys.recv().await.is_some() {} });

    let (line_tx, mut lines) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        for l in std::io::stdin().lock().lines().map_while(Result::ok) {
            if line_tx.send(l).is_err() {
                return;
            }
        }
        let _ = line_tx.send("quit".to_owned());
    });
    let (mut n, mut all) = (0u32, false);
    while let Some(l) = lines.recv().await {
        let mut it = l.split_whitespace();
        match (it.next(), it.next()) {
            (Some("chord"), Some(v)) => n = v.parse().unwrap_or(n),
            (Some("all"), Some(v)) => all = v == "1",
            (Some("quit"), _) => break,
            _ => continue,
        }
        let _ = tx.send(set_for(n, all));
    }
    shutdown.cancel();
    let _ = run.await;
}
