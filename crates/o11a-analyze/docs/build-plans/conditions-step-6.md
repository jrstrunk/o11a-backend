# Step 6 — Conditions on Non-Pure Subjects

This is the implementation plan for pipeline step 6: generating **conditions** — purpose-driven observations about a non-pure subject's interaction surface — for every non-pure subject that received a functional purpose in step 5. Conditions are the structured input that step 7 (threats) reasons from.

Read `pipeline-dag.md` first; this doc assumes its DAG, batching, and step-5 patterns are in place.

## Summary of decisions

These were settled during design and should not be re-litigated during implementation:

- **Per-function generation, per-subject output.** One LLM call per in-scope function with non-pure subjects. Output is keyed by subject; one ConditionTopic per observation. A single subject typically produces 1–8 conditions. Step 5 is also being refactored to per-function in this work (see Phase 0); step 3 stays batched. The rule: LLM call granularity matches the reasoning unit — function-scope reasoning batches functions; subject-within-function reasoning calls per function.
- **A-prefixed shared topic family.** ConditionTopic, ThreatTopic, InvariantTopic all share the `A` prefix and the `NEXT_ADVERSARIAL_PROPERTY_ID` counter. The migration is already done; this work just adds ConditionTopic to the family.
- **Old `Condition` / `ConditionEvaluation` model is retired.** The existing per-template Q/A model in `domain/mod.rs` and its DB tables are replaced wholesale. DB has no production data; no migration shim needed.
- **Loose taxonomy with eight kinds**, plus `Other`. Listed below.
- **Evidence is `Vec<topic::Topic>`** — a list of topic IDs that justify the observation. No structural constraints beyond well-formed topic IDs (cross-pipeline rendered-context validation is deferred to a later refinement pass).
- **Skip-on-no-feature.** Functions without feature links are skipped, same as step 5. Reported as a reconciliation gap. No degraded fallback.
- **Per-subject duplication of cross-cutting observations.** If "caller is unrestricted" applies to every external call in a function, each call site gets its own ConditionTopic. Text is cheap; auditor focus is expensive.
- **No on-the-fly path yet.** Pipeline only. Generator is structured so an on-the-fly caller can reuse it later, but that path is not built or tested in this work.

## What you will build

The work splits into two precursor commits and one additive commit:

**Precursor 1 — Renderer unification + step 5 per-function refactor (Phase 0).** Replace the two existing per-step renderers (`render_batch_for_behavior_extraction`, `render_batch_for_functional_properties`) with one unified `render_batch_for_extraction`. Add new top-level fields (`visibility`, `modifiers`, `state_reads`, `state_writes`, `features` plural). Inline declaration semantics and callee behaviors at reference sites. Switch step 5 (`build_functional_properties`) from per-batch to per-function generation — its output is per-subject, so per-function is the right granularity. Step 3 stays batched. Update prompts to match. No new pipeline step, no new types.

**Precursor 2 — Retire old condition data (Phase 1).** Delete the `Condition` / `ConditionEvaluation` structs, the conditions field on `AuditData`, the conditions/condition_evaluations DB tables, and the `analysis_artifact.rs` references. Bump `ARTIFACT_SCHEMA_VERSION`. Pure deletion — nothing introduced.

**Step 6 commit — Conditions (Phases 2–5).** Purely additive on top of the two precursors:

2. Add `ConditionTopic` and `ConditionKind` to the domain layer.
3. Add `subject_conditions` reverse index to `AuditData`, populated by `rebuild_feature_context`. Inline-conditions injection in the unified renderer (Phase 0 prepared the hook; Phase 3 supplies the data the hook reads).
4. Add the task layer (`extract_conditions_from_batch`) with prompt and JSON schema.
5. Wire it as pipeline step 6 (`build_conditions`) and renumber steps in `run_full_pipeline`.

Each phase is independently verifiable. Do not move on until the previous one compiles, its tests pass, and `cargo test --workspace` is green.

## Phase 0 — Renderer unification (precursor)

### Goal

Replace the two existing extraction renderers (`render_batch_for_behavior_extraction` at `context.rs:2427`, `render_batch_for_functional_properties` at `context.rs:2672`) with one renderer that every pipeline step uses. Enrich its output with fields step 6 will need (and that prior steps benefit from). Switch step 5 (`build_functional_properties`) from per-batch to per-function generation, since its output is per-subject and the function is the natural reasoning unit.

After Phase 0, step 6's renderer requirements are zero — the unified renderer reads `audit_data` and emits whatever metadata previous steps wrote, automatically. Step 6 will adopt the same per-function pattern as the refactored step 5.

This phase preserves existing pipeline output quality at minimum and improves it where added context and per-function attention help. It must end with `cargo build --workspace` and `cargo test --workspace` green.

### Files to change

**`crates/o11a-core/src/collaborator/agent/context.rs`**

1. **Replace the two renderers with one.** New signature:

   ```rust
   pub fn render_batch_for_extraction(
     members: &[topic::Topic],
     audit_data: &AuditData,
   ) -> Option<BatchForExtraction>
   ```

   Same return type. Per-function callers (step 6) pass `&[member]` (slice of length 1); multi-function callers (steps 3, 5) pass their batch. Delete the two old `render_batch_for_*` functions outright once their callers have been updated.

