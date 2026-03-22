use animus_core::{GoalId, InstanceId, SegmentId};
use animus_federation::audit::*;
use tempfile::TempDir;

#[test]
fn append_and_read_entries() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("federation_audit.jsonl");
    let mut trail = FederationAuditTrail::open(&path).unwrap();

    let entry = FederationAuditEntry {
        timestamp: chrono::Utc::now(),
        action: FederationAuditAction::HandshakeCompleted,
        peer_instance_id: InstanceId::new(),
        segment_id: None,
        goal_id: None,
    };

    trail.append(&entry).unwrap();

    let entry2 = FederationAuditEntry {
        timestamp: chrono::Utc::now(),
        action: FederationAuditAction::SegmentReceived,
        peer_instance_id: InstanceId::new(),
        segment_id: Some(SegmentId::new()),
        goal_id: None,
    };

    trail.append(&entry2).unwrap();

    assert_eq!(trail.entry_count(), 2);

    let entries = FederationAuditTrail::read_all(&path).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].action, FederationAuditAction::HandshakeCompleted);
    assert_eq!(entries[1].action, FederationAuditAction::SegmentReceived);
}

#[test]
fn audit_trail_survives_reopen() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("federation_audit.jsonl");

    {
        let mut trail = FederationAuditTrail::open(&path).unwrap();
        trail
            .append(&FederationAuditEntry {
                timestamp: chrono::Utc::now(),
                action: FederationAuditAction::PeerTrusted,
                peer_instance_id: InstanceId::new(),
                segment_id: None,
                goal_id: Some(GoalId::new()),
            })
            .unwrap();
        assert_eq!(trail.entry_count(), 1);
    }

    {
        let mut trail = FederationAuditTrail::open(&path).unwrap();
        assert_eq!(trail.entry_count(), 1);
        trail
            .append(&FederationAuditEntry {
                timestamp: chrono::Utc::now(),
                action: FederationAuditAction::GoalReceived,
                peer_instance_id: InstanceId::new(),
                segment_id: None,
                goal_id: Some(GoalId::new()),
            })
            .unwrap();
        assert_eq!(trail.entry_count(), 2);
    }

    let entries = FederationAuditTrail::read_all(&path).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].action, FederationAuditAction::PeerTrusted);
    assert_eq!(entries[1].action, FederationAuditAction::GoalReceived);
}
