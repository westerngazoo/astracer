//! Stable, compact identifiers used across the whole pipeline.
//!
//! Ids are plain `u32` newtypes (Principle II: flat, cache-friendly data).
//! A [`NodeId`] is the index of a node inside a [`crate::graph::CodeGraph`];
//! a [`FileId`] indexes the graph's interned file table.

use serde::{Deserialize, Serialize};

/// Index of a node (callable symbol) inside a [`crate::graph::CodeGraph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId(pub u32);

/// Index of a source file inside a [`crate::graph::CodeGraph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FileId(pub u32);

impl NodeId {
    #[inline]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

impl FileId {
    #[inline]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "n{}", self.0)
    }
}
