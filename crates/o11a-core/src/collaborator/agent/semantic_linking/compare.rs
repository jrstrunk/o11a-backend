//! Side-by-side comparison harness for `--semantic-linking-compare-all`.
//!
//! Runs all four workflow variants (mechanical, bm25-gap, bm25-top-k-floor,
//! llm) on every section's Pass 1 + Pass 2, logs the (section, contract,
//! member) matches each variant produced as a per-variant JSONL file under
//! `<output_dir>/semantic-linking-compare/`, and discards the extra results.
//!
//! Pass 3 is **not** invoked here — we're comparing pair identification, not
//! semantic synthesis. The main artifact is unaffected by this harness; it
//! reflects only the configured workflow.
//!
//! Performance:
//! - All read-side audit data is snapshotted into immutable indexes once
//!   under a single lock acquisition; no further locks are taken during
//!   variant evaluation.
//! - LLM Pass 1 and Pass 2 calls fan out via `tokio::spawn` (mirroring the
//!   main pipeline). Each call still goes through the global rate-limit
//!   semaphore in `router.rs`, so they don't all hit the API at once.
//! - BM25 expansion runs synchronously against the pre-built per-contract
//!   corpora.
//!
//! See `docs/specs/semantic-linking.md`.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use serde::Serialize;

use super::bm25::{self, MemberDoc};
use super::CutoffAlgorithm;
use crate::collaborator::agent::context;
use crate::collaborator::agent::task;
use crate::domain::{
  self, AuditData, DataContext, NamedTopicKind, Scope, TopicMetadata, topic,
};

const COMPARE_DIR: &str = "semantic-linking-compare";

/// String label for a `NamedTopicKind` taking scope into account so callers
/// can distinguish parameters from named locals from struct fields. Used in
/// every output record that needs declaration-kind metadata.
fn kind_label(
  topic: &topic::Topic,
  audit_data: &AuditData,
) -> (&'static str, bool) {
  let Some(meta) = audit_data.topic_metadata.get(topic) else {
    return ("unknown", false);
  };
  let TopicMetadata::NamedTopic { kind, scope, .. } = meta else {
    return ("non-named", false);
  };
  // Returns (label, is_legacy_corpus). Legacy = original `is_member_kind`
  // set: Function/Modifier/Event/Error/Struct/Enum/StateVariable.
  match kind {
    NamedTopicKind::Function(_) => ("function", true),
    NamedTopicKind::Modifier => ("modifier", true),
    NamedTopicKind::Event => ("event", true),
    NamedTopicKind::Error => ("error", true),
    NamedTopicKind::Struct => ("struct", true),
    NamedTopicKind::Enum => ("enum", true),
    NamedTopicKind::StateVariable(_) => ("state_variable", true),
    NamedTopicKind::Contract(_) => ("contract", false),
    NamedTopicKind::Builtin => ("builtin", false),
    NamedTopicKind::EnumMember => ("enum_member", false),
    NamedTopicKind::LocalVariable => {
      // Disambiguate parameter vs local vs struct field via scope.
      match scope {
        Scope::Member {
          signature_container: Some(_),
          ..
        } => ("parameter", false),
        Scope::Member {
          signature_container: None,
          ..
        } => ("local_variable", false),
        Scope::ContainingBlock { .. } => ("local_variable", false),
        Scope::Component { component, .. } => {
          // Struct/enum field: parent component would be a struct/enum.
          let parent_is_struct = audit_data
            .topic_metadata
            .get(component)
            .map(|m| {
              matches!(
                m,
                TopicMetadata::NamedTopic {
                  kind: NamedTopicKind::Struct,
                  ..
                }
              )
            })
            .unwrap_or(false);
          if parent_is_struct {
            ("struct_field", false)
          } else {
            ("local_variable", false)
          }
        }
        _ => ("local_variable", false),
      }
    }
  }
}

/// Provenance metadata for a BM25-promoted candidate. Stamped onto every
/// Pass 3 link whose underlying source was BM25 so the output supports
/// score×survival, rank×survival, and length-confound analysis.
#[derive(Debug, Clone, Copy)]
struct Bm25Provenance {
  score: f32,
  /// Rank within the (section, contract) ranking, 1-indexed.
  rank: usize,
  /// Token count of the BM25 document that scored.
  doc_length: usize,
}

/// Per-variant pre-Pass-3 state: the (section, contract, member) tuples each
/// variant proposed, plus the doc-grouped member sets and contract anchors
/// needed to feed Pass 3. Mirrors the shape the main pipeline builds in
/// `pipeline::build_semantic_links`.
struct VariantData {
  records: Vec<MatchRecord>,
  /// section_topic → doc_topic → [(member, source)] for member-scoped Pass 3.
  doc_members: BTreeMap<
    topic::Topic,
    BTreeMap<topic::Topic, Vec<(topic::Topic, domain::MatchSource)>>,
  >,
  /// section_topic → [(contract, source)] for contract-scoped Pass 3.
  section_contracts: BTreeMap<
    topic::Topic,
    Vec<(topic::Topic, domain::MatchSource)>,
  >,
  /// (section_topic, member_topic) → BM25 provenance, for BM25-source
  /// candidates only. Stamped onto each surviving Pass 3 link.
  bm25_provenance: HashMap<(topic::Topic, topic::Topic), Bm25Provenance>,
}

impl VariantData {
  fn empty() -> Self {
    Self {
      records: Vec::new(),
      doc_members: BTreeMap::new(),
      section_contracts: BTreeMap::new(),
      bm25_provenance: HashMap::new(),
    }
  }
}

/// One Pass 3 result with enough surrounding context to qualitatively review
/// whether the proposed semantic is correct. Section text and declaration
/// source are embedded so each line is self-contained for `grep`/`jq`.
#[derive(Debug, Clone, Serialize)]
struct Pass3Record {
  variant: String,
  section_topic: String,
  section_path: String,
  section_text: String,
  doc_topics: Vec<String>,
  declaration_topic: String,
  declaration_name: String,
  declaration_source: String,
  description: String,
  /// Per-link `MatchSource` from the resulting `SemanticLink`. After Pass 3,
  /// this reflects the dominant source of the input batch (highest confidence
  /// wins via `MatchSource::merge`).
  match_source: String,
  /// `"member"` for member-scoped Pass 3 batches; `"contract"` for the
  /// contract-scoped batch over state vars / events / structs.
  scope: String,
  /// BM25 score that promoted this candidate, if the underlying source was
  /// BM25. `None` for mechanical and LLM-source candidates. Used for
  /// score×survival analysis of the BM25 cutoff.
  #[serde(skip_serializing_if = "Option::is_none")]
  bm25_score: Option<f32>,
  /// Rank within the BM25 (section, contract) ranking that surfaced this
  /// candidate (1-indexed). Lets us evaluate whether rank is a more useful
  /// cutoff signal than absolute score.
  #[serde(skip_serializing_if = "Option::is_none")]
  bm25_rank: Option<usize>,
  /// Token count of the matched BM25 document, for length-confound analysis.
  #[serde(skip_serializing_if = "Option::is_none")]
  bm25_doc_length: Option<usize>,
  /// Declaration kind label: function/modifier/event/error/struct/enum/
  /// state_variable/parameter/local_variable/struct_field/enum_member/
  /// contract/builtin. Lets us group survival rates by kind.
  kind: String,
  /// True iff the declaration's kind was in the corpus *before* the
  /// 2026-05 expansion (Function/Modifier/Event/Error/Struct/Enum/
  /// StateVariable). False for newly-indexed kinds (parameter,
  /// local_variable, struct_field, enum_member). Tells us whether the
  /// corpus expansion contributed this match.
  is_legacy_corpus: bool,
  /// Identifier of the Pass 3 batch that produced this link. Same `batch_id`
  /// means same Pass 3 prompt context. Lets us reconstruct sibling sets and
  /// distinguish "Pass 3 returned no semantic" from "Pass 3 call failed".
  batch_id: String,
}

/// Side-by-side comparison: for each (section, declaration) pair, what each
/// variant said. `variants[v] == None` ⇒ variant did not propose this
/// declaration; `variants[v] == Some(vec![])` ⇒ variant proposed it but
/// Pass 3 returned no semantic; `Some(vec![…])` ⇒ Pass 3 produced semantics.
/// Use these tri-states to distinguish "missed" from "rejected".
#[derive(Debug, Serialize)]
struct Pass3SummaryRecord {
  section_topic: String,
  section_path: String,
  section_text: String,
  declaration_topic: String,
  declaration_name: String,
  declaration_source: String,
  variants: BTreeMap<String, Option<Vec<Pass3VariantOutput>>>,
}

#[derive(Debug, Clone, Serialize)]
struct Pass3VariantOutput {
  description: String,
  match_source: String,
  doc_topics: Vec<String>,
  scope: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  bm25_score: Option<f32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  bm25_rank: Option<usize>,
  #[serde(skip_serializing_if = "Option::is_none")]
  bm25_doc_length: Option<usize>,
  kind: String,
  is_legacy_corpus: bool,
  batch_id: String,
}

const VARIANTS: &[&str] = &[
  "mechanical",
  "bm25-gap",
  "bm25-top-k-floor",
  "bm25-permissive",
  "llm",
];

