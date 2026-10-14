//! Private Firefox for the harness.
//!
//! * Own profile directory under a temp dir (`--profile`), `--no-remote --new-instance`, so it can
//!   never attach to the user's Firefox.
//! * Accessibility forced on: `GNOME_ACCESSIBILITY=1` and `accessibility.force_disabled = 0`, with
//!   the private a11y bus found through the inherited `DBUS_SESSION_BUS_ADDRESS`.
//! * No proxy, no telemetry, no first-run pages, so a corpus page is the only thing it loads.
//! * It is the child of this process and the leader of its own process group; teardown signals
//!   only that group (never a name or pattern, CLAUDE.md "Live-session safety").
//!
//! The harness refuses to start unless the session looks private (see [`assert_private_session`]).
//! Firefox needs a Wayland compositor: run the harness inside a private headless gnome-shell
//! (`tests/web/b6/run_b6.sh`), never on the user's display.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::process::{Child, Command};

pub const USER_JS: &str = r#"
user_pref("accessibility.force_disabled", 0);
user_pref("browser.shell.checkDefaultBrowser", false);
user_pref("browser.startup.homepage_override.mstone", "ignore");
user_pref("browser.aboutwelcome.enabled", false);
user_pref("browser.tabs.warnOnClose", false);
user_pref("browser.warnOnQuit", false);
user_pref("datareporting.policy.dataSubmissionEnabled", false);
user_pref("datareporting.healthreport.uploadEnabled", false);
user_pref("toolkit.telemetry.enabled", false);
user_pref("toolkit.telemetry.reportingpolicy.firstRun", false);
user_pref("app.update.auto", false);
user_pref("app.update.enabled", false);
user_pref("extensions.update.enabled", false);
user_pref("network.proxy.type", 0);
user_pref("network.captive-portal-service.enabled", false);
user_pref("browser.search.update", false);
user_pref("browser.newtabpage.enabled", false);
user_pref("startup.homepage_welcome_url", "");
user_pref("startup.homepage_welcome_url.additional", "");
user_pref("browser.startup.page", 0);
user_pref("browser.sessionstore.resume_from_crash", false);
user_pref("browser.shell.didSkipDefaultBrowserCheckOnFirstRun", true);
user_pref("privacy.trackingprotection.enabled", false);
user_pref("security.sandbox.content.level", 4);
"#;

/// Refuse to run anywhere near the user's live session.
pub fn assert_private_session(a11y_bus_addr: &str) -> Result<()> {
    let xdg = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    if xdg.is_empty() || xdg.starts_with("/run/user/") {
        bail!(
            "XDG_RUNTIME_DIR={xdg:?} looks like the live session; run via tests/web/b6/run_b6.sh (private runtime dir)"
        );
    }
    if a11y_bus_addr.contains("/run/user/") {
        bail!("refusing to use the live a11y bus {a11y_bus_addr}");
    }
    if let Ok(w) = std::env::var("WAYLAND_DISPLAY")
        && (w == "wayland-0" || w == "wayland-1")
    {
        bail!(
            "WAYLAND_DISPLAY={w} looks like the live compositor; use the private headless gnome-shell"
        );
    }
    Ok(())
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BrowserKind {
    Firefox,
    /// Chromium / Chrome for Testing, `--headless=new` (registers on the a11y bus, TBD-WEB research).
    Chromium,
}

impl BrowserKind {
    /// Substring of the application name on the a11y bus.
    pub fn app_needle(self) -> &'static str {
        match self {
            BrowserKind::Firefox => "firefox",
            BrowserKind::Chromium => "chrom",
        }
    }
}

pub struct PrivateFirefox {
    child: Child,
    pub profile: PathBuf,
    pub pid: u32,
}

impl PrivateFirefox {
    pub fn launch(
        kind: BrowserKind,
        binary: &str,
        profile: &Path,
        url: &str,
        log: &Path,
    ) -> Result<Self> {
        std::fs::create_dir_all(profile).context("profile dir")?;
        let out = std::fs::File::create(log).context("browser log")?;
        let err = out.try_clone()?;
        let mut cmd = Command::new(binary);
        match kind {
            BrowserKind::Firefox => {
                std::fs::write(profile.join("user.js"), USER_JS).context("user.js")?;
                cmd.args(["--no-remote", "--new-instance", "--profile"])
                    .arg(profile)
                    .args(["--width", "1200", "--height", "800"])
                    .arg(url)
                    .env("GNOME_ACCESSIBILITY", "1")
                    .env("MOZ_ENABLE_WAYLAND", "1")
                    .env_remove("DISPLAY");
            }
            BrowserKind::Chromium => {
                cmd.args([
                    "--headless=new",
                    "--force-renderer-accessibility",
                    "--no-first-run",
                    "--no-default-browser-check",
                    "--disable-extensions",
                    "--disable-background-networking",
                    "--no-sandbox",
                    "--no-proxy-server",
                    "--window-size=1200,800",
                ])
                .arg(format!("--user-data-dir={}", profile.display()))
                .arg(url)
                .env("GNOME_ACCESSIBILITY", "1");
            }
        }
        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .process_group(0)
            .kill_on_drop(false)
            .spawn()
            .with_context(|| format!("spawning {binary}"))?;
        let pid = child.id().context("browser pid")?;
        Ok(Self {
            child,
            profile: profile.to_owned(),
            pid,
        })
    }

    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Signal our own process group: TERM, a grace period, then KILL.
    pub async fn shutdown(mut self) {
        let group = format!("-{}", self.pid);
        let _ = Command::new("kill")
            .args(["-TERM", "--", &group])
            .status()
            .await;
        let waited = tokio::time::timeout(Duration::from_secs(4), self.child.wait()).await;
        if waited.is_err() {
            let _ = Command::new("kill")
                .args(["-KILL", "--", &group])
                .status()
                .await;
            let _ = tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await;
        }
        // Content processes can outlive the parent for a moment; the group kill is idempotent.
        let _ = Command::new("kill")
            .args(["-KILL", "--", &group])
            .status()
            .await;
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_session_paths_are_refused() {
        // The test runner's own XDG_RUNTIME_DIR is normally the live one.
        let xdg = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
        let r = assert_private_session("unix:path=/run/user/1000/at-spi/bus");
        assert!(r.is_err());
        if xdg.starts_with("/run/user/") {
            assert!(assert_private_session("unix:path=/tmp/x/at-spi/bus").is_err());
        }
    }

    #[test]
    fn user_js_forces_accessibility_and_no_proxy() {
        assert!(USER_JS.contains("accessibility.force_disabled\", 0"));
        assert!(USER_JS.contains("network.proxy.type\", 0"));
    }
}
