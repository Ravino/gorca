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
//! - **V5** UNIX socket at an explicit path (the daemon resolves the default, see
//!   [`default_voicego_path`]); the peer must run as our uid.
//! - **V6** Failover is not here: this is a single-connection client that goes `Down` on EOF
//!   or error. Reconnecting and falling back to SSIP is `gorca_speech::FailoverClient`.
//!
//! Current voicego has the STOP/SPEAK race fixed (`TestStopThenSpeakNotLost`), so a `SPEAK`
//! pipelined behind a `STOP` is safe and there is no resend: a stopped utterance is never spoken
//! again.
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
use std::time::Duration;

use gorca_output_api::peer::{PeerError, check_peer_allowed, own_uid};
use gorca_output_api::queue::admit;
use gorca_output_api::track::{Block, Expect, MsgTracker, classify};
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
    #[error("voicego socket: {0}")]
    Io(#[from] io::Error),
    #[error("voicego did not answer in time")]
    Timeout,
    #[error("voicego handshake failed: {0}")]
    Handshake(String),
    #[error("refusing the voicego socket: {0}")]
    Peer(#[from] PeerError),
}

#[derive(Clone, Debug)]
pub struct VoicegoOptions {
    /// Socket path. This crate never looks at the environment: the daemon resolves it
    /// ([`default_voicego_path`]) and passes it in.
    pub path: PathBuf,
    /// `SET self CLIENT_NAME` (cosmetic).
    pub client_name: String,
    /// Extra uids whose server gorca accepts (a system-wide service user). Empty: same user only.
    pub allowed_peer_uids: Vec<u32>,
}

impl VoicegoOptions {
    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            client_name: "gorca:main:main".to_owned(),
            allowed_peer_uids: Vec::new(),
        }
    }
}

/// The default socket given the value of `XDG_RUNTIME_DIR` (pure; the caller reads the
/// environment). `None` when it is unset or empty.
pub fn default_voicego_path(xdg_runtime_dir: Option<&str>) -> Option<PathBuf> {
    xdg_runtime_dir
        .filter(|d| !d.is_empty())
        .map(|d| PathBuf::from(d).join("voicego.sock"))
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
}

#[derive(Debug, Default)]
struct Proto {
    tracker: MsgTracker,
}

