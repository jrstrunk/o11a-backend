# Refactor: Unified Conversation Entry Rendering with Structured Output

## Overview

This document describes a refactor of the conversation entry rendering pipeline in the `o11a-web-backend` crate. The goals are:

1. **Unify all conversation entry types** (comments, mentions, requirements, behaviors, functional semantics) through a single rendering function that produces a structured intermediate representation.
2. **Expand thread support** to all entry types — not just comments. Every conversation entry can now have reply-thread children.
3. **Provide stripped-down inline HTML** for trusted entities (FunctionalSemantics only) so the frontend can inject them inline beside code without container styling or headers.
4. **Refactor `render_source_text`** to be a pure body renderer (no header, no container wrapping), making it the single source of truth for body content.

There are no external consumers yet. The API shape can change freely.

## File Layout

All changes are in:
- `crates/o11a-web-backend/src/topic_view.rs` — main file, contains response types, rendering functions, and the conversation builder
- `crates/o11a-web-backend/src/formatting.rs` — low-level HTML formatting helpers (no changes expected)
- `crates/o11a-web-backend/src/comment_formatter.rs` — comment AST to HTML rendering (no changes expected)
- `crates/o11a-web-backend/src/handlers.rs` — HTTP handlers (minor changes to `/source_text` handler)

## Current Architecture (what exists today)

### Response types

```rust
// topic_view.rs
pub struct ConversationResponse {
    pub entries: Vec<ConversationEntry>,
}

pub struct ConversationEntry {
    pub topic_id: String,
    pub kind: ConversationEntryKind,
    pub created_at: Option<String>,
    pub html: String,  // monolithic blob: container + header + body + thread children
}

pub enum ConversationEntryKind {
    FunctionalSemantics,
    Behavior,
    Requirement,
    Comment,
    Mention,
}
```

### Three independent rendering paths

**Path A — Generated topics** (requirements, behaviors, functional semantics):
- Function: `build_generated_conversation_entry`
- Output: `<div class="requirement" data-topic="..." style="COMBINED_PANEL_STYLE"> <header> <p>description</p> </div>`
- No thread children support.

**Path B — Comments and mentions (comment topics)**:
- Function: `build_comment_thread_html` → `render_comment_node` (flat list with depth)
- Output: A chain of `<div class="comment-thread-node" data-topic="..." style="COMBINED_PANEL_STYLE"> <header> <div class="comment-content code-style">body</div> </div>` — one per node in the thread (root + recursive children).
- Has thread children support via `collect_children_recursive`.

**Path C — Mentions (non-comment topics)**:
- Function: `render_topic_node`
- Output: `<div class="conversation-node" data-topic="..." style="COMBINED_PANEL_STYLE"> [optional header] <div class="comment-content">body</div> </div>`
- No thread children support.

### `render_source_text` — current behavior

Currently produces complete HTML including headers and containers:

- **Authored topics** (Feature, Requirement, Behavior, FunctionalSemantic, Threat, Invariant): renders `render_authored_header` + `<p>description</p>`, wraps in `formatting::format_topic_block` which produces `<div class="topic-token {css_class}" data-node-topic="..." data-topic="..." tabindex="0">...</div>`.
- **Comments**: calls `comment_formatter::render_comment_html` which wraps parsed comment nodes in `formatting::format_topic_block` with class `"comment-root target-topic"`.
- **Solidity/Documentation**: renders from AST nodes, already produces its own HTML with topic tokens.
- **Global builtins**: renders from the global lookup.

`render_source_text` is called from:
1. The `/source_text` HTTP handler (produces source text for a topic)
2. Internal `get_source_text` (used by reference rendering in source panels, and by conversation entry rendering)

### Comment thread children

Currently, comment children are found by scanning `audit_data.topic_metadata.values()` for entries whose `target_topic()` equals the parent topic. This is done in `collect_children_recursive`. Only comment-type topics are collected as children.

## Target Architecture

### New `RenderedEntry` intermediate

A structured tree that all conversation entry types produce:

