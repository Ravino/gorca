//! Announcements and notifications (M4b, phase-2.md M4; Orca `_on_showing_changed` for the
//! `notification` role and `object:announcement`).
//!
//! Both are *interruption-free* speech: a notification or an announcement never cuts off focus
//! speech, say-all or the echo of a key (`Prio::Notification` queues behind whatever is being
//! spoken, in the speech lane and in its state machine). Two kinds of source:
//!
//! - `object:announcement` (GTK 4.14 `gtk_accessible_announce`, the AT-SPI `Announcement`
//!   signal): the text is in the event; polite ones die when the user moves on, assertive ones
//!   survive a focus change because the user is not at fault for not having heard them yet;
//! - a node that appears with an alert or notification role (Shell banners, GTK info bars): the
//!   app actor reports `showing`, the Reader asks the application once for the node and its text
//!   (`DesktopEffect::Notice`) and speaks role, name and text. A banner is only worth speaking
//!   for a few seconds, so it survives focus changes but not a flood: the number of reads per
//!   second is bounded.
//!
//! Nothing here names an application (ADR-0008 WE-1); what Shell-specific policy needs lives in
//! the `ext-gnome-shell` extension.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_model::{
    AppId, Epoch, NodeKey, NodeSnapshot, Politeness, Prio, Role, Segment, SegmentKind, UttId,
    Utterance,
};
use smallvec::SmallVec;

use super::fair::{Fair, Kind};
use super::utterance::seg;
use super::window::{DesktopEffect, WindowInfo};
use crate::SpeechIntent;
use crate::directory::Owner;
use crate::reader::{Reader, ReaderEffect, ReaderEffects};

/// Longest announcement spoken, in characters. A page-sized `Announcement` is a bug or an attack.
pub const ANNOUNCE_MAX_CHARS: usize = 400;
/// The same announcement from the same app inside this window is spoken once.
pub const DUPLICATE_ANNOUNCEMENT: Duration = Duration::from_millis(500);
/// A node already read this recently is not read again (banners re-show while they animate).
/// A level bar animates its value over ~100 ms (a dozen events); it is read when quiet.
pub const LEVEL_QUIET: Duration = Duration::from_millis(120);
/// The same level text twice inside this window is spoken once.
const LEVEL_DUPLICATE: Duration = Duration::from_millis(300);
pub const NOTICE_NODE_GAP: Duration = Duration::from_secs(2);
/// Nodes whose read is remembered per owner (a banner re-shows while it animates).
const ASKED_CAP: usize = 64;

/// The text of an announcement: control characters out, whitespace collapsed to single spaces and
/// the length bounded at a character boundary.
pub fn clean_announcement(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(ANNOUNCE_MAX_CHARS));
    let mut pending_space = false;
    let mut chars = 0;
    for c in text.chars() {
        if c.is_whitespace() || c.is_control() {
            pending_space = !out.is_empty();
            continue;
        }
        if chars >= ANNOUNCE_MAX_CHARS {
            break;
        }
        if pending_space {
            out.push(' ');
            chars += 1;
            pending_space = false;
        }
        out.push(c);
        chars += 1;
    }
    out
}

/// The utterance of an announcement. `Prio::Notification` never interrupts; an assertive one
/// survives the next focus change, a polite one does not.
pub fn announcement_utterance(
    text: &str,
    politeness: Politeness,
    id: UttId,
    epoch: Epoch,
) -> Option<Utterance> {
    let text = clean_announcement(text);
    if text.is_empty() {
        return None;
    }
    let survives = match politeness {
        Politeness::Assertive => true,
        Politeness::Polite => false,
    };
    let mut segments = SmallVec::new();
    segments.push(seg(&text, SegmentKind::Text));
    Some(Utterance {
        id,
        segments,
        lang: None,
        epoch,
        prio: Prio::Notification,
        may_refine: false,
        survives_focus_change: survives,
        ttl_secs: 0,
        refines: None,
    })
}

/// Roles that are read as a notice when one appears: the notification and alert roles (the
/// connector maps both to `Alert`).
pub fn is_notice_role(role: Role) -> bool {
    matches!(role, Role::Alert)
}

