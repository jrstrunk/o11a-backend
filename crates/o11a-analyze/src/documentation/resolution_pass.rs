//! Phases B + C + D + E of the resolution pipeline for documentation
//! files.
//!
//! Runs after the doc parser stamps Phase-A resolutions on every
//! `CodeIdentifier` (qualified-name and unique-simple-name lookups via
//! `code_refs::find_declaration_by_name`). For every section in the
//! parsed doc tree:
//!
//! 1. **Phase B** — gather seeds from Phase-A resolutions in that
//!    section and its ancestors (LCA-distance-weighted via `2^(-d)`),
//!    run personalized PageRank against the audit's resolution graph,
//!    pick a winner per ambiguous reference when the confidence
//!    threshold clears.
//! 2. **Phase C** — for any pair of still-ambiguous references in the
//!    section whose immediate-enclosing-scope sets intersect at exactly
//!    one function/modifier/struct/event/error, pin both via the
//!    co-location rule (see `o11a_core::resolution_graph::coloc`).
//! 3. **Phase D** — re-iterate: each new resolution becomes a Phase-A
//!    seed for the next iteration, which may unlock further refs. Cap
//!    at `MAX_ITERATIONS` rounds.
//! 4. **Phase E** — anchor-by-name fallback for refs the prior phases
//!    could not pin to one topic. Their full candidate list is written
//!    into `CodeIdentifier::referenced_topic_candidates` so downstream
//!    consumers (`mechanical_semantic_links`) can union each candidate's
//!    containing contract into the section's contract anchor set.
//!    `referenced_topic` stays `None` — the resolver does not contribute
//!    a specific declaration to the section, only contract anchors.
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
//! asc), Phase C iterates pairs in deterministic input order, Phase D's
//! iteration cap is fixed, and PR is delegated to the engine in
//! `o11a-core` whose own contract pins floating-point summation order.

use std::collections::BTreeMap;

use o11a_core::documentation::ast::DocumentationNode;
use o11a_core::domain;
use o11a_core::domain::topic;
use o11a_core::resolution_graph::{
  CandidateScore, CoLocInput, EdgeContribution, OutEdge, ResolutionGraph,
  ResolutionPhase, ResolutionRefId, ResolutionTrace, co_locate,
  personalized_pagerank,
};

/// Confidence threshold from the spec's "Confidence threshold and
/// fallback" section. A candidate wins when
/// `score_top / (score_top + score_runner_up) >= THRESHOLD`. Below
/// this, Phase B falls through to Phase C; if Phase C also abstains,
/// Phase E records the full candidate list as the anchor-by-name
/// fallback and `referenced_topic` stays `None`.
const CONFIDENCE_THRESHOLD: f32 = 0.65;

/// Multiplier applied to a parameter candidate's PR score when its
/// enclosing function/modifier is in the section's seed set. Encodes
/// the policy "when a function is referenced, its parameters should
/// rank above a state variable with the same name; when the function
/// isn't referenced, its parameters should not". Without this, a
/// section that mentions `deployCampaign` would have
/// `deployCampaign.rewardPPQ` (the parameter) and
/// `NudgeCampaign.rewardPPQ` (the state variable) score similarly via
/// raw PR — the resolver couldn't pick reliably.
///
/// Set to 2.0: with the spec's `0.65` threshold a candidate has to
/// beat its runner-up by roughly 2:1. A symmetric raw-PR pair plus
/// this boost lifts the param's relative share to `2/(2+1)=0.667`,
/// just past the threshold. Lower values (e.g. 1.5 → 0.6) leave both
/// candidates below threshold and drop the resolution to Phase E.
const FUNCTION_PARAM_BOOST: f32 = 2.0;

/// Maximum depth contribution for ancestor-section seeding. Beyond this
/// the `2^(-d)` weight is below `1/64` and the seed contributes
/// negligibly to PR — capping keeps seed vectors bounded on deeply
/// nested doc trees.
const MAX_SEED_DEPTH: u32 = 6;

/// Spec's "top three contributing edges" cap for the resolution trace.
const MAX_TOP_EDGES: usize = 3;

