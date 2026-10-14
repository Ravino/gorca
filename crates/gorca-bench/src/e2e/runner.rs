//! The rig of one run: private-bus guard, mock speech server, reader child, fake apps, taps.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::apps::{FakeApp, FakeMutter, FocusTap, Reader, ReaderSpec};
use super::env;
use super::mock::{Flavor, Kind, MockServer};

static RUN_SEQ: AtomicU32 = AtomicU32::new(0);

/// Where the harness lives and which (private) buses it may touch.
#[derive(Clone, Debug)]
pub struct HostEnv {
    /// `tests/harness` (fakeapp.py, fake_mutter.py).
    pub harness_dir: PathBuf,
    pub a11y_addr: String,
    pub session_addr: String,
    /// Directory for the mock speech sockets (short path: unix socket limit).
    pub sock_dir: PathBuf,
    /// Extra environment for the reader (`--reader-env K=V`).
    pub reader_env: Vec<(String, String)>,
}

impl HostEnv {
    /// Refuses to run unless both buses live under a `run-isolated.sh` temp dir. This is the
    /// guard that keeps the harness away from the real session.
    pub fn from_env() -> Result<Self> {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let a11y_addr = get(env::A11Y_BUS)
            .or_else(|| get("AT_SPI_BUS_ADDRESS"))
            .context("no a11y bus address: run under tests/harness/run-isolated.sh")?;
        let session_addr = get(env::SESSION_BUS).context("no session bus address")?;
        for (what, a) in [("a11y", &a11y_addr), ("session", &session_addr)] {
            if !a.contains("/gorca-h.") {
                bail!(
                    "refusing to run: the {what} bus {a} is not a harness bus (tests/harness/run-isolated.sh)"
                );
            }
        }
        let root = get("GORCA_HARNESS_DIR").context("GORCA_HARNESS_DIR not set")?;
        let harness_dir = std::env::var("GORCA_E2E_SCRIPTS")
            .map_or_else(|_| PathBuf::from("tests/harness"), PathBuf::from);
        Ok(Self {
            harness_dir,
            a11y_addr,
            session_addr,
            sock_dir: PathBuf::from(root).join("run"),
            reader_env: Vec::new(),
        })
    }
}

#[derive(Clone, Debug)]
pub struct RigOpts {
    pub flavor: Flavor,
    pub reader: ReaderSpec,
    /// Stub only: per-focus `Get Name` peer call.
    pub calls: bool,
    pub keys: bool,
    /// Healthy apps (role -> node count).
    pub apps: Vec<(String, usize)>,
    /// Extra arguments for a daemon reader (for example `--bench-present-log=PATH`).
    pub extra_reader_args: Vec<String>,
    /// Capture the reader's standard error in this file instead of inheriting it.
    pub reader_stderr: Option<PathBuf>,
    /// Hung apps by role: `tree` with `--hang never` (all D-Bus calls unanswered).
    pub hung_apps: Vec<(String, usize)>,
}

#[derive(Debug)]
pub struct Rig {
    // Field order is drop order: the reader goes first, the mock server last.
    pub reader: Reader,
    pub apps: HashMap<String, FakeApp>,
    pub mutter: Option<FakeMutter>,
    pub tap: FocusTap,
    pub mock: MockServer,
    pub env: HostEnv,
    pub keys_ok: bool,
    pub flavor: Flavor,
}

impl Rig {
    pub fn start(env: &HostEnv, o: &RigOpts) -> Result<Self> {
        let seq = RUN_SEQ.fetch_add(1, Ordering::Relaxed);
        let sock = env.sock_dir.join(format!("sp{seq}.sock"));
        let mock = MockServer::start(&sock, o.flavor)?;
        let tap = FocusTap::start(&env.a11y_addr)?;
        let mut mutter = if o.keys {
            Some(FakeMutter::spawn(&env.harness_dir)?)
        } else {
            None
        };
        let mut apps = HashMap::new();
        for (role, nodes) in &o.apps {
            apps.insert(
                role.clone(),
                FakeApp::spawn(&env.harness_dir, "tree", role, *nodes + 2, &[])?,
            );
        }
        for (role, nodes) in &o.hung_apps {
            apps.insert(
                role.clone(),
                FakeApp::spawn(
                    &env.harness_dir,
                    "hung",
                    role,
                    *nodes + 2,
                    &["--hang", "never"],
                )?,
            );
        }
        let mut envs = vec![
            (env::A11Y_BUS.to_owned(), env.a11y_addr.clone()),
            (env::SESSION_BUS.to_owned(), env.session_addr.clone()),
            (env::SPEECH_BACKEND.to_owned(), o.flavor.name().to_owned()),
            (env::SPEECH_SOCKET.to_owned(), sock.display().to_string()),
            (
                env::STUB_CALLS.to_owned(),
                if o.calls { "1" } else { "0" }.to_owned(),
            ),
        ];
        envs.extend(env.reader_env.iter().cloned());
        let stderr = match &o.reader_stderr {
            Some(p) => Some(std::fs::File::create(p)?),
            None => None,
        };
        let mut reader = Reader::spawn_with(&o.reader, &envs, &o.extra_reader_args, stderr)?;
        // The reader is up when it has connected to the speech server.
        let end = std::time::Instant::now() + Duration::from_secs(20);
        while mock.connections() == 0 {
            if !reader.alive() {
                bail!("the reader exited before connecting to the speech server");
            }
            if std::time::Instant::now() > end {
                bail!("the reader did not connect to the speech server within 20 s");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut keys_ok = false;
        if let Some(m) = mutter.as_mut() {
            let end = std::time::Instant::now() + Duration::from_secs(5);
            while std::time::Instant::now() < end {
                if m.clients() > 0 {
                    keys_ok = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let mut rig = Self {
            env: env.clone(),
            mock,
            reader,
            tap,
            apps,
            mutter,
            keys_ok,
            flavor: o.flavor,
        };
        rig.warm_up()?;
        Ok(rig)
    }

    /// Focus nodes until the reader demonstrably speaks, then a few more (code paths, caches).
    fn warm_up(&mut self) -> Result<()> {
        let Some(role) = self.apps.keys().next().cloned() else {
            return Ok(());
        };
        let mark = self.mock.mark();
        let end = std::time::Instant::now() + Duration::from_secs(15);
        let mut live = false;
        let mut node = 1usize;
        while std::time::Instant::now() < end {
            node = if node >= 40 { 1 } else { node + 1 };
            if let Some(app) = self.apps.get_mut(&role) {
                app.focus(node)?;
            }
            if self
                .mock
                .wait_for(mark, Duration::from_millis(300), |r| {
                    matches!(r.kind, Kind::Speak { .. })
                })
                .is_some()
            {
                live = true;
                break;
            }
        }
        if !live {
            bail!("the reader never spoke for a focus event within 15 s (warm-up)");
        }
        for i in 0..30 {
            node = 41 + i;
            if let Some(app) = self.apps.get_mut(&role) {
                app.focus(node)?;
            }
            std::thread::sleep(Duration::from_millis(12));
        }
        std::thread::sleep(Duration::from_millis(300));
        self.tap.clear();
        Ok(())
    }

    pub fn socket(&self) -> &Path {
        self.mock.path()
    }
}
