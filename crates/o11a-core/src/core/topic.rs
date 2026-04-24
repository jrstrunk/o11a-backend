use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopicKind {
  Node,
  Documentation,
  Comment,
  Invariant,
  AttackVector,
  Feature,
  Requirement,
  Behavior,
  FunctionalProperty,
  TypeConstraint,
}

#[derive(
  Debug, Clone, PartialEq, PartialOrd, Eq, Ord, Hash, Serialize, Deserialize,
)]
pub struct Topic {
  pub id: String,
}

impl Topic {
  pub fn id(&self) -> &str {
    &self.id
  }

  /// Returns the kind of this topic based on its prefix, or `None` for
  /// ad-hoc topics created via `new_topic`.
  pub fn kind(&self) -> Option<TopicKind> {
    match self.id.as_bytes().first() {
      Some(b'N') => Some(TopicKind::Node),
      Some(b'D') => Some(TopicKind::Documentation),
      Some(b'C') => Some(TopicKind::Comment),
      Some(b'I') => Some(TopicKind::Invariant),
      Some(b'T') => Some(TopicKind::AttackVector),
      Some(b'F') => Some(TopicKind::Feature),
      Some(b'R') => Some(TopicKind::Requirement),
      Some(b'B') => Some(TopicKind::Behavior),
      Some(b'P') => Some(TopicKind::FunctionalProperty),
      Some(b'Y') => Some(TopicKind::TypeConstraint),
      _ => None,
    }
  }

  /// Extracts the numeric suffix of this topic ID, regardless of kind.
  pub fn numeric_id(&self) -> Option<i64> {
    if self.id.len() > 1 && self.kind().is_some() {
      self.id[1..].parse::<i64>().ok()
    } else {
      None
    }
  }

  /// Extracts the numeric ID as i32. Kept for compatibility with the
  /// many solidity analyzer call sites that expect `Result<i32, ()>`.
  pub fn underlying_id(&self) -> Result<i32, ()> {
    self
      .numeric_id()
      .and_then(|id| i32::try_from(id).ok())
      .ok_or(())
  }
}

#[derive(Debug)]
pub struct ParseError {
  pub input: String,
  pub message: String,
}

impl std::fmt::Display for ParseError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "invalid topic ID '{}': {}", self.input, self.message)
  }
}

impl std::error::Error for ParseError {}

/// All path parameters that identify a topic use the prefixed form
/// exclusively (e.g. `F42`, `R7`, `B13`, `P99`, `N-100`, `D34`, `C2`,
/// `I4`, `T1`). Bare numeric IDs are not accepted.
pub fn parse_topic_id(
  input: &str,
  expected_kind: TopicKind,
) -> Result<Topic, ParseError> {
  let topic = Topic {
    id: input.to_string(),
  };
  match topic.kind() {
    Some(kind) if kind == expected_kind => {
      if topic.numeric_id().is_some() {
        Ok(topic)
      } else {
        Err(ParseError {
          input: input.to_string(),
          message: "missing or non-integer numeric suffix".to_string(),
        })
      }
    }
    Some(kind) => Err(ParseError {
      input: input.to_string(),
      message: format!(
        "expected {:?} prefix but got {:?}",
        expected_kind, kind
      ),
    }),
    None => Err(ParseError {
      input: input.to_string(),
      message: format!(
        "missing prefix; expected a {:?} topic in prefixed form",
        expected_kind
      ),
    }),
  }
}

pub fn new_topic(id: &str) -> Topic {
  Topic { id: id.to_string() }
}

pub fn new_node_topic(node_id: &i32) -> Topic {
  Topic {
    id: format!("N{}", node_id),
  }
}

pub fn new_documentation_topic(doc_id: i32) -> Topic {
  Topic {
    id: format!("D{}", doc_id),
  }
}

pub fn new_comment_topic(comment_id: i32) -> Topic {
  Topic {
    id: format!("C{}", comment_id),
  }
}

pub fn new_invariant_topic(invariant_id: i32) -> Topic {
  Topic {
    id: format!("I{}", invariant_id),
  }
}

pub fn new_attack_vector_topic(id: i32) -> Topic {
  Topic {
    id: format!("T{}", id),
  }
}

pub fn new_feature_topic(id: i32) -> Topic {
  Topic {
    id: format!("F{}", id),
  }
}

pub fn new_requirement_topic(id: i32) -> Topic {
  Topic {
    id: format!("R{}", id),
  }
}

pub fn new_behavior_topic(id: i32) -> Topic {
  Topic {
    id: format!("B{}", id),
  }
}

pub fn new_functional_property_topic(id: i32) -> Topic {
  Topic {
    id: format!("P{}", id),
  }
}

pub fn new_type_constraint_topic(id: i32) -> Topic {
  Topic {
    id: format!("Y{}", id),
  }
}
