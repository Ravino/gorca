#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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
use crate::doc_actor::{DocCtl, DocHandle, DocNotice};
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
    assert_eq!(spoken(&fx), ["browse mode the docs link"]);
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
    assert_eq!(spoken(&fx), ["focus mode Name edit"]);
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

/// Say-all and say-selection belong to the native text layer; in browse mode the document owns
/// reading, so they refuse with a short cue and start nothing.
#[test]
fn say_all_and_say_selection_refuse_cleanly_in_browse_mode() {
    let mut r = rig();
    r.focus(1006);
    assert!(matches!(r.reader.mode(), Mode::Browse { .. }));
    for cmd in [CoreCmd::TextSayAll, CoreCmd::InfoSelection] {
        let fx = r.key(cmd);
        assert_eq!(spoken(&fx), ["not available in this mode"], "{cmd:?}");
        assert!(
            !fx.iter().any(|e| matches!(
                e,
                ReaderEffect::SayAllChunk { .. } | ReaderEffect::TextFetch { .. }
            )),
            "{cmd:?} started text work"
        );
    }
}

/// A Firefox field (text offsets in UTF-16, so the native caret layer stays out) in focus mode:
/// a typed letter is echoed once, by the generic key echo.
#[test]
fn typing_in_a_web_field_in_focus_mode_is_echoed_once() {
    let mut r = rig();
    r.focus(1014);
    assert!(matches!(r.mode(), WebMode::Focus { .. }));
    r.now += Duration::from_millis(50);
    let ev = KeyEvent {
        keycode: 38,
        keysym: 0x61,
        unichar: 'a' as u32,
        modifiers: Modifiers::empty(),
        pressed: true,
    };
    let fx = r.reader.handle(
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
    );
    assert_eq!(spoken(&fx), ["a"]);
}
