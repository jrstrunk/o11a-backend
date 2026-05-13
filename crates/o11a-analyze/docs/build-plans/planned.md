 On the framing — "per non-pure subject" vs. "per function"

The author isn't mixed up. They're naming the **output unit** (one bundle of conditions per subject), but the **LLM call** still lives at the batch boundary. Step 5 already has exactly this shape: the prompt at `pipeline.rs:1430` (`extract_functional_properties_from_batch`) takes a 5-function batch and returns subject-keyed entries. Step 6 should follow the same DAG/affinity batches, render the same per-function context (feature, behaviors, semantics, callee behaviors), and additionally inject each subject's `functional_purpose` + `placement_rationale` from step 5. The output is `{ subjects: [{ subject_topic, conditions: [...] }] }`.

That second outline paragraph ("You could batch this per-function too") is the author confirming the same intent — they wrote "per subject" to describe the unit of reasoning, not the unit of API call. So: keep the DAG, keep the batches, change what's injected and what comes out.

## Proposed shape for steps 6–8

| Step | Generation unit | Output keyed by | Prior context needed |
|---|---|---|---|
| 6 Conditions | per-batch | subject | purpose + placement_rationale (step 5), behaviors (step 3), feature (step 4), callee behaviors |
| 7 Threats | per-batch | subject | step 6 conditions on the subject + everything step 6 had |
| 8 Invariants | per-batch (or per-threat post-pass) | threat | step 7 threats + the threat's subject context |

Steps 6 and 7 should be separated because the outline's diagnosable-disagreement property is the whole point: the auditor needs to be able to agree with an observation but reject the threat without re-deriving both.

## What delivers the most audit value

**Step 6 is the unlock.** The whole reasoning chain hinges on conditions being purpose-derived rather than type-templated. The current `NonPureSubjectType` enum (`domain/mod.rs:1172`) has six variants and the SPEC defines a fixed question list per variant — the same questions on every `StateWrite`, regardless of what the write does. Replacing that with "given this purpose, what observations would matter for whether the purpose can fail?" is what produces interesting conditions. Without this change, threats remain generic.

**The per-function entry-boundary absence check is the second-most-valuable addition** and I'd build it before step 8. It catches missing-modifier bugs (the highest-frequency real-audit finding) that subject-level analysis structurally cannot surface — if `onlyOwner` is missing on a privileged setter, no subject inside the function will flag its own absence. Cheap: one call per public in-scope function, depends only on function-level purpose, no batch coordination. Slot it as `[5b/N]` between step 5 and step 6.

**Step 7 is high-value but only after step 6 lands.** The "purpose-inversion + who controls the mechanism" framing is what produces actionable threats vs. generic ones. The `ThreatTopic` variant (`domain/mod.rs:1944`) already has `description` and `subject_topic`; the actor identity could go in the description or get its own field — worth deciding before building.

**Step 8 is the most mechanical.** Defer until 6+7 are validated end-to-end on a real audit; you'll learn whether one invariant per threat is actually right (some threats imply multiple invariants; multiple threats can share one invariant).

**Interaction sequences (data model only) are worth adding now even though the outline says they don't depend on this work.** Detecting maximal runs of non-pure subjects is mechanical, and if the AST/topic model has the structure in place, the future CEI/TOCTOU pass slots in without retrofitting. This is the cheapest piece of the whole plan and the only one that ages well — purpose-driven conditions might evolve, but "consecutive non-pure subjects in a function body" is a stable concept.

## Three design questions to settle before coding

1. **Do `Condition`s get a `ConditionTopic` `TopicMetadata` variant?** Currently they're anonymous entries in `AuditData.conditions: Vec<Condition>` (`domain/mod.rs:461`), unlike `ThreatTopic`/`InvariantTopic` which are addressable. If conditions need approval, correction, or re-derivation triggers (which the SPEC implies — "an auditor can agree with an observation but disagree with the threat assessment"), they need topic IDs. I'd promote them.

2. **What happens to `condition_type: NonPureSubjectType` (`domain/mod.rs:373`) when conditions are purpose-driven?** Either vestigial (drop it), or a filter facet for the auditor UI (keep it, populated mechanically from the subject kind). Probably keep — it's free to populate from the subject's `UnnamedTopicKind`, and grouping conditions by subject type in review is useful.

3. **Threat actor as a structured field?** The outline says step 7 produces "for each [mechanism], identify who controls the mechanism." That's load-bearing for impact analysis. Either widen `ThreatTopic` with a `controlled_by: ThreatActor` enum (`Caller`, `Admin`, `External`, `Miner/Sequencer`, `AnyParty`), or leave it embedded in `description`. I'd lean structured — it's used for downstream filtering ("show me all caller-controlled threats on this feature").

Want me to turn any of this into a concrete plan doc next to `pipeline-dag.md`, or work through one of those design questions in more depth first?

