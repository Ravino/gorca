// SPDX-License-Identifier: GPL-3.0-or-later
//! The focus and state-change utterances of a node, made by `gorca-present` from the node's fused
//! entry (ADR-0022 section 2.5, M22b1 cut-over): `compose` -> IR edits -> `render`.
//!
//! The Reader's input is a fused node. [`Reader::fused_entry`] gives the entry of any snapshot:
//! the document's entry for a web node, the attached entry otherwise, and, for a snapshot that
//! only carries its source record, the provisional entry made from that record (PV-1). There is no
//! branch that presents from legacy snapshot fields.
//!
//! Edits that are not part of the fused model yet are applied as IR edits in one fixed order
//! (`gorca_present::edit`): the switcher's "selected" word, the contents of a field, the
//! application policy's verdict. The window lead-in and the browse mode word still arrive as
//! tokens from their own modules and are added after render (their surfaces, b4 and b2, move them
//! into `compose`).

use std::sync::Arc;

use gorca_model::sem::SemEntry;
use gorca_model::{Epoch, Freshness, NodeKey, NodeSnapshot, Prio, Segment, StateSet, UttId};
use gorca_present::{
    ComposeCtx, IrEdit, PresIr, Reason, RenderOpts, ReportFlags, apply_edits, compose,
    compose_change, fields_from_tokens, render,
};
use smallvec::SmallVec;

use super::PresentContext;
use crate::reader::Reader;

/// `Origin::via` of fields adapted from an extension's presentation.
const VIA_EXT: u32 = 1;
/// `Origin::via` of fields written by the application policy.
const VIA_POLICY: u32 = 2;

/// The extension catalogs `render` resolves add-on messages through: none are needed by a focus
/// utterance (an extension's words arrive as tokens), so the empty snapshot is shared.
fn no_ext() -> &'static gorca_i18n::ext::ExtCatalogs {
    static EMPTY: std::sync::OnceLock<gorca_i18n::ext::ExtCatalogs> = std::sync::OnceLock::new();
    EMPTY.get_or_init(gorca_i18n::ext::ExtCatalogs::empty)
}

/// What the focus presentation of a node came to.
pub(crate) struct Built {
    pub utterance: gorca_model::Utterance,
    /// The application policy says nothing for this node.
    pub silent: bool,
}

/// What the application policy did to a focus utterance.
enum PolicyEdits {
    Default,
    Silence,
    Edits(Vec<IrEdit>),
}

impl Reader {
    /// The document mirror's entry for a web node (fused by the document actor before the batch is
    /// published), `None` for a node that is not in a mirrored document.
    fn web_entry(&self, key: NodeKey) -> Option<Arc<SemEntry>> {
        let b = self.web.as_ref()?;
        let (doc, id) = b.join.get(key)?;
        let h = b.docs.get(doc)?;
        h.read.with(|d| d.node(id)?.sem())
    }

    /// Whether `key` is a node of a mirrored web document (the web rows of `compose` apply).
    pub(crate) fn node_is_web(&self, key: NodeKey) -> bool {
        self.web.as_ref().is_some_and(|b| b.join.get(key).is_some())
    }

    /// Whether `snap` is web content: a node of a mirrored document, or one whose source record
    /// carries a DOM tag (the engine exports it for web nodes only).
    pub(crate) fn snap_is_web(&self, snap: &NodeSnapshot) -> bool {
        self.node_is_web(snap.key) || snap.sem.rec.as_ref().is_some_and(|r| !r.tag.is_empty())
    }

    /// The fused entry of `snap` as it is spoken (PV-1, no `Option`): the document's entry for a
    /// web node, the attached one, the provisional entry of its source record, and for a node no
    /// connector described, an entry that has only its name.
    pub(crate) fn fused_entry(&self, snap: &NodeSnapshot) -> Arc<SemEntry> {
        if let Some(e) = &snap.sem.entry {
            return Arc::clone(e);
        }
        if let Some(e) = self.web_entry(snap.key) {
            return e;
        }
        if let Some(r) = &snap.sem.rec {
            return Arc::new(r.provisional_entry());
        }
        Arc::new(SemEntry {
            name: snap.name.clone(),
            ..SemEntry::default()
        })
    }

