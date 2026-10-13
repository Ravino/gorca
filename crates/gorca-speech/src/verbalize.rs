use std::sync::Arc;

use gorca_model::Utterance;
use gorca_output_api::Verbalized;

/// Turns an utterance into server text. TBD: punctuation levels, symbol table, pronunciation
/// dictionary (section 8.1). Verbalisation runs before enqueueing, on the producer's thread
/// or `cpu`, never on the speech lane.
#[derive(Debug, Default)]
pub struct Verbalizer;

impl Verbalizer {
    pub fn verbalize(&self, u: &Utterance) -> Verbalized {
        let mut text = String::new();
        for seg in &u.segments {
            if seg.text.is_empty() {
                continue;
            }
            if !text.is_empty() {
                text.push_str(", ");
            }
            text.push_str(&seg.text);
        }
        Verbalized {
            text: Arc::from(text),
            lang: u.lang.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::speech::SegmentKind;
    use gorca_model::{Epoch, Prio, Segment, UttId};
    use smallvec::smallvec;

    #[test]
    fn segments_are_joined_and_empty_ones_skipped() {
        let seg = |t: &str, kind| Segment {
            text: Arc::from(t),
            kind,
        };
        let u = Utterance {
            id: UttId(1),
            segments: smallvec![
                seg("OK", SegmentKind::Text),
                seg("", SegmentKind::Value),
                seg("button", SegmentKind::Role)
            ],
            lang: Some(Arc::from("en")),
            epoch: Epoch(1),
            prio: Prio::Text,
            may_refine: false,
            survives_focus_change: false,
            refines: None,
        };
        let v = Verbalizer.verbalize(&u);
        assert_eq!(&*v.text, "OK, button");
        assert_eq!(v.lang.as_deref(), Some("en"));
    }
}
