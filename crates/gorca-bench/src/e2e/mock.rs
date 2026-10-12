//! Mock speech servers: voicego (binary framing, `docs/platform/voicego-protocol.md`) and
//! speech-dispatcher (text SSIP). Both record a `CLOCK_MONOTONIC` timestamp the moment a read
//! returns data that contains a complete SPEAK or STOP/CANCEL.
//!
//! Behaviour modelled (voicego protocol doc, sections 2-6):
//! * binary framing is opt-in with the 4-byte prefix `AA 56 47 01` (voicego flavor only);
//!   `u32 BE length | payload` in both directions, one frame per reply or event
//! * `SPEAK [n]` -> `230`, then one data frame (binary) or text up to `\r\n.\r\n`, then
//!   `225-<id>` / `225 OK MESSAGE QUEUED`; events `701 BEGIN` after `begin_us`, `702 END` after
//!   `utter_ms` unless cancelled; queued messages start after the active one ends
//! * `STOP` / `CANCEL` -> `210` / `213`, `703 CANCELED` for the active and every queued message
//! * speech-dispatcher flavor: text only, `X-*` verbs rejected, events only after
//!   `SET self NOTIFICATION all on`
//!
//! The SPEAK timestamp is taken when the **data** is complete (that is when voicego starts
//! working on it); the time the verb line arrived is kept too (`t_verb_ns`).

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};

use super::mono::now_ns;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Flavor {
    /// voicego: binary framing accepted, `X-*` verbs, events always on.
    Voicego,
    /// speech-dispatcher: text only.
    Ssip,
}

impl Flavor {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "voicego" => Some(Self::Voicego),
            "ssip" => Some(Self::Ssip),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Voicego => "voicego",
            Self::Ssip => "ssip",
        }
    }
}

#[derive(Clone, Debug)]
pub enum Kind {
    Connected,
    Speak {
        text: String,
        /// When the `SPEAK` verb itself arrived.
        t_verb_ns: u64,
    },
    Cancel {
        verb: String,
        /// There was an active or queued message to cancel.
        had_speech: bool,
    },
}

#[derive(Clone, Debug)]
pub struct Rec {
    pub t_ns: u64,
    pub conn: u32,
    pub kind: Kind,
}

#[derive(Debug)]
struct Shared {
    flavor: Flavor,
    log: Mutex<Vec<Rec>>,
    cv: Condvar,
    begin_us: AtomicU64,
    utter_ms: AtomicU64,
    stop: AtomicBool,
    next_conn: AtomicU32,
    next_msg: AtomicU64,
    commands: AtomicU64,
    /// Connections currently open.
    open_conns: AtomicU32,
}

impl Shared {
    fn push(&self, rec: Rec) {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(rec);
        self.cv.notify_all();
    }
}

#[derive(Debug)]
pub struct MockServer {
    shared: Arc<Shared>,
    path: PathBuf,
    accept: Option<JoinHandle<()>>,
}

