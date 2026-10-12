//! In-process mock broker. It speaks just enough of the bus protocol for a drain to connect
//! (SASL EXTERNAL, `Hello`, `AddMatch`) and then lets the harness push signal bytes at it with
//! full control: pacing, fragmentation, and the **F-3 behaviour of dbus-broker**: the bytes a
//! slow reader has not consumed are queued without bound in the broker, and the reader is
//! disconnected once its quota is exceeded.

use std::io::{ErrorKind, Read};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use crate::clock::Rng;
use crate::sys;
use crate::wire::{self, Enc, Fields, METHOD_CALL, METHOD_RETURN, Parse};

#[derive(Clone, Debug)]
pub struct MockOpts {
    /// Unread bytes a reader may accumulate in the broker before it is disconnected (F-3).
    pub quota: usize,
    /// Answer `AddMatch` (false simulates a hung broker).
    pub reply_addmatch: bool,
    /// Write to the reader in random pieces of at most this many bytes (framing tests).
    pub chunk: Option<usize>,
    /// Kernel send buffer of the broker side, to keep the in-kernel backlog small and the
    /// userspace queue (what dbus-broker does) the dominant one.
    pub sndbuf: Option<usize>,
}

impl Default for MockOpts {
    fn default() -> Self {
        Self {
            quota: 64 * 1024 * 1024,
            reply_addmatch: true,
            chunk: None,
            sndbuf: Some(256 * 1024),
        }
    }
}

#[derive(Debug, Default)]
pub struct LinkStats {
    pub written: AtomicU64,
    pub queued: AtomicU64,
    pub queued_hwm: AtomicU64,
    pub outq_hwm: AtomicU64,
    pub disconnected: AtomicBool,
    /// The reader closed its end (EOF seen by the server thread).
    pub reader_closed: AtomicBool,
    pub disconnect_ns: AtomicU64,
}

#[derive(Debug)]
struct LinkState {
    sock: UnixStream,
    q: Vec<u8>,
    head: usize,
    dead: bool,
    rng: Rng,
}

/// Broker-to-reader direction of one connection, with a userspace queue and a quota.
#[derive(Debug)]
pub struct Link {
    state: Mutex<LinkState>,
    quota: usize,
    chunk: Option<usize>,
    pub stats: LinkStats,
    pub unique: String,
    rules: Mutex<Vec<String>>,
}

impl Link {
    fn new(sock: UnixStream, opts: &MockOpts, unique: String) -> Self {
        Self {
            state: Mutex::new(LinkState {
                sock,
                q: Vec::new(),
                head: 0,
                dead: false,
                rng: Rng::new(0xC0FFEE),
            }),
            quota: opts.quota,
            chunk: opts.chunk,
            stats: LinkStats::default(),
            unique,
            rules: Mutex::new(Vec::new()),
        }
    }

