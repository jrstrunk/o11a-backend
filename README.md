# o11a-backend
The o11a backend has five modules that form a processing pipeline:

 1. Parser - parses the audit source and allows clients to post new text blobs (added comments and docs) to be parsed in the context of the audit
 2. Analyzer - analyzes the audit source, producing a directory of definitions and their attributes, allowing clients to fetch structured audit data
 3. Formatter - transforms the AST into formatter nodes
 4. Collaborator - stores discussion topics and the comments under them, allowing clients to post new comments and approve or disapprove of comments from other users
 5. Checker - checks variable constraints and allows clients to post new constraints to be checked, providing data on any conflicting constraints

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

The checker is responsible for checking the subjects of convergences across the audit for contradictions in their properties. If the properties at a convergence contradict, that is a potential place for a vulnerability or implementation flaw.

Across the audit, two types of convergences are checked for contradictions:
 1. Type Convergences (where two values interact with each other via an operator)
 2. Specification Convergences (where many pieces of code implement a functional specification)

## Type Convergence

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

### Managing Type Convergences

#### Variable Ancestors

Function parameters each have ancestors that are the call arguments which correspond to the function parameters. A parameter may have many ancestors, as it has one for each call to that function. In the case of a function that is only called once, the parameter ancestors can be thought of as the same as the parameter itself. This is called a transitive ancestor. In the analysis, both the subject and ancestor can be given the same topic so they will be treated as the same in the audit.

Ancestors can be literal values.

When auditing a variable, it is useful for the client to present all of its ancestors to the user to check at once. This may reveal a common pattern or outlier among the ancestors.

#### Variable Descendants

Variable descendants are the function parameters that are passed the subject variable. A variable may have many descendants if it is passed into multiple functions. When auditing a variable, it is useful for the client to present all of its descendants to the user to check at once. This may reveal how a variable value propagates through the codebase, seeing the properties of the places it ends up, especially into other contracts and blocks of logic.

## Specification Convergence

There are three types of properties that may be checked on the subjects of a specification convergence, depending on the kind of subject:
 - Project Implementation, Contracts, Blocks, Statements:
   1. Functional Purpose (what purpose it serves within the context of the application)
   2. Functional Requirements (what it has to do and not to do to fulfill the functional purpose)
   3. Functional Dependencies (what it depends on to fulfill the functional purpose)
 - Expressions and Values:
   1. Functional Semantics (what it represents within the context of the application)

Specification properties converge between the declaration and implementation of the subject:
 - Project Implementation (checked that the sum of the properties of the contracts match the properties of the project implementation)
 - Contracts (checked that the sum of the properties of the functions match the properties of the contract)
 - Functions (checked that the sum of the properties of the block statements match the properties of the function)
 - If/else statements (checked that the sum of the properties of the block statements match the properties of the if/else statement)
 - Loops (checked that the sum of the properties of the block statements match the properties of the loop)
 - Statements (checked that the sum of the properties of the expressions in the statement match the properties of the statement)
 - Expressions (checked that the the properties of the values/subexpressions in context of the operation match the properties of the expression)

Specification properties are intrinsic to the project documentation, and these properties converge with the project implementation's specification properties.

Specification convergences are based on project-specific design and cannot be checked by an algorithm. Instead, they must be manually verified to uphold within the unique project environment.

Should statements be actual source statements, or a semantic grouping of statements based on extra whitespace in the source?

### Function Call Convergences

Function calls are an expression that has a subexpression of argument list passing, which results in the return value of the function. Because of this, function calls have both semantic properties based on their return value to can be checked with other semantic properties in a straightforward way, and other functional properties.

Index access only has semantic properties like values.

Functions have a semantic return value can converge with other semantic properties in an expression, but they also have functional properties. These functional properties may not affect the semantics of the expression, but they converge with the containing statement's functional properties. For example, `add(a, b) - c` has a semantic property convergence at `the_result_of_add_a_b - c` to form one semantic property for the expression, but the functional properties of `add` converge with the containing statement's functional properties to make sure they fulfill the containing statement's functional requirements and align with its purpose.


Can we have a convergence property graph that represents the properties that are dependent on other properties, so that if a property downstream of something is contradicted, we can see how it affects the upstream properties? Maybe all requirements stem from an underlying invariant (which all stem from an attack vector), so when a property is contradicted, we can immediately trace it back through the graph to see how and which invariant is violated.

#### Function Call Signatures

