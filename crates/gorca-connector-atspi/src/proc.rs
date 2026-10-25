// SPDX-License-Identifier: GPL-3.0-or-later
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

/// Prefixes of the root-owned system trees a native application's executable may live in.
const SYSTEM_PREFIXES: &[&str] = &["/usr/", "/opt/"];

/// True when `path` (absolute, canonical) is a regular file in a system tree that only root can
/// change: under `/usr` or `/opt`, and the file and every directory above it owned by root, not
/// writable by anyone else (group-writable only for group 0). A binary a user copied to `~/bin`
/// or `/tmp` fails, whatever it is called.
pub fn is_trusted_system_path(path: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(text) = path.to_str() else {
        return false;
    };
    if text.ends_with(" (deleted)") || !SYSTEM_PREFIXES.iter().any(|p| text.starts_with(p)) {
        return false;
    }
    let root_only = |m: &std::fs::Metadata| {
        let mode = m.mode();
        m.uid() == 0 && mode & 0o002 == 0 && (mode & 0o020 == 0 || m.gid() == 0)
    };
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_file() && root_only(&m) => {}
        _ => return false,
    }
    path.ancestors()
        .skip(1)
        .filter(|a| !a.as_os_str().is_empty())
        .all(|a| {
            std::fs::symlink_metadata(a)
                .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink() && root_only(&m))
        })
}

/// The `[Application] name=` of the contents of a `.flatpak-info` file.
pub fn parse_flatpak_info(text: &str) -> Option<String> {
    let mut in_app = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_app = line == "[Application]";
        } else if in_app && let Some(v) = line.strip_prefix("name=") {
            let ok = !v.is_empty()
                && v.len() <= 255
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-');
            return ok.then(|| v.to_owned());
        }
    }
    None
}

/// The flatpak application id when the process is a flatpak sandbox: `.flatpak-info` names it
/// and the systemd scope of the process agrees (`app-flatpak-<id>-<n>.scope`). Case-sensitive.
pub fn flatpak_identity(info: &str, cgroup_scope: Option<&str>) -> Option<String> {
    let id = parse_flatpak_info(info)?;
    let scope = cgroup_scope?;
    scope
        .strip_prefix("app-flatpak-")
        .is_some_and(|rest| rest.starts_with(&format!("{id}-")))
        .then_some(id)
}

/// The snap name in an AppArmor label (`snap.<name>.<app> (enforce)`). Only an **enforced**
/// confinement counts: the kernel sets the label, a process cannot choose it, but a profile in
/// complain mode confines nothing.
pub fn snap_identity(label: &str) -> Option<String> {
    let label = label.trim();
    let rest = label.strip_prefix("snap.")?;
    if !label.ends_with("(enforce)") {
        return None;
    }
    let name = rest.split(['.', ' ']).next()?;
    (!name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
    .then(|| name.to_owned())
}

/// True when the file `/proc/<pid>/exe` really is (same device, same inode) the file at `path`
/// as gorca sees it. The `exe` link is resolved in the process's own mount namespace, so a
/// process that bind-mounted a look-alike over a system path, or runs in a namespace where
/// `/usr/bin/firefox` is something else, does not match.
pub fn exe_is_file(pid: u32, path: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Ok(theirs), Ok(ours)) = (
        std::fs::metadata(format!("/proc/{pid}/exe")),
        std::fs::metadata(path),
    ) else {
        return false;
    };
    theirs.dev() == ours.dev() && theirs.ino() == ours.ino()
}

/// The mount namespace of a process (the target of `/proc/<pid>/ns/mnt`).
fn mount_ns(pid: &str) -> Option<std::path::PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/ns/mnt")).ok()
}

/// The verified identity of process `pid`, or `None` (unverified). The process must still be the
/// one that started at `start` (pids are reused). Evidence, in order:
///
/// 1. a **snap**: an enforced AppArmor label `snap.<name>.<app>` (only
///    `/proc/<pid>/attr/apparmor/current`), and an executable under `/snap/<name>/` that is the
///    same file (device and inode) as that path in gorca's namespace: identity `snap:<name>`;
/// 2. a **native** application, in gorca's own mount namespace: `/proc/<pid>/exe`
///    canonicalised, a live file in a root-owned system tree ([`is_trusted_system_path`]) that is
///    the same file as the process runs ([`exe_is_file`]): identity is the file's name, which is
///    trustworthy because only root could have put it there;
/// 3. a **flatpak** sandbox (another mount namespace with a `.flatpak-info` naming the app and
///    a matching systemd scope): identity `flatpak:<app id>`, which the caller treats as
///    **sandboxed-unverified**, see [`is_sandboxed_unverified`].
///
/// Anything else (a copy under `$HOME` or `/tmp`, a deleted binary, another mount namespace
/// without the evidence above, an unreadable `/proc`) is unverified. The self-reported
/// application name is never consulted. Comparison with a manifest's `[match] apps` is
/// case-sensitive.
///
/// Threat model: this stops accidental misattribution and cheap spoofing. A determined process
/// of the same user can already present any accessibility tree it likes, so impersonating an
/// application gains it nothing it could not do without a plugin. It is not a defence against a
/// same-user adversary (ADR-0026 section 2.7).
///
/// TODO(portal): a flatpak or snap application id from the portal (`org.freedesktop.portal`
/// `GetHostAppInfo`/`GetAppInfo`) is the backed evidence; until it is read, a flatpak sandbox
/// stays sandboxed-unverified because `.flatpak-info` can be forged with a user namespace.
pub fn verified_identity(pid: u32, start: u64) -> Option<String> {
    if read_proc_info(pid).start != Some(start) {
        return None;
    }
    let found = identity_unchecked(pid);
    // Re-check after the reads: the pid may have been recycled in between.
    (read_proc_info(pid).start == Some(start))
        .then_some(found)
        .flatten()
}

