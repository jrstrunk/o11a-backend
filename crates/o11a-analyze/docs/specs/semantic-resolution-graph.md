# Semantic Resolution Graph — Design Spec

## Overview

This spec describes a graph-based replacement for the deterministic name resolver used during documentation analysis. The current resolver (`domain::TopicNameIndex`, `code_refs::find_declaration_by_name`) maps an inline-code identifier in a doc to a single topic by exact-name lookup; on ambiguity (multiple non-transitive declarations share the simple name) it gives up and emits no resolution. That under-matching manifests as missing contract anchors, missing member candidates in mechanical Pass 2, and ultimately Pass 3 batches with shrunken input. The trace evidence (`mechanical-trace.json`, `name-index.json`) shows ~70% of unresolved code references in the nudgexyz audit fall into three patterns the current resolver could in principle disambiguate using surrounding doc context: state-var-shadowed-by-parameter, multi-implementation interface methods, and same-name-across-unrelated-contracts.

Rather than encoding each pattern as an ad-hoc feature, this spec proposes a single algorithm — personalized PageRank over a typed, weighted graph of audit declarations — that handles all three patterns uniformly and degrades gracefully on the long tail of patterns we haven't yet enumerated. The graph also becomes a substrate for downstream features (caller analysis, modifier-effect tracing, library-using-for fanout) so its construction is amortized across multiple consumers.

The current resolver is preserved as Phase A of the new pipeline (it handles the unambiguous cases with no graph cost). The graph is only consulted for ambiguous references.

## Goals

- **Resolve more references correctly than the current name resolver.** Specifically, recover the resolutions in the three patterns above without introducing material false positives.
- **Deterministic.** Same parsed audit → same resolutions, byte for byte. The non-determinism in the wider pipeline lives in the LLM layer; this layer is pure function of static analysis.
- **Inspectable.** For every resolution, surface enough information to explain why a candidate won (which seeds activated it, the dominant edge paths). Operators must be able to audit a resolution and override it.
- **Graceful at scale.** The audit codebases will grow ~100×; the algorithm should not require hand-tuned features for each new pattern. Edge construction should be additive and modular — new edge types are encoded in one place, not threaded through scoring rules.
- **Reusable.** The graph is built once per audit and consumed by both the resolver and any downstream features that need static-relationship reasoning. Edge quality is shared across consumers, which makes errors observable from multiple angles.
- **Polyglot.** Audits may span multiple source languages (Solidity now; Rust soon; more later). The graph is one structure for the whole audit, with language-specific edge extractors plugging into a shared algorithm. Adding a language adds an extractor; it does not require changes to the resolver core.

## Non-goals

- **Replacing transitive_topic.** The existing analyzer-level transitive flag (interface stub → unique implementation) remains. The graph augments it; it does not replace the cases the analyzer can already prove statically.
- **Cross-document semantic reasoning.** This algorithm is per-section disambiguation. Section context is the doc tree's header hierarchy plus the doc filename, not the corpus of all docs.
- **Replacing the current full-name resolver.** Qualified-name lookup (`Contract.member`) is unambiguous by definition and remains in Phase A.
- **Disambiguating language built-ins or imported third-party identifiers** that the analyzer hasn't represented as topics. The graph reasons about audit topics; what isn't a topic isn't a node.
- **Probabilistic / non-deterministic scoring.** No sampling, no learned weights, no model-derived priors.
- **Per-language scoring branches.** The PR algorithm and edge weights are language-agnostic. If empirical calibration shows that one language's `calls` edges should weigh differently from another's, that's a signal the edge type should be split into two distinct types — not that weights should branch on language at runtime. Determinism and simplicity beat per-language tuning.

## Algorithm summary

For each documentation section S, compute a personalized-PageRank distribution over the audit declaration graph, seeded by the topics that Phase A resolved unambiguously in S and S's enclosing sections (header-tree ancestors). For each ambiguous reference R in S, score each of R's candidates by its PageRank value. If the top candidate exceeds a confidence threshold, R resolves to it; otherwise R falls through to the anchor-by-name fallback (Strategy 3 in the brainstorm), which records R's containing contracts as section anchors but does not record a specific member.

The graph is built once per audit and reused across all sections. Personalized PageRank is run per-section, not per-reference, because seeds are section-scoped and most ambiguities in a section share the same seed set.

## Polyglot model

The graph spans the entire audit, regardless of source language. An audit that mixes Solidity contracts and Rust code (a Rust off-chain client; a coprocessor service; a SDK alongside contract bindings) produces **one** unified graph where:

