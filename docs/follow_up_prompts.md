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

---

## 8. Delete the pipeline-trigger HTTP endpoints

> You are working in the Rust workspace at `/home/john/o11a-backend`. It
> has four crates: `o11a-core` (library), `o11a-analyze` (bin — runs the
> pipeline once and writes `audit.json`), `o11a-server` (bin — serves
> HTTP), `o11a-web-backend` (library — HTML rendering).
>
> Context: the analysis pipeline is now run by the `o11a-analyze` binary
> and writes its output to `audit.json`. The server loads that file on
> startup. The HTTP pipeline-trigger endpoints are a vestige of the old
> architecture where the server ran the pipeline; they serve no purpose
> now and invite confusion. We are in alpha — delete them cleanly. When
> we later want "re-run task X with user feedback," it will be a new
> endpoint with different semantics and should not reuse these URLs.
>
> **Task:**
>
> 1. In `crates/o11a-core/src/api/routes.rs` (or
>    `crates/o11a-server/src/api/routes.rs` if task 7 has already moved
>    the module), delete the route entries for:
>    - `POST /api/v1/audits/:audit_id/analyze`
>    - `POST /api/v1/audits/:audit_id/pipeline/semantic_links`
>    - `POST /api/v1/audits/:audit_id/pipeline/requirements`
>    - `POST /api/v1/audits/:audit_id/pipeline/behaviors`
>    - `POST /api/v1/audits/:audit_id/pipeline/synthesize`
>
> 2. In the matching `handlers.rs`, delete the five handler functions:
>    `analyze`, `pipeline_semantic_links`, `pipeline_requirements`,
>    `pipeline_behaviors`, `pipeline_synthesize`. Delete the
>    `pipeline_state()` helper and the `run_pipeline_step()` helper that
>    exist only to support them. Remove any now-unused imports.
>
> 3. If `pipeline::PipelineState` is no longer referenced anywhere outside
>    the pipeline module itself, leave the type as-is (it's still used
>    internally and by `o11a-analyze`) — but double-check that no stale
>    usage remains in the server.
>
> **Verify:** `cargo build --workspace` and `cargo test --workspace`
> both succeed. `grep -R "pipeline_state\|run_pipeline_step\|pipeline_semantic_links\|pipeline_requirements\|pipeline_behaviors\|pipeline_synthesize" crates/` should return no hits outside of the
> pipeline module itself.

---

## 9. Unify user-create endpoints with pipeline-read resource URLs

> You are working in the Rust workspace at `/home/john/o11a-backend`. It
> has four crates: `o11a-core` (library), `o11a-analyze` (bin),
> `o11a-server` (bin), `o11a-web-backend` (HTML).
>
> Pipeline-generated entities (features, requirements, behaviors,
> functional semantics) and user-created entities have the same shape;
> they only differ by `author_id`. The HTTP surface should reflect this:
> one resource URL per kind, with `GET` listing pipeline + user entries
> together and `POST` creating a new user entry.
>
> This task supersedes the URL conventions in task **6** in this
> document. If task 6 has already been done with `/user/features`-style
> URLs, rename them here. If task 6 hasn't been done yet, apply this
> convention when implementing it and skip the rename step.
>
> **Task:**
>
> 1. Create (or rename if already present) the following POST routes.
>    Each creates one user entity; none take a `/user/` infix:
>    - `POST /api/v1/audits/:audit_id/features`
>    - `POST /api/v1/audits/:audit_id/requirements`
>    - `POST /api/v1/audits/:audit_id/behaviors`
>    - `POST /api/v1/audits/:audit_id/functional_semantics`
>
> 2. Keep the existing `GET` endpoints:
>    - `GET /api/v1/audits/:audit_id/features` (existing)
>    - `GET /api/v1/audits/:audit_id/requirements` (add if missing —
>      returns all requirements, pipeline + user)
>    - `GET /api/v1/audits/:audit_id/behaviors` (existing)
>    - `GET /api/v1/audits/:audit_id/functional_semantics` (add if
>      missing)
>
>    These GETs already return any entity present in `DataContext`, so
>    merging pipeline + user happens for free once user entities are
>    loaded at startup (task 5 already does that).
>
> 3. Keep per-item GETs:
>    - `GET /api/v1/audits/:audit_id/features/:topic_id`
>    - `GET /api/v1/audits/:audit_id/requirements/:topic_id`
>    - `GET /api/v1/audits/:audit_id/behaviors/:topic_id`
>    - `GET /api/v1/audits/:audit_id/functional_semantics/:topic_id`
>
>    Rename any existing `:feature_id` / `:requirement_id` /
>    `:behavior_id` path params to `:topic_id` while you're here — see
>    task 10 for the full topic-id-only convention.
>
> **Verify:** `cargo build --workspace` succeeds. `curl` against each
> POST endpoint creates an entity; a subsequent GET on the same resource
> returns it alongside pipeline-produced entries. Restart the server
> and confirm user entries persist (they live in SQLite via task 5).

