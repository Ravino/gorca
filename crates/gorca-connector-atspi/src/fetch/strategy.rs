//! How each `BackendOp` is executed: which AT-SPI calls, in which order, how many at once.
//!
//! This file is the **naive reference strategy**, the one R0 uses: one sequential call chain per
//! op, no pipelining, no `Properties.GetAll`. A fill strategy may replace any function here
//! (pipelining over the app's connection, splitting large ops) as long as the signatures hold,
//! every call carries the op's deadline (D-4), it never exceeds the op's share of `MAX_IN_FLIGHT`,
//! and everything it learns is app-supplied and untrusted (refs to other bus names are dropped).

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

use gorca_model::{NodeKey, PropMask, Role, StateSet};
use gorca_rt::Deadline;
use smallvec::SmallVec;

use super::items::{self, MAX_DUMP_BYTES};
use super::{
    CallCx, Caller, MAX_ANCESTRY_DEPTH, MAX_CHILD_RANGE, MAX_SUBTREE_NODES, RawBatch, Resolved,
    backend_error, label_may_be_read, wants_entry_text,
};
use crate::decode::{MAX_TEXT_BYTES, RawNode, RawValue, clip, clip_child_count};
use gorca_connector_api::BackendError;

const NULL_PATH: &str = "/org/a11y/atspi/null";

/// Name, role, state and what `want` asks on top of one object. The base calls run
/// concurrently; all share one deadline. Failures of the optional parts are not errors: the
/// node is simply poorer, and `have` says so.
pub(crate) async fn fetch(
    cx: &CallCx,
    node: &Resolved,
    want: PropMask,
    dl: Deadline,
) -> Result<RawNode, BackendError> {
    fetch_node(cx, node, want, None, dl)
        .await
        .map(|(raw, _)| raw)
}

/// [`fetch`] that also hands back the raw `Parent` reference when `want` asked for it, so a
/// caller that climbs (`ancestry`) needs no second call for the same property. A foreign parent
/// is left out of `raw.parent` and counted. With `known_parent` (a children walk: the caller
/// listed the node under it) no `Parent` call is made and the link is taken as given.
async fn fetch_node(
    cx: &CallCx,
    node: &Resolved,
    want: PropMask,
    known_parent: Option<NodeKey>,
    dl: Deadline,
) -> Result<(RawNode, Option<(String, String)>), BackendError> {
    let c = &cx.caller;
    let path: &str = &node.path;
    let name_fut = async {
        if want.contains(PropMask::NAME) {
            c.name_of(c.bus_name(), path, dl).await.map(Some)
        } else {
            Ok(None)
        }
    };
    // Everything asked for is requested at once: the calls pipeline on the app's connection, so
    // an extra field costs no extra round trip on the cold path.
    let desc_fut = async {
        if want.contains(PropMask::DESCRIPTION) {
            c.description_of(path, dl).await.map(Some)
        } else {
            Ok(None)
        }
    };
    let parent_fut = async {
        if want.contains(PropMask::PARENT) && known_parent.is_none() {
            c.parent_of(path, dl).await.map(Some)
        } else {
            Ok(None)
        }
    };
    let count_fut = async {
        if want.contains(PropMask::CHILDREN) {
            c.child_count_of(path, dl).await.map(Some)
        } else {
            Ok(None)
        }
    };
    let (role, state, name, desc, parent, count) = tokio::join!(
        c.role(path, dl),
        c.state(path, dl),
        name_fut,
        desc_fut,
        parent_fut,
        count_fut
    );
    // Role and state are required; a failed name only costs the name.
    let role = role.map_err(|e| backend_error(&e))?;
    let state_words = state.map_err(|e| backend_error(&e))?;
    let core_role = crate::decode::role_from_raw(role);
    let states = crate::decode::states_from_raw(&state_words);
    let mut have = PropMask::ROLE | PropMask::STATES;
    let name_ok = name.is_ok();
    let mut raw = RawNode {
        role,
        state_words,
        name: name.ok().flatten(),
        ..RawNode::default()
    };
    if want.contains(PropMask::NAME) && name_ok {
        have |= PropMask::NAME;
    }
    if let Ok(Some(d)) = desc {
        raw.description = Some(clip(&d, MAX_TEXT_BYTES).to_owned());
        have |= PropMask::DESCRIPTION;
    }
    let mut parent_ref = None;
    if let Some(p) = known_parent {
        raw.parent = Some(p);
        have |= PropMask::PARENT;
    } else if let Ok(Some(parent)) = parent {
        raw.parent = parent.as_ref().and_then(|(bus, p)| cx.key_of_ref(bus, p));
        parent_ref = parent;
        have |= PropMask::PARENT;
    }
    if let Ok(Some(n)) = count {
        raw.child_count = Some(clip_child_count(i64::from(n)));
        have |= PropMask::CHILDREN;
    }
    if want.contains(PropMask::NAME)
        && states.contains(StateSet::INVALID)
        && gorca_doc_source_api::takes_error_text(core_role)
    {
        // M8b: the error message of a focused invalid control is read with the focus, however
        // many other invalid controls the page has (the document fetch bounds its own reads).
        raw.error_text = c.error_text_of(path, dl).await;
    }
    if want.contains(PropMask::VALUE) {
        raw.value = value_for(cx, core_role, states, path, dl).await;
        have |= PropMask::VALUE;
    }
    if raw.name.as_deref().is_none_or(str::is_empty)
        && want.contains(PropMask::NAME)
        && name_ok
        && source_for(core_role, states) != Source::Secret
    {
        have |= PropMask::RELATIONS;
        if let Some(label) = name_fallback(c, path, dl).await {
            raw.name = Some(label);
        }
    }
    raw.have = have;
    Ok((raw, parent_ref))
}

