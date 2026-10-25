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
///    **sandboxed-unverified** (the core never treats a `flatpak:` identity as verified).
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

fn identity_unchecked(pid: u32) -> Option<String> {
    identity_in(
        pid,
        mount_ns(&pid.to_string()).as_deref(),
        mount_ns("self").as_deref(),
        std::fs::read_link(format!("/proc/{pid}/ns/user"))
            .ok()
            .as_deref(),
    )
}

/// True when this process is in a user namespace of its own. Then the kernel refuses the
/// ptrace-class `/proc/<pid>/{exe,ns/*,root}` reads of every process outside it, so no peer can be
/// verified: gorca runs this way under a systemd *user* unit with any mount-namespace sandboxing
/// (`PrivateTmp=`, `ProtectSystem=`, `ProtectKernelTunables=`, `ProtectControlGroups=`, ...).
pub fn in_private_user_ns() -> bool {
    std::fs::read_link("/proc/self/ns/user").is_ok_and(|l| l != std::path::Path::new(INIT_USER_NS))
}

/// The initial user namespace (`PROC_USER_INIT_INO`): where the session's own processes live.
const INIT_USER_NS: &str = "user:[4026531837]";

/// [`identity_unchecked`] with the namespaces given: the peer's mount namespace, gorca's own and
/// the peer's user namespace.
///
/// A peer in gorca's own mount namespace is the plain case. gorca as a hardened systemd unit
/// (`PrivateTmp=`, `ProtectSystem=`) lives in mount and user namespaces of its own, so every
/// ordinary desktop application looked foreign (the live bug: no native application was ever
/// verified, so no URL-scoped plugin opened). An unprivileged process can only get a private
/// mount namespace through a user namespace of its own, so a peer in a foreign mount namespace
/// but the **initial user namespace** was placed there by privileged software and is as native
/// as one in gorca's. A peer in a foreign mount namespace that is not in the initial user
/// namespace (an `unshare -Ur` impostor, a user-namespace sandbox) needs sandbox evidence.
/// Either way the executable must be the very file (device, inode) that gorca sees at that path.
fn identity_in(
    pid: u32,
    theirs: Option<&std::path::Path>,
    ours: Option<&std::path::Path>,
    peer_user_ns: Option<&std::path::Path>,
) -> Option<String> {
    let same_ns = theirs.is_some() && theirs == ours;
    let privileged_ns = peer_user_ns == Some(std::path::Path::new(INIT_USER_NS));
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
        // Another mount namespace without a snap label: a flatpak sandbox, or an impostor, or
        // (when it is in the initial user namespace) software started by privileged code.
        let info = std::fs::read_to_string(format!("/proc/{pid}/root/.flatpak-info"));
        match info {
            Ok(info) => {
                let scope = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
                    .ok()
                    .and_then(|s| parse_cgroup(&s));
                return flatpak_identity(&info, scope.as_deref()).map(|id| format!("flatpak:{id}"));
            }
            Err(_) if privileged_ns && theirs.is_some() => {}
            Err(_) => return None,
        }
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

/// The verified identity off the async executor under a deadline: [`verified_identity`] where
/// `/proc` is readable, else (gorca in a private user namespace) the identity helper's answer
/// ([`crate::identity_svc`]).
pub async fn exe_of_async(pid: u32, start: u64) -> Option<String> {
    let job = tokio::task::spawn_blocking(move || {
        crate::identity_client::resolve(pid, start, std::time::Duration::from_millis(200))
    });
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
        assert_eq!(
            id,
            None,
            "a binary called firefox in {} is not Firefox",
            dir.path().display()
        );
        // Deleted while running: unverified as well.
        std::fs::remove_file(&copy).expect("rm");
        assert_eq!(verified_identity(c.id(), start_of(&c)), None);
        let _ = c.kill();
        let _ = c.wait();
        // And the path rules on their own.
        assert!(!is_trusted_system_path(&copy));
        assert!(!is_trusted_system_path(std::path::Path::new(
            "/usr/bin/sleep (deleted)"
        )));
        assert!(!is_trusted_system_path(std::path::Path::new("/var/tmp/x")));
        assert!(!is_trusted_system_path(std::path::Path::new(
            "/usr/bin/does-not-exist"
        )));
        assert!(is_trusted_system_path(std::path::Path::new(
            "/usr/bin/sleep"
        )));
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
    #[allow(clippy::disallowed_methods)] // polling a child process: a plain test thread
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
            if mount_ns(&pid.to_string()).is_some()
                && mount_ns(&pid.to_string()) != mount_ns("self")
            {
                ns_differs = true;
                break;
            }
        }
        let id = read_proc_info(pid)
            .start
            .and_then(|st| verified_identity(pid, st));
        let _ = c.kill();
        let _ = c.wait();
        if ns_differs {
            assert_eq!(
                id, None,
                "a system binary in a foreign mount namespace is not verified"
            );
        }
    }

    #[test]
    fn a_system_binary_is_verified_when_gorca_runs_in_a_private_unit_namespace() {
        // gorca as a hardened systemd unit: mount and user namespaces of its own; the peer is
        // an ordinary session process (foreign mount namespace, initial user namespace).
        let mut c = spawn("/usr/bin/sleep");
        let pid = c.id();
        let theirs = mount_ns(&pid.to_string());
        let other = std::path::PathBuf::from("mnt:[1]");
        let init = std::path::PathBuf::from(INIT_USER_NS);
        let sandboxed_user = std::path::PathBuf::from("user:[4026539999]");
        let unit = identity_in(pid, theirs.as_deref(), Some(&other), Some(&init));
        let impostor = identity_in(pid, theirs.as_deref(), Some(&other), Some(&sandboxed_user));
        let unknown_user = identity_in(pid, theirs.as_deref(), Some(&other), None);
        let unreadable = identity_in(pid, None, None, Some(&init));
        let _ = c.kill();
        let _ = c.wait();
        assert_eq!(unit.as_deref(), Some("sleep"));
        assert_eq!(
            impostor, None,
            "a foreign user namespace needs sandbox evidence"
        );
        assert_eq!(unknown_user, None);
        assert_eq!(unreadable, None);
    }

    /// Probe for a live session (read-only): `GORCA_PROBE_PID=<pid> GORCA_PROBE_OURS=mnt:[N]
    /// cargo test -p gorca-connector-atspi --lib live_peer -- --ignored --nocapture` prints the
    /// identity gorca would verify for `pid` when it runs in the mount namespace `OURS`.
    #[test]
    #[ignore = "needs a live pid"]
    fn live_peer_identity_probe() {
        let Some(pid) = std::env::var("GORCA_PROBE_PID")
            .ok()
            .and_then(|p| p.parse::<u32>().ok())
        else {
            return;
        };
        let ours = std::env::var("GORCA_PROBE_OURS")
            .ok()
            .map(std::path::PathBuf::from);
        if ours.is_none() {
            // Run inside a transient unit: the whole real path, no injected values.
            let start = read_proc_info(pid).start.unwrap_or(0);
            println!(
                "private user ns: {}, identity of {pid}: {:?}",
                in_private_user_ns(),
                verified_identity(pid, start)
            );
            return;
        }
        let id = identity_in(
            pid,
            mount_ns(&pid.to_string()).as_deref(),
            ours.as_deref(),
            std::fs::read_link(format!("/proc/{pid}/ns/user"))
                .ok()
                .as_deref(),
        );
        println!("identity of {pid}: {id:?}");
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
        assert_eq!(
            parse_flatpak_info(info).as_deref(),
            Some("org.mozilla.firefox")
        );
        let scope = Some("app-flatpak-org.mozilla.firefox-1234.scope");
        assert_eq!(
            flatpak_identity(info, scope).as_deref(),
            Some("org.mozilla.firefox")
        );
        // The scope must agree with the info file; a mismatch or no scope is unverified.
        assert_eq!(
            flatpak_identity(info, Some("app-flatpak-org.evil.app-1.scope")),
            None
        );
        assert_eq!(flatpak_identity(info, None), None);
        // Case-sensitive: a differently-cased id is a different application.
        assert_eq!(
            flatpak_identity(info, Some("app-flatpak-org.mozilla.Firefox-1.scope")),
            None
        );
        assert_eq!(parse_flatpak_info("[Application]\nname=bad name\n"), None);
        assert_eq!(
            snap_identity("snap.chromium.chromium (enforce)\n").as_deref(),
            Some("chromium")
        );
        // Complain mode confines nothing: not evidence.
        assert_eq!(snap_identity("snap.chromium.chromium (complain)"), None);
        assert_eq!(snap_identity("snap.chromium.chromium"), None);
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
