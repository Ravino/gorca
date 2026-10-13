//! The presentation log (B-1 PW-10): one JSON line per utterance that presents a node, and one per
//! refine or follow-up of it, so a harness can judge every presentation against the truth.
//!
//! Passive (the daemon opens it for `GORCA_PRESENT_LOG=<path>`): compiled in all builds, written by its own thread through a bounded channel, never
//! blocks the Reader (a full channel drops the record and counts it), and no behaviour depends on
//! it. The core holds no object paths: a [`NodeLabel`] supplied by the daemon (backed by the
//! connector's path table) turns a `NodeKey` into `(bus name, path)` on the writer thread.
//!
//! Line format (`CLOCK_MONOTONIC` nanoseconds, read by `gorca-bench-b1`'s verdict classifier):
//!
//! ```json
//! {"t_ns":1,"utt_id":7,"epoch":3,"app_bus":":1.42","path":"/org/a11y/atspi/accessible/9",
//!  "key":"1:4:00ff","source":"hit","role":"check_box","states":["checked"],"name":"Wi-Fi",
//!  "value":null,"may_refine":true,"refines":null}
//! ```
//!
//! `role` is the snake_case name of the core `Role`; `states` lists every state the connector
//! decoded by its AT-SPI name (`checked`, `enabled`, `sensitive`, `showing`, ...), sorted; `name`/`value` are those of the snapshot the utterance was
//! built from (for a follow-up: the corrected snapshot, not the spoken delta). `key` is the stable
//! join key `<connector>:<app serial>:<local id hex>`, present even when the path is unknown.

use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use gorca_model::{NodeKey, NodeSnapshot, Value};

use super::log_states;

/// Records waiting for the writer thread.
const LOG_QUEUE: usize = 8_192;
/// How long dropping the log waits for the writer to drain.
const DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Where a presentation came from (the verdict classifier's `source`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PresentSource {
    /// Served from the mirror with every needed field present and fresh.
    Hit,
    /// Served from the mirror although fields were missing (not NAME or ROLE).
    Partial,
    /// Served from the mirror although fields were stale (not NAME or ROLE).
    Stale,
    /// Built from a fetch reply (the cold path, or a state change).
    MissReply,
    /// A correction that replaces an earlier utterance.
    Refine,
    /// A correction spoken after the earlier utterance was too far gone to replace.
    Followup,
    /// The change of a state or value of the node being presented (B-1 S1).
    StateChange,
}

impl PresentSource {
    pub fn as_str(self) -> &'static str {
        match self {
            PresentSource::Hit => "hit",
            PresentSource::Partial => "partial",
            PresentSource::Stale => "stale",
            PresentSource::MissReply => "miss_reply",
            PresentSource::Refine => "refine",
            PresentSource::Followup => "followup",
            PresentSource::StateChange => "s1",
        }
    }
}

/// Turns a key into `(bus name, object path)` for the log. Implemented by the daemon over the
/// connector's path table; the core never sees a path.
pub trait NodeLabel: Send + Sync + 'static {
    fn label(&self, key: NodeKey) -> Option<(String, String)>;
}

/// No paths: records carry the `key` only.
#[derive(Debug, Default)]
pub struct NoLabels;

impl NodeLabel for NoLabels {
    fn label(&self, _key: NodeKey) -> Option<(String, String)> {
        None
    }
}

/// One presented node, captured on the Reader (cheap) and formatted on the writer thread.
#[derive(Debug)]
pub struct PresentRecord {
    pub t_ns: u64,
    pub utt_id: u64,
    pub epoch: u64,
    pub source: PresentSource,
    pub may_refine: bool,
    pub refines: Option<u64>,
    pub snap: Arc<NodeSnapshot>,
}

