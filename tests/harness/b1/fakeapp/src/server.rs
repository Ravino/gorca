//! The D-Bus face of the fake app: connections, the serial "main loop" thread, signals.

use std::collections::{BTreeSet, HashMap};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use futures_lite::StreamExt;
use gorca_testkit::b1::proto::{AppStats, Cmd, CmdReply, ConnKind, HangMode, ModelSpec, P2pMode};
use gorca_testkit::b1::service::ServiceModel;
use gorca_testkit::b1::world::{Effect, World};
use serde_json::json;
use tokio::runtime::Handle;
use zbus::message::Type as MsgType;
use zbus::{Connection, Message, MessageStream};
use zvariant::{OwnedValue, Value};

use crate::dispatch::{self, Args, CACHE_PATH, Ctx, DbusErr, Reply};
use crate::mono::now_ns;

const MAX_DUMP_ITEMS: usize = 1 << 20;

#[derive(Clone, Debug)]
pub struct Options {
    pub spec: ModelSpec,
    pub label: String,
    /// Address of the (private) a11y bus.
    pub bus_address: String,
    pub truth_path: Option<PathBuf>,
    /// Call `Registry.Socket.Embed` (needs a registry daemon on the bus).
    pub register: bool,
    /// Directory for the peer socket.
    pub peer_dir: PathBuf,
    /// Also connect to this (session) bus and own `own_name` on it: the fake gnome-shell of the
    /// eligibility check (SPEC 2.2, IC-22). The same process holds both connections.
    pub session_bus: Option<String>,
    pub own_name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Ready {
    pub bus_name: String,
    pub root: String,
    pub p2p_address: String,
    pub nodes: u32,
    pub landmarks: serde_json::Value,
}

/// Something the main-loop thread processes, strictly in arrival order.
enum Work {
    Call(Box<Job>),
    Control(Cmd, tokio::sync::oneshot::Sender<CmdReply>),
    Stop,
}

struct Job {
    kind: ConnKind,
    conn: Connection,
    msg: Message,
}

#[derive(Debug)]
struct DelayRule {
    member: String,
    ms: u64,
    remaining: u32,
    conn: Option<ConnKind>,
}

/// Owned by the thread; nothing here is shared.
struct Core {
    world: World,
    svc: ServiceModel,
    stats: AppStats,
    bus: Connection,
    handle: Handle,
    ctx: Ctx,
    truth: Option<std::io::BufWriter<std::fs::File>>,
    hang: (HangMode, u64),
    delays: Vec<DelayRule>,
    callers: BTreeSet<String>,
    /// Unique name of the owner of `org.a11y.atspi.Registry`, once seen.
    registry: Option<String>,
    storm_stop: Arc<AtomicBool>,
    /// Signals the storm task has emitted (folded into `signals_sent` by `stats_now`).
    storm_sent: Arc<std::sync::atomic::AtomicU64>,
    peer: Arc<PeerCtl>,
    /// P9-1: false while no AT listener registered and the model has the gate.
    listeners: Arc<AtomicBool>,
}

/// Wait until `d` has passed with sub-millisecond accuracy: sleep most of it, spin the rest.
/// This runs on the dedicated main-loop thread, never on a runtime worker.
#[allow(clippy::disallowed_methods)]
fn precise_wait(d: Duration) {
    let end = Instant::now() + d;
    // A sleep wakes late under host load (measured: +0.4 ms), so only long waits sleep, and
    // they leave 1.5 ms of spinning for accuracy.
    if d > Duration::from_micros(2_500) {
        std::thread::sleep(d.saturating_sub(Duration::from_micros(1_500)));
    }
    while Instant::now() < end {
        std::hint::spin_loop();
    }
}

fn short_iface(i: &str) -> &str {
    i.rsplit('.').next().unwrap_or(i)
}

impl Core {
    fn flush_truth(&mut self) {
        let lines = self.world.take_truth();
        if let Some(f) = self.truth.as_mut() {
            for l in lines {
                if let Ok(s) = serde_json::to_string(&l) {
                    let _ = writeln!(f, "{s}");
                }
            }
            let _ = f.flush();
        }
    }

