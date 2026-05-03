//! Phases B + C + D of the resolution pipeline for synthetic developer
//! documentation: NatSpec docstrings on contracts/functions/modifiers,
//! per-parameter `@param` blocks, and SemanticBlock inline source
//! comments. Mirrors the doc-tree pass in
//! `crates/o11a-analyze/src/documentation/resolution_pass.rs` — same
//! algorithm, same threshold, same determinism contract — but seeds
//! from the source-tree scope chain of each comment's `target_topic`
//! instead of the doc-tree header hierarchy.
//!
//! Pipeline order (set by `analysis.rs`):
//!
//! ```text
//! solidity::analyzer::analyze
//! resolution_graph::build              ← graph populated
//! inject_developer_documentation       ← synthetic CommentTopics + Phase A
//! resolve_dev_doc_comments             ← THIS pass (Phases B + C + D)
//! documentation::analyzer::analyze
//! ```
//!
//! The pass reads `audit_data.resolution_graph`, `audit_data.name_index`,
//! and the comment's stored `Vec<CommentNode>`; it mutates the comment's
//! node tree (in-place rewrites of ambiguous `referenced_topic`),
//! `audit_data.mentions_index` (additive merge of newly-resolved
//! mentions), `audit_data.topic_metadata` (the `mentioned_topics` field
//! on each updated `CommentTopic`), and `audit_data.resolution_traces`
//! (one trace per attempted ambiguous reference).
//!
//! Scope-chain seeding follows the spec table verbatim:
//!
//! | Attached topic kind | Chain (distance 0 → up)        | Default seeds |
//! |---|---|---|
//! | Contract            | contract                       | 1.0 |
//! | State variable      | state-var → contract           | 1.0, 0.5 |
//! | Function / modifier | function → contract            | 1.0, 0.5 |
//! | `@param`            | param → function → contract    | 1.0, 0.5, 0.25 |
//! | SemanticBlock       | block → function → contract    | 1.0, 0.5, 0.25 |
//! | Nested block        | (extend with halving)          | 1.0, 0.5, 0.25, ... |
//!
//! Phase-A-resolved references inside the comment text seed at distance
//! 0 alongside the attached topic. Same-scope siblings are reached via
//! the graph's `contains-member` and `contains-local` edges — siblings
//! are deliberately NOT seeded individually (that is a calibration
//! knob, per the spec).
//!
//! Phase D (re-iteration) runs Phases B + C until either no new
//! resolutions appear or the iteration cap (`MAX_ITERATIONS = 4`) is
//! hit. Each iteration's resolutions feed the next iteration's seed
//! vector, so cascading disambiguation is automatic.

use std::collections::{BTreeMap, BTreeSet};

use o11a_core::collaborator::models::Author;
use o11a_core::collaborator::parser::CommentNode;
use o11a_core::domain;
use o11a_core::domain::Node;
use o11a_core::domain::topic;
use o11a_core::resolution_graph::{
  CandidateScore, CoLocInput, EdgeContribution, OutEdge, ResolutionGraph,
  ResolutionPhase, ResolutionRefId, ResolutionTrace, co_locate,
  personalized_pagerank,
};

/// Confidence threshold from the spec's "Confidence threshold and
/// fallback" section. Must match the doc-tree pass's value — the two
/// consumers share the contract.
const CONFIDENCE_THRESHOLD: f32 = 0.65;

/// Maximum distance (in scope-chain hops) at which an ancestor still
/// contributes a seed. Beyond this, `2^(-d)` falls below `1/64` and
/// caps the seed vector against pathologically deep nesting.
const MAX_SEED_DEPTH: u32 = 6;

/// Spec's "top three contributing edges" cap for the resolution trace.
const MAX_TOP_EDGES: usize = 3;

/// Phase D iteration cap. Same as the doc-tree pass — most ambiguity
/// converges in 1-2 iterations; the cap protects pathological cases.
const MAX_ITERATIONS: u32 = 4;

