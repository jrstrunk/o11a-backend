//! Phase B of the resolution pipeline for documentation files.
//!
//! Runs after the doc parser stamps Phase-A resolutions on every
//! `CodeIdentifier` (qualified-name and unique-simple-name lookups via
//! `code_refs::find_declaration_by_name`). For every section in the
//! parsed doc tree, gathers seeds from Phase-A resolutions in that
//! section and its ancestors (LCA-distance-weighted via `2^(-d)`),
//! runs personalized PageRank against the audit's resolution graph,
//! and rewrites ambiguous `CodeIdentifier::referenced_topic` to the
//! winning candidate when it clears the confidence threshold.
//!
//! The pass is a pure read-then-mutate over the parsed `DocumentationAST`
//! and a read of `AuditData`. The graph itself is consulted only for PR
//! scoring; nothing in the pass mutates it. Downstream consumers
//! (`mechanical_semantic_links`, `enumerate_section_code_references`,
//! the doc analyzer's mention-collection step) read `referenced_topic`
//! and pick up the improved resolutions transparently.
//!
//! Determinism contract (mirrors the spec's "Determinism contract"):
//! all hot collections are `BTreeMap`s, candidate ordering is the
//! deterministic tie-break (PR desc → qualified-name asc → topic-ID
//! asc), and PR is delegated to the engine in `o11a-core` whose own
//! contract pins floating-point summation order.

use std::collections::BTreeMap;

use o11a_core::documentation::ast::DocumentationNode;
use o11a_core::domain;
use o11a_core::domain::topic;
use o11a_core::resolution_graph::{
  CandidateScore, EdgeContribution, OutEdge, ResolutionGraph, ResolutionPhase,
  ResolutionRefId, ResolutionTrace, personalized_pagerank,
};

/// Confidence threshold from the spec's "Confidence threshold and
/// fallback" section. A candidate wins when
/// `score_top / (score_top + score_runner_up) >= THRESHOLD`. Below
/// this, the resolver leaves `referenced_topic = None` and Phase 10's
/// anchor-by-name fallback (out of scope here) takes over.
const CONFIDENCE_THRESHOLD: f32 = 0.65;

/// Maximum depth contribution for ancestor-section seeding. Beyond this
/// the `2^(-d)` weight is below `1/64` and the seed contributes
/// negligibly to PR — capping keeps seed vectors bounded on deeply
/// nested doc trees.
const MAX_SEED_DEPTH: u32 = 6;

/// Spec's "top three contributing edges" cap for the resolution trace.
const MAX_TOP_EDGES: usize = 3;

/// Mutates ambiguous `CodeIdentifier::referenced_topic` entries in
/// `doc_root` in place. Returns one trace per ambiguous reference the
/// pass attempted, regardless of whether a candidate was picked. The
/// caller merges the traces into `AuditData::resolution_traces`.
///
/// `doc_root` is expected to be the `DocumentationNode::Root` produced
/// by the parser (or any node — the walker tolerates non-Root inputs
/// for fixture testing). When the audit has no resolution graph yet
/// (i.e. Phase 4 didn't run), the pass is a no-op and returns no
/// traces.
pub fn resolve_doc_tree(
  doc_root: &mut DocumentationNode,
  audit_data: &domain::AuditData,
) -> Vec<(ResolutionRefId, ResolutionTrace)> {
  let Some(graph) = audit_data.resolution_graph.as_ref() else {
    return Vec::new();
  };

  // Pass 1: read-only walk to enumerate sections and the references
  // each one owns. Sections form a tree; we materialize it as a flat
  // `Vec<SectionInfo>` with `parent_index` pointers because the seed
  // builder later walks the chain by repeated index lookup.
  let mut sections: Vec<SectionInfo> = Vec::new();
  collect_sections(doc_root, None, &mut sections);

  // Sort + dedup each section's direct Phase-A topics now (Pass 1
  // appends in document order; the seed-iteration order must be
  // topic-ID ascending for the determinism contract). Done here, in
  // O(n log n) per section, so Pass 2 reads a stable view without
  // having to clone.
  for info in &mut sections {
    info.direct_phase_a_topics.sort();
    info.direct_phase_a_topics.dedup();
  }

  // Pass 2: per-section PR + scoring. Produces a flat list of
  // resolutions to apply and traces to persist.
  let (resolutions, traces) = compute_resolutions(&sections, audit_data, graph);

  // Pass 3: mutation. Walk the tree once with `&mut`, set
  // `referenced_topic` (and the `kind` / `referenced_name` snapshots
  // the parser fills in alongside it) on every winning reference.
  if !resolutions.is_empty() {
    apply_resolutions(doc_root, &resolutions);
  }

  traces
}

