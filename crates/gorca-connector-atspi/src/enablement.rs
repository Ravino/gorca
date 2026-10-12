//! ID-5: "At startup gorca sets `org.a11y.Status.IsEnabled = true` and `ScreenReaderEnabled =
//! true`. It does not toggle them off while running. Shutdown restores the previous values."
//!
//! The D-Bus property reads and writes are TBD (they go through `gorca_dbus::call_method` with
//! a Startup-class deadline). This module is the pure plan.

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StatusValues {
    pub is_enabled: bool,
    pub screen_reader_enabled: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Enablement {
    previous: StatusValues,
}

impl Enablement {
    /// `previous` are the values read at startup, before anything was changed.
    pub fn new(previous: StatusValues) -> Self {
        Self { previous }
    }

    /// Values to write at startup.
    pub fn on_startup(&self) -> StatusValues {
        StatusValues {
            is_enabled: true,
            screen_reader_enabled: true,
        }
    }

    /// Values to write at shutdown. Only what gorca changed is restored.
    pub fn on_shutdown(&self) -> StatusValues {
        self.previous
    }

    /// Does startup need to write anything?
    pub fn startup_is_noop(&self) -> bool {
        self.previous == self.on_startup()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_enables_and_shutdown_restores() {
        let prev = StatusValues {
            is_enabled: false,
            screen_reader_enabled: false,
        };
        let e = Enablement::new(prev);
        assert_eq!(
            e.on_startup(),
            StatusValues {
                is_enabled: true,
                screen_reader_enabled: true
            }
        );
        assert_eq!(e.on_shutdown(), prev);
        assert!(!e.startup_is_noop());
    }

    #[test]
    fn already_enabled_is_a_noop() {
        let prev = StatusValues {
            is_enabled: true,
            screen_reader_enabled: true,
        };
        assert!(Enablement::new(prev).startup_is_noop());
    }
}
