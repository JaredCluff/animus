use animus_core::identity::ThreadId;
use animus_core::threading::{Signal, SignalPriority};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::warn;

// ---------------------------------------------------------------------------
// TaskId
// ---------------------------------------------------------------------------

/// Generate an 8-character lowercase hex task ID from UUID v4.
pub fn new_task_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

// ---------------------------------------------------------------------------
// TaskState
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    Running,
    Completed,   // exit code 0
    Failed,      // exit code non-zero
    Cancelled,   // killed via task_cancel tool
    Interrupted, // was Running at last shutdown; output may be partial
}

// ---------------------------------------------------------------------------
// TaskRecord  (persisted to index.json)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: String,
    pub label: String,
    pub command: String,
    pub state: TaskState,
    pub exit_code: Option<i32>,
    pub spawned_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub log_path: PathBuf,
}

// ---------------------------------------------------------------------------
// TaskManagerState  (behind Arc<Mutex>)
// ---------------------------------------------------------------------------

pub(crate) const MAX_COMPLETED_RECORDS: usize = 50;

pub(crate) struct TaskManagerState {
    pub records: HashMap<String, TaskRecord>,
    /// Abort handles for Running tasks. Not persisted — empty after restart.
    pub handles: HashMap<String, tokio::task::AbortHandle>,
    pub max_concurrent: usize,
}

impl TaskManagerState {
    /// Load from index.json, or start with empty state. Any `Running` records
    /// found on disk are immediately marked `Interrupted` (no handles survived restart).
    pub fn load_or_default(index_path: &Path, max_concurrent: usize) -> Self {
        let mut records: HashMap<String, TaskRecord> = match std::fs::read_to_string(index_path) {
            Err(_) => HashMap::new(),
            Ok(raw) => match serde_json::from_str::<HashMap<String, TaskRecord>>(&raw) {
                Ok(records) => records,
                Err(e) => {
                    warn!(
                        "Could not parse tasks/index.json ({}): {}. Proceeding with empty state.",
                        index_path.display(), e
                    );
                    HashMap::new()
                }
            },
        };
        let now = Utc::now();
        for rec in records.values_mut() {
            if rec.state == TaskState::Running {
                rec.state = TaskState::Interrupted;
                rec.finished_at = Some(now);
            }
        }
        Self { records, handles: HashMap::new(), max_concurrent }
    }

    /// Atomic write: write to .tmp then rename into place.
    pub fn save(&self, index_path: &Path) -> std::io::Result<()> {
        if let Some(parent) = index_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = index_path.with_extension("tmp");
        let json = serde_json::to_string_pretty(&self.records)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(&tmp, json.as_bytes())?;
        std::fs::rename(&tmp, index_path)?;
        Ok(())
    }

    pub fn running_count(&self) -> usize {
        self.records.values().filter(|r| r.state == TaskState::Running).count()
    }

