// SPDX-License-Identifier: GPL-3.0-or-later
//! The identity helper: application identity for a gorca that runs in a private user namespace
//! (ADR-0026 section 2.7, 2026-10-25 note).
//!
//! A systemd *user* unit with `PrivateTmp=`, `ProtectSystem=`, `ProtectKernelTunables=`,
//! `ProtectKernelModules=`, `ProtectControlGroups=` (and `ProtectClock=`, `PrivateUsers=`, ...)
//! runs in a user namespace of its own, and then the kernel refuses `/proc/<pid>/{exe,root,ns/*,
//! environ,maps}` of every process outside it. Identity needs `exe`. So the sandboxed daemon asks
//! a tiny helper that runs in the initial namespaces (the same binary: `gorca --identity-helper`,
//! its own unit with hardening that creates no namespace) and that applies exactly the checks of
//! [`crate::proc::verified_identity`].
//!
//! Wire protocol on a `SOCK_STREAM` Unix socket (`$XDG_RUNTIME_DIR/gorca/identity.sock`, directory
//! 0700, socket 0600), one line each way, many requests per connection allowed:
//!
//! ```text
//! -> ID <pid> <start-ticks>\n
//! <- OK <identity>\n     (verified; the identity may contain spaces, never a newline)
//! <- NO\n                (unverified, unknown or recycled pid, malformed identity)
//! ```
//!
//! Both ends check the other with `SO_PEERCRED` and the systemd unit in `/proc/<pid>/cgroup`
//! (readable from inside any namespace): the helper serves `gorca.service` only, the client
//! trusts an answer only from `gorca-identity.service`. A process of the same user that moves
//! itself into those cgroups is the same-user adversary the design does not defend against
//! (ADR-0026 section 2.7); the check closes the cheap case of a look-alike server squatting on the
//! socket path while the helper is down. The client fails closed: no answer is `None`
//! (unverified), never a weaker in-sandbox guess.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use crate::proc::{in_private_user_ns, parse_cgroup, verified_identity};

/// The systemd unit of the helper.
pub const HELPER_UNIT: &str = "gorca-identity.service";
/// The systemd unit of the daemon.
pub const DAEMON_UNIT: &str = "gorca.service";

/// Environment overrides (tests and transient units): socket path, the unit the helper serves
/// and the unit the client expects as the helper. An empty unit disables that check.
pub const ENV_SOCKET: &str = "GORCA_IDENTITY_SOCK";
pub const ENV_CLIENT_UNIT: &str = "GORCA_IDENTITY_CLIENT_UNIT";
pub const ENV_SERVER_UNIT: &str = "GORCA_IDENTITY_SERVER_UNIT";

/// Concurrent connections the helper serves; further ones wait in the backlog.
const MAX_CONNECTIONS: usize = 8;
/// Idle time after which the helper drops a connection.
const IDLE: Duration = Duration::from_secs(5);
/// A request line is `ID <u32> <u64>\n`: well under this.
const MAX_LINE: u64 = 64;

/// Where the helper listens: `$GORCA_IDENTITY_SOCK`, else `$XDG_RUNTIME_DIR/gorca/identity.sock`.
pub fn socket_path_from(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(p) = env(ENV_SOCKET).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let dir = env("XDG_RUNTIME_DIR").filter(|d| !d.is_empty())?;
    Some(Path::new(&dir).join("gorca").join("identity.sock"))
}

fn unit_from(env: &impl Fn(&str) -> Option<String>, key: &str, default: &str) -> Option<String> {
    match env(key) {
        Some(v) if v.is_empty() => None,
        Some(v) => Some(v),
        None => Some(default.to_owned()),
    }
}

/// What the helper serves.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// The socket to bind.
    pub socket: PathBuf,
    /// Serve only a client in this systemd unit; `None` serves any client of the user.
    pub client_unit: Option<String>,
}

