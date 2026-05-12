# Transitive Effects

This is the implementation plan for adding **transitive side-effect tracking** to every function and modifier's `FunctionModProperties`. For each of the four direct-effect fields the analyzer already populates (`reverts`, `mutations`, `reads`, `events_emitted`), we add a sibling `effective_*` field carrying the transitive union — what the function can raise, write, read, or emit not just directly but through everything it (transitively) calls. The goal is an auditor-facing view in which a function's signature truthfully advertises every effect the function can transitively cause.

Read `pipeline-dag.md` first; this doc assumes its call-DAG / SCC vocabulary is familiar.

Phase 0 of this plan (MemberAccess fix) is already landed. The remaining prerequisites are in place: `CallInfo` (with `in_try_block: bool`) has replaced `Vec<Topic>` for `FunctionModProperties.calls`, `try_call` has been removed from `TypeConversion` / `StructConstructor` AST variants (it remains only on `FunctionCall` where the Solidity grammar admits it), and the analyzer captures every callable expression shape — Identifier, MemberAccess, IdentifierPath, FunctionCallOptions — into `function_calls`.

## Summary of decisions

These were settled during design and should not be re-litigated during implementation:

- **All four effective fields live on `FunctionModProperties`, not in a sibling map.** The existing direct fields (`reverts`, `mutations`, `reads`, `events_emitted`, `calls`) are already filtered/deduped/proxy-resolved derived data, not raw AST. The transitive companions fit the same shape and the same place. One struct, one lookup, one serialized payload.
- **Reverts and non-revert effects use *different propagation graphs*.** Try/catch catches reverts but does **not** suppress state mutations, state reads, or events emitted by a successful callee — those flow through the call regardless of whether it's try-wrapped. So:
  - **Reverts** propagate over the *non-try* call graph (call sites with `in_try_block == false` only). This is "the propagation graph" referred to elsewhere — try-call edges are dropped.
  - **Mutations, reads, events** propagate over the *full* call graph (all call edges, try or not). A try-call to a callee that mutates state still contributes that mutation to the caller's effective set; whether the call ultimately succeeds is a runtime question, but the *possibility* is what the auditor view tracks.

  This asymmetry is load-bearing. It shapes the algorithm (two SCC passes, not one), the test matrix, and the worked example below.
- **Two SCC computations, one set of folds.** Because the two propagation graphs differ, their SCC structures legitimately differ across them too — same nodes, different edge subsets, different cycle membership. Each graph gets its own Tarjan run; the per-effect folds reuse the appropriate ordering. Same algorithm, same dedup pattern; what varies per effect is the graph, the direct-accessor, the lift-to-effective function, and the dedup key.
- **One shared `EffectiveTopic` type covers mutations, reads, and events.** All three reduce to "topic + origin": which state variable / event is touched, and which function/modifier directly touches it. `EffectiveRevert` stays separate — it carries `RevertInfo`'s richer structure (kind, error_topic, statement node), and that structure doesn't generalize.
- **Tarjan SCC + reverse-topological fold, in a new submodule.** Implemented self-contained in `crates/o11a-analyze/src/solidity/effective_properties.rs`. Not shared with `function_dag.rs` — that module's SCC pass is wired into batch-building infrastructure; reusing it would require a generic graph utility larger than this work needs. The iterative Tarjan implementation at `function_dag.rs:152` is the verbatim template to copy.
- **The fold runs at the tail of the Solidity analyzer pass.** After `second_pass(...)` returns, before any downstream consumer (resolution graph extractor, pipeline steps) reads `function_properties`. Both `function_properties` and `topic_metadata` are then fully populated; proxies are resolvable.
- **Per-call-site flow is derived at render time, not pre-computed.** Given a `CallInfo`, the propagated/caught partition for any effect is a lookup on `effective_*[resolve(callee)]`. Storing this redundantly per site would double the data with no readers.
- **Resolution graph emits transitive `ErrorThrown`, `WritesState`, and `EventEmitted` edges in the same commit as the fold.** The graph and the properties are two views of the same data; landing them in lockstep prevents an intermediate state where one source disagrees with the other. State reads do *not* get a new edge type — see Phase 3 for the decision note.
- **Cached audits regenerate via `ARTIFACT_SCHEMA_VERSION` bump.** Stored caches from before this work are invalid and the schema-version gate rejects them on load. The four new fields all carry `#[serde(default)]` (matching the existing pattern on `reads` / `events_emitted`) so any in-process deserialization that skips the version gate still works — that's a deserialization-tolerance concern, separate from cache invalidation.

## What you will build

Three commits, the first of which is already landed:

**Precursor — MemberAccess fix (Phase 0, already done).** Restructures the `FunctionCall` arm of `collect_references_and_statements` so the call-target expression is handled explicitly, regardless of whether it is `Identifier`, `MemberAccess`, `IdentifierPath`, `FunctionCallOptions` (recurse), or `NewExpression`. The fix also routes the callee out of `variable_reads` (the misrouting only escaped notice today because the consumer filters to state variables). With Phase 0 in place, every callable expression shape contributes to `function_calls` and try-call sites correctly carry `in_try_block: true`. The rest of this plan builds on that foundation.

**Main commit — Fold + graph (Phases 1–3).** Adds `EffectiveRevert`, `EffectiveTopic`, and four new fields on both `FunctionModProperties::FunctionProperties` and `FunctionModProperties::ModifierProperties`:

- `effective_reverts: Vec<EffectiveRevert>` — transitive reverts (over the non-try propagation graph).
- `effective_mutations: Vec<EffectiveTopic>` — transitive state writes (over the full call graph).
- `effective_reads: Vec<EffectiveTopic>` — transitive state reads (over the full call graph).
- `effective_events_emitted: Vec<EffectiveTopic>` — transitive event emissions (over the full call graph).

New module `effective_properties.rs` implements both SCC passes (one per propagation graph) and four folds (one per effect kind), with shared helpers for adjacency building, Tarjan, and the per-SCC fold. The analyzer calls a single public entry point at the tail of the pass and patches all four fields per `function_properties` entry from the result. The resolution graph's existing `ErrorThrown` / `WritesState` / `EventEmitted` emission loops extend to also iterate the corresponding transitive sets.

**Renderers (Phase 4) — deferred to a follow-up.** Surfacing `reverts (...)`, `writes (...)`, `reads (...)`, `emits (...)`, and `handles (...)` clauses in the Solidity formatter, plus parallel `transitive_*` JSON fields in the agent context, is a separate additive PR. The data lands first; the renderers consume it.

Each phase is independently verifiable. Do not move on until the previous one compiles, its tests pass, and `cargo test --workspace` is green.

## Worked example (anchor for the rest of the doc)

Two functions, with both a direct revert and a direct state write on each, plus a cycle where one edge is try-wrapped:

```
A: directly raises RA, directly writes state X;  calls B inside a try { ... } catch
B: directly raises RB, directly writes state Y;  calls A WITHOUT try
```

Call graph: `A → B` (try-wrapped) and `B → A` (no try).

**Two propagation graphs** to consider:

| Graph | Edges | SCCs |
|---|---|---|
| Revert propagation (try edges dropped) | `B → A` only | `{A}`, `{B}` (no cycle) |
| Full call graph (used for mutations, reads, events) | `A → B`, `B → A` | `{A, B}` (single cycle) |

**Correct effective sets:**

| Function | `effective_reverts` | `effective_mutations` |
|---|---|---|
| A | `{(RA, origin=A)}` | `{(X, origin=A), (Y, origin=B)}` |
| B | `{(RB, origin=B), (RA, origin=A)}` | `{(Y, origin=B), (X, origin=A)}` |

Two things stand out:

1. **Reverts are asymmetric, state mutations are symmetric.** A's `effective_reverts` doesn't include `RB` because the call to B is in a try block (try catches reverts). But A's `effective_mutations` *does* include `Y` because if B's call succeeds, B's write to Y persists — try doesn't suppress that. The asymmetry is real and intentional.
2. **A "fold per call-graph SCC" would get reverts wrong.** It would lump A and B together in one SCC, giving A and B the same `effective_reverts`, which is incorrect — A's try block legitimately breaks the cycle for revert propagation. The non-try propagation graph computes SCCs over a different edge set and gets the right answer.

**This contrast is the load-bearing reason for two separate propagation graphs and two separate Tarjan runs.** Test cases in Phase 2 lock both halves of the contrast down explicitly.

## Phase 0 — MemberAccess fix (precursor) — **ALREADY LANDED**

> This phase is **complete** and on the main branch. The section is preserved as historical record for context — do not re-implement. If you're a lower-reasoning agent picking up this plan, start at **Phase 1**.

### Goal

Make `FunctionModProperties.calls` capture every call site whose callee is resolvable, regardless of whether the call expression is `Identifier`, `MemberAccess`, `IdentifierPath`, `FunctionCallOptions`, or `NewExpression`. Also stop misrouting function-callee MemberAccess into `variable_reads`. Self-contained; no fold yet.

The pre-fix state: the analyzer only captured `Identifier`-expression callees (e.g., `foo()`). External and qualified calls (`obj.foo()`, `Lib.foo()`, `obj.foo{value: x}()`, `new C()`) were missing from `function_calls`, and the function-callee MemberAccess was pushed into `variable_reads` by the generic recursion (the agent-context consumer filters reads to `NamedTopicKind::StateVariable`, so the bug was masked — but the data was wrong).

This phase ended with `cargo build --workspace` and `cargo test --workspace` green.

### Files to change

**`crates/o11a-analyze/src/solidity/analyzer.rs`**

1. **Add a callee-extraction helper** near the existing `extract_referenced_declaration` (grep for that name; current location around `analyzer.rs:2758`). Mirror its shape:

   ```rust
   /// Resolve a `FunctionCall.expression` to the callee's
   /// `referenced_declaration` node id. Returns `None` for expression
   /// kinds that are not direct callables (e.g., TypeConversion,
   /// NewExpression — see note below). For `FunctionCallOptions` the
   /// unwrapping is recursive — `obj.foo{...}()` nests a MemberAccess
   /// inside FunctionCallOptions inside FunctionCall.
   fn callee_from_call_expression(expr: &ASTNode) -> Option<i32> {
     match expr {
       ASTNode::Identifier { referenced_declaration, .. }
       | ASTNode::IdentifierPath { referenced_declaration, .. } => {
         Some(*referenced_declaration)
       }
       ASTNode::MemberAccess { referenced_declaration: Some(rd), .. } => {
         Some(*rd)
       }
       ASTNode::FunctionCallOptions { expression, .. } => {
         callee_from_call_expression(expression)
       }
       _ => None,
     }
   }
   ```

   **NewExpression is deliberately not handled.** A `new C()` call's `FunctionCall.expression` is a `NewExpression` whose `type_name` is a `UserDefinedTypeName` pointing at the contract declaration, not at C's constructor function. Resolving "contract → constructor function" is non-trivial (constructors are tracked under their own function topic, distinct from the contract topic), and contract decls are not entries in `function_properties` so the fold would drop them anyway. Constructor-call tracking is intentionally out-of-scope for this work; leave a `// TODO: constructor calls — out of scope, see transitive-effects.md` comment in the `_ => None` arm so the omission is discoverable.

2. **Restructure the `FunctionCall` arm of `collect_references_and_statements`.** Anchor: grep for `// Function calls - check for require()/revert()`. The current arm handles `expression: Identifier` and falls through to the generic child-recursion loop at the bottom of the function (grep `// Continue traversing child nodes`). After the rewrite, the arm must:

   - Take ownership of the call-target walk so the generic recursion does not also visit it. Mirror the pattern in `walk_call_skipping_callee` (defined just below the match for revert/emit statements).
   - Handle the special-case `require` / `revert` Identifier callees first, exactly as today.
   - Otherwise, use `callee_from_call_expression(expression)` to obtain the callee; push a `FirstPassCall { call_node: *node_id, callee_node, in_try_block: *try_call }` if `Some`.
   - Walk the receiver of MemberAccess callees (the `expression` field inside the MemberAccess) through the normal walker so `obj` in `obj.foo()` still flows into `referenced_nodes` / `variable_reads`. The MemberAccess's *own* `referenced_declaration` (the callee) must NOT be re-walked — it's already in `function_calls`.
   - For `FunctionCallOptions` callees, walk the option arguments (`{value: x, gas: y}`) so any topics referenced in option values still flow through.
   - Walk arguments normally (existing behavior).
   - Walk the return-decl references (existing behavior).
   - `return` from the match arm so the generic child-recursion below does not re-walk anything.

   Concretely, lift the arguments-and-return-decl bookkeeping out of the FunctionCall arm and into a new helper `walk_call_children`, callable from both the FunctionCall arm and (after) `walk_call_skipping_callee`. Or inline; reviewer's choice. The invariant is: every child of the FunctionCall is walked exactly once, with the callee landing in `function_calls` and never in `variable_reads`.

3. **Leave the standalone `MemberAccess` arm alone** (grep `// Member access (e.g., EnumType.Value`). It still handles MemberAccess that appears as a value (`s.balance` read in an expression context). The fix above ensures the function-callee case never reaches this arm because the FunctionCall arm now consumes it.

