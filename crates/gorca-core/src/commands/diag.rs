// SPDX-License-Identifier: GPL-3.0-or-later
//! `explain` diagnostics (M22a-C4, ADR-0022 section 2.5, ADR-0024 FS-9): where the words of the
//! last utterance came from.
//!
//! The report is a pure function of the utterance, the fused entry of the node it was about and
//! (when an engine trace is at hand) the trail of the engine: per segment its kind and, with the
//! `seg-origin` feature, the catalogue message and the producer; per semantic group the stratum
//! that won (`ProvCode`), the hedge class, the taint tier, the fallback/stale bits and the
//! quirk rule that rewrote an observed value. It runs on a command, never on the key path.
//!
//! # Privacy
//!
//! Page text can be a private message or a form value, and a log is not the place for it: the
//! default report carries lengths and kinds only. The text of segments is added only when the
//! process opted in ([`set_explain_text`], the daemon's `GORCA_EXPLAIN_TEXT=1`); library code
//! never reads the environment. What the user hears about the result goes through the string
//! catalogue; the detailed dump is a developer log line (English, like `debug.node_info`).

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};

use gorca_fuse::{Explain, TrailEntry};
use gorca_model::fact::SourceId;
use gorca_model::sem::{ConfClass, Group, ProvCode, SemEntry, SourceClass, TrustTier};
use gorca_model::{SegmentKind, Utterance};

/// Whether the explain log may carry the text of segments. Off by default.
static EXPLAIN_TEXT: AtomicBool = AtomicBool::new(false);

/// Opt in to segment text in the explain log (default off: it can be private).
pub fn set_explain_text(on: bool) {
    EXPLAIN_TEXT.store(on, Ordering::Relaxed);
}

/// The longest stretch of text one explained segment shows, with the opt-in.
const TEXT_CUT: usize = 80;

/// The `SourceId` of the AT-SPI connector: the primary source of every node (ADR-0024 2.2).
pub const ATSPI_SOURCE: SourceId = SourceId {
    slot: 0,
    generation: 0,
};

/// One registered source, for naming in explain output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceEntry {
    pub id: SourceId,
    /// The stable name: also what ranks ties, so it is unique.
    pub name: &'static str,
    pub class: SourceClass,
    pub tier: TrustTier,
}

/// The sources the daemon registered, by id. Wired once at start (`gorca-daemon`
/// `wire_sources`); the Reader only reads it.
#[derive(Clone, Debug, Default)]
pub struct SourceDirectory {
    sources: Vec<SourceEntry>,
}

impl SourceDirectory {
    /// An empty directory: every source is named by its id.
    pub fn new() -> SourceDirectory {
        SourceDirectory::default()
    }

    /// Register a source; a second one with the same id or name is refused (names order ties).
    pub fn register(&mut self, e: SourceEntry) -> Result<(), SourceEntry> {
        if self
            .sources
            .iter()
            .any(|s| s.id == e.id || s.name == e.name)
        {
            return Err(e);
        }
        self.sources.push(e);
        Ok(())
    }

    /// The AT-SPI connector as the Primary source `SourceId{0,0}`, trust tier T1.
    pub fn with_atspi_primary() -> SourceDirectory {
        let mut d = SourceDirectory::new();
        // The directory is empty, so the first registration cannot collide.
        let _ = d.register(SourceEntry {
            id: ATSPI_SOURCE,
            name: "atspi",
            class: SourceClass::Primary,
            tier: TrustTier::T1Platform,
        });
        d
    }

    pub fn get(&self, id: SourceId) -> Option<&SourceEntry> {
        self.sources.iter().find(|s| s.id == id)
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// The name of a source, or its slot and generation when it is not registered.
    fn label(&self, id: SourceId) -> String {
        match self.get(id) {
            Some(s) => s.name.to_owned(),
            None => format!("#{}.{}", id.slot, id.generation),
        }
    }
}

/// One segment of the explained utterance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SegLine {
    pub kind: SegmentKind,
    pub chars: usize,
    /// The catalogue message (its dense number); `None` for a literal. Always `None` without
    /// the `seg-origin` feature.
    pub msg: Option<u32>,
    /// The producer id; `0` is unknown. Always `0` without `seg-origin`.
    pub origin: u32,
    /// The (cut) text, only with the opt-in.
    pub text: Option<String>,
}