/// The identity of a flatpak sandbox is evidence of a weaker kind (see [`verified_identity`]).
pub fn is_sandboxed_unverified(identity: &str) -> bool {
    identity.starts_with("flatpak:")
}

fn identity_unchecked(pid: u32) -> Option<String> {
    let ours = mount_ns("self");
    let theirs = mount_ns(&pid.to_string());
    let same_ns = ours.is_some() && ours == theirs;
    // Snap: the kernel's enforced confinement label, then the executable under /snap/<name>/.
    if let Ok(label) = std::fs::read_to_string(format!("/proc/{pid}/attr/apparmor/current"))
        && let Some(name) = snap_identity(&label)
    {
        let target = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
        let canon = std::fs::canonicalize(&target).ok()?;
        let under = canon.starts_with(format!("/snap/{name}/"));
        return (under && exe_is_file(pid, &canon)).then(|| format!("snap:{name}"));
    }
    if !same_ns {
        // Another mount namespace without a snap label: a flatpak sandbox, or an impostor.
        let info = std::fs::read_to_string(format!("/proc/{pid}/root/.flatpak-info")).ok()?;
        let scope = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
            .ok()
            .and_then(|s| parse_cgroup(&s));
        return flatpak_identity(&info, scope.as_deref()).map(|id| format!("flatpak:{id}"));
    }
    let target = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    if target.to_str()?.ends_with(" (deleted)") {
        return None;
    }
    let canon = std::fs::canonicalize(&target).ok()?;
    if !is_trusted_system_path(&canon) || !exe_is_file(pid, &canon) {
        return None;
    }
    canon.file_name()?.to_str().map(str::to_owned)
}

/// [`verified_identity`] off the async executor under a deadline.
pub async fn exe_of_async(pid: u32, start: u64) -> Option<String> {
    let job = tokio::task::spawn_blocking(move || verified_identity(pid, start));
    match tokio::time::timeout(std::time::Duration::from_millis(250), job).await {
        Ok(Ok(e)) => e,
        Ok(Err(_)) | Err(_) => None,
    }
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
    let first = uptime.split_whitespace().next()?;
    let (whole, frac) = first.split_once('.').unwrap_or((first, "0"));
    let secs: u64 = whole.parse().ok()?;
    // Hundredths of a second: `USER_HZ` is 100, so a tick is 10 ms.
    let hundredths: u64 = format!("{frac:0<2}").get(..2)?.parse().ok()?;
    let now_ms = secs.checked_mul(1000)?.checked_add(hundredths * 10)?;
    now_ms.checked_sub(start_ticks.saturating_mul(1000 / USER_HZ))
}

