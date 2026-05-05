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

### Initial Generation

The security model is initially seeded from project documentation and source code through an automated pipeline.

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

### Out-of-Scope and Dependency Code

The initial generation pipeline — semantic linking, requirement extraction, behavior extraction, and feature synthesis — runs only against **in-scope contracts**: the contracts listed in the audit's `scope.txt` file. Out-of-scope contracts and library dependencies (OpenZeppelin, Solmate, etc.) are excluded from the pipeline even though they are present in the analyzed codebase.

This distinction is fundamental. In-scope code is the code under review — it cannot be assumed to be correct, which is why it needs requirements extracted from documentation, behaviors reconciled against those requirements, threats identified on its non-pure subjects, and invariants checked at its convergences. The full security model exists to surface mismatches between what the documentation claims and what the code does.

Out-of-scope code and dependencies are assumed to be correct. They are battle-tested libraries or previously audited contracts that the in-scope code builds on. Running the full pipeline against them would produce requirements like "tokens must transfer correctly" against OpenZeppelin's `SafeERC20` — requirements that are not the concern of this audit and would dilute the auditor's focus.

However, out-of-scope code still needs **functional semantics** and **behaviors** for the in-scope analysis to work. When in-scope code calls `SafeERC20.safeTransfer()`, the auditor needs to know what that function does. When a dependency's state variable is read by in-scope code, the auditor needs its semantic meaning. This context is necessary for understanding the in-scope code's behavior, even though the dependency itself is not being audited.

Out-of-scope code therefore requires a separate, lighter mechanism: generating semantics and behaviors without requirements, features, threats, or reconciliation. This mechanism documents what the dependency does (semantics and behaviors) so that in-scope analysis has the context it needs, without generating the security model artifacts that only apply to code under review. This separate mechanism is not yet implemented.

### On-the-Fly Generation

The security model is not static after initial generation. As auditors review code and encounter security-relevant patterns, they add new requirements, behaviors, conditions, threats, invariants, and functional semantics directly from the code context. This on-the-fly generation is the primary mechanism through which the audit achieves comprehensive coverage.

When a user adds a new element to the security model, the relevant pipeline steps run automatically in the background:

- **New requirement** — Added under the relevant documentation section or feature. Requirements do not trigger threat generation; they are documentation claims that will be verified during reconciliation against behaviors.
- **New behavior** — Created during code review, grouped under the code scope where it was observed. If features have already been synthesized, the behavior is associated with the appropriate feature based on its code scope. Behaviors are reconciliation artifacts and do not carry threat or invariant links.
- **New functional semantic** — Persisted on a declaration with provenance to its source documentation topic. If the semantic changes the meaning of a declaration, downstream properties (behaviors that reference the declaration, functional purpose on containing statements) may need re-evaluation.
- **New condition** — Evaluated on a non-pure subject with standardized questions. Condition evaluations trigger re-evaluation of the subject's threats, as new condition answers may reveal risks not previously identified.
- **New threat** — Generated on non-pure subjects from condition evaluations (see Managing Threats and Invariants below). Created without a mandatory link to a feature, allowing the discoverer to record it immediately. Invariants are generated from the threat and attached to the subject. The system triggers re-checks on the subject. The feature linkage is established during impact analysis.
- **New invariant** — Attached to a source subject, linked to its parent threat. The system triggers re-checks against the subject and any related subjects within the invariant's scope.

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
  ├── Condition: Revert "PAIR_EXISTS" on createPair
  │     ├── Can an attacker trigger? Yes — token address is deterministic (CREATE opcode)
  │     ├── Can normal operation trigger? No
  │     └── Is it recoverable? No — createPair has no overwrite mechanism
  ├── Condition: Reentrancy via token callback
  │     ├── Does the call transfer control to untrusted code? Yes — token is user-supplied
  │     └── Can the callee re-enter? Yes — no reentrancy guard
  ├── Threat: "Frontrunning via pre-computed token address bricks deployment"
  │     └── Impact: [is vulnerable to] "Protocol Campaigns" (severity: critical)
  ├── Threat: "Reentrancy via token callback during campaign setup"
  │     └── Impact: [is vulnerable to] "Protocol Campaigns" (severity: critical)
  ├── Invariant: "State updates precede external calls in deployCampaign"
  │     └── Parent Threat: "Reentrancy via token callback..."
  └── Invariant: "Token address must not be predictable or pair creation must be atomic"
        └── Parent Threat: "Frontrunning via pre-computed token address..."
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
  2. Conditions (structured evaluations of the subject's interaction surface, determined by subject type)
  3. Threats (implementation-specific risks derived from condition evaluations)
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

The functional purpose is the business logic reason for a statement — the "why" that statement is there, or more precisely, "why did the developer write this statement to do what it does?" An access control guard's functional purpose is not "checks that msg.sender equals owner" (that's what it does), but "to prevent other users from taking other users' funds" (that's why it exists).

Functional purpose is derived from the feature the subject belongs to combined with understanding of the code, and in a single-agent context it could be considered redundant — an auditor who knows the feature and reads the code can infer the purpose implicitly. The reason it exists as an explicit, stored property is to **externalize an intermediate reasoning step that would otherwise be opaque at collaboration boundaries**.

When reasoning passes between agents — an LLM doing an initial pass and a human following behind, or two human auditors reviewing the same code — implicit reasoning is lost at each handoff. If the LLM implicitly reasons "this access check prevents unauthorized fund access" and generates threats based on that understanding, but the human auditor thinks it enforces a delegation model, they disagree on threat coverage without knowing they disagree on the premise. That disagreement surfaces late, at the threat level, where it's harder to diagnose and resolve. Making the "why" explicit creates a checkpoint where reasoning can be inspected, corrected, and agreed upon before downstream analysis builds on it. The human sees the LLM's stated purpose, corrects it if wrong, and now threat generation and behavior documentation operate from a shared, verified premise.

This applies in both directions. When the human states a purpose and the LLM generates threats, the LLM has an explicit anchor to reason from rather than inferring intent from a behavior description that could be interpreted multiple ways. When the LLM states a purpose and the human reviews, the human gets a window into the LLM's understanding that they can verify independently of whether the downstream outputs look reasonable.

Functional purpose is generated on-demand: when a subject is first evaluated, the LLM is given the subject, its feature context (once features are synthesized), the feature's documentation and requirements, and the subject's functional semantics, and asked "why does this exist?" The result is cached on the subject and presented to the auditor for review. The auditor can correct, enrich, or confirm it before downstream analysis proceeds.

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

Each non-pure subject has conditions determined by its type. Conditions are the concrete, enumerable aspects of a subject's interaction surface that must be evaluated. The analyzer identifies conditions from data it already tracks — revert conditions from function signatures, value domains from type constraints, access patterns from require statements. Each condition is evaluated with standardized questions specific to the subject type, and the answers are stored on the subject and shared between evaluators.

Conditions are evaluated before threat generation. This provides threat generation with concrete, bounded inputs to reason from rather than relying on open-ended adversarial reasoning. The reasoning chain is: structured observations about each condition → threats derived from those observations → invariants derived from those threats. Separating observations from threats also allows the human auditor to agree with an observation but disagree with the threat assessment, making disagreements diagnosable.

Standardized questions by condition type:

**Revert conditions on function calls.** For each revert condition in the callee's signature:

- Can an attacker trigger this condition?
- Can normal system operation trigger this condition?
- Is this condition recoverable if triggered?

For example, Uniswap's `createPair` has a revert condition "PAIR_EXISTS." When evaluating an external call to `createPair` where the token address is produced by `Clones.clone()` (which uses the deterministic CREATE opcode), the evaluation yields: an attacker *can* trigger PAIR_EXISTS by pre-computing the token address and calling `createPair` first; this condition is *not* recoverable because `createPair` has no mechanism to overwrite an existing pair. These observations directly produce the threat "frontrunning via pre-computed token address bricks deployment."

**State mutations.** For each state write:

- Can the value be set to something that makes other functionality fail?
- Can the value be set by an unauthorized party?
- Is the mutation reversible?

**State reads of mutable variables.** For each state read:

- Can the value be manipulated before this read?
- Can the value be stale?
- Does the caller control what value is read (via timing, ordering)?

**External calls (beyond revert conditions).** For each external call:

- Does the call transfer control to untrusted code?
- Can the callee re-enter the current contract?
- Is the return value checked?

**Delegatecalls.** For each delegatecall:

- Can the target address be manipulated?
- Does the delegated code make assumptions about storage layout?

**Assembly blocks.** For each assembly block:

- Does the assembly bypass compiler safety checks that the surrounding code relies on?
- Does the assembly make assumptions about memory or storage layout?

### Managing Threats and Invariants

Threats are generated on-demand for non-pure subjects only, after their conditions have been evaluated. When a non-pure subject is first evaluated during the audit, the LLM is given the subject, its condition evaluations, its backward context, its feature description and requirements (as the adversarial context), and its type constraints (to avoid restating those), and asked "given these condition evaluations, what implementation-specific risks exist at this code point?" The condition evaluations provide concrete inputs — "an attacker can trigger PAIR_EXISTS and it is not recoverable" — from which the LLM derives specific threats rather than reasoning from scratch. The result is cached on the subject and presented to the auditor for review.

Threats are created without a mandatory link to a feature. This keeps discovery lightweight — the auditor or LLM records the implementation-specific risk immediately without needing to perform impact analysis on the spot. Cross-cutting vulnerabilities like storage collisions may affect multiple features in ways that aren't apparent during code review, and forcing the link at creation time would either block documentation, produce a hasty and potentially misleading link, or cause the threat to go unrecorded. The linkage to features, along with severity and the relationship type ("is vulnerable to" or "defends against"), is established during the dedicated impact analysis step.

Invariants are generated from threats and attached directly to the source subjects they protect. When the auditor evaluates a convergence, the invariants are immediately present alongside functional purpose, functional semantics, and type constraints — no indirection through abstract structures. Each invariant links back to its parent threat for traceability.

Pure expressions can house semantic bugs — `propFactor + bal` where the operator should be `*` — but these are caught by functional semantics at specification convergences, not by threat generation. The adversarial reasoning that threats capture always bottlenecks through non-pure operations, because those are the points where the system interacts with the outside world. Pure expressions may be *incorrect*, but their incorrectness becomes *exploitable* only through the non-pure operations that act on their results. Threats on non-pure subjects have visibility into the pure expressions that feed into them through backward context, so the threats account for insufficiencies in upstream checks and computations.

### Threat Traceability

When an invariant is contradicted at a convergence, the traceability chain identifies the security impact. For threats with completed impact analysis: convergence → invariant → threat → feature → documentation. For threats without a feature link: the contradiction is flagged but severity is pending impact analysis.

This traceability enables prioritization: when the auditor is evaluating a convergence, the severity of the threats behind its invariants determines how much scrutiny the convergence warrants. Invariants from unlinked threats are flagged as needing impact assessment, ensuring they are not deprioritized by default but are also not assigned an arbitrary severity.

## Auditing Convergences

Type constraint checks can be checked by a constraint algorithm. Specification convergence checks are annotated in regular language with business logic and can only be checked by something that understands the specific business logic semantics.

General audit flow is:

1. Read and understand the docs and the purpose of the project
2. Run the initial generation pipeline: extract requirements from documentation, generate functional semantics via semantic linking, extract behaviors from source code with semantics in context, and synthesize features via reconciliation
3. Review and refine the generated security model — verify functional semantics, add missing requirements, correct behavior descriptions, adjust feature groupings
4. Step through all convergences — for each subject, functional purpose is generated on-demand; for non-pure subjects, conditions are evaluated with standardized questions, then threats are generated from the condition evaluations, and invariants from threats are attached and checked at convergences
5. Perform impact analysis — link threats to the features they affect, establishing severity and the relationship type ("is vulnerable to" or "defends against"); unlinked threats are flagged for review
6. Re-reconcile requirements against behaviors per feature as needed, identifying unimplemented specification and undocumented implementation
7. As new requirements, behaviors, conditions, threats, invariants, or functional semantics are discovered during code review, add them to the security model — the re-check system propagates them to all relevant subjects, ensuring nothing is missed

### Managing Convergences

Convergences are the main point of verification in the audit process. They have four states: waiting, unverified, verified, and contradicted. A waiting convergence is one that is waiting for properties to be added to its parts, i.e. `a + b`, but `a` does not have any properties added so we cannot judge the convergence.

## Subject Evaluation Context Strategy

When evaluating subjects for invariant upholding or invariant recognition, we use **backward-only context** as the default strategy. When auditing a given subject, the system provides context about where the subject's values originated (their data provenance, taint sources, and upstream transformations), the subject's functional purpose (why it exists, derived from the feature), the subject's functional semantics (what it represents, derived from project documentation via semantic linking, with provenance to the source documentation topic), its invariants (defensive properties it must uphold), and for non-pure subjects, its condition evaluations (structured analysis of the subject's interaction surface) and threats (implementation-specific risks derived from condition evaluations). The system does not, by default, include forward context about where those values propagate to downstream.

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
