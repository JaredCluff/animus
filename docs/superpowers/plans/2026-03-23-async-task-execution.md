# Async Task Execution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give Animus the ability to spawn long-running shell processes in the background, receive a Signal on completion, and query or cancel tasks via four new tools.

**Architecture:** A `TaskManager` struct (mirroring `WatcherRegistry`) lives in `animus-cortex`, is injected into `ToolContext`, and is wired into `animus-runtime`. Each spawned task runs as a self-contained tokio future that writes output to disk and fires a Signal on exit. Cancellation aborts the tokio future (killing the child process via `kill_on_drop`) and updates state.

**Tech Stack:** Rust, tokio 1.x (`full` features including `process`), parking_lot, serde_json, uuid, chrono, animus_core Signal/ThreadId types.

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/animus-cortex/src/task_manager.rs` | Create | TaskId, TaskState, TaskRecord, TaskManagerState, TaskManager, background future |
| `crates/animus-cortex/src/tools/spawn_task.rs` | Create | spawn_task LLM tool |
| `crates/animus-cortex/src/tools/task_status.rs` | Create | task_status LLM tool |
| `crates/animus-cortex/src/tools/task_output.rs` | Create | task_output LLM tool |
| `crates/animus-cortex/src/tools/task_cancel.rs` | Create | task_cancel LLM tool |
| `crates/animus-cortex/src/tools/mod.rs` | Modify | Add `task_manager: Option<TaskManager>` to ToolContext; `pub mod` for 4 new tools |
| `crates/animus-cortex/src/lib.rs` | Modify | `pub mod task_manager`; re-export public types |
| `crates/animus-runtime/src/main.rs` | Modify | Construct TaskManager; ToolContext; register tools; CommandContext; /task handler; system prompt |
| `crates/animus-tests/tests/integration/tool_use.rs` | Modify | Add `task_manager: None` to test_ctx helper |

---

## Task 1: task_manager.rs — Data Model and State

**Files:**
- Create: `crates/animus-cortex/src/task_manager.rs`

### Background

`WatcherRegistry` in `watcher.rs` is the reference pattern. Read it before implementing this task (`crates/animus-cortex/src/watcher.rs`). Follow the same Arc/Mutex/Clone structure.

Key decisions:
- `TaskId` = 8-char lowercase hex string from first 8 chars of `Uuid::new_v4().simple().to_string()`
- `handles` stores `tokio::task::AbortHandle` (not `Child`) — the background future owns the child
- `kill_on_drop(true)` on the Command ensures the child is killed when the future is aborted
- On restart, any `Running` records in `index.json` are immediately marked `Interrupted`
- Index retention: keep last 50 non-Running records; evict oldest on overflow
- `signal_tx` and `source_id` stored on `TaskManager` at construction (same as `WatcherRegistry.signal_tx` / `source_id`)

- [ ] **Step 1: Write failing unit tests**

Paste this entire block into `crates/animus-cortex/src/task_manager.rs` as a stub + tests:

```rust
use animus_core::identity::ThreadId;
use animus_core::threading::{Signal, SignalPriority};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;

pub type TaskId = String; // will replace with newtype below

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

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

pub(crate) const MAX_COMPLETED_RECORDS: usize = 50;

pub(crate) struct TaskManagerState {
    pub records: HashMap<String, TaskRecord>,
    pub handles: HashMap<String, tokio::task::AbortHandle>,
    pub max_concurrent: usize,
}

// Stub impls — will be filled in Step 3
impl TaskManagerState {
    pub fn load_or_default(_index_path: &Path, _max_concurrent: usize) -> Self { todo!() }
    pub fn save(&self, _index_path: &Path) -> std::io::Result<()> { todo!() }
    pub fn running_count(&self) -> usize { todo!() }
    pub fn evict_old(&mut self) { todo!() }
}

#[derive(Clone)]
pub struct TaskManager {
    pub(crate) state: Arc<parking_lot::Mutex<TaskManagerState>>,
    pub(crate) signal_tx: mpsc::Sender<Signal>,
    pub(crate) source_id: ThreadId,
    pub(crate) data_dir: Arc<PathBuf>,
}

