// SPDX-License-Identifier: GPL-3.0-or-later
//! Interactive pointer client for the private-shell end-to-end tests (`contrib/gnome-shell-extension/
//! testclients/test_rust_client.py`): connects to the session bus at ADDRESS (argument 1), waits for
//! the helper to be usable, then reads commands from stdin and prints one reply line per command.
//!
//! Commands: `avail`, `click L|R|M`, `dclick L|R|M`, `press L|R`, `release L|R`, `releaseall`, `pos`,
//! `quit` (graceful shutdown: the actor releases held buttons).
#![allow(clippy::print_stdout, clippy::print_stderr, clippy::disallowed_methods)]

use std::io::BufRead;
use std::time::{Duration, Instant};

use gorca_output_api::{Button, ButtonAct, PointerAvail, PointerSink};
use gorca_rt::Deadline;
use gorca_shell_helper::ShellHelperClient;

fn button(s: &str) -> Option<Button> {
    match s {
        "L" => Some(Button::Left),
        "R" => Some(Button::Right),
        "M" => Some(Button::Middle),
        _ => None,
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let Some(address) = std::env::args().nth(1) else {
        eprintln!("usage: pointer_cli ADDRESS");
        std::process::exit(2);
    };
    let conn = match zbus::connection::Builder::address(address.as_str()) {
        Ok(b) => match b.method_timeout(Duration::from_secs(10)).build().await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("connect failed: {e}");
                std::process::exit(2);
            }
        },
        Err(e) => {
            eprintln!("bad address: {e}");
            std::process::exit(2);
        }
    };
    let (client, actor) = ShellHelperClient::start(conn);
    let mut rx = client.availability();
    let deadline = Instant::now() + Duration::from_secs(8);
    while !rx.borrow().can_call() && Instant::now() < deadline {
        let _ = tokio::time::timeout(Duration::from_millis(200), rx.changed()).await;
    }
    println!("avail {:?}", *rx.borrow());
    let d = || Deadline(Instant::now() + Duration::from_millis(150));
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let parts: Vec<&str> = line.split_whitespace().collect();
        let reply = match parts.as_slice() {
            ["avail"] => format!("avail {:?}", *rx.borrow()),
            ["pos"] => format!("{:?}", client.position(d()).await),
            ["releaseall"] => format!("{:?}", client.release_all(d()).await),
            [op @ ("click" | "dclick" | "press" | "release"), b] => match button(b) {
                Some(b) => {
                    let act = match *op {
                        "click" => ButtonAct::Click,
                        "dclick" => ButtonAct::DoubleClick,
                        "press" => ButtonAct::Press,
                        _ => ButtonAct::Release,
                    };
                    format!("{:?}", client.button(b, act, d()).await)
                }
                None => "bad button".to_owned(),
            },
            ["quit"] => break,
            _ => "unknown command".to_owned(),
        };
        println!("{reply}");
    }
    client.shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(2), actor).await;
    println!("bye {:?}", PointerAvail::Absent == *rx.borrow());
}