    fn emit<B>(&self, path: &str, iface: &str, member: &str, body: &B)
    where
        B: serde::Serialize + zvariant::DynamicType,
    {
        let conn = self.bus.clone();
        let (p, i, m) = (path.to_owned(), iface.to_owned(), member.to_owned());
        // Blocking here keeps emission order identical to mutation order and charges the
        // main loop the cost of the send, as a real toolkit pays it.
        let _ = self
            .handle
            .block_on(async { conn.emit_signal(None::<&str>, p, i, m, body).await });
    }

    fn emit_event(
        &mut self,
        path: &str,
        iface: &str,
        member: &str,
        detail: &str,
        d1: i32,
        any: Value<'_>,
    ) {
        let props: HashMap<String, OwnedValue> = HashMap::new();
        self.emit(path, iface, member, &(detail, d1, 0i32, any, props));
        self.stats.signals_sent += 1;
    }

    fn emit_effects(&mut self, fx: &[Effect]) {
        const OBJ: &str = "org.a11y.atspi.Event.Object";
        let gated = self.world.spec.quirks.listener_gate && !self.listeners.load(Ordering::SeqCst);
        for e in fx {
            if gated
                && matches!(
                    e,
                    Effect::Prop { .. } | Effect::State { .. } | Effect::Children { .. }
                )
            {
                continue; // P9-1: GTK4 emits no Object event without a listener
            }
            match e {
                Effect::Prop { path, kind } => {
                    let any: Value<'_> =
                        if matches!(kind, gorca_testkit::b1::world::PropKind::Value) {
                            Value::from(0i32)
                        } else {
                            Value::from("")
                        };
                    self.emit_event(path, OBJ, "PropertyChange", kind.atspi_kind(), 0, any);
                }
                Effect::State { path, state, on } => {
                    self.emit_event(
                        path,
                        OBJ,
                        "StateChanged",
                        gorca_testkit::b1::world::state::name_of(*state),
                        i32::from(*on),
                        Value::from(0i32),
                    );
                }
                Effect::Children {
                    parent,
                    add,
                    index,
                    child,
                } => {
                    let r = (self.ctx.unique.clone(), dispatch::opath(child));
                    self.emit_event(
                        parent,
                        OBJ,
                        "ChildrenChanged",
                        if *add { "add" } else { "remove" },
                        *index,
                        Value::from(r),
                    );
                }
                Effect::Window { path, kind } => {
                    let member = match *kind {
                        "destroy" => "Destroy",
                        "activate" => "Activate",
                        "deactivate" => "Deactivate",
                        _ => "Create",
                    };
                    self.emit_event(
                        path,
                        "org.a11y.atspi.Event.Window",
                        member,
                        "",
                        0,
                        Value::from(0i32),
                    );
                }
                Effect::Focus { path } => {
                    self.emit_event(
                        path,
                        "org.a11y.atspi.Event.Focus",
                        "Focus",
                        "",
                        0,
                        Value::from(0i32),
                    );
                }
                Effect::CacheAdd(item) => {
                    let root = self
                        .world
                        .node(0)
                        .map(|n| n.path.clone())
                        .unwrap_or_default();
                    let wire = dispatch::item_wire(&self.ctx, &root, item);
                    self.emit(CACHE_PATH, "org.a11y.atspi.Cache", "AddAccessible", &wire);
                    self.stats.signals_sent += 1;
                }
                Effect::CacheRemove { path } => {
                    let r = (self.ctx.unique.clone(), dispatch::opath(path));
                    self.emit(CACHE_PATH, "org.a11y.atspi.Cache", "RemoveAccessible", &r);
                    self.stats.signals_sent += 1;
                }
            }
        }
    }

