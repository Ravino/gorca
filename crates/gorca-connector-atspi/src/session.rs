//! Bootstrap I/O: finding the a11y bus, connecting to it, ID-5 enablement and listener
//! registration. Every call goes through `gorca_dbus::call_method` with a deadline (R2).

use std::time::Duration;

use gorca_dbus::{DbusError, MAX_QUEUED, call_method, with_deadline};
use gorca_rt::{BudgetClass, Clock, Deadline};
use zbus::Connection;
use zbus::zvariant::{OwnedValue, Value};

use crate::enablement::StatusValues;

const BUS_NAME: &str = "org.a11y.Bus";
const BUS_PATH: &str = "/org/a11y/bus";
const STATUS_IFACE: &str = "org.a11y.Status";
const PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";
const REGISTRY_NAME: &str = "org.a11y.atspi.Registry";
const REGISTRY_PATH: &str = "/org/a11y/atspi/registry";
const REGISTRY_IFACE: &str = "org.a11y.atspi.Registry";

/// The event classes gorca asks every application to emit (`Registry.RegisterEvent`, D-5).
/// `StateChanged`, `ChildrenChanged` and the name/role/description property changes are always
/// on in atk-bridge and GTK4, so they are not listed. Wide registrations such as `Object:*`
/// would turn every application into an event firehose.
pub const REGISTERED_EVENTS: &[&str] = &[
    "Focus:",
    "Object:StateChanged:Focused",
    "Object:ActiveDescendantChanged:",
    "Object:Announcement:",
    "Window:Activate:",
    "Window:Deactivate:",
    "Window:Create:",
    "Window:Destroy:",
];

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("{0}")]
    Dbus(#[from] DbusError),
    #[error("unexpected reply from {what}: {detail}")]
    Reply { what: &'static str, detail: String },
}

fn reply_err(what: &'static str, e: &zbus::Error) -> SessionError {
    SessionError::Reply {
        what,
        detail: e.to_string(),
    }
}

/// Connect to a bus at `address` with our own configuration (D-6: `max_queued` >= 4096).
pub async fn connect(address: &str, deadline: Deadline) -> Result<Connection, SessionError> {
    let builder = zbus::connection::Builder::address(address)
        .map_err(|e| reply_err("bus address", &e))?
        .max_queued(MAX_QUEUED)
        .method_timeout(Duration::from_secs(2));
    Ok(with_deadline(deadline, builder.build()).await?)
}

/// The session bus of the environment (`DBUS_SESSION_BUS_ADDRESS`).
pub async fn connect_session(
    address: Option<&str>,
    deadline: Deadline,
) -> Result<Connection, SessionError> {
    if let Some(a) = address {
        return connect(a, deadline).await;
    }
    let builder = zbus::connection::Builder::session()
        .map_err(|e| reply_err("session bus", &e))?
        .method_timeout(Duration::from_secs(2));
    Ok(with_deadline(deadline, builder.build()).await?)
}

/// `org.a11y.Bus.GetAddress` on the session bus.
pub async fn a11y_bus_address(
    session: &Connection,
    deadline: Deadline,
) -> Result<String, SessionError> {
    let msg = call_method(
        session,
        deadline,
        BUS_NAME,
        BUS_PATH,
        "org.a11y.Bus",
        "GetAddress",
        &(),
    )
    .await?;
    msg.body()
        .deserialize::<String>()
        .map_err(|e| reply_err("GetAddress", &e))
}

async fn get_bool(
    session: &Connection,
    deadline: Deadline,
    prop: &'static str,
) -> Result<bool, SessionError> {
    let msg = call_method(
        session,
        deadline,
        BUS_NAME,
        BUS_PATH,
        PROPS_IFACE,
        "Get",
        &(STATUS_IFACE, prop),
    )
    .await?;
    let v: OwnedValue = msg
        .body()
        .deserialize()
        .map_err(|e| reply_err("Properties.Get", &e))?;
    bool::try_from(v).map_err(|e| SessionError::Reply {
        what: "Properties.Get",
        detail: e.to_string(),
    })
}

async fn set_bool(
    session: &Connection,
    deadline: Deadline,
    prop: &'static str,
    value: bool,
) -> Result<(), SessionError> {
    call_method(
        session,
        deadline,
        BUS_NAME,
        BUS_PATH,
        PROPS_IFACE,
        "Set",
        &(STATUS_IFACE, prop, Value::from(value)),
    )
    .await?;
    Ok(())
}

/// Read `org.a11y.Status` (ID-5, "previous values").
pub async fn read_status(
    session: &Connection,
    deadline: Deadline,
) -> Result<StatusValues, SessionError> {
    Ok(StatusValues {
        is_enabled: get_bool(session, deadline, "IsEnabled").await?,
        screen_reader_enabled: get_bool(session, deadline, "ScreenReaderEnabled").await?,
    })
}

/// Write only the properties that differ from `current` (never toggle a value that is already
/// right, so a running Orca sees no change).
pub async fn write_status(
    session: &Connection,
    deadline: Deadline,
    current: StatusValues,
    want: StatusValues,
) -> Result<(), SessionError> {
    if current.is_enabled != want.is_enabled {
        set_bool(session, deadline, "IsEnabled", want.is_enabled).await?;
    }
    if current.screen_reader_enabled != want.screen_reader_enabled {
        set_bool(
            session,
            deadline,
            "ScreenReaderEnabled",
            want.screen_reader_enabled,
        )
        .await?;
    }
    Ok(())
}

/// `Registry.RegisterEvent` for each of `events`, from our call connection. Newer at-spi2-core
/// takes `(event, properties, app_bus_name)`; older versions take `(event, properties)`, which is
/// tried when the three-argument form is rejected. Returns how many were accepted.
pub async fn register_events(
    conn: &Connection,
    clock: &Clock,
    events: &[&str],
) -> Result<usize, SessionError> {
    let mut accepted = 0;
    let props: Vec<&str> = Vec::new();
    for ev in events {
        let deadline = Deadline::for_class(clock, BudgetClass::Startup);
        let three = call_method(
            conn,
            deadline,
            REGISTRY_NAME,
            REGISTRY_PATH,
            REGISTRY_IFACE,
            "RegisterEvent",
            &(*ev, &props, ""),
        )
        .await;
        match three {
            Ok(_) => accepted += 1,
            Err(DbusError::Timeout) => return Err(SessionError::Dbus(DbusError::Timeout)),
            Err(DbusError::Zbus(_)) => {
                let deadline = Deadline::for_class(clock, BudgetClass::Startup);
                call_method(
                    conn,
                    deadline,
                    REGISTRY_NAME,
                    REGISTRY_PATH,
                    REGISTRY_IFACE,
                    "RegisterEvent",
                    &(*ev, &props),
                )
                .await?;
                accepted += 1;
            }
        }
    }
    Ok(accepted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_events_are_a_small_explicit_set() {
        assert!(REGISTERED_EVENTS.len() < 16);
        assert!(REGISTERED_EVENTS.iter().all(|e| !e.contains('*')));
        assert!(REGISTERED_EVENTS.contains(&"Object:StateChanged:Focused"));
    }

    #[tokio::test]
    async fn connecting_to_a_missing_bus_fails_fast_with_an_error() {
        let clock = Clock::System;
        let d = Deadline::after(&clock, Duration::from_millis(500));
        let r = connect("unix:path=/nonexistent/gorca-test-bus", d).await;
        assert!(r.is_err());
    }
}
