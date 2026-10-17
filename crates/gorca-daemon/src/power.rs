//! Battery state from UPower's display device (`org.freedesktop.UPower`), read-only, on the
//! system bus. Opened per request: the command is rare, so no connection is kept open, and
//! every await runs under the core's own deadline (`gorca_core::BATTERY_DEADLINE`) plus the
//! per-call bound below.

use std::time::Duration;

use gorca_core::{BatteryState, PowerInfo, PowerSource};
use gorca_rt::BoxFuture;

const SYSTEM_BUS: &str = "unix:path=/run/dbus/system_bus_socket";
const DEST: &str = "org.freedesktop.UPower";
const PATH: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
const IFACE: &str = "org.freedesktop.UPower.Device";
/// Each D-Bus call is bounded on its own, inside the core's overall deadline.
const CALL: Duration = Duration::from_millis(200);

/// UPower `State` property (`up-types.h`): 1 charging, 2 discharging, 3 empty, 4 fully charged,
/// 5 pending charge, 6 pending discharge.
pub(crate) fn state_of(code: u32) -> BatteryState {
    match code {
        1 => BatteryState::Charging,
        2 | 6 => BatteryState::Discharging,
        3 => BatteryState::Empty,
        4 => BatteryState::FullyCharged,
        5 => BatteryState::PendingCharge,
        _ => BatteryState::Unknown,
    }
}

/// UPower over the system bus.
#[derive(Debug, Default)]
pub(crate) struct UPower;

async fn read_once() -> Option<PowerInfo> {
    // The standard system bus socket, by explicit address (the environment is read only in
    // `main.rs`, xtask rule 8); UPower lives on the system bus only.
    let builder = zbus::connection::Builder::address(SYSTEM_BUS).ok()?;
    let conn = tokio::time::timeout(CALL, builder.build())
        .await
        .ok()?
        .ok()?;
    let proxy = tokio::time::timeout(CALL, zbus::Proxy::new(&conn, DEST, PATH, IFACE))
        .await
        .ok()?
        .ok()?;
    let present: bool = tokio::time::timeout(CALL, proxy.get_property("IsPresent"))
        .await
        .ok()?
        .ok()?;
    if !present {
        return Some(PowerInfo {
            present: false,
            percent: 0,
            state: BatteryState::Unknown,
            secs_left: None,
        });
    }
    let percent: f64 = tokio::time::timeout(CALL, proxy.get_property("Percentage"))
        .await
        .ok()?
        .ok()?;
    let code: u32 = tokio::time::timeout(CALL, proxy.get_property("State"))
        .await
        .ok()?
        .ok()?;
    let state = state_of(code);
    // The time property that matches the state; 0 means "unknown".
    let prop = match state {
        BatteryState::Charging => Some("TimeToFull"),
        BatteryState::Discharging => Some("TimeToEmpty"),
        _ => None,
    };
    let mut secs_left = None;
    if let Some(p) = prop {
        let secs: Option<i64> = match tokio::time::timeout(CALL, proxy.get_property(p)).await {
            Ok(Ok(v)) => Some(v),
            Ok(Err(_)) | Err(_) => None,
        };
        secs_left = secs.and_then(|s| u64::try_from(s).ok()).filter(|s| *s > 0);
    }
    Some(PowerInfo {
        present: true,
        percent: to_percent(percent),
        state,
        secs_left,
    })
}

/// UPower's percentage (0.0 to 100.0) as a whole number.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn to_percent(p: f64) -> u8 {
    p.round().clamp(0.0, 100.0) as u8
}

impl PowerSource for UPower {
    fn read(&self) -> BoxFuture<'static, Option<PowerInfo>> {
        Box::pin(read_once())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upower_state_codes_map() {
        assert_eq!(state_of(1), BatteryState::Charging);
        assert_eq!(state_of(2), BatteryState::Discharging);
        assert_eq!(state_of(4), BatteryState::FullyCharged);
        assert_eq!(state_of(0), BatteryState::Unknown);
    }
}
