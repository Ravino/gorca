//! Handler tests: every implemented command, driven through `Reader::handle` (CR-1).

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_config::{Overlay, Presets, load};
use gorca_input_api::{CmdId, KeyClass, KeyEvent, KeyIntent, Modifiers, PressKind};
use gorca_mirror::{MirrorWriter, mirror};
use gorca_model::{
    AppId, ChildList, ConnectorId, Epoch, Freshness, Ifaces, LocalId, NodeKey, NodeSnapshot, Role,
    SnapshotBatch, StateSet,
};
use gorca_rt::EpochCell;

use super::*;
use crate::directory::MirrorDirectory;
use crate::mode::{BaseMode, Mode, ModeKeys};
use crate::reader::{QueryReply, Reader, ReaderEffect, ReaderEffects, ReaderMsg};

fn app() -> AppId {
    AppId {
        connector: ConnectorId(1),
        serial: 1,
    }
}

fn key(n: u64) -> NodeKey {
    NodeKey {
        app: app(),
        local: LocalId(n),
    }
}

fn snap(n: u64, role: Role, name: &str, parent: Option<u64>) -> Arc<NodeSnapshot> {
    Arc::new(NodeSnapshot {
        key: key(n),
        role,
        states: StateSet::empty(),
        name: Some(Arc::from(name)),
        description: None,
        value: None,
        parent: parent.map(key),
        children: ChildList::Unknown,
        interfaces: Ifaces::empty(),
        attrs: gorca_model::AttrMap::default(),
        relations: gorca_model::Relations::default(),
        bounds: None,
        have: gorca_model::PropMask::PRESENT,
        confirmed_seq: 0,
        fresh: Freshness::Fresh { at: Instant::now() },
    })
}

#[derive(Debug)]
struct FixedClock;

impl WallClock for FixedClock {
    fn now(&self) -> LocalTime {
        LocalTime {
            year: 2026,
            month: 10,
            day: 13,
            weekday: 1,
            hour: 14,
            minute: 5,
        }
    }
}

struct Fx {
    reader: Reader,
    writer: MirrorWriter,
    epoch: Arc<EpochCell>,
    now: Instant,
}

fn fixture() -> Fx {
    let epoch = Arc::new(EpochCell::new());
    let dir = Arc::new(MirrorDirectory::new());
    let (writer, handle) = mirror();
    dir.insert(app(), &handle);
    let reader = Reader::new(Arc::clone(&epoch), dir, ModeKeys::default())
        .with_wall_clock(Arc::new(FixedClock));
    Fx {
        reader,
        writer,
        epoch,
        now: Instant::now(),
    }
}

fn intent(cmd: CoreCmd, repeat: u8, epoch: u64) -> KeyIntent {
    KeyIntent {
        event: KeyEvent {
            keycode: 28,
            keysym: 0x74,
            unichar: 0x74,
            modifiers: Modifiers::empty(),
            pressed: true,
        },
        kind: PressKind::Press,
        class: KeyClass::Command(cmd.id()),
        repeat,
        chord: None,
        epoch: Epoch(epoch),
        at: Instant::now(),
    }
}

impl Fx {
    fn run(&mut self, cmd: CoreCmd, repeat: u8) -> ReaderEffects {
        let e = self.epoch.advance();
        self.reader
            .handle(ReaderMsg::Key(intent(cmd, repeat, e.0)), self.now)
    }

    fn control(&mut self, m: ControlMsg) -> ReaderEffects {
        self.reader.handle(ReaderMsg::Control(m), self.now)
    }

    fn focus(&mut self, n: u64) {
        self.writer.apply(
            SnapshotBatch {
                nodes: vec![snap(n, Role::Button, "OK", Some(2))],
                ..Default::default()
            },
            gorca_mirror::ApplyOrigin::Push,
        );
        self.reader.handle(
            ReaderMsg::Focus(crate::FocusLaneMsg::FocusChanged { node: key(n) }),
            self.now,
        );
    }
}

