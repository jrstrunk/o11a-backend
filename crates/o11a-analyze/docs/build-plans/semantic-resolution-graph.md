# Semantic Resolution Graph — Build Plan

This document is the implementation plan for the semantic resolution graph described in `crates/o11a-analyze/docs/specs/semantic-resolution-graph.md`. It is organized as a sequence of self-contained agent prompts. Each phase's prompt block is intended to be handed verbatim (or near-verbatim) to a coding agent, with the spec attached for reference.

The plan is intentionally conservative: each phase produces a small, verifiable change and lands behind tests before the next phase begins. Phases must be completed in order unless an explicit "may run in parallel with" note is given.

---

## 1. Design reasoning summary

The current name resolver (`code_refs::find_declaration_by_name`, backed by `domain::TopicNameIndex`) maps a documentation inline-code identifier to a single topic by exact-name lookup. When multiple non-transitive declarations share a simple name, it gives up and the caller silently drops the resolution. This causes ~70% of unresolved code references in the nudgexyz audit and equivalent losses in NatSpec and inline-comment text.

The fix is **personalized PageRank over a typed weighted graph of audit declarations**, run per documentation section / per NatSpec block. The current resolver becomes Phase A (handles unambiguous cases). The graph is consulted only for ambiguous references.

The graph spans the entire audit (one structure for all source languages). Solidity is the only language extractor we build now; the extractor trait is designed so a Rust extractor can plug in later. Edge weights and PR parameters are language-agnostic constants — calibration via the existing comparison harness picks final values.

There are two consumer sites: the documentation analyzer (per-section graph scoring over the doc-tree header hierarchy) and the developer-documentation injector (per-NatSpec-block graph scoring over the source scope tree). Both use the same algorithm; the only difference is whether the "section ancestry" comes from the doc-tree header chain or from the source-tree scope chain. Downstream consumers (`mechanical_semantic_links`, etc.) read `referenced_topic` from identifier nodes and pick up the improved resolutions for free — no edits at those sites.

For full motivation and algorithm details, read the spec end-to-end. Each phase prompt links the relevant spec sections.

## 2. Guiding principles

These cross-cutting rules apply to every phase. Agents should re-read this section before starting work.

1. **Determinism is non-negotiable.** Same parsed `AuditData` produces the same resolutions byte-for-byte. Use `BTreeMap` not `HashMap` where iteration order is observed by the algorithm. Sort edge insertion by `(source_topic_id, dest_topic_id, edge_type)`. Sort seed vectors by topic ID. Run PageRank for a fixed number of iterations regardless of convergence. Break ties lexicographically (PR descending → qualified-name ascending → topic-ID ascending).

2. **Additive over rewriting.** Every analyzer-side data dependency is a new field on an existing struct or a new `BTreeMap` on `AuditData`. Do not reshape existing types. Do not change existing analyzer stages' outputs; only add new sinks alongside the existing ones.

3. **One mutation point per consumer.** The graph-driven resolution pass mutates `referenced_topic` on already-parsed identifier nodes. No downstream consumer needs to know the graph exists. If you find yourself editing `context::mechanical_semantic_links` or similar, stop and reconsider — that is out of scope.

4. **Do not pre-implement future calibration knobs.** The spec mentions per-sibling seeding, header-text seeding, transitive-`implements`-flattening, and similar as calibration knobs. None are part of the default. Do not build them speculatively.

5. **Stay inside the phase.** Each phase has explicit out-of-scope items. If a phase's tasks reveal a missing piece outside its scope, surface the gap (in the PR description or commit message); do not silently expand scope.

6. **Tests prove the determinism contract.** Every phase that produces graph state, edges, PR values, or resolutions must have a test that builds the same input twice and asserts byte-identical outputs.

7. **No `unsafe`. No external dependencies for graph or PR.** The graph and PageRank engine are pure Rust against existing crate dependencies.

8. **No flag toggles for incomplete features.** The spec mentions `--resolver=name-index|graph` for development comparison; that flag lives at the harness level (Phase 8). Internal resolver code does not branch on it.

9. **Agent self-check before merging.** Before declaring a phase complete, the agent must run `cargo test -p o11a-core -p o11a-analyze` and confirm all tests pass. Any new types must build under the existing `cargo clippy` lint set with no new warnings.

10. **Read before editing.** Read the listed context files in full at the start of each phase. The codebase has invariants that are not always documented; understanding them prevents 80% of the off-track failures.

## 3. Phase map

```
Phase 0  Data plumbing                     — independent, do first
Phase 1  Graph types + module skeleton     — depends on Phase 0
Phase 2  Solidity edge extractor           — depends on Phase 1
Phase 3  PageRank engine                   — depends on Phase 1; may run in parallel with Phase 2
Phase 4  Wire builder into analysis.rs     — depends on Phases 2 and 3
Phase 5  Relocate inject_developer_documentation + add candidates field
                                           — depends on Phase 4
Phase 6  Doc-tree resolution pass          — depends on Phase 5
Phase 7  Dev-doc resolution pass           — depends on Phase 5; may run in parallel with Phase 6
Phase 8  Harness `mechanical-graph` variant — depends on Phases 6 and 7
Phase 9  Phase C (co-location) + Phase D (re-iteration)
                                           — depends on Phase 8 (so improvements are measurable)
Phase 10 Phase E (anchor-by-name fallback) + candidates field population
                                           — depends on Phase 9
Phase 11 Operator inspection (resolution-graph and resolution-trace dumps)
                                           — depends on Phase 10
```

The Rust edge extractor (spec phase 8) is genuinely deferred until the Rust analyzer lands and is not in this build plan.

---

## Phase 0 — Data plumbing in `AuditData` and `TopicNameIndex`

### Goal

Add the analyzer-side data the future Solidity edge extractor will read, and the candidate-list accessor the future graph resolver will read. No graph code is added yet.

### Preconditions

None.

### Context

Spec sections to read: "Integration with the existing pipeline" → "What the graph replaces" and "Data dependencies — edges from existing analyzer state" in `crates/o11a-analyze/docs/specs/semantic-resolution-graph.md`.

Files to read in full before editing:

- `crates/o11a-core/src/domain/mod.rs` — particularly `TopicNameIndex` (around line 496), `AuditData` (around line 397), `FunctionModProperties` (around line 1909).
- `crates/o11a-analyze/src/solidity/analyzer.rs` — `analyze` function (around line 21), `first_pass` and its `FirstPassDeclaration::Contract` (around line 165), and the `second_pass` body that populates `FunctionModProperties` (search for `mutations.push` and `calls.push`).
- Search for `RevertInfo` definition and call sites; read everywhere it is constructed.

Key existing invariants:

- `TopicNameIndex::build` filters simple-name candidates down to the unique non-transitive match; the existing public methods `get_by_simple_name` and `get_by_qualified_name` return `Option<&Topic>`.
- `FirstPassDeclaration::Contract::base_contracts: Vec<ReferencedNode>` is built during `first_pass` and consumed by `tree_shake`. It is currently dropped after that.
- `FunctionModProperties` is an enum with two variants (`FunctionProperties`, `ModifierProperties`); each currently has `reverts: Vec<RevertInfo>`, `calls: Vec<Topic>`, `mutations: Vec<Topic>`.

### Tasks

1. **Add `candidates_by_simple_name` to `TopicNameIndex`.**
   - Internally retain the full pre-dedup candidate list keyed by simple name. The existing `Option`-returning `get_by_simple_name` keeps its semantics; the new accessor returns `&[Topic]` (empty slice when unknown).
   - Iteration order must be deterministic: store as `BTreeMap<String, Vec<Topic>>` and sort each `Vec<Topic>` ascending by topic ID at build time.
   - Add a unit test in `domain/mod.rs` that builds a `TopicNameIndex` from a fixture with two topics sharing a simple name and asserts `candidates_by_simple_name` returns both, while `get_by_simple_name` returns `None`.

2. **Persist contract inheritance to `AuditData`.**
   - Add `pub inheritance: BTreeMap<topic::Topic, Vec<topic::Topic>>` to `AuditData`. Default to empty map. Update the constructor / `Default` impl as needed.
   - Populate it from `FirstPassDeclaration::Contract::base_contracts` at the boundary between `first_pass` and `tree_shake` (read the existing first-pass output, resolve each `ReferencedNode` to its topic, write into `audit_data.inheritance`). Do not change `tree_shake`'s consumption of the first-pass data.
   - Edge case: contracts with no bases should still appear as keys with empty `Vec`s, OR be absent — pick one convention and document it inline above the field. Recommend: absent (sparse). Sort `Vec<Topic>` ascending.
   - Add an integration test that runs the analyzer against a small fixture with `contract A is B, C` and asserts `audit_data.inheritance[A]` is `[B_topic, C_topic]` (sorted).

3. **Add `events_emitted` to both `FunctionModProperties` variants.**
   - Add `events_emitted: Vec<Topic>` to `FunctionProperties` and `ModifierProperties`. Default to empty vec.
   - In `second_pass`, locate the existing visitor that walks function/modifier bodies and populates `calls` / `mutations` (search for the `EmitStatement` AST variant). For each `EmitStatement`, resolve the emitted event topic and push it into `events_emitted` for the enclosing function or modifier.
   - Sort `events_emitted` ascending by topic ID and dedup before storing.
   - Add a fixture-driven test that emits an event and asserts `events_emitted` contains the expected topic.

4. **Audit `RevertInfo` for the error topic.**
   - Read every variant of `RevertInfo` and every site that constructs one.
   - If every variant that revert-with-custom-error covers exposes the resolved error topic explicitly (i.e. there is a way to extract `Option<Topic>` for the error from any `RevertInfo`), document this with a one-line comment above the type and add a small test that asserts a `revert MyError(...)` produces a `RevertInfo` from which `MyError`'s topic is recoverable.
   - If it does not, add the error topic to the appropriate variant. Be additive — do not remove existing fields. Update all construction sites. Add the same test.

### Verification

- `cargo test -p o11a-core -p o11a-analyze` passes.
- The four new tests pass.
- No new warnings under `cargo clippy`.

### Out of scope

- No graph types. No PageRank. No edge extractor. No resolution pass.
- Do not refactor `TopicNameIndex::build` beyond adding the new field. Do not change the existing public methods.
- Do not move `inject_developer_documentation` yet — that is Phase 5.

---

## Phase 1 — Graph types and module skeleton in `o11a-core`

### Goal

Define the in-memory `ResolutionGraph` data structures, the per-language extractor trait, and the builder dispatcher entry point. No edges are produced yet; no PR runs yet.

### Preconditions

Phase 0 complete.

### Context

Spec sections to read: "Polyglot model" → "Edge vocabulary layering" and "Adding a new language", "Graph specification" → "Node set", "Edge types", "Edge construction", and the universal-core + Solidity extension edge tables.

Files to read in full:

- `crates/o11a-core/src/domain/mod.rs` — `TopicMetadata`, `NamedTopicKind`, `TopicNameIndex`, `AuditData`.
- `crates/o11a-core/src/lib.rs` — top-level module exports.
- `crates/o11a-core/Cargo.toml` — confirm no new dependencies are needed.

Key invariants:

- The graph contains one node per `TopicMetadata::NamedTopic` only. Other `TopicMetadata` variants are excluded.
- Edge weights are `f32`. Edge type is an enum with variants for every universal-core type plus every Solidity-specific type listed in the spec edge tables.
- Edge insertion order must be lexicographic by `(source_topic_id, dest_topic_id, edge_type_discriminant)` — this is enforced at the storage layer, not at extractor call sites.

### Tasks

1. **Create the module structure** under `crates/o11a-core/src/resolution_graph/`:
   - `mod.rs` — public surface.
   - `graph.rs` — `ResolutionGraph` type.
   - `edge.rs` — `EdgeType` enum, edge weight constants.
   - `builder.rs` — `Extractor` trait, `build` entry point.
   - Wire into `crates/o11a-core/src/lib.rs`.

2. **Define `EdgeType`** in `edge.rs`. One variant per spec edge type:
   - Universal core: `ContainsMember`, `ContainsLocal`, `ContainsField`, `Calls`, `References`, `Implements`, `ProxyOf`.
   - Solidity-specific: `WritesState`, `UsingFor`, `ModifierApplied`, `ErrorThrown`, `EventEmitted`.
   - Add a `pub fn default_weight(self) -> f32` method returning the spec's default weight for each variant.
   - Add a `pub fn directionality(self) -> Direction` method returning `Direction::Directed` or `Direction::Undirected` per the spec tables.

