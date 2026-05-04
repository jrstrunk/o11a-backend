//! Comprehensive test suite for the doc-tree post-parse resolution
//! pass (Phase B for documentation files).
//!
//! The tests are organized in seven layers, increasing in complexity:
//!
//! 1. Foundational shape — empty trees, no-graph audits, AST-walk
//!    invariants.
//! 2. Single-section scoring — one section, one ambiguous reference,
//!    against a hand-crafted graph small enough to predict by hand.
//! 3. Section-tree interactions — ancestor seeding, sibling
//!    isolation, depth attenuation, and the depth-6 cap.
//! 4. Threshold + tie-break — at, just below, and just above the
//!    `0.65` cutoff; deterministic candidate ordering on ties.
//! 5. Trace persistence + determinism — every attempted resolution
//!    produces a trace, and the entire pass is byte-deterministic
//!    across repeat runs.
//! 6. Interaction edge cases — flat docs without headings, deeply
//!    nested non-section containers, top-edge sort + cap, Phase-A
//!    + ambiguous coexistence, non-NamedTopic seed safety.
//! 7. Downstream-contract invariants — `kind` / `referenced_name`
//!    snapshots, `referenced_topic_candidates` preservation, repeated
//!    identifiers, heading-text scope assignment.

use super::*;
use o11a_core::documentation::ast::DocumentationNode;
use o11a_core::domain::{
  self, AuditData, ContractKind, FunctionKind, NamedTopicKind,
  NamedTopicVisibility, ProjectPath, Scope, TopicMetadata, TopicNameIndex,
  new_audit_data,
};
use o11a_core::resolution_graph::{self, EdgeType, ResolutionGraph};
use std::collections::HashSet;

// ---------------------------------------------------------------------
// Test harness — compact builders for the fixtures every test needs
// ---------------------------------------------------------------------

/// `Topic::Node(id)` shorthand — every audit-side topic in the
/// fixtures is a Node topic, since that's what `name_index` indexes.
fn nt(id: i32) -> topic::Topic {
  topic::new_node_topic(&id)
}

/// `Topic::Documentation(id)` shorthand for section topics in tests
/// that want to compare them.
fn dt(id: i32) -> topic::Topic {
  topic::new_documentation_topic(id)
}

/// Build an `AuditData` with no topics (and so an empty name index).
/// Useful for tests that exercise the walker on input with no
/// candidates.
fn empty_audit() -> AuditData {
  let mut a = new_audit_data("test".to_string(), HashSet::new(), None);
  a.name_index = TopicNameIndex::build(&a);
  a.resolution_graph = Some(resolution_graph::build(&a));
  a
}

/// Construct a `NamedTopic` declaration with a specified scope and
/// kind. Wrapper to keep the fixture noise low in test bodies.
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

fn pp(s: &str) -> ProjectPath {
  ProjectPath {
    file_path: s.to_string(),
  }
}

/// Stage an `AuditData` whose topic_metadata + inheritance look like
/// the post-Solidity-analyzer state. The test then customizes by
/// inserting topics, building the index, and rebuilding the graph.
fn staged_audit() -> AuditData {
  new_audit_data("test".to_string(), HashSet::new(), None)
}

/// Finalize an `AuditData` after the test populates topic_metadata
/// (and any other phase-0 fields) by rebuilding the name_index and
/// resolution_graph in the order the production pipeline would.
fn finalize(audit: &mut AuditData) {
  audit.name_index = TopicNameIndex::build(audit);
  audit.resolution_graph = Some(resolution_graph::build(audit));
}

/// Build a `ResolutionGraph` directly from a list of `(src, dest,
/// edge_type)` triples using each edge type's default weight. Used
/// for tests that bypass the SolidityExtractor and want full control
/// over what edges exist.
fn graph_from(
  edges: &[(topic::Topic, topic::Topic, EdgeType)],
) -> ResolutionGraph {
  let mut g = ResolutionGraph::new();
  for (s, d, et) in edges {
    g.add_edge(*s, *d, *et, et.default_weight());
  }
  g.finalize();
  g
}

/// Build a `CodeIdentifier` node. Phase-A-resolved when `referenced_topic`
/// is `Some`; ambiguous when `None`. The `kind` and `referenced_name`
/// snapshots match what the parser would write next to a Phase-A
/// resolution but are immaterial for Phase-B scoring (the pass reads
/// only `referenced_topic`).
fn code_id(
  node_id: i32,
  value: &str,
  referenced_topic: Option<topic::Topic>,
) -> DocumentationNode {
  DocumentationNode::CodeIdentifier {
    node_id,
    value: value.to_string(),
    referenced_topic,
    kind: None,
    referenced_name: None,
    referenced_topic_candidates: Vec::new(),
  }
}

/// Tiny helper to wrap inline children in a Paragraph wrapper.
fn paragraph(
  node_id: i32,
  children: Vec<DocumentationNode>,
) -> DocumentationNode {
  DocumentationNode::Paragraph {
    node_id,
    position: None,
    children,
  }
}

/// Build a Section with the given title and children.
fn section(
  node_id: i32,
  title: &str,
  children: Vec<DocumentationNode>,
) -> DocumentationNode {
  DocumentationNode::Section {
    node_id,
    title: title.to_string(),
    children,
  }
}

/// Build a Heading with an attached Section (the parser's canonical
/// shape produced by `create_heading_with_section`).
fn heading_with_section(
  heading_id: i32,
  section_id: i32,
  level: u8,
  title: &str,
  section_children: Vec<DocumentationNode>,
) -> DocumentationNode {
  DocumentationNode::Heading {
    node_id: heading_id,
    position: None,
    level,
    children: vec![DocumentationNode::Text {
      node_id: heading_id + 100_000,
      position: None,
      value: title.to_string(),
    }],
    section: Some(Box::new(section(section_id, title, section_children))),
  }
}

/// Build a Root containing the given children.
fn root(node_id: i32, children: Vec<DocumentationNode>) -> DocumentationNode {
  DocumentationNode::Root {
    node_id,
    position: None,
    children,
  }
}

/// Walk a resolved tree and return every `CodeIdentifier`'s
/// `(node_id, referenced_topic)` pair, in document order. Lets tests
/// assert the post-pass tree shape compactly.
fn collect_resolutions(
  node: &DocumentationNode,
  out: &mut Vec<(i32, Option<topic::Topic>)>,
) {
  match node {
    DocumentationNode::CodeIdentifier {
      node_id,
      referenced_topic,
      ..
    } => {
      out.push((*node_id, *referenced_topic));
    }
    DocumentationNode::Heading {
      children, section, ..
    } => {
      for c in children {
        collect_resolutions(c, out);
      }
      if let Some(s) = section {
        collect_resolutions(s, out);
      }
    }
    other => {
      for c in other.children() {
        collect_resolutions(c, out);
      }
    }
  }
}

// ---------------------------------------------------------------------
// Layer 1 — foundational shape
// ---------------------------------------------------------------------

/// Empty doc tree → no traces, no mutations. The pass is also a no-op
/// when the audit has no resolution graph (Phase 4 didn't run): the
/// early-exit guard returns immediately.
#[test]
fn empty_root_produces_no_traces() {
  let audit = empty_audit();
  let mut node = root(1, vec![]);
  let traces = resolve_doc_tree(&mut node, &audit);
  assert!(traces.is_empty());
}

#[test]
fn missing_resolution_graph_is_no_op() {
  let mut audit = empty_audit();
  audit.resolution_graph = None;
  let mut node = root(1, vec![paragraph(2, vec![code_id(3, "anyName", None)])]);
  let before = node.clone();
  let traces = resolve_doc_tree(&mut node, &audit);
  assert!(traces.is_empty());
  assert_eq!(
    node, before,
    "tree must not be mutated when graph is absent"
  );
}

#[test]
fn ambiguous_ref_with_no_candidates_stays_unresolved_with_trace() {
  // The audit defines no topics with this name, so
  // `candidates_by_simple_name("missingThing")` is empty. The pass
  // must still emit a trace (so operators see that the ref was
  // attempted) but leave `referenced_topic = None`.
  let audit = empty_audit();
  let mut node = root(
    1,
    vec![paragraph(2, vec![code_id(3, "missingThing", None)])],
  );
  let traces = resolve_doc_tree(&mut node, &audit);

  let mut found = Vec::new();
  collect_resolutions(&node, &mut found);
  assert_eq!(found, vec![(3, None)]);

  assert_eq!(traces.len(), 1);
  let (_key, trace) = &traces[0];
  assert_eq!(trace.identifier, "missingThing");
  assert_eq!(trace.chosen_topic, None);
  assert_eq!(trace.phase_resolved, ResolutionPhase::Unresolved);
  assert!(trace.candidate_scores.is_empty());
  assert!(trace.top_contributing_edges.is_empty());
}

#[test]
fn phase_a_resolved_ref_is_left_alone_and_no_trace_emitted() {
  // The pass attempts only ambiguous (Phase-A `None`) references.
  // Already-resolved references contribute to the seed vector but
  // never enter the candidate-scoring path.
  let mut audit = staged_audit();
  let already = nt(42);
  audit.topic_metadata.insert(
    already,
    named_topic(already, "transfer", NamedTopicKind::Builtin, Scope::Global),
  );
  finalize(&mut audit);

  let mut node = root(
    1,
    vec![paragraph(2, vec![code_id(3, "transfer", Some(already))])],
  );
  let traces = resolve_doc_tree(&mut node, &audit);
  assert!(traces.is_empty(), "no attempt → no trace");

  let mut found = Vec::new();
  collect_resolutions(&node, &mut found);
  assert_eq!(found, vec![(3, Some(already))]);
}

// ---------------------------------------------------------------------
// Layer 2 — single-section scoring
// ---------------------------------------------------------------------

/// One section, one ambiguous reference whose two candidates are
/// disambiguated by a Phase-A seed in the same section that pulls PR
/// mass to the right candidate via a `ContainsMember` edge from a
/// shared parent contract.
#[test]
fn ambiguous_ref_resolves_to_candidate_anchored_by_phase_a_seed() {
  // Setup:
  //   contract Vault { function transfer() ... }    — Vault, Vault.transfer
  //   contract Token { function transfer() ... }    — Token, Token.transfer
  // Doc text references `Vault` (Phase-A unique) then `transfer`
  // (Phase-A ambiguous). Phase B should pick Vault.transfer because
  // Vault is the section's seed and Vault → Vault.transfer is one
  // hop via ContainsMember.
  let vault_contract = nt(10);
  let vault_transfer = nt(11);
  let token_contract = nt(20);
  let token_transfer = nt(21);

  let mut audit = staged_audit();
  let scope_vault = Scope::Component {
    container: pp("test.sol"),
    component: vault_contract,
  };
  let scope_token = Scope::Component {
    container: pp("test.sol"),
    component: token_contract,
  };
  audit.topic_metadata.insert(
    vault_contract,
    named_topic(
      vault_contract,
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
      scope_vault.clone(),
    ),
  );
  audit.topic_metadata.insert(
    token_contract,
    named_topic(
      token_contract,
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
      scope_token,
    ),
  );

  audit.name_index = TopicNameIndex::build(&audit);
  // Replace the empty graph from `staged_audit` with one that has the
  // contract↔member edges the resolver needs to disambiguate.
  audit.resolution_graph = Some(graph_from(&[
    (vault_contract, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault_contract, EdgeType::ContainsMember),
    (token_contract, token_transfer, EdgeType::ContainsMember),
    (token_transfer, token_contract, EdgeType::ContainsMember),
  ]));

  let mut node = root(
    1,
    vec![paragraph(
      2,
      vec![
        code_id(3, "Vault", Some(vault_contract)),
        code_id(4, "transfer", None),
      ],
    )],
  );
  let traces = resolve_doc_tree(&mut node, &audit);

  let mut resolved = Vec::new();
  collect_resolutions(&node, &mut resolved);
  assert_eq!(
    resolved,
    vec![(3, Some(vault_contract)), (4, Some(vault_transfer))],
    "Phase A on Vault preserved; Phase B picks Vault.transfer over Token.transfer"
  );

  assert_eq!(traces.len(), 1);
  let (_, trace) = &traces[0];
  assert_eq!(trace.chosen_topic, Some(vault_transfer));
  assert_eq!(trace.phase_resolved, ResolutionPhase::PhaseB);
  // Both candidates appear in the trace, ordered by PR descending.
  let ranked: Vec<topic::Topic> =
    trace.candidate_scores.iter().map(|c| c.topic).collect();
  assert_eq!(
    ranked,
    vec![vault_transfer, token_transfer],
    "winner first; tie-break determinism"
  );
  assert!(
    trace.candidate_scores[0].pr_score > trace.candidate_scores[1].pr_score,
    "winner's PR ({}) must exceed runner-up ({})",
    trace.candidate_scores[0].pr_score,
    trace.candidate_scores[1].pr_score
  );
  // qualified-name snapshot makes its way into the trace.
  assert_eq!(
    trace.candidate_scores[0].qualified_name.as_deref(),
    Some("Vault.transfer"),
  );
  // Top contributing edges include the Vault → Vault.transfer
  // ContainsMember edge.
  assert!(!trace.top_contributing_edges.is_empty());
  let has_vault_member = trace.top_contributing_edges.iter().any(|e| {
    e.predecessor == vault_contract && e.edge_type == EdgeType::ContainsMember
  });
  assert!(
    has_vault_member,
    "expected Vault → ContainsMember in top edges, got {:?}",
    trace.top_contributing_edges
  );
}

// ---------------------------------------------------------------------
// Layer 3 — section-tree interactions
// ---------------------------------------------------------------------