// ---------------------------------------------------------------------
// Pass 1 — section enumeration and per-section reference collection
// ---------------------------------------------------------------------

/// One enumerated doc-section (a `Root` or `Section` node). The pass
/// treats `Root` as a virtual outermost section so that doc files
/// without any heading still get scored against their own top-level
/// content.
#[derive(Debug)]
struct SectionInfo {
  /// `Topic::Documentation(node_id)` — the section's identifier in the
  /// audit's topic space. Used as the `section_topic` field on every
  /// trace produced by the section.
  topic: topic::Topic,
  /// Index into the parent `SectionInfo` in the enclosing `Vec`.
  /// `None` only for the document root. The seed-vector builder
  /// walks this chain to gather ancestor seeds at distance
  /// 1, 2, 3, … up to the depth-6 cap.
  parent_index: Option<usize>,
  /// Phase-A-resolved topics that live *directly* under this section
  /// (i.e., in its subtree but not inside any nested doc-section).
  /// Sorted ascending, deduped — multiple references to the same
  /// topic count once for seeding purposes.
  direct_phase_a_topics: Vec<topic::Topic>,
  /// Ambiguous `CodeIdentifier` nodes (Phase A returned `None`) that
  /// live directly under this section, in document order. Each one
  /// will be scored against the section's PR result.
  ambiguous_refs: Vec<AmbiguousRef>,
}

#[derive(Debug, Clone)]
struct AmbiguousRef {
  /// The `CodeIdentifier`'s `node_id`. Stable within an audit and used
  /// as the trace key + the lookup key during mutation.
  node_id: i32,
  /// The literal token text (e.g. `"transfer"`). Saved here to avoid a
  /// second walk during scoring.
  identifier: String,
}

