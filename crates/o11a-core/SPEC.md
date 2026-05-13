# o11a-core SPEC

This document is the implementation spec for the parser, analyzer, and checker. The README covers what each stage does and why; this document covers how each is structured. Read the README first.

# Parser

## Solidity Source Parsing

Solidity source files are parsed and compiled by the Foundry compilers, which output an AST file in JSON format that the parser reads.

The parser enhances original Solidity ASTs by adding semantic blocks. Semantic blocks are not in the original AST; they are added by analyzing source files for consecutive newlines between statements in a block. This produces a structure where Block nodes contain SemanticBlock nodes, which in turn contain statement nodes. Each semantic block has optional documentation drawn from comments that appear at the beginning of the block in the source, preserving the developer's contextual and documentation comments.

## Documentation Source Parsing

Documentation source files (Markdown today; potentially Plain Text and Djot later) are compiled by a markdown Rust crate that produces an AST directly within the application. Documentation files are read from the list of files in the `documents.txt` file, which should be present in the audit's root project directory.

In documentation files, sections and paragraphs become declarations and inline code snippets become references. These may contain references by name to the same variables and functions as the implementation. Because of this, documents are parsed last so that they can be parsed with the full context of the source code, and can resolve any references to the code by searching for a declaration that contains the same name.

# Analyzer

The AST given to the analyzer contains unique IDs for each node, and each time an identifier appears in the AST, its node contains a `reference_id` property pointing to the ID of the node that declared it.

Each audit contains a `scope.txt` file in the root project directory. This file lists paths to all files in scope of the audit, allowing the analyzer to focus on in-scope contracts while still providing differentiated support for non-in-scope contracts.

Projects under audit can pull in many dependencies but only use a few functions from them. Naively processing every contract and function in every file leads to excessive processing and bloating in the audit. To avoid this, the analyzer takes a two-pass approach so that only in-scope contracts/functions, and the contracts/functions used by them, are processed:

1. First, read and parse each AST, storing its declarations in an accumulating simple declaration dictionary that stores all declarations in the audit by node ID. With function and modifier declarations, store a list of the other nodes referenced in their bodies, the require and revert statements, the function calls, and the variable mutations. Note whether the function/modifier at hand is from an in-scope AST. This is the first pass and is a great place to do processing that needs to check all child nodes recursively but without knowledge of the nodes they may reference.
2. Loop over all the declarations, storing the publicly (public or external; not internal, private, or local) in-scope declarations in a new dictionary that stores all publicly in-scope declarations in the audit and the nodes that reference them. When a declaration is found to be publicly in-scope, add it to the in-scope declaration dictionary and look up its referenced nodes in the previously generated dictionary. Add each of these references to the accumulating in-scope dictionary along with the node that referenced it, then recursively check these references for their own references, adding them as needed and so on.
3. Now with a dictionary of all in-scope declarations, parse each AST into memory one at a time, checking each declaration for inclusion in the in-scope dictionary. If it is, add it and its child nodes to an accumulating collection of dictionaries that make up the complete data set needed for the rest of the application. This is the second pass; it is a great place to perform processing that requires knowledge of a node's references.

The exact data this three-step process creates goes into forming the `Data Context` type:

1. A set of files that are in scope for the audit
2. A directory of nodes by topic ID, where each node's children are stored as node stubs
3. A directory of all declarations (reference-able identifiers in the source code) by topic ID with their name, scope, and declaration kind
4. A directory of references to the declaration by topic ID
5. A directory of extended properties for functions and modifiers, including function parameters, returns, reverts, calls to other functions within them, and mutations to state variables within them. Each of these properties has rich data about the subjects and contains references to the relevant declarations when possible (e.g., the function arguments list the topic ID for the local variables that the arguments are mapped to)

See the collaborator section in the o11a-server README for topic ID details.

Declarations are scoped by four properties: Container, Component, Member, and SemanticBlock. Using the scope, any identifier/operation or its parent can be linked to.

For contract source files, the container is the source file, the component is a contract, the member is a function, and the semantic block is a semantic block, block, or signature. A contract's scope is only a container, a function's scope is a container and a component, a parameter variable's scope is a container, component, and member, and a local variable's scope is a container, component, member, and semantic block. For documentation, the container is the source file, the component is the first header section, the member is the second header section, and the semantic block is the following nested header sections. For comments, the scope is copied from the topic the comment is being added to, since comments do not have complex formatting and structures.

# Checker

## Security Model

### Topic Prefix Map

Every artifact in an audit is addressed by a `(prefix_char, i32)` pair. Prefixes group entity kinds that share a single `i32` counter; the entity kind itself lives in `TopicMetadata`, so two artifacts of different kinds can never collide on the same ID.

| Prefix | Variant                   | TopicMetadata variants sharing this prefix                                            |
| ------ | ------------------------- | ------------------------------------------------------------------------------------- |
| `N`    | `Node`                    | Source AST nodes                                                                      |
| `D`    | `Documentation`           | Documentation sections, paragraphs, inline references                                 |
| `C`    | `Comment`                 | User and agent comments                                                               |
| `S`    | `Spec`                    | `FeatureTopic`, `RequirementTopic`, `BehaviorTopic`, `CharacteristicTopic`            |
| `A`    | `AdversarialProperty`     | `ConditionTopic`, `ThreatTopic`, `InvariantTopic`, `ValidationTopic`                  |
| `P`    | `FunctionalProperty`      | `FunctionalSemanticTopic`, `FunctionalPurposeTopic`, `PlacementRationaleTopic`        |
| `Y`    | `TypeConstraint`          | Type constraints                                                                      |

The `S`-family lets all four security-model spec entities share one counter and one wire-format prefix, paralleling how `A` covers the four adversarial-layer entities and `P` covers the three functional-property entities. The split between counter and kind is what lets a comment on, say, `S42` point at whichever of feature/requirement/behavior/characteristic occupies that ID without losing addressability.

### Initial Generation

The security model is initially seeded from project documentation and source code through an automated pipeline of ten steps:

1. Semantic Linking
2. Requirement Extraction
3. Behavior Extraction
4. Feature Synthesis via Reconciliation
5. Characteristic Synthesis
6. Functional Purpose & Placement Generation
7. Condition Generation
8. Threat Generation
9. Invariant Generation
10. Invariant Validation

Step 1 produces the project-specific vocabulary that every later step relies on. Steps 2–5 build the documentation-and-code-derived spec layer (`S`-family entities). Steps 6–10 build the adversarial layer on every non-pure source subject (`P`-family purpose+placement, then `A`-family conditions, threats, invariants, and validation verdicts on those invariants). Each step's output is durable and individually addressable, and the pipeline is structured so any one of them can be re-run on its own when its inputs change.

**1. Semantic Linking.** Documentation sections are linked to source code declarations to establish functional semantics — the project-specific meaning of each declaration. This step runs before requirement and behavior extraction so that both can be generated with business-level meaning. Functional semantics are injected into the rendered documentation that requirement extraction sees, so inline code references like `pID` appear annotated with their project-specific meaning (e.g., "participation identifier"), giving the LLM proper context to produce behavioral requirements without using declaration names.

Semantic linking uses a five-step approach that alternates between **association** (mechanical inline-reference resolution + BM25 expansion) and **synthesis** (LLM generation). Earlier synthesis steps inform later ones — the contract's semantic gives context for member synthesis, and the member's semantic gives context for body-local synthesis:

*Step 1 — associate document sections to contracts:* The mechanical layer resolves inline code references in documentation to declarations and walks each declaration's scope chain upward to find its containing contract. BM25 then unions in additional contracts whose member surface lexically resembles the section's prose.

*Step 2 — add semantic links to contracts:* For each section, the LLM sees the section text alongside the matched contracts (each rendered as name + contract-level NatSpec + a list of public member names) and produces one semantic per contract entity. Because the same contract may match multiple sections, semantics produced for the same contract across sections are condensed in place into a single semantic before the next step runs.

*Step 3 — associate document sections to contract members:* The mechanical layer takes the declarations resolved in step 1 and walks their scope chains to the member level. For state variables (scoped at the contract level, not inside any member), it fans out to members that read or write the variable using the analyzer's tracked mutations. BM25 then unions in additional indexable members whose name + NatSpec lexically resembles the section's prose.

*Step 4 — add semantic links to contract members:* Two LLM batches per section, both rendered with the step 2 contract semantics injected as context: a *member-scoped* batch covering function/modifier topics with their parameters and return values, and a *contract-scoped* batch covering non-function component-scoped declarations (state variables, events, errors, struct/enum definitions, struct fields, enum members). Member semantics are then condensed in place to one per declaration.

*Step 5 — add semantic links to contract member bodies:* Per-section LLM call covering body locals (declarations with `Scope::ContainingBlock`), rendered with both step 2 (contract) and step 4 (member + signature) semantics in context. A statement like `let ret = Contract.transfer(input, to)` produces meaningful semantics for `ret`, `input`, and `to` only when the contract, function, and signature are already meaningful. Body-local semantics are condensed in place at the end of the step.

The prompts in steps 2, 4, and 5 are explicit: semantics must reflect what the **documentation says** the declaration represents, not what the code does with it. Source code is provided only to help the LLM identify which declarations the documentation is describing (e.g., confirming that `pID` in the docs refers to the `participationId` parameter). If the documentation says a variable is a "proportional reward factor" but the code uses it as a divisor, the semantic should still be "proportional reward factor" — that mismatch is valuable information for auditors.

Each functional semantic is persisted on the declaration with both the semantic text and a provenance link to the documentation topic it was derived from. This provenance enables auditor verification (trace back to the source passage), change tracking (documentation edits invalidate only affected semantics), and the full traceability chain once features are synthesized. Declarations that no documentation section matches don't get semantics in this step — they can receive semantics on-demand during convergence evaluation, or manually from the auditor.

