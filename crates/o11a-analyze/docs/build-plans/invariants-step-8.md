# Step 8 — Invariants on Threats

This is the implementation plan for pipeline step 8: generating **invariants** — codebase-level defensive properties stated against the threats produced in step 7 — for every threat with at least one defendable property. Invariants anchor 1:N to the threat they defend; one threat can carry many invariants. Step 9 (the per-function entry-boundary absence check, planned next) will consume these invariants; this step makes step 9 unblockable but does not implement it.

Read `threats-step-7.md` first; this doc assumes its phases landed and reuses its structural patterns wholesale (per-function generation, A-prefixed topic family, unified renderer, post-processor validation shape, agent-comment rationale for empties).

## Summary of decisions

These were settled during design and should not be re-litigated during implementation:

- **Invariants are defensive properties stated in "X must Y" / "every Z does W" framing.** Each invariant states what the codebase must enforce to prevent a specific threat scenario from occurring. The reasoning chain (per SPEC's "Conditions vs. Invariants"): purpose → conditions → threats → invariants. Step 8 produces invariants from the threats step 7 generated; verification of whether each invariant actually holds in the code is a deferred later pipeline step (re-check propagation), not in scope here.
- **One threat can carry many invariants; each invariant names exactly one parent threat.** `threat_topic: topic::Topic` stays singular on `InvariantTopic`. If a single defensive property defends multiple threats, emit duplicate-description invariants with different `threat_topic` links — text is cheap, attribution is expensive (same logic steps 6/7 used for cross-cutting duplication).
- **Invariants attach to a single subject.** `subject_topic: topic::Topic` lives on each invariant, inherited at write time from the parent threat's `subject_topic`. Singular; cross-site application is handled by duplicate-description invariants on each affected subject. Scope-organized re-check propagation is a deferred later step.
- **`InvariantKind` is a closed enum.** Defensive patterns have a stronger native taxonomy than threats do, so unlike threats (which inherit grouping from `falsifies_condition.kind`) invariants get their own kind enum. Seven variants + `Other`, matching the shape of `ConditionKind` and `ThreatActor`.
- **Invariants carry no `evidence_topics`.** Unlike conditions and threats, invariants do not anchor to in-codebase topic IDs at generation time. The parent threat is the evidence for the invariant's existence; whether the invariant actually holds in the code is the subject of a later verification step (re-check). This keeps step 8's prompt tight on "what defense is needed" without mixing in "where is the defense."
- **Severity is denormalized from the parent threat.** `severity: Option<ThreatSeverity>` stays on `InvariantTopic`, populated at write time from the threat's severity (`None` while threat severity is pending). Same denormalization rationale as the existing field — query efficiency without forcing every invariant consumer to traverse `threat_topic`. **The field is a write-time snapshot, not a live mirror**: if impact analysis later updates the parent threat's severity, the invariant's copy goes stale. Acceptable for v1; a future pass can either propagate on threat-severity edits or drop the field and look up via `threat_topic` if staleness becomes a real problem.
- **`created_at: Option<String>`.** Flipped from the current non-Option shape to match the FunctionalPurposeTopic / ConditionTopic / ThreatTopic convention for pipeline-produced topics.
- **Empty-invariants per threat is allowed, with explicit rationale.** A threat for which the LLM considered the codebase and found no defendable property emits a `no_invariant_rationale: String` rather than a silent skip. Same posting strategy as step 7's `no_threat_rationale`: an agent-authored comment on the threat topic carrying the rationale, with a `[step-8 / no-invariant]` prefix.
- **Per-function generation, per-threat output, nested under subject.** One LLM call per in-scope function with threats. The unified renderer (post-step-7) already inlines threats on each non-pure-subject node, so the input shape carries the function's threats grouped by their subject. Output mirrors that nesting: `{subjects: [{subject_topic, threats: [{threat_topic, invariants: [...], no_invariant_rationale: ...}]}]}`.
- **A-prefixed topic family.** `InvariantTopic` already shares the `A` prefix and `NEXT_ADVERSARIAL_PROPERTY_ID` counter with `ConditionTopic` and `ThreatTopic`. No counter changes.
- **No adversarial critique pass.** Same out-of-scope status as steps 5/6/7. Defer.
- **No on-the-fly path.** Pipeline only. Generator is structured so a single-threat caller can reuse it later, but that path is not built or tested here.
- **Retire the `Invariant` struct and `audit_data.invariants`.** `domain/mod.rs:514` defines `pub struct Invariant { source_topics: Vec<Topic> }` — the pre-A-prefix denormalization that the threats step explicitly left for step 8. Phase 1 retires it in the same shape phase 1 of step 7 retired `Threat`: delete the struct, drop the snapshot field, rely on a `threat_invariants` reverse index built from `InvariantTopic.threat_topic`. Bumps `ARTIFACT_SCHEMA_VERSION` to 4.
- **Step 8 reruns clear only `InvariantTopic` entries.** Step 8 is the last step in the pipeline at this writing; there is no downstream artifact to also clear. Step 9 will own its own clear when it lands.
- **Renderer hook for downstream consumers.** Inline `invariants` array on non-pure-subject nodes, parallel to the existing `conditions` and `threats` hooks. Empty in step 8's own consumption (step 8 doesn't read its own hook), wired now so step 9 inherits it for free — the same architectural prep step 7 made for step 8.

