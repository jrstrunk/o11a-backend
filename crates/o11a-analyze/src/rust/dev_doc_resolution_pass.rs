//! Rust dev-doc resolution pass skeleton.
//!
//! Mirrors `crate::solidity::dev_doc_resolution_pass`. The Solidity
//! pass walks every `Author::DevTechnical | DevDocumentation`
//! `CommentTopic` regardless of language origin — its implementation
//! is genuinely language-agnostic. Once the Rust analyzer produces
//! synthetic dev-doc CommentTopics for rustdoc and inline `//`
//! comments, those CommentTopics will be picked up by the existing
//! Solidity-named pass on the next iteration, so this skeleton stays a
//! no-op today.
//!
//! The file exists so `crate::analysis::run_analysis` can mirror its
//! Solidity counterpart 1:1 (one wrapper per language), keeping the
//! polyglot pipeline shape uniform. When the spec's
//! "Adding a new language" recipe wants Rust-specific resolution-pass
//! behavior (e.g. trait-method seed boosting), it lands here without
//! changing the analysis-level orchestration.

use o11a_core::domain::AuditData;

/// Run the Rust dev-doc resolution pass. Skeleton — no-op today; the
/// language-agnostic resolver in
/// `crate::solidity::dev_doc_resolution_pass::resolve_dev_doc_comments`
/// already handles every dev-doc CommentTopic regardless of language.
pub fn resolve_dev_doc_comments(_audit_data: &mut AuditData) {
  // Future: any Rust-specific seed adjustments or extra walks beyond
  // what the language-agnostic pass already does. Today there are no
  // Rust CommentTopics, so the function exits without mutating state.
}
