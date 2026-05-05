# Semantic Linking — Pipeline Spec

## Overview

Five-step pipeline that links documentation sections to code declarations and assigns project-specific semantics to those declarations. Steps alternate between **association** (mechanical anchors + BM25 expansion) and **synthesis** (LLM generation), so that semantics produced in earlier synthesis steps are available as context for later ones:

1. **Step 1 — associate document sections to contracts.** Mechanical anchor resolution (`context::mechanical_semantic_links`, Phase A + every graph-resolver phase) plus BM25 contract discovery (top-K above `MIN_SCORE`).
2. **Step 2 — add semantic links to contracts.** LLM call per section: given the section text and the matched contracts (each with name + contract-level NatSpec + a list of public member names), generate one semantic per contract entity. After this step, every contract that has any links is condensed down to a single link.
3. **Step 3 — associate document sections to contract members.** Mechanical seed (members reached by anchored declarations and by state-variable mutation fanout) plus BM25 member expansion within each anchored contract (top-K above `MIN_SCORE` per contract, with the short-document length floor described below).
4. **Step 4 — add semantic links to contract members.** Two LLM batches per section:
   - *Member-scoped batch* — function and modifier topics from step 3, batched together with their parameters and return values. Each containing contract's already-condensed step-2 semantic is injected into the prompt as context.
   - *Contract-scoped batch* — non-function component-scoped declarations (state variables, events, errors, struct/enum definitions, struct fields, enum members) for the section's matched contracts. Same contract-context injection.

   After this step, every member-level declaration that has any links is condensed down to a single link.
