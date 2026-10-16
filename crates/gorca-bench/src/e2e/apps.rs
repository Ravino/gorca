//! Child processes of a run: the Python fake apps / fake Mutter (JSON-lines control channel),
//! the reader under test, and the bus-floor focus tap.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use gorca_bench_b2::wire::{BusConn, Parse, SIGNAL, parse};

use super::mono::now_ns;

pub const PYTHON: &str = "/usr/bin/python3";

/// Extract `"key":"value"` from a flat JSON line.
pub fn json_str(line: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let rest = &line[line.find(&pat)? + pat.len()..];
    let rest = rest
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_owned())
}

/// Extract `"key":number` from a flat JSON line.
pub fn json_num(line: &str, key: &str) -> Option<f64> {
    let pat = format!("\"{key}\"");
    let rest = &line[line.find(&pat)? + pat.len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | 'e' | 'E' | '+')))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// A child speaking JSON lines on stdin/stdout (fakeapp.py, fake_mutter.py).
#[derive(Debug)]
pub struct JsonProc {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next_id: u64,
    pub ready: String,
}

impl JsonProc {
    pub fn spawn(script: &Path, args: &[String]) -> Result<Self> {
        let mut child = Command::new(PYTHON)
            .arg("-W")
            .arg("ignore::ResourceWarning")
            .arg(script)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("starting {}", script.display()))?;
        let stdin = child.stdin.take();
        let out = child.stdout.take().context("no stdout")?;
        let (tx, lines) = channel();
        std::thread::Builder::new()
            .name("hx-json-rd".to_owned())
            .spawn(move || {
                for l in BufReader::new(out).lines().map_while(Result::ok) {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
            })?;
        let mut p = Self {
            child,
            stdin,
            lines,
            next_id: 1,
            ready: String::new(),
        };
        let ready = p.wait_line(
            |l| l.contains("\"ready\"") || l.contains("\"error\""),
            Duration::from_secs(60),
        )?;
        if !ready.contains("\"ready\"") {
            bail!("{} failed to start: {ready}", script.display());
        }
        p.ready = ready;
        Ok(p)
    }