/// Walks every dev-doc CommentTopic in `audit_data` and resolves
/// ambiguous `CommentNode::CodeIdentifier` nodes inside each comment's
/// stored node tree using personalized PageRank seeded by the target
/// topic's scope chain (Phase B), co-location pinning of remaining
/// pairs (Phase C), and re-iteration of B + C until a fixed point or
/// `MAX_ITERATIONS` rounds (Phase D). Mutates the comment's node tree
/// in place, merges newly-resolved references into
/// `audit_data.mentions_index` and the `mentioned_topics` field on
/// each `CommentTopic`, and records one `ResolutionTrace` per
/// attempted ambiguous reference on `audit_data.resolution_traces`.
///
/// No-op when the audit's `resolution_graph` has not been built (Phase
/// 4 of the build plan didn't run): the early-exit guard returns
/// before any plan enumeration so the pass is safe to call before the
/// graph is wired in.
pub fn resolve_dev_doc_comments(audit_data: &mut domain::AuditData) {
  if audit_data.resolution_graph.is_none() {
    return;
  }

  // Pass 1 — read-only walk: snapshot every dev-doc CommentTopic, its
  // target topic, scope chain, the Phase-A-resolved topics inside its
  // comment-node tree, and the depth-first index of each ambiguous
  // CodeIdentifier. Because Pass 2 needs read access to most of
  // `AuditData`, we collect plans into an owned vector first instead
  // of streaming.
  let mut plans = collect_plans(audit_data);
  if plans.is_empty() {
    return;
  }

  // Pass 2 — score: Phases B + C run inside a Phase D iteration loop.
  // Each iteration mutates `plans` (pinning resolved refs into the
  // plan's `phase_a_topics`) and accumulates resolutions + traces.
  // The graph is borrowed for the duration of this call; we re-borrow
  // it inside `compute_resolutions` rather than passing it, since it
  // can be read off `audit_data` directly.
  let (resolutions, traces) = compute_resolutions(&mut plans, audit_data);

  // Pass 3 — mutate. Apply each resolution to its comment's node tree,
  // merge newly-resolved mentions into `mentions_index` and the
  // `mentioned_topics` field on the comment's metadata, and persist
  // traces.
  apply_pass(audit_data, &plans, &resolutions, traces);
}

// ---------------------------------------------------------------------
// Pass 1 — plan enumeration
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
struct CommentPlan {
  /// The synthetic CommentTopic itself (C-prefixed). Used as the trace
  /// key's discriminator and as the lookup key into `audit_data.nodes`
  /// during mutation.
  comment_topic: topic::Topic,
  /// Seed table from the spec: the target topic at distance 0
  /// (`scope_chain[0]`), then each enclosing scope topic at distances
  /// 1, 2, … capped at `MAX_SEED_DEPTH`.
  scope_chain: Vec<topic::Topic>,
  /// Phase-A-resolved `referenced_topic` values harvested from this
  /// comment's `CodeIdentifier` nodes. Sorted ascending and deduped so
  /// the seed-vector builder's iteration order is deterministic. Phase
  /// D appends new resolutions here at the end of each iteration.
  phase_a_topics: Vec<topic::Topic>,
  /// `(occurrence_index, identifier)` for every still-ambiguous
  /// `CodeIdentifier` in the comment's node tree, in depth-first
  /// document order. As Phase D iterates, resolved refs are removed.
  /// The occurrence index is the one used to key into
  /// `ResolutionRefId::DevDocComment`.
  ambiguous_refs: Vec<AmbiguousRef>,
}

#[derive(Debug, Clone)]
struct AmbiguousRef {
  occurrence: u32,
  identifier: String,
}

