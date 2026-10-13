//! The emulated Mutter keyboard manager (`tests/harness/fake_mutter.py`), as the B-1 walk uses it
//! to press the daemon's real `info.where_am_i` command (Insert+Tab of the NVDA desktop keymap,
//! ADR-0005) instead of calling an in-process shortcut.
//!
//! The e2e latency gates K2/K3 (`crates/gorca-bench/src/e2e/m1.rs`) drive the same script; this is
//! the small client the walk needs: spawn, wait until the daemon registered its grabs, press a
//! chord with virtual event times so Mutter's first-click rule is deterministic.
//!
//! The script refuses to start unless the session bus lives in a `/gorca-h.*` harness session
//! (`tests/harness/run-isolated.sh`), so it can never touch a live desktop.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The system python has the D-Bus bindings (the project venv does not).
const PYTHON: &str = "/usr/bin/python3";

/// A physical key: XKB keycode (evdev + 8) and the keysym it produces.
#[derive(Copy, Clone, Debug)]
pub struct Key {
    pub code: u16,
    pub sym: u32,
}

pub const INSERT: Key = Key {
    code: 118,
    sym: 0xff63,
};
pub const TAB: Key = Key {
    code: 23,
    sym: 0xff09,
};

/// Virtual gap between chords: above Mutter's default repeat delay (500 ms), so the modifier
/// press is always a fresh first click (swallowed, not passed to the application).
const CHORD_GAP_MS: u64 = 700;

#[derive(Debug, thiserror::Error)]
pub enum KeysError {
    #[error("fake mutter: {0}")]
    Msg(String),
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
}

type Result<T> = std::result::Result<T, KeysError>;

fn err<T>(m: impl Into<String>) -> Result<T> {
    Err(KeysError::Msg(m.into()))
}

/// Where `fake_mutter.py` lives in this repository.
pub fn default_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/harness/fake_mutter.py")
}

/// What Mutter did with a chord's key press.
#[derive(Copy, Clone, Debug)]
pub struct ChordReply {
    /// `CLOCK_MONOTONIC` ns just before the key event was sent to the reader.
    pub t_emit_ns: u64,
    /// The key never reached the application (the reader had grabbed it).
    pub consumed: bool,
}

#[derive(Debug)]
pub struct FakeMutter {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next_id: u64,
    t_ms: u64,
}

impl FakeMutter {
    pub fn spawn(script: &Path) -> Result<Self> {
        let mut child = Command::new(PYTHON)
            .arg("-W")
            .arg("ignore::ResourceWarning")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child.stdin.take();
        let Some(out) = child.stdout.take() else {
            return err("no stdout");
        };
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for l in BufReader::new(out)
                .lines()
                .map_while(std::result::Result::ok)
            {
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
        let mut m = Self {
            child,
            stdin,
            lines,
            next_id: 1,
            t_ms: 1_000_000,
        };
        let ready = m.wait_line(Duration::from_secs(60), |l| {
            l.contains("\"ready\"") || l.contains("\"error\"")
        })?;
        if !ready.contains("\"ready\"") {
            return err(format!("failed to start: {ready}"));
        }
        Ok(m)
    }

    fn wait_line(&mut self, timeout: Duration, pred: impl Fn(&str) -> bool) -> Result<String> {
        let end = Instant::now() + timeout;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left.max(Duration::from_millis(1))) {
                Ok(l) if pred(&l) => return Ok(l),
                Ok(_) => {}
                Err(_) => return err("timed out waiting for a line"),
            }
        }
    }

    fn request(&mut self, mut cmd: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        cmd["id"] = json!(id);
        let Some(stdin) = self.stdin.as_mut() else {
            return err("stdin closed");
        };
        writeln!(stdin, "{cmd}")?;
        stdin.flush()?;
        let line = self.wait_line(timeout, |l| {
            serde_json::from_str::<Value>(l).is_ok_and(|v| {
                v.get("event").and_then(Value::as_str) == Some("reply")
                    && v.get("id").and_then(Value::as_u64) == Some(id)
            })
        })?;
        let v: Value = serde_json::from_str(&line).map_err(|e| KeysError::Msg(e.to_string()))?;
        if v.get("ok").and_then(Value::as_bool) == Some(false) {
            return err(format!("request failed: {line}"));
        }
        Ok(v)
    }

    /// Wait until the reader holds a modifier grab (`SetKeyGrabs` applied): from then on a
    /// command chord is consumed by Mutter and delivered to the reader.
    #[allow(clippy::disallowed_methods)] // a driver process: real time must pass
    pub fn wait_grabs(&mut self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if let Ok(v) = self.request(json!({"cmd": "grab_state"}), Duration::from_secs(2))
                && v.get("n_modifiers").and_then(Value::as_u64).unwrap_or(0) > 0
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    fn key(&mut self, k: Key, released: bool, at_ms: u64) -> Result<ChordReply> {
        let v = self.request(
            json!({"cmd": "key", "keycode": k.code, "keysym": k.sym, "unichar": 0, "state": 0,
                   "released": released, "time_ms": at_ms}),
            Duration::from_secs(5),
        )?;
        Ok(ChordReply {
            t_emit_ns: v.get("t_emit_ns").and_then(Value::as_u64).unwrap_or(0),
            consumed: v.get("consumed").and_then(Value::as_bool).unwrap_or(false),
        })
    }

    /// `modifier` down, `key` down, `key` up, `modifier` up. The reply is the key press's.
    pub fn chord(&mut self, modifier: Key, key: Key) -> Result<ChordReply> {
        self.t_ms += CHORD_GAP_MS;
        let t = self.t_ms;
        self.key(modifier, false, t)?;
        let down = self.key(key, false, t + 5)?;
        self.key(key, true, t + 15)?;
        self.key(modifier, true, t + 25)?;
        self.t_ms += 25;
        Ok(down)
    }
}

impl Drop for FakeMutter {
    #[allow(clippy::disallowed_methods)] // teardown of a child this value started itself
    fn drop(&mut self) {
        drop(self.stdin.take()); // EOF quits the script
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
