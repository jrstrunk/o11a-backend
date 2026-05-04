//! Diagnostic dumps of parsed audit-data internals.
//!
//! Exposes a small set of "kind" values that the operator can request via
//! the `dump` CLI subcommand. Each kind serializes a focused slice of
//! [`AuditData`] to a pretty-printed JSON file (one root array or object,
//! valid for any standard JSON formatter / IDE folding) so operators can
//! manually inspect parsed state and spot edge cases without running the
//! full pipeline or hunting through the binary artifact.
//!
//! Adding a new dump kind:
//!   1. Add a variant to [`DumpKind`].
//!   2. Add an arm to [`DumpKind::parse`] (accept both kebab and snake
//!      case for the user input — both forms feel natural on a CLI).
//!   3. Add an arm to [`DumpKind::file_name`].
//!   4. Add the variant to [`DumpKind::all`] so the `all` CLI shorthand
//!      includes it.
//!   5. Add an arm to [`dump_to_file`] that writes the JSON.
//!
//! Everything else is mechanical.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::domain::{
  AuditData, NamedTopicKind, Scope, TopicMetadata, is_common_word, topic,
};
use crate::resolution_graph::{
  EdgeType, ResolutionPhase, ResolutionRefId, ResolutionTrace,
};

/// One kind of audit-data dump. The set is small on purpose — each variant
/// represents a curated diagnostic view, not a raw struct dump.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum DumpKind {
  /// Maps every interface-stub topic to its implementation topic via
  /// `transitive_topic`. Useful for spotting interface methods that
  /// should map to an implementation but don't.
  InterfaceMapping,
  /// Every simple identifier name in the audit, the full set of topic
  /// candidates that share it, and whether the resolver was able to
  /// disambiguate to a single topic. Useful for spotting names that fail
  /// to resolve due to ambiguity.
  NameIndex,
  /// The full personalized-PageRank `ResolutionGraph` — every node that
  /// participates in an edge, with kind and qualified-name annotations,
  /// plus every typed weighted edge. Lets operators inspect the structure
  /// the resolver scores against.
  ResolutionGraph,
  /// One record per ambiguous reference the graph-driven resolver
  /// attempted, with the chosen topic, ranked candidate scores, and the
  /// top contributing edges. Lets operators see *why* the resolver picked
  /// (or could not pick) each topic.
  ResolutionTrace,
}

impl DumpKind {
  /// Parse a CLI argument. Accepts kebab-case (`interface-mapping`),
  /// snake_case (`interface_mapping`), and the special value `all` (which
  /// is handled by [`parse_kinds`] rather than producing a single kind).
  pub fn parse(s: &str) -> Result<Self, String> {
    let normalized = s.trim().to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
      "interface-mapping" => Ok(DumpKind::InterfaceMapping),
      "name-index" => Ok(DumpKind::NameIndex),
      "resolution-graph" => Ok(DumpKind::ResolutionGraph),
      "resolution-trace" => Ok(DumpKind::ResolutionTrace),
      other => Err(format!(
        "unknown dump kind '{}' (expected one of: interface-mapping, name-index, resolution-graph, resolution-trace, all)",
        other
      )),
    }
  }

  pub fn file_name(&self) -> &'static str {
    match self {
      DumpKind::InterfaceMapping => "interface-mapping.json",
      DumpKind::NameIndex => "name-index.json",
      DumpKind::ResolutionGraph => "resolution-graph.json",
      DumpKind::ResolutionTrace => "resolution-trace.json",
    }
  }

  pub fn all() -> Vec<DumpKind> {
    vec![
      DumpKind::InterfaceMapping,
      DumpKind::NameIndex,
      DumpKind::ResolutionGraph,
      DumpKind::ResolutionTrace,
    ]
  }
}

/// Parse a list of CLI dump-kind arguments. `"all"` (anywhere in the list)
/// expands to every kind. Duplicate kinds are deduped while preserving
/// order. Unknown kinds produce an error.
pub fn parse_kinds(args: &[String]) -> Result<Vec<DumpKind>, String> {
  let mut out: Vec<DumpKind> = Vec::new();
  let mut seen = std::collections::HashSet::new();
  for raw in args {
    for piece in raw.split(',') {
      let piece = piece.trim();
      if piece.is_empty() {
        continue;
      }
      if piece.eq_ignore_ascii_case("all") {
        for k in DumpKind::all() {
          if seen.insert(k) {
            out.push(k);
          }
        }
        continue;
      }
      let k = DumpKind::parse(piece)?;
      if seen.insert(k) {
        out.push(k);
      }
    }
  }
  Ok(out)
}

/// Run the dump for `kind` against `audit_data` and write the result as a
/// pretty-printed JSON file to `<output_dir>/<file_name>`. Returns the
/// final file path.
pub fn dump_to_file(
  kind: DumpKind,
  audit_data: &AuditData,
  output_dir: &Path,
) -> std::io::Result<PathBuf> {
  let path = output_dir.join(kind.file_name());
  let json = match kind {
    DumpKind::InterfaceMapping => {
      let records = dump_interface_mapping(audit_data);
      serde_json::to_string_pretty(&records)
    }
    DumpKind::NameIndex => {
      let entries = dump_name_index(audit_data);
      serde_json::to_string_pretty(&entries)
    }
    DumpKind::ResolutionGraph => {
      let dump = dump_resolution_graph(audit_data);
      serde_json::to_string_pretty(&dump)
    }
    DumpKind::ResolutionTrace => {
      let traces = dump_resolution_traces(audit_data);
      serde_json::to_string_pretty(&traces)
    }
  };
  let json = json.map_err(|e| {
    std::io::Error::other(format!(
      "serializing {} dump: {}",
      kind.file_name(),
      e
    ))
  })?;

  let tmp = path.with_extension("json.tmp");
  {
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(json.as_bytes())?;
    f.write_all(b"\n")?;
  }
  std::fs::rename(&tmp, &path)?;
  Ok(path)
}

// ---------------------------------------------------------------------------
// interface-mapping
// ---------------------------------------------------------------------------

/// One row in `interface-mapping.json`: a topic that proxies to another
/// (`transitive_topic` is `Some`) — typically an interface stub pointing
/// at its implementation. Both ends are surfaced with full identifying
/// metadata so a reviewer can spot mappings that look wrong (or, more
/// often, mappings that *should* exist but don't — that absence shows up
/// as an interface stub appearing in `name-index.json` with a non-empty
/// candidate list and missing from this file).
#[derive(Debug, Clone, Serialize)]
struct InterfaceMappingRecord {
  proxy_topic: String,
  proxy_name: String,
  proxy_kind: String,
  proxy_qualified_name: String,
  proxy_scope: String,
  target_topic: String,
  target_name: String,
  target_kind: String,
  target_qualified_name: String,
  target_scope: String,
}

fn dump_interface_mapping(
  audit_data: &AuditData,
) -> Vec<InterfaceMappingRecord> {
  let mut out: Vec<InterfaceMappingRecord> = Vec::new();
  for (proxy_topic, meta) in &audit_data.topic_metadata {
    let Some(target_topic) = meta.transitive_topic() else {
      continue;
    };
    // Only `NamedTopic` ↔ `NamedTopic` mappings are interesting here —
    // these are the interface-stub → implementation links. The AST-level
    // stub graph (used for cross-file reference resolution) also populates
    // `transitive_topic` on unnamed nodes, but those aren't what
    // "interface mapping" means semantically and would otherwise drown
    // out the signal.
    let TopicMetadata::NamedTopic { .. } = meta else {
      continue;
    };
    let target_meta = audit_data.topic_metadata.get(target_topic);
    let Some(TopicMetadata::NamedTopic { .. }) = target_meta else {
      continue;
    };

    out.push(InterfaceMappingRecord {
      proxy_topic: proxy_topic.id().to_string(),
      proxy_name: meta.name().unwrap_or("").to_string(),
      proxy_kind: kind_label(meta),
      proxy_qualified_name: meta.qualified_name(audit_data).unwrap_or_default(),
      proxy_scope: scope_summary(meta.scope(), audit_data),
      target_topic: target_topic.id().to_string(),
      target_name: target_meta.and_then(|m| m.name()).unwrap_or("").to_string(),
      target_kind: target_meta.map(kind_label).unwrap_or_default(),
      target_qualified_name: target_meta
        .and_then(|m| m.qualified_name(audit_data))
        .unwrap_or_default(),
      target_scope: target_meta
        .map(|m| scope_summary(m.scope(), audit_data))
        .unwrap_or_default(),
    });
  }
  out.sort_by(|a, b| {
    (a.proxy_qualified_name.as_str(), a.proxy_topic.as_str())
      .cmp(&(b.proxy_qualified_name.as_str(), b.proxy_topic.as_str()))
  });
  out
}

// ---------------------------------------------------------------------------
// name-index
// ---------------------------------------------------------------------------

/// One entry in `name-index.json`: a simple identifier name and the full
/// list of topic candidates that share it, plus whether the resolver was
/// able to pick a unique answer. The candidates carry enough metadata to
/// see what kind / scope / qualified-name each candidate has, so a
/// reviewer can spot ambiguities that should be resolvable (e.g. exactly
/// one `StateVariable` plus N `LocalVariable` parameters).
#[derive(Debug, Clone, Serialize)]
struct NameIndexEntry {
  name: String,
  /// True when the name is in `is_common_word`'s English-connective
  /// stoplist and was therefore excluded from the simple-name index.
  is_common_word: bool,
  /// True when there are >1 non-transitive candidates AND the resolver
  /// did not pick a unique answer (i.e. lookup returns `None`).
  ambiguous: bool,
  /// The topic the simple-name index points to, if it resolved uniquely.
  /// `None` when the name is ambiguous, common-word filtered, or absent.
  #[serde(skip_serializing_if = "Option::is_none")]
  resolved_topic: Option<String>,
  /// Every NamedTopic with this exact simple name, in deterministic order.
  candidates: Vec<NameCandidate>,
}

#[derive(Debug, Clone, Serialize)]
struct NameCandidate {
  topic: String,
  qualified_name: String,
  kind: String,
  scope: String,
  is_transitive: bool,
  /// When `is_transitive` is true, the topic this candidate proxies to.
  #[serde(skip_serializing_if = "Option::is_none")]
  transitive_target: Option<String>,
}

