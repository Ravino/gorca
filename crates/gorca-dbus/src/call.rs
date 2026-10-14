use std::future::Future;

use gorca_rt::{Deadline, DeadlineExceeded};
use zbus::zvariant::DynamicType;

#[derive(Debug, thiserror::Error)]
pub enum DbusError {
    #[error("D-Bus call exceeded its deadline")]
    Timeout,
    #[error("D-Bus error: {0}")]
    Zbus(#[from] zbus::Error),
}

impl From<DeadlineExceeded> for DbusError {
    fn from(_: DeadlineExceeded) -> Self {
        DbusError::Timeout
    }
}

/// Await any zbus future, never past `deadline` (D-4). The future is dropped on expiry, which
/// cancels the call on our side; the broker may still deliver a late reply, which zbus discards.
pub async fn with_deadline<T>(
    deadline: Deadline,
    fut: impl Future<Output = zbus::Result<T>>,
) -> Result<T, DbusError> {
    Ok(gorca_rt::with_deadline(deadline, fut).await??)
}

/// The one place that calls `Connection::call_method`; everything else goes through here.
pub async fn call_method<B>(
    conn: &zbus::Connection,
    deadline: Deadline,
    destination: &str,
    path: &str,
    interface: &str,
    member: &str,
    body: &B,
) -> Result<zbus::Message, DbusError>
where
    B: serde::Serialize + DynamicType,
{
    call_method_to(
        conn,
        deadline,
        Some(destination),
        path,
        interface,
        member,
        body,
    )
    .await
}

/// Like [`call_method`] but the destination may be absent, which is the case on a peer-to-peer
/// connection (the peer has no bus name, ADR-0001 D-7).
pub async fn call_method_to<B>(
    conn: &zbus::Connection,
    deadline: Deadline,
    destination: Option<&str>,
    path: &str,
    interface: &str,
    member: &str,
    body: &B,
) -> Result<zbus::Message, DbusError>
where
    B: serde::Serialize + DynamicType,
{
    #[allow(clippy::disallowed_methods)]
    let call = conn.call_method(destination, path, Some(interface), member, body);
    with_deadline(deadline, call).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[tokio::test(start_paused = true)]
    async fn a_hung_call_is_cut_at_the_deadline() {
        let d = Deadline(Instant::now() + Duration::from_millis(120));
        let r = with_deadline(d, std::future::pending::<zbus::Result<()>>()).await;
        assert!(matches!(r, Err(DbusError::Timeout)));
    }

    #[tokio::test(start_paused = true)]
    async fn zbus_errors_pass_through() {
        let d = Deadline(Instant::now() + Duration::from_millis(120));
        let r = with_deadline(d, async { Err::<(), _>(zbus::Error::InterfaceNotFound) }).await;
        assert!(matches!(
            r,
            Err(DbusError::Zbus(zbus::Error::InterfaceNotFound))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn success_passes_through() {
        let d = Deadline(Instant::now() + Duration::from_millis(120));
        assert_eq!(
            with_deadline(d, async { Ok::<_, zbus::Error>(5) })
                .await
                .unwrap(),
            5
        );
    }
}