---

## 10. Adopt topic IDs as the sole path identifier; drop numeric fallback

> You are working in the Rust workspace at `/home/john/o11a-backend`. It
> has four crates: `o11a-core` (library), `o11a-analyze` (bin),
> `o11a-server` (bin), `o11a-web-backend` (HTML).
>
> Topic IDs (`F42`, `R7`, `B13`, `P99`, `N-100`, `D34`, `C2`, `I4`,
> `T1`) are globally unique across kinds. The API currently accepts
> both the prefixed form (`F42`) and a bare numeric ID (`42`) via the
> `parse_path_id` helper in `crates/o11a-core/src/api/handlers.rs` (or
> `crates/o11a-server/src/api/handlers.rs` post-task-7). The dual
> acceptance is a vestige of an earlier migration and forces every
> handler to tell the parser which kind it expects.
>
> **Task:**
>
> 1. Introduce (or locate) `parse_topic_id(input: &str, expected_kind:
>    TopicKind) -> Result<Topic, ParseError>` in
>    `crates/o11a-core/src/core/topic.rs` if one does not already exist.
>    It must require the prefix and validate the kind. Reject bare
>    numeric input with a clear error.
>
> 2. Replace every call site of `parse_path_id(&raw, Kind)` in the
>    handlers module with `parse_topic_id(&raw, Kind)` (or
>    `topic::parse_topic_id`). The handler then uses the returned
>    `Topic` directly — no need to convert back to `i64` unless the
>    DB call wants a numeric ID (and even then, call `Topic::numeric_id()`
>    at the narrow seam).
>
> 3. Delete the `parse_path_id` function and its `FromStr`-ish
>    numeric fallback.
>
> 4. Update handler signatures/bodies that previously accepted `:id: i64`
>    path params to accept `:topic_id: String` instead. Example URLs
>    affected: `/features/:feature_id`, `/requirements/:requirement_id`,
>    `/behaviors/:behavior_id`, `/threats/:threat_id`,
>    `/invariants/:invariant_id`, `/conditions/id/:condition_id` (see
>    task 11 for the further URL cleanup on conditions), and the various
>    vote/comment `:comment_id` routes. For comments specifically,
>    prefer the prefixed `C42` form; the payload/body form stays
>    numeric where Sqlx needs it.
>
> 5. Document the contract in a brief comment above `parse_topic_id`:
>    "All path parameters that identify a topic use the prefixed form
>    exclusively. Bare numeric IDs are not accepted."
>
> **Verify:** `cargo build --workspace` succeeds. `cargo test
> --workspace` passes. Manual: hitting `GET /audits/:id/features/42`
> (bare numeric) now returns 400; `GET /audits/:id/features/F42` returns
> the feature as before.

---

## 11. Clean up path-shape warts

