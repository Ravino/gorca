// SPDX-License-Identifier: GPL-3.0-or-later
//! The source side of the one AT-SPI normaliser (M22a-C2, ADR-0024 H-7): every node this source
//! delivers carries a [`SrcRecord`] built by `gorca-sem-atspi`, so the document actor (which may
//! not depend on the normaliser) can fuse it without ever seeing a raw role number.
//!
//! Also builds the [`FuseSetup`] of an application: the engine's trust table and quirk rows,
//! chosen by the selector data in `gorca-sem-atspi` (never by a name test here).

use std::sync::Arc;

use gorca_doc_source_api::{FuseSetup, web_attrs};
use gorca_fuse::{
    AppFacts, PrimarySlot, Profile, ProfileSelector, QuirkSet, Rules, parse_inexpressible, parse_pi,
};
use gorca_model::{AppInfo, AttrKey, PropMask, WebEngine, sem::SrcRecord};
use gorca_sem_atspi::tables::{self, Bundle};
use gorca_sem_atspi::{RawNode, StateBits, normalise};
use smallvec::SmallVec;

use crate::wire::AttrInterner;

/// Names of the dynamically interned attribute keys, in key order: one snapshot per fetch turns
/// a key back into its toolkit name without taking the interner's lock per attribute.
#[derive(Debug)]
pub struct KeyNames(Vec<Box<str>>);

impl KeyNames {
    /// Snapshot the interner.
    pub fn of(interner: &AttrInterner) -> KeyNames {
        KeyNames(interner.dynamic_names())
    }

    /// The toolkit name of `key`, `None` for an unknown dynamic key and for the keys that two
    /// toolkit names share (those come with their exact names, see `RawNode::aliased`).
    pub fn name(&self, key: AttrKey) -> Option<&str> {
        if matches!(
            key,
            web_attrs::ATOMIC | web_attrs::RELEVANT | web_attrs::BUSY
        ) {
            return None;
        }
        if let Some(n) = web_attrs::name_of(key) {
            return Some(n);
        }
        let ix = usize::from(key.0.checked_sub(web_attrs::FIRST_DYNAMIC)?);
        self.0.get(ix).map(AsRef::as_ref)
    }
}

/// Normalise one node read by name: `attrs` are toolkit names with values (the synthetic
/// `description` pair carries the accessible description). `state` is `None` when the state was
/// not fetched (the state groups are then Unknown, not Absent).
pub fn record(
    raw_role: u32,
    state: Option<u64>,
    name: Option<&str>,
    attrs: &[(&str, &str)],
) -> Arc<SrcRecord> {
    let description = attrs
        .iter()
        .find(|(k, _)| *k == "description")
        .map(|(_, v)| *v);
    let mut fetched = PropMask::ROLE | PropMask::ATTRS | PropMask::NAME | PropMask::DESCRIPTION;
    if state.is_some() {
        fetched |= PropMask::STATES;
    }
    Arc::new(normalise(&RawNode {
        fetched,
        role: raw_role,
        states: StateBits(state.unwrap_or(0)),
        attrs,
        name,
        description,
    }))
}

/// Normalise a node whose attributes are interned keys (the `GetTree` path); `aliased` carries
/// the exact names of the attributes whose keys are shared.
pub fn record_interned(
    names: &KeyNames,
    raw_role: u32,
    state: Option<u64>,
    name: Option<&str>,
    attrs: &[(AttrKey, Arc<str>)],
    aliased: &[(&'static str, Arc<str>)],
) -> Arc<SrcRecord> {
    let mut pairs: SmallVec<[(&str, &str); 16]> = SmallVec::new();
    for (k, v) in attrs {
        if let Some(n) = names.name(*k) {
            pairs.push((n, v));
        }
    }
    for (n, v) in aliased {
        pairs.push((n, v));
    }
    record(raw_role, state, name, &pairs)
}

/// The fusion tables of the profile that fits `info`. A broken shipped table is a bug caught by
/// the `gorca-sem-atspi` tests; here it only turns fusion off (`None`) and is logged.
pub fn fuse_setup(info: &AppInfo) -> Option<FuseSetup> {
    let engine = match info.web_engine {
        WebEngine::Gecko => Some("gecko"),
        WebEngine::Blink => Some("blink"),
        WebEngine::None => None,
    };
    let facts = AppFacts {
        app_name: &info.app_name,
        toolkit: Some(&*info.toolkit).filter(|t| !t.is_empty()),
        web_engine: engine,
    };
    let selector = match ProfileSelector::parse(tables::SELECTOR) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "source-profile selector does not parse; fusion is off");
            return None;
        }
    };
    let name = selector.select(&facts).unwrap_or(tables::DEFAULT_PROFILE);
    let bundle = tables::bundle(name).or_else(|| tables::bundle(tables::DEFAULT_PROFILE))?;
    match build(bundle) {
        Ok((profile, quirks)) => Some(FuseSetup {
            name: Arc::from(bundle.name),
            profile,
            quirks,
        }),
        Err(e) => {
            tracing::warn!(profile = bundle.name, error = %e, "fusion tables do not load; fusion is off");
            None
        }
    }
}

