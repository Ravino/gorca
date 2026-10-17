//! Decoder of `org.a11y.atspi.Cache.GetItems` replies (B-1 PW-9, audit MA-7).
//!
//! The reply is **untrusted input of up to 16 MiB**. This decoder never panics, bounds everything
//! it allocates by the section 7 caps, and keeps the app inside its own mirror:
//!
//! - a reply larger than [`MAX_DUMP_BYTES`] is rejected whole (`TooLarge`);
//! - at most `max_nodes` items are decoded (the rest is dropped, `truncated`);
//! - a ref that names another bus name is dropped and counted (an item with a foreign object
//!   ref is skipped; a foreign parent leaves the node parentless);
//! - self-parents and parent cycles are cut and counted;
//! - names and descriptions are clipped, child counts are clipped to `[0, MAX_CHILD_COUNT]`;
//! - `GetItems` order is a hash order (children may precede parents, P9-6), so the tree is built
//!   in two passes by parent ref; a parent whose recorded children do not add up to its child
//!   count keeps only the count.
//!
//! Decoding is pure CPU on bytes, so callers run it off the I/O threads (`spawn_blocking`).

use std::collections::HashMap;

use gorca_dbus::drain::local_id;
use gorca_model::{AppId, Ifaces, NodeKey, PropMask};
use serde::Deserialize;
use smallvec::SmallVec;
use zbus::zvariant::serialized::{Context, Data};
use zbus::zvariant::{Endian, Signature, Type};

use crate::decode::{MAX_CHILD_COUNT, MAX_TEXT_BYTES, RawNode, clip, clip_child_count};

/// A reply larger than this is rejected whole.
pub const MAX_DUMP_BYTES: usize = 16 * 1024 * 1024;
/// Items decoded at most.
pub const MAX_DUMP_NODES: usize = 65_536;
/// Longest object path kept.
pub const MAX_ITEM_PATH_BYTES: usize = 256;
/// The AT-SPI null object (an absent parent).
const NULL_PATH: &str = "/org/a11y/atspi/null";

/// `o` on the wire, kept as a string here: validity is judged per item, not for the whole reply.
#[derive(Debug, Deserialize)]
struct Obj(String);

impl Type for Obj {
    const SIGNATURE: &'static Signature = &Signature::ObjectPath;
}

type Ref = (String, Obj);

