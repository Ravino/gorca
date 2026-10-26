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
//! 0700, socket 0600). A connection carries any number of pipelined requests; replies are tagged
//! and may arrive in any order (workers answer concurrently):
//!
//! ```text
//! -> ID <tag> <pid> <start-ticks>\n
//! <- OK <tag> <identity>\n   (verified; the identity may contain spaces, never a newline)
//! <- NO <tag>\n              (unverified, unknown or recycled pid, malformed identity)
//! <- NO\n                    (a line that is not a request; carries no tag)
//! ```
//!
//! The daemon keeps one connection and a cache ([`crate::identity_client`]).
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
use std::sync::{Arc, Mutex};
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

/// Worker threads answering requests (each is a few `/proc` reads); the queue in front of them
/// is bounded by memory only.
const WORKERS: usize = 8;
/// Connections served at once (each has a reader thread); a further one waits to be accepted.
const MAX_CONNECTIONS: usize = 256;
/// Idle time after which the helper drops a connection (the client reconnects on demand).
const IDLE: Duration = Duration::from_secs(600);
/// How long a reply write may take before the connection is given up.
const WRITE: Duration = Duration::from_secs(5);
/// A request line is `ID <u64> <u32> <u64>\n`: well under this.
const MAX_LINE: u64 = 96;

/// Where the helper listens: `$GORCA_IDENTITY_SOCK`, else `$XDG_RUNTIME_DIR/gorca/identity.sock`.
pub fn socket_path_from(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(p) = env(ENV_SOCKET).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let dir = env("XDG_RUNTIME_DIR").filter(|d| !d.is_empty())?;
    Some(Path::new(&dir).join("gorca").join("identity.sock"))
}

