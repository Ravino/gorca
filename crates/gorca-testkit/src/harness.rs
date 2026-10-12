//! Rust side of `tests/harness`: an isolated session (private session bus, a11y bus, registry
//! daemon) and scriptable fake AT-SPI applications, for tests that need a real bus.
//!
//! Nothing here touches the user's session: `run-isolated.sh` builds private buses under a
//! temporary directory and tears everything down when the [`Session`] is dropped (EOF on its
//! stdin). When the prerequisites are missing, [`Session::unavailable_reason`] says why, so a
//! test can skip loudly instead of failing on a machine without AT-SPI.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

const PYTHON: &str = "/usr/bin/python3";

/// `tests/harness` of this repository.
pub fn harness_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/harness")
}

#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
}

/// A private AT-SPI session. Dropping it tears it down.
#[derive(Debug)]
pub struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    env: HashMap<String, String>,
}

impl Session {
    /// `Some(reason)` when this machine cannot run the harness (missing binaries or Python
    /// modules). Tests print the reason and return, so `make check` stays green on a bare box
    /// while a developer machine runs them for real.
    pub fn unavailable_reason() -> Option<String> {
        let script = harness_dir().join("run-isolated.sh");
        if !script.exists() {
            return Some(format!("{} not found", script.display()));
        }
        for bin in [
            "/usr/libexec/at-spi-bus-launcher",
            "/usr/libexec/at-spi2-registryd",
            PYTHON,
        ] {
            if !Path::new(bin).exists() {
                return Some(format!("{bin} not installed"));
            }
        }
        let py = Command::new(PYTHON)
            .args(["-c", "import dbus, gi.repository.GLib"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !py.is_ok_and(|s| s.success()) {
            return Some("python3-dbus / PyGObject missing".to_owned());
        }
        let bus = Command::new("dbus-daemon")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !bus.is_ok_and(|s| s.success()) {
            return Some("dbus-daemon missing".to_owned());
        }
        None
    }

    /// Start a session and wait until it is ready (about 1.5 s).
    pub fn start() -> Result<Self, HarnessError> {
        let script = harness_dir().join("run-isolated.sh");
        let mut child = Command::new(script)
            .arg("--hold")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HarnessError::Protocol("no stdout".to_owned()))?;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let done = line == "READY";
                if tx.send(line).is_err() || done {
                    break;
                }
            }
        });
        let mut env = HashMap::new();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(line) if line == "READY" => break,
                Ok(line) => {
                    if let Some((k, v)) = line.split_once('=') {
                        env.insert(k.to_owned(), v.to_owned());
                    }
                }
                Err(RecvTimeoutError::Timeout) => return Err(HarnessError::Timeout("READY")),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(HarnessError::Protocol(
                        "session ended before READY".to_owned(),
                    ));
                }
            }
        }
        Ok(Self { child, stdin, env })
    }

    pub fn env(&self) -> &HashMap<String, String> {
        &self.env
    }

    pub fn a11y_address(&self) -> &str {
        self.env
            .get("AT_SPI_BUS_ADDRESS")
            .map_or("", String::as_str)
    }

    pub fn session_address(&self) -> &str {
        self.env
            .get("DBUS_SESSION_BUS_ADDRESS")
            .map_or("", String::as_str)
    }

    /// The temporary directory of the session (sockets for mock servers can live here).
    pub fn dir(&self) -> PathBuf {
        PathBuf::from(self.env.get("GORCA_HARNESS_DIR").map_or("", String::as_str))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}

/// A fake AT-SPI application (`tests/harness/fakeapp.py`).
#[derive(Debug)]
pub struct FakeApp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    ready: String,
    next_id: u64,
}

/// Value of `"key":"..."` in a flat JSON line (the fake app prints compact JSON).
fn json_str<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\":\"");
    let start = line.find(&pat)? + pat.len();
    let end = line[start..].find('"')? + start;
    Some(&line[start..end])
}

impl FakeApp {
    /// Spawn `fakeapp.py` with `args` inside `session` and wait for its `ready` line.
    pub fn start(session: &Session, args: &[&str]) -> Result<Self, HarnessError> {
        Self::start_script(session, "fakeapp.py", args)
    }

