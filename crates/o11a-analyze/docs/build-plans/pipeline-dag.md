# Behavior refactor and functional property generation

Below are plans to change the behavior generation to be batched based on a DAG. The same structure can then be used when generating functional purpose and functional placement rational on functions as well.

# Behavior refactor


## Algorithm

### Step 1: Build the project-wide call DAG

```
for each in-scope function/modifier topic f:
  props = function_properties[f]
  for each callee_topic in props.calls:
    callee = resolve through transitive chain to real function
    if callee is in-scope:
      add edge callee → f   (callee must come before f)
```

`FunctionModProperties.calls` already stores N-topics of called functions, including cross-contract calls. Resolve through `transitive_topic` so interface stubs collapse to their implementations. The result is a DAG rooted at leaf functions.

**Pivotal decision #1: Where to resolve transitive topics.** Resolving at graph-build time (collapsing interface stubs into implementations) means the topological sort never produces a layer for the stub. The implementation's behaviors cover the interface. This is correct — the current behavior extraction already filters out transitive members.

**Pivotal decision #2: Out-of-scope callees.** Functions that call into OpenZeppelin or other dependencies hit callee topics that have no in-scope function. These edges are dropped — the callee has no behaviors to inject. The function calling into the dependency is a leaf from the DAG's perspective. This matches the current system: out-of-scope code has no behaviors.

### Step 2: Topological sort with SCC collapse

```
graph = edges from step 1
sccs = tarjan(graph)   // strongly connected components
condensed = contract each SCC to a single node
layers = topological_sort(condensed)
```

Each SCC is one extraction unit — all functions in the cycle go in the same batch. In practice, Solidity SCCs are rare (modifier ↔ function cycles), so most SCCs are singletons.

**Pivotal decision #3: SCCs are rare but must be handled.** If two functions form a cycle, extracting them together preserves the current per-contract behavior for that pair. The batch size limit of 5 means an SCC of size >5 must go in its own oversized batch — don't split it.

### Step 3: Affinity batching

Within each layer, group functions that share callees:

```
for each layer:
  build affinity graph: f1 -- f2 share a callee → edge weight = count(shared callees)
  greedily partition into batches of ≤5:
    seed batch with function having most uncalled callees (most isolated)
    grow batch by adding the function with highest affinity to current batch
    stop at 5
```

**Pivotal decision #4: Affinity seeding strategy.** The seed function determines the batch's character. Two options:

- **Highest affinity seed**: pick the pair of functions that share the most callees, then grow. This maximizes callee behavior reuse within the batch.
- **Most independent seed**: pick the function with the fewest shared callees first. This gets isolated functions out of the way, leaving highly-connected functions for later batches where their shared callees are already extracted.

I'd go with **highest affinity seed**. The goal is maximizing the overlap of callee behaviors within a batch so the LLM sees a coherent, interrelated set of functions — producing more internally-consistent behavior descriptions.

### Step 4: Render each batch as JSON

The rendering mirrors `render_solidity_ast_snippet` for the AST, but injects semantics and callee behaviors as properties on the function object rather than as a separate block.

Example output for a batch containing `swap` and `skim`:

```json
{
  "batch": [
    {
      "topic": "N45",
      "name": "swap",
      "kind": "function",
      "signature": { /* existing render_solidity_ast_snippet output */ },
      "body_statements": [ /* existing render */ ],
      "semantics": {
        "N50": { "name": "amount0Out", "semantic": "requested output amount of token0" },
        "N51": { "name": "amount1Out", "semantic": "requested output amount of token1" },
        "N72": { "name": "balance0", "semantic": "contract's token0 balance before swap" },
        "N73": { "name": "amount0In", "semantic": "computed input amount of token0" }
      },
      "called_function_behaviors": {
        "N30": {
          "name": "_update",
          "behaviors": [
            "Updates stored reserves to match actual balances",
            "Emits Sync event with current reserve values"
          ]
        },
        "N22": {
          "name": "_mintFee",
          "behaviors": [
            "Calculates protocol fee and mints LP tokens to fee recipient"
          ]
        }
      }
    },
    {
      "topic": "N46",
      "name": "skim",
      "kind": "function",
      "signature": { /* ... */ },
      "body_statements": [ /* ... */ ],
      "semantics": {
        "N55": { "name": "to", "semantic": "recipient address for excess tokens" }
      },
      "called_function_behaviors": {
        "N99": {
          "name": "IERC20.transfer",
          "behaviors": []
        }
      }
    }
  ]
}
```

