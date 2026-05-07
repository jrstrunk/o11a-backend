//! Project-wide call DAG with SCC collapse and affinity batching.
//!
//! Used by behavior extraction (pipeline step 3) and functional purpose /
//! placement rationale generation (pipeline step 5). Both pass functions
//! to the LLM in batches that respect dependency ordering: a callee's
//! behaviors are extracted (and available as context) before any of its
//! callers run, so each batch can render `called_function_behaviors` for
//! its members.
//!
//! See `crates/o11a-analyze/docs/build-plans/pipeline-dag.md` for the
//! design and pivotal decisions.

use crate::domain::{
  AuditData, FunctionModProperties, NamedTopicKind, TopicMetadata, topic,
};

use std::collections::{BTreeSet, HashMap, HashSet};

/// Maximum number of functions in a single LLM batch. SCCs larger than
/// this go in a single oversized batch — the LLM handles them but
/// affinity grouping does not split them.
pub const MAX_BATCH_SIZE: usize = 5;

/// One unit of work: a list of in-scope function/modifier topics that
/// will be sent to the LLM together. Members of the same batch share the
/// same DAG layer and are grouped by callee affinity. SCCs collapse into
/// one batch even if larger than `MAX_BATCH_SIZE`.
pub struct Batch {
  pub members: Vec<topic::Topic>,
}

/// Build all batches in dependency order. Earlier batches contain
/// callees; later batches contain callers. Every member of every batch is
/// an in-scope function or modifier whose behaviors / functional
/// properties can be generated in parallel within the batch.
pub fn build_batches(audit_data: &AuditData) -> Vec<Batch> {
  let in_scope: BTreeSet<topic::Topic> = collect_in_scope_callees(audit_data);

  // Edges: callee → caller (callee must be processed first).
  let edges = build_call_edges(audit_data, &in_scope);

  // Group SCCs (cycles between functions/modifiers — rare in Solidity).
  let sccs = tarjan_scc(&in_scope, &edges);

  // Topological order over the SCC condensation: leaves first.
  let layers = layer_order(&sccs, &edges);

  // Within each layer, batch by callee affinity.
  let mut batches = Vec::new();
  for layer in layers {
    for batch_members in affinity_batch(layer, &edges) {
      batches.push(Batch {
        members: batch_members,
      });
    }
  }
  batches
}

/// Collect every in-scope function and modifier topic. These are the
/// nodes of the DAG.
fn collect_in_scope_callees(audit_data: &AuditData) -> BTreeSet<topic::Topic> {
  let mut out = BTreeSet::new();
  for (topic, metadata) in &audit_data.topic_metadata {
    let TopicMetadata::NamedTopic { kind, scope, .. } = metadata else {
      continue;
    };
    let is_callable =
      matches!(kind, NamedTopicKind::Function(_) | NamedTopicKind::Modifier);
    if !is_callable {
      continue;
    }
    // Skip transitive proxies — their canonical implementation will
    // appear under its own topic.
    if metadata.transitive_topic().is_some() {
      continue;
    }
    if !scope_is_in_scope(scope, audit_data) {
      continue;
    }
    out.insert(*topic);
  }
  out
}

fn scope_is_in_scope(
  scope: &crate::domain::Scope,
  audit_data: &AuditData,
) -> bool {
  use crate::domain::Scope;
  let container = match scope {
    Scope::Container { container } => container,
    Scope::Component { container, .. } => container,
    Scope::Member { container, .. } => container,
    Scope::ContainingBlock { container, .. } => container,
    Scope::Global => return false,
  };
  audit_data.in_scope_files.contains(container)
}

