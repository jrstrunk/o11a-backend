//! Audit report: the canonical JSON interchange format for pipeline output.
//!
//! The analysis pipeline is a one-shot batch process whose output is purely a
//! function of the source inputs. This module models that output as a
//! versioned JSON document (`audit.json`) that can be consumed by:
//!   - `o11a-server` at startup to hydrate in-memory analysis state
//!   - CI runners, LLM toolchains, dashboards, or any external consumer
//!
//! Collaboration state (user comments, votes, statuses, user-authored
//! threats/invariants/conditions) is _not_ part of this report — it is
//! owned by the collaboration server's SQLite store and layered on top of
//! the report at read time.
//!
//! ## Schema versioning
//!
//! `schema_version` is bumped whenever a breaking change is made to the
//! shape of the report. Readers should reject reports with a version they
//! don't understand rather than silently skipping fields. Additive changes
//! (new optional fields) do not require a bump.
//!
//! Current version: 2 (alpha — stability not yet guaranteed)

use crate::collaborator::models::AUTHOR_SYSTEM;
use crate::core::{AuditData, Requirement, TopicMetadata};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The current audit-report schema version. Bump on breaking changes.
pub const SCHEMA_VERSION: u32 = 2;

/// Name of the tool that produced the report.
pub const GENERATOR_NAME: &str = "o11a-analyze";

