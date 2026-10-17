//! Process identity beyond the pid.
//!
//! A pid alone is not an identity: pids are reused. A process is identified by `(pid, start
//! time)`, the start time being field 22 of `/proc/<pid>/stat` in clock ticks since boot, which
//! never repeats for one pid. The process group (field 5) is what the application cap groups by:
//! a fork attack shares its group, a legitimately launched application does not.

/// What `/proc/<pid>/stat` tells about a process.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProcInfo {
    /// Process group id.
    pub pgrp: Option<u32>,
    /// Start time in clock ticks since boot.
    pub start: Option<u64>,
    /// The systemd unit the process runs in (`app-gnome-firefox-123.scope`, a service), from
    /// `/proc/<pid>/cgroup`. `None` when there is none (root cgroup, no systemd).
    pub scope: Option<String>,
}

impl ProcInfo {
    /// The key the application cap groups by: the systemd unit if there is one (GNOME starts
    /// every application in its own scope, while all of them share the shell's process group),
    /// else the process group.
    pub fn group_key(&self) -> Option<u64> {
        match (&self.scope, self.pgrp) {
            (Some(unit), _) => Some(fnv(unit.as_bytes()) | (1 << 63)),
            (None, Some(g)) => Some(u64::from(g)),
            (None, None) => None,
        }
    }
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The innermost `.scope` or `.service` component of the cgroup path in the contents of
/// `/proc/<pid>/cgroup` (cgroup v2: `0::/user.slice/.../app-foo-1.scope`), else `None`.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
pub fn parse_cgroup(contents: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let path = line.rsplit(':').next()?;
        path.rsplit('/')
            // Unit names are case-sensitive; this is not a file extension.
            .find(|c| c.ends_with(".scope") || c.ends_with(".service"))
            .map(str::to_owned)
    })
}

/// Parse the contents of `/proc/<pid>/stat`. The command name (field 2) is in parentheses and
/// may itself contain spaces and parentheses, so fields are counted from the last `)`.
pub fn parse_stat(contents: &str) -> ProcInfo {
    let Some(rest) = contents.rfind(')').map(|i| &contents[i + 1..]) else {
        return ProcInfo::default();
    };
    // After the comm: state(3) ppid(4) pgrp(5) ... starttime(22).
    let fields: Vec<&str> = rest.split_whitespace().collect();
    ProcInfo {
        pgrp: fields.get(2).and_then(|f| f.parse().ok()),
        start: fields.get(19).and_then(|f| f.parse().ok()),
        scope: None,
    }
}

/// Read `/proc/<pid>/stat`. Unknown (not running, not readable) is `ProcInfo::default()`.
pub fn read_proc_info(pid: u32) -> ProcInfo {
    let mut info = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|s| parse_stat(&s))
        .unwrap_or_default();
    info.scope = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .ok()
        .and_then(|s| parse_cgroup(&s));
    info
}

/// Kernel clock ticks per second in `/proc/<pid>/stat` times (`USER_HZ`: 100 on every Linux
/// architecture gorca targets).
const USER_HZ: u64 = 100;

/// How long ago a process started, from its start time in clock ticks since boot and
/// `/proc/uptime`. `None` when `/proc/uptime` cannot be read or the start is in the future.
pub fn age_ms(start_ticks: u64) -> Option<u64> {
    let up = std::fs::read_to_string("/proc/uptime").ok()?;
    age_ms_at(start_ticks, &up)
}

/// [`age_ms`] for the contents of `/proc/uptime` (`"<uptime secs> <idle secs>"`).
pub fn age_ms_at(start_ticks: u64, uptime: &str) -> Option<u64> {
    let secs: f64 = uptime.split_whitespace().next()?.parse().ok()?;
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    // Centiseconds, rounded down: `USER_HZ` is 100, so a tick is 10 ms.
    let now_ms = (secs * 1000.0) as u64;
    now_ms.checked_sub(start_ticks.saturating_mul(1000 / USER_HZ))
}

/// [`age_ms`] off the async executor (a short `/proc` read under a deadline).
pub async fn age_ms_async(start_ticks: u64) -> Option<u64> {
    let job = tokio::task::spawn_blocking(move || age_ms(start_ticks));
    match tokio::time::timeout(std::time::Duration::from_millis(250), job).await {
        Ok(Ok(a)) => a,
        Ok(Err(_)) | Err(_) => None,
    }
}

