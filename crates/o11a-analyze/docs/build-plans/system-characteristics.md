# System Characteristics — Spec-Prefix Unification + Characteristic Synthesis

This is the implementation plan for elevating system characteristics into a first-class audit entity alongside requirements, behaviors, and features. It also unifies the existing `R`/`B`/`F` topic prefixes into a single `S` ("spec") prefix so the four entity kinds share one topic family and one ID counter, matching the precedent set by `P` (`FunctionalProperty`) and `A` (`AdversarialProperty`).

Characteristics are a new entity kind. They are extracted from documentation alongside requirements (as a separate array in the extraction schema), then re-synthesized after feature synthesis using the raw `security.md` notes as additional context. The only downstream consumer is threat generation (step 8), which renders all `Security`-kind characteristics as a text block in place of the old raw `security_notes` blob.

Read `threats-step-7.md` first; this doc reuses its structural patterns (post-processor validation, idempotent rerun semantics, snapshot/report layer split).

## Summary of decisions

These were settled during design and should not be re-litigated during implementation:

- **One topic prefix for the four-kind spec family.** `Topic::Feature` / `::Requirement` / `::Behavior` collapse into `Topic::Spec(i32)` with prefix `S`. Disambiguation moves entirely to `TopicMetadata` (`FeatureTopic` / `RequirementTopic` / `BehaviorTopic` / new `CharacteristicTopic`). One atomic counter (`NEXT_SPEC_ID`); one allocator (`allocate_spec_id`); one reseeder. Exactly the precedent of `P` covering three property kinds and `A` covering condition/threat/invariant.
- **Characteristics are extracted, then synthesized — both produce `CharacteristicTopic` entries.** Step 2 extraction emits raw `CharacteristicTopic` entries (one per LLM-emitted item). Step 5 synthesis replaces them in-place with a refined set, optionally consolidating across sections and incorporating content from raw `security.md`. Same shape both times; the synthesis step is a rewrite, not a parallel-entity creation. This matches the way requirement extraction's multi-doc consolidation rewrites the requirement set rather than producing a sibling type.
- **Two parallel arrays in the extraction schema.** The LLM returns `feature_requirements` and `system_characteristics` per section, not a single tagged array. Claims that fall in both categories are emitted twice — one in each array, framed appropriately. Same `documentation_topics` across both is fine.
- **`SystemCharacteristicKind` is a pre-defined enum carried on each `CharacteristicTopic`.** Only `Security` is implemented now; the enum exists so other characteristic types (Performance, Convention, …) can be added without further schema bumps. Add a doc comment explaining the other kinds coming later. Each kind has at most one downstream pipeline step it feeds (`Security` → threats); the mapping is hardcoded per kind, not data-driven.
- **No feature linking for characteristics.** Characteristics do not have a `feature_characteristic_links` reverse index. They are included in their entirety in whatever prompt consumes them. Self-contained, no filtering.
- **One characteristic per claim, not per kind.** Per-claim granularity matches `Requirement`. The set of `Security` characteristics taken together forms the audit's "security characteristics document"; threats consumption renders them as a concatenated text block.
- **Synthesis happens after feature synthesis, not before.** The pipeline order is `extract → behaviors → features → characteristics → properties → conditions → threats`. Feature synthesis sees only requirements and behaviors (never characteristics). Characteristic synthesis sees the extracted characteristics + raw `security_notes` (never features). The boundary is enforced by what the renderers emit, not by the prompt instructions alone — verified by a unit test that walks every step's renderer output.
- **No conciseness enforcement.** The synthesis prompt guides the LLM toward brevity but applies no hard cap, no truncation, no compress-pass. If the output bloats in practice, the prompt evolves; the plumbing does not.
- **`security_notes: Option<String>` stays on `AuditData` as the raw input to synthesis.** Its role narrows: previously the threats prompt prefix; now only the characteristic synthesizer's input. It remains in the snapshot and the report for diagnostic/audit-trail purposes. The threats prompt no longer references it.
- **Author follows the task model size.** A `TaskSize::Large` synthesis call writes `Author::AgentLarge`; `TaskSize::Medium` writes `Author::AgentMedium`. Mirrors `extract_requirements_from_documentation` at `task.rs:287`. Initial implementation uses `TaskSize::Large`.
- **Alpha — no migration story.** No existing audits depend on `R`/`B`/`F` prefixes in shipped artifacts or DBs. The schema bump invalidates any in-flight data; re-run `o11a-analyze` to regenerate. The DB tables get fresh names — no `R*`/`B*`/`F*` → `S*` rewriting.
- **`is_technical: bool` stays a bool.** The original rationale (turn it into an enum to include `SecurityCharacteristics`) is gone now that characteristics are not a document kind. No changes to `DocumentFileEntry` or `TopicMetadata::DocumentationTopic`.
- **Re-runs proactively clear downstream data.** The synthesis step is parallel to feature synthesis: clears prior `CharacteristicTopic` entries from `topic_metadata`, clears `audit_data.characteristics`, rebuilds the `section_characteristics` reverse index. The threats step (step 8) does not need any new clear logic.
- **Multi-doc consolidation passes characteristics through unchanged.** The existing `CONSOLIDATE_REQUIREMENTS_PROMPT` second pass (`task.rs:336`) deduplicates requirements across documents but does *not* deduplicate characteristics. All characteristic consolidation happens in Phase 4 synthesis. One consolidation point keeps the data model simple and avoids the LLM having to reason about characteristic merging at two stages.
- **`security_notes` stays in the snapshot and report even after Phase 5.** No remaining server-side reader after Phase 5, but the field is useful for surfacing the original `security.md` alongside synthesized characteristics in the UI, and the bytes cost is negligible.
- **Unknown `SystemCharacteristicKind` values fail loudly at parse time.** The JSON Schema enum constraint on `system_characteristics[*].kind` rejects strings outside the supported set. The extraction prompt instructs the LLM to omit characteristics that don't fit a supported kind rather than emit a sentinel. Silent dropping is the wrong failure mode for a security-relevant artifact.
- **Characteristic synthesis runs even when `security.md` is absent.** As long as there are extracted characteristics, synthesis is valuable for consolidating overlapping claims across documentation sections. Skip only when *both* `security_notes` is empty and the extracted characteristics set is empty.
- **User-authored characteristic creation is deferred.** Phase 6 ships read-only: characteristics flow from the pipeline to the UI; the user can comment on them via the universal comment surface but cannot author new ones in this work. The `user_characteristics` DB tables in Phase 2 land empty for now; the create endpoint and the synthesis-clear interaction (must not clobber user-authored entries) are work for a follow-up.
- **The renderer-leak unit test stays in the test suite permanently.** Phase 4's verification includes a check that no other step's renderer emits `CharacteristicTopic` entries. This is the only mechanical guard against accidental drift; it stays.