/// `((so)(so)(so)iiassusau)`.
#[derive(Debug, Deserialize, Type)]
struct Item {
    object: Ref,
    _app: Ref,
    parent: Ref,
    index: i32,
    children: i32,
    interfaces: Vec<String>,
    name: String,
    role: u32,
    description: String,
    states: Vec<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ItemsError {
    #[error("reply of {0} bytes exceeds the dump cap")]
    TooLarge(usize),
    #[error("malformed GetItems reply: {0}")]
    Malformed(String),
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct DumpStats {
    /// Items seen before the node cap.
    pub items: u32,
    /// Items skipped because their object ref named another bus name or was unusable.
    pub foreign_items: u32,
    /// Parent refs dropped because they named another bus name.
    pub foreign_parents: u32,
    /// Self-parents and parent cycles that were cut.
    pub cyclic_parents: u32,
    /// Duplicate object paths (the first one wins).
    pub duplicates: u32,
}

#[derive(Debug, Default)]
pub struct Decoded {
    /// Decoded nodes with their object path (to record in the path table).
    pub nodes: Vec<(NodeKey, Box<str>, RawNode)>,
    pub truncated: bool,
    pub stats: DumpStats,
}

fn iface_bit(name: &str) -> Ifaces {
    match name {
        "org.a11y.atspi.Text" => Ifaces::TEXT,
        "org.a11y.atspi.EditableText" => Ifaces::EDITABLE_TEXT,
        "org.a11y.atspi.Hypertext" => Ifaces::HYPERTEXT,
        "org.a11y.atspi.Table" | "org.a11y.atspi.TableCell" => Ifaces::TABLE,
        "org.a11y.atspi.Action" => Ifaces::ACTION,
        "org.a11y.atspi.Selection" => Ifaces::SELECTION,
        "org.a11y.atspi.Document" => Ifaces::DOCUMENT,
        "org.a11y.atspi.Value" => Ifaces::VALUE,
        "org.a11y.atspi.Collection" => Ifaces::COLLECTION,
        _ => Ifaces::empty(),
    }
}

/// Decode `bytes`, the body of a `GetItems` reply.
pub fn decode_get_items(
    bytes: &[u8],
    endian: Endian,
    app: AppId,
    app_bus: &str,
    max_nodes: usize,
) -> Result<Decoded, ItemsError> {
    if bytes.len() > MAX_DUMP_BYTES {
        return Err(ItemsError::TooLarge(bytes.len()));
    }
    let ctx = Context::new_dbus(endian, 0);
    let data = Data::new(bytes, ctx);
    let (items, _) = data
        .deserialize::<Vec<Item>>()
        .map_err(|e| ItemsError::Malformed(e.to_string()))?;
    Ok(build(items, app, app_bus, max_nodes.min(MAX_DUMP_NODES)))
}

fn build(items: Vec<Item>, app: AppId, app_bus: &str, max_nodes: usize) -> Decoded {
    let mut out = Decoded {
        truncated: items.len() > max_nodes,
        ..Decoded::default()
    };
    out.stats.items = u32::try_from(items.len().min(max_nodes)).unwrap_or(u32::MAX);
    let mut index_of: HashMap<NodeKey, usize> = HashMap::with_capacity(items.len().min(max_nodes));
    let mut order: Vec<(NodeKey, u32)> = Vec::new();
    for it in items.into_iter().take(max_nodes) {
        let (bus, Obj(path)) = it.object;
        if bus != app_bus || path.is_empty() || path.len() > MAX_ITEM_PATH_BYTES {
            out.stats.foreign_items += 1;
            continue;
        }
        let key = NodeKey {
            app,
            local: local_id(&path),
        };
        if index_of.contains_key(&key) {
            out.stats.duplicates += 1;
            continue;
        }
        let (pbus, Obj(ppath)) = it.parent;
        let parent = if ppath.is_empty() || ppath == NULL_PATH || pbus.is_empty() {
            None
        } else if pbus != app_bus || ppath.len() > MAX_ITEM_PATH_BYTES {
            out.stats.foreign_parents += 1;
            None
        } else {
            let pk = NodeKey {
                app,
                local: local_id(&ppath),
            };
            if pk == key {
                out.stats.cyclic_parents += 1;
                None
            } else {
                Some(pk)
            }
        };
        let interfaces = it
            .interfaces
            .iter()
            .fold(Ifaces::empty(), |a, n| a | iface_bit(n));
        let raw = RawNode {
            role: it.role,
            state_words: it.states.into_iter().take(2).collect(),
            name: Some(clip(&it.name, MAX_TEXT_BYTES).to_owned()),
            description: Some(clip(&it.description, MAX_TEXT_BYTES).to_owned()),
            error_text: None,
            value: None,
            parent,
            child_count: Some(clip_child_count(i64::from(it.children))),
            children: None,
            interfaces,
            have: PropMask::CACHE_ITEM,
        };
        index_of.insert(key, out.nodes.len());
        order.push((key, u32::try_from(it.index).unwrap_or(u32::MAX)));
        out.nodes.push((key, path.into_boxed_str(), raw));
    }
    cut_cycles(&mut out, &index_of);
    link_children(&mut out, &index_of, &order);
    out
}

/// Parent chains that loop are cut at the node where the loop is detected.
fn cut_cycles(out: &mut Decoded, index_of: &HashMap<NodeKey, usize>) {
    // 0 = unvisited, 1 = on the current chain, 2 = known to reach a root or a foreign parent.
    let mut state = vec![0u8; out.nodes.len()];
    let mut chain: Vec<usize> = Vec::new();
    for start in 0..out.nodes.len() {
        if state[start] != 0 {
            continue;
        }
        chain.clear();
        let mut cur = start;
        loop {
            state[cur] = 1;
            chain.push(cur);
            let next = out.nodes[cur]
                .2
                .parent
                .and_then(|p| index_of.get(&p).copied());
            match next {
                None => break,
                Some(n) if state[n] == 2 => break,
                Some(n) if state[n] == 1 => {
                    // `cur -> n` closes a loop: cut the edge.
                    out.nodes[cur].2.parent = None;
                    out.stats.cyclic_parents += 1;
                    break;
                }
                Some(n) => cur = n,
            }
        }
        for &i in &chain {
            state[i] = 2;
        }
    }
}

fn link_children(out: &mut Decoded, index_of: &HashMap<NodeKey, usize>, order: &[(NodeKey, u32)]) {
    let mut by_parent: HashMap<usize, Vec<(u32, NodeKey)>> = HashMap::new();
    for (i, (key, idx)) in order.iter().enumerate() {
        if let Some(p) = out.nodes[i]
            .2
            .parent
            .and_then(|p| index_of.get(&p).copied())
        {
            by_parent.entry(p).or_default().push((*idx, *key));
        }
    }
    for (p, mut kids) in by_parent {
        kids.sort_unstable();
        let node = &mut out.nodes[p].2;
        let count = node.child_count.unwrap_or(0) as usize;
        // A complete, consistent list is worth keeping; anything else stays a count.
        if kids.len() == count && count <= MAX_CHILD_COUNT as usize {
            let list: SmallVec<[NodeKey; 8]> = kids.into_iter().map(|(_, k)| k).collect();
            node.children = Some(list);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::range_plus_one
    )]
    use super::*;
    use gorca_model::ConnectorId;
    use zbus::zvariant::{ObjectPath, to_bytes};

    const BUS: &str = ":1.7";

    fn app() -> AppId {
        AppId {
            connector: ConnectorId(1),
            serial: 1,
        }
    }

    type WireItem<'a> = (
        (&'a str, ObjectPath<'a>),
        (&'a str, ObjectPath<'a>),
        (&'a str, ObjectPath<'a>),
        i32,
        i32,
        Vec<&'a str>,
        &'a str,
        u32,
        &'a str,
        Vec<u32>,
    );

    #[allow(clippy::too_many_arguments)]
    fn item<'a>(
        bus: &'a str,
        path: &'a str,
        parent_bus: &'a str,
        parent: &'a str,
        index: i32,
        children: i32,
        name: &'a str,
        role: u32,
    ) -> WireItem<'a> {
        (
            (bus, ObjectPath::try_from(path).unwrap()),
            (
                bus,
                ObjectPath::try_from("/org/a11y/atspi/accessible/root").unwrap(),
            ),
            (parent_bus, ObjectPath::try_from(parent).unwrap()),
            index,
            children,
            vec!["org.a11y.atspi.Accessible", "org.a11y.atspi.Value"],
            name,
            role,
            "",
            vec![0, 0],
        )
    }

    fn encode(items: &[WireItem<'_>]) -> Vec<u8> {
        let ctx = Context::new_dbus(Endian::Little, 0);
        to_bytes(ctx, &items.to_vec()).unwrap().to_vec()
    }

    fn run(items: &[WireItem<'_>], max: usize) -> Decoded {
        decode_get_items(&encode(items), Endian::Little, app(), BUS, max).unwrap()
    }

    #[test]
    fn a_dump_in_hash_order_becomes_a_tree() {
        let items = [
            item(BUS, "/c/2", BUS, "/c/root", 1, 0, "second", 43),
            item(
                BUS,
                "/c/root",
                BUS,
                "/org/a11y/atspi/null",
                0,
                2,
                "root",
                75,
            ),
            item(BUS, "/c/1", BUS, "/c/root", 0, 0, "first", 43),
        ];
        let d = run(&items, 100);
        assert_eq!(d.nodes.len(), 3);
        assert!(!d.truncated);
        let find = |p: &str| {
            d.nodes
                .iter()
                .find(|(_, path, _)| &**path == p)
                .map(|(k, _, r)| (*k, r))
                .unwrap()
        };
        let (rk, root) = find("/c/root");
        assert!(root.parent.is_none());
        assert_eq!(root.child_count, Some(2));
        let kids = root.children.clone().unwrap();
        assert_eq!(
            kids.as_slice(),
            [find("/c/1").0, find("/c/2").0],
            "by index"
        );
        assert_eq!(find("/c/1").1.parent, Some(rk));
        assert_eq!(root.have, PropMask::CACHE_ITEM);
        assert!(root.interfaces.contains(Ifaces::VALUE));
    }

    #[test]
    fn an_incomplete_child_list_stays_a_count() {
        let items = [
            item(
                BUS,
                "/c/root",
                BUS,
                "/org/a11y/atspi/null",
                0,
                5,
                "root",
                75,
            ),
            item(BUS, "/c/1", BUS, "/c/root", 0, 0, "first", 43),
        ];
        let d = run(&items, 100);
        let root = d.nodes.iter().find(|(_, p, _)| &**p == "/c/root").unwrap();
        assert!(root.2.children.is_none());
        assert_eq!(root.2.child_count, Some(5));
    }

    #[test]
    fn foreign_refs_are_dropped_and_counted() {
        let items = [
            item(":1.99", "/evil/1", BUS, "/c/root", 0, 0, "evil", 43),
            item(BUS, "/c/1", ":1.99", "/victim/parent", 0, 0, "ok", 43),
        ];
        let d = run(&items, 100);
        assert_eq!(d.nodes.len(), 1);
        assert_eq!(d.stats.foreign_items, 1);
        assert_eq!(d.stats.foreign_parents, 1);
        assert!(d.nodes[0].2.parent.is_none());
    }

    #[test]
    fn self_parents_and_cycles_are_cut() {
        let items = [
            item(BUS, "/c/a", BUS, "/c/a", 0, 0, "self", 43),
            item(BUS, "/c/b", BUS, "/c/c", 0, 1, "b", 43),
            item(BUS, "/c/c", BUS, "/c/d", 0, 1, "c", 43),
            item(BUS, "/c/d", BUS, "/c/b", 0, 1, "d", 43),
        ];
        let d = run(&items, 100);
        assert_eq!(d.nodes.len(), 4);
        assert_eq!(d.stats.cyclic_parents, 2);
        // Walking parents from any node terminates.
        let parent_of: HashMap<NodeKey, Option<NodeKey>> =
            d.nodes.iter().map(|(k, _, r)| (*k, r.parent)).collect();
        for (k, _, _) in &d.nodes {
            let mut cur = Some(*k);
            for _ in 0..10 {
                cur = cur.and_then(|c| parent_of.get(&c).copied().flatten());
            }
            assert!(cur.is_none(), "chain from {k:?} does not terminate");
        }
    }

    #[test]
    fn counts_text_and_paths_are_bounded() {
        let long = "n".repeat(100_000);
        let long_path = format!("/p/{}", "x".repeat(MAX_ITEM_PATH_BYTES));
        let items = [
            item(BUS, "/c/1", BUS, "/org/a11y/atspi/null", 0, -5, &long, 43),
            item(
                BUS,
                "/c/2",
                BUS,
                "/org/a11y/atspi/null",
                0,
                i32::MAX,
                "x",
                43,
            ),
            item(BUS, &long_path, BUS, "/org/a11y/atspi/null", 0, 0, "x", 43),
        ];
        let d = run(&items, 100);
        assert_eq!(d.nodes.len(), 2, "the oversized path is skipped");
        assert_eq!(
            d.nodes[0].2.name.as_deref().map(str::len),
            Some(MAX_TEXT_BYTES)
        );
        assert_eq!(d.nodes[0].2.child_count, Some(0), "negative means zero");
        assert_eq!(d.nodes[1].2.child_count, Some(MAX_CHILD_COUNT));
    }

    #[test]
    fn the_node_cap_truncates() {
        let paths: Vec<String> = (0..50).map(|i| format!("/c/{i}")).collect();
        let items: Vec<_> = paths
            .iter()
            .map(|p| item(BUS, p, BUS, "/org/a11y/atspi/null", 0, 0, "n", 43))
            .collect();
        let d = run(&items, 10);
        assert_eq!(d.nodes.len(), 10);
        assert!(d.truncated);
    }

    #[test]
    fn duplicate_paths_keep_the_first() {
        let items = [
            item(BUS, "/c/1", BUS, "/org/a11y/atspi/null", 0, 0, "first", 43),
            item(BUS, "/c/1", BUS, "/org/a11y/atspi/null", 0, 0, "second", 43),
        ];
        let d = run(&items, 100);
        assert_eq!(d.nodes.len(), 1);
        assert_eq!(d.stats.duplicates, 1);
        assert_eq!(d.nodes[0].2.name.as_deref(), Some("first"));
    }

    #[test]
    fn an_oversized_reply_is_rejected_whole() {
        let big = vec![0u8; MAX_DUMP_BYTES + 1];
        assert_eq!(
            decode_get_items(&big, Endian::Little, app(), BUS, 10).unwrap_err(),
            ItemsError::TooLarge(MAX_DUMP_BYTES + 1)
        );
    }

    #[test]
    fn big_endian_replies_decode() {
        let ctx = Context::new_dbus(Endian::Big, 0);
        let items = vec![item(
            BUS,
            "/c/1",
            BUS,
            "/org/a11y/atspi/null",
            0,
            0,
            "be",
            43,
        )];
        let bytes = to_bytes(ctx, &items).unwrap().to_vec();
        let d = decode_get_items(&bytes, Endian::Big, app(), BUS, 10).unwrap();
        assert_eq!(d.nodes[0].2.name.as_deref(), Some("be"));
    }

    /// MA-7: arbitrary and mutated input never panics and always respects the caps.
    #[test]
    fn hostile_bytes_never_panic() {
        let paths: Vec<String> = (0..40).map(|i| format!("/c/{i}")).collect();
        let items: Vec<_> = paths
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let parent = if i == 0 {
                    "/org/a11y/atspi/null".to_owned()
                } else {
                    format!("/c/{}", i / 2)
                };
                // The parent string must outlive the tuple: leak into the test's scope.
                let parent: &'static str = Box::leak(parent.into_boxed_str());
                item(BUS, p, BUS, parent, i as i32, 2, "name", 43)
            })
            .collect();
        let good = encode(&items);
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..3000 {
            let mut m = good.clone();
            for _ in 0..(1 + next() % 8) {
                let i = (next() % m.len() as u64) as usize;
                match next() % 3 {
                    0 => m[i] = next() as u8,
                    1 => m[i] ^= 1 << (next() % 8),
                    _ => m.truncate(i.max(1)),
                }
            }
            for endian in [Endian::Little, Endian::Big] {
                if let Ok(d) = decode_get_items(&m, endian, app(), BUS, 25) {
                    assert!(d.nodes.len() <= 25);
                }
            }
        }
        for _ in 0..3000 {
            let n = (next() % 600) as usize;
            let junk: Vec<u8> = (0..n).map(|_| next() as u8).collect();
            let _ = decode_get_items(&junk, Endian::Little, app(), BUS, 25);
        }
    }
}
