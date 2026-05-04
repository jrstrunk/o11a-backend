//! Rust analyzer skeleton.
//!
//! Mirrors `crate::solidity::analyzer` so that
//! `crate::analysis::run_analysis` can orchestrate the same
//! per-language stages for Rust as it does for Solidity (parse →
//! transform → first pass → tree shake → second pass → name index →
//! dev-doc injection). The Rust parser does not exist yet; until it
//! lands, every public entry point here is a no-op so polyglot
//! pipelines can be wired without crashing on a missing analyzer.
//!
//! When the Rust analyzer lands, this file becomes the home for the
//! actual passes — same shape as `solidity::analyzer::analyze`.
//! Downstream consumers (resolution-graph builder, documentation
//! analyzer, dev-doc resolution pass) read shared state on
//! `AuditData` and need no edits.

use crate::rust::parser;
use crate::rust::transform;
use o11a_core::domain::{AuditData, DataContext};
use std::path::Path;

/// Drive the Rust analyzer for a single audit. Skeleton — returns
/// `Ok(())` after a no-op parser/transform pair so callers wire up the
/// stage even though no Rust source is parsed today.
pub fn analyze(
  project_root: &Path,
  audit_id: &str,
  data_context: &mut DataContext,
) -> Result<(), String> {
  let audit_data = data_context
    .get_audit_mut(audit_id)
    .ok_or_else(|| format!("Audit '{}' not found", audit_id))?;

  // Parse all Rust ASTs (no-op skeleton today).
  let mut ast_map = parser::process(project_root).map_err(|e| e.to_string())?;

  // Transform pass (no-op skeleton today).
  transform::transform_ast(&mut ast_map, &audit_data.in_scope_files)?;

  // First / tree-shake / second / name-index passes will land here when
  // the Rust analyzer ships. Today there are no parsed Rust files so
  // there is nothing to walk; the function returns successfully so a
  // polyglot `run_analysis` continues into the resolution-graph build.

  Ok(())
}

/// Inject synthetic developer-documentation `CommentTopic`s for Rust
/// items (rustdoc `///` and `//!` comments, plus inline source
/// comments above expressions). Skeleton — same shape as
/// `crate::solidity::analyzer::inject_developer_documentation`,
/// signaled-out so `crate::analysis::run_analysis` can mirror its
/// Solidity counterpart without a missing-symbol failure.
pub fn inject_developer_documentation(_audit_data: &mut AuditData) {
  // Future: walk Rust ASTNode entries with attached docstrings,
  // create synthetic dev-doc CommentTopics via
  // `o11a_core::collaborator::synthetic::create_synthetic_dev_comment`,
  // and emit them through `comment_index`. The Solidity analyzer's
  // `inject_developer_documentation` is the reference implementation.
}