/// What a notice says: the role word, the name, the description, then the static text inside.
/// Pieces equal to an earlier piece are dropped (a banner's name is often its title label).
pub fn notice_segments(snap: &NodeSnapshot, texts: &[Arc<str>]) -> SmallVec<[Segment; 6]> {
    let mut out: SmallVec<[Segment; 6]> = SmallVec::new();
    out.push(seg(snap.role.spoken(), SegmentKind::Role));
    let add = |out: &mut SmallVec<[Segment; 6]>, text: &str| {
        let t = text.trim();
        if t.is_empty() || out.iter().skip(1).any(|s| s.text.trim() == t) {
            return;
        }
        out.push(seg(t, SegmentKind::Text));
    };
    if let Some(n) = &snap.name {
        add(&mut out, n);
    }
    if let Some(d) = &snap.description {
        add(&mut out, d);
    }
    for t in texts {
        add(&mut out, t);
    }
    out
}

/// What the Reader remembers about notices: the per-owner budgets (`fair`) and counters.
#[derive(Debug)]
pub(crate) struct NoticeCtx {
    pub(crate) fair: Fair,
    /// The level bar that changed last and when: read once it has been quiet for [`LEVEL_QUIET`].
    level: Option<(NodeKey, Instant)>,
    last_level: Option<(u64, Instant)>,
    /// Counters, for the e2e gates and the log.
    pub(crate) notices_spoken: u64,
    pub(crate) announcements_spoken: u64,
    pub(crate) notice_reads_refused: u64,
    /// Utterances over an owner's budget that were replaced by a newer one or dropped.
    pub(crate) notices_dropped: u64,
}

impl NoticeCtx {
    pub(crate) fn new() -> Self {
        Self {
            fair: Fair::new(Instant::now()),
            level: None,
            last_level: None,
            notices_spoken: 0,
            announcements_spoken: 0,
            notice_reads_refused: 0,
            notices_dropped: 0,
        }
    }

    /// May `owner` have another window activation handled (title read and spoken)?
    pub(crate) fn allow_activation(&mut self, owner: Owner, now: Instant) -> bool {
        let ok = self.fair.allow(owner, Kind::Activate, now);
        if !ok {
            self.notice_reads_refused += 1;
        }
        ok
    }
}