- Every `NamedTopic`, regardless of language origin, is a node. Language identity travels via the topic's `kind` (e.g. `Function(Solidity::External)`, `Function(Rust::Method)`), not via a separate graph or namespace.
- Each language's analyzer contributes edges from its own static analysis. Solidity's analyzer extracts inheritance and modifier edges; Rust's analyzer will extract trait-impl edges; both extract `calls`, `references`, and containment.
- Cross-language references are first-class. When an analyzer records a topic-to-topic relationship that crosses languages — e.g. a Rust binding referring to a Solidity contract — that's a regular edge with whatever weight the relationship merits.
- The PageRank algorithm makes no language distinction. Typed weighted edges are typed weighted edges.

### Edge vocabulary layering

Edges are organized into two layers:

1. **Universal core** — relationships present (in spirit) in every typed source language we expect to support. These edge types are shared across all language extractors. New languages adopt them directly, mapping their constructs onto the universal names. PR weights for these edges are global constants, not per-language.

2. **Language-specific extensions** — relationships that encode semantic distinctions only meaningful in one language (Solidity events, Solidity modifiers, Rust derives, etc.). Each language extractor declares the extensions it produces. Each extension carries its own weight.

The universal-core layer is where most of the resolution signal lives. Language-specific extensions are additive — they sharpen scoring in cases the universal layer can't differentiate, but the resolver works correctly for any language using only the universal core.

### Adding a new language

The full integration recipe:

1. The language's analyzer must produce `NamedTopic` entries for declarations — this is already required by the existing pipeline; no new requirement.
2. Implement an edge extractor for the language. The extractor takes that language's parsed representation (AST, HIR, etc.) and emits edges into the shared `ResolutionGraph` using:
   - Universal-core edge types where the relationship maps cleanly.
   - Language-specific extension edge types where finer distinction matters.
3. Register the extractor in the graph builder's per-language dispatch. The builder runs every registered extractor and merges their output into one graph.
4. If the language introduces new edge-type names, register their default weights in the central edge-weight table. Calibration via the harness picks final values.

No changes are required to the resolver, the PR engine, or the dump tools — they consume an opaque `ResolutionGraph` regardless of how many languages contributed to it.

## Graph specification

### Node set

One node per `TopicMetadata::NamedTopic` in the audit. Other `TopicMetadata` variants (Documentation, Titled, Requirement, Behavior, FunctionalSemantic, etc.) are excluded from the graph: they are documentation-side or generated artifacts, not source declarations, and including them would mix two different relationship semantics.

Unnamed topics (parameter-list nodes, expression nodes, etc.) are also excluded.

### Edge types

All edges are weighted real numbers. Weights are starting values; calibration via the comparison harness will refine them (see "Quality measurement"). Edge types come in two layers: a universal core every language extractor produces, plus language-specific extensions.

#### Universal core

These relationships exist in every typed source language we expect to support. PR weights here are language-agnostic — when calibration shows one language's edges deserve a different weight, the right move is to split the edge type, not to branch weights on language.

| Edge type | Direction | Default weight | Captured relationship |
|---|---|---|---|
| `contains-member` | undirected | 1.0 | Container ↔ its top-level members. Solidity: contract ↔ functions, modifiers, events, errors, structs, enums, state variables. Rust: module ↔ items; impl block ↔ methods. |
| `contains-local` | undirected | 1.2 | Function/method ↔ its parameters and named locals. Heavier than `contains-member` because locals' meaning is tightly bound to the enclosing function. |
| `contains-field` | undirected | 1.0 | Struct/enum ↔ fields/variants. Distinct from `contains-member` so that struct-field disambiguation can be tuned independently of contract-member disambiguation. |
| `calls` | directed (caller → callee) | 0.7 | Function/method body invokes another function/method. |
| `references` | directed (function → topic) | 0.5 | Function body references a declaration in a non-call non-mutation position (read of state/field, type usage, constant read). |
| `implements` | undirected | 0.8 | Type ↔ implemented interface/trait. Solidity: contract ↔ inherited contract or interface. Rust: type ↔ implemented trait. Captures inheritance and trait-implementation as one relationship. |
| `proxy-of` | directed (proxy → target) | 0.9 | Mirrors the analyzer's `transitive_topic` field. Interface stub → unique implementation, regardless of language. |

`contains-local` is intentionally heavier than `contains-member` (`1.2` vs `1.0`): a parameter's meaning is more tightly bound to its function than a function's meaning is bound to its enclosing contract. Disambiguating `pID` (a parameter shared across many functions) should give very high probability mass to the *one function* whose body shares context with the seeds.

#### Solidity-specific extensions

Encoded by the Solidity edge extractor. They sharpen disambiguation in Solidity-specific patterns (mutator-vs-reader of state variables, errors and events thrown/emitted by specific functions).