/// One known semantic group of the fused entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GroupLine {
    pub group: Group,
    /// The stratum that produced the value (`None`: a spare code the registry does not know).
    pub prov: Option<ProvCode>,
    pub hedge: ConfClass,
    /// Highest trust tier among the winner's inputs.
    pub taint: TrustTier,
    pub fallback: bool,
    pub stale: bool,
    /// Quirk rule ids that rewrote an observed value of this group (engine trace only).
    pub quirks: Vec<u16>,
}

/// What `explain` says about one utterance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplainReport {
    pub utterance: u64,
    pub segments: Vec<SegLine>,
    /// `None` when the node carried no fused entry (nothing is spoken from it before C3).
    pub groups: Option<Vec<GroupLine>>,
    /// The sources that contributed, by name.
    pub sources: Vec<String>,
}

impl ExplainReport {
    /// Segments that are catalogue messages.
    pub fn catalogue(&self) -> usize {
        self.segments.iter().filter(|s| s.msg.is_some()).count()
    }

    /// Segments that are not (page text, the reader's own words without an entry).
    pub fn literal(&self) -> usize {
        self.segments.len() - self.catalogue()
    }

    /// Known groups of the fused entry.
    pub fn group_count(&self) -> usize {
        self.groups.as_ref().map_or(0, Vec::len)
    }

    /// Groups whose value is hedged ("probably", "maybe").
    pub fn hedged(&self) -> usize {
        self.groups.as_ref().map_or(0, |g| {
            g.iter().filter(|l| l.hedge != ConfClass::Asserted).count()
        })
    }

    /// Groups that depend on an input less trusted than the platform.
    pub fn tainted(&self) -> usize {
        self.groups.as_ref().map_or(0, |g| {
            g.iter()
                .filter(|l| l.taint.code() > TrustTier::T1Platform.code())
                .count()
        })
    }

    /// The developer log: one line per segment, one per known group. English, and no page text
    /// unless the report was built with it.
    pub fn render(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.segments.len() + 2);
        out.push(format!(
            "explain utt={} segments={} catalogue={} literal={} sources=[{}]",
            self.utterance,
            self.segments.len(),
            self.catalogue(),
            self.literal(),
            self.sources.join(",")
        ));
        for (i, s) in self.segments.iter().enumerate() {
            let mut l = format!("  seg {i} kind={:?} chars={}", s.kind, s.chars);
            match s.msg {
                Some(m) => {
                    let _ = write!(l, " msg=catalogue:{m}");
                }
                None => l.push_str(" msg=literal"),
            }
            let _ = write!(l, " origin={}", s.origin);
            if let Some(t) = &s.text {
                let _ = write!(l, " text={t:?}");
            }
            out.push(l);
        }
        match &self.groups {
            None => out.push("  no fused entry on the node".to_owned()),
            Some(groups) => {
                for g in groups {
                    let prov = g.prov.map_or("spare", ProvCode::name);
                    let mut l = format!(
                        "  group {} prov={prov} hedge={} taint={}",
                        g.group.name(),
                        g.hedge.name(),
                        g.taint.name()
                    );
                    if g.fallback {
                        l.push_str(" fallback");
                    }
                    if g.stale {
                        l.push_str(" stale");
                    }
                    if !g.quirks.is_empty() {
                        let ids: Vec<String> = g.quirks.iter().map(u16::to_string).collect();
                        let _ = write!(l, " quirks=[{}]", ids.join(","));
                    }
                    out.push(l);
                }
            }
        }
        out
    }
}