## What you will build

Seven phases, sequenced so each compiles and tests clean before the next starts.

1. **Topic prefix unification (precursor).** Collapse `R`/`B`/`F` into `S`. Pure refactor; no new entities or behaviors. Touches `topic.rs`, `ids.rs`, every callsite of the three retired constructors/parsers/allocators, every prompt that quotes a prefix, every test asserting wire format, every DB column comment. Bumps `ARTIFACT_SCHEMA_VERSION` and `SCHEMA_VERSION`.
2. **Domain additions for `Characteristic`.** New struct, new `TopicMetadata` variant, new enum, new `audit_data` field, new reverse index, new `rebuild_feature_context` arm. No pipeline usage yet.
3. **Extraction schema split.** Modify `EXTRACT_REQUIREMENTS_PROMPT`, the JSON schema, `LLMSectionGroup`, `ParsedRequirements`, and `build_requirements` to materialize feature requirements and characteristics in parallel.
4. **Characteristic synthesis step.** New `synthesize_characteristics` pipeline function and `synthesize_characteristics` task. Slot in as step 5 of 8.
5. **Threats consumption swap.** `build_threats` reads from `audit_data.characteristics` (Security kind) instead of `audit_data.security_notes`. `extract_threats_from_batch` keeps its signature.
6. **Web/API surface.** Renderer for `CharacteristicTopic`, response shape, topic view formatting.
7. **README and SPEC sync.** Update the pipeline description, security model docs, and any prefix references in `CLAUDE.md` / `SPEC.md`.

Each phase is independently verifiable. Do not move on until `cargo build --workspace` is clean and `cargo test --workspace` is green.

---

## Phase 1 — Topic prefix unification

### Goal

Reduce the four entity kinds (Requirement, Behavior, Feature, and the new Characteristic) to one shared topic prefix `S` with one shared counter. Pure refactor: identical inputs and outputs to the pipeline modulo the wire-format prefix swap. Phase 1 must end green; Phase 2 adds the new entity on top.

### Files to change

**`crates/o11a-core/src/domain/topic.rs`**

- Remove `Topic::Feature(i32)`, `Topic::Requirement(i32)`, `Topic::Behavior(i32)`. Add `Topic::Spec(i32)`.
- Update the prefix doc comment (lines 5–23): drop `F`/`R`/`B`, add `S → Spec (shared by FeatureTopic, RequirementTopic, BehaviorTopic, and CharacteristicTopic — all four entity kinds in the security-model spec family)`.
- `Topic::prefix()` and `Topic::numeric_id()`: drop the three old arms, add `Topic::Spec(_) => 'S'` / matching id arm.
- `FromStr for Topic`: drop `'F'`/`'R'`/`'B'`, add `'S' => Ok(Topic::Spec(id))`.
- Delete `new_feature_topic`, `new_requirement_topic`, `new_behavior_topic`. Add `new_spec_topic(id: i32) -> Topic`.
- Delete `parse_feature_topic`, `parse_requirement_topic`, `parse_behavior_topic`. Add `parse_spec_topic(s: &str)` using the `define_parse_variant!` macro with the `Spec` variant.
- Update the `tests` module: drop the `Topic::Feature(42)` / `Topic::Requirement(7)` / `Topic::Behavior(13)` cases; add `Topic::Spec(_)` cases asserting `S42`, `S7`, `S13` and round-trip.