**Pivotal decision #5: Where semantics attach.** Currently, semantics are a flat array on the contract JSON. The new design attaches them to each function object, keyed by declaration topic. This means:

- **Params/returns**: from `Scope::Member { member: f }` within the function's signature
- **Body locals**: from `Scope::ContainingBlock { member: f, .. }` within the function's body
- **State variables read/written**: from `FunctionModProperties.mutations` plus any state var in the function's scope chain

This is the same set the current system computes — just scoped to the batch's functions instead of the whole contract, and structured as a property of the function rather than a separate lookup table.

**Pivotal decision #6: Called-function behaviors are keyed by topic, not name.** This lets the LLM cross-reference `called_function_behaviors.N30` with `"referenced_declaration": "N30"` already present in the AST JSON at the call site. The LLM can see that this call invokes `_update` and that `_update` does X and Y — no ambiguity.

**Pivotal decision #7: Empty behavior arrays for out-of-scope callees.** When `swap` calls `IERC20.transfer` (out-of-scope), the entry exists with an empty array. This signals "no behaviors available" rather than "the LLM forgot to include it." The LLM will describe the call's effect from its own context.

### Step 5: Extract, store, repeat

```
prior_behaviors: Map<Topic, Vec<String>>

for each layer in order:
  batches = affinity_batch(layer, prior_behaviors)
  for each batch:
    response = extract_behaviors_from_batch(batch_json)
    for each (member_topic, descriptions) in response:
      for each description:
        allocate B-topic
        store BehaviorTopic { member_topic, description }
        prior_behaviors[member_topic] = descriptions
```

### The prompt

The LLM prompt changes from "here's a whole contract" to:

> Below are one or more functions/modifiers from an in-scope smart contract project. Each function includes:
> - Its signature and body as an AST
> - **Semantics** for its parameters, return values, local variables, and state variables it accesses — the project-specific meaning of each value
> - **Called function behaviors** — what each internal function it calls does (already extracted)
>
> Your task is to extract **behaviors** for each function — what it actually does, described in business-level terms.
>
> Use the semantics to describe behaviors at a business level. Use the called function behaviors to understand what internal calls do without re-describing them — instead describe the composite effect.
>
> Return the same format as before: `{ "members": [{ "member_topic": "...", "behaviors": ["..."] }] }`

### Cost model

For a project with 60 in-scope functions across 5 contracts:
- **Current**: 5 calls, each containing ~12 function bodies + all semantics
- **Proposed**: ~12 calls (batches of 5), each containing 5 function bodies + value semantics + callee behavior summaries
- Per-call token count is lower (5 bodies vs 12), but the callee behavior summaries are new overhead
- Total input tokens: roughly similar, distributed across more calls
- Total output tokens: roughly the same (same number of behaviors)
- Call overhead: ~12 vs 5 LLM calls, but each is smaller and faster

The cost is marginally higher (~1.3-1.5×) for significantly better quality through focused attention and dependency-ordered callee context.

# Functional property generation as pipeline step 5

After feature synthesis (step 4) and the refactored behavior extraction (step 3) complete, a fifth pipeline step generates two sibling properties on every non-pure subject in every in-scope function: **functional purpose** (the business-logic reason the subject exists) and **placement rationale** (the ordering reason it is at this point in its containing function rather than earlier or later). Both properties are produced by a single LLM call per batch, and both are persisted as separate `TopicMetadata` variants (see "Storage" below) so they remain independently addressable, correctable, and re-derivable.

This section assumes the design principles in `crates/o11a-core/SPEC.md` under "Managing Functional Purpose" — particularly that purpose and placement are sibling properties (not fields on one struct), that the pipeline pre-generates them after feature synthesis, and that an adversarial second pass is specified but **not in scope for this implementation**.

## Algorithm

### Step 1: Reuse the call DAG, SCC collapse, and affinity batching

Functional property generation reuses the call DAG, topological sort, and affinity batching defined for the behavior refactor (algorithm steps 1–3 above). Same edges, same SCC handling, same affinity-seeded batches of ≤5 functions. The shared module that exposes these to behavior extraction also exposes them here.

