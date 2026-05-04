//! Rust AST transformation skeleton.
//!
//! Mirrors `crate::solidity::transform`. The Solidity transform pass
//! wraps function-call arguments and remaps interface members onto
//! their implementations. Rust has analogous concerns (e.g. desugaring
//! `?` operators, lifting trait method calls into their concrete impls)
//! that the future implementation will encode here. Today this is a
//! no-op so `crate::rust::analyzer::analyze` can call through it
//! without conditional logic.

use o11a_core::domain::ProjectPath;
use o11a_core::rust::ast::RustAST;

use std::collections::{BTreeMap, HashSet};

/// Apply Rust-side AST transformations in place. Skeleton — no-op.
pub fn transform_ast(
  _ast_map: &mut BTreeMap<ProjectPath, Vec<RustAST>>,
  _in_scope_files: &HashSet<ProjectPath>,
) -> Result<(), String> {
  Ok(())
}
