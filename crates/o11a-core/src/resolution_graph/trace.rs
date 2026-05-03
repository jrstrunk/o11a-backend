//! Per-resolution explanation records produced by the graph-driven
//! resolution passes (Phase B in the doc-tree consumer, Phase B in the
//! dev-doc consumer, and — once Phases C / D / E land — the later phases
//! of the resolution pipeline).
//!
//! Each ambiguous reference that the resolution pass attempts produces
//! one `ResolutionTrace`, regardless of whether a winning candidate was
//! picked. Recording attempts that did *not* resolve is the contract
//! that lets operators see *why* the threshold wasn't met and override
//! the resolver if needed; the spec's "Confidence threshold and
//! fallback" section pins this behavior.
//!
//! Storage layout: traces live on `AuditData::resolution_traces` keyed
//! by a `ResolutionRefId`. The key is intentionally an enum (not the
//! raw `i32` doc node ID) so Phase 7's NatSpec resolution pass can plug
//! its own variant in additively, and Phase 11's dump tooling can stay
//! one read site.

use serde::{Deserialize, Serialize};

use super::edge::EdgeType;
use crate::domain::topic;

/// Identifies which reference a trace is about. A doc-tree
/// `CodeIdentifier` is keyed by its node ID (parser-assigned, unique
/// within an audit). Future variants land additively.
#[derive(
  Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub enum ResolutionRefId {
  /// A `DocumentationNode::CodeIdentifier` produced by the doc parser.
  /// Holds the doc node ID.
  DocumentationNode(i32),
  /// A `CommentNode::CodeIdentifier` inside a synthetic dev-doc
  /// `CommentTopic` (NatSpec block, SemanticBlock inline comment).
  /// Identified by the comment's topic plus the 0-based depth-first
  /// occurrence index of the `CodeIdentifier` within the comment's
  /// node tree. The pair is unique within an audit because each
  /// CommentTopic has its own topic ID and the occurrence index
  /// depth-first-orders the identifiers inside it.
  DevDocComment {
    comment_topic: topic::Topic,
    occurrence: u32,
  },
}

/// Which phase of the resolution pipeline produced this trace. Phase 6
/// only records `PhaseB` for resolved entries and `Unresolved` for
/// attempts that fell through the threshold; later phases will use the
/// remaining variants.
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
pub enum ResolutionPhase {
  /// Resolved by Phase B — section-context personalized PageRank.
  PhaseB,
  /// Resolved by Phase C — co-location pinning. (Reserved; written by
  /// Phase 9 and later.)
  PhaseC,
  /// Resolved by Phase E — anchor-by-name fallback. (Reserved; written
  /// by Phase 10.)
  PhaseE,
  /// Attempted but no candidate cleared the confidence threshold. Phase
  /// 6 leaves `referenced_topic = None` for these; Phase 10's anchor
  /// fallback will revisit them.
  Unresolved,
}

/// One candidate's PageRank score in the ranked candidate list. Sorted
/// in the trace by the spec's tie-break order: PR descending → qualified
/// name ascending → topic ID ascending.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateScore {
  pub topic: topic::Topic,
  /// `qualified_name` from `TopicMetadata::qualified_name`. `None` when
  /// the candidate's metadata cannot produce one (defensive — every
  /// `NamedTopic` should). Stored as a snapshot so operator inspection
  /// does not need to re-query `AuditData` for it.
  pub qualified_name: Option<String>,
  /// PR score after the configured iteration count. `f32` matches the
  /// engine's accumulator type.
  pub pr_score: f32,
}

/// One predecessor's contribution to the chosen candidate's PR mass.
/// Used to surface "this resolution won because of these edges" for
/// operator inspection. The contribution is the steady-state mass
/// `final_r[predecessor] * (weight / total_outgoing_weight)` — the
/// per-iteration mass that flows from predecessor to candidate at the
/// fixed point. Multiplying by the damping factor gives the actual
/// per-iteration arrival; we surface the unscaled flow here because it
/// is more interpretable as "how strongly this predecessor pushes mass
/// at this candidate".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeContribution {
  pub predecessor: topic::Topic,
  pub edge_type: EdgeType,
  pub weighted_contribution: f32,
}

