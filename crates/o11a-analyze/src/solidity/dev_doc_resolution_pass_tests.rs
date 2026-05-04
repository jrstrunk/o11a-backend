//! Comprehensive test suite for the dev-doc post-parse resolution
//! pass (Phase B for synthetic NatSpec / inline comment CommentTopics).
//!
//! Mirrors the layered structure of the doc-tree pass tests so the two
//! consumers stay legible side-by-side:
//!
//! 1. Foundational shape — empty audits, no-graph audits, author
//!    filtering, missing-tree defenses.
//! 2. Single-comment scoring — one comment, one ambiguous ref, against
//!    a hand-crafted graph.
//! 3. Scope-chain seeding — function vs. param vs. nested-block targets;
//!    the spec table verbatim.
//! 4. Threshold + tie-break — at, just below, and above the `0.65`
//!    cutoff; deterministic candidate ordering.
//! 5. Determinism + trace persistence — byte-identical runs and trace
//!    map population.
//! 6. mentions_index / mentioned_topics merge — additivity contract.
//! 7. Downstream-contract invariants — kind/referenced_name snapshots,
//!    Phase-A preservation, candidate-field non-touch.

use super::*;
use o11a_core::collaborator::models::Author;
use o11a_core::collaborator::parser::CommentNode;
use o11a_core::domain::{
  self, AuditData, CommentType, ContainingBlockLayer, ContractKind,
  FunctionKind, NamedTopicKind, NamedTopicVisibility, Node, ProjectPath, Scope,
  TopicMetadata, TopicNameIndex, UnnamedTopicKind, new_audit_data,
};
use o11a_core::resolution_graph::{self, EdgeType, ResolutionGraph};
use std::collections::HashSet;

// ---------------------------------------------------------------------
// Test harness — compact builders for the fixtures every test needs
// ---------------------------------------------------------------------

fn nt(id: i32) -> topic::Topic {
  topic::new_node_topic(&id)
}

fn ct(id: i32) -> topic::Topic {
  topic::new_comment_topic(id)
}

fn pp(s: &str) -> ProjectPath {
  ProjectPath {
    file_path: s.to_string(),
  }
}

fn empty_audit() -> AuditData {
  let mut a = new_audit_data("test".to_string(), HashSet::new(), None);
  a.name_index = TopicNameIndex::build(&a);
  a.resolution_graph = Some(resolution_graph::build(&a));
  a
}

fn staged_audit() -> AuditData {
  new_audit_data("test".to_string(), HashSet::new(), None)
}

fn named_topic(
  t: topic::Topic,
  name: &str,
  kind: NamedTopicKind,
  scope: Scope,
) -> TopicMetadata {
  TopicMetadata::NamedTopic {
    topic: t,
    scope,
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

/// Build a `ResolutionGraph` directly from edge triples. Used by tests
/// that bypass the SolidityExtractor.
fn graph_from(edges: &[(topic::Topic, topic::Topic, EdgeType)]) -> ResolutionGraph {
  let mut g = ResolutionGraph::new();
  for (s, d, et) in edges {
    g.add_edge(*s, *d, *et, et.default_weight());
  }
  g.finalize();
  g
}

/// Insert a synthetic dev-doc CommentTopic with the given comment-node
/// tree, target topic, and author. The audit's `comment_index` is
/// updated to mirror what `synthetic::create_synthetic_dev_comment`
/// would have done. Returns the comment topic.
fn insert_dev_doc(
  audit: &mut AuditData,
  comment_id: i32,
  target_topic: topic::Topic,
  author: Author,
  nodes: Vec<CommentNode>,
) -> topic::Topic {
  let comment_topic = ct(comment_id);

  // Pre-compute mentioned_topics from any Phase-A-resolved CodeIdentifiers
  // so initial state matches what the synthetic-comment factory writes.
  let mut mentioned: Vec<topic::Topic> = collect_phase_a_mentions(&nodes);
  mentioned.sort_unstable();
  mentioned.dedup();

  let scope = audit
    .topic_metadata
    .get(&target_topic)
    .map(|m| m.scope().clone())
    .unwrap_or(Scope::Global);

  audit
    .nodes
    .insert(comment_topic, Node::Comment(nodes));
  audit.topic_metadata.insert(
    comment_topic,
    TopicMetadata::CommentTopic {
      topic: comment_topic,
      target_topic,
      comment_type: CommentType::DevTechnical,
      author,
      created_at: String::new(),
      scope,
      mentioned_topics: mentioned.clone(),
    },
  );
  audit
    .comment_index
    .entry(target_topic)
    .or_default()
    .push(comment_topic);
  for m in &mentioned {
    audit
      .mentions_index
      .entry(*m)
      .or_default()
      .push(comment_topic);
  }
  comment_topic
}

fn collect_phase_a_mentions(nodes: &[CommentNode]) -> Vec<topic::Topic> {
  fn walk(node: &CommentNode, out: &mut Vec<topic::Topic>) {
    match node {
      CommentNode::CodeIdentifier {
        referenced_topic: Some(t),
        ..
      } => out.push(*t),
      CommentNode::InlineCode { children, .. } => {
        for c in children {
          walk(c, out);
        }
      }
      _ => {}
    }
  }
  let mut out = Vec::new();
  for n in nodes {
    walk(n, &mut out);
  }
  out
}

/// Build a `CodeIdentifier` comment node. `Some` referenced_topic
/// simulates a Phase-A win; `None` is ambiguous (Phase B's input).
fn code_id(value: &str, referenced_topic: Option<topic::Topic>) -> CommentNode {
  CommentNode::CodeIdentifier {
    value: value.to_string(),
    referenced_topic,
    kind: None,
    referenced_name: None,
    referenced_topic_candidates: Vec::new(),
  }
}

/// Walk an `audit_data.nodes` Comment entry and return every
/// `CodeIdentifier`'s `(value, referenced_topic)` pair in document
/// order. Lets tests assert tree shape compactly.
fn comment_resolutions(
  audit: &AuditData,
  comment_topic: topic::Topic,
) -> Vec<(String, Option<topic::Topic>)> {
  let Some(Node::Comment(nodes)) = audit.nodes.get(&comment_topic) else {
    return Vec::new();
  };
  fn walk(
    node: &CommentNode,
    out: &mut Vec<(String, Option<topic::Topic>)>,
  ) {
    match node {
      CommentNode::CodeIdentifier {
        value,
        referenced_topic,
        ..
      } => out.push((value.clone(), *referenced_topic)),
      CommentNode::InlineCode { children, .. } => {
        for c in children {
          walk(c, out);
        }
      }
      _ => {}
    }
  }
  let mut out = Vec::new();
  for n in nodes {
    walk(n, &mut out);
  }
  out
}

// ---------------------------------------------------------------------
// Layer 1 — foundational shape
// ---------------------------------------------------------------------

#[test]
fn empty_audit_is_a_no_op() {
  let mut audit = empty_audit();
  resolve_dev_doc_comments(&mut audit);
  assert!(audit.resolution_traces.is_empty());
  assert!(audit.mentions_index.is_empty());
}

#[test]
fn missing_resolution_graph_is_a_no_op() {
  // Stage one dev-doc CommentTopic with an ambiguous ref. With no
  // resolution_graph, the pass returns immediately — the comment
  // tree must remain untouched.
  let mut audit = staged_audit();
  let func = nt(10);
  audit.topic_metadata.insert(
    func,
    named_topic(func, "f", NamedTopicKind::Builtin, Scope::Global),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = None;

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    func,
    Author::DevTechnical,
    vec![code_id("name", None)],
  );

  let before = audit.nodes.get(&comment).cloned();
  resolve_dev_doc_comments(&mut audit);
  let after = audit.nodes.get(&comment).cloned();
  assert_eq!(before, after, "tree must not mutate when graph is absent");
  assert!(audit.resolution_traces.is_empty());
}

#[test]
fn comment_with_no_ambiguous_refs_emits_no_traces() {
  // A NatSpec block whose every reference resolved at Phase A: the
  // pass walks it, finds no work, exits without traces or mutations.
  let mut audit = staged_audit();
  let func = nt(10);
  audit.topic_metadata.insert(
    func,
    named_topic(func, "f", NamedTopicKind::Builtin, Scope::Global),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(resolution_graph::build(&audit));

  insert_dev_doc(
    &mut audit,
    -10,
    func,
    Author::DevTechnical,
    vec![code_id("f", Some(func))],
  );
  resolve_dev_doc_comments(&mut audit);
  assert!(audit.resolution_traces.is_empty());
}

#[test]
fn non_dev_doc_authors_are_skipped() {
  // User-authored / agent-authored comments are not in scope. The pass
  // walks only Author::DevTechnical and Author::DevDocumentation
  // entries; everything else stays untouched even when ambiguous.
  let mut audit = staged_audit();
  let func = nt(10);
  audit.topic_metadata.insert(
    func,
    named_topic(func, "f", NamedTopicKind::Builtin, Scope::Global),
  );
  for id in &[100, 200] {
    audit.topic_metadata.insert(
      nt(*id),
      named_topic(nt(*id), "ambig", NamedTopicKind::Builtin, Scope::Global),
    );
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(resolution_graph::build(&audit));

  // Three comments with the same body; only the dev-doc-authored one
  // should be considered.
  for (cid, author) in [
    (-10, Author::DevTechnical),
    (-11, Author::System),
    (-12, Author::AgentMicro),
  ] {
    insert_dev_doc(
      &mut audit,
      cid,
      func,
      author,
      vec![code_id("ambig", None)],
    );
  }
  resolve_dev_doc_comments(&mut audit);
  // Exactly one trace, keyed under the DevTechnical comment.
  assert_eq!(audit.resolution_traces.len(), 1);
  let only_key = audit.resolution_traces.keys().next().unwrap();
  assert_eq!(
    *only_key,
    ResolutionRefId::DevDocComment {
      comment_topic: ct(-10),
      occurrence: 0
    },
  );
}

#[test]
fn ambiguous_ref_with_no_candidates_stays_unresolved_with_trace() {
  // No topic in the audit has the simple name "missingThing", so the
  // candidate list is empty. The pass still records the attempt with
  // `Unresolved`.
  let mut audit = staged_audit();
  let func = nt(10);
  audit.topic_metadata.insert(
    func,
    named_topic(func, "f", NamedTopicKind::Builtin, Scope::Global),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(resolution_graph::build(&audit));

  insert_dev_doc(
    &mut audit,
    -10,
    func,
    Author::DevTechnical,
    vec![code_id("missingThing", None)],
  );

  resolve_dev_doc_comments(&mut audit);

  let res = comment_resolutions(&audit, ct(-10));
  assert_eq!(res, vec![("missingThing".to_string(), None)]);

  assert_eq!(audit.resolution_traces.len(), 1);
  let trace = audit
    .resolution_traces
    .get(&ResolutionRefId::DevDocComment {
      comment_topic: ct(-10),
      occurrence: 0,
    })
    .unwrap();
  assert_eq!(trace.identifier, "missingThing");
  assert_eq!(trace.chosen_topic, None);
  assert_eq!(trace.phase_resolved, ResolutionPhase::Unresolved);
  assert!(trace.candidate_scores.is_empty());
}

#[test]
fn comment_topic_without_node_tree_is_skipped() {
  // Defensive: a CommentTopic registered in topic_metadata but with no
  // matching `audit_data.nodes` entry doesn't crash the pass.
  let mut audit = staged_audit();
  let func = nt(10);
  audit.topic_metadata.insert(
    func,
    named_topic(func, "f", NamedTopicKind::Builtin, Scope::Global),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(resolution_graph::build(&audit));

  audit.topic_metadata.insert(
    ct(-10),
    TopicMetadata::CommentTopic {
      topic: ct(-10),
      target_topic: func,
      comment_type: CommentType::DevTechnical,
      author: Author::DevTechnical,
      created_at: String::new(),
      scope: Scope::Global,
      mentioned_topics: Vec::new(),
    },
  );
  // No corresponding entry in audit.nodes.
  resolve_dev_doc_comments(&mut audit);
  // Pass returns cleanly.
  assert!(audit.resolution_traces.is_empty());
}

// ---------------------------------------------------------------------
// Layer 2 — single-comment scoring
// ---------------------------------------------------------------------

/// Function NatSpec: an ambiguous `transfer` resolves to the same
/// contract's `transfer` because the function's scope chain seeds
/// PR mass at the contract → ContainsMember → vault_transfer.
#[test]
fn function_natspec_resolves_member_via_scope_chain() {
  let vault = nt(10);
  let vault_transfer = nt(11);
  let vault_func = nt(12); // The function the NatSpec is attached to.
  let token = nt(20);
  let token_transfer = nt(21);

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault,
    named_topic(
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_transfer,
    named_topic(
      vault_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: vault,
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_func,
    named_topic(
      vault_func,
      "doStuff",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: vault,
      },
    ),
  );
  audit.topic_metadata.insert(
    token,
    named_topic(
      token,
      "Token",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    token_transfer,
    named_topic(
      token_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: token,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault, EdgeType::ContainsMember),
    (vault, vault_func, EdgeType::ContainsMember),
    (vault_func, vault, EdgeType::ContainsMember),
    (token, token_transfer, EdgeType::ContainsMember),
    (token_transfer, token, EdgeType::ContainsMember),
  ]));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    vault_func,
    Author::DevTechnical,
    vec![code_id("transfer", None)],
  );

  resolve_dev_doc_comments(&mut audit);

  let res = comment_resolutions(&audit, comment);
  assert_eq!(
    res,
    vec![("transfer".to_string(), Some(vault_transfer))],
    "scope-chain seed at vault_func + vault should pull mass to vault_transfer",
  );

  let trace = audit
    .resolution_traces
    .get(&ResolutionRefId::DevDocComment {
      comment_topic: comment,
      occurrence: 0,
    })
    .unwrap();
  assert_eq!(trace.chosen_topic, Some(vault_transfer));
  assert_eq!(trace.phase_resolved, ResolutionPhase::PhaseB);
  assert_eq!(trace.section_topic, comment);
  assert_eq!(trace.iteration, 1);
  // Two candidates ranked, vault_transfer first.
  assert_eq!(trace.candidate_scores.len(), 2);
  assert_eq!(trace.candidate_scores[0].topic, vault_transfer);
  assert_eq!(
    trace.candidate_scores[0].qualified_name.as_deref(),
    Some("Vault.transfer"),
  );
}

// ---------------------------------------------------------------------
// Layer 3 — scope-chain seeding (the spec's seed table)
// ---------------------------------------------------------------------

/// Contract NatSpec target: chain is just the contract. An ambiguous
/// `transfer` should resolve to that contract's member because the
/// contract is the only seed.
#[test]
fn contract_natspec_seeds_from_contract_only() {
  let vault = nt(10);
  let vault_transfer = nt(11);
  let token = nt(20);
  let token_transfer = nt(21);

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault,
    named_topic(
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_transfer,
    named_topic(
      vault_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: vault,
      },
    ),
  );
  audit.topic_metadata.insert(
    token,
    named_topic(
      token,
      "Token",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    token_transfer,
    named_topic(
      token_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: token,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault, EdgeType::ContainsMember),
    (token, token_transfer, EdgeType::ContainsMember),
    (token_transfer, token, EdgeType::ContainsMember),
  ]));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    vault, // attached to Vault directly
    Author::DevDocumentation,
    vec![code_id("transfer", None)],
  );
  resolve_dev_doc_comments(&mut audit);

  let res = comment_resolutions(&audit, comment);
  assert_eq!(res, vec![("transfer".to_string(), Some(vault_transfer))]);
}

/// `@param`-style NatSpec target: parameter → function → contract,
/// halving each step. The deepest seed (contract @ 0.25) still pulls
/// PR through ContainsMember to the same contract's `transfer`.
#[test]
fn param_natspec_chains_param_function_contract() {
  let vault = nt(10);
  let vault_transfer = nt(11);
  let vault_func = nt(12);
  let vault_param = nt(13);
  let token = nt(20);
  let token_transfer = nt(21);

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault,
    named_topic(
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_transfer,
    named_topic(
      vault_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: vault,
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_func,
    named_topic(
      vault_func,
      "doStuff",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: vault,
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_param,
    named_topic(
      vault_param,
      "amount",
      NamedTopicKind::Builtin,
      Scope::Member {
        container: pp("test.sol"),
        component: vault,
        member: vault_func,
        signature_container: None,
      },
    ),
  );
  audit.topic_metadata.insert(
    token,
    named_topic(
      token,
      "Token",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    token_transfer,
    named_topic(
      token_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: token,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault, EdgeType::ContainsMember),
    (vault, vault_func, EdgeType::ContainsMember),
    (vault_func, vault, EdgeType::ContainsMember),
    (vault_func, vault_param, EdgeType::ContainsLocal),
    (vault_param, vault_func, EdgeType::ContainsLocal),
    (token, token_transfer, EdgeType::ContainsMember),
    (token_transfer, token, EdgeType::ContainsMember),
  ]));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    vault_param,
    Author::DevTechnical,
    vec![code_id("transfer", None)],
  );
  resolve_dev_doc_comments(&mut audit);
  let res = comment_resolutions(&audit, comment);
  assert_eq!(res, vec![("transfer".to_string(), Some(vault_transfer))]);
}

/// SemanticBlock comment: ambiguous reference resolves to a name
/// declared inside the block, not to a same-named state variable in
/// another contract. Validates the closer-seed-wins rule (block @ 1.0
/// outweighs same-name competitors not in the block's scope chain).
#[test]
fn semantic_block_comment_in_block_local_wins_over_distant_state_var() {
  let vault = nt(10);
  let vault_func = nt(11);
  let vault_block = nt(12);
  let vault_local = nt(13); // local "value" inside the block
  let token = nt(20);
  let token_value = nt(21); // state variable "value" on Token

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault,
    named_topic(
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_func,
    named_topic(
      vault_func,
      "doStuff",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: vault,
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_block,
    TopicMetadata::UnnamedTopic {
      topic: vault_block,
      kind: UnnamedTopicKind::SemanticBlock,
      scope: Scope::Member {
        container: pp("test.sol"),
        component: vault,
        member: vault_func,
        signature_container: None,
      },
      transitive_topic: None,
    },
  );
  audit.topic_metadata.insert(
    vault_local,
    named_topic(
      vault_local,
      "value",
      NamedTopicKind::Builtin,
      Scope::ContainingBlock {
        container: pp("test.sol"),
        component: vault,
        member: vault_func,
        containing_blocks: vec![ContainingBlockLayer {
          block: vault_block,
          annotation: None,
        }],
      },
    ),
  );
  audit.topic_metadata.insert(
    token,
    named_topic(
      token,
      "Token",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    token_value,
    named_topic(
      token_value,
      "value",
      NamedTopicKind::StateVariable(domain::VariableMutability::Mutable),
      Scope::Component {
        container: pp("test.sol"),
        component: token,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault, vault_func, EdgeType::ContainsMember),
    (vault_func, vault, EdgeType::ContainsMember),
    (vault_func, vault_block, EdgeType::ContainsLocal),
    (vault_block, vault_func, EdgeType::ContainsLocal),
    (vault_block, vault_local, EdgeType::ContainsLocal),
    (vault_local, vault_block, EdgeType::ContainsLocal),
    (token, token_value, EdgeType::ContainsMember),
    (token_value, token, EdgeType::ContainsMember),
  ]));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    vault_block,
    Author::DevTechnical,
    vec![code_id("value", None)],
  );
  resolve_dev_doc_comments(&mut audit);

  let res = comment_resolutions(&audit, comment);
  assert_eq!(
    res,
    vec![("value".to_string(), Some(vault_local))],
    "in-block local should beat a same-name state variable in another contract",
  );
}

/// Phase-A-resolved references inside the comment body seed the PR run
/// at distance 0 — same weight as the target topic. Validates that
/// inline references contribute to disambiguation alongside the scope
/// chain.
#[test]
fn phase_a_inline_references_contribute_to_seed_vector() {
  // No scope-chain anchor: the comment is attached to a Global topic.
  // The only PR signal comes from a Phase-A-resolved `Vault` ref in
  // the comment text. That seed alone disambiguates `transfer` to the
  // same contract's member.
  let global = nt(1);
  let vault = nt(10);
  let vault_transfer = nt(11);
  let token = nt(20);
  let token_transfer = nt(21);

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    global,
    named_topic(global, "Anchor", NamedTopicKind::Builtin, Scope::Global),
  );
  audit.topic_metadata.insert(
    vault,
    named_topic(
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_transfer,
    named_topic(
      vault_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: vault,
      },
    ),
  );
  audit.topic_metadata.insert(
    token,
    named_topic(
      token,
      "Token",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    token_transfer,
    named_topic(
      token_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: token,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault, EdgeType::ContainsMember),
    (token, token_transfer, EdgeType::ContainsMember),
    (token_transfer, token, EdgeType::ContainsMember),
  ]));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    global,
    Author::DevTechnical,
    vec![code_id("Vault", Some(vault)), code_id("transfer", None)],
  );
  resolve_dev_doc_comments(&mut audit);
  let res = comment_resolutions(&audit, comment);
  assert_eq!(
    res,
    vec![
      ("Vault".to_string(), Some(vault)),
      ("transfer".to_string(), Some(vault_transfer)),
    ],
  );
}