3. **Define `ResolutionGraph`** in `graph.rs`. Storage: a `BTreeMap<topic::Topic, Vec<OutEdge>>` for directed adjacency, where `OutEdge { dest: Topic, edge_type: EdgeType, weight: f32 }`. For undirected edges, the builder inserts both directions explicitly.
   - Public methods: `pub fn new() -> Self`, `pub fn add_edge(&mut self, src: Topic, dest: Topic, edge_type: EdgeType, weight: f32)`, `pub fn out_edges(&self, src: Topic) -> &[OutEdge]`, `pub fn nodes(&self) -> impl Iterator<Item = Topic> + '_`.
   - `add_edge` inserts into the source's adjacency list. After all edges are added, a finalization step (`pub fn finalize(&mut self)`) sorts each adjacency list by `(dest_topic_id, edge_type_discriminant)`. Builder calls `finalize()` exactly once after all extractors have run.
   - The graph also exposes a stable node iteration order via `nodes()` — implement by collecting source topics from the adjacency map and sorting ascending.

4. **Define the `Extractor` trait** in `builder.rs`:
   ```rust
   pub trait Extractor {
       fn applies_to(&self, audit_data: &AuditData) -> bool;
       fn extract(&self, audit_data: &AuditData, graph: &mut ResolutionGraph);
   }
   ```
   Plus a top-level `pub fn build(audit_data: &AuditData) -> ResolutionGraph` that:
   - Constructs an empty `ResolutionGraph`.
   - Iterates over a hardcoded list of `Box<dyn Extractor>` (for now, empty — the Solidity extractor is registered in Phase 2).
   - Calls `applies_to` for each; if true, calls `extract`.
   - Calls `graph.finalize()`.
   - Returns the graph.

5. **Add a `resolution_graph: Option<ResolutionGraph>` field on `AuditData`.** Default to `None`. Document inline that it is populated by `o11a_core::resolution_graph::build` at audit-load time, after all language analyzers complete. Do not call `build` yet — that is Phase 4.

6. **Add a smoke test** in `resolution_graph/mod.rs` that builds an empty `AuditData` (or near-empty), runs `build`, and asserts the resulting graph has no edges and no nodes. Then a second test that asserts `build(audit) == build(audit)` (byte-identical) by serializing both graphs and comparing — this anchors the determinism contract.

### Verification

- `cargo test -p o11a-core` passes.
- The two smoke tests pass.
- `cargo clippy` clean.

### Out of scope

- No actual edge extraction. Adding `Box::new(SolidityExtractor)` to the builder list is Phase 2's job.
- No PR engine. No serialization for inspection. No CLI dump.
- Do not add a Rust extractor stub.

---

## Phase 2 — Solidity edge extractor

### Goal

Implement `SolidityExtractor`, which walks the populated `AuditData` and produces every edge described in the spec's universal-core and Solidity-specific tables. After this phase, calling `resolution_graph::build` against an analyzed Solidity audit produces a complete graph.

### Preconditions

Phase 1 complete. Phase 0 complete (the data the extractor reads exists).

### Context

Spec sections: "Graph specification" (full), "Integration with the existing pipeline" → "Data dependencies — edges from existing analyzer state".

Re-read the data dependency table in the spec — it is the contract for this extractor. Each row specifies one edge type, the analyzer pass that produces the data, and the field to read.

Files to read:

- `crates/o11a-core/src/domain/mod.rs` — every type the table references: `Scope`, `TopicMetadata`, `NamedTopicKind`, `FunctionModProperties`, `RevertInfo`, `TopicContext`, the new `inheritance` field, etc.
- `crates/o11a-analyze/src/solidity/analyzer.rs` — `second_pass` for understanding what `topic_context.scope_references` contains and how to filter calls/mutations out of it.
- `crates/o11a-analyze/src/solidity/ast.rs` — `ASTNode` variants `UsingForDirective`, `FunctionDefinition` (the `modifiers` field).

Key invariants:

- The extractor is a **pure read** of `AuditData`. No analyzer state is modified. No new analysis logic.
- Every edge insertion goes through `graph.add_edge`. For undirected edges, insert both directions; the storage layer only tracks directed adjacency.
- Edge weights come from `EdgeType::default_weight()` — the extractor does not hardcode constants.
- Skip edges to topics that are not `TopicMetadata::NamedTopic` (the graph excludes other variants per the spec's "Node set" section).

### Tasks

1. **Create `crates/o11a-core/src/resolution_graph/solidity_extractor.rs`** containing `pub struct SolidityExtractor;` with `impl Extractor for SolidityExtractor`.

2. **Implement `applies_to`** by checking that `audit_data.asts` contains at least one Solidity AST. If the AST representation already has a clear language tag, use it; otherwise, check for any `Node::Solidity(_)` variant in `audit_data.nodes`.

3. **Implement `extract`** by iterating once over `audit_data.topic_metadata`. For each `TopicMetadata::NamedTopic`, emit edges per the spec table:

   - **`contains-member`:** if the topic's `Scope` is `Scope::Member { component, .. }` or `Scope::Component { component, .. }`, and the `component` topic has `NamedTopicKind::Contract`, emit an undirected edge between the topic and the component.
   - **`contains-field`:** same as `contains-member` but when the parent component's kind is `Struct` or `Enum`. (Distinguish by reading the parent's `NamedTopicKind` from `topic_metadata`.)
   - **`contains-local`:** if the topic's scope is `Scope::Member { signature_container: Some(c), .. }` or `Scope::ContainingBlock { containing_blocks, .. }`, emit an undirected edge between the topic and the immediate enclosing function/modifier or block. For `ContainingBlock`, the immediate enclosing scope is the innermost block in `containing_blocks`.
   - **`calls`:** for every topic with `function_properties[t]`, iterate `calls: Vec<Topic>` and emit a directed edge `t → callee`.
   - **`writes-state`:** same as `calls` but using `mutations`.
   - **`event-emitted`:** same as `calls` but using the new `events_emitted` field.
   - **`error-thrown`:** for every topic with `function_properties[t]`, iterate `reverts: Vec<RevertInfo>`. For each `RevertInfo` that names a custom error, extract the error topic (per the convention established in Phase 0 step 4) and emit a directed edge `t → error_topic`. Skip reverts that do not have an associated topic (e.g., `require(cond, "string")`).
   - **`references`:** for every topic with `topic_context[t]`, iterate `scope_references` and emit a directed edge `t → referenced_topic` for each reference that is **not** already covered by a `calls`, `writes-state`, `error-thrown`, or `event-emitted` edge. Maintain a per-source set of already-emitted destinations during this pass to enforce non-duplication.
   - **`implements`:** for every entry `(child, bases)` in `audit_data.inheritance`, emit an undirected edge between `child` and each `base`.
   - **`proxy-of`:** for every topic whose `TopicMetadata::transitive_topic()` returns `Some(target)`, emit a directed edge `topic → target`.
   - **`using-for`:** walk every `ASTNode::ContractDefinition` body for `UsingForDirective` nodes. For each, emit an undirected edge between the affected type's topic and the library's topic.
   - **`modifier-applied`:** walk every `ASTNode::FunctionDefinition` for its `modifiers` field. For each modifier reference, emit an undirected edge between the function and the modifier topic.

