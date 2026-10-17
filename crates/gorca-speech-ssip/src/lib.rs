//! SSIP `SpeechClient` (speech-dispatcher; voicego in text-SSIP mode).
//!
//! Responsibility (ADR-0001 sections 8.2 and 11): compatibility backend. Wire mapping from the
//! ADR:
//!
//! > "`Speak` = `SET self PRIORITY text` + `SPEAK`; `Stop` = `STOP self`; `Replace` lowered to
//! > `STOP self` + `SPEAK`, pipelined. `utterance_ids` are emulated by mapping SSIP msg-ids from
//! > `225 OK MESSAGE QUEUED` replies. `max_in_flight = 1`. Events via
//! > `SET self NOTIFICATION all on`."
//!
//! [`lower`] is the pure command lowering. [`SsipClient`] is the socket client: one reader task
//! (reply and `7xx` event blocks), one writer task fed by a 4-slot queue (S-3).
//!
//! `SPEAK` needs care that the binary-framed voicego does not: the body must not be sent before
//! the server's `230 OK RECEIVING DATA`, and no other command may be written while a body is
//! outstanding. The writer therefore pipelines `STOP`, `SET ...` and the `SPEAK` header in one
//! write, holds the body until the reader has seen `230`, and holds later commands until the
//! body is out. A server that never sends `230` fails the connection after [`DATA_DEADLINE`].
//!
//! Allowed dependencies: `gorca-model`, `gorca-rt`, `gorca-output-api`. No D-Bus.

mod lower;

