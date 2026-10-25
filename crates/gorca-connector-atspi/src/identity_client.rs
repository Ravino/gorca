// SPDX-License-Identifier: GPL-3.0-or-later
//! The daemon's side of the identity helper ([`crate::identity_svc`]): one persistent connection
//! with tagged, pipelined requests, and a cache so that a process costs one lookup.
//!
//! * **Scales with the number of applications.** Requests are lines `ID <tag> <pid> <start>`
//!   written to one shared connection; a reader thread routes `OK|NO <tag> ...` replies to the
//!   callers waiting for them. Nothing is refused: pending requests are bounded by memory only,
//!   and the helper answers them on a pool of workers, not in order.
//! * **One lookup per process.** `(pid, start)` is stable for the life of a process, so the
//!   answer (verified or not) is cached; concurrent requests for the same key share one wire
//!   request. [`forget`] drops an entry when the application is gone. A failed lookup (helper
//!   missing, timeout) is never cached.
//! * **Fails closed.** No answer is `None` (unverified), never a weaker in-sandbox guess.
//!
//! Where gorca is not in a private user namespace the client reads `/proc` itself (still cached).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::identity_svc::{
    ENV_SERVER_UNIT, HELPER_UNIT, HelperError, RETRY, parse_reply, peer_unit, socket_path_from,
    unit_from, uses_helper,
};
use crate::proc::verified_identity;

/// Entries after which finished ones are dropped (running applications are far fewer; this only
/// bounds a pathological churn of short-lived processes).
const CACHE_MAX: usize = 8192;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

type Reply = mpsc::Sender<Option<String>>;

enum Slot {
    Done(Option<String>),
    /// A lookup is in flight; these callers wait for its answer.
    Pending(Vec<Reply>),
}

struct Conn {
    writer: Mutex<UnixStream>,
    pending: Mutex<HashMap<u64, Reply>>,
    dead: AtomicBool,
}

/// An identity resolver with its cache.
pub struct Client {
    socket: Option<PathBuf>,
    server_unit: Option<String>,
    local: bool,
    conn: Mutex<Option<Arc<Conn>>>,
    next_tag: AtomicU64,
    cache: Mutex<HashMap<(u32, u64), Slot>>,
}

impl Client {
    /// A client of the helper at `socket`; with `server_unit` the server must be in that unit.
    pub fn new(socket: Option<PathBuf>, server_unit: Option<String>) -> Self {
        Self {
            socket,
            server_unit,
            local: false,
            conn: Mutex::new(None),
            next_tag: AtomicU64::new(1),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// A client that reads `/proc` itself (gorca in the initial user namespace).
    pub fn local() -> Self {
        Self {
            local: true,
            ..Self::new(None, None)
        }
    }

    /// The identity of `(pid, start)` within `timeout`, `None` when unverified or unanswered.
    /// Blocking: call it off the async executor.
    pub fn resolve(&self, pid: u32, start: u64, timeout: Duration) -> Option<String> {
        let key = (pid, start);
        let waiter = {
            let mut cache = lock(&self.cache);
            match cache.get_mut(&key) {
                Some(Slot::Done(v)) => return v.clone(),
                Some(Slot::Pending(waiters)) => {
                    let (tx, rx) = mpsc::channel();
                    waiters.push(tx);
                    Some(rx)
                }
                None => {
                    cache.insert(key, Slot::Pending(Vec::new()));
                    None
                }
            }
        };
        if let Some(rx) = waiter {
            return rx.recv_timeout(timeout).ok().flatten();
        }
        let found = self.lookup(pid, start, timeout);
        let answer = found.as_ref().ok().cloned().flatten();
        let waiters = {
            let mut cache = lock(&self.cache);
            let waiters = match cache.remove(&key) {
                Some(Slot::Pending(w)) => w,
                Some(Slot::Done(_)) | None => Vec::new(),
            };
            if found.is_ok() {
                if cache.len() >= CACHE_MAX {
                    cache.retain(|_, s| matches!(s, Slot::Pending(_)));
                }
                cache.insert(key, Slot::Done(answer.clone()));
            }
            waiters
        };
        for w in waiters {
            let _ = w.send(answer.clone());
        }
        if let Err(e) = found {
            warn_once(&e, self.socket.as_deref());
        }
        answer
    }

    /// Drop the cached answer of an application that is gone (a new process may reuse the pid).
    pub fn forget(&self, pid: u32, start: u64) {
        let mut cache = lock(&self.cache);
        if matches!(cache.get(&(pid, start)), Some(Slot::Done(_))) {
            cache.remove(&(pid, start));
        }
    }

    fn lookup(
        &self,
        pid: u32,
        start: u64,
        timeout: Duration,
    ) -> Result<Option<String>, HelperError> {
        if self.local {
            return Ok(verified_identity(pid, start));
        }
        if self.socket.is_none() {
            return Err(HelperError::Unreachable);
        }
        let end = Instant::now() + timeout;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.attempt(pid, start, left) {
                Err(HelperError::Unreachable | HelperError::Protocol) if left > RETRY => {
                    // A blocking-pool thread, never an async worker.
                    #[allow(clippy::disallowed_methods)]
                    std::thread::sleep(RETRY);
                }
                other => return other,
            }
        }
    }

    fn attempt(&self, pid: u32, start: u64, left: Duration) -> Result<Option<String>, HelperError> {
        let end = Instant::now() + left;
        let conn = self.connection(left)?;
        let tag = self.next_tag.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        lock(&conn.pending).insert(tag, tx);
        if conn.dead.load(Ordering::Acquire) {
            lock(&conn.pending).remove(&tag);
            return Err(HelperError::Protocol);
        }
        let sent = lock(&conn.writer).write_all(format!("ID {tag} {pid} {start}\n").as_bytes());
        if sent.is_err() {
            conn.dead.store(true, Ordering::Release);
            lock(&conn.pending).remove(&tag);
            return Err(HelperError::Protocol);
        }
        if let Ok(v) = rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
            Ok(v)
        } else {
            lock(&conn.pending).remove(&tag);
            Err(HelperError::Protocol)
        }
    }

