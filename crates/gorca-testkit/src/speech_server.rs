// SPDX-License-Identifier: GPL-3.0-or-later
//! A mock speech server for tests: speaks voicego's binary-framed SSIP or plain text SSIP over a
//! UNIX socket in a caller-supplied directory, records everything it is told to speak, and emits
//! `225` / `701` / `702` / `703` the way `docs/platform/voicego-protocol.md` describes.
//!
//! Knobs for the failure modes the speech stack must survive: a reply delay, a server that
//! stops reading (S-3), events that overtake their `225`, connections killed from the outside,
//! stop and restart on the same path (voicego down and back), and `lose_after_stop`, which
//! emulates the pre-fix voicegod bug where a `SPEAK` pipelined behind a `STOP` is cancelled.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use gorca_rt::CancellationToken;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinSet;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MockProto {
    /// `AA 56 47 01` prefix, then `u32` big-endian length-prefixed frames.
    Voicego,
    /// Text SSIP as spoken by speech-dispatcher.
    Ssip,
}

/// One utterance that reached the (pretend) engine, recorded when `701 BEGIN` is sent.
#[derive(Clone, Debug)]
pub struct Spoken {
    pub at: Instant,
    pub text: String,
    pub msg_id: u64,
}

#[derive(Debug)]
struct Shared {
    proto: MockProto,
    spoken: Mutex<Vec<Spoken>>,
    verbs: Mutex<Vec<String>>,
    changed: Notify,
    next_msg: AtomicU64,
    reply_delay_us: AtomicU64,
    speak_time_us: AtomicU64,
    never_read: AtomicBool,
    lose_after_stop: AtomicBool,
    early_events: AtomicBool,
    accepted: AtomicUsize,
    live: AtomicUsize,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug)]
pub struct MockSpeechServer {
    shared: Arc<Shared>,
    path: PathBuf,
    conns: Arc<Mutex<CancellationToken>>,
    run: Option<(CancellationToken, tokio::task::JoinHandle<()>)>,
}