4. **Update tests** under the existing first-pass test module (grep `fn run_visitor_full`). Add cases — model on the existing `collect_revert_does_not_pollute_function_calls` test:

   - `obj.foo()` (MemberAccess callee) → `function_calls` has one entry with `callee_node = foo_id`; `variable_reads` does NOT contain `foo_id`; `variable_reads` DOES contain `obj_id` (the receiver is a normal read).
   - `Lib.foo()` (MemberAccess on a contract Identifier) → `function_calls` has `foo_id`; `Lib` should appear in `referenced_nodes` but not `variable_reads` (it's a contract reference, not a state variable). The existing logic already handles contract refs; verify the test.
   - `obj.foo{value: x}()` (FunctionCallOptions wrapping MemberAccess) → `function_calls` has `foo_id`; `x` appears as a read via the option-argument walk.
   - `try obj.foo() { ... } catch { ... }` → `function_calls` has one entry with `in_try_block: true`. The existing AST shape for TryStatement is `external_call: Box<ASTNode>` pointing at the same FunctionCall node that Solidity flags with `try_call: true`; no special handling needed beyond the existing destructure.
   - Chained call `a.b().c()` → `function_calls` has two entries: `b_id` and `c_id`. (The inner FunctionCall is the expression-side of the MemberAccess in the outer FunctionCall; the walker visits both as separate FunctionCall nodes.)
   - `new MyContract(arg)` → `function_calls` has **zero entries** (NewExpression intentionally unhandled, see the helper above). Argument expressions in the `arg` position still flow through arguments-walking as references. Lock this no-op behavior down with the test so a future contributor sees the intentional gap if they try to add it.
   - Existing tests under `collect_references_and_statements` should all continue to pass without modification — none of them probe MemberAccess-callee paths, so the new behavior is strictly additive.

### How to verify Phase 0

- `cargo build --workspace` clean, no new warnings.
- `cargo test --workspace` all green.
- New unit tests above all pass.
- `grep -rn "obj\.foo" crates/o11a-analyze/src/solidity/analyzer.rs` shows the new tests exist.
- Smoke run on a reference audit: `function_dag.rs`'s `build_call_edges` (which iterates `props.calls`) now produces additional edges for external calls. Pre-existing DAG-based snapshot tests may shift — re-baseline if so. The pipeline batching order can legitimately change because more edges mean tighter dependency ordering; that is the intended outcome.

### Pivotal decisions

- **The FunctionCall arm takes ownership of the call-target walk.** Letting the generic recursion descend into `expression` causes the MemberAccess arm to push the callee into `variable_reads`. Returning from the arm after explicit walking is the only way to suppress the misroute.
- **`callee_from_call_expression` returns `Option<i32>`, not a result.** Unresolvable callees (compiler couldn't resolve, or expression kind not callable) drop the site silently. The walker's invariant is "best-effort capture of resolvable callees"; failing loudly here would block analysis of pathological-but-legal Solidity that the analyzer already runs against.
- **NewExpression is intentionally not tracked in v1.** Constructor calls *can* revert, but `NewExpression.type_name` points at the contract declaration (a `UserDefinedTypeName.referenced_declaration` deeper inside), not at the constructor function topic that `function_properties` indexes. Resolving "contract → constructor" requires logic this work doesn't need elsewhere. Document the gap with a TODO comment and revisit when constructor-call propagation matters for an audit.

## Phase 1 — Domain types

### Goal

Add two new types (`EffectiveRevert`, `EffectiveTopic`) and four new fields on both variants of `FunctionModProperties`. Initialize empty everywhere `FunctionModProperties` is constructed; consumers handle the empty case as "no transitive entries known yet" (correct pre-fold).

### Files to change

**`crates/o11a-core/src/domain/mod.rs`**

1. **Add `EffectiveRevert` and `EffectiveTopic` next to `RevertInfo` and `CallInfo`** (grep `pub struct RevertInfo`):

   ```rust
   /// One revert that a function can transitively raise. Produced by
   /// the bottom-up fold in `effective_properties.rs`. `origin` is the
   /// function or modifier whose body directly raises `revert` — i.e.,
   /// the leaf of the propagation chain. The intermediate call path is
   /// not stored; it can be reconstructed from the call graph if a
   /// render site needs it, and storing one canonical "via" hop would
   /// lose information when two paths converge.
   #[derive(Debug, Clone, Serialize, Deserialize)]
   pub struct EffectiveRevert {
     pub revert: RevertInfo,
     pub origin: topic::Topic,
   }

   /// One transitive non-revert side-effect entry — a state-variable
   /// access (read or write) or an event emission. Shared across the
   /// three `effective_mutations` / `effective_reads` /
   /// `effective_events_emitted` fields. `topic` is the state variable
   /// or event being referenced; `origin` is the function/modifier
   /// whose body directly triggers it (the leaf of the propagation
   /// chain). Same not-storing-path rationale as `EffectiveRevert`.
   #[derive(Debug, Clone, Serialize, Deserialize)]
   pub struct EffectiveTopic {
     pub topic: topic::Topic,
     pub origin: topic::Topic,
   }
   ```

2. **Add four fields to both `FunctionModProperties` variants** (grep `pub enum FunctionModProperties`). Each effective field sits immediately after its direct counterpart so the variant reads as paired direct/effective rows:

   ```rust
   pub enum FunctionModProperties {
     FunctionProperties {
       reverts: Vec<RevertInfo>,
       /// Transitive union of `reverts` plus the `effective_reverts` of
       /// every non-try callee (resolved through proxies). Computed
       /// over the *non-try propagation graph* — try-call sites are
       /// excluded because try/catch absorbs them. See
       /// `effective_properties::compute_transitive_effects`.
       #[serde(default)]
       effective_reverts: Vec<EffectiveRevert>,
       calls: Vec<CallInfo>,
       mutations: Vec<topic::Topic>,
       /// Transitive union of `mutations` plus the `effective_mutations`
       /// of every callee (resolved through proxies). Computed over
       /// the *full call graph* — try-call sites are INCLUDED, since
       /// try/catch doesn't suppress state changes from successful
       /// callees, only catches reverts from failing ones.
       #[serde(default)]
       effective_mutations: Vec<EffectiveTopic>,
       reads: Vec<topic::Topic>,
       /// Transitive union of `reads` plus the `effective_reads` of
       /// every callee. Same graph and rationale as
       /// `effective_mutations` — try doesn't suppress reads from a
       /// successful callee.
       #[serde(default)]
       effective_reads: Vec<EffectiveTopic>,
       events_emitted: Vec<topic::Topic>,
       /// Transitive union of `events_emitted` plus the
       /// `effective_events_emitted` of every callee. Same graph and
       /// rationale as `effective_mutations` — try doesn't suppress
       /// events from a successful callee.
       #[serde(default)]
       effective_events_emitted: Vec<EffectiveTopic>,
     },
     ModifierProperties {
       reverts: Vec<RevertInfo>,
       #[serde(default)]
       effective_reverts: Vec<EffectiveRevert>,
       calls: Vec<CallInfo>,
       mutations: Vec<topic::Topic>,
       #[serde(default)]
       effective_mutations: Vec<EffectiveTopic>,
       reads: Vec<topic::Topic>,
       #[serde(default)]
       effective_reads: Vec<EffectiveTopic>,
       events_emitted: Vec<topic::Topic>,
       #[serde(default)]
       effective_events_emitted: Vec<EffectiveTopic>,
     },
   }
   ```

   `#[serde(default)]` matches the existing pattern on `reads` and `events_emitted` (same struct) and keeps the legacy-deserialize test at `domain/mod.rs` (grep `legacy_modifier`) passing without modification. Cache invalidation is handled separately by the `ARTIFACT_SCHEMA_VERSION` bump; serde defaults are about deserialization tolerance, not staleness detection.

**`crates/o11a-core/src/analysis_artifact.rs`**

3. **Bump `ARTIFACT_SCHEMA_VERSION` from `2` to `3`** (grep `ARTIFACT_SCHEMA_VERSION:`). The doc comment on the constant requires a bump for any breaking change to `AuditDataSnapshot`. Adding new non-defaulted fields to a serialized struct (transitively via `function_properties`) is breaking; even though serde defaults make deserialization tolerant, the schema bump is the authoritative signal that downstream tools should regenerate.

**`crates/o11a-analyze/src/solidity/analyzer.rs`**

4. **Initialize all four fields to `vec![]` at the two `FunctionModProperties` construction sites.** Grep `FunctionModProperties::FunctionProperties {` and `FunctionModProperties::ModifierProperties {` inside `analyzer.rs` — there is exactly one of each, inside `second_pass`, immediately next to the existing `calls: call_infos` line. Match the ordering of fields in the enum definition (effective field immediately after its direct counterpart). The fold populates these in Phase 2; here we just need the fields present.

**Workspace-wide test fixtures**

5. **Initialize all four effective fields to `vec![]` in every test fixture that constructs `FunctionModProperties::FunctionProperties` or `FunctionModProperties::ModifierProperties` directly.** Run `grep -rn "FunctionModProperties::FunctionProperties {" crates/` to enumerate. Sites include:

   - `crates/o11a-core/src/collaborator/agent/context.rs` — several test sites (state-var semantics, called-function behaviors, etc.).
   - `crates/o11a-core/src/resolution_graph/solidity_extractor.rs` — inside `insert_function_props` test helper (one site that all extractor tests funnel through).

   Each gets four new `effective_*: vec![]` lines. The compiler will flag every site you miss via "missing field" errors — let it.

### How to verify Phase 1

- `cargo build --workspace` clean. Missing-field errors point you at every construction site.
- `cargo test --workspace` all green. No behavior change — every effective field is empty everywhere.
- `grep -rn "effective_reverts\|effective_mutations\|effective_reads\|effective_events_emitted" crates/` shows all four fields threaded through.
- `ARTIFACT_SCHEMA_VERSION` is `3`.

### Pivotal decisions

- **Four fields on `FunctionModProperties`, not a sibling map and not a single composite struct.** Each direct effect already has a top-level field on this enum; the transitive companions parallel exactly. Bundling them into a substruct would force consumers to learn a second indirection for what's structurally one row.
- **`EffectiveTopic` is shared across mutations, reads, and events.** The three reduce to "topic + origin" and don't benefit from per-kind types. `EffectiveRevert` stays distinct because `RevertInfo` carries richer structure (kind, error_topic, statement node).
- **`#[serde(default)]` on all four new fields; cache invalidation via `ARTIFACT_SCHEMA_VERSION`.** The two mechanisms address different problems. Serde-default makes deserialization tolerant of missing fields in any incoming payload; the schema-version bump rejects entire artifacts whose top-level version doesn't match. Stale caches fail at the version check before deserialization runs.

## Phase 2 — Fold pass + analyzer wire-up

### Goal

Implement two SCC passes (one per propagation graph) and four folds (one per effect kind) in a new module `effective_properties.rs`. Call a single public entry point at the tail of the Solidity analyzer pass; patch all four `effective_*` fields per `function_properties` entry from the result.

### Files to change

**`crates/o11a-analyze/src/solidity/effective_properties.rs` (new file)**

1. **Module declaration.** Add `pub mod effective_properties;` to `crates/o11a-analyze/src/solidity/mod.rs`.

2. **Imports at the top of the new file.** Mirror analyzer.rs's import style:

   ```rust
   use o11a_core::domain::{
     topic, EffectiveRevert, EffectiveTopic, FunctionModProperties,
     TopicMetadata,
   };
   use std::collections::{BTreeMap, BTreeSet, HashMap};
   ```

3. **Public output type and entry point.** One call produces all four maps:

   ```rust
   pub struct TransitiveEffects {
     pub reverts: HashMap<topic::Topic, Vec<EffectiveRevert>>,
     pub mutations: HashMap<topic::Topic, Vec<EffectiveTopic>>,
     pub reads: HashMap<topic::Topic, Vec<EffectiveTopic>>,
     pub events_emitted: HashMap<topic::Topic, Vec<EffectiveTopic>>,
   }

   pub fn compute_transitive_effects(
     function_properties: &BTreeMap<topic::Topic, FunctionModProperties>,
     topic_metadata: &BTreeMap<topic::Topic, TopicMetadata>,
   ) -> TransitiveEffects;
   ```

   Callers patch each `function_properties` entry from this struct (see step 9 below). Returning four maps in one struct keeps the public surface to one call and one borrow pair on the analyzer side.

4. **Step A — `build_call_adjacency` helper.** One helper, parameterized by whether to include try edges. Builds the adjacency map used by both SCC passes:

   ```rust
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
           continue; // try edges drop for the revert graph only
         }
         // Resolve proxy → impl. Match function_dag.rs:122-127.
         let resolved = topic_metadata
           .get(&call.callee)
           .and_then(|m| m.transitive_topic())
           .copied()
           .unwrap_or(call.callee);
         if function_properties.contains_key(&resolved) {
           callees.push(resolved);
         }
         // Out-of-scope callees are dropped — same convention as
         // function_dag.rs build_call_edges.
       }
       callees.sort();
       callees.dedup();
       adj.insert(*caller, callees);
     }
     adj
   }
   ```

   Two calls in the entry point:
   - `build_call_adjacency(.., .., include_try_edges = false)` → the **revert propagation graph**.
   - `build_call_adjacency(.., .., include_try_edges = true)` → the **full call graph** (mutations / reads / events).

5. **Step B — Tarjan SCC.** **An iterative, topic-keyed Tarjan implementation already exists in this repo at `crates/o11a-core/src/collaborator/agent/function_dag.rs:152` (`fn tarjan_scc`).** Copy it verbatim into the new module — it takes `(&BTreeSet<Topic>, &HashMap<Topic, Vec<Topic>>)` and returns `Vec<Vec<topic::Topic>>` in reverse topological order, which is exactly what each fold needs.

   Do not import directly from `function_dag.rs` — that module is in `o11a-core` and we're in `o11a-analyze`; reach-across crate dependencies for one function is not worth it, and the algorithm is short enough to inline. Copying is the explicit choice. (If a future generic graph utility lands, both call sites can collapse to it.)

   Pair the copied `tarjan_scc` with a small post-processing helper that maps each topic to its SCC index — the fold needs this lookup to identify inside-SCC vs outside-SCC edges:

   ```rust
   fn build_scc_index(sccs: &[Vec<topic::Topic>]) -> HashMap<topic::Topic, usize> {
     let mut map = HashMap::new();
     for (idx, scc) in sccs.iter().enumerate() {
       for member in scc {
         map.insert(*member, idx);
       }
     }
     map
   }
   ```

   Run Tarjan **twice**: once on the revert adjacency, once on the full-call adjacency. Each pass produces `(sccs, scc_id)`. The two outputs are independent; the SCC index spaces are not interchangeable.

6. **Step C — `fold_per_scc` generic helper.** The fold structure is identical for every effect; what varies is the direct accessor (which field of `FunctionModProperties` to read), the lift function (how to wrap each direct entry into an Effective entry with `origin`), and the dedup key. Capture this once:

   ```rust
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

       // 1. Every SCC member's direct entries become Effective entries
       //    with origin = the member.
       for member in scc {
         let Some(props) = function_properties.get(member) else { continue };
         for d in direct(props) {
           union.push(lift(d, *member));
         }
       }

       // 2. For every outgoing propagation edge from any SCC member that
       //    lands OUTSIDE this SCC, fold in the callee's already-computed
       //    effective entries. Inside-SCC edges contribute nothing —
       //    every member's direct entries are already in `union`.
       for member in scc {
         let Some(callees) = adj.get(member) else { continue };
         for callee in callees {
           if scc_id.get(callee).copied() == Some(scc_idx) { continue; }
           if let Some(callee_eff) = effective.get(callee) {
             union.extend(callee_eff.iter().cloned());
           }
         }
       }

       // 3. Dedup using the per-effect key.
       union.sort_by(|a, b| dedup_key(a).cmp(&dedup_key(b)));
       union.dedup_by(|a, b| dedup_key(a) == dedup_key(b));

       // 4. Every SCC member receives the union.
       for member in scc {
         effective.insert(*member, union.clone());
       }
     }
     effective
   }
   ```

   This is the same algorithm the revert fold uses — abstracted so the four effects share one implementation.

7. **Step D — Call the fold four times.** Inside `compute_transitive_effects`:

   ```rust
   let revert_adj = build_call_adjacency(function_properties, topic_metadata, /*include_try_edges=*/ false);
   let full_adj   = build_call_adjacency(function_properties, topic_metadata, /*include_try_edges=*/ true);

   let revert_nodes: BTreeSet<topic::Topic> = revert_adj.keys().copied().collect();
   let full_nodes:   BTreeSet<topic::Topic> = full_adj.keys().copied().collect();

   let revert_sccs   = tarjan_scc(&revert_nodes, &revert_adj);
   let revert_scc_id = build_scc_index(&revert_sccs);
   let full_sccs     = tarjan_scc(&full_nodes, &full_adj);
   let full_scc_id   = build_scc_index(&full_sccs);

   let reverts = fold_per_scc(
     function_properties, &revert_sccs, &revert_scc_id, &revert_adj,
     |props| match props {
       FunctionModProperties::FunctionProperties { reverts, .. }
       | FunctionModProperties::ModifierProperties { reverts, .. } => reverts.as_slice(),
     },
     |r, origin| EffectiveRevert { revert: r.clone(), origin },
     // Custom errors dedup by error_topic; bare requires by statement node.
     |e| (e.origin, e.revert.kind, e.revert.error_topic.unwrap_or(e.revert.topic)),
   );

   let mutations = fold_per_scc(
     function_properties, &full_sccs, &full_scc_id, &full_adj,
     |props| match props {
       FunctionModProperties::FunctionProperties { mutations, .. }
       | FunctionModProperties::ModifierProperties { mutations, .. } => mutations.as_slice(),
     },
     |t, origin| EffectiveTopic { topic: *t, origin },
     |e| (e.origin, e.topic),
   );

   let reads = fold_per_scc(
     function_properties, &full_sccs, &full_scc_id, &full_adj,
     |props| match props {
       FunctionModProperties::FunctionProperties { reads, .. }
       | FunctionModProperties::ModifierProperties { reads, .. } => reads.as_slice(),
     },
     |t, origin| EffectiveTopic { topic: *t, origin },
     |e| (e.origin, e.topic),
   );

   let events_emitted = fold_per_scc(
     function_properties, &full_sccs, &full_scc_id, &full_adj,
     |props| match props {
       FunctionModProperties::FunctionProperties { events_emitted, .. }
       | FunctionModProperties::ModifierProperties { events_emitted, .. } => events_emitted.as_slice(),
     },
     |t, origin| EffectiveTopic { topic: *t, origin },
     |e| (e.origin, e.topic),
   );

   TransitiveEffects { reverts, mutations, reads, events_emitted }
   ```

   Note that mutations / reads / events all share `full_sccs` and `full_adj` — three folds, one SCC pass. Reverts get their own SCC pass.

**`crates/o11a-analyze/src/solidity/analyzer.rs`**

8. **Add the use-import.** At the top of the file:

   ```rust
   use crate::solidity::effective_properties;
   ```

9. **Invoke the fold at the tail of the analyzer pass.** The analyzer's entry point is `pub fn analyze(project_root, audit_id, data_context) -> Result<(), String>` (top of `analyzer.rs`; grep `pub fn analyze`). It mutates `audit_data` (obtained from `data_context.get_audit_mut(audit_id)`) in place; there is no `AuditData` value constructed at the end. `function_properties` lives at `audit_data.function_properties` (a `BTreeMap`); `second_pass` populates it via a `&mut` borrow.

   The fold must run **after** `second_pass(...)` returns (every `function_properties` and `topic_metadata` entry is now present) and **before** `analyze` returns (so downstream consumers see populated data). The current line position is right after `second_pass(...)` returns and before the "Insert ASTs with stubbed nodes" loop; grep `Insert ASTs with stubbed nodes` to find the comment anchor. Insert:

   ```rust
   // Compute transitive effects per function/modifier. Runs after
   // second_pass populates function_properties and topic_metadata,
   // before downstream consumers (resolution graph extractor,
   // pipeline steps) read the data.
   let transitive = effective_properties::compute_transitive_effects(
     &audit_data.function_properties,
     &audit_data.topic_metadata,
   );
   for (topic, props) in audit_data.function_properties.iter_mut() {
     let new_reverts = transitive.reverts.get(topic).cloned().unwrap_or_default();
     let new_mutations = transitive.mutations.get(topic).cloned().unwrap_or_default();
     let new_reads = transitive.reads.get(topic).cloned().unwrap_or_default();
     let new_events = transitive.events_emitted.get(topic).cloned().unwrap_or_default();
     match props {
       FunctionModProperties::FunctionProperties {
         effective_reverts,
         effective_mutations,
         effective_reads,
         effective_events_emitted,
         ..
       }
       | FunctionModProperties::ModifierProperties {
         effective_reverts,
         effective_mutations,
         effective_reads,
         effective_events_emitted,
         ..
       } => {
         *effective_reverts = new_reverts;
         *effective_mutations = new_mutations;
         *effective_reads = new_reads;
         *effective_events_emitted = new_events;
       }
     }
   }
   ```

   The immutable borrows of `function_properties` and `topic_metadata` end at the semicolon after `compute_transitive_effects(...)`; the subsequent `iter_mut()` on `function_properties` starts a fresh mutable borrow. If borrow-check complains, the most likely cause is misreading the fold signature as taking `&mut` instead of `&`.

10. **Unit tests in `effective_properties.rs`.** Write them against direct calls to `compute_transitive_effects` with hand-built `BTreeMap<topic::Topic, FunctionModProperties>` and `BTreeMap<topic::Topic, TopicMetadata>` maps — no `AuditData` involved. `crates/o11a-core/src/resolution_graph/solidity_extractor.rs::insert_function_props` is loose inspiration for how to construct the `FunctionModProperties` shape; the test fixtures here are smaller (they don't need AuditData).

   **Revert cases** — exercise the non-try propagation graph:

   - **Direct only.** One function, one direct revert, no calls → its `effective_reverts` is one entry with `origin = self`.
   - **One hop, no try.** A calls B; B raises X → `A.effective_reverts ⊇ {(X, origin=B)}`. A's own direct reverts are also present if any.
   - **Two hops, no try.** A → B → C; C raises X → `A.effective_reverts ⊇ {(X, origin=C)}`.
   - **Try blocks exclusion.** A calls B with `in_try_block: true`; B raises X → `A.effective_reverts` excludes X. A's own direct reverts remain.
   - **Symmetric cycle, no try.** A ↔ B without try; A raises RA, B raises RB → both `A.effective_reverts` and `B.effective_reverts` are `{(RA, A), (RB, B)}` (same set). Locks down the same-SCC-shares-set invariant.
   - **Asymmetric try cycle (the worked example, revert half).** A try-calls B; B calls A without try. A raises RA, B raises RB → `A.effective_reverts = {(RA, A)}`; `B.effective_reverts = {(RB, B), (RA, A)}`. Asymmetric. Locks down the propagation-graph (not call-graph) SCC behavior.
   - **Dedup of custom errors across paths.** A calls B and C; both raise the same custom error E → `A.effective_reverts` has exactly one entry for E.
   - **No dedup of distinct bare requires.** A function with two bare `require(x, "first")` and `require(y, "second")` → both appear in its own `effective_reverts`, distinguished by `revert.topic` (the statement node).

   **Non-revert cases** — exercise the full-call-graph propagation (the contrast tests):

   - **Mutations propagate over try edges (and reverts do not — same setup).** A try-calls B; B writes state Y AND raises revert RB. Single test asserts both halves: `A.effective_mutations ⊇ {(Y, origin=B)}` (write propagates) AND `A.effective_reverts` does NOT contain `(RB, origin=B)` (revert is absorbed). Pairing the two assertions in one test makes the contrast undeniable.
   - **Reads propagate over try edges.** Same shape as mutations but with B reading state. Single-half test (no revert pairing needed — the mutations test already locks the contrast).
   - **Events propagate over try edges.** Same shape, B emitting an event. Single-half test.
   - **Asymmetric try cycle (the worked example, full).** A try-calls B; B calls A without try. A writes X, B writes Y. → Both `A.effective_mutations` and `B.effective_mutations` are `{(X, A), (Y, B)}` (symmetric — same full-call-graph SCC), while `effective_reverts` remains asymmetric (A's = `{(RA, A)}`, B's = `{(RB, B), (RA, A)}`). THE canonical test that locks the two-propagation-graph distinction — write it last; getting it right exercises every other piece.
   - **Dedup by (origin, topic).** A calls B and C, both write state Y → `A.effective_mutations` has exactly two entries: `(Y, origin=B)` and `(Y, origin=C)` (different origins stay distinct). A calls B twice (two call sites) → still one `(Y, origin=B)` entry (dedup'd by `(origin, topic)`).

   **Shared cases** — apply across all four effects:

   - **Proxy resolution.** A calls interface I; I.transitive_topic = Impl; Impl raises X, writes Y, emits E → `A.effective_reverts ⊇ {(X, origin=Impl)}` AND `A.effective_mutations ⊇ {(Y, origin=Impl)}` AND `A.effective_events_emitted ⊇ {(E, origin=Impl)}`. The origin is the implementation, not the interface.
   - **Out-of-scope callee.** A calls a callee whose topic is not in `function_properties` → no contribution to any effective set. (Conservative: out-of-scope callee may do anything, but we cannot reason about it. Same convention as `function_dag.rs`.)

### How to verify Phase 2

- `cargo build --workspace` clean.
- `cargo test --workspace` all green; new `effective_properties` tests all pass.
- Smoke run on a reference audit: serialize `audit_data` (or dump `function_properties` for a known function) and confirm all four `effective_*` fields are populated with sensible content. Specifically: pick a function known to call something that reverts AND writes state, and verify both flow up appropriately, with the try-call asymmetry visible in one but not the other.
- `grep -rn "effective_properties" crates/o11a-analyze/src/solidity/` shows the new module exists and the analyzer calls it.

### Pivotal decisions

- **Two propagation graphs.** The worked example up top is the reason. Reverts use the non-try graph; mutations/reads/events use the full call graph. Do not try to share a single SCC pass across both — the cycle membership legitimately differs.
- **Shared `fold_per_scc` generic helper.** The fold algorithm is identical across effects; the implementation captures that once with closures for the per-effect variation. Resist the temptation to write four hand-rolled folds — drift between them is exactly the kind of bug that's hard to catch.
- **`fold_per_scc`'s inside-SCC edge skip is correctness, not optimization.** Every member's direct entries are already in `union` via step 1, so re-adding via inside-SCC propagation would be a redundant pass through the same data with the same `origin` — dedup'd anyway, but skipping the work is explicit.
- **Origin is the function whose body directly triggers the effect.** Not the immediate caller, not the chain — the leaf. This is the only stable identity across SCC collapse and proxy resolution. The same convention applies to reverts, mutations, reads, and events.
- **Dedup keys differ per effect.** Reverts: `(origin, kind, error_topic.or(revert.topic))` — custom errors collapse across paths; bare requires distinguished by statement. Non-reverts: `(origin, topic)` — state vars and events are addressable singletons; one entry per (originating function, target) pair.

## Phase 3 — Resolution graph: transitive edges

### Goal

Extend the Solidity extractor's edge emission so `ErrorThrown`, `WritesState`, and `EventEmitted` are produced from the transitive sets in addition to the direct sets. The graph's "what can this function affect" view now matches the property's view. State reads remain untouched — see the decision note below.

### Files to change

**`crates/o11a-core/src/resolution_graph/solidity_extractor.rs`**

1. **Locate the existing emission loops.** Anchor: `fn extract_function_property_edges` (grep that name). The current shape destructures a 4-tuple and runs four loops (calls, mutations, events, reverts) against a shared `covered: BTreeSet` for dedup:

   ```rust
   let (calls, mutations, reverts, events) = match props {
     FunctionModProperties::FunctionProperties { calls, mutations, reverts, events_emitted, .. }
     | FunctionModProperties::ModifierProperties { calls, mutations, reverts, events_emitted, .. }
     => (calls, mutations, reverts, events_emitted),
   };
   // ... loops over calls, mutations, events, then reverts emitting ErrorThrown.
   ```

2. **Extend the destructure from a 4-tuple to a 7-tuple** so all three relevant effective fields are in scope (`effective_reads` is intentionally absent — see decision below). Note: the existing direct-events binding uses local name `events`; keep that for backward consistency with the existing loop bodies below. Effective-event binding uses `effective_events_emitted` (matches the field).

   ```rust
   let (calls, mutations, reverts, events,
        effective_reverts, effective_mutations, effective_events_emitted) = match props {
     FunctionModProperties::FunctionProperties {
       calls, mutations, reverts, events_emitted: events,
       effective_reverts, effective_mutations, effective_events_emitted, ..
     }
     | FunctionModProperties::ModifierProperties {
       calls, mutations, reverts, events_emitted: events,
       effective_reverts, effective_mutations, effective_events_emitted, ..
     } => (calls, mutations, reverts, events,
           effective_reverts, effective_mutations, effective_events_emitted),
   };
   ```

   The `events_emitted: events` syntax renames the field binding to a local name — explicit and clearer than the positional rename used previously.

3. **Add three transitive emission loops, each after its direct counterpart.** All three follow the same pattern: skip if the target is already in `covered` (to dedup against direct emission of the same target).

   After the existing direct-mutations loop:

   ```rust
   for e in effective_mutations {
     if covered.contains(&e.topic) { continue; }
     add_directed(audit_data, graph, emitted, *src, e.topic, EdgeType::WritesState);
     covered.insert(e.topic);
   }
   ```

   After the existing direct-events loop:

   ```rust
   for e in effective_events_emitted {
     if covered.contains(&e.topic) { continue; }
     add_directed(audit_data, graph, emitted, *src, e.topic, EdgeType::EventEmitted);
     covered.insert(e.topic);
   }
   ```

   After the existing direct-reverts loop:

   ```rust
   for e in effective_reverts {
     let Some(error_topic) = e.revert.error_topic else { continue };
     if covered.contains(&error_topic) { continue; }
     add_directed(audit_data, graph, emitted, *src, error_topic, EdgeType::ErrorThrown);
     covered.insert(error_topic);
   }
   ```

   The `covered` set is already in scope for the broader dedup pass that follows (against `topic_context` references). The three new loops feed into the same set; ordering relative to the existing loops doesn't matter for correctness, but mirroring direct-first/transitive-second keeps the file readable.

4. **Decisions: edge taxonomy unchanged.**

   - **Same `EdgeType::ErrorThrown` / `EdgeType::WritesState` / `EdgeType::EventEmitted` for direct and transitive.** No new variants. The graph's edge weight system can grow a "transitive" modifier later if a consumer needs to distinguish direct from indirect, but the semantic question of "can this function throw this error / write this state / emit this event" has the same answer in both cases. One edge type per effect, dedup'd, simplest.
   - **No `ReadsState` / `EffectiveReadsState` edge type.** State reads are inferred today via the broader `references` edges produced by the `topic_context` pass that follows this loop. The property field `effective_reads` carries the transitive view for renderers that want it, but the resolution graph's edge taxonomy stays as-is. Introducing a new edge type for transitive-only state reads would be incongruous (no direct counterpart exists either). Revisit if a consumer ever needs read-edges at all; the property is the source of truth.

5. **Update tests.** The existing tests around `function_calls_emit_directed_calls_edges` (grep that name) exercise direct emission via `insert_function_props`. Phase 1 already added hardcoded `effective_*: vec![]` initializations inside that helper. For the new Phase 3 tests:

   - **Extend `insert_function_props` with three new parameters** (`effective_reverts`, `effective_mutations`, `effective_events_emitted`), each `Vec<EffectiveRevert>` / `Vec<EffectiveTopic>` / `Vec<EffectiveTopic>` respectively, default-positioned at the end of the signature. Update every existing call site to pass `vec![]` for the three new parameters (mechanical; the compiler will list them). Single helper, three new parameters — avoids parallel test entry points and keeps fixture call shapes uniform.

   Required new test cases (one set per effect kind):

   **Transitive ErrorThrown:**
   - A function with one `effective_revert` (custom-error) and no direct reverts → one `ErrorThrown` edge to the error topic.
   - A function with both a direct and a transitive revert for the *same* error topic → exactly one `ErrorThrown` edge (dedup via `covered`).
   - A function with bare-revert (no `error_topic`) in `effective_reverts` → no edge emitted (skip on `None`).

   **Transitive WritesState:**
   - A function with one `effective_mutation` and no direct mutations → one `WritesState` edge to the state-var topic.
   - A function with both direct and transitive mutation of the *same* state var → exactly one `WritesState` edge.
   - Two `effective_mutations` entries with different origins but the *same* state-var topic → still only one `WritesState` edge (the graph deduplicates edges; the property's per-origin distinction is render-side only).

   **Transitive EventEmitted:**
   - A function with one `effective_events_emitted` and no direct emissions → one `EventEmitted` edge.
   - Both direct and transitive emission of the same event → exactly one edge.

   **No-new-edge for reads:** Confirm `ReadsState` does not exist in `EdgeType` (a `grep -rn "ReadsState" crates/o11a-core/` returning zero matches is the assertion). If someone later adds it without coordinating, this test fails loud.

### How to verify Phase 3

- `cargo build --workspace` clean.
- `cargo test --workspace` all green.
- Resolution graph snapshot tests, if any, may shift because functions now emit additional `ErrorThrown` / `WritesState` / `EventEmitted` edges for transitively-touched targets. Re-baseline if so; the additional edges are correct.

### Pivotal decisions

- **One `EdgeType` per effect covers direct and transitive.** No variant explosion until a consumer needs to filter.
- **No edge type for state reads — neither direct nor transitive.** The graph's read information lives in the generic `references` edges; introducing a `ReadsState` for the transitive case would be more inconsistency than improvement.
- **Dedup via the existing `covered` set, not parallel structures.** Adds three `if covered.contains` lines and keeps the existing reference-edge suppression below the new loops functioning.

## Phase 4 — Renderers (deferred to a follow-up plan)

Surfacing the four effective sets to auditors and the LLM is additive work that consumes the data Phases 0–3 land. It is **not** part of this plan. The follow-up touches:

- **Agent context** (`crates/o11a-core/src/collaborator/agent/context.rs`): the unified renderer's per-function envelope gains four parallel JSON fields:
  - `transitive_reverts` — from `effective_reverts`, parallel to the existing `reverts`.
  - `transitive_state_writes` — from `effective_mutations`, parallel to the existing `state_writes`.
  - `transitive_state_reads` — from `effective_reads`, parallel to the existing `state_reads`.
  - `transitive_events_emitted` — from `effective_events_emitted`, parallel to the existing `events_emitted`.

  Each list comes straight from `audit_data.function_properties[member].effective_*`. Whether caught reverts are included in the LLM context is a prompt-design question to settle in that PR.

- **Solidity formatter** (`crates/o11a-web-backend/src/solidity_formatter.rs`): synthetic clauses on function/modifier signatures, mirroring the Solidity grammar's `returns (...)` convention:

  ```solidity
  function transferBatch(uint[] ids) external
    reverts (NotAuthorized, InsufficientBalance)
    writes (balances, totalSupply)
    reads (paused, owner)
    emits (Transfer, ApprovalForAll)
  { ... }
  ```

  And at call sites: `reverts (...)`/`writes (...)`/etc. after each call expression, plus a `handles (...)` clause on try blocks (the caught-revert set, which never appears in the outer function's `reverts (...)` clause). The clause shape was prototyped during design. Resolving the names from topics requires `topic_metadata` lookup; the formatter already has that.

Defer until after Phases 0–3 are merged and validated end-to-end on a reference audit. The data must be trustworthy before renderers expose it.

## Out of scope

These are tracked decisions; do not build them in this work:

- **Per-call-site pre-computation of propagated/caught sets for any effect.** The view is derived at render time from `CallInfo.in_try_block` plus `effective_*[callee]`. Storing it per site would double the data with no readers, and the partition is trivial (a single boolean check per site).
- **Transitive `EdgeType` variants on the resolution graph.** One `ErrorThrown` / `WritesState` / `EventEmitted` edge type for both direct and transitive. Revisit only when a consumer needs the distinction.
- **`ReadsState` edge type (direct or transitive).** State reads are not part of the resolution graph's edge taxonomy today, and the transitive view doesn't change that. Renderers and other consumers read from `effective_reads` on the property directly.
- **Path information beyond `origin` on `EffectiveRevert` / `EffectiveTopic`.** No "via" chain, no intermediate hops. The propagation chain can be reconstructed from the call graph if a render site needs it.
- **Fold for Rust functions.** `crates/o11a-core/src/resolution_graph/rust_extractor.rs` handles Rust function properties; only the Solidity analyzer populates the effective fields in this work. If a Rust analyzer ever populates `FunctionModProperties` properly, it can call `effective_properties::compute_transitive_effects` — the module signature is language-agnostic.
- **Adversarial second pass / auditor approval flow for transitive effects.** All four are mechanically derived, not LLM-generated. No approval or correction surface is needed.
- **Re-derivation triggers.** When an auditor edits a function (adding/removing a revert, wrapping a call in try/catch, adding a state write, etc.), the effective fields will be stale. Recomputation is currently a full analyzer re-run. Incremental update is out of scope.

## Final verification

After all three phases land (Phase 0 as precursor commit, already done; Phases 1–3 as the main commit):

- `cargo build --workspace` clean, no new warnings.
- `cargo test --workspace` all green.
- `grep -rn "effective_reverts\|effective_mutations\|effective_reads\|effective_events_emitted" crates/` returns matches in:
  - `crates/o11a-core/src/domain/mod.rs` (four fields + `EffectiveRevert` + `EffectiveTopic`)
  - `crates/o11a-analyze/src/solidity/analyzer.rs` (initialization + fold invocation patching all four fields)
  - `crates/o11a-analyze/src/solidity/effective_properties.rs` (the fold itself)
  - `crates/o11a-core/src/resolution_graph/solidity_extractor.rs` (three transitive emission loops — reverts, mutations, events; no reads loop)
  - test fixtures across the workspace
- `grep -rn "ReadsState" crates/o11a-core/` returns zero matches (the no-new-edge decision is held).
- `grep -rn "callee_from_call_expression" crates/o11a-analyze/src/` returns matches in `analyzer.rs` only (Phase 0 already landed).
- `grep -rn "in_try_block: true" crates/o11a-analyze/` returns matches in the MemberAccess/try tests (Phase 0) AND the new asymmetric-cycle tests (Phase 2).
- `ARTIFACT_SCHEMA_VERSION` in `crates/o11a-core/src/analysis_artifact.rs` is `3`.
- Smoke run of the analyzer on a reference Solidity audit fixture:
  - Pick a function known to (a) call an in-scope function that reverts AND writes state, and (b) call something in a `try`/`catch` that reverts AND writes state. Confirm:
    - Its `effective_reverts` contains the propagated revert from (a), with `origin` pointing to the callee that raises it.
    - Its `effective_reverts` does NOT contain the revert from the try-wrapped callee (b) — try absorbs it.
    - Its `effective_mutations` contains state mutations from BOTH (a) and (b) — try does not absorb writes.
    - The resolution graph has `ErrorThrown` edges to the error topics from (a) (transitive, not caught) only, and `WritesState` edges to the state vars from both (a) and (b) (transitive, both propagate).
  - Pick a function with no calls and one `require` and one `emit Foo()`. Confirm `effective_reverts == [direct]` (one entry, `origin == self`), `effective_events_emitted == [direct]` likewise.
  - Pick a mutually-recursive function pair without try edges between them. Confirm both have the same `effective_reverts` set (comprising both members' direct reverts) AND the same `effective_mutations` set (comprising both members' direct writes). Both propagation graphs collapse this SCC the same way when no try edges are involved.

The renderer (Phase 4) is a separate plan; landing this work without it leaves all four effective fields populated on every function but not yet surfaced to humans or the LLM. That intermediate state is intentional and safe — the resolution graph already exposes three of the four (reverts, writes, events) via edges, and any downstream consumer can read `function_properties[fn].effective_*` directly.
