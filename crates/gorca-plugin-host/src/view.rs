// SPDX-License-Identifier: GPL-3.0-or-later
//! The neutral read side of the `model` import (ADR-0026 §2.3, §2.4.6 `FrozenRows`).
//!
//! The host implements this over an immutable snapshot taken at the writer's turn. It names
//! nodes by host-side `index` only; handles are minted per instance by the runtime. Nothing here
//! mentions a connector.

/// An immutable, `Send + Sync` view of the nodes a job may read.
pub trait ModelView: Send + Sync + std::fmt::Debug {
    /// Live generation of `index`, or `None` when the slot is gone.
    fn generation(&self, index: u32) -> Option<u16>;
    /// Role registry code.
    fn role(&self, index: u32) -> Option<u16>;
    /// State bits.
    fn states(&self, index: u32) -> u64;
    /// Accessible name (caller has checked `read-names`).
    fn name(&self, index: u32) -> Option<String>;
    /// Cached text slice (caller has checked `read-text`).
    fn text(&self, index: u32, start: u32, len: u32) -> Option<String>;
    /// Neutral attribute (caller has checked `read-attributes`).
    fn attr(&self, index: u32, key: &str) -> Option<String>;
    /// Parent index.
    fn parent(&self, index: u32) -> Option<u32>;
    /// Child indices, at most `max`.
    fn children(&self, index: u32, max: u16) -> Vec<u32>;
}

/// A view with no nodes (every read is `none`).
#[derive(Debug, Default, Clone, Copy)]
pub struct EmptyView;

impl ModelView for EmptyView {
    fn generation(&self, _index: u32) -> Option<u16> {
        None
    }
    fn role(&self, _index: u32) -> Option<u16> {
        None
    }
    fn states(&self, _index: u32) -> u64 {
        0
    }
    fn name(&self, _index: u32) -> Option<String> {
        None
    }
    fn text(&self, _index: u32, _start: u32, _len: u32) -> Option<String> {
        None
    }
    fn attr(&self, _index: u32, _key: &str) -> Option<String> {
        None
    }
    fn parent(&self, _index: u32) -> Option<u32> {
        None
    }
    fn children(&self, _index: u32, _max: u16) -> Vec<u32> {
        Vec::new()
    }
}