## What you will build

The work splits into one precursor commit (legacy retirement) and one additive commit (the new step). Phases:

**Precursor — Retire the `Invariant` struct and `audit_data.invariants` (Phase 1).** Parallel to step 7 phase 1. Delete `pub struct Invariant`, the `pub invariants: BTreeMap<Topic, Invariant>` field on `AuditData`, the matching snapshot field. (The DB tables `invariants` and `invariant_source_topics` were already retired in step 7 phase 1.) Pure deletion — nothing introduced.

**Step 8 commit — Invariants (Phases 2–6).** Purely additive on top of the precursor:

2. **Domain additions.** Widen `InvariantTopic` with `subject_topic`, `kind`; flip `created_at` to `Option<String>`. Add `InvariantKind` enum.
3. **Reverse indexes and renderer hook.** Add `threat_invariants` and `subject_invariants` to `AuditData`, populated by `rebuild_feature_context`. Extend the renderer's existing per-subject inline injection to stamp `invariants` on non-pure-subject nodes (so step 9 inherits it for free).
4. **Task layer.** Add `extract_invariants_from_batch` with prompt, JSON schema, and post-processor.
5. **Pipeline step.** Wire `build_invariants` into `run_full_pipeline` as step 8 of 8. Storage block posts agent comments on threat topics for `no_invariant_rationale` entries. Renumber existing log lines.
6. **README and SPEC sync.** Update `README.md` and `SPEC.md` to describe invariants as defensive properties with the kind enum, "X must Y" framing, and the deferred re-check verification step.

Each phase is independently verifiable. Do not move on until the previous one compiles, its tests pass, and `cargo test --workspace` is green.

## Phase 1 — Retire the `Invariant` struct and `audit_data.invariants`

### Goal

Remove the pre-A-prefix `Invariant` denormalization layer that step 7 phase 1 left in place, so the new step lands cleanly without naming collisions or two-source-of-truth ambiguity. The DB tables that backed this struct (`invariants`, `invariant_source_topics`) were retired in step 7 phase 1; only the in-memory and snapshot fields remain. Phase 1 must end with `cargo build --workspace` and `cargo test --workspace` green.

### Files to change

**`crates/o11a-core/src/domain/mod.rs`**

Delete:
- `pub struct Invariant` (around line 514).
- `pub invariants: BTreeMap<topic::Topic, Invariant>` field on `AuditData` (around line 605).

Keep:
- `TopicMetadata::InvariantTopic` — this is the new shape Phase 2 widens.

Every `AuditData` constructor and `Default`-shaped initialization site that mentioned `invariants: BTreeMap::new()` needs that line removed. The compiler will flag every miss.

**`crates/o11a-core/src/analysis_artifact.rs` (CRITICAL — same shape as step 7 phase 1)**

- Remove `Invariant` from the `use crate::domain::{...}` import.
- Remove `pub invariants: BTreeMap<topic::Topic, Invariant>` from `AuditDataSnapshot`.
- Remove `invariants: audit_data.invariants.clone(),` from `snapshot_from_audit_data`.
- Remove `audit_data.invariants = snap.invariants;` from `apply_snapshot`.
- **Bump `ARTIFACT_SCHEMA_VERSION` from `3` to `4`.** Removing a field is breaking.
- Update the doc comment listing snapshot bullets — drop the remaining `invariants` mention.

**`crates/o11a-core/src/collaborator/agent/pipeline.rs`**

`build_threats` currently includes `audit_data.invariants.clear();` as part of its proactive clear-on-rerun block (added by step 7 phase 5 against the now-retired denormalization). Remove that line; the surviving clear of `InvariantTopic` entries from `topic_metadata` is sufficient. The `topic_metadata` InvariantTopic clear in step 7 is now defensively redundant with step 8's own clear but harmless — leave it in step 7.

**Anywhere else**

Run these greps and touch every match that refers to the old type:
- `grep -rn "domain::Invariant\b" crates/`
- `grep -rn "Vec<Invariant>\|BTreeMap<.*, Invariant>" crates/`
- `grep -rn "audit_data\.invariants" crates/`

The trailing `\b` is deliberate — a naive `Invariant` match catches `InvariantTopic`, which is kept.

If a test, fixture, or doc string references the old type, remove or rewrite. Do not leave a `// TODO: re-add` comment.

### How to verify Phase 1

- `cargo build --workspace` clean, no new warnings.
- `cargo test --workspace` all pass. (Tests that exercised the old type should be deleted, not skipped.)
- `grep -rn "pub struct Invariant\b" crates/` returns zero matches.
- `grep -rn "BTreeMap<.*, Invariant>\|audit_data\.invariants" crates/` returns zero matches.
- `ARTIFACT_SCHEMA_VERSION` is `4`.