```rust
/// Structured rendering of a single conversation entry node.
/// All entry types (comments, requirements, behaviors, functional semantics, mentions)
/// produce one of these. Thread children are represented as a recursive tree.
pub struct RenderedEntry {
    /// The topic ID of this entry.
    pub topic_id: String,
    /// The conversation entry kind.
    pub kind: ConversationEntryKind,
    /// The rendered header HTML (meta div with keyword/type, author, timestamp).
    /// Empty string if no header (should not happen in practice for conversation entries).
    pub header_html: String,
    /// The rendered body HTML: just the content with `data-topic` attributes preserved.
    /// For authored topics: a `format_topic_block` wrapper containing the description.
    /// For comments: a `format_topic_block` wrapper containing the parsed comment nodes.
    /// This is what gets used for `inline_html` on trusted entities.
    pub body_html: String,
    /// Creation timestamp from metadata.
    pub created_at: Option<String>,
    /// Recursive thread children (replies to this entry).
    pub children: Vec<RenderedEntry>,
}
```

### New `ConversationEntry` response

```rust
#[derive(Debug, Clone, Serialize)]
pub struct ConversationEntry {
    pub topic_id: String,
    pub kind: ConversationEntryKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Full thread HTML: container + header + body + thread children.
    pub html: String,
    /// Stripped-down body HTML for inline injection.
    /// Only present for trusted entity kinds (FunctionalSemantics).
    /// Contains just the body_html from the RenderedEntry — no header,
    /// no container styling, no thread children.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline_html: Option<String>,
}
```

### `render_source_text` refactored — pure body renderer

`render_source_text` becomes a **body-only renderer**. It no longer:
- Renders the `render_authored_header` header
- Wraps authored topic content in `formatting::format_topic_block`

Instead, it produces just the body content:

- **Authored topics**: renders just the description via `comment_formatter::render_description_html`, then wraps it in a plain unstyled `<div>` with a `data-topic` attribute (no class, no tabindex, no styling — just a selectable container). This is the same pattern as `format_topic_block` but stripped down:
  ```html
  <div data-topic="..."><p style="margin: 0">...rendered description...</p></div>
  ```
- **Comments**: unchanged — still calls `comment_formatter::render_comment_html` which wraps in `format_topic_block`. This already produces a `data-topic` wrapper.
- **Solidity/Documentation**: unchanged — renders from AST nodes as before.

The callers that previously relied on `render_source_text` including the header/container must now compose their own:

1. **`/source_text` handler**: wraps the body in a container + header using a new helper function `render_source_text_as_block`. This function checks if the topic is authored and, if so, adds the header and wraps in `format_topic_block`.
2. **Conversation pipeline**: uses `render_source_text` for the body, and composes header + body + children into the full `html` and optional `inline_html`.
3. **Source panel rendering** (reference rendering, inline comments): uses `render_source_text` for the body as before, but these paths never needed the authored header anyway (they render source code, not conversation content).

### Unified rendering function

A single function replaces `build_generated_conversation_entry`, `build_comment_thread_html`, and `render_topic_node`:

```rust
/// Render a single conversation entry as a RenderedEntry tree.
/// Handles all entry types: comments, mentions (always comments), requirements,
/// behaviors, and functional semantics.
/// Recursively collects thread children for all types.
fn render_entry_tree(
    entry_topic: &topic::Topic,
    kind: ConversationEntryKind,
    audit_data: &AuditData,
    source_text_cache: &mut HashMap<String, String>,
) -> Option<RenderedEntry>
```

This function:
1. Looks up metadata for `entry_topic`.
2. Determines the header:
   - **Comments**: renders header with comment type keyword (note/info/question/etc.), author, timestamp — same as current `render_comment_node` header.
   - **Mentions (comments)**: same as comments — mentions are always comments.
   - **Requirements**: renders header with "req" keyword, author, timestamp.
   - **Behaviors**: renders header with "behavior" keyword, author, timestamp.
   - **FunctionalSemantics**: renders header with "semantics" keyword, author, timestamp.
3. Gets the body via `render_source_text` (the refactored body-only version).
4. Recursively collects thread children by looking up `audit_data.comment_index[entry_topic]` (for comments on this entry) and scanning `topic_metadata` for replies. Children are always comment-type entries.
5. Returns a `RenderedEntry` with header, body, created_at, and children.

### Composing `html` from `RenderedEntry`

```rust
/// Compose a full HTML string from a RenderedEntry tree.
/// Renders: container div (with COMBINED_PANEL_STYLE) > header + body + children.
/// Children are recursively rendered inside the container with indentation.
fn render_entry_html(entry: &RenderedEntry, index: usize, total: usize, depth: usize) -> String
```