impl ServerConfig {
    /// From the environment (`None` without a runtime directory).
    pub fn from_env(env: impl Fn(&str) -> Option<String>) -> Option<Self> {
        Some(Self {
            socket: socket_path_from(&env)?,
            client_unit: unit_from(&env, ENV_CLIENT_UNIT, DAEMON_UNIT),
        })
    }
}

/// Bind the socket (directory 0700, stale socket replaced, socket 0600).
pub fn bind(cfg: &ServerConfig) -> std::io::Result<UnixListener> {
    if let Some(dir) = cfg.socket.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    // Replace a stale socket of an earlier run, but nothing else that sits on the path.
    if std::fs::symlink_metadata(&cfg.socket).is_ok_and(|m| m.file_type().is_socket()) {
        std::fs::remove_file(&cfg.socket)?;
    }
    let listener = UnixListener::bind(&cfg.socket)?;
    std::fs::set_permissions(&cfg.socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Serve forever on a bound listener. Never returns while the listener works.
pub fn serve(listener: &UnixListener, cfg: &ServerConfig) {
    static ACTIVE: AtomicUsize = AtomicUsize::new(0);
    for conn in listener.incoming() {
        let Ok(conn) = conn else {
            // A failed accept (descriptor limit, aborted connection): back off, keep serving.
            // The helper is a plain thread-per-connection process, not an async actor.
            #[allow(clippy::disallowed_methods)]
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        if ACTIVE.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            ACTIVE.fetch_sub(1, Ordering::AcqRel);
            drop(conn);
            continue;
        }
        let cfg = cfg.clone();
        let spawned = std::thread::Builder::new()
            .name("identity-conn".into())
            .spawn(move || {
                handle(&conn, &cfg);
                ACTIVE.fetch_sub(1, Ordering::AcqRel);
            });
        if spawned.is_err() {
            ACTIVE.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// The unit of the process at the other end of `conn`.
fn peer_unit(conn: &UnixStream) -> Option<String> {
    let pid = gorca_dbus::peer_pid(conn).ok()?;
    parse_cgroup(&std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?)
}

fn handle(conn: &UnixStream, cfg: &ServerConfig) {
    if let Some(want) = &cfg.client_unit
        && peer_unit(conn).as_deref() != Some(want.as_str())
    {
        return;
    }
    if conn.set_read_timeout(Some(IDLE)).is_err() || conn.set_write_timeout(Some(IDLE)).is_err() {
        return;
    }
    let mut reader = BufReader::new(conn.take(MAX_LINE));
    let mut out = conn;
    loop {
        let mut line = String::new();
        reader.get_mut().set_limit(MAX_LINE);
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if !line.ends_with('\n') {
            return;
        }
        let reply = match parse_request(&line) {
            Some((pid, start)) => format_reply(verified_identity(pid, start).as_deref()),
            None => "NO\n".to_owned(),
        };
        if out.write_all(reply.as_bytes()).is_err() {
            return;
        }
    }
}

/// `ID <pid> <start>\n` to `(pid, start)`.
pub fn parse_request(line: &str) -> Option<(u32, u64)> {
    let mut it = line.trim_end_matches('\n').split(' ');
    (it.next()? == "ID").then_some(())?;
    let pid = it.next()?.parse().ok()?;
    let start = it.next()?.parse().ok()?;
    it.next().is_none().then_some((pid, start))
}

fn format_reply(identity: Option<&str>) -> String {
    match identity {
        Some(id) if !id.is_empty() && !id.contains(['\n', '\r', '\0']) => format!("OK {id}\n"),
        _ => "NO\n".to_owned(),
    }
}

/// `OK <identity>\n` / `NO\n` to the identity; `None` for `NO` and for anything malformed.
pub fn parse_reply(line: &str) -> Option<String> {
    let body = line.strip_suffix('\n')?;
    let id = body.strip_prefix("OK ")?;
    (!id.is_empty() && !id.contains(['\r', '\0'])).then(|| id.to_owned())
}

/// Why the helper gave no answer.
#[derive(Debug, Eq, PartialEq)]
pub enum HelperError {
    /// Not running, or no socket: the unit is not installed or not started.
    Unreachable,
    /// A server answered, but it is not the helper's unit.
    Untrusted,
    /// Timeout, closed connection or a malformed reply.
    Protocol,
}

/// Ask the helper at `socket` for the identity of `(pid, start)`. With `server_unit` the server
/// must be in that systemd unit. `Ok(None)` is the helper's answer "unverified".
pub fn ask(
    socket: &Path,
    server_unit: Option<&str>,
    pid: u32,
    start: u64,
    timeout: Duration,
) -> Result<Option<String>, HelperError> {
    let conn = UnixStream::connect(socket).map_err(|_| HelperError::Unreachable)?;
    if let Some(want) = server_unit
        && peer_unit(&conn).as_deref() != Some(want)
    {
        return Err(HelperError::Untrusted);
    }
    conn.set_read_timeout(Some(timeout))
        .and_then(|()| conn.set_write_timeout(Some(timeout)))
        .map_err(|_| HelperError::Protocol)?;
    (&conn)
        .write_all(format!("ID {pid} {start}\n").as_bytes())
        .map_err(|_| HelperError::Protocol)?;
    let mut line = String::new();
    BufReader::new((&conn).take(512))
        .read_line(&mut line)
        .map_err(|_| HelperError::Protocol)?;
    if line == "NO\n" {
        return Ok(None);
    }
    parse_reply(&line).map(Some).ok_or(HelperError::Protocol)
}

/// The identity of `(pid, start)` for the running daemon, blocking (call off the executor).
///
/// In the initial user namespace the reads are made here. In a private one (a hardened systemd
/// user unit) the kernel refuses them, so the helper is asked; no helper means unverified.
pub fn resolve(pid: u32, start: u64, timeout: Duration) -> Option<String> {
    if !uses_helper() {
        return verified_identity(pid, start);
    }
    let env = |k: &str| std::env::var(k).ok();
    let socket = socket_path_from(env)?;
    let server_unit = unit_from(&env, ENV_SERVER_UNIT, HELPER_UNIT);
    match ask(&socket, server_unit.as_deref(), pid, start, timeout) {
        Ok(found) => found,
        Err(e) => {
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    error = ?e,
                    socket = %socket.display(),
                    "gorca runs in a private user namespace and the identity helper does not \
                     answer: application identities stay unverified (start gorca-identity.service)"
                );
            }
            None
        }
    }
}

/// True when identities must come from the helper (this process is in a private user namespace).
pub fn uses_helper() -> bool {
    static PRIVATE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PRIVATE.get_or_init(in_private_user_ns)
}

/// Run the helper until killed: `gorca --identity-helper`. Returns only on a start-up error.
pub fn run_helper() -> std::io::Result<()> {
    let env = |k: &str| std::env::var(k).ok();
    let cfg = ServerConfig::from_env(env)
        .ok_or_else(|| std::io::Error::other("no XDG_RUNTIME_DIR and no GORCA_IDENTITY_SOCK"))?;
    let listener = bind(&cfg)?;
    eprintln!(
        "gorca identity helper listening on {} (client unit {:?})",
        cfg.socket.display(),
        cfg.client_unit
    );
    serve(&listener, &cfg);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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

    const T: Duration = Duration::from_secs(2);

    /// Read-only live probe for `systemd-run --user` transient units with the full hardening
    /// (docs/reports/2026-10-25-linux-platform-identity-helper.md): resolves the identity of
    /// `GORCA_PROBE_PID` through [`resolve`] exactly as the daemon does and prints the result and
    /// the latency. It never writes to the session.
    #[test]
    #[ignore = "needs a live pid and a helper; run inside a transient unit"]
    fn live_helper_identity_probe() {
        let pid: u32 = std::env::var("GORCA_PROBE_PID")
            .expect("GORCA_PROBE_PID")
            .parse()
            .expect("pid");
        let start = crate::proc::read_proc_info(pid).start.expect("start time");
        let t = std::time::Instant::now();
        let id = resolve(pid, start, Duration::from_millis(200));
        let first = t.elapsed();
        let t = std::time::Instant::now();
        for _ in 0..100 {
            let _ = resolve(pid, start, Duration::from_millis(200));
        }
        println!(
            "PROBE private-user-ns={} uses-helper={} identity={id:?} first={first:?} mean-of-100={:?}",
            in_private_user_ns(),
            uses_helper(),
            t.elapsed() / 100
        );
    }

    #[test]
    fn requests_and_replies_round_trip_and_garbage_is_refused() {
        assert_eq!(parse_request("ID 12 345\n"), Some((12, 345)));
        for bad in [
            "",
            "ID 12\n",
            "ID x 3\n",
            "ID 1 2 3\n",
            "id 1 2\n",
            "ID -1 2\n",
        ] {
            assert_eq!(parse_request(bad), None, "{bad:?}");
        }
        assert_eq!(
            parse_reply("OK firefox-esr\n").as_deref(),
            Some("firefox-esr")
        );
        assert_eq!(parse_reply("OK my app\n").as_deref(), Some("my app"));
        for bad in ["OK \n", "NO\n", "OK x", "", "OKx\n"] {
            assert_eq!(parse_reply(bad), None, "{bad:?}");
        }
        assert_eq!(format_reply(Some("a\nb")), "NO\n");
        assert_eq!(format_reply(None), "NO\n");
    }

    #[test]
    fn the_helper_answers_with_the_same_identity_as_a_local_check() {
        let dir = tempfile::tempdir().expect("dir");
        let socket = serve_in(dir.path(), None);
        let (mut c, pid, start) = sleeper();
        assert_eq!(ask(&socket, None, pid, start, T), Ok(Some("sleep".into())));
        assert_eq!(verified_identity(pid, start).as_deref(), Some("sleep"));
        // A recycled pid (wrong start time) is unverified.
        assert_eq!(ask(&socket, None, pid, start + 1, T), Ok(None));
        let _ = c.kill();
        let _ = c.wait();
    }

    #[test]
    fn a_copy_in_a_user_directory_is_unverified_through_the_helper_too() {
        let dir = tempfile::tempdir().expect("dir");
        let socket = serve_in(dir.path(), None);
        let copy = dir.path().join("firefox-esr");
        std::fs::copy("/usr/bin/sleep", &copy).expect("copy");
        let mut c = Command::new(&copy).arg("30").spawn().expect("spawn");
        let pid = c.id();
        let start = crate::proc::read_proc_info(pid).start.expect("start");
        assert_eq!(ask(&socket, None, pid, start, T), Ok(None));
        let _ = c.kill();
        let _ = c.wait();
    }

    #[test]
    fn many_requests_share_one_connection() {
        let dir = tempfile::tempdir().expect("dir");
        let socket = serve_in(dir.path(), None);
        let (mut c, pid, start) = sleeper();
        let mut conn = UnixStream::connect(&socket).expect("connect");
        let mut reader = BufReader::new(conn.try_clone().expect("clone"));
        for _ in 0..3 {
            conn.write_all(format!("ID {pid} {start}\n").as_bytes())
                .expect("write");
            let mut line = String::new();
            reader.read_line(&mut line).expect("read");
            assert_eq!(line, "OK sleep\n");
        }
        conn.write_all(b"garbage\n").expect("write");
        let mut line = String::new();
        reader.read_line(&mut line).expect("read");
        assert_eq!(line, "NO\n");
        let _ = c.kill();
        let _ = c.wait();
    }

    #[test]
    fn the_helper_serves_only_the_configured_unit() {
        let dir = tempfile::tempdir().expect("dir");
        // The test process runs in some unit (or none); a unit name it cannot have is refused.
        let socket = serve_in(dir.path(), Some("not-gorca.service"));
        let (mut c, pid, start) = sleeper();
        assert_eq!(
            ask(&socket, None, pid, start, T),
            Err(HelperError::Protocol)
        );
        let _ = c.kill();
        let _ = c.wait();
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
            ask(&dir.path().join("none.sock"), None, 1, 1, T),
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
}
