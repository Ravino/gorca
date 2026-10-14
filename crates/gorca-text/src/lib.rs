//! The shared text layer (ADR-0007 section 3). Pure: no tokio, no I/O, no connector types.
//!
//! One implementation of every text rule, used by native text fields (a window over AT-SPI
//! `Text`), by browse mode (the document mirror's flat text) and by review:
//!
//! - [`seg`]: character (grapheme), word, sentence and soft-line segmentation of `&[char]`;
//! - [`space`]: the [`TextSpace`] trait and [`CharsSpace`], its implementation over a slice;
//! - [`units`]: raw offsets (UTF-8 bytes, Firefox UTF-16 with phantom U+FEFF) to scalars;
//! - [`caret`]: which unit to speak for which movement key, and the range to present;
//! - [`echo`]: the typing-echo model (expectation ring, insert dedupe, word buffer);
//! - [`sel`]: selection diffing;
//! - [`coalesce`]: folding bursts of edits into net splices;
//! - [`chunk`]: say-all chunking.
//!
//! All offsets are Unicode scalar offsets (TU-1). No function panics on any input.

pub mod caret;
pub mod chunk;
pub mod coalesce;
pub mod echo;
pub mod seg;
pub mod sel;
pub mod space;
pub mod units;

pub use space::{CharsSpace, Dir, Lookup, Need, Step, TextSpace, Unit};
