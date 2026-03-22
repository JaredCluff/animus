use animus_core::threading::*;
use animus_core::{ThreadId, SegmentId};

#[test]
fn thread_status_transitions() {
    let status = ThreadStatus::Active;
    assert!(status.can_transition_to(ThreadStatus::Suspended));
    assert!(status.can_transition_to(ThreadStatus::Background));
    assert!(status.can_transition_to(ThreadStatus::Completed));
    assert!(!status.can_transition_to(ThreadStatus::Active));
}

#[test]
fn completed_thread_cannot_transition() {
    let status = ThreadStatus::Completed;
    assert!(!status.can_transition_to(ThreadStatus::Active));
    assert!(!status.can_transition_to(ThreadStatus::Suspended));
}

#[test]
fn signal_construction() {
    let signal = Signal {
        source_thread: ThreadId::new(),
        target_thread: ThreadId::new(),
        priority: SignalPriority::Normal,
        summary: "test signal".to_string(),
        segment_refs: vec![SegmentId::new()],
        created: chrono::Utc::now(),
    };
    assert_eq!(signal.priority, SignalPriority::Normal);
    assert_eq!(signal.segment_refs.len(), 1);
}

#[test]
fn signal_priority_ordering() {
    assert!(SignalPriority::Urgent > SignalPriority::Normal);
    assert!(SignalPriority::Normal > SignalPriority::Info);
}

#[test]
fn thread_status_serialization_roundtrip() {
    let status = ThreadStatus::Background;
    let json = serde_json::to_string(&status).unwrap();
    let back: ThreadStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(back, status);
}
