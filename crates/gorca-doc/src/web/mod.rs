//! Web-prep core (ADR-0003 sections 4.1, 4.4, 4.5, 4.8, 9.6; GM section 1.2): generational
//! arena, document-order treap with per-category summaries, flat-text map, incremental ops and
//! left-right publication. Pure data; no tokio, no I/O, no `gorca-core` types.
//!
//! Layout:
//! - [`cats`]: categories, flags, node and subtree specs (DM-4);
//! - [`doc`]: [`WebDoc`], the arena + tree + treap with navigation and text queries;
//! - [`ops`]: [`DocOp`] and its application, keyed child sync, signature alignment;
//! - [`publish`]: [`DocWriter`] / [`DocRead`], the double buffer;
//! - [`synth`]: deterministic synthetic pages and a tiny RNG for tests and benches.
//!
//! The treap's summary is a concrete struct rather than the `Summary` trait of the ADR sketch:
//! the trait costs a generic parameter on every query and no second implementation exists
//! until the B+tree upgrade path is taken (GM section 1.2 option E).
#![allow(
    clippy::cast_possible_truncation,
    reason = "arena indices and text offsets are u32 by design (NIL = u32::MAX)"
)]

pub mod cats;
pub mod doc;
pub mod error;
pub mod flat;
pub mod ops;
pub mod publish;
pub mod space;
pub mod synth;

pub use cats::{Cat, NodeFlags, NodeMeta, NodeSpec, PropsPatch, RANKED, SubtreeSpec, cats_of};
pub use doc::{Children, NodeView, Preorder, WebDoc};
pub use error::DocError;
pub use flat::{ReadUnit, word_range_in};
pub use gorca_model::DocNodeId;
pub use ops::{ChildSpec, DocOp, OpOutput, align_by_key};
pub use publish::{DocRead, DocWriter, PublishStats, channel, channel_from};
pub use space::{CHECKPOINT, LONG_LINE, WebSpace};
