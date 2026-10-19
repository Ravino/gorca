// SPDX-License-Identifier: GPL-3.0-or-later
//! Utterances (ADR-0001 sections 7.4 and 8.1).

use std::sync::Arc;

use smallvec::SmallVec;

use crate::ids::{Epoch, UttId};

/// Speech priority classes, highest first.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Prio {
    Interrupt,
    Message,
    Text,
    Progress,
    Notification,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SegmentKind {
    Text,
    Role,
    State,
    Value,
    /// An accessible description (`aria-describedby`): set off from the words before it by a
    /// sentence break instead of the comma that joins segments, like NVDA's separate description
    /// sequence (M8b). `browse.description_pause = false` turns it into plain `Text`. Carried as a
    /// kind, never as characters of the text, so page text that starts with ". " is untouched.
    Description,
    /// Text that is already verbalised (symbols named by the punctuation level) or is the
    /// reader's own message: never symbol-processed again on its way to the speech lane.
    Said,
    /// A capital letter spoken alone (`speech.caps = "pitch"`): already verbalised like
    /// `Said`, and the speech client speaks it with a raised pitch (NVDA `capPitchChange`).
    Capital,
}

/// Which catalog message a segment is, or that it is a literal (page text, the reader's own
/// words that have no catalog entry yet). Feature `seg-origin` only.
#[cfg(feature = "seg-origin")]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum MsgId {
    /// Not a catalog message: text of the page or of an extension.
    #[default]
    Literal,
    /// A catalog id (`gorca-i18n`), by its dense number.
    Catalog(u32),
}

/// Which producer made a segment: the rule row, node and group that contributed it. `0` is
/// "unknown"; the presentation pipeline (M22b) assigns the real ones. Feature `seg-origin` only.
#[cfg(feature = "seg-origin")]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct OriginId(pub u32);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Segment {
    pub text: Arc<str>,
    pub kind: SegmentKind,
    /// The message this segment renders (feature `seg-origin`).
    #[cfg(feature = "seg-origin")]
    pub msg: MsgId,
    /// The producer of this segment (feature `seg-origin`).
    #[cfg(feature = "seg-origin")]
    pub origin: OriginId,
}

impl Segment {
    /// A segment of `text` and `kind`; with `seg-origin`, a literal of unknown origin.
    pub fn new(text: Arc<str>, kind: SegmentKind) -> Segment {
        Segment {
            text,
            kind,
            #[cfg(feature = "seg-origin")]
            msg: MsgId::Literal,
            #[cfg(feature = "seg-origin")]
            origin: OriginId(0),
        }
    }

    /// The same segment tagged with its message (`0` is a literal) and producer; a no-op without
    /// `seg-origin`, so producers can tag unconditionally.
    #[must_use]
    #[cfg(feature = "seg-origin")]
    pub fn tagged(mut self, msg: u32, origin: u32) -> Segment {
        self.msg = if msg == 0 {
            MsgId::Literal
        } else {
            MsgId::Catalog(msg)
        };
        self.origin = OriginId(origin);
        self
    }

    /// See the `seg-origin` form: without the feature a segment carries no tags.
    #[must_use]
    #[cfg(not(feature = "seg-origin"))]
    pub fn tagged(self, _msg: u32, _origin: u32) -> Segment {
        self
    }
}

/// Output of the presenter, input of the verbalizer and the Braille actor.
#[derive(Clone)]
pub struct Utterance {
    pub id: UttId,
    pub segments: SmallVec<[Segment; 4]>,
    pub lang: Option<Arc<str>>,
    /// E-1: the epoch this utterance was created in.
    pub epoch: Epoch,
    pub prio: Prio,
    /// Built from stale data; a refined utterance may replace it (section 8.1).
    pub may_refine: bool,
    /// Policy escape from the E-2 check, for example say-all progress.
    pub survives_focus_change: bool,
    /// Not worth speaking later than this after it was queued (live-region news, ADR-0003 13.2:
    /// 5 s polite, 10 s assertive). Expired entries are dropped from the speech queue unspoken,
    /// and rank below non-expiring ones of the same priority when the queue is full.
    pub ttl_secs: u8,
    /// A correction of an earlier utterance (B-1 RV-1): the cache-served presentation `id` was
    /// wrong in a presented field. A refine replaces it; a follow-up only adds the change.
    pub refines: Option<UttId>,
}

impl std::fmt::Debug for Utterance {
    // The text may come from a password field (nothing marks it): only its length is shown.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let chars: usize = self.segments.iter().map(|s| s.text.chars().count()).sum();
        f.debug_struct("Utterance")
            .field("id", &self.id)
            .field("segments", &self.segments.len())
            .field("chars", &chars)
            .field("epoch", &self.epoch)
            .field("prio", &self.prio)
            .field("may_refine", &self.may_refine)
            .field("survives_focus_change", &self.survives_focus_change)
            .field("ttl_secs", &self.ttl_secs)
            .field("refines", &self.refines)
            .field("has_lang", &self.lang.is_some())
            .finish()
    }
}

impl Utterance {
    /// E-2: valid iff `epoch >= current` or the survival policy applies.
    pub fn is_valid(&self, current: Epoch) -> bool {
        self.epoch >= current || self.survives_focus_change
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utt(epoch: u64, survives: bool) -> Utterance {
        Utterance {
            id: UttId(1),
            segments: SmallVec::new(),
            lang: None,
            epoch: Epoch(epoch),
            prio: Prio::Text,
            may_refine: false,
            survives_focus_change: survives,
            ttl_secs: 0,
            refines: None,
        }
    }

    #[test]
    fn validity_follows_epoch_and_policy() {
        assert!(utt(5, false).is_valid(Epoch(5)));
        assert!(utt(6, false).is_valid(Epoch(5)));
        assert!(!utt(4, false).is_valid(Epoch(5)));
        assert!(utt(4, true).is_valid(Epoch(5)));
    }

    #[test]
    fn tagging_never_changes_text_or_kind() {
        let a = Segment::new(Arc::from("hi"), SegmentKind::Text);
        let b = a.clone().tagged(7, 3);
        assert_eq!((&*a.text, a.kind), (&*b.text, b.kind));
        #[cfg(feature = "seg-origin")]
        {
            assert_ne!(a, b);
            assert_eq!(b.msg, MsgId::Catalog(7));
            assert_eq!(b.origin, OriginId(3));
            assert_eq!(a.msg, MsgId::Literal);
        }
        #[cfg(not(feature = "seg-origin"))]
        assert_eq!(a, b);
    }

    #[test]
    fn interrupt_sorts_before_notification() {
        assert!(Prio::Interrupt < Prio::Notification);
    }
}
