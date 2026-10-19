// SPDX-License-Identifier: GPL-3.0-or-later
//! Wire format of `org.gorca.ShellHelper` v2 (`docs/platform/org.gorca.ShellHelper.md`): request
//! bodies, `not-after`, reply decoding and the mapping of helper errors.

use std::collections::HashMap;
use std::time::Instant;

use gorca_dbus::DbusError;
use gorca_output_api::{
    Button, ClickCount, ClickOpts, IfCovered, OutcomeKind, PointerError, PointerOutcome, PointerPos,
};
use gorca_rt::{Deadline, monotonic_ns};
use zbus::zvariant::{OwnedValue, Value};

pub const HELPER_NAME: &str = "org.gorca.ShellHelper";
pub const HELPER_PATH: &str = "/org/gorca/ShellHelper";
pub const HELPER_INTERFACE: &str = "org.gorca.ShellHelper";
/// MI-7: the helper is trusted only if its owner is the owner of this name.
pub const SHELL_NAME: &str = "org.gnome.Shell";
const ERROR_PREFIX: &str = "org.gorca.ShellHelper.Error.";

/// Which rectangle of the window the coordinates are relative to, resolved inside the compositor
/// at call time (ADR-0025 2.2).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Origin {
    Frame,
    Buffer,
    Stage,
}

impl Origin {
    pub const fn wire(self) -> &'static str {
        match self {
            Origin::Frame => "frame",
            Origin::Buffer => "buffer",
            Origin::Stage => "stage",
        }
    }
}

/// A point as the helper takes it: a Meta window id (0 = stage space) and an origin.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct WireTarget {
    pub window: u64,
    pub origin: Origin,
    pub x: f64,
    pub y: f64,
}

impl WireTarget {
    pub const fn stage(x: f64, y: f64) -> Self {
        Self {
            window: 0,
            origin: Origin::Stage,
            x,
            y,
        }
    }
}

/// `not-after` for a call that must run by `deadline`: CLOCK_MONOTONIC microseconds, the clock of
/// `GLib.get_monotonic_time()` in the Shell (ADR-0025 C-1). `Instant` is `CLOCK_MONOTONIC` on
/// Linux, so the remaining time is added to the current reading.
pub fn not_after_us(deadline: Deadline, now: Instant) -> i64 {
    let now_us = i64::try_from(monotonic_ns() / 1_000).unwrap_or(i64::MAX);
    let remain = i64::try_from(deadline.remaining(now).as_micros()).unwrap_or(i64::MAX);
    now_us.saturating_add(remain)
}

/// The `a{sv}` options of an injecting call.
pub fn opts_body(not_after: i64, opts: ClickOpts) -> HashMap<&'static str, Value<'static>> {
    let mut m: HashMap<&'static str, Value<'static>> = HashMap::new();
    m.insert("not-after", Value::from(not_after));
    match opts.if_covered {
        IfCovered::Refuse => {}
        IfCovered::Proceed => {
            m.insert("if-occluded", Value::from("proceed"));
        }
    }
    match opts.count {
        ClickCount::One => {}
        ClickCount::Two => {
            m.insert("count", Value::from(2_u32));
        }
    }
    m
}

pub const fn button_arg(b: Button) -> u32 {
    b.wire()
}

/// Map an error reply of the helper to a [`PointerError`]. `name` is the D-Bus error name,
/// `msg` the description (`Error.Failed` carries a machine prefix: `locked:` `rate:` ...).
pub fn map_remote_error(name: &str, msg: &str) -> PointerError {
    let short = name.strip_prefix(ERROR_PREFIX).unwrap_or(name);
    match short {
        "AccessDenied" => PointerError::Denied,
        "Failed" => match msg.split_once(':').map(|(p, _)| p) {
            Some("locked") => PointerError::Locked,
            Some("rate") => PointerError::RateLimited,
            Some("busy") => PointerError::Busy,
            Some("no-window") => PointerError::NoWindow,
            Some("bad-args") => PointerError::BadArgs(msg.to_owned()),
            Some(_) | None => PointerError::Failed(msg.to_owned()),
        },
        _ => PointerError::Failed(format!("{name}: {msg}")),
    }
}