// ---------------------------------------------------------------------
// Layer 4 — threshold and tie-break
// ---------------------------------------------------------------------

#[test]
fn ratio_below_threshold_leaves_reference_unresolved() {
  // Symmetric two-candidate setup: same-name state var on two
  // contracts, scope chain is Global. Without a strong anchor, both
  // candidates score equally and the threshold (`0.65`) is missed.
  let global = nt(1);
  let cand_a = nt(10);
  let cand_b = nt(20);
  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    global,
    named_topic(global, "Anchor", NamedTopicKind::Builtin, Scope::Global),
  );
  audit.topic_metadata.insert(
    cand_a,
    named_topic(cand_a, "x", NamedTopicKind::Builtin, Scope::Global),
  );
  audit.topic_metadata.insert(
    cand_b,
    named_topic(cand_b, "x", NamedTopicKind::Builtin, Scope::Global),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (global, cand_a, EdgeType::ContainsMember),
    (cand_a, global, EdgeType::ContainsMember),
    (global, cand_b, EdgeType::ContainsMember),
    (cand_b, global, EdgeType::ContainsMember),
  ]));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    global,
    Author::DevTechnical,
    vec![code_id("x", None)],
  );
  resolve_dev_doc_comments(&mut audit);

  let res = comment_resolutions(&audit, comment);
  assert_eq!(res, vec![("x".to_string(), None)]);

  let trace = audit
    .resolution_traces
    .get(&ResolutionRefId::DevDocComment {
      comment_topic: comment,
      occurrence: 0,
    })
    .unwrap();
  assert_eq!(trace.chosen_topic, None);
  // Phase E records the anchor-by-name fallback; no winner picked.
  assert_eq!(trace.phase_resolved, ResolutionPhase::PhaseE);
  assert_eq!(trace.candidate_scores.len(), 2);
  assert_eq!(
    trace.candidate_scores[0].pr_score.to_bits(),
    trace.candidate_scores[1].pr_score.to_bits(),
    "symmetric topology must produce bit-identical PR",
  );
}

