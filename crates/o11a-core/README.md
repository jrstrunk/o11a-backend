# o11a-core

The core processing pipeline for an o11a audit. It turns raw source and documentation into a structured, queryable model of what the project is, what it claims to do, and where its risks live. Three stages cooperate to build that model: parser, analyzer, and checker.

# Parser

The parser reads source and documentation files in an audit and produces enhanced AST representations for the rest of the application to use.

It does not just pass through whatever the underlying language tools produce. It enriches each AST with the information the rest of the pipeline needs, so downstream stages can work from the parser's output alone without having to go back to the original source files. This is essential when the system needs to format or analyze a single node in isolation.

## Solidity Source Parsing

Solidity source is read through the Foundry compiler ASTs. The parser augments those ASTs with semantic blocks — groupings of statements based on whitespace in the original source — and attaches the developer's documentation comments to the block they describe. This preserves the structural and documentary cues a human reader relies on, which the raw compiler AST throws away.

## Documentation Source Parsing

Documentation files (Markdown today; potentially Plain Text and Djot later) are parsed into ASTs in-process. Sections and paragraphs become declarations, and inline code snippets become references that can resolve to the same identifiers used in the source code. Documentation is parsed last so that its references can be resolved against the full code context.

# Analyzer

The analyzer traverses the parsed ASTs and builds a structured directory of everything that is reference-able in the audit: declarations, references, scopes, and the extended properties of functions and modifiers.

It exists because raw ASTs are not a good substrate for an audit. Auditors do not work file-by-file; they jump between definitions, references, and call sites. The analyzer materializes those relationships up front so the rest of the system — and the auditor — can navigate by topic rather than by file offset.

The analyzer also enforces audit scope. Projects under audit pull in many dependencies but only use a handful of functions from them. The analyzer keeps the model focused on in-scope code and the dependency code reachable from it, so the audit is not drowned in irrelevant declarations.

Declarations are scoped by container, component, member, and semantic block, so any identifier or operation can be linked precisely. For Solidity that is file → contract → function → block; for documentation it is file → top-level header → second-level header → nested headers. Comments inherit the scope of the topic they attach to.

# Checker

The checker is responsible for two things: detecting contradictions where independent properties about the same value meet (convergences), and managing the security model that defines which properties should hold in the first place.

A convergence with contradicting properties is a likely place for a vulnerability or implementation flaw. The security model is what gives the checker something to check against — it captures the project's claimed behavior, its actual behavior, and the risks that follow from the gap between the two.

## Security Model

The security model is the structured representation of what the documentation claims, what the code actually does, what could go wrong, and which source locations are relevant to each concern. It is built incrementally throughout the audit — seeded from project documentation and refined on-the-fly as auditors review code and discover new concerns.

### Design Principles

**Documentation is untrusted.** Developer docs represent claimed behavior, not verified truth. The system treats them as input to reason about, not as a source of correctness.

**Features are synthesized from reconciliation.** Features are not created upfront — they emerge from reconciling documentation-derived requirements with code-derived behaviors. Features therefore carry both the documented intent and the implementation reality, providing richer context for downstream analysis than features derived from documentation alone.

**Requirements capture all documented claims.** Requirements are what the documentation says the system does. They cover any documented behavior — functionality, constraints, access control, edge cases.

**Behaviors capture what the code actually does.** Behaviors are extracted from source code and represent the real implementation. They are described in business terms rather than mechanical ones, so they line up with requirements during reconciliation.

**Functional semantics are the bridge between documentation and code.** Functional semantics link documentation concepts to code declarations — they record what each declaration represents in the context of the project. They are established before behavior extraction so that behaviors can be described with the same vocabulary as requirements.

**Conditions are the structured analysis of a non-pure subject's interaction surface.** Each non-pure subject (state read/write, external call, delegatecall, assembly block, etc.) has conditions determined by its type. Conditions are evaluated with standardized questions, providing concrete inputs for adversarial reasoning rather than relying on open-ended threat identification.

**Threats live on non-pure source code subjects.** Threats capture implementation-specific risks derived from condition evaluations — concerns like "frontrunning via pre-computed token address bricks deployment." Pure operations (arithmetic, comparisons, local assignments) are excluded because their threat surface is fully covered by type convergences.

**Invariants live on source code subjects.** Invariants are the defensive properties the code must uphold to protect against threats. They are attached directly to the subjects where they are checked at convergences, with no indirection through abstract structures.

### Initial Generation and On-the-Fly Generation

The security model is seeded by an automated pipeline that runs against in-scope contracts: semantic linking, requirement extraction, behavior extraction, and feature synthesis via reconciliation. Out-of-scope code and library dependencies are assumed correct, so the full pipeline does not run against them, but they still receive lighter-weight semantics and behaviors so in-scope analysis has the context it needs.

After initial generation, the model is not static. As auditors review code, they add new requirements, behaviors, conditions, threats, invariants, and functional semantics directly from the code context. Background re-checks propagate each new element to all relevant subjects, so a single observation on one function ripples into every other subject the new property touches. This is how the system achieves comprehensive coverage without forcing the auditor to keep a global picture in their head.

### Reconciliation and Impact Analysis

Reconciliation is both the final step of initial generation (where features are first synthesized from requirements and behaviors) and an ongoing audit activity. As an activity, it surfaces requirements without matching behaviors (unimplemented specification) and behaviors without matching requirements (undocumented implementation). Both are findings.

Impact analysis is the dedicated step where threats are linked to the features they affect, with a relationship type ("is vulnerable to" or "defends against") and a severity. Threats are recorded immediately during code review without forcing a feature link on the spot — the link is established later with the right context. Threats that cannot be linked to any feature signal either an incomplete feature set or a finding that needs to be reconsidered.

## Convergences

Two types of convergences are checked across the audit:

1. **Type convergences** — where two values interact via an operator (arguments to parameters, assignment, unary/binary/ternary operators). These are purely logical and can be checked algorithmically against properties such as bounds, value sets, trust, and length.
2. **Specification convergences** — where many pieces of code implement a behavioral feature. These check that observed behaviors, defensive invariants, functional purpose, and functional semantics are consistent and correctly implemented. Specification convergences are project-specific and cannot be checked by an algorithm; they require an evaluator that understands the project's business logic.

A convergence has four states: waiting (parts have no properties yet), unverified, verified, and contradicted. A contradiction is the signal that something the model says should hold does not hold — the most important thing the checker produces.

## Subject Evaluation Context Strategy

When the checker evaluates a subject, it provides **backward-only context** by default: where the subject's values came from, the subject's purpose and semantics, its invariants, and (for non-pure subjects) its conditions and threats. Forward context — where the subject's values propagate to — is deliberately omitted.

This is a design choice, not a limitation. Forward context scales with the number of consumers of a value, and in smart contracts heavily-read state can fan out across many functions. Including all of that downstream context in every audit pass dilutes the signal about the code under review, which measurably degrades the quality of LLM reasoning and adds noise for human auditors.

The class of vulnerability where forward context is most valuable — incomplete invariant enforcement, where a check is applied at some sites but not others — is handled instead by the security model's re-check mechanism. When an auditor encounters a pattern at one site and registers it as an invariant, the system mechanically applies that invariant to every other in-scope subject and flags the ones where it does not hold. This is more reliable than forward context, which still depends on the auditor noticing a discrepancy in a long list of destinations.

The result is a clean separation: focused backward context for understanding what a specific piece of code does, and the security model's re-check system for verifying that invariants hold globally.
