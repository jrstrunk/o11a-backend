# Follow-up Implementation Prompts

Each numbered section below is a **self-contained prompt** for a new agent
session. Copy the section into a fresh Claude Code session, then have the
agent execute it. Work through them **in order** — later prompts depend on
the outcomes of earlier ones.

Every prompt assumes the agent starts with no memory of the surrounding
design discussion. Everything it needs is in the prompt body.

---

## 1. Add atomic ID counters for pipeline-kind topics to `o11a-core`

> You are working in the Rust workspace at `/home/john/o11a-backend`. It has
> four crates:
> - `crates/o11a-core` — library: parsing, analysis, agent tasks + pipeline,
>   persistence. No HTTP types.
> - `crates/o11a-analyze` — binary: runs the pipeline once and writes
>   `audit.json` (a versioned JSON report of the pipeline output).
> - `crates/o11a-server` — binary: loads `audit.json` + SQLite on startup
>   and serves the HTTP API.
> - `crates/o11a-web-backend` — library: HTML rendering for the server.
>
> Context for this task: pipeline-produced entities (features, requirements,
> behaviors, functional semantics) currently get their numeric IDs from
> SQLite autoincrement. We're moving to a process-wide atomic counter per
> kind. The same counter is used during the analyze run (starts at 1) and
> later on the server side when user/agent-triggered creates allocate new
> IDs on top of the pipeline's range. The server seeds the counter from the
> max loaded ID when it applies the `audit.json` report.
>
> There is a prior-art pattern for this in `crates/o11a-core/src/core/topic.rs`
> (search for `NEXT_GENERATED_NODE_ID`). Follow the same shape, but use
> `std::sync::atomic::AtomicI32` instead of `static mut`.
>
> **Task:**
>
> 1. Create a new module `crates/o11a-core/src/ids.rs` containing four
>    `AtomicI32` counters (all initialized to `1`):
>    - `NEXT_FEATURE_ID`, `NEXT_REQUIREMENT_ID`, `NEXT_BEHAVIOR_ID`,
>      `NEXT_FUNCTIONAL_SEMANTIC_ID`.
> 2. For each, expose a pair of `pub fn`s:
>    - `allocate_feature_id() -> i32` — returns `fetch_add(1, Relaxed)`.
>    - `reseed_feature_id(max_loaded: i32)` — sets the counter to
>      `max_loaded + 1` via `store(..., Relaxed)`.
> 3. Add `pub mod ids;` to `crates/o11a-core/src/lib.rs`.
> 4. Write unit tests in the same file for: monotonic allocation, reseed
>    behavior, and that reseed with a lower value still advances (use
>    `store` unconditionally — we trust callers).
>
> **Verify:** `cargo build` and `cargo test -p o11a-core` both succeed.
> No other crate should need changes.

---

## 2. Refactor `pipeline.rs` to drop SQL writes and use the ID counters

