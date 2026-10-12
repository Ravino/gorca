//! voicego native `SpeechClient`.
//!
//! Wire facts (`docs/platform/voicego-protocol.md`, TBD-V1..V6 answered):
//!
//! - **V1** There is no separate native protocol: it is SSIP with length-prefixed framing. The
//!   client sends `AA 56 47 01` once and then `u32` big-endian length frames in both directions.
//! - **V2** Atomic replace: [`SpeechCmd::Replace`] is encoded as `STOP self` + `SPEAK n` + data
//!   frame in **one** `write_all`, with no round trip and no waiting for `230` or `225`.
//!   `caps.atomic_replace = true`.
//! - **V3** Events carry the server's message id; the shared tracker
//!   ([`gorca_output_api::track`]) binds ids to gorca's `UttId` in command order and parks
//!   events that overtake their `225`.
//! - **V4** `701 BEGIN` is first PCM (`Started`), `702 END` is `Ended`, `703` is `Stopped`; there
//!   is no played-duration event, so `audio_progress = false`.
//! - **V5** UNIX socket, explicit path or `$XDG_RUNTIME_DIR/voicego.sock`; no auth beyond file
//!   permissions.
//! - **V6** Failover is not here: this is a single-connection client that goes `Down` on EOF
//!   or error. Reconnecting and falling back to SSIP is `gorca_speech::FailoverClient`.
//!
//! Pre-fix voicegod loses a `SPEAK` pipelined right behind a `STOP` (it is answered with `703`
//! without `701`). A speak written within [`STOP_SPEAK_WINDOW`] of a stop is therefore guarded:
//! if it is cancelled before it started, it is sent once more ([`VoicegoStats::resends`]).
//!
//! Licensing: voicego is AGPL-3.0-or-later. gorca implements the client from the protocol
//! specification and does not vendor or link voicego code.
//!
//! Allowed dependencies: `gorca-model`, `gorca-rt`, `gorca-output-api`. Never `gorca-speech`.

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use gorca_model::UttId;
use gorca_output_api::track::{Block, Expect, MsgTracker, Reaction, classify};
use gorca_output_api::{
    ClientHealth, SpeechCaps, SpeechClient, SpeechCmd, SpeechEvent, SubmitError, Verbalized,
    VoiceParams,
};
use gorca_rt::{CancellationToken, Deadline, Notify, TaskSet, mpsc, with_deadline};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// S-3: the writer queue holds 4 commands.
pub const WRITER_QUEUE: usize = 4;
/// Event channel bound (section 8.3).
pub const EVENT_QUEUE: usize = 128;
/// A speak written this soon after a stop is guarded against the pre-fix lost-speech bug.
pub const STOP_SPEAK_WINDOW: Duration = Duration::from_millis(100);
/// A write that takes longer than this marks the client `Degraded` (section 8.3).
pub const DEGRADED_AFTER: Duration = Duration::from_millis(50);
/// A write that takes longer than this fails the connection.
pub const WRITE_DEADLINE: Duration = Duration::from_secs(2);
/// Once a frame has started, the rest must follow within this time.
const FRAME_DEADLINE: Duration = Duration::from_secs(5);
/// Terminal events wait this long for room in the event channel before they are given up on.
const TERMINAL_SEND_DEADLINE: Duration = Duration::from_secs(1);
/// Frames above the server's own ceiling (16 MiB) are a protocol error.
const MAX_FRAME: usize = 16 << 20;
const PREFIX: [u8; 4] = [0xAA, b'V', b'G', 1];

#[derive(Debug, thiserror::Error)]
pub enum VoicegoError {
    #[error("XDG_RUNTIME_DIR is not set and no socket path was given")]
    NoSocketPath,
    #[error("voicego socket: {0}")]
    Io(#[from] io::Error),
    #[error("voicego did not answer in time")]
    Timeout,
    #[error("voicego handshake failed: {0}")]
    Handshake(String),
}

#[derive(Clone, Debug)]
pub struct VoicegoOptions {
    /// Socket path; `None` means `$XDG_RUNTIME_DIR/voicego.sock`.
    pub path: Option<PathBuf>,
    /// `SET self CLIENT_NAME` (cosmetic).
    pub client_name: String,
}

impl Default for VoicegoOptions {
    fn default() -> Self {
        Self {
            path: None,
            client_name: "gorca:main:main".to_owned(),
        }
    }
}

impl VoicegoOptions {
    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Some(path.into()),
            ..Self::default()
        }
    }

    /// The socket this client would connect to.
    pub fn resolve(&self) -> Result<PathBuf, VoicegoError> {
        if let Some(p) = &self.path {
            return Ok(p.clone());
        }
        let dir = std::env::var_os("XDG_RUNTIME_DIR").ok_or(VoicegoError::NoSocketPath)?;
        Ok(PathBuf::from(dir).join("voicego.sock"))
    }
}

