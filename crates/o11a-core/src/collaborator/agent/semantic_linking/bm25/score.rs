//! BM25 scoring against a fixed in-memory corpus.
//!
//! This is a hand-rolled BM25 because the corpus we score against is small
//! (members of a single contract — typically 5–50 documents) and we don't
//! need an inverted index, persistence, or query parsing. See
//! `docs/specs/semantic-linking.md` for context.
//!
//! Formula (Robertson/Sparck Jones, with Lucene-style IDF smoothing and a
//! short-document length floor described below):
//!
//! ```text
//! IDF(qi) = ln((N - n(qi) + 0.5) / (n(qi) + 0.5) + 1)
//! eff_dl = max(|D|, avgdl * MIN_LENGTH_RATIO)
//!
//! score(D, Q) = sum over qi in Q of:
//!     IDF(qi) * (f(qi, D) * (k1 + 1))
//!     ----------------------------------------------
//!     (f(qi, D) + k1 * (1 - b + b * eff_dl / avgdl))
//! ```
//!
//! Defaults: `k1 = 1.2`, `b = 0.75`, `MIN_LENGTH_RATIO = 0.75`.
//!
//! The length floor `MIN_LENGTH_RATIO` addresses an empirical pathology in
//! the comparison-harness data: very short member documents (1–3 tokens —
//! bare identifier names with no NatSpec) get the smallest possible
//! denominator and produce inflated scores that don't correlate with
//! Pass 3 LLM acceptance. Treating any document shorter than
//! `0.75 * avgdl` as if it were `0.75 * avgdl` long caps the bonus
//! short docs receive without zeroing length normalization for
//! mid-length docs. See the analysis in
//! `docs/specs/semantic-linking.md`.

use std::collections::HashMap;

use super::ScoredCandidate;
use super::corpus::{ContractDoc, MemberDoc};

impl BM25Doc for MemberDoc {
  fn tokens(&self) -> &[String] {
    &self.tokens
  }
}

impl BM25Doc for ContractDoc {
  fn tokens(&self) -> &[String] {
    &self.tokens
  }
}

const K1: f32 = 1.2;
const B: f32 = 0.75;
/// Documents shorter than `MIN_LENGTH_RATIO * avgdl` are treated as if
/// they were exactly that long for length normalization. Caps the
/// scoring bonus very short docs would otherwise receive.
const MIN_LENGTH_RATIO: f32 = 0.75;

/// Trait for documents BM25 can score: any pre-tokenized text.
/// Implemented by `MemberDoc` (member-level corpus) and `ContractDoc`
/// (contract-summary corpus for Pass 1).
pub trait BM25Doc {
  fn tokens(&self) -> &[String];
}