> You are working in the Rust workspace at `/home/john/o11a-backend`. It has
> four crates: `o11a-core` (library — parsing, analysis, agent pipeline,
> persistence), `o11a-analyze` (bin — runs pipeline once, writes
> `audit.json`), `o11a-server` (bin — loads JSON + SQLite, serves HTTP),
> `o11a-web-backend` (lib — HTML rendering).
>
> The pipeline's output is now captured in `audit.json` (see
> `crates/o11a-core/src/report.rs`), not SQLite. That means the pipeline
> should run entirely in memory: it no longer needs a `SqlitePool`, and it
> no longer needs to call `db::create_feature` / `create_requirement` /
> `create_behavior` / `create_functional_semantic` / their link helpers.
>
> Prerequisite: task **1** in `docs/follow_up_prompts.md` (atomic ID
> counters) must be done first. You will use `o11a_core::ids::allocate_*`
> from this task.
>
> **Task (in `crates/o11a-core/src/collaborator/agent/pipeline.rs`):**
>
> 1. Remove the `db: SqlitePool` field from `PipelineState`.
> 2. Walk every function in `pipeline.rs`. Replace every `db::create_*` call
>    that returns an autoincrement row with: a counter call from
>    `crate::ids::allocate_*`, plus the run's timestamp.
> 3. Add a `generated_at: String` parameter to `run_full_pipeline` (and
>    propagate to each step function). Use this value for every
>    `created_at` field on pipeline-produced `TopicMetadata` entries.
> 4. Delete link-write calls (`db::add_feature_requirement_link`,
>    `add_feature_behavior_link`) and the deletion helpers
>    (`delete_all_features_for_audit`, `delete_all_feature_links_for_audit`).
>    Everything the pipeline produces now lives only in the in-memory
>    `DataContext`.
> 5. Update every caller of `PipelineState { db, data_context }` so they
>    pass only `data_context`. Search the workspace: callers include
>    `crates/o11a-core/src/api/handlers.rs` (pipeline_state() helper) and
>    `crates/o11a-analyze/src/main.rs`.
> 6. In `crates/o11a-core/src/api/handlers.rs`, the HTTP trigger handlers
>    (`pipeline_semantic_links`, `pipeline_requirements`,
>    `pipeline_behaviors`, `pipeline_synthesize`, `analyze`) should derive
>    a fresh `generated_at` string (ISO-8601 UTC, seconds precision) and
>    pass it through. There is a reference `now_iso8601()` in
>    `crates/o11a-analyze/src/main.rs` — extract it into
>    `crates/o11a-core/src/ids.rs` (or a sibling module) so both crates
>    share it.
>
> **Verify:** `cargo build` and `cargo test --workspace` both pass. The
> pipeline tables in the SQLite schema are still present — don't touch them
> in this task; they're handled in task 4.

---

## 3. Drop in-memory SQLite from `o11a-analyze`

> You are working in the Rust workspace at `/home/john/o11a-backend`. The
> `o11a-analyze` binary at `crates/o11a-analyze/src/main.rs` currently sets
> up an in-memory SQLite pool, initializes the schema, and hands the pool
> to `PipelineState` only so that the pipeline's (now-removed) SQL writes
> have a target.
>
> Prerequisite: task **2** in `docs/follow_up_prompts.md` — the pipeline no
> longer needs a `SqlitePool`.
>
> **Task:**
>
> 1. Remove the `sqlx` dependency from `crates/o11a-analyze/Cargo.toml`.
> 2. In `crates/o11a-analyze/src/main.rs`:
>    - Delete the pool creation and `init_schema` call.
>    - Remove the `use o11a_core::db as core_db;` import.
>    - Construct `PipelineState` with only `data_context`.
>    - Pass the existing `generated_at` string into `run_full_pipeline`.
> 3. If you extracted `now_iso8601` into `o11a-core` as part of task 2,
>    delete the local copy and import from there.
>
> **Verify:** `cargo build -p o11a-analyze` succeeds. The binary still
> produces a valid `audit.json` when run against a real project (you can
> skip the full run and trust the build + existing tests as a signal).

---

## 4. Replace pipeline-output SQLite tables with `user_*` equivalents