/// Visited-node budget of the descendant label search (breadth first).
pub(super) const LABEL_SEARCH_NODES: u32 = 8;
/// Depth of the descendant label search below the unnamed node.
const LABEL_SEARCH_DEPTH: usize = 2;

/// Name of a node that has none, by accname-like rules available over AT-SPI: the first
/// `LabelledBy` target's name (then its text), else the names (or text) of its descendants,
/// bounded in depth and count, all within the caller's deadline. Any failure only costs the
/// fallback.
async fn name_fallback(c: &Caller, path: &str, dl: Deadline) -> Option<String> {
    if let Ok(Some((bus, p))) = c.label_target(path, dl).await
        && label_may_be_read(&bus, c.bus_name())
        && let Some(kind) = classify(c, &p, dl).await
        && kind != Source::Secret
    {
        if let Ok(label) = c.name_of(&bus, &p, dl).await
            && !label.trim().is_empty()
        {
            return Some(label);
        }
        if kind == Source::Label
            && let Ok(t) = c.text_prefix(&p, dl).await
            && !t.trim().is_empty()
        {
            return Some(clip(&t, MAX_TEXT_BYTES).to_owned());
        }
    }
    descendant_label(c, path, dl).await
}

/// What a node may contribute to a name it does not own.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Source {
    /// A password field, or a node that could not be classified: neither its name nor its text
    /// is read (fail closed; secrets may only be spoken by typed echo, never become a name).
    Secret,
    /// A label-like, non-editable node: its name, else its text.
    Label,
    /// Anything else: its name only, never its text (an entry, a terminal, a document).
    NameOnly,
}

fn source_for(role: Role, states: StateSet) -> Source {
    match role {
        Role::PasswordEntry => Source::Secret,
        Role::Label | Role::StaticText if !states.contains(StateSet::EDITABLE) => Source::Label,
        _ => Source::NameOnly,
    }
}

/// Role and state of `path` decide what it may contribute; a failed read is `None` (skip).
async fn classify(c: &Caller, path: &str, dl: Deadline) -> Option<Source> {
    let (role, state) = tokio::join!(c.role(path, dl), c.state(path, dl));
    let role = crate::decode::role_from_raw(role.ok()?);
    let states = crate::decode::states_from_raw(&state.ok()?);
    Some(source_for(role, states))
}

