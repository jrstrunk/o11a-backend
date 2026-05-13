//! Analysis artifact: the binary handoff from `o11a-analyze` to
//! `o11a-server`.
//!
//! The analyzer produces two outputs alongside each audit:
//!   - `audit.json` — the canonical, human-readable pipeline report
//!     (features, requirements, behaviors, functional semantics, and the
//!     feature links). Readable by external tooling.
//!   - `audit.analysis.bin` — this module's output: a bincode-encoded
//!     snapshot of the analyzed `AuditData` (ASTs, topic metadata, source
//!     contexts, name indexes, security notes, etc.). Everything the
//!     server needs to serve code views without re-running the analyzer.
//!
//! ## Why binary
//!
//! The snapshot contains the full AST graph for every Solidity file in the
//! audit plus the derived analyzer state. Serializing that as JSON blows up
//! both file size and the server's startup parse time, so this artifact uses
//! bincode. It is a private producer/consumer contract between `o11a-analyze`
//! and `o11a-server`; external consumers should read `audit.json` instead.
//!
//! ## What's in vs. out of the snapshot
//!
//! Included (see [`AuditDataSnapshot`]):
//!   - `audit_name`, `in_scope_files`, `security_notes`
//!   - `asts`, `nodes`, `function_properties`, `variable_types`
//!   - `topic_metadata` *minus* the pipeline-output variants
//!     (FeatureTopic, RequirementTopic, BehaviorTopic,
//!     CharacteristicTopic, FunctionalSemanticTopic — those are in
//!     `audit.json` and are reapplied by `report::apply_report`)
//!   - `name_index`, `comment_index`
//!   - `topic_context`, `expanded_topic_context`
//!   - `threat_feature_links`
//!   - `mentions_index`
//!
//! Excluded (reconstructed after load):
//!   - `requirements`, `characteristics`, `feature_requirement_links`,
//!     `feature_behavior_links` — applied from `audit.json` via
//!     `report::apply_report`
//!   - `section_requirements`, `section_characteristics`, `member_behaviors`,
//!     `declaration_semantics`, `subject_purposes`, `subject_placements`,
//!     `subject_conditions`, `subject_threats`, `condition_threats`,
//!     `threat_invariants`, `subject_invariants` — derivable reverse
//!     indexes, rebuilt via `domain::rebuild_feature_context`
//!
//! ## Version compatibility
//!
//! [`ARTIFACT_SCHEMA_VERSION`] is bumped on any breaking change to the
//! bincode layout (new fields, removed fields, or changes to any nested
//! type's serialization). [`read_artifact`] rejects mismatched versions
//! with a clear error so the server can ask the operator to regenerate
//! the artifact by re-running `o11a-analyze`.