    fn stats_now(&self) -> AppStats {
        let mut s = self.stats.clone();
        s.signals_sent += self.storm_sent.load(Ordering::SeqCst);
        s.nodes_live = u64::from(self.world.live_count());
        s.bus_callers = self.callers.len() as u64;
        s.now_ns = now_ns();
        s
    }

    fn control(&mut self, cmd: &Cmd) -> CmdReply {
        match cmd {
            Cmd::Hang { mode, ms } => {
                self.hang = (*mode, *ms);
                CmdReply::ok(serde_json::Value::Null)
            }
            Cmd::DelayReplies {
                member,
                ms,
                count,
                conn,
            } => {
                self.delays.push(DelayRule {
                    member: member.clone(),
                    ms: *ms,
                    remaining: *count,
                    conn: *conn,
                });
                CmdReply::ok(serde_json::Value::Null)
            }
            Cmd::Storm {
                rate_per_s,
                duration_ms,
                distinct_nodes,
                targets,
            } => match self.world.storm_target() {
                Some(path) => {
                    self.storm_stop.store(false, Ordering::SeqCst);
                    let shape = StormShape {
                        rate: *rate_per_s,
                        duration_ms: *duration_ms,
                        distinct_nodes: *distinct_nodes as usize,
                        targets: targets.clone(),
                    };
                    spawn_storm(
                        &self.handle,
                        self.bus.clone(),
                        path,
                        shape,
                        Arc::clone(&self.storm_stop),
                        Arc::clone(&self.storm_sent),
                    );
                    CmdReply::ok(serde_json::Value::Null)
                }
                None => CmdReply::err("no terminal node in this model"),
            },
            Cmd::Malform { on, foreign_bus } => {
                self.ctx.malform = on.then(|| foreign_bus.clone());
                CmdReply::ok(serde_json::Value::Null)
            }
            Cmd::RegisterListener => {
                self.listeners.store(true, Ordering::SeqCst);
                CmdReply::ok(serde_json::Value::Null)
            }
            Cmd::ClosePeers => {
                self.peer.generation.send_modify(|g| *g += 1);
                CmdReply::ok(serde_json::Value::Null)
            }
            Cmd::PeerAcceptDelay { ms } => {
                self.peer.accept_delay_ms.store(*ms, Ordering::SeqCst);
                CmdReply::ok(serde_json::Value::Null)
            }
            Cmd::Stats => CmdReply::ok(serde_json::to_value(self.stats_now()).unwrap_or_default()),
            Cmd::Kill => std::process::exit(137),
            Cmd::Quit => CmdReply::ok(serde_json::Value::Null),
            other => {
                let (reply, fx) = self.world.apply(other, now_ns());
                self.flush_truth();
                self.emit_effects(&fx);
                reply
            }
        }
    }

    /// Is `sender` the registry? Asked of the bus daemon until it answers, then remembered.
    fn is_registry(&mut self, sender: &str) -> bool {
        if self.registry.is_none() {
            let conn = self.bus.clone();
            #[allow(clippy::disallowed_methods)] // test fixture, bounded by the timeout below
            let owner = self.handle.block_on(async {
                let call = conn.call_method(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    Some("org.freedesktop.DBus"),
                    "GetNameOwner",
                    &("org.a11y.atspi.Registry",),
                );
                tokio::time::timeout(Duration::from_millis(500), call)
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .and_then(|m| m.body().deserialize::<String>().ok())
            });
            self.registry = owner;
        }
        self.registry.as_deref() == Some(sender)
    }