impl TaskManager {
    pub fn new(_signal_tx: mpsc::Sender<Signal>, _data_dir: PathBuf, _max_concurrent: usize) -> Self { todo!() }
    pub fn list_all(&self) -> Vec<TaskRecord> { todo!() }
    pub fn get_record(&self, _id: &str) -> Option<TaskRecord> { todo!() }
}

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
        let id = uuid::Uuid::new_v4().simple().to_string();
        let short = &id[..8];
        assert_eq!(short.len(), 8);
        assert!(short.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn task_id_unique_under_load() {
        let ids: std::collections::HashSet<String> = (0..1000)
            .map(|_| uuid::Uuid::new_v4().simple().to_string()[..8].to_string())
            .collect();
        assert_eq!(ids.len(), 1000, "collision detected");
    }

    #[test]
    fn task_state_serde_roundtrip() {
        for state in [
            TaskState::Running,
            TaskState::Completed,
            TaskState::Failed,
            TaskState::Cancelled,
            TaskState::Interrupted,
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
        let tmp = tempfile::tempdir().unwrap();
        let index = tmp.path().join("index.json");
        let mut state = TaskManagerState {
            records: HashMap::new(),
            handles: HashMap::new(),
            max_concurrent: 5,
        };
        for i in 0..60usize {
            let id = format!("{:08x}", i);
            state.records.insert(id.clone(), make_record(&id, TaskState::Completed, i as i64));
        }
        // Also add 2 Running records — they must NOT be evicted
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
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p animus-cortex task_manager 2>&1 | head -30
```

Expected: compilation errors (`todo!()` panics or type errors).

- [ ] **Step 3: Implement data model and state**

Replace the stub file with the full implementation:

```rust
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

/// 8-character lowercase hex string. Generated from UUID v4 — no rand needed.
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
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
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

    /// Atomic write: .tmp then rename.
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

// Tests are at the bottom of this file (added in Task 2 as well).
#[cfg(test)]
mod tests {
    // ... (tests from Step 1 above, paste in full)
}
```

Paste the test module from Step 1 at the bottom.

- [ ] **Step 4: Run tests**

```bash
cargo test -p animus-cortex task_manager 2>&1
```

Expected: all 7 data model tests pass (task_id_is_8_chars, task_id_unique_under_load, task_state_serde_roundtrip, task_record_serde_roundtrip, load_missing_index_gives_empty_state, load_marks_running_records_as_interrupted, evict_old_keeps_last_50_non_running, running_count_counts_only_running).

- [ ] **Step 5: Commit**

```bash
git add crates/animus-cortex/src/task_manager.rs
git commit -m "feat(task-manager): add TaskManager data model and state persistence"
```

---

## Task 2: task_manager.rs — Spawn, Background Future, and Cancel

**Files:**
- Modify: `crates/animus-cortex/src/task_manager.rs`

### How the background future works

1. Spawns child with `kill_on_drop(true)` and piped stdout+stderr
2. Awaits `child.wait_with_output()` (with optional timeout wrapper)
3. On natural completion: checks state under mutex — if still Running, marks Completed/Failed, writes log, saves index, fires Signal
4. On timeout: writes `[timed out]` to log, marks Cancelled, saves index, returns (no Signal)
5. On abort (from `cancel_task`): future is dropped → child killed via `kill_on_drop`. **The `cancel_task` method (not the future) is responsible for state update, log write, and index save.**

`cancel_task` flow:
1. Lock mutex, check state == Running, mark Cancelled, remove AbortHandle from map
2. Drop lock
3. Write `[CANCELLED]` to log file (async, outside lock)
4. Save index (outside lock)
5. Call `abort_handle.abort()` — the background future is cancelled, child killed

Race safety: both the background future and `cancel_task` check `state == Running` under the mutex before writing state. Only one wins; the other is a no-op.

- [ ] **Step 1: Write failing integration tests**

Add to the `#[cfg(test)]` module in `task_manager.rs`:

```rust
    // ── Integration tests ────────────────────────────────────────────────────

    #[tokio::test]
    async fn spawn_echo_completes_with_signal() {
        let tmp = tempfile::tempdir().unwrap();
        let (signal_tx, mut signal_rx) = tokio::sync::mpsc::channel::<Signal>(10);
        let mgr = TaskManager::new(signal_tx, tmp.path().to_path_buf(), 5);

        let id = mgr.spawn_task("echo hello_task_output".to_string(), Some("echo test".to_string()), None)
            .await
            .expect("spawn should succeed");

        // Wait for signal (with timeout so test doesn't hang)
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

        // Give the process a moment to actually start
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let result = mgr.cancel_task(&id).await;
        assert!(result.is_ok(), "cancel should succeed: {:?}", result);

        let rec = mgr.get_record(&id).expect("record should exist");
        assert_eq!(rec.state, TaskState::Cancelled);

        // Handle should be gone
        let handles_has_id = mgr.state.lock().handles.contains_key(&id);
        assert!(!handles_has_id, "abort handle should be removed after cancel");
    }

    #[tokio::test]
    async fn cap_exceeded_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let (signal_tx, _) = tokio::sync::mpsc::channel::<Signal>(10);
        let mgr = TaskManager::new(signal_tx, tmp.path().to_path_buf(), 2); // cap of 2

        mgr.spawn_task("sleep 60".to_string(), None, None).await.unwrap();
        mgr.spawn_task("sleep 60".to_string(), None, None).await.unwrap();
        let result = mgr.spawn_task("sleep 60".to_string(), None, None).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("cap reached"), "error should mention cap: {err}");
    }

    #[tokio::test]
    async fn cancel_nonrunning_task_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let (signal_tx, mut signal_rx) = tokio::sync::mpsc::channel::<Signal>(10);
        let mgr = TaskManager::new(signal_tx, tmp.path().to_path_buf(), 5);

        let id = mgr.spawn_task("echo done".to_string(), None, None).await.unwrap();
        // Wait for completion
        tokio::time::timeout(std::time::Duration::from_secs(5), signal_rx.recv()).await.unwrap();

        let result = mgr.cancel_task(&id).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not running"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p animus-cortex "spawn_echo\|spawn_failing\|cancel_running\|cap_exceeded\|cancel_nonrunning" 2>&1 | head -20
```

Expected: compilation errors (spawn_task and cancel_task not yet defined).

- [ ] **Step 3: Implement spawn_task, cancel_task, and background future**

Add these functions to `task_manager.rs` (below the `TaskManager` impl block):

```rust
impl TaskManager {
    // ... existing methods ...

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

        // Check cap and register record under lock
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
            // Persist before spawning — record survives a crash between here and spawn
            state.save(&self.index_path())
                .map_err(|e| format!("Failed to persist task record: {e}"))?;
        }

        // Spawn background future
        let mgr_state = self.state.clone();
        let signal_tx = self.signal_tx.clone();
        let source_id = self.source_id;
        let index_path = self.index_path();
        let id_clone = id.clone();
        let label_clone = label.clone();
        let command_clone = command.clone();

        let join_handle = tokio::spawn(async move {
            run_background_task(
                id_clone,
                command_clone,
                label_clone,
                timeout_secs,
                log_path,
                mgr_state,
                signal_tx,
                source_id,
                index_path,
            ).await;
        });

        // Store AbortHandle; drop JoinHandle (task is detached, continues running)
        {
            let mut state = self.state.lock();
            state.handles.insert(id.clone(), join_handle.abort_handle());
        }

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

        // Append CANCELLED marker to log (outside lock) — preserves any partial output
        // already written by the process before cancellation.
        let _ = {
            use tokio::io::AsyncWriteExt;
            tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .await
                .and_then(|mut f| async move { f.write_all(b"\n[CANCELLED]\n").await })
        }
        .await;

        // Persist index (outside lock)
        {
            let state = self.state.lock();
            if let Err(e) = state.save(&self.index_path()) {
                warn!("TaskManager: failed to persist after cancel: {e}");
            }
        }

        // Abort the background future — kills child via kill_on_drop
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
            // Collect stdout + stderr into log
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
```

- [ ] **Step 4: Run all task_manager tests**

```bash
cargo test -p animus-cortex task_manager 2>&1
```

Expected: all tests pass including the 5 integration tests. The integration tests use real processes (`echo`, `exit 1`, `sleep 60`).

- [ ] **Step 5: Commit**

```bash
git add crates/animus-cortex/src/task_manager.rs
git commit -m "feat(task-manager): implement spawn_task, cancel_task, and background future"
```

---

## Task 3: Four LLM Tools

**Files:**
- Create: `crates/animus-cortex/src/tools/spawn_task.rs`
- Create: `crates/animus-cortex/src/tools/task_status.rs`
- Create: `crates/animus-cortex/src/tools/task_output.rs`
- Create: `crates/animus-cortex/src/tools/task_cancel.rs`

### Note on test ToolContext

These tests use a minimal ToolContext with no TaskManager (`task_manager: None` for most tests). For tests that exercise the actual tool behavior, construct a real `TaskManager` with a tempdir. The ToolContext struct will have the new `task_manager` field after Task 4, so implement Task 4 first if compilation fails — or use a `todo!()` stub field temporarily.

Actually: do Task 4 (wiring) first, then come back to write tests for these tools if compilation requires it. The test structure below assumes Task 4 is done.

### spawn_task.rs

- [ ] **Step 1: Write failing tests**

```rust
// crates/animus-cortex/src/tools/spawn_task.rs
use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

pub struct SpawnTaskTool;

#[async_trait::async_trait]
impl Tool for SpawnTaskTool {
    fn name(&self) -> &str { "spawn_task" }
    fn description(&self) -> &str {
        "Spawn a long-running shell command in the background. Returns a task_id immediately. \
         You receive a Signal when it completes. Read output with task_output(task_id)."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to run" },
                "label": { "type": "string", "description": "Short human-readable label (optional)" },
                "timeout_secs": { "type": "integer", "description": "Kill after N seconds (optional)" }
            },
            "required": ["command"]
        })
    }
    fn required_autonomy(&self) -> Autonomy { Autonomy::Act }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let command = match params["command"].as_str() {
            Some(c) => c.to_string(),
            None => return Ok(ToolResult { content: "missing 'command' parameter".to_string(), is_error: true }),
        };
        let label = params["label"].as_str().map(|s| s.to_string());
        let timeout_secs = params["timeout_secs"].as_u64();

        let manager = match &ctx.task_manager {
            Some(m) => m,
            None => return Ok(ToolResult { content: "Task manager not available".to_string(), is_error: true }),
        };

        match manager.spawn_task(command.clone(), label.clone(), timeout_secs).await {
            Ok(id) => {
                let display_label = label.as_deref().unwrap_or(&command);
                let display_label = if display_label.len() > 40 { &display_label[..40] } else { display_label };
                Ok(ToolResult {
                    content: format!("Task spawned: id={id} label=\"{display_label}\""),
                    is_error: false,
                })
            }
            Err(e) => Ok(ToolResult { content: e, is_error: true }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_manager::TaskManager;
    use std::sync::Arc;

    fn make_ctx_with_manager(dir: &std::path::Path) -> ToolContext {
        use animus_vectorfs::store::MmapVectorStore;
        use animus_embed::synthetic::SyntheticEmbedding;
        let store_dir = dir.join("vectorfs");
        std::fs::create_dir_all(&store_dir).unwrap();
        let store = Arc::new(MmapVectorStore::open(&store_dir, 4).unwrap());
        let embedder = Arc::new(SyntheticEmbedding::new(4));
        let (tx, _rx) = tokio::sync::mpsc::channel(10);
        let mgr = TaskManager::new(tx, dir.to_path_buf(), 5);
        ToolContext {
            data_dir: dir.to_path_buf(),
            store: store as Arc<dyn animus_vectorfs::VectorStore>,
            embedder: embedder as Arc<dyn animus_core::EmbeddingService>,
            signal_tx: None,
            autonomy_tx: None,
            active_telegram_chat_id: Arc::new(parking_lot::Mutex::new(None)),
            watcher_registry: None,
            task_manager: Some(mgr),
        }
    }

    #[tokio::test]
    async fn missing_command_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = make_ctx_with_manager(tmp.path());
        let result = SpawnTaskTool.execute(serde_json::json!({}), &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("missing"));
    }

    #[tokio::test]
    async fn valid_command_returns_task_id() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = make_ctx_with_manager(tmp.path());
        let result = SpawnTaskTool.execute(
            serde_json::json!({"command": "echo hello"}),
            &ctx
        ).await.unwrap();
        assert!(!result.is_error, "unexpected error: {}", result.content);
        assert!(result.content.contains("id="), "expected task id: {}", result.content);
    }

    #[tokio::test]
    async fn no_manager_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        use animus_vectorfs::store::MmapVectorStore;
        use animus_embed::synthetic::SyntheticEmbedding;
        let store_dir = tmp.path().join("vectorfs");
        std::fs::create_dir_all(&store_dir).unwrap();
        let store = Arc::new(MmapVectorStore::open(&store_dir, 4).unwrap());
        let embedder = Arc::new(SyntheticEmbedding::new(4));
        let ctx = ToolContext {
            data_dir: tmp.path().to_path_buf(),
            store: store as Arc<dyn animus_vectorfs::VectorStore>,
            embedder: embedder as Arc<dyn animus_core::EmbeddingService>,
            signal_tx: None,
            autonomy_tx: None,
            active_telegram_chat_id: Arc::new(parking_lot::Mutex::new(None)),
            watcher_registry: None,
            task_manager: None,
        };
        let result = SpawnTaskTool.execute(
            serde_json::json!({"command": "echo hi"}),
            &ctx
        ).await.unwrap();
        assert!(result.is_error);
    }
}
```

- [ ] **Step 2: Run to verify fail, then implement** (the impl above is already the full implementation — no stub needed since tools are simple)

```bash
cargo test -p animus-cortex tools::spawn_task 2>&1 | head -20
```

Expected: compilation errors until Task 4 wiring is done. Proceed to Task 4, then return here.

### task_status.rs

```rust
// crates/animus-cortex/src/tools/task_status.rs
use crate::telos::Autonomy;
use crate::task_manager::TaskState;
use super::{Tool, ToolResult, ToolContext};

pub struct TaskStatusTool;

#[async_trait::async_trait]
impl Tool for TaskStatusTool {
    fn name(&self) -> &str { "task_status" }
    fn description(&self) -> &str {
        "List all background tasks or check a specific one. Omit task_id to list all."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "description": "ID of task to inspect (omit to list all)" }
            },
            "required": []
        })
    }
    fn required_autonomy(&self) -> Autonomy { Autonomy::Inform }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let manager = match &ctx.task_manager {
            Some(m) => m,
            None => return Ok(ToolResult { content: "Task manager not available".to_string(), is_error: true }),
        };

        if let Some(id) = params["task_id"].as_str() {
            return match manager.get_record(id) {
                None => Ok(ToolResult { content: format!("Unknown task id: {id}"), is_error: true }),
                Some(rec) => {
                    let now = chrono::Utc::now();
                    let end = rec.finished_at.unwrap_or(now);
                    let secs = (end - rec.spawned_at).num_seconds();
                    let runtime = format!("{:02}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60);
                    let exit = rec.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "—".to_string());
                    Ok(ToolResult {
                        content: format!(
                            "ID: {}\nLabel: {}\nState: {:?}\nRuntime: {}\nExit: {}\nLog: {}",
                            rec.id, rec.label, rec.state, runtime, exit, rec.log_path.display()
                        ),
                        is_error: false,
                    })
                }
            };
        }

        let records = manager.list_all();
        if records.is_empty() {
            return Ok(ToolResult { content: "No tasks.".to_string(), is_error: false });
        }

        let now = chrono::Utc::now();
        let header = format!("{:<10} {:<32} {:<12} {:<10} EXIT", "ID", "LABEL", "STATE", "RUNTIME");
        let mut lines = vec![header];
        for rec in &records {
            let end = rec.finished_at.unwrap_or(now);
            let secs = (end - rec.spawned_at).num_seconds();
            let runtime = format!("{:02}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60);
            let exit = rec.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "—".to_string());
            let label: &str = if rec.label.len() > 32 { &rec.label[..32] } else { &rec.label };
            lines.push(format!(
                "{:<10} {:<32} {:<12} {:<10} {}",
                rec.id, label, format!("{:?}", rec.state), runtime, exit
            ));
        }
        Ok(ToolResult { content: lines.join("\n"), is_error: false })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_manager::TaskManager;
    use std::sync::Arc;

    fn make_ctx(dir: &std::path::Path) -> ToolContext {
        use animus_vectorfs::store::MmapVectorStore;
        use animus_embed::synthetic::SyntheticEmbedding;
        let store_dir = dir.join("vectorfs");
        std::fs::create_dir_all(&store_dir).unwrap();
        let store = Arc::new(MmapVectorStore::open(&store_dir, 4).unwrap());
        let embedder = Arc::new(SyntheticEmbedding::new(4));
        let (tx, _rx) = tokio::sync::mpsc::channel(10);
        let mgr = TaskManager::new(tx, dir.to_path_buf(), 5);
        ToolContext {
            data_dir: dir.to_path_buf(),
            store: store as Arc<dyn animus_vectorfs::VectorStore>,
            embedder: embedder as Arc<dyn animus_core::EmbeddingService>,
            signal_tx: None,
            autonomy_tx: None,
            active_telegram_chat_id: Arc::new(parking_lot::Mutex::new(None)),
            watcher_registry: None,
            task_manager: Some(mgr),
        }
    }

    #[tokio::test]
    async fn no_tasks_returns_no_tasks_message() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = make_ctx(tmp.path());
        let result = TaskStatusTool.execute(serde_json::json!({}), &ctx).await.unwrap();
        assert!(!result.is_error);
        assert_eq!(result.content, "No tasks.");
    }

    #[tokio::test]
    async fn unknown_task_id_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = make_ctx(tmp.path());
        let result = TaskStatusTool.execute(
            serde_json::json!({"task_id": "notexist"}),
            &ctx
        ).await.unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("Unknown task id"));
    }

    #[tokio::test]
    async fn lists_tasks_after_spawn() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = make_ctx(tmp.path());
        // Spawn a task (will complete quickly)
        let spawn_result = crate::tools::spawn_task::SpawnTaskTool.execute(
            serde_json::json!({"command": "echo status_test", "label": "status_test"}),
            &ctx
        ).await.unwrap();
        assert!(!spawn_result.is_error);

        // List tasks — should show at least one row
        let list_result = TaskStatusTool.execute(serde_json::json!({}), &ctx).await.unwrap();
        assert!(!list_result.is_error);
        assert!(list_result.content.contains("status_test") || list_result.content.contains("ID"));
    }
}
```

(Fill in test bodies following the same `make_ctx_with_manager` helper pattern from spawn_task tests.)

### task_output.rs

```rust
// crates/animus-cortex/src/tools/task_output.rs
use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

const MAX_OUTPUT_BYTES: usize = 1_048_576; // 1 MB

pub struct TaskOutputTool;

#[async_trait::async_trait]
impl Tool for TaskOutputTool {
    fn name(&self) -> &str { "task_output" }
    fn description(&self) -> &str {
        "Read the stdout+stderr log of a background task (last 1MB). Works for running and finished tasks."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "description": "Task ID to read output for" }
            },
            "required": ["task_id"]
        })
    }
    fn required_autonomy(&self) -> Autonomy { Autonomy::Inform }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let id = match params["task_id"].as_str() {
            Some(id) => id,
            None => return Ok(ToolResult { content: "missing 'task_id' parameter".to_string(), is_error: true }),
        };
        let manager = match &ctx.task_manager {
            Some(m) => m,
            None => return Ok(ToolResult { content: "Task manager not available".to_string(), is_error: true }),
        };
        let record = match manager.get_record(id) {
            None => return Ok(ToolResult { content: format!("Unknown task id: {id}"), is_error: true }),
            Some(r) => r,
        };
        match tokio::fs::read(&record.log_path).await {
            Err(_) => Ok(ToolResult { content: "(no output yet)".to_string(), is_error: false }),
            Ok(bytes) => {
                let content = if bytes.len() > MAX_OUTPUT_BYTES {
                    let offset = bytes.len() - MAX_OUTPUT_BYTES;
                    format!("[...truncated, showing last 1MB]\n{}", String::from_utf8_lossy(&bytes[offset..]))
                } else {
                    String::from_utf8_lossy(&bytes).into_owned()
                };
                Ok(ToolResult { content, is_error: false })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_manager::TaskManager;
    use std::sync::Arc;

    fn make_ctx_with_channel(dir: &std::path::Path) -> (ToolContext, tokio::sync::mpsc::Receiver<animus_core::threading::Signal>) {
        use animus_vectorfs::store::MmapVectorStore;
        use animus_embed::synthetic::SyntheticEmbedding;
        let store_dir = dir.join("vectorfs");
        std::fs::create_dir_all(&store_dir).unwrap();
        let store = Arc::new(MmapVectorStore::open(&store_dir, 4).unwrap());
        let embedder = Arc::new(SyntheticEmbedding::new(4));
        let (tx, rx) = tokio::sync::mpsc::channel(10);
        let mgr = TaskManager::new(tx, dir.to_path_buf(), 5);
        let ctx = ToolContext {
            data_dir: dir.to_path_buf(),
            store: store as Arc<dyn animus_vectorfs::VectorStore>,
            embedder: embedder as Arc<dyn animus_core::EmbeddingService>,
            signal_tx: None,
            autonomy_tx: None,
            active_telegram_chat_id: Arc::new(parking_lot::Mutex::new(None)),
            watcher_registry: None,
            task_manager: Some(mgr),
        };
        (ctx, rx)
    }

    #[tokio::test]
    async fn missing_task_id_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, _rx) = make_ctx_with_channel(tmp.path());
        let result = TaskOutputTool.execute(serde_json::json!({}), &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("missing"));
    }

    #[tokio::test]
    async fn unknown_task_id_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, _rx) = make_ctx_with_channel(tmp.path());
        let result = TaskOutputTool.execute(
            serde_json::json!({"task_id": "notexist"}),
            &ctx
        ).await.unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("Unknown task id"));
    }

    #[tokio::test]
    async fn returns_log_content_for_completed_task() {
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, mut rx) = make_ctx_with_channel(tmp.path());

        let spawn_result = crate::tools::spawn_task::SpawnTaskTool.execute(
            serde_json::json!({"command": "echo hello_output_test"}),
            &ctx
        ).await.unwrap();
        assert!(!spawn_result.is_error);
        // Extract task_id from "Task spawned: id=XXXXXXXX"
        let id = spawn_result.content.split("id=").nth(1).unwrap().trim().to_string();

        // Wait for completion signal
        tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await.expect("signal timeout").expect("channel closed");

        let output_result = TaskOutputTool.execute(
            serde_json::json!({"task_id": id}),
            &ctx
        ).await.unwrap();
        assert!(!output_result.is_error, "{}", output_result.content);
        assert!(output_result.content.contains("hello_output_test"),
            "expected output in log: {}", output_result.content);
    }
}
```

### task_cancel.rs

```rust
// crates/animus-cortex/src/tools/task_cancel.rs
use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

pub struct TaskCancelTool;

#[async_trait::async_trait]
impl Tool for TaskCancelTool {
    fn name(&self) -> &str { "task_cancel" }
    fn description(&self) -> &str { "Cancel a running background task by ID." }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "description": "Task ID to cancel" }
            },
            "required": ["task_id"]
        })
    }
    fn required_autonomy(&self) -> Autonomy { Autonomy::Act }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let id = match params["task_id"].as_str() {
            Some(id) => id,
            None => return Ok(ToolResult { content: "missing 'task_id' parameter".to_string(), is_error: true }),
        };
        let manager = match &ctx.task_manager {
            Some(m) => m,
            None => return Ok(ToolResult { content: "Task manager not available".to_string(), is_error: true }),
        };
        match manager.cancel_task(id).await {
            Ok(msg) => Ok(ToolResult { content: msg, is_error: false }),
            Err(e) => Ok(ToolResult { content: e, is_error: true }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_manager::TaskManager;
    use std::sync::Arc;

    fn make_ctx_with_channel(dir: &std::path::Path) -> (ToolContext, tokio::sync::mpsc::Receiver<animus_core::threading::Signal>) {
        use animus_vectorfs::store::MmapVectorStore;
        use animus_embed::synthetic::SyntheticEmbedding;
        let store_dir = dir.join("vectorfs");
        std::fs::create_dir_all(&store_dir).unwrap();
        let store = Arc::new(MmapVectorStore::open(&store_dir, 4).unwrap());
        let embedder = Arc::new(SyntheticEmbedding::new(4));
        let (tx, rx) = tokio::sync::mpsc::channel(10);
        let mgr = TaskManager::new(tx, dir.to_path_buf(), 5);
        let ctx = ToolContext {
            data_dir: dir.to_path_buf(),
            store: store as Arc<dyn animus_vectorfs::VectorStore>,
            embedder: embedder as Arc<dyn animus_core::EmbeddingService>,
            signal_tx: None,
            autonomy_tx: None,
            active_telegram_chat_id: Arc::new(parking_lot::Mutex::new(None)),
            watcher_registry: None,
            task_manager: Some(mgr),
        };
        (ctx, rx)
    }

    #[tokio::test]
    async fn missing_task_id_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, _rx) = make_ctx_with_channel(tmp.path());
        let result = TaskCancelTool.execute(serde_json::json!({}), &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("missing"));
    }

    #[tokio::test]
    async fn unknown_task_id_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, _rx) = make_ctx_with_channel(tmp.path());
        let result = TaskCancelTool.execute(
            serde_json::json!({"task_id": "notexist"}),
            &ctx
        ).await.unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("Unknown task id"));
    }

    #[tokio::test]
    async fn nonrunning_task_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, mut rx) = make_ctx_with_channel(tmp.path());

        // Spawn a quick task
        let spawn_result = crate::tools::spawn_task::SpawnTaskTool.execute(
            serde_json::json!({"command": "echo cancel_test"}),
            &ctx
        ).await.unwrap();
        let id = spawn_result.content.split("id=").nth(1).unwrap().trim().to_string();

        // Wait for it to complete
        tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await.expect("timeout").expect("closed");

        // Now cancel the already-completed task — must return error
        let result = TaskCancelTool.execute(
            serde_json::json!({"task_id": id}),
            &ctx
        ).await.unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("not running"), "{}", result.content);
    }
}
```

- [ ] **Step 3: Run all tool tests**

```bash
cargo test -p animus-cortex "tools::spawn_task\|tools::task_status\|tools::task_output\|tools::task_cancel" 2>&1
```

Expected: all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/animus-cortex/src/tools/spawn_task.rs \
        crates/animus-cortex/src/tools/task_status.rs \
        crates/animus-cortex/src/tools/task_output.rs \
        crates/animus-cortex/src/tools/task_cancel.rs
git commit -m "feat(task-manager): add spawn_task, task_status, task_output, task_cancel tools"
```

---

## Task 4: Cortex Wiring — lib.rs and tools/mod.rs

**Files:**
- Modify: `crates/animus-cortex/src/tools/mod.rs`
- Modify: `crates/animus-cortex/src/lib.rs`
- Modify: `crates/animus-tests/tests/integration/tool_use.rs`

- [ ] **Step 1: Add pub mod declarations in tools/mod.rs**

After `pub mod manage_watcher;` add:
```rust
pub mod spawn_task;
pub mod task_status;
pub mod task_output;
pub mod task_cancel;
```

- [ ] **Step 2: Add task_manager to ToolContext in tools/mod.rs**

In `tools/mod.rs`, add this import at the top:
```rust
use crate::task_manager::TaskManager;
```

In the `ToolContext` struct, after `watcher_registry`:
```rust
/// Task manager for background process execution.
pub task_manager: Option<TaskManager>,
```

- [ ] **Step 3: Add pub mod and re-exports to lib.rs**

In `crates/animus-cortex/src/lib.rs`, after `pub mod watcher;`:
```rust
pub mod task_manager;
```

After `pub use watcher::{Watcher, WatcherConfig, WatcherEvent, WatcherRegistry};`:
```rust
pub use task_manager::{TaskManager, TaskRecord, TaskState, new_task_id};
```

- [ ] **Step 4: Fix integration test helper**

In `crates/animus-tests/tests/integration/tool_use.rs`, in the `test_ctx` function, add `task_manager: None,` after `watcher_registry: None,`.

- [ ] **Step 5: Build to confirm compilation**

```bash
cargo build -p animus-cortex -p animus-tests 2>&1 | grep "^error"
```

Expected: no errors.

- [ ] **Step 6: Run full test suite**

```bash
cargo test --workspace 2>&1 | grep -E "^test result"
```

Expected: all test results show `ok. N passed; 0 failed`.

- [ ] **Step 7: Commit**

```bash
git add crates/animus-cortex/src/lib.rs \
        crates/animus-cortex/src/tools/mod.rs \
        crates/animus-tests/tests/integration/tool_use.rs
git commit -m "feat(task-manager): wire TaskManager into ToolContext and cortex lib exports"
```

---

## Task 5: Runtime Wiring — main.rs

**Files:**
- Modify: `crates/animus-runtime/src/main.rs`

There are 7 changes to make. Read the file before starting. Each change is small and localized.

### Change 1: Import TaskManager and tools

At the top of `main.rs`, after the `use animus_cortex::tools::{...}` line, add:
```rust
use animus_cortex::TaskManager;
```

### Change 2: Construct TaskManager (after WatcherRegistry construction, ~line 234)

After the `watcher_registry.start();` line:
```rust
// ── Task Manager ──────────────────────────────────────────────────────────────
let task_manager = TaskManager::new(
    signal_tx.clone(),
    data_dir.clone(),
    5, // max concurrent tasks
);
tracing::info!("Task manager initialized");
```

### Change 3: Add to ToolContext

In the `ToolContext { ... }` construction, after `watcher_registry: Some(watcher_registry.clone()),`:
```rust
task_manager: Some(task_manager.clone()),
```

### Change 4: Register four new tools

After `reg.register(Box::new(animus_cortex::tools::manage_watcher::ManageWatcherTool));` add:
```rust
reg.register(Box::new(animus_cortex::tools::spawn_task::SpawnTaskTool));
reg.register(Box::new(animus_cortex::tools::task_status::TaskStatusTool));
reg.register(Box::new(animus_cortex::tools::task_output::TaskOutputTool));
reg.register(Box::new(animus_cortex::tools::task_cancel::TaskCancelTool));
```

### Change 5: Update system prompt

In `DEFAULT_SYSTEM_PROMPT`, after the `manage_watcher` line in the `## Your Tools` section:
```
- `spawn_task(command, label?, timeout_secs?)` — Spawn a long-running process in background. Returns task_id immediately. You get a Signal on completion.
- `task_status(task_id?)` — Check status of all tasks or a specific one.
- `task_output(task_id)` — Read the stdout+stderr log of a task (last 1MB).
- `task_cancel(task_id)` — Kill a running task.
```

Add `/task` to the User Commands line:
```
/goals /remember /forget /status /threads /thread /sleep /wake /watch /task /quit
```

### Change 6: Add task_manager to CommandContext struct

In the `CommandContext<'a>` struct definition, after `watcher_registry`:
```rust
task_manager: &'a animus_cortex::TaskManager,
```

### Change 7: Wire task_manager into CommandContext construction and add /task handler

In the `CommandContext { ... }` construction (inside the slash command handler), after `watcher_registry: &watcher_registry,`:
```rust
task_manager: &task_manager,
```

Add `/task` handler in `handle_command`, BEFORE the existing `"/watch"` match arms:
```rust
"/task" if matches!(arg.split_whitespace().next(), Some("list" | "cancel")) => {
    let parts: Vec<&str> = arg.splitn(2, ' ').collect();
    match parts[0] {
        "list" => {
            let records = ctx.task_manager.list_all();
            if records.is_empty() {
                ctx.interface.display_status("No tasks.");
            } else {
                let now = chrono::Utc::now();
                let header = format!("{:<10} {:<32} {:<12} {:<10} EXIT", "ID", "LABEL", "STATE", "RUNTIME");
                let mut lines = vec![header];
                for rec in &records {
                    let end = rec.finished_at.unwrap_or(now);
                    let secs = (end - rec.spawned_at).num_seconds();
                    let runtime = format!("{:02}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60);
                    let exit = rec.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "—".to_string());
                    let label: &str = if rec.label.len() > 32 { &rec.label[..32] } else { &rec.label };
                    lines.push(format!("{:<10} {:<32} {:<12} {:<10} {}", rec.id, label, format!("{:?}", rec.state), runtime, exit));
                }
                ctx.interface.display_status(&lines.join("\n"));
            }
        }
        "cancel" => {
            let id = match parts.get(1) {
                Some(id) => *id,
                None => {
                    ctx.interface.display_status("Usage: /task cancel <id>");
                    return Ok(CommandResult::Continue);
                }
            };
            match ctx.task_manager.cancel_task(id).await {
                Ok(msg) => ctx.interface.display_status(&msg),
                Err(e) => ctx.interface.display_status(&format!("Error: {e}")),
            }
        }
        _ => ctx.interface.display_status("Unknown /task subcommand. Use: list, cancel <id>"),
    }
}
```

- [ ] **Step 1: Make all 7 changes to main.rs**

- [ ] **Step 2: Build runtime**

```bash
cargo build -p animus-runtime 2>&1 | grep "^error"
```

Expected: no errors.

- [ ] **Step 3: Run full workspace test suite**

```bash
cargo test --workspace 2>&1 | grep -E "^test result"
```

Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add crates/animus-runtime/src/main.rs
git commit -m "feat(task-manager): wire TaskManager into runtime, register tools, add /task command"
```

---

## Task 6: Verify and Finishing

- [ ] **Step 1: Full workspace test suite**

```bash
cargo test --workspace 2>&1 | grep -E "^test result|FAILED"
```

Expected: all `ok`, no `FAILED`.

- [ ] **Step 2: Release build**

```bash
cargo build --release -p animus-runtime 2>&1 | grep "^error"
```

Expected: clean.

- [ ] **Step 3: Invoke finishing-a-development-branch skill**

Use `superpowers:finishing-a-development-branch` to decide how to integrate: push and create a PR to master.
