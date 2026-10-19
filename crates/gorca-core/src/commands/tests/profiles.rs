// SPDX-License-Identifier: GPL-3.0-or-later
//! ADR-0016 slices P4 and P5 in the Reader: the profile key from the focus, the published
//! snapshot, edits that go to the focused application's profile, the chords and their commands
//! (section 6.1).

use std::collections::BTreeSet;

use gorca_config::profiles::{
    BuiltLayers, DiskView, ExtSchema, ProfileLoader, ProfileMode, SaveRefusal, build_layers,
};
use gorca_model::DocId;

use super::*;
use crate::browse::{Scope, WebMode};
use crate::mode::{ModeMsg, WebCursor};

const FIREFOX: &str = "[profile]\nname = \"Firefox\"\ntrigger = [{ app = { name = [\"firefox\"] } }]\n[speech]\nrate = 70\n";

fn layers(global: Option<&str>, files: &[(&str, &str)]) -> Arc<BuiltLayers> {
    let l = load(global, &Presets::builtin());
    let files: Vec<(String, String)> = files
        .iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect();
    let b = build_layers(
        &l.patch,
        l.hash,
        &files,
        &ExtSchema::new(),
        &mut ProfileLoader::new(),
    );
    assert!(b.diags.is_empty(), "{:?}", b.diags);
    Arc::new(b)
}

fn deliver(f: &mut Fx, global: Option<&str>, files: &[(&str, &str)], cause: LoadCause) {
    let mut l = (*loaded(global.unwrap_or(""), cause)).clone();
    l.profiles = Some(layers(global, files));
    f.control(ControlMsg::Settings(SettingsMsg::Loaded(Arc::new(l))));
}

/// A Reader with profiles loaded and application `app_name` focused.
fn pf(app_name: &str, global: Option<&str>, files: &[(&str, &str)]) -> Fx {
    let mut f = fixture();
    f.reader
        .dir
        .set_app_name(app(), Some(&Arc::<str>::from(app_name)));
    deliver(&mut f, global, files, LoadCause::Startup);
    f.focus(1);
    f
}

fn rename(f: &mut Fx, name: &str) -> ReaderEffects {
    f.reader
        .dir
        .set_app_name(app(), Some(&Arc::<str>::from(name)));
    f.reader.handle(ReaderMsg::Timer, f.now)
}

fn rate(f: &Fx) -> u8 {
    f.reader.settings().speech.rate
}

fn said(fx: &ReaderEffects) -> String {
    spoken(fx).join(" | ")
}

#[allow(clippy::wildcard_enum_match_arm)] // a test helper that looks for one effect kind
fn requests(fx: &ReaderEffects) -> Vec<&ConfigRequest> {
    fx.iter()
        .filter_map(|e| match e {
            ReaderEffect::Config(r) => Some(r),
            _ => None,
        })
        .collect()
}

#[test]
fn the_focused_applications_profile_applies_and_the_voice_is_set_before_speech() {
    let mut f = pf("Signal", None, &[("firefox", FIREFOX)]);
    assert_eq!(rate(&f), 50);
    let fx = rename(&mut f, "Firefox");
    assert_eq!(rate(&f), 70, "the App trigger matched");
    // SW-3: the new levels reach the lane before anything spoken in the same message.
    let first = fx
        .iter()
        .position(|e| matches!(e, ReaderEffect::SpeechCtl(SpeechCtl::SetParams(_))));
    assert!(first.is_some(), "{fx:?}");
    assert!(
        fx.iter()
            .position(|e| matches!(e, ReaderEffect::Speak(_)))
            .is_none_or(|s| first.unwrap_or(usize::MAX) < s)
    );
    let fx = rename(&mut f, "Signal");
    assert_eq!(rate(&f), 50);
    assert!(matches!(
        ctls(&fx).as_slice(),
        [SpeechCtl::SetParams(VoiceLevels { rate: 50, .. })]
    ));
    // Triggers are matched as the profile says: case-insensitively, exactly.
    rename(&mut f, "FIREFOX");
    assert_eq!(rate(&f), 70);
    rename(&mut f, "firefox-esr");
    assert_eq!(rate(&f), 50);
}