/// Build callee→caller edges by walking each in-scope function's
/// `FunctionModProperties.calls` list. Out-of-scope callees are dropped:
/// they have no behaviors and thus contribute no ordering constraint.
/// Transitive callees (interface stubs) collapse to their canonical
/// implementation so the topological sort never produces a layer for the
/// stub.
fn build_call_edges(
  audit_data: &AuditData,
  in_scope: &BTreeSet<topic::Topic>,
) -> HashMap<topic::Topic, Vec<topic::Topic>> {
  let mut edges: HashMap<topic::Topic, Vec<topic::Topic>> = HashMap::new();

  for caller in in_scope {
    let Some(props) = audit_data.function_properties.get(caller) else {
      continue;
    };
    let calls = match props {
      FunctionModProperties::FunctionProperties { calls, .. }
      | FunctionModProperties::ModifierProperties { calls, .. } => calls,
    };
    for callee in calls {
      let resolved = audit_data
        .topic_metadata
        .get(callee)
        .and_then(|m| m.transitive_topic())
        .copied()
        .unwrap_or(*callee);
      if !in_scope.contains(&resolved) {
        continue;
      }
      // Skip self-loops; they're handled implicitly by SCC collapse but
      // adding a self-edge bloats the affinity weight for no benefit.
      if resolved == *caller {
        continue;
      }
      edges.entry(resolved).or_default().push(*caller);
    }
  }

  // Dedupe each adjacency list — multiple call sites to the same callee
  // should produce one edge.
  for adj in edges.values_mut() {
    adj.sort();
    adj.dedup();
  }
  edges
}

/// Tarjan's strongly-connected-components algorithm. Each returned vec
/// is one SCC. SCCs are returned in reverse topological order
/// (callees-of-condensation first), matching Tarjan's natural output.
fn tarjan_scc(
  nodes: &BTreeSet<topic::Topic>,
  edges: &HashMap<topic::Topic, Vec<topic::Topic>>,
) -> Vec<Vec<topic::Topic>> {
  // State for the algorithm.
  let mut index_of: HashMap<topic::Topic, usize> = HashMap::new();
  let mut lowlink: HashMap<topic::Topic, usize> = HashMap::new();
  let mut on_stack: HashSet<topic::Topic> = HashSet::new();
  let mut stack: Vec<topic::Topic> = Vec::new();
  let mut next_index: usize = 0;
  let mut sccs: Vec<Vec<topic::Topic>> = Vec::new();

  // Iterative DFS to avoid stack overflow on very deep call chains.
  // Frame: (node, child iterator index, called-from-parent topic if any).
  enum Action {
    Enter(topic::Topic),
    Resume(topic::Topic, usize),
  }

  for &start in nodes {
    if index_of.contains_key(&start) {
      continue;
    }
    let mut work: Vec<Action> = vec![Action::Enter(start)];
    while let Some(action) = work.pop() {
      match action {
        Action::Enter(v) => {
          index_of.insert(v, next_index);
          lowlink.insert(v, next_index);
          next_index += 1;
          stack.push(v);
          on_stack.insert(v);
          work.push(Action::Resume(v, 0));
        }
        Action::Resume(v, i) => {
          let children = edges.get(&v).map(|c| c.as_slice()).unwrap_or(&[]);
          let mut next_i = i;
          let mut descended = false;
          while next_i < children.len() {
            let w = children[next_i];
            next_i += 1;
            if !index_of.contains_key(&w) {
              // Continue v's iteration after w finishes.
              work.push(Action::Resume(v, next_i));
              work.push(Action::Enter(w));
              descended = true;
              break;
            } else if on_stack.contains(&w) {
              let w_idx = *index_of.get(&w).unwrap();
              let v_low = *lowlink.get(&v).unwrap();
              lowlink.insert(v, v_low.min(w_idx));
            }
          }
          if descended {
            continue;
          }
          // Post-process v: propagate lowlink from completed children.
          for &w in children {
            if on_stack.contains(&w) {
              let w_low = *lowlink.get(&w).unwrap();
              let v_low = *lowlink.get(&v).unwrap();
              lowlink.insert(v, v_low.min(w_low));
            }
          }
          if lowlink.get(&v) == index_of.get(&v) {
            let mut component = Vec::new();
            loop {
              let w = stack.pop().expect("stack underflow");
              on_stack.remove(&w);
              component.push(w);
              if w == v {
                break;
              }
            }
            sccs.push(component);
          }
        }
      }
    }
  }
  sccs
}

