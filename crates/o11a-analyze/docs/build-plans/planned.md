On the framing — "per non-pure subject" vs. "per function"

The author isn't mixed up. They're naming the **output unit** (one bundle of conditions per subject), but the **LLM call** still lives at the batch boundary. Step 5 already has exactly this shape: the prompt at `pipeline.rs:1430` (`extract_functional_properties_from_batch`) takes a 5-function batch and returns subject-keyed entries. Step 6 should follow the same DAG/affinity batches, render the same per-function context (feature, behaviors, semantics, callee behaviors), and additionally inject each subject's `functional_purpose` + `placement_rationale` from step 5. The output is `{ subjects: [{ subject_topic, conditions: [...] }] }`.

That second outline paragraph ("You could batch this per-function too") is the author confirming the same intent — they wrote "per subject" to describe the unit of reasoning, not the unit of API call. So: keep the DAG, keep the batches, change what's injected and what comes out.

## Proposed shape for steps 6–8

| Step | Generation unit | Output keyed by | Prior context needed |
|---|---|---|---|
| 6 Conditions | per-batch | subject | purpose + placement_rationale (step 5), behaviors (step 3), feature (step 4), callee behaviors |
| 7 Threats | per-batch | subject | step 6 conditions on the subject + everything step 6 had |
| 8 Invariants | per-batch (or per-threat post-pass) | threat | step 7 threats + the threat's subject context |

Steps 6 and 7 can stay bundled in one LLM call per batch if you want — "for each subject in this batch, list condition observations and the threats those observations imply" — but I'd separate them because the outline's diagnosable-disagreement property is the whole point: the auditor needs to be able to agree with an observation but reject the threat without re-deriving both.

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