/// One row in `bm25-pass3-batches.jsonl`: metadata about a single Pass 3
/// invocation. Lets reviewers tell "Pass 3 returned no semantic for this
/// candidate" from "Pass 3 call failed/panicked" — and reconstruct sibling
/// sets within a single Pass 3 prompt context.
#[derive(Debug, Clone, Serialize)]
struct Pass3BatchRecord {
  batch_id: String,
  variant: String,
  /// `"member"` or `"contract"`.
  scope: String,
  section_topic: String,
  section_path: String,
  /// For member-scoped batches: the doc_topic anchor passed to Pass 3.
  /// For contract-scoped batches: same as `section_topic` (the fallback).
  doc_topic: String,
  /// The declaration topics fed into the batch (members for member-scoped,
  /// contracts for contract-scoped).
  input_topics: Vec<String>,
  /// Names of the input topics (parallel to `input_topics`).
  input_names: Vec<String>,
  /// Number of links Pass 3 returned for this batch (0 means Pass 3 said
  /// nothing; check `error` for failure cases).
  num_links_returned: usize,
  /// `"ok"`, `"failed"`, or `"panicked"`.
  status: String,
  /// Error message when `status != "ok"`. Omitted on success.
  #[serde(skip_serializing_if = "Option::is_none")]
  error: Option<String>,
  /// Dominant `MatchSource` used as Pass 3's `match_source` argument.
  match_source: String,
}

/// Atomically-incremented counter for Pass 3 batch IDs. Reset is fine
/// because the harness builds a single comparison output per run.
static BATCH_SEQ: AtomicUsize = AtomicUsize::new(0);

fn next_batch_id(variant: &str, scope: &str) -> String {
  let n = BATCH_SEQ.fetch_add(1, Ordering::Relaxed);
  format!("v-{}-{}-{:04}", variant, scope, n)
}

/// One row in `bm25-pass1-ranking.jsonl`: a contract scored against a
/// section by BM25 Pass 1 using a specific corpus variant, with its rank
/// in the descending-score ranking for that (section, corpus_variant)
/// pair. Logged for every scored contract so cutoffs can be calibrated
/// post-hoc, AND so we can A/B the two corpus formulations.
#[derive(Debug, Clone, Serialize)]
struct Pass1RankingRecord {
  /// `"signatures"` (declarations + signatures, no function bodies) or
  /// `"body"` (full source including function bodies). Both variants
  /// include the contract NatSpec + per-member name + NatSpec.
  corpus_variant: String,
  section_topic: String,
  section_path: String,
  contract_topic: String,
  contract_name: String,
  rank: usize,
  score: f32,
  /// `true` when this contract is in the top-K cutoff for its (section,
  /// corpus_variant) ranking.
  in_top_k: bool,
  /// Token count of the contract's BM25 summary document for this variant.
  /// Big-contract confound diagnostic.
  contract_doc_length: usize,
  /// Token count of the section's tokenized query.
  section_query_length: usize,
  /// True if this contract is also mechanically anchored to this section.
  is_mechanical_anchor: bool,
  /// True if LLM Pass 1 also picked this contract for this section.
  /// Backfilled after the LLM variant completes.
  is_llm_anchor: bool,
}

/// Output of `run_pass3_for_variant`: the JSONL records (for per-variant
/// `<variant>.pass3.jsonl` files), a topic-keyed index used when building
/// the side-by-side summary, and the per-batch metadata for
/// `bm25-pass3-batches.jsonl`.
struct VariantPass3 {
  records: Vec<Pass3Record>,
  by_section_decl:
    BTreeMap<(topic::Topic, topic::Topic), Vec<Pass3VariantOutput>>,
  batches: Vec<Pass3BatchRecord>,
}

/// One match record, written as a single JSONL line.
#[derive(Debug, Clone, Serialize)]
struct MatchRecord {
  section_topic: String,
  /// Best-effort source path of the section's parent document; empty if
  /// unresolvable.
  section_path: String,
  contract: String,
  contract_topic: String,
  member: String,
  member_topic: String,
  /// Source within the variant: how this specific match was produced inside
  /// that workflow (`mechanical`, `bm25`, or `llm`). Distinct from the
  /// variant name, which describes the whole workflow.
  source: String,
  /// BM25 score; populated only for matches added by BM25 expansion.
  /// Omitted from JSON for non-BM25 matches.
  #[serde(skip_serializing_if = "Option::is_none")]
  score: Option<f32>,
}

/// Read-only snapshot of every audit bit the harness needs. Built once,
/// shared across all per-section tasks via `Arc` so we don't hold the
/// `DataContext` lock during the comparison run.
struct CompareIndexes {
  /// Section topics in deterministic order.
  sections: Vec<topic::Topic>,
  /// Section text per section topic.
  section_text: HashMap<topic::Topic, String>,
  /// File path of the section's parent document, per section topic.
  section_path: HashMap<topic::Topic, String>,
  /// Mechanical section→[contracts] mapping.
  mechanical_section_to_contracts:
    HashMap<topic::Topic, Vec<topic::Topic>>,
  /// Mechanical (section, contract)→[members] mapping.
  mechanical_members_by_section_contract:
    HashMap<(topic::Topic, topic::Topic), Vec<topic::Topic>>,
  /// Pre-built BM25 corpus per contract.
  bm25_corpus_by_contract: HashMap<topic::Topic, Vec<MemberDoc>>,
  /// JSON for one specific contract (used by LLM Pass 2).
  contract_json_by_topic: HashMap<topic::Topic, String>,
  /// JSON list of all in-scope contracts (used by LLM Pass 1).
  contract_list_json: String,
  /// Display name per contract topic.
  contract_name_by_topic: HashMap<topic::Topic, String>,
  /// Display name per member topic.
  member_name_by_topic: HashMap<topic::Topic, String>,
}

