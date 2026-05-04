//! Tokenization pipeline for BM25 scoring of code-derived documents and
//! documentation prose. See `docs/specs/semantic-linking.md` for the design.
//!
//! Pipeline stages:
//!
//! 1. Compound-term detection (`msg.sender` → "amount sent", etc.) — only on
//!    code-derived text, applied before identifier splitting so that the dot
//!    isn't mistaken for an operator/separator.
//! 2. Operator expansion (`*` → "multiply", `=` → "update") — only on
//!    code-derived text.
//! 3. Identifier splitting (`computeShares` → "compute shares") — applied to
//!    both code and prose tokens.
//! 4. Abbreviation expansion (`acc` → "account") — applied per-token.
//! 5. Lowercase + stop-word removal + Porter stemming.
//!
//! Stages 1, 2 are skipped for prose. Otherwise the pipelines are identical.

/// Tokenize text drawn from source code (member signatures, NatSpec,
/// declaration source). Applies the full code pipeline.
pub fn tokenize_code_text(text: &str) -> Vec<String> {
  let stage1 = expand_compound_terms(text);
  let stage2 = expand_operators(&stage1);
  tokenize_words(&stage2)
}

/// Tokenize text drawn from documentation prose (section text). Skips the
/// compound-term and operator-expansion stages — prose contains no raw
/// operators or magic identifiers like `msg.sender`.
pub fn tokenize_prose_text(text: &str) -> Vec<String> {
  tokenize_words(text)
}

/// Split into word-like tokens, then run identifier splitting, abbreviation
/// expansion, lowercase, stop-words, and stemming.
fn tokenize_words(text: &str) -> Vec<String> {
  let mut out: Vec<String> = Vec::new();
  for raw in text.split(|c: char| !is_identifier_char(c)) {
    if raw.is_empty() {
      continue;
    }
    for piece in split_identifier(raw) {
      let lower = piece.to_ascii_lowercase();
      let expanded = expand_abbreviation(&lower);
      // Expansion may emit multiple tokens (split on whitespace).
      for word in expanded.split_whitespace() {
        if word.is_empty() {
          continue;
        }
        if STOP_WORDS.binary_search(&word).is_ok() {
          continue;
        }
        let stemmed = porter_stem(word);
        if stemmed.len() < 2 && !stemmed.chars().all(|c| c.is_ascii_digit()) {
          // Drop length-1 alphabetic tokens (no IDF discrimination); keep
          // pure-digit tokens.
          continue;
        }
        out.push(stemmed);
      }
    }
  }
  out
}

fn is_identifier_char(c: char) -> bool {
  c.is_ascii_alphanumeric() || c == '_'
}

// ---------------------------------------------------------------------------
// Compound-term detection
// ---------------------------------------------------------------------------

/// Solidity domain compound terms — multi-token phrases that should be
/// expanded to plain English before identifier splitting breaks them apart.
/// Match longest-first.
const COMPOUND_TERMS: &[(&str, &str)] = &[
  ("block.timestamp", " time "),
  ("block.number", " block height "),
  ("msg.sender", " caller "),
  ("msg.value", " amount sent "),
  ("msg.data", " calldata "),
  ("tx.origin", " originator "),
];

fn expand_compound_terms(text: &str) -> String {
  // Naive but predictable: iterate the COMPOUND_TERMS list and replace each
  // occurrence. Order matters when two terms share a prefix; the table is
  // ordered so longer entries come first if needed.
  let mut s = text.to_string();
  for (src, dst) in COMPOUND_TERMS {
    if s.contains(src) {
      s = s.replace(src, dst);
    }
  }
  s
}

// ---------------------------------------------------------------------------
// Operator expansion
// ---------------------------------------------------------------------------

/// Operator → English word table. See `docs/specs/semantic-linking.md`.
/// Sorted longest-first so that `==` matches before `=`, `<<=` before `<<`,
/// etc.
const OPERATORS: &[(&str, &str)] = &[
  // 3-char
  ("<<=", " shift left "),
  (">>=", " shift right "),
  // 2-char
  ("==", " equal "),
  ("!=", " unequal "),
  ("<=", " less equal "),
  (">=", " greater equal "),
  ("&&", " and "),
  ("||", " or "),
  ("<<", " shift left "),
  (">>", " shift right "),
  ("++", " increment "),
  ("--", " decrement "),
  ("+=", " increment "),
  ("-=", " decrement "),
  ("*=", " multiply "),
  ("/=", " divide "),
  ("%=", " modulo "),
  ("=>", " maps "),
  // 1-char
  ("+", " add "),
  ("-", " subtract "),
  ("*", " multiply "),
  ("/", " divide "),
  ("%", " modulo "),
  ("=", " update "),
  ("<", " less "),
  (">", " greater "),
  ("!", " not "),
  ("&", " bitand "),
  ("|", " bitor "),
  ("^", " xor "),
  ("~", " bitnot "),
];

