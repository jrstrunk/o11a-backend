use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::domain::topic;

use super::edge::EdgeType;

/// One outgoing edge from a source topic. The graph stores adjacency lists
/// keyed by source; undirected edges are materialized as two `OutEdge`
/// entries (one per direction).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OutEdge {
  pub dest: topic::Topic,
  pub edge_type: EdgeType,
  pub weight: f32,
}

/// A typed weighted graph of audit declarations. Storage is a per-source
/// adjacency list; edge insertion order is normalized at `finalize()` time
/// by sorting each list lexicographically by `(dest_topic_id,
/// edge_type_discriminant)`.
///
/// Determinism contract: same set of `add_edge` calls (in any order)
/// followed by `finalize()` produces byte-identical adjacency.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResolutionGraph {
  adjacency: BTreeMap<topic::Topic, Vec<OutEdge>>,
}

impl ResolutionGraph {
  pub fn new() -> Self {
    ResolutionGraph {
      adjacency: BTreeMap::new(),
    }
  }

  /// Append a directed edge `src → dest`. For undirected edges, callers
  /// must invoke `add_edge` twice (once in each direction). The list is
  /// not sorted on insertion; call `finalize()` once after all edges have
  /// been added to normalize ordering.
  pub fn add_edge(
    &mut self,
    src: topic::Topic,
    dest: topic::Topic,
    edge_type: EdgeType,
    weight: f32,
  ) {
    self.adjacency.entry(src).or_default().push(OutEdge {
      dest,
      edge_type,
      weight,
    });
  }

  /// Sort each adjacency list by `(dest_topic_id,
  /// edge_type_discriminant)`. Idempotent. Builders call this exactly
  /// once after every extractor has run.
  pub fn finalize(&mut self) {
    for edges in self.adjacency.values_mut() {
      edges.sort_by(|a, b| {
        a.dest
          .cmp(&b.dest)
          .then_with(|| a.edge_type.cmp(&b.edge_type))
      });
    }
  }

  /// Outgoing edges from `src`. Returns an empty slice when `src` has
  /// none.
  pub fn out_edges(&self, src: topic::Topic) -> &[OutEdge] {
    self
      .adjacency
      .get(&src)
      .map(|v| v.as_slice())
      .unwrap_or(&[])
  }

  /// Iterates every source topic in ascending topic-ID order. Topics
  /// that appear only as destinations are not yielded.
  pub fn nodes(&self) -> impl Iterator<Item = topic::Topic> + '_ {
    self.adjacency.keys().copied()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn t(id: i32) -> topic::Topic {
    topic::new_node_topic(&id)
  }

