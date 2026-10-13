//! H-5: D-Bus message accounting.
//!
//! Finding (Debian 13, dbus-daemon 1.16.2): the daemon **does** expose
//! `org.freedesktop.DBus.Debug.Stats` (`GetStats`, `GetConnectionStats`), but it is useless for
//! counting traffic: `GetStats` is memory/connection gauges, and in `GetConnectionStats` the keys
//! `IncomingMessages`/`OutgoingMessages` are the **current queue depth** (0 on an idle bus), with
//! `Peak*` high-water marks, not cumulative counters (verified: 25 round trips leave both at 0).
//! So cumulative counts come from a passive **monitor** on the private bus ([`Monitor`],
//! `BecomeMonitor`): with gorca's own declared match rules it receives exactly the signals gorca
//! receives (the SPEC 6.1 cross-check against `DrainStats.messages`), and with an empty rule set
//! it counts every call, reply and signal per sender/destination/member. Fake-app counters
//! (calls served) are the third, independent source. dbus-broker (live runs) has no Stats at all.
//!
//! SPEC 6.1: `msgs = 2 * calls + signals_received`; counted calls (fake apps) versus signals
//! (drain) versus the monitor must agree within 1 %, otherwise the run is invalid.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use gorca_bench_b2::wire::{BusConn, ERROR, Enc, METHOD_CALL, METHOD_RETURN, Parse, SIGNAL, parse};

use crate::limits::MSG_CROSSCHECK_TOL;

const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const STATS_IFACE: &str = "org.freedesktop.DBus.Debug.Stats";

// ---- a{sv} reply parsing -----------------------------------------------------------------------

fn align(at: &mut usize, n: usize) {
    *at = (*at + n - 1) & !(n - 1);
}

fn take<'a>(b: &'a [u8], at: &mut usize, n: usize) -> Result<&'a [u8]> {
    let s = b.get(*at..*at + n).context("truncated reply")?;
    *at += n;
    Ok(s)
}

fn rd_u32(b: &[u8], at: &mut usize) -> Result<u32> {
    align(at, 4);
    Ok(u32::from_le_bytes(take(b, at, 4)?.try_into()?))
}

/// Decode `a{sv}` whose values are integers or strings into numbers (strings are skipped).
/// The body must be little-endian and start at an 8-aligned offset (a method-return body does).
pub fn parse_a_sv_numbers(b: &[u8]) -> Result<BTreeMap<String, u64>> {
    let mut at = 0usize;
    let len = rd_u32(b, &mut at)? as usize;
    align(&mut at, 8);
    let end = at + len;
    if end > b.len() {
        bail!("array longer than the body");
    }
    let mut out = BTreeMap::new();
    while at < end {
        align(&mut at, 8);
        let klen = rd_u32(b, &mut at)? as usize;
        let key = String::from_utf8_lossy(take(b, &mut at, klen)?).into_owned();
        take(b, &mut at, 1)?;
        let slen = take(b, &mut at, 1)?[0] as usize;
        let sig = take(b, &mut at, slen)?.to_vec();
        take(b, &mut at, 1)?;
        let val = match sig.as_slice() {
            b"u" => Some(u64::from(rd_u32(b, &mut at)?)),
            b"i" => Some(u64::from(rd_u32(b, &mut at)?)),
            b"t" | b"x" => {
                align(&mut at, 8);
                Some(u64::from_le_bytes(take(b, &mut at, 8)?.try_into()?))
            }
            b"b" => Some(u64::from(rd_u32(b, &mut at)?)),
            b"s" | b"o" => {
                let n = rd_u32(b, &mut at)? as usize;
                take(b, &mut at, n + 1)?;
                None
            }
            other => bail!(
                "unsupported variant type {:?}",
                String::from_utf8_lossy(other)
            ),
        };
        if let Some(v) = val {
            out.insert(key, v);
        }
    }
    Ok(out)
}

/// Is `org.freedesktop.DBus.Debug.Stats` there? (Answered from the bus, never assumed.)
/// Note it only offers gauges, see the module doc.
pub fn stats_interface_available(conn: &mut BusConn, timeout: Duration) -> bool {
    conn.call(DBUS, DBUS_PATH, STATS_IFACE, "GetStats", None, &[], timeout)
        .is_ok()
}

/// `GetConnectionStats(name)`, every integer key as the daemon reports it.
pub fn connection_stats_raw(
    conn: &mut BusConn,
    name: &str,
    timeout: Duration,
) -> Result<BTreeMap<String, u64>> {
    let mut e = Enc::default();
    e.string(name);
    let body = conn.call(
        DBUS,
        DBUS_PATH,
        STATS_IFACE,
        "GetConnectionStats",
        Some("s"),
        &e.buf,
        timeout,
    )?;
    parse_a_sv_numbers(&body)
}

// ---- monitor -----------------------------------------------------------------------------------

