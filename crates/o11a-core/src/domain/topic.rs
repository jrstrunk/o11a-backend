use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::fmt;
use std::str::FromStr;

/// A topic identifier in the audit graph.
///
/// The wire format is a single prefix character followed by a signed integer,
/// e.g. `"S42"`, `"P99"`, `"N-100"`, `"D34"`, `"C2"`, `"A4"`, `"Y5"`. Clients,
/// the on-disk JSON report, and the DB columns all use this form. The prefix
/// determines the variant; the suffix is stored as an `i32`.
///
/// Prefix map:
/// - `N` → `Node`
/// - `D` → `Documentation`
/// - `C` → `Comment`
/// - `A` → `AdversarialProperty` (shared by `ConditionTopic`,
///   `ThreatTopic`, and `InvariantTopic`)
/// - `S` → `Spec` (shared by `FeatureTopic`, `RequirementTopic`,
///   `BehaviorTopic`, and `CharacteristicTopic` — all four entity kinds
///   in the security-model spec family)
/// - `P` → `FunctionalProperty` (shared by `FunctionalSemanticTopic`,
///   `FunctionalPurposeTopic`, and `PlacementRationaleTopic`)
/// - `Y` → `TypeConstraint` (chosen as the next unused ASCII letter; no
///   existing wire producer emits this prefix yet)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Topic {
  Node(i32),
  Documentation(i32),
  Comment(i32),
  AdversarialProperty(i32),
  Spec(i32),
  FunctionalProperty(i32),
  TypeConstraint(i32),
}

impl Topic {
  /// The single-character prefix identifying this topic's variant.
  pub fn prefix(&self) -> char {
    match self {
      Topic::Node(_) => 'N',
      Topic::Documentation(_) => 'D',
      Topic::Comment(_) => 'C',
      Topic::AdversarialProperty(_) => 'A',
      Topic::Spec(_) => 'S',
      Topic::FunctionalProperty(_) => 'P',
      Topic::TypeConstraint(_) => 'Y',
    }
  }

  /// The numeric suffix of this topic, regardless of variant.
  pub fn numeric_id(&self) -> i32 {
    match self {
      Topic::Node(id)
      | Topic::Documentation(id)
      | Topic::Comment(id)
      | Topic::AdversarialProperty(id)
      | Topic::Spec(id)
      | Topic::FunctionalProperty(id)
      | Topic::TypeConstraint(id) => *id,
    }
  }

  /// The prefixed string form of this topic, e.g. `"S42"` or `"N-100"`.
  /// Equivalent to `format!("{}", topic)`.
  pub fn id(&self) -> String {
    self.to_string()
  }
}

impl fmt::Display for Topic {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}{}", self.prefix(), self.numeric_id())
  }
}

#[derive(Debug)]
pub enum ParseTopicError {
  Empty,
  UnknownPrefix(char),
  InvalidNumericSuffix(String),
}

impl fmt::Display for ParseTopicError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      ParseTopicError::Empty => write!(f, "empty topic ID"),
      ParseTopicError::UnknownPrefix(c) => {
        write!(f, "unknown topic prefix '{}'", c)
      }
      ParseTopicError::InvalidNumericSuffix(s) => {
        write!(f, "invalid numeric suffix '{}'", s)
      }
    }
  }
}

impl std::error::Error for ParseTopicError {}

// Kept for backward compatibility with callers that imported this name.
pub type ParseError = ParseTopicError;

impl FromStr for Topic {
  type Err = ParseTopicError;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    let mut chars = s.chars();
    let prefix = chars.next().ok_or(ParseTopicError::Empty)?;
    let rest = chars.as_str();
    let id: i32 = rest
      .parse()
      .map_err(|_| ParseTopicError::InvalidNumericSuffix(rest.to_string()))?;
    match prefix {
      'N' => Ok(Topic::Node(id)),
      'D' => Ok(Topic::Documentation(id)),
      'C' => Ok(Topic::Comment(id)),
      'A' => Ok(Topic::AdversarialProperty(id)),
      'S' => Ok(Topic::Spec(id)),
      'P' => Ok(Topic::FunctionalProperty(id)),
      'Y' => Ok(Topic::TypeConstraint(id)),
      other => Err(ParseTopicError::UnknownPrefix(other)),
    }
  }
}

impl Serialize for Topic {
  fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(self)
  }
}

impl<'de> Deserialize<'de> for Topic {
  fn deserialize<D: Deserializer<'de>>(
    deserializer: D,
  ) -> Result<Self, D::Error> {
    struct TopicVisitor;

    impl<'de> de::Visitor<'de> for TopicVisitor {
      type Value = Topic;

      fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a prefixed topic ID string like \"S42\"")
      }

      fn visit_str<E: de::Error>(self, v: &str) -> Result<Topic, E> {
        v.parse().map_err(de::Error::custom)
      }

      fn visit_string<E: de::Error>(self, v: String) -> Result<Topic, E> {
        v.parse().map_err(de::Error::custom)
      }
    }

    deserializer.deserialize_str(TopicVisitor)
  }
}

// ---------------------------------------------------------------------------
// Thin constructor helpers. Each is a one-liner; callers may use the variant
// syntax directly (`Topic::Spec(id)`) instead of these helpers.
// ---------------------------------------------------------------------------

pub fn new_node_topic(node_id: &i32) -> Topic {
  Topic::Node(*node_id)
}

pub fn new_documentation_topic(doc_id: i32) -> Topic {
  Topic::Documentation(doc_id)
}

pub fn new_comment_topic(comment_id: i32) -> Topic {
  Topic::Comment(comment_id)
}

pub fn new_adversarial_property_topic(id: i32) -> Topic {
  Topic::AdversarialProperty(id)
}

