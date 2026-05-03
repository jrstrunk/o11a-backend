//! Configuration, routing, BM25 expansion, and the side-by-side comparison
//! harness for the semantic-linking pipeline.
//!
//! See `docs/specs/semantic-linking.md` for the full design. This module owns:
//! - The [`SemanticLinkingConfig`] passed through the pipeline.
//! - The [`SemanticLinkingMode`] / [`CutoffAlgorithm`] enums and CLI parsing
//!   helpers.
//! - The per-section workflow router (`workflow_for_section`).
//! - BM25 tokenization (operator/abbreviation/domain expansion + identifier
//!   splitting), corpus assembly, and cutoff algorithms.
//! - The compare-all driver (writes per-variant JSONL logs without affecting
//!   the main artifact).
//!
//! The mechanical and LLM workflows themselves live in `pipeline.rs` and
//! `task.rs` respectively — this module is the routing/expansion/comparison
//! layer that sits on top.

use crate::domain::{AuditData, Scope, TopicMetadata, topic};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Which workflow to apply when building semantic links.
///
/// `Auto` (the default) picks per document: technical documents use the
/// mechanical-only workflow, non-technical use the LLM workflow. The other
/// values force a single workflow for every document, which is useful when
/// running side-by-side comparisons or evaluating one approach in isolation.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SemanticLinkingMode {
  /// Route by `is_technical`: technical → mechanical, non-technical → LLM.
  #[default]
  Auto,
  /// Force the LLM workflow (Pass 1 + Pass 2 LLM, Pass 3 LLM).
  Llm,
  /// Force mechanical Passes 1 & 2 + BM25 expansion in Pass 2 + LLM Pass 3.
  Bm25,
  /// Force mechanical-only Passes 1 & 2 + LLM Pass 3.
  Mechanical,
}

impl SemanticLinkingMode {
  pub fn parse(s: &str) -> Result<Self, String> {
    match s.trim().to_ascii_lowercase().as_str() {
      "auto" => Ok(SemanticLinkingMode::Auto),
      "llm" => Ok(SemanticLinkingMode::Llm),
      "bm25" => Ok(SemanticLinkingMode::Bm25),
      "mechanical" => Ok(SemanticLinkingMode::Mechanical),
      other => Err(format!(
        "invalid --semantic-linking-mode value '{other}' (expected one of: auto, llm, bm25, mechanical)"
      )),
    }
  }

  pub fn as_str(self) -> &'static str {
    match self {
      SemanticLinkingMode::Auto => "auto",
      SemanticLinkingMode::Llm => "llm",
      SemanticLinkingMode::Bm25 => "bm25",
      SemanticLinkingMode::Mechanical => "mechanical",
    }
  }
}

/// Cutoff algorithm for Pass 2 BM25 ranking.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CutoffAlgorithm {
  /// Algorithm A: hard floor + relative gap detection (default).
  #[default]
  Gap,
  /// Algorithm B: top-K with absolute score floor.
  TopKFloor,
}

impl CutoffAlgorithm {
  pub fn parse(s: &str) -> Result<Self, String> {
    match s.trim().to_ascii_lowercase().as_str() {
      "gap" => Ok(CutoffAlgorithm::Gap),
      "top-k-floor" | "topkfloor" | "top_k_floor" => Ok(CutoffAlgorithm::TopKFloor),
      other => Err(format!(
        "invalid --semantic-linking-pass2-algo value '{other}' (expected one of: gap, top-k-floor)"
      )),
    }
  }

  pub fn as_str(self) -> &'static str {
    match self {
      CutoffAlgorithm::Gap => "gap",
      CutoffAlgorithm::TopKFloor => "top-k-floor",
    }
  }
}