Function calls need to have comprehensive signatures that include all necessary information for the caller to understand the function's behavior and potential side effects. These are of course the input parameters and return types like Solidity, but also the exceptions that can be thrown by the function, the state variables that are read, the state variables that are mutated, and the functions that are called (with their respective signatures).

Functions need to track side effects and present them to the user in the interface, like exceptions are.

### Managing Functional Purpose

The functional purpose is the business logic reason for a statement—the "why" that statement is there. Try to avoid implementation details here, and focus on the business logic. "Why" is not "to do the thing it does". It is "from the project perspective, what value does this statement provide?", and "what impact would it have on the users or system if it weren't there?"

When adding functional purposes, preset questions are presented to the user that help guide them in thinking about the purpose correctly. The purpose is NOT what it does, but WHY it does it.

Functional purposes can be categorized, which will adjust the preset questions for the user to answer. Some of the categories could be for Shared Resources, Authorization, or Reentry Guards. This will make sure the user is reminded of common issues with these things, like a DOS exhaustion of a shared resource.

### Managing Functional Requirements

One subject may have many functional requirements. All functional requirements are distinct and independent of each other.

Functional requirement properties are added to a definition initially as unverified, then are able to be marked as verified by each party in the audit as they review convergences. When reviewing a specification convergence (comparing definition requirements to containing item purposes), each functional requirement should be reviewed and marked as verified or contradicted independently of the others.

Functional requirements have dependencies attached to them. When a functional requirement is added to a subject, it needs to have a reason for that requirement. Whatever depends on that requirement is a dependency. In every place a function is called, if it has to uphold a certain property, it should be listed as a dependency of the requirement. This way, if the requirement is contradicted, we can immediately trace it back through the graph to see how and which invariant is violated.

Functional requirements are generally explored as the usage of the subject is studied; therefore, it is important that clients allow users to see and add functional requirements to the subject at a call site.

### Managing Functional Dependencies

Functional dependencies can only be satisfied by statements, not expressions or values.

Functional dependencies are things that a subject depends on outside of its interface (and thus cannot be expressed by a type constraint). These are things like a stateful function that requires another block to set some required piece of state for it to work with. Like an emergency exit function that requires an exit address to be set first. Because the dependency can be fulfilled by another block far away from the subject, statement chains have to be tracked throughout the project so we can tell where a project dependency could be fulfilled. To mark a dependency as fulfilled, the user has to provide a statement id that satisfies the dependency. This statement then becomes a convergence point for the functional dependency properties of the subject.

Statement chains are as follows: there is a main, same block chain. This represents all the statements before the subject in the same block. If the dependency is satisfied by one of these statements, then the dependency will always be satisified and we can mark it as verified. If these same-block statements do not satisfy the dependency, then it may be satisfied by a statement above the containing block in the same function, but not in sub-statements of prior sibling statements (this makes it so that both blocks of an if/else statement are blanked checked as one). If statements within the containing function do not satisfy the dependency, then it may be satisfied within the constructor statements. If is it not satisfied within the constructor, then it may be satisfied within a prior sibling statement in each block where the containing function is called. (If the containing function is an external function, then the dependency can be found anywhere in the project, as they will have to be two separate user calls.) If one of the calls does not satisfy the dependency, then it is unsatisfied as a whole. If the immediate call block does not satisfy the dependency, then a prior block in ethe call chain is searched for in the same way as the original same block chain.

Referencing mutable values (notibly state variables, but mutable local variables too) will always have a Functional Dependency associated. Mutations to the state variable that happen always within the call chain of the statement must be considered, and introduce local variable refinements (ie., a variable may always be non-zero in one place it is referenced, but not in another because of the checks / mutations that happen before it in a chain). Because of this, a reference to a mutable variable will have its own topic (which represents the unique state of the referenced variable at the reference point), while also being linked to the topic of the variable itself (maybe it is linked as an ancestor). A reference to a immutable variable can be treated as the same topic as the variable itself.

To implement this, we can gather all statements that could satisfy the dependency (when a dependency is added, as many statements will have none and we do not want to waste time processing them) and store them as a tree, with the subject being the root, and tree depth representing prior statements. Then we take the statements that the user said satisfied the dependency, and search for a path to a leaf statement that does not encounter one of the satisfying statements. If we can get to a leaf without encountering a satisfying statement, then that is a path to the subject where the dependency is not satisfied, and the dependency is not satisfied as a whole. A leaf node would be some entry point to the project, like a public function. For this implementation, we would have to store an AST of statements to pull the sub-trees from to get the subject as the root.

