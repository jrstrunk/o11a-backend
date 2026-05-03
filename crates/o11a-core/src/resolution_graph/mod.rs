//! Typed weighted graph of audit declarations used for ambiguity-resolving
//! personalized PageRank. The graph spans the entire audit; per-language
//! extractors contribute their edge sets through the `Extractor` trait.
//!
//! See `crates/o11a-analyze/docs/specs/semantic-resolution-graph.md` and
//! `crates/o11a-analyze/docs/build-plans/semantic-resolution-graph.md`.

pub mod builder;
pub mod edge;
pub mod graph;
pub mod solidity_extractor;

pub use builder::{Extractor, build};
pub use edge::{Direction, EdgeType};
pub use graph::{OutEdge, ResolutionGraph};
pub use solidity_extractor::SolidityExtractor;

#[cfg(test)]
mod tests {
  use super::*;
  use crate::domain::{
    AuditData, NamedTopicKind, NamedTopicVisibility, Scope, TopicMetadata,
    TopicNameIndex, new_audit_data, topic,
  };
  use std::collections::HashSet;

  fn empty_audit() -> AuditData {
    new_audit_data("test".to_string(), HashSet::new(), None)
  }

  fn named_topic(t: topic::Topic, name: &str) -> TopicMetadata {
    TopicMetadata::NamedTopic {
      topic: t,
      scope: Scope::Global,
      kind: NamedTopicKind::Builtin,
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

  /// Build an AuditData with a small set of named topics, an
  /// inheritance edge, and a built name index — close enough to a real
  /// post-analyzer state to exercise the builder's read paths against
  /// Phase 0's outputs without requiring the full Solidity pipeline.
  fn populated_audit() -> AuditData {
    let mut audit = empty_audit();
    let parent = topic::new_node_topic(&100);
    let child = topic::new_node_topic(&200);
    let other = topic::new_node_topic(&300);
    audit.topic_metadata.insert(parent, named_topic(parent, "Parent"));
    audit.topic_metadata.insert(child, named_topic(child, "Child"));
    // Two topics with the same name to exercise candidate dedup/lookup.
    audit.topic_metadata.insert(other, named_topic(other, "Child"));
    audit.inheritance.insert(child, vec![parent]);
    audit.name_index = TopicNameIndex::build(&audit);
    audit
  }

  #[test]
  fn build_empty_audit_produces_empty_graph() {
    let audit = empty_audit();
    let graph = build(&audit);
    assert_eq!(graph.nodes().count(), 0);
  }

  #[test]
  fn build_is_deterministic() {
    let audit_a = empty_audit();
    let audit_b = empty_audit();
    let graph_a = build(&audit_a);
    let graph_b = build(&audit_b);
    let bytes_a = serde_json::to_vec(&graph_a).unwrap();
    let bytes_b = serde_json::to_vec(&graph_b).unwrap();
    assert_eq!(bytes_a, bytes_b);
  }

  #[test]
  fn build_against_populated_audit_does_not_panic() {
    // Phase 1 has no registered extractors yet, so build returns an
    // empty graph even when AuditData is populated. The point of this
    // test is that build tolerates real Phase 0 outputs (inheritance,
    // a non-empty name index, multiple NamedTopic entries) without
    // panicking on shape assumptions a future extractor might break.
    let audit = populated_audit();
    let graph = build(&audit);
    assert_eq!(graph.nodes().count(), 0);
  }

  #[test]
  fn build_against_populated_audit_is_deterministic() {
    let audit = populated_audit();
    let g1 = build(&audit);
    let g2 = build(&audit);
    assert_eq!(g1, g2);
    assert_eq!(
      serde_json::to_vec(&g1).unwrap(),
      serde_json::to_vec(&g2).unwrap()
    );
  }

  // ---------------------------------------------------------------------
  // Extractor trait contract
  //
  // The builder pipeline today registers no extractors. Tests below
  // exercise the trait's invariants against a hand-rolled mock so the
  // contract Phase 2's SolidityExtractor will rely on stays stable.
  // ---------------------------------------------------------------------

  /// Mock extractor: emits a fixed set of edges in a deliberately
  /// scrambled order so the test can verify that whoever orchestrates
  /// extraction (today the test, tomorrow `build`) finalizes the
  /// graph.
  struct ScrambledMockExtractor;

  impl Extractor for ScrambledMockExtractor {
    fn applies_to(&self, _audit_data: &AuditData) -> bool {
      true
    }

    fn extract(
      &self,
      _audit_data: &AuditData,
      graph: &mut ResolutionGraph,
    ) {
      let s = topic::new_node_topic(&1);
      // Insert in (dest desc, edge_type desc) order to exercise the
      // sort.
      graph.add_edge(s, topic::new_node_topic(&5), EdgeType::References, 0.5);
      graph.add_edge(s, topic::new_node_topic(&5), EdgeType::Calls, 0.7);
      graph.add_edge(s, topic::new_node_topic(&2), EdgeType::Calls, 0.7);
    }
  }

  /// Mock extractor whose `applies_to` always returns false. Used to
  /// confirm that a non-applicable extractor is not run.
  struct InertExtractor;

  impl Extractor for InertExtractor {
    fn applies_to(&self, _audit_data: &AuditData) -> bool {
      false
    }

    fn extract(
      &self,
      _audit_data: &AuditData,
      _graph: &mut ResolutionGraph,
    ) {
      panic!("InertExtractor.extract must not be called");
    }
  }

  /// Direct re-implementation of `build`'s orchestration loop. Using
  /// this lets tests exercise the trait contract with mock extractors
  /// without exposing a public `build_with_extractors` purely for
  /// tests.
  fn run_extractors(
    audit: &AuditData,
    extractors: Vec<Box<dyn Extractor>>,
  ) -> ResolutionGraph {
    let mut graph = ResolutionGraph::new();
    for e in &extractors {
      if e.applies_to(audit) {
        e.extract(audit, &mut graph);
      }
    }
    graph.finalize();
    graph
  }

  #[test]
  fn extractor_pipeline_finalizes_after_extraction() {
    let audit = empty_audit();
    let g = run_extractors(
      &audit,
      vec![Box::new(ScrambledMockExtractor) as Box<dyn Extractor>],
    );
    let s = topic::new_node_topic(&1);
    let pairs: Vec<(topic::Topic, EdgeType)> =
      g.out_edges(s).iter().map(|e| (e.dest, e.edge_type)).collect();
    // Sorted ascending by (dest, edge_type) — proves finalize ran.
    assert_eq!(
      pairs,
      vec![
        (topic::new_node_topic(&2), EdgeType::Calls),
        (topic::new_node_topic(&5), EdgeType::Calls),
        (topic::new_node_topic(&5), EdgeType::References),
      ]
    );
  }

  #[test]
  fn extractor_pipeline_skips_extractors_that_do_not_apply() {
    let audit = empty_audit();
    // InertExtractor's extract panics; if applies_to is honored we
    // never reach it.
    let g = run_extractors(
      &audit,
      vec![Box::new(InertExtractor) as Box<dyn Extractor>],
    );
    assert_eq!(g.nodes().count(), 0);
  }

  #[test]
  fn extractor_pipeline_runs_multiple_extractors_in_order() {
    struct AddsSourceOne;
    struct AddsSourceTwo;
    impl Extractor for AddsSourceOne {
      fn applies_to(&self, _: &AuditData) -> bool {
        true
      }
      fn extract(&self, _: &AuditData, g: &mut ResolutionGraph) {
        g.add_edge(
          topic::new_node_topic(&1),
          topic::new_node_topic(&10),
          EdgeType::Calls,
          0.7,
        );
      }
    }
    impl Extractor for AddsSourceTwo {
      fn applies_to(&self, _: &AuditData) -> bool {
        true
      }
      fn extract(&self, _: &AuditData, g: &mut ResolutionGraph) {
        g.add_edge(
          topic::new_node_topic(&2),
          topic::new_node_topic(&20),
          EdgeType::Calls,
          0.7,
        );
      }
    }

    let audit = empty_audit();
    let g = run_extractors(
      &audit,
      vec![
        Box::new(AddsSourceOne) as Box<dyn Extractor>,
        Box::new(AddsSourceTwo) as Box<dyn Extractor>,
      ],
    );
    let nodes: Vec<topic::Topic> = g.nodes().collect();
    assert_eq!(
      nodes,
      vec![topic::new_node_topic(&1), topic::new_node_topic(&2)]
    );
  }

  // ---------------------------------------------------------------------
  // Phase 0 ↔ Phase 1 integration
  //
  // Phase 1 doesn't consume Phase 0 outputs yet (Phase 2 will), but the
  // tests below pin down the wiring that connects them so a Phase 2
  // extractor can rely on it.
  // ---------------------------------------------------------------------

  #[test]
  fn populated_audit_exposes_phase0_data_to_extractors() {
    // A Phase 2 extractor will read `inheritance` and `name_index`
    // candidates from a `&AuditData`. Pin down reachability and the
    // ordering contract (`candidates_by_simple_name` is already sorted
    // ascending by topic ID) so the extractor can rely on both without
    // re-sorting.
    let audit = populated_audit();
    let parent = topic::new_node_topic(&100);
    let child = topic::new_node_topic(&200);
    let other = topic::new_node_topic(&300);

    assert_eq!(audit.inheritance.get(&child), Some(&vec![parent]));
    assert_eq!(
      audit.name_index.candidates_by_simple_name("Child"),
      &[child, other],
    );
  }

  #[test]
  fn audit_data_carries_resolution_graph_after_build() {
    // Mirrors the wiring Phase 4 of the build plan will land:
    // analysis.rs sets `audit_data.resolution_graph = Some(build(&audit))`.
    let mut audit = populated_audit();
    audit.resolution_graph = Some(build(&audit));
    assert!(audit.resolution_graph.is_some());
  }

  #[test]
  fn resolution_graph_field_round_trips_through_audit_data() {
    // Wiring sanity: the field on AuditData uses the same
    // ResolutionGraph type the builder returns. Stuff a build()
    // result into the field, read it back, compare.
    let mut audit = populated_audit();
    let built: ResolutionGraph = build(&audit);
    audit.resolution_graph = Some(built.clone());
    assert_eq!(audit.resolution_graph.as_ref(), Some(&built));
  }
}