/// Top-level configuration for the semantic-linking pipeline.
#[derive(Debug, Default, Clone, Copy)]
pub struct SemanticLinkingConfig {
  pub mode: SemanticLinkingMode,
  pub pass2_algo: CutoffAlgorithm,
  /// When set, run all four workflow variants per section (mechanical,
  /// bm25-gap, bm25-top-k-floor, llm) for Passes 1 & 2 only and write
  /// per-variant logs to `<output_dir>/semantic-linking-compare/`. Pass 3
  /// runs only for the configured workflow's matches; the comparison
  /// outputs are discarded after logging.
  pub compare_all: bool,
  /// When set, run only mechanical Pass 1 + Pass 2 (no LLM, no Pass 3),
  /// write a detailed JSONL trace of every section's resolved/unresolved
  /// inline-code references and derived contract/member candidates to
  /// `<output_dir>/mechanical-trace.jsonl`, then exit. Used to verify the
  /// deterministic name resolver before treating its output as the floor
  /// of the comparison benchmark.
  pub mechanical_trace: bool,
}

/// Resolved per-section workflow choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionWorkflow {
  Mechanical,
  Bm25,
  Llm,
}

/// Choose the workflow for a single section, given the global mode and the
/// section's parent document `is_technical` flag.
pub fn workflow_for_section(
  mode: SemanticLinkingMode,
  is_technical: bool,
) -> SectionWorkflow {
  match mode {
    SemanticLinkingMode::Auto => {
      if is_technical {
        SectionWorkflow::Mechanical
      } else {
        SectionWorkflow::Llm
      }
    }
    SemanticLinkingMode::Llm => SectionWorkflow::Llm,
    SemanticLinkingMode::Bm25 => SectionWorkflow::Bm25,
    SemanticLinkingMode::Mechanical => SectionWorkflow::Mechanical,
  }
}

// ---------------------------------------------------------------------------
// is_technical lookup
// ---------------------------------------------------------------------------

/// Index of `is_technical` flags keyed by document container path. Build
/// once per audit (O(N) over `topic_metadata`); subsequent per-section
/// lookups are O(1).
pub struct IsTechnicalIndex {
  by_container: std::collections::HashMap<crate::domain::ProjectPath, bool>,
}

impl IsTechnicalIndex {
  pub fn build(audit_data: &AuditData) -> Self {
    let mut by_container = std::collections::HashMap::new();
    for m in audit_data.topic_metadata.values() {
      if let TopicMetadata::DocumentationTopic {
        scope: Scope::Container { container },
        is_technical,
        ..
      } = m
      {
        by_container.insert(container.clone(), *is_technical);
      }
    }
    IsTechnicalIndex { by_container }
  }

  /// Look up the `is_technical` flag for the document containing this section.
  /// Returns `false` (the safer "route to LLM" default) if the section's
  /// document can't be found in the index.
  pub fn lookup(
    &self,
    section_topic: &topic::Topic,
    audit_data: &AuditData,
  ) -> bool {
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
    self.by_container.get(container).copied().unwrap_or(false)
  }
}

/// One-shot helper for callers that don't already hold an
/// [`IsTechnicalIndex`]. Allocates a fresh index per call and is therefore
/// `O(N)` per invocation — prefer building the index once when checking
/// many sections (the routing layer in `pipeline.rs` does this).
pub fn section_is_technical(
  section_topic: &topic::Topic,
  audit_data: &AuditData,
) -> bool {
  IsTechnicalIndex::build(audit_data).lookup(section_topic, audit_data)
}

// ---------------------------------------------------------------------------
// CLI / env parsing
// ---------------------------------------------------------------------------