#[test]
fn equal_pr_breaks_tie_on_qualified_name_ascending() {
  // Two candidates equidistant from the seed; ranked by qualified
  // name ascending after PR ties.
  let parent = nt(1);
  let aaa = nt(10);
  let aaa_target = nt(11);
  let bbb = nt(20);
  let bbb_target = nt(21);

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    parent,
    named_topic(
      parent,
      "Parent",
      NamedTopicKind::Builtin,
      Scope::Container {
        container: pp("x.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    aaa,
    named_topic(
      aaa,
      "AAA",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("x.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    bbb,
    named_topic(
      bbb,
      "BBB",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("x.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    aaa_target,
    named_topic(
      aaa_target,
      "target",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("x.sol"),
        component: aaa,
      },
    ),
  );
  audit.topic_metadata.insert(
    bbb_target,
    named_topic(
      bbb_target,
      "target",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("x.sol"),
        component: bbb,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (parent, aaa_target, EdgeType::Calls),
    (parent, bbb_target, EdgeType::Calls),
  ]));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    parent,
    Author::DevTechnical,
    vec![code_id("target", None)],
  );
  resolve_dev_doc_comments(&mut audit);

  let trace = audit
    .resolution_traces
    .get(&ResolutionRefId::DevDocComment {
      comment_topic: comment,
      occurrence: 0,
    })
    .unwrap();
  assert_eq!(trace.candidate_scores.len(), 2);
  assert_eq!(
    trace.candidate_scores[0].qualified_name.as_deref(),
    Some("AAA.target"),
  );
  assert_eq!(
    trace.candidate_scores[1].qualified_name.as_deref(),
    Some("BBB.target"),
  );
}

// ---------------------------------------------------------------------
// Layer 5 — determinism + trace persistence
// ---------------------------------------------------------------------

#[test]
fn pass_is_byte_deterministic_across_repeat_runs() {
  fn fixture() -> AuditData {
    let vault = nt(10);
    let vault_transfer = nt(11);
    let token = nt(20);
    let token_transfer = nt(21);
    let mut audit = staged_audit();
    for (t, name, parent) in [
      (vault, "Vault", None),
      (token, "Token", None),
      (vault_transfer, "transfer", Some(vault)),
      (token_transfer, "transfer", Some(token)),
    ] {
      let scope = match parent {
        None => Scope::Container {
          container: pp("test.sol"),
        },
        Some(c) => Scope::Component {
          container: pp("test.sol"),
          component: c,
        },
      };
      let kind = if parent.is_none() {
        NamedTopicKind::Contract(ContractKind::Contract)
      } else {
        NamedTopicKind::Function(FunctionKind::Function)
      };
      audit.topic_metadata.insert(t, named_topic(t, name, kind, scope));
    }
    audit.name_index = TopicNameIndex::build(&audit);
    audit.resolution_graph = Some(graph_from(&[
      (vault, vault_transfer, EdgeType::ContainsMember),
      (vault_transfer, vault, EdgeType::ContainsMember),
      (token, token_transfer, EdgeType::ContainsMember),
      (token_transfer, token, EdgeType::ContainsMember),
    ]));
    insert_dev_doc(
      &mut audit,
      -10,
      vault,
      Author::DevTechnical,
      vec![code_id("transfer", None)],
    );
    audit
  }

  let mut audit_a = fixture();
  let mut audit_b = fixture();
  resolve_dev_doc_comments(&mut audit_a);
  resolve_dev_doc_comments(&mut audit_b);

  // Serialize trace maps as Vec to avoid serde_json's "non-string map
  // key" rejection — a `ResolutionRefId` enum can't be a JSON object
  // key, so we project the BTreeMap into a sorted Vec<(K, V)> for the
  // byte comparison. BTreeMap iteration is already sorted, so the
  // resulting Vec ordering is deterministic.
  let traces_a: Vec<_> = audit_a.resolution_traces.iter().collect();
  let traces_b: Vec<_> = audit_b.resolution_traces.iter().collect();
  let bytes_a = serde_json::to_vec(&traces_a).unwrap();
  let bytes_b = serde_json::to_vec(&traces_b).unwrap();
  assert_eq!(bytes_a, bytes_b, "traces must serialize identically");

  let nodes_a = serde_json::to_vec(audit_a.nodes.get(&ct(-10)).unwrap()).unwrap();
  let nodes_b = serde_json::to_vec(audit_b.nodes.get(&ct(-10)).unwrap()).unwrap();
  assert_eq!(nodes_a, nodes_b, "post-pass comment trees must match byte-for-byte");
}

#[test]
fn one_trace_per_ambiguous_reference_attempted() {
  // Three ambiguous references in one comment → three traces, each
  // keyed by its depth-first occurrence index.
  let mut audit = staged_audit();
  let func = nt(10);
  audit.topic_metadata.insert(
    func,
    named_topic(func, "f", NamedTopicKind::Builtin, Scope::Global),
  );
  for id in &[100, 200] {
    audit.topic_metadata.insert(
      nt(*id),
      named_topic(nt(*id), "x", NamedTopicKind::Builtin, Scope::Global),
    );
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(resolution_graph::build(&audit));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    func,
    Author::DevTechnical,
    vec![
      code_id("x", None),
      code_id("x", None),
      code_id("x", None),
    ],
  );
  resolve_dev_doc_comments(&mut audit);
  assert_eq!(audit.resolution_traces.len(), 3);
  for occurrence in 0..3 {
    let key = ResolutionRefId::DevDocComment {
      comment_topic: comment,
      occurrence,
    };
    assert!(audit.resolution_traces.contains_key(&key));
  }
}

#[test]
fn ambiguous_inside_inline_code_uses_depth_first_occurrence_index() {
  // CodeIdentifiers nested inside InlineCode are walked in depth-first
  // order. Their occurrence indices must match the order the parser
  // would assign — so the trace key for a nested ambiguous ref is
  // the same number whether the ref sits inline or under InlineCode.
  let mut audit = staged_audit();
  let func = nt(10);
  audit.topic_metadata.insert(
    func,
    named_topic(func, "f", NamedTopicKind::Builtin, Scope::Global),
  );
  for id in &[100, 200] {
    audit.topic_metadata.insert(
      nt(*id),
      named_topic(nt(*id), "x", NamedTopicKind::Builtin, Scope::Global),
    );
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(resolution_graph::build(&audit));

  let inline = CommentNode::InlineCode {
    value: "x".to_string(),
    children: vec![code_id("x", None)],
  };
  let comment = insert_dev_doc(
    &mut audit,
    -10,
    func,
    Author::DevTechnical,
    vec![code_id("x", None), inline],
  );
  resolve_dev_doc_comments(&mut audit);
  // Two ambiguous refs → two traces at occurrences 0 and 1.
  assert!(
    audit.resolution_traces.contains_key(&ResolutionRefId::DevDocComment {
      comment_topic: comment,
      occurrence: 0
    })
  );
  assert!(
    audit.resolution_traces.contains_key(&ResolutionRefId::DevDocComment {
      comment_topic: comment,
      occurrence: 1
    })
  );
}

// ---------------------------------------------------------------------
// Layer 6 — mentions_index / mentioned_topics merge
// ---------------------------------------------------------------------

#[test]
fn newly_resolved_mentions_are_added_to_mentions_index_additively() {
  let vault = nt(10);
  let vault_transfer = nt(11);
  let token = nt(20);
  let token_transfer = nt(21);

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault,
    named_topic(
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("x.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_transfer,
    named_topic(
      vault_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("x.sol"),
        component: vault,
      },
    ),
  );
  audit.topic_metadata.insert(
    token,
    named_topic(
      token,
      "Token",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("x.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    token_transfer,
    named_topic(
      token_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("x.sol"),
        component: token,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault, EdgeType::ContainsMember),
    (token, token_transfer, EdgeType::ContainsMember),
    (token_transfer, token, EdgeType::ContainsMember),
  ]));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    vault,
    Author::DevTechnical,
    vec![code_id("transfer", None)],
  );
  // Pre-existing entry that must survive the merge — picks any
  // unrelated topic so we can assert non-removal.
  audit.mentions_index.insert(nt(999), vec![ct(-99)]);
  resolve_dev_doc_comments(&mut audit);

  // mentions_index now lists the comment under vault_transfer.
  assert_eq!(
    audit.mentions_index.get(&vault_transfer),
    Some(&vec![comment]),
  );
  // Pre-existing unrelated entry intact.
  assert_eq!(audit.mentions_index.get(&nt(999)), Some(&vec![ct(-99)]));

  // mentioned_topics on the CommentTopic metadata also updated.
  let TopicMetadata::CommentTopic {
    mentioned_topics, ..
  } = audit.topic_metadata.get(&comment).unwrap()
  else {
    panic!("CommentTopic metadata missing");
  };
  assert!(mentioned_topics.contains(&vault_transfer));
}

/// Running the pass twice on the same audit must produce identical
/// state — Pass 2 contributes no new resolutions on the second run
/// because every previously ambiguous CodeIdentifier is now Phase-A
/// resolved (we just rewrote its `referenced_topic`). The
/// `mentions_index` and `mentioned_topics` lists must not grow on
/// the second pass even though the merge code re-encounters every
/// resolved topic.
#[test]
fn pass_is_idempotent_under_repeat_application() {
  let vault = nt(10);
  let vault_transfer = nt(11);
  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault,
    named_topic(
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("x.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_transfer,
    named_topic(
      vault_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("x.sol"),
        component: vault,
      },
    ),
  );
  audit.topic_metadata.insert(
    nt(20),
    named_topic(
      nt(20),
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Global,
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault, EdgeType::ContainsMember),
  ]));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    vault,
    Author::DevTechnical,
    vec![code_id("transfer", None)],
  );

  resolve_dev_doc_comments(&mut audit);
  let traces_after_first = audit.resolution_traces.clone();
  let mentions_after_first = audit.mentions_index.clone();
  let mentioned_after_first = match audit.topic_metadata.get(&comment).unwrap()
  {
    TopicMetadata::CommentTopic { mentioned_topics, .. } => {
      mentioned_topics.clone()
    }
    _ => panic!(),
  };

  resolve_dev_doc_comments(&mut audit);
  // Trace map untouched (no new ambiguous attempts).
  assert_eq!(audit.resolution_traces, traces_after_first);
  // mentions_index entry is still a single-element vec — the second
  // pass's no-op `new_mentions` set means the contains() guard is
  // never hit, but more importantly the list never grew.
  assert_eq!(audit.mentions_index, mentions_after_first);
  let mentioned_after_second =
    match audit.topic_metadata.get(&comment).unwrap() {
      TopicMetadata::CommentTopic { mentioned_topics, .. } => {
        mentioned_topics.clone()
      }
      _ => panic!(),
    };
  assert_eq!(mentioned_after_second, mentioned_after_first);
}

#[test]
fn mentions_merge_does_not_duplicate_existing_entries() {
  // If a topic already has the comment listed as a mention (e.g.
  // because it was Phase-A-resolved during synthetic creation), the
  // merge must not append a second copy.
  let vault = nt(10);
  let vault_transfer = nt(11);
  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault,
    named_topic(
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("x.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_transfer,
    named_topic(
      vault_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("x.sol"),
        component: vault,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault, EdgeType::ContainsMember),
  ]));

  // Comment with both a Phase-A `Vault` and an ambiguous `transfer`.
  // Phase-A resolution to Vault populates mentions_index[vault] = [comment]
  // via insert_dev_doc. Then Phase B resolves `transfer` and merges
  // mentions_index[vault_transfer] = [comment].
  let comment = insert_dev_doc(
    &mut audit,
    -10,
    vault,
    Author::DevTechnical,
    vec![code_id("Vault", Some(vault)), code_id("transfer", None)],
  );
  // Sanity check pre-state.
  assert_eq!(audit.mentions_index.get(&vault), Some(&vec![comment]));
  resolve_dev_doc_comments(&mut audit);

  // Vault's mentions list still has exactly one entry — the merge
  // must not duplicate it.
  assert_eq!(audit.mentions_index.get(&vault), Some(&vec![comment]));
  // transfer was newly added.
  assert_eq!(
    audit.mentions_index.get(&vault_transfer),
    Some(&vec![comment]),
  );

  // mentioned_topics is sort+deduped; check ordering and uniqueness.
  let TopicMetadata::CommentTopic {
    mentioned_topics, ..
  } = audit.topic_metadata.get(&comment).unwrap()
  else {
    panic!()
  };
  let mut expected = vec![vault, vault_transfer];
  expected.sort_unstable();
  assert_eq!(mentioned_topics, &expected);
}

// ---------------------------------------------------------------------
// Layer 7 — downstream-contract invariants
// ---------------------------------------------------------------------

#[test]
fn phase_b_winner_carries_kind_and_referenced_name_snapshots() {
  // Phase-B winner must look indistinguishable from a Phase-A winner
  // downstream — `kind` and `referenced_name` are written alongside
  // `referenced_topic`.
  let vault = nt(10);
  let vault_transfer = nt(11);
  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault,
    named_topic(
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("x.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_transfer,
    named_topic(
      vault_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("x.sol"),
        component: vault,
      },
    ),
  );
  audit.topic_metadata.insert(
    nt(20),
    named_topic(
      nt(20),
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Global,
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault, EdgeType::ContainsMember),
  ]));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    vault,
    Author::DevTechnical,
    vec![code_id("transfer", None)],
  );
  resolve_dev_doc_comments(&mut audit);

  let Node::Comment(nodes) = audit.nodes.get(&comment).unwrap() else {
    panic!()
  };
  match &nodes[0] {
    CommentNode::CodeIdentifier {
      referenced_topic,
      kind,
      referenced_name,
      ..
    } => {
      assert_eq!(*referenced_topic, Some(vault_transfer));
      assert_eq!(
        *kind,
        Some(NamedTopicKind::Function(FunctionKind::Function)),
      );
      assert_eq!(referenced_name.as_deref(), Some("transfer"));
    }
    other => panic!("expected CodeIdentifier, got {:?}", other),
  }
}

#[test]
fn phase_a_resolved_natspec_ref_stays_resolved_after_pass() {
  // Regression guard from the build plan: a Phase-A win must never be
  // overwritten by Phase B. Set `Vault` as Phase-A → Vault; pass runs;
  // ref still points at Vault.
  let vault = nt(10);
  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault,
    named_topic(
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("x.sol"),
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(resolution_graph::build(&audit));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    vault,
    Author::DevTechnical,
    vec![code_id("Vault", Some(vault))],
  );
  resolve_dev_doc_comments(&mut audit);
  let res = comment_resolutions(&audit, comment);
  assert_eq!(res, vec![("Vault".to_string(), Some(vault))]);
  // No trace emitted — Phase A wins are not attempted.
  assert!(audit.resolution_traces.is_empty());
}

// ---------------------------------------------------------------------
// Layer 8 — robustness sweep: complex interactions
// ---------------------------------------------------------------------

/// A comment containing only prose (Text / Strong / Emphasis / Link) —
/// no `CodeIdentifier` nodes at all — must produce no traces and no
/// mutations. Pin this so the Pass-1 early-skip (added once we observed
/// that empty-ambiguous plans were dragging the scope-chain walk along
/// for nothing) cannot regress without trips.
#[test]
fn comment_with_only_prose_emits_no_traces() {
  let mut audit = staged_audit();
  let func = nt(10);
  audit.topic_metadata.insert(
    func,
    named_topic(func, "f", NamedTopicKind::Builtin, Scope::Global),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(resolution_graph::build(&audit));

  insert_dev_doc(
    &mut audit,
    -10,
    func,
    Author::DevTechnical,
    vec![
      CommentNode::Text {
        value: "see ".to_string(),
      },
      CommentNode::Strong {
        text: "this".to_string(),
      },
      CommentNode::Text {
        value: " for context, also ".to_string(),
      },
      CommentNode::Emphasis {
        text: "that".to_string(),
      },
      CommentNode::Link {
        url: "http://x".to_string(),
        text: "link".to_string(),
      },
    ],
  );
  resolve_dev_doc_comments(&mut audit);
  assert!(audit.resolution_traces.is_empty());
}

/// Mixed `CommentNode` shapes around an ambiguous `CodeIdentifier`.
/// The pass must walk through Strong, Emphasis, Link siblings without
/// traversing into them (they don't carry CodeIdentifiers) and still
/// find the ambiguous ref nested inside the InlineCode wrapper.
/// Validates the `match`-against-every-variant contract.
#[test]
fn ambiguous_ref_buried_among_prose_nodes_still_resolves() {
  let vault = nt(10);
  let vault_transfer = nt(11);
  let token_transfer = nt(20);

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault,
    named_topic(
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("x.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_transfer,
    named_topic(
      vault_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("x.sol"),
        component: vault,
      },
    ),
  );
  audit.topic_metadata.insert(
    token_transfer,
    named_topic(
      token_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Global,
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault, EdgeType::ContainsMember),
  ]));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    vault,
    Author::DevTechnical,
    vec![
      CommentNode::Text {
        value: "Note: ".to_string(),
      },
      CommentNode::Strong {
        text: "important".to_string(),
      },
      CommentNode::Text {
        value: " — call ".to_string(),
      },
      CommentNode::InlineCode {
        value: "transfer()".to_string(),
        children: vec![
          CommentNode::CodeText {
            value: "(".to_string(),
          },
          code_id("transfer", None),
          CommentNode::CodeText {
            value: ")".to_string(),
          },
        ],
      },
      CommentNode::Emphasis {
        text: "afterwards".to_string(),
      },
      CommentNode::Link {
        url: "https://example.com".to_string(),
        text: "docs".to_string(),
      },
    ],
  );
  resolve_dev_doc_comments(&mut audit);

  let res = comment_resolutions(&audit, comment);
  assert_eq!(res, vec![("transfer".to_string(), Some(vault_transfer))]);
  assert_eq!(audit.resolution_traces.len(), 1);
}

