//! Personalized `PageRank` over a `ResolutionGraph`.
//!
//! Phase B of the resolution pipeline (see
//! `crates/o11a-analyze/docs/specs/semantic-resolution-graph.md`). The
//! engine is a pure function of `(graph, seeds)` — no environment
//! dependencies, no parallelism — and is consumed by the per-section /
//! per-NatSpec resolution passes wired in later phases.
//!
//! Determinism contract:
//!
//! * Damping factor and iteration count are compile-time constants. We
//!   never short-circuit on convergence; the iteration count *is* the
//!   determinism guarantee.
//! * The seed input and result are `BTreeMap`s keyed by topic ID, so
//!   callers see entries in topic-ID order. Internally the engine
//!   materializes a single `Vec<topic::Topic>` (the node universe in
//!   ascending topic-ID order) and uses positions in that vector as the
//!   indices of all hot-path data structures. Position order is topic-ID
//!   order, so "ascending source position" and "ascending source topic
//!   ID" are the same invariant.
//! * Floating-point math is sequential and `f32` end-to-end. Per-node
//!   accumulators add the restart, the dangling-mass redistribution,
//!   and each predecessor's contribution in that order.
//! * Dangling nodes (no outgoing edges) redistribute their full mass
//!   back through the (normalized) seed vector each iteration. This
//!   conserves total mass at 1.0 and makes seeded nodes win against
//!   sinks that bleed mass; the alternative self-loop convention would
//!   instead let dangling sinks accumulate mass indefinitely.

use std::collections::{BTreeMap, BTreeSet};

use crate::domain::topic;

use super::graph::ResolutionGraph;

/// Damping factor `d`. Standard value; calibration lives in the
/// edge-weight table, not here.
const DAMPING: f32 = 0.85;

/// Fixed iteration count. Convergence-based early-stop is forbidden by
/// the determinism contract — every input runs the same number of
/// iterations.
const ITERATIONS: u32 = 30;