/// Replace operators with their English-word expansions. Iteration order
/// (longest-first) ensures `==` becomes "equal" rather than two "update"s.
fn expand_operators(text: &str) -> String {
  let mut s = text.to_string();
  for (src, dst) in OPERATORS {
    if s.contains(src) {
      s = s.replace(src, dst);
    }
  }
  s
}

// ---------------------------------------------------------------------------
// Identifier splitting
// ---------------------------------------------------------------------------

/// Split an identifier into constituent words. See the spec table for rules.
/// Returns `Vec<&str>` borrowing from the input where possible to avoid
/// allocation churn — but note callers may turn results into owned strings
/// when expansion is applied later.
pub fn split_identifier(ident: &str) -> Vec<String> {
  // Strip leading underscores (Solidity convention; not semantic).
  let ident = ident.trim_start_matches('_');
  if ident.is_empty() {
    return Vec::new();
  }

  // Pure-uppercase token with embedded digits (acronym + version, e.g.
  // `IERC20`, `ERC721`): emit verbatim, lowercased.
  if is_acronym_with_digits(ident) {
    return vec![ident.to_string()];
  }

  let chars: Vec<char> = ident.chars().collect();
  let n = chars.len();
  let mut out: Vec<String> = Vec::new();
  let mut start = 0usize;

  for i in 1..n {
    let prev = chars[i - 1];
    let curr = chars[i];
    // Snake-case + camelCase + letter/digit boundaries are unconditional
    // splits; the uppercase-uppercase case needs lookahead (URLParser →
    // URL | Parser) so we don't break mid-acronym.
    let snake = curr == '_' || prev == '_';
    let camel = prev.is_ascii_lowercase() && curr.is_ascii_uppercase();
    let letter_digit = (prev.is_ascii_alphabetic() && curr.is_ascii_digit())
      || (prev.is_ascii_digit() && curr.is_ascii_alphabetic());
    let acronym_then_word = prev.is_ascii_uppercase()
      && curr.is_ascii_uppercase()
      && matches!(chars.get(i + 1).copied(), Some(c) if c.is_ascii_lowercase());

    let split_here = snake || camel || letter_digit || acronym_then_word;

    if split_here {
      let piece: String = chars[start..i].iter().collect();
      let piece = piece.trim_matches('_').to_string();
      if !piece.is_empty() {
        out.push(piece);
      }
      start = i;
      // Skip the underscore itself.
      if curr == '_' {
        start = i + 1;
      }
    }
  }
  let last: String = chars[start..n].iter().collect();
  let last = last.trim_matches('_').to_string();
  if !last.is_empty() {
    out.push(last);
  }
  out
}

fn is_acronym_with_digits(s: &str) -> bool {
  let mut has_upper = false;
  let mut has_digit = false;
  for c in s.chars() {
    if c.is_ascii_uppercase() {
      has_upper = true;
    } else if c.is_ascii_digit() {
      has_digit = true;
    } else {
      return false;
    }
  }
  has_upper && has_digit
}

// ---------------------------------------------------------------------------
// Abbreviation expansion
// ---------------------------------------------------------------------------

/// Universal abbreviations — case-insensitive lookup (the caller has already
/// lowercased the token by this point).
const ABBREVIATIONS: &[(&str, &str)] = &[
  ("acc", "account"),
  ("acct", "account"),
  ("addr", "address"),
  ("amt", "amount"),
  ("arr", "array"),
  ("bal", "balance"),
  ("cfg", "config"),
  ("cnt", "count"),
  ("dest", "destination"),
  ("id", "identifier"),
  ("idx", "index"),
  ("init", "initialize"),
  ("len", "length"),
  ("msg", "message"),
  ("num", "number"),
  ("prev", "previous"),
  ("qty", "quantity"),
  ("recv", "receive"),
  ("ref", "reference"),
  ("src", "source"),
  ("tmp", "temporary"),
  ("tx", "transaction"),
  ("txn", "transaction"),
];

