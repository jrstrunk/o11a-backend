//! BM25 expansion for Pass 2 of semantic linking.
//!
//! See `docs/specs/semantic-linking.md` for the full design. This module
//! implements:
//! - Tokenization pipeline: compound-term detection → operator expansion →
//!   identifier splitting → abbreviation/domain expansion → lowercase →
//!   stop-word removal → Porter stemming.
//! - BM25 scoring against per-contract member corpora.
//! - Cutoff algorithms A (gap) and B (top-k-floor) from the spec.
//!
//! BM25 is **never** in the default workflow; it exists as an evaluation
//! tool used via `--semantic-linking-mode=bm25` and
//! `--semantic-linking-compare-all`.

mod tokenize;
mod corpus;
mod score;

pub use corpus::{
  ContractDoc, MemberDoc, SummaryCorpusVariant, build_contract_member_corpus,
  build_contract_summary_corpus,
};
pub use score::score;
pub use tokenize::{tokenize_code_text, tokenize_prose_text};

use crate::collaborator::agent::semantic_linking::CutoffAlgorithm;
use crate::domain::{AuditData, topic};

/// Expand the mechanical Pass 2 result with BM25-ranked members from a single
/// contract. Returns each kept member with its BM25 score so callers that
/// need provenance (e.g. the comparison harness) can surface it.
///
/// Returns an empty vec on any of:
/// - empty section text,
/// - empty member corpus,
/// - cutoff returning no candidates (no confident match).
pub fn expand_members(
  section_text: &str,
  contract_topic: &topic::Topic,
  audit_data: &AuditData,
  algo: CutoffAlgorithm,
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
  let kept = cutoff(&scored, algo);
  kept
    .into_iter()
    .map(|i| (scored[i].item.member_topic, scored[i].score))
    .collect()
}

/// Tunable constants for the cutoff algorithms. See the spec for guidance on
/// adjusting these. Defaults are starting points; refine manually after
/// observing comparison output.
pub mod constants {
  /// Algorithm A — drop candidates whose normalized score is below this.
  pub const FLOOR: f32 = 0.20;
  /// Algorithm A — minimum relative gap to consider an "elbow" significant.
  /// (next/prev must be ≤ this value.)
  pub const GAP_RATIO: f32 = 0.60;
  /// Algorithm A — minimum absolute top score; below this, return empty.
  pub const MIN_TOP_SCORE: f32 = 1.0;

  /// Algorithm B — minimum absolute score floor.
  pub const MIN_SCORE: f32 = 1.0;
  /// Algorithm B — maximum number of matches per (section, contract) pair.
  pub const TOP_K: usize = 10;

  /// BM25 Pass 1 — top-K contracts per section to feed into Pass 2.
  pub const PASS1_TOP_K: usize = 10;
}

/// A single BM25-scored candidate.
#[derive(Debug, Clone)]
pub struct ScoredCandidate<T> {
  pub item: T,
  pub score: f32,
}

/// Apply the configured cutoff algorithm to a sorted-descending list of
/// scored candidates. Returns the indices of candidates that pass.
pub fn cutoff<T>(
  scored_desc: &[ScoredCandidate<T>],
  algo: CutoffAlgorithm,
) -> Vec<usize> {
  match algo {
    CutoffAlgorithm::Gap => cutoff_gap(scored_desc),
    CutoffAlgorithm::TopKFloor => cutoff_top_k_floor(scored_desc),
  }
}

/// Algorithm A: hard floor + relative gap detection.
fn cutoff_gap<T>(scored_desc: &[ScoredCandidate<T>]) -> Vec<usize> {
  if scored_desc.is_empty() {
    return Vec::new();
  }
  // Safety gate: top score below absolute minimum → no confident answer.
  if scored_desc[0].score < constants::MIN_TOP_SCORE {
    return Vec::new();
  }
  let top = scored_desc[0].score;
  if top <= 0.0 {
    return Vec::new();
  }
  // Normalize and apply the hard floor.
  let normalized: Vec<f32> =
    scored_desc.iter().map(|c| c.score / top).collect();
  let above_floor: Vec<usize> = (0..normalized.len())
    .filter(|&i| normalized[i] >= constants::FLOOR)
    .collect();
  if above_floor.len() <= 1 {
    return above_floor;
  }
  // Find the largest relative drop within the survivors.
  let mut min_ratio = f32::INFINITY;
  let mut cut_at: Option<usize> = None;
  for (k, win) in above_floor.windows(2).enumerate() {
    let prev = normalized[win[0]];
    let next = normalized[win[1]];
    if prev <= 0.0 {
      continue;
    }
    let ratio = next / prev;
    if ratio < min_ratio {
      min_ratio = ratio;
      cut_at = Some(k); // index into above_floor pairs
    }
  }
  // If the biggest gap is significant, cut there; otherwise keep all.
  match cut_at {
    Some(k) if min_ratio < constants::GAP_RATIO => {
      above_floor.into_iter().take(k + 1).collect()
    }
    _ => above_floor,
  }
}