**`crates/o11a-core/src/ids.rs`**

- Delete `NEXT_FEATURE_ID`, `NEXT_REQUIREMENT_ID`, `NEXT_BEHAVIOR_ID` and their `allocate_*` / `reseed_*` functions.
- Add `NEXT_SPEC_ID: AtomicI32 = AtomicI32::new(1)`, `allocate_spec_id()`, `reseed_spec_id(max_loaded: i32)`. Doc comment: shared by FeatureTopic, RequirementTopic, BehaviorTopic, CharacteristicTopic.
- Delete the three retired locks (`FEATURE_LOCK`, `REQUIREMENT_LOCK`, `BEHAVIOR_LOCK`) and their tests. Add a `SPEC_LOCK` plus the standard three-test pattern (monotonic, reseed_advances_past_max, reseed_lower_still_stores) used by every other counter.

**Every callsite** of the retired helpers. Find with:
```
grep -rn "new_feature_topic\|new_requirement_topic\|new_behavior_topic" crates/
grep -rn "allocate_feature_id\|allocate_requirement_id\|allocate_behavior_id" crates/
grep -rn "reseed_feature_id\|reseed_requirement_id\|reseed_behavior_id" crates/
grep -rn "parse_feature_topic\|parse_requirement_topic\|parse_behavior_topic" crates/
grep -rn "Topic::Feature\b\|Topic::Requirement\b\|Topic::Behavior\b" crates/
```

The mechanical substitutions are:
- `topic::new_feature_topic(id)` → `topic::new_spec_topic(id)`
- `topic::new_requirement_topic(id)` → `topic::new_spec_topic(id)`
- `topic::new_behavior_topic(id)` → `topic::new_spec_topic(id)`
- `ids::allocate_feature_id()` / `_requirement_id()` / `_behavior_id()` → `ids::allocate_spec_id()`
- `ids::reseed_feature_id(...)` / `_requirement_id(...)` / `_behavior_id(...)` → `ids::reseed_spec_id(...)`
- `Topic::Feature(id)` / `::Requirement(id)` / `::Behavior(id)` pattern matches → `Topic::Spec(id)` (with the kind distinction moved to a `topic_metadata` lookup if the surrounding logic actually depends on it)

The non-mechanical sites are pattern-match arms that branch on kind. `feature_lookup::features_for_topic` (`crates/o11a-core/src/feature_lookup.rs`) currently dispatches on `Topic::Requirement(_)` and `Topic::Behavior(_)` distinctly. After unification, all three (and Characteristic) collapse to `Topic::Spec(_)`; the dispatch moves to `audit_data.topic_metadata.get(t)` and matches on the `TopicMetadata` variant. Same for any other site that pattern-matches the topic variant directly.

**Reseed callers.** The two callers that matter:
- `o11a-server/src/main.rs` (around line 147–179) calls three `reseed_*_id` in sequence after `apply_report` and again after loading user entities. Collapse to one `reseed_spec_id(max)` call per phase, computing `max` as the maximum numeric_id across every `Topic::Spec(_)` in `topic_metadata` plus any spec-topics referenced in `feature_requirement_links` / `feature_behavior_links` keys/values.
- `crates/o11a-core/src/collaborator/db/user_entities.rs` may have analogous reseed logic; collapse the same way.

**Prompts that quote the old prefixes.** Several LLM prompts have hardcoded language like *"R-prefixed topic ID"* or *"B-prefixed"*. Rewrite to `S-prefixed`. The doc-comment-style references are mostly in `crates/o11a-core/src/collaborator/agent/task.rs`. Grep:
```
grep -rn "F-prefixed\|R-prefixed\|B-prefixed\|R-topic\|B-topic\|F-topic" crates/
grep -rn '"R[0-9]\|"B[0-9]\|"F[0-9]' crates/
```
The second grep catches example IDs in prompts (`"R42"`, `"F3"`); update to `S<n>` for stylistic consistency. Examples in tests that assert against canned strings (`"F1"`, `"R3"`) need to update too.

**JSON schemas.** The `REQUIREMENTS_SCHEMA` JSON Schema (`task.rs:205–242`) lists `section_topic` as a `D`-prefixed string and accepts `R`-prefixed strings for requirements implicitly via prose. No structural change to the JSON Schema itself, but the prompt prose must say `S-prefixed`. Same for `features` / `behaviors` schemas elsewhere in `task.rs`.

**DB columns.** Topic IDs are stored as `TEXT` (e.g., `feature_topic TEXT NOT NULL`). The wire format change is purely on write/read paths; no DDL change needed. Comments that say `R-prefixed`/`B-prefixed`/`F-prefixed` update for accuracy.

**`crates/o11a-core/src/analysis_artifact.rs`**

