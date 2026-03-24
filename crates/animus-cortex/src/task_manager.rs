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

    pub async fn spawn_task(
        &self,
        command: String,
        label: Option<String>,
        timeout_secs: Option<u64>,
    ) -> Result<String, String> {
        let id = new_task_id();
        let label = label.unwrap_or_else(|| {
            let c = command.trim();
            if c.len() > 40 { c[..40].to_string() } else { c.to_string() }
        });
        let log_path = self.data_dir.join("tasks").join(format!("{id}.log"));

        {
            let mut state = self.state.lock();
            if state.running_count() >= state.max_concurrent {
                let running: Vec<String> = state.records.values()
                    .filter(|r| r.state == TaskState::Running)
                    .map(|r| format!("{} \"{}\"", r.id, r.label))
                    .collect();
                return Err(format!(
                    "Task cap reached ({} running). Cancel a task or wait for one to complete.\nCurrently running: [{}]",
                    state.max_concurrent,
                    running.join(", ")
                ));
            }
            let record = TaskRecord {
                id: id.clone(),
                label: label.clone(),
                command: command.clone(),
                state: TaskState::Running,
                exit_code: None,
                spawned_at: Utc::now(),
                finished_at: None,
                log_path: log_path.clone(),
            };
            state.records.insert(id.clone(), record);
            state.save(&self.index_path())
                .map_err(|e| format!("Failed to persist task record: {e}"))?;
        }

        let mgr_state = self.state.clone();
        let signal_tx = self.signal_tx.clone();
        let source_id = self.source_id;
        let index_path = self.index_path();
        let id_clone = id.clone();
        let label_clone = label.clone();
        let command_clone = command.clone();

        let join_handle = tokio::spawn(async move {
            run_background_task(
                id_clone, command_clone, label_clone, timeout_secs,
                log_path, mgr_state, signal_tx, source_id, index_path,
            ).await;
        });

        {
            let mut state = self.state.lock();
            state.handles.insert(id.clone(), join_handle.abort_handle());
        }
        // JoinHandle dropped here — task is detached and continues running independently

        Ok(id)
    }

    pub async fn cancel_task(&self, id: &str) -> Result<String, String> {
        let (abort_handle, label, log_path) = {
            let mut state = self.state.lock();
            match state.records.get_mut(id) {
                None => return Err(format!("Unknown task id: {id}")),
                Some(rec) if rec.state != TaskState::Running => {
                    return Err(format!(
                        "Task {id} is not running (state: {:?})",
                        rec.state
                    ));
                }
                Some(rec) => {
                    let label = rec.label.clone();
                    let log_path = rec.log_path.clone();
                    rec.state = TaskState::Cancelled;
                    rec.finished_at = Some(Utc::now());
                    let handle = state.handles.remove(id);
                    (handle, label, log_path)
                }
            }
        };

        // Append CANCELLED marker — preserves any partial output already written
        {
            use tokio::io::AsyncWriteExt;
            if let Ok(mut f) = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .await
            {
                let _ = f.write_all(b"\n[CANCELLED]\n").await;
            }
        }

        {
            let state = self.state.lock();
            if let Err(e) = state.save(&self.index_path()) {
                warn!("TaskManager: failed to persist after cancel: {e}");
            }
        }

        if let Some(h) = abort_handle {
            h.abort();
        }

        Ok(format!("Task {id} (\"{label}\") cancelled."))
    }
}

// ---------------------------------------------------------------------------
// Background task future
// ---------------------------------------------------------------------------

async fn run_background_task(
    id: String,
    command: String,
    label: String,
    timeout_secs: Option<u64>,
    log_path: PathBuf,
    state: Arc<parking_lot::Mutex<TaskManagerState>>,
    signal_tx: mpsc::Sender<Signal>,
    source_id: ThreadId,
    index_path: PathBuf,
) {
    use std::process::Stdio;

    let spawn_result = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();

    let child = match spawn_result {
        Err(e) => {
            let _ = tokio::fs::write(&log_path, format!("[spawn error: {e}]\n").as_bytes()).await;
            finalize_task(&id, &label, None, false, &state, &signal_tx, source_id, &index_path).await;
            return;
        }
        Ok(c) => c,
    };

    let output_result = if let Some(secs) = timeout_secs {
        match tokio::time::timeout(
            std::time::Duration::from_secs(secs),
            child.wait_with_output()
        ).await {
            Ok(r) => r,
            Err(_elapsed) => {
                // Child dropped here — kill_on_drop fires
                let _ = tokio::fs::write(&log_path, format!("[timed out after {secs}s]\n").as_bytes()).await;
                let mut s = state.lock();
                if let Some(rec) = s.records.get_mut(&id) {
                    if rec.state == TaskState::Running {
                        rec.state = TaskState::Cancelled;
                        rec.finished_at = Some(Utc::now());
                    }
                }
                s.handles.remove(&id);
                let _ = s.save(&index_path);
                return; // No signal for timeout-cancelled
            }
        }
    } else {
        child.wait_with_output().await
    };

    match output_result {
        Err(e) => {
            let _ = tokio::fs::write(&log_path, format!("[wait error: {e}]\n").as_bytes()).await;
            finalize_task(&id, &label, None, false, &state, &signal_tx, source_id, &index_path).await;
        }
        Ok(output) => {
            let mut log_bytes: Vec<u8> = Vec::new();
            log_bytes.extend_from_slice(&output.stdout);
            if !output.stderr.is_empty() {
                if !log_bytes.is_empty() { log_bytes.push(b'\n'); }
                log_bytes.extend_from_slice(b"[stderr]\n");
                log_bytes.extend_from_slice(&output.stderr);
            }
            let _ = tokio::fs::write(&log_path, &log_bytes).await;
            let success = output.status.success();
            finalize_task(&id, &label, output.status.code(), success, &state, &signal_tx, source_id, &index_path).await;
        }
    }
}