  #[test]
  fn add_edge_then_out_edges_returns_inserted_edge() {
    let mut g = ResolutionGraph::new();
    g.add_edge(t(1), t(2), EdgeType::Calls, 0.7);
    let edges = g.out_edges(t(1));
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].dest, t(2));
    assert_eq!(edges[0].edge_type, EdgeType::Calls);
    assert_eq!(edges[0].weight, 0.7);
  }

  #[test]
  fn out_edges_for_unknown_source_is_empty() {
    let g = ResolutionGraph::new();
    assert!(g.out_edges(t(99)).is_empty());
  }

  #[test]
  fn finalize_sorts_by_dest_then_edge_type() {
    let mut g = ResolutionGraph::new();
    let s = t(1);
    // Insert in deliberately scrambled order.
    g.add_edge(s, t(5), EdgeType::Calls, 0.7);
    g.add_edge(s, t(2), EdgeType::References, 0.5);
    g.add_edge(s, t(5), EdgeType::References, 0.5);
    g.add_edge(s, t(3), EdgeType::Calls, 0.7);
    g.add_edge(s, t(2), EdgeType::Calls, 0.7);

    g.finalize();

    let edges = g.out_edges(s);
    let pairs: Vec<(topic::Topic, EdgeType)> =
      edges.iter().map(|e| (e.dest, e.edge_type)).collect();
    // Sorted by (dest_topic_id, edge_type discriminant).
    // ContainsMember (declaration index 0) < ... < Calls (3) < References (4).
    assert_eq!(
      pairs,
      vec![
        (t(2), EdgeType::Calls),
        (t(2), EdgeType::References),
        (t(3), EdgeType::Calls),
        (t(5), EdgeType::Calls),
        (t(5), EdgeType::References),
      ]
    );
  }

  #[test]
  fn finalize_is_idempotent() {
    let mut g = ResolutionGraph::new();
    g.add_edge(t(1), t(3), EdgeType::Calls, 0.7);
    g.add_edge(t(1), t(2), EdgeType::Calls, 0.7);

    g.finalize();
    let after_first: Vec<OutEdge> = g.out_edges(t(1)).to_vec();
    g.finalize();
    let after_second: Vec<OutEdge> = g.out_edges(t(1)).to_vec();
    assert_eq!(after_first, after_second);
  }

  #[test]
  fn finalize_preserves_per_source_independence() {
    let mut g = ResolutionGraph::new();
    g.add_edge(t(1), t(20), EdgeType::Calls, 0.7);
    g.add_edge(t(2), t(10), EdgeType::Calls, 0.7);
    g.finalize();
    // Each source has exactly its own one outgoing edge — finalize
    // must not bleed edges across sources.
    assert_eq!(g.out_edges(t(1)).len(), 1);
    assert_eq!(g.out_edges(t(1))[0].dest, t(20));
    assert_eq!(g.out_edges(t(2)).len(), 1);
    assert_eq!(g.out_edges(t(2))[0].dest, t(10));
  }

  #[test]
  fn nodes_returns_sources_in_ascending_order() {
    let mut g = ResolutionGraph::new();
    g.add_edge(t(5), t(1), EdgeType::Calls, 0.7);
    g.add_edge(t(2), t(1), EdgeType::Calls, 0.7);
    g.add_edge(t(8), t(1), EdgeType::Calls, 0.7);
    g.finalize();

    let nodes: Vec<topic::Topic> = g.nodes().collect();
    assert_eq!(nodes, vec![t(2), t(5), t(8)]);
  }

  #[test]
  fn nodes_excludes_dest_only_topics() {
    // t(99) appears only as a destination, never as a source. Per Phase 1's
    // contract, nodes() yields source topics only.
    let mut g = ResolutionGraph::new();
    g.add_edge(t(1), t(99), EdgeType::Calls, 0.7);
    g.finalize();

    let nodes: Vec<topic::Topic> = g.nodes().collect();
    assert_eq!(nodes, vec![t(1)]);
  }

  #[test]
  fn undirected_edges_inserted_as_two_directed_entries() {
    // Builders materialize an undirected edge by calling add_edge twice.
    // The graph layer treats both calls as ordinary directed entries.
    let mut g = ResolutionGraph::new();
    g.add_edge(t(1), t(2), EdgeType::Implements, 0.8);
    g.add_edge(t(2), t(1), EdgeType::Implements, 0.8);
    g.finalize();

    assert_eq!(g.out_edges(t(1))[0].dest, t(2));
    assert_eq!(g.out_edges(t(2))[0].dest, t(1));
    let nodes: Vec<topic::Topic> = g.nodes().collect();
    assert_eq!(nodes, vec![t(1), t(2)]);
  }

  #[test]
  fn deterministic_under_insertion_order_permutation() {
    // Inserting the same edges in two different orders produces
    // byte-identical adjacency after finalize().
    let mut a = ResolutionGraph::new();
    a.add_edge(t(1), t(3), EdgeType::Calls, 0.7);
    a.add_edge(t(1), t(2), EdgeType::References, 0.5);
    a.add_edge(t(1), t(3), EdgeType::References, 0.5);
    a.finalize();

    let mut b = ResolutionGraph::new();
    b.add_edge(t(1), t(3), EdgeType::References, 0.5);
    b.add_edge(t(1), t(2), EdgeType::References, 0.5);
    b.add_edge(t(1), t(3), EdgeType::Calls, 0.7);
    b.finalize();

    assert_eq!(a, b);
    assert_eq!(
      serde_json::to_vec(&a).unwrap(),
      serde_json::to_vec(&b).unwrap()
    );
  }

  #[test]
  fn empty_graph_serde_round_trip() {
    let g = ResolutionGraph::new();
    let json = serde_json::to_string(&g).unwrap();
    let back: ResolutionGraph = serde_json::from_str(&json).unwrap();
    assert_eq!(g, back);
  }

  #[test]
  fn populated_graph_serde_round_trip() {
    let mut g = ResolutionGraph::new();
    g.add_edge(t(1), t(2), EdgeType::Calls, 0.7);
    g.add_edge(t(2), t(1), EdgeType::Implements, 0.8);
    g.add_edge(t(1), t(3), EdgeType::References, 0.5);
    g.finalize();

    let json = serde_json::to_string(&g).unwrap();
    let back: ResolutionGraph = serde_json::from_str(&json).unwrap();
    assert_eq!(g, back);
    assert_eq!(back.out_edges(t(1)).len(), 2);
  }

  #[test]
  fn weight_difference_breaks_equality() {
    // The determinism contract is byte-equality of the serialized
    // graph; weights are part of the bytes. A regression where two
    // extractor runs produced edges with subtly different weights
    // would otherwise slip through the structural equality checks.
    let mut a = ResolutionGraph::new();
    a.add_edge(t(1), t(2), EdgeType::Calls, 0.7);
    a.finalize();

    let mut b = ResolutionGraph::new();
    b.add_edge(t(1), t(2), EdgeType::Calls, 0.8);
    b.finalize();

    assert_ne!(a, b);
    assert_ne!(
      serde_json::to_vec(&a).unwrap(),
      serde_json::to_vec(&b).unwrap()
    );
  }

  #[test]
  fn add_edge_negative_topic_ids_sort_below_positives() {
    // Solidity built-ins use negative node IDs (e.g. N-8, N-27). The
    // graph must order them deterministically alongside positive IDs.
    let mut g = ResolutionGraph::new();
    g.add_edge(t(0), t(5), EdgeType::Calls, 0.7);
    g.add_edge(t(0), t(-8), EdgeType::Calls, 0.7);
    g.add_edge(t(0), t(-100), EdgeType::Calls, 0.7);
    g.finalize();

    let dests: Vec<topic::Topic> =
      g.out_edges(t(0)).iter().map(|e| e.dest).collect();
    assert_eq!(dests, vec![t(-100), t(-8), t(5)]);
  }
}
