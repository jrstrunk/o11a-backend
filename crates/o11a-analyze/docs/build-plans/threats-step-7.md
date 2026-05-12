# Step 7 — Threats on Conditions

This is the implementation plan for pipeline step 7: generating **threats** — adversarial scenarios that falsify a specific condition — for every non-pure subject that received conditions in step 6. Threats anchor 1:1 to the condition they falsify; one condition can be the target of many threats. Step 8 (invariants) will consume these threats; this step makes step 8 unblockable but does not implement it.

Read `conditions-step-6.md` first; this doc assumes its phases landed and reuses its structural patterns wholesale (per-function generation, A-prefixed topic family, unified renderer, post-processor validation shape).

## Summary of decisions

These were settled during design and should not be re-litigated during implementation:

- **Threats are adversarial inversions of a specific condition.** Each threat states a scenario in which the named condition fails to hold. The reasoning chain (per SPEC's "Conditions vs. Invariants"): purpose → conditions → threats → invariants. Step 7 produces threats from the conditions step 6 generated; step 8 will produce invariants as the codebase-level defenses against those threats.
- **One condition can be the target of many threats; each threat names exactly one condition.** `falsifies_condition: topic::Topic` lives on the `ThreatTopic` (singular). If the same scenario plausibly falsifies two conditions, emit two threats with identical descriptions and different `falsifies_condition` links — text is cheap, attribution is expensive (same logic step 6 used for cross-cutting condition duplication).
- **Threat actor is structured.** A closed `ThreatActor` enum lives on each threat as `controlled_by: ThreatActor` (singular — one primary actor per threat). The enum is flat; no role-name carrying. Multi-actor scenarios are captured in the description if they arise.
- **No `ThreatKind` enum.** Structural classification is inherited from `falsifies_condition.kind`; the auditor UI groups threats by the linked condition's kind. Adding an explicit `ThreatKind` later is additive if real output shows the inherited grouping is insufficient.
- **Empty-threats per condition is allowed, with explicit rationale.** A condition that the LLM considered and found no falsifier for produces a `no_threat_rationale: String` rather than a silent skip. This preserves the audit signal that the assertion was reviewed and discharged.
- **Threat evidence stays inside the subject's containing function.** `evidence_topics: Vec<topic::Topic>` on a threat may reference: the subject node itself, descendants of the subject node, sibling statements in the same semantic block, the subject's containing function, and the function's signature/modifiers/parameters. **Cross-function evidence is invalid on threats** — that surface belongs to invariants (step 8), which will point outside the subject to the codebase-level defenses. For "absence of X enables this threat" cases (e.g., no reentrancy guard), the evidence points to the subject node showing the absence, not to the missing element.
- **Per-function generation, per-subject output.** One LLM call per in-scope function with conditions. Same call shape as step 6: render the function via the unified renderer (which already inlines conditions on non-pure-subject nodes after step 6 phase 3), spawn one tokio task per member, parse and validate, single final `rebuild_feature_context`.
- **A-prefixed topic family.** `ThreatTopic` already shares the `A` prefix and `NEXT_ADVERSARIAL_PROPERTY_ID` counter with `ConditionTopic` and `InvariantTopic`. No counter changes.
- **No adversarial critique pass.** Same out-of-scope status as step 5/6 critique. Defer.
- **No on-the-fly path.** Pipeline only. Generator is structured so a single-subject caller can reuse it later, but that path is not built or tested here.
- **`security_notes` is a prompt segment, not a renderer field.** Audit-wide framing (loaded by `analysis.rs:49` from `security.md`) is passed as a separate string into `extract_threats_from_batch` and prepended to the LLM call as system-context. It is not stamped into the rendered batch JSON.
- **Retire the `Threat` struct and its DB tables.** `domain/mod.rs:409` defines `pub struct Threat { invariant_topics: Vec<Topic> }` — pure denormalization that step 8 would extend. Step 6 retired the old `Condition` struct in its phase 1; the parallel cleanup for `Threat` lands here so step 8 starts from a reverse-index pattern (`threat_invariants` built from `InvariantTopic.threat_topic`) rather than inheriting denormalization. The legacy DB tables `threats`, `invariants`, `invariant_source_topics`, and `threat_feature_links` (`db/mod.rs:172, :200, :221, :276`) are also retired; the snapshot (`analysis_artifact.rs:98–100, :137–139, :162–164`) is the canonical store for impact-analysis links and the in-memory invariant denormalization. The `Invariant` struct itself stays — step 8 will deal with it in lockstep with its own work.
- **`no_threat_rationale` is a comment on the condition topic, not a field on `ConditionTopic`.** The pipeline data flow is one-way (purpose → conditions → threats → invariants); persisting threat-side rationale on a condition topic would create back-flow and conflate layers. Instead, when the LLM emits `no_threat_rationale` for a condition, the pipeline posts an agent-authored comment on that condition topic with the rationale text. This uses the existing collaborator comment surface (the same one auditors and agents use for approvals and disagreements), keeps the rationale durable across pipeline reruns, and lets the auditor reply or contest in the same thread.
- **Threat description prose stays actor-agnostic.** The structured `controlled_by` field is the canonical home for actor identity; the description names the scenario without naming the actor. This keeps the actor classification a separately scrutinized artifact — the auditor can approve the description while disagreeing with the actor (or vice versa) without the prose forcing a paired interpretation. Same disagreement-axis logic that put `ConditionKind` in a structured field rather than the description.
- **Step 7 reruns proactively clear downstream invariant data.** A rerun clears `ThreatTopic` entries (the threats themselves) plus `InvariantTopic` entries and `audit_data.invariants` (the downstream Invariant denormalization). It also prunes `audit_data.threat_feature_links` entries whose `threat_topic` no longer exists in `topic_metadata` after the clear (orphaned impact-analysis links). It does **not** clear non-orphaned `threat_feature_links` — impact analysis is separate auditor work and surviving links should re-attach to threats whose `topic` is preserved across the rerun. (In practice, A-prefixed IDs are reallocated on rerun, so most links will be orphaned; this is the existing reality across all A-family pipeline steps and is not new to step 7.)

## What you will build

The work splits into one precursor commit (legacy retirement) and one additive commit (the new step). Phases:

**Precursor — Retire the `Threat` struct and legacy DB tables (Phase 1).** Parallel to step 6 phase 1. Delete `pub struct Threat`, the `pub threats: BTreeMap<Topic, Threat>` field on `AuditData`, the matching snapshot field, the `threats`/`invariants`/`invariant_source_topics`/`threat_feature_links` DB tables, the row structs, the API handlers, and the route registrations. The snapshot becomes the canonical store for impact-analysis links. Bump `ARTIFACT_SCHEMA_VERSION` from `2` to `3`. Pure deletion — nothing introduced.

**Step 7 commit — Threats (Phases 2–6).** Purely additive on top of the precursor:

2. **Domain additions.** Widen `ThreatTopic` with `falsifies_condition`, `controlled_by`, `evidence_topics`; flip `created_at` to `Option<String>`. Add `ThreatActor` enum.
3. **Reverse indexes.** Add `subject_threats` and `condition_threats` to `AuditData`, populated by `rebuild_feature_context`. Extend the renderer's existing per-subject inline injection to stamp `threats` on non-pure-subject nodes (so step 8 inherits it for free, the same way step 6 prepared `conditions` for downstream consumers).
4. **Task layer.** Add `extract_threats_from_batch` with prompt, JSON schema, and post-processor. Validation enforces the in-function evidence scope and the actor-agnostic prose rule.
5. **Pipeline step.** Wire `build_threats` into `run_full_pipeline` as step 7 of 7. Storage block posts agent comments on condition topics for `no_threat_rationale` entries. Proactive clear of `InvariantTopic` and `audit_data.invariants` on rerun. Renumber the existing `[1/6]`–`[6/6]` log lines.
6. **README and SPEC sync.** Update root `README.md`'s outdated condition Q/A framing and dependent threat description to the post-step-7 model. Lighter touchups in `SPEC.md` for the parts that step 6 didn't already rewrite (the threat description references the chain in passing); add the `controlled_by` and falsifies-condition-link wording.

Each phase is independently verifiable. Do not move on until the previous one compiles, its tests pass, and `cargo test --workspace` is green.

## Phase 1 — Retire the `Threat` struct and legacy DB tables

### Goal

Remove the pre-A-prefix `Threat`/`Invariant` denormalization layer and the legacy DB tables that mirrored it, so the new step lands cleanly without naming collisions or two-source-of-truth ambiguity. The snapshot (`analysis_artifact.rs`) becomes the canonical store for `threat_feature_links` (impact analysis) and for the in-memory invariant denormalization (until step 8 retires `Invariant` on its own schedule). Phase 1 must end with `cargo build --workspace` and `cargo test --workspace` green.

### Files to change

**`crates/o11a-core/src/domain/mod.rs`**

Delete:
- `pub struct Threat` (around line 408–413).
- `pub threats: BTreeMap<topic::Topic, Threat>` field on `AuditData` (around line 487).

Keep:
- `pub struct Invariant` and `pub invariants: BTreeMap<topic::Topic, Invariant>` — step 8 still uses them.
- `pub struct ThreatFeatureLink`, `pub enum ThreatFeatureRelation`, `pub enum ThreatSeverity` — impact analysis still uses them; the link list lives at `audit_data.threat_feature_links`.
- `TopicMetadata::ThreatTopic` — this is the new shape Phase 2 widens.

**`crates/o11a-core/src/analysis_artifact.rs` (CRITICAL — same shape as step 6 phase 1)**

- Remove `Threat` from the `use crate::domain::{...}` import (around line 50–53).
- Remove `pub threats: BTreeMap<topic::Topic, Threat>` from `AuditDataSnapshot` (around line 99).
- Remove `threats: audit_data.threats.clone(),` from `snapshot_from_audit_data` (around line 138).
- Remove `audit_data.threats = snap.threats;` from `apply_snapshot` (around line 163).
- **Bump `ARTIFACT_SCHEMA_VERSION` from `2` to `3`.** The doc comment on this constant requires a bump for any breaking change to `AuditDataSnapshot`. Removing a field is breaking.
- Update the doc comment at line 32 that lists `threat_feature_links, threats, invariants` — drop `threats` from that bullet.

`audit_data.invariants` and `audit_data.threat_feature_links` stay in the snapshot. Only the `threats` field is removed.

**`crates/o11a-core/src/db/mod.rs`**

Delete the table definitions and indices for:
- `threats` (around line 172) and its `idx_threats_*` indices.
- `invariants` (around line 200) and its `idx_invariants_*` index.
- `invariant_source_topics` (around line 221) and its index.
- `threat_feature_links` (around line 276) and its index.

These are stand-alone `sqlx::query(...).execute(pool).await?` blocks; remove them entirely. Re-running the schema setup against an empty DB should now skip them.

**`crates/o11a-core/src/collaborator/db/mod.rs`**

Delete the row structs and every function that wrote to or read from the four retired tables. Find by reading the SQL: anything touching `FROM threats`, `FROM invariants`, `FROM invariant_source_topics`, or `FROM threat_feature_links` goes. Common targets: `ThreatRow`, `InvariantRow`, `InvariantSourceTopicRow`, `ThreatFeatureLinkRow`, plus their `create_*` / `get_*` / `delete_*` accessor functions.

**`crates/o11a-server/src/api/handlers.rs` and `crates/o11a-server/src/api/routes.rs`**

Delete every HTTP handler that called the now-gone DB functions, and their route registrations. Find by following the compile errors after the DB layer cleanup, or by grepping `threat\|invariant` in `crates/o11a-server/src/api/`. The collaborator module's snapshot read/write paths replace what these handlers used to do.

**Anywhere else**

Run these greps and touch every match that refers to the old types:
- `grep -rn "domain::Threat\b" crates/`
- `grep -rn "Vec<Threat>\|BTreeMap<.*, Threat>" crates/`
- `grep -rn "audit_data\.threats" crates/`

The trailing `\b` (word boundary) on `domain::Threat` is deliberate — a naive `Threat` match catches `ThreatTopic`, `ThreatActor`, `ThreatSeverity`, `ThreatFeatureRelation`, `ThreatFeatureLink`, all of which are kept.

If a test, fixture, or doc string references the old types, remove or rewrite. Do not leave a `// TODO: re-add` comment — they cause review confusion.

### How to verify Phase 1

- `cargo build --workspace` clean, no new warnings.
- `cargo test --workspace` all pass. (Tests that exercised the old types should be deleted, not skipped.)
- `grep -rn "pub struct Threat\b" crates/` returns zero matches.
- `grep -rn "BTreeMap<.*, Threat>" crates/` returns zero matches.
- `grep -rn "audit_data\.threats" crates/` returns zero matches.
- `grep -rn "FROM threats\|FROM invariants\|FROM invariant_source_topics\|FROM threat_feature_links" crates/` returns zero matches.
- `ARTIFACT_SCHEMA_VERSION` is `3`.

### Pivotal decision

The `Invariant` struct stays; only `Threat` is retired. Step 8 (invariants) will retire `Invariant` on its own schedule, in the same shape as this phase: delete the struct, drop the snapshot field, rely on a `threat_invariants` reverse index built from `InvariantTopic.threat_topic`. Doing it now would land step-8-shaped work in a step-7 commit and create a phase that nothing in this work consumes.

## Phase 2 — Domain additions

### Goal

Widen the persistent shape so step 7 has somewhere to write its outputs and step 8 has somewhere to read from.

### Files to change

**`crates/o11a-core/src/domain/mod.rs`**

Add `ThreatActor` near `ThreatSeverity` and `ThreatFeatureRelation` (around line 280–400). Closed enum, flat:

```rust
/// The party whose action drives the threat scenario. One primary actor
/// per threat; multi-actor coordination scenarios are captured in the
/// threat's description prose. Loose taxonomy; the LLM picks; the auditor
/// groups by actor in the review UI. `Other` is the escape hatch for
/// scenarios that don't fit a named variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThreatActor {
  /// An unauthenticated external caller of a public/external entry point.
  Caller,
  /// A role-gated party (admin, owner, governor, operator). The specific
  /// role lives in the threat's description, not in this variant.
  PrivilegedRole,
  /// A third-party contract — typically the callee in an external call,
  /// an oracle the subject reads from, or a token the subject interacts
  /// with.
  External,
  /// A miner, sequencer, validator, or other party with control over
  /// transaction ordering or inclusion.
  BlockProducer,
  /// A peer in the protocol's economic model (LP, borrower, counterparty
  /// to a trade) whose interests differ from the subject's purpose.
  Counterparty,
  /// The contract itself reentering through an external call.
  Self_,
  /// No constraint on who triggers the scenario; permissionless.
  AnyParty,
  /// Genuinely novel actor classification; description carries the
  /// structure.
  Other,
}
```

(The variant name is `Self_` because `Self` is a Rust keyword; serde rename to `"Self"` in the JSON schema below.)

Update `ThreatTopic` (around line 2007). Diff:

- Add `falsifies_condition: topic::Topic`. Required, single. The condition this threat is the adversarial inversion of.
- Add `controlled_by: ThreatActor`. Required, single.
- Add `evidence_topics: Vec<topic::Topic>`. Bounded to in-function scope (validation in Phase 3). Empty is allowed for absence-of-X threats whose "evidence" is the subject itself — but in practice the post-processor will populate at least the `subject_topic` if the LLM emits zero topics, so the field is rarely empty by the time it lands.
- Flip `created_at: String` → `created_at: Option<String>`. Matches the `FunctionalPurposeTopic` / `ConditionTopic` convention for pipeline-produced topics.

Resulting shape:

```rust
ThreatTopic {
  topic: topic::Topic,
  description: String,
  /// The non-pure subject this threat belongs to.
  subject_topic: topic::Topic,
  /// The condition (assertion) this threat is the adversarial inversion of.
  /// One threat targets exactly one condition; one condition can be targeted
  /// by many threats. Auditor disagreeing with this threat does not
  /// invalidate the underlying condition.
  falsifies_condition: topic::Topic,
  /// The party whose action drives the scenario.
  controlled_by: ThreatActor,
  /// Topic IDs the LLM cited as the vulnerable code surface this threat
  /// plays out across. Constrained to the subject's containing function:
  /// the subject node, its descendants, sibling statements in the same
  /// semantic block, and the function's signature/modifiers/parameters.
  /// Cross-function anchors are an invariant-layer concern (step 8).
  evidence_topics: Vec<topic::Topic>,
  author: crate::collaborator::models::Author,
  /// `None` for pipeline-produced entities — see FeatureTopic for rationale.
  created_at: Option<String>,
  /// Severity is assigned during impact analysis; None means pending.
  severity: Option<ThreatSeverity>,
}
```

The `Threat` struct is gone after Phase 1. Step 7 does not insert anything into a parallel `audit_data.threats` (the field is also gone). Threat→invariant linkage is established in step 8 via a `threat_invariants` reverse index built from `InvariantTopic.threat_topic`.

Update every match against `TopicMetadata`. The new fields don't change the variant signature in ways that affect non-`subject()`/`description()`/`author()`/`created_at()` arms — the additional fields are read inside step 7's code paths only. The `created_at` flip moves `ThreatTopic` from the non-Option arm to the Option arm of `TopicMetadata::created_at()`. Match exhaustiveness errors after `cargo build` will tell you exactly which arms you missed.

### How to verify Phase 2

- `cargo build --workspace` compiles cleanly.
- Existing tests still pass.
- New unit test in the `domain` test module: construct a `ThreatTopic` with all new fields, insert into a `TopicMetadata` map, round-trip through the existing serialize/deserialize.
- A test confirming `Self_` serializes as `"Self"` and parses back.

### Pivotal decision

`ThreatActor` is a closed enum, not a string. Same rationale as `ConditionKind`: strings drift, type-checking can't help, and the LLM-driven generator path will deserialize the string anyway via Serde's enum representation. If the LLM picks an off-list actor, deserialization fails and the post-processor logs a warning — exactly the failure surface we want.

## Phase 3 — Reverse indexes and renderer hook

### Goal

Make threats queryable per-subject and per-condition, and make them visible to downstream consumers (step 8) through the unified renderer's existing per-subject inline-injection mechanism.

### Files to change

**`crates/o11a-core/src/domain/mod.rs`**

Add two fields to `AuditData`, immediately after `subject_conditions` (find by name):

```rust
/// Reverse index: non-pure subject topic → A-prefixed threat topics.
/// Each subject has zero or more threats; later writes append rather than
/// replace (a threat is its own topic, addressed by topic ID). Derived
/// from `ThreatTopic.subject_topic`, rebuilt with `rebuild_feature_context`.
pub subject_threats: BTreeMap<topic::Topic, Vec<topic::Topic>>,
/// Reverse index: A-prefixed condition topic → A-prefixed threat topics
/// that target it. Each condition has zero or more threats. Used for
/// the condition-detail UI ("show all threats falsifying this assertion")
/// and for re-derivation triggers (auditor edits condition X → re-run
/// threats anchored to X). Derived from `ThreatTopic.falsifies_condition`,
/// rebuilt with `rebuild_feature_context`.
pub condition_threats: BTreeMap<topic::Topic, Vec<topic::Topic>>,
```

Populate inside `rebuild_feature_context`, in the same block that handles `subject_conditions`:

```rust
audit_data.subject_threats.clear();
audit_data.condition_threats.clear();
for (threat_topic, metadata) in &audit_data.topic_metadata {
  if let TopicMetadata::ThreatTopic { subject_topic, falsifies_condition, .. } = metadata {
    audit_data
      .subject_threats
      .entry(*subject_topic)
      .or_default()
      .push(*threat_topic);
    audit_data
      .condition_threats
      .entry(*falsifies_condition)
      .or_default()
      .push(*threat_topic);
  }
}
```

Initialize both fields everywhere `AuditData` is constructed. Find by grepping `subject_conditions: BTreeMap` — every site needs both new lines added. Compiler will flag misses.

**`crates/o11a-core/src/collaborator/agent/context.rs`**

Extend the existing per-subject inline injection on non-pure-subject nodes (the same hook step 6 uses for `conditions`). Add:

```rust
// stamp `threats` on the subject node when subject_threats has entries
"threats" -> array of {
  topic, description, falsifies_condition, controlled_by, evidence_topics
}
```

Same pattern as the conditions hook: gated on presence (omit field if `audit_data.subject_threats` has no entry for that topic), no placeholder values, descriptions resolved through `topic_metadata` lookup.

This hook is empty in step 7 itself (step 7's data lands and renders for step 8). It is added now so step 8 inherits inline threats for free, the same way step 6 prepared the conditions hook in phase 0 before step 6 had data to write.

### How to verify Phase 3

- `cargo build --workspace` clean.
- Add a unit test next to the existing `rebuild_feature_context` tests for conditions: insert two `ThreatTopic` entries with the same `subject_topic` and different `falsifies_condition`s, plus one with a different subject, call `rebuild_feature_context`, assert both reverse indexes have the expected shape.
- Renderer test: with non-empty `subject_threats`, the inline hook on a non-pure-subject node emits `threats: [{topic, description, falsifies_condition, controlled_by, evidence_topics}, ...]`. With empty `subject_threats`, the field is omitted.

### Pivotal decision

Both indexes carry `Vec<Topic>`, not richer structures. Consumers look up the metadata via `topic_metadata`. Same shape rule as `subject_conditions` and `member_behaviors` — never denormalize description/kind/etc. into reverse indexes.

## Phase 4 — Task layer

### Goal

Run the LLM call against the rendered batch JSON (which already has conditions inlined per step 6 phase 3) and parse the response into well-typed `ParsedThreat` entries with strict in-function evidence validation.

### Files to change

**`crates/o11a-core/src/collaborator/agent/task.rs`**

Add a section header after the conditions section:

```rust
// ============================================================================
// Threats (Pipeline Step 7)
// ============================================================================
```

Add the prompt constant (`EXTRACT_THREATS_PROMPT`). Use `EXTRACT_CONDITIONS_PROMPT` as your structural model. The prompt should:

- Describe the input format (the unified renderer's `subject` envelope, including the inline `conditions` array on each non-pure subject — this is the load-bearing input).
- Explain the task: for **each condition** on each non-pure subject, generate zero or more **threats** — concrete adversarial scenarios in which the named assertion fails to hold. Phrase each threat as a scenario, not as a guard recommendation: "the deterministic token address can be pre-computed and `createPair` called first, bricking deployment" — never "the function should use a commit-reveal scheme."
- **Description prose stays actor-agnostic.** The structured `controlled_by` field is the canonical home for actor identity. The description must not name the actor ("an attacker," "a miner," "the admin"). Phrase scenarios in the passive or in terms of the mechanism: "the value can be reordered before the dependent read commits" — not "a miner reorders the value before the dependent read commits." This keeps the actor classification independently approvable: an auditor can agree with the scenario and disagree with the actor classification (or vice versa) without the prose forcing a paired interpretation. Include the distinguishing test in the prompt: "if your description starts with 'an attacker,' 'a miner,' 'the caller,' or any noun naming a party, restate the scenario without naming the party."
- **Bound the evidence scope explicitly.** The prompt must say: "evidence_topics may reference only topics inside the subject's containing function: the subject itself, its descendants, sibling statements in the same semantic block, the function's signature, and the function's modifiers and parameters. Cross-function topics (other functions, other declarations, documentation) are invalid here — those are invariant-layer anchors and will be rejected." Include the rationale: "threats describe the vulnerable surface; invariants describe the codebase-level defenses that protect it."
- **Frame absence as in-subject evidence.** Include this guidance verbatim: "If the threat is enabled by the absence of something (e.g., no reentrancy guard, no slippage check, no access control modifier), point evidence_topics at the subject node or the function's modifier list to anchor the absence — do not point at the missing element, since by definition it is not in the codebase."
- Describe the output schema: per-condition entries, each with `falsifies_condition`, an array of `threats`, and an optional `no_threat_rationale` for empty arrays.
- Enumerate the eight `ThreatActor` values with one-line descriptions matching the enum doc-comments. Tell the LLM to pick the primary actor; multi-actor scenarios go in the description.
- **Empty-threats handling.** Tell the LLM: "if a condition has no plausible falsifying scenario you can identify (because the assertion is enforced by Solidity itself, by an upstream type constraint, or by a structural property of the codebase), emit an empty `threats` array and a `no_threat_rationale` string explaining why. Do not invent threats to fill the slot; the rationale is the audit signal."
- Tell it the threat description should name a concrete scenario, not abstractions ("gas exhaustion in the unbounded loop over `users`" — not "this is vulnerable to DOS").
- Reference the audit-wide context: "the `security_notes` block above this prompt may name known threats, role definitions, and security considerations specific to this audit. Use it to pick realistic actors and to avoid restating defenses the auditor has already documented."

Define the deserialization types:

```rust
#[derive(Deserialize)]
struct LLMThreat {
  description: String,
  controlled_by: ThreatActor,
  evidence_topics: Vec<String>,
}

#[derive(Deserialize)]
struct LLMConditionThreats {
  falsifies_condition: String,        // condition topic
  threats: Vec<LLMThreat>,
  no_threat_rationale: Option<String>, // required when threats is empty
}

#[derive(Deserialize)]
struct LLMSubjectThreats {
  subject_topic: String,
  conditions: Vec<LLMConditionThreats>,
}

#[derive(Deserialize)]
struct LLMThreatsResponse {
  subjects: Vec<LLMSubjectThreats>,
}
```

Define the JSON schema (`THREATS_SCHEMA`) mirroring `CONDITIONS_SCHEMA`. The `controlled_by` property constrains to the eight enum string forms via `"enum": ["Caller", "PrivilegedRole", "External", "BlockProducer", "Counterparty", "Self", "AnyParty", "Other"]` — note `"Self"`, not `"Self_"`. The `no_threat_rationale` is an optional string.

Define the parsed output types:

```rust
pub struct ParsedThreats {
  pub entries: Vec<ParsedSubjectThreats>,
}

pub struct ParsedSubjectThreats {
  pub subject_topic: topic::Topic,
  pub conditions: Vec<ParsedConditionThreats>,
}

pub struct ParsedConditionThreats {
  pub falsifies_condition: topic::Topic,
  pub threats: Vec<ParsedThreat>,
  pub no_threat_rationale: Option<String>,
}

pub struct ParsedThreat {
  pub description: String,
  pub controlled_by: ThreatActor,
  pub evidence_topics: Vec<topic::Topic>,
}
```

Define `extract_threats_from_batch(batch_json: &str, label: &str, security_notes: Option<&str>) -> Result<ParsedThreats, TaskError>`. Mirror `extract_conditions_from_batch`. The `security_notes` parameter is prepended to the LLM call as system-context if `Some`. Validation rules:

- Every subject in the batch's `non_pure_subjects` that has a non-empty `conditions` array in its rendered JSON must appear in the response (warn on missing or extra; do not fail the batch).
- `subject_topic` must parse and must be a `Topic::Node(_)` variant (warn + skip otherwise).
- Subjects outside the batch are dropped (warn + skip).
- Duplicate subjects in the response are deduped — first occurrence wins.
- For each `LLMConditionThreats`:
  - `falsifies_condition` must parse to an `A`-prefixed `Topic` (warn + skip otherwise).
  - The condition topic must appear in the subject's inline `conditions` array in the rendered batch JSON (cross-reference; warn + drop the entry if not).
  - If `threats` is empty and `no_threat_rationale` is `None`, warn and drop (the LLM left a slot blank without explanation — same shape as a zero-condition subject in step 6).
  - If `threats` is non-empty and `no_threat_rationale` is `Some`, warn and drop the rationale (kept the threats, discarded the contradictory rationale).
- For each threat:
  - `controlled_by` parses as the enum (the schema enforces this; assert the error path on off-list values).
  - Each `evidence_topic` parses via `topic::parse_topic`. Parse failures log `warn` and the single topic is dropped.
  - **In-function scope validation**: every parsed evidence topic must be either (a) the subject_topic, (b) a descendant of the subject_topic in the AST (use existing `ASTNode` walk helpers), (c) a sibling in the same semantic block as the subject, (d) the subject's containing function topic, or (e) a parameter or modifier topic on that function. Topics outside this set log `warn` and are dropped from the threat's evidence list (the threat itself is kept). If the LLM emits zero topics or all topics fail this check, the post-processor populates `evidence_topics` with `[subject_topic]` so the threat carries at least its own anchor.
  - Empty `description` after `trim()` → warn + drop the threat.
  - Description that starts with a party-naming noun (`/^(an? attacker|the attacker|an? user|the caller|an? caller|an? miner|the miner|an? validator|an? sequencer|the admin|an? admin|the owner|the operator|the ?contract|an? counterparty|the counterparty)\b/i` or similar lightweight check) → warn but keep. Tracked as a prompt-quality signal in the smoke run; if it spikes, tighten the prompt rather than escalating to drop.

### How to verify Phase 4

Add tests in the `task` test module mirroring the conditions parser tests:

- Well-formed response: full round-trip into `ParsedSubjectThreats`.
- Multiple threats targeting the same condition: kept (1:N is the supported shape).
- Subject missing from response: warning logged, no entry.
- Subject not in `non_pure_subjects`: rejected.
- Duplicate subject in response: deduped.
- Malformed topic ID in `subject_topic`: skipped.
- `falsifies_condition` not in the subject's inline conditions array: dropped with warning.
- Empty `threats` with no `no_threat_rationale`: dropped with warning.
- Empty `threats` with a `no_threat_rationale`: kept, rationale preserved.
- Non-empty `threats` with a `no_threat_rationale`: rationale dropped, threats kept.
- Each `ThreatActor` value parses correctly under its JSON name (including `"Self"` → `Self_`).
- An off-list `controlled_by`: deserialization fails (schema enforcement; assert the error path).
- Evidence topic outside the containing function: dropped with warning.
- Evidence topic that is a descendant of the subject: kept.
- Evidence topic that is the function's modifier: kept.
- Evidence topic that is the subject's containing function: kept.
- Zero valid evidence topics after validation: post-processor populates `[subject_topic]`.
- Description starting with a party-naming noun: warning logged, threat kept.

### Pivotal decision

Validation drops bad entries but keeps the good. A batch with one malformed evidence topic still produces threats with the remaining valid topics. The auditor sees partial output and a warning trail; the pipeline does not abort. Same behavior as step 5 and step 6. The actor-naming check is the one exception — descriptions are kept on warning rather than dropped, because a description with a party noun still carries useful scenario content; tightening the prompt is the right intervention if the warn rate spikes, not silently discarding output.

## Phase 5 — Pipeline step

### Goal

Wire `build_threats` into `run_full_pipeline` as step 7 of 7.

### Files to change

**`crates/o11a-core/src/collaborator/agent/pipeline.rs`**

Add `build_threats` immediately after `build_conditions`. Model on `build_conditions`. Differences:

- Clear-on-rerun retain: clear `ThreatTopic` entries from `topic_metadata`. Also proactively clear `InvariantTopic` entries from `topic_metadata` and clear `audit_data.invariants` (the `Invariant` struct denormalization that step 8 populates) — a deleted threat orphans its invariants, and the audit data must be internally consistent at step boundaries. Then prune `audit_data.threat_feature_links` of any entry whose `threat_topic` no longer exists in `topic_metadata` after the clear (orphaned impact-analysis links). **Do not** wipe `threat_feature_links` wholesale — non-orphaned links survive (in practice, A-prefix reallocation on rerun makes most links orphaned, which is the existing reality across all A-family pipeline steps and not new to step 7).
- Early-return condition: if `audit_data.subject_conditions.is_empty()`, log "no conditions found, skipping threat generation" and return cleanly. Threats are downstream of conditions; if step 6 produced nothing, there is nothing to generate against.
- Per-function skip: within the function loop, skip any function whose subjects all have empty `subject_conditions` arrays — there's nothing for the LLM to invert.
- Render call: identical to step 6 — `context::render_batch_for_extraction(&[member], audit_data)`. The unified renderer now inlines conditions on each non-pure subject (step 6 phase 3 wired this), which is the load-bearing input for step 7.
- Extract call: `task::extract_threats_from_batch(&rendered.json, &rendered.label, audit_data.security_notes.as_deref())`.
- Storage block: for each `ParsedConditionThreats`, allocate one A-topic per `ParsedThreat` via `ids::allocate_adversarial_property_id()`. For each, build the `ThreatTopic` metadata (severity = `None`, created_at = `None`, controlled_by/falsifies_condition/evidence_topics from the parsed entry) and insert into `topic_metadata`. **Do not** insert anything into `audit_data.threats` — the field is gone after Phase 1.
- **`no_threat_rationale` posts as an agent comment on the condition topic.** When a `ParsedConditionThreats` has `Some(rationale)`, call the collaborator's comment-creation API to post a comment on `falsifies_condition` with the rationale text, authored by the agent identity. This uses the existing comment surface (the same one auditors use for approvals/disagreements), so the rationale renders naturally in the condition's discussion thread, persists across pipeline reruns (comments are not cleared by step 7's clear-on-rerun retain), and lets the auditor reply in-thread to contest the rationale. The comment body should include a structural prefix (e.g. `[step-7 / no-threat]`) so the UI can distinguish pipeline-emitted rationale comments from human discussion if filtering is added later. The agent identity comes from the same author-resolution path step 5/6 use for their pipeline-authored topics.

After the storage block, call `domain::rebuild_feature_context(audit_data)` once.

Update `run_full_pipeline`:

```rust
tracing::info!("[1/7] Semantic Linking");
build_semantic_links(state, audit_id).await?;

tracing::info!("[2/7] Requirement Extraction");
build_requirements(state, audit_id).await?;

tracing::info!("[3/7] Behavior Extraction");
build_behaviors(state, audit_id).await?;

tracing::info!("[4/7] Feature Synthesis");
synthesize_features(state, audit_id).await?;

tracing::info!("[5/7] Functional Purpose & Placement Generation");
build_functional_properties(state, audit_id).await?;

tracing::info!("[6/7] Condition Generation");
build_conditions(state, audit_id).await?;

tracing::info!("[7/7] Threat Generation");
build_threats(state, audit_id).await?;
```

Update the docstring at the top of `run_full_pipeline` to reflect seven steps and what step 7 does.

### How to verify Phase 5

- `cargo build --workspace` clean, no new warnings.
- `cargo test --workspace` all pass.
- Coverage for this phase comes from unit tests in earlier phases:
  - Phase 3: renderer tests cover the input shape (conditions inline) and the inline-threats hook.
  - Phase 4: parser tests cover the response shape and validation.
  - Phase 3: `rebuild_feature_context` tests cover the storage side.
- Smoke test: run the analyzer on the seed audit fixture, confirm `audit.json` contains `ThreatTopic` entries with at least one threat per non-trivially-safe condition. Sample ten descriptions: each should name a concrete scenario without naming a party (no "an attacker," "a miner," etc.), have a single `controlled_by` actor that matches the scenario, and have evidence_topics that are all inside the subject's containing function. Conditions with `no_threat_rationale` entries should have a `[step-7 / no-threat]` agent comment on the condition's discussion thread. Document the smoke-test command in the PR description (same convention as step 5/6 — orchestration is not unit-tested).
- Rerun-clear test: run the pipeline twice in succession against the same fixture and assert that the second run does not produce duplicated `InvariantTopic` entries (i.e. the proactive clear ran). Easy to construct because step 7 is the only writer of either topic family at this point in the pipeline.

### Pivotal decision

Threats are allocated one A-topic per scenario, not per condition. A condition with three threats consumes three A-IDs. Each threat is independently addressable, independently approvable, and independently linked to invariants in step 8. The per-condition entry is a grouping construct only; it does not itself receive a topic ID.

## Phase 6 — README and SPEC sync

### Goal

The root `README.md` still describes conditions as the retired Q/A model and threats as "derived from condition evaluations" with the matching framing. Step 6 phase 6 rewrote `SPEC.md` but explicitly skipped `README.md`. Step 7 is the natural moment to bring `README.md` to current and to add the threat-actor and threat-condition-link framing to both docs.

### Files to change

**`crates/o11a-core/README.md`** (the root README — `crates/o11a-backend/README.md` is the same file at a different path; touch whichever is canonical and confirm there is only one)

- Lines 121, 174, 234–239, 308, 416–450 (and the Hierarchy diagram around L228–248) carry the Q/A model. Rewrite per the same edits already done in `SPEC.md` phase 6 of step 6: conditions are positive assertions; threats are adversarial inversions; the chain is purpose → conditions → threats → invariants.
- Add a short paragraph on threat actors and on the falsifies-condition link. The new framing of threats as 1:N targeting conditions is load-bearing for the audit reading the doc.

**`crates/o11a-core/SPEC.md`**

`SPEC.md` is mostly current after step 6 phase 6. Touchups:

- The "Managing Threats and Invariants" section (around L445) describes threats as "the adversarial inversion of a specific condition" but does not yet describe the structured `ThreatActor` field or the in-function evidence scope. Add one paragraph each.
- The hierarchy diagram (around L162) shows `Threat: ... ├── Falsifies condition: ...`. Add a sibling `├── Controlled by: <actor>` line under each example threat for consistency with the new field.

### How to verify Phase 6

- No code changes; verification is read-through.
- `grep -n "standardized questions\|condition evaluations" crates/o11a-core/README.md` returns zero matches.
- `grep -n "controlled_by\|ThreatActor" crates/o11a-core/SPEC.md` returns matches in the threats section.
- A re-read of the Hierarchy block in both README and SPEC reads coherently end-to-end (purpose → conditions → threats → invariants), with the example threats showing the new fields.

### Pivotal decision

Doc cleanup ships in the same commit as the code change. Splitting it into a follow-up commit leaves the README contradicting the code for whatever interval that takes; given that step 6 phase 6 already did this work for SPEC.md, the precedent is to keep doc and code in sync within one commit.

## Cost notes

For an audit with ~60 in-scope functions (the same fixture step 5 and 6 cost-modeled against):

- **Step 7 call count**: ~60, one per function with conditions. Same as step 5/6 after their per-function refactors.
- **Per-call input**: same as step 6 plus the inline `conditions` array on each non-pure subject. Roughly 15–25% larger than step 6's per-call input by token count (conditions render as `{topic, description, kind, evidence_topics}` per assertion, typically 1–8 per subject).
- **Per-call output**: 0–3 threats per condition × 1–8 conditions per subject × the function's non-pure subjects (typically 2–6). Each threat is one prose sentence, an actor token, and 1–4 topic IDs. Roughly 1.5–3× the per-call output of step 6.
- **Total per-audit token cost**: in the same range as step 6, modestly larger because of the input growth and the larger output. No expectation of a step change.

The `security_notes` prompt segment is added once per call (~hundreds of tokens for typical audit). Negligible compared to the rendered batch.

## Out of scope

These are tracked decisions; do not build them in this work:

- **On-the-fly threat generation** when an auditor adds or corrects a condition post-pipeline. The generator is structured so a single-condition (or single-subject) caller can reuse it; the call site is not built.
- **Adversarial critique pass on threats.** Specified but not implemented; same deferral as step 5/6 critique passes.
- **Multi-actor threats.** Single `controlled_by` per threat; multi-actor coordination scenarios go in the description prose. Add `Vec<ThreatActor>` later if real output shows the description is losing signal.
- **`ThreatKind` enum.** Inherited grouping from `falsifies_condition.kind` is the v1 mechanism. Add an explicit kind later if UI grouping needs a threat-native taxonomy.
- **Retiring the `Invariant` struct and `audit_data.invariants`.** Phase 1 retires `Threat` only. The parallel cleanup for `Invariant` lands in step 8's own work, where it will introduce a `threat_invariants` reverse index built from `InvariantTopic.threat_topic` and drop the denormalization layer.
- **Cross-pipeline rendered-context validation of `evidence_topics`.** Same defer as step 6 — a future refinement pass will tighten validation across all steps that accept topic-typed LLM output.
- **Step 8 (invariants).** This work makes step 8 unblockable, not implemented. The renderer's inline-threats hook (Phase 3) is the architectural prep step 8 will consume.

## Final verification

After all phases land:

- `cargo build --workspace` clean, no new warnings.
- `cargo test --workspace` all green.
- `ARTIFACT_SCHEMA_VERSION` in `analysis_artifact.rs` is `3`.
- `grep -rn "pub struct Threat\b" crates/` returns zero matches (legacy struct retired).
- `grep -rn "BTreeMap<.*, Threat>\|audit_data\.threats" crates/` returns zero matches.
- `grep -rn "FROM threats\|FROM invariants\|FROM invariant_source_topics\|FROM threat_feature_links" crates/` returns zero matches (legacy DB tables retired).
- `grep -rn --include='*.rs' "ThreatActor::" crates/` returns matches in the domain layer, the task layer, the pipeline step, and the renderer hook — but not in any HTTP handler (API layer is out of scope).
- `grep -rn --include='*.rs' "falsifies_condition" crates/` returns matches in the domain layer, parser, post-processor, and renderer.
- `grep -rn "subject_threats\|condition_threats" crates/` returns matches in the domain layer, `rebuild_feature_context`, the pipeline step, and (for the renderer) the inline-injection block.
- A trial run of the full pipeline on a known audit produces `ThreatTopic` entries on a meaningful fraction of conditions (some conditions will legitimately have empty threats with `[step-7 / no-threat]` agent comments on their discussion threads). Sampled threats: each names a concrete scenario without naming a party in the prose, has exactly one `controlled_by` from the eight-variant enum, and has all evidence_topics inside the subject's containing function. Step 5/6 outputs are equivalent or improved against the same fixture — not regressed.
- Rerunning the pipeline twice produces no duplicated `InvariantTopic` entries (proactive clear ran).
- Root `README.md` and `SPEC.md` describe threats and conditions consistently with the post-step-7 model.