    pub fn wait_line(&mut self, pred: impl Fn(&str) -> bool, timeout: Duration) -> Result<String> {
        let end = Instant::now() + timeout;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left.max(Duration::from_millis(1))) {
                Ok(l) if pred(&l) => return Ok(l),
                Ok(_) => {}
                Err(_) => bail!("timed out waiting for a line from the child"),
            }
        }
    }

    /// Send `{"cmd":..., "id":N, <fields>}` and wait for the matching reply line.
    /// `fields` is the JSON object body without braces, e.g. `"node":7`.
    pub fn request(&mut self, cmd: &str, fields: &str, timeout: Duration) -> Result<String> {
        let id = self.next_id;
        self.next_id += 1;
        let sep = if fields.is_empty() { "" } else { "," };
        let stdin = self.stdin.as_mut().context("stdin closed")?;
        writeln!(stdin, "{{\"cmd\":\"{cmd}\",\"id\":{id}{sep}{fields}}}")?;
        stdin.flush()?;
        let needle = format!("\"id\":{id}");
        let line = self.wait_line(|l| l.contains("\"reply\"") && l.contains(&needle), timeout)?;
        if line.contains("\"ok\":false") {
            bail!("{cmd} failed: {line}");
        }
        Ok(line)
    }

    /// Fire a command without waiting for the reply (the reply is drained by later requests).
    pub fn send(&mut self, cmd: &str, fields: &str) -> Result<()> {
        let id = self.next_id;
        self.next_id += 1;
        let sep = if fields.is_empty() { "" } else { "," };
        let stdin = self.stdin.as_mut().context("stdin closed")?;
        writeln!(stdin, "{{\"cmd\":\"{cmd}\",\"id\":{id}{sep}{fields}}}")?;
        stdin.flush()?;
        Ok(())
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for JsonProc {
    fn drop(&mut self) {
        drop(self.stdin.take()); // EOF quits the app
        let end = Instant::now() + Duration::from_millis(500);
        while Instant::now() < end {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A fake AT-SPI app. `focus(i)` returns the emit timestamp reported by the app.
#[derive(Debug)]
pub struct FakeApp {
    pub proc: JsonProc,
    pub bus_name: String,
}

impl FakeApp {
    /// `scenario`: `tree` (healthy) or `hung`. `nodes` focusable list items `node-<i>`.
    pub fn spawn(
        dir: &Path,
        scenario: &str,
        name: &str,
        nodes: usize,
        extra: &[&str],
    ) -> Result<Self> {
        let mut args = vec![
            "--scenario".to_owned(),
            scenario.to_owned(),
            "--name".to_owned(),
            name.to_owned(),
            "--nodes".to_owned(),
            nodes.to_string(),
            "--fanout".to_owned(),
            "10".to_owned(),
        ];
        args.extend(extra.iter().map(|s| (*s).to_owned()));
        let proc = JsonProc::spawn(&dir.join("fakeapp.py"), &args)?;
        let bus_name = json_str(&proc.ready, "bus_name").context("no bus_name in ready line")?;
        Ok(Self { proc, bus_name })
    }

    /// Focus node `id`; returns `t_emit_ns` (0 when the app reported none).
    pub fn focus(&mut self, id: usize) -> Result<u64> {
        let r = self
            .proc
            .request("focus", &format!("\"node\":{id}"), Duration::from_secs(5))?;
        Ok(json_num(&r, "t_emit_ns").unwrap_or(0.0) as u64)
    }
}

/// The reader's grab state as the emulated manager holds it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GrabState {
    pub grab_all: bool,
    pub modifiers: u32,
    pub applied: u64,
    pub held: u32,
    /// Keys the emulated XKB state believes are down.
    pub xkb_down: u32,
    /// Lowest keycode among them (debugging).
    pub xkb_first: u32,
}

/// Mutter's verdict on one emulated key event (`fake_mutter.py` `key` reply).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct KeyReply {
    pub t_emit_ns: u64,
    /// Clients a `KeyEvent` was sent to.
    pub notified: u32,
    /// A client grabbed it: the app never sees it.
    pub consumed: bool,
}

impl KeyReply {
    pub fn parse(line: &str) -> Self {
        Self {
            t_emit_ns: json_num(line, "t_emit_ns").unwrap_or(0.0) as u64,
            notified: json_num(line, "notified").unwrap_or(0.0) as u32,
            consumed: line.contains("\"consumed\":true"),
        }
    }
}

/// Outcome of [`FakeMutter::hold`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Hold {
    /// Autorepeat events the timer produced.
    pub repeats: u32,
    /// After the release the key is still in Mutter's `grabbed_keypresses` (the 48.7 bug).
    pub stale: bool,
}

/// Parse `"keys":[[kc,sym,released],...]` of an `app_log` reply.
pub fn parse_app_log(line: &str) -> Vec<(u16, u32, bool)> {
    let Some(i) = line.find("\"keys\":[") else {
        return Vec::new();
    };
    let body = &line[i + 8..];
    let Some(end) = body.find("]]") else {
        return Vec::new(); // "[]" or malformed
    };
    body[1..end]
        .split("],[")
        .filter_map(|item| {
            let mut f = item.trim_matches(['[', ']']).split(',');
            let kc = f.next()?.parse::<u16>().ok()?;
            let sym = f.next()?.parse::<u32>().ok()?;
            let rel = f.next()? == "true";
            Some((kc, sym, rel))
        })
        .collect()
}

/// The emulated Mutter keyboard monitor on the private session bus.
#[derive(Debug)]
pub struct FakeMutter {
    pub proc: JsonProc,
}

impl FakeMutter {
    pub fn spawn(dir: &Path) -> Result<Self> {
        let proc = JsonProc::spawn(&dir.join("fake_mutter.py"), &[])?;
        Ok(Self { proc })
    }

    /// Emit one key press; returns (`t_emit_ns`, number of clients it was sent to).
    pub fn key(&mut self, keycode: u16, unichar: u32) -> Result<(u64, u32)> {
        let r = self.proc.request(
            "key",
            &format!("\"keycode\":{keycode},\"keysym\":{unichar},\"unichar\":{unichar}"),
            Duration::from_secs(5),
        )?;
        Ok((
            json_num(&r, "t_emit_ns").unwrap_or(0.0) as u64,
            json_num(&r, "sent").unwrap_or(0.0) as u32,
        ))
    }

    /// One key event with full control of state and event time (`time_ms` is what Mutter's
    /// first-click rule sees). The reply carries Mutter's verdict: was it notified to a client,
    /// consumed (never reaches the app) or passed to the app.
    pub fn key_ex(
        &mut self,
        keycode: u16,
        keysym: u32,
        state: u32,
        released: bool,
        time_ms: u64,
    ) -> Result<KeyReply> {
        self.key_full(keycode, keysym, 0, state, released, time_ms)
    }

    /// [`Self::key_ex`] with the Unicode scalar the key produces (typing).
    pub fn key_full(
        &mut self,
        keycode: u16,
        keysym: u32,
        unichar: u32,
        state: u32,
        released: bool,
        time_ms: u64,
    ) -> Result<KeyReply> {
        let r = self.proc.request(
            "key",
            &format!(
                "\"keycode\":{keycode},\"keysym\":{keysym},\"unichar\":{unichar},\"state\":{state},\"released\":{released},\"time_ms\":{time_ms}"
            ),
            Duration::from_secs(5),
        )?;
        Ok(KeyReply::parse(&r))
    }

    /// Hold a key down for `hold_ms` (virtual time): the press, Mutter's autorepeat timer (first
    /// repeat after the repeat delay, then every 30 ms) and the release. Mutter 48.7 runs the
    /// a11y double-tap rule for the repeats, which can leave a grabbed modifier stale.
    pub fn hold(&mut self, keycode: u16, keysym: u32, time_ms: u64, hold_ms: u64) -> Result<Hold> {
        let r = self.proc.request(
            "hold",
            &format!(
                "\"keycode\":{keycode},\"keysym\":{keysym},\"time_ms\":{time_ms},\"hold_ms\":{hold_ms}"
            ),
            Duration::from_secs(5),
        )?;
        Ok(Hold {
            repeats: json_num(&r, "repeats").unwrap_or(0.0) as u32,
            stale: r.contains("\"stale\":true"),
        })
    }

    /// Keys that reached the app so far: `(keycode, keysym, released)`.
    pub fn app_log(&mut self, clear: bool) -> Result<Vec<(u16, u32, bool)>> {
        let r = self.proc.request(
            "app_log",
            &format!("\"clear\":{clear}"),
            Duration::from_secs(20),
        )?;
        Ok(parse_app_log(&r))
    }

    /// Clients currently registered with the monitor (watching, grabbing or with a grab set).
    pub fn clients(&mut self) -> u32 {
        self.proc
            .request("stats", "", Duration::from_secs(2))
            .ok()
            .and_then(|r| json_num(&r, "grabbers"))
            .unwrap_or(0.0) as u32
    }

    /// Tell the emulated manager to apply grab calls after a random delay in `[0, max_ms]`
    /// (0 turns it off), so that concurrent calls can reorder.
    pub fn set_latency(&mut self, max_ms: u32, seed: u64) -> Result<()> {
        self.proc.request(
            "set_latency",
            &format!("\"max_ms\":{max_ms},\"seed\":{seed}"),
            Duration::from_secs(2),
        )?;
        Ok(())
    }

    /// The reader's grab state as Mutter sees it: `(grab_all, number of modifier grabs,
    /// grab calls applied so far, keys held as grabbed)`.
    pub fn grab_state(&mut self) -> Result<GrabState> {
        let r = self
            .proc
            .request("grab_state", "", Duration::from_secs(2))?;
        Ok(GrabState {
            grab_all: json_num(&r, "grab_all").unwrap_or(0.0) > 0.5,
            modifiers: json_num(&r, "n_modifiers").unwrap_or(0.0) as u32,
            applied: json_num(&r, "applied").unwrap_or(0.0) as u64,
            held: json_num(&r, "held").unwrap_or(0.0) as u32,
            xkb_down: json_num(&r, "xkb_down").unwrap_or(0.0) as u32,
            xkb_first: json_num(&r, "xkb_first").unwrap_or(0.0) as u32,
        })
    }

    /// Wait (polling, no fixed sleep) until the grab state satisfies `ok`, at most `timeout`.
    /// Returns the last state seen and whether it satisfied the predicate.
    pub fn wait_grabs(
        &mut self,
        timeout: Duration,
        ok: impl Fn(&GrabState) -> bool,
    ) -> Result<(GrabState, bool)> {
        let end = std::time::Instant::now() + timeout;
        loop {
            let st = self.grab_state()?;
            if ok(&st) {
                return Ok((st, true));
            }
            if std::time::Instant::now() > end {
                return Ok((st, false));
            }
            std::thread::sleep(Duration::from_micros(300));
        }
    }

    pub fn watchers(&mut self) -> u32 {
        self.proc
            .request("stats", "", Duration::from_secs(2))
            .ok()
            .and_then(|r| json_num(&r, "watchers"))
            .unwrap_or(0.0) as u32
    }
}

// ---- the reader under test -----------------------------------------------------------------------

/// How to start the reader.
#[derive(Clone, Debug)]
pub enum ReaderSpec {
    /// The in-tree stub (`gorca-stub-reader`, built next to this binary).
    Stub,
    /// `gorca-daemon` (or any binary honouring [`super::env`]): path and extra arguments.
    Daemon { path: PathBuf, args: Vec<String> },
}

impl ReaderSpec {
    pub fn label(&self) -> String {
        match self {
            Self::Stub => "stub".to_owned(),
            Self::Daemon { path, .. } => format!("daemon:{}", path.display()),
        }
    }
}

#[derive(Debug)]
pub struct Reader {
    child: Child,
    stdin: Option<ChildStdin>,
    pub pid: u32,
}

impl Reader {
    /// Spawn with the harness environment. `env` entries are added to the inherited env (which
    /// is already the private session environment under `run-isolated.sh`).
    pub fn spawn(spec: &ReaderSpec, env: &[(String, String)]) -> Result<Self> {
        Self::spawn_with(spec, env, &[], None)
    }

    /// [`Self::spawn`] with extra arguments for a daemon reader and, optionally, a file that
    /// receives its standard error (the password gate greps it).
    pub fn spawn_with(
        spec: &ReaderSpec,
        env: &[(String, String)],
        extra_args: &[String],
        stderr: Option<std::fs::File>,
    ) -> Result<Self> {
        let (program, mut args): (PathBuf, Vec<String>) = match spec {
            ReaderSpec::Stub => {
                let me = std::env::current_exe()?;
                let dir = me.parent().context("no exe dir")?;
                (dir.join("gorca-stub-reader"), Vec::new())
            }
            ReaderSpec::Daemon { path, args } => (path.clone(), args.clone()),
        };
        args.extend(extra_args.iter().cloned());
        let mut cmd = Command::new(&program);
        // GORCA_E2E_READER_LOG: append the reader's stderr to this file (the web scenarios read
        // the `gorca_phase` timing lines from it) instead of passing it through.
        let log_stdio = match std::env::var("GORCA_E2E_READER_LOG") {
            Ok(path) if !path.is_empty() => std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_or_else(|_| Stdio::inherit(), Stdio::from),
            Ok(_) | Err(_) => Stdio::inherit(),
        };
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(stderr.map_or(log_stdio, Stdio::from));
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("starting reader {}", program.display()))?;
        let stdin = child.stdin.take();
        let pid = child.id();
        Ok(Self { child, stdin, pid })
    }

    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Ask the reader to shut down (SIGTERM to the child this struct spawned) and wait up to
    /// `within` for it to exit, so its shutdown path prints its counters. `false` when it did
    /// not exit in time (it is then killed by `Drop`).
    pub fn stop(&mut self, within: Duration) -> bool {
        drop(self.stdin.take());
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(self.pid.to_string())
            .status();
        let end = Instant::now() + within;
        while Instant::now() < end {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// CPU time (user + system, all threads incl. exited ones counted by the kernel) in ns.
    pub fn cpu_ns(&self) -> u64 {
        proc_cpu_ns(self.pid)
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        drop(self.stdin.take()); // the stub exits on EOF
        let end = Instant::now() + Duration::from_millis(400);
        while Instant::now() < end {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // SIGTERM first so a real daemon can run its shutdown path, then SIGKILL.
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(self.pid.to_string())
            .status();
        let end = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < end {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Sum of `utime + stime` of a process from `/proc/<pid>/stat`, converted to ns (clock ticks
/// are 10 ms: use windows of several seconds), or the finer `schedstat` sum of its threads when
/// the kernel provides it.
pub fn proc_cpu_ns(pid: u32) -> u64 {
    let mut ns = 0u64;
    let mut any = false;
    if let Ok(dir) = std::fs::read_dir(format!("/proc/{pid}/task")) {
        for e in dir.flatten() {
            if let Some(v) = std::fs::read_to_string(e.path().join("schedstat"))
                .ok()
                .and_then(|s| {
                    s.split_whitespace()
                        .next()
                        .and_then(|x| x.parse::<u64>().ok())
                })
            {
                ns += v;
                any = true;
            }
        }
    }
    if any && ns > 0 {
        return ns;
    }
    let Ok(s) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return 0;
    };
    // Fields after the parenthesised comm: state is index 0; utime = 11, stime = 12.
    let Some(rest) = s.rsplit_once(')').map(|x| x.1) else {
        return 0;
    };
    let f: Vec<&str> = rest.split_whitespace().collect();
    let tick = |i: usize| f.get(i).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
    (tick(11) + tick(12)) * 10_000_000
}

// ---- bus floor ------------------------------------------------------------------------------------

/// An independent subscriber to `org.a11y.atspi.Event.Focus` on the a11y bus. It stamps every
/// focus signal on arrival, so `speak - tap` is the reader-side time and `tap - emit` the cost
/// of the app's send plus the bus hop (the harness floor).
#[derive(Debug)]
pub struct FocusTap {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// (sender, node id) -> first arrival `now_ns`.
    seen: Arc<Mutex<HashMap<(String, u64), u64>>>,
}

impl FocusTap {
    pub fn start(a11y_addr: &str) -> Result<Self> {
        let mut c = BusConn::connect(a11y_addr, Duration::from_secs(5))?;
        c.add_match(
            "type='signal',interface='org.a11y.atspi.Event.Focus'",
            Duration::from_secs(5),
        )?;
        c.stream.set_read_timeout(Some(Duration::from_millis(20)))?;
        let stop = Arc::new(AtomicBool::new(false));
        let seen: Arc<Mutex<HashMap<(String, u64), u64>>> = Arc::default();
        let (st, sn) = (Arc::clone(&stop), Arc::clone(&seen));
        let thread = std::thread::Builder::new()
            .name("hx-focus-tap".to_owned())
            .spawn(move || {
                let mut buf = std::mem::take(&mut c.leftover);
                let mut chunk = vec![0u8; 64 * 1024];
                loop {
                    let mut off = 0;
                    loop {
                        match parse(&buf[off..]) {
                            Parse::Msg(m, used) => {
                                let t = now_ns();
                                if m.mtype == SIGNAL
                                    && m.member == Some("Focus")
                                    && let (Some(s), Some(p)) = (m.sender, m.path)
                                    && let Some(id) =
                                        p.rsplit('/').next().and_then(|x| x.parse::<u64>().ok())
                                {
                                    sn.lock()
                                        .unwrap_or_else(PoisonError::into_inner)
                                        .entry((s.to_owned(), id))
                                        .or_insert(t);
                                }
                                off += used;
                            }
                            Parse::NeedMore => break,
                            Parse::Bad => return,
                        }
                    }
                    buf.drain(..off);
                    if st.load(Ordering::Acquire) {
                        return;
                    }
                    match c.stream.read(&mut chunk) {
                        Ok(0) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        Err(_) => return,
                    }
                }
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
            seen,
        })
    }

    /// Arrival time of the Focus signal of node `id` from `sender`, if seen.
    pub fn arrival(&self, sender: &str, id: usize) -> Option<u64> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(sender.to_owned(), id as u64))
            .copied()
    }

    pub fn clear(&self) {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

impl Drop for FocusTap {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