| Edge type | Direction | Default weight | Captured relationship |
|---|---|---|---|
| `writes-state` | directed (function → state-var) | 0.7 | Function body mutates a state variable. Heavier than universal `references` because mutators are typically the canonical context for the variable: docs about a state var describe how it's *changed*, more than how it's read. |
| `using-for` | undirected | 0.5 | `using L for T` directives — type ↔ library. |
| `modifier-applied` | undirected | 0.5 | Function ↔ modifiers in its declaration. |
| `error-thrown` | directed (function → error) | 0.4 | Function body reverts with a custom error. |
| `event-emitted` | directed (function → event) | 0.4 | Function body emits an event. |

#### Rust-specific extensions (planned)

Final set is defined alongside the Rust analyzer's integration. Likely candidates, with reasoning to be validated empirically:

| Edge type | Direction | Default weight | Captured relationship |
|---|---|---|---|
| `derives` | undirected | 0.6 | Struct/enum ↔ derived traits (`#[derive(...)]`). |
| `re-exports` | directed (module → item) | 0.4 | Module ↔ items it re-exports via `pub use`. |
| `mutates-field` | directed (function → field) | 0.7 | Function body mutates a struct field through a `&mut` reference. Analogous to Solidity's `writes-state` — same reasoning, different mechanism. |

When the Rust analyzer lands, this table is finalized and integrated. Additional languages add their own table.

#### Adding a new edge type

Two requirements: (a) the edge encodes a semantic relationship the analyzer can extract deterministically, and (b) the relationship contributes to disambiguation in measurable cases. Edges that never appear in winning PR paths during calibration get removed; edges that always dominate get their weight tuned. The cost of an extra edge type is a row in the weight table, a case in the extractor, and a recalibration pass — all bounded.

### Edge construction

Edges are derived from `AuditData` after the analyzer completes. Each language has its own edge extractor that consumes the audit data for that language and emits edges into a shared `ResolutionGraph`. The Solidity extractor uses the analyzer's already-tracked function calls, state-var mutations, modifier applications, and inheritance — those produce the edge set without re-parsing. The Rust extractor will do the same against Rust's HIR or equivalent.

The graph builder lives in `o11a-core` (shared across all consumers) and dispatches to per-language extractors. The output is one `ResolutionGraph` for the audit.

The builder must be deterministic: edge insertion order is fixed by `(source_topic_id, dest_topic_id, edge_type)` lexicographic order, and any internal collections use ordered types (`BTreeMap` rather than `HashMap` where iteration order would otherwise be observed by the algorithm).

## Integration with the existing pipeline

The graph slots into the analysis pipeline as a new step between language analyzers and any consumer that resolves names against doc text. It does not replace the existing `TopicNameIndex`; it augments it for ambiguous references.

### What the graph replaces

**Only the under-matching behavior of `TopicNameIndex::get_by_simple_name`** when multiple non-transitive candidates share a simple name. Today the index's `build` step filters simple-name candidates down to a unique non-transitive match, and `get_by_simple_name` returns `None` whenever that filter fails — the caller then silently drops the resolution. The graph is consulted in exactly that case.

What stays unchanged:

- The qualified-name path of `TopicNameIndex` — `get_by_qualified_name` is unambiguous by definition; Phase A keeps using it.
- The unique-simple-name path of `TopicNameIndex` — Phase A still resolves these directly.
- The analyzer-level `transitive_topic` filter for proxy-style ambiguity.
- The public signature of `code_refs::find_declaration_by_name` for Phase A consumers.

One small required refactor: `TopicNameIndex` grows a sibling accessor that returns the **full pre-dedup candidate list** for a simple name (e.g. `candidates_by_simple_name`), since Phase B's scoring needs every candidate, not just the one that survived dedup. The existing `Option`-returning method stays for Phase A.

### Data dependencies — edges from existing analyzer state

Each edge type in the graph maps to a field the Solidity analyzer already produces, with three additive exceptions called out below.

| Edge | Source pass | Field / location |
|---|---|---|
| `contains-member` | analyzer | `topic_metadata[t].scope` chain — `Scope::Member { component, .. }` and `Scope::Component { component, .. }` |
| `contains-local` | analyzer | `Scope::Member { signature_container: Some(_), .. }` and `Scope::ContainingBlock { .. }` |
| `contains-field` | analyzer | `Scope::Component { component }` whose parent is `NamedTopicKind::Struct` / `Enum` |
| `calls` | second_pass | `FunctionModProperties::calls: Vec<Topic>` |
| `references` | second_pass | `topic_context[t].scope_references` filtered to non-call non-mutation positions |
| `implements` | first_pass | `FirstPassDeclaration::Contract::base_contracts` — currently consumed only by `tree_shake`; needs to be persisted to `AuditData` to reach the graph builder |
| `proxy-of` | analyzer | `TopicMetadata::transitive_topic()` |
| `writes-state` (Solidity) | second_pass | `FunctionModProperties::mutations: Vec<Topic>` |
| `using-for` (Solidity) | extractor | walk `ContractDefinition` nodes for `UsingForDirective` (no analyzer change required) |
| `modifier-applied` (Solidity) | extractor | walk `FunctionDefinition::modifiers` (no analyzer change required) |
| `error-thrown` (Solidity) | second_pass | `FunctionModProperties::reverts: Vec<RevertInfo>` — the `RevertInfo` variant must expose the error topic explicitly |
| `event-emitted` (Solidity) | second_pass | not currently tracked as a structured field; needs a new `events_emitted: Vec<Topic>` slot on `FunctionModProperties` |

