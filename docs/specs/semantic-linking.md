# Semantic Linking — Implementation Plan

## Overview

Implement the three-pass semantic linking pipeline described in `README.md` (the "Semantic Linking" section under Knowledge Pipeline). This spec records the implementation plan, including a routing strategy that uses the admin-supplied `is_technical` document flag to choose between an LLM-driven workflow (for non-technical prose) and a BM25-driven workflow (for technical documentation), and configuration flags that let both workflows run side-by-side for comparison.

The pipeline is greenfield. There is no existing semantic linking code to migrate — the analyzer (`crates/o11a-analyze/src/documentation/analyzer.rs`) currently builds documentation topics and resolves inline references but does not assign functional semantics to declarations.

## Routing — `is_technical` selects the workflow

Each document already carries an `is_technical: bool` flag, set by the auditor in `documents.txt` (parsed in `crates/o11a-core/src/domain/mod.rs`, used in `crates/o11a-analyze/src/documentation/analyzer.rs`). This flag is the routing signal:

- **`is_technical = true`** → mechanical-only workflow for Passes 1 & 2, LLM for Pass 3.
- **`is_technical = false`** → LLM workflow for all three passes (the spec described in `README.md`).

Rationale: technical documents (API references, contract specs, NatSpec-style prose) are written around code entities and densely populated with inline references. The mechanical layer — anchor resolution, scope walking, and state-variable mutation fanout — captures the relationships those docs encode. Adding BM25 on top buys little marginal recall for the calibration cost it imposes. Non-technical documents (whitepapers, threat models, design rationale) describe concepts in abstract prose with sparse or no inline references; only the LLM's reading comprehension can resolve those reliably.

BM25 remains implemented (see "BM25 expansion" below) but is **not** in the default workflow for either document class and will never be promoted to a default. It exists exclusively as an evaluation tool — usable via `--semantic-linking-mode=bm25` for one-off runs and via `--semantic-linking-compare-all` for side-by-side comparison.

The routing decision is per-document, not per-section. Sections inherit their document's flag.

Pass 3 always uses the LLM regardless of routing — semantic synthesis is generation, not ranking, and no non-AI alternative produces the short, label-shaped output the downstream pipeline consumes.

## The mechanical-only workflow (technical documents, default)

### Pass 1 — section to contracts

Mechanical only: collect declarations resolved from the section's inline code references, walk each declaration's scope chain upward to find its containing contract. The resulting set is the Pass 1 output. No BM25, no LLM.

If a technical-doc section has zero mechanical anchors, it produces zero Pass 1 matches in the default workflow. This is intentional — the auditor labeled the document technical, which implies the section ought to be anchored. Unanchored technical sections are surfaced in the analysis report so the auditor can either add references or relabel the document. (Compare run via `--semantic-linking-mode=llm` if you want to see what an LLM would have inferred.)

### Pass 2 — section × contract to members

