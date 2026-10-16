//! M4b end to end: window activation, dialogs, announcements and Shell notifications through the
//! real daemon wiring, against a fake AT-SPI application and a mock speech server, in private
//! sessions (`tests/harness`). Needs the harness prerequisites; without them each test prints
//! `SKIPPED` and passes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods
)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gorca_daemon::{Options, SpeechMode, run_with};
use gorca_rt::CancellationToken;
use gorca_testkit::MockSpeechServer;
use gorca_testkit::harness::{FakeApp, Session};

fn session() -> Option<Session> {
    if let Some(why) = Session::unavailable_reason() {
        eprintln!("SKIPPED (harness unavailable): {why}");
        return None;
    }
    Some(Session::start().expect("isolated session"))
}

fn sock_dir(session: &Session, name: &str) -> PathBuf {
    let d = session.dir().join("run").join(name);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run<F: std::future::Future<Output = ()>>(f: F) {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap()
        .block_on(f);
}

struct Rig {
    token: CancellationToken,
    join: tokio::task::JoinHandle<anyhow::Result<()>>,
    voicego: MockSpeechServer,
    app: FakeApp,
}

/// The fake app's tree: a window of two plain buttons (priming and a bystander), a dialog with
/// a text label and two buttons, a plain window, and a window with no focusable child.
const TREE: &str = r#""cmd":"build","tree":[
 {"name":"Main","role":"frame","children":[
   {"name":"Prime A","role":"push button","focusable":true},
   {"name":"Prime B","role":"push button","focusable":true},
   {"name":"Item 1","role":"push button","focusable":true},
   {"name":"Item 2","role":"push button","focusable":true}]},
 {"name":"Delete item?","role":"dialog","children":[
   {"name":"Item will be lost.","role":"label"},
   {"name":"Confirm","role":"push button","focusable":true},
   {"name":"Cancel","role":"push button","focusable":true}]},
 {"name":"Files","role":"frame","children":[
   {"name":"Back","role":"push button","focusable":true}]},
 {"name":"Settings","role":"frame","children":[
   {"name":"Static text","role":"label"}]},
 {"name":"OSD","role":"frame","children":[
   {"name":"Volume","role":"label"},
   {"name":"level","role":"level bar","value":[0.5,0.0,1.0]}]}
]"#;

/// Where the Shell policy runs in these tests. `GORCA_TEST_SHELL=native` forces the native build;
/// otherwise the `ext-gnome-shell` component is loaded when `cargo xtask build-ext` (`make ext`)
/// installed it, exactly as the daemon's default does, else the native build runs.
fn shell_runtime(shell: bool) -> (Option<PathBuf>, gorca_daemon::ShellPolicyMode) {
    use gorca_daemon::ShellPolicyMode;
    if !shell || std::env::var("GORCA_TEST_SHELL").is_ok_and(|v| v == "native") {
        return (None, ShellPolicyMode::Native);
    }
    let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"),
        PathBuf::from,
    );
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let dir = target.join(profile).join("extensions");
    if dir.join("ext-gnome-shell/ext_gnome_shell.wasm").is_file() {
        eprintln!("shell policy under test: WASM component ({})", dir.display());
        (Some(dir), ShellPolicyMode::Auto)
    } else {
        eprintln!("shell policy under test: native build (no component in {})", dir.display());
        (None, ShellPolicyMode::Native)
    }
}

async fn start(session: &Session, shell: bool, speak_ms: u64) -> Rig {
    // Logs go to stderr when `GORCA_LOG` is set (a failure under load is diagnosed from them).
    let _ = gorca_daemon::init_tracing();
    let voicego = MockSpeechServer::start_voicego(&sock_dir(session, "v"))
        .await
        .unwrap();
    voicego.set_speak_time(Duration::from_millis(speak_ms));
    let ssip_path = sock_dir(session, "s").join("none.sock");
    let mut args = vec!["--scenario", "review", "--name"];
    args.push(if shell { "gnome-shell" } else { "desktop" });
    if shell {
        args.push("--own-shell-name");
    }
    let mut app = FakeApp::start(session, &args).unwrap();
    app.cmd(&TREE.replace('\n', "")).unwrap();
    let (ext_dir, shell_policy) = shell_runtime(shell);
    let opts = Options {
        speech: SpeechMode::Voicego,
        ext_dir,
        shell_policy,
        ..Options::isolated(
            session.a11y_address(),
            session.session_address(),
            voicego.socket_path(),
            &ssip_path,
        )
    };
    let token = CancellationToken::new();
    let join = tokio::spawn(run_with(opts, token.clone()));
    let mut rig = Rig {
        token,
        join,
        voicego,
        app,
    };
    prime(&mut rig).await;
    rig
}

