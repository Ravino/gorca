// SPDX-License-Identifier: GPL-3.0-or-later
//! Stub "reader": the stand-in for `gorca-daemon` until the Phase 1 slice lands.
//!
//! It links exactly the pieces that exist today: the raw a11y-bus drain (`gorca-dbus`), the
//! per-app coalescing ingress (`gorca-ingress`), one app-actor task per sender, and a minimal
//! speech lane that talks voicego binary framing or text SSIP to the mock server. A focus event
//! becomes `SPEAK f:<local id>`; a newer one first sends `STOP` for the stale utterance. Keys
//! from the (emulated) Mutter keyboard monitor become `SPEAK k:<keycode>`.
//!
//! It is deliberately *not* a model of the real core (no mirror, no FSM, no verbalizer). Its
//! numbers are the ingress-only baseline: bus -> drain -> coalesce -> lane -> socket. Anything
//! the daemon adds on top shows up as the difference.
//!
//! Configuration is by environment, shared with the daemon seam ([`super::env`]).

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use gorca_bench_b2::wire::{BusConn, Enc, Parse, SIGNAL, parse};
use gorca_dbus::drain::{AppRoute, DrainConfig, IngressFactory, Router, spawn_drain};
use gorca_model::NodeEvent;
use tokio::runtime::Handle;

use super::mock::Flavor;

pub const ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";

#[derive(Clone, Debug)]
pub struct StubConfig {
    pub a11y_addr: String,
    /// Private session bus; `None` disables the keyboard path.
    pub session_addr: Option<String>,
    pub flavor: Flavor,
    pub socket: PathBuf,
    /// Call `Get Name` on the source app for every non-empty event batch (deadline [`CALL_DEADLINE`]).
    pub calls: bool,
    /// Test hook: sleep this long in the lane before every send. The harness self-check uses
    /// it to prove that an injected delay is measured (`--reader-env GORCA_STUB_DELAY_US=5000`).
    pub delay: Duration,
}

/// Deadline of the per-focus peer call (CLAUDE.md: no IPC without a deadline).
pub const CALL_DEADLINE: Duration = Duration::from_millis(100);

#[derive(Debug)]
struct Intent {
    text: String,
}

// ---- speech lane ---------------------------------------------------------------------------------

#[derive(Debug, Default)]
struct LaneState {
    got_230: Mutex<bool>,
    cv: Condvar,
    /// SPEAKs sent minus 702/703 events seen: > 0 means stale speech may still play.
    active: AtomicI64,
}

fn frame(out: &mut Vec<u8>, binary: bool, payload: &str) {
    if binary {
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload.as_bytes());
    } else {
        out.extend_from_slice(payload.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
}

/// Reads replies and events; flags `230` and counts `702`/`703` terminal events.
fn lane_reader(mut s: UnixStream, binary: bool, st: Arc<LaneState>) {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; 16 * 1024];
    let scan = |text: &str, st: &LaneState| {
        for line in text.split("\r\n") {
            let b = line.as_bytes();
            if b.len() >= 4 && b[3] == b' ' {
                match &line[..3] {
                    "230" => {
                        *st.got_230.lock().unwrap_or_else(PoisonError::into_inner) = true;
                        st.cv.notify_all();
                    }
                    "702" | "703" => {
                        st.active.fetch_sub(1, Ordering::AcqRel);
                    }
                    _ => {}
                }
            }
        }
    };
    loop {
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        if binary {
            while buf.len() >= 4 {
                let n = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
                if buf.len() < 4 + n {
                    break;
                }
                let text = String::from_utf8_lossy(&buf[4..4 + n]).into_owned();
                buf.drain(..4 + n);
                scan(&text, &st);
            }
        } else if let Some(end) = buf.windows(2).rposition(|w| w == b"\r\n") {
            let text = String::from_utf8_lossy(&buf[..end + 2]).into_owned();
            buf.drain(..end + 2);
            scan(&text, &st);
        }
    }
}