    /// The snapshot with its fused entry attached (a clone only when the entry changes), so that
    /// everything downstream of a presentation reads one thing.
    pub(crate) fn with_fused_entry(&self, snap: &Arc<NodeSnapshot>) -> Arc<NodeSnapshot> {
        // A web node is spoken with the document's entry (fused there, with the document's
        // quirks and its content names), whatever entry the application mirror attached.
        let e = self
            .web_entry(snap.key)
            .unwrap_or_else(|| self.fused_entry(snap));
        if snap
            .sem
            .entry
            .as_ref()
            .is_some_and(|cur| Arc::ptr_eq(cur, &e))
        {
            return Arc::clone(snap);
        }
        let mut c = (**snap).clone();
        c.sem.entry = Some(e);
        Arc::new(c)
    }

    fn compose_ctx(&self, key: NodeKey, may_refine: bool, skip_selected: bool) -> ComposeCtx {
        ComposeCtx {
            flags: ReportFlags::default(),
            may_refine,
            web: self.node_is_web(key),
            skip_selected,
            hedge_inferred: self.cmd.effective.browse.hedge_inferred,
            lang: gorca_i18n::language(),
        }
    }

    fn render_opts(&self) -> RenderOpts<'static> {
        RenderOpts {
            description_pause: self.cmd.effective.browse.description_pause,
            lang: gorca_i18n::language(),
            ext: no_ext(),
        }
    }

    /// What the application policy (ADR-0008 W3) says to a focus utterance of `snap`, as edits.
    fn policy_edits(&self, snap: &NodeSnapshot) -> PolicyEdits {
        use crate::builtin_ext::{PolicyFacts, PolicyMoment, PolicyVerdict};
        if !self.policy_active(snap.key.app) {
            return PolicyEdits::Default;
        }
        let range = match &snap.value {
            Some(gorca_model::Value::Range { current, min, max }) => Some((*current, *min, *max)),
            Some(gorca_model::Value::Text(_)) | None => None,
        };
        let facts = PolicyFacts {
            moment: PolicyMoment::Focus,
            role: snap.role,
            states: snap.states,
            name: snap.name.as_deref(),
            description: snap.description.as_deref(),
            range,
        };
        match self.decide_policy(snap.key.app, &facts) {
            PolicyVerdict::Default | PolicyVerdict::Notice => PolicyEdits::Default,
            PolicyVerdict::Silence => PolicyEdits::Silence,
            PolicyVerdict::Value(text) => {
                PolicyEdits::Edits(vec![IrEdit::SetValue(Arc::from(text))])
            }
            PolicyVerdict::Say(segs) => PolicyEdits::Edits(vec![IrEdit::ReplaceAll(
                fields_from_tokens(segs.iter().map(|s| (s.kind, &*s.text)), VIA_POLICY),
            )]),
        }
    }

    /// The focus utterance of `snap` in `ctx`: the extension's words if its application has any,
    /// else `compose` over the fused entry; then the edits; then `render`. `snap` has its entry
    /// attached ([`Self::with_fused_entry`]). `policy` asks the application policy (only the
    /// first presentation of a focus does).
    pub(crate) fn build_focus(
        &self,
        ctx: PresentContext,
        snap: &NodeSnapshot,
        id: UttId,
        epoch: Epoch,
        policy: bool,
    ) -> Built {
        let may_refine = !matches!(snap.fresh, Freshness::Fresh { .. });
        let mut ir: PresIr = if let Some(segs) = self.presenters.ext_segments(ctx, snap, epoch) {
            PresIr::new(
                Reason::Focus,
                snap.key,
                0,
                fields_from_tokens(segs.iter().map(|s| (s.kind, &*s.text)), VIA_EXT)
                    .into_iter()
                    .collect(),
            )
        } else {
            let entry = self.fused_entry(snap);
            let cctx = self.compose_ctx(snap.key, may_refine, false);
            let mut ir = compose(&entry, snap.key, Reason::Focus, cctx).unwrap_or_else(|_| {
                PresIr::new(Reason::Focus, snap.key, entry.sem_rev, SmallVec::new())
            });
            // The contents of a field are text-layer data: the entry holds no value for them.
            if let Some(gorca_model::Value::Text(t)) = &snap.value
                && !t.is_empty()
                && !matches!(entry.sem.role, gorca_model::sem::SemRole::PasswordBox)
                && snap.role != gorca_model::Role::PasswordEntry
            {
                apply_edits(&mut ir, &[IrEdit::SetValue(Arc::clone(t))]);
            }
            ir
        };
        let mut edits: Vec<IrEdit> = Vec::new();
        if ctx == PresentContext::SelectedItem {
            // Being selected is the reason a switcher entry is spoken at all.
            edits.push(IrEdit::DropMsg(gorca_i18n::MsgId::StateSelected));
        }
        let mut silent = false;
        if policy && ctx == PresentContext::Focus {
            match self.policy_edits(snap) {
                PolicyEdits::Default => {}
                PolicyEdits::Silence => silent = true,
                PolicyEdits::Edits(mut e) => edits.append(&mut e),
            }
        }
        apply_edits(&mut ir, &edits);
        ir.may_refine = may_refine;
        Built {
            utterance: self.utterance_of(&ir, id, epoch, Prio::Interrupt, None),
            silent,
        }
    }

    /// The words of a web menu spoken before its item: the menu's name and role, composed from
    /// an entry that has only those (no states, value or description), so the lead goes through
    /// the same `compose` and `render` as everything else.
    pub(crate) fn menu_lead_segments(
        &self,
        key: NodeKey,
        role: gorca_model::Role,
        name: Arc<str>,
        id: UttId,
        epoch: Epoch,
    ) -> Vec<Segment> {
        use gorca_model::sem::SemRole;
        let mut e = SemEntry {
            name: Some(name),
            ..SemEntry::default()
        };
        e.sem.role = if role == gorca_model::Role::MenuBar {
            SemRole::Menubar
        } else {
            SemRole::Menu
        };
        let cctx = self.compose_ctx(key, false, false);
        let Ok(ir) = compose(&e, key, Reason::Focus, cctx) else {
            return Vec::new();
        };
        self.utterance_of(&ir, id, epoch, Prio::Interrupt, None)
            .segments
            .into_vec()
    }

    /// Render `ir` into the utterance the speech lane takes.
    fn utterance_of(
        &self,
        ir: &PresIr,
        id: UttId,
        epoch: Epoch,
        prio: Prio,
        refines: Option<UttId>,
    ) -> gorca_model::Utterance {
        let segments: SmallVec<[Segment; 4]> = render(ir, self.render_opts())
            .iter()
            .map(|t| Segment::new(Arc::from(t.text.as_str()), t.kind))
            .collect();
        gorca_model::Utterance {
            id,
            segments,
            lang: None,
            epoch,
            prio,
            may_refine: ir.may_refine,
            survives_focus_change: false,
            ttl_secs: 0,
            refines,
        }
    }

    /// `new` (a fresh read of the node: a state-change fetch, a revalidation) with the entry it
    /// would have after the change. A read only carries the observed record; the entry is the one
    /// that was spoken (or the document's, for a web node) with the observed *state* groups laid
    /// over it, so a derived name or a refined role never shows up as a change of its own.
    pub(crate) fn with_entry_after(
        &self,
        old: &NodeSnapshot,
        new: &Arc<NodeSnapshot>,
    ) -> Arc<NodeSnapshot> {
        let Some(rec) = &new.sem.rec else {
            return self.with_fused_entry(new);
        };
        let mut e = (*self
            .web_entry(new.key)
            .unwrap_or_else(|| self.fused_entry(old)))
        .clone();
        overlay_observed(&mut e, rec);
        let mut c = (**new).clone();
        c.sem.entry = Some(Arc::new(e));
        Arc::new(c)
    }

    /// The change utterance of the focused node: what differs between the entry that was spoken
    /// and the one now. `None` when nothing speakable changed.
    pub(crate) fn build_change(
        &self,
        old: &NodeSnapshot,
        new: &NodeSnapshot,
        ctx: PresentContext,
        epoch: Epoch,
        refines: Option<UttId>,
    ) -> Option<gorca_model::Utterance> {
        let skip = super::flow::skip_states(ctx).contains(StateSet::SELECTED);
        let cctx = self.compose_ctx(new.key, false, skip);
        let ir = compose_change(
            &self.fused_entry(old),
            &self.fused_entry(new),
            new.key,
            cctx,
        );
        if ir.fields.is_empty() {
            return None;
        }
        // The caller numbers the utterance when it speaks it.
        Some(self.utterance_of(&ir, UttId(0), epoch, Prio::Message, refines))
    }
}