/// Phase A and Phase B both resolve to the same topic — the merge's
/// `contains()` guard prevents the comment from being listed twice
/// under that topic in `mentions_index`. This exercise hits the guard
/// directly: Phase A populates `mentions_index[X] = [comment]` via the
/// synthetic factory; Phase B's ambiguous ref also resolves to `X`;
/// after merge, the entry must still be `[comment]`, not
/// `[comment, comment]`.
#[test]
fn mentions_index_dedups_when_phase_a_and_phase_b_agree_on_topic() {
  let vault = nt(10);
  let vault_transfer = nt(11);
  let token_transfer = nt(20);

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault,
    named_topic(
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("x.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_transfer,
    named_topic(
      vault_transfer,
      "Vault.transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("x.sol"),
        component: vault,
      },
    ),
  );
  // Plus a same-simple-name "transfer" topic so Phase A leaves the
  // bare `transfer` ambiguous.
  audit.topic_metadata.insert(
    token_transfer,
    named_topic(
      token_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Global,
    ),
  );
  // And a second "transfer" so Phase A's `transfer` is ambiguous.
  let third_transfer = nt(30);
  audit.topic_metadata.insert(
    third_transfer,
    named_topic(
      third_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Global,
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault, EdgeType::ContainsMember),
  ]));

  // The CodeIdentifier with referenced_topic = Some(vault_transfer)
  // simulates a parser where one occurrence resolved (e.g. via the
  // qualified name "Vault.transfer") and another did not (the bare
  // "transfer" with two same-name competitors).
  let comment = insert_dev_doc(
    &mut audit,
    -10,
    vault,
    Author::DevTechnical,
    vec![
      code_id("Vault.transfer", Some(vault_transfer)),
      code_id("transfer", None),
    ],
  );
  // Pre-state: synthetic factory populated mentions_index[vault_transfer] = [comment].
  assert_eq!(
    audit.mentions_index.get(&vault_transfer),
    Some(&vec![comment]),
  );

  resolve_dev_doc_comments(&mut audit);

  // Phase B resolves the bare `transfer` to vault_transfer (via the
  // Vault scope-chain seed). After merge, mentions_index[vault_transfer]
  // must remain a single-element list — the contains() guard prevents
  // a second push of `comment`.
  assert_eq!(
    audit.mentions_index.get(&vault_transfer),
    Some(&vec![comment]),
    "comment must not appear twice under vault_transfer in mentions_index",
  );

  // mentioned_topics on the metadata also stays single-entry for
  // vault_transfer.
  let TopicMetadata::CommentTopic {
    mentioned_topics, ..
  } = audit.topic_metadata.get(&comment).unwrap()
  else {
    panic!()
  };
  assert_eq!(
    mentioned_topics
      .iter()
      .filter(|t| **t == vault_transfer)
      .count(),
    1,
    "vault_transfer must appear exactly once in mentioned_topics; got {:?}",
    mentioned_topics,
  );
}

/// Multiple ambiguous occurrences of the same identifier inside one
/// comment must all resolve to the same topic — they share one PR
/// run, so any per-occurrence divergence would mean the resolver
/// re-scored the candidate set per ref instead of once per comment.
/// Mirrors the doc-tree pass's
/// `repeated_ambiguous_identifier_in_one_section_resolves_consistently`.
#[test]
fn repeated_ambiguous_identifier_in_one_comment_resolves_consistently() {
  let vault = nt(10);
  let vault_transfer = nt(11);
  let token = nt(20);
  let token_transfer = nt(21);

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault,
    named_topic(
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("x.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_transfer,
    named_topic(
      vault_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("x.sol"),
        component: vault,
      },
    ),
  );
  audit.topic_metadata.insert(
    token,
    named_topic(
      token,
      "Token",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("x.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    token_transfer,
    named_topic(
      token_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("x.sol"),
        component: token,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault, EdgeType::ContainsMember),
    (token, token_transfer, EdgeType::ContainsMember),
    (token_transfer, token, EdgeType::ContainsMember),
  ]));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    vault,
    Author::DevTechnical,
    vec![
      code_id("transfer", None),
      code_id("transfer", None),
      code_id("transfer", None),
    ],
  );
  resolve_dev_doc_comments(&mut audit);

  let res = comment_resolutions(&audit, comment);
  assert_eq!(
    res,
    vec![
      ("transfer".to_string(), Some(vault_transfer)),
      ("transfer".to_string(), Some(vault_transfer)),
      ("transfer".to_string(), Some(vault_transfer)),
    ],
    "every occurrence must resolve to the same topic via one PR run",
  );
  // Three traces — one per occurrence; all should agree on candidate
  // PR scores because they came from the same PR call.
  assert_eq!(audit.resolution_traces.len(), 3);
  let traces: Vec<_> = audit.resolution_traces.values().collect();
  for t in &traces[1..] {
    assert_eq!(
      t.candidate_scores[0].pr_score.to_bits(),
      traces[0].candidate_scores[0].pr_score.to_bits(),
      "all occurrences share one PR run; bit-identical scores expected",
    );
  }
}

/// Empty graph: `audit.resolution_graph = Some(empty)`. PR returns
/// all-zero, the threshold can't clear, the ref stays unresolved with
/// a trace recording zero PR for every candidate. Validates that
/// "graph exists but has no edges" is a valid no-resolution state, not
/// a panic.
#[test]
fn empty_resolution_graph_returns_unresolved_with_zero_scores() {
  let mut audit = staged_audit();
  let func = nt(10);
  audit.topic_metadata.insert(
    func,
    named_topic(func, "f", NamedTopicKind::Builtin, Scope::Global),
  );
  for id in &[100, 200] {
    audit.topic_metadata.insert(
      nt(*id),
      named_topic(nt(*id), "x", NamedTopicKind::Builtin, Scope::Global),
    );
  }
  audit.name_index = TopicNameIndex::build(&audit);
  // Explicit empty graph — `Some(empty)` rather than `None`.
  audit.resolution_graph = Some(ResolutionGraph::new());

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    func,
    Author::DevTechnical,
    vec![code_id("x", None)],
  );
  resolve_dev_doc_comments(&mut audit);

  let res = comment_resolutions(&audit, comment);
  assert_eq!(res, vec![("x".to_string(), None)]);

  let trace = audit
    .resolution_traces
    .get(&ResolutionRefId::DevDocComment {
      comment_topic: comment,
      occurrence: 0,
    })
    .unwrap();
  assert_eq!(trace.chosen_topic, None);
  // Phase E records the anchor-by-name fallback once Phases B + C exit
  // without a winner; the trace is rewritten from `Unresolved` to
  // `PhaseE` and the candidate scores stay attached for inspection.
  assert_eq!(trace.phase_resolved, ResolutionPhase::PhaseE);
  // Both candidates appear in the trace with PR=0 — the seed lands
  // outside any edge, so PR has nothing to spread.
  assert_eq!(trace.candidate_scores.len(), 2);
  for score in &trace.candidate_scores {
    assert_eq!(score.pr_score, 0.0);
  }
}

/// A Phase-A-resolved topic that is not a `NamedTopic` (e.g. a feature
/// topic recorded by user authoring) must not panic the seed builder
/// or PR engine. The seed lands harmlessly in the PR's node universe.
/// Mirrors Phase 6's defensive test for the same scenario.
#[test]
fn phase_a_seed_to_non_named_topic_does_not_panic() {
  let func = nt(10);
  let target = nt(20);
  let other = nt(30);
  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    func,
    named_topic(func, "f", NamedTopicKind::Builtin, Scope::Global),
  );
  let feature = topic::new_feature_topic(7);
  audit.topic_metadata.insert(
    feature,
    domain::TopicMetadata::FeatureTopic {
      topic: feature,
      name: "synthetic".to_string(),
      description: "synthetic feature for the test".to_string(),
      author: Author::System,
      created_at: None,
    },
  );
  audit.topic_metadata.insert(
    target,
    named_topic(target, "thing", NamedTopicKind::Builtin, Scope::Global),
  );
  audit.topic_metadata.insert(
    other,
    named_topic(other, "thing", NamedTopicKind::Builtin, Scope::Global),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(resolution_graph::build(&audit));

  // The Phase-A reference's referenced_topic is a feature topic — not
  // a NamedTopic. The seed builder should accept it without checking
  // the variant.
  insert_dev_doc(
    &mut audit,
    -10,
    func,
    Author::DevTechnical,
    vec![
      code_id("synthetic", Some(feature)),
      code_id("thing", None),
    ],
  );
  // Should not panic.
  resolve_dev_doc_comments(&mut audit);
}

/// When a Phase-A inline reference happens to resolve to a topic
/// that is also in the scope chain (common when the comment names its
/// own enclosing contract), the seed weights for that topic sum:
/// `2^(-d)` (chain) + `1.0` (Phase A). This is intentional — the
/// target-adjacent contract gets a boost. Pin the math here so a
/// future change that drops the `+=` accumulator can't silently
/// regress to "last write wins".
#[test]
fn scope_chain_and_phase_a_seed_weights_sum_for_overlapping_topic() {
  // Two contracts each containing the same-name function `helper`.
  // Comment is attached to `vault.doStuff`; chain = [doStuff, vault].
  // The text mentions `Vault` as a Phase-A reference — overlapping
  // with the chain entry for `vault`, the contract topic gets
  // weight 0.5 (chain) + 1.0 (Phase A) = 1.5. With or without the
  // overlap, vault's mass dominates and `helper` resolves to
  // vault.helper.
  let vault = nt(10);
  let vault_func = nt(11);
  let vault_helper = nt(12);
  let token = nt(20);
  let token_helper = nt(21);

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault,
    named_topic(
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("x.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_func,
    named_topic(
      vault_func,
      "doStuff",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("x.sol"),
        component: vault,
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_helper,
    named_topic(
      vault_helper,
      "helper",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("x.sol"),
        component: vault,
      },
    ),
  );
  audit.topic_metadata.insert(
    token,
    named_topic(
      token,
      "Token",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("x.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    token_helper,
    named_topic(
      token_helper,
      "helper",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("x.sol"),
        component: token,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault, vault_func, EdgeType::ContainsMember),
    (vault_func, vault, EdgeType::ContainsMember),
    (vault, vault_helper, EdgeType::ContainsMember),
    (vault_helper, vault, EdgeType::ContainsMember),
    (token, token_helper, EdgeType::ContainsMember),
    (token_helper, token, EdgeType::ContainsMember),
  ]));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    vault_func,
    Author::DevTechnical,
    vec![
      code_id("Vault", Some(vault)),
      code_id("helper", None),
    ],
  );
  resolve_dev_doc_comments(&mut audit);

  let res = comment_resolutions(&audit, comment);
  assert_eq!(
    res,
    vec![
      ("Vault".to_string(), Some(vault)),
      ("helper".to_string(), Some(vault_helper)),
    ],
  );

  let trace = audit
    .resolution_traces
    .get(&ResolutionRefId::DevDocComment {
      comment_topic: comment,
      occurrence: 1,
    })
    .unwrap();
  // Confidence ratio must be substantially above 0.65 — vault gets
  // boosted seed mass, so vault_helper's PR dominates token_helper's
  // by a wider margin than scope-chain-alone would produce.
  let top = trace.candidate_scores[0].pr_score;
  let runner = trace.candidate_scores[1].pr_score;
  assert!(top > 0.0);
  assert!(top > runner);
  assert!(top / (top + runner) >= 0.65);
}

/// Deeply nested `InlineCode`-within-`InlineCode` is not produced by
/// the production parser (`tokenize_comment_code` emits leaf-only
/// children), but the recursive walker is structurally capable of
/// it. Pin the contract: a CodeIdentifier two `InlineCode` levels
/// deep is found, scored, and mutated correctly. Defensive — protects
/// against a future parser change that introduces nested inline-code.
#[test]
fn nested_inline_code_does_not_break_walk() {
  let vault = nt(10);
  let vault_transfer = nt(11);
  let token_transfer = nt(20);

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault,
    named_topic(
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("x.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_transfer,
    named_topic(
      vault_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("x.sol"),
        component: vault,
      },
    ),
  );
  audit.topic_metadata.insert(
    token_transfer,
    named_topic(
      token_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Global,
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault, EdgeType::ContainsMember),
  ]));

  // Two levels of InlineCode wrapping a single CodeIdentifier.
  let inner_inline = CommentNode::InlineCode {
    value: "transfer".to_string(),
    children: vec![code_id("transfer", None)],
  };
  let outer_inline = CommentNode::InlineCode {
    value: "transfer".to_string(),
    children: vec![inner_inline],
  };
  let comment = insert_dev_doc(
    &mut audit,
    -10,
    vault,
    Author::DevTechnical,
    vec![outer_inline],
  );
  resolve_dev_doc_comments(&mut audit);

  let res = comment_resolutions(&audit, comment);
  assert_eq!(res, vec![("transfer".to_string(), Some(vault_transfer))]);
  // Trace key must use occurrence index 0 — the deeply-nested
  // CodeIdentifier is the first (and only) one walked depth-first.
  assert!(
    audit
      .resolution_traces
      .contains_key(&ResolutionRefId::DevDocComment {
        comment_topic: comment,
        occurrence: 0,
      })
  );
}