use std::collections::VecDeque;
use std::io::{self, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use gorca_model::{Prio, UttId};
use gorca_output_api::peer::{PeerError, check_peer_allowed, own_uid};
use gorca_output_api::queue::admit;
use gorca_output_api::track::{Block, Expect, MsgTracker, classify};
use gorca_output_api::{
    ClientHealth, SpeechCaps, SpeechClient, SpeechCmd, SpeechEvent, SubmitError,
};
use gorca_rt::{CancellationToken, Deadline, Notify, TaskSet, mpsc, with_deadline};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

pub use lower::{
    EVENT_QUEUE, SsipOut, SsipTransportCaps, WRITER_QUEUE, data_block, handshake, lower,
};

/// A write that takes longer than this marks the client `Degraded` (section 8.3).
pub const DEGRADED_AFTER: Duration = Duration::from_millis(50);
/// A write that takes longer than this fails the connection.
pub const WRITE_DEADLINE: Duration = Duration::from_secs(2);
/// The server must answer `SPEAK` with `230` within this time.
pub const DATA_DEADLINE: Duration = Duration::from_secs(2);
const BLOCK_DEADLINE: Duration = Duration::from_secs(5);
const TERMINAL_SEND_DEADLINE: Duration = Duration::from_secs(1);
const MAX_LINE: u64 = 1 << 20;
/// A reply or event block (`NNN-` continuation lines and all) is cut off at this size.
const MAX_BLOCK: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum SsipError {
    #[error("speech-dispatcher socket: {0}")]
    Io(#[from] io::Error),
    #[error("speech-dispatcher did not answer in time")]
    Timeout,
    #[error("SSIP handshake failed: {0}")]
    Handshake(String),
    #[error("refusing the speech-dispatcher socket: {0}")]
    Peer(#[from] PeerError),
}

#[derive(Clone, Debug)]
pub struct SsipOptions {
    /// Socket path. This crate never looks at the environment: the daemon resolves it
    /// ([`ssip_path_from_env_values`]) and passes it in.
    pub path: PathBuf,
    pub client_name: String,
    /// Extra uids whose server gorca accepts (a system-wide service user). Empty: same user only.
    pub allowed_peer_uids: Vec<u32>,
}

impl SsipOptions {
    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            client_name: "gorca:main:main".to_owned(),
            allowed_peer_uids: Vec::new(),
        }
    }
}

/// `unix_socket:path=/x/y` (the `SPEECHD_ADDRESS` form) or a plain absolute path. Anything
/// else (`inet_socket:...`) is not handled.
fn parse_address(v: &str) -> Option<PathBuf> {
    if let Some(p) = v.strip_prefix("unix_socket:path=") {
        return Some(PathBuf::from(p));
    }
    v.starts_with('/').then(|| PathBuf::from(v))
}

/// The speech-dispatcher socket given the values of `SPEECHD_ADDRESS`, `SPEECHD_SOCKET` and
/// `XDG_RUNTIME_DIR` (pure; the caller reads the environment). The first non-empty of the two
/// speechd variables wins; if it is not a unix socket address the result is `None` rather than a
/// silently different server.
pub fn ssip_path_from_env_values(
    speechd_address: Option<&str>,
    speechd_socket: Option<&str>,
    xdg_runtime_dir: Option<&str>,
) -> Option<PathBuf> {
    for v in [speechd_address, speechd_socket].into_iter().flatten() {
        if !v.is_empty() {
            return parse_address(v);
        }
    }
    xdg_runtime_dir
        .filter(|d| !d.is_empty())
        .map(|d| PathBuf::from(d).join("speech-dispatcher/speechd.sock"))
}

fn health_to_u8(h: ClientHealth) -> u8 {
    match h {
        ClientHealth::Connecting => 0,
        ClientHealth::Ready => 1,
        ClientHealth::Degraded => 2,
        ClientHealth::Down => 3,
    }
}

fn health_from_u8(v: u8) -> ClientHealth {
    match v {
        0 => ClientHealth::Connecting,
        1 => ClientHealth::Ready,
        2 => ClientHealth::Degraded,
        _ => ClientHealth::Down,
    }
}

#[derive(Debug, Default)]
struct Proto {
    tracker: MsgTracker,
    /// A `SPEAK` header is out; its body waits for `230`.
    awaiting_230: bool,
    /// The body of that `SPEAK`.
    pending_body: Option<String>,
    /// `230` arrived: the writer may send the body.
    body_ready: Option<String>,
}

#[derive(Debug)]
struct Shared {
    health: AtomicU8,
    queue: Mutex<VecDeque<SpeechCmd>>,
    wake: Notify,
    proto: Mutex<Proto>,
    /// Terminal events of commands evicted before they were written (see `deliver_pending`).
    pending: Mutex<VecDeque<SpeechEvent>>,
    /// `close()` was called: write `QUIT` once the queue is empty.
    quit: AtomicBool,
    /// `CANCEL`/`QUIT` went out (by the writer or by `Drop`).
    goodbye: AtomicBool,
    cancel: CancellationToken,
    writes: AtomicU64,
    superseded: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn set_health(&self, h: ClientHealth) {
        let mut cur = self.health.load(Ordering::Acquire);
        loop {
            if health_from_u8(cur) == ClientHealth::Down {
                return;
            }
            match self.health.compare_exchange(
                cur,
                health_to_u8(h),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(now) => cur = now,
            }
        }
    }

    fn is_down(&self) -> bool {
        health_from_u8(self.health.load(Ordering::Acquire)) == ClientHealth::Down
    }
}

fn speak_id(cmd: &SpeechCmd) -> Option<UttId> {
    match cmd {
        SpeechCmd::Speak { id, .. } => Some(*id),
        SpeechCmd::Replace { then, .. } => speak_id(then),
        SpeechCmd::Stop { .. }
        | SpeechCmd::StopAll
        | SpeechCmd::Pause
        | SpeechCmd::Resume
        | SpeechCmd::SetParams(_)
        | SpeechCmd::Configure(_) => None,
    }
}

/// Encode one command's lines into `out`; the body (if any) is parked in `proto`.
fn encode(cmd: &SpeechCmd, proto: &mut Proto, out: &mut Vec<u8>) {
    if let SpeechCmd::Replace { stop, .. } = cmd {
        proto.tracker.mark_replaced(*stop);
    }
    let id = speak_id(cmd);
    for item in lower(cmd, Prio::Text) {
        match item {
            SsipOut::Line(l) => {
                out.extend_from_slice(l.as_bytes());
                match (l.as_str(), id) {
                    ("SPEAK\r\n", Some(id)) => {
                        proto.tracker.expect(Expect::Speak(id));
                        proto.awaiting_230 = true;
                    }
                    _ => proto.tracker.expect(Expect::Simple),
                }
            }
            SsipOut::Data(d) => proto.pending_body = Some(d),
        }
    }
}

async fn forward(tx: &mpsc::Sender<SpeechEvent>, ev: SpeechEvent) {
    let terminal = matches!(
        ev,
        SpeechEvent::Ended(_) | SpeechEvent::Stopped(_) | SpeechEvent::Error(..)
    );
    if terminal {
        let _ = tokio::time::timeout(TERMINAL_SEND_DEADLINE, tx.send(ev)).await;
    } else {
        let _ = tx.try_send(ev);
    }
}

/// Read one reply or event block: lines up to and including the one whose 4th byte is a space.
async fn read_block(
    rd: &mut BufReader<OwnedReadHalf>,
    block: &mut Vec<u8>,
    idle: &CancellationToken,
) -> Result<bool, io::Error> {
    block.clear();
    let mut first = true;
    loop {
        let start = block.len();
        let mut limited = (&mut *rd).take(MAX_LINE);
        let n = if first {
            tokio::select! {
                biased;
                () = idle.cancelled() => return Ok(false),
                r = limited.read_until(b'\n', block) => r?,
            }
        } else {
            match tokio::time::timeout(BLOCK_DEADLINE, limited.read_until(b'\n', block)).await {
                Ok(r) => r?,
                Err(_) => return Err(io::Error::new(io::ErrorKind::TimedOut, "block stalled")),
            }
        };
        first = false;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "eof"));
        }
        if block.last() != Some(&b'\n') {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
        }
        if block.len() > MAX_BLOCK {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "block too large",
            ));
        }
        let line = block.get(start..).unwrap_or(&[]);
        let final_line = line.get(3) == Some(&b' ') || line.len() < 4;
        if final_line {
            return Ok(true);
        }
    }
}