/// Map a failed call (transport, deadline or remote error).
pub fn map_dbus_error(e: &DbusError) -> PointerError {
    match e {
        DbusError::Timeout => PointerError::Timeout,
        DbusError::Zbus(zbus::Error::MethodError(name, desc, _)) => {
            map_remote_error(name.as_str(), desc.as_deref().unwrap_or(""))
        }
        DbusError::Zbus(other) => PointerError::Failed(other.to_string()),
    }
}

/// Decode the `a{sv}` reply of an injecting call.
#[allow(clippy::implicit_hasher)] // the reply type is zbus's default-hasher map
pub fn decode_outcome(body: &HashMap<String, OwnedValue>) -> Result<PointerOutcome, PointerError> {
    let kind = match str_of(body, "outcome")?.as_str() {
        "done" => OutcomeKind::Done,
        "expired" => OutcomeKind::Expired,
        "refused-occluded" => OutcomeKind::RefusedOccluded,
        "refused-outside" => OutcomeKind::RefusedOutside,
        other => return Err(PointerError::Failed(format!("unknown outcome {other:?}"))),
    };
    Ok(PointerOutcome {
        kind,
        x: f64_of(body, "x")?,
        y: f64_of(body, "y")?,
        window_at: u64_of(body, "window-at")?,
        occluded: bool_of(body, "occluded")?,
        clamped: bool_of(body, "clamped")?,
        held: u32_of(body, "held")?,
    })
}

/// Decode `GetPointer`.
#[allow(clippy::implicit_hasher)] // the reply type is zbus's default-hasher map
pub fn decode_pos(body: &HashMap<String, OwnedValue>) -> Result<PointerPos, PointerError> {
    Ok(PointerPos {
        x: f64_of(body, "x")?,
        y: f64_of(body, "y")?,
        window: u64_of(body, "window")?,
        wx: f64_of(body, "wx")?,
        wy: f64_of(body, "wy")?,
        monitor: i32_of(body, "monitor")?,
    })
}

fn missing(key: &str) -> PointerError {
    PointerError::Failed(format!("reply lacks or mistypes {key:?}"))
}

fn get<'a>(
    body: &'a HashMap<String, OwnedValue>,
    key: &str,
) -> Result<&'a Value<'static>, PointerError> {
    body.get(key).map(|v| &**v).ok_or_else(|| missing(key))
}

fn f64_of(body: &HashMap<String, OwnedValue>, key: &str) -> Result<f64, PointerError> {
    get(body, key)?
        .downcast_ref::<f64>()
        .map_err(|_| missing(key))
}

fn u64_of(body: &HashMap<String, OwnedValue>, key: &str) -> Result<u64, PointerError> {
    get(body, key)?
        .downcast_ref::<u64>()
        .map_err(|_| missing(key))
}

fn u32_of(body: &HashMap<String, OwnedValue>, key: &str) -> Result<u32, PointerError> {
    get(body, key)?
        .downcast_ref::<u32>()
        .map_err(|_| missing(key))
}

fn i32_of(body: &HashMap<String, OwnedValue>, key: &str) -> Result<i32, PointerError> {
    get(body, key)?
        .downcast_ref::<i32>()
        .map_err(|_| missing(key))
}

fn bool_of(body: &HashMap<String, OwnedValue>, key: &str) -> Result<bool, PointerError> {
    get(body, key)?
        .downcast_ref::<bool>()
        .map_err(|_| missing(key))
}

