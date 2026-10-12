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
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use gorca_model::{Prio, UttId};
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

#[derive(Debug, thiserror::Error)]
pub enum SsipError {
    #[error("no speech-dispatcher socket: neither SPEECHD_ADDRESS nor XDG_RUNTIME_DIR is set")]
    NoSocketPath,
    #[error("unsupported SPEECHD_ADDRESS `{0}` (only unix_socket:path=... is handled)")]
    UnsupportedAddress(String),
    #[error("speech-dispatcher socket: {0}")]
    Io(#[from] io::Error),
    #[error("speech-dispatcher did not answer in time")]
    Timeout,
    #[error("SSIP handshake failed: {0}")]
    Handshake(String),
}

#[derive(Clone, Debug)]
pub struct SsipOptions {
    /// Socket path; `None` means the environment, then `$XDG_RUNTIME_DIR/speech-dispatcher/
    /// speechd.sock`.
    pub path: Option<PathBuf>,
    pub client_name: String,
}

impl Default for SsipOptions {
    fn default() -> Self {
        Self {
            path: None,
            client_name: "gorca:main:main".to_owned(),
        }
    }
}

/// `unix_socket:path=/x/y` (the `SPEECHD_ADDRESS` form) or a plain path.
fn parse_address(v: &str) -> Result<PathBuf, SsipError> {
    if let Some(p) = v.strip_prefix("unix_socket:path=") {
        return Ok(PathBuf::from(p));
    }
    if v.starts_with('/') {
        return Ok(PathBuf::from(v));
    }
    Err(SsipError::UnsupportedAddress(v.to_owned()))
}

impl SsipOptions {
    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Some(path.into()),
            ..Self::default()
        }
    }

    /// Resolve from explicit path, `SPEECHD_ADDRESS`, `SPEECHD_SOCKET`, then the default.
    pub fn resolve(&self) -> Result<PathBuf, SsipError> {
        if let Some(p) = &self.path {
            return Ok(p.clone());
        }
        for var in ["SPEECHD_ADDRESS", "SPEECHD_SOCKET"] {
            if let Ok(v) = std::env::var(var)
                && !v.is_empty()
            {
                return parse_address(&v);
            }
        }
        let dir = std::env::var_os("XDG_RUNTIME_DIR").ok_or(SsipError::NoSocketPath)?;
        Ok(PathBuf::from(dir).join("speech-dispatcher/speechd.sock"))
    }
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
        | SpeechCmd::SetParams(_) => None,
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
                    let _ = p.tracker.on_reply(code, msg_id, &mut evs);
                    false
                }
                Block::Event { ev, msg } => {
                    let _ = p.tracker.on_event(ev, msg, &mut evs);
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

async fn writer(shared: Arc<Shared>, mut wr: OwnedWriteHalf) {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
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
                break;
            }
            shared.writes.fetch_add(1, Ordering::Relaxed);
            if let Err(e) = write_batch(&shared, &mut wr, &buf).await {
                tracing::debug!(error = %e, "SSIP write failed");
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
        let path = opts.resolve()?;
        let stream = with_deadline(deadline, UnixStream::connect(&path))
            .await
            .map_err(|_| SsipError::Timeout)??;
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
            cancel: cancel.clone(),
            writes: AtomicU64::new(0),
            superseded: AtomicU64::new(0),
        });
        let (etx, erx) = mpsc::channel(EVENT_QUEUE);
        let mut tasks = TaskSet::new(cancel);
        tasks.spawn(
            "ssip-reader",
            reader(Arc::clone(&shared), rd.into_inner(), etx),
        );
        tasks.spawn("ssip-writer", writer(Arc::clone(&shared), wr));
        Ok(SsipClient {
            caps: SsipTransportCaps::caps(),
            shared,
            events: erx,
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
    fn drop(&mut self) {
        self.shared.cancel.cancel();
    }
}

fn supersedable(c: &SpeechCmd) -> bool {
    match c {
        SpeechCmd::Speak { .. } | SpeechCmd::Replace { .. } => true,
        SpeechCmd::Stop { .. }
        | SpeechCmd::StopAll
        | SpeechCmd::Pause
        | SpeechCmd::Resume
        | SpeechCmd::SetParams(_) => false,
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
        {
            let mut q = lock(&self.shared.queue);
            if q.len() >= WRITER_QUEUE {
                if supersedable(&cmd) {
                    let before = q.len();
                    q.retain(|c| !supersedable(c));
                    self.shared
                        .superseded
                        .fetch_add((before - q.len()) as u64, Ordering::Relaxed);
                }
                if q.len() >= WRITER_QUEUE {
                    return Err(SubmitError::Full);
                }
            }
            q.push_back(cmd);
        }
        self.shared.wake.notify_one();
        Ok(())
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
            parse_address("unix_socket:path=/run/x.sock").unwrap(),
            PathBuf::from("/run/x.sock")
        );
        assert_eq!(parse_address("/a/b").unwrap(), PathBuf::from("/a/b"));
        assert!(matches!(
            parse_address("inet_socket:host=h,port=6560"),
            Err(SsipError::UnsupportedAddress(_))
        ));
        assert_eq!(
            SsipOptions::with_path("/p").resolve().unwrap(),
            PathBuf::from("/p")
        );
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
        assert!(t.elapsed() < Duration::from_millis(600));
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
        assert!(t.elapsed() < Duration::from_millis(500));
        assert!(c.stats().superseded > 0 || full > 0);
        drop(c);
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }
}
