//! The flat-review line model (ADR-0006 3, 6): an immutable `Arc<LineModel>` per surface.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use gorca_model::{AppId, NodeKey, Rect, TextLine};
use smallvec::SmallVec;

use super::cursor::ModelId;

/// Hostile-input bounds of ADR-0006 6.8 and 5.3.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ReviewLimits {
    /// Nodes visited by one model build.
    pub nodes_per_build: usize,
    /// Zones per model.
    pub zones: usize,
    /// Text bytes per model.
    pub text_bytes: usize,
    /// `VisibleLines` per text node.
    pub lines_per_node: u16,
    pub bytes_per_node: u32,
    /// Nodes visited by one object-navigation step (OB-1).
    pub nav_visits: usize,
    /// Layout descent depth of one step (OB-2).
    pub nav_depth: usize,
    /// Suspensions of one step (OB-3).
    pub nav_suspensions: u8,
    /// Reviews fetches in flight per app.
    pub in_flight: usize,
}

impl Default for ReviewLimits {
    fn default() -> Self {
        Self {
            nodes_per_build: 4_096,
            zones: 2_048,
            text_bytes: 512 * 1024,
            lines_per_node: 200,
            bytes_per_node: 64 * 1024,
            nav_visits: 512,
            nav_depth: 32,
            nav_suspensions: 3,
            in_flight: 8,
        }
    }
}

/// Why a model is not the whole surface.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TruncWhy {
    NodeCap,
    ZoneCap,
    TextCap,
    Deadline,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Completeness {
    Complete,
    Truncated(TruncWhy),
    /// More than half of the zones have no extents: one line per zone in tree order (GX-2).
    GeometryFallback,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LineSource {
    Geometry,
    Terminal,
    Document,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ZoneKind {
    /// A visual line of a text node; `src` is its scalar range in the node's text.
    Text { src: Range<u32> },
    /// A whole object: name, value, role word.
    Object,
    /// The name of a container, spoken before its content (rule 3).
    Name,
}

#[derive(Clone, Debug)]
pub struct Zone {
    pub node: NodeKey,
    pub kind: ZoneKind,
    /// What is spoken for this zone.
    pub text: Arc<str>,
    /// Surface-relative; `None` = synthetic placement (GX-1).
    pub rect: Option<Rect>,
    /// The node's accessible name, for object zones (label elision, rule 4).
    pub name: Option<Arc<str>>,
    /// The zone is the text of a label: dropped when a control on its line carries the same name.
    pub label_like: bool,
}

impl Zone {
    pub fn src_start(&self) -> u32 {
        match &self.kind {
            ZoneKind::Text { src } => src.start,
            ZoneKind::Object | ZoneKind::Name => 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Line {
    pub zones: Range<u32>,
    pub top: i32,
    pub bottom: i32,
    /// The zone texts joined by a single space (CL-4); "blank" is applied at speech time.
    pub text: Arc<str>,
}

/// What the validity check compares per distinct node (ADR-0006 6.5).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NodeCheck {
    pub node: NodeKey,
    pub hash: u64,
    /// The node's own extents as the app reported them (not a text line's), for RR-1 (F-5).
    pub rect: Option<Rect>,
}

#[derive(Debug)]
pub struct LineModel {
    pub id: ModelId,
    pub app: AppId,
    pub surface: NodeKey,
    pub source: LineSource,
    pub lines: Box<[Line]>,
    pub zones: Box<[Zone]>,
    pub by_node: HashMap<NodeKey, SmallVec<[u32; 2]>>,
    /// One entry per distinct node that contributed a zone.
    pub checks: Box<[NodeCheck]>,
    /// The visible lines of each text node, for incremental refreshes.
    pub texts: HashMap<NodeKey, Arc<[TextLine]>>,
    /// `MirrorRead::revision()` when the build read the mirror.
    pub built_rev: u64,
    pub built_at: Instant,
    pub completeness: Completeness,
    /// The rectangle text lines were asked for; revalidation uses the same one (F-5).
    pub clip: Rect,
    /// The line the caret is on, for a model of a screen that has one (a terminal, LS-1):
    /// where flat review starts.
    pub focus_line: Option<u32>,
}

static NEXT_MODEL: AtomicU64 = AtomicU64::new(1);

/// A fresh process-unique model id.
pub fn next_model_id() -> ModelId {
    ModelId(NEXT_MODEL.fetch_add(1, Ordering::Relaxed))
}

impl LineModel {
    /// The zones of `line`.
    pub fn line_zones(&self, line: usize) -> &[Zone] {
        let Some(l) = self.lines.get(line) else {
            return &[];
        };
        self.zones
            .get(l.zones.start as usize..l.zones.end as usize)
            .unwrap_or(&[])
    }

    /// Line index and zone index within the line of the global zone `z`.
    pub fn locate(&self, z: u32) -> Option<(u32, u16)> {
        let idx = self
            .lines
            .partition_point(|l| l.zones.end <= z)
            .min(self.lines.len());
        let l = self.lines.get(idx)?;
        if !l.zones.contains(&z) {
            return None;
        }
        let line = u32::try_from(idx).ok()?;
        let zone = u16::try_from(z - l.zones.start).ok()?;
        Some((line, zone))
    }

    /// The extents recorded for `node` when the model was built.
    pub fn node_rect(&self, node: NodeKey) -> Option<Rect> {
        let i = self.checks.binary_search_by_key(&node, |c| c.node).ok()?;
        self.checks.get(i)?.rect
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Approximate footprint for the `RI-4` counters.
    pub fn bytes(&self) -> usize {
        let z: usize = self
            .zones
            .iter()
            .map(|z| std::mem::size_of::<Zone>() + z.text.len())
            .sum();
        let l: usize = self
            .lines
            .iter()
            .map(|l| std::mem::size_of::<Line>() + l.text.len())
            .sum();
        z + l
            + self.checks.len() * std::mem::size_of::<NodeCheck>()
            + self.by_node.len() * 48
            + self
                .texts
                .values()
                .map(|t| t.iter().map(|x| x.text.len() + 48).sum::<usize>())
                .sum::<usize>()
    }
}