#[test]
fn the_snapshot_is_published_and_its_generation_bumps_on_every_change() {
    let mut f = pf("Firefox", None, &[("firefox", FIREFOX)]);
    let g0 = f.reader.profile_snapshot().unwrap().generation;
    f.run(CoreCmd::SpeechRateUp, 1);
    let g1 = f.reader.profile_snapshot().unwrap().generation;
    assert!(g1 > g0, "an edit publishes a new snapshot");
    f.run(CoreCmd::ProfilesSuspend, 1);
    let s = f.reader.profile_snapshot().unwrap();
    assert!(s.generation > g1);
    assert_eq!(s.mode, ProfileMode::Suspended);
    f.run(CoreCmd::ProfilesCycle, 1);
    assert!(f.reader.profile_snapshot().unwrap().generation > s.generation);
    f.run(CoreCmd::ProfilesEditTargetCycle, 1);
    let c = f.reader.profile_counters().unwrap();
    assert!(c.switches > 0);
}

#[test]
fn a_rate_change_goes_to_the_focused_applications_profile_and_leaves_other_apps_alone() {
    let mut f = pf("Firefox", None, &[]);
    let fx = f.run(CoreCmd::SpeechRateUp, 1);
    // PE-1: the first edit creates the profile, announced once, and the target is named.
    assert_eq!(said(&fx), "rate 55, new profile for Firefox. for Firefox");
    assert_eq!(rate(&f), 55);
    let snap = f.reader.profile_snapshot().unwrap();
    assert_eq!(snap.deltas.created.len(), 1);
    assert!(
        snap.deltas.base.is_empty(),
        "the global profile is untouched"
    );
    // Again: the same target within the burst stays short.
    let fx = f.run(CoreCmd::SpeechRateUp, 1);
    assert_eq!(said(&fx), "rate 60");
    // Signal keeps the global rate.
    rename(&mut f, "Signal");
    assert_eq!(rate(&f), 50);
    rename(&mut f, "Firefox");
    assert_eq!(
        rate(&f),
        60,
        "the unsaved profile edit is back with the focus"
    );
}

#[test]
fn person_level_keys_always_go_to_the_global_profile() {
    let mut f = pf("Firefox", None, &[("firefox", FIREFOX)]);
    // `echo.*` is a privacy key: global even though an application profile exists.
    let fx = f.run(CoreCmd::EchoTypedWordsToggle, 1);
    assert_eq!(said(&fx), "speak typed words on, globally");
    let snap = f.reader.profile_snapshot().unwrap();
    assert!(!snap.deltas.base.is_empty());
    assert!(snap.deltas.profiles.is_empty());
    rename(&mut f, "Signal");
    assert!(f.reader.settings().echo.typed_words, "applies everywhere");
}

#[test]
fn the_edit_target_cycle_sends_edits_to_global_and_says_so() {
    let mut f = pf("Firefox", None, &[("firefox", FIREFOX)]);
    let fx = f.run(CoreCmd::ProfilesEditTargetCycle, 1);
    assert_eq!(
        said(&fx),
        "to the global settings",
        "no site outside a document"
    );
    let fx = f.run(CoreCmd::SpeechRateUp, 1);
    assert_eq!(said(&fx), "rate 75, globally; Firefox keeps its own value");
    // The profile still wins for Firefox; the global value changed for the others.
    assert_eq!(rate(&f), 70);
    rename(&mut f, "Signal");
    assert_eq!(rate(&f), 75);
    // Round: global -> application.
    rename(&mut f, "Firefox");
    let fx = f.run(CoreCmd::ProfilesEditTargetCycle, 1);
    assert_eq!(said(&fx), "to the Firefox profile");
}