/// Run the comparison harness. Errors during individual variant evaluations
/// are logged but do not abort the harness — partial outputs are still
/// written for the other variants.
pub async fn run(
  data_context: Arc<Mutex<DataContext>>,
  audit_id: &str,
  output_dir: &Path,
) -> std::io::Result<()> {
  let total_start = Instant::now();

  // Build the index under one lock acquisition. If the audit isn't there or
  // there's nothing to compare, return without creating output files.
  let snapshot_start = Instant::now();
  let indexes = match snapshot_indexes(&data_context, audit_id) {
    Ok(Some(i)) => Arc::new(i),
    Ok(None) => {
      tracing::info!("compare: no audit/sections to compare; skipping");
      return Ok(());
    }
    Err(e) => {
      // A poisoned mutex during snapshot is a hard error: refuse to run
      // rather than silently produce incomplete output.
      return Err(std::io::Error::other(e));
    }
  };

  tracing::info!("compare: snapshot built in {:?}", snapshot_start.elapsed());

  let dir = output_dir.join(COMPARE_DIR);
  std::fs::create_dir_all(&dir)?;

  // ---- BM25 Pass 1: rank every contract against every section ----
  // We compute this once, log the full ranking (one row per scored contract),
  // and stash the top-K per section to augment the contract anchor set used
  // by the BM25 variants. Mechanical does *not* use Pass 1 (it's the
  // anchor-only baseline).
  let pass1_start = Instant::now();
  let pass1_log = run_bm25_pass1_and_log(&data_context, audit_id, &indexes)?;
  let pass1_top_k_by_section = pass1_log.top_k_by_section.clone();
  write_pass1_ranking(&dir.join("bm25-pass1-ranking.jsonl"), &pass1_log.rows)?;
  tracing::info!(
    "compare: bm25 pass1 ranked {} (section, contract) pairs across {} sections in {:?}",
    pass1_log.rows.len(),
    pass1_top_k_by_section.len(),
    pass1_start.elapsed(),
  );

  // Mechanical's Pass 1 (contract anchors from name-resolution) — the
  // floor of the benchmark. We log this separately so the full
  // mechanical chain (Pass 1 → Pass 2 → Pass 3) is observable in the
  // harness output without having to derive Pass 1 from `mechanical.jsonl`.
  let mechanical_pass1 = build_mechanical_pass1_records(&indexes);
  write_mechanical_pass1(
    &dir.join("mechanical-pass1.jsonl"),
    &mechanical_pass1,
  )?;
  tracing::info!(
    "compare: mechanical pass1 wrote {} (section, contract) anchors",
    mechanical_pass1.len(),
  );

  // ---- Mechanical and BM25 variants: all synchronous ----
  // Built up front in deterministic order, then sorted at the end. We build
  // the records list (Pass 2 candidates) AND the doc_members /
  // section_contracts maps that feed Pass 3 in the same loop.
  let sync_start = Instant::now();
  let mut mech_data = VariantData::empty();
  let mut bm25_gap_data = VariantData::empty();
  let mut bm25_topk_data = VariantData::empty();
  let mut bm25_permissive_data = VariantData::empty();

  for section_topic in &indexes.sections {
    let section_text = indexes
      .section_text
      .get(section_topic)
      .map(String::as_str)
      .unwrap_or("");
    let section_path = indexes
      .section_path
      .get(section_topic)
      .cloned()
      .unwrap_or_default();
    let mech_contracts = indexes
      .mechanical_section_to_contracts
      .get(section_topic)
      .cloned()
      .unwrap_or_default();

    // BM25 contract anchor set = mechanical ∪ Pass-1 top-K. Mechanical
    // entries take provenance precedence (Mechanical > Bm25 in `merge`).
    let mut bm25_contracts: Vec<(topic::Topic, domain::MatchSource)> =
      mech_contracts
        .iter()
        .map(|c| (*c, domain::MatchSource::Mechanical))
        .collect();
    if let Some(pass1) = pass1_top_k_by_section.get(section_topic) {
      for (ct, _score) in pass1 {
        if !bm25_contracts.iter().any(|(c, _)| c == ct) {
          bm25_contracts.push((*ct, domain::MatchSource::Bm25));
        }
      }
    }

    // Mechanical variant: anchor-only, no Pass-1 expansion (it's the
    // baseline). BM25 variants: augmented set so they can reach contracts
    // mechanical didn't anchor.
    if !mech_contracts.is_empty() {
      let entry: Vec<_> = mech_contracts
        .iter()
        .map(|c| (*c, domain::MatchSource::Mechanical))
        .collect();
      mech_data.section_contracts.insert(*section_topic, entry);
    }
    if !bm25_contracts.is_empty() {
      for data in [
        &mut bm25_gap_data,
        &mut bm25_topk_data,
        &mut bm25_permissive_data,
      ] {
        data
          .section_contracts
          .insert(*section_topic, bm25_contracts.clone());
      }
    }

    // Mechanical: one record per (section, contract, mechanical member).
    for ct in &mech_contracts {
      for m in indexes
        .mechanical_members_by_section_contract
        .get(&(*section_topic, *ct))
        .into_iter()
        .flatten()
      {
        mech_data.records.push(make_record(
          section_topic,
          &section_path,
          ct,
          m,
          domain::MatchSource::Mechanical,
          None,
          &indexes,
        ));
        // Mechanical: doc_topic = section_topic (no LLM Pass 2 disambiguation).
        let doc_map = mech_data.doc_members.entry(*section_topic).or_default();
        let entry = doc_map.entry(*section_topic).or_default();
        if !entry.iter().any(|(t, _)| t == m) {
          entry.push((*m, domain::MatchSource::Mechanical));
        }
      }
    }

    // BM25 variants: mechanical seed + BM25 expansion across the augmented
    // contract set. Each variant differs only in the cutoff function.
    if section_text.is_empty() {
      continue;
    }
    enum CutoffKind {
      Gap,
      TopKFloor,
      Permissive,
    }
    for (data, cutoff_kind) in [
      (&mut bm25_gap_data, CutoffKind::Gap),
      (&mut bm25_topk_data, CutoffKind::TopKFloor),
      (&mut bm25_permissive_data, CutoffKind::Permissive),
    ] {
      // Mechanical seed (only from mechanically-anchored contracts;
      // Pass-1-discovered contracts have no mechanical members for this
      // section by definition).
      for ct in &mech_contracts {
        for m in indexes
          .mechanical_members_by_section_contract
          .get(&(*section_topic, *ct))
          .into_iter()
          .flatten()
        {
          data.records.push(make_record(
            section_topic,
            &section_path,
            ct,
            m,
            domain::MatchSource::Mechanical,
            None,
            &indexes,
          ));
          let doc_map = data.doc_members.entry(*section_topic).or_default();
          let entry = doc_map.entry(*section_topic).or_default();
          if !entry.iter().any(|(t, _)| t == m) {
            entry.push((*m, domain::MatchSource::Mechanical));
          }
        }
      }
      // BM25 expansion across the augmented contract set.
      for (ct, _) in &bm25_contracts {
        let corpus = match indexes.bm25_corpus_by_contract.get(ct) {
          Some(c) => c,
          None => continue,
        };
        let query_tokens = bm25::tokenize_prose_text(section_text);
        let scored = bm25::score(&query_tokens, corpus);
        let kept = match cutoff_kind {
          CutoffKind::Gap => bm25::cutoff(&scored, CutoffAlgorithm::Gap),
          CutoffKind::TopKFloor => {
            bm25::cutoff(&scored, CutoffAlgorithm::TopKFloor)
          }
          CutoffKind::Permissive => bm25::cutoff_permissive(&scored),
        };
        for i in kept {
          let cand = &scored[i];
          let m = cand.item.member_topic;
          let already_seeded = indexes
            .mechanical_members_by_section_contract
            .get(&(*section_topic, *ct))
            .map(|v| v.contains(&m))
            .unwrap_or(false);
          if already_seeded {
            continue;
          }
          data.records.push(make_record(
            section_topic,
            &section_path,
            ct,
            &m,
            domain::MatchSource::Bm25,
            Some(cand.score),
            &indexes,
          ));
          let doc_map = data.doc_members.entry(*section_topic).or_default();
          let entry = doc_map.entry(*section_topic).or_default();
          if !entry.iter().any(|(t, _)| *t == m) {
            entry.push((m, domain::MatchSource::Bm25));
          }
          // Track provenance so Pass3Records can carry it through. Keep
          // the entry with the *highest* score if the same member shows
          // up via multiple contracts.
          let key = (*section_topic, m);
          let new_prov = Bm25Provenance {
            score: cand.score,
            rank: i + 1,
            doc_length: cand.item.tokens.len(),
          };
          let keep = data
            .bm25_provenance
            .get(&key)
            .map(|p| new_prov.score > p.score)
            .unwrap_or(true);
          if keep {
            data.bm25_provenance.insert(key, new_prov);
          }
        }
      }
    }
  }

  let sync_elapsed = sync_start.elapsed();
  tracing::info!(
    "compare: mechanical + bm25 variants in {:?} (mechanical={}, bm25-gap={}, bm25-top-k-floor={}, bm25-permissive={})",
    sync_elapsed,
    mech_data.records.len(),
    bm25_gap_data.records.len(),
    bm25_topk_data.records.len(),
    bm25_permissive_data.records.len(),
  );

  // ---- LLM variant: parallelized via tokio::spawn ----
  let llm_start = Instant::now();
  let mut llm_data = match run_llm_variant(indexes.clone()).await {
    Ok(d) => d,
    Err(e) => {
      tracing::warn!("compare llm variant failed (writing partial output): {}", e);
      VariantData::empty()
    }
  };
  tracing::info!(
    "compare: llm variant in {:?} ({} records)",
    llm_start.elapsed(),
    llm_data.records.len()
  );

  // Deterministic sort so two runs on unchanged input produce byte-identical
  // files (makes `diff` directly useful).
  for d in [
    &mut mech_data,
    &mut bm25_gap_data,
    &mut bm25_topk_data,
    &mut bm25_permissive_data,
    &mut llm_data,
  ] {
    sort_records(&mut d.records);
  }

  write_jsonl(&dir.join("mechanical.jsonl"), &mech_data.records)?;
  write_jsonl(&dir.join("bm25-gap.jsonl"), &bm25_gap_data.records)?;
  write_jsonl(&dir.join("bm25-top-k-floor.jsonl"), &bm25_topk_data.records)?;
  write_jsonl(
    &dir.join("bm25-permissive.jsonl"),
    &bm25_permissive_data.records,
  )?;
  write_jsonl(&dir.join("llm.jsonl"), &llm_data.records)?;

  tracing::info!(
    "compare: wrote pass2 candidate logs (mechanical={}, bm25-gap={}, bm25-top-k-floor={}, bm25-permissive={}, llm={})",
    mech_data.records.len(),
    bm25_gap_data.records.len(),
    bm25_topk_data.records.len(),
    bm25_permissive_data.records.len(),
    llm_data.records.len(),
  );

  // ---- Pass 3 per variant ----
  // Each variant gets its own Pass 3 run (member-scoped + contract-scoped
  // batches), giving us per-variant survival data to evaluate quality.
  let pass3_start = Instant::now();
  let variant_inputs: Vec<(&str, &VariantData)> = vec![
    ("mechanical", &mech_data),
    ("bm25-gap", &bm25_gap_data),
    ("bm25-top-k-floor", &bm25_topk_data),
    ("bm25-permissive", &bm25_permissive_data),
    ("llm", &llm_data),
  ];

  let mut all_pass3: BTreeMap<
    String,
    BTreeMap<(topic::Topic, topic::Topic), Vec<Pass3VariantOutput>>,
  > = BTreeMap::new();
  let mut all_batches: Vec<Pass3BatchRecord> = Vec::new();
  for (name, data) in &variant_inputs {
    let v_start = Instant::now();
    let mut vp3 =
      run_pass3_for_variant(name, data, &data_context, audit_id).await;
    tracing::info!(
      "compare pass3 variant {} in {:?}: {} surviving links from {} (section,contract,member) tuples, {} batches",
      name,
      v_start.elapsed(),
      vp3.records.len(),
      data.records.len(),
      vp3.batches.len(),
    );
    sort_pass3_records(&mut vp3.records);
    write_pass3_jsonl(&dir.join(format!("{}.pass3.jsonl", name)), &vp3.records)?;
    all_pass3.insert((*name).to_string(), vp3.by_section_decl);
    all_batches.extend(vp3.batches);
  }

  tracing::info!(
    "compare pass3 (all variants) in {:?}",
    pass3_start.elapsed()
  );

  // Per-batch metadata: status, errors, sibling sets.
  all_batches.sort_by(|a, b| {
    (a.variant.as_str(), a.batch_id.as_str())
      .cmp(&(b.variant.as_str(), b.batch_id.as_str()))
  });
  write_pass3_batches(
    &dir.join("bm25-pass3-batches.jsonl"),
    &all_batches,
  )?;

  // Backfill `is_llm_anchor` on Pass1RankingRecord rows now that LLM
  // Pass 1 has run. Then rewrite the Pass 1 ranking file with the
  // updated rows.
  let mut pass1_rows = pass1_log.rows;
  let llm_anchors: HashMap<String, std::collections::HashSet<String>> =
    llm_data
      .section_contracts
      .iter()
      .map(|(st, contracts)| {
        (
          st.id().to_string(),
          contracts.iter().map(|(c, _)| c.id().to_string()).collect(),
        )
      })
      .collect();
  for row in &mut pass1_rows {
    if let Some(set) = llm_anchors.get(&row.section_topic) {
      row.is_llm_anchor = set.contains(&row.contract_topic);
    }
  }
  write_pass1_ranking(&dir.join("bm25-pass1-ranking.jsonl"), &pass1_rows)?;

  // Per-contract corpus statistics: kind counts, summary doc length.
  // Lets reviewers see what was actually indexed without re-deriving.
  let corpus_summary =
    build_corpus_summary(&data_context, audit_id);
  write_corpus_summary(
    &dir.join("bm25-corpus-summary.jsonl"),
    &corpus_summary,
  )?;

  // Side-by-side comparison: for each (section, declaration), what each
  // variant said. The proposed-but-no-semantic case (input member that did
  // not produce a Pass 3 link) is preserved as `Some(vec![])` so the user
  // can distinguish "missed" from "proposed-and-rejected".
  let summary =
    build_pass3_summary(&variant_inputs, &all_pass3, &data_context, audit_id);
  write_pass3_summary(&dir.join("pass3-summary.jsonl"), &summary)?;

  tracing::info!(
    "compare-all: wrote per-variant logs + Pass 3 outputs to {} in {:?} total",
    dir.display(),
    total_start.elapsed(),
  );

  Ok(())
}