## User

Should the per-function entry-boundary absence check happen as step 9 after step 8, so that it can have full context of the conditions, threats, and invariants?

Move conditions to the topic metadata so that they fit in with the rest of the system and can have conversation around them.[Request interrupted by user]Sorry.

Should the per-function entry-boundary absence check happen as step 9 after step 8, so that it can have full context of the conditions, threats, and invariants?

Make the threat actor a structured field in step 7.

To focus on step 6, the next step to implement:

Move conditions to the topic metadata so that they fit in with the rest of the system and can have conversation around them.

Ignoring what was put in place before, what structure of conditions would deliver the most value? The condiditon has to be linked to the function it is generated against, but should it also hold semantic blocks within the function that are relevant to the condidion? Should it hold anything else structurally? What delivers the most value?

## Assistant

<thinking>

</thinking>

Quick yeses on the first two:

**Step 9 entry-boundary check after step 8:** yes, that's better. With conditions/threats/invariants on hand, the entry-boundary prompt becomes "what threats already documented inside this function would a missing entry-level guard *enable*?" — sharper than "what protections would you expect from purpose alone." Same one-call-per-public-function shape, much higher-precision findings. The function's accumulated threats also let the check distinguish "this needs an `onlyOwner`" from "this needs a `nonReentrant`" from "this needs a price-deviation bound."

**Threat actor structured in step 7:** noted, will design that into the `ThreatTopic` widening when we get there.

## Step 10 — Cross-Site Pattern Analysis (per declaration)

A pipeline step that consumes the per-subject reasoning produced by steps 5–7 (and 9) and produces **findings** by comparing how a declaration is used across all sites that touch it. Internal name: "patterns." Slots after step 9; can run in parallel with step 9 since both consume the same per-subject input.

### The gap this fills

Subject-level reasoning answers "is this statement correct?" Cross-site analysis answers "are these statements consistent?" The audit needs both, and they are different mechanisms. Steps 5–7 produce per-subject artifacts; step 9 produces per-function findings. Neither layer can structurally surface findings whose signal lives in the *delta* between sibling sites that touch the same declaration. Examples of findings step 10 produces that no other step can:

- "12 functions write `_balances`; 11 check `msg.sender`; 1 doesn't" — missing access control at the outlier site.
- "Every read of `lastUpdate` is followed by a freshness comparison except in this one function" — staleness bug.
- "All state-modifying functions are gated by `whenNotPaused` except this admin path" — pause bypass.
- "Writes to `_totalSupply` use `+=` at 5 sites and `=` at 1 site" — accounting discrepancy.

These are bread-and-butter audit findings. The SPEC's invariant + re-check mechanism handles them today only after the auditor manually notices the pattern at one canonical site. Step 10 mechanizes the noticing.

### Generation unit and call shape

Per declaration with at least two non-pure references in scope. Singletons get no call (no cross-site signal exists yet). Input per call: every site that references the declaration, rendered as `(subject_topic, role from AST, containing function + feature, purpose, placement, conditions, threats, adjacent defenses: modifiers on the function + prior checks in the same block)`. Output:

| Output kind | Description |
|---|---|
| Declaration role synthesis | What this declaration *is* in the project, derived from observed usage patterns. Persists as a project-level annotation on the declaration. |
| Consistency findings | Defenses/conditions present at most sites but missing at some. Each finding cites the canonical sites and the outlier. |
| Outlier findings | Unique threats, unique placements, or unique purposes at one site that don't appear elsewhere. May be intentional or buggy — surfaces for review either way. |
| Cross-cutting threats | Threat patterns that span multiple sites and motivate a scope-level invariant. Feeds invariant generation when the threat would be defended by a single property propagated across all sites. |

Each finding is a first-class topic with traceability links back to every per-site artifact it was derived from. Auditors review findings the same way they review threats — with the underlying per-site reasoning visible.

### Sibling to step 9, not a duplicate

| | Pattern axis | Detection target |
|---|---|---|
| Step 9 | per-function | "this function lacks an entry-level guard its threats imply" |
| Step 10 | per-declaration | "this site lacks a defense its sibling sites have" |

Both consume the per-subject pipeline artifacts. Both produce findings, not enrichment. Together they capture the two axes where cross-site signal lives that subject-level reasoning structurally cannot reach.

### Why this is the right answer over enrichment alternatives

Earlier design iterations explored injecting cross-site context into the per-subject steps (cross-feature stamps in renderer, reverse-indexed purposes feeding step 5 second pass, per-block behaviors). All of those try to make per-statement prose richer. The audit value isn't there — a statement's purpose is local and doesn't need cross-site context to be correct. The audit value lives in *aggregation* of per-statement reasoning at the declaration level, where consistency findings emerge.