#[derive(Debug)]
pub struct PresentLog {
    tx: Option<SyncSender<PresentRecord>>,
    dropped: Arc<AtomicU64>,
    written: Arc<AtomicU64>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl PresentLog {
    /// Log to `out`, formatting on a dedicated thread. Fails only if the thread cannot start.
    pub fn new<W: Write + Send + 'static>(
        out: W,
        labels: Arc<dyn NodeLabel>,
    ) -> std::io::Result<Arc<Self>> {
        let (tx, rx) = sync_channel::<PresentRecord>(LOG_QUEUE);
        let written = Arc::new(AtomicU64::new(0));
        let w = Arc::clone(&written);
        let thread = std::thread::Builder::new()
            .name("present-log".to_owned())
            .spawn(move || {
                let mut out = std::io::BufWriter::new(out);
                let mut line = String::with_capacity(256);
                while let Ok(first) = rx.recv() {
                    let mut next = Some(first);
                    while let Some(rec) = next {
                        line.clear();
                        format_record(&mut line, &rec, labels.as_ref());
                        if out.write_all(line.as_bytes()).is_err() {
                            return;
                        }
                        w.fetch_add(1, Ordering::Relaxed);
                        next = rx.try_recv().ok();
                    }
                    if out.flush().is_err() {
                        return;
                    }
                }
                let _ = out.flush();
            })?;
        Ok(Arc::new(Self {
            tx: Some(tx),
            dropped: Arc::new(AtomicU64::new(0)),
            written,
            thread: Mutex::new(Some(thread)),
        }))
    }

    /// Log to the file `path` (created or truncated).
    pub fn to_file(path: &Path, labels: Arc<dyn NodeLabel>) -> std::io::Result<Arc<Self>> {
        Self::new(std::fs::File::create(path)?, labels)
    }

    /// Never blocks: a full queue drops the record and counts it.
    pub fn record(&self, rec: PresentRecord) {
        let Some(tx) = &self.tx else { return };
        match tx.try_send(rec) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Relaxed)
    }
}

impl Drop for PresentLog {
    /// Closes the queue and gives the writer thread a moment to drain it. A write that is stuck
    /// (a pipe nobody reads) must not hold up shutdown: the thread is then detached, and ends
    /// by itself when the write returns.
    #[allow(clippy::disallowed_methods)] // a bounded wait (2 s) in a Drop that never runs on a lane
    fn drop(&mut self) {
        drop(self.tx.take());
        let h = self
            .thread
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(h) = h else { return };
        let deadline = std::time::Instant::now() + DRAIN_GRACE;
        while !h.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        if h.is_finished() {
            let _ = h.join();
        } else {
            tracing::warn!("the presentation log writer is stuck; detaching it");
        }
    }
}