/// Build all the read-side snapshots the harness needs. Returns `Ok(None)`
/// when the audit isn't found or has no sections (a no-op situation, not an
/// error). Returns `Err` only on real failures (poisoned mutex).
fn snapshot_indexes(
  data_context: &Arc<Mutex<DataContext>>,
  audit_id: &str,
) -> Result<Option<CompareIndexes>, String> {
  let ctx = data_context
    .lock()
    .map_err(|e| format!("data_context mutex poisoned: {}", e))?;
  let Some(audit_data) = ctx.get_audit(audit_id) else {
    return Ok(None);
  };

  let mechanical = context::mechanical_semantic_links(audit_data);
  let sections = task::collect_documentation_sections(audit_data);
  if sections.is_empty() {
    return Ok(None);
  }

  let contracts =
    context::render_contract_list_for_semantic_linking(audit_data);

  let contract_list_json = {
    let list: Vec<serde_json::Value> = contracts
      .iter()
      .map(|(ct, json)| {
        serde_json::json!({
          "contract_topic": ct.id(),
          "contract": serde_json::from_str::<serde_json::Value>(json)
            .unwrap_or_default(),
        })
      })
      .collect();
    serde_json::to_string(&list).unwrap_or_default()
  };

  let contract_json_by_topic: HashMap<topic::Topic, String> = contracts
    .iter()
    .map(|(ct, json)| (*ct, json.clone()))
    .collect();

  // Pre-build BM25 corpora per contract — each corpus is independent of the
  // section, so we only need to build it once even when many sections refer
  // to the same contract.
  let bm25_corpus_by_contract: HashMap<topic::Topic, Vec<MemberDoc>> =
    contracts
      .iter()
      .map(|(ct, _)| {
        (*ct, bm25::build_contract_member_corpus(ct, audit_data))
      })
      .collect();

  // Per-section text + path.
  let mut section_text: HashMap<topic::Topic, String> = HashMap::new();
  let mut section_path: HashMap<topic::Topic, String> = HashMap::new();
  for s in &sections {
    let txt = context::render_section_text(s, audit_data).unwrap_or_default();
    section_text.insert(*s, txt);
    section_path.insert(*s, section_path_for(s, audit_data));
  }

  // Mechanical members per (section, contract).
  let mut mechanical_members_by_section_contract: HashMap<
    (topic::Topic, topic::Topic),
    Vec<topic::Topic>,
  > = HashMap::new();
  for s in &sections {
    let section_decls = mechanical
      .section_to_declarations
      .get(s)
      .cloned()
      .unwrap_or_default();
    let cs = mechanical
      .section_to_contracts
      .get(s)
      .cloned()
      .unwrap_or_default();
    for ct in &cs {
      let members =
        context::mechanical_section_to_members(&section_decls, ct, audit_data);
      mechanical_members_by_section_contract.insert((*s, *ct), members);
    }
  }

  // Display-name lookups.
  let contract_name_by_topic: HashMap<topic::Topic, String> = contracts
    .iter()
    .filter_map(|(ct, _)| {
      audit_data
        .topic_metadata
        .get(ct)
        .and_then(|m| m.name())
        .map(|n| (*ct, n.to_string()))
    })
    .collect();

  // Build a member-name lookup for every member topic that appears in any
  // mechanical or BM25 corpus, so per-record name resolution is O(1).
  let mut member_name_by_topic: HashMap<topic::Topic, String> = HashMap::new();
  let mut record_name = |m: topic::Topic| {
    if let std::collections::hash_map::Entry::Vacant(slot) =
      member_name_by_topic.entry(m)
      && let Some(name) =
        audit_data.topic_metadata.get(&m).and_then(|md| md.name())
    {
      slot.insert(name.to_string());
    }
  };
  for members in mechanical_members_by_section_contract.values() {
    for m in members {
      record_name(*m);
    }
  }
  for corpus in bm25_corpus_by_contract.values() {
    for doc in corpus {
      record_name(doc.member_topic);
    }
  }

  Ok(Some(CompareIndexes {
    sections,
    section_text,
    section_path,
    mechanical_section_to_contracts: mechanical.section_to_contracts,
    mechanical_members_by_section_contract,
    bm25_corpus_by_contract,
    contract_json_by_topic,
    contract_list_json,
    contract_name_by_topic,
    member_name_by_topic,
  }))
}

/// Run the LLM variant against all sections. Pass 1 and Pass 2 calls all
/// fan out via `tokio::spawn`; the global rate limiter in `router.rs` keeps
/// concurrency in check. Returns the Pass 2 candidate records *and* the
/// doc_members / section_contracts maps needed by Pass 3.
async fn run_llm_variant(
  indexes: Arc<CompareIndexes>,
) -> Result<VariantData, String> {
  // Pass 1: section → additional contracts.
  let mut pass1_handles = Vec::new();
  for section_topic in &indexes.sections {
    let section_text = indexes
      .section_text
      .get(section_topic)
      .cloned()
      .unwrap_or_default();
    if section_text.is_empty() {
      continue;
    }
    let confirmed = indexes
      .mechanical_section_to_contracts
      .get(section_topic)
      .cloned()
      .unwrap_or_default();
    let st = *section_topic;
    let clj = indexes.contract_list_json.clone();
    pass1_handles.push(tokio::spawn(async move {
      task::semantic_link_pass1(&st, &section_text, &clj, &confirmed).await
    }));
  }

  // Per-section contract list including LLM additions, keyed by section.
  let mut llm_contracts_by_section: HashMap<topic::Topic, Vec<topic::Topic>> =
    HashMap::new();
  for (st, ctrs) in &indexes.mechanical_section_to_contracts {
    llm_contracts_by_section.insert(*st, ctrs.clone());
  }
  for handle in pass1_handles {
    match handle.await {
      Ok(Ok(result)) => {
        let entry = llm_contracts_by_section
          .entry(result.section_topic)
          .or_default();
        for ct in result.contract_topics {
          if !entry.contains(&ct) {
            entry.push(ct);
          }
        }
      }
      Ok(Err(e)) => tracing::warn!("compare llm pass1 failed: {}", e),
      Err(e) => tracing::warn!("compare llm pass1 panicked: {}", e),
    }
  }

  // Emit mechanical-seed records first (these are produced regardless of
  // Pass 2 outcome). Track them so we can dedupe against Pass 2 LLM hits.
  let mut records: Vec<MatchRecord> = Vec::new();
  let mut doc_members: BTreeMap<
    topic::Topic,
    BTreeMap<topic::Topic, Vec<(topic::Topic, domain::MatchSource)>>,
  > = BTreeMap::new();
  let mut section_contracts: BTreeMap<
    topic::Topic,
    Vec<(topic::Topic, domain::MatchSource)>,
  > = BTreeMap::new();
  let mut emitted: std::collections::HashSet<
    (topic::Topic, topic::Topic, topic::Topic),
  > = std::collections::HashSet::new();
  for (section_topic, contracts) in &llm_contracts_by_section {
    let section_path = indexes
      .section_path
      .get(section_topic)
      .cloned()
      .unwrap_or_default();
    // Section_contracts: mark each contract's source. Mechanical-anchored
    // contracts win over LLM-added (highest-confidence merge); we walk
    // mechanically-anchored ones first then LLM-added ones.
    let mech_anchored: std::collections::HashSet<topic::Topic> = indexes
      .mechanical_section_to_contracts
      .get(section_topic)
      .map(|v| v.iter().copied().collect())
      .unwrap_or_default();
    let entry = section_contracts.entry(*section_topic).or_default();
    for ct in contracts {
      let src = if mech_anchored.contains(ct) {
        domain::MatchSource::Mechanical
      } else {
        domain::MatchSource::Llm
      };
      if !entry.iter().any(|(c, _)| c == ct) {
        entry.push((*ct, src));
      }
    }

    for ct in contracts {
      for m in indexes
        .mechanical_members_by_section_contract
        .get(&(*section_topic, *ct))
        .into_iter()
        .flatten()
      {
        if emitted.insert((*section_topic, *ct, *m)) {
          records.push(make_record(
            section_topic,
            &section_path,
            ct,
            m,
            domain::MatchSource::Mechanical,
            None,
            &indexes,
          ));
        }
        // doc_topic = section_topic for mechanical seed (same convention as
        // the main pipeline before Pass 2 disambiguation).
        let doc_map = doc_members.entry(*section_topic).or_default();
        let dm_entry = doc_map.entry(*section_topic).or_default();
        if !dm_entry.iter().any(|(t, _)| t == m) {
          dm_entry.push((*m, domain::MatchSource::Mechanical));
        }
      }
    }
  }

  // Pass 2: per (section, contract) LLM call. Confirmed-members list is the
  // union of mechanical members across all contracts of the section
  // (matches the main pipeline's behaviour).
  let mut pass2_handles = Vec::new();
  for (section_topic, contracts) in &llm_contracts_by_section {
    let section_text = indexes
      .section_text
      .get(section_topic)
      .cloned()
      .unwrap_or_default();
    if section_text.is_empty() {
      continue;
    }
    let mut confirmed_members: Vec<topic::Topic> = Vec::new();
    for ct in contracts {
      if let Some(v) =
        indexes.mechanical_members_by_section_contract.get(&(*section_topic, *ct))
      {
        for m in v {
          if !confirmed_members.contains(m) {
            confirmed_members.push(*m);
          }
        }
      }
    }
    for ct in contracts {
      let contract_json = match indexes.contract_json_by_topic.get(ct) {
        Some(j) => j.clone(),
        None => continue,
      };
      let st = *section_topic;
      let cct = *ct;
      let stxt = section_text.clone();
      let confirmed = confirmed_members.clone();
      pass2_handles.push(tokio::spawn(async move {
        let result = task::semantic_link_pass2(
          &st,
          &stxt,
          &contract_json,
          &confirmed,
        )
        .await;
        (st, cct, result)
      }));
    }
  }

  for handle in pass2_handles {
    match handle.await {
      Ok((section_topic, contract_topic, Ok(result))) => {
        let section_path = indexes
          .section_path
          .get(&section_topic)
          .cloned()
          .unwrap_or_default();
        for mapping in result.member_mappings {
          let m = mapping.member_topic;
          // Apply doc_topics from Pass 2 (or fall back to section_topic
          // — same convention as the main pipeline at pipeline.rs).
          let doc_topics = if mapping.doc_topics.is_empty() {
            vec![section_topic]
          } else {
            mapping.doc_topics.clone()
          };
          let doc_map = doc_members.entry(section_topic).or_default();
          for dt in &doc_topics {
            let dm_entry = doc_map.entry(*dt).or_default();
            if !dm_entry.iter().any(|(t, _)| *t == m) {
              dm_entry.push((m, domain::MatchSource::Llm));
            }
          }

          if !emitted.insert((section_topic, contract_topic, m)) {
            continue;
          }
          records.push(make_record(
            &section_topic,
            &section_path,
            &contract_topic,
            &m,
            domain::MatchSource::Llm,
            None,
            &indexes,
          ));
        }
      }
      Ok((st, ct, Err(e))) => tracing::warn!(
        "compare llm pass2 failed for section {} contract {}: {}",
        st.id(),
        ct.id(),
        e
      ),
      Err(e) => tracing::warn!("compare llm pass2 panicked: {}", e),
    }
  }

  Ok(VariantData {
    records,
    doc_members,
    section_contracts,
    bm25_provenance: HashMap::new(),
  })
}

