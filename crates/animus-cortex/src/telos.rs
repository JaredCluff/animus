use animus_core::error::{AnimusError, Result};
use animus_core::identity::{GoalId, SegmentId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Priority level for goals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Priority {
    Critical,
    High,
    Normal,
    Low,
    Background,
}

/// Current status of a goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoalStatus {
    Active,
    Paused,
    Completed,
    Abandoned,
}

/// Where a goal came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GoalSource {
    Human,
    SelfDerived,
    Federated { source_ailf: animus_core::InstanceId },
}

/// Autonomy level — how much freedom the AILF has with this goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Autonomy {
    Inform,
    Suggest,
    Act,
    Full,
}

/// A goal tracked by Telos.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    pub id: GoalId,
    pub description: String,
    pub source: GoalSource,
    pub priority: Priority,
    pub status: GoalStatus,
    pub success_criteria: Vec<String>,
    pub autonomy: Autonomy,
    pub sub_goals: Vec<GoalId>,
    pub progress_notes: Vec<SegmentId>,
    pub cached_embedding: Option<Vec<f32>>,
    pub created: chrono::DateTime<chrono::Utc>,
    pub deadline: Option<chrono::DateTime<chrono::Utc>>,
}

/// Simple goal manager — tracks goals in memory with persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalManager {
    goals: HashMap<GoalId, Goal>,
}

impl GoalManager {
    pub fn new() -> Self {
        Self {
            goals: HashMap::new(),
        }
    }

    /// Maximum number of goals the manager will hold simultaneously.
    pub const MAX_GOALS: usize = 10_000;

    /// Maximum number of federated goals allowed at once, to prevent a peer AILF
    /// from flooding the goal manager and starving human-originated goals.
    pub const MAX_FEDERATED_GOALS: usize = 100;

    /// Create a new goal. Returns an error if a goal limit has been reached.
    pub fn create_goal(
        &mut self,
        description: String,
        source: GoalSource,
        priority: Priority,
    ) -> Result<GoalId> {
        if self.goals.len() >= Self::MAX_GOALS {
            return Err(AnimusError::Goal(format!(
                "goal limit reached ({} goals); complete or abandon existing goals first",
                Self::MAX_GOALS
            )));
        }

        if matches!(source, GoalSource::Federated { .. }) {
            let federated_count = self
                .goals
                .values()
                .filter(|g| matches!(g.source, GoalSource::Federated { .. }))
                .count();
            if federated_count >= Self::MAX_FEDERATED_GOALS {
                return Err(AnimusError::Goal(format!(
                    "federated goal limit reached ({} goals); complete or abandon existing federated goals first",
                    Self::MAX_FEDERATED_GOALS
                )));
            }
        }

        let autonomy = match &source {
            GoalSource::Human => Autonomy::Act,
            GoalSource::SelfDerived => Autonomy::Suggest,
            GoalSource::Federated { .. } => Autonomy::Inform,
        };

        let goal = Goal {
            id: GoalId::new(),
            description,
            source,
            priority,
            status: GoalStatus::Active,
            success_criteria: Vec::new(),
            autonomy,
            sub_goals: Vec::new(),
            progress_notes: Vec::new(),
            cached_embedding: None,
            created: chrono::Utc::now(),
            deadline: None,
        };

        let id = goal.id;
        self.goals.insert(id, goal);
        Ok(id)
    }

    /// Get a goal by ID.
    pub fn get(&self, id: GoalId) -> Option<&Goal> {
        self.goals.get(&id)
    }

    /// List active goals, sorted by priority.
    pub fn active_goals(&self) -> Vec<&Goal> {
        let mut active: Vec<&Goal> = self
            .goals
            .values()
            .filter(|g| g.status == GoalStatus::Active)
            .collect();
        active.sort_by_key(|g| match g.priority {
            Priority::Critical => 0,
            Priority::High => 1,
            Priority::Normal => 2,
            Priority::Low => 3,
            Priority::Background => 4,
        });
        active
    }

    /// Mark a goal as completed.
    pub fn complete_goal(&mut self, id: GoalId) -> Result<()> {
        let goal = self
            .goals
            .get_mut(&id)
            .ok_or(AnimusError::Goal(format!("goal not found: {}", id.0)))?;
        goal.status = GoalStatus::Completed;
        Ok(())
    }

    /// Pause a goal.
    pub fn pause_goal(&mut self, id: GoalId) -> Result<()> {
        let goal = self
            .goals
            .get_mut(&id)
            .ok_or(AnimusError::Goal(format!("goal not found: {}", id.0)))?;
        goal.status = GoalStatus::Paused;
        Ok(())
    }

    /// Abandon a goal.
    pub fn abandon_goal(&mut self, id: GoalId) -> Result<()> {
        let goal = self
            .goals
            .get_mut(&id)
            .ok_or(AnimusError::Goal(format!("goal not found: {}", id.0)))?;
        goal.status = GoalStatus::Abandoned;
        Ok(())
    }

    /// Maximum number of progress notes per goal.
    pub const MAX_PROGRESS_NOTES: usize = 1_000;

    /// Add a progress note (segment ID) to a goal.
    pub fn add_progress_note(&mut self, goal_id: GoalId, segment_id: SegmentId) -> Result<()> {
        let goal = self
            .goals
            .get_mut(&goal_id)
            .ok_or(AnimusError::Goal(format!("goal not found: {}", goal_id.0)))?;
        if goal.progress_notes.len() >= Self::MAX_PROGRESS_NOTES {
            return Err(AnimusError::Goal(format!(
                "progress note limit ({}) reached for goal {}",
                Self::MAX_PROGRESS_NOTES,
                goal_id.0
            )));
        }
        goal.progress_notes.push(segment_id);
        Ok(())
    }

    /// Get a summary of active goals for context injection.
    pub fn goals_summary(&self) -> String {
        let active = self.active_goals();
        if active.is_empty() {
            return String::new();
        }
        let mut summary = String::from("Active goals:\n");
        for goal in active {
            let priority = format!("{:?}", goal.priority).to_uppercase();
            summary.push_str(&format!("- [{}] {}\n", priority, goal.description));
        }
        summary
    }

    /// Total number of goals.
    pub fn count(&self) -> usize {
        self.goals.len()
    }

    /// Persist to a file.
    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        let data = bincode::serialize(self)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp_path = path.with_extension("tmp");
        std::fs::write(&tmp_path, &data)?;
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }

    /// Load from a file.
    pub fn load(path: &std::path::Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let metadata = std::fs::metadata(path)?;
        if metadata.len() > 67_108_864 {
            return Err(AnimusError::Goal(
                format!("goals file too large: {} bytes (max 64 MiB)", metadata.len())
            ));
        }
        let data = std::fs::read(path)?;
        let manager: Self = bincode::deserialize(&data)
            .map_err(|e| AnimusError::Goal(
                format!("failed to load goals from {}: {e}", path.display())
            ))?;
        Ok(manager)
    }
}

impl Default for GoalManager {
    fn default() -> Self {
        Self::new()
    }
}