/// Build-plan unit ask: a function NatSpec that mentions a sibling
/// state variable name shared with another contract's state variable —
/// the same-contract sibling must win. The doc-tree pass tests cover
/// the analogous case for documentation files; this test pins the
/// dev-doc side independently.
#[test]
fn function_natspec_sibling_state_var_wins_over_other_contract() {
  let vault = nt(10);
  let vault_func = nt(11);
  let vault_balance = nt(12); // sibling state var
  let token = nt(20);
  let token_balance = nt(21); // same-name state var on a different contract

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault,
    named_topic(
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("x.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_func,
    named_topic(
      vault_func,
      "doStuff",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("x.sol"),
        component: vault,
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_balance,
    named_topic(
      vault_balance,
      "balance",
      NamedTopicKind::StateVariable(domain::VariableMutability::Mutable),
      Scope::Component {
        container: pp("x.sol"),
        component: vault,
      },
    ),
  );
  audit.topic_metadata.insert(
    token,
    named_topic(
      token,
      "Token",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("x.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    token_balance,
    named_topic(
      token_balance,
      "balance",
      NamedTopicKind::StateVariable(domain::VariableMutability::Mutable),
      Scope::Component {
        container: pp("x.sol"),
        component: token,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault, vault_func, EdgeType::ContainsMember),
    (vault_func, vault, EdgeType::ContainsMember),
    (vault, vault_balance, EdgeType::ContainsMember),
    (vault_balance, vault, EdgeType::ContainsMember),
    (token, token_balance, EdgeType::ContainsMember),
    (token_balance, token, EdgeType::ContainsMember),
  ]));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    vault_func,
    Author::DevTechnical,
    vec![code_id("balance", None)],
  );
  resolve_dev_doc_comments(&mut audit);
  let res = comment_resolutions(&audit, comment);
  assert_eq!(res, vec![("balance".to_string(), Some(vault_balance))]);
}

/// Modifier NatSpec target: structurally identical to a function in
/// the scope chain (`Scope::Component { component: contract }`), so
/// resolution should work the same way. Pinned separately so a
/// future scope-shape change to modifiers can't silently regress this.
#[test]
fn modifier_natspec_resolves_member_via_scope_chain() {
  let vault = nt(10);
  let vault_modifier = nt(11);
  let vault_internal = nt(12); // ambiguous candidate
  let token = nt(20);
  let token_internal = nt(21);

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault,
    named_topic(
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("x.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_modifier,
    named_topic(
      vault_modifier,
      "onlyOwner",
      NamedTopicKind::Modifier,
      Scope::Component {
        container: pp("x.sol"),
        component: vault,
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_internal,
    named_topic(
      vault_internal,
      "guard",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("x.sol"),
        component: vault,
      },
    ),
  );
  audit.topic_metadata.insert(
    token,
    named_topic(
      token,
      "Token",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("x.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    token_internal,
    named_topic(
      token_internal,
      "guard",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("x.sol"),
        component: token,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault, vault_modifier, EdgeType::ContainsMember),
    (vault_modifier, vault, EdgeType::ContainsMember),
    (vault, vault_internal, EdgeType::ContainsMember),
    (vault_internal, vault, EdgeType::ContainsMember),
    (token, token_internal, EdgeType::ContainsMember),
    (token_internal, token, EdgeType::ContainsMember),
  ]));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    vault_modifier,
    Author::DevTechnical,
    vec![code_id("guard", None)],
  );
  resolve_dev_doc_comments(&mut audit);
  let res = comment_resolutions(&audit, comment);
  assert_eq!(res, vec![("guard".to_string(), Some(vault_internal))]);
}

/// Multiple dev-doc comments attached to different targets in one pass
/// are scored independently — each gets its own seed vector and PR
/// run. Cross-comment leakage would surface here if a future change
/// accidentally reused PR results across plans.
#[test]
fn multiple_comments_resolve_independently_against_their_own_chains() {
  let vault = nt(10);
  let vault_transfer = nt(11);
  let vault_func = nt(12);
  let token = nt(20);
  let token_transfer = nt(21);
  let token_func = nt(22);

  let mut audit = staged_audit();
  for (t, name, kind, parent) in [
    (
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      None,
    ),
    (
      token,
      "Token",
      NamedTopicKind::Contract(ContractKind::Contract),
      None,
    ),
    (
      vault_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Some(vault),
    ),
    (
      token_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Some(token),
    ),
    (
      vault_func,
      "doVaultStuff",
      NamedTopicKind::Function(FunctionKind::Function),
      Some(vault),
    ),
    (
      token_func,
      "doTokenStuff",
      NamedTopicKind::Function(FunctionKind::Function),
      Some(token),
    ),
  ] {
    let scope = match parent {
      None => Scope::Container {
        container: pp("x.sol"),
      },
      Some(c) => Scope::Component {
        container: pp("x.sol"),
        component: c,
      },
    };
    audit.topic_metadata.insert(t, named_topic(t, name, kind, scope));
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault, EdgeType::ContainsMember),
    (vault, vault_func, EdgeType::ContainsMember),
    (vault_func, vault, EdgeType::ContainsMember),
    (token, token_transfer, EdgeType::ContainsMember),
    (token_transfer, token, EdgeType::ContainsMember),
    (token, token_func, EdgeType::ContainsMember),
    (token_func, token, EdgeType::ContainsMember),
  ]));

  let vault_comment = insert_dev_doc(
    &mut audit,
    -10,
    vault_func,
    Author::DevTechnical,
    vec![code_id("transfer", None)],
  );
  let token_comment = insert_dev_doc(
    &mut audit,
    -11,
    token_func,
    Author::DevTechnical,
    vec![code_id("transfer", None)],
  );
  resolve_dev_doc_comments(&mut audit);

  // Each comment's `transfer` resolves to the function on its own
  // contract — proving the seed vectors stayed scoped per-comment.
  assert_eq!(
    comment_resolutions(&audit, vault_comment),
    vec![("transfer".to_string(), Some(vault_transfer))],
    "vault comment must resolve to vault.transfer",
  );
  assert_eq!(
    comment_resolutions(&audit, token_comment),
    vec![("transfer".to_string(), Some(token_transfer))],
    "token comment must resolve to token.transfer",
  );
  // Two traces, one per comment.
  assert_eq!(audit.resolution_traces.len(), 2);
}

/// Scope chains deeper than `MAX_SEED_DEPTH` are truncated so
/// extremely nested blocks do not produce arbitrarily long seed
/// vectors. Constructed by piling on `ContainingBlockLayer` entries
/// — each one extends the chain by one. A chain longer than 7
/// (`MAX_SEED_DEPTH + 1`) entries means at least one ancestor is
/// dropped from the seed.
#[test]
fn scope_chain_truncates_at_max_seed_depth() {
  let vault = nt(1);
  let vault_func = nt(2);
  // Build eight nested blocks: b0 (outermost) … b7 (innermost target)
  let blocks: Vec<topic::Topic> = (0..8).map(|i| nt(10 + i)).collect();

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault,
    named_topic(
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("x.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_func,
    named_topic(
      vault_func,
      "f",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("x.sol"),
        component: vault,
      },
    ),
  );
  // Each block_i has scope ContainingBlock with all blocks before it
  // as containing_blocks. block_7 (innermost) has [b0..b6].
  for (i, block) in blocks.iter().enumerate() {
    let containing_blocks: Vec<ContainingBlockLayer> = blocks[..i]
      .iter()
      .map(|b| ContainingBlockLayer {
        block: *b,
        annotation: None,
      })
      .collect();
    let scope = if i == 0 {
      Scope::Member {
        container: pp("x.sol"),
        component: vault,
        member: vault_func,
        signature_container: None,
      }
    } else {
      Scope::ContainingBlock {
        container: pp("x.sol"),
        component: vault,
        member: vault_func,
        containing_blocks,
      }
    };
    audit.topic_metadata.insert(
      *block,
      TopicMetadata::UnnamedTopic {
        topic: *block,
        kind: UnnamedTopicKind::SemanticBlock,
        scope,
        transitive_topic: None,
      },
    );
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(resolution_graph::build(&audit));

  // The innermost block's chain (uncapped) would be:
  //   [b7, b6, b5, b4, b3, b2, b1, b0, vault_func, vault]   (10 entries)
  // After capping at MAX_SEED_DEPTH + 1 = 7 entries:
  //   [b7, b6, b5, b4, b3, b2, b1]
  // — vault_func and vault both fall off the seed vector.
  let chain = domain::scope_ancestor_chain(&audit, blocks[7]);
  assert_eq!(chain.len(), 10, "uncapped chain must include every ancestor");
  // The pass's helper truncates it; we test the cap is enforced by
  // exercising a comment attached to b7 and asserting that an
  // ambiguous identifier resolvable only via vault_func / vault stays
  // unresolved (because those topics fell off the seed).
  let target_only = nt(100); // accessible only through vault as a sibling
  let other = nt(200); // unrelated competitor
  audit.topic_metadata.insert(
    target_only,
    named_topic(
      target_only,
      "amb",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("x.sol"),
        component: vault,
      },
    ),
  );
  audit.topic_metadata.insert(
    other,
    named_topic(
      other,
      "amb",
      NamedTopicKind::Builtin,
      Scope::Global,
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault, target_only, EdgeType::ContainsMember),
    (target_only, vault, EdgeType::ContainsMember),
  ]));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    blocks[7],
    Author::DevTechnical,
    vec![code_id("amb", None)],
  );
  resolve_dev_doc_comments(&mut audit);
  // With the contract beyond the depth cap, no seed reaches
  // `target_only` via the graph; PR is zero on both candidates and
  // neither clears the threshold.
  let res = comment_resolutions(&audit, comment);
  assert_eq!(res, vec![("amb".to_string(), None)]);
}

/// `referenced_topic_candidates` invariant: non-empty IFF the ref is
/// unresolved. Stale Phase E candidates from a prior run must be
/// cleared when Phase B/C succeeds — otherwise an audit re-run with a
/// changed graph would leave inconsistent state. Mirrors the doc-tree
/// counterpart and pins the same invariant for dev-doc comments.
#[test]
fn phase_b_clears_stale_candidates_phase_e_repopulates_them() {
  let vault = nt(10);
  let vault_transfer = nt(11);
  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault,
    named_topic(
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("x.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    vault_transfer,
    named_topic(
      vault_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("x.sol"),
        component: vault,
      },
    ),
  );
  audit.topic_metadata.insert(
    nt(20),
    named_topic(
      nt(20),
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Global,
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault, EdgeType::ContainsMember),
  ]));

  let stale_candidates = vec![nt(7), nt(8)];
  // Build the comment manually with stale candidates pre-populated:
  //   - First ref: Phase B will resolve `transfer` to `vault_transfer`
  //     (scope-chain seed pulls PR mass via ContainsMember). Stale
  //     candidates must be cleared.
  //   - Second ref: name has no candidates → Phase B fails → Phase E
  //     skips (empty candidate list) → stale candidates stay.
  let phase_b_node = CommentNode::CodeIdentifier {
    value: "transfer".to_string(),
    referenced_topic: None,
    kind: None,
    referenced_name: None,
    referenced_topic_candidates: stale_candidates.clone(),
  };
  let no_candidates_node = CommentNode::CodeIdentifier {
    value: "missingThing".to_string(),
    referenced_topic: None,
    kind: None,
    referenced_name: None,
    referenced_topic_candidates: stale_candidates.clone(),
  };
  let comment = insert_dev_doc(
    &mut audit,
    -10,
    vault,
    Author::DevTechnical,
    vec![phase_b_node, no_candidates_node],
  );
  resolve_dev_doc_comments(&mut audit);

  let Node::Comment(nodes) = audit.nodes.get(&comment).unwrap() else {
    panic!()
  };
  let CommentNode::CodeIdentifier {
    referenced_topic: ref0,
    referenced_topic_candidates: cand0,
    ..
  } = &nodes[0]
  else {
    panic!()
  };
  assert_eq!(*ref0, Some(vault_transfer));
  assert!(
    cand0.is_empty(),
    "Phase B winner must clear stale Phase E candidates: got {:?}",
    cand0,
  );

  let CommentNode::CodeIdentifier {
    referenced_topic: ref1,
    referenced_topic_candidates: cand1,
    ..
  } = &nodes[1]
  else {
    panic!()
  };
  assert_eq!(*ref1, None);
  assert_eq!(
    *cand1, stale_candidates,
    "refs with no name candidates skip Phase E → field stays untouched",
  );
}

// ---------------------------------------------------------------------
// Layer 8 — Phases C (co-location) + D (re-iteration)
//
// Mirrors Layer 8 of the doc-tree pass tests but exercises the dev-doc
// consumer: per-comment co-location of locals declared inside the same
// function/modifier/struct/event/error scope, plus the iteration loop
// that lets new resolutions feed forward into the next round's seeds.
// ---------------------------------------------------------------------

/// Helper: build a contract with two functions, each declaring local
/// variables. Returns the audit and useful topics for assertions.
fn dev_doc_co_loc_fixture() -> (
  AuditData,
  topic::Topic, // contract C
  topic::Topic, // foo function (in C)
  topic::Topic, // bar function (in C)
  topic::Topic, // foo.amount
  topic::Topic, // foo.tmp
  topic::Topic, // bar.amount
  topic::Topic, // bar.tmp
) {
  let contract = nt(1);
  let foo = nt(10);
  let bar = nt(20);
  let foo_amount = nt(11);
  let foo_tmp = nt(12);
  let bar_amount = nt(21);
  let bar_tmp = nt(22);

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    contract,
    named_topic(
      contract,
      "C",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
  );
  for (t, name, member) in [
    (foo, "foo", None),
    (bar, "bar", None),
    (foo_amount, "amount", Some(foo)),
    (foo_tmp, "tmp", Some(foo)),
    (bar_amount, "amount", Some(bar)),
    (bar_tmp, "tmp", Some(bar)),
  ] {
    let scope = match member {
      None => Scope::Component {
        container: pp("test.sol"),
        component: contract,
      },
      Some(m) => Scope::Member {
        container: pp("test.sol"),
        component: contract,
        member: m,
        signature_container: None,
      },
    };
    let kind = if member.is_none() {
      NamedTopicKind::Function(FunctionKind::Function)
    } else {
      NamedTopicKind::LocalVariable
    };
    audit.topic_metadata.insert(t, named_topic(t, name, kind, scope));
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (contract, foo, EdgeType::ContainsMember),
    (foo, contract, EdgeType::ContainsMember),
    (contract, bar, EdgeType::ContainsMember),
    (bar, contract, EdgeType::ContainsMember),
    (foo, foo_amount, EdgeType::ContainsLocal),
    (foo_amount, foo, EdgeType::ContainsLocal),
    (foo, foo_tmp, EdgeType::ContainsLocal),
    (foo_tmp, foo, EdgeType::ContainsLocal),
    (bar, bar_amount, EdgeType::ContainsLocal),
    (bar_amount, bar, EdgeType::ContainsLocal),
    (bar, bar_tmp, EdgeType::ContainsLocal),
    (bar_tmp, bar, EdgeType::ContainsLocal),
  ]));
  (audit, contract, foo, bar, foo_amount, foo_tmp, bar_amount, bar_tmp)
}