> You are working in the Rust workspace at `/home/john/o11a-backend`. It
> has four crates: `o11a-core` (library), `o11a-analyze` (bin),
> `o11a-server` (bin), `o11a-web-backend` (HTML).
>
> A handful of route shapes show their history as collision-avoidance
> hacks or older vocabulary. We're in alpha — clean them up so the API
> reads uniformly.
>
> **Task:**
>
> 1. Fold `GET /api/v1/audits/:audit_id/requirements/topic/:topic_id`
>    into `GET /api/v1/audits/:audit_id/requirements?for_topic=T42`.
>    - Delete the `/requirements/topic/:topic_id` route entry.
>    - Modify the existing `GET /requirements` handler (add if missing)
>      to accept an optional `for_topic` query parameter. When present,
>      return only requirements related to that topic using the logic
>      that the old handler used (scan section_requirements, walk to
>      features via behaviors, etc.).
>
> 2. `DELETE /api/v1/audits/:audit_id/conditions/id/:condition_id` →
>    `DELETE /api/v1/audits/:audit_id/conditions/:condition_id`. The
>    `/id/` segment is redundant since the path slot already implies an
>    identifier. Update the route, the handler signature, and any
>    client docs or specs in `docs/specs/`.
>
> 3. `GET /api/v1/audits/:audit_id/subjects/:topic_id/semantics` →
>    `GET /api/v1/audits/:audit_id/topics/:topic_id/semantics`. The
>    term "subject" is inconsistent with the "topic" vocabulary used
>    everywhere else. Update route, handler path, and doc strings.
>
> 4. Grep the workspace and `docs/` tree for any references to the old
>    URLs and update them. Example pattern: `grep -R
>    "requirements/topic/\|conditions/id/\|subjects/" --include="*.{rs,md}"
>    crates/ docs/`.
>
> **Verify:** `cargo build --workspace` and `cargo test --workspace`
> succeed. Manual: each renamed endpoint responds on its new URL and
> 404s on its old one.

---

## 12. Rename the event WebSocket and its event type

> You are working in the Rust workspace at `/home/john/o11a-backend`. It
> has four crates: `o11a-core` (library), `o11a-analyze` (bin),
> `o11a-server` (bin), `o11a-web-backend` (HTML).
>
> The WebSocket at `/api/v1/audits/:audit_id/comments/ws` carries
> several event kinds, not just comment activity: `ConversationUpdated`
> (triggered by new comments), `StatusUpdated` (comment-status flips),
> `VoteUpdated` (vote activity), and future kinds the audit UI will
> want (e.g., pipeline-triggered refresh, user-created entity). The
> URL and type names should reflect the general "audit event stream"
> role.
>
> **Task:**
>
> 1. Rename the route: `/api/v1/audits/:audit_id/comments/ws` →
>    `/api/v1/audits/:audit_id/events/ws`. Update
>    `routes.rs` and `websocket.rs` (these may be in `o11a-core` or
>    `o11a-server` depending on whether task 7 has been done).
>
> 2. Rename the Rust enum `CommentEvent` →
>    `AuditEvent` throughout. Its declaration lives in
>    `crates/o11a-core/src/collaborator/models.rs`.
>
> 3. Rename the variant `CommentEvent::ConversationUpdated` →
>    `AuditEvent::TopicUpdated`. The serde tag stays `type` but the tag
>    value changes from `conversation_updated` to `topic_updated`. This
>    is a breaking wire change; document it in a short entry in
>    `docs/follow_up_prompts.md` if a changelog exists, or note it in
>    the commit message.
>
> 4. Rename the broadcast field on `AppState`:
>    `comment_broadcast: broadcast::Sender<CommentEvent>` →
>    `event_broadcast: broadcast::Sender<AuditEvent>`. Update every
>    call site: the comment-creation handler, status handler, vote
>    handlers, and the WebSocket relay.
>
> 5. Rename the web-backend re-exports and any doc strings that mention
>    "comment events" or "comment websocket" to "audit events" /
>    "event stream". Grep the tree for the old phrasing.
>
> **Verify:** `cargo build --workspace` and `cargo test --workspace`
> succeed. Starting the server and connecting a WebSocket client to
> the new URL should receive events in the new shape. The old URL
> should 404.

---

## 13. Make pipeline-entity `author_id` and `created_at` optional in the JSON report

