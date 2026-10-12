# gorca-helper@gorca

GNOME Shell 48 extension giving gorca in-memory screen crops and window geometry
(`docs/platform/org.gorca.ShellHelper.md`, report `docs/reports/2026-10-12-screen-capture-options.md`).

## Test without touching the real session (no logout)
    contrib/gnome-shell-extension/headless-shell.sh /tmp/gh /usr/bin/gjs-console=/user.slice/.../app.slice/X.service   # private bus + headless shell, extension preloaded
    # read DBUS_SESSION_BUS_ADDRESS from the new gnome-shell's /proc/<pid>/environ, then call the interface
Set `GORCA_TRACE=1` for per-call timing in the shell log. Kill only the gnome-shell whose environ contains your workdir.

## Install for the real session (this user only)
    contrib/gnome-shell-extension/dev.sh install      # copies to ~/.local/share/gnome-shell/extensions/
    # GNOME Shell scans extensions ONLY at startup: log out and in once.
    gsettings set org.gnome.shell disable-user-extensions false    # currently TRUE on this host
    contrib/gnome-shell-extension/dev.sh enable
    # then create /etc/gorca/shell-helper.json (root-owned, preferred) or ~/.config/gorca/shell-helper.json with exe AND full cgroup path, see the platform doc. Access test: test-access.sh
Remove: `dev.sh uninstall [--purge]`. Disable only: `dev.sh disable`.
Production install should use `GORCA_HELPER_DEV=0 dev.sh install` (no `.dev` marker, so no hot re-import).