4. **Register `SolidityExtractor`** in `builder.rs`'s extractor list.

5. **Test against a fixture audit.** Use an existing small Solidity fixture in the repo (search `fixtures/` or test directories under `o11a-analyze`). Run the analyzer through `run_analysis`, then call `build` on the resulting `AuditData`. Assert:
   - The number of nodes equals the number of `NamedTopic` entries in `topic_metadata`.
   - Every spec edge type appears at least once (or is justified absent for that fixture).
   - Determinism: build twice, assert byte-identical adjacency lists.
   - For one specific known relationship (e.g., a contract that inherits from another in the fixture), assert the `Implements` edge exists in both directions.

### Verification

- `cargo test -p o11a-core -p o11a-analyze` passes.
- The fixture-based test passes.
- `cargo clippy` clean.

### Out of scope

- No PageRank. No resolution pass. No CLI dump.
- Do not modify the Solidity analyzer to produce more data than Phase 0 already added. If you find an edge type whose source data is missing, stop and revisit Phase 0 — do not add new analyzer logic mid-extractor.
- Do not add a Rust extractor stub.

---

## Phase 3 — PageRank engine

### Goal

Implement personalized PageRank against an arbitrary `ResolutionGraph` with a seed vector. Pure, deterministic, fixture-tested in isolation. May run in parallel with Phase 2 — it depends only on Phase 1's types.

### Preconditions

Phase 1 complete.

### Context

Spec sections: "Algorithm summary", "Personalized PageRank parameters", "Determinism contract".

Key invariants from the spec:

- Damping factor: `0.85`.
- Iteration count: fixed at `30` (no early-stop on convergence — the iteration count IS the determinism guarantee).
- Numeric type: `f32`.
- Seed input: `BTreeMap<topic::Topic, f32>` (sorted by topic ID for deterministic iteration).
- Output: `BTreeMap<topic::Topic, f32>` mapping every node in the graph to its PR value.
- Edge weights are used in the PR transition: from a node with outgoing edges of weights `w_1, …, w_k`, mass distributes proportionally to `w_i / Σ w_j`.
- Sequential summation in fixed order — no parallel iteration. (Floating-point summation is not associative; parallelism would break determinism.)
- All accumulator math uses `f32`.

### Context files

- `crates/o11a-core/src/resolution_graph/graph.rs` — to understand adjacency representation.
- `crates/o11a-core/src/resolution_graph/edge.rs` — for edge weight access.

### Tasks

1. **Create `crates/o11a-core/src/resolution_graph/pagerank.rs`** with one public function:

   ```rust
   pub fn personalized_pagerank(
       graph: &ResolutionGraph,
       seeds: &BTreeMap<topic::Topic, f32>,
   ) -> BTreeMap<topic::Topic, f32>
   ```

2. **Algorithm.** The standard personalized PageRank update:
   - Normalize seed vector so its values sum to 1.0 (call this `s`).
   - Let `r` be the rank vector, initialized to `s`.
   - For 30 iterations:
     - Compute new `r'`. For each node `n` in `graph.nodes()` (in sorted topic order):
       - `r'[n] = (1 - d) * s[n]` (the personalization restart, where `d = 0.85`).
       - For each predecessor `p` of `n` (i.e., each node with an out-edge to `n`): `r'[n] += d * r[p] * (weight(p, n) / total_outgoing_weight(p))`.
       - Sum predecessor contributions in ascending source topic order. Sequential, fixed order.
     - Replace `r` with `r'`.
   - Return `r`.
   - For determinism: handle dangling nodes (no outgoing edges) by treating their full `r[p]` mass as if it had a self-loop — or equivalently, redistribute it back into the seed vector. Pick one convention; document inline.

3. **Optimization considerations.** The naive predecessor walk is O(E) per iteration. For audits with up to ~500k nodes and millions of edges, 30 iterations is ~30 × E. Implement directly without optimizing; the spec says this is sufficient at our expected scale. Do not introduce sparse matrix libraries.

4. **Tests.**
   - **Trivial graph:** 1 node, no edges, single seed of weight 1.0. Assert PR converges to ~1.0 on that node within tolerance.
   - **Two-node chain:** A → B with weight 1.0. Seed at A. Assert B's PR is non-zero and less than A's after 30 iterations.
   - **Three-node star:** A connected to B and C undirected. Seed at A. Assert B and C have equal PR values (symmetry).
   - **Determinism:** identical graph, identical seed, run twice — assert byte-identical output.
   - **Floating-point summation order:** construct a graph where the order of predecessor contributions to a node would matter under non-associative addition. Compare the engine's output to a hand-computed reference value using the documented summation order. (This guards against accidental reordering in optimization passes.)

### Verification

- `cargo test -p o11a-core` passes.
- The four tests pass.
- `cargo clippy` clean.

### Out of scope

