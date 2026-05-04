use serde::{Deserialize, Serialize};

/// Whether an edge is directed (source → dest only) or undirected (both
/// directions). Undirected edges are materialized by the builder as two
/// directed entries; this enum is metadata for extractors deciding whether to
/// emit one or two `add_edge` calls.
#[derive(
  Debug,
  Clone,
  Copy,
  PartialEq,
  Eq,
  PartialOrd,
  Ord,
  Hash,
  Serialize,
  Deserialize,
)]
pub enum Direction {
  Directed,
  Undirected,
}

/// The kind of relationship an edge represents. Variants are split between a
/// universal core (relationships present in every typed source language we
/// expect to support) and language-specific extensions.
///
/// The `Ord` derivation uses declaration order; that order is the
/// canonical edge-type discriminant for deterministic sorting.
#[derive(
  Debug,
  Clone,
  Copy,
  PartialEq,
  Eq,
  PartialOrd,
  Ord,
  Hash,
  Serialize,
  Deserialize,
)]
pub enum EdgeType {
  // Universal core
  ContainsMember,
  ContainsLocal,
  ContainsField,
  Calls,
  References,
  Implements,
  ProxyOf,

  // Solidity-specific extensions
  WritesState,
  UsingFor,
  ModifierApplied,
  ErrorThrown,
  EventEmitted,

  // Rust-specific extensions (planned, per spec). Inert until the Rust
  // edge extractor produces them — calibration against a real Rust-bearing
  // audit will refine the default weights below. Variant order is the
  // canonical edge-type discriminant for deterministic sorting and must
  // not be reshuffled.
  Derives,
  ReExports,
  MutatesField,
}

impl EdgeType {
  /// The starting weight for this edge type per the spec's edge tables.
  /// Calibration via the comparison harness picks the final values; these
  /// are the language-agnostic defaults.
  pub fn default_weight(self) -> f32 {
    match self {
      EdgeType::ContainsMember => 1.0,
      EdgeType::ContainsLocal => 1.2,
      EdgeType::ContainsField => 1.0,
      EdgeType::Calls => 0.7,
      EdgeType::References => 0.5,
      EdgeType::Implements => 0.8,
      EdgeType::ProxyOf => 0.9,
      EdgeType::WritesState => 0.7,
      EdgeType::UsingFor => 0.5,
      EdgeType::ModifierApplied => 0.5,
      EdgeType::ErrorThrown => 0.4,
      EdgeType::EventEmitted => 0.4,
      EdgeType::Derives => 0.6,
      EdgeType::ReExports => 0.4,
      EdgeType::MutatesField => 0.7,
    }
  }