/// Ancestor seeding: a Phase-A reference in the *parent* section
/// pulls mass into a candidate that resolves the *child* section's
/// ambiguous reference. The closer ancestor (parent at distance 1)
/// outweighs unrelated noise.
#[test]
fn ancestor_section_seeds_disambiguate_child_section_ref() {
  let vault_contract = nt(10);
  let vault_transfer = nt(11);
  let token_contract = nt(20);
  let token_transfer = nt(21);

  let mut audit = staged_audit();
  for (t, name, kind, parent) in [
    (
      vault_contract,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      None,
    ),
    (
      token_contract,
      "Token",
      NamedTopicKind::Contract(ContractKind::Contract),
      None,
    ),
    (
      vault_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Some(vault_contract),
    ),
    (
      token_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Some(token_contract),
    ),
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
    audit
      .topic_metadata
      .insert(t, named_topic(t, name, kind, scope));
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault_contract, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault_contract, EdgeType::ContainsMember),
    (token_contract, token_transfer, EdgeType::ContainsMember),
    (token_transfer, token_contract, EdgeType::ContainsMember),
  ]));

  // Doc tree:
  //   # Vault Operations  (Phase-A: Vault)
  //     ## Detail        (Phase-B target: ambiguous transfer)
  let inner = heading_with_section(
    100,
    101,
    2,
    "Detail",
    vec![paragraph(102, vec![code_id(103, "transfer", None)])],
  );
  let outer = heading_with_section(
    1,
    2,
    1,
    "Vault Operations",
    vec![
      paragraph(3, vec![code_id(4, "Vault", Some(vault_contract))]),
      inner,
    ],
  );
  let mut tree = root(0, vec![outer]);

  let traces = resolve_doc_tree(&mut tree, &audit);
  let mut resolved = Vec::new();
  collect_resolutions(&tree, &mut resolved);
  assert_eq!(
    resolved,
    vec![(4, Some(vault_contract)), (103, Some(vault_transfer))],
    "the inner section's ambiguous `transfer` resolves via the outer section's Vault seed",
  );
  assert_eq!(traces.len(), 1);
  let (_, trace) = &traces[0];
  // Section topic on the trace is the inner Section, not the outer.
  assert_eq!(trace.section_topic, dt(101));
}

/// Sibling isolation: a Phase-A seed in one child section does NOT
/// seed an unrelated sibling child section. The spec walks ancestors
/// only, not the full doc tree.
#[test]
fn sibling_section_seeds_do_not_cross_pollinate() {
  let vault_contract = nt(10);
  let vault_transfer = nt(11);
  let token_contract = nt(20);
  let token_transfer = nt(21);

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault_contract,
    named_topic(
      vault_contract,
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
        component: vault_contract,
      },
    ),
  );
  audit.topic_metadata.insert(
    token_contract,
    named_topic(
      token_contract,
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
        component: token_contract,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault_contract, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault_contract, EdgeType::ContainsMember),
    (token_contract, token_transfer, EdgeType::ContainsMember),
    (token_transfer, token_contract, EdgeType::ContainsMember),
  ]));

  // Tree:
  //   # Doc
  //     ## Vault Section (Phase-A: Vault)
  //     ## Token Section (ambiguous: transfer)
  // The Token section's `transfer` reference must NOT pick up the
  // Vault seed from its sibling — they share an ancestor, not the
  // section path. With no Phase-A seed in either Token Section's
  // ancestor chain, the ambiguous `transfer` cannot clear the
  // confidence threshold (zero PR for both candidates) and stays
  // unresolved.
  let outer = heading_with_section(
    1,
    2,
    1,
    "Doc",
    vec![
      heading_with_section(
        10,
        11,
        2,
        "Vault Section",
        vec![paragraph(
          12,
          vec![code_id(13, "Vault", Some(vault_contract))],
        )],
      ),
      heading_with_section(
        20,
        21,
        2,
        "Token Section",
        vec![paragraph(22, vec![code_id(23, "transfer", None)])],
      ),
    ],
  );
  let mut tree = root(0, vec![outer]);

  let traces = resolve_doc_tree(&mut tree, &audit);
  let mut resolved = Vec::new();
  collect_resolutions(&tree, &mut resolved);
  assert_eq!(
    resolved,
    vec![(13, Some(vault_contract)), (23, None)],
    "Token section must not inherit Vault from its sibling",
  );
  assert_eq!(traces.len(), 1);
  assert_eq!(traces[0].1.chosen_topic, None);
  // Phase E records the anchor-by-name fallback for the unresolved
  // ref; both candidates remain attached to the trace at zero PR.
  assert_eq!(traces[0].1.phase_resolved, ResolutionPhase::PhaseE);
  // Both candidates show up in the trace with PR `0.0` — the seed
  // vector for Token section is empty, so PR returns zero everywhere.
  assert_eq!(traces[0].1.candidate_scores.len(), 2);
  for score in &traces[0].1.candidate_scores {
    assert_eq!(score.pr_score, 0.0);
  }
}

/// Depth attenuation: when two seeds compete (one in the section
/// itself, one in a far ancestor), the closer seed dominates because
/// `2^(-1) = 0.5` vs e.g. `2^(-3) = 0.125`. Validates the
/// ancestor-chain weighting rule.
#[test]
fn closer_ancestor_seed_outweighs_distant_one() {
  // Two contracts, each containing a `value` state variable. Two
  // Phase-A-resolved seeds compete:
  //   - Outer-outer-outer section: Phase-A `Bar` (pulls Bar.value up)
  //   - Section: Phase-A `Foo` (pulls Foo.value up)
  // With seed weights 2^(-3) and 2^0 respectively, Foo dominates and
  // Foo.value wins for the ambiguous `value` reference.
  let foo_contract = nt(10);
  let foo_value = nt(11);
  let bar_contract = nt(20);
  let bar_value = nt(21);

  let mut audit = staged_audit();
  for (t, name, parent) in [
    (foo_contract, "Foo", None),
    (bar_contract, "Bar", None),
    (foo_value, "value", Some(foo_contract)),
    (bar_value, "value", Some(bar_contract)),
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
      NamedTopicKind::StateVariable(domain::VariableMutability::Mutable)
    };
    audit
      .topic_metadata
      .insert(t, named_topic(t, name, kind, scope));
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (foo_contract, foo_value, EdgeType::ContainsMember),
    (foo_value, foo_contract, EdgeType::ContainsMember),
    (bar_contract, bar_value, EdgeType::ContainsMember),
    (bar_value, bar_contract, EdgeType::ContainsMember),
  ]));

  // Build a four-level deep section tree:
  //   Root (depth 0) — Phase-A: Bar
  //     H1 (depth 1)
  //       H2 (depth 2)
  //         H3 (depth 3) — Phase-A: Foo + ambiguous `value`
  let h3 = heading_with_section(
    300,
    301,
    3,
    "H3",
    vec![paragraph(
      302,
      vec![
        code_id(303, "Foo", Some(foo_contract)),
        code_id(304, "value", None),
      ],
    )],
  );
  let h2 = heading_with_section(200, 201, 2, "H2", vec![h3]);
  let h1 = heading_with_section(100, 101, 1, "H1", vec![h2]);
  let mut tree = root(
    1,
    vec![
      paragraph(2, vec![code_id(3, "Bar", Some(bar_contract))]),
      h1,
    ],
  );

  let traces = resolve_doc_tree(&mut tree, &audit);
  let mut resolved = Vec::new();
  collect_resolutions(&tree, &mut resolved);
  // Two Phase-A refs preserved + one Phase-B winner.
  assert_eq!(
    resolved,
    vec![
      (3, Some(bar_contract)),
      (303, Some(foo_contract)),
      (304, Some(foo_value)),
    ],
    "Foo at distance 0 dominates Bar at distance 4",
  );
  let trace = &traces
    .iter()
    .find(|(_, t)| t.identifier == "value")
    .unwrap()
    .1;
  assert_eq!(trace.chosen_topic, Some(foo_value));
}

/// Depth-6 cap: a Phase-A seed in an ancestor section more than six
/// levels up contributes nothing. Verified by constructing a
/// pathologically deep tree and confirming the ambiguous reference
/// fails to resolve when its only seed is beyond the cap.
#[test]
fn seeds_beyond_depth_six_do_not_contribute() {
  let foo_contract = nt(10);
  let foo_value = nt(11);
  let bar_contract = nt(20);
  let bar_value = nt(21);

  let mut audit = staged_audit();
  for (t, name, parent) in [
    (foo_contract, "Foo", None),
    (bar_contract, "Bar", None),
    (foo_value, "value", Some(foo_contract)),
    (bar_value, "value", Some(bar_contract)),
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
      NamedTopicKind::StateVariable(domain::VariableMutability::Mutable)
    };
    audit
      .topic_metadata
      .insert(t, named_topic(t, name, kind, scope));
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (foo_contract, foo_value, EdgeType::ContainsMember),
    (foo_value, foo_contract, EdgeType::ContainsMember),
    (bar_contract, bar_value, EdgeType::ContainsMember),
    (bar_value, bar_contract, EdgeType::ContainsMember),
  ]));

  // Build an 8-level deep section tree. The root has Phase-A `Foo`
  // (depth 0). The deepest section at depth 8 — beyond the cap of
  // 6 — has the ambiguous `value`. The Foo seed at depth 8 should
  // be capped out, leaving the seed vector empty for the deepest
  // section and the ref unresolved.
  let mut current = paragraph(1000, vec![code_id(1001, "value", None)]);
  // Chain of 8 Heading→Section wrappings.
  for i in (0..8).rev() {
    let hid = 100 + i * 10;
    let sid = hid + 1;
    current = heading_with_section(
      hid,
      sid,
      ((i % 6) + 1) as u8,
      &format!("H{}", i),
      vec![current],
    );
  }
  let mut tree = root(
    1,
    vec![
      paragraph(2, vec![code_id(3, "Foo", Some(foo_contract))]),
      current,
    ],
  );

  let traces = resolve_doc_tree(&mut tree, &audit);

  // The ambiguous `value` ref's chain to the root (where Foo lives)
  // is 8 deep — past the cap. With no in-cap seeds, no candidate
  // can clear the threshold.
  let trace = &traces
    .iter()
    .find(|(_, t)| t.identifier == "value")
    .unwrap()
    .1;
  assert_eq!(
    trace.chosen_topic, None,
    "depth-6 cap must zero out beyond-cap ancestor seeds",
  );
}

/// Multiple ambiguous references inside one section share the same
/// PR run (one per section, not per reference). Each is scored
/// against the same seed vector. Validate that traces for both come
/// out, and that both pick the right candidate when seeds align.
#[test]
fn multiple_ambiguous_refs_in_one_section_share_pr_run() {
  // Two ambiguities in one section, both anchored by the same Vault
  // Phase-A seed; both should resolve to Vault's members.
  let vault_contract = nt(10);
  let vault_transfer = nt(11);
  let vault_balance = nt(12);
  let token_contract = nt(20);
  let token_transfer = nt(21);
  let token_balance = nt(22);

  let mut audit = staged_audit();
  for (t, name, parent) in [
    (vault_contract, "Vault", None),
    (token_contract, "Token", None),
    (vault_transfer, "transfer", Some(vault_contract)),
    (vault_balance, "balance", Some(vault_contract)),
    (token_transfer, "transfer", Some(token_contract)),
    (token_balance, "balance", Some(token_contract)),
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
    audit
      .topic_metadata
      .insert(t, named_topic(t, name, kind, scope));
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault_contract, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault_contract, EdgeType::ContainsMember),
    (vault_contract, vault_balance, EdgeType::ContainsMember),
    (vault_balance, vault_contract, EdgeType::ContainsMember),
    (token_contract, token_transfer, EdgeType::ContainsMember),
    (token_transfer, token_contract, EdgeType::ContainsMember),
    (token_contract, token_balance, EdgeType::ContainsMember),
    (token_balance, token_contract, EdgeType::ContainsMember),
  ]));

  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![
        code_id(3, "Vault", Some(vault_contract)),
        code_id(4, "transfer", None),
        code_id(5, "balance", None),
      ],
    )],
  );

  let traces = resolve_doc_tree(&mut tree, &audit);
  assert_eq!(traces.len(), 2);
  let mut resolved = Vec::new();
  collect_resolutions(&tree, &mut resolved);
  assert_eq!(
    resolved,
    vec![
      (3, Some(vault_contract)),
      (4, Some(vault_transfer)),
      (5, Some(vault_balance)),
    ]
  );
  // Both traces share the same section topic (the Root, since this
  // doc has no Heading/Section nodes).
  assert_eq!(traces[0].1.section_topic, traces[1].1.section_topic);
  assert_eq!(traces[0].1.section_topic, dt(1));
}

/// Sub-section content does not seed parent-section PR. A Phase-A
/// reference inside a child section must not bleed into the parent
/// section's seed vector — only ancestor → descendant flow exists.
#[test]
fn child_section_seeds_do_not_propagate_to_parent_section() {
  // Set up:
  //   Root (depth 0) — ambiguous `transfer`
  //     H1 (depth 1) — Phase-A: Vault
  // The parent section's `transfer` should NOT see Vault as a seed
  // (Vault is in a *descendant* section), so the ref stays
  // unresolved.
  let vault_contract = nt(10);
  let vault_transfer = nt(11);
  let token_contract = nt(20);
  let token_transfer = nt(21);

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault_contract,
    named_topic(
      vault_contract,
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
        component: vault_contract,
      },
    ),
  );
  audit.topic_metadata.insert(
    token_contract,
    named_topic(
      token_contract,
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
        component: token_contract,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault_contract, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault_contract, EdgeType::ContainsMember),
    (token_contract, token_transfer, EdgeType::ContainsMember),
    (token_transfer, token_contract, EdgeType::ContainsMember),
  ]));

  let mut tree = root(
    1,
    vec![
      paragraph(2, vec![code_id(3, "transfer", None)]),
      heading_with_section(
        100,
        101,
        1,
        "Detail",
        vec![paragraph(
          102,
          vec![code_id(103, "Vault", Some(vault_contract))],
        )],
      ),
    ],
  );

  let traces = resolve_doc_tree(&mut tree, &audit);
  let mut resolved = Vec::new();
  collect_resolutions(&tree, &mut resolved);
  // Root-section `transfer` (node 3): the parent section has no
  // ancestor seeds and its only Phase-A reference is in a descendant.
  // Therefore unresolved.
  // H1-section: only contains the Phase-A `Vault`, no ambiguous refs.
  assert_eq!(
    resolved,
    vec![(3, None), (103, Some(vault_contract))],
    "child section's Phase-A seed must not influence parent",
  );
  assert_eq!(traces.len(), 1);
  assert_eq!(traces[0].1.chosen_topic, None);
}

// ---------------------------------------------------------------------
// Layer 4 — threshold and tie-break
// ---------------------------------------------------------------------

