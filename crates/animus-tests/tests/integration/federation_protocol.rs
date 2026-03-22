use animus_core::{GoalId, InstanceId, SegmentId};
use animus_federation::protocol::*;
use chrono::Utc;
use std::collections::HashMap;

#[test]
fn content_kind_serialization_roundtrip() {
    for kind in [ContentKind::Text, ContentKind::Structured, ContentKind::Binary, ContentKind::Reference] {
        let json = serde_json::to_string(&kind).unwrap();
        let back: ContentKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, back);
    }
}

#[test]
fn segment_announcement_roundtrip() {
    let ann = SegmentAnnouncement {
        segment_id: SegmentId::new(),
        embedding: vec![0.1, 0.2, 0.3],
        content_kind: ContentKind::Text,
        created: Utc::now(),
        tags: HashMap::from([("topic".to_string(), "rust".to_string())]),
    };
    let json = serde_json::to_string(&ann).unwrap();
    let back: SegmentAnnouncement = serde_json::from_str(&json).unwrap();
    assert_eq!(back.segment_id, ann.segment_id);
    assert_eq!(back.content_kind, ContentKind::Text);
}

#[test]
fn handshake_request_roundtrip() {
    let req = HandshakeRequest {
        instance_id: InstanceId::new(),
        verifying_key_hex: "ab".repeat(32),
        nonce: [42u8; 32],
        protocol_version: 1,
    };
    let json = serde_json::to_string(&req).unwrap();
    let back: HandshakeRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(back.instance_id, req.instance_id);
    assert_eq!(back.nonce, req.nonce);
    assert_eq!(back.protocol_version, 1);
}

#[test]
fn goal_announcement_roundtrip() {
    let ann = GoalAnnouncement {
        goal_id: GoalId::new(),
        description: "Learn Rust".to_string(),
        priority: animus_cortex::Priority::Normal,
        source_ailf: InstanceId::new(),
    };
    let json = serde_json::to_string(&ann).unwrap();
    let back: GoalAnnouncement = serde_json::from_str(&json).unwrap();
    assert_eq!(back.goal_id, ann.goal_id);
    assert_eq!(back.description, "Learn Rust");
}
