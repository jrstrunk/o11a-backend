# Follow-up Implementation Prompts

This set of prompts performs the "types pass" — six alpha-window
hygiene refactors that change the shape of code every other task
touches, so it's cheap to do now and expensive later.

The workspace has four crates:

- `o11a-core` — library. Domain types (`AuditData`, `Topic`,
  `TopicMetadata`), pipeline, report schema, analysis-artifact
  serialization, collaboration persistence.
- `o11a-analyze` — binary. Owns source parsers + analyzers. Runs the
  pipeline once and writes `<project_root>/o11a/audit.json` +
  `<project_root>/o11a/audit.analysis.bin`.
- `o11a-server` — binary. Loads both files at startup, serves the
  HTTP API + WebSocket.
- `o11a-web-backend` — library. HTML rendering used by the server.

Each numbered section is a **self-contained prompt** for a fresh
agent session. Execute them in order — later prompts reference type
names and module paths established by earlier ones.

---

## 1. Make `Topic` a typed enum, preserving the string wire format

> You are working in the Rust workspace at `/home/john/o11a-backend`.
> It has four crates: `o11a-core` (library — domain types, pipeline,
> persistence), `o11a-analyze` (bin — parser + analyzer + pipeline
> runner), `o11a-server` (bin — HTTP API), `o11a-web-backend` (lib —
> HTML rendering).
>
> Today `Topic` is a string wrapper:
>
> ```rust
> pub struct Topic { pub id: String }
> pub enum TopicKind { Node, Documentation, Comment, Invariant,
>     AttackVector, Feature, Requirement, Behavior,
>     FunctionalProperty, TypeConstraint }
> ```
>
> Every handler that takes a topic re-parses the prefix character at
> runtime via `Topic::kind()` and validates via `parse_topic_id`.
> Consumers carry the kind as context implicitly. Change `Topic` into
> a typed enum so the kind is a compile-time property everywhere:
>
> ```rust
> pub enum Topic {
>     Node(i32),
>     Documentation(i32),
>     Comment(i32),
>     Invariant(i32),
>     AttackVector(i32),
>     Feature(i32),
>     Requirement(i32),
>     Behavior(i32),
>     FunctionalProperty(i32),
>     TypeConstraint(i32),
> }
> ```
>
> The wire format stays the same prefixed string (`"F42"`, `"N-100"`,
> `"C7"`, etc.) via custom `Serialize`/`Deserialize`. Clients see no
> change. The old `TopicKind` enum disappears — the variant *is* the
> kind.
>
> **Task:**
>
> 1. In `crates/o11a-core/src/core/topic.rs` (or wherever `Topic`
>    lives today — check `crate::core::topic`):
>    - Replace the `Topic` struct with the enum above.
>    - Delete the `TopicKind` enum (use `core::mem::discriminant` or
>      a helper method if any caller needed just-the-kind).
>    - Implement `Display` emitting the prefixed string. Prefix map
>      derived from the existing `Topic::kind()` match: `N` for Node,
>      `D` for Documentation, `C` for Comment, `I` for Invariant,
>      `T` for AttackVector, `F` for Feature, `R` for Requirement,
>      `B` for Behavior, `P` for FunctionalProperty. TypeConstraint's
>      prefix isn't in current use; pick `Y` (next unused ASCII) and
>      document the choice.
>    - Implement `FromStr` with a `ParseTopicError` variant for
>      unknown prefix and bad numeric suffix.
>    - Implement `Serialize` via `Display`, `Deserialize` via a
>      visitor that calls `FromStr`. Round-trip must be lossless.
>    - Keep constructor helpers (`new_feature_topic(i32)`, etc.) as
>      thin wrappers — they become `Topic::Feature`, etc. Each helper
>      is a one-liner now. Alternatively delete them; grep the
>      workspace for usages and update call sites to variant syntax
>      directly.
>    - Provide a method `numeric_id(&self) -> i32` that returns the
>      inner integer regardless of variant, for call sites that need
>      the raw id. The prior `Topic::numeric_id() -> Option<i32>`
>      that returned `Some` on prefixed topics and `None` elsewhere
>      is now infallible.
>    - Provide `id(&self) -> String` returning the prefixed form
>      (calls `Display`). Prior `id(&self) -> &str` callers must
>      switch to `id()` returning `String`, or to `format!("{}",
>      topic)` where they want a format argument. Fix every caller.
>
> 2. Update every consumer in the workspace. Expect to touch: handler
>    request/response types (many `String`-typed path parameters
>    become `Topic`), pipeline code, agent context, topic metadata
>    variants, DB serialization (comments store topic IDs as TEXT;
>    adjust the round-trip to use `Display`/`FromStr`).
>
> 3. Bincode round-trip for the analysis artifact: the
>    `AuditDataSnapshot` contains `Topic`s deep in its field
>    hierarchy (as map keys and as fields). Verify bincode with
>    `features = ["serde"]` uses the custom `Serialize`/`Deserialize`
>    — it should. Add a unit test: build a `Topic::Feature(42)`,
>    serialize with bincode, deserialize, assert equality.
>
> 4. Expected ergonomic wins to verify in the diff:
>    - `match topic.kind()` becomes `match topic` at the use site.
>    - Functions that took a topic and asserted its kind at runtime
>      can now take `feature: Topic` and pattern-match, or take a
>      typed wrapper like `FeatureTopic(i32)` if preferred. Don't
>      introduce per-kind wrappers in this prompt — that's a
>      potential future refinement.
>    - Handler path params that were `String` and then parsed should
>      use `axum`'s `FromStr`-based extraction. If that isn't
>      straightforward, keep `String` at the axum boundary and parse
>      in the handler body.
>
> **Verify:**
>
> - `cargo build --workspace` and `cargo test --workspace` pass.
> - Hit an existing endpoint like
>   `GET /api/v1/audits/:audit_id/features/F1` and confirm the
>   response unchanged. `curl`-diff the same endpoint before/after
>   if possible — byte-identical response is the goal.
> - Round-trip an `audit.json` file produced before the change:
>   server should load it without complaint (topics stored as
>   prefixed strings should `FromStr` cleanly into the new enum).
> - Round-trip an `audit.analysis.bin` produced before the change:
>   if bincode's string encoding is unchanged, old artifacts load.
>   If not, regenerate the artifact and note in the commit message
>   that the binary format bumped.