**2. Requirement Extraction.** Documentation files are processed to extract requirements, with functional semantics from the previous step injected into the rendered documentation. Inline code references appear annotated with their semantic meaning, so the LLM has project-specific context for every resolved declaration name. Requirements are extracted preserving the documentation's section structure — each documentation section produces a list of requirements grouped under that section. Requirements describe capabilities in behavioral terms without using code declaration names (e.g., "only the authorized relayer can invalidate participations" rather than "invalidateParticipations() must have access control"), and each requirement captures exactly one claim. Each requirement retains links to the documentation topic it was derived from, preserving traceability. When multiple documents exist, each is analyzed independently and then consolidated. Requirements remain organized under their documentation sections until reconciliation groups them into features.

**3. Behavior Extraction.** Source code is processed to extract behaviors, preserving the code's scope structure. Each contract member produces a list of behaviors grouped under its scope (container, component, member). With functional semantics already populated on declarations, behavior extraction produces business-level descriptions rather than mechanical ones. A function containing `propFactor * stakerBalance` where `propFactor` has the semantic "proportional reward multiplier" and `stakerBalance` has the semantic "user's staked token balance" produces the behavior "calculates proportional reward share for the staker" rather than "multiplies propFactor by stakerBalance." Behaviors remain organized under their code scopes until reconciliation groups them into features.

**4. Feature Synthesis via Reconciliation.** All requirements and all behaviors are presented to the LLM in a single pass for reconciliation. Because both requirements and behaviors were generated with functional semantics in context, they share a consistent vocabulary — requirements say "proportional reward distribution" and behaviors say "calculates proportional reward share for the staker" — making the grouping task straightforward textual matching rather than requiring mechanical bridging through semantic links.

The reconciliation groups related requirements and behaviors into features, with each feature's description synthesized from both the documented intent and the implemented reality. Requirements with no matching behaviors produce features representing unimplemented specification. Behaviors with no matching requirements produce features representing undocumented implementation. Both are findings that the reconciliation surfaces structurally.

Where requirements and invariants both describe things the code must do, they serve different concerns. Requirements capture what the documentation claims — the functionality described to users or the protocol. Invariants capture what the code must enforce to protect against threats — the defensive properties that prevent threats from materializing. A collateral lending feature has a documented requirement that users can deposit ETH, but the invariant that only the position owner can withdraw collateral exists to protect against a threat, not to fulfill a documented claim. Requirements are verified by matching them to behaviors during reconciliation; invariants are verified by checking them against convergences in the source code.

**5. Characteristic Synthesis.** System characteristics are extracted from documentation alongside requirements in step 2 (the per-section extraction call emits two parallel arrays — `feature_requirements` and `system_characteristics` — so claims framed in feature terms and claims framed as system-wide guarantees coexist without being conflated). After feature synthesis, step 5 consolidates and refines those raw characteristics. The synthesis call takes two inputs: the raw `security.md` notes (`audit_data.security_notes`) and the JSON-serialized list of extracted characteristics. It produces a refined set that merges overlapping claims, promotes `security.md`-only items to first-class topics with `section_topic = None`, and refines descriptions for clarity. The synthesis runs whenever either input has content — an absent `security.md` with extracted characteristics still benefits from cross-section consolidation; the step is skipped only when both inputs are empty.

Each characteristic carries a `kind` drawn from a closed `SystemCharacteristicKind` enum; only `Security` is implemented at present, and the enum exists so other kinds (performance, convention, etc.) can be added without further schema bumps. Each `kind` is consumed by exactly one downstream pipeline step. Characteristics are deliberately not linked to features and not reconciled against behaviors — linking them to features would force a feature-level decomposition of claims that are system-wide by construction; reconciling them against behaviors would conflate the layer they live at with the behavior layer. Their self-contained nature is what lets downstream steps consume them in entirety without filtering.

The pipeline order is deliberate: feature synthesis runs before characteristic synthesis (not after) so that the boundary stays clean. Feature synthesis sees only requirements and behaviors; characteristic synthesis sees only its raw `security.md` notes and the extracted characteristics. The split is enforced by what each step's renderer emits, not by prompt prose alone, and a permanent unit test verifies that no other step's renderer leaks `CharacteristicTopic` entries into its rendered context. On rerun, step 5 proactively clears prior `CharacteristicTopic` entries from `topic_metadata` and clears `audit_data.characteristics` and the `section_characteristics` reverse index before inserting the new set; the threats step does not need a separate clear because its consumption is read-only.

The raw `security.md` content stays in `audit_data.security_notes` after Phase 5: it remains the input to step 5's synthesis on future reruns, it can be surfaced in the UI alongside the synthesized characteristics for diagnostic comparison, and the bytes cost is negligible. Phase 5 retired the use of `security_notes` as the threats prompt's free-form security blob; threats consume the rendered Security characteristic set instead.

**6. Functional Purpose & Placement Generation.** For every non-pure source subject in an in-scope function with a feature link, the pipeline produces two sibling properties: the **functional purpose** (the business-logic reason the subject exists) and the **placement rationale** (the ordering reason the subject is at this point in its containing function rather than earlier or later). Both are produced by a single per-function LLM call that renders the function with each non-pure subject marked inline and injects the function's feature context plus the relevant functional semantics. See "Managing Functional Purpose" below for the design rationale (purpose and placement as siblings, per-function batching, adversarial second pass).

**7. Condition Generation.** For each non-pure subject, conditions — positive assertions about what must hold for the subject's purpose+placement to be fulfilled — are produced uniformly. Each condition carries a `kind` drawn from the closed `ConditionKind` enum. Conditions provide threat generation with bounded, concrete inputs to invert rather than open-ended adversarial reasoning. See "Managing Conditions" below.

**8. Threat Generation.** For each non-pure subject, threats are generated as adversarial inversions of its conditions — each threat states a scenario in which a specific condition fails to hold and links back to that condition. The audit-wide adversarial context is the rendered set of `Security`-kind characteristics from step 5 (a concatenated text block, one bullet per characteristic), which replaces the role the raw `security.md` blob used to play; the threats prompt no longer references `security_notes` directly. See "Managing Threats and Invariants" below.

**9. Invariant Generation.** For each threat produced in step 8, the pipeline generates zero or more **invariants** — codebase-level defensive properties stated against the threat. Each invariant is phrased as an "X must Y" / "every Z does W" property statement, carries a `kind` drawn from the closed `InvariantKind` enum (grouping authorization & lifecycle gates, reentrancy & ordering, bounds & ranges, freshness & oracles, state conservation, input well-formedness, external call safety, cryptography & one-shot, bounded computation, semantic/economic identities, and a catch-all `Other`), inherits `subject_topic` and `severity` from the parent threat at write time, carries optional `anchors` (the declaration topics the property is stated against — modifier, role variable, state field), and names exactly one parent threat. One threat can carry many invariants. When the LLM finds no defendable property for a threat, it records a `no_invariant_rationale` so the threat's review carries an explicit audit signal that it was considered. See "Managing Threats and Invariants" below.

**10. Invariant Validation.** For each invariant produced in step 9, the pipeline produces a single `ValidationTopic` carrying a **verdict** on whether the invariant's property actually holds in the code at the validated subject. The verdict is one of `Enforced` / `Absent` / `Partial` / `Inconclusive`; `Inconclusive` is a first-class verdict (no v1 harness for `EconomicInvariant`, anchor declarations not visible in the function's surface, etc.) rather than a fallback. Each validation names exactly one parent `invariant_topic`, carries a one-sentence `rationale` explaining the verdict, and cites `evidence_topics` (modifiers, role checks, state writes, return-value checks, or — for `Absent` — the subject itself as "where the enforcement should have been") inside the subject's containing function. Cross-site propagation — validating one invariant at every subject in scope where the property must hold — and the entry-boundary absence check are **deferred later pipeline steps** (steps 11/12 when they land).

### Out-of-Scope and Dependency Code

The source-driven steps of the initial generation pipeline — behavior extraction, feature synthesis, functional purpose & placement generation, condition generation, threat generation, invariant generation, and invariant validation — run only against **in-scope contracts**: the contracts listed in the audit's `scope.txt` file. Out-of-scope contracts and library dependencies (OpenZeppelin, Solmate, etc.) are excluded from these steps even though they are present in the analyzed codebase. The documentation-driven steps (requirement extraction, characteristic synthesis) operate on the audit's documentation and `security.md` regardless of which contracts those documents reference. Semantic linking sits in between: it produces semantics for both in-scope declarations (so behavior extraction can use them) and out-of-scope declarations that in-scope code calls into (so the auditor knows what the dependency does).

This distinction is fundamental. In-scope code is the code under review — it cannot be assumed to be correct, which is why it needs requirements extracted from documentation, behaviors reconciled against those requirements, threats identified on its non-pure subjects, and invariants checked at its convergences. The full security model exists to surface mismatches between what the documentation claims and what the code does.

Out-of-scope code and dependencies are assumed to be correct. They are battle-tested libraries or previously audited contracts that the in-scope code builds on. Running the full pipeline against them would produce requirements like "tokens must transfer correctly" against OpenZeppelin's `SafeERC20` — requirements that are not the concern of this audit and would dilute the auditor's focus.

However, out-of-scope code still needs **functional semantics** and **behaviors** for the in-scope analysis to work. When in-scope code calls `SafeERC20.safeTransfer()`, the auditor needs to know what that function does. When a dependency's state variable is read by in-scope code, the auditor needs its semantic meaning. This context is necessary for understanding the in-scope code's behavior, even though the dependency itself is not being audited.

Out-of-scope code therefore requires a separate, lighter mechanism: generating semantics and behaviors without requirements, features, threats, or reconciliation. This mechanism documents what the dependency does (semantics and behaviors) so that in-scope analysis has the context it needs, without generating the security model artifacts that only apply to code under review. This separate mechanism is not yet implemented.

### On-the-Fly Generation

The security model is not static after initial generation. As auditors review code and encounter security-relevant patterns, they add new requirements, behaviors, conditions, threats, invariants, and functional semantics directly from the code context. This on-the-fly generation is the primary mechanism through which the audit achieves comprehensive coverage.

