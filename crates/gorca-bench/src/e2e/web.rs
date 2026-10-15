//! M7a end-to-end gates and walkthrough (browse-mode design 15, 16): gorca (the real daemon)
//! reading a PRIVATE Firefox, with the emulated Mutter delivering keys and the mock speech server
//! scoring the answers.
//!
//! | id | stimulus | metrics |
//! |---|---|---|
//! | B0 | a scripted walk over `article.html`: focus the document, lines, headings, links, a form field, Escape, Enter on a link (a page load) | `web_failures` (gate 0); the transcript is in the notes |
//! | W1 | alternating `H` / `Shift+H` on a 10k-node page | `w1_ms` key-down emit to SPEAK (gate p99 5 ms), `lost`, `not_consumed` |
//! | BR1 | `Down` / `Up`, then `Right` / `Left`, then `Ctrl+Right` / `Ctrl+Left` | `br1_ms`, `br1c_ms`, `br1w_ms` (gate p99 5 ms each), `lost` |
//! | BR2 | held `Down` at 30 Hz for 3 s | `stale` (gate 0: a line spoken after the next key), `last_ok` (gate 1: the final line is spoken) |
//! | W2b | focus enters a loaded, never visited 10k-node document | `w2b_ms` focus emit to the first line spoken (wall; gate 150 ms) |
//! | W2 | a link click loads a 10k-node page | `w2_ms` load-complete event to speech (wall, informational: cold start is reported, not gated) |
//! | W3 | tab switch back to a document that slept | `w3_ms` focus event arrival to speech (gate p99 15 ms) |
//! | FM1 | focus lands on a text field | `fm1_ms` focus event arrival to "focus mode" speech (gate p99 15 ms) |
//! | FM2 | Escape in auto focus mode | `fm2_ms` key emit to "browse mode" speech (gate p99 5 ms) |
//!
//! Everything is skipped unless `--reader-env GORCA_M7=1` (and `GORCA_M7_WAYLAND` names the private
//! compositor socket, set by `tests/web/m7/run_m7.sh`); `GORCA_M7_STRICT=1` fails instead of
//! skipping. Firefox is the harness's own child in its own process group with a throw-away
//! profile; teardown signals only that group.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use gorca_bench_b2::wire::{BusConn, Parse, SIGNAL, parse};

use super::apps::{JsonProc, PYTHON, json_num, json_str};
use super::m1::{K, Typist, flag, m1_env};
use super::mock::{Flavor, Kind, Rec};
use super::mono::now_ns;
use super::runner::{HostEnv, Rig, RigOpts};
use super::scenarios::{RunOut, Size, ms};

pub(super) const DOWN: K = K {
    code: 116,
    sym: 0xff54,
};
pub(super) const UP: K = K {
    code: 111,
    sym: 0xff52,
};
pub(super) const LEFT: K = K {
    code: 113,
    sym: 0xff51,
};
pub(super) const RIGHT: K = K {
    code: 114,
    sym: 0xff53,
};
pub(super) const ENTER: K = K {
    code: 36,
    sym: 0xff0d,
};
pub(super) const ESC: K = K {
    code: 9,
    sym: 0xff1b,
};
pub(super) const KEY_H: K = K {
    code: 43,
    sym: 0x68,
};
pub(super) const KEY_H_CAP: K = K {
    code: 43,
    sym: 0x48,
};
pub(super) const KEY_K: K = K {
    code: 45,
    sym: 0x6b,
};
pub(super) const KEY_2: K = K {
    code: 11,
    sym: 0x32,
};
pub(super) const CTRL: u32 = 0x04;
pub(super) const SHIFT: u32 = 0x01;

pub(super) fn gate_open(host: &HostEnv, id: &str, out: &mut RunOut) -> Result<bool> {
    let ready = flag(host, "GORCA_M7") && std::env::var_os("GORCA_M7_WAYLAND").is_some();
    if ready {
        return Ok(true);
    }
    if flag(host, "GORCA_M7_STRICT") {
        bail!(
            "{id}: GORCA_M7_STRICT=1 requires GORCA_M7=1 and the private compositor (tests/web/m7/run_m7.sh)"
        );
    }
    out.notes.push(format!(
        "{id} SKIPPED: run tests/web/m7/run_m7.sh (sets GORCA_M7=1 and the private compositor)"
    ));
    out.x("skipped", 1.0, "flag");
    Ok(false)
}

// ---------------------------------------------------------------------------------------------
// The private Firefox

const USER_JS: &str = r#"
user_pref("accessibility.force_disabled", 0);
user_pref("browser.shell.checkDefaultBrowser", false);
user_pref("browser.startup.homepage_override.mstone", "ignore");
user_pref("browser.aboutwelcome.enabled", false);
user_pref("browser.tabs.warnOnClose", false);
user_pref("browser.warnOnQuit", false);
user_pref("datareporting.policy.dataSubmissionEnabled", false);
user_pref("datareporting.healthreport.uploadEnabled", false);
user_pref("toolkit.telemetry.enabled", false);
user_pref("app.update.auto", false);
user_pref("app.update.enabled", false);
user_pref("network.proxy.type", 0);
user_pref("network.captive-portal-service.enabled", false);
user_pref("browser.startup.page", 0);
user_pref("browser.newtabpage.enabled", false);
user_pref("browser.sessionstore.resume_from_crash", false);
user_pref("security.sandbox.content.level", 4);
"#;

/// A Firefox of ours: own profile, own process group, signalled only through that group.
#[derive(Debug)]
pub(super) struct Firefox {
    child: Child,
    pgid: i32,
    /// `tests/web/m7/rd.py`: a RemoteDesktop session on the private compositor. A headless
    /// shell has no keyboard, so no window is ever active until a virtual input device clicks.
    rd: Option<Child>,
}