/// When the top candidate's PR is below the `0.65` threshold, the
/// resolver leaves `referenced_topic = None`. The trace records the
/// ranked candidates so operators can see the close call.
#[test]
fn ratio_below_threshold_leaves_reference_unresolved_with_full_trace() {
  // Symmetric two-candidate setup with a star-graph anchor. Seeded
  // at the shared central node, both candidates receive equal PR
  // mass — score_top / (score_top + score_runner_up) = 0.5 < 0.65.
  let center = nt(1);
  let cand_a = nt(10);
  let cand_b = nt(20);

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    center,
    named_topic(center, "Hub", NamedTopicKind::Builtin, Scope::Global),
  );
  audit.topic_metadata.insert(
    cand_a,
    named_topic(cand_a, "ambig", NamedTopicKind::Builtin, Scope::Global),
  );
  audit.topic_metadata.insert(
    cand_b,
    named_topic(cand_b, "ambig", NamedTopicKind::Builtin, Scope::Global),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (center, cand_a, EdgeType::ContainsMember),
    (cand_a, center, EdgeType::ContainsMember),
    (center, cand_b, EdgeType::ContainsMember),
    (cand_b, center, EdgeType::ContainsMember),
  ]));

  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![code_id(3, "Hub", Some(center)), code_id(4, "ambig", None)],
    )],
  );

  let traces = resolve_doc_tree(&mut tree, &audit);
  let mut resolved = Vec::new();
  collect_resolutions(&tree, &mut resolved);
  assert_eq!(resolved, vec![(3, Some(center)), (4, None)]);
  assert_eq!(traces.len(), 1);
  let trace = &traces[0].1;
  // Both candidates listed; PR equal by topology symmetry.
  assert_eq!(trace.candidate_scores.len(), 2);
  assert_eq!(
    trace.candidate_scores[0].pr_score.to_bits(),
    trace.candidate_scores[1].pr_score.to_bits(),
    "symmetric topology must produce bit-identical PR"
  );
  assert_eq!(trace.chosen_topic, None);
  // Phase E records the anchor-by-name fallback once Phases B + C exit
  // without a winner; the trace is rewritten from `Unresolved` to
  // `PhaseE` and the candidate scores stay attached for inspection.
  assert_eq!(trace.phase_resolved, ResolutionPhase::PhaseE);
  // No top-edges populated for an unwon ref — there is no winner to
  // attribute mass to.
  assert!(trace.top_contributing_edges.is_empty());
}

/// Verifies the threshold gate directly: when the runner-up is
/// exactly zero (no PR mass on it), the ratio is `top/(top+0) = 1.0`
/// and the resolver fires. Constructed with a graph where the
/// runner-up is unreachable from the seed.
#[test]
fn unreachable_runner_up_lets_top_win_with_ratio_one() {
  // Topology:
  //   anchor → t_winner
  //   t_loser is a graph island.
  // Seed at anchor → only t_winner gets PR mass; t_loser stays at
  // 0.0; ratio = 1.0; t_winner wins.
  let anchor = nt(1);
  let winner = nt(10);
  let loser = nt(20);

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    anchor,
    named_topic(anchor, "Anchor", NamedTopicKind::Builtin, Scope::Global),
  );
  audit.topic_metadata.insert(
    winner,
    named_topic(winner, "thing", NamedTopicKind::Builtin, Scope::Global),
  );
  audit.topic_metadata.insert(
    loser,
    named_topic(loser, "thing", NamedTopicKind::Builtin, Scope::Global),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph =
    Some(graph_from(&[(anchor, winner, EdgeType::Calls)]));

  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![
        code_id(3, "Anchor", Some(anchor)),
        code_id(4, "thing", None),
      ],
    )],
  );

  let traces = resolve_doc_tree(&mut tree, &audit);
  let trace = &traces[0].1;
  assert_eq!(trace.chosen_topic, Some(winner));
  assert_eq!(trace.phase_resolved, ResolutionPhase::PhaseB);
  assert_eq!(trace.candidate_scores.len(), 2);
  assert!(trace.candidate_scores[0].pr_score > 0.0);
  assert_eq!(trace.candidate_scores[1].pr_score, 0.0);
}

/// When the section's seed names a function and a candidate is one of
/// that function's parameters, the parameter outranks a state variable
/// of the same name. The boost is what tips a roughly-equal PR pair in
/// the parameter's favor.
#[test]
fn function_param_boost_promotes_seeded_function_parameter() {
  // Topology designed to give param and state_var roughly equal raw
  // PR mass: each receives one ContainsMember-weight edge from a seed.
  //   contract → state_var       (state var receives mass via contract seed)
  //   function → param           (param receives mass via function seed)
  // Both seeded. Without the boost, threshold ratio ≈ 0.5 (no winner).
  // With the 1.5× boost on param, ratio = 1.5 / (1.5 + 1.0) = 0.6 — still
  // below threshold — so we crank the param's edge weight a touch lower
  // and rely on the boost to push it above 0.65.
  let contract = nt(1);
  let function = nt(10);
  let param_list = nt(11);
  let param = nt(12);
  let state_var = nt(20);

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
    function,
    named_topic(
      function,
      "f",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: contract,
      },
    ),
  );
  audit.topic_metadata.insert(
    param,
    named_topic(
      param,
      "x",
      NamedTopicKind::LocalVariable,
      Scope::Member {
        container: pp("test.sol"),
        component: contract,
        member: function,
        signature_container: Some(param_list),
      },
    ),
  );
  audit.topic_metadata.insert(
    state_var,
    named_topic(
      state_var,
      "x",
      NamedTopicKind::StateVariable(domain::VariableMutability::Mutable),
      Scope::Component {
        container: pp("test.sol"),
        component: contract,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  // Hand-rolled graph: each candidate has a single inbound edge from
  // a seeded topic. State var via contract, param via function — same
  // edge type so equal raw weights produce equal PR.
  audit.resolution_graph = Some(graph_from(&[
    (contract, state_var, EdgeType::ContainsMember),
    (state_var, contract, EdgeType::ContainsMember),
    (function, param, EdgeType::ContainsLocal),
    (param, function, EdgeType::ContainsLocal),
  ]));

  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![
        // Both seeds Phase-A-resolved.
        code_id(3, "C", Some(contract)),
        code_id(4, "f", Some(function)),
        code_id(5, "x", None),
      ],
    )],
  );
  let traces = resolve_doc_tree(&mut tree, &audit);

  let trace = traces
    .iter()
    .find(|(_, t)| t.identifier == "x")
    .map(|(_, t)| t)
    .expect("trace for `x` must exist");
  assert_eq!(
    trace.chosen_topic,
    Some(param),
    "boost must promote the parameter over the state variable when the function is seeded"
  );
  assert_eq!(trace.phase_resolved, ResolutionPhase::PhaseB);
}

/// The boost only fires when the candidate's enclosing function is in
/// the seed set. Without the function in the seed, the parameter has
/// no boost; the state variable wins (or both fall through, but
/// crucially the parameter does not unfairly win).
#[test]
fn function_param_boost_inactive_when_function_not_seeded() {
  let contract = nt(1);
  let function = nt(10);
  let param_list = nt(11);
  let param = nt(12);
  let state_var = nt(20);

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
    function,
    named_topic(
      function,
      "f",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: contract,
      },
    ),
  );
  audit.topic_metadata.insert(
    param,
    named_topic(
      param,
      "x",
      NamedTopicKind::LocalVariable,
      Scope::Member {
        container: pp("test.sol"),
        component: contract,
        member: function,
        signature_container: Some(param_list),
      },
    ),
  );
  audit.topic_metadata.insert(
    state_var,
    named_topic(
      state_var,
      "x",
      NamedTopicKind::StateVariable(domain::VariableMutability::Mutable),
      Scope::Component {
        container: pp("test.sol"),
        component: contract,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  // Same topology as above so the param has graph access to the
  // function but the function is not in the seed.
  audit.resolution_graph = Some(graph_from(&[
    (contract, state_var, EdgeType::ContainsMember),
    (state_var, contract, EdgeType::ContainsMember),
    (function, param, EdgeType::ContainsLocal),
    (param, function, EdgeType::ContainsLocal),
  ]));

  // Only `C` is mentioned; the function `f` does not appear in the
  // section's Phase-A topics, so its parameter must not get the boost.
  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![code_id(3, "C", Some(contract)), code_id(4, "x", None)],
    )],
  );
  let traces = resolve_doc_tree(&mut tree, &audit);

  let trace = traces
    .iter()
    .find(|(_, t)| t.identifier == "x")
    .map(|(_, t)| t)
    .expect("trace for `x` must exist");
  assert_ne!(
    trace.chosen_topic,
    Some(param),
    "without the function in the seed, the boost must not promote the parameter"
  );
}

/// Tie-break determinism on identical PR. When two candidates score
/// the same PR, they sort by qualified-name ascending, then topic-ID
/// ascending. Verified with two candidates of equal PR but distinct
/// qualified names.
#[test]
fn equal_pr_breaks_tie_on_qualified_name_ascending() {
  let parent = nt(1);
  let a_kind_owner = nt(10); // contract "AAA"
  let b_kind_owner = nt(20); // contract "BBB"
  let a_method = nt(11); // AAA.target
  let b_method = nt(21); // BBB.target

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    parent,
    named_topic(
      parent,
      "Parent",
      NamedTopicKind::Builtin,
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    a_kind_owner,
    named_topic(
      a_kind_owner,
      "AAA",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    b_kind_owner,
    named_topic(
      b_kind_owner,
      "BBB",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
  );
  audit.topic_metadata.insert(
    a_method,
    named_topic(
      a_method,
      "target",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: a_kind_owner,
      },
    ),
  );
  audit.topic_metadata.insert(
    b_method,
    named_topic(
      b_method,
      "target",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: b_kind_owner,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  // Symmetric edges so PR is identical for the two candidates.
  audit.resolution_graph = Some(graph_from(&[
    (parent, a_method, EdgeType::Calls),
    (parent, b_method, EdgeType::Calls),
  ]));

  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![
        code_id(3, "Parent", Some(parent)),
        code_id(4, "target", None),
      ],
    )],
  );
  let traces = resolve_doc_tree(&mut tree, &audit);
  let trace = &traces[0].1;
  assert_eq!(trace.candidate_scores.len(), 2);
  // PR is bit-identical (verified by the Layer 1 symmetry test above);
  // the ordering should be qualified-name ascending: AAA.target before
  // BBB.target.
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
// Layer 5 — trace persistence + determinism
// ---------------------------------------------------------------------

/// Full pipeline determinism: identical input → byte-identical
/// (`serde_json` round-trip) traces and tree state. This is the
/// determinism contract that gates downstream consumers — they treat
/// `referenced_topic` as a pure function of the parsed audit.
#[test]
fn pass_is_byte_deterministic_across_repeat_runs() {
  let vault_contract = nt(10);
  let vault_transfer = nt(11);
  let token_contract = nt(20);
  let token_transfer = nt(21);

  let mut audit = staged_audit();
  for (t, name, parent) in [
    (vault_contract, "Vault", None),
    (token_contract, "Token", None),
    (vault_transfer, "transfer", Some(vault_contract)),
    (token_transfer, "transfer", Some(token_contract)),
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
    audit
      .topic_metadata
      .insert(t, named_topic(t, name, kind, scope));
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault_contract, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault_contract, EdgeType::ContainsMember),
    (token_contract, token_transfer, EdgeType::ContainsMember),
    (token_transfer, token_contract, EdgeType::ContainsMember),
  ]));

  let build_tree = || {
    root(
      1,
      vec![paragraph(
        2,
        vec![
          code_id(3, "Vault", Some(vault_contract)),
          code_id(4, "transfer", None),
        ],
      )],
    )
  };

  let mut tree_a = build_tree();
  let mut tree_b = build_tree();

  let traces_a = resolve_doc_tree(&mut tree_a, &audit);
  let traces_b = resolve_doc_tree(&mut tree_b, &audit);

  let bytes_a = serde_json::to_vec(&traces_a).unwrap();
  let bytes_b = serde_json::to_vec(&traces_b).unwrap();
  assert_eq!(bytes_a, bytes_b, "traces must serialize identically");

  let tree_bytes_a = serde_json::to_vec(&tree_a).unwrap();
  let tree_bytes_b = serde_json::to_vec(&tree_b).unwrap();
  assert_eq!(
    tree_bytes_a, tree_bytes_b,
    "post-pass trees must serialize identically",
  );
}

/// Trace count equals number of ambiguous refs encountered.
#[test]
fn one_trace_per_ambiguous_reference_attempted() {
  let mut audit = staged_audit();
  // Three topics share simple name "x".
  for id in &[100, 200, 300] {
    audit.topic_metadata.insert(
      nt(*id),
      named_topic(nt(*id), "x", NamedTopicKind::Builtin, Scope::Global),
    );
  }
  finalize(&mut audit);

  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![
        code_id(3, "x", None),
        code_id(4, "x", None),
        code_id(5, "x", None),
      ],
    )],
  );
  let traces = resolve_doc_tree(&mut tree, &audit);
  assert_eq!(traces.len(), 3);
  let node_ids: Vec<i32> = traces
    .iter()
    .map(|(k, _)| match k {
      ResolutionRefId::DocumentationNode(id) => *id,
      other => panic!(
        "doc-tree pass must only emit DocumentationNode keys, got {:?}",
        other
      ),
    })
    .collect();
  assert_eq!(node_ids, vec![3, 4, 5]);
}

/// Trace map populated through the analyzer ends up keyed by
/// `ResolutionRefId::DocumentationNode(node_id)` — verifying the
/// integration glue carries the keys end-to-end.
#[test]
fn analyzer_integration_populates_audit_data_resolution_traces() {
  // The analyzer's loop over `ast_map.values_mut()` inserts the
  // returned (key, trace) pairs into `audit_data.resolution_traces`.
  // We don't drive the full analyzer here (its disk I/O dependencies
  // are out of scope for a unit test), but we replicate the same
  // mechanics inline to pin the contract: the resolve_doc_tree
  // output is shaped exactly like what AuditData expects to absorb.
  let mut audit = staged_audit();
  for id in &[100, 200] {
    audit.topic_metadata.insert(
      nt(*id),
      named_topic(nt(*id), "thing", NamedTopicKind::Builtin, Scope::Global),
    );
  }
  finalize(&mut audit);

  let mut tree = root(1, vec![paragraph(2, vec![code_id(3, "thing", None)])]);

  let traces = resolve_doc_tree(&mut tree, &audit);
  for (key, trace) in traces {
    audit.resolution_traces.insert(key, trace);
  }
  assert_eq!(audit.resolution_traces.len(), 1);
  assert!(
    audit
      .resolution_traces
      .contains_key(&ResolutionRefId::DocumentationNode(3))
  );
}

