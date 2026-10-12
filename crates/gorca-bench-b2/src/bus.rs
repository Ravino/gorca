//! Bus fixtures: the mock broker, a private `dbus-daemon`, a private `dbus-broker` (the one that
//! reproduces F-3 for real), or an existing bus (the isolated AT-SPI harness).
//!
//! dbus-broker cannot be started by `dbus-broker-launch --scope user` without a systemd user
//! manager. `--scope system` takes its "regular bus" from `DBUS_SYSTEM_BUS_ADDRESS`, which we
//! point at a helper `dbus-daemon`, and `systemd-socket-activate` provides the listening socket
//! the launcher expects (`LISTEN_FDS`). Nothing talks to the host's systemd or buses.

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::feeder::SocketSink;
use crate::mockbus::{MockBroker, MockOpts};
use crate::wire::BusConn;

#[derive(Clone, Debug)]
pub enum BusKind {
    Mock(MockOpts),
    /// Private `dbus-daemon`, with this `max_outgoing_bytes` per connection.
    Daemon {
        max_outgoing: u64,
    },
    /// Private `dbus-broker`; `pool_bytes` is the per-uid quota (`--max-bytes`).
    Broker {
        pool_bytes: u64,
    },
    /// An already running bus, e.g. `AT_SPI_BUS_ADDRESS` inside `run-isolated.sh`.
    Existing(String),
}

impl BusKind {
    pub fn label(&self) -> &'static str {
        match self {
            BusKind::Mock(_) => "mock",
            BusKind::Daemon { .. } => "dbus-daemon",
            BusKind::Broker { .. } => "dbus-broker",
            BusKind::Existing(_) => "existing",
        }
    }
}

struct Proc {
    child: Child,
}

impl Drop for Proc {
    fn drop(&mut self) {
        crate::sys::kill_group(self.child.id());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct Bus {
    pub kind: BusKind,
    pub address: String,
    pub dir: PathBuf,
    mock: Option<MockBroker>,
    procs: Vec<Proc>,
    own_dir: bool,
}

impl std::fmt::Debug for Bus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Bus({} at {})", self.kind.label(), self.address)
    }
}

static SEQ: AtomicU32 = AtomicU32::new(0);

fn temp_dir() -> Result<PathBuf> {
    let mut base = std::env::temp_dir();
    if base.as_os_str().len() > 60 {
        base = PathBuf::from("/tmp");
    }
    let d = base.join(format!(
        "gb2.{}.{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&d)?;
    Ok(d)
}

const POLICY: &str = r#"<policy context="default"><allow user="*"/><allow send_destination="*"/><allow receive_type="method_call"/><allow receive_type="method_return"/><allow receive_type="error"/><allow receive_type="signal"/><allow own="*"/></policy>"#;

fn bus_config(listen: &Path, max_out: u64, conns: u64) -> String {
    format!(
        r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>accessibility</type><auth>EXTERNAL</auth><listen>unix:path={}</listen>{POLICY}
<limit name="max_incoming_bytes">1000000000</limit><limit name="max_outgoing_bytes">{max_out}</limit>
<limit name="max_message_size">1000000000</limit><limit name="max_connections_per_user">{conns}</limit>
<limit name="max_completed_connections">{conns}</limit><limit name="max_incomplete_connections">64</limit>
<limit name="max_match_rules_per_connection">50000</limit><limit name="max_names_per_connection">50000</limit>
<limit name="max_replies_per_connection">50000</limit><limit name="reply_timeout">300000</limit></busconfig>"#,
        listen.display()
    )
}

fn spawn(cmd: &mut Command) -> Result<Proc> {
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(if std::env::var_os("B2_BUS_LOG").is_some() {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .process_group(0)
        .spawn()
        .with_context(|| format!("spawning {cmd:?}"))?;
    Ok(Proc { child })
}

fn wait_ready(addr: &str, timeout: Duration) -> Result<()> {
    let end = Instant::now() + timeout;
    let mut last = String::new();
    while Instant::now() < end {
        match BusConn::connect(addr, Duration::from_millis(800)) {
            Ok(_) => return Ok(()),
            Err(e) => last = format!("{e:#}"),
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!("bus at {addr} not ready within {timeout:?}: {last}")
}

impl Bus {
    pub fn start(kind: BusKind) -> Result<Self> {
        match kind.clone() {
            BusKind::Mock(opts) => {
                let dir = temp_dir()?;
                let mock = MockBroker::start(&dir, opts)?;
                Ok(Self {
                    address: mock.address(),
                    kind,
                    dir,
                    mock: Some(mock),
                    procs: Vec::new(),
                    own_dir: true,
                })
            }
            BusKind::Existing(address) => Ok(Self {
                address,
                kind,
                dir: PathBuf::new(),
                mock: None,
                procs: Vec::new(),
                own_dir: false,
            }),
            BusKind::Daemon { max_outgoing } => {
                let dir = temp_dir()?;
                let sock = dir.join("daemon-bus");
                let cfg = dir.join("daemon.conf");
                fs::write(&cfg, bus_config(&sock, max_outgoing, 256))?;
                let p = spawn(
                    Command::new("dbus-daemon")
                        .arg(format!("--config-file={}", cfg.display()))
                        .arg("--nofork"),
                )?;
                let address = format!("unix:path={}", sock.display());
                wait_ready(&address, Duration::from_secs(5))?;
                Ok(Self {
                    kind,
                    address,
                    dir,
                    mock: None,
                    procs: vec![p],
                    own_dir: true,
                })
            }
            BusKind::Broker { pool_bytes } => {
                let dir = temp_dir()?;
                // Helper bus the launcher insists on talking to.
                let helper = dir.join("helper-bus");
                let hcfg = dir.join("helper.conf");
                fs::write(&hcfg, bus_config(&helper, 100_000_000, 64))?;
                let hp = spawn(
                    Command::new("dbus-daemon")
                        .arg(format!("--config-file={}", hcfg.display()))
                        .arg("--nofork"),
                )?;
                wait_ready(
                    &format!("unix:path={}", helper.display()),
                    Duration::from_secs(5),
                )?;
                let sock = dir.join("broker-bus");
                let conns = 64u64;
                let cfg = dir.join("broker.conf");
                fs::write(&cfg, bus_config(&sock, (pool_bytes / conns).max(1), conns))?;
                let path = std::env::var("PATH").unwrap_or_default();
                let bp = spawn(
                    Command::new("systemd-socket-activate")
                        .arg("-l")
                        .arg(&sock)
                        .arg("dbus-broker-launch")
                        .arg("--config-file")
                        .arg(&cfg)
                        .arg("--scope")
                        .arg("system")
                        .env_clear()
                        .env("PATH", path)
                        .env(
                            "DBUS_SYSTEM_BUS_ADDRESS",
                            format!("unix:path={}", helper.display()),
                        )
                        .env("XDG_RUNTIME_DIR", &dir),
                )?;
                let address = format!("unix:path={}", sock.display());
                wait_ready(&address, Duration::from_secs(8))?;
                Ok(Self {
                    kind,
                    address,
                    dir,
                    mock: None,
                    procs: vec![bp, hp],
                    own_dir: true,
                })
            }
        }
    }

    pub fn mock(&self) -> Option<&MockBroker> {
        self.mock.as_ref()
    }

    /// A client connection that will carry one app's events (real buses only).
    pub fn sender(&self) -> Result<(SocketSink, String)> {
        let c = BusConn::connect(&self.address, Duration::from_secs(5))?;
        Ok((SocketSink { stream: c.stream }, c.unique))
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        self.mock = None;
        self.procs.clear();
        if self.own_dir {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}