### Pivotal decision

The `Invariant` struct survives in no form. Audit data is regenerated by the pipeline; nothing to migrate. The `threat_invariants` reverse index added in Phase 3 is the canonical replacement.

## Phase 2 — Domain additions

### Goal

Widen the persistent shape so step 8 has somewhere to write its outputs and step 9 has somewhere to read from.

### Files to change

**`crates/o11a-core/src/domain/mod.rs`**

Add `InvariantKind` near `ThreatActor` / `ConditionKind`. Closed enum, flat:

```rust
/// The defensive pattern this invariant expresses — the category of
/// codebase-level property the parent threat scenario violates. Loose
/// taxonomy; the LLM picks; the auditor groups by category in the
/// review UI. `Other` is the escape hatch for novel defenses that
/// don't fit a named variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvariantKind {
  /// A privilege check — modifier-based role gating, owner check, or
  /// other authorization mechanism — gates the operation.
  AccessGate,
  /// A lock, reentrancy guard, or CEI ordering pattern prevents reentry
  /// from observing partial state.
  ReentrancyGuard,
  /// A paused-state check halts the operation under emergency-stop
  /// conditions.
  PauseGate,
  /// A bound on slippage, deadline, or numeric range constrains the
  /// caller's tolerated outcome.
  BoundedTolerance,
  /// A staleness check ensures the value read is current relative to
  /// the operation's needs.
  FreshnessCheck,
  /// A conservation invariant — sum, total, or balance equality — holds
  /// across the operation.
  ConservationCheck,
  /// Argument well-formedness — zero address, range bounds, sentinel
  /// checks — rejects malformed input before it propagates.
  InputValidation,
  /// Genuinely novel defense; description carries the structure.
  Other,
}
```

Update `InvariantTopic` (around current line 2213). Diff:

- Add `subject_topic: topic::Topic`. Required, single. The non-pure subject this invariant protects, inherited at write time from the parent threat.
- Add `kind: InvariantKind`. Required, single.
- Flip `created_at: String` → `created_at: Option<String>`. Match the FunctionalPurposeTopic / ConditionTopic / ThreatTopic convention.

Resulting shape:

```rust
InvariantTopic {
  topic: topic::Topic,
  /// The defensive property, in prose, phrased as "X must Y" or
  /// "every Z does W" — what the codebase must enforce, not how to
  /// enforce it.
  description: String,
  /// The threat this invariant defends against. One invariant defends
  /// exactly one threat; one threat can be defended by many invariants.
  threat_topic: topic::Topic,
  /// The non-pure subject this invariant protects. Inherited at write
  /// time from the parent threat's `subject_topic`. Singular; cross-site
  /// application is handled by duplicate-description invariants on each
  /// affected subject. Scope-organized re-check propagation is a
  /// deferred later step.
  subject_topic: topic::Topic,
  /// Category of defensive pattern this invariant expresses.
  kind: InvariantKind,
  author: crate::collaborator::models::Author,
  /// `None` for pipeline-produced entities — see FeatureTopic for
  /// rationale.
  created_at: Option<String>,
  /// Severity inherited from the parent threat at write time; `None`
  /// while threat severity is pending impact analysis.
  severity: Option<ThreatSeverity>,
}
```

Note: invariants carry no `evidence_topics`. The parent threat is the evidence; cross-codebase verification (re-check) is a later pipeline step.

Update every match against `TopicMetadata`. The new fields don't change the variant signature in ways that affect arms other than `subject()` and `created_at()`. Specifically:

- `subject()` — **semantic change, not just a refactor.** Today the InvariantTopic arm at `domain/mod.rs:2358` returns `Some(threat_topic)` as a stand-in (invariants previously had no real subject; the parent threat was the closest proxy). After Phase 2, return `Some(*subject_topic)` so InvariantTopic folds into the same arm as `ThreatTopic` / `ConditionTopic` / `FunctionalPurposeTopic` / `PlacementRationaleTopic`. **Any caller that walked `subject()` on an InvariantTopic expecting a threat topic now gets a subject topic instead** — audit those call sites. The likely callers are the comment/mention surfaces and the auditor-UI subject-resolver; the change is correct (a subject is what callers actually want), but it is a behavior change, not a cosmetic refactor.
- `created_at()` — `InvariantTopic` moves from the non-Option arm to the Option arm (folds into the FunctionalPurposeTopic / ConditionTopic / ThreatTopic arm).

Match exhaustiveness errors after `cargo build` will tell you exactly which arms you missed. The `subject()` change is silent — `cargo build` will not flag it because the type is unchanged; rely on a grep for callers of `subject()` and review what they do with the result.

### How to verify Phase 2

- `cargo build --workspace` compiles cleanly.
- Existing tests still pass.
- New unit test in the `domain` test module: construct an `InvariantTopic` with all new fields, insert into a `TopicMetadata` map, round-trip through the existing serialize/deserialize.
- A test confirming every `InvariantKind` variant parses correctly under its JSON name.