fn make_record(
  section_topic: &topic::Topic,
  section_path: &str,
  contract_topic: &topic::Topic,
  member_topic: &topic::Topic,
  source: domain::MatchSource,
  score: Option<f32>,
  indexes: &CompareIndexes,
) -> MatchRecord {
  MatchRecord {
    section_topic: section_topic.id().to_string(),
    section_path: section_path.to_string(),
    contract: indexes
      .contract_name_by_topic
      .get(contract_topic)
      .cloned()
      .unwrap_or_default(),
    contract_topic: contract_topic.id().to_string(),
    member: indexes
      .member_name_by_topic
      .get(member_topic)
      .cloned()
      .unwrap_or_default(),
    member_topic: member_topic.id().to_string(),
    source: source.as_str().to_string(),
    score,
  }
}

fn sort_records(records: &mut [MatchRecord]) {
  records.sort_by(|a, b| {
    (
      a.section_topic.as_str(),
      a.contract_topic.as_str(),
      a.member_topic.as_str(),
      a.source.as_str(),
    )
      .cmp(&(
        b.section_topic.as_str(),
        b.contract_topic.as_str(),
        b.member_topic.as_str(),
        b.source.as_str(),
      ))
  });
}

fn section_path_for(
  section_topic: &topic::Topic,
  audit_data: &AuditData,
) -> String {
  audit_data
    .topic_metadata
    .get(section_topic)
    .and_then(|m| match m.scope() {
      Scope::Container { container }
      | Scope::Component { container, .. }
      | Scope::Member { container, .. }
      | Scope::ContainingBlock { container, .. } => {
        Some(container.file_path.clone())
      }
      _ => None,
    })
    .unwrap_or_default()
}

/// Output of `run_bm25_pass1_and_log`: every (section, contract) score in
/// log-friendly form, plus the per-section top-K cutoff applied to feed Pass 2.
struct Pass1Output {
  rows: Vec<Pass1RankingRecord>,
  /// Top-K contracts per section (passed to BM25 variants for Pass 2
  /// expansion). Order matches descending BM25 score.
  top_k_by_section: HashMap<topic::Topic, Vec<(topic::Topic, f32)>>,
}

/// Run BM25 Pass 1 against every section using BOTH corpus variants
/// (signatures and body), log the full ranking for each, and return the
/// **union** of top-K per section for Pass 2 augmentation. The ranking
/// log carries a `corpus_variant` tag so the two formulations can be
/// compared post-hoc.
fn run_bm25_pass1_and_log(
  data_context: &Arc<Mutex<DataContext>>,
  audit_id: &str,
  indexes: &CompareIndexes,
) -> std::io::Result<Pass1Output> {
  let ctx = data_context
    .lock()
    .map_err(|e| std::io::Error::other(format!("data_context poisoned: {}", e)))?;
  let audit_data = match ctx.get_audit(audit_id) {
    Some(a) => a,
    None => {
      return Ok(Pass1Output {
        rows: Vec::new(),
        top_k_by_section: HashMap::new(),
      });
    }
  };

  let mut rows: Vec<Pass1RankingRecord> = Vec::new();
  let mut top_k_by_section: HashMap<topic::Topic, Vec<(topic::Topic, f32)>> =
    HashMap::new();

  // Pre-compute per-variant doc lengths and labels. Doc lengths are
  // variant-specific (body docs are longer) so we compute both up-front.
  let variants: &[(&str, bm25::SummaryCorpusVariant)] = &[
    ("signatures", bm25::SummaryCorpusVariant::Signatures),
    ("body", bm25::SummaryCorpusVariant::Body),
  ];
  let mut doc_length_by_variant: HashMap<&str, HashMap<topic::Topic, usize>> =
    HashMap::new();
  for (label, variant) in variants {
    let corpus = bm25::build_contract_summary_corpus(audit_data, *variant);
    let lengths: HashMap<topic::Topic, usize> = corpus
      .iter()
      .map(|d| (d.contract_topic, d.tokens.len()))
      .collect();
    doc_length_by_variant.insert(*label, lengths);
  }

  for section_topic in &indexes.sections {
    let section_text = match indexes.section_text.get(section_topic) {
      Some(s) if !s.is_empty() => s.as_str(),
      _ => continue,
    };
    let section_path = indexes
      .section_path
      .get(section_topic)
      .cloned()
      .unwrap_or_default();

    let section_query_length =
      bm25::tokenize_prose_text(section_text).len();

    let mech_anchor_set: std::collections::HashSet<topic::Topic> = indexes
      .mechanical_section_to_contracts
      .get(section_topic)
      .map(|v| v.iter().copied().collect())
      .unwrap_or_default();

    // Per-section: union of top-Ks across variants, dedup'd by topic with
    // the highest score across variants used as the merged score.
    let mut section_top_k_union: HashMap<topic::Topic, f32> = HashMap::new();

    for (label, variant) in variants {
      let ranking = bm25::rank_contracts(section_text, audit_data, *variant);
      if ranking.is_empty() {
        continue;
      }

      let top_k: Vec<(topic::Topic, f32)> = ranking
        .iter()
        .filter(|(_, s)| *s >= bm25::constants::MIN_SCORE)
        .take(bm25::constants::PASS1_TOP_K)
        .copied()
        .collect();
      let top_k_set: std::collections::HashSet<topic::Topic> =
        top_k.iter().map(|(t, _)| *t).collect();

      for (ct, score) in &top_k {
        section_top_k_union
          .entry(*ct)
          .and_modify(|s| {
            if *score > *s {
              *s = *score;
            }
          })
          .or_insert(*score);
      }

      let dl = doc_length_by_variant.get(*label);
      for (rank, (ct, score)) in ranking.into_iter().enumerate() {
        let contract_name = audit_data
          .topic_metadata
          .get(&ct)
          .and_then(|m| m.name())
          .unwrap_or_default()
          .to_string();
        rows.push(Pass1RankingRecord {
          corpus_variant: (*label).to_string(),
          section_topic: section_topic.id().to_string(),
          section_path: section_path.clone(),
          contract_topic: ct.id().to_string(),
          contract_name,
          rank: rank + 1,
          score,
          in_top_k: top_k_set.contains(&ct),
          contract_doc_length: dl
            .and_then(|m| m.get(&ct).copied())
            .unwrap_or(0),
          section_query_length,
          is_mechanical_anchor: mech_anchor_set.contains(&ct),
          // Backfilled later from llm_data.section_contracts.
          is_llm_anchor: false,
        });
      }
    }

    if !section_top_k_union.is_empty() {
      // Sort union by score desc for downstream determinism.
      let mut entries: Vec<(topic::Topic, f32)> =
        section_top_k_union.into_iter().collect();
      entries.sort_by(|a, b| {
        b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
      });
      top_k_by_section.insert(*section_topic, entries);
    }
  }

  // Sort log rows for deterministic output: by corpus_variant, then
  // section_topic, then rank.
  rows.sort_by(|a, b| {
    (a.corpus_variant.as_str(), a.section_topic.as_str(), a.rank).cmp(&(
      b.corpus_variant.as_str(),
      b.section_topic.as_str(),
      b.rank,
    ))
  });

  Ok(Pass1Output {
    rows,
    top_k_by_section,
  })
}

