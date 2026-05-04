//! BM25 scoring against a fixed in-memory corpus.
//!
//! This is a hand-rolled BM25 because the corpus we score against is small
//! (members of a single contract — typically 5–50 documents) and we don't
//! need an inverted index, persistence, or query parsing. See
//! `docs/specs/semantic-linking.md` for context.
//!
//! Formula (Robertson/Sparck Jones, with Lucene-style IDF smoothing):
//!
//! ```text
//! IDF(qi) = ln((N - n(qi) + 0.5) / (n(qi) + 0.5) + 1)
//!
//! score(D, Q) = sum over qi in Q of:
//!     IDF(qi) * (f(qi, D) * (k1 + 1))
//!     ----------------------------------------------
//!     (f(qi, D) + k1 * (1 - b + b * |D| / avgdl))
//! ```
//!
//! Defaults: `k1 = 1.2`, `b = 0.75`.

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

  let mut out: Vec<ScoredCandidate<&D>> = Vec::with_capacity(corpus.len());
  for doc in corpus {
    let dl = doc.tokens().len() as f32;
    if dl <= 0.0 {
      continue;
    }

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
      let denominator = f + K1 * (1.0 - B + B * dl / avgdl);
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
}