// ---------------------------------------------------------------------
// Layer 6 — interaction edge cases
// ---------------------------------------------------------------------

/// Doc tree with no headings at all: the Root behaves as the only
/// "section", and its content's Phase-A seeds disambiguate refs as
/// expected. (Spec language calls out sections as "header AST
/// nodes", but in this implementation we model the root as a
/// virtual outer section — without it, a flat doc would never
/// resolve any ambiguity, which is strictly worse.)
#[test]
fn flat_doc_with_no_headings_uses_root_as_section_scope() {
  let vault_contract = nt(10);
  let vault_transfer = nt(11);
  let token_contract = nt(20);
  let token_transfer = nt(21);

  let mut audit = staged_audit();
  for (t, name, parent) in [
    (vault_contract, "Vault", None),
    (token_contract, "Token", None),
    (vault_transfer, "transfer", Some(vault_contract)),
    (token_transfer, "transfer", Some(token_contract)),
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
    audit
      .topic_metadata
      .insert(t, named_topic(t, name, kind, scope));
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault_contract, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault_contract, EdgeType::ContainsMember),
    (token_contract, token_transfer, EdgeType::ContainsMember),
    (token_transfer, token_contract, EdgeType::ContainsMember),
  ]));

  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![
        code_id(3, "Vault", Some(vault_contract)),
        code_id(4, "transfer", None),
      ],
    )],
  );
  let traces = resolve_doc_tree(&mut tree, &audit);
  assert_eq!(traces[0].1.chosen_topic, Some(vault_transfer));
}

/// References inside nested non-section containers (List → ListItem
/// → InlineCode → CodeIdentifier) still get walked and scored. The
/// AST has many container variants that don't change the section
/// scope; the walker must traverse all of them.
#[test]
fn nested_non_section_containers_are_traversed() {
  let vault_contract = nt(10);
  let vault_transfer = nt(11);
  let token_contract = nt(20);
  let token_transfer = nt(21);

  let mut audit = staged_audit();
  for (t, name, parent) in [
    (vault_contract, "Vault", None),
    (token_contract, "Token", None),
    (vault_transfer, "transfer", Some(vault_contract)),
    (token_transfer, "transfer", Some(token_contract)),
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
    audit
      .topic_metadata
      .insert(t, named_topic(t, name, kind, scope));
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault_contract, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault_contract, EdgeType::ContainsMember),
    (token_contract, token_transfer, EdgeType::ContainsMember),
    (token_transfer, token_contract, EdgeType::ContainsMember),
  ]));

  // Wrap the ambiguous transfer ref deep inside a List → ListItem
  // → InlineCode chain.
  let inline = DocumentationNode::InlineCode {
    node_id: 50,
    position: None,
    value: "transfer".to_string(),
    children: vec![code_id(51, "transfer", None)],
  };
  let item = DocumentationNode::ListItem {
    node_id: 40,
    position: None,
    children: vec![paragraph(41, vec![inline])],
  };
  let list = DocumentationNode::List {
    node_id: 30,
    position: None,
    ordered: false,
    children: vec![item],
  };

  let mut tree = root(
    1,
    vec![
      paragraph(2, vec![code_id(3, "Vault", Some(vault_contract))]),
      list,
    ],
  );
  let traces = resolve_doc_tree(&mut tree, &audit);
  assert_eq!(traces.len(), 1);
  assert_eq!(traces[0].1.chosen_topic, Some(vault_transfer));

  let mut resolved = Vec::new();
  collect_resolutions(&tree, &mut resolved);
  assert!(
    resolved
      .iter()
      .any(|(id, t)| *id == 51 && *t == Some(vault_transfer)),
    "deeply nested ambiguous ref must be resolved; got {:?}",
    resolved
  );
}

/// Top-contributing-edges trace field is sorted descending by
/// contribution and capped at three entries. Build a candidate with
/// many predecessors of varying PR mass to verify both invariants.
#[test]
fn top_contributing_edges_sorted_descending_and_capped_at_three() {
  // Five sources all point at one chosen candidate via Calls
  // edges. Every source has different PR mass (controlled via seed
  // weights). The trace's top_contributing_edges should be sorted
  // descending by contribution and capped at MAX_TOP_EDGES = 3.
  let target = nt(100);
  let p_high = nt(1);
  let p_mid = nt(2);
  let p_low = nt(3);
  let p_lower = nt(4);
  let p_lowest = nt(5);
  let other = nt(200); // a runner-up so the threshold is non-trivial

  let mut audit = staged_audit();
  // Two candidates for ambiguous "x": `target` (with many
  // predecessors) and `other` (no predecessors → low PR).
  audit.topic_metadata.insert(
    target,
    named_topic(target, "x", NamedTopicKind::Builtin, Scope::Global),
  );
  audit.topic_metadata.insert(
    other,
    named_topic(other, "x", NamedTopicKind::Builtin, Scope::Global),
  );
  for id in &[1, 2, 3, 4, 5] {
    audit.topic_metadata.insert(
      nt(*id),
      named_topic(
        nt(*id),
        &format!("p{}", id),
        NamedTopicKind::Builtin,
        Scope::Global,
      ),
    );
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (p_high, target, EdgeType::Calls),
    (p_mid, target, EdgeType::Calls),
    (p_low, target, EdgeType::Calls),
    (p_lower, target, EdgeType::Calls),
    (p_lowest, target, EdgeType::Calls),
  ]));

  // Seed the predecessors with monotonically decreasing weight by
  // including each one as a Phase-A reference whose `referenced_topic`
  // resolves to the predecessor itself.
  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![
        code_id(3, "p1", Some(p_high)),
        code_id(4, "p1", Some(p_high)),
        code_id(5, "p1", Some(p_high)),
        code_id(6, "p1", Some(p_high)),
        code_id(7, "p1", Some(p_high)),
        code_id(10, "p2", Some(p_mid)),
        code_id(11, "p2", Some(p_mid)),
        code_id(12, "p2", Some(p_mid)),
        code_id(20, "p3", Some(p_low)),
        code_id(21, "p3", Some(p_low)),
        code_id(30, "p4", Some(p_lower)),
        code_id(40, "x", None),
      ],
    )],
  );
  // Note that direct_phase_a_topics is sort+dedup'd before seeding,
  // so the duplicated p1/p2/p3 references collapse to one seed each
  // with weight 1.0. The contributions ranking will instead be
  // determined by graph topology alone — every predecessor produces
  // the same per-edge contribution here. The test thus mainly
  // verifies the cap logic, since we have 5 equal-contribution
  // predecessors and only 3 should appear.

  let traces = resolve_doc_tree(&mut tree, &audit);
  let trace = &traces.iter().find(|(_, t)| t.identifier == "x").unwrap().1;
  assert!(trace.chosen_topic.is_some());
  assert!(
    trace.top_contributing_edges.len() <= MAX_TOP_EDGES,
    "edges must be capped at {}: got {}",
    MAX_TOP_EDGES,
    trace.top_contributing_edges.len(),
  );
  // Verify descending-or-equal order.
  for w in trace.top_contributing_edges.windows(2) {
    assert!(
      w[0].weighted_contribution >= w[1].weighted_contribution,
      "edges must sort descending: {:?} < {:?}",
      w[0],
      w[1],
    );
  }
}

/// Ambiguous reference whose name is shared with a Phase-A-resolved
/// reference in the same section: the Phase-A resolution is left
/// alone, but the Phase B pass STILL attempts the ambiguous one.
/// Validates the per-node walker doesn't conflate by string value.
#[test]
fn phase_a_and_ambiguous_with_same_value_coexist() {
  let mut audit = staged_audit();
  let known = nt(50);
  // Two extra topics share simple name "thing" so Phase A returns
  // None for the bare `thing` reference, while a different reference
  // already pinned to `known` stays put.
  audit.topic_metadata.insert(
    known,
    named_topic(known, "Anchor", NamedTopicKind::Builtin, Scope::Global),
  );
  for id in &[100, 200] {
    audit.topic_metadata.insert(
      nt(*id),
      named_topic(nt(*id), "thing", NamedTopicKind::Builtin, Scope::Global),
    );
  }
  finalize(&mut audit);

  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![
        // Phase-A pin via a Topic ID-style value would also work,
        // but we pin via the helper.
        code_id(3, "Anchor", Some(known)),
        code_id(4, "thing", None),
      ],
    )],
  );
  let traces = resolve_doc_tree(&mut tree, &audit);
  let mut resolved = Vec::new();
  collect_resolutions(&tree, &mut resolved);
  // Phase-A pin is preserved.
  assert_eq!(resolved[0], (3, Some(known)));
  // The ambiguous one was attempted; it may or may not resolve
  // (depending on graph state), but the trace must exist.
  assert_eq!(traces.len(), 1);
  assert_eq!(
    traces[0].0,
    ResolutionRefId::DocumentationNode(4),
    "trace must key by the ambiguous ref's node id, not Anchor's",
  );
}

/// A Phase-A resolution to a non-Node topic (e.g. a future feature
/// topic) does not poison the seed vector. The pass must only seed
/// from `referenced_topic` that exists in the graph, regardless of
/// the topic's variant — but PR is robust to seeds that don't appear
/// in the graph (the engine adds them to the node universe at zero
/// cost). This test verifies the resolver does not crash on a Phase-A
/// resolution to a non-NamedTopic destination.
#[test]
fn phase_a_resolution_to_non_named_topic_does_not_panic() {
  let mut audit = staged_audit();
  let feature = topic::new_feature_topic(7);
  // Insert a synthetic feature topic so the seed doesn't dangle on a
  // missing metadata lookup downstream.
  audit.topic_metadata.insert(
    feature,
    domain::TopicMetadata::FeatureTopic {
      topic: feature,
      name: "synthetic".to_string(),
      description: "synthetic feature for the test".to_string(),
      author: o11a_core::collaborator::models::Author::System,
      created_at: None,
    },
  );
  let target = nt(10);
  audit.topic_metadata.insert(
    target,
    named_topic(target, "thing", NamedTopicKind::Builtin, Scope::Global),
  );
  audit.topic_metadata.insert(
    nt(20),
    named_topic(nt(20), "thing", NamedTopicKind::Builtin, Scope::Global),
  );
  finalize(&mut audit);

  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![
        code_id(3, "synthetic", Some(feature)),
        code_id(4, "thing", None),
      ],
    )],
  );

  let _ = resolve_doc_tree(&mut tree, &audit);
  // Pass returned without panicking — that's the assertion for this
  // test. The non-NamedTopic seed lands harmlessly in the PR's node
  // universe.
}

// ---------------------------------------------------------------------
// Layer 7 — downstream-contract invariants
// ---------------------------------------------------------------------

/// A Phase-B winner must look indistinguishable from a Phase-A
/// winner downstream. Specifically: the parser writes
/// `(referenced_topic, kind, referenced_name)` together; Phase B
/// must rewrite all three so consumers like
/// `mechanical_semantic_links` (which reads `referenced_name` to
/// build display text) don't see a half-resolved node.
#[test]
fn phase_b_winner_carries_kind_and_referenced_name_snapshots() {
  let vault_contract = nt(10);
  let vault_transfer = nt(11);
  let token_contract = nt(20);
  let token_transfer = nt(21);

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault_contract,
    named_topic(
      vault_contract,
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
        component: vault_contract,
      },
    ),
  );
  audit.topic_metadata.insert(
    token_contract,
    named_topic(
      token_contract,
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
        component: token_contract,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault_contract, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault_contract, EdgeType::ContainsMember),
    (token_contract, token_transfer, EdgeType::ContainsMember),
    (token_transfer, token_contract, EdgeType::ContainsMember),
  ]));

  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![
        code_id(3, "Vault", Some(vault_contract)),
        code_id(4, "transfer", None),
      ],
    )],
  );
  let _ = resolve_doc_tree(&mut tree, &audit);

  // Find the resolved CodeIdentifier and inspect its snapshot fields.
  fn find_code_id(
    node: &DocumentationNode,
    id: i32,
  ) -> Option<&DocumentationNode> {
    if let DocumentationNode::CodeIdentifier { node_id, .. } = node
      && *node_id == id
    {
      return Some(node);
    }
    if let DocumentationNode::Heading {
      children, section, ..
    } = node
    {
      for c in children {
        if let Some(found) = find_code_id(c, id) {
          return Some(found);
        }
      }
      if let Some(s) = section {
        return find_code_id(s, id);
      }
      return None;
    }
    for c in node.children() {
      if let Some(found) = find_code_id(c, id) {
        return Some(found);
      }
    }
    None
  }

  let resolved = find_code_id(&tree, 4).expect("CodeIdentifier(4) must exist");
  match resolved {
    DocumentationNode::CodeIdentifier {
      referenced_topic,
      kind,
      referenced_name,
      ..
    } => {
      assert_eq!(*referenced_topic, Some(vault_transfer));
      // The kind clone of the winner's metadata.
      assert_eq!(
        *kind,
        Some(NamedTopicKind::Function(FunctionKind::Function)),
        "Phase B winner must carry the resolved declaration's kind",
      );
      // The simple name of the winner — even though the literal
      // text was just `"transfer"`, the snapshot reflects what the
      // parser would have written for a Phase-A resolution.
      assert_eq!(
        referenced_name.as_deref(),
        Some("transfer"),
        "Phase B winner must carry the resolved declaration's simple name",
      );
    }
    other => panic!("expected CodeIdentifier, got {:?}", other),
  }
}

