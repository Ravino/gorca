#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_doc::web::{DocWriter, channel};
use gorca_doc_source_api::web_attrs;
use gorca_input_api::{Chord, KeyClass, KeyEvent, KeyIntent, Modifiers, PressKind};
use gorca_mirror::{ApplyOrigin, MirrorWriter, mirror};
use gorca_model::{
    AppId, ChildList, ConnectorId, DocDelta, DocId, DocNodeId, Epoch, Freshness, Ifaces, LocalId,
    NodeKey, NodeSnapshot, Role, SnapshotBatch, SrcNode, SrcNodeId, StateSet,
};
use gorca_rt::{EpochCell, mpsc};

use super::*;
use crate::WebEffect;
use crate::commands::CoreCmd;
use crate::directory::MirrorDirectory;
use crate::doc_actor::translate::Translator;
use crate::doc_actor::{DocCtl, DocHandle, DocNotice, SelOutcome};
use crate::mode::{Mode, ModeKeys};
use crate::reader::{Reader, ReaderEffect, ReaderEffects, ReaderMsg};
use gorca_model::DocAction;

fn app() -> AppId {
    AppId {
        connector: ConnectorId(1),
        serial: 1,
    }
}

fn nk(n: u64) -> NodeKey {
    NodeKey {
        app: app(),
        local: LocalId(n),
    }
}

fn nid(n: u32) -> DocNodeId {
    DocNodeId::from_parts(n, 0)
}

fn cur(n: u32) -> WebCursor {
    WebCursor {
        node: nid(n),
        local: 0,
    }
}

// ---------------------------------------------------------------------------------------------
// The pure state machine

fn browse(doc: u32, n: u32) -> WebMode {
    WebMode::Browse {
        doc: DocId(doc),
        cur: cur(n),
        scope: Scope::Document,
        sticky: false,
    }
}

fn focus(doc: u32, n: u32, cause: FocusCause) -> WebMode {
    WebMode::Focus {
        doc: DocId(doc),
        node: nid(n),
        cause,
        resume: cur(n),
        scope: Scope::Document,
    }
}

fn focus_in(doc: u32, n: u32, passes: bool) -> WebEv {
    WebEv::FocusIn {
        doc: DocId(doc),
        node: nid(n),
        passes,
        application: false,
    }
}

#[test]
fn entering_a_document_announces_the_mode_and_placing_follows_the_focus() {
    let (m, fx) = step(WebMode::Inactive, focus_in(1, 5, false));
    assert_eq!(m, browse(1, 5));
    assert_eq!(fx.as_slice(), [WebFx::Announce(ModeWord::Browse)]);
    // A field switches to focus mode on entry.
    let (m, fx) = step(WebMode::Inactive, focus_in(1, 6, true));
    assert_eq!(m, focus(1, 6, FocusCause::Auto));
    assert_eq!(fx.as_slice(), [WebFx::Announce(ModeWord::Focus)]);
}

#[test]
fn tab_between_a_link_and_a_field_switches_modes_and_only_then_speaks_the_word() {
    let (m, fx) = step(browse(1, 5), focus_in(1, 6, false));
    assert_eq!(m, browse(1, 6));
    assert!(fx.is_empty(), "same kind: no word");
    let (m, fx) = step(m, focus_in(1, 7, true));
    assert_eq!(m, focus(1, 7, FocusCause::Auto));
    assert_eq!(fx.as_slice(), [WebFx::Announce(ModeWord::Focus)]);
    // Tab on to a link: back to browse mode.
    let (m, fx) = step(m, focus_in(1, 8, false));
    assert_eq!(m, browse(1, 8));
    assert_eq!(fx.as_slice(), [WebFx::Announce(ModeWord::Browse)]);
}

#[test]
fn escape_leaves_auto_focus_mode_only() {
    let m = focus(1, 7, FocusCause::Auto);
    assert!(m.escape_grabbed());
    let (m, fx) = step(m, WebEv::Escape);
    assert_eq!(m, browse(1, 7));
    assert_eq!(fx.as_slice(), [WebFx::Announce(ModeWord::Browse)]);
    assert!(!m.escape_grabbed());
    let (m2, fx) = step(m, WebEv::Escape);
    assert_eq!(m2, m, "browse mode: nothing to leave");
    assert!(fx.is_empty());
    for cause in [FocusCause::Manual, FocusCause::Application] {
        let f = focus(1, 7, cause);
        assert!(!f.escape_grabbed());
        assert_eq!(
            step(f, WebEv::Escape).0,
            f,
            "Escape is not ours in {cause:?}"
        );
    }
}

#[test]
fn the_toggle_is_manual_focus_mode_and_sticky_until_toggled_again() {
    let (m, fx) = step(
        browse(1, 5),
        WebEv::Toggle {
            focusable: Some(nid(4)),
            root: nid(0),
        },
    );
    assert_eq!(
        m,
        WebMode::Focus {
            doc: DocId(1),
            node: nid(4),
            cause: FocusCause::Manual,
            resume: cur(5),
            scope: Scope::Document
        }
    );
    assert_eq!(
        fx.as_slice(),
        [
            WebFx::Announce(ModeWord::Focus),
            WebFx::FocusNode(DocId(1), nid(4))
        ]
    );
    // Focus moving inside the document keeps manual focus mode.
    let (m, fx) = step(m, focus_in(1, 9, false));
    assert!(matches!(
        m,
        WebMode::Focus {
            cause: FocusCause::Manual,
            ..
        }
    ));
    assert!(fx.is_empty());
    let (m, fx) = step(
        m,
        WebEv::Toggle {
            focusable: None,
            root: nid(0),
        },
    );
    assert!(matches!(m, WebMode::Browse { .. }));
    assert_eq!(fx.as_slice(), [WebFx::Announce(ModeWord::Browse)]);
}

#[test]
fn role_application_is_left_only_by_the_toggle() {
    let (m, _) = step(
        browse(1, 1),
        WebEv::FocusIn {
            doc: DocId(1),
            node: nid(2),
            passes: false,
            application: true,
        },
    );
    assert!(matches!(
        m,
        WebMode::Focus {
            cause: FocusCause::Application,
            ..
        }
    ));
    assert_eq!(step(m, WebEv::Escape).0, m);
    let (still, _) = step(m, focus_in(1, 3, false));
    assert!(matches!(
        still,
        WebMode::Focus {
            cause: FocusCause::Application,
            ..
        }
    ));
}

#[test]
fn activation_on_a_field_enters_focus_mode_and_focuses_it() {
    let (m, fx) = step(
        browse(1, 5),
        WebEv::Activate {
            node: nid(7),
            passes: true,
        },
    );
    assert_eq!(m, focus(1, 7, FocusCause::Auto).with_resume(cur(5)));
    assert!(fx.contains(&WebFx::FocusNode(DocId(1), nid(7))));
    let (same, fx) = step(
        browse(1, 5),
        WebEv::Activate {
            node: nid(7),
            passes: false,
        },
    );
    assert_eq!(same, browse(1, 5));
    assert!(fx.is_empty());
}

trait WithResume {
    fn with_resume(self, r: WebCursor) -> Self;
}

impl WithResume for WebMode {
    fn with_resume(self, r: WebCursor) -> Self {
        match self {
            WebMode::Focus {
                doc,
                node,
                cause,
                scope,
                ..
            } => WebMode::Focus {
                doc,
                node,
                cause,
                resume: r,
                scope,
            },
            other @ (WebMode::Inactive | WebMode::Loading { .. } | WebMode::Browse { .. }) => other,
        }
    }
}

#[test]
fn loading_waits_for_the_document_and_resolves_to_browse_or_focus() {
    let now = Instant::now();
    let pending = WebEv::FocusPending {
        focus: nk(9),
        editable: false,
        known: true,
        now,
    };
    let (m, fx) = step(WebMode::Inactive, pending);
    assert!(matches!(m, WebMode::Loading { .. }));
    assert_eq!(fx.as_slice(), [WebFx::Ensure(nk(9))]);
    // The same focus again does not ask twice.
    let (m2, fx) = step(m, pending);
    assert_eq!(m2, m);
    assert!(fx.is_empty());
    // The page arrives without a focus node: browse from the top.
    let (b, fx) = step(
        m,
        WebEv::FirstUseful {
            doc: DocId(2),
            focus: None,
            top: nid(0),
        },
    );
    assert_eq!(b, browse(2, 0));
    assert_eq!(fx.as_slice(), [WebFx::Announce(ModeWord::Browse)]);
    // The page arrives with an editable focus: focus mode.
    let (f, _) = step(
        m,
        WebEv::FirstUseful {
            doc: DocId(2),
            focus: Some((nid(3), true)),
            top: nid(0),
        },
    );
    assert_eq!(f, focus(2, 3, FocusCause::Auto));
    // No document: back to inactive, but only for the node that asked.
    assert_eq!(
        step(m, WebEv::NoDocument { focus: nk(9) }).0,
        WebMode::Inactive
    );
    assert_eq!(step(m, WebEv::NoDocument { focus: nk(1) }).0, m);
}

#[test]
fn an_editable_loading_focus_is_already_focus_mode_and_says_so_once() {
    let now = Instant::now();
    let (m, fx) = step(
        browse(1, 5),
        WebEv::FocusPending {
            focus: nk(9),
            editable: true,
            known: true,
            now,
        },
    );
    assert_eq!(
        fx.as_slice(),
        [WebFx::Announce(ModeWord::Focus), WebFx::Ensure(nk(9))]
    );
    let (f, fx) = step(
        m,
        WebEv::FirstUseful {
            doc: DocId(1),
            focus: Some((nid(3), true)),
            top: nid(0),
        },
    );
    assert_eq!(f, focus(1, 3, FocusCause::Auto));
    assert!(fx.is_empty(), "already announced while loading");
}

#[test]
fn leaving_and_losing_documents() {
    assert_eq!(step(browse(1, 5), WebEv::FocusAway).0, WebMode::Inactive);
    assert_eq!(
        step(
            focus(1, 5, FocusCause::Auto),
            WebEv::DocGone { doc: DocId(1) }
        )
        .0,
        WebMode::Inactive
    );
    assert_eq!(
        step(browse(1, 5), WebEv::DocGone { doc: DocId(2) }).0,
        browse(1, 5)
    );
    let (b, fx) = step(
        focus(1, 5, FocusCause::Auto),
        WebEv::FocusRemoved { fallback: cur(2) },
    );
    assert_eq!(b, browse(1, 2));
    assert_eq!(fx.as_slice(), [WebFx::Announce(ModeWord::Browse)]);
}

// ---------------------------------------------------------------------------------------------
// The Reader over a real mirror

fn src(id: u64, role: Role, local: Option<u64>) -> SrcNode {
    SrcNode {
        id: SrcNodeId(id),
        role,
        states: StateSet::VISIBLE | StateSet::SHOWING | StateSet::ENABLED,
        name: None,
        text: None,
        attrs: smallvec::SmallVec::default(),
        local: local.map(LocalId),
    }
}

fn block(mut n: SrcNode) -> SrcNode {
    n.attrs.push((web_attrs::DISPLAY, Arc::from("block")));
    n
}

fn run(id: u64, t: &str) -> SrcNode {
    let mut n = src(id, Role::StaticText, None);
    n.text = Some(Arc::from(t));
    n
}

fn kids(parent: u64, ks: &[u64]) -> DocDelta {
    DocDelta::Children {
        parent: SrcNodeId(parent),
        children: ks.iter().map(|k| SrcNodeId(*k)).collect(),
    }
}

/// body "Test page"(1) ->
///   h1(2) "Welcome"
///   p(4) "Read " a(6) "the docs" " now."
///   h2(9) "Details"
///   p(11) "Last line"
///   p(13) [edit(14, editable)]
fn page() -> Vec<DocDelta> {
    let mut body = block(src(1, Role::DocumentWeb, Some(1001)));
    body.name = Some(Arc::from("Test page"));
    let mut h1 = block(src(2, Role::Heading, Some(1002)));
    h1.attrs.push((web_attrs::LEVEL, Arc::from("1")));
    let mut h2 = block(src(9, Role::Heading, Some(1009)));
    h2.attrs.push((web_attrs::LEVEL, Arc::from("2")));
    let mut link = src(6, Role::Link, Some(1006));
    link.states |= StateSet::FOCUSABLE;
    let mut edit = src(14, Role::TextEntry, Some(1014));
    edit.states |= StateSet::FOCUSABLE | StateSet::EDITABLE;
    vec![
        DocDelta::Reset {
            root: SrcNodeId(1),
            nav_id: 1,
        },
        DocDelta::Upsert(vec![
            body,
            h1,
            run(3, "Welcome"),
            block(src(4, Role::Paragraph, Some(1004))),
            run(5, "Read "),
            link,
            run(7, "the docs"),
            run(8, " now."),
            h2,
            run(10, "Details"),
            block(src(11, Role::Paragraph, Some(1011))),
            run(12, "Last line"),
            block(src(13, Role::Paragraph, Some(1013))),
            edit,
        ]),
        kids(1, &[2, 4, 9, 11, 13]),
        kids(2, &[3]),
        kids(4, &[5, 6, 8]),
        kids(6, &[7]),
        kids(9, &[10]),
        kids(11, &[12]),
        kids(13, &[14]),
        DocDelta::Settled { batch: 1 },
    ]
}

struct Rig {
    reader: Reader,
    docs: Arc<DocDirectory>,
    join: Arc<WebJoin>,
    writer: DocWriter,
    tr: Translator,
    mirror: MirrorWriter,
    epoch: Arc<EpochCell>,
    ctl: mpsc::Receiver<DocCtl>,
    now: Instant,
}

