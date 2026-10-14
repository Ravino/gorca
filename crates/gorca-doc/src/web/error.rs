use thiserror::Error;

use gorca_model::DocNodeId;

/// Why an operation was rejected. A rejected operation never mutates the document.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Error)]
pub enum DocError {
    #[error("node {0:?} is stale (freed or never existed)")]
    Stale(DocNodeId),
    #[error("node {0:?} is not in the document tree (detached or inside a detached subtree)")]
    NotAttached(DocNodeId),
    #[error("node {0:?} is not a detached subtree root")]
    NotDetached(DocNodeId),
    #[error("the document already has a root")]
    RootExists,
    #[error("the document has no root")]
    NoRoot,
    #[error("the document root cannot be removed or moved")]
    RootImmutable,
    #[error("node {0:?} is not a child of the given parent")]
    NotChild(DocNodeId),
    #[error("moving node {0:?} there would create a cycle")]
    Cycle(DocNodeId),
    #[error("node {0:?} appears twice in the desired child list")]
    Duplicate(DocNodeId),
    #[error("the desired order is not a permutation of the current children")]
    NotPermutation,
    #[error("arena is full (u32 slot space exhausted)")]
    Full,
}