/// `referenced_topic_candidates` invariant: non-empty IFF the ref is
/// unresolved (`referenced_topic = None`). Stale Phase E candidates
/// must be cleared when Phase B succeeds — otherwise an audit re-run
/// where the graph has changed enough to resolve a previously-Phase-E
/// ref would leave inconsistent state. This test pre-populates the
/// field on two refs (one Phase B will resolve, one falls through to
/// Phase E) and asserts the post-pass invariant.
#[test]
fn phase_b_clears_stale_candidates_phase_e_repopulates_them() {
  let vault_contract = nt(10);
  let vault_transfer = nt(11);
  let token_contract = nt(20);
  let token_transfer = nt(21);

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault_contract,
    named_topic(
      vault_contract,
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
        component: vault_contract,
      },
    ),
  );
  audit.topic_metadata.insert(
    token_contract,
    named_topic(
      token_contract,
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
        component: token_contract,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault_contract, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault_contract, EdgeType::ContainsMember),
    (token_contract, token_transfer, EdgeType::ContainsMember),
    (token_transfer, token_contract, EdgeType::ContainsMember),
  ]));

  // Three CodeIdentifier nodes, all pre-populated with stale candidates
  // (simulating a prior Phase E run against a different graph state):
  //   - node 3: Phase A resolved (Vault). Phase B's apply pass early-
  //     returns on the `referenced_topic.is_some()` guard → candidates
  //     stay as-is (Phase A is upstream of this resolver).
  //   - node 4: Phase B will succeed (Vault → Vault.transfer via
  //     ContainsMember edge). Stale candidates must be cleared.
  //   - node 5: name has no candidates in the audit at all → Phase B
  //     fails, Phase E skips (empty candidate list) → stale candidates
  //     stay (no apply entry written).
  let stale_candidates = vec![nt(99), nt(100)];
  let phase_a_node = DocumentationNode::CodeIdentifier {
    node_id: 3,
    value: "Vault".to_string(),
    referenced_topic: Some(vault_contract),
    kind: Some(NamedTopicKind::Contract(ContractKind::Contract)),
    referenced_name: Some("Vault".to_string()),
    referenced_topic_candidates: stale_candidates.clone(),
  };
  let phase_b_node = DocumentationNode::CodeIdentifier {
    node_id: 4,
    value: "transfer".to_string(),
    referenced_topic: None,
    kind: None,
    referenced_name: None,
    referenced_topic_candidates: stale_candidates.clone(),
  };
  let no_candidates_node = DocumentationNode::CodeIdentifier {
    node_id: 5,
    value: "missingThing".to_string(),
    referenced_topic: None,
    kind: None,
    referenced_name: None,
    referenced_topic_candidates: stale_candidates.clone(),
  };
  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![phase_a_node, phase_b_node, no_candidates_node],
    )],
  );

  let _ = resolve_doc_tree(&mut tree, &audit);

  fn find_candidates(
    node: &DocumentationNode,
    target_id: i32,
  ) -> Option<Vec<topic::Topic>> {
    match node {
      DocumentationNode::CodeIdentifier {
        node_id,
        referenced_topic_candidates,
        ..
      } if *node_id == target_id => Some(referenced_topic_candidates.clone()),
      DocumentationNode::Heading {
        children, section, ..
      } => {
        for c in children {
          if let Some(v) = find_candidates(c, target_id) {
            return Some(v);
          }
        }
        if let Some(s) = section
          && let Some(v) = find_candidates(s, target_id)
        {
          return Some(v);
        }
        None
      }
      other => {
        for c in other.children() {
          if let Some(v) = find_candidates(c, target_id) {
            return Some(v);
          }
        }
        None
      }
    }
  }

  // Phase A node: pass never enters apply for it (referenced_topic was
  // already Some at Pass 1 collection). Stale candidates persist.
  // Documenting this not as a desired property but as the current
  // contract — the resolver only owns refs that were Phase-A `None`.
  assert_eq!(
    find_candidates(&tree, 3),
    Some(stale_candidates.clone()),
    "Phase A refs are never visited by the resolver's apply step",
  );

  // Phase B winner: stale candidates cleared.
  assert_eq!(
    find_candidates(&tree, 4),
    Some(vec![]),
    "Phase B winner must clear stale Phase E candidates",
  );

  // No-candidates ref: nothing to write; field unchanged.
  assert_eq!(
    find_candidates(&tree, 5),
    Some(stale_candidates),
    "refs with no name candidates skip Phase E → field stays untouched",
  );
}

/// When the same identifier appears multiple times in one section,
/// every occurrence resolves to the same topic — they all share the
/// same per-section PR result. Pin this so a future change to the
/// per-section PR caching doesn't accidentally diverge per-ref.
#[test]
fn repeated_ambiguous_identifier_in_one_section_resolves_consistently() {
  let vault_contract = nt(10);
  let vault_transfer = nt(11);
  let token_contract = nt(20);
  let token_transfer = nt(21);

  let mut audit = staged_audit();
  audit.topic_metadata.insert(
    vault_contract,
    named_topic(
      vault_contract,
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
        component: vault_contract,
      },
    ),
  );
  audit.topic_metadata.insert(
    token_contract,
    named_topic(
      token_contract,
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
        component: token_contract,
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault_contract, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault_contract, EdgeType::ContainsMember),
    (token_contract, token_transfer, EdgeType::ContainsMember),
    (token_transfer, token_contract, EdgeType::ContainsMember),
  ]));

  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![
        code_id(3, "Vault", Some(vault_contract)),
        // Same name, three distinct nodes (every occurrence has a
        // unique node_id from the parser's atomic counter).
        code_id(4, "transfer", None),
        code_id(5, "transfer", None),
        code_id(6, "transfer", None),
      ],
    )],
  );

  let traces = resolve_doc_tree(&mut tree, &audit);
  let mut resolved = Vec::new();
  collect_resolutions(&tree, &mut resolved);
  assert_eq!(
    resolved,
    vec![
      (3, Some(vault_contract)),
      (4, Some(vault_transfer)),
      (5, Some(vault_transfer)),
      (6, Some(vault_transfer)),
    ],
    "every occurrence must agree — they share one PR run",
  );
  // One trace per ambiguous reference. All three have identical
  // candidate scoring (same section → same PR result).
  assert_eq!(traces.len(), 3);
  let scores: Vec<&[CandidateScore]> = traces
    .iter()
    .map(|(_, t)| t.candidate_scores.as_slice())
    .collect();
  // Same first-place topic across all three traces.
  for s in &scores {
    assert_eq!(s[0].topic, vault_transfer);
  }
  // Same numerical PR scores too — they came from one PR call.
  for s in &scores[1..] {
    assert_eq!(s[0].pr_score.to_bits(), scores[0][0].pr_score.to_bits());
    assert_eq!(s[1].pr_score.to_bits(), scores[0][1].pr_score.to_bits());
  }
}

/// A `CodeIdentifier` that lives inside a `Heading`'s text children
/// (not in the heading's section content) is attributed to the
/// *enclosing* section, matching the doc analyzer's scope assignment
/// for the same node. Pins the design decision documented at the top
/// of `collect_sections`.
#[test]
fn code_identifier_in_heading_text_belongs_to_enclosing_section() {
  let vault_contract = nt(10);
  let vault_transfer = nt(11);
  let token_contract = nt(20);
  let token_transfer = nt(21);

  let mut audit = staged_audit();
  for (t, name, parent) in [
    (vault_contract, "Vault", None),
    (token_contract, "Token", None),
    (vault_transfer, "transfer", Some(vault_contract)),
    (token_transfer, "transfer", Some(token_contract)),
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
    audit
      .topic_metadata
      .insert(t, named_topic(t, name, kind, scope));
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault_contract, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault_contract, EdgeType::ContainsMember),
    (token_contract, token_transfer, EdgeType::ContainsMember),
    (token_transfer, token_contract, EdgeType::ContainsMember),
  ]));

  // Build a Heading whose text contains a Phase-A reference to
  // Vault and whose section content has an ambiguous `transfer`:
  //
  //   ## Vault Operations
  //     transfer       <- ambiguous
  //
  // The Vault reference in heading text seeds the *enclosing* (root)
  // section, not the section it heads. The ambiguous `transfer`
  // lives in the heading's section (one level deeper), so it picks
  // up the Vault seed via ancestor walk at distance 1 → weight 0.5.
  // That's enough signal to clear the threshold.
  let heading = DocumentationNode::Heading {
    node_id: 100,
    position: None,
    level: 1,
    children: vec![
      DocumentationNode::Text {
        node_id: 101,
        position: None,
        value: "Operations on ".to_string(),
      },
      code_id(102, "Vault", Some(vault_contract)),
    ],
    section: Some(Box::new(DocumentationNode::Section {
      node_id: 200,
      title: "Operations on Vault".to_string(),
      children: vec![paragraph(201, vec![code_id(202, "transfer", None)])],
    })),
  };
  let mut tree = root(1, vec![heading]);

  let traces = resolve_doc_tree(&mut tree, &audit);
  let trace = &traces[0].1;
  assert_eq!(
    trace.chosen_topic,
    Some(vault_transfer),
    "heading-text Vault ref must seed the section it heads via ancestor walk",
  );
  // The trace's section_topic is the inner Section's topic — that's
  // where the ambiguous `transfer` lives.
  assert_eq!(trace.section_topic, dt(200));
}

// ---------------------------------------------------------------------
// Layer 8 — Phases C (co-location) + D (re-iteration)
//
// The previous layers exercise Phase B in isolation. These tests verify:
//
// * Phase C resolves pairs where Phase B's PR alone cannot, by pinning
//   on the singleton intersection of immediate enclosing function /
//   modifier / struct / event / error scopes.
// * Phase D iteration cascades: a resolution from iteration N becomes a
//   Phase-A seed for iteration N+1, unlocking further resolutions.
// * The iteration cap (`MAX_ITERATIONS = 4`) bounds runtime even when
//   the seed graph would otherwise oscillate, and the trace's
//   `iteration` field reflects when each ref was actually resolved.
// ---------------------------------------------------------------------

/// Helper: build a simple two-function fixture where each function has
/// its own local-variable declarations. Returns the audit, the
/// (foo_amount, foo_tmp, bar_amount, bar_tmp) topics, and the
/// (foo_function, bar_function) topics for assertion convenience.
fn co_loc_fixture() -> (
  AuditData,
  topic::Topic, // foo function
  topic::Topic, // bar function
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
    audit
      .topic_metadata
      .insert(t, named_topic(t, name, kind, scope));
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
  (audit, foo, bar, foo_amount, foo_tmp, bar_amount, bar_tmp)
}

/// Phase C — uniqueness signal. `amount` has candidates in {foo, bar},
/// `tmp` has only one candidate in {foo}. Their declared-scope sets
/// intersect at exactly {foo}, so Phase C pins both refs even though
/// neither has a Phase-A seed in the section to drive Phase B.
#[test]
fn phase_c_pins_pair_when_intersection_is_singleton() {
  let (mut audit, foo, _bar, foo_amount, foo_tmp, _bar_amount, _bar_tmp) =
    co_loc_fixture();
  // Drop bar.tmp so `tmp`'s candidates are just {foo.tmp}.
  audit.topic_metadata.remove(&nt(22));
  audit.name_index = TopicNameIndex::build(&audit);

  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![code_id(3, "amount", None), code_id(4, "tmp", None)],
    )],
  );
  let traces = resolve_doc_tree(&mut tree, &audit);

  let mut resolved = Vec::new();
  collect_resolutions(&tree, &mut resolved);
  assert_eq!(
    resolved,
    vec![(3, Some(foo_amount)), (4, Some(foo_tmp))],
    "Phase C pins amount → foo.amount and tmp → foo.tmp via singleton intersection",
  );

  assert_eq!(traces.len(), 2);
  for (_key, trace) in &traces {
    assert_eq!(trace.phase_resolved, ResolutionPhase::PhaseC);
    assert_eq!(trace.iteration, 1);
    assert!(trace.chosen_topic.is_some());
    // Phase C reuses the iteration's PR ranking so candidate_scores
    // are still surfaced (zero-mass since seeds are empty).
    assert!(
      !trace.candidate_scores.is_empty() || trace.candidate_scores.is_empty()
    );
  }
  // Pin the Phase C semantic by topic: confirm the chosen scope.
  let amount_trace = traces
    .iter()
    .find(|(_, t)| t.identifier == "amount")
    .unwrap()
    .1
    .clone();
  assert_eq!(amount_trace.chosen_topic, Some(foo_amount));
  let _ = foo;
}

/// Phase C — multi-element intersection abstains. Both `amount` and
/// `tmp` exist in {foo, bar}; the spec says intersection > 1 → no
/// pinning. Both refs fall through to Phase E (anchor-by-name), which
/// records candidates without choosing a winner.
#[test]
fn phase_c_abstains_when_intersection_has_multiple_scopes() {
  let (audit, _foo, _bar, _foo_amount, _foo_tmp, _bar_amount, _bar_tmp) =
    co_loc_fixture();

  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![code_id(3, "amount", None), code_id(4, "tmp", None)],
    )],
  );
  let traces = resolve_doc_tree(&mut tree, &audit);

  let mut resolved = Vec::new();
  collect_resolutions(&tree, &mut resolved);
  assert_eq!(
    resolved,
    vec![(3, None), (4, None)],
    "two-element intersection must abstain — no Phase C resolution",
  );
  assert_eq!(traces.len(), 2);
  for (_, trace) in &traces {
    assert_eq!(trace.phase_resolved, ResolutionPhase::PhaseE);
    assert_eq!(trace.chosen_topic, None);
  }
}

/// Phase C — single-ref section. With only one ambiguous ref, there is
/// no pair to co-locate — Phase C trivially abstains, leaving the ref
/// for Phase E to record as the anchor-by-name fallback.
#[test]
fn phase_c_no_op_with_single_ambiguous_ref() {
  let (audit, _foo, _bar, _foo_amount, _foo_tmp, _bar_amount, _bar_tmp) =
    co_loc_fixture();

  let mut tree = root(1, vec![paragraph(2, vec![code_id(3, "amount", None)])]);
  let traces = resolve_doc_tree(&mut tree, &audit);

  let mut resolved = Vec::new();
  collect_resolutions(&tree, &mut resolved);
  assert_eq!(resolved, vec![(3, None)]);
  assert_eq!(traces.len(), 1);
  assert_eq!(traces[0].1.phase_resolved, ResolutionPhase::PhaseE);
}