---

## 2. Make `Author` a typed enum, preserving the integer wire format

> You are working in the Rust workspace at `/home/john/o11a-backend`.
> Four crates: `o11a-core` (library), `o11a-analyze` (bin —
> parser/analyzer/pipeline), `o11a-server` (bin — HTTP),
> `o11a-web-backend` (lib — HTML). Comments, pipeline entities, and
> user-created entities all carry an `author_id: i64` with a set of
> magic constants:
>
> ```rust
> pub const AUTHOR_SYSTEM: i64 = 1;
> pub const AUTHOR_DEV_TECHNICAL: i64 = 2;
> pub const AUTHOR_DEV_DOCUMENTATION: i64 = 3;
> pub const AUTHOR_AGENT_MICRO: i64 = 4;
> pub const AUTHOR_AGENT_SMALL: i64 = 5;
> pub const AUTHOR_AGENT_MEDIUM: i64 = 6;
> pub const AUTHOR_AGENT_LARGE: i64 = 7;
> ```
>
> These live in `crates/o11a-core/src/collaborator/models.rs`.
> User IDs are `i64 >= 8` by convention. Code scattered across the
> workspace compares `author_id == AUTHOR_AGENT_LARGE`, sets defaults
> to `AUTHOR_SYSTEM`, etc.
>
> Replace the raw integer with a typed enum **while preserving the
> integer wire format** (clients, DB rows, and the migration guide
> all expect integers).
>
> ```rust
> pub enum Author {
>     System,              // 1
>     DevTechnical,        // 2
>     DevDocumentation,    // 3
>     AgentMicro,          // 4
>     AgentSmall,          // 5
>     AgentMedium,         // 6
>     AgentLarge,          // 7
>     User(u64),           // >= 8 (inner value = the raw integer)
> }
> ```
>
> **Task:**
>
> 1. Define the enum in `crates/o11a-core/src/collaborator/models.rs`
>    next to the existing constants. Keep the constants as
>    `pub const` for now — they're still useful for DB migration
>    values and for tests. Optionally mark them
>    `#[deprecated(note = "use Author variants instead")]` to drive
>    a later cleanup pass.
>
> 2. Provide `From<i64>` and `Into<i64>` (or equivalently
>    `from_id`/`to_id` methods). The `From<i64>` handles `1..=7`
>    mapping to named variants and anything `>= 8` mapping to
>    `Author::User(n as u64)`. Values `<= 0` are invalid — return
>    `Result` via a `parse_author(i: i64) -> Result<Author, ...>`
>    helper in the fallible direction. Provide `Author::as_i64(self)
>    -> i64` for the other direction (infallible).
>
> 3. Implement `Serialize` and `Deserialize` so Author serializes as
>    a plain integer. Use:
>    - `impl Serialize for Author` → `i64::serialize(&self.as_i64(),
>      ser)`
>    - `impl<'de> Deserialize<'de> for Author` → deserialize `i64`,
>      then `Author::from_id(n)` (with error on invalid integers).
>
> 4. Change `author_id: i64` to `author: Author` on:
>    - All `TopicMetadata` variants (`FeatureTopic`,
>      `RequirementTopic`, `BehaviorTopic`, `FunctionalSemanticTopic`,
>      `CommentTopic`, `ThreatTopic`, `InvariantTopic`).
>    - `Comment` row struct.
>    - `CommentCreatedResponse` and any other response DTO that
>      carries it.
>    - User-entity creation request DTOs (payload field
>      `author_id: i64` becomes `author: Author`). Wire format still
>      receives an integer and deserializes via the custom impl.
>
> 5. At DB boundaries where SQL columns are `INTEGER`:
>    - `sqlx` query bindings/extractions need `.as_i64()` on write
>      and `Author::from_id(col)` on read. Add helper conversion
>      functions if this pattern repeats.
>
> 6. Delete every `if author_id == AUTHOR_AGENT_LARGE` style
>    comparison and replace with `match author { Author::AgentLarge
>    => ... }`. Likewise for default values:
>    `AUTHOR_SYSTEM` → `Author::System`.
>
> 7. Agent pipeline code at `crates/o11a-analyze/src/` and
>    `crates/o11a-core/src/collaborator/agent/` currently assigns
>    `author_id: AUTHOR_AGENT_LARGE` on generated entities. Now that
>    (per earlier work) pipeline entities are tagged `Author::System`
>    on apply and don't carry per-entity author in `audit.json`, this
>    mostly goes away. Grep for remaining assignments and update.
>
> **Verify:**
>
> - `cargo build --workspace` and `cargo test --workspace` pass.
> - Snapshot a response before/after: the JSON integer field
>   `author_id` (or `author`, depending on how DTOs are named) is
>   byte-identical to the prior output.
> - DB round-trip: create a comment via POST, confirm the
>   `comments.author_id` column holds the expected integer, reload
>   the server, confirm the comment deserializes with the right
>   `Author` variant.

