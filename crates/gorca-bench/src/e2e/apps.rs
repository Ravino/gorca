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
        let (program, args): (PathBuf, Vec<String>) = match spec {
            ReaderSpec::Stub => {
                let me = std::env::current_exe()?;
                let dir = me.parent().context("no exe dir")?;
                (dir.join("gorca-stub-reader"), Vec::new())
            }
            ReaderSpec::Daemon { path, args } => (path.clone(), args.clone()),
        };
        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
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
