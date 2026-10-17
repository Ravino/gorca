//! Per-application connection pool: N peer-to-peer connections to the browser's own AT-SPI socket
//! (ADR-0003 AS-1, ADR-0001 D-7), or the bus call connection when P2P is unavailable.
//!
//! The client must not link libatspi: that makes the browser's `GetApplicationBusAddress` return
//! an empty string (web source-layer report 2.1). This crate talks raw D-Bus through zbus.

use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use gorca_dbus::{DbusError, call_method_to, with_deadline};
use gorca_rt::Deadline;
use serde::Serialize;
use zbus::zvariant::DynamicType;

const ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";
const APPLICATION: &str = "org.a11y.atspi.Application";

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error(transparent)]
    Dbus(#[from] DbusError),
    #[error("application returned no P2P address")]
    NoAddress,
    #[error("P2P address rejected: {0}")]
    Rejected(String),
}

#[derive(Clone, Debug)]
pub struct ConnectOpts {
    /// Number of P2P connections (AS-1: 4; more gives nothing, the server is one thread).
    pub connections: usize,
    /// Deadline for the address call and for each connect (Startup class, section 6.4).
    pub step_timeout: Duration,
    /// Where the socket must live. The caller resolves it (library crates never read the
    /// environment, xtask rule 8).
    pub runtime_dir: PathBuf,
}

impl ConnectOpts {
    pub fn new(runtime_dir: PathBuf) -> Self {
        Self {
            connections: 4,
            step_timeout: Duration::from_millis(500),
            runtime_dir,
        }
    }
}

/// Connections to one application. `lane(i)` addresses connection `i % len`.
#[derive(Debug)]
pub struct AppConn {
    conns: Vec<zbus::Connection>,
    dest: Option<String>,
    p2p: bool,
    rr: AtomicUsize,
    warm: AtomicBool,
    /// Set once the relations cache domain of the application has been woken (`WebQuirks`).
    relations_warm: tokio::sync::OnceCell<()>,
    max_tree_us: AtomicU64,
    max_tree_nodes: AtomicU64,
    pub connect_time: Duration,
}

impl AppConn {
    /// The once-per-connection cell of the relations warm-up (see `extras::warm_relations`).
    pub(crate) fn relations_warm(&self) -> &tokio::sync::OnceCell<()> {
        &self.relations_warm
    }

    /// Resolve the app's P2P address over `bus` and open the pool. Any failure to get a usable
    /// socket degrades to the bus connection (`is_p2p() == false`), never to an error, unless
    /// the bus call itself fails.
    pub async fn connect(
        bus: &zbus::Connection,
        app_bus_name: &str,
        opts: &ConnectOpts,
    ) -> Result<Self, ConnectError> {
        let t0 = Instant::now();
        match Self::try_p2p(bus, app_bus_name, opts).await {
            Ok(conns) if !conns.is_empty() => Ok(Self {
                conns,
                dest: None,
                p2p: true,
                rr: AtomicUsize::new(0),
                warm: AtomicBool::new(false),
                relations_warm: tokio::sync::OnceCell::new(),
                max_tree_us: AtomicU64::new(0),
                max_tree_nodes: AtomicU64::new(0),
                connect_time: t0.elapsed(),
            }),
            Ok(_) | Err(_) => Ok(Self {
                conns: vec![bus.clone()],
                dest: Some(app_bus_name.to_owned()),
                p2p: false,
                rr: AtomicUsize::new(0),
                warm: AtomicBool::new(false),
                relations_warm: tokio::sync::OnceCell::new(),
                max_tree_us: AtomicU64::new(0),
                max_tree_nodes: AtomicU64::new(0),
                connect_time: t0.elapsed(),
            }),
        }
    }

    async fn try_p2p(
        bus: &zbus::Connection,
        app: &str,
        opts: &ConnectOpts,
    ) -> Result<Vec<zbus::Connection>, ConnectError> {
        let dl = Deadline(Instant::now() + opts.step_timeout);
        let msg = call_method_to(
            bus,
            dl,
            Some(app),
            ROOT_PATH,
            APPLICATION,
            "GetApplicationBusAddress",
            &(),
        )
        .await?;
        let addr: String = msg.body().deserialize().map_err(DbusError::Zbus)?;
        if addr.is_empty() {
            return Err(ConnectError::NoAddress);
        }
        check_socket_address(&addr, &opts.runtime_dir).map_err(ConnectError::Rejected)?;
        let mut out = Vec::with_capacity(opts.connections);
        for _ in 0..opts.connections.max(1) {
            let dl = Deadline(Instant::now() + opts.step_timeout);
            let builder = zbus::connection::Builder::address(addr.as_str())
                .map_err(DbusError::Zbus)?
                .p2p();
            let conn = with_deadline(dl, builder.build()).await?;
            out.push(conn);
        }
        Ok(out)
    }

