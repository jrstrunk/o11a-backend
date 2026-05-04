//! The Rust edge extractor (skeleton).
//!
//! Mirrors `solidity_extractor.rs` for the Rust language layer described
//! in `crates/o11a-analyze/docs/specs/semantic-resolution-graph.md`
//! (sections "Polyglot model", "Rust-specific extensions (planned)",
//! and "Adding a new language"). The Rust analyzer does not yet exist,
//! so this extractor produces no edges in practice — it walks an empty
//! `Vec<ASTNode>` for each registered `AST::Rust`. The structure is in
//! place so that when the Rust analyzer lands and produces topics with
//! Rust-flavored `NamedTopicKind` variants plus parsed `RustAST` nodes,
//! the universal-core and Rust-specific edges land in one wired
//! pipeline pass.
//!
//! Determinism contract is the same as the Solidity extractor:
//!
//! * Iteration sources are ordered (`BTreeMap`, `Vec` with a stable
//!   producer) so traversal order is fixed.
//! * Per-source duplicate edges are filtered by a `BTreeSet`, so the
//!   same `(src, dst, edge_type)` triple is never emitted twice from one
//!   extractor pass.
//! * Edge weights come from `EdgeType::default_weight` only.
//! * Edges to non-`NamedTopic` destinations are dropped.
//! * Solidity-flavored topics (those whose defining file is an
//!   `AST::Solidity`) are skipped. The dispatch is by **file**, not by
//!   topic kind, so when Rust topics enter the audit they will be
//!   matched on container path without reshaping `NamedTopicKind`.

use std::collections::BTreeSet;

use crate::domain::{
  AST, AuditData, FunctionModProperties, ProjectPath, Scope, TopicMetadata,
  topic,
};
use crate::rust::ast::ASTNode;

use super::builder::Extractor;
use super::edge::EdgeType;
use super::graph::ResolutionGraph;

/// Rust-specific edge extractor. Registered in `builder::extractors()`.
pub struct RustExtractor;

impl Extractor for RustExtractor {
  fn applies_to(&self, audit_data: &AuditData) -> bool {
    audit_data
      .asts
      .values()
      .any(|ast| matches!(ast, AST::Rust(_)))
  }

  fn extract(&self, audit_data: &AuditData, graph: &mut ResolutionGraph) {
    let mut emitted: BTreeSet<(topic::Topic, topic::Topic, EdgeType)> =
      BTreeSet::new();
    let rust_files: BTreeSet<ProjectPath> = audit_data
      .asts
      .iter()
      .filter_map(|(path, ast)| match ast {
        AST::Rust(_) => Some(path.clone()),
        _ => None,
      })
      .collect();

    extract_containment_edges(audit_data, &rust_files, graph, &mut emitted);
    extract_inheritance_edges(audit_data, &rust_files, graph, &mut emitted);
    extract_proxy_of_edges(audit_data, &rust_files, graph, &mut emitted);
    extract_function_property_edges(
      audit_data,
      &rust_files,
      graph,
      &mut emitted,
    );
    extract_derives_edges(audit_data, graph, &mut emitted);
    extract_re_exports_edges(audit_data, graph, &mut emitted);
    extract_mutates_field_edges(audit_data, graph, &mut emitted);
  }
}

// ---------------------------------------------------------------------------
// Edge emission helpers (mirror solidity_extractor)
// ---------------------------------------------------------------------------

/// Insert a directed edge if `(src, dst, edge_type)` has not been emitted
/// yet in this extractor pass and the destination is a `NamedTopic`.
/// Self-loops (`src == dst`) are silently dropped.
fn add_directed(
  audit_data: &AuditData,
  graph: &mut ResolutionGraph,
  emitted: &mut BTreeSet<(topic::Topic, topic::Topic, EdgeType)>,
  src: topic::Topic,
  dst: topic::Topic,
  edge_type: EdgeType,
) {
  if src == dst || !is_named_topic(audit_data, &dst) {
    return;
  }
  if emitted.insert((src, dst, edge_type)) {
    graph.add_edge(src, dst, edge_type, edge_type.default_weight());
  }
}