/// Build the report. `trace` is the engine's trail for the node when one was recorded (it names
/// the quirk rows that hit); `with_text` adds the (cut) text of each segment.
pub fn explain_utterance(
    u: &Utterance,
    entry: Option<&SemEntry>,
    trace: Option<&Explain>,
    sources: &SourceDirectory,
    with_text: bool,
) -> ExplainReport {
    // The text of what was said about a secret field is never shown, opt-in or not.
    let with_text = with_text && !entry.is_some_and(gorca_present::is_secret_entry);
    let segments = u
        .segments
        .iter()
        .map(|s| SegLine {
            kind: s.kind,
            chars: s.text.chars().count(),
            msg: seg_msg(s),
            origin: seg_origin(s),
            text: with_text.then(|| s.text.chars().take(TEXT_CUT).collect()),
        })
        .collect();
    let groups = entry.map(|e| {
        Group::ALL
            .iter()
            .copied()
            .filter(|g| e.known.has(*g))
            .map(|g| GroupLine {
                group: g,
                prov: e.prov.get(g),
                hedge: ConfClass::from_code(e.hedge.get(g)).unwrap_or_default(),
                taint: TrustTier::from_code(e.taint.get(g)).unwrap_or_default(),
                fallback: e.fallback.has(g),
                stale: e.stale.has(g),
                quirks: trace.map_or_else(Vec::new, |t| quirk_ids(t, g)),
            })
            .collect()
    });
    // The AT-SPI primary is the observed source of every fused node; secondaries join the list
    // when they write (M23).
    let names = if entry.is_some() {
        vec![sources.label(ATSPI_SOURCE)]
    } else {
        Vec::new()
    };
    ExplainReport {
        utterance: u.id.0,
        segments,
        groups,
        sources: names,
    }
}

