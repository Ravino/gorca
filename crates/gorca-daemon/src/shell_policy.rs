//! The native adapter between the core's `AppPolicy` and the `ext-gnome-shell` rule table
//! (ADR-0008 section 3.3: first-party extensions run native in-process now and as WASM after M5).
//! It turns core facts into the extension's plain facts and the verdict back, and contains the
//! extension: a panic is caught and counted, and three of them switch the policy off (the core
//! then presents the Shell like any application).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU32, Ordering};

use ext_gnome_shell::{Facts, Moment, Range, Verdict, decide, role};
use gorca_core::{AppPolicy, PolicyFacts, PolicyMoment, PolicyVerdict};
use gorca_model::{Role, Segment, SegmentKind};

/// Faults after which the extension is off for the rest of the run.
const DISABLE_AFTER: u32 = 3;

#[derive(Debug, Default)]
pub struct ShellPolicy {
    faults: AtomicU32,
}

/// The WIT role number of a core role (`gorca-ext-api/wit/gorca-ext.wit`, same order).
fn ext_role(r: Role) -> u8 {
    match r {
        Role::Unknown => 0,
        Role::Application => 1,
        Role::Window => role::WINDOW,
        Role::Dialog => role::DIALOG,
        Role::Panel => 4,
        Role::Document | Role::DocumentWeb => 5,
        Role::Heading => 6,
        Role::Paragraph => 7,
        Role::Link => 8,
        Role::Image => 9,
        Role::List => 10,
        Role::ListItem => 11,
        Role::Table => 12,
        Role::Row => 13,
        Role::Cell => 14,
        Role::ColumnHeader => 15,
        Role::RowHeader => 16,
        Role::Button => 17,
        Role::ToggleButton => 18,
        Role::CheckBox => 19,
        Role::RadioButton => 20,
        Role::ComboBox => 21,
        Role::TextEntry => 22,
        Role::PasswordEntry => 23,
        Role::SpinButton => 24,
        Role::Slider => role::SLIDER,
        Role::ProgressBar => role::PROGRESS_BAR,
        Role::Menu => 27,
        Role::MenuBar => 28,
        Role::MenuItem => 29,
        Role::Popover => 30,
        Role::Tab => 31,
        Role::TabList => 32,
        Role::Tree => 33,
        Role::TreeItem => 34,
        Role::Toolbar => 35,
        Role::StatusBar => 36,
        Role::Separator => 37,
        Role::Label => 38,
        Role::StaticText => 39,
        Role::Terminal => 40,
        Role::Landmark => 41,
        Role::Alert => role::ALERT,
    }
}

fn kind(k: ext_gnome_shell::SegmentKind) -> SegmentKind {
    match k {
        ext_gnome_shell::SegmentKind::Text => SegmentKind::Text,
        ext_gnome_shell::SegmentKind::Role => SegmentKind::Role,
        ext_gnome_shell::SegmentKind::State => SegmentKind::State,
        ext_gnome_shell::SegmentKind::Value => SegmentKind::Value,
    }
}

impl AppPolicy for ShellPolicy {
    fn decide(&self, f: &PolicyFacts<'_>) -> PolicyVerdict {
        if self.faults.load(Ordering::Relaxed) >= DISABLE_AFTER {
            return PolicyVerdict::Default;
        }
        let facts = Facts {
            moment: match f.moment {
                PolicyMoment::Focus => Moment::Focus,
                PolicyMoment::Value => Moment::Value,
                PolicyMoment::Activate => Moment::Activate,
                PolicyMoment::Showing => Moment::Showing,
            },
            role: ext_role(f.role),
            states: f.states.bits(),
            name: f.name,
            description: f.description,
            range: f
                .range
                .map(|(current, min, max)| Range { current, min, max }),
        };
        match catch_unwind(AssertUnwindSafe(|| decide(&facts))) {
            Ok(Verdict::Default) => PolicyVerdict::Default,
            Ok(Verdict::Silence) => PolicyVerdict::Silence,
            Ok(Verdict::Value(v)) => PolicyVerdict::Value(v),
            Ok(Verdict::Notice) => PolicyVerdict::Notice,
            Ok(Verdict::Say(segs)) => PolicyVerdict::Say(
                segs.into_iter()
                    .map(|s| Segment {
                        text: s.text.into(),
                        kind: kind(s.kind),
                    })
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
    use gorca_model::StateSet;

    fn facts(role: Role, range: Option<(f64, f64, f64)>) -> PolicyFacts<'static> {
        PolicyFacts {
            moment: PolicyMoment::Focus,
            role,
            states: StateSet::empty(),
            name: Some("Volume"),
            description: None,
            range,
        }
    }

    #[test]
    fn the_shell_window_claim_is_silenced_and_a_unit_slider_speaks_percent() {
        let p = ShellPolicy::default();
        assert_eq!(p.decide(&facts(Role::Window, None)), PolicyVerdict::Silence);
        assert_eq!(
            p.decide(&facts(Role::Slider, Some((0.25, 0.0, 1.0)))),
            PolicyVerdict::Value("25 percent".to_owned())
        );
        assert_eq!(p.decide(&facts(Role::Button, None)), PolicyVerdict::Default);
    }

    #[test]
    fn every_core_role_maps_to_the_wit_number_of_its_name() {
        // The two roles the rules look at, plus a spread; the full order is checked by the
        // extension's own test against the WIT file.
        assert_eq!(ext_role(Role::Window), 2);
        assert_eq!(ext_role(Role::Alert), 42);
        assert_eq!(ext_role(Role::Button), 17);
        assert_eq!(ext_role(Role::Landmark), 41);
    }
}
