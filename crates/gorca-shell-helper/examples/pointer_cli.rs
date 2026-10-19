// SPDX-License-Identifier: GPL-3.0-or-later
//! Interactive pointer client for the private-shell end-to-end tests (`contrib/gnome-shell-extension/
//! testclients/test_rust_client.py`): connects to the session bus at ADDRESS (argument 1), waits for
//! the helper to be usable, then reads commands from stdin and prints one reply line per command.
//!
//! Commands: `avail`, `click L|R|M`, `dclick L|R|M`, `press L|R`, `release L|R`, `releaseall`, `pos`,
//! `clickwin|movewin PID|TITLE|OX|OY|W|H|toplevel/device|X|Y[|L/R/M]` (a point in a toplevel's AT-SPI space, matched
//! to a window by the helper client), `quit` (graceful shutdown: the actor releases held buttons).
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::disallowed_methods,
    clippy::cast_possible_truncation
)]

use std::io::BufRead;
use std::time::{Duration, Instant};

use gorca_output_api::{
    Button, ButtonAct, ChildSpace, ClickOpts, PointTarget, PointerAvail, PointerSink, ToplevelHint,
};
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
        // The window specs carry a title that may contain spaces: split them off before the word split.
        let win = line
            .strip_prefix("clickwin ")
            .map(|r| ("clickwin", r))
            .or_else(|| line.strip_prefix("movewin ").map(|r| ("movewin", r)));
        let line_words = if win.is_some() {
            String::new()
        } else {
            line.clone()
        };
        let parts: Vec<&str> = line_words.split_whitespace().collect();
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
            _ if win.is_some() => {
                let (op, spec) = win.unwrap_or_default();
                let f: Vec<&str> = spec.split('|').collect();
                let n = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok());
                match (
                    f.first().and_then(|v| v.parse::<u32>().ok()),
                    n(2),
                    n(3),
                    n(4),
                    n(5),
                    n(7),
                    n(8),
                ) {
                    (Some(pid), Some(ox), Some(oy), Some(w), Some(h), Some(x), Some(y)) => {
                        let top = ToplevelHint {
                            pid,
                            title: std::sync::Arc::from(f.get(1).copied().unwrap_or("")),
                            atspi_origin: (ox as i32, oy as i32),
                            atspi_size: (w as i32, h as i32),
                            child_space: if f.get(6) == Some(&"device") {
                                ChildSpace::DevicePixels
                            } else {
                                ChildSpace::Toplevel
                            },
                            pid_is_proxy: false,
                        };
                        let t = PointTarget::Window { top, x, y };
                        if op == "movewin" {
                            format!("{:?}", client.move_to(t, ClickOpts::default(), d()).await)
                        } else {
                            let b = f.get(9).and_then(|v| button(v)).unwrap_or(Button::Left);
                            format!(
                                "{:?}",
                                client.click_at(t, b, ClickOpts::default(), d()).await
                            )
                        }
                    }
                    _ => "bad window spec".to_owned(),
                }
            }
            ["quit"] => break,
            _ => "unknown command".to_owned(),
        };
        println!("{reply}");
    }
    client.shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(2), actor).await;
    println!("bye {:?}", PointerAvail::Absent == *rx.borrow());
}