/// Walk `audit_data.topic_metadata` for every dev-doc CommentTopic and
/// build a plan per comment. Skips comments whose author is neither
/// `DevTechnical` nor `DevDocumentation` — those are user-authored or
/// agent-authored conversation comments and are not in scope for this
/// pass.
///
/// Iteration order is the `topic_metadata` BTreeMap's ascending topic
/// order, which is deterministic. Plans within the returned vector
/// inherit that order.
fn collect_plans(audit_data: &domain::AuditData) -> Vec<CommentPlan> {
  let mut plans = Vec::new();

  for (comment_topic, metadata) in &audit_data.topic_metadata {
    let domain::TopicMetadata::CommentTopic {
      target_topic,
      author,
      ..
    } = metadata
    else {
      continue;
    };
    if !matches!(author, Author::DevTechnical | Author::DevDocumentation) {
      continue;
    }

    // The comment's CommentNode tree lives in `audit_data.nodes` keyed
    // by the comment topic. A missing entry would mean the synthetic
    // comment was created without its tree — defensive skip rather
    // than panic.
    let Some(Node::Comment(nodes)) = audit_data.nodes.get(comment_topic) else {
      continue;
    };

    let mut phase_a_topics: Vec<topic::Topic> = Vec::new();
    let mut ambiguous_refs: Vec<AmbiguousRef> = Vec::new();
    let mut counter: u32 = 0;
    for node in nodes {
      collect_refs(node, &mut counter, &mut phase_a_topics, &mut ambiguous_refs);
    }

    // Skip comments with no ambiguous references entirely: nothing for
    // Pass 2 to score and nothing for Pass 3 to mutate. The scope-chain
    // walk and phase_a sort/dedup would both be wasted work. This is
    // safe because — unlike the doc-tree pass's section-as-ancestor
    // model — dev-doc comments do not seed each other; each comment's
    // PR run is isolated to its own scope chain.
    if ambiguous_refs.is_empty() {
      continue;
    }

    phase_a_topics.sort();
    phase_a_topics.dedup();

    plans.push(CommentPlan {
      comment_topic: *comment_topic,
      scope_chain: build_scope_chain(audit_data, *target_topic),
      phase_a_topics,
      ambiguous_refs,
    });
  }

  plans
}

/// Build the seed-table chain for a target topic, capped at
/// `MAX_SEED_DEPTH + 1` entries — one per distance level that can
/// still contribute non-zero mass. The chain is indexed by distance
/// starting at 0, so a chain of length N reaches distance `N - 1`;
/// truncating to `MAX_SEED_DEPTH + 1` guarantees the seed vector
/// never weights an ancestor below `2^(-MAX_SEED_DEPTH)`.
fn build_scope_chain(
  audit_data: &domain::AuditData,
  target: topic::Topic,
) -> Vec<topic::Topic> {
  let mut chain = domain::scope_ancestor_chain(audit_data, target);
  chain.truncate((MAX_SEED_DEPTH as usize).saturating_add(1));
  chain
}

/// Depth-first walk of one CommentNode subtree, recording Phase-A topics
/// and indexing ambiguous `CodeIdentifier` occurrences for trace keys.
fn collect_refs(
  node: &CommentNode,
  counter: &mut u32,
  phase_a_topics: &mut Vec<topic::Topic>,
  ambiguous_refs: &mut Vec<AmbiguousRef>,
) {
  match node {
    CommentNode::CodeIdentifier {
      value,
      referenced_topic,
      ..
    } => {
      let occurrence = *counter;
      *counter = counter.wrapping_add(1);
      match referenced_topic {
        Some(t) => phase_a_topics.push(*t),
        None => ambiguous_refs.push(AmbiguousRef {
          occurrence,
          identifier: value.clone(),
        }),
      }
    }
    CommentNode::InlineCode { children, .. } => {
      for c in children {
        collect_refs(c, counter, phase_a_topics, ambiguous_refs);
      }
    }
    // Leaf-like variants: no children that can carry a CodeIdentifier.
    CommentNode::Text { .. }
    | CommentNode::CodeKeyword { .. }
    | CommentNode::CodeOperator { .. }
    | CommentNode::CodeText { .. }
    | CommentNode::Emphasis { .. }
    | CommentNode::Strong { .. }
    | CommentNode::Link { .. } => {}
  }
}

// ---------------------------------------------------------------------
// Pass 2 — scoring (Phases B + C inside Phase D loop)
// ---------------------------------------------------------------------

/// One winning resolution. Carried out of Pass 2 and applied to the
/// comment's node tree in Pass 3. The new `kind` and `referenced_name`
/// mirror what the parser would have written next to a Phase-A
/// resolution.
#[derive(Debug)]
struct AppliedResolution {
  chosen_topic: topic::Topic,
  kind: Option<domain::NamedTopicKind>,
  referenced_name: Option<String>,
}