> You are working in the Rust workspace at `/home/john/o11a-backend`. It
> has four crates: `o11a-core` (library), `o11a-analyze` (bin — pipeline
> runner that writes `audit.json`), `o11a-server` (bin), `o11a-web-backend`
> (HTML rendering).
>
> Context: every `TopicMetadata::{FeatureTopic, RequirementTopic,
> BehaviorTopic, FunctionalSemanticTopic}` variant currently carries
> `author_id: i64` and `created_at: String` as required fields. For
> pipeline-produced entities these are redundant — every entity from
> one analyze run shares the same generation timestamp (already captured
> at the top level of `audit.json` as `generated_at`) and was produced
> by the batch rather than a specific actor. Carrying them per-entity
> only makes sense when they're meaningful, i.e. for user-created and
> server-agent-created entities, not for pipeline output.
>
> The new rule:
> - `audit.json` omits `author_id` and `created_at` on each pipeline
>   entity.
> - On parse, the server assigns `AUTHOR_SYSTEM` (`1`) as the author
>   and `None` as the creation time — "this came from the analyze
>   batch, whose single top-level `generated_at` already tells you
>   when."
> - User- or server-agent-created entities (via the POST endpoints
>   from tasks 5, 6, and 9) carry both `author_id` and `created_at`
>   with real values.
>
> This is a breaking change to the on-disk JSON schema. Bump
> `SCHEMA_VERSION` from `1` to `2` in `crates/o11a-core/src/report.rs`.
>
> **Task:**
>
> 1. In `crates/o11a-core/src/core/mod.rs`: change `created_at: String`
>    → `created_at: Option<String>` on these four `TopicMetadata`
>    variants: `FeatureTopic`, `RequirementTopic`, `BehaviorTopic`,
>    `FunctionalSemanticTopic`. Do **not** change `created_at` on
>    `CommentTopic` or any other variant — those are user-authored and
>    always have a real timestamp.
>
> 2. In `crates/o11a-core/src/report.rs`:
>    - Bump `SCHEMA_VERSION` to `2`.
>    - Remove the `author_id` and `created_at` fields from
>      `ReportFeature`, `ReportRequirement`, `ReportBehavior`, and
>      `ReportFunctionalSemantic`.
>    - In `build_report`: stop reading those fields off `TopicMetadata`.
>    - In `apply_report`: when constructing each variant, set
>      `author_id: o11a_core::collaborator::models::AUTHOR_SYSTEM` and
>      `created_at: None`. The `AUTHOR_SYSTEM` constant equals `1`.
>
> 3. In `crates/o11a-core/src/collaborator/agent/pipeline.rs`: the
>    pipeline no longer needs a `generated_at` value to stamp per-entity
>    timestamps. If task 2 added a `generated_at: &str` parameter to
>    `run_full_pipeline` and its step functions, remove it. Each
>    pipeline-built `TopicMetadata` should use
>    `author_id: AUTHOR_SYSTEM` and `created_at: None`.
>
> 4. In `crates/o11a-analyze/src/main.rs`: drop the `generated_at`
>    argument passed to `run_full_pipeline`. Keep computing
>    `generated_at` via `o11a_core::ids::now_iso8601()` and pass it
>    directly to `build_report` — the report's top-level field is still
>    needed and is set by the caller, not by the pipeline.
>
> 5. In the user-entity DB layer (task 5:
>    `crates/o11a-core/src/collaborator/db/user_entities.rs`):
>    `load_user_*` must return `TopicMetadata` entries with the real
>    `author_id` from the row and `Some(created_at)` from the row.
>    `create_user_*` already takes both as parameters — no change
>    needed there, just confirm.
>
> 6. User-entity creation handlers (task 6): fill
>    `created_at = Some(now_iso8601())` and the user-supplied
>    `author_id` when constructing the `TopicMetadata`. If these
>    handlers were written assuming `created_at: String`, update to
>    `Some(String)`.
>
> 7. API response types (in `crates/o11a-server/src/api/handlers.rs`
>    post-task-7, or `crates/o11a-core/src/api/handlers.rs` pre-move):
>    the response shape for `TopicMetadataResponse` (and any per-kind
>    response) should serialize `created_at` with
>    `#[serde(skip_serializing_if = "Option::is_none")]` so absent
>    values are genuinely absent in the JSON rather than `null`. This
>    lets the client tell "never recorded" from "explicitly null."
>
> 8. Grep for any call site that unconditionally read `.created_at`
>    from a pipeline-kind `TopicMetadata` — it now returns
>    `&Option<String>`. Typical fix: `.as_deref().unwrap_or("")` for
>    display-only paths, or propagate the optionality upstream.
>
> **Verify:** `cargo build --workspace` and `cargo test --workspace`
> succeed.
>
> Run `cargo run -p o11a-analyze -- <project> <audit_id>`. Open the
> resulting `audit.json`; inside `pipeline.features` (or
> `pipeline.requirements`, etc.) entries should have **no**
> `author_id` or `created_at` field. The top-level `schema_version`
> should be `2`.
>
> Start the server against that report. `GET /api/v1/audits/:audit_id/features`
> should return entries with `author_id: 1` and no `created_at` field.
> POST a user feature via `/api/v1/audits/:audit_id/features`; the
> created entry should round-trip back through GET with both
> `author_id` and `created_at` populated.

