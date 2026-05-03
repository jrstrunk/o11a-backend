//! Phase C — co-location resolution. Pins a pair of ambiguous references
//! `(a, b)` whose immediate-enclosing-scope sets intersect at exactly one
//! function/modifier/struct/event/error scope. Complements Phase B (PR
//! gradient): B exploits topology, C exploits *uniqueness* — when two
//! distinct names co-occur and only one shared scope declares both, that
//! shared scope is necessarily where the reference resolves.
//!
//! The algorithm here is consumer-agnostic: it operates on a flat slice
//! of `(ref_id, candidate_set)` and returns winners. The doc-tree pass
//! (per section) and the dev-doc pass (per comment) both feed into this
//! one entry point. `RefId` is generic so each consumer can reuse its
//! own keying.
//!
//! Spec: see "Phase C — co-location resolution" in
//! `crates/o11a-analyze/docs/specs/semantic-resolution-graph.md` and
//! "Phase 9" in
//! `crates/o11a-analyze/docs/build-plans/semantic-resolution-graph.md`.
//!
//! Determinism contract: pair iteration is `(i, j)` with `i < j` over
//! the input slice's existing order; intersections are computed via
//! `BTreeSet` so iteration is topic-ID ascending; conflicting pins (the
//! same ref pinned to two different candidates by two different pairs)
//! drop the ref entirely rather than picking one — that keeps the result
//! a pure function of the input.

use std::collections::{BTreeMap, BTreeSet};

use crate::domain;
use crate::domain::topic;

/// One ambiguous reference fed into Phase C. `ref_id` is opaque (the
/// consumer keys its resolution map on whatever shape it likes); the
/// algorithm only requires `Clone + Ord`. `candidates` is the full
/// candidate list from `TopicNameIndex::candidates_by_simple_name`.
#[derive(Debug, Clone)]
pub struct CoLocInput<R> {
  pub ref_id: R,
  pub candidates: Vec<topic::Topic>,
}

/// One pinning produced by Phase C. The chosen candidate is the only
/// candidate of `ref_id` whose immediate enclosing function/modifier/
/// struct/event/error scope is the singleton intersection scope shared
/// with at least one other co-located reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoLocResolution<R> {
  pub ref_id: R,
  pub chosen_topic: topic::Topic,
  /// The function/modifier/struct/event/error topic that uniquely pinned
  /// this resolution. Surfaced for traces / debugging — consumers don't
  /// need it to apply the resolution.
  pub pinned_scope: topic::Topic,
}

/// Walk a candidate's scope chain (innermost-first) and return the first
/// enclosing topic whose `NamedTopicKind` is one of the Phase-C-eligible
/// scopes (Function, Modifier, Struct, Event, Error). `None` when the
/// candidate has no such enclosing scope — e.g., a contract-level
/// function (whose immediate enclosing topic is a contract, too coarse
/// per the spec) or a builtin / global declaration.
///
/// Skips the candidate itself: a function reference's *own* scope chain
/// starts with the function, but Phase C looks at what *encloses* the
/// candidate, not the candidate itself.
pub fn enclosing_member_scope(
  audit_data: &domain::AuditData,
  candidate: topic::Topic,
) -> Option<topic::Topic> {
  let chain = domain::scope_ancestor_chain(audit_data, candidate);
  // chain[0] is the candidate itself; start at index 1.
  for ancestor in chain.iter().skip(1) {
    let Some(metadata) = audit_data.topic_metadata.get(ancestor) else {
      continue;
    };
    let domain::TopicMetadata::NamedTopic { kind, .. } = metadata else {
      continue;
    };
    if matches!(
      kind,
      domain::NamedTopicKind::Function(_)
        | domain::NamedTopicKind::Modifier
        | domain::NamedTopicKind::Struct
        | domain::NamedTopicKind::Event
        | domain::NamedTopicKind::Error
    ) {
      return Some(*ancestor);
    }
  }
  None
}

