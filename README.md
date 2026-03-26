# o11a-backend
The o11a backend has five modules that form a processing pipeline:

 1. Parser - parses the audit source and allows clients to post new text blobs (added comments and docs) to be parsed in the context of the audit
 2. Analyzer - analyzes the audit source, producing a directory of definitions and their attributes, allowing clients to fetch structured audit data
 3. Formatter - transforms the AST into formatter nodes
 4. Collaborator - stores discussion topics and the comments under them, allowing clients to post new comments and approve or disapprove of comments from other users
 5. Checker - checks variable constraints, manages the security model of features, requirements, behaviors, threats, and invariants, and allows clients to post new constraints to be checked, providing data on any conflicting constraints

# Parser

The parser is responsible for reading source files in an audit and producing AST representations of them for the rest of the application to use.

The parser provides extra information not found in the original ASTs it parses in various ways, depending on the type of file. This allows the formatter to work with only the modified AST provided by the parser, not needing to reference the original source files. This is paramount when needing to format a single node in an isolated way.

## Solidity Source Parsing

Solidity source files are parsed and compiled by the Foundry compilers, which can output an AST file in JSON format that the parser can read and work with.

The parser enhances original Solidity ASTs by adding semantic blocks, which are a grouping of statements based on whitespace in the original source file. Semantic blocks are not in the original AST but are added by the parser by analyzing the source files for consecutive newlines between statements in a block. This creates a structure where Block nodes contain SemanticBlock nodes, which contain statement nodes. Each semantic block has optional documentation from comments that appear at the beginning of the semantic block in the source, preserving semantic context and documentation comments from the developers.

## Documentation Source Parsing

Documentation (Markdown, but maybe later Plain Text and Djot) source files are compiled by a markdown rust crate that produces an AST directly within the application. Documentation files are looked for in the `src` and `docs` directories of the audit project recursively.

In Documentation files, sections and paragraphs become declarations and inline code snippets become references. These may contain references by name to the same variables and functions as the implementation. Because of this, Text files are parsed last, so that they can be parsed with the full context of the source code, and can resolve any references to the code by searching for a declaration that contains the same name.

# Analyzer
The analyzer is responsible for traversing the Abstract Syntax Tree and performing static analysis on it.

The AST we are given to analyze contains unique IDs for each node, and each time an identifier appears in the AST, its node contains a reference_id property that points to the ID of the node that declared it.

Each audit will contain a scope.txt file in the root project directory. This file contains paths to all files that are in scope of the audit, and will allow us to focus on in-scope contracts, while also providing differentiated support for non-in-scope contracts.

Projects being audited can pull in lots of dependencies, yet only use a few functions from them. Naively processing all contracts/functions from every file in the project leads to excessive processing and bloating in the audit. To avoid this, we can take the following two-pass approach to make sure only in-scope and used by in-scope contracts/functions are processed:
 1. First read and parse each AST, storing its declarations in an accumulating simple declaration dictionary that stores all declarations in the audit by node ID. With the function/modifier declarations, store a list of the other nodes referenced in its body, the require and revert statements, the function calls, and the variable mutations, making note of whether the function/modifier at hand is from an in-scope ast or not. This is the first pass, and is a great place to do processing that needs to check all child nodes recursively, but without knowledge of the nodes it may reference.
 2. Loop over all the declarations, storing the publicly (public or external; NOT internal, private, or local) in-scope declarations in a new dictionary that stores all publicly in-scope declarations in the audit and the nodes that reference them. When a declaration is found to be publicly in-scope, add it to the in-scope declaration dictionary and look up its referenced nodes in the previously generated dictionary. Add each of these references to the accumulating in-scope dictionary with the node at hand that referenced it, then recursively check these references for their references, adding them as needed and the node that referenced them and so on.
 3. Now with a dictionary of all in-scope and used by in-scope declarations, we can parse each AST into memory one at a time, checking each declaration for inclusion in the in-scope dictionary. If it is, we add it and its child nodes to an accumulating collection of dictionaries that make up the complete data set needed for the rest of the application. This is the second pass, and it's a great place to perform processing that requires knowledge of a node's references.