/// Insert an undirected edge as two directed entries (one per direction).
/// Both endpoints must be `NamedTopic`.
fn add_undirected(
  audit_data: &AuditData,
  graph: &mut ResolutionGraph,
  emitted: &mut BTreeSet<(topic::Topic, topic::Topic, EdgeType)>,
  a: topic::Topic,
  b: topic::Topic,
  edge_type: EdgeType,
) {
  if a == b
    || !is_named_topic(audit_data, &a)
    || !is_named_topic(audit_data, &b)
  {
    return;
  }
  let w = edge_type.default_weight();
  if emitted.insert((a, b, edge_type)) {
    graph.add_edge(a, b, edge_type, w);
  }
  if emitted.insert((b, a, edge_type)) {
    graph.add_edge(b, a, edge_type, w);
  }
}

fn is_named_topic(audit_data: &AuditData, t: &topic::Topic) -> bool {
  matches!(
    audit_data.topic_metadata.get(t),
    Some(TopicMetadata::NamedTopic { .. })
  )
}

/// Whether a topic's defining container file is a Rust AST. Used to
/// keep the Rust extractor from re-emitting edges for Solidity topics
/// in a polyglot audit (the Solidity extractor already covers them).
///
/// Topics in `Scope::Global` or with no container are treated as
/// non-Rust; the Rust analyzer will never produce a global-only named
/// topic without an originating file. Built-in cross-language topics
/// would slip through with no edges, which is the desired skeleton
/// behavior.
fn is_rust_topic(
  audit_data: &AuditData,
  rust_files: &BTreeSet<ProjectPath>,
  t: &topic::Topic,
) -> bool {
  let Some(TopicMetadata::NamedTopic { scope, .. }) =
    audit_data.topic_metadata.get(t)
  else {
    return false;
  };
  match scope {
    Scope::Container { container }
    | Scope::Component { container, .. }
    | Scope::Member { container, .. }
    | Scope::ContainingBlock { container, .. } => rust_files.contains(container),
    Scope::Global => false,
  }
}

// ---------------------------------------------------------------------------
// Edge-type extractors (mirrors solidity_extractor module layout)
// ---------------------------------------------------------------------------

/// `contains-member`, `contains-field`, `contains-local`.
///
/// Universal-core containment edges. The Rust analyzer's eventual
/// `NamedTopicKind` additions (e.g. `Module`, `Trait`, `Impl`) will
/// match here. Until those exist, every match arm is a no-op.
fn extract_containment_edges(
  audit_data: &AuditData,
  rust_files: &BTreeSet<ProjectPath>,
  _graph: &mut ResolutionGraph,
  _emitted: &mut BTreeSet<(topic::Topic, topic::Topic, EdgeType)>,
) {
  for (topic, metadata) in &audit_data.topic_metadata {
    let TopicMetadata::NamedTopic { scope, .. } = metadata else {
      continue;
    };
    if !is_rust_topic(audit_data, rust_files, topic) {
      continue;
    }
    match scope {
      // When the Rust analyzer adds Rust-flavored `NamedTopicKind`
      // variants for the parent component (module, struct, enum, impl,
      // trait), match them here and call `add_undirected` with the
      // appropriate universal-core edge type:
      //
      //   - Module / Impl block / Trait → ContainsMember
      //   - Struct / Enum               → ContainsField
      //   - Function                    → ContainsLocal
      //
      // Today no such variants exist, so the body is intentionally
      // empty. The structural placeholder keeps the Rust extractor
      // shape consistent with `solidity_extractor::extract_containment_edges`.
      Scope::Member { .. }
      | Scope::ContainingBlock { .. }
      | Scope::Component { .. }
      | Scope::Container { .. }
      | Scope::Global => {}
    }
  }
}