Three upstream additions are required and are scheduled into the implementation phasing below:

1. **Inheritance reaches `AuditData`.** `base_contracts` is built during `first_pass` and consumed during `tree_shake` but is not retained on `AuditData`. Add a `pub inheritance: BTreeMap<topic::Topic, Vec<topic::Topic>>` field on `AuditData` (cheaper than reshaping `NamedTopicKind::Contract`) and populate it from the existing first-pass data. Additive only.
2. **`RevertInfo` audit.** Confirm that `RevertInfo` carries the resolved error topic explicitly, not just a kind tag. If it does not, add the topic to the appropriate variant. The error topic is the destination of every `error-thrown` edge, so it must be reachable without re-walking the AST.
3. **Track event emissions.** Add `events_emitted: Vec<topic::Topic>` to both `FunctionModProperties::FunctionProperties` and `::ModifierProperties`, and have `second_pass` populate it from `EmitStatement` nodes alongside the existing `calls` and `mutations` collection. Same visitor, additional sink.

These are additive changes to existing structures, not redesigns. The graph builder is then a pure read of `AuditData`.

### Where the graph is built

**In `analysis.rs::run_analysis`, after `solidity::analyzer::analyze` completes and before `documentation::analyzer::analyze` runs.** Not inside `solidity::analyzer::analyze`.

The reasoning is polyglot-shape: the graph is per-audit, not per-language. For audits that span multiple source languages, every language analyzer must complete before the graph can be built — a Rust analyzer cannot add edges to a graph that has already been locked. Putting the build call in `analysis.rs`, after all language analyzers, gives every extractor its turn.

```rust
// in run_analysis, after all language analyzers finish:
{
  let mut ctx = data_context.lock()?;
  let graph = o11a_core::resolution_graph::build(&ctx, audit_id);
  ctx.audit_data_mut(audit_id).resolution_graph = Some(graph);
}
```

The `build` function dispatches to per-language edge extractors based on which AST kinds are present in `audit_data.asts`. The Solidity extractor lands first; the Rust extractor plugs in alongside it when that analyzer arrives.

### Where the graph is consumed

There are two existing consumers of name resolution; the graph plugs into each through one shared mechanism — a post-parse resolution pass that mutates `referenced_topic` in place on already-parsed identifier nodes.

**Consumer 1: documentation parser (`documentation::parser::parse`).** Today this walks the doc AST and resolves inline-code spans via `code_refs::find_declaration_by_name`, producing `DocumentationNode::CodeIdentifier { referenced_topic: Some(t) }` for unambiguous tokens and `referenced_topic: None` for ambiguous ones. The graph does not replace per-token resolution; it adds a post-parse pass:

1. The parser runs as today and produces Phase-A resolutions (or `None` on ambiguity).
2. After the doc tree is built, a new pass walks the section header tree:
   - For each section S, gather Phase-A-resolved `CodeIdentifier` topics in S and ancestor sections; build the seed vector with the LCA-distance weighting from the algorithm above.
   - Run personalized PageRank from the seed vector once per section.
   - For each `CodeIdentifier` in S with `referenced_topic = None`, look up its candidate list via `candidates_by_simple_name`, score each candidate by its PR value, pick the winner if above the confidence threshold, and **mutate `referenced_topic` in place**.
3. Phase E fallback (anchor-by-name) writes the candidate set into a side channel — a new `referenced_topic_candidates: Vec<Topic>` field on `CodeIdentifier` — so downstream `mechanical_semantic_links` can read all candidates without picking one.

This pass lives in `documentation::analyzer` (likely a new `documentation::resolution_pass` module) and runs inside `documentation::analyzer::analyze` after each doc file is parsed and before any downstream consumer of `referenced_topic` runs.

**Consumer 2: developer-documentation injection (`inject_developer_documentation`).** Same shape — code references resolved in NatSpec and inline-comment text, via `comment_parser::parse_comment` inside `synthetic::create_synthetic_dev_comment`, producing `CommentNode::CodeIdentifier { referenced_topic, … }` whose `referenced_topic` is filled by the same `find_declaration_by_name` path and exhibits the same ambiguity drop. NatSpec is not deferred to a later phase: dev-doc injection is **moved out of `solidity::analyzer::analyze`** and run from `analysis.rs` after the graph build, then a graph-driven post-parse pass updates the synthetic comments' `CodeIdentifier` nodes the same way the documentation analyzer updates doc-tree nodes.

