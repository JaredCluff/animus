use animus_core::{Content, InstanceId, PolicyId, Principal, Segment, SegmentId, Source};
use animus_federation::knowledge::{
    FederationPermission, FederationPolicy, FederationRule, FederationScope, KnowledgeSharing,
};
use animus_federation::protocol::{ContentKind, SegmentAnnouncement};
use chrono::Utc;
use std::collections::HashMap;

#[test]
fn publish_check_allows_matching_policy() {
    let policies = vec![FederationPolicy {
        id: PolicyId::new(),
        name: "share-text".to_string(),
        active: true,
        publish_rules: vec![FederationRule {
            scope: FederationScope::AllNonPrivate,
            permission: FederationPermission::Allow,
        }],
        subscribe_rules: vec![],
    }];

    let sharing = KnowledgeSharing::new(policies, 0.5);
    let segment = Segment::new(
        Content::Text("hello world".to_string()),
        vec![0.1, 0.2, 0.3],
        Source::Manual { description: "test".to_string() },
    );
    let target = InstanceId::new();
    assert!(sharing.can_publish(&segment, &target));
}

#[test]
fn publish_check_denies_when_no_policy() {
    let sharing = KnowledgeSharing::new(vec![], 0.5);
    let segment = Segment::new(
        Content::Text("hello".to_string()),
        vec![0.1],
        Source::Manual { description: "test".to_string() },
    );
    let target = InstanceId::new();
    assert!(!sharing.can_publish(&segment, &target));
}

#[test]
fn publish_check_denies_private_segment() {
    let policies = vec![FederationPolicy {
        id: PolicyId::new(),
        name: "share-all".to_string(),
        active: true,
        publish_rules: vec![FederationRule {
            scope: FederationScope::AllNonPrivate,
            permission: FederationPermission::Allow,
        }],
        subscribe_rules: vec![],
    }];

    let sharing = KnowledgeSharing::new(policies, 0.5);
    let mut segment = Segment::new(
        Content::Text("secret".to_string()),
        vec![0.1],
        Source::Manual { description: "test".to_string() },
    );
    // Segment has specific observability — only a human
    segment.observable_by.push(Principal::Human("alice".to_string()));
    let target = InstanceId::new();
    // AllNonPrivate should deny because observable_by is non-empty and doesn't include the target
    assert!(!sharing.can_publish(&segment, &target));
}

#[test]
fn publish_check_allows_when_target_in_observable_by() {
    let target = InstanceId::new();
    let sharing = KnowledgeSharing::new(vec![], 0.5); // no policies needed

    let mut segment = Segment::new(
        Content::Text("for you".to_string()),
        vec![0.1],
        Source::Manual { description: "test".to_string() },
    );
    segment.observable_by.push(Principal::Ailf(target));
    assert!(sharing.can_publish(&segment, &target));
}

#[test]
fn relevance_check_passes_similar_embedding() {
    let sharing = KnowledgeSharing::new(vec![], 0.5);
    let ann = SegmentAnnouncement {
        segment_id: SegmentId::new(),
        embedding: vec![1.0, 0.0, 0.0],
        content_kind: ContentKind::Text,
        created: Utc::now(),
        tags: HashMap::new(),
    };
    let goal_embeddings = vec![vec![0.9, 0.1, 0.0]]; // very similar
    assert!(sharing.is_relevant(&ann, &goal_embeddings));
}

#[test]
fn relevance_check_rejects_dissimilar_embedding() {
    let sharing = KnowledgeSharing::new(vec![], 0.8); // high threshold
    let ann = SegmentAnnouncement {
        segment_id: SegmentId::new(),
        embedding: vec![1.0, 0.0, 0.0],
        content_kind: ContentKind::Text,
        created: Utc::now(),
        tags: HashMap::new(),
    };
    let goal_embeddings = vec![vec![0.0, 1.0, 0.0]]; // orthogonal
    assert!(!sharing.is_relevant(&ann, &goal_embeddings));
}

#[test]
fn inactive_policy_is_skipped() {
    let policies = vec![FederationPolicy {
        id: PolicyId::new(),
        name: "inactive".to_string(),
        active: false,
        publish_rules: vec![FederationRule {
            scope: FederationScope::AllNonPrivate,
            permission: FederationPermission::Allow,
        }],
        subscribe_rules: vec![],
    }];

    let sharing = KnowledgeSharing::new(policies, 0.5);
    let segment = Segment::new(
        Content::Text("hello".to_string()),
        vec![0.1],
        Source::Manual { description: "test".to_string() },
    );
    let target = InstanceId::new();
    assert!(!sharing.can_publish(&segment, &target));
}