#[test]
fn a_document_gets_a_site_target_and_a_site_profile() {
    // Put the Reader in a document of host web.example.
    let docs = Arc::new(crate::DocDirectory::new());
    let join = Arc::new(crate::WebJoin::new());
    let mut g = fixture();
    g.reader = g.reader.with_web(docs, join);
    g.reader
        .dir
        .set_app_name(app(), Some(&Arc::<str>::from("Firefox")));
    deliver(&mut g, None, &[], LoadCause::Startup);
    g.focus(1);
    g.reader.web.as_mut().unwrap().mode = WebMode::Browse {
        doc: DocId(1),
        cur: WebCursor {
            node: gorca_model::DocNodeId::from_parts(0, 0),
            local: 0,
        },
        scope: Scope::Document,
        sticky: false,
    };
    g.reader.set_document_host(Some(Arc::from("web.example")));
    g.reader.handle(ReaderMsg::Timer, g.now);
    let fx = g.run(CoreCmd::ProfilesEditTargetCycle, 1);
    assert_eq!(said(&fx), "to the profile of web.example");
    let fx = g.run(CoreCmd::SpeechRateUp, 1);
    assert!(said(&fx).contains("for web.example"), "{}", said(&fx));
    assert_eq!(rate(&g), 55);
    // Another host of the same application does not get the site's rate.
    g.reader.set_document_host(Some(Arc::from("other.example")));
    g.reader.handle(ReaderMsg::Timer, g.now);
    assert_eq!(rate(&g), 50);
    g.reader.set_document_host(Some(Arc::from("web.example")));
    g.reader.handle(ReaderMsg::Timer, g.now);
    assert_eq!(rate(&g), 55);
}

#[test]
fn context_changes_use_prewarmed_keys_and_never_fold_on_the_key_path() {
    let focus_mode = "[profile]\nname = \"Forms\"\ntrigger = [{ context = \"focus-mode\" }]\n[verbosity]\nlevel = \"verbose\"\n";
    let mut f = fixture();
    f.reader = f.reader.with_web(
        Arc::new(crate::DocDirectory::new()),
        Arc::new(crate::WebJoin::new()),
    );
    f.reader
        .dir
        .set_app_name(app(), Some(&Arc::<str>::from("Firefox")));
    deliver(&mut f, None, &[("forms", focus_mode)], LoadCause::Startup);
    f.reader.web.as_mut().unwrap().mode = WebMode::Browse {
        doc: DocId(1),
        cur: WebCursor {
            node: gorca_model::DocNodeId::from_parts(0, 0),
            local: 0,
        },
        scope: Scope::Document,
        sticky: false,
    };
    let fx = f.reader.handle(
        ReaderMsg::Focus(crate::FocusLaneMsg::FocusChanged { node: key(1) }),
        f.now,
    );
    // The Reader asked the config task to fold the keys one step away.
    let reqs = requests(&fx);
    let Some(ConfigRequest::Prewarm(p)) = reqs.first().copied() else {
        panic!("{fx:?}")
    };
    let entries = crate::prewarm(p);
    assert_eq!(
        entries.entries.len(),
        2,
        "two distinct keys: no profile, and the focus-mode profile"
    );
    f.control(ControlMsg::Settings(SettingsMsg::Prewarm(Arc::new(
        entries,
    ))));
    let before = f.reader.profile_counters().unwrap();
    assert_eq!(before.prewarmed, 2);
    assert_eq!(
        f.reader.settings().verbosity.level,
        gorca_config::Verbosity::Normal
    );
    // gorca+Space: browse mode becomes focus mode; the key is a cache hit.
    f.reader.web.as_mut().unwrap().mode = WebMode::Focus {
        doc: DocId(1),
        node: gorca_model::DocNodeId::from_parts(0, 0),
        cause: crate::FocusCause::Manual,
        resume: WebCursor {
            node: gorca_model::DocNodeId::from_parts(0, 0),
            local: 0,
        },
        scope: Scope::Document,
    };
    f.reader.handle(ReaderMsg::Timer, f.now);
    let after = f.reader.profile_counters().unwrap();
    assert_eq!(
        f.reader.settings().verbosity.level,
        gorca_config::Verbosity::Verbose,
        "the FocusMode profile applies"
    );
    assert_eq!(after.folds, before.folds, "no fold on the key path");
    assert!(after.hits > before.hits);
}

#[test]
fn prewarm_entries_of_an_older_generation_are_dropped() {
    let mut f = pf("Firefox", None, &[("firefox", FIREFOX)]);
    let fx = f.run(CoreCmd::SpeechRateUp, 1);
    let _ = fx;
    let snap = f.reader.profile_snapshot().unwrap();
    let stale = crate::PrewarmEntries {
        generation: snap.generation - 1,
        entries: crate::prewarm(&crate::PrewarmRequest {
            snapshot: Arc::clone(&snap),
            focus: crate::ProfileFocus::default(),
        })
        .entries,
    };
    let before = f.reader.profile_counters().unwrap();
    f.control(ControlMsg::Settings(SettingsMsg::Prewarm(Arc::new(stale))));
    let after = f.reader.profile_counters().unwrap();
    assert_eq!(after.prewarmed, before.prewarmed);
    assert!(after.prewarm_dropped > before.prewarm_dropped);
}