fn snap(local: u64, role: Role, name: &str, states: StateSet) -> Arc<NodeSnapshot> {
    Arc::new(NodeSnapshot {
        key: nk(local),
        role,
        states,
        name: Some(Arc::from(name)),
        description: None,
        value: None,
        parent: None,
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

fn escape_chord() -> Chord {
    Chord {
        keysym: 0xff1b,
        modifiers: Modifiers::empty(),
    }
}

fn rig() -> Rig {
    let epoch = Arc::new(EpochCell::new());
    let dir = Arc::new(MirrorDirectory::new());
    let (mut mw, handle) = mirror();
    dir.insert(app(), &handle);
    mw.apply(
        SnapshotBatch {
            nodes: vec![
                snap(1006, Role::Link, "the docs", StateSet::FOCUSABLE),
                snap(
                    1014,
                    Role::TextEntry,
                    "Name",
                    StateSet::EDITABLE | StateSet::FOCUSABLE,
                ),
                snap(1002, Role::Heading, "Welcome", StateSet::empty()),
                snap(2000, Role::Button, "Chrome", StateSet::FOCUSABLE),
                snap(1001, Role::DocumentWeb, "Test page", StateSet::FOCUSABLE),
            ],
            ..Default::default()
        },
        ApplyOrigin::Push,
    );
    let docs = Arc::new(DocDirectory::new());
    let join = Arc::new(WebJoin::new());
    let (mut writer, read) = channel(7);
    let mut tr = Translator::new();
    let out = tr.apply(&mut writer, page()).unwrap();
    writer.publish();
    for (l, id) in out.join_add {
        join.set(
            NodeKey {
                app: app(),
                local: l,
            },
            (DocId(1), id),
        );
    }
    let (ctl_tx, ctl) = mpsc::channel(16);
    let (ev_tx, _ev) = mpsc::channel(16);
    docs.insert(DocHandle {
        doc: DocId(1),
        app: app(),
        root: nk(1001),
        read,
        ctl: ctl_tx,
        events: ev_tx,
    });
    let keys = ModeKeys {
        web_escape: vec![escape_chord()],
        ..ModeKeys::default()
    };
    let mut reader =
        Reader::new(Arc::clone(&epoch), dir, keys).with_web(Arc::clone(&docs), Arc::clone(&join));
    // The M7a scenarios read line by line; page auto-read has its own tests below.
    Arc::make_mut(&mut reader.cmd.effective)
        .browse
        .say_all_on_load = false;
    let now = Instant::now();
    reader.handle(ReaderMsg::Doc(DocNotice::WebApp { app: app() }), now);
    Rig {
        reader,
        docs,
        join,
        writer,
        tr,
        mirror: mw,
        epoch,
        ctl,
        now,
    }
}

fn spoken(fx: &ReaderEffects) -> Vec<String> {
    fx.iter()
        .filter_map(|e| {
            if let ReaderEffect::Speak(s) = e {
                Some(
                    s.utterance
                        .segments
                        .iter()
                        .map(|g| &*g.text)
                        .collect::<Vec<_>>()
                        .join(" "),
                )
            } else {
                None
            }
        })
        .collect()
}

fn effects_web(fx: &ReaderEffects) -> Vec<WebEffect> {
    fx.iter()
        .filter_map(|e| {
            if let ReaderEffect::Web(w) = e {
                Some(w.clone())
            } else {
                None
            }
        })
        .collect()
}

impl Rig {
    fn focus(&mut self, local: u64) -> ReaderEffects {
        self.now += Duration::from_millis(200);
        self.reader.handle(
            ReaderMsg::Focus(crate::FocusLaneMsg::FocusChanged { node: nk(local) }),
            self.now,
        )
    }

    fn key(&mut self, cmd: CoreCmd) -> ReaderEffects {
        self.now += Duration::from_millis(50);
        let ev = KeyEvent {
            keycode: 1,
            keysym: 1,
            unichar: 0,
            modifiers: Modifiers::empty(),
            pressed: true,
        };
        self.reader.handle(
            ReaderMsg::Key(KeyIntent {
                event: ev,
                class: KeyClass::Command(cmd.id()),
                kind: PressKind::Press,
                repeat: 1,
                chord: None,
                at: self.now,
                epoch: Epoch(self.epoch.current().0 + 1),
            }),
            self.now,
        )
    }

    fn mode(&self) -> WebMode {
        self.reader.web_mode()
    }
}

#[test]
fn focus_on_a_link_enters_browse_mode_with_the_word_on_the_focus_speech() {
    let mut r = rig();
    let fx = r.focus(1006);
    assert!(matches!(r.mode(), WebMode::Browse { .. }));
    assert!(matches!(r.reader.mode(), Mode::Browse { .. }));
    // The first document the focus enters leads with its title.
    assert_eq!(spoken(&fx), ["browse mode Test page the docs link"]);
    assert!(
        effects_web(&fx)
            .iter()
            .any(|w| matches!(w, WebEffect::Activity { awake: true, .. })),
        "the active document is woken"
    );
    // Focus on the same kind of node again: no word.
    let fx = r.focus(1002);
    assert_eq!(spoken(&fx), ["Welcome heading"]);
}

#[test]
fn focus_on_an_edit_field_enters_focus_mode_and_grabs_escape() {
    let mut r = rig();
    let fx = r.focus(1014);
    assert!(matches!(
        r.mode(),
        WebMode::Focus {
            cause: FocusCause::Auto,
            ..
        }
    ));
    assert_eq!(r.reader.mode(), Mode::Focus);
    assert_eq!(spoken(&fx), ["focus mode Test page Name edit"]);
    let grabs: Vec<_> = fx
        .iter()
        .filter_map(|e| {
            if let ReaderEffect::SetGrabSet(g) = e {
                Some(g.clone())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(grabs.len(), 1, "one grab set for the transition");
    assert!(
        grabs[0].contains(0xff1b, Modifiers::empty()),
        "Escape is grabbed"
    );
    assert_eq!(grabs[0].ctx.mode, gorca_input_api::ModeKind::Browse);
    // Escape returns to browse mode, says so, and reads the line of the field.
    let fx = r.key(CoreCmd::BrowseEscape);
    assert!(matches!(r.mode(), WebMode::Browse { .. }));
    assert_eq!(
        r.reader.mode().context().mode,
        gorca_input_api::ModeKind::Browse
    );
    let said = spoken(&fx);
    assert_eq!(said.len(), 1);
    assert!(said[0].starts_with("browse mode"), "{said:?}");
    let last = fx.iter().rev().find_map(|e| {
        if let ReaderEffect::SetGrabSet(g) = e {
            Some(g.clone())
        } else {
            None
        }
    });
    assert!(last.is_some_and(|g| !g.contains(0xff1b, Modifiers::empty())));
}

#[test]
fn arrow_keys_read_lines_characters_and_words_without_ipc() {
    let mut r = rig();
    r.focus(1002);
    // The cursor starts on the heading's line.
    let fx = r.key(CoreCmd::BrowseCursorNextLine);
    assert_eq!(
        spoken(&fx),
        ["Read visited link the docs now."].map(|s| s.replace("visited ", ""))
    );
    let fx = r.key(CoreCmd::BrowseCursorNextChar);
    assert_eq!(spoken(&fx), ["e"]);
    let fx = r.key(CoreCmd::BrowseCursorNextWord);
    assert_eq!(spoken(&fx), ["link the"].map(String::from));
    let fx = r.key(CoreCmd::BrowseCursorPrevLine);
    assert_eq!(spoken(&fx), ["heading level 1 Welcome"]);
    let fx = r.key(CoreCmd::BrowseCursorPrevLine);
    assert_eq!(spoken(&fx), ["top"]);
    let fx = r.key(CoreCmd::BrowseCursorBottom);
    assert_eq!(spoken(&fx), ["edit blank"]);
    let fx = r.key(CoreCmd::BrowseCursorNextLine);
    assert_eq!(spoken(&fx), ["bottom"]);
    let fx = r.key(CoreCmd::BrowseCursorTop);
    assert_eq!(spoken(&fx), ["heading level 1 Welcome"]);
    // Nothing above issued a fetch or any request: reading is zero IPC.
    // (The rig has no connector; a fetch effect would show up in `fx`.)
}

#[test]
fn quick_navigation_speaks_the_target_with_its_role_and_never_wraps() {
    let mut r = rig();
    r.focus(1002);
    let fx = r.key(CoreCmd::QuicknavNextHeading);
    assert_eq!(spoken(&fx), ["heading level 2 Details"]);
    let fx = r.key(CoreCmd::QuicknavNextHeading);
    assert_eq!(spoken(&fx), ["no next heading"]);
    let fx = r.key(CoreCmd::QuicknavPrevHeading);
    assert_eq!(spoken(&fx), ["heading level 1 Welcome"]);
    let fx = r.key(CoreCmd::QuicknavPrevHeading);
    assert_eq!(spoken(&fx), ["no previous heading"]);
    let fx = r.key(CoreCmd::QuicknavNextHeading2);
    assert_eq!(spoken(&fx), ["heading level 2 Details"]);
    let fx = r.key(CoreCmd::QuicknavNextLink);
    assert_eq!(
        spoken(&fx),
        ["no next link"],
        "the link is above the cursor"
    );
    let fx = r.key(CoreCmd::QuicknavPrevLink);
    assert_eq!(spoken(&fx), ["link the docs"]);
    // Quick navigation never changes the mode.
    assert!(matches!(r.mode(), WebMode::Browse { .. }));
    // Moving by a quick-nav key schedules a debounced scroll of the target.
    assert!(
        r.reader
            .web
            .as_ref()
            .is_some_and(|w| w.scroll_due.is_some())
    );
}

#[test]
fn enter_on_a_link_clicks_it_and_on_a_field_enters_focus_mode() {
    let mut r = rig();
    r.focus(1006);
    let fx = r.key(CoreCmd::BrowseActivate);
    let acts = effects_web(&fx);
    assert!(
        acts.iter().any(|a| matches!(
            a,
            WebEffect::Act {
                act: DocAction::Click,
                ..
            }
        )),
        "{acts:?}"
    );
    assert!(matches!(r.mode(), WebMode::Browse { .. }));
    // On a field: `E` finds it without switching, Enter switches and focuses it.
    r.key(CoreCmd::QuicknavNextEdit);
    assert!(
        matches!(r.mode(), WebMode::Browse { .. }),
        "quick nav never switches"
    );
    let fx = r.key(CoreCmd::BrowseActivate);
    assert!(matches!(
        r.mode(),
        WebMode::Focus {
            cause: FocusCause::Auto,
            ..
        }
    ));
    assert!(effects_web(&fx).iter().any(|a| matches!(
        a,
        WebEffect::Act {
            act: DocAction::Focus,
            ..
        }
    )));
}

#[test]
fn the_toggle_switches_between_the_modes_and_speaks_them() {
    let mut r = rig();
    r.focus(1006);
    let fx = r.key(CoreCmd::BrowseToggle);
    assert!(matches!(
        r.mode(),
        WebMode::Focus {
            cause: FocusCause::Manual,
            ..
        }
    ));
    assert_eq!(spoken(&fx), ["focus mode"]);
    assert!(effects_web(&fx).iter().any(|a| matches!(
        a,
        WebEffect::Act {
            act: DocAction::Focus,
            ..
        }
    )));
    let fx = r.key(CoreCmd::BrowseToggle);
    assert!(matches!(r.mode(), WebMode::Browse { .. }));
    let said = spoken(&fx);
    assert!(said[0].starts_with("browse mode"), "{said:?}");
}

#[test]
fn focus_in_the_browser_chrome_leaves_browse_mode_and_asks_for_the_document() {
    let mut r = rig();
    r.focus(1006);
    assert!(matches!(r.reader.mode(), Mode::Browse { .. }));
    // A node of the web application that no document knows: chrome or a new node.
    let fx = r.focus(2000);
    assert!(matches!(r.mode(), WebMode::Loading { .. }));
    assert_eq!(
        r.reader.mode(),
        Mode::Focus,
        "browse keys are not grabbed while unsure"
    );
    assert!(
        effects_web(&fx)
            .iter()
            .any(|a| matches!(a, WebEffect::Ensure { focus } if *focus == nk(2000)))
    );
    // The registry says there is no document there.
    r.reader.handle(
        ReaderMsg::Doc(DocNotice::NoDocument { focus: nk(2000) }),
        r.now,
    );
    assert_eq!(r.mode(), WebMode::Inactive);
}

#[test]
fn a_node_of_another_application_leaves_web_mode() {
    let mut r = rig();
    r.focus(1006);
    r.now += Duration::from_millis(200);
    r.reader.handle(
        ReaderMsg::Focus(crate::FocusLaneMsg::FocusChanged {
            node: NodeKey {
                app: AppId {
                    connector: ConnectorId(1),
                    serial: 2,
                },
                local: LocalId(5),
            },
        }),
        r.now,
    );
    assert_eq!(r.mode(), WebMode::Inactive);
    assert_eq!(r.reader.mode(), Mode::Focus);
}

#[test]
fn a_new_document_in_the_focused_application_is_adopted_with_title_and_first_line() {
    let mut r = rig();
    r.focus(2000); // chrome: Loading
    r.reader.handle(
        ReaderMsg::Doc(DocNotice::NoDocument { focus: nk(2000) }),
        r.now,
    );
    assert_eq!(r.mode(), WebMode::Inactive);
    let fx = r.reader.handle(
        ReaderMsg::Doc(DocNotice::FirstUseful {
            doc: DocId(1),
            app: app(),
        }),
        r.now,
    );
    assert!(matches!(r.mode(), WebMode::Browse { .. }));
    assert_eq!(
        spoken(&fx),
        ["browse mode Test page heading level 1 Welcome"]
    );
}

#[test]
fn a_gone_document_returns_to_inactive_and_unregisters_the_cursor() {
    let mut r = rig();
    r.focus(1006);
    r.reader
        .handle(ReaderMsg::Doc(DocNotice::Gone { doc: DocId(1) }), r.now);
    assert_eq!(r.mode(), WebMode::Inactive);
    assert_eq!(r.reader.mode(), Mode::Focus);
}

#[test]
fn edits_under_the_cursor_do_not_strand_it() {
    let mut r = rig();
    r.focus(1006);
    r.key(CoreCmd::BrowseCursorNextLine);
    // The page drops the paragraph the cursor is in.
    r.tr.apply(
        &mut r.writer,
        vec![
            kids(1, &[2, 9, 11, 13]),
            DocDelta::Removed(vec![
                SrcNodeId(4),
                SrcNodeId(5),
                SrcNodeId(6),
                SrcNodeId(7),
                SrcNodeId(8),
            ]),
            DocDelta::Settled { batch: 2 },
        ],
    )
    .unwrap();
    r.writer.publish();
    r.reader.handle(
        ReaderMsg::Doc(DocNotice::Changed {
            doc: DocId(1),
            rev: 2,
        }),
        r.now,
    );
    let fx = r.key(CoreCmd::BrowseCursorNextLine);
    let said = spoken(&fx);
    assert_eq!(said.len(), 1, "{said:?}");
    let _ = (&r.join, &r.docs, &mut r.mirror, &mut r.ctl);
}

#[test]
fn coming_back_to_a_document_reads_its_title_and_the_line_where_the_cursor_was() {
    let mut r = rig();
    r.focus(1002);
    let fx = r.key(CoreCmd::BrowseCursorNextLine);
    assert_eq!(spoken(&fx), ["Read link the docs now."]);
    // Away to the browser chrome and back to the document itself (a tab switch).
    r.focus(2000);
    r.reader.handle(
        ReaderMsg::Doc(DocNotice::NoDocument { focus: nk(2000) }),
        r.now,
    );
    assert_eq!(r.mode(), WebMode::Inactive);
    let fx = r.focus(1001);
    assert!(matches!(r.mode(), WebMode::Browse { .. }));
    assert_eq!(
        spoken(&fx),
        ["browse mode Test page document Read link the docs now."]
    );
}

#[test]
fn the_first_focus_on_a_document_starts_at_its_first_line() {
    let mut r = rig();
    let fx = r.focus(1001);
    assert_eq!(
        spoken(&fx),
        ["browse mode Test page document heading level 1 Welcome"]
    );
    let fx = r.key(CoreCmd::BrowseCursorNextLine);
    assert_eq!(spoken(&fx), ["Read link the docs now."]);
}

// ---- reviewer probes turned into regression tests (M7a review: H2, M3, M4) ---------------

/// H2: a focused field the mirror does not know yet must never become browse mode.
#[test]
fn a_cold_unknown_focus_stays_loading_with_the_grabs_off() {
    let mut r = rig();
    r.focus(3000); // not in the desktop mirror, not in the join
    assert!(matches!(r.mode(), WebMode::Loading { known: false, .. }));
    r.reader.handle(
        ReaderMsg::Doc(DocNotice::InDocument {
            focus: nk(3000),
            doc: DocId(1),
        }),
        r.now,
    );
    r.mirror.apply(
        SnapshotBatch {
            nodes: vec![snap(
                3000,
                Role::TextEntry,
                "Search",
                StateSet::EDITABLE | StateSet::FOCUSABLE,
            )],
            ..Default::default()
        },
        ApplyOrigin::Push,
    );
    r.now += Duration::from_millis(1100);
    r.reader.handle(ReaderMsg::Timer, r.now);
    assert!(
        matches!(r.mode(), WebMode::Loading { .. }),
        "still waiting, got {:?}",
        r.mode()
    );
    assert!(
        !matches!(r.reader.mode(), Mode::Browse { .. }),
        "browse grabs must stay off while the focus may be a field"
    );
}

#[test]
fn a_better_reading_of_the_pending_focus_is_kept() {
    let now = Instant::now();
    let pending = |editable, known| WebEv::FocusPending {
        focus: nk(9),
        editable,
        known,
        now,
    };
    let (m, _) = step(WebMode::Inactive, pending(false, false));
    let (m, _) = step(m, pending(true, true));
    assert!(matches!(
        m,
        WebMode::Loading {
            editable: true,
            known: true,
            ..
        }
    ));
    // A later, vaguer reading does not erase it.
    let (m, _) = step(m, pending(false, false));
    assert!(matches!(
        m,
        WebMode::Loading {
            editable: true,
            known: true,
            ..
        }
    ));
}

fn page2_root_only() -> Vec<DocDelta> {
    let mut body = block(src(1, Role::DocumentWeb, Some(5001)));
    body.name = Some(Arc::from("Second"));
    vec![
        DocDelta::Reset {
            root: SrcNodeId(1),
            nav_id: 1,
        },
        DocDelta::Upsert(vec![body]),
        DocDelta::Settled { batch: 1 },
    ]
}

fn page2_rest() -> Vec<DocDelta> {
    vec![
        DocDelta::Upsert(vec![
            block(src(2, Role::Paragraph, Some(5002))),
            run(3, "First line of the second page"),
        ]),
        kids(1, &[2]),
        kids(2, &[3]),
        DocDelta::Settled { batch: 2 },
    ]
}

/// Enter on a link: the new document's first publish holds the root only; the first line is
/// spoken when the content arrives (M4).
fn navigate_to_a_page_that_builds_progressively(
    r: &mut Rig,
) -> (Vec<String>, Translator, DocWriter) {
    r.focus(1006);
    r.key(CoreCmd::BrowseCursorNextLine);
    let (mut w2, read2) = channel(9);
    let mut t2 = Translator::new();
    let out = t2.apply(&mut w2, page2_root_only()).unwrap();
    w2.publish();
    for (l, id) in out.join_add {
        r.join.set(
            NodeKey {
                app: app(),
                local: l,
            },
            (DocId(2), id),
        );
    }
    let (ctl_tx, _ctl2) = mpsc::channel(16);
    let (ev_tx, _ev2) = mpsc::channel(16);
    r.docs.insert(DocHandle {
        doc: DocId(2),
        app: app(),
        root: nk(5001),
        read: read2,
        ctl: ctl_tx,
        events: ev_tx,
    });
    r.reader.handle(
        ReaderMsg::Doc(DocNotice::FirstUseful {
            doc: DocId(2),
            app: app(),
        }),
        r.now,
    );
    r.mirror.apply(
        SnapshotBatch {
            nodes: vec![snap(5001, Role::DocumentWeb, "Second", StateSet::FOCUSABLE)],
            ..Default::default()
        },
        ApplyOrigin::Push,
    );
    let fx_focus = r.focus(5001);
    (spoken(&fx_focus), t2, w2)
}

fn deliver_page2_rest(r: &mut Rig, t2: &mut Translator, w2: &mut DocWriter) -> Vec<String> {
    let out = t2.apply(w2, page2_rest()).unwrap();
    w2.publish();
    for (l, id) in out.join_add {
        r.join.set(
            NodeKey {
                app: app(),
                local: l,
            },
            (DocId(2), id),
        );
    }
    let fx = r.reader.handle(
        ReaderMsg::Doc(DocNotice::Changed {
            doc: DocId(2),
            rev: 2,
        }),
        r.now,
    );
    spoken(&fx)
}

#[test]
fn the_first_line_of_a_page_that_builds_after_focus_is_spoken_when_it_arrives() {
    let mut r = rig();
    let (at_focus, mut t2, mut w2) = navigate_to_a_page_that_builds_progressively(&mut r);
    assert!(!at_focus.iter().any(|s| s.contains("First line")));
    r.now += Duration::from_millis(300);
    let late = deliver_page2_rest(&mut r, &mut t2, &mut w2);
    assert!(
        late.iter().any(|s| s.contains("First line")),
        "the first line was not spoken: {late:?}"
    );
}

#[test]
fn a_late_first_line_is_dropped_after_a_key_or_a_stale_wait() {
    // A key was pressed first: the user is already reading.
    let mut r = rig();
    let (_, mut t2, mut w2) = navigate_to_a_page_that_builds_progressively(&mut r);
    r.key(CoreCmd::BrowseCursorNextLine);
    let late = deliver_page2_rest(&mut r, &mut t2, &mut w2);
    assert!(!late.iter().any(|s| s.contains("First line")), "{late:?}");
    // Content that arrives long after the focus is not announced out of the blue.
    let mut r = rig();
    let (_, mut t2, mut w2) = navigate_to_a_page_that_builds_progressively(&mut r);
    r.now += Duration::from_secs(3);
    let late = deliver_page2_rest(&mut r, &mut t2, &mut w2);
    assert!(!late.iter().any(|s| s.contains("First line")), "{late:?}");
}

/// M3: the document vanished and its Gone notice never arrived: the next browse key gives the
/// keys back instead of swallowing them.
#[test]
fn browse_keys_on_a_vanished_document_hand_the_keyboard_back() {
    let mut r = rig();
    r.focus(1006);
    r.docs.remove(DocId(1));
    r.key(CoreCmd::QuicknavNextHeading);
    assert!(
        !matches!(r.reader.mode(), Mode::Browse { .. }),
        "browse grabs must be released, mode {:?}",
        r.reader.mode()
    );
    assert!(matches!(
        r.mode(),
        WebMode::Inactive | WebMode::Loading { .. }
    ));
}

// ---- integration with the M3 text layer ---------------------------------------------------

/// Say-selection belongs to the native text layer; in browse mode the document owns reading,
/// so it refuses with a short cue and starts no text work.
#[test]
fn say_selection_refuses_cleanly_in_browse_mode() {
    let mut r = rig();
    r.focus(1006);
    assert!(matches!(r.reader.mode(), Mode::Browse { .. }));
    let fx = r.key(CoreCmd::InfoSelection);
    assert_eq!(spoken(&fx), ["not available in this mode"]);
    assert!(
        !fx.iter().any(|e| matches!(
            e,
            ReaderEffect::SayAllChunk { .. } | ReaderEffect::TextFetch { .. }
        )),
        "started text work"
    );
}

// ---- M7b: say-all and page auto-read ------------------------------------------------------

fn chunks(fx: &ReaderEffects) -> Vec<(String, bool)> {
    fx.iter()
        .filter_map(|e| {
            if let ReaderEffect::SayAllChunk { intent, first } = e {
                Some((
                    intent
                        .utterance
                        .segments
                        .iter()
                        .map(|g| &*g.text)
                        .collect::<Vec<_>>()
                        .join(" "),
                    *first,
                ))
            } else {
                None
            }
        })
        .collect()
}

fn chunk_ids(fx: &ReaderEffects) -> Vec<gorca_model::UttId> {
    fx.iter()
        .filter_map(|e| {
            if let ReaderEffect::SayAllChunk { intent, .. } = e {
                Some(intent.utterance.id)
            } else {
                None
            }
        })
        .collect()
}

fn progress(r: &mut Rig, p: gorca_speech::LaneProgress) -> ReaderEffects {
    r.reader
        .handle(ReaderMsg::Text(crate::text::TextMsg::Progress(p)), r.now)
}

/// The cursor's flat offset.
fn cursor_off(r: &Rig) -> u32 {
    let WebMode::Browse { doc, cur, .. } = r.mode() else {
        panic!("not browsing: {:?}", r.mode());
    };
    r.docs
        .get(doc)
        .unwrap()
        .read
        .with(|d| cursor::offset_of(d, cur).unwrap())
}

#[test]
fn say_all_in_browse_mode_reads_the_page_in_chunks_as_the_line_presenter_speaks_it() {
    let mut r = rig();
    r.focus(1006); // the link: browse mode, cursor in "Read the docs now."
    let fx = r.key(CoreCmd::TextSayAll);
    let c = chunks(&fx);
    assert!(!c.is_empty(), "{fx:?}");
    assert!(c[0].1, "the first chunk interrupts");
    assert!(
        c[0].0.contains("link") && c[0].0.contains("the docs"),
        "objects are spoken by role and name: {c:?}"
    );
    assert!(
        c.iter()
            .any(|(t, _)| t.contains("heading level 2") && t.contains("Details")),
        "headings are announced: {c:?}"
    );
    assert!(
        !fx.iter()
            .any(|e| matches!(e, ReaderEffect::TextFetch { .. }))
    );
    assert!(
        matches!(
            r.reader.text.say_all,
            crate::text::say_all::SayAll::Running(_)
        ),
        "running"
    );
}

#[test]
fn the_virtual_cursor_follows_the_chunk_the_server_starts() {
    let mut r = rig();
    r.focus(1006);
    let fx = r.key(CoreCmd::TextSayAll);
    let ids = chunk_ids(&fx);
    assert!(ids.len() >= 2, "{fx:?}");
    let start = cursor_off(&r);
    let _ = progress(&mut r, gorca_speech::LaneProgress::Started(ids[0]));
    let _ = progress(&mut r, gorca_speech::LaneProgress::Ended(ids[0]));
    let fx = progress(&mut r, gorca_speech::LaneProgress::Started(ids[1]));
    let after = cursor_off(&r);
    assert!(
        after > start,
        "the cursor moved on with the reading: {start} -> {after}"
    );
    // Scrolling is asked for after the pause, not on every chunk.
    assert!(effects_web(&fx).is_empty());
    // A key stops the reading where it is: the next move starts at the chunk being read.
    let _ = r.key(CoreCmd::BrowseCursorNextLine);
    assert!(matches!(
        r.reader.text.say_all,
        crate::text::say_all::SayAll::Idle
    ));
    assert!(cursor_off(&r) >= after);
}

#[test]
fn a_reading_that_reaches_the_end_stops_quietly() {
    let mut r = rig();
    r.focus(1006);
    let fx = r.key(CoreCmd::TextSayAll);
    let mut all = chunk_ids(&fx);
    // Keep the server fed until the reading has nothing left to submit.
    let mut done = 0;
    while done < all.len() && done < 50 {
        let id = all[done];
        let _ = progress(&mut r, gorca_speech::LaneProgress::Started(id));
        let fx = progress(&mut r, gorca_speech::LaneProgress::Ended(id));
        all.extend(chunk_ids(&fx));
        done += 1;
    }
    assert!(matches!(
        r.reader.text.say_all,
        crate::text::say_all::SayAll::Idle
    ));
    assert!(done >= 3, "the page has several chunks");
}

fn first_publish(r: &mut Rig) -> ReaderEffects {
    r.reader.handle(
        ReaderMsg::Doc(DocNotice::FirstUseful {
            doc: DocId(1),
            app: app(),
        }),
        r.now,
    )
}

fn auto_read_on(r: &mut Rig) {
    Arc::make_mut(&mut r.reader.cmd.effective)
        .browse
        .say_all_on_load = true;
}

fn into_chrome(r: &mut Rig) {
    r.focus(2000);
    r.reader.handle(
        ReaderMsg::Doc(DocNotice::NoDocument { focus: nk(2000) }),
        r.now,
    );
}

#[test]
fn a_loaded_page_speaks_its_title_and_then_reads_itself() {
    let mut r = rig();
    auto_read_on(&mut r);
    into_chrome(&mut r);
    let _ = r
        .reader
        .handle(ReaderMsg::Doc(DocNotice::Loaded { node: nk(1001) }), r.now);
    let fx = first_publish(&mut r);
    assert_eq!(spoken(&fx), ["browse mode Test page"], "title only");
    let c = chunks(&fx);
    assert!(!c.is_empty(), "the page starts reading: {fx:?}");
    assert!(!c[0].1, "queued behind the title, it does not cut it off");
    assert!(
        c[0].0.contains("heading level 1") && c[0].0.contains("Welcome"),
        "{c:?}"
    );
}

#[test]
fn a_page_that_loads_after_its_first_publish_starts_reading_at_load_complete() {
    let mut r = rig();
    auto_read_on(&mut r);
    into_chrome(&mut r);
    let fx = first_publish(&mut r);
    assert_eq!(spoken(&fx), ["browse mode Test page"]);
    assert!(chunks(&fx).is_empty(), "not loaded yet");
    let fx = r
        .reader
        .handle(ReaderMsg::Doc(DocNotice::Loaded { node: nk(1001) }), r.now);
    assert!(!chunks(&fx).is_empty(), "load-complete starts it: {fx:?}");
}

#[test]
fn a_page_whose_load_complete_never_comes_reads_itself_after_a_moment() {
    let mut r = rig();
    auto_read_on(&mut r);
    into_chrome(&mut r);
    let fx = first_publish(&mut r);
    assert!(
        arms(&fx).len() == 1,
        "the wait is bounded by a timer: {fx:?}"
    );
    r.now += Duration::from_millis(2000);
    let fx = r.reader.handle(ReaderMsg::Timer, r.now);
    assert!(!chunks(&fx).is_empty(), "{fx:?}");
}

#[test]
fn a_key_before_load_complete_cancels_the_auto_read() {
    let mut r = rig();
    auto_read_on(&mut r);
    into_chrome(&mut r);
    let _ = first_publish(&mut r);
    let _ = r.key(CoreCmd::BrowseCursorNextLine);
    let fx = r
        .reader
        .handle(ReaderMsg::Doc(DocNotice::Loaded { node: nk(1001) }), r.now);
    assert!(chunks(&fx).is_empty(), "the user already took over: {fx:?}");
    r.now += Duration::from_millis(3000);
    let fx = r.reader.handle(ReaderMsg::Timer, r.now);
    assert!(chunks(&fx).is_empty(), "{fx:?}");
}

#[test]
fn auto_read_can_be_switched_off_and_then_the_first_line_is_spoken_as_before() {
    let mut r = rig();
    into_chrome(&mut r);
    let _ = r
        .reader
        .handle(ReaderMsg::Doc(DocNotice::Loaded { node: nk(1001) }), r.now);
    let fx = first_publish(&mut r);
    assert_eq!(
        spoken(&fx),
        ["browse mode Test page heading level 1 Welcome"]
    );
    assert!(chunks(&fx).is_empty());
}

// ---- re-review: timers (N1) and the Firefox typing path (N5) -------------------------------

fn type_a(r: &mut Rig) -> ReaderEffects {
    r.now += Duration::from_millis(50);
    let ev = KeyEvent {
        keycode: 38,
        keysym: 0x61,
        unichar: 'a' as u32,
        modifiers: Modifiers::empty(),
        pressed: true,
    };
    r.reader.handle(
        ReaderMsg::Key(KeyIntent {
            event: ev,
            class: KeyClass::PassToApp,
            kind: PressKind::Press,
            repeat: 1,
            chord: None,
            at: r.now,
            epoch: Epoch(r.epoch.current().0 + 1),
        }),
        r.now,
    )
}

fn arms(fx: &ReaderEffects) -> Vec<Instant> {
    fx.iter()
        .filter_map(|e| {
            if let ReaderEffect::ArmTimer(t) = e {
                Some(*t)
            } else {
                None
            }
        })
        .collect()
}

/// N5: the real Firefox path. The app counts UTF-16 units; the caret layer follows them as
/// they are (M3b), owns the typed key and speaks the letter once.
#[test]
fn a_firefox_field_is_echoed_once_by_the_caret_layer() {
    let mut r = rig();
    r.reader
        .dir
        .set_text_unit(app(), gorca_model::TextUnit::Utf16Bom);
    r.focus(1014);
    assert!(
        r.reader.text.caret.tracked().is_some(),
        "the caret layer tracks the field"
    );
    let fx = type_a(&mut r);
    assert_eq!(spoken(&fx), ["a"]);
}

/// N1: a timer that fires for another subsystem before a settling focus is due must arm the
/// focus deadline again.
#[test]
fn an_early_timer_keeps_the_focus_settle_deadline_armed() {
    let mut r = rig();
    r.mirror.apply(
        SnapshotBatch {
            nodes: vec![snap(4000, Role::MenuItem, "Open", StateSet::FOCUSABLE)],
            ..Default::default()
        },
        ApplyOrigin::Push,
    );
    r.focus(1006);
    let fx = r.focus(4000);
    let due = arms(&fx);
    assert!(spoken(&fx).is_empty() && !due.is_empty());
    let early = r.now + Duration::from_millis(5);
    let fx = r.reader.handle(ReaderMsg::Timer, early);
    assert!(spoken(&fx).is_empty());
    let rearm = arms(&fx);
    let settle = *due.iter().min().unwrap_or(&r.now);
    assert!(
        rearm.contains(&settle),
        "the settle deadline {due:?} was not armed again: {rearm:?}"
    );
    let fx = r.reader.handle(ReaderMsg::Timer, settle);
    assert!(
        spoken(&fx).iter().any(|s| s.contains("Open menu item")),
        "{:?}",
        spoken(&fx)
    );
}

/// N1, end to end in the Reader: Down arms the scroll debounce (150 ms); a menu opens inside
/// that window; the debounce timer fires first; the menu item is still spoken.
#[test]
fn a_menu_opened_within_the_scroll_debounce_is_spoken() {
    let mut r = rig();
    r.mirror.apply(
        SnapshotBatch {
            nodes: vec![snap(4000, Role::MenuItem, "Open", StateSet::FOCUSABLE)],
            ..Default::default()
        },
        ApplyOrigin::Push,
    );
    r.focus(1006);
    let fx = r.key(CoreCmd::BrowseCursorNextLine);
    let scroll = arms(&fx);
    assert!(!scroll.is_empty(), "Down arms the scroll debounce");
    let fx = r.focus(4000);
    let settle = arms(&fx);
    assert!(!settle.is_empty());
    // The runtime wakes the Reader for the earlier deadline first.
    let first = *scroll.iter().chain(settle.iter()).min().unwrap_or(&r.now);
    let fx = r.reader.handle(ReaderMsg::Timer, first);
    let mut said = spoken(&fx);
    for t in arms(&fx) {
        said.extend(spoken(&r.reader.handle(ReaderMsg::Timer, t)));
    }
    assert!(
        said.iter().any(|s| s.contains("Open")),
        "the menu item was never spoken: {said:?}"
    );
}

/// N1: the same for the browse `Loading` grace.
#[test]
fn an_early_timer_keeps_the_loading_grace_armed() {
    let mut r = rig();
    // In the mirror (so its snapshot is known and not a field) but not in the document join.
    r.mirror.apply(
        SnapshotBatch {
            nodes: vec![snap(3001, Role::Link, "late", StateSet::FOCUSABLE)],
            ..Default::default()
        },
        ApplyOrigin::Push,
    );
    r.focus(3001);
    assert!(matches!(r.mode(), WebMode::Loading { known: true, .. }));
    r.reader.handle(
        ReaderMsg::Doc(DocNotice::InDocument {
            focus: nk(3001),
            doc: DocId(1),
        }),
        r.now,
    );
    let early = r.now + Duration::from_millis(100);
    let fx = r.reader.handle(ReaderMsg::Timer, early);
    assert!(!arms(&fx).is_empty(), "the grace deadline was dropped");
}

// ---- re-review N2: a focus whose fetch fails is never silent -------------------------------

fn fetch_epoch(fx: &ReaderEffects) -> Option<Epoch> {
    fx.iter().find_map(|e| {
        if let ReaderEffect::Fetch { epoch, .. } = e {
            Some(*epoch)
        } else {
            None
        }
    })
}

fn failed_reply(node: NodeKey, epoch: Epoch) -> ReaderMsg {
    ReaderMsg::Reply(crate::QueryReply {
        node,
        epoch,
        kind: crate::FetchKind::Focus,
        result: Err(gorca_connector_api::BackendError::Timeout),
    })
}

#[test]
fn a_web_focus_whose_fetch_times_out_is_spoken_from_the_document_mirror() {
    let mut r = rig();
    // 1009 is in the document join but not in the B-1 mirror: the focus needs a fetch.
    let fx = r.focus(1009);
    let epoch = fetch_epoch(&fx).expect("a focus fetch");
    let fx = r.reader.handle(failed_reply(nk(1009), epoch), r.now);
    let said = spoken(&fx);
    assert!(!said.is_empty(), "the focus stayed silent");
    assert!(said.iter().any(|s| s.contains("heading")), "{said:?}");
}

#[test]
fn a_native_focus_whose_fetch_fails_is_retried_once_and_then_says_unknown() {
    let mut r = rig();
    // 7777 is nowhere: not in the mirror and not in a document.
    let fx = r.focus(7777);
    let epoch = fetch_epoch(&fx).expect("a focus fetch");
    let fx = r.reader.handle(failed_reply(nk(7777), epoch), r.now);
    assert!(fetch_epoch(&fx).is_some(), "one more try");
    assert!(spoken(&fx).is_empty());
    let fx = r.reader.handle(failed_reply(nk(7777), epoch), r.now);
    assert_eq!(spoken(&fx), ["unknown"]);
}

// ---- M7b: Escape passes through an open popup ------------------------------------------------

fn last_grab(fx: &ReaderEffects) -> Option<gorca_input_api::GrabSet> {
    fx.iter().rev().find_map(|e| {
        if let ReaderEffect::SetGrabSet(g) = e {
            Some(g.clone())
        } else {
            None
        }
    })
}

/// The field changes its `expanded` state in the page and the mirror says so.
fn set_expanded(r: &mut Rig, on: bool) -> ReaderEffects {
    let mut edit = src(14, Role::TextEntry, Some(1014));
    edit.states |= StateSet::FOCUSABLE | StateSet::EDITABLE;
    if on {
        edit.states |= StateSet::EXPANDED;
    }
    r.tr.apply(&mut r.writer, vec![DocDelta::Upsert(vec![edit])])
        .unwrap();
    r.writer.publish();
    r.reader.handle(
        ReaderMsg::Doc(DocNotice::Changed {
            doc: DocId(1),
            rev: 9,
        }),
        r.now,
    )
}

#[test]
fn escape_belongs_to_the_page_while_the_field_has_a_popup_open() {
    let mut r = rig();
    let fx = r.focus(1014);
    assert!(last_grab(&fx).is_some_and(|g| g.contains(0xff1b, Modifiers::empty())));
    // The drop-down opens: Escape is released so the page can close it.
    let fx = set_expanded(&mut r, true);
    let g = last_grab(&fx).expect("the grab set is republished");
    assert!(
        !g.contains(0xff1b, Modifiers::empty()),
        "Escape is the page's now"
    );
    assert!(
        matches!(r.mode(), WebMode::Focus { .. }),
        "still focus mode"
    );
    // It closes: the next Escape returns to browse mode.
    let fx = set_expanded(&mut r, false);
    let g = last_grab(&fx).expect("the grab set is republished");
    assert!(g.contains(0xff1b, Modifiers::empty()));
    let fx = r.key(CoreCmd::BrowseEscape);
    assert!(matches!(r.mode(), WebMode::Browse { .. }));
    assert!(spoken(&fx)[0].starts_with("browse mode"));
}

#[test]
fn a_field_that_is_open_when_it_gets_the_focus_does_not_grab_escape() {
    let mut r = rig();
    // The field is expanded before the focus arrives.
    let _ = set_expanded(&mut r, true);
    let fx = r.focus(1014);
    assert!(matches!(r.mode(), WebMode::Focus { .. }));
    let g = last_grab(&fx).expect("a grab set for the transition");
    assert!(!g.contains(0xff1b, Modifiers::empty()));
}

// ---- M7b: frame and article quick navigation ---------------------------------------------------

#[test]
fn quick_navigation_finds_articles_and_frames_and_names_them() {
    let mut r = rig();
    r.focus(1002);
    let mut article = block(src(20, Role::Paragraph, None));
    article.attrs.push((web_attrs::TAG, Arc::from("article")));
    let mut frame = block(src(30, Role::DocumentWeb, None));
    frame.name = Some(Arc::from("Ad frame"));
    r.tr.apply(
        &mut r.writer,
        vec![
            DocDelta::Upsert(vec![
                article,
                run(21, "An article text"),
                frame,
                run(31, "Inside the frame"),
            ]),
            kids(1, &[2, 4, 9, 11, 13, 20, 30]),
            kids(20, &[21]),
            kids(30, &[31]),
            DocDelta::Settled { batch: 2 },
        ],
    )
    .unwrap();
    r.writer.publish();
    r.reader.handle(
        ReaderMsg::Doc(DocNotice::Changed {
            doc: DocId(1),
            rev: 2,
        }),
        r.now,
    );
    let fx = r.key(CoreCmd::QuicknavNextArticle);
    assert_eq!(spoken(&fx), ["article An article text"]);
    let fx = r.key(CoreCmd::QuicknavNextArticle);
    assert_eq!(spoken(&fx), ["no next article"]);
    let fx = r.key(CoreCmd::QuicknavNextFrame);
    assert_eq!(spoken(&fx), ["frame Ad frame Inside the frame"]);
    let fx = r.key(CoreCmd::QuicknavNextFrame);
    assert_eq!(spoken(&fx), ["no next frame"]);
    let fx = r.key(CoreCmd::QuicknavPrevArticle);
    assert_eq!(spoken(&fx), ["article An article text"]);
    let fx = r.key(CoreCmd::QuicknavPrevFrame);
    assert_eq!(spoken(&fx), ["no previous frame"]);
}

/// A frame document (a nested `DocumentWeb`, local 1030) joined to the page of the rig.
fn with_frame(r: &mut Rig) {
    r.focus(1002);
    let mut frame = block(src(30, Role::DocumentWeb, Some(1030)));
    frame.name = Some(Arc::from("Ad frame"));
    let out =
        r.tr.apply(
            &mut r.writer,
            vec![
                DocDelta::Upsert(vec![frame, run(31, "Inside the frame")]),
                kids(1, &[2, 4, 9, 11, 13, 30]),
                kids(30, &[31]),
                DocDelta::Settled { batch: 2 },
            ],
        )
        .unwrap();
    r.writer.publish();
    for (l, id) in out.join_add {
        r.join.set(
            NodeKey {
                app: app(),
                local: l,
            },
            (DocId(1), id),
        );
    }
    r.mirror.apply(
        SnapshotBatch {
            nodes: vec![snap(
                1030,
                Role::DocumentWeb,
                "Ad frame",
                StateSet::FOCUSABLE,
            )],
            ..Default::default()
        },
        ApplyOrigin::Push,
    );
    r.reader.handle(
        ReaderMsg::Doc(DocNotice::Changed {
            doc: DocId(1),
            rev: 2,
        }),
        r.now,
    );
}

#[test]
fn a_frame_document_on_the_way_to_its_content_stays_silent() {
    // Chromium focuses the frame's document, then the link inside it: one announcement.
    let mut r = rig();
    with_frame(&mut r);
    let fx = r.focus(1030);
    assert!(
        spoken(&fx).is_empty(),
        "the frame stop is deferred: {:?}",
        spoken(&fx)
    );
    let fx = r.focus(1006);
    assert_eq!(spoken(&fx), ["the docs link"]);
    r.now += Duration::from_millis(400);
    let fx = r.reader.handle(ReaderMsg::Timer, r.now);
    assert!(
        spoken(&fx).is_empty(),
        "the deferred frame speech is cancelled: {:?}",
        spoken(&fx)
    );
}

#[test]
fn a_tab_stop_on_a_frame_document_is_announced_when_nothing_follows() {
    // Firefox stops Tab on the frame document itself: the stop speaks the frame (L-1).
    let mut r = rig();
    with_frame(&mut r);
    let fx = r.focus(1030);
    assert!(spoken(&fx).is_empty());
    r.now += Duration::from_millis(400);
    let fx = r.reader.handle(ReaderMsg::Timer, r.now);
    let said = spoken(&fx);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].contains("Ad frame"),
        "the frame title is spoken: {said:?}"
    );
}

#[test]
fn the_load_complete_of_another_document_does_not_start_the_read() {
    // Review F-8: an iframe (or another tab) of the same browser finished loading a moment
    // ago; the page that is still loading must wait for its own load-complete.
    let mut r = rig();
    auto_read_on(&mut r);
    into_chrome(&mut r);
    let _ = r
        .reader
        .handle(ReaderMsg::Doc(DocNotice::Loaded { node: nk(7777) }), r.now);
    let fx = first_publish(&mut r);
    assert_eq!(spoken(&fx), ["browse mode Test page"]);
    assert!(
        chunks(&fx).is_empty(),
        "another document's load-complete read this page: {fx:?}"
    );
}

// ---- M8: containers, selection, single-letter toggle, out-of announcements -------------------

/// The page with a two-item list between "Details" and "Last line".
fn with_list(r: &mut Rig) {
    r.focus(1002);
    let list = block(src(40, Role::List, None));
    let li1 = block(src(41, Role::ListItem, None));
    let li2 = block(src(43, Role::ListItem, None));
    r.tr.apply(
        &mut r.writer,
        vec![
            DocDelta::Upsert(vec![list, li1, run(42, "One"), li2, run(44, "Two")]),
            kids(1, &[2, 4, 9, 40, 11, 13]),
            kids(40, &[41, 43]),
            kids(41, &[42]),
            kids(43, &[44]),
            DocDelta::Settled { batch: 2 },
        ],
    )
    .unwrap();
    r.writer.publish();
    r.reader.handle(
        ReaderMsg::Doc(DocNotice::Changed {
            doc: DocId(1),
            rev: 2,
        }),
        r.now,
    );
}

#[test]
fn comma_jumps_past_the_end_of_the_container_and_shift_comma_back_to_its_start() {
    let mut r = rig();
    with_list(&mut r);
    // Outside any container.
    let fx = r.key(CoreCmd::BrowseContainerEnd);
    assert_eq!(spoken(&fx), ["Not in a container"]);
    let fx = r.key(CoreCmd::QuicknavNextList);
    assert_eq!(spoken(&fx)[0], "list with 2 items One");
    // Past the end: the line after the list, and the exit is announced.
    let fx = r.key(CoreCmd::BrowseContainerEnd);
    assert_eq!(spoken(&fx), ["out of list Last line"]);
    // Back inside, then to the start of the list.
    let _ = r.key(CoreCmd::QuicknavPrevList);
    let fx = r.key(CoreCmd::BrowseCursorNextLine);
    assert_eq!(spoken(&fx), ["Two"]);
    let fx = r.key(CoreCmd::BrowseContainerStart);
    assert_eq!(spoken(&fx), ["One"], "already inside: not announced again");
}

#[test]
fn single_letter_navigation_toggles_and_releases_the_bare_keys() {
    let mut r = rig();
    r.focus(1002);
    let h = Chord {
        keysym: 0x68,
        modifiers: Modifiers::empty(),
    };
    let down = Chord {
        keysym: 0xff54,
        modifiers: Modifiers::empty(),
    };
    r.reader.keys.letters = vec![h];
    r.reader.keys.quick_nav = vec![h, down];
    let fx = r.key(CoreCmd::BrowseSingleLetterToggle);
    assert_eq!(spoken(&fx), ["single letter navigation off"]);
    let g = last_grab(&fx).expect("the grab set is republished");
    assert!(!g.contains(0x68, Modifiers::empty()), "H goes to the page");
    assert!(g.contains(0xff54, Modifiers::empty()), "Down stays ours");
    let fx = r.key(CoreCmd::BrowseSingleLetterToggle);
    assert_eq!(spoken(&fx), ["single letter navigation on"]);
    let g = last_grab(&fx).expect("the grab set is republished");
    assert!(g.contains(0x68, Modifiers::empty()));
}

#[test]
fn shift_arrows_select_and_unselect_and_a_plain_move_clears() {
    let mut r = rig();
    r.focus(1002);
    let fx = r.key(CoreCmd::BrowseSelectNextChar);
    assert_eq!(spoken(&fx), ["W selected"]);
    let fx = r.key(CoreCmd::BrowseSelectNextChar);
    assert_eq!(spoken(&fx), ["e selected"]);
    let fx = r.key(CoreCmd::BrowseSelectPrevChar);
    assert_eq!(spoken(&fx), ["e unselected"]);
    assert!(r.reader.web.as_ref().is_some_and(|w| w.sel.is_some()));
    let _ = r.key(CoreCmd::BrowseCursorNextChar);
    assert!(r.reader.web.as_ref().is_some_and(|w| w.sel.is_none()));
    let fx = r.key(CoreCmd::BrowseSelectAll);
    assert_eq!(spoken(&fx), ["all selected"]);
}

#[test]
fn a_field_that_gets_the_focus_with_its_help_text_is_not_mistaken_for_a_dialog() {
    // The dialog lead is by node: a focus in the same document without a dialog says nothing.
    let mut r = rig();
    r.focus(1002);
    let fx = r.focus(1014);
    assert!(!spoken(&fx).iter().any(|s| s.contains("dialog")));
}

// ---- review round 1: H-1 (D-2 Tab) and M-5 (native selection) ---------------------------------

fn tab_chord() -> Chord {
    Chord {
        keysym: 0xff09,
        modifiers: Modifiers::empty(),
    }
}

fn shift_tab_chord() -> Chord {
    Chord {
        keysym: 0xfe20,
        modifiers: Modifiers::SHIFT,
    }
}

/// A rig whose document root is FOCUSABLE as Gecko reports it and whose keys know the Tab chords.
fn tab_rig() -> Rig {
    let mut r = rig();
    let mut body = block(src(1, Role::DocumentWeb, Some(1001)));
    body.name = Some(Arc::from("Test page"));
    body.states |= StateSet::FOCUSABLE;
    r.tr.apply(
        &mut r.writer,
        vec![DocDelta::Upsert(vec![body]), DocDelta::Settled { batch: 2 }],
    )
    .unwrap();
    r.writer.publish();
    r.reader.keys.quick_nav = vec![tab_chord(), shift_tab_chord()];
    r.reader.keys.tab_next = vec![tab_chord()];
    r.reader.keys.tab_prev = vec![shift_tab_chord()];
    r
}

fn acts(fx: &ReaderEffects) -> Vec<(DocAction, DocNodeId)> {
    effects_web(fx)
        .iter()
        .filter_map(|w| match w {
            WebEffect::Act { node, act, .. } | WebEffect::Select { node, act, .. } => {
                Some((act.clone(), *node))
            }
            WebEffect::Ensure { .. } | WebEffect::Activity { .. } => None,
        })
        .collect()
}

fn last_grabs(fx: &ReaderEffects) -> Option<gorca_input_api::GrabSet> {
    fx.iter().rev().find_map(|e| {
        if let ReaderEffect::SetGrabSet(g) = e {
            Some(g.clone())
        } else {
            None
        }
    })
}

/// Let the debounced cursor timers run (scroll, tab availability).
fn settle(r: &mut Rig) -> ReaderEffects {
    r.now += Duration::from_millis(300);
    r.reader.handle(ReaderMsg::Timer, r.now)
}

#[test]
fn reading_never_sets_a_page_caret_or_moves_focus() {
    // H-1: the caret-follow substitute is gone: Down, K and the timers send only scrolls.
    let mut r = tab_rig();
    r.focus(1002);
    let mut all = acts(&settle(&mut r));
    for cmd in [
        CoreCmd::BrowseCursorNextLine,
        CoreCmd::QuicknavNextLink,
        CoreCmd::BrowseCursorNextLine,
        CoreCmd::BrowseCursorNextChar,
    ] {
        let fx = r.key(cmd);
        all.extend(acts(&fx));
        all.extend(acts(&settle(&mut r)));
    }
    assert!(!all.is_empty(), "the scrolls still go out");
    assert!(
        all.iter()
            .all(|(a, _)| matches!(a, DocAction::ScrollIntoView)),
        "reading only scrolls: {all:?}"
    );
}

#[test]
fn tab_focuses_the_next_focusable_after_the_cursor_and_shift_tab_the_previous() {
    let mut r = tab_rig();
    r.focus(1002);
    settle(&mut r);
    let link = r.join.get(nk(1006)).unwrap().1;
    let edit = r.join.get(nk(1014)).unwrap().1;
    // From the heading: the link comes first, never the document root.
    let fx = r.key(CoreCmd::BrowseTabNext);
    assert_eq!(acts(&fx), [(DocAction::Focus, link)]);
    // The cursor moves with a plain read to the last line: the edit is next.
    for _ in 0..3 {
        r.key(CoreCmd::BrowseCursorNextLine);
    }
    let fx = r.key(CoreCmd::BrowseTabNext);
    assert_eq!(acts(&fx), [(DocAction::Focus, edit)]);
    // Shift+Tab from the same place goes back to the link, never to the document.
    let fx = r.key(CoreCmd::BrowseTabPrev);
    assert_eq!(acts(&fx), [(DocAction::Focus, link)]);
}

#[test]
fn tab_is_grabbed_only_in_the_directions_that_have_a_target() {
    let mut r = tab_rig();
    let fx = r.focus(1002);
    let g = last_grabs(&fx).expect("browse mode sets the grabs");
    assert!(
        g.contains(0xff09, Modifiers::empty()),
        "a target after the heading"
    );
    assert!(
        !g.contains(0xfe20, Modifiers::SHIFT),
        "nothing focusable before the heading: Shift+Tab goes to the page"
    );
    // Past the last focusable (the edit): Tab is released, Shift+Tab comes back.
    for _ in 0..6 {
        r.key(CoreCmd::BrowseCursorNextLine);
    }
    let fx = settle(&mut r);
    let g = last_grabs(&fx).expect("the grab set follows the cursor");
    assert!(!g.contains(0xff09, Modifiers::empty()));
    assert!(g.contains(0xfe20, Modifiers::SHIFT));
    // A grab that went stale says so instead of staying silent.
    let fx = r.key(CoreCmd::BrowseTabNext);
    assert!(acts(&fx).is_empty());
    assert_eq!(spoken(&fx), ["no next focusable element"]);
}

/// The first `SetSelection` the effects carry: its exact spans as `(range)` per container.
fn selections(fx: &ReaderEffects) -> Vec<gorca_model::SelectionSpec<DocNodeId>> {
    acts(fx)
        .into_iter()
        .filter_map(|(a, _)| match a {
            DocAction::SetSelection(spec) => Some(spec),
            DocAction::Focus
            | DocAction::Click
            | DocAction::ScrollIntoView
            | DocAction::SetCaret(_)
            | DocAction::SetSourceSelection(_)
            | DocAction::ClearSelection => None,
        })
        .collect()
}

fn cleared(fx: &ReaderEffects) -> bool {
    acts(fx)
        .iter()
        .any(|(a, _)| matches!(a, DocAction::ClearSelection))
}

#[test]
fn the_page_selection_follows_the_virtual_one_so_the_browsers_ctrl_c_copies_it() {
    let mut r = tab_rig();
    r.focus(1002);
    settle(&mut r);
    r.key(CoreCmd::BrowseCursorTop);
    // A held Shift+Right sets the page selection once, after the debounce.
    let mut early = Vec::new();
    for _ in 0..3 {
        early.extend(selections(&r.key(CoreCmd::BrowseSelectNextChar)));
    }
    assert!(early.is_empty(), "nothing before the debounce: {early:?}");
    let sel = selections(&settle(&mut r));
    assert_eq!(sel.len(), 1, "{sel:?}");
    assert_eq!(sel[0].spans.len(), 1);
    assert_eq!(sel[0].spans[0].range, 0..3, "Wel");
    // A plain move drops the virtual range, so the page's selection is cleared.
    r.key(CoreCmd::BrowseCursorNextLine);
    assert!(cleared(&settle(&mut r)), "no stale page selection");
    // gorca+Shift+F10 switches to the virtual selection: the page is left alone.
    let fx = r.key(CoreCmd::BrowseNativeSelectionToggle);
    assert_eq!(spoken(&fx), ["virtual selection"]);
    r.key(CoreCmd::BrowseCursorTop);
    r.key(CoreCmd::BrowseSelectNextChar);
    assert!(selections(&settle(&mut r)).is_empty());
    // Back to the page selection: the document is probed first, and "page selection" is said
    // only when the probe succeeds (S-4).
    let fx = r.key(CoreCmd::BrowseNativeSelectionToggle);
    assert!(
        spoken(&fx).is_empty(),
        "nothing is claimed before the probe answers"
    );
    let req = last_select_req(&fx).expect("a probe");
    let fx = r
        .reader
        .handle(selection_done(req, SelOutcome::Done), r.now);
    assert_eq!(spoken(&fx), ["page selection"]);
}

fn last_select_req(fx: &ReaderEffects) -> Option<u32> {
    effects_web(fx).into_iter().rev().find_map(|w| match w {
        WebEffect::Select { req, .. } => Some(req),
        WebEffect::Ensure { .. } | WebEffect::Activity { .. } | WebEffect::Act { .. } => None,
    })
}

fn selection_done(req: u32, outcome: SelOutcome) -> ReaderMsg {
    ReaderMsg::Doc(DocNotice::SelectionDone {
        doc: DocId(1),
        req,
        outcome,
    })
}

#[test]
fn a_document_that_cannot_select_keeps_the_virtual_selection_and_says_so() {
    let mut r = tab_rig();
    r.focus(1002);
    settle(&mut r);
    r.key(CoreCmd::BrowseNativeSelectionToggle);
    let fx = r.key(CoreCmd::BrowseNativeSelectionToggle);
    let req = last_select_req(&fx).expect("a probe");
    let fx = r
        .reader
        .handle(selection_done(req, SelOutcome::NotSupported), r.now);
    assert_eq!(spoken(&fx), ["page selection not available here"]);
    // Still virtual: a selection writes nothing to the page.
    r.key(CoreCmd::BrowseCursorTop);
    r.key(CoreCmd::BrowseSelectNextChar);
    assert!(selections(&settle(&mut r)).is_empty());
}

#[test]
fn a_mirror_that_fails_is_known_and_a_stale_answer_is_ignored() {
    let mut r = tab_rig();
    r.focus(1002);
    settle(&mut r);
    r.key(CoreCmd::BrowseCursorTop);
    r.key(CoreCmd::BrowseSelectNextChar);
    let fx = settle(&mut r);
    let req = last_select_req(&fx).expect("a mirror request");
    // An answer for an older request changes nothing.
    let stale = r.reader.handle(
        selection_done(req.wrapping_sub(1), SelOutcome::Failed),
        r.now,
    );
    assert!(spoken(&stale).is_empty());
    let fx = r
        .reader
        .handle(selection_done(req, SelOutcome::Failed), r.now);
    assert_eq!(spoken(&fx), ["page selection not set"]);
}

#[test]
fn select_all_mirrors_the_whole_text_and_never_a_clipboard_write() {
    let mut r = tab_rig();
    r.focus(1002);
    settle(&mut r);
    r.key(CoreCmd::BrowseSelectAll);
    let sel = selections(&settle(&mut r));
    assert_eq!(sel.len(), 1);
    // The whole text is whole blocks at both ends: one range on the document, from the heading
    // through the last paragraph (an end that names a child includes it).
    let spans = &sel[0].spans;
    assert_eq!(spans.len(), 1, "{spans:?}");
    assert_eq!(spans[0].range, 0..4);
}

#[test]
fn shift_end_and_select_all_include_the_last_character_and_shift_left_takes_back_one() {
    let mut r = tab_rig();
    r.focus(1002);
    settle(&mut r);
    r.key(CoreCmd::BrowseCursorTop);
    // Shift+End selects the whole first line, "Welcome": the last character is inside.
    let fx = r.key(CoreCmd::BrowseSelectLineEnd);
    assert!(
        spoken(&fx).iter().any(|s| s.contains("Welcome")),
        "{:?}",
        spoken(&fx)
    );
    let sel = selections(&settle(&mut r));
    assert_eq!(sel[0].spans[0].range, 0..7, "Welcome");
    // Shift+Left takes back exactly the last character.
    r.key(CoreCmd::BrowseSelectPrevChar);
    let sel = selections(&settle(&mut r));
    assert_eq!(sel[0].spans[0].range, 0..6, "Welcom");
    // Ctrl+A, then Shift+Left: the whole text minus the last character.
    r.key(CoreCmd::BrowseSelectAll);
    settle(&mut r);
    let len = r
        .docs
        .get(DocId(1))
        .expect("document")
        .read
        .with(gorca_doc::web::WebDoc::text_len);
    let sel = |r: &Rig| r.reader.web.as_ref().and_then(|b| b.sel);
    assert_eq!(
        sel(&r),
        Some((0, len)),
        "the whole text, last character included"
    );
    r.key(CoreCmd::BrowseSelectPrevChar);
    assert_eq!(sel(&r), Some((0, len - 1)), "one character less");
}

/// The text a plan selects, read back from the mirror the way the browser would join it.
fn plan_of(
    r: &Rig,
    from_text: &str,
    from_local: u32,
    to_text: &str,
    to_local: u32,
) -> super::selmirror::Plan {
    r.docs.get(DocId(1)).expect("document").read.with(|d| {
        let find = |t: &str| {
            d.preorder()
                .find(|id| d.node(*id).is_some_and(|v| v.text == Some(t)))
                .expect(t)
        };
        let lo = d.offset_of(find(from_text)).expect("offset") + from_local;
        let hi = d.offset_of(find(to_text)).expect("offset") + to_local;
        super::selmirror::plan(d, lo, hi).expect("a plan")
    })
}

fn ranges(p: &super::selmirror::Plan) -> Vec<std::ops::Range<u32>> {
    p.spans.iter().map(|s| s.range.clone()).collect()
}

#[test]
fn a_range_inside_one_container_is_one_exact_span_across_a_link() {
    let mut r = tab_rig();
    r.focus(1002);
    settle(&mut r);
    // "ead the docs n" of "Read " + link("the docs") + " now.": hypertext "Read \u{fffc} now."
    let p = plan_of(&r, "Read ", 1, " now.", 3);
    assert_eq!(ranges(&p), vec![1..9]);
    assert_eq!(p.snap.range, 1..9);
}

#[test]
fn a_range_across_blocks_is_exact_legs_and_has_a_rounded_out_alternative() {
    let mut r = tab_rig();
    r.focus(1002);
    settle(&mut r);
    // From "lcome" of the heading to "Read" of the paragraph: tail, head.
    let p = plan_of(&r, "Welcome", 2, "Read ", 3);
    assert_eq!(ranges(&p), vec![2..7, 0..3]);
    // The alternative is one range on the document, whole children at the nested ends.
    assert_eq!(p.snap.range, 0..2);
    assert!(p.snap.end_on_child);
    // To "Last" of the last paragraph: the blocks between ride in one range.
    let p = plan_of(&r, "Welcome", 2, "Last line", 4);
    assert_eq!(ranges(&p), vec![2..7, 1..3, 0..4]);
}

#[test]
fn a_range_that_ends_at_a_link_says_so_and_a_space_before_it_can_be_left_out() {
    let mut r = tab_rig();
    r.focus(1002);
    settle(&mut r);
    // "Read " is followed by the link: the span is exact (0..5) and tells the source that a
    // child follows and that the last character is white space; the source decides.
    let p = plan_of(&r, "Read ", 0, "Read ", 5);
    assert_eq!(ranges(&p), vec![0..5]);
    // A caller that must not start in the first leaf's container plans again from its end.
    let at = r.docs.get(DocId(1)).expect("document").read.with(|d| {
        let id = d
            .preorder()
            .find(|id| d.node(*id).is_some_and(|v| v.text == Some("Read ")))
            .expect("run");
        d.offset_of(id).expect("offset")
    });
    assert_eq!(p.first_end, at + 5);
    assert!(p.spans[0].child_follows && p.spans[0].trim_ws && !p.spans[0].end_on_child);
}

#[test]
fn a_selection_that_starts_inside_a_link_is_cleared_not_mirrored() {
    let mut r = tab_rig();
    r.focus(1002);
    settle(&mut r);
    r.key(CoreCmd::QuicknavNextLink);
    settle(&mut r);
    r.key(CoreCmd::BrowseSelectNextChar);
    r.key(CoreCmd::BrowseSelectNextChar);
    let fx = settle(&mut r);
    assert!(
        selections(&fx).is_empty(),
        "no selection that would focus the link"
    );
}

// ---- selection mapping vectors (docs/reports/selection-mapping-math.md V2..V8, S-7) ------------

/// A document from nodes and parent -> children edges (ids as in `src`).
fn doc_of(nodes: Vec<SrcNode>, edges: &[(u64, &[u64])]) -> gorca_doc::web::DocRead {
    let (mut w, read) = channel(11);
    let mut tr = Translator::new();
    let mut deltas = vec![
        DocDelta::Reset {
            root: SrcNodeId(1),
            nav_id: 1,
        },
        DocDelta::Upsert(nodes),
    ];
    deltas.extend(edges.iter().map(|(p, ks)| kids(*p, ks)));
    deltas.push(DocDelta::Settled { batch: 1 });
    tr.apply(&mut w, deltas).unwrap();
    w.publish();
    read
}

fn root() -> SrcNode {
    block(src(1, Role::DocumentWeb, Some(1001)))
}

/// The flat offset `local` scalars into the first node whose text is `text`, or of the image.
fn flat_at(d: &gorca_doc::web::WebDoc, text: &str, local: u32) -> u32 {
    let id = d
        .preorder()
        .find(|id| {
            d.node(*id).is_some_and(|v| {
                if text.is_empty() {
                    v.role == Role::Image
                } else {
                    v.text == Some(text)
                }
            })
        })
        .expect(text);
    let lead = u32::from(d.node(id).is_some_and(|v| {
        v.flags.contains(gorca_doc::web::NodeFlags::BREAK_BEFORE)
            || v.flags.contains(gorca_doc::web::NodeFlags::CELL_SEP)
    }));
    d.offset_of(id).expect("offset") + lead + local
}

#[test]
fn v2_a_range_into_a_link_keeps_the_space_where_two_legs_meet() {
    // "See " + link("link text") + " end": the copy of "See li" must not become "Seeli".
    let read = doc_of(
        vec![
            root(),
            block(src(2, Role::Paragraph, Some(1002))),
            run(3, "See "),
            src(4, Role::Link, Some(1004)),
            run(5, "link text"),
            run(6, " end"),
        ],
        &[(1, &[2]), (2, &[3, 4, 6]), (4, &[5])],
    );
    let p = read.with(|d| {
        super::selmirror::plan(d, flat_at(d, "See ", 0), flat_at(d, "link text", 2)).expect("plan")
    });
    assert_eq!(p.spans.len(), 2, "{:?}", p.spans);
    assert_eq!(p.spans[0].range, 0..4);
    assert!(p.spans[0].child_follows);
    assert!(
        !p.spans[0].trim_ws,
        "the space before the link is real text here"
    );
    assert_eq!(p.spans[1].range, 0..2);
    assert!(
        p.gecko_lossy,
        "Gecko takes the whole link: the source reports it"
    );
}

#[test]
fn v3_a_range_into_a_nested_list_is_the_item_tail_and_the_nested_head_only() {
    let read = doc_of(
        vec![
            root(),
            src(2, Role::List, Some(1002)),
            src(3, Role::ListItem, Some(1003)),
            run(4, "Item"),
            src(5, Role::List, Some(1005)),
            src(6, Role::ListItem, Some(1006)),
            run(7, "nested"),
        ],
        &[(1, &[2]), (2, &[3]), (3, &[4, 5]), (5, &[6]), (6, &[7])],
    );
    let p = read.with(|d| {
        super::selmirror::plan(d, flat_at(d, "Item", 2), flat_at(d, "nested", 3)).expect("plan")
    });
    let r: Vec<_> = p.spans.iter().map(|s| s.range.clone()).collect();
    assert_eq!(r, vec![2..4, 0..3], "no whole sub-list, nothing twice");
    assert!(p.gecko_lossy);
}

#[test]
fn v6_a_range_that_ends_on_an_image_includes_it() {
    let read = doc_of(
        vec![
            root(),
            block(src(2, Role::Paragraph, Some(1002))),
            run(3, "Logo "),
            src(4, Role::Image, Some(1004)),
            block(src(5, Role::Paragraph, Some(1005))),
            run(6, "Next"),
        ],
        &[(1, &[2, 5]), (2, &[3, 4]), (5, &[6])],
    );
    let p = read.with(|d| {
        super::selmirror::plan(d, flat_at(d, "Logo ", 0), flat_at(d, "", 0) + 1).expect("plan")
    });
    assert_eq!(p.spans.len(), 1);
    assert_eq!(p.spans[0].range, 0..6, "five letters and the image");
    assert!(p.spans[0].end_on_object && !p.spans[0].end_on_child);
    // Ending after the image is exact for Gecko (it is not hypertext); at the very end of the
    // container the engine may refuse it, which the plan reports as lossy.
    assert_eq!(crate::browse::selmirror::tests_gecko_end(&p.spans[0]), 6);
    assert!(p.gecko_lossy);
}

#[test]
fn v7_a_range_of_only_an_image_is_planned() {
    let read = doc_of(
        vec![
            root(),
            block(src(2, Role::Paragraph, Some(1002))),
            run(3, "a "),
            src(4, Role::Image, Some(1004)),
            run(5, " b"),
        ],
        &[(1, &[2]), (2, &[3, 4, 5])],
    );
    let p = read.with(|d| {
        let at = flat_at(d, "", 0);
        super::selmirror::plan(d, at, at + 1).expect("plan")
    });
    assert_eq!(p.spans.len(), 1);
    assert_eq!(p.spans[0].range, 2..3);
}

#[test]
fn v8_blink_offsets_are_scalars_and_gecko_offsets_are_utf16() {
    let read = doc_of(
        vec![
            root(),
            block(src(2, Role::Paragraph, Some(1002))),
            run(3, "x\u{1F600}y alpha"),
        ],
        &[(1, &[2]), (2, &[3])],
    );
    let p = read.with(|d| {
        super::selmirror::plan(
            d,
            flat_at(d, "x\u{1F600}y alpha", 4),
            flat_at(d, "x\u{1F600}y alpha", 9),
        )
        .expect("plan")
    });
    assert_eq!(p.spans[0].range, 5..10, "UTF-16: the emoji takes two units");
    assert_eq!(p.spans[0].range_cp, 4..9, "scalars");
    assert_eq!(p.doc_range.start.1, 4);
    assert_eq!(p.doc_range.end.1, 9);
}

#[test]
fn r3_a_range_that_grows_into_a_link_does_not_start_a_leg_inside_the_link() {
    // "• " + link("First link") + " after": Shift+Right x4 from the start ends inside the link.
    let mut link = src(4, Role::Link, Some(1004));
    link.states |= StateSet::FOCUSABLE;
    let read = doc_of(
        vec![
            root(),
            block(src(2, Role::ListItem, Some(1002))),
            run(3, "\u{2022} "),
            link,
            run(5, "First link"),
            run(6, " after"),
        ],
        &[(1, &[2]), (2, &[3, 4, 6]), (4, &[5])],
    );
    let risky = |d: &gorca_doc::web::WebDoc, n: DocNodeId| {
        d.node(n)
            .is_some_and(|v| v.cats.intersects(gorca_doc::web::Cat::FOCUSABLE))
            && Some(n) != d.root()
    };
    let p = read.with(|d| {
        super::selmirror::plan_with(
            d,
            flat_at(d, "\u{2022} ", 0),
            flat_at(d, "First link", 3),
            &risky,
        )
        .expect("plan")
    });
    // The head leg inside the link would focus it: it is left out, the leg before it ends in
    // front of the link (the engine takes the link whole) and the plan says it is lossy.
    assert_eq!(p.spans.len(), 1, "{:?}", p.spans);
    assert_eq!(p.spans[0].range, 0..2);
    assert!(p.spans[0].child_follows && p.gecko_lossy);
}

#[test]
fn s7_text_of_a_password_field_is_never_mirrored_even_when_the_range_starts_inside_it() {
    let read = doc_of(
        vec![
            root(),
            block(src(2, Role::Paragraph, Some(1002))),
            run(3, "pre "),
            src(4, Role::PasswordEntry, Some(1004)),
            run(5, "secret"),
            run(6, " post"),
        ],
        &[(1, &[2]), (2, &[3, 4, 6]), (4, &[5])],
    );
    read.with(|d| {
        // Starting inside the field: nothing.
        assert!(
            super::selmirror::plan(d, flat_at(d, "secret", 1), flat_at(d, " post", 3)).is_none()
        );
        // Spanning it: the range stops before the field.
        let p =
            super::selmirror::plan(d, flat_at(d, "pre ", 0), flat_at(d, " post", 3)).expect("plan");
        assert_eq!(p.spans.last().expect("a span").range.end, 4);
    });
}

#[test]
fn a_selection_starting_in_a_link_is_not_mirrored_because_the_browser_would_focus_it() {
    let mut r = tab_rig();
    r.focus(1002);
    settle(&mut r);
    let link_container = r.docs.get(DocId(1)).expect("document").read.with(|d| {
        let run = d
            .preorder()
            .find(|id| d.node(*id).is_some_and(|v| v.text == Some("the docs")))
            .expect("run");
        let lo = d.offset_of(run).expect("offset");
        let p = super::selmirror::plan(d, lo, lo + 3).expect("a plan");
        (p.start_container, d.parent(run))
    });
    assert_eq!(Some(link_container.0), link_container.1);
}

// ---- review round 1: M-4 live speech expires and spends the owner's budget ---------------------

fn live_msg(text: &str, assertive: bool) -> ReaderMsg {
    ReaderMsg::Doc(DocNotice::Live {
        doc: DocId(1),
        msg: crate::doc_actor::live::LiveMsg {
            text: Arc::from(text),
            assertive,
        },
    })
}

fn speak_utts(fx: &ReaderEffects) -> Vec<gorca_model::Utterance> {
    fx.iter()
        .filter_map(|e| {
            if let ReaderEffect::Speak(s) = e {
                Some(s.utterance.clone())
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn live_speech_carries_the_adr_ttl_and_a_flood_spends_only_its_owners_budget() {
    let mut r = tab_rig();
    r.focus(1002);
    settle(&mut r);
    let fx = r.reader.handle(live_msg("polite news", false), r.now);
    let u = speak_utts(&fx);
    assert_eq!(u.len(), 1);
    assert_eq!(u[0].ttl_secs, 5);
    let fx = r.reader.handle(live_msg("alert news", true), r.now);
    assert_eq!(speak_utts(&fx)[0].ttl_secs, 10);
    // A flood in one instant: the owner's speech bucket (burst 3) lets a few through and holds
    // only the newest of the rest; nothing near the 32-entry speech FIFO is ever produced.
    let mut spoken_now = 0;
    for i in 0..200 {
        let fx = r
            .reader
            .handle(live_msg(&format!("flood {i}"), false), r.now);
        spoken_now += speak_utts(&fx).len();
    }
    assert!(
        spoken_now <= 4,
        "a flood spoke {spoken_now} messages at once"
    );
}

// ---- m8 re-review (round 2) regressions ------------------------------------------------------

fn tab_pair(g: &gorca_input_api::GrabSet) -> (bool, bool) {
    (
        g.contains(0xff09, Modifiers::empty()),
        g.contains(0xfe20, Modifiers::SHIFT),
    )
}

/// The Tab grabs of the newest pushed set, or the last one seen.
fn track(last: &mut Option<gorca_input_api::GrabSet>, fx: &ReaderEffects) {
    if let Some(g) = last_grabs(fx) {
        *last = Some(g);
    }
}

#[test]
fn r2_the_tab_grabs_follow_the_live_targets_after_a_foreign_grab_push() {
    let mut r = tab_rig();
    let mut last = None;
    track(&mut last, &r.focus(1002));
    track(&mut last, &r.key(CoreCmd::BrowseCursorNextLine));
    track(&mut last, &r.key(CoreCmd::BrowseCursorTop));
    track(&mut last, &settle(&mut r));
    assert_eq!(
        tab_pair(last.as_ref().unwrap()),
        (true, false),
        "at the heading"
    );
    track(&mut last, &r.focus(1014)); // focus mode on the edit
    track(&mut last, &r.key(CoreCmd::BrowseEscape)); // browse at the edit: pushed by the mode change
    track(&mut last, &settle(&mut r));
    r.now += Duration::from_secs(5);
    track(&mut last, &r.key(CoreCmd::BrowseCursorTop));
    track(&mut last, &settle(&mut r));
    let g = last.unwrap();
    assert_eq!(
        r.reader.web_tab_targets(),
        (true, false),
        "the live targets at the top"
    );
    assert_eq!(
        tab_pair(&g),
        (true, false),
        "the grabs follow them, not a stale cache"
    );
}

#[test]
fn r4_shift_tab_inside_a_focusable_container_finds_the_link_before_the_cursor() {
    let mut r = tab_rig();
    let mut p = block(src(4, Role::Paragraph, Some(1004)));
    p.states |= StateSet::FOCUSABLE;
    r.tr.apply(
        &mut r.writer,
        vec![DocDelta::Upsert(vec![p]), DocDelta::Settled { batch: 3 }],
    )
    .unwrap();
    r.writer.publish();
    r.focus(1002);
    settle(&mut r);
    r.key(CoreCmd::BrowseCursorNextLine);
    for _ in 0..16 {
        r.key(CoreCmd::BrowseCursorNextChar);
    }
    let link = r.join.get(nk(1006)).unwrap().1;
    let fx = r.key(CoreCmd::BrowseTabPrev);
    assert_eq!(acts(&fx), [(DocAction::Focus, link)], "{:?}", spoken(&fx));
}

#[test]
fn r5_tab_belongs_to_the_browser_after_a_focus_driven_move_and_inside_the_focused_node() {
    let mut r = tab_rig();
    r.focus(1002);
    settle(&mut r);
    assert!(r.reader.web_tab_targets().0, "from the heading Tab is ours");
    // The browser focuses the link (our Tab, or its own): the cursor came from a focus change.
    let fx = r.focus(1006);
    assert_eq!(r.reader.web_tab_targets(), (false, false));
    // The grab set follows at once, not at the next scroll timer.
    let g = last_grabs(&fx).expect("the focus move pushes the grab set");
    assert_eq!(tab_pair(&g), (false, false));
    let _ = settle(&mut r);
    // Any cursor move of our own ends it: the cursor is no longer on the focused node.
    r.key(CoreCmd::BrowseCursorNextLine);
    r.key(CoreCmd::BrowseCursorNextLine);
    let _ = settle(&mut r);
    assert_ne!(r.reader.web_tab_targets(), (false, false));
}

#[test]
fn r3_an_alert_waiting_for_the_budget_is_not_replaced_by_polite_news() {
    let mut r = tab_rig();
    r.focus(1002);
    settle(&mut r);
    let mut said = Vec::new();
    for m in ["news 1", "news 2", "news 3"] {
        said.extend(spoken(&r.reader.handle(live_msg(m, false), r.now)));
    }
    r.now += Duration::from_millis(20);
    said.extend(spoken(
        &r.reader
            .handle(live_msg("ALERT payment failed", true), r.now),
    ));
    r.now += Duration::from_millis(20);
    said.extend(spoken(&r.reader.handle(live_msg("news 4", false), r.now)));
    for _ in 0..20 {
        r.now += Duration::from_millis(200);
        said.extend(spoken(&r.reader.handle(ReaderMsg::Timer, r.now)));
    }
    assert!(
        said.iter().any(|s| s.contains("ALERT payment failed")),
        "the alert is spoken: {said:?}"
    );
    assert!(
        said.iter().any(|s| s.contains("news 4")),
        "polite coalescing keeps the newest: {said:?}"
    );
}

#[test]
fn r6_native_selection_toggle_outside_a_web_page_says_it_is_unavailable() {
    let mut r = rig();
    let fx = r.key(CoreCmd::BrowseNativeSelectionToggle);
    assert_eq!(
        spoken(&fx),
        ["native selection is only available on web pages"]
    );
}

// ---- degraded state: a web application that is not browsing resolves its focus at a key -----

#[test]
fn a_key_after_a_failed_document_lookup_asks_again_and_the_page_comes_back() {
    let mut r = rig();
    r.focus(1006);
    assert!(matches!(r.reader.mode(), Mode::Browse { .. }));
    // The popup closed, the focus is on a node no document knows yet, and the lookup failed:
    // the web layer is inactive and the keys are passed on (the key echo says "enter").
    r.focus(2000);
    r.reader.handle(
        ReaderMsg::Doc(DocNotice::NoDocument { focus: nk(2000) }),
        r.now,
    );
    assert_eq!(r.mode(), WebMode::Inactive);
    assert_eq!(r.reader.mode(), Mode::Focus);
    // One key is enough to ask again.
    let fx = type_a(&mut r);
    assert!(
        effects_web(&fx)
            .iter()
            .any(|a| matches!(a, WebEffect::Ensure { focus } if *focus == nk(2000))),
        "{fx:?}"
    );
    assert!(matches!(r.mode(), WebMode::Loading { .. }));
    assert_eq!(
        r.reader.web.as_ref().map(|b| b.counters.key_resyncs),
        Some(1)
    );
    // The document answers: browse mode is back.
    r.reader.handle(
        ReaderMsg::Doc(DocNotice::FirstUseful {
            doc: DocId(1),
            app: app(),
        }),
        r.now,
    );
    assert!(matches!(r.mode(), WebMode::Browse { .. }), "{:?}", r.mode());
}

#[test]
fn a_key_while_the_document_is_still_unknown_asks_for_it_again_at_most_twice_a_second() {
    let mut r = rig();
    r.focus(2000);
    assert!(matches!(r.mode(), WebMode::Loading { .. }));
    let asks = |fx: &ReaderEffects| {
        effects_web(fx)
            .iter()
            .filter(|a| matches!(a, WebEffect::Ensure { focus } if *focus == nk(2000)))
            .count()
    };
    let first = type_a(&mut r);
    assert_eq!(asks(&first), 1, "the lookup is asked again");
    let second = type_a(&mut r);
    assert_eq!(asks(&second), 0, "rate limited");
    r.now += Duration::from_millis(600);
    let third = type_a(&mut r);
    assert_eq!(asks(&third), 1);
}

#[test]
fn a_key_in_a_browsing_page_resolves_nothing() {
    let mut r = rig();
    r.focus(1006);
    type_a(&mut r);
    assert_eq!(
        r.reader.web.as_ref().map(|b| b.counters.key_resyncs),
        Some(0)
    );
}

// ---- tab switches: the page title leads when the focus enters another document ------------------

/// A second copy of the test page (local ids + 2000, titled `title`), registered as document 2.
fn add_second_doc(r: &mut Rig, title: &str) {
    let joins = add_second_doc_unjoined(r, title);
    join_second_doc(r, joins);
}

fn join_second_doc(r: &mut Rig, joins: Vec<(LocalId, DocNodeId)>) {
    for (l, id) in joins {
        r.join.set(
            NodeKey {
                app: app(),
                local: l,
            },
            (DocId(2), id),
        );
    }
}

/// The second document, published but not yet joined to the nodes of the focus events.
fn add_second_doc_unjoined(r: &mut Rig, title: &str) -> Vec<(LocalId, DocNodeId)> {
    let mut deltas = page();
    for d in &mut deltas {
        if let DocDelta::Upsert(nodes) = d {
            for n in nodes.iter_mut() {
                n.local = n.local.map(|l| LocalId(l.0 + 2000));
                if n.id == SrcNodeId(1) {
                    n.name = Some(Arc::from(title));
                }
            }
        }
    }
    let (mut writer, read) = channel(9);
    let mut tr = Translator::new();
    let out = tr.apply(&mut writer, deltas).unwrap();
    writer.publish();
    let joins = out.join_add;
    let (ctl_tx, _ctl) = mpsc::channel(16);
    let (ev_tx, _ev) = mpsc::channel(16);
    r.docs.insert(DocHandle {
        doc: DocId(2),
        app: app(),
        root: nk(3001),
        read,
        ctl: ctl_tx,
        events: ev_tx,
    });
    std::mem::forget(writer);
    r.mirror.apply(
        SnapshotBatch {
            nodes: vec![
                snap(3006, Role::Link, "the docs", StateSet::FOCUSABLE),
                snap(3002, Role::Heading, "Welcome", StateSet::empty()),
                snap(3001, Role::DocumentWeb, title, StateSet::FOCUSABLE),
            ],
            ..Default::default()
        },
        ApplyOrigin::Push,
    );
    joins
}

#[test]
fn a_tab_switch_onto_a_link_leads_with_the_new_page_title_once() {
    let mut r = rig();
    add_second_doc(&mut r, "Second page");
    r.focus(1006);
    let fx = r.focus(1002);
    assert_eq!(spoken(&fx), ["Welcome heading"], "same document: no title");
    // The browser focuses the link the other tab had focused (no event for the document).
    let fx = r.focus(3006);
    assert_eq!(spoken(&fx), ["Second page the docs link"]);
    let fx = r.focus(3002);
    assert_eq!(
        spoken(&fx),
        ["Welcome heading"],
        "the title leads only once"
    );
    // And back, with the chrome in between: the first page is announced again.
    r.focus(2000);
    r.reader.handle(
        ReaderMsg::Doc(DocNotice::NoDocument { focus: nk(2000) }),
        r.now,
    );
    let fx = r.focus(1006);
    assert_eq!(spoken(&fx), ["browse mode Test page the docs link"]);
}

#[test]
fn leaving_to_the_chrome_and_returning_to_the_same_page_does_not_repeat_its_title() {
    let mut r = rig();
    r.focus(1006);
    r.focus(2000);
    r.reader.handle(
        ReaderMsg::Doc(DocNotice::NoDocument { focus: nk(2000) }),
        r.now,
    );
    let fx = r.focus(1006);
    assert!(
        !spoken(&fx).iter().any(|s| s.contains("Test page")),
        "{:?}",
        spoken(&fx)
    );
}

#[test]
fn the_first_visit_of_a_tab_resolves_with_its_title_and_the_focused_node_in_one_piece() {
    let mut r = rig();
    r.focus(1006);
    let joins = add_second_doc_unjoined(&mut r, "Second page");
    // The browser focuses a link in a tab whose document we have not built yet.
    let fx = r.focus(3006);
    assert!(matches!(r.mode(), WebMode::Loading { .. }));
    assert!(!spoken(&fx).iter().any(|s| s.contains("Second page")));
    join_second_doc(&mut r, joins);
    let fx = r.reader.handle(
        ReaderMsg::Doc(DocNotice::FirstUseful {
            doc: DocId(2),
            app: app(),
        }),
        r.now,
    );
    assert!(matches!(r.mode(), WebMode::Browse { .. }));
    let said = spoken(&fx);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("Second page"), "{said:?}");
    assert!(said[0].contains("the docs link"), "{said:?}");
}

fn child_of(parent: u64, mut n: NodeSnapshot) -> Arc<NodeSnapshot> {
    n.parent = Some(nk(parent));
    Arc::new(n)
}

#[test]
fn a_focus_in_the_browsers_own_interface_neither_waits_for_a_document_nor_says_a_mode_word() {
    let mut r = rig();
    // Application > window > toolbar > editable combo box (the address bar), all in the cache.
    r.mirror.apply(
        SnapshotBatch {
            nodes: vec![
                (*snap(5000, Role::Application, "Firefox", StateSet::empty()))
                    .clone()
                    .into(),
                child_of(
                    5000,
                    (*snap(5001, Role::Window, "Firefox", StateSet::empty())).clone(),
                ),
                child_of(
                    5001,
                    (*snap(5002, Role::Toolbar, "", StateSet::empty())).clone(),
                ),
                child_of(
                    5002,
                    (*snap(
                        5003,
                        Role::ComboBox,
                        "Search or enter address",
                        StateSet::EDITABLE | StateSet::FOCUSABLE,
                    ))
                    .clone(),
                ),
            ],
            ..Default::default()
        },
        ApplyOrigin::Push,
    );
    r.focus(1006); // browsing a page first
    let fx = r.focus(5003);
    assert_eq!(r.mode(), WebMode::Inactive);
    assert!(
        !spoken(&fx)
            .iter()
            .any(|s| s.contains("focus mode") || s.contains("browse mode")),
        "{:?}",
        spoken(&fx)
    );
}

#[test]
fn focus_falling_to_the_document_keeps_the_cursor_and_enter_still_clicks_the_item() {
    let mut r = rig();
    r.focus(1006);
    let before = r.mode();
    assert!(matches!(before, WebMode::Browse { .. }));
    // The page re-rendered its list under the focused link: the browser parks the focus on the
    // document. Nothing is announced and the cursor does not go to the top of the page.
    let fx = r.focus(1001);
    assert!(spoken(&fx).is_empty(), "{:?}", spoken(&fx));
    assert_eq!(r.mode(), before);
    let fx = r.key(CoreCmd::BrowseActivate);
    let acts = effects_web(&fx);
    assert!(
        acts.iter().any(|a| matches!(
            a,
            WebEffect::Act {
                act: DocAction::Click,
                ..
            }
        )),
        "{acts:?}"
    );
}

#[test]
fn a_failed_click_is_said_never_silent() {
    let mut r = rig();
    r.focus(1006);
    let fx = r.reader.handle(
        ReaderMsg::Doc(DocNotice::ActionFailed {
            doc: DocId(1),
            gone: true,
        }),
        r.now,
    );
    assert_eq!(spoken(&fx), ["that item is gone"]);
    let fx = r.reader.handle(
        ReaderMsg::Doc(DocNotice::ActionFailed {
            doc: DocId(1),
            gone: false,
        }),
        r.now,
    );
    assert_eq!(spoken(&fx), ["can't activate that"]);
    // Another document's failure is not ours to speak.
    let fx = r.reader.handle(
        ReaderMsg::Doc(DocNotice::ActionFailed {
            doc: DocId(9),
            gone: true,
        }),
        r.now,
    );
    assert!(spoken(&fx).is_empty());
}

/// The page drops the paragraph (and the link) the cursor is in.
fn drop_cursor_paragraph(r: &mut Rig, rev: u64) {
    r.tr.apply(
        &mut r.writer,
        vec![
            kids(1, &[2, 9, 11, 13]),
            DocDelta::Removed(vec![
                SrcNodeId(4),
                SrcNodeId(5),
                SrcNodeId(6),
                SrcNodeId(7),
                SrcNodeId(8),
            ]),
            DocDelta::Settled { batch: rev },
        ],
    )
    .unwrap();
    r.writer.publish();
    r.reader.handle(
        ReaderMsg::Doc(DocNotice::Changed { doc: DocId(1), rev }),
        r.now,
    );
}

#[test]
fn a_document_focus_after_the_cursor_node_vanished_is_announced_as_before() {
    let mut r = rig();
    r.focus(1006);
    // A route change: the view holding the cursor is replaced, then the browser parks the focus
    // on the document. The cursor was moved by position restore, so this is a new place.
    drop_cursor_paragraph(&mut r, 2);
    let fx = r.focus(1001);
    assert!(!spoken(&fx).is_empty(), "the document is announced");
}

#[test]
fn enter_after_the_cursor_item_vanished_says_the_new_item_before_clicking() {
    let mut r = rig();
    r.focus(1006);
    drop_cursor_paragraph(&mut r, 2);
    let fx = r.key(CoreCmd::BrowseActivate);
    assert!(
        effects_web(&fx)
            .iter()
            .all(|a| !matches!(a, WebEffect::Act { .. })),
        "the first Enter only says where the cursor is"
    );
    assert!(!spoken(&fx).is_empty(), "the new item is spoken");
    // Moving on and activating again clicks as usual (the announcement is one-shot).
    let fx = r.key(CoreCmd::BrowseActivate);
    // The cursor now sits on plain text: nothing to click, and nothing is announced twice.
    assert!(spoken(&fx).is_empty(), "the announcement is one-shot");
}

fn click_of(fx: &ReaderEffects) -> bool {
    effects_web(fx).iter().any(|a| {
        matches!(
            a,
            WebEffect::Act {
                act: DocAction::Click,
                ..
            }
        )
    })
}

/// The page re-uses the link node for another item (new name and text).
fn relabel_link(r: &mut Rig, name: &str, rev: u64) {
    let mut link = src(6, Role::Link, Some(1006));
    link.states |= StateSet::FOCUSABLE;
    link.name = Some(Arc::from(name));
    r.tr.apply(
        &mut r.writer,
        vec![
            DocDelta::Upsert(vec![link, run(7, name)]),
            DocDelta::Settled { batch: rev },
        ],
    )
    .unwrap();
    r.writer.publish();
    r.reader.handle(
        ReaderMsg::Doc(DocNotice::Changed { doc: DocId(1), rev }),
        r.now,
    );
}

/// Turn the page's link node into another kind of control, before anything is clicked.
fn make_control(r: &mut Rig, role: Role, name: &str, rev: u64) {
    let mut n = src(6, role, Some(1006));
    n.states |= StateSet::FOCUSABLE;
    n.name = Some(Arc::from(name));
    r.tr.apply(
        &mut r.writer,
        vec![
            DocDelta::Upsert(vec![n, run(7, name)]),
            DocDelta::Settled { batch: rev },
        ],
    )
    .unwrap();
    r.writer.publish();
    r.reader.handle(
        ReaderMsg::Doc(DocNotice::Changed { doc: DocId(1), rev }),
        r.now,
    );
}

#[test]
fn a_toggle_relabelled_after_the_users_own_click_clicks_again() {
    let mut r = rig();
    make_control(&mut r, Role::Button, "Play", 3);
    r.focus(1006);
    assert!(click_of(&r.key(CoreCmd::BrowseActivate)));
    make_control(&mut r, Role::Button, "Pause", 4);
    let fx = r.key(CoreCmd::BrowseActivate);
    assert!(
        click_of(&fx),
        "second Enter on the user's own toggle clicks"
    );
}

#[test]
fn the_clicked_control_changing_role_is_displacement_and_costs_the_click() {
    let mut r = rig();
    make_control(&mut r, Role::Button, "Play", 3);
    r.focus(1006);
    assert!(click_of(&r.key(CoreCmd::BrowseActivate)));
    // Same node, but now a different kind of control: not the toggle the user pressed.
    make_control(&mut r, Role::Link, "Buy now", 4);
    let fx = r.key(CoreCmd::BrowseActivate);
    assert!(
        !click_of(&fx),
        "an unseen control is announced, not clicked"
    );
    assert!(!spoken(&fx).is_empty());
}

#[test]
fn a_link_relabel_after_a_click_is_displacement() {
    let mut r = rig();
    r.focus(1006);
    assert!(click_of(&r.key(CoreCmd::BrowseActivate)));
    relabel_link(&mut r, "Elsewhere", 3);
    let fx = r.key(CoreCmd::BrowseActivate);
    assert!(!click_of(&fx));
}

#[test]
fn a_different_node_swapped_in_within_the_window_is_displacement() {
    let mut r = rig();
    make_control(&mut r, Role::Button, "Play", 3);
    r.focus(1006);
    assert!(click_of(&r.key(CoreCmd::BrowseActivate)));
    // The page removes the control and the cursor settles on content that was never read.
    drop_cursor_paragraph(&mut r, 4);
    let fx = r.key(CoreCmd::BrowseActivate);
    assert!(!click_of(&fx));
    assert!(!spoken(&fx).is_empty(), "the new place is announced");
}

#[test]
fn a_late_relabel_of_the_clicked_toggle_is_displacement() {
    let mut r = rig();
    make_control(&mut r, Role::Button, "Play", 3);
    r.focus(1006);
    assert!(click_of(&r.key(CoreCmd::BrowseActivate)));
    r.now += super::CARET_FOLLOW + Duration::from_millis(50);
    make_control(&mut r, Role::Button, "Pause", 4);
    let fx = r.key(CoreCmd::BrowseActivate);
    assert!(
        !click_of(&fx),
        "outside the window the change is not the click's answer"
    );
}

#[test]
fn a_swap_after_a_focus_placement_is_announced_not_clicked() {
    let mut r = rig();
    r.focus(1006);
    relabel_link(&mut r, "Dave", 3);
    let fx = r.key(CoreCmd::BrowseActivate);
    assert!(!click_of(&fx), "the unseen item is not clicked");
    assert!(!spoken(&fx).is_empty(), "it is spoken instead");
    assert!(click_of(&r.key(CoreCmd::BrowseActivate)));
}

#[test]
fn a_displacement_is_spent_by_moving_the_cursor() {
    let mut r = rig();
    r.focus(1006);
    drop_cursor_paragraph(&mut r, 2);
    r.key(CoreCmd::BrowseCursorNextLine);
    r.key(CoreCmd::BrowseCursorPrevLine);
    let fx = r.key(CoreCmd::BrowseActivate);
    assert!(
        spoken(&fx).is_empty(),
        "Enter does not re-announce after the user moved"
    );
    assert!(r.reader.web.as_ref().is_some_and(|b| b.displaced.is_none()));
}