async fn reader(shared: Arc<Shared>, rd: OwnedReadHalf, tx: mpsc::Sender<SpeechEvent>) {
    let mut rd = BufReader::new(rd);
    let mut block = Vec::with_capacity(256);
    let mut evs: Vec<SpeechEvent> = Vec::with_capacity(4);
    loop {
        match read_block(&mut rd, &mut block, &shared.cancel).await {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                tracing::debug!(error = %e, "SSIP connection ended");
                break;
            }
        }
        evs.clear();
        let got_230 = {
            let mut p = lock(&shared.proto);
            match classify(&block) {
                Block::Reply { code: 230, .. } => {
                    p.awaiting_230 = false;
                    p.body_ready = p.pending_body.take();
                    true
                }
                Block::Reply { code, msg_id } => {
                    p.tracker.on_reply(code, msg_id, &mut evs);
                    false
                }
                Block::Event { ev, msg } => {
                    p.tracker.on_event(ev, msg, &mut evs);
                    false
                }
                Block::Garbage => false,
            }
        };
        if got_230 {
            shared.wake.notify_one();
        }
        for ev in evs.drain(..) {
            forward(&tx, ev).await;
        }
    }
    shared.set_health(ClientHealth::Down);
    shared.cancel.cancel();
    let mut last = Vec::new();
    lock(&shared.proto).tracker.fail_all(&mut last);
    for ev in last {
        forward(&tx, ev).await;
    }
}

async fn write_batch(
    shared: &Shared,
    wr: &mut OwnedWriteHalf,
    buf: &[u8],
) -> Result<(), io::Error> {
    let fut = wr.write_all(buf);
    tokio::pin!(fut);
    if let Ok(r) = tokio::time::timeout(DEGRADED_AFTER, &mut fut).await {
        return r;
    }
    shared.set_health(ClientHealth::Degraded);
    let rest = WRITE_DEADLINE.saturating_sub(DEGRADED_AFTER);
    match tokio::time::timeout(rest, fut).await {
        Ok(r) => {
            if r.is_ok() {
                shared.set_health(ClientHealth::Ready);
            }
            r
        }
        Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "write stalled")),
    }
}

async fn deliver_pending(shared: &Shared, tx: &mpsc::Sender<SpeechEvent>) {
    loop {
        let ev = lock(&shared.pending).pop_front();
        let Some(ev) = ev else { return };
        forward(tx, ev).await;
    }
}