#[test]
fn profiles_cycle_pins_automatic_profile_global_only() {
    let mut f = pf("Firefox", None, &[("firefox", FIREFOX)]);
    assert_eq!(rate(&f), 70);
    let fx = f.run(CoreCmd::ProfilesCycle, 1);
    assert_eq!(said(&fx), "profile Firefox for Firefox");
    assert_eq!(rate(&f), 70);
    let fx = f.run(CoreCmd::ProfilesCycle, 1);
    assert_eq!(said(&fx), "profiles for Firefox: global settings only");
    assert_eq!(rate(&f), 50);
    let fx = f.run(CoreCmd::ProfilesCycle, 1);
    assert_eq!(said(&fx), "profiles for Firefox: automatic");
    assert_eq!(rate(&f), 70);
}

#[test]
fn toggling_the_current_profile_switches_it_off_and_on() {
    let mut f = pf("Firefox", None, &[("firefox", FIREFOX)]);
    let fx = f.run(CoreCmd::ProfilesToggleEnabled, 1);
    assert_eq!(said(&fx), "profile Firefox off");
    assert_eq!(rate(&f), 50);
    let fx = f.run(CoreCmd::ProfilesToggleEnabled, 1);
    assert_eq!(said(&fx), "profile Firefox on");
    assert_eq!(rate(&f), 70);
    let mut g = pf("Signal", None, &[("firefox", FIREFOX)]);
    assert_eq!(
        said(&g.run(CoreCmd::ProfilesToggleEnabled, 1)),
        "no profile for Signal"
    );
}

#[test]
fn profiles_suspend_folds_only_the_surviving_layers_and_says_so() {
    let mut f = pf("Code", None, &[("firefox", FIREFOX)]);
    rename(&mut f, "Firefox");
    assert_eq!(rate(&f), 70);
    let fx = f.run(CoreCmd::ProfilesSuspend, 1);
    assert!(said(&fx).contains("profiles off"), "{}", said(&fx));
    assert_eq!(rate(&f), 50);
    let fx = f.run(CoreCmd::ProfilesSuspend, 1);
    assert!(said(&fx).contains("profiles on"));
    assert_eq!(rate(&f), 70);
}

#[test]
fn learn_mode_holds_the_triggers_and_evaluates_the_latest_focus_once_on_leaving() {
    let mut f = pf("Signal", None, &[("firefox", FIREFOX)]);
    f.run(CoreCmd::InputLearnModeToggle, 1);
    assert!(matches!(f.reader.mode(), Mode::InputHelp { .. }));
    rename(&mut f, "Firefox");
    assert_eq!(rate(&f), 50, "TR-4: not re-evaluated while in learn mode");
    f.reader
        .handle(ReaderMsg::Mode(ModeMsg::InputHelpExit), f.now);
    assert_eq!(rate(&f), 70, "evaluated once on leaving");
}

#[test]
fn announce_says_the_profile_and_where_changes_go_and_twice_the_stack() {
    let mut f = pf("Firefox", None, &[("firefox", FIREFOX)]);
    assert_eq!(
        said(&f.run(CoreCmd::ProfilesAnnounce, 1)),
        "Firefox profile, changes go to the Firefox profile"
    );
    assert_eq!(
        said(&f.run(CoreCmd::ProfilesAnnounce, 2)),
        "profiles in effect: Firefox"
    );
    let mut g = pf("Signal", None, &[("firefox", FIREFOX)]);
    assert_eq!(
        said(&g.run(CoreCmd::ProfilesAnnounce, 1)),
        "no profile for Signal, changes go to the Signal profile"
    );
    // A product row shows in the stack.
    let mut c = pf("Code", None, &[]);
    assert_eq!(
        said(&c.run(CoreCmd::ProfilesAnnounce, 2)),
        "profiles in effect: Visual Studio Code"
    );
}