This replaces both `render_comment_node` and the inline HTML construction in `build_generated_conversation_entry`.

The structure for each node:

```html
<div class="{css_class}" data-topic="{topic_id}" style="COMBINED_PANEL_STYLE [first/last styles]">
  {header_html}
  <div class="comment-content [code-style if comment]">{body_html}</div>
  {for each child: render_entry_html(child, child_index, children.len(), depth+1)}
</div>
```

CSS class on the outer div:
- Comments and mentions: `"comment-thread-node"`
- Requirements: `"requirement"`
- Behaviors: `"behavior"`
- FunctionalSemantics: `"functional-semantics"`

The `first_last_style` is applied based on `index` and `total` at each depth level, matching current behavior.

### `inline_html` for trusted entities

For `ConversationEntryKind::FunctionalSemantics` only, `inline_html` is set to the `body_html` from the root `RenderedEntry`. This is the `render_source_text` output — a `<div data-topic="...">` wrapping the rendered description with `data-topic` spans for clickable tokens. No header, no container borders, no thread children.

### `build_conversation` — updated flow

```rust
pub fn build_conversation(
    topic_id: &str,
    audit_data: &AuditData,
    source_text_cache: &mut HashMap<String, String>,
) -> Option<ConversationResponse>
```

1. Resolve the topic through the transitive chain (as today).
2. Collect functional semantics, behaviors, and requirements via reverse indexes (as today).
3. Collect direct comments from `comment_index` (as today).
4. Collect mentioning comments from `mentions_index` (as today).
5. For each collected topic, call `render_entry_tree` to produce a `RenderedEntry`.
6. For each `RenderedEntry`:
   - Compute `html` via `render_entry_html(entry, 0, 1, 0)`.
   - If kind is `FunctionalSemantics`, set `inline_html = Some(entry.body_html.clone())`.
   - Build `ConversationEntry` from the result.

### `build_thread` — updated flow

Same as today conceptually, but uses `render_entry_tree` + `render_entry_html` instead of the old path-specific functions.

### `build_topic_panel_prefix` — updated flow

Currently uses `render_comment_node` directly. Update to use `render_entry_tree` + `render_entry_html`.

## Detailed Changes by Function

### Functions to remove

- `build_generated_conversation_entry` — replaced by `render_entry_tree`
- `build_comment_thread_html` — replaced by `render_entry_tree` + `render_entry_html`
- `render_comment_node` — replaced by `render_entry_html`
- `render_topic_node` — replaced by `render_entry_html`
- `collect_children_recursive` — replaced by tree-based child collection in `render_entry_tree`

### Functions to modify

| Function | Change |
|---|---|
| `render_source_text` | Remove authored header rendering and `format_topic_block` wrapping. For authored topics, return just the description wrapped in an unstyled `<div data-topic="...">`. Add a new helper `render_body_html` for this. |
| `build_conversation` | Use `render_entry_tree` for all entries. Compute `html` and `inline_html` from the `RenderedEntry`. |
| `build_conversation_entry` | Replaced by `render_entry_tree`. |
| `build_thread` | Use `render_entry_tree` + `render_entry_html`. |
| `build_topic_panel_prefix` | Use `render_entry_tree` + `render_entry_html` for comment parent chains and requirement/threat/invariant nodes. |
| `get_source_text` (handler in `handlers.rs`) | Call a new wrapper function `render_source_text_as_block` that adds back the header + `format_topic_block` wrapping for authored topics, so the `/source_text` endpoint output remains correct. |

### Functions to add

| Function | Purpose |
|---|---|
| `render_entry_tree` | Unified entry renderer producing `RenderedEntry` tree (described above). |
| `render_entry_html` | Composes full HTML from a `RenderedEntry` tree (described above). |
| `render_entry_header` | Extracts the per-type header HTML. Replaces both `render_authored_header` (for generated types) and the inline header construction in `render_comment_node`. |
| `render_body_html` | New helper called by `render_source_text` for authored topics. Produces `<div data-topic="..."><p>description</p></div>`. |
| `render_source_text_as_block` | Wraps `render_source_text` output with header + `format_topic_block` for the `/source_text` endpoint. Only adds header/wrapper for authored topics; passes through other types unchanged. |
| `collect_thread_children` | New function to find comment children for any topic (not just comment topics). Looks up `comment_index` and scans `topic_metadata` for replies, producing `Vec<RenderedEntry>`. |

