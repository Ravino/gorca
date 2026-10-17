// SPDX-License-Identifier: GPL-3.0-or-later
//! R0: the baseline policy. Nothing is prefetched: every focus miss is fetched by the Reader's
//! shared path, and the cache serves revisits.

use super::{FillCtx, FillEffects, FillInput, FillPolicy};

#[derive(Copy, Clone, Debug, Default)]
pub struct NullPolicy;

impl FillPolicy for NullPolicy {
    fn name(&self) -> &'static str {
        "r0"
    }

    fn step(&mut self, _cx: &FillCtx<'_>, input: FillInput<'_>, _out: &mut FillEffects) {
        match input {
            FillInput::Appeared { .. }
            | FillInput::Ready
            | FillInput::Events(_)
            | FillInput::Resync { .. }
            | FillInput::Demand { .. }
            | FillInput::FocusSettled { .. }
            | FillInput::WindowActivated { .. }
            | FillInput::Reply { .. }
            | FillInput::Tick => {}
        }
    }

    fn memory_bytes(&self) -> usize {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fill::{AppClass, HealthView, SlotView};
    use crate::limits::FillLimits;
    use crate::store::mirror;
    use gorca_model::{AppId, ConnectorId};
    use std::time::Instant;

    #[test]
    fn r0_never_asks_for_anything() {
        let (_w, r) = mirror();
        let limits = FillLimits::default();
        let cx = FillCtx {
            now: Instant::now(),
            app: AppId {
                connector: ConnectorId(0),
                serial: 1,
            },
            app_class: AppClass::Unknown,
            health: HealthView::Healthy,
            dump_complete: false,
            mirror: &r,
            focus: None,
            slots: SlotView::default(),
            limits: &limits,
        };
        let mut p = NullPolicy;
        let mut out = FillEffects::new();
        p.step(&cx, FillInput::Ready, &mut out);
        p.step(&cx, FillInput::Tick, &mut out);
        assert!(out.is_empty());
        assert_eq!(p.memory_bytes(), 0);
        assert_eq!(p.name(), "r0");
    }
}