**Pivotal decision #8: Reuse vs. independent batching.** Behavior extraction needs callee behaviors as input and so must process callees first. Functional property generation also needs callee behaviors as input — a call site's purpose ("transfer rewards to the user") and placement rationale ("must be after the balance update so the callback observes the post-update state") both depend on knowing what the callee does. Reusing the same DAG and batches gives both steps the same callee-context shape with one implementation, and preserves the layered ordering that makes callee behaviors available before the callers that consume them.

The cost model differs (see below) but the structure does not.

### Step 2: Filter targets per batch

Within each batch, drop functions that:

- **Have no feature link** after reconciliation. Logged as a reconciliation gap (a structured warning in the pipeline output). These are candidates for on-the-fly generation later, after the auditor addresses the missing feature link, but they do not enter step 5.
- **Have zero non-pure subjects** in their body. Pure helpers reduce purpose to "computes what it computes" and have no placement reasoning to surface.

If a batch is empty after filtering, skip the LLM call entirely for that batch.

**Pivotal decision #9: Skip rather than degrade.** The earlier design considered a degraded prompt for functions without feature links (no business context). Skipping is preferred: a functional purpose generated against missing feature context is low-quality and may anchor downstream review against a wrong premise. Forcing the auditor to address the reconciliation gap first produces better purpose later, and the absence is itself useful audit data.

### Step 3: Classify FunctionCall purity (one-time analyzer change)

The current `UnnamedTopicKind::FunctionCall` is unconditionally `Pure`, gated on a TODO. Replace with:

```rust
pub enum CallKind { Pure, NonPure }

UnnamedTopicKind::FunctionCall(CallKind)
```

Mirrors the `StateVariable(VariableMutability)` pattern. `purity()` returns `NonPure` for `FunctionCall(CallKind::NonPure)` and `Pure` for `FunctionCall(CallKind::Pure)`.

**Pivotal decision #10: Purity, not externality, is the classifier.** An external call into a pure view function (returns a literal, reads no state) is still pure — it has no observable effect beyond its return value. An internal call into a function that mutates state is non-pure. The classifier is whether the call carries observable side effects, not where the callee lives. External pure functions exist; internal non-pure functions are common.

The classification populates from the resolved callee's `FunctionModProperties`: a call is `NonPure` if the callee mutates state, calls another non-pure function, performs an external call, uses delegatecall, contains inline assembly, or invokes selfdestruct/create/create2. Otherwise `Pure`. Out-of-scope callees default to `NonPure` conservatively (we cannot verify their purity), with the same effect: their call sites are subject to functional purpose generation.

This change happens in the analyzer pass that already builds `FunctionModProperties.calls`. Every existing site that constructs `UnnamedTopicKind::FunctionCall` updates to compute the call kind from the callee.

### Step 4: Render each batch as JSON

The rendering reuses the per-function structure from behavior extraction (signature, body_statements, semantics, called_function_behaviors) and adds three things:

- A top-level **`non_pure_subjects`** field: an array of every non-pure subject's topic ID across all functions in this batch. This is the authoritative list of subjects the LLM must produce output for.
- A per-function **`feature`** field: the feature's name, description, and requirements (deduplicated when shared across functions in the batch).
- A per-function **`behaviors`** field: the function's own behaviors as extracted in step 3. Available because step 3 ran first and stored them in `DataContext`.

In addition, every AST node in `body_statements` whose topic is a non-pure subject gains an **`is_non_pure: true`** field, injected by the renderer.

Example output for a batch containing `swap`:

```json
{
  "non_pure_subjects": ["N123", "N124", "N130", "N142"],
  "batch": [
    {
      "topic": "N45",
      "name": "swap",
      "kind": "function",
      "feature": {
        "topic": "F3",
        "name": "Token Swap",
        "description": "Swaps token0 for token1 (or vice versa) at the constant-product price, with reserve invariants enforced.",
        "requirements": [
          "Reserves must remain consistent with actual contract balances after every swap.",
          "Callers must receive at least the minimum output amount they request."
        ]
      },
      "behaviors": [
        "Updates stored reserves to match actual balances after transferring out requested amounts",
        "Reverts when the constant-product invariant is violated",
        "Emits a Swap event with input and output amounts"
      ],
      "signature": { /* existing render */ },
      "body_statements": [
        {
          "node_kind": "ExpressionStatement",
          "topic": "N123",
          "is_non_pure": true,
          "expression": { /* IERC20(token0).transfer(...) */ }
        },
        {
          "node_kind": "ExpressionStatement",
          "topic": "N124",
          "is_non_pure": true,
          "expression": { /* _update(balance0, balance1, ...) */ }
        }
        /* ... */
      ],
      "semantics": { /* same shape as behavior batch */ },
      "called_function_behaviors": { /* same shape as behavior batch */ }
    }
  ]
}
```