---

## 3. Flatten the `o11a_core::core` path by renaming `core/` to `domain/`

> You are working in the Rust workspace at `/home/john/o11a-backend`.
> Crate layout: `o11a-core` (library), `o11a-analyze` (bin),
> `o11a-server` (bin), `o11a-web-backend` (lib).
>
> Inside `o11a-core`, the domain types live at `src/core/` so every
> consumer writes `o11a_core::core::AuditData`,
> `o11a_core::core::Topic`, `o11a_core::core::TopicMetadata`. The
> double `core` is an accident — the submodule was named that when
> the crate itself was `o11a-backend`. Rename the submodule to
> `domain`.
>
> This is a mechanical refactor with no semantic change. Do it now,
> in one commit, while the codebase still has one owner.
>
> **Task:**
>
> 1. `git mv crates/o11a-core/src/core crates/o11a-core/src/domain`.
>
> 2. Update `crates/o11a-core/src/lib.rs`:
>    - `pub mod core;` → `pub mod domain;`.
>    - If any `pub use crate::core::*;` re-exports at the crate root
>      exist, rewrite them to `pub use crate::domain::*;`.
>
> 3. Inside `crates/o11a-core/src/`, search-and-replace every
>    `crate::core::` → `crate::domain::`. Grep confirm:
>    `grep -R "crate::core::" crates/o11a-core/src/` returns nothing.
>
> 4. Across the rest of the workspace (`crates/o11a-analyze`,
>    `crates/o11a-server`, `crates/o11a-web-backend`, and any other
>    crate), search-and-replace every `o11a_core::core::` →
>    `o11a_core::domain::`. Grep confirm:
>    `grep -R "o11a_core::core" crates/` returns nothing.
>
> 5. If any code inside `o11a-core` used `use super::core::...`
>    through nested modules, update those to `super::domain::...`.
>
> 6. If any doc comments mention the old path, update them. Example:
>    `crates/o11a-core/src/report.rs` might say "see
>    `crate::core::TopicMetadata`" — rewrite to
>    `crate::domain::TopicMetadata`.
>
> **Verify:**
>
> - `cargo build --workspace` and `cargo test --workspace` pass.
> - Grep the workspace: no hit for `o11a_core::core::` or
>   `crate::core::` inside source files, only `o11a_core::domain::`
>   and `crate::domain::`.
> - The renamed directory still has its `mod.rs` inside (git
>   preserves contents).
> - Documentation files in `docs/` that reference the old path are
>   updated to the new one.