/// Phase C — singleton intersection. NatSpec attached to a sibling
/// function `helper` mentions `amount` and `tmp`. By dropping
/// `bar.tmp`, only `foo` declares both locals; Phase C pins both refs
/// to foo's declarations even though Phase B's PR (with `helper`'s
/// scope chain seeded) does not flow into either local.
#[test]
fn phase_c_pins_pair_in_dev_doc_when_intersection_is_singleton() {
  let (mut audit, contract, _foo, _bar, foo_amount, foo_tmp, _bar_amount, _bar_tmp) =
    dev_doc_co_loc_fixture();
  // Drop bar.tmp so Phase C can find a singleton intersection.
  audit.topic_metadata.remove(&nt(22));
  audit.name_index = TopicNameIndex::build(&audit);

  // Add a sibling function `helper` whose NatSpec is the test target.
  let helper = nt(50);
  audit.topic_metadata.insert(
    helper,
    named_topic(
      helper,
      "helper",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: contract,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(resolution_graph::build(&audit));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    helper,
    Author::DevTechnical,
    vec![code_id("amount", None), code_id("tmp", None)],
  );

  resolve_dev_doc_comments(&mut audit);

  let res = comment_resolutions(&audit, comment);
  assert_eq!(
    res,
    vec![
      ("amount".to_string(), Some(foo_amount)),
      ("tmp".to_string(), Some(foo_tmp)),
    ],
  );

  // Both refs phase_resolved == PhaseC, iteration == 1.
  for occ in [0u32, 1] {
    let trace = audit
      .resolution_traces
      .get(&ResolutionRefId::DevDocComment {
        comment_topic: comment,
        occurrence: occ,
      })
      .unwrap();
    assert_eq!(trace.phase_resolved, ResolutionPhase::PhaseC);
    assert_eq!(trace.iteration, 1);
  }
}

/// Phase C — multi-element intersection. Both `amount` and `tmp` exist
/// in {foo, bar}. Phase C abstains; the refs stay Unresolved.
#[test]
fn phase_c_dev_doc_abstains_on_multi_element_intersection() {
  let (mut audit, contract, _foo, _bar, _fa, _ft, _ba, _bt) =
    dev_doc_co_loc_fixture();
  // Add helper function as the comment target — a sibling that doesn't
  // declare amount or tmp, so the seed chain doesn't push PR mass into
  // either function's locals.
  let helper = nt(50);
  audit.topic_metadata.insert(
    helper,
    named_topic(
      helper,
      "helper",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: contract,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(resolution_graph::build(&audit));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    helper,
    Author::DevTechnical,
    vec![code_id("amount", None), code_id("tmp", None)],
  );

  resolve_dev_doc_comments(&mut audit);

  let res = comment_resolutions(&audit, comment);
  assert_eq!(
    res,
    vec![
      ("amount".to_string(), None),
      ("tmp".to_string(), None),
    ],
    "two-element intersection must abstain",
  );

  // Both refs fall through to Phase E — the anchor-by-name fallback
  // records the candidate list without picking one.
  for occ in [0u32, 1] {
    let trace = audit
      .resolution_traces
      .get(&ResolutionRefId::DevDocComment {
        comment_topic: comment,
        occurrence: occ,
      })
      .unwrap();
    assert_eq!(trace.phase_resolved, ResolutionPhase::PhaseE);
  }
}

/// Phase D — second iteration unlocks a third reference. Iter 1's
/// Phase C resolves `amount` and `tmp` (singleton scope foo). The new
/// foo / foo_tmp seeds become Phase-A inputs for iter 2, where
/// Phase B's PR mass flows into `wire_in_foo` via a Calls edge.
/// `wire_in_bar` is a graph island (zero PR), so the threshold check
/// `top/(top+0) = 1.0` clears trivially. Verifies the cascade:
/// PhaseC (iter 1) → PhaseB (iter 2).
#[test]
fn phase_d_dev_doc_cascades_resolutions_across_iterations() {
  let (mut audit, contract, foo, _bar, foo_amount, foo_tmp, _bar_amount, _bar_tmp) =
    dev_doc_co_loc_fixture();
  audit.topic_metadata.remove(&nt(22)); // drop bar.tmp

  // Sibling function `helper` is the comment target. To force Phase C
  // (not Phase B) to resolve amount/tmp in iter 1, we use a graph
  // where neither candidate is reachable from helper's scope chain —
  // the foo and bar regions are disconnected from the seeds entirely.
  let helper = nt(50);
  audit.topic_metadata.insert(
    helper,
    named_topic(
      helper,
      "helper",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: contract,
      },
    ),
  );

  // `wire` is a contract-level function — Phase C cannot pin it (its
  // immediate enclosing scope is the contract, too coarse). The only
  // path to a Phase B win for `wire` is graph mass from a foo-side
  // Phase-A seed (added by iter 1's Phase C). wire_in_bar is a graph
  // island so its PR is always zero.
  let wire_in_foo = nt(40);
  let wire_in_bar = nt(41);
  audit.topic_metadata.insert(
    wire_in_foo,
    named_topic(
      wire_in_foo,
      "wire",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: contract,
      },
    ),
  );
  audit.topic_metadata.insert(
    wire_in_bar,
    named_topic(
      wire_in_bar,
      "wire",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: contract,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);

  // Disconnected graph regions: helper has no graph edges (so iter 1
  // PR is empty), foo / foo_amount / foo_tmp form one cluster, bar /
  // bar_amount form another. iter 1: zero PR everywhere → Phase B
  // fails → Phase C pins amount + tmp via singleton {foo}. iter 2:
  // foo_amount and foo_tmp seeded → PR mass flows via foo_tmp →
  // wire_in_foo. wire_in_bar untouched.
  audit.resolution_graph = Some(graph_from(&[
    // Foo's locals.
    (foo, foo_amount, EdgeType::ContainsLocal),
    (foo_amount, foo, EdgeType::ContainsLocal),
    (foo, foo_tmp, EdgeType::ContainsLocal),
    (foo_tmp, foo, EdgeType::ContainsLocal),
    // Bar's only local.
    (nt(20), nt(21), EdgeType::ContainsLocal),
    (nt(21), nt(20), EdgeType::ContainsLocal),
    // Iter-2 path: foo_tmp → wire_in_foo via Calls.
    (foo_tmp, wire_in_foo, EdgeType::Calls),
  ]));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    helper,
    Author::DevTechnical,
    vec![
      code_id("amount", None),
      code_id("tmp", None),
      code_id("wire", None),
    ],
  );

  resolve_dev_doc_comments(&mut audit);

  let res = comment_resolutions(&audit, comment);
  assert!(
    res.iter().all(|(_, t)| t.is_some()),
    "all three refs must resolve across iterations: {:?}",
    res
  );

  let trace_for = |occ: u32| {
    audit
      .resolution_traces
      .get(&ResolutionRefId::DevDocComment {
        comment_topic: comment,
        occurrence: occ,
      })
      .unwrap()
      .clone()
  };
  let amount_trace = trace_for(0);
  let tmp_trace = trace_for(1);
  let wire_trace = trace_for(2);

  // amount + tmp via PhaseC iter 1.
  assert_eq!(amount_trace.phase_resolved, ResolutionPhase::PhaseC);
  assert_eq!(amount_trace.iteration, 1);
  assert_eq!(tmp_trace.phase_resolved, ResolutionPhase::PhaseC);
  assert_eq!(tmp_trace.iteration, 1);

  // wire via PhaseB iter 2 once foo / foo_tmp are Phase-A seeds.
  assert_eq!(wire_trace.phase_resolved, ResolutionPhase::PhaseB);
  assert!(
    wire_trace.iteration >= 2,
    "wire must resolve in iteration ≥ 2: got {}",
    wire_trace.iteration
  );
  assert_eq!(wire_trace.chosen_topic, Some(wire_in_foo));
}

/// Phase D — exits early when no progress. Single ambiguous ref with
/// no candidates → Phase B fails → Phase C abstains (single ref) →
/// loop exits after iteration 1.
#[test]
fn phase_d_dev_doc_exits_early_when_no_new_resolutions() {
  let mut audit = staged_audit();
  let func = nt(10);
  audit.topic_metadata.insert(
    func,
    named_topic(
      func,
      "f",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(resolution_graph::build(&audit));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    func,
    Author::DevTechnical,
    vec![code_id("missing", None)],
  );
  resolve_dev_doc_comments(&mut audit);
  let trace = audit
    .resolution_traces
    .get(&ResolutionRefId::DevDocComment {
      comment_topic: comment,
      occurrence: 0,
    })
    .unwrap();
  assert_eq!(trace.iteration, 1);
  assert_eq!(trace.phase_resolved, ResolutionPhase::Unresolved);
}

/// Phase D — iteration field never exceeds the cap.
#[test]
fn phase_d_dev_doc_iteration_field_never_exceeds_cap() {
  let (mut audit, contract, _foo, _bar, _fa, _ft, _ba, _bt) =
    dev_doc_co_loc_fixture();
  audit.topic_metadata.remove(&nt(22));
  audit.name_index = TopicNameIndex::build(&audit);

  let helper = nt(50);
  audit.topic_metadata.insert(
    helper,
    named_topic(
      helper,
      "helper",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: contract,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(resolution_graph::build(&audit));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    helper,
    Author::DevTechnical,
    vec![code_id("amount", None), code_id("tmp", None)],
  );
  resolve_dev_doc_comments(&mut audit);

  for trace in audit.resolution_traces.values() {
    assert!(
      trace.iteration >= 1 && trace.iteration <= 4,
      "iteration must be in [1, 4]: {} ({})",
      trace.iteration,
      trace.identifier
    );
  }
  let _ = comment;
}

/// Determinism — Phase C + D output is byte-identical across repeat
/// runs of the dev-doc consumer.
#[test]
fn phase_c_and_d_dev_doc_byte_deterministic_across_repeat_runs() {
  let build_audit = || {
    let (mut audit, contract, _foo, _bar, _fa, _ft, _ba, _bt) =
      dev_doc_co_loc_fixture();
    audit.topic_metadata.remove(&nt(22));
    let helper = nt(50);
    audit.topic_metadata.insert(
      helper,
      named_topic(
        helper,
        "helper",
        NamedTopicKind::Function(FunctionKind::Function),
        Scope::Component {
          container: pp("test.sol"),
          component: contract,
        },
      ),
    );
    audit.name_index = TopicNameIndex::build(&audit);
    // Build the graph with explicit edges so PR has signal to flow.
    audit.resolution_graph = Some(graph_from(&[
      (contract, helper, EdgeType::ContainsMember),
      (helper, contract, EdgeType::ContainsMember),
      (contract, nt(10), EdgeType::ContainsMember),
      (nt(10), contract, EdgeType::ContainsMember),
      (contract, nt(20), EdgeType::ContainsMember),
      (nt(20), contract, EdgeType::ContainsMember),
      (nt(10), nt(11), EdgeType::ContainsLocal),
      (nt(11), nt(10), EdgeType::ContainsLocal),
      (nt(10), nt(12), EdgeType::ContainsLocal),
      (nt(12), nt(10), EdgeType::ContainsLocal),
      (nt(20), nt(21), EdgeType::ContainsLocal),
      (nt(21), nt(20), EdgeType::ContainsLocal),
    ]));
    insert_dev_doc(
      &mut audit,
      -10,
      helper,
      Author::DevTechnical,
      vec![code_id("amount", None), code_id("tmp", None)],
    );
    audit
  };

  let mut audit_a = build_audit();
  let mut audit_b = build_audit();
  resolve_dev_doc_comments(&mut audit_a);
  resolve_dev_doc_comments(&mut audit_b);

  // Project BTreeMap → sorted Vec<(K, V)> for serde_json (the trace
  // map's ResolutionRefId key is an enum, not a string, and serde_json
  // rejects non-string map keys).
  let traces_a: Vec<_> = audit_a.resolution_traces.iter().collect();
  let traces_b: Vec<_> = audit_b.resolution_traces.iter().collect();
  let bytes_a = serde_json::to_vec(&traces_a).unwrap();
  let bytes_b = serde_json::to_vec(&traces_b).unwrap();
  assert_eq!(bytes_a, bytes_b);

  // Compare the post-pass comment tree byte-for-byte — the actual
  // mutation Phase B / C / D produced.
  let comment = ct(-10);
  let nodes_a = serde_json::to_vec(audit_a.nodes.get(&comment).unwrap()).unwrap();
  let nodes_b = serde_json::to_vec(audit_b.nodes.get(&comment).unwrap()).unwrap();
  assert_eq!(nodes_a, nodes_b);
}

/// Phase C — newly-resolved mentions still flow into mentions_index
/// and the comment's `mentioned_topics` field. The accumulator merge
/// is shared with Phase B's mentions logic, so a Phase C win must end
/// up in both maps.
#[test]
fn phase_c_dev_doc_resolutions_appear_in_mentions_index() {
  let (mut audit, contract, _foo, _bar, foo_amount, foo_tmp, _bar_amount, _bar_tmp) =
    dev_doc_co_loc_fixture();
  audit.topic_metadata.remove(&nt(22));
  audit.name_index = TopicNameIndex::build(&audit);

  let helper = nt(50);
  audit.topic_metadata.insert(
    helper,
    named_topic(
      helper,
      "helper",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: contract,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(resolution_graph::build(&audit));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    helper,
    Author::DevTechnical,
    vec![code_id("amount", None), code_id("tmp", None)],
  );

  resolve_dev_doc_comments(&mut audit);

  // Both topics now key into mentions_index pointing at this comment.
  assert!(audit
    .mentions_index
    .get(&foo_amount)
    .map(|v| v.contains(&comment))
    .unwrap_or(false));
  assert!(audit
    .mentions_index
    .get(&foo_tmp)
    .map(|v| v.contains(&comment))
    .unwrap_or(false));

  let TopicMetadata::CommentTopic { mentioned_topics, .. } =
    audit.topic_metadata.get(&comment).unwrap()
  else {
    panic!("expected CommentTopic")
  };
  assert!(mentioned_topics.contains(&foo_amount));
  assert!(mentioned_topics.contains(&foo_tmp));
}

/// Phase C — three-ref interaction with one conflict. Two non-
/// conflicting pairs each pin one ref; the conflicted ref stays
/// unresolved. Mirrors the doc-tree counterpart.
#[test]
fn phase_c_dev_doc_conflicting_pin_drops_only_the_conflicting_ref() {
  let contract = nt(1);
  let foo = nt(10);
  let bar = nt(20);
  let foo_x = nt(11);
  let foo_y = nt(12);
  let bar_x = nt(21);
  let bar_z = nt(22);
  let helper = nt(50);

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    contract,
    named_topic(
      contract,
      "C",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
  );
  for (t, name, member) in [
    (foo, "foo", None),
    (bar, "bar", None),
    (helper, "helper", None),
    (foo_x, "x", Some(foo)),
    (foo_y, "y", Some(foo)),
    (bar_x, "x", Some(bar)),
    (bar_z, "z", Some(bar)),
  ] {
    let scope = match member {
      None => Scope::Component {
        container: pp("test.sol"),
        component: contract,
      },
      Some(m) => Scope::Member {
        container: pp("test.sol"),
        component: contract,
        member: m,
        signature_container: None,
      },
    };
    let kind = if member.is_none() {
      NamedTopicKind::Function(FunctionKind::Function)
    } else {
      NamedTopicKind::LocalVariable
    };
    audit.topic_metadata.insert(t, named_topic(t, name, kind, scope));
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (contract, foo, EdgeType::ContainsMember),
    (foo, contract, EdgeType::ContainsMember),
    (contract, bar, EdgeType::ContainsMember),
    (bar, contract, EdgeType::ContainsMember),
    (contract, helper, EdgeType::ContainsMember),
    (helper, contract, EdgeType::ContainsMember),
  ]));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    helper,
    Author::DevTechnical,
    vec![code_id("x", None), code_id("y", None), code_id("z", None)],
  );
  resolve_dev_doc_comments(&mut audit);

  let res = comment_resolutions(&audit, comment);
  assert_eq!(
    res,
    vec![
      ("x".to_string(), None),
      ("y".to_string(), Some(foo_y)),
      ("z".to_string(), Some(bar_z)),
    ]
  );

  let trace_for = |occ: u32| {
    audit
      .resolution_traces
      .get(&ResolutionRefId::DevDocComment {
        comment_topic: comment,
        occurrence: occ,
      })
      .unwrap()
      .clone()
  };
  // x conflicts (would-be foo.x via (x, y) AND would-be bar.x via
  // (x, z)) → Phase C drops it → Phase E records the candidate set.
  assert_eq!(trace_for(0).phase_resolved, ResolutionPhase::PhaseE);
  assert_eq!(trace_for(1).phase_resolved, ResolutionPhase::PhaseC);
  assert_eq!(trace_for(2).phase_resolved, ResolutionPhase::PhaseC);
  let _ = bar_x;
  let _ = foo_x;
}