    /// Match rules the reader installed with `AddMatch`.
    pub fn rules(&self) -> Vec<String> {
        self.rules.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn flush(&self, st: &mut LinkState) {
        while st.head < st.q.len() && !st.dead {
            let end = match self.chunk {
                Some(max) => (st.head + 1 + st.rng.below(max as u64) as usize).min(st.q.len()),
                None => st.q.len(),
            };
            match sys::send_nowait(&st.sock, &st.q[st.head..end]) {
                Ok(0) => break,
                Ok(n) => {
                    st.head += n;
                    self.stats.written.fetch_add(n as u64, Ordering::Relaxed);
                }
                Err(_) => {
                    self.kill(st);
                    return;
                }
            }
        }
        if st.head == st.q.len() {
            st.q.clear();
            st.head = 0;
        } else if st.head > 4 * 1024 * 1024 {
            st.q.drain(..st.head);
            st.head = 0;
        }
    }

    fn kill(&self, st: &mut LinkState) {
        if !st.dead {
            st.dead = true;
            let _ = st.sock.shutdown(Shutdown::Both);
            self.stats.disconnect_ns.store(crate::clock::now_ns(), Ordering::Relaxed);
            self.stats.disconnected.store(true, Ordering::Release);
        }
    }

    /// Queue bytes for the reader and push as much as the socket accepts. Never blocks. Returns
    /// `false` once the reader has been disconnected.
    pub fn send(&self, data: &[u8]) -> bool {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if st.dead {
            return false;
        }
        st.q.extend_from_slice(data);
        self.flush(&mut st);
        let queued = st.q.len() - st.head;
        self.stats.queued.store(queued as u64, Ordering::Relaxed);
        self.stats.queued_hwm.fetch_max(queued as u64, Ordering::Relaxed);
        if queued > self.quota {
            self.kill(&mut st);
            return false;
        }
        !st.dead
    }

    /// Try to push queued bytes without adding any. Returns bytes still queued.
    pub fn pump(&self) -> usize {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if st.dead {
            return 0;
        }
        self.flush(&mut st);
        let queued = st.q.len() - st.head;
        self.stats.queued.store(queued as u64, Ordering::Relaxed);
        let o = sys::outq(&st.sock) as u64;
        self.stats.outq_hwm.fetch_max(o, Ordering::Relaxed);
        queued
    }

    /// Wait until everything queued was handed to the kernel, or the deadline passes.
    pub fn flush_all(&self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        loop {
            if self.pump() == 0 {
                return true;
            }
            if Instant::now() >= end || self.stats.disconnected.load(Ordering::Acquire) {
                return false;
            }
            std::thread::sleep(Duration::from_micros(200));
        }
    }

    /// Bytes held for the reader: userspace queue plus what the kernel still has.
    pub fn backlog(&self) -> usize {
        let st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        (st.q.len() - st.head) + sys::outq(&st.sock)
    }

    pub fn is_disconnected(&self) -> bool {
        self.stats.disconnected.load(Ordering::Acquire)
    }
}

type LinkList = Arc<(Mutex<Vec<Arc<Link>>>, Condvar)>;

#[derive(Debug)]
pub struct MockBroker {
    path: PathBuf,
    links: LinkList,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    claimed: Mutex<usize>,
}

impl MockBroker {
    /// Listen on `dir/mock-bus`.
    pub fn start(dir: &Path, opts: MockOpts) -> Result<Self> {
        let path = dir.join("mock-bus");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        let links: LinkList = Arc::new((Mutex::new(Vec::new()), Condvar::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (links, stop) = (Arc::clone(&links), Arc::clone(&stop));
            std::thread::Builder::new()
                .name("hx-mock-accept".to_owned())
                .spawn(move || accept_loop(&listener, &opts, &links, &stop))?
        };
        Ok(Self {
            path,
            links,
            stop,
            thread: Some(thread),
            claimed: Mutex::new(0),
        })
    }

    pub fn address(&self) -> String {
        format!("unix:path={}", self.path.display())
    }

    /// The next connection that completed `Hello`, in order of arrival.
    pub fn next_link(&self, timeout: Duration) -> Result<Arc<Link>> {
        let (lock, cv) = &*self.links;
        let end = Instant::now() + timeout;
        let mut claimed = self.claimed.lock().unwrap_or_else(PoisonError::into_inner);
        let mut g = lock.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(l) = g.get(*claimed) {
                *claimed += 1;
                return Ok(Arc::clone(l));
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("no connection reached the mock broker within {timeout:?}");
            }
            g = cv.wait_timeout(g, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }
}

impl Drop for MockBroker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

fn accept_loop(listener: &UnixListener, opts: &MockOpts, links: &LinkList, stop: &AtomicBool) {
    let mut serial = 0u32;
    let mut servers: Vec<JoinHandle<()>> = Vec::new();
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((s, _)) => {
                serial += 1;
                let (opts, links) = (opts.clone(), Arc::clone(links));
                let unique = format!(":1.{}", 1000 + serial);
                if let Ok(h) = std::thread::Builder::new()
                    .name("hx-mock-srv".to_owned())
                    .spawn(move || serve(s, &opts, &links, unique))
                {
                    servers.push(h);
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(2)),
            Err(_) => break,
        }
    }
    // Reader sockets close when their `Link`s drop; servers end on EOF.
    for l in links.0.lock().unwrap_or_else(PoisonError::into_inner).iter() {
        let st = l.state.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = st.sock.shutdown(Shutdown::Both);
    }
    for h in servers {
        let _ = h.join();
    }
}

fn serve(mut s: UnixStream, opts: &MockOpts, links: &LinkList, unique: String) {
    if s.set_nonblocking(false).is_err() {
        return;
    }
    // --- SASL: read byte by byte up to BEGIN so no message byte is consumed ---
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    loop {
        if s.read(&mut b).unwrap_or(0) == 0 {
            return;
        }
        line.push(b[0]);
        if line.ends_with(b"\r\n") {
            let text = String::from_utf8_lossy(&line);
            let t = text.trim_start_matches('\0').trim();
            if t.starts_with("BEGIN") {
                break;
            }
            let reply: &[u8] = if t.starts_with("AUTH") {
                b"OK 00112233445566778899aabbccddeeff\r\n"
            } else if t.starts_with("NEGOTIATE_UNIX_FD") {
                b"AGREE_UNIX_FD\r\n"
            } else {
                b"ERROR\r\n"
            };
            if sys::send_nowait(&s, reply).is_err() {
                return;
            }
            line.clear();
        }
    }
    let Ok(wr) = s.try_clone() else { return };
    if let Some(n) = opts.sndbuf {
        sys::set_send_buffer(&wr, n);
    }
    let link = Arc::new(Link::new(wr, opts, unique.clone()));
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    let mut registered = false;
    loop {
        loop {
            let (reply, used) = match wire::parse(&buf) {
                Parse::Msg(m, used) => {
                    let mut out = None;
                    if m.mtype == METHOD_CALL {
                        out = handle_call(&m, &link, opts);
                        if m.member == Some("Hello") && !registered {
                            registered = true;
                        }
                    }
                    (out, used)
                }
                Parse::NeedMore => break,
                Parse::Bad => {
                    link.stats.reader_closed.store(true, Ordering::Release);
                    return;
                }
            };
            buf.drain(..used);
            if let Some(r) = reply {
                link.send(&r);
            }
            if registered {
                let (lock, cv) = &**links;
                let mut g = lock.lock().unwrap_or_else(PoisonError::into_inner);
                if !g.iter().any(|l| Arc::ptr_eq(l, &link)) {
                    g.push(Arc::clone(&link));
                    cv.notify_all();
                }
            }
        }
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => {
                link.stats.reader_closed.store(true, Ordering::Release);
                return;
            }
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

fn handle_call(m: &wire::Msg<'_>, link: &Link, opts: &MockOpts) -> Option<Vec<u8>> {
    let mut body = Enc::default();
    let sig;
    match m.member {
        Some("Hello") => {
            body.string(&link.unique);
            sig = Some("s");
        }
        Some("AddMatch") => {
            if !opts.reply_addmatch {
                return None;
            }
            if m.body.len() >= 4 {
                let n = u32::from_le_bytes([m.body[0], m.body[1], m.body[2], m.body[3]]) as usize;
                if let Some(r) = m.body.get(4..4 + n) {
                    link.rules
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(String::from_utf8_lossy(r).into_owned());
                }
            }
            sig = None;
        }
        _ => sig = None,
    }
    let mut out = Vec::new();
    wire::encode_message(
        &mut out,
        METHOD_RETURN,
        m.serial.wrapping_add(10_000),
        &Fields {
            reply_serial: Some(m.serial),
            destination: Some(&link.unique),
            sender: Some("org.freedesktop.DBus"),
            signature: sig,
            ..Fields::default()
        },
        &body.buf,
    );
    Some(out)
}