/// The groups a state-change read observes, laid over `e`: the check, press, expand, select,
/// availability and invalid states, the value text, and the engine's own name when it gave one.
fn overlay_observed(e: &mut SemEntry, rec: &gorca_model::sem::SrcRecord) {
    use gorca_model::sem::{Group, SemFlags};
    let k = &rec.known;
    let (o, n) = (&mut e.sem, &rec.sem);
    if k.has(Group::Check) {
        o.check = n.check;
    }
    if k.has(Group::Press) {
        o.press = n.press;
    }
    if k.has(Group::Expand) {
        o.expand = n.expand;
    }
    if k.has(Group::Select) {
        o.select = n.select;
    }
    if k.has(Group::Avail) {
        o.avail = n.avail;
    }
    if k.has(Group::Invalid) {
        o.invalid = n.invalid;
    }
    if k.has(Group::Flags) {
        let bits = SemFlags::REQUIRED | SemFlags::BUSY | SemFlags::READ_ONLY;
        o.flags = (o.flags & !bits) | (n.flags & bits);
    }
    if k.has(Group::ValueText)
        && let Some(v) = n.text.as_deref().and_then(|t| t.value_text.clone())
    {
        let mut t = o.text.as_deref().cloned().unwrap_or_default();
        t.value_text = Some(v);
        o.text = Some(Arc::new(t));
    }
    // A name an add-on patch or the user set is not the engine's to take back: the engine's own
    // name is the observed one that the patch deliberately replaced (ADR-0026 section 2.6, S1
    // beats S0). A genuine change of the observed name re-fuses the node in its document, so the
    // entry already follows it where the patch no longer applies.
    let owned = matches!(
        e.prov.get(Group::Name),
        Some(gorca_model::sem::ProvCode::Patch | gorca_model::sem::ProvCode::User)
    );
    if !owned
        && let Some(name) = rec
            .name
            .as_ref()
            .filter(|n| !gorca_present::name_is_blank(Some(n)))
    {
        e.name = Some(Arc::clone(name));
    }
}

