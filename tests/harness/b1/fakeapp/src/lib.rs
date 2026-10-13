//! `b1-fakeapp`: the fake AT-SPI application of bake-off B-1 (SPEC 8.1, H-2).
//!
//! One binary, many models (`../models/*.json`). The tree, its mutations and the truth log are
//! [`gorca_testkit::b1::world::World`]; this crate serves it over D-Bus:
//!
//! * `Accessible`, `Component`, `Value`, `Text` (minimal), `Action` (count only), `Application`,
//!   `Cache.GetItems` and `org.freedesktop.DBus.Properties` on **any** path, answered by a
//!   hand-written dispatcher (no zbus object server: paths are dynamic and replies must be
//!   delayed, dropped or reordered on command);
//! * `Application.GetApplicationBusAddress` in the three modes of SPEC D-7 (`peer`, `same_bus`,
//!   `none`); `peer` is a real unix socket speaking the same dispatcher;
//! * a **serial service-time model** (D-1): one thread plays the toolkit main loop. Every call,
//!   from any connection, and every control command waits in one FIFO and occupies the thread for
//!   a log-normal service time (plus a per-item cost for `GetItems`);
//! * signals for every evented mutation, `Cache.AddAccessible` / `RemoveAccessible` per quirk
//!   profile;
//! * call counters per member and connection, and the truth log (SPEC 6.1);
//! * a control channel: JSON lines on stdin, answers on stdout (see `main.rs`).
//!
//! Isolation: the binary refuses to connect to a bus address that does not belong to a harness
//! session unless `--allow-any-bus` is given (tests of this crate use their own dbus-daemon).

#![allow(unsafe_code)] // only `mono`, with a SAFETY comment

pub mod dispatch;
pub mod mono;
pub mod server;

pub use server::{FakeApp, Options, Ready};
