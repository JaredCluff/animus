# Async Task Execution — Design Spec

**Date:** 2026-03-23
**Status:** Draft
**Crate:** `animus-cortex`

---

## 1. Purpose

`shell_exec` kills any process that runs longer than 30 seconds. Animus cannot run builds, test suites, data-processing scripts, or any other long-running operation without hitting this wall.

This spec defines a `TaskManager` subsystem that lets Animus spawn a shell process, receive a `task_id` immediately, continue reasoning, and get a Signal when the process finishes. Output is stored to disk; Animus reads it on demand.

---

## 2. Architecture

`TaskManager` lives in `crates/animus-cortex/src/task_manager.rs`. It follows the `WatcherRegistry` pattern: an `Arc`-wrapped struct cloned cheaply into `ToolContext` and into each spawned background future.

```
TaskManager
  ├── Arc<parking_lot::Mutex<TaskManagerState>>
  │     ├── records: HashMap<TaskId, TaskRecord>              (persisted)
  │     ├── handles: HashMap<TaskId, tokio::task::AbortHandle> // abort the background future, which kills the child
  │     └── max_concurrent: usize                             (default: 5)
  ├── signal_tx: mpsc::Sender<Signal>   // stored at construction; cloned into each background future
  └── data_dir: PathBuf                 // $ANIMUS_DATA_DIR — for index.json and log paths
```

`TaskManager::new(signal_tx, data_dir, max_concurrent)` — mirrors `WatcherRegistry::new`. `signal_tx` is stored on `TaskManager` at construction so background futures can capture it by clone without requiring `ToolContext` access. A `source_thread: ThreadId` is also stored at construction (use `ThreadId::new("task-manager")` or similar); Signals use `source_thread` as source and `ThreadId::default()` as target, matching the WatcherRegistry pattern.

**Lifecycle:**

```
spawn_task called
  → count Running records; reject if at cap
  → generate TaskId
  → write TaskRecord { state: Running, ... } to state
  → tokio::spawn background future:
        → tokio::process::Command::new("sh").arg("-c").arg(&command)
        → pipe stdout + stderr to log file ($ANIMUS_DATA_DIR/tasks/<id>.log)
        → on exit: update record (Completed/Failed), persist index, fire Signal
  → return task_id to LLM

task_cancel called
  → look up ChildHandle
  → child.kill().await
  → write CANCELLED to log, update record, persist index
```

No separate poll loop. Each spawned task is a self-contained tokio future that manages its own cleanup.

`TaskManager` is injected into `ToolContext`:

```rust
pub task_manager: Option<TaskManager>,
```

Constructed in `animus-runtime/src/main.rs` alongside `WatcherRegistry`, passed into `ToolContext` and registered tools.

---

## 3. Data Model

### TaskId

Short random alphanumeric string, 8 characters (e.g. `a3f9bc12`). Human-readable in Signal summaries; collision-resistant at Animus's scale.