/// Topological order of SCCs, leaves first. Returned as a list of layers
/// where every SCC in layer N depends only on SCCs in earlier layers.
/// SCCs collapse to a single node — within an SCC, all members are in
/// the same batch.
fn layer_order(
  sccs: &[Vec<topic::Topic>],
  edges: &HashMap<topic::Topic, Vec<topic::Topic>>,
) -> Vec<Vec<Vec<topic::Topic>>> {
  // Map each topic to its SCC index.
  let mut scc_of: HashMap<topic::Topic, usize> = HashMap::new();
  for (i, scc) in sccs.iter().enumerate() {
    for &t in scc {
      scc_of.insert(t, i);
    }
  }

  // Condensation graph: scc_index → set of dependent scc_indices.
  let mut dep_indegree: Vec<usize> = vec![0; sccs.len()];
  let mut condensed: Vec<HashSet<usize>> = vec![HashSet::new(); sccs.len()];
  for (&from, tos) in edges {
    let Some(&from_scc) = scc_of.get(&from) else {
      continue;
    };
    for &to in tos {
      let Some(&to_scc) = scc_of.get(&to) else {
        continue;
      };
      if from_scc == to_scc {
        continue;
      }
      if condensed[from_scc].insert(to_scc) {
        dep_indegree[to_scc] += 1;
      }
    }
  }

  // Kahn-style layering: each layer is the set of SCCs with current
  // indegree 0. After emitting a layer, decrement indegree on its
  // dependents and emit the next.
  let mut remaining: BTreeSet<usize> = (0..sccs.len()).collect();
  let mut layers: Vec<Vec<Vec<topic::Topic>>> = Vec::new();
  while !remaining.is_empty() {
    let ready: Vec<usize> = remaining
      .iter()
      .copied()
      .filter(|&i| dep_indegree[i] == 0)
      .collect();
    if ready.is_empty() {
      // Shouldn't happen — Tarjan guarantees a DAG over SCCs. Defensive
      // exit so a degenerate dataset doesn't infinite-loop the pipeline.
      break;
    }
    let mut layer = Vec::with_capacity(ready.len());
    for i in &ready {
      layer.push(sccs[*i].clone());
      remaining.remove(i);
      for &dep in &condensed[*i] {
        dep_indegree[dep] -= 1;
      }
    }
    layers.push(layer);
  }
  layers
}