### Pivotal decision

`InvariantKind` is a closed enum, not a string. Same rationale as `ConditionKind` and `ThreatActor`. Strings drift, type-checking can't help, and the LLM-driven generator path will deserialize the string anyway. If the LLM picks an off-list defense, deserialization fails and the post-processor logs a warning — exactly the failure surface we want.

## Phase 3 — Reverse indexes and renderer hook

### Goal

Make invariants queryable per-threat and per-subject, and make them visible to downstream consumers (step 9) through the unified renderer's existing per-subject inline-injection mechanism.

### Files to change

**`crates/o11a-core/src/domain/mod.rs`**

Add two fields to `AuditData`, immediately after `condition_threats` (find by name):

```rust
/// Reverse index: A-prefixed threat topic → A-prefixed invariant topics
/// that defend it. Each threat has zero or more invariants. Canonical
/// replacement for the retired `audit_data.invariants` denormalization.
/// Used for the threat-detail UI ("show all defenses for this scenario")
/// and for re-derivation triggers (auditor edits threat X → re-run
/// invariants anchored to X). Derived from `InvariantTopic.threat_topic`,
/// rebuilt with `rebuild_feature_context`.
pub threat_invariants: BTreeMap<topic::Topic, Vec<topic::Topic>>,
/// Reverse index: non-pure subject topic → A-prefixed invariant topics
/// protecting it. Each subject has zero or more invariants. Drives the
/// per-subject inline-invariants renderer hook and step 9's per-function
/// entry-boundary check input. Derived from `InvariantTopic.subject_topic`,
/// rebuilt with `rebuild_feature_context`.
pub subject_invariants: BTreeMap<topic::Topic, Vec<topic::Topic>>,
```

Populate inside `rebuild_feature_context`, in the same block that handles `subject_threats` / `condition_threats`:

```rust
audit_data.threat_invariants.clear();
audit_data.subject_invariants.clear();
for (inv_topic, metadata) in &audit_data.topic_metadata {
  if let TopicMetadata::InvariantTopic { threat_topic, subject_topic, .. } = metadata {
    audit_data
      .threat_invariants
      .entry(*threat_topic)
      .or_default()
      .push(*inv_topic);
    audit_data
      .subject_invariants
      .entry(*subject_topic)
      .or_default()
      .push(*inv_topic);
  }
}
```

Initialize both fields everywhere `AuditData` is constructed. Find by grepping `subject_threats: BTreeMap` — every site that initializes that field needs both new lines added. Compiler will flag misses.

**Update the existing InvariantTopic context-builder in the same file.** `rebuild_feature_context` already has an InvariantTopic-specific block around `domain/mod.rs:3228` that builds `topic_context` using only `threat_topic` (the parent threat is added as a SourceContext entry). With `subject_topic` now on the variant, extend that block to **also** add a SourceContext entry for `subject_topic` — without this, the auditor UI's expanded view for an invariant shows only the parent threat and misses the subject the invariant protects. Mirror the shape of the existing entry (same `reference_topic`/`scope` pattern). The threats step's per-subject context entry (`domain/mod.rs:~3213`) is the closest template — invariants want both: the threat as the upstream link, the subject as the protected anchor.

**`crates/o11a-core/src/collaborator/agent/context.rs`**

Extend the existing per-subject inline injection on non-pure-subject nodes (same hook step 6 wired for `conditions` and step 7 wired for `threats`). Find the existing injection sites with `grep -n '"conditions"\|"threats"' crates/o11a-core/src/collaborator/agent/context.rs` — both hooks live in the same per-node emission block, gated on `subject_conditions` / `subject_threats` presence. Add a third gated stamp:

```rust
// stamp `invariants` on the subject node when subject_invariants has entries
"invariants" -> array of {
  topic, description, kind, threat_topic, severity
}
```

Same pattern as the existing hooks: gated on presence (omit field if `audit_data.subject_invariants` has no entry for that topic), no placeholder values, descriptions resolved through `topic_metadata` lookup.