/// Map of winning resolutions, keyed by `(comment_topic, occurrence)`
/// — Pass 3 applies the entry whose key matches each ambiguous
/// `CodeIdentifier` it walks.
type ResolutionMap = BTreeMap<(topic::Topic, u32), AppliedResolution>;

/// Trace map keyed by ref id. Each iteration overwrites entries for
/// refs it attempted, so the final map carries the *latest* attempt
/// per ref — exactly what Phase 11's dump tooling expects.
type TraceMap = BTreeMap<ResolutionRefId, ResolutionTrace>;

/// Compute every comment's resolutions. Resolutions are keyed by
/// `(comment_topic, occurrence)`; traces are keyed by
/// `ResolutionRefId::DevDocComment { comment_topic, occurrence }`.
///
/// Plans are mutated across Phase D iterations: pinned refs migrate
/// from `ambiguous_refs` into `phase_a_topics` so the next iteration's
/// seed vector reflects them.
fn compute_resolutions(
  plans: &mut [CommentPlan],
  audit_data: &domain::AuditData,
) -> (ResolutionMap, TraceMap) {
  let graph = audit_data
    .resolution_graph
    .as_ref()
    .expect("graph existence checked by caller");

  let mut resolutions: ResolutionMap = BTreeMap::new();
  let mut traces: TraceMap = BTreeMap::new();

  for iteration in 1..=MAX_ITERATIONS {
    let mut newly_resolved: usize = 0;
    for plan_idx in 0..plans.len() {
      if plans[plan_idx].ambiguous_refs.is_empty() {
        continue;
      }

      // Phase B — PR scoring of this comment's still-ambiguous refs.
      let pr_result = run_phase_b(
        plans,
        plan_idx,
        audit_data,
        graph,
        iteration,
        &mut resolutions,
        &mut traces,
        &mut newly_resolved,
      );

      if plans[plan_idx].ambiguous_refs.is_empty() {
        continue;
      }

      // Phase C — co-location pinning of remaining pairs.
      run_phase_c(
        plans,
        plan_idx,
        audit_data,
        graph,
        iteration,
        &pr_result,
        &mut resolutions,
        &mut traces,
        &mut newly_resolved,
      );
    }

    if newly_resolved == 0 {
      break;
    }
  }

  (resolutions, traces)
}

/// Run Phase B for one comment plan: build the seed vector, run PR,
/// score remaining ambiguous refs, apply the threshold rule. Resolved
/// refs migrate to the plan's `phase_a_topics`; survivors stay in
/// `ambiguous_refs` for Phase C. Returns the PR result so Phase C can
/// reuse it for trace candidate scores without re-running the engine.
#[allow(clippy::too_many_arguments)]
fn run_phase_b(
  plans: &mut [CommentPlan],
  plan_idx: usize,
  audit_data: &domain::AuditData,
  graph: &ResolutionGraph,
  iteration: u32,
  resolutions: &mut ResolutionMap,
  traces: &mut TraceMap,
  newly_resolved: &mut usize,
) -> BTreeMap<topic::Topic, f32> {
  let seeds = build_seed_vector(&plans[plan_idx]);

  // PR with an empty seed vector returns all-zero per the engine's
  // contract; emit Unresolved traces and skip the PR work in that case.
  let pr_result = if seeds.is_empty() {
    BTreeMap::new()
  } else {
    personalized_pagerank(graph, &seeds)
  };

  let comment_topic = plans[plan_idx].comment_topic;

  let mut survivors: Vec<AmbiguousRef> = Vec::new();
  let mut resolved_here: u32 = 0;
  for ambiguous in std::mem::take(&mut plans[plan_idx].ambiguous_refs) {
    let trace_key = ResolutionRefId::DevDocComment {
      comment_topic,
      occurrence: ambiguous.occurrence,
    };
    let candidates = audit_data
      .name_index
      .candidates_by_simple_name(&ambiguous.identifier);
    let candidate_scores = rank_candidates(candidates, audit_data, &pr_result);
    let (chosen, edges) =
      pick_phase_b_winner(&candidate_scores, graph, &pr_result);

    if let Some(chosen_topic) = chosen {
      let (kind, referenced_name) = lookup_kind_and_name(chosen_topic, audit_data);
      resolutions.insert(
        (comment_topic, ambiguous.occurrence),
        AppliedResolution {
          chosen_topic,
          kind,
          referenced_name,
        },
      );
      plans[plan_idx].phase_a_topics.push(chosen_topic);
      resolved_here += 1;
      traces.insert(
        trace_key.clone(),
        ResolutionTrace {
          reference_id: trace_key,
          identifier: ambiguous.identifier.clone(),
          // For dev-doc resolution, the "section" is the synthetic
          // CommentTopic — each comment owns one PR run.
          section_topic: comment_topic,
          phase_resolved: ResolutionPhase::PhaseB,
          iteration,
          chosen_topic: Some(chosen_topic),
          candidate_scores,
          top_contributing_edges: edges,
        },
      );
    } else {
      // Tentative Unresolved trace; Phase C may overwrite.
      traces.insert(
        trace_key.clone(),
        ResolutionTrace {
          reference_id: trace_key,
          identifier: ambiguous.identifier.clone(),
          section_topic: comment_topic,
          phase_resolved: ResolutionPhase::Unresolved,
          iteration,
          chosen_topic: None,
          candidate_scores,
          top_contributing_edges: Vec::new(),
        },
      );
      survivors.push(ambiguous);
    }
  }

  plans[plan_idx].ambiguous_refs = survivors;
  if resolved_here > 0 {
    plans[plan_idx].phase_a_topics.sort();
    plans[plan_idx].phase_a_topics.dedup();
    *newly_resolved += resolved_here as usize;
  }

  pr_result
}