When a user adds a new element to the security model, the relevant pipeline steps run automatically in the background:

- **New requirement** — Added under the relevant documentation section or feature. Requirements do not trigger threat generation; they are documentation claims that will be verified during reconciliation against behaviors.
- **New behavior** — Created during code review, grouped under the code scope where it was observed. If features have already been synthesized, the behavior is associated with the appropriate feature based on its code scope. Behaviors are reconciliation artifacts and do not carry threat or invariant links.
- **New system characteristic** — Authoring of new characteristics from the audit UI is deferred; the read-only flow ships now. Pipeline-produced characteristics participate in the universal comment and approval surface like any other topic, but cannot be directly created or rewritten by auditors in this milestone. When the create-endpoint lands, a new `Security` characteristic will trigger re-evaluation of threats on subjects whose threat generation incorporated the previous characteristic set, since the system-wide adversarial context will have shifted; the synthesis-clear in step 5 will be made author-aware so that auditor-authored entries survive pipeline reruns.
- **New functional semantic** — Persisted on a declaration with provenance to its source documentation topic. If the semantic changes the meaning of a declaration, downstream properties (behaviors that reference the declaration, functional purpose on containing statements) may need re-evaluation.
- **New functional purpose / placement rationale** — Generated as a sibling pair on a non-pure subject. If purpose is added or corrected, conditions and threats on the same subject re-evaluate against the new purpose. Placement rationale invalidates when surrounding statements in the containing function change; purpose invalidates when the subject's feature description changes.
- **New condition** — Recorded as an assertion on a non-pure subject (what must hold for its purpose+placement to be fulfilled). Adding or correcting a condition triggers re-evaluation of the subject's threats, since each threat is anchored to a specific condition it falsifies; a new or revised assertion may surface threat scenarios not previously identified.
- **New threat** — Generated on non-pure subjects as the adversarial inversion of a specific condition (see Managing Threats and Invariants below). Carries a link to the condition it falsifies. Created without a mandatory link to a feature, allowing the discoverer to record it immediately. Invariants are generated from the threat and attached to the subject. The system triggers re-checks on the subject. The feature linkage is established during impact analysis.
- **New invariant** — Recorded as a defensive property against a parent threat, attached to the threat's subject. Adding or correcting an invariant flags re-verification of where the property holds in the code (deferred to a later pipeline step). The system does not yet trigger re-checks against related subjects within scope — that propagation is part of the deferred step.
- **New validation** — Recorded as a verdict on a parent invariant at the validated subject. Adding or correcting a validation does not propagate to other subjects in scope; cross-site propagation is a deferred later pipeline step.

The user's request completes immediately; background work finishes asynchronously.

This re-check mechanism is what makes the system's backward-only evaluation context sufficient for comprehensive coverage (see Subject Evaluation Context Strategy below). Rather than including forward context to passively surface gaps, the system actively propagates newly-discovered concerns to all relevant code locations. When an auditor notices an access control check on one function and registers it as an invariant, every other function within that invariant's scope is mechanically re-evaluated — surfacing any that lack the expected check without the auditor needing to have seen all those functions in the same context window.

### Reconciliation

Reconciliation is both the final step of initial generation (where features are first synthesized) and an ongoing audit activity (where the auditor evaluates coverage). During initial generation, reconciliation groups requirements and behaviors into features using the semantic links as the bridge. As an audit activity, the auditor compares the feature's requirements against its behaviors and evaluates coverage in both directions:

- **Requirements without matching behaviors** are unimplemented specification — the documentation claims the system does something, but no corresponding behavior was observed in the code.
- **Behaviors without matching requirements** are undocumented implementation — the code does something significant that the documentation does not describe.

Both are findings. The nature of the relationship between individual requirements and behaviors — whether a behavior fulfills, constrains, or conflicts with a requirement — is assessed by the auditor during this step. This keeps behavior extraction lightweight and focused on the code, while reconciliation provides the dedicated context for documentation-implementation comparison.

Features structure the reconciliation into manageable units. Instead of reconciling all requirements against all behaviors in an audit at once, the auditor works through one feature at a time.

### Impact Analysis

Impact analysis is the dedicated step where threats on source subjects are linked to the features they affect. Threats are created during code review without a mandatory feature link — the discoverer records the implementation-specific risk immediately with whatever context they have, keeping them focused on the code. Impact analysis is where the linkage happens, with the right context and the right person.

The link between a threat and a feature carries a relationship type: either "is vulnerable to" (the subject is part of the attack surface for a concern within the feature) or "defends against" (the subject is part of the defense against a concern within the feature). Both relationships establish why the subject matters from a security perspective and determine the nature of the audit emphasis — attack surface subjects need verification that they're safe, defense subjects need verification that they're sufficient.

Severity is assigned during impact analysis based on the feature context and the nature of the threat. A reentrancy risk on an external call in a lending contract is critical because the feature involves user funds. A configuration read related to a display-only feature is low severity. Until impact analysis is performed, the threat has no severity — it is flagged as needing impact assessment.

Threats that cannot be linked to any feature are one of two things: either the feature set is incomplete (a behavioral area of the code wasn't captured as a feature), which prompts adding a missing feature; or the threat is not actually a threat (the failure mode doesn't harm any feature), which prompts reconsidering whether it's valid. Both are useful signals that impact analysis surfaces.

Some threats link to multiple features — a storage collision might affect collateral management and governance simultaneously. A proxy vulnerability might affect every feature. Impact analysis supports multiple links, and the threat inherits the highest severity among them.

The system tracks unlinked threats and surfaces them for review. Unlinked threats that persist to the end of the audit indicate either incomplete impact analysis or threats that should be reconsidered.

### Hierarchy

```
Feature
  "Protocol Campaigns"
  ├── Requirement (from docs)                              ← what the docs claim
  │     "Protocols can permissionlessly deploy a campaign for their token"
  ├── Requirement (from docs)
  │     "Campaign creators can withdraw remaining tokens after campaign ends"
  ├── Behavior (from code)                                 ← what the code does (reconciliation)
  │     "Campaign deploys and registers token in campaignsByToken mapping"
  │     └── Source Links: [deployCampaign(), campaignsByToken]
  └── Behavior (from code)
        "Only one campaign can exist per token"
        └── Source Links: [deployCampaign(), campaignsByToken]

Feature-to-Source Links (from reconciliation):
  deployCampaign()    → "Protocol Campaigns"
  withdrawRemaining() → "Protocol Campaigns"
  campaignsByToken    → "Protocol Campaigns"

Source Subject: deployCampaign() → IERC20(token).transferFrom(...)   [non-pure: external call]
  ├── Functional Purpose: "Transfer campaign tokens from creator into contract"
  ├── Functional Semantics:
  │     ├── "token": "campaign reward token" (from docs: D3 "Campaign Token Setup")
  │     └── "amount": "total campaign allocation" (from docs: D5 "Campaign Funding")
  ├── Condition: "createPair invocations are not pre-emptable for the same token address"
  │     └── Kind: RestrictedReachability
  ├── Condition: "no token-callback re-entry observes partial state during setup"
  │     └── Kind: AtomicConsistency
  ├── Threat: "The deterministic token address can be pre-computed and createPair called first, bricking deployment"
  │     ├── Falsifies condition: "createPair invocations are not pre-emptable..."
  │     ├── Controlled by: AnyParty
  │     └── Impact: [is vulnerable to] "Protocol Campaigns" (severity: critical)
  ├── Threat: "The token callback re-enters before the surrounding state commits"
  │     ├── Falsifies condition: "no token-callback re-entry observes partial state..."
  │     ├── Controlled by: External
  │     └── Impact: [is vulnerable to] "Protocol Campaigns" (severity: critical)
  ├── Invariant: "State updates must precede external calls in deployCampaign"
  │     ├── Kind: CheckEffectsInteractions
  │     ├── Parent Threat: "The token callback re-enters..."
  │     └── Validation: Enforced — "campaignsByToken write precedes the IERC20.transferFrom call; no state mutation follows the external call"
  └── Invariant: "Every campaign token address must be unpredictable to outside callers before deployment commits"
        ├── Kind: Other
        ├── Parent Threat: "The deterministic token address can be pre-computed..."
        └── Validation: Inconclusive — "Token address derivation happens outside the validated function; predictability cannot be judged from the subject's surface"
```

## Convergences

Across the audit, two types of convergences are checked for contradictions:

1. Type Convergences (where two values interact with each other via an operator)
2. Specification Convergences (where many pieces of code implement a behavioral feature)

### Type Convergence

There are many type properties that can be checked on the subjects of a type convergence, depending on the type of subject:

- All:
  1. Error-Causing Values (values for this subject that will cause an error)
- Numbers:
  1. Upper bound
  2. Lower bound
  3. Set of values
  4. Set of excluded values
- Addresses:
  1. Trusted
  2. Untrusted
  3. Set of values
  4. Set of excluded values
  5. Implements
- Lists:
  1. Length
- Mappings:
  1. Set of keys
- Enums:
  1. Enum variants

Type properties converge when an operator is called:

- Function/struct arguments (checked from the argument variable to the parameter variable)
- Variable assignment/mutations (checked from the value to the variable)
- Unary, binary, ternary operators (checked on the operands)

Type convergences are purely logical and can be checked for contradictions by a type checking algorithm.

Type constraints help identify values that cause error conditions.

#### Variable Ancestors

Function parameters each have ancestors that are the call arguments which correspond to the function parameters. A parameter may have many ancestors, as it has one for each call to that function. In the case of a function that is only called once, the parameter ancestors can be thought of as the same as the parameter itself. This is called a transitive ancestor. In the analysis, both the subject and ancestor can be given the same topic so they will be treated as the same in the audit.

Ancestors can be literal values.

When auditing a variable, it is useful for the client to present all of its ancestors to the user to check at once. This may reveal a common pattern or outlier among the ancestors.

### Specification Convergence