/// [`read_proc_info`] off the async executor: `/proc` reads are fast but they are synchronous
/// file I/O, so they run on a blocking thread under a deadline. Unknown on timeout.
pub async fn read_proc_info_async(pid: u32) -> ProcInfo {
    let job = tokio::task::spawn_blocking(move || read_proc_info(pid));
    match tokio::time::timeout(std::time::Duration::from_millis(250), job).await {
        Ok(Ok(info)) => info,
        Ok(Err(_)) | Err(_) => ProcInfo::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "1234 (gnome-shell) S 1 1234 1234 0 -1 4194560 1000 0 0 0 10 5 0 0 20 0 12 0 98765 1000000 500 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 0";

    #[test]
    fn the_age_of_a_process_follows_uptime_and_start_ticks() {
        // Started at tick 98_765 (987.65 s), now 990.0 s since boot: 2.35 s old.
        assert_eq!(age_ms_at(98_765, "990.00 5000.00\n"), Some(2350));
        assert_eq!(age_ms_at(99_100, "990.00 1.0"), None, "a start in the future is no age");
        assert_eq!(age_ms_at(1, "garbage"), None);
    }

    #[test]
    fn pgrp_and_start_time_are_read() {
        let p = parse_stat(STAT);
        assert_eq!(p.pgrp, Some(1234));
        assert_eq!(p.start, Some(98_765));
    }

    #[test]
    fn a_hostile_command_name_cannot_shift_the_fields() {
        let evil = STAT.replace("(gnome-shell)", "(a) S 9 9 9 0 (b c)");
        let p = parse_stat(&evil);
        assert_eq!(p.start, Some(98_765));
        assert_eq!(p.pgrp, Some(1234));
    }

    #[test]
    fn the_systemd_unit_comes_from_the_cgroup_path() {
        let gnome = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-firefox-533563.scope\n";
        assert_eq!(
            parse_cgroup(gnome).as_deref(),
            Some("app-gnome-firefox-533563.scope")
        );
        // A process inside a service's sub-cgroup belongs to the service.
        let svc = "0::/system.slice/foo.service/sub\n";
        assert_eq!(parse_cgroup(svc).as_deref(), Some("foo.service"));
        // Only slices, or the root: no unit, the caller falls back to the process group.
        assert_eq!(parse_cgroup("0::/user.slice\n"), None);
        assert_eq!(parse_cgroup("0::/\n"), None);
        assert_eq!(parse_cgroup(""), None);
        // cgroup v1 style lines work too.
        assert_eq!(
            parse_cgroup("12:cpu:/user.slice/session-3.scope\n").as_deref(),
            Some("session-3.scope")
        );
    }

    /// Shell-launched applications share the shell's process group but not its systemd scope:
    /// the unit is the grouping key, the process group only the fallback.
    #[test]
    fn grouping_prefers_the_scope_over_the_process_group() {
        let firefox = ProcInfo {
            pgrp: Some(532_394),
            start: Some(1),
            scope: Some("app-gnome-firefox-533563.scope".to_owned()),
        };
        let terminal = ProcInfo {
            pgrp: Some(532_394),
            start: Some(2),
            scope: Some("app-org.gnome.Terminal.slice-1.scope".to_owned()),
        };
        let shell_pgrp_only = ProcInfo {
            pgrp: Some(532_394),
            start: Some(3),
            scope: None,
        };
        assert_ne!(
            firefox.group_key(),
            terminal.group_key(),
            "same pgid, different scopes"
        );
        assert_ne!(firefox.group_key(), shell_pgrp_only.group_key());
        assert_eq!(
            shell_pgrp_only.group_key(),
            Some(532_394),
            "fallback: the pgid"
        );
        assert_eq!(ProcInfo::default().group_key(), None);
        // A scope key never collides with a pgid-valued key.
        assert!(firefox.group_key().is_some_and(|k| k >= 1 << 63));
    }

    #[test]
    fn garbage_is_unknown_not_a_panic() {
        assert_eq!(parse_stat(""), ProcInfo::default());
        assert_eq!(parse_stat("no parens here"), ProcInfo::default());
        assert_eq!(parse_stat("1 (x) S"), ProcInfo::default());
    }

    #[test]
    fn our_own_process_is_known() {
        let p = read_proc_info(std::process::id());
        assert!(p.start.is_some() && p.pgrp.is_some());
        assert_eq!(read_proc_info(u32::MAX), ProcInfo::default());
    }
}