- `ARTIFACT_SCHEMA_VERSION` bump from `4` to `5`. The bincode layout for `Topic` changes (variant index renumbered when three variants merge into one); existing artifacts are unreadable.
- Replace the explicit `FeatureTopic | RequirementTopic | BehaviorTopic | FunctionalSemanticTopic` filter (lines 111–118 and 377–385) with the same four variants — they live on at the `TopicMetadata` level. **Phase 2 adds `CharacteristicTopic` to this filter.**

**`crates/o11a-core/src/report.rs`**

- `SCHEMA_VERSION` bump from `2` to `3`. Topic strings change prefix; consumers expecting `F1`/`R1`/`B1` will fail loudly.
- The `report.pipeline` field types are unchanged in shape; only the embedded topic IDs differ. The hand-written `apply_report` clear block at lines 377–388 (which calls out the four pipeline-output variants) is unchanged in this phase — it's about variant kinds, not prefixes.

**`crates/o11a-core/src/db/mod.rs` and `collaborator/db/`**

DB table names (`user_features`, `user_requirements`, etc.) stay. They identify entity kind, not topic prefix. Inside, the TEXT topic columns store `S<n>` strings going forward.

### How to verify Phase 1

- `cargo build --workspace` clean, no new warnings.
- `cargo test --workspace` all green.
- `grep -rn "Topic::Feature\b\|Topic::Requirement\b\|Topic::Behavior\b" crates/` returns zero matches.
- `grep -rn "new_feature_topic\|new_requirement_topic\|new_behavior_topic" crates/` returns zero matches.
- `grep -rn "allocate_feature_id\|allocate_requirement_id\|allocate_behavior_id" crates/` returns zero matches.
- A round-trip test on every kind: insert `TopicMetadata::FeatureTopic { topic: Topic::Spec(1), .. }`, render it, parse it back, assert variant matches. (Probably already exists for Feature/Requirement/Behavior; adapt.)
- A full `cargo run --bin o11a-analyze -- analyze <fixture> <id>` run finishes and the produced `audit.json` contains only `S`-prefixed topics in `pipeline.features` / `requirements` / `behaviors` / `feature_*_links`. The artifact at `audit.analysis.bin` decodes cleanly with the new `ARTIFACT_SCHEMA_VERSION = 5`.

### Pivotal decision

The `TopicMetadata` variant names (`FeatureTopic`, `RequirementTopic`, `BehaviorTopic`) stay distinct. Topic prefix unification does not unify variant kinds — every pattern-match that genuinely needs to branch on "is this a feature or a behavior?" still has a clean enum to match on. Only the topic ID's serialized form and counter are unified. This matches `P` exactly: `FunctionalSemanticTopic`, `FunctionalPurposeTopic`, `PlacementRationaleTopic` are all distinct variants sharing one prefix.

---

## Phase 2 — Domain additions for `Characteristic`

### Goal

Introduce the new entity into the in-memory model, snapshot, report, DB, and reverse indexes. No pipeline logic exercises it yet — that's Phase 3 (extraction) and Phase 4 (synthesis).

### Files to change

**`crates/o11a-core/src/domain/mod.rs`**

After `pub struct Requirement` (around line 367), add:

```rust
/// Kind of system characteristic. Each kind is consumed by exactly one
/// downstream pipeline step (Security → threats). Add variants additively
/// as new characteristic types are introduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SystemCharacteristicKind {
  Security,
}

impl SystemCharacteristicKind {
  pub fn as_str(self) -> &'static str {
    match self {
      SystemCharacteristicKind::Security => "Security",
    }
  }
}

/// A system characteristic — a system-wide claim (security property, role
/// assumption, trust assumption) extracted from documentation and refined
/// during characteristic synthesis. Characteristics are *not* reconciled
/// against behaviors and are *not* linked to features. The complete set of
/// characteristics of a given kind is consumed in entirety by that kind's
/// downstream pipeline step (Security → threats).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Characteristic {
  /// D-prefixed documentation topics that informed this characteristic.
  /// May be empty for characteristics that originated from `security.md`
  /// rather than a documentation section.
  pub documentation_topics: Vec<topic::Topic>,
}
```

In `TopicMetadata` (around line 1913), add a new variant after `BehaviorTopic`:

```rust
/// A system characteristic — paired with a `Characteristic` entry in
/// `audit_data.characteristics`. The `kind` field selects which downstream
/// pipeline step consumes this characteristic (Security → threats).
CharacteristicTopic {
  topic: topic::Topic,
  description: String,
  kind: SystemCharacteristicKind,
  /// D-prefixed documentation section this characteristic was extracted
  /// from. Empty (`None`) for characteristics whose only source is the
  /// raw `security.md` (no documentation section to anchor to). Matches
  /// the field name on `RequirementTopic` for renderer symmetry; the
  /// `Option` is the only structural difference.
  section_topic: Option<topic::Topic>,
  author: crate::collaborator::models::Author,
  /// `None` for pipeline-produced entities — see FeatureTopic for rationale.
  created_at: Option<String>,
},
```