> You are working in the Rust workspace at `/home/john/o11a-backend`. It
> has four crates: `o11a-core` (library), `o11a-analyze` (bin — pipeline
> runner that writes `audit.json`), `o11a-server` (bin — HTTP), and
> `o11a-web-backend` (HTML).
>
> The database schema at `crates/o11a-core/src/db/mod.rs` (and whatever it
> delegates to) currently has tables for pipeline output: `features`,
> `requirements`, `behaviors`, `feature_requirement_links`,
> `feature_behavior_links`, `semantic_links`, `semantic_link_docs`,
> `requirement_documentation_topics`, `behavior_source_topics`. As of the
> recent architectural split, pipeline output lives in `audit.json`, so
> these tables are unused.
>
> The same shape of entities can now be created by users (or by
> user-triggered agent runs) on the server. Those *mutable* entities need
> new tables with distinct names so we never conflate pipeline output
> (JSON) with user-authored content (SQL).
>
> Prerequisite: tasks **2** and **3** — pipeline no longer writes to the
> old tables, and `o11a-analyze` no longer uses a DB.
>
> **Task:**
>
> 1. In `crates/o11a-core/src/db/mod.rs` (or wherever `init_schema` lives):
>    drop the `CREATE TABLE` statements for the pipeline-output tables
>    listed above. This is the full migration — we're in alpha, no data
>    preservation required.
> 2. Add these replacement tables. Column shapes should mirror the fields
>    on the matching `TopicMetadata` variants in
>    `crates/o11a-core/src/core/mod.rs` — read those variants to get names
>    and types right.
>    - `user_features (id INTEGER PRIMARY KEY, audit_id TEXT NOT NULL,
>       name TEXT NOT NULL, description TEXT NOT NULL, author_id INTEGER
>       NOT NULL, created_at TEXT NOT NULL)`
>    - `user_requirements (id, audit_id, description, section_topic TEXT,
>       author_id, created_at)`
>    - `user_behaviors (id, audit_id, description, member_topic TEXT,
>       author_id, created_at)`
>    - `user_functional_semantics (id, audit_id, description,
>       declaration_topic TEXT, author_id, created_at)`
>    - `user_requirement_documentation_topics (user_requirement_id INTEGER,
>       documentation_topic TEXT, PRIMARY KEY (user_requirement_id,
>       documentation_topic))`
>    - `user_functional_semantic_documentation_topics` with the same shape
>      (per-semantic doc-topic list).
>    - `user_feature_requirement_links (user_feature_id INTEGER,
>       requirement_topic TEXT)` — `requirement_topic` may reference either
>       a pipeline or a user requirement.
>    - `user_feature_behavior_links` with analogous columns.
> 3. Remove any now-unused Rust functions that touched the deleted tables:
>    `db::create_feature`, `db::create_requirement`, `db::create_behavior`,
>    `db::create_functional_semantic`, `db::add_feature_requirement_link`,
>    `db::add_feature_behavior_link`, `db::delete_all_features_for_audit`,
>    `db::delete_all_feature_links_for_audit`,
>    `db::set_requirement_section`,
>    `db::add_requirement_documentation_topic`,
>    `db::load_all_features`, and any sibling `load_*` for these tables.
>    Search the crate before deleting to make sure no caller remains.
> 4. In `crates/o11a-server/src/main.rs`, delete the legacy fallback
>    branch that called `collab_db::load_all_features` when the audit
>    report was absent. The server now requires an `audit.json` to have
>    pipeline data; print an error and exit if the report is missing.
>
> **Verify:** `cargo build --workspace` and `cargo test --workspace`
> succeed. Running the server against a project with an existing
> `audit.json` still hydrates pipeline data. Running it without an
> `audit.json` now errors out with a clear message.

---

## 5. Add a DB layer and server load step for user-created entities

> You are working in the Rust workspace at `/home/john/o11a-backend`. It
> has four crates: `o11a-core` (library), `o11a-analyze` (bin),
> `o11a-server` (bin), `o11a-web-backend` (HTML).
>
> Pipeline output (features, requirements, behaviors, functional
> semantics) lives in `audit.json` and is applied at server startup by
> `o11a_core::report::apply_report`. The same *shape* of data can also be
> created by users (or by user-triggered agent runs) on the server, and
> those live in SQLite tables — `user_features`, `user_requirements`,
> `user_behaviors`, `user_functional_semantics` (plus the
> `user_*_documentation_topics` side tables and `user_feature_*_links`).
>
> Prerequisite: task **4** — those tables exist in `init_schema`.
>
> Task context: on server startup, after the JSON report is applied and
> the ID counters are reseeded from the report's max IDs, we need to load
> user entities from SQLite and register them in `DataContext` using the
> counter's now-advanced range. Only *creation* and *listing* are needed
> for this task; the HTTP endpoints and the hide feature are separate
> tasks.
>
> **Task:**
>
> 1. Create `crates/o11a-core/src/collaborator/db/user_entities.rs` (new
>    file; if `db` is a single file today, split it into a module
>    directory). Expose:
>    - `async fn create_user_feature(pool, audit_id, name, description,
>       author_id, created_at) -> Result<UserFeatureRow, sqlx::Error>`
>      plus analogous functions for the other three kinds.
>    - `async fn load_user_features(pool, audit_id) ->
>       Result<Vec<UserFeatureRow>, sqlx::Error>` plus analogs.
>    - Each create fn takes the ID already allocated from
>      `o11a_core::ids::allocate_*` as an explicit parameter (so the DB
>      doesn't autoincrement its own ID — the counter owns ID
>      allocation).
> 2. Add `apply_user_entities(pool, audit_id, audit_data)` to
>    `crates/o11a-core/src/collaborator/db/mod.rs` (or a new
>    `user_entities` module). It loads every kind, constructs
>    `TopicMetadata::FeatureTopic` etc., inserts into
>    `audit_data.topic_metadata`, and populates `audit_data.requirements`
>    / `feature_requirement_links` / `feature_behavior_links` from the
>    link tables.
> 3. In `crates/o11a-server/src/main.rs`, call `apply_user_entities`
>    after `apply_report` and before `rebuild_feature_context`. IDs from
>    the JSON report have already reseeded the counters; user entities
>    loaded now coexist with pipeline entities in the same `i32` space.
>
> **Verify:** `cargo build` and `cargo test --workspace` pass. Server
> starts cleanly against a project with an `audit.json` and an empty
> collaboration DB.

