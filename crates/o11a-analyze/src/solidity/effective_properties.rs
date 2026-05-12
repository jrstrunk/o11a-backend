//! Transitive side-effect propagation over the call graph.
//!
//! Computes the transitive union of reverts, state mutations, state
//! reads, and events emitted for every function/modifier in
//! `audit_data.function_properties`. Two propagation graphs are used:
//!
//! - **Revert propagation graph** — the call graph minus try-call edges.
//!   Try/catch absorbs reverts from its external call, so those edges
//!   do not propagate revert effects.
//! - **Full call graph** — all call edges, try-wrapped or not. State
//!   mutations, reads, and event emissions from a successful callee
//!   persist regardless of whether the call was try-wrapped; the
//!   auditor view tracks possibility, not outcome.
//!
//! The two graphs may legitimately have different SCC structures —
//! see the `asymmetric_try_cycle_*` tests for the canonical case.
//!
//! Each effect kind is folded bottom-up: for an SCC, the union is
//! every member's direct entries (lifted with `origin = the member`)
//! plus the already-computed effective sets of outside-SCC callees,
//! then deduped on a per-effect key. Within an SCC, inside-SCC edges
//! contribute nothing — every member's direct entries are already in
//! the union — so the inside-SCC skip is correctness, not optimization.

use o11a_core::domain::{
  EffectiveRevert, EffectiveTopic, FunctionModProperties, TopicMetadata, topic,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// Output of [`compute_transitive_effects`]. Four maps, one per
/// effect kind, each keyed by the function/modifier topic. Empty
/// entries for topics with no effective set are equivalent to a
/// missing key — consumers use `.get(...).cloned().unwrap_or_default()`.
pub struct TransitiveEffects {
  pub reverts: HashMap<topic::Topic, Vec<EffectiveRevert>>,
  pub mutations: HashMap<topic::Topic, Vec<EffectiveTopic>>,
  pub reads: HashMap<topic::Topic, Vec<EffectiveTopic>>,
  pub events_emitted: HashMap<topic::Topic, Vec<EffectiveTopic>>,
}

/// Compute the transitive sets for every function/modifier in
/// `function_properties`. Returns four maps that the caller writes
/// back onto the corresponding `effective_*` fields.
///
/// Reverts propagate over the non-try call graph; mutations / reads /
/// events propagate over the full call graph. Proxies are resolved
/// through `topic_metadata.transitive_topic()`. Out-of-scope callees
/// (whose resolved topic is absent from `function_properties`) are
/// dropped — the fold cannot reason about effects of code it doesn't
/// see, matching the existing call-graph convention in
/// `function_dag::build_call_edges`.
pub fn compute_transitive_effects(
  function_properties: &BTreeMap<topic::Topic, FunctionModProperties>,
  topic_metadata: &BTreeMap<topic::Topic, TopicMetadata>,
) -> TransitiveEffects {
  let revert_adj = build_call_adjacency(
    function_properties,
    topic_metadata,
    /* include_try_edges = */ false,
  );
  let full_adj = build_call_adjacency(
    function_properties,
    topic_metadata,
    /* include_try_edges = */ true,
  );

  let revert_nodes: BTreeSet<topic::Topic> =
    revert_adj.keys().copied().collect();
  let full_nodes: BTreeSet<topic::Topic> = full_adj.keys().copied().collect();

  let revert_sccs = tarjan_scc(&revert_nodes, &revert_adj);
  let revert_scc_id = build_scc_index(&revert_sccs);
  let full_sccs = tarjan_scc(&full_nodes, &full_adj);
  let full_scc_id = build_scc_index(&full_sccs);

  // Reverts: non-try propagation graph. Dedup by (origin, kind,
  // error_topic.or(revert.topic)) so two paths to the same custom
  // error collapse but distinct bare requires from the same origin
  // remain separate (distinguished by the require/revert statement
  // node).
  let reverts = fold_per_scc(
    function_properties,
    &revert_sccs,
    &revert_scc_id,
    &revert_adj,
    |props| match props {
      FunctionModProperties::FunctionProperties { reverts, .. }
      | FunctionModProperties::ModifierProperties { reverts, .. } => {
        reverts.as_slice()
      }
    },
    |r, origin| EffectiveRevert {
      revert: r.clone(),
      origin,
    },
    |e| {
      (
        e.origin,
        e.revert.kind,
        e.revert.error_topic.unwrap_or(e.revert.topic),
      )
    },
  );

  // State writes: full call graph. Try-call edges propagate writes;
  // the caller view tracks possibility, not guarantee. Dedup by
  // (origin, topic).
  let mutations = fold_per_scc(
    function_properties,
    &full_sccs,
    &full_scc_id,
    &full_adj,
    |props| match props {
      FunctionModProperties::FunctionProperties { mutations, .. }
      | FunctionModProperties::ModifierProperties { mutations, .. } => {
        mutations.as_slice()
      }
    },
    |t, origin| EffectiveTopic { topic: *t, origin },
    |e| (e.origin, e.topic),
  );

  // State reads: same propagation graph as mutations.
  let reads = fold_per_scc(
    function_properties,
    &full_sccs,
    &full_scc_id,
    &full_adj,
    |props| match props {
      FunctionModProperties::FunctionProperties { reads, .. }
      | FunctionModProperties::ModifierProperties { reads, .. } => {
        reads.as_slice()
      }
    },
    |t, origin| EffectiveTopic { topic: *t, origin },
    |e| (e.origin, e.topic),
  );

  // Events emitted: same propagation graph as mutations.
  let events_emitted = fold_per_scc(
    function_properties,
    &full_sccs,
    &full_scc_id,
    &full_adj,
    |props| match props {
      FunctionModProperties::FunctionProperties { events_emitted, .. }
      | FunctionModProperties::ModifierProperties { events_emitted, .. } => {
        events_emitted.as_slice()
      }
    },
    |t, origin| EffectiveTopic { topic: *t, origin },
    |e| (e.origin, e.topic),
  );

  TransitiveEffects {
    reverts,
    mutations,
    reads,
    events_emitted,
  }
}

/// Build adjacency map for the call graph. When `include_try_edges` is
/// false, try-call sites are dropped — that's the revert propagation
/// graph. When true, every call edge is included — the full call graph
/// used for non-revert effects. Callees are resolved through proxies
/// (interface→impl via `transitive_topic`) and filtered to
/// `function_properties` keys; out-of-scope callees are dropped.
fn build_call_adjacency(
  function_properties: &BTreeMap<topic::Topic, FunctionModProperties>,
  topic_metadata: &BTreeMap<topic::Topic, TopicMetadata>,
  include_try_edges: bool,
) -> HashMap<topic::Topic, Vec<topic::Topic>> {
  let mut adj: HashMap<topic::Topic, Vec<topic::Topic>> = HashMap::new();
  for (caller, props) in function_properties {
    let calls = match props {
      FunctionModProperties::FunctionProperties { calls, .. }
      | FunctionModProperties::ModifierProperties { calls, .. } => calls,
    };
    let mut callees: Vec<topic::Topic> = Vec::new();
    for call in calls {
      if !include_try_edges && call.in_try_block {
        continue;
      }
      let resolved = topic_metadata
        .get(&call.callee)
        .and_then(|m| m.transitive_topic())
        .copied()
        .unwrap_or(call.callee);
      if function_properties.contains_key(&resolved) {
        callees.push(resolved);
      }
    }
    callees.sort();
    callees.dedup();
    adj.insert(*caller, callees);
  }
  adj
}

/// Map every topic to the index of its SCC in `sccs`. Topics not in
/// any SCC (which shouldn't happen for our inputs — Tarjan visits
/// every node in `nodes`) won't be in the map; callers that index
/// into this should use `.get(&topic).copied()` and treat absence as
/// "outside any computed SCC."
fn build_scc_index(sccs: &[Vec<topic::Topic>]) -> HashMap<topic::Topic, usize> {
  let mut map = HashMap::new();
  for (idx, scc) in sccs.iter().enumerate() {
    for member in scc {
      map.insert(*member, idx);
    }
  }
  map
}

/// Bottom-up fold over SCCs. Per SCC:
///
/// 1. Every member's direct entries (extracted via `direct`) are
///    lifted to the Effective type with `origin = the member`.
/// 2. For every outgoing edge from any SCC member that lands OUTSIDE
///    this SCC, fold in the callee's already-computed effective set.
///    Inside-SCC edges contribute nothing — every member's direct
///    entries are already in the union via step 1, and re-adding
///    through inside-SCC propagation would be redundant.
/// 3. Dedup using `dedup_key`.
/// 4. Every SCC member receives the same union — the canonical
///    SCC-shares-set property.
///
/// SCCs are visited in the order Tarjan emits them (reverse
/// topological — leaves of the condensation first), so by the time we
/// process an SCC, all its outside-SCC callees have already been
/// folded.
fn fold_per_scc<Direct, Effective, K>(
  function_properties: &BTreeMap<topic::Topic, FunctionModProperties>,
  sccs: &[Vec<topic::Topic>],
  scc_id: &HashMap<topic::Topic, usize>,
  adj: &HashMap<topic::Topic, Vec<topic::Topic>>,
  direct: impl Fn(&FunctionModProperties) -> &[Direct],
  lift: impl Fn(&Direct, topic::Topic) -> Effective,
  dedup_key: impl Fn(&Effective) -> K,
) -> HashMap<topic::Topic, Vec<Effective>>
where
  Effective: Clone,
  K: Ord,
{
  let mut effective: HashMap<topic::Topic, Vec<Effective>> = HashMap::new();
  for (scc_idx, scc) in sccs.iter().enumerate() {
    let mut union: Vec<Effective> = Vec::new();

    // 1. Member-direct entries with origin = member.
    for member in scc {
      let Some(props) = function_properties.get(member) else {
        continue;
      };
      for d in direct(props) {
        union.push(lift(d, *member));
      }
    }

    // 2. Outside-SCC callees fold in their already-computed set.
    for member in scc {
      let Some(callees) = adj.get(member) else {
        continue;
      };
      for callee in callees {
        if scc_id.get(callee).copied() == Some(scc_idx) {
          continue;
        }
        if let Some(callee_eff) = effective.get(callee) {
          union.extend(callee_eff.iter().cloned());
        }
      }
    }

    // 3. Dedup using the per-effect key.
    union.sort_by_key(|e| dedup_key(e));
    union.dedup_by(|a, b| dedup_key(a) == dedup_key(b));

    // 4. Every SCC member receives the union.
    for member in scc {
      effective.insert(*member, union.clone());
    }
  }
  effective
}

/// Tarjan's strongly-connected-components algorithm. Each returned vec
/// is one SCC. SCCs are returned in reverse topological order
/// (callees-of-condensation first), matching Tarjan's natural output.
///
/// Copied verbatim from
/// `crates/o11a-core/src/collaborator/agent/function_dag.rs:152`. The
/// algorithm there operates on the full call graph for batch ordering;
/// here we run it twice, once per propagation graph. If a future
/// generic graph utility lands, both call sites can collapse to it.
fn tarjan_scc(
  nodes: &BTreeSet<topic::Topic>,
  edges: &HashMap<topic::Topic, Vec<topic::Topic>>,
) -> Vec<Vec<topic::Topic>> {
  let mut index_of: HashMap<topic::Topic, usize> = HashMap::new();
  let mut lowlink: HashMap<topic::Topic, usize> = HashMap::new();
  let mut on_stack: HashSet<topic::Topic> = HashSet::new();
  let mut stack: Vec<topic::Topic> = Vec::new();
  let mut next_index: usize = 0;
  let mut sccs: Vec<Vec<topic::Topic>> = Vec::new();

  // Iterative DFS to avoid stack overflow on very deep call chains.
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

#[cfg(test)]
mod tests {
  use super::*;
  use o11a_core::domain::{
    CallInfo, FunctionKind, NamedTopicKind, NamedTopicVisibility,
    RevertConstraintKind, RevertInfo, Scope,
  };

  // ==========================================================================
  // Test fixtures
  //
  // The fold takes two maps (function_properties, topic_metadata) and
  // returns a TransitiveEffects struct. Tests build those maps
  // directly — no AuditData needed. Helpers below provide minimal
  // constructors for the entry kinds the fold reads.
  // ==========================================================================

  /// Function topic for tests, using node-id N.
  fn fn_topic(n: i32) -> topic::Topic {
    topic::new_node_topic(&n)
  }

  /// State variable topic. Same shape as fn_topic since both are
  /// node-topics; the helper exists to make test reads scannable.
  fn var_topic(n: i32) -> topic::Topic {
    topic::new_node_topic(&n)
  }

  /// Event/error topic. Same shape as fn_topic for the same reason.
  fn ev_topic(n: i32) -> topic::Topic {
    topic::new_node_topic(&n)
  }

  /// Minimal NamedTopic metadata, with optional `transitive_topic`
  /// for proxy-resolution tests. Defaults the unused fields to keep
  /// the test bodies readable.
  fn named_topic_meta(
    topic: topic::Topic,
    transitive: Option<topic::Topic>,
  ) -> TopicMetadata {
    TopicMetadata::NamedTopic {
      topic,
      kind: NamedTopicKind::Function(FunctionKind::Function),
      visibility: NamedTopicVisibility::Internal,
      name: "f".to_string(),
      scope: Scope::Global,
      is_mutable: false,
      mutations: Vec::new(),
      ancestors: Vec::new(),
      descendants: Vec::new(),
      relatives: Vec::new(),
      transitive_topic: transitive,
      doc_references: Vec::new(),
    }
  }

  /// Direct call with no try-wrapping.
  fn call(callee: topic::Topic) -> CallInfo {
    CallInfo {
      site: callee, // site identity doesn't matter for the fold
      callee,
      in_try_block: false,
    }
  }

  /// Try-wrapped call — drops out of the revert propagation graph
  /// but stays in the full call graph.
  fn try_call(callee: topic::Topic) -> CallInfo {
    CallInfo {
      site: callee,
      callee,
      in_try_block: true,
    }
  }

  /// Custom-error revert (e.g. `revert MyError(...)`). `statement` is
  /// the require/revert statement's node topic; `error` is the custom
  /// error declaration's topic.
  fn custom_revert(statement: topic::Topic, error: topic::Topic) -> RevertInfo {
    RevertInfo {
      topic: statement,
      kind: RevertConstraintKind::Revert,
      error_topic: Some(error),
    }
  }

  /// Bare `require(cond, "msg")` — no custom error declaration.
  fn require_revert(statement: topic::Topic) -> RevertInfo {
    RevertInfo {
      topic: statement,
      kind: RevertConstraintKind::Require,
      error_topic: None,
    }
  }

  /// Build a FunctionProperties entry with the given direct effects.
  /// Effective fields default to empty (the fold populates them).
  fn fn_props(
    reverts: Vec<RevertInfo>,
    calls: Vec<CallInfo>,
    mutations: Vec<topic::Topic>,
    reads: Vec<topic::Topic>,
    events_emitted: Vec<topic::Topic>,
  ) -> FunctionModProperties {
    FunctionModProperties::FunctionProperties {
      reverts,
      effective_reverts: vec![],
      calls,
      mutations,
      effective_mutations: vec![],
      reads,
      effective_reads: vec![],
      events_emitted,
      effective_events_emitted: vec![],
    }
  }

  /// Build a ModifierProperties entry — same shape as fn_props but
  /// for the other enum variant. Used to verify the fold's match arms
  /// handle both variants symmetrically.
  fn mod_props(
    reverts: Vec<RevertInfo>,
    calls: Vec<CallInfo>,
    mutations: Vec<topic::Topic>,
    reads: Vec<topic::Topic>,
    events_emitted: Vec<topic::Topic>,
  ) -> FunctionModProperties {
    FunctionModProperties::ModifierProperties {
      reverts,
      effective_reverts: vec![],
      calls,
      mutations,
      effective_mutations: vec![],
      reads,
      effective_reads: vec![],
      events_emitted,
      effective_events_emitted: vec![],
    }
  }

  fn empty_metadata() -> BTreeMap<topic::Topic, TopicMetadata> {
    BTreeMap::new()
  }

  // ==========================================================================
  // Revert cases — exercise the non-try propagation graph.
  // ==========================================================================

  #[test]
  fn direct_only_revert() {
    let a = fn_topic(1);
    let stmt = fn_topic(10);
    let err = fn_topic(20);
    let mut props = BTreeMap::new();
    props.insert(
      a,
      fn_props(
        vec![custom_revert(stmt, err)],
        vec![],
        vec![],
        vec![],
        vec![],
      ),
    );

    let effects = compute_transitive_effects(&props, &empty_metadata());
    let a_reverts = &effects.reverts[&a];
    assert_eq!(a_reverts.len(), 1);
    assert_eq!(a_reverts[0].origin, a);
    assert_eq!(a_reverts[0].revert.error_topic, Some(err));
  }

  #[test]
  fn one_hop_no_try_propagates_revert() {
    // A calls B (no try); B raises X. A.effective_reverts ⊇ {X, B}.
    let a = fn_topic(1);
    let b = fn_topic(2);
    let stmt = fn_topic(10);
    let err = fn_topic(20);
    let mut props = BTreeMap::new();
    props.insert(a, fn_props(vec![], vec![call(b)], vec![], vec![], vec![]));
    props.insert(
      b,
      fn_props(
        vec![custom_revert(stmt, err)],
        vec![],
        vec![],
        vec![],
        vec![],
      ),
    );

    let effects = compute_transitive_effects(&props, &empty_metadata());
    let a_reverts = &effects.reverts[&a];
    assert_eq!(a_reverts.len(), 1);
    assert_eq!(a_reverts[0].origin, b);
    assert_eq!(a_reverts[0].revert.error_topic, Some(err));
  }

  #[test]
  fn two_hops_no_try_propagates_revert() {
    // A → B → C; C raises X. A.effective_reverts ⊇ {X, C}.
    let a = fn_topic(1);
    let b = fn_topic(2);
    let c = fn_topic(3);
    let stmt = fn_topic(10);
    let err = fn_topic(20);
    let mut props = BTreeMap::new();
    props.insert(a, fn_props(vec![], vec![call(b)], vec![], vec![], vec![]));
    props.insert(b, fn_props(vec![], vec![call(c)], vec![], vec![], vec![]));
    props.insert(
      c,
      fn_props(
        vec![custom_revert(stmt, err)],
        vec![],
        vec![],
        vec![],
        vec![],
      ),
    );

    let effects = compute_transitive_effects(&props, &empty_metadata());
    let a_reverts = &effects.reverts[&a];
    assert_eq!(a_reverts.len(), 1);
    assert_eq!(a_reverts[0].origin, c);
    assert_eq!(a_reverts[0].revert.error_topic, Some(err));
  }

  #[test]
  fn try_block_excludes_callee_reverts() {
    // A try-calls B; B raises X. A.effective_reverts excludes X.
    let a = fn_topic(1);
    let b = fn_topic(2);
    let stmt_a = fn_topic(11);
    let err_a = fn_topic(21);
    let stmt_b = fn_topic(12);
    let err_b = fn_topic(22);
    let mut props = BTreeMap::new();
    props.insert(
      a,
      fn_props(
        vec![custom_revert(stmt_a, err_a)],
        vec![try_call(b)],
        vec![],
        vec![],
        vec![],
      ),
    );
    props.insert(
      b,
      fn_props(
        vec![custom_revert(stmt_b, err_b)],
        vec![],
        vec![],
        vec![],
        vec![],
      ),
    );

    let effects = compute_transitive_effects(&props, &empty_metadata());
    let a_reverts = &effects.reverts[&a];
    // A's own RA stays; B's RB is absorbed by the try.
    assert_eq!(a_reverts.len(), 1);
    assert_eq!(a_reverts[0].origin, a);
    assert_eq!(a_reverts[0].revert.error_topic, Some(err_a));
  }

  #[test]
  fn symmetric_cycle_no_try_shares_revert_set() {
    // A ↔ B without try; A raises RA, B raises RB. Both share
    // {RA from A, RB from B}.
    let a = fn_topic(1);
    let b = fn_topic(2);
    let err_a = fn_topic(21);
    let err_b = fn_topic(22);
    let mut props = BTreeMap::new();
    props.insert(
      a,
      fn_props(
        vec![custom_revert(fn_topic(11), err_a)],
        vec![call(b)],
        vec![],
        vec![],
        vec![],
      ),
    );
    props.insert(
      b,
      fn_props(
        vec![custom_revert(fn_topic(12), err_b)],
        vec![call(a)],
        vec![],
        vec![],
        vec![],
      ),
    );

    let effects = compute_transitive_effects(&props, &empty_metadata());
    let a_set: Vec<_> = effects.reverts[&a]
      .iter()
      .map(|e| (e.origin, e.revert.error_topic))
      .collect();
    let b_set: Vec<_> = effects.reverts[&b]
      .iter()
      .map(|e| (e.origin, e.revert.error_topic))
      .collect();
    assert_eq!(a_set, b_set, "SCC members must share the effective set");
    assert_eq!(a_set.len(), 2);
    assert!(a_set.contains(&(a, Some(err_a))));
    assert!(a_set.contains(&(b, Some(err_b))));
  }

  #[test]
  fn asymmetric_try_cycle_reverts_are_asymmetric() {
    // The canonical contrast test (revert half). A try-calls B; B
    // calls A without try. A raises RA, B raises RB.
    //
    // Propagation (non-try) graph: only B → A.
    // SCCs: {A}, {B}. A processed first.
    //
    // A.effective_reverts = {(RA, A)} — no outgoing prop edges.
    // B.effective_reverts = {(RB, B), (RA, A)} — B → A propagates.
    let a = fn_topic(1);
    let b = fn_topic(2);
    let err_a = fn_topic(21);
    let err_b = fn_topic(22);
    let mut props = BTreeMap::new();
    props.insert(
      a,
      fn_props(
        vec![custom_revert(fn_topic(11), err_a)],
        vec![try_call(b)],
        vec![],
        vec![],
        vec![],
      ),
    );
    props.insert(
      b,
      fn_props(
        vec![custom_revert(fn_topic(12), err_b)],
        vec![call(a)],
        vec![],
        vec![],
        vec![],
      ),
    );

    let effects = compute_transitive_effects(&props, &empty_metadata());
    let a_reverts = &effects.reverts[&a];
    let b_reverts = &effects.reverts[&b];

    assert_eq!(a_reverts.len(), 1);
    assert_eq!(a_reverts[0].origin, a);
    assert_eq!(a_reverts[0].revert.error_topic, Some(err_a));

    let b_set: Vec<_> = b_reverts
      .iter()
      .map(|e| (e.origin, e.revert.error_topic))
      .collect();
    assert_eq!(b_set.len(), 2);
    assert!(b_set.contains(&(a, Some(err_a))));
    assert!(b_set.contains(&(b, Some(err_b))));
  }

  #[test]
  fn dedup_custom_errors_across_paths() {
    // A calls B and C; both raise the same custom error E.
    // A.effective_reverts has exactly one entry for E. (The two
    // origins differ — both contribute (E, B) and (E, C), distinct
    // entries by origin. So expect 2 entries here, NOT 1; the dedup
    // collapses paths *to the same origin*, not different origins.)
    let a = fn_topic(1);
    let b = fn_topic(2);
    let c = fn_topic(3);
    let err = fn_topic(20);
    let mut props = BTreeMap::new();
    props.insert(
      a,
      fn_props(vec![], vec![call(b), call(c)], vec![], vec![], vec![]),
    );
    props.insert(
      b,
      fn_props(
        vec![custom_revert(fn_topic(11), err)],
        vec![],
        vec![],
        vec![],
        vec![],
      ),
    );
    props.insert(
      c,
      fn_props(
        vec![custom_revert(fn_topic(12), err)],
        vec![],
        vec![],
        vec![],
        vec![],
      ),
    );

    let effects = compute_transitive_effects(&props, &empty_metadata());
    let a_set: Vec<_> = effects.reverts[&a]
      .iter()
      .map(|e| (e.origin, e.revert.error_topic))
      .collect();
    assert_eq!(
      a_set.len(),
      2,
      "different origins for same error stay distinct: {:?}",
      a_set
    );
    assert!(a_set.contains(&(b, Some(err))));
    assert!(a_set.contains(&(c, Some(err))));
  }

  #[test]
  fn dedup_same_origin_same_error_collapses() {
    // A calls B twice (two call sites). B raises E. A's set has
    // exactly one (E, origin=B) entry — the (origin, error) pair is
    // the dedup key.
    let a = fn_topic(1);
    let b = fn_topic(2);
    let err = fn_topic(20);
    let mut props = BTreeMap::new();
    // Two call sites to B; the adjacency builder dedups, so this
    // tests one effect of that dedup. The fold itself would also
    // collapse via dedup_key.
    props.insert(
      a,
      fn_props(
        vec![],
        vec![
          CallInfo {
            site: fn_topic(100),
            callee: b,
            in_try_block: false,
          },
          CallInfo {
            site: fn_topic(101),
            callee: b,
            in_try_block: false,
          },
        ],
        vec![],
        vec![],
        vec![],
      ),
    );
    props.insert(
      b,
      fn_props(
        vec![custom_revert(fn_topic(11), err)],
        vec![],
        vec![],
        vec![],
        vec![],
      ),
    );

    let effects = compute_transitive_effects(&props, &empty_metadata());
    let a_reverts = &effects.reverts[&a];
    assert_eq!(a_reverts.len(), 1);
    assert_eq!(a_reverts[0].origin, b);
    assert_eq!(a_reverts[0].revert.error_topic, Some(err));
  }

  #[test]
  fn distinct_bare_requires_stay_distinct() {
    // A function with two bare requires at different statement nodes.
    // Both appear in its own effective_reverts (no dedup — distinct
    // statement topics are distinct entries).
    let a = fn_topic(1);
    let s1 = fn_topic(11);
    let s2 = fn_topic(12);
    let mut props = BTreeMap::new();
    props.insert(
      a,
      fn_props(
        vec![require_revert(s1), require_revert(s2)],
        vec![],
        vec![],
        vec![],
        vec![],
      ),
    );

    let effects = compute_transitive_effects(&props, &empty_metadata());
    let a_reverts = &effects.reverts[&a];
    assert_eq!(a_reverts.len(), 2);
    let statements: Vec<_> = a_reverts.iter().map(|e| e.revert.topic).collect();
    assert!(statements.contains(&s1));
    assert!(statements.contains(&s2));
  }

  // ==========================================================================
  // Non-revert cases — exercise the full-call-graph propagation.
  // ==========================================================================

  #[test]
  fn mutation_propagates_over_try_edge_but_revert_does_not() {
    // The pairing test that proves the two-graphs split.
    // A try-calls B; B writes state Y AND raises RB.
    // - A.effective_mutations contains (Y, B) — write propagates.
    // - A.effective_reverts does NOT contain RB — revert absorbed.
    let a = fn_topic(1);
    let b = fn_topic(2);
    let y = var_topic(50);
    let err_b = fn_topic(22);
    let mut props = BTreeMap::new();
    props.insert(
      a,
      fn_props(vec![], vec![try_call(b)], vec![], vec![], vec![]),
    );
    props.insert(
      b,
      fn_props(
        vec![custom_revert(fn_topic(12), err_b)],
        vec![],
        vec![y],
        vec![],
        vec![],
      ),
    );

    let effects = compute_transitive_effects(&props, &empty_metadata());

    let a_muts: Vec<_> = effects.mutations[&a]
      .iter()
      .map(|e| (e.origin, e.topic))
      .collect();
    assert_eq!(a_muts.len(), 1);
    assert_eq!(a_muts[0], (b, y), "write must propagate over try edge");

    let a_reverts = &effects.reverts[&a];
    assert!(
      a_reverts.is_empty(),
      "revert must NOT propagate over try edge; got {:?}",
      a_reverts.iter().map(|e| e.origin).collect::<Vec<_>>(),
    );
  }

  #[test]
  fn read_propagates_over_try_edge() {
    let a = fn_topic(1);
    let b = fn_topic(2);
    let x = var_topic(50);
    let mut props = BTreeMap::new();
    props.insert(
      a,
      fn_props(vec![], vec![try_call(b)], vec![], vec![], vec![]),
    );
    props.insert(b, fn_props(vec![], vec![], vec![], vec![x], vec![]));

    let effects = compute_transitive_effects(&props, &empty_metadata());
    let a_reads: Vec<_> = effects.reads[&a]
      .iter()
      .map(|e| (e.origin, e.topic))
      .collect();
    assert_eq!(a_reads, vec![(b, x)]);
  }

  #[test]
  fn event_propagates_over_try_edge() {
    let a = fn_topic(1);
    let b = fn_topic(2);
    let evt = ev_topic(60);
    let mut props = BTreeMap::new();
    props.insert(
      a,
      fn_props(vec![], vec![try_call(b)], vec![], vec![], vec![]),
    );
    props.insert(b, fn_props(vec![], vec![], vec![], vec![], vec![evt]));

    let effects = compute_transitive_effects(&props, &empty_metadata());
    let a_events: Vec<_> = effects.events_emitted[&a]
      .iter()
      .map(|e| (e.origin, e.topic))
      .collect();
    assert_eq!(a_events, vec![(b, evt)]);
  }

  #[test]
  fn asymmetric_try_cycle_mutations_symmetric_reverts_asymmetric() {
    // THE canonical test that locks the two-propagation-graph
    // distinction. A try-calls B; B calls A without try. A writes X,
    // B writes Y. A raises RA, B raises RB.
    //
    // Full call graph SCC = {A, B} → mutations are symmetric.
    // Non-try graph SCCs = {A}, {B} → reverts are asymmetric.
    let a = fn_topic(1);
    let b = fn_topic(2);
    let x = var_topic(50);
    let y = var_topic(51);
    let err_a = fn_topic(21);
    let err_b = fn_topic(22);
    let mut props = BTreeMap::new();
    props.insert(
      a,
      fn_props(
        vec![custom_revert(fn_topic(11), err_a)],
        vec![try_call(b)],
        vec![x],
        vec![],
        vec![],
      ),
    );
    props.insert(
      b,
      fn_props(
        vec![custom_revert(fn_topic(12), err_b)],
        vec![call(a)],
        vec![y],
        vec![],
        vec![],
      ),
    );

    let effects = compute_transitive_effects(&props, &empty_metadata());

    // Mutations: symmetric (full call graph SCC collapses A and B).
    let a_muts: Vec<_> = effects.mutations[&a]
      .iter()
      .map(|e| (e.origin, e.topic))
      .collect();
    let b_muts: Vec<_> = effects.mutations[&b]
      .iter()
      .map(|e| (e.origin, e.topic))
      .collect();
    assert_eq!(a_muts, b_muts, "full-SCC members must share mutation set");
    assert_eq!(a_muts.len(), 2);
    assert!(a_muts.contains(&(a, x)));
    assert!(a_muts.contains(&(b, y)));

    // Reverts: asymmetric (non-try graph splits A and B).
    let a_reverts: Vec<_> = effects.reverts[&a]
      .iter()
      .map(|e| (e.origin, e.revert.error_topic))
      .collect();
    let b_reverts: Vec<_> = effects.reverts[&b]
      .iter()
      .map(|e| (e.origin, e.revert.error_topic))
      .collect();
    assert_eq!(a_reverts, vec![(a, Some(err_a))]);
    assert_eq!(b_reverts.len(), 2);
    assert!(b_reverts.contains(&(a, Some(err_a))));
    assert!(b_reverts.contains(&(b, Some(err_b))));
  }

  #[test]
  fn mutation_dedup_distinct_origins_kept() {
    // A calls B and C, both write the same state var Y.
    // A.effective_mutations has two entries: (Y, B) and (Y, C).
    let a = fn_topic(1);
    let b = fn_topic(2);
    let c = fn_topic(3);
    let y = var_topic(50);
    let mut props = BTreeMap::new();
    props.insert(
      a,
      fn_props(vec![], vec![call(b), call(c)], vec![], vec![], vec![]),
    );
    props.insert(b, fn_props(vec![], vec![], vec![y], vec![], vec![]));
    props.insert(c, fn_props(vec![], vec![], vec![y], vec![], vec![]));

    let effects = compute_transitive_effects(&props, &empty_metadata());
    let a_muts: Vec<_> = effects.mutations[&a]
      .iter()
      .map(|e| (e.origin, e.topic))
      .collect();
    assert_eq!(a_muts.len(), 2);
    assert!(a_muts.contains(&(b, y)));
    assert!(a_muts.contains(&(c, y)));
  }

  #[test]
  fn mutation_dedup_same_origin_collapses() {
    // A calls B twice; B writes Y. A's set has exactly one (Y, B)
    // entry. Two call sites at distinct nodes, same callee.
    let a = fn_topic(1);
    let b = fn_topic(2);
    let y = var_topic(50);
    let mut props = BTreeMap::new();
    props.insert(
      a,
      fn_props(
        vec![],
        vec![
          CallInfo {
            site: fn_topic(100),
            callee: b,
            in_try_block: false,
          },
          CallInfo {
            site: fn_topic(101),
            callee: b,
            in_try_block: false,
          },
        ],
        vec![],
        vec![],
        vec![],
      ),
    );
    props.insert(b, fn_props(vec![], vec![], vec![y], vec![], vec![]));

    let effects = compute_transitive_effects(&props, &empty_metadata());
    let a_muts = &effects.mutations[&a];
    assert_eq!(a_muts.len(), 1);
    assert_eq!(a_muts[0].origin, b);
    assert_eq!(a_muts[0].topic, y);
  }

  // ==========================================================================
  // Shared cases — apply across all four effects.
  // ==========================================================================

  #[test]
  fn proxy_resolution_origin_is_implementation_not_interface() {
    // A calls interface I; I.transitive_topic = Impl; Impl raises X,
    // writes Y, emits E. A's effective sets all carry origin=Impl.
    let a = fn_topic(1);
    let interface = fn_topic(100);
    let impl_fn = fn_topic(2);
    let stmt = fn_topic(11);
    let err = fn_topic(21);
    let y = var_topic(50);
    let evt = ev_topic(60);

    let mut props = BTreeMap::new();
    props.insert(
      a,
      fn_props(vec![], vec![call(interface)], vec![], vec![], vec![]),
    );
    // Impl itself: in scope, raises/writes/emits.
    props.insert(
      impl_fn,
      fn_props(
        vec![custom_revert(stmt, err)],
        vec![],
        vec![y],
        vec![],
        vec![evt],
      ),
    );
    // Interface: NOT in function_properties (stubs are filtered out
    // in the analyzer). Resolution happens via topic_metadata.

    let mut metadata = BTreeMap::new();
    metadata.insert(interface, named_topic_meta(interface, Some(impl_fn)));
    metadata.insert(impl_fn, named_topic_meta(impl_fn, None));

    let effects = compute_transitive_effects(&props, &metadata);

    let a_reverts = &effects.reverts[&a];
    assert_eq!(a_reverts.len(), 1);
    assert_eq!(
      a_reverts[0].origin, impl_fn,
      "origin must be the impl, not the interface"
    );
    assert_eq!(a_reverts[0].revert.error_topic, Some(err));

    let a_muts = &effects.mutations[&a];
    assert_eq!(a_muts.len(), 1);
    assert_eq!(a_muts[0].origin, impl_fn);
    assert_eq!(a_muts[0].topic, y);

    let a_events = &effects.events_emitted[&a];
    assert_eq!(a_events.len(), 1);
    assert_eq!(a_events[0].origin, impl_fn);
    assert_eq!(a_events[0].topic, evt);
  }

  #[test]
  fn out_of_scope_callee_contributes_nothing() {
    // A calls B; B is not in function_properties (e.g. OZ
    // dependency). A's effective sets are exactly A's direct sets.
    let a = fn_topic(1);
    let b = fn_topic(2);
    let mut props = BTreeMap::new();
    props.insert(
      a,
      fn_props(
        vec![custom_revert(fn_topic(11), fn_topic(21))],
        vec![call(b)],
        vec![var_topic(50)],
        vec![],
        vec![],
      ),
    );
    // B intentionally NOT inserted into props.

    let effects = compute_transitive_effects(&props, &empty_metadata());
    assert_eq!(effects.reverts[&a].len(), 1);
    assert_eq!(effects.reverts[&a][0].origin, a);
    assert_eq!(effects.mutations[&a].len(), 1);
    assert_eq!(effects.mutations[&a][0].origin, a);
  }

  #[test]
  fn isolated_function_has_only_direct_effects() {
    // No calls, all direct effects present.
    let a = fn_topic(1);
    let stmt = fn_topic(11);
    let err = fn_topic(21);
    let x = var_topic(50);
    let r = var_topic(51);
    let evt = ev_topic(60);
    let mut props = BTreeMap::new();
    props.insert(
      a,
      fn_props(
        vec![custom_revert(stmt, err)],
        vec![],
        vec![x],
        vec![r],
        vec![evt],
      ),
    );

    let effects = compute_transitive_effects(&props, &empty_metadata());
    assert_eq!(effects.reverts[&a].len(), 1);
    assert_eq!(effects.reverts[&a][0].origin, a);
    assert_eq!(effects.mutations[&a].len(), 1);
    assert_eq!(effects.mutations[&a][0].topic, x);
    assert_eq!(effects.reads[&a].len(), 1);
    assert_eq!(effects.reads[&a][0].topic, r);
    assert_eq!(effects.events_emitted[&a].len(), 1);
    assert_eq!(effects.events_emitted[&a][0].topic, evt);
  }

  #[test]
  fn empty_input_returns_empty_maps() {
    let props = BTreeMap::new();
    let effects = compute_transitive_effects(&props, &empty_metadata());
    assert!(effects.reverts.is_empty());
    assert!(effects.mutations.is_empty());
    assert!(effects.reads.is_empty());
    assert!(effects.events_emitted.is_empty());
  }

  #[test]
  fn self_recursive_function_inside_scc_skip_holds() {
    // A calls A; no outside-SCC callees. A is its own SCC with a
    // self-loop edge. The inside-SCC skip must keep us from
    // re-folding A's own effective set into itself (which would
    // either infinite-loop or double the entries before dedup).
    // The correct result is just A's direct effects with origin=A.
    let a = fn_topic(1);
    let stmt = fn_topic(11);
    let err = fn_topic(21);
    let x = var_topic(50);
    let mut props = BTreeMap::new();
    props.insert(
      a,
      fn_props(
        vec![custom_revert(stmt, err)],
        vec![call(a)],
        vec![x],
        vec![],
        vec![],
      ),
    );

    let effects = compute_transitive_effects(&props, &empty_metadata());
    assert_eq!(effects.reverts[&a].len(), 1);
    assert_eq!(effects.reverts[&a][0].origin, a);
    assert_eq!(effects.mutations[&a].len(), 1);
    assert_eq!(effects.mutations[&a][0].topic, x);
    assert_eq!(effects.mutations[&a][0].origin, a);
  }

  #[test]
  fn diamond_same_origin_dedups_via_paths() {
    // A → {B, C}; both B and C call D; D raises X.
    //   - A.effective_reverts has exactly one entry (X, origin=D)
    //     because B.effective and C.effective both contain the same
    //     (X, D) and the per-effect dedup key collapses them.
    let a = fn_topic(1);
    let b = fn_topic(2);
    let c = fn_topic(3);
    let d = fn_topic(4);
    let stmt = fn_topic(11);
    let err = fn_topic(21);
    let mut props = BTreeMap::new();
    props.insert(
      a,
      fn_props(vec![], vec![call(b), call(c)], vec![], vec![], vec![]),
    );
    props.insert(b, fn_props(vec![], vec![call(d)], vec![], vec![], vec![]));
    props.insert(c, fn_props(vec![], vec![call(d)], vec![], vec![], vec![]));
    props.insert(
      d,
      fn_props(
        vec![custom_revert(stmt, err)],
        vec![],
        vec![],
        vec![],
        vec![],
      ),
    );

    let effects = compute_transitive_effects(&props, &empty_metadata());
    let a_reverts = &effects.reverts[&a];
    assert_eq!(
      a_reverts.len(),
      1,
      "same (origin=D, error=X) via two paths must dedup; got {:?}",
      a_reverts.iter().map(|e| e.origin).collect::<Vec<_>>(),
    );
    assert_eq!(a_reverts[0].origin, d);
    assert_eq!(a_reverts[0].revert.error_topic, Some(err));
  }

  #[test]
  fn out_of_scope_callee_mid_chain_breaks_propagation() {
    // A calls B (in scope); B calls C (out of scope); C raises Z
    // (unobservable). A.effective contains nothing from C — the
    // conservative semantics propagate: out-of-scope cuts the chain.
    let a = fn_topic(1);
    let b = fn_topic(2);
    let c_out = fn_topic(3);
    let mut props = BTreeMap::new();
    props.insert(a, fn_props(vec![], vec![call(b)], vec![], vec![], vec![]));
    props.insert(
      b,
      fn_props(vec![], vec![call(c_out)], vec![], vec![], vec![]),
    );
    // c_out intentionally NOT in props — represents an OZ-style dep.

    let effects = compute_transitive_effects(&props, &empty_metadata());
    assert!(
      effects.reverts[&a].is_empty(),
      "out-of-scope mid-chain must NOT leak unknown effects; got {:?}",
      effects.reverts[&a]
        .iter()
        .map(|e| e.origin)
        .collect::<Vec<_>>(),
    );
    assert!(effects.mutations[&a].is_empty());
    assert!(effects.reads[&a].is_empty());
    assert!(effects.events_emitted[&a].is_empty());
  }

  #[test]
  fn modifier_variant_folds_symmetrically_with_function_variant() {
    // Both FunctionProperties and ModifierProperties go through the
    // same fold's `| ModifierProperties { .. }` match arms. Verify
    // that a modifier on each end of the call chain works:
    //   function A calls modifier M; M writes state X.
    //   A.effective_mutations ⊇ {(X, origin=M)}.
    let a = fn_topic(1);
    let m = fn_topic(2);
    let x = var_topic(50);
    let mut props = BTreeMap::new();
    props.insert(a, fn_props(vec![], vec![call(m)], vec![], vec![], vec![]));
    props.insert(m, mod_props(vec![], vec![], vec![x], vec![], vec![]));

    let effects = compute_transitive_effects(&props, &empty_metadata());
    let a_muts = &effects.mutations[&a];
    assert_eq!(a_muts.len(), 1);
    assert_eq!(a_muts[0].origin, m);
    assert_eq!(a_muts[0].topic, x);

    // And M's own effective_mutations should include its direct (X, M).
    let m_muts = &effects.mutations[&m];
    assert_eq!(m_muts.len(), 1);
    assert_eq!(m_muts[0].origin, m);
    assert_eq!(m_muts[0].topic, x);
  }

  #[test]
  fn bidirectional_try_cycle_no_revert_propagation_either_way() {
    // Both edges are try-wrapped: A try-calls B, B try-calls A.
    // The revert propagation graph has zero edges between A and B,
    // so each is in its own singleton SCC. Reverts don't propagate
    // either direction. But the full call graph still has both
    // edges → mutations propagate as a cycle.
    let a = fn_topic(1);
    let b = fn_topic(2);
    let err_a = fn_topic(21);
    let err_b = fn_topic(22);
    let x = var_topic(50);
    let y = var_topic(51);
    let mut props = BTreeMap::new();
    props.insert(
      a,
      fn_props(
        vec![custom_revert(fn_topic(11), err_a)],
        vec![try_call(b)],
        vec![x],
        vec![],
        vec![],
      ),
    );
    props.insert(
      b,
      fn_props(
        vec![custom_revert(fn_topic(12), err_b)],
        vec![try_call(a)],
        vec![y],
        vec![],
        vec![],
      ),
    );

    let effects = compute_transitive_effects(&props, &empty_metadata());

    // Reverts: each function sees only its own.
    let a_reverts: Vec<_> = effects.reverts[&a]
      .iter()
      .map(|e| (e.origin, e.revert.error_topic))
      .collect();
    let b_reverts: Vec<_> = effects.reverts[&b]
      .iter()
      .map(|e| (e.origin, e.revert.error_topic))
      .collect();
    assert_eq!(a_reverts, vec![(a, Some(err_a))]);
    assert_eq!(b_reverts, vec![(b, Some(err_b))]);

    // Mutations: full call graph still cycles → A and B share set.
    let a_muts: Vec<_> = effects.mutations[&a]
      .iter()
      .map(|e| (e.origin, e.topic))
      .collect();
    let b_muts: Vec<_> = effects.mutations[&b]
      .iter()
      .map(|e| (e.origin, e.topic))
      .collect();
    assert_eq!(a_muts, b_muts);
    assert_eq!(a_muts.len(), 2);
    assert!(a_muts.contains(&(a, x)));
    assert!(a_muts.contains(&(b, y)));
  }
}