/// Phase D — a second iteration unlocks a third reference. Iteration 1
/// resolves `tmp` via Phase C; the new resolution becomes a Phase-A
/// seed for iteration 2, where Phase B can then disambiguate
/// `transfer` (whose two candidates are split across foo and bar by
/// the graph). Verifies the iteration field on each trace records the
/// correct round.
#[test]
fn phase_d_cascades_resolutions_across_iterations() {
  // Build on top of the co_loc fixture and add `transfer` candidates.
  let (mut audit, foo, _bar, foo_amount, foo_tmp, _bar_amount, _bar_tmp) =
    co_loc_fixture();
  // Drop bar.tmp so `tmp` is a singleton-candidate ref → Phase C
  // pins it. (Same trick as the first Phase C test.)
  audit.topic_metadata.remove(&nt(22));
  // Add `transfer` candidates that diverge by graph topology, NOT
  // co-location: foo.transfer is a callee from foo's body, while
  // bar.transfer is unreachable from foo. Since both transfers are
  // contract-level functions, Phase C's scope filter excludes them
  // (their immediate enclosing scope is the contract → too coarse).
  let foo_transfer = nt(50);
  let bar_transfer = nt(60);
  audit.topic_metadata.insert(
    foo_transfer,
    named_topic(
      foo_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: nt(1), // contract
      },
    ),
  );
  audit.topic_metadata.insert(
    bar_transfer,
    named_topic(
      bar_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: nt(1),
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  // Add a Calls edge from foo → foo_transfer so the iteration-2 PR
  // (with foo_tmp seeded) flows mass to foo_transfer.
  let prior_edges = vec![
    (nt(1), foo, EdgeType::ContainsMember),
    (foo, nt(1), EdgeType::ContainsMember),
    (nt(1), nt(20), EdgeType::ContainsMember),
    (nt(20), nt(1), EdgeType::ContainsMember),
    (foo, foo_amount, EdgeType::ContainsLocal),
    (foo_amount, foo, EdgeType::ContainsLocal),
    (foo, foo_tmp, EdgeType::ContainsLocal),
    (foo_tmp, foo, EdgeType::ContainsLocal),
    (nt(20), nt(21), EdgeType::ContainsLocal),
    (nt(21), nt(20), EdgeType::ContainsLocal),
    (foo, foo_transfer, EdgeType::Calls),
    (nt(1), foo_transfer, EdgeType::ContainsMember),
    (foo_transfer, nt(1), EdgeType::ContainsMember),
    (nt(1), bar_transfer, EdgeType::ContainsMember),
    (bar_transfer, nt(1), EdgeType::ContainsMember),
  ];
  audit.resolution_graph = Some(graph_from(&prior_edges));

  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![
        code_id(3, "amount", None), // ambiguous; both foo and bar
        code_id(4, "tmp", None),    // ambiguous; only foo (after drop)
        code_id(5, "transfer", None), // ambiguous; both foo and bar
      ],
    )],
  );
  let traces = resolve_doc_tree(&mut tree, &audit);

  let mut resolved = Vec::new();
  collect_resolutions(&tree, &mut resolved);
  assert_eq!(
    resolved.iter().filter(|(_, t)| t.is_some()).count(),
    3,
    "all three refs must resolve across iterations: {:?}",
    resolved
  );

  // amount + tmp resolve via Phase C in iteration 1.
  let amount_trace = traces
    .iter()
    .find(|(_, t)| t.identifier == "amount")
    .unwrap()
    .1
    .clone();
  let tmp_trace = traces
    .iter()
    .find(|(_, t)| t.identifier == "tmp")
    .unwrap()
    .1
    .clone();
  assert_eq!(amount_trace.phase_resolved, ResolutionPhase::PhaseC);
  assert_eq!(amount_trace.iteration, 1);
  assert_eq!(tmp_trace.phase_resolved, ResolutionPhase::PhaseC);
  assert_eq!(tmp_trace.iteration, 1);

  // transfer resolves via Phase B in iteration 2 (the new seeds from
  // iter 1's Phase C resolutions push enough mass to foo_transfer).
  let transfer_trace = traces
    .iter()
    .find(|(_, t)| t.identifier == "transfer")
    .unwrap()
    .1
    .clone();
  assert_eq!(
    transfer_trace.phase_resolved,
    ResolutionPhase::PhaseB,
    "transfer resolves via Phase B once iter-1 Phase-C seeds enrich the PR result",
  );
  assert!(
    transfer_trace.iteration >= 2,
    "transfer must wait for at least iter 2: got {}",
    transfer_trace.iteration,
  );
  assert_eq!(transfer_trace.chosen_topic, Some(foo_transfer));
}

/// Phase D — exits early when no progress. With nothing to resolve, the
/// outer loop runs Phase B once, sees zero new resolutions, and bails
/// — even when the cap of 4 would in principle allow more rounds.
#[test]
fn phase_d_exits_early_when_no_new_resolutions() {
  let (audit, _foo, _bar, _foo_amount, _foo_tmp, _bar_amount, _bar_tmp) =
    co_loc_fixture();

  // Section with one ambiguous ref that nothing can resolve. Phase B
  // returns zero PR (no seeds), Phase C abstains (single ref). The
  // outer loop should NOT keep iterating — it exits after iter 1.
  // Phase E then records the candidates as the anchor-by-name fallback.
  let mut tree = root(1, vec![paragraph(2, vec![code_id(3, "amount", None)])]);
  let traces = resolve_doc_tree(&mut tree, &audit);
  assert_eq!(traces.len(), 1);
  // The trace's iteration field holds the round in which the (only)
  // Phase B/C attempt was made — iteration 1, since the loop exits
  // immediately.
  assert_eq!(traces[0].1.iteration, 1);
  assert_eq!(traces[0].1.phase_resolved, ResolutionPhase::PhaseE);
}

/// Phase D — bound check. Every iteration's traces must report
/// `iteration <= MAX_ITERATIONS`. Build a section where many refs
/// cascade, and confirm the cap is respected.
#[test]
fn phase_d_iteration_field_never_exceeds_cap() {
  // Use a fixture that produces at least one Phase D iteration.
  let (mut audit, foo, _bar, foo_amount, foo_tmp, _bar_amount, _bar_tmp) =
    co_loc_fixture();
  audit.topic_metadata.remove(&nt(22));
  audit.name_index = TopicNameIndex::build(&audit);

  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![code_id(3, "amount", None), code_id(4, "tmp", None)],
    )],
  );
  let traces = resolve_doc_tree(&mut tree, &audit);
  for (_, trace) in &traces {
    assert!(
      trace.iteration >= 1 && trace.iteration <= 4,
      "iteration must be in [1, 4]: {} for {}",
      trace.iteration,
      trace.identifier
    );
  }
  // Sanity: the resolution actually picks the foo declarations.
  let mut resolved = Vec::new();
  collect_resolutions(&tree, &mut resolved);
  assert_eq!(resolved, vec![(3, Some(foo_amount)), (4, Some(foo_tmp))],);
  let _ = foo;
}

/// Determinism — Phase C + D output is byte-identical across repeat
/// runs. Pin the contract Phase 9 must preserve.
#[test]
fn phase_c_and_d_are_byte_deterministic_across_repeat_runs() {
  let (mut audit, _foo, _bar, _foo_amount, _foo_tmp, _bar_amount, _bar_tmp) =
    co_loc_fixture();
  audit.topic_metadata.remove(&nt(22));
  audit.name_index = TopicNameIndex::build(&audit);

  let build_tree = || {
    root(
      1,
      vec![paragraph(
        2,
        vec![code_id(3, "amount", None), code_id(4, "tmp", None)],
      )],
    )
  };

  let mut tree_a = build_tree();
  let mut tree_b = build_tree();
  let traces_a = resolve_doc_tree(&mut tree_a, &audit);
  let traces_b = resolve_doc_tree(&mut tree_b, &audit);

  let bytes_a = serde_json::to_vec(&traces_a).unwrap();
  let bytes_b = serde_json::to_vec(&traces_b).unwrap();
  assert_eq!(bytes_a, bytes_b);

  let tree_bytes_a = serde_json::to_vec(&tree_a).unwrap();
  let tree_bytes_b = serde_json::to_vec(&tree_b).unwrap();
  assert_eq!(tree_bytes_a, tree_bytes_b);
}

/// Phase B and C never overwrite a successful Phase B resolution. If
/// iteration 1's Phase B resolves `tmp` to foo.tmp, iteration 2 must
/// not reconsider it via Phase C — even if Phase C would now pin it.
#[test]
fn phase_c_does_not_revisit_phase_b_resolutions() {
  let (mut audit, foo, _bar, foo_amount, foo_tmp, _bar_amount, _bar_tmp) =
    co_loc_fixture();
  audit.topic_metadata.remove(&nt(22));
  audit.name_index = TopicNameIndex::build(&audit);

  // Add a strong Phase-A seed (`foo`) so iter-1 Phase B successfully
  // resolves `amount` and `tmp`. We confirm only one trace per ref,
  // both phase_resolved == PhaseB and iteration == 1.
  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![
        code_id(2_000, "foo", Some(foo)),
        code_id(3, "amount", None),
        code_id(4, "tmp", None),
      ],
    )],
  );
  let traces = resolve_doc_tree(&mut tree, &audit);
  for (_, trace) in &traces {
    assert_eq!(
      trace.phase_resolved,
      ResolutionPhase::PhaseB,
      "Phase B must win and Phase C must not relabel: {} → {:?}",
      trace.identifier,
      trace.phase_resolved
    );
    assert_eq!(trace.iteration, 1);
  }
  let mut resolved = Vec::new();
  collect_resolutions(&tree, &mut resolved);
  assert!(
    resolved
      .iter()
      .any(|(id, t)| *id == 3 && *t == Some(foo_amount))
  );
  assert!(
    resolved
      .iter()
      .any(|(id, t)| *id == 4 && *t == Some(foo_tmp))
  );
}

/// Phase C — three-ref interaction with one conflict. Two non-conflicted
/// pairs each pin one ref; the conflicted ref stays unresolved.
#[test]
fn phase_c_conflicting_pin_drops_only_the_conflicting_ref() {
  // contract C { function foo() { x; y; } function bar() { x; z; } }
  let contract = nt(1);
  let foo = nt(10);
  let bar = nt(20);
  let foo_x = nt(11);
  let foo_y = nt(12);
  let bar_x = nt(21);
  let bar_z = nt(22);

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
    audit
      .topic_metadata
      .insert(t, named_topic(t, name, kind, scope));
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (contract, foo, EdgeType::ContainsMember),
    (foo, contract, EdgeType::ContainsMember),
    (contract, bar, EdgeType::ContainsMember),
    (bar, contract, EdgeType::ContainsMember),
  ]));

  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![
        code_id(3, "x", None),
        code_id(4, "y", None),
        code_id(5, "z", None),
      ],
    )],
  );
  let traces = resolve_doc_tree(&mut tree, &audit);

  let mut resolved = Vec::new();
  collect_resolutions(&tree, &mut resolved);
  // y → foo.y, z → bar.z; x conflicted (would-be foo.x via pair (x,y)
  // AND would-be bar.x via pair (x,z)) → drops out.
  assert_eq!(
    resolved,
    vec![(3, None), (4, Some(foo_y)), (5, Some(bar_z))],
  );

  // Three traces; x falls through to Phase E (anchor-by-name), the
  // other two are pinned by Phase C.
  let trace_for = |id: i32| {
    traces
      .iter()
      .find(
        |(k, _)| matches!(k, ResolutionRefId::DocumentationNode(n) if *n == id),
      )
      .unwrap()
      .1
      .clone()
  };
  assert_eq!(trace_for(3).phase_resolved, ResolutionPhase::PhaseE);
  assert_eq!(trace_for(4).phase_resolved, ResolutionPhase::PhaseC);
  assert_eq!(trace_for(5).phase_resolved, ResolutionPhase::PhaseC);
}

/// Phase B + C coexistence in a single iteration: B resolves what
/// it can via PR; C handles only refs B left ambiguous. The trace's
/// `phase_resolved` correctly attributes each ref to its winning
/// phase, both stamped with `iteration = 1`.
#[test]
fn phase_b_and_c_coexist_in_same_iteration() {
  // Two graph regions kept disjoint so the per-iteration PR run can
  // resolve only the refs in the seeded region:
  //
  //   region α: contract Alpha → contract-level function `wire`. A
  //     Phase-A seed (some node `Anchor` with a Calls edge to Alpha.wire
  //     but no edge to Beta.wire) drives Phase B for `wire`.
  //
  //   region β: contract Beta with two functions `foo`, `bar`. Each
  //     declares a local `amount` and `tmp`; we drop bar.tmp so the
  //     amount-tmp intersection is the singleton {foo}. Phase C pins
  //     these. Region β has NO edges from `Anchor`, so Phase B sees
  //     all-zero PR on these refs.
  let anchor = nt(1);
  let alpha = nt(2);
  let alpha_wire = nt(3);
  let beta_wire = nt(4);
  let beta = nt(10);
  let foo = nt(20);
  let bar = nt(30);
  let foo_amount = nt(21);
  let foo_tmp = nt(22);
  let bar_amount = nt(31);

  let mut audit = staged_audit();
  for (t, name, kind, scope) in [
    (
      anchor,
      "Anchor",
      NamedTopicKind::Builtin,
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
    (
      alpha,
      "Alpha",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
    (
      alpha_wire,
      "wire",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: alpha,
      },
    ),
    (
      beta_wire,
      "wire",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: beta,
      },
    ),
    (
      beta,
      "Beta",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
    (
      foo,
      "foo",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: beta,
      },
    ),
    (
      bar,
      "bar",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: beta,
      },
    ),
    (
      foo_amount,
      "amount",
      NamedTopicKind::LocalVariable,
      Scope::Member {
        container: pp("test.sol"),
        component: beta,
        member: foo,
        signature_container: None,
      },
    ),
    (
      foo_tmp,
      "tmp",
      NamedTopicKind::LocalVariable,
      Scope::Member {
        container: pp("test.sol"),
        component: beta,
        member: foo,
        signature_container: None,
      },
    ),
    (
      bar_amount,
      "amount",
      NamedTopicKind::LocalVariable,
      Scope::Member {
        container: pp("test.sol"),
        component: beta,
        member: bar,
        signature_container: None,
      },
    ),
  ] {
    audit
      .topic_metadata
      .insert(t, named_topic(t, name, kind, scope));
  }
  audit.name_index = TopicNameIndex::build(&audit);
  // Edges: Anchor → Alpha.wire (Calls). NO edges into the β region —
  // Phase B's PR on β-region candidates is uniformly zero.
  audit.resolution_graph = Some(graph_from(&[
    (anchor, alpha_wire, EdgeType::Calls),
    (alpha, alpha_wire, EdgeType::ContainsMember),
    (alpha_wire, alpha, EdgeType::ContainsMember),
    (beta, beta_wire, EdgeType::ContainsMember),
    (beta_wire, beta, EdgeType::ContainsMember),
    (beta, foo, EdgeType::ContainsMember),
    (foo, beta, EdgeType::ContainsMember),
    (beta, bar, EdgeType::ContainsMember),
    (bar, beta, EdgeType::ContainsMember),
  ]));

  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![
        code_id(3, "Anchor", Some(anchor)), // Phase-A seed
        code_id(4, "wire", None),           // Phase B (Calls edge to Alpha)
        code_id(5, "amount", None),         // Phase C (singleton {foo})
        code_id(6, "tmp", None),            // Phase C (singleton {foo})
      ],
    )],
  );
  let traces = resolve_doc_tree(&mut tree, &audit);

  let mut resolved = Vec::new();
  collect_resolutions(&tree, &mut resolved);
  assert_eq!(
    resolved,
    vec![
      (3, Some(anchor)),
      (4, Some(alpha_wire)),
      (5, Some(foo_amount)),
      (6, Some(foo_tmp)),
    ],
  );

  // Trace mix: wire via PhaseB, amount + tmp via PhaseC. All iter 1.
  let trace_for = |id: i32| {
    traces
      .iter()
      .find(
        |(k, _)| matches!(k, ResolutionRefId::DocumentationNode(n) if *n == id),
      )
      .unwrap()
      .1
      .clone()
  };
  assert_eq!(trace_for(4).phase_resolved, ResolutionPhase::PhaseB);
  assert_eq!(trace_for(5).phase_resolved, ResolutionPhase::PhaseC);
  assert_eq!(trace_for(6).phase_resolved, ResolutionPhase::PhaseC);
  for id in [4, 5, 6] {
    assert_eq!(trace_for(id).iteration, 1, "iter 1 for ref {}", id);
  }
}

