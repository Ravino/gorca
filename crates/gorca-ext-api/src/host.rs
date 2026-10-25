// SPDX-License-Identifier: MIT OR Apache-2.0
//! The host functions, as free functions: one API for both builds of an extension.
//!
//! - Component build (`wasm32`): each call is the WIT import of the `host` interface.
//! - Native build: the host installs a [`Host`](crate::Host) for the duration of each hook call
//!   ([`with_host`]); outside a hook the calls are inert (`get_node` returns `None`, `fetch` is
//!   `Denied`, `speak` and `log` do nothing).

use crate::api::{Budget, GroupCode, GroupValue, Node, NodeKey, Prio, QueryError, Segment};

/// Mirror only, never blocks.
pub fn get_node(key: NodeKey) -> Option<Node> {
    imp::get_node(key)
}

/// One fused group of a node of this application. Mirror only, never blocks.
pub fn read_group(key: NodeKey, group: GroupCode) -> Option<GroupValue> {
    imp::read_group(key, group)
}

/// Host-enforced deadline.
pub fn fetch(key: NodeKey, mask: u32, b: Budget) -> Result<Node, QueryError> {
    imp::fetch(key, mask, b)
}

pub fn text(key: NodeKey, start: u32, end: u32, b: Budget) -> Result<String, QueryError> {
    imp::text(key, start, end, b)
}

/// Speak now. Tagged with the current epoch by the host.
pub fn speak(segments: Vec<Segment>, prio: Prio) {
    imp::speak(segments, prio);
}

/// Own namespace only.
pub fn get_setting(k: &str) -> Option<String> {
    imp::get_setting(k)
}

/// Own namespace only; bounded sizes, extra keys are dropped.
pub fn set_setting(k: &str, v: &str) {
    imp::set_setting(k, v);
}

/// 0 error, 1 warn, 2 info, 3 debug.
pub fn log(level: u8, msg: &str) {
    imp::log(level, msg);
}

#[cfg(not(target_arch = "wasm32"))]
pub use imp::with_host;

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::cell::RefCell;
    use std::sync::Arc;

    use crate::api::{
        Budget, GroupCode, GroupValue, Host, Node, NodeKey, Prio, QueryError, Segment,
    };

    thread_local! {
        static CURRENT: RefCell<Option<Arc<dyn Host>>> = const { RefCell::new(None) };
    }

    struct Restore(Option<Arc<dyn Host>>);

    impl Drop for Restore {
        fn drop(&mut self) {
            let prev = self.0.take();
            let _ = CURRENT.try_with(|c| {
                if let Ok(mut c) = c.try_borrow_mut() {
                    *c = prev;
                }
            });
        }
    }

    /// Run `f` with `host` installed on this thread; the previous host (if any) is restored on
    /// return and on unwind.
    pub fn with_host<R>(host: Arc<dyn Host>, f: impl FnOnce() -> R) -> R {
        let prev = CURRENT.with(|c| c.replace(Some(host)));
        let _restore = Restore(prev);
        f()
    }

    fn current() -> Option<Arc<dyn Host>> {
        CURRENT.with(|c| c.borrow().clone())
    }

    pub fn get_node(key: NodeKey) -> Option<Node> {
        current()?.get_node(key)
    }

    pub fn read_group(key: NodeKey, group: GroupCode) -> Option<GroupValue> {
        current()?.read_group(key, group)
    }

    pub fn fetch(key: NodeKey, mask: u32, b: Budget) -> Result<Node, QueryError> {
        current().ok_or(QueryError::Denied)?.fetch(key, mask, b)
    }

    pub fn text(key: NodeKey, start: u32, end: u32, b: Budget) -> Result<String, QueryError> {
        current()
            .ok_or(QueryError::Denied)?
            .text(key, start, end, b)
    }

    pub fn speak(segments: Vec<Segment>, prio: Prio) {
        if let Some(h) = current() {
            h.speak(segments, prio);
        }
    }

    pub fn get_setting(k: &str) -> Option<String> {
        current()?.get_setting(k)
    }

    pub fn set_setting(k: &str, v: &str) {
        if let Some(h) = current() {
            h.set_setting(k, v);
        }
    }

    pub fn log(level: u8, msg: &str) {
        if let Some(h) = current() {
            h.log(level, msg);
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use crate::api::{Budget, GroupCode, GroupValue, Node, NodeKey, Prio, QueryError, Segment};
    use crate::guest::{self, wit};

    pub fn get_node(key: NodeKey) -> Option<Node> {
        wit::gorca::ext::host::get_node(key).map(guest::node_from_wit)
    }

    pub fn read_group(key: NodeKey, group: GroupCode) -> Option<GroupValue> {
        wit::gorca::ext::host::read_group(key, group).map(guest::group_value_from_wit)
    }

    pub fn fetch(key: NodeKey, mask: u32, b: Budget) -> Result<Node, QueryError> {
        wit::gorca::ext::host::fetch(key, mask, guest::budget_to_wit(b))
            .map(guest::node_from_wit)
            .map_err(guest::query_error_from_wit)
    }

    pub fn text(key: NodeKey, start: u32, end: u32, b: Budget) -> Result<String, QueryError> {
        wit::gorca::ext::host::text(key, start, end, guest::budget_to_wit(b))
            .map_err(guest::query_error_from_wit)
    }

    pub fn speak(segments: Vec<Segment>, prio: Prio) {
        let segs: Vec<_> = segments.into_iter().map(guest::segment_to_wit).collect();
        wit::gorca::ext::host::speak(&segs, guest::prio_to_wit(prio));
    }

    pub fn get_setting(k: &str) -> Option<String> {
        wit::gorca::ext::host::get_setting(k)
    }

    pub fn set_setting(k: &str, v: &str) {
        wit::gorca::ext::host::set_setting(k, v);
    }

    pub fn log(level: u8, msg: &str) {
        wit::gorca::ext::host::log(level, msg);
    }
}