/// Recursive read-only walker. `current_section` is the index of the
/// nearest enclosing section in `out`. The walker treats `Root` and
/// `Section` identically — both push a new entry into `out` and
/// become the parent of any descendants until another section node
/// is encountered.
///
/// Heading-text references are deliberately attributed to the
/// *enclosing* section, not the heading's own section — that
/// matches the doc analyzer's scope assignment for the same nodes
/// (`solidity::analyzer::process_documentation_node` recurses into
/// `Heading::children` with the outer scope and reserves the inner
/// scope for the boxed `section` child).
fn collect_sections(
  node: &DocumentationNode,
  current_section: Option<usize>,
  out: &mut Vec<SectionInfo>,
) {
  match node {
    DocumentationNode::Root { node_id, children, .. }
    | DocumentationNode::Section { node_id, children, .. } => {
      let my_index = out.len();
      out.push(SectionInfo {
        topic: topic::new_documentation_topic(*node_id),
        parent_index: current_section,
        direct_phase_a_topics: Vec::new(),
        ambiguous_refs: Vec::new(),
      });
      for child in children {
        collect_sections(child, Some(my_index), out);
      }
    }

    DocumentationNode::Heading { children, section, .. } => {
      // The heading itself contributes no section level — its inline
      // text children belong to the *enclosing* section, and the
      // boxed `section` child (if present) becomes its own section
      // one level deeper.
      for child in children {
        collect_sections(child, current_section, out);
      }
      if let Some(sec) = section {
        collect_sections(sec, current_section, out);
      }
    }

    DocumentationNode::CodeIdentifier {
      node_id,
      value,
      referenced_topic,
      ..
    } => {
      let Some(section_idx) = current_section else {
        // Defensive — every CodeIdentifier produced by the parser
        // sits under a Root, so `current_section` is always Some by
        // the time we get here. Skipping rather than panicking keeps
        // a malformed fixture from crashing the pass.
        return;
      };
      let info = &mut out[section_idx];
      match referenced_topic {
        // Append unconditionally; `resolve_doc_tree` sort+dedups
        // each section's topic list once after Pass 1 completes.
        // Skipping the `.contains()` check here turns the per-
        // insertion cost from O(n) into O(1) — relevant when one
        // section repeats the same identifier dozens of times.
        Some(t) => info.direct_phase_a_topics.push(*t),
        None => info.ambiguous_refs.push(AmbiguousRef {
          node_id: *node_id,
          identifier: value.clone(),
        }),
      }
    }

    // Container variants — recurse, no scope change.
    DocumentationNode::Paragraph { children, .. }
    | DocumentationNode::Sentence { children, .. }
    | DocumentationNode::InlineCode { children, .. }
    | DocumentationNode::CodeBlock { children, .. }
    | DocumentationNode::List { children, .. }
    | DocumentationNode::ListItem { children, .. }
    | DocumentationNode::Emphasis { children, .. }
    | DocumentationNode::Strong { children, .. }
    | DocumentationNode::Link { children, .. }
    | DocumentationNode::BlockQuote { children, .. }
    | DocumentationNode::Delete { children, .. }
    | DocumentationNode::Table { children, .. }
    | DocumentationNode::TableRow { children, .. }
    | DocumentationNode::TableCell { children, .. }
    | DocumentationNode::FootnoteDefinition { children, .. }
    | DocumentationNode::LinkReference { children, .. } => {
      for child in children {
        collect_sections(child, current_section, out);
      }
    }

    // Leaf-only variants (no children, no Phase-A topic to harvest).
    _ => {}
  }
}

// ---------------------------------------------------------------------
// Pass 2 — scoring
// ---------------------------------------------------------------------

/// One winning resolution. Carried out of Pass 2 and applied to the
/// tree in Pass 3. The new `kind` and `referenced_name` mirror what
/// the parser would have written if Phase A had succeeded for this
/// reference.
#[derive(Debug)]
struct AppliedResolution {
  chosen_topic: topic::Topic,
  kind: Option<domain::NamedTopicKind>,
  referenced_name: Option<String>,
}