#[test]
fn settings_save_names_where_it_saved_and_completes_the_snapshot() {
    let mut f = pf("Firefox", None, &[]);
    f.run(CoreCmd::SpeechRateUp, 1);
    f.run(CoreCmd::EchoTypedWordsToggle, 1);
    let fx = f.run(CoreCmd::SettingsSave, 1);
    let reqs = requests(&fx);
    let Some(ConfigRequest::SaveProfiles(req)) = reqs.first().copied() else {
        panic!("{fx:?}")
    };
    assert!(matches!(req.kind, SaveKind::All));
    // Edits are refused while the save is in flight.
    assert_eq!(
        said(&f.run(CoreCmd::SpeechRateUp, 1)),
        "saving, try again in a moment"
    );
    // The config task plans against what is on disk and reports how far it got.
    let plan = req.snapshot.plan_save(&DiskView::default()).unwrap();
    let total = plan.steps.len();
    assert_eq!(total, 2, "the global file and the new Firefox profile");
    let fx = f.control(ControlMsg::Settings(SettingsMsg::ProfilesSaved(Arc::new(
        ProfilesSaved {
            plan: Some(Arc::new(plan)),
            outcome: ProfileSaveOutcome::Written { done: total, total },
            kind: SaveKind::All,
        },
    ))));
    assert_eq!(said(&fx), "saved for Firefox, global settings");
    let snap = f.reader.profile_snapshot().unwrap();
    assert!(snap.deltas.is_empty(), "nothing is dirty after the save");
    // Nothing left to save.
    assert_eq!(said(&f.run(CoreCmd::SettingsSave, 1)), "nothing to save");
}

#[test]
fn a_refused_save_says_why_and_keeps_the_edits() {
    let mut f = pf("Firefox", None, &[]);
    f.run(CoreCmd::SpeechRateUp, 1);
    let fx = f.run(CoreCmd::SettingsSave, 1);
    let reqs = requests(&fx);
    let Some(ConfigRequest::SaveProfiles(req)) = reqs.first().copied() else {
        panic!("{fx:?}")
    };
    let snapshot = Arc::clone(&req.snapshot);
    let _ = snapshot;
    let fx = f.control(ControlMsg::Settings(SettingsMsg::ProfilesSaved(Arc::new(
        ProfilesSaved {
            plan: None,
            outcome: ProfileSaveOutcome::Refused(SaveRefusal::ChangedOnDisk(
                gorca_config::profiles::FileId::Global,
            )),
            kind: SaveKind::All,
        },
    ))));
    assert_eq!(said(&fx), "not saved: settings file changed on disk");
    assert!(!f.reader.profile_snapshot().unwrap().deltas.is_empty());
    // And the edit is allowed again.
    assert!(said(&f.run(CoreCmd::SpeechRateUp, 1)).starts_with("rate 60"));
}

#[test]
fn a_save_nobody_answers_stops_blocking_edits_after_a_while() {
    let mut f = pf("Firefox", None, &[]);
    f.run(CoreCmd::SpeechRateUp, 1);
    f.run(CoreCmd::SettingsSave, 1);
    assert_eq!(
        said(&f.run(CoreCmd::SpeechRateUp, 1)),
        "saving, try again in a moment"
    );
    f.now += Duration::from_secs(11);
    assert!(said(&f.run(CoreCmd::SpeechRateUp, 1)).starts_with("rate 60"));
}

#[test]
fn save_global_takes_the_profile_of_the_focused_application_not_another_ones() {
    let signal = "[profile]\nname = \"Signal\"\ntrigger = [{ app = { name = [\"signal\"] } }]\n";
    let mut f = pf("Signal", None, &[("signal", signal)]);
    f.run(CoreCmd::SpeechRateUp, 1);
    // Firefox has no profile and nothing touched: the command must not move Signal's keys.
    rename(&mut f, "Firefox");
    let fx = f.run(CoreCmd::ProfilesSaveGlobal, 1);
    assert_eq!(said(&fx), "nothing changed for Firefox in this session");
    rename(&mut f, "Signal");
    let fx = f.run(CoreCmd::ProfilesSaveGlobal, 1);
    let reqs = requests(&fx);
    let Some(ConfigRequest::SaveProfiles(req)) = reqs.first().copied() else {
        panic!("{fx:?}")
    };
    let SaveKind::Global { profile_id, .. } = &req.kind else {
        panic!("{:?}", req.kind)
    };
    assert_eq!(profile_id, "signal");
}