This hook is empty in step 8 itself (step 8's own LLM call does not consume its own hook). It is added now so step 9 inherits inline invariants for free, the same way step 7 prepared the threats hook in phase 3 before step 8 had data to write.

### How to verify Phase 3

- `cargo build --workspace` clean.
- Unit test next to the existing `rebuild_feature_context` tests for threats: insert two `InvariantTopic` entries with the same `threat_topic` and different `subject_topic`s, plus one with a different threat, call `rebuild_feature_context`, assert both reverse indexes have the expected shape.
- Context-builder test: after `rebuild_feature_context`, an InvariantTopic's entry in `audit_data.topic_context` carries SourceContext entries for **both** its `threat_topic` and its `subject_topic`. Before Phase 3, only the threat entry was present; the test pins the new shape.
- Renderer test: with non-empty `subject_invariants`, the inline hook on a non-pure-subject node emits `invariants: [{topic, description, kind, threat_topic, severity}, ...]`. With empty `subject_invariants`, the field is omitted.

### Pivotal decision

Both indexes carry `Vec<Topic>`, not richer structures. Same shape rule as `subject_conditions`, `subject_threats`, `condition_threats`, `member_behaviors`. Consumers look up the metadata via `topic_metadata`.

## Phase 4 — Task layer

### Goal

Run the LLM call against the rendered batch JSON (which already has conditions and threats inlined per steps 6/7 phase 3s) and parse the response into well-typed `ParsedInvariant` entries.

### Files to change

**`crates/o11a-core/src/collaborator/agent/task.rs`**

Add a section header after the threats section:

```rust
// ============================================================================
// Invariants (Pipeline Step 8)
// ============================================================================
```

Add the prompt constant (`EXTRACT_INVARIANTS_PROMPT`). Use `EXTRACT_THREATS_PROMPT` as your structural model. The prompt should:

- Describe the input format (the unified renderer's `subject` envelope, including the inline `conditions` and `threats` arrays on each non-pure subject — the threats array is the load-bearing input).
- Explain the task: for **each threat** on each non-pure subject, generate zero or more **invariants** — codebase-level defensive properties the threat scenario violates. Phrase each invariant as a property statement, not a code recommendation: "every privileged-state-modifying function checks ownership" — never "add `onlyOwner` to this setter." The invariant is what must hold; how the code enforces it is a separate concern.
- **Description framing is "X must Y" / "every Z does W."** Include the distinguishing test verbatim: "if your invariant reads as a fix recommendation ('add a guard,' 'use a check,' 'implement X'), restate as the property the fix would enforce ('the operation is guarded by a check on X'). If your invariant reads as a scenario ('the caller might bypass X'), you have miswritten a threat — invariants state what holds across the codebase, not what could fail."
- **No evidence_topics field.** Tell the LLM: "do not list topic IDs where the invariant is enforced; verification of where each invariant actually holds in the code is a later pipeline step. State the property; the codebase locations come later."
- Describe the output schema: per-subject entries containing per-threat entries, each with `threat_topic`, an array of `invariants`, and an optional `no_invariant_rationale` for empty arrays.
- Enumerate the eight `InvariantKind` values with one-line descriptions matching the enum doc-comments. Tell the LLM to pick the kind that names the defensive pattern; use `Other` for genuinely novel defenses rather than forcing a fit.
- **Empty-invariants handling.** Tell the LLM: "if a threat has no codebase-level defense you can identify — because the threat is mitigated by user discretion, by economic incentives, by an external trust assumption, or because the threat is genuinely unmitigable in the current design — emit an empty `invariants` array and a `no_invariant_rationale` string explaining why. Do not invent defenses to fill the slot; the rationale is the audit signal."
- Tell it explicitly: "one defense can defend multiple threats. If the same property defends three threats in this function, emit it three times with different `threat_topic` links. Text is cheap; attribution is expensive."
- Reference the audit-wide context: "the `security_notes` block above this prompt may name known defenses, role definitions, and security considerations specific to this audit. Use it to anchor your invariants in defenses the auditor has already documented."

Define the deserialization types:

```rust
#[derive(Deserialize)]
struct LLMInvariant {
  description: String,
  kind: InvariantKind,
}

#[derive(Deserialize)]
struct LLMThreatInvariants {
  threat_topic: String,
  invariants: Vec<LLMInvariant>,
  no_invariant_rationale: Option<String>,
}

#[derive(Deserialize)]
struct LLMSubjectInvariants {
  subject_topic: String,
  threats: Vec<LLMThreatInvariants>,
}

#[derive(Deserialize)]
struct LLMInvariantsResponse {
  subjects: Vec<LLMSubjectInvariants>,
}
```

Define the JSON schema (`INVARIANTS_SCHEMA`) mirroring `THREATS_SCHEMA`. The `kind` property constrains to the eight enum string forms via `"enum": ["AccessGate", "ReentrancyGuard", "PauseGate", "BoundedTolerance", "FreshnessCheck", "ConservationCheck", "InputValidation", "Other"]`. The `no_invariant_rationale` is an optional string.

Define the parsed output types:

```rust
pub struct ParsedInvariants {
  pub entries: Vec<ParsedSubjectInvariants>,
}

pub struct ParsedSubjectInvariants {
  pub subject_topic: topic::Topic,
  pub threats: Vec<ParsedThreatInvariants>,
}

pub struct ParsedThreatInvariants {
  pub threat_topic: topic::Topic,
  pub invariants: Vec<ParsedInvariant>,
  pub no_invariant_rationale: Option<String>,
}

pub struct ParsedInvariant {
  pub description: String,
  pub kind: InvariantKind,
}
```

Define `extract_invariants_from_batch(batch_json: &str, label: &str, security_notes: Option<&str>) -> Result<ParsedInvariants, TaskError>`. Mirror `extract_threats_from_batch`. The `security_notes` parameter is prepended to the LLM call as system-context if `Some`. Validation rules:

- Every subject in the batch's `non_pure_subjects` that has a non-empty `threats` array in its rendered JSON should appear in the response (warn on missing or extra; do not fail the batch).
- `subject_topic` must parse and must be a `Topic::Node(_)` variant (warn + skip otherwise).
- Subjects outside the batch are dropped (warn + skip).
- Duplicate subjects in the response are deduped — first occurrence wins.
- For each `LLMThreatInvariants`:
  - `threat_topic` must parse to an `A`-prefixed `Topic` (warn + skip otherwise).
  - The threat topic must appear in the subject's inline `threats` array in the rendered batch JSON (cross-reference; warn + drop the entry if not). **Mirror the threats parser's cross-reference for `falsifies_condition` against inline conditions** — same shape, different field name. The threats step parses the rendered JSON into a `HashMap<subject_topic, HashSet<inline_anchor_topic>>` for the lookup; reuse that helper if it exists as a generic, otherwise add a sibling helper that walks `subjects[i].threats[j].topic` instead of `subjects[i].conditions[j].topic`.
  - If `invariants` is empty and `no_invariant_rationale` is `None`, warn and drop (the LLM left a slot blank without explanation — same shape as a zero-threat condition in step 7).
  - If `invariants` is non-empty and `no_invariant_rationale` is `Some`, warn and drop the rationale (kept the invariants, discarded the contradictory rationale).
- For each invariant:
  - `kind` parses as the enum (the schema enforces this; assert the error path on off-list values).
  - Empty `description` after `trim()` → warn + drop the invariant.
  - Description that reads as a code recommendation (lightweight regex match on `/^(add|use|implement|include|insert|introduce|wrap with|apply|enforce by) /i` or similar) → warn but keep. Tracked as a prompt-quality signal in the smoke run; if it spikes, tighten the prompt rather than escalating to drop.

### How to verify Phase 4

Add tests in the `task` test module mirroring the threats parser tests:

- Well-formed response: full round-trip into `ParsedSubjectInvariants`.
- Multiple invariants on the same threat: kept (1:N is the supported shape).
- Same invariant description on multiple threats: kept as separate entries.
- Subject missing from response: warning logged, no entry.
- Subject not in `non_pure_subjects`: rejected.
- Duplicate subject in response: deduped.
- Malformed topic ID in `subject_topic`: skipped.
- `threat_topic` not in the subject's inline threats array: dropped with warning.
- Empty `invariants` with no `no_invariant_rationale`: dropped with warning.
- Empty `invariants` with a `no_invariant_rationale`: kept, rationale preserved.
- Non-empty `invariants` with a `no_invariant_rationale`: rationale dropped, invariants kept.
- Each `InvariantKind` value parses correctly under its JSON name.
- An off-list `kind`: deserialization fails (schema enforcement; assert the error path).
- Recommendation-style description ("add a check on X"): warning logged, invariant kept.

### Pivotal decision

Validation drops bad entries but keeps the good. A batch with one malformed invariant still produces the rest. Same partial-output behavior as steps 5/6/7. The recommendation-style description check is the one warn-but-keep exception, because a recommendation-style description still carries useful audit content; the right intervention is to tighten the prompt if drift becomes systemic.

## Phase 5 — Pipeline step

### Goal

Wire `build_invariants` into `run_full_pipeline` as step 8 of 8.

### Files to change

**`crates/o11a-core/src/collaborator/agent/pipeline.rs`**

Add `build_invariants` immediately after `build_threats`. Model on `build_threats`. Differences:

- Clear-on-rerun retain: clear `InvariantTopic` entries from `topic_metadata`. No downstream cleanup — step 8 is the last step in the pipeline at this writing.
- Early-return condition: if `audit_data.subject_threats.is_empty()`, log "no threats found, skipping invariant generation" and return cleanly. Invariants are downstream of threats; if step 7 produced nothing, there is nothing to generate against.
- Per-function skip: within the function loop, skip any function whose subjects all have empty `subject_threats` arrays — there is nothing to defend.
- Render call: identical to step 7 — `context::render_batch_for_extraction(&[member], audit_data)`. The unified renderer now inlines both conditions and threats on each non-pure subject (step 7 phase 3 wired threats), which is the load-bearing input for step 8.
- Extract call: `task::extract_invariants_from_batch(&rendered.json, &rendered.label, audit_data.security_notes.as_deref())`.
- Storage block: for each `ParsedThreatInvariants`, look up the parent threat in `topic_metadata` to read its `subject_topic` and `severity`. The threat exists by construction — it's the input — but the lookup is fallible (the topic may have been cleared concurrently or the rendered batch may have referenced a stale topic). If the lookup fails or the metadata is not a `ThreatTopic` variant, warn and skip the entire `ParsedThreatInvariants` entry; do not fall back to defaults. Allocate one A-topic per `ParsedInvariant` via `ids::allocate_adversarial_property_id()`. For each, build the `InvariantTopic` metadata (`subject_topic` inherited from the threat, `severity` denormalized from the threat at write time, `created_at = None`, `kind`/`description` from the parsed entry) and insert into `topic_metadata`.
- **`no_invariant_rationale` posts as an agent comment on the threat topic.** Mirror step 7's pattern: when a `ParsedThreatInvariants` has `Some(rationale)`, call the collaborator's comment-creation API to post a comment on `threat_topic` with the rationale text, authored by the agent identity. The comment body includes a structural prefix `[step-8 / no-invariant]` so the UI can distinguish pipeline-emitted rationale comments from human discussion. Agent identity comes from the same author-resolution path step 5/6/7 use.

After the storage block, call `domain::rebuild_feature_context(audit_data)` once.

Update `run_full_pipeline`:

```rust
tracing::info!("[1/8] Semantic Linking");
build_semantic_links(state, audit_id).await?;

tracing::info!("[2/8] Requirement Extraction");
build_requirements(state, audit_id).await?;

tracing::info!("[3/8] Behavior Extraction");
build_behaviors(state, audit_id).await?;

tracing::info!("[4/8] Feature Synthesis");
synthesize_features(state, audit_id).await?;

tracing::info!("[5/8] Functional Purpose & Placement Generation");
build_functional_properties(state, audit_id).await?;

tracing::info!("[6/8] Condition Generation");
build_conditions(state, audit_id).await?;

tracing::info!("[7/8] Threat Generation");
build_threats(state, audit_id).await?;

tracing::info!("[8/8] Invariant Generation");
build_invariants(state, audit_id).await?;
```

Update the docstring at the top of `run_full_pipeline` to reflect eight steps and what step 8 does.

### How to verify Phase 5

- `cargo build --workspace` clean, no new warnings.
- `cargo test --workspace` all pass.
- Coverage for this phase comes from unit tests in earlier phases:
  - Phase 3: renderer tests cover the input shape (conditions + threats inline) and the inline-invariants hook.
  - Phase 4: parser tests cover the response shape and validation.
  - Phase 3: `rebuild_feature_context` tests cover the storage side.
- Smoke test: run the analyzer on the seed audit fixture, confirm `audit.json` contains `InvariantTopic` entries with at least one invariant per non-trivially-mitigable threat. Sample ten descriptions: each reads as an "X must Y" / "every Z does W" property statement (not a code recommendation, not a scenario), has a single `kind` from the eight-variant enum that matches the description, has `subject_topic` matching the parent threat's `subject_topic`, and has `severity` matching the parent threat's severity (or `None` if the threat's is pending). Threats with `no_invariant_rationale` entries should have a `[step-8 / no-invariant]` agent comment on the threat's discussion thread. Document the smoke-test command in the PR description (same convention as steps 5/6/7).
- Rerun-clear test: run the pipeline twice in succession against the same fixture and assert that the second run does not produce duplicated `InvariantTopic` entries (i.e. the clear-on-rerun retain ran).

### Pivotal decision

Invariants are allocated one A-topic per defense, not per threat. A threat with three invariants consumes three A-IDs. Each invariant is independently addressable, independently approvable, independently re-verifiable when the deferred re-check step lands. The per-threat entry is a grouping construct only; it does not itself receive a topic ID.

## Phase 6 — README and SPEC sync

### Goal

Update `README.md` and `SPEC.md` to describe the eight-step pipeline, the invariant kind enum, the "X must Y" framing, and the deferred re-check verification step. Step 7 phase 6 brought both docs to the post-step-7 model; step 8's touchups are mostly to the invariants subsection.

### Files to change

**`crates/o11a-core/README.md`** (touch whichever path is canonical; confirm there is only one)

- The pipeline overview (currently lists seven steps) — extend to eight, with step 8 framed as "invariants are defensive properties stated against each threat; verification of where they hold in the code is a deferred later pipeline step."
- The Hierarchy block — add a `Kind: <invariant kind>` line under each example invariant. Remove any `Source topics: …` line (the retired denormalization).
- The `Threat → Invariant` arrows in any diagrams — confirm the 1:N relationship reads correctly.

**`crates/o11a-core/SPEC.md`**

- "Managing Threats and Invariants" section (around L537) — expand the invariants subsection: invariants are pipeline-generated defensive properties per threat, with a closed `InvariantKind` taxonomy, "X must Y" / "every Z does W" framing, `subject_topic` inherited from the parent threat, and `severity` denormalized from the parent threat. Cross-codebase verification of whether each invariant actually holds is a deferred later pipeline step (re-check propagation). Any prose that implies invariants carry per-codebase-location anchors at generation time needs to be removed.
- "Conditions vs. Invariants" subsection — re-read end-to-end after the rewrites above. The role-distinction prose remains correct; only any per-location anchoring claims about invariants need adjusting.
- Hierarchy diagram (the block starting around L162; the example invariant lines are deeper, around L214–219) — add the `Kind` line under each example invariant; remove any source-topics line.
- "On-the-Fly Generation" entry for "New invariant" (currently L147) — rephrase: "Recorded as a defensive property against a parent threat, attached to the threat's subject. Adding or correcting an invariant flags re-verification of where the property holds in the code (deferred to a later pipeline step). The system does not yet trigger re-checks against related subjects within scope — that propagation is part of the deferred step."
- "Threat Traceability" section (around L553) — update any reference to invariants having explicit source links (was previously implied by the now-retired `Invariant.source_topics`). Re-check is the surface that will surface the source-of-enforcement linkage when it lands.

### How to verify Phase 6

- No code changes; verification is read-through.
- `grep -n "source_topics\|source topics" crates/o11a-core/SPEC.md` returns zero matches in invariant-related context.
- `grep -n "InvariantKind\|invariant kind" crates/o11a-core/SPEC.md` returns matches in the invariants section.
- A re-read of the Hierarchy block reads coherently end-to-end (purpose → conditions → threats → invariants) with each example invariant showing the new fields.

### Pivotal decision

Doc cleanup ships in the same commit as the code change. Same precedent as steps 6 and 7 phase 6.

## Cost notes

For an audit with ~60 in-scope functions (the same fixture steps 5/6/7 cost-modeled against):

- **Step 8 call count**: ~60, one per function with threats. Same as steps 5/6/7.
- **Per-call input**: same as step 7 plus the inline `threats` array on each non-pure subject (already wired by step 7 phase 3). The threats array is the load-bearing input. Roughly 15–25% larger than step 7's per-call input by token count (each threat renders as `{topic, description, falsifies_condition, controlled_by, evidence_topics}`, typically 1–3 per condition × 1–8 conditions per subject × 2–6 subjects per function).
- **Per-call output**: 0–3 invariants per threat × 0–~10 threats per function. Each invariant is one property statement and a kind token (no evidence_topics array). Roughly 1.5–2× per-call output of step 7 by sentence count, but each invariant entry is smaller than each threat entry. Net output token count is comparable.
- **Total per-audit token cost**: in the same range as step 7, modestly larger on input, comparable on output. No expectation of a step change.

The `security_notes` prompt segment is added once per call (~hundreds of tokens for typical audit). Negligible compared to the rendered batch.

## Out of scope

These are tracked decisions; do not build them in this work:

- **Re-check / verification propagation.** Whether each generated invariant actually holds in the code, and where else in scope the same property is needed, is the subject of a deferred later pipeline step. Step 8 emits the property; the verification step will check enforcement at all subjects in scope. The renderer's inline-invariants hook (Phase 3) is the architectural prep that verification step will read from.
- **On-the-fly invariant generation** when an auditor adds or corrects a threat post-pipeline. The generator is structured so a single-threat caller can reuse it; the call site is not built or tested.
- **Adversarial critique pass on invariants.** Specified but not implemented; same deferral as steps 5/6/7 critique passes.
- **Multi-threat invariants as a single topic.** Each invariant names exactly one parent threat. If a defense covers many threats, the LLM emits duplicate descriptions with different `threat_topic` links — same pattern conditions/threats use.
- **Scope-organized invariant rendering.** Invariants attach to subjects, not abstract scopes. Scope-level rendering ("show every function that should carry this invariant") is the deferred re-check step's surface, not step 8's.
- **Step 9 (per-function entry-boundary check).** This work makes step 9 unblockable, not implemented. The renderer's inline-invariants hook (Phase 3) is the architectural prep step 9 will consume.

## Final verification

After all phases land:

- `cargo build --workspace` clean, no new warnings.
- `cargo test --workspace` all green.
- `ARTIFACT_SCHEMA_VERSION` in `analysis_artifact.rs` is `4`.
- `grep -rn "pub struct Invariant\b" crates/` returns zero matches (legacy struct retired).
- `grep -rn "BTreeMap<.*, Invariant>\|audit_data\.invariants" crates/` returns zero matches.
- `grep -rn --include='*.rs' "InvariantKind::" crates/` returns matches in the domain layer, the task layer, the pipeline step, and the renderer hook — but not in any HTTP handler (API layer is out of scope).
- `grep -rn --include='*.rs' "threat_invariants\|subject_invariants" crates/` returns matches in the domain layer, `rebuild_feature_context`, the pipeline step, and (for the renderer) the inline-injection block.
- A trial run of the full pipeline on a known audit produces `InvariantTopic` entries on a meaningful fraction of threats (some threats will legitimately have no invariants, with `[step-8 / no-invariant]` agent comments on their discussion threads). Sampled invariants: each reads as an "X must Y" / "every Z does W" property statement, not a code recommendation and not a scenario; each has exactly one `kind` from the eight-variant enum; each `subject_topic` matches the parent threat's `subject_topic`; each `severity` matches the parent threat's severity (or `None`). Steps 5/6/7 outputs are equivalent or improved against the same fixture — not regressed.
- Rerunning the pipeline twice produces no duplicated `InvariantTopic` entries (clear-on-rerun ran).
- Root `README.md` and `SPEC.md` describe invariants consistently with the post-step-8 model.
