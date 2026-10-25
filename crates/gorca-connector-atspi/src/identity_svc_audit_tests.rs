// SPDX-License-Identifier: GPL-3.0-or-later
//! Reviewer audit tests for the identity helper (2026-10-26). Each one fails on b41a4be0.
//! Run:
//! `cargo test -p gorca-connector-atspi --lib identity_svc::audit -- `

use super::*;
use std::process::Command;
use std::sync::mpsc;

const T: Duration = Duration::from_secs(2);

fn sleeper() -> (std::process::Child, u32, u64) {
    let c = Command::new("/usr/bin/sleep")
        .arg("30")
        .spawn()
        .expect("sleep");
    let pid = c.id();
    let start = crate::proc::read_proc_info(pid).start.expect("start");
    (c, pid, start)
}

/// IH-1: the doc says "further ones wait in the backlog", but a connection beyond
/// `MAX_CONNECTIONS` is accepted and dropped. The daemon asks once per application and never
/// retries, so in a start-up burst (every running application resolved at once) the overflow
/// stays unverified for the lifetime of its actor.
#[test]

fn audit_a_request_beyond_the_connection_cap_waits_instead_of_being_dropped() {
    let dir = tempfile::tempdir().expect("dir");
    let cfg = ServerConfig {
        socket: dir.path().join("gorca").join("identity.sock"),
        client_unit: None,
    };
    let listener = bind(&cfg).expect("bind");
    let socket = cfg.socket.clone();
    std::thread::spawn(move || serve(&listener, &cfg));
    // Eight legitimate requests in flight (held open 300 ms, as a slow /proc read would).
    let held: Vec<UnixStream> = (0..MAX_CONNECTIONS)
        .map(|_| UnixStream::connect(&socket).expect("connect"))
        .collect();
    std::thread::spawn(move || {
        #[allow(clippy::disallowed_methods)]
        std::thread::sleep(Duration::from_millis(300));
        drop(held);
    });
    #[allow(clippy::disallowed_methods)]
    std::thread::sleep(Duration::from_millis(50));
    let (mut c, pid, start) = sleeper();
    let got = ask(&socket, None, pid, start, T);
    let _ = c.kill();
    let _ = c.wait();
    assert_eq!(
        got,
        Ok(Some("sleep".into())),
        "the ninth request was dropped"
    );
}

/// IH-2: `ask` connects with a blocking `connect(2)` and only then sets its deadline. A helper
/// that stops accepting (frozen unit, SIGSTOP, wedged accept loop) fills its backlog, and every
/// further `connect` blocks forever: the 250 ms tokio timeout in `exe_of_async` returns, but the
/// `spawn_blocking` thread is leaked, one per new application, until the blocking pool is gone.
#[test]

fn audit_ask_honours_its_deadline_when_the_backlog_is_full() {
    let dir = tempfile::tempdir().expect("dir");
    let socket = dir.path().join("frozen.sock");
    let _listener = UnixListener::bind(&socket).expect("bind"); // never accepts
    let fill = socket.clone();
    std::thread::spawn(move || {
        let mut keep = Vec::new();
        loop {
            match UnixStream::connect(&fill) {
                Ok(s) => keep.push(s),
                Err(_) => return,
            }
        }
    });
    #[allow(clippy::disallowed_methods)]
    std::thread::sleep(Duration::from_millis(500));
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(ask(&socket, None, 1, 1, Duration::from_millis(200)));
    });
    assert!(
        rx.recv_timeout(Duration::from_secs(2)).is_ok(),
        "ask blocked in connect() far past its 200 ms deadline"
    );
}

/// IH-3: `gorca-identity.service` is `Type=simple`, so `After=` orders gorca only after the
/// helper's exec, not after its bind. A request in that window is `Unreachable`, and the
/// application actor never asks again (fetch/mod.rs, one `exe_of_async` per actor): the
/// applications already running when gorca starts can stay unverified. A transient error
/// should be retried within the deadline.
#[test]

fn audit_a_helper_that_binds_a_moment_late_is_still_answered() {
    let dir = tempfile::tempdir().expect("dir");
    let cfg = ServerConfig {
        socket: dir.path().join("gorca").join("identity.sock"),
        client_unit: None,
    };
    let socket = cfg.socket.clone();
    std::thread::spawn(move || {
        #[allow(clippy::disallowed_methods)]
        std::thread::sleep(Duration::from_millis(30));
        let listener = bind(&cfg).expect("bind");
        serve(&listener, &cfg);
    });
    let (mut c, pid, start) = sleeper();
    let got = ask(&socket, None, pid, start, Duration::from_millis(200));
    let _ = c.kill();
    let _ = c.wait();
    assert_eq!(got, Ok(Some("sleep".into())));
}

/// Robustness (passes on b41a4be0): random bytes, non-UTF-8, over-long lines, partial lines and
/// abrupt closes over many connections never stop the helper from serving a good request.
#[test]

fn audit_fuzzed_garbage_never_wedges_the_helper() {
    use std::io::{BufRead, BufReader, Write};
    let dir = tempfile::tempdir().expect("dir");
    let cfg = ServerConfig {
        socket: dir.path().join("gorca").join("identity.sock"),
        client_unit: None,
    };
    let listener = bind(&cfg).expect("bind");
    let socket = cfg.socket.clone();
    std::thread::spawn(move || serve(&listener, &cfg));
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let fixed: [&[u8]; 8] = [
        b"ID 1 2",
        b"\xff\xfe\n",
        b"ID 4294967296 1\n",
        b"ID 1 18446744073709551616\n",
        b"ID +1 +2\n",
        b"ID 0 0\n",
        &[b'A'; 70],
        b"\n\n\n\n",
    ];
    for i in 0..400u32 {
        let Ok(mut s) = UnixStream::connect(&socket) else {
            continue;
        };
        let _ = s.set_read_timeout(Some(Duration::from_millis(100)));
        let payload: Vec<u8> = if (i as usize) < fixed.len() {
            fixed[i as usize].to_vec()
        } else {
            let n = (next() % 200) as usize;
            (0..n).map(|_| (next() & 0xff) as u8).collect()
        };
        let _ = s.write_all(&payload);
        if i % 3 == 0 {
            let mut line = String::new();
            let _ = BufReader::new(&s).read_line(&mut line);
            assert!(line.is_empty() || line == "NO\n", "{payload:?} -> {line:?}");
        }
    }
    #[allow(clippy::disallowed_methods)]
    std::thread::sleep(Duration::from_millis(200));
    let (mut c, pid, start) = sleeper();
    let got = ask(&socket, None, pid, start, T);
    let _ = c.kill();
    let _ = c.wait();
    assert_eq!(got, Ok(Some("sleep".into())));
}