/// Join the names of the descendants of `path` (breadth first, at most
/// [`LABEL_SEARCH_NODES`] visited, [`LABEL_SEARCH_DEPTH`] deep). A label-like leaf without a
/// name contributes its text; secret nodes are skipped whole. A named node is taken whole: its
/// own children are not searched.
async fn descendant_label(c: &Caller, path: &str, dl: Deadline) -> Option<String> {
    let mut queue: std::collections::VecDeque<(String, usize)> = std::collections::VecDeque::new();
    queue.push_back((path.to_owned(), 0));
    let mut visited = 0u32;
    let mut parts: Vec<String> = Vec::new();
    while let Some((p, depth)) = queue.pop_front() {
        if depth >= LABEL_SEARCH_DEPTH {
            continue;
        }
        let count = c.child_count_of(&p, dl).await.ok()?;
        let count = u32::try_from(count).unwrap_or(0).min(LABEL_SEARCH_NODES);
        for i in 0..count {
            if visited >= LABEL_SEARCH_NODES {
                break;
            }
            let Ok(Some((bus, child))) = c.child_at(&p, i, dl).await else {
                continue;
            };
            if bus != c.bus_name() {
                continue;
            }
            visited += 1;
            let Some(kind) = classify(c, &child, dl).await else {
                continue;
            };
            if kind == Source::Secret {
                continue;
            }
            if let Ok(n) = c.name_of(&bus, &child, dl).await
                && !n.trim().is_empty()
            {
                parts.push(n);
                continue;
            }
            let kids = c.child_count_of(&child, dl).await.unwrap_or(0);
            if kids > 0 {
                queue.push_back((child, depth + 1));
            } else if kind == Source::Label
                && let Ok(t) = c.text_prefix(&child, dl).await
                && !t.trim().is_empty()
            {
                parts.push(clip(&t, MAX_TEXT_BYTES).to_owned());
            }
        }
    }
    let joined = parts.join(" ");
    (!joined.trim().is_empty()).then_some(joined)
}

async fn value_for(
    cx: &CallCx,
    role: Role,
    states: StateSet,
    path: &str,
    dl: Deadline,
) -> Option<RawValue> {
    let c = &cx.caller;
    match role {
        Role::Slider | Role::ProgressBar | Role::SpinButton => {
            let (cur, min, max) = tokio::join!(
                c.f64_prop(path, "CurrentValue", dl),
                c.f64_prop(path, "MinimumValue", dl),
                c.f64_prop(path, "MaximumValue", dl)
            );
            Some(RawValue::Number {
                current: cur.ok()?,
                min: min.unwrap_or(f64::NAN),
                max: max.unwrap_or(f64::NAN),
            })
        }
        // Product decision: gorca reads whatever the application exposes, password fields
        // included. The text is never logged.
        Role::TextEntry | Role::PasswordEntry if wants_entry_text(role, states) => {
            let t = c.text_prefix(path, dl).await.ok()?;
            Some(RawValue::Text(clip(&t, MAX_TEXT_BYTES).to_owned()))
        }
        // Everything else has no value on focus.
        Role::PasswordEntry
        | Role::TextEntry
        | Role::Unknown
        | Role::Application
        | Role::Window
        | Role::Dialog
        | Role::Panel
        | Role::Document
        | Role::DocumentWeb
        | Role::Heading
        | Role::Paragraph
        | Role::Link
        | Role::Image
        | Role::List
        | Role::ListItem
        | Role::Table
        | Role::Row
        | Role::Cell
        | Role::ColumnHeader
        | Role::RowHeader
        | Role::Button
        | Role::ToggleButton
        | Role::CheckBox
        | Role::RadioButton
        | Role::ComboBox
        | Role::Menu
        | Role::MenuBar
        | Role::MenuItem
        | Role::Popover
        | Role::Tab
        | Role::TabList
        | Role::Tree
        | Role::TreeItem
        | Role::Toolbar
        | Role::StatusBar
        | Role::Separator
        | Role::Label
        | Role::StaticText
        | Role::Terminal
        | Role::Landmark
        | Role::Alert => None,
    }
}

