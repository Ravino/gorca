//! Candidate A: a raw socket drain. One named thread does a blocking `recv` into a 1 MiB
//! buffer, frames the D-Bus stream itself, peeks at the header fields, maps the signal with
//! [`map_signal`] and pushes the events into the lock-free ring of the sending app. No zbus on
//! the hot path, no allocation per message, no method call but `Hello` and `AddMatch`.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::{ErrorKind, Read, Write};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use smallvec::SmallVec;

use super::wire::{self, ERROR, Frame, MAX_MESSAGE_BYTES, METHOD_RETURN, SIGNAL};
use super::{
    AppRoute, CACHE_IFACE, DrainConfig, DrainEnd, DrainError, DrainHandle, DrainShared, MAX_ROUTES,
    RouteDecision, Router, SignalView, map_cache_signal, map_signal, needs_hint_path, needs_path,
};
use crate::{set_recv_buffer, socket_backlog};

const EVENT_PREFIX: &[u8] = b"org.a11y.atspi.Event.";
const OBJECT_IFACE: &[u8] = b"org.a11y.atspi.Event.Object";
const NAME_OWNER_RULE: &str = "type='signal',sender='org.freedesktop.DBus',\
    interface='org.freedesktop.DBus',member='NameOwnerChanged'";
/// The read buffer; it grows for one big message and shrinks back afterwards.
const BASE_BUFFER: usize = 1024 * 1024;
const HEXCHARS: &[u8; 16] = b"0123456789abcdef";

type Ready = SyncSender<Result<(), DrainError>>;