async fn writer(shared: Arc<Shared>, mut wr: OwnedWriteHalf, tx: mpsc::Sender<SpeechEvent>) {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut quit_sent = false;
    loop {
        let awaiting = lock(&shared.proto).awaiting_230;
        let wait = async {
            tokio::select! {
                biased;
                () = shared.cancel.cancelled() => false,
                () = shared.wake.notified() => true,
            }
        };
        if awaiting {
            match tokio::time::timeout(DATA_DEADLINE, wait).await {
                Ok(true) => {}
                Ok(false) => return,
                Err(_) => {
                    tracing::debug!("SSIP server never answered SPEAK with 230");
                    shared.set_health(ClientHealth::Down);
                    shared.cancel.cancel();
                    return;
                }
            }
        } else if !wait.await {
            return;
        }
        deliver_pending(&shared, &tx).await;
        loop {
            buf.clear();
            {
                let mut q = lock(&shared.queue);
                let mut p = lock(&shared.proto);
                if let Some(body) = p.body_ready.take() {
                    buf.extend_from_slice(body.as_bytes());
                }
                // No new command while a body is outstanding.
                while !p.awaiting_230 && p.pending_body.is_none() {
                    let Some(cmd) = q.pop_front() else { break };
                    encode(&cmd, &mut p, &mut buf);
                }
            }
            if buf.is_empty() {
                let idle = {
                    let p = lock(&shared.proto);
                    !p.awaiting_230 && p.pending_body.is_none()
                };
                if idle && !quit_sent && shared.quit.load(Ordering::Acquire) {
                    // Everything queued before `close()` is out: say goodbye.
                    quit_sent = true;
                    shared.goodbye.store(true, Ordering::Release);
                    buf.extend_from_slice(b"QUIT\r\n");
                } else {
                    break;
                }
            }
            shared.writes.fetch_add(1, Ordering::Relaxed);
            if let Err(e) = write_batch(&shared, &mut wr, &buf).await {
                tracing::debug!(error = %e, "SSIP write failed");
                shared.set_health(ClientHealth::Down);
                shared.cancel.cancel();
                return;
            }
            if quit_sent {
                shared.set_health(ClientHealth::Down);
                shared.cancel.cancel();
                return;
            }
        }
    }
}

/// Counters for tests and telemetry.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct SsipStats {
    pub writes: u64,
    pub superseded: u64,
}

#[derive(Debug)]
pub struct SsipClient {
    caps: SpeechCaps,
    shared: Arc<Shared>,
    events: mpsc::Receiver<SpeechEvent>,
    /// A duplicate of the socket, so `Drop` can still say goodbye after the tasks are gone.
    closer: Option<OwnedFd>,
    _tasks: TaskSet,
}

async fn read_reply(
    rd: &mut BufReader<OwnedReadHalf>,
    block: &mut Vec<u8>,
) -> Result<(), SsipError> {
    let never = CancellationToken::new();
    match read_block(rd, block, &never).await {
        Ok(_) => {}
        Err(e) => return Err(SsipError::Io(e)),
    }
    if block.first() == Some(&b'2') {
        Ok(())
    } else {
        Err(SsipError::Handshake(
            String::from_utf8_lossy(block).trim().to_owned(),
        ))
    }
}

impl SsipClient {
    /// Connect and run the handshake (`CLIENT_NAME`, `NOTIFICATION all on`), under `deadline`.
    pub async fn connect(opts: &SsipOptions, deadline: Deadline) -> Result<SsipClient, SsipError> {
        Self::connect_as(opts, deadline, own_uid()).await
    }

    /// [`connect`](Self::connect) with the uid the peer must have (tests pass a foreign one).
    pub async fn connect_as(
        opts: &SsipOptions,
        deadline: Deadline,
        own: Option<u32>,
    ) -> Result<SsipClient, SsipError> {
        gorca_rt::guard::check(&opts.path.to_string_lossy());
        let stream = with_deadline(deadline, UnixStream::connect(&opts.path))
            .await
            .map_err(|_| SsipError::Timeout)??;
        // Everything gorca reads is sent here: only talk to our own user's server.
        check_peer_allowed(stream.peer_cred()?.uid(), own, &opts.allowed_peer_uids)?;
        let closer = stream.as_fd().try_clone_to_owned().ok();
        let (rd, mut wr) = stream.into_split();
        let mut rd = BufReader::new(rd);
        let mut hello = Vec::new();
        let lines = handshake(&opts.client_name);
        for l in &lines {
            match l {
                SsipOut::Line(s) | SsipOut::Data(s) => hello.extend_from_slice(s.as_bytes()),
            }
        }
        let mut block = Vec::with_capacity(64);
        let shake = async {
            wr.write_all(&hello).await?;
            for _ in 0..lines.len() {
                read_reply(&mut rd, &mut block).await.map_err(|e| match e {
                    SsipError::Io(e) => e,
                    other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
                })?;
            }
            Ok::<(), io::Error>(())
        };
        match with_deadline(deadline, shake).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) if e.kind() == io::ErrorKind::InvalidData => {
                return Err(SsipError::Handshake(e.to_string()));
            }
            Ok(Err(e)) => return Err(SsipError::Io(e)),
            Err(_) => return Err(SsipError::Timeout),
        }
        let cancel = CancellationToken::new();
        let shared = Arc::new(Shared {
            health: AtomicU8::new(health_to_u8(ClientHealth::Ready)),
            queue: Mutex::new(VecDeque::new()),
            wake: Notify::new(),
            proto: Mutex::new(Proto::default()),
            pending: Mutex::new(VecDeque::new()),
            quit: AtomicBool::new(false),
            goodbye: AtomicBool::new(false),
            cancel: cancel.clone(),
            writes: AtomicU64::new(0),
            superseded: AtomicU64::new(0),
        });
        let (etx, erx) = mpsc::channel(EVENT_QUEUE);
        let mut tasks = TaskSet::new(cancel);
        tasks.spawn(
            "ssip-reader",
            reader(Arc::clone(&shared), rd.into_inner(), etx.clone()),
        );
        tasks.spawn("ssip-writer", writer(Arc::clone(&shared), wr, etx));
        Ok(SsipClient {
            caps: SsipTransportCaps::caps(),
            shared,
            events: erx,
            closer,
            _tasks: tasks,
        })
    }

    pub fn stats(&self) -> SsipStats {
        SsipStats {
            writes: self.shared.writes.load(Ordering::Relaxed),
            superseded: self.shared.superseded.load(Ordering::Relaxed),
        }
    }
}