fn hash_text(s: &str) -> u64 {
    // FNV-1a: no allocation, stable, good enough to tell two short strings apart.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

impl Reader {
    /// Is a node of this role read as a notice when it appears? Alerts and notifications
    /// everywhere; for the rest the application's installed policy decides (the Shell's
    /// extension says its dialogs are, ADR-0008: the rule is data in `ext-gnome-shell`, not a
    /// branch here).
    fn reads_as_notice(&self, app: AppId, role: Role) -> bool {
        use crate::builtin_ext::{PolicyFacts, PolicyMoment, PolicyVerdict};
        if is_notice_role(role) {
            return true;
        }
        if !self.policy_active(app) {
            return false;
        }
        let facts = PolicyFacts {
            moment: PolicyMoment::Showing,
            role,
            states: gorca_model::StateSet::empty(),
            name: None,
            description: None,
            range: None,
        };
        self.decide_policy(app, &facts) == PolicyVerdict::Notice
    }

    /// Speak `u` for `app` within its owner's budget, else keep it as the newest waiting one.
    pub(crate) fn notice_speak(&mut self, app: AppId, u: Utterance, fx: &mut ReaderEffects) {
        let now = self.now;
        let owner = self.dir.owner(app);
        if self.notice.fair.allow(owner, Kind::Speak, now) {
            fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
            return;
        }
        // Over budget: the newest replaces the older waiting one (drop-oldest, counted); the
        // shared overflow buckets keep none.
        self.notice.notices_dropped += self.notice.fair.wait(owner, u, now);
        if let Some(due) = self.notice.fair.next_due() {
            fx.push(ReaderEffect::ArmTimer(due));
        }
    }

    /// A timer fired: speak what waited for an owner's budget.
    pub(crate) fn notice_timer(&mut self, fx: &mut ReaderEffects) {
        let now = self.now;
        for u in self.notice.fair.take_due(now) {
            // Its epoch may be stale by now: the lane drops what the user moved on from (a
            // polite announcement); a banner survives.
            fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
        }
        if let Some(at) = self.notice.fair.next_due() {
            // Never a busy loop: a token short by rounding is waited out for a millisecond.
            fx.push(ReaderEffect::ArmTimer(
                at.max(now + Duration::from_millis(1)),
            ));
        }
    }

    /// `object:announcement` from `app`.
    pub(crate) fn on_announcement(
        &mut self,
        app: AppId,
        text: &str,
        politeness: Politeness,
        fx: &mut ReaderEffects,
    ) {
        let now = self.now;
        let h = hash_text(text);
        let owner = self.dir.owner(app);
        let (e, _) = self.notice.fair.entry(owner, now);
        if let Some((last, at)) = e.last_announcement
            && last == h
            && now.saturating_duration_since(at) < DUPLICATE_ANNOUNCEMENT
        {
            return;
        }
        e.last_announcement = Some((h, now));
        let id = self.utt_id();
        let Some(u) = announcement_utterance(text, politeness, id, self.epoch.current()) else {
            return;
        };
        self.notice.announcements_spoken += 1;
        self.notice_speak(app, u, fx);
    }

    /// A node of `app` became visible (`state-changed:showing`): read it if it is a notice.
    pub(crate) fn on_showing(&mut self, node: NodeKey, fx: &mut ReaderEffects) {
        let now = self.now;
        // The mirror knows what it is already: a node of another role costs nothing.
        if let Some(snap) = self.dir.lookup(node).hit()
            && !self.reads_as_notice(node.app, snap.role)
        {
            return;
        }
        let epoch = self.epoch.current();
        let has_policy = self.dir.has_policy(node.app);
        let owner = self.dir.owner(node.app);
        let (e, _) = self.notice.fair.entry(owner, now);
        e.asked
            .retain(|(_, at)| now.saturating_duration_since(*at) < NOTICE_NODE_GAP);
        if e.asked.iter().any(|(n, _)| *n == node) {
            return;
        }
        let kind = if has_policy { Kind::Probe } else { Kind::Read };
        if !e.take(kind, now) {
            self.notice.notice_reads_refused += 1;
            return;
        }
        if e.asked.len() >= ASKED_CAP {
            e.asked.remove(0);
        }
        e.asked.push((node, now));
        fx.push(ReaderEffect::Desktop(DesktopEffect::Notice { node, epoch }));
    }

    /// The value of a node that does not hold focus changed (the OSD level bar): note it, and
    /// read it when the animation is over.
    pub(crate) fn on_level_changed(&mut self, node: NodeKey, fx: &mut ReaderEffects) {
        if !self.policy_active(node.app) {
            return;
        }
        let now = self.now;
        self.notice.level = Some((node, now));
        fx.push(ReaderEffect::ArmTimer(now + LEVEL_QUIET));
    }

    /// A timer fired: read the level bar if it has been quiet long enough.
    pub(crate) fn level_timer(&mut self, fx: &mut ReaderEffects) {
        let Some((node, at)) = self.notice.level else {
            return;
        };
        let now = self.now;
        let due = at + LEVEL_QUIET;
        if now < due {
            fx.push(ReaderEffect::ArmTimer(due));
            return;
        }
        self.notice.level = None;
        let owner = self.dir.owner(node.app);
        let (e, _) = self.notice.fair.entry(owner, now);
        if !e.take(Kind::Probe, now) {
            self.notice.notice_reads_refused += 1;
            return;
        }
        fx.push(ReaderEffect::Desktop(DesktopEffect::Level { node }));
    }

    /// The application answered a level read: the installed policy decides what is said.
    pub(crate) fn on_level_read(&mut self, info: &WindowInfo, fx: &mut ReaderEffects) {
        use crate::builtin_ext::{PolicyFacts, PolicyMoment, PolicyVerdict};
        let Some(snap) = info.snap.as_ref() else {
            return;
        };
        if !self.policy_active(snap.key.app) {
            return;
        }
        let range = match &snap.value {
            Some(gorca_model::Value::Range { current, min, max }) => Some((*current, *min, *max)),
            Some(gorca_model::Value::Text(_)) | None => None,
        };
        let label = info.texts.first().map(|t| &**t);
        let facts = PolicyFacts {
            moment: PolicyMoment::Value,
            role: snap.role,
            states: snap.states,
            name: label.or(snap.name.as_deref()),
            description: snap.description.as_deref(),
            range,
        };
        let PolicyVerdict::Say(segs) = self.decide_policy(snap.key.app, &facts) else {
            return;
        };
        let text: String = segs.iter().map(|s| &*s.text).collect::<Vec<_>>().join(",");
        let h = hash_text(&text);
        let now = self.now;
        if let Some((last, at)) = self.notice.last_level
            && last == h
            && now.saturating_duration_since(at) < LEVEL_DUPLICATE
        {
            return;
        }
        self.notice.last_level = Some((h, now));
        let id = self.utt_id();
        let u = super::window::follow_up(segs, id, self.epoch.current());
        self.notice_speak(snap.key.app, u, fx);
    }

    /// The application answered a notice read.
    pub(crate) fn on_notice(&mut self, info: &WindowInfo, fx: &mut ReaderEffects) {
        let Some(snap) = info.snap.as_ref() else {
            return;
        };
        if !self.reads_as_notice(snap.key.app, snap.role) {
            return;
        }
        let segs = notice_segments(snap, &info.texts);
        // Only the role word: an empty notification says nothing worth speaking.
        if segs.len() < 2 {
            return;
        }
        let id = self.utt_id();
        let mut u = super::window::follow_up(segs, id, self.epoch.current());
        // A banner is a message the user has not asked for and may miss: it outlives the next
        // focus change (and still waits for what is being spoken).
        u.survives_focus_change = true;
        self.notice.notices_spoken += 1;
        self.notice_speak(info.window.app, u, fx);
    }
}

#[cfg(test)]
#[allow(clippy::wildcard_enum_match_arm)]
mod tests {
    /// The bursts of `fair` (reads, speech, activations per second) and its window.
    const NOTICE_RATE_MAX: usize = 8;
    const NOTICE_SPEAK_MAX: usize = 3;
    const ACTIVATION_MAX: usize = 4;
    const NOTICE_RATE_WINDOW: Duration = Duration::from_secs(1);

    use super::super::window::tests::{Fx, app, fixture, key, node, prios, said, snap};
    use super::*;
    use crate::reader::{BackgroundMsg, ReaderMsg};

    fn bg(f: &mut Fx, m: BackgroundMsg, at: Instant) -> ReaderEffects {
        f.reader.handle(ReaderMsg::Background(m), at)
    }

    fn ann(text: &str, p: Politeness) -> BackgroundMsg {
        BackgroundMsg::Announcement {
            app: app(1),
            text: Arc::from(text),
            politeness: p,
        }
    }

    #[test]
    fn announcement_text_is_cleaned_and_bounded() {
        assert_eq!(clean_announcement("  a \n\t b\u{7}c  "), "a b c");
        assert_eq!(clean_announcement("\n\n"), "");
        let long = "x".repeat(ANNOUNCE_MAX_CHARS * 3);
        assert_eq!(
            clean_announcement(&long).chars().count(),
            ANNOUNCE_MAX_CHARS
        );
        // A multi-byte character is never cut in the middle.
        let wide = "\u{442}".repeat(ANNOUNCE_MAX_CHARS + 5);
        assert_eq!(
            clean_announcement(&wide).chars().count(),
            ANNOUNCE_MAX_CHARS
        );
    }

    #[test]
    fn an_announcement_never_interrupts_and_never_advances_the_epoch() {
        let mut f = fixture(&[]);
        let t = Instant::now();
        let before = f.epoch.current();
        let fx = bg(&mut f, ann("Download complete", Politeness::Polite), t);
        assert_eq!(said(&fx), ["Download complete"]);
        assert_eq!(prios(&fx), [Prio::Notification]);
        assert_eq!(f.epoch.current(), before);
        let ReaderEffect::Speak(s) = &fx[0] else {
            panic!("speech")
        };
        assert!(
            !s.utterance.survives_focus_change,
            "polite: dies when stale"
        );
    }

    #[test]
    fn an_assertive_announcement_survives_a_focus_change() {
        let mut f = fixture(&[]);
        let fx = bg(
            &mut f,
            ann("Connection lost", Politeness::Assertive),
            Instant::now(),
        );
        let ReaderEffect::Speak(s) = &fx[0] else {
            panic!("speech")
        };
        assert!(s.utterance.survives_focus_change);
        assert_eq!(s.utterance.prio, Prio::Notification);
    }

    #[test]
    fn the_same_announcement_twice_in_a_row_is_spoken_once() {
        let mut f = fixture(&[]);
        let t = Instant::now();
        assert_eq!(bg(&mut f, ann("Saved", Politeness::Polite), t).len(), 1);
        let again = bg(
            &mut f,
            ann("Saved", Politeness::Polite),
            t + Duration::from_millis(100),
        );
        assert!(again.is_empty());
        let later = bg(
            &mut f,
            ann("Saved", Politeness::Polite),
            t + DUPLICATE_ANNOUNCEMENT + Duration::from_millis(1),
        );
        assert_eq!(later.len(), 1);
        assert!(bg(&mut f, ann("   ", Politeness::Polite), t).is_empty());
    }

    fn showing(n: u64) -> BackgroundMsg {
        BackgroundMsg::Showing { node: key(n) }
    }

    fn asks(fx: &ReaderEffects) -> usize {
        fx.iter()
            .filter(|e| matches!(e, ReaderEffect::Desktop(DesktopEffect::Notice { .. })))
            .count()
    }

    #[test]
    fn a_node_the_mirror_knows_to_be_no_notice_costs_no_round_trip() {
        let mut f = fixture(&[
            node(1, Role::Button, "OK", &[]),
            node(2, Role::Alert, "Hi", &[]),
        ]);
        let t = Instant::now();
        assert_eq!(asks(&bg(&mut f, showing(1), t)), 0);
        assert_eq!(asks(&bg(&mut f, showing(2), t)), 1);
    }

    #[test]
    fn an_unknown_node_is_read_once_and_reads_are_rate_limited() {
        let mut f = fixture(&[]);
        let t = Instant::now();
        assert_eq!(asks(&bg(&mut f, showing(10), t)), 1);
        assert_eq!(
            asks(&bg(&mut f, showing(10), t + Duration::from_millis(50))),
            0,
            "the same banner re-showing while it animates"
        );
        let mut total = 1;
        for n in 11..40 {
            total += asks(&bg(&mut f, showing(n), t + Duration::from_millis(60)));
        }
        assert_eq!(total, NOTICE_RATE_MAX, "a widget flood is bounded");
        assert!(f.reader.notice.notice_reads_refused > 0);
        // A second later the budget is back.
        let later = t + NOTICE_RATE_WINDOW + Duration::from_millis(100);
        assert_eq!(asks(&bg(&mut f, showing(50), later)), 1);
    }

    fn info(role: Role, name: &'static str, texts: &[&str]) -> WindowInfo {
        WindowInfo {
            window: key(7),
            snap: Some(snap(&node(7, role, name, &[]))),
            texts: texts.iter().map(|t| Arc::from(*t)).collect(),
        }
    }

    #[test]
    fn a_notice_speaks_role_name_and_text_without_interrupting_and_survives() {
        let mut f = fixture(&[]);
        let before = f.epoch.current();
        let fx = bg(
            &mut f,
            BackgroundMsg::Notice {
                info: info(
                    Role::Alert,
                    "Message from Alice",
                    &["Hi there", "Message from Alice"],
                ),
            },
            Instant::now(),
        );
        assert_eq!(said(&fx), ["alert, Message from Alice, Hi there"]);
        assert_eq!(prios(&fx), [Prio::Notification]);
        assert_eq!(f.epoch.current(), before, "a notice cancels nothing");
        let ReaderEffect::Speak(s) = &fx[0] else {
            panic!("speech")
        };
        assert!(s.utterance.survives_focus_change);
        assert_eq!(f.reader.notice.notices_spoken, 1);
    }

    #[test]
    fn what_is_not_a_notice_or_says_nothing_is_not_spoken() {
        let mut f = fixture(&[]);
        let t = Instant::now();
        let button = BackgroundMsg::Notice {
            info: info(Role::Button, "OK", &["x"]),
        };
        assert!(bg(&mut f, button, t).is_empty());
        let empty = BackgroundMsg::Notice {
            info: info(Role::Alert, "", &[]),
        };
        assert!(bg(&mut f, empty, t).is_empty());
        let none = BackgroundMsg::Notice {
            info: WindowInfo {
                window: key(7),
                snap: None,
                texts: vec![],
            },
        };
        assert!(bg(&mut f, none, t).is_empty());
    }

    fn ann_from(a: u32, text: &str) -> BackgroundMsg {
        BackgroundMsg::Announcement {
            app: app(a),
            text: Arc::from(text),
            politeness: Politeness::Assertive,
        }
    }

    #[test]
    fn a_flooding_app_spends_only_its_own_budget() {
        let mut f = fixture(&[node(1, Role::Button, "OK", &[])]);
        let t = Instant::now();
        let mut spoken = 0;
        for i in 0..40 {
            spoken += said(&bg(&mut f, ann_from(7, &format!("spam {i}")), t)).len();
        }
        assert_eq!(spoken, NOTICE_SPEAK_MAX, "the flood is bounded per window");
        // Another app's announcement and banner are not suppressed by it.
        let other = bg(&mut f, ann_from(8, "Battery low"), t);
        assert_eq!(said(&other), ["Battery low"]);
        let banner = BackgroundMsg::Notice {
            info: WindowInfo {
                window: NodeKey {
                    app: app(8),
                    local: key(7).local,
                },
                snap: Some(snap(&node(7, Role::Alert, "Alice", &[]))),
                texts: vec![Arc::from("Hi")],
            },
        };
        assert_eq!(said(&bg(&mut f, banner, t)), ["alert, Alice, Hi"]);
        // Node reads are per app too.
        for n in 0..40 {
            let m = BackgroundMsg::Showing {
                node: NodeKey {
                    app: app(7),
                    local: key(100 + n).local,
                },
            };
            bg(&mut f, m, t);
        }
        let m = BackgroundMsg::Showing {
            node: NodeKey {
                app: app(8),
                local: key(500).local,
            },
        };
        assert_eq!(asks(&bg(&mut f, m, t)), 1);
        // Focus speech is not delayed: it never passes through a notice budget.
        let fx = f.reader.handle(
            ReaderMsg::Focus(crate::reader::FocusLaneMsg::FocusChanged { node: key(1) }),
            t,
        );
        assert_eq!(said(&fx), ["OK, button"]);
        assert_eq!(prios(&fx), [Prio::Interrupt]);
    }

    #[test]
    fn over_budget_keeps_only_the_newest_and_speaks_it_when_the_window_ends() {
        let mut f = fixture(&[]);
        let t = Instant::now();
        for i in 0..NOTICE_SPEAK_MAX {
            bg(&mut f, ann_from(7, &format!("early {i}")), t);
        }
        let fx = bg(&mut f, ann_from(7, "old waiting"), t);
        assert!(said(&fx).is_empty());
        assert!(fx.iter().any(|e| matches!(e, ReaderEffect::ArmTimer(_))));
        bg(&mut f, ann_from(7, "newest waiting"), t);
        assert_eq!(f.reader.notice.notices_dropped, 1, "drop-oldest is counted");
        let fx = f.reader.handle(ReaderMsg::Timer, t + NOTICE_RATE_WINDOW);
        assert_eq!(said(&fx), ["newest waiting"]);
    }

    #[test]
    fn the_budget_table_is_bounded() {
        let mut f = fixture(&[]);
        let t = Instant::now();
        for a in 0..100 {
            bg(&mut f, ann_from(a, "x"), t);
        }
        assert!(f.reader.notice.fair.len() <= super::super::fair::TABLE_MAX);
    }

    // ---- rate-limit bypass vectors ----

    fn spent(f: &mut Fx, a: u32, t: Instant) -> usize {
        (0..20)
            .map(|i| said(&bg(f, ann_from(a, &format!("m{a}-{i}")), t)).len())
            .sum()
    }

    #[test]
    fn many_connections_of_one_process_share_one_budget() {
        let mut f = fixture(&[]);
        let t = Instant::now();
        for a in 20..40 {
            f.reader.dir.set_pid(app(a), Some(4242));
        }
        let total: usize = (20..40).map(|a| spent_one(&mut f, a, t)).sum();
        assert_eq!(total, NOTICE_SPEAK_MAX, "twenty bus names, one budget");
        // Another process is untouched.
        f.reader.dir.set_pid(app(50), Some(77));
        assert_eq!(said(&bg(&mut f, ann_from(50, "mine"), t)), ["mine"]);
    }

    fn spent_one(f: &mut Fx, a: u32, t: Instant) -> usize {
        said(&bg(f, ann_from(a, &format!("from {a}")), t)).len()
    }

    #[test]
    fn varying_text_node_role_or_politeness_does_not_escape_the_budget() {
        let mut f = fixture(&[]);
        let t = Instant::now();
        let mut n = 0;
        for i in 0..30 {
            let p = if i % 2 == 0 {
                Politeness::Polite
            } else {
                Politeness::Assertive
            };
            let m = BackgroundMsg::Announcement {
                app: app(7),
                text: Arc::from(format!("t{i}")),
                politeness: p,
            };
            n += said(&bg(&mut f, m, t)).len();
        }
        // Banners of distinct nodes and roles spend the same per-owner speech budget.
        for i in 0..30u64 {
            let info = WindowInfo {
                window: NodeKey {
                    app: app(7),
                    local: key(300 + i).local,
                },
                snap: Some(snap(&node(300 + i, Role::Alert, "x", &[]))),
                texts: vec![Arc::from(format!("body {i}"))],
            };
            n += said(&bg(&mut f, BackgroundMsg::Notice { info }, t)).len();
        }
        assert_eq!(n, NOTICE_SPEAK_MAX, "announcements and banners share it");
    }

    #[test]
    fn many_short_announcements_are_bounded_by_count_and_the_waiting_slot_is_single() {
        let mut f = fixture(&[]);
        let t = Instant::now();
        for i in 0..500 {
            bg(&mut f, ann_from(7, &format!("{i}")), t);
        }
        let owner = f.reader.dir.owner(app(7));
        let waiting = usize::from(f.reader.notice.fair.entry(owner, t).0.waiting.is_some());
        assert_eq!(waiting, 1);
        assert_eq!(
            f.reader.notice.notices_dropped,
            500 - NOTICE_SPEAK_MAX as u64 - 1
        );
    }

    #[test]
    fn a_window_activation_flood_is_bounded_per_owner() {
        let mut f = fixture(&[]);
        let t = Instant::now();
        let mut handled = 0;
        for w in 0..30u64 {
            let fx = f.reader.handle(
                ReaderMsg::Focus(crate::reader::FocusLaneMsg::WindowActivated {
                    window: key(100 + w),
                }),
                t + Duration::from_millis(w),
            );
            handled += fx
                .iter()
                .filter(|e| matches!(e, ReaderEffect::Desktop(DesktopEffect::Window { .. })))
                .count();
        }
        assert_eq!(
            handled, ACTIVATION_MAX,
            "reads and speech per owner are bounded"
        );
        // Another owner's window still activates.
        let other = f.reader.handle(
            ReaderMsg::Focus(crate::reader::FocusLaneMsg::WindowActivated {
                window: NodeKey {
                    app: app(9),
                    local: key(1).local,
                },
            }),
            t + Duration::from_millis(40),
        );
        assert!(
            other
                .iter()
                .any(|e| matches!(e, ReaderEffect::Desktop(DesktopEffect::Window { .. })))
        );
    }

    #[test]
    fn creating_owners_does_not_reset_a_busy_owners_budget() {
        let mut f = fixture(&[]);
        let t = Instant::now();
        assert_eq!(spent(&mut f, 7, t), NOTICE_SPEAK_MAX);
        for a in 1000..1400 {
            f.reader.dir.set_pid(app(a), Some(a));
            bg(&mut f, ann_from(a, "x"), t);
        }
        assert!(said(&bg(&mut f, ann_from(7, "again"), t)).is_empty());
        assert!(f.reader.notice.fair.len() <= super::super::fair::TABLE_MAX);
    }

    #[test]
    fn a_quiet_app_is_not_starved_by_a_flooding_one_across_every_presenter() {
        let mut f = fixture(&[]);
        let t = Instant::now();
        // The flooder: announcements, banners, node reads and window activations.
        spent(&mut f, 7, t);
        for n in 0..40u64 {
            let m = BackgroundMsg::Showing {
                node: NodeKey {
                    app: app(7),
                    local: key(100 + n).local,
                },
            };
            bg(&mut f, m, t);
            f.reader.handle(
                ReaderMsg::Focus(crate::reader::FocusLaneMsg::WindowActivated {
                    window: NodeKey {
                        app: app(7),
                        local: key(200 + n).local,
                    },
                }),
                t + Duration::from_millis(n),
            );
        }
        // The quiet app gets its full budget on every path.
        assert_eq!(said(&bg(&mut f, ann_from(8, "quiet"), t)), ["quiet"]);
        let show = BackgroundMsg::Showing {
            node: NodeKey {
                app: app(8),
                local: key(5).local,
            },
        };
        assert_eq!(asks(&bg(&mut f, show, t)), 1);
        let fx = f.reader.handle(
            ReaderMsg::Focus(crate::reader::FocusLaneMsg::WindowActivated {
                window: NodeKey {
                    app: app(8),
                    local: key(300).local,
                },
            }),
            t + Duration::from_millis(50),
        );
        assert!(
            fx.iter()
                .any(|e| matches!(e, ReaderEffect::Desktop(DesktopEffect::Window { .. })))
        );
    }

    #[derive(Debug)]
    struct SayLevel;

    impl crate::builtin_ext::AppPolicy for SayLevel {
        fn decide(
            &self,
            f: &crate::builtin_ext::PolicyFacts<'_>,
        ) -> crate::builtin_ext::PolicyVerdict {
            use crate::builtin_ext::{PolicyMoment, PolicyVerdict};
            // Like `ext-gnome-shell`: a dialog that appears reads as a notice.
            if f.moment == PolicyMoment::Showing {
                return if f.role == Role::Dialog {
                    PolicyVerdict::Notice
                } else {
                    PolicyVerdict::Default
                };
            }
            if f.moment != PolicyMoment::Value {
                return PolicyVerdict::Default;
            }
            PolicyVerdict::Say(vec![gorca_model::Segment {
                text: Arc::from(f.name.unwrap_or("level")),
                kind: gorca_model::SegmentKind::Text,
            }])
        }
    }

    fn level_fixture(policy: bool) -> Fx {
        let mut f = fixture(&[]);
        f.reader = f.reader.with_app_policy(Arc::new(SayLevel));
        f.reader.dir.set_policy(app(1), policy);
        f
    }

    fn level_effects(fx: &ReaderEffects) -> usize {
        fx.iter()
            .filter(|e| matches!(e, ReaderEffect::Desktop(DesktopEffect::Level { .. })))
            .count()
    }

    #[test]
    fn a_level_bar_is_read_once_after_its_animation_stops() {
        let mut f = level_fixture(true);
        let t = Instant::now();
        let ev = |f: &mut Fx, ms: u64| {
            bg(
                f,
                BackgroundMsg::LevelChanged { node: key(40) },
                t + Duration::from_millis(ms),
            )
        };
        for i in 0..8 {
            ev(&mut f, i * 17);
        }
        // Early timers re-arm; nothing is read while it still moves.
        let early = f
            .reader
            .handle(ReaderMsg::Timer, t + Duration::from_millis(150));
        assert_eq!(level_effects(&early), 0);
        let quiet = f.reader.handle(
            ReaderMsg::Timer,
            t + Duration::from_millis(7 * 17) + LEVEL_QUIET,
        );
        assert_eq!(level_effects(&quiet), 1, "one read for the whole animation");
        // The answer is spoken through the policy, queued, once.
        let info = WindowInfo {
            window: key(40),
            snap: Some(snap(&node(40, Role::ProgressBar, "", &[]))),
            texts: vec![Arc::from("Volume")],
        };
        let t2 = t + Duration::from_secs(1);
        let said1 = said(&bg(
            &mut f,
            BackgroundMsg::LevelRead { info: info.clone() },
            t2,
        ));
        assert_eq!(said1, ["Volume"]);
        let again = bg(
            &mut f,
            BackgroundMsg::LevelRead { info },
            t2 + Duration::from_millis(50),
        );
        assert!(said(&again).is_empty(), "the same level is not repeated");
    }

    #[test]
    fn levels_of_apps_without_the_policy_are_ignored() {
        let mut f = level_fixture(false);
        let fx = bg(
            &mut f,
            BackgroundMsg::LevelChanged { node: key(40) },
            Instant::now(),
        );
        assert!(fx.is_empty());
    }

    #[test]
    fn a_dialog_that_appears_is_read_as_a_notice_only_in_an_app_with_a_policy() {
        let t = Instant::now();
        let note = |role| BackgroundMsg::Notice {
            info: info(role, "", &["Run a Command", "Press ESC to close"]),
        };
        let mut f = level_fixture(true);
        let fx = bg(&mut f, note(Role::Dialog), t);
        assert_eq!(said(&fx), ["dialog, Run a Command, Press ESC to close"]);
        assert_eq!(prios(&fx), [Prio::Notification]);
        let mut f = level_fixture(false);
        assert!(bg(&mut f, note(Role::Dialog), t).is_empty());
    }
}
