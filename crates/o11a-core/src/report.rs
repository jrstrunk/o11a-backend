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
//! Current version: 4 (alpha — stability not yet guaranteed)

use crate::collaborator::models::Author;
use crate::domain::{
  AuditData, Characteristic, MatchSource, Requirement,
  SystemCharacteristicKind, ThreatFeatureLink, TopicMetadata,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The current audit-report schema version. Bump on breaking changes.
pub const SCHEMA_VERSION: u32 = 5;

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
  pub characteristics: Vec<ReportCharacteristic>,
  pub functional_semantics: Vec<ReportFunctionalSemantic>,
  pub functional_purposes: Vec<ReportFunctionalPurpose>,
  pub placement_rationales: Vec<ReportPlacementRationale>,
  pub conditions: Vec<ReportCondition>,
  pub threats: Vec<ReportThreat>,
  pub invariants: Vec<ReportInvariant>,
  pub validations: Vec<ReportValidation>,
  pub threat_feature_links: Vec<ReportThreatFeatureLink>,
  pub feature_requirement_links: Vec<FeatureLink>,
  pub feature_behavior_links: Vec<FeatureLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportFeature {
  /// S-prefixed topic id.
  pub topic: String,
  pub name: String,
  pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportRequirement {
  /// S-prefixed topic id.
  pub topic: String,
  pub description: String,
  /// D-prefixed documentation section topic this requirement was extracted from.
  pub section_topic: String,
  /// D-prefixed documentation topics that informed this requirement.
  pub documentation_topics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportBehavior {
  /// S-prefixed topic id.
  pub topic: String,
  pub description: String,
  /// N-prefixed code member topic this behavior belongs to.
  pub member_topic: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportCharacteristic {
  /// S-prefixed topic id.
  pub topic: String,
  pub description: String,
  pub kind: SystemCharacteristicKind,
  /// D-prefixed documentation section this characteristic was extracted
  /// from. `None` for characteristics whose only source is the raw
  /// `security.md` (no documentation section to anchor to).
  #[serde(skip_serializing_if = "Option::is_none")]
  pub section_topic: Option<String>,
  /// D-prefixed documentation topics that informed this characteristic.
  pub documentation_topics: Vec<String>,
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
  /// Provenance: which workflow variant produced the underlying match.
  /// Optional for backward compatibility with reports written before this
  /// field was added.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub match_source: Option<MatchSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureLink {
  pub feature_topic: String,
  pub topic: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportFunctionalPurpose {
  /// P-prefixed topic id.
  pub topic: String,
  /// Why this subject exists in business terms.
  pub description: String,
  /// The non-pure subject this purpose is on (N-prefixed).
  pub subject_topic: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportPlacementRationale {
  /// P-prefixed topic id.
  pub topic: String,
  /// Why this subject is at this point in its containing function.
  pub description: String,
  /// The non-pure subject this placement rationale is on (N-prefixed).
  pub subject_topic: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportCondition {
  /// A-prefixed topic id.
  pub topic: String,
  /// The assertion, phrased affirmatively.
  pub description: String,
  /// The non-pure subject whose purpose+placement this assertion supports
  /// (N-prefixed).
  pub subject_topic: String,
  /// Category of assertion.
  pub kind: crate::domain::ConditionKind,
  /// Topic IDs cited as justifying the assertion.
  pub evidence_topics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportThreat {
  /// A-prefixed topic id.
  pub topic: String,
  /// The scenario, phrased actor-agnostically.
  pub description: String,
  /// The non-pure subject this threat belongs to (N-prefixed).
  pub subject_topic: String,
  /// The A-prefixed condition this threat is the adversarial inversion of.
  pub falsifies_condition: String,
  /// The party whose action drives the scenario.
  pub controlled_by: crate::domain::ThreatActor,
  /// Topic IDs cited as the vulnerable code surface.
  pub evidence_topics: Vec<String>,
  /// Severity assigned during impact analysis; None means pending.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub severity: Option<crate::domain::ThreatSeverity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportInvariant {
  /// A-prefixed topic id.
  pub topic: String,
  /// The defensive property ("X must Y" / "every Z does W").
  pub description: String,
  /// The A-prefixed threat this invariant defends against.
  pub threat_topic: String,
  /// The non-pure subject this invariant protects (N-prefixed).
  pub subject_topic: String,
  /// Category of defensive pattern.
  pub kind: crate::domain::InvariantKind,
  /// Declaration topics the property is stated against.
  #[serde(default)]
  pub anchors: Vec<String>,
  /// Severity inherited from parent threat at write time.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub severity: Option<crate::domain::ThreatSeverity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportValidation {
  /// A-prefixed topic id.
  pub topic: String,
  /// The A-prefixed invariant this validation verdicts on.
  pub invariant_topic: String,
  /// The non-pure subject this validation was performed at (N-prefixed).
  pub subject_topic: String,
  /// The verdict.
  pub verdict: crate::domain::ValidationVerdict,
  /// One-sentence justification of the verdict.
  pub rationale: String,
  /// Topic IDs cited as evidence for the verdict.
  pub evidence_topics: Vec<String>,
}

/// A link between a threat and a feature, established during impact
/// analysis. Carries the relationship type and severity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportThreatFeatureLink {
  pub threat_topic: String,
  pub feature_topic: String,
  pub relation: crate::domain::ThreatFeatureRelation,
  pub severity: crate::domain::ThreatSeverity,
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
    characteristics: collect_characteristics(audit_data),
    functional_semantics: collect_functional_semantics(audit_data),
    functional_purposes: collect_functional_purposes(audit_data),
    placement_rationales: collect_placement_rationales(audit_data),
    conditions: collect_conditions(audit_data),
    threats: collect_threats(audit_data),
    invariants: collect_invariants(audit_data),
    validations: collect_validations(audit_data),
    threat_feature_links: collect_threat_feature_links(audit_data),
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

fn collect_characteristics(
  audit_data: &AuditData,
) -> Vec<ReportCharacteristic> {
  let mut out: Vec<ReportCharacteristic> = audit_data
    .topic_metadata
    .values()
    .filter_map(|m| match m {
      TopicMetadata::CharacteristicTopic {
        topic,
        description,
        kind,
        section_topic,
        ..
      } => {
        let documentation_topics = audit_data
          .characteristics
          .get(topic)
          .map(
            |Characteristic {
               documentation_topics,
             }| {
              documentation_topics
                .iter()
                .map(|t| t.id().to_string())
                .collect()
            },
          )
          .unwrap_or_default();
        Some(ReportCharacteristic {
          topic: topic.id().to_string(),
          description: description.clone(),
          kind: *kind,
          section_topic: section_topic.map(|t| t.id().to_string()),
          documentation_topics,
        })
      }
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
        match_source,
        ..
      } => Some(ReportFunctionalSemantic {
        topic: topic.id().to_string(),
        description: description.clone(),
        declaration_topic: declaration_topic.id().to_string(),
        documentation_topics: documentation_topics
          .iter()
          .map(|t| t.id().to_string())
          .collect(),
        match_source: *match_source,
      }),
      _ => None,
    })
    .collect();
  out.sort_by(|a, b| a.topic.cmp(&b.topic));
  out
}

fn flatten_links(
  map: &BTreeMap<crate::domain::topic::Topic, Vec<crate::domain::topic::Topic>>,
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

fn collect_functional_purposes(
  audit_data: &AuditData,
) -> Vec<ReportFunctionalPurpose> {
  let mut out: Vec<ReportFunctionalPurpose> = audit_data
    .topic_metadata
    .values()
    .filter_map(|m| match m {
      TopicMetadata::FunctionalPurposeTopic {
        topic,
        description,
        subject_topic,
        ..
      } => Some(ReportFunctionalPurpose {
        topic: topic.id().to_string(),
        description: description.clone(),
        subject_topic: subject_topic.id().to_string(),
      }),
      _ => None,
    })
    .collect();
  out.sort_by(|a, b| a.topic.cmp(&b.topic));
  out
}

fn collect_placement_rationales(
  audit_data: &AuditData,
) -> Vec<ReportPlacementRationale> {
  let mut out: Vec<ReportPlacementRationale> = audit_data
    .topic_metadata
    .values()
    .filter_map(|m| match m {
      TopicMetadata::PlacementRationaleTopic {
        topic,
        description,
        subject_topic,
        ..
      } => Some(ReportPlacementRationale {
        topic: topic.id().to_string(),
        description: description.clone(),
        subject_topic: subject_topic.id().to_string(),
      }),
      _ => None,
    })
    .collect();
  out.sort_by(|a, b| a.topic.cmp(&b.topic));
  out
}

fn collect_conditions(audit_data: &AuditData) -> Vec<ReportCondition> {
  let mut out: Vec<ReportCondition> = audit_data
    .topic_metadata
    .values()
    .filter_map(|m| match m {
      TopicMetadata::ConditionTopic {
        topic,
        description,
        subject_topic,
        kind,
        evidence_topics,
        ..
      } => Some(ReportCondition {
        topic: topic.id().to_string(),
        description: description.clone(),
        subject_topic: subject_topic.id().to_string(),
        kind: *kind,
        evidence_topics: evidence_topics
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

fn collect_threats(audit_data: &AuditData) -> Vec<ReportThreat> {
  let mut out: Vec<ReportThreat> = audit_data
    .topic_metadata
    .values()
    .filter_map(|m| match m {
      TopicMetadata::ThreatTopic {
        topic,
        description,
        subject_topic,
        falsifies_condition,
        controlled_by,
        evidence_topics,
        severity,
        ..
      } => Some(ReportThreat {
        topic: topic.id().to_string(),
        description: description.clone(),
        subject_topic: subject_topic.id().to_string(),
        falsifies_condition: falsifies_condition.id().to_string(),
        controlled_by: *controlled_by,
        evidence_topics: evidence_topics
          .iter()
          .map(|t| t.id().to_string())
          .collect(),
        severity: *severity,
      }),
      _ => None,
    })
    .collect();
  out.sort_by(|a, b| a.topic.cmp(&b.topic));
  out
}

fn collect_invariants(audit_data: &AuditData) -> Vec<ReportInvariant> {
  let mut out: Vec<ReportInvariant> = audit_data
    .topic_metadata
    .values()
    .filter_map(|m| match m {
      TopicMetadata::InvariantTopic {
        topic,
        description,
        threat_topic,
        subject_topic,
        kind,
        anchors,
        severity,
        ..
      } => Some(ReportInvariant {
        topic: topic.id().to_string(),
        description: description.clone(),
        threat_topic: threat_topic.id().to_string(),
        subject_topic: subject_topic.id().to_string(),
        kind: *kind,
        anchors: anchors.iter().map(|t| t.id().to_string()).collect(),
        severity: *severity,
      }),
      _ => None,
    })
    .collect();
  out.sort_by(|a, b| a.topic.cmp(&b.topic));
  out
}

fn collect_validations(audit_data: &AuditData) -> Vec<ReportValidation> {
  let mut out: Vec<ReportValidation> = audit_data
    .topic_metadata
    .values()
    .filter_map(|m| match m {
      TopicMetadata::ValidationTopic {
        topic,
        invariant_topic,
        subject_topic,
        verdict,
        rationale,
        evidence_topics,
        ..
      } => Some(ReportValidation {
        topic: topic.id().to_string(),
        invariant_topic: invariant_topic.id().to_string(),
        subject_topic: subject_topic.id().to_string(),
        verdict: *verdict,
        rationale: rationale.clone(),
        evidence_topics: evidence_topics
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

fn collect_threat_feature_links(
  audit_data: &AuditData,
) -> Vec<ReportThreatFeatureLink> {
  let mut out: Vec<ReportThreatFeatureLink> = audit_data
    .threat_feature_links
    .iter()
    .map(|link| ReportThreatFeatureLink {
      threat_topic: link.threat_topic.id().to_string(),
      feature_topic: link.feature_topic.id().to_string(),
      relation: link.relation,
      severity: link.severity,
    })
    .collect();
  out.sort_by(|a, b| {
    (a.threat_topic.as_str(), a.feature_topic.as_str())
      .cmp(&(b.threat_topic.as_str(), b.feature_topic.as_str()))
  });
  out
}

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
/// requirements, behaviors, characteristics, functional semantics,
/// functional purposes, placement rationales, conditions, threats, invariants,
/// validations) and their links. Also reseeds every per-prefix ID counter
/// (`S`, `P`, `A`) past the highest ID of that variant in the merged
/// `topic_metadata`, so subsequent in-memory allocations (user-entity
/// hydration, comment authoring, pipeline rerun) never collide with anything
/// the report installed.
///
/// Callers should invoke `crate::domain::rebuild_feature_context` on the audit
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

  use crate::domain::topic;

  // Drop any stale pipeline-topic metadata before hydrating from the
  // report. All pipeline-output TopicMetadata variants are stripped so
  // the report is the sole source of truth for the full pipeline state.
  audit_data.topic_metadata.retain(|_, m| {
    !matches!(
      m,
      TopicMetadata::FeatureTopic { .. }
        | TopicMetadata::RequirementTopic { .. }
        | TopicMetadata::BehaviorTopic { .. }
        | TopicMetadata::CharacteristicTopic { .. }
        | TopicMetadata::FunctionalSemanticTopic { .. }
        | TopicMetadata::FunctionalPurposeTopic { .. }
        | TopicMetadata::PlacementRationaleTopic { .. }
        | TopicMetadata::ConditionTopic { .. }
        | TopicMetadata::ThreatTopic { .. }
        | TopicMetadata::InvariantTopic { .. }
        | TopicMetadata::ValidationTopic { .. }
    )
  });
  audit_data.requirements.clear();
  audit_data.characteristics.clear();
  audit_data.feature_requirement_links.clear();
  audit_data.feature_behavior_links.clear();
  audit_data.threat_feature_links.clear();

  for f in &report.pipeline.features {
    let topic = topic::new_topic(&f.topic);
    audit_data.topic_metadata.insert(
      topic,
      TopicMetadata::FeatureTopic {
        topic,
        name: f.name.clone(),
        description: f.description.clone(),
        author: Author::System,
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
        author: Author::System,
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
        author: Author::System,
        created_at: None,
      },
    );
  }

  for c in &report.pipeline.characteristics {
    let topic = topic::new_topic(&c.topic);
    let section_topic = c.section_topic.as_deref().map(topic::new_topic);
    let documentation_topics: Vec<_> = c
      .documentation_topics
      .iter()
      .map(|id| topic::new_topic(id))
      .collect();

    audit_data.characteristics.insert(
      topic,
      Characteristic {
        documentation_topics,
      },
    );

    audit_data.topic_metadata.insert(
      topic,
      TopicMetadata::CharacteristicTopic {
        topic,
        description: c.description.clone(),
        kind: c.kind,
        section_topic,
        author: Author::System,
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
        author: Author::System,
        created_at: None,
        match_source: s.match_source,
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

  for p in &report.pipeline.functional_purposes {
    let topic = topic::new_topic(&p.topic);
    let subject_topic = topic::new_topic(&p.subject_topic);
    audit_data.topic_metadata.insert(
      topic,
      TopicMetadata::FunctionalPurposeTopic {
        topic,
        description: p.description.clone(),
        subject_topic,
        author: Author::System,
        created_at: None,
      },
    );
  }

  for pr in &report.pipeline.placement_rationales {
    let topic = topic::new_topic(&pr.topic);
    let subject_topic = topic::new_topic(&pr.subject_topic);
    audit_data.topic_metadata.insert(
      topic,
      TopicMetadata::PlacementRationaleTopic {
        topic,
        description: pr.description.clone(),
        subject_topic,
        author: Author::System,
        created_at: None,
      },
    );
  }

  for c in &report.pipeline.conditions {
    let topic = topic::new_topic(&c.topic);
    let subject_topic = topic::new_topic(&c.subject_topic);
    let evidence_topics = c
      .evidence_topics
      .iter()
      .map(|s| topic::new_topic(s))
      .collect();
    audit_data.topic_metadata.insert(
      topic,
      TopicMetadata::ConditionTopic {
        topic,
        description: c.description.clone(),
        subject_topic,
        kind: c.kind,
        evidence_topics,
        author: Author::System,
        created_at: None,
      },
    );
  }

  for t in &report.pipeline.threats {
    let topic = topic::new_topic(&t.topic);
    let subject_topic = topic::new_topic(&t.subject_topic);
    let falsifies_condition = topic::new_topic(&t.falsifies_condition);
    let evidence_topics = t
      .evidence_topics
      .iter()
      .map(|s| topic::new_topic(s))
      .collect();
    audit_data.topic_metadata.insert(
      topic,
      TopicMetadata::ThreatTopic {
        topic,
        description: t.description.clone(),
        subject_topic,
        falsifies_condition,
        controlled_by: t.controlled_by,
        evidence_topics,
        author: Author::System,
        created_at: None,
        severity: t.severity,
      },
    );
  }

  for inv in &report.pipeline.invariants {
    let topic = topic::new_topic(&inv.topic);
    let threat_topic = topic::new_topic(&inv.threat_topic);
    let subject_topic = topic::new_topic(&inv.subject_topic);
    let anchors = inv.anchors.iter().map(|s| topic::new_topic(s)).collect();
    audit_data.topic_metadata.insert(
      topic,
      TopicMetadata::InvariantTopic {
        topic,
        description: inv.description.clone(),
        threat_topic,
        subject_topic,
        kind: inv.kind,
        anchors,
        author: Author::System,
        created_at: None,
        severity: inv.severity,
      },
    );
  }

  for v in &report.pipeline.validations {
    let topic = topic::new_topic(&v.topic);
    let invariant_topic = topic::new_topic(&v.invariant_topic);
    let subject_topic = topic::new_topic(&v.subject_topic);
    let evidence_topics = v
      .evidence_topics
      .iter()
      .map(|s| topic::new_topic(s))
      .collect();
    audit_data.topic_metadata.insert(
      topic,
      TopicMetadata::ValidationTopic {
        topic,
        invariant_topic,
        subject_topic,
        verdict: v.verdict,
        rationale: v.rationale.clone(),
        evidence_topics,
        author: Author::System,
        created_at: None,
      },
    );
  }

  for link in &report.pipeline.threat_feature_links {
    audit_data.threat_feature_links.push(ThreatFeatureLink {
      threat_topic: topic::new_topic(&link.threat_topic),
      feature_topic: topic::new_topic(&link.feature_topic),
      relation: link.relation,
      severity: link.severity,
    });
  }

  // Reseed every per-prefix counter so subsequent allocations skip past
  // every topic this report installed and every topic `apply_snapshot`
  // hydrated earlier in the startup sequence.
  //
  // - `S` (spec): Feature/Requirement/Behavior/Characteristic all key by
  //   `Topic::Spec`. Apply_report just reinserted them; scanning
  //   `topic_metadata` keys covers every spec ID.
  // - `P` (functional property): FunctionalSemantic, FunctionalPurpose,
  //   PlacementRationale all key by `Topic::FunctionalProperty`.
  //   Apply_report just reinserted them.
  // - `A` (adversarial property): Condition/Threat/Invariant/Validation
  //   all key by `Topic::AdversarialProperty`. Apply_report just
  //   reinserted them.
  //
  // Each scan is O(n) over `topic_metadata` and runs at startup only.
  crate::ids::reseed_spec_id(max_spec_id(audit_data));
  crate::ids::reseed_functional_property_id(max_functional_property_id(
    audit_data,
  ));
  crate::ids::reseed_adversarial_property_id(max_adversarial_property_id(
    audit_data,
  ));

  Ok(())
}

/// Highest numeric ID across every `Topic::Spec` key in `topic_metadata`,
/// or 0 if there are none. Used to bound the spec-counter reseed after
/// hydration paths that mutate `topic_metadata`.
fn max_spec_id(audit_data: &AuditData) -> i32 {
  audit_data
    .topic_metadata
    .keys()
    .filter_map(|t| match t {
      crate::domain::topic::Topic::Spec(id) => Some(*id),
      _ => None,
    })
    .max()
    .unwrap_or(0)
}

/// Highest numeric ID across every `Topic::FunctionalProperty` key in
/// `topic_metadata`, or 0 if there are none. Peer of `max_spec_id`; used
/// to bound the functional-property-counter reseed.
fn max_functional_property_id(audit_data: &AuditData) -> i32 {
  audit_data
    .topic_metadata
    .keys()
    .filter_map(|t| match t {
      crate::domain::topic::Topic::FunctionalProperty(id) => Some(*id),
      _ => None,
    })
    .max()
    .unwrap_or(0)
}

/// Highest numeric ID across every `Topic::AdversarialProperty` key in
/// `topic_metadata`, or 0 if there are none. Peer of `max_spec_id`; used
/// to bound the adversarial-property-counter reseed.
fn max_adversarial_property_id(audit_data: &AuditData) -> i32 {
  audit_data
    .topic_metadata
    .keys()
    .filter_map(|t| match t {
      crate::domain::topic::Topic::AdversarialProperty(id) => Some(*id),
      _ => None,
    })
    .max()
    .unwrap_or(0)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::domain::{ProjectPath, new_audit_data};
  use std::collections::HashSet;

  fn empty_audit() -> AuditData {
    new_audit_data("test".to_string(), HashSet::<ProjectPath>::new(), None)
  }

  fn report_with_pipeline(pipeline: PipelineOutput) -> AuditReport {
    AuditReport {
      schema_version: SCHEMA_VERSION,
      generator: GeneratorInfo {
        name: "test".to_string(),
        version: "0".to_string(),
      },
      generated_at: "2026-05-12T00:00:00Z".to_string(),
      audit: AuditMetadata {
        id: "audit-1".to_string(),
        name: "Test".to_string(),
        in_scope_files: vec![],
        security_notes: None,
      },
      pipeline,
    }
  }

  fn empty_pipeline() -> PipelineOutput {
    PipelineOutput {
      features: vec![],
      requirements: vec![],
      behaviors: vec![],
      characteristics: vec![],
      functional_semantics: vec![],
      functional_purposes: vec![],
      placement_rationales: vec![],
      conditions: vec![],
      threats: vec![],
      invariants: vec![],
      validations: vec![],
      threat_feature_links: vec![],
      feature_requirement_links: vec![],
      feature_behavior_links: vec![],
    }
  }

  /// Acquire every per-prefix counter lock the `apply_report` reseed
  /// touches, in a stable order. Returns the guards so the caller can
  /// drop them together at test end. Stable acquisition order avoids
  /// the cross-test deadlock that would otherwise be possible (e.g.
  /// test A grabs S then P, test B grabs P then S).
  fn lock_all_counters() -> (
    std::sync::MutexGuard<'static, ()>,
    std::sync::MutexGuard<'static, ()>,
    std::sync::MutexGuard<'static, ()>,
  ) {
    let spec = crate::ids::SPEC_LOCK.lock().unwrap();
    let functional = crate::ids::FUNCTIONAL_PROPERTY_LOCK.lock().unwrap();
    let adversarial = crate::ids::ADVERSARIAL_PROPERTY_LOCK.lock().unwrap();
    (spec, functional, adversarial)
  }

  #[test]
  fn apply_report_reseeds_spec_counter_past_highest_pipeline_topic() {
    // `apply_report` reseeds all three counters; hold every lock so
    // parallel tests in `ids` don't race on the side-effected counters.
    let _guards = lock_all_counters();

    // Bottom out the counter so allocation after apply_report observably
    // skips past the report's max — proving the reseed fired.
    crate::ids::reseed_spec_id(0);

    let mut pipeline = empty_pipeline();
    // Spread the max across the four pipeline-output kinds so the test
    // covers all of them, not just the one that happens to hold the max.
    pipeline.features.push(ReportFeature {
      topic: "S3".to_string(),
      name: "f3".to_string(),
      description: "f3 desc".to_string(),
    });
    pipeline.requirements.push(ReportRequirement {
      topic: "S7".to_string(),
      description: "r7".to_string(),
      section_topic: "D1".to_string(),
      documentation_topics: vec!["D1".to_string()],
    });
    pipeline.behaviors.push(ReportBehavior {
      topic: "S11".to_string(),
      description: "b11".to_string(),
      member_topic: "N1".to_string(),
    });
    pipeline.characteristics.push(ReportCharacteristic {
      topic: "S42".to_string(),
      description: "security claim".to_string(),
      kind: crate::domain::SystemCharacteristicKind::Security,
      section_topic: Some("D1".to_string()),
      documentation_topics: vec!["D1".to_string()],
    });

    let mut audit = empty_audit();
    apply_report("audit-1", &mut audit, &report_with_pipeline(pipeline))
      .expect("apply_report");

    // Next allocation must skip past the highest spec ID the report
    // installed (S42).
    let next = crate::ids::allocate_spec_id();
    assert_eq!(
      next, 43,
      "spec counter must reseed past max pipeline spec id"
    );
  }

  #[test]
  fn apply_report_with_no_spec_topics_reseeds_counter_to_one() {
    let _guards = lock_all_counters();

    crate::ids::reseed_spec_id(0);
    let mut audit = empty_audit();
    apply_report(
      "audit-1",
      &mut audit,
      &report_with_pipeline(empty_pipeline()),
    )
    .expect("apply_report");

    // No spec topics → max is 0 → reseed(0) → next allocation is 1.
    assert_eq!(crate::ids::allocate_spec_id(), 1);
  }

  #[test]
  fn apply_report_reseeds_functional_property_counter_past_pipeline_semantic() {
    // Functional semantics in the report key by `Topic::FunctionalProperty`.
    // Without the P-counter reseed wired into apply_report, user-entity
    // hydration (which runs next in startup) would collide on these IDs.
    let _guards = lock_all_counters();

    crate::ids::reseed_functional_property_id(0);

    let mut pipeline = empty_pipeline();
    pipeline
      .functional_semantics
      .push(ReportFunctionalSemantic {
        topic: "P57".to_string(),
        description: "what this declaration means".to_string(),
        declaration_topic: "N9".to_string(),
        documentation_topics: vec!["D2".to_string()],
        match_source: None,
      });

    let mut audit = empty_audit();
    apply_report("audit-1", &mut audit, &report_with_pipeline(pipeline))
      .expect("apply_report");

    let next = crate::ids::allocate_functional_property_id();
    assert_eq!(
      next, 58,
      "functional-property counter must reseed past max pipeline P-id"
    );
  }

  #[test]
  fn apply_report_reseeds_adversarial_property_counter_past_report_topic() {
    // Adversarial-property topics now come from the report (not the
    // snapshot). The retain filter clears any stale A-prefixed entries
    // before hydration, so the reseed sees only what the report installed.
    let _guards = lock_all_counters();

    crate::ids::reseed_adversarial_property_id(0);

    let mut pipeline = empty_pipeline();
    pipeline.invariants.push(ReportInvariant {
      topic: "A73".to_string(),
      description: "every privileged setter checks ownership".to_string(),
      threat_topic: "A5".to_string(),
      subject_topic: "N10".to_string(),
      kind: crate::domain::InvariantKind::AccessGate,
      anchors: vec![],
      severity: None,
    });
    // Also add the parent threat so the invariant has a valid chain.
    pipeline.threats.push(ReportThreat {
      topic: "A5".to_string(),
      description: "unauthorized access".to_string(),
      subject_topic: "N10".to_string(),
      falsifies_condition: "A3".to_string(),
      controlled_by: crate::domain::ThreatActor::Caller,
      evidence_topics: vec![],
      severity: None,
    });

    let mut audit = empty_audit();
    apply_report("audit-1", &mut audit, &report_with_pipeline(pipeline))
      .expect("apply_report");

    let next = crate::ids::allocate_adversarial_property_id();
    assert_eq!(
      next, 74,
      "adversarial-property counter must reseed past max A-id in report"
    );
  }

  #[test]
  fn apply_report_with_no_p_or_a_topics_reseeds_counters_to_one() {
    // Empty audit + empty report → both P and A counters reseed to 0,
    // next allocation is 1 on each. Matches the equivalent S-counter
    // test above.
    let _guards = lock_all_counters();

    crate::ids::reseed_functional_property_id(0);
    crate::ids::reseed_adversarial_property_id(0);
    let mut audit = empty_audit();
    apply_report(
      "audit-1",
      &mut audit,
      &report_with_pipeline(empty_pipeline()),
    )
    .expect("apply_report");

    assert_eq!(crate::ids::allocate_functional_property_id(), 1);
    assert_eq!(crate::ids::allocate_adversarial_property_id(), 1);
  }
}
