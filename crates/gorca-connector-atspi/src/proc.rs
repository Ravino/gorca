//! Process identity beyond the pid.
//!
//! A pid alone is not an identity: pids are reused. A process is identified by `(pid, start
//! time)`, the start time being field 22 of `/proc/<pid>/stat` in clock ticks since boot, which
//! never repeats for one pid. The process group (field 5) is what the application cap groups by:
//! a fork attack shares its group, a legitimately launched application does not.

/// What `/proc/<pid>/stat` tells about a process.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ProcInfo {
    /// Process group id.
    pub pgrp: Option<u32>,
    /// Start time in clock ticks since boot.
    pub start: Option<u64>,
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
    }
}

/// Read `/proc/<pid>/stat`. Unknown (not running, not readable) is `ProcInfo::default()`.
pub fn read_proc_info(pid: u32) -> ProcInfo {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|s| parse_stat(&s))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "1234 (gnome-shell) S 1 1234 1234 0 -1 4194560 1000 0 0 0 10 5 0 0 20 0 12 0 98765 1000000 500 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 0";

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
