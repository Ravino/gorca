//! `ext-gnome-shell`: GNOME Shell presentation policy (M4b, ADR-0001 section 10, ADR-0008).
//!
//! The Shell is a Clutter/St application: no Selection interface, actors that appear and vanish
//! on demand, sliders that run from 0 to 1, a window that claims focus after the Alt+Tab switcher
//! closes. The core presents it like any application; this extension holds only what is
//! specific to the Shell, **as data**: a table of [`RULES`], each with a stable id, a condition
//! over [`Facts`] and one of a closed set of outcomes ([`Then`]). There is no code per rule
//! beyond the evaluation loop, so the same table is the WASM component of M5 unchanged.
//!
//! The interface is narrow on purpose: facts in (role, states, name, description, a value range,
//! which moment), [`Verdict`] out. It depends only on `gorca-ext-api`; the host (today the
//! daemon's native adapter, after M5 the WASM host) turns its own node types into [`Facts`].
//! Sources of the rules: `refs/orca/src/orca/scripts/apps/gnome-shell/script.py` (focus claims
//! of the window after the switcher), `refs/gnome-shell/js/ui/barLevel.js` and `slider.js`
//! (levels run 0..1), `refs/gnome-shell/js/ui/osdWindow.js` (the OSD is a level bar).

use gorca_ext_api::{
    CommandResult, Event, EventVerdict, Hooks, NavUnit, Node, PresentCtx, PresentReason,
};
pub use gorca_ext_api::{Range, Segment, SegmentKind};

/// Role numbers of the WIT `role` enum (`gorca-ext-api/wit/gorca-ext.wit`); a test keeps them in
/// step with the file.
pub mod role {
    pub const WINDOW: u8 = 2;
    pub const DIALOG: u8 = 3;
    pub const SLIDER: u8 = 25;
    pub const PROGRESS_BAR: u8 = 26;
    pub const ALERT: u8 = 42;
}

/// When a decision is asked for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Moment {
    /// A node took focus and is about to be presented.
    Focus,
    /// The value of a node that does not hold focus changed (the OSD level bar).
    Value,
    /// A window was activated and is about to be announced.
    Activate,
    /// A node of a role the mirror does not otherwise treat as a notice became visible: does it
    /// read like one?
    Showing,
}

/// What a rule looks at.
#[derive(Copy, Clone, Debug)]
pub struct Facts<'a> {
    pub moment: Moment,
    /// WIT role number (see [`role`]).
    pub role: u8,
    pub states: u64,
    pub name: Option<&'a str>,
    pub description: Option<&'a str>,
    pub range: Option<Range>,
}

/// What the extension decides.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// The core presents the node as it would anywhere.
    Default,
    /// Say nothing.
    Silence,
    /// Replace the value in the core's presentation with this text.
    Value(String),
    /// Say this instead of the core's presentation.
    Say(Vec<Segment>),
    /// Read this node as a notice when it appears (title, role word, text), like an alert.
    Notice,
}

/// The outcome of a rule: a closed set, so a rule cannot do anything the host did not expect.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Then {
    Silence,
    /// The value as a whole percent (`0.5` of `0..1` is "50 percent").
    PercentValue,
    /// The name (or "level") and the percent, for a level bar that appears on its own.
    SayLevel,
    /// Read the node as a notice.
    ReadAsNotice,
}

/// The condition of a rule. `None` matches anything.
#[derive(Copy, Clone, Debug)]
pub struct When {
    pub moment: Moment,
    pub roles: &'static [u8],
    /// The node reports a value range from 0 to 1 (St levels).
    pub unit_range: bool,
}

#[derive(Copy, Clone, Debug)]
pub struct Rule {
    /// Stable id, logged and testable (ADR-0008 WE-6).
    pub id: &'static str,
    pub when: When,
    pub then: Then,
}