5. **Step 5 — add semantic links to contract member bodies.** LLM call per section: locals declared inside each member body (`Scope::ContainingBlock`). The containing contract's semantic from step 2 and the containing member's (and its params/returns') semantics from step 4 are injected into the prompt as context, so a body statement like `let ret = Contract.transfer(input, to)` can be interpreted with `Contract`, `transfer`, `input`, and `to` already meaningful. After this step, every body-local declaration with any links is condensed down to a single link.

**Why the alternation matters.** Once step 2 has assigned a semantic to a contract, step 4's prompt for that contract's members can read "this contract represents X" and produce member semantics in terms of X. Once step 4 has assigned semantics to functions and their params, step 5 can interpret a local like `let amount = ...` against the already-known meaning of the value flowing into it. Concretely, a body statement like `let ret = Contract.transfer(input, to)` produces much sharper semantics for `ret`, `input`, and `to` when the contract, function, and signature semantics are already known.

**Per-step condensation.** The pipeline aspires to one semantic per code declaration. Because steps 2, 4, and 5 may each produce multiple links for the same declaration (one per section that referenced it, since context is rendered cross-section), we condense in place after each synthesis step. Topics with a single link pass through; topics with multiple links go through one `task::condense_semantics` LLM call. The accumulator stays in memory until the very end of the pipeline, when the condensed links are written into `audit_data.topic_metadata` as `FunctionalSemanticTopic` entries.

## Why one workflow

The earlier design routed sections by an `is_technical` flag: technical docs got mechanical-only association + LLM synthesis, non-technical docs got LLM all the way through. We also kept BM25 and an LLM association path behind opt-in flags for evaluation.

After running the comparison harness (`--semantic-linking-compare-all`, since deleted) across multiple BM25 cutoff variants and the LLM workflow on a real audit, the data showed:

- Mechanical-only adds zero unique signal pairs (everything it surfaces is also surfaced by BM25 or LLM association). It's a precision floor, not a recall contributor.
- BM25 K=10 per (section, contract) achieves 67.6% downstream LLM SNR — 3× the permissive variant — at 25% of the candidate volume. The K=10 batch size (~40 candidates) keeps the LLM batches within reliable API limits.
- LLM-driven association catches roughly 12% unique pairs but at substantially higher per-call cost and latency. Dropping it removes some recall in exchange for cheaper, more predictable runs.
- The previous routing flag's effect was largely explained by the BM25 / LLM choice underneath; with one path, routing is no longer needed.

The collapsed pipeline is what production uses. The detailed evaluation log is in the project's commit history; the BM25 length-floor variant and the K=10 cutoff are the two empirical defaults that fell out of it.

## BM25 details

### Length-floored scoring (`bm25/score.rs`)

Standard BM25 with one modification — a length floor on the document-length normalization:

```text
IDF(qi) = ln((N - n(qi) + 0.5) / (n(qi) + 0.5) + 1)
eff_dl  = max(|D|, avgdl * MIN_LENGTH_RATIO)

score(D, Q) = sum over qi in Q of:
    IDF(qi) * (f(qi, D) * (k1 + 1))
    ----------------------------------------------
    (f(qi, D) + k1 * (1 - b + b * eff_dl / avgdl))
```

Defaults: `k1 = 1.2`, `b = 0.75`, `MIN_LENGTH_RATIO = 0.75`.

The floor exists because raw BM25 over a per-contract member corpus inflates the scores of very short member documents (1–3 tokens — bare identifier names with no NatSpec). Empirically, those documents dominated the top decile of scores but had near-zero LLM-synthesis acceptance — pure length-confound noise. Treating any document shorter than `0.75 * avgdl` as if it were exactly that long bounds the bonus they receive without zeroing length normalization for mid-length docs. This is similar in spirit to a pivoted-length normalization but simpler — it's not a published variant (BM25L, BM25+, etc. address the opposite bias).

### Cutoff (`bm25.rs::cutoff`)

Top-K above absolute floor — a single parameterless function:

```rust
fn cutoff<T>(scored_desc: &[ScoredCandidate<T>]) -> Vec<usize> {
    scored_desc
        .iter()
        .enumerate()
        .filter(|(_, c)| c.score >= constants::MIN_SCORE)
        .take(constants::TOP_K)
        .map(|(i, _)| i)
        .collect()
}
```

Defaults: `MIN_SCORE = 1.0`, `TOP_K = 10`, `STEP1_TOP_K = 10`. The K=10 cutoff was calibrated against a 9-section audit; raising or lowering it should be re-justified with a fresh evaluation, not a flag.

### Tokenization

Both step 1 and step 3 share a tokenization pipeline:

```text
1. Operator expansion         (raw source-derived text only)
2. Identifier splitting       (camelCase + snake_case + acronym handling)
3. Abbreviation + domain expansion  (per token)
4. Lowercase
5. Stop-word removal          (bm25 crate)
6. Porter stem                (bm25 crate)
```

Steps 1, 2, and 3 are custom (`bm25/tokenize.rs`); 4–6 are the `bm25` crate's defaults. Steps 1 and 3 (the expansion steps) apply only to code-derived documents (member signatures, NatSpec, source text). Documentation prose queries skip them — prose contains no raw operators or code abbreviations to expand. Asymmetric tokenization is fine; both sides converge on English words BM25 can score.

#### Identifier splitting rules

Apply to every alphanumeric-with-underscore token:

| Input | Output |
|-------|--------|
| `computeShares` | `["compute", "shares"]` |
| `participation_id` | `["participation", "id"]` |
| `PARTICIPATION_ID` | `["participation", "id"]` |
| `IERC20` | `["ierc20"]` |
| `URLParser` | `["url", "parser"]` |
| `parseURL` | `["parse", "url"]` |
| `_internalState` | `["internal", "state"]` |
| `parse2Numbers` | `["parse", "2", "numbers"]` |

Rules:
- Split on `_` (snake_case, SCREAMING_SNAKE_CASE).
- Split on lowercase→uppercase boundary (camelCase): `computeShares` → `compute|Shares`.
- Split on letter→digit and digit→letter boundaries: `parse2Numbers` → `parse|2|Numbers`.
- For uppercase runs followed by a lowercase letter (acronym + word, e.g., `URLParser`), split such that the trailing uppercase joins the lowercase word: `URL|Parser`. Keeps `URL` as a single acronym token rather than `U|R|L`.
- For pure-uppercase tokens with embedded digits (`IERC20`, `ERC721`), do not split — these are recognizable acronyms documentation refers to verbatim.
- Strip leading underscores (`_internalState` → `internalState` → `internal|State`); they're a Solidity convention, not semantic content.
- Drop emitted tokens of length 1 (apart from digits) — single-letter tokens introduce noise without IDF discrimination.

#### Code-to-word expansion

**Operators** (mandatory, fixed table, applied as Step 1):

| Symbol | Expansion | | Symbol | Expansion |
|--------|-----------|-|--------|-----------|
| `+`    | add       | | `&&`   | and       |
| `-`    | subtract  | | `\|\|` | or        |
| `*`    | multiply  | | `!`    | not       |
| `/`    | divide    | | `++`   | increment |
| `%`    | modulo    | | `--`   | decrement |
| `=`    | update    | | `+=`   | increment |
| `==`   | equal     | | `-=`   | decrement |
| `!=`   | unequal   | | `*=`   | multiply  |
| `<`    | less      | | `/=`   | divide    |
| `>`    | greater   | | `%=`   | modulo    |
| `<=`   | less equal | | `=>`  | maps      |
| `>=`   | greater equal | | `&` | bitand    |
| `<<`   | shift left | | `\|`  | bitor     |
| `>>`   | shift right | | `^`   | xor       |
| `~`    | bitnot    | |        |           |

Notes: `=` → `update` (matches doc phrasing like "the function updates X"); compound assignments collapse to `increment`/`decrement`; bitwise operators get `bit*` prefixes to avoid clashing with logical `and`/`or`. Match longest-first when a shorter operator is a prefix of a longer one (`==` before `=`). Operators only expand outside identifiers and string literals.

**Abbreviations** (mandatory, fixed table, applied per-token after splitting):

| Token | Expansion | | Token | Expansion |
|-------|-----------|-|-------|-----------|
| `id`   | identifier | | `ref`  | reference |
| `idx`  | index    | | `arr`  | array     |
| `qty`  | quantity | | `tmp`  | temporary |
| `amt`  | amount   | | `cfg`  | config    |
| `bal`  | balance  | | `init` | initialize |
| `acc`  | account  | | `dest` | destination |
| `acct` | account  | | `src`  | source    |
| `tx`   | transaction | | `prev` | previous  |
| `txn`  | transaction | | `addr` | address  |
| `msg`  | message  | | `recv` | receive   |
| `len`  | length   | | `num`  | number    |
| `cnt`  | count    | |        |           |

Comparison is case-insensitive (lookup happens after lowercasing). Skipped: ambiguous abbreviations like `pid` (could be participation id, process id, payer id) — project-specific and not in the universal table.

**Solidity domain terms** (mandatory, fixed table; compound-term detection runs before operator expansion so `.` doesn't get expanded inside these phrases):

| Term | Expansion |
|------|-----------|
| `msg.sender`     | caller |
| `msg.value`      | amount sent |
| `msg.data`       | calldata |
| `block.timestamp` | time |
| `block.number`   | block height |
| `tx.origin`      | originator |
| `payable`        | receives ether |
| `view`           | read only |
| `pure`           | read only |

**Not expanded** (explicit decisions):

- **Type names** (`uint256`, `bytes32`, `bool`, `string`): kept as-is. Documentation often uses them verbatim.
- **Visibility modifiers** (`external`, `public`, `private`, `internal`): kept as-is. Too generic to expand usefully.
- **Method-name prefixes** (`set`, `get`, `is`, `has`): not expanded — identifier splitting exposes them as standalone tokens.
- **Numeric literals**: stripped during normal tokenization.
- **Solidity keywords without obvious English equivalents** (`mapping`, `storage`, `memory`, `calldata`): kept as-is.

## Output and provenance

Each functional semantic is persisted with:

- The semantic text (the synthesis step's output, after per-step condensation).
- A provenance link to every documentation topic that contributed to it. Cross-section condensation collects all source sections' doc topics into one merged list.
- A `match_source` field on each (section, member) pairing, single-valued: `mechanical` (mechanical association produced it) or `bm25` (BM25 expansion did). The dominant source across a synthesis batch's input wins; `mechanical` outranks `bm25`.

## Crate choices

- **`bm25` (v2.3.2)** — handles scoring, default tokenizer, stop-words, stemming, deunicode. Right-sized for in-process scoring against small candidate sets. We use the crate's tokenizer building blocks but supply our own scoring loop (`bm25/score.rs`) so we can apply the length floor.
- **No tantivy.** Overkill — we don't need indexing/querying.

`bm25` is in `crates/o11a-analyze/Cargo.toml` with the `default_tokenizer` feature.

## Configuration flags

The pipeline has no production-tunable flags — defaults are baked in. The only `--semantic-linking-*` flag still wired is `--semantic-linking-mechanical-trace` (debugging-only):

### `--semantic-linking-mechanical-trace` / `O11A_SEMANTIC_LINKING_MECHANICAL_TRACE`

When set, run only the mechanical halves of step 1 + step 3 (no LLM, no BM25, no synthesis steps), write a JSON trace of every section's resolved/unresolved inline-code references and derived contract/member candidates to `<output_dir>/mechanical-trace.json`, then exit. Used to validate the deterministic name resolver in isolation when debugging.

## Implementation files

- `crates/o11a-core/src/collaborator/agent/pipeline.rs::build_semantic_links` — the pipeline entry point, all five steps.
- `crates/o11a-core/src/collaborator/agent/context.rs::mechanical_semantic_links` — the mechanical resolver (anchors + member fanout) used as both the seed and the standalone trace mode.
- `crates/o11a-core/src/collaborator/agent/semantic_linking.rs` — config struct, CLI parsing, `is_technical` lookup helper.
- `crates/o11a-core/src/collaborator/agent/semantic_linking/bm25.rs` — step 1 contract discovery (`discover_top_k_contracts`) and step 3 member expansion (`expand_members`), plus the cutoff function.
- `crates/o11a-core/src/collaborator/agent/semantic_linking/bm25/score.rs` — length-floored BM25 scoring.
- `crates/o11a-core/src/collaborator/agent/semantic_linking/bm25/corpus.rs` — per-contract member corpus and contract-summary corpus assembly.
- `crates/o11a-core/src/collaborator/agent/semantic_linking/bm25/tokenize.rs` — operator / identifier / abbreviation / domain-term tokenization.
- `crates/o11a-core/src/collaborator/agent/semantic_linking/trace.rs` — the mechanical-trace JSONL writer.
- `crates/o11a-core/src/collaborator/agent/task.rs::link_contracts` (step 2), `task::link_member_signatures` (step 4), `task::link_member_bodies` (step 5) — the three LLM synthesis tasks.