#[test]
fn save_global_moves_only_the_touched_keys_and_twice_applies_everywhere() {
    let signal = "[profile]\nname = \"Signal\"\ntrigger = [{ app = { name = [\"signal\"] } }]\n[verbosity]\nlevel = \"brief\"\n[speech]\nrate = 30\n";
    let mut f = pf("Firefox", None, &[("firefox", FIREFOX), ("signal", signal)]);
    f.run(CoreCmd::SpeechRateUp, 1);
    let fx = f.run(CoreCmd::ProfilesSaveGlobal, 1);
    let reqs = requests(&fx);
    let Some(ConfigRequest::SaveProfiles(req)) = reqs.first().copied() else {
        panic!("{fx:?}")
    };
    let SaveKind::Global { profile_id, focus } = &req.kind else {
        panic!("{:?}", req.kind)
    };
    assert_eq!(profile_id, "firefox");
    // The config task reads the files and plans.
    let disk = DiskView {
        global: None,
        profiles: [
            ("firefox".to_owned(), FIREFOX.to_owned()),
            ("signal".to_owned(), signal.to_owned()),
        ]
        .into_iter()
        .collect(),
    };
    let plan = req
        .snapshot
        .plan_save_global(&focus.facts(), profile_id, false, &disk)
        .unwrap();
    let total = plan.steps.len();
    let kind = req.kind.clone();
    let fx = f.control(ControlMsg::Settings(SettingsMsg::ProfilesSaved(Arc::new(
        ProfilesSaved {
            plan: Some(Arc::new(plan)),
            outcome: ProfileSaveOutcome::Written { done: total, total },
            kind,
        },
    ))));
    assert_eq!(said(&fx), "1 setting saved globally, removed from Firefox");
    // Second press within the window: the same keys leave the other profiles.
    let fx = f.run(CoreCmd::ProfilesSaveGlobal, 2);
    let reqs = requests(&fx);
    let Some(ConfigRequest::SaveProfiles(req)) = reqs.first().copied() else {
        panic!("{fx:?}")
    };
    let SaveKind::Everywhere { keys, except } = &req.kind else {
        panic!("{:?}", req.kind)
    };
    assert_eq!(except, "firefox");
    assert_eq!(keys.len(), 1);
    let disk = DiskView {
        global: Some(String::new()),
        profiles: [("signal".to_owned(), signal.to_owned())]
            .into_iter()
            .collect(),
    };
    let plan = req
        .snapshot
        .plan_apply_everywhere(keys, except, &disk)
        .unwrap();
    assert_eq!(plan.report.other_profiles_changed, ["signal"]);
    let kind = req.kind.clone();
    let total = plan.steps.len();
    let fx = f.control(ControlMsg::Settings(SettingsMsg::ProfilesSaved(Arc::new(
        ProfilesSaved {
            plan: Some(Arc::new(plan)),
            outcome: ProfileSaveOutcome::Written { done: total, total },
            kind,
        },
    ))));
    assert_eq!(said(&fx), "applies to all applications; 1 profile changed");
    // Nothing touched: nothing to move.
    let fx = f.run(CoreCmd::ProfilesSaveGlobal, 1);
    let reqs = requests(&fx);
    let Some(ConfigRequest::SaveProfiles(req)) = reqs.first().copied() else {
        panic!("{fx:?}")
    };
    let plan = req.snapshot.plan_save_global(
        &crate::ProfileFocus::default().facts(),
        "firefox",
        false,
        &DiskView::default(),
    );
    assert!(matches!(plan, Err(SaveRefusal::NothingChanged(_))));
}