impl MockSpeechServer {
    /// Listen on `dir/mock.sock` (keep `dir` short: UNIX socket paths are limited to 107 bytes).
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)] // async like `stop`
    pub async fn start(dir: &Path, proto: MockProto) -> io::Result<Self> {
        let mut s = Self {
            shared: Arc::new(Shared {
                proto,
                spoken: Mutex::new(Vec::new()),
                verbs: Mutex::new(Vec::new()),
                changed: Notify::new(),
                next_msg: AtomicU64::new(1),
                reply_delay_us: AtomicU64::new(0),
                speak_time_us: AtomicU64::new(30_000),
                never_read: AtomicBool::new(false),
                lose_after_stop: AtomicBool::new(false),
                early_events: AtomicBool::new(false),
                accepted: AtomicUsize::new(0),
                live: AtomicUsize::new(0),
            }),
            path: dir.join("mock.sock"),
            conns: Arc::new(Mutex::new(CancellationToken::new())),
            run: None,
        };
        s.restart()?;
        Ok(s)
    }

    #[allow(clippy::unused_async)]
    pub async fn start_voicego(dir: &Path) -> io::Result<Self> {
        Self::start(dir, MockProto::Voicego).await
    }

    #[allow(clippy::unused_async)]
    pub async fn start_ssip(dir: &Path) -> io::Result<Self> {
        Self::start(dir, MockProto::Ssip).await
    }

    pub fn socket_path(&self) -> &Path {
        &self.path
    }

    /// Bodies of every utterance that started, in order.
    pub fn spoken(&self) -> Vec<String> {
        lock(&self.shared.spoken)
            .iter()
            .map(|s| s.text.clone())
            .collect()
    }

    pub fn spoken_records(&self) -> Vec<Spoken> {
        lock(&self.shared.spoken).clone()
    }

    /// Every command line received, in order (`STOP self`, `SET self ...`, `SPEAK 5`, ...).
    pub fn verbs(&self) -> Vec<String> {
        lock(&self.shared.verbs).clone()
    }

    /// Connections accepted since the server was created.
    pub fn accepted(&self) -> usize {
        self.shared.accepted.load(Ordering::Acquire)
    }

    /// Connections open right now.
    pub fn live_connections(&self) -> usize {
        self.shared.live.load(Ordering::Acquire)
    }

    /// Wait until at least `n` utterances started. False on timeout.
    pub async fn wait_spoken(&self, n: usize, timeout: Duration) -> bool {
        self.wait_until(timeout, || lock(&self.shared.spoken).len() >= n)
            .await
    }

    /// Wait until `pred` holds over the verb log. False on timeout.
    pub async fn wait_verbs(&self, timeout: Duration, pred: impl Fn(&[String]) -> bool) -> bool {
        self.wait_until(timeout, || pred(&lock(&self.shared.verbs)))
            .await
    }

    /// Wait for `live_connections() >= n`.
    pub async fn wait_connections(&self, n: usize, timeout: Duration) -> bool {
        self.wait_until(timeout, || self.live_connections() >= n)
            .await
    }

    async fn wait_until(&self, timeout: Duration, cond: impl Fn() -> bool) -> bool {
        let end = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.shared.changed.notified();
            if cond() {
                return true;
            }
            // Wake on change, but also poll: some state (live connections) has no notifier.
            let step = Duration::from_millis(10);
            if tokio::time::timeout_at(end, tokio::time::timeout(step, notified))
                .await
                .is_err()
            {
                return cond();
            }
        }
    }

    pub fn set_reply_delay(&self, d: Duration) {
        let us = u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
        self.shared.reply_delay_us.store(us, Ordering::Release);
    }

    /// How long an utterance "plays" between `701 BEGIN` and `702 END`.
    pub fn set_speak_time(&self, d: Duration) {
        let us = u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
        self.shared.speak_time_us.store(us, Ordering::Release);
    }

    /// Stop reading from every connection (the kernel buffers fill, the client's writes block).
    pub fn set_never_read(&self, on: bool) {
        self.shared.never_read.store(on, Ordering::Release);
    }

    /// Emulate the pre-fix voicegod: the first `SPEAK` after a `STOP` that cancelled something is
    /// itself cancelled (`703` without `701`).
    pub fn set_lose_after_stop(&self, on: bool) {
        self.shared.lose_after_stop.store(on, Ordering::Release);
    }

    /// Send `701` before the `225` of the same message (the documented race).
    pub fn set_early_events(&self, on: bool) {
        self.shared.early_events.store(on, Ordering::Release);
    }

    /// Drop every client connection; the listener stays up.
    pub fn kill_connections(&self) {
        let old = std::mem::replace(&mut *lock(&self.conns), CancellationToken::new());
        old.cancel();
    }

    /// Stop listening, close every connection and remove the socket file (voicego down).
    pub async fn stop(&mut self) {
        self.kill_connections();
        if let Some((token, join)) = self.run.take() {
            token.cancel();
            let _ = join.await;
        }
        let _ = std::fs::remove_file(&self.path);
    }

    /// Listen again on the same path (voicego back). Records are kept.
    pub fn restart(&mut self) -> io::Result<()> {
        if self.run.is_some() {
            return Ok(());
        }
        let _ = std::fs::remove_file(&self.path);
        let listener = UnixListener::bind(&self.path)?;
        let token = CancellationToken::new();
        let shared = Arc::clone(&self.shared);
        let conns = Arc::clone(&self.conns);
        let t = token.clone();
        let join = tokio::spawn(async move { accept_loop(listener, shared, conns, t).await });
        self.run = Some((token, join));
        Ok(())
    }
}

