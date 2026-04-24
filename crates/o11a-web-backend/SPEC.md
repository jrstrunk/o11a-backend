# o11a-web-backend SPEC

This document is the implementation spec for the formatter. The README covers what the formatter does and why; this document covers how it is structured. Read the README first.

# Formatter

The forty-character width is implicit, not enforced. It falls out of the rule that all nodes are always formatted in the most vertical way possible. A variable name or literal value can overflow the forty-character limit, and that is unavoidable.

There are two core primitives the formatter respects: identifiers and operators. Each identifier and each operator has a dedicated topic for discussion, so each must be on a different line so that its comments can be displayed inline above it. Identifier and operator inline comments are formatted differently, so the same line can contain both an identifier and an operator, but it cannot contain two identifiers or two operators.

There is only one way to format each expression because the per-line formatting rules are strict. This makes the formatter output very vertical but straightforward to implement.

When any declaration/reference, or operator line is rendered to HTML, it is preceded by an empty span element. Clients inject info comments into that span dynamically. Because the code width is always set to 40 characters, formatting the inline comments to be injected into the HTML is straightforward as well.

The formatter output does not include traditional line numbers because the formatter is aggressive in changing the source text, and the API is designed to enable clients to display many smaller snippets of code. Clients are not expected to show complete source files in regular use, so the original line numbers are not particularly meaningful. Where a gutter is shown, it carries operation numbers instead.

Although the complete source code of a file is not used in regular use, clients may want to display or allow copying the full source file in a separate view, at the user's request, for niche purposes. It should not be interactive.

The formatter can format nodes as source text or as signatures. Source text is how the node would appear in the source file; a signature is how the node should be represented in an isolated way, nested inside discussions or modals. For example, the source text of a function contains the function's body, but the signature does not. Source text for variables does not include type information, but signatures do. Text is rendered the same either way.