/// Open the drain: connect, authenticate, `Hello`, `AddMatch`, then read on a dedicated thread.
/// Returns once the match rules are installed or the deadline expired.
pub fn spawn_drain(cfg: DrainConfig, router: Arc<dyn Router>) -> Result<DrainHandle, DrainError> {
    let shared = DrainShared::new();
    let socket: Arc<Mutex<Option<UnixStream>>> = Arc::new(Mutex::new(None));
    let (ready_tx, ready_rx) = sync_channel(1);
    let wait = cfg.connect_deadline + Duration::from_secs(1);
    let thread = {
        let (shared, socket) = (Arc::clone(&shared), Arc::clone(&socket));
        std::thread::Builder::new()
            .name("gorca-drain-0".to_owned())
            .spawn(move || {
                gorca_rt::lane_started(gorca_rt::Lane::Drain(0));
                run(&cfg, &router, &shared, &socket, &ready_tx);
            })?
    };
    let wake: Box<dyn Fn() + Send + Sync> = {
        let socket = Arc::clone(&socket);
        Box::new(move || {
            let g = socket.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(s) = g.as_ref() {
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
        })
    };
    let handle = DrainHandle::new(Arc::clone(&shared), wake, vec![thread]);
    match ready_rx.recv_timeout(wait) {
        Ok(Ok(())) => Ok(handle),
        Ok(Err(e)) => {
            handle.stop();
            Err(e)
        }
        Err(_) => {
            // The thread may be stuck in `connect`: tell it to stop and let it go (detached),
            // never join something that may not return.
            handle.shared.stopping.store(true, Ordering::Release);
            (handle.wake)();
            Err(DrainError::Deadline)
        }
    }
}

fn run(
    cfg: &DrainConfig,
    router: &Arc<dyn Router>,
    shared: &Arc<DrainShared>,
    socket: &Arc<Mutex<Option<UnixStream>>>,
    ready: &Ready,
) {
    let r = catch_unwind(AssertUnwindSafe(|| {
        run_inner(cfg, router, shared, socket, ready);
    }));
    if r.is_err() {
        let _ = ready.try_send(Err(DrainError::Connect("drain thread panicked".to_owned())));
        shared.set_end(DrainEnd::Disconnected("drain thread panicked".to_owned()));
    }
}

enum Fatal {
    Setup(DrainError),
    /// EOF, a socket error or garbage on the wire.
    Closed(String),
}

fn run_inner(
    cfg: &DrainConfig,
    router: &Arc<dyn Router>,
    shared: &Arc<DrainShared>,
    socket: &Arc<Mutex<Option<UnixStream>>>,
    ready: &Ready,
) {
    let deadline = Instant::now() + cfg.connect_deadline;
    let result = (|| -> Result<(), Fatal> {
        let stream = connect(&cfg.address).map_err(|e| Fatal::Setup(DrainError::Connect(e)))?;
        if let Some(bytes) = cfg.rcvbuf_bytes {
            let _ = set_recv_buffer(&stream, bytes);
        }
        {
            let clone = stream
                .try_clone()
                .map_err(|e| Fatal::Setup(DrainError::Connect(e.to_string())))?;
            *socket.lock().unwrap_or_else(PoisonError::into_inner) = Some(clone);
        }
        if shared.is_stopping() {
            return Err(Fatal::Closed("stopped".to_owned()));
        }
        let mut conn = Conn::new(cfg, router, shared, stream, deadline);
        conn.authenticate()?;
        conn.run_loop(ready)
    })();
    if shared.is_stopping() {
        let _ = ready.try_send(Err(DrainError::Connect("stopped".to_owned())));
        return;
    }
    match result {
        Ok(()) => {}
        Err(Fatal::Setup(e)) => {
            let _ = ready.try_send(Err(e));
        }
        Err(Fatal::Closed(why)) => {
            // Before the rules were installed this is a start-up failure.
            let _ = ready.try_send(Err(DrainError::Connect(why.clone())));
            shared.set_end(DrainEnd::Disconnected(why));
        }
    }
}

fn connect(address: &str) -> Result<UnixStream, String> {
    gorca_rt::guard::check(address);
    for one in address.split(';') {
        let Some(rest) = one.strip_prefix("unix:") else {
            continue;
        };
        for kv in rest.split(',') {
            if let Some(path) = kv.strip_prefix("path=") {
                return UnixStream::connect(path).map_err(|e| e.to_string());
            }
            if let Some(name) = kv.strip_prefix("abstract=") {
                let addr =
                    SocketAddr::from_abstract_name(name.as_bytes()).map_err(|e| e.to_string())?;
                return UnixStream::connect_addr(&addr).map_err(|e| e.to_string());
            }
        }
    }
    Err(format!("unsupported bus address {address:?}"))
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Phase {
    AwaitHello,
    AwaitMatches { left: usize },
    Running,
}

/// FNV-1a over the bytes of a bus name. Names are assigned by the bus, not chosen by peers.
#[derive(Default)]
struct NameHasher(u64);

impl Hasher for NameHasher {
    fn write(&mut self, bytes: &[u8]) {
        let mut h = if self.0 == 0 {
            0xcbf2_9ce4_8422_2325
        } else {
            self.0
        };
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.0 = h;
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// Outcome of resolving the sender of one signal.
enum Routed {
    Idx(usize),
    NotApp,
    Retry,
}

#[derive(Default)]
struct Batch {
    deferred: u64,
    messages: u64,
    ignored: u64,
    unrouted: u64,
    malformed: u64,
    pushed: u64,
}

struct Conn<'a> {
    cfg: &'a DrainConfig,
    router: &'a Arc<dyn Router>,
    shared: &'a Arc<DrainShared>,
    stream: UnixStream,
    deadline: Instant,
    buf: Vec<u8>,
    base: usize,
    filled: usize,
    /// Bytes of an oversized message still to be discarded.
    skip: usize,
    phase: Phase,
    rules: Vec<String>,
    routes: Vec<Option<AppRoute>>,
    /// Sender name of `routes[i]`, kept for the one-entry fast path.
    names: Vec<Box<[u8]>>,
    index: HashMap<Box<[u8]>, usize, BuildHasherDefault<NameHasher>>,
    last: Option<usize>,
    mapped: SmallVec<[gorca_model::NodeEvent; 2]>,
    shedding: bool,
    batch: Batch,
}

impl<'a> Conn<'a> {
    fn new(
        cfg: &'a DrainConfig,
        router: &'a Arc<dyn Router>,
        shared: &'a Arc<DrainShared>,
        stream: UnixStream,
        deadline: Instant,
    ) -> Self {
        let base = cfg.recv_bytes.max(BASE_BUFFER);
        let mut rules = cfg.match_rules.clone();
        // A departing application loses its cached route (SPEC AU-2).
        rules.push(NAME_OWNER_RULE.to_owned());
        Self {
            cfg,
            router,
            shared,
            stream,
            deadline,
            buf: vec![0; base],
            base,
            filled: 0,
            skip: 0,
            phase: Phase::AwaitHello,
            rules,
            routes: Vec::new(),
            names: Vec::new(),
            index: HashMap::default(),
            last: None,
            mapped: SmallVec::new(),
            shedding: false,
            batch: Batch::default(),
        }
    }

    fn remaining(&self) -> Result<Duration, Fatal> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or(Fatal::Setup(DrainError::Deadline))
    }

    /// SASL EXTERNAL, then `BEGIN` and `Hello` in one write.
    fn authenticate(&mut self) -> Result<(), Fatal> {
        let io = |e: std::io::Error| Fatal::Setup(DrainError::Connect(e.to_string()));
        let uid = std::fs::metadata("/proc/self")
            .map_err(io)?
            .uid()
            .to_string();
        let mut hex = String::with_capacity(uid.len() * 2);
        for b in uid.bytes() {
            hex.push(char::from(HEXCHARS[usize::from(b >> 4)]));
            hex.push(char::from(HEXCHARS[usize::from(b & 15)]));
        }
        self.stream
            .write_all(format!("\0AUTH EXTERNAL {hex}\r\n").as_bytes())
            .map_err(io)?;
        let mut line = Vec::with_capacity(64);
        let mut byte = [0u8; 1];
        while !line.ends_with(b"\r\n") {
            self.stream
                .set_read_timeout(Some(self.remaining()?))
                .map_err(io)?;
            match self.stream.read(&mut byte) {
                Ok(0) => return Err(Fatal::Closed("closed during authentication".to_owned())),
                Ok(_) => line.push(byte[0]),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return Err(Fatal::Setup(DrainError::Deadline));
                }
                Err(e) => return Err(io(e)),
            }
            if line.len() > 512 {
                return Err(Fatal::Setup(DrainError::Connect(
                    "authentication reply too long".to_owned(),
                )));
            }
        }
        if !line.starts_with(b"OK") {
            return Err(Fatal::Setup(DrainError::Connect(format!(
                "authentication refused: {}",
                String::from_utf8_lossy(&line).trim_end()
            ))));
        }
        let mut out = b"BEGIN\r\n".to_vec();
        out.extend_from_slice(&wire::bus_call(1, "Hello", None, &[]));
        self.stream.write_all(&out).map_err(io)
    }

    fn on_reply(&mut self, serial: u32, is_error: bool, ready: &Ready) -> Result<(), Fatal> {
        let refused = |what: &str| Fatal::Setup(DrainError::Connect(format!("{what} refused")));
        match self.phase {
            Phase::AwaitHello if serial == 1 => {
                if is_error {
                    return Err(refused("Hello"));
                }
                let mut out = Vec::new();
                for (i, rule) in self.rules.iter().enumerate() {
                    out.extend_from_slice(&wire::add_match(
                        2 + u32::try_from(i).unwrap_or(0),
                        rule,
                    ));
                }
                self.stream
                    .write_all(&out)
                    .map_err(|e| Fatal::Setup(DrainError::Connect(e.to_string())))?;
                self.phase = Phase::AwaitMatches {
                    left: self.rules.len(),
                };
                self.check_ready(ready)
            }
            Phase::AwaitMatches { left } if serial >= 2 => {
                if is_error {
                    return Err(refused("AddMatch"));
                }
                self.phase = Phase::AwaitMatches {
                    left: left.saturating_sub(1),
                };
                self.check_ready(ready)
            }
            Phase::AwaitHello | Phase::AwaitMatches { .. } | Phase::Running => Ok(()),
        }
    }

    fn check_ready(&mut self, ready: &Ready) -> Result<(), Fatal> {
        if self.phase == (Phase::AwaitMatches { left: 0 }) {
            self.phase = Phase::Running;
            self.stream
                .set_read_timeout(None)
                .map_err(|e| Fatal::Setup(DrainError::Connect(e.to_string())))?;
            let _ = ready.try_send(Ok(()));
        }
        Ok(())
    }

    fn run_loop(&mut self, ready: &Ready) -> Result<(), Fatal> {
        loop {
            if self.phase != Phase::Running {
                self.stream
                    .set_read_timeout(Some(self.remaining()?))
                    .map_err(|e| Fatal::Setup(DrainError::Connect(e.to_string())))?;
            }
            let room = self.buf.len() - self.filled;
            let n = match self.stream.read(&mut self.buf[self.filled..]) {
                Ok(0) => return Err(Fatal::Closed("end of stream".to_owned())),
                Ok(n) => n,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e)
                    if self.phase != Phase::Running
                        && matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    return Err(Fatal::Setup(DrainError::Deadline));
                }
                Err(e) => return Err(Fatal::Closed(e.to_string())),
            };
            let c = &self.shared.counters;
            c.bytes.fetch_add(n as u64, Ordering::Relaxed);
            c.batches.fetch_add(1, Ordering::Relaxed);
            self.filled += n;
            // A read that did not fill the buffer emptied the socket: nothing to measure.
            if n == room {
                self.sample_backlog();
            }
            let r = self.process(ready);
            self.flush();
            r?;
        }
    }

    /// D-2: `SIOCINQ` after every batch, with hysteresis for the shed signal.
    fn sample_backlog(&mut self) {
        let Ok(backlog) = socket_backlog(&self.stream) else {
            return;
        };
        self.shared.raise_backlog(backlog);
        let c = &self.shared.counters;
        if !self.shedding && backlog >= self.cfg.shed_watermark {
            self.shedding = true;
            c.shed_starts.fetch_add(1, Ordering::Relaxed);
        } else if self.shedding && backlog < self.cfg.shed_watermark / 2 {
            self.shedding = false;
        }
    }

    fn flush(&mut self) {
        let c = &self.shared.counters;
        let b = std::mem::take(&mut self.batch);
        for (counter, v) in [
            (&c.messages, b.messages),
            (&c.ignored, b.ignored),
            (&c.unrouted, b.unrouted),
            (&c.deferred, b.deferred),
            (&c.malformed, b.malformed),
            (&c.pushed, b.pushed),
        ] {
            if v != 0 {
                counter.fetch_add(v, Ordering::Relaxed);
            }
        }
    }

    /// Frame and handle every complete message in the buffer; keep the partial tail.
    fn process(&mut self, ready: &Ready) -> Result<(), Fatal> {
        let mut buf = std::mem::take(&mut self.buf);
        let mut pos = 0usize;
        let mut need = 0usize;
        let mut result = Ok(());
        loop {
            if self.skip > 0 {
                let d = self.skip.min(self.filled - pos);
                pos += d;
                self.skip -= d;
                if self.skip > 0 {
                    break;
                }
            }
            let avail = self.filled - pos;
            if avail < 16 {
                break;
            }
            let Ok(frame) = wire::peek(&buf[pos..self.filled]) else {
                result = Err(Fatal::Closed("not a D-Bus stream".to_owned()));
                break;
            };
            if frame.total > MAX_MESSAGE_BYTES {
                self.batch.malformed += 1;
                self.skip = frame.total;
                continue;
            }
            if avail < frame.total {
                need = frame.total;
                break;
            }
            if let Err(e) = self.handle(&buf[pos..pos + frame.total], &frame, ready) {
                result = Err(e);
                break;
            }
            pos += frame.total;
        }
        if pos > 0 {
            buf.copy_within(pos..self.filled, 0);
            self.filled -= pos;
        }
        if need > buf.len() {
            buf.resize(need, 0);
        } else if buf.len() > self.base && self.filled <= self.base / 2 && need <= self.base {
            buf.truncate(self.base);
            buf.shrink_to_fit();
        }
        self.buf = buf;
        result
    }

    fn handle(&mut self, msg: &[u8], f: &Frame, ready: &Ready) -> Result<(), Fatal> {
        match f.mtype {
            SIGNAL => {
                self.handle_signal(msg, f);
                Ok(())
            }
            METHOD_RETURN | ERROR if self.phase != Phase::Running => {
                if let Ok(h) = wire::fields(msg, f)
                    && let Some(serial) = h.reply_serial
                    && usize::try_from(serial)
                        .is_ok_and(|s| (1..=1 + self.rules.len()).contains(&s))
                {
                    return self.on_reply(serial, f.mtype == ERROR, ready);
                }
                self.batch.messages += 1;
                self.batch.ignored += 1;
                Ok(())
            }
            _ => {
                self.batch.messages += 1;
                self.batch.ignored += 1;
                Ok(())
            }
        }
    }

    fn handle_signal(&mut self, msg: &[u8], f: &Frame) {
        self.batch.messages += 1;
        let Ok(h) = wire::fields(msg, f) else {
            self.batch.malformed += 1;
            return;
        };
        let (Some(sender), Some(path), Some(iface), Some(member)) =
            (h.sender, h.path, h.interface, h.member)
        else {
            self.batch.ignored += 1;
            return;
        };
        let body = msg.get(f.body_at..).unwrap_or(&[]);
        if iface == b"org.freedesktop.DBus" && member == b"NameOwnerChanged" {
            self.batch.ignored += 1;
            self.name_owner_changed(body, f.big);
            return;
        }
        let is_cache = iface == CACHE_IFACE.as_bytes();
        if !iface.starts_with(EVENT_PREFIX) && !is_cache {
            self.batch.ignored += 1;
            return;
        }
        let (Ok(path), Ok(iface_s), Ok(member)) = (
            std::str::from_utf8(path),
            std::str::from_utf8(iface),
            std::str::from_utf8(member),
        ) else {
            self.batch.malformed += 1;
            return;
        };
        let ridx = match self.route(sender) {
            Routed::Idx(i) => i,
            Routed::NotApp => {
                self.batch.unrouted += 1;
                return;
            }
            Routed::Retry => {
                // The router is still registering this sender (or backing off after a failure).
                // Nothing is cached, so the next event asks again; this one is handed to the
                // router to park, or dropped and counted. The drain never waits.
                self.batch.deferred += 1;
                if let Ok(name) = std::str::from_utf8(sender) {
                    let view = SignalView {
                        interface: iface_s,
                        member,
                        path,
                        body,
                        big_endian: f.big,
                    };
                    self.router.park(name, &view);
                }
                return;
            }
        };
        if iface == OBJECT_IFACE
            && reads_body(member)
            && !h.signature.is_some_and(|s| s.starts_with(b"siiva"))
        {
            self.batch.malformed += 1;
            return;
        }
        let Some(Some(route)) = self.routes.get(ridx) else {
            self.batch.unrouted += 1;
            return;
        };
        let view = SignalView {
            interface: iface_s,
            member,
            path,
            body,
            big_endian: f.big,
        };
        self.mapped.clear();
        let mapped = if is_cache {
            let sender_s = std::str::from_utf8(sender).unwrap_or("");
            map_cache_signal(route.app, sender_s, &view, &mut self.mapped)
        } else {
            map_signal(route.app, &view, &mut self.mapped)
        };
        if mapped.is_err() {
            self.batch.malformed += 1;
            return;
        }
        if self.mapped.is_empty() {
            self.batch.ignored += 1;
        }
        for ev in self.mapped.drain(..) {
            if let Some(node) = needs_path(&ev) {
                self.router.focus_seen(route.app, node, path);
            } else if route.hints.load(Ordering::Relaxed)
                && let Some(node) = needs_hint_path(&ev)
            {
                self.router.hint_seen(route.app, node, path);
            }
            route.tx.push(ev);
            self.batch.pushed += 1;
        }
    }

    /// Index into `routes` of the route for `sender`, asking the router once per distinct name.
    /// `Route` and `NotAnApp` answers are cached; `Retry` never is (a transient failure must not
    /// mute an application for the life of the connection).
    fn route(&mut self, sender: &[u8]) -> Routed {
        let cached = |routes: &[Option<AppRoute>], idx: usize| match routes.get(idx) {
            Some(Some(_)) => Routed::Idx(idx),
            Some(None) | None => Routed::NotApp,
        };
        if let Some(idx) = self.last
            && self.names.get(idx).is_some_and(|n| &**n == sender)
            && !self.drop_if_closed(idx, sender)
        {
            return cached(&self.routes, idx);
        }
        let known = self
            .index
            .get(sender)
            .copied()
            .filter(|&idx| !self.drop_if_closed(idx, sender));
        let idx = if let Some(idx) = known {
            idx
        } else {
            let decision = match std::str::from_utf8(sender) {
                Ok(s) => self.router.resolve(s),
                Err(_) => RouteDecision::NotAnApp,
            };
            let route = match decision {
                RouteDecision::Route(r) => Some(r),
                RouteDecision::NotAnApp => None,
                RouteDecision::Retry => return Routed::Retry,
            };
            if self.routes.len() >= MAX_ROUTES {
                self.routes.clear();
                self.names.clear();
                self.index.clear();
                self.last = None;
            }
            self.routes.push(route);
            self.names.push(sender.into());
            let idx = self.routes.len() - 1;
            self.index.insert(sender.into(), idx);
            idx
        };
        self.last = Some(idx);
        cached(&self.routes, idx)
    }

    /// A cached route whose ingress was closed (the application was retired or evicted) is
    /// stale: forget it, so the router is asked again on this sender's next event instead of the
    /// application staying muted. `true` if the entry was dropped.
    fn drop_if_closed(&mut self, idx: usize, sender: &[u8]) -> bool {
        let closed = matches!(self.routes.get(idx), Some(Some(r)) if r.tx.is_closed());
        if closed {
            self.index.remove(sender);
            if let Some(slot) = self.routes.get_mut(idx) {
                *slot = None;
            }
            if self.last == Some(idx) {
                self.last = None;
            }
        }
        closed
    }

    /// A unique name that lost its owner: drop its route so its ingress can go away (AU-2).
    fn name_owner_changed(&mut self, body: &[u8], big: bool) {
        let Some([name, _old, new]) = wire::strings::<3>(body, big) else {
            return;
        };
        if !name.starts_with(b":") || !new.is_empty() {
            return;
        }
        if let Some(idx) = self.index.remove(name) {
            if let Some(slot) = self.routes.get_mut(idx) {
                *slot = None;
            }
            if self.last == Some(idx) {
                self.last = None;
            }
        }
    }
}

