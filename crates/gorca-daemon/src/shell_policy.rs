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
        Role::Document => 5,
        Role::DocumentWeb => 6,
        Role::Heading => 7,
        Role::Paragraph => 8,
        Role::Link => 9,
        Role::Image => 10,
        Role::List => 11,
        Role::ListItem => 12,
        Role::Table => 13,
        Role::Row => 14,
        Role::Cell => 15,
        Role::ColumnHeader => 16,
        Role::RowHeader => 17,
        Role::Button => 18,
        Role::ToggleButton => 19,
        Role::CheckBox => 20,
        Role::RadioButton => 21,
        Role::ComboBox => 22,
        Role::TextEntry => 23,
        Role::PasswordEntry => 24,
        Role::SpinButton => 25,
        Role::Slider => role::SLIDER,
        Role::ProgressBar => role::PROGRESS_BAR,
        Role::Menu => 28,
        Role::MenuBar => 29,
        Role::MenuItem => 30,
        Role::Popover => 31,
        Role::Tab => 32,
        Role::TabList => 33,
        Role::Tree => 34,
        Role::TreeItem => 35,
        Role::Toolbar => 36,
        Role::StatusBar => 37,
        Role::Separator => 38,
        Role::Label => 39,
        Role::StaticText => 40,
        Role::Terminal => 41,
        Role::Landmark => 42,
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

/// An extension text in the UI language: a message reference of the extension's catalog is
/// resolved, a literal passes through.
fn resolve(text: &str) -> String {
    gorca_i18n::ext::resolve(text, gorca_i18n::language()).into_owned()
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
            Ok(Verdict::Value(v)) => PolicyVerdict::Value(resolve(&v)),
            Ok(Verdict::Notice) => PolicyVerdict::Notice,
            Ok(Verdict::Say(segs)) => PolicyVerdict::Say(
                segs.into_iter()
                    .map(|s| Segment {
                        text: resolve(&s.text).into(),
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
        ext_gnome_shell::register_catalogs();
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
        assert_eq!(ext_role(Role::Alert), 43);
        assert_eq!(ext_role(Role::Button), 18);
        assert_eq!(ext_role(Role::Landmark), 42);
    }
}
