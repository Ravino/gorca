// SPDX-License-Identifier: GPL-3.0-or-later
//! Process-level checks: the binary starts, ignores SIGUSR1 and exits cleanly (status 0) on
//! SIGINT and SIGTERM.
// Test code: ADR-0001 section 11 rule 6 exempts it from the unwrap/expect/panic lints.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods
)]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct Daemon {
    child: Child,
    lines: mpsc::Receiver<String>,
}

fn start() -> Daemon {
    // The daemon must never reach the real desktop from a test: every bus and speech socket is
    // pointed at a path that does not exist, accessibility is not enabled, and the inherited
    // session variables are removed.
    let mut child = Command::new(env!("CARGO_BIN_EXE_gorca"))
        .args([
            "--no-keyboard",
            "--no-enable",
            "--a11y-address=unix:path=/nonexistent/gorca-test-a11y",
            "--session-address=unix:path=/nonexistent/gorca-test-session",
            "--voicego-socket=/nonexistent/gorca-test-voicego.sock",
            "--ssip-socket=/nonexistent/gorca-test-speechd.sock",
        ])
        // Nothing from the user's environment: no session or a11y bus, no XDG_RUNTIME_DIR, no
        // speech-dispatcher variables. The binary reads its environment only in `main`, and
        // here that environment is empty apart from the log filter.
        .env_clear()
        .env("GORCA_LOG", "info")
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut d = Daemon { child, lines };
    d.wait_for("gorca started");
    d
}

impl Daemon {
    fn wait_for(&mut self, needle: &str) {
        // Generous: the host this runs on is routinely at load 10 to 48; polled, so a quick
        // start returns at once.
        let end = Instant::now() + Duration::from_secs(60);
        while Instant::now() < end {
            if let Ok(line) = self.lines.recv_timeout(Duration::from_millis(100))
                && line.contains(needle)
            {
                return;
            }
        }
        let _ = self.child.kill();
        panic!("did not see `{needle}` on stderr");
    }

    fn signal(&self, sig: &str) {
        let status = Command::new("kill")
            .arg(format!("-{sig}"))
            .arg(self.child.id().to_string())
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn wait_exit(&mut self) -> std::process::ExitStatus {
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end {
            if let Some(s) = self.child.try_wait().unwrap() {
                return s;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        panic!("daemon did not exit within 10 s");
    }
}

#[test]
fn sigint_exits_cleanly() {
    let mut d = start();
    d.signal("INT");
    let status = d.wait_exit();
    assert!(status.success(), "{status:?}");
}

#[test]
fn sigterm_exits_cleanly() {
    let mut d = start();
    d.signal("TERM");
    assert!(d.wait_exit().success());
}

#[test]
fn sigusr1_is_ignored_and_the_daemon_keeps_running() {
    let mut d = start();
    d.signal("USR1");
    d.wait_for("SIGUSR1 received, ignored");
    assert!(
        d.child.try_wait().unwrap().is_none(),
        "SIGUSR1 must not terminate gorca (ID-3)"
    );
    d.signal("INT");
    assert!(d.wait_exit().success());
}
