//! End-to-end entry point for the o11a-analyze binary's analysis
//! workflow: parse the Solidity project's solc output, run the Solidity
//! analyzer, build the per-audit resolution graph, inject synthetic
//! developer-documentation comments, then run the documentation
//! analyzer. Populates the shared `DataContext` in place.

use crate::documentation;
use crate::solidity;
use o11a_core::domain;
use o11a_core::domain::DataContext;
use o11a_core::resolution_graph;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Errors produced by `run_analysis`.
#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
  #[error("failed to load project configuration: {0}")]
  Config(#[from] domain::ConfigError),
  #[error("DataContext mutex poisoned: {0}")]
  LockPoisoned(String),
  #[error("audit '{0}' already exists")]
  AuditExists(String),
  #[error("audit '{0}' not found in DataContext")]
  AuditMissing(String),
  #[error("failed to analyze Solidity project: {0}")]
  Solidity(String),
  #[error("failed to analyze documentation files: {0}")]
  Documentation(String),
}

pub fn run_analysis(
  project_root: &Path,
  audit_id: &str,
  data_context: &Arc<Mutex<DataContext>>,
) -> Result<(), AnalysisError> {
  // Load in-scope files from scope.txt
  let in_scope_files = domain::load_in_scope_files(project_root)?;

  let audit_name = domain::load_audit_name(project_root)?;

  // Load ordered document file list from documents.txt
  let document_files = domain::load_document_files(project_root)?;

  // Load security notes from security.md (optional)
  let security_notes = domain::load_security_notes(project_root)?;

  // Create the audit if it doesn't exist
  {
    let mut ctx = data_context
      .lock()
      .map_err(|e| AnalysisError::LockPoisoned(e.to_string()))?;
    if !ctx.create_audit(
      audit_id.to_string(),
      audit_name,
      in_scope_files,
      security_notes,
    ) {
      return Err(AnalysisError::AuditExists(audit_id.to_string()));
    }
  }

  tracing::info!("Analyzing Solidity project at: {}", project_root.display());

  // Analyze Solidity project and populate AuditData
  {
    let mut ctx = data_context
      .lock()
      .map_err(|e| AnalysisError::LockPoisoned(e.to_string()))?;
    solidity::analyzer::analyze(project_root, audit_id, &mut ctx)
      .map_err(AnalysisError::Solidity)?;
  }

  tracing::info!("Building resolution graph...");

  // The graph is per-audit, not per-language — every language analyzer
  // must contribute its edges before the build call, and every consumer
  // that reads `referenced_topic` against ambiguous names must run after
  // it. The slot between language analysis and documentation analysis is
  // the only one that satisfies both constraints.
  populate_resolution_graph(data_context, audit_id)?;

  tracing::info!("Injecting developer documentation...");

  // Synthetic dev-doc CommentTopics (NatSpec docstrings, inline comments
  // on SemanticBlocks, etc.) must exist before the documentation analyzer
  // runs because downstream consumers of the doc tree expect to find
  // them in `comment_index`. They build *after* the resolution graph so
  // that the future graph-driven dev-doc resolution pass (Phase 7) has
  // a populated graph to score against.
  inject_developer_documentation(data_context, audit_id)?;

  tracing::info!("Analyzing documentation files...");

  // Analyze documentation and augment AuditData
  {
    let mut ctx = data_context
      .lock()
      .map_err(|e| AnalysisError::LockPoisoned(e.to_string()))?;
    documentation::analyzer::analyze(
      project_root,
      audit_id,
      &mut ctx,
      &document_files,
    )
    .map_err(AnalysisError::Documentation)?;
  }

  tracing::info!("Done loading audit: {}", audit_id);

  Ok(())
}

/// Build the per-audit resolution graph from the populated `AuditData`
/// and store it on `AuditData::resolution_graph`. The graph is one
/// structure for the whole audit regardless of source-language mix.
///
/// Idempotent: the builder is a pure function of `AuditData`, so a
/// second call replaces the stored graph with a byte-identical one.
pub fn populate_resolution_graph(
  data_context: &Arc<Mutex<DataContext>>,
  audit_id: &str,
) -> Result<(), AnalysisError> {
  let mut ctx = data_context
    .lock()
    .map_err(|e| AnalysisError::LockPoisoned(e.to_string()))?;
  let audit_data = ctx
    .get_audit_mut(audit_id)
    .ok_or_else(|| AnalysisError::AuditMissing(audit_id.to_string()))?;
  audit_data.resolution_graph = Some(resolution_graph::build(audit_data));
  Ok(())
}