#[cfg(test)]
mod overlay_tests {
    use super::*;
    use gorca_model::sem::{Group, ProvCode, SrcRecord};

    fn rec(name: &str) -> SrcRecord {
        let mut r = SrcRecord::default();
        r.name = Some(Arc::from(name));
        r.known.set(Group::Name, true);
        r
    }

    #[test]
    fn the_observed_name_replaces_an_observed_or_derived_name() {
        let mut e = SemEntry::default();
        e.name = Some(Arc::from("old"));
        e.prov.set(Group::Name, ProvCode::ObservedPrimary);
        overlay_observed(&mut e, &rec("new"));
        assert_eq!(e.name.as_deref(), Some("new"));
    }

    #[test]
    fn a_patched_or_user_name_survives_the_overlay() {
        for p in [ProvCode::Patch, ProvCode::User] {
            let mut e = SemEntry::default();
            e.name = Some(Arc::from("Alice, 1 unread"));
            e.prov.set(Group::Name, p);
            overlay_observed(&mut e, &rec("AL Alice 15:00"));
            assert_eq!(e.name.as_deref(), Some("Alice, 1 unread"), "{p:?}");
        }
    }
}

/// The name the node is spoken with: its entry's (None when it has none).
pub(crate) fn spoken_name(snap: &NodeSnapshot) -> Option<&str> {
    snap.sem.entry.as_ref().and_then(|e| e.name.as_deref())
}