/// Phase D iteration cap. The spec sets `4` as a pathological-edge-case
/// guard; most ambiguity converges in 1-2 iterations.
const MAX_ITERATIONS: u32 = 4;

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
  // topic-ID ascending for the determinism contract).
  for info in &mut sections {
    info.direct_phase_a_topics.sort();
    info.direct_phase_a_topics.dedup();
  }

  // Pass 2: per-iteration B + C scoring (Phase D loop). Produces a flat
  // list of resolutions to apply and a trace map keyed by ref id.
  let mut resolutions: BTreeMap<i32, AppliedResolution> = BTreeMap::new();
  let mut traces: BTreeMap<ResolutionRefId, ResolutionTrace> = BTreeMap::new();

  for iteration in 1..=MAX_ITERATIONS {
    let new_count = run_iteration(
      &mut sections,
      audit_data,
      graph,
      iteration,
      &mut resolutions,
      &mut traces,
    );
    if new_count == 0 {
      break;
    }
  }

  // Phase E — anchor-by-name fallback. Refs still in `ambiguous_refs`
  // after Phase D record the full candidate list onto the AST node so
  // `mechanical_semantic_links` can union each candidate's containing
  // contract into the section's anchor set. `referenced_topic` stays
  // `None`: the resolver does not contribute a specific declaration,
  // only contracts.
  run_phase_e(&sections, audit_data, &mut resolutions, &mut traces);

  // Pass 3: mutation. Walk the tree once with `&mut`, applying each
  // resolved entry's chosen topic (Phases B / C) or candidate list
  // (Phase E) to the matching `CodeIdentifier`.
  if !resolutions.is_empty() {
    apply_resolutions(doc_root, &resolutions);
  }

  traces.into_iter().collect()
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
  /// topic count once for seeding purposes. Phase D appends new
  /// resolutions here at the end of each iteration so the next
  /// iteration's seed vector reflects them.
  direct_phase_a_topics: Vec<topic::Topic>,
  /// Ambiguous `CodeIdentifier` nodes (Phase A returned `None`) that
  /// live directly under this section, in document order. As Phase D
  /// iterates, resolved refs are removed so the next iteration scores
  /// only what's still unresolved.
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
    DocumentationNode::Root {
      node_id, children, ..
    }
    | DocumentationNode::Section {
      node_id, children, ..
    } => {
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

    DocumentationNode::Heading {
      children, section, ..
    } => {
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
// Pass 2 — scoring (Phases B + C inside Phase D loop)
// ---------------------------------------------------------------------

/// One outcome from the resolution pipeline. Carried out of Pass 2 (or
/// Phase E) and applied to the AST tree in Pass 3. Phases B / C produce
/// `Resolved` (a chosen topic plus the snapshot fields the parser
/// stamps next to a Phase-A winner). Phase E produces `Candidates` (a
/// list of all candidates, with `referenced_topic` left `None`).
#[derive(Debug)]
enum AppliedResolution {
  Resolved {
    chosen_topic: topic::Topic,
    kind: Option<domain::NamedTopicKind>,
    referenced_name: Option<String>,
  },
  Candidates(Vec<topic::Topic>),
}

/// One iteration of Phase D: run Phase B per section, then Phase C on
/// any references Phase B left ambiguous in this round. Mutates
/// `sections` in place (resolved refs migrate from
/// `ambiguous_refs` to `direct_phase_a_topics`) and writes one trace
/// per attempt into `traces` (overwriting prior-iteration traces for
/// the same ref). Returns the count of refs newly resolved this
/// iteration; Phase D's outer loop exits early when this hits zero.
fn run_iteration(
  sections: &mut [SectionInfo],
  audit_data: &domain::AuditData,
  graph: &ResolutionGraph,
  iteration: u32,
  resolutions: &mut BTreeMap<i32, AppliedResolution>,
  traces: &mut BTreeMap<ResolutionRefId, ResolutionTrace>,
) -> usize {
  let mut newly_resolved: usize = 0;

  for section_idx in 0..sections.len() {
    if sections[section_idx].ambiguous_refs.is_empty() {
      continue;
    }

    // Build the seed vector once per iteration. Both Phase B and Phase
    // C use it: B for the PR run and the parameter-boost lookup, C for
    // the same parameter-boost on its post-pinning rank_candidates
    // call. Phase B may append resolutions to `direct_phase_a_topics`
    // before Phase C runs, but C deliberately uses the *pre-mutation*
    // seeds — those are the seeds that produced this iteration's
    // `pr_result`, and Phase C reuses that result rather than re-running
    // PR.
    let seeds = build_seed_vector(section_idx, sections);

    // Phase B — PR-driven scoring of every ambiguous ref in this
    // section. Newly-resolved refs are removed from `ambiguous_refs`;
    // unresolved refs stay in for Phase C.
    let pr_result = run_phase_b(
      sections,
      section_idx,
      audit_data,
      graph,
      iteration,
      &seeds,
      resolutions,
      traces,
      &mut newly_resolved,
    );

    if sections[section_idx].ambiguous_refs.is_empty() {
      // Phase B resolved everything in this section — no Phase C work.
      continue;
    }

    // Phase C — co-location pinning of remaining ambiguities. Each pair
    // whose declared scopes intersect at exactly one function/modifier/
    // struct/event/error pins both refs to that scope's declaration.
    run_phase_c(
      sections,
      section_idx,
      audit_data,
      graph,
      &seeds,
      iteration,
      &pr_result,
      resolutions,
      traces,
      &mut newly_resolved,
    );
  }

  newly_resolved
}

/// Runs Phase B for one section: builds the seed vector, runs PR, and
/// for each ambiguous ref scores candidates and applies the threshold
/// rule. Resolved refs migrate to `direct_phase_a_topics`. Returns the
/// PR result so Phase C can reuse it for trace's candidate scores
/// without re-running the engine.
#[allow(clippy::too_many_arguments)]
fn run_phase_b(
  sections: &mut [SectionInfo],
  section_idx: usize,
  audit_data: &domain::AuditData,
  graph: &ResolutionGraph,
  iteration: u32,
  seeds: &BTreeMap<topic::Topic, f32>,
  resolutions: &mut BTreeMap<i32, AppliedResolution>,
  traces: &mut BTreeMap<ResolutionRefId, ResolutionTrace>,
  newly_resolved: &mut usize,
) -> BTreeMap<topic::Topic, f32> {
  // PR with an empty seed vector returns all-zero per the engine's
  // contract. No candidate could clear the threshold, but we still
  // emit Unresolved traces (operators expect to see the attempt).
  let pr_result = if seeds.is_empty() {
    BTreeMap::new()
  } else {
    personalized_pagerank(graph, seeds)
  };

  let section = &mut sections[section_idx];
  let section_topic = section.topic;

  // Drain each ambiguous ref through Phase B; survivors (Unresolved
  // by B but possibly resolvable by C) go back into the section.
  let mut survivors: Vec<AmbiguousRef> = Vec::new();
  let mut resolved_here: u32 = 0;
  for ambiguous in std::mem::take(&mut section.ambiguous_refs) {
    let trace_key = ResolutionRefId::DocumentationNode(ambiguous.node_id);

    let candidates = audit_data
      .name_index
      .candidates_by_simple_name(&ambiguous.identifier);
    let candidate_scores =
      rank_candidates(candidates, audit_data, &pr_result, seeds);
    let (chosen, edges) =
      pick_phase_b_winner(&candidate_scores, graph, &pr_result);

    if let Some(chosen_topic) = chosen {
      let (kind, referenced_name) =
        lookup_kind_and_name(chosen_topic, audit_data);
      resolutions.insert(
        ambiguous.node_id,
        AppliedResolution::Resolved {
          chosen_topic,
          kind,
          referenced_name,
        },
      );
      section.direct_phase_a_topics.push(chosen_topic);
      resolved_here += 1;
      traces.insert(
        trace_key.clone(),
        ResolutionTrace {
          reference_id: trace_key,
          identifier: ambiguous.identifier.clone(),
          section_topic,
          phase_resolved: ResolutionPhase::PhaseB,
          iteration,
          chosen_topic: Some(chosen_topic),
          candidate_scores,
          top_contributing_edges: edges,
        },
      );
    } else {
      // Tentatively record an Unresolved trace; Phase C may overwrite
      // it later in this same iteration if co-location pins the ref.
      traces.insert(
        trace_key.clone(),
        ResolutionTrace {
          reference_id: trace_key,
          identifier: ambiguous.identifier.clone(),
          section_topic,
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

  // Restore survivors so Phase C can attempt them.
  section.ambiguous_refs = survivors;
  // Sort + dedup only if Phase B for THIS section actually appended;
  // checking the global `newly_resolved` would also fire (idempotently)
  // for sections that resolved nothing themselves.
  if resolved_here > 0 {
    section.direct_phase_a_topics.sort();
    section.direct_phase_a_topics.dedup();
    *newly_resolved += resolved_here as usize;
  }

  pr_result
}

/// Runs Phase C for one section: builds CoLocInput entries from the
/// section's still-ambiguous refs, runs the shared `co_locate`
/// algorithm, and applies any pinnings it produces. Each pinned ref's
/// trace is rewritten to `PhaseC`, with the `candidate_scores` and
/// `top_contributing_edges` reused from the iteration's PR run so
/// operators see the PR ranking alongside the co-location decision.
#[allow(clippy::too_many_arguments)]
fn run_phase_c(
  sections: &mut [SectionInfo],
  section_idx: usize,
  audit_data: &domain::AuditData,
  graph: &ResolutionGraph,
  seeds: &BTreeMap<topic::Topic, f32>,
  iteration: u32,
  pr_result: &BTreeMap<topic::Topic, f32>,
  resolutions: &mut BTreeMap<i32, AppliedResolution>,
  traces: &mut BTreeMap<ResolutionRefId, ResolutionTrace>,
  newly_resolved: &mut usize,
) {
  let section = &mut sections[section_idx];
  let section_topic = section.topic;

  // Build CoLocInput. The ref_id is the node_id (i32) — that keys
  // back to the AmbiguousRef during apply.
  let inputs: Vec<CoLocInput<i32>> = section
    .ambiguous_refs
    .iter()
    .map(|a| CoLocInput {
      ref_id: a.node_id,
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

  // Apply pinnings.
  let pinned_ids: BTreeMap<i32, topic::Topic> = pinnings
    .iter()
    .map(|r| (r.ref_id, r.chosen_topic))
    .collect();

  // Migrate pinned refs out of ambiguous_refs (preserve order of
  // remaining ones).
  let mut survivors: Vec<AmbiguousRef> = Vec::new();
  for ambiguous in std::mem::take(&mut section.ambiguous_refs) {
    if let Some(&chosen_topic) = pinned_ids.get(&ambiguous.node_id) {
      let trace_key = ResolutionRefId::DocumentationNode(ambiguous.node_id);
      let (kind, referenced_name) =
        lookup_kind_and_name(chosen_topic, audit_data);
      resolutions.insert(
        ambiguous.node_id,
        AppliedResolution::Resolved {
          chosen_topic,
          kind,
          referenced_name,
        },
      );
      section.direct_phase_a_topics.push(chosen_topic);
      *newly_resolved += 1;

      // Rewrite the trace from Unresolved → PhaseC. Reuse the PR
      // ranking (with the chosen topic now first) and the edge
      // attribution for the chosen candidate.
      let candidates = audit_data
        .name_index
        .candidates_by_simple_name(&ambiguous.identifier);
      let candidate_scores =
        rank_candidates(candidates, audit_data, pr_result, seeds);
      let edges = top_contributing_edges(graph, pr_result, chosen_topic);

      traces.insert(
        trace_key.clone(),
        ResolutionTrace {
          reference_id: trace_key,
          identifier: ambiguous.identifier.clone(),
          section_topic,
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
  section.ambiguous_refs = survivors;
  section.direct_phase_a_topics.sort();
  section.direct_phase_a_topics.dedup();
}

/// Phase E — anchor-by-name fallback. After Phase D's loop exits,
/// walk every section's surviving ambiguous refs. Each ref with at
/// least one candidate gets its full candidate list pushed into
/// `referenced_topic_candidates` (via `AppliedResolution::Candidates`)
/// and its trace relabeled from `Unresolved` to `PhaseE`. Refs whose
/// candidate list is empty stay `Unresolved` — there is nothing to
/// anchor on.
///
/// Iteration order is `sections` index then ref insertion order, both
/// deterministic. The trace's `iteration` field is preserved from the
/// last Phase B attempt, which already records the round in which the
/// ref was last considered.
fn run_phase_e(
  sections: &[SectionInfo],
  audit_data: &domain::AuditData,
  resolutions: &mut BTreeMap<i32, AppliedResolution>,
  traces: &mut BTreeMap<ResolutionRefId, ResolutionTrace>,
) {
  for section in sections {
    for ambiguous in &section.ambiguous_refs {
      let candidates = audit_data
        .name_index
        .candidates_by_simple_name(&ambiguous.identifier);
      if candidates.is_empty() {
        // Nothing to anchor on; trace stays `Unresolved`.
        continue;
      }

      resolutions.insert(
        ambiguous.node_id,
        AppliedResolution::Candidates(candidates.to_vec()),
      );

      let trace_key = ResolutionRefId::DocumentationNode(ambiguous.node_id);
      if let Some(trace) = traces.get_mut(&trace_key) {
        trace.phase_resolved = ResolutionPhase::PhaseE;
      }
    }
  }
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
///
/// Parameter candidates whose enclosing function/modifier is in
/// `seeds` have their `pr_score` multiplied by [`FUNCTION_PARAM_BOOST`]
/// before sorting and the threshold check. The boosted score replaces
/// the raw PR in the trace's `pr_score` field — operators inspecting
/// the trace see the value the threshold acted on, which matches the
/// resolver's actual decision.
fn rank_candidates(
  candidates: &[topic::Topic],
  audit_data: &domain::AuditData,
  pr_result: &BTreeMap<topic::Topic, f32>,
  seeds: &BTreeMap<topic::Topic, f32>,
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
      let raw_pr = pr_result.get(t).copied().unwrap_or(0.0);
      let pr_score = raw_pr * function_param_boost(*t, audit_data, seeds);
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

/// Multiplier applied to a candidate's raw PR score before sorting and
/// thresholding. Returns [`FUNCTION_PARAM_BOOST`] when the candidate is
/// a parameter (it has a `signature_container`) of a function/modifier
/// that itself appears in the section's seed set; `1.0` otherwise.
///
/// The seed test uses the post-Phase-A topic membership rather than
/// the seed weights — any non-zero seed counts. That matches the
/// natural-language reading "the section mentions this function".
fn function_param_boost(
  candidate: topic::Topic,
  audit_data: &domain::AuditData,
  seeds: &BTreeMap<topic::Topic, f32>,
) -> f32 {
  let Some(domain::TopicMetadata::NamedTopic { scope, .. }) =
    audit_data.topic_metadata.get(&candidate)
  else {
    return 1.0;
  };
  if let domain::Scope::Member {
    member,
    signature_container: Some(_),
    ..
  } = scope
    && seeds.contains_key(member)
  {
    FUNCTION_PARAM_BOOST
  } else {
    1.0
  }
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
  let runner_up_score =
    candidate_scores.get(1).map(|c| c.pr_score).unwrap_or(0.0);

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
/// here means a Phase B / C winner ends up indistinguishable from a
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

/// Walk the doc tree once with `&mut`, applying each ambiguous
/// `CodeIdentifier`'s outcome from the resolution pipeline:
/// - `AppliedResolution::Resolved` writes `referenced_topic` plus the
///   `kind` / `referenced_name` snapshots the parser stamps next to it.
/// - `AppliedResolution::Candidates` writes `referenced_topic_candidates`
///   (Phase E fallback). `referenced_topic` stays `None`.
///
/// Phase B / C / E never overwrite Phase A: the lookup is keyed on
/// `node_id`, and the resolver only enters entries for references
/// whose Phase-A `referenced_topic` was `None`.
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
      referenced_topic_candidates,
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
      match resolutions.get(node_id) {
        Some(AppliedResolution::Resolved {
          chosen_topic,
          kind: applied_kind,
          referenced_name: applied_name,
        }) => {
          *referenced_topic = Some(*chosen_topic);
          *kind = applied_kind.clone();
          *referenced_name = applied_name.clone();
          // Clear any stale Phase E candidates — the contract is
          // "candidates non-empty IFF referenced_topic is None".
          // Matters when the resolver re-runs against an audit whose
          // graph has changed enough that a previously-Phase-E ref
          // now resolves via B/C.
          referenced_topic_candidates.clear();
        }
        Some(AppliedResolution::Candidates(candidates)) => {
          *referenced_topic_candidates = candidates.clone();
        }
        None => {}
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

    DocumentationNode::Heading {
      children, section, ..
    } => {
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