The exact data this three step process creates goes into forming the Data Context type:
 1. A set of files that are in scope for the audit
 2. A directory of nodes by topic ID, where each node's children are stored as node stubs
 3. A directory of all declarations (reference-able identifiers in the source code) by topic ID with their name, scope, and declaration kind
 4. A directory of references to the declaration by topic ID
 5. A directory of extended properties for functions and modifiers, this will include function parameters, returns, reverts, calls to other functions within it, and mutations to state variables within it. Each of these properties will have rich data about the subjects and should contain references to the relevant declarations when possible (ie, the function arguments should list the topic ID for the local variables that the arguments are mapped to).

See the collaborator section for topic ID details.

Declarations are scoped by four properties: Container, Component, Member, and SemanticBlock. Using the scope, any identifier/operation or its parent can be linked to.

For contract source files, the container is the source file, the component is a contract, the member is a function, and the semantic block is a semantic block, block, or signature. A contract's scope will only be a container, a function's scope will be a container and a component, a parameter variable's scope will be a container, component, and member, and a local variable's scope will be a container, component, member, and semantic block. For documentation, the container is the source file, the component is the first header section, the member is the second header section, and the semantic block is the following nested header sections. For comments, the scope is copied from the topic it is being added to, as comments do not have complex formatting and structures.

# Formatter
The formatter takes an AST node from the parser and the data context from the analyzer and returns an HTML string.

When rendered, this HTML is designed to be forty characters wide. Forty characters allows for four columns of code to be displayed side-by-side on a typical screen while not being so small that the comments are difficult to read. (Forty characters also allow for two columns of code to be printed on standard paper.) The formatter does not enforce the forty character width explicitly, but implicitly because all nodes are always formatted in the most vertical way possible. A variable name or literal value could overflow the forty character limit, and that is unavoidable.

There are two core primitives for the formatter to respect: identifiers and operators. Each identifier and each operator will have dedicated topics for discussion, so they each need to be set on different lines so that their comments can be displayed inline above them. Identifier and operator inline comments are formatted differently, so there can be both an identifier and an operator on the same line, but the same line cannot have two identifiers or two operators.

There is only one way to format each expression because there are strict per-line formatting rules, making the formatter output very vertical but straightforward to implement.

When any declaration/reference, or operator line is rendered to HTML, it will have an empty span element before it, allowing the client to inject info comments into that element dynamically. Because the code width is always set to 40 characters, formatting of the inline comments to be injected into the HTML is straightforward as well.

The formatter output does not include traditional line numbers because the formatter is aggressive in changing the source text, and the API is designed to enable clients to display many smaller snippets of code. Clients are not expected to show complete source files in regular use, so the original line numbers are not particularly meaningful. Because of this, if a gutter is shown, it will have operation numbers instead of line numbers.

Although the complete source code of a file is not used, clients may want to display or allow copying the full source file in a separate view, at the user's request, for niche purposes. It should not be interactive.

The formatter can format nodes as source text or as signatures. Source text is now the node would appear in the source file, but a signature is how a node should be represented in an isolated way, nested inside discussions or modals. For example, the source text of a function would contain the function's body, but the signature would not contain the body. Source text for variables will not include type information, but signatures will. Text is rendered the same either way.

# Collaborator

The collaborator allows users to comment on topics in the audit. The types of comments users can leave on topics depends on the type of topic. These comments can be collaborative discussions or structured properties of the topics.

The types of topics and their prefixes are:
 - Source code nodes (N)
 - Documentation (D)
 - Comments (C)
 - Attack vectors (A)
 - Invariants (I)

 The topic id is a string identifier that uniquely identifies a topic within the audit. Each source code contract, function, block, statement, expression, variable, and literal value has an unique topic id. Each text document/comment and section has an unique topic id. It is sequential number for that topic type preceeded by the topic type prefix. For example, the first added Attack Vector will be `A1`, and the second `A2`.

## Documentation

Documentation is a first class citizen in the system. It is as interactive as the source code, and is parsed for sections and paragraphs, which become topics for users to comment on and link to other topics.

Relevant documentation may not be included in the source code originally, so there should be a way to add external documentation to be parsed and brought into the system, allowing users to comment on it as well.

## Discussion Comments

Users can leave discussion comments on any topic. These comments are first class citizens in the system, like documentation. They are parsed in the same way, and are given topics within them that users can comment on and link to other topics.

# Checker

The checker is responsible for two things: checking the subjects of convergences across the audit for contradictions in their properties, and managing the security model that defines what properties should hold. If the properties at a convergence contradict, that is a potential place for a vulnerability or implementation flaw.

## Security Model