### Types to modify

| Type | Change |
|---|---|
| `ConversationEntry` | Add `inline_html: Option<String>`. Keep `html` as the full thread rendering. |
| `RenderedEntry` | New struct (described above). |

## Thread Children Collection

The new `collect_thread_children` function collects children for **any** conversation entry, not just comments:

1. Look up `audit_data.comment_index[entry_topic]` for direct comments on the entry.
2. Also scan `audit_data.topic_metadata.values()` for topics whose `target_topic()` equals `entry_topic` (existing behavior for comment threads).
3. Merge and deduplicate.
4. For each child, call `render_entry_tree` with `kind: Comment` (children are always comments).
5. Children recurse — a child comment can have its own children.

This means:
- A **requirement** entry can have **comment** children (replies discussing the requirement).
- A **behavior** entry can have **comment** children.
- A **functional semantics** entry can have **comment** children.
- A **comment** entry can have **comment** children (existing behavior).
- Thread children are never requirements/behaviors/semantics themselves — only comments.

## Header Rendering per Type

The new `render_entry_header` function:

| Entry kind | Header format |
|---|---|
| Comment | `<div style="COMMENT_META_STYLE"><span class="comment-type keyword">{type}</span> <span class="comment-author">author:{id}</span> <span class="comment-time">{timestamp}</span></div>` |
| Mention | Same as Comment (mentions are always comments) |
| Requirement | `<div style="display:flex; gap:0.5rem; align-items:center; margin-bottom:0.25rem;"><span class="keyword">req</span> <span class="comment-author" style="font-size:0.8em; opacity:0.7;">author:{id}</span> <span class="comment-time" style="font-size:0.8em; opacity:0.7;">{timestamp}</span></div>` |
| Behavior | Same structure as Requirement, with keyword `"behavior"` |
| FunctionalSemantics | Same structure as Requirement, with keyword `"semantics"` |

This consolidates `render_authored_header` (used for generated topics) and the inline header in `render_comment_node` into one function. The visual output should remain identical.

## Body HTML per Type

| Entry kind | Body source |
|---|---|
| Comment | `render_source_text(entry_topic, ...)` — which calls `comment_formatter::render_comment_html`, producing a `format_topic_block` wrapper with the parsed comment nodes |
| Mention | Same as Comment |
| Requirement | `render_source_text(entry_topic, ...)` — which for authored topics now returns `<div data-topic="..."><p>rendered description</p></div>` |
| Behavior | Same as Requirement |
| FunctionalSemantics | Same as Requirement |

## Inline HTML for Trusted Entities

Only `ConversationEntryKind::FunctionalSemantics` gets `inline_html`.

`inline_html` = the `body_html` from the root `RenderedEntry`. This is exactly what `render_source_text` produces: an unstyled `<div data-topic="...">` containing the rendered description with clickable `data-topic` token spans. The frontend can inject this into a placeholder div beside the code.

No header, no container borders, no thread children.

## `render_source_text` Refactor Details

### Before (current)

```rust
pub fn render_source_text(topic, audit_data) -> Option<String> {
    // Authored topics: header + description wrapped in format_topic_block
    if authored_topic_label(metadata).is_some() {
        let header = render_authored_header(...);
        let desc_html = render_description_html(...);
        let content = format!("{header}<p>{desc_html}</p>");
        return Some(format_topic_block(topic, content, css_class, topic));
    }
    // Comments: render_comment_html (also wraps in format_topic_block)
    // Solidity/Documentation: render from AST
}
```

### After (refactored)

```rust
pub fn render_source_text(topic, audit_data) -> Option<String> {
    // Authored topics: just the body, no header, minimal wrapper
    if authored_topic_label(metadata).is_some() {
        let desc_html = render_description_html(...);
        return Some(format!(
            "<div data-topic=\"{}\"><p style=\"margin: 0\">{}</p></div>",
            html_escape(topic.id()),
            desc_html,
        ));
    }
    // Comments: unchanged — render_comment_html already produces appropriate body
    // Solidity/Documentation: unchanged — render from AST as before
    // Global builtins: unchanged
}
```