/// Children fetched at the same time inside one op. Each fetch pipelines up to six calls on the
/// app's connection and the app serves them one at a time, so a small bound keeps the Interactive
/// share of the app's serial loop short.
const CHILD_CONCURRENCY: usize = 3;
/// A child range up to this size is read with one `GetChildAtIndex` per child, all in flight at
/// once; a larger one with a single `GetChildren`.
const INDEXED_RANGE: u32 = 8;

/// Run `f` over `items` with at most `conc` in flight, results in input order. A task that
/// panicked or was cancelled yields `None`. The tasks belong to this future: dropping it (a
/// deadline, a cancelled op) aborts them.
async fn map_concurrent<T, R, F, Fut>(items: Vec<T>, conc: usize, f: F) -> Vec<Option<R>>
where
    T: Send + 'static,
    R: Send + 'static,
    F: Fn(T) -> Fut,
    Fut: std::future::Future<Output = R> + Send + 'static,
{
    let mut out: Vec<Option<R>> = Vec::with_capacity(items.len());
    out.resize_with(items.len(), || None);
    let mut set = tokio::task::JoinSet::new();
    let mut iter = items.into_iter().enumerate();
    loop {
        while set.len() < conc.max(1) {
            let Some((i, item)) = iter.next() else { break };
            let fut = f(item);
            set.spawn(async move { (i, fut.await) });
        }
        let Some(done) = set.join_next().await else {
            break;
        };
        if let Ok((i, r)) = done
            && let Some(slot) = out.get_mut(i)
        {
            *slot = Some(r);
        }
    }
    out
}

/// The refs of the children of `node` in `start..end`, in order. Fewer when the list is shorter.
async fn child_refs(
    cx: &CallCx,
    node: &Resolved,
    start: u32,
    end: u32,
    dl: Deadline,
) -> Result<Vec<(String, String)>, BackendError> {
    if end.saturating_sub(start) > INDEXED_RANGE {
        let all = cx
            .caller
            .children_of(&node.path, dl)
            .await
            .map_err(|e| backend_error(&e))?;
        // `GetChildren` is clipped at the subtree cap: a range beyond the clip is read by index.
        if (start as usize) < all.len() || all.len() < MAX_SUBTREE_NODES as usize {
            return Ok(all
                .into_iter()
                .skip(start as usize)
                .take((end - start) as usize)
                .collect());
        }
    }
    let results = {
        let c = cx.caller.clone();
        let path = Arc::clone(&node.path);
        map_concurrent(
            (start..end).collect(),
            INDEXED_RANGE as usize,
            move |i: u32| {
                let c = c.clone();
                let path = Arc::clone(&path);
                async move { c.child_at(&path, i, dl).await }
            },
        )
        .await
    };
    let mut refs = Vec::new();
    for (n, r) in results.into_iter().enumerate() {
        match r {
            Some(Ok(Some(r))) => refs.push(r),
            // Past the last child: the range was larger than the list.
            Some(Err(e)) if n == 0 => return Err(backend_error(&e)),
            Some(Ok(None) | Err(_)) | None => break,
        }
    }
    Ok(refs)
}

/// Fetch `refs` (children of `parent`) with `want`, a few at a time; the parent link is the
/// walk's own (no `Parent` call per child). Returns the nodes in order and whether any was lost
/// to a failure.
async fn fetch_children(
    cx: &CallCx,
    parent: &Resolved,
    refs: Vec<Resolved>,
    want: PropMask,
    dl: Deadline,
) -> (Vec<(Resolved, RawNode)>, bool, Option<BackendError>) {
    let results = {
        let cx2 = cx.clone();
        let parent_key = parent.key;
        let shared: Vec<Resolved> = refs.clone();
        map_concurrent(shared, CHILD_CONCURRENCY, move |child: Resolved| {
            let cx = cx2.clone();
            async move {
                fetch_node(&cx, &child, want, Some(parent_key), dl)
                    .await
                    .map(|(raw, _)| raw)
            }
        })
        .await
    };
    let mut nodes = Vec::with_capacity(refs.len());
    let mut lost = false;
    let mut first_err = None;
    for (child, r) in refs.into_iter().zip(results) {
        match r {
            Some(Ok(raw)) => nodes.push((child, raw)),
            // Gone in the meantime: not an error of the op.
            Some(Err(BackendError::Gone | BackendError::Unresolved)) => {}
            Some(Err(e)) => {
                lost = true;
                first_err.get_or_insert(e);
            }
            None => lost = true,
        }
    }
    (nodes, lost, first_err)
}