/// Group SCCs within one layer into batches of ≤ `MAX_BATCH_SIZE` by
/// shared-callee affinity. SCCs that are themselves larger than
/// `MAX_BATCH_SIZE` go in their own oversized batch — splitting them
/// would defeat the purpose of grouping a cycle together.
///
/// Affinity is symmetric: two SCCs share affinity equal to the count of
/// distinct callees that any member of one SCC also calls from any
/// member of the other. Greedy grow: seed with the highest-affinity pair
/// available, then add the SCC with the highest affinity to the current
/// batch until size hits the cap.
fn affinity_batch(
  layer: Vec<Vec<topic::Topic>>,
  edges: &HashMap<topic::Topic, Vec<topic::Topic>>,
) -> Vec<Vec<topic::Topic>> {
  // Reverse-lookup callees per SCC. The DAG edges go callee → caller, so
  // a member's callees are the keys whose adjacency contains that
  // member. Build once.
  let mut callees_of_scc: Vec<BTreeSet<topic::Topic>> =
    Vec::with_capacity(layer.len());
  for scc in &layer {
    let mut callees: BTreeSet<topic::Topic> = BTreeSet::new();
    for member in scc {
      for (callee, callers) in edges {
        if callers.contains(member) {
          callees.insert(*callee);
        }
      }
    }
    callees_of_scc.push(callees);
  }

  let mut remaining: BTreeSet<usize> = (0..layer.len()).collect();
  let mut batches: Vec<Vec<topic::Topic>> = Vec::new();

  while !remaining.is_empty() {
    let mut current: Vec<usize> = Vec::new();
    let mut current_size: usize = 0;

    // Seed: highest-affinity pair within `remaining`. Falls back to the
    // smallest remaining SCC if no pair shares any callee.
    let seed = pick_seed(&remaining, &callees_of_scc, &layer);
    let seed_size = layer[seed].len();
    current.push(seed);
    current_size += seed_size;
    remaining.remove(&seed);

    if seed_size > MAX_BATCH_SIZE {
      // Oversized SCC — emit alone; continue the outer loop.
      batches.push(flatten_sccs(&current, &layer));
      continue;
    }

    while current_size < MAX_BATCH_SIZE {
      let mut best: Option<(usize, usize)> = None; // (scc_idx, affinity)
      for &candidate in &remaining {
        let candidate_size = layer[candidate].len();
        if current_size + candidate_size > MAX_BATCH_SIZE {
          continue;
        }
        let affinity = current
          .iter()
          .map(|&c| {
            callees_of_scc[c]
              .intersection(&callees_of_scc[candidate])
              .count()
          })
          .sum();
        match best {
          None => best = Some((candidate, affinity)),
          Some((_, a)) if affinity > a => best = Some((candidate, affinity)),
          _ => {}
        }
      }
      let Some((next, _)) = best else {
        break;
      };
      current.push(next);
      current_size += layer[next].len();
      remaining.remove(&next);
    }

    batches.push(flatten_sccs(&current, &layer));
  }
  batches
}

fn pick_seed(
  remaining: &BTreeSet<usize>,
  callees_of_scc: &[BTreeSet<topic::Topic>],
  layer: &[Vec<topic::Topic>],
) -> usize {
  // Highest-affinity pair: search every (a, b) in remaining. O(n²) is
  // fine for layer sizes typical in Solidity audits (tens at most).
  let mut best_pair: Option<(usize, usize, usize)> = None;
  for &a in remaining {
    for &b in remaining {
      if b <= a {
        continue;
      }
      let aff = callees_of_scc[a].intersection(&callees_of_scc[b]).count();
      if aff == 0 {
        continue;
      }
      // Prefer higher affinity, then lower combined size as tiebreaker.
      let combined = layer[a].len() + layer[b].len();
      let candidate = (a, b, aff);
      match best_pair {
        None => best_pair = Some(candidate),
        Some((pa, pb, paff)) => {
          let prev_combined = layer[pa].len() + layer[pb].len();
          if aff > paff || (aff == paff && combined < prev_combined) {
            best_pair = Some(candidate);
          }
        }
      }
    }
  }
  if let Some((a, _, _)) = best_pair {
    return a;
  }
  // No shared affinity in this layer — fall back to the smallest SCC so
  // oversized cycles don't crowd a batch with only one neighbor.
  *remaining
    .iter()
    .min_by_key(|&&i| layer[i].len())
    .expect("remaining is non-empty")
}

fn flatten_sccs(
  scc_indices: &[usize],
  layer: &[Vec<topic::Topic>],
) -> Vec<topic::Topic> {
  let mut out = Vec::new();
  for &i in scc_indices {
    out.extend(layer[i].iter().copied());
  }
  out
}