/// The Shell's rules, evaluated in order; the first that matches wins.
pub const RULES: &[Rule] = &[
    // The Shell window claims focus after the Alt+Tab switcher closes; the application that
    // really has focus reports itself next (Orca `_on_focused_changed`).
    Rule {
        id: "shell.window-focus-claim",
        when: When {
            moment: Moment::Focus,
            roles: &[role::WINDOW],
            unit_range: false,
        },
        then: Then::Silence,
    },
    // The Shell's own "Main stage" window activates for every popup, menu and the switcher
    // (probe: m4b-shell-probe.md); announcing it would drown the real window's title.
    Rule {
        id: "shell.stage-activate",
        when: When {
            moment: Moment::Activate,
            roles: &[role::WINDOW],
            unit_range: false,
        },
        then: Then::Silence,
    },
    // The Shell shows its dialogs (end session, polkit, the extension prompts) without a
    // `window:activate` of their own: they get `showing` and focus events only, so a dialog
    // that appears is read like a notification (probe: m4b-shell-probe.md).
    Rule {
        id: "shell.dialog-notice",
        when: When {
            moment: Moment::Showing,
            roles: &[role::DIALOG],
            unit_range: false,
        },
        then: Then::ReadAsNotice,
    },
    // St sliders and level bars run from 0 to 1: "0.5" is not what a person hears as volume.
    Rule {
        id: "shell.level-percent",
        when: When {
            moment: Moment::Focus,
            roles: &[role::SLIDER, role::PROGRESS_BAR],
            unit_range: true,
        },
        then: Then::PercentValue,
    },
    // The OSD (volume, brightness) is a level bar that appears for a second and changes value.
    Rule {
        id: "shell.osd-level",
        when: When {
            moment: Moment::Value,
            roles: &[role::PROGRESS_BAR, role::SLIDER],
            unit_range: true,
        },
        then: Then::SayLevel,
    },
];

fn matches(w: &When, f: &Facts<'_>) -> bool {
    w.moment == f.moment
        && w.roles.contains(&f.role)
        && (!w.unit_range
            || f.range
                .is_some_and(|r| r.min == 0.0 && (r.max - 1.0).abs() < f64::EPSILON))
}

/// A level as a whole percent, rounded, clamped to 0..=100.
fn percent(r: Range) -> i64 {
    let span = r.max - r.min;
    if span <= 0.0 {
        return 0;
    }
    let p = ((r.current - r.min) / span * 100.0).round();
    // Clamped first: the cast cannot overflow or lose the sign.
    #[allow(clippy::cast_possible_truncation)]
    {
        p.clamp(0.0, 100.0) as i64
    }
}