2. **Top-level shape: `subject` vs. `batch` keyed by length.** When `members.len() == 1`, the JSON envelope uses `subject` for the single member object; when `members.len() > 1`, it uses `batch` for the array. The `non_pure_subjects` array stays at the top level in both shapes.

   ```jsonc
   // len == 1 (used by step 6)
   { "non_pure_subjects": [...], "subject": { ... } }

   // len > 1 (used by steps 3, 5)
   { "non_pure_subjects": [...], "batch": [ { ... }, ... ] }
   ```

3. **Per-member fields, top-level on each member object** (these are new vs. the old renderers — every step gets them):

   - `visibility` — `"public" | "external" | "internal" | "private"`. Read from the function's `NamedTopicVisibility` (already on the topic metadata).
   - `modifiers` — array of `{ topic: "Nxx", name: "<modifier name>" }`. Source from the function's AST signature; modifier topics resolve via the existing reference resolution.
   - `state_reads` — array of `topic_id` strings for state variables this function reads. Source from `FunctionModProperties` for the function topic.
   - `state_writes` — array of `topic_id` strings for state variables this function writes. Source from `FunctionModProperties.mutations`.

4. **`features` as a plural array.** Replace the existing single-feature lookup at `context.rs:2731` (`lookup_member_feature`) with a multi-feature variant, e.g. `lookup_member_features`. It returns a `Vec<serde_json::Value>` containing every feature whose behaviors include any of this member's behaviors. Output the array on the member object as:

   ```jsonc
   "features": [
     { "topic": "F3", "name": "...", "description": "...", "requirements": [...] },
     { "topic": "F7", "name": "...", "description": "...", "requirements": [...] }
   ]
   ```

   Drop the multi-feature warn at line 2749 — it's no longer an unexpected state. Skip-on-no-feature stays: zero-element `features` array → skip the member entirely (same effect as today's null-feature skip). Requirements should be deduped across features when the same requirement is linked to more than one.

5. **Inline metadata injection on reference and call-site nodes.** This is the part of the renderer that was unclear in earlier conversation: the existing renderer already descends node-by-node emitting JSON for each AST node. For each node, it can do an `audit_data` lookup keyed on the node's topic (or its `referenced_declaration` topic) and stamp extra fields onto the same JSON object before emitting. There is no separate AST walk — it's the same node-by-node rendering pass.

   When rendering any node that carries a `referenced_declaration`, look up the referenced topic in `audit_data` and stamp:

   - `semantic` — from `audit_data.declaration_semantics`. Format: the FunctionalSemanticTopic's description string.

   When rendering a `FunctionCall` node specifically, additionally stamp:

   - `callee_behaviors` — array of behavior description strings, from `audit_data.member_behaviors` lookup on the callee topic. Empty array if the callee is out-of-scope or has no behaviors yet.

   When rendering a non-pure-subject node (the existing `is_non_pure: true` injection sites), additionally stamp:

   - `functional_purpose` — from `audit_data.subject_purposes`, resolved through `topic_metadata`.
   - `placement_rationale` — from `audit_data.subject_placements`, same pattern.
   - `conditions` — array of `{ topic, description, kind, evidence_topics }` objects, from `audit_data.subject_conditions`. (This entry is empty in Phase 0 because step 6 hasn't run yet; the lookup hook is wired now so step 6's data appears automatically once written, and step 7+ get the same treatment without renderer changes.)

   Each lookup is gated on presence: if `audit_data` has no entry for that topic, the field is omitted. There are no "missing field" placeholder values.

6. **Top-level enumeration maps stay.** `semantics: { topic: { name, semantic } }` and `called_function_behaviors: { topic: { name, behaviors } }` continue to be emitted as deduped lookup tables alongside the inline injections. Inline is for source-order reasoning; top-level is for enumeration. Both serve the LLM differently; cost is small.

**`crates/o11a-core/src/collaborator/agent/pipeline.rs`**

7. **Update `build_behaviors`.** Switch its render call from `render_batch_for_behavior_extraction` to `render_batch_for_extraction`. Same input (multi-function batches), same output type. No other logic changes — step 3 stays batched because behaviors are per-function output and benefit from cross-function callee sharing.