The pipeline order becomes:

```
solidity::analyzer::analyze            (no longer calls inject_developer_documentation)
  └── name_index built as final step
resolution_graph::build                 (in analysis.rs)
inject_developer_documentation          (moved to analysis.rs, runs after graph build)
  └── per-NatSpec-block resolution pass
documentation::analyzer::analyze
  └── per-section resolution pass
```

**Seed construction — scope-chain seeding.** Both consumers use the same algorithm to construct a seed vector for an attached comment: walk the scope chain from the attached topic upward and seed each level at `2^(−distance)`. This is the LCA-distance rule from Phase B applied over the source-tree scope tree instead of the doc-tree header tree — the two trees have the same mathematical shape and the algorithm is shared. Phase-A-resolved references inside the comment text seed at distance 0 alongside the attached topic.

| Attached topic kind | Scope chain (distance 0 → up) | Seeds |
|---|---|---|
| Contract | contract | contract @ 1.0 |
| State variable | state-var → contract | state-var @ 1.0, contract @ 0.5 |
| Function / modifier | function → contract | function @ 1.0, contract @ 0.5 |
| Function/modifier `@param` NatSpec | param → function → contract | param @ 1.0, function @ 0.5, contract @ 0.25 |
| SemanticBlock (inline `// …` comment above a statement) | block → function → contract | block @ 1.0, function @ 0.5, contract @ 0.25 |

Nested blocks extend the chain: a comment on an inner block has chain `inner-block → outer-block → … → function → contract`, halving at each step. The depth-6 cap from Phase B applies. The PR engine, damping factor, iteration count, confidence threshold, and Phase E fallback are unchanged.

**Same-scope siblings are reached via graph topology, not direct seeding.** The seed at each scope level is the scope topic itself — not its members. PR distributes mass from each scope seed to its members in one hop via `contains-member` (contract → state-vars and functions, weight 1.0) and `contains-local` (function → params and locals; SemanticBlock → its declared locals; both weight 1.2). A reference in a function NatSpec to one of the function's own params is one hop from the function seed via `contains-local`. A reference to another state variable in the same contract is one hop from the contract seed via `contains-member`. A reference in a SemanticBlock comment to a local declared inside that block is one hop from the block seed via `contains-local`, while a same-named state variable in the same contract is three hops away — the local wins on PR mass without any special handling.

Per-sibling seeding (seeding every state variable of the contract individually for a member NatSpec, every param of the function individually for a function NatSpec, every local of a block individually for a block comment, etc.) is **not** part of the default seed construction. It is reserved as a calibration knob, available if harness measurements show that contracts or functions with very large member counts dilute per-sibling mass below the noise floor in practice. The default keeps the seed vector small and lets the graph topology do the spreading.

The graph-based pass fixes the same ambiguity patterns the doc-tree consumer fixes — state-var-shadowed-by-parameter, multi-implementation interface methods, same-name-across-unrelated-contracts — when they appear in NatSpec or any other inline-comment context.

**Downstream consumers need no change.** `context::mechanical_semantic_links`, `context::collect_mechanical_links_recursive`, and `context::enumerate_section_code_references` all read `referenced_topic` from the parsed identifier nodes. After the resolution pass mutates those fields, every downstream consumer picks up the improved resolutions for free. This is the cleanest property of the integration: **one mutation point per consumer site** (doc tree, synthetic-comment tree), and every reader benefits transparently.

### Summary of integration deltas

**`o11a-core`:**
- New `resolution_graph/` module: `builder` (per-language extractor dispatcher), `graph` (`ResolutionGraph` with sorted adjacency lists), `pagerank` (PR engine), `solidity_extractor`.
- `domain::TopicNameIndex` — add `candidates_by_simple_name`. Existing methods unchanged.
- `domain::AuditData` — add `inheritance: BTreeMap<topic::Topic, Vec<topic::Topic>>` and `resolution_graph: Option<ResolutionGraph>`.
- `domain::FunctionModProperties` — add `events_emitted: Vec<topic::Topic>` to both variants. Audit `RevertInfo` for the error topic.
- `documentation::ast::DocumentationNode::CodeIdentifier` and `collaborator::parser::CommentNode::CodeIdentifier` — add `referenced_topic_candidates: Vec<Topic>` for the Phase E fallback.