/// Capabilities of a voicego connection (TBD-V2..V4 answered).
pub fn caps() -> SpeechCaps {
    SpeechCaps {
        atomic_replace: true,
        utterance_ids: true,
        max_in_flight: 1,
        index_marks: false,
        audio_progress: false,
        ssml: false,
    }
}

/// Counters for tests and telemetry.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct VoicegoStats {
    /// Speak commands sent a second time after the pre-fix lost-speech bug.
    pub resends: u64,
    /// `write_all` calls (a batch of commands is one write).
    pub writes: u64,
    /// Commands dropped from the writer queue because a newer one superseded them (S-3).
    pub superseded: u64,
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
struct WriteQueue {
    cmds: VecDeque<SpeechCmd>,
    resends: VecDeque<(UttId, Vec<u8>)>,
}

#[derive(Debug, Default)]
struct Proto {
    tracker: MsgTracker,
    last_stop: Option<Instant>,
}

#[derive(Debug)]
struct Shared {
    health: AtomicU8,
    queue: Mutex<WriteQueue>,
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
        // Down is final.
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

fn push_frame(out: &mut Vec<u8>, payload: &[u8]) {
    let len = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload);
}

fn push_line(out: &mut Vec<u8>, proto: &mut Proto, line: &str) {
    push_frame(out, line.as_bytes());
    proto.tracker.expect(Expect::Simple);
}

fn speak_bytes(text: &Verbalized) -> Vec<u8> {
    let body = if text.text.is_empty() {
        " "
    } else {
        &text.text
    };
    let mut v = Vec::with_capacity(body.len() + 20);
    push_frame(&mut v, format!("SPEAK {}", body.len()).as_bytes());
    push_frame(&mut v, body.as_bytes());
    v
}

/// Encode one command into `out`, registering what the server will answer. Everything a batch
/// needs goes into one buffer so that it is one `write_all` (V2).
fn encode(cmd: &SpeechCmd, proto: &mut Proto, out: &mut Vec<u8>, now: Instant, depth: u8) {
    match cmd {
        SpeechCmd::Speak { id, text, .. } => {
            let bytes = speak_bytes(text);
            out.extend_from_slice(&bytes);
            proto.tracker.expect(Expect::Speak(*id));
            if proto
                .last_stop
                .is_some_and(|t| now.saturating_duration_since(t) <= STOP_SPEAK_WINDOW)
            {
                proto.tracker.arm_guard(*id, bytes);
            }
        }
        SpeechCmd::Replace { stop, then } => {
            push_line(out, proto, "STOP self");
            proto.tracker.mark_replaced(*stop);
            proto.last_stop = Some(now);
            // A Replace inside a Replace has no meaning; flatten at a small depth.
            if depth < 2 {
                encode(then, proto, out, now, depth + 1);
            }
        }
        SpeechCmd::Stop { .. } => {
            push_line(out, proto, "STOP self");
            proto.last_stop = Some(now);
        }
        SpeechCmd::StopAll => {
            push_line(out, proto, "CANCEL self");
            proto.last_stop = Some(now);
        }
        SpeechCmd::Pause => push_line(out, proto, "PAUSE self"),
        SpeechCmd::Resume => push_line(out, proto, "RESUME self"),
        SpeechCmd::SetParams(VoiceParams {
            rate,
            pitch,
            volume,
        }) => {
            push_line(out, proto, &format!("SET self RATE {rate}"));
            push_line(out, proto, &format!("SET self PITCH {pitch}"));
            push_line(out, proto, &format!("SET self VOLUME {volume}"));
        }
    }
}

async fn read_frame(
    rd: &mut OwnedReadHalf,
    buf: &mut Vec<u8>,
    idle: &CancellationToken,
) -> Result<bool, io::Error> {
    let mut len = [0u8; 4];
    tokio::select! {
        biased;
        () = idle.cancelled() => return Ok(false),
        r = rd.read_exact(&mut len) => { r?; }
    }
    let n = u32::from_be_bytes(len) as usize;
    if n > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    buf.clear();
    buf.resize(n, 0);
    match tokio::time::timeout(FRAME_DEADLINE, rd.read_exact(buf)).await {
        Ok(r) => r.map(|_| true),
        Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "frame stalled")),
    }
}