8. **Refactor `build_functional_properties` to per-function.** Step 5's output is per-subject (one `FunctionalPurposeTopic` and one `PlacementRationaleTopic` per non-pure subject); the function — not the batch of five — is the natural reasoning unit. Aligning the LLM call with the function gives full attention budget per function and tightens failure isolation. Concretely:

   - Replace the batch loop with a member loop. Today the function calls `render_batch_for_functional_properties(&batch.members, audit_data)` per batch; the new shape unrolls every batch's members and calls `render_batch_for_extraction(&[member], audit_data)` per member. The unified renderer emits the `subject` envelope shape automatically for length-1 input.
   - The DAG batch infrastructure (`function_dag::build_batches`) is still used to enumerate members — the existing call returns members grouped into DAG-respecting batches and the new per-function caller iterates `batch.members` flat across all batches. No `function_dag` changes are needed.
   - Concurrency model is unchanged in shape, finer in granularity: render all eligible members up front under one lock, spawn one tokio task per rendered member (instead of one per rendered batch), collect all results, commit under a single final lock with `rebuild_feature_context`. Step 5's batches today have no inter-batch dependencies (the comment at the existing `for rendered in rendered_batches` loop says so); the same is true at member granularity.
   - The skip-on-no-feature accounting (`total_skipped_no_feature`) and the reconciliation-gap warning are now computed per member rather than aggregated per batch. Same total count, same reporting cadence.
   - Validation logic in `extract_functional_properties_from_batch` (strict membership filter, dedup by subject_topic, Node-prefix check) carries over unchanged — it already works correctly when the input has one member and the LLM returns a single-function response.

   Cost: ~5× LLM calls, ~1/5 input per call, similar wall-clock (concurrency-bound by tokio), similar total token cost. Quality should improve from full-attention per function; verify against the reference fixture after the refactor lands.

   `build_conditions` (built later in Phase 5 of this doc) follows the same per-function shape, calling the same unified renderer. Steps 5 and 6 share the per-function pattern; step 3 keeps the batch pattern. The rule: **the LLM call granularity matches the unit of context the prompt asks the LLM to reason about.** Behaviors reason at the function level; batch. Purposes/placements/conditions reason at the subject level within a function; per-function.

**`crates/o11a-core/src/collaborator/agent/task.rs`**

9. **Update prompts in steps 3 and 5** to reflect the new envelope and (for step 5) the per-function caller. Specifically:

   - **Step 3's prompt (`extract_behaviors_from_batch`)** — input description now mentions `visibility`, `modifiers`, `state_reads`, `state_writes`, `features` (plural), and inline `semantic` / `callee_behaviors` on reference nodes. The envelope-shape paragraph reads: "the input has a `subject` field (single member) or a `batch` field (multiple members); each member has the fields above." Step 3 always passes batches >= 1, but a batch can legitimately be size 1 (singleton SCC, single-function layer), so the prompt must read both shapes correctly. The "feature context" paragraph becomes "the feature(s) this function contributes to" — singular wording removed.

   - **Step 5's prompt (`extract_functional_properties_from_batch`)** — input description includes the same new fields. The envelope-shape paragraph is *tightened* to subject-only: "the input has a `subject` field with the function/modifier to analyze." The plural-batch framing ("Below are one or more in-scope functions/modifiers" at the top of `EXTRACT_FUNCTIONAL_PROPERTIES_PROMPT`) becomes singular: "Below is one in-scope function or modifier from a smart contract project." The `non_pure_subjects` reference still works — that array's contents are now the single function's non-pure subjects, but the LLM doesn't need to know whether other functions exist. Tighten where the singular framing helps clarity; do not retune the *task* of the prompt (step 5 still extracts purpose + placement, same fields, same validation).

   - **`FUNCTIONAL_PROPERTIES_SCHEMA`** — unchanged. The schema is keyed on the response (`subjects` array), which is the same shape regardless of input cardinality. The prompt shape changes only.

### How to verify Phase 0

- `cargo build --workspace` clean, no new warnings.
- `cargo test --workspace` all pass. Some existing snapshot tests in `context.rs` for the old renderers will need to be re-baselined (the JSON envelope is enriched). The existing `build_functional_properties` tests in `pipeline.rs` may need re-baselining for the per-function granularity (single-task spawning, single-member rendering paths).
- New renderer tests:
  - Single-member call returns `subject` shape; multi-member returns `batch` shape.
  - A member with two features in `feature_behavior_links` produces a `features` array of length 2 with deduped requirements.
  - A member with zero features is omitted from the output (skip behavior preserved).
  - A reference node with `referenced_declaration` pointing at a declaration that has a `FunctionalSemanticTopic` carries inline `semantic`.
  - A `FunctionCall` node whose callee has `BehaviorTopic` entries carries inline `callee_behaviors`; out-of-scope callee yields empty array.
  - A non-pure subject node with `FunctionalPurposeTopic` and `PlacementRationaleTopic` entries carries both inline. Without those entries, neither field appears.
  - The conditions inline hook returns nothing when `subject_conditions` is empty (Phase 0 state); add the test for non-empty in Phase 3.
- New `build_functional_properties` test (or refactor of an existing one):
  - Spawning happens per member: an audit with N feature-linked members produces N spawned tasks, not one per batch.
  - Skip-on-no-feature accounting is correct under per-member iteration (count matches the number of feature-less members across all batches).
- End-to-end smoke run of step 3 + step 5 against the reference audit fixture: step 3 output is equivalent (modulo small wording shifts from enriched input); step 5 output should be **as good or better** under per-function attention. Compare against the prior run — small wording differences are expected; substantive coverage regressions (missing subjects, missing properties) are not.

### Pivotal decisions

