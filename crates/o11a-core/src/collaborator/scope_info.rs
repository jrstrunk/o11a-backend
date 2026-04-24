//! Serializable projections of `core::Scope` for on-disk persistence (comment
//! rows) and for HTTP responses. Lives in core because the collaborator DB
//! writes comment.scope as JSON using this shape; HTTP layers reuse it.

use crate::core::{self, topic::new_topic};
use serde::{Deserialize, Serialize};

/// Serializable block annotation kind for API responses and DB persistence.
/// Flattens `BlockAnnotationKind::If(ControlFlowBranch)` into
/// `if_true`/`if_false` for a clean single-discriminator JSON representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockAnnotationKindInfo {
  #[serde(rename = "if_true")]
  IfTrue,
  #[serde(rename = "if_false")]
  IfFalse,
  For,
  While,
  DoWhile,
  Unchecked,
  InlineAssembly,
}

impl BlockAnnotationKindInfo {
  pub fn from_core(kind: &core::BlockAnnotationKind) -> Self {
    match kind {
      core::BlockAnnotationKind::If(core::ControlFlowBranch::True) => {
        Self::IfTrue
      }
      core::BlockAnnotationKind::If(core::ControlFlowBranch::False) => {
        Self::IfFalse
      }
      core::BlockAnnotationKind::For => Self::For,
      core::BlockAnnotationKind::While => Self::While,
      core::BlockAnnotationKind::DoWhile => Self::DoWhile,
      core::BlockAnnotationKind::Unchecked => Self::Unchecked,
      core::BlockAnnotationKind::InlineAssembly => Self::InlineAssembly,
    }
  }

  pub fn to_core(&self) -> core::BlockAnnotationKind {
    match self {
      Self::IfTrue => {
        core::BlockAnnotationKind::If(core::ControlFlowBranch::True)
      }
      Self::IfFalse => {
        core::BlockAnnotationKind::If(core::ControlFlowBranch::False)
      }
      Self::For => core::BlockAnnotationKind::For,
      Self::While => core::BlockAnnotationKind::While,
      Self::DoWhile => core::BlockAnnotationKind::DoWhile,
      Self::Unchecked => core::BlockAnnotationKind::Unchecked,
      Self::InlineAssembly => core::BlockAnnotationKind::InlineAssembly,
    }
  }
}

/// Serializable block annotation for API responses and DB persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockAnnotationResponse {
  pub topic: String,
  pub kind: BlockAnnotationKindInfo,
}

/// One layer in the containing-block nesting chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainingBlockLayerInfo {
  pub block: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub annotation: Option<BlockAnnotationResponse>,
}

/// Serializable scope information for comment DB persistence and API responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeInfo {
  pub scope_type: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub container: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub component: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub member: Option<String>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub containing_blocks: Vec<ContainingBlockLayerInfo>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub signature_container: Option<String>,
}

impl ScopeInfo {
  /// Convert from core::Scope to ScopeInfo
  pub fn from_scope(scope: &core::Scope) -> Self {
    match scope {
      core::Scope::Global => ScopeInfo {
        scope_type: "Global".to_string(),
        container: None,
        component: None,
        member: None,
        containing_blocks: vec![],
        signature_container: None,
      },
      core::Scope::Container { container } => ScopeInfo {
        scope_type: "Container".to_string(),
        container: Some(container.file_path.clone()),
        component: None,
        member: None,
        containing_blocks: vec![],
        signature_container: None,
      },
      core::Scope::Component {
        container,
        component,
      } => ScopeInfo {
        scope_type: "Component".to_string(),
        container: Some(container.file_path.clone()),
        component: Some(component.id.clone()),
        member: None,
        containing_blocks: vec![],
        signature_container: None,
      },
      core::Scope::Member {
        container,
        component,
        member,
        signature_container,
      } => ScopeInfo {
        scope_type: "Member".to_string(),
        container: Some(container.file_path.clone()),
        component: Some(component.id.clone()),
        member: Some(member.id.clone()),
        containing_blocks: vec![],
        signature_container: signature_container.as_ref().map(|t| t.id.clone()),
      },
      core::Scope::ContainingBlock {
        container,
        component,
        member,
        containing_blocks,
      } => ScopeInfo {
        scope_type: "ContainingBlock".to_string(),
        container: Some(container.file_path.clone()),
        component: Some(component.id.clone()),
        member: Some(member.id.clone()),
        containing_blocks: containing_blocks
          .iter()
          .map(|layer| ContainingBlockLayerInfo {
            block: layer.block.id.clone(),
            annotation: layer.annotation.as_ref().map(|ann| {
              BlockAnnotationResponse {
                topic: ann.topic.id.clone(),
                kind: BlockAnnotationKindInfo::from_core(&ann.kind),
              }
            }),
          })
          .collect(),
        signature_container: None,
      },
    }
  }

  /// Get the scope from a topic's metadata, or return Global scope if not found
  pub fn from_topic(topic_id: &str, audit_data: &core::AuditData) -> Self {
    let topic = new_topic(topic_id);
    if let Some(metadata) = audit_data.topic_metadata.get(&topic) {
      Self::from_scope(metadata.scope())
    } else {
      Self::default()
    }
  }

  /// Returns the lowest (most specific) scope topic ID.
  /// Returns innermost containing_block > member > component > None for Container/Global.
  pub fn lowest_scope_topic_id(&self) -> Option<&str> {
    self
      .containing_blocks
      .last()
      .map(|l| l.block.as_str())
      .or(self.member.as_deref())
      .or(self.component.as_deref())
  }

  /// Convert from ScopeInfo back to core::Scope
  pub fn to_scope(&self) -> core::Scope {
    let container = || core::ProjectPath {
      file_path: self.container.clone().unwrap(),
    };
    match self.scope_type.as_str() {
      "ContainingBlock" => core::Scope::ContainingBlock {
        container: container(),
        component: new_topic(self.component.as_ref().unwrap()),
        member: new_topic(self.member.as_ref().unwrap()),
        containing_blocks: self
          .containing_blocks
          .iter()
          .map(|layer| core::ContainingBlockLayer {
            block: new_topic(&layer.block),
            annotation: layer.annotation.as_ref().map(|ann| {
              core::BlockAnnotation {
                topic: new_topic(&ann.topic),
                kind: ann.kind.to_core(),
              }
            }),
          })
          .collect(),
      },
      "Member" => core::Scope::Member {
        container: container(),
        component: new_topic(self.component.as_ref().unwrap()),
        member: new_topic(self.member.as_ref().unwrap()),
        signature_container: self
          .signature_container
          .as_ref()
          .map(|s| new_topic(s)),
      },
      "Component" => core::Scope::Component {
        container: container(),
        component: new_topic(self.component.as_ref().unwrap()),
      },
      "Container" => core::Scope::Container {
        container: container(),
      },
      _ => core::Scope::Global,
    }
  }
}

impl Default for ScopeInfo {
  fn default() -> Self {
    ScopeInfo {
      scope_type: "Global".to_string(),
      container: None,
      component: None,
      member: None,
      containing_blocks: vec![],
      signature_container: None,
    }
  }
}
