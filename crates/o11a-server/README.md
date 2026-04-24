# o11a-server

The server crate that hosts an o11a audit and exposes it to clients. It wraps the data context produced by `o11a-core` with HTTP and WebSocket endpoints, and runs the collaborator — the layer where human auditors and AI agents work together on the audit.

# Collaborator

The collaborator is what makes the audit a shared workspace rather than a static report. It lets users post comments on any topic in the audit, and it lets AI agents participate as peers — answering questions, reviewing comments, adding properties, and checking properties at convergences.

Every reference-able thing in the audit is a topic with a stable, prefixed identifier:

 - Source code nodes (N)
 - Documentation (D)
 - Comments (C)
 - Features (F)
 - Requirements (R)
 - Threats (T)
 - Invariants (I)
 - Functional Properties (P)
 - Type Constraints (Y)

Topic IDs are sequential within their kind (e.g. `T1`, `T2`). They are the addressing scheme that holds everything in the audit together — comments, AI tasks, and security-model elements all attach to topics, so a single auditor's note and a generated invariant can both live on the same expression and reinforce each other.

## Documentation

Documentation is a first-class citizen in the system, treated with the same interactivity as source code. It is parsed into sections and paragraphs that become topics in their own right, so users can comment on documentation passages and link them to the code they describe.

Relevant documentation is not always shipped with the source code, so the system supports importing external documentation into the audit. Once imported, it is parsed and made interactive in the same way as in-tree docs.

## Discussion Comments

Comments are also first-class citizens. They are parsed in the same way as documentation, and the topics within a comment (sections, references) can themselves be commented on and linked to other topics. A discussion can therefore branch and accumulate context the same way the source code and documentation do, rather than dead-ending in flat comment threads.

## AI Agent Collaboration

The collaborator includes infrastructure for AI agent tasks. Agents act as additional auditors in the system: they review user comments, answer questions, add properties to topics, and check properties at convergences. They participate through the same topic and comment surfaces as human auditors, so their work is visible, reviewable, and correctable in place rather than hidden in a separate pipeline.