/// Parse the three `--semantic-linking-*` flags from a CLI argument list,
/// falling back to env vars when a flag is not present. Returns `(config,
/// remaining_args)` — non-recognized arguments are passed through unchanged.
///
/// Supports both `--flag=value` and `--flag value` forms; for the
/// `compare-all` boolean flag, both bare presence (`--semantic-linking-compare-all`)
/// and `--semantic-linking-compare-all=true|false` are accepted.
pub fn parse_cli(
  args: &[String],
) -> Result<(SemanticLinkingConfig, Vec<String>), String> {
  let mut mode: Option<SemanticLinkingMode> = None;
  let mut algo: Option<CutoffAlgorithm> = None;
  let mut compare_all: Option<bool> = None;
  let mut mechanical_trace: Option<bool> = None;
  let mut remaining: Vec<String> = Vec::with_capacity(args.len());

  let mut i = 0;
  while i < args.len() {
    let arg = &args[i];

    // --flag=value form
    if let Some(eq) = arg.find('=') {
      let (flag, val) = arg.split_at(eq);
      let val = &val[1..];
      match flag {
        "--semantic-linking-mode" => {
          mode = Some(SemanticLinkingMode::parse(val)?);
          i += 1;
          continue;
        }
        "--semantic-linking-pass2-algo" => {
          algo = Some(CutoffAlgorithm::parse(val)?);
          i += 1;
          continue;
        }
        "--semantic-linking-compare-all" => {
          compare_all = Some(parse_bool(val)?);
          i += 1;
          continue;
        }
        "--semantic-linking-mechanical-trace" => {
          mechanical_trace = Some(parse_bool(val)?);
          i += 1;
          continue;
        }
        _ => {}
      }
    }

    // --flag value form (or bare boolean)
    match arg.as_str() {
      "--semantic-linking-mode" => {
        let val = args.get(i + 1).ok_or_else(|| {
          "--semantic-linking-mode requires a value".to_string()
        })?;
        mode = Some(SemanticLinkingMode::parse(val)?);
        i += 2;
        continue;
      }
      "--semantic-linking-pass2-algo" => {
        let val = args.get(i + 1).ok_or_else(|| {
          "--semantic-linking-pass2-algo requires a value".to_string()
        })?;
        algo = Some(CutoffAlgorithm::parse(val)?);
        i += 2;
        continue;
      }
      "--semantic-linking-compare-all" => {
        compare_all = Some(true);
        i += 1;
        continue;
      }
      "--semantic-linking-mechanical-trace" => {
        mechanical_trace = Some(true);
        i += 1;
        continue;
      }
      _ => {}
    }

    remaining.push(arg.clone());
    i += 1;
  }

  // Env-var fallback (lower precedence than CLI).
  if let (None, Ok(s)) = (mode, std::env::var("O11A_SEMANTIC_LINKING_MODE")) {
    mode = Some(SemanticLinkingMode::parse(&s)?);
  }
  if let (None, Ok(s)) =
    (algo, std::env::var("O11A_SEMANTIC_LINKING_PASS2_ALGO"))
  {
    algo = Some(CutoffAlgorithm::parse(&s)?);
  }
  if let (None, Ok(s)) =
    (compare_all, std::env::var("O11A_SEMANTIC_LINKING_COMPARE_ALL"))
  {
    compare_all = Some(parse_bool(&s)?);
  }
  if let (None, Ok(s)) = (
    mechanical_trace,
    std::env::var("O11A_SEMANTIC_LINKING_MECHANICAL_TRACE"),
  ) {
    mechanical_trace = Some(parse_bool(&s)?);
  }

  Ok((
    SemanticLinkingConfig {
      mode: mode.unwrap_or_default(),
      pass2_algo: algo.unwrap_or_default(),
      compare_all: compare_all.unwrap_or(false),
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
// BM25 plumbing
// ---------------------------------------------------------------------------

pub mod bm25;

// ---------------------------------------------------------------------------
// Mechanical-only Pass 1 + Pass 2 trace mode (--semantic-linking-mechanical-trace)
// ---------------------------------------------------------------------------

pub mod trace;

// ---------------------------------------------------------------------------
// Comparison harness (--semantic-linking-compare-all)
// ---------------------------------------------------------------------------

pub mod compare;

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parse_mode_values() {
    assert_eq!(
      SemanticLinkingMode::parse("auto").unwrap(),
      SemanticLinkingMode::Auto
    );
    assert_eq!(
      SemanticLinkingMode::parse("LLM").unwrap(),
      SemanticLinkingMode::Llm
    );
    assert_eq!(
      SemanticLinkingMode::parse(" bm25 ").unwrap(),
      SemanticLinkingMode::Bm25
    );
    assert_eq!(
      SemanticLinkingMode::parse("mechanical").unwrap(),
      SemanticLinkingMode::Mechanical
    );
    assert!(SemanticLinkingMode::parse("nonsense").is_err());
  }

  #[test]
  fn parse_algo_values() {
    assert_eq!(
      CutoffAlgorithm::parse("gap").unwrap(),
      CutoffAlgorithm::Gap
    );
    assert_eq!(
      CutoffAlgorithm::parse("top-k-floor").unwrap(),
      CutoffAlgorithm::TopKFloor
    );
    assert_eq!(
      CutoffAlgorithm::parse("topkfloor").unwrap(),
      CutoffAlgorithm::TopKFloor
    );
    assert!(CutoffAlgorithm::parse("nonsense").is_err());
  }

  #[test]
  fn workflow_for_section_routes_by_is_technical() {
    assert_eq!(
      workflow_for_section(SemanticLinkingMode::Auto, true),
      SectionWorkflow::Mechanical
    );
    assert_eq!(
      workflow_for_section(SemanticLinkingMode::Auto, false),
      SectionWorkflow::Llm
    );
    assert_eq!(
      workflow_for_section(SemanticLinkingMode::Llm, true),
      SectionWorkflow::Llm
    );
    assert_eq!(
      workflow_for_section(SemanticLinkingMode::Llm, false),
      SectionWorkflow::Llm
    );
    assert_eq!(
      workflow_for_section(SemanticLinkingMode::Bm25, true),
      SectionWorkflow::Bm25
    );
    assert_eq!(
      workflow_for_section(SemanticLinkingMode::Bm25, false),
      SectionWorkflow::Bm25
    );
    assert_eq!(
      workflow_for_section(SemanticLinkingMode::Mechanical, true),
      SectionWorkflow::Mechanical
    );
    assert_eq!(
      workflow_for_section(SemanticLinkingMode::Mechanical, false),
      SectionWorkflow::Mechanical
    );
  }

  #[test]
  fn cli_parses_value_in_two_forms() {
    let args = vec![
      "--semantic-linking-mode".to_string(),
      "bm25".to_string(),
      "--semantic-linking-pass2-algo=top-k-floor".to_string(),
      "--semantic-linking-compare-all".to_string(),
      "positional1".to_string(),
      "positional2".to_string(),
    ];
    let (cfg, rest) = parse_cli(&args).unwrap();
    assert_eq!(cfg.mode, SemanticLinkingMode::Bm25);
    assert_eq!(cfg.pass2_algo, CutoffAlgorithm::TopKFloor);
    assert!(cfg.compare_all);
    assert_eq!(rest, vec!["positional1", "positional2"]);
  }

  #[test]
  fn cli_passes_through_unrecognized_args() {
    // Sanity: positional args are returned unchanged when no flags appear.
    let args: Vec<String> = vec![
      "positional1".to_string(),
      "positional2".to_string(),
      "positional3".to_string(),
    ];
    let (_, rest) = parse_cli(&args).unwrap();
    assert_eq!(rest, vec!["positional1", "positional2", "positional3"]);
  }

  #[test]
  fn cli_rejects_unknown_mode_value() {
    let args = vec![
      "--semantic-linking-mode=invalid-mode".to_string(),
    ];
    let err = parse_cli(&args).unwrap_err();
    assert!(err.contains("invalid"), "got: {}", err);
  }

  #[test]
  fn cli_rejects_unknown_algo_value() {
    let args = vec![
      "--semantic-linking-pass2-algo=invalid-algo".to_string(),
    ];
    let err = parse_cli(&args).unwrap_err();
    assert!(err.contains("invalid"), "got: {}", err);
  }

  #[test]
  fn cli_compare_all_value_form_accepts_false() {
    let args = vec![
      "--semantic-linking-compare-all=false".to_string(),
      "positional".to_string(),
    ];
    let (cfg, rest) = parse_cli(&args).unwrap();
    assert!(!cfg.compare_all);
    assert_eq!(rest, vec!["positional"]);
  }

  #[test]
  fn cli_missing_value_after_flag_errors() {
    let args = vec!["--semantic-linking-mode".to_string()];
    let err = parse_cli(&args).unwrap_err();
    assert!(err.contains("requires a value"), "got: {}", err);
  }
}
