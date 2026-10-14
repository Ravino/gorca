//! ID-5 (amended 2026-10-14, docs/reports/live-keys-swallowed.md): "At startup gorca sets
//! `org.a11y.Status.IsEnabled = true`. It never writes `ScreenReaderEnabled`. Shutdown restores
//! the previous `IsEnabled`, unless a screen reader has since claimed the desktop."
//!
//! Why not `ScreenReaderEnabled`: in at-spi2-core 2.56 a `Set` of that property writes the
//! GSettings key `org.gnome.desktop.a11y.applications screen-reader-enabled` (and
//! `toolkit-accessibility`), and GNOME's session autostart then launches Orca, which queues for
//! `org.gnome.Orca.KeyboardMonitor` behind gorca and adds its own Insert grab. Every toolkit gorca
//! cares about wakes up on `IsEnabled` alone (Firefox reads only `IsEnabled`; Qt reads either;
//! Chromium reads `IsEnabled` and finds a reader by process name, not by this property).
//!
//! The D-Bus property reads and writes live in `events::session`; this module is the pure plan.

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

    /// Values to write at startup: `IsEnabled` on, `ScreenReaderEnabled` exactly as found.
    pub fn on_startup(&self) -> StatusValues {
        StatusValues {
            is_enabled: true,
            screen_reader_enabled: self.previous.screen_reader_enabled,
        }
    }

    /// Values to write at shutdown, given what the bus says now. Only what gorca changed is
    /// restored, and never while a screen reader has claimed the desktop in the meantime
    /// (`ScreenReaderEnabled` became true): turning `IsEnabled` off would cut its applications.
    pub fn on_shutdown(&self, now: StatusValues) -> StatusValues {
        if now.screen_reader_enabled && !self.previous.screen_reader_enabled {
            return now;
        }
        StatusValues {
            is_enabled: self.previous.is_enabled,
            screen_reader_enabled: now.screen_reader_enabled,
        }
    }

    /// Does startup need to write anything?
    pub fn startup_is_noop(&self) -> bool {
        self.previous == self.on_startup()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OFF: StatusValues = StatusValues {
        is_enabled: false,
        screen_reader_enabled: false,
    };

    #[test]
    fn startup_enables_only_is_enabled_and_shutdown_restores_it() {
        let e = Enablement::new(OFF);
        assert_eq!(
            e.on_startup(),
            StatusValues {
                is_enabled: true,
                screen_reader_enabled: false
            },
            "ScreenReaderEnabled would launch Orca through the session autostart"
        );
        assert!(!e.startup_is_noop());
        assert_eq!(e.on_shutdown(e.on_startup()), OFF);
    }

    #[test]
    fn screen_reader_enabled_is_never_changed() {
        for sr in [false, true] {
            for ie in [false, true] {
                let prev = StatusValues {
                    is_enabled: ie,
                    screen_reader_enabled: sr,
                };
                let e = Enablement::new(prev);
                assert_eq!(e.on_startup().screen_reader_enabled, sr);
                assert_eq!(e.on_shutdown(e.on_startup()).screen_reader_enabled, sr);
            }
        }
    }

    #[test]
    fn a_screen_reader_started_meanwhile_keeps_is_enabled() {
        let e = Enablement::new(OFF);
        let orca_started = StatusValues {
            is_enabled: true,
            screen_reader_enabled: true,
        };
        assert_eq!(e.on_shutdown(orca_started), orca_started);
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
