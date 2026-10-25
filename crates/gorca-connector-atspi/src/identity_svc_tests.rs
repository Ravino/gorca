// SPDX-License-Identifier: GPL-3.0-or-later
//! Tests of the identity helper and its client.

use super::*;
use crate::identity_client::Client;
use std::process::{Child, Command};

fn serve_in(dir: &Path, client_unit: Option<&str>) -> PathBuf {
    let cfg = ServerConfig {
        socket: dir.join("gorca").join("identity.sock"),
        client_unit: client_unit.map(str::to_owned),
    };
    let listener = bind(&cfg).expect("bind");
    let socket = cfg.socket.clone();
    std::thread::spawn(move || serve(&listener, &cfg));
    socket
}

fn sleeper() -> (Child, u32, u64) {
    let c = Command::new("/usr/bin/sleep")
        .arg("30")
        .spawn()
        .expect("sleep");
    let pid = c.id();
    let start = crate::proc::read_proc_info(pid).start.expect("start");
    (c, pid, start)
}

fn reap(mut c: Child) {
    let _ = c.kill();
    let _ = c.wait();
}

const T: Duration = Duration::from_secs(2);
const SHORT: Duration = Duration::from_millis(150);

/// The helper itself (what `gorca --identity-helper` runs), for the live proof when the daemon
/// binary is not at hand: `idtest --ignored live_helper_server`. Never returns.
#[test]
#[ignore = "runs the helper until killed; used by the transient-unit proof"]
fn live_helper_server() {
    run_helper().expect("helper");
}