/// The id of the first rule that matches `f`, for the log.
pub fn matching_rule(f: &Facts<'_>) -> Option<&'static Rule> {
    RULES.iter().find(|r| matches(&r.when, f))
}

/// Decide what to say for `f`.
pub fn decide(f: &Facts<'_>) -> Verdict {
    let Some(rule) = matching_rule(f) else {
        return Verdict::Default;
    };
    match rule.then {
        Then::Silence => Verdict::Silence,
        Then::ReadAsNotice => Verdict::Notice,
        Then::PercentValue => match f.range {
            Some(r) => Verdict::Value(format!("{} percent", percent(r))),
            None => Verdict::Default,
        },
        Then::SayLevel => match f.range {
            Some(r) => {
                let name = f.name.filter(|n| !n.trim().is_empty()).unwrap_or("level");
                Verdict::Say(vec![
                    Segment {
                        text: name.to_owned(),
                        kind: SegmentKind::Text,
                    },
                    Segment {
                        text: format!("{} percent", percent(r)),
                        kind: SegmentKind::Value,
                    },
                ])
            }
            None => Verdict::Default,
        },
    }
}

/// The same policy through the extension API, for the host of M5.
#[derive(Debug, Default)]
pub struct ShellExtension;

impl Hooks for ShellExtension {
    fn on_event(&mut self, _ev: &Event) -> EventVerdict {
        EventVerdict::Pass
    }

    fn present(&mut self, n: &Node, ctx: PresentCtx) -> Option<Vec<Segment>> {
        if ctx.reason != PresentReason::Focus {
            return None;
        }
        let facts = Facts {
            moment: Moment::Focus,
            role: n.role,
            states: n.states,
            name: n.name.as_deref(),
            description: n.description.as_deref(),
            range: n.range,
        };
        match decide(&facts) {
            Verdict::Silence => Some(Vec::new()),
            Verdict::Say(s) => Some(s),
            Verdict::Value(v) => {
                let mut segs = Vec::with_capacity(2);
                // An unnamed slider says only its value, never an empty text segment (N-6).
                if let Some(name) = n.name.as_ref().filter(|s| !s.trim().is_empty()) {
                    segs.push(Segment {
                        text: name.clone(),
                        kind: SegmentKind::Text,
                    });
                }
                segs.push(Segment {
                    text: v,
                    kind: SegmentKind::Value,
                });
                Some(segs)
            }
            Verdict::Default | Verdict::Notice => None,
        }
    }

    fn nav_units(&self) -> Vec<NavUnit> {
        Vec::new()
    }

    fn nav_match(&self, _unit: u32, _n: &Node) -> bool {
        false
    }

    fn command(&mut self, _id: u32) -> CommandResult {
        CommandResult::NotHandled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(moment: Moment, role: u8, range: Option<Range>) -> Facts<'static> {
        Facts {
            moment,
            role,
            states: 0,
            name: Some("Volume"),
            description: None,
            range,
        }
    }

    #[allow(clippy::unnecessary_wraps)]
    fn unit(current: f64) -> Option<Range> {
        Some(range(current, 0.0, 1.0))
    }

    fn range(current: f64, min: f64, max: f64) -> Range {
        Range { current, min, max }
    }

    #[test]
    fn role_numbers_follow_the_wit_enum() {
        let wit = gorca_ext_api::WIT_SOURCE;
        let start = wit.find("enum role {").expect("role enum");
        let body = &wit[start..];
        let body = &body[body.find('{').expect("brace") + 1..body.find('}').expect("end")];
        let names: Vec<String> = body
            .split(',')
            .map(|s| s.trim().trim_start_matches('%').to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        let at = |n: &str| {
            u8::try_from(names.iter().position(|x| x == n).expect("role name")).expect("small")
        };
        assert_eq!(at("window"), role::WINDOW);
        assert_eq!(at("dialog"), role::DIALOG);
        assert_eq!(at("slider"), role::SLIDER);
        assert_eq!(at("progress-bar"), role::PROGRESS_BAR);
        assert_eq!(at("alert"), role::ALERT);
    }

    #[test]
    fn the_window_focus_claim_is_silenced_and_a_button_is_not() {
        assert_eq!(
            decide(&facts(Moment::Focus, role::WINDOW, None)),
            Verdict::Silence
        );
        assert_eq!(decide(&facts(Moment::Focus, 17, None)), Verdict::Default);
    }

    #[test]
    fn the_shell_stage_activation_is_not_announced_but_a_dialog_is() {
        assert_eq!(
            decide(&facts(Moment::Activate, role::WINDOW, None)),
            Verdict::Silence
        );
        assert_eq!(
            decide(&facts(Moment::Activate, role::DIALOG, None)),
            Verdict::Default
        );
    }

    #[test]
    fn a_unit_slider_is_spoken_as_a_percentage() {
        let v = decide(&facts(Moment::Focus, role::SLIDER, unit(0.5)));
        assert_eq!(v, Verdict::Value("50 percent".to_owned()));
        // A 0..100 slider (GTK) is left to the core.
        let gtk = Some(range(50.0, 0.0, 100.0));
        assert_eq!(
            decide(&facts(Moment::Focus, role::SLIDER, gtk)),
            Verdict::Default
        );
    }

    #[test]
    fn percentages_round_and_clamp() {
        let p = |c: f64| match decide(&facts(Moment::Focus, role::SLIDER, unit(c))) {
            Verdict::Value(s) => s,
            other => panic!("{other:?}"),
        };
        assert_eq!(p(0.0), "0 percent");
        assert_eq!(p(0.334), "33 percent");
        assert_eq!(p(1.0), "100 percent");
        assert_eq!(p(1.7), "100 percent");
        assert_eq!(p(-0.2), "0 percent");
    }

    #[test]
    fn the_osd_level_says_its_name_and_percent() {
        let v = decide(&facts(Moment::Value, role::PROGRESS_BAR, unit(0.25)));
        let Verdict::Say(s) = v else { panic!("{v:?}") };
        assert_eq!(s[0].text, "Volume");
        assert_eq!(s[1].text, "25 percent");
        assert_eq!(s[1].kind, SegmentKind::Value);
        let mut nameless = facts(Moment::Value, role::PROGRESS_BAR, unit(1.0));
        nameless.name = Some("  ");
        let Verdict::Say(s) = decide(&nameless) else {
            panic!("say")
        };
        assert_eq!(s[0].text, "level");
    }

    #[test]
    fn an_unnamed_unit_slider_says_only_its_value() {
        let mut e = ShellExtension;
        let mut node = Node {
            key: (1, 1),
            role: role::SLIDER,
            states: 0,
            name: None,
            description: None,
            range: unit(0.5),
        };
        let focus = PresentCtx {
            epoch: 1,
            reason: PresentReason::Focus,
        };
        let segs = e.present(&node, focus).unwrap_or_default();
        assert_eq!(segs.len(), 1, "{segs:?}");
        assert_eq!(segs[0].text, "50 percent");
        node.name = Some("Volume".into());
        let segs = e.present(&node, focus).unwrap_or_default();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].text, "Volume");
    }

    #[test]
    fn rules_have_unique_stable_ids() {
        let mut ids: Vec<&str> = RULES.iter().map(|r| r.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), RULES.len());
        assert!(RULES.iter().all(|r| r.id.starts_with("shell.")));
    }

    #[test]
    fn the_extension_api_silences_the_window_claim_only_for_focus() {
        let mut e = ShellExtension;
        let node = Node {
            key: (1, 1),
            role: role::WINDOW,
            states: 0,
            name: Some("Main".into()),
            description: None,
            range: None,
        };
        let focus = PresentCtx {
            epoch: 1,
            reason: PresentReason::Focus,
        };
        let nav = PresentCtx {
            epoch: 1,
            reason: PresentReason::Navigation,
        };
        assert_eq!(e.present(&node, focus), Some(Vec::new()));
        assert_eq!(e.present(&node, nav), None);
        assert_eq!(
            e.on_event(&Event::Announcement("x".into())),
            EventVerdict::Pass
        );
    }

    #[test]
    fn a_dialog_that_appears_reads_as_a_notice_and_other_roles_do_not() {
        assert_eq!(
            decide(&facts(Moment::Showing, role::DIALOG, None)),
            Verdict::Notice
        );
        assert_eq!(
            decide(&facts(Moment::Showing, role::WINDOW, None)),
            Verdict::Default
        );
        assert_eq!(
            decide(&facts(Moment::Focus, role::DIALOG, None)),
            Verdict::Default
        );
    }

    #[test]
    fn the_extension_api_speaks_a_unit_slider_as_a_percentage() {
        let mut e = ShellExtension;
        let node = Node {
            key: (1, 2),
            role: role::SLIDER,
            states: 0,
            name: Some("Volume".into()),
            description: None,
            range: unit(0.5),
        };
        let focus = PresentCtx {
            epoch: 1,
            reason: PresentReason::Focus,
        };
        let segs = e.present(&node, focus).expect("a verdict");
        assert_eq!(segs[0].text, "Volume");
        assert_eq!(segs[1].text, "50 percent");
    }
}