/// The children of `node` in `range`, each fetched with `want` plus its parent link.
pub(crate) async fn child_snapshots(
    cx: &CallCx,
    node: &Resolved,
    range: Range<u32>,
    want: PropMask,
    dl: Deadline,
) -> Result<RawBatch, BackendError> {
    let end = range.end.min(range.start.saturating_add(MAX_CHILD_RANGE));
    let mut batch = RawBatch {
        truncated: end < range.end,
        ..RawBatch::default()
    };
    if range.start >= end {
        return Ok(batch);
    }
    let mut kids = Vec::new();
    for (bus, p) in child_refs(cx, node, range.start, end, dl).await? {
        match cx.resolve_ref(&bus, &p) {
            Some(child) => kids.push(child),
            None => cx.counters.foreign_refs(),
        }
    }
    let (nodes, lost, err) = fetch_children(cx, node, kids, want, dl).await;
    if nodes.is_empty()
        && let Some(e) = err
    {
        return Err(e);
    }
    batch.truncated |= lost;
    batch.nodes = nodes.into_iter().map(|(c, raw)| (c.key, raw)).collect();
    Ok(batch)
}

/// A parent's child list longer than this (bytes of the `GetChildren` reply, about 4 000 refs) is
/// not read whole to find a window of it.
const SIBLING_LIST_MAX_BYTES: usize = 512 * 1024;

/// The siblings of `node` (children of `parent`): the `before` entries ahead of it, the node
/// itself and the `after` entries behind it, each fetched with `want` and the walk's parent link.
///
/// Normally the node's index comes out of the one `GetChildren` reply that lists the parent's
/// children, so finding it costs no probing; a reply above [`SIBLING_LIST_MAX_BYTES`] is refused
/// before it is decoded (`Protocol`). With `indexed` (the caller knows the list is long) nothing
/// is listed: `GetIndexInParent` of the node, then `GetChildAtIndex` for the window (at most
/// [`INDEXED_RANGE`] entries), so a 300 000-row list costs the same as a short one. A node that
/// is not in the list (it moved or went away) or sits beyond the `GetChildren` clip yields an
/// empty batch, not an error.
pub(crate) async fn siblings(
    cx: &CallCx,
    parent: &Resolved,
    node: &Resolved,
    window: (u32, u32),
    indexed: bool,
    want: PropMask,
    dl: Deadline,
) -> Result<RawBatch, BackendError> {
    let (before, after) = window;
    let mut batch = RawBatch::default();
    let refs: Vec<(String, String)>;
    let at: usize;
    if indexed {
        let idx = cx
            .caller
            .index_in_parent(&node.path, dl)
            .await
            .map_err(|e| backend_error(&e))?;
        let Ok(idx) = u32::try_from(idx) else {
            return Ok(batch);
        };
        let start = idx.saturating_sub(before);
        let end = idx
            .saturating_add(after)
            .saturating_add(1)
            .min(start.saturating_add(INDEXED_RANGE));
        refs = child_refs(cx, parent, start, end, dl).await?;
        at = (idx - start) as usize;
        // The node must be where the app says it is.
        if refs.get(at).is_none_or(|(_, p)| p.as_str() != &*node.path) {
            return Ok(batch);
        }
        batch.truncated = false;
        return finish_siblings(cx, parent, &refs, 0..refs.len(), want, dl, batch).await;
    }
    let all = cx
        .caller
        .children_of_capped(&parent.path, dl, SIBLING_LIST_MAX_BYTES)
        .await
        .map_err(|e| backend_error(&e))?;
    let Some(pos) = all.iter().position(|(_, p)| p.as_str() == &*node.path) else {
        return Ok(batch);
    };
    let start = pos.saturating_sub(before as usize);
    let end = pos
        .saturating_add(after as usize)
        .saturating_add(1)
        .min(all.len());
    batch.truncated = end >= MAX_SUBTREE_NODES as usize && all.len() >= MAX_SUBTREE_NODES as usize;
    finish_siblings(cx, parent, &all, start..end, want, dl, batch).await
}