---

## 6. HTTP endpoints for creating user entities

> You are working in the Rust workspace at `/home/john/o11a-backend`. The
> server crate `o11a-server` exposes an HTTP API; handlers live in
> `crates/o11a-core/src/api/handlers.rs` and routes in
> `crates/o11a-core/src/api/routes.rs` (the HTTP surface is temporarily
> hosted in core; don't move it as part of this task).
>
> Pipeline-shape entities (features, requirements, behaviors, functional
> semantics) can now be created by users or by user-triggered agent runs.
> DB functions exist at `crates/o11a-core/src/collaborator/db/user_entities.rs`
> (`create_user_feature`, etc.) and ID allocation is via
> `o11a_core::ids::allocate_*`. See the `TopicMetadata` variants in
> `crates/o11a-core/src/core/mod.rs` for the exact fields each kind needs.
>
> Prerequisite: tasks **4** and **5**.
>
> **Task:**
>
> 1. In `crates/o11a-core/src/api/handlers.rs`, add four POST handlers:
>    - `create_user_feature(State<AppState>, Path<audit_id>, Json<payload>)`
>    - `create_user_requirement(...)`
>    - `create_user_behavior(...)`
>    - `create_user_functional_semantic(...)`
>
>    Each handler:
>    1. Allocates an ID via `o11a_core::ids::allocate_*`.
>    2. Uses `now_iso8601()` (from task 2) for `created_at`.
>    3. Persists via the matching `db::user_entities::create_*`.
>    4. Inserts a `TopicMetadata::*Topic` entry into `DataContext`.
>    5. Returns `Json<CreatedResponse { topic_id }>` where `topic_id` is
>       the `F12`/`R12`/`B12`/`P12`-prefixed id.
>
> 2. In `crates/o11a-core/src/api/routes.rs`, wire the four routes under
>    a consistent prefix — suggested:
>    - `POST /api/v1/audits/:audit_id/user/features`
>    - `POST /api/v1/audits/:audit_id/user/requirements`
>    - `POST /api/v1/audits/:audit_id/user/behaviors`
>    - `POST /api/v1/audits/:audit_id/user/functional_semantics`
>
> 3. Request payload structs go in the same file as the handlers. For
>    requirements and functional semantics, accept an optional list of
>    `documentation_topics: Vec<String>` and persist via the side table.
>    For features, accept `name`, `description`, plus optional
>    `requirement_topics` / `behavior_topics` arrays, and write the
>    link tables.
>
> **Verify:** `cargo build` succeeds, `cargo test --workspace` passes.
> Manually: start the server, POST to each endpoint, restart the server
> (no `audit.json` edit), confirm the entity is re-loaded from SQLite and
> appears in subsequent GETs (e.g. `GET /api/v1/audits/:audit_id/features`
> should include the newly created feature).

---

## 7. Move HTTP `api/` and `collaborator/websocket.rs` out of `o11a-core` into `o11a-server`

> You are working in the Rust workspace at `/home/john/o11a-backend`. It
> has four crates: `o11a-core` (library), `o11a-analyze` (bin),
> `o11a-server` (bin, currently just `main.rs`), `o11a-web-backend`
> (library that renders HTML, depends on `o11a-core`).
>
> The architectural goal is for `o11a-core` to be **library-only** with no
> HTTP dependency — so it can be reused by the analyze binary, by future
> CLI tools, and by an eventual MCP server without pulling in axum. The
> HTTP layer currently lives in `crates/o11a-core/src/api/{mod,handlers,routes}.rs`
> (~3,000 LOC) and in `crates/o11a-core/src/collaborator/websocket.rs`.
> This task moves all of that into `o11a-server`.
>
> Two prior preparations have already been done:
> - `AppState` is at `crates/o11a-core/src/state.rs` (re-exported via
>   `o11a_core::api::AppState` for now). It can stay in core.
> - `ScopeInfo` and its DTO neighbors were moved into
>   `crates/o11a-core/src/collaborator/scope_info.rs`. They stay in core.
>
> **Task:**
>
> 1. **Move the HTTP files into the server crate:**
>    - `git mv crates/o11a-core/src/api crates/o11a-server/src/api`
>    - `git mv crates/o11a-core/src/collaborator/websocket.rs crates/o11a-server/src/websocket.rs`
>
> 2. **Rewrite imports inside the moved files:**
>    - Every `crate::core::...`, `crate::collaborator::...`,
>      `crate::solidity::...`, `crate::documentation::...`,
>      `crate::db::...`, `crate::state::...`, `crate::ids::...`,
>      `crate::report::...` → `o11a_core::...` variant.
>    - Every `crate::api::AppState` import (some handlers already use
>      this) → `o11a_core::state::AppState`.
>    - `use crate::api::handlers::features_for_topic;` inside `routes.rs`
>      → `use crate::api::handlers::features_for_topic;` (stays, still
>      sibling inside `o11a-server`).
>
> 3. **Relocate `features_for_topic`** (currently `pub` in
>    `api/handlers.rs`) to `o11a-core` — it's a pure data function
>    (`fn(&Topic, &AuditData) -> Vec<Topic>`) and `o11a-web-backend`
>    imports it. Good home: `crates/o11a-core/src/core/mod.rs` or a new
>    `crates/o11a-core/src/feature_lookup.rs`. Update the two known
>    callers (`crates/o11a-web-backend/src/handlers.rs`,
>    `crates/o11a-server/src/api/handlers.rs` post-move).
>
> 4. **Update `crates/o11a-core/src/lib.rs`:** remove `pub mod api;`.
>
> 5. **Update `crates/o11a-core/src/collaborator/mod.rs`:** remove
>    `pub mod websocket;`.
>
> 6. **Update `crates/o11a-core/src/api/mod.rs`:** this file is gone
>    after step 1; make sure the `pub use handlers::SourceContextResponse`
>    and `pub use crate::state::AppState` re-exports move to whatever
>    module declares the new api in server.
>
> 7. **Update `crates/o11a-server/Cargo.toml`:** add the transitive deps
>    the moved code needs — `axum` (already present, add `ws` feature if
>    missing), `sqlx` with `runtime-tokio` and `sqlite`, `tower`,
>    `tower-http` with `cors`, `serde` (with `derive`), `serde_json`
>    (already present), `futures`.
>
> 8. **Update `crates/o11a-server/src/main.rs`:** declare `mod api;`
>    and `mod websocket;`. References that were previously
>    `o11a_core::api::{AppState, routes}` become `o11a_core::state::AppState`
>    and `crate::api::routes` respectively.
>
> 9. **Update `crates/o11a-web-backend/src/`** — the two files that import
>    from `o11a_core::api::*` need to import from `o11a_core::state` /
>    the new `features_for_topic` location.
>
> **Verify:** `cargo build --workspace`, `cargo test --workspace`, and
> `cargo clippy --workspace --all-targets` all succeed. The server should
> start and serve all its previous endpoints. `o11a-core` should have no
> HTTP dependency — check by inspecting that `axum` is no longer a
> required dep of `o11a-core`'s Cargo.toml (it's OK if `axum` stays as a
> dep temporarily for the move; remove it in the same PR if nothing in
> `o11a-core` needs it anymore, which should be the case).