use crate::domain::{
  AST, AuditData, FunctionModProperties, Node, ProjectPath, SolidityType,
  SourceContext, ThreatFeatureLink, TopicMetadata, TopicNameIndex, topic,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Bumped on any breaking change to [`AuditDataSnapshot`] or
/// [`AnalysisArtifact`]. The server refuses to load a file whose version
/// it doesn't recognize.
pub const ARTIFACT_SCHEMA_VERSION: u32 = 9;

/// Binary envelope for the analyzed `AuditData` snapshot. Private format
/// between `o11a-analyze` (writer) and `o11a-server` (reader). Encoded
/// with bincode v2.
#[derive(Debug, Serialize, Deserialize)]
pub struct AnalysisArtifact {
  pub schema_version: u32,
  pub generator: String,
  pub generator_version: String,
  pub generated_at: String,
  pub audit_id: String,
  pub payload: AuditDataSnapshot,
}

/// The subset of `AuditData` produced by the analyzer and preserved in
/// the binary artifact. Pipeline-output fields (requirements, links,
/// and the four LLM-sourced topic metadata variants) are stripped out
/// and arrive separately via `audit.json`. Derivable reverse indexes
/// are likewise stripped and rebuilt by
/// [`crate::domain::rebuild_feature_context`] after load.
#[derive(Debug, Serialize, Deserialize)]
pub struct AuditDataSnapshot {
  pub audit_name: String,
  pub in_scope_files: HashSet<ProjectPath>,
  pub security_notes: Option<String>,
  pub asts: BTreeMap<ProjectPath, AST>,
  pub nodes: BTreeMap<topic::Topic, Node>,
  pub topic_metadata: BTreeMap<topic::Topic, TopicMetadata>,
  pub function_properties: BTreeMap<topic::Topic, FunctionModProperties>,
  pub variable_types: BTreeMap<topic::Topic, SolidityType>,
  pub name_index: TopicNameIndex,
  pub comment_index: HashMap<topic::Topic, Vec<topic::Topic>>,
  pub topic_context: BTreeMap<topic::Topic, Vec<SourceContext>>,
  pub expanded_topic_context: BTreeMap<topic::Topic, Vec<SourceContext>>,
  pub threat_feature_links: Vec<ThreatFeatureLink>,
  pub mentions_index: HashMap<topic::Topic, Vec<topic::Topic>>,
}

/// Strip pipeline-output fields and derivable reverse indexes from an
/// `AuditData` to produce a snapshot suitable for serialization. The
/// pipeline-sourced `TopicMetadata` variants are filtered out of
/// `topic_metadata`; they will be reinstalled by `report::apply_report`.
pub fn snapshot_from_audit_data(audit_data: &AuditData) -> AuditDataSnapshot {
  let topic_metadata = audit_data
    .topic_metadata
    .iter()
    .filter(|(_, m)| {
      !matches!(
        m,
        TopicMetadata::FeatureTopic { .. }
          | TopicMetadata::RequirementTopic { .. }
          | TopicMetadata::BehaviorTopic { .. }
          | TopicMetadata::CharacteristicTopic { .. }
          | TopicMetadata::FunctionalSemanticTopic { .. }
      )
    })
    .map(|(t, m)| (*t, m.clone()))
    .collect();

  AuditDataSnapshot {
    audit_name: audit_data.audit_name.clone(),
    in_scope_files: audit_data.in_scope_files.clone(),
    security_notes: audit_data.security_notes.clone(),
    asts: audit_data.asts.clone(),
    nodes: audit_data.nodes.clone(),
    topic_metadata,
    function_properties: audit_data.function_properties.clone(),
    variable_types: audit_data.variable_types.clone(),
    name_index: audit_data.name_index.clone(),
    comment_index: audit_data.comment_index.clone(),
    topic_context: audit_data.topic_context.clone(),
    expanded_topic_context: audit_data.expanded_topic_context.clone(),
    threat_feature_links: audit_data.threat_feature_links.clone(),
    mentions_index: audit_data.mentions_index.clone(),
  }
}

/// Rehydrate a snapshot into a fresh `AuditData`. Pipeline-output fields
/// are left empty; the caller must invoke
/// [`crate::report::apply_report`] (to fill in requirements,
/// characteristics, and links) and then
/// [`crate::domain::rebuild_feature_context`] (to rebuild reverse indexes)
/// before serving requests.
pub fn apply_snapshot(audit_data: &mut AuditData, snap: AuditDataSnapshot) {
  audit_data.audit_name = snap.audit_name;
  audit_data.in_scope_files = snap.in_scope_files;
  audit_data.security_notes = snap.security_notes;
  audit_data.asts = snap.asts;
  audit_data.nodes = snap.nodes;
  audit_data.topic_metadata = snap.topic_metadata;
  audit_data.function_properties = snap.function_properties;
  audit_data.variable_types = snap.variable_types;
  audit_data.name_index = snap.name_index;
  audit_data.comment_index = snap.comment_index;
  audit_data.topic_context = snap.topic_context;
  audit_data.expanded_topic_context = snap.expanded_topic_context;
  audit_data.threat_feature_links = snap.threat_feature_links;
  audit_data.mentions_index = snap.mentions_index;
  audit_data.requirements.clear();
  audit_data.characteristics.clear();
  audit_data.feature_requirement_links.clear();
  audit_data.feature_behavior_links.clear();
  audit_data.section_requirements.clear();
  audit_data.section_characteristics.clear();
  audit_data.member_behaviors.clear();
  audit_data.declaration_semantics.clear();
  audit_data.subject_purposes.clear();
  audit_data.subject_placements.clear();
  audit_data.subject_conditions.clear();
  audit_data.subject_threats.clear();
  audit_data.condition_threats.clear();
  audit_data.threat_invariants.clear();
  audit_data.subject_invariants.clear();
}

/// Errors that can occur when reading, writing, or decoding an artifact.
#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
  #[error("I/O error: {0}")]
  Io(#[from] std::io::Error),
  #[error("decode error: {0}")]
  Decode(#[from] bincode::error::DecodeError),
  #[error("encode error: {0}")]
  Encode(#[from] bincode::error::EncodeError),
  #[error(
    "artifact schema version mismatch: found {found}, expected {expected}"
  )]
  VersionMismatch { found: u32, expected: u32 },
  #[error("artifact audit id mismatch: expected '{expected}', found '{found}'")]
  AuditIdMismatch { expected: String, found: String },
}

/// Serialize an [`AnalysisArtifact`] to `path` atomically: writes to a
/// sibling `<path>.tmp`, fsyncs, and renames into place. Creates the
/// parent directory if it does not already exist.
pub fn write_artifact(
  path: &Path,
  artifact: &AnalysisArtifact,
) -> Result<(), ArtifactError> {
  if let Some(parent) = path.parent()
    && !parent.as_os_str().is_empty()
  {
    std::fs::create_dir_all(parent)?;
  }

  let tmp_path: PathBuf = match path.file_name() {
    Some(name) => {
      let mut tmp_name = name.to_os_string();
      tmp_name.push(".tmp");
      path.with_file_name(tmp_name)
    }
    None => {
      return Err(ArtifactError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "artifact path has no file name",
      )));
    }
  };

  let encoded = bincode::serde::encode_to_vec(artifact, bincode_config())?;

  {
    let mut file = std::fs::File::create(&tmp_path)?;
    file.write_all(&encoded)?;
    file.sync_all()?;
  }

  std::fs::rename(&tmp_path, path)?;
  Ok(())
}

/// Read and decode an [`AnalysisArtifact`] from `path`. Returns an error
/// if the embedded `schema_version` does not match
/// [`ARTIFACT_SCHEMA_VERSION`].
pub fn read_artifact(path: &Path) -> Result<AnalysisArtifact, ArtifactError> {
  let bytes = std::fs::read(path)?;
  let (artifact, _read): (AnalysisArtifact, usize) =
    bincode::serde::decode_from_slice(&bytes, bincode_config())?;
  if artifact.schema_version != ARTIFACT_SCHEMA_VERSION {
    return Err(ArtifactError::VersionMismatch {
      found: artifact.schema_version,
      expected: ARTIFACT_SCHEMA_VERSION,
    });
  }
  Ok(artifact)
}

fn bincode_config() -> bincode::config::Configuration {
  bincode::config::standard()
}