/// Run Phase C for one comment plan: builds CoLocInput entries from
/// the comment's still-ambiguous refs, runs the shared `co_locate`
/// algorithm, and applies any pinnings it produces. Each pinned ref's
/// trace is rewritten to `PhaseC`, with the `candidate_scores` and
/// `top_contributing_edges` reused from the iteration's PR run so
/// operators see the PR ranking alongside the co-location decision.
#[allow(clippy::too_many_arguments)]
fn run_phase_c(
  plans: &mut [CommentPlan],
  plan_idx: usize,
  audit_data: &domain::AuditData,
  graph: &ResolutionGraph,
  iteration: u32,
  pr_result: &BTreeMap<topic::Topic, f32>,
  resolutions: &mut ResolutionMap,
  traces: &mut TraceMap,
  newly_resolved: &mut usize,
) {
  let comment_topic = plans[plan_idx].comment_topic;

  let inputs: Vec<CoLocInput<u32>> = plans[plan_idx]
    .ambiguous_refs
    .iter()
    .map(|a| CoLocInput {
      ref_id: a.occurrence,
      candidates: audit_data
        .name_index
        .candidates_by_simple_name(&a.identifier)
        .to_vec(),
    })
    .collect();

  let pinnings = co_locate(audit_data, &inputs);
  if pinnings.is_empty() {
    return;
  }

  let pinned_ids: BTreeMap<u32, topic::Topic> =
    pinnings.iter().map(|r| (r.ref_id, r.chosen_topic)).collect();

  let mut survivors: Vec<AmbiguousRef> = Vec::new();
  for ambiguous in std::mem::take(&mut plans[plan_idx].ambiguous_refs) {
    if let Some(&chosen_topic) = pinned_ids.get(&ambiguous.occurrence) {
      let trace_key = ResolutionRefId::DevDocComment {
        comment_topic,
        occurrence: ambiguous.occurrence,
      };
      let (kind, referenced_name) = lookup_kind_and_name(chosen_topic, audit_data);
      resolutions.insert(
        (comment_topic, ambiguous.occurrence),
        AppliedResolution {
          chosen_topic,
          kind,
          referenced_name,
        },
      );
      plans[plan_idx].phase_a_topics.push(chosen_topic);
      *newly_resolved += 1;

      let candidates = audit_data
        .name_index
        .candidates_by_simple_name(&ambiguous.identifier);
      let candidate_scores = rank_candidates(candidates, audit_data, pr_result);
      let edges = top_contributing_edges(graph, pr_result, chosen_topic);

      traces.insert(
        trace_key.clone(),
        ResolutionTrace {
          reference_id: trace_key,
          identifier: ambiguous.identifier.clone(),
          section_topic: comment_topic,
          phase_resolved: ResolutionPhase::PhaseC,
          iteration,
          chosen_topic: Some(chosen_topic),
          candidate_scores,
          top_contributing_edges: edges,
        },
      );
    } else {
      survivors.push(ambiguous);
    }
  }
  plans[plan_idx].ambiguous_refs = survivors;
  plans[plan_idx].phase_a_topics.sort();
  plans[plan_idx].phase_a_topics.dedup();
}