    /// The live connection, made (with the peer check) when there is none.
    fn connection(&self, left: Duration) -> Result<Arc<Conn>, HelperError> {
        let mut slot = lock(&self.conn);
        if let Some(c) = slot.as_ref()
            && !c.dead.load(Ordering::Acquire)
        {
            return Ok(Arc::clone(c));
        }
        let socket = self.socket.as_deref().ok_or(HelperError::Unreachable)?;
        let stream =
            gorca_dbus::connect_unix_timeout(socket, left).map_err(|_| HelperError::Unreachable)?;
        if let Some(want) = &self.server_unit
            && peer_unit(&stream).as_deref() != Some(want.as_str())
        {
            return Err(HelperError::Untrusted);
        }
        let writer = stream.try_clone().map_err(|_| HelperError::Protocol)?;
        writer
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| HelperError::Protocol)?;
        let conn = Arc::new(Conn {
            writer: Mutex::new(writer),
            pending: Mutex::new(HashMap::new()),
            dead: AtomicBool::new(false),
        });
        let reader_conn = Arc::clone(&conn);
        std::thread::Builder::new()
            .name("identity-reader".into())
            .spawn(move || read_replies(stream, &reader_conn))
            .map_err(|_| HelperError::Protocol)?;
        *slot = Some(Arc::clone(&conn));
        Ok(conn)
    }
}

/// Route replies to their callers until the connection ends, then fail the rest.
fn read_replies(stream: UnixStream, conn: &Conn) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if let Some((tag, identity)) = parse_reply(&line)
            && let Some(tx) = lock(&conn.pending).remove(&tag)
        {
            let _ = tx.send(identity);
        }
    }
    conn.dead.store(true, Ordering::Release);
    // Dropping the senders wakes every waiter with an error; they retry on a new connection.
    lock(&conn.pending).clear();
}

fn warn_once(e: &HelperError, socket: Option<&std::path::Path>) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            error = ?e,
            socket = ?socket,
            "gorca runs in a private user namespace and the identity helper does not answer: \
             application identities stay unverified for now (start gorca-identity.service)"
        );
    }
}

fn global() -> &'static Client {
    static GLOBAL: OnceLock<Client> = OnceLock::new();
    GLOBAL.get_or_init(|| {
        if !uses_helper() {
            return Client::local();
        }
        let env = |k: &str| std::env::var(k).ok();
        Client::new(
            socket_path_from(env),
            unit_from(&env, ENV_SERVER_UNIT, HELPER_UNIT),
        )
    })
}

/// The identity of `(pid, start)` for the running daemon (cached; see [`Client`]).
pub fn resolve(pid: u32, start: u64, timeout: Duration) -> Option<String> {
    global().resolve(pid, start, timeout)
}

/// Forget the cached identity of an application that is gone.
pub fn forget(pid: u32, start: u64) {
    global().forget(pid, start);
}