/// Look up `lowered` in the abbreviation table. Returns the expansion
/// (possibly multi-word) or the input unchanged.
fn expand_abbreviation(lowered: &str) -> String {
  match ABBREVIATIONS.binary_search_by_key(&lowered, |(k, _)| *k) {
    Ok(i) => ABBREVIATIONS[i].1.to_string(),
    Err(_) => lowered.to_string(),
  }
}

// ---------------------------------------------------------------------------
// Stop words
// ---------------------------------------------------------------------------

/// Sorted list of English stop words plus a handful of Solidity boilerplate
/// keywords that don't carry semantic weight in BM25 scoring. Sorted so that
/// `binary_search` works.
const STOP_WORDS: &[&str] = &[
  "a",
  "about",
  "above",
  "after",
  "again",
  "against",
  "all",
  "am",
  "an",
  "and",
  "any",
  "are",
  "as",
  "at",
  "be",
  "because",
  "been",
  "before",
  "being",
  "below",
  "between",
  "both",
  "but",
  "by",
  "can",
  "did",
  "do",
  "does",
  "doing",
  "don",
  "down",
  "during",
  "each",
  "few",
  "for",
  "from",
  "further",
  "had",
  "has",
  "have",
  "having",
  "he",
  "her",
  "here",
  "hers",
  "herself",
  "him",
  "himself",
  "his",
  "how",
  "i",
  "if",
  "in",
  "into",
  "is",
  "it",
  "its",
  "itself",
  "just",
  "me",
  "more",
  "most",
  "my",
  "myself",
  "no",
  "nor",
  "not",
  "now",
  "of",
  "off",
  "on",
  "once",
  "only",
  "or",
  "other",
  "our",
  "ours",
  "ourselves",
  "out",
  "over",
  "own",
  "s",
  "same",
  "she",
  "should",
  "so",
  "some",
  "such",
  "t",
  "than",
  "that",
  "the",
  "their",
  "theirs",
  "them",
  "themselves",
  "then",
  "there",
  "these",
  "they",
  "this",
  "those",
  "through",
  "to",
  "too",
  "under",
  "until",
  "up",
  "very",
  "was",
  "we",
  "were",
  "what",
  "when",
  "where",
  "which",
  "while",
  "who",
  "whom",
  "why",
  "will",
  "with",
  "you",
  "your",
  "yours",
  "yourself",
  "yourselves",
];

// ---------------------------------------------------------------------------
// Porter stemmer (minimal in-tree implementation)
// ---------------------------------------------------------------------------

/// Very small Porter stemmer. We use a hand-rolled subset rather than pulling
/// in `rust-stemmers` because we already depend on no extra crates here and
/// the rule subset below covers the common English morphology without the
/// pitfalls of full Porter's edge cases. Future work can swap this out for
/// `rust-stemmers` if needed (e.g., via the `bm25` crate's default tokenizer).
fn porter_stem(word: &str) -> String {
  let mut s = word.to_string();
  // Step 1a — plurals.
  // sses → ss   (drop 2): processes → process
  // ies  → i    (drop 2): cries → cri
  // ss   → ss   (no change): pass → pass
  // s    → ""   (drop 1): cats → cat
  if s.ends_with("sses") || s.ends_with("ies") {
    s.truncate(s.len() - 2);
  } else if !s.ends_with("ss") && s.ends_with('s') && s.len() > 2 {
    s.truncate(s.len() - 1);
  }
  // Step 1b — verb endings.
  if s.ends_with("eed") && s.len() > 4 {
    s.truncate(s.len() - 1);
  } else if s.ends_with("ed") && s.len() > 3 {
    s.truncate(s.len() - 2);
  } else if s.ends_with("ing") && s.len() > 4 {
    s.truncate(s.len() - 3);
  }
  // Step 1c — y → i (only after a consonant, e.g. "cry" → "cri").
  if s.len() > 2 && s.ends_with('y') {
    let bytes = s.as_bytes();
    let prev = bytes[bytes.len() - 2] as char;
    if prev.is_ascii_alphabetic() && !is_vowel(prev) {
      s.truncate(s.len() - 1);
      s.push('i');
    }
  }
  s
}

