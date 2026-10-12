//! The focused application, published lock-free.
//!
//! The Reader writes it on every focus change; the connector's router reads it when it must
//! decide who gives way at the application cap. One `AtomicU64`: no lock, no message.

use std::sync::atomic::{AtomicU64, Ordering};

use gorca_model::{AppId, ConnectorId};

/// `0` means "no application is focused"; otherwise `(connector + 1) << 32 | serial`.
#[derive(Debug, Default)]
pub struct FocusedApp(AtomicU64);

impl FocusedApp {
    pub const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    pub fn set(&self, app: AppId) {
        let v = (u64::from(app.connector.0) + 1) << 32 | u64::from(app.serial);
        self.0.store(v, Ordering::Release);
    }

    pub fn clear(&self) {
        self.0.store(0, Ordering::Release);
    }

    pub fn get(&self) -> Option<AppId> {
        let v = self.0.load(Ordering::Acquire);
        if v == 0 {
            return None;
        }
        let connector = u16::try_from((v >> 32) - 1).ok()?;
        Some(AppId {
            connector: ConnectorId(connector),
            serial: u32::try_from(v & 0xffff_ffff).ok()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_app_id_including_zero() {
        let f = FocusedApp::new();
        assert_eq!(f.get(), None);
        for (c, n) in [(0u16, 0u32), (1, 7), (u16::MAX, u32::MAX)] {
            let app = AppId {
                connector: ConnectorId(c),
                serial: n,
            };
            f.set(app);
            assert_eq!(f.get(), Some(app));
        }
        f.clear();
        assert_eq!(f.get(), None);
    }
}
