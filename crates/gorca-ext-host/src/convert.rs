// SPDX-License-Identifier: GPL-3.0-or-later
//! Core types to extension types and back. The extension API is deliberately a narrow subset of
//! the core model (ADR-0001 section 10): ids are packed integers, roles are registry codes, text is
//! copied. Everything an extension returns is clamped here, so a hostile or buggy extension cannot
//! make the core allocate or speak without bound.

use gorca_ext_api as api;
use gorca_model::{
    AppId, ConnectorId, LocalId, NodeEvent, NodeKey, Segment, SegmentKind, WindowKind,
};

/// Most segments one verdict, presentation or command answer may carry.
pub const MAX_SEGMENTS: usize = 32;
/// Most characters one segment may carry.
pub const MAX_SEGMENT_CHARS: usize = 2048;
/// Most characters of one answer, summed over its segments (about three screens of text).
pub const MAX_ANSWER_CHARS: usize = 8192;
/// Most characters of a name or description copied into an extension.
pub const MAX_NODE_TEXT_CHARS: usize = 4096;

pub fn app_to_ext(a: AppId) -> u64 {
    (u64::from(a.connector.0) << 32) | u64::from(a.serial)
}

pub fn app_from_ext(v: u64) -> Option<AppId> {
    Some(AppId {
        connector: ConnectorId(u16::try_from(v >> 32).ok()?),
        serial: u32::try_from(v & 0xffff_ffff).ok()?,
    })
}

pub fn key_to_ext(k: NodeKey) -> api::NodeKey {
    (app_to_ext(k.app), k.local.0)
}

pub fn key_from_ext(k: api::NodeKey) -> Option<NodeKey> {
    Some(NodeKey {
        app: app_from_ext(k.0)?,
        local: LocalId(k.1),
    })
}

fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_owned(),
        None => s.to_owned(),
    }
}

/// Text that comes from an application, as an extension may see it: clipped, and without the
/// message-reference marker characters, so that it can never be taken for a reference when the
/// extension hands it back as speech.
pub fn app_text(s: &str, max: usize) -> String {
    clip(&gorca_i18n::ext::sanitize(s), max)
}

/// The node as an extension sees it: the facts of its fused entry, clipped. The core withheld a
/// secret field's text and range already (`ExtNode::new`), so there is no second secrecy
/// decision here.
pub fn node_to_ext(n: &gorca_core::ExtNode) -> api::Node {
    api::Node {
        key: key_to_ext(n.key),
        role: n.role.code(),
        flags: n.flags,
        name: n.name.as_deref().map(|s| app_text(s, MAX_NODE_TEXT_CHARS)),
        description: n
            .description
            .as_deref()
            .map(|s| app_text(s, MAX_NODE_TEXT_CHARS)),
        range: n
            .range
            .map(|(current, min, max)| api::Range { current, min, max }),
    }
}

/// One fused group as an extension reads it.
pub fn group_to_ext(g: gorca_core::ExtGroup) -> api::GroupValue {
    match g {
        gorca_core::ExtGroup::Code(c) => api::GroupValue::Code(c),
        gorca_core::ExtGroup::Bits(b) => api::GroupValue::Bits(b),
        gorca_core::ExtGroup::Number(n) => api::GroupValue::Number(n),
        gorca_core::ExtGroup::Text(t) => {
            api::GroupValue::Text(clip(&gorca_i18n::ext::sanitize(&t), MAX_NODE_TEXT_CHARS))
        }
    }
}

/// The event as an extension sees it, or `None` for events outside the WIT subset.
pub fn event_to_ext(ev: &NodeEvent) -> Option<api::Event> {
    Some(match ev {
        NodeEvent::FocusChanged { node } => api::Event::FocusChanged(key_to_ext(*node)),
        NodeEvent::ActiveDescendantChanged { container, .. } => {
            api::Event::PropertiesChanged(key_to_ext(*container), api::prop::STATES)
        }
        NodeEvent::PropertiesChanged { node, mask } => {
            api::Event::PropertiesChanged(key_to_ext(*node), mask.bits())
        }
        NodeEvent::ChildrenChanged { node } => api::Event::ChildrenChanged(key_to_ext(*node)),
        NodeEvent::TextChanged { node, .. } => api::Event::TextChanged(key_to_ext(*node)),
        NodeEvent::CaretMoved { node, offset, .. } => {
            api::Event::CaretMoved(key_to_ext(*node), *offset)
        }
        NodeEvent::SelectionChanged { node } => api::Event::SelectionChanged(key_to_ext(*node)),
        NodeEvent::Announcement { text, .. } => {
            api::Event::Announcement(app_text(text, MAX_NODE_TEXT_CHARS))
        }
        NodeEvent::WindowEvent { node, kind } => match kind {
            WindowKind::Activate => api::Event::WindowActivated(key_to_ext(*node)),
            WindowKind::Deactivate | WindowKind::Create | WindowKind::Destroy => return None,
        },
        NodeEvent::DocumentEvent { .. } => return None,
        NodeEvent::NodeRemoved { node } => api::Event::NodeRemoved(key_to_ext(*node)),
    })
}

