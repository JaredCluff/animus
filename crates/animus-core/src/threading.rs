use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use crate::identity::{SegmentId, ThreadId};

/// Status of a reasoning thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThreadStatus {
    Active,
    Suspended,
    Background,
    Completed,
}

impl ThreadStatus {
    pub fn can_transition_to(self, target: Self) -> bool {
        match (self, target) {
            (s, t) if s == t => false,
            (ThreadStatus::Completed, _) => false,
            _ => true,
        }
    }
}

/// Priority of an inter-thread signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SignalPriority {
    Info,
    Normal,
    Urgent,
}

/// A message between reasoning threads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub source_thread: ThreadId,
    pub target_thread: ThreadId,
    pub priority: SignalPriority,
    pub summary: String,
    pub segment_refs: Vec<SegmentId>,
    pub created: DateTime<Utc>,
}