fn build(b: &Bundle) -> Result<(Profile, QuirkSet), gorca_fuse::QuirkError> {
    let quirks = QuirkSet::parse(b.quirks)?;
    let pi = parse_pi(b.pi)?;
    let inexpressible = parse_inexpressible(b.inexpressible)?;
    quirks.validate_against(inexpressible)?;
    let mut profile = Profile::new(&[PrimarySlot::P0, PrimarySlot::P1], Rules::aria());
    pi.apply(&mut profile);
    profile.inexpressible = inexpressible;
    Ok((profile, quirks))
}

#[cfg(test)]
mod tests {
    use gorca_model::ConnectorKind;
    use gorca_model::sem::{Group, SemRole};

    use super::*;

    fn info(toolkit: &str, name: &str, engine: WebEngine) -> AppInfo {
        AppInfo {
            toolkit: Arc::from(toolkit),
            app_name: Arc::from(name),
            pid: None,
            connector_kind: ConnectorKind::Atspi,
            verified_shell: false,
            dump_complete: false,
            text_unit: gorca_model::TextUnit::Scalar,
            text_insert_slack: 0,
            edit_quirks: gorca_model::EditQuirks::default(),
            page_events_settle_ms: 0,
            web_engine: engine,
            root: None,
            sem_tables: None,
        }
    }

    #[test]
    fn the_selector_picks_the_engine_tables_from_data() {
        let g = fuse_setup(&info("Gecko", "Firefox", WebEngine::Gecko)).unwrap();
        let b = fuse_setup(&info("Chromium", "Google Chrome", WebEngine::Blink)).unwrap();
        let n = fuse_setup(&info("GTK", "Settings", WebEngine::None)).unwrap();
        assert_eq!(&*g.name, "gecko");
        assert_eq!(&*b.name, "blink");
        assert_eq!(&*n.name, "gtk4");
        assert!(!g.quirks.is_empty(), "Gecko ships the list-marker row");
        assert_eq!(
            b.quirks.len(),
            1,
            "Blink ships only the contenteditable-host row"
        );
    }

    #[test]
    fn a_node_read_by_name_is_normalised_with_unknown_vs_absent() {
        let attrs = [
            ("tag", "h2"),
            ("level", "2"),
            ("description", "Opens a menu"),
        ];
        let r = record(83, Some(0), Some("Title"), &attrs);
        assert_eq!(r.sem.role, SemRole::Heading);
        assert_eq!(r.sem.level, 2);
        assert_eq!(r.sem.description(), Some("Opens a menu"));
        assert_eq!(r.name.as_deref(), Some("Title"));
        assert!(r.known.has(Group::Check), "states were fetched");
        // No state fetched (the Stateful policy skips plain roles): the state groups are Unknown.
        let r = record(83, None, Some("Title"), &attrs);
        assert!(!r.known.has(Group::Check) && !r.known.has(Group::Expand));
        assert!(r.known.has(Group::Role) && r.known.has(Group::Level));
    }

    #[test]
    fn interned_keys_map_back_to_toolkit_names() {
        let interner = AttrInterner::default();
        let custom = interner.key("placeholder-text");
        let names = KeyNames::of(&interner);
        assert_eq!(names.name(web_attrs::TAG), Some("tag"));
        assert_eq!(names.name(custom), Some("placeholder-text"));
        assert_eq!(names.name(AttrKey(9999)), None);
        let attrs = [
            (web_attrs::TAG, Arc::<str>::from("input")),
            (custom, Arc::<str>::from("Search")),
        ];
        let aliased = [("container-atomic", Arc::<str>::from("true"))];
        let r = record_interned(&names, 79, Some(0), None, &attrs, &aliased);
        assert_eq!(r.tag.as_str(), "input");
        assert!(
            r.sem
                .text
                .as_deref()
                .is_some_and(|t| t.placeholder.is_some())
        );
    }
}