    /// Cache domains were activated for this app (see [`crate::warm`]).
    pub fn is_warm(&self) -> bool {
        self.warm.load(Ordering::Acquire)
    }

    /// Record one `GetTree` round trip (the heaviest since the last reset is kept).
    pub fn note_tree_call(&self, d: Duration) {
        let us = u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
        self.max_tree_us.fetch_max(us, Ordering::Relaxed);
    }

    pub fn note_tree_nodes(&self, n: usize) {
        self.max_tree_nodes
            .fetch_max(u64::try_from(n).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    /// `(heaviest call, heaviest reply in nodes)` since the last reset.
    pub fn tree_stats(&self) -> (Duration, u64) {
        (
            Duration::from_micros(self.max_tree_us.load(Ordering::Relaxed)),
            self.max_tree_nodes.load(Ordering::Relaxed),
        )
    }

    pub fn reset_tree_stats(&self) {
        self.max_tree_us.store(0, Ordering::Relaxed);
        self.max_tree_nodes.store(0, Ordering::Relaxed);
    }

    pub fn mark_warm(&self) {
        self.warm.store(true, Ordering::Release);
    }

    /// Wrap already established peer-to-peer connections (tests, or a caller that connected itself).
    pub fn from_p2p(conns: Vec<zbus::Connection>) -> Self {
        Self {
            conns,
            dest: None,
            p2p: true,
            rr: AtomicUsize::new(0),
            warm: AtomicBool::new(false),
            relations_warm: tokio::sync::OnceCell::new(),
            max_tree_us: AtomicU64::new(0),
            max_tree_nodes: AtomicU64::new(0),
            connect_time: Duration::ZERO,
        }
    }

    /// The application's bus name for object references, empty on a peer-to-peer connection
    /// (the engines read the path of a reference, not its name).
    pub fn bus_name(&self) -> String {
        self.dest.clone().unwrap_or_default()
    }

    pub fn is_p2p(&self) -> bool {
        self.p2p
    }

    pub fn lanes(&self) -> usize {
        self.conns.len()
    }

    /// Call on lane `lane` (modulo the pool size). Never past `deadline`.
    pub async fn call<B>(
        &self,
        lane: usize,
        deadline: Deadline,
        path: &str,
        interface: &str,
        member: &str,
        body: &B,
    ) -> Result<zbus::Message, DbusError>
    where
        B: Serialize + DynamicType,
    {
        let c = &self.conns[lane % self.conns.len()];
        call_method_to(
            c,
            deadline,
            self.dest.as_deref(),
            path,
            interface,
            member,
            body,
        )
        .await
    }

    /// Round-robin lane for one-off calls.
    pub fn next_lane(&self) -> usize {
        self.rr.fetch_add(1, Ordering::Relaxed)
    }
}

/// The path is chosen by the app, so it is untrusted input (P-9 report section 4): it must be a
/// `unix:path=` socket below the runtime dir, owned by the same uid as the runtime dir.
pub fn check_socket_address(addr: &str, runtime_dir: &Path) -> Result<(), String> {
    let rest = addr
        .strip_prefix("unix:path=")
        .ok_or_else(|| format!("not a unix:path address: {addr}"))?;
    let path = rest.split(',').next().unwrap_or(rest);
    let p = Path::new(path);
    if !p.starts_with(runtime_dir) {
        return Err(format!("socket outside the runtime dir: {path}"));
    }
    if p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err("socket path contains ..".into());
    }
    let md = std::fs::symlink_metadata(p).map_err(|e| format!("stat {path}: {e}"))?;
    if !md.file_type().is_socket() {
        return Err(format!("{path} is not a socket"));
    }
    let rd = std::fs::metadata(runtime_dir).map_err(|e| format!("stat runtime dir: {e}"))?;
    if md.uid() != rd.uid() {
        return Err(format!("{path} is not owned by the runtime dir owner"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_foreign_addresses() {
        let rt = Path::new("/run/user/1000");
        assert!(check_socket_address("tcp:host=x,port=1", rt).is_err());
        assert!(check_socket_address("unix:path=/tmp/evil", rt).is_err());
        assert!(check_socket_address("unix:path=/run/user/1000/../../tmp/x", rt).is_err());
        assert!(check_socket_address("unix:path=/run/user/1000/at-spi2-X/socket", rt).is_err());
    }

    #[test]
    fn accepts_a_real_socket_under_the_runtime_dir() {
        let dir = std::env::temp_dir().join(format!("gorca-p2p-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("socket");
        let _l = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let addr = format!("unix:path={},guid=abc", sock.display());
        assert_eq!(check_socket_address(&addr, &dir), Ok(()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
