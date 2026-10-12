//! `org.freedesktop.a11y.KeyboardMonitor` client: name ownership (ID-1) and the `MonitorState`
//! state machine.
//!
//! Responsibility (ADR-0001 section 11): "org.freedesktop.a11y.KeyboardMonitor client, name
//! ownership (ID-1), `MonitorState` SM". Sections 5.5 and 7.3, platform contract in
//! `docs/platform/org.freedesktop.a11y.KeyboardMonitor.md`.
//!
//! > "F-1: Mutter authorises `org.freedesktop.a11y.KeyboardMonitor` only for the owner of
//! > `org.gnome.Orca.KeyboardMonitor` ... F-2: `KeyEvent` is a unicast signal with no
//! > acknowledgement."
//! > "K-4: Every keyboard-monitor call is async with a deadline."
//! > "K-5: The monitor uses its own session-bus connection with one match rule, drained on the
//! > input lane."
//!
//! Phase 0 contains the pure state machine ([`MonitorState::step`], exhaustive, no wildcard arm)
//! and the interface constants. TBD: the zbus tasks that perform the calls, the `KeyEvent`
//! decoder, and the `KeySource` implementation that wires effects to the input lane.
//!
//! Allowed dependencies: `gorca-input-api`, `gorca-rt`, `gorca-model`, and `zbus` (rule 2 of
//! section 11 allows it in this crate).
#![deny(clippy::wildcard_enum_match_arm)]

mod monitor;

pub use monitor::{
    AUTHORIZE_DEADLINE, GRAB_DEADLINE, GRAB_RETRIES, GrabSetState, MonitorEffect, MonitorMsg,
    MonitorState, Notice, RequestNameReply, SETTLE, StepCtx, lost_backoff,
};

/// Well-known name that Mutter allow-lists (ID-1).
pub const ORCA_MONITOR_NAME: &str = "org.gnome.Orca.KeyboardMonitor";
/// Service that exports the interface.
pub const MANAGER_NAME: &str = "org.freedesktop.a11y.Manager";
pub const MANAGER_PATH: &str = "/org/freedesktop/a11y/Manager";
pub const MONITOR_INTERFACE: &str = "org.freedesktop.a11y.KeyboardMonitor";

/// The name gorca must own, validated by zbus.
pub fn orca_monitor_name() -> Result<zbus::names::WellKnownName<'static>, zbus::names::Error> {
    zbus::names::WellKnownName::from_static_str(ORCA_MONITOR_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_are_valid_dbus_names() {
        assert_eq!(orca_monitor_name().unwrap().as_str(), ORCA_MONITOR_NAME);
        assert!(zbus::names::WellKnownName::try_from(MANAGER_NAME).is_ok());
        assert!(zbus::names::InterfaceName::try_from(MONITOR_INTERFACE).is_ok());
        assert!(zbus::zvariant::ObjectPath::try_from(MANAGER_PATH).is_ok());
    }
}
