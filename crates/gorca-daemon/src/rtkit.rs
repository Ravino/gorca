//! Client of `org.freedesktop.RealtimeKit1` (rtkit) for the hot lanes of runtime-topology arm
//! (c), following the order PipeWire uses (P-12 report, section 2): the caller sets
//! `RLIMIT_RTTIME` first (rtkit refuses an unlimited process), clamps the wanted priority to the
//! daemon's `MaxRealtimePriority`, asks for a real-time class (rtkit grants `SCHED_RR`, not FIFO) and, when that is refused, for a raised
//! nice level.
//!
//! Every call has a deadline (D-4). The connection lives on one helper thread with a private
//! current-thread runtime, so lane threads (which have no async context of their own at start)
//! make a blocking request with a bounded wait and never touch D-Bus themselves.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use gorca_dbus::call_method;
use gorca_rt::Deadline;
use zbus::zvariant::OwnedValue;

/// The system bus when the caller (the daemon's `main`, which alone reads the environment) knows
/// no better.
const DEFAULT_SYSTEM_BUS: &str = "unix:path=/run/dbus/system_bus_socket";
const DEST: &str = "org.freedesktop.RealtimeKit1";
const PATH: &str = "/org/freedesktop/RealtimeKit1";
const PROPS: &str = "org.freedesktop.DBus.Properties";
/// Per-call deadline of the helper.
const CALL_DEADLINE: Duration = Duration::from_millis(800);

/// What rtkit allows this process (properties of the daemon).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_realtime_priority: i32,
    pub min_nice_level: i32,
    pub rttime_usec_max: i64,
}

enum Req {
    Realtime {
        tid: u32,
        priority: u32,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    HighPriority {
        tid: u32,
        nice: i32,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
}

/// Handle to the helper thread. Dropping it ends the thread.
#[derive(Debug)]
pub struct Rtkit {
    tx: mpsc::Sender<Req>,
    limits: Limits,
}

impl std::fmt::Debug for Req {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Req")
    }
}

impl Rtkit {
    /// Connect to the system bus (at `address`; the default system socket when `None`) and read rtkit's limits, all within `timeout`.
    pub fn connect(address: Option<&str>, timeout: Duration) -> Result<Self, String> {
        let address = address.map(str::to_owned);
        let (tx, rx) = mpsc::channel::<Req>();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<Limits, String>>(1);
        std::thread::Builder::new()
            .name("gorca-rtkit".to_owned())
            .spawn(move || helper(address.as_deref(), timeout, &rx, &ready_tx))
            .map_err(|e| format!("spawning the rtkit helper: {e}"))?;
        let limits = ready_rx
            .recv_timeout(timeout + Duration::from_millis(500))
            .map_err(|_| "the rtkit helper did not answer in time".to_owned())??;
        Ok(Self { tx, limits })
    }

    #[must_use]
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// `MakeThreadRealtime(tid, priority)`, priority clamped to `MaxRealtimePriority`. Returns
    /// the priority granted.
    pub fn make_realtime(&self, tid: u32, priority: u8, wait: Duration) -> Result<u8, String> {
        let granted = i32::from(priority).min(self.limits.max_realtime_priority);
        let granted_u8 = u8::try_from(granted).map_err(|_| "no realtime priority".to_owned())?;
        if granted_u8 == 0 {
            return Err("rtkit allows no realtime priority".to_owned());
        }
        let (reply, rx) = mpsc::sync_channel(1);
        self.tx
            .send(Req::Realtime {
                tid,
                priority: u32::from(granted_u8),
                reply,
            })
            .map_err(|_| "the rtkit helper is gone".to_owned())?;
        rx.recv_timeout(wait)
            .map_err(|_| "rtkit did not answer in time".to_owned())??;
        Ok(granted_u8)
    }

    /// `MakeThreadHighPriority(tid, nice)`, nice clamped to `MinNiceLevel`. Returns the nice
    /// level granted.
    pub fn make_high_priority(&self, tid: u32, nice: i32, wait: Duration) -> Result<i32, String> {
        let granted = nice.max(self.limits.min_nice_level);
        let (reply, rx) = mpsc::sync_channel(1);
        self.tx
            .send(Req::HighPriority {
                tid,
                nice: granted,
                reply,
            })
            .map_err(|_| "the rtkit helper is gone".to_owned())?;
        rx.recv_timeout(wait)
            .map_err(|_| "rtkit did not answer in time".to_owned())??;
        Ok(granted)
    }
}

fn deadline() -> Deadline {
    Deadline(Instant::now() + CALL_DEADLINE)
}

fn helper(
    address: Option<&str>,
    timeout: Duration,
    rx: &mpsc::Receiver<Req>,
    ready: &mpsc::SyncSender<Result<Limits, String>>,
) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = ready.send(Err(format!("rtkit runtime: {e}")));
            return;
        }
    };
    let conn = match rt.block_on(connect_and_read(address, timeout)) {
        Ok((conn, limits)) => {
            let _ = ready.send(Ok(limits));
            conn
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    while let Ok(req) = rx.recv() {
        match req {
            Req::Realtime {
                tid,
                priority,
                reply,
            } => {
                let r = rt.block_on(call(
                    &conn,
                    "MakeThreadRealtime",
                    &(u64::from(tid), priority),
                ));
                let _ = reply.send(r);
            }
            Req::HighPriority { tid, nice, reply } => {
                let r = rt.block_on(call(
                    &conn,
                    "MakeThreadHighPriority",
                    &(u64::from(tid), nice),
                ));
                let _ = reply.send(r);
            }
        }
    }
}

async fn connect_and_read(
    address: Option<&str>,
    timeout: Duration,
) -> Result<(zbus::Connection, Limits), String> {
    let builder = zbus::connection::Builder::address(address.unwrap_or(DEFAULT_SYSTEM_BUS))
        .map_err(|e| format!("system bus address: {e}"))?;
    let conn = tokio::time::timeout(timeout, builder.build())
        .await
        .map_err(|_| "connecting to the system bus timed out".to_owned())?
        .map_err(|e| format!("system bus: {e}"))?;
    let max_realtime_priority = prop::<i32>(&conn, "MaxRealtimePriority").await?;
    let min_nice_level = prop::<i32>(&conn, "MinNiceLevel").await?;
    let rttime_usec_max = prop::<i64>(&conn, "RTTimeUSecMax").await?;
    Ok((
        conn,
        Limits {
            max_realtime_priority,
            min_nice_level,
            rttime_usec_max,
        },
    ))
}

async fn prop<T: TryFrom<OwnedValue>>(conn: &zbus::Connection, name: &str) -> Result<T, String> {
    let m = call_method(conn, deadline(), DEST, PATH, PROPS, "Get", &(DEST, name))
        .await
        .map_err(|e| format!("reading {name}: {e}"))?;
    let v: OwnedValue = m
        .body()
        .deserialize()
        .map_err(|e| format!("decoding {name}: {e}"))?;
    T::try_from(v).map_err(|_| format!("{name} has an unexpected type"))
}

async fn call<B>(conn: &zbus::Connection, member: &str, body: &B) -> Result<(), String>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    call_method(conn, deadline(), DEST, PATH, DEST, member, body)
        .await
        .map(drop)
        .map_err(|e| format!("{member}: {e}"))
}