### Managing Functional Consumers

A functional consumer is a statement that consumes what was set up by the current statement. A functional dependency checks that something was set up before the current statement, and a functional consumer checks that what is set up by the current statement is preperly used by later statements. A functional consumer turns into a functional dependency on the consuming statement when it is linked, and a functional dependency turns into a functional consumer on the consuming statement when it is linked, and so they can both be checked in the same way. Dependencies and consumers cannot be checked exhuastively, so checking for the existence or fullness of them is an important job of the auditor. When a statement has a side effect, at least one functional consumer must exist and should be searched for. The consumer is first added in an unsatisfied state, and added for satisfaction when a satifying statement is found. Checking for a least one consumer is a way to make sure that something is not set up, and then forgotten to be consumed, indicating a potential bug.

### Managing Functional Failure Points

The functional failure points are a list of things that could invalidate the current functional property. These things help the auditor enumerate what could go wrong, and serve like a checklist of things to check.

### Managing Functional Failure Consequences

There needs to be a way to state the severity of a functional property not holding so that the auditor can prioritize the importance of the property. Maybe the functional property can be linked to an attack vector as a defense to it, so that the consequence of a statement can be understood. Then, each attack vector can be linked to an invariant, which has a severity.

What is the difference in stating an invariant and an attack vector -- are they inverses? What is the relationship between an invariant/attack vector and a convergence. Does the convergence uphold an invariant, or defend against an attack vector? Answer: Invariants are few and general, attack vectors are many and specific to the implementation. Project documentation informs invariants, and implementation informs attack vectors. An invariant may be that users cannot lose funds, an attack vector may be that funds can be sent to an irrecoverable address, or that funds can be burned -- both invalidate the invariant. I think each topic should have a "threatened by", "subject to", or "targeted by" section that attaches to an attack vector, then each attack vector is attached to an invariant. So, the flow is 1. read the documentation and enstablish protocol design invariants. 2. read the implementation and establish attack vectors. 3. work through functional requirements and establish the attack vector a topic is subject to.

## Implementation Patterns

Keeping note of implementation patterns (or similarly reused logic throughout the codebase) are a good way to determine when logic may be missing or misaligned, and helps show implementation assumptions that may not be in the documentation. It can show assumptions that are completely missing from the documentation, or may highlight the *how* for the documentation's *what*. For example, the documentation may state that each function should emit an event when state changes. If every function except one then keys the event by some ID, then when looking at that function individually, the auditor may not notice that it is missing the event key because it still adheres to the vauge documentation, yet if a pattern had been established that all events are keyed (then it may reveal that the developers knew event keying was important (and that is why they did it everywhere else)), then if the one function without it was checked for adherance to the implementation patterns, then it would have become clear that the event key was missing. Patterns give concise checks against the rest of the codebase without having to pull the rest of the codebase into the current context.

For another example, maybe a codebase always submits some data to an integration, but forgets one place. It is hard to identify when logic is missing, but the identifying the pattern of consistently submitting the data in other places may help highlight where it is missing.

When viewing the expanded context of a named topic is a good time to gather patterns, as you can see in one place how that variable effects the rest of the codebase and can identify any patterns with it. Each topic should have a prompt for checking for new patterns with the context of the subject topic, and for checking that the subjec topic adheres to the implementation patterns.

## Auditing Convergences

Patterns annotate type constraint checks and can be checked by a constraint algorithm. Specification constraint checks are annotated in regular language with business logic and can only be checked by something that understands the specific business logic semantics.

General audit flow is:
 1. Read and understand the docs and the purpose of the project
 2. List out project features, linking each section of the documentation that informs the feature
 3. Brainstorm invariants for each feature, linking them
 4. Read the code, noting what every variable is, is used for, and its type and specification properties, linking them to the feature invariants they uphold and listing the attack vectors they are subject to
 5. Step through all convergences, checking that the properties hold up correctly (types and specifications), and noting the invariants that uphold the attack vectors as functional requirements

### Managing Convergences

Convergences are the main point of verification in the audit process. They have four states: waiting, unverified, verified, and contradicted. A waiting convergence is one is waiting for properties to be added to its parts, ie `a + b`, but `a` does not have any properties added so we cannot judge the convergence.

## Subject Evaluation Context Strategy

When evaluating subjects for invariant upholding or invariant recognition, we use **backward-only context** as the default strategy. When auditing a given subject, the system provides context about where the subject's values originated — their data provenance, taint sources, and upstream transformations — but does not, by default, include forward context about where those values propagate to downstream.