/// Read-only live probe for `systemd-run --user` transient units with the full hardening
/// (docs/reports/2026-10-26-linux-platform-identity-helper.md): resolves the identity of
/// `GORCA_PROBE_PID` through the daemon's client exactly as the daemon does and prints the
/// result and the latency. It never writes to the session.
#[test]
#[ignore = "needs a live pid and a helper; run inside a transient unit"]
fn live_helper_identity_probe() {
    let pid: u32 = std::env::var("GORCA_PROBE_PID")
        .expect("GORCA_PROBE_PID")
        .parse()
        .expect("pid");
    let start = crate::proc::read_proc_info(pid).start.expect("start time");
    let t = std::time::Instant::now();
    let id = crate::identity_client::resolve(pid, start, Duration::from_millis(200));
    let first = t.elapsed();
    let t = std::time::Instant::now();
    for _ in 0..100 {
        let _ = crate::identity_client::resolve(pid, start, Duration::from_millis(200));
    }
    if let Ok(list) = std::env::var("GORCA_PROBE_PIDS") {
        // Processes started outside this unit (a unit's own children share its namespaces and
        // are, rightly, unverified): the comma-separated pids are resolved from 64 threads.
        let keys: Vec<(u32, u64)> = list
            .split(',')
            .filter_map(|p| p.parse::<u32>().ok())
            .filter_map(|p| Some((p, crate::proc::read_proc_info(p).start?)))
            .collect();
        let n = keys.len();
        let ok = std::sync::atomic::AtomicUsize::new(0);
        let t = std::time::Instant::now();
        std::thread::scope(|scope| {
            for chunk in keys.chunks(n / 64 + 1) {
                let ok = &ok;
                scope.spawn(move || {
                    for (pid, start) in chunk {
                        if crate::identity_client::resolve(*pid, *start, T).is_some() {
                            ok.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                });
            }
        });
        println!(
            "PROBE burst {n} processes: {} verified in {:?}",
            ok.load(std::sync::atomic::Ordering::Relaxed),
            t.elapsed()
        );
    }
    println!(
        "PROBE private-user-ns={} uses-helper={} identity={id:?} first={first:?} cached-mean-of-100={:?}",
        in_private_user_ns(),
        uses_helper(),
        t.elapsed() / 100
    );
}

#[test]
fn requests_and_replies_round_trip_and_garbage_is_refused() {
    assert_eq!(parse_request("ID 7 12 345\n"), Some((7, 12, 345)));
    for bad in [
        "",
        "ID 1 12\n",
        "ID 1 x 3\n",
        "ID 1 2 3 4\n",
        "id 1 2 3\n",
        "ID 1 -1 2\n",
        "ID 12 345\n",
    ] {
        assert_eq!(parse_request(bad), None, "{bad:?}");
    }
    assert_eq!(
        parse_reply("OK 3 firefox-esr\n"),
        Some((3, Some("firefox-esr".into())))
    );
    assert_eq!(
        parse_reply("OK 3 my app\n"),
        Some((3, Some("my app".into())))
    );
    assert_eq!(parse_reply("NO 9\n"), Some((9, None)));
    for bad in [
        "OK 3 \n", "NO\n", "OK 3 x", "", "OKx\n", "OK x y\n", "NO x\n",
    ] {
        assert_eq!(parse_reply(bad), None, "{bad:?}");
    }
    assert_eq!(format_reply(4, Some("a\nb")), "NO 4\n");
    assert_eq!(format_reply(4, None), "NO 4\n");
    assert_eq!(format_reply(4, Some("x")), "OK 4 x\n");
}

#[test]
fn the_helper_answers_with_the_same_identity_as_a_local_check() {
    let dir = tempfile::tempdir().expect("dir");
    let socket = serve_in(dir.path(), None);
    let (c, pid, start) = sleeper();
    assert_eq!(ask(&socket, None, pid, start, T), Ok(Some("sleep".into())));
    assert_eq!(verified_identity(pid, start).as_deref(), Some("sleep"));
    // A recycled pid (wrong start time) is unverified.
    assert_eq!(ask(&socket, None, pid, start + 1, T), Ok(None));
    reap(c);
}

#[test]
fn a_copy_in_a_user_directory_is_unverified_through_the_helper_too() {
    let dir = tempfile::tempdir().expect("dir");
    let socket = serve_in(dir.path(), None);
    let copy = dir.path().join("firefox-esr");
    std::fs::copy("/usr/bin/sleep", &copy).expect("copy");
    // A parallel test's fork can still hold the freshly written file open (ETXTBSY): retry.
    let c = (0..50)
        .find_map(|_| {
            Command::new(&copy).arg("30").spawn().ok().or_else(|| {
                #[allow(clippy::disallowed_methods)]
                std::thread::sleep(Duration::from_millis(20));
                None
            })
        })
        .expect("spawn");
    let (pid, start) = (
        c.id(),
        crate::proc::read_proc_info(c.id()).start.expect("start"),
    );
    assert_eq!(ask(&socket, None, pid, start, T), Ok(None));
    reap(c);
}

#[test]
fn pipelined_requests_on_one_connection_are_answered_by_tag() {
    use std::io::Write as _;
    let dir = tempfile::tempdir().expect("dir");
    let socket = serve_in(dir.path(), None);
    let (c, pid, start) = sleeper();
    let mut conn = UnixStream::connect(&socket).expect("connect");
    let mut reader = BufReader::new(conn.try_clone().expect("clone"));
    // Three requests in one write, then a line that is not a request.
    conn.write_all(
        format!(
            "ID 10 {pid} {start}\nID 11 {pid} {}\nID 12 {pid} {start}\ngarbage\n",
            start + 1
        )
        .as_bytes(),
    )
    .expect("write");
    let mut got = Vec::new();
    for _ in 0..4 {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read");
        got.push(line);
    }
    got.sort();
    assert_eq!(got, ["NO\n", "NO 11\n", "OK 10 sleep\n", "OK 12 sleep\n"]);
    reap(c);
}

#[test]
fn the_helper_serves_only_the_configured_unit() {
    let dir = tempfile::tempdir().expect("dir");
    // The test process runs in some unit (or none); a unit name it cannot have is refused.
    let socket = serve_in(dir.path(), Some("not-gorca.service"));
    let (c, pid, start) = sleeper();
    assert_eq!(
        ask(&socket, None, pid, start, SHORT),
        Err(HelperError::Protocol)
    );
    reap(c);
}

#[test]
fn a_server_in_the_wrong_unit_is_not_trusted_and_a_missing_one_is_unreachable() {
    let dir = tempfile::tempdir().expect("dir");
    let socket = serve_in(dir.path(), None);
    assert_eq!(
        ask(&socket, Some("gorca-identity.service"), 1, 1, T),
        Err(HelperError::Untrusted)
    );
    assert_eq!(
        ask(&dir.path().join("none.sock"), None, 1, 1, SHORT),
        Err(HelperError::Unreachable)
    );
}

#[test]
fn the_socket_and_its_directory_are_private_and_a_stale_socket_is_replaced() {
    let dir = tempfile::tempdir().expect("dir");
    let cfg = ServerConfig {
        socket: dir.path().join("gorca").join("identity.sock"),
        client_unit: None,
    };
    drop(bind(&cfg).expect("first"));
    let _second = bind(&cfg).expect("second replaces the stale socket");
    let mode = |p: &Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
    assert_eq!(mode(&cfg.socket), 0o600);
    assert_eq!(mode(cfg.socket.parent().expect("dir")), 0o700);
    // A regular file on the path is not ours to delete.
    let other = dir.path().join("gorca").join("file");
    std::fs::write(&other, b"x").expect("write");
    let blocked = ServerConfig {
        socket: other.clone(),
        client_unit: None,
    };
    assert!(bind(&blocked).is_err());
    assert!(other.exists());
}

#[test]
fn paths_and_units_come_from_the_environment() {
    let env = |k: &str| match k {
        "XDG_RUNTIME_DIR" => Some("/run/user/1000".to_owned()),
        _ => None,
    };
    assert_eq!(
        socket_path_from(env),
        Some(PathBuf::from("/run/user/1000/gorca/identity.sock"))
    );
    assert_eq!(
        unit_from(&env, ENV_CLIENT_UNIT, DAEMON_UNIT).as_deref(),
        Some(DAEMON_UNIT)
    );
    let off = |k: &str| (k == ENV_CLIENT_UNIT).then(String::new);
    assert_eq!(unit_from(&off, ENV_CLIENT_UNIT, DAEMON_UNIT), None);
    assert_eq!(socket_path_from(|_| None), None);
}

/// The owner's scaling requirement: a start-up burst of many applications is all verified, over
/// one connection, within a bounded time. Prints the measured time (see the report).
#[test]
fn a_startup_burst_of_200_applications_is_verified_within_a_bounded_time() {
    const N: usize = 200;
    let dir = tempfile::tempdir().expect("dir");
    let socket = serve_in(dir.path(), None);
    let apps: Vec<(Child, u32, u64)> = (0..N).map(|_| sleeper()).collect();
    let client = Arc::new(Client::new(Some(socket), None));
    let t = std::time::Instant::now();
    // 64 callers at once, as the blocking pool of the daemon would issue them.
    let keys: Vec<(u32, u64)> = apps.iter().map(|(_, p, s)| (*p, *s)).collect();
    let verified = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for chunk in keys.chunks(N / 64 + 1) {
            let (client, verified) = (&client, &verified);
            scope.spawn(move || {
                for (pid, start) in chunk {
                    if client.resolve(*pid, *start, T).as_deref() == Some("sleep") {
                        verified.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            });
        }
    });
    let took = t.elapsed();
    println!("BURST {N} applications verified in {took:?}");
    assert_eq!(verified.load(std::sync::atomic::Ordering::Relaxed), N);
    assert!(took < Duration::from_secs(2), "took {took:?}");
    // The same applications again: all from the cache, no wire traffic needed.
    let t = std::time::Instant::now();
    for (pid, start) in &keys {
        assert_eq!(client.resolve(*pid, *start, T).as_deref(), Some("sleep"));
    }
    println!("BURST second pass (cached) {:?}", t.elapsed());
    for (c, _, _) in apps {
        reap(c);
    }
}

#[test]
fn one_process_costs_one_lookup_and_forget_drops_it() {
    let dir = tempfile::tempdir().expect("dir");
    let socket = serve_in(dir.path(), None);
    let (c, pid, start) = sleeper();
    let client = Arc::new(Client::new(Some(socket), None));
    // Fifty windows of one application at once: all get the answer.
    let answers: Vec<Option<String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..50)
            .map(|_| {
                let client = &client;
                scope.spawn(move || client.resolve(pid, start, T))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("join"))
            .collect()
    });
    assert!(answers.iter().all(|a| a.as_deref() == Some("sleep")));
    // Once the process is gone the cached answer is still served until it is forgotten.
    reap(c);
    assert_eq!(client.resolve(pid, start, T).as_deref(), Some("sleep"));
    client.forget(pid, start);
    assert_eq!(client.resolve(pid, start, T), None);
}

#[test]
fn a_failed_lookup_is_not_cached() {
    let dir = tempfile::tempdir().expect("dir");
    let socket = dir.path().join("gorca").join("identity.sock");
    let (c, pid, start) = sleeper();
    let client = Client::new(Some(socket.clone()), None);
    // No helper yet: unverified, and not remembered.
    assert_eq!(client.resolve(pid, start, SHORT), None);
    let cfg = ServerConfig {
        socket,
        client_unit: None,
    };
    let listener = bind(&cfg).expect("bind");
    std::thread::spawn(move || serve(&listener, &cfg));
    assert_eq!(client.resolve(pid, start, T).as_deref(), Some("sleep"));
    reap(c);
}