/// Build the seed vector for one comment plan: scope-chain entries at
/// `2^(-distance)` weights, plus Phase-A-resolved topics inside the
/// comment text at distance 0. Topics that appear in both lists sum
/// their weights, which deliberately gives the target topic a slight
/// bonus when an inline reference happens to mention it.
fn build_seed_vector(plan: &CommentPlan) -> BTreeMap<topic::Topic, f32> {
  let mut seeds: BTreeMap<topic::Topic, f32> = BTreeMap::new();

  // Scope chain — distance 0 is the target topic itself, distance 1
  // is the immediately enclosing scope, etc. The chain has already
  // been capped at `MAX_SEED_DEPTH + 1` entries, so the loop reaches
  // distance `MAX_SEED_DEPTH` at most.
  for (distance, topic) in plan.scope_chain.iter().enumerate() {
    let weight = (2.0_f32).powi(-(distance as i32));
    *seeds.entry(*topic).or_insert(0.0) += weight;
  }

  // Phase-A-resolved references in the comment text — distance 0,
  // alongside the target topic. Already sorted+deduped during plan
  // collection (and after each Phase D iteration), so iteration order
  // is deterministic.
  for topic in &plan.phase_a_topics {
    *seeds.entry(*topic).or_insert(0.0) += 1.0;
  }

  seeds
}

/// Score every candidate by its PR value, then sort by the
/// determinism-contract tie-break: PR descending → qualified-name
/// ascending → topic-ID ascending.
fn rank_candidates(
  candidates: &[topic::Topic],
  audit_data: &domain::AuditData,
  pr_result: &BTreeMap<topic::Topic, f32>,
) -> Vec<CandidateScore> {
  let mut scored: Vec<CandidateScore> = candidates
    .iter()
    .filter_map(|t| {
      let metadata = audit_data.topic_metadata.get(t)?;
      // Defensive — `candidates_by_simple_name` only inserts
      // NamedTopics, but skip anything else that leaks through so a
      // future refactor can't surprise us.
      if !matches!(metadata, domain::TopicMetadata::NamedTopic { .. }) {
        return None;
      }
      let pr_score = pr_result.get(t).copied().unwrap_or(0.0);
      Some(CandidateScore {
        topic: *t,
        qualified_name: metadata.qualified_name(audit_data),
        pr_score,
      })
    })
    .collect();

  scored.sort_by(|a, b| {
    b.pr_score
      .partial_cmp(&a.pr_score)
      .unwrap_or(std::cmp::Ordering::Equal)
      .then_with(|| a.qualified_name.cmp(&b.qualified_name))
      .then_with(|| a.topic.cmp(&b.topic))
  });

  scored
}

/// Apply Phase B's confidence rule. Returns the chosen topic (if any)
/// and the top-three contributing-edge breakdown when it does.
fn pick_phase_b_winner(
  candidate_scores: &[CandidateScore],
  graph: &ResolutionGraph,
  pr_result: &BTreeMap<topic::Topic, f32>,
) -> (Option<topic::Topic>, Vec<EdgeContribution>) {
  let Some(top) = candidate_scores.first() else {
    return (None, Vec::new());
  };
  let runner_up_score = candidate_scores
    .get(1)
    .map(|c| c.pr_score)
    .unwrap_or(0.0);

  if !passes_threshold(top.pr_score, runner_up_score) {
    return (None, Vec::new());
  }

  let edges = top_contributing_edges(graph, pr_result, top.topic);
  (Some(top.topic), edges)
}

