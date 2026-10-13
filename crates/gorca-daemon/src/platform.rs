//! Platform probe (ADR-0005 8): the desktop facts that change which keys gorca can use.
//!
//! Read through `gsettings get` subprocesses, each under a 500 ms deadline, off the hot lanes.
//! A missing tool, a timeout or an unparsable answer leaves the documented default in place:
//! the probe never blocks startup and never fails.

use std::time::Duration;

use gorca_core::PlatformFacts;
use tokio::process::Command;

/// Per-query deadline.
pub const PROBE_DEADLINE: Duration = Duration::from_millis(500);

/// Values given by the process environment or the command line, resolved by `main`. A set field
/// replaces the probe for that fact (and keeps tests off the real desktop settings).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PlatformOverrides {
    pub repeat_delay_ms: Option<u32>,
    pub xkb_options: Option<String>,
    pub clock_24h: Option<bool>,
}

impl PlatformOverrides {
    pub fn is_complete(&self) -> bool {
        self.repeat_delay_ms.is_some() && self.xkb_options.is_some() && self.clock_24h.is_some()
    }

    /// `facts` with every overridden fact replaced.
    pub fn apply(&self, mut facts: PlatformFacts) -> PlatformFacts {
        if let Some(v) = self.repeat_delay_ms {
            facts.repeat_delay_ms = v;
        }
        if let Some(v) = &self.xkb_options {
            facts.xkb_options.clone_from(v);
        }
        if let Some(v) = self.clock_24h {
            facts.clock_24h = v;
        }
        facts
    }
}

async fn gsettings(schema: &str, key: &str) -> Option<String> {
    let run = Command::new("gsettings")
        .args(["get", schema, key])
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let out = tokio::time::timeout(PROBE_DEADLINE, run).await.ok()?.ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Probe what the overrides do not already answer.
pub async fn probe(overrides: &PlatformOverrides) -> PlatformFacts {
    let mut facts = PlatformFacts::default();
    if !overrides.is_complete() {
        let (delay, opts, clock) = tokio::join!(
            gsettings("org.gnome.desktop.peripherals.keyboard", "delay"),
            gsettings("org.gnome.desktop.input-sources", "xkb-options"),
            gsettings("org.gnome.desktop.interface", "clock-format"),
        );
        if let Some(v) = delay.as_deref().and_then(parse_uint) {
            facts.repeat_delay_ms = v;
        }
        if let Some(v) = opts.as_deref() {
            facts.xkb_options = parse_string_list(v);
        }
        if let Some(v) = clock.as_deref() {
            facts.clock_24h = parse_clock_24h(v);
        }
    }
    overrides.apply(facts)
}

/// `uint32 500` or `500`.
pub fn parse_uint(s: &str) -> Option<u32> {
    s.split_whitespace().last()?.parse().ok()
}

/// `['grp:caps_toggle', 'ctrl:nocaps']` or `@as []` into a comma-separated list.
pub fn parse_string_list(s: &str) -> String {
    let inner = s
        .trim()
        .trim_start_matches("@as")
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']');
    inner
        .split(',')
        .map(|p| p.trim().trim_matches('\'').trim_matches('"'))
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(",")
}

/// `'24h'` or `'12h'`; anything else keeps 24 hours.
pub fn parse_clock_24h(s: &str) -> bool {
    !s.contains("12h")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gsettings_values_parse() {
        assert_eq!(parse_uint("uint32 500"), Some(500));
        assert_eq!(parse_uint("250"), Some(250));
        assert_eq!(parse_uint("garbage"), None);
        assert_eq!(
            parse_string_list("['grp:caps_toggle', 'ctrl:nocaps']"),
            "grp:caps_toggle,ctrl:nocaps"
        );
        assert_eq!(parse_string_list("@as []"), "");
        assert!(parse_clock_24h("'24h'"));
        assert!(!parse_clock_24h("'12h'"));
    }

    #[tokio::test]
    async fn complete_overrides_never_start_a_process() {
        let o = PlatformOverrides {
            repeat_delay_ms: Some(300),
            xkb_options: Some("grp:caps_toggle".into()),
            clock_24h: Some(false),
        };
        let f = probe(&o).await;
        assert_eq!(f.repeat_delay_ms, 300);
        assert!(f.caps_switches_layout());
        assert!(!f.clock_24h);
    }
}