On `AuditData` (around line 467), add:

```rust
/// Characteristics keyed by S-prefixed topic ID. Replaces the role the raw
/// `security_notes` blob used to play in threats prompting; that field
/// stays as the synthesizer's raw input.
pub characteristics: BTreeMap<topic::Topic, Characteristic>,

/// Reverse index: D-prefixed section topic → S-prefixed characteristic
/// topics. Derived from `CharacteristicTopic.section_topic`, rebuilt with
/// `rebuild_feature_context`. Entries with `section_topic = None` are not
/// indexed here.
pub section_characteristics: BTreeMap<topic::Topic, Vec<topic::Topic>>,
```

Initialize both in `AuditData::new` (or wherever the struct is constructed) as empty.

Extend `rebuild_feature_context` to populate `section_characteristics`:

```rust
audit_data.section_characteristics.clear();
for m in audit_data.topic_metadata.values() {
  if let TopicMetadata::CharacteristicTopic {
    topic,
    section_topic: Some(section),
    ..
  } = m
  {
    audit_data
      .section_characteristics
      .entry(*section)
      .or_default()
      .push(*topic);
  }
}
```

(Match the existing `section_requirements` / `member_behaviors` rebuild idiom in the same function.)

**`crates/o11a-core/src/analysis_artifact.rs`**

- Extend the filter in `snapshot_from_audit_data` (lines 111–118) to include `CharacteristicTopic` — characteristics are pipeline output, so they flow through `audit.json`, not the snapshot.
- Extend `apply_snapshot` (line 165 region) to clear `audit_data.characteristics` and `audit_data.section_characteristics` after applying the snapshot, matching the clear pattern for `requirements` / `feature_requirement_links`.
- Update the doc comment (lines 21–40) to list `characteristics` in the "Excluded — applied from `audit.json`" bullet.
- **`ARTIFACT_SCHEMA_VERSION` bump** from `5` to `6` — new `TopicMetadata` variant changes bincode layout.

**`crates/o11a-core/src/report.rs`**

