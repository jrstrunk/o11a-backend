//! Skeleton Rust language support, mirroring `crate::solidity`.
//!
//! Rust is not yet supported by the analyzer pipeline; this module
//! exists so the resolution-graph builder and the polyglot pieces of
//! the AST / topic-metadata layer have a stable place to plug Rust
//! types into. When the Rust analyzer lands it will populate the AST
//! types defined here and produce `NamedTopic` entries the
//! `RustExtractor` reads.

pub mod ast;

// Re-export AST types at the rust root so `o11a_core::rust::ASTNode`
// follows the same shape as `o11a_core::solidity::ASTNode`.
pub use ast::*;