pub(crate) fn unit_from(
    env: &impl Fn(&str) -> Option<String>,
    key: &str,
    default: &str,
) -> Option<String> {
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

/// One request waiting for a worker, with the connection to answer on.
struct Job {
    tag: u64,
    pid: u32,
    start: u64,
    out: Arc<Mutex<UnixStream>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Serve forever on a bound listener. Each connection has a reader thread that queues its
/// requests (any number, pipelined) for [`WORKERS`] workers; replies carry the request's tag and
/// may come in any order. Nothing is dropped: the queue is bounded by memory, and a connection
/// beyond [`MAX_CONNECTIONS`] waits to be accepted.
pub fn serve(listener: &UnixListener, cfg: &ServerConfig) {
    let (jobs, queue) = std::sync::mpsc::channel::<Job>();
    let queue = Arc::new(Mutex::new(queue));
    for _ in 0..WORKERS {
        let queue = Arc::clone(&queue);
        let _ = std::thread::Builder::new()
            .name("identity-worker".into())
            .spawn(move || {
                loop {
                    let next = lock(&queue).recv();
                    let Ok(job) = next else { return };
                    let identity = verified_identity(job.pid, job.start);
                    let reply = format_reply(job.tag, identity.as_deref());
                    let _ = lock(&job.out).write_all(reply.as_bytes());
                }
            });
    }
    let active = Arc::new((Mutex::new(0usize), std::sync::Condvar::new()));
    for conn in listener.incoming() {
        let Ok(conn) = conn else {
            // A failed accept (descriptor limit, aborted connection): back off, keep serving.
            // The helper is a plain thread-per-connection process, not an async actor.
            #[allow(clippy::disallowed_methods)]
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        {
            let (count, freed) = &*active;
            let mut n = lock(count);
            while *n >= MAX_CONNECTIONS {
                n = freed
                    .wait(n)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            *n += 1;
        }
        let (cfg, jobs, active) = (cfg.clone(), jobs.clone(), Arc::clone(&active));
        let spawned = std::thread::Builder::new()
            .name("identity-conn".into())
            .spawn({
                let active = Arc::clone(&active);
                move || {
                    handle(&conn, &cfg, &jobs);
                    let (count, freed) = &*active;
                    *lock(count) -= 1;
                    freed.notify_one();
                }
            });
        if spawned.is_err() {
            *lock(&active.0) -= 1;
        }
    }
}

/// The unit of the process at the other end of `conn`.
pub(crate) fn peer_unit(conn: &UnixStream) -> Option<String> {
    let pid = gorca_dbus::peer_pid(conn).ok()?;
    parse_cgroup(&std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?)
}

fn handle(conn: &UnixStream, cfg: &ServerConfig, jobs: &std::sync::mpsc::Sender<Job>) {
    if let Some(want) = &cfg.client_unit
        && peer_unit(conn).as_deref() != Some(want.as_str())
    {
        return;
    }
    if conn.set_read_timeout(Some(IDLE)).is_err() || conn.set_write_timeout(Some(WRITE)).is_err() {
        return;
    }
    let Ok(writer) = conn.try_clone() else { return };
    let out = Arc::new(Mutex::new(writer));
    let mut reader = BufReader::new(conn.take(MAX_LINE));
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
        match parse_request(&line) {
            Some((tag, pid, start)) => {
                let job = Job {
                    tag,
                    pid,
                    start,
                    out: Arc::clone(&out),
                };
                if jobs.send(job).is_err() {
                    return;
                }
            }
            None => {
                if lock(&out).write_all(b"NO\n").is_err() {
                    return;
                }
            }
        }
    }
}

/// `ID <tag> <pid> <start>\n` to `(tag, pid, start)`.
pub fn parse_request(line: &str) -> Option<(u64, u32, u64)> {
    let mut it = line.trim_end_matches('\n').split(' ');
    (it.next()? == "ID").then_some(())?;
    let tag = it.next()?.parse().ok()?;
    let pid = it.next()?.parse().ok()?;
    let start = it.next()?.parse().ok()?;
    it.next().is_none().then_some((tag, pid, start))
}

fn format_reply(tag: u64, identity: Option<&str>) -> String {
    match identity {
        Some(id) if !id.is_empty() && !id.contains(['\n', '\r', '\0']) => {
            format!("OK {tag} {id}\n")
        }
        _ => format!("NO {tag}\n"),
    }
}

/// `OK <tag> <identity>\n` / `NO <tag>\n` to `(tag, identity)`; `None` for anything else.
pub fn parse_reply(line: &str) -> Option<(u64, Option<String>)> {
    let body = line.strip_suffix('\n')?;
    if let Some(rest) = body.strip_prefix("NO ") {
        return Some((rest.parse().ok()?, None));
    }
    let (tag, id) = body.strip_prefix("OK ")?.split_once(' ')?;
    if id.is_empty() || id.contains(['\r', '\0']) {
        return None;
    }
    Some((tag.parse().ok()?, Some(id.to_owned())))
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

/// Pause between attempts while the helper is not there yet or is momentarily busy.
pub(crate) const RETRY: Duration = Duration::from_millis(10);

#[cfg(test)]
/// One-shot client (a fresh connection per call; the daemon uses [`crate::identity_client`]).
/// Asks the helper at `socket` for the identity of `(pid, start)` within `timeout` in total. With
/// `server_unit` the server must be in that systemd unit. `Ok(None)` is the helper's answer
/// "unverified". An unreachable helper (still starting) and a dropped or garbled exchange are
/// retried until the deadline; an untrusted server is not.
pub fn ask(
    socket: &Path,
    server_unit: Option<&str>,
    pid: u32,
    start: u64,
    timeout: Duration,
) -> Result<Option<String>, HelperError> {
    let end = std::time::Instant::now() + timeout;
    loop {
        let left = end.saturating_duration_since(std::time::Instant::now());
        match ask_once(socket, server_unit, pid, start, left) {
            Err(HelperError::Unreachable | HelperError::Protocol) if left > RETRY => {
                // Runs on a blocking-pool thread, never on an async worker.
                #[allow(clippy::disallowed_methods)]
                std::thread::sleep(RETRY);
            }
            other => return other,
        }
    }
}

#[cfg(test)]
fn ask_once(
    socket: &Path,
    server_unit: Option<&str>,
    pid: u32,
    start: u64,
    timeout: Duration,
) -> Result<Option<String>, HelperError> {
    let end = std::time::Instant::now() + timeout;
    let conn =
        gorca_dbus::connect_unix_timeout(socket, timeout).map_err(|_| HelperError::Unreachable)?;
    if let Some(want) = server_unit
        && peer_unit(&conn).as_deref() != Some(want)
    {
        return Err(HelperError::Untrusted);
    }
    // The rest of the deadline (a zero timeout is refused by the OS: keep at least 1 ms).
    let left = end
        .saturating_duration_since(std::time::Instant::now())
        .max(Duration::from_millis(1));
    conn.set_read_timeout(Some(left))
        .and_then(|()| conn.set_write_timeout(Some(left)))
        .map_err(|_| HelperError::Protocol)?;
    (&conn)
        .write_all(format!("ID 1 {pid} {start}\n").as_bytes())
        .map_err(|_| HelperError::Protocol)?;
    let mut line = String::new();
    BufReader::new((&conn).take(512))
        .read_line(&mut line)
        .map_err(|_| HelperError::Protocol)?;
    match parse_reply(&line) {
        Some((1, found)) => Ok(found),
        _ => Err(HelperError::Protocol),
    }
}

/// True when identities must come from the helper (this process is in a private user namespace).
pub fn uses_helper() -> bool {
    static PRIVATE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PRIVATE.get_or_init(in_private_user_ns)
}

/// Run the helper until killed: `gorca --identity-helper`. Returns only on a start-up error.
pub fn run_helper(env: impl Fn(&str) -> Option<String>) -> std::io::Result<()> {
    let cfg = ServerConfig::from_env(env)
        .ok_or_else(|| std::io::Error::other("no XDG_RUNTIME_DIR and no GORCA_IDENTITY_SOCK"))?;
    let listener = bind(&cfg)?;
    eprintln!(
        "gorca identity helper listening on {} (client unit {:?})",
        cfg.socket.display(),
        cfg.client_unit
    );
    notify_ready();
    serve(&listener, &cfg);
    Ok(())
}

/// `sd_notify(READY=1)` (the unit is `Type=notify`): the socket is bound and listening, so a
/// daemon ordered after the helper finds it. A datagram to `$NOTIFY_SOCKET`; no-op without one.
fn notify_ready() {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixDatagram};
    let Some(path) = std::env::var_os("NOTIFY_SOCKET").filter(|p| !p.is_empty()) else {
        return;
    };
    let Ok(sock) = UnixDatagram::unbound() else {
        return;
    };
    let bytes = std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str());
    let addr = match bytes.strip_prefix(b"@") {
        Some(name) => SocketAddr::from_abstract_name(name),
        None => SocketAddr::from_pathname(&path),
    };
    if let Ok(addr) = addr {
        let _ = sock.send_to_addr(b"READY=1\n", &addr);
    }
}

#[cfg(test)]
#[path = "identity_svc_audit_tests.rs"]
mod audit;

#[cfg(test)]
#[path = "identity_svc_tests.rs"]
mod tests;