- Add `ReportCharacteristic` next to `ReportFeature` / `ReportRequirement`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportCharacteristic {
  /// S-prefixed topic id.
  pub topic: String,
  pub description: String,
  pub kind: crate::domain::SystemCharacteristicKind,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub section_topic: Option<String>,
  pub documentation_topics: Vec<String>,
}
```

- Add `characteristics: Vec<ReportCharacteristic>` to `PipelineOutput` (around line 73).
- Add `collect_characteristics` mirroring `collect_requirements`, sorting by topic ID.
- In `apply_report` (line 356 region), extend the clear block to include `TopicMetadata::CharacteristicTopic { .. }` and clear `audit_data.characteristics`. Hydrate from `report.pipeline.characteristics`: insert into `topic_metadata`, `audit_data.characteristics`, computing `max` for the S-counter reseed alongside features/requirements/behaviors.
- **`SCHEMA_VERSION` bump** from `3` to `4`.

**`crates/o11a-core/src/db/mod.rs`**

Mirror the `user_requirements` table pattern (search for `CREATE TABLE IF NOT EXISTS user_requirements`):

```sql
CREATE TABLE IF NOT EXISTS user_characteristics (
  topic_id TEXT PRIMARY KEY,                 -- S-prefixed
  description TEXT NOT NULL,
  kind TEXT NOT NULL,                        -- SystemCharacteristicKind as_str()
  section_topic TEXT,                        -- D-prefixed, nullable
  author TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_user_characteristics_section
  ON user_characteristics(section_topic);
```

Add a parallel `user_characteristic_documentation_topics` table if `user_requirements` has one for its `documentation_topics` list — match the shape exactly.

**`crates/o11a-core/src/collaborator/db/user_entities.rs`**

Add `UserCharacteristicRow`, `insert_user_characteristic`, `load_user_characteristics`, mirroring the user-requirement equivalents. The hydration path in `apply_user_entities_snapshot` inserts `CharacteristicTopic` entries into `topic_metadata`, populates `audit_data.characteristics`, and contributes to the S-counter reseed computation.

### How to verify Phase 2

- `cargo build --workspace` clean.
- `cargo test --workspace` green.
- A unit test in `domain::mod.rs` constructs an `AuditData` with one `CharacteristicTopic` of `kind: Security`, calls `rebuild_feature_context`, and asserts `section_characteristics` contains the expected entry.
- `cargo run --bin o11a-analyze -- analyze <fixture> <id>` still completes — Phase 2 added fields and a variant but no pipeline step that writes them. The fixture produces zero characteristics; `audit.json`'s `pipeline.characteristics` is `[]`.

---

## Phase 3 — Extraction schema split

### Goal

Modify documentation extraction so the LLM emits feature requirements and system characteristics as two parallel arrays per section. Materialize both into `DataContext` during `build_requirements`. Characteristics emitted here are "raw extracted" — Phase 4's synthesis replaces them.

### Files to change

**`crates/o11a-core/src/collaborator/agent/task.rs`**

Rewrite `EXTRACT_REQUIREMENTS_PROMPT` (lines 106–162). The new prompt:

- Keeps the "what to extract" framing for feature requirements verbatim (single-claim, behavioral, no code names).
- Adds a parallel framing for system characteristics: *"System characteristics are system-wide claims about the system that an auditor must take as developer-asserted ground truth when reasoning about adversarial scenarios. For each section, list system-wide constraints, trust assumptions, role definitions, and threat-model statements as `system_characteristics` items. Each characteristic has a `kind` — only `"security"` is supported now."*
- States the overlap rule explicitly: *"A claim that is both a feature-level requirement and a system-wide security characteristic must be emitted twice — once in `feature_requirements` (framed as feature behavior) and once in `system_characteristics` (framed as system-wide guarantee or threat-model statement)."*
- Updates the example IDs to `S`-prefixed where the prompt quotes them.

Rewrite `LLMSectionGroup` (line 195):

```rust
#[derive(Deserialize)]
struct LLMSectionGroup {
  section_topic: String,
  feature_requirements: Vec<LLMRequirement>,
  system_characteristics: Vec<LLMCharacteristic>,
}

#[derive(Deserialize)]
struct LLMCharacteristic {
  description: String,
  documentation_topics: Vec<String>,
  kind: String,  // "security" — parsed into SystemCharacteristicKind
}
```

Update `REQUIREMENTS_SCHEMA` (lines 205–242): each section object now has `feature_requirements` and `system_characteristics` arrays. Add a JSON Schema enum constraint on `system_characteristics[*].kind` listing the currently supported variants (`["security"]`).

Rewrite `ParsedRequirements` (lines 244–250):

```rust
pub struct ParsedRequirements {
  pub requirements: BTreeMap<topic::Topic, Requirement>,
  pub topic_metadata: BTreeMap<topic::Topic, domain::TopicMetadata>,
  pub section_requirements: BTreeMap<topic::Topic, Vec<topic::Topic>>,
  pub characteristics: BTreeMap<topic::Topic, Characteristic>,
  pub section_characteristics: BTreeMap<topic::Topic, Vec<topic::Topic>>,
}
```

Update `parse_requirements_response` (line 253):
- Allocate two independent local-id counters (`req_counter`, `char_counter`).
- Build characteristic topics as `Topic::Spec(<local-id>)` exactly like requirements; the pipeline re-keys both with allocated process-wide IDs.
- Parse `kind` into `SystemCharacteristicKind` (reject unknown variants with a `TaskError`).

Update the multi-doc consolidation flow (`task.rs:336`) to carry characteristics through unchanged. The consolidation LLM call operates on the `feature_requirements` array only — characteristics are accumulated across all per-document responses with no LLM-side merging at this stage, since Phase 4's synthesis is the single canonical consolidation point. Concretely:
- The multi-doc fan-out path collects per-document `ParsedRequirements` results.
- For each result, requirements feed the consolidation prompt (existing behavior).
- Characteristics from every document accumulate into the final `ParsedRequirements.characteristics` and `section_characteristics` without passing through the consolidation LLM.
- Duplicate characteristic descriptions across documents are *expected* at this stage and resolved by Phase 4.

**`crates/o11a-core/src/collaborator/agent/pipeline.rs`**

Extend `build_requirements` (lines 132–263):

- Add a parallel id-remap pass for characteristic topics, using `ids::allocate_spec_id()` (one counter, three uses: existing requirements, new characteristics, future Phase 4 synthesized characteristics).
- After remapping, insert all `CharacteristicTopic` metadata into `topic_metadata`, all `Characteristic` entries into `audit_data.characteristics`, and the `section_characteristics` reverse index entries.
- Extend the existing clear block (line 369 region) to drop `TopicMetadata::CharacteristicTopic { .. }` entries on rerun, and to clear `audit_data.characteristics`.
- Update the log line to mention both counts: *"Extracted N requirements and M characteristics across K sections"*.

### How to verify Phase 3

- A fixture audit with documentation that mentions both feature behavior and access-control rules produces a non-empty `audit.json` `pipeline.characteristics`.
- All emitted characteristics have `kind = "Security"`.
- A characteristic referencing the same documentation topic as a requirement is allowed; the test fixture exercises this overlap.
- Re-running `build_requirements` (e.g., via the test harness's re-entry path) clears prior characteristics and re-emits the new set with freshly allocated S-IDs.

---

## Phase 4 — Characteristic synthesis step

### Goal

Add a new pipeline step that consolidates and refines the raw characteristics extracted in Phase 3, using `audit_data.security_notes` (the raw `security.md` text) as additional context. The output replaces the prior characteristic set in `audit_data.characteristics`. No feature context is rendered; the boundary is enforced by what the synthesizer's renderer emits.

### Files to change

**`crates/o11a-core/src/collaborator/agent/task.rs`**

Add `SYNTHESIZE_CHARACTERISTICS_PROMPT`. The prompt:

- Takes two inputs: `security_notes` (raw `security.md` content, possibly empty) and `extracted_characteristics` (JSON list of `{topic, description, kind, documentation_topics}`).
- Asks the LLM to produce a refined, consolidated set of characteristics that:
  - Merges overlapping claims from `security.md` and the extracted set into single items, preserving `documentation_topics` from the extracted side and dropping the `security.md`-only items' missing section anchors (`section_topic = None`).
  - Promotes any claim present only in `security.md` to a first-class `CharacteristicTopic` with `kind: Security` and no `section_topic`.
  - Refines descriptions for clarity; guides the model toward conciseness without prescribing a length cap.
  - Preserves `documentation_topics` for each claim that traces back to one or more documentation sections.
- Output schema:

```json
{
  "characteristics": [
    {
      "description": "...",
      "kind": "security",
      "section_topic": null | "D5",
      "documentation_topics": ["D7", "D9"]
    }
  ]
}
```

Add `SynthesizedCharacteristics` parsed-shape struct, `synthesize_characteristics` async function, and a `CHARACTERISTICS_SCHEMA` JSON Schema. Author resolution: if the call uses `TaskSize::Large`, the parsed entries carry `Author::AgentLarge`; mirror the convention in `task::extract_requirements_from_documentation` at `task.rs:287`. Initial implementation uses `TaskSize::Large` (parallel to `extract_requirements_from_documentation`).

Add a renderer helper `render_characteristic_synthesis_context(audit_data: &AuditData) -> (String /* security_notes */, String /* extracted_json */)`:
- `security_notes`: `audit_data.security_notes.clone().unwrap_or_default()`.
- `extracted_json`: serialized list of `{topic, description, kind, section_topic, documentation_topics}` for every `CharacteristicTopic` in `topic_metadata`, sorted by topic ID for determinism.

**`crates/o11a-core/src/collaborator/agent/pipeline.rs`**

Add `synthesize_characteristics` mirroring `synthesize_features` (lines 267–382):

1. Snapshot the renderer inputs under a read lock.
2. Early-return *only* if `security_notes` is empty/None **AND** extracted characteristics is empty. If either side has content, the synthesizer runs — an absent `security.md` with extracted characteristics still benefits from cross-section consolidation.
3. Call `task::synthesize_characteristics(security_notes, extracted_json)`.
4. Allocate process-wide IDs via `ids::allocate_spec_id` for each synthesized item.
5. Take a write lock; clear prior `TopicMetadata::CharacteristicTopic { .. }` from `topic_metadata` and clear `audit_data.characteristics`; insert the new set; call `rebuild_feature_context`.

Wire into `run_full_pipeline` as **step 5 of 8**, between feature synthesis and functional purpose/placement:

```
1. Semantic Linking
2. Requirement Extraction
3. Behavior Extraction
4. Feature Synthesis
5. Characteristic Synthesis   ← NEW
6. Functional Purpose & Placement Generation   (was step 5)
7. Condition Generation        (was step 6)
8. Threat Generation           (was step 7)
```

Renumber the existing `[1/7]`–`[7/7]` log lines to `[1/8]`–`[8/8]`. Update the function's docstring (lines 66–96) to describe the 8-step shape and explain where characteristic synthesis fits.

### How to verify Phase 4

- A fixture with both a populated `security.md` and documentation-extracted characteristics produces a single, consolidated `audit.json` `pipeline.characteristics` list. Items appearing only in `security.md` show `section_topic: null`; items extracted from a doc section preserve their `D`-prefixed `section_topic`.
- A fixture with an empty `security.md` but non-empty extracted characteristics runs synthesis and produces a consolidated list (cross-section dedup). The log line distinguishes this case from the no-input case.
- A fixture with an empty `security.md` and no extracted characteristics produces `pipeline.characteristics: []` and Phase 4 logs `"No characteristic input, skipping synthesis"`.
- Re-running `synthesize_characteristics` from a clean state yields the same number of items as a single full pipeline run (idempotent rerun).
- A unit test asserts that none of the renderers used in steps 4 (feature synthesis), 6 (functional properties), or 7 (conditions) include a `CharacteristicTopic` in their rendered output. The check is: render the context for each step on a fixture that contains characteristics, then `grep` the rendered JSON for the characteristics' topic IDs — there should be zero hits. **This test stays in the suite permanently as the drift guard.**

---

## Phase 5 — Threats consumption swap

### Goal

Replace the threats step's read of `audit_data.security_notes` with a rendered text dump of all `CharacteristicTopic { kind: Security }` entries.

### Files to change

**`crates/o11a-core/src/collaborator/agent/pipeline.rs`**

In `build_threats` (around line 1782), replace:

```rust
audit_data.security_notes.clone()
```

with a render of the Security-kind characteristics:

```rust
fn render_security_characteristics(audit_data: &AuditData) -> Option<String> {
  let mut items: Vec<String> = audit_data
    .topic_metadata
    .values()
    .filter_map(|m| match m {
      TopicMetadata::CharacteristicTopic {
        topic,
        description,
        kind: SystemCharacteristicKind::Security,
        ..
      } => Some((topic.numeric_id(), description.clone())),
      _ => None,
    })
    .collect::<Vec<_>>();
  items.sort_by_key(|(id, _)| *id);
  if items.is_empty() {
    return None;
  }
  Some(
    items
      .into_iter()
      .map(|(_, d)| format!("- {}", d))
      .collect::<Vec<_>>()
      .join("\n"),
  )
}
```

The `extract_threats_from_batch(batch_json, label, security_notes: Option<&str>)` signature (`task.rs:2297–2310`) is unchanged — the parameter is just sourced differently. The `"Security context:"` prefix in the prompt body stays.

The threats prompt's surrounding prose should be updated to drop any language tied to the old free-form blob (e.g., references to "the audit's security.md"). Replace with "the audit's security characteristics".

### How to verify Phase 5

- A fixture with one Security characteristic produces a threats prompt whose `Security context:` block contains exactly that description.
- A fixture with zero Security characteristics produces a threats prompt with no `Security context:` block (the `None` path).
- A diff of the threats prompt before/after Phase 5 on the same fixture should show only the security-context content swapping — actor enum, evidence rules, schema, all unchanged.

---

## Phase 6 — Web/API surface

### Goal

Make characteristics visible in the UI and addressable by API consumers, mirroring the existing patterns for requirements and features.

### Files to change

**`crates/o11a-server/src/api/handlers.rs`**

- Add `CharacteristicTopicResponse` mirroring `RequirementTopicResponse` (look at the existing response struct for shape; add `kind` field).
- Add a `GET /audits/:audit_id/characteristics` handler that filters `topic_metadata` for `CharacteristicTopic` entries and returns the list.
- Extend the existing `topic_metadata_to_response` dispatcher (around `api/handlers.rs:748`) with a `CharacteristicTopic` arm.

**User-authored characteristics are deferred** — no `POST /audits/:audit_id/characteristics` handler in this work. The `user_characteristics` tables from Phase 2 stay empty for now. A follow-up will add the create endpoint and resolve the synthesis-clear interaction (the Phase 4 clear must not clobber user-authored entries — likely by filtering on `Author` during the clear).

**`crates/o11a-server/src/api/routes.rs`**

Register the GET route alongside `requirements` routes. No POST route in this work.

**`crates/o11a-web-backend/src/topic_view.rs`**

Add a renderer arm for `TopicMetadata::CharacteristicTopic` mirroring the `RequirementTopic` arm. Title: `Security Characteristic` (parameterized on `kind` once more variants exist).

**`crates/o11a-web-backend/src/formatting.rs`** (or wherever topic-kind labels live)

Add the `CharacteristicTopic` kind label.

### How to verify Phase 6

- `curl /audits/<id>/characteristics` returns the synthesized characteristics from `audit.json`.
- The web frontend's topic view for an `S`-topic that is a CharacteristicTopic renders the description, kind label, and (if present) link back to the source documentation section.
- A comment posted on a CharacteristicTopic via the existing comment surface persists and re-loads (no changes needed — comments key off the universal `Topic` mechanism).
- No `POST /characteristics` route exists; user-authored characteristic creation is deferred.

---

## Phase 7 — README and SPEC sync

### Goal

Reflect the new entity and the prefix unification in user-facing and developer-facing docs.

### Files to change

- **`README.md`** (root) — pipeline diagram, security model section. Add characteristic synthesis as step 5 of 8.
- **`crates/o11a-core/SPEC.md`** — security model section, entity catalog. Add `Characteristic` alongside `Feature`/`Requirement`/`Behavior`. Update the topic-prefix table to show `S` (Spec) covering all four.
- **`CLAUDE.md`** — the "Topic IDs" section at the top of the file lists the prefix map. Update to reflect `S` replacing `R`/`B`/`F` and covering `CharacteristicTopic` as well. Update the "How the pipeline fits together" enumeration to 8 steps.
- **`crates/o11a-analyze/docs/build-plans/threats-step-7.md`** — add a one-paragraph "After step 7 landed, step 7's input changed from raw `security_notes` to rendered Security characteristics" note, with a pointer to this file.
- **`docs/specs/semantic-linking.md`** — no changes (it doesn't reference R/B/F prefixes structurally).
- **`crates/o11a-web-backend/SPEC.md`** — add a renderer note if it has one for topic-kind labels.

### How to verify Phase 7

- `grep -rn "R-prefixed\|B-prefixed\|F-prefixed\|R-topic\|B-topic\|F-topic" .` returns zero matches outside historical-context callouts inside build plans (which describe the previous state and are kept as a record).
- `grep -rn '"R[0-9]\|"B[0-9]\|"F[0-9]' .` returns zero matches outside the same.
- The README's pipeline diagram lists 8 steps in the order in this plan.