fn dump_name_index(audit_data: &AuditData) -> Vec<NameIndexEntry> {
  // Group every NamedTopic by simple name. Skip empty names — these are
  // unnamed AST nodes (e.g. constructor parameter lists) that share an
  // empty `name` field; lumping them together here is noise.
  let mut by_name: BTreeMap<String, Vec<topic::Topic>> = BTreeMap::new();
  for (t, meta) in &audit_data.topic_metadata {
    if let TopicMetadata::NamedTopic { name, .. } = meta
      && !name.is_empty()
    {
      by_name.entry(name.clone()).or_default().push(*t);
    }
  }

  let mut out: Vec<NameIndexEntry> = Vec::with_capacity(by_name.len());
  for (name, topics) in by_name {
    let is_common = is_common_word(&name);
    let resolved = audit_data.name_index.get_by_simple_name(&name).copied();
    // Ambiguous: resolver couldn't pick a unique answer despite >1
    // candidates with this simple name. Common-word filtering shows up
    // here too but is flagged separately.
    let ambiguous = resolved.is_none() && topics.len() > 1 && !is_common;

    let mut candidates: Vec<NameCandidate> = topics
      .iter()
      .map(|t| {
        let meta = audit_data.topic_metadata.get(t);
        let qualified_name = meta
          .and_then(|m| m.qualified_name(audit_data))
          .unwrap_or_default();
        let kind = meta.map(kind_label).unwrap_or_default();
        let scope = meta
          .map(|m| scope_summary(m.scope(), audit_data))
          .unwrap_or_default();
        let transitive = meta.and_then(|m| m.transitive_topic());
        NameCandidate {
          topic: t.id().to_string(),
          qualified_name,
          kind,
          scope,
          is_transitive: transitive.is_some(),
          transitive_target: transitive.map(|tt| tt.id().to_string()),
        }
      })
      .collect();
    candidates.sort_by(|a, b| {
      (a.qualified_name.as_str(), a.topic.as_str())
        .cmp(&(b.qualified_name.as_str(), b.topic.as_str()))
    });

    out.push(NameIndexEntry {
      name,
      is_common_word: is_common,
      ambiguous,
      resolved_topic: resolved.map(|t| t.id().to_string()),
      candidates,
    });
  }

  // Order: ambiguous first (most diagnostic), then alphabetical.
  out.sort_by(|a, b| {
    b.ambiguous
      .cmp(&a.ambiguous)
      .then_with(|| a.name.cmp(&b.name))
  });
  out
}

// ---------------------------------------------------------------------------
// resolution-graph
// ---------------------------------------------------------------------------

/// `resolution-graph.json` payload: every topic that participates in an
/// edge, paired with every typed weighted edge. Sorting is stable across
/// runs — nodes by `Topic` order (BTreeSet), edges by
/// `(source, dest, edge_type)` Ord. Topics that never participate in any
/// edge are omitted; they would contribute no signal to a PR run anyway.
#[derive(Debug, Clone, Serialize)]
struct ResolutionGraphDump {
  nodes: Vec<ResolutionGraphNode>,
  edges: Vec<ResolutionGraphEdge>,
}

#[derive(Debug, Clone, Serialize)]
struct ResolutionGraphNode {
  topic: String,
  /// Kind label from `kind_label`, or empty when the topic has no
  /// metadata. Empty is a real signal — a graph node with no metadata
  /// means the extractor referenced a topic the analyzer never recorded.
  kind: String,
  qualified_name: String,
}

#[derive(Debug, Clone, Serialize)]
struct ResolutionGraphEdge {
  source: String,
  dest: String,
  edge_type: EdgeType,
  weight: f32,
}

fn dump_resolution_graph(audit_data: &AuditData) -> ResolutionGraphDump {
  let Some(graph) = audit_data.resolution_graph.as_ref() else {
    return ResolutionGraphDump {
      nodes: Vec::new(),
      edges: Vec::new(),
    };
  };

  // Walk every (source, dest, edge_type, weight) tuple in topic order so
  // we collect both nodes (BTreeSet → sorted on insert) and edges (sorted
  // explicitly below — defensive against any future change to the
  // adjacency-walk order).
  let mut node_set: BTreeSet<topic::Topic> = BTreeSet::new();
  let mut edge_tuples: Vec<(topic::Topic, topic::Topic, EdgeType, f32)> =
    Vec::new();
  for src in graph.nodes() {
    node_set.insert(src);
    for e in graph.out_edges(src) {
      node_set.insert(e.dest);
      edge_tuples.push((src, e.dest, e.edge_type, e.weight));
    }
  }
  edge_tuples.sort_by(|a, b| {
    a.0
      .cmp(&b.0)
      .then_with(|| a.1.cmp(&b.1))
      .then_with(|| a.2.cmp(&b.2))
  });

  let nodes: Vec<ResolutionGraphNode> = node_set
    .into_iter()
    .map(|t| {
      let meta = audit_data.topic_metadata.get(&t);
      ResolutionGraphNode {
        topic: t.id(),
        kind: meta.map(kind_label).unwrap_or_default(),
        qualified_name: meta
          .and_then(|m| m.qualified_name(audit_data))
          .unwrap_or_default(),
      }
    })
    .collect();

  let edges: Vec<ResolutionGraphEdge> = edge_tuples
    .into_iter()
    .map(|(s, d, et, w)| ResolutionGraphEdge {
      source: s.id(),
      dest: d.id(),
      edge_type: et,
      weight: w,
    })
    .collect();

  ResolutionGraphDump { nodes, edges }
}

// ---------------------------------------------------------------------------
// resolution-trace
// ---------------------------------------------------------------------------

/// One row in `resolution-trace.json`: a single ambiguous-reference
/// scoring attempt. Records appear in `ResolutionRefId` order (the
/// BTreeMap key on `AuditData::resolution_traces`), which is itself a
/// stable function of the AST node IDs the parser assigns.
#[derive(Debug, Clone, Serialize)]
struct ResolutionTraceRecord {
  /// Stable string identifier for the reference being resolved. Encodes
  /// the `ResolutionRefId` variant so a reader can route back to the
  /// parser node or the comment occurrence:
  ///   `doc-node:<id>` for documentation references,
  ///   `comment:<topic>:<occurrence>` for dev-doc references.
  reference_node: String,
  /// Topic of the section / NatSpec block whose seed vector produced
  /// this scoring. Useful for grouping all references resolved in the
  /// same context.
  section_or_comment_id: String,
  /// Literal text of the reference (e.g. `"transfer"`). Not in the
  /// minimum schema but included because every operator inspecting a
  /// trace asks "which name?" first.
  identifier: String,
  /// Spec-shaped phase label: `"B"`, `"C"`, `"E"`, or `"unresolved"`.
  /// Mapping is in [`phase_label`] — the internal enum keeps the longer
  /// names; the dump shape stays terse.
  phase_resolved: &'static str,
  /// Phase D iteration that produced the resolution. Always `1` for
  /// Phase B–only resolutions; later iterations reflect cascade resolutions.
  iteration: u32,
  /// The chosen topic, or `null` when no candidate cleared the threshold.
  /// Serialized as `null` rather than omitted so the field shape matches
  /// the spec schema for every record.
  chosen_topic: Option<String>,
  candidate_scores: Vec<TraceCandidateRecord>,
  top_contributing_edges: Vec<TraceEdgeRecord>,
}

#[derive(Debug, Clone, Serialize)]
struct TraceCandidateRecord {
  topic: String,
  /// Empty string when the candidate's metadata did not yield a
  /// qualified name (defensive — every `NamedTopic` should). Kept as a
  /// plain string rather than `Option` so the field always renders.
  qualified_name: String,
  pr_score: f32,
}

#[derive(Debug, Clone, Serialize)]
struct TraceEdgeRecord {
  predecessor: String,
  edge_type: EdgeType,
  weighted_contribution: f32,
}

fn dump_resolution_traces(
  audit_data: &AuditData,
) -> Vec<ResolutionTraceRecord> {
  // BTreeMap iterates in `ResolutionRefId` Ord order, which already
  // satisfies the spec's "Sort by reference node ID" contract.
  audit_data
    .resolution_traces
    .values()
    .map(trace_to_record)
    .collect()
}

fn trace_to_record(trace: &ResolutionTrace) -> ResolutionTraceRecord {
  ResolutionTraceRecord {
    reference_node: format_resolution_ref_id(&trace.reference_id),
    section_or_comment_id: trace.section_topic.id(),
    identifier: trace.identifier.clone(),
    phase_resolved: phase_label(trace.phase_resolved),
    iteration: trace.iteration,
    chosen_topic: trace.chosen_topic.map(|t| t.id()),
    candidate_scores: trace
      .candidate_scores
      .iter()
      .map(|c| TraceCandidateRecord {
        topic: c.topic.id(),
        qualified_name: c.qualified_name.clone().unwrap_or_default(),
        pr_score: c.pr_score,
      })
      .collect(),
    top_contributing_edges: trace
      .top_contributing_edges
      .iter()
      .map(|e| TraceEdgeRecord {
        predecessor: e.predecessor.id(),
        edge_type: e.edge_type,
        weighted_contribution: e.weighted_contribution,
      })
      .collect(),
  }
}

/// Encode a `ResolutionRefId` as a stable string. The two variants need
/// disambiguation in the dump (a doc node ID and a comment topic ID
/// could otherwise collide as bare integers), so each is namespaced.
fn format_resolution_ref_id(id: &ResolutionRefId) -> String {
  match id {
    ResolutionRefId::DocumentationNode(node_id) => {
      format!("doc-node:{}", node_id)
    }
    ResolutionRefId::DevDocComment {
      comment_topic,
      occurrence,
    } => format!("comment:{}:{}", comment_topic.id(), occurrence),
  }
}