/// Connect, handshake, and spawn the lane. Returns the intent sender.
fn start_lane(cfg: &StubConfig) -> Result<Sender<Intent>> {
    let delay = cfg.delay;
    let mut w = UnixStream::connect(&cfg.socket)
        .with_context(|| format!("connecting to {}", cfg.socket.display()))?;
    let binary = cfg.flavor == Flavor::Voicego;
    let st = Arc::new(LaneState::default());
    let rd = w.try_clone()?;
    {
        let st = Arc::clone(&st);
        std::thread::Builder::new()
            .name("stub-lane-rd".to_owned())
            .spawn(move || lane_reader(rd, binary, st))?;
    }
    let mut hello = Vec::new();
    if binary {
        hello.extend_from_slice(&[0xAA, b'V', b'G', 1]);
    }
    frame(&mut hello, binary, "SET self CLIENT_NAME gorca:stub:main");
    frame(&mut hello, binary, "SET self NOTIFICATION all on");
    w.write_all(&hello)?;
    let (tx, rx): (Sender<Intent>, Receiver<Intent>) = channel();
    std::thread::Builder::new()
        .name("stub-lane".to_owned())
        .spawn(move || {
            while let Ok(mut it) = rx.recv() {
                // Latest wins: anything queued behind is stale before it was ever sent.
                while let Ok(n) = rx.try_recv() {
                    it = n;
                }
                if !delay.is_zero() {
                    std::thread::sleep(delay);
                }
                *st.got_230.lock().unwrap_or_else(PoisonError::into_inner) = false;
                let mut out = Vec::with_capacity(128);
                if st.active.load(Ordering::Acquire) > 0 {
                    frame(&mut out, binary, "STOP");
                }
                if binary {
                    frame(&mut out, true, &format!("SPEAK {}", it.text.len()));
                } else {
                    frame(&mut out, false, "SPEAK");
                }
                if w.write_all(&out).is_err() {
                    return;
                }
                let g = st.got_230.lock().unwrap_or_else(PoisonError::into_inner);
                let (g, _) = st
                    .cv
                    .wait_timeout_while(g, Duration::from_millis(250), |ok| !*ok)
                    .unwrap_or_else(PoisonError::into_inner);
                let got = *g;
                drop(g);
                if !got {
                    continue; // deadline: drop this utterance, never block the lane
                }
                let mut data = Vec::with_capacity(it.text.len() + 8);
                if binary {
                    frame(&mut data, true, &it.text);
                } else {
                    data.extend_from_slice(it.text.as_bytes());
                    data.extend_from_slice(b"\r\n.\r\n");
                }
                st.active.fetch_add(1, Ordering::AcqRel);
                if w.write_all(&data).is_err() {
                    return;
                }
            }
        })?;
    Ok(tx)
}

// ---- app actors ----------------------------------------------------------------------------------

/// Hands out one ingress + one actor task per bus sender.
struct StubRouter {
    factory: IngressFactory,
    handle: Handle,
    lane: Sender<Intent>,
    a11y_addr: String,
    calls: bool,
    routes: Mutex<std::collections::HashMap<String, AppRoute>>,
    next: Mutex<u32>,
}

impl std::fmt::Debug for StubRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StubRouter").finish_non_exhaustive()
    }
}