/// Rule ids of the quirk rows that rewrote an observed value of `g`.
fn quirk_ids(t: &Explain, g: Group) -> Vec<u16> {
    let mut ids: Vec<u16> = t
        .trail(g)
        .iter()
        .filter_map(|e| match e {
            TrailEntry::Observed { cand, .. } if cand.rule != 0 => Some(cand.rule),
            TrailEntry::Observed { .. }
            | TrailEntry::Fact(_)
            | TrailEntry::Patch(_)
            | TrailEntry::User { .. }
            | TrailEntry::Content { .. }
            | TrailEntry::Repair { .. } => None,
        })
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

#[cfg(feature = "seg-origin")]
fn seg_msg(s: &gorca_model::Segment) -> Option<u32> {
    match s.msg {
        gorca_model::MsgId::Literal => None,
        gorca_model::MsgId::Catalog(n) => Some(n),
    }
}

#[cfg(not(feature = "seg-origin"))]
fn seg_msg(_s: &gorca_model::Segment) -> Option<u32> {
    None
}

#[cfg(feature = "seg-origin")]
fn seg_origin(s: &gorca_model::Segment) -> u32 {
    s.origin.0
}

#[cfg(not(feature = "seg-origin"))]
fn seg_origin(_s: &gorca_model::Segment) -> u32 {
    0
}

/// Log the report of an utterance, one `info` line per report line (target `gorca::explain`).
/// Segment text appears only when the opt-in is on.
pub fn log_report(
    sources: &SourceDirectory,
    u: &Utterance,
    entry: Option<&SemEntry>,
    secret: bool,
) -> ExplainReport {
    let with_text = EXPLAIN_TEXT.load(Ordering::Relaxed) && !secret;
    let report = explain_utterance(u, entry, None, sources, with_text);
    for line in report.render() {
        tracing::info!(target: "gorca::explain", "{line}");
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::sem::{GroupMask, SlotBits2};
    use gorca_model::{Epoch, Prio, Segment, UttId};

    fn utt(texts: &[&str]) -> Utterance {
        Utterance {
            id: UttId(7),
            segments: texts
                .iter()
                .map(|t| Segment::new((*t).into(), SegmentKind::Text))
                .collect(),
            lang: None,
            epoch: Epoch::default(),
            prio: Prio::Interrupt,
            may_refine: false,
            survives_focus_change: false,
            ttl_secs: 0,
            refines: None,
        }
    }

    fn entry() -> SemEntry {
        let mut e = SemEntry::default();
        e.known = e.known.with(Group::Role).with(Group::Name);
        e.prov.set(Group::Role, ProvCode::ObservedPrimary);
        e.prov.set(Group::Name, ProvCode::DerivedTainted);
        let mut hedge = SlotBits2::default();
        hedge.set(Group::Name, ConfClass::Probable.code());
        e.hedge = hedge;
        let mut taint = SlotBits2::default();
        taint.set(Group::Name, TrustTier::T3Untrusted.code());
        e.taint = taint;
        e.fallback = GroupMask::EMPTY.with(Group::Role);
        e
    }

    #[test]
    fn the_default_report_never_carries_page_text() {
        let u = utt(&["secret message body", "button"]);
        let d = SourceDirectory::with_atspi_primary();
        let r = explain_utterance(&u, Some(&entry()), None, &d, false);
        let log = r.render().join("\n");
        assert!(!log.contains("secret"), "{log}");
        assert!(log.contains("chars=19"), "{log}");
        let with = explain_utterance(&u, None, None, &d, true);
        assert!(with.render().join("\n").contains("secret message body"));
    }

    #[test]
    fn groups_name_stratum_hedge_taint_and_fallback() {
        let d = SourceDirectory::with_atspi_primary();
        let r = explain_utterance(&utt(&["x"]), Some(&entry()), None, &d, false);
        assert_eq!(r.group_count(), 2);
        let name = r
            .groups
            .iter()
            .flatten()
            .find(|g| g.group == Group::Name)
            .cloned();
        assert_eq!(
            name.as_ref().map(|g| (g.prov, g.hedge)),
            Some((Some(ProvCode::DerivedTainted), ConfClass::Probable))
        );
        assert_eq!((r.hedged(), r.tainted()), (1, 1));
        assert_eq!(r.sources, ["atspi"]);
        let log = r.render().join("\n");
        assert!(
            log.contains("group role prov=ObservedPrimary hedge=Asserted taint=T0 fallback"),
            "{log}"
        );
    }

    #[test]
    fn a_node_without_an_entry_says_so() {
        let r = explain_utterance(&utt(&["a"]), None, None, &SourceDirectory::new(), false);
        assert!(r.groups.is_none() && r.sources.is_empty());
        assert!(r.render().join("\n").contains("no fused entry"));
    }

    #[test]
    fn the_atspi_primary_is_source_zero_zero_and_names_are_unique() {
        let mut d = SourceDirectory::with_atspi_primary();
        let a = d.get(ATSPI_SOURCE);
        assert_eq!(
            a.map(|s| (s.class, s.tier)),
            Some((SourceClass::Primary, TrustTier::T1Platform))
        );
        assert_eq!((ATSPI_SOURCE.slot, ATSPI_SOURCE.generation), (0, 0));
        let dup = SourceEntry {
            id: SourceId {
                slot: 1,
                generation: 0,
            },
            name: "atspi",
            class: SourceClass::Secondary,
            tier: TrustTier::T3Untrusted,
        };
        assert!(d.register(dup).is_err());
        assert_eq!(d.len(), 1);
    }

    #[cfg(feature = "seg-origin")]
    #[test]
    fn segment_origin_shows_catalogue_and_producer() {
        let mut u = utt(&["page", "button"]);
        let tagged = u.segments[1].clone().tagged(42, 9);
        u.segments[1] = tagged;
        let r = explain_utterance(&u, None, None, &SourceDirectory::new(), false);
        assert_eq!((r.catalogue(), r.literal()), (1, 1));
        assert_eq!(r.segments[1].origin, 9);
        assert!(r.render().join("\n").contains("msg=catalogue:42 origin=9"));
    }

    #[test]
    fn explain_never_shows_the_text_said_about_a_password_field() {
        let mut e = entry();
        e.sem.role = gorca_model::sem::SemRole::PasswordBox;
        let u = utt(&["hunter2-correct-horse"]);
        let r = explain_utterance(&u, Some(&e), None, &SourceDirectory::new(), true);
        assert!(r.segments.iter().all(|s| s.text.is_none()), "{r:?}");
        // The caller knows better than the entry (native role says password): same result.
        let r = log_report(&SourceDirectory::new(), &u, Some(&entry()), true);
        assert!(r.segments.iter().all(|s| s.text.is_none()), "{r:?}");
        // An ordinary node with the opt-in still shows its (cut) text.
        let r = explain_utterance(&u, Some(&entry()), None, &SourceDirectory::new(), true);
        assert!(r.segments.iter().all(|s| s.text.is_some()));
    }
}