- **LLM call granularity matches reasoning unit.** Step 3 (behaviors) reasons at the function level → batches functions. Steps 5 (purposes/placements) and 6 (conditions) reason at the subject level within a function → call per function. The renderer is unified; the *caller* picks the granularity. Step 5's existing batch-of-five was inherited from step 3's shape and is wrong for per-subject output; Phase 0 corrects it.
- **Length-keyed `subject` vs. `batch`.** Step 3 still uses `batch` (multi-function); singleton-batch case still emits `subject`, so step 3's prompt reads both. Step 5 always emits `subject` after the per-function refactor; step 5's prompt is tightened to subject-only.
- **Field availability is data-flow driven.** No step-aware flags. Step 3's input naturally lacks the function's own behaviors (they don't exist yet). Step 5's input has behaviors but no purposes (purposes are step 5's output). Step 6's input has both. Step 7+ extend without renderer changes. This is enforced by always reading `audit_data` and emitting whatever's there.
- **Inline injection coexists with top-level maps.** Both serve the LLM. Inline is for source-order reasoning ("at this call site, what does the callee do?"). Top-level is for enumeration ("which functions does this function call?"). Deduping inline against top-level is not worth the renderer complexity.
- **Subject-local metadata stays subject-local.** Functional purposes, placements, and conditions inline only on the subject node within its own function — not on caller-side reference nodes that point at the callee's internal subjects. The cross-call propagation is via callee behaviors, which already summarize at the right abstraction.
- **`function_dag::build_batches` is unchanged.** The DAG batching infrastructure is reused without modification; per-function callers iterate `batch.members` flat. This avoids touching DAG code in this refactor and keeps the affinity-batching logic available for step 3.

## Phase 1 — Retire the old condition model

### Goal

Remove every trace of the old per-template `Condition` / `ConditionEvaluation` model so the new model lands cleanly without naming collisions or dead branches. Phase 1 must end with `cargo build --workspace` and `cargo test --workspace` green — every site that referenced the old types is touched in this phase.

### Files to change

**`crates/o11a-core/src/domain/mod.rs`**

Delete:
- `pub struct Condition` (around line 368).
- `pub struct ConditionEvaluation` (around line 381).
- The `pub conditions: Vec<Condition>` field on `AuditData` (around line 461).

Keep:
- `pub enum NonPureSubjectType` (around line 1172). It classifies the subject, not the observation, and remains useful as a filter facet on the auditor UI.
- `pub enum SubjectPurity` and the `purity()` impls.
- `ASTNode::Conditional` (an unrelated AST node variant — do not confuse with `Condition`).

**`crates/o11a-core/src/analysis_artifact.rs` (CRITICAL — easy to miss)**

This is the binary serialization format the analyzer writes and the server reads. It holds its own copy of the conditions field. Touch every site:

- Remove `Condition` from the `use crate::domain::{...}` import (around line 50–53).
- Remove `pub conditions: Vec<Condition>` from `AuditDataSnapshot` (around line 99).
- Remove `conditions: audit_data.conditions.clone(),` from `snapshot_from_audit_data` (around line 139).
- Remove the matching `audit_data.conditions = snap.conditions;` line in `apply_snapshot` (around line 164 — read the function body to find it).
- **Bump `ARTIFACT_SCHEMA_VERSION` from `1` to `2`** (around line 63). The doc comment on this constant explicitly requires a bump for any breaking change to `AuditDataSnapshot`. Removing a field is breaking.

**`crates/o11a-core/src/db/mod.rs`**

Delete the table definitions and indices for:
- `conditions` (around line 304).
- `condition_evaluations` (around line 331).

Their CREATE TABLE statements are stand-alone `sqlx::query(...).execute(pool).await?` blocks; remove them entirely. Re-running the schema setup against an empty DB should now skip them.

**`crates/o11a-core/src/collaborator/db/mod.rs`**

Delete the row structs and every function that wrote to or read from the two retired tables:
- `pub struct ConditionRow` (around line 433).
- `pub struct ConditionEvaluationRow` (around line 446).
- All `fn .*condition*` functions in this file that touch the retired tables — `create_condition`, `create_condition_evaluation`, `get_conditions_for_subject`, `get_condition_evaluations`, etc. Confirm by reading the SQL: anything touching `FROM conditions` or `FROM condition_evaluations` goes.

Leave any unrelated function alone.

**`crates/o11a-server/src/api/handlers.rs`**

Delete every HTTP handler that called the now-gone DB functions. Find them by following the compile errors after the DB layer cleanup. Also delete their route registrations — find by grepping `condition` in `crates/o11a-server/src/api/` (the routes file location varies; the grep will take you to it).

**Anywhere else**

Run these greps and touch every match that refers to the old types:
- `grep -rn "domain::Condition\b" crates/`
- `grep -rn "ConditionEvaluation" crates/`
- `grep -rn "domain::Condition[^a-zA-Z]" crates/`

The trailing `\b` (word boundary) and the explicit `domain::` prefix are deliberate: a naive `grep "Condition"` matches `Conditional` (an unrelated AST node), `ConditionalExpression`, etc., which are false positives that will waste time. The existence test `Condition\b` is fine because no word boundary appears between `Condition` and `al` in `Conditional`.

If a test, fixture, or doc string references the old types, remove or rewrite. Do **not** silently leave a `// TODO: re-add` comment — they cause review confusion.

### How to verify Phase 1

- `cargo build --workspace` clean, no new warnings.
- `cargo test --workspace` all pass. (Tests that exercised the old types should be deleted, not skipped.)
- `grep -rn "ConditionEvaluation" crates/` returns zero matches.
- `grep -rn "pub struct Condition\b" crates/` returns zero matches.
- `grep -rn "Vec<Condition>" crates/` returns zero matches.
- `ARTIFACT_SCHEMA_VERSION` is `2`.

### Pivotal decision

The old model survives in no form. We do not preserve it as a "legacy variant" or rename it to `LegacyCondition`. Audit data is regenerated by the pipeline; there is nothing to migrate. Leaving partial old code creates two parallel models for an auditor to navigate, which is exactly the fragmentation we are trying to eliminate.

## Phase 2 — Domain additions

### Goal

Add the `ConditionTopic` `TopicMetadata` variant and the `ConditionKind` enum. They are the persistent shape every other phase produces or consumes.

### Files to change

**`crates/o11a-core/src/domain/mod.rs`**

Add a `ConditionKind` enum, modeled on the existing small enums in the file. Place it near `NonPureSubjectType` (around line 1170) so related taxonomies cluster:

```rust
/// The reasoning angle a Condition observation is taking. Loose taxonomy —
/// the LLM picks; the auditor groups by kind in the review UI; mitigation
/// strategies tend to differ across kinds (which is why the kinds split
/// the way they do). Use `Other` for genuinely novel observations rather
/// than forcing a fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConditionKind {
  /// Can the interaction be triggered, and by whom?
  Reachability,
  /// Is the trigger restricted to authorized parties?
  Authorization,
  /// If the operation produces a wrong outcome, can the system recover?
  Recoverability,
  /// Can a caller control the inputs or state being read here?
  Manipulability,
  /// Can the value being read be out of date?
  Staleness,
  /// Can interleaving operations observe inconsistent state?
  Atomicity,
  /// Can shared resources be drained, locked, or starved?
  ResourceExhaustion,
  /// Genuinely novel observation; description carries the structure.
  Other,
}
```

Add the `ConditionTopic` variant to `TopicMetadata`. Place it next to `ThreatTopic` and `InvariantTopic` (around line 1944). Mirror `FunctionalPurposeTopic` for the field shape:

```rust
/// A condition — a purpose-driven observation about a non-pure subject's
/// interaction surface. Generated in pipeline step 6 from the subject's
/// functional purpose and placement rationale. Each observation is its
/// own ConditionTopic; subjects typically have multiple. Step 7 (threats)
/// reasons from these as structured inputs.
ConditionTopic {
  topic: topic::Topic,
  /// The observation, in prose. One thing the auditor can agree or
  /// disagree with independently.
  description: String,
  /// The non-pure subject this observation is about.
  subject_topic: topic::Topic,
  /// Reasoning angle this observation took.
  kind: ConditionKind,
  /// Topic IDs the LLM cited as justifying the observation. May include
  /// subject siblings, called functions, declarations the function uses,
  /// or documentation topics. Validated for well-formedness only in this
  /// work; cross-pipeline rendered-context validation is a later
  /// refinement.
  evidence_topics: Vec<topic::Topic>,
  author: crate::collaborator::models::Author,
  /// `None` for pipeline-produced entities — see FeatureTopic for
  /// rationale. (Following the FunctionalPurposeTopic pattern; not the
  /// ThreatTopic/InvariantTopic non-Option shape.)
  created_at: Option<String>,
}
```

Update every match against `TopicMetadata` to handle the new variant. After Phase 1 the file's line numbers have shifted, so use anchors instead of line numbers — search for `TopicMetadata::ThreatTopic` (or grep `match.*TopicMetadata` and `TopicMetadata::FunctionalPurposeTopic`) and patch each surrounding match. Most ConditionTopic arms will be identical to the FunctionalPurposeTopic arm.

Method-by-method guidance (find each by its `impl TopicMetadata` method name):
- `scope()` — returns `&Scope::Global` (folded into the same arm as ThreatTopic/InvariantTopic/FunctionalPurposeTopic).
- `topic()` — returns `*topic` (same arm).
- `author()` — returns `Some(*author)` (same arm).
- `description()` — returns `Some(description)` (same arm).
- `subject()` — returns `Some(*subject_topic)` (same arm as ThreatTopic, FunctionalPurposeTopic, PlacementRationaleTopic).
- `created_at()` — returns the `Option<String>` directly (folds into the FunctionalPurposeTopic / PlacementRationaleTopic arm, **not** the ThreatTopic / InvariantTopic arm whose `created_at` is non-Option `String`).

Match exhaustiveness errors after `cargo build` will tell you exactly which arms you missed. If a method has no obvious arm for ConditionTopic, copy the FunctionalPurposeTopic arm — that is almost always the right choice.

The clear-on-rerun retain block in `build_functional_properties` will need extending to also clear ConditionTopic entries when conditions get regenerated; that happens in Phase 5, not here.

### How to verify Phase 2

- `cargo build --workspace` compiles cleanly. Match exhaustiveness errors will tell you which sites you missed.
- Add a unit test in the `domain` test module that constructs a `ConditionTopic`, inserts it into a `TopicMetadata` map, and round-trips it through the existing serialize/deserialize.

### Pivotal decision

`ConditionKind` is a closed enum, not a string. Strings drift, type-checking can't help, and the LLM-driven generator path will deserialize the string anyway via Serde's `untagged`-friendly enum representation. If the LLM picks an off-list kind, deserialization fails and the post-processor logs a warning — exactly the failure surface we want.

## Phase 3 — Reverse index

### Goal

Make conditions queryable per-subject via `audit_data.subject_conditions`, mirroring how `subject_purposes` and `member_behaviors` work.

### Files to change

**`crates/o11a-core/src/domain/mod.rs`**

Add the field to `AuditData`, immediately after `subject_placements` (find the field by name; line numbers have shifted from Phase 1):

```rust
/// Reverse index: non-pure subject topic → A-prefixed condition topics.
/// Each subject has zero or more conditions; later writes append rather
/// than replace (a condition is its own topic, addressed by topic ID, so
/// duplicates would already be distinct topics). Derived from
/// `ConditionTopic.subject_topic`, rebuilt with `rebuild_feature_context`.
pub subject_conditions: BTreeMap<topic::Topic, Vec<topic::Topic>>,
```

Populate it inside `rebuild_feature_context`, in the same block that handles `subject_purposes` and `subject_placements` (find the block by grepping `subject_purposes.clear`):

```rust
audit_data.subject_conditions.clear();
for (cond_topic, metadata) in &audit_data.topic_metadata {
  if let TopicMetadata::ConditionTopic { subject_topic, .. } = metadata {
    audit_data
      .subject_conditions
      .entry(*subject_topic)
      .or_default()
      .push(*cond_topic);
  }
}
```

Initialize the field everywhere `AuditData` is constructed. There is no `Default` impl; sites are explicit struct literals. Find them by grepping `subject_placements: BTreeMap` across the workspace — every site that initializes that field also needs the new `subject_conditions: BTreeMap::new()` line. The compiler will also flag any you missed via "missing field" errors.

### How to verify Phase 3

- `cargo build --workspace` compiles cleanly.
- Add a unit test next to the existing `rebuild_feature_context` tests: insert two `ConditionTopic` entries with the same `subject_topic` and one with a different one, call `rebuild_feature_context`, assert `subject_conditions` has the expected shape (Vec of length 2 under the first subject; Vec of length 1 under the second).
- Add the deferred renderer test from Phase 0: with non-empty `subject_conditions`, the inline conditions hook on a non-pure-subject node emits `conditions: [{topic, description, kind, evidence_topics}, ...]`. The hook itself is wired in Phase 0 against an empty `subject_conditions`; Phase 3 supplies the data and the test confirms the hook emits correctly.

### Pivotal decision

`Vec<Topic>`, not `Vec<ConditionTopic>` or anything richer. The reverse index points at topic IDs; consumers look up the metadata via `topic_metadata`. Same shape as `member_behaviors`. Do not denormalize the description / kind / evidence into the index.

## Phase 4 — Task layer

### Goal

Run the LLM call against the rendered batch JSON and parse the response into well-typed `ParsedCondition` entries.

### Files to change

**`crates/o11a-core/src/collaborator/agent/task.rs`**

Add a section header (mirror the `// Functional Purpose & Placement Rationale (Pipeline Step 5)` comment around line 1330):

```rust
// ============================================================================
// Conditions (Pipeline Step 6)
// ============================================================================
```

Add the prompt constant. Use `EXTRACT_FUNCTIONAL_PROPERTIES_PROMPT` at line 1337 as your structural model. The prompt should:

- Describe the input format (batch with non_pure_subjects list, members with feature/behaviors/semantics/called_function_behaviors, AST nodes with `purity` markers and `functional_purpose` / `placement_rationale` fields on non-pure subjects).
- Explain the task: for **each** topic in `non_pure_subjects`, produce one or more conditions. Each condition is one observation about why the subject's purpose could fail or be violated, given its placement.
- Describe the output schema (subjects array; each entry has subject_topic and a conditions array; each condition has description, kind, evidence_topics).
- Enumerate the eight `ConditionKind` values with one-line descriptions, identical to the doc-comments on the enum variants. Tell the LLM to pick the kind that best names the *reasoning angle* it took, and to use `Other` rather than force-fitting.
- Tell it to ground evidence_topics in topic IDs visible in the rendered batch (state vars, parameters, callees, sibling subjects, semantic blocks). Cross-pipeline validation is deferred, so the prompt is the only enforcement layer for this work.
- Direct it to produce **at least one** condition per subject. A zero-condition subject should be impossible — if no observations about a subject's interaction surface seem worth recording, that itself is signal the subject's purpose is degenerate, and the post-processor will warn but not error.

Define the deserialization types (model on `LLMSubjectFunctionalProperties` / `LLMFunctionalPropertiesResponse` at line 1374):

```rust
#[derive(Deserialize)]
struct LLMCondition {
  description: String,
  kind: ConditionKind,           // serde will validate against the enum
  evidence_topics: Vec<String>,  // parsed to Topic in the post-processor
}

#[derive(Deserialize)]
struct LLMSubjectConditions {
  subject_topic: String,
  conditions: Vec<LLMCondition>,
}

#[derive(Deserialize)]
struct LLMConditionsResponse {
  subjects: Vec<LLMSubjectConditions>,
}
```

Define the JSON schema (mirror `FUNCTIONAL_PROPERTIES_SCHEMA` at line 1385). The `kind` property should constrain to the eight enum string forms via `"enum": ["Reachability", "Authorization", ...]`.

Define the parsed output types:

```rust
pub struct ParsedConditions {
  pub entries: Vec<ParsedSubjectConditions>,
}

pub struct ParsedSubjectConditions {
  pub subject_topic: topic::Topic,
  pub conditions: Vec<ParsedCondition>,
}

pub struct ParsedCondition {
  pub description: String,
  pub kind: ConditionKind,
  pub evidence_topics: Vec<topic::Topic>,
}
```

Define `extract_conditions_from_batch(batch_json: &str, label: &str) -> Result<ParsedConditions, TaskError>`. Mirror `extract_functional_properties_from_batch` at line 1436. Reuse the `parse_non_pure_subjects` helper (already in `task.rs`, used by step 5 at line 1458) to extract the expected subject set from the input batch JSON — do not write a second copy. Validation rules:

- Every topic in the batch's `non_pure_subjects` must appear exactly once in the response (warn on missing or extra; do not fail).
- `subject_topic` must parse and must be a `Topic::Node(_)` variant (warn + skip otherwise — same check step 5 uses at line 1473–1494).
- Subjects outside the batch's `non_pure_subjects` list are dropped (warn + skip; matches step 5's strict membership filter at line 1500).
- Duplicate subjects in the response are deduped — first occurrence wins (matches step 5).
- A subject with zero conditions in the response is logged at `warn` and dropped (no entry produced for that subject — step 7 will not see it). Do not error out the whole batch.
- Each `evidence_topic` in a condition is parsed via `topic::parse_topic`. Parse failures log `warn` and that single evidence topic is dropped; the condition itself is kept with the remaining valid evidence topics.
- Empty `evidence_topics` after parsing is allowed — the LLM might be making an absence-of-code observation that doesn't have a positive code anchor.

### How to verify Phase 4

Add tests in the `task` test module mirroring the parsing tests around line 1789. Cover:

- Well-formed response: full round-trip into `ParsedSubjectConditions`.
- Subject missing from response: warning logged, no entry.
- Subject not in `non_pure_subjects`: rejected.
- Duplicate subject in response: deduped, second entry dropped.
- Malformed topic ID in `subject_topic`: skipped.
- Malformed topic ID in `evidence_topics`: that one topic dropped, condition kept.
- Zero conditions for a subject: warning logged, subject's entry dropped.
- Each `kind` value parses correctly.
- An off-list `kind` value: deserialization fails (this is the schema's job; assert the error path).

### Pivotal decision

Validation drops bad entries but keeps the good. A batch that comes back with one malformed subject still produces conditions for the rest. The auditor sees partial output and a warning trail; the pipeline does not abort. Same behavior as step 5.

## Phase 5 — Pipeline step

### Goal

Wire `build_conditions` into `run_full_pipeline` as step 6 of 6.

### Files to change

**`crates/o11a-core/src/collaborator/agent/pipeline.rs`**

Add `build_conditions` immediately after `build_functional_properties`. Model it on `build_functional_properties` (line 1333). Differences:

- Clear-on-rerun retain (line 1353): clear `ConditionTopic` entries instead of `FunctionalPurposeTopic` / `PlacementRationaleTopic`.
- Early-return condition: if `audit_data.subject_purposes.is_empty()`, log "no functional purposes found, skipping condition generation" and return cleanly. Conditions are downstream of step 5; if step 5 produced nothing there is nothing to generate against.
- Render call: `context::render_batch_for_extraction(&[member], audit_data)` — pass a single-element slice for per-function generation. The unified renderer (Phase 0) emits the `subject` shape automatically for length-1 input. No conditions-specific render function exists; the existing renderer plus inline injections from Phases 0 and 3 produces the right output.
- Extract call: `task::extract_conditions_from_batch(&rendered.json, &rendered.label)` instead of `extract_functional_properties_from_batch`.
- Storage block (line 1467-1494): for each `ParsedSubjectConditions`, allocate one A-topic per `ParsedCondition` via `ids::allocate_adversarial_property_id()`, build the `ConditionTopic` metadata, insert into `topic_metadata`. Do not allocate one per subject — the unit of allocation is the condition, not the subject.

After the storage block, call `domain::rebuild_feature_context(audit_data)` once (matches step 5).

Update `run_full_pipeline` (line 84):

```rust
tracing::info!("[1/6] Semantic Linking");
build_semantic_links(state, audit_id).await?;

tracing::info!("[2/6] Requirement Extraction");
build_requirements(state, audit_id).await?;

tracing::info!("[3/6] Behavior Extraction");
build_behaviors(state, audit_id).await?;

tracing::info!("[4/6] Feature Synthesis");
synthesize_features(state, audit_id).await?;

tracing::info!("[5/6] Functional Purpose & Placement Generation");
build_functional_properties(state, audit_id).await?;

tracing::info!("[6/6] Condition Generation");
build_conditions(state, audit_id).await?;
```

Update the docstring at line 66 to reflect six steps and what step 6 does.

### How to verify Phase 5

- `cargo build --workspace` clean, no new warnings.
- `cargo test --workspace` all pass.

There is no existing integration-test pattern that stubs the LLM at the `build_*` boundary. Coverage for this phase comes from the unit-level tests already added in earlier phases:
- Phase 0: renderer tests cover the input shape going into the LLM call.
- Phase 4: parser tests cover the response shape coming back.
- Phase 3: `rebuild_feature_context` tests cover the storage side.

The only thing not exercised by unit tests is the orchestration in `build_conditions` itself (lock acquisition, batch dispatch, A-topic allocation per condition, final `rebuild_feature_context`). For that, do an end-to-end smoke test: run the analyzer on the seed audit fixture used by other pipeline runs, confirm `audit.json` contains `ConditionTopic` entries with at least one condition per non-pure subject in feature-linked functions, and that every `kind` value in the produced output is from the eight-variant enum. Document the smoke-test command in your PR description rather than committing it as a Rust test (the existing `build_*` functions follow the same convention — they are not unit-tested at the orchestration level).

### Pivotal decision

Conditions are allocated one A-topic per observation, not one per subject. A subject with five conditions consumes five A-IDs. This matches the design that each observation is independently addressable (its own topic, its own conversation, its own approval state).

## Cost notes

For the cost model section in the eventual design doc (not part of this work, but worth recording while building):

After Phase 0, both step 5 and step 6 are per-function (step 3 stays per-batch). For an audit with ~60 in-scope functions:

- **Step 5 call count**: ~60 (one per function), down from ~12 batches × ~5 functions before. ~5× more calls; ~1/5 the size per call. Total tokens roughly equal. Wall-clock similar (concurrency-bound by tokio).
- **Step 6 per-call input**: same as step 5 plus inline `functional_purpose` and `placement_rationale` on each non-pure subject node. Roughly 10–20% larger by token count than step 5's per-call input.
- **Step 6 per-call output**: ~1–8 conditions per subject × the function's non-pure subjects (typically 2–6). Each condition is ~one short prose sentence, a kind token, and 0–4 topic IDs. Roughly 2–4× the per-call output of step 5.
- **Step 6 total call count**: ~60, same as step 5 after the refactor.

Quality argument for the per-function shift: full attention budget per function for the per-subject reasoning steps, sharper failure isolation, and consistent granularity across steps 5 and 6.

## Out of scope

These are tracked decisions; do not build them in this work:

- **On-the-fly condition generation** when an auditor adds a new subject post-pipeline. The generator is structured so a single-subject caller can reuse it; the call site is not built.
- **Cross-pipeline rendered-context validation** of `evidence_topics`. Deferred to a refinement pass that will tighten validation across all steps that accept topic-typed LLM output.
- **Adversarial second pass** that critiques each condition. Specified but not implemented; same deferral as step 5's adversarial pass.
- **Sequence-level conditions** on consecutive non-pure subjects. The `SemanticInteractionSequence` data model is not added here.
- **Step 7 (threats) and step 8 (invariants).** This work makes step 7 unblockable, not implemented.

## Final verification

After all six phases land (Phase 0 + Phase 1 as precursors; Phases 2-5 as the additive step-6 commit):

- `cargo build --workspace` clean, no new warnings.
- `cargo test --workspace` all green.
- `grep -rn "ConditionEvaluation" crates/` returns zero matches.
- `grep -rn "pub struct Condition\b" crates/` returns zero matches.
- `grep -rn "render_batch_for_behavior_extraction\|render_batch_for_functional_properties" crates/` returns zero matches (both old renderers replaced by `render_batch_for_extraction`).
- `grep -rn "TopicMetadata::ConditionTopic" crates/` returns matches in the domain layer, the rebuild_feature_context, the pipeline step, and (optionally) the renderer if it inspects the variant by name — but **not** in any HTTP handler (the API layer is the next layer up; it is not part of this work).
- `ARTIFACT_SCHEMA_VERSION` in `analysis_artifact.rs` is `2`.
- A trial run of the full pipeline on a known audit produces ConditionTopic entries on every non-pure subject in feature-linked functions, with at least one condition per subject and every kind represented at least once across the run. Step 3 and step 5 outputs against the same fixture should be equivalent or improved — not regressed.