impl Router for StubRouter {
    fn route(&self, sender: &str) -> Option<AppRoute> {
        let mut routes = self.routes.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(r) = routes.get(sender) {
            return Some(r.clone());
        }
        let serial = {
            let mut n = self.next.lock().unwrap_or_else(PoisonError::into_inner);
            *n += 1;
            *n
        };
        let app = gorca_model::AppId {
            connector: gorca_model::ConnectorId(0),
            serial,
        };
        let (tx, rx) = self.factory.new_app();
        let lane = self.lane.clone();
        let calls = if self.calls {
            Some(peer_caller(&self.a11y_addr, sender))
        } else {
            None
        };
        self.handle.spawn(async move {
            let mut last: Option<u64> = None;
            loop {
                rx.notified().await;
                let d = rx.drain();
                if !d.events.is_empty()
                    && let Some(c) = &calls
                {
                    // one deadline-bound peer call per batch; never waits (the actor of the
                    // app owns the hang)
                    let _ = c.try_send(());
                }
                for ev in &d.events {
                    if let NodeEvent::FocusChanged { node } = ev {
                        if last == Some(node.local.0) {
                            continue; // the same focus announced twice (state + Focus signal)
                        }
                        last = Some(node.local.0);
                        let _ = lane.send(Intent {
                            text: format!("f:{:016x}", node.local.0),
                        });
                    }
                }
            }
        });
        let route = AppRoute {
            app,
            tx,
            hints: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        routes.insert(sender.to_owned(), route.clone());
        Some(route)
    }
}

/// A thread per app that performs `Properties.Get(Name)` with [`CALL_DEADLINE`] for every
/// focus event, serially. A hung peer blocks only this thread; the queue is bounded and drops.
fn peer_caller(addr: &str, dest: &str) -> SyncSender<()> {
    let (tx, rx) = sync_channel::<()>(8);
    let (addr, dest) = (addr.to_owned(), dest.to_owned());
    let _ = std::thread::Builder::new()
        .name("stub-peer".to_owned())
        .spawn(move || {
            let Ok(mut c) = BusConn::connect(&addr, Duration::from_secs(5)) else {
                return;
            };
            while rx.recv().is_ok() {
                let mut e = Enc::default();
                e.string("org.a11y.atspi.Accessible");
                e.string("Name");
                let _ = c.call(
                    &dest,
                    ROOT_PATH,
                    "org.freedesktop.DBus.Properties",
                    "Get",
                    Some("ss"),
                    &e.buf,
                    CALL_DEADLINE,
                );
            }
        });
    tx
}

// ---- keyboard monitor client ---------------------------------------------------------------------

/// Own `org.gnome.Orca.KeyboardMonitor`, `WatchKeyboard`, then turn each key press into an
/// intent. Returns an error if the monitor refuses (no access, no manager).
fn start_keys(session: &str, lane: Sender<Intent>) -> Result<()> {
    let t = Duration::from_secs(2);
    let mut c = BusConn::connect(session, t)?;
    let mut e = Enc::default();
    e.string("org.gnome.Orca.KeyboardMonitor");
    e.u32(4); // DBUS_NAME_FLAG_DO_NOT_QUEUE
    c.call(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "RequestName",
        Some("su"),
        &e.buf,
        t,
    )?;
    c.add_match(
        "type='signal',interface='org.freedesktop.a11y.KeyboardMonitor',member='KeyEvent'",
        t,
    )?;
    // Mutter 48.7 crashes in rebuild_all_grabbed_modifiers on a grabber that never called
    // SetKeyGrabs (NULL modifiers): an empty set (two empty arrays, 8 zero bytes) goes first.
    c.call(
        "org.freedesktop.a11y.Manager",
        "/org/freedesktop/a11y/Manager",
        "org.freedesktop.a11y.KeyboardMonitor",
        "SetKeyGrabs",
        Some("aua(uu)"),
        &[0u8; 8],
        t,
    )?;
    c.call(
        "org.freedesktop.a11y.Manager",
        "/org/freedesktop/a11y/Manager",
        "org.freedesktop.a11y.KeyboardMonitor",
        "WatchKeyboard",
        None,
        &[],
        t,
    )?;
    c.stream.set_read_timeout(None)?;
    std::thread::Builder::new()
        .name("stub-keys".to_owned())
        .spawn(move || {
            let mut buf = std::mem::take(&mut c.leftover);
            let mut chunk = vec![0u8; 16 * 1024];
            loop {
                let mut off = 0;
                loop {
                    match parse(&buf[off..]) {
                        Parse::Msg(m, used) => {
                            if m.mtype == SIGNAL
                                && m.member == Some("KeyEvent")
                                && m.body.len() >= 18
                            {
                                let u32_at = |i: usize| {
                                    let b: [u8; 4] = m.body[i..i + 4].try_into().unwrap_or([0; 4]);
                                    if m.big {
                                        u32::from_be_bytes(b)
                                    } else {
                                        u32::from_le_bytes(b)
                                    }
                                };
                                let released = u32_at(0) != 0;
                                let kc = [m.body[16], m.body[17]];
                                let keycode = if m.big {
                                    u16::from_be_bytes(kc)
                                } else {
                                    u16::from_le_bytes(kc)
                                };
                                if !released {
                                    let _ = lane.send(Intent {
                                        text: format!("k:{keycode}"),
                                    });
                                }
                            }
                            off += used;
                        }
                        Parse::NeedMore => break,
                        Parse::Bad => return,
                    }
                }
                buf.drain(..off);
                match c.stream.read(&mut chunk) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
        })?;
    Ok(())
}

// ---- entry point ---------------------------------------------------------------------------------

/// Everything a running stub owns; dropping it stops the drain.
#[derive(Debug)]
pub struct Stub {
    drain: Option<gorca_dbus::drain::DrainHandle>,
    _rt: tokio::runtime::Runtime,
    pub keys_error: Option<String>,
}

impl Stub {
    pub fn start(cfg: &StubConfig) -> Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("stub-actor")
            .enable_all()
            .build()?;
        let lane = start_lane(cfg)?;
        let router = Arc::new(StubRouter {
            factory: IngressFactory::new(),
            handle: rt.handle().clone(),
            lane: lane.clone(),
            a11y_addr: cfg.a11y_addr.clone(),
            calls: cfg.calls,
            routes: Mutex::new(std::collections::HashMap::new()),
            next: Mutex::new(0),
        });
        let drain = spawn_drain(DrainConfig::new(cfg.a11y_addr.clone()), router)
            .map_err(|e| anyhow::anyhow!("starting the drain: {e}"))?;
        let mut keys_error = None;
        if let Some(sess) = &cfg.session_addr
            && let Err(e) = start_keys(sess, lane)
        {
            keys_error = Some(format!("{e:#}"));
        }
        Ok(Self {
            drain: Some(drain),
            _rt: rt,
            keys_error,
        })
    }

    pub fn stop(mut self) {
        if let Some(d) = self.drain.take() {
            d.stop();
        }
    }
}

/// Parse the shared environment; used by the `gorca-stub-reader` binary.
pub fn config_from_env() -> Result<StubConfig> {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let Some(a11y_addr) = get(super::env::A11Y_BUS).or_else(|| get("AT_SPI_BUS_ADDRESS")) else {
        bail!("{} is not set", super::env::A11Y_BUS);
    };
    let flavor = get(super::env::SPEECH_BACKEND)
        .and_then(|s| Flavor::parse(&s))
        .unwrap_or(Flavor::Voicego);
    let Some(socket) = get(super::env::SPEECH_SOCKET) else {
        bail!("{} is not set", super::env::SPEECH_SOCKET);
    };
    Ok(StubConfig {
        a11y_addr,
        session_addr: get(super::env::SESSION_BUS),
        flavor,
        socket: PathBuf::from(socket),
        calls: get(super::env::STUB_CALLS).is_some_and(|v| v == "1"),
        delay: Duration::from_micros(
            get("GORCA_STUB_DELAY_US")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        ),
    })
}

/// Keep the stop flag type visible for the binary.
pub type StopFlag = Arc<AtomicBool>;
