//! Configuration, BM25 scoring, and the mechanical-trace harness for the
//! semantic-linking pipeline.
//!
//! See `docs/specs/semantic-linking.md` for the full design. The production
//! pipeline has a single workflow, applied uniformly to every documentation
//! section regardless of `is_technical`:
//!
//!   1. **Step 1 — associate document sections to contracts.** Mechanical
//!      anchor resolution (`context::mechanical_semantic_links`) plus BM25
//!      contract discovery (top-K above `MIN_SCORE`).
//!   2. **Step 2 — add semantic links to contracts.** LLM synthesis of one
//!      semantic per contract entity; condensed in place to one link per
//!      contract.
//!   3. **Step 3 — associate document sections to contract members.**
//!      Mechanical seed (members reached by anchored declarations and by
//!      state-variable mutation fanout) plus BM25 member expansion within
//!      each anchored contract.
//!   4. **Step 4 — add semantic links to contract members.** LLM
//!      synthesis: one batch per section for function/modifier signatures
//!      (with their params/returns), one for non-function component-scoped
//!      declarations (state vars, events, errors, struct/enum defs +
//!      fields/members). Step 2 contract semantics are injected as
//!      context. Condensed in place to one link per declaration.
//!   5. **Step 5 — add semantic links to contract member bodies.** LLM
//!      synthesis of body locals (`Scope::ContainingBlock`); step 2 +
//!      step 4 semantics are injected as context. Condensed in place.
//!
//! K is a build-time constant (`bm25::TOP_K = 10`). The cutoff calibration
//! that arrived at K=10 is documented in the spec; rolling new defaults
//! requires a fresh evaluation run, not a flag.

use crate::domain::AuditData;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Top-level configuration for the semantic-linking pipeline.
///
/// Currently only carries the `mechanical_trace` debugging toggle. The
/// production workflow is fixed (no mode / no algorithm flag).
#[derive(Debug, Default, Clone, Copy)]
pub struct SemanticLinkingConfig {
  /// When set, run only the mechanical halves of step 1 + step 3 (no LLM,
  /// no synthesis steps), write a detailed JSONL trace of every section's
  /// resolved/unresolved inline-code references and derived
  /// contract/member candidates to `<output_dir>/mechanical-trace.jsonl`,
  /// then exit. Used to verify the deterministic name resolver in
  /// isolation.
  pub mechanical_trace: bool,
}

// ---------------------------------------------------------------------------
// CLI / env parsing
// ---------------------------------------------------------------------------

/// Parse the `--semantic-linking-*` flags from a CLI argument list, falling
/// back to env vars when a flag is not present. Returns `(config,
/// remaining_args)` — non-recognized arguments are passed through unchanged.
///
/// Only `--semantic-linking-mechanical-trace` is recognized; both bare
/// presence and `--semantic-linking-mechanical-trace=true|false` are
/// accepted.
pub fn parse_cli(
  args: &[String],
) -> Result<(SemanticLinkingConfig, Vec<String>), String> {
  let mut mechanical_trace: Option<bool> = None;
  let mut remaining: Vec<String> = Vec::with_capacity(args.len());

  let mut i = 0;
  while i < args.len() {
    let arg = &args[i];

    if let Some(eq) = arg.find('=') {
      let (flag, val) = arg.split_at(eq);
      let val = &val[1..];
      if flag == "--semantic-linking-mechanical-trace" {
        mechanical_trace = Some(parse_bool(val)?);
        i += 1;
        continue;
      }
    }

    if arg.as_str() == "--semantic-linking-mechanical-trace" {
      mechanical_trace = Some(true);
      i += 1;
      continue;
    }

    remaining.push(arg.clone());
    i += 1;
  }

  if let (None, Ok(s)) = (
    mechanical_trace,
    std::env::var("O11A_SEMANTIC_LINKING_MECHANICAL_TRACE"),
  ) {
    mechanical_trace = Some(parse_bool(&s)?);
  }

  Ok((
    SemanticLinkingConfig {
      mechanical_trace: mechanical_trace.unwrap_or(false),
    },
    remaining,
  ))
}

fn parse_bool(s: &str) -> Result<bool, String> {
  match s.trim().to_ascii_lowercase().as_str() {
    "1" | "true" | "yes" | "on" => Ok(true),
    "0" | "false" | "no" | "off" => Ok(false),
    other => Err(format!(
      "invalid boolean value '{other}' (expected one of: true, false, 1, 0, yes, no, on, off)"
    )),
  }
}

// ---------------------------------------------------------------------------
// is_technical lookup (still consumed by analysis-side tooling)
// ---------------------------------------------------------------------------

/// Look up the `is_technical` flag for the document containing this section.
/// Returns `false` (the conservative default) if the section's document
/// can't be found.
///
/// The pipeline itself no longer routes on `is_technical` — every section
/// gets the same workflow — but the flag remains a useful piece of metadata
/// for downstream tooling and tests.
pub fn section_is_technical(
  section_topic: &crate::domain::topic::Topic,
  audit_data: &AuditData,
) -> bool {
  use crate::domain::{Scope, TopicMetadata};
  let Some(metadata) = audit_data.topic_metadata.get(section_topic) else {
    return false;
  };
  let container = match metadata.scope() {
    Scope::Container { container }
    | Scope::Component { container, .. }
    | Scope::Member { container, .. }
    | Scope::ContainingBlock { container, .. } => container,
    Scope::Global => return false,
  };
  for m in audit_data.topic_metadata.values() {
    if let TopicMetadata::DocumentationTopic {
      scope: Scope::Container { container: c },
      is_technical,
      ..
    } = m
      && c == container
    {
      return *is_technical;
    }
  }
  false
}

// ---------------------------------------------------------------------------
// BM25 plumbing
// ---------------------------------------------------------------------------

pub mod bm25;

// ---------------------------------------------------------------------------
// Mechanical-only step 1 + step 3 trace mode (--semantic-linking-mechanical-trace)
// ---------------------------------------------------------------------------

pub mod trace;

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn cli_parses_mechanical_trace_bare_flag() {
    let args = vec![
      "--semantic-linking-mechanical-trace".to_string(),
      "positional".to_string(),
    ];
    let (cfg, rest) = parse_cli(&args).unwrap();
    assert!(cfg.mechanical_trace);
    assert_eq!(rest, vec!["positional"]);
  }

  #[test]
  fn cli_parses_mechanical_trace_value_form() {
    let args = vec![
      "--semantic-linking-mechanical-trace=false".to_string(),
      "positional".to_string(),
    ];
    let (cfg, rest) = parse_cli(&args).unwrap();
    assert!(!cfg.mechanical_trace);
    assert_eq!(rest, vec!["positional"]);
  }

  #[test]
  fn cli_passes_through_unrecognized_args() {
    let args: Vec<String> = vec![
      "positional1".to_string(),
      "positional2".to_string(),
      "positional3".to_string(),
    ];
    let (cfg, rest) = parse_cli(&args).unwrap();
    assert!(!cfg.mechanical_trace);
    assert_eq!(rest, vec!["positional1", "positional2", "positional3"]);
  }
}