  /// Whether the relationship is directed or undirected, per the spec
  /// edge tables. The graph storage layer is purely directed; extractors
  /// emit both directions for undirected edges.
  pub fn directionality(self) -> Direction {
    match self {
      EdgeType::ContainsMember
      | EdgeType::ContainsLocal
      | EdgeType::ContainsField
      | EdgeType::Implements
      | EdgeType::UsingFor
      | EdgeType::ModifierApplied
      | EdgeType::Derives => Direction::Undirected,

      EdgeType::Calls
      | EdgeType::References
      | EdgeType::ProxyOf
      | EdgeType::WritesState
      | EdgeType::ErrorThrown
      | EdgeType::EventEmitted
      | EdgeType::ReExports
      | EdgeType::MutatesField => Direction::Directed,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Every variant the spec defines today. Tests iterate this so a new
  /// variant added without updating defaults will surface as an
  /// out-of-table assertion failure rather than silently using zero.
  const ALL_EDGE_TYPES: &[EdgeType] = &[
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
    EdgeType::Derives,
    EdgeType::ReExports,
    EdgeType::MutatesField,
  ];

  #[test]
  fn default_weight_matches_spec_universal_core() {
    assert_eq!(EdgeType::ContainsMember.default_weight(), 1.0);
    assert_eq!(EdgeType::ContainsLocal.default_weight(), 1.2);
    assert_eq!(EdgeType::ContainsField.default_weight(), 1.0);
    assert_eq!(EdgeType::Calls.default_weight(), 0.7);
    assert_eq!(EdgeType::References.default_weight(), 0.5);
    assert_eq!(EdgeType::Implements.default_weight(), 0.8);
    assert_eq!(EdgeType::ProxyOf.default_weight(), 0.9);
  }

  #[test]
  fn default_weight_matches_spec_solidity_extensions() {
    assert_eq!(EdgeType::WritesState.default_weight(), 0.7);
    assert_eq!(EdgeType::UsingFor.default_weight(), 0.5);
    assert_eq!(EdgeType::ModifierApplied.default_weight(), 0.5);
    assert_eq!(EdgeType::ErrorThrown.default_weight(), 0.4);
    assert_eq!(EdgeType::EventEmitted.default_weight(), 0.4);
  }

  #[test]
  fn default_weight_matches_spec_rust_extensions() {
    // Per the spec's "Rust-specific extensions (planned)" table.
    assert_eq!(EdgeType::Derives.default_weight(), 0.6);
    assert_eq!(EdgeType::ReExports.default_weight(), 0.4);
    assert_eq!(EdgeType::MutatesField.default_weight(), 0.7);
  }

  #[test]
  fn all_edge_types_have_positive_weight() {
    for et in ALL_EDGE_TYPES {
      assert!(
        et.default_weight() > 0.0,
        "{:?} must have positive weight",
        et
      );
    }
  }

  #[test]
  fn directionality_matches_spec() {
    let undirected = [
      EdgeType::ContainsMember,
      EdgeType::ContainsLocal,
      EdgeType::ContainsField,
      EdgeType::Implements,
      EdgeType::UsingFor,
      EdgeType::ModifierApplied,
      EdgeType::Derives,
    ];
    let directed = [
      EdgeType::Calls,
      EdgeType::References,
      EdgeType::ProxyOf,
      EdgeType::WritesState,
      EdgeType::ErrorThrown,
      EdgeType::EventEmitted,
      EdgeType::ReExports,
      EdgeType::MutatesField,
    ];
    for et in undirected {
      assert_eq!(
        et.directionality(),
        Direction::Undirected,
        "{:?} must be undirected",
        et
      );
    }
    for et in directed {
      assert_eq!(
        et.directionality(),
        Direction::Directed,
        "{:?} must be directed",
        et
      );
    }
    // Sanity: sum of partitions equals total — we have not silently
    // dropped a variant from the directionality classification.
    assert_eq!(undirected.len() + directed.len(), ALL_EDGE_TYPES.len());
  }

  #[test]
  fn ord_uses_declaration_order() {
    // The Ord derivation underpins finalize() sorting; the order is
    // the declared variant order.
    assert!(EdgeType::ContainsMember < EdgeType::ContainsLocal);
    assert!(EdgeType::ContainsLocal < EdgeType::Calls);
    assert!(EdgeType::Implements < EdgeType::ProxyOf);
    assert!(EdgeType::ProxyOf < EdgeType::WritesState);
    assert!(EdgeType::ErrorThrown < EdgeType::EventEmitted);
    // Rust extensions sort after every Solidity extension — the order
    // pins the discriminant for adjacency-list sorting.
    assert!(EdgeType::EventEmitted < EdgeType::Derives);
    assert!(EdgeType::Derives < EdgeType::ReExports);
    assert!(EdgeType::ReExports < EdgeType::MutatesField);
  }

  #[test]
  fn serde_round_trip() {
    for et in ALL_EDGE_TYPES {
      let json = serde_json::to_string(et).unwrap();
      let back: EdgeType = serde_json::from_str(&json).unwrap();
      assert_eq!(*et, back);
    }
  }
}