Mechanical only: for each contract resolved in Pass 1, walk member scopes and fan out from state-variable references to members that read or write each variable (uses the analyzer's tracked mutations). The resulting set is the Pass 2 output.

### Pass 3 — section × member to semantics

LLM only. Input: documentation section, list of declaration names + topic IDs needing semantics, member source code for disambiguation. Same as the README spec.

## BM25 expansion (alternative, available behind flags)

Implemented but not in any default workflow. Used by `--semantic-linking-mode=bm25` and `--semantic-linking-compare-all`.

### Pass 1 BM25

Skipped — same reasoning as the mechanical-only workflow above. Cross-contract BM25 expansion is not currently implemented.

### Pass 2 BM25 expansion within contract

For each contract resolved by the mechanical Pass 1:

- Build a candidate corpus from the contract's members. Each "document" is the concatenation of: member signature, identifier-split member name, and any NatSpec/comments attached to the member.
- The query is the section's prose (after the same tokenization).
- Score each candidate with BM25.
- Apply the configured cutoff algorithm (see "Cutoff algorithms") to decide which candidates pass.
- Union with the mechanical Pass 2 matches.

### Pass 3

Same LLM step as everywhere else.

## The LLM workflow (non-technical documents)

Exactly as described in `README.md`. The mechanical pre-step still runs and its output is marked "confirmed" in the LLM prompt, but the LLM is the decision-maker for which contracts and members are relevant. No BM25 involvement.

## Cutoff algorithms

BM25 produces a ranked list of candidates with scores. Two algorithms are implemented for Pass 2 to support side-by-side comparison.

### Algorithm A — gap (default)

Two-stage cutoff: hard floor + relative gap detection.

```
1. Sort candidates by BM25 score descending.
2. Normalize: norm[i] = score[i] / score[0].
3. Hard floor: drop candidates where norm[i] < FLOOR.
4. On survivors, find the largest relative gap:
     gap[i] = norm[i+1] / norm[i]   (smaller = bigger drop)
5. If min(gap[i]) < GAP_RATIO, cut at that boundary.
6. Otherwise (no clear elbow), keep all survivors.
7. Safety gate: if score[0] < MIN_TOP_SCORE, return empty
   (BM25 has no confident answer; let nothing through).
```

Constants (shipped defaults; refine manually after observing comparison output):

- `FLOOR = 0.20` — clearly noise.
- `GAP_RATIO = 0.60` — drop must be at least 40% of the prior normalized score.
- `MIN_TOP_SCORE = 1.0` — absolute BM25 score floor below which BM25 is treated as "no confident answer" and returns empty.

### Algorithm B — top-k-floor (alternative)

Simpler baseline: take the top K candidates above an absolute score floor.

```
1. Sort candidates by BM25 score descending.
2. Drop candidates where score[i] < MIN_SCORE.
3. Take at most TOP_K of the remaining.
```

Constants (shipped defaults; refine manually after observing comparison output):

- `MIN_SCORE = 1.0` — absolute score floor.
- `TOP_K = 5` — caps the number of matches per (section, contract) pair.

This is the predictable, low-variance alternative — useful as a baseline and as a fallback if Algorithm A's gap behavior turns out to be too sensitive in practice.

Pass 1 uses no cutoff algorithm in the BM25 workflow because we skip BM25 expansion there (see Pass 1 step 2). If we ever introduce cross-contract BM25 expansion, it should reuse Algorithm A.

## Tokenization

Both algorithms depend on tokenization quality. The pipeline:

```
1. Operator expansion         (raw source-derived text only — see below)
2. Identifier splitting       (camelCase + snake_case + acronym handling)
3. Abbreviation + domain expansion  (per token)
4. Lowercase
5. Stop-word removal          (bm25 crate)
6. Porter stem                (bm25 crate)
```

Steps 1, 2, and 3 are custom; 4–6 are the `bm25` crate's default tokenizer.

Steps 1 and 3 (the expansion steps) apply **only to code-derived documents** (member signatures, NatSpec, source text). Documentation prose queries skip them — prose contains no raw operators or code abbreviations to expand. Asymmetric tokenization is fine; both sides converge on English words BM25 can score.

### Identifier splitting rules

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
- For uppercase runs followed by a lowercase letter (acronym + word, e.g., `URLParser`), split such that the trailing uppercase joins the lowercase word: `URL|Parser`. This keeps `URL` as a single acronym token rather than `U|R|L`.
- For pure-uppercase tokens with embedded digits (`IERC20`, `ERC721`), do not split — these are recognizable acronyms that documentation refers to verbatim.
- Strip leading underscores (`_internalState` → `internalState` → `internal|State`); they're a Solidity convention, not semantic content.
- Drop emitted tokens of length 1 (apart from digits) — single-letter tokens introduce noise without IDF discrimination.

### Code-to-word expansion

#### Category 1 — Operators (mandatory, fixed table, applied as Step 1)

Operator expansion runs **before** identifier splitting because operators are inter-token characters in source text. After expansion, the operator becomes a regular word that splits and tokenizes normally.

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

Notes:
- `=` → `update` (matches documentation phrasing like "the function updates X").
- Compound assignments (`+=`, `-=`) collapse to `increment` / `decrement`. Stems collapse `incrementing` / `incremented` to the same root.
- Bitwise operators get `bit*` prefixes to avoid clashing with logical `and` / `or`.

Match longest-first when a shorter operator is a prefix of a longer one (`==` before `=`, `<=` before `<`). Operators only expand outside identifiers and string literals.

#### Category 2 — Abbreviations (mandatory, fixed table, applied per-token after splitting)

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

Comparison is case-insensitive (lookup happens after lowercasing).

Skipped: ambiguous abbreviations like `pid` (could be participation id, process id, payer id) — project-specific and not in the universal table. Adding a project dictionary is a possible future extension.

#### Category 3 — Solidity domain terms (mandatory, fixed table)

Compound terms like `msg.sender` are detected before identifier splitting (which would otherwise break `msg.sender` into separate tokens):

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

Compound-term detection runs before operator expansion (so `.` doesn't get expanded in those phrases) and uses longest-match.

#### NOT expanded (explicit decisions)

- **Type names** (`uint256`, `bytes32`, `bool`, `string`): kept as-is. Documentation often uses them verbatim, and expanding `uint256` → "integer" would create spurious matches with any doc mentioning numbers.
- **Visibility modifiers** (`external`, `public`, `private`, `internal`): kept as-is. Too generic to expand usefully.
- **Method-name prefixes** (`set`, `get`, `is`, `has`): not expanded. Identifier splitting exposes them as standalone tokens; let BM25 score them naturally. Expanding `set` → "update" globally would match every `setX` member to every doc about updating anything.
- **Numeric literals**: stripped during normal tokenization.
- **Solidity keywords without obvious English equivalents** (`mapping`, `storage`, `memory`, `calldata`): kept as-is. Documentation uses these verbatim.

#### Pipeline order summary (code-derived documents)

```
"function updateBalance(address acc) external { balance[acc] += msg.value; }"
  ↓ compound-term detection (msg.value → "amount sent")
"function updateBalance(address acc) external { balance[acc] += amount sent ; }"
  ↓ operator expansion (+= → "increment")
"function updateBalance(address acc) external { balance[acc] increment amount sent ; }"
  ↓ identifier splitting (updateBalance → "update Balance"; etc.)
["function", "update", "Balance", "address", "acc", "external", "balance", "acc", "increment", "amount", "sent"]
  ↓ abbreviation expansion (acc → "account")
["function", "update", "Balance", "address", "account", "external", "balance", "account", "increment", "amount", "sent"]
  ↓ lowercase + stop-word removal + Porter stem (bm25 crate)
["updat", "balanc", "address", "aoccountt", "extern", "balanc", "account", "increment", "amount", "sent"]
```

Documentation prose query skips compound-term detection and operator expansion (no raw operators in prose), but still goes through identifier splitting (in case docs use `computeShares` style names inline) and abbreviation expansion.

## Crate choices

- **`bm25` (v2.3.2)** — handles scoring, default tokenizer, stop-words, stemming, deunicode. Right-sized for in-process scoring against small candidate sets. Not building an inverted index.
- **No tantivy.** Overkill for this use case (full-text search engine; we don't need indexing/querying).

Add `bm25` to `crates/o11a-analyze/Cargo.toml` with the `default_tokenizer` feature.

## Configuration flags

All flags exposed both as CLI arguments to `o11a-analyze analyze` and as environment variables. The CLI argument takes precedence.

### `--semantic-linking-mode <auto|llm|bm25|mechanical>` / `O11A_SEMANTIC_LINKING_MODE`

- `auto` (default): route by `is_technical` — technical docs use mechanical-only Passes 1 & 2 + LLM Pass 3; non-technical docs use LLM all three.
- `llm`: force LLM workflow for all documents, regardless of `is_technical`. Reproduces the original README spec end-to-end.
- `bm25`: force mechanical + BM25-expansion Passes 1 & 2 + LLM Pass 3 for all documents. Used to evaluate whether BM25 expansion catches anything the mechanical-only path missed.
- `mechanical`: force mechanical-only Passes 1 & 2 + LLM Pass 3 for all documents (i.e., apply the technical-doc workflow even to non-technical docs). Used to see how much non-technical docs lose without the LLM.

Ignored when `--semantic-linking-compare-all` is set.

### `--semantic-linking-pass2-algo <gap|top-k-floor>` / `O11A_SEMANTIC_LINKING_PASS2_ALGO`

- `gap` (default): Algorithm A.
- `top-k-floor`: Algorithm B.

Only takes effect when Pass 2 actually runs BM25. Under `--semantic-linking-compare-all`, both algorithms run regardless and this flag is ignored.

### `--semantic-linking-compare-all` / `O11A_SEMANTIC_LINKING_COMPARE_ALL`

Manual evaluation aid. When set, the analyzer runs the additional non-primary workflow variants alongside the configured one (`--semantic-linking-mode`), logs each variant's Pass 1 + Pass 2 output to its own file in a clear format, and **discards the extra results**. The main analysis artifact (`audit.json`, `audit.analysis.bin`) is unaffected — it still reflects only the configured workflow.

The variants logged are:

1. **mechanical** — mechanical Passes 1 & 2.
2. **bm25-gap** — mechanical + BM25 expansion (Algorithm A).
3. **bm25-top-k-floor** — mechanical + BM25 expansion (Algorithm B).
4. **llm** — LLM Passes 1 & 2.

Note: only Passes 1 & 2 run for the comparison variants. Pass 3 (semantic synthesis) runs only for the primary workflow's matches, since what's being compared is which (section, member) pairs each approach identifies — not the semantic text, which is deterministic from a confirmed pair. This keeps the comparison cost bounded.

The mechanical pre-step runs once per section and is shared across variants 1–3.

**Output format.** One JSONL file per variant in the output directory:

```
semantic-linking-compare/mechanical.jsonl
semantic-linking-compare/bm25-gap.jsonl
semantic-linking-compare/bm25-top-k-floor.jsonl
semantic-linking-compare/llm.jsonl
```

Each line is one (section, member) pair, with consistent fields across variants so a side-by-side diff or notebook analysis is trivial:

```json
{"section_topic": "T-123", "section_path": "docs/staking.md#L40", "contract": "StakeManager", "member": "claimReward", "member_topic": "N-456", "score": 0.78}
```

`score` is omitted for variants that don't produce one (mechanical, llm). Records are sorted deterministically (by section_topic, then contract, then member) so two runs on unchanged input produce byte-identical files — making `diff` directly useful.

This is a manual-inspection tool. There's no companion summary report; load the JSONL files into whatever analysis tool you prefer (jq, a notebook, a spreadsheet) to compute agreement rates or pull variant-exclusive matches.

## Output and provenance

Per the README spec, each functional semantic is persisted with:

- The semantic text (Pass 3 output).
- A provenance link to the documentation topic it was derived from.

Add to that: a `match_source` field on each (section, member) pairing, with a single value: `mechanical`, `bm25`, or `llm`. This is the workflow variant that produced the match in the configured run. The `--semantic-linking-compare-all` flag does not affect this field — the main artifact only reflects the configured workflow.

## Implementation phases

1. **Wiring & data model** — define the workflow + cutoff-algorithm enums, config plumbing through CLI + env (including `--semantic-linking-compare-all`), extend the persistence schema with `match_source`.
2. **Mechanical Passes 1 & 2** — anchor resolution, scope walking, state-variable mutation fanout. This is the default workflow path; gets the technical-doc case working end-to-end with Pass 3.
3. **LLM workflow** — Passes 1, 2, and 3 prompts. Pass 3 is shared across all workflows; Passes 1 & 2 LLM are used for non-technical docs and `--semantic-linking-mode=llm`.
4. **BM25 plumbing** — add `bm25` dep, identifier splitter, code-to-word expansion, tokenizer integration, corpus assembly per contract. Unit-test tokenization on representative Solidity identifiers.
5. **Pass 2 BM25 + Algorithm A & B** — score, cut, emit matches. Wired in behind `--semantic-linking-mode=bm25`.
6. **Comparison harness** — `--semantic-linking-compare-all` driver that runs the non-primary variants' Passes 1 & 2 alongside the configured workflow and writes per-variant JSONL logs to `semantic-linking-compare/`. No merging into the main artifact.

Rough estimate: 3–4 days for phases 1–6. Phases 1–3 alone get the production default workflow shipped (mechanical-only for technical, LLM for non-technical); 4–6 add the BM25 evaluation tooling.