The security model is the structured representation of what the documentation claims, what the code actually does, what could go wrong, and which source locations are relevant to each concern. It is built incrementally throughout the audit — seeded from project documentation and refined on-the-fly as auditors review code and discover new concerns.

### Design Principles

**Documentation is untrusted.** Developer docs represent claimed behavior, not verified truth. The system treats them as input to reason about, not as a source of correctness.

**Features describe behavior, not implementation.** Feature names and descriptions are written at the user-visible or protocol level. A pause mechanism is a feature whether the docs describe it as "emergency stop" or "modifier onlyOwner on pause()".

**Requirements capture all documented claims.** Requirements are what the documentation says the system does. They can describe any kind of documented behavior — functionality, constraints, access control, edge case handling. They are extracted from documentation and verified by confirming that corresponding behaviors exist in the implementation.

**Behaviors capture what the code actually does.** Behaviors are observed during code review and represent the real implementation logic. They are reconciliation artifacts — compared against requirements to surface gaps between documentation and implementation.

**Threats operate at two levels.** Feature-level threats capture design-level risks derived from documentation and features — broad concerns like "unauthorized fund access." Subject-level threats capture implementation-specific risks on non-pure source code subjects (state reads, state writes, external calls) — specific concerns like "reentrancy via token callback in this external call." Pure operations (arithmetic, comparisons, local variable assignments) are excluded from subject-level threat generation because their threat surface is fully covered by type convergences and their correctness is checked by functional semantics.