**Pivotal decision #11: Two redundant signals for non-pure identification.** The top-level `non_pure_subjects` list and the per-node `is_non_pure: true` flag carry the same information. The redundancy is intentional: the list gives the LLM an unambiguous, easily-iterable set of subjects to address in its output (avoiding the LLM scanning a large AST to find what to write about), and the AST flag lets each subject be seen in source-order context inline (so placement reasoning can refer to "the assignment immediately above this transfer" rather than referring to topics in isolation). Either signal alone would work; together they make both the enumeration task and the contextual reasoning task easier.

**Pivotal decision #12: Feature context lives on the function, not on the subject.** All non-pure subjects within one function share the same feature. Attaching the feature once per function (rather than per subject) keeps the JSON compact and reflects the design that a function's role within its feature constrains every subject inside it.

### Step 5: Renderer change

`ASTRenderContext` (in `crates/o11a-core/src/collaborator/agent/context.rs`) gains:

```rust
pub struct ASTRenderContext {
  pub target_topic: topic::Topic,
  pub omit_function_and_modifier_bodies: bool,
  pub include_untrusted_comments: bool,
  /// When true, every AST node whose topic resolves to a non-pure
  /// subject gets `"is_non_pure": true` injected into its rendered JSON.
  /// Used by functional property generation; defaults to false elsewhere.
  pub flag_non_pure_subjects: bool,
}
```

The renderer's per-node JSON emission consults this flag and the existing purity classification (`UnnamedTopicKind::purity()` and `NamedTopicKind::purity()`) to decide whether to inject the field. No other call sites change behavior — they default to `false` and produce identical output.

### Step 6: The prompt

> Below are one or more in-scope functions/modifiers from a smart contract project. Each function includes:
>
> - Its signature and body as an AST. **Non-pure subjects in the body are flagged with `is_non_pure: true`.**
> - The function's **feature context** — the feature it belongs to, with name, description, and requirements.
> - The function's **behaviors** — what the function as a whole does (already extracted).
> - **Semantics** for its parameters, return values, locals, and state variables it accesses.
> - **Called function behaviors** — what each in-scope function it calls does.
>
> The top-level **`non_pure_subjects`** field lists every non-pure subject's topic across all functions in this batch. Your task is to produce, for **each** topic in that list, two properties:
>
> - **`functional_purpose`** — the business-logic reason this subject exists, expressed in terms of the function's feature and the value the subject contributes to that feature. Avoid restating what the operation mechanically does; explain the impact on users or the system if it were absent.
> - **`placement_rationale`** — the ordering reason this subject is at this point in its function rather than earlier or later. Refer to specific neighboring operations in the function body when relevant: what state must already exist before this subject runs, what state this subject must commit before subsequent operations, what would change if this subject moved.
>
> Use the function's behaviors and feature context to ground both answers. Use the called function behaviors to understand what internal calls do without re-describing them. Use the semantics to describe values at a business level rather than mechanically.

### Step 7: Output schema

```json
{
  "subjects": [
    {
      "subject_topic": "N123",
      "functional_purpose": "...",
      "placement_rationale": "..."
    }
  ]
}
```

Validation: every topic in the input's `non_pure_subjects` array must appear exactly once in the output's `subjects` array. The post-processor logs missing or extra topics as warnings and proceeds with what it received — incomplete output should not block storage of the subjects that did come back.

### Step 8: Storage

Two new `TopicMetadata` variants in `crates/o11a-core/src/domain/mod.rs`:

```rust
FunctionalPurposeTopic {
  topic: topic::Topic,           // P-prefixed
  description: String,
  subject_topic: topic::Topic,   // the non-pure subject this purpose is on
  author: Author,
  created_at: Option<String>,
}

PlacementRationaleTopic {
  topic: topic::Topic,           // P-prefixed
  description: String,
  subject_topic: topic::Topic,
  author: Author,
  created_at: Option<String>,
}
```