fn is_vowel(c: char) -> bool {
  matches!(c, 'a' | 'e' | 'i' | 'o' | 'u')
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn split_camel_case() {
    assert_eq!(split_identifier("computeShares"), vec!["compute", "Shares"]);
  }

  #[test]
  fn split_pascal_case() {
    assert_eq!(split_identifier("ComputeShares"), vec!["Compute", "Shares"]);
  }

  #[test]
  fn split_snake_case() {
    assert_eq!(
      split_identifier("participation_id"),
      vec!["participation", "id"]
    );
  }

  #[test]
  fn split_screaming_snake() {
    assert_eq!(
      split_identifier("PARTICIPATION_ID"),
      vec!["PARTICIPATION", "ID"]
    );
  }

  #[test]
  fn split_acronym_with_digits_kept_whole() {
    assert_eq!(split_identifier("IERC20"), vec!["IERC20"]);
    assert_eq!(split_identifier("ERC721"), vec!["ERC721"]);
  }

  #[test]
  fn split_url_parser_acronym_then_word() {
    assert_eq!(split_identifier("URLParser"), vec!["URL", "Parser"]);
    assert_eq!(split_identifier("parseURL"), vec!["parse", "URL"]);
  }

  #[test]
  fn split_strips_leading_underscore() {
    assert_eq!(
      split_identifier("_internalState"),
      vec!["internal", "State"]
    );
  }

  #[test]
  fn split_letter_digit_boundaries() {
    assert_eq!(
      split_identifier("parse2Numbers"),
      vec!["parse", "2", "Numbers"]
    );
  }

  #[test]
  fn operator_expansion_basics() {
    let s = expand_operators("a += b * c");
    assert!(s.contains("increment"));
    assert!(s.contains("multiply"));
  }

  #[test]
  fn operator_expansion_longer_first() {
    // `==` should expand to "equal", not "update update".
    let s = expand_operators("a == b");
    assert!(s.contains("equal"));
    assert!(!s.contains("update update"));
  }

  #[test]
  fn compound_msg_sender_to_caller() {
    let s = expand_compound_terms("require(msg.sender == admin);");
    assert!(s.contains("caller"));
    assert!(!s.contains("msg.sender"));
  }

  #[test]
  fn abbreviation_expansion_lookup() {
    assert_eq!(expand_abbreviation("acc"), "account");
    assert_eq!(expand_abbreviation("addr"), "address");
    assert_eq!(expand_abbreviation("nope"), "nope");
  }

  #[test]
  fn full_code_pipeline_example() {
    let toks = tokenize_code_text(
      "function updateBalance(address acc) external { balance[acc] += msg.value; }",
    );
    // Should contain (post-stem) tokens for: update, balance, address,
    // account, external, increment, amount, sent.
    let s = toks.join(" ");
    assert!(s.contains("updat"));
    assert!(s.contains("balanc"));
    assert!(s.contains("account"));
    assert!(s.contains("address"));
    assert!(s.contains("increment"));
    assert!(s.contains("amount"));
    assert!(s.contains("sent"));
  }

  #[test]
  fn prose_pipeline_skips_operator_expansion() {
    // Prose with code-like fragments shouldn't expand `=` because we don't
    // run operator expansion on prose.
    let toks = tokenize_prose_text(
      "The function updates the user's balance when the participation_id is valid.",
    );
    let s = toks.join(" ");
    assert!(s.contains("updat"));
    assert!(s.contains("balanc"));
    assert!(s.contains("participation"));
    assert!(s.contains("identifier"));
  }

  #[test]
  fn stop_words_are_sorted_for_binary_search() {
    let mut sorted: Vec<&&str> = STOP_WORDS.iter().collect();
    sorted.sort();
    let pairs: Vec<(&&str, &&str)> =
      sorted.iter().copied().zip(STOP_WORDS.iter()).collect();
    for (a, b) in pairs {
      assert_eq!(a, b, "STOP_WORDS must be sorted");
    }
  }

  #[test]
  fn abbreviations_are_sorted_for_binary_search() {
    let mut sorted: Vec<(&str, &str)> = ABBREVIATIONS.to_vec();
    sorted.sort_by_key(|(k, _)| *k);
    for ((a, _), (b, _)) in sorted.iter().zip(ABBREVIATIONS.iter()) {
      assert_eq!(a, b, "ABBREVIATIONS must be sorted by key");
    }
  }
}