fn spoken(fx: &ReaderEffects) -> Vec<String> {
    fx.iter()
        .filter_map(|e| match e {
            ReaderEffect::Speak(s) => Some(
                s.utterance
                    .segments
                    .iter()
                    .map(|g| &*g.text)
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            ReaderEffect::Fetch { .. }
            | ReaderEffect::Demand { .. }
            | ReaderEffect::FocusSettled { .. }
            | ReaderEffect::QueryActiveWindowFocus
            | ReaderEffect::ArmTimer(_)
            | ReaderEffect::SetGrabSet(_)
            | ReaderEffect::SpeechCtl(_)
            | ReaderEffect::Config(_)
            | ReaderEffect::ReviewNav { .. }
            | ReaderEffect::ReviewBuild(_)
            | ReaderEffect::ReviewRevalidate { .. }
            | ReaderEffect::ReviewStatusBar { .. }
            | ReaderEffect::ReviewAct { .. } => None,
        })
        .collect()
}

fn ctls(fx: &ReaderEffects) -> Vec<SpeechCtl> {
    fx.iter()
        .filter_map(|e| match e {
            ReaderEffect::SpeechCtl(c) => Some(*c),
            ReaderEffect::Speak(_)
            | ReaderEffect::Fetch { .. }
            | ReaderEffect::Demand { .. }
            | ReaderEffect::FocusSettled { .. }
            | ReaderEffect::QueryActiveWindowFocus
            | ReaderEffect::ArmTimer(_)
            | ReaderEffect::SetGrabSet(_)
            | ReaderEffect::Config(_)
            | ReaderEffect::ReviewNav { .. }
            | ReaderEffect::ReviewBuild(_)
            | ReaderEffect::ReviewRevalidate { .. }
            | ReaderEffect::ReviewStatusBar { .. }
            | ReaderEffect::ReviewAct { .. } => None,
        })
        .collect()
}

fn loaded(text: &str, cause: LoadCause) -> Arc<LoadedSettings> {
    let l = load(Some(text), &Presets::builtin());
    let facts = PlatformFacts::default();
    let (mods, dropped) = gorca_modifiers(&l.settings, &facts);
    let problems: Vec<Arc<str>> = l.diags.iter().map(|d| Arc::from(d.to_string())).collect();
    Arc::new(LoadedSettings {
        cause,
        settings: l.settings,
        hash: l.hash,
        problems_hash: gorca_config::diags_hash(&l.diags),
        problems,
        newer_schema: l.newer_schema,
        mode_keys: ModeKeys {
            modifiers: mods,
            ..ModeKeys::default()
        },
        caps_lock_dropped: dropped,
        clock_24h: true,
    })
}

#[test]
fn every_implemented_command_does_something() {
    for cmd in CoreCmd::ALL {
        if !cmd.is_implemented() {
            continue;
        }
        let mut f = fixture();
        f.focus(1);
        // Give "repeat last" something to repeat.
        f.reader
            .handle(ReaderMsg::Timer, f.now + Duration::from_secs(1));
        let fx = f.run(cmd, 1);
        let quiet_ok = matches!(
            cmd,
            CoreCmd::SpeechStop | CoreCmd::SpeechPauseToggle | CoreCmd::SettingsReload
        );
        assert!(
            quiet_ok || !fx.is_empty(),
            "{} produced neither speech nor an effect",
            cmd.name()
        );
    }
}

#[test]
fn stop_and_pause_have_no_speech_but_pause_toggles_the_lane() {
    let mut f = fixture();
    let fx = f.run(CoreCmd::SpeechPauseToggle, 1);
    assert_eq!(ctls(&fx), [SpeechCtl::Pause]);
    let fx = f.run(CoreCmd::SpeechPauseToggle, 1);
    assert_eq!(ctls(&fx), [SpeechCtl::Resume]);
    assert!(f.run(CoreCmd::SpeechStop, 1).is_empty());
}

#[test]
fn ring_changes_send_the_levels_before_the_confirmation_and_clamp() {
    let mut f = fixture();
    let fx = f.run(CoreCmd::SpeechRingInc, 1);
    // SetParams first, then the words: "rate 55" is spoken at rate 55 (ADR-0005 6.2).
    assert!(matches!(
        fx.as_slice(),
        [
            ReaderEffect::SpeechCtl(SpeechCtl::SetParams(VoiceLevels {
                rate: 55,
                pitch: 50,
                volume: 100
            })),
            ReaderEffect::Speak(_)
        ]
    ));
    assert_eq!(spoken(&fx), ["rate 55"]);
    let fx = f.run(CoreCmd::SpeechRingDecLarge, 1);
    assert_eq!(spoken(&fx), ["rate 35"]);
    let fx = f.run(CoreCmd::SpeechRingNext, 1);
    assert_eq!(spoken(&fx), ["pitch 50"]);
    let fx = f.run(CoreCmd::SpeechRingInc, 1);
    assert_eq!(spoken(&fx), ["pitch 55"]);
    let fx = f.run(CoreCmd::SpeechVolumeUp, 1);
    assert_eq!(spoken(&fx), ["volume 100"], "clamped at the maximum");
    assert_eq!(f.reader.settings().speech.rate, 35);
    for _ in 0..30 {
        f.run(CoreCmd::SpeechRateDown, 1);
    }
    assert_eq!(f.reader.settings().speech.rate, 0);
}

#[test]
fn toggles_confirm_and_change_the_effective_settings_without_saving() {
    let mut f = fixture();
    assert_eq!(
        spoken(&f.run(CoreCmd::EchoTypedWordsToggle, 1)),
        ["speak typed words on"]
    );
    assert!(f.reader.settings().echo.typed_words);
    assert_eq!(
        spoken(&f.run(CoreCmd::EchoTypedCharsToggle, 1)),
        ["speak typed characters off"]
    );
    assert_eq!(
        spoken(&f.run(CoreCmd::EchoCommandKeysToggle, 1)),
        ["speak command keys on"]
    );
    assert_eq!(
        spoken(&f.run(CoreCmd::VerbosityCycle, 1)),
        ["verbosity verbose"]
    );
    assert_eq!(
        spoken(&f.run(CoreCmd::VerbosityCycle, 1)),
        ["verbosity brief"]
    );
    // Nothing is written anywhere: no Config effect from toggles (Q-1).
    assert!(
        f.run(CoreCmd::EchoTypedWordsToggle, 1)
            .iter()
            .all(|e| !matches!(e, ReaderEffect::Config(_)))
    );
}

#[test]
fn brief_confirmations_say_only_the_value() {
    let mut f = fixture();
    f.control(ControlMsg::Settings(SettingsMsg::Loaded(loaded(
        "[verbosity]\nconfirmations = \"brief\"\n",
        LoadCause::Startup,
    ))));
    assert_eq!(spoken(&f.run(CoreCmd::SpeechRingInc, 1)), ["55"]);
    assert_eq!(spoken(&f.run(CoreCmd::EchoTypedWordsToggle, 1)), ["on"]);
}

#[test]
fn typed_character_echo_follows_the_setting() {
    let mut f = fixture();
    let press = |epoch| {
        ReaderMsg::Key(KeyIntent {
            class: KeyClass::PassToApp,
            event: KeyEvent {
                unichar: 0x61,
                keysym: 0x61,
                ..intent(CoreCmd::SpeechStop, 1, 0).event
            },
            ..intent(CoreCmd::SpeechStop, 1, epoch)
        })
    };
    assert_eq!(spoken(&f.reader.handle(press(1), f.now)), ["a"]);
    f.run(CoreCmd::EchoTypedCharsToggle, 1);
    assert!(spoken(&f.reader.handle(press(9), f.now)).is_empty());
}

#[test]
fn speech_mode_cycle_speaks_before_muting_and_after_unmuting() {
    let mut f = fixture();
    let fx = f.run(CoreCmd::SpeechModeCycle, 1);
    assert!(matches!(
        fx.as_slice(),
        [
            ReaderEffect::Speak(_),
            ReaderEffect::SpeechCtl(SpeechCtl::Mute(true))
        ]
    ));
    assert_eq!(spoken(&fx), ["speech off"]);
    let fx = f.run(CoreCmd::SpeechModeCycle, 1);
    assert!(matches!(
        fx.as_slice(),
        [
            ReaderEffect::SpeechCtl(SpeechCtl::Mute(false)),
            ReaderEffect::Speak(_)
        ]
    ));
    assert_eq!(spoken(&fx), ["speech on"]);
}

#[test]
fn repeat_last_speaks_the_last_focus_speech_again_with_the_key_epoch() {
    let mut f = fixture();
    assert_eq!(
        spoken(&f.run(CoreCmd::SpeechRepeatLast, 1)),
        ["nothing to repeat"]
    );
    f.focus(1);
    f.reader.handle(ReaderMsg::Timer, f.now);
    let before = f.epoch.current();
    let fx = f.run(CoreCmd::SpeechRepeatLast, 1);
    assert_eq!(spoken(&fx), ["OK button"]);
    let ReaderEffect::Speak(s) = &fx[0] else {
        panic!("speech expected")
    };
    assert!(s.utterance.epoch > before, "fresh epoch of the key press");
    // Repeating does not change what "last" is, and key echo / confirmations never replace it.
    f.run(CoreCmd::SpeechRingInc, 1);
    assert_eq!(spoken(&f.run(CoreCmd::SpeechRepeatLast, 1)), ["OK button"]);
}

#[test]
fn time_and_date() {
    let mut f = fixture();
    assert_eq!(spoken(&f.run(CoreCmd::InfoTimeDate, 1)), ["14:05"]);
    assert_eq!(
        spoken(&f.run(CoreCmd::InfoTimeDate, 2)),
        ["Tuesday, October 13, 2026"]
    );
}

#[test]
fn where_am_i_uses_the_mirror_and_repeats_escalate() {
    let mut f = fixture();
    f.writer.apply(
        SnapshotBatch {
            nodes: vec![
                snap(1, Role::Application, "Files", None),
                snap(2, Role::Window, "Downloads", Some(1)),
                snap(3, Role::Button, "Save", Some(2)),
            ],
            ..Default::default()
        },
        gorca_mirror::ApplyOrigin::Push,
    );
    f.reader.handle(
        ReaderMsg::Focus(crate::FocusLaneMsg::FocusChanged { node: key(3) }),
        f.now,
    );
    f.reader.handle(ReaderMsg::Timer, f.now);
    let fx = f.run(CoreCmd::InfoWhereAmI, 1);
    assert_eq!(spoken(&fx), ["Files, Downloads window, Save button"]);
    assert!(
        fx.iter().any(|e| matches!(
            e,
            ReaderEffect::Demand {
                why: gorca_mirror::fill::DemandWhy::WhereAmI,
                outcome: gorca_mirror::fill::DemandOutcome::Hit,
                ..
            }
        )),
        "{fx:?}"
    );
    assert_eq!(spoken(&f.run(CoreCmd::InfoWhereAmI, 3)), ["S a v e"]);
    assert_eq!(spoken(&f.run(CoreCmd::InfoWindowTitle, 1)), ["Downloads"]);
    assert_eq!(
        spoken(&f.run(CoreCmd::InfoWindowTitle, 2)),
        ["D o w n l o a d s"]
    );
    let info = spoken(&f.run(CoreCmd::DebugNodeInfo, 1));
    assert!(
        info[0].contains("node 3") && info[0].contains("2 ancestors"),
        "{info:?}"
    );
}

#[test]
fn where_am_i_on_a_miss_fetches_once_and_answers_on_the_reply() {
    let mut f = fixture();
    // Focus a node the mirror does not know yet; the focus fetch is for an older epoch.
    f.reader.handle(
        ReaderMsg::Focus(crate::FocusLaneMsg::FocusChanged { node: key(5) }),
        f.now,
    );
    f.reader.handle(ReaderMsg::Timer, f.now);
    let fx = f.run(CoreCmd::InfoWhereAmI, 1);
    let epoch = f.epoch.current();
    assert!(
        matches!(
            fx.as_slice(),
            [
                ReaderEffect::Demand {
                    why: gorca_mirror::fill::DemandWhy::WhereAmI,
                    outcome: gorca_mirror::fill::DemandOutcome::Miss,
                    ..
                },
                ReaderEffect::Fetch {
                    class: gorca_rt::BudgetClass::Interactive,
                    ..
                }
            ]
        ),
        "{fx:?}"
    );
    let reply = QueryReply {
        node: key(5),
        epoch,
        kind: crate::reader::FetchKind::Focus,
        result: Ok(snap(5, Role::Link, "Docs", None)),
    };
    let fx = f.reader.handle(ReaderMsg::Reply(reply), f.now);
    assert_eq!(spoken(&fx), ["Docs link"]);
}

#[test]
fn a_late_reply_after_a_newer_key_is_dropped_and_a_timeout_speaks_the_partial_answer() {
    let mut f = fixture();
    f.reader.handle(
        ReaderMsg::Focus(crate::FocusLaneMsg::FocusChanged { node: key(5) }),
        f.now,
    );
    f.reader.handle(ReaderMsg::Timer, f.now);
    f.run(CoreCmd::InfoWhereAmI, 1);
    let asked = f.epoch.current();
    // The user presses another key before the answer arrives: the answer is stale (E-3).
    f.epoch.advance();
    let reply = QueryReply {
        node: key(5),
        epoch: asked,
        kind: crate::reader::FetchKind::Info,
        result: Ok(snap(5, Role::Link, "Docs", None)),
    };
    assert!(spoken(&f.reader.handle(ReaderMsg::Reply(reply), f.now)).is_empty());
    assert_eq!(f.reader.stale_replies_dropped(), 1);

    // A timed-out fetch says so instead of staying silent.
    f.run(CoreCmd::InfoWhereAmI, 1);
    let reply = QueryReply {
        node: key(5),
        epoch: f.epoch.current(),
        kind: crate::reader::FetchKind::Info,
        result: Err(gorca_connector_api::BackendError::Timeout),
    };
    let fx = f.reader.handle(ReaderMsg::Reply(reply), f.now);
    assert_eq!(spoken(&fx), ["application not responding"]);
}

#[test]
fn without_focus_the_node_commands_say_so() {
    let mut f = fixture();
    assert_eq!(spoken(&f.run(CoreCmd::InfoWhereAmI, 1)), ["no focus"]);
}

#[test]
fn learn_mode_toggles_the_grab_set_and_describes_keys() {
    let mut f = fixture();
    let fx = f.run(CoreCmd::InputLearnModeToggle, 1);
    assert_eq!(spoken(&fx), ["learn mode on, Escape to exit"]);
    assert!(matches!(
        fx.first(),
        Some(ReaderEffect::SetGrabSet(g)) if g.grab_all && g.ctx.mode == gorca_input_api::ModeKind::InputHelp
    ));
    assert_eq!(
        f.reader.mode(),
        Mode::InputHelp {
            resume: BaseMode::Focus
        }
    );
    // A described key is spoken, not executed.
    let mut i = intent(CoreCmd::InfoWindowTitle, 1, f.epoch.current().0 + 1);
    i.class = KeyClass::Described(Some(CoreCmd::InfoWindowTitle.id()));
    i.chord = Some(gorca_input_api::ChordKey {
        mods: Modifiers::GORCA,
        key: gorca_input_api::KeyRef::Code(28),
    });
    let fx = f.reader.handle(ReaderMsg::Key(i), f.now);
    assert_eq!(
        spoken(&fx),
        ["gorca T: report the window title; twice to spell it"]
    );
    i.class = KeyClass::Described(Some(CoreCmd::ReviewSayAll.id()));
    let fx = f.reader.handle(ReaderMsg::Key(i), f.now);
    assert_eq!(
        spoken(&fx),
        ["gorca T: read from the review cursor, not available yet"]
    );
    i.class = KeyClass::Described(None);
    assert_eq!(
        spoken(&f.reader.handle(ReaderMsg::Key(i), f.now)),
        ["gorca T: not bound"]
    );
    // Leaving works from the command and from a lane alert (LM-3).
    let fx = f.run(CoreCmd::InputLearnModeToggle, 1);
    assert_eq!(spoken(&fx), ["learn mode off"]);
    assert_eq!(f.reader.mode(), Mode::Focus);
    f.run(CoreCmd::InputLearnModeToggle, 1);
    f.control(ControlMsg::Mode(crate::ModeMsg::InputHelpExit));
    assert_eq!(f.reader.mode(), Mode::Focus);
}

#[test]
fn pass_next_key_empties_the_grab_set_and_ends_on_the_next_key_or_the_timeout() {
    let mut f = fixture();
    let fx = f.run(CoreCmd::InputPassNextKey, 1);
    assert_eq!(spoken(&fx), ["pass next key"]);
    assert!(matches!(
        fx.first(),
        Some(ReaderEffect::SetGrabSet(g)) if g.is_empty()
    ));
    assert!(fx.iter().any(|e| matches!(e, ReaderEffect::ArmTimer(_))));
    // Modifier keys do not end it; the first other key does and restores the grabs.
    let key_msg = |keysym: u32| {
        ReaderMsg::Key(KeyIntent {
            class: KeyClass::PassToApp,
            event: KeyEvent {
                keysym,
                unichar: 0,
                ..intent(CoreCmd::SpeechStop, 1, 0).event
            },
            ..intent(CoreCmd::SpeechStop, 1, 1)
        })
    };
    f.reader.handle(key_msg(0xffe1), f.now);
    assert!(matches!(f.reader.mode(), Mode::PassThrough { .. }));
    let fx = f.reader.handle(key_msg(0xffbf), f.now);
    assert_eq!(f.reader.mode(), Mode::Focus);
    assert!(fx.iter().any(
        |e| matches!(e, ReaderEffect::SetGrabSet(g) if !g.is_empty() || g.modifiers.is_empty())
    ));

    // Timeout: nobody typed anything for 10 s.
    f.run(CoreCmd::InputPassNextKey, 1);
    let fx = f.reader.handle(
        ReaderMsg::Timer,
        f.now + crate::PASS_TIMEOUT + Duration::from_millis(1),
    );
    assert_eq!(f.reader.mode(), Mode::Focus);
    assert_eq!(spoken(&fx), ["pass next key cancelled"]);
}

#[test]
fn settings_reload_save_and_their_results() {
    let mut f = fixture();
    assert!(matches!(
        f.run(CoreCmd::SettingsReload, 1).as_slice(),
        [ReaderEffect::Config(ConfigRequest::Reload)]
    ));
    f.run(CoreCmd::SpeechRateUp, 1);
    let fx = f.run(CoreCmd::SettingsSave, 1);
    let [ReaderEffect::Config(ConfigRequest::Save(o))] = fx.as_slice() else {
        panic!("{fx:?}")
    };
    assert_eq!(o.rate, Some(55));
    for (r, text) in [
        (SaveResult::Saved, "settings saved"),
        (
            SaveResult::FileChanged,
            "not saved: settings file changed on disk",
        ),
        (
            SaveResult::NewerSchema,
            "not saved: settings file is from a newer version",
        ),
        (SaveResult::Failed, "settings could not be saved"),
    ] {
        let fx = f.control(ControlMsg::Settings(SettingsMsg::SaveResult(r)));
        assert_eq!(spoken(&fx), [text]);
    }
}

#[test]
fn a_file_reload_keeps_unrelated_runtime_changes_and_an_explicit_one_reverts() {
    let mut f = fixture();
    f.control(ControlMsg::Settings(SettingsMsg::Loaded(loaded(
        "",
        LoadCause::Startup,
    ))));
    f.run(CoreCmd::SpeechRateUp, 1);
    f.run(CoreCmd::EchoTypedWordsToggle, 1);
    // The user edits the file: the rate changes there, the toggle is untouched (HR-4).
    let fx = f.control(ControlMsg::Settings(SettingsMsg::Loaded(loaded(
        "[speech]\nrate = 30\n",
        LoadCause::FileChanged,
    ))));
    assert_eq!(spoken(&fx), ["settings reloaded"]);
    assert_eq!(f.reader.settings().speech.rate, 30, "the file edit wins");
    assert!(f.reader.settings().echo.typed_words, "the toggle survives");
    assert!(ctls(&fx).contains(&SpeechCtl::SetParams(VoiceLevels {
        rate: 30,
        pitch: 50,
        volume: 100
    })));
    // Explicit reload: revert to the saved file.
    let fx = f.control(ControlMsg::Settings(SettingsMsg::Loaded(loaded(
        "[speech]\nrate = 30\n",
        LoadCause::Explicit,
    ))));
    assert_eq!(spoken(&fx), ["settings reloaded"]);
    assert!(!f.reader.settings().echo.typed_words);
    // Our own save: the overlay becomes part of the file.
    f.run(CoreCmd::SpeechRateUp, 1);
    f.control(ControlMsg::Settings(SettingsMsg::Loaded(loaded(
        "[speech]\nrate = 35\n",
        LoadCause::Saved,
    ))));
    assert_eq!(f.reader.settings().speech.rate, 35);
}

#[test]
fn startup_publishes_the_grab_set_and_speaks_problems_once_per_distinct_set() {
    let mut f = fixture();
    let bad = "[speech]\nrate = \"x\"\nbogus = 1\n";
    let fx = f.control(ControlMsg::Settings(SettingsMsg::Loaded(loaded(
        bad,
        LoadCause::Startup,
    ))));
    assert!(
        fx.iter()
            .any(|e| matches!(e, ReaderEffect::SetGrabSet(g) if !g.modifiers.is_empty()))
    );
    assert_eq!(spoken(&fx), ["Settings: 2 problems"]);
    let fx = f.control(ControlMsg::Settings(SettingsMsg::Loaded(loaded(
        bad,
        LoadCause::FileChanged,
    ))));
    assert_eq!(
        spoken(&fx),
        ["settings reloaded"],
        "the same set is not announced again"
    );
    let fx = f.run(CoreCmd::SettingsReportProblems, 1);
    let t = &spoken(&fx)[0];
    assert!(
        t.starts_with("2 settings problems. ") && t.contains("speech.rate"),
        "{t}"
    );
    let fx = f.control(ControlMsg::Settings(SettingsMsg::Loaded(loaded(
        "",
        LoadCause::FileChanged,
    ))));
    assert_eq!(spoken(&fx), ["settings reloaded"]);
    assert_eq!(
        spoken(&f.run(CoreCmd::SettingsReportProblems, 1)),
        ["no settings problems"]
    );
}

#[test]
fn caps_lock_drop_is_announced_once() {
    let mut f = fixture();
    let mut l = (*loaded("", LoadCause::Startup)).clone();
    l.caps_lock_dropped = true;
    let l = Arc::new(l);
    let fx = f.control(ControlMsg::Settings(SettingsMsg::Loaded(Arc::clone(&l))));
    assert!(
        spoken(&fx)
            .iter()
            .any(|t| t.contains("gorca key is Insert"))
    );
    let fx = f.control(ControlMsg::Settings(SettingsMsg::Loaded(l)));
    assert!(
        !spoken(&fx)
            .iter()
            .any(|t| t.contains("gorca key is Insert"))
    );
}

#[test]
fn input_notices_are_announced_once() {
    let mut f = fixture();
    let n = |f: &mut Fx, n| spoken(&f.control(ControlMsg::Input(n)));
    assert_eq!(n(&mut f, InputNotice::HeldByOtherScreenReader).len(), 1);
    assert!(n(&mut f, InputNotice::HeldByOtherScreenReader).is_empty());
    assert_eq!(
        n(&mut f, InputNotice::Lost),
        ["keyboard commands unavailable"]
    );
    assert!(
        n(&mut f, InputNotice::Lost).is_empty(),
        "retries stay quiet"
    );
    assert_eq!(
        n(&mut f, InputNotice::Restored),
        ["keyboard commands restored"]
    );
    assert!(n(&mut f, InputNotice::Restored).is_empty());
    assert_eq!(
        n(&mut f, InputNotice::Lost).len(),
        1,
        "a new loss is news again"
    );
}

#[test]
fn the_modifier_double_tap_is_described_in_learn_mode_only() {
    let mut f = fixture();
    let mut i = intent(CoreCmd::SpeechStop, 1, 1);
    i.class = KeyClass::ModifierPassThrough;
    assert!(spoken(&f.reader.handle(ReaderMsg::Key(i), f.now)).is_empty());
    f.run(CoreCmd::InputLearnModeToggle, 1);
    i.epoch = f.epoch.current();
    assert_eq!(
        spoken(&f.reader.handle(ReaderMsg::Key(i), f.now)),
        ["gorca key passed to application"]
    );
}

#[test]
fn announcements_survive_focus_changes_and_command_answers_do_not() {
    let mut f = fixture();
    let fx = f.control(ControlMsg::Input(InputNotice::HeldByOtherScreenReader));
    let ReaderEffect::Speak(s) = &fx[0] else {
        panic!("speech expected")
    };
    assert!(s.utterance.survives_focus_change);
    let fx = f.run(CoreCmd::InfoTimeDate, 1);
    let ReaderEffect::Speak(s) = &fx[0] else {
        panic!("speech expected")
    };
    assert!(!s.utterance.survives_focus_change);
    assert_eq!(s.utterance.prio, gorca_model::Prio::Interrupt);
}

#[test]
fn the_reader_acknowledges_keys_to_the_lane() {
    let mut f = fixture();
    let links = gorca_input_api::LaneLinks::new();
    f.reader = fixture().reader.with_links(Arc::clone(&links));
    f.run(CoreCmd::InfoTimeDate, 1);
    f.run(CoreCmd::InfoTimeDate, 1);
    assert_eq!(
        links.reader_ack.load(std::sync::atomic::Ordering::Acquire),
        2
    );
}

#[test]
fn an_ambiguous_chord_is_never_run_but_the_user_is_told_to_press_again() {
    let mut f = fixture();
    let mut i = intent(CoreCmd::SettingsSave, 1, 1);
    // The chord matches settings.save but Mutter's fate for the key is unknown.
    i.class = KeyClass::Ambiguous {
        committed: Some(CoreCmd::SettingsSave.id()),
    };
    let fx = f.reader.handle(ReaderMsg::Key(i), f.now);
    assert_eq!(spoken(&fx), ["again"]);
    assert!(
        fx.iter().all(|e| !matches!(e, ReaderEffect::Config(_))),
        "the command did not run"
    );
    assert_eq!(
        (f.reader.ambiguous_keys(), f.reader.ambiguous_cues()),
        (1, 1)
    );
    // No command behind it (a modifier, an unbound key) or a release: silent, still counted.
    i.class = KeyClass::Ambiguous { committed: None };
    assert!(f.reader.handle(ReaderMsg::Key(i), f.now).is_empty());
    i.class = KeyClass::Ambiguous {
        committed: Some(CoreCmd::SettingsSave.id()),
    };
    i.kind = PressKind::Release;
    assert!(f.reader.handle(ReaderMsg::Key(i), f.now).is_empty());
    assert_eq!(
        (f.reader.ambiguous_keys(), f.reader.ambiguous_cues()),
        (3, 1)
    );
}

#[test]
fn unknown_command_ids_are_ignored() {
    let mut f = fixture();
    let mut i = intent(CoreCmd::SpeechStop, 1, 1);
    i.class = KeyClass::Command(CmdId(u32::MAX));
    assert!(f.reader.handle(ReaderMsg::Key(i), f.now).is_empty());
}

#[test]
fn overlay_type_is_re_exported_for_saving() {
    assert!(Overlay::default().is_empty());
}