**Pivotal decision #13: Share the P prefix and counter with FunctionalSemanticTopic.** `Topic::FunctionalProperty(i32)` was named generically when added; the existing `NEXT_FUNCTIONAL_SEMANTIC_ID` counter (which should be renamed to `NEXT_FUNCTIONAL_PROPERTY_ID` in this work) allocates IDs across all three variants. P1 may be a semantic, P2 a purpose, P3 a placement — distinguishable only by the `TopicMetadata` variant the topic resolves to. This honors the generic name, avoids burning two new prefix letters, and keeps the ID space simple.

Existing code that switches on `TopicMetadata` already handles variant-specific behavior; nothing assumes that all P-topics map to a single variant.

**Pivotal decision #14: Reverse indices on `AuditData`.** Two new maps, rebuilt by the existing `rebuild_feature_context` helper (or a sibling):

```rust
subject_purposes: BTreeMap<topic::Topic, topic::Topic>,    // subject → P-topic
subject_placements: BTreeMap<topic::Topic, topic::Topic>,  // subject → P-topic
```

Mirrors `member_behaviors: HashMap<Topic, Vec<Topic>>` in pattern. Each subject has at most one purpose and one placement at a time; corrections replace the entry rather than appending.

### Step 9: Extract, store, repeat

```
for each layer in DAG order:
  batches = affinity_batch(layer)
  for each batch:
    filtered = drop functions without feature link or without non-pure subjects
    if filtered is empty: continue
    json = render_purpose_batch(filtered)   // includes non_pure_subjects, feature, behaviors, is_non_pure flags
    response = extract_purpose_and_placement_from_batch(json)
    for each entry in response.subjects:
      allocate P-topic for purpose
      store FunctionalPurposeTopic { subject_topic, description: entry.functional_purpose }
      allocate P-topic for placement
      store PlacementRationaleTopic { subject_topic, description: entry.placement_rationale }
    rebuild_feature_context(audit_data)
```

The DAG layer ordering is followed for the same reason behavior extraction follows it: callee behaviors must exist before they can be injected into a caller's batch. Within a layer, batches can run in parallel via `tokio::spawn` (mirrors behavior extraction's pattern).

## Pipeline integration

`run_full_pipeline` in `crates/o11a-core/src/collaborator/agent/pipeline.rs` adds a fifth step:

```
[1/5] Semantic Linking
[2/5] Requirement Extraction
[3/5] Behavior Extraction          (refactored to DAG batching)
[4/5] Feature Synthesis
[5/5] Functional Purpose & Placement Generation
```

The new step (`build_functional_properties`) follows the existing step signature: `async fn(state: &PipelineState, audit_id: &str) -> Result<(), PipelineError>`. Persistence pattern matches behavior extraction — collect per-batch results, lock `DataContext` once at the end, clear any prior `FunctionalPurposeTopic` and `PlacementRationaleTopic` entries (in case of re-runs), and insert the new metadata before calling `rebuild_feature_context`.

## Adversarial second pass — deferred

`crates/o11a-core/SPEC.md` describes an adversarial second pass that critiques each generated purpose and placement, with the critique stored as a comment on the property topic (a new `CommentType` variant for "system critique"). **This is specified but explicitly not implemented in this work.** No comment kind, no LLM call, no field. It is added in a follow-up so the initial generation pipeline can be validated end-to-end first.

## Cost model

For a project with 60 in-scope functions, ~12 batches (same DAG and batching as behavior extraction):

- **Per-batch input:** behavior batch input (signature + body + semantics + callee behaviors) + per-function feature context (name + description + requirements) + per-function behaviors (already extracted) + top-level `non_pure_subjects` list. Roughly 30–50% larger than the behavior batch input by token count.
- **Per-batch output:** scales with non-pure subject count, typically 2–6 per function = 10–30 subjects per batch. Each subject produces two short prose fields. Roughly 2–3× the output token count of behavior extraction (which produces 1–3 short behaviors per function).
- **Total LLM calls:** ~12, same count as behavior extraction.
- **Combined effect:** functional property generation costs roughly 2× what behavior extraction costs in tokens, with the same number of API calls.

The cost is justified by the per-non-pure-subject coverage: every subject that will later receive conditions, threats, and invariants gets its purpose and placement anchored upfront, so downstream steps (and human review) operate against a verified premise rather than re-deriving it from scratch.