impl Firefox {
    pub(super) fn launch(host: &HostEnv, tag: &str, urls: &[String]) -> Result<Self> {
        let wayland = std::env::var("GORCA_M7_WAYLAND").context("GORCA_M7_WAYLAND not set")?;
        let bin = std::env::var("GORCA_M7_FIREFOX").unwrap_or_else(|_| "firefox".to_owned());
        let root = std::env::var("GORCA_HARNESS_DIR").context("GORCA_HARNESS_DIR not set")?;
        if !host.a11y_addr.contains("/gorca-h.") {
            bail!("refusing to start a browser outside a harness session");
        }
        // A fresh profile for every launch: a killed Firefox leaves a session-restore prompt
        // and a stale lock behind, and the next launch would show that page instead.
        static LAUNCHES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = LAUNCHES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let profile = PathBuf::from(&root).join(format!("ffprofile-{tag}{n}"));
        std::fs::create_dir_all(&profile)?;
        std::fs::write(profile.join("user.js"), USER_JS)?;
        let log = std::fs::File::create(PathBuf::from(&root).join(format!("ff-{tag}{n}.log")))?;
        let err = log.try_clone()?;
        let child = Command::new(&bin)
            .args(["--no-remote", "--new-instance", "--profile"])
            .arg(&profile)
            .args(["--width", "1200", "--height", "800"])
            .args(urls)
            .env_remove("DISPLAY")
            .env("WAYLAND_DISPLAY", wayland)
            .env("MOZ_ENABLE_WAYLAND", "1")
            .env("GNOME_ACCESSIBILITY", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err))
            .process_group(0)
            .spawn()
            .with_context(|| format!("starting {bin}"))?;
        let pgid = i32::try_from(child.id()).context("pid")?;
        let rd = Self::start_rd(&root);
        Ok(Self { child, pgid, rd })
    }

    fn start_rd(root: &str) -> Option<Child> {
        let script = std::env::var("GORCA_E2E_SCRIPTS").ok()?;
        let script = PathBuf::from(script).join("../web/m7/rd.py");
        let log = std::fs::File::create(PathBuf::from(root).join("rd.log")).ok()?;
        let err = log.try_clone().ok()?;
        Command::new("/usr/bin/python3")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err))
            .spawn()
            .ok()
    }

    /// Press evdev key codes in the private Firefox window (the compositor's virtual keyboard;
    /// this bypasses the emulated Mutter, so the reader does not see them as key events).
    pub(super) fn keys(&mut self, codes: &[u32]) {
        use std::io::Write;
        let list: Vec<String> = codes.iter().map(u32::to_string).collect();
        if let Some(stdin) = self.rd.as_mut().and_then(|c| c.stdin.as_mut()) {
            let _ = writeln!(stdin, "key {}", list.join(" "));
            let _ = stdin.flush();
        }
    }

    /// Click inside the private Firefox window so the compositor activates it.
    pub(super) fn click(&mut self) {
        use std::io::Write;
        if let Some(stdin) = self.rd.as_mut().and_then(|c| c.stdin.as_mut()) {
            let _ = stdin.write_all(b"click\n");
            let _ = stdin.flush();
        }
    }
}