/// Construct an `S`-prefixed spec topic. The single counter behind this
/// prefix is shared across `FeatureTopic`, `RequirementTopic`,
/// `BehaviorTopic`, and `CharacteristicTopic` — all four entity kinds in
/// the security-model spec family. The kind distinction lives on the
/// corresponding `TopicMetadata` variant, not on the topic itself.
pub fn new_spec_topic(id: i32) -> Topic {
  Topic::Spec(id)
}

pub fn new_functional_property_topic(id: i32) -> Topic {
  Topic::FunctionalProperty(id)
}

pub fn new_type_constraint_topic(id: i32) -> Topic {
  Topic::TypeConstraint(id)
}

/// Checks if value is a topic string, if not returns an error.
pub fn parse_topic(s: &str) -> Result<Topic, ParseTopicError> {
  let topic: Topic = s.parse()?;
  Ok(topic)
}

/// Parse a topic from its prefixed string form. Panics on malformed input.
/// Prefer `str::parse::<Topic>()` when a `Result` is desired.
pub fn new_topic(id: &str) -> Topic {
  id.parse::<Topic>().unwrap_or_else(|e| {
    panic!("invalid topic ID '{}': {}", id, e);
  })
}

macro_rules! define_parse_variant {
  ($name:ident, $variant:ident, $expected:literal) => {
    /// Parse a topic from its prefixed string form and verify it is a
    #[doc = concat!("`Topic::", stringify!($variant), "`.")]
    /// Returns an error on malformed input or a mismatched variant.
    pub fn $name(s: &str) -> Result<Topic, ParseTopicError> {
      let topic: Topic = s.parse()?;
      match topic {
        Topic::$variant(_) => Ok(topic),
        other => Err(ParseTopicError::UnknownPrefix(other.prefix())),
      }
    }
  };
}

define_parse_variant!(parse_node_topic, Node, "N");
define_parse_variant!(parse_documentation_topic, Documentation, "D");
define_parse_variant!(parse_comment_topic, Comment, "C");
define_parse_variant!(
  parse_adversarial_property_topic,
  AdversarialProperty,
  "A"
);
define_parse_variant!(parse_spec_topic, Spec, "S");
define_parse_variant!(parse_functional_property_topic, FunctionalProperty, "P");
define_parse_variant!(parse_type_constraint_topic, TypeConstraint, "Y");

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn display_matches_wire_format() {
    assert_eq!(Topic::Spec(42).to_string(), "S42");
    assert_eq!(Topic::Node(-100).to_string(), "N-100");
    assert_eq!(Topic::Comment(7).to_string(), "C7");
    assert_eq!(Topic::Documentation(34).to_string(), "D34");
    assert_eq!(Topic::AdversarialProperty(4).to_string(), "A4");
    assert_eq!(Topic::AdversarialProperty(1).to_string(), "A1");
    assert_eq!(Topic::Spec(7).to_string(), "S7");
    assert_eq!(Topic::Spec(13).to_string(), "S13");
    assert_eq!(Topic::FunctionalProperty(99).to_string(), "P99");
    assert_eq!(Topic::TypeConstraint(5).to_string(), "Y5");
  }

  #[test]
  fn from_str_parses_wire_format() {
    assert_eq!("S42".parse::<Topic>().unwrap(), Topic::Spec(42));
    assert_eq!("N-100".parse::<Topic>().unwrap(), Topic::Node(-100));
    assert_eq!("Y5".parse::<Topic>().unwrap(), Topic::TypeConstraint(5));
  }

  #[test]
  fn from_str_rejects_bad_input() {
    assert!(matches!("".parse::<Topic>(), Err(ParseTopicError::Empty)));
    assert!(matches!(
      "Xfoo".parse::<Topic>(),
      Err(ParseTopicError::InvalidNumericSuffix(_))
    ));
    assert!(matches!(
      "X5".parse::<Topic>(),
      Err(ParseTopicError::UnknownPrefix('X'))
    ));
    assert!(matches!(
      "S".parse::<Topic>(),
      Err(ParseTopicError::InvalidNumericSuffix(_))
    ));
  }

  #[test]
  fn display_and_from_str_round_trip() {
    for topic in [
      Topic::Node(-100),
      Topic::Documentation(34),
      Topic::Comment(7),
      Topic::AdversarialProperty(4),
      Topic::AdversarialProperty(1),
      Topic::Spec(42),
      Topic::Spec(7),
      Topic::Spec(13),
      Topic::FunctionalProperty(99),
      Topic::TypeConstraint(5),
    ] {
      let encoded = topic.to_string();
      let decoded: Topic = encoded.parse().unwrap();
      assert_eq!(decoded, topic);
    }
  }

  #[test]
  fn serde_json_round_trip() {
    let topic = Topic::Spec(42);
    let json = serde_json::to_string(&topic).unwrap();
    assert_eq!(json, "\"S42\"");
    let back: Topic = serde_json::from_str(&json).unwrap();
    assert_eq!(back, topic);
  }

  #[test]
  fn bincode_round_trip() {
    let topic = Topic::Spec(42);
    let bytes =
      bincode::serde::encode_to_vec(&topic, bincode::config::standard())
        .unwrap();
    let (decoded, _len): (Topic, _) =
      bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
        .unwrap();
    assert_eq!(decoded, topic);

    // Also verify a negative-id Node variant round-trips.
    let node = Topic::Node(-100);
    let bytes =
      bincode::serde::encode_to_vec(&node, bincode::config::standard())
        .unwrap();
    let (decoded, _len): (Topic, _) =
      bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
        .unwrap();
    assert_eq!(decoded, node);
  }
}