/// Compute personalized `PageRank` against `graph`, restarted at the
/// (un-normalized) `seeds` distribution.
///
/// The output map has an entry for every topic that is either a graph
/// node (appears as a source or destination of any edge) or a seeded
/// topic. Seeds whose total weight is zero produce an all-zero output
/// (no personalization → no mass to spread).
#[must_use]
pub fn personalized_pagerank(
  graph: &ResolutionGraph,
  seeds: &BTreeMap<topic::Topic, f32>,
) -> BTreeMap<topic::Topic, f32> {
  // -----------------------------------------------------------------
  // 1. Node universe.
  //
  // The graph's adjacency map only keys *source* topics; destinations
  // appear inside `OutEdge` payloads. PR needs a rank slot for every
  // topic that participates — sources, destinations, and seeded
  // topics — so we materialize the union here. The `BTreeSet` anchors
  // ascending topic-ID order; the resulting `Vec` is the canonical
  // position index used by every other data structure below.
  // -----------------------------------------------------------------
  let mut node_set: BTreeSet<topic::Topic> = BTreeSet::new();
  for src in graph.nodes() {
    node_set.insert(src);
    for edge in graph.out_edges(src) {
      node_set.insert(edge.dest);
    }
  }
  for k in seeds.keys() {
    node_set.insert(*k);
  }
  let nodes: Vec<topic::Topic> = node_set.into_iter().collect();
  let n = nodes.len();

  if n == 0 {
    return BTreeMap::new();
  }

  // Topic → position map. Built once; from here on, the algorithm
  // operates on positions and only converts back to topics when
  // assembling the result.
  let position: BTreeMap<topic::Topic, usize> =
    nodes.iter().enumerate().map(|(i, t)| (*t, i)).collect();

  // -----------------------------------------------------------------
  // 2. Predecessor lists and per-source total outgoing weight, both
  //    indexed by node position.
  //
  // `graph.nodes()` yields sources in ascending topic-ID order, which
  // (because `nodes` is sorted ascending by topic ID) is also
  // ascending source-position order. Predecessor entries are therefore
  // appended to each destination's list in the exact order the
  // determinism contract pins for the inner summation. No defensive
  // re-sort is needed; if `graph::nodes` ever loses that ordering, the
  // contract test `predecessor_summation_order_is_ascending_source_id`
  // is the canary.
  // -----------------------------------------------------------------
  let mut predecessors: Vec<Vec<(usize, f32)>> = vec![Vec::new(); n];
  let mut total_out_weight: Vec<f32> = vec![0.0; n];
  for src in graph.nodes() {
    let src_pos = position[&src];
    let mut total = 0.0_f32;
    for edge in graph.out_edges(src) {
      total += edge.weight;
      let dest_pos = position[&edge.dest];
      predecessors[dest_pos].push((src_pos, edge.weight));
    }
    total_out_weight[src_pos] = total;
  }

  // -----------------------------------------------------------------
  // 3. Personalization vector `s`.
  //
  // Normalize the seed map to sum to 1.0. Zero-sum (or empty) seeds
  // leave `s` at all zeros; the PR fixed point is then the all-zero
  // vector, which we surface verbatim.
  // -----------------------------------------------------------------
  let total_seed_weight: f32 = seeds.values().sum();
  let mut s: Vec<f32> = vec![0.0; n];
  if total_seed_weight > 0.0 {
    for (k, v) in seeds {
      s[position[k]] = v / total_seed_weight;
    }
  }

  // -----------------------------------------------------------------
  // 4. Iterate.
  //
  // `r` initializes at `s`. Dangling nodes (no outgoing edges) have
  // their entire mass redistributed through `s` each iteration — this
  // conserves total mass at 1.0 and means a sink node never
  // out-competes seeded nodes on PR mass. Two `Vec<f32>` buffers are
  // swapped in place to avoid 30× the allocations a fresh map per
  // iteration would cost.
  // -----------------------------------------------------------------
  let mut r: Vec<f32> = s.clone();
  let mut r_new: Vec<f32> = vec![0.0; n];
  let dangling: Vec<usize> = (0..n)
    .filter(|i| graph.out_edges(nodes[*i]).is_empty())
    .collect();

  for _ in 0..ITERATIONS {
    let mut dangling_mass = 0.0_f32;
    for &i in &dangling {
      dangling_mass += r[i];
    }

    for i in 0..n {
      // Sum order: restart, then dangling spillover through `s`, then
      // each predecessor in ascending source-position order (=
      // ascending source topic-ID order). Fixed sequential order is
      // the determinism contract.
      let mut new_value = (1.0 - DAMPING) * s[i];
      new_value += DAMPING * dangling_mass * s[i];
      for &(p, w) in &predecessors[i] {
        new_value += DAMPING * r[p] * (w / total_out_weight[p]);
      }
      r_new[i] = new_value;
    }
    std::mem::swap(&mut r, &mut r_new);
  }

  nodes.into_iter().zip(r).collect()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::resolution_graph::edge::EdgeType;

  fn t(id: i32) -> topic::Topic {
    topic::new_node_topic(&id)
  }

  /// Build a graph from a list of `(src, dest, edge_type)` triples
  /// using each edge type's default weight. `finalize()` is invoked so
  /// adjacency ordering matches what the production builder produces.
  fn graph_from(
    edges: &[(topic::Topic, topic::Topic, EdgeType)],
  ) -> ResolutionGraph {
    let mut g = ResolutionGraph::new();
    for (src, dst, et) in edges {
      g.add_edge(*src, *dst, *et, et.default_weight());
    }
    g.finalize();
    g
  }

  fn seed(pairs: &[(topic::Topic, f32)]) -> BTreeMap<topic::Topic, f32> {
    pairs.iter().copied().collect()
  }

  // ---------------------------------------------------------------------
  // Algorithm fixtures from the build plan
  // ---------------------------------------------------------------------

  /// Trivial graph: 1 node, no edges, single seed of weight 1.0.
  /// With redistribution-into-seed dangling handling, the node's full
  /// mass loops through the personalization vector each iteration and
  /// stays at 1.0.
  #[test]
  fn trivial_graph_single_seeded_node_keeps_full_mass() {
    let graph = ResolutionGraph::new();
    let s = seed(&[(t(1), 1.0)]);
    let r = personalized_pagerank(&graph, &s);
    assert!((r[&t(1)] - 1.0).abs() < 1e-6, "got {}", r[&t(1)]);
    assert_eq!(r.len(), 1);
  }

  /// Two-node chain `A → B`. Seeded at A. After 30 iterations A still
  /// holds more PR mass than B because the personalization vector
  /// keeps pulling mass back to A; both are non-zero because PR mass
  /// flows A → B every iteration.
  #[test]
  fn two_node_chain_seeded_source_dominates_sink() {
    let graph = graph_from(&[(t(1), t(2), EdgeType::Calls)]);
    let s = seed(&[(t(1), 1.0)]);
    let r = personalized_pagerank(&graph, &s);

    let r_a = r[&t(1)];
    let r_b = r[&t(2)];
    assert!(r_b > 0.0, "B's PR must be > 0 after 30 iterations: {}", r_b);
    assert!(
      r_a > r_b,
      "A's PR ({}) must dominate B's ({}) — seed at A pulls mass back",
      r_a,
      r_b
    );
  }

  /// Three-node star: A undirected with B and C (so four directed
  /// edges total). Seeded at A. By topology symmetry, B and C end up
  /// with byte-identical PR values.
  #[test]
  fn three_node_star_symmetric_neighbors_score_identically() {
    let graph = graph_from(&[
      (t(1), t(2), EdgeType::ContainsMember),
      (t(2), t(1), EdgeType::ContainsMember),
      (t(1), t(3), EdgeType::ContainsMember),
      (t(3), t(1), EdgeType::ContainsMember),
    ]);
    let s = seed(&[(t(1), 1.0)]);
    let r = personalized_pagerank(&graph, &s);

    assert_eq!(
      r[&t(2)].to_bits(),
      r[&t(3)].to_bits(),
      "symmetric neighbors B={}, C={} must match bit-for-bit",
      r[&t(2)],
      r[&t(3)]
    );
    assert!(
      r[&t(1)] > r[&t(2)],
      "seeded center should outscore its leaves: A={}, B={}",
      r[&t(1)],
      r[&t(2)]
    );
  }

  /// Determinism: identical graph + identical seed → byte-identical
  /// output. This is the contract that gates Phase 6 / 7's "build the
  /// same audit twice and assert the same resolutions".
  #[test]
  fn identical_inputs_produce_byte_identical_output() {
    let graph = graph_from(&[
      (t(1), t(2), EdgeType::Calls),
      (t(2), t(3), EdgeType::Calls),
      (t(3), t(1), EdgeType::Calls),
      (t(1), t(4), EdgeType::References),
      (t(4), t(2), EdgeType::References),
    ]);
    let s = seed(&[(t(1), 1.0), (t(2), 0.5)]);

    let r_a = personalized_pagerank(&graph, &s);
    let r_b = personalized_pagerank(&graph, &s);

    let bytes_a = serde_json::to_vec(&r_a).unwrap();
    let bytes_b = serde_json::to_vec(&r_b).unwrap();
    assert_eq!(bytes_a, bytes_b);
  }

  /// Floating-point summation-order regression guard, per build plan
  /// Phase 3 task 4. The expected values below are hand-pinned bit
  /// patterns captured from a reference run with the documented
  /// summation order (restart → dangling spillover → predecessors
  /// ascending by source ID). Any change that perturbs any of:
  ///
  ///  * the algorithm (damping, iteration count, dangling convention),
  ///  * the per-node sum order,
  ///  * the predecessor traversal order, or
  ///  * the f32 promotion/truncation pattern,
  ///
  /// will shift at least one bit pattern and trip this test. To
  /// regenerate (after an *intentional* algorithm change), follow the
  /// recipe inlined next to the `expected` table below.
  ///
  /// The graph is chosen to put non-trivial mass on every node — a
  /// triangle plus two extra predecessors converging on node 2 with
  /// distinct edge weights — so f32 rounding is exercised in every
  /// accumulator and the per-destination summation has a non-trivial
  /// ordering to depend on.
  #[test]
  fn engine_output_matches_pinned_reference() {
    let graph = graph_from(&[
      (t(1), t(2), EdgeType::ContainsMember),
      (t(2), t(1), EdgeType::ContainsMember),
      (t(2), t(3), EdgeType::ContainsMember),
      (t(3), t(2), EdgeType::ContainsMember),
      (t(1), t(3), EdgeType::ContainsMember),
      (t(3), t(1), EdgeType::ContainsMember),
      (t(4), t(2), EdgeType::Calls),
      (t(5), t(2), EdgeType::References),
    ]);
    let s = seed(&[(t(1), 1.0), (t(4), 0.3)]);

    let r = personalized_pagerank(&graph, &s);

    // Every node from the universe must appear in the output, in
    // ascending topic-ID order.
    let topics: Vec<topic::Topic> = r.keys().copied().collect();
    assert_eq!(topics, vec![t(1), t(2), t(3), t(4), t(5)]);

    // Hand-pinned bit patterns. Regenerate via:
    //
    //     for topic in &topics {
    //         println!("(t({}), 0x{:08x}),", topic.numeric_id(), r[topic].to_bits());
    //     }
    let expected: &[(topic::Topic, u32)] = &[
      (t(1), 0x3ebcdf9a), // = 0.36889344
      (t(2), 0x3e9dfcd4), // = 0.30856955
      (t(3), 0x3e936a7c), // = 0.2879218
      (t(4), 0x3d0dc8dc), // = 0.034615383
      (t(5), 0x00000000), // = 0
    ];

    for (topic, bits) in expected {
      let got = r[topic].to_bits();
      assert_eq!(
        got, *bits,
        "topic {topic}: expected bits 0x{bits:08x}, got 0x{got:08x} (= {})",
        r[topic],
      );
    }

    // Mass conservation invariant under the redistribute-into-seed
    // dangling convention: total ≈ 1.0. Tested independently of the
    // pinned bits so a future bit regeneration that violates the
    // invariant trips here even if the pinned values are updated
    // sloppily.
    let total: f32 = topics.iter().map(|topic| r[topic]).sum();
    assert!(
      (total - 1.0).abs() < 1e-4,
      "PR mass must conserve under redistribution: total={}",
      total
    );
  }

  // ---------------------------------------------------------------------
  // Behavioral edge cases
  //
  // Not in the build plan's named four, but they pin down the
  // contract documented at the top of this module so a future change
  // that violates an invariant trips at least one test.
  // ---------------------------------------------------------------------

  #[test]
  fn empty_graph_and_empty_seeds_returns_empty_map() {
    let graph = ResolutionGraph::new();
    let s: BTreeMap<topic::Topic, f32> = BTreeMap::new();
    let r = personalized_pagerank(&graph, &s);
    assert!(r.is_empty());
  }

  #[test]
  fn empty_seeds_against_populated_graph_returns_all_zero() {
    let graph = graph_from(&[(t(1), t(2), EdgeType::Calls)]);
    let s: BTreeMap<topic::Topic, f32> = BTreeMap::new();
    let r = personalized_pagerank(&graph, &s);
    // Both nodes appear in the output and both are zero.
    assert_eq!(r.len(), 2);
    assert_eq!(r[&t(1)], 0.0);
    assert_eq!(r[&t(2)], 0.0);
  }

  #[test]
  fn seed_outside_graph_appears_in_output() {
    // A seeded topic that isn't referenced by any edge must still
    // appear in the result — callers expect to look up every seed.
    let graph = graph_from(&[(t(1), t(2), EdgeType::Calls)]);
    let s = seed(&[(t(1), 0.5), (t(99), 0.5)]);
    let r = personalized_pagerank(&graph, &s);
    assert!(r.contains_key(&t(99)));
    assert!(r[&t(99)] > 0.0);
  }

  #[test]
  fn seed_weights_are_normalized_so_scaling_does_not_change_output() {
    let graph = graph_from(&[
      (t(1), t(2), EdgeType::Calls),
      (t(2), t(3), EdgeType::Calls),
    ]);
    let scaled_low = seed(&[(t(1), 1.0), (t(2), 2.0)]);
    let scaled_high = seed(&[(t(1), 100.0), (t(2), 200.0)]);
    let r_low = personalized_pagerank(&graph, &scaled_low);
    let r_high = personalized_pagerank(&graph, &scaled_high);
    // Same ratios → identical normalized seed vector → identical
    // output bit-for-bit.
    assert_eq!(
      serde_json::to_vec(&r_low).unwrap(),
      serde_json::to_vec(&r_high).unwrap()
    );
  }

  #[test]
  fn weighted_edges_split_mass_in_proportion() {
    // Source `t(1)` has two outgoing edges: one heavy, one light.
    // After one iteration, the heavier-edge destination should hold
    // a larger share of the mass that flowed out of t(1).
    //
    // ContainsMember weight = 1.0; ErrorThrown weight = 0.4. The
    // heavy edge wins.
    let graph = graph_from(&[
      (t(1), t(2), EdgeType::ContainsMember),
      (t(1), t(3), EdgeType::ErrorThrown),
    ]);
    let s = seed(&[(t(1), 1.0)]);
    let r = personalized_pagerank(&graph, &s);
    assert!(
      r[&t(2)] > r[&t(3)],
      "heavier edge destination must outscore lighter: t(2)={}, t(3)={}",
      r[&t(2)],
      r[&t(3)]
    );
  }

  #[test]
  fn predecessor_summation_order_is_ascending_source_id() {
    // Build a graph whose adjacency was inserted in deliberately
    // shuffled order. Because `add_edge` does not sort on insertion
    // and `finalize` only sorts per-source by (dest, edge_type), the
    // *cross-source* visit order is governed by the BTreeMap key
    // order — already topic-ID ascending. The invariant we want to
    // pin: the predecessor list for every destination is consumed
    // in ascending source-ID order regardless of how the edges were
    // inserted. We test it by running PR twice — once on a graph
    // built in one order, once in the reverse order — and asserting
    // bit-identical output.
    let mut g_forward = ResolutionGraph::new();
    g_forward.add_edge(
      t(1),
      t(10),
      EdgeType::Calls,
      EdgeType::Calls.default_weight(),
    );
    g_forward.add_edge(
      t(2),
      t(10),
      EdgeType::Calls,
      EdgeType::Calls.default_weight(),
    );
    g_forward.add_edge(
      t(3),
      t(10),
      EdgeType::Calls,
      EdgeType::Calls.default_weight(),
    );
    g_forward.finalize();

    let mut g_reverse = ResolutionGraph::new();
    g_reverse.add_edge(
      t(3),
      t(10),
      EdgeType::Calls,
      EdgeType::Calls.default_weight(),
    );
    g_reverse.add_edge(
      t(2),
      t(10),
      EdgeType::Calls,
      EdgeType::Calls.default_weight(),
    );
    g_reverse.add_edge(
      t(1),
      t(10),
      EdgeType::Calls,
      EdgeType::Calls.default_weight(),
    );
    g_reverse.finalize();

    let s = seed(&[(t(1), 1.0), (t(2), 1.0), (t(3), 1.0)]);
    let r_forward = personalized_pagerank(&g_forward, &s);
    let r_reverse = personalized_pagerank(&g_reverse, &s);
    assert_eq!(
      serde_json::to_vec(&r_forward).unwrap(),
      serde_json::to_vec(&r_reverse).unwrap(),
    );
  }
}
