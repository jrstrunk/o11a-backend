# Frontend Migration Guide

This guide describes the API changes a client must adopt to work with the
new `o11a-server`. The changes consolidate an architectural shift: the
pipeline now runs as a separate binary (`o11a-analyze`) that writes
`audit.json`; the server loads that file and serves collaboration data on
top. User-created and user-triggered-agent content is additive; nothing is
editable after creation, only hidable (hide is not yet implemented).

These are breaking changes. We are in alpha — there is no deprecation
window. Upgrade all calls in one pass.

---

## Summary of changes

| Area | Change kind | Section |
|------|-------------|---------|
| Pipeline HTTP triggers | **Removed** | [1](#1-pipeline-trigger-endpoints-removed) |
| User entity creation | **New** | [2](#2-new-user-entity-creation-endpoints) |
| Path ID format | **Breaking** | [3](#3-path-parameters-require-prefixed-topic-ids) |
| `/requirements/topic/:id` | **Moved** | [4](#4-path-shape-cleanup) |
| `/conditions/id/:id` | **Renamed** | [4](#4-path-shape-cleanup) |
| `/subjects/:id/semantics` | **Renamed** | [4](#4-path-shape-cleanup) |
| WebSocket URL + event types | **Renamed** | [5](#5-websocket-url-and-event-type-renames) |
| `ConversationUpdated.entry` | **Removed from payload** | [5](#5-websocket-url-and-event-type-renames) |
| `/delimiter/:id` response | **Shape changed** | [6](#6-delimiter-endpoint-returns-structured-data) |
| Pipeline entity provenance | **Shape changed** | [7](#7-pipeline-entities-author_id-semantics-and-optional-created_at) |

---

## 1. Pipeline-trigger endpoints removed

**What changed:** The server no longer runs the analysis pipeline on
demand. Pipeline output comes from `audit.json` written by the separate
`o11a-analyze` binary.

**Endpoints removed (all POST):**

- `/api/v1/audits/:audit_id/analyze`
- `/api/v1/audits/:audit_id/pipeline/semantic_links`
- `/api/v1/audits/:audit_id/pipeline/requirements`
- `/api/v1/audits/:audit_id/pipeline/behaviors`
- `/api/v1/audits/:audit_id/pipeline/synthesize`

**Frontend action:** Remove any UI that triggered these (e.g. an
"Analyze" or "Re-run pipeline" button). Analysis is now a CI/offline
step. A subsequent release will introduce per-task refine endpoints for
user-triggered agent work — those will be new URLs with different
semantics, not revivals of the old ones.

---

## 2. New user-entity creation endpoints

**What changed:** Users (and user-triggered agent runs) can now create
the same kinds of entities the pipeline produces. Creation uses POST on
the kind's resource URL. No `/user/` infix — pipeline and user entities
share one resource and are distinguished only by `author_id`.

**New endpoints:**

| Method | Path | Creates |
|--------|------|---------|
| POST | `/api/v1/audits/:audit_id/features` | A user feature |
| POST | `/api/v1/audits/:audit_id/requirements` | A user requirement |
| POST | `/api/v1/audits/:audit_id/behaviors` | A user behavior |
| POST | `/api/v1/audits/:audit_id/functional_semantics` | A user functional semantic |

**Request bodies:**

```jsonc
// POST /features
{
  "name": "Staking reward distribution",
  "description": "Reward accrual rules for stakers.",
  "author_id": 42,
  "requirement_topics": ["R3", "R7"],   // optional
  "behavior_topics": ["B12"]             // optional
}

// POST /requirements
{
  "description": "Only the admin can pause the contract.",
  "section_topic": "D14",
  "author_id": 42,
  "documentation_topics": ["D14", "D15"]  // optional
}

// POST /behaviors
{
  "description": "Reverts with PAIR_EXISTS when pair is already set.",
  "member_topic": "N1234",
  "author_id": 42
}

// POST /functional_semantics
{
  "description": "Participation identifier",
  "declaration_topic": "N456",
  "author_id": 42,
  "documentation_topics": ["D20"]
}
```

**Response:**

```json
{ "topic_id": "F10042" }
```

The returned `topic_id` is server-allocated above the max pipeline ID,
so it won't collide with anything in `audit.json`.

**Reads are unchanged in shape, but the data is unified.** The existing
GET endpoints now return pipeline-produced *and* user-created entries
together:

- `GET /api/v1/audits/:audit_id/features`
- `GET /api/v1/audits/:audit_id/requirements`
- `GET /api/v1/audits/:audit_id/behaviors`
- `GET /api/v1/audits/:audit_id/functional_semantics`

Use `author_id` on each item to tell origin:
- `1` = system, `2` = dev technical, `3` = dev documentation,
  `4–7` = agent tiers (pipeline); `≥ 8` = user.

---

## 3. Path parameters require prefixed topic IDs

**What changed:** Every URL path parameter that identifies a topic now
requires the prefixed form (`F42`, `R7`, `B13`, `P99`, `N-100`, `D34`,
`C2`, `I4`, `T1`). The server used to accept bare numeric IDs (`42`) as
a legacy fallback; that is now rejected with `400 Bad Request`.

**Affected endpoints** (path segment changed from `:feature_id` /
`:requirement_id` / `:behavior_id` / etc. to `:topic_id`):

- `/features/:topic_id`
- `/requirements/:topic_id`
- `/behaviors/:topic_id`
- `/threats/:topic_id`
- `/invariants/:topic_id`
- `/conditions/:topic_id` *(see also §4)*
- `/threats/:topic_id/invariants`
- `/threats/:topic_id/invariants/:invariant_topic_id`
- `/invariants/:topic_id/source_topics`
- `/invariants/:topic_id/source_topics/:source_topic_id`

For **comment** IDs in paths (`/comments/:comment_id/status`,
`/votes/:comment_id`): the preferred form is now the prefixed `C42`.
Request/response *bodies* that carry a comment ID (e.g., the
`comment_topic_id` field in responses) are unchanged — they've always
used the prefixed form.

**Frontend action:** If your client ever builds URLs from a bare numeric
ID (e.g. from a database column), prefix it at the seam:
`` `F${featureId}` ``. The server no longer does this for you.

---

## 4. Path-shape cleanup

Three routes were renamed or restructured for consistency.

### 4a. Filtering requirements by related topic

- **Old:** `GET /api/v1/audits/:audit_id/requirements/topic/:topic_id`
- **New:** `GET /api/v1/audits/:audit_id/requirements?for_topic=:topic_id`

```diff
- GET /api/v1/audits/alpha/requirements/topic/N42
+ GET /api/v1/audits/alpha/requirements?for_topic=N42
```

Response shape is unchanged.

### 4b. Deleting a condition

- **Old:** `DELETE /api/v1/audits/:audit_id/conditions/id/:condition_id`
- **New:** `DELETE /api/v1/audits/:audit_id/conditions/:condition_id`

The redundant `/id/` segment is gone. Note that the topic-ID rule from
§3 applies: pass the prefixed ID.

### 4c. Functional semantics by topic

- **Old:** `GET /api/v1/audits/:audit_id/subjects/:topic_id/semantics`
- **New:** `GET /api/v1/audits/:audit_id/topics/:topic_id/semantics`

Response shape is unchanged. The vocabulary now says "topic" uniformly
— "subject" was vestigial.

---

## 5. WebSocket URL and event type renames

The real-time stream carries more than comment activity (vote updates,
status transitions, and future kinds), so both the URL and the event
type are renamed to reflect the broader scope.

### 5a. URL

- **Old:** `/api/v1/audits/:audit_id/comments/ws`
- **New:** `/api/v1/audits/:audit_id/events/ws`

### 5b. Event envelope

The `type` discriminator values change, and one variant's payload
shrinks:

| Old `type` | New `type` | Payload change |
|------------|------------|----------------|
| `conversation_updated` | `topic_updated` | No `entry` field — see below |
| `status_updated` | `status_updated` | unchanged |
| `vote_updated` | `vote_updated` | unchanged |

If your client has a TypeScript/Gleam type for the envelope, rename it
from `CommentEvent` to `AuditEvent`.

### 5c. `ConversationUpdated` → `TopicUpdated` payload

This is the one whose payload shrank. Old shape (for reference):

```jsonc
// OLD — do not rely on this
{
  "type": "conversation_updated",
  "audit_id": "alpha",
  "topic_id": "N42",
  "entry": { /* pre-rendered HTML conversation entry */ },
  "invalidated_thread_ids": ["C7"]
}
```

New shape:

```jsonc
{
  "type": "topic_updated",
  "audit_id": "alpha",
  "topic_id": "N42",
  "comment_topic_id": "C100",
  "invalidated_thread_ids": ["C7"]   // optional, empty array omitted
}
```

**Migration pattern** (the important behavioral change): you no longer
receive the rendered entry inline. On receipt of `topic_updated`:

1. Refetch the conversation for `topic_id`:
   `GET /api/v1/audits/:audit_id/conversation/:topic_id`
2. For each id in `invalidated_thread_ids`, refetch:
   `GET /api/v1/audits/:audit_id/thread/:id`

This costs one extra HTTP round-trip per event; it removed HTML from
the event payload, which lets the rendering layer evolve without
touching the event protocol.

---

## 6. Delimiter endpoint returns structured data

`GET /api/v1/audits/:audit_id/delimiter/:topic_id` used to return an
object with HTML strings for opening/closing delimiter markup. It now
returns structured data and leaves the rendering to the client (or to
the HTML-server's own internal renderer, which does the same).

**Old response** (deprecated):

```jsonc
// OLD — do not rely on this
{
  "opening": "<pre><code>...<span>if</span> (...) {</code></pre>",
  "closing": "<pre><code>}</code></pre>"
}
```

**New response:**

```json
{
  "kind": "if",
  "node_topic": "N123",
  "condition_topic": "N456"
}
```

Or `null` if the topic is not a control-flow node with delimiters.

`kind` is one of `"if"`, `"for"`, `"while"`, `"do_while"`. To render
the header, fetch source text for the `condition_topic` via
`GET /api/v1/audits/:audit_id/source_text/:topic_id` and wrap it in
the appropriate language-level delimiters.

---

## 7. Pipeline entities: `author_id` semantics and optional `created_at`

**What changed:** Pipeline-produced features, requirements, behaviors,
and functional semantics no longer carry per-entity `author_id` or
`created_at` values. The JSON report (`audit.json`) omits them, and
the server fills them in at parse time with synthetic defaults:

- `author_id = 1` (`AUTHOR_SYSTEM`) on every pipeline entity.
- `created_at` is **omitted from the response** (treat as `null` /
  `undefined` in client types).

User-created and server-agent-created entries continue to carry their
own `author_id` (the creator) and `created_at` (the moment of
creation). The top-level `generated_at` field on `audit.json` remains
and tells you when the audit was analyzed — surface that once at the
audit level if you need to display "analyzed on …".

**Consequence for clients:**

- `author_id = 1` now means "from the analyze batch." Previously
  pipeline entities reported `author_id = 7` (`AUTHOR_AGENT_LARGE`) —
  the pipeline no longer uses that value. Values `4–7` now indicate
  *interactive* agent runs (server-triggered, post-analysis), not
  batch pipeline output.
- `created_at` on features / requirements / behaviors / functional
  semantics is now optional. Type it as `string | undefined` (TS) or
  `Option(String)` (Gleam). UI that assumed it was always present
  must fall back gracefully.

**Response shape change (illustrative):**

```diff
  {
    "topic_id": "F42",
    "name": "Staking reward distribution",
    "description": "...",
-   "author_id": 7,
-   "created_at": "2026-04-24T10:00:00Z"
+   "author_id": 1
+   // created_at omitted from pipeline entities
  }
```

For user-created entities the shape is unchanged from §2 — both
fields are present:

```json
{
  "topic_id": "F10042",
  "name": "...",
  "description": "...",
  "author_id": 42,
  "created_at": "2026-04-24T15:30:00Z"
}
```

**Frontend action:**

1. Update type definitions so `created_at` is optional on
   `Feature`, `Requirement`, `Behavior`, and `FunctionalSemantic`
   responses.
2. If any UI branched on `author_id === 7` to mean "produced by the
   pipeline," switch to `author_id === 1`.
3. Handle the absence of `created_at` at the render site — pipeline
   entities come through without a time value, and the frontend decides
   what (if anything) to show in its place.

---

## 8. Unaffected endpoints

The following endpoints' *shapes* are unchanged (though several now
serve merged pipeline+user data as noted in §2):

- `GET /health`
- `GET/POST/DELETE /api/v1/audits[/...]`
- `GET /api/v1/audits/:audit_id/data-context`
- `GET /api/v1/audits/:audit_id/boundaries`
- `GET /api/v1/audits/:audit_id/in_scope_files`
- `GET /api/v1/audits/:audit_id/contracts`
- `GET /api/v1/audits/:audit_id/qualified_names`
- `GET /api/v1/audits/:audit_id/documents`
- `GET /api/v1/audits/:audit_id/metadata/:topic_id`
- `GET /api/v1/audits/:audit_id/agent_context/:topic_id`
- `GET /api/v1/audits/:audit_id/source_text/:topic_id` *(HTML)*
- `GET /api/v1/audits/:audit_id/topic_view/:topic_id` *(HTML-in-JSON)*
- `GET /api/v1/audits/:audit_id/conversation/:topic_id` *(HTML-in-JSON)*
- `GET /api/v1/audits/:audit_id/thread/:topic_id` *(HTML)*
- `POST /api/v1/audits/:audit_id/documentation` *(HTML)*
- Comment CRUD and status: `/comments[/...]`
- Votes: `/votes/...`
- Chats: `/chats`, `/chats/...`
- Features: `GET /features`, `GET /features/:topic_id`,
  `GET /features/:topic_id/requirements`
- Threats/Invariants/Conditions routes (aside from §4b for conditions)
- Impact analysis: `POST /impact_analysis`,
  `DELETE /impact_analysis/:threat_topic/:feature_topic`

The HTML-returning endpoints (marked *HTML* / *HTML-in-JSON*) will
eventually be replaced by a Gleam client-side renderer consuming only
the JSON endpoints, but nothing in this migration changes their
current shape.

---

## 9. Migration checklist

Run through this list in order:

1. [ ] Remove any UI that hits `/analyze` or `/pipeline/*`. (§1)
2. [ ] Add user-entity creation UI if applicable; POST to
       `/features`, `/requirements`, `/behaviors`,
       `/functional_semantics`. (§2)
3. [ ] Audit your URL-building code for bare numeric IDs; prefix them
       with the kind letter. (§3)
4. [ ] Rename three URLs in your client:
       - `/requirements/topic/X` → `/requirements?for_topic=X` (§4a)
       - `/conditions/id/X` → `/conditions/X` (§4b)
       - `/subjects/X/semantics` → `/topics/X/semantics` (§4c)
5. [ ] Repoint the WebSocket: `/comments/ws` → `/events/ws`. (§5a)
6. [ ] Rename your event type if you have one:
       `CommentEvent` → `AuditEvent`. (§5b)
7. [ ] Replace `type: "conversation_updated"` handling with
       `type: "topic_updated"`. On receipt, refetch `conversation`
       and any `invalidated_thread_ids` `thread`s. (§5c)
8. [ ] Replace delimiter-HTML consumption with structured-data
       rendering. (§6)
9. [ ] Mark `created_at` optional on pipeline-entity response types;
       switch any `author_id === 7` branches to `author_id === 1`. (§7)
10. [ ] Re-run your end-to-end tests.

---

## 10. Open questions worth asking before you start

- **How do you want to allocate `author_id` for user-created entries?**
  The server accepts whatever you send. If your client doesn't yet
  have auth, pick a fixed high integer per deploy (e.g., `100`).
- **Do you surface agent-triggered creates differently from pure user
  creates in the UI?** Both go through the same POST endpoints; only
  the `author_id` in the response differs. Decide UX early.
- **What's your refetch strategy on a burst of `topic_updated` events?**
  Debounce per `topic_id` — you don't need to refetch the same
  conversation three times if three mentions land in a second.
