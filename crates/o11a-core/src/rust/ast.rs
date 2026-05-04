//! Skeleton Rust AST types.
//!
//! Mirrors the shape of `crate::solidity::ast` so that the resolution
//! graph, web rendering, and analyzer scaffolding can reference a
//! `RustAST` / `ASTNode` pair without depending on a Rust parser. The
//! Rust analyzer is not yet implemented; until it lands, every
//! `RustAST` produced by the codebase is empty (no top-level nodes),
//! and the `ASTNode` enum has only a single placeholder variant. New
//! variants will be added alongside the parser.
//!
//! Determinism contract is identical to the Solidity AST: structures
//! are `Serialize`/`Deserialize`, and any code that walks them must do
//! so in declaration order so a future parser change does not introduce
//! observable non-determinism through this layer.

use serde::{Deserialize, Serialize};

use crate::domain::ProjectPath;

/// One Rust source file's AST. Mirrors `SolidityAST`'s shape so that
/// `domain::AST::Rust(RustAST)` slots into the same dispatch points.
///
/// `node_id` is reserved for parity with `SolidityAST`; the eventual
/// Rust parser may use it as a stable identifier for the file root.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RustAST {
  pub node_id: i32,
  pub nodes: Vec<ASTNode>,
  pub project_path: ProjectPath,
}

/// Source location inside a Rust file. Same byte/length encoding the
/// Solidity AST uses, kept structurally compatible so cross-language
/// tooling (delimiter rendering, dump output) does not need to branch
/// on language.
#[derive(
  Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize,
)]
pub struct SourceLocation {
  pub start: Option<usize>,
  pub length: Option<usize>,
  pub index: Option<usize>,
}

/// Skeleton Rust AST node enum. The `SourceFile` variant is the only
/// inhabitant today and exists so consumers that walk a `RustAST`'s
/// top-level nodes have a non-empty enum to pattern-match on. The Rust
/// parser will replace this with the real variant set (items, exprs,
/// patterns, etc.) when it lands.
///
/// The variant set mirrors the layered structure of
/// `crate::solidity::ast::ASTNode` — eventual additions go alongside
/// the parser and follow the spec's "Adding a new edge type" recipe
/// when they imply new graph relationships.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ASTNode {
  /// File root. Mirrors Solidity's `SourceUnit`.
  SourceFile {
    node_id: i32,
    src_location: SourceLocation,
    items: Vec<ASTNode>,
  },
}

impl ASTNode {
  pub fn src_location(&self) -> &SourceLocation {
    match self {
      ASTNode::SourceFile { src_location, .. } => src_location,
    }
  }

  pub fn node_id(&self) -> i32 {
    match self {
      ASTNode::SourceFile { node_id, .. } => *node_id,
    }
  }
}
