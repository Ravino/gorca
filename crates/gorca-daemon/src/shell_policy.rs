// SPDX-License-Identifier: GPL-3.0-or-later
//! The native adapter between the core's `AppPolicy` and the `ext-gnome-shell` rule table
//! (ADR-0008 section 3.3: first-party extensions run native in-process now and as WASM after M5).
//! It turns core facts into the extension's plain facts and the verdict back, and contains the
//! extension: a panic is caught and counted, and three of them switch the policy off (the core
//! then presents the Shell like any application).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU32, Ordering};

use ext_gnome_shell::{Facts, Moment, Range, Verdict, decide};
use gorca_core::{AppPolicy, PolicyMoment, PolicyVerdict, PolicyView};
use gorca_model::{Segment, SegmentKind};

/// Faults after which the extension is off for the rest of the run.
const DISABLE_AFTER: u32 = 3;

#[derive(Debug, Default)]
pub struct ShellPolicy {
    faults: AtomicU32,
}

fn kind(k: ext_gnome_shell::SegmentKind) -> SegmentKind {
    match k {
        ext_gnome_shell::SegmentKind::Text => SegmentKind::Text,
        ext_gnome_shell::SegmentKind::Role => SegmentKind::Role,
        ext_gnome_shell::SegmentKind::State => SegmentKind::State,
        ext_gnome_shell::SegmentKind::Value => SegmentKind::Value,
    }
}

/// An extension text in the UI language: a message reference of the extension's catalog is
/// resolved, a literal passes through.
fn resolve(text: &str) -> String {
    gorca_i18n::ext::resolve_for(text, gorca_i18n::language(), "ext-gnome-shell").into_owned()
}

impl AppPolicy for ShellPolicy {
    fn decide(&self, f: &PolicyView<'_>) -> PolicyVerdict {
        if self.faults.load(Ordering::Relaxed) >= DISABLE_AFTER {
            return PolicyVerdict::Default;
        }
        // Application text never reads as a message reference (ADR-0010).
        let name = f.name.map(gorca_i18n::ext::sanitize);
        let description = f.description.map(gorca_i18n::ext::sanitize);
        let facts = Facts {
            moment: match f.moment {
                PolicyMoment::Focus => Moment::Focus,
                PolicyMoment::Value => Moment::Value,
                PolicyMoment::Activate => Moment::Activate,
                PolicyMoment::Showing => Moment::Showing,
            },
            role: f.role.code(),
            flags: f.flags,
            name: name.as_deref(),
            description: description.as_deref(),
            range: f
                .range
                .map(|(current, min, max)| Range { current, min, max }),
        };
        match catch_unwind(AssertUnwindSafe(|| decide(&facts))) {
            Ok(Verdict::Default) => PolicyVerdict::Default,
            Ok(Verdict::Silence) => PolicyVerdict::Silence,
            Ok(Verdict::Value(v)) => PolicyVerdict::Value(resolve(&v)),
            Ok(Verdict::Notice) => PolicyVerdict::Notice,
            Ok(Verdict::Say(segs)) => PolicyVerdict::Say(
                segs.into_iter()
                    .map(|s| Segment::new(resolve(&s.text).into(), kind(s.kind)))
                    .collect(),
            ),
            Err(_) => {
                self.faults.fetch_add(1, Ordering::Relaxed);
                tracing::error!("ext-gnome-shell panicked; the Shell is presented by the core");
                PolicyVerdict::Default
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::sem::SemRole;

    fn facts(role: SemRole, range: Option<(f64, f64, f64)>) -> PolicyView<'static> {
        PolicyView {
            moment: PolicyMoment::Focus,
            role,
            flags: 0,
            name: Some("Volume"),
            description: None,
            range,
        }
    }

    #[test]
    fn the_shell_window_claim_is_silenced_and_a_unit_slider_speaks_percent() {
        ext_gnome_shell::register_catalogs();
        let p = ShellPolicy::default();
        assert_eq!(
            p.decide(&facts(SemRole::Window, None)),
            PolicyVerdict::Silence
        );
        assert_eq!(
            p.decide(&facts(SemRole::Slider, Some((0.25, 0.0, 1.0)))),
            PolicyVerdict::Value("25 percent".to_owned())
        );
        assert_eq!(
            p.decide(&facts(SemRole::Button, None)),
            PolicyVerdict::Default
        );
    }

    #[test]
    fn the_roles_the_rules_look_at_are_the_registry_codes_of_the_model() {
        assert_eq!(SemRole::Window.code(), ext_gnome_shell::role::WINDOW);
        assert_eq!(SemRole::Dialog.code(), ext_gnome_shell::role::DIALOG);
        assert_eq!(SemRole::Alert.code(), ext_gnome_shell::role::ALERT);
        assert_eq!(SemRole::Slider.code(), ext_gnome_shell::role::SLIDER);
        assert_eq!(
            SemRole::ProgressBar.code(),
            ext_gnome_shell::role::PROGRESS_BAR
        );
        for r in [SemRole::Dialog, SemRole::AlertDialog] {
            assert!(ext_gnome_shell::role::DIALOGS.contains(&r.code()));
        }
        for r in [SemRole::Slider, SemRole::ProgressBar, SemRole::Meter] {
            assert!(ext_gnome_shell::role::LEVELS.contains(&r.code()));
        }
        // A scroll bar is not a level (the legacy mapping had no scroll bar role at all).
        assert!(!ext_gnome_shell::role::LEVELS.contains(&SemRole::Scrollbar.code()));
    }
}
