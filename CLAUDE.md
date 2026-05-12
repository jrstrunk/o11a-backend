# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`o11a-backend` is the Rust backend for o11a, a tool for performing smart-contract audits collaboratively with AI agents. It parses smart contract source code plus project documentation, builds a topic-addressable model of the codebase, and runs an LLM-driven pipeline that produces a structured security model (requirements, behaviors, features, functional semantics, conditions, threats, invariants) which auditors and agents collaboratively refine.

The thinking behind the pipeline lives in the root `README.md` and in `crates/o11a-core/SPEC.md`. Read those before changing pipeline behavior — many of the "weird" choices (backward-only context, conditions-before-threats, etc.) are deliberate and explained there.

## Workspace layout

Cargo workspace with four crates in `crates/`:

- **`o11a-core`** — Parser, analyzer, checker, security-model pipeline, agent infrastructure, SQLite persistence. Pure logic + DB; no HTTP. Most pipeline code lives under `src/collaborator/agent/` (notably `pipeline.rs`, `semantic_linking.rs`, `task.rs`). Topic-id allocation lives in `src/ids.rs`. The in-memory model is `domain::DataContext`.
- **`o11a-analyze`** — Batch CLI binary. Subcommands: `analyze` (full pipeline), `dump` (diagnostic JSON dumps), `normalize-docs` (LLM doc normalization). Writes `<project_root>/o11a/audit.json` and `<project_root>/o11a/audit.analysis.bin`.
- **`o11a-server`** — Axum HTTP + WebSocket server. Loads the artifact + report produced by `o11a-analyze`, hydrates user-created entities and comments from SQLite, then serves the audit. Listens on `0.0.0.0:3058`. Reads `PROJECT_ROOT` and `AUDIT_ID` env vars; overridable `AUDIT_REPORT` / `AUDIT_ARTIFACT` paths.
- **`o11a-web-backend`** — HTML formatting layer (Solidity / documentation / comment formatters, topic view, formatting helpers) and the HTML-returning HTTP routes. Server `main.rs` merges its router into the core router so both share `AppState`. No analysis logic.

The client (Gleam frontend) lives in a separate repository.

## Build / run / test

```bash
# Build the whole workspace
cargo build

# Run the batch analysis pipeline (requires OPENROUTER_API_KEY for real LLM calls;
# set AGENT_DRY_RUN=1 to stub them out)
cargo run --bin o11a-analyze -- analyze <project_root> <audit_id>

# Diagnostic dump (no LLM)
cargo run --bin o11a-analyze -- dump <project_root> <audit_id> all

# Normalize documentation files listed in <project_root>/documents.txt
cargo run --bin o11a-analyze -- normalize-docs <project_root>

# Run the server (requires audit.analysis.bin + audit.json already produced)
PROJECT_ROOT=/path/to/audit AUDIT_ID=<id> cargo run --bin o11a-server

# Tests — whole workspace, single crate, single test
cargo test
cargo test -p o11a-core
cargo test -p o11a-analyze documentation::resolution_pass_tests::

# Format / lint
cargo fmt
cargo clippy
```

`rustfmt.toml` pins `tab_spaces = 2`, `max_width = 80`. Match that.

The server writes/reads SQLite at `data/o11a.db` by default (`DATABASE_URL` overrides). `init_schema` is idempotent (CREATE IF NOT EXISTS) and runs on startup.

## How the pipeline fits together

The two-binary split (`o11a-analyze` produces an artifact, `o11a-server` consumes it) is load-bearing: the server no longer reads the project source tree. All AST/topic state comes from `audit.analysis.bin` (bincode) plus `audit.json` (pipeline report). Schema changes require bumping `ARTIFACT_SCHEMA_VERSION` in `o11a-core::analysis_artifact` and regenerating the artifact, or the server refuses to load.

Pipeline ordering (see `crates/o11a-core/src/collaborator/agent/pipeline.rs` and `semantic_linking.rs`):

