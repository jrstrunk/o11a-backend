//! Rust parser skeleton.
//!
//! Mirrors `crate::solidity::parser` so that
//! `crate::rust::analyzer::analyze` has a stable parser entry point.
//! Returns an empty `BTreeMap` today — no Rust source on disk is
//! consulted yet. The eventual implementation will discover and parse
//! `.rs` files (or `cargo metadata` output, depending on the chosen
//! input shape) and produce one `RustAST` per source file.

use o11a_core::domain;
use o11a_core::rust::ast::RustAST;

use std::collections::BTreeMap;
use std::path::Path;

/// Errors produced by the Rust parser. Mirrors `solidity::parser::ParserError`'s
/// shape so the analyzer can pattern-match on the failure kind without
/// string introspection.
#[derive(Debug, thiserror::Error)]
pub enum ParserError {
  #[error("I/O error reading {path}: {source}")]
  Io {
    path: std::path::PathBuf,
    #[source]
    source: std::io::Error,
  },
  #[error("failed to parse Rust source: {0}")]
  Parse(String),
}

/// Discover Rust source files under `project_root` and parse them.
/// Skeleton — returns an empty map; no I/O is performed.
pub fn process(
  _project_root: &Path,
) -> Result<BTreeMap<domain::ProjectPath, Vec<RustAST>>, ParserError> {
  Ok(BTreeMap::new())
}
