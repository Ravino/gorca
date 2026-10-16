"""The M4b probe scenarios (driven by probe_shell.py). Each trigger returns a detail dict."""
import json
import os
import sys
import time

import probe_shell as P  # noqa: F401  (imported as a module by name when run as script: see below)

K = dict(ESC=1, TAB=15, ENTER=28, A=30, D=32, E=18, G=34, ALT=56, F2=60, META=125, DOWN=108, RIGHT=106, SPACE=57,
         LEFTCTRL=29)


def run(pr, only):
    def want(n):
        return not only or n in only

    # ---- identity (no trigger) -------------------------------------------------------------
    ident = pr.identity()
    with open(os.path.join(pr.out, "identity.json"), "w") as f:
        json.dump(P.jsonable(ident), f, indent=1, default=str)
    print("IDENTITY", json.dumps(P.jsonable(ident), default=str)[:3000], file=sys.stderr, flush=True)
    with open(os.path.join(pr.out, "registration.json"), "w") as f:
        json.dump(pr.cap.reg_log, f, indent=1)
    if not pr.shell_bus:
        print("FATAL: Shell is not on the a11y bus", file=sys.stderr)
        return
    print("RD", pr.rd_start(), file=sys.stderr, flush=True)

    print("SETTLED (startup) after s:", pr.settle(), file=sys.stderr, flush=True)
    # GNOME 48 starts a session IN the Overview; leave it, as a user would, and give the Shell
    # some user activity (the notification banner timeout waits for it)
    print("INITIAL STATE", pr.shell_state(), file=sys.stderr, flush=True)
    pr.eval("Main.overview.hide(); 1")
    time.sleep(1)
    pr.activity()
    # hiding the Overview makes the Shell rebuild a lot of its tree a few seconds later (lazy idle work)
    print("SETTLED (overview hidden) after s:", pr.settle(6.0, 60), "state", pr.shell_state(), file=sys.stderr, flush=True)
    # eval availability
    ev = pr.eval("'ok'")
    print("EVAL", ev, file=sys.stderr, flush=True)

    if want("S0_idle"):
        pr.scenario("S0_idle", lambda m: time.sleep(3), min_s=3, quiet_s=3, max_s=4, note="no trigger; idle noise")

    # ---- notifications ---------------------------------------------------------------------
    st = {}

    def n1(m):
        m("Notify call")
        st["n1"] = pr.notify("Probe title", "Probe body text of the first notification")
        m(f"Notify returned id={st['n1']}")
        pr.wait_for("banner shown", lambda: (pr.shell_state() or {}).get("trayState") == 2, 15, 0.2)
        pr.peak("banner_shown")
        time.sleep(6.5)
        m(f"6.5 s without user input: {pr.shell_state()}")
        pr.activity()
        m("user activity (Shift tap): the banner timeout can now run")
        time.sleep(1.0)
        return {"id": st["n1"]}

    if want("N1_notification"):
        pr.scenario("N1_notification", n1, min_s=8, quiet_s=5.0, max_s=40,
                    note="title+body, no actions, default expiry; waits for the banner to expire")

    def n2(m):
        st["n2"] = pr.notify("Action title", "Body with actions", actions='["default","Open","reply","Reply"]',
                             hints="{'urgency': <byte 1>}")
        m(f"Notify returned id={st['n2']}")
        pr.wait_for("banner shown", lambda: (pr.shell_state() or {}).get("trayState") == 2, 15, 0.2)
        pr.peak("banner_shown")
        time.sleep(1.0)
        pr.activity()
        m("user activity (Shift tap)")
        return {"id": st["n2"]}

    if want("N2_notification_actions"):
        pr.scenario("N2_notification_actions", n2, min_s=8, quiet_s=5.0, max_s=40, note="with default+named action")

    def n3(m):
        pr.activity()
        a = pr.notify("First showing", "first body")
        m(f"first id={a}")
        pr.wait_for("banner", lambda: any("First showing" in (v.get("name") or "")
                                           for v in P.Snap(pr.peer, pr.shell_bus).nodes.values()), 15, 0.3)
        m("banner visible in tree (poll)")
        pr.peak("first_banner_shown")
        b = pr.notify("Second arrives", "second body")
        m(f"second id={b}")
        time.sleep(1.0)
        pr.peak("second_queued")
        m(f"state with the second queued: {pr.shell_state()}")
        c = pr.notify("Second arrives", "replaced body", replaces=b)
        m(f"replace of {b} -> id {c}")
        time.sleep(1.5)
        pr.peak("after_replace")
        pr.activity()
        m("user activity (Shift tap): the first banner may expire now, the queued one should follow")
        ready = lambda: (lambda s: s and s.get("queue") == 0 and s.get("banner"))(pr.shell_state())  # noqa: E731
        got = pr.wait_for("second banner after the first expired", lambda: ready() and not any(
            "First showing" in (v.get("name") or "") for v in P.Snap(pr.peer, pr.shell_bus).nodes.values() if "showing" in v["states"]), 25, 0.3)
        m(f"second banner followed the natural expiry: {bool(got)}; state {pr.shell_state()}")
        if not got:
            pr.close_notification(a)
            m("fallback: CloseNotification(first)")
            got = pr.wait_for("second banner after CloseNotification", ready, 15, 0.2)
            m(f"second banner after CloseNotification: {bool(got)}; state {pr.shell_state()}")
        time.sleep(1.0)
        pr.peak("second_banner_shown")
        for i in (b, c):
            pr.close_notification(i)
        m("CloseNotification(second)")
        return {"a": a, "b": b, "c": c}

    if want("N3_second_and_replace"):
        pr.scenario("N3_second_and_replace", n3, min_s=3, quiet_s=3.0, max_s=30,
                    note="second notification while one shows; then replaces_id update; then close")

    def n4(m):
        i = pr.notify("Critical title", "critical body", hints="{'urgency': <byte 2>}")
        m(f"id={i}")
        pr.wait_for("banner shown", lambda: (pr.shell_state() or {}).get("trayState") == 2, 15, 0.2)
        pr.peak("banner_shown")
        time.sleep(1.0)
        pr.activity()
        m("user activity (Shift tap)")
        return {"id": i}

    if want("N4_critical"):
        pr.scenario("N4_critical", n4, min_s=4, quiet_s=3.0, max_s=15, note="urgency=critical (does not expire)")
        pr.eval("Main.messageTray.getSources().forEach(s=>s.destroy()); 1")
        pr.settle(2.0, 20)

    # ---- OSD -------------------------------------------------------------------------------
    def osd1(m):
        r = pr.gdbus("--dest", "org.gnome.Shell", "--object-path", "/org/gnome/Shell", "--method",
                     "org.gnome.Shell.ShowOSD",
                     "{'icon': <'audio-volume-high-symbolic'>, 'label': <'Volume'>, 'level': <0.5>}")
        m("ShowOSD(level .5) returned")
        time.sleep(0.5)
        pr.peak("osd_volume_shown")
        time.sleep(0.3)
        pr.gdbus("--dest", "org.gnome.Shell", "--object-path", "/org/gnome/Shell", "--method",
                 "org.gnome.Shell.ShowOSD",
                 "{'icon': <'audio-volume-high-symbolic'>, 'label': <'Volume'>, 'level': <0.75>}")
        m("ShowOSD(level .75) returned (update while showing)")
        time.sleep(0.8)
        pr.gdbus("--dest", "org.gnome.Shell", "--object-path", "/org/gnome/Shell", "--method",
                 "org.gnome.Shell.ShowOSD",
                 "{'icon': <'display-brightness-symbolic'>, 'label': <'Brightness'>, 'level': <0.3>}")
        m("ShowOSD(brightness .3) returned (different osd, same window)")
        return {"first_reply": r.stdout.strip() + r.stderr.strip()}

    if want("O1_osd"):
        pr.scenario("O1_osd", osd1, min_s=5, quiet_s=2.5, max_s=15, note="org.gnome.Shell.ShowOSD volume then update then brightness")

    def osd2(m):
        pr.gdbus("--dest", "org.gnome.Shell", "--object-path", "/org/gnome/Shell", "--method",
                 "org.gnome.Shell.ShowOSD", "{'icon': <'audio-volume-muted-symbolic'>, 'label': <'Muted'>}")
        m("ShowOSD(no level) returned")
        time.sleep(0.6)
        pr.peak("osd_nolevel_shown")
        return {}

    if want("O2_osd_nolevel"):
        pr.scenario("O2_osd_nolevel", osd2, min_s=4, quiet_s=2.5, max_s=12, note="OSD without a level (mute)")

    # ---- overview / search / app grid ------------------------------------------------------
    def ov_open(m):
        r = pr.eval("Main.overview.show(); 1")
        m(f"overview.show() eval -> {r}")
        time.sleep(2.0)
        pr.peak("overview_shown")
        r = pr.eval("Main.overview.hide(); 1")
        m(f"overview.hide() eval -> {r}")
        return {}

    if want("V1_overview_eval"):
        pr.scenario("V1_overview_eval", ov_open, min_s=3, quiet_s=2, max_s=15, note="Overview open (2s) then close, via Eval")

    def ov_key(m):
        pr.tap(K["META"])
        m("Super tap")
        time.sleep(2.0)
        pr.peak("overview_shown")
        pr.tap(K["META"])
        m("Super tap (close)")
        return {"overview_visible_after": pr.eval("Main.overview.visible")}

    if want("V2_overview_super_key"):
        pr.scenario("V2_overview_super_key", ov_key, min_s=3, quiet_s=2, max_s=15, note="Super key via RemoteDesktop")

    def ov_search(m):
        pr.eval("Main.overview.show(); 1")
        m("overview.show()")
        time.sleep(2.0)
        for k in (K["G"], K["E"], K["D"]):
            pr.tap(k)
            m(f"typed key {k}")
            time.sleep(0.4)
        m("typed 'ged'")
        time.sleep(1.5)
        pr.peak("search_results")
        pr.tap(K["DOWN"])
        m("Down (move through results)")
        time.sleep(0.8)
        pr.tap(K["ESC"])
        m("Escape")
        return {"search_text": pr.eval("Main.overview.searchEntry.text"),
                "windows": pr.windows()}

    if want("V3_overview_search"):
        pr.scenario("V3_overview_search", ov_search, min_s=3, quiet_s=2, max_s=20, note="type 'ged' into overview search; Down; Esc")

    def appgrid(m):
        pr.eval("Main.overview.show(); 1")
        m("overview.show()")
        time.sleep(2)
        r = pr.eval("Main.overview.dash.showAppsButton.checked = true; 1")
        m(f"showAppsButton.checked -> {r}")
        time.sleep(2.5)
        pr.peak("app_grid")
        pr.tap(K["ESC"])
        m("Escape")
        time.sleep(1)
        pr.eval("Main.overview.hide(); 1")
        m("overview.hide()")
        return {}

    if want("V4_app_grid"):
        pr.scenario("V4_app_grid", appgrid, min_s=3, quiet_s=2, max_s=20, note="app grid via the Show Apps button")

    # ---- client windows + switcher ---------------------------------------------------------
    def need_clients():
        if st.get("clients"):
            return
        st["clients"] = True
        pr.spawn_client(["/usr/bin/python3", os.path.join(P.HERE, "gtkwin.py"), "Probe Window One"])
        pr.spawn_client(["/usr/bin/gedit", "--standalone"])
        got = pr.wait_for("two windows", lambda: (lambda w: w if isinstance(w, list) and len(w) >= 2 else None)(pr.windows()), 90, 0.5)
        print("WINDOWS", got, file=sys.stderr, flush=True)
        pr.click()
        time.sleep(1.5)
        print("WINDOWS", pr.windows(), file=sys.stderr, flush=True)

    if want("W1_windows_appear") or want("T1_alt_tab") or want("T2_alt_grave"):
        if want("W1_windows_appear"):
            def w1(m):
                need_clients()
                m("two client windows up")
                return {"windows": pr.windows()}
            pr.scenario("W1_windows_appear", w1, min_s=3, quiet_s=3, max_s=120, note="spawn gtk3 window + gedit (client side events)")
        else:
            need_clients()

    def alttab(m):
        pr.key(K["ALT"], True)
        m("Alt down")
        time.sleep(0.15)
        pr.tap(K["TAB"])
        m("Tab (switcher opens, selects 2nd app)")
        time.sleep(1.5)
        pr.peak("switcher_open")
        pr.tap(K["TAB"])
        m("Tab again (cycles)")
        time.sleep(1.0)
        pr.key(K["ALT"], False)
        m("Alt up (commit)")
        return {"windows": pr.windows()}

    if want("T1_alt_tab"):
        pr.scenario("T1_alt_tab", alttab, min_s=3, quiet_s=2, max_s=20, note="Alt+Tab app switcher with 2 apps")

    def altgrave(m):
        pr.key(K["ALT"], True)
        time.sleep(0.15)
        pr.tap(41)
        m("grave (window switcher)")
        time.sleep(1.2)
        pr.peak("window_switcher_open")
        pr.key(K["ALT"], False)
        m("Alt up")
        return {"windows": pr.windows()}

    if want("T2_alt_grave"):
        pr.scenario("T2_alt_grave", altgrave, min_s=3, quiet_s=2, max_s=20, note="Alt+` same-app window switcher")

    # ---- quick settings / dialogs ---------------------------------------------------------
    def qs(m):
        r = pr.eval("Main.panel.statusArea.quickSettings.menu.open(true); 1")
        m(f"quickSettings.menu.open -> {r}")
        time.sleep(1.5)
        pr.peak("qs_open")
        pr.tap(K["TAB"])
        m("Tab")
        time.sleep(0.5)
        pr.tap(K["TAB"])
        m("Tab")
        time.sleep(0.5)
        pr.tap(K["ESC"])
        m("Escape")
        return {"menu_open_after": pr.eval("Main.panel.statusArea.quickSettings.menu.isOpen")}

    if want("Q1_quick_settings"):
        pr.scenario("Q1_quick_settings", qs, min_s=3, quiet_s=2, max_s=15, note="quick settings menu open, Tab Tab, Esc")

    def rundlg(m):
        pr.tap(K["ALT"], K["F2"])
        m("Alt+F2")
        time.sleep(1.5)
        pr.peak("run_dialog_open")
        vis = pr.eval("Main.runDialog && Main.runDialog.visible")
        m(f"runDialog visible -> {vis}")
        if not (vis and vis[0] and vis[1] == "true"):
            pr.eval("Main.openRunDialog(); 1")
            m("fallback Main.openRunDialog()")
            time.sleep(1.5)
        pr.tap(K["ESC"])
        m("Escape")
        return {}

    if want("D1_run_dialog"):
        pr.scenario("D1_run_dialog", rundlg, min_s=3, quiet_s=2, max_s=15, note="Run dialog: Alt+F2 then Esc")

    def endsess(m):
        r = pr.gdbus("--dest", "org.gnome.Shell", "--object-path", "/org/gnome/SessionManager/EndSessionDialog",
                     "--method", "org.gnome.SessionManager.EndSessionDialog.Open", "0", "0", "0", "[]")
        m(f"EndSessionDialog.Open(logout) -> {r.stdout.strip()} {r.stderr.strip()}")
        time.sleep(2)
        pr.peak("end_session_dialog_open")
        pr.tap(K["TAB"])
        m("Tab inside dialog")
        time.sleep(0.6)
        pr.tap(K["ESC"])
        m("Escape")
        return {}

    if want("D2_end_session_dialog"):
        pr.scenario("D2_end_session_dialog", endsess, min_s=3, quiet_s=2, max_s=15, note="real Shell ModalDialog via EndSessionDialog D-Bus")

    def polkit(m):
        js = ("imports.gi.PolkitAgent.Session.prototype.initiate = function() {}; "
              "const a = Main.componentManager._allComponents.polkitAgent; "
              "a._onInitiate(null, 'org.gorca.probe.run', 'Authentication is needed to run /usr/bin/probe as the super user', "
              "'dialog-password-symbolic', 'cookie-probe', [imports.gi.GLib.get_user_name()]); "
              # the stubbed Session.initiate never emits the PAM 'request' signal that opens the dialog:
              # emit the handler's input by hand ("Password:" prompt, echo off)
              "a._currentDialog._onSessionRequest(null, 'Password:', false); 'shown'")
        r = pr.eval(js)
        m(f"polkit _onInitiate -> {r}")
        time.sleep(2.5)
        pr.peak("polkit_dialog_open")
        pr.tap(K["ESC"])
        m("Escape")
        pr.eval("Main.componentManager._allComponents.polkitAgent._currentDialog && "
                "Main.componentManager._allComponents.polkitAgent._currentDialog.close(); 1")
        return {"eval": r}

    if want("D3_polkit_dialog"):
        pr.scenario("D3_polkit_dialog", polkit, min_s=3, quiet_s=2, max_s=15,
                    note="synthetic: Shell's real AuthenticationDialog, PolkitAgent.Session.initiate stubbed")

    def lock(m):
        before = pr.sess_lockhint()
        m(f"host session LockedHint before (read only): {before}")
        # Main.screenShield is null in the private Shell: LoginManager.canLock() needs
        # org.gnome.DisplayManager >= 3.5.91 on the (host) system bus and the host's fake one says
        # 1.0. Build the real ScreenShield by hand (dynamic import inside Eval) and neuter its
        # SetLockedHint so the host logind session is never touched.
        r = pr.eval("import('resource:///org/gnome/shell/ui/screenShield.js').then(m => { globalThis.__ss = new m.ScreenShield(); })"
                    ".catch(e => { globalThis.__err = String(e); }); 'importing'")
        m(f"create ScreenShield -> {r}")
        ok = pr.wait_for("ScreenShield._loginSession", lambda: (lambda x: x and x[0] and x[1] == '"yes"')(
            pr.eval("globalThis.__ss && globalThis.__ss._loginSession ? 'yes' : 'no'")), 30, 0.5)
        r = pr.eval("globalThis.__ss._loginSession.SetLockedHintAsync = () => Promise.resolve(); 'patched'")
        m(f"SetLockedHintAsync neutered -> {r} (ready={bool(ok)})")
        if not ok:
            return {"error": "ScreenShield did not become ready", "err": pr.eval("String(globalThis.__err)")}
        pr.activity()
        time.sleep(0.5)
        r = pr.eval("globalThis.__ss.lock(true); 'lock called'")
        m(f"ScreenShield.lock(true) -> {r}")
        time.sleep(3)
        m(f"state: {pr.shell_state()}")
        pr.peak("lock_shield")
        pr.tap(K["SPACE"])
        m("Space (wake the shield: shows the unlock prompt)")
        time.sleep(3)
        m(f"state: {pr.shell_state()}")
        pr.peak("lock_prompt")
        pr.tap(K["ESC"])
        m("Escape")
        time.sleep(2)
        pr.eval("globalThis.__ss.deactivate(true); 1")
        m("ScreenShield.deactivate(true)")
        time.sleep(2)
        m(f"state: {pr.shell_state()}")
        return {"locked_hint_before": before, "locked_hint_after": pr.sess_lockhint()}

    if want("L1_lock_screen"):
        pr.scenario("L1_lock_screen", lock, min_s=4, quiet_s=3.0, max_s=60,
                    note="real ScreenShield built by hand (canLock() is false here), SetLockedHint neutered; host hint must not change")