/// Algorithm B: top-K with absolute score floor.
fn cutoff_top_k_floor<T>(scored_desc: &[ScoredCandidate<T>]) -> Vec<usize> {
  scored_desc
    .iter()
    .enumerate()
    .filter(|(_, c)| c.score >= constants::MIN_SCORE)
    .take(constants::TOP_K)
    .map(|(i, _)| i)
    .collect()
}

/// Permissive cutoff: keep everything above the absolute `MIN_SCORE` floor,
/// no relative-gap detection, no top-K cap. Used by the comparison harness
/// to study the full score-survival distribution.
pub fn cutoff_permissive<T>(scored_desc: &[ScoredCandidate<T>]) -> Vec<usize> {
  scored_desc
    .iter()
    .enumerate()
    .filter(|(_, c)| c.score >= constants::MIN_SCORE)
    .map(|(i, _)| i)
    .collect()
}

/// Variant of `expand_members` that returns every above-`MIN_SCORE`
/// candidate (no cutoff at all). Members that don't appear in the corpus
/// score zero and are dropped automatically. Use for the
/// `bm25-permissive` evaluation variant.
pub fn expand_members_permissive(
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
  cutoff_permissive(&scored)
    .into_iter()
    .map(|i| (scored[i].item.member_topic, scored[i].score))
    .collect()
}

// ---------------------------------------------------------------------------
// BM25 Pass 1: contract discovery
// ---------------------------------------------------------------------------

/// Score every contract's summary document against the section text. Returns
/// **every** scored contract with its score in descending order — callers
/// apply their own cutoff (`discover_top_k_contracts` for the production
/// path; the comparison harness logs the full ranking for analysis).
///
/// Contracts whose summary corpus is empty (no indexable declarations) score
/// zero and are dropped.
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
/// the main pipeline and the four production-equivalent harness variants.
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
  fn cutoff_gap_finds_clear_elbow() {
    // Top 3 are clear winners, then big drop.
    let scored = vec![
      cs(10.0),
      cs(8.0),
      cs(7.0),
      cs(2.0),
      cs(1.5),
      cs(1.2),
    ];
    let kept = cutoff_gap(&scored);
    assert_eq!(kept, vec![0, 1, 2]);
  }

  #[test]
  fn cutoff_gap_keeps_all_when_no_clear_drop() {
    // Smooth decline — no significant gap.
    let scored = vec![cs(10.0), cs(9.0), cs(8.0), cs(7.5), cs(7.0)];
    let kept = cutoff_gap(&scored);
    assert_eq!(kept, vec![0, 1, 2, 3, 4]);
  }

  #[test]
  fn cutoff_gap_safety_gate_returns_empty_for_low_top_score() {
    let scored = vec![cs(0.5), cs(0.4), cs(0.3)];
    assert!(cutoff_gap(&scored).is_empty());
  }

  #[test]
  fn cutoff_gap_floor_drops_noise() {
    // Top is 10.0, floor = 0.2 → keep scores ≥ 2.0.
    let scored = vec![cs(10.0), cs(9.0), cs(1.0), cs(0.5)];
    let kept = cutoff_gap(&scored);
    assert_eq!(kept, vec![0, 1]);
  }

  #[test]
  fn cutoff_top_k_floor_caps_at_k_and_floor() {
    let scored = vec![
      cs(5.0),
      cs(4.0),
      cs(3.0),
      cs(2.0),
      cs(1.5),
      cs(1.2),
      cs(0.5),
    ];
    let kept = cutoff_top_k_floor(&scored);
    // floor=1.0 drops the 0.5 entry; TOP_K=10 means everything else is kept.
    assert_eq!(kept, vec![0, 1, 2, 3, 4, 5]);
  }

  #[test]
  fn cutoff_permissive_keeps_everything_above_floor() {
    let scored =
      vec![cs(50.0), cs(20.0), cs(5.0), cs(1.5), cs(1.0), cs(0.99)];
    let kept = cutoff_permissive(&scored);
    // floor=1.0 (inclusive); 0.99 dropped.
    assert_eq!(kept, vec![0, 1, 2, 3, 4]);
  }

  #[test]
  fn cutoff_empty_input() {
    let scored: Vec<ScoredCandidate<()>> = Vec::new();
    assert!(cutoff_gap(&scored).is_empty());
    assert!(cutoff_top_k_floor(&scored).is_empty());
  }
}