impl Drop for SsipClient {
    /// speech-dispatcher keeps playing what a vanished client queued, so a client that is
    /// dropped (failback, shutdown) cancels its speech and quits first, best effort. Not when a
    /// `SPEAK` body is half written: the connection is closed right after anyway.
    fn drop(&mut self) {
        if !self.shared.is_down() && !self.shared.goodbye.swap(true, Ordering::AcqRel) {
            let idle = {
                let p = lock(&self.shared.proto);
                !p.awaiting_230 && p.pending_body.is_none()
            };
            if idle && let Some(fd) = self.closer.take() {
                let mut s = std::os::unix::net::UnixStream::from(fd);
                let _ = s.set_nonblocking(true);
                let _ = s.write(b"CANCEL self\r\nQUIT\r\n");
            }
        }
        self.shared.cancel.cancel();
    }
}

impl SpeechClient for SsipClient {
    fn caps(&self) -> &SpeechCaps {
        &self.caps
    }

    fn submit(&self, cmd: SpeechCmd) -> Result<(), SubmitError> {
        if self.shared.is_down() {
            return Err(SubmitError::Disconnected);
        }
        let gone = {
            let mut q = lock(&self.shared.queue);
            // S-3: a stop always gets through; speaks are evicted first (review F10).
            admit(&mut q, cmd, WRITER_QUEUE, |c| c, |c| c)?
        };
        if !gone.is_empty() {
            self.shared
                .superseded
                .fetch_add(gone.len() as u64, Ordering::Relaxed);
            lock(&self.shared.pending).extend(gone.into_iter().map(SpeechEvent::Stopped));
        }
        self.shared.wake.notify_one();
        Ok(())
    }

    fn close(&self) {
        self.shared.quit.store(true, Ordering::Release);
        self.shared.wake.notify_one();
    }

    fn events(&mut self) -> &mut mpsc::Receiver<SpeechEvent> {
        &mut self.events
    }