---

## 4. Replace `Result<T, String>` with typed errors via `thiserror`

> You are working in the Rust workspace at `/home/john/o11a-backend`.
> Crate layout: `o11a-core` (library — pipeline, agent tasks, db,
> report), `o11a-analyze` (bin), `o11a-server` (bin),
> `o11a-web-backend` (lib).
>
> Pipeline and agent code currently returns `Result<T, String>`:
> errors are ad-hoc formatted strings. Callers can't branch on
> error kind, and logs lose structure. Switch to typed errors
> using `thiserror`.
>
> **Task:**
>
> 1. Add `thiserror = "1"` to `crates/o11a-core/Cargo.toml`.
>
> 2. Define error enums in these locations, one enum per natural
>    boundary:
>
>    - `crates/o11a-core/src/collaborator/agent/pipeline.rs` (or a
>      sibling `error.rs`): `PipelineError`:
>      ```rust
>      #[derive(Debug, thiserror::Error)]
>      pub enum PipelineError {
>          #[error("audit not found: {audit_id}")]
>          AuditNotFound { audit_id: String },
>          #[error("DataContext mutex poisoned: {0}")]
>          LockPoisoned(String),
>          #[error("agent task failed: {0}")]
>          AgentTask(#[from] crate::collaborator::agent::task::TaskError),
>          #[error("database error: {0}")]
>          Database(#[from] sqlx::Error),
>          #[error("{0}")]
>          Other(String),
>      }
>      ```
>
>    - `crates/o11a-core/src/collaborator/agent/task.rs`:
>      `TaskError` wraps LLM/HTTP errors. Likely variants:
>      `HttpError(reqwest::Error)`, `JsonParse(serde_json::Error)`,
>      `MissingEnv(String)`, `MissingField(&'static str)`,
>      `Other(String)`.
>
>    - `crates/o11a-core/src/analysis_artifact.rs`: `ArtifactError`
>      (if it doesn't already exist per the earlier binary-artifact
>      prompt). Variants: `Io(std::io::Error)`,
>      `Decode(bincode::error::DecodeError)`,
>      `Encode(bincode::error::EncodeError)`,
>      `VersionMismatch { found: u32, expected: u32 }`,
>      `AuditIdMismatch { expected: String, found: String }`.
>
>    - `crates/o11a-analyze/src/analysis.rs` (the renamed
>      `run_analysis`): `AnalysisError` wrapping foundry-compilers
>      errors, parse errors, etc.
>
> 3. Replace every `Result<T, String>` on public functions in those
>    modules with `Result<T, PipelineError>` / `TaskError` / etc.
>    Replace `.map_err(|e| format!(...))` chains with `?` plus
>    `#[from]` variants (or explicit `.map_err(PipelineError::...)`
>    where the conversion isn't implicit).
>
> 4. Server HTTP handlers that call these functions translate typed
>    errors to status codes:
>    - `AuditNotFound` → `StatusCode::NOT_FOUND`.
>    - `AuditIdMismatch` → `StatusCode::BAD_REQUEST`.
>    - Database/Io errors → `StatusCode::INTERNAL_SERVER_ERROR`.
>    - Others default to 500 with the error message in the body or
>      log only, per existing convention.
>
>    Introduce a small extension helper
>    (`into_http_response(err: PipelineError) -> (StatusCode,
>    String)`) to keep handlers terse.
>
> 5. Grep the workspace for `Result<[^,]*, String>` on pub functions
>    and upgrade each site. Private helpers can keep `String` errors
>    if the cost of typing them isn't worth it — use your judgment,
>    but prefer typed at module boundaries.
>
> 6. Don't introduce `anyhow` anywhere. This refactor is about
>    *structure*; `anyhow`'s opacity defeats the point.
>
> **Verify:**
>
> - `cargo build --workspace` and `cargo test --workspace` pass.
> - `grep -R "Result<[^,]*, String>" crates/o11a-core/src/
>   crates/o11a-analyze/src/` returns only private helpers or tests.
> - Clippy is not noisier than before.
> - A handler that returns 404 on unknown audit continues to do so,
>   but the mapping is now `matches!(err, PipelineError::AuditNotFound
>   { .. })` rather than a string match on the error message.

---

## 5. Switch `println!`/`eprintln!` to `tracing`

> You are working in the Rust workspace at `/home/john/o11a-backend`.
> Crate layout: `o11a-core` (library), `o11a-analyze` (bin),
> `o11a-server` (bin), `o11a-web-backend` (lib).
>
> Every log line in the workspace is a `println!` or `eprintln!`.
> Swap to the `tracing` ecosystem: structured fields, log levels,
> span-based timing, environment-variable filtering.
>
> **Task:**
>
> 1. Add to every crate's `Cargo.toml` that currently logs:
>    ```toml
>    tracing = "0.1"
>    ```
>    And to **each binary** (`o11a-analyze`, `o11a-server`):
>    ```toml
>    tracing-subscriber = { version = "0.3", features = ["env-filter"] }
>    ```
>
> 2. In each binary's `main()`, initialize a subscriber at the top:
>    ```rust
>    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
>
>    tracing_subscriber::registry()
>        .with(fmt::layer().with_target(false))
>        .with(EnvFilter::try_from_default_env()
>            .unwrap_or_else(|_| EnvFilter::new("info")))
>        .init();
>    ```
>    This gives `RUST_LOG=debug cargo run` / `RUST_LOG=o11a_core=trace`
>    filtering out of the box. Default level: `info`.
>
> 3. Replace calls mechanically:
>    - `println!("something: {}", x)` → `tracing::info!(x = %x, "something")`
>      (or `info!("something: {}", x)` where structured fields
>      aren't worth extracting).
>    - `eprintln!("Warning: ...")` → `tracing::warn!(...)`.
>    - `eprintln!("error: ...")` → `tracing::error!(...)`.
>    - Use `tracing::debug!` for things that are useful in
>      development but too noisy for info (e.g., per-request
>      handler entry logs).
>
> 4. Add `#[tracing::instrument(skip_all, fields(audit_id = %audit_id))]`
>    to the pipeline step functions in
>    `crates/o11a-core/src/collaborator/agent/pipeline.rs`
>    (`run_full_pipeline`, `build_requirements`, `build_behaviors`,
>    `build_semantic_links`, `synthesize_features`). This gives you
>    per-step timing for free. Skip large arguments with
>    `skip_all`.
>
> 5. For the server's HTTP layer, add `TraceLayer::new_for_http()`
>    from `tower-http` (already a dep) to the router so each request
>    logs with method/path/status/latency:
>    ```rust
>    use tower_http::trace::TraceLayer;
>    let app = router.layer(TraceLayer::new_for_http());
>    ```
>
> 6. Do **not** do any of these:
>    - Change log content/severity to "improve" wording — preserve
>      meaning and level.
>    - Remove a log line because it seems redundant.
>    - Introduce a correlation ID or request ID yet — separate
>      concern.
>
> **Verify:**
>
> - `cargo build --workspace` and `cargo test --workspace` pass.
> - `grep -R "println!\|eprintln!" crates/ --include="*.rs"` returns
>   only: the `Usage: ...` message in `main.rs` files (user-facing
>   CLI output, not logs) and test output. No hits in handlers,
>   pipeline, or agent code.
> - Run `cargo run -p o11a-server` and confirm a normal-looking
>   structured log stream.
> - Run `RUST_LOG=warn cargo run -p o11a-server` and confirm info
>   lines are filtered.

---

## 6. Refactor handlers to not hold `DataContext` locks across `.await`

> You are working in the Rust workspace at `/home/john/o11a-backend`.
> Crate layout: `o11a-core` (library), `o11a-analyze` (bin),
> `o11a-server` (bin), `o11a-web-backend` (lib).
>
> `AppState` holds `data_context: Arc<Mutex<DataContext>>` (a
> `std::sync::Mutex`). Several handlers acquire the lock, then
> `.await` on a database call while still holding it. Clippy flags
> this as `await_holding_lock` — a known latent bug that manifests
> under concurrent load by holding up readers while an HTTP-bound
> await completes.
>
> Fix is **per handler**, not a single type-system swap. Don't
> replace `std::sync::Mutex` with `tokio::sync::Mutex`; that changes
> lock semantics without fixing the root cause (async mutex across
> awaits is correct but still serializes readers pointlessly).
> Instead, restructure each problem handler to drop the lock before
> awaiting.
>
> Optionally, after the handler refactor, swap to
> `parking_lot::RwLock` for concurrent reads — most handlers are
> read-only, and `parking_lot::RwLock` is a drop-in with less
> ceremony than `std::sync::RwLock`. This second step is optional
> within this prompt's scope; do it only if all handler rewrites
> land cleanly and you want the extra win.
>
> **Task:**
>
> 1. Run clippy and collect the list of sites:
>    ```
>    cargo clippy --workspace --all-targets 2>&1 | grep -B2 await_holding_lock
>    ```
>    Record each as a TODO for this prompt.
>
> 2. For each flagged handler, apply the drop-then-await pattern:
>
>    ```rust
>    // BEFORE:
>    let mut ctx = state.data_context.lock().unwrap();
>    let row = db::create_something(&state.db, ...).await?;  // await while holding lock
>    ctx.get_audit_mut(...).unwrap().insert_row(row);
>
>    // AFTER:
>    // Phase 1: do any pre-compute under the lock, then drop.
>    let precomputed = {
>        let ctx = state.data_context.lock().unwrap();
>        // Read anything needed from ctx to build the DB payload.
>        compute_something(&ctx, ...)
>    };
>
>    // Phase 2: DB I/O with no lock held.
>    let row = db::create_something(&state.db, precomputed).await?;
>
>    // Phase 3: re-acquire the lock and apply the in-memory update.
>    {
>        let mut ctx = state.data_context.lock().unwrap();
>        ctx.get_audit_mut(...).unwrap().insert_row(row);
>    }
>    ```
>
>    The pattern is "read → I/O → write" with explicit lock scopes
>    around each phase. Never let a `MutexGuard` outlive a `.await`.
>
> 3. Be careful about consistency: if a handler needs invariants to
>    hold across the await (e.g., "no one inserts a conflicting row
>    in the window between reading and writing"), name the risk in
>    a comment and decide whether to serialize via a DB-level
>    uniqueness constraint or a short lock re-acquire + validate
>    step. In practice, most of our handlers are simple
>    create/read/delete and don't need cross-await serialization —
>    but don't lose that property silently.
>
> 4. **Do not** unwrap mutex `.lock()` calls blindly if the existing
>    code maps poisoning to a 500 response. Preserve that behavior.
>
> 5. After all handler rewrites:
>    - `cargo clippy --workspace --all-targets -- -D
>      clippy::await_holding_lock` should pass.
>    - Optionally add `tower::limit::ConcurrencyLimitLayer` or
>      similar to the server router as a cheap regression guard.
>      Skip if it clutters the layer stack.
>
> 6. **Optional second pass** (same prompt): switch
>    `Arc<Mutex<DataContext>>` to
>    `Arc<parking_lot::RwLock<DataContext>>`. Add
>    `parking_lot = "0.12"` to `crates/o11a-core/Cargo.toml`.
>    Update `AppState::new` and every `.lock()` / `.lock().unwrap()`
>    to `.read()` or `.write()` depending on mutation. Handlers that
>    only read become read-only, allowing parallel access.
>
> **Verify:**
>
> - `cargo build --workspace` and `cargo test --workspace` pass.
> - `cargo clippy --workspace --all-targets -- -D
>   clippy::await_holding_lock` passes (zero warnings of that kind).
> - Manual smoke test: hit a handler that was on the flagged list
>   with `curl` and confirm it still behaves correctly.
> - If the optional `parking_lot::RwLock` swap was applied: run the
>   server under a two-client concurrent read test (two `curl`
>   commands piped to `&` hitting a read-heavy endpoint) and confirm
>   they don't serialize.

---

## 7. Review the types-pass work end to end

> You are a third-party reviewer auditing recent work in the Rust
> workspace at `/home/john/o11a-backend`. You have no prior context
> beyond this prompt. Your job is to verify that tasks **1–6** in
> `docs/follow_up_prompts.md` (the "types pass") were implemented
> correctly and that the workspace is in a coherent end state.
> Produce a written report, do not make code changes.
>
> ### Project at a glance
>
> Four crates:
> - `o11a-core` — library. Domain types (`AuditData`, `Topic`,
>   `TopicMetadata`, `Author`), pipeline, report schema,
>   analysis-artifact serialization, collaboration persistence.
> - `o11a-analyze` — binary. Source parsers + analyzers + pipeline.
>   Writes `<project_root>/o11a/audit.json` and
>   `<project_root>/o11a/audit.analysis.bin`.
> - `o11a-server` — binary. Loads both files, serves the HTTP API
>   and WebSocket.
> - `o11a-web-backend` — library. HTML rendering.
>
> The six tasks you are auditing:
> 1. `Topic` became a typed enum, wire format preserved as prefixed
>    string.
> 2. `Author` became a typed enum, wire format preserved as integer.
> 3. `o11a-core`'s `core` submodule was renamed to `domain`.
> 4. `Result<T, String>` was replaced with typed error enums on
>    module boundaries (`PipelineError`, `TaskError`,
>    `ArtifactError`, `AnalysisError`).
> 5. `println!`/`eprintln!` was replaced with `tracing` macros;
>    binaries initialize a subscriber.
> 6. Handlers no longer hold `DataContext` locks across `.await`.
>
> ### Audit checklist
>
> **Build hygiene.** Run and report:
> - `cargo build --workspace`
> - `cargo test --workspace` — report PASS/FAIL and test count
> - `cargo clippy --workspace --all-targets -- -D clippy::await_holding_lock`
> - `cargo fmt --all --check`
>
> **Per-task verification.**
>
> 1. **`Topic` enum.**
>    - `crates/o11a-core/src/domain/topic.rs` (note: path changed in
>      task 3) declares `pub enum Topic { Node(i32), Documentation(i32),
>      Comment(i32), Invariant(i32), AttackVector(i32), Feature(i32),
>      Requirement(i32), Behavior(i32), FunctionalProperty(i32),
>      TypeConstraint(i32) }`.
>    - `impl Serialize for Topic` emits the prefixed string form.
>      Spot-check: serialize a `Topic::Feature(42)` and confirm
>      output is `"F42"`.
>    - `impl<'de> Deserialize<'de> for Topic` round-trips the string.
>    - `TopicKind` (the old accompanying enum) no longer exists.
>    - `grep -R "Topic::kind\(\)" crates/` shows no or few hits;
>      callers pattern-match on `Topic` directly.
>    - `grep -R "\.id: String" crates/o11a-core/src/domain/topic.rs`
>      returns nothing — the struct field is gone.
>    - Existing `audit.json` files still load (wire format
>      unchanged).
>
> 2. **`Author` enum.**
>    - `crates/o11a-core/src/collaborator/models.rs` declares
>      `pub enum Author { System, DevTechnical, DevDocumentation,
>      AgentMicro, AgentSmall, AgentMedium, AgentLarge, User(u64) }`
>      or similar.
>    - Serialize/Deserialize produce a plain integer (use `serde_json`
>      snapshot test to confirm).
>    - `TopicMetadata` variants' `author_id: i64` fields are now
>      `author: Author`.
>    - DB read/write uses conversion helpers.
>    - `grep -R "AUTHOR_AGENT_LARGE\|AUTHOR_SYSTEM" crates/` shows
>      the constants still exist but are used only in DB migration
>      or test code — not in domain logic.
>
> 3. **`core` → `domain` rename.**
>    - `crates/o11a-core/src/core/` does not exist.
>    - `crates/o11a-core/src/domain/` does exist.
>    - `grep -R "o11a_core::core" crates/` returns zero hits.
>    - `grep -R "crate::core::" crates/o11a-core/src/` returns zero
>      hits.
>    - `crates/o11a-core/src/lib.rs` declares `pub mod domain;`.
>
> 4. **Typed errors.**
>    - `thiserror` is in `crates/o11a-core/Cargo.toml`.
>    - `PipelineError`, `TaskError`, `ArtifactError`, and similar
>      enums exist with `#[derive(thiserror::Error)]`.
>    - Public pipeline/agent function signatures use typed errors,
>      not `Result<_, String>`. Spot-check
>      `run_full_pipeline`, `build_requirements`,
>      `build_behaviors`, `synthesize_features`.
>    - HTTP handlers pattern-match error variants to produce
>      appropriate status codes (e.g., `NotFound` → 404).
>    - `anyhow` is **not** a dependency anywhere.
>
> 5. **`tracing`.**
>    - `tracing` is a workspace dep; `tracing-subscriber` is in each
>      binary's Cargo.toml.
>    - `main()` in `crates/o11a-analyze/src/main.rs` and
>      `crates/o11a-server/src/main.rs` initializes a subscriber
>      with `EnvFilter`.
>    - `grep -R "println!\|eprintln!" crates/ --include="*.rs"`
>      returns only: CLI `Usage:` text in `main.rs` files, and
>      test code. No hits in handlers, pipeline, or agent modules.
>    - Pipeline step functions have `#[tracing::instrument]`.
>    - The server router includes `TraceLayer::new_for_http()`.
>    - `RUST_LOG=warn cargo run -p o11a-server` starts and logs less
>      than the default.
>
> 6. **No lock across await.**
>    - `cargo clippy --workspace --all-targets -- -D
>      clippy::await_holding_lock` passes.
>    - Handler bodies visibly follow the "read → drop lock → await
>      → re-acquire → write" pattern.
>    - (Optional) `parking_lot::RwLock` is present in core; handlers
>      call `.read()` / `.write()`.
>
> ### Cross-cutting checks
>
> - **No dead code.** `cargo build --workspace 2>&1 | grep -i
>   "unused"` — flag any unused imports or functions left over
>   from the refactor.
> - **Documentation drift.** Grep `docs/` for references to the old
>   `Topic` struct, `TopicKind` enum, `core::` module path,
>   `author_id: i64`, or `println!` logging style. All should be
>   updated.
> - **Smoke test, if feasible.**
>   1. `cargo run -p o11a-analyze -- <project> <audit_id>` — still
>      produces both files in `<project>/o11a/`.
>   2. `cargo run -p o11a-server` (with appropriate env) — starts
>      and serves.
>   3. `curl /api/v1/audits/<id>/features` — response JSON shape
>      byte-identical to pre-refactor (run a diff against a saved
>      sample if available).
>   4. `curl -X POST /api/v1/audits/<id>/features -d '{...}'` —
>      creates a user feature as before.
>
> ### Report format
>
> Produce a concise report (under 800 words) with three sections:
> - **PASS** — tasks that look correctly implemented. One line
>   each.
> - **ISSUES** — concrete problems, each tied to a task number with
>   a file path and the specific concern.
> - **FOLLOW-UPS** — drift or nice-to-haves that aren't bugs but
>   are worth cleaning up (stale doc strings, orphaned constants,
>   tests that could be sharpened now that errors are typed).
>
> Do not attempt to fix anything. Produce the report only.
