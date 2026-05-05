//! BM25 expansion for the semantic-linking pipeline (Pass 1 contract
//! discovery + Pass 2 member expansion).
//!
//! See `docs/specs/semantic-linking.md` for the full design. This module
//! implements:
//! - Tokenization pipeline: compound-term detection → operator expansion →
//!   identifier splitting → abbreviation/domain expansion → lowercase →
//!   stop-word removal → Porter stemming.
//! - BM25 scoring with a length-floor variant (see `score.rs`) that caps
//!   the inflation very-short member documents would otherwise receive.
//! - A single cutoff: keep the top `TOP_K` candidates whose absolute score
//!   is at least `MIN_SCORE`, per (section, contract).
//!
//! The `TOP_K = 10` constant is the calibrated production cutoff; the
//! K=10 vs K=20 vs permissive trade-off is documented in the spec.

mod corpus;
mod score;
mod tokenize;

pub use corpus::{
  ContractDoc, MemberDoc, SummaryCorpusVariant, build_contract_member_corpus,
  build_contract_summary_corpus,
};
pub use score::score;
pub use tokenize::{tokenize_code_text, tokenize_prose_text};

use crate::domain::{AuditData, topic};

/// Expand the mechanical Pass 2 result with BM25-ranked members from a
/// single contract. Returns each kept member with its BM25 score so callers
/// can surface provenance.
///
/// Returns an empty vec on any of:
/// - empty section text,
/// - empty member corpus,
/// - cutoff returning no candidates (no candidate cleared `MIN_SCORE`).
pub fn expand_members(
  section_text: &str,
  contract_topic: &topic::Topic,
  audit_data: &AuditData,
) -> Vec<(topic::Topic, f32)> {
  if section_text.trim().is_empty() {
    return Vec::new();
  }

  let corpus = build_contract_member_corpus(contract_topic, audit_data);
  if corpus.is_empty() {
    return Vec::new();
  }

  let query_tokens = tokenize_prose_text(section_text);
  if query_tokens.is_empty() {
    return Vec::new();
  }

  let scored = score(&query_tokens, &corpus);
  cutoff(&scored)
    .into_iter()
    .map(|i| (scored[i].item.member_topic, scored[i].score))
    .collect()
}

/// Tunable constants for the BM25 cutoffs. Calibrated empirically against
/// the comparison harness; see `docs/specs/semantic-linking.md`.
pub mod constants {
  /// Pass 2 — drop candidates below this absolute score floor.
  pub const MIN_SCORE: f32 = 1.0;
  /// Pass 2 — keep at most this many members per (section, contract).
  pub const TOP_K: usize = 10;
  /// Pass 1 — top-K contracts per section to feed into Pass 2.
  pub const PASS1_TOP_K: usize = 10;
}

/// A single BM25-scored candidate.
#[derive(Debug, Clone)]
pub struct ScoredCandidate<T> {
  pub item: T,
  pub score: f32,
}

/// Top-K above absolute floor cutoff. `scored_desc` must already be sorted
/// in descending score order (which `score()` guarantees). Returns the
/// indices in `scored_desc` of candidates that pass.
pub fn cutoff<T>(scored_desc: &[ScoredCandidate<T>]) -> Vec<usize> {
  scored_desc
    .iter()
    .enumerate()
    .filter(|(_, c)| c.score >= constants::MIN_SCORE)
    .take(constants::TOP_K)
    .map(|(i, _)| i)
    .collect()
}

// ---------------------------------------------------------------------------
// BM25 Pass 1: contract discovery
// ---------------------------------------------------------------------------

/// Score every contract's summary document against the section text.
/// Returns **every** scored contract with its score in descending order;
/// callers apply their own cutoff (`discover_top_k_contracts` for the
/// production path).
///
/// Contracts whose summary corpus is empty (no indexable declarations)
/// score zero and are dropped.
pub fn rank_contracts(
  section_text: &str,
  audit_data: &AuditData,
  variant: SummaryCorpusVariant,
) -> Vec<(topic::Topic, f32)> {
  if section_text.trim().is_empty() {
    return Vec::new();
  }

  let corpus = build_contract_summary_corpus(audit_data, variant);
  if corpus.is_empty() {
    return Vec::new();
  }

  let query_tokens = tokenize_prose_text(section_text);
  if query_tokens.is_empty() {
    return Vec::new();
  }

  let scored = score(&query_tokens, &corpus);
  scored
    .into_iter()
    .map(|c| (c.item.contract_topic, c.score))
    .collect()
}

/// Top-K contract topics from BM25 Pass 1 — the production cutoff used by
/// the main pipeline.
pub fn discover_top_k_contracts(
  section_text: &str,
  audit_data: &AuditData,
  variant: SummaryCorpusVariant,
) -> Vec<(topic::Topic, f32)> {
  rank_contracts(section_text, audit_data, variant)
    .into_iter()
    .filter(|(_, score)| *score >= constants::MIN_SCORE)
    .take(constants::PASS1_TOP_K)
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn cs(score: f32) -> ScoredCandidate<()> {
    ScoredCandidate { item: (), score }
  }

  #[test]
  fn cutoff_caps_at_top_k_and_min_score() {
    let scored = vec![
      cs(5.0),
      cs(4.0),
      cs(3.0),
      cs(2.0),
      cs(1.5),
      cs(1.2),
      cs(0.5),
    ];
    let kept = cutoff(&scored);
    // floor=1.0 drops the 0.5 entry; TOP_K=10 means everything else is kept.
    assert_eq!(kept, vec![0, 1, 2, 3, 4, 5]);
  }

  #[test]
  fn cutoff_truncates_when_more_than_top_k_pass_floor() {
    let scored: Vec<ScoredCandidate<()>> =
      (0..15).rev().map(|i| cs((i + 2) as f32)).collect();
    let kept = cutoff(&scored);
    assert_eq!(kept.len(), constants::TOP_K);
    assert_eq!(kept, (0..constants::TOP_K).collect::<Vec<_>>());
  }

  #[test]
  fn cutoff_empty_input_yields_empty() {
    let scored: Vec<ScoredCandidate<()>> = Vec::new();
    assert!(cutoff(&scored).is_empty());
  }

  #[test]
  fn cutoff_drops_everything_below_min_score() {
    let scored = vec![cs(0.99), cs(0.5), cs(0.1)];
    assert!(cutoff(&scored).is_empty());
  }
}
