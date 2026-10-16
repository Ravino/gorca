//! Live-region policy (ADR-0003 section 13, M8): which changes of a document become speech,
//! and how many. Pure over a published `WebDoc` plus a caller-supplied clock, so it is tested
//! without a browser and costs nothing on the key path: the document actor feeds it the nodes a
//! batch touched and polls it for due messages.
//!
//! * A *region* is a node with a `live` attribute other than `off`, or a live ARIA role
//!   (`alert`, `status`, `log`). The nearest region above a touched node owns the change.
//! * A change is measured on the mirror, not on raw event strings (LR-2): the lines of the
//!   region before and after, new lines for `additions`, the whole text for `atomic`, removed
//!   lines for `relevant` containing `removals`/`all`.
//! * A burst settles per region: 50 ms after the last touch, 200 ms after the first (LR-3).
//! * Text present when a region appears is not spoken, except an `alert` added after the page
//!   settled (LR-4); `busy` defers a region until it clears or 3 s pass (LR-5).
//! * Bounded: a token bucket per region (polite 1/s burst 3, assertive 2/s burst 3), 4/s per
//!   document, a ring of 8 recent texts per region (the same text within 10 s is dropped), and
//!   at most one "N updates in region" summary per 5 s. All state is capped (64 regions).

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_doc::web::{NodeFlags, NodeView, WebDoc};
use gorca_doc_source_api::web_attrs;
use gorca_model::{DocNodeId, Role};

/// Regions tracked per document; further regions are ignored.
pub const MAX_REGIONS: usize = 64;
/// Touched nodes looked at per batch.
const MAX_TOUCHED: usize = 4096;
/// Hops from a touched node up to its region.
const REGION_HOPS: usize = 24;
/// A burst is spoken this long after its last touch...
const SETTLE: Duration = Duration::from_millis(50);
/// ...and at the latest this long after its first.
const SETTLE_MAX: Duration = Duration::from_millis(200);
/// `aria-busy` defers a region at most this long.
const BUSY_MAX: Duration = Duration::from_secs(3);
/// Regions that appear within this time of the first batch are part of the page, not news.
const LOAD_QUIET: Duration = Duration::from_millis(600);
/// A batch touching at least this many nodes is page construction, not an update.
const BULK_TOUCH: usize = 64;
/// The same text in the same region is not spoken twice within this time.
const DEDUPE_WINDOW: Duration = Duration::from_secs(10);
/// A summary of dropped updates is spoken at most this often.
const SUMMARY_EVERY: Duration = Duration::from_secs(5);
/// Lines kept per region snapshot (the newest).
const MAX_LINES: usize = 256;
/// Characters kept per line.
const MAX_LINE: usize = 1024;
/// An announcement is cut at this many characters (a page cannot make the reader read a book).
pub const MAX_ANNOUNCE: usize = 300;
/// Characters of a region read per snapshot (bounds the cost of one settle).
const MAX_SNAPSHOT: u32 = 16 * 1024;
/// Snapshots taken per `tick` over all regions (the rest wait for the next tick).
const MAX_SNAPSHOTS_PER_TICK: usize = 16;
/// Per-document rate (messages per second, burst).
const DOC_RATE: (f32, f32) = (4.0, 4.0);

/// One thing to say.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiveMsg {
    pub text: Arc<str>,
    pub assertive: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Politeness {
    Off,
    Polite,
    Assertive,
}

#[derive(Copy, Clone, Debug)]
#[allow(clippy::struct_excessive_bools)] // independent ARIA flags of one region
struct Props {
    politeness: Politeness,
    atomic: bool,
    removals: bool,
    busy: bool,
    alert: bool,
}

fn attr<'a>(v: &'a NodeView<'_>, k: gorca_model::AttrKey) -> Option<&'a str> {
    v.meta
        .and_then(|m| m.attrs.iter().find(|(a, _)| *a == k).map(|(_, x)| &**x))
}

fn has_role(v: &NodeView<'_>, tok: &str) -> bool {
    attr(v, web_attrs::XML_ROLES).is_some_and(|r| r.split_ascii_whitespace().any(|t| t == tok))
}