/// Compute every section's resolutions. Returns:
///
/// * `resolutions`: keyed by `node_id`, only winning resolutions.
/// * `traces`: every attempted resolution, including unresolved
///   attempts (so operators see the per-candidate scores via the
///   trace dump even when no candidate cleared the threshold).
fn compute_resolutions(
  sections: &[SectionInfo],
  audit_data: &domain::AuditData,
  graph: &ResolutionGraph,
) -> (
  BTreeMap<i32, AppliedResolution>,
  Vec<(ResolutionRefId, ResolutionTrace)>,
) {
  let mut resolutions: BTreeMap<i32, AppliedResolution> = BTreeMap::new();
  let mut traces: Vec<(ResolutionRefId, ResolutionTrace)> = Vec::new();

  for (idx, section) in sections.iter().enumerate() {
    if section.ambiguous_refs.is_empty() {
      // No work for this section — skip the PR run entirely.
      continue;
    }

    let seeds = build_seed_vector(idx, sections);

    // When the ancestor chain has no Phase-A seeds, PR with an empty
    // seed vector returns all-zero (per the engine's contract). No
    // candidate can clear the threshold against zero PR, so skip the
    // 30 wasted iterations and emit Unresolved traces directly. The
    // candidate scores below come from a fresh empty map, which
    // hands out 0.0 for every candidate via `unwrap_or(0.0)`.
    let pr_result = if seeds.is_empty() {
      BTreeMap::new()
    } else {
      personalized_pagerank(graph, &seeds)
    };

    for ambiguous in &section.ambiguous_refs {
      let trace_key =
        ResolutionRefId::DocumentationNode(ambiguous.node_id);

      let candidates =
        audit_data.name_index.candidates_by_simple_name(&ambiguous.identifier);

      let candidate_scores =
        rank_candidates(candidates, audit_data, &pr_result);

      let (chosen, edges) =
        pick_winner(&candidate_scores, graph, &pr_result);

      let phase_resolved = if chosen.is_some() {
        ResolutionPhase::PhaseB
      } else {
        ResolutionPhase::Unresolved
      };

      if let Some(chosen_topic) = chosen {
        let (kind, referenced_name) = lookup_kind_and_name(
          chosen_topic,
          audit_data,
        );
        resolutions.insert(
          ambiguous.node_id,
          AppliedResolution {
            chosen_topic,
            kind,
            referenced_name,
          },
        );
      }

      traces.push((
        trace_key.clone(),
        ResolutionTrace {
          reference_id: trace_key,
          identifier: ambiguous.identifier.clone(),
          section_topic: section.topic,
          phase_resolved,
          iteration: 1,
          chosen_topic: chosen,
          candidate_scores,
          top_contributing_edges: edges,
        },
      ));
    }
  }

  (resolutions, traces)
}