**`o11a-analyze`:**
- `analysis.rs::run_analysis` — call `resolution_graph::build` between language analyzers and documentation analysis; call `inject_developer_documentation` from here, after the build.
- `solidity::analyzer::analyze` — `second_pass` records `events_emitted`; `first_pass` (or a small persistence shim after it) writes inheritance into `AuditData`. Remove the trailing `inject_developer_documentation` call. Other stages unchanged.
- `solidity::analyzer::inject_developer_documentation` — gains a graph-driven post-parse pass over each NatSpec block's synthetic `CommentNode` tree.
- `documentation::analyzer::analyze` — call new `resolution_pass::resolve_doc_tree` after parsing each document, before downstream post-processing.

**No semantic-linking pipeline changes** beyond reading better-resolved `referenced_topic` fields. No changes to `o11a-core::collaborator::agent::*` or `o11a-core::collaborator::agent::pipeline`.

## Resolution pipeline

```
Phase A: Unique-name resolution         (existing — unchanged)
Phase B: Section-context graph scoring  (new)
Phase C: Co-location resolution         (new — see "Co-location" below)
Phase D: Re-iteration                   (B + C, until fixed point or bounded)
Phase E: Anchor-by-name fallback        (Strategy 3 from the brainstorm)
```

### Phase A — unique-name resolution

Unchanged. `code_refs::find_declaration_by_name` runs first; it resolves identifiers whose qualified name is unambiguous and identifiers whose simple name has exactly one non-transitive candidate. The output is the existing `referenced_topic: Some(t)` field on `DocumentationNode::CodeIdentifier`, populated as today.

A reference unresolved by Phase A enters the graph pipeline.

### Phase B — section-context graph scoring

For each section S in the document tree (sections being identified by header AST nodes, not paragraphs):

1. **Construct the seed vector.** For each Phase-A-resolved code reference R' in S or in any ancestor section S' (walking the header tree to the document root):
   - Compute LCA-based distance: `dist(S, S') = depth(S) + depth(S') − 2 × depth(LCA(S, S'))`. Same section = 0; direct parent = 1; sibling = 2; grandparent = 2; cousin = 4.
   - Seed weight: `2^(−dist)`. Caps at depth 6 (anything farther than that contributes negligibly anyway and the cap keeps seed sets bounded).
   - The seeded node is R''s `referenced_topic`, weighted by `2^(−dist)`.
   - When two seeds land on the same topic, weights sum.
2. **Optionally seed from header text and document filename.**
   - Tokenize the section's enclosing header chain and the document filename via the same identifier-splitter the BM25 corpus uses.
   - For each token, look up exact name matches in the topic name index. Each hit contributes a small seed weight (`0.3` per direct match in the immediate header; `0.1` for the document filename).
3. **Run personalized PageRank** from the seed vector. Damping `0.85`, fixed `30` iterations, L1 convergence tolerance `1e-6` (used as an early-stop signal but iteration count is the determinism guarantee).
4. **Score each ambiguous reference R in S.** For each candidate C of R, score = PR(C). Pick the highest-scoring candidate above the confidence threshold (see below).
5. **Record the resolution and the explanation.** Persist the chosen candidate's PR value, its top three contributing edges (by PR-weighted edge contribution), and the resolution itself. Operators inspecting a resolution can look at the explanation to understand or override it.

The PR computation is shared across all references in S (the seed vector is per-section, not per-reference).

### Phase C — co-location resolution

For each pair of ambiguous references (a, b) in section S:

1. Compute `Decl(a)` = the set of *immediate enclosing scopes* in which `a` is declared, where "scope" is restricted to function, modifier, struct, event, or error. (Contract level is too coarse — most names co-occur at contract level — and block level is too fine.)
2. Same for `Decl(b)`.
3. If `Decl(a) ∩ Decl(b) = {scope_x}` is a singleton, both `a` and `b` resolve to their respective declarations within `scope_x`.
4. If the intersection has more than one element, no resolution from this pair (but other pairs may resolve).

Phase C complements Phase B: B exploits *gradient* (nearby refs activate by topology), C exploits *uniqueness* (rare co-occurrence pins the answer). Run them in parallel, use C's resolutions as additional Phase A inputs for re-iteration.

### Phase D — re-iteration

If Phase B or C resolved any new references, run Phase B and C again with the expanded seed set. A reference resolved in pass N may unlock another reference in pass N+1.

Iterate until a fixed point or a bounded cap (cap = `4`). Empirically, most ambiguity converges in 1–2 iterations; the cap protects against pathological edge cases.

### Phase E — anchor-by-name fallback

References still unresolved after Phase D: instead of emitting nothing, the resolver records the section's contract anchor set as the union over all candidates of each candidate's containing contract. The reference does *not* contribute a specific member to `section_to_declarations`, only contracts to `section_to_contracts`.

Rationale: the contract anchors are typically already in the section's resolved set (from other references); the fallback is rarely the *only* signal contributing a contract anchor. But when it is, anchoring to the candidate's contract is strictly better than emitting nothing — Pass 3's contract-scoped batch will see the contract and can produce semantics for declarations within it.