/// `implements`. Driven by `audit_data.inheritance` for Rust topics
/// (`impl Trait for Type` produces an inheritance entry once the Rust
/// analyzer populates this map).
fn extract_inheritance_edges(
  audit_data: &AuditData,
  rust_files: &BTreeSet<ProjectPath>,
  graph: &mut ResolutionGraph,
  emitted: &mut BTreeSet<(topic::Topic, topic::Topic, EdgeType)>,
) {
  for (child, bases) in &audit_data.inheritance {
    if !is_rust_topic(audit_data, rust_files, child) {
      continue;
    }
    for base in bases {
      add_undirected(
        audit_data,
        graph,
        emitted,
        *child,
        *base,
        EdgeType::Implements,
      );
    }
  }
}

/// `proxy-of`. Driven by `TopicMetadata::transitive_topic`. The Rust
/// analyzer's analogue of Solidity's interface→implementation
/// resolution will write into the same field once it lands.
fn extract_proxy_of_edges(
  audit_data: &AuditData,
  rust_files: &BTreeSet<ProjectPath>,
  graph: &mut ResolutionGraph,
  emitted: &mut BTreeSet<(topic::Topic, topic::Topic, EdgeType)>,
) {
  for (src, metadata) in &audit_data.topic_metadata {
    if !matches!(metadata, TopicMetadata::NamedTopic { .. }) {
      continue;
    }
    if !is_rust_topic(audit_data, rust_files, src) {
      continue;
    }
    let Some(target) = metadata.transitive_topic() else {
      continue;
    };
    add_directed(
      audit_data,
      graph,
      emitted,
      *src,
      *target,
      EdgeType::ProxyOf,
    );
  }
}

/// `calls`, `mutates-field` (Rust-specific replacement for
/// `writes-state`), and the `references` residual.
///
/// `function_properties` is shared across language analyzers; the Rust
/// analyzer will write into it the same way Solidity does, with one
/// difference — Rust mutations are field-level (`writes through &mut`)
/// rather than state-variable-level. To keep the data shape stable
/// between languages, the Rust extractor reuses
/// `FunctionModProperties::mutations` and emits `MutatesField` edges
/// from those entries. (See open calibration question #6 in the spec
/// for the unification discussion.)
fn extract_function_property_edges(
  audit_data: &AuditData,
  rust_files: &BTreeSet<ProjectPath>,
  graph: &mut ResolutionGraph,
  emitted: &mut BTreeSet<(topic::Topic, topic::Topic, EdgeType)>,
) {
  for (src, props) in &audit_data.function_properties {
    if !is_named_topic(audit_data, src) {
      continue;
    }
    if !is_rust_topic(audit_data, rust_files, src) {
      continue;
    }

    let (calls, mutations) = match props {
      FunctionModProperties::FunctionProperties {
        calls, mutations, ..
      }
      | FunctionModProperties::ModifierProperties {
        calls, mutations, ..
      } => (calls, mutations),
    };

    let mut covered: BTreeSet<topic::Topic> = BTreeSet::new();

    for callee in calls {
      add_directed(audit_data, graph, emitted, *src, *callee, EdgeType::Calls);
      covered.insert(*callee);
    }
    for field in mutations {
      add_directed(
        audit_data,
        graph,
        emitted,
        *src,
        *field,
        EdgeType::MutatesField,
      );
      covered.insert(*field);
    }
    // `events_emitted` and `reverts` are Solidity-specific in spirit
    // (Solidity events / custom errors). Rust does not currently use
    // those edge kinds; leaving them unhandled here is intentional
    // until the Rust analyzer signals a need.

    if let Some(contexts) = audit_data.topic_context.get(src) {
      for ctx in contexts {
        for r in ctx.scope_references() {
          let dst = *r.reference_topic();
          if covered.contains(&dst) {
            continue;
          }
          add_directed(
            audit_data,
            graph,
            emitted,
            *src,
            dst,
            EdgeType::References,
          );
        }
      }
    }
  }
}