/// One row in `bm25-corpus-summary.jsonl`: per-contract corpus statistics
/// (member counts by kind, summary-doc lengths for both Pass 1 corpus
/// variants, member-doc count). Lets reviewers verify what was indexed.
#[derive(Debug, Clone, Serialize)]
struct CorpusSummaryRecord {
  contract_topic: String,
  contract_name: String,
  /// Token count of the contract's BM25 Pass 1 summary doc — signatures
  /// variant (declarations + signatures, no function bodies).
  summary_doc_length_signatures: usize,
  /// Token count of the contract's BM25 Pass 1 summary doc — body
  /// variant (full member source, function bodies included).
  summary_doc_length_body: usize,
  /// Number of member documents in this contract's BM25 Pass 2 corpus.
  member_doc_count: usize,
  /// Total tokens across all member documents in the Pass 2 corpus.
  member_total_tokens: usize,
  /// Per-kind counts of indexed declarations.
  kind_counts: BTreeMap<String, usize>,
  /// Longest member doc (Pass 2 corpus).
  longest_member_doc: usize,
  /// Shortest member doc (Pass 2 corpus).
  shortest_member_doc: usize,
  /// Mean member-doc length, rounded to 1 decimal.
  mean_member_doc_length: f32,
}

fn build_corpus_summary(
  data_context: &Arc<Mutex<DataContext>>,
  audit_id: &str,
) -> Vec<CorpusSummaryRecord> {
  let Ok(ctx) = data_context.lock() else {
    return Vec::new();
  };
  let Some(audit_data) = ctx.get_audit(audit_id) else {
    return Vec::new();
  };

  let sigs_corpus = bm25::build_contract_summary_corpus(
    audit_data,
    bm25::SummaryCorpusVariant::Signatures,
  );
  let body_corpus = bm25::build_contract_summary_corpus(
    audit_data,
    bm25::SummaryCorpusVariant::Body,
  );
  let sigs_lengths: HashMap<topic::Topic, usize> = sigs_corpus
    .iter()
    .map(|d| (d.contract_topic, d.tokens.len()))
    .collect();
  let body_lengths: HashMap<topic::Topic, usize> = body_corpus
    .iter()
    .map(|d| (d.contract_topic, d.tokens.len()))
    .collect();
  // Union of contract topics from both variants.
  let mut all_contracts: std::collections::HashSet<topic::Topic> =
    std::collections::HashSet::new();
  all_contracts.extend(sigs_lengths.keys().copied());
  all_contracts.extend(body_lengths.keys().copied());

  let mut out: Vec<CorpusSummaryRecord> = Vec::new();
  for ct in &all_contracts {
    let contract_name = audit_data
      .topic_metadata
      .get(ct)
      .and_then(|m| m.name())
      .unwrap_or_default()
      .to_string();

    let member_corpus = bm25::build_contract_member_corpus(ct, audit_data);
    let member_doc_count = member_corpus.len();
    let member_total_tokens: usize =
      member_corpus.iter().map(|d| d.tokens.len()).sum();
    let longest_member_doc = member_corpus
      .iter()
      .map(|d| d.tokens.len())
      .max()
      .unwrap_or(0);
    let shortest_member_doc = member_corpus
      .iter()
      .map(|d| d.tokens.len())
      .min()
      .unwrap_or(0);
    let mean_member_doc_length = if member_doc_count > 0 {
      (member_total_tokens as f32) / (member_doc_count as f32)
    } else {
      0.0
    };

    let mut kind_counts: BTreeMap<String, usize> = BTreeMap::new();
    for doc in &member_corpus {
      let (label, _) = kind_label(&doc.member_topic, audit_data);
      *kind_counts.entry(label.to_string()).or_default() += 1;
    }

    out.push(CorpusSummaryRecord {
      contract_topic: ct.id().to_string(),
      contract_name,
      summary_doc_length_signatures: sigs_lengths
        .get(ct)
        .copied()
        .unwrap_or(0),
      summary_doc_length_body: body_lengths.get(ct).copied().unwrap_or(0),
      member_doc_count,
      member_total_tokens,
      kind_counts,
      longest_member_doc,
      shortest_member_doc,
      mean_member_doc_length: (mean_member_doc_length * 10.0).round() / 10.0,
    });
  }

  out.sort_by(|a, b| a.contract_topic.cmp(&b.contract_topic));
  out
}

fn write_corpus_summary(
  path: &Path,
  records: &[CorpusSummaryRecord],
) -> std::io::Result<()> {
  use std::io::Write;
  let tmp = path.with_extension("jsonl.tmp");
  {
    let mut f = std::fs::File::create(&tmp)?;
    for r in records {
      let line = serde_json::to_string(r).unwrap_or_default();
      writeln!(f, "{}", line)?;
    }
  }
  std::fs::rename(&tmp, path)?;
  Ok(())
}

fn write_pass3_batches(
  path: &Path,
  records: &[Pass3BatchRecord],
) -> std::io::Result<()> {
  use std::io::Write;
  let tmp = path.with_extension("jsonl.tmp");
  {
    let mut f = std::fs::File::create(&tmp)?;
    for r in records {
      let line = serde_json::to_string(r).unwrap_or_default();
      writeln!(f, "{}", line)?;
    }
  }
  std::fs::rename(&tmp, path)?;
  Ok(())
}

/// One row in `mechanical-pass1.jsonl`: a (section, contract) anchor
/// derived purely from name-resolution. The mechanical pipeline takes
/// every CodeIdentifier the parser resolved in a section, walks each
/// resolution up to its containing contract, and emits the result as
/// the section's contract anchors. This file is the per-row form of
/// `mechanical.section_to_contracts`.
#[derive(Debug, Clone, Serialize)]
struct MechanicalPass1Record {
  section_topic: String,
  section_path: String,
  contract_topic: String,
  contract_name: String,
}

fn build_mechanical_pass1_records(
  indexes: &CompareIndexes,
) -> Vec<MechanicalPass1Record> {
  let mut out: Vec<MechanicalPass1Record> = Vec::new();
  for (section_topic, contracts) in &indexes.mechanical_section_to_contracts {
    let section_path = indexes
      .section_path
      .get(section_topic)
      .cloned()
      .unwrap_or_default();
    for ct in contracts {
      let contract_name = indexes
        .contract_name_by_topic
        .get(ct)
        .cloned()
        .unwrap_or_default();
      out.push(MechanicalPass1Record {
        section_topic: section_topic.id().to_string(),
        section_path: section_path.clone(),
        contract_topic: ct.id().to_string(),
        contract_name,
      });
    }
  }
  // Deterministic order.
  out.sort_by(|a, b| {
    (a.section_topic.as_str(), a.contract_topic.as_str())
      .cmp(&(b.section_topic.as_str(), b.contract_topic.as_str()))
  });
  out
}

fn write_mechanical_pass1(
  path: &Path,
  records: &[MechanicalPass1Record],
) -> std::io::Result<()> {
  use std::io::Write;
  let tmp = path.with_extension("jsonl.tmp");
  {
    let mut f = std::fs::File::create(&tmp)?;
    for r in records {
      let line = serde_json::to_string(r).unwrap_or_default();
      writeln!(f, "{}", line)?;
    }
  }
  std::fs::rename(&tmp, path)?;
  Ok(())
}

fn write_pass1_ranking(
  path: &Path,
  records: &[Pass1RankingRecord],
) -> std::io::Result<()> {
  use std::io::Write;
  let tmp = path.with_extension("jsonl.tmp");
  {
    let mut f = std::fs::File::create(&tmp)?;
    for r in records {
      let line = serde_json::to_string(r).unwrap_or_default();
      writeln!(f, "{}", line)?;
    }
  }
  std::fs::rename(&tmp, path)?;
  Ok(())
}

fn write_jsonl(
  path: &Path,
  records: &[MatchRecord],
) -> std::io::Result<()> {
  use std::io::Write;
  let tmp = path.with_extension("jsonl.tmp");
  {
    let mut f = std::fs::File::create(&tmp)?;
    for r in records {
      let line = serde_json::to_string(r).unwrap_or_default();
      writeln!(f, "{}", line)?;
    }
  }
  std::fs::rename(&tmp, path)?;
  Ok(())
}

// ---------------------------------------------------------------------------
// Pass 3 per variant
// ---------------------------------------------------------------------------

