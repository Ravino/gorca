//! How each `BackendOp` is executed: which AT-SPI calls, in which order, how many at once.
//!
//! This file is the **naive reference strategy**, the one R0 uses: one sequential call chain per
//! op, no pipelining, no `Properties.GetAll`. A fill strategy may replace any function here
//! (pipelining over the app's connection, splitting large ops) as long as the signatures hold,
//! every call carries the op's deadline (D-4), it never exceeds the op's share of `MAX_IN_FLIGHT`,
//! and everything it learns is app-supplied and untrusted (refs to other bus names are dropped).

use std::collections::HashSet;
use std::ops::Range;

use gorca_model::{PropMask, Role, StateSet};
use gorca_rt::Deadline;
use smallvec::SmallVec;

use super::items::{self, MAX_DUMP_BYTES};
use super::{
    CallCx, MAX_ANCESTRY_DEPTH, MAX_CHILD_RANGE, MAX_SUBTREE_NODES, RawBatch, Resolved,
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
        if want.contains(PropMask::PARENT) {
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
    if let Ok(Some(parent)) = parent {
        raw.parent = parent.and_then(|(bus, p)| cx.key_of_ref(&bus, &p));
        have |= PropMask::PARENT;
    }
    if let Ok(Some(n)) = count {
        raw.child_count = Some(clip_child_count(i64::from(n)));
        have |= PropMask::CHILDREN;
    }
    if want.contains(PropMask::VALUE) {
        raw.value = value_for(cx, core_role, states, path, dl).await;
        have |= PropMask::VALUE;
    }
    if raw.name.as_deref().is_none_or(str::is_empty) && want.contains(PropMask::NAME) && name_ok {
        have |= PropMask::RELATIONS;
        if let Ok(Some((bus, p))) = c.label_target(path, dl).await
            && label_may_be_read(&bus, c.bus_name())
            && let Ok(label) = c.name_of(&bus, &p, dl).await
        {
            raw.name = Some(label);
        }
    }
    raw.have = have;
    Ok(raw)
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
    for i in range.start..end {
        if dl.expired(std::time::Instant::now()) {
            batch.truncated = true;
            break;
        }
        let r = match cx.caller.child_at(&node.path, i, dl).await {
            Ok(Some((bus, p))) => cx.resolve_ref(&bus, &p),
            // Past the last child: the range was larger than the list.
            Ok(None) => break,
            Err(e) if batch.nodes.is_empty() => return Err(backend_error(&e)),
            Err(_) => {
                batch.truncated = true;
                break;
            }
        };
        let Some(child) = r else {
            cx.counters.foreign_refs();
            continue;
        };
        match fetch(cx, &child, want | PropMask::PARENT, dl).await {
            Ok(mut raw) => {
                raw.parent = Some(node.key);
                batch.nodes.push((child.key, raw));
            }
            Err(BackendError::Gone | BackendError::Unresolved) => {}
            Err(e) if batch.nodes.is_empty() => return Err(e),
            Err(_) => {
                batch.truncated = true;
                break;
            }
        }
    }
    Ok(batch)
}

/// `node`, its parent, ... up to `max_depth` or the app root. Stops (truncated) at a cycle, a
/// foreign reference or a `Document` role; the document node itself is not included.
pub(crate) async fn ancestry(
    cx: &CallCx,
    node: &Resolved,
    max_depth: u8,
    want: PropMask,
    dl: Deadline,
) -> Result<RawBatch, BackendError> {
    let depth = max_depth.min(MAX_ANCESTRY_DEPTH);
    let mut batch = RawBatch::default();
    let mut seen: HashSet<std::sync::Arc<str>> = HashSet::new();
    let mut cur = node.clone();
    for level in 0..=usize::from(depth) {
        if !seen.insert(std::sync::Arc::clone(&cur.path)) {
            batch.truncated = true;
            break;
        }
        let raw = match fetch(cx, &cur, want | PropMask::PARENT, dl).await {
            Ok(r) => r,
            Err(e) if batch.nodes.is_empty() => return Err(e),
            Err(_) => {
                batch.truncated = true;
                break;
            }
        };
        if matches!(
            crate::decode::role_from_raw(raw.role),
            Role::Document | Role::DocumentWeb
        ) && level > 0
        {
            batch.truncated = true;
            break;
        }
        // The application root is the top: its own parent is the desktop, on another bus name,
        // which is the normal end of the chain and not a foreign reference.
        let at_root = crate::decode::role_from_raw(raw.role) == Role::Application;
        let parent_path = if at_root {
            None
        } else {
            cx.caller.parent_of(&cur.path, dl).await.unwrap_or_default()
        };
        let key = cur.key;
        batch.nodes.push((key, raw));
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

/// Breadth-first walk below `root`, at most `max_nodes` nodes, never into a `Document`.
pub(crate) async fn subtree(
    cx: &CallCx,
    root: &Resolved,
    max_nodes: u32,
    dl: Deadline,
) -> Result<RawBatch, BackendError> {
    let cap = max_nodes.min(MAX_SUBTREE_NODES) as usize;
    let mut batch = RawBatch::default();
    let first = fetch(
        cx,
        root,
        PropMask::PRESENT | PropMask::CHILDREN | PropMask::PARENT,
        dl,
    )
    .await?;
    let mut queue: std::collections::VecDeque<Resolved> = std::collections::VecDeque::new();
    if !matches!(
        crate::decode::role_from_raw(first.role),
        Role::Document | Role::DocumentWeb
    ) {
        queue.push_back(root.clone());
    }
    batch.nodes.push((root.key, first));
    while let Some(parent) = queue.pop_front() {
        if batch.nodes.len() >= cap || dl.expired(std::time::Instant::now()) {
            batch.truncated = true;
            break;
        }
        let Ok(kids) = cx.caller.children_of(&parent.path, dl).await else {
            batch.truncated = true;
            continue;
        };
        let mut keys: SmallVec<[gorca_model::NodeKey; 8]> = SmallVec::new();
        for (bus, p) in kids {
            if batch.nodes.len() >= cap {
                batch.truncated = true;
                break;
            }
            let Some(child) = cx.resolve_ref(&bus, &p) else {
                cx.counters.foreign_refs();
                continue;
            };
            match fetch(
                cx,
                &child,
                PropMask::PRESENT | PropMask::CHILDREN | PropMask::PARENT,
                dl,
            )
            .await
            {
                Ok(mut raw) => {
                    raw.parent = Some(parent.key);
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
                Err(_) => batch.truncated = true,
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