/// Lookup helper: collect each batch member's callees (transitive-resolved
/// to canonical topics, including out-of-scope ones) for use when
/// rendering `called_function_behaviors`. Callers use this to attach
/// callee context to each function in the batch.
pub fn callees_of(
  member: &topic::Topic,
  audit_data: &AuditData,
) -> Vec<topic::Topic> {
  let Some(props) = audit_data.function_properties.get(member) else {
    return Vec::new();
  };
  let calls = match props {
    FunctionModProperties::FunctionProperties { calls, .. }
    | FunctionModProperties::ModifierProperties { calls, .. } => calls,
  };
  let mut out: Vec<topic::Topic> = calls
    .iter()
    .map(|c| {
      audit_data
        .topic_metadata
        .get(c)
        .and_then(|m| m.transitive_topic())
        .copied()
        .unwrap_or(*c)
    })
    .collect();
  out.sort();
  out.dedup();
  out
}

/// Map a member topic to its prior behaviors (B-topic descriptions).
/// Returns an empty vec for members not yet processed or with no
/// behaviors. Used by render functions to populate
/// `called_function_behaviors`.
pub fn behaviors_of(
  member: &topic::Topic,
  audit_data: &AuditData,
) -> Vec<String> {
  let Some(beh_topics) = audit_data.member_behaviors.get(member) else {
    return Vec::new();
  };
  let mut out = Vec::new();
  for bt in beh_topics {
    if let Some(TopicMetadata::BehaviorTopic { description, .. }) =
      audit_data.topic_metadata.get(bt)
    {
      out.push(description.clone());
    }
  }
  out
}