/// Run Phase C against the supplied ambiguous references and return any
/// resolutions Phase C can pin. The output is in BTreeMap-sorted order
/// over input indices, which — because iteration in the algorithm is
/// `(i, j)` with `i < j` over the input — produces a stable, repeatable
/// result for the same input.
///
/// Empty result is normal: Phase C only fires when *uniqueness* is
/// available; ambiguities resolved by Phase B alone never need Phase C.
pub fn co_locate<R: Clone + Ord>(
  audit_data: &domain::AuditData,
  refs: &[CoLocInput<R>],
) -> Vec<CoLocResolution<R>> {
  if refs.len() < 2 {
    // Phase C requires at least one *pair* — a single ref has no
    // co-location signal to draw on.
    return Vec::new();
  }

  // Per-ref: { enclosing_scope_topic → [candidates in that scope] }.
  // BTreeMap iteration order is topic-ID ascending, which makes the
  // intersection step deterministic.
  let decl_maps: Vec<BTreeMap<topic::Topic, Vec<topic::Topic>>> = refs
    .iter()
    .map(|input| build_decl_map(audit_data, &input.candidates))
    .collect();

  let mut pins: BTreeMap<usize, Pin> = BTreeMap::new();

  for i in 0..refs.len() {
    if decl_maps[i].is_empty() {
      // No Phase-C-eligible candidate scopes for ref i — it cannot pin
      // anything and cannot be pinned.
      continue;
    }
    for j in (i + 1)..refs.len() {
      if decl_maps[j].is_empty() {
        continue;
      }
      let i_scopes: BTreeSet<topic::Topic> =
        decl_maps[i].keys().copied().collect();
      let j_scopes: BTreeSet<topic::Topic> =
        decl_maps[j].keys().copied().collect();

      // Use the first two intersection elements to detect
      // "exactly one"; a streaming check avoids materializing a
      // possibly-large intersection just to count it.
      let mut iter = i_scopes.intersection(&j_scopes);
      let Some(scope_x) = iter.next().copied() else {
        continue;
      };
      if iter.next().is_some() {
        // Intersection has 2+ elements — Phase C deliberately abstains.
        continue;
      }

      let i_cands = &decl_maps[i][&scope_x];
      let j_cands = &decl_maps[j][&scope_x];
      if i_cands.len() != 1 || j_cands.len() != 1 {
        // Multiple candidates of `i` (or `j`) live inside the singleton
        // scope — pinning would be ambiguous. Skip rather than pick
        // arbitrarily.
        continue;
      }

      record_pin(&mut pins, i, i_cands[0], scope_x);
      record_pin(&mut pins, j, j_cands[0], scope_x);
    }
  }

  pins
    .into_iter()
    .filter_map(|(idx, pin)| match pin {
      Pin::Single(cand, scope) => Some(CoLocResolution {
        ref_id: refs[idx].ref_id.clone(),
        chosen_topic: cand,
        pinned_scope: scope,
      }),
      Pin::Conflicted => None,
    })
    .collect()
}

/// Build the per-ref `decl_map`: for each candidate that has an
/// enclosing function/modifier/struct/event/error scope, record the
/// candidate under that scope. Sort + dedup the candidate vecs so two
/// repeated candidate-list entries never look like a multi-candidate
/// scope.
fn build_decl_map(
  audit_data: &domain::AuditData,
  candidates: &[topic::Topic],
) -> BTreeMap<topic::Topic, Vec<topic::Topic>> {
  let mut map: BTreeMap<topic::Topic, Vec<topic::Topic>> = BTreeMap::new();
  for &candidate in candidates {
    if let Some(scope) = enclosing_member_scope(audit_data, candidate) {
      map.entry(scope).or_default().push(candidate);
    }
  }
  for v in map.values_mut() {
    v.sort();
    v.dedup();
  }
  map
}

/// Tracks the resolution state of one ref during the pair scan. A
/// single pin is `Single(candidate, scope)`; two pairs that disagree on
/// the same ref escalate to `Conflicted` — the ref drops out. Pinning
/// to the SAME candidate from multiple pairs is a no-op.
enum Pin {
  Single(topic::Topic, topic::Topic),
  Conflicted,
}