/// Extension segments as core segments, clamped (count and length) and stripped of segments
/// that are empty after clamping.
pub fn segments_from_ext(ns: &str, segs: Vec<api::Segment>) -> Vec<Segment> {
    segments_from_iter(ns, segs)
}

/// Like [`segments_from_ext`] over any iterator: stops pulling at the caps, so a hostile list
/// is never copied whole.
pub fn segments_from_iter(ns: &str, segs: impl IntoIterator<Item = api::Segment>) -> Vec<Segment> {
    let mut budget = MAX_ANSWER_CHARS;
    let mut out = Vec::new();
    for s in segs.into_iter().take(MAX_SEGMENTS) {
        if budget == 0 {
            break;
        }
        if s.text.is_empty() {
            continue;
        }
        // A message reference of the extension's own catalog is resolved in the UI language
        // here, the one place extension text enters the presentation. A reference into another
        // namespace resolves to nothing.
        let said = gorca_i18n::ext::resolve_for(&s.text, gorca_i18n::language(), ns);
        let text = clip(&said, MAX_SEGMENT_CHARS.min(budget));
        if text.is_empty() {
            continue;
        }
        budget = budget.saturating_sub(text.chars().count());
        out.push(Segment::new(
            text.into(),
            match s.kind {
                api::SegmentKind::Text => SegmentKind::Text,
                api::SegmentKind::Role => SegmentKind::Role,
                api::SegmentKind::State => SegmentKind::State,
                api::SegmentKind::Value => SegmentKind::Value,
            },
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_reference_cannot_exceed_the_segment_and_answer_caps() {
        gorca_i18n::ext::register("hostile-conv", gorca_i18n::Lang::En, "b = {a}{a}{a}{a}\n");
        let arg = "x".repeat(1500);
        let r = gorca_i18n::ext::encode("hostile-conv", "b", &[("a", &arg)]);
        let segs: Vec<api::Segment> = (0..64)
            .map(|_| api::Segment {
                text: r.clone(),
                kind: api::SegmentKind::Text,
            })
            .collect();
        let out = segments_from_ext("hostile-conv", segs);
        let total: usize = out.iter().map(|s| s.text.chars().count()).sum();
        assert!(
            out.iter()
                .all(|s| s.text.chars().count() <= MAX_SEGMENT_CHARS)
        );
        assert!(total <= MAX_ANSWER_CHARS, "{total}");
    }

    #[test]
    fn a_reference_into_another_namespace_is_silent_and_app_text_never_a_reference() {
        gorca_i18n::ext::register("conv-a", gorca_i18n::Lang::En, "k = text of a\n");
        let r = gorca_i18n::ext::encode("conv-a", "k", &[]);
        let own = segments_from_ext("conv-a", vec![api::Segment::text(r.clone())]);
        assert_eq!(&*own[0].text, "text of a");
        assert!(segments_from_ext("conv-b", vec![api::Segment::text(r.clone())]).is_empty());
        // Application text with the marker is cleaned before an extension sees it.
        assert_eq!(app_text(&r, 100), "conv-a:k");
    }

    #[test]
    fn app_ids_round_trip() {
        for (c, s) in [(0u16, 0u32), (1, 1), (u16::MAX, u32::MAX), (7, 123_456)] {
            let a = AppId {
                connector: ConnectorId(c),
                serial: s,
            };
            assert_eq!(app_from_ext(app_to_ext(a)), Some(a));
        }
        // Garbage from an extension never panics.
        assert_eq!(app_from_ext(u64::MAX), None);
    }

    #[test]
    fn the_property_mask_bits_are_the_models() {
        use gorca_model::PropMask;
        for (ext, model) in [
            (api::prop::NAME, PropMask::NAME),
            (api::prop::DESCRIPTION, PropMask::DESCRIPTION),
            (api::prop::VALUE, PropMask::VALUE),
            (api::prop::ROLE, PropMask::ROLE),
            (api::prop::STATES, PropMask::STATES),
            (api::prop::BOUNDS, PropMask::BOUNDS),
            (api::prop::ATTRS, PropMask::ATTRS),
            (api::prop::RELATIONS, PropMask::RELATIONS),
            (api::prop::CHILDREN, PropMask::CHILDREN),
            (api::prop::SELECTED_NOW, PropMask::SELECTED_NOW),
            (api::prop::PARENT, PropMask::PARENT),
        ] {
            assert_eq!(ext, model.bits());
        }
    }

    #[test]
    fn clip_counts_characters_not_bytes() {
        assert_eq!(clip("héllo", 2), "hé");
        assert_eq!(clip("ab", 5), "ab");
        assert_eq!(clip("", 0), "");
    }

    #[test]
    fn segments_are_clamped() {
        let many: Vec<_> = (0..1000)
            .map(|_| api::Segment::text("x".repeat(10_000)))
            .collect();
        let out = segments_from_ext("x", many);
        assert!(out.len() <= MAX_SEGMENTS);
        assert!(
            out.iter()
                .all(|s| s.text.chars().count() <= MAX_SEGMENT_CHARS)
        );
        let total: usize = out.iter().map(|s| s.text.chars().count()).sum();
        assert_eq!(total, MAX_ANSWER_CHARS, "the answer is capped as a whole");
        assert!(segments_from_ext("x", vec![api::Segment::text("")]).is_empty());
    }
}