/// `derives`. Walks every Rust AST for `#[derive(...)]` attributes
/// (skeleton — the `RustAST` shape today has no derive variant). When
/// the parser produces them, follow the `walk_for_using_for` shape from
/// the Solidity extractor: enumerate items in source-unit order and
/// emit one undirected edge per `(struct/enum, derived trait)` pair.
fn extract_derives_edges(
  audit_data: &AuditData,
  graph: &mut ResolutionGraph,
  emitted: &mut BTreeSet<(topic::Topic, topic::Topic, EdgeType)>,
) {
  for ast in audit_data.asts.values() {
    if let AST::Rust(rust) = ast {
      for node in &rust.nodes {
        walk_for_derives(node, audit_data, graph, emitted);
      }
    }
  }
}

fn walk_for_derives(
  node: &ASTNode,
  _audit_data: &AuditData,
  _graph: &mut ResolutionGraph,
  _emitted: &mut BTreeSet<(topic::Topic, topic::Topic, EdgeType)>,
) {
  // Future: pattern-match on the parser-emitted derive variant and
  // call `add_undirected(.., EdgeType::Derives)` for each derived
  // trait topic. Today the AST has only `SourceFile`, which carries
  // no items.
  match node {
    ASTNode::SourceFile { items, .. } => {
      for item in items {
        walk_for_derives(item, _audit_data, _graph, _emitted);
      }
    }
  }
}

/// `re-exports`. Walks every Rust AST for `pub use ...;` items
/// (skeleton). Future: emit a directed edge `module → item` per
/// re-exported declaration.
fn extract_re_exports_edges(
  audit_data: &AuditData,
  graph: &mut ResolutionGraph,
  emitted: &mut BTreeSet<(topic::Topic, topic::Topic, EdgeType)>,
) {
  for ast in audit_data.asts.values() {
    if let AST::Rust(rust) = ast {
      for node in &rust.nodes {
        walk_for_re_exports(node, audit_data, graph, emitted);
      }
    }
  }
}

fn walk_for_re_exports(
  node: &ASTNode,
  _audit_data: &AuditData,
  _graph: &mut ResolutionGraph,
  _emitted: &mut BTreeSet<(topic::Topic, topic::Topic, EdgeType)>,
) {
  match node {
    ASTNode::SourceFile { items, .. } => {
      for item in items {
        walk_for_re_exports(item, _audit_data, _graph, _emitted);
      }
    }
  }
}

/// `mutates-field`. Driven primarily by `FunctionModProperties::mutations`
/// in `extract_function_property_edges`; this AST-driven helper is a
/// placeholder for future cases where the Rust analyzer cannot lift a
/// mutation into `function_properties` (e.g. cross-module mutation through
/// a captured reference) and the extractor needs to walk the body
/// directly. Skeleton today.
fn extract_mutates_field_edges(
  audit_data: &AuditData,
  graph: &mut ResolutionGraph,
  emitted: &mut BTreeSet<(topic::Topic, topic::Topic, EdgeType)>,
) {
  for ast in audit_data.asts.values() {
    if let AST::Rust(rust) = ast {
      for node in &rust.nodes {
        walk_for_mutations(node, audit_data, graph, emitted);
      }
    }
  }
}

