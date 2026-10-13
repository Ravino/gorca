//! Shadow mode (B-1 PW-11, SPEC 8.4): gorca runs beside the user's real screen reader and may
//! only **read**. This is the allowlist the connector enforces on every call it makes to an
//! application; the run is audited independently from a `dbus-monitor` capture (MA-10).
//!
//! Allowed: `Accessible` getters, `Properties.Get`/`GetAll`, `Cache.GetItems`,
//! `Application.GetApplicationBusAddress`, `Text.GetText`, and the bus's own housekeeping
//! (`AddMatch`, name and credential lookups). Everything else (`Action`, `EditableText`,
//! `Selection`, `Component.GrabFocus`, `Registry.RegisterEvent`, `Properties.Set`, ...) is refused
//! before it reaches the wire.

/// May a call to `interface.member` be made in shadow mode?
pub fn allowed(interface: &str, member: &str) -> bool {
    match interface {
        "org.a11y.atspi.Accessible" => member.starts_with("Get"),
        "org.freedesktop.DBus.Properties" => matches!(member, "Get" | "GetAll"),
        "org.a11y.atspi.Cache" => member == "GetItems",
        "org.a11y.atspi.Application" => member == "GetApplicationBusAddress",
        "org.a11y.atspi.Text" => member == "GetText",
        "org.freedesktop.DBus" => matches!(
            member,
            "Hello"
                | "AddMatch"
                | "RemoveMatch"
                | "GetNameOwner"
                | "NameHasOwner"
                | "ListNames"
                | "GetConnectionCredentials"
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MA-10 (unit part): reads pass, everything that could change an application or the
    /// user's session is refused.
    #[test]
    fn the_allowlist_admits_reads_and_nothing_that_writes() {
        for (i, m) in [
            ("org.a11y.atspi.Accessible", "GetRole"),
            ("org.a11y.atspi.Accessible", "GetState"),
            ("org.a11y.atspi.Accessible", "GetChildren"),
            ("org.a11y.atspi.Accessible", "GetChildAtIndex"),
            ("org.a11y.atspi.Accessible", "GetRelationSet"),
            ("org.freedesktop.DBus.Properties", "Get"),
            ("org.freedesktop.DBus.Properties", "GetAll"),
            ("org.a11y.atspi.Cache", "GetItems"),
            ("org.a11y.atspi.Application", "GetApplicationBusAddress"),
            ("org.a11y.atspi.Text", "GetText"),
            ("org.freedesktop.DBus", "AddMatch"),
        ] {
            assert!(allowed(i, m), "{i}.{m} must be allowed");
        }
        for (i, m) in [
            ("org.a11y.atspi.Action", "DoAction"),
            ("org.a11y.atspi.EditableText", "SetTextContents"),
            ("org.a11y.atspi.EditableText", "InsertText"),
            ("org.a11y.atspi.Selection", "SelectChild"),
            ("org.a11y.atspi.Component", "GrabFocus"),
            ("org.a11y.atspi.Component", "ScrollTo"),
            ("org.a11y.atspi.Text", "SetCaretOffset"),
            ("org.a11y.atspi.Value", "SetCurrentValue"),
            ("org.a11y.atspi.Registry", "RegisterEvent"),
            ("org.a11y.atspi.Registry", "DeregisterEvent"),
            ("org.freedesktop.DBus.Properties", "Set"),
            ("org.a11y.Status", "Set"),
            (
                "org.a11y.atspi.DeviceEventController",
                "NotifyListenersSync",
            ),
            ("org.freedesktop.DBus", "RequestName"),
            ("org.freedesktop.DBus", "BecomeMonitor"),
            ("", "GetRole"),
        ] {
            assert!(!allowed(i, m), "{i}.{m} must be refused");
        }
    }
}
