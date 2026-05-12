use crate::domain::{self, AuditData, topic};

/// Find feature topics for any topic by walking the appropriate chain.
/// Spec topics (`S`) cover four entity kinds; the kind distinction comes
/// from the corresponding `TopicMetadata` variant:
/// - Feature: returns itself
/// - Requirement: reverse-lookups `feature_requirement_links`
/// - Behavior: reverse-lookups `feature_behavior_links`
/// - Characteristic: never linked to a feature, returns nothing
///
/// For a code topic (`N`), walks to containing member → behaviors →
/// `feature_behavior_links`.
pub fn features_for_topic(
  t: &topic::Topic,
  audit_data: &AuditData,
) -> Vec<topic::Topic> {
  let mut features = Vec::new();

  if matches!(t, topic::Topic::Spec(_)) {
    match audit_data.topic_metadata.get(t) {
      Some(domain::TopicMetadata::FeatureTopic { .. }) => {
        features.push(*t);
        return features;
      }
      Some(domain::TopicMetadata::RequirementTopic { .. }) => {
        for (ft, req_topics) in &audit_data.feature_requirement_links {
          if req_topics.contains(t) && !features.contains(ft) {
            features.push(*ft);
          }
        }
        return features;
      }
      Some(domain::TopicMetadata::BehaviorTopic { .. }) => {
        for (ft, beh_topics) in &audit_data.feature_behavior_links {
          if beh_topics.contains(t) && !features.contains(ft) {
            features.push(*ft);
          }
        }
        return features;
      }
      _ => return features,
    }
  }

  // Code topic: determine the member topic (self if already a member, or walk up)
  let member_topic = if let Some(metadata) = audit_data.topic_metadata.get(t) {
    match metadata {
      domain::TopicMetadata::NamedTopic {
        kind:
          domain::NamedTopicKind::Function(_) | domain::NamedTopicKind::Modifier,
        ..
      } => Some(*t),
      _ => match metadata.scope() {
        domain::Scope::Member { member, .. }
        | domain::Scope::ContainingBlock { member, .. } => Some(*member),
        _ => None,
      },
    }
  } else {
    None
  };

  let member_topic = match member_topic {
    Some(mt) => mt,
    None => return features,
  };

  // Find features via behaviors for this member
  for (ft, beh_topics) in &audit_data.feature_behavior_links {
    for bt in beh_topics {
      if let Some(domain::TopicMetadata::BehaviorTopic {
        member_topic: bmt,
        ..
      }) = audit_data.topic_metadata.get(bt)
        && *bmt == member_topic
        && !features.contains(ft)
      {
        features.push(*ft);
      }
    }
  }

  features
}