fn record_pin(
  pins: &mut BTreeMap<usize, Pin>,
  idx: usize,
  cand: topic::Topic,
  scope: topic::Topic,
) {
  match pins.get(&idx) {
    None => {
      pins.insert(idx, Pin::Single(cand, scope));
    }
    Some(Pin::Single(existing, _)) if *existing == cand => {}
    Some(_) => {
      pins.insert(idx, Pin::Conflicted);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::domain::{
    AuditData, ContainingBlockLayer, ContractKind, FunctionKind, NamedTopicKind,
    NamedTopicVisibility, ProjectPath, Scope, TopicMetadata, UnnamedTopicKind,
    new_audit_data,
  };
  use std::collections::HashSet;

  fn audit() -> AuditData {
    new_audit_data("t".to_string(), HashSet::new(), None)
  }

  fn nt(id: i32) -> topic::Topic {
    topic::new_node_topic(&id)
  }

  fn pp() -> ProjectPath {
    ProjectPath {
      file_path: "x.sol".to_string(),
    }
  }

  fn named(t: topic::Topic, name: &str, kind: NamedTopicKind, scope: Scope) -> TopicMetadata {
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

  fn unnamed(t: topic::Topic, kind: UnnamedTopicKind, scope: Scope) -> TopicMetadata {
    TopicMetadata::UnnamedTopic {
      topic: t,
      scope,
      kind,
      transitive_topic: None,
    }
  }

  // ---- enclosing_member_scope ----

  #[test]
  fn enclosing_scope_for_function_inside_contract_is_none() {
    // A contract-level function's enclosing topic is the contract,
    // which the spec deliberately excludes (too coarse).
    let mut a = audit();
    let contract = nt(1);
    let func = nt(10);
    a.topic_metadata.insert(
      contract,
      named(
        contract,
        "Vault",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container { container: pp() },
      ),
    );
    a.topic_metadata.insert(
      func,
      named(
        func,
        "transfer",
        NamedTopicKind::Function(FunctionKind::Function),
        Scope::Component {
          container: pp(),
          component: contract,
        },
      ),
    );
    assert_eq!(enclosing_member_scope(&a, func), None);
  }

  #[test]
  fn enclosing_scope_for_param_in_function_signature_is_function() {
    // function foo(uint amount) — `amount` is `Member`-scoped under foo.
    let mut a = audit();
    let contract = nt(1);
    let func = nt(10);
    let param = nt(11);
    a.topic_metadata.insert(
      contract,
      named(
        contract,
        "Vault",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container { container: pp() },
      ),
    );
    a.topic_metadata.insert(
      func,
      named(
        func,
        "foo",
        NamedTopicKind::Function(FunctionKind::Function),
        Scope::Component {
          container: pp(),
          component: contract,
        },
      ),
    );
    a.topic_metadata.insert(
      param,
      named(
        param,
        "amount",
        NamedTopicKind::LocalVariable,
        Scope::Member {
          container: pp(),
          component: contract,
          member: func,
          signature_container: None,
        },
      ),
    );
    assert_eq!(enclosing_member_scope(&a, param), Some(func));
  }

  #[test]
  fn enclosing_scope_for_local_in_nested_block_walks_through_blocks_to_function() {
    // function foo() { { local_x } } — local_x sits inside an inner
    // SemanticBlock inside an outer SemanticBlock inside foo. The
    // enclosing function-level scope is `foo` regardless of how many
    // (un-named) blocks separate them.
    let mut a = audit();
    let contract = nt(1);
    let func = nt(10);
    let outer_block = nt(20);
    let inner_block = nt(21);
    let local = nt(30);
    a.topic_metadata.insert(
      contract,
      named(
        contract,
        "Vault",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container { container: pp() },
      ),
    );
    a.topic_metadata.insert(
      func,
      named(
        func,
        "foo",
        NamedTopicKind::Function(FunctionKind::Function),
        Scope::Component {
          container: pp(),
          component: contract,
        },
      ),
    );
    a.topic_metadata.insert(
      outer_block,
      unnamed(
        outer_block,
        UnnamedTopicKind::SemanticBlock,
        Scope::Member {
          container: pp(),
          component: contract,
          member: func,
          signature_container: None,
        },
      ),
    );
    a.topic_metadata.insert(
      inner_block,
      unnamed(
        inner_block,
        UnnamedTopicKind::SemanticBlock,
        Scope::ContainingBlock {
          container: pp(),
          component: contract,
          member: func,
          containing_blocks: vec![ContainingBlockLayer {
            block: outer_block,
            annotation: None,
          }],
        },
      ),
    );
    a.topic_metadata.insert(
      local,
      named(
        local,
        "local_x",
        NamedTopicKind::LocalVariable,
        Scope::ContainingBlock {
          container: pp(),
          component: contract,
          member: func,
          containing_blocks: vec![
            ContainingBlockLayer {
              block: outer_block,
              annotation: None,
            },
            ContainingBlockLayer {
              block: inner_block,
              annotation: None,
            },
          ],
        },
      ),
    );
    assert_eq!(enclosing_member_scope(&a, local), Some(func));
  }

  #[test]
  fn enclosing_scope_for_struct_field_is_struct() {
    let mut a = audit();
    let contract = nt(1);
    let s = nt(10);
    let field = nt(11);
    a.topic_metadata.insert(
      contract,
      named(
        contract,
        "Vault",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container { container: pp() },
      ),
    );
    a.topic_metadata.insert(
      s,
      named(
        s,
        "Point",
        NamedTopicKind::Struct,
        Scope::Component {
          container: pp(),
          component: contract,
        },
      ),
    );
    a.topic_metadata.insert(
      field,
      named(
        field,
        "x",
        NamedTopicKind::LocalVariable,
        Scope::Component {
          container: pp(),
          component: s,
        },
      ),
    );
    assert_eq!(enclosing_member_scope(&a, field), Some(s));
  }

  #[test]
  fn enclosing_scope_for_global_topic_is_none() {
    let mut a = audit();
    let g = nt(1);
    a.topic_metadata.insert(
      g,
      named(g, "g", NamedTopicKind::Builtin, Scope::Global),
    );
    assert_eq!(enclosing_member_scope(&a, g), None);
  }

  // ---- co_locate ----

  /// Two ambiguous refs whose only shared enclosing scope is one
  /// function — both pin to that function's declarations.
  #[test]
  fn co_locate_singleton_intersection_pins_pair() {
    let mut a = audit();
    let contract = nt(1);
    let foo = nt(10);
    let bar = nt(20);
    let foo_amount = nt(11);
    let foo_tmp = nt(12);
    let bar_amount = nt(21);
    let bar_tmp = nt(22);

    a.topic_metadata.insert(
      contract,
      named(
        contract,
        "C",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container { container: pp() },
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
          container: pp(),
          component: contract,
        },
        Some(m) => Scope::Member {
          container: pp(),
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
      a.topic_metadata.insert(t, named(t, name, kind, scope));
    }

    let refs = vec![
      CoLocInput {
        ref_id: 1usize,
        candidates: vec![foo_amount, bar_amount],
      },
      CoLocInput {
        ref_id: 2usize,
        candidates: vec![foo_tmp, bar_tmp],
      },
    ];

    // amount-decls = {foo, bar}; tmp-decls = {foo, bar}; intersection
    // is {foo, bar} — two scopes, NOT a singleton. Phase C abstains.
    let res = co_locate(&a, &refs);
    assert!(
      res.is_empty(),
      "two-element intersection must abstain: got {:?}",
      res,
    );

    // Now constrain: only `foo` declares both `amount` and `tmp`. Drop
    // bar_tmp from the candidates.
    let refs = vec![
      CoLocInput {
        ref_id: 1usize,
        candidates: vec![foo_amount, bar_amount],
      },
      CoLocInput {
        ref_id: 2usize,
        candidates: vec![foo_tmp],
      },
    ];
    let res = co_locate(&a, &refs);
    assert_eq!(res.len(), 2);
    let pins: BTreeMap<usize, topic::Topic> =
      res.iter().map(|r| (r.ref_id, r.chosen_topic)).collect();
    assert_eq!(pins.get(&1), Some(&foo_amount));
    assert_eq!(pins.get(&2), Some(&foo_tmp));
    for r in &res {
      assert_eq!(r.pinned_scope, foo);
    }
  }

  /// Single ref → no pair → no Phase C output.
  #[test]
  fn co_locate_with_single_ref_returns_empty() {
    let mut a = audit();
    let contract = nt(1);
    let foo = nt(10);
    let foo_amount = nt(11);
    a.topic_metadata.insert(
      contract,
      named(
        contract,
        "C",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container { container: pp() },
      ),
    );
    a.topic_metadata.insert(
      foo,
      named(
        foo,
        "foo",
        NamedTopicKind::Function(FunctionKind::Function),
        Scope::Component {
          container: pp(),
          component: contract,
        },
      ),
    );
    a.topic_metadata.insert(
      foo_amount,
      named(
        foo_amount,
        "amount",
        NamedTopicKind::LocalVariable,
        Scope::Member {
          container: pp(),
          component: contract,
          member: foo,
          signature_container: None,
        },
      ),
    );
    let refs = vec![CoLocInput {
      ref_id: 1usize,
      candidates: vec![foo_amount],
    }];
    assert!(co_locate(&a, &refs).is_empty());
  }

  /// Multi-element intersection: two scopes share both names → Phase C
  /// abstains for that pair.
  #[test]
  fn co_locate_multi_element_intersection_abstains() {
    let mut a = audit();
    let contract = nt(1);
    let foo = nt(10);
    let bar = nt(20);
    // Both functions declare `amount` and `tmp`.
    let foo_amount = nt(11);
    let foo_tmp = nt(12);
    let bar_amount = nt(21);
    let bar_tmp = nt(22);

    a.topic_metadata.insert(
      contract,
      named(
        contract,
        "C",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container { container: pp() },
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
          container: pp(),
          component: contract,
        },
        Some(m) => Scope::Member {
          container: pp(),
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
      a.topic_metadata.insert(t, named(t, name, kind, scope));
    }

    let refs = vec![
      CoLocInput {
        ref_id: 1usize,
        candidates: vec![foo_amount, bar_amount],
      },
      CoLocInput {
        ref_id: 2usize,
        candidates: vec![foo_tmp, bar_tmp],
      },
    ];
    assert!(co_locate(&a, &refs).is_empty());
  }

  /// Three-ref interaction where two pairs would pin the same ref to
  /// different candidates → conflict → that ref drops out, others stay.
  #[test]
  fn co_locate_conflicting_pin_drops_only_the_conflicting_ref() {
    // contract C { function foo() { x; y; } function bar() { x; z; } }
    // refs: x (candidates {foo.x, bar.x}), y (candidates {foo.y}), z
    // (candidates {bar.z}).
    // Pair (x, y): intersection {foo} → x pins to foo.x.
    // Pair (x, z): intersection {bar} → x pins to bar.x → CONFLICT.
    // Pair (y, z): intersection {} → no pin.
    // Final: y → foo.y, z → bar.z, x dropped.
    let mut a = audit();
    let contract = nt(1);
    let foo = nt(10);
    let bar = nt(20);
    let foo_x = nt(11);
    let foo_y = nt(12);
    let bar_x = nt(21);
    let bar_z = nt(22);

    a.topic_metadata.insert(
      contract,
      named(
        contract,
        "C",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container { container: pp() },
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
          container: pp(),
          component: contract,
        },
        Some(m) => Scope::Member {
          container: pp(),
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
      a.topic_metadata.insert(t, named(t, name, kind, scope));
    }

    let refs = vec![
      CoLocInput {
        ref_id: 1usize, // x
        candidates: vec![foo_x, bar_x],
      },
      CoLocInput {
        ref_id: 2usize, // y
        candidates: vec![foo_y],
      },
      CoLocInput {
        ref_id: 3usize, // z
        candidates: vec![bar_z],
      },
    ];
    let res = co_locate(&a, &refs);
    let pins: BTreeMap<usize, topic::Topic> =
      res.iter().map(|r| (r.ref_id, r.chosen_topic)).collect();
    assert_eq!(pins.get(&1), None, "conflicted ref must drop out");
    assert_eq!(pins.get(&2), Some(&foo_y));
    assert_eq!(pins.get(&3), Some(&bar_z));
  }

  /// Determinism: identical input produces byte-identical output even
  /// when candidates are repeated (the dedup guarantees one entry per
  /// scope) and even across different topic-id orderings inside the
  /// candidate vec.
  #[test]
  fn co_locate_is_deterministic() {
    let mut a = audit();
    let contract = nt(1);
    let foo = nt(10);
    let bar = nt(20);
    let foo_amount = nt(11);
    let foo_tmp = nt(12);
    let bar_amount = nt(21);

    a.topic_metadata.insert(
      contract,
      named(
        contract,
        "C",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container { container: pp() },
      ),
    );
    for (t, name, member) in [
      (foo, "foo", None),
      (bar, "bar", None),
      (foo_amount, "amount", Some(foo)),
      (foo_tmp, "tmp", Some(foo)),
      (bar_amount, "amount", Some(bar)),
    ] {
      let scope = match member {
        None => Scope::Component {
          container: pp(),
          component: contract,
        },
        Some(m) => Scope::Member {
          container: pp(),
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
      a.topic_metadata.insert(t, named(t, name, kind, scope));
    }

    let refs_a = vec![
      CoLocInput {
        ref_id: 1usize,
        candidates: vec![foo_amount, bar_amount],
      },
      CoLocInput {
        ref_id: 2usize,
        candidates: vec![foo_tmp],
      },
    ];
    // Same logical input but candidates in reverse order.
    let refs_b = vec![
      CoLocInput {
        ref_id: 1usize,
        candidates: vec![bar_amount, foo_amount],
      },
      CoLocInput {
        ref_id: 2usize,
        candidates: vec![foo_tmp],
      },
    ];
    let res_a = co_locate(&a, &refs_a);
    let res_b = co_locate(&a, &refs_b);
    assert_eq!(res_a, res_b);
  }

  /// Singleton intersection scope but multiple candidates of one ref
  /// reside in that scope → abstain. This is the rare case where the
  /// same simple name is declared twice inside one function (e.g. via
  /// shadowing a parameter with a local). Pinning would be ambiguous,
  /// so Phase C correctly stays out.
  #[test]
  fn co_locate_abstains_when_singleton_scope_holds_multiple_candidates() {
    let mut a = audit();
    let contract = nt(1);
    let foo = nt(10);
    let bar = nt(20);
    // Two `amount`s declared inside foo (param + body local would be
    // the real-world shape — both carry the simple name).
    let foo_amount_param = nt(11);
    let foo_amount_local = nt(12);
    let foo_tmp = nt(13);
    let bar_amount = nt(21);

    a.topic_metadata.insert(
      contract,
      named(
        contract,
        "C",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container { container: pp() },
      ),
    );
    for (t, name, member) in [
      (foo, "foo", None),
      (bar, "bar", None),
      (foo_amount_param, "amount", Some(foo)),
      (foo_amount_local, "amount", Some(foo)),
      (foo_tmp, "tmp", Some(foo)),
      (bar_amount, "amount", Some(bar)),
    ] {
      let scope = match member {
        None => Scope::Component {
          container: pp(),
          component: contract,
        },
        Some(m) => Scope::Member {
          container: pp(),
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
      a.topic_metadata.insert(t, named(t, name, kind, scope));
    }

    // amount-decls = {foo: [foo_amount_param, foo_amount_local], bar:
    // [bar_amount]}; tmp-decls = {foo: [foo_tmp]}. Intersection = {foo}
    // (singleton), but `foo` holds TWO candidates of `amount`. Phase C
    // must abstain to avoid an arbitrary pick.
    let refs = vec![
      CoLocInput {
        ref_id: 1usize,
        candidates: vec![foo_amount_param, foo_amount_local, bar_amount],
      },
      CoLocInput {
        ref_id: 2usize,
        candidates: vec![foo_tmp],
      },
    ];
    let res = co_locate(&a, &refs);
    assert!(
      res.is_empty(),
      "multi-candidate-in-singleton-scope must abstain: got {:?}",
      res,
    );
  }

  /// Four-ref scenario testing the full pin/conflict/abstain interaction:
  ///
  ///   - A: candidates {f.A, g.A}     (in foo or bar)
  ///   - B: candidates {f.B}          (only in foo)
  ///   - C: candidates {f.C}          (only in foo)
  ///   - D: candidates {g.D}          (only in bar)
  ///
  /// Pair (A,B): intersection {foo} → A pins to f.A, B pins to f.B.
  /// Pair (A,C): intersection {foo} → A's existing f.A confirmed (no-op),
  ///             C pins to f.C.
  /// Pair (A,D): intersection {bar} → A would pin to g.A, but f.A
  ///             already pinned ≠ g.A → A escalates to Conflicted.
  ///             D pins to g.D.
  /// Pair (B,C): intersection {foo} → both already pinned consistently.
  /// Pair (B,D): intersection {} → no pin.
  /// Pair (C,D): intersection {} → no pin.
  ///
  /// Final: A dropped (conflict), B → f.B, C → f.C, D → g.D.
  ///
  /// Pins this is order-independent: even if D appears before A in the
  /// input, the same final state must emerge.
  #[test]
  fn co_locate_four_ref_complex_pin_conflict_survivor_pattern() {
    let mut a = audit();
    let contract = nt(1);
    let foo = nt(10);
    let bar = nt(20);
    let foo_a = nt(11);
    let foo_b = nt(12);
    let foo_c = nt(13);
    let bar_a = nt(21);
    let bar_d = nt(22);

    a.topic_metadata.insert(
      contract,
      named(
        contract,
        "C",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container { container: pp() },
      ),
    );
    for (t, name, member) in [
      (foo, "foo", None),
      (bar, "bar", None),
      (foo_a, "A", Some(foo)),
      (foo_b, "B", Some(foo)),
      (foo_c, "C", Some(foo)),
      (bar_a, "A", Some(bar)),
      (bar_d, "D", Some(bar)),
    ] {
      let scope = match member {
        None => Scope::Component {
          container: pp(),
          component: contract,
        },
        Some(m) => Scope::Member {
          container: pp(),
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
      a.topic_metadata.insert(t, named(t, name, kind, scope));
    }

    let canonical_refs = vec![
      CoLocInput {
        ref_id: 1usize,
        candidates: vec![foo_a, bar_a],
      },
      CoLocInput {
        ref_id: 2usize,
        candidates: vec![foo_b],
      },
      CoLocInput {
        ref_id: 3usize,
        candidates: vec![foo_c],
      },
      CoLocInput {
        ref_id: 4usize,
        candidates: vec![bar_d],
      },
    ];

    let res = co_locate(&a, &canonical_refs);
    let pins: BTreeMap<usize, topic::Topic> =
      res.iter().map(|r| (r.ref_id, r.chosen_topic)).collect();

    assert_eq!(pins.get(&1), None, "A must drop out (conflict)");
    assert_eq!(pins.get(&2), Some(&foo_b));
    assert_eq!(pins.get(&3), Some(&foo_c));
    assert_eq!(pins.get(&4), Some(&bar_d));

    // Order-independence: shuffle the input and assert the same final
    // pins. The iteration order of `(i, j)` pairs differs but the
    // converged result must match.
    let shuffled_refs = vec![
      CoLocInput {
        ref_id: 4usize,
        candidates: vec![bar_d],
      },
      CoLocInput {
        ref_id: 1usize,
        candidates: vec![bar_a, foo_a], // order also swapped
      },
      CoLocInput {
        ref_id: 3usize,
        candidates: vec![foo_c],
      },
      CoLocInput {
        ref_id: 2usize,
        candidates: vec![foo_b],
      },
    ];
    let res_shuffled = co_locate(&a, &shuffled_refs);
    let pins_shuffled: BTreeMap<usize, topic::Topic> =
      res_shuffled.iter().map(|r| (r.ref_id, r.chosen_topic)).collect();
    assert_eq!(
      pins, pins_shuffled,
      "co_locate's final state must be input-order-independent",
    );
  }

  /// Skipping-empty-decl-maps invariant: a ref whose every candidate has
  /// no enclosing function/modifier/etc. (e.g., all candidates are
  /// contract-level functions) takes no part in Phase C and never
  /// affects the rest.
  #[test]
  fn co_locate_skips_refs_with_only_contract_level_candidates() {
    let mut a = audit();
    let contract = nt(1);
    let foo = nt(10); // contract-level function — no enclosing member scope
    let bar = nt(20); // contract-level function — no enclosing member scope
    a.topic_metadata.insert(
      contract,
      named(
        contract,
        "C",
        NamedTopicKind::Contract(ContractKind::Contract),
        Scope::Container { container: pp() },
      ),
    );
    a.topic_metadata.insert(
      foo,
      named(
        foo,
        "transfer",
        NamedTopicKind::Function(FunctionKind::Function),
        Scope::Component {
          container: pp(),
          component: contract,
        },
      ),
    );
    a.topic_metadata.insert(
      bar,
      named(
        bar,
        "transfer",
        NamedTopicKind::Function(FunctionKind::Function),
        Scope::Component {
          container: pp(),
          component: contract,
        },
      ),
    );

    let refs = vec![
      CoLocInput {
        ref_id: 1usize,
        candidates: vec![foo, bar],
      },
      CoLocInput {
        ref_id: 2usize,
        candidates: vec![foo, bar],
      },
    ];
    assert!(co_locate(&a, &refs).is_empty());
  }
}