/// Focus the buttons alternately until the daemon speaks: events sent before its match rules are
/// installed are lost by design, so a fixed sleep would be a race.
async fn prime(rig: &mut Rig) {
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut flip = false;
    loop {
        flip = !flip;
        rig.app
            .focus(if flip { "Prime A" } else { "Prime B" })
            .unwrap();
        if rig.voicego.wait_spoken(1, Duration::from_millis(400)).await {
            break;
        }
        assert!(Instant::now() < deadline, "the daemon never became ready");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
}

impl Rig {
    async fn stop(self) {
        drop(self.app);
        self.token.cancel();
        let r = tokio::time::timeout(Duration::from_secs(10), self.join).await;
        assert!(matches!(r, Ok(Ok(Ok(())))), "{r:?}");
    }

    fn spoken_since(&self, mark: usize) -> Vec<String> {
        self.voicego.spoken()[mark..].to_vec()
    }

    /// Wait until `pred` holds over the utterances after `mark`.
    async fn wait(
        &self,
        mark: usize,
        timeout: Duration,
        pred: impl Fn(&[String]) -> bool,
    ) -> Vec<String> {
        let t0 = Instant::now();
        loop {
            let said = self.spoken_since(mark);
            if pred(&said) || t0.elapsed() > timeout {
                return said;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

fn joined(said: &[String]) -> String {
    said.join(" | ")
}

/// Position of `needle` in the joined transcript.
fn at(said: &[String], needle: &str) -> Option<usize> {
    joined(said).find(needle)
}

#[test]
fn a_dialog_opening_speaks_its_title_its_text_and_the_focused_button_in_that_order() {
    let Some(session) = session() else { return };
    run(async {
        let mut rig = start(&session, false, 30).await;
        let mark = rig.voicego.spoken().len();
        rig.app
            .cmd(r#""cmd":"activate","node":"Delete item?","focus":"Confirm","gap_ms":3"#)
            .unwrap();
        let full = "Delete item?, dialog, Item will be lost., Confirm, button";
        let said = rig
            .wait(mark, Duration::from_secs(5), |s| {
                s.last().map(String::as_str) == Some(full)
            })
            .await;
        // Exactly two shapes are correct, whatever the load. The window read beat the 10 ms hold:
        // one utterance in reading order. It missed it: the focus (with whatever was known of
        // the window) went first and the whole line followed once, in reading order, so the last
        // thing heard is never out of order. Anything else (the text after the button, a missing
        // or doubled line) is a bug.
        let shape: Vec<&str> = said.iter().map(String::as_str).collect();
        match shape.as_slice() {
            [one] => assert_eq!(*one, full, "{said:?}"),
            [first, last] => {
                assert!(
                    matches!(
                        *first,
                        "Confirm, button" | "Delete item?, dialog, Confirm, button"
                    ),
                    "the focus goes first when the read missed the hold: {said:?}"
                );
                assert_eq!(*last, full, "{said:?}");
            }
            other => panic!("not one of the two shapes: {other:?}"),
        }
        rig.stop().await;
    });
}

#[test]
fn a_plain_window_speaks_its_title_with_the_focus_and_a_window_without_focus_speaks_alone() {
    let Some(session) = session() else { return };
    run(async {
        let mut rig = start(&session, false, 30).await;
        let mark = rig.voicego.spoken().len();
        rig.app
            .cmd(r#""cmd":"activate","node":"Files","focus":"Back","gap_ms":3"#)
            .unwrap();
        let full = "Files, Back, button";
        let said = rig
            .wait(mark, Duration::from_secs(5), |s| {
                s.last().map(String::as_str) == Some(full)
            })
            .await;
        let shape: Vec<&str> = said.iter().map(String::as_str).collect();
        match shape.as_slice() {
            [one] => assert_eq!(*one, full, "{said:?}"),
            [first, last] => {
                // The read missed the hold: the focus first, then the line once in reading order.
                assert_eq!(*first, "Back, button", "{said:?}");
                assert_eq!(*last, full, "{said:?}");
            }
            other => panic!("not one of the two shapes: {other:?}"),
        }
        assert!(
            at(&said, "dialog").is_none(),
            "a plain window is not a dialog: {said:?}"
        );

        let mark = rig.voicego.spoken().len();
        rig.app
            .cmd(r#""cmd":"activate","node":"Settings""#)
            .unwrap();
        let said = rig
            .wait(mark, Duration::from_secs(5), |s| {
                at(s, "Settings").is_some()
            })
            .await;
        assert_eq!(
            joined(&said),
            "Settings",
            "no focus followed: the title alone"
        );
        rig.stop().await;
    });
}

#[test]
fn an_announcement_is_spoken_after_the_focus_speech_and_never_cuts_it_off() {
    let Some(session) = session() else { return };
    run(async {
        // Long utterances, so there is a speech to interrupt.
        let mut rig = start(&session, false, 500).await;
        tokio::time::sleep(Duration::from_millis(700)).await;
        let mark = rig.voicego.spoken().len();
        let vmark = rig.voicego.verbs().len();
        rig.app.focus("Item 1").unwrap();
        let said = rig
            .wait(mark, Duration::from_secs(3), |s| !s.is_empty())
            .await;
        assert_eq!(said, ["Item 1, button"]);
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.app
            .cmd(r#""cmd":"announce","node":"Main","text":"Download complete","politeness":1"#)
            .unwrap();
        let said = rig
            .wait(mark, Duration::from_secs(5), |s| s.len() >= 2)
            .await;
        assert_eq!(said, ["Item 1, button", "Download complete"], "{said:?}");
        let recs = rig.voicego.spoken_records();
        let gap = recs[mark + 1].at.duration_since(recs[mark].at);
        assert!(
            gap >= Duration::from_millis(400),
            "the announcement waited for the focus speech to end: {gap:?}"
        );
        let verbs = rig.voicego.verbs()[vmark..].to_vec();
        assert!(
            verbs.iter().all(|v| !v.starts_with("STOP")),
            "nothing was cut off: {verbs:?}"
        );
        rig.stop().await;
    });
}

#[test]
fn a_new_focus_is_not_held_back_by_a_queued_announcement() {
    let Some(session) = session() else { return };
    run(async {
        let mut rig = start(&session, false, 500).await;
        tokio::time::sleep(Duration::from_millis(700)).await;
        let mark = rig.voicego.spoken().len();
        rig.app.focus("Item 1").unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        rig.app
            .cmd(r#""cmd":"announce","node":"Main","text":"Battery low","politeness":2"#)
            .unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        let t0 = Instant::now();
        rig.app.focus("Item 2").unwrap();
        let said = rig
            .wait(mark, Duration::from_secs(3), |s| {
                s.iter().any(|t| t == "Item 2, button")
            })
            .await;
        assert!(
            said.iter().any(|t| t == "Item 2, button"),
            "the new focus speaks: {said:?}"
        );
        assert!(
            t0.elapsed() < Duration::from_millis(300),
            "and at once, not behind the announcement: {:?}",
            t0.elapsed()
        );
        // The assertive announcement outlives the focus change and is spoken afterwards.
        let said = rig
            .wait(mark, Duration::from_secs(5), |s| {
                s.iter().any(|t| t == "Battery low")
            })
            .await;
        assert!(
            at(&said, "Item 2, button") < at(&said, "Battery low"),
            "{said:?}"
        );
        rig.stop().await;
    });
}

#[test]
fn a_notification_banner_in_the_shell_is_read_and_waits_for_the_speech_in_progress() {
    let Some(session) = session() else { return };
    run(async {
        let mut rig = start(&session, true, 500).await;
        tokio::time::sleep(Duration::from_millis(700)).await;
        let mark = rig.voicego.spoken().len();
        rig.app.focus("Item 1").unwrap();
        let said = rig
            .wait(mark, Duration::from_secs(3), |s| !s.is_empty())
            .await;
        assert_eq!(said, ["Item 1, button"]);
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.app
            .cmd(concat!(
                r#""cmd":"popup","parent":"Main","tree":{"name":"Alice","role":"notification","children":["#,
                r#"{"name":"Alice","role":"label"},{"name":"Are you coming tonight?","role":"label"},"#,
                r#"{"name":"Reply","role":"push button","focusable":true}]}"#
            ))
            .unwrap();
        let said = rig
            .wait(mark, Duration::from_secs(5), |s| s.len() >= 2)
            .await;
        assert_eq!(
            said,
            ["Item 1, button", "alert, Alice, Are you coming tonight?"],
            "role, title and body; the action button is not read: {said:?}"
        );
        let recs = rig.voicego.spoken_records();
        let gap = recs[mark + 1].at.duration_since(recs[mark].at);
        assert!(gap >= Duration::from_millis(400), "{gap:?}");
        rig.stop().await;
    });
}

#[test]
fn the_shells_osd_level_is_announced_once_when_it_stops_changing() {
    let Some(session) = session() else { return };
    run(async {
        let mut rig = start(&session, true, 30).await;
        let mark = rig.voicego.spoken().len();
        // The level bar animates 0.5 -> 0.75 in a dozen steps, the way St eases it.
        for i in 1..=8 {
            let v = 0.5 + 0.25 * f64::from(i) / 8.0;
            rig.app
                .cmd(&format!(r#""cmd":"set_value","node":"level","value":{v}"#))
                .unwrap();
            tokio::time::sleep(Duration::from_millis(17)).await;
        }
        let said = rig
            .wait(mark, Duration::from_secs(5), |s| !s.is_empty())
            .await;
        assert_eq!(said, ["Volume, 75 percent"], "{said:?}");
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(
            rig.spoken_since(mark),
            ["Volume, 75 percent"],
            "once, not per step"
        );
        rig.stop().await;
    });
}