Specification convergences verify that the implementation of a subject upholds the properties defined in the security model. Where type convergences check mechanical type properties, specification convergences check that the behaviors observed in code, the defensive invariants on subjects, and the functional properties are consistent and correctly implemented.

There are three types of properties that may be checked on the subjects of a specification convergence, depending on the kind of subject:

- Project Implementation, Contracts, Blocks, Statements:
  1. Functional Purpose (what purpose it serves within the context of the application, derived from the feature it belongs to)
  2. Behaviors (the observed behaviors that apply to this subject, associated via feature reconciliation)
  3. Dependencies (what the subject depends on to fulfill its purpose, expressed as links to other statements or mechanisms)
  4. Invariants (defensive properties the subject must uphold, derived from threats)
- Non-Pure Subjects (state reads, state writes, external calls, delegatecalls, assembly blocks):
  1. All of the above, plus:
  2. Conditions (positive assertions about what must hold for the subject's purpose+placement to be fulfilled)
  3. Threats (implementation-specific risks generated as adversarial inversions of specific conditions)
- Expressions and Values:
  1. Functional Semantics (what it represents within the context of the application, derived from project documentation)

Specification properties converge between the declaration and implementation of the subject:

- Project Implementation (checked that the sum of the properties of the contracts match the properties of the project implementation)
- Contracts (checked that the sum of the properties of the functions match the properties of the contract)
- Functions (checked that the sum of the properties of the block statements match the properties of the function)
- If/else statements (checked that the sum of the properties of the block statements match the properties of the if/else statement)
- Loops (checked that the sum of the properties of the block statements match the properties of the loop)
- Statements (checked that the sum of the properties of the expressions in the statement match the properties of the statement)
- Expressions (checked that the the properties of the values/subexpressions in context of the operation match the properties of the expression)

Specification properties are intrinsic to the project's security model, and these properties converge with the project implementation's specification properties.

Specification convergences are based on project-specific design and cannot be checked by an algorithm. Instead, they must be manually verified to uphold within the unique project environment.

#### Function Call Convergences

Function calls are an expression that has a subexpression of argument list passing, which results in the return value of the function. Because of this, function calls have both semantic properties based on their return value that can be checked with other semantic properties in a straightforward way, and other functional properties.

Index access only has semantic properties like values.

Functions have a semantic return value that can converge with other semantic properties in an expression, but they also have functional properties. These functional properties may not affect the semantics of the expression, but they converge with the containing statement's functional properties. For example, `add(a, b) - c` has a semantic property convergence at `the_result_of_add_a_b - c` to form one semantic property for the expression, but the functional properties of `add` converge with the containing statement's functional properties to make sure they fulfill the containing statement's behaviors and align with its purpose.

#### Function Call Signatures

Function calls need to have comprehensive signatures that include all necessary information for the caller to understand the function's behavior and potential side effects. These are of course the input parameters and return types like Solidity, but also the exceptions that can be thrown by the function, the state variables that are read, the state variables that are mutated, and the functions that are called (with their respective signatures).

Functions need to track side effects and present them to the user in the interface, like exceptions are.

### Managing Functional Purpose

The functional purpose is the business logic reason for a non-pure subject — the "why" that subject is there, or more precisely, "why did the developer write this subject to do what it does?" An access control guard's functional purpose is not "checks that msg.sender equals owner" (that's what it does), but "to prevent other users from taking other users' funds" (that's why it exists).

Functional purpose is derived from the feature the subject belongs to combined with understanding of the code, and in a single-agent context it could be considered redundant — an auditor who knows the feature and reads the code can infer the purpose implicitly. The reason it exists as an explicit, stored property is to **externalize an intermediate reasoning step that would otherwise be opaque at collaboration boundaries**.

When reasoning passes between agents — an LLM doing an initial pass and a human following behind, or two human auditors reviewing the same code — implicit reasoning is lost at each handoff. If the LLM implicitly reasons "this access check prevents unauthorized fund access" and generates threats based on that understanding, but the human auditor thinks it enforces a delegation model, they disagree on threat coverage without knowing they disagree on the premise. That disagreement surfaces late, at the threat level, where it's harder to diagnose and resolve. Making the "why" explicit creates a checkpoint where reasoning can be inspected, corrected, and agreed upon before downstream analysis builds on it. The human sees the LLM's stated purpose, corrects it if wrong, and now threat generation and behavior documentation operate from a shared, verified premise.

This applies in both directions. When the human states a purpose and the LLM generates threats, the LLM has an explicit anchor to reason from rather than inferring intent from a behavior description that could be interpreted multiple ways. When the LLM states a purpose and the human reviews, the human gets a window into the LLM's understanding that they can verify independently of whether the downstream outputs look reasonable.

#### Purpose and Placement Are Separate Properties

Functional purpose is captured as **two sibling properties** on each non-pure subject:

- **Functional purpose** — the business-logic reason this subject exists. Answers "what value does this provide to the project?" Independent of where in the function the subject sits.
- **Placement rationale** — the ordering reason this subject is at this point in its containing function rather than earlier or later. Answers "what invariant or behavior would break if this moved?"

Both are generated by the same LLM pass and persisted as separate `TopicMetadata` variants (`FunctionalPurposeTopic` and `PlacementRationaleTopic`), each carrying a `subject_topic` link to the non-pure subject they describe. Keeping them as siblings rather than fields on a single struct gives each independent addressability: specification convergences that care only about purpose (e.g., feature alignment) can depend on it without pulling in placement, and convergences that care only about ordering (e.g., CEI compliance across an external call) can do the inverse. It also lets the auditor agree with one and disagree with the other as a single-property correction rather than a struct edit, and lets each property re-derive independently when its inputs change — purpose invalidates when a feature description shifts, placement invalidates when surrounding statements change.

The two questions are also not redundant. "Why does this exist?" and "Why is this here?" can produce the same answer for trivially-placed subjects, but for any subject with ordering significance — guards relative to mutations, mutations relative to external calls, reads relative to writes that produce them — the placement question carries reasoning that the purpose question does not surface. Asking only "why?" makes the system answer the easier of the two questions and skip the one where most security-relevant ordering bugs live.

#### Initial Pipeline Generation

Functional purpose and placement rationale are generated as a dedicated pipeline step that runs after feature synthesis. Per-function batching is the unit of generation: one LLM call per in-scope function (or modifier) that contains at least one non-pure subject. The function is rendered with each non-pure subject marked inline, the function's feature context (description, requirements, behaviors) is injected, and the LLM produces both properties for each marked subject in a single response. Per-function batching is the right granularity because placement reasoning requires seeing the function as a whole — what comes before and after each subject is implicit in the rendered ordering — and because purpose itself is bounded by the function's role within its feature.

The pipeline step runs after feature synthesis specifically because it depends on feature context. Subjects in functions that did not receive a feature link during reconciliation generate with a degraded prompt (no business context); the absence of a feature link is itself a reconciliation finding that should be addressed before downstream threat generation depends on the purpose.

#### On-the-Fly Generation

Subjects added or modified after the initial pipeline pass — newly-introduced statements, or subjects whose feature context has shifted — generate purpose and placement on-demand using the same prompt scoped to a single subject within its containing function. This shares the prompt and rendering with the batch path; only the input scope differs.

#### Adversarial Second Pass

After the initial generation, an adversarial second pass critiques the generated purpose and placement rationale. The critique LLM call is given the generated properties and the same context as the initial generation, and asked to argue against them — what would the strongest case for these being wrong look like, and what incorrect code would still match the stated purpose? The critique is persisted alongside the purpose and placement and presented to the auditor during review, so the auditor sees both the proposed reasoning and its strongest counterargument. This breaks the anchoring effect of a single confident answer without restructuring the pipeline around multi-hypothesis selection: one canonical purpose still flows downstream, but the auditor's review surface includes the critique that pushes back on it.

The critique is not an alternative purpose to choose from. It is an explicit attempt to falsify the generated answer. If the critique succeeds (the auditor agrees the purpose is wrong), the auditor corrects the purpose; the critique becomes evidence that prompted the correction. If the critique fails (the auditor agrees the purpose holds despite the strongest counterargument), the act of considering and dismissing the critique converts approval from a frictionless click into a verified judgment.

#### Guidance on Writing Purpose

Try to avoid implementation details in functional purposes, and focus on the business logic. "Why" is not "to do the thing it does." It is "from the project perspective, what value does this statement provide?", and "what impact would it have on the users or system if it weren't there?"

When adding functional purposes, preset questions are presented to the user that help guide them in thinking about the purpose correctly. Functional purposes can be categorized, which will adjust the preset questions for the user to answer. Some of the categories could be for Shared Resources, Authorization, or Reentry Guards. This will make sure the user is reminded of common issues with these things, like a DOS exhaustion of a shared resource.

### Managing Functional Semantics

Functional semantics define what an expression or value represents within the context of the application — the project-specific meaning of a variable, literal, or sub-expression. For example, `userBalance` might semantically represent "the user's total balance" in one project and "the user's locked balance" in another. `propFactor` might semantically represent "a balance multiplier." These meanings are not derivable from the code alone — they come from project documentation.

Functional semantics are generated upfront during the semantic linking step of initial generation, before behavior extraction. This ordering is critical — behaviors extracted with functional semantics in context carry business-level meaning ("calculates proportional reward share") rather than mechanical descriptions ("multiplies propFactor by stakerBalance"), which makes reconciliation between documentation requirements and code behaviors a straightforward matching task.

Each functional semantic is persisted on the declaration with both the semantic text and a provenance link to the documentation topic it was derived from. The provenance enables the auditor to trace any semantic back to the documentation passage that established it, verify the LLM's interpretation against the original text, and identify which semantics are affected when documentation changes.

The semantic linking process uses a layered approach to manage cost (see Initial Generation). The mechanical layer resolves inline code references and walks scopes to produce section-to-contract associations. The LLM passes match documentation sections to contracts (pass one, cheap) and then match within each section-contract pair to specific declarations (pass two, bounded context). This avoids processing thousands of declarations against full documentation, instead decomposing the problem into many small, tractable matching tasks.

Declarations that are not matched to any documentation section during semantic linking can receive semantics through two fallback paths: on-demand generation during convergence evaluation (the LLM is given the declaration and its feature context and asked "what is the semantic meaning of this declaration within this feature?"), or manual annotation by the auditor at any point during code review. Both produce the same result — a functional semantic persisted on the declaration — but without documentation provenance.

Once generated, the functional semantic is presented to the human auditor for review. The auditor can correct it, enrich it with domain knowledge, or confirm it. The corrected semantic is cached and reused at every convergence involving that subject.

Functional semantics are checked at specification convergences. When the auditor or LLM evaluates a statement like `userBonus = interest * userBalance`, the functional semantic on `userBalance` ("the user's total balance") can contradict the containing statement's functional purpose ("calculate bonus based on locked token interest"). Similarly, if `propFactor` has the semantic "a balance multiplier" but appears in an addition operation `val = propFactor + bal`, the semantic contradicts the operator. These contradictions are only surfaceable when the documentation-derived semantic and the code-derived context are both present at the convergence point.

### Managing Requirements

Requirements are what the documentation claims the system does. Each requirement captures a documented behavior — something the documentation says the system needs to accomplish. They can describe any kind of documented behavior: functionality, constraints, access control, edge case handling. For example, documentation about campaign deployment might have requirements like "protocols can permissionlessly deploy a campaign for their token" and "campaign creators can withdraw remaining tokens after campaign ends."

Requirements are extracted from documentation sections during initial generation, preserving the documentation's section structure. They do not carry source links, threat links, or invariant links — security analysis operates at the source subject level. Requirements are verified not by checking them directly against source code, but by confirming during reconciliation that corresponding behaviors exist in the implementation. Once reconciliation synthesizes features, requirements are grouped under their feature.

All requirements on a feature are distinct and independent of each other. Requirements are added initially as unverified, then are able to be marked as verified by each party in the audit during reconciliation.

Requirements are generally explored as documentation is reviewed, but auditors can also add requirements on-the-fly when they encounter documented claims that were not captured during initial generation.

### Managing Behaviors

Behaviors are what the code actually does. Each behavior captures an observed implementation behavior — extracted from source code or identified by the auditor during code review. A behavior is a description and source links to the code where it was observed. Because behaviors are extracted after functional semantics are populated, they carry business-level meaning derived from the documentation-linked semantics on the declarations they involve.

Behaviors are extracted preserving the code's scope structure (container, component, member). Once reconciliation synthesizes features, behaviors are grouped under their feature based on the semantic links between their code scope and the documentation sections that form the feature.

Behaviors are purely reconciliation artifacts. They do not carry threat links or invariant links — security analysis operates at the source subject level through threats and invariants on convergences. Behaviors exist to enable the comparison between what the documentation claims (requirements) and what the code does (behaviors) during reconciliation.

### Managing System Characteristics

System characteristics are system-wide claims about the project that an auditor must take as developer-asserted ground truth when reasoning about adversarial scenarios. They capture trust assumptions ("the relayer is honest within the bound of its bond"), role definitions ("the owner is the only address that can pause"), and threat-model statements ("front-running is in scope; censorship by the sequencer is not") — claims that bound what threat reasoning must consider rather than naming behaviors of any one feature.

#### Data Model

Each characteristic is represented by a `TopicMetadata::CharacteristicTopic` paired with an entry in `audit_data.characteristics`:

- The `CharacteristicTopic` variant carries the topic (`S`-prefixed), the description, the `kind: SystemCharacteristicKind`, an optional `section_topic` (`D`-prefixed; `None` when the characteristic came from `security.md` rather than a documentation section), the author, and an optional `created_at`. The `Option<String>` on `created_at` matches the pipeline-author convention used by feature/requirement/behavior topics — pipeline-produced entities omit it.
- The `Characteristic` struct holds the trace links: a list of `D`-prefixed documentation topics that informed the characteristic. Pipeline-only characteristics may have an empty list when the original source was `security.md` with no documentation anchor.
- `SystemCharacteristicKind` is a closed enum. Only `Security` is implemented at present. The enum exists so additional kinds (performance, convention, etc.) can be added in an additive way; each kind has at most one downstream pipeline step that consumes it.
- The `section_characteristics` reverse index on `AuditData` maps a `D`-prefixed section topic to the list of `S`-prefixed characteristic topics anchored to it. It is rebuilt by `rebuild_feature_context` from the `CharacteristicTopic.section_topic` field. Characteristics with `section_topic = None` are not indexed here.

#### Lifecycle in the Pipeline

Characteristics are produced in two pipeline steps:

1. **Extraction (step 2).** The documentation extraction prompt asks the LLM to emit two parallel arrays per section: `feature_requirements` and `system_characteristics`. A claim that is both a feature-level requirement and a system-wide characteristic is emitted twice, framed appropriately in each array; no deduplication is attempted at this stage. The extraction schema enforces `kind` as a JSON Schema enum (currently `["security"]`); unknown values fail loudly at parse time. The multi-doc consolidation pass that runs over `feature_requirements` does not run over `system_characteristics` — duplicates across documents are expected and resolved in step 5.
2. **Synthesis (step 5).** After feature synthesis, the synthesizer reads `audit_data.security_notes` and the JSON-rendered extracted characteristics, and produces a refined, consolidated set. The output replaces the prior characteristics: step 5 clears `CharacteristicTopic` entries from `topic_metadata`, clears `audit_data.characteristics`, allocates fresh `S`-IDs for the synthesized items, and rebuilds the `section_characteristics` reverse index. The synthesis is skipped only when both `security_notes` and the extracted set are empty.

Steps 6–7 do not touch characteristics. Step 8 (threats) consumes them — see "Managing Threats and Invariants".

#### Layer Boundary

Feature synthesis does not see characteristics, and characteristic synthesis does not see features. The boundary is enforced by what each step's renderer emits, not by prompt prose; a permanent unit test asserts that no other step's renderer leaks `CharacteristicTopic` entries into its rendered context. This is the only mechanical guard against accidental drift across the layer split.

#### Consumption by Downstream Steps

Each `SystemCharacteristicKind` is consumed by exactly one downstream pipeline step. The mapping is hardcoded per kind, not data-driven. At present:

- `Security` → threat generation (step 8). The threats prompt's `Security context:` block is built by rendering every `CharacteristicTopic { kind: Security }` description as a single concatenated text block (one bullet per characteristic, sorted by topic ID), replacing the role `audit_data.security_notes` played in earlier versions of the pipeline. When there are no `Security` characteristics, the block is omitted entirely (no fallback to the raw `security.md`).

The set of characteristics of a given kind is consumed in entirety — there is no feature-level filtering. This is intentional: characteristics are system-wide claims that bound how every threat must be reasoned about; restricting them per-feature would defeat their role. As more kinds are added, each will name its own downstream consumer; the dispatch on `kind` lives in the consumer, not in the characteristic itself.

#### Raw Security Notes

`audit_data.security_notes: Option<String>` remains the raw text of the audit's `security.md` (if any). After step 5 retired its role in threat prompting, its remaining purposes are:

- Input to step 5 synthesis on every pipeline run.
- Diagnostic surfacing in the UI alongside synthesized characteristics, so an auditor can compare the synthesized set against the original prose.
- Audit-trail durability: the field stays in the snapshot and the report so the original input is recoverable.

There is no remaining server-side reader after step 5; the field is a quiet record of the input, not an active prompt segment.

#### Auditor-Created Characteristics (Deferred)

The DB tables for `user_characteristics` are provisioned in the schema, but no `POST /audits/:audit_id/characteristics` route exists in this milestone. When the create endpoint lands, the step 5 synthesis-clear path must filter on `Author` so that auditor-authored entries survive pipeline reruns. This work is deliberately deferred; the read-only flow ships first to validate the pipeline-produced set in real audits before adding an authoring surface that has to coexist with rerun semantics.

### Managing Dependencies

Dependencies are things that a subject depends on outside of its interface (and thus cannot be expressed by a type constraint). These are things like a stateful function that requires another block to set some required piece of state for it to work with. Like an emergency exit function that requires an exit address to be set first. Because the dependency can be fulfilled by another block far away from the subject, statement chains have to be tracked throughout the project so we can tell where a project dependency could be fulfilled. To mark a dependency as fulfilled, the user has to provide a statement id that satisfies the dependency. This statement then becomes a convergence point for the dependency.

Statement chains are as follows: there is a main, same block chain. This represents all the statements before the subject in the same block. If the dependency is satisfied by one of these statements, then the dependency will always be satisfied and we can mark it as verified. If these same-block statements do not satisfy the dependency, then it may be satisfied by a statement above the containing block in the same function, but not in sub-statements of prior sibling statements (this makes it so that both blocks of an if/else statement are blanket checked as one). If statements within the containing function do not satisfy the dependency, then it may be satisfied within the constructor statements. If it is not satisfied within the constructor, then it may be satisfied within a prior sibling statement in each block where the containing function is called. (If the containing function is an external function, then the dependency can be found anywhere in the project, as they will have to be two separate user calls.) If one of the calls does not satisfy the dependency, then it is unsatisfied as a whole. If the immediate call block does not satisfy the dependency, then a prior block in the call chain is searched for in the same way as the original same block chain.

Referencing mutable values (notably state variables, but mutable local variables too) will always have a dependency associated. Mutations to the state variable that happen always within the call chain of the statement must be considered, and introduce local variable refinements (i.e., a variable may always be non-zero in one place it is referenced, but not in another because of the checks/mutations that happen before it in a chain). Because of this, a reference to a mutable variable will have its own topic (which represents the unique state of the referenced variable at the reference point), while also being linked to the topic of the variable itself. A reference to an immutable variable can be treated as the same topic as the variable itself.

To implement this, we can gather all statements that could satisfy the dependency (when a dependency is added, as many statements will have none and we do not want to waste time processing them) and store them as a tree, with the subject being the root, and tree depth representing prior statements. Then we take the statements that the user said satisfied the dependency, and search for a path to a leaf statement that does not encounter one of the satisfying statements. If we can get to a leaf without encountering a satisfying statement, then that is a path to the subject where the dependency is not satisfied, and the dependency is not satisfied as a whole. A leaf node would be some entry point to the project, like a public function. For this implementation, we would have to store an AST of statements to pull the sub-trees from to get the subject as the root.

### Managing Consumers

A consumer is a statement that consumes what was set up by the current statement. A dependency checks that something was set up before the current statement, and a consumer checks that what is set up by the current statement is properly used by later statements. A consumer turns into a dependency on the consuming statement when it is linked, and a dependency turns into a consumer on the depended-upon statement when it is linked, so they can both be checked in the same way. Dependencies and consumers cannot be checked exhaustively, so checking for the existence or fullness of them is an important job of the auditor. When a statement has a side effect, at least one consumer must exist and should be searched for. The consumer is first added in an unsatisfied state, and added for satisfaction when a satisfying statement is found. Checking for at least one consumer is a way to make sure that something is not set up and then forgotten to be consumed, indicating a potential bug.

### Managing Conditions

The checker classifies source code subjects as pure or non-pure based on their interaction surface. Pure operations (arithmetic, comparisons, boolean logic, local variable assignments) have a closed threat surface — the only things that can go wrong are mechanical (overflow, type mismatch, wrong operands), and these are fully covered by type convergences. Non-pure operations interact with persistent state, external code, or the blockchain environment, creating implementation-specific attack surface that requires structured analysis before adversarial reasoning.

Non-pure subject types include:

- State writes (storage mutations)
- State reads of mutable variables (storage reads that could return manipulated or stale data)
- External calls (transfers control to untrusted code)
- Delegatecalls (executes in a different context than it appears)
- Assembly blocks (bypasses compiler safety checks)
- Selfdestruct / create / create2

Each non-pure subject carries conditions: positive **assertions** about what must hold for the subject's functional purpose and placement rationale to be fulfilled. A condition catalogues an assumption the subject's purpose makes about its environment — what its caller, inputs, callees, or surrounding state must look like for the subject to do its job. Each condition is one assertion the auditor can independently agree with, and each carries a `kind` naming the category of assertion (caller restriction, value freshness, atomic consistency, etc.; see the closed `ConditionKind` enum for the eight categories).

Conditions are generated before threat generation. This provides threat generation with concrete, bounded inputs to reason from rather than relying on open-ended adversarial reasoning. The reasoning chain is: **purpose+placement → conditions (what must hold for the purpose to be fulfilled) → threats (scenarios where assertions break) → invariants (code-enforced defenses against those threat scenarios, propagated by re-check across scope)**. Separating positive assertions from threats also allows the human auditor to agree with an assertion but disagree with the threat assessment, making disagreements diagnosable.

Each condition's `description` is phrased affirmatively: "the caller carries the privilege the subject's purpose presumes," "the value reflects the latest committed state," "no interleaving operation observes inconsistent state across this point." Failure-mode language ("could fail," "an attacker can…") is reserved for threats, which generate adversarial inversions of these assertions.

Example assertions per non-pure subject type. These are illustrative, not enumerative — the LLM produces conditions tailored to each subject's purpose+placement and may emit none from a list below if the purpose does not presume them, or several including some not listed:

**External calls.** For an external call site, conditions might assert:

- The callee is restricted to trusted addresses (kind: `AuthorizedAccess` or `RestrictedReachability`).
- The callee cannot re-enter the current contract before the surrounding state commits (kind: `AtomicConsistency`).
- The return value is honored before subsequent operations rely on success (kind: `ErrorRecoverability`).

For example, an external call to Uniswap's `createPair` whose token address comes from `Clones.clone()` typically emits a `RestrictedReachability` condition: "the token address is constrained to one that no other party can pre-compute." The threats step then generates the inversion: "the deterministic token address can be pre-computed and `createPair` called first, bricking the deployment."

**State mutations.** For a state write, conditions might assert:

- Only the authorized writer can reach this mutation (kind: `AuthorizedAccess`).
- The pre-write state matches the purpose's preconditions (kind: `InputIntegrity`).
- The mutation is recoverable or its invalid range is unreachable (kind: `ErrorRecoverability`).

**State reads of mutable variables.** For a state read, conditions might assert:

- The value reflects the latest committed state (kind: `ValueFreshness`).
- The read is not influenced by attacker-controlled timing or ordering (kind: `InputIntegrity`).
- The read order with respect to surrounding operations preserves consistency (kind: `AtomicConsistency`).

**Delegatecalls.** For a delegatecall, conditions might assert:

- The target address is restricted to trusted, audited code (kind: `AuthorizedAccess`).
- The delegated code's storage assumptions match the calling contract's layout (kind: `InputIntegrity`).

**Assembly blocks.** For an assembly block, conditions might assert:

- The compiler safety check the assembly bypasses is upheld by the surrounding code (kind: `InputIntegrity`).
- The block's memory and storage assumptions match the surrounding contract's layout (kind: `AtomicConsistency` or `InputIntegrity`).

#### Auditor verification of conditions

When an auditor approves a condition, the verification prompt asks three questions to convert approval from a frictionless click into a verified judgment:

- **Is this assertion load-bearing for the subject's purpose?** If the purpose would still be fulfilled without this assertion, the condition is trivia and should be removed or rewritten.
- **Would the purpose still hold if this assertion failed?** If yes, the condition is mislabeled — it is not actually a presumption the purpose makes.
- **Does the assertion anchor to topics visible in the subject's surrounding code?** Evidence topics should reference state vars, parameters, callees, or documentation topics that establish what the assertion is about. Empty `evidence_topics` is acceptable when the assertion is about an absence (e.g. "the caller is constrained to the contract's owner" with no positive code anchor for the constraint), but should be questioned.

The first two questions test whether the condition is doing the work it claims; the third tests evidence grounding. Conditions are independently approvable: an auditor can approve a condition while disagreeing with the threats that step 7 generates against it.

### Conditions vs. Invariants

The chain: **purpose → conditions → threats → invariants**. Conditions are not a redundant layer above invariants; the two serve different reasoning loops, and the textual content can overlap (a condition "the caller carries the privilege the subject's purpose presumes" and an invariant "every privileged-state-modifying function checks ownership" can read nearly identically). The role split is what differs.

**Conditions' unique upstream value over invariants:**

1. **Layer.** Conditions live at the purpose layer — they catalogue what each subject's purpose presumes about its environment. Invariants live at the threat layer — they catalogue what the codebase must enforce to block specific threat scenarios. Conditions answer "what does this subject's purpose presume?"; invariants answer "what must the codebase enforce to defend against this threat?"
2. **Coverage.** Conditions are uniform — every non-pure subject has them by construction during the initial generation pipeline. Invariants are sparse — they exist only where threat work has surfaced one. Conditions provide a complete map of purpose-level assumptions across the audit; invariants provide depth on the subset of subjects where threat analysis has been done. Skipping conditions means subjects whose purposes are misinterpreted but never threat-analyzed have no checkpoint.
3. **Threat bounding.** Conditions provide bounded inputs to threat generation. With conditions, threat generation prompts "find scenarios that falsify each assertion" — anchored, tractable. Without conditions, threat generation is open-ended adversarial reasoning ("find what could go wrong") — much weaker, harder to verify completeness, and harder to attribute disagreements.
4. **Correction surface.** When the auditor disagrees with a condition, that signal propagates back to questioning the purpose itself, before threat work begins. Without conditions, the only correction handle on a misinterpreted purpose is downstream threat disagreement — more expensive to surface, harder to attribute to root cause.
5. **Indexing.** Conditions are subject-local — every subject has its own list, addressable by the subject's topic. Invariants are subject-local at generation time too — each invariant attaches to exactly one subject, inherited from its parent threat, with cross-site application handled by duplicate-description invariants on each affected subject. Scope-organized propagation — finding every other subject in scope that lacks the expected defense — is the responsibility of the deferred cross-site propagation step (steps 11/12), not a property carried on the invariant at generation time; step 10 validates only the invariant's own subject.

**Surface similarity vs. structural difference.** A condition and an invariant can name the same code-level statement; that is the chain working, not redundancy. The condition is organized around one subject's purpose-presumptions; the invariant is organized around one threat's enforced defense, validated at its own subject by step 10 and propagated across other in-scope subjects by the deferred cross-site step. Both can exist for the same statement — the condition records why the subject's purpose needs the property; the invariant records that the codebase must enforce the property as a defense, with a parent threat naming what the defense protects against.

**Distinguishing test for prompts and review.** A condition prompt asks "what does this subject's purpose presume?" — never "what should the code enforce?" An invariant prompt asks "given this threat, what defensive property prevents it?" — naming the codebase-level enforcement need as an "X must Y" / "every Z does W" statement, with optional `anchors` citing the declarations the property is stated against; verification of whether the property holds at the invariant's own subject lands in step 10 (validation), and verification of where else in scope it must hold is the deferred cross-site step. Miswriting cues:

- If a condition is written as "the code must X," it has been miswritten as a small invariant — rewrite as "the purpose presumes X."
- If a condition names a specific failure scenario, it has been miswritten as a threat — rewrite as the assumption that scenario would violate.
- If an invariant is written as "the purpose presumes X," it has been miswritten as a condition — restate as "the codebase must enforce X to defend against [threat]."

Validation closes the loop: the codebase-level property the invariant states is mechanically checked at the validated subject, producing a verdict that the auditor can accept or contest. See "Validating Invariants" below for the verdict enum and the in-function evidence scope.

### Managing Threats and Invariants

Threats are generated on-demand for non-pure subjects only, after their conditions have been generated. When a non-pure subject is first evaluated during the audit, the LLM is given the subject, its conditions (the assertions that must hold for the subject's purpose to be fulfilled), its backward context, its feature description and requirements (as the adversarial context), the audit's security characteristics (the complete set of `Security`-kind `CharacteristicTopic` entries rendered as a single text block — see "Managing System Characteristics"), and its type constraints (to avoid restating those), and asked "given these assertions, what scenarios would falsify each one — and what implementation-specific risks does that create?" The conditions provide concrete inputs — "the token address is constrained to one that no other party can pre-compute" — from which the LLM derives specific threats by inverting each assertion ("the deterministic token address can be pre-computed and `createPair` called first, bricking deployment") rather than reasoning from scratch. Each generated threat carries a link back to the condition it falsifies, so an auditor disagreeing with a threat can trace the disagreement to the assertion the threat targets without invalidating that assertion. A condition can be the target of many threats; each threat names exactly one condition. The result is cached on the subject and presented to the auditor for review.

Each threat carries a structured **`controlled_by`** field that classifies the primary actor whose action drives the scenario, drawn from a closed eight-variant `ThreatActor` enum: `Caller` (an unauthenticated external caller of a public entry point), `PrivilegedRole` (a role-gated party such as an admin, owner, or operator — the specific role lives in the description, not in this variant), `External` (a third-party contract, typically the callee in an external call, an oracle, or a token the subject interacts with), `BlockProducer` (a miner, sequencer, or validator with control over transaction ordering or inclusion), `Counterparty` (a peer in the protocol's economic model whose interests differ from the subject's purpose), `Self_` (the contract itself reentering through an external call), `AnyParty` (no constraint on who triggers the scenario; permissionless), and `Other` (a genuinely novel actor classification, with the structure carried in the description). One primary actor per threat; multi-actor coordination scenarios are captured in the description prose. Threat descriptions themselves remain **actor-agnostic** — the prose names the scenario in passive or mechanism terms ("the deterministic token address can be pre-computed and `createPair` called first") rather than naming the party ("an attacker pre-computes..."). This keeps the actor classification a separately scrutinized artifact: an auditor can approve the description while disagreeing with the actor (or vice versa) without the prose forcing a paired interpretation.

Threat evidence is **scoped to the subject's containing function.** A threat's `evidence_topics` may reference only topics inside the subject's containing function: the subject node itself, descendants of the subject node, sibling statements in the same semantic block, the containing function, and the function's signature, modifiers, and parameters. Cross-function evidence is invalid on threats — that surface belongs to invariants, which point outside the subject to the codebase-level defenses. For "absence of X enables this threat" scenarios (no reentrancy guard, no slippage check, no access control modifier), the evidence points to the subject node showing the absence or to the function's modifier list — never to the missing element, which by definition is not in the codebase. The scope rule keeps the layer split clean: threats describe the vulnerable surface, invariants describe the defenses that protect it.

Threats are created without a mandatory link to a feature. This keeps discovery lightweight — the auditor or LLM records the implementation-specific risk immediately without needing to perform impact analysis on the spot. Cross-cutting vulnerabilities like storage collisions may affect multiple features in ways that aren't apparent during code review, and forcing the link at creation time would either block documentation, produce a hasty and potentially misleading link, or cause the threat to go unrecorded. The linkage to features, along with severity and the relationship type ("is vulnerable to" or "defends against"), is established during the dedicated impact analysis step.

When the threat-generation pass considers a condition and finds no plausible falsifying scenario — for example, an assertion enforced by Solidity itself, by an upstream type constraint, or by a structural property of the codebase — it records a `no_threat_rationale` rather than silently emitting nothing. The rationale is posted as an agent-authored comment on the condition's discussion thread (prefixed `[step-7 / no-threat]`), so the assertion's review carries an explicit audit signal that it was considered and discharged. Auditors can reply or contest in the same thread.

Invariants are pipeline-generated defensive properties stated against each threat. Each invariant is phrased as an "X must Y" or "every Z does W" property — what the codebase must enforce, not how to enforce it — and carries a closed `kind` drawn from the `InvariantKind` enum (grouping authorization & lifecycle gates, reentrancy & ordering, bounds & ranges, freshness & oracles, state conservation, input well-formedness, external call safety, cryptography & one-shot, bounded computation, semantic/economic identities, and a catch-all `Other`). Each invariant names exactly one parent threat; one threat can carry many invariants. If a single defensive property defends multiple threats, the pipeline emits duplicate-description invariants with different `threat_topic` links — the same pattern conditions and threats use for cross-cutting duplication, since text is cheap and attribution is expensive.

Invariants attach to a single subject: `subject_topic` is inherited at write time from the parent threat's subject. Singular by construction; cross-site application of the same defense is handled by duplicate-description invariants on each affected subject. `severity` is denormalized from the parent threat the same way (`Option<ThreatSeverity>`, `None` while threat severity is pending impact analysis); the copy is a write-time snapshot, not a live mirror, and goes stale if impact analysis later updates the parent threat's severity.

Invariants carry **no `evidence_topics`** — the parent threat is the evidence for the invariant's existence. Each invariant does carry an optional **`anchors`** field: a list of declaration topics (a `nonReentrant` modifier, a role state variable, a pause flag, a state field whose conservation is being asserted) the property is stated against. Anchors are untyped citations — there is no `AnchorRole` enum distinguishing "the modifier that enforces" from "the variable that the property is about"; validator-side ambiguity is resolved by producing an `Inconclusive` verdict rather than guessing at intent. The LLM may emit an empty list when no in-codebase declaration cleanly anchors the property, and the field exists primarily to seed validation's evidence search rather than to certify enforcement.

When the auditor evaluates a convergence, the invariants attached to its subject are immediately present alongside functional purpose, functional semantics, and type constraints — no indirection through abstract structures — and the parent-threat link carries the traceability chain back to the feature.

When step 9 considers a threat and finds no defendable property — for example, a threat already eliminated by an upstream type constraint, by Solidity itself, or by a structural property of the codebase — it records a `no_invariant_rationale` rather than silently emitting nothing. The rationale is posted as an agent-authored comment on the threat's discussion thread (prefixed `[step-8 / no-invariant]`), so the threat's review carries an explicit audit signal that it was considered. Auditors can reply or contest in the same thread.

Pure expressions can house semantic bugs — `propFactor + bal` where the operator should be `*` — but these are caught by functional semantics at specification convergences, not by threat generation. The adversarial reasoning that threats capture always bottlenecks through non-pure operations, because those are the points where the system interacts with the outside world. Pure expressions may be *incorrect*, but their incorrectness becomes *exploitable* only through the non-pure operations that act on their results. Threats on non-pure subjects have visibility into the pure expressions that feed into them through backward context, so the threats account for insufficiencies in upstream checks and computations.

### Validating Invariants

Validation (step 10) is the convergence-to-invariant link that earlier versions of this spec described as a deferred re-check step. For each invariant produced in step 9, the pipeline emits exactly one `ValidationTopic` carrying a verdict on whether the invariant's property actually holds in the code at the validated subject — the subject the invariant inherited from its parent threat. The validation is the codebase-side counterpart to the invariant's property statement: the invariant says what must be enforced; the validation says whether it is enforced here.

**Verdict enum.** Each validation carries one `ValidationVerdict` value from a closed four-variant enum:

- **`Enforced`** — the property holds at the subject. Evidence cites the modifier, role check, ordering arrangement, or state write that establishes it.
- **`Absent`** — the property does not hold at the subject. Evidence cites the subject itself as "where the enforcement should have been," or the function's modifier list to show the expected guard is missing. `Absent` is a finding, not a defect of the validation; the auditor's review surface is where it gets graded.
- **`Partial`** — the property holds in some code paths through the subject but not all (e.g., one branch of an if/else carries the check; the other does not). Evidence cites the branch where the property holds and the branch where it does not.
- **`Inconclusive`** — the validator cannot judge enforcement from the function's surface. This is a first-class verdict, not a fallback: `EconomicInvariant` and `Other`-kind invariants typically validate as `Inconclusive` because no v1 harness exists for them; invariants whose anchors point at declarations not visible in the validated function's surface also fall here. The rationale must name why the verdict is inconclusive so the auditor can decide whether the gap is the validator's or the codebase's.

**Evidence scope.** A validation's `evidence_topics` may reference only topics inside the subject's containing function — the same scope rule threats use for their evidence. Permitted topics are the subject node itself, descendants of the subject node, sibling statements in the same semantic block, the containing function, and the function's signature, modifiers, parameters, and local variables. State variables, role identifiers, and other declarations outside the function are reached via the parent invariant's `anchors`, not via evidence; validation evidence answers "where in this function is the property checked (or not)?", not "which declaration is the property about?" Cross-function evidence is invalid — that surface belongs to cross-site propagation (step 11/12), which validates one invariant at every subject in scope where the property must hold.

**Inherited fields.** Each validation names exactly one parent `invariant_topic`; the addressability is the invariant, not the subject (the subject is reachable through the invariant's `subject_topic`). Severity is not denormalized onto the validation — it is reached through `validation.invariant_topic → invariant.threat_topic → threat.severity` on every lookup, since the validation itself is a verdict on enforcement rather than an independent risk artifact.

**No-validation cases.** Step 10 emits a validation for every invariant the step considers. Invariants whose parent threat has been dropped (the LLM emitted an `invariant_topic` for a threat that no longer exists by validation time) are skipped before the LLM call, with a dropped-unknown-parent count recorded in the step's report. There is no `no_validation_rationale` mirror of step 8/9's no-rationale comment thread: validation always produces a verdict, with `Inconclusive` covering the "cannot judge" case in-band.

Cross-site propagation — taking one invariant and emitting validations at every other subject in scope where the property must hold — is a **deferred later pipeline step** (step 11 entry-boundary absence check; step 12 cross-site pattern analysis). Step 10 produces the per-subject baseline that those later steps will fan out from. On-the-fly validation when an auditor adds or corrects an invariant post-pipeline is also deferred; the generator is structured so a single-invariant caller can reuse it once the call site is wired.

### Threat Traceability

When an invariant is contradicted at a convergence, the traceability chain identifies the security impact. For threats with completed impact analysis: convergence → invariant → threat → feature → documentation. For threats without a feature link: the contradiction is flagged but severity is pending impact analysis.

The convergence-to-invariant link is surfaced by validation (step 10), which propagates each invariant to its subject and emits a verdict on enforcement. Cross-codebase propagation to other in-scope subjects is the deferred next step (steps 11/12); until that lands, an invariant's traceability surface beyond its own subject is the parent-threat chain back to the feature, and the source-of-enforcement linkage that earlier denormalizations approximated lives on the validation artifact at the invariant's subject rather than on the invariant itself.

This traceability enables prioritization: when the auditor is evaluating a convergence, the severity of the threats behind its invariants determines how much scrutiny the convergence warrants. Invariants from unlinked threats are flagged as needing impact assessment, ensuring they are not deprioritized by default but are also not assigned an arbitrary severity.

## Property Approval

Every property in the security model — features, requirements, behaviors, system characteristics, functional semantics, functional purpose, placement rationale, conditions, threats, invariants — is subject to approval. Approval is the mechanism by which a property transitions from unverified to verified, and the mechanism by which one party in the audit (human auditor or AI agent) records agreement with another party's property.

**Approval is bidirectional.** Human auditors approve AI-generated properties; AI agents approve human-authored properties. Both directions matter because the audit is a collaboration: an AI-generated property that no human has approved has not yet earned its place in downstream reasoning, and a human-authored property that no AI agent has reviewed has not been checked for consistency against the rest of the model. Approval makes agreement explicit so downstream analysis can rely on it.

**Approval requires a comment.** A property cannot be approved silently. The approver must record the reasoning that led to their agreement, attached as a comment on the property. This converts the act of approval from a frictionless click into an act that produces signal, capturing the rationale that would otherwise stay implicit. Required-comment approval directly counters the anchoring bias that affects review of well-written generated answers — when approval requires articulating *why* the property holds, the approver must engage with the property's substance rather than its surface plausibility.

**The comment is the unit of disagreement, too.** When an approver disagrees with a property, the same comment surface captures the correction and its reasoning. A correction is an approval of a different property than the one originally generated; the original property is replaced and the comment records why. This means the approval and correction workflows share the same UI affordance, and the comment thread on a property naturally captures the full history of its reasoning.

**Audit trail value compounds over time.** When a property is later contradicted at a convergence, the approval comments on the property and its inputs become evidence of the prior interpretation that turned out to be wrong — useful audit data that diagnoses the disagreement chain. When a property is corrected, the comment records the reason the original was insufficient, which helps subsequent generations of the same kind of property avoid the same mistake.

The approval mechanism is implemented by the collaborator module (see `crates/o11a-server`). Properties carry an approval state and a list of approval-comment topics; both are surfaced wherever the property is displayed. The agent infrastructure participates in approval through the same comment surface as human auditors, so approvals from agents and humans are visible on the same property and can reinforce or contradict each other.

## Auditing Convergences

Type constraint checks can be checked by a constraint algorithm. Specification convergence checks are annotated in regular language with business logic and can only be checked by something that understands the specific business logic semantics.

General audit flow is:

1. Read and understand the docs and the purpose of the project
2. Run the initial generation pipeline (ten steps): semantic linking, requirement extraction, behavior extraction, feature synthesis via reconciliation, characteristic synthesis, functional purpose and placement generation on every non-pure subject in an in-scope function (with adversarial critique attached), condition generation, threat generation against each condition, invariant generation against each threat, and invariant validation producing per-invariant verdicts (`Enforced` / `Absent` / `Partial` / `Inconclusive`)
3. Review and refine the generated security model — verify functional semantics, add missing requirements, correct behavior descriptions, adjust feature groupings, confirm or correct the synthesized security characteristics, confirm or correct generated purpose and placement rationale
4. Step through all convergences — invariants from threats are attached and checked at convergences; conditions and threats are re-examined as needed
5. Perform impact analysis — link threats to the features they affect, establishing severity and the relationship type ("is vulnerable to" or "defends against"); unlinked threats are flagged for review
6. Re-reconcile requirements against behaviors per feature as needed, identifying unimplemented specification and undocumented implementation
7. As new requirements, behaviors, characteristics, conditions, threats, invariants, functional semantics, functional purpose, or placement rationale are discovered during code review, add them to the security model — the re-check system propagates them to all relevant subjects, ensuring nothing is missed

### Managing Convergences

Convergences are the main point of verification in the audit process. They have four states: waiting, unverified, verified, and contradicted. A waiting convergence is one that is waiting for properties to be added to its parts, i.e. `a + b`, but `a` does not have any properties added so we cannot judge the convergence.

## Subject Evaluation Context Strategy

When evaluating subjects for invariant upholding or invariant recognition, we use **backward-only context** as the default strategy. When auditing a given subject, the system provides context about where the subject's values originated (their data provenance, taint sources, and upstream transformations), the subject's functional purpose (why it exists, derived from the feature), the subject's functional semantics (what it represents, derived from project documentation via semantic linking, with provenance to the source documentation topic), its invariants (defensive properties it must uphold), and for non-pure subjects, its conditions (assertions about its interaction surface that its purpose presumes) and threats (adversarial inversions of those assertions). The system does not, by default, include forward context about where those values propagate to downstream.

This is a deliberate architectural choice, not a limitation. The system compensates for the absence of forward context through the **security model's on-the-fly generation and re-check mechanism**, which provides equivalent or superior coverage to bidirectional context while maintaining focused, low-noise audit passes.

### Why Not Bidirectional Context

An intuitive approach to auditing a variable is to present both where its value came from and where it goes. This gives a complete picture of the variable's role in the system. The case for this is strongest when a single value fans out to multiple destinations with different requirements — seeing the full fan-out from one vantage point can surface design-level issues like incomplete access control coverage or inconsistent invariant enforcement across functions.

However, this advantage is narrower than it first appears, for two reasons.

**The inferential demand is the same either way.** Forward context presents the auditor with a list of destinations and relies on the auditor to recognize that a pattern (such as an access control check) is present at some sites and missing at others. Backward-only context presents the auditor with the pattern at the first site encountered and relies on the auditor to generalize it into an invariant. In both cases, the auditor must perform the same act of recognition and generalization. Forward context does not reduce the cognitive work; it merely rearranges when it happens.

**Forward context introduces noise that degrades audit quality.** Forward propagation scales with the number of consumers of a state variable. In smart contracts, heavily-read state variables (token balances, approval mappings, configuration parameters) may be referenced across many functions. Including all downstream consumers in the context of every audit pass dilutes the signal about the specific code under review. For LLM auditors in particular, this context dilution measurably degrades reasoning about the subject at hand. Backward-only context keeps each audit pass lean and focused on the immediate code being evaluated.

### How the Security Model Closes the Gap

The primary class of vulnerability where forward context provides value is **incomplete invariant enforcement**: cases where a check or pattern should be applied uniformly across a set of functions but is missing from one or more of them. The canonical smart contract examples are access control gaps (a modifier guards some privileged functions but not all) and economic invariant violations (some functions that modify a balance correctly maintain a sum-total relationship while others do not).

Rather than using forward context to surface these issues passively, this system surfaces them actively through the security model's on-the-fly generation:

1. **Invariant discovery.** When the auditor encounters a security-relevant pattern during a backward-context audit pass (e.g., a role-based access check on a state-modifying function), they register it as a threat on the non-pure subject with an associated invariant. The incremental update pipeline propagates the invariant to all subjects within its scope via re-checks.

2. **Propagation via re-check.** The system applies the new invariant to all previously-audited subjects within its scope. Any function that modifies the same state but lacks the expected check is flagged mechanically, without requiring forward context at the point of origin.

3. **Completeness guarantee.** Because the audit is programmatic and covers every variable and statement, the re-check mechanism provides exhaustive coverage. Once an invariant is articulated, no violation can escape detection. This is stronger than forward context, which still depends on an auditor noticing a discrepancy within a potentially large list of destinations.

This architecture cleanly separates two concerns that bidirectional context conflates: understanding what a specific piece of code does (served by focused backward context) and verifying that invariants hold globally (served by the security model's re-check system). Each concern is handled by a mechanism optimized for it.

### Known Limitations and Mitigations

The backward-only strategy with invariant re-checking has one residual limitation: invariant discovery depends on the auditor encountering at least one instance of a pattern and recognizing its security significance. Two edge cases deserve attention.

**Subtle semantic distinctions.** Some invariants require recognizing that two superficially similar operations are categorically different. For example, a function that writes to `balances[msg.sender]` (self-modification) versus one that writes to `balances[account]` (arbitrary-address modification) may both appear as simple mapping writes, but only the latter requires elevated privilege. Forward context does not inherently make this distinction more visible — the auditor must apply domain knowledge in either case — but the system should include heuristic prompts that direct attention to these distinctions. Examples include flagging functions that modify state keyed by a caller-supplied address, or functions that bypass standard entry points.

**Absent mechanisms.** If a contract should implement a security pattern (such as a pause mechanism) but does not, no backward-context encounter will contain a reference to the missing pattern. This class of finding is addressed by reconciliation: if the documentation describes pausability, a requirement will exist for it, and the absence of a matching behavior surfaces the gap. Additionally, when the auditor reviews the feature's requirements and finds no corresponding code, the missing mechanism is identified as unimplemented specification.

Patterns could also be catalouged from the perspective of all Behaviors, relieving the need for the context mangagement strategy to reveal them as a side effect of evaluating convergences.

### Summary

The backward-only context strategy is not a cost-saving approximation of bidirectional analysis. It is a design that optimizes for audit quality by keeping individual audit passes focused, while relying on the security model to provide the global coverage that forward context would otherwise supply. The security model achieves this coverage more reliably — mechanically and exhaustively — once an invariant is identified, and the investment in invariant discovery heuristics and threat generation compounds across engagements in a way that raw forward context never does.