/// The live properties of a node that is a region root; `None` for an ordinary node.
fn props_of(v: &NodeView<'_>) -> Option<Props> {
    if v.flags.contains(NodeFlags::HIDDEN) {
        return None;
    }
    let alert = v.role == Role::Alert || has_role(v, "alert") || has_role(v, "alertdialog");
    let status = has_role(v, "status");
    let log = has_role(v, "log");
    let quiet_role = has_role(v, "timer") || has_role(v, "marquee");
    let explicit = attr(v, web_attrs::LIVE);
    let politeness = match explicit {
        Some("assertive") => Politeness::Assertive,
        Some("polite") => Politeness::Polite,
        Some("off") => Politeness::Off,
        _ if alert => Politeness::Assertive,
        _ if status || log => Politeness::Polite,
        _ if quiet_role => Politeness::Off,
        _ => return None,
    };
    let relevant = attr(v, web_attrs::RELEVANT).unwrap_or("");
    let removals = relevant
        .split_ascii_whitespace()
        .any(|t| t == "removals" || t == "all");
    let atomic = match attr(v, web_attrs::ATOMIC) {
        Some("true") => true,
        Some(_) => false,
        None => alert || (status && !log),
    };
    Some(Props {
        politeness,
        atomic,
        removals,
        busy: attr(v, web_attrs::BUSY) == Some("true"),
        alert,
    })
}

/// The region that owns a change at `id`: the nearest ancestor-or-self that is a region root.
fn region_of(doc: &WebDoc, id: DocNodeId) -> Option<(DocNodeId, Props)> {
    let mut cur = id;
    for _ in 0..REGION_HOPS {
        let v = doc.node(cur)?;
        if let Some(p) = props_of(&v) {
            return Some((cur, p));
        }
        cur = doc.parent(cur)?;
    }
    None
}