    /// Evict oldest terminal (non-Running) records beyond MAX_COMPLETED_RECORDS.
    pub fn evict_old(&mut self) {
        let mut terminal: Vec<String> = self.records.iter()
            .filter(|(_, r)| r.state != TaskState::Running)
            .map(|(k, _)| k.clone())
            .collect();
        if terminal.len() > MAX_COMPLETED_RECORDS {
            terminal.sort_by_key(|k| self.records[k].spawned_at);
            let to_remove = terminal.len() - MAX_COMPLETED_RECORDS;
            for key in &terminal[..to_remove] {
                self.records.remove(key);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// TaskManager  (cheaply cloneable via Arc)
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct TaskManager {
    pub(crate) state: Arc<parking_lot::Mutex<TaskManagerState>>,
    pub(crate) signal_tx: mpsc::Sender<Signal>,
    pub(crate) source_id: ThreadId,
    pub(crate) data_dir: Arc<PathBuf>,
}

impl TaskManager {
    pub fn new(signal_tx: mpsc::Sender<Signal>, data_dir: PathBuf, max_concurrent: usize) -> Self {
        let tasks_dir = data_dir.join("tasks");
        let _ = std::fs::create_dir_all(&tasks_dir);
        let index_path = tasks_dir.join("index.json");
        let state = TaskManagerState::load_or_default(&index_path, max_concurrent);
        Self {
            state: Arc::new(parking_lot::Mutex::new(state)),
            signal_tx,
            source_id: ThreadId::new(),
            data_dir: Arc::new(data_dir),
        }
    }

    pub(crate) fn index_path(&self) -> PathBuf {
        self.data_dir.join("tasks").join("index.json")
    }

    /// All records, sorted by spawn time (oldest first).
    pub fn list_all(&self) -> Vec<TaskRecord> {
        let state = self.state.lock();
        let mut records: Vec<TaskRecord> = state.records.values().cloned().collect();
        records.sort_by_key(|r| r.spawned_at);
        records
    }

    pub fn get_record(&self, id: &str) -> Option<TaskRecord> {
        self.state.lock().records.get(id).cloned()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(id: &str, state: TaskState, offset_secs: i64) -> TaskRecord {
        TaskRecord {
            id: id.to_string(),
            label: format!("task {id}"),
            command: "echo".to_string(),
            state,
            exit_code: None,
            spawned_at: Utc::now() + chrono::Duration::seconds(offset_secs),
            finished_at: None,
            log_path: PathBuf::from(format!("/tmp/{id}.log")),
        }
    }

    #[test]
    fn task_id_is_8_chars() {
        let id = new_task_id();
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn task_id_unique_under_load() {
        let ids: std::collections::HashSet<String> = (0..1000)
            .map(|_| new_task_id())
            .collect();
        assert_eq!(ids.len(), 1000, "collision detected");
    }

    #[test]
    fn task_state_serde_roundtrip() {
        for state in [
            TaskState::Running, TaskState::Completed, TaskState::Failed,
            TaskState::Cancelled, TaskState::Interrupted,
        ] {
            let json = serde_json::to_string(&state).unwrap();
            let decoded: TaskState = serde_json::from_str(&json).unwrap();
            assert_eq!(state, decoded);
        }
    }

    #[test]
    fn task_record_serde_roundtrip() {
        let rec = make_record("a3f9bc12", TaskState::Completed, 0);
        let json = serde_json::to_string(&rec).unwrap();
        let decoded: TaskRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.id, "a3f9bc12");
        assert_eq!(decoded.state, TaskState::Completed);
    }

    #[test]
    fn load_missing_index_gives_empty_state() {
        let tmp = tempfile::tempdir().unwrap();
        let index = tmp.path().join("tasks").join("index.json");
        let state = TaskManagerState::load_or_default(&index, 5);
        assert!(state.records.is_empty());
        assert_eq!(state.max_concurrent, 5);
    }

    #[test]
    fn load_marks_running_records_as_interrupted() {
        let tmp = tempfile::tempdir().unwrap();
        let tasks_dir = tmp.path().join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        let index = tasks_dir.join("index.json");
        let mut records = HashMap::new();
        records.insert("aabbccdd".to_string(), make_record("aabbccdd", TaskState::Running, 0));
        records.insert("11223344".to_string(), make_record("11223344", TaskState::Completed, -10));
        std::fs::write(&index, serde_json::to_string(&records).unwrap()).unwrap();

        let state = TaskManagerState::load_or_default(&index, 5);
        assert_eq!(state.records["aabbccdd"].state, TaskState::Interrupted);
        assert_eq!(state.records["11223344"].state, TaskState::Completed);
    }

    #[test]
    fn evict_old_keeps_last_50_non_running() {
        let mut state = TaskManagerState {
            records: HashMap::new(),
            handles: HashMap::new(),
            max_concurrent: 5,
        };
        for i in 0..60usize {
            let id = format!("{:08x}", i);
            state.records.insert(id.clone(), make_record(&id, TaskState::Completed, i as i64));
        }
        state.records.insert("run00001".to_string(), make_record("run00001", TaskState::Running, 100));
        state.records.insert("run00002".to_string(), make_record("run00002", TaskState::Running, 101));

        state.evict_old();

        let running_count = state.records.values().filter(|r| r.state == TaskState::Running).count();
        let terminal_count = state.records.values().filter(|r| r.state != TaskState::Running).count();
        assert_eq!(running_count, 2, "running records must not be evicted");
        assert_eq!(terminal_count, MAX_COMPLETED_RECORDS, "should keep exactly 50 terminal records");
    }

    #[test]
    fn running_count_counts_only_running() {
        let mut state = TaskManagerState {
            records: HashMap::new(),
            handles: HashMap::new(),
            max_concurrent: 5,
        };
        state.records.insert("r1".to_string(), make_record("r1", TaskState::Running, 0));
        state.records.insert("r2".to_string(), make_record("r2", TaskState::Running, 0));
        state.records.insert("c1".to_string(), make_record("c1", TaskState::Completed, -5));
        assert_eq!(state.running_count(), 2);
    }
}