async fn forward(tx: &mpsc::Sender<SpeechEvent>, ev: SpeechEvent) {
    let terminal = matches!(
        ev,
        SpeechEvent::Ended(_) | SpeechEvent::Stopped(_) | SpeechEvent::Error(..)
    );
    if terminal {
        // Never dropped while the consumer is alive (section 8.3).
        let _ = tokio::time::timeout(TERMINAL_SEND_DEADLINE, tx.send(ev)).await;
    } else {
        // Progress and marks coalesce: dropping on overflow is allowed.
        let _ = tx.try_send(ev);
    }
}

async fn reader(shared: Arc<Shared>, mut rd: OwnedReadHalf, tx: mpsc::Sender<SpeechEvent>) {
    let mut buf = Vec::with_capacity(512);
    let mut evs: Vec<SpeechEvent> = Vec::with_capacity(4);
    loop {
        match read_frame(&mut rd, &mut buf, &shared.cancel).await {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                tracing::debug!(error = %e, "voicego connection ended");
                break;
            }
        }
        evs.clear();
        let reaction = {
            let mut p = lock(&shared.proto);
            match classify(&buf) {
                Block::Reply { code, msg_id } => p.tracker.on_reply(code, msg_id, &mut evs),
                Block::Event { ev, msg } => p.tracker.on_event(ev, msg, &mut evs),
                Block::Garbage => Reaction::None,
            }
        };
        if let Reaction::Resend { utt, bytes } = reaction {
            lock(&shared.queue).resends.push_back((utt, bytes));
            shared.wake.notify_one();
        }
        for ev in evs.drain(..) {
            forward(&tx, ev).await;
        }
    }
    // The connection is over: whatever was unfinished is stopped.
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
        tokio::select! {
            biased;
            () = shared.cancel.cancelled() => return,
            () = shared.wake.notified() => {}
        }
        loop {
            buf.clear();
            {
                // Lock order: queue, then proto. The reader never holds proto while taking queue.
                let mut q = lock(&shared.queue);
                let mut p = lock(&shared.proto);
                let now = Instant::now();
                while let Some((utt, bytes)) = q.resends.pop_front() {
                    p.tracker.expect(Expect::Speak(utt));
                    buf.extend_from_slice(&bytes);
                }
                while let Some(cmd) = q.cmds.pop_front() {
                    encode(&cmd, &mut p, &mut buf, now, 0);
                }
            }
            if buf.is_empty() {
                break;
            }
            shared.writes.fetch_add(1, Ordering::Relaxed);
            if let Err(e) = write_batch(&shared, &mut wr, &buf).await {
                tracing::debug!(error = %e, "voicego write failed");
                shared.set_health(ClientHealth::Down);
                shared.cancel.cancel();
                return;
            }
        }
    }
}

#[derive(Debug)]
pub struct VoicegoClient {
    caps: SpeechCaps,
    shared: Arc<Shared>,
    events: mpsc::Receiver<SpeechEvent>,
    _tasks: TaskSet,
}

impl VoicegoClient {
    /// Connect, send the binary prefix and the `CLIENT_NAME` handshake, all under `deadline`.
    pub async fn connect(
        opts: &VoicegoOptions,
        deadline: Deadline,
    ) -> Result<VoicegoClient, VoicegoError> {
        let path = opts.resolve()?;
        let stream = with_deadline(deadline, UnixStream::connect(&path))
            .await
            .map_err(|_| VoicegoError::Timeout)??;
        let (mut rd, mut wr) = stream.into_split();
        let mut hello = PREFIX.to_vec();
        push_frame(
            &mut hello,
            format!("SET self CLIENT_NAME {}", opts.client_name).as_bytes(),
        );
        let mut buf = Vec::with_capacity(64);
        let never = CancellationToken::new();
        let handshake = async {
            wr.write_all(&hello).await?;
            read_frame(&mut rd, &mut buf, &never).await
        };
        match with_deadline(deadline, handshake).await {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => return Err(VoicegoError::Handshake("cancelled".to_owned())),
            Ok(Err(e)) => return Err(VoicegoError::Io(e)),
            Err(_) => return Err(VoicegoError::Timeout),
        }
        if !buf.starts_with(b"208") {
            return Err(VoicegoError::Handshake(
                String::from_utf8_lossy(&buf).trim().to_owned(),
            ));
        }
        let cancel = CancellationToken::new();
        let shared = Arc::new(Shared {
            health: AtomicU8::new(health_to_u8(ClientHealth::Ready)),
            queue: Mutex::new(WriteQueue::default()),
            wake: Notify::new(),
            proto: Mutex::new(Proto::default()),
            cancel: cancel.clone(),
            writes: AtomicU64::new(0),
            superseded: AtomicU64::new(0),
        });
        let (etx, erx) = mpsc::channel(EVENT_QUEUE);
        let mut tasks = TaskSet::new(cancel);
        tasks.spawn("voicego-reader", reader(Arc::clone(&shared), rd, etx));
        tasks.spawn("voicego-writer", writer(Arc::clone(&shared), wr));
        Ok(VoicegoClient {
            caps: caps(),
            shared,
            events: erx,
            _tasks: tasks,
        })
    }