/// `score_top / (score_top + score_runner_up) >= 0.65`, with degenerate
/// cases (zero or non-finite scores) collapsing to "no resolution".
fn passes_threshold(score_top: f32, score_runner_up: f32) -> bool {
  if !(score_top.is_finite() && score_top > 0.0) {
    return false;
  }
  let total = score_top + score_runner_up;
  if !(total.is_finite() && total > 0.0) {
    return false;
  }
  let ratio = score_top / total;
  ratio.is_finite() && ratio >= CONFIDENCE_THRESHOLD
}

/// Walk the graph to find every predecessor of `chosen` and rank them
/// by their steady-state contribution
/// `pr_result[predecessor] * weight(predecessor → chosen) /
/// total_outgoing_weight(predecessor)`. Sorted descending, capped at
/// `MAX_TOP_EDGES`.
fn top_contributing_edges(
  graph: &ResolutionGraph,
  pr_result: &BTreeMap<topic::Topic, f32>,
  chosen: topic::Topic,
) -> Vec<EdgeContribution> {
  let mut contributions: Vec<EdgeContribution> = Vec::new();
  for src in graph.nodes() {
    let edges = graph.out_edges(src);
    let total_out: f32 = edges.iter().map(|e| e.weight).sum();
    if !(total_out.is_finite() && total_out > 0.0) {
      continue;
    }
    for OutEdge {
      dest,
      edge_type,
      weight,
    } in edges
    {
      if *dest != chosen {
        continue;
      }
      let pr = pr_result.get(&src).copied().unwrap_or(0.0);
      let contribution = pr * weight / total_out;
      contributions.push(EdgeContribution {
        predecessor: src,
        edge_type: *edge_type,
        weighted_contribution: contribution,
      });
    }
  }

  contributions.sort_by(|a, b| {
    b.weighted_contribution
      .partial_cmp(&a.weighted_contribution)
      .unwrap_or(std::cmp::Ordering::Equal)
      .then_with(|| a.predecessor.cmp(&b.predecessor))
      .then_with(|| a.edge_type.cmp(&b.edge_type))
  });
  contributions.truncate(MAX_TOP_EDGES);

  // Drop predecessors with zero contribution to keep the trace's
  // explanation focused. If everything is zero — the pathological
  // case where the candidate cleared the threshold without any
  // predecessor mass at all — keep one entry so the trace is still
  // debuggable.
  let mut filtered: Vec<EdgeContribution> = contributions
    .iter()
    .filter(|c| c.weighted_contribution > 0.0)
    .cloned()
    .collect();
  if filtered.is_empty()
    && let Some(first) = contributions.into_iter().next()
  {
    filtered.push(first);
  }
  filtered
}

/// Look up the `kind` and `referenced_name` snapshot fields the comment
/// parser stamps next to `referenced_topic`. Phase-B / C winners must
/// be indistinguishable from Phase-A winners downstream, so the
/// snapshot fields are rewritten alongside `referenced_topic`.
fn lookup_kind_and_name(
  topic: topic::Topic,
  audit_data: &domain::AuditData,
) -> (Option<domain::NamedTopicKind>, Option<String>) {
  match audit_data.topic_metadata.get(&topic) {
    Some(domain::TopicMetadata::NamedTopic { kind, name, .. }) => {
      (Some(kind.clone()), Some(name.clone()))
    }
    _ => (None, None),
  }
}

// ---------------------------------------------------------------------
// Pass 3 — mutation, mentions_index merge, trace persistence
// ---------------------------------------------------------------------

/// For each plan, mutate the comment's `Vec<CommentNode>` to apply any
/// new resolutions, then merge newly-resolved mentions into the
/// audit's `mentions_index` and the `mentioned_topics` field on the
/// comment's metadata. Finally, persist all traces.
fn apply_pass(
  audit_data: &mut domain::AuditData,
  plans: &[CommentPlan],
  resolutions: &ResolutionMap,
  traces: TraceMap,
) {
  for plan in plans {
    let new_mentions =
      mutate_comment_tree(audit_data, plan.comment_topic, resolutions);

    if !new_mentions.is_empty() {
      merge_new_mentions(audit_data, plan.comment_topic, &new_mentions);
    }
  }

  for (key, trace) in traces {
    audit_data.resolution_traces.insert(key, trace);
  }
}

