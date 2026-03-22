use animus_core::sensorium::*;
use animus_core::{EventId, PolicyId, SegmentId};
use animus_sensorium::audit::AuditTrail;
use tempfile::TempDir;

#[test]
fn append_and_read_entries() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("audit.jsonl");
    let mut trail = AuditTrail::open(&path).unwrap();

    let entry = AuditEntry {
        timestamp: chrono::Utc::now(),
        event_id: EventId::new(),
        consent_policy: Some(PolicyId::new()),
        attention_tier_reached: 1,
        action_taken: AuditAction::Logged,
        segment_created: None,
    };

    trail.append(&entry).unwrap();
    trail.append(&entry).unwrap();

    let entries = AuditTrail::read_all(&path).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].action_taken, AuditAction::Logged);
}

#[test]
fn audit_trail_survives_reopen() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("audit.jsonl");

    {
        let mut trail = AuditTrail::open(&path).unwrap();
        trail
            .append(&AuditEntry {
                timestamp: chrono::Utc::now(),
                event_id: EventId::new(),
                consent_policy: None,
                attention_tier_reached: 2,
                action_taken: AuditAction::Promoted,
                segment_created: Some(SegmentId::new()),
            })
            .unwrap();
    }

    {
        let mut trail = AuditTrail::open(&path).unwrap();
        trail
            .append(&AuditEntry {
                timestamp: chrono::Utc::now(),
                event_id: EventId::new(),
                consent_policy: None,
                attention_tier_reached: 1,
                action_taken: AuditAction::Ignored,
                segment_created: None,
            })
            .unwrap();
    }

    let entries = AuditTrail::read_all(&path).unwrap();
    assert_eq!(entries.len(), 2);
}

#[test]
fn audit_trail_entry_count() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("audit.jsonl");
    let mut trail = AuditTrail::open(&path).unwrap();

    for _ in 0..5 {
        trail
            .append(&AuditEntry {
                timestamp: chrono::Utc::now(),
                event_id: EventId::new(),
                consent_policy: None,
                attention_tier_reached: 1,
                action_taken: AuditAction::Logged,
                segment_created: None,
            })
            .unwrap();
    }

    assert_eq!(trail.entry_count(), 5);
}

#[test]
fn audit_trail_rotation() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("audit.jsonl");

    // Use a tiny max size so rotation triggers quickly
    let mut trail = AuditTrail::open_with_max_size(&path, 200).unwrap();

    let entry = AuditEntry {
        timestamp: chrono::Utc::now(),
        event_id: EventId::new(),
        consent_policy: None,
        attention_tier_reached: 1,
        action_taken: AuditAction::Logged,
        segment_created: None,
    };

    // Write enough entries to trigger at least one rotation
    for _ in 0..20 {
        trail.append(&entry).unwrap();
    }

    // The rotated file should exist
    let rotated = dir.path().join("audit.jsonl.1");
    assert!(rotated.exists(), "rotation should create .1 file");

    // Current file should have been reset (fewer entries than total)
    assert!(trail.entry_count() < 20);

    // Rotated file should contain entries
    let rotated_entries = AuditTrail::read_all(&rotated).unwrap();
    assert!(!rotated_entries.is_empty(), "rotated file should have entries");
}