1. Parse — Solidity via Foundry compilers (`forge build --ast` must have run in the audit project to produce the JSON ASTs the parser reads); documentation via the `markdown` crate.
2. Analyze — two-pass scope walk producing `DataContext` (declarations, references, scopes, function/modifier extended properties).
3. Semantic linking — five-step pipeline alternating mechanical/BM25 association with LLM synthesis. Steps 1–2 do contract semantics; steps 3–4 do member semantics; step 5 does body-local semantics. Each synthesis step's output feeds the next as context. The full design lives in `docs/specs/semantic-linking.md`.
4. Requirement extraction (docs) → behavior extraction (code, with semantics injected) → feature synthesis via reconciliation.
5. Threats are generated from conditions on non-pure subjects; invariants attach to subjects, not to abstract structures. See SPEC.md for the full state machine.

## Topic IDs: the universal audit-artifact addressing system

`o11a-core/src/domain/topic.rs` and `o11a-core/src/ids.rs` together define the single addressing scheme that everything in an audit attaches to — source nodes, documentation sections, comments, and every security-model artifact. This is what lets a comment on one artifact link to and cross-reference any other artifact in the audit (a comment on a function can point at a requirement, a threat, another comment, a doc section, etc., using the exact same id surface).

- `domain::topic::Topic` is the enum of variants, each one a `(prefix_char, i32)` pair with a uniform wire format of `prefix + signed integer`:
  - `N` Node (source AST node)
  - `D` Documentation
  - `C` Comment
  - `B` Behavior
  - `F` Feature
  - `R` Requirement
  - `P` FunctionalProperty (shared by `FunctionalSemanticTopic`, `FunctionalPurposeTopic`, `PlacementRationaleTopic`)
  - `A` AdversarialProperty (shared by `ConditionTopic`, `ThreatTopic` and `InvariantTopic`)
  - `Y` TypeConstraint
- `ids.rs` owns the atomic counters that allocate the numeric suffix for each prefix, plus `reseed_*` functions used during artifact/DB hydration so freshly allocated user IDs never collide with pipeline-generated ones. The split between `allocate_*` and `reseed_*` is load-bearing — `o11a-server::main` calls `reseed` after applying the report and again after loading user entities so the `i32` space stays unified.

Wire format, DB columns, the JSON report, and the in-memory model all use the same `prefix+integer` form. When adding a new artifact kind, extend `Topic`, add an `allocate_*`/`reseed_*` pair in `ids.rs`, and the rest of the system (comments, references, approvals, agent tasks) automatically lights up because they all key off `Topic`.

## Conventions worth knowing

- **Standalone functions over methods.** Per `.rules`. Prefer free functions in a module over `impl` methods unless there's a strong type-ergonomics reason.
- **Look before adding types/functions.** The codebase has a lot of cross-cutting types (`DataContext`, `AuditData`, scope kinds, topic ids). Check `o11a-core/src/domain/` and `ids.rs` before introducing new ones.

## Where things live (quick map for unfamiliar regions)

- AST + parser entry points: `o11a-analyze/src/{solidity,documentation,rust}/parser.rs`
- Per-language analyzer (in-scope walking, scope construction): `o11a-analyze/src/{solidity,documentation}/analyzer.rs`
- Pipeline orchestrator: `o11a-core/src/collaborator/agent/pipeline.rs`
- LLM task wrapper + prompt routing: `o11a-core/src/collaborator/agent/{task.rs,router.rs}`
- Agent context assembly (backward-only by design): `o11a-core/src/collaborator/agent/context.rs`
- Topic enum + parse/format (the universal artifact address): `o11a-core/src/domain/topic.rs`
- Topic-id allocation counters + reseed hooks: `o11a-core/src/ids.rs`
- Resolution graph (cross-language reference resolution + PageRank for surprise scoring): `o11a-core/src/resolution_graph/`
- Web formatter (40-char vertical layout, see `o11a-web-backend/SPEC.md`): `o11a-web-backend/src/{solidity_formatter,documentation_formatter,comment_formatter,formatting,topic_view}.rs`
- DB schema + migrations: `o11a-core/src/db/mod.rs` and `o11a-core/src/collaborator/db/`