    fn on_call(&mut self, job: Job) {
        let hdr = job.msg.header();
        let path = hdr.path().map(ToString::to_string).unwrap_or_default();
        let interface = hdr.interface().map(ToString::to_string).unwrap_or_default();
        let member = hdr.member().map(ToString::to_string).unwrap_or_default();
        let kind_label = match job.kind {
            ConnKind::Peer => "peer",
            ConnKind::SameBus => "samebus",
            ConnKind::Bus => "bus",
        };
        *self
            .stats
            .calls
            .entry(format!("{kind_label}/{}.{member}", short_iface(&interface)))
            .or_insert(0) += 1;
        if job.kind != ConnKind::Peer
            && let Some(s) = hdr.sender()
        {
            // The registry (it sets `Application.Id` on the root while embedding) is not a
            // reader's connection; every other sender is, whatever it calls.
            if !self.is_registry(s.as_str()) {
                self.callers.insert(s.to_string());
            }
        }
        if self.world.below_document(&path) {
            self.stats.calls_below_document += 1;
        }
        let known = path == CACHE_PATH
            || interface == "org.freedesktop.DBus.Peer"
            || self.world.is_alive_path(&path);
        if !known {
            self.stats.calls_unknown_object += 1;
        }
        if self.hang.0 == HangMode::Never {
            return;
        }
        // Service time: occupy the main loop.
        let mut ns = self.svc.sample_ns();
        if member == "GetItems" {
            ns += ServiceModel::bulk_ns(
                self.world.dump(MAX_DUMP_ITEMS).len(),
                self.world.spec.getitems_us_per_item,
            );
        }
        precise_wait(Duration::from_nanos(ns));
        if self.world.spec.quirks.dump_realised_only
            && interface == "org.a11y.atspi.Accessible"
            && member != "GetItems"
        {
            // A client walking the tree realises GTK4 contexts (P-9).
            self.world.mark_realised(&path, member == "GetChildren");
        }
        let args = decode_args(&job.msg, &interface, &member);
        let result = dispatch::handle(
            &self.ctx,
            &self.world,
            &path,
            &interface,
            &member,
            &args,
            MAX_DUMP_ITEMS,
        );
        let mut delay_ms = if self.hang.0 == HangMode::Delay {
            self.hang.1
        } else {
            0
        };
        for r in &mut self.delays {
            if r.remaining > 0
                && (r.member.is_empty() || r.member == member)
                && r.conn.is_none_or(|c| c == job.kind)
            {
                r.remaining -= 1;
                delay_ms += r.ms;
                break;
            }
        }
        self.delays.retain(|r| r.remaining > 0);
        let (conn, msg) = (job.conn, job.msg);
        let fut = async move {
            send_reply(&conn, &msg, result).await;
        };
        if delay_ms == 0 {
            self.handle.block_on(fut);
        } else {
            self.handle.spawn(async move {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                fut.await;
            });
        }
    }
}

fn decode_args(msg: &Message, interface: &str, member: &str) -> Args {
    let body = msg.body();
    let mut a = Args::default();
    match (interface, member) {
        ("org.freedesktop.DBus.Properties", "Get") => {
            if let Ok((i, p)) = body.deserialize::<(String, String)>() {
                a.s1 = Some(i);
                a.s2 = Some(p);
            }
        }
        ("org.freedesktop.DBus.Properties", "GetAll") => {
            if let Ok((i,)) = body.deserialize::<(String,)>() {
                a.s1 = Some(i);
            }
        }
        ("org.a11y.atspi.Accessible", "GetChildAtIndex") => {
            if let Ok((i,)) = body.deserialize::<(i32,)>() {
                a.i1 = Some(i);
            }
        }
        ("org.a11y.atspi.Text", "GetText") => {
            if let Ok((x, y)) = body.deserialize::<(i32, i32)>() {
                a.i1 = Some(x);
                a.i2 = Some(y);
            }
        }
        _ => {}
    }
    a
}

async fn send_reply(conn: &Connection, msg: &Message, result: Result<Reply, DbusErr>) {
    let hdr = msg.header();
    let r = match result {
        Ok(Reply::Unit) => conn.reply(&hdr, &()).await,
        Ok(Reply::Str(s)) => conn.reply(&hdr, &(s,)).await,
        Ok(Reply::U32(v)) => conn.reply(&hdr, &(v,)).await,
        Ok(Reply::I32(v)) => conn.reply(&hdr, &(v,)).await,
        Ok(Reply::Ref(v)) => conn.reply(&hdr, &(v,)).await,
        Ok(Reply::Refs(v)) => conn.reply(&hdr, &(v,)).await,
        Ok(Reply::Strs(v)) => conn.reply(&hdr, &(v,)).await,
        Ok(Reply::U32s(v)) => conn.reply(&hdr, &(v,)).await,
        Ok(Reply::Rect(v)) => conn.reply(&hdr, &(v,)).await,
        Ok(Reply::StrMap(v)) => conn.reply(&hdr, &(v,)).await,
        Ok(Reply::Relations(v)) => conn.reply(&hdr, &(v,)).await,
        Ok(Reply::Items(v)) => conn.reply(&hdr, &(v,)).await,
        Ok(Reply::Variant(v)) => conn.reply(&hdr, &(v,)).await,
        Ok(Reply::Props(v)) => conn.reply(&hdr, &(v,)).await,
        Err(e) => conn.reply_error(&hdr, e.name(), &(e.name(),)).await,
    };
    let _ = r;
}

/// What a storm sends: `rate` events a second for `duration_ms`, on the terminal node or (when
/// `distinct_nodes` is set) spread over that many made-up objects followed by `targets`.
struct StormShape {
    rate: u64,
    duration_ms: u64,
    distinct_nodes: usize,
    targets: Vec<String>,
}

fn spawn_storm(
    handle: &Handle,
    conn: Connection,
    path: String,
    shape: StormShape,
    stop: Arc<AtomicBool>,
    sent: Arc<std::sync::atomic::AtomicU64>,
) {
    let StormShape {
        rate,
        duration_ms,
        distinct_nodes,
        targets,
    } = shape;
    handle.spawn(async move {
        // The count is fixed (`rate * duration`); the clock only paces it. A starved host
        // stretches the storm but never shrinks it, so a test can assert exact counts.
        let start = Instant::now();
        let total = rate.saturating_mul(duration_ms) / 1000;
        let mut tick = tokio::time::interval(Duration::from_millis(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
        let mut emitted = 0u64;
        let mut offset = 0i32;
        let mut n = 0usize;
        while emitted < total && !stop.load(Ordering::Relaxed) {
            tick.tick().await;
            let us = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
            let due = (u128::from(rate) * u128::from(us) / 1_000_000).min(u128::from(total));
            while u128::from(emitted) < due {
                emitted += 1;
                offset = (offset + 1) % 4096;
                let props: HashMap<String, OwnedValue> = HashMap::new();
                let at = if distinct_nodes == 0 {
                    path.clone()
                } else {
                    n = (n + 1) % (distinct_nodes + targets.len());
                    match targets.get(n.wrapping_sub(distinct_nodes)) {
                        Some(t) => t.clone(),
                        None => format!("/org/a11y/atspi/accessible/flood{n}"),
                    }
                };
                let _ = conn
                    .emit_signal(
                        None::<&str>,
                        at.as_str(),
                        "org.a11y.atspi.Event.Object",
                        "TextChanged",
                        &("insert", offset, 1i32, Value::from("x"), props),
                    )
                    .await;
                sent.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
}

/// The running fake app.
pub struct FakeApp {
    _session: Option<Connection>,
    tx: Sender<Work>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub ready: Ready,
}

impl std::fmt::Debug for FakeApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeApp")
            .field("ready", &self.ready)
            .finish_non_exhaustive()
    }
}

/// Control of the peer listener (IC-19): drop all peer connections, delay the handshake.
#[derive(Debug)]
struct PeerCtl {
    generation: tokio::sync::watch::Sender<u64>,
    accept_delay_ms: std::sync::atomic::AtomicU64,
}

async fn recv_loop(
    conn: Connection,
    kind: ConnKind,
    tx: Sender<Work>,
    mut closed: Option<tokio::sync::watch::Receiver<u64>>,
) {
    let mut stream = MessageStream::from(&conn);
    loop {
        let next = match closed.as_mut() {
            Some(rx) => tokio::select! {
                m = stream.next() => m,
                _ = rx.changed() => break,
            },
            None => stream.next().await,
        };
        let Some(Ok(msg)) = next else { break };
        if msg.message_type() != MsgType::MethodCall {
            continue;
        }
        let job = Job {
            kind,
            conn: conn.clone(),
            msg,
        };
        if tx.send(Work::Call(Box::new(job))).is_err() {
            break;
        }
    }
}

impl FakeApp {
    /// Connect, serve and (optionally) register. Must be called inside a multi-thread runtime.
    pub async fn start(opts: Options) -> Result<Self> {
        let spec = opts.spec.clone();
        let bus = zbus::connection::Builder::address(opts.bus_address.as_str())
            .context("bus address")?
            .build()
            .await
            .context("connect to the a11y bus")?;
        let unique = bus
            .unique_name()
            .map(ToString::to_string)
            .ok_or_else(|| anyhow!("no unique name"))?;
        let (tx, rx) = channel::<Work>();
        let handle = Handle::current();
        let listeners = Arc::new(AtomicBool::new(false));
        let peer = Arc::new(PeerCtl {
            generation: tokio::sync::watch::channel(0u64).0,
            accept_delay_ms: std::sync::atomic::AtomicU64::new(0),
        });

        // Peer socket.
        let mut p2p_address = String::new();
        if spec.p2p == P2pMode::Peer {
            let sock = opts
                .peer_dir
                .join(format!("b1-{}-{}.sock", opts.label, std::process::id()));
            let _ = std::fs::remove_file(&sock);
            let listener = tokio::net::UnixListener::bind(&sock).context("bind peer socket")?;
            let guid = zbus::Guid::generate();
            p2p_address = format!("unix:path={},guid={guid}", sock.display());
            let tx2 = tx.clone();
            let peer2 = Arc::clone(&peer);
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let tx3 = tx2.clone();
                    let guid = guid.clone();
                    let peer3 = Arc::clone(&peer2);
                    tokio::spawn(async move {
                        let delay = peer3.accept_delay_ms.load(Ordering::SeqCst);
                        if delay > 0 {
                            tokio::time::sleep(Duration::from_millis(delay)).await;
                        }
                        let closed = peer3.generation.subscribe();
                        let built = zbus::connection::Builder::unix_stream(stream)
                            .server(guid)
                            .map(zbus::connection::Builder::p2p);
                        let Ok(b) = built else { return };
                        if let Ok(conn) = b.build().await {
                            recv_loop(conn, ConnKind::Peer, tx3, Some(closed)).await;
                        }
                    });
                }
            });
        }
        let app_bus_address = match spec.p2p {
            P2pMode::Peer => p2p_address.clone(),
            P2pMode::SameBus => opts.bus_address.clone(),
            P2pMode::None => String::new(),
        };

        let world = World::new(spec.clone(), &opts.label);
        let root = world.node(0).map(|n| n.path.clone()).unwrap_or_default();
        let ready = Ready {
            bus_name: unique.clone(),
            root: root.clone(),
            p2p_address,
            nodes: world.live_count(),
            landmarks: json!(world.landmark_paths()),
        };
        let truth = match &opts.truth_path {
            Some(p) => Some(std::io::BufWriter::new(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)?,
            )),
            None => None,
        };
        let ctx = Ctx {
            unique: unique.clone(),
            desktop: (
                "org.a11y.atspi.Registry".to_owned(),
                dispatch::opath("/org/a11y/atspi/accessible/root"),
            ),
            app_bus_address,
            p2p: spec.p2p,
            toolkit: spec.toolkit.clone(),
            malform: None,
        };
        let svc = ServiceModel::new(spec.service, spec.seed ^ 0xA5A5);
        let core = Core {
            world,
            svc,
            stats: AppStats::default(),
            bus: bus.clone(),
            handle: handle.clone(),
            ctx,
            truth,
            hang: (HangMode::Off, 0),
            delays: Vec::new(),
            callers: BTreeSet::new(),
            registry: None,
            storm_stop: Arc::new(AtomicBool::new(false)),
            storm_sent: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            peer: Arc::clone(&peer),
            listeners: Arc::clone(&listeners),
        };
        let thread = std::thread::Builder::new()
            .name("b1-main-loop".into())
            .spawn(move || main_loop(core, &rx))
            .context("spawn main loop")?;
        tokio::spawn(recv_loop(bus.clone(), ConnKind::Bus, tx.clone(), None));

        // Registry broadcast: an AT registered an event listener (lifts the P9-1 gate).
        {
            let rule = zbus::MatchRule::builder()
                .msg_type(MsgType::Signal)
                .interface("org.a11y.atspi.Registry")
                .map_err(|e| anyhow!("{e}"))?
                .member("EventListenerRegistered")
                .map_err(|e| anyhow!("{e}"))?
                .build();
            if let Ok(mut st) = MessageStream::for_match_rule(rule, &bus, None).await {
                let l = Arc::clone(&listeners);
                tokio::spawn(async move {
                    while st.next().await.is_some() {
                        l.store(true, Ordering::SeqCst);
                    }
                });
            }
        }
        let session_conn = match (&opts.session_bus, &opts.own_name) {
            (Some(addr), Some(name)) => {
                let c = zbus::connection::Builder::address(addr.as_str())
                    .context("session bus address")?
                    .name(name.as_str())
                    .context("own name")?
                    .build()
                    .await
                    .context("connect to the session bus")?;
                Some(c)
            }
            _ => None,
        };
        if opts.register {
            // Embed into the registry so the app shows up under the desktop root.
            let me = (unique.clone(), dispatch::opath(&root));
            let embed_body = (me,);
            #[allow(clippy::disallowed_methods)] // test fixture, bounded by the timeout below
            let call = bus.call_method(
                Some("org.a11y.atspi.Registry"),
                "/org/a11y/atspi/accessible/root",
                Some("org.a11y.atspi.Socket"),
                "Embed",
                &embed_body,
            );
            let reply = tokio::time::timeout(Duration::from_secs(10), call)
                .await
                .context("Socket.Embed timed out")?
                .context("Socket.Embed")?;
            let _desktop: (String, zvariant::OwnedObjectPath) = reply.body().deserialize()?;
        }
        Ok(Self {
            tx,
            thread: Some(thread),
            ready,
            _session: session_conn,
        })
    }

    /// Send a control command and wait for its answer (it queues behind earlier calls, as in a
    /// real toolkit).
    pub async fn control(&self, cmd: Cmd) -> CmdReply {
        let (rtx, rrx) = tokio::sync::oneshot::channel();
        if self.tx.send(Work::Control(cmd, rtx)).is_err() {
            return CmdReply::err("main loop is gone");
        }
        rrx.await
            .unwrap_or_else(|_| CmdReply::err("main loop is gone"))
    }
}

impl Drop for FakeApp {
    fn drop(&mut self) {
        let _ = self.tx.send(Work::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn main_loop(mut core: Core, rx: &Receiver<Work>) {
    while let Ok(w) = rx.recv() {
        match w {
            Work::Call(job) => core.on_call(*job),
            Work::Control(cmd, reply) => {
                let r = core.control(&cmd);
                let quit = matches!(cmd, Cmd::Quit);
                let _ = reply.send(r);
                if quit {
                    break;
                }
            }
            Work::Stop => break,
        }
    }
}