impl Drop for Firefox {
    fn drop(&mut self) {
        // Our own process group only (CLAUDE.md "Live-session safety"), through kill(1).
        let group = format!("-{}", self.pgid);
        let _ = Command::new("kill").args(["-TERM", "--", &group]).status();
        let end = Instant::now() + Duration::from_secs(4);
        while Instant::now() < end {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = Command::new("kill").args(["-KILL", "--", &group]).status();
        let _ = self.child.wait();
        if let Some(mut rd) = self.rd.take() {
            drop(rd.stdin.take());
            let _ = rd.wait();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The event tap: focus and load-complete arrival times on the a11y bus

#[derive(Clone, Debug)]
struct Seen {
    kind: u8,
    sender: String,
    path: String,
    t_ns: u64,
    /// The first integer of the event (a caret event's offset).
    d1: i32,
}

pub(super) const FOCUS: u8 = 1;
pub(super) const LOAD: u8 = 2;
pub(super) const CARET: u8 = 3;
pub(super) const TEXT: u8 = 4;
pub(super) const SEL: u8 = 5;

#[derive(Debug)]
pub(super) struct EventTap {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

/// `(detail, first integer)` of an AT-SPI event body (`siiva{sv}`).
fn event_head(body: &[u8]) -> Option<(&str, i32)> {
    let len = u32::from_le_bytes(body.get(0..4)?.try_into().ok()?) as usize;
    let detail = std::str::from_utf8(body.get(4..4 + len)?).ok()?;
    let at = (4 + len + 1 + 3) & !3;
    let d1 = i32::from_le_bytes(body.get(at..at + 4)?.try_into().ok()?);
    Some((detail, d1))
}

impl EventTap {
    pub(super) fn start(a11y_addr: &str) -> Result<Self> {
        let mut c = BusConn::connect(a11y_addr, Duration::from_secs(5))?;
        for rule in [
            "type='signal',interface='org.a11y.atspi.Event.Focus'",
            "type='signal',interface='org.a11y.atspi.Event.Object',member='StateChanged'",
            "type='signal',interface='org.a11y.atspi.Event.Document'",
            "type='signal',interface='org.a11y.atspi.Event.Object',member='TextCaretMoved'",
            "type='signal',interface='org.a11y.atspi.Event.Object',member='TextChanged'",
            "type='signal',interface='org.a11y.atspi.Event.Object',member='TextSelectionChanged'",
        ] {
            c.add_match(rule, Duration::from_secs(5))?;
        }
        c.stream.set_read_timeout(Some(Duration::from_millis(20)))?;
        let stop = Arc::new(AtomicBool::new(false));
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let (st, sn) = (Arc::clone(&stop), Arc::clone(&seen));
        let thread = std::thread::Builder::new()
            .name("m7-event-tap".to_owned())
            .spawn(move || {
                use std::io::Read;
                let mut buf = std::mem::take(&mut c.leftover);
                let mut chunk = vec![0u8; 64 * 1024];
                loop {
                    let mut off = 0;
                    loop {
                        match parse(&buf[off..]) {
                            Parse::Msg(m, used) => {
                                let t = now_ns();
                                if m.mtype == SIGNAL
                                    && let (Some(s), Some(p), Some(member)) =
                                        (m.sender, m.path, m.member)
                                {
                                    let kind = match (m.interface, member) {
                                        (Some("org.a11y.atspi.Event.Focus"), "Focus") => FOCUS,
                                        (Some("org.a11y.atspi.Event.Object"), "StateChanged")
                                            if matches!(
                                                event_head(m.body),
                                                Some(("focused", 1))
                                            ) =>
                                        {
                                            FOCUS
                                        }
                                        (Some("org.a11y.atspi.Event.Document"), "LoadComplete") => {
                                            LOAD
                                        }
                                        (Some("org.a11y.atspi.Event.Object"), "TextCaretMoved") => {
                                            CARET
                                        }
                                        (Some("org.a11y.atspi.Event.Object"), "TextChanged") => {
                                            TEXT
                                        }
                                        (
                                            Some("org.a11y.atspi.Event.Object"),
                                            "TextSelectionChanged",
                                        ) => SEL,
                                        _ => 0,
                                    };
                                    if kind != 0 {
                                        sn.lock().unwrap_or_else(PoisonError::into_inner).push(
                                            Seen {
                                                kind,
                                                sender: s.to_owned(),
                                                path: p.to_owned(),
                                                t_ns: t,
                                                d1: event_head(m.body).map_or(-1, |(_, d)| d),
                                            },
                                        );
                                    }
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

    /// `(arrival, first integer)` of every event of `kind` from `sender` at `path` since `t0`,
    /// oldest first (a caret event's integer is its offset).
    pub(super) fn events_since(
        &self,
        kind: u8,
        sender: &str,
        path: &str,
        t0: u64,
    ) -> Vec<(u64, i32)> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|e| e.kind == kind && e.sender == sender && e.path == path && e.t_ns >= t0)
            .map(|e| (e.t_ns, e.d1))
            .collect()
    }

    /// The last `n` events as "kind path" strings, oldest first.
    pub(super) fn tail(&self, n: usize) -> Vec<String> {
        let seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        let skip = seen.len().saturating_sub(n);
        seen.iter()
            .skip(skip)
            .map(|e| format!("{} {}", e.kind, e.path))
            .collect()
    }

    pub(super) fn count(&self, kind: u8) -> usize {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|e| e.kind == kind)
            .count()
    }

    /// First event of `kind` from `sender` (for `path` when given) that arrived at or after `t0`.
    pub(super) fn first_after(
        &self,
        kind: u8,
        sender: &str,
        path: Option<&str>,
        t0: u64,
    ) -> Option<u64> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|e| e.kind == kind && e.sender == sender && e.t_ns >= t0)
            .find(|e| path.is_none_or(|p| e.path == p))
            .map(|e| e.t_ns)
    }
}

impl Drop for EventTap {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// One run: reader, mutter, mock, Firefox, the AT-SPI driver

#[derive(Debug)]
pub(super) struct Web {
    // Drop order: the browser first, the reader last.
    pub(super) ff: Firefox,
    pub(super) ax: JsonProc,
    pub(super) tap: EventTap,
    pub(super) rig: Rig,
    pub(super) ty: Typist,
    /// A focus or mode change may still be changing the grab set: wait before the next key.
    pub(super) unsettled: bool,
    pub(super) bus: String,
    /// The reader's presentation log of this run (`--bench-present-log`), kept for a loss dump.
    pub(super) present_log: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub(super) struct DocRef {
    pub(super) path: String,
    pub(super) name: String,
}

pub(super) fn pages_dir(host: &HostEnv) -> PathBuf {
    host.harness_dir.join("../web/m7/pages")
}

/// Write the generated 10k-node page next to the static ones.
fn generate_large(host: &HostEnv, dir: &Path, nodes: usize) -> Result<PathBuf> {
    let out = dir.join(format!("large{nodes}.html"));
    if !out.exists() {
        let gen_script = host.harness_dir.join("../web/pages/gen_large.py");
        let st = Command::new(PYTHON)
            .arg(gen_script)
            .arg(nodes.to_string())
            .arg(&out)
            .status()?;
        if !st.success() {
            bail!("gen_large.py failed");
        }
    }
    Ok(out)
}

pub(super) fn file_url(p: &Path) -> Result<String> {
    Ok(format!("file://{}", std::fs::canonicalize(p)?.display()))
}

impl Web {
    /// Start everything and wait for the first document titled `title`. The M7a scenarios key
    /// by line, so the page does not read itself (`[browse] say_all_on_load = false`); the
    /// auto-read scenarios use [`Web::start_cfg`] with the shipped default.
    pub(super) fn start(
        host: &HostEnv,
        reader: &super::apps::ReaderSpec,
        flavor: Flavor,
        urls: &[String],
        title: &str,
    ) -> Result<(Self, DocRef)> {
        Self::start_cfg(
            host,
            reader,
            flavor,
            urls,
            title,
            "schema = 1\n[browse]\nsay_all_on_load = false\n",
        )
    }

    /// [`Web::start`] with the reader's settings file given.
    pub(super) fn start_cfg(
        host: &HostEnv,
        reader: &super::apps::ReaderSpec,
        flavor: Flavor,
        urls: &[String],
        title: &str,
        settings: &str,
    ) -> Result<(Self, DocRef)> {
        let env = m1_env(host);
        // The presentation log of the run: copied next to the results when a trial is lost.
        static RUNS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let run_no = RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let present_log = std::env::var("GORCA_HARNESS_DIR")
            .ok()
            .map(|d| PathBuf::from(d).join(format!("present-{run_no}.log")));
        let mut extra_reader_args: Vec<String> = present_log
            .iter()
            .map(|p| format!("--bench-present-log={}", p.display()))
            .collect();
        let tmp =
            PathBuf::from(std::env::var("GORCA_HARNESS_DIR").context("GORCA_HARNESS_DIR not set")?);
        let cfg_path = tmp.join(format!("gorca-web-{run_no}.toml"));
        std::fs::write(&cfg_path, settings)?;
        extra_reader_args.push(format!("--config={}", cfg_path.display()));
        let opts = RigOpts {
            flavor,
            reader: reader.clone(),
            calls: false,
            keys: true,
            apps: Vec::new(),
            extra_reader_args,
            reader_stderr: None,
            hung_apps: Vec::new(),
        };
        let rig = Rig::start(&env, &opts)?;
        rig.mock
            .configure(Duration::from_millis(1), Duration::from_millis(1));
        if !rig.keys_ok {
            bail!("the reader did not register with the emulated keyboard monitor");
        }
        let tap = EventTap::start(&env.a11y_addr)?;
        let ax = JsonProc::spawn(&host.harness_dir.join("../web/m7/ax.py"), &[])?;
        let ff = Firefox::launch(&env, "a", urls)?;
        let mut w = Self {
            ff,
            ax,
            tap,
            rig,
            ty: Typist::new(),
            unsettled: false,
            present_log,
            bus: String::new(),
        };
        let r = w.ax.request(
            "find_doc",
            &format!("\"title\":\"{title}\",\"timeout_ms\":60000"),
            Duration::from_secs(70),
        )?;
        w.bus = json_str(&r, "bus").context("no bus in find_doc reply")?;
        let doc = DocRef {
            path: json_str(&r, "path").context("no path")?,
            name: json_str(&r, "name").unwrap_or_default(),
        };
        // The browser's own startup work (cache, extensions) settles before anything is timed.
        std::thread::sleep(Duration::from_millis(1500));
        Ok((w, doc))
    }

    pub(super) fn mark(&self) -> usize {
        self.rig.mock.mark()
    }

    /// Focus `path`; returns the emit time of the call.
    pub(super) fn focus(&mut self, path: &str) -> Result<u64> {
        self.unsettled = true;
        let r = self.ax.request(
            "focus",
            &format!("\"bus\":\"{}\",\"path\":\"{path}\"", self.bus),
            Duration::from_secs(10),
        )?;
        Ok(json_num(&r, "t_emit_ns").unwrap_or(0.0) as u64)
    }

    pub(super) fn action(&mut self, path: &str) -> Result<u64> {
        let r = self.ax.request(
            "action",
            &format!("\"bus\":\"{}\",\"path\":\"{path}\",\"index\":0", self.bus),
            Duration::from_secs(10),
        )?;
        Ok(json_num(&r, "t_emit_ns").unwrap_or(0.0) as u64)
    }

    /// A descendant of `root` by role and exact name (empty: any name).
    pub(super) fn find(&mut self, root: &str, role: &str, name: &str) -> Result<String> {
        self.find_with(root, role, name, false)
    }

    /// Like `find` but `name` may be a part of the name (opt-in, for links whose text the
    /// page does not fix).
    pub(super) fn find_partial(&mut self, root: &str, role: &str, name: &str) -> Result<String> {
        self.find_with(root, role, name, true)
    }

    fn find_with(&mut self, root: &str, role: &str, name: &str, partial: bool) -> Result<String> {
        let r = self.ax.request(
            "find",
            &format!(
                "\"bus\":\"{}\",\"root\":\"{root}\",\"role\":\"{role}\",\"name\":\"{name}\",\"partial\":{partial},\"timeout_ms\":8000",
                self.bus
            ),
            Duration::from_secs(12),
        )?;
        json_str(&r, "path").with_context(|| format!("no {role} {name:?} under {root}: {r}"))
    }

    /// Press and release a key; returns `(emit time of the press, consumed)`.
    /// A trial was lost: keep what names the route. The presentation log (snapshot based, so a
    /// password field is redacted), the speech transcript and the last bus events go to the
    /// results directory; the reader's own counters (`stale_dropped`, `inbox_dropped`, `fetch_*`)
    /// are in the reader log that lands there too.
    pub(super) fn dump_loss(&self, what: &str) {
        let Some(dir) = std::env::var("GORCA_E2E_READER_LOG")
            .ok()
            .and_then(|p| PathBuf::from(p).parent().map(PathBuf::from))
        else {
            return;
        };
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        if let Some(src) = &self.present_log {
            let _ = std::fs::copy(src, dir.join(format!("lost-{what}-{stamp}-present.log")));
        }
        use std::fmt::Write as _;
        let mut text = String::new();
        for (t, line) in speaks_since(self, 0) {
            let _ = writeln!(text, "{t} said {line:?}");
        }
        for e in self.tap.tail(40) {
            let _ = writeln!(text, "bus {e}");
        }
        let _ = std::fs::write(dir.join(format!("lost-{what}-{stamp}-state.txt")), text);
    }

    /// Wait until Mutter has stopped receiving grab changes (the count is steady for 120 ms).
    /// A key that races a grab change is Ambiguous by design ("again"), which is not what the
    /// latency scenarios measure.
    pub(super) fn settle_grabs(&mut self) -> Result<()> {
        let m = self.rig.mutter.as_mut().context("no mutter")?;
        let end = Instant::now() + Duration::from_secs(2);
        let mut last = m.grab_state()?.applied;
        let mut since = Instant::now();
        while Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
            let now = m.grab_state()?.applied;
            if now != last {
                last = now;
                since = Instant::now();
            } else if since.elapsed() >= Duration::from_millis(120) {
                break;
            }
        }
        self.unsettled = false;
        Ok(())
    }

    pub(super) fn press(&mut self, k: K, state: u32) -> Result<(u64, bool)> {
        if self.unsettled {
            self.settle_grabs()?;
        }
        if matches!(k.sym, 0xff1b | 0xff0d | 0xff09 | 0x20) {
            self.unsettled = true;
        }
        let m = self.rig.mutter.as_mut().context("no mutter")?;
        self.ty.t_ms += 12;
        let down = m.key_ex(k.code, k.sym, state, false, self.ty.t_ms)?;
        self.ty.t_ms += 3;
        m.key_ex(k.code, k.sym, state, true, self.ty.t_ms)?;
        Ok((down.t_emit_ns, down.consumed))
    }

    /// The first speech after `mark` whose text satisfies `pred`.
    pub(super) fn speech(
        &self,
        mark: usize,
        wait: Duration,
        pred: impl Fn(&str) -> bool,
    ) -> Option<Rec> {
        self.rig.mock.wait_for(
            mark,
            wait,
            |r| matches!(&r.kind, Kind::Speak { text, .. } if pred(text)),
        )
    }

    pub(super) fn press_and_hear(
        &mut self,
        k: K,
        state: u32,
        pred: impl Fn(&str) -> bool,
    ) -> Result<(String, f64)> {
        let mark = self.mark();
        let (t, consumed) = self.press(k, state)?;
        if !consumed {
            bail!("key {:#x} was not grabbed", k.sym);
        }
        match self.speech(mark, Duration::from_secs(3), pred) {
            Some(Rec {
                t_ns,
                kind: Kind::Speak { text, .. },
                ..
            }) => Ok((text, ms(t_ns.saturating_sub(t)))),
            Some(_) | None => {
                let heard: Vec<String> =
                    speaks_since(self, mark).into_iter().map(|s| s.1).collect();
                bail!("no matching speech for key {:#x}; heard {heard:?}", k.sym)
            }
        }
    }

    /// Focus nodes until the reader demonstrably speaks (events before its registration are lost).
    pub(super) fn prime(&mut self, doc: &DocRef) -> Result<()> {
        let link = self.find(&doc.path, "link", "").ok();
        let end = Instant::now() + Duration::from_secs(30);
        let mut flip = false;
        while Instant::now() < end {
            let mark = self.mark();
            let target = if flip { link.clone() } else { None }.unwrap_or_else(|| doc.path.clone());
            flip = !flip;
            if self.tap.count(FOCUS) == 0 {
                self.ff.click();
                std::thread::sleep(Duration::from_millis(300));
            }
            self.focus(&target)?;
            if self
                .speech(mark, Duration::from_millis(1200), |t| {
                    t.contains("browse mode") || t.contains("focus mode")
                })
                .is_some()
            {
                std::thread::sleep(Duration::from_millis(300));
                return Ok(());
            }
        }
        let heard: Vec<String> = speaks_since(self, 0).into_iter().map(|s| s.1).collect();
        bail!(
            "gorca never entered browse mode for {}; it said {heard:?}; the bus carried {} focus and {} load events",
            doc.name,
            self.tap.count(FOCUS),
            self.tap.count(LOAD)
        );
    }
}

pub(super) fn latencies(
    emits: &[u64],
    spoken: &[(u64, String)],
    want: impl Fn(usize, &str) -> bool,
) -> (Vec<f64>, u64, u64) {
    let mut used = vec![false; spoken.len()];
    let mut lat = Vec::with_capacity(emits.len());
    let (mut lost, mut late) = (0u64, 0u64);
    for (i, t) in emits.iter().enumerate() {
        let next = emits.get(i + 1).copied().unwrap_or(u64::MAX);
        let horizon = emits.get(i + 2).copied().unwrap_or(u64::MAX);
        let hit = spoken
            .iter()
            .enumerate()
            .find(|(j, (s, text))| !used[*j] && *s >= *t && *s < horizon && want(i, text));
        match hit {
            Some((j, (s, _))) => {
                used[j] = true;
                lat.push(ms(*s - *t));
                if *s >= next {
                    late += 1;
                }
            }
            None => lost += 1,
        }
    }
    (lat, lost, late)
}

pub(super) fn speaks_since(w: &Web, mark: usize) -> Vec<(u64, String)> {
    w.rig
        .mock
        .since(mark)
        .into_iter()
        .filter_map(|r| match r.kind {
            Kind::Speak { text, .. } => Some((r.t_ns, text)),
            Kind::Connected | Kind::Cancel { .. } | Kind::End => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Scenarios

pub(super) fn article(host: &HostEnv) -> Result<String> {
    file_url(&pages_dir(host).join("article.html"))
}

pub(super) fn check(
    out: &mut RunOut,
    fails: &mut Vec<String>,
    what: &str,
    got: Result<(String, f64)>,
    want: &[&str],
) {
    match got {
        Ok((text, lat)) => {
            if !want.iter().all(|x| text.contains(x)) {
                fails.push(format!("{what}: wanted {want:?}, heard {text:?}"));
            }
            out.notes.push(format!("{what}: {text:?} ({lat:.2} ms)"));
        }
        Err(e) => {
            fails.push(format!("{what}: {e}"));
            out.notes.push(format!("{what}: FAILED {e}"));
        }
    }
}

/// B0: the walkthrough of the M7a acceptance paragraph, on `article.html`.
pub fn b0(
    host: &HostEnv,
    reader: &super::apps::ReaderSpec,
    flavor: Flavor,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "B0", out)? {
        return Ok(());
    }
    let (mut w, doc) = Web::start(host, reader, flavor, &[article(host)?], "M7 Article")?;
    let mut fails: Vec<String> = Vec::new();
    // Entering the page: the title and the first line, after "browse mode".
    w.prime(&doc)?;
    let mark = w.mark();
    w.focus(&doc.path)?;
    let heard = w.speech(mark, Duration::from_secs(5), |t| {
        t.contains("Gorca article")
    });
    out.notes.push(format!(
        "entry: {:?}",
        speaks_since(&w, mark)
            .into_iter()
            .map(|s| s.1)
            .collect::<Vec<_>>()
    ));
    if heard.is_none() {
        fails.push("entry: the first line was not spoken".into());
    }
    let g = |x: &str| x.len() > 3;
    let r = w.press_and_hear(DOWN, 0, g);
    check(out, &mut fails, "Down", r, &["This is the first paragraph"]);
    let any = |x: &str| !x.is_empty();
    let r = w.press_and_hear(RIGHT, 0, any);
    check(out, &mut fails, "Right", r, &[]);
    let r = w.press_and_hear(RIGHT, CTRL, any);
    check(out, &mut fails, "Ctrl+Right", r, &[]);
    let r = w.press_and_hear(KEY_H, 0, g);
    check(
        out,
        &mut fails,
        "H",
        r,
        &["heading level 2", "Introduction"],
    );
    let r = w.press_and_hear(KEY_2, 0, g);
    check(out, &mut fails, "2", r, &["heading level 2", "Details"]);
    let r = w.press_and_hear(KEY_H, 0, g);
    check(
        out,
        &mut fails,
        "H at the last heading",
        r,
        &["no next heading"],
    );
    let r = w.press_and_hear(KEY_H_CAP, SHIFT, g);
    check(
        out,
        &mut fails,
        "Shift+H",
        r,
        &["heading level 2", "Introduction"],
    );
    let r = w.press_and_hear(KEY_K, 0, g);
    check(out, &mut fails, "K", r, &["link"]);
    // A form field by focus (Tab's effect): focus mode, typing would work, Escape returns.
    let field = w.find(&doc.path, "entry", "")?;
    let mark = w.mark();
    w.focus(&field)?;
    let heard = w.speech(mark, Duration::from_secs(3), |t| t.contains("focus mode"));
    if heard.is_none() {
        fails.push("field focus: no \"focus mode\"".into());
    }
    let r = w.press_and_hear(ESC, 0, |t| t.contains("browse mode"));
    check(out, &mut fails, "Escape", r, &["browse mode"]);
    // Back on a link by focus: still browse mode, no word.
    let link = w.find_partial(&doc.path, "link", "second page")?;
    let mark = w.mark();
    w.focus(&link)?;
    let h = w.speech(mark, Duration::from_secs(3), |t| t.contains("link"));
    if let Some(Rec {
        kind: Kind::Speak { text, .. },
        ..
    }) = h
    {
        out.notes.push(format!("Tab onto a link: {text:?}"));
        if text.contains("focus mode") {
            fails.push(format!("a link must stay in browse mode: {text:?}"));
        }
    } else {
        fails.push("link focus: no speech".into());
    }
    // Enter follows the link: the next page is read from its top.
    let mark = w.mark();
    let (_, consumed) = w.press(ENTER, 0)?;
    if !consumed {
        fails.push("Enter was not grabbed in browse mode".into());
    }
    let page2 = w.speech(mark, Duration::from_secs(15), |t| t.contains("M7 Second"));
    out.notes.push(format!(
        "after Enter: {:?}",
        speaks_since(&w, mark)
            .into_iter()
            .map(|s| s.1)
            .collect::<Vec<_>>()
    ));
    if page2.is_none() {
        fails.push("Enter on the link: the new page was not announced".into());
    }
    for f in &fails {
        out.notes.push(format!("FAIL {f}"));
    }
    out.x("web_failures", fails.len() as f64, "checks");
    Ok(())
}

/// Spoken lines of a series, with the plan of keys.
fn key_series(
    w: &mut Web,
    out: &mut RunOut,
    metric: &str,
    plan: &[(K, u32)],
    n: usize,
    want: impl Fn(usize, &str) -> bool,
) -> Result<()> {
    // Warm: a few of the same keys, unscored.
    for i in 0..6 {
        let (k, st) = plan[i % plan.len()];
        w.press(k, st)?;
        std::thread::sleep(Duration::from_millis(60));
    }
    std::thread::sleep(Duration::from_millis(200));
    let mark = w.mark();
    let mut emits = Vec::with_capacity(n);
    let mut not_consumed = 0u64;
    for i in 0..n {
        let (k, st) = plan[i % plan.len()];
        let (t, consumed) = w.press(k, st)?;
        emits.push(t);
        if !consumed {
            not_consumed += 1;
        }
        std::thread::sleep(Duration::from_millis(45));
    }
    std::thread::sleep(Duration::from_millis(500));
    let spoken = speaks_since(w, mark);
    let (lat, lost, late) = latencies(&emits, &spoken, want);
    out.s(metric, lat);
    out.x(&format!("lost_{metric}"), lost as f64, "keys");
    out.x(&format!("late_{metric}"), late as f64, "keys");
    out.x("not_consumed", not_consumed as f64, "keys");
    Ok(())
}

pub(super) fn large(host: &HostEnv) -> Result<(String, String)> {
    let dir =
        PathBuf::from(std::env::var("GORCA_HARNESS_DIR").context("harness dir")?).join("pages");
    std::fs::create_dir_all(&dir)?;
    let p = generate_large(host, &dir, 10_000)?;
    Ok((file_url(&p)?, "large 10000".to_owned()))
}

/// W1 and BR1 share a rig on the 10k page.
fn on_large(
    host: &HostEnv,
    reader: &super::apps::ReaderSpec,
    flavor: Flavor,
) -> Result<(Web, DocRef)> {
    let (url, title) = large(host)?;
    let (mut w, doc) = Web::start(host, reader, flavor, &[url], &title)?;
    w.prime(&doc)?;
    Ok((w, doc))
}

pub fn w1(
    host: &HostEnv,
    reader: &super::apps::ReaderSpec,
    flavor: Flavor,
    size: Size,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "W1", out)? {
        return Ok(());
    }
    let (mut w, _doc) = on_large(host, reader, flavor)?;
    // Start in the middle of the heading list so both directions have room.
    for _ in 0..40 {
        w.press(KEY_H, 0)?;
        std::thread::sleep(Duration::from_millis(30));
    }
    key_series(
        &mut w,
        out,
        "w1_ms",
        &[(KEY_H, 0), (KEY_H_CAP, SHIFT)],
        size.n,
        |_, t| t.contains("heading level"),
    )
}

pub fn br1(
    host: &HostEnv,
    reader: &super::apps::ReaderSpec,
    flavor: Flavor,
    size: Size,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "BR1", out)? {
        return Ok(());
    }
    let (mut w, _doc) = on_large(host, reader, flavor)?;
    for _ in 0..30 {
        w.press(DOWN, 0)?;
        std::thread::sleep(Duration::from_millis(30));
    }
    let any = |_: usize, t: &str| !t.is_empty();
    key_series(&mut w, out, "br1_ms", &[(DOWN, 0), (UP, 0)], size.n, any)?;
    key_series(
        &mut w,
        out,
        "br1c_ms",
        &[(RIGHT, 0), (LEFT, 0)],
        size.n,
        any,
    )?;
    key_series(
        &mut w,
        out,
        "br1w_ms",
        &[(RIGHT, CTRL), (LEFT, CTRL)],
        size.n,
        any,
    )
}

/// BR2: held Down at 30 Hz. A line spoken after the next key was pressed is stale.
pub fn br2(
    host: &HostEnv,
    reader: &super::apps::ReaderSpec,
    flavor: Flavor,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "BR2", out)? {
        return Ok(());
    }
    let (mut w, _doc) = on_large(host, reader, flavor)?;
    std::thread::sleep(Duration::from_millis(300));
    let mark = w.mark();
    let mut emits = Vec::new();
    let period = Duration::from_micros(33_333);
    let t_end = Instant::now() + Duration::from_secs(3);
    let mut next = Instant::now();
    while Instant::now() < t_end {
        let m = w.rig.mutter.as_mut().context("no mutter")?;
        w.ty.t_ms += 33;
        let down = m.key_ex(DOWN.code, DOWN.sym, 0, false, w.ty.t_ms)?;
        emits.push(down.t_emit_ns);
        next += period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        }
    }
    let m = w.rig.mutter.as_mut().context("no mutter")?;
    m.key_ex(DOWN.code, DOWN.sym, 0, true, w.ty.t_ms + 5)?;
    std::thread::sleep(Duration::from_millis(600));
    let spoken = speaks_since(&w, mark);
    // stale: a SPEAK that arrived after a later key was already pressed (it should have been
    // cancelled by that key's epoch).
    let mut stale = 0u64;
    for (t, _) in &spoken {
        let pressed_after = emits.iter().filter(|e| **e <= *t).count();
        let newest_before = emits.iter().rposition(|e| *e <= *t);
        // The speech belongs to some key; it is stale when two or more keys were pressed
        // after the one it answers (a line is answered within a couple of ms).
        if let Some(i) = newest_before
            && let Some(prev) = i.checked_sub(1)
            && *t > emits[prev].saturating_add(15_000_000)
            && pressed_after > 1
            && t.saturating_sub(emits[i]) > 15_000_000
        {
            stale += 1;
        }
    }
    let last_emit = emits.last().copied().unwrap_or(0);
    let last_ok = spoken.iter().any(|(t, _)| *t >= last_emit);
    out.x("keys", emits.len() as f64, "keys");
    out.x("spoken", spoken.len() as f64, "utterances");
    out.x("stale", stale as f64, "utterances");
    out.x("last_ok", f64::from(u8::from(last_ok)), "bool");
    Ok(())
}

/// The `gorca_phase` timing lines the reader wrote to `GORCA_E2E_READER_LOG` after byte `from`:
/// per phase, the numeric fields of its last line.
fn phase_lines(
    from: u64,
) -> std::collections::HashMap<String, std::collections::HashMap<String, f64>> {
    let mut out = std::collections::HashMap::new();
    let Ok(path) = std::env::var("GORCA_E2E_READER_LOG") else {
        return out;
    };
    let Ok(bytes) = std::fs::read(path) else {
        return out;
    };
    let from = usize::try_from(from).unwrap_or(0).min(bytes.len());
    let text = String::from_utf8_lossy(&bytes[from..]);
    for line in text.lines().filter(|l| l.contains("gorca_phase")) {
        let line = strip_ansi(line);
        let mut phase = None;
        let mut fields = std::collections::HashMap::new();
        for tok in line.split_whitespace() {
            if let Some((k, v)) = tok.split_once('=') {
                if k == "phase" {
                    phase = Some(v.trim_matches('"').to_owned());
                } else if let Ok(x) = v.parse::<f64>() {
                    fields.insert(k.to_owned(), x);
                }
            }
        }
        if let Some(p) = phase {
            out.insert(p, fields);
        }
    }
    out
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn reader_log_len() -> u64 {
    std::env::var("GORCA_E2E_READER_LOG")
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .map_or(0, |m| m.len())
}

/// W2b: focus enters a loaded, never visited 10k-node document.
pub fn w2b(
    host: &HostEnv,
    reader: &super::apps::ReaderSpec,
    flavor: Flavor,
    size: Size,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "W2b", out)? {
        return Ok(());
    }
    let trials = (size.trials / 20).clamp(3, 6);
    let mut lat = Vec::new();
    // Per-phase timings (ms), one value per trial: see `phase_lines`.
    let mut gorca_side = Vec::new();
    let mut phases: std::collections::BTreeMap<&str, Vec<f64>> = std::collections::BTreeMap::new();
    let mut lost = 0u64;
    let mut untagged_warm = 0u64;
    for _ in 0..trials {
        let log_from = reader_log_len();
        // A fresh reader and browser every time: the 10k document must be never visited. A
        // headless compositor activates no window by itself; the click that does it makes
        // Firefox focus the document, which is the first event the reader sees (it attaches to
        // the application at that moment). Time runs from that event's arrival on the bus.
        let (url, title) = large(host)?;
        let (mut w, _doc) = Web::start(host, reader, flavor, &[url], &title)?;
        // Let the attach-time cache warm-up settle (it runs in the background).
        std::thread::sleep(Duration::from_secs(4));
        let mark = w.mark();
        let t0 = super::mono::now_ns();
        let mut heard = None;
        for _ in 0..10 {
            if w.tap.count(FOCUS) == 0 {
                w.ff.click();
            }
            heard = w.speech(mark, Duration::from_secs(2), |t| t.contains("link"));
            if heard.is_some() {
                break;
            }
        }
        match heard {
            Some(r) => {
                let arrival = w.tap.first_after(FOCUS, &w.bus, None, t0).unwrap_or(t0);
                let wall = ms(r.t_ns.saturating_sub(arrival));
                lat.push(wall);
                // Phases from the reader's own log. Firefox-blocked time is what Firefox
                // needed to answer the locate walk and the document fetch; the whole warm-up
                // protocol (its probes, sampling rounds and sleeps) is the reader's own cost.
                let ph = phase_lines(log_from);
                let f = |p: &str, k: &str| ph.get(p).and_then(|m| m.get(k)).copied();
                let fetch_blocked = [
                    "frontier_ms",
                    "get_tree_ms",
                    "roles_ms",
                    "states_ms",
                    "text_ms",
                ]
                .iter()
                .filter_map(|k| f("fetch", k))
                .sum::<f64>();
                // The progressive build reports its first block (fetched from Firefox), the
                // single-block build reports per-stage call times.
                let build_blocked = f("first_block", "wall_ms").unwrap_or(fetch_blocked);
                let firefox = f("locate", "wall_ms").unwrap_or(0.0) + build_blocked;
                gorca_side.push((wall - firefox).max(0.0));
                for (name, p, k) in [
                    ("warm_wall_ms", "warm", "wall_ms"),
                    ("warm_blocked_ms", "warm", "blocked_ms"),
                    ("warm_slept_ms", "warm", "slept_ms"),
                    ("warm_rounds", "warm", "rounds"),
                    ("warm_probes", "warm", "probes"),
                    ("warm_sampled_nodes", "warm", "sampled_nodes"),
                    ("warm_tagged_nodes", "warm", "tagged_nodes"),
                    ("locate_ms", "locate", "wall_ms"),
                    ("create_ms", "create", "wall_ms"),
                    ("fetch_total_ms", "fetch", "total_ms"),
                    ("fetch_get_tree_ms", "fetch", "get_tree_ms"),
                    (
                        "fetch_heaviest_get_tree_ms",
                        "fetch",
                        "heaviest_get_tree_ms",
                    ),
                    ("fetch_calls", "fetch", "calls"),
                    ("fetch_nodes", "fetch", "nodes"),
                    ("first_block_ms", "first_block", "wall_ms"),
                    ("first_block_nodes", "first_block", "nodes"),
                    ("translate_ms", "translate", "wall_ms"),
                ] {
                    if let Some(v) = f(p, k) {
                        phases.entry(name).or_default().push(v);
                    }
                }
                phases
                    .entry("firefox_blocked_ms")
                    .or_default()
                    .push(firefox);
                // Attribute completeness: the warm-up's sample must have carried tags.
                if f("warm", "tagged_nodes").unwrap_or(0.0) < 1.0 {
                    untagged_warm += 1;
                }
            }
            None => {
                lost += 1;
                w.dump_loss("w2b");
                let heard: Vec<String> = speaks_since(&w, mark).into_iter().map(|s| s.1).collect();
                out.notes.push(format!(
                    "W2b lost: heard {heard:?}; {} focus, {} load events on the bus",
                    w.tap.count(FOCUS),
                    w.tap.count(LOAD)
                ));
            }
        }
    }
    out.x("untagged_warm", untagged_warm as f64, "trials");
    out.s("w2b_gorca_ms", gorca_side);
    for (name, v) in phases {
        out.s(&format!("w2b_phase_{name}"), v);
    }
    // `w2b_ms` is the wall time (information); the gate is on `w2b_gorca_ms`.
    out.s("w2b_ms", lat);
    out.x("lost", lost as f64, "trials");
    Ok(())
}

/// FM1 and FM2 on the form page: focus onto a field, then Escape.
pub fn fm(
    host: &HostEnv,
    reader: &super::apps::ReaderSpec,
    flavor: Flavor,
    size: Size,
    out: &mut RunOut,
    escape: bool,
) -> Result<()> {
    let id = if escape { "FM2" } else { "FM1" };
    if !gate_open(host, id, out)? {
        return Ok(());
    }
    let url = file_url(&pages_dir(host).join("form.html"))?;
    let (mut w, doc) = Web::start(host, reader, flavor, &[url], "M7 Sign in")?;
    w.prime(&doc)?;
    let field = w.find(&doc.path, "entry", "")?;
    let link = w.find(&doc.path, "link", "Back")?;
    let (mut fm1, mut fm2) = (Vec::new(), Vec::new());
    let (mut lost, mut not_consumed, mut lost_after_switch, mut race_lost) =
        (0u64, 0u64, 0u64, 0u64);
    let mut grab_ms: Vec<f64> = Vec::new();
    let trials = (size.trials / 4).clamp(8, 60);
    for trial in 0..trials {
        // Onto a link first (browse mode), then onto the field (focus mode).
        w.focus(&link)?;
        std::thread::sleep(Duration::from_millis(250));
        let mark = w.mark();
        let applied0 = w
            .rig
            .mutter
            .as_mut()
            .context("no mutter")?
            .grab_state()?
            .applied;
        let t_call = w.focus(&field)?;
        // How long the browse grabs stay on after the focus event reached the bus.
        let (_, grabbed) = w
            .rig
            .mutter
            .as_mut()
            .context("no mutter")?
            .wait_grabs(Duration::from_secs(2), |g| g.applied > applied0)?;
        let t_grab = super::mono::now_ns();
        if grabbed && !escape {
            let arrival = w
                .tap
                .first_after(FOCUS, &w.bus, Some(&field), t_call)
                .unwrap_or(t_call);
            grab_ms.push(ms(t_grab.saturating_sub(arrival)));
        }
        match w.speech(mark, Duration::from_secs(3), |t| t.contains("focus mode")) {
            Some(r) => {
                // Gorca side: from the focus event on the bus to the speech.
                let arrival = w
                    .tap
                    .first_after(FOCUS, &w.bus, Some(&field), t_call)
                    .unwrap_or(t_call);
                fm1.push(ms(r.t_ns.saturating_sub(arrival)));
            }
            None => {
                lost += 1;
                w.dump_loss("fm1");
                continue;
            }
        }
        if !escape {
            // L3: a key typed after the switch to focus mode must reach the field: not grabbed,
            // not answered with "again". A typist needs 100 ms at the very least after hearing
            // the word: that is the gated case. A key sent at once is the race window itself
            // (reported, not gated; its length is `fm_grab_ms`).
            let human = trial % 2 == 0;
            w.unsettled = false;
            if human {
                std::thread::sleep(Duration::from_millis(100));
            }
            let mark = w.mark();
            let (_, consumed) = w.press(KEY_H, 0)?;
            let again = w
                .speech(mark, Duration::from_millis(150), |t| t == "again")
                .is_some();
            if consumed || again {
                if human {
                    lost_after_switch += 1;
                } else {
                    race_lost += 1;
                }
            }
        }
        if escape {
            std::thread::sleep(Duration::from_millis(250));
            let mark = w.mark();
            let (t, consumed) = w.press(ESC, 0)?;
            if !consumed {
                not_consumed += 1;
            }
            match w.speech(mark, Duration::from_secs(3), |t| t.contains("browse mode")) {
                Some(r) => fm2.push(ms(r.t_ns.saturating_sub(t))),
                None => {
                    lost += 1;
                    w.dump_loss("fm2");
                }
            }
        }
    }
    if escape {
        out.s("fm2_ms", fm2);
    } else {
        out.s("fm1_ms", fm1);
    }
    out.x("lost", lost as f64, "trials");
    out.x("not_consumed", not_consumed as f64, "keys");
    if !escape {
        out.x("lost_after_switch", lost_after_switch as f64, "keys");
        out.x("race_lost_at_once", race_lost as f64, "keys");
        out.s("fm_grab_ms", grab_ms);
    }
    Ok(())
}

/// W3: two tabs; back to the first, which slept while the second was read.
pub fn w3(
    host: &HostEnv,
    reader: &super::apps::ReaderSpec,
    flavor: Flavor,
    size: Size,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "W3", out)? {
        return Ok(());
    }
    let a = article(host)?;
    let b = file_url(&pages_dir(host).join("second.html"))?;
    let (mut w, doc_a) = Web::start(host, reader, flavor, &[a, b], "M7 Article")?;
    w.prime(&doc_a)?;
    let r = w.ax.request(
        "find_doc",
        "\"title\":\"M7 Second\",\"timeout_ms\":20000",
        Duration::from_secs(30),
    )?;
    let doc_b = DocRef {
        path: json_str(&r, "path").context("second document not found")?,
        name: "M7 Second".to_owned(),
    };
    let mut lat = Vec::new();
    let mut lost = 0u64;
    // Firefox shows the last URL it was given: Alt+1 selects the first tab.
    w.ff.keys(&[56, 2]);
    std::thread::sleep(Duration::from_millis(1000));
    for _ in 0..(size.trials / 20).clamp(3, 10) {
        // Read in A (the cursor moves), go to B, come back to A.
        w.press(DOWN, 0)?;
        std::thread::sleep(Duration::from_millis(300));
        // Ctrl+PageDown / Ctrl+PageUp switch tabs in the private Firefox.
        w.ff.keys(&[29, 109]);
        std::thread::sleep(Duration::from_millis(800));
        let mark = w.mark();
        let t_call = super::mono::now_ns();
        w.ff.keys(&[29, 104]);
        // Which tab Alt+1 selected can vary under load, so either tab speaking counts: the gate is
        // that a tab switch is never silent. Firefox restores focus to what the tab had (a link)
        // or to the document.
        match w.speech(mark, Duration::from_secs(3), |t| {
            t.contains("M7 ") || t.contains("link")
        }) {
            Some(r) => {
                let arrival = w
                    .tap
                    .first_after(FOCUS, &w.bus, None, t_call)
                    .unwrap_or(t_call);
                lat.push(ms(r.t_ns.saturating_sub(arrival)));
            }
            None => {
                lost += 1;
                w.dump_loss("w3");
                let heard: Vec<String> = speaks_since(&w, mark).into_iter().map(|s| s.1).collect();
                out.notes.push(format!(
                    "W3 lost: heard {heard:?}; {} focus events on the bus, last {:?} (A is {}, B is {})",
                    w.tap.count(FOCUS),
                    w.tap.tail(8),
                    doc_a.path,
                    doc_b.path
                ));
            }
        }
    }
    out.s("w3_ms", lat);
    out.x("lost", lost as f64, "trials");
    Ok(())
}

/// W2: a link click loads the 10k-node page; informational wall time from load-complete.
pub fn w2(
    host: &HostEnv,
    reader: &super::apps::ReaderSpec,
    flavor: Flavor,
    out: &mut RunOut,
) -> Result<()> {
    if !gate_open(host, "W2", out)? {
        return Ok(());
    }
    let (url, title) = large(host)?;
    let dir =
        PathBuf::from(std::env::var("GORCA_HARNESS_DIR").context("harness dir")?).join("pages");
    let start = dir.join("start.html");
    std::fs::write(
        &start,
        format!(
            "<!doctype html><title>M7 Start</title><h1>Start</h1><p><a href=\"{url}\">go to the large page</a></p>"
        ),
    )?;
    let (mut w, doc) = Web::start(host, reader, flavor, &[file_url(&start)?], "M7 Start")?;
    w.prime(&doc)?;
    let link = w.find(&doc.path, "link", "go to")?;
    let mark = w.mark();
    let t_call = w.action(&link)?;
    let heard = w.speech(mark, Duration::from_secs(30), |t| t.contains(&title));
    let load = w.tap.first_after(LOAD, &w.bus, None, t_call);
    match (heard, load) {
        (Some(r), Some(lc)) => out.x("w2_ms", ms(r.t_ns.saturating_sub(lc)), "ms"),
        (Some(r), None) => out.x("w2_wall_ms", ms(r.t_ns.saturating_sub(t_call)), "ms"),
        (None, _) => out.x("lost", 1.0, "trials"),
    }
    Ok(())
}