/// Score every document in `corpus` against the given query tokens. Returns
/// candidates sorted in descending-score order. Documents with score 0 are
/// dropped.
pub fn score<'a, D: BM25Doc>(
  query_tokens: &[String],
  corpus: &'a [D],
) -> Vec<ScoredCandidate<&'a D>> {
  if query_tokens.is_empty() || corpus.is_empty() {
    return Vec::new();
  }

  let n = corpus.len() as f32;
  let avgdl: f32 =
    corpus.iter().map(|d| d.tokens().len() as f32).sum::<f32>() / n;
  if avgdl <= 0.0 {
    return Vec::new();
  }

  // Document frequency for each unique query term.
  let mut df: HashMap<&str, u32> = HashMap::new();
  for qt in query_tokens {
    if df.contains_key(qt.as_str()) {
      continue;
    }
    let count = corpus
      .iter()
      .filter(|d| d.tokens().iter().any(|t| t == qt))
      .count() as u32;
    df.insert(qt.as_str(), count);
  }

  let mut idf: HashMap<&str, f32> = HashMap::new();
  for (term, n_qi) in &df {
    let n_qi = *n_qi as f32;
    idf.insert(*term, ((n - n_qi + 0.5) / (n_qi + 0.5) + 1.0).ln());
  }

  let length_floor = avgdl * MIN_LENGTH_RATIO;
  let mut out: Vec<ScoredCandidate<&D>> = Vec::with_capacity(corpus.len());
  for doc in corpus {
    let dl = doc.tokens().len() as f32;
    if dl <= 0.0 {
      continue;
    }
    let effective_dl = dl.max(length_floor);

    // Term frequencies in this doc, computed once per scoring.
    let mut tf: HashMap<&str, u32> = HashMap::new();
    for t in doc.tokens() {
      *tf.entry(t.as_str()).or_insert(0) += 1;
    }

    let mut s = 0.0f32;
    for qt in query_tokens {
      let f = *tf.get(qt.as_str()).unwrap_or(&0) as f32;
      if f == 0.0 {
        continue;
      }
      let idf_qi = *idf.get(qt.as_str()).unwrap_or(&0.0);
      let numerator = f * (K1 + 1.0);
      let denominator = f + K1 * (1.0 - B + B * effective_dl / avgdl);
      s += idf_qi * (numerator / denominator);
    }

    if s > 0.0 {
      out.push(ScoredCandidate {
        item: doc,
        score: s,
      });
    }
  }

  out.sort_by(|a, b| {
    b.score
      .partial_cmp(&a.score)
      .unwrap_or(std::cmp::Ordering::Equal)
  });
  out
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::domain::topic;

  fn make_doc(id: i32, tokens: &[&str]) -> MemberDoc {
    MemberDoc {
      member_topic: topic::new_node_topic(&id),
      tokens: tokens.iter().map(|s| s.to_string()).collect(),
    }
  }

  #[test]
  fn score_empty_inputs_return_empty() {
    let query: Vec<String> = vec![];
    let corpus: Vec<MemberDoc> = vec![];
    assert!(score(&query, &corpus).is_empty());

    let query = vec!["foo".to_string()];
    assert!(score(&query, &corpus).is_empty());
  }

  #[test]
  fn score_ranks_by_term_overlap() {
    let corpus = vec![
      make_doc(1, &["stake", "deposit", "vault"]),
      make_doc(2, &["transfer", "balance"]),
      make_doc(3, &["stake", "reward", "claim"]),
    ];
    let query = vec!["stake".to_string(), "reward".to_string()];
    let scored = score(&query, &corpus);

    // Doc 3 has both "stake" and "reward" — should rank first.
    assert!(!scored.is_empty());
    assert_eq!(scored[0].item.member_topic, corpus[2].member_topic);
  }

  #[test]
  fn score_ignores_zero_overlap_docs() {
    let corpus = vec![
      make_doc(1, &["unrelated", "tokens", "here"]),
      make_doc(2, &["match", "this"]),
    ];
    let query = vec!["match".to_string()];
    let scored = score(&query, &corpus);

    // Only one doc shares any term — only it should appear.
    assert_eq!(scored.len(), 1);
    assert_eq!(scored[0].item.member_topic, corpus[1].member_topic);
  }

  /// Two docs match the same single query term exactly once each: a very
  /// short doc and an average-length one. Without the length floor the
  /// short doc would dominate; with the floor (any doc shorter than
  /// `0.75 * avgdl` is treated as `0.75 * avgdl` long) the short doc's
  /// per-term contribution is bounded so the gap is bounded — pinning
  /// the short-doc bias fix.
  #[test]
  fn length_floor_caps_short_doc_score_inflation() {
    // Very short matching doc (1 token), padded by long unrelated docs
    // so avgdl is high. Without the floor, the short doc gets the full
    // length-normalization bonus.
    let mut corpus =
      vec![make_doc(1, &["match"]), make_doc(2, &["match", "filler"])];
    // Pad with long non-matching docs so avgdl is well above 1.
    for i in 0..8 {
      let pad: Vec<&str> = (0..40).map(|_| "noise").collect();
      corpus.push(make_doc(100 + i, &pad));
    }
    let query = vec!["match".to_string()];
    let scored = score(&query, &corpus);

    let s_short = scored
      .iter()
      .find(|c| c.item.member_topic == corpus[0].member_topic)
      .map(|c| c.score)
      .expect("short doc must score");
    let s_pair = scored
      .iter()
      .find(|c| c.item.member_topic == corpus[1].member_topic)
      .map(|c| c.score)
      .expect("two-token doc must score");

    // With the length floor, both very-short docs are normalized at the
    // same effective length (avgdl * MIN_LENGTH_RATIO), so their
    // per-term scores are equal.
    assert!(
      (s_short - s_pair).abs() < 1e-4,
      "length-floored docs should score equally on identical TF; got short={} vs pair={}",
      s_short,
      s_pair
    );

    // And the score should be modest, not the inflated short-doc value
    // from raw length normalization. Sanity-check it's well under what
    // un-floored BM25 would produce for a 1-token doc with avgdl ≈ 33.
    // Un-floored: f=1, denom = 1 + 1.2*(0.25 + 0.75 * 1/33) ≈ 1.327
    //   per-term ≈ 2.2 / 1.327 ≈ 1.66
    // Floored: effective_dl = 33 * 0.75 ≈ 24.75
    //   denom = 1 + 1.2*(0.25 + 0.75 * 24.75/33) ≈ 1 + 1.2*0.8125 ≈ 1.975
    //   per-term ≈ 2.2 / 1.975 ≈ 1.114
    // (IDF for an N=10 corpus with df=2 is ln((10-2+0.5)/(2+0.5)+1)≈1.522)
    // Expected score ≈ 1.522 * 1.114 ≈ 1.69; certainly below 2.6 from
    // the un-floored case.
    assert!(
      s_short < 2.0,
      "length-floored short doc score {} should not be inflated",
      s_short
    );
  }
}