fn walk_for_mutations(
  node: &ASTNode,
  _audit_data: &AuditData,
  _graph: &mut ResolutionGraph,
  _emitted: &mut BTreeSet<(topic::Topic, topic::Topic, EdgeType)>,
) {
  match node {
    ASTNode::SourceFile { items, .. } => {
      for item in items {
        walk_for_mutations(item, _audit_data, _graph, _emitted);
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  use crate::domain::{
    NamedTopicKind, NamedTopicVisibility, new_audit_data,
  };
  use crate::rust::ast::{RustAST, SourceLocation};
  use std::collections::HashSet;

  fn t(id: i32) -> topic::Topic {
    topic::new_node_topic(&id)
  }

  fn empty_audit() -> AuditData {
    new_audit_data("test".to_string(), HashSet::new(), None)
  }

  fn project_path(name: &str) -> ProjectPath {
    ProjectPath {
      file_path: name.to_string(),
    }
  }

  fn loc() -> SourceLocation {
    SourceLocation {
      start: None,
      length: None,
      index: None,
    }
  }

  fn make_rust_ast(file: &str) -> RustAST {
    RustAST {
      node_id: 0,
      nodes: Vec::new(),
      project_path: project_path(file),
    }
  }

  fn insert_named(
    audit: &mut AuditData,
    id: i32,
    kind: NamedTopicKind,
    scope: Scope,
    name: &str,
  ) -> topic::Topic {
    let topic = t(id);
    audit.topic_metadata.insert(
      topic,
      TopicMetadata::NamedTopic {
        topic,
        scope,
        kind,
        name: name.to_string(),
        visibility: NamedTopicVisibility::Public,
        is_mutable: false,
        mutations: Vec::new(),
        ancestors: Vec::new(),
        descendants: Vec::new(),
        relatives: Vec::new(),
        transitive_topic: None,
        doc_references: Vec::new(),
      },
    );
    topic
  }

  // -----------------------------------------------------------------------
  // applies_to
  // -----------------------------------------------------------------------

  #[test]
  fn applies_to_returns_false_when_no_rust_ast_present() {
    let audit = empty_audit();
    assert!(!RustExtractor.applies_to(&audit));
  }

  #[test]
  fn applies_to_returns_true_when_a_rust_ast_is_present() {
    let mut audit = empty_audit();
    audit
      .asts
      .insert(project_path("a.rs"), AST::Rust(make_rust_ast("a.rs")));
    assert!(RustExtractor.applies_to(&audit));
  }

  #[test]
  fn applies_to_ignores_solidity_only_audits() {
    let mut audit = empty_audit();
    audit.asts.insert(
      project_path("a.sol"),
      AST::Solidity(crate::solidity::ast::SolidityAST {
        node_id: 0,
        nodes: Vec::new(),
        project_path: project_path("a.sol"),
      }),
    );
    assert!(!RustExtractor.applies_to(&audit));
  }

  // -----------------------------------------------------------------------
  // Skeleton extract — no Rust topics produces no edges
  // -----------------------------------------------------------------------

  #[test]
  fn extract_against_empty_rust_ast_emits_no_edges() {
    let mut audit = empty_audit();
    audit
      .asts
      .insert(project_path("a.rs"), AST::Rust(make_rust_ast("a.rs")));

    let mut graph = ResolutionGraph::new();
    RustExtractor.extract(&audit, &mut graph);
    graph.finalize();

    let edge_count: usize =
      graph.nodes().map(|n| graph.out_edges(n).len()).sum();
    assert_eq!(edge_count, 0);
  }

  // -----------------------------------------------------------------------
  // Polyglot disambiguation: the Rust extractor must not double-emit
  // edges for Solidity-origin topics.
  // -----------------------------------------------------------------------

  #[test]
  fn extract_skips_topics_whose_container_is_a_solidity_file() {
    // A topic in a .sol file is owned by the Solidity extractor; the
    // Rust extractor must not emit an Implements edge against it even
    // when both ASTs are present.
    let mut audit = empty_audit();
    audit
      .asts
      .insert(project_path("a.rs"), AST::Rust(make_rust_ast("a.rs")));
    audit.asts.insert(
      project_path("a.sol"),
      AST::Solidity(crate::solidity::ast::SolidityAST {
        node_id: 0,
        nodes: Vec::new(),
        project_path: project_path("a.sol"),
      }),
    );

    let parent = insert_named(
      &mut audit,
      1,
      NamedTopicKind::Builtin,
      Scope::Component {
        container: project_path("a.sol"),
        component: t(1),
      },
      "Parent",
    );
    let child = insert_named(
      &mut audit,
      2,
      NamedTopicKind::Builtin,
      Scope::Component {
        container: project_path("a.sol"),
        component: t(2),
      },
      "Child",
    );
    audit.inheritance.insert(child, vec![parent]);

    let mut graph = ResolutionGraph::new();
    RustExtractor.extract(&audit, &mut graph);
    graph.finalize();

    let edge_count: usize =
      graph.nodes().map(|n| graph.out_edges(n).len()).sum();
    assert_eq!(
      edge_count, 0,
      "Rust extractor must not emit edges for Solidity-file topics",
    );
  }

  #[test]
  fn extract_emits_implements_for_topics_in_rust_files() {
    // When the inheritance map points at topics whose container is a
    // .rs file, the Rust extractor takes ownership and emits Implements.
    let mut audit = empty_audit();
    audit
      .asts
      .insert(project_path("a.rs"), AST::Rust(make_rust_ast("a.rs")));

    let trait_t = insert_named(
      &mut audit,
      1,
      NamedTopicKind::Builtin,
      Scope::Component {
        container: project_path("a.rs"),
        component: t(1),
      },
      "Trait",
    );
    let impl_t = insert_named(
      &mut audit,
      2,
      NamedTopicKind::Builtin,
      Scope::Component {
        container: project_path("a.rs"),
        component: t(2),
      },
      "Impl",
    );
    audit.inheritance.insert(impl_t, vec![trait_t]);

    let mut graph = ResolutionGraph::new();
    RustExtractor.extract(&audit, &mut graph);
    graph.finalize();

    assert!(
      graph
        .out_edges(impl_t)
        .iter()
        .any(|e| e.dest == trait_t && e.edge_type == EdgeType::Implements)
    );
    assert!(
      graph
        .out_edges(trait_t)
        .iter()
        .any(|e| e.dest == impl_t && e.edge_type == EdgeType::Implements)
    );
  }

  // -----------------------------------------------------------------------
  // Determinism
  // -----------------------------------------------------------------------

  #[test]
  fn extraction_is_deterministic() {
    let mut audit = empty_audit();
    audit
      .asts
      .insert(project_path("a.rs"), AST::Rust(make_rust_ast("a.rs")));
    let trait_t = insert_named(
      &mut audit,
      1,
      NamedTopicKind::Builtin,
      Scope::Component {
        container: project_path("a.rs"),
        component: t(1),
      },
      "Trait",
    );
    let impl_t = insert_named(
      &mut audit,
      2,
      NamedTopicKind::Builtin,
      Scope::Component {
        container: project_path("a.rs"),
        component: t(2),
      },
      "Impl",
    );
    audit.inheritance.insert(impl_t, vec![trait_t]);

    let mut g1 = ResolutionGraph::new();
    RustExtractor.extract(&audit, &mut g1);
    g1.finalize();

    let mut g2 = ResolutionGraph::new();
    RustExtractor.extract(&audit, &mut g2);
    g2.finalize();

    assert_eq!(g1, g2);
    assert_eq!(
      serde_json::to_vec(&g1).unwrap(),
      serde_json::to_vec(&g2).unwrap(),
    );
  }

  #[test]
  fn build_pipeline_includes_rust_extractor() {
    // Builder smoke test: registering RustExtractor in
    // `builder::extractors()` is what makes a Rust-only audit produce a
    // graph at all. Verify by emitting an Implements edge through the
    // top-level `build` entry point.
    let mut audit = empty_audit();
    audit
      .asts
      .insert(project_path("a.rs"), AST::Rust(make_rust_ast("a.rs")));
    let trait_t = insert_named(
      &mut audit,
      1,
      NamedTopicKind::Builtin,
      Scope::Component {
        container: project_path("a.rs"),
        component: t(1),
      },
      "Trait",
    );
    let impl_t = insert_named(
      &mut audit,
      2,
      NamedTopicKind::Builtin,
      Scope::Component {
        container: project_path("a.rs"),
        component: t(2),
      },
      "Impl",
    );
    audit.inheritance.insert(impl_t, vec![trait_t]);

    let graph = super::super::build(&audit);
    let edges: usize = graph.nodes().map(|n| graph.out_edges(n).len()).sum();
    assert!(edges > 0, "RustExtractor must run via build()");
  }
}