**Invariants live on source code subjects.** Invariants are properties the code must uphold to protect against threats. They are attached directly to the source subjects where they are checked at convergences, not to abstract structures like behaviors. This eliminates indirection — when the auditor evaluates a convergence, the invariants that apply are immediately present. Invariants are generated from both feature-level threats (broad invariants that apply across a feature's source subjects) and subject-level threats (specific invariants on individual non-pure subjects). Each invariant links back to its parent threat for traceability.

**Source locations link to features.** Contract members are linked to the features they participate in. This allows behaviors created during code review to automatically associate with the right features, and enables reconciliation between documented requirements and observed behaviors.

### Initial Generation

The security model is initially seeded from project documentation through an automated pipeline:

**1. Feature Extraction.** Documentation files are processed to extract behavioral features and their requirements. When multiple documents exist, each is analyzed independently and the results are consolidated into a unified feature set. During consolidation, broad features with overlapping requirements are dissolved into more specific ones. Each requirement retains links to the documentation it was derived from, preserving traceability to the original developer claims.

**2. Threat and Invariant Generation.** For each feature, the pipeline generates feature-level threats — design-level risks derived from the feature's documentation and requirements. Each threat carries a severity rating. When the project includes security notes (developer-documented threats, roles, known invariants), a review pass checks whether those concerns are covered and fills gaps. Feature-level invariants are generated from these threats and attached to the source subjects within the feature's scope.

Feature-level threats are few and general, derived from project documentation and the behavioral features. A threat may be that users can lose funds; invariants against that threat may be that funds cannot be sent to an irrecoverable address, or that balances cannot underflow. The hierarchy flows from documentation to features to threats to invariants on source subjects.

Where requirements and invariants both describe things the code must do, they serve different concerns. Requirements capture what the documentation claims — the functionality described to users or the protocol. Invariants capture what the code must enforce to protect against threats — the defensive properties that prevent threats from materializing. A collateral lending feature has a documented requirement that users can deposit ETH, but the invariant that only the position owner can withdraw collateral exists to protect against a threat, not to fulfill a documented claim. Requirements are verified by matching them to behaviors during reconciliation; invariants are verified by checking them against convergences in the source code.

**3. Source-to-Feature Linking.** Each contract member in scope is evaluated to determine which features it participates in. This produces source-to-feature links: a map from concrete code locations to the features they implement. A function can link to more than one feature. This mapping is the mechanism through which behaviors created during code review are automatically associated with the correct features — when an auditor adds a behavior while reviewing a function, the behavior inherits the feature association from the source location.

### On-the-Fly Generation

The security model is not static after initial generation. As auditors review code and encounter security-relevant patterns, they add new features, requirements, behaviors, threats, and invariants directly from the code context. This on-the-fly generation is the primary mechanism through which the audit achieves comprehensive coverage.

When a user adds a new element to the security model, the relevant pipeline steps run automatically in the background:

- **New feature** — Generates feature-level threats, then links source locations to the feature. Invariants from the generated threats are attached to the source subjects within the feature's scope.
- **New requirement** — Added to its parent feature. Requirements do not trigger threat generation or source linking; they are documentation claims that will be verified during reconciliation against behaviors.
- **New behavior** — Created during code review and automatically associated with a feature via the source-to-feature link on the source location where it was observed. Behaviors are reconciliation artifacts and do not carry threat or invariant links.
- **New feature-level threat** — Invariants are generated from the threat and attached to source subjects within the feature's scope. The system triggers re-checks against all previously-audited subjects that fall within scope.
- **New subject-level threat** — Generated on non-pure subjects during code review (see Subject-Level Threats below). Invariants are generated from the threat and attached to the subject. The system triggers re-checks on the subject.
- **New invariant** — Attached to a source subject, linked to its parent threat. The system triggers re-checks against the subject and any related subjects within the invariant's scope.

The user's request completes immediately; background work finishes asynchronously.

This re-check mechanism is what makes the system's backward-only evaluation context sufficient for comprehensive coverage (see Subject Evaluation Context Strategy below). Rather than including forward context to passively surface gaps, the system actively propagates newly-discovered concerns to all relevant code locations. When an auditor notices an access control check on one function and registers it as an invariant, every other function within that invariant's scope is mechanically re-evaluated — surfacing any that lack the expected check without the auditor needing to have seen all those functions in the same context window.

### Reconciliation

Reconciliation is the explicit step where the auditor compares documented requirements against observed behaviors for a given feature. The system presents the feature's requirements (from documentation) alongside the behaviors associated with the feature (via source-to-feature links). The auditor evaluates coverage in both directions:

- **Requirements without matching behaviors** are unimplemented specification — the documentation claims the system does something, but no corresponding behavior was observed in the code.
- **Behaviors without matching requirements** are undocumented implementation — the code does something significant that the documentation does not describe.

Both are findings. The nature of the relationship between individual requirements and behaviors — whether a behavior fulfills, constrains, or conflicts with a requirement — is assessed by the auditor during this step, not at behavior creation time. This keeps behavior creation lightweight and focused on the code, while reconciliation provides the dedicated context for documentation-implementation comparison.

Features structure the reconciliation into manageable units. Instead of reconciling all requirements against all behaviors in an audit at once, the auditor works through one feature at a time.

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
  ├── Behavior (from code)
  │     "Only one campaign can exist per token"
  │     └── Source Links: [deployCampaign(), campaignsByToken]
  ├── Feature-Level Threat                                 ← design-level risks
  │     "Token slot squatting prevents legitimate protocols from deploying"
  └── Feature-Level Threat
        "Unauthorized withdrawal of campaign tokens"

Source-to-Feature Links:
  deployCampaign()    → "Protocol Campaigns"
  withdrawRemaining() → "Protocol Campaigns"
  campaignsByToken    → "Protocol Campaigns"

Source Subject: deployCampaign() → IERC20(token).transferFrom(...)   [non-pure: external call]
  ├── Functional Purpose: "Transfer campaign tokens from creator into contract"
  ├── Functional Semantics: "token is the campaign reward token, amount is total campaign allocation"
  ├── Subject-Level Threat: "Reentrancy via token callback during campaign setup"
  ├── Invariant: "State updates precede external calls in deployCampaign"
  │     └── Parent Threat: (subject-level) "Reentrancy via token callback..."
  └── Invariant: "Only campaign creator can initiate token transfer"
        └── Parent Threat: (feature-level) "Unauthorized withdrawal..."
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
   2. Behaviors (the observed behaviors that apply to this subject, associated via source-to-feature linking)
   3. Dependencies (what the subject depends on to fulfill its purpose, expressed as links to other statements or mechanisms)
   4. Invariants (defensive properties the subject must uphold, derived from feature-level and subject-level threats)
 - Non-Pure Subjects (state reads, state writes, external calls, delegatecalls, assembly blocks):
   1. All of the above, plus:
   2. Subject-Level Threats (implementation-specific risks at this code point, generated on-demand)
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

Functional purpose is generated on-demand using the same pattern as functional semantics: when a subject is first evaluated, the LLM is given the subject, its feature context, and the feature's documentation, and asked "why does this exist?" The result is cached on the subject and presented to the auditor for review. The auditor can correct, enrich, or confirm it before downstream analysis proceeds.

Try to avoid implementation details in functional purposes, and focus on the business logic. "Why" is not "to do the thing it does." It is "from the project perspective, what value does this statement provide?", and "what impact would it have on the users or system if it weren't there?"

When adding functional purposes, preset questions are presented to the user that help guide them in thinking about the purpose correctly. Functional purposes can be categorized, which will adjust the preset questions for the user to answer. Some of the categories could be for Shared Resources, Authorization, or Reentry Guards. This will make sure the user is reminded of common issues with these things, like a DOS exhaustion of a shared resource.

### Managing Functional Semantics

Functional semantics define what an expression or value represents within the context of the application — the project-specific meaning of a variable, literal, or sub-expression. For example, `userBalance` might semantically represent "the user's total balance" in one project and "the user's locked balance" in another. `propFactor` might semantically represent "a balance multiplier." These meanings are not derivable from the code alone — they come from project documentation.

Functional semantics are generated on-demand rather than through a structured upfront extraction pipeline. When a subject is first evaluated during the audit, the LLM is given the subject itself, the features the subject is linked to (via source-to-feature links), and the documentation linked to those features, with the question: "what is the semantic meaning of this subject within this feature?" The LLM synthesizes a semantic meaning from the full documentation context in a single pass, and the result is cached on the subject.

This on-demand approach is preferred over an upfront pipeline that would extract semantic claims from documentation, resolve them to declarations, and propagate properties to convergence points. Such a pipeline introduces a lossy transformation chain — each extraction and resolution step can distort or lose information, and errors propagate forward undetected. The on-demand approach avoids this by keeping the full documentation in context at the moment of evaluation, which also handles implicit and contextual semantics naturally. Documentation often defines meaning through surrounding context rather than explicit definition (e.g., a paragraph discussing "rewards accumulate proportionally to stake" implies a multiplicative relationship without stating it as a discrete claim), and the on-demand approach lets the LLM reason about these implications directly.

Coverage is guaranteed by the programmatic audit's traversal of every subject. Every expression and value gets evaluated, so every subject that has a feature with documentation gets a semantic annotation. Nothing is missed due to the lack of upfront extraction.

Once generated, the functional semantic is presented to the human auditor for review. The auditor can correct it, enrich it with domain knowledge, or confirm it. The corrected semantic is cached and reused at every convergence involving that subject. The auditor can also add functional semantics manually at any point when they recognize a meaning the LLM missed.

Functional semantics are checked at specification convergences. When the auditor or LLM evaluates a statement like `userBonus = interest * userBalance`, the functional semantic on `userBalance` ("the user's total balance") can contradict the containing statement's functional purpose ("calculate bonus based on locked token interest"). Similarly, if `propFactor` has the semantic "a balance multiplier" but appears in an addition operation `val = propFactor + bal`, the semantic contradicts the operator. These contradictions are only surfaceable when the documentation-derived semantic and the code-derived context are both present at the convergence point.

### Managing Requirements

Requirements are what the documentation claims the system does. Each requirement captures a documented behavior — something the documentation says the system needs to accomplish. They can describe any kind of documented behavior: functionality, constraints, access control, edge case handling. For example, a "Protocol Campaigns" feature might have requirements like "protocols can permissionlessly deploy a campaign for their token" and "campaign creators can withdraw remaining tokens after campaign ends."

Requirements are defined at the feature level in the security model. They do not carry source links, threat links, or invariant links — security analysis operates at the source subject level. Requirements are verified not by checking them directly against source code, but by confirming during reconciliation that corresponding behaviors exist in the implementation.

All requirements on a feature are distinct and independent of each other. Requirements are added initially as unverified, then are able to be marked as verified by each party in the audit during reconciliation.

Requirements are generally explored as documentation is reviewed, but auditors can also add requirements on-the-fly when they encounter documented claims that were not captured during initial generation.

### Managing Behaviors

Behaviors are what the code actually does. Each behavior captures an observed implementation behavior — something the auditor noticed while reviewing code. A behavior is a description and source links to the code where it was observed.

Behaviors are created during code review and automatically associated with a feature via the source-to-feature link on the source location where they were observed. This means the auditor never leaves the code context when adding a behavior — they describe what the code does, and the model knows which feature it belongs to.

Behaviors are purely reconciliation artifacts. They do not carry threat links or invariant links — security analysis operates at the source subject level through subject-level threats and invariants on convergences. Behaviors exist to enable the comparison between what the documentation claims (requirements) and what the code does (behaviors) during reconciliation.

### Managing Dependencies

Dependencies are things that a subject depends on outside of its interface (and thus cannot be expressed by a type constraint). These are things like a stateful function that requires another block to set some required piece of state for it to work with. Like an emergency exit function that requires an exit address to be set first. Because the dependency can be fulfilled by another block far away from the subject, statement chains have to be tracked throughout the project so we can tell where a project dependency could be fulfilled. To mark a dependency as fulfilled, the user has to provide a statement id that satisfies the dependency. This statement then becomes a convergence point for the dependency.

Statement chains are as follows: there is a main, same block chain. This represents all the statements before the subject in the same block. If the dependency is satisfied by one of these statements, then the dependency will always be satisfied and we can mark it as verified. If these same-block statements do not satisfy the dependency, then it may be satisfied by a statement above the containing block in the same function, but not in sub-statements of prior sibling statements (this makes it so that both blocks of an if/else statement are blanket checked as one). If statements within the containing function do not satisfy the dependency, then it may be satisfied within the constructor statements. If it is not satisfied within the constructor, then it may be satisfied within a prior sibling statement in each block where the containing function is called. (If the containing function is an external function, then the dependency can be found anywhere in the project, as they will have to be two separate user calls.) If one of the calls does not satisfy the dependency, then it is unsatisfied as a whole. If the immediate call block does not satisfy the dependency, then a prior block in the call chain is searched for in the same way as the original same block chain.

Referencing mutable values (notably state variables, but mutable local variables too) will always have a dependency associated. Mutations to the state variable that happen always within the call chain of the statement must be considered, and introduce local variable refinements (i.e., a variable may always be non-zero in one place it is referenced, but not in another because of the checks/mutations that happen before it in a chain). Because of this, a reference to a mutable variable will have its own topic (which represents the unique state of the referenced variable at the reference point), while also being linked to the topic of the variable itself. A reference to an immutable variable can be treated as the same topic as the variable itself.

To implement this, we can gather all statements that could satisfy the dependency (when a dependency is added, as many statements will have none and we do not want to waste time processing them) and store them as a tree, with the subject being the root, and tree depth representing prior statements. Then we take the statements that the user said satisfied the dependency, and search for a path to a leaf statement that does not encounter one of the satisfying statements. If we can get to a leaf without encountering a satisfying statement, then that is a path to the subject where the dependency is not satisfied, and the dependency is not satisfied as a whole. A leaf node would be some entry point to the project, like a public function. For this implementation, we would have to store an AST of statements to pull the sub-trees from to get the subject as the root.

### Managing Consumers

A consumer is a statement that consumes what was set up by the current statement. A dependency checks that something was set up before the current statement, and a consumer checks that what is set up by the current statement is properly used by later statements. A consumer turns into a dependency on the consuming statement when it is linked, and a dependency turns into a consumer on the depended-upon statement when it is linked, so they can both be checked in the same way. Dependencies and consumers cannot be checked exhaustively, so checking for the existence or fullness of them is an important job of the auditor. When a statement has a side effect, at least one consumer must exist and should be searched for. The consumer is first added in an unsatisfied state, and added for satisfaction when a satisfying statement is found. Checking for at least one consumer is a way to make sure that something is not set up and then forgotten to be consumed, indicating a potential bug.

### Managing Subject-Level Threats and Invariants

The checker classifies source code subjects as pure or non-pure based on their interaction surface. Pure operations (arithmetic, comparisons, boolean logic, local variable assignments) have a closed threat surface — the only things that can go wrong are mechanical (overflow, type mismatch, wrong operands), and these are fully covered by type convergences. Non-pure operations interact with persistent state, external code, or the blockchain environment, creating implementation-specific attack surface that requires adversarial reasoning.

Non-pure subject types include:
 - State writes (storage mutations)
 - State reads of mutable variables (storage reads that could return manipulated or stale data)
 - External calls (transfers control to untrusted code)
 - Delegatecalls (executes in a different context than it appears)
 - Assembly blocks (bypasses compiler safety checks)
 - Selfdestruct / create / create2

Subject-level threats are generated on-demand for non-pure subjects only. When a non-pure subject is first evaluated during the audit, the LLM is given the subject, its backward context, its feature-level threats (to avoid restating them), and its type constraints (to avoid restating those), and asked "what implementation-specific risks exist at this code point that aren't already captured?" The result is cached on the subject and presented to the auditor for review.

Invariants are generated from both feature-level and subject-level threats and attached directly to the source subjects they protect. When the auditor evaluates a convergence, the invariants are immediately present alongside functional purpose, functional semantics, and type constraints — no indirection through abstract structures. Each invariant links back to its parent threat (either feature-level or subject-level) for traceability.

Pure operations are not excluded from invariant coverage. Feature-level threats can generate invariants that apply to pure subjects — for example, a feature-level threat about reward calculation errors might produce an invariant on an arithmetic expression. What pure subjects are excluded from is dedicated subject-level threat generation, because the threats at a pure operation are either mechanical (covered by type convergences) or semantic (covered by functional semantics at specification convergences).

The three layers of the threat model cover distinct risk categories with minimal overlap:
 - **Type convergences** catch mechanical correctness at all subjects (overflow, underflow, type mismatches)
 - **Feature-level threats** capture design-level risks (unauthorized access, economic invariant violations) and generate broad invariants across a feature's source subjects
 - **Subject-level threats** capture implementation-specific risks at non-pure subjects (reentrancy at a specific external call, stale data from a specific state read) and generate targeted invariants on individual subjects

Pure expressions can house semantic bugs — `propFactor + bal` where the operator should be `*` — but these are caught by functional semantics at specification convergences, not by threat generation. The adversarial reasoning that threats capture always bottlenecks through non-pure operations, because those are the points where the system interacts with the outside world. Pure expressions may be *incorrect*, but their incorrectness becomes *exploitable* only through the non-pure operations that act on their results. Subject-level threats on non-pure subjects have visibility into the pure expressions that feed into them through backward context, so the threats account for insufficiencies in upstream checks and computations.

### Threat Traceability

When an invariant is contradicted at a convergence, the traceability chain identifies the security impact. For invariants from feature-level threats: convergence → invariant → feature-level threat → feature → documentation. For invariants from subject-level threats: convergence → invariant → subject-level threat → feature (via source-to-feature link). Both paths lead to a feature, which provides the severity context and the connection to documentation and requirements.

This traceability enables prioritization: when the auditor is evaluating a convergence, the severity of the threats behind its invariants determines how much scrutiny the convergence warrants.

## Auditing Convergences

Type constraint checks can be checked by a constraint algorithm. Specification convergence checks are annotated in regular language with business logic and can only be checked by something that understands the specific business logic semantics.

General audit flow is:
 1. Read and understand the docs and the purpose of the project
 2. Run the initial generation pipeline to extract features, requirements, and feature-level threats from the documentation, link source locations to features, and attach invariants from feature-level threats to source subjects
 3. Review and refine the generated security model, adding missing features or requirements as needed
 4. Read the code, documenting behaviors as they are observed (behaviors automatically associate with features via source-to-feature links)
 5. Step through all convergences — for each subject, functional purpose, functional semantics, and subject-level threats (on non-pure subjects) are generated on-demand; invariants from subject-level threats are attached and checked alongside existing invariants from feature-level threats
 6. Reconcile requirements against behaviors per feature, identifying unimplemented specification and undocumented implementation
 7. As new features, requirements, behaviors, threats, or invariants are discovered during code review, add them to the security model — the re-check system propagates them to all relevant subjects, ensuring nothing is missed

### Managing Convergences

Convergences are the main point of verification in the audit process. They have four states: waiting, unverified, verified, and contradicted. A waiting convergence is one that is waiting for properties to be added to its parts, i.e. `a + b`, but `a` does not have any properties added so we cannot judge the convergence.

## Subject Evaluation Context Strategy

When evaluating subjects for invariant upholding or invariant recognition, we use **backward-only context** as the default strategy. When auditing a given subject, the system provides context about where the subject's values originated (their data provenance, taint sources, and upstream transformations), the subject's functional purpose (why it exists, derived from the feature), the subject's functional semantics (what it represents, derived on-demand from project documentation), its invariants (defensive properties it must uphold, from feature-level and subject-level threats), and for non-pure subjects, its subject-level threats (implementation-specific risks at this code point). The system does not, by default, include forward context about where those values propagate to downstream.

This is a deliberate architectural choice, not a limitation. The system compensates for the absence of forward context through the **security model's on-the-fly generation and re-check mechanism**, which provides equivalent or superior coverage to bidirectional context while maintaining focused, low-noise audit passes.

### Why Not Bidirectional Context

An intuitive approach to auditing a variable is to present both where its value came from and where it goes. This gives a complete picture of the variable's role in the system. The case for this is strongest when a single value fans out to multiple destinations with different requirements — seeing the full fan-out from one vantage point can surface design-level issues like incomplete access control coverage or inconsistent invariant enforcement across functions.

However, this advantage is narrower than it first appears, for two reasons.

**The inferential demand is the same either way.** Forward context presents the auditor with a list of destinations and relies on the auditor to recognize that a pattern (such as an access control check) is present at some sites and missing at others. Backward-only context presents the auditor with the pattern at the first site encountered and relies on the auditor to generalize it into an invariant. In both cases, the auditor must perform the same act of recognition and generalization. Forward context does not reduce the cognitive work; it merely rearranges when it happens.

**Forward context introduces noise that degrades audit quality.** Forward propagation scales with the number of consumers of a state variable. In smart contracts, heavily-read state variables (token balances, approval mappings, configuration parameters) may be referenced across many functions. Including all downstream consumers in the context of every audit pass dilutes the signal about the specific code under review. For LLM auditors in particular, this context dilution measurably degrades reasoning about the subject at hand. Backward-only context keeps each audit pass lean and focused on the immediate code being evaluated.

### How the Security Model Closes the Gap

The primary class of vulnerability where forward context provides value is **incomplete invariant enforcement**: cases where a check or pattern should be applied uniformly across a set of functions but is missing from one or more of them. The canonical smart contract examples are access control gaps (a modifier guards some privileged functions but not all) and economic invariant violations (some functions that modify a balance correctly maintain a sum-total relationship while others do not).

Rather than using forward context to surface these issues passively, this system surfaces them actively through the security model's on-the-fly generation:

1. **Invariant discovery.** When the auditor encounters a security-relevant pattern during a backward-context audit pass (e.g., a role-based access check on a state-modifying function), they register it as a threat on the relevant feature (or a subject-level threat on a non-pure subject) with an associated invariant attached to the source subject. The incremental update pipeline propagates the invariant to all subjects within its scope via re-checks.

2. **Propagation via re-check.** The system applies the new invariant to all previously-audited subjects within its scope. Any function that modifies the same state but lacks the expected check is flagged mechanically, without requiring forward context at the point of origin.

3. **Completeness guarantee.** Because the audit is programmatic and covers every variable and statement, the re-check mechanism provides exhaustive coverage. Once an invariant is articulated, no violation can escape detection. This is stronger than forward context, which still depends on an auditor noticing a discrepancy within a potentially large list of destinations.

This architecture cleanly separates two concerns that bidirectional context conflates: understanding what a specific piece of code does (served by focused backward context) and verifying that invariants hold globally (served by the security model's re-check system). Each concern is handled by a mechanism optimized for it.

### Known Limitations and Mitigations

The backward-only strategy with invariant re-checking has one residual limitation: invariant discovery depends on the auditor encountering at least one instance of a pattern and recognizing its security significance. Two edge cases deserve attention.

**Subtle semantic distinctions.** Some invariants require recognizing that two superficially similar operations are categorically different. For example, a function that writes to `balances[msg.sender]` (self-modification) versus one that writes to `balances[account]` (arbitrary-address modification) may both appear as simple mapping writes, but only the latter requires elevated privilege. Forward context does not inherently make this distinction more visible — the auditor must apply domain knowledge in either case — but the system should include heuristic prompts that direct attention to these distinctions. Examples include flagging functions that modify state keyed by a caller-supplied address, or functions that bypass standard entry points.

**Absent mechanisms.** If a contract should implement a security pattern (such as a pause mechanism) but does not, no backward-context encounter will contain a reference to the missing pattern. This class of finding is addressed by the initial generation pipeline's threat and invariant generation step, which derives expected security mechanisms from the project's documentation and feature set independently of what the code contains. When the initial generation identifies that a feature implies a pause mechanism, the corresponding invariant is checked against the codebase regardless of whether any code references pausing.

### Summary

The backward-only context strategy is not a cost-saving approximation of bidirectional analysis. It is a design that optimizes for audit quality by keeping individual audit passes focused, while relying on the security model to provide the global coverage that forward context would otherwise supply. The security model achieves this coverage more reliably — mechanically and exhaustively — once an invariant is identified, and the investment in invariant discovery heuristics and threat generation compounds across engagements in a way that raw forward context never does.