/// Persistent record of one ambiguous-reference resolution attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolutionTrace {
  pub reference_id: ResolutionRefId,
  /// The literal text of the reference (e.g. `"transfer"`). Cached so
  /// inspection tools don't have to look up the AST node.
  pub identifier: String,
  /// Topic representing the doc-section / NatSpec block whose seed
  /// vector produced this scoring. For Phase 6, this is the
  /// `Topic::Documentation(node_id)` of the enclosing `Section`
  /// or document `Root`.
  pub section_topic: topic::Topic,
  /// Which pipeline phase resolved this reference (or `Unresolved`).
  pub phase_resolved: ResolutionPhase,
  /// Phase D iteration that produced the resolution. Phase 6 always
  /// emits `1`; Phases 9 and 10 will increment.
  pub iteration: u32,
  /// Topic chosen by the resolver, or `None` when no candidate cleared
  /// the threshold.
  pub chosen_topic: Option<topic::Topic>,
  /// Every candidate the resolver scored, ordered by the deterministic
  /// tie-break. Always populated even when no winner was picked.
  pub candidate_scores: Vec<CandidateScore>,
  /// The (predecessor, edge_type) pairs whose flow into the chosen
  /// candidate dominated its PR mass at steady state, sorted descending
  /// by `weighted_contribution`. Capped at 3 entries per the spec.
  /// Empty when `chosen_topic` is `None` — without a winner there is
  /// nothing to attribute.
  pub top_contributing_edges: Vec<EdgeContribution>,
}

#[cfg(test)]
mod tests {
  use super::*;

  fn t(id: i32) -> topic::Topic {
    topic::new_node_topic(&id)
  }

  /// A minimal trace serializes cleanly and round-trips through serde.
  /// Pinning this lets Phase 11's dump tooling rely on the on-disk
  /// shape without depending on a separate fixture suite.
  #[test]
  fn resolution_trace_serde_round_trip() {
    let original = ResolutionTrace {
      reference_id: ResolutionRefId::DocumentationNode(7),
      identifier: "transfer".to_string(),
      section_topic: topic::new_documentation_topic(3),
      phase_resolved: ResolutionPhase::PhaseB,
      iteration: 1,
      chosen_topic: Some(t(42)),
      candidate_scores: vec![
        CandidateScore {
          topic: t(42),
          qualified_name: Some("Vault.transfer".to_string()),
          pr_score: 0.75,
        },
        CandidateScore {
          topic: t(43),
          qualified_name: Some("Token.transfer".to_string()),
          pr_score: 0.20,
        },
      ],
      top_contributing_edges: vec![EdgeContribution {
        predecessor: t(42),
        edge_type: EdgeType::ContainsMember,
        weighted_contribution: 0.40,
      }],
    };
    let bytes = serde_json::to_vec(&original).unwrap();
    let decoded: ResolutionTrace = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(original, decoded);
  }

  /// `ResolutionRefId` is the key type on `AuditData::resolution_traces`,
  /// so a wire change (e.g. variant reorder) would corrupt the trace
  /// dump's BTreeMap iteration order. Pin the encoding.
  #[test]
  fn resolution_ref_id_documentation_node_serializes_predictably() {
    let id = ResolutionRefId::DocumentationNode(42);
    let s = serde_json::to_string(&id).unwrap();
    assert_eq!(s, r#"{"DocumentationNode":42}"#);
  }

  /// Same pin for the dev-doc variant added in Phase 7 — its on-disk
  /// shape is shared with downstream tooling (Phase 11 dump kinds).
  #[test]
  fn resolution_ref_id_dev_doc_comment_serializes_predictably() {
    let id = ResolutionRefId::DevDocComment {
      comment_topic: topic::new_comment_topic(-7),
      occurrence: 3,
    };
    let s = serde_json::to_string(&id).unwrap();
    assert!(
      s.contains(r#""DevDocComment""#)
        && s.contains(r#""occurrence":3"#),
      "got: {s}"
    );
    let round: ResolutionRefId = serde_json::from_str(&s).unwrap();
    assert_eq!(round, id);
  }

  /// `ResolutionRefId` is the BTreeMap key on `resolution_traces`. The
  /// derived `Ord` orders by variant declaration order, so existing
  /// `DocumentationNode` keys sort before any newly-added
  /// `DevDocComment` keys. Pin that.
  #[test]
  fn resolution_ref_id_doc_variant_orders_before_dev_doc_variant() {
    let doc = ResolutionRefId::DocumentationNode(999);
    let dev = ResolutionRefId::DevDocComment {
      comment_topic: topic::new_comment_topic(-1),
      occurrence: 0,
    };
    assert!(doc < dev);
  }

  /// `ResolutionPhase` similarly has on-disk shape that the trace dump
  /// reads. Pin every variant so any silent rename trips here, not in
  /// downstream consumers.
  #[test]
  fn resolution_phase_serializes_to_named_string() {
    assert_eq!(
      serde_json::to_string(&ResolutionPhase::PhaseB).unwrap(),
      r#""PhaseB""#
    );
    assert_eq!(
      serde_json::to_string(&ResolutionPhase::PhaseC).unwrap(),
      r#""PhaseC""#
    );
    assert_eq!(
      serde_json::to_string(&ResolutionPhase::PhaseE).unwrap(),
      r#""PhaseE""#
    );
    assert_eq!(
      serde_json::to_string(&ResolutionPhase::Unresolved).unwrap(),
      r#""Unresolved""#
    );
  }
}