/// The non-empty lines of a region's text, whitespace collapsed.
fn lines_of(doc: &WebDoc, id: DocNodeId) -> Vec<String> {
    let Some((a, b)) = doc.subtree_text_range(id) else {
        return Vec::new();
    };
    let text = doc.text_in(a, b.min(a.saturating_add(MAX_SNAPSHOT)));
    let mut out: Vec<String> = text
        .split('\n')
        .map(|l| {
            l.replace('\u{FFFC}', " ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|l| !l.is_empty())
        .map(|mut l| {
            if let Some((ix, _)) = l.char_indices().nth(MAX_LINE) {
                l.truncate(ix);
            }
            l
        })
        .collect();
    if out.len() > MAX_LINES {
        out.drain(..out.len() - MAX_LINES);
    }
    out
}

/// What was added (or removed) between two snapshots of a region, as one text.
fn diff(old: &[String], new: &[String], props: Props) -> Option<String> {
    if old == new {
        return None;
    }
    if props.atomic {
        let text = new.join(" ");
        return (!text.is_empty()).then(|| cut(text));
    }
    let mut pool: HashMap<&str, usize> = HashMap::new();
    for l in old {
        *pool.entry(l.as_str()).or_default() += 1;
    }
    let mut added: Vec<&str> = Vec::new();
    for l in new {
        match pool.get_mut(l.as_str()) {
            Some(n) if *n > 0 => *n -= 1,
            _ => added.push(l.as_str()),
        }
    }
    // The leftovers of the pool are the removed lines.
    let mut gone: Vec<&str> = Vec::new();
    let mut left = pool;
    for l in old {
        if let Some(n) = left.get_mut(l.as_str())
            && *n > 0
        {
            *n -= 1;
            gone.push(l.as_str());
        }
    }
    let mut parts: Vec<String> = Vec::new();
    if let ([one], [was]) = (added.as_slice(), gone.as_slice())
        && let Some(rest) = one.strip_prefix(*was)
        && !rest.trim_matches(|c: char| !c.is_alphanumeric()).is_empty()
    {
        // The same line grew at its end ("Saved" -> "Saved. Retrying"): say what is new.
        parts.push(
            rest.trim_start_matches(|c: char| !c.is_alphanumeric())
                .to_owned(),
        );
    } else {
        parts.extend(added.iter().map(|s| (*s).to_owned()));
    }
    if props.removals {
        parts.extend(gone.iter().map(|s| (*s).to_owned()));
    }
    let text = parts.join(" ");
    (!text.trim().is_empty()).then(|| cut(text))
}

/// Cut an announcement at [`MAX_ANNOUNCE`] characters.
fn cut(mut text: String) -> String {
    if let Some((ix, _)) = text.char_indices().nth(MAX_ANNOUNCE) {
        text.truncate(ix);
    }
    text
}

/// Case and spacing do not make a message new.
fn normalise(text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut last_space = true;
    for c in text.chars().take(512) {
        if c.is_whitespace() {
            if !last_space {
                ' '.hash(&mut h);
            }
            last_space = true;
        } else {
            last_space = false;
            // Digits count: "Log entry 1" and "Log entry 2" are different news (ADR-0003 GM 5.3
            // folded them; a counter, a log or a score must not be swallowed).
            for l in c.to_lowercase() {
                l.hash(&mut h);
            }
        }
    }
    h.finish()
}

#[derive(Clone, Copy, Debug)]
struct Bucket {
    tokens: f32,
    last: Instant,
    rate: f32,
    burst: f32,
}

impl Bucket {
    fn new(rate: f32, burst: f32, now: Instant) -> Self {
        Self {
            tokens: burst,
            last: now,
            rate,
            burst,
        }
    }

    fn take(&mut self, now: Instant) -> bool {
        let dt = now.saturating_duration_since(self.last).as_secs_f32();
        self.last = now;
        self.tokens = (self.tokens + dt * self.rate).min(self.burst);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[derive(Debug)]
struct Region {
    base: Vec<String>,
    first_dirty: Option<Instant>,
    last_touch: Instant,
    ring: VecDeque<(u64, Instant)>,
    bucket: Option<Bucket>,
    dropped: u32,
}

impl Region {
    fn new(base: Vec<String>, now: Instant) -> Self {
        Self {
            base,
            first_dirty: None,
            last_touch: now,
            ring: VecDeque::new(),
            bucket: None,
            dropped: 0,
        }
    }

    fn seen(&mut self, key: u64, now: Instant) -> bool {
        while self
            .ring
            .front()
            .is_some_and(|(_, t)| now.saturating_duration_since(*t) > DEDUPE_WINDOW)
        {
            self.ring.pop_front();
        }
        if self.ring.iter().any(|(k, _)| *k == key) {
            return true;
        }
        if self.ring.len() >= 8 {
            self.ring.pop_front();
        }
        self.ring.push_back((key, now));
        false
    }
}

/// Per-document live-region state.
#[derive(Debug)]
pub struct LiveTracker {
    regions: HashMap<DocNodeId, Region>,
    started: Option<Instant>,
    doc_bucket: Option<Bucket>,
    next_summary: Option<Instant>,
}

impl Default for LiveTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveTracker {
    pub fn new() -> Self {
        Self {
            regions: HashMap::new(),
            started: None,
            doc_bucket: None,
            next_summary: None,
        }
    }

    /// Forget everything (the document went dormant or navigated): regions found later are
    /// part of the page, not news.
    pub fn reset(&mut self) {
        self.regions.clear();
        self.started = None;
    }

    /// A batch was published: mark the regions that own the touched nodes.
    pub fn on_batch(&mut self, doc: &WebDoc, touched: &[DocNodeId], now: Instant) {
        // The quiet period runs from the last bulk batch (page construction), not from the first.
        if touched.len() >= BULK_TOUCH || self.started.is_none() {
            self.started = Some(now);
        }
        let started = self.started.unwrap_or(now);
        let quiet = now.saturating_duration_since(started) < LOAD_QUIET;
        let mut seen: std::collections::HashSet<DocNodeId> = std::collections::HashSet::new();
        for id in touched.iter().take(MAX_TOUCHED) {
            let Some((root, props)) = region_of(doc, *id) else {
                continue;
            };
            if !seen.insert(root) {
                continue;
            }
            if let Some(r) = self.regions.get_mut(&root) {
                r.first_dirty.get_or_insert(now);
                r.last_touch = now;
            } else if self.regions.len() < MAX_REGIONS {
                // A new region: its present text is not news, except an alert after the load.
                let base = if props.alert && !quiet {
                    Vec::new()
                } else {
                    lines_of(doc, root)
                };
                let mut r = Region::new(base, now);
                if props.alert && !quiet {
                    r.first_dirty = Some(now);
                }
                self.regions.insert(root, r);
            }
        }
        // Regions that left the document stop costing anything.
        if self.regions.len() > MAX_REGIONS / 2 {
            self.regions.retain(|id, _| doc.is_attached(*id));
        }
    }

    /// When `tick` next has something to do.
    pub fn next_deadline(&self) -> Option<Instant> {
        let due = self
            .regions
            .values()
            .filter_map(|r| {
                r.first_dirty
                    .map(|f| (r.last_touch + SETTLE).min(f + SETTLE_MAX))
            })
            .min();
        match (due, self.next_summary) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Messages that are due at `now`, in region order of appearance (assertive first).
    pub fn tick(&mut self, doc: &WebDoc, now: Instant) -> Vec<LiveMsg> {
        let mut out: Vec<LiveMsg> = Vec::new();
        let mut gone: Vec<DocNodeId> = Vec::new();
        let ids: Vec<DocNodeId> = self.regions.keys().copied().collect();
        let mut budget = MAX_SNAPSHOTS_PER_TICK;
        for id in ids {
            let Some(region) = self.regions.get_mut(&id) else {
                continue;
            };
            let Some(first) = region.first_dirty else {
                continue;
            };
            if now < (region.last_touch + SETTLE).min(first + SETTLE_MAX) {
                continue;
            }
            let Some(v) = doc.node(id).filter(|_| doc.is_attached(id)) else {
                gone.push(id);
                continue;
            };
            let Some(props) = props_of(&v) else {
                // No longer a region (hidden, attribute removed): follow the text silently.
                region.base = lines_of(doc, id);
                region.first_dirty = None;
                continue;
            };
            if props.busy && now.saturating_duration_since(first) < BUSY_MAX {
                continue;
            }
            if budget == 0 {
                // Over the per-tick budget: stays dirty, settles on the next tick.
                continue;
            }
            budget -= 1;
            region.first_dirty = None;
            let lines = lines_of(doc, id);
            let text = diff(&region.base, &lines, props);
            region.base = lines;
            if props.politeness == Politeness::Off {
                continue;
            }
            let Some(text) = text else { continue };
            if region.seen(normalise(&text), now) {
                continue;
            }
            let assertive = props.politeness == Politeness::Assertive;
            let (rate, burst) = if assertive { (2.0, 3.0) } else { (1.0, 3.0) };
            let rb = region
                .bucket
                .get_or_insert_with(|| Bucket::new(rate, burst, now));
            let doc_ok = self
                .doc_bucket
                .get_or_insert_with(|| Bucket::new(DOC_RATE.0, DOC_RATE.1, now));
            if rb.take(now) && doc_ok.take(now) {
                out.push(LiveMsg {
                    text: Arc::from(text.as_str()),
                    assertive,
                });
            } else {
                region.dropped = region.dropped.saturating_add(1);
                self.next_summary.get_or_insert(now + SUMMARY_EVERY);
            }
        }
        for id in gone {
            self.regions.remove(&id);
        }
        if self.next_summary.is_some_and(|t| now >= t) {
            self.next_summary = None;
            let mut total = 0u32;
            let mut name: Option<String> = None;
            for (id, r) in &mut self.regions {
                if r.dropped > 0 {
                    total = total.saturating_add(r.dropped);
                    if name.is_none() {
                        name = doc
                            .node(*id)
                            .and_then(|v| v.name.map(str::to_owned))
                            .filter(|n| !n.is_empty());
                    }
                    r.dropped = 0;
                }
            }
            if total > 0 {
                let text = match name {
                    Some(n) => format!("{total} updates in {n}"),
                    None => format!("{total} updates"),
                };
                out.push(LiveMsg {
                    text: Arc::from(text.as_str()),
                    assertive: false,
                });
            }
        }
        out.sort_by_key(|m| !m.assertive);
        out
    }
}

#[cfg(test)]
mod tests {
    use gorca_doc::web::{Cat, DocOp, NodeMeta, NodeSpec, SubtreeSpec};

    use super::*;

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    fn polite() -> Props {
        Props {
            politeness: Politeness::Polite,
            atomic: false,
            removals: false,
            busy: false,
            alert: false,
        }
    }

    #[test]
    fn additions_are_the_new_lines_only() {
        let d = diff(&lines(&["a", "b"]), &lines(&["a", "b", "c"]), polite());
        assert_eq!(d.as_deref(), Some("c"));
        assert_eq!(diff(&lines(&["a"]), &lines(&["a"]), polite()), None);
        // A replaced line is new text.
        let d = diff(&lines(&["Saved at 1"]), &lines(&["Saved at 2"]), polite());
        assert_eq!(d.as_deref(), Some("Saved at 2"));
        // A grown line says its tail.
        let d = diff(&lines(&["Saved"]), &lines(&["Saved. Retrying"]), polite());
        assert_eq!(d.as_deref(), Some("Retrying"));
    }

    #[test]
    fn atomic_says_the_whole_region_and_removals_only_when_relevant() {
        let atomic = Props {
            atomic: true,
            ..polite()
        };
        let d = diff(
            &lines(&["Ticker: 1 points"]),
            &lines(&["Ticker: 2 points"]),
            atomic,
        );
        assert_eq!(d.as_deref(), Some("Ticker: 2 points"));
        assert_eq!(diff(&lines(&["gone"]), &lines(&[]), polite()), None);
        let rm = Props {
            removals: true,
            ..polite()
        };
        assert_eq!(
            diff(&lines(&["gone"]), &lines(&[]), rm).as_deref(),
            Some("gone")
        );
    }

    #[test]
    fn normalise_ignores_case_digits_and_spacing() {
        assert_eq!(normalise("Score  12"), normalise("score 12"));
        assert_ne!(normalise("score 12"), normalise("score 13"));
    }

    #[test]
    fn a_bucket_allows_a_burst_then_the_rate() {
        let t0 = Instant::now();
        let mut b = Bucket::new(1.0, 3.0, t0);
        assert!(b.take(t0) && b.take(t0) && b.take(t0));
        assert!(!b.take(t0));
        assert!(b.take(t0 + Duration::from_millis(1100)));
        assert!(!b.take(t0 + Duration::from_millis(1200)));
    }

    fn page(text: &str, live: &str) -> (WebDoc, DocNodeId) {
        let mut d = WebDoc::new(1);
        let mut m = NodeMeta::default();
        m.attrs.push((web_attrs::LIVE, Arc::from(live)));
        let region = SubtreeSpec::with_children(
            NodeSpec::new(Role::Panel)
                .flags(NodeFlags::BLOCK | NodeFlags::LIVE)
                .name("Flood region")
                .meta(Arc::new(m)),
            vec![SubtreeSpec::leaf(
                NodeSpec::new(Role::StaticText).text(text),
            )],
        );
        let root = SubtreeSpec::with_children(NodeSpec::new(Role::DocumentWeb), vec![region]);
        assert!(
            d.apply(&DocOp::InsertSubtree {
                parent: None,
                before: None,
                subtree: Arc::new(root),
            })
            .is_ok()
        );
        let r = d.first_of(Cat::LIVE_REGION).unwrap();
        (d, r)
    }

    fn set_text(d: &mut WebDoc, region: DocNodeId, text: &str) -> DocNodeId {
        let leaf = d.first_child(region).unwrap();
        assert!(
            d.apply(&DocOp::SetText {
                id: leaf,
                text: Some(Arc::from(text)),
            })
            .is_ok()
        );
        leaf
    }

    #[test]
    fn a_change_after_the_load_is_spoken_once_the_burst_settles() {
        let t0 = Instant::now();
        let (mut d, r) = page("idle", "polite");
        let mut t = LiveTracker::new();
        t.on_batch(&d, &[r], t0);
        // present at load: silent
        assert!(t.tick(&d, t0 + Duration::from_millis(300)).is_empty());
        let later = t0 + Duration::from_secs(3);
        let leaf = set_text(&mut d, r, "Saved at step 1");
        t.on_batch(&d, &[leaf], later);
        assert!(
            t.tick(&d, later + Duration::from_millis(10)).is_empty(),
            "settling"
        );
        let m = t.tick(&d, later + Duration::from_millis(60));
        assert_eq!(m.len(), 1);
        assert_eq!(&*m[0].text, "Saved at step 1");
        assert!(!m[0].assertive);
        // the same text again within 10 s is dropped
        let leaf = set_text(&mut d, r, "x");
        t.on_batch(&d, &[leaf], later + Duration::from_secs(1));
        let _ = t.tick(&d, later + Duration::from_millis(1100));
        let leaf = set_text(&mut d, r, "Saved at step 1");
        t.on_batch(&d, &[leaf], later + Duration::from_secs(2));
        assert!(t.tick(&d, later + Duration::from_millis(2100)).is_empty());
    }

    #[test]
    fn a_hostile_flood_of_regions_and_long_text_stays_bounded() {
        let t0 = Instant::now();
        let (mut d, r) = page("idle", "polite");
        let mut t = LiveTracker::new();
        t.on_batch(&d, &[r], t0);
        let start = t0 + Duration::from_secs(3);
        let long = "word ".repeat(4_000);
        let mut spoken = 0usize;
        let mut worst = 0usize;
        // 3 000 batches in 9 simulated seconds, each touching the region, with a huge text.
        for i in 0..3_000u32 {
            let now = start + Duration::from_micros(u64::from(i) * 3000);
            let leaf = set_text(&mut d, r, &format!("{i} {long}"));
            t.on_batch(&d, &[leaf], now);
            while let Some(due) = t.next_deadline().filter(|due| *due <= now) {
                for m in t.tick(&d, due) {
                    spoken += 1;
                    worst = worst.max(m.text.chars().count());
                }
            }
        }
        assert!(worst <= MAX_ANNOUNCE, "announcement of {worst} chars");
        // polite 1/s burst 3 over 10 s, plus summaries, far below the 3 000 updates
        assert!(spoken <= 20, "spoken {spoken}");
        assert!(t.regions.len() <= MAX_REGIONS);
    }

    #[test]
    fn a_flood_is_bounded_and_summarised() {
        let t0 = Instant::now();
        let (mut d, r) = page("idle", "polite");
        let mut t = LiveTracker::new();
        t.on_batch(&d, &[r], t0);
        let start = t0 + Duration::from_secs(3);
        let mut spoken = 0;
        let mut summaries = 0;
        for i in 0..200u32 {
            let now = start + Duration::from_millis(u64::from(i) * 5);
            let leaf = set_text(&mut d, r, &format!("Flood update {}", i * 7));
            t.on_batch(&d, &[leaf], now);
            // poll like the actor's timer: at the deadline
            while let Some(due) = t.next_deadline().filter(|due| *due <= now) {
                for m in t.tick(&d, due) {
                    if m.text.contains("updates in") {
                        summaries += 1;
                    } else {
                        spoken += 1;
                    }
                }
            }
        }
        for step in 0..60u64 {
            let now = start + Duration::from_millis(1000 + step * 100);
            for m in t.tick(&d, now) {
                if m.text.contains("updates in") {
                    summaries += 1;
                } else {
                    spoken += 1;
                }
            }
        }
        assert!(spoken <= 8, "spoken {spoken}");
        assert!(spoken >= 1);
        assert!(summaries <= 2, "summaries {summaries}");
    }
}
