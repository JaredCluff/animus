use animus_core::InstanceId;
use animus_cortex::telos::{Autonomy, GoalManager, GoalSource, GoalStatus, Priority};
use tempfile::TempDir;

#[test]
fn test_create_and_list_goals() {
    let mut manager = GoalManager::new();

    manager.create_goal("Learn Rust".to_string(), GoalSource::Human, Priority::High).unwrap();
    manager.create_goal("Read docs".to_string(), GoalSource::Human, Priority::Low).unwrap();

    let active = manager.active_goals();
    assert_eq!(active.len(), 2);
    assert_eq!(active[0].description, "Learn Rust");
    assert_eq!(active[1].description, "Read docs");
}

#[test]
fn test_complete_goal() {
    let mut manager = GoalManager::new();
    let id = manager.create_goal("Test goal".to_string(), GoalSource::Human, Priority::Normal).unwrap();

    manager.complete_goal(id).unwrap();

    let goal = manager.get(id).unwrap();
    assert_eq!(goal.status, GoalStatus::Completed);
    assert!(manager.active_goals().is_empty());
}

#[test]
fn test_goal_persistence() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("goals.bin");

    let id = {
        let mut manager = GoalManager::new();
        let id = manager.create_goal("Persist me".to_string(), GoalSource::Human, Priority::Normal).unwrap();
        manager.save(&path).unwrap();
        id
    };

    let loaded = GoalManager::load(&path).unwrap();
    let goal = loaded.get(id).unwrap();
    assert_eq!(goal.description, "Persist me");
}

#[test]
fn test_goals_summary() {
    let mut manager = GoalManager::new();
    manager.create_goal("High priority task".to_string(), GoalSource::Human, Priority::High).unwrap();
    manager.create_goal("Background task".to_string(), GoalSource::SelfDerived, Priority::Background).unwrap();

    let summary = manager.goals_summary();
    assert!(summary.contains("High priority task"));
    assert!(summary.contains("Background task"));
}

#[test]
fn test_default_autonomy_by_source() {
    let mut manager = GoalManager::new();

    let human_id = manager.create_goal("Human goal".to_string(), GoalSource::Human, Priority::Normal).unwrap();
    let self_id = manager.create_goal("Self goal".to_string(), GoalSource::SelfDerived, Priority::Normal).unwrap();
    let fed_id = manager.create_goal("Fed goal".to_string(), GoalSource::Federated { source_ailf: InstanceId::new() }, Priority::Normal).unwrap();

    assert_eq!(manager.get(human_id).unwrap().autonomy, Autonomy::Act);
    assert_eq!(manager.get(self_id).unwrap().autonomy, Autonomy::Suggest);
    assert_eq!(manager.get(fed_id).unwrap().autonomy, Autonomy::Inform);
}