- No integration with `AuditData` or with the resolver. Phase 4 wires the build call; Phase 6 / 7 wire the PR call.
- No early-stop based on convergence tolerance. The spec lists `1e-6` as a hint — do not implement it as a code branch; iteration count is the contract.
- No parallel iteration.

---

## Phase 4 — Wire `build_resolution_graph` into `analysis.rs`

### Goal

Make the resolution graph build at audit-load time. After this phase, every analyzed audit has its graph populated in `AuditData::resolution_graph`, available for any subsequent code to consume.

### Preconditions

Phases 2 and 3 complete.

### Context

Spec sections: "Integration with the existing pipeline" → "Where the graph is built".

Files to read:

- `crates/o11a-analyze/src/analysis.rs` — the `run_analysis` function around line 28.
- `crates/o11a-core/src/resolution_graph/builder.rs` — the `build` entry point from Phase 1.

Key invariant:

- The graph builds **after all language analyzers complete and before the documentation analyzer runs**. For polyglot future-proofing, the build call must sit between every language analyzer block and the documentation analyzer call. Today only Solidity is present, but the call site is positioned for the multi-language case.

### Tasks

1. **In `run_analysis`, between `solidity::analyzer::analyze` and `documentation::analyzer::analyze`:** call `o11a_core::resolution_graph::build(&audit_data)` and store the result in `audit_data.resolution_graph`. Maintain the existing locking pattern (the existing code uses a `data_context.lock()` mutex; if so, keep it).

2. **Add a regression test** in the same crate: run `run_analysis` against a fixture, then assert that `audit_data.resolution_graph.is_some()` and that the graph has at least one edge.

3. **Performance check.** Run `run_analysis` against the largest fixture in the repo (if any) and confirm the build does not measurably regress total analysis time beyond ~10%. If it does, surface the issue (do not optimize speculatively); the spec assumes linear-in-edges performance is fine.

### Verification

- `cargo test -p o11a-analyze` passes.
- The new regression test passes.
- `cargo clippy` clean.

### Out of scope

- No resolver changes. No documentation analyzer changes. Phases 5 onward handle those.
- Do not call `inject_developer_documentation` from `analysis.rs` yet — that move is Phase 5.

---

## Phase 5 — Relocate `inject_developer_documentation`; add `referenced_topic_candidates`

### Goal