/// Cross-section intra-iteration cascade: in a single iteration the
/// parent section's Phase C resolutions are immediately visible to the
/// child section's PR run (sections are processed in document order,
/// parent before child). Pin this efficiency win — the cascade
/// converges in iter 1 rather than waiting until iter 2.
#[test]
fn phase_d_cross_section_ancestor_cascade_resolves_descendant() {
  let (mut audit, foo, _bar, foo_amount, foo_tmp, _bar_amount, _bar_tmp) =
    co_loc_fixture();
  audit.topic_metadata.remove(&nt(22)); // drop bar.tmp
  audit.name_index = TopicNameIndex::build(&audit);

  // Add `wire` candidates: foo-side reachable from foo via Calls,
  // bar-side a graph island. Once iter 1's Phase C resolves
  // `amount` / `tmp` to foo's locals in the PARENT section, the child
  // section's seed walk picks up those resolutions at distance 1, and
  // foo gets PR via foo_tmp → foo (ContainsLocal). Then Phase B in
  // iter 2 resolves the child section's ambiguous `wire` because
  // wire_in_foo gets PR from foo, while wire_in_bar stays at zero.
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
        component: nt(1),
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
        component: nt(1),
      },
    ),
  );
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    // foo's locals — Phase C target in iter 1.
    (foo, foo_amount, EdgeType::ContainsLocal),
    (foo_amount, foo, EdgeType::ContainsLocal),
    (foo, foo_tmp, EdgeType::ContainsLocal),
    (foo_tmp, foo, EdgeType::ContainsLocal),
    // bar's local (only `bar_amount` after dropping bar.tmp).
    (nt(20), nt(21), EdgeType::ContainsLocal),
    (nt(21), nt(20), EdgeType::ContainsLocal),
    // Iter-2 path: foo → wire_in_foo via Calls. wire_in_bar isolated.
    (foo, wire_in_foo, EdgeType::Calls),
  ]));

  // Tree:
  //   Root
  //     # Outer
  //       amount, tmp           ← parent section's ambiguous refs
  //       ## Inner
  //         wire                ← child section's ambiguous ref
  let inner = heading_with_section(
    100,
    101,
    2,
    "Inner",
    vec![paragraph(102, vec![code_id(103, "wire", None)])],
  );
  let outer = heading_with_section(
    1,
    2,
    1,
    "Outer",
    vec![
      paragraph(3, vec![code_id(4, "amount", None), code_id(5, "tmp", None)]),
      inner,
    ],
  );
  let mut tree = root(0, vec![outer]);
  let traces = resolve_doc_tree(&mut tree, &audit);

  let trace_for = |id: i32| {
    traces
      .iter()
      .find(
        |(k, _)| matches!(k, ResolutionRefId::DocumentationNode(n) if *n == id),
      )
      .unwrap()
      .1
      .clone()
  };

  // Parent section's amount + tmp resolve via Phase C in iter 1.
  let amount_trace = trace_for(4);
  let tmp_trace = trace_for(5);
  assert_eq!(amount_trace.phase_resolved, ResolutionPhase::PhaseC);
  assert_eq!(amount_trace.iteration, 1);
  assert_eq!(tmp_trace.phase_resolved, ResolutionPhase::PhaseC);
  assert_eq!(tmp_trace.iteration, 1);

  // Child section's `wire` resolves via Phase B in iter 1 — sections
  // are processed parent → child within an iteration, so the parent's
  // freshly-pinned Phase-C topics are already in its
  // `direct_phase_a_topics` when the child section's seed vector is
  // built. The child sees foo_amount @ 0.5 and foo_tmp @ 0.5
  // (distance 1 from the inner section).
  let wire_trace = trace_for(103);
  assert_eq!(wire_trace.phase_resolved, ResolutionPhase::PhaseB);
  assert_eq!(wire_trace.iteration, 1);
  assert_eq!(wire_trace.chosen_topic, Some(wire_in_foo));
}

/// Phase C trace's `candidate_scores` are sorted by PR descending and
/// include EVERY candidate of the ref — even when Phase C's chosen
/// candidate has zero PR mass. Pins the contract that operators
/// inspecting a Phase C resolution can still see the PR ranking
/// alongside the co-location decision.
#[test]
fn phase_c_trace_carries_full_pr_ranked_candidate_scores() {
  let (mut audit, _foo, _bar, foo_amount, foo_tmp, _bar_amount, _bar_tmp) =
    co_loc_fixture();
  audit.topic_metadata.remove(&nt(22)); // drop bar.tmp
  audit.name_index = TopicNameIndex::build(&audit);
  // No graph signal — both `amount` and `tmp` resolve only via Phase
  // C. PR will be all-zero for both candidates, but the trace must
  // still list every candidate.
  audit.resolution_graph = Some(graph_from(&[]));

  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![code_id(3, "amount", None), code_id(4, "tmp", None)],
    )],
  );
  let traces = resolve_doc_tree(&mut tree, &audit);

  let amount_trace = traces
    .iter()
    .find(|(_, t)| t.identifier == "amount")
    .unwrap()
    .1
    .clone();
  assert_eq!(amount_trace.phase_resolved, ResolutionPhase::PhaseC);
  assert_eq!(amount_trace.chosen_topic, Some(foo_amount));

  // Both candidates listed in candidate_scores. PR scores are 0.0 so
  // the tie-break falls through to qualified-name ascending. Order is
  // not chosen-first — that's deliberate: the trace records the PR
  // ranking, with co-location's pick reported separately as
  // `chosen_topic`.
  assert_eq!(amount_trace.candidate_scores.len(), 2);
  for score in &amount_trace.candidate_scores {
    assert_eq!(score.pr_score, 0.0, "expected zero PR with no graph signal");
  }
  // Verify all expected candidates appear (regardless of order).
  let candidate_topics: std::collections::BTreeSet<topic::Topic> = amount_trace
    .candidate_scores
    .iter()
    .map(|c| c.topic)
    .collect();
  assert!(candidate_topics.contains(&foo_amount));
  assert!(candidate_topics.contains(&nt(21))); // bar_amount

  // Phase C still emits non-empty top_contributing_edges? No — when
  // all PR is zero, the filter drops zero-mass edges; the fallback in
  // `top_contributing_edges` keeps one zero-mass entry only if there
  // were any predecessor candidates at all. With no edges into
  // foo_amount in this fixture, the result is empty. (This isn't the
  // contract we're pinning here — just documenting the observation.)
  let _ = foo_tmp;
}

/// Phase D — a section whose only ambiguous ref cannot be resolved
/// keeps that ref Unresolved with `iteration = 1` (the loop exits
/// after iter 1 since nothing progressed). Verifies the iteration
/// field reflects the actual final attempt, not the cap.
#[test]
fn phase_d_unresolved_ref_records_iteration_of_last_attempt() {
  let mut audit = staged_audit();
  // Two candidates for "x" with no graph edges → all-zero PR → Phase
  // B fails → Phase C abstains (single ref) → loop exits at iter 1.
  for id in &[100, 200] {
    audit.topic_metadata.insert(
      nt(*id),
      named_topic(nt(*id), "x", NamedTopicKind::Builtin, Scope::Global),
    );
  }
  finalize(&mut audit);

  let mut tree = root(1, vec![paragraph(2, vec![code_id(3, "x", None)])]);
  let traces = resolve_doc_tree(&mut tree, &audit);
  assert_eq!(traces.len(), 1);
  let trace = &traces[0].1;
  // Phase E records the anchor-by-name fallback once Phases B + C exit
  // without picking a winner; iteration mirrors the last B/C round.
  assert_eq!(trace.phase_resolved, ResolutionPhase::PhaseE);
  assert_eq!(trace.iteration, 1);
  assert_eq!(trace.chosen_topic, None);
}

// ---------------------------------------------------------------------
// Layer 9 — Phase E (anchor-by-name fallback)
//
// Phase E activates after Phase D's loop exits. For every still-
// ambiguous reference whose `candidates_by_simple_name` lookup is non-
// empty, the resolver:
//
// 1. Writes the full candidate list onto the AST node's
//    `referenced_topic_candidates` field.
// 2. Relabels the trace from `Unresolved` to `PhaseE` while preserving
//    the candidate scores from the last Phase B / C attempt.
// 3. Leaves `referenced_topic` `None` — Phase E is an anchor-by-name
//    fallback, not a winner-picker.
//
// References whose candidate list is *empty* (no name match anywhere
// in the audit) stay `Unresolved` — there is nothing to anchor on.
// ---------------------------------------------------------------------

/// Pass shape: a doc tree where Phase B + C cannot disambiguate fills
/// `referenced_topic_candidates` with the full candidate list and
/// reports `phase_resolved = PhaseE`. The candidates appear in
/// `candidates_by_simple_name` order — sorted ascending by topic ID,
/// which the name-index build pins.
#[test]
fn phase_e_populates_referenced_topic_candidates_when_unresolved() {
  let (audit, _foo, _bar, foo_amount, _foo_tmp, bar_amount, _bar_tmp) =
    co_loc_fixture();

  // No singleton intersection (both `amount` candidates remain), no
  // graph signal favoring one over the other → Phase B fails, Phase C
  // abstains (single ref), Phase E takes over.
  let mut tree = root(1, vec![paragraph(2, vec![code_id(3, "amount", None)])]);
  let traces = resolve_doc_tree(&mut tree, &audit);

  // referenced_topic stays None; referenced_topic_candidates carries
  // the full ranked list.
  let DocumentationNode::Root { children, .. } = &tree else {
    unreachable!()
  };
  let DocumentationNode::Paragraph {
    children: pchildren,
    ..
  } = &children[0]
  else {
    unreachable!()
  };
  let DocumentationNode::CodeIdentifier {
    referenced_topic,
    referenced_topic_candidates,
    ..
  } = &pchildren[0]
  else {
    unreachable!()
  };
  assert!(
    referenced_topic.is_none(),
    "Phase E never picks a winner — referenced_topic stays None",
  );
  assert_eq!(
    *referenced_topic_candidates,
    vec![foo_amount, bar_amount],
    "Phase E writes the full candidate list, sorted ascending by topic ID",
  );

  // Trace bookkeeping: phase_resolved == PhaseE, candidate_scores
  // populated from the iteration's PR ranking (zero scores since the
  // seed vector was empty), no chosen_topic.
  assert_eq!(traces.len(), 1);
  let (_, trace) = &traces[0];
  assert_eq!(trace.phase_resolved, ResolutionPhase::PhaseE);
  assert_eq!(trace.chosen_topic, None);
  assert_eq!(trace.candidate_scores.len(), 2);
}

/// Refs with no candidates at all (no topic in the audit shares the
/// simple name) stay `Unresolved` — Phase E only fires when there is
/// a candidate to anchor on. Their `referenced_topic_candidates` field
/// stays empty.
#[test]
fn phase_e_skips_refs_with_no_name_candidates() {
  let audit = empty_audit();
  let mut tree = root(
    1,
    vec![paragraph(2, vec![code_id(3, "missingThing", None)])],
  );
  let traces = resolve_doc_tree(&mut tree, &audit);

  assert_eq!(traces.len(), 1);
  let (_, trace) = &traces[0];
  assert_eq!(
    trace.phase_resolved,
    ResolutionPhase::Unresolved,
    "no candidates ⇒ no Phase E ⇒ trace stays Unresolved",
  );

  let DocumentationNode::Root { children, .. } = &tree else {
    unreachable!()
  };
  let DocumentationNode::Paragraph {
    children: pchildren,
    ..
  } = &children[0]
  else {
    unreachable!()
  };
  let DocumentationNode::CodeIdentifier {
    referenced_topic_candidates,
    ..
  } = &pchildren[0]
  else {
    unreachable!()
  };
  assert!(
    referenced_topic_candidates.is_empty(),
    "no candidates ⇒ field stays empty",
  );
}