    /// Spawn `fake_mutter.py`: Mutter's `org.freedesktop.a11y.Manager` on the private session
    /// bus. Drive it with [`FakeApp::key`].
    pub fn start_mutter(session: &Session) -> Result<Self, HarnessError> {
        Self::start_script(session, "fake_mutter.py", &[])
    }

    fn start_script(session: &Session, script: &str, args: &[&str]) -> Result<Self, HarnessError> {
        let mut cmd = Command::new(PYTHON);
        cmd.arg(harness_dir().join(script)).args(args);
        cmd.env_clear();
        for (k, v) in session.env() {
            cmd.env(k, v);
        }
        if let Some(p) = std::env::var_os("PATH") {
            cmd.env("PATH", p);
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
            .ok_or_else(|| HarnessError::Protocol("no stdout".to_owned()))?;
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
            ready: String::new(),
            next_id: 1,
        };
        app.ready = app.wait_line(Duration::from_secs(60), "ready", |l| {
            l.contains("\"event\":\"ready\"") || l.contains("\"event\":\"error\"")
        })?;
        if app.ready.contains("\"event\":\"error\"") {
            return Err(HarnessError::Protocol(app.ready.clone()));
        }
        Ok(app)
    }

    fn wait_line(
        &self,
        timeout: Duration,
        what: &'static str,
        pred: impl Fn(&str) -> bool,
    ) -> Result<String, HarnessError> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(l) if pred(&l) => return Ok(l),
                Ok(_) => {}
                Err(_) => return Err(HarnessError::Timeout(what)),
            }
        }
    }

    /// The app's unique bus name.
    pub fn bus_name(&self) -> Option<&str> {
        json_str(&self.ready, "bus_name")
    }

    /// Send one control command (`body` is the JSON object body without braces and without an
    /// id, for example `"cmd":"focus","node":"email"`) and wait for its reply.
    pub fn cmd(&mut self, body: &str) -> Result<String, HarnessError> {
        let id = self.next_id;
        self.next_id += 1;
        let line = format!("{{{body},\"id\":{id}}}\n");
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| HarnessError::Protocol("stdin closed".to_owned()))?;
        stdin.write_all(line.as_bytes())?;
        stdin.flush()?;
        let want = format!("\"id\":{id}");
        let reply = self.wait_line(Duration::from_secs(30), "reply", |l| {
            l.contains("\"event\":\"reply\"") && l.contains(&want)
        })?;
        if reply.contains("\"ok\":false") {
            return Err(HarnessError::Protocol(reply));
        }
        Ok(reply)
    }

    /// Move focus to the named node (state-changed:focused plus a `Focus:Focus` event).
    pub fn focus(&mut self, node: &str) -> Result<(), HarnessError> {
        self.cmd(&format!("\"cmd\":\"focus\",\"node\":\"{node}\""))
            .map(|_| ())
    }

    /// Send a key press (and release) from the emulated Mutter.
    pub fn key(&mut self, keycode: u32, keysym: u32, unichar: u32) -> Result<(), HarnessError> {
        for released in ["false", "true"] {
            self.cmd(&format!(
                "\"cmd\":\"key\",\"keycode\":{keycode},\"keysym\":{keysym},\"unichar\":{unichar},\"state\":0,\"released\":{released}"
            ))?;
        }
        Ok(())
    }

    /// Change the hang policy: `mode` is `never`, `delay` or `off`.
    pub fn hang(&mut self, mode: &str, ms: u64) -> Result<(), HarnessError> {
        self.cmd(&format!("\"cmd\":\"hang\",\"mode\":\"{mode}\",\"ms\":{ms}"))
            .map(|_| ())
    }
}

impl Drop for FakeApp {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_str_reads_flat_values() {
        let l = r#"{"event":"ready","bus_name":":1.7","root":"/x"}"#;
        assert_eq!(json_str(l, "bus_name"), Some(":1.7"));
        assert_eq!(json_str(l, "root"), Some("/x"));
        assert_eq!(json_str(l, "nope"), None);
    }

    #[test]
    fn harness_dir_points_at_the_scripts() {
        assert!(harness_dir().join("fakeapp.py").exists());
    }
}