    fn health(&self) -> ClientHealth {
        health_from_u8(self.shared.health.load(Ordering::Acquire))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_output_api::{Verbalized, VoiceRef};
    use gorca_testkit::MockSpeechServer;
    use std::time::Instant;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gs-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn dl(ms: u64) -> Deadline {
        Deadline(Instant::now() + Duration::from_millis(ms))
    }

    fn speak(id: u64, text: &str) -> SpeechCmd {
        SpeechCmd::Speak {
            id: UttId(id),
            text: Verbalized {
                text: Arc::from(text),
                lang: None,
            },
            voice: VoiceRef::Default,
            marks: false,
        }
    }

    async fn connect(srv: &MockSpeechServer) -> SsipClient {
        SsipClient::connect(&SsipOptions::with_path(srv.socket_path()), dl(2000))
            .await
            .unwrap()
    }

    async fn until(c: &mut SsipClient, want: SpeechEvent) {
        for _ in 0..20 {
            let ev = tokio::time::timeout(Duration::from_secs(2), c.events().recv())
                .await
                .unwrap()
                .unwrap();
            if ev == want {
                return;
            }
        }
        panic!("never saw {want:?}");
    }

    #[test]
    fn addresses() {
        assert_eq!(
            parse_address("unix_socket:path=/run/x.sock"),
            Some(PathBuf::from("/run/x.sock"))
        );
        assert_eq!(parse_address("/a/b"), Some(PathBuf::from("/a/b")));
        assert_eq!(parse_address("inet_socket:host=h,port=6560"), None);
    }

    #[test]
    fn the_path_comes_from_values_in_priority_order() {
        let p = |a, s, x| ssip_path_from_env_values(a, s, x);
        assert_eq!(
            p(
                Some("unix_socket:path=/a.sock"),
                Some("/b.sock"),
                Some("/run/u")
            ),
            Some(PathBuf::from("/a.sock"))
        );
        assert_eq!(
            p(None, Some("/b.sock"), Some("/run/u")),
            Some(PathBuf::from("/b.sock"))
        );
        assert_eq!(
            p(Some(""), None, Some("/run/u")),
            Some(PathBuf::from("/run/u/speech-dispatcher/speechd.sock"))
        );
        assert_eq!(p(None, None, None), None);
        assert_eq!(p(None, None, Some("")), None);
        // A TCP address is not silently replaced by the default socket.
        assert_eq!(
            p(Some("inet_socket:host=h,port=1"), None, Some("/run/u")),
            None
        );
    }

    #[tokio::test]
    async fn a_server_of_another_user_is_refused() {
        let d = tmp("uid");
        let srv = MockSpeechServer::start_ssip(&d).await.unwrap();
        let opts = SsipOptions::with_path(srv.socket_path());
        let ours = own_uid().unwrap();
        let r = SsipClient::connect_as(&opts, dl(2000), Some(ours.wrapping_add(1))).await;
        assert!(matches!(r, Err(SsipError::Peer(PeerError::Other { .. }))));
        assert!(
            SsipClient::connect_as(&opts, dl(2000), Some(ours))
                .await
                .is_ok()
        );
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// An explicit allow-list admits a server of another uid (a system-wide service user); the
    /// default, an empty list, keeps the same-user rule.
    #[tokio::test]
    async fn the_peer_uid_allow_list_admits_a_listed_foreign_server() {
        let d = tmp("uidlist");
        let srv = MockSpeechServer::start_ssip(&d).await.unwrap();
        let mut opts = SsipOptions::with_path(srv.socket_path());
        let ours = own_uid().unwrap();
        // Pretend we are another user: the mock's uid (ours) is foreign to `own`.
        let other = ours.wrapping_add(1);
        assert!(
            SsipClient::connect_as(&opts, dl(2000), Some(other))
                .await
                .is_err()
        );
        opts.allowed_peer_uids = vec![ours];
        assert!(
            SsipClient::connect_as(&opts, dl(2000), Some(other))
                .await
                .is_ok()
        );
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Review R2-7: the connect goes through the isolation tripwire, so a test that targets the
    /// ambient socket dies loudly instead of speaking through the user's server.
    #[cfg(debug_assertions)]
    #[tokio::test]
    #[should_panic(expected = "test isolation tripwire")]
    async fn connecting_to_a_forbidden_socket_trips_the_wire() {
        gorca_rt::guard::forbid("/run/user/424244/speech-tripwire-gorca-speech-ssip.sock");
        let opts =
            SsipOptions::with_path("/run/user/424244/speech-tripwire-gorca-speech-ssip.sock");
        let _ = SsipClient::connect(&opts, dl(500)).await;
    }

    /// F13: a reply block of continuation lines is cut off at 64 KiB in total.
    #[tokio::test]
    async fn an_endless_continuation_block_fails_the_connection() {
        let d = tmp("blk");
        let p = d.join("blk.sock");
        let l = tokio::net::UnixListener::bind(&p).unwrap();
        let server = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = [0u8; 512];
            // Answer the two handshake lines normally.
            let mut got = 0;
            while got < 2 {
                let n = s.read(&mut buf).await.unwrap();
                got += buf[..n].split(|b| *b == b'\n').count() - 1;
            }
            s.write_all(b"208 OK\r\n220 OK\r\n").await.unwrap();
            // Then a block that never ends.
            let line = format!("249-{}\r\n", "v".repeat(900));
            for _ in 0..200 {
                if s.write_all(line.as_bytes()).await.is_err() {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let c = SsipClient::connect(&SsipOptions::with_path(&p), dl(2000))
            .await
            .unwrap();
        let end = Instant::now() + Duration::from_secs(3);
        while c.health() != ClientHealth::Down {
            assert!(Instant::now() < end, "the oversized block was accepted");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        drop(c);
        server.abort();
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Regression (review F10): a stop gets through a full queue and evicted speaks are
    /// reported `Stopped`.
    #[tokio::test]
    async fn a_stop_gets_through_a_full_queue() {
        let d = tmp("stopq");
        let srv = MockSpeechServer::start_ssip(&d).await.unwrap();
        srv.set_speak_time(Duration::from_secs(10));
        let mut c = connect(&srv).await;
        c.submit(speak(1, "playing")).unwrap();
        until(&mut c, SpeechEvent::Started(UttId(1))).await;
        srv.set_never_read(true);
        let big = "x".repeat(512 * 1024);
        for i in 11..60 {
            let _ = c.submit(speak(i, &big));
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        c.submit(SpeechCmd::Stop { id: UttId(1) })
            .expect("a stop is never refused");
        c.submit(SpeechCmd::StopAll)
            .expect("a stop is never refused");
        assert!(c.stats().superseded >= 1);
        srv.set_never_read(false);
        assert!(
            srv.wait_verbs(Duration::from_secs(5), |v| v
                .iter()
                .any(|l| l.starts_with("STOP") || l.starts_with("CANCEL")))
                .await
        );
        let mut stopped = 0;
        // Wait for the Stopped report with a generous deadline (a loaded host may deliver it
        // late); stop as soon as it is in.
        while stopped < 1
            && let Ok(Some(ev)) =
                tokio::time::timeout(Duration::from_secs(10), c.events().recv()).await
        {
            if matches!(ev, SpeechEvent::Stopped(u) if u.0 > 10) {
                stopped += 1;
            }
        }
        assert!(stopped >= 1, "evicted speaks are reported Stopped");
        drop(c);
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// F3: dropping a client cancels its speech and quits, so speech-dispatcher stops playing.
    #[tokio::test]
    async fn dropping_a_client_cancels_and_quits() {
        let d = tmp("drop");
        let srv = MockSpeechServer::start_ssip(&d).await.unwrap();
        srv.set_speak_time(Duration::from_secs(10));
        let mut c = connect(&srv).await;
        c.submit(speak(1, "playing")).unwrap();
        until(&mut c, SpeechEvent::Started(UttId(1))).await;
        drop(c);
        assert!(
            srv.wait_verbs(Duration::from_secs(3), |v| v.iter().any(|l| l == "QUIT"))
                .await,
            "{:?}",
            srv.verbs()
        );
        let v = srv.verbs();
        let cancel = v.iter().rposition(|l| l.starts_with("CANCEL")).unwrap();
        let quit = v.iter().position(|l| l == "QUIT").unwrap();
        assert!(cancel < quit, "{v:?}");
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// F3: `close()` writes QUIT after everything queued before it.
    #[tokio::test]
    async fn close_sends_cancel_then_quit_and_ends_the_connection() {
        let d = tmp("quit");
        let srv = MockSpeechServer::start_ssip(&d).await.unwrap();
        srv.set_speak_time(Duration::from_secs(10));
        let mut c = connect(&srv).await;
        c.submit(speak(1, "playing")).unwrap();
        until(&mut c, SpeechEvent::Started(UttId(1))).await;
        c.submit(SpeechCmd::StopAll).unwrap();
        c.close();
        assert!(
            srv.wait_verbs(Duration::from_secs(3), |v| v.iter().any(|l| l == "QUIT"))
                .await
        );
        let v = srv.verbs();
        let cancel = v.iter().position(|l| l.starts_with("CANCEL")).unwrap();
        let quit = v.iter().position(|l| l == "QUIT").unwrap();
        assert!(cancel < quit, "{v:?}");
        let end = Instant::now() + Duration::from_secs(3);
        while c.health() != ClientHealth::Down {
            assert!(Instant::now() < end);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        drop(c);
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn speak_waits_for_230_and_maps_events_to_the_utterance() {
        let d = tmp("a");
        let srv = MockSpeechServer::start_ssip(&d).await.unwrap();
        srv.set_speak_time(Duration::from_millis(5));
        let mut c = connect(&srv).await;
        c.submit(speak(4, "hello")).unwrap();
        until(&mut c, SpeechEvent::Accepted(UttId(4))).await;
        until(&mut c, SpeechEvent::Started(UttId(4))).await;
        until(&mut c, SpeechEvent::Ended(UttId(4))).await;
        assert_eq!(srv.spoken(), ["hello"]);
        let v = srv.verbs();
        assert!(v.iter().any(|l| l.contains("NOTIFICATION all on")));
        assert!(v.contains(&"SPEAK".to_owned()));
        drop(c);
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn configure_sends_the_language_module_and_voice_to_the_server() {
        let d = tmp("cfg");
        let srv = MockSpeechServer::start_ssip(&d).await.unwrap();
        let c = connect(&srv).await;
        c.submit(SpeechCmd::Configure(gorca_output_api::VoiceSetup {
            language: Arc::from("ru"),
            output_module: Arc::from("espeak-ng"),
            voice: Arc::from(""),
        }))
        .unwrap();
        assert!(
            srv.wait_verbs(Duration::from_secs(2), |v| v
                .iter()
                .any(|l| l == "SET self LANGUAGE ru"))
                .await
        );
        let v = srv.verbs();
        let m = v
            .iter()
            .position(|l| l == "SET self OUTPUT_MODULE espeak-ng");
        let l = v.iter().position(|l| l == "SET self LANGUAGE ru");
        assert!(m.is_some() && m < l, "{v:?}");
        assert!(!v.iter().any(|l| l.contains("SYNTHESIS_VOICE")));
        drop(c);
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn replace_is_stop_then_speak_and_the_old_cancel_is_swallowed() {
        let d = tmp("b");
        let srv = MockSpeechServer::start_ssip(&d).await.unwrap();
        srv.set_speak_time(Duration::from_secs(10));
        let mut c = connect(&srv).await;
        c.submit(speak(1, "one")).unwrap();
        until(&mut c, SpeechEvent::Started(UttId(1))).await;
        c.submit(SpeechCmd::Replace {
            stop: UttId(1),
            then: Box::new(speak(2, "two")),
        })
        .unwrap();
        until(&mut c, SpeechEvent::Started(UttId(2))).await;
        assert_eq!(srv.spoken(), ["one", "two"]);
        let v = srv.verbs();
        let stop = v.iter().position(|l| l == "STOP self").unwrap();
        let sp = v.iter().rposition(|l| l == "SPEAK").unwrap();
        assert!(stop < sp);
        drop(c);
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn two_speaks_back_to_back_never_interleave_a_body_with_a_command() {
        let d = tmp("c");
        let srv = MockSpeechServer::start_ssip(&d).await.unwrap();
        srv.set_speak_time(Duration::from_millis(1));
        srv.set_reply_delay(Duration::from_millis(5));
        let c = connect(&srv).await;
        for i in 0..3 {
            c.submit(speak(i, &format!("msg {i}"))).unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert!(srv.wait_spoken(3, Duration::from_secs(3)).await);
        assert_eq!(srv.spoken(), ["msg 0", "msg 1", "msg 2"]);
        drop(c);
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn server_death_goes_down() {
        let d = tmp("d");
        let mut srv = MockSpeechServer::start_ssip(&d).await.unwrap();
        srv.set_speak_time(Duration::from_secs(10));
        let mut c = connect(&srv).await;
        c.submit(speak(1, "long")).unwrap();
        until(&mut c, SpeechEvent::Started(UttId(1))).await;
        srv.stop().await;
        until(&mut c, SpeechEvent::Stopped(UttId(1))).await;
        assert_eq!(c.health(), ClientHealth::Down);
        assert_eq!(c.submit(SpeechCmd::StopAll), Err(SubmitError::Disconnected));
        drop(c);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn connect_errors_are_bounded() {
        let d = tmp("e");
        let r = SsipClient::connect(&SsipOptions::with_path(d.join("none")), dl(300)).await;
        assert!(matches!(r, Err(SsipError::Io(_))));
        let p = d.join("silent.sock");
        let l = tokio::net::UnixListener::bind(&p).unwrap();
        let _hold = tokio::spawn(async move {
            let _s = l.accept().await;
            std::future::pending::<()>().await;
        });
        let t = Instant::now();
        let r = SsipClient::connect(&SsipOptions::with_path(&p), dl(150)).await;
        assert!(matches!(r, Err(SsipError::Timeout)));
        // The deadline is 150 ms; the bound only has to tell "enforced" from "never returns".
        assert!(t.elapsed() < Duration::from_secs(10));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn the_writer_queue_is_bounded_and_submit_never_blocks() {
        let d = tmp("f");
        let srv = MockSpeechServer::start_ssip(&d).await.unwrap();
        let c = connect(&srv).await;
        srv.set_never_read(true);
        let big = "y".repeat(256 * 1024);
        let t = Instant::now();
        let mut full = 0;
        for i in 0..40 {
            if c.submit(speak(i, &big)) == Err(SubmitError::Full) {
                full += 1;
            }
        }
        // The server never reads: a blocking submit would hang for good, so any finite bound
        // detects it; the bound is generous because the host may be loaded.
        assert!(t.elapsed() < Duration::from_secs(10));
        assert!(c.stats().superseded > 0 || full > 0);
        drop(c);
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }
}