---

## 14. Audit the follow-up work end to end

> You are a third-party reviewer auditing recent architectural work in
> the Rust workspace at `/home/john/o11a-backend`. You have no prior
> context beyond this prompt. Your job is to verify that tasks 1–13 in
> `docs/follow_up_prompts.md` were implemented correctly and that the
> workspace is in a coherent end state. Produce a written report, do
> not make code changes.
>
> ### Project at a glance
>
> The workspace has four crates:
> - `crates/o11a-core` — library. Parsing (Solidity + NatSpec),
>   analysis, agent tasks + pipeline, domain types (`AuditData`,
>   `TopicMetadata`, `Topic`), persistence (SQLite for collaboration
>   state), and the `report` module that defines `audit.json`'s schema.
> - `crates/o11a-analyze` — binary. Runs the analysis pipeline once
>   and writes `audit.json`. Expected to have no `sqlx` dependency.
> - `crates/o11a-server` — binary. Loads `audit.json` + SQLite on
>   startup, serves the HTTP API and WebSocket event stream.
> - `crates/o11a-web-backend` — library. HTML rendering consumed by
>   the server.
>
> Design invariants (all tasks should reinforce these):
> - Pipeline output is immutable and lives in `audit.json`. The
>   `o11a-analyze` binary writes it; the server reads it read-only at
>   startup.
> - User-created and user-triggered-agent-created entities live in
>   SQLite alongside comments and votes. They share shape with pipeline
>   entities but are distinguished by `author_id`.
> - Topic IDs (`F`, `R`, `B`, `P`, `N`, `D`, `C`, `I`, `T` prefixes)
>   are globally unique and allocated via process-wide atomic counters
>   in `o11a_core::ids`.
> - `o11a-core` is library-only; no HTTP routing types. HTTP lives in
>   `o11a-server`.
>
> ### Audit checklist
>
> **Build hygiene.** Run and report the output status of:
> - `cargo build --workspace`
> - `cargo test --workspace`
> - `cargo clippy --workspace --all-targets -- -D warnings` (non-blocking
>   if clippy warnings are pre-existing, but note any new ones)
> - `cargo fmt --all --check`
>
> Also count tests: `cargo test --workspace 2>&1 | grep "test result"`
> should show ~99+ passing tests.
>
> **Per-task verification.** For each task 1–13, check:
>
> 1. **Atomic ID counters** (`crates/o11a-core/src/ids.rs`): confirm four
>    `AtomicI32` counters exist for Feature / Requirement / Behavior /
>    FunctionalSemantic. Confirm `allocate_*` uses `fetch_add` with
>    `Relaxed` and `reseed_*` uses `store`. Confirm unit tests exist.
>
> 2. **Pipeline refactor** (`crates/o11a-core/src/collaborator/agent/pipeline.rs`):
>    `grep -n "db::" pipeline.rs` should return no matches for the
>    deleted functions (`create_feature`, `create_requirement`,
>    `create_behavior`, `create_functional_semantic`, their link/delete
>    helpers). `PipelineState` should have no `db` field. Note that
>    task 13 subsequently removed the `generated_at` parameter from
>    `run_full_pipeline`, so don't expect it on the signature.
>
> 3. **`o11a-analyze` simplification** (`crates/o11a-analyze/`): `Cargo.toml`
>    should not list `sqlx`. `main.rs` should not mention pools or
>    `init_schema`. Binary should compile with only `o11a-core`,
>    `tokio`, and `serde_json` as deps.
>
> 4. **SQL schema replacement** (`crates/o11a-core/src/db/` or wherever
>    `init_schema` lives): grep for `CREATE TABLE features`, `CREATE
>    TABLE requirements`, `CREATE TABLE behaviors`, `CREATE TABLE
>    semantic_links`, `CREATE TABLE feature_requirement_links`,
>    `CREATE TABLE feature_behavior_links`. All should be absent.
>    Their `user_*` replacements should exist with the column shapes
>    described in task 4. `db::load_all_features` should be deleted;
>    `server/src/main.rs` should no longer reference it.
>
> 5. **User-entity load step** (`crates/o11a-core/src/collaborator/db/user_entities.rs`
>    or equivalent): functions `create_user_feature`,
>    `create_user_requirement`, `create_user_behavior`,
>    `create_user_functional_semantic`, their `load_*` pairs, and
>    `apply_user_entities` should exist. Each `create_*` takes the
>    already-allocated ID as a parameter (no DB autoincrement for
>    these). `server/src/main.rs` should call `apply_user_entities`
>    after `apply_report` and before `rebuild_feature_context`.
>
> 6. **User-entity HTTP endpoints**: four POST handlers exist for the
>    four kinds. After task 9 is applied, their paths should be
>    `/audits/:audit_id/features`, `/audits/:audit_id/requirements`,
>    `/audits/:audit_id/behaviors`,
>    `/audits/:audit_id/functional_semantics` (no `/user/` infix).
>    Each handler calls `allocate_*_id`, persists via
>    `db::user_entities::create_*`, and updates `DataContext`.
>
> 7. **`api/` + `websocket.rs` move**: neither
>    `crates/o11a-core/src/api/` nor `crates/o11a-core/src/collaborator/websocket.rs`
>    should exist. They live at `crates/o11a-server/src/api/` and
>    `crates/o11a-server/src/websocket.rs`. `crates/o11a-core/Cargo.toml`
>    ideally no longer depends on `axum` (acceptable if it still does
>    for a transitional reason — flag it). `features_for_topic` lives
>    in `o11a-core` (not in `o11a-server/src/api/handlers.rs`).
>    `crates/o11a-web-backend/src/` should not import from
>    `o11a_core::api::*`.
>
> 8. **Pipeline-trigger removal**: grep for the handler names —
>    `analyze`, `pipeline_semantic_links`, `pipeline_requirements`,
>    `pipeline_behaviors`, `pipeline_synthesize`. None should exist as
>    route handlers. `run_pipeline_step` helper should also be gone.
>
> 9. **URL unification**: POST routes for user entities should be
>    exactly `/api/v1/audits/:audit_id/{features,requirements,behaviors,functional_semantics}`.
>    No `/user/` prefix anywhere.
>
> 10. **Topic-ID-only paths**: `grep -R "parse_path_id" crates/` should
>     return no hits. `parse_topic_id` should be the only helper and
>     should reject bare numeric IDs. Handler signatures should take
>     `:topic_id: String` path params, not `:feature_id: i64` etc.
>
> 11. **Path hygiene**: grep for the old shapes — `"requirements/topic/"`,
>     `"/conditions/id/"`, `"subjects/"` — and report any hits.
>     `GET /requirements?for_topic=...` should be supported.
>
> 12. **WebSocket rename**: `grep -R "comments/ws\|CommentEvent\|comment_broadcast"
>     crates/` should return no hits in production code (test fixtures
>     are OK). The new names should appear uniformly:
>     `/events/ws`, `AuditEvent`, `event_broadcast`,
>     `AuditEvent::TopicUpdated`.
>
> 13. **Optional pipeline `created_at` + `AUTHOR_SYSTEM` provenance**:
>     - `crates/o11a-core/src/report.rs` declares `SCHEMA_VERSION = 2`.
>     - `ReportFeature` / `ReportRequirement` / `ReportBehavior` /
>       `ReportFunctionalSemantic` have no `author_id` or `created_at`
>       fields.
>     - `TopicMetadata::{Feature,Requirement,Behavior,FunctionalSemantic}Topic`
>       variants have `created_at: Option<String>` (but `CommentTopic`
>       and others remain `String`).
>     - In a freshly-generated `audit.json`, `jq '.pipeline.features[0]
>       | keys'` does not list `"author_id"` or `"created_at"`.
>     - Freshly-loaded server: `curl /audits/:id/features | jq '.[0]'`
>       shows `author_id: 1` and no `created_at` key.
>     - POST-then-GET of a user feature round-trips with both
>       `author_id` (user's id) and `created_at` populated.
>
> ### Cross-cutting checks
>
> - **JSON schema stability**: `crates/o11a-core/src/report.rs` should
>   declare `SCHEMA_VERSION = 2` after task 13 (bumped from 1 by the
>   `author_id`/`created_at` removal on pipeline entities). If it reads
>   `3` or higher, verify each bump was actually breaking and not just
>   an accidental change.
>
> - **Dead code**: run `cargo build --workspace 2>&1 | grep -i warning`
>   and report any `unused_*` warnings. These often point at leftover
>   imports or functions from the deleted code paths.
>
> - **Dependency drift**: run `cargo tree -p o11a-core --depth 1` and
>   confirm: no `axum`, no `tower*`, no `futures`. (If any remain,
>   flag them — they were supposed to move to server in task 7.)
>   `cargo tree -p o11a-analyze --depth 1`: no `sqlx`, no `axum`.
>
> - **Doc/comment drift**: grep `docs/` for mentions of deleted
>   endpoints, old URL shapes, old event type names. Update or flag.
>
> - **End-to-end smoke test**, if feasible:
>   1. `cargo run -p o11a-analyze -- <some project> <audit_id>` —
>      should produce a valid `audit.json` at the project root.
>      `jq '.schema_version' audit.json` → `2`.
>      `jq '.pipeline.features[0] | keys' audit.json` → should **not**
>      include `"author_id"` or `"created_at"`.
>   2. `PROJECT_ROOT=<path> AUDIT_ID=<audit_id> cargo run -p o11a-server`
>      — server should start, load the report, listen on port 3058.
>   3. `curl http://localhost:3058/health` → 200 OK.
>   4. `curl http://localhost:3058/api/v1/audits/<audit_id>/features |
>      jq '.[0]'` → JSON object with `author_id: 1` (AUTHOR_SYSTEM)
>      and no `created_at` key. Array may be empty if the pipeline
>      didn't synthesize any features for that fixture.
>   5. `curl -X POST http://localhost:3058/api/v1/audits/<audit_id>/features
>      -H 'content-type: application/json' -d '{"name":"Test","description":"t","author_id":100}'` →
>      returns `{topic_id: "F..."}` where the ID is above the max
>      pipeline-assigned feature ID. A subsequent GET on that topic
>      returns `author_id: 100` and a populated `created_at`.
>
> ### Report format
>
> Produce a concise report (under 800 words) with three sections:
> - **PASS** — tasks that look correctly implemented. One line each.
> - **ISSUES** — concrete problems found, each tied to a task number,
>   with a file path and the specific concern.
> - **FOLLOW-UPS** — nice-to-haves or drift that's not a bug but is
>   worth cleaning up (e.g., stale doc strings, orphaned tests).
>
> Do not attempt to fix anything. Produce the report only.
