//! Stand-alone event feed for the source: subscribes to `org.a11y.atspi.Event.Object` of one
//! application on an a11y bus and forwards normalised [`AtspiEvent`]s.
//!
//! In the daemon the connector owns the bus ingress and forwards events itself; this module is for
//! the B-6 harness and the integration tests, which run against a private bus.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use futures_lite::StreamExt;
use gorca_dbus::{DbusError, call_method};
use gorca_rt::{CancellationToken, Deadline, mpsc};
use zbus::message::Type as MsgType;
use zbus::zvariant::{OwnedValue, Value};

use crate::events::AtspiEvent;
use crate::wire::PathKey;

const REGISTRY: &str = "org.a11y.atspi.Registry";
const REGISTRY_PATH: &str = "/org/a11y/atspi/registry";

/// Ask the registry to forward `object:` events (the AT's side of event registration).
pub async fn register_object_events(bus: &zbus::Connection) -> Result<(), DbusError> {
    let dl = Deadline(Instant::now() + Duration::from_secs(3));
    call_method(
        bus,
        dl,
        REGISTRY,
        REGISTRY_PATH,
        REGISTRY,
        "RegisterEvent",
        &("object:",),
    )
    .await?;
    Ok(())
}

/// Forward the app's object events into `tx` until `shutdown` or the receiver closes. Returns once
/// the match rule is installed so the caller knows no event is missed after that point.
pub async fn subscribe(
    bus: &zbus::Connection,
    app_unique_name: &str,
    tx: mpsc::Sender<AtspiEvent>,
    shutdown: CancellationToken,
) -> zbus::Result<tokio::task::JoinHandle<u64>> {
    let rule = zbus::MatchRule::builder()
        .msg_type(MsgType::Signal)
        .sender(app_unique_name)?
        .interface("org.a11y.atspi.Event.Object")?
        .build();
    let mut stream = zbus::MessageStream::for_match_rule(rule, bus, Some(8192)).await?;
    Ok(tokio::spawn(async move {
        let mut dropped = 0u64;
        loop {
            let msg = tokio::select! {
                () = shutdown.cancelled() => break,
                m = stream.next() => match m { Some(Ok(m)) => m, Some(Err(_)) => continue, None => break },
            };
            let at = Instant::now();
            let hdr = msg.header();
            let (Some(member), Some(path)) = (hdr.member(), hdr.path()) else {
                continue;
            };
            let Ok((detail, d1, d2, any, _props)) =
                msg.body()
                    .deserialize::<(String, i32, i32, OwnedValue, HashMap<String, OwnedValue>)>()
            else {
                continue;
            };
            let child = child_ref(&any);
            let Some(pk) = PathKey::parse(path.as_str()) else {
                continue;
            };
            if let Some(ev) =
                AtspiEvent::from_object_signal(pk, member.as_str(), &detail, d1, d2, child, at)
                && tx.try_send(ev).is_err()
            {
                dropped += 1;
            }
        }
        dropped
    }))
}

/// The `(so)` object reference carried in the event's variant, if it is one.
fn child_ref(v: &OwnedValue) -> Option<PathKey> {
    let Value::Structure(s) = &**v else {
        return None;
    };
    let f = s.fields();
    match f.get(1)? {
        Value::ObjectPath(p) => PathKey::parse(p.as_str()),
        _ => None,
    }
}