/// Walk the comment's `Vec<CommentNode>` once with `&mut`, applying
/// each resolution by `(comment_topic, occurrence)` key. Returns the
/// set of topics newly resolved by this pass — used by the caller to
/// merge into `mentions_index` and `mentioned_topics`.
///
/// Returns an empty set if the comment's node tree was missing or if
/// none of the comment's ambiguous references had a winner.
fn mutate_comment_tree(
  audit_data: &mut domain::AuditData,
  comment_topic: topic::Topic,
  resolutions: &ResolutionMap,
) -> BTreeSet<topic::Topic> {
  let mut new_mentions: BTreeSet<topic::Topic> = BTreeSet::new();

  let Some(Node::Comment(nodes)) = audit_data.nodes.get_mut(&comment_topic)
  else {
    return new_mentions;
  };

  let mut counter: u32 = 0;
  for node in nodes.iter_mut() {
    apply_to_node(
      node,
      comment_topic,
      &mut counter,
      resolutions,
      &mut new_mentions,
    );
  }

  new_mentions
}

fn apply_to_node(
  node: &mut CommentNode,
  comment_topic: topic::Topic,
  counter: &mut u32,
  resolutions: &ResolutionMap,
  new_mentions: &mut BTreeSet<topic::Topic>,
) {
  match node {
    CommentNode::CodeIdentifier {
      referenced_topic,
      kind,
      referenced_name,
      ..
    } => {
      let occurrence = *counter;
      *counter = counter.wrapping_add(1);

      // Defensive: never overwrite a Phase-A resolution. Pass 1 only
      // recorded `None`-resolved CodeIdentifiers as ambiguous, so a
      // matching key in `resolutions` against an already-resolved
      // node would be a bug. Bail rather than clobber.
      if referenced_topic.is_some() {
        return;
      }

      if let Some(applied) = resolutions.get(&(comment_topic, occurrence)) {
        *referenced_topic = Some(applied.chosen_topic);
        *kind = applied.kind.clone();
        *referenced_name = applied.referenced_name.clone();
        new_mentions.insert(applied.chosen_topic);
      }
    }
    CommentNode::InlineCode { children, .. } => {
      for c in children {
        apply_to_node(c, comment_topic, counter, resolutions, new_mentions);
      }
    }
    CommentNode::Text { .. }
    | CommentNode::CodeKeyword { .. }
    | CommentNode::CodeOperator { .. }
    | CommentNode::CodeText { .. }
    | CommentNode::Emphasis { .. }
    | CommentNode::Strong { .. }
    | CommentNode::Link { .. } => {}
  }
}

/// Additively merge `new_mentions` into the audit's `mentions_index`
/// and the comment's `mentioned_topics` field. The build plan's task 5
/// pins the contract: never remove existing entries, sort + dedup the
/// updated lists so subsequent reads are deterministic.
fn merge_new_mentions(
  audit_data: &mut domain::AuditData,
  comment_topic: topic::Topic,
  new_mentions: &BTreeSet<topic::Topic>,
) {
  // mentions_index: append `comment_topic` under each newly-mentioned
  // topic if it isn't already there. The list-per-topic semantics are
  // preserved (insertion-order-ish, with a contains check rather than
  // sort) because downstream consumers walk these lists in their
  // current order.
  for mention in new_mentions {
    let entry = audit_data.mentions_index.entry(*mention).or_default();
    if !entry.contains(&comment_topic) {
      entry.push(comment_topic);
    }
  }

  // mentioned_topics on the CommentTopic metadata: union with the
  // existing entries, sort + dedup so two runs produce byte-identical
  // metadata.
  if let Some(domain::TopicMetadata::CommentTopic {
    mentioned_topics, ..
  }) = audit_data.topic_metadata.get_mut(&comment_topic)
  {
    for mention in new_mentions {
      mentioned_topics.push(*mention);
    }
    mentioned_topics.sort_unstable();
    mentioned_topics.dedup();
  }
}

#[cfg(test)]
#[path = "dev_doc_resolution_pass_tests.rs"]
mod tests;
