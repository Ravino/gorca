//! Driving the `b1-fakeapp` binary: spawn it inside an isolated bus, send [`Cmd`]s, read the
//! truth log. The binary's path is the caller's business (`env!("CARGO_BIN_EXE_b1-fakeapp")` in
//! the fake app's own tests; the runner locates it next to its own executable).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use super::proto::{AppStats, Cmd, CmdReply, TruthLine};

#[derive(Debug, thiserror::Error)]
pub enum AppCtlError {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
}

/// What to start.
#[derive(Clone, Debug)]
pub struct AppArgs {
    /// Model name or path (`settings`, `tests/harness/b1/models/x.json`).
    pub model: String,
    pub label: String,
    /// Address of the private a11y bus.
    pub bus: String,
    pub register: bool,
    pub seed: Option<u64>,
    /// `peer`, `same_bus`, `none`.
    pub p2p: Option<String>,
    /// `pessimistic` or `none`.
    pub quirks: Option<String>,
    /// Test buses that do not live under a `/gorca-h.` session directory.
    pub allow_any_bus: bool,
    pub dir: Option<PathBuf>,
    /// Own `own_name` on this session bus (the fake gnome-shell, IC-22).
    pub session_bus: Option<String>,
    pub own_name: Option<String>,
}

impl AppArgs {
    pub fn new(model: &str, label: &str, bus: &str) -> Self {
        Self {
            model: model.to_owned(),
            label: label.to_owned(),
            bus: bus.to_owned(),
            register: true,
            seed: None,
            p2p: None,
            quirks: None,
            allow_any_bus: false,
            dir: None,
            session_bus: None,
            own_name: None,
        }
    }
}

/// A running `b1-fakeapp` process.
#[derive(Debug)]
pub struct B1App {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next_id: u64,
    pub ready: serde_json::Value,
    pub truth_path: PathBuf,
}

impl B1App {
    pub fn spawn(bin: &Path, a: &AppArgs, truth_dir: &Path) -> Result<Self, AppCtlError> {
        let truth_path = truth_dir.join(format!("{}.truth.jsonl", a.label));
        let mut cmd = Command::new(bin);
        cmd.args(["--model", &a.model, "--label", &a.label, "--bus", &a.bus, "--truth"]);
        cmd.arg(&truth_path);
        if !a.register {
            cmd.arg("--no-register");
        }
        if let Some(s) = a.seed {
            cmd.args(["--seed", &s.to_string()]);
        }
        if let Some(p) = &a.p2p {
            cmd.args(["--p2p", p]);
        }
        if let Some(q) = &a.quirks {
            cmd.args(["--quirks", q]);
        }
        if a.allow_any_bus {
            cmd.arg("--allow-any-bus");
        }
        if let (Some(sb), Some(n)) = (&a.session_bus, &a.own_name) {
            cmd.args(["--session-bus", sb, "--own-name", n]);
        }
        if let Some(d) = &a.dir {
            cmd.arg("--dir").arg(d);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppCtlError::Protocol("no stdout".to_owned()))?;
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut app = Self {
            child,
            stdin,
            lines,
            next_id: 1,
            ready: serde_json::Value::Null,
            truth_path,
        };
        let line = app.wait_line(Duration::from_secs(30), "ready", |l| l.contains("\"event\":\"ready\""))?;
        app.ready =
            serde_json::from_str(&line).map_err(|e| AppCtlError::Protocol(format!("ready: {e}")))?;
        Ok(app)
    }

    fn wait_line(
        &self,
        timeout: Duration,
        what: &'static str,
        pred: impl Fn(&str) -> bool,
    ) -> Result<String, AppCtlError> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(l) if pred(&l) => return Ok(l),
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => return Err(AppCtlError::Timeout(what)),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(AppCtlError::Protocol("process ended".to_owned()));
                }
            }
        }
    }

    pub fn bus_name(&self) -> &str {
        self.ready.get("bus_name").and_then(|v| v.as_str()).unwrap_or("")
    }

    pub fn root(&self) -> &str {
        self.ready.get("root").and_then(|v| v.as_str()).unwrap_or("")
    }

    /// Send a command and wait for its answer (`timeout` covers a busy or hung app).
    pub fn cmd_timeout(&mut self, c: &Cmd, timeout: Duration) -> Result<CmdReply, AppCtlError> {
        let id = self.next_id;
        self.next_id += 1;
        let mut v = serde_json::to_value(c).map_err(|e| AppCtlError::Protocol(e.to_string()))?;
        if let Some(o) = v.as_object_mut() {
            o.insert("id".to_owned(), id.into());
        }
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| AppCtlError::Protocol("stdin closed".to_owned()))?;
        writeln!(stdin, "{v}")?;
        stdin.flush()?;
        let want = format!("\"id\":{id}");
        let line = self.wait_line(timeout, "reply", |l| {
            l.contains("\"event\":\"reply\"") && l.contains(&want)
        })?;
        let j: serde_json::Value =
            serde_json::from_str(&line).map_err(|e| AppCtlError::Protocol(e.to_string()))?;
        Ok(CmdReply {
            ok: j.get("ok").and_then(serde_json::Value::as_bool).unwrap_or(false),
            error: j.get("error").and_then(|e| e.as_str()).map(str::to_owned),
            data: j.get("data").cloned().unwrap_or_default(),
        })
    }

    pub fn cmd(&mut self, c: &Cmd) -> Result<CmdReply, AppCtlError> {
        self.cmd_timeout(c, Duration::from_secs(30))
    }

    pub fn stats(&mut self) -> Result<AppStats, AppCtlError> {
        let r = self.cmd(&Cmd::Stats)?;
        serde_json::from_value(r.data).map_err(|e| AppCtlError::Protocol(e.to_string()))
    }

    /// The truth log written so far.
    pub fn truth(&self) -> Vec<TruthLine> {
        read_truth(&self.truth_path)
    }

    /// Kill without a goodbye (a crash, IC-6) and reap.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Parse a truth log file; unparsable lines are skipped.
pub fn read_truth(path: &Path) -> Vec<TruthLine> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

impl Drop for B1App {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    #[allow(clippy::disallowed_methods)]
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return;
                }
            }
        }
    }
}