/// Start time of this process in clock ticks since boot (read once).
pub fn own_start_ticks() -> Option<u64> {
    static START: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *START.get_or_init(|| {
        std::fs::read_to_string("/proc/self/stat")
            .ok()
            .and_then(|s| parse_stat(&s).start)
    })
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

    use std::os::unix::fs::PermissionsExt as _;
    use std::process::{Child, Command, Stdio};

    fn spawn(path: &str) -> Child {
        Command::new(path)
            .arg("30")
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn")
    }

    fn start_of(c: &Child) -> u64 {
        read_proc_info(c.id()).start.expect("start")
    }

    #[test]
    fn a_system_binary_is_verified_by_its_file_name() {
        let mut c = spawn("/usr/bin/sleep");
        let id = verified_identity(c.id(), start_of(&c));
        let _ = c.kill();
        let _ = c.wait();
        assert_eq!(id.as_deref(), Some("sleep"));
    }

    #[test]
    fn a_copy_in_a_user_writable_directory_is_not_verified_whatever_it_is_called() {
        let dir = tempfile::tempdir_in("/var/tmp").expect("tmp");
        let copy = dir.path().join("firefox");
        std::fs::copy("/usr/bin/sleep", &copy).expect("copy");
        std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let mut c = spawn(copy.to_str().expect("utf8"));
        let id = verified_identity(c.id(), start_of(&c));
        assert_eq!(id, None, "a binary called firefox in {} is not Firefox", dir.path().display());
        // Deleted while running: unverified as well.
        std::fs::remove_file(&copy).expect("rm");
        assert_eq!(verified_identity(c.id(), start_of(&c)), None);
        let _ = c.kill();
        let _ = c.wait();
        // And the path rules on their own.
        assert!(!is_trusted_system_path(&copy));
        assert!(!is_trusted_system_path(std::path::Path::new("/usr/bin/sleep (deleted)")));
        assert!(!is_trusted_system_path(std::path::Path::new("/var/tmp/x")));
        assert!(!is_trusted_system_path(std::path::Path::new("/usr/bin/does-not-exist")));
        assert!(is_trusted_system_path(std::path::Path::new("/usr/bin/sleep")));
    }

    #[test]
    fn the_executable_must_be_the_same_file_by_device_and_inode() {
        let mut c = spawn("/usr/bin/sleep");
        let pid = c.id();
        let same = exe_is_file(pid, std::path::Path::new("/usr/bin/sleep"));
        // Another file with the same content is not the process's executable.
        let other = exe_is_file(pid, std::path::Path::new("/usr/bin/true"));
        let _ = c.kill();
        let _ = c.wait();
        assert!(same);
        assert!(!other);
    }

    #[test]
    fn a_process_in_another_mount_namespace_is_unverified_without_sandbox_evidence() {
        // `unshare` needs unprivileged user namespaces; where the kernel forbids them there is
        // nothing to test.
        let Ok(mut c) = Command::new("/usr/bin/unshare")
            .args(["-Urm", "/usr/bin/sleep", "30"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return;
        };
        // Wait until the exec of sleep, so the namespace and the exe are the final ones.
        let pid = c.id();
        let mut ns_differs = false;
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            if mount_ns(&pid.to_string()).is_some() && mount_ns(&pid.to_string()) != mount_ns("self") {
                ns_differs = true;
                break;
            }
        }
        let id = read_proc_info(pid).start.and_then(|st| verified_identity(pid, st));
        let _ = c.kill();
        let _ = c.wait();
        if ns_differs {
            assert_eq!(id, None, "a system binary in a foreign mount namespace is not verified");
        }
    }

    #[test]
    fn a_wrong_start_time_is_a_different_process() {
        let me = std::process::id();
        let start = read_proc_info(me).start.expect("own start time");
        assert_eq!(verified_identity(me, start + 1), None);
    }

    #[test]
    fn flatpak_and_snap_identities_are_ids_not_binary_names() {
        let info = "[Application]\nname=org.mozilla.firefox\nruntime=runtime/x\n[Instance]\ninstance-id=1\n";
        assert_eq!(parse_flatpak_info(info).as_deref(), Some("org.mozilla.firefox"));
        let scope = Some("app-flatpak-org.mozilla.firefox-1234.scope");
        assert_eq!(flatpak_identity(info, scope).as_deref(), Some("org.mozilla.firefox"));
        // The scope must agree with the info file; a mismatch or no scope is unverified.
        assert_eq!(flatpak_identity(info, Some("app-flatpak-org.evil.app-1.scope")), None);
        assert_eq!(flatpak_identity(info, None), None);
        // Case-sensitive: a differently-cased id is a different application.
        assert_eq!(flatpak_identity(info, Some("app-flatpak-org.mozilla.Firefox-1.scope")), None);
        assert_eq!(parse_flatpak_info("[Application]\nname=bad name\n"), None);
        assert_eq!(snap_identity("snap.chromium.chromium (enforce)\n").as_deref(), Some("chromium"));
        // Complain mode confines nothing: not evidence.
        assert_eq!(snap_identity("snap.chromium.chromium (complain)"), None);
        assert_eq!(snap_identity("snap.chromium.chromium"), None);
        assert!(is_sandboxed_unverified("flatpak:org.mozilla.firefox"));
        assert!(!is_sandboxed_unverified("snap:chromium"));
        assert!(!is_sandboxed_unverified("firefox"));
        assert_eq!(snap_identity("unconfined"), None);
        assert_eq!(snap_identity("snap..x"), None);
    }

    #[test]
    fn the_age_of_a_process_follows_uptime_and_start_ticks() {
        // Started at tick 98_765 (987.65 s), now 990.0 s since boot: 2.35 s old.
        assert_eq!(age_ms_at(98_765, "990.00 5000.00\n"), Some(2350));
        assert_eq!(
            age_ms_at(99_100, "990.00 1.0"),
            None,
            "a start in the future is no age"
        );
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