/// Phase C — does not revisit Phase B resolutions. Once Phase B
/// resolves a ref in iter 1, Phase C must leave it alone — even when
/// its co-location signal would also fire.
///
/// Setup: comment attached to foo. The graph has ONLY foo's edges into
/// foo_amount / foo_tmp; bar is a graph island so bar_amount has zero
/// PR. Phase B's threshold (`top/(top+0) = 1.0`) clears trivially for
/// both refs in iter 1. Phase C would also pin them (singleton {foo}
/// since we drop bar.tmp), but Phase B wins first and Phase C must
/// not relabel.
#[test]
fn phase_c_dev_doc_does_not_revisit_phase_b_resolutions() {
  let (mut audit, _contract, foo, _bar, foo_amount, foo_tmp, _bar_amount, _bar_tmp) =
    dev_doc_co_loc_fixture();
  audit.topic_metadata.remove(&nt(22)); // drop bar.tmp

  // Replace the symmetric fixture graph with one that ONLY connects
  // foo's locals — bar is an island. This forces Phase B to win
  // decisively (zero PR on bar_amount → ratio = 1.0 ≥ 0.65).
  audit.resolution_graph = Some(graph_from(&[
    (foo, foo_amount, EdgeType::ContainsLocal),
    (foo_amount, foo, EdgeType::ContainsLocal),
    (foo, foo_tmp, EdgeType::ContainsLocal),
    (foo_tmp, foo, EdgeType::ContainsLocal),
  ]));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    foo,
    Author::DevTechnical,
    vec![code_id("amount", None), code_id("tmp", None)],
  );
  resolve_dev_doc_comments(&mut audit);

  let res = comment_resolutions(&audit, comment);
  assert_eq!(
    res,
    vec![
      ("amount".to_string(), Some(foo_amount)),
      ("tmp".to_string(), Some(foo_tmp)),
    ],
  );

  for occ in [0u32, 1] {
    let trace = audit
      .resolution_traces
      .get(&ResolutionRefId::DevDocComment {
        comment_topic: comment,
        occurrence: occ,
      })
      .unwrap();
    assert_eq!(
      trace.phase_resolved,
      ResolutionPhase::PhaseB,
      "Phase B resolutions must NOT be relabeled by Phase C: {} → {:?}",
      trace.identifier,
      trace.phase_resolved
    );
    assert_eq!(trace.iteration, 1);
  }
}

/// Phase D — unresolved ref's trace records the iteration of the LAST
/// attempt, not the cap. With nothing to cascade, the loop exits at
/// iter 1 and the trace's `iteration` field is `1`.
#[test]
fn phase_d_dev_doc_unresolved_ref_records_iteration_of_last_attempt() {
  let mut audit = staged_audit();
  let func = nt(10);
  audit.topic_metadata.insert(
    func,
    named_topic(
      func,
      "f",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
  );
  // Two candidates for "x" → ambiguous, no graph signal → unresolved.
  for id in &[100, 200] {
    audit.topic_metadata.insert(
      nt(*id),
      named_topic(nt(*id), "x", NamedTopicKind::Builtin, Scope::Global),
    );
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[]));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    func,
    Author::DevTechnical,
    vec![code_id("x", None)],
  );
  resolve_dev_doc_comments(&mut audit);

  let trace = audit
    .resolution_traces
    .get(&ResolutionRefId::DevDocComment {
      comment_topic: comment,
      occurrence: 0,
    })
    .unwrap();
  // Phase E records the anchor-by-name fallback once Phases B + C exit
  // without a winner; iteration mirrors the last B/C round.
  assert_eq!(trace.phase_resolved, ResolutionPhase::PhaseE);
  assert_eq!(trace.iteration, 1);
}

/// Phase C — singleton intersection but multiple candidates inside the
/// singleton scope (e.g., shadowing) → abstain. Mirrors the doc-tree
/// counterpart and the coloc-module unit test, exercised through the
/// dev-doc consumer's full pipeline.
#[test]
fn phase_c_dev_doc_abstains_when_singleton_scope_holds_multiple_candidates() {
  // contract C { function foo() { /* two `amount`s */ } }. Phase C
  // sees `tmp`'s candidate set {foo.tmp} and `amount`'s {foo.amount1,
  // foo.amount2}. Intersection is {foo} (singleton) but foo holds two
  // candidates of `amount` → ambiguous pin → abstain.
  let contract = nt(1);
  let foo = nt(10);
  let amount1 = nt(11);
  let amount2 = nt(12);
  let foo_tmp = nt(13);
  let helper = nt(50);

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    contract,
    named_topic(
      contract,
      "C",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    foo,
    named_topic(
      foo,
      "foo",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: contract,
      },
    ),
  );
  audit.topic_metadata.insert(
    helper,
    named_topic(
      helper,
      "helper",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: contract,
      },
    ),
  );
  for (t, name) in [(amount1, "amount"), (amount2, "amount"), (foo_tmp, "tmp")] {
    audit.topic_metadata.insert(
      t,
      named_topic(
        t,
        name,
        NamedTopicKind::LocalVariable,
        Scope::Member {
          container: pp("test.sol"),
          component: contract,
          member: foo,
          signature_container: None,
        },
      ),
    );
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[]));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    helper,
    Author::DevTechnical,
    vec![code_id("amount", None), code_id("tmp", None)],
  );
  resolve_dev_doc_comments(&mut audit);

  // Phase C abstains because foo holds two `amount` candidates → both
  // refs fall through to Phase E (anchor-by-name fallback).
  for occ in [0u32, 1] {
    let trace = audit
      .resolution_traces
      .get(&ResolutionRefId::DevDocComment {
        comment_topic: comment,
        occurrence: occ,
      })
      .unwrap();
    assert_eq!(
      trace.phase_resolved,
      ResolutionPhase::PhaseE,
      "Phase C must abstain on multi-candidate singleton scope: {} → {:?}",
      trace.identifier,
      trace.phase_resolved,
    );
  }
}

/// Two independent comments — each runs its own Phase D loop; one
/// comment's resolutions do not feed another's seed vector. Verifies
/// the per-comment isolation contract: dev-doc plans are independent
/// during iteration even though they share the trace + resolution
/// maps.
#[test]
fn phase_d_dev_doc_per_comment_isolation_during_iteration() {
  let (mut audit, contract, _foo, _bar, foo_amount, foo_tmp, _bar_amount, _bar_tmp) =
    dev_doc_co_loc_fixture();
  audit.topic_metadata.remove(&nt(22));
  audit.name_index = TopicNameIndex::build(&audit);

  // Two helpers, each owning its own comment with the same body.
  let helper_a = nt(50);
  let helper_b = nt(60);
  for h in [helper_a, helper_b] {
    audit.topic_metadata.insert(
      h,
      named_topic(
        h,
        "helper",
        NamedTopicKind::Function(FunctionKind::Function),
        Scope::Component {
          container: pp("test.sol"),
          component: contract,
        },
      ),
    );
  }
  audit.name_index = TopicNameIndex::build(&audit);
  // Disconnected graph — Phase B fails, Phase C must do the work for
  // both comments independently.
  audit.resolution_graph = Some(graph_from(&[
    (nt(10), foo_amount, EdgeType::ContainsLocal),
    (foo_amount, nt(10), EdgeType::ContainsLocal),
    (nt(10), foo_tmp, EdgeType::ContainsLocal),
    (foo_tmp, nt(10), EdgeType::ContainsLocal),
    (nt(20), nt(21), EdgeType::ContainsLocal),
    (nt(21), nt(20), EdgeType::ContainsLocal),
  ]));

  let comment_a = insert_dev_doc(
    &mut audit,
    -10,
    helper_a,
    Author::DevTechnical,
    vec![code_id("amount", None), code_id("tmp", None)],
  );
  let comment_b = insert_dev_doc(
    &mut audit,
    -11,
    helper_b,
    Author::DevTechnical,
    vec![code_id("amount", None), code_id("tmp", None)],
  );

  resolve_dev_doc_comments(&mut audit);

  // Both comments resolved their refs to the same foo locals via
  // Phase C, independently of each other.
  for c in [comment_a, comment_b] {
    let res = comment_resolutions(&audit, c);
    assert_eq!(
      res,
      vec![
        ("amount".to_string(), Some(foo_amount)),
        ("tmp".to_string(), Some(foo_tmp)),
      ],
      "comment {:?} must resolve independently",
      c,
    );
    for occ in [0u32, 1] {
      let trace = audit
        .resolution_traces
        .get(&ResolutionRefId::DevDocComment {
          comment_topic: c,
          occurrence: occ,
        })
        .unwrap();
      assert_eq!(trace.phase_resolved, ResolutionPhase::PhaseC);
      assert_eq!(trace.iteration, 1);
    }
  }
}

// ---------------------------------------------------------------------
// Layer 8 — Phase E (anchor-by-name fallback)
//
// Phase E activates after Phase D's loop exits. For every still-
// ambiguous reference whose `candidates_by_simple_name` lookup is non-
// empty, the resolver:
//
// 1. Writes the full candidate list onto the comment node's
//    `referenced_topic_candidates` field.
// 2. Relabels the trace from `Unresolved` to `PhaseE` while preserving
//    the candidate scores from the last Phase B / C attempt.
// 3. Leaves `referenced_topic` `None`.
//
// Unlike the doc-tree consumer, the dev-doc consumer's contract anchor
// is already pinned by the comment's `target_topic`, so no additional
// section-level anchoring is needed downstream — the field exists for
// operator inspection.
// ---------------------------------------------------------------------