/// Map the internal `ResolutionPhase` enum to the spec's terse label.
/// Kept `&'static str` so the serialized form is the literal string with
/// no per-call allocation.
fn phase_label(phase: ResolutionPhase) -> &'static str {
  match phase {
    ResolutionPhase::PhaseB => "B",
    ResolutionPhase::PhaseC => "C",
    ResolutionPhase::PhaseE => "E",
    ResolutionPhase::Unresolved => "unresolved",
  }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn kind_label(meta: &TopicMetadata) -> String {
  match meta {
    TopicMetadata::NamedTopic { kind, .. } => match kind {
      NamedTopicKind::Function(k) => format!("Function({:?})", k),
      NamedTopicKind::Modifier => "Modifier".to_string(),
      NamedTopicKind::Event => "Event".to_string(),
      NamedTopicKind::Error => "Error".to_string(),
      NamedTopicKind::Struct => "Struct".to_string(),
      NamedTopicKind::Enum => "Enum".to_string(),
      NamedTopicKind::EnumMember => "EnumMember".to_string(),
      NamedTopicKind::StateVariable(m) => format!("StateVariable({:?})", m),
      NamedTopicKind::LocalVariable => "LocalVariable".to_string(),
      NamedTopicKind::Contract(k) => format!("Contract({:?})", k),
      NamedTopicKind::Builtin => "Builtin".to_string(),
    },
    TopicMetadata::TitledTopic { kind, .. } => format!("Titled({:?})", kind),
    TopicMetadata::DocumentationTopic { .. } => "Documentation".to_string(),
    other => format!("{:?}", std::mem::discriminant(other)),
  }
}