Move dev-doc injection out of `solidity::analyzer::analyze` and run it from `analysis.rs` after the graph build. Add the `referenced_topic_candidates` field on identifier-node types (used later by Phase 10's Phase E fallback). No resolution-pass logic yet — this is preparation.

### Preconditions

Phase 4 complete.

### Context

Spec sections: "Integration with the existing pipeline" → "Where the graph is consumed" (the pipeline-order code block) and "Summary of integration deltas".

Files to read:

- `crates/o11a-analyze/src/solidity/analyzer.rs` — the existing `inject_developer_documentation` call site (around line 114) and the function body (around line 3797).
- `crates/o11a-analyze/src/analysis.rs` — `run_analysis`.
- `crates/o11a-analyze/src/documentation/ast.rs` — `DocumentationNode::CodeIdentifier`.
- `crates/o11a-core/src/collaborator/parser.rs` — `CommentNode::CodeIdentifier`.

Key invariants:

- The behavior of `inject_developer_documentation` does not change in this phase. Only its call site moves.
- The synthetic comments `inject_developer_documentation` produces are read by the documentation analyzer downstream. The new call order must preserve "synthetic comments exist before documentation analysis runs."

### Tasks

1. **Remove the `inject_developer_documentation(audit_data)` call from the end of `solidity::analyzer::analyze`.** Leave the function definition where it is — only the call site moves.

2. **Add a `pub fn` wrapper** at the appropriate level in `solidity::analyzer` (or re-export) so that `analysis.rs` can call it. The simplest path: make `inject_developer_documentation` `pub` and call it directly via its module path.

3. **In `analysis.rs::run_analysis`,** after `resolution_graph::build` (Phase 4) and before `documentation::analyzer::analyze`, call `solidity::analyzer::inject_developer_documentation(&mut audit_data)`. The order is now:
   ```
   solidity::analyzer::analyze            (no longer calls inject_developer_documentation)
   resolution_graph::build
   inject_developer_documentation
   documentation::analyzer::analyze
   ```

4. **Add `referenced_topic_candidates: Vec<topic::Topic>` on `DocumentationNode::CodeIdentifier`** (in `crates/o11a-analyze/src/documentation/ast.rs`). Default to empty vec on parser construction. No code populates it yet — that is Phase 10.

5. **Add the same field on `CommentNode::CodeIdentifier`** (in `crates/o11a-core/src/collaborator/parser.rs`). Default to empty.

6. **Update construction sites** of these node variants to initialize the new field (compiler will guide). Do not add fields to any `serde` payload assumptions you do not control — if the structs are serialized to disk, ensure backward compatibility (typically by `#[serde(default)]` on the new field).

7. **Regression test:** run `run_analysis` and assert that synthetic dev-doc CommentTopics are still created (check `audit_data.comment_index` for entries authored by `Author::DevTechnical` or `Author::DevDocumentation`). The count and content should be byte-identical to before this phase.

### Verification

- `cargo test -p o11a-core -p o11a-analyze` passes.
- The dev-doc regression test passes.
- `cargo clippy` clean.

### Out of scope

- No graph-driven resolution. The dev-doc injection still uses Phase A only — that is Phase 7's job.
- No mutation of `referenced_topic` from any new code. The new candidate field stays empty.
- No documentation-analyzer changes in this phase.

---

## Phase 6 — Doc-tree post-parse resolution pass (Phase B for documentation files)

### Goal

Implement and integrate the per-section graph scoring for documentation files. After this phase, ambiguous `CodeIdentifier` nodes in the doc tree are resolved using personalized PageRank seeded by the section header tree.

### Preconditions

Phase 5 complete.

### Context

Spec sections: "Resolution pipeline" → "Phase A" and "Phase B", "Personalized PageRank parameters", "Confidence threshold and fallback", and "Integration with the existing pipeline" → "Where the graph is consumed" → Consumer 1 description and the Seed construction subsection (note: Consumer 1 uses doc-tree LCA distance over header sections, not the source scope tree — that's Phase 7).

Files to read:

- `crates/o11a-analyze/src/documentation/parser.rs` — particularly the `parse` function and the `find_declaration_by_name` call around line 89.
- `crates/o11a-analyze/src/documentation/analyzer.rs` — the `analyze` function.
- `crates/o11a-analyze/src/documentation/ast.rs` — section / header AST nodes.
- `crates/o11a-core/src/code_refs.rs` — the existing Phase A entry point.

Key invariants:

- Phase A must run unchanged. The parser keeps producing `referenced_topic = Some(t)` for unambiguous tokens and `None` for ambiguous ones.
- The new pass mutates `referenced_topic` in place. It does **not** re-parse anything.
- The pass is a single tree walk over the parsed doc AST plus one PR run per section. Both happen after the parser finishes.
- Confidence threshold: `0.65`. Below threshold → leave `referenced_topic = None` (Phase E in Phase 10 will handle the fallback).

### Tasks

1. **Create `crates/o11a-analyze/src/documentation/resolution_pass.rs`** with the entry point:
   ```rust
   pub fn resolve_doc_tree(
       doc_root: &mut DocumentationNode,
       audit_data: &AuditData,
   )
   ```

2. **Walk the doc AST and identify sections.** A section is delimited by a header AST node. Build the LCA depth structure (each section knows its parent and its depth from the document root).

3. **For each section S**, build the seed vector:
   - For every Phase-A-resolved `CodeIdentifier` in S and in S's ancestor sections, compute `dist(S, S') = depth(S) + depth(S') − 2 × depth(LCA(S, S'))`. Cap at depth 6.
   - Seed weight: `2^(−dist)`. Sum weights when two seeds land on the same topic.

4. **Run `personalized_pagerank`** against `audit_data.resolution_graph.as_ref().expect("graph built in Phase 4")` using this seed vector.

5. **Score and assign.** For each ambiguous `CodeIdentifier` in S (those with `referenced_topic = None`):
   - Look up the candidate list via `audit_data.name_index.candidates_by_simple_name(value)`.
   - Filter out candidates that are not `NamedTopic` (defensive — they shouldn't be in the candidate list, but skip them if present).
   - Sort candidates by PR value descending, then by qualified name ascending, then by topic ID ascending (the spec's deterministic tie-break).
   - If `score_top / (score_top + score_runner_up) ≥ 0.65`, set `referenced_topic = Some(top)`. Otherwise leave it `None`.

6. **Integrate** in `documentation::analyzer::analyze`: after each document file is parsed and before any downstream post-processing of that file's tree, call `resolve_doc_tree(doc_root, audit_data)`.

7. **Persist a per-resolution explanation** (chosen candidate's PR value, top three contributing edges by PR-weighted contribution, the ranked candidate scores). Add a `BTreeMap<NodeId, ResolutionTrace>` field on `AuditData` (or similar — pick a reasonable home). This is consumed by Phase 11's `resolution-trace` dump kind. Storing this is cheap; not storing it is much harder to retrofit later. For the trace's "top contributing edges": during the PR per-candidate score computation, also tally which (predecessor → candidate, edge type) contributions delivered the most mass; take the top three.

8. **Tests:**
   - **Unit:** A small synthetic doc tree with one ambiguous `CodeIdentifier` in a section whose ancestor seeds resolve to one of the candidates. Build a small graph by hand. Assert the resolver picks the right candidate.
   - **Determinism:** Run the pass twice on the same input, assert byte-identical results (including the trace).
   - **Threshold:** Construct a case where top/(top+runner_up) is just below 0.65; assert `referenced_topic` stays `None`.
   - **Regression:** A case the current `find_declaration_by_name` resolves correctly should still resolve correctly after the new pass.

### Verification

- `cargo test -p o11a-analyze` passes.
- The four new tests pass.
- `cargo clippy` clean.

### Out of scope

- No NatSpec / dev-doc resolution — that is Phase 7.
- No co-location / re-iteration / fallback — Phases 9 and 10.
- No header-text or filename seeding (the spec lists this as a calibration knob; it is not part of the default).
- No CLI dumps.

---

## Phase 7 — Dev-doc post-parse resolution pass (Phase B for NatSpec and inline comments)

### Goal

Implement and integrate the per-NatSpec-block / per-inline-comment graph scoring. After this phase, ambiguous `CodeIdentifier` nodes inside synthetic dev-doc CommentTopics are resolved using PR seeded by the source scope chain.

### Preconditions

Phase 5 complete. (Independent of Phase 6 — may run in parallel.)

### Context

Spec sections: "Integration with the existing pipeline" → "Where the graph is consumed" → Consumer 2 description and the Seed construction (scope-chain seeding) subsection — including the seed table.

Files to read:

- `crates/o11a-analyze/src/solidity/analyzer.rs::inject_developer_documentation` (around line 3797) — to understand which topics own dev-doc CommentTopics.
- `crates/o11a-core/src/collaborator/synthetic.rs` — `create_synthetic_dev_comment`, which builds `CommentNode::CodeIdentifier` nodes via `comment_parser::parse_comment`.
- `crates/o11a-core/src/collaborator/parser.rs::parse_comment` — the inline-comment parser.
- `crates/o11a-core/src/domain/mod.rs` — the `Scope` enum (`Member`, `Component`, `ContainingBlock`, `Container`, `Global`).

Key invariants:

- Each dev-doc CommentTopic has a `target_topic` — the source-tree topic the NatSpec / comment is attached to (function, modifier, contract, state var, SemanticBlock, etc.). The "section" for graph seeding is this target topic's scope chain.
- The seed table from the spec:

  | Attached topic kind | Scope chain (distance 0 → up) | Seeds |
  |---|---|---|
  | Contract | contract | contract @ 1.0 |
  | State variable | state-var → contract | state-var @ 1.0, contract @ 0.5 |
  | Function / modifier | function → contract | function @ 1.0, contract @ 0.5 |
  | `@param` NatSpec | param → function → contract | param @ 1.0, function @ 0.5, contract @ 0.25 |
  | SemanticBlock | block → function → contract | block @ 1.0, function @ 0.5, contract @ 0.25 |

- Nested blocks: extend the chain step by step, halving at each level. Depth-6 cap from Phase B applies.
- Same-scope siblings are reached via the graph's `contains-member` and `contains-local` edges — do **not** seed siblings individually. That is a calibration knob, not the default.
- Phase-A-resolved references inside the comment text seed at distance 0 alongside the attached topic.

### Tasks

1. **Implement scope-chain walking.** Add a helper in `o11a-core` (e.g., `domain::scope::ancestor_chain(topic, audit_data) -> Vec<topic::Topic>`) that returns the topic followed by its enclosing scopes up to the contract level. Read `Scope` variants in `topic_metadata` to walk the chain. The helper terminates when it reaches `Scope::Container` / `Scope::Global` or a contract.

2. **Create `crates/o11a-analyze/src/solidity/dev_doc_resolution_pass.rs`** with:
   ```rust
   pub fn resolve_dev_doc_comments(audit_data: &mut AuditData)
   ```
   This walks every CommentTopic with `Author::DevTechnical` or `Author::DevDocumentation` (the synthetic dev-doc author tags) and resolves ambiguous `CommentNode::CodeIdentifier` nodes inside its comment-node tree.

3. **For each dev-doc CommentTopic:**
   - Get its `target_topic` from `TopicMetadata::CommentTopic`.
   - Build the scope chain via the helper.
   - Build the seed vector: for each scope-chain entry at distance `d`, seed at `2^(−d)`. Phase-A-resolved `CodeIdentifier` topics inside this comment also seed at distance 0.
   - Run `personalized_pagerank`.
   - For each ambiguous `CodeIdentifier` in this comment's node tree, score candidates and apply the same threshold rule from Phase 6 (`0.65`, deterministic tie-break, mutate `referenced_topic` in place on success, leave `None` on failure).
   - Persist resolution traces to the same store Phase 6 added.

4. **Wire into `analysis.rs`:** after `inject_developer_documentation` and before `documentation::analyzer::analyze`, call `resolve_dev_doc_comments(&mut audit_data)`.

5. **Update `audit_data.mentions_index`** for any references whose `referenced_topic` newly became `Some(t)` after this pass (the synthetic-comment construction in `synthetic::create_synthetic_dev_comment` populates `mentions_index` based on Phase A resolutions; new resolutions need to be added). Do this by walking the comment-node tree for each updated CommentTopic, re-collecting `referenced_topic` values, and merging into `mentions_index`. Be additive — do not remove existing entries.

6. **Tests:**
   - **Unit:** A function NatSpec that mentions a sibling state variable name shared with another contract's state variable. Assert the same-contract sibling wins.
   - **Unit:** A SemanticBlock comment that mentions a name declared inside the block. Assert the in-block local wins over a same-named state variable.
   - **Determinism:** identical input → byte-identical output.
   - **Regression:** verify that a Phase-A-resolved name in a NatSpec stays resolved to the same topic (the new pass never overwrites a Phase A `Some`).

### Verification

- `cargo test -p o11a-core -p o11a-analyze` passes.
- The four new tests pass.
- `cargo clippy` clean.

### Out of scope

- No co-location / re-iteration / fallback. Phase 9 and 10.
- No per-sibling seeding. Default only.
- No changes to the documentation-tree resolution pass (Phase 6).

---

## Phase 8 — Comparison harness `mechanical-graph` variant

### Goal

Add the new resolver as a fifth variant in the existing `--semantic-linking-compare-all` harness, so recall × precision can be measured against `mechanical` (the current resolver).

### Preconditions

Phases 6 and 7 complete.

### Context

Spec section: "Quality measurement plan".

Files to read:

- Search for `semantic-linking-compare-all` in the repo to locate the harness implementation.
- The existing resolver variants (likely four: `name-index`, `mechanical`, plus two others) — read their dispatch points to understand how a new variant is registered.

Key invariants:

- The harness already runs Pass 1 + Pass 2 + Pass 3 per variant. The `mechanical-graph` variant differs only in its resolver: Phase A → Phase B (no C/D/E yet — those are later phases). The downstream passes are unchanged.
- The variant must be reproducible: the same parsed audit run twice produces identical comparison output.

### Tasks

1. **Add the `mechanical-graph` variant** to whichever enum or string-match site defines harness variants.

2. **Wire its resolver** to use Phase A + Phase B (Phases 6 and 7 of this build plan). For Phase 8 specifically, this means the same resolver code paths the production analysis runs through — no fork.

3. **Run the harness** against the existing fixture audits (the spec mentions nudgexyz). Capture:
   - Recall change vs. `mechanical` (count of new (section, decl) pairs).
   - Precision: of new pairs, what fraction survive Pass 3.
   - Edge-contribution histogram (read from the resolution traces persisted in Phase 6 / 7).

4. **Add the run output to a results doc** at `crates/o11a-analyze/docs/build-plans/semantic-resolution-graph-baseline.md` (create the file). This is the baseline measurement that subsequent phases (9, 10) compare against.

### Verification

- The harness runs to completion.
- The baseline doc is produced.
- All existing tests pass.

### Out of scope

- No tuning of edge weights or threshold based on the results yet — calibration is a separate concern after Phase 10. Just record the numbers.
- No graphing / plotting infrastructure beyond what the harness already produces.

---

## Phase 9 — Phase C (co-location) and Phase D (re-iteration)

### Goal

Add co-location and re-iteration to both resolution passes. After this phase, ambiguities that Phase B alone leaves unresolved get one more shot via uniqueness-based pinning, and resolutions cascade across iterations.

### Preconditions

Phase 8 complete (so improvements are measurable).

### Context

Spec sections: "Resolution pipeline" → "Phase C" and "Phase D".

Key invariants:

- Phase C uses **immediate enclosing scope intersection** restricted to function, modifier, struct, event, or error — not contract level (too coarse) or block level (too fine).
- Phase C resolves a **pair** of ambiguous references when their `Decl(a) ∩ Decl(b)` is exactly one scope.
- Phase D iterates Phases B and C until fixed point or a cap of `4` iterations.
- Each Phase D iteration may unlock new resolutions; new resolutions become Phase A inputs for the next iteration's Phase B seed construction.

### Tasks

1. **Implement Phase C** as a function in each resolution pass (doc-tree and dev-doc). Inputs: the section/comment with its set of ambiguous references. Output: zero or more resolved references plus their resolutions.

2. **Wrap Phases B + C in an iteration loop** (Phase D), capped at 4 iterations. Track which references were resolved in the most recent pass; if zero, exit early. The same iteration cap and exit logic applies to both consumer sites.

3. **Update resolution traces** to record in which iteration a reference was resolved.

4. **Re-run the harness.** Update the baseline doc with the new numbers. Note any regressions and investigate.

5. **Tests:**
   - **Phase C unit:** two ambiguous references whose declared scopes intersect at exactly one function. Assert both resolve.
   - **Phase C non-resolution:** two ambiguous references whose intersection has more than one element. Assert neither resolves via this pass.
   - **Phase D cascade:** a case where iteration 1 resolves reference X, which adds X as a Phase A input to iteration 2's seed vector, which then resolves reference Y. Assert both end up resolved.
   - **Cap test:** construct a case where resolutions never converge (artificial). Assert the cap of 4 stops iteration cleanly with no panic.
   - **Determinism:** byte-identical outputs across runs.

### Verification

- All tests pass.
- Harness runs and updated numbers are recorded.
- `cargo clippy` clean.

### Out of scope

- No Phase E fallback. Phase 10.
- No threshold tuning yet.

---

## Phase 10 — Phase E (anchor-by-name fallback) + populate `referenced_topic_candidates`

### Goal

For references unresolved after Phase D, populate the `referenced_topic_candidates` field with the full candidate list and record the section's contract anchor set as the union of each candidate's containing contract.

### Preconditions

Phase 9 complete.

### Context

Spec section: "Resolution pipeline" → "Phase E — anchor-by-name fallback".

Files to read:

- The downstream consumer that reads `section_to_contracts` (search for it in `crates/o11a-core/src/collaborator/agent/context.rs`).
- The `referenced_topic_candidates` field added in Phase 5.

Key invariants:

- A Phase E reference does **not** contribute a member to `section_to_declarations`. It contributes only contract anchors to `section_to_contracts`.
- For dev-doc comments, the equivalent is the comment's target-topic-containing-contract set — apply the same union rule.
- The fallback must still emit the per-candidate scores in the resolution trace so operators can override.

### Tasks

1. **In each resolution pass**, after Phase D's loop exits with references still unresolved, run Phase E:
   - For each remaining ambiguous reference, populate its `referenced_topic_candidates` field with the candidate list from `candidates_by_simple_name`.
   - For doc-tree consumers: identify the section's containing-contract set; add the union of each candidate's containing contract to it.
   - For dev-doc consumers: the comment's target topic already pins a contract; no additional anchoring needed there.

2. **Record per-candidate scores** in the trace for every Phase-E reference, even though `referenced_topic` stays `None`.

3. **Re-run the harness.** Update the baseline doc.

4. **Tests:**
   - **Unit:** a doc-tree reference that fails Phases B + C with two candidates in different contracts. Assert `referenced_topic_candidates` contains both, `referenced_topic` is `None`, and the section's contract anchor set includes both contracts.
   - **Dev-doc unit:** same-shape test for a NatSpec block.
   - **Determinism.**

### Verification

- All tests pass.
- Harness updated numbers recorded.
- `cargo clippy` clean.

### Out of scope

- No threshold or weight tuning. Calibration is post-Phase-11 work, separate from this build plan.

---

## Phase 11 — Operator inspection tools

### Goal

Add CLI dump kinds `resolution-graph` and `resolution-trace` to `o11a-analyze dump`, so operators can audit graph structure and individual resolutions.

### Preconditions

Phase 10 complete.

### Context

Spec section: "Implementation phases" → step 7.

Files to read:

- `crates/o11a-core/src/audit_dump.rs` — existing dump infrastructure.
- The CLI entry point that exposes `dump` subcommand kinds.

Key invariants:

- Dump output must be deterministic (same audit → byte-identical JSON).
- Sort all collections in dump output (graph adjacency lists, candidate lists in traces, etc.) before serialization.

### Tasks

1. **`resolution-graph` dump kind.** Serialize the full `ResolutionGraph` to JSON. Schema:
   ```
   {
     "nodes": [{ "topic": "...", "kind": "...", "qualified_name": "..." }, ...],
     "edges": [
       { "source": "...", "dest": "...", "edge_type": "ContainsMember", "weight": 1.0 },
       ...
     ]
   }
   ```
   Sort nodes ascending by topic ID. Sort edges by `(source, dest, edge_type)`.

2. **`resolution-trace` dump kind.** Per ambiguous reference, write:
   ```
   {
     "reference_node": "...",
     "section_or_comment_id": "...",
     "phase_resolved": "B" | "C" | "E" | "unresolved",
     "iteration": 1 | 2 | ...,
     "chosen_topic": "..." | null,
     "candidate_scores": [
       { "topic": "...", "qualified_name": "...", "pr_score": 0.123 },
       ...
     ],
     "top_contributing_edges": [
       { "predecessor": "...", "edge_type": "...", "weighted_contribution": 0.04 },
       ...
     ]
   }
   ```
   Sort by reference node ID.

3. **Wire into the existing dump CLI dispatch.**

4. **Tests:** dump twice from the same audit; assert byte-identical JSON.

### Verification

- The two new dump kinds work end-to-end.
- All tests pass.
- `cargo clippy` clean.

### Out of scope

- No interactive UI. No HTML rendering. JSON only.
- No streaming; the graph is small enough to dump in one pass.

---

## 4. Beyond the build plan

After Phase 11, the resolution graph feature is feature-complete for Solidity audits. The remaining work — none of which is part of this plan — is:

- **Calibration.** Run the harness across multiple audits at different thresholds (`0.55`, `0.60`, `0.65`, `0.70`, `0.75`) and edge weight perturbations. Pick the recall × precision knee. Update the constants in `edge.rs`.
- **Rust extractor.** Adds support when the Rust analyzer lands.
- **Calibration knobs.** Per-sibling seeding, header-text seeding, transitive-`implements`-flattening, etc. Implement only when calibration data shows a measurable need.

These are scoped as separate efforts after this build plan completes.