/// Look up the display name of a callable topic (function/modifier) for
/// rendering. Falls back to the topic ID if the metadata is unavailable.
pub fn callable_name(topic: &topic::Topic, audit_data: &AuditData) -> String {
  audit_data
    .topic_metadata
    .get(topic)
    .and_then(|m| m.name())
    .map(|s| s.to_string())
    .unwrap_or_else(|| topic.id())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::domain::topic;
  use std::collections::BTreeSet;

  fn t(id: i32) -> topic::Topic {
    topic::new_node_topic(&id)
  }

  #[test]
  fn tarjan_handles_simple_chain() {
    let nodes: BTreeSet<topic::Topic> =
      [t(1), t(2), t(3)].into_iter().collect();
    let mut edges: HashMap<topic::Topic, Vec<topic::Topic>> = HashMap::new();
    edges.insert(t(1), vec![t(2)]); // 1 → 2
    edges.insert(t(2), vec![t(3)]); // 2 → 3
    let sccs = tarjan_scc(&nodes, &edges);
    assert_eq!(sccs.len(), 3);
    for scc in &sccs {
      assert_eq!(scc.len(), 1);
    }
  }

  #[test]
  fn tarjan_collapses_cycle() {
    let nodes: BTreeSet<topic::Topic> = [t(1), t(2)].into_iter().collect();
    let mut edges: HashMap<topic::Topic, Vec<topic::Topic>> = HashMap::new();
    edges.insert(t(1), vec![t(2)]);
    edges.insert(t(2), vec![t(1)]);
    let sccs = tarjan_scc(&nodes, &edges);
    assert_eq!(sccs.len(), 1);
    assert_eq!(sccs[0].len(), 2);
  }

  #[test]
  fn layer_order_emits_callees_first() {
    // 1 → 2 → 3 (callee → caller). Layer 0 should be {1}, layer 1
    // should be {2}, layer 2 should be {3}.
    let mut edges: HashMap<topic::Topic, Vec<topic::Topic>> = HashMap::new();
    edges.insert(t(1), vec![t(2)]);
    edges.insert(t(2), vec![t(3)]);
    let sccs = vec![vec![t(1)], vec![t(2)], vec![t(3)]];
    let layers = layer_order(&sccs, &edges);
    assert_eq!(layers.len(), 3);
    assert_eq!(layers[0][0], vec![t(1)]);
    assert_eq!(layers[1][0], vec![t(2)]);
    assert_eq!(layers[2][0], vec![t(3)]);
  }

  #[test]
  fn affinity_batches_share_callees() {
    // Functions a, b, c each call shared callee Z. They should batch
    // together up to the cap.
    let layer = vec![vec![t(10)], vec![t(20)], vec![t(30)]];
    let mut edges: HashMap<topic::Topic, Vec<topic::Topic>> = HashMap::new();
    // callee Z (t(99)) → callers a, b, c
    edges.insert(t(99), vec![t(10), t(20), t(30)]);
    let batches = affinity_batch(layer, &edges);
    assert_eq!(batches.len(), 1, "all three share callee Z, one batch");
    assert_eq!(batches[0].len(), 3);
  }

  #[test]
  fn affinity_respects_max_batch_size() {
    // Six callers all share one callee — should split into batches of
    // ≤ MAX_BATCH_SIZE.
    let layer: Vec<Vec<topic::Topic>> = (10..16).map(|i| vec![t(i)]).collect();
    let mut edges: HashMap<topic::Topic, Vec<topic::Topic>> = HashMap::new();
    edges.insert(t(99), (10..16).map(t).collect());
    let batches = affinity_batch(layer, &edges);
    let total: usize = batches.iter().map(|b| b.len()).sum();
    assert_eq!(total, 6);
    for b in &batches {
      assert!(b.len() <= MAX_BATCH_SIZE);
    }
  }

  #[test]
  fn oversized_scc_emits_alone() {
    // SCC of 7 (larger than MAX_BATCH_SIZE) should emit as one batch.
    let big_scc: Vec<topic::Topic> = (10..17).map(t).collect();
    let layer = vec![big_scc.clone()];
    let edges: HashMap<topic::Topic, Vec<topic::Topic>> = HashMap::new();
    let batches = affinity_batch(layer, &edges);
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].len(), 7);
  }

  #[test]
  fn tarjan_handles_empty_input() {
    let nodes: BTreeSet<topic::Topic> = BTreeSet::new();
    let edges: HashMap<topic::Topic, Vec<topic::Topic>> = HashMap::new();
    let sccs = tarjan_scc(&nodes, &edges);
    assert!(sccs.is_empty());
  }

  #[test]
  fn tarjan_handles_isolated_nodes() {
    // Three nodes with no edges between them — three singleton SCCs.
    let nodes: BTreeSet<topic::Topic> =
      [t(1), t(2), t(3)].into_iter().collect();
    let edges: HashMap<topic::Topic, Vec<topic::Topic>> = HashMap::new();
    let sccs = tarjan_scc(&nodes, &edges);
    assert_eq!(sccs.len(), 3);
    for scc in &sccs {
      assert_eq!(scc.len(), 1);
    }
  }

  #[test]
  fn tarjan_handles_long_chain() {
    // 1 → 2 → 3 → 4 → 5: five singleton SCCs.
    let nodes: BTreeSet<topic::Topic> = (1..=5).map(t).collect();
    let mut edges: HashMap<topic::Topic, Vec<topic::Topic>> = HashMap::new();
    for i in 1..5 {
      edges.insert(t(i), vec![t(i + 1)]);
    }
    let sccs = tarjan_scc(&nodes, &edges);
    assert_eq!(sccs.len(), 5);
  }

  #[test]
  fn tarjan_handles_three_node_cycle() {
    let nodes: BTreeSet<topic::Topic> =
      [t(1), t(2), t(3)].into_iter().collect();
    let mut edges: HashMap<topic::Topic, Vec<topic::Topic>> = HashMap::new();
    edges.insert(t(1), vec![t(2)]);
    edges.insert(t(2), vec![t(3)]);
    edges.insert(t(3), vec![t(1)]);
    let sccs = tarjan_scc(&nodes, &edges);
    assert_eq!(sccs.len(), 1);
    assert_eq!(sccs[0].len(), 3);
  }

  #[test]
  fn tarjan_handles_disconnected_components_with_internal_cycles() {
    // Two disjoint components: {1↔2} and {3↔4}. Two SCCs, each size 2.
    let nodes: BTreeSet<topic::Topic> =
      [t(1), t(2), t(3), t(4)].into_iter().collect();
    let mut edges: HashMap<topic::Topic, Vec<topic::Topic>> = HashMap::new();
    edges.insert(t(1), vec![t(2)]);
    edges.insert(t(2), vec![t(1)]);
    edges.insert(t(3), vec![t(4)]);
    edges.insert(t(4), vec![t(3)]);
    let sccs = tarjan_scc(&nodes, &edges);
    assert_eq!(sccs.len(), 2);
    for scc in &sccs {
      assert_eq!(scc.len(), 2);
    }
  }

  #[test]
  fn layer_order_handles_diamond() {
    // A → B, A → C, B → D, C → D. Layers: [A], [B, C], [D].
    let mut edges: HashMap<topic::Topic, Vec<topic::Topic>> = HashMap::new();
    edges.insert(t(1), vec![t(2), t(3)]);
    edges.insert(t(2), vec![t(4)]);
    edges.insert(t(3), vec![t(4)]);
    let sccs = vec![vec![t(1)], vec![t(2)], vec![t(3)], vec![t(4)]];
    let layers = layer_order(&sccs, &edges);
    assert_eq!(layers.len(), 3);
    assert_eq!(layers[0].len(), 1);
    assert_eq!(layers[0][0], vec![t(1)]);
    // B and C are both indegree-0 once A is emitted.
    assert_eq!(layers[1].len(), 2);
    assert_eq!(layers[2][0], vec![t(4)]);
  }

  #[test]
  fn layer_order_handles_disconnected_layers() {
    // Two disjoint chains: 1→2 and 10→11. Both fit in two layers each,
    // and indegree-0 nodes from disjoint chains share their layer.
    let mut edges: HashMap<topic::Topic, Vec<topic::Topic>> = HashMap::new();
    edges.insert(t(1), vec![t(2)]);
    edges.insert(t(10), vec![t(11)]);
    let sccs = vec![vec![t(1)], vec![t(2)], vec![t(10)], vec![t(11)]];
    let layers = layer_order(&sccs, &edges);
    assert_eq!(layers.len(), 2);
    // Layer 0: {1, 10}. Layer 1: {2, 11}.
    assert_eq!(layers[0].len(), 2);
    assert_eq!(layers[1].len(), 2);
  }

  #[test]
  fn affinity_batch_with_no_shared_callees_still_fills() {
    // Three isolated functions in the same layer with no shared callees.
    // The batcher fills the batch greedily even at zero affinity so we
    // don't emit one-batch-per-function for unrelated work.
    let layer = vec![vec![t(10)], vec![t(20)], vec![t(30)]];
    let edges: HashMap<topic::Topic, Vec<topic::Topic>> = HashMap::new();
    let batches = affinity_batch(layer, &edges);
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].len(), 3);
  }

  #[test]
  fn affinity_batch_handles_empty_layer() {
    let layer: Vec<Vec<topic::Topic>> = vec![];
    let edges: HashMap<topic::Topic, Vec<topic::Topic>> = HashMap::new();
    let batches = affinity_batch(layer, &edges);
    assert!(batches.is_empty());
  }

  #[test]
  fn affinity_batch_does_not_split_oversized_scc() {
    // Mixed: one normal SCC and one oversized SCC in the same layer.
    // The oversized one must emit alone; the normal one batches by
    // itself (or with anything else that fits).
    let layer = vec![vec![t(1)], (10..17).map(t).collect()];
    let edges: HashMap<topic::Topic, Vec<topic::Topic>> = HashMap::new();
    let batches = affinity_batch(layer, &edges);
    let total: usize = batches.iter().map(|b| b.len()).sum();
    assert_eq!(total, 8);
    assert!(
      batches.iter().any(|b| b.len() == 7),
      "oversized SCC should emit as a single batch of 7"
    );
  }
}