fn str_of(body: &HashMap<String, OwnedValue>, key: &str) -> Result<String, PointerError> {
    get(body, key)?
        .downcast_ref::<&str>()
        .map(str::to_owned)
        .map_err(|_| missing(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn ov<'a, T: Into<Value<'a>>>(v: T) -> OwnedValue {
        OwnedValue::try_from(v.into()).unwrap()
    }

    fn reply(outcome: &str) -> HashMap<String, OwnedValue> {
        let mut m = HashMap::new();
        m.insert("x".to_owned(), ov(10.5_f64));
        m.insert("y".to_owned(), ov(20.0_f64));
        m.insert("window-at".to_owned(), ov(77_u64));
        m.insert("occluded".to_owned(), ov(false));
        m.insert("clamped".to_owned(), ov(true));
        m.insert("outcome".to_owned(), ov(outcome));
        m.insert("held".to_owned(), ov(5_u32));
        m
    }

    #[test]
    fn outcome_decodes_every_kind() {
        for (s, k) in [
            ("done", OutcomeKind::Done),
            ("expired", OutcomeKind::Expired),
            ("refused-occluded", OutcomeKind::RefusedOccluded),
            ("refused-outside", OutcomeKind::RefusedOutside),
        ] {
            let o = decode_outcome(&reply(s)).unwrap();
            assert_eq!(o.kind, k);
            assert_eq!((o.x, o.y, o.window_at, o.held), (10.5, 20.0, 77, 5));
            assert!(o.clamped && !o.occluded);
        }
    }

    #[test]
    fn a_bad_reply_is_an_error_not_a_panic() {
        assert!(matches!(
            decode_outcome(&reply("weird")),
            Err(PointerError::Failed(_))
        ));
        let mut m = reply("done");
        m.remove("held");
        assert!(decode_outcome(&m).is_err());
        let mut m = reply("done");
        m.insert("x".to_owned(), ov("not a number"));
        assert!(decode_outcome(&m).is_err());
    }

    #[test]
    fn pos_decodes() {
        let mut m = HashMap::new();
        m.insert("x".to_owned(), ov(1.0_f64));
        m.insert("y".to_owned(), ov(2.0_f64));
        m.insert("window".to_owned(), ov(9_u64));
        m.insert("wx".to_owned(), ov(0.5_f64));
        m.insert("wy".to_owned(), ov(0.25_f64));
        m.insert("monitor".to_owned(), ov(1_i32));
        let p = decode_pos(&m).unwrap();
        assert_eq!((p.window, p.monitor), (9, 1));
    }

    #[test]
    fn remote_errors_map_by_prefix() {
        let f = "org.gorca.ShellHelper.Error.Failed";
        assert_eq!(
            map_remote_error(f, "locked: session is locked"),
            PointerError::Locked
        );
        assert_eq!(
            map_remote_error(f, "rate: slow down"),
            PointerError::RateLimited
        );
        assert_eq!(
            map_remote_error(f, "busy: another caller"),
            PointerError::Busy
        );
        assert_eq!(map_remote_error(f, "no-window: 5"), PointerError::NoWindow);
        assert!(matches!(
            map_remote_error(f, "bad-args: x"),
            PointerError::BadArgs(_)
        ));
        assert!(matches!(
            map_remote_error(f, "something"),
            PointerError::Failed(_)
        ));
        assert_eq!(
            map_remote_error("org.gorca.ShellHelper.Error.AccessDenied", "no"),
            PointerError::Denied
        );
        assert!(matches!(
            map_remote_error("org.freedesktop.DBus.Error.UnknownMethod", "x"),
            PointerError::Failed(_)
        ));
    }

    #[test]
    fn not_after_is_now_plus_the_remaining_time_on_the_monotonic_clock() {
        let now = Instant::now();
        let d = Deadline(now + Duration::from_millis(150));
        let a = not_after_us(d, now);
        let mono = i64::try_from(monotonic_ns() / 1_000).unwrap();
        let delta = a - mono;
        assert!((149_000..=151_000).contains(&delta), "{delta}");
        // already past: not after "now"
        let past = not_after_us(Deadline(now), now + Duration::from_secs(1));
        assert!(past <= i64::try_from(monotonic_ns() / 1_000).unwrap() + 1);
    }

    #[test]
    fn options_carry_only_what_differs_from_the_defaults() {
        let m = opts_body(5, ClickOpts::default());
        assert_eq!(m.len(), 1);
        let m = opts_body(
            5,
            ClickOpts {
                if_covered: IfCovered::Proceed,
                count: ClickCount::Two,
            },
        );
        assert_eq!(m.len(), 3);
    }
}