fn scope_summary(scope: &Scope, audit_data: &AuditData) -> String {
  let name_of = |t: &topic::Topic| -> String {
    audit_data
      .topic_metadata
      .get(t)
      .and_then(|m| m.name())
      .unwrap_or("?")
      .to_string()
  };
  match scope {
    Scope::Global => "global".to_string(),
    Scope::Container { container } => {
      format!("file: {}", container.file_path)
    }
    Scope::Component {
      container,
      component,
    } => format!("in {} (file: {})", name_of(component), container.file_path),
    Scope::Member {
      member,
      component,
      signature_container,
      ..
    } => {
      let sig = if signature_container.is_some() {
        " [signature]"
      } else {
        ""
      };
      format!("in {}::{}{}", name_of(component), name_of(member), sig)
    }
    Scope::ContainingBlock {
      member, component, ..
    } => format!("inside {}::{}", name_of(component), name_of(member)),
  }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
  use super::*;
  use crate::domain::{
    AuditData, NamedTopicKind, NamedTopicVisibility, Scope, TopicMetadata,
    new_audit_data,
  };
  use crate::resolution_graph::{
    self, CandidateScore, EdgeContribution, ResolutionGraph,
  };
  use std::collections::HashSet;

  // -------------------------------------------------------------------------
  // Fixture helpers
  //
  // Each test below builds an in-memory `AuditData` directly rather than
  // going through the analyzer. The dump producers are pure reads of the
  // already-built fields (`resolution_graph`, `resolution_traces`,
  // `topic_metadata`), so a synthetic fixture is sufficient and keeps the
  // tests fast and decoupled from the Solidity pipeline.
  // -------------------------------------------------------------------------

  fn nt(id: i32) -> topic::Topic {
    topic::new_node_topic(&id)
  }

  fn ct(id: i32) -> topic::Topic {
    topic::new_comment_topic(id)
  }

  fn dt(id: i32) -> topic::Topic {
    topic::new_documentation_topic(id)
  }

  fn named(t: topic::Topic, name: &str, kind: NamedTopicKind) -> TopicMetadata {
    TopicMetadata::NamedTopic {
      topic: t,
      scope: Scope::Global,
      kind,
      visibility: NamedTopicVisibility::Public,
      name: name.to_string(),
      is_mutable: false,
      mutations: Vec::new(),
      ancestors: Vec::new(),
      descendants: Vec::new(),
      relatives: Vec::new(),
      transitive_topic: None,
      doc_references: Vec::new(),
    }
  }

  fn empty_audit() -> AuditData {
    new_audit_data("test".to_string(), HashSet::new(), None)
  }

  /// Build a tiny audit with a hand-rolled graph and one trace per
  /// `ResolutionPhase` variant. Enough surface area to exercise both
  /// dump producers and assert their structural invariants without
  /// requiring the full analyzer pipeline.
  fn populated_audit() -> AuditData {
    let mut audit = empty_audit();

    let foo = nt(10);
    let bar = nt(20);
    let baz = nt(30);
    audit
      .topic_metadata
      .insert(foo, named(foo, "foo", NamedTopicKind::LocalVariable));
    audit
      .topic_metadata
      .insert(bar, named(bar, "bar", NamedTopicKind::LocalVariable));
    audit
      .topic_metadata
      .insert(baz, named(baz, "baz", NamedTopicKind::LocalVariable));

    let mut graph = ResolutionGraph::new();
    // Insert in scrambled order so the dump producer's sort is exercised.
    graph.add_edge(bar, baz, EdgeType::References, 0.5);
    graph.add_edge(foo, bar, EdgeType::Calls, 0.7);
    graph.add_edge(foo, baz, EdgeType::References, 0.5);
    graph.finalize();
    audit.resolution_graph = Some(graph);

    audit.resolution_traces.insert(
      ResolutionRefId::DocumentationNode(7),
      ResolutionTrace {
        reference_id: ResolutionRefId::DocumentationNode(7),
        identifier: "foo".to_string(),
        section_topic: dt(1),
        phase_resolved: ResolutionPhase::PhaseB,
        iteration: 1,
        chosen_topic: Some(foo),
        candidate_scores: vec![
          CandidateScore {
            topic: foo,
            qualified_name: Some("Module.foo".to_string()),
            pr_score: 0.75,
          },
          CandidateScore {
            topic: bar,
            qualified_name: Some("Other.foo".to_string()),
            pr_score: 0.20,
          },
        ],
        top_contributing_edges: vec![EdgeContribution {
          predecessor: bar,
          edge_type: EdgeType::Calls,
          weighted_contribution: 0.40,
        }],
      },
    );
    audit.resolution_traces.insert(
      ResolutionRefId::DocumentationNode(11),
      ResolutionTrace {
        reference_id: ResolutionRefId::DocumentationNode(11),
        identifier: "bar".to_string(),
        section_topic: dt(1),
        phase_resolved: ResolutionPhase::Unresolved,
        iteration: 2,
        chosen_topic: None,
        candidate_scores: vec![CandidateScore {
          topic: bar,
          qualified_name: None,
          pr_score: 0.10,
        }],
        top_contributing_edges: Vec::new(),
      },
    );
    audit.resolution_traces.insert(
      ResolutionRefId::DevDocComment {
        comment_topic: ct(-7),
        occurrence: 3,
      },
      ResolutionTrace {
        reference_id: ResolutionRefId::DevDocComment {
          comment_topic: ct(-7),
          occurrence: 3,
        },
        identifier: "baz".to_string(),
        section_topic: ct(-7),
        phase_resolved: ResolutionPhase::PhaseE,
        iteration: 4,
        chosen_topic: None,
        candidate_scores: vec![CandidateScore {
          topic: baz,
          qualified_name: Some("Module.baz".to_string()),
          pr_score: 0.05,
        }],
        top_contributing_edges: Vec::new(),
      },
    );

    audit
  }

  // -------------------------------------------------------------------------
  // DumpKind plumbing
  //
  // The two new variants must round-trip through CLI parsing, file naming,
  // and `all` expansion. Pin each so a future rename or accidental drop
  // surfaces here, not in the operator's shell session.
  // -------------------------------------------------------------------------

  #[test]
  fn parse_accepts_resolution_graph_kind() {
    assert_eq!(
      DumpKind::parse("resolution-graph").unwrap(),
      DumpKind::ResolutionGraph
    );
    assert_eq!(
      DumpKind::parse("resolution_graph").unwrap(),
      DumpKind::ResolutionGraph
    );
    assert_eq!(
      DumpKind::parse("RESOLUTION-GRAPH").unwrap(),
      DumpKind::ResolutionGraph
    );
  }

  #[test]
  fn parse_accepts_resolution_trace_kind() {
    assert_eq!(
      DumpKind::parse("resolution-trace").unwrap(),
      DumpKind::ResolutionTrace
    );
    assert_eq!(
      DumpKind::parse("resolution_trace").unwrap(),
      DumpKind::ResolutionTrace
    );
  }

  #[test]
  fn file_name_per_kind() {
    assert_eq!(
      DumpKind::ResolutionGraph.file_name(),
      "resolution-graph.json"
    );
    assert_eq!(
      DumpKind::ResolutionTrace.file_name(),
      "resolution-trace.json"
    );
  }

  #[test]
  fn all_includes_new_kinds() {
    let all = DumpKind::all();
    assert!(all.contains(&DumpKind::ResolutionGraph));
    assert!(all.contains(&DumpKind::ResolutionTrace));
  }

  #[test]
  fn parse_kinds_with_all_emits_every_variant_once() {
    let kinds = parse_kinds(&["all".to_string()]).unwrap();
    assert_eq!(kinds.len(), DumpKind::all().len());
    assert!(kinds.contains(&DumpKind::ResolutionGraph));
    assert!(kinds.contains(&DumpKind::ResolutionTrace));
  }

  // -------------------------------------------------------------------------
  // resolution-graph: structural invariants
  // -------------------------------------------------------------------------

  #[test]
  fn graph_dump_with_no_built_graph_is_empty() {
    // An audit that hasn't run the graph builder yet: dump must be the
    // empty-shape object, not a panic and not a missing field. This
    // matches the early-return guard in `dump_resolution_graph`.
    let audit = empty_audit();
    let dump = dump_resolution_graph(&audit);
    assert!(dump.nodes.is_empty());
    assert!(dump.edges.is_empty());
  }

  #[test]
  fn graph_dump_collects_every_participating_topic_into_nodes() {
    // Both source and dest topics should appear in `nodes`. With three
    // topics participating in three edges, every one shows up exactly
    // once even though `bar` appears as both source and dest.
    let audit = populated_audit();
    let dump = dump_resolution_graph(&audit);
    let node_topics: Vec<&str> =
      dump.nodes.iter().map(|n| n.topic.as_str()).collect();
    assert_eq!(node_topics, vec!["N10", "N20", "N30"]);
  }

  #[test]
  fn graph_dump_annotates_nodes_with_kind_and_qualified_name() {
    // The dump's node records must surface the `topic_metadata` view —
    // operators rely on the kind + qualified-name to interpret the graph
    // without a separate metadata dump.
    let audit = populated_audit();
    let dump = dump_resolution_graph(&audit);
    let foo = dump.nodes.iter().find(|n| n.topic == "N10").unwrap();
    assert_eq!(foo.kind, "LocalVariable");
    assert_eq!(foo.qualified_name, "foo");
  }

  #[test]
  fn graph_dump_emits_one_record_per_directed_edge() {
    // The graph stores three edges; the dump emits three records (no
    // dedup, no implicit symmetry — undirected edges are two records).
    let audit = populated_audit();
    let dump = dump_resolution_graph(&audit);
    assert_eq!(dump.edges.len(), 3);
    assert!(dump.edges.iter().any(|e| e.source == "N10"
      && e.dest == "N20"
      && e.edge_type == EdgeType::Calls));
    assert!(dump.edges.iter().any(|e| e.source == "N10"
      && e.dest == "N30"
      && e.edge_type == EdgeType::References));
    assert!(dump.edges.iter().any(|e| e.source == "N20"
      && e.dest == "N30"
      && e.edge_type == EdgeType::References));
  }

  #[test]
  fn graph_dump_edges_sorted_by_source_dest_edge_type() {
    // The spec contract: edges sorted by `(source, dest, edge_type)`.
    // Insertion order at fixture time is deliberately scrambled; if the
    // sort regresses, this comparison breaks.
    let audit = populated_audit();
    let dump = dump_resolution_graph(&audit);
    let triples: Vec<(&str, &str, EdgeType)> = dump
      .edges
      .iter()
      .map(|e| (e.source.as_str(), e.dest.as_str(), e.edge_type))
      .collect();
    assert_eq!(
      triples,
      vec![
        ("N10", "N20", EdgeType::Calls),
        ("N10", "N30", EdgeType::References),
        ("N20", "N30", EdgeType::References),
      ]
    );
  }

  #[test]
  fn graph_dump_preserves_edge_weights() {
    // Weights are part of the graph's identity (the determinism contract
    // pins byte-equality of the serialized graph). Pin one edge end-to-
    // end so a future weight-mangling regression surfaces here.
    let audit = populated_audit();
    let dump = dump_resolution_graph(&audit);
    let edge = dump
      .edges
      .iter()
      .find(|e| e.source == "N10" && e.dest == "N20")
      .unwrap();
    assert_eq!(edge.weight, 0.7);
  }

  #[test]
  fn graph_dump_excludes_topics_with_no_edges() {
    // `keccak256` (built-in, populated by `new_audit_data`) has no
    // edges. It must not appear in the node list — including isolated
    // nodes would clutter the dump and waste operator attention.
    let audit = populated_audit();
    let dump = dump_resolution_graph(&audit);
    assert!(!dump.nodes.iter().any(|n| n.topic == "N-8"));
  }

  // -------------------------------------------------------------------------
  // resolution-trace: structural invariants
  // -------------------------------------------------------------------------

  #[test]
  fn trace_dump_empty_when_no_traces_recorded() {
    let audit = empty_audit();
    let dump = dump_resolution_traces(&audit);
    assert!(dump.is_empty());
  }

  #[test]
  fn trace_dump_emits_one_record_per_trace_in_btree_order() {
    // Three traces: doc-node:7, doc-node:11, comment:C-7:3. The
    // BTreeMap key Ord puts both DocumentationNode entries before the
    // DevDocComment entry (variant declaration order), then sorts
    // numerically within DocumentationNode.
    let audit = populated_audit();
    let dump = dump_resolution_traces(&audit);
    let refs: Vec<&str> =
      dump.iter().map(|r| r.reference_node.as_str()).collect();
    assert_eq!(refs, vec!["doc-node:7", "doc-node:11", "comment:C-7:3"]);
  }

  #[test]
  fn trace_dump_phase_label_maps_every_variant() {
    // Phase mapping is the spec's externally-facing rename. Pin every
    // arm — a regression here would silently change the on-disk shape
    // operators script against.
    assert_eq!(phase_label(ResolutionPhase::PhaseB), "B");
    assert_eq!(phase_label(ResolutionPhase::PhaseC), "C");
    assert_eq!(phase_label(ResolutionPhase::PhaseE), "E");
    assert_eq!(phase_label(ResolutionPhase::Unresolved), "unresolved");
  }

  #[test]
  fn trace_dump_resolved_record_has_all_fields_populated() {
    let audit = populated_audit();
    let dump = dump_resolution_traces(&audit);
    let resolved = dump
      .iter()
      .find(|r| r.reference_node == "doc-node:7")
      .unwrap();
    assert_eq!(resolved.identifier, "foo");
    assert_eq!(resolved.section_or_comment_id, "D1");
    assert_eq!(resolved.phase_resolved, "B");
    assert_eq!(resolved.iteration, 1);
    assert_eq!(resolved.chosen_topic.as_deref(), Some("N10"));
    assert_eq!(resolved.candidate_scores.len(), 2);
    assert_eq!(resolved.candidate_scores[0].topic, "N10");
    assert_eq!(resolved.candidate_scores[0].qualified_name, "Module.foo");
    assert_eq!(resolved.candidate_scores[0].pr_score, 0.75);
    assert_eq!(resolved.top_contributing_edges.len(), 1);
    assert_eq!(resolved.top_contributing_edges[0].predecessor, "N20");
    assert_eq!(
      resolved.top_contributing_edges[0].edge_type,
      EdgeType::Calls
    );
    assert_eq!(
      resolved.top_contributing_edges[0].weighted_contribution,
      0.40
    );
  }

  #[test]
  fn trace_dump_unresolved_record_has_null_chosen_topic() {
    let audit = populated_audit();
    let dump = dump_resolution_traces(&audit);
    let unresolved = dump
      .iter()
      .find(|r| r.reference_node == "doc-node:11")
      .unwrap();
    assert_eq!(unresolved.phase_resolved, "unresolved");
    assert!(unresolved.chosen_topic.is_none());
    // Even unresolved records carry the candidate scoreboard — that is
    // the *point* of writing them; otherwise operators couldn't see
    // why the threshold wasn't met.
    assert_eq!(unresolved.candidate_scores.len(), 1);
    // No winner → no contributing-edge attribution.
    assert!(unresolved.top_contributing_edges.is_empty());
  }

  #[test]
  fn trace_dump_dev_doc_record_encodes_comment_topic_and_occurrence() {
    let audit = populated_audit();
    let dump = dump_resolution_traces(&audit);
    let dev = dump
      .iter()
      .find(|r| r.reference_node == "comment:C-7:3")
      .unwrap();
    assert_eq!(dev.section_or_comment_id, "C-7");
    assert_eq!(dev.phase_resolved, "E");
    assert_eq!(dev.iteration, 4);
  }

  #[test]
  fn trace_dump_json_pins_chosen_topic_serialization_shape() {
    // The spec is explicit: `"chosen_topic": "..." | null`. The field
    // must always be present, with `null` for unresolved records and a
    // string for resolved ones — not omitted via skip_serializing_if.
    // A struct-level `is_none()` check passes either way, so we have to
    // pin the JSON output literally.
    let audit = populated_audit();
    let json = serde_json::to_string(&dump_resolution_traces(&audit)).unwrap();
    assert!(
      json.contains(r#""chosen_topic":"N10""#),
      "resolved record must serialize chosen_topic as a topic string; got: {json}"
    );
    assert!(
      json.contains(r#""chosen_topic":null"#),
      "unresolved record must serialize chosen_topic as JSON null; got: {json}"
    );
  }

  #[test]
  fn trace_dump_json_pins_wire_field_names_and_phase_label() {
    // Operators script against the on-disk shape — pin every field
    // name and the phase-label spelling so any silent rename surfaces
    // here, not in their tooling.
    let audit = populated_audit();
    let json = serde_json::to_string(&dump_resolution_traces(&audit)).unwrap();
    for field in [
      r#""reference_node":"#,
      r#""section_or_comment_id":"#,
      r#""identifier":"#,
      r#""phase_resolved":"#,
      r#""iteration":"#,
      r#""chosen_topic":"#,
      r#""candidate_scores":"#,
      r#""top_contributing_edges":"#,
      r#""topic":"#,
      r#""qualified_name":"#,
      r#""pr_score":"#,
      r#""predecessor":"#,
      r#""edge_type":"#,
      r#""weighted_contribution":"#,
    ] {
      assert!(json.contains(field), "missing field {field} in: {json}");
    }
    // Spec-shaped phase labels — the externally-facing rename of the
    // `ResolutionPhase` enum.
    assert!(json.contains(r#""phase_resolved":"B""#));
    assert!(json.contains(r#""phase_resolved":"E""#));
    assert!(json.contains(r#""phase_resolved":"unresolved""#));
  }

  #[test]
  fn graph_dump_json_pins_wire_field_names_and_edge_type_variant() {
    // Same contract as the trace dump: pin field names and at least one
    // EdgeType variant in the on-disk JSON. EdgeType derives Serialize
    // so by default it emits the variant name; if anyone added a
    // `#[serde(rename = ...)]` attr or restructured the enum, the dump's
    // wire shape would silently change. This test catches that.
    let audit = populated_audit();
    let json = serde_json::to_string(&dump_resolution_graph(&audit)).unwrap();
    for field in [
      r#""nodes":"#,
      r#""edges":"#,
      r#""topic":"#,
      r#""kind":"#,
      r#""qualified_name":"#,
      r#""source":"#,
      r#""dest":"#,
      r#""edge_type":"#,
      r#""weight":"#,
    ] {
      assert!(json.contains(field), "missing field {field} in: {json}");
    }
    // Pin EdgeType variants the populated fixture uses.
    assert!(json.contains(r#""edge_type":"Calls""#));
    assert!(json.contains(r#""edge_type":"References""#));
  }

  #[test]
  fn trace_dump_candidate_with_no_qualified_name_renders_empty_string() {
    // The fixture's unresolved trace stores `qualified_name: None` on
    // its candidate. The dump always renders the field; `None` becomes
    // the empty string so the JSON shape is uniform across records.
    let audit = populated_audit();
    let dump = dump_resolution_traces(&audit);
    let unresolved = dump
      .iter()
      .find(|r| r.reference_node == "doc-node:11")
      .unwrap();
    assert_eq!(unresolved.candidate_scores[0].qualified_name, "");
  }

  // -------------------------------------------------------------------------
  // resolution-ref-id formatting
  // -------------------------------------------------------------------------

  #[test]
  fn format_ref_id_doc_node_uses_doc_node_prefix() {
    assert_eq!(
      format_resolution_ref_id(&ResolutionRefId::DocumentationNode(42)),
      "doc-node:42"
    );
    // Negative IDs are legal (parsers may emit them in some scenarios);
    // the format must not lose the sign.
    assert_eq!(
      format_resolution_ref_id(&ResolutionRefId::DocumentationNode(-5)),
      "doc-node:-5"
    );
  }

  #[test]
  fn format_ref_id_dev_doc_uses_topic_id_and_occurrence() {
    let id = ResolutionRefId::DevDocComment {
      comment_topic: ct(-7),
      occurrence: 3,
    };
    assert_eq!(format_resolution_ref_id(&id), "comment:C-7:3");
  }

  // -------------------------------------------------------------------------
  // Determinism contract — byte-identical JSON across runs
  //
  // This is the headline contract for both dump kinds. If anything in the
  // walk order, sort order, or float formatting silently regresses, these
  // tests fail.
  // -------------------------------------------------------------------------

  #[test]
  fn graph_dump_json_is_byte_identical_across_runs() {
    let audit = populated_audit();
    let json_a =
      serde_json::to_string_pretty(&dump_resolution_graph(&audit)).unwrap();
    let json_b =
      serde_json::to_string_pretty(&dump_resolution_graph(&audit)).unwrap();
    assert_eq!(json_a, json_b);
  }

  #[test]
  fn trace_dump_json_is_byte_identical_across_runs() {
    let audit = populated_audit();
    let json_a =
      serde_json::to_string_pretty(&dump_resolution_traces(&audit)).unwrap();
    let json_b =
      serde_json::to_string_pretty(&dump_resolution_traces(&audit)).unwrap();
    assert_eq!(json_a, json_b);
  }

  #[test]
  fn graph_dump_independent_of_edge_insertion_order() {
    // Build two graphs with the same edges in different insertion
    // orders. After `finalize()` both should produce byte-identical
    // dumps. This pins the determinism contract one layer below the
    // dump producer — through the graph's own normalization.
    let mut audit_a = empty_audit();
    let mut g_a = ResolutionGraph::new();
    g_a.add_edge(nt(1), nt(3), EdgeType::Calls, 0.7);
    g_a.add_edge(nt(1), nt(2), EdgeType::References, 0.5);
    g_a.add_edge(nt(2), nt(3), EdgeType::Calls, 0.7);
    g_a.finalize();
    audit_a.resolution_graph = Some(g_a);

    let mut audit_b = empty_audit();
    let mut g_b = ResolutionGraph::new();
    g_b.add_edge(nt(2), nt(3), EdgeType::Calls, 0.7);
    g_b.add_edge(nt(1), nt(2), EdgeType::References, 0.5);
    g_b.add_edge(nt(1), nt(3), EdgeType::Calls, 0.7);
    g_b.finalize();
    audit_b.resolution_graph = Some(g_b);

    let json_a =
      serde_json::to_string_pretty(&dump_resolution_graph(&audit_a)).unwrap();
    let json_b =
      serde_json::to_string_pretty(&dump_resolution_graph(&audit_b)).unwrap();
    assert_eq!(json_a, json_b);
  }

  // -------------------------------------------------------------------------
  // dump_to_file end-to-end
  //
  // Everything above tests the in-memory producers and serialization. The
  // tests below cover the full file-write path: dispatch → JSON →
  // atomic-rename. They write to a per-test temp directory.
  // -------------------------------------------------------------------------

  fn unique_tmp_dir(label: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
      "o11a-audit-dump-test-{}-{}-{}",
      label,
      std::process::id(),
      n
    ));
    std::fs::create_dir_all(&dir).expect("create tmp dir");
    dir
  }

  #[test]
  fn dump_to_file_writes_resolution_graph_with_expected_filename() {
    let audit = populated_audit();
    let dir = unique_tmp_dir("graph");
    let path = dump_to_file(DumpKind::ResolutionGraph, &audit, &dir).unwrap();
    assert_eq!(path, dir.join("resolution-graph.json"));
    let bytes = std::fs::read(&path).unwrap();
    // File ends with a trailing newline (a convention the existing
    // `dump_to_file` writer enforces) so editors don't complain.
    assert_eq!(bytes.last(), Some(&b'\n'));
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    // Schema sanity: top-level object with `nodes` and `edges` arrays.
    assert!(parsed["nodes"].is_array());
    assert!(parsed["edges"].is_array());
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn dump_to_file_writes_resolution_trace_with_expected_filename() {
    let audit = populated_audit();
    let dir = unique_tmp_dir("trace");
    let path = dump_to_file(DumpKind::ResolutionTrace, &audit, &dir).unwrap();
    assert_eq!(path, dir.join("resolution-trace.json"));
    let bytes = std::fs::read(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    // Schema sanity: top-level array with three records.
    assert_eq!(parsed.as_array().map(|a| a.len()), Some(3));
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn dump_to_file_resolution_graph_byte_identical_across_runs() {
    // The headline determinism contract at the file-write layer.
    let audit = populated_audit();
    let dir = unique_tmp_dir("graph-determinism");
    let p1 = dump_to_file(DumpKind::ResolutionGraph, &audit, &dir).unwrap();
    let bytes_a = std::fs::read(&p1).unwrap();
    // Overwrite via a second invocation; atomic rename handles the
    // replacement.
    let p2 = dump_to_file(DumpKind::ResolutionGraph, &audit, &dir).unwrap();
    let bytes_b = std::fs::read(&p2).unwrap();
    assert_eq!(bytes_a, bytes_b);
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn dump_to_file_resolution_trace_byte_identical_across_runs() {
    let audit = populated_audit();
    let dir = unique_tmp_dir("trace-determinism");
    let p1 = dump_to_file(DumpKind::ResolutionTrace, &audit, &dir).unwrap();
    let bytes_a = std::fs::read(&p1).unwrap();
    let p2 = dump_to_file(DumpKind::ResolutionTrace, &audit, &dir).unwrap();
    let bytes_b = std::fs::read(&p2).unwrap();
    assert_eq!(bytes_a, bytes_b);
    let _ = std::fs::remove_dir_all(&dir);
  }

  // -------------------------------------------------------------------------
  // Phase-4 wiring sanity: the dump producers must not panic on the
  // graph the production builder constructs against an empty audit.
  // -------------------------------------------------------------------------

  #[test]
  fn graph_dump_handles_built_but_empty_graph() {
    let mut audit = empty_audit();
    audit.resolution_graph = Some(resolution_graph::build(&audit));
    let dump = dump_resolution_graph(&audit);
    // The empty-audit build registers no edges, so both lists are empty.
    assert!(dump.nodes.is_empty());
    assert!(dump.edges.is_empty());
  }

  // -------------------------------------------------------------------------
  // Additional coverage for complex interactions
  // -------------------------------------------------------------------------

  /// Pin `phase_resolved: "C"` on the wire. The shared `populated_audit`
  /// fixture covers B / E / Unresolved; a dedicated mini-fixture is the
  /// least-invasive way to add C without re-flowing the count assertions
  /// the shared fixture's tests rely on.
  #[test]
  fn trace_dump_json_pins_phase_c_label() {
    let mut audit = empty_audit();
    let topic = nt(99);
    audit.resolution_traces.insert(
      ResolutionRefId::DocumentationNode(1),
      ResolutionTrace {
        reference_id: ResolutionRefId::DocumentationNode(1),
        identifier: "co_located".to_string(),
        section_topic: dt(1),
        phase_resolved: ResolutionPhase::PhaseC,
        iteration: 2,
        chosen_topic: Some(topic),
        candidate_scores: vec![CandidateScore {
          topic,
          qualified_name: Some("X.co_located".to_string()),
          pr_score: 0.5,
        }],
        top_contributing_edges: Vec::new(),
      },
    );
    let json = serde_json::to_string(&dump_resolution_traces(&audit)).unwrap();
    assert!(
      json.contains(r#""phase_resolved":"C""#),
      "PhaseC must serialize as the spec label \"C\"; got: {json}"
    );
  }

  /// Multiple `DevDocComment` traces under different comment topics and
  /// occurrences. Pins that the BTreeMap iteration sorts by
  /// `(comment_topic, occurrence)` — the derived `Ord` on
  /// `ResolutionRefId::DevDocComment` declares the fields in that order,
  /// so a future field reorder would silently re-sort the dump.
  #[test]
  fn trace_dump_orders_dev_doc_entries_by_topic_then_occurrence() {
    let mut audit = empty_audit();
    let mk_trace = |comment_topic, occurrence: u32| {
      let id = ResolutionRefId::DevDocComment {
        comment_topic,
        occurrence,
      };
      (
        id.clone(),
        ResolutionTrace {
          reference_id: id,
          identifier: format!("ref_{occurrence}"),
          section_topic: comment_topic,
          phase_resolved: ResolutionPhase::Unresolved,
          iteration: 1,
          chosen_topic: None,
          candidate_scores: Vec::new(),
          top_contributing_edges: Vec::new(),
        },
      )
    };

    // Insert in scrambled order across two comment topics; the dump
    // must emerge in (comment_topic asc, occurrence asc) order.
    let topic_a = ct(-10);
    let topic_b = ct(-5);
    for (id, trace) in [
      mk_trace(topic_b, 1),
      mk_trace(topic_a, 2),
      mk_trace(topic_b, 0),
      mk_trace(topic_a, 0),
    ] {
      audit.resolution_traces.insert(id, trace);
    }

    let dump = dump_resolution_traces(&audit);
    let refs: Vec<&str> =
      dump.iter().map(|r| r.reference_node.as_str()).collect();
    // Topic Ord: Comment(-10) < Comment(-5). Within each, occurrence asc.
    assert_eq!(
      refs,
      vec![
        "comment:C-10:0",
        "comment:C-10:2",
        "comment:C-5:0",
        "comment:C-5:1"
      ]
    );
  }

  /// Empty audit through the full `dump_to_file` path produces a valid
  /// `{"nodes":[],"edges":[]}` document on disk. Operators get a
  /// well-formed (and parseable) file even before the graph builder has
  /// run, so downstream tooling can rely on the file existing rather
  /// than special-casing the cold-start case.
  #[test]
  fn dump_to_file_resolution_graph_for_no_graph_audit_writes_empty_shape() {
    let audit = empty_audit();
    let dir = unique_tmp_dir("graph-empty");
    let path = dump_to_file(DumpKind::ResolutionGraph, &audit, &dir).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["nodes"].as_array().map(|a| a.len()), Some(0));
    assert_eq!(parsed["edges"].as_array().map(|a| a.len()), Some(0));
    let _ = std::fs::remove_dir_all(&dir);
  }

  /// A graph edge whose destination has no `topic_metadata` entry must
  /// still appear in the node list with empty `kind` / `qualified_name`
  /// — the docstring on `ResolutionGraphNode::kind` advertises this
  /// fallback. Without this test, removing `unwrap_or_default()` would
  /// regress to an `Option` field (or a panic on `.unwrap()`) silently.
  #[test]
  fn graph_dump_orphan_dest_without_metadata_renders_empty_kind_and_name() {
    let mut audit = empty_audit();
    let src = nt(1000);
    let orphan = nt(2000);
    audit
      .topic_metadata
      .insert(src, named(src, "src", NamedTopicKind::LocalVariable));
    // Deliberately omit metadata for `orphan`.
    let mut graph = ResolutionGraph::new();
    graph.add_edge(src, orphan, EdgeType::References, 0.5);
    graph.finalize();
    audit.resolution_graph = Some(graph);

    let dump = dump_resolution_graph(&audit);
    let orphan_node = dump.nodes.iter().find(|n| n.topic == "N2000").unwrap();
    assert_eq!(orphan_node.kind, "");
    assert_eq!(orphan_node.qualified_name, "");
    // The source-with-metadata node must still render normally —
    // verifies the empty fallback is per-node, not a global behavior.
    let src_node = dump.nodes.iter().find(|n| n.topic == "N1000").unwrap();
    assert_eq!(src_node.kind, "LocalVariable");
    assert_eq!(src_node.qualified_name, "src");
  }

  // -------------------------------------------------------------------------
  // Helpers for the backfill suites below
  // -------------------------------------------------------------------------

  /// Variant of `named` that points at another topic via `transitive_topic`.
  /// Required for `InterfaceMapping` and the "transitive" branch of
  /// `NameIndex`.
  fn named_with_transitive(
    t: topic::Topic,
    name: &str,
    kind: NamedTopicKind,
    target: topic::Topic,
  ) -> TopicMetadata {
    let mut meta = named(t, name, kind);
    if let TopicMetadata::NamedTopic {
      transitive_topic, ..
    } = &mut meta
    {
      *transitive_topic = Some(target);
    }
    meta
  }

  // -------------------------------------------------------------------------
  // interface-mapping: structural invariants
  //
  // Backfilled here alongside the new dump kinds so the entire dump module
  // sits behind one consistent test scaffold.
  // -------------------------------------------------------------------------

  #[test]
  fn interface_mapping_dump_empty_audit_is_empty() {
    let audit = empty_audit();
    assert!(dump_interface_mapping(&audit).is_empty());
  }

  #[test]
  fn interface_mapping_dump_emits_one_record_per_named_to_named_proxy() {
    let mut audit = empty_audit();
    let stub = nt(1);
    let target = nt(2);
    audit.topic_metadata.insert(
      target,
      named(target, "transfer", NamedTopicKind::LocalVariable),
    );
    audit.topic_metadata.insert(
      stub,
      named_with_transitive(
        stub,
        "transfer",
        NamedTopicKind::LocalVariable,
        target,
      ),
    );

    let records = dump_interface_mapping(&audit);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].proxy_topic, "N1");
    assert_eq!(records[0].target_topic, "N2");
    assert_eq!(records[0].proxy_name, "transfer");
    assert_eq!(records[0].target_name, "transfer");
  }

  #[test]
  fn interface_mapping_dump_skips_topics_without_transitive_target() {
    let mut audit = empty_audit();
    let plain = nt(1);
    audit.topic_metadata.insert(
      plain,
      named(plain, "transfer", NamedTopicKind::LocalVariable),
    );
    assert!(dump_interface_mapping(&audit).is_empty());
  }

  #[test]
  fn interface_mapping_dump_skips_when_target_metadata_missing() {
    // Defensive — `transitive_topic` could in principle point at a topic
    // the analyzer never registered. Silently skip rather than render a
    // half-populated record.
    let mut audit = empty_audit();
    let stub = nt(1);
    let target = nt(2);
    audit.topic_metadata.insert(
      stub,
      named_with_transitive(
        stub,
        "transfer",
        NamedTopicKind::LocalVariable,
        target,
      ),
    );
    // Deliberately omit `target` from topic_metadata.
    assert!(dump_interface_mapping(&audit).is_empty());
  }

  #[test]
  fn interface_mapping_dump_sorts_by_proxy_qualified_name_then_topic() {
    // Two stubs, same simple name "transfer", inserted in scrambled order.
    // Sorted output should be stable across runs.
    let mut audit = empty_audit();
    let stub_a = nt(50);
    let stub_b = nt(10);
    let target_a = nt(60);
    let target_b = nt(20);
    audit.topic_metadata.insert(
      target_a,
      named(target_a, "implA", NamedTopicKind::LocalVariable),
    );
    audit.topic_metadata.insert(
      target_b,
      named(target_b, "implB", NamedTopicKind::LocalVariable),
    );
    // Insert proxies in topic-id-descending order to exercise the sort.
    audit.topic_metadata.insert(
      stub_a,
      named_with_transitive(
        stub_a,
        "transfer",
        NamedTopicKind::LocalVariable,
        target_a,
      ),
    );
    audit.topic_metadata.insert(
      stub_b,
      named_with_transitive(
        stub_b,
        "transfer",
        NamedTopicKind::LocalVariable,
        target_b,
      ),
    );

    let records = dump_interface_mapping(&audit);
    // Both proxies have the same qualified name ("transfer"), so the
    // tie-break is on proxy topic ID ascending: N10 before N50.
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].proxy_topic, "N10");
    assert_eq!(records[1].proxy_topic, "N50");
  }

  #[test]
  fn interface_mapping_dump_is_deterministic() {
    let mut audit = empty_audit();
    let stub = nt(1);
    let target = nt(2);
    audit.topic_metadata.insert(
      target,
      named(target, "transfer", NamedTopicKind::LocalVariable),
    );
    audit.topic_metadata.insert(
      stub,
      named_with_transitive(
        stub,
        "transfer",
        NamedTopicKind::LocalVariable,
        target,
      ),
    );
    let json_a =
      serde_json::to_string_pretty(&dump_interface_mapping(&audit)).unwrap();
    let json_b =
      serde_json::to_string_pretty(&dump_interface_mapping(&audit)).unwrap();
    assert_eq!(json_a, json_b);
  }

  // -------------------------------------------------------------------------
  // name-index: structural invariants
  // -------------------------------------------------------------------------

  /// Build an audit with every distinct shape the name-index dump cares
  /// about: a unique name, an ambiguous pair, a transitive-resolves-to-one
  /// case, a common-word entry, and an empty-name entry that must not
  /// surface at all.
  fn name_index_fixture() -> AuditData {
    let mut audit = empty_audit();
    // Unique name → resolved.
    let unique = nt(1);
    audit
      .topic_metadata
      .insert(unique, named(unique, "uniq", NamedTopicKind::LocalVariable));
    // Two non-transitive candidates with the same simple name → ambiguous.
    let amb_a = nt(2);
    let amb_b = nt(3);
    audit
      .topic_metadata
      .insert(amb_a, named(amb_a, "ambig", NamedTopicKind::LocalVariable));
    audit
      .topic_metadata
      .insert(amb_b, named(amb_b, "ambig", NamedTopicKind::LocalVariable));
    // One non-transitive + one transitive sharing a name → resolves to
    // the non-transitive (per `TopicNameIndex::build`).
    let real = nt(4);
    let proxy = nt(5);
    audit
      .topic_metadata
      .insert(real, named(real, "transfer", NamedTopicKind::LocalVariable));
    audit.topic_metadata.insert(
      proxy,
      named_with_transitive(
        proxy,
        "transfer",
        NamedTopicKind::LocalVariable,
        real,
      ),
    );
    // A common-word name — `is_common_word` filters it from name_index;
    // the dump must still show it but flag `is_common_word: true`.
    let common = nt(6);
    audit
      .topic_metadata
      .insert(common, named(common, "for", NamedTopicKind::LocalVariable));
    // Empty name — must be excluded from the dump.
    let nameless = nt(7);
    audit
      .topic_metadata
      .insert(nameless, named(nameless, "", NamedTopicKind::LocalVariable));
    audit.name_index = crate::domain::TopicNameIndex::build(&audit);
    audit
  }

  #[test]
  fn name_index_dump_empty_audit_is_empty() {
    // `new_audit_data` registers a couple of built-in topics
    // (`keccak256`, etc.). Each has a non-empty name, so the dump is
    // not literally empty — but every entry should be a single-candidate
    // resolved record. This pins the cold-start surface against future
    // built-in additions.
    let audit = empty_audit();
    let entries = dump_name_index(&audit);
    for entry in &entries {
      assert!(
        !entry.ambiguous,
        "built-in entry {:?} unexpectedly flagged ambiguous",
        entry.name
      );
      assert_eq!(entry.candidates.len(), 1, "built-in {:?}", entry.name);
    }
  }

  #[test]
  fn name_index_dump_resolved_unique_name_has_resolved_topic() {
    let audit = name_index_fixture();
    let entries = dump_name_index(&audit);
    let entry = entries.iter().find(|e| e.name == "uniq").unwrap();
    assert!(!entry.ambiguous);
    assert!(!entry.is_common_word);
    assert_eq!(entry.resolved_topic.as_deref(), Some("N1"));
    assert_eq!(entry.candidates.len(), 1);
  }

  #[test]
  fn name_index_dump_two_non_transitive_candidates_are_ambiguous() {
    let audit = name_index_fixture();
    let entries = dump_name_index(&audit);
    let entry = entries.iter().find(|e| e.name == "ambig").unwrap();
    assert!(entry.ambiguous);
    assert!(entry.resolved_topic.is_none());
    assert_eq!(entry.candidates.len(), 2);
  }

  #[test]
  fn name_index_dump_non_transitive_plus_transitive_is_resolved() {
    let audit = name_index_fixture();
    let entries = dump_name_index(&audit);
    let entry = entries.iter().find(|e| e.name == "transfer").unwrap();
    // Resolves to the non-transitive (real) topic — `TopicNameIndex::build`
    // strips proxies when there's exactly one real declaration.
    assert!(!entry.ambiguous);
    assert_eq!(entry.resolved_topic.as_deref(), Some("N4"));
    // Both candidates surface so an operator can see the proxy.
    assert_eq!(entry.candidates.len(), 2);
    let proxy_cand = entry.candidates.iter().find(|c| c.is_transitive).unwrap();
    assert_eq!(proxy_cand.transitive_target.as_deref(), Some("N4"));
  }

  #[test]
  fn name_index_dump_common_word_flag_is_set() {
    let audit = name_index_fixture();
    let entries = dump_name_index(&audit);
    let entry = entries.iter().find(|e| e.name == "for").unwrap();
    assert!(entry.is_common_word);
    // Common-word filtering means resolved is None even though there's
    // a single candidate — but `ambiguous` must NOT be set, since the
    // common-word case is not a resolver-ambiguity case.
    assert!(!entry.ambiguous);
    assert!(entry.resolved_topic.is_none());
  }

  #[test]
  fn name_index_dump_excludes_topics_with_empty_name() {
    let audit = name_index_fixture();
    let entries = dump_name_index(&audit);
    // The fixture inserted a NamedTopic with name = "". Confirm the dump
    // skipped it — empty names are never code identifiers.
    assert!(!entries.iter().any(|e| e.name.is_empty()));
  }

  #[test]
  fn name_index_dump_orders_ambiguous_first_then_alphabetical() {
    let audit = name_index_fixture();
    let entries = dump_name_index(&audit);
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    let first_unambig = names.iter().position(|n| {
      let e = entries.iter().find(|e| &e.name == n).unwrap();
      !e.ambiguous
    });
    let last_ambig = names.iter().rposition(|n| {
      let e = entries.iter().find(|e| &e.name == n).unwrap();
      e.ambiguous
    });
    if let (Some(unambig), Some(ambig)) = (first_unambig, last_ambig) {
      assert!(
        ambig < unambig,
        "ambiguous entries must come before unambiguous ones; \
         got names: {:?}",
        names,
      );
    }
  }

  #[test]
  fn name_index_dump_candidates_sorted_by_qualified_name_then_topic() {
    // Two ambiguous candidates with the same name share an empty
    // qualified-name prefix (they're at `Scope::Global`), so the tie-
    // break falls to topic ID ascending: N2 < N3.
    let audit = name_index_fixture();
    let entries = dump_name_index(&audit);
    let entry = entries.iter().find(|e| e.name == "ambig").unwrap();
    let topics: Vec<&str> =
      entry.candidates.iter().map(|c| c.topic.as_str()).collect();
    assert_eq!(topics, vec!["N2", "N3"]);
  }

  #[test]
  fn name_index_dump_is_deterministic() {
    let audit = name_index_fixture();
    let json_a =
      serde_json::to_string_pretty(&dump_name_index(&audit)).unwrap();
    let json_b =
      serde_json::to_string_pretty(&dump_name_index(&audit)).unwrap();
    assert_eq!(json_a, json_b);
  }

  #[test]
  fn name_index_dump_uses_canonical_is_common_word() {
    // The dump's `is_common_word` flag is now sourced from
    // `domain::is_common_word` — pin a representative entry so a future
    // domain-side stoplist change shows up here, not as a silent shift
    // in dump output.
    let mut audit = empty_audit();
    let common = nt(1);
    audit
      .topic_metadata
      .insert(common, named(common, "for", NamedTopicKind::LocalVariable));
    audit.name_index = crate::domain::TopicNameIndex::build(&audit);
    let entry = dump_name_index(&audit)
      .into_iter()
      .find(|e| e.name == "for")
      .unwrap();
    assert!(entry.is_common_word);
  }

  // -------------------------------------------------------------------------
  // parse_kinds: complex CLI surface
  //
  // The CLI accepts comma-separated, mixed-case, kebab/snake input plus
  // the `all` shorthand. Pin the dedup, error message, and ordering
  // contract — operators script against this.
  // -------------------------------------------------------------------------

  #[test]
  fn parse_kinds_accepts_comma_separated_value_in_single_arg() {
    let kinds =
      parse_kinds(&["interface-mapping,name-index".to_string()]).unwrap();
    assert_eq!(kinds, vec![DumpKind::InterfaceMapping, DumpKind::NameIndex]);
  }

  #[test]
  fn parse_kinds_accepts_mixed_kebab_and_snake_separators() {
    let kinds = parse_kinds(&[
      "resolution_graph".to_string(),
      "resolution-trace".to_string(),
    ])
    .unwrap();
    assert_eq!(
      kinds,
      vec![DumpKind::ResolutionGraph, DumpKind::ResolutionTrace]
    );
  }

  #[test]
  fn parse_kinds_dedupes_repeated_inputs_preserving_order() {
    let kinds = parse_kinds(&[
      "name-index".to_string(),
      "interface-mapping".to_string(),
      "name-index".to_string(),
    ])
    .unwrap();
    assert_eq!(kinds, vec![DumpKind::NameIndex, DumpKind::InterfaceMapping]);
  }

  #[test]
  fn parse_kinds_all_plus_explicit_kinds_dedupes() {
    // `all` already includes every kind; an extra explicit kind on the
    // command line must not duplicate it. First-occurrence ordering means
    // `all`'s expansion runs first, and the trailing explicit kind is
    // dropped from the output rather than re-appended.
    let kinds =
      parse_kinds(&["all".to_string(), "name-index".to_string()]).unwrap();
    assert_eq!(kinds, DumpKind::all());
  }

  #[test]
  fn parse_kinds_explicit_kind_before_all_pins_first_position_then_expands() {
    // The complement to the `all`-then-explicit case: when a kind is
    // named explicitly *before* `all`, it stays at index 0 and `all`
    // backfills the rest. Pinning this guarantees a stable mental model
    // for operators who scan `dump` output in CLI order.
    let kinds =
      parse_kinds(&["name-index".to_string(), "all".to_string()]).unwrap();
    assert_eq!(kinds[0], DumpKind::NameIndex);
    assert_eq!(kinds.len(), DumpKind::all().len());
    let unique: HashSet<DumpKind> = kinds.iter().copied().collect();
    assert_eq!(unique.len(), kinds.len());
  }

  #[test]
  fn parse_kinds_skips_empty_and_whitespace_only_pieces() {
    // Trailing or leading commas yield empty pieces; whitespace-only
    // pieces should be ignored too. This makes scripts that build the
    // CLI string by joining a Vec<String> robust to empty members.
    let kinds = parse_kinds(&[
      ",interface-mapping,, ,".to_string(),
      "  ".to_string(),
      "name-index".to_string(),
    ])
    .unwrap();
    assert_eq!(kinds, vec![DumpKind::InterfaceMapping, DumpKind::NameIndex]);
  }

  #[test]
  fn parse_kinds_unknown_kind_error_lists_every_known_kind() {
    let err = parse_kinds(&["bogus".to_string()]).unwrap_err();
    // The error must guide the operator: name the offender and list
    // every accepted kind so a typo is one read away from a fix.
    assert!(err.contains("bogus"), "got: {err}");
    for kind in [
      "interface-mapping",
      "name-index",
      "resolution-graph",
      "resolution-trace",
      "all",
    ] {
      assert!(err.contains(kind), "error must mention {}: {err}", kind);
    }
  }

  // -------------------------------------------------------------------------
  // resolution-graph: extra complex-interaction coverage
  // -------------------------------------------------------------------------

  #[test]
  fn graph_dump_handles_self_loops_as_single_record() {
    // A topic with an out-edge to itself appears in `nodes` exactly once
    // (BTreeSet dedup) and in `edges` exactly once. Self-loops are rare
    // but legal under the spec — e.g. recursive functions.
    let mut audit = empty_audit();
    let recursive = nt(1);
    audit.topic_metadata.insert(
      recursive,
      named(recursive, "recur", NamedTopicKind::LocalVariable),
    );
    let mut graph = ResolutionGraph::new();
    graph.add_edge(recursive, recursive, EdgeType::Calls, 0.7);
    graph.finalize();
    audit.resolution_graph = Some(graph);

    let dump = dump_resolution_graph(&audit);
    assert_eq!(dump.nodes.len(), 1);
    assert_eq!(dump.nodes[0].topic, "N1");
    assert_eq!(dump.edges.len(), 1);
    assert_eq!(dump.edges[0].source, "N1");
    assert_eq!(dump.edges[0].dest, "N1");
  }

  #[test]
  fn graph_dump_emits_distinct_records_for_parallel_edges_of_different_types() {
    // Same (source, dest) but two edge types — common in the producer
    // (e.g. a function both Calls and References another). Each must
    // get its own record and the sort must order by edge_type
    // discriminant.
    let mut audit = empty_audit();
    let s = nt(1);
    let d = nt(2);
    audit
      .topic_metadata
      .insert(s, named(s, "s", NamedTopicKind::LocalVariable));
    audit
      .topic_metadata
      .insert(d, named(d, "d", NamedTopicKind::LocalVariable));
    let mut graph = ResolutionGraph::new();
    // Insert References before Calls; the dump's secondary sort fixes it.
    graph.add_edge(s, d, EdgeType::References, 0.5);
    graph.add_edge(s, d, EdgeType::Calls, 0.7);
    graph.finalize();
    audit.resolution_graph = Some(graph);

    let dump = dump_resolution_graph(&audit);
    assert_eq!(dump.edges.len(), 2);
    assert_eq!(dump.edges[0].edge_type, EdgeType::Calls);
    assert_eq!(dump.edges[1].edge_type, EdgeType::References);
  }

  #[test]
  fn graph_dump_orders_topics_across_variant_kinds() {
    // The graph can in principle hold topics of any `Topic` variant,
    // not only `Node`. Variant declaration order is `Node < Documentation
    // < Comment`; the dump must respect that.
    let mut audit = empty_audit();
    let n = nt(1);
    let d = topic::new_documentation_topic(1);
    let c = topic::new_comment_topic(1);
    let mut graph = ResolutionGraph::new();
    graph.add_edge(c, n, EdgeType::References, 0.5);
    graph.add_edge(d, n, EdgeType::References, 0.5);
    graph.add_edge(n, d, EdgeType::References, 0.5);
    graph.finalize();
    audit.resolution_graph = Some(graph);

    let dump = dump_resolution_graph(&audit);
    let topics: Vec<&str> =
      dump.nodes.iter().map(|n| n.topic.as_str()).collect();
    // Variant order: Node < Documentation < Comment.
    assert_eq!(topics, vec!["N1", "D1", "C1"]);
  }

  // -------------------------------------------------------------------------
  // resolution-trace: extra complex-interaction coverage
  // -------------------------------------------------------------------------

  #[test]
  fn trace_dump_preserves_producer_candidate_order_for_many_candidates() {
    // The producer (resolution_pass.rs) sorts candidates by
    // (pr_score desc, qualified_name asc, topic asc). The dump trusts
    // that ordering. Pin it with five candidates in a non-trivial order
    // so a regression that re-sorts on a different key surfaces here.
    let mut audit = empty_audit();
    let scores: Vec<CandidateScore> = (0..5)
      .map(|i| CandidateScore {
        topic: nt(i + 100),
        qualified_name: Some(format!("Mod.cand_{i}")),
        pr_score: 1.0 - (i as f32) * 0.1,
      })
      .collect();
    audit.resolution_traces.insert(
      ResolutionRefId::DocumentationNode(1),
      ResolutionTrace {
        reference_id: ResolutionRefId::DocumentationNode(1),
        identifier: "x".to_string(),
        section_topic: dt(1),
        phase_resolved: ResolutionPhase::PhaseB,
        iteration: 1,
        chosen_topic: Some(scores[0].topic),
        candidate_scores: scores.clone(),
        top_contributing_edges: Vec::new(),
      },
    );

    let record = dump_resolution_traces(&audit).into_iter().next().unwrap();
    let topics: Vec<String> = record
      .candidate_scores
      .iter()
      .map(|c| c.topic.clone())
      .collect();
    let expected: Vec<String> = scores.iter().map(|c| c.topic.id()).collect();
    assert_eq!(topics, expected);
  }

  #[test]
  fn trace_dump_preserves_top_contributing_edge_order_with_ties() {
    // Producer documents a (predecessor asc, edge_type asc) tie-break
    // when `weighted_contribution` is equal. The dump must not re-sort
    // by some other key (e.g. by edge_type) and lose the tie-break.
    let mut audit = empty_audit();
    let chosen = nt(1);
    let pred_a = nt(2);
    let pred_b = nt(3);
    audit.resolution_traces.insert(
      ResolutionRefId::DocumentationNode(1),
      ResolutionTrace {
        reference_id: ResolutionRefId::DocumentationNode(1),
        identifier: "x".to_string(),
        section_topic: dt(1),
        phase_resolved: ResolutionPhase::PhaseB,
        iteration: 1,
        chosen_topic: Some(chosen),
        candidate_scores: vec![CandidateScore {
          topic: chosen,
          qualified_name: Some("Mod.x".to_string()),
          pr_score: 1.0,
        }],
        // Three contributions: same weight on first two (tie-break by
        // predecessor asc), strictly smaller on the third.
        top_contributing_edges: vec![
          EdgeContribution {
            predecessor: pred_a,
            edge_type: EdgeType::Calls,
            weighted_contribution: 0.30,
          },
          EdgeContribution {
            predecessor: pred_b,
            edge_type: EdgeType::Calls,
            weighted_contribution: 0.30,
          },
          EdgeContribution {
            predecessor: pred_b,
            edge_type: EdgeType::References,
            weighted_contribution: 0.10,
          },
        ],
      },
    );
    let record = dump_resolution_traces(&audit).into_iter().next().unwrap();
    let preds: Vec<&str> = record
      .top_contributing_edges
      .iter()
      .map(|e| e.predecessor.as_str())
      .collect();
    assert_eq!(preds, vec!["N2", "N3", "N3"]);
  }

  #[test]
  fn trace_dump_renders_empty_candidate_scores_as_empty_array() {
    // A trace with no candidates is pathological but legal. The dump
    // should render an empty JSON array, not omit the field — operator
    // tooling iterating `record.candidate_scores` must always find a
    // value.
    let mut audit = empty_audit();
    audit.resolution_traces.insert(
      ResolutionRefId::DocumentationNode(1),
      ResolutionTrace {
        reference_id: ResolutionRefId::DocumentationNode(1),
        identifier: "x".to_string(),
        section_topic: dt(1),
        phase_resolved: ResolutionPhase::Unresolved,
        iteration: 1,
        chosen_topic: None,
        candidate_scores: Vec::new(),
        top_contributing_edges: Vec::new(),
      },
    );
    let json = serde_json::to_string(&dump_resolution_traces(&audit)).unwrap();
    assert!(
      json.contains(r#""candidate_scores":[]"#),
      "empty candidate list must serialize as `[]`, got: {json}"
    );
  }

  #[test]
  fn trace_dump_groups_multiple_traces_under_same_section_in_order() {
    // Two ambiguous references in one section: the dump must list both
    // and tag them with the same `section_or_comment_id`. Pin the
    // grouping so a future change that flattens or merges traces shows
    // up here.
    let mut audit = empty_audit();
    let section = dt(42);
    for (id, name) in [(1, "alpha"), (2, "beta"), (3, "gamma")] {
      audit.resolution_traces.insert(
        ResolutionRefId::DocumentationNode(id),
        ResolutionTrace {
          reference_id: ResolutionRefId::DocumentationNode(id),
          identifier: name.to_string(),
          section_topic: section,
          phase_resolved: ResolutionPhase::PhaseB,
          iteration: 1,
          chosen_topic: None,
          candidate_scores: Vec::new(),
          top_contributing_edges: Vec::new(),
        },
      );
    }
    let dump = dump_resolution_traces(&audit);
    assert_eq!(dump.len(), 3);
    for r in &dump {
      assert_eq!(r.section_or_comment_id, "D42");
    }
    let identifiers: Vec<&str> =
      dump.iter().map(|r| r.identifier.as_str()).collect();
    assert_eq!(identifiers, vec!["alpha", "beta", "gamma"]);
  }

  // -------------------------------------------------------------------------
  // dump_to_file: complex-interaction coverage
  // -------------------------------------------------------------------------

  #[test]
  fn dump_to_file_overwrites_existing_file_with_atomic_rename() {
    // Two dumps to the same path with different contents — the second
    // must replace the first byte-for-byte. Validates the temp-file +
    // rename pattern survives a populated destination.
    let dir = unique_tmp_dir("graph-overwrite");
    // First write: empty audit → empty graph.
    let empty = empty_audit();
    let path = dump_to_file(DumpKind::ResolutionGraph, &empty, &dir).unwrap();
    let bytes_first = std::fs::read(&path).unwrap();

    // Second write to same path: populated audit → non-empty graph.
    let populated = populated_audit();
    let path_again =
      dump_to_file(DumpKind::ResolutionGraph, &populated, &dir).unwrap();
    assert_eq!(path, path_again);
    let bytes_second = std::fs::read(&path).unwrap();

    assert_ne!(bytes_first, bytes_second);
    let parsed: serde_json::Value =
      serde_json::from_slice(&bytes_second).unwrap();
    assert!(!parsed["edges"].as_array().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn dump_to_file_recovers_when_stale_tmp_file_present() {
    // Simulate a previous run that crashed mid-write, leaving the
    // `.tmp` file around. The next dump must clobber it cleanly via
    // `File::create` truncation, then atomically rename. No leftover
    // `.tmp` should remain on disk.
    let dir = unique_tmp_dir("graph-stale-tmp");
    let final_path = dir.join("resolution-graph.json");
    let stale_tmp = final_path.with_extension("json.tmp");
    std::fs::write(&stale_tmp, b"junk from previous crashed run\n").unwrap();
    assert!(stale_tmp.exists());

    let audit = populated_audit();
    let path = dump_to_file(DumpKind::ResolutionGraph, &audit, &dir).unwrap();
    assert_eq!(path, final_path);
    // `rename` consumes the `.tmp`, leaving only the final file.
    assert!(!stale_tmp.exists());

    let parsed: serde_json::Value =
      serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(parsed["nodes"].as_array().is_some());
    let _ = std::fs::remove_dir_all(&dir);
  }

  // -------------------------------------------------------------------------
  // kind_label: wire format
  //
  // The label string surfaces in three places: graph dump nodes,
  // name-index candidates, and interface-mapping records. Operators
  // grep these values, so the wire shape (variant name, parenthesized
  // payload) is part of the public contract.
  // -------------------------------------------------------------------------

  #[test]
  fn kind_label_renders_every_named_topic_variant_distinctly() {
    use crate::domain::{ContractKind, FunctionKind, VariableMutability};
    let cases: &[(NamedTopicKind, &str)] = &[
      (
        NamedTopicKind::Function(FunctionKind::Function),
        "Function(Function)",
      ),
      (
        NamedTopicKind::Function(FunctionKind::Constructor),
        "Function(Constructor)",
      ),
      (NamedTopicKind::Modifier, "Modifier"),
      (NamedTopicKind::Event, "Event"),
      (NamedTopicKind::Error, "Error"),
      (NamedTopicKind::Struct, "Struct"),
      (NamedTopicKind::Enum, "Enum"),
      (NamedTopicKind::EnumMember, "EnumMember"),
      (
        NamedTopicKind::StateVariable(VariableMutability::Constant),
        "StateVariable(Constant)",
      ),
      (NamedTopicKind::LocalVariable, "LocalVariable"),
      (
        NamedTopicKind::Contract(ContractKind::Interface),
        "Contract(Interface)",
      ),
      (
        NamedTopicKind::Contract(ContractKind::Library),
        "Contract(Library)",
      ),
      (NamedTopicKind::Builtin, "Builtin"),
    ];
    for (kind, expected) in cases {
      let t = nt(0);
      let meta = named(t, "x", kind.clone());
      assert_eq!(kind_label(&meta), *expected, "for kind {:?}", kind);
    }
  }

  #[test]
  fn dump_to_file_writes_distinct_kinds_to_distinct_filenames() {
    // Sanity for the multi-kind CLI flow: dispatching every kind in
    // one invocation produces one file per kind, all in the same
    // directory, with no overlap.
    let dir = unique_tmp_dir("multi-kind");
    let audit = populated_audit();
    for kind in DumpKind::all() {
      let path = dump_to_file(kind, &audit, &dir).unwrap();
      assert_eq!(
        path.file_name().and_then(|n| n.to_str()),
        Some(kind.file_name())
      );
      assert!(path.exists(), "{} not written", kind.file_name());
    }
    // All four files coexist.
    let entries: Vec<String> = std::fs::read_dir(&dir)
      .unwrap()
      .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
      .collect();
    for kind in DumpKind::all() {
      assert!(
        entries.iter().any(|n| n == kind.file_name()),
        "{} missing from {:?}",
        kind.file_name(),
        entries
      );
    }
    let _ = std::fs::remove_dir_all(&dir);
  }
}