async fn finalize_task(
    id: &str,
    label: &str,
    exit_code: Option<i32>,
    success: bool,
    state: &Arc<parking_lot::Mutex<TaskManagerState>>,
    signal_tx: &mpsc::Sender<Signal>,
    source_id: ThreadId,
    index_path: &Path,
) {
    let should_signal = {
        let mut s = state.lock();
        s.handles.remove(id);
        if let Some(rec) = s.records.get_mut(id) {
            if rec.state == TaskState::Running {
                rec.state = if success { TaskState::Completed } else { TaskState::Failed };
                rec.exit_code = exit_code;
                rec.finished_at = Some(Utc::now());
                s.evict_old();
                let _ = s.save(index_path);
                true
            } else {
                false // already cancelled — do not overwrite
            }
        } else {
            false
        }
    };

    if should_signal {
        let priority = if success { SignalPriority::Normal } else { SignalPriority::Urgent };
        let result_str = if success {
            format!("completed — exit {}", exit_code.unwrap_or(0))
        } else {
            format!("FAILED — exit {}", exit_code.unwrap_or(-1))
        };
        let summary = format!(
            "Task '{}' [{}] {}. Read output: task_output(\"{}\")",
            label, id, result_str, id
        );
        let sig = Signal {
            source_thread: source_id,
            target_thread: ThreadId::default(),
            priority,
            summary,
            segment_refs: vec![],
            created: Utc::now(),
        };
        if let Err(e) = signal_tx.send(sig).await {
            warn!("TaskManager: failed to send completion signal: {e}");
        }
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

    // ── Integration tests ────────────────────────────────────────────────────

    #[tokio::test]
    async fn spawn_echo_completes_with_signal() {
        let tmp = tempfile::tempdir().unwrap();
        let (signal_tx, mut signal_rx) = tokio::sync::mpsc::channel::<Signal>(10);
        let mgr = TaskManager::new(signal_tx, tmp.path().to_path_buf(), 5);

        let id = mgr.spawn_task("echo hello_task_output".to_string(), Some("echo test".to_string()), None)
            .await
            .expect("spawn should succeed");

        let sig = tokio::time::timeout(std::time::Duration::from_secs(5), signal_rx.recv())
            .await
            .expect("signal should arrive within 5s")
            .expect("channel should not close");

        assert!(sig.summary.contains(&id), "signal summary should contain task id");
        assert_eq!(sig.priority, SignalPriority::Normal, "exit-0 should be Normal priority");

        let rec = mgr.get_record(&id).expect("record should exist");
        assert_eq!(rec.state, TaskState::Completed);
        assert_eq!(rec.exit_code, Some(0));

        let log = std::fs::read_to_string(&rec.log_path).expect("log file should exist");
        assert!(log.contains("hello_task_output"), "log should contain stdout");
    }

    #[tokio::test]
    async fn spawn_failing_command_fires_urgent_signal() {
        let tmp = tempfile::tempdir().unwrap();
        let (signal_tx, mut signal_rx) = tokio::sync::mpsc::channel::<Signal>(10);
        let mgr = TaskManager::new(signal_tx, tmp.path().to_path_buf(), 5);

        let id = mgr.spawn_task("exit 1".to_string(), None, None)
            .await
            .expect("spawn should succeed");

        let sig = tokio::time::timeout(std::time::Duration::from_secs(5), signal_rx.recv())
            .await
            .expect("signal should arrive")
            .expect("channel open");

        assert_eq!(sig.priority, SignalPriority::Urgent);

        let rec = mgr.get_record(&id).expect("record should exist");
        assert_eq!(rec.state, TaskState::Failed);
        assert_eq!(rec.exit_code, Some(1));
    }

    #[tokio::test]
    async fn cancel_running_task_marks_cancelled_and_kills_process() {
        let tmp = tempfile::tempdir().unwrap();
        let (signal_tx, _signal_rx) = tokio::sync::mpsc::channel::<Signal>(10);
        let mgr = TaskManager::new(signal_tx, tmp.path().to_path_buf(), 5);

        let id = mgr.spawn_task("sleep 60".to_string(), Some("long sleep".to_string()), None)
            .await
            .expect("spawn should succeed");

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let result = mgr.cancel_task(&id).await;
        assert!(result.is_ok(), "cancel should succeed: {:?}", result);

        let rec = mgr.get_record(&id).expect("record should exist");
        assert_eq!(rec.state, TaskState::Cancelled);

        let handles_has_id = mgr.state.lock().handles.contains_key(&id);
        assert!(!handles_has_id, "abort handle should be removed after cancel");
    }

    #[tokio::test]
    async fn cap_exceeded_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let (signal_tx, _) = tokio::sync::mpsc::channel::<Signal>(10);
        let mgr = TaskManager::new(signal_tx, tmp.path().to_path_buf(), 2);

        mgr.spawn_task("sleep 60".to_string(), None, None).await.unwrap();
        mgr.spawn_task("sleep 60".to_string(), None, None).await.unwrap();
        let result = mgr.spawn_task("sleep 60".to_string(), None, None).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("cap reached"), "error should mention cap");
    }

    #[tokio::test]
    async fn cancel_nonrunning_task_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let (signal_tx, mut signal_rx) = tokio::sync::mpsc::channel::<Signal>(10);
        let mgr = TaskManager::new(signal_tx, tmp.path().to_path_buf(), 5);

        let id = mgr.spawn_task("echo done".to_string(), None, None).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), signal_rx.recv()).await.unwrap();

        let result = mgr.cancel_task(&id).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not running"));
    }
}
