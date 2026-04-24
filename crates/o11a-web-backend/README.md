# o11a-web-backend

The web-facing backend for an o11a audit. It serves the HTML-returning endpoints of the audit UI and owns the formatter — the layer that turns parsed AST nodes from `o11a-core` into the rendered code views that the client displays.

# Formatter

The formatter takes a node from the parser plus the analyzer's data context and produces an HTML rendering of that node.

It exists because the audit interface is not a traditional source viewer. The system displays many small code snippets side-by-side, each annotated with inline discussion. To make that work, the formatter renders code aggressively vertically and to a fixed forty-character width — narrow enough that four columns of code fit comfortably on a screen and two columns fit on standard paper, but wide enough that inline annotations remain readable.

Identifiers and operators are first-class and always live on their own line, because each one is its own discussion topic that may carry inline comments. This vertical-by-default layout is what lets the audit UI annotate any expression without the formatter and the client having to negotiate over whitespace.

The formatter renders nodes either as source text (how a node would appear in the source file, e.g., a function with its body) or as signatures (how a node should be presented in isolation inside discussions or modals, e.g., a function without its body). This distinction lets the same node appear differently depending on context without the caller doing any work.

Traditional line numbers are not used, because the formatter changes the surface text and the API hands clients many small snippets rather than whole files. Where a gutter is shown, it carries operation numbers instead.