/// Counts by message type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MonitorCounts {
    pub calls: u64,
    pub replies: u64,
    pub errors: u64,
    pub signals: u64,
    pub bytes: u64,
    /// `interface.member` -> count (calls and signals).
    pub by_member: BTreeMap<String, u64>,
    /// Method calls by destination as written in the header (unique or well-known name).
    pub calls_to: BTreeMap<String, u64>,
    /// Method calls by sender (unique name).
    pub calls_from: BTreeMap<String, u64>,
    /// Signals by sender (unique name).
    pub signals_from: BTreeMap<String, u64>,
}

impl MonitorCounts {
    pub fn total(&self) -> u64 {
        self.calls + self.replies + self.errors + self.signals
    }

    fn merge(&mut self, o: &MonitorCounts) {
        self.calls += o.calls;
        self.replies += o.replies;
        self.errors += o.errors;
        self.signals += o.signals;
        self.bytes += o.bytes;
        for (dst, src) in [
            (&mut self.by_member, &o.by_member),
            (&mut self.calls_to, &o.calls_to),
            (&mut self.calls_from, &o.calls_from),
            (&mut self.signals_from, &o.signals_from),
        ] {
            for (k, v) in src {
                *dst.entry(k.clone()).or_default() += v;
            }
        }
    }
}

fn bump(map: &mut BTreeMap<String, u64>, key: &str) {
    if let Some(v) = map.get_mut(key) {
        *v += 1;
    } else {
        map.insert(key.to_owned(), 1);
    }
}