/// Version of the generator tool. Tied to the crate version.
pub const GENERATOR_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A complete audit report — the on-disk form of `audit.json`.
///
/// Top-level fields:
/// - `schema_version`: readers must reject unknown versions.
/// - `generator`: metadata about the tool that wrote the file.
/// - `generated_at`: ISO-8601 UTC timestamp of when the file was produced.
/// - `audit`: identifying metadata about the audit (name, scope).
/// - `pipeline`: the pipeline's immutable outputs (features, behaviors, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditReport {
  pub schema_version: u32,
  pub generator: GeneratorInfo,
  pub generated_at: String,
  pub audit: AuditMetadata,
  pub pipeline: PipelineOutput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratorInfo {
  pub name: String,
  pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditMetadata {
  pub id: String,
  pub name: String,
  /// In-scope file paths, relative to the project root, sorted for stability.
  pub in_scope_files: Vec<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub security_notes: Option<String>,
}

/// All pipeline-produced data. Everything here is derived deterministically
/// from the source inputs and is rewritten wholesale on each analysis run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineOutput {
  pub features: Vec<ReportFeature>,
  pub requirements: Vec<ReportRequirement>,
  pub behaviors: Vec<ReportBehavior>,
  pub functional_semantics: Vec<ReportFunctionalSemantic>,
  pub feature_requirement_links: Vec<FeatureLink>,
  pub feature_behavior_links: Vec<FeatureLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportFeature {
  /// F-prefixed topic id.
  pub topic: String,
  pub name: String,
  pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportRequirement {
  /// R-prefixed topic id.
  pub topic: String,
  pub description: String,
  /// D-prefixed documentation section topic this requirement was extracted from.
  pub section_topic: String,
  /// D-prefixed documentation topics that informed this requirement.
  pub documentation_topics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportBehavior {
  /// B-prefixed topic id.
  pub topic: String,
  pub description: String,
  /// N-prefixed code member topic this behavior belongs to.
  pub member_topic: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportFunctionalSemantic {
  /// P-prefixed topic id.
  pub topic: String,
  pub description: String,
  /// N-prefixed code declaration this semantic describes.
  pub declaration_topic: String,
  /// D-prefixed documentation topics this semantic was derived from.
  pub documentation_topics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureLink {
  pub feature_topic: String,
  pub topic: String,
}

// ============================================================================
// Export: AuditData → AuditReport
// ============================================================================

/// Capture the pipeline output portion of an `AuditData` as an `AuditReport`.
///
/// `generated_at` is an ISO-8601 UTC timestamp that callers supply so that
/// this module stays free of time-source dependencies.
pub fn build_report(
  audit_id: &str,
  audit_data: &AuditData,
  generated_at: String,
) -> AuditReport {
  let mut in_scope_files: Vec<String> = audit_data
    .in_scope_files
    .iter()
    .map(|p| p.file_path.clone())
    .collect();
  in_scope_files.sort_unstable();

  let audit = AuditMetadata {
    id: audit_id.to_string(),
    name: audit_data.audit_name.clone(),
    in_scope_files,
    security_notes: audit_data.security_notes.clone(),
  };

  let pipeline = PipelineOutput {
    features: collect_features(audit_data),
    requirements: collect_requirements(audit_data),
    behaviors: collect_behaviors(audit_data),
    functional_semantics: collect_functional_semantics(audit_data),
    feature_requirement_links: flatten_links(
      &audit_data.feature_requirement_links,
    ),
    feature_behavior_links: flatten_links(&audit_data.feature_behavior_links),
  };

  AuditReport {
    schema_version: SCHEMA_VERSION,
    generator: GeneratorInfo {
      name: GENERATOR_NAME.to_string(),
      version: GENERATOR_VERSION.to_string(),
    },
    generated_at,
    audit,
    pipeline,
  }
}

fn collect_features(audit_data: &AuditData) -> Vec<ReportFeature> {
  let mut out: Vec<ReportFeature> = audit_data
    .topic_metadata
    .values()
    .filter_map(|m| match m {
      TopicMetadata::FeatureTopic {
        topic,
        name,
        description,
        ..
      } => Some(ReportFeature {
        topic: topic.id().to_string(),
        name: name.clone(),
        description: description.clone(),
      }),
      _ => None,
    })
    .collect();
  out.sort_by(|a, b| a.topic.cmp(&b.topic));
  out
}

fn collect_requirements(audit_data: &AuditData) -> Vec<ReportRequirement> {
  let mut out: Vec<ReportRequirement> = audit_data
    .topic_metadata
    .values()
    .filter_map(|m| match m {
      TopicMetadata::RequirementTopic {
        topic,
        description,
        section_topic,
        ..
      } => {
        let documentation_topics = audit_data
          .requirements
          .get(topic)
          .map(
            |Requirement {
               documentation_topics,
             }| {
              documentation_topics
                .iter()
                .map(|t| t.id().to_string())
                .collect()
            },
          )
          .unwrap_or_default();
        Some(ReportRequirement {
          topic: topic.id().to_string(),
          description: description.clone(),
          section_topic: section_topic.id().to_string(),
          documentation_topics,
        })
      }
      _ => None,
    })
    .collect();
  out.sort_by(|a, b| a.topic.cmp(&b.topic));
  out
}

fn collect_behaviors(audit_data: &AuditData) -> Vec<ReportBehavior> {
  let mut out: Vec<ReportBehavior> = audit_data
    .topic_metadata
    .values()
    .filter_map(|m| match m {
      TopicMetadata::BehaviorTopic {
        topic,
        description,
        member_topic,
        ..
      } => Some(ReportBehavior {
        topic: topic.id().to_string(),
        description: description.clone(),
        member_topic: member_topic.id().to_string(),
      }),
      _ => None,
    })
    .collect();
  out.sort_by(|a, b| a.topic.cmp(&b.topic));
  out
}

fn collect_functional_semantics(
  audit_data: &AuditData,
) -> Vec<ReportFunctionalSemantic> {
  let mut out: Vec<ReportFunctionalSemantic> = audit_data
    .topic_metadata
    .values()
    .filter_map(|m| match m {
      TopicMetadata::FunctionalSemanticTopic {
        topic,
        description,
        declaration_topic,
        documentation_topics,
        ..
      } => Some(ReportFunctionalSemantic {
        topic: topic.id().to_string(),
        description: description.clone(),
        declaration_topic: declaration_topic.id().to_string(),
        documentation_topics: documentation_topics
          .iter()
          .map(|t| t.id().to_string())
          .collect(),
      }),
      _ => None,
    })
    .collect();
  out.sort_by(|a, b| a.topic.cmp(&b.topic));
  out
}

fn flatten_links(
  map: &BTreeMap<crate::core::topic::Topic, Vec<crate::core::topic::Topic>>,
) -> Vec<FeatureLink> {
  let mut out = Vec::new();
  for (feat_topic, linked) in map {
    for t in linked {
      out.push(FeatureLink {
        feature_topic: feat_topic.id().to_string(),
        topic: t.id().to_string(),
      });
    }
  }
  out.sort_by(|a, b| {
    (a.feature_topic.as_str(), a.topic.as_str())
      .cmp(&(b.feature_topic.as_str(), b.topic.as_str()))
  });
  out
}

// ============================================================================
// Import: AuditReport → AuditData (applied on top of a freshly-parsed audit)
// ============================================================================

/// Errors that can occur when applying a report to an `AuditData`.
#[derive(Debug)]
pub enum ApplyReportError {
  UnsupportedSchemaVersion { found: u32, supported: u32 },
  AuditIdMismatch { expected: String, found: String },
}

impl std::fmt::Display for ApplyReportError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      ApplyReportError::UnsupportedSchemaVersion { found, supported } => {
        write!(
          f,
          "unsupported schema_version {} (this build supports {})",
          found, supported
        )
      }
      ApplyReportError::AuditIdMismatch { expected, found } => {
        write!(
          f,
          "audit id mismatch: report is for '{}', but target is '{}'",
          found, expected
        )
      }
    }
  }
}

impl std::error::Error for ApplyReportError {}

/// Apply a report's pipeline output onto an `AuditData` that has already been
/// populated from source parsing (ASTs, symbol tables, topic metadata for
/// parsed declarations). This installs the LLM-derived topics (features,
/// requirements, behaviors, functional semantics) and their links.
///
/// Callers should invoke `crate::core::rebuild_feature_context` on the audit
/// data after applying the report, so that reverse indexes are refreshed.
pub fn apply_report(
  audit_id: &str,
  audit_data: &mut AuditData,
  report: &AuditReport,
) -> Result<(), ApplyReportError> {
  if report.schema_version != SCHEMA_VERSION {
    return Err(ApplyReportError::UnsupportedSchemaVersion {
      found: report.schema_version,
      supported: SCHEMA_VERSION,
    });
  }
  if report.audit.id != audit_id {
    return Err(ApplyReportError::AuditIdMismatch {
      expected: audit_id.to_string(),
      found: report.audit.id.clone(),
    });
  }

  use crate::core::topic;

  // Drop any stale pipeline-topic metadata before hydrating from the report.
  audit_data.topic_metadata.retain(|_, m| {
    !matches!(
      m,
      TopicMetadata::FeatureTopic { .. }
        | TopicMetadata::RequirementTopic { .. }
        | TopicMetadata::BehaviorTopic { .. }
        | TopicMetadata::FunctionalSemanticTopic { .. }
    )
  });
  audit_data.requirements.clear();
  audit_data.feature_requirement_links.clear();
  audit_data.feature_behavior_links.clear();

  for f in &report.pipeline.features {
    let topic = topic::new_topic(&f.topic);
    audit_data.topic_metadata.insert(
      topic,
      TopicMetadata::FeatureTopic {
        topic,
        name: f.name.clone(),
        description: f.description.clone(),
        author_id: AUTHOR_SYSTEM,
        created_at: None,
      },
    );
  }

  for r in &report.pipeline.requirements {
    let topic = topic::new_topic(&r.topic);
    let section_topic = topic::new_topic(&r.section_topic);
    let documentation_topics: Vec<_> = r
      .documentation_topics
      .iter()
      .map(|id| topic::new_topic(id))
      .collect();

    audit_data.requirements.insert(
      topic,
      Requirement {
        documentation_topics,
      },
    );

    audit_data.topic_metadata.insert(
      topic,
      TopicMetadata::RequirementTopic {
        topic,
        description: r.description.clone(),
        section_topic,
        author_id: AUTHOR_SYSTEM,
        created_at: None,
      },
    );
  }

  for b in &report.pipeline.behaviors {
    let topic = topic::new_topic(&b.topic);
    let member_topic = topic::new_topic(&b.member_topic);
    audit_data.topic_metadata.insert(
      topic,
      TopicMetadata::BehaviorTopic {
        topic,
        description: b.description.clone(),
        member_topic,
        author_id: AUTHOR_SYSTEM,
        created_at: None,
      },
    );
  }

  for s in &report.pipeline.functional_semantics {
    let topic = topic::new_topic(&s.topic);
    let declaration_topic = topic::new_topic(&s.declaration_topic);
    let documentation_topics: Vec<_> = s
      .documentation_topics
      .iter()
      .map(|id| topic::new_topic(id))
      .collect();
    audit_data.topic_metadata.insert(
      topic,
      TopicMetadata::FunctionalSemanticTopic {
        topic,
        description: s.description.clone(),
        declaration_topic,
        documentation_topics,
        author_id: AUTHOR_SYSTEM,
        created_at: None,
      },
    );
  }

  for link in &report.pipeline.feature_requirement_links {
    audit_data
      .feature_requirement_links
      .entry(topic::new_topic(&link.feature_topic))
      .or_default()
      .push(topic::new_topic(&link.topic));
  }
  for link in &report.pipeline.feature_behavior_links {
    audit_data
      .feature_behavior_links
      .entry(topic::new_topic(&link.feature_topic))
      .or_default()
      .push(topic::new_topic(&link.topic));
  }

  Ok(())
}