### TaskState

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    Running,
    Completed,   // exit code 0
    Failed,      // exit code non-zero
    Cancelled,   // killed via task_cancel
    Interrupted, // was Running at shutdown; output may be partial
}
```

### TaskRecord

Persisted to `$ANIMUS_DATA_DIR/tasks/index.json`:

```rust
pub struct TaskRecord {
    pub id: TaskId,
    pub label: String,           // user-provided or first 40 chars of command
    pub command: String,
    pub state: TaskState,
    pub exit_code: Option<i32>,
    pub spawned_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub log_path: PathBuf,       // $ANIMUS_DATA_DIR/tasks/<id>.log
}
```

`ChildHandle` is **not** persisted. On restart, any records with `state: Running` are immediately marked `Interrupted`.

### Log Files

`$ANIMUS_DATA_DIR/tasks/<id>.log` — stdout and stderr written as received, interleaved. Capped at **1 MB** via tail-truncation on read (the file may grow larger; `task_output` returns only the last 1 MB).

### Index Retention

Keep the **last 50 completed/failed/cancelled/interrupted** records. Evict oldest on overflow. Running records are never evicted.

---

## 4. Tools

### `spawn_task`

```json
{
  "command": "cargo test --workspace",
  "label": "workspace tests",        // optional; defaults to first 40 chars of command
  "timeout_secs": 600                // optional; no default timeout (runs to completion)
}
```

Returns on success:
```
Task spawned: id=a3f9bc12 label="workspace tests"
```

Returns on cap exceeded:
```
Task cap reached (5 running). Cancel a task or wait for one to complete.
Currently running: [a3f9bc12 "workspace tests" 00:02:14, ...]
```

`required_autonomy: Autonomy::Act`

### `task_status`

```json
{
  "task_id": "a3f9bc12"   // optional; omit to list all active + recent
}
```

Returns a table of records:

```
ID        LABEL                STATE       RUNTIME    EXIT
a3f9bc12  workspace tests      Running     00:02:14   —
b7d21e44  build release        Completed   00:08:33   0
c9f03a11  integration tests    Failed      00:01:22   1
```

`required_autonomy: Autonomy::Inform`

### `task_output`

```json
{
  "task_id": "a3f9bc12"
}
```

Returns the last 1 MB of the task's log file (stdout + stderr interleaved). Works for any terminal state and for Running tasks (reads whatever has been written so far).

Returns on success:
```
[log content — last 1MB of $ANIMUS_DATA_DIR/tasks/a3f9bc12.log]
```

Error if task not found:
```
Unknown task id: a3f9bc12
```

`required_autonomy: Autonomy::Inform`

### `task_cancel`

```json
{
  "task_id": "a3f9bc12"
}
```

Kills the child process, writes `[CANCELLED]` to the log, marks record `Cancelled`.

Returns:
```
Task a3f9bc12 ("workspace tests") cancelled.
```

Error if task not found or already finished:
```
Task a3f9bc12 is not running (state: Completed).
```

`required_autonomy: Autonomy::Act`

---

## 5. Completion Signal

When a task exits (any terminal state), the background future fires a Signal via `signal_tx`:

```rust
Signal {
    priority: if exit_code == Some(0) { SignalPriority::Normal } else { SignalPriority::Urgent },
    summary: "Task 'workspace tests' [a3f9bc12] completed — exit 0. Read output: task_output(\"a3f9bc12\")",
    // or: "Task 'build release' [b7d21e44] FAILED — exit 1. Read output: task_output(\"b7d21e44\")"
    segment_refs: vec![],
    ...
}
```

Failed tasks fire `SignalPriority::Urgent` to surface errors promptly.

---

## 6. Persistence

**At spawn:** `TaskRecord` written to `index.json` before the tokio task is spawned (so a crash between spawn and first write doesn't lose the record).

**On completion/cancel:** `index.json` updated atomically (write `.tmp`, rename).

**At startup:** `index.json` loaded; any `Running` records marked `Interrupted`. `ChildHandle` map starts empty.

**Index location:** `$ANIMUS_DATA_DIR/tasks/index.json`
**Log location:** `$ANIMUS_DATA_DIR/tasks/<id>.log`

---

## 7. Error Handling

| Scenario | Behavior |
|----------|----------|
| Cap exceeded | Error returned to LLM with list of running tasks |
| Command not found / spawn fails | Record immediately marked Failed; Signal fired |
| Log write fails | Process still runs; log_path noted as partial in record |
| `task_cancel` on non-running task | Error returned; no state change |
| `task_cancel` with unknown id | Error returned |
| Timeout reached (`timeout_secs`) | Child killed, record marked `Cancelled` with note "timed out after Ns" |
| Signal channel closed | Completion logged to tracing; no panic |

---

## 8. System Prompt & Slash Commands

Add to system prompt tools list:
```
- spawn_task(command, label?, timeout_secs?) — Run a long process in background. Returns task_id immediately.
- task_status(task_id?) — Check status of one or all tasks.
- task_output(task_id) — Read the log output of a task (last 1MB).
- task_cancel(task_id) — Kill a running task.
```

Add `/task` to user commands:
```
/task list|cancel <id>
```

`/task list` → calls `task_status()` and displays the table.
`/task cancel <id>` → calls `task_cancel` for the given id.

---

## 9. Files Changed

| File | Change |
|------|--------|
| `crates/animus-cortex/src/task_manager.rs` | New — TaskManager, TaskRecord, TaskState, TaskId |
| `crates/animus-cortex/src/tools/spawn_task.rs` | New |
| `crates/animus-cortex/src/tools/task_status.rs` | New |
| `crates/animus-cortex/src/tools/task_output.rs` | New |
| `crates/animus-cortex/src/tools/task_cancel.rs` | New |
| `crates/animus-cortex/src/tools/mod.rs` | Add `task_manager: Option<TaskManager>` to ToolContext; pub mod for new tools |
| `crates/animus-cortex/src/lib.rs` | pub mod task_manager; pub use TaskManager |
| `crates/animus-runtime/src/main.rs` | Construct TaskManager; wire into ToolContext; register tools; update system prompt + commands |
| `crates/animus-tests/tests/integration/tool_use.rs` | Add `task_manager: None` to test_ctx helper |

---

## 10. Testing Strategy

Unit tests in `task_manager.rs`:
- TaskId uniqueness (generate 1000, assert no duplicates)
- Serde roundtrip for TaskRecord
- State transitions: Running → Completed, Running → Failed, Running → Cancelled, Running → Interrupted
- Index load with Running records → all marked Interrupted
- Index retention evicts oldest when > 50

Unit tests in each tool file:
- `spawn_task`: cap enforcement returns error with running list
- `spawn_task`: missing command parameter
- `task_status`: no tasks returns empty table
- `task_status`: unknown task_id returns error
- `task_cancel`: non-running task returns error
- `task_cancel`: unknown task_id returns error

Integration tests (using a real tokio runtime; each test creates a `(signal_tx, signal_rx)` pair and passes `signal_tx` to `TaskManager::new`, keeping `signal_rx` in scope for Signal assertions):
- Spawn `echo hello` → assert record state is Completed, log file contains "hello", and `signal_rx.recv()` returns a Signal whose summary contains the task_id.
- Spawn `exit 1` → assert record state is Failed, Signal priority is Urgent.
- Spawn `sleep 60` (or equivalent), call `task_cancel` → assert record state is Cancelled, `AbortHandle` is removed from handles map.