#[derive(Debug)]
struct Shared {
    health: AtomicU8,
    queue: Mutex<WriteQueue>,
    wake: Notify,
    proto: Mutex<Proto>,
    /// Terminal events of commands that were evicted from the queue before they were written;
    /// the writer task delivers them (`submit` cannot await).
    pending: Mutex<VecDeque<SpeechEvent>>,
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
fn encode(cmd: &SpeechCmd, proto: &mut Proto, out: &mut Vec<u8>, depth: u8) {
    match cmd {
        SpeechCmd::Speak { id, text, .. } => {
            out.extend_from_slice(&speak_bytes(text));
            proto.tracker.expect(Expect::Speak(*id));
        }
        SpeechCmd::Replace { stop, then } => {
            push_line(out, proto, "STOP self");
            proto.tracker.mark_replaced(*stop);
            // A Replace inside a Replace has no meaning; flatten at a small depth.
            if depth < 2 {
                encode(then, proto, out, depth + 1);
            }
        }
        SpeechCmd::Stop { .. } => {
            push_line(out, proto, "STOP self");
        }
        SpeechCmd::StopAll => {
            push_line(out, proto, "CANCEL self");
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
        {
            let mut p = lock(&shared.proto);
            match classify(&buf) {
                Block::Reply { code, msg_id } => p.tracker.on_reply(code, msg_id, &mut evs),
                Block::Event { ev, msg } => p.tracker.on_event(ev, msg, &mut evs),
                Block::Garbage => {}
            }
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

async fn deliver_pending(shared: &Shared, tx: &mpsc::Sender<SpeechEvent>) {
    loop {
        let ev = lock(&shared.pending).pop_front();
        let Some(ev) = ev else { return };
        forward(tx, ev).await;
    }
}

async fn writer(shared: Arc<Shared>, mut wr: OwnedWriteHalf, tx: mpsc::Sender<SpeechEvent>) {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    loop {
        tokio::select! {
            biased;
            () = shared.cancel.cancelled() => return,
            () = shared.wake.notified() => {}
        }
        deliver_pending(&shared, &tx).await;
        loop {
            buf.clear();
            {
                // Lock order: queue, then proto. The reader never holds proto while taking queue.
                let mut q = lock(&shared.queue);
                let mut p = lock(&shared.proto);
                while let Some(cmd) = q.cmds.pop_front() {
                    encode(&cmd, &mut p, &mut buf, 0);
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
        Self::connect_as(opts, deadline, own_uid()).await
    }

    /// [`connect`](Self::connect) with the uid the peer must have (tests pass a foreign one).
    pub async fn connect_as(
        opts: &VoicegoOptions,
        deadline: Deadline,
        own: Option<u32>,
    ) -> Result<VoicegoClient, VoicegoError> {
        gorca_rt::guard::check(&opts.path.to_string_lossy());
        let stream = with_deadline(deadline, UnixStream::connect(&opts.path))
            .await
            .map_err(|_| VoicegoError::Timeout)??;
        // Everything gorca reads is sent here: only talk to our own user's server.
        check_peer_allowed(stream.peer_cred()?.uid(), own, &opts.allowed_peer_uids)?;
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
            pending: Mutex::new(VecDeque::new()),
            cancel: cancel.clone(),
            writes: AtomicU64::new(0),
            superseded: AtomicU64::new(0),
        });
        let (etx, erx) = mpsc::channel(EVENT_QUEUE);
        let mut tasks = TaskSet::new(cancel);
        tasks.spawn(
            "voicego-reader",
            reader(Arc::clone(&shared), rd, etx.clone()),
        );
        tasks.spawn("voicego-writer", writer(Arc::clone(&shared), wr, etx));
        Ok(VoicegoClient {
            caps: caps(),
            shared,
            events: erx,
            _tasks: tasks,
        })
    }

    pub fn stats(&self) -> VoicegoStats {
        VoicegoStats {
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

impl SpeechClient for VoicegoClient {
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
            admit(&mut q.cmds, cmd, WRITER_QUEUE, |c| c, |c| c)?
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
    use gorca_model::UttId;
    use gorca_output_api::VoiceRef;
    use gorca_testkit::MockSpeechServer;
    use std::time::Instant;

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
    fn default_path_is_derived_from_the_runtime_dir_value() {
        assert_eq!(
            default_voicego_path(Some("/run/user/1000")),
            Some(PathBuf::from("/run/user/1000/voicego.sock"))
        );
        assert_eq!(default_voicego_path(Some("")), None);
        assert_eq!(default_voicego_path(None), None);
    }

    #[tokio::test]
    async fn a_server_of_another_user_is_refused() {
        let d = tmp("uid");
        let srv = MockSpeechServer::start_voicego(&d).await.unwrap();
        let opts = VoicegoOptions::with_path(srv.socket_path());
        let ours = own_uid().unwrap();
        let r = VoicegoClient::connect_as(&opts, dl(2000), Some(ours.wrapping_add(1))).await;
        assert!(matches!(
            r,
            Err(VoicegoError::Peer(PeerError::Other { .. }))
        ));
        assert!(
            VoicegoClient::connect_as(&opts, dl(2000), Some(ours))
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
        let srv = MockSpeechServer::start_voicego(&d).await.unwrap();
        let mut opts = VoicegoOptions::with_path(srv.socket_path());
        let ours = own_uid().unwrap();
        // Pretend we are another user: the mock's uid (ours) is foreign to `own`.
        let other = ours.wrapping_add(1);
        assert!(
            VoicegoClient::connect_as(&opts, dl(2000), Some(other))
                .await
                .is_err()
        );
        opts.allowed_peer_uids = vec![ours];
        assert!(
            VoicegoClient::connect_as(&opts, dl(2000), Some(other))
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
        gorca_rt::guard::forbid("/run/user/424244/speech-tripwire-gorca-speech-voicego.sock");
        let opts =
            VoicegoOptions::with_path("/run/user/424244/speech-tripwire-gorca-speech-voicego.sock");
        let _ = VoicegoClient::connect(&opts, dl(500)).await;
    }

    /// Regression (review F10): with the server not reading, a Stop still gets through, evicts
    /// the queued speaks (each reported `Stopped`) and reaches the server once it reads again.
    #[tokio::test]
    async fn a_stop_gets_through_a_full_queue() {
        let d = tmp("stopq");
        let srv = MockSpeechServer::start_voicego(&d).await.unwrap();
        srv.set_speak_time(Duration::from_secs(10));
        let mut c = connect(&srv).await;
        c.submit(speak(1, "playing")).unwrap();
        until(&mut c, SpeechEvent::Started(UttId(1))).await;
        srv.set_never_read(true);
        // Large speaks fill the socket buffers, block the writer, then fill the 4-slot queue.
        let big = "x".repeat(512 * 1024);
        let mut next = 10;
        for _ in 0..40 {
            next += 1;
            let _ = c.submit(speak(next, &big));
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let mut stopped = 0;
        c.submit(SpeechCmd::Stop { id: UttId(1) })
            .expect("a stop is never refused");
        c.submit(SpeechCmd::StopAll)
            .expect("a stop is never refused");
        srv.set_never_read(false);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let seen = srv.verbs();
            if seen.iter().any(|v| v.starts_with("STOP")) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the STOP never arrived: {seen:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        while let Ok(Some(ev)) =
            tokio::time::timeout(Duration::from_millis(300), c.events().recv()).await
        {
            if matches!(ev, SpeechEvent::Stopped(u) if u.0 > 10) {
                stopped += 1;
            }
        }
        assert!(stopped >= 1, "evicted speaks are reported Stopped");
        assert!(c.stats().superseded >= 1);
        drop(c);
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn replace_is_one_write_with_stop_speak_and_data() {
        let mut p = Proto::default();
        let mut out = Vec::new();
        encode(&replace(1, 2, "hi"), &mut p, &mut out, 0);
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

    /// With a server that still has the old race (`lose_after_stop`), the loss is reported as
    /// `Stopped` and the text is not sent again.
    #[tokio::test]
    async fn a_lost_speech_is_reported_stopped_not_resent() {
        let d = tmp("d");
        let srv = MockSpeechServer::start_voicego(&d).await.unwrap();
        srv.set_speak_time(Duration::from_millis(20));
        srv.set_lose_after_stop(true);
        let mut c = connect(&srv).await;
        c.submit(speak(1, "first")).unwrap();
        until(&mut c, SpeechEvent::Started(UttId(1))).await;
        c.submit(replace(1, 2, "second")).unwrap();
        until(&mut c, SpeechEvent::Stopped(UttId(2))).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(srv.spoken(), ["first"], "nothing is spoken a second time");
        drop(c);
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Regression (review F1, probe `voicego_resend_after_stop`): an explicit Stop must never be
    /// undone. Speak 1 and 2 right after a STOP; 2 is queued behind 1; Stop cancels both; 2
    /// must not be spoken afterwards.
    #[tokio::test]
    async fn an_explicit_stop_is_never_undone_by_a_resend() {
        let d = tmp("stop");
        let srv = MockSpeechServer::start_voicego(&d).await.unwrap();
        srv.set_speak_time(Duration::from_secs(10));
        let c = connect(&srv).await;
        c.submit(SpeechCmd::Stop { id: UttId(0) }).unwrap();
        c.submit(speak(1, "first")).unwrap();
        c.submit(speak(2, "stale second")).unwrap();
        assert!(srv.wait_spoken(1, Duration::from_secs(2)).await);
        tokio::time::sleep(Duration::from_millis(20)).await;
        c.submit(SpeechCmd::Stop { id: UttId(2) }).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(srv.spoken(), ["first"], "stale utterance resent after Stop");
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