/// Walks the section's ancestor chain (including itself) and sums
/// `2^(-distance)` weights into a topic-keyed seed map. Distances are
/// capped at `MAX_SEED_DEPTH`; any ancestor farther than that
/// contributes nothing.
fn build_seed_vector(
  section_idx: usize,
  sections: &[SectionInfo],
) -> BTreeMap<topic::Topic, f32> {
  let mut seeds: BTreeMap<topic::Topic, f32> = BTreeMap::new();

  let mut cursor = Some(section_idx);
  let mut distance: u32 = 0;
  while let Some(idx) = cursor {
    if distance > MAX_SEED_DEPTH {
      break;
    }
    let section = &sections[idx];
    let weight = (2.0_f32).powi(-(distance as i32));
    for topic in &section.direct_phase_a_topics {
      *seeds.entry(*topic).or_insert(0.0) += weight;
    }
    cursor = section.parent_index;
    distance = distance.saturating_add(1);
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
      // NamedTopics, but if a future refactor ever leaks something
      // else into the candidate slice, skip it here rather than
      // surface as a confused PR scorelookup downstream.
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

/// Apply the spec's confidence rule. Returns the chosen topic (if any)
/// and the top-three contributing-edge breakdown when it does.
fn pick_winner(
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

/// `score_top / (score_top + score_runner_up) >= 0.65`, with
/// degenerate cases (zero or negative scores) collapsing to "no
/// resolution".
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
/// `MAX_TOP_EDGES`. When the chosen candidate has no predecessors —
/// e.g. it's a graph leaf seeded directly — the result is empty.
///
/// Cost: `O(E)` per resolved reference. Expected to dominate the per-
/// section work only on graphs where the average node has many
/// predecessors *and* a section has many ambiguous refs. Profiling
/// after Phase 8's harness baseline will tell us whether to memoize
/// predecessor lists per PR run.
fn top_contributing_edges(
  graph: &ResolutionGraph,
  pr_result: &BTreeMap<topic::Topic, f32>,
  chosen: topic::Topic,
) -> Vec<EdgeContribution> {
  // Cache per-source total outgoing weights in this scan to avoid an
  // inner re-sum for every match.
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

  // Sort descending by contribution, then break ties on (predecessor,
  // edge_type) ascending — the same lexicographic discipline used
  // elsewhere in the pipeline so a future tie does not flicker.
  contributions.sort_by(|a, b| {
    b.weighted_contribution
      .partial_cmp(&a.weighted_contribution)
      .unwrap_or(std::cmp::Ordering::Equal)
      .then_with(|| a.predecessor.cmp(&b.predecessor))
      .then_with(|| a.edge_type.cmp(&b.edge_type))
  });
  contributions.truncate(MAX_TOP_EDGES);

  // Drop predecessors with zero contribution — surfacing them in the
  // explanation would add noise without information. (Audit-side
  // graphs typically have many sources whose PR is exactly zero
  // because the seed vector did not reach them.) Leave one zero-mass
  // entry behind only if everything is zero; that pathological case
  // is unreachable when the candidate cleared the threshold, but
  // discarding silently makes the trace less debuggable.
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

/// Look up the `kind` and `referenced_name` snapshot the parser
/// stamps next to `referenced_topic`. Mirroring the parser's logic
/// here means a Phase B winner ends up indistinguishable from a
/// Phase A winner downstream — `mechanical_semantic_links` and the
/// other consumers see one shape, not two.
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
// Pass 3 — mutation
// ---------------------------------------------------------------------

/// Walk the doc tree once with `&mut`, replacing each ambiguous
/// `CodeIdentifier`'s `referenced_topic` (and the snapshot fields the
/// parser writes alongside it) with the chosen topic.
///
/// Phase B never overwrites Phase A: the lookup is keyed on
/// `node_id`, and `compute_resolutions` only enters resolutions for
/// references whose Phase-A `referenced_topic` was `None`.
fn apply_resolutions(
  node: &mut DocumentationNode,
  resolutions: &BTreeMap<i32, AppliedResolution>,
) {
  match node {
    DocumentationNode::CodeIdentifier {
      node_id,
      referenced_topic,
      kind,
      referenced_name,
      ..
    } => {
      // Defensive guard: never overwrite a Phase-A resolution. The
      // resolutions map should not contain such entries (Pass 1 only
      // adds None-resolved CodeIdentifiers as ambiguous), but this
      // line is the contract the spec explicitly mandates ("Phase A
      // unresolved → enters the graph pipeline; Phase A resolved →
      // unchanged").
      if referenced_topic.is_some() {
        return;
      }
      if let Some(applied) = resolutions.get(node_id) {
        *referenced_topic = Some(applied.chosen_topic);
        *kind = applied.kind.clone();
        *referenced_name = applied.referenced_name.clone();
      }
    }

    DocumentationNode::Root { children, .. }
    | DocumentationNode::Section { children, .. }
    | DocumentationNode::Paragraph { children, .. }
    | DocumentationNode::Sentence { children, .. }
    | DocumentationNode::InlineCode { children, .. }
    | DocumentationNode::CodeBlock { children, .. }
    | DocumentationNode::List { children, .. }
    | DocumentationNode::ListItem { children, .. }
    | DocumentationNode::Emphasis { children, .. }
    | DocumentationNode::Strong { children, .. }
    | DocumentationNode::Link { children, .. }
    | DocumentationNode::BlockQuote { children, .. }
    | DocumentationNode::Delete { children, .. }
    | DocumentationNode::Table { children, .. }
    | DocumentationNode::TableRow { children, .. }
    | DocumentationNode::TableCell { children, .. }
    | DocumentationNode::FootnoteDefinition { children, .. }
    | DocumentationNode::LinkReference { children, .. } => {
      for child in children {
        apply_resolutions(child, resolutions);
      }
    }

    DocumentationNode::Heading { children, section, .. } => {
      for child in children {
        apply_resolutions(child, resolutions);
      }
      if let Some(sec) = section {
        apply_resolutions(sec, resolutions);
      }
    }

    _ => {}
  }
}

#[cfg(test)]
#[path = "resolution_pass_tests.rs"]
mod tests;
