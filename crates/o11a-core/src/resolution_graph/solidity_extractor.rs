//! The Solidity edge extractor.
//!
//! Walks a fully-analyzed `AuditData` and emits every edge described in
//! the universal-core and Solidity-specific tables of
//! `crates/o11a-analyze/docs/specs/semantic-resolution-graph.md`. The
//! extractor is a pure read of `AuditData`; it does not mutate analyzer
//! state and runs no new analysis logic. All inputs are populated by
//! Phase 0 (`inheritance`, `RevertInfo::error_topic`, `events_emitted`)
//! and the existing analyzer passes.
//!
//! Determinism contract (per the build plan's guiding principles):
//!
//! * Every iteration source is a `BTreeMap` (or other ordered structure),
//!   so traversal order is fixed by topic ID.
//! * Per-source duplicate edges are filtered by a `BTreeSet`, so the same
//!   `(src, dst, edge_type)` triple is never emitted twice from one
//!   extractor pass.
//! * Edge weights come from `EdgeType::default_weight` only — extractors
//!   do not hardcode constants.
//! * Edges to non-`NamedTopic` destinations are dropped (the graph
//!   contains one node per `NamedTopic`).

use std::collections::BTreeSet;

use crate::domain::{
  AST, AuditData, FunctionModProperties, NamedTopicKind, Scope, TopicMetadata,
  topic,
};
use crate::solidity::ast::ASTNode;

use super::builder::Extractor;
use super::edge::EdgeType;
use super::graph::ResolutionGraph;

/// Solidity-specific edge extractor. Registered in `builder::extractors()`.
pub struct SolidityExtractor;

impl Extractor for SolidityExtractor {
  fn applies_to(&self, audit_data: &AuditData) -> bool {
    audit_data
      .asts
      .values()
      .any(|ast| matches!(ast, AST::Solidity(_)))
  }

  fn extract(&self, audit_data: &AuditData, graph: &mut ResolutionGraph) {
    let mut emitted: BTreeSet<(topic::Topic, topic::Topic, EdgeType)> =
      BTreeSet::new();

    extract_containment_edges(audit_data, graph, &mut emitted);
    extract_inheritance_edges(audit_data, graph, &mut emitted);
    extract_proxy_of_edges(audit_data, graph, &mut emitted);
    extract_function_property_edges(audit_data, graph, &mut emitted);
    extract_using_for_edges(audit_data, graph, &mut emitted);
    extract_modifier_applied_edges(audit_data, graph, &mut emitted);
  }
}

// ---------------------------------------------------------------------------
// Edge emission helpers
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

/// Look up a topic's `NamedTopicKind`, returning `None` if absent or not
/// a `NamedTopic`.
fn named_kind<'a>(
  audit_data: &'a AuditData,
  t: &topic::Topic,
) -> Option<&'a NamedTopicKind> {
  match audit_data.topic_metadata.get(t)? {
    TopicMetadata::NamedTopic { kind, .. } => Some(kind),
    _ => None,
  }
}

// ---------------------------------------------------------------------------
// Edge-type extractors
// ---------------------------------------------------------------------------

/// `contains-member`, `contains-field`, `contains-local`.
///
/// All three are derived from a topic's `Scope`. The topic's enclosing
/// component is the contract / struct / enum it lives in; the enclosing
/// member is the function / modifier its parameters belong to; the
/// enclosing block is the innermost semantic block its locals live in.
fn extract_containment_edges(
  audit_data: &AuditData,
  graph: &mut ResolutionGraph,
  emitted: &mut BTreeSet<(topic::Topic, topic::Topic, EdgeType)>,
) {
  for (topic, metadata) in &audit_data.topic_metadata {
    let TopicMetadata::NamedTopic { scope, .. } = metadata else {
      continue;
    };
    match scope {
      Scope::Member {
        component,
        member,
        signature_container,
        ..
      } => {
        if topic != component
          && let Some(kind) = named_kind(audit_data, component)
        {
          match kind {
            NamedTopicKind::Contract(_) => add_undirected(
              audit_data,
              graph,
              emitted,
              *topic,
              *component,
              EdgeType::ContainsMember,
            ),
            NamedTopicKind::Struct | NamedTopicKind::Enum => add_undirected(
              audit_data,
              graph,
              emitted,
              *topic,
              *component,
              EdgeType::ContainsField,
            ),
            _ => {}
          }
        }

        // A NamedTopic carrying `signature_container = Some(_)` is a
        // parameter, return value, or modifier specifier — link it back
        // to the enclosing function / modifier.
        if signature_container.is_some() && topic != member {
          add_undirected(
            audit_data,
            graph,
            emitted,
            *topic,
            *member,
            EdgeType::ContainsLocal,
          );
        }
      }
      Scope::ContainingBlock {
        containing_blocks, ..
      } => {
        if let Some(innermost) = containing_blocks.last() {
          // The innermost block is typically an UnnamedTopic
          // (SemanticBlock / annotated block); `add_undirected` will
          // drop the edge in that case. The wiring is kept explicit so
          // that named blocks (when the analyzer ever introduces them)
          // are picked up automatically.
          add_undirected(
            audit_data,
            graph,
            emitted,
            *topic,
            innermost.block,
            EdgeType::ContainsLocal,
          );
        }
      }
      Scope::Component { .. } | Scope::Container { .. } | Scope::Global => {}
    }
  }
}

