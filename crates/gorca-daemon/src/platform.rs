//! Platform probe (ADR-0005 8): the desktop facts that change which keys gorca can use.
//!
//! Read through `gsettings get` subprocesses, each under a 500 ms deadline, off the hot lanes.
//! A missing tool, a timeout or an unparsable answer leaves the documented default in place:
//! the probe never blocks startup and never fails.

use std::time::Duration;

use gorca_core::PlatformFacts;
use gorca_input_api::KeysymTable;
use gorca_input_xkb::{LayoutSpec, build_table};
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
    /// Input sources instead of `org.gnome.desktop.input-sources sources`: comma separated XKB
    /// layouts, `us,ru` or `us,us+dvorak` (`GORCA_LAYOUTS`).
    pub layouts: Option<String>,
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
    let mut sources = None;
    if !overrides.is_complete() {
        let (delay, opts, clock, src) = tokio::join!(
            gsettings("org.gnome.desktop.peripherals.keyboard", "delay"),
            gsettings("org.gnome.desktop.input-sources", "xkb-options"),
            gsettings("org.gnome.desktop.interface", "clock-format"),
            gsettings("org.gnome.desktop.input-sources", "sources"),
        );
        sources = src.as_deref().map(parse_sources);
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
    let mut facts = overrides.apply(facts);
    let ids = match &overrides.layouts {
        Some(l) => parse_layout_list(l),
        None => sources.unwrap_or_default(),
    };
    facts.keysyms = keysyms(ids, facts.xkb_options.clone()).await;
    facts
}

/// The union keysym table of `ids` (XKB layout names) plus `us`, built off the async threads
/// under a deadline; the US-only table when it takes too long or libxkbcommon is missing.
pub async fn keysyms(mut ids: Vec<String>, options: String) -> KeysymTable {
    if !ids.iter().any(|i| i == "us") {
        ids.insert(0, "us".to_owned());
    }
    if ids.len() == 1 {
        // US only: the table's own fallback is exactly that.
        return KeysymTable::us();
    }
    let build = tokio::task::spawn_blocking(move || {
        let specs: Vec<LayoutSpec> = ids.iter().filter_map(|i| LayoutSpec::parse(i)).collect();
        build_table(&specs, &options)
    });
    match tokio::time::timeout(KEYMAP_DEADLINE, build).await {
        Ok(Ok(t)) => t,
        Ok(Err(_)) | Err(_) => KeysymTable::us(),
    }
}

/// Deadline for building the table (a keymap compile per layout).
pub const KEYMAP_DEADLINE: Duration = Duration::from_secs(3);

/// `[('xkb', 'us'), ('xkb', 'ru'), ('ibus', 'mozc-jp')]` into `["us", "ru"]`. IBus engines type
/// through the baseline `us` layout, which is always part of the table, so they add nothing.
pub fn parse_sources(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(open) = rest.find('(') {
        let Some(close) = rest[open..].find(')') else {
            break;
        };
        let inner = &rest[open + 1..open + close];
        rest = &rest[open + close + 1..];
        let mut parts = inner
            .split(',')
            .map(|p| p.trim().trim_matches('\'').trim_matches('"'));
        if let (Some("xkb"), Some(id)) = (parts.next(), parts.next())
            && !id.is_empty()
            && !out.iter().any(|o: &String| o == id)
        {
            out.push(id.to_owned());
        }
    }
    out
}

/// `us,ru` into `["us", "ru"]`.
pub fn parse_layout_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
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

    #[test]
    fn input_sources_parse() {
        assert_eq!(
            parse_sources("[('xkb', 'us'), ('xkb', 'ru'), ('ibus', 'mozc-jp')]"),
            vec!["us", "ru"]
        );
        assert_eq!(parse_sources("[('xkb', 'us+dvorak')]"), vec!["us+dvorak"]);
        assert!(parse_sources("@a(ss) []").is_empty());
        assert_eq!(parse_layout_list(" us, ru ,"), vec!["us", "ru"]);
    }

    #[tokio::test]
    async fn layout_overrides_build_the_union_table() {
        let o = PlatformOverrides {
            repeat_delay_ms: Some(300),
            xkb_options: Some(String::new()),
            clock_24h: Some(true),
            layouts: Some("us,ru".into()),
        };
        let f = probe(&o).await;
        // Physical H: `h` always; `Cyrillic_er` when libxkbcommon is present.
        assert!(f.keysyms.syms(43, false).contains(&0x68));
        if gorca_input_xkb::available() {
            assert!(f.keysyms.syms(43, false).contains(&0x6d2));
        }
    }

    #[tokio::test]
    async fn complete_overrides_never_start_a_process() {
        let o = PlatformOverrides {
            repeat_delay_ms: Some(300),
            xkb_options: Some("grp:caps_toggle".into()),
            clock_24h: Some(false),
            layouts: None,
        };
        let f = probe(&o).await;
        assert_eq!(f.repeat_delay_ms, 300);
        assert!(f.caps_switches_layout());
        assert!(!f.clock_24h);
    }
}