/// Pin the field-write contract: a comment whose ambiguous ref Phase D
/// could not resolve gets `referenced_topic_candidates` populated with
/// the full candidate list (in topic-ID ascending order — the
/// `candidates_by_simple_name` contract).
#[test]
fn phase_e_dev_doc_populates_referenced_topic_candidates() {
  // Two `transfer` candidates split across two contracts, no graph
  // signal favoring either, comment attached to a third (unrelated)
  // function whose scope chain seeds nothing useful for either
  // candidate. Phase B abstains, Phase C abstains (single ref) → Phase
  // E records candidates.
  let vault = nt(10);
  let vault_transfer = nt(11);
  let token = nt(20);
  let token_transfer = nt(21);
  let helper_contract = nt(30);
  let helper_func = nt(31);

  let mut audit = staged_audit();
  for (t, name, kind, scope) in [
    (
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container { container: pp("test.sol") },
    ),
    (
      token,
      "Token",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container { container: pp("test.sol") },
    ),
    (
      helper_contract,
      "Helper",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container { container: pp("test.sol") },
    ),
    (
      vault_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component { container: pp("test.sol"), component: vault },
    ),
    (
      token_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component { container: pp("test.sol"), component: token },
    ),
    (
      helper_func,
      "doStuff",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: helper_contract,
      },
    ),
  ] {
    audit.topic_metadata.insert(t, named_topic(t, name, kind, scope));
  }
  audit.name_index = TopicNameIndex::build(&audit);
  // No edges into either transfer from helper_contract or helper_func.
  audit.resolution_graph = Some(graph_from(&[
    (vault, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault, EdgeType::ContainsMember),
    (token, token_transfer, EdgeType::ContainsMember),
    (token_transfer, token, EdgeType::ContainsMember),
    (helper_contract, helper_func, EdgeType::ContainsMember),
    (helper_func, helper_contract, EdgeType::ContainsMember),
  ]));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    helper_func,
    Author::DevTechnical,
    vec![code_id("transfer", None)],
  );
  resolve_dev_doc_comments(&mut audit);

  // referenced_topic stays None; referenced_topic_candidates carries
  // both candidates in ascending topic-ID order.
  let Some(Node::Comment(nodes)) = audit.nodes.get(&comment) else {
    unreachable!()
  };
  let CommentNode::CodeIdentifier {
    referenced_topic,
    referenced_topic_candidates,
    ..
  } = &nodes[0]
  else {
    unreachable!()
  };
  assert!(
    referenced_topic.is_none(),
    "Phase E never picks a winner",
  );
  assert_eq!(
    *referenced_topic_candidates,
    vec![vault_transfer, token_transfer],
    "Phase E writes full candidate list, sorted ascending by topic ID",
  );

  // Trace shape: PhaseE, no chosen, candidate_scores carry both.
  let trace = audit
    .resolution_traces
    .get(&ResolutionRefId::DevDocComment {
      comment_topic: comment,
      occurrence: 0,
    })
    .unwrap();
  assert_eq!(trace.phase_resolved, ResolutionPhase::PhaseE);
  assert_eq!(trace.chosen_topic, None);
  assert_eq!(trace.candidate_scores.len(), 2);
}

/// A dev-doc ref whose candidates is empty stays `Unresolved` — no
/// fallback fires, no field write happens.
#[test]
fn phase_e_dev_doc_skips_refs_with_no_name_candidates() {
  let mut audit = staged_audit();
  let func = nt(10);
  audit.topic_metadata.insert(
    func,
    named_topic(func, "f", NamedTopicKind::Builtin, Scope::Global),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(resolution_graph::build(&audit));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    func,
    Author::DevTechnical,
    vec![code_id("missingThing", None)],
  );
  resolve_dev_doc_comments(&mut audit);

  let trace = audit
    .resolution_traces
    .get(&ResolutionRefId::DevDocComment {
      comment_topic: comment,
      occurrence: 0,
    })
    .unwrap();
  assert_eq!(
    trace.phase_resolved,
    ResolutionPhase::Unresolved,
    "no candidates ⇒ no Phase E ⇒ trace stays Unresolved",
  );

  let Some(Node::Comment(nodes)) = audit.nodes.get(&comment) else {
    unreachable!()
  };
  let CommentNode::CodeIdentifier {
    referenced_topic_candidates,
    ..
  } = &nodes[0]
  else {
    unreachable!()
  };
  assert!(
    referenced_topic_candidates.is_empty(),
    "no candidates ⇒ field stays empty",
  );
}

/// Phase E never overwrites a Phase B / C win in the dev-doc tree:
/// a comment containing one resolved-by-B ref and one falling-to-E ref
/// produces an empty candidates list on the winner and a populated
/// list on the loser.
#[test]
fn phase_e_dev_doc_does_not_touch_phase_b_winners() {
  let vault = nt(10);
  let vault_transfer = nt(11);
  let vault_func = nt(12);
  let token = nt(20);
  let token_transfer = nt(21);
  let other_a = nt(50);
  let other_b = nt(60);

  let mut audit = staged_audit();
  for (t, name, kind, scope) in [
    (
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container { container: pp("test.sol") },
    ),
    (
      token,
      "Token",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container { container: pp("test.sol") },
    ),
    (
      vault_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component { container: pp("test.sol"), component: vault },
    ),
    (
      vault_func,
      "doStuff",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component { container: pp("test.sol"), component: vault },
    ),
    (
      token_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component { container: pp("test.sol"), component: token },
    ),
    (other_a, "ambig", NamedTopicKind::Builtin, Scope::Global),
    (other_b, "ambig", NamedTopicKind::Builtin, Scope::Global),
  ] {
    audit.topic_metadata.insert(t, named_topic(t, name, kind, scope));
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault, EdgeType::ContainsMember),
    (vault, vault_func, EdgeType::ContainsMember),
    (vault_func, vault, EdgeType::ContainsMember),
    (token, token_transfer, EdgeType::ContainsMember),
    (token_transfer, token, EdgeType::ContainsMember),
  ]));

  // Comment attached to vault.doStuff: scope chain seeds Vault →
  // Vault.transfer wins via Phase B; "ambig" has no graph anchor →
  // Phase E records candidates.
  let comment = insert_dev_doc(
    &mut audit,
    -10,
    vault_func,
    Author::DevTechnical,
    vec![code_id("transfer", None), code_id("ambig", None)],
  );
  resolve_dev_doc_comments(&mut audit);

  let Some(Node::Comment(nodes)) = audit.nodes.get(&comment) else {
    unreachable!()
  };
  // First ref — Phase B winner.
  let CommentNode::CodeIdentifier {
    referenced_topic: tref0,
    referenced_topic_candidates: tcand0,
    ..
  } = &nodes[0]
  else {
    unreachable!()
  };
  assert_eq!(*tref0, Some(vault_transfer));
  assert!(tcand0.is_empty(), "Phase B winner's candidates field stays empty");

  // Second ref — Phase E.
  let CommentNode::CodeIdentifier {
    referenced_topic: tref1,
    referenced_topic_candidates: tcand1,
    ..
  } = &nodes[1]
  else {
    unreachable!()
  };
  assert_eq!(*tref1, None);
  assert_eq!(*tcand1, vec![other_a, other_b]);
}

/// `mentions_index` and the `mentioned_topics` field are *not* touched
/// for Phase E references — only Phase B / C winners contribute. Pin
/// the contract: the dev-doc pass's mention-merge step only follows
/// `referenced_topic`, never `referenced_topic_candidates`.
#[test]
fn phase_e_dev_doc_does_not_pollute_mentions_index() {
  let mut audit = staged_audit();
  let func = nt(10);
  audit.topic_metadata.insert(
    func,
    named_topic(func, "f", NamedTopicKind::Builtin, Scope::Global),
  );
  for id in &[100, 200] {
    audit.topic_metadata.insert(
      nt(*id),
      named_topic(nt(*id), "ambig", NamedTopicKind::Builtin, Scope::Global),
    );
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(resolution_graph::build(&audit));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    func,
    Author::DevTechnical,
    vec![code_id("ambig", None)],
  );
  let mentions_before = audit.mentions_index.clone();
  resolve_dev_doc_comments(&mut audit);

  // mentions_index unchanged — Phase E does not produce a winner to
  // record.
  assert_eq!(
    audit.mentions_index, mentions_before,
    "Phase E must not contribute to mentions_index",
  );

  // mentioned_topics on the comment metadata is also unchanged.
  let TopicMetadata::CommentTopic { mentioned_topics, .. } =
    audit.topic_metadata.get(&comment).unwrap()
  else {
    unreachable!()
  };
  assert!(
    mentioned_topics.is_empty(),
    "Phase E must not contribute to mentioned_topics",
  );
}

/// Phase E preserves candidate ordering: the `candidates_by_simple_name`
/// slice is sorted ascending by topic ID; Phase E writes it verbatim.
#[test]
fn phase_e_dev_doc_preserves_candidate_iteration_order() {
  let mut audit = staged_audit();
  let func = nt(10);
  audit.topic_metadata.insert(
    func,
    named_topic(func, "f", NamedTopicKind::Builtin, Scope::Global),
  );
  for id in &[500, 100, 300, 200] {
    audit.topic_metadata.insert(
      nt(*id),
      named_topic(nt(*id), "thing", NamedTopicKind::Builtin, Scope::Global),
    );
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(resolution_graph::build(&audit));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    func,
    Author::DevTechnical,
    vec![code_id("thing", None)],
  );
  resolve_dev_doc_comments(&mut audit);

  let Some(Node::Comment(nodes)) = audit.nodes.get(&comment) else {
    unreachable!()
  };
  let CommentNode::CodeIdentifier {
    referenced_topic_candidates,
    ..
  } = &nodes[0]
  else {
    unreachable!()
  };
  assert_eq!(
    *referenced_topic_candidates,
    vec![nt(100), nt(200), nt(300), nt(500)],
    "Phase E writes candidates in candidates_by_simple_name's sorted order",
  );
}

/// Determinism: the same audit + same comment runs twice → same
/// trace bytes, same comment-tree bytes (including
/// `referenced_topic_candidates`).
#[test]
fn phase_e_dev_doc_is_byte_deterministic_across_repeat_runs() {
  fn build() -> AuditData {
    let vault = nt(10);
    let vault_transfer = nt(11);
    let vault_func = nt(12);
    let token = nt(20);
    let token_transfer = nt(21);

    let mut audit = staged_audit();
    for (t, name, kind, scope) in [
      (
        vault,
        "Vault",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container { container: pp("test.sol") },
      ),
      (
        token,
        "Token",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container { container: pp("test.sol") },
      ),
      (
        vault_transfer,
        "transfer",
        NamedTopicKind::Function(FunctionKind::Function),
        Scope::Component {
          container: pp("test.sol"),
          component: vault,
        },
      ),
      (
        vault_func,
        "doStuff",
        NamedTopicKind::Function(FunctionKind::Function),
        Scope::Component {
          container: pp("test.sol"),
          component: vault,
        },
      ),
      (
        token_transfer,
        "transfer",
        NamedTopicKind::Function(FunctionKind::Function),
        Scope::Component {
          container: pp("test.sol"),
          component: token,
        },
      ),
    ] {
      audit.topic_metadata.insert(t, named_topic(t, name, kind, scope));
    }
    audit.name_index = TopicNameIndex::build(&audit);
    // No edges so PR is uniformly zero — `transfer` falls to Phase E.
    audit.resolution_graph = Some(graph_from(&[]));
    insert_dev_doc(
      &mut audit,
      -10,
      vault_func,
      Author::DevTechnical,
      vec![code_id("transfer", None)],
    );
    audit
  }

  let mut a = build();
  let mut b = build();
  resolve_dev_doc_comments(&mut a);
  resolve_dev_doc_comments(&mut b);

  // Compare comment-tree bytes via the `nodes` map entry for the
  // synthetic comment.
  let nodes_a = a.nodes.get(&ct(-10)).cloned().unwrap();
  let nodes_b = b.nodes.get(&ct(-10)).cloned().unwrap();
  assert_eq!(
    serde_json::to_vec(&nodes_a).unwrap(),
    serde_json::to_vec(&nodes_b).unwrap(),
    "comment tree bytes must match across repeat runs",
  );

  // Trace contents match too. (`resolution_traces` is keyed by
  // `ResolutionRefId`, which serde_json can't render as a JSON map
  // key, so we compare the values vector instead — same determinism
  // contract, expressible in JSON.)
  let traces_a: Vec<_> = a.resolution_traces.iter().collect();
  let traces_b: Vec<_> = b.resolution_traces.iter().collect();
  assert_eq!(
    serde_json::to_vec(&traces_a).unwrap(),
    serde_json::to_vec(&traces_b).unwrap(),
    "trace bytes must match across repeat runs",
  );
}

/// Phase E idempotency for the dev-doc consumer: running the pass
/// twice on the same audit produces byte-identical state. Pins the
/// contract that the resolver can re-run safely (e.g., after a re-load
/// that doesn't change the underlying graph).
#[test]
fn phase_e_dev_doc_is_idempotent_across_repeat_passes() {
  let mut audit = staged_audit();
  let func = nt(10);
  audit.topic_metadata.insert(
    func,
    named_topic(func, "f", NamedTopicKind::Builtin, Scope::Global),
  );
  for id in &[100, 200] {
    audit.topic_metadata.insert(
      nt(*id),
      named_topic(nt(*id), "ambig", NamedTopicKind::Builtin, Scope::Global),
    );
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(resolution_graph::build(&audit));

  let comment = insert_dev_doc(
    &mut audit,
    -10,
    func,
    Author::DevTechnical,
    vec![code_id("ambig", None)],
  );

  resolve_dev_doc_comments(&mut audit);
  let nodes_first = audit.nodes.get(&comment).cloned().unwrap();
  let traces_first: Vec<_> =
    audit.resolution_traces.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
  let mentions_first = audit.mentions_index.clone();

  resolve_dev_doc_comments(&mut audit);
  let nodes_second = audit.nodes.get(&comment).cloned().unwrap();
  let traces_second: Vec<_> =
    audit.resolution_traces.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
  let mentions_second = audit.mentions_index.clone();

  assert_eq!(
    serde_json::to_vec(&nodes_first).unwrap(),
    serde_json::to_vec(&nodes_second).unwrap(),
    "comment tree must be byte-identical after repeat pass",
  );
  assert_eq!(
    serde_json::to_vec(&traces_first).unwrap(),
    serde_json::to_vec(&traces_second).unwrap(),
    "traces must be byte-identical after repeat pass",
  );
  assert_eq!(
    mentions_first, mentions_second,
    "mentions_index must be unchanged across repeat pass",
  );
}