This is a deliberate architectural choice, not a limitation. The system compensates for the absence of forward context through its **on-the-fly invariant tracking and re-check mechanism**, which provides equivalent or superior coverage to bidirectional context while maintaining focused, low-noise audit passes.

### Why Not Bidirectional Context

An intuitive approach to auditing a variable is to present both where its value came from and where it goes. This gives a complete picture of the variable's role in the system. The case for this is strongest when a single value fans out to multiple destinations with different requirements — seeing the full fan-out from one vantage point can surface design-level issues like incomplete access control coverage or inconsistent invariant enforcement across functions.

However, this advantage is narrower than it first appears, for two reasons.

**The inferential demand is the same either way.** Forward context presents the auditor with a list of destinations and relies on the auditor to recognize that a pattern (such as an access control check) is present at some sites and missing at others. Backward-only context presents the auditor with the pattern at the first site encountered and relies on the auditor to generalize it into an invariant. In both cases, the auditor must perform the same act of recognition and generalization. Forward context does not reduce the cognitive work; it merely rearranges when it happens.

**Forward context introduces noise that degrades audit quality.** Forward propagation scales with the number of consumers of a state variable. In smart contracts, heavily-read state variables (token balances, approval mappings, configuration parameters) may be referenced across many functions. Including all downstream consumers in the context of every audit pass dilutes the signal about the specific code under review. For LLM auditors in particular, this context dilution measurably degrades reasoning about the subject at hand. Backward-only context keeps each audit pass lean and focused on the immediate code being evaluated.

### How the Invariant System Closes the Gap

The primary class of vulnerability where forward context provides value is **incomplete invariant enforcement**: cases where a check or pattern should be applied uniformly across a set of functions but is missing from one or more of them. The canonical smart contract examples are access control gaps (a modifier guards some privileged functions but not all) and economic invariant violations (some functions that modify a balance correctly maintain a sum-total relationship while others do not).

Rather than using forward context to surface these issues passively, this system surfaces them actively through invariant tracking:

1. **Invariant discovery.** When the auditor encounters a pattern during a backward-context audit pass (e.g., a role-based access check on a state-modifying function), they register it as an invariant linked to the relevant code.

2. **Propagation via re-check.** The system applies the new invariant to all previously-audited subjects. Any function that modifies the same state but lacks the expected check is flagged mechanically, without requiring forward context at the point of origin.

3. **Completeness guarantee.** Because the audit is programmatic and covers every variable and statement, the re-check mechanism provides exhaustive coverage. Once an invariant is articulated, no violation can escape detection. This is stronger than forward context, which still depends on an auditor noticing a discrepancy within a potentially large list of destinations.

This architecture cleanly separates two concerns that bidirectional context conflates: understanding what a specific piece of code does (served by focused backward context) and verifying that invariants hold globally (served by the re-check system). Each concern is handled by a mechanism optimized for it.

### Known Limitations and Mitigations

The backward-only strategy with invariant re-checking has one residual limitation: invariant discovery depends on the auditor encountering at least one instance of a pattern and recognizing its security significance. Two edge cases deserve attention.

**Subtle semantic distinctions.** Some invariants require recognizing that two superficially similar operations are categorically different. For example, a function that writes to `balances[msg.sender]` (self-modification) versus one that writes to `balances[account]` (arbitrary-address modification) may both appear as simple mapping writes, but only the latter requires elevated privilege. Forward context does not inherently make this distinction more visible — the auditor must apply domain knowledge in either case — but the system should include heuristic prompts that direct attention to these distinctions. Examples include flagging functions that modify state keyed by a caller-supplied address, or functions that bypass standard entry points.

**Absent mechanisms.** If a contract should implement a security pattern (such as a pause mechanism) but does not, no backward-context encounter will contain a reference to the missing pattern. This class of finding is better addressed by a checklist of expected security patterns for the contract type, applied as a design-review pass separate from variable-level auditing. This is an inherent scope boundary of code-level auditing regardless of context strategy.

### Summary

The backward-only context strategy is not a cost-saving approximation of bidirectional analysis. It is a design that optimizes for audit quality by keeping individual audit passes focused, while relying on the invariant system to provide the global coverage that forward context would otherwise supply. The invariant system achieves this coverage more reliably — mechanically and exhaustively — once an invariant is identified, and the investment in invariant discovery heuristics and pattern templates compounds across engagements in a way that raw forward context never does.
