use crate::core::{self, AuditData, topic};

/// Find feature topics for any topic by walking the appropriate chain:
/// - Feature topic: returns itself
/// - Requirement topic: reverse-lookups feature_requirement_links
/// - Behavior topic: reverse-lookups feature_behavior_links
/// - Code topic: walks to containing member → behaviors → feature_behavior_links
pub fn features_for_topic(
  t: &topic::Topic,
  audit_data: &AuditData,
) -> Vec<topic::Topic> {
  let mut features = Vec::new();

  match t.kind() {
    Some(topic::TopicKind::Feature) => {
      if matches!(
        audit_data.topic_metadata.get(t),
        Some(core::TopicMetadata::FeatureTopic { .. })
      ) {
        features.push(t.clone());
      }
      return features;
    }
    Some(topic::TopicKind::Requirement) => {
      for (ft, req_topics) in &audit_data.feature_requirement_links {
        if req_topics.contains(t) && !features.contains(ft) {
          features.push(ft.clone());
        }
      }
      return features;
    }
    Some(topic::TopicKind::Behavior) => {
      for (ft, beh_topics) in &audit_data.feature_behavior_links {
        if beh_topics.contains(t) && !features.contains(ft) {
          features.push(ft.clone());
        }
      }
      return features;
    }
    _ => {}
  }

  // Code topic: determine the member topic (self if already a member, or walk up)
  let member_topic = if let Some(metadata) = audit_data.topic_metadata.get(t) {
    match metadata {
      core::TopicMetadata::NamedTopic {
        kind: core::NamedTopicKind::Function(_) | core::NamedTopicKind::Modifier,
        ..
      } => Some(t.clone()),
      _ => match metadata.scope() {
        core::Scope::Member { member, .. }
        | core::Scope::ContainingBlock { member, .. } => Some(member.clone()),
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
      if let Some(core::TopicMetadata::BehaviorTopic {
        member_topic: bmt, ..
      }) = audit_data.topic_metadata.get(bt)
        && *bmt == member_topic
        && !features.contains(ft)
      {
        features.push(ft.clone());
      }
    }
  }

  features
}