impl MockServer {
    /// Listen on `path` (removed first). Defaults: BEGIN after 2 ms, END after 10 ms.
    pub fn start(path: &Path, flavor: Flavor) -> Result<Self> {
        let _ = std::fs::remove_file(path);
        let listener =
            UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))?;
        listener.set_nonblocking(true)?;
        let shared = Arc::new(Shared {
            flavor,
            log: Mutex::new(Vec::new()),
            cv: Condvar::new(),
            begin_us: AtomicU64::new(2000),
            utter_ms: AtomicU64::new(10),
            stop: AtomicBool::new(false),
            next_conn: AtomicU32::new(1),
            next_msg: AtomicU64::new(1),
            commands: AtomicU64::new(0),
            open_conns: AtomicU32::new(0),
        });
        let sh = Arc::clone(&shared);
        let accept = std::thread::Builder::new()
            .name("hx-mock-acc".to_owned())
            .spawn(move || {
                let mut workers: Vec<JoinHandle<()>> = Vec::new();
                while !sh.stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((s, _)) => {
                            let id = sh.next_conn.fetch_add(1, Ordering::Relaxed);
                            let t = now_ns();
                            sh.open_conns.fetch_add(1, Ordering::Relaxed);
                            sh.push(Rec {
                                t_ns: t,
                                conn: id,
                                kind: Kind::Connected,
                            });
                            let sh2 = Arc::clone(&sh);
                            if let Ok(h) = std::thread::Builder::new()
                                .name(format!("hx-mock-c{id}"))
                                .spawn(move || {
                                    let _ = serve(&sh2, s, id);
                                    sh2.open_conns.fetch_sub(1, Ordering::Relaxed);
                                })
                            {
                                workers.push(h);
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
                for w in workers {
                    let _ = w.join();
                }
            })?;
        Ok(Self {
            shared,
            path: path.to_owned(),
            accept: Some(accept),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn flavor(&self) -> Flavor {
        self.shared.flavor
    }

    /// Simulated synthesis start (`701 BEGIN`) and utterance length (`702 END`).
    pub fn configure(&self, begin: Duration, utterance: Duration) {
        self.shared
            .begin_us
            .store(begin.as_micros() as u64, Ordering::Relaxed);
        self.shared
            .utter_ms
            .store(utterance.as_millis() as u64, Ordering::Relaxed);
    }

    /// Number of records so far; pass it to [`Self::since`] later.
    pub fn mark(&self) -> usize {
        self.shared
            .log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    pub fn since(&self, mark: usize) -> Vec<Rec> {
        let g = self
            .shared
            .log
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        g.get(mark..).unwrap_or_default().to_vec()
    }

    pub fn connections(&self) -> u32 {
        self.shared.open_conns.load(Ordering::Relaxed)
    }

    /// Commands received so far (any verb); a connected client that says nothing stays at 0.
    pub fn commands(&self) -> u64 {
        self.shared.commands.load(Ordering::Relaxed)
    }

    /// Block until a record after `mark` satisfies `pred`, or `timeout` passes.
    pub fn wait_for(
        &self,
        mark: usize,
        timeout: Duration,
        pred: impl Fn(&Rec) -> bool,
    ) -> Option<Rec> {
        let end = std::time::Instant::now() + timeout;
        let mut g = self
            .shared
            .log
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(r) = g.get(mark..).and_then(|s| s.iter().find(|r| pred(r))) {
                return Some(r.clone());
            }
            let left = end.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            g = self
                .shared
                .cv
                .wait_timeout(g, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(h) = self.accept.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

// ---- per-connection protocol ---------------------------------------------------------------------

#[derive(Debug)]
struct Utt {
    id: u64,
    /// Absolute `now_ns` at which BEGIN / END are due; set when it becomes active.
    begin_at: u64,
    end_at: u64,
    began: bool,
}

#[derive(Debug)]
enum Phase {
    Idle,
    /// After `230`: the next frame / text block is the SPEAK body.
    Data {
        t_verb_ns: u64,
    },
}

struct Conn<'a> {
    sh: &'a Shared,
    s: UnixStream,
    id: u32,
    binary: bool,
    notify: bool,
    phase: Phase,
    queue: VecDeque<Utt>,
    /// Cleaned text lines of a text-mode SPEAK body.
    text_body: Vec<String>,
}

impl Conn<'_> {
    fn send(&mut self, payload: &str) -> std::io::Result<()> {
        if self.binary {
            let mut v = Vec::with_capacity(payload.len() + 4);
            v.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            v.extend_from_slice(payload.as_bytes());
            self.s.write_all(&v)
        } else {
            self.s.write_all(payload.as_bytes())
        }
    }

    fn event(&mut self, code: u16, id: u64, word: &str) -> std::io::Result<()> {
        if self.sh.flavor == Flavor::Ssip && !self.notify {
            return Ok(());
        }
        self.send(&format!("{code}-{id}\r\n{code}-1\r\n{code} {word}\r\n"))
    }

    /// Fire due BEGIN / END timers. Returns the delay until the next timer.
    fn timers(&mut self) -> std::io::Result<Option<u64>> {
        let now = now_ns();
        loop {
            let Some(front) = self.queue.front_mut() else {
                return Ok(None);
            };
            if !front.began {
                if now < front.begin_at {
                    return Ok(Some(front.begin_at - now));
                }
                front.began = true;
                let id = front.id;
                self.event(701, id, "BEGIN")?;
                continue;
            }
            if now < front.end_at {
                return Ok(Some(front.end_at - now));
            }
            let id = front.id;
            self.queue.pop_front();
            self.event(702, id, "END")?;
            self.activate_front();
        }
    }

    fn activate_front(&mut self) {
        let now = now_ns();
        let begin = self.sh.begin_us.load(Ordering::Relaxed) * 1000;
        let utter = self.sh.utter_ms.load(Ordering::Relaxed) * 1_000_000;
        if let Some(f) = self.queue.front_mut()
            && !f.began
            && f.begin_at == 0
        {
            f.begin_at = now + begin;
            f.end_at = now + begin + utter;
        }
    }

    fn speak(&mut self, text: String, t_data: u64, t_verb: u64) -> std::io::Result<()> {
        let id = self.sh.next_msg.fetch_add(1, Ordering::Relaxed);
        self.sh.push(Rec {
            t_ns: t_data,
            conn: self.id,
            kind: Kind::Speak {
                text,
                t_verb_ns: t_verb,
            },
        });
        self.queue.push_back(Utt {
            id,
            begin_at: 0,
            end_at: 0,
            began: false,
        });
        self.send(&format!("225-{id}\r\n225 OK MESSAGE QUEUED\r\n"))?;
        if self.queue.len() == 1 {
            self.activate_front();
        }
        Ok(())
    }

    fn cancel(&mut self, verb: &str, t: u64) -> std::io::Result<()> {
        let had = !self.queue.is_empty();
        self.sh.push(Rec {
            t_ns: t,
            conn: self.id,
            kind: Kind::Cancel {
                verb: verb.to_owned(),
                had_speech: had,
            },
        });
        let reply = if verb == "STOP" {
            "210 OK STOPPED\r\n"
        } else {
            "213 OK CANCELED\r\n"
        };
        self.send(reply)?;
        while let Some(u) = self.queue.pop_front() {
            self.event(703, u.id, "CANCELED")?;
        }
        Ok(())
    }

    fn command(&mut self, line: &str, t: u64) -> std::io::Result<()> {
        self.sh.commands.fetch_add(1, Ordering::Relaxed);
        let mut it = line.split_whitespace();
        let verb = it.next().unwrap_or("").to_ascii_uppercase();
        let rest: Vec<&str> = it.collect();
        let voicego = self.sh.flavor == Flavor::Voicego;
        match verb.as_str() {
            "SPEAK" => {
                self.phase = Phase::Data { t_verb_ns: t };
                self.text_body.clear();
                self.send("230 OK RECEIVING DATA\r\n")
            }
            "CHAR" | "KEY" | "SOUND_ICON" => self.speak(rest.join(" "), t, t),
            "STOP" | "CANCEL" => self.cancel(&verb, t),
            "PAUSE" => self.send("211 OK PAUSED\r\n"),
            "RESUME" => self.send("212 OK RESUMED\r\n"),
            "BLOCK" => self.send(
                if rest.first().is_some_and(|w| w.eq_ignore_ascii_case("END")) {
                    "261 OK OUTPUT BLOCK CLOSED\r\n"
                } else {
                    "260 OK OUTPUT BLOCK OPENED\r\n"
                },
            ),
            "SET" => {
                let param = rest
                    .get(1)
                    .map_or(String::new(), |p| p.to_ascii_uppercase());
                if param == "NOTIFICATION" {
                    self.notify = rest.last().is_some_and(|v| v.eq_ignore_ascii_case("on"));
                }
                let code = match param.as_str() {
                    "CLIENT_NAME" => "208 OK CLIENT NAME SET",
                    "PRIORITY" => "202 OK PRIORITY SET",
                    "OUTPUT_MODULE" => "216 OK OUTPUT MODULE SET",
                    "LANGUAGE" => "201 OK LANGUAGE SET",
                    "VOICE" | "SYNTHESIS_VOICE" | "VOICE_TYPE" => "209 OK VOICE SET",
                    "RATE" => "203 OK RATE SET",
                    "PITCH" => "204 OK PITCH SET",
                    "PITCH_RANGE" => "263 OK PITCH RANGE SET",
                    "VOLUME" => "218 OK VOLUME SET",
                    "PUNCTUATION" => "205 OK PUNCTUATION SET",
                    "SPELLING" => "207 OK SPELLING SET",
                    "CAP_LET_RECOGN" => "206 OK CAP LET RECOGNITION SET",
                    "SSML_MODE" => "219 OK SSML MODE SET",
                    "NOTIFICATION" | "DEBUG" | "PAUSE_CONTEXT" | "HISTORY" => {
                        "220 OK NOTIFICATION SET"
                    }
                    _ => "410 ERR INVALID PARAM",
                };
                self.send(&format!("{code}\r\n"))
            }
            "GET" => self.send("251-0\r\n251 OK GET RETURNED\r\n"),
            "LIST" => self.send("250-espeak-ng\r\n250 OK MODULE LIST SENT\r\n"),
            "HISTORY" => self.send("245-1\r\n245 OK CLIENT ID SENT\r\n"),
            "HELLO" => self.send("200 OK HELLO\r\n"),
            "HELP" => self.send("248-HELP\r\n248 OK HELP SENT\r\n"),
            "QUIT" | "BYE" => {
                self.send("231 HAPPY HACKING\r\n")?;
                Err(std::io::ErrorKind::ConnectionAborted.into())
            }
            "X-PING" if voicego => self.send("280 PONG\r\n"),
            "X-CAPABILITIES" if voicego => {
                self.send("281-version: mock\r\n281 OK CAPABILITIES\r\n")
            }
            _ if voicego => self.send("420 ERR UNKNOWN COMMAND\r\n"),
            _ => self.send("500 ERR COMMAND ERROR\r\n"),
        }
    }

    fn data(&mut self, body: &[u8], t: u64) -> std::io::Result<()> {
        let t_verb = match self.phase {
            Phase::Data { t_verb_ns } => t_verb_ns,
            Phase::Idle => t,
        };
        self.phase = Phase::Idle;
        self.speak(String::from_utf8_lossy(body).into_owned(), t, t_verb)
    }

    /// Consume as much of `buf` as forms complete frames / lines.
    fn parse(&mut self, buf: &mut Vec<u8>, t: u64) -> std::io::Result<()> {
        loop {
            if self.binary {
                if buf.len() < 4 {
                    return Ok(());
                }
                let n = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
                if n > 16 * 1024 * 1024 {
                    return Err(std::io::ErrorKind::InvalidData.into());
                }
                if buf.len() < 4 + n {
                    return Ok(());
                }
                let payload: Vec<u8> = buf[4..4 + n].to_vec();
                buf.drain(..4 + n);
                if matches!(self.phase, Phase::Data { .. }) {
                    self.data(&payload, t)?;
                } else {
                    let line = String::from_utf8_lossy(&payload).into_owned();
                    self.command(&line, t)?;
                }
            } else {
                let Some(nl) = buf.iter().position(|&b| b == b'\n') else {
                    return Ok(());
                };
                let raw: Vec<u8> = buf.drain(..=nl).collect();
                let line = String::from_utf8_lossy(&raw)
                    .trim_end_matches(['\r', '\n'])
                    .to_owned();
                if matches!(self.phase, Phase::Data { .. }) {
                    if line == "." {
                        let body = std::mem::take(&mut self.text_body).join("\n");
                        self.data(body.as_bytes(), t)?;
                    } else {
                        let l = line.strip_prefix('.').map_or(line.clone(), str::to_owned);
                        self.text_body.push(l);
                    }
                } else if !line.is_empty() {
                    self.command(&line, t)?;
                }
            }
        }
    }
}

fn serve(sh: &Shared, s: UnixStream, id: u32) -> std::io::Result<()> {
    let mut conn = Conn {
        sh,
        s,
        id,
        binary: false,
        notify: false,
        phase: Phase::Idle,
        queue: VecDeque::new(),
        text_body: Vec::new(),
    };
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = vec![0u8; 64 * 1024];
    let mut detected = false;
    while !sh.stop.load(Ordering::Acquire) {
        let wait = conn.timers()?.map_or(100_000_000, |ns| ns.min(100_000_000));
        conn.s
            .set_read_timeout(Some(Duration::from_nanos(wait.max(50_000))))?;
        match conn.s.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                let t = now_ns();
                buf.extend_from_slice(&chunk[..n]);
                if !detected {
                    if buf.len() < 4 && buf.first() == Some(&0xAA) {
                        continue;
                    }
                    detected = true;
                    if sh.flavor == Flavor::Voicego && buf.starts_with(&[0xAA, b'V', b'G', 1]) {
                        conn.binary = true;
                        buf.drain(..4);
                    }
                }
                conn.parse(&mut buf, t)?;
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    fn sock(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("gorca-mock-{}-{name}.sock", std::process::id()))
    }

    fn read_frame(s: &mut UnixStream) -> String {
        let mut l = [0u8; 4];
        s.read_exact(&mut l).unwrap();
        let mut p = vec![0u8; u32::from_be_bytes(l) as usize];
        s.read_exact(&mut p).unwrap();
        String::from_utf8(p).unwrap()
    }

    fn frame(s: &mut UnixStream, p: &str) {
        let mut v = (p.len() as u32).to_be_bytes().to_vec();
        v.extend_from_slice(p.as_bytes());
        s.write_all(&v).unwrap();
    }

    #[test]
    fn voicego_binary_speak_then_stop() {
        let m = MockServer::start(&sock("vb"), Flavor::Voicego).unwrap();
        m.configure(Duration::from_millis(1), Duration::from_secs(5));
        let mut s = UnixStream::connect(m.path()).unwrap();
        s.write_all(&[0xAA, b'V', b'G', 1]).unwrap();
        frame(&mut s, "SET self CLIENT_NAME gorca:t:t");
        assert!(read_frame(&mut s).starts_with("208"));
        frame(&mut s, "SPEAK 5");
        assert!(read_frame(&mut s).starts_with("230"));
        frame(&mut s, "hello");
        assert!(read_frame(&mut s).starts_with("225-"));
        assert!(read_frame(&mut s).contains("701 BEGIN"));
        frame(&mut s, "STOP");
        let mut seen = String::new();
        for _ in 0..2 {
            seen.push_str(&read_frame(&mut s));
        }
        assert!(
            seen.contains("210") && seen.contains("703 CANCELED"),
            "{seen}"
        );
        let recs = m.since(0);
        let speak = recs
            .iter()
            .find(|r| matches!(&r.kind, Kind::Speak { text, .. } if text == "hello"));
        assert!(speak.is_some(), "{recs:?}");
        let cancel = recs.iter().find(|r| {
            matches!(
                &r.kind,
                Kind::Cancel {
                    had_speech: true,
                    ..
                }
            )
        });
        assert!(cancel.is_some_and(|c| c.t_ns >= speak.map_or(u64::MAX, |s| s.t_ns)));
    }

    #[test]
    fn ssip_text_dot_framing_and_notification_is_opt_in() {
        let m = MockServer::start(&sock("st"), Flavor::Ssip).unwrap();
        m.configure(Duration::from_millis(1), Duration::from_millis(5));
        let s = UnixStream::connect(m.path()).unwrap();
        let mut w = s.try_clone().unwrap();
        let mut r = BufReader::new(s);
        let mut line = String::new();
        w.write_all(b"SPEAK\r\n").unwrap();
        r.read_line(&mut line).unwrap();
        assert!(line.starts_with("230"));
        w.write_all(b"line one\r\n..dot\r\n.\r\n").unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        assert!(line.starts_with("225-"));
        line.clear();
        r.read_line(&mut line).unwrap();
        assert!(line.starts_with("225 "));
        std::thread::sleep(Duration::from_millis(30));
        w.write_all(b"X-PING\r\n").unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        assert!(
            line.starts_with("500"),
            "no events without NOTIFICATION, got {line}"
        );
        let recs = m.since(0);
        assert!(
            recs.iter()
                .any(|r| matches!(&r.kind, Kind::Speak { text, .. } if text == "line one\n.dot"))
        );
    }
}