Step 10 uses the per-subject pipeline artifacts as input without changing how they're produced. No new prose level (per-block behaviors), no new bridging structure (evidence pointers, member-feature classification), no second pass of per-subject steps. Strictly additive.

### Cost notes

One LLM call per multi-reference declaration in scope. For an audit with ~60 in-scope functions, that's roughly the count of state variables + frequently-used errors + frequently-used modifiers + frequently-called internal helpers — typically 30–80 calls. Per-call input is bounded by the declaration's reference count; for hot declarations, sample by feature diversity to cap input size. Per-call output is small (a handful of findings).

Total cost is in the same range as steps 5/6/7 each. Quality argument: the findings produced are structurally not derivable from any other step, so the cost isn't competing with per-subject quality — it's buying a deliverable that doesn't exist otherwise.

### Three open design questions

1. **Findings as their own topic family or piggyback on threats?** Consistency findings ("this site lacks a defense its siblings have") are threat-shaped — they describe a risk. Outlier findings less so. Probably introduce a `PatternFindingTopic` to keep the surface auditable independently, with links *to* threats it implies rather than *being* threats.

2. **Re-derivation triggers on edit.** When an auditor corrects a single subject's purpose/conditions/threats, the relevant declarations' findings need re-running. Scope: declarations referenced by the edited subject. Re-check infrastructure already exists for invariants; reuse the propagation pattern.

3. **Severity of consistency findings.** Outlier-among-N is a strong signal; severity could be derived from N (more siblings → stronger signal that the outlier is a bug). But the outlier might be intentional (admin path), so initial severity is "needs review" with auditor confirmation upgrading to a fixed severity. Mirrors the impact-analysis pattern for threats.

### Out of scope for this step

- On-the-fly per-declaration re-analysis when an auditor adds a new subject post-pipeline. The generator should be structured so a single-declaration caller can reuse it, but the call site is not built initially.
- Cross-codebase pattern transfer (learning that "all X-pattern violations were bugs in audit Y → flag them harder in audit Z"). Out of scope; future work.
- Statistical confidence on patterns (e.g., bayesian scoring of outlier-likelihood). LLM judgment over the aggregate is the v1 mechanism; quantitative scoring can layer on if useful.

## Non-goals

### Patterns stay a derived view; do not move pattern signal into per-subject generation

**Decision.** Cross-site pattern analysis (step 10) remains a strictly separate, derived view built on top of the per-subject primary artifacts (purpose, placement, conditions, threats). Pattern signal is **not** injected into step 5/6/7 prompts; per-subject artifact storage is **not** decomposed by any cross-site sharing structure (declaration-axis baselines, role clusters, evidence pointers, or similar); per-subject steps run single-pass without cross-site enrichment.

**Reasoning.** When 12 sites generate similar "caller is authorized" conditions, that is **regularity**, not duplication. Regularity is the signal the auditor cares about — it tells them "these sites share an assumption." Eliminating the regularity at the storage layer (by sharing one condition object across sites, or by clustering sites into roles, or by inventing baseline + local + exemption layers) erases the audit signal and forces every subsequent design choice to invent a way back to per-site reasoning. Each attempted decomposition during design produced bandaids — exemptions, effective-condition merge functions, multiple-baselines-per-axis, role confidence — because the decomposition was trying to convert a derived view (cross-site similarity) into primary storage.

The principle: **don't conflate the layer where audit work happens with the layer where data is stored.** Audit work happens across sites (pattern recognition, consistency checks, outlier detection); audit data lives at its natural unit (the subject). Build aggregation surfaces at the work layer that consume — but do not replace — the primary per-subject data.

**What this means concretely.** Step 6 produces per-subject conditions with each subject's local context. Step 7 produces per-subject threats. Step 10 consumes those per-subject artifacts and emits cross-site pattern findings as a separate topic family with traceability back to the underlying per-site reasoning. Approval and correction at the pattern level are a UX affordance — the auditor reviews "the caller-authorization pattern across 12 sites" once at the pattern layer, with the approval propagating to the 12 underlying conditions through the existing approval mechanism. This is a workflow optimization, not a data model change.

**What was considered and rejected.**

- Cross-feature stamps in renderer / reverse-indexed purposes feeding step 5 second pass / per-block behaviors → conflated audit work layer with data storage layer; statement purpose is locally determined and doesn't benefit from cross-site context.
- Per-(declaration, axis) baseline conditions with per-subject local conditions and exemptions → introduced exemptions and a merge function as bandaids over the false assumption that axes are the unit of sharing.
- Per-(declaration, axis, role) baseline conditions with role-based clustering → roles are non-crisp and LLM-derived; singleton roles collapse to per-site; role confidence is another bandaid.

Each alternative was a progressively more elaborate attempt to move pattern signal into primary storage. The cleanest answer is that the original step 6 + step 10 split is correct: per-subject primary, patterns derived.