async fn finish_siblings(
    cx: &CallCx,
    parent: &Resolved,
    refs: &[(String, String)],
    range: Range<usize>,
    want: PropMask,
    dl: Deadline,
    mut batch: RawBatch,
) -> Result<RawBatch, BackendError> {
    let mut kids = Vec::with_capacity(range.len());
    for (bus, p) in refs.get(range).unwrap_or(&[]) {
        match cx.resolve_ref(bus, p) {
            Some(child) => kids.push(child),
            None => cx.counters.foreign_refs(),
        }
    }
    let (nodes, lost, err) = fetch_children(cx, parent, kids, want, dl).await;
    if nodes.is_empty()
        && let Some(e) = err
    {
        return Err(e);
    }
    batch.truncated |= lost;
    batch.nodes = nodes.into_iter().map(|(c, raw)| (c.key, raw)).collect();
    Ok(batch)
}

/// `node`, its parent, ... up to `max_depth` or the app root. Stops (truncated) at a cycle, a
/// foreign reference or a `Document` role; the document node itself is not included. The parent
/// reference comes from the fetch of each level: one round of calls per level, none extra.
pub(crate) async fn ancestry(
    cx: &CallCx,
    node: &Resolved,
    max_depth: u8,
    want: PropMask,
    dl: Deadline,
) -> Result<RawBatch, BackendError> {
    let depth = max_depth.min(MAX_ANCESTRY_DEPTH);
    let mut batch = RawBatch::default();
    let mut seen: HashSet<Arc<str>> = HashSet::new();
    let mut cur = node.clone();
    for level in 0..=usize::from(depth) {
        if !seen.insert(Arc::clone(&cur.path)) {
            batch.truncated = true;
            break;
        }
        let (raw, parent_ref) = match fetch_node(cx, &cur, want | PropMask::PARENT, None, dl).await
        {
            Ok(r) => r,
            Err(e) if batch.nodes.is_empty() => return Err(e),
            Err(_) => {
                batch.truncated = true;
                break;
            }
        };
        let role = crate::decode::role_from_raw(raw.role);
        if matches!(role, Role::Document | Role::DocumentWeb) && level > 0 {
            batch.truncated = true;
            break;
        }
        // The application root is the top: its own parent is the desktop, on another bus name,
        // which is the normal end of the chain and not a foreign reference.
        let parent_path = if role == Role::Application {
            None
        } else {
            parent_ref
        };
        batch.nodes.push((cur.key, raw));
        let Some((bus, p)) = parent_path else { break };
        if p == NULL_PATH || p.is_empty() {
            break;
        }
        if let Some(next) = cx.resolve_ref(&bus, &p) {
            cur = next;
        } else {
            cx.counters.foreign_refs();
            batch.truncated = true;
            break;
        }
        if level == usize::from(depth) {
            batch.truncated = true;
        }
    }
    Ok(batch)
}