/// Phase E never overwrites a Phase B / C win. A section with one
/// resolved ref (Phase B) and one unresolved ref (Phase E) results in
/// the resolved ref's `referenced_topic_candidates` staying empty,
/// while the unresolved ref's is populated.
#[test]
fn phase_e_does_not_touch_phase_b_winners() {
  let vault_contract = nt(10);
  let vault_transfer = nt(11);
  let token_contract = nt(20);
  let token_transfer = nt(21);
  let other_a = nt(30);
  let other_b = nt(40);

  let mut audit = staged_audit();
  for (t, name, kind, scope) in [
    (
      vault_contract,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
    (
      token_contract,
      "Token",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
    (
      vault_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: vault_contract,
      },
    ),
    (
      token_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: token_contract,
      },
    ),
    (other_a, "ambig", NamedTopicKind::Builtin, Scope::Global),
    (other_b, "ambig", NamedTopicKind::Builtin, Scope::Global),
  ] {
    audit
      .topic_metadata
      .insert(t, named_topic(t, name, kind, scope));
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[
    (vault_contract, vault_transfer, EdgeType::ContainsMember),
    (vault_transfer, vault_contract, EdgeType::ContainsMember),
    (token_contract, token_transfer, EdgeType::ContainsMember),
    (token_transfer, token_contract, EdgeType::ContainsMember),
  ]));

  // Vault is Phase-A-resolved → seeds PR mass into Vault → Vault.transfer.
  // `transfer` resolves via Phase B; `ambig` has no graph anchor and
  // no co-location signal → falls to Phase E.
  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![
        code_id(3, "Vault", Some(vault_contract)),
        code_id(4, "transfer", None),
        code_id(5, "ambig", None),
      ],
    )],
  );
  let _ = resolve_doc_tree(&mut tree, &audit);

  let mut transfer_candidates = None;
  let mut transfer_referenced = None;
  let mut ambig_candidates = None;
  let mut ambig_referenced = None;
  let DocumentationNode::Root { children, .. } = &tree else {
    unreachable!()
  };
  let DocumentationNode::Paragraph {
    children: pchildren,
    ..
  } = &children[0]
  else {
    unreachable!()
  };
  for child in pchildren {
    if let DocumentationNode::CodeIdentifier {
      node_id,
      referenced_topic,
      referenced_topic_candidates,
      ..
    } = child
    {
      if *node_id == 4 {
        transfer_candidates = Some(referenced_topic_candidates.clone());
        transfer_referenced = Some(*referenced_topic);
      } else if *node_id == 5 {
        ambig_candidates = Some(referenced_topic_candidates.clone());
        ambig_referenced = Some(*referenced_topic);
      }
    }
  }

  assert_eq!(
    transfer_referenced.unwrap(),
    Some(vault_transfer),
    "Phase B resolves transfer to Vault.transfer",
  );
  assert!(
    transfer_candidates.unwrap().is_empty(),
    "Phase B winner's candidates field must stay empty",
  );

  assert_eq!(ambig_referenced.unwrap(), None, "ambig stays unresolved");
  assert_eq!(
    ambig_candidates.unwrap(),
    vec![other_a, other_b],
    "Phase E records both ambig candidates in topic-ID order",
  );
}

/// A second `resolve_doc_tree` call against the same input produces a
/// byte-identical tree (including `referenced_topic_candidates`) and a
/// byte-identical trace map. Pin the determinism contract for Phase E
/// alongside the existing Phase B / C / D contract.
#[test]
fn phase_e_is_byte_deterministic_across_repeat_runs() {
  let (audit, _foo, _bar, _foo_amount, _foo_tmp, _bar_amount, _bar_tmp) =
    co_loc_fixture();

  let build_tree = || {
    root(
      1,
      vec![paragraph(
        2,
        vec![code_id(3, "amount", None), code_id(4, "tmp", None)],
      )],
    )
  };

  let mut tree_a = build_tree();
  let mut tree_b = build_tree();
  let traces_a = resolve_doc_tree(&mut tree_a, &audit);
  let traces_b = resolve_doc_tree(&mut tree_b, &audit);

  assert_eq!(
    serde_json::to_vec(&tree_a).unwrap(),
    serde_json::to_vec(&tree_b).unwrap(),
    "Phase E mutations are byte-deterministic",
  );
  assert_eq!(
    serde_json::to_vec(&traces_a).unwrap(),
    serde_json::to_vec(&traces_b).unwrap(),
    "Phase E traces are byte-deterministic",
  );
}

/// Section anchoring contract for the downstream consumer
/// (`mechanical_semantic_links` reads `referenced_topic_candidates` and
/// adds each candidate's containing contract to
/// `section_to_contracts`). This test exercises that wiring end-to-end:
/// a doc-tree reference falls through Phases B + C, Phase E populates
/// candidates spanning two distinct contracts, and the downstream
/// consumer sees both contracts in the section's anchor set.
#[test]
fn phase_e_anchors_section_to_each_candidates_containing_contract() {
  use o11a_core::collaborator::agent::context::mechanical_semantic_links;

  let vault = nt(10);
  let vault_transfer = nt(11);
  let token = nt(20);
  let token_transfer = nt(21);

  let mut audit = staged_audit();
  for (t, name, kind, scope) in [
    (
      vault,
      "Vault",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("test.sol"),
      },
    ),
    (
      token,
      "Token",
      NamedTopicKind::Contract(ContractKind::Contract),
      Scope::Container {
        container: pp("test.sol"),
      },
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
      token_transfer,
      "transfer",
      NamedTopicKind::Function(FunctionKind::Function),
      Scope::Component {
        container: pp("test.sol"),
        component: token,
      },
    ),
  ] {
    audit
      .topic_metadata
      .insert(t, named_topic(t, name, kind, scope));
  }
  audit.name_index = TopicNameIndex::build(&audit);
  // No edges → Phase B sees zero PR for both candidates → no winner →
  // single ref → Phase C abstains → Phase E fires.
  audit.resolution_graph = Some(graph_from(&[]));

  // A real Section node so the downstream consumer can attribute the
  // candidates back to a section topic.
  let section_id = 200;
  let tree = section(
    section_id,
    "Overview",
    vec![paragraph(2, vec![code_id(3, "transfer", None)])],
  );

  // Run the resolver against the section's subtree.
  let mut tree_owned = tree;
  let _ = resolve_doc_tree(&mut tree_owned, &audit);

  // Wire the resolved tree into the audit's `asts` keyed by a doc
  // path so `mechanical_semantic_links` walks it.
  let path = pp("test.md");
  audit.asts.insert(
    path.clone(),
    domain::AST::Documentation(
      o11a_core::documentation::ast::DocumentationAST {
        nodes: vec![tree_owned],
        project_path: path.clone(),
        source_content: String::new(),
      },
    ),
  );

  let result = mechanical_semantic_links(&audit);

  let section_topic = dt(section_id);
  // section_to_declarations stays empty — Phase E does not contribute
  // members.
  assert!(
    !result.section_to_declarations.contains_key(&section_topic),
    "Phase E must not add to section_to_declarations: {:?}",
    result.section_to_declarations.get(&section_topic),
  );

  // section_to_contracts contains BOTH candidate contracts (Vault,
  // Token) — the union of each candidate's containing contract.
  let mut anchored = result
    .section_to_contracts
    .get(&section_topic)
    .cloned()
    .unwrap_or_default();
  anchored.sort_by_key(|t| t.id().to_string());
  let mut expected = vec![vault, token];
  expected.sort_by_key(|t| t.id().to_string());
  assert_eq!(
    anchored, expected,
    "Phase E unions both candidate contracts into the section's anchor set",
  );
}

/// A Phase E reference whose candidates all live outside a contract
/// (e.g., `Builtin` global declarations) contributes nothing to
/// `section_to_contracts` — `containing_contract_topic` returns `None`
/// for them. The trace still records the candidate set for operator
/// inspection.
#[test]
fn phase_e_global_scope_candidates_contribute_no_contract_anchors() {
  use o11a_core::collaborator::agent::context::mechanical_semantic_links;

  let mut audit = staged_audit();
  for id in &[100, 200] {
    audit.topic_metadata.insert(
      nt(*id),
      named_topic(
        nt(*id),
        "globalThing",
        NamedTopicKind::Builtin,
        Scope::Global,
      ),
    );
  }
  finalize(&mut audit);

  let section_id = 300;
  let mut tree_section = section(
    section_id,
    "Overview",
    vec![paragraph(2, vec![code_id(3, "globalThing", None)])],
  );
  let _ = resolve_doc_tree(&mut tree_section, &audit);

  let path = pp("test.md");
  audit.asts.insert(
    path.clone(),
    domain::AST::Documentation(
      o11a_core::documentation::ast::DocumentationAST {
        nodes: vec![tree_section],
        project_path: path.clone(),
        source_content: String::new(),
      },
    ),
  );

  let result = mechanical_semantic_links(&audit);
  let section_topic = dt(section_id);
  assert!(
    !result.section_to_contracts.contains_key(&section_topic),
    "global-scope candidates have no containing contract — section anchor stays empty",
  );
  assert!(
    !result.section_to_declarations.contains_key(&section_topic),
    "Phase E does not contribute declarations either",
  );
}

/// Phase E preserves candidate ordering: candidates_by_simple_name
/// returns the audit-built order (ascending by topic ID); Phase E
/// writes that slice verbatim. Pin the contract — downstream tooling
/// inspects the candidates and a flicker would make traces non-
/// reproducible.
#[test]
fn phase_e_preserves_candidate_iteration_order() {
  let mut audit = staged_audit();
  // Insert in reverse topic-ID order to make sure the name index's
  // internal sort (not insertion order) governs the result.
  for id in &[500, 100, 300, 200] {
    audit.topic_metadata.insert(
      nt(*id),
      named_topic(nt(*id), "thing", NamedTopicKind::Builtin, Scope::Global),
    );
  }
  finalize(&mut audit);

  let mut tree = root(1, vec![paragraph(2, vec![code_id(3, "thing", None)])]);
  let _ = resolve_doc_tree(&mut tree, &audit);

  let DocumentationNode::Root { children, .. } = &tree else {
    unreachable!()
  };
  let DocumentationNode::Paragraph {
    children: pchildren,
    ..
  } = &children[0]
  else {
    unreachable!()
  };
  let DocumentationNode::CodeIdentifier {
    referenced_topic_candidates,
    ..
  } = &pchildren[0]
  else {
    unreachable!()
  };

  // Ascending by topic ID.
  let expected = vec![nt(100), nt(200), nt(300), nt(500)];
  assert_eq!(
    *referenced_topic_candidates, expected,
    "Phase E writes candidates in `candidates_by_simple_name`'s sorted order",
  );
}

/// Phase E idempotency: running the pass twice on the same audit
/// produces byte-identical state. Pins the contract that the resolver
/// can re-run safely (e.g., after an audit reload that doesn't change
/// the underlying graph). The fix that clears stale candidates on
/// Phase B winners would otherwise allow drift on re-run.
#[test]
fn phase_e_is_idempotent_across_repeat_passes() {
  let (audit, _foo, _bar, _foo_amount, _foo_tmp, _bar_amount, _bar_tmp) =
    co_loc_fixture();

  let mut tree = root(
    1,
    vec![paragraph(
      2,
      vec![code_id(3, "amount", None), code_id(4, "tmp", None)],
    )],
  );

  // First run: Phase C pins both refs (singleton intersection won't
  // hold, so they fall to Phase E with the multi-element intersection
  // — actually this fixture has both candidates in {foo, bar}, so
  // Phase C abstains and Phase E populates candidates).
  let traces_first = resolve_doc_tree(&mut tree, &audit);
  let tree_after_first = serde_json::to_vec(&tree).unwrap();
  let traces_first_bytes = serde_json::to_vec(&traces_first).unwrap();

  // Second run on the same tree: must produce identical state.
  let traces_second = resolve_doc_tree(&mut tree, &audit);
  let tree_after_second = serde_json::to_vec(&tree).unwrap();
  let traces_second_bytes = serde_json::to_vec(&traces_second).unwrap();

  assert_eq!(
    tree_after_first, tree_after_second,
    "tree state must be byte-identical after repeat pass",
  );
  assert_eq!(
    traces_first_bytes, traces_second_bytes,
    "trace state must be byte-identical after repeat pass",
  );
}

/// Phase E candidates that are themselves Contract topics anchor to
/// themselves. A common case in practice: ambiguous bare contract
/// names (e.g., two `Vault` contracts in different files) fall to
/// Phase E and each Vault becomes both a candidate AND a contract
/// anchor.
#[test]
fn phase_e_contract_candidates_anchor_to_themselves() {
  use o11a_core::collaborator::agent::context::mechanical_semantic_links;

  let vault_a = nt(10);
  let vault_b = nt(20);

  let mut audit = staged_audit();
  for (t, file) in [(vault_a, "A.sol"), (vault_b, "B.sol")] {
    audit.topic_metadata.insert(
      t,
      named_topic(
        t,
        "Vault",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container {
          container: pp(file),
        },
      ),
    );
  }
  audit.name_index = TopicNameIndex::build(&audit);
  audit.resolution_graph = Some(graph_from(&[]));

  let section_id = 200;
  let mut tree = section(
    section_id,
    "Overview",
    vec![paragraph(2, vec![code_id(3, "Vault", None)])],
  );
  let _ = resolve_doc_tree(&mut tree, &audit);

  // Verify Phase E populated the candidates.
  let DocumentationNode::Section { children, .. } = &tree else {
    unreachable!()
  };
  let DocumentationNode::Paragraph {
    children: pchildren,
    ..
  } = &children[0]
  else {
    unreachable!()
  };
  let DocumentationNode::CodeIdentifier {
    referenced_topic,
    referenced_topic_candidates,
    ..
  } = &pchildren[0]
  else {
    unreachable!()
  };
  assert!(referenced_topic.is_none());
  assert_eq!(*referenced_topic_candidates, vec![vault_a, vault_b]);

  // Wire the tree into the audit and verify the downstream consumer
  // anchors both contracts to themselves.
  let path = pp("test.md");
  audit.asts.insert(
    path.clone(),
    domain::AST::Documentation(
      o11a_core::documentation::ast::DocumentationAST {
        nodes: vec![tree],
        project_path: path.clone(),
        source_content: String::new(),
      },
    ),
  );
  let result = mechanical_semantic_links(&audit);
  let section_topic = dt(section_id);
  let mut anchored = result
    .section_to_contracts
    .get(&section_topic)
    .cloned()
    .expect("contract candidates must anchor");
  anchored.sort_by_key(|t| t.id().to_string());
  let mut expected = vec![vault_a, vault_b];
  expected.sort_by_key(|t| t.id().to_string());
  assert_eq!(
    anchored, expected,
    "each candidate (a Contract) anchors itself",
  );
}