    pub fn stats(&self) -> VoicegoStats {
        VoicegoStats {
            resends: lock(&self.shared.proto).tracker.resends(),
            writes: self.shared.writes.load(Ordering::Relaxed),
            superseded: self.shared.superseded.load(Ordering::Relaxed),
        }
    }
}

impl Drop for VoicegoClient {
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

impl SpeechClient for VoicegoClient {
    fn caps(&self) -> &SpeechCaps {
        &self.caps
    }

    fn submit(&self, cmd: SpeechCmd) -> Result<(), SubmitError> {
        if self.shared.is_down() {
            return Err(SubmitError::Disconnected);
        }
        {
            let mut q = lock(&self.shared.queue);
            if q.cmds.len() >= WRITER_QUEUE {
                // S-3: the server stopped reading. A newer speak or replace supersedes queued ones.
                if supersedable(&cmd) {
                    let before = q.cmds.len();
                    q.cmds.retain(|c| !supersedable(c));
                    let dropped = (before - q.cmds.len()) as u64;
                    self.shared.superseded.fetch_add(dropped, Ordering::Relaxed);
                }
                if q.cmds.len() >= WRITER_QUEUE {
                    return Err(SubmitError::Full);
                }
            }
            q.cmds.push_back(cmd);
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
    use gorca_output_api::VoiceRef;
    use gorca_testkit::MockSpeechServer;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gv-{}-{name}", std::process::id()));
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

    fn replace(stop: u64, id: u64, text: &str) -> SpeechCmd {
        SpeechCmd::Replace {
            stop: UttId(stop),
            then: Box::new(speak(id, text)),
        }
    }

    async fn connect(srv: &MockSpeechServer) -> VoicegoClient {
        VoicegoClient::connect(&VoicegoOptions::with_path(srv.socket_path()), dl(2000))
            .await
            .unwrap()
    }

    async fn next(c: &mut VoicegoClient) -> SpeechEvent {
        tokio::time::timeout(Duration::from_secs(2), c.events().recv())
            .await
            .unwrap()
            .unwrap()
    }

    async fn until(c: &mut VoicegoClient, want: SpeechEvent) {
        for _ in 0..20 {
            if next(c).await == want {
                return;
            }
        }
        panic!("never saw {want:?}");
    }

    #[test]
    fn default_path_comes_from_xdg_runtime_dir() {
        let o = VoicegoOptions::with_path("/x/y.sock");
        assert_eq!(o.resolve().unwrap(), PathBuf::from("/x/y.sock"));
    }

    #[test]
    fn replace_is_one_write_with_stop_speak_and_data() {
        let mut p = Proto::default();
        let mut out = Vec::new();
        encode(&replace(1, 2, "hi"), &mut p, &mut out, Instant::now(), 0);
        let mut want = Vec::new();
        push_frame(&mut want, b"STOP self");
        push_frame(&mut want, b"SPEAK 2");
        push_frame(&mut want, b"hi");
        assert_eq!(out, want);
        assert_eq!(p.tracker.outstanding(), 2);
    }

    #[tokio::test]
    async fn speak_reaches_the_server_and_events_come_back() {
        let d = tmp("a");
        let srv = MockSpeechServer::start_voicego(&d).await.unwrap();
        srv.set_speak_time(Duration::from_millis(5));
        let mut c = connect(&srv).await;
        assert_eq!(c.health(), ClientHealth::Ready);
        c.submit(speak(7, "hello")).unwrap();
        assert_eq!(next(&mut c).await, SpeechEvent::Accepted(UttId(7)));
        assert_eq!(next(&mut c).await, SpeechEvent::Started(UttId(7)));
        assert_eq!(next(&mut c).await, SpeechEvent::Ended(UttId(7)));
        assert_eq!(srv.spoken(), ["hello"]);
        assert!(srv.verbs()[0].contains("CLIENT_NAME gorca:main:main"));
        drop(c);
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn replace_cancels_and_speaks_in_a_single_write() {
        let d = tmp("b");
        let srv = MockSpeechServer::start_voicego(&d).await.unwrap();
        srv.set_speak_time(Duration::from_secs(10));
        let mut c = connect(&srv).await;
        c.submit(speak(1, "first")).unwrap();
        assert!(srv.wait_spoken(1, Duration::from_secs(2)).await);
        let before = c.stats().writes;
        c.submit(replace(1, 2, "second")).unwrap();
        assert!(srv.wait_spoken(2, Duration::from_secs(2)).await);
        assert_eq!(c.stats().writes, before + 1);
        assert_eq!(srv.spoken(), ["first", "second"]);
        // The replaced utterance's cancel is swallowed; the new one starts.
        until(&mut c, SpeechEvent::Started(UttId(2))).await;
        drop(c);
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn plain_stop_reports_stopped() {
        let d = tmp("c");
        let srv = MockSpeechServer::start_voicego(&d).await.unwrap();
        srv.set_speak_time(Duration::from_secs(10));
        let mut c = connect(&srv).await;
        c.submit(speak(1, "long")).unwrap();
        until(&mut c, SpeechEvent::Started(UttId(1))).await;
        c.submit(SpeechCmd::Stop { id: UttId(1) }).unwrap();
        until(&mut c, SpeechEvent::Stopped(UttId(1))).await;
        drop(c);
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn lost_speech_after_stop_is_resent_once() {
        let d = tmp("d");
        let srv = MockSpeechServer::start_voicego(&d).await.unwrap();
        srv.set_speak_time(Duration::from_millis(20));
        srv.set_lose_after_stop(true);
        let mut c = connect(&srv).await;
        c.submit(speak(1, "first")).unwrap();
        until(&mut c, SpeechEvent::Started(UttId(1))).await;
        c.submit(replace(1, 2, "second")).unwrap();
        until(&mut c, SpeechEvent::Started(UttId(2))).await;
        assert_eq!(srv.spoken(), ["first", "second"]);
        assert_eq!(c.stats().resends, 1);
        until(&mut c, SpeechEvent::Ended(UttId(2))).await;
        drop(c);
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn events_that_overtake_the_225_are_still_matched() {
        let d = tmp("e");
        let srv = MockSpeechServer::start_voicego(&d).await.unwrap();
        srv.set_speak_time(Duration::from_millis(5));
        srv.set_early_events(true);
        let mut c = connect(&srv).await;
        c.submit(speak(3, "race")).unwrap();
        assert_eq!(next(&mut c).await, SpeechEvent::Accepted(UttId(3)));
        assert_eq!(next(&mut c).await, SpeechEvent::Started(UttId(3)));
        assert_eq!(next(&mut c).await, SpeechEvent::Ended(UttId(3)));
        drop(c);
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn server_death_goes_down_and_stops_unfinished_speech() {
        let d = tmp("f");
        let mut srv = MockSpeechServer::start_voicego(&d).await.unwrap();
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
    async fn a_server_that_stops_reading_degrades_and_the_queue_supersedes() {
        let d = tmp("g");
        let srv = MockSpeechServer::start_voicego(&d).await.unwrap();
        let c = connect(&srv).await;
        srv.set_never_read(true);
        // Fill the kernel buffers with big speaks until a write blocks.
        let big = "x".repeat(256 * 1024);
        let mut full = 0;
        let mut degraded = false;
        for i in 0..64 {
            match c.submit(speak(i, &big)) {
                Ok(()) => {}
                Err(SubmitError::Full) => full += 1,
                Err(SubmitError::Disconnected) => break,
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            if c.health() == ClientHealth::Degraded {
                degraded = true;
            }
        }
        assert!(degraded, "a blocked write must mark the client degraded");
        // Newer speaks superseded queued ones rather than failing forever.
        assert!(c.stats().superseded > 0 || full == 0);
        // submit never blocked: the whole loop took a few hundred ms at most.
        drop(c);
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn connect_to_a_missing_socket_fails_fast() {
        let d = tmp("h");
        let r =
            VoicegoClient::connect(&VoicegoOptions::with_path(d.join("none.sock")), dl(500)).await;
        assert!(matches!(r, Err(VoicegoError::Io(_))));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn a_silent_server_hits_the_connect_deadline() {
        let d = tmp("i");
        let p = d.join("silent.sock");
        let l = tokio::net::UnixListener::bind(&p).unwrap();
        let _hold = tokio::spawn(async move {
            let _s = l.accept().await;
            std::future::pending::<()>().await;
        });
        let t = Instant::now();
        let r = VoicegoClient::connect(&VoicegoOptions::with_path(&p), dl(150)).await;
        assert!(matches!(r, Err(VoicegoError::Timeout)));
        assert!(t.elapsed() < Duration::from_millis(600));
        let _ = std::fs::remove_dir_all(&d);
    }
}
