use crate::domain::AuditData;

use super::graph::ResolutionGraph;
use super::rust_extractor::RustExtractor;
use super::solidity_extractor::SolidityExtractor;

/// Per-language edge extractor. Each language analyzer registers an
/// implementation; `build` dispatches to all that apply to the audit.
///
/// Extractors are pure reads of `AuditData` — they must not mutate
/// analyzer state. They contribute edges via `graph.add_edge`; the
/// builder finalizes the graph once all extractors have run.
pub trait Extractor {
  fn applies_to(&self, audit_data: &AuditData) -> bool;
  fn extract(&self, audit_data: &AuditData, graph: &mut ResolutionGraph);
}

/// Build the resolution graph for an audit. Iterates the registered
/// extractors, runs the ones that apply, and returns the finalized
/// graph.
pub fn build(audit_data: &AuditData) -> ResolutionGraph {
  let mut graph = ResolutionGraph::new();
  for extractor in extractors() {
    if extractor.applies_to(audit_data) {
      extractor.extract(audit_data, &mut graph);
    }
  }
  graph.finalize();
  graph
}

fn extractors() -> Vec<Box<dyn Extractor>> {
  // Register one entry per language. Order matters only insofar as
  // extractors share a determinism contract — each emits its own edge
  // set against its own topic subset, so they cannot conflict.
  vec![Box::new(SolidityExtractor), Box::new(RustExtractor)]
}