fn json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn format_record(out: &mut String, r: &PresentRecord, labels: &dyn NodeLabel) {
    let n = &*r.snap;
    let (bus, path) = labels.label(n.key).unwrap_or_default();
    let _ = write!(
        out,
        "{{\"t_ns\":{},\"utt_id\":{},\"epoch\":{},\"app_bus\":",
        r.t_ns, r.utt_id, r.epoch
    );
    json_str(out, &bus);
    out.push_str(",\"path\":");
    json_str(out, &path);
    let _ = write!(
        out,
        ",\"key\":\"{}:{}:{:016x}\",\"source\":\"{}\",\"role\":\"{}\",\"states\":[",
        n.key.app.connector.0,
        n.key.app.serial,
        n.key.local.0,
        r.source.as_str(),
        n.role.log_name()
    );
    for (i, s) in log_states(n).iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(out, "\"{s}\"");
    }
    out.push_str("],\"name\":");
    match n.name.as_deref() {
        Some(name) if !name.is_empty() => json_str(out, name),
        Some(_) | None => out.push_str("null"),
    }
    out.push_str(",\"value\":");
    match &n.value {
        Some(Value::Range { current, .. }) if current.is_finite() => {
            let _ = write!(out, "{current}");
        }
        Some(Value::Text(t)) if !t.is_empty() => json_str(out, t),
        Some(Value::Range { .. } | Value::Text(_)) | None => out.push_str("null"),
    }
    let _ = write!(out, ",\"may_refine\":{},\"refines\":", r.may_refine);
    match r.refines {
        Some(id) => {
            let _ = write!(out, "{id}");
        }
        None => out.push_str("null"),
    }
    out.push_str("}\n");
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use gorca_model::{
        AppId, AttrMap, ChildList, ConnectorId, Freshness, Ifaces, LocalId, PropMask, Relations,
        Role, StateSet,
    };
    use std::time::Instant;

    struct Open(Arc<(Mutex<bool>, std::sync::Condvar)>);
    impl Drop for Open {
        fn drop(&mut self) {
            let (m, cv) = &*self.0;
            *m.lock().unwrap_or_else(PoisonError::into_inner) = true;
            cv.notify_all();
        }
    }

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct Fixed;
    impl NodeLabel for Fixed {
        fn label(&self, k: NodeKey) -> Option<(String, String)> {
            (k.local.0 == 9).then(|| (":1.42".to_owned(), "/a/9".to_owned()))
        }
    }

    fn snap(local: u64, name: Option<&str>, value: Option<Value>) -> Arc<NodeSnapshot> {
        Arc::new(NodeSnapshot {
            key: NodeKey {
                app: AppId {
                    connector: ConnectorId(1),
                    serial: 4,
                },
                local: LocalId(local),
            },
            role: Role::CheckBox,
            states: StateSet::CHECKED | StateSet::FOCUSED,
            name: name.map(Arc::from),
            description: None,
            value,
            parent: None,
            children: ChildList::Unknown,
            interfaces: Ifaces::empty(),
            attrs: AttrMap::default(),
            relations: Relations::default(),
            bounds: None,
            have: PropMask::PRESENT,
            confirmed_seq: 0,
            fresh: Freshness::Fresh { at: Instant::now() },
        })
    }

    fn rec(s: Arc<NodeSnapshot>, source: PresentSource, refines: Option<u64>) -> PresentRecord {
        PresentRecord {
            t_ns: 123,
            utt_id: 7,
            epoch: 3,
            source,
            may_refine: true,
            refines,
            snap: s,
        }
    }

    #[test]
    fn a_record_is_one_json_line_with_the_documented_fields() {
        let sink = Sink::default();
        let log = PresentLog::new(sink.clone(), Arc::new(Fixed)).unwrap();
        log.record(rec(
            snap(9, Some("Wi\"Fi\n"), None),
            PresentSource::Hit,
            None,
        ));
        log.record(rec(
            snap(
                10,
                None,
                Some(Value::Range {
                    current: 4.5,
                    min: 0.0,
                    max: 9.0,
                }),
            ),
            PresentSource::Followup,
            Some(7),
        ));
        drop(log);
        let text = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let a: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(a["t_ns"], 123);
        assert_eq!(a["utt_id"], 7);
        assert_eq!(a["epoch"], 3);
        assert_eq!(a["app_bus"], ":1.42");
        assert_eq!(a["path"], "/a/9");
        assert_eq!(a["key"], "1:4:0000000000000009");
        assert_eq!(a["source"], "hit");
        assert_eq!(a["role"], "check_box");
        assert_eq!(
            a["states"],
            serde_json::json!(["checked", "focused", "sensitive"])
        );
        assert_eq!(a["name"], "Wi\"Fi\n");
        assert!(a["value"].is_null());
        assert_eq!(a["may_refine"], true);
        assert!(a["refines"].is_null());
        let b: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(b["source"], "followup");
        assert_eq!(b["refines"], 7);
        assert_eq!(b["value"], 4.5);
        assert!(b["name"].is_null());
        assert_eq!(b["path"], "", "unknown path: the key is still there");
        assert_eq!(b["key"], "1:4:000000000000000a");
    }

    #[test]
    fn a_full_queue_drops_and_never_blocks() {
        struct Stuck(Arc<(Mutex<bool>, std::sync::Condvar)>);
        impl Write for Stuck {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                let (m, cv) = &*self.0;
                let mut open = m.lock().unwrap_or_else(PoisonError::into_inner);
                while !*open {
                    open = cv.wait(open).unwrap_or_else(PoisonError::into_inner);
                }
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        // Opens the gate however the test ends, so the writer thread can finish.
        let _open = Open(Arc::clone(&gate));
        let log = PresentLog::new(Stuck(Arc::clone(&gate)), Arc::new(NoLabels)).unwrap();
        let s = snap(1, Some("x"), None);
        let t0 = Instant::now();
        for _ in 0..(LOG_QUEUE + 2_000) {
            log.record(rec(Arc::clone(&s), PresentSource::Hit, None));
        }
        assert!(t0.elapsed() < std::time::Duration::from_secs(2));
        assert!(log.dropped() > 0);
    }
}