/// Run Pass 3 over a variant's candidates. Mirrors the main pipeline's
/// member-scoped + contract-scoped batching, but does **not** condense or
/// resolve transitive topics — raw Pass 3 output is what we want for
/// quality review.
async fn run_pass3_for_variant(
  variant: &str,
  data: &VariantData,
  data_context: &Arc<Mutex<DataContext>>,
  audit_id: &str,
) -> VariantPass3 {
  // Each batch carries enough info to write a `Pass3BatchRecord` regardless
  // of whether the LLM call succeeded. The handle returns the batch_id so
  // we can join results back to the recorded batch metadata.
  type Pass3Handle = tokio::task::JoinHandle<(
    String, // batch_id
    Result<task::SemanticLinkPass3Result, task::TaskError>,
  )>;
  let mut handles: Vec<Pass3Handle> = Vec::new();
  let mut batches: Vec<Pass3BatchRecord> = Vec::new();
  // Map from batch_id → (scope, section_topic) so we can attribute output
  // links back to their batch when stamping records.
  let mut batch_index: HashMap<String, (&'static str, topic::Topic)> =
    HashMap::new();

  // Resolve input names once per batch under a brief lock (handy for
  // sibling-set inspection in the batches log).
  let resolve_names = |topics: &[topic::Topic]| -> Vec<String> {
    let Ok(ctx) = data_context.lock() else {
      return topics.iter().map(|_| String::new()).collect();
    };
    let Some(audit_data) = ctx.get_audit(audit_id) else {
      return topics.iter().map(|_| String::new()).collect();
    };
    topics
      .iter()
      .map(|t| {
        audit_data
          .topic_metadata
          .get(t)
          .and_then(|m| m.name())
          .unwrap_or_default()
          .to_string()
      })
      .collect()
  };
  // (a) Member-scoped: one Pass 3 call per (section, doc_topic) batch.
  for (section_topic, doc_member_map) in &data.doc_members {
    let (section_text, section_path) = match render_section_payload(
      section_topic,
      data_context,
      audit_id,
    ) {
      Some(v) => v,
      None => continue,
    };
    for (doc_topic, member_pairs) in doc_member_map {
      let member_topics: Vec<topic::Topic> =
        member_pairs.iter().map(|(t, _)| *t).collect();
      let batch_source = member_pairs
        .iter()
        .map(|(_, s)| *s)
        .reduce(|a, b| a.merge(b))
        .unwrap_or(domain::MatchSource::Mechanical);

      let (declarations_json, source_code) =
        match render_member_batch(&member_topics, data_context, audit_id) {
          Some(v) => v,
          None => continue,
        };
      if declarations_json == "[]" {
        continue;
      }

      let batch_id = next_batch_id(variant, "member");
      batch_index.insert(batch_id.clone(), ("member", *section_topic));
      let input_names = resolve_names(&member_topics);
      batches.push(Pass3BatchRecord {
        batch_id: batch_id.clone(),
        variant: variant.to_string(),
        scope: "member".to_string(),
        section_topic: section_topic.id().to_string(),
        section_path: section_path.clone(),
        doc_topic: doc_topic.id().to_string(),
        input_topics: member_topics.iter().map(|t| t.id().to_string()).collect(),
        input_names,
        num_links_returned: 0, // backfilled after await
        status: "pending".to_string(),
        error: None,
        match_source: batch_source.as_str().to_string(),
      });

      let st = *section_topic;
      let stxt = section_text.clone();
      let fallback_dt = *doc_topic;
      let bid = batch_id.clone();
      handles.push(tokio::spawn(async move {
        let res = task::semantic_link_pass3(
          &st,
          &stxt,
          &declarations_json,
          &source_code,
          &fallback_dt,
          batch_source,
        )
        .await;
        (bid, res)
      }));
    }
  }

  // (b) Contract-scoped: one Pass 3 call per section batching all contracts.
  for (section_topic, contract_pairs) in &data.section_contracts {
    let contract_topics: Vec<topic::Topic> =
      contract_pairs.iter().map(|(t, _)| *t).collect();
    let batch_source = contract_pairs
      .iter()
      .map(|(_, s)| *s)
      .reduce(|a, b| a.merge(b))
      .unwrap_or(domain::MatchSource::Mechanical);

    let (section_text, section_path) = match render_section_payload(
      section_topic,
      data_context,
      audit_id,
    ) {
      Some(v) => v,
      None => continue,
    };
    let (declarations_json, signatures_source) =
      match render_contract_batch(&contract_topics, data_context, audit_id) {
        Some(v) => v,
        None => continue,
      };
    if declarations_json == "[]" {
      continue;
    }

    let batch_id = next_batch_id(variant, "contract");
    batch_index.insert(batch_id.clone(), ("contract", *section_topic));
    let input_names = resolve_names(&contract_topics);
    batches.push(Pass3BatchRecord {
      batch_id: batch_id.clone(),
      variant: variant.to_string(),
      scope: "contract".to_string(),
      section_topic: section_topic.id().to_string(),
      section_path: section_path.clone(),
      doc_topic: section_topic.id().to_string(),
      input_topics: contract_topics.iter().map(|t| t.id().to_string()).collect(),
      input_names,
      num_links_returned: 0,
      status: "pending".to_string(),
      error: None,
      match_source: batch_source.as_str().to_string(),
    });

    let st = *section_topic;
    let fallback_dt = *section_topic;
    let bid = batch_id.clone();
    handles.push(tokio::spawn(async move {
      let res = task::semantic_link_pass3(
        &st,
        &section_text,
        &declarations_json,
        &signatures_source,
        &fallback_dt,
        batch_source,
      )
      .await;
      (bid, res)
    }));
  }

  // Collect raw links by batch, recording per-batch status/result counts.
  let mut links_by_batch: HashMap<String, Vec<domain::SemanticLink>> =
    HashMap::new();
  let mut status_by_batch: HashMap<String, (String, Option<String>, usize)> =
    HashMap::new();
  for h in handles {
    match h.await {
      Ok((bid, Ok(result))) => {
        let n = result.links.len();
        status_by_batch
          .insert(bid.clone(), ("ok".to_string(), None, n));
        links_by_batch.insert(bid, result.links);
      }
      Ok((bid, Err(e))) => {
        let msg = format!("{}", e);
        tracing::warn!(
          "compare pass3 ({} variant, batch {}) failed: {}",
          variant,
          bid,
          msg
        );
        status_by_batch
          .insert(bid, ("failed".to_string(), Some(msg), 0));
      }
      Err(e) => {
        // We lost track of the batch_id when the join itself errored
        // (panic). Record under a sentinel so the count is still right;
        // the batches log will show the original batches as `pending`.
        tracing::warn!("compare pass3 ({}) panicked: {}", variant, e);
      }
    }
  }

  // Backfill batches with status info.
  for batch in &mut batches {
    if let Some((status, error, n)) = status_by_batch.get(&batch.batch_id) {
      batch.status = status.clone();
      batch.error = error.clone();
      batch.num_links_returned = *n;
    } else {
      // Handle that didn't return — panic case.
      batch.status = "panicked".to_string();
      batch.error = Some("tokio JoinHandle panic".to_string());
    }
  }

  // Enrich each link with section text, declaration source, name, kind, and
  // BM25 provenance under one lock acquisition.
  let mut records: Vec<Pass3Record> = Vec::new();
  let mut by_section_decl: BTreeMap<
    (topic::Topic, topic::Topic),
    Vec<Pass3VariantOutput>,
  > = BTreeMap::new();
  if links_by_batch.is_empty() {
    return VariantPass3 {
      records,
      by_section_decl,
      batches,
    };
  }

  let ctx = match data_context.lock() {
    Ok(g) => g,
    Err(e) => {
      tracing::warn!("compare pass3 enrich: lock poisoned: {}", e);
      return VariantPass3 {
        records,
        by_section_decl,
        batches,
      };
    }
  };
  let audit_data = match ctx.get_audit(audit_id) {
    Some(a) => a,
    None => {
      return VariantPass3 {
        records,
        by_section_decl,
        batches,
      };
    }
  };

  // Caches for the rendering work.
  let mut section_text_cache: HashMap<topic::Topic, String> = HashMap::new();
  let mut section_path_cache: HashMap<topic::Topic, String> = HashMap::new();
  let mut decl_source_cache: HashMap<topic::Topic, String> = HashMap::new();
  let mut decl_name_cache: HashMap<topic::Topic, String> = HashMap::new();
  let mut decl_kind_cache: HashMap<topic::Topic, (String, bool)> =
    HashMap::new();

  // Iterate by batch so each link knows its batch_id.
  for (batch_id, links) in links_by_batch {
    let (scope_str, section_topic) = match batch_index.get(&batch_id) {
      Some((s, st)) => (s.to_string(), *st),
      None => continue,
    };

    for link in links {
      let section_text = section_text_cache
        .entry(section_topic)
        .or_insert_with(|| {
          context::render_section_text(&section_topic, audit_data)
            .unwrap_or_default()
        })
        .clone();
      let section_path = section_path_cache
        .entry(section_topic)
        .or_insert_with(|| section_path_for(&section_topic, audit_data))
        .clone();
      let declaration_source = decl_source_cache
        .entry(link.declaration_topic)
        .or_insert_with(|| {
          let member_src =
            context::render_batched_member_sources_for_semantics(
              &[link.declaration_topic],
              audit_data,
            );
          if member_src.trim().is_empty() {
            context::render_batched_contract_declaration_signatures(
              &[link.declaration_topic],
              audit_data,
            )
          } else {
            member_src
          }
        })
        .clone();
      let declaration_name = decl_name_cache
        .entry(link.declaration_topic)
        .or_insert_with(|| {
          audit_data
            .topic_metadata
            .get(&link.declaration_topic)
            .and_then(|m| m.name())
            .map(|n| n.to_string())
            .unwrap_or_default()
        })
        .clone();
      let (kind, is_legacy_corpus) = decl_kind_cache
        .entry(link.declaration_topic)
        .or_insert_with(|| {
          let (label, legacy) =
            kind_label(&link.declaration_topic, audit_data);
          (label.to_string(), legacy)
        })
        .clone();

      let doc_topic_ids: Vec<String> = link
        .documentation_topics
        .iter()
        .map(|t| t.id().to_string())
        .collect();
      let match_source_str = link.match_source.as_str().to_string();
      let description = link.description.clone();
      let prov = data
        .bm25_provenance
        .get(&(section_topic, link.declaration_topic))
        .copied();
      let bm25_score = prov.map(|p| p.score);
      let bm25_rank = prov.map(|p| p.rank);
      let bm25_doc_length = prov.map(|p| p.doc_length);

      records.push(Pass3Record {
        variant: variant.to_string(),
        section_topic: section_topic.id().to_string(),
        section_path,
        section_text,
        doc_topics: doc_topic_ids.clone(),
        declaration_topic: link.declaration_topic.id().to_string(),
        declaration_name,
        declaration_source,
        description: description.clone(),
        match_source: match_source_str.clone(),
        scope: scope_str.clone(),
        bm25_score,
        bm25_rank,
        bm25_doc_length,
        kind: kind.clone(),
        is_legacy_corpus,
        batch_id: batch_id.clone(),
      });

      by_section_decl
        .entry((section_topic, link.declaration_topic))
        .or_default()
        .push(Pass3VariantOutput {
          description,
          match_source: match_source_str,
          doc_topics: doc_topic_ids,
          scope: scope_str.clone(),
          bm25_score,
          bm25_rank,
          bm25_doc_length,
          kind,
          is_legacy_corpus,
          batch_id: batch_id.clone(),
        });
    }
  }

  VariantPass3 {
    records,
    by_section_decl,
    batches,
  }
}

/// Lock and render a section's text + path. Returns None on lock errors or
/// missing audit. Held briefly.
fn render_section_payload(
  section_topic: &topic::Topic,
  data_context: &Arc<Mutex<DataContext>>,
  audit_id: &str,
) -> Option<(String, String)> {
  let ctx = data_context.lock().ok()?;
  let audit_data = ctx.get_audit(audit_id)?;
  let text = context::render_section_text(section_topic, audit_data)?;
  let path = section_path_for(section_topic, audit_data);
  Some((text, path))
}

fn render_member_batch(
  members: &[topic::Topic],
  data_context: &Arc<Mutex<DataContext>>,
  audit_id: &str,
) -> Option<(String, String)> {
  let ctx = data_context.lock().ok()?;
  let audit_data = ctx.get_audit(audit_id)?;
  Some((
    context::render_batched_member_declarations_for_semantics(
      members, audit_data,
    ),
    context::render_batched_member_sources_for_semantics(members, audit_data),
  ))
}

fn render_contract_batch(
  contracts: &[topic::Topic],
  data_context: &Arc<Mutex<DataContext>>,
  audit_id: &str,
) -> Option<(String, String)> {
  let ctx = data_context.lock().ok()?;
  let audit_data = ctx.get_audit(audit_id)?;
  Some((
    context::render_batched_contract_declarations_for_semantics(
      contracts, audit_data,
    ),
    context::render_batched_contract_declaration_signatures(
      contracts, audit_data,
    ),
  ))
}

fn sort_pass3_records(records: &mut [Pass3Record]) {
  records.sort_by(|a, b| {
    (
      a.section_topic.as_str(),
      a.declaration_topic.as_str(),
      a.scope.as_str(),
      a.description.as_str(),
    )
      .cmp(&(
        b.section_topic.as_str(),
        b.declaration_topic.as_str(),
        b.scope.as_str(),
        b.description.as_str(),
      ))
  });
}

fn write_pass3_jsonl(
  path: &Path,
  records: &[Pass3Record],
) -> std::io::Result<()> {
  use std::io::Write;
  let tmp = path.with_extension("jsonl.tmp");
  {
    let mut f = std::fs::File::create(&tmp)?;
    for r in records {
      let line = serde_json::to_string(r).unwrap_or_default();
      writeln!(f, "{}", line)?;
    }
  }
  std::fs::rename(&tmp, path)?;
  Ok(())
}

/// Build the side-by-side summary keyed by (section, declaration). Each
/// variant entry is `Some(Vec<output>)` if the variant proposed the
/// declaration in its Pass 3 input — even if Pass 3 returned no semantic
/// (empty Vec) — and `None` if the variant did not propose it. This lets
/// reviewers tell "missed" from "proposed-and-rejected".
fn build_pass3_summary(
  variant_inputs: &[(&str, &VariantData)],
  all_pass3: &BTreeMap<
    String,
    BTreeMap<(topic::Topic, topic::Topic), Vec<Pass3VariantOutput>>,
  >,
  data_context: &Arc<Mutex<DataContext>>,
  audit_id: &str,
) -> Vec<Pass3SummaryRecord> {
  use std::collections::BTreeSet;

  // For each variant: set of (section, declaration) pairs the variant
  // proposed via its member-scoped Pass 3 input. The contract-scoped batch
  // is a different path — it doesn't enumerate specific declarations to the
  // model — so a contract-scoped Pass 3 hit shows up only via its surviving
  // result, which we pick up below from `all_pass3`.
  let mut proposed_per_variant: BTreeMap<
    String,
    BTreeSet<(topic::Topic, topic::Topic)>,
  > = BTreeMap::new();
  for (name, data) in variant_inputs {
    let mut set: BTreeSet<(topic::Topic, topic::Topic)> = BTreeSet::new();
    for (section_topic, doc_map) in &data.doc_members {
      for member_pairs in doc_map.values() {
        for (m, _) in member_pairs {
          set.insert((*section_topic, *m));
        }
      }
    }
    proposed_per_variant.insert((*name).to_string(), set);
  }

  // Union of all (section, declaration) pairs that anyone proposed or got a
  // Pass 3 result for.
  let mut keys: BTreeSet<(topic::Topic, topic::Topic)> = BTreeSet::new();
  for set in proposed_per_variant.values() {
    for k in set {
      keys.insert(*k);
    }
  }
  for index in all_pass3.values() {
    for k in index.keys() {
      keys.insert(*k);
    }
  }

  // Pre-render context (section text/path, declaration source/name) once
  // per unique topic in a single lock acquisition.
  let mut section_text_cache: HashMap<topic::Topic, String> = HashMap::new();
  let mut section_path_cache: HashMap<topic::Topic, String> = HashMap::new();
  let mut decl_source_cache: HashMap<topic::Topic, String> = HashMap::new();
  let mut decl_name_cache: HashMap<topic::Topic, String> = HashMap::new();
  if let Ok(ctx) = data_context.lock()
    && let Some(audit_data) = ctx.get_audit(audit_id)
  {
    for (st, dt) in &keys {
      section_text_cache.entry(*st).or_insert_with(|| {
        context::render_section_text(st, audit_data).unwrap_or_default()
      });
      section_path_cache
        .entry(*st)
        .or_insert_with(|| section_path_for(st, audit_data));
      decl_source_cache.entry(*dt).or_insert_with(|| {
        let m = context::render_batched_member_sources_for_semantics(
          &[*dt],
          audit_data,
        );
        if m.trim().is_empty() {
          context::render_batched_contract_declaration_signatures(
            &[*dt],
            audit_data,
          )
        } else {
          m
        }
      });
      decl_name_cache.entry(*dt).or_insert_with(|| {
        audit_data
          .topic_metadata
          .get(dt)
          .and_then(|md| md.name())
          .map(|n| n.to_string())
          .unwrap_or_default()
      });
    }
  }

  let mut out: Vec<Pass3SummaryRecord> = Vec::new();
  for (st, dt) in &keys {
    let mut variants: BTreeMap<String, Option<Vec<Pass3VariantOutput>>> =
      BTreeMap::new();
    for v in VARIANTS {
      let outputs = all_pass3.get(*v).and_then(|idx| idx.get(&(*st, *dt)));
      if let Some(outputs) = outputs {
        variants.insert((*v).to_string(), Some(outputs.clone()));
      } else if proposed_per_variant
        .get(*v)
        .map(|set| set.contains(&(*st, *dt)))
        .unwrap_or(false)
      {
        variants.insert((*v).to_string(), Some(Vec::new()));
      } else {
        variants.insert((*v).to_string(), None);
      }
    }

    out.push(Pass3SummaryRecord {
      section_topic: st.id().to_string(),
      section_path: section_path_cache.get(st).cloned().unwrap_or_default(),
      section_text: section_text_cache.get(st).cloned().unwrap_or_default(),
      declaration_topic: dt.id().to_string(),
      declaration_name: decl_name_cache.get(dt).cloned().unwrap_or_default(),
      declaration_source: decl_source_cache
        .get(dt)
        .cloned()
        .unwrap_or_default(),
      variants,
    });
  }

  out.sort_by(|a, b| {
    (a.section_topic.as_str(), a.declaration_topic.as_str()).cmp(&(
      b.section_topic.as_str(),
      b.declaration_topic.as_str(),
    ))
  });
  out
}

fn write_pass3_summary(
  path: &Path,
  records: &[Pass3SummaryRecord],
) -> std::io::Result<()> {
  use std::io::Write;
  let tmp = path.with_extension("jsonl.tmp");
  {
    let mut f = std::fs::File::create(&tmp)?;
    for r in records {
      let line = serde_json::to_string(r).unwrap_or_default();
      writeln!(f, "{}", line)?;
    }
  }
  std::fs::rename(&tmp, path)?;
  Ok(())
}