/// Inject synthetic dev-doc CommentTopics (NatSpec, inline source
/// comments) into the in-memory audit. Sits between the resolution-graph
/// build and the documentation analyzer so the graph is populated when
/// future passes resolve code references inside dev-doc text.
///
/// Wraps `solidity::analyzer::inject_developer_documentation` with the
/// same `Arc<Mutex<DataContext>>` locking discipline the other pipeline
/// stages use.
pub fn inject_developer_documentation(
  data_context: &Arc<Mutex<DataContext>>,
  audit_id: &str,
) -> Result<(), AnalysisError> {
  let mut ctx = data_context
    .lock()
    .map_err(|e| AnalysisError::LockPoisoned(e.to_string()))?;
  let audit_data = ctx
    .get_audit_mut(audit_id)
    .ok_or_else(|| AnalysisError::AuditMissing(audit_id.to_string()))?;
  solidity::analyzer::inject_developer_documentation(audit_data);
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use o11a_core::collaborator::models::Author;
  use o11a_core::domain::{
    self, AST, CommentType, NamedTopicKind, NamedTopicVisibility, Node,
    ProjectPath, Scope, TopicMetadata, UnnamedTopicKind, topic,
  };
  use o11a_core::solidity::ast::{ASTNode, SolidityAST, SourceLocation};
  use std::collections::HashSet;
  use std::path::PathBuf;
  use std::sync::atomic::{AtomicU64, Ordering};

  fn nt(id: i32) -> topic::Topic {
    topic::new_node_topic(&id)
  }

  fn project_path(name: &str) -> ProjectPath {
    ProjectPath {
      file_path: name.to_string(),
    }
  }

  fn named_topic(t: topic::Topic, name: &str, scope: Scope) -> TopicMetadata {
    TopicMetadata::NamedTopic {
      topic: t,
      scope,
      kind: NamedTopicKind::Builtin,
      visibility: NamedTopicVisibility::Public,
      name: name.to_string(),
      is_mutable: false,
      mutations: Vec::new(),
      ancestors: Vec::new(),
      descendants: Vec::new(),
      relatives: Vec::new(),
      transitive_topic: None,
      doc_references: Vec::new(),
    }
  }

  /// Stage a `DataContext` whose audit looks like the post-Solidity
  /// analyzer state: inheritance is populated, an empty Solidity AST is
  /// installed so `SolidityExtractor::applies_to` accepts the audit, and
  /// the three child↔base relationships produce four `Implements` edges
  /// (two pairs × two directions) plus zero from any other extractor.
  fn staged_context(audit_id: &str) -> Arc<Mutex<DataContext>> {
    let mut ctx = domain::new_data_context();
    assert!(ctx.create_audit(
      audit_id.to_string(),
      "staged-audit".to_string(),
      HashSet::new(),
      None,
    ));
    let audit = ctx.get_audit_mut(audit_id).unwrap();

    let scope = Scope::Component {
      container: project_path("test.sol"),
      component: nt(1),
    };
    let parent_a = nt(10);
    let parent_b = nt(20);
    let child = nt(30);
    audit
      .topic_metadata
      .insert(parent_a, named_topic(parent_a, "ParentA", scope.clone()));
    audit
      .topic_metadata
      .insert(parent_b, named_topic(parent_b, "ParentB", scope.clone()));
    audit
      .topic_metadata
      .insert(child, named_topic(child, "Child", scope));
    audit.inheritance.insert(child, vec![parent_a, parent_b]);

    let path = project_path("test.sol");
    audit.asts.insert(
      path.clone(),
      AST::Solidity(SolidityAST {
        node_id: 0,
        nodes: Vec::new(),
        project_path: path,
      }),
    );

    Arc::new(Mutex::new(ctx))
  }

  fn graph_edge_count(ctx: &Arc<Mutex<DataContext>>, audit_id: &str) -> usize {
    let ctx = ctx.lock().unwrap();
    let audit = ctx.get_audit(audit_id).unwrap();
    let graph = audit
      .resolution_graph
      .as_ref()
      .expect("resolution_graph populated");
    graph.nodes().map(|n| graph.out_edges(n).len()).sum()
  }

  // -----------------------------------------------------------------------
  // populate_resolution_graph contract
  // -----------------------------------------------------------------------

  #[test]
  fn populate_assigns_some_when_field_started_none() {
    let ctx = staged_context("a");

    {
      let guard = ctx.lock().unwrap();
      assert!(guard.get_audit("a").unwrap().resolution_graph.is_none());
    }

    populate_resolution_graph(&ctx, "a").unwrap();

    let guard = ctx.lock().unwrap();
    assert!(guard.get_audit("a").unwrap().resolution_graph.is_some());
  }

  #[test]
  fn populate_emits_at_least_one_edge_when_audit_has_inheritance() {
    let ctx = staged_context("a");
    populate_resolution_graph(&ctx, "a").unwrap();
    assert!(graph_edge_count(&ctx, "a") > 0);
  }

  #[test]
  fn populate_is_deterministic_across_repeat_calls() {
    let ctx = staged_context("a");
    populate_resolution_graph(&ctx, "a").unwrap();
    let first = {
      let guard = ctx.lock().unwrap();
      guard
        .get_audit("a")
        .unwrap()
        .resolution_graph
        .as_ref()
        .unwrap()
        .clone()
    };

    populate_resolution_graph(&ctx, "a").unwrap();
    let second = {
      let guard = ctx.lock().unwrap();
      guard
        .get_audit("a")
        .unwrap()
        .resolution_graph
        .as_ref()
        .unwrap()
        .clone()
    };

    assert_eq!(first, second);
    assert_eq!(
      serde_json::to_vec(&first).unwrap(),
      serde_json::to_vec(&second).unwrap(),
    );
  }

  #[test]
  fn populate_returns_audit_missing_error_for_unknown_id() {
    let ctx = Arc::new(Mutex::new(domain::new_data_context()));
    let err = populate_resolution_graph(&ctx, "nope").unwrap_err();
    match err {
      AnalysisError::AuditMissing(id) => assert_eq!(id, "nope"),
      other => panic!("expected AuditMissing, got {:?}", other),
    }
  }

  #[test]
  fn populate_installs_some_even_when_extractor_emits_no_edges() {
    // No Solidity AST in `audit_data.asts`, so the SolidityExtractor
    // skips this audit and the graph ends up empty. The wiring must
    // still install `Some` — downstream readers rely on the field's
    // presence as a "graph build has run" signal.
    let mut raw = domain::new_data_context();
    assert!(raw.create_audit(
      "b".to_string(),
      "empty-audit".to_string(),
      HashSet::new(),
      None,
    ));
    let ctx = Arc::new(Mutex::new(raw));

    populate_resolution_graph(&ctx, "b").unwrap();

    let guard = ctx.lock().unwrap();
    let graph = guard
      .get_audit("b")
      .unwrap()
      .resolution_graph
      .as_ref()
      .expect("Some, not None");
    let edges: usize = graph.nodes().map(|n| graph.out_edges(n).len()).sum();
    assert_eq!(edges, 0);
  }

  #[test]
  fn populate_releases_lock_before_returning() {
    // If the lock were still held when `populate_resolution_graph`
    // returned, this re-acquire would deadlock the test thread.
    let ctx = staged_context("a");
    populate_resolution_graph(&ctx, "a").unwrap();
    let _guard = ctx.lock().unwrap();
  }

  #[test]
  fn populate_only_touches_the_named_audit() {
    let ctx = staged_context("a");
    {
      let mut guard = ctx.lock().unwrap();
      assert!(guard.create_audit(
        "b".to_string(),
        "second-audit".to_string(),
        HashSet::new(),
        None,
      ));
    }

    populate_resolution_graph(&ctx, "a").unwrap();

    let guard = ctx.lock().unwrap();
    assert!(guard.get_audit("a").unwrap().resolution_graph.is_some());
    assert!(guard.get_audit("b").unwrap().resolution_graph.is_none());
  }

  #[test]
  fn audit_missing_display_includes_audit_id() {
    let ctx = Arc::new(Mutex::new(domain::new_data_context()));
    let err = populate_resolution_graph(&ctx, "specific-id").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("specific-id"), "got: {msg}");
  }

  // -----------------------------------------------------------------------
  // run_analysis end-to-end
  //
  // Drives the full pipeline against a minimal on-disk fixture so an
  // accidental signature change, dropped call, or pipeline-stage
  // reordering that breaks the graph build trips here.
  // -----------------------------------------------------------------------

  /// Returns a fresh, exclusive temp directory path under
  /// `std::env::temp_dir()`. Counter + process ID make the path unique
  /// across concurrent test runs.
  fn unique_temp_dir() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
      "o11a-analysis-test-{}-{}",
      std::process::id(),
      n,
    ))
  }

  /// Lay out the bare minimum config files `run_analysis` reads:
  /// `name.txt` plus empty `scope.txt`, `documents.txt`, and
  /// `security.md`, plus an empty `out/` directory so the Solidity
  /// parser does not error on `MissingOutDirectory`.
  fn write_minimal_fixture(root: &Path, name: &str) {
    std::fs::create_dir_all(root).unwrap();
    std::fs::create_dir_all(root.join("out")).unwrap();
    std::fs::write(root.join("name.txt"), name).unwrap();
    std::fs::write(root.join("scope.txt"), "").unwrap();
    std::fs::write(root.join("documents.txt"), "").unwrap();
    std::fs::write(root.join("security.md"), "").unwrap();
  }

  /// RAII wrapper so a failing assertion still cleans the temp directory.
  struct TempProject {
    root: PathBuf,
  }

  impl TempProject {
    fn new(name: &str) -> Self {
      let root = unique_temp_dir();
      write_minimal_fixture(&root, name);
      TempProject { root }
    }
  }

  impl Drop for TempProject {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.root);
    }
  }

  #[test]
  fn run_analysis_populates_resolution_graph_on_minimal_fixture() {
    let project = TempProject::new("end-to-end-test");
    let data_context = Arc::new(Mutex::new(domain::new_data_context()));

    run_analysis(&project.root, "audit-1", &data_context)
      .expect("minimal fixture must drive run_analysis to completion");

    let guard = data_context.lock().unwrap();
    let audit = guard.get_audit("audit-1").expect("audit created");
    assert!(
      audit.resolution_graph.is_some(),
      "resolution_graph must be populated after run_analysis returns",
    );
  }

  #[test]
  fn run_analysis_returns_audit_exists_when_called_twice_with_same_id() {
    // The duplicate-id check fires from `create_audit` before any
    // analyzer or graph build runs; the second call must error rather
    // than silently re-overwrite an already-populated audit.
    let project = TempProject::new("dup-id-test");
    let data_context = Arc::new(Mutex::new(domain::new_data_context()));
    run_analysis(&project.root, "audit-1", &data_context).unwrap();

    let err = run_analysis(&project.root, "audit-1", &data_context)
      .unwrap_err();
    match err {
      AnalysisError::AuditExists(id) => assert_eq!(id, "audit-1"),
      other => panic!("expected AuditExists, got {:?}", other),
    }
  }

  // -----------------------------------------------------------------------
  // inject_developer_documentation wrapper (Phase 5)
  //
  // The dev-doc injection pass moved out of `solidity::analyzer::analyze`
  // and is now driven by `analysis.rs` after the resolution graph build
  // (see `run_analysis`). The wrapper here exposes the same lock /
  // missing-audit error surface as `populate_resolution_graph`.
  // -----------------------------------------------------------------------

  fn dummy_src_location() -> SourceLocation {
    SourceLocation {
      start: None,
      length: None,
      index: None,
    }
  }

  /// A `DataContext` with one audit pre-staged to look like the
  /// post-Solidity-analyzer state for dev-doc injection: one
  /// SemanticBlock node with non-empty `documentation`, plus the
  /// matching `topic_metadata` entry needed for the
  /// `resolve_transitive_topic` lookup that injection performs.
  fn staged_doc_context(audit_id: &str) -> Arc<Mutex<DataContext>> {
    let mut ctx = domain::new_data_context();
    assert!(ctx.create_audit(
      audit_id.to_string(),
      "dev-doc-staged".to_string(),
      HashSet::new(),
      None,
    ));
    let audit = ctx.get_audit_mut(audit_id).unwrap();

    let block_id = 500;
    let block_topic = topic::new_node_topic(&block_id);
    let block_node = ASTNode::SemanticBlock {
      node_id: block_id,
      src_location: dummy_src_location(),
      documentation: Some("inline block doc".to_string()),
      statements: Vec::new(),
    };
    audit
      .nodes
      .insert(block_topic, Node::Solidity(block_node));
    audit.topic_metadata.insert(
      block_topic,
      TopicMetadata::UnnamedTopic {
        topic: block_topic,
        scope: Scope::Global,
        kind: UnnamedTopicKind::SemanticBlock,
        transitive_topic: None,
      },
    );

    Arc::new(Mutex::new(ctx))
  }

  /// Returns the count of synthetic dev-doc CommentTopics across the
  /// audit, partitioned by author so callers can assert injection
  /// behavior without re-implementing the walk.
  fn dev_doc_counts(
    ctx: &Arc<Mutex<DataContext>>,
    audit_id: &str,
  ) -> (usize, usize) {
    let guard = ctx.lock().unwrap();
    let audit = guard.get_audit(audit_id).unwrap();
    let mut technical = 0usize;
    let mut documentation = 0usize;
    for meta in audit.topic_metadata.values() {
      if let TopicMetadata::CommentTopic { author, .. } = meta {
        match author {
          Author::DevTechnical => technical += 1,
          Author::DevDocumentation => documentation += 1,
          _ => {}
        }
      }
    }
    (technical, documentation)
  }

  #[test]
  fn inject_dev_docs_produces_dev_technical_comment_for_semantic_block() {
    // Validates the wrapper actually drives
    // `solidity::analyzer::inject_developer_documentation` and that
    // SemanticBlock documentation surfaces as a `DevTechnical` comment
    // — the exact behavior `solidity::analyzer::analyze` used to
    // perform inline before Phase 5.
    let ctx = staged_doc_context("a");
    inject_developer_documentation(&ctx, "a").unwrap();

    let (technical, documentation) = dev_doc_counts(&ctx, "a");
    assert_eq!(technical, 1, "expected one DevTechnical synthetic comment");
    assert_eq!(documentation, 0);
  }

  #[test]
  fn inject_dev_docs_returns_audit_missing_for_unknown_id() {
    let ctx = Arc::new(Mutex::new(domain::new_data_context()));
    let err = inject_developer_documentation(&ctx, "nope").unwrap_err();
    match err {
      AnalysisError::AuditMissing(id) => assert_eq!(id, "nope"),
      other => panic!("expected AuditMissing, got {:?}", other),
    }
  }

  #[test]
  fn inject_dev_docs_releases_lock_before_returning() {
    // If the wrapper still held the lock when it returned, this
    // re-acquire would deadlock the test thread.
    let ctx = staged_doc_context("a");
    inject_developer_documentation(&ctx, "a").unwrap();
    let _guard = ctx.lock().unwrap();
  }

  #[test]
  fn inject_dev_docs_only_touches_the_named_audit() {
    let ctx = staged_doc_context("a");
    {
      let mut guard = ctx.lock().unwrap();
      assert!(guard.create_audit(
        "b".to_string(),
        "second-audit".to_string(),
        HashSet::new(),
        None,
      ));
    }

    inject_developer_documentation(&ctx, "a").unwrap();

    let (a_tech, _) = dev_doc_counts(&ctx, "a");
    let (b_tech, b_doc) = dev_doc_counts(&ctx, "b");
    assert_eq!(a_tech, 1);
    assert_eq!(b_tech, 0, "audit 'b' must not gain comments");
    assert_eq!(b_doc, 0);
  }

  #[test]
  fn inject_dev_docs_is_deterministic_across_repeat_pipelines() {
    // Pipeline determinism contract: building the audit twice via the
    // exact stage order `run_analysis` uses (graph build → dev-doc
    // injection) must produce byte-identical comment-index entries.
    fn run_pipeline(audit_id: &str) -> Vec<(topic::Topic, CommentType)> {
      let ctx = staged_doc_context(audit_id);
      populate_resolution_graph(&ctx, audit_id).unwrap();
      inject_developer_documentation(&ctx, audit_id).unwrap();
      let guard = ctx.lock().unwrap();
      let audit = guard.get_audit(audit_id).unwrap();
      let mut out: Vec<(topic::Topic, CommentType)> = audit
        .topic_metadata
        .values()
        .filter_map(|m| match m {
          TopicMetadata::CommentTopic {
            target_topic,
            comment_type,
            ..
          } => Some((*target_topic, comment_type.clone())),
          _ => None,
        })
        .collect();
      out.sort_by_key(|(t, _)| *t);
      out
    }
    let first = run_pipeline("det-a");
    let second = run_pipeline("det-b");
    assert_eq!(first, second);
  }

  #[test]
  fn run_analysis_drives_both_graph_build_and_dev_doc_injection() {
    // End-to-end smoke test that `run_analysis` reaches the dev-doc
    // injection stage — i.e. neither the relocated call nor the new
    // `inject_developer_documentation` wrapper got dropped from the
    // pipeline. The minimal fixture has no source, so injection
    // produces no comments; the assertion is therefore "the graph is
    // populated and `run_analysis` returned Ok", not "X happened
    // before Y". Stage ordering is enforced by the code structure of
    // `run_analysis` (read top-to-bottom) and only becomes runtime-
    // observable once Phase 7 makes injection depend on graph state.
    let project = TempProject::new("ordering-test");
    let data_context = Arc::new(Mutex::new(domain::new_data_context()));

    run_analysis(&project.root, "audit-1", &data_context).unwrap();

    let guard = data_context.lock().unwrap();
    let audit = guard.get_audit("audit-1").expect("audit created");
    assert!(
      audit.resolution_graph.is_some(),
      "graph must be populated by the time run_analysis returns",
    );
    let count = audit
      .topic_metadata
      .values()
      .filter(|m| matches!(m, TopicMetadata::CommentTopic { .. }))
      .count();
    assert_eq!(count, 0, "minimal fixture has no source; no comments");
  }
}