/// Breadth-first walk below `root`, at most `max_nodes` nodes, never into a `Document`. The
/// children of one node are fetched a few at a time.
pub(crate) async fn subtree(
    cx: &CallCx,
    root: &Resolved,
    max_nodes: u32,
    dl: Deadline,
) -> Result<RawBatch, BackendError> {
    const WANT: PropMask = PropMask::PRESENT
        .union(PropMask::CHILDREN)
        .union(PropMask::PARENT);
    let cap = max_nodes.min(MAX_SUBTREE_NODES) as usize;
    let mut batch = RawBatch::default();
    let first = fetch(cx, root, WANT, dl).await?;
    let mut queue: std::collections::VecDeque<Resolved> = std::collections::VecDeque::new();
    if !matches!(
        crate::decode::role_from_raw(first.role),
        Role::Document | Role::DocumentWeb
    ) {
        queue.push_back(root.clone());
    }
    batch.nodes.push((root.key, first));
    // An object listed twice (a hostile or buggy app: self reference, cycle) is walked once.
    let mut seen: HashSet<Arc<str>> = HashSet::new();
    seen.insert(Arc::clone(&root.path));
    while let Some(parent) = queue.pop_front() {
        if batch.nodes.len() >= cap || dl.expired(std::time::Instant::now()) {
            batch.truncated = true;
            break;
        }
        let Ok(kids) = cx.caller.children_of(&parent.path, dl).await else {
            batch.truncated = true;
            continue;
        };
        let mut refs: Vec<Resolved> = Vec::new();
        for (bus, p) in kids {
            if batch.nodes.len() + refs.len() >= cap {
                batch.truncated = true;
                break;
            }
            match cx.resolve_ref(&bus, &p) {
                Some(child) => {
                    if seen.insert(Arc::clone(&child.path)) {
                        refs.push(child);
                    }
                }
                None => cx.counters.foreign_refs(),
            }
        }
        let (nodes, lost, _) = fetch_children(cx, &parent, refs, WANT, dl).await;
        batch.truncated |= lost;
        let mut keys: SmallVec<[gorca_model::NodeKey; 8]> = SmallVec::new();
        for (child, raw) in nodes {
            keys.push(child.key);
            let is_doc = matches!(
                crate::decode::role_from_raw(raw.role),
                Role::Document | Role::DocumentWeb
            );
            batch.nodes.push((child.key, raw));
            if !is_doc {
                queue.push_back(child);
            }
        }
        if let Some((_, raw)) = batch.nodes.iter_mut().find(|(k, _)| *k == parent.key) {
            raw.children = Some(keys);
        }
    }
    Ok(batch)
}

/// `Cache.GetItems`, decoded off the I/O threads.
pub(crate) async fn cache_dump(
    cx: &CallCx,
    max_nodes: u32,
    dl: Deadline,
) -> Result<RawBatch, BackendError> {
    let msg = cx
        .caller
        .get_items(dl)
        .await
        .map_err(|e| backend_error(&e))?;
    let body = msg.body();
    let data = body.data();
    let bytes_len = data.len();
    if bytes_len > MAX_DUMP_BYTES {
        cx.counters.dump_rejected();
        return Err(BackendError::Protocol(format!(
            "GetItems reply of {bytes_len} bytes exceeds the cap"
        )));
    }
    let endian = data.context().endian();
    let bytes: Vec<u8> = data.to_vec();
    let (app, bus) = (cx.app, cx.caller.bus_name().to_owned());
    let paths = std::sync::Arc::clone(&cx.paths);
    let cap = max_nodes as usize;
    let job = tokio::task::spawn_blocking(move || {
        let d = items::decode_get_items(&bytes, endian, app, &bus, cap)?;
        for (key, path, _) in &d.nodes {
            paths.insert(*key, path);
        }
        Ok::<_, items::ItemsError>(d)
    });
    let decoded = match gorca_rt::with_deadline(dl, job).await {
        Ok(Ok(Ok(d))) => d,
        Ok(Ok(Err(e))) => {
            cx.counters.dump_rejected();
            return Err(BackendError::Protocol(e.to_string()));
        }
        Ok(Err(_join)) => return Err(BackendError::Protocol("dump decoder failed".to_owned())),
        Err(_) => return Err(BackendError::Timeout),
    };
    cx.counters
        .dump_items(decoded.stats.foreign_items + decoded.stats.foreign_parents);
    Ok(RawBatch {
        nodes: decoded.nodes.into_iter().map(|(k, _, r)| (k, r)).collect(),
        truncated: decoded.truncated,
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod source_tests {
    use super::*;

    #[test]
    fn only_non_editable_labels_may_lend_text_and_passwords_lend_nothing() {
        let none = StateSet::empty();
        assert_eq!(source_for(Role::PasswordEntry, none), Source::Secret);
        assert_eq!(source_for(Role::Label, none), Source::Label);
        assert_eq!(source_for(Role::StaticText, none), Source::Label);
        assert_eq!(
            source_for(Role::Label, StateSet::EDITABLE),
            Source::NameOnly
        );
        for r in [Role::TextEntry, Role::Document, Role::Button, Role::Unknown] {
            assert_eq!(source_for(r, none), Source::NameOnly, "{r:?}");
        }
    }
}