/// A passive observer on the private bus: `BecomeMonitor` with the given match rules (empty =
/// everything). With gorca's own declared rules it receives exactly the signals gorca receives.
#[derive(Debug)]
pub struct Monitor {
    counts: Arc<Mutex<MonitorCounts>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Monitor {
    pub fn start(address: &str, rules: &[&str]) -> Result<Self> {
        let mut conn = BusConn::connect(address, Duration::from_secs(5))?;
        let mut e = Enc::default();
        // as: byte length, then each string (4-aligned), then the flags word.
        let lenpos = e.buf.len();
        e.u32(0);
        let start = e.buf.len();
        for r in rules {
            e.string(r);
        }
        let n = (e.buf.len() - start) as u32;
        e.buf[lenpos..lenpos + 4].copy_from_slice(&n.to_le_bytes());
        e.u32(0);
        conn.call(
            DBUS,
            DBUS_PATH,
            "org.freedesktop.DBus.Monitoring",
            "BecomeMonitor",
            Some("asu"),
            &e.buf,
            Duration::from_secs(5),
        )
        .context("BecomeMonitor")?;
        let counts = Arc::new(Mutex::new(MonitorCounts::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (c2, s2) = (counts.clone(), stop.clone());
        let thread = std::thread::Builder::new()
            .name("hx-b1-monitor".into())
            .spawn(move || monitor_loop(conn, &c2, &s2))
            .context("spawn monitor")?;
        Ok(Self {
            counts,
            stop,
            thread: Some(thread),
        })
    }

    pub fn snapshot(&self) -> MonitorCounts {
        self.counts.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn stop(mut self) -> MonitorCounts {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.snapshot()
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn monitor_loop(mut conn: BusConn, counts: &Mutex<MonitorCounts>, stop: &AtomicBool) {
    let mut buf = std::mem::take(&mut conn.leftover);
    let mut chunk = vec![0u8; 256 * 1024];
    let me = conn.unique.clone();
    let _ = conn
        .stream
        .set_read_timeout(Some(Duration::from_millis(50)));
    let mut key = String::new();
    loop {
        // Count a whole read chunk locally, then merge once: the lock is not on the per-message path.
        let mut local = MonitorCounts::default();
        let mut consumed = 0usize;
        let mut bad = false;
        loop {
            match parse(&buf[consumed..]) {
                Parse::Msg(m, used) => {
                    consumed += used;
                    // Messages addressed to the monitor itself (NameLost, ...) are not traffic.
                    if m.destination == Some(me.as_str()) {
                        continue;
                    }
                    local.bytes += used as u64;
                    match m.mtype {
                        METHOD_CALL => {
                            local.calls += 1;
                            bump(&mut local.calls_to, m.destination.unwrap_or_default());
                            bump(&mut local.calls_from, m.sender.unwrap_or_default());
                        }
                        METHOD_RETURN => local.replies += 1,
                        ERROR => local.errors += 1,
                        SIGNAL => {
                            local.signals += 1;
                            bump(&mut local.signals_from, m.sender.unwrap_or_default());
                        }
                        _ => {}
                    }
                    if matches!(m.mtype, METHOD_CALL | SIGNAL) {
                        key.clear();
                        key.push_str(m.interface.unwrap_or_default());
                        key.push('.');
                        key.push_str(m.member.unwrap_or_default());
                        bump(&mut local.by_member, &key);
                    }
                }
                Parse::NeedMore => break,
                Parse::Bad => {
                    bad = true;
                    break;
                }
            }
        }
        buf.drain(..consumed);
        if local.total() > 0
            && let Ok(mut c) = counts.lock()
        {
            c.merge(&local);
        }
        if bad || stop.load(Ordering::Relaxed) {
            return;
        }
        match conn.stream.read(&mut chunk) {
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
}

// ---- accounting --------------------------------------------------------------------------------

/// Counters of one run window, from the three independent sources.
#[derive(Clone, Debug, Default)]
pub struct Accounting {
    /// Method calls served, per `(app, member)` (fake-app counters).
    pub calls: BTreeMap<(String, String), u64>,
    /// Signals gorca's drain reports as received (`DrainStats.messages`).
    pub signals_received: u64,
    /// Signals the monitor saw under gorca's match rules.
    pub signals_monitored: u64,
    pub window_s: f64,
}

impl Accounting {
    pub fn total_calls(&self) -> u64 {
        self.calls.values().sum()
    }

    pub fn calls_of(&self, app: &str) -> u64 {
        self.calls
            .iter()
            .filter(|((a, _), _)| a == app)
            .map(|(_, v)| *v)
            .sum()
    }

    /// SPEC 6.1: `2 * calls + signals_received`.
    pub fn msgs(&self) -> u64 {
        2 * self.total_calls() + self.signals_received
    }

    pub fn msgs_per_s(&self) -> f64 {
        per_s(self.msgs(), self.window_s)
    }

    pub fn calls_per_s(&self, app: &str) -> f64 {
        per_s(self.calls_of(app), self.window_s)
    }

    /// `Err` when the drain count and the monitor disagree by more than 1 % (the run is invalid).
    pub fn cross_check(&self) -> Result<f64, CrossCheckError> {
        let (a, b) = (self.signals_received as f64, self.signals_monitored as f64);
        let denom = a.max(b).max(1.0);
        let rel = (a - b).abs() / denom;
        if rel > MSG_CROSSCHECK_TOL {
            Err(CrossCheckError {
                drain: self.signals_received,
                monitor: self.signals_monitored,
                rel,
            })
        } else {
            Ok(rel)
        }
    }
}

#[derive(Clone, Debug)]
pub struct CrossCheckError {
    pub drain: u64,
    pub monitor: u64,
    pub rel: f64,
}

impl std::fmt::Display for CrossCheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "signal counts disagree: drain {} vs monitor {} ({:.2} % > {:.0} %)",
            self.drain,
            self.monitor,
            self.rel * 100.0,
            MSG_CROSSCHECK_TOL * 100.0
        )
    }
}

impl std::error::Error for CrossCheckError {}

fn per_s(n: u64, window_s: f64) -> f64 {
    if window_s > 0.0 {
        n as f64 / window_s
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msgs_formula_and_rates() {
        let mut a = Accounting {
            signals_received: 100,
            signals_monitored: 100,
            window_s: 10.0,
            ..Accounting::default()
        };
        a.calls.insert(("settings".into(), "GetName".into()), 30);
        a.calls.insert(("storm".into(), "GetName".into()), 20);
        assert_eq!(a.msgs(), 2 * 50 + 100);
        assert!((a.msgs_per_s() - 20.0).abs() < 1e-9);
        assert!((a.calls_per_s("storm") - 2.0).abs() < 1e-9);
    }

    #[test]
    fn cross_check_tolerance_is_one_percent_of_the_larger() {
        let ok = Accounting {
            signals_received: 1000,
            signals_monitored: 1009,
            ..Accounting::default()
        };
        assert!(ok.cross_check().is_ok());
        let bad = Accounting {
            signals_received: 1000,
            signals_monitored: 1020,
            ..Accounting::default()
        };
        assert!(bad.cross_check().is_err());
        assert!(
            Accounting::default().cross_check().is_ok(),
            "0 vs 0 is consistent"
        );
    }

    #[test]
    fn a_sv_parser_reads_integers_and_skips_strings() {
        // a{sv} { "IncomingMessages": u 7, "Name": s "x", "OutgoingBytes": t 9 }
        let mut e = Enc::default();
        e.u32(0);
        e.align(8);
        let start = e.buf.len();
        e.align(8);
        e.string("IncomingMessages");
        e.sig("u");
        e.u32(7);
        e.align(8);
        e.string("Name");
        e.sig("s");
        e.string("x");
        e.align(8);
        e.string("OutgoingBytes");
        e.sig("t");
        e.u64(9);
        let n = (e.buf.len() - start) as u32;
        e.buf[0..4].copy_from_slice(&n.to_le_bytes());
        let m = parse_a_sv_numbers(&e.buf).unwrap();
        assert_eq!(m.get("IncomingMessages"), Some(&7));
        assert_eq!(m.get("OutgoingBytes"), Some(&9));
        assert!(!m.contains_key("Name"));
        assert!(parse_a_sv_numbers(&e.buf[..10]).is_err());
    }
}