#[test]
fn save_mode_default_writes_the_documents_mode_as_a_profile_value() {
    let mut f = fixture();
    f.reader = f.reader.with_web(
        Arc::new(crate::DocDirectory::new()),
        Arc::new(crate::WebJoin::new()),
    );
    f.reader
        .dir
        .set_app_name(app(), Some(&Arc::<str>::from("Firefox")));
    deliver(&mut f, None, &[], LoadCause::Startup);
    f.focus(1);
    assert_eq!(
        said(&f.run(CoreCmd::BrowseSaveModeDefault, 1)),
        "no web page is focused"
    );
    f.reader.web.as_mut().unwrap().mode = WebMode::Focus {
        doc: DocId(1),
        node: gorca_model::DocNodeId::from_parts(0, 0),
        cause: crate::FocusCause::Manual,
        resume: WebCursor {
            node: gorca_model::DocNodeId::from_parts(0, 0),
            local: 0,
        },
        scope: Scope::Document,
    };
    let fx = f.run(CoreCmd::BrowseSaveModeDefault, 1);
    let text = said(&fx);
    assert!(
        text.starts_with("focus mode saved as the default"),
        "{text}"
    );
    assert!(text.contains("for Firefox"), "{text}");
    assert_eq!(
        f.reader.settings().browse.mode_default,
        gorca_config::ModeDefault::Focus
    );
}

#[test]
fn a_reload_keeps_unsaved_edits_and_a_new_file_changes_the_profile() {
    let mut f = pf("Firefox", None, &[("firefox", FIREFOX)]);
    f.run(CoreCmd::SpeechRateUp, 1);
    assert_eq!(rate(&f), 75);
    // The profile file is edited on disk: its rate changes, so the unsaved edit of that key is
    // dropped (HR-4 per layer).
    let edited = FIREFOX.replace("rate = 70", "rate = 40");
    deliver(
        &mut f,
        None,
        &[("firefox", &edited)],
        LoadCause::FileChanged,
    );
    assert_eq!(rate(&f), 40);
    // An unrelated reload keeps an unsaved edit.
    f.run(CoreCmd::SpeechRateUp, 1);
    deliver(
        &mut f,
        None,
        &[("firefox", &edited)],
        LoadCause::FileChanged,
    );
    assert_eq!(rate(&f), 45);
    // An explicit reload reverts everything unsaved.
    deliver(&mut f, None, &[("firefox", &edited)], LoadCause::Explicit);
    assert_eq!(rate(&f), 40);
}

#[test]
fn the_product_row_turns_off_browse_for_vs_code_and_applies_in_every_mode() {
    let mut f = pf("Code", None, &[]);
    assert!(!f.reader.settings().browse.enabled);
    f.run(CoreCmd::ProfilesSuspend, 1);
    assert!(
        !f.reader.settings().browse.enabled,
        "product rows apply under suspend"
    );
    let g = pf("Firefox", None, &[]);
    assert!(g.reader.settings().browse.enabled);
}

#[test]
fn profile_chords_are_bound_once_per_preset_and_conflict_free() {
    let want = [
        ("gorca+ctrl+shift+c", "profiles.save_global"),
        ("gorca+ctrl+p", "profiles.cycle"),
        ("gorca+ctrl+shift+p", "profiles.toggle_enabled"),
        ("gorca+ctrl+alt+p", "profiles.edit_target_cycle"),
        ("gorca+shift+p", "profiles.announce"),
        ("gorca+ctrl+shift+space", "browse.save_mode_default"),
        ("gorca+ctrl+c", "settings.save"),
    ];
    for preset in ["nvda", "orca"] {
        for layout in ["desktop", "laptop"] {
            let text = format!("[keyboard]\npreset=\"{preset}\"\nlayout=\"{layout}\"\n");
            let l = load(Some(&text), &Presets::builtin());
            assert!(l.diags.is_empty(), "{preset} {layout}: {:?}", l.diags);
            let bound: Vec<(String, &str)> = l
                .keymap
                .bound()
                .map(|(_, k, c)| (k.to_string(), c))
                .collect();
            let mut chords = BTreeSet::new();
            for (chord, cmd) in want {
                let hits: Vec<_> = bound
                    .iter()
                    .filter(|(k, c)| k == chord && *c == cmd)
                    .collect();
                assert_eq!(hits.len(), 1, "{preset} {layout}: {chord} -> {cmd}");
                assert!(chords.insert(chord));
                // No other command shares the chord.
                assert_eq!(
                    bound.iter().filter(|(k, _)| k == chord).count(),
                    1,
                    "{preset} {layout}: {chord}"
                );
            }
            // `profiles.suspend` has no chord (command list only).
            assert!(!bound.iter().any(|(_, c)| *c == "profiles.suspend"));
        }
    }
}