impl Drop for MockSpeechServer {
    fn drop(&mut self) {
        self.kill_connections();
        if let Some((token, join)) = self.run.take() {
            token.cancel();
            join.abort();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

async fn accept_loop(
    listener: UnixListener,
    shared: Arc<Shared>,
    conns: Arc<Mutex<CancellationToken>>,
    stop: CancellationToken,
) {
    let mut set: JoinSet<()> = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            () = stop.cancelled() => break,
            Some(_) = set.join_next(), if !set.is_empty() => {}
            r = listener.accept() => {
                let Ok((stream, _)) = r else { continue };
                let token = lock(&conns).child_token();
                shared.accepted.fetch_add(1, Ordering::AcqRel);
                shared.live.fetch_add(1, Ordering::AcqRel);
                let sh = Arc::clone(&shared);
                set.spawn(async move {
                    handle_conn(stream, Arc::clone(&sh), token).await;
                    sh.live.fetch_sub(1, Ordering::AcqRel);
                    sh.changed.notify_waiters();
                });
                shared.changed.notify_waiters();
            }
        }
    }
    set.shutdown().await;
}

fn encode(proto: MockProto, payload: &str) -> Vec<u8> {
    match proto {
        MockProto::Voicego => {
            let mut v = Vec::with_capacity(payload.len() + 4);
            let len = u32::try_from(payload.len()).unwrap_or(u32::MAX);
            v.extend_from_slice(&len.to_be_bytes());
            v.extend_from_slice(payload.as_bytes());
            v
        }
        MockProto::Ssip => payload.as_bytes().to_vec(),
    }
}

enum PlayerCmd {
    Enqueue { id: u64, body: String },
    Cancel,
}

fn event_block(code: u16, msg: u64, tail: &str) -> String {
    format!("{code}-{msg}\r\n{code}-1\r\n{code} {tail}\r\n")
}

/// Plays queued messages one at a time, emitting events.
async fn player(
    shared: Arc<Shared>,
    out: mpsc::Sender<Vec<u8>>,
    mut rx: mpsc::Receiver<PlayerCmd>,
    token: CancellationToken,
) {
    let proto = shared.proto;
    let mut queue: VecDeque<(u64, String)> = VecDeque::new();
    let mut active: Option<u64> = None;
    let mut lose_next = false;
    let mut lost: Vec<u64> = Vec::new();
    let mut timer: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;
    let send = |s: String| {
        let out = out.clone();
        async move {
            let _ = out.send(encode(proto, &s)).await;
        }
    };
    loop {
        // Start the next message if idle.
        while active.is_none() {
            let Some((id, body)) = queue.pop_front() else {
                break;
            };
            if lost.contains(&id) {
                lost.retain(|m| *m != id);
                send(event_block(703, id, "CANCELED")).await;
                continue;
            }
            lock(&shared.spoken).push(Spoken {
                at: Instant::now(),
                text: body,
                msg_id: id,
            });
            shared.changed.notify_waiters();
            send(event_block(701, id, "BEGIN")).await;
            active = Some(id);
            let us = shared.speak_time_us.load(Ordering::Acquire);
            timer = Some(Box::pin(tokio::time::sleep(Duration::from_micros(us))));
        }
        let timer_fut = async {
            match timer.as_mut() {
                Some(t) => t.await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            () = token.cancelled() => return,
            cmd = rx.recv() => match cmd {
                None => return,
                Some(PlayerCmd::Enqueue { id, body }) => {
                    if lose_next {
                        lose_next = false;
                        lost.push(id);
                    }
                    queue.push_back((id, body));
                }
                Some(PlayerCmd::Cancel) => {
                    let mut any = false;
                    if let Some(id) = active.take() {
                        any = true;
                        send(event_block(703, id, "CANCELED")).await;
                    }
                    timer = None;
                    for (id, _) in queue.drain(..) {
                        any = true;
                        send(event_block(703, id, "CANCELED")).await;
                    }
                    lost.clear();
                    if any && shared.lose_after_stop.load(Ordering::Acquire) {
                        lose_next = true;
                    }
                }
            },
            () = timer_fut => {
                timer = None;
                if let Some(id) = active.take() {
                    send(event_block(702, id, "END")).await;
                }
            }
        }
    }
}

async fn writer(
    shared: Arc<Shared>,
    mut wr: tokio::net::unix::OwnedWriteHalf,
    mut rx: mpsc::Receiver<Vec<u8>>,
) {
    while let Some(buf) = rx.recv().await {
        let us = shared.reply_delay_us.load(Ordering::Acquire);
        if us > 0 {
            tokio::time::sleep(Duration::from_micros(us)).await;
        }
        if wr.write_all(&buf).await.is_err() {
            return;
        }
    }
}

/// What a received line wants next.
enum Next {
    Continue,
    Data,
    Quit,
}

struct Conn {
    shared: Arc<Shared>,
    out: mpsc::Sender<Vec<u8>>,
    player: mpsc::Sender<PlayerCmd>,
}

impl Conn {
    async fn send(&self, payload: &str) {
        let _ = self.out.send(encode(self.shared.proto, payload)).await;
    }

    async fn command(&self, line: &str) -> Next {
        lock(&self.shared.verbs).push(line.to_owned());
        self.shared.changed.notify_waiters();
        let verb = line
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        match verb.as_str() {
            "SET" => {
                let code = if line.to_ascii_uppercase().contains("CLIENT_NAME") {
                    "208 OK CLIENT NAME SET"
                } else {
                    "203 OK SET"
                };
                self.send(&format!("{code}\r\n")).await;
            }
            "SPEAK" => {
                self.send("230 OK RECEIVING DATA\r\n").await;
                return Next::Data;
            }
            "STOP" | "CANCEL" => {
                let code = if verb == "STOP" {
                    "210 OK STOPPED"
                } else {
                    "213 OK CANCELED"
                };
                self.send(&format!("{code}\r\n")).await;
                let _ = self.player.send(PlayerCmd::Cancel).await;
            }
            "PAUSE" => self.send("211 OK PAUSED\r\n").await,
            "RESUME" => self.send("212 OK RESUMED\r\n").await,
            "X-PING" => self.send("280 PONG\r\n").await,
            "HELLO" => self.send("200 OK HELLO\r\n").await,
            "QUIT" | "BYE" => {
                self.send("231 HAPPY HACKING\r\n").await;
                return Next::Quit;
            }
            _ => self.send("420 ERR UNKNOWN COMMAND\r\n").await,
        }
        Next::Continue
    }

    async fn body(&self, text: String) {
        let id = self.shared.next_msg.fetch_add(1, Ordering::AcqRel);
        let reply = format!("225-{id}\r\n225 OK MESSAGE QUEUED\r\n");
        if self.shared.early_events.load(Ordering::Acquire) {
            let _ = self
                .player
                .send(PlayerCmd::Enqueue { id, body: text })
                .await;
            tokio::time::sleep(Duration::from_millis(5)).await;
            self.send(&reply).await;
        } else {
            self.send(&reply).await;
            let _ = self
                .player
                .send(PlayerCmd::Enqueue { id, body: text })
                .await;
        }
    }
}

async fn pause_while_not_reading(shared: &Shared) {
    while shared.never_read.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn handle_conn(stream: UnixStream, shared: Arc<Shared>, token: CancellationToken) {
    let (rd, wr) = stream.into_split();
    let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(256);
    let (pl_tx, pl_rx) = mpsc::channel::<PlayerCmd>(64);
    let mut subs: JoinSet<()> = JoinSet::new();
    subs.spawn(writer(Arc::clone(&shared), wr, out_rx));
    subs.spawn(player(
        Arc::clone(&shared),
        out_tx.clone(),
        pl_rx,
        token.clone(),
    ));
    let conn = Conn {
        shared: Arc::clone(&shared),
        out: out_tx,
        player: pl_tx,
    };
    let proto = shared.proto;
    let work = async {
        match proto {
            MockProto::Voicego => serve_voicego(rd, &conn).await,
            MockProto::Ssip => serve_ssip(rd, &conn).await,
        }
    };
    tokio::select! {
        () = token.cancelled() => {}
        () = work => {}
    }
    subs.shutdown().await;
}

async fn serve_voicego(mut rd: tokio::net::unix::OwnedReadHalf, conn: &Conn) {
    let mut prefix = [0u8; 4];
    if rd.read_exact(&mut prefix).await.is_err() || prefix != [0xAA, b'V', b'G', 1] {
        return;
    }
    let mut want_data = false;
    loop {
        pause_while_not_reading(&conn.shared).await;
        let mut len = [0u8; 4];
        if rd.read_exact(&mut len).await.is_err() {
            return;
        }
        let n = u32::from_be_bytes(len) as usize;
        if n > 16 << 20 {
            return;
        }
        let mut payload = vec![0u8; n];
        if rd.read_exact(&mut payload).await.is_err() {
            return;
        }
        let text = String::from_utf8_lossy(&payload).into_owned();
        if want_data {
            want_data = false;
            conn.body(text).await;
        } else {
            match conn.command(&text).await {
                Next::Continue => {}
                Next::Data => want_data = true,
                Next::Quit => return,
            }
        }
    }
}

async fn serve_ssip(rd: tokio::net::unix::OwnedReadHalf, conn: &Conn) {
    let mut lines = BufReader::new(rd);
    let mut data: Option<String> = None;
    let mut buf = Vec::new();
    loop {
        pause_while_not_reading(&conn.shared).await;
        buf.clear();
        match lines.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim_end_matches(['\r', '\n']);
        if let Some(acc) = data.as_mut() {
            if line == "." {
                let text = data.take().unwrap_or_default();
                conn.body(text).await;
            } else {
                if !acc.is_empty() {
                    acc.push('\n');
                }
                acc.push_str(
                    line.strip_prefix('.')
                        .filter(|_| line.starts_with(".."))
                        .unwrap_or(line),
                );
            }
            continue;
        }
        match conn.command(line).await {
            Next::Continue => {}
            Next::Data => data = Some(String::new()),
            Next::Quit => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gk-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    async fn frame(s: &mut UnixStream, payload: &str) {
        s.write_all(&encode(MockProto::Voicego, payload))
            .await
            .unwrap();
    }

    async fn read_frame(s: &mut UnixStream) -> String {
        let mut len = [0u8; 4];
        s.read_exact(&mut len).await.unwrap();
        let mut p = vec![0u8; u32::from_be_bytes(len) as usize];
        s.read_exact(&mut p).await.unwrap();
        String::from_utf8(p).unwrap()
    }

    #[tokio::test]
    async fn voicego_mode_speaks_and_reports_events() {
        let d = dir("v1");
        let srv = MockSpeechServer::start_voicego(&d).await.unwrap();
        srv.set_speak_time(Duration::from_millis(5));
        let mut c = UnixStream::connect(srv.socket_path()).await.unwrap();
        c.write_all(&[0xAA, b'V', b'G', 1]).await.unwrap();
        frame(&mut c, "SPEAK 5").await;
        frame(&mut c, "hello").await;
        assert!(read_frame(&mut c).await.starts_with("230"));
        assert!(read_frame(&mut c).await.starts_with("225-1"));
        assert!(read_frame(&mut c).await.starts_with("701-1"));
        assert!(read_frame(&mut c).await.starts_with("702-1"));
        assert_eq!(srv.spoken(), ["hello"]);
        assert_eq!(srv.verbs(), ["SPEAK 5"]);
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn stop_cancels_the_active_message() {
        let d = dir("v2");
        let srv = MockSpeechServer::start_voicego(&d).await.unwrap();
        srv.set_speak_time(Duration::from_secs(10));
        let mut c = UnixStream::connect(srv.socket_path()).await.unwrap();
        c.write_all(&[0xAA, b'V', b'G', 1]).await.unwrap();
        frame(&mut c, "SPEAK").await;
        frame(&mut c, "long").await;
        assert!(srv.wait_spoken(1, Duration::from_secs(2)).await);
        frame(&mut c, "STOP self").await;
        let mut seen = String::new();
        for _ in 0..5 {
            seen.push_str(&read_frame(&mut c).await);
            if seen.contains("703 CANCELED") {
                break;
            }
        }
        assert!(seen.contains("210"));
        assert!(seen.contains("703 CANCELED"), "{seen}");
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn lose_after_stop_cancels_the_next_speak_once() {
        let d = dir("v3");
        let srv = MockSpeechServer::start_voicego(&d).await.unwrap();
        srv.set_speak_time(Duration::from_secs(10));
        srv.set_lose_after_stop(true);
        let mut c = UnixStream::connect(srv.socket_path()).await.unwrap();
        c.write_all(&[0xAA, b'V', b'G', 1]).await.unwrap();
        frame(&mut c, "SPEAK").await;
        frame(&mut c, "a").await;
        assert!(srv.wait_spoken(1, Duration::from_secs(2)).await);
        frame(&mut c, "STOP self").await;
        frame(&mut c, "SPEAK").await;
        frame(&mut c, "b").await;
        frame(&mut c, "SPEAK").await;
        frame(&mut c, "c").await;
        assert!(srv.wait_spoken(2, Duration::from_secs(2)).await);
        assert_eq!(srv.spoken(), ["a", "c"], "b was lost");
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn ssip_mode_handles_data_blocks_and_dot_stuffing() {
        let d = dir("s1");
        let srv = MockSpeechServer::start_ssip(&d).await.unwrap();
        srv.set_speak_time(Duration::from_millis(5));
        let mut c = UnixStream::connect(srv.socket_path()).await.unwrap();
        c.write_all(b"SET self CLIENT_NAME a:b:c\r\nSPEAK\r\nline one\r\n..dot\r\n.\r\n")
            .await
            .unwrap();
        assert!(srv.wait_spoken(1, Duration::from_secs(2)).await);
        assert_eq!(srv.spoken(), ["line one\n.dot"]);
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn stop_and_restart_reuse_the_path() {
        let d = dir("s2");
        let mut srv = MockSpeechServer::start_ssip(&d).await.unwrap();
        let _c = UnixStream::connect(srv.socket_path()).await.unwrap();
        assert!(srv.wait_connections(1, Duration::from_secs(1)).await);
        srv.stop().await;
        assert!(UnixStream::connect(srv.socket_path()).await.is_err());
        srv.restart().unwrap();
        assert!(UnixStream::connect(srv.socket_path()).await.is_ok());
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn kill_connections_closes_clients() {
        let d = dir("s3");
        let srv = MockSpeechServer::start_ssip(&d).await.unwrap();
        let mut c = UnixStream::connect(srv.socket_path()).await.unwrap();
        assert!(srv.wait_connections(1, Duration::from_secs(1)).await);
        srv.kill_connections();
        let mut b = [0u8; 1];
        assert_eq!(c.read(&mut b).await.unwrap_or(0), 0);
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }
}