/// Members of `Event.Object` whose mapping reads the `siiva{sv}` body.
fn reads_body(member: &str) -> bool {
    matches!(
        member,
        "StateChanged"
            | "PropertyChange"
            | "ChildrenChanged"
            | "TextChanged"
            | "TextCaretMoved"
            | "ActiveDescendantChanged"
            | "Announcement"
    )
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc::channel;

    use gorca_ingress::{IngressConfig, IngressReceiver, ingress};
    use gorca_model::{AppId, ConnectorId};

    use super::*;
    use crate::drain::wire::testutil::message;

    struct CountingRouter {
        calls: AtomicUsize,
        route: AppRoute,
    }

    impl Router for CountingRouter {
        fn route(&self, sender: &str) -> Option<AppRoute> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            (sender == ":1.5").then(|| self.route.clone())
        }
    }

    fn event(sender: &str) -> Vec<u8> {
        message(
            SIGNAL,
            9,
            &[
                (1, b'o', "/org/a11y/atspi/accessible/7"),
                (2, b's', "org.a11y.atspi.Event.Object"),
                (3, b's', "SelectionChanged"),
                (7, b's', sender),
            ],
            None,
            None,
            &[],
        )
    }

    fn name_lost(name: &str) -> Vec<u8> {
        let mut body = Vec::new();
        for s in [name, name, ""] {
            while !body.len().is_multiple_of(4) {
                body.push(0);
            }
            body.extend_from_slice(&(s.len() as u32).to_le_bytes());
            body.extend_from_slice(s.as_bytes());
            body.push(0);
        }
        message(
            SIGNAL,
            10,
            &[
                (1, b'o', "/org/freedesktop/DBus"),
                (2, b's', "org.freedesktop.DBus"),
                (3, b's', "NameOwnerChanged"),
                (7, b's', "org.freedesktop.DBus"),
            ],
            None,
            Some("sss"),
            &body,
        )
    }

    type Rules = Arc<Mutex<Vec<String>>>;

    /// A broker that does SASL, answers `Hello` and `AddMatch`, then relays what the test sends.
    fn fake_broker(path: &std::path::Path) -> (std::sync::mpsc::Sender<Vec<u8>>, Rules) {
        let listener = UnixListener::bind(path).unwrap();
        let (tx, rx) = channel::<Vec<u8>>();
        let rules: Rules = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&rules);
        std::thread::spawn(move || {
            let Ok((mut s, _)) = listener.accept() else {
                return;
            };
            let mut line = Vec::new();
            let mut b = [0u8; 1];
            loop {
                if s.read(&mut b).unwrap_or(0) == 0 {
                    return;
                }
                line.push(b[0]);
                if line.ends_with(b"\r\n") {
                    if line.starts_with(b"BEGIN") {
                        break;
                    }
                    s.write_all(b"OK 00112233445566778899aabbccddeeff\r\n")
                        .unwrap();
                    line.clear();
                }
            }
            let mut buf: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 4096];
            let mut calls = 0;
            while calls < 6 {
                // Hello plus the four default rules plus the ownership rule.
                let n = s.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&chunk[..n]);
                while buf.len() >= 16 {
                    let f = wire::peek(&buf).unwrap();
                    if buf.len() < f.total {
                        break;
                    }
                    let msg: Vec<u8> = buf.drain(..f.total).collect();
                    let h = wire::fields(&msg, &f).unwrap();
                    if h.member == Some(&b"AddMatch"[..]) {
                        let [rule] = wire::strings::<1>(&msg[f.body_at..], false).unwrap();
                        seen.lock()
                            .unwrap()
                            .push(String::from_utf8_lossy(rule).into_owned());
                    }
                    let serial = u32::from_le_bytes(msg[8..12].try_into().unwrap());
                    s.write_all(&message(
                        METHOD_RETURN,
                        1000 + serial,
                        &[],
                        Some(serial),
                        None,
                        &[],
                    ))
                    .unwrap();
                    calls += 1;
                }
            }
            for m in rx {
                if s.write_all(&m).is_err() {
                    return;
                }
            }
        });
        (tx, rules)
    }

    fn wait(rx: &IngressReceiver, want: usize) -> bool {
        let end = Instant::now() + Duration::from_secs(5);
        let mut got = 0;
        while Instant::now() < end {
            got += rx.drain().events.len();
            if got >= want {
                return true;
            }
            #[allow(clippy::disallowed_methods)]
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }

    /// SPEC AU-2: a unique name that lost its owner loses its cached route; the router is asked
    /// again if the name ever speaks again.
    #[test]
    fn the_route_is_dropped_when_the_name_loses_its_owner() {
        let dir = std::env::temp_dir().join(format!("gorca-drain-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bus");
        let (send, rules) = fake_broker(&path);
        let (tx, rx) = ingress(IngressConfig::default());
        let router = Arc::new(CountingRouter {
            calls: AtomicUsize::new(0),
            route: AppRoute {
                app: AppId {
                    connector: ConnectorId(0),
                    serial: 1,
                },
                tx,
                hints: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
        });
        let mut cfg = DrainConfig::new(format!("unix:path={}", path.display()));
        cfg.connect_deadline = Duration::from_secs(3);
        let drain = spawn_drain(cfg, Arc::clone(&router) as Arc<dyn Router>).unwrap();
        assert!(
            rules
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.contains("NameOwnerChanged")),
            "the drain must watch name ownership: {:?}",
            rules.lock().unwrap()
        );
        send.send(event(":1.5")).unwrap();
        assert!(wait(&rx, 1));
        send.send(event(":1.5")).unwrap();
        assert!(wait(&rx, 1));
        assert_eq!(router.calls.load(Ordering::SeqCst), 1, "route is cached");
        send.send(name_lost(":1.5")).unwrap();
        send.send(event(":1.5")).unwrap();
        assert!(wait(&rx, 1));
        assert_eq!(
            router.calls.load(Ordering::SeqCst),
            2,
            "the router is asked again after the owner left"
        );
        let stats = drain.stop();
        assert_eq!(stats.malformed, 0);
        drop(send);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn cache_signal(sender: &str, member: &str, refs: &[(&str, &str)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (bus, path) in refs {
            while !body.len().is_multiple_of(8) {
                body.push(0);
            }
            for s in [bus, path] {
                while !body.len().is_multiple_of(4) {
                    body.push(0);
                }
                body.extend_from_slice(&(s.len() as u32).to_le_bytes());
                body.extend_from_slice(s.as_bytes());
                body.push(0);
            }
        }
        message(
            SIGNAL,
            11,
            &[
                (1, b'o', "/org/a11y/atspi/cache"),
                (2, b's', "org.a11y.atspi.Cache"),
                (3, b's', member),
                (7, b's', sender),
            ],
            None,
            None,
            &body,
        )
    }

    /// B-1 PW-8 through the real drain: cache signals become events, and a ref that names another
    /// bus name is malformed (counted, never pushed).
    #[test]
    fn cache_signals_map_to_events_and_foreign_refs_are_counted_malformed() {
        let dir = std::env::temp_dir().join(format!("gorca-drain-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bus");
        let _ = std::fs::remove_file(&path);
        let (send, _rules) = fake_broker(&path);
        let (tx, rx) = ingress(IngressConfig::default());
        let router = Arc::new(CountingRouter {
            calls: AtomicUsize::new(0),
            route: AppRoute {
                app: AppId {
                    connector: ConnectorId(0),
                    serial: 1,
                },
                tx,
                hints: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
        });
        let mut cfg = DrainConfig::new(format!("unix:path={}", path.display()));
        cfg.connect_deadline = Duration::from_secs(3);
        let drain = spawn_drain(cfg, Arc::clone(&router) as Arc<dyn Router>).unwrap();
        // An honest remove, an honest add, then one remove and one add naming another bus name.
        send.send(cache_signal(
            ":1.5",
            "RemoveAccessible",
            &[(":1.5", "/x/9")],
        ))
        .unwrap();
        send.send(cache_signal(
            ":1.5",
            "AddAccessible",
            &[(":1.5", "/x/new"), (":1.5", "/root"), (":1.5", "/x/parent")],
        ))
        .unwrap();
        send.send(cache_signal(
            ":1.5",
            "RemoveAccessible",
            &[(":1.99", "/x/9")],
        ))
        .unwrap();
        send.send(cache_signal(
            ":1.5",
            "AddAccessible",
            &[
                (":1.5", "/x/new"),
                (":1.5", "/root"),
                (":1.99", "/x/parent"),
            ],
        ))
        .unwrap();
        assert!(wait(&rx, 2), "the two honest signals reach the ingress");
        let stats = drain.stop();
        assert_eq!(stats.malformed, 2, "{stats:?}");
        assert_eq!(stats.pushed, 2);
        drop(send);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression (review F4): the router answers `Retry` once (registration in flight, core
    /// busy). The answer must not be cached: the sender's later events reach the ingress.
    struct FlakyRouter {
        calls: AtomicUsize,
        parked: AtomicUsize,
        route: AppRoute,
    }

    impl Router for FlakyRouter {
        fn route(&self, _sender: &str) -> Option<AppRoute> {
            None
        }

        fn resolve(&self, _sender: &str) -> RouteDecision {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                RouteDecision::Retry
            } else {
                RouteDecision::Route(self.route.clone())
            }
        }

        fn park(&self, _sender: &str, _signal: &SignalView<'_>) {
            self.parked.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn a_transient_route_failure_does_not_mute_the_sender() {
        let dir = std::env::temp_dir().join(format!("gorca-drain-retry-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bus");
        let _ = std::fs::remove_file(&path);
        let (send, _rules) = fake_broker(&path);
        let (tx, rx) = ingress(IngressConfig::default());
        let router = Arc::new(FlakyRouter {
            calls: AtomicUsize::new(0),
            parked: AtomicUsize::new(0),
            route: AppRoute {
                app: AppId {
                    connector: ConnectorId(0),
                    serial: 1,
                },
                tx,
                hints: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
        });
        let mut cfg = DrainConfig::new(format!("unix:path={}", path.display()));
        cfg.connect_deadline = Duration::from_secs(3);
        let drain = spawn_drain(cfg, Arc::clone(&router) as Arc<dyn Router>).unwrap();
        send.send(event(":1.5")).unwrap(); // Retry: parked/dropped, not cached
        for _ in 0..50 {
            send.send(event(":1.5")).unwrap();
        }
        let delivered = wait(&rx, 1);
        let stats = drain.stop();
        drop(send);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(delivered, "later events of the app were dropped");
        assert_eq!(stats.deferred, 1, "exactly the Retry event was deferred");
        assert_eq!(router.parked.load(Ordering::SeqCst), 1);
        assert!(router.calls.load(Ordering::SeqCst) >= 2);
    }

    /// `NotAnApp` is cached: the router is asked once.
    #[test]
    fn not_an_app_is_cached_for_the_connection() {
        let dir = std::env::temp_dir().join(format!("gorca-drain-notapp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bus");
        let _ = std::fs::remove_file(&path);
        let (send, _rules) = fake_broker(&path);
        let (tx, _rx) = ingress(IngressConfig::default());
        let router = Arc::new(CountingRouter {
            calls: AtomicUsize::new(0),
            route: AppRoute {
                app: AppId {
                    connector: ConnectorId(0),
                    serial: 1,
                },
                tx,
                hints: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
        });
        let mut cfg = DrainConfig::new(format!("unix:path={}", path.display()));
        cfg.connect_deadline = Duration::from_secs(3);
        let drain = spawn_drain(cfg, Arc::clone(&router) as Arc<dyn Router>).unwrap();
        for _ in 0..20 {
            send.send(event(":1.99")).unwrap();
        }
        #[allow(clippy::disallowed_methods)]
        std::thread::sleep(Duration::from_millis(300));
        let stats = drain.stop();
        drop(send);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(router.calls.load(Ordering::SeqCst), 1);
        assert_eq!(stats.unrouted, 20);
    }

    /// Regression (review R3-1c): when an application is evicted, its ingress closes; the drain
    /// must notice, forget the cached route and ask the router again, instead of pushing into the
    /// dead ingress for the rest of the connection (a permanent mute).
    struct GenRouter {
        calls: AtomicUsize,
        first: AppRoute,
        second: AppRoute,
    }

    impl Router for GenRouter {
        fn route(&self, _sender: &str) -> Option<AppRoute> {
            None
        }

        fn resolve(&self, _sender: &str) -> RouteDecision {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                RouteDecision::Route(self.first.clone())
            } else {
                RouteDecision::Route(self.second.clone())
            }
        }
    }

    #[test]
    fn an_evicted_apps_cached_route_is_dropped_and_asked_again() {
        let dir = std::env::temp_dir().join(format!("gorca-drain-evict-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bus");
        let _ = std::fs::remove_file(&path);
        let (send, _rules) = fake_broker(&path);
        let app = |serial| AppId {
            connector: ConnectorId(0),
            serial,
        };
        let (tx1, rx1) = ingress(IngressConfig::default());
        let (tx2, rx2) = ingress(IngressConfig::default());
        let router = Arc::new(GenRouter {
            calls: AtomicUsize::new(0),
            first: AppRoute {
                app: app(1),
                tx: tx1,
                hints: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
            second: AppRoute {
                app: app(2),
                tx: tx2,
                hints: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
        });
        let mut cfg = DrainConfig::new(format!("unix:path={}", path.display()));
        cfg.connect_deadline = Duration::from_secs(3);
        let drain = spawn_drain(cfg, Arc::clone(&router) as Arc<dyn Router>).unwrap();
        send.send(event(":1.5")).unwrap();
        assert!(wait(&rx1, 1), "the first route works");
        // The application is evicted: the core drops its receiver.
        drop(rx1);
        // Its next events must reach the re-resolved route.
        for _ in 0..20 {
            send.send(event(":1.5")).unwrap();
        }
        let delivered = wait(&rx2, 1);
        let _ = drain.stop();
        drop(send);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(delivered, "the evicted app stayed muted");
        assert_eq!(router.calls.load(Ordering::SeqCst), 2);
    }
}