## Personalized PageRank parameters

| Parameter | Value | Reasoning |
|---|---|---|
| Damping factor | `0.85` | Standard. Higher concentrates rank around seeds (less spreading); lower flattens the distribution. `0.85` is empirically robust across applications. |
| Iteration count | `30` | Generous for the size of audit graphs we see (~5k–500k nodes). PR converges geometrically; 30 iterations produces L1 error < `1e-9` in practice. Fixed iteration count guarantees determinism regardless of input. |
| Convergence tolerance | `1e-6` | Used as an *early-stop hint* to skip remaining iterations once stable, but iteration count is the determinism guarantee — never stop before iteration 30 on records consumed by the resolver. |
| Numeric type | `f32` | Sufficient precision for scoring; halves memory pressure vs `f64` at scale; deterministic given fixed iteration order. |
| Seed weighting | `2^(−lca_distance)` | Halves at each step away. After 6 hops, weight is `1/64`; cap at depth 6. |

## Determinism contract

The resolver's output is fully determined by:

1. The parsed `AuditData` (specifically: `topic_metadata`, the AST node graph, the analyzer-tracked relationships).
2. The graph builder's edge types and weights (compile-time constants).
3. The PageRank parameters (compile-time constants).

For the same `AuditData`, the same code produces the same resolutions, byte for byte. The pipeline is a pure function with no environment dependencies.

To preserve this:

- Edge insertion order is fixed by `(source_topic_id, dest_topic_id, edge_type)` lexicographic order. The graph internally uses a sorted adjacency list, not a `HashMap`.
- The seed vector is built in topic-ID order.
- Parallel iteration is forbidden in the PR loop. (PR's update is associative and commutative, so parallel sums *are* mathematically equivalent — but floating-point summation is not associative under rounding. We sum sequentially in fixed order.)
- The candidate-tie-break ordering: by PR score descending; ties broken by candidate qualified-name ascending; further ties broken by topic-ID ascending. This is purely lexicographic and stable.

## Confidence threshold and fallback

A candidate's PR score is meaningful only relative to other candidates — absolute PR magnitudes vary by graph size and seed density. The confidence rule:

- Let `score_top` = PR of the highest-scoring candidate.
- Let `score_runner_up` = PR of the second-highest candidate (or `0` if only one candidate).
- Resolution accepted if **`score_top / (score_top + score_runner_up) ≥ 0.65`**.

`0.65` is the starting threshold; calibration is via the harness measurement plan below. Above it, the top candidate is decisively ahead. Below it, the top wins by less than 2× the runner-up — too close to call deterministically, fall through to Phase E.

The threshold is tunable per audit if needed (`--graph-confidence-threshold`), but the default should not need adjustment.

When falling through to Phase E, the resolver still emits the per-candidate scores in the resolution log so operators can see *why* the threshold wasn't met and override if desired.

## Quality measurement plan

The existing comparison harness (`--semantic-linking-compare-all`) produces the data needed to measure this. Add a fifth resolver variant — call it `mechanical-graph` — that runs the new graph-based pipeline through Phase A → E. The harness already runs Pass 1 + Pass 2 + Pass 3 per variant and produces the survival data; reusing that infrastructure gives us:

1. **Recall change**: how many more (section, decl) pairs does `mechanical-graph` produce vs. `mechanical`?
2. **Precision check**: of the new pairs, what fraction survive Pass 3? Pass 3 is the LLM filter — declarations the model can't synthesize semantics for are noise.
3. **Edge contribution analysis**: from the resolution log, which edge types most often dominated the winning candidate's PR mass? Edges that never contribute can be removed; edges that always dominate can have their weight increased.
4. **Threshold sensitivity**: re-run the harness against the same parsed audit at thresholds `0.55`, `0.60`, `0.65`, `0.70`, `0.75`. Plot recall × precision. Pick the knee.

A regression test: package a small fixture audit with hand-labeled "correct" resolutions for ~20 well-understood ambiguous references. Run the resolver in CI. Failures fail the build.

## Implementation phases

Phasing within the resolver itself, not phases A-E of the algorithm:

1. **Graph types and builder skeleton** in `o11a-core`. Defines `ResolutionGraph` (sorted adjacency lists, edge type enum) and the per-language extractor trait/dispatch. Adds the `candidates_by_simple_name` accessor on `TopicNameIndex`. No edges produced yet.
2. **Solidity edge extractor and analyzer-side data plumbing.** First language extractor — produces universal-core edges plus Solidity-specific extensions. Includes the three additive analyzer changes the extractor reads from:
   - Persist inheritance to `AuditData` as `inheritance: BTreeMap<topic::Topic, Vec<topic::Topic>>`, populated from `FirstPassDeclaration::Contract::base_contracts` (currently consumed only by `tree_shake`).
   - Audit `RevertInfo` and ensure the resolved error topic is exposed explicitly; add it if missing.
   - Add `events_emitted: Vec<topic::Topic>` to both `FunctionModProperties::FunctionProperties` and `::ModifierProperties`, populated by `second_pass` from `EmitStatement` nodes alongside the existing `calls` and `mutations` collection.

   With those in place, the extractor is a pure read of `AuditData`.
3. **PR engine**. Personalized PageRank against an arbitrary `ResolutionGraph` with a seed vector. Tested in isolation against fixtures.
4. **Phase B integration at the post-parse layer**. Integration is at the documentation analyzer's post-parse step, **not at the parser** — the parser keeps producing `referenced_topic` via Phase A; a new resolution pass mutates ambiguous entries afterward. Phase A still handles unambiguous references; Phases C, D, and E are not yet present. Two consumer sites are wired in this phase:
   - `documentation::analyzer::analyze` runs `resolution_pass::resolve_doc_tree` over each parsed document.
   - `inject_developer_documentation` is moved out of `solidity::analyzer::analyze` to `analysis.rs` (running after `resolution_graph::build`) and gains an analogous post-parse pass over the synthetic `CommentNode` trees attached to each NatSpec block. Each NatSpec block is treated as a single-section context whose seed is the attached topic plus any in-block Phase-A resolutions; the same PR engine and threshold apply.

   Downstream consumers (`context::mechanical_semantic_links`, `context::collect_mechanical_links_recursive`, `context::enumerate_section_code_references`) read `referenced_topic` and benefit transparently — no edits at those sites. Compare against the current resolver via the harness across both doc-tree and NatSpec ambiguities.
5. **Phase C and D**. Add co-location and re-iteration to the post-parse pass. Re-measure.
6. **Phase E fallback**. Anchor-by-name for the residual; populate `referenced_topic_candidates` on identifier nodes for downstream consumers that want the full candidate set.
7. **Operator inspection tools**. CLI subcommand `o11a-analyze dump` gains kinds: `resolution-graph` (writes the full graph to JSON for inspection) and `resolution-trace` (per-reference resolution explanations: chosen candidate, scores, top contributing edges).
8. **Rust edge extractor** (and recalibration). When the Rust analyzer is integrated, plug its extractor into the dispatcher and run the harness against an audit containing both Solidity and Rust to confirm cross-language references resolve correctly. Calibrate Rust-specific edge weights against the same recall × precision curve.

Each phase ships independently, gated behind a flag during development (`--resolver=name-index|graph`) so the comparison is direct and rollback is trivial. Adding a future language reduces to repeating step 8: implement an extractor, register it, run the harness.

## Open calibration questions

These are settled by data, not by debate:

1. **Edge weights.** The defaults above are starting points. The harness will reveal which edges contribute meaningful PR mass to correct resolutions vs. noise.
2. **Whether to include header text and document filename as seeds.** Cheap to add, possibly noisy. Implement behind a flag, measure both ways.
3. **Whether to flatten `implements` edges transitively** (child → all ancestor members) at build time, or rely on PR to traverse multi-step inheritance / impl-trait chains. Flattening is faster per-query but bloats the graph.
4. **Confidence threshold default.** Start at `0.65`, tune via the harness recall × precision curve.
5. **Re-iteration cap.** Start at `4`, observe convergence rate, possibly drop to `2` if most ambiguities resolve in one pass.
6. **Universal vs. language-specific for state mutation.** Solidity's `writes-state` and Rust's `mutates-field` are conceptually the same relationship at different granularities (contract state vs. struct instance state). After Rust integration, measure whether unifying them as a universal `mutates` edge degrades or improves resolution. If the harness shows no quality difference, unify and reduce edge-type sprawl.

## Why graph-based, not feature-weighted

Documented separately in the design discussion that produced this spec; not repeated here. Briefly: graph methods compose new patterns from existing edges, capture multi-hop and hub-corrected signal naturally, and at 100× scale, the cases where local features break down become a larger fraction of total ambiguity. The graph is also a substrate for downstream consumers (callers/uses analysis, modifier-effect tracing) that we'd build anyway, so its construction is amortized across multiple features.

## References

- `docs/specs/semantic-linking.md` — outer pipeline this resolver feeds.
- `docs/specs/inline-code-reference-parsing.md` — how inline code identifiers reach the resolver.
- `crates/o11a-core/src/audit_dump.rs` — operator inspection infrastructure; `resolution-graph` and `resolution-trace` dump kinds will live here.
- `crates/o11a-core/src/code_refs.rs` — current name resolver; Phase A wraps `find_declaration_by_name`.
- `crates/o11a-core/src/domain/mod.rs::TopicNameIndex` — current ambiguity heuristic; replaced for ambiguous cases by Phase B.