/// `implements`. Driven by `audit_data.inheritance` (populated in Phase 0
/// from `FirstPassDeclaration::Contract::base_contracts`).
fn extract_inheritance_edges(
  audit_data: &AuditData,
  graph: &mut ResolutionGraph,
  emitted: &mut BTreeSet<(topic::Topic, topic::Topic, EdgeType)>,
) {
  for (child, bases) in &audit_data.inheritance {
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

/// `proxy-of`. Driven by `TopicMetadata::transitive_topic`. The canonical
/// case is an interface member resolving to its single in-scope
/// implementation.
fn extract_proxy_of_edges(
  audit_data: &AuditData,
  graph: &mut ResolutionGraph,
  emitted: &mut BTreeSet<(topic::Topic, topic::Topic, EdgeType)>,
) {
  for (src, metadata) in &audit_data.topic_metadata {
    if !matches!(metadata, TopicMetadata::NamedTopic { .. }) {
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

/// `calls`, `writes-state`, `event-emitted`, `error-thrown`, and the
/// `references` residual.
///
/// `references` deliberately excludes destinations already covered by one
/// of the four typed edges above, per the spec — otherwise the same
/// callee shows up as both a `calls` and a `references` edge from the same
/// source, which double-counts mass during PageRank.
fn extract_function_property_edges(
  audit_data: &AuditData,
  graph: &mut ResolutionGraph,
  emitted: &mut BTreeSet<(topic::Topic, topic::Topic, EdgeType)>,
) {
  for (src, props) in &audit_data.function_properties {
    if !is_named_topic(audit_data, src) {
      continue;
    }

    let (calls, mutations, reverts, events) = match props {
      FunctionModProperties::FunctionProperties {
        calls,
        mutations,
        reverts,
        events_emitted,
      }
      | FunctionModProperties::ModifierProperties {
        calls,
        mutations,
        reverts,
        events_emitted,
      } => (calls, mutations, reverts, events_emitted),
    };

    // Per-source set of destinations covered by a typed edge — used to
    // suppress duplicate `references` edges below.
    let mut covered: BTreeSet<topic::Topic> = BTreeSet::new();

    for callee in calls {
      add_directed(audit_data, graph, emitted, *src, *callee, EdgeType::Calls);
      covered.insert(*callee);
    }
    for state_var in mutations {
      add_directed(
        audit_data,
        graph,
        emitted,
        *src,
        *state_var,
        EdgeType::WritesState,
      );
      covered.insert(*state_var);
    }
    for event in events {
      add_directed(
        audit_data,
        graph,
        emitted,
        *src,
        *event,
        EdgeType::EventEmitted,
      );
      covered.insert(*event);
    }
    for r in reverts {
      let Some(error_topic) = r.error_topic else {
        continue;
      };
      add_directed(
        audit_data,
        graph,
        emitted,
        *src,
        error_topic,
        EdgeType::ErrorThrown,
      );
      covered.insert(error_topic);
    }

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

/// `using-for`. Walks every Solidity AST for `UsingForDirective` nodes,
/// then emits an undirected edge between the affected type's topic and
/// the library's topic.
fn extract_using_for_edges(
  audit_data: &AuditData,
  graph: &mut ResolutionGraph,
  emitted: &mut BTreeSet<(topic::Topic, topic::Topic, EdgeType)>,
) {
  for ast in audit_data.asts.values() {
    if let AST::Solidity(sol) = ast {
      for node in &sol.nodes {
        walk_for_using_for(node, audit_data, graph, emitted);
      }
    }
  }
}

fn walk_for_using_for(
  node: &ASTNode,
  audit_data: &AuditData,
  graph: &mut ResolutionGraph,
  emitted: &mut BTreeSet<(topic::Topic, topic::Topic, EdgeType)>,
) {
  if let ASTNode::UsingForDirective {
    library_name,
    type_name,
    ..
  } = node
  {
    let lib_topic = library_name.as_deref().and_then(referenced_topic_of);
    let type_topic = type_name.as_deref().and_then(referenced_topic_of);
    if let (Some(lib), Some(ty)) = (lib_topic, type_topic) {
      add_undirected(
        audit_data,
        graph,
        emitted,
        ty,
        lib,
        EdgeType::UsingFor,
      );
    }
  }

  // Recurse only into containers that can host directives. This is
  // intentionally not a full AST walk — UsingForDirective only ever
  // appears at file scope or inside a contract signature.
  match node {
    ASTNode::SourceUnit { nodes, .. } => {
      for child in nodes {
        walk_for_using_for(child, audit_data, graph, emitted);
      }
    }
    ASTNode::ContractDefinition {
      signature, nodes, ..
    } => {
      walk_for_using_for(signature, audit_data, graph, emitted);
      for child in nodes {
        walk_for_using_for(child, audit_data, graph, emitted);
      }
    }
    ASTNode::ContractSignature { directives, .. } => {
      for d in directives {
        walk_for_using_for(d, audit_data, graph, emitted);
      }
    }
    ASTNode::ContractMemberGroup { members, .. } => {
      for m in members {
        walk_for_using_for(m, audit_data, graph, emitted);
      }
    }
    _ => {}
  }
}

/// `modifier-applied`. Walks every Solidity AST for `FunctionDefinition`
/// nodes, then emits an undirected edge between the function and each
/// applied modifier.
fn extract_modifier_applied_edges(
  audit_data: &AuditData,
  graph: &mut ResolutionGraph,
  emitted: &mut BTreeSet<(topic::Topic, topic::Topic, EdgeType)>,
) {
  for ast in audit_data.asts.values() {
    if let AST::Solidity(sol) = ast {
      for node in &sol.nodes {
        walk_for_modifiers(node, audit_data, graph, emitted);
      }
    }
  }
}

fn walk_for_modifiers(
  node: &ASTNode,
  audit_data: &AuditData,
  graph: &mut ResolutionGraph,
  emitted: &mut BTreeSet<(topic::Topic, topic::Topic, EdgeType)>,
) {
  if let ASTNode::FunctionDefinition {
    node_id, signature, ..
  } = node
    && let ASTNode::FunctionSignature { modifiers, .. } = signature.as_ref()
    && let ASTNode::ModifierList {
      modifiers: invocations,
      ..
    } = modifiers.as_ref()
  {
    let function_topic = topic::new_node_topic(node_id);
    for inv in invocations {
      if let ASTNode::ModifierInvocation { modifier_name, .. } = inv
        && let Some(mod_topic) = referenced_topic_of(modifier_name)
      {
        add_undirected(
          audit_data,
          graph,
          emitted,
          function_topic,
          mod_topic,
          EdgeType::ModifierApplied,
        );
      }
    }
  }

  match node {
    ASTNode::SourceUnit { nodes, .. } => {
      for child in nodes {
        walk_for_modifiers(child, audit_data, graph, emitted);
      }
    }
    ASTNode::ContractDefinition { nodes, .. } => {
      for child in nodes {
        walk_for_modifiers(child, audit_data, graph, emitted);
      }
    }
    ASTNode::ContractMemberGroup { members, .. } => {
      for m in members {
        walk_for_modifiers(m, audit_data, graph, emitted);
      }
    }
    _ => {}
  }
}

/// Extract the referenced declaration's topic from a name-bearing AST
/// node. Returns `None` for nodes that do not name a declaration (e.g.
/// elementary type names).
fn referenced_topic_of(node: &ASTNode) -> Option<topic::Topic> {
  match node {
    ASTNode::Identifier {
      referenced_declaration,
      ..
    }
    | ASTNode::IdentifierPath {
      referenced_declaration,
      ..
    }
    | ASTNode::UserDefinedTypeName {
      referenced_declaration,
      ..
    } => Some(topic::new_node_topic(referenced_declaration)),
    ASTNode::MemberAccess {
      referenced_declaration: Some(id),
      ..
    } => Some(topic::new_node_topic(id)),
    _ => None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  use crate::domain::{
    ContractKind, FunctionKind, NamedTopicVisibility, ProjectPath, Reference,
    RevertConstraintKind, RevertInfo, SourceContext, VariableMutability,
    new_audit_data,
  };
  use crate::solidity::ast::{
    FunctionStateMutability, FunctionVisibility, SolidityAST, SourceLocation,
  };
  use std::collections::HashSet;

  // -----------------------------------------------------------------------
  // Test helpers
  // -----------------------------------------------------------------------

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

  fn insert_named(
    audit: &mut AuditData,
    id: i32,
    kind: NamedTopicKind,
    scope: Scope,
    name: &str,
    transitive_topic: Option<topic::Topic>,
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
        transitive_topic,
        doc_references: Vec::new(),
      },
    );
    topic
  }

  fn insert_unnamed(
    audit: &mut AuditData,
    id: i32,
    scope: Scope,
    kind: crate::domain::UnnamedTopicKind,
  ) -> topic::Topic {
    let topic = t(id);
    audit.topic_metadata.insert(
      topic,
      TopicMetadata::UnnamedTopic {
        topic,
        scope,
        kind,
        transitive_topic: None,
      },
    );
    topic
  }

  fn make_identifier(node_id: i32, name: &str, ref_decl: i32) -> ASTNode {
    ASTNode::Identifier {
      node_id,
      src_location: loc(),
      name: name.to_string(),
      overloaded_declarations: Vec::new(),
      referenced_declaration: ref_decl,
    }
  }

  fn make_user_defined_type_name(node_id: i32, ref_decl: i32) -> ASTNode {
    ASTNode::UserDefinedTypeName {
      node_id,
      src_location: loc(),
      path_node: Box::new(make_identifier(node_id + 1, "Path", ref_decl)),
      referenced_declaration: ref_decl,
    }
  }

  /// Build a minimal `SolidityAST` whose top-level `nodes` are the given
  /// declarations. Mirrors the Foundry-AST shape closely enough that the
  /// extractor's walker traverses it correctly.
  fn make_solidity_ast(file: &str, nodes: Vec<ASTNode>) -> SolidityAST {
    SolidityAST {
      node_id: 0,
      nodes,
      project_path: project_path(file),
    }
  }

  fn make_contract_def(
    contract_id: i32,
    name: &str,
    contract_kind: ContractKind,
    directives: Vec<ASTNode>,
    body: Vec<ASTNode>,
  ) -> ASTNode {
    let signature = ASTNode::ContractSignature {
      node_id: contract_id + 100_000,
      src_location: loc(),
      documentation: None,
      name: name.to_string(),
      name_location: loc(),
      declaration_id: contract_id,
      contract_kind,
      abstract_: false,
      base_contracts: Vec::new(),
      directives,
    };
    ASTNode::ContractDefinition {
      node_id: contract_id,
      src_location: loc(),
      signature: Box::new(signature),
      nodes: body,
    }
  }

  fn make_function_def(
    function_id: i32,
    name: &str,
    modifier_invocations: Vec<ASTNode>,
  ) -> ASTNode {
    let modifiers = ASTNode::ModifierList {
      node_id: function_id + 200_000,
      src_location: loc(),
      modifiers: modifier_invocations,
    };
    let parameters = ASTNode::ParameterList {
      node_id: function_id + 300_000,
      src_location: loc(),
      parameters: Vec::new(),
      is_return_parameters: false,
    };
    let return_parameters = ASTNode::ParameterList {
      node_id: function_id + 400_000,
      src_location: loc(),
      parameters: Vec::new(),
      is_return_parameters: true,
    };
    let signature = ASTNode::FunctionSignature {
      node_id: function_id + 500_000,
      src_location: loc(),
      documentation: None,
      kind: FunctionKind::Function,
      modifiers: Box::new(modifiers),
      name: name.to_string(),
      name_location: loc(),
      declaration_id: function_id,
      parameters: Box::new(parameters),
      return_parameters: Box::new(return_parameters),
      scope: 0,
      state_mutability: FunctionStateMutability::NonPayable,
      virtual_: false,
      visibility: FunctionVisibility::Public,
      implementation_declaration: None,
    };
    ASTNode::FunctionDefinition {
      node_id: function_id,
      src_location: loc(),
      signature: Box::new(signature),
      implemented: true,
      body: None,
    }
  }

  fn make_modifier_invocation(node_id: i32, modifier_id: i32) -> ASTNode {
    ASTNode::ModifierInvocation {
      node_id,
      src_location: loc(),
      modifier_name: Box::new(make_identifier(
        node_id + 10_000,
        "Mod",
        modifier_id,
      )),
      arguments: None,
    }
  }

  fn make_using_for(
    node_id: i32,
    library_id: i32,
    type_id: i32,
  ) -> ASTNode {
    ASTNode::UsingForDirective {
      node_id,
      src_location: loc(),
      global: false,
      library_name: Some(Box::new(make_identifier(
        node_id + 1000,
        "Lib",
        library_id,
      ))),
      type_name: Some(Box::new(make_user_defined_type_name(
        node_id + 2000,
        type_id,
      ))),
    }
  }

  /// Convenience: install a contract topic at the given scope.
  fn insert_contract(audit: &mut AuditData, id: i32, name: &str) -> topic::Topic {
    insert_named(
      audit,
      id,
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Component {
        container: project_path("test.sol"),
        component: t(id),
      },
      name,
      None,
    )
  }

  fn insert_function(
    audit: &mut AuditData,
    id: i32,
    name: &str,
    contract_topic: topic::Topic,
  ) -> topic::Topic {
    insert_named(
      audit,
      id,
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Member {
        container: project_path("test.sol"),
        component: contract_topic,
        member: t(id),
        signature_container: None,
      },
      name,
      None,
    )
  }

  // -----------------------------------------------------------------------
  // applies_to
  // -----------------------------------------------------------------------

  #[test]
  fn applies_to_returns_false_for_audit_with_no_solidity_ast() {
    let audit = empty_audit();
    assert!(!SolidityExtractor.applies_to(&audit));
  }

  #[test]
  fn applies_to_returns_true_when_any_solidity_ast_present() {
    let mut audit = empty_audit();
    audit.asts.insert(
      project_path("a.sol"),
      AST::Solidity(make_solidity_ast("a.sol", Vec::new())),
    );
    assert!(SolidityExtractor.applies_to(&audit));
  }

  // -----------------------------------------------------------------------
  // contains-member / contains-field
  // -----------------------------------------------------------------------

  #[test]
  fn member_in_contract_emits_contains_member_undirected() {
    let mut audit = empty_audit();
    let contract = insert_contract(&mut audit, 1, "Foo");
    let _state_var = insert_named(
      &mut audit,
      2,
      NamedTopicKind::StateVariable(VariableMutability::Mutable),
      Scope::Member {
        container: project_path("test.sol"),
        component: contract,
        member: t(2),
        signature_container: None,
      },
      "x",
      None,
    );

    let mut graph = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut graph);
    graph.finalize();

    // Both directions present.
    assert!(
      graph
        .out_edges(t(2))
        .iter()
        .any(|e| e.dest == contract && e.edge_type == EdgeType::ContainsMember)
    );
    assert!(
      graph
        .out_edges(contract)
        .iter()
        .any(|e| e.dest == t(2) && e.edge_type == EdgeType::ContainsMember)
    );
  }

  #[test]
  fn field_in_struct_emits_contains_field_undirected() {
    let mut audit = empty_audit();
    let contract = insert_contract(&mut audit, 1, "Foo");
    let struct_topic = insert_named(
      &mut audit,
      2,
      NamedTopicKind::Struct,
      Scope::Member {
        container: project_path("test.sol"),
        component: contract,
        member: t(2),
        signature_container: None,
      },
      "S",
      None,
    );
    let _field = insert_named(
      &mut audit,
      3,
      NamedTopicKind::LocalVariable,
      Scope::Member {
        container: project_path("test.sol"),
        component: struct_topic,
        member: t(3),
        signature_container: None,
      },
      "field",
      None,
    );

    let mut graph = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut graph);
    graph.finalize();

    assert!(
      graph
        .out_edges(t(3))
        .iter()
        .any(|e| e.dest == struct_topic && e.edge_type == EdgeType::ContainsField)
    );
    assert!(
      graph
        .out_edges(struct_topic)
        .iter()
        .any(|e| e.dest == t(3) && e.edge_type == EdgeType::ContainsField)
    );
  }

  #[test]
  fn contains_member_uses_default_weight() {
    let mut audit = empty_audit();
    let contract = insert_contract(&mut audit, 1, "Foo");
    let _state_var = insert_named(
      &mut audit,
      2,
      NamedTopicKind::StateVariable(VariableMutability::Mutable),
      Scope::Member {
        container: project_path("test.sol"),
        component: contract,
        member: t(2),
        signature_container: None,
      },
      "x",
      None,
    );

    let mut graph = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut graph);
    graph.finalize();

    let edge = graph
      .out_edges(t(2))
      .iter()
      .find(|e| e.dest == contract && e.edge_type == EdgeType::ContainsMember)
      .unwrap();
    assert_eq!(edge.weight, EdgeType::ContainsMember.default_weight());
  }

  #[test]
  fn contract_self_member_does_not_self_loop() {
    // A contract topic at Component scope has component = topic-itself.
    // The extractor must not emit a self-loop ContainsMember edge.
    let mut audit = empty_audit();
    let contract = insert_contract(&mut audit, 1, "Foo");

    let mut graph = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut graph);
    graph.finalize();

    assert!(graph.out_edges(contract).iter().all(|e| e.dest != contract));
  }

  // -----------------------------------------------------------------------
  // contains-local
  // -----------------------------------------------------------------------

  #[test]
  fn parameter_with_signature_container_emits_contains_local_to_member() {
    let mut audit = empty_audit();
    let contract = insert_contract(&mut audit, 1, "Foo");
    let function = insert_function(&mut audit, 2, "f", contract);
    // Parameter inside the function's parameter list. signature_container
    // is set, so the extractor must emit a ContainsLocal edge to the
    // function topic (the immediate enclosing member).
    let _param = insert_named(
      &mut audit,
      3,
      NamedTopicKind::LocalVariable,
      Scope::Member {
        container: project_path("test.sol"),
        component: contract,
        member: function,
        signature_container: Some(t(99)),
      },
      "x",
      None,
    );

    let mut graph = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut graph);
    graph.finalize();

    assert!(
      graph
        .out_edges(t(3))
        .iter()
        .any(|e| e.dest == function && e.edge_type == EdgeType::ContainsLocal)
    );
    assert!(
      graph
        .out_edges(function)
        .iter()
        .any(|e| e.dest == t(3) && e.edge_type == EdgeType::ContainsLocal)
    );
  }

  #[test]
  fn member_without_signature_container_does_not_emit_contains_local() {
    // State variables, events, etc. live in Member scope but without a
    // signature_container. They must not produce ContainsLocal edges.
    let mut audit = empty_audit();
    let contract = insert_contract(&mut audit, 1, "Foo");
    let _state_var = insert_named(
      &mut audit,
      2,
      NamedTopicKind::StateVariable(VariableMutability::Mutable),
      Scope::Member {
        container: project_path("test.sol"),
        component: contract,
        member: t(2),
        signature_container: None,
      },
      "x",
      None,
    );

    let mut graph = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut graph);
    graph.finalize();

    assert!(
      graph
        .out_edges(t(2))
        .iter()
        .all(|e| e.edge_type != EdgeType::ContainsLocal)
    );
  }

  #[test]
  fn containing_block_local_to_unnamed_block_is_dropped() {
    // The innermost block in a ContainingBlock scope is typically an
    // UnnamedTopic (SemanticBlock). Edges to non-NamedTopic destinations
    // must be silently dropped per the spec's "Node set" section.
    let mut audit = empty_audit();
    let contract = insert_contract(&mut audit, 1, "Foo");
    let function = insert_function(&mut audit, 2, "f", contract);
    let block = insert_unnamed(
      &mut audit,
      3,
      Scope::Member {
        container: project_path("test.sol"),
        component: contract,
        member: function,
        signature_container: None,
      },
      crate::domain::UnnamedTopicKind::SemanticBlock,
    );
    let _local = insert_named(
      &mut audit,
      4,
      NamedTopicKind::LocalVariable,
      Scope::ContainingBlock {
        container: project_path("test.sol"),
        component: contract,
        member: function,
        containing_blocks: vec![crate::domain::ContainingBlockLayer {
          block,
          annotation: None,
        }],
      },
      "y",
      None,
    );

    let mut graph = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut graph);
    graph.finalize();

    assert!(graph.out_edges(t(4)).iter().all(|e| e.dest != block));
  }

  // -----------------------------------------------------------------------
  // implements
  // -----------------------------------------------------------------------

  #[test]
  fn inheritance_emits_implements_undirected_for_each_base() {
    let mut audit = empty_audit();
    let child = insert_contract(&mut audit, 1, "Child");
    let base_a = insert_contract(&mut audit, 2, "BaseA");
    let base_b = insert_contract(&mut audit, 3, "BaseB");
    audit.inheritance.insert(child, vec![base_a, base_b]);

    let mut graph = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut graph);
    graph.finalize();

    for base in [base_a, base_b] {
      assert!(
        graph
          .out_edges(child)
          .iter()
          .any(|e| e.dest == base && e.edge_type == EdgeType::Implements)
      );
      assert!(
        graph
          .out_edges(base)
          .iter()
          .any(|e| e.dest == child && e.edge_type == EdgeType::Implements)
      );
    }
  }

  // -----------------------------------------------------------------------
  // proxy-of
  // -----------------------------------------------------------------------

  #[test]
  fn transitive_topic_emits_directed_proxy_of_edge() {
    let mut audit = empty_audit();
    let interface = insert_named(
      &mut audit,
      1,
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Member {
        container: project_path("test.sol"),
        component: t(99),
        member: t(1),
        signature_container: None,
      },
      "iface_fn",
      Some(t(2)),
    );
    let _impl_topic = insert_named(
      &mut audit,
      2,
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Member {
        container: project_path("test.sol"),
        component: t(98),
        member: t(2),
        signature_container: None,
      },
      "impl_fn",
      None,
    );

    let mut graph = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut graph);
    graph.finalize();

    assert!(
      graph
        .out_edges(interface)
        .iter()
        .any(|e| e.dest == t(2) && e.edge_type == EdgeType::ProxyOf)
    );
    // Directed: t(2) → interface should NOT exist.
    assert!(
      graph
        .out_edges(t(2))
        .iter()
        .all(|e| !(e.dest == interface && e.edge_type == EdgeType::ProxyOf))
    );
  }

  // -----------------------------------------------------------------------
  // calls / writes-state / event-emitted / error-thrown / references
  // -----------------------------------------------------------------------

  fn insert_function_props(
    audit: &mut AuditData,
    function_topic: topic::Topic,
    calls: Vec<topic::Topic>,
    mutations: Vec<topic::Topic>,
    events: Vec<topic::Topic>,
    reverts: Vec<RevertInfo>,
  ) {
    audit.function_properties.insert(
      function_topic,
      FunctionModProperties::FunctionProperties {
        reverts,
        calls,
        mutations,
        events_emitted: events,
      },
    );
  }

  #[test]
  fn function_calls_emit_directed_calls_edges() {
    let mut audit = empty_audit();
    let contract = insert_contract(&mut audit, 1, "C");
    let f = insert_function(&mut audit, 2, "f", contract);
    let g = insert_function(&mut audit, 3, "g", contract);
    insert_function_props(&mut audit, f, vec![g], vec![], vec![], vec![]);

    let mut graph = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut graph);
    graph.finalize();

    assert!(
      graph
        .out_edges(f)
        .iter()
        .any(|e| e.dest == g && e.edge_type == EdgeType::Calls)
    );
    // Directed: no reverse edge.
    assert!(
      graph
        .out_edges(g)
        .iter()
        .all(|e| !(e.dest == f && e.edge_type == EdgeType::Calls))
    );
  }

  #[test]
  fn duplicate_calls_dedupe_within_one_source() {
    let mut audit = empty_audit();
    let contract = insert_contract(&mut audit, 1, "C");
    let f = insert_function(&mut audit, 2, "f", contract);
    let g = insert_function(&mut audit, 3, "g", contract);
    // Same callee listed three times — the analyzer records each call
    // site, but the graph must end up with one Calls edge.
    insert_function_props(
      &mut audit,
      f,
      vec![g, g, g],
      vec![],
      vec![],
      vec![],
    );

    let mut graph = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut graph);
    graph.finalize();

    let calls_to_g = graph
      .out_edges(f)
      .iter()
      .filter(|e| e.dest == g && e.edge_type == EdgeType::Calls)
      .count();
    assert_eq!(calls_to_g, 1);
  }

  #[test]
  fn mutations_emit_directed_writes_state_edges() {
    let mut audit = empty_audit();
    let contract = insert_contract(&mut audit, 1, "C");
    let f = insert_function(&mut audit, 2, "f", contract);
    let state_var = insert_named(
      &mut audit,
      3,
      NamedTopicKind::StateVariable(VariableMutability::Mutable),
      Scope::Member {
        container: project_path("test.sol"),
        component: contract,
        member: t(3),
        signature_container: None,
      },
      "x",
      None,
    );
    insert_function_props(
      &mut audit,
      f,
      vec![],
      vec![state_var],
      vec![],
      vec![],
    );

    let mut graph = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut graph);
    graph.finalize();

    assert!(
      graph
        .out_edges(f)
        .iter()
        .any(|e| e.dest == state_var && e.edge_type == EdgeType::WritesState)
    );
  }

  #[test]
  fn events_emit_directed_event_emitted_edges() {
    let mut audit = empty_audit();
    let contract = insert_contract(&mut audit, 1, "C");
    let f = insert_function(&mut audit, 2, "f", contract);
    let event = insert_named(
      &mut audit,
      3,
      NamedTopicKind::Event,
      Scope::Member {
        container: project_path("test.sol"),
        component: contract,
        member: t(3),
        signature_container: None,
      },
      "Pinged",
      None,
    );
    insert_function_props(&mut audit, f, vec![], vec![], vec![event], vec![]);

    let mut graph = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut graph);
    graph.finalize();

    assert!(
      graph
        .out_edges(f)
        .iter()
        .any(|e| e.dest == event && e.edge_type == EdgeType::EventEmitted)
    );
  }

  #[test]
  fn revert_with_error_topic_emits_error_thrown_edge() {
    let mut audit = empty_audit();
    let contract = insert_contract(&mut audit, 1, "C");
    let f = insert_function(&mut audit, 2, "f", contract);
    let err = insert_named(
      &mut audit,
      3,
      NamedTopicKind::Error,
      Scope::Member {
        container: project_path("test.sol"),
        component: contract,
        member: t(3),
        signature_container: None,
      },
      "BadStuff",
      None,
    );
    insert_function_props(
      &mut audit,
      f,
      vec![],
      vec![],
      vec![],
      vec![RevertInfo {
        topic: t(99),
        kind: RevertConstraintKind::Revert,
        error_topic: Some(err),
      }],
    );

    let mut graph = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut graph);
    graph.finalize();

    assert!(
      graph
        .out_edges(f)
        .iter()
        .any(|e| e.dest == err && e.edge_type == EdgeType::ErrorThrown)
    );
  }

  #[test]
  fn revert_without_error_topic_emits_no_error_thrown_edge() {
    // require(cond, "string") and bare revert("string") have no error
    // topic; they must not produce an ErrorThrown edge.
    let mut audit = empty_audit();
    let contract = insert_contract(&mut audit, 1, "C");
    let f = insert_function(&mut audit, 2, "f", contract);
    insert_function_props(
      &mut audit,
      f,
      vec![],
      vec![],
      vec![],
      vec![RevertInfo {
        topic: t(99),
        kind: RevertConstraintKind::Require,
        error_topic: None,
      }],
    );

    let mut graph = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut graph);
    graph.finalize();

    assert!(
      graph
        .out_edges(f)
        .iter()
        .all(|e| e.edge_type != EdgeType::ErrorThrown)
    );
  }

  #[test]
  fn references_emits_only_for_uncovered_destinations() {
    // A reference to the same callee should NOT produce a References
    // edge in addition to the Calls edge — the spec deduplicates against
    // calls / writes-state / event-emitted / error-thrown.
    let mut audit = empty_audit();
    let contract = insert_contract(&mut audit, 1, "C");
    let f = insert_function(&mut audit, 2, "f", contract);
    let g = insert_function(&mut audit, 3, "g", contract);
    let plain_ref = insert_function(&mut audit, 4, "h", contract);
    insert_function_props(&mut audit, f, vec![g], vec![], vec![], vec![]);

    audit.topic_context.insert(
      f,
      vec![SourceContext::new_with_scope_references(
        contract,
        Some(0),
        true,
        vec![
          Reference::project_reference(g, Some(0)),
          Reference::project_reference(plain_ref, Some(1)),
        ],
      )],
    );

    let mut graph = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut graph);
    graph.finalize();

    let f_edges = graph.out_edges(f);
    // g: covered by Calls; no References edge.
    assert!(
      f_edges
        .iter()
        .all(|e| !(e.dest == g && e.edge_type == EdgeType::References))
    );
    // plain_ref: only a References edge, no Calls edge.
    assert!(
      f_edges
        .iter()
        .any(|e| e.dest == plain_ref && e.edge_type == EdgeType::References)
    );
  }

  #[test]
  fn references_dedupe_repeated_targets() {
    // Two contexts referencing the same topic — the graph must end up
    // with one References edge, not two.
    let mut audit = empty_audit();
    let contract = insert_contract(&mut audit, 1, "C");
    let f = insert_function(&mut audit, 2, "f", contract);
    let referent = insert_function(&mut audit, 3, "g", contract);
    insert_function_props(&mut audit, f, vec![], vec![], vec![], vec![]);

    audit.topic_context.insert(
      f,
      vec![
        SourceContext::new_with_scope_references(
          contract,
          Some(0),
          true,
          vec![Reference::project_reference(referent, Some(0))],
        ),
        SourceContext::new_with_scope_references(
          contract,
          Some(1),
          true,
          vec![Reference::project_reference(referent, Some(1))],
        ),
      ],
    );

    let mut graph = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut graph);
    graph.finalize();

    let count = graph
      .out_edges(f)
      .iter()
      .filter(|e| e.dest == referent && e.edge_type == EdgeType::References)
      .count();
    assert_eq!(count, 1);
  }

  // -----------------------------------------------------------------------
  // using-for
  // -----------------------------------------------------------------------

  #[test]
  fn using_for_in_contract_signature_emits_undirected_edge() {
    let mut audit = empty_audit();
    let library = insert_named(
      &mut audit,
      10,
      NamedTopicKind::Contract(ContractKind::Library),
      Scope::Component {
        container: project_path("test.sol"),
        component: t(10),
      },
      "Math",
      None,
    );
    let value_type = insert_named(
      &mut audit,
      20,
      NamedTopicKind::Struct,
      Scope::Component {
        container: project_path("test.sol"),
        component: t(20),
      },
      "MyType",
      None,
    );
    let _contract = insert_contract(&mut audit, 1, "C");
    let using_for = make_using_for(900, library.numeric_id(), value_type.numeric_id());
    let contract = make_contract_def(
      1,
      "C",
      ContractKind::Contract,
      vec![using_for],
      Vec::new(),
    );
    audit.asts.insert(
      project_path("test.sol"),
      AST::Solidity(make_solidity_ast("test.sol", vec![contract])),
    );

    let mut graph = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut graph);
    graph.finalize();

    assert!(
      graph
        .out_edges(value_type)
        .iter()
        .any(|e| e.dest == library && e.edge_type == EdgeType::UsingFor)
    );
    assert!(
      graph
        .out_edges(library)
        .iter()
        .any(|e| e.dest == value_type && e.edge_type == EdgeType::UsingFor)
    );
  }

  #[test]
  fn using_for_at_source_unit_level_is_picked_up() {
    let mut audit = empty_audit();
    let library = insert_named(
      &mut audit,
      10,
      NamedTopicKind::Contract(ContractKind::Library),
      Scope::Component {
        container: project_path("test.sol"),
        component: t(10),
      },
      "Math",
      None,
    );
    let value_type = insert_named(
      &mut audit,
      20,
      NamedTopicKind::Struct,
      Scope::Component {
        container: project_path("test.sol"),
        component: t(20),
      },
      "MyType",
      None,
    );

    let source_unit = ASTNode::SourceUnit {
      node_id: 50,
      src_location: loc(),
      nodes: vec![make_using_for(
        900,
        library.numeric_id(),
        value_type.numeric_id(),
      )],
    };
    audit.asts.insert(
      project_path("test.sol"),
      AST::Solidity(make_solidity_ast("test.sol", vec![source_unit])),
    );

    let mut graph = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut graph);
    graph.finalize();

    assert!(
      graph
        .out_edges(value_type)
        .iter()
        .any(|e| e.dest == library && e.edge_type == EdgeType::UsingFor)
    );
  }

  // -----------------------------------------------------------------------
  // modifier-applied
  // -----------------------------------------------------------------------

  #[test]
  fn modifier_invocation_emits_modifier_applied_undirected_edge() {
    let mut audit = empty_audit();
    let contract = insert_contract(&mut audit, 1, "C");
    let modifier = insert_named(
      &mut audit,
      5,
      NamedTopicKind::Modifier,
      Scope::Member {
        container: project_path("test.sol"),
        component: contract,
        member: t(5),
        signature_container: None,
      },
      "onlyOwner",
      None,
    );
    let function = insert_function(&mut audit, 2, "f", contract);

    let inv = make_modifier_invocation(700, modifier.numeric_id());
    let function_def = make_function_def(2, "f", vec![inv]);
    let contract_def = make_contract_def(
      1,
      "C",
      ContractKind::Contract,
      Vec::new(),
      vec![function_def],
    );
    audit.asts.insert(
      project_path("test.sol"),
      AST::Solidity(make_solidity_ast("test.sol", vec![contract_def])),
    );

    let mut graph = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut graph);
    graph.finalize();

    assert!(
      graph
        .out_edges(function)
        .iter()
        .any(|e| e.dest == modifier && e.edge_type == EdgeType::ModifierApplied)
    );
    assert!(
      graph
        .out_edges(modifier)
        .iter()
        .any(|e| e.dest == function && e.edge_type == EdgeType::ModifierApplied)
    );
  }

  #[test]
  fn modifier_applied_dedupes_when_same_function_visited_twice() {
    // Construct an AST where the function appears inside both the
    // ContractDefinition.body and (defensively) inside a
    // ContractMemberGroup. Both visits should leave one edge in the
    // graph, not two.
    let mut audit = empty_audit();
    let contract = insert_contract(&mut audit, 1, "C");
    let modifier = insert_named(
      &mut audit,
      5,
      NamedTopicKind::Modifier,
      Scope::Member {
        container: project_path("test.sol"),
        component: contract,
        member: t(5),
        signature_container: None,
      },
      "onlyOwner",
      None,
    );
    let function = insert_function(&mut audit, 2, "f", contract);

    let function_def_a = make_function_def(
      2,
      "f",
      vec![make_modifier_invocation(700, modifier.numeric_id())],
    );
    let function_def_b = make_function_def(
      2,
      "f",
      vec![make_modifier_invocation(701, modifier.numeric_id())],
    );
    let group = ASTNode::ContractMemberGroup {
      node_id: 8000,
      src_location: loc(),
      documentation: None,
      members: vec![function_def_b],
    };
    let contract_def = make_contract_def(
      1,
      "C",
      ContractKind::Contract,
      Vec::new(),
      vec![function_def_a, group],
    );
    audit.asts.insert(
      project_path("test.sol"),
      AST::Solidity(make_solidity_ast("test.sol", vec![contract_def])),
    );

    let mut graph = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut graph);
    graph.finalize();

    let count = graph
      .out_edges(function)
      .iter()
      .filter(|e| e.dest == modifier && e.edge_type == EdgeType::ModifierApplied)
      .count();
    assert_eq!(count, 1);
  }

  // -----------------------------------------------------------------------
  // Filter: edges to non-NamedTopic destinations
  // -----------------------------------------------------------------------

  #[test]
  fn edges_to_unknown_topics_are_dropped() {
    // Calls listing a topic that is absent from topic_metadata must not
    // appear in the graph.
    let mut audit = empty_audit();
    let contract = insert_contract(&mut audit, 1, "C");
    let f = insert_function(&mut audit, 2, "f", contract);
    let unknown = t(999);
    insert_function_props(&mut audit, f, vec![unknown], vec![], vec![], vec![]);

    let mut graph = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut graph);
    graph.finalize();

    assert!(graph.out_edges(f).iter().all(|e| e.dest != unknown));
  }

  // -----------------------------------------------------------------------
  // Determinism
  // -----------------------------------------------------------------------

  /// Builds a small audit exercising every spec-defined edge type. Used
  /// by both the determinism test and the all-edge-types-present test.
  fn integrated_audit() -> AuditData {
    let mut audit = empty_audit();
    let contract = insert_contract(&mut audit, 1, "Token");
    let base = insert_contract(&mut audit, 7, "BaseToken");
    audit.inheritance.insert(contract, vec![base]);

    let state_var = insert_named(
      &mut audit,
      2,
      NamedTopicKind::StateVariable(VariableMutability::Mutable),
      Scope::Member {
        container: project_path("token.sol"),
        component: contract,
        member: t(2),
        signature_container: None,
      },
      "balance",
      None,
    );
    let event = insert_named(
      &mut audit,
      3,
      NamedTopicKind::Event,
      Scope::Member {
        container: project_path("token.sol"),
        component: contract,
        member: t(3),
        signature_container: None,
      },
      "Transferred",
      None,
    );
    let err = insert_named(
      &mut audit,
      4,
      NamedTopicKind::Error,
      Scope::Member {
        container: project_path("token.sol"),
        component: contract,
        member: t(4),
        signature_container: None,
      },
      "InsufficientBalance",
      None,
    );
    let helper = insert_function(&mut audit, 5, "helper", contract);
    let main = insert_function(&mut audit, 6, "transfer", contract);
    let modifier = insert_named(
      &mut audit,
      8,
      NamedTopicKind::Modifier,
      Scope::Member {
        container: project_path("token.sol"),
        component: contract,
        member: t(8),
        signature_container: None,
      },
      "onlyOwner",
      None,
    );
    // Parameter to enable a contains-local edge.
    let _param = insert_named(
      &mut audit,
      9,
      NamedTopicKind::LocalVariable,
      Scope::Member {
        container: project_path("token.sol"),
        component: contract,
        member: main,
        signature_container: Some(t(9999)),
      },
      "amount",
      None,
    );
    // Struct + field — for contains-field.
    let s = insert_named(
      &mut audit,
      11,
      NamedTopicKind::Struct,
      Scope::Member {
        container: project_path("token.sol"),
        component: contract,
        member: t(11),
        signature_container: None,
      },
      "Holder",
      None,
    );
    let _field = insert_named(
      &mut audit,
      12,
      NamedTopicKind::LocalVariable,
      Scope::Member {
        container: project_path("token.sol"),
        component: s,
        member: t(12),
        signature_container: None,
      },
      "addr",
      None,
    );
    // Library + value type for using-for.
    let library = insert_named(
      &mut audit,
      13,
      NamedTopicKind::Contract(ContractKind::Library),
      Scope::Component {
        container: project_path("token.sol"),
        component: t(13),
      },
      "Math",
      None,
    );
    let value_type = insert_named(
      &mut audit,
      14,
      NamedTopicKind::Struct,
      Scope::Member {
        container: project_path("token.sol"),
        component: contract,
        member: t(14),
        signature_container: None,
      },
      "Amount",
      None,
    );
    // Interface member that is transitive to its implementation — for proxy-of.
    let iface_fn = insert_named(
      &mut audit,
      15,
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Member {
        container: project_path("token.sol"),
        component: contract,
        member: t(15),
        signature_container: None,
      },
      "transferImpl",
      Some(helper),
    );

    insert_function_props(
      &mut audit,
      main,
      vec![helper],
      vec![state_var],
      vec![event],
      vec![RevertInfo {
        topic: t(900),
        kind: RevertConstraintKind::Revert,
        error_topic: Some(err),
      }],
    );

    // A scope reference unrelated to the typed edges, to drive References.
    audit.topic_context.insert(
      main,
      vec![SourceContext::new_with_scope_references(
        contract,
        Some(0),
        true,
        vec![Reference::project_reference(iface_fn, Some(0))],
      )],
    );

    // AST surface for using-for and modifier-applied.
    let function_def = make_function_def(
      6,
      "transfer",
      vec![make_modifier_invocation(700, modifier.numeric_id())],
    );
    let contract_def = make_contract_def(
      1,
      "Token",
      ContractKind::Contract,
      vec![make_using_for(800, library.numeric_id(), value_type.numeric_id())],
      vec![function_def],
    );
    audit.asts.insert(
      project_path("token.sol"),
      AST::Solidity(make_solidity_ast("token.sol", vec![contract_def])),
    );

    audit
  }

  #[test]
  fn integrated_extraction_is_deterministic() {
    let audit = integrated_audit();

    let mut g1 = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut g1);
    g1.finalize();

    let mut g2 = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut g2);
    g2.finalize();

    assert_eq!(g1, g2);
    assert_eq!(
      serde_json::to_vec(&g1).unwrap(),
      serde_json::to_vec(&g2).unwrap()
    );
  }

  #[test]
  fn integrated_extraction_emits_every_spec_edge_type() {
    let audit = integrated_audit();
    let mut g = ResolutionGraph::new();
    SolidityExtractor.extract(&audit, &mut g);
    g.finalize();

    let mut found: BTreeSet<EdgeType> = BTreeSet::new();
    for src in g.nodes() {
      for e in g.out_edges(src) {
        found.insert(e.edge_type);
      }
    }

    let expected = [
      EdgeType::ContainsMember,
      EdgeType::ContainsLocal,
      EdgeType::ContainsField,
      EdgeType::Calls,
      EdgeType::References,
      EdgeType::Implements,
      EdgeType::ProxyOf,
      EdgeType::WritesState,
      EdgeType::UsingFor,
      EdgeType::ModifierApplied,
      EdgeType::ErrorThrown,
      EdgeType::EventEmitted,
    ];
    for et in expected {
      assert!(
        found.contains(&et),
        "expected {:?} to appear at least once; got {:?}",
        et,
        found
      );
    }
  }

  #[test]
  fn build_pipeline_runs_solidity_extractor_when_solidity_ast_present() {
    // Smoke test for the builder registration in Phase 2: a fresh
    // build() against an audit with a Solidity AST must produce edges
    // (it would not, in Phase 1, since no extractor was registered).
    let audit = integrated_audit();
    let graph = super::super::build(&audit);
    let edge_count: usize = graph.nodes().map(|n| graph.out_edges(n).len()).sum();
    assert!(edge_count > 0, "expected at least one edge to be emitted");
  }
}
