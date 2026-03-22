use animus_core::sensorium::*;
use animus_core::EventId;
use animus_sensorium::attention::{AttentionFilter, AttentionRule, RuleAction};

fn make_event(event_type: EventType, data: serde_json::Value) -> SensorEvent {
    SensorEvent {
        id: EventId::new(),
        timestamp: chrono::Utc::now(),
        event_type,
        source: "test".to_string(),
        data,
        consent_policy: None,
    }
}

// === Tier 1 tests ===

#[test]
fn tier1_ignore_tmp_files() {
    let filter = AttentionFilter::new(vec![AttentionRule {
        event_types: vec![EventType::FileChange],
        path_patterns: vec!["/tmp/**".to_string()],
        action: RuleAction::Ignore,
    }]);
    let event = make_event(
        EventType::FileChange,
        serde_json::json!({"path": "/tmp/scratch.txt", "op": "modify"}),
    );
    let decision = filter.tier1_evaluate(&event);
    assert!(matches!(decision, AttentionDecision::Drop { .. }));
}

#[test]
fn tier1_pass_interesting_files() {
    let filter = AttentionFilter::new(vec![AttentionRule {
        event_types: vec![EventType::FileChange],
        path_patterns: vec!["/tmp/**".to_string()],
        action: RuleAction::Ignore,
    }]);
    let event = make_event(
        EventType::FileChange,
        serde_json::json!({"path": "/home/user/project/src/main.rs", "op": "modify"}),
    );
    let decision = filter.tier1_evaluate(&event);
    assert!(matches!(decision, AttentionDecision::Pass { promoted: false }));
}

#[test]
fn tier1_promote_high_priority_pattern() {
    let filter = AttentionFilter::new(vec![AttentionRule {
        event_types: vec![EventType::FileChange],
        path_patterns: vec!["**/Cargo.toml".to_string()],
        action: RuleAction::Promote,
    }]);
    let event = make_event(
        EventType::FileChange,
        serde_json::json!({"path": "/home/user/project/Cargo.toml", "op": "modify"}),
    );
    let decision = filter.tier1_evaluate(&event);
    assert!(matches!(decision, AttentionDecision::Pass { promoted: true }));
}

#[test]
fn tier1_no_rules_passes_through() {
    let filter = AttentionFilter::new(vec![]);
    let event = make_event(
        EventType::FileChange,
        serde_json::json!({"path": "/anything"}),
    );
    let decision = filter.tier1_evaluate(&event);
    assert!(matches!(decision, AttentionDecision::Pass { promoted: false }));
}

#[test]
fn tier1_ignores_non_matching_event_types() {
    let filter = AttentionFilter::new(vec![AttentionRule {
        event_types: vec![EventType::ProcessLifecycle],
        path_patterns: vec![],
        action: RuleAction::Ignore,
    }]);
    let event = make_event(
        EventType::FileChange,
        serde_json::json!({"path": "/test.rs"}),
    );
    let decision = filter.tier1_evaluate(&event);
    assert!(matches!(decision, AttentionDecision::Pass { promoted: false }));
}

// === Tier 2 tests ===

#[test]
fn tier2_similar_event_passes() {
    let filter = AttentionFilter::new(vec![]);
    let event_embedding = vec![1.0, 0.0, 0.0, 0.0];
    let goal_embeddings = vec![vec![0.9, 0.1, 0.0, 0.0]];
    let decision = filter.tier2_evaluate(&event_embedding, &goal_embeddings, 0.8);
    assert!(matches!(decision, AttentionDecision::Pass { promoted: true }));
}

#[test]
fn tier2_dissimilar_event_dropped() {
    let filter = AttentionFilter::new(vec![]);
    let event_embedding = vec![1.0, 0.0, 0.0, 0.0];
    let goal_embeddings = vec![vec![0.0, 0.0, 0.0, 1.0]];
    let decision = filter.tier2_evaluate(&event_embedding, &goal_embeddings, 0.5);
    assert!(matches!(decision, AttentionDecision::Drop { .. }));
}

#[test]
fn tier2_multiple_goals_best_match() {
    let filter = AttentionFilter::new(vec![]);
    let event_embedding = vec![1.0, 0.0, 0.0, 0.0];
    let goal_embeddings = vec![
        vec![0.0, 1.0, 0.0, 0.0],
        vec![0.95, 0.05, 0.0, 0.0],
    ];
    let decision = filter.tier2_evaluate(&event_embedding, &goal_embeddings, 0.9);
    assert!(matches!(decision, AttentionDecision::Pass { promoted: true }));
}

#[test]
fn tier2_no_goals_drops() {
    let filter = AttentionFilter::new(vec![]);
    let event_embedding = vec![1.0, 0.0, 0.0, 0.0];
    let goal_embeddings: Vec<Vec<f32>> = vec![];
    let decision = filter.tier2_evaluate(&event_embedding, &goal_embeddings, 0.5);
    assert!(matches!(decision, AttentionDecision::Drop { .. }));
}