The key change: authored topics no longer include `render_authored_header` and no longer use `format_topic_block`. They get a plain `<div data-topic="...">` wrapper so the body content is selectable as a whole (matching the comment pattern where `render_comment_html` wraps content in `format_topic_block`).

### New `render_source_text_as_block`

For the `/source_text` endpoint and other callers that need the full block with header:

```rust
/// Wraps render_source_text output with a header and format_topic_block
/// for authored topics. Non-authored topics pass through unchanged.
fn render_source_text_as_block(topic, audit_data, source_text_cache) -> String {
    let body = get_source_text(topic, audit_data, source_text_cache);
    if let Some(metadata) = audit_data.topic_metadata.get(topic)
        && let Some((keyword, css_class)) = authored_topic_label(metadata)
    {
        let header = render_entry_header(...);
        let content = format!("{header}{body}");
        return format_topic_block(topic, &content, css_class, topic);
    }
    body
}
```

### Impact on other callers of `get_source_text`

`get_source_text` is called in:
1. **`render_reference_source`** (source panel rendering) — this renders source code in the expanded references panel. Authored topics appearing here get their body rendered. Since we removed the header from `render_source_text`, these won't show a duplicate header. This is correct — the source panel shows code, not conversation headers.
2. **`render_comment_node`** — being removed.
3. **`render_topic_node`** — being removed.
4. **`build_topic_panel_prefix`** — being refactored to use `render_entry_tree`.

The only place that needs the full header + `format_topic_block` wrapping is the `/source_text` endpoint. Everything else either uses the body directly or composes its own header via the conversation pipeline.

## Implementation Order

1. **Add `RenderedEntry` struct** — no behavior change yet.
2. **Refactor `render_source_text`** — remove header and `format_topic_block` from authored topics, add plain `<div data-topic>` wrapper.
3. **Add `render_source_text_as_block`** — wraps body with header + `format_topic_block` for authored topics. Used by `/source_text` handler.
4. **Update `/source_text` handler** — call `render_source_text_as_block` instead of `render_source_text` directly.
5. **Add `render_entry_header`** — unified header function replacing `render_authored_header` and the inline header in `render_comment_node`.
6. **Add `collect_thread_children`** — finds comment children for any topic.
7. **Add `render_entry_tree`** — unified tree renderer using `render_source_text` for body, `render_entry_header` for header, `collect_thread_children` for children.
8. **Add `render_entry_html`** — composes full HTML from `RenderedEntry` tree.
9. **Update `build_conversation`** — use `render_entry_tree` + `render_entry_html`. Add `inline_html` to `ConversationEntry`.
10. **Update `build_thread`** — use `render_entry_tree` + `render_entry_html`.
11. **Update `build_topic_panel_prefix`** — use `render_entry_tree` + `render_entry_html`.
12. **Remove old functions** — `build_generated_conversation_entry`, `build_comment_thread_html`, `render_comment_node`, `render_topic_node`, `collect_children_recursive`, `build_conversation_entry`.
13. **Remove `render_authored_header`** — replaced by `render_entry_header`.

## Testing Strategy

After each step, verify:
- `/source_text` endpoint returns correct HTML for authored topics (with header) and non-authored topics (unchanged).
- `/conversation` endpoint returns entries with correct `html` (full thread) and `inline_html` (for FunctionalSemantics only).
- `/thread` endpoint returns correct thread HTML.
- Thread children appear for all entry types, not just comments.
- The visual appearance of the frontend should not change (same CSS classes, same structure, same inline styles).

## Key Design Principles

- **Single source of truth for body content**: `render_source_text` is the only function that renders a topic's body. All paths (source text endpoint, conversation entries, thread endpoint) use it.
- **Separation of header and body**: Headers are composed by callers, not baked into the body renderer.
- **Tree structure**: `RenderedEntry` uses recursive `children` rather than a flat list with depth integers.
- **Unified rendering**: One function (`render_entry_tree`) handles all entry types. The kind-specific logic (header format, CSS class) is parameterized.
- **Minimal `inline_html`**: Only trusted entities (FunctionalSemantics) get inline HTML. It contains only the body with `data-topic` attributes — no header, no container, no children.
