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

## Property Approval

Every property in the security model carries an approval state and a list of approval-comment topics. The collaborator module is responsible for surfacing properties for approval, recording approvals against them, and propagating state changes to dependent convergences and re-checks. See the o11a-core SPEC's "Property Approval" section for the design principles; this section describes the implementation surface.

**Approval is bidirectional and comment-required.** Both human auditors and AI agents approve properties through the same affordance: posting a comment on the property whose role is "approval." The approval comment must contain a rationale — the system rejects empty approvals. This applies symmetrically: a human approving an AI-generated functional purpose must justify why the purpose holds, and an AI agent approving a human-authored invariant must articulate what evidence supports it. Both directions of approval rely on the same comment infrastructure that powers general discussion, so the approval workflow inherits the threading, mention, and topic-resolution features without duplicating them.

**Corrections share the approval surface.** When a reviewer disagrees with a property, the same comment posting workflow records the correction and its reasoning — a correction is structurally an approval of a different property than the one originally generated. The original property is replaced and the comment thread captures both the prior reasoning (from the original generation or approval) and the correction's reasoning. This keeps disagreement diagnosable: any property with a correction in its history shows the chain that led there.

**Agent participation in approval.** AI agents post approval comments through the same agent infrastructure that handles property generation. When an agent approves, the comment carries the agent's identity as author, distinguishing it from human approvals while keeping both visible on the same property. Approvals from agents and humans are independent signals; a property with both human and agent approval has the strongest verification, while a property with only one side's approval is flagged for the other side's review.
