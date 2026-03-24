# Phase 4 — Multi-Threading + Signaling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable the AILF to handle multiple concurrent tasks without context leakage — thread scheduler, context isolation, inter-thread signaling, background threads.

**Architecture:** A `ThreadScheduler` manages a collection of `ReasoningThread` instances, each with its own status (Active/Suspended/Background/Completed). Only one thread is Active at a time (the human-facing one). Background threads can pursue goals autonomously. Threads communicate exclusively through `Signal` structs — no direct context sharing. The runtime exposes thread management via slash commands.

**Tech Stack:** Rust, tokio, existing animus-cortex crate (ReasoningThread), animus-core types (ThreadId, GoalId).

---

### Task 1: Core Threading Types

**Files:**
- Create: `crates/animus-core/src/threading.rs`
- Modify: `crates/animus-core/src/lib.rs`

Add shared types for thread management that other crates reference.

- [ ] **Step 1: Write the failing test**

Create `crates/animus-tests/tests/integration/threading_types.rs`:

```rust
use animus_core::threading::*;
use animus_core::{ThreadId, GoalId, SegmentId};

#[test]
fn thread_status_transitions() {
    let mut status = ThreadStatus::Active;
    assert!(status.can_transition_to(ThreadStatus::Suspended));
    assert!(status.can_transition_to(ThreadStatus::Background));
    assert!(status.can_transition_to(ThreadStatus::Completed));
    assert!(!status.can_transition_to(ThreadStatus::Active)); // already active
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
```

Add `mod threading_types;` to main.rs.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p animus-tests threading_types -- --nocapture`

- [ ] **Step 3: Implement threading types**

Create `crates/animus-core/src/threading.rs`:

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use crate::identity::{GoalId, SegmentId, ThreadId};

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
            (ThreadStatus::Completed, _) => false, // completed threads can't resume
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
```

Add `pub mod threading;` to lib.rs and re-export key types.

- [ ] **Step 4: Run test to verify it passes**

- [ ] **Step 5: Commit**

```bash
git commit -m "feat(core): add threading types — ThreadStatus, Signal, SignalPriority"
```

---

### Task 2: Extend ReasoningThread with Status and Signals

**Files:**
- Modify: `crates/animus-cortex/src/thread.rs`

Add thread status, signal inbox, priority, and signal handling to the existing ReasoningThread.

- [ ] **Step 1: Write the failing test**

Create `crates/animus-tests/tests/integration/threading_signals.rs`:

```rust
use animus_core::threading::*;
use animus_core::{GoalId, SegmentId, ThreadId};
use animus_cortex::thread::ReasoningThread;
use animus_embed::SyntheticEmbedding;
use animus_vectorfs::store::MmapVectorStore;
use std::sync::Arc;
use tempfile::TempDir;

fn make_thread(name: &str) -> (ReasoningThread<MmapVectorStore>, TempDir) {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(&dir.path().join("vfs"), 128).unwrap());
    let thread = ReasoningThread::new(name.to_string(), store, 8000, 128);
    (thread, dir)
}

#[test]
fn thread_starts_active() {
    let (thread, _dir) = make_thread("test");
    assert_eq!(thread.status(), ThreadStatus::Active);
}

#[test]
fn thread_can_be_suspended() {
    let (mut thread, _dir) = make_thread("test");
    assert!(thread.set_status(ThreadStatus::Suspended).is_ok());
    assert_eq!(thread.status(), ThreadStatus::Suspended);
}

#[test]
fn completed_thread_cannot_resume() {
    let (mut thread, _dir) = make_thread("test");
    thread.set_status(ThreadStatus::Completed).unwrap();
    assert!(thread.set_status(ThreadStatus::Active).is_err());
}

#[test]
fn signal_delivery() {
    let (mut thread, _dir) = make_thread("receiver");
    let signal = Signal {
        source_thread: ThreadId::new(),
        target_thread: thread.id,
        priority: SignalPriority::Normal,
        summary: "hello from thread A".to_string(),
        segment_refs: vec![],
        created: chrono::Utc::now(),
    };
    thread.deliver_signal(signal);
    assert_eq!(thread.pending_signals().len(), 1);
}

#[test]
fn signals_ordered_by_priority() {
    let (mut thread, _dir) = make_thread("receiver");

    let info_signal = Signal {
        source_thread: ThreadId::new(),
        target_thread: thread.id,
        priority: SignalPriority::Info,
        summary: "low priority".to_string(),
        segment_refs: vec![],
        created: chrono::Utc::now(),
    };
    let urgent_signal = Signal {
        source_thread: ThreadId::new(),
        target_thread: thread.id,
        priority: SignalPriority::Urgent,
        summary: "high priority".to_string(),
        segment_refs: vec![],
        created: chrono::Utc::now(),
    };

    thread.deliver_signal(info_signal);
    thread.deliver_signal(urgent_signal);

    let signals = thread.drain_signals();
    assert_eq!(signals.len(), 2);
    assert_eq!(signals[0].priority, SignalPriority::Urgent);
    assert_eq!(signals[1].priority, SignalPriority::Info);
}

#[test]
fn drain_signals_clears_inbox() {
    let (mut thread, _dir) = make_thread("receiver");
    let signal = Signal {
        source_thread: ThreadId::new(),
        target_thread: thread.id,
        priority: SignalPriority::Normal,
        summary: "test".to_string(),
        segment_refs: vec![],
        created: chrono::Utc::now(),
    };
    thread.deliver_signal(signal);
    let _ = thread.drain_signals();
    assert!(thread.pending_signals().is_empty());
}
```

Add `mod threading_signals;` to main.rs.

- [ ] **Step 2: Run test to verify it fails**

- [ ] **Step 3: Extend ReasoningThread**

Add to `ReasoningThread`:
- `status: ThreadStatus` field (default: Active)
- `priority: Priority` field (default: Normal)
- `pending_signals: Vec<Signal>` field
- `status()` getter
- `set_status(status) -> Result<()>` with transition validation
- `deliver_signal(signal)` method
- `pending_signals() -> &[Signal]` getter
- `drain_signals() -> Vec<Signal>` — returns signals sorted by priority (Urgent first), clears inbox

- [ ] **Step 4: Run test to verify it passes**

- [ ] **Step 5: Commit**

```bash
git commit -m "feat(cortex): extend ReasoningThread with status, priority, and signal inbox"
```

---

### Task 3: ThreadScheduler

**Files:**
- Create: `crates/animus-cortex/src/scheduler.rs`
- Modify: `crates/animus-cortex/src/lib.rs`

The scheduler manages multiple threads: creates, switches, suspends, resumes. Enforces that only one thread is Active at a time.

- [ ] **Step 1: Write the failing test**

Create `crates/animus-tests/tests/integration/threading_scheduler.rs`:

```rust
use animus_core::threading::*;
use animus_cortex::scheduler::ThreadScheduler;
use animus_vectorfs::store::MmapVectorStore;
use std::sync::Arc;
use tempfile::TempDir;

fn make_scheduler() -> (ThreadScheduler<MmapVectorStore>, TempDir) {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(&dir.path().join("vfs"), 128).unwrap());
    let scheduler = ThreadScheduler::new(store, 8000, 128);
    (scheduler, dir)
}

#[test]
fn scheduler_creates_initial_thread() {
    let (scheduler, _dir) = make_scheduler();
    // Scheduler starts with no threads — the runtime creates the initial "main" thread
    assert_eq!(scheduler.thread_count(), 0);
}

#[test]
fn create_and_list_threads() {
    let (mut scheduler, _dir) = make_scheduler();
    let id = scheduler.create_thread("main".to_string());
    assert_eq!(scheduler.thread_count(), 1);

    let threads = scheduler.list_threads();
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].0, id);
    assert_eq!(threads[0].1, "main");
    assert_eq!(threads[0].2, ThreadStatus::Active);
}

#[test]
fn switch_to_thread() {
    let (mut scheduler, _dir) = make_scheduler();
    let main_id = scheduler.create_thread("main".to_string());
    let work_id = scheduler.create_thread("work".to_string());

    // main should be active, work should be suspended (only one active at a time)
    assert_eq!(scheduler.active_thread_id(), Some(main_id));

    scheduler.switch_to(work_id).unwrap();
    assert_eq!(scheduler.active_thread_id(), Some(work_id));

    // main should now be suspended
    let threads = scheduler.list_threads();
    let main_thread = threads.iter().find(|(id, _, _)| *id == main_id).unwrap();
    assert_eq!(main_thread.2, ThreadStatus::Suspended);
}

#[test]
fn suspend_active_thread() {
    let (mut scheduler, _dir) = make_scheduler();
    let id = scheduler.create_thread("main".to_string());
    scheduler.suspend(id).unwrap();
    assert_eq!(scheduler.active_thread_id(), None);
}

#[test]
fn cannot_switch_to_completed_thread() {
    let (mut scheduler, _dir) = make_scheduler();
    let id = scheduler.create_thread("main".to_string());
    scheduler.complete(id).unwrap();
    assert!(scheduler.switch_to(id).is_err());
}

#[test]
fn signal_between_threads() {
    let (mut scheduler, _dir) = make_scheduler();
    let t1 = scheduler.create_thread("thread-1".to_string());
    let t2 = scheduler.create_thread("thread-2".to_string());

    scheduler.send_signal(t1, t2, SignalPriority::Normal, "hello".to_string(), vec![]).unwrap();

    let signals = scheduler.drain_signals(t2);
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].summary, "hello");
}
```

Add `mod threading_scheduler;` to main.rs.

- [ ] **Step 2: Run test to verify it fails**

- [ ] **Step 3: Implement ThreadScheduler**

Create `crates/animus-cortex/src/scheduler.rs`:

```rust
use animus_core::error::{AnimusError, Result};
use animus_core::identity::{SegmentId, ThreadId};
use animus_core::threading::*;
use animus_vectorfs::VectorStore;
use std::collections::HashMap;
use std::sync::Arc;

use crate::thread::ReasoningThread;

/// Manages multiple reasoning threads with scheduling and signal routing.
pub struct ThreadScheduler<S: VectorStore> {
    threads: HashMap<ThreadId, ReasoningThread<S>>,
    active_thread: Option<ThreadId>,
    store: Arc<S>,
    token_budget: usize,
    embedding_dim: usize,
}

impl<S: VectorStore> ThreadScheduler<S> {
    pub fn new(store: Arc<S>, token_budget: usize, embedding_dim: usize) -> Self {
        Self {
            threads: HashMap::new(),
            active_thread: None,
            store,
            token_budget,
            embedding_dim,
        }
    }

    /// Create a new thread. If no thread is active, this becomes the active thread.
    pub fn create_thread(&mut self, name: String) -> ThreadId {
        let mut thread = ReasoningThread::new(
            name,
            self.store.clone(),
            self.token_budget,
            self.embedding_dim,
        );

        if self.active_thread.is_none() {
            // First thread becomes active
        } else {
            // Additional threads start suspended
            let _ = thread.set_status(ThreadStatus::Suspended);
        }

        let id = thread.id;
        if self.active_thread.is_none() {
            self.active_thread = Some(id);
        }
        self.threads.insert(id, thread);
        id
    }

    /// Switch to a different thread. The current active thread becomes suspended.
    pub fn switch_to(&mut self, thread_id: ThreadId) -> Result<()> {
        let target = self.threads.get(&thread_id)
            .ok_or_else(|| AnimusError::Llm(format!("thread not found: {thread_id}")))?;
        if target.status() == ThreadStatus::Completed {
            return Err(AnimusError::Llm("cannot switch to a completed thread".to_string()));
        }

        // Suspend current active thread
        if let Some(current_id) = self.active_thread {
            if current_id != thread_id {
                if let Some(current) = self.threads.get_mut(&current_id) {
                    let _ = current.set_status(ThreadStatus::Suspended);
                }
            }
        }

        // Activate target thread
        if let Some(target) = self.threads.get_mut(&thread_id) {
            target.set_status(ThreadStatus::Active)?;
        }
        self.active_thread = Some(thread_id);
        Ok(())
    }

    /// Suspend a thread.
    pub fn suspend(&mut self, thread_id: ThreadId) -> Result<()> {
        let thread = self.threads.get_mut(&thread_id)
            .ok_or_else(|| AnimusError::Llm(format!("thread not found: {thread_id}")))?;
        thread.set_status(ThreadStatus::Suspended)?;
        if self.active_thread == Some(thread_id) {
            self.active_thread = None;
        }
        Ok(())
    }

    /// Mark a thread as completed.
    pub fn complete(&mut self, thread_id: ThreadId) -> Result<()> {
        let thread = self.threads.get_mut(&thread_id)
            .ok_or_else(|| AnimusError::Llm(format!("thread not found: {thread_id}")))?;
        thread.set_status(ThreadStatus::Completed)?;
        if self.active_thread == Some(thread_id) {
            self.active_thread = None;
        }
        Ok(())
    }

    /// Get the active thread ID.
    pub fn active_thread_id(&self) -> Option<ThreadId> {
        self.active_thread
    }

    /// Get a mutable reference to the active thread.
    pub fn active_thread_mut(&mut self) -> Option<&mut ReasoningThread<S>> {
        self.active_thread.and_then(|id| self.threads.get_mut(&id))
    }

    /// Get an immutable reference to the active thread.
    pub fn active_thread(&self) -> Option<&ReasoningThread<S>> {
        self.active_thread.and_then(|id| self.threads.get(&id))
    }

    /// List all threads: (id, name, status).
    pub fn list_threads(&self) -> Vec<(ThreadId, String, ThreadStatus)> {
        self.threads.values()
            .map(|t| (t.id, t.name.clone(), t.status()))
            .collect()
    }

    /// Number of threads.
    pub fn thread_count(&self) -> usize {
        self.threads.len()
    }

    /// Send a signal from one thread to another.
    pub fn send_signal(
        &mut self,
        source: ThreadId,
        target: ThreadId,
        priority: SignalPriority,
        summary: String,
        segment_refs: Vec<SegmentId>,
    ) -> Result<()> {
        if !self.threads.contains_key(&target) {
            return Err(AnimusError::Llm(format!("target thread not found: {target}")));
        }
        let signal = Signal {
            source_thread: source,
            target_thread: target,
            priority,
            summary,
            segment_refs,
            created: chrono::Utc::now(),
        };
        if let Some(thread) = self.threads.get_mut(&target) {
            thread.deliver_signal(signal);
        }
        Ok(())
    }

    /// Drain signals from a thread's inbox.
    pub fn drain_signals(&mut self, thread_id: ThreadId) -> Vec<Signal> {
        self.threads.get_mut(&thread_id)
            .map(|t| t.drain_signals())
            .unwrap_or_default()
    }
}
```

Add `pub mod scheduler;` to `crates/animus-cortex/src/lib.rs` and re-export key types.

- [ ] **Step 4: Run test to verify it passes**

- [ ] **Step 5: Commit**

```bash
git commit -m "feat(cortex): ThreadScheduler with multi-thread management and signal routing"
```

---

### Task 4: Runtime Integration — Thread Commands

**Files:**
- Modify: `crates/animus-runtime/src/main.rs`

Replace the single ReasoningThread with ThreadScheduler. Add thread management commands: `/threads`, `/thread new <name>`, `/thread switch <id>`, `/thread complete <id>`.

- [ ] **Step 1: Refactor runtime to use ThreadScheduler**

In `main.rs`:
- Replace `ReasoningThread::new(...)` with `ThreadScheduler::new(store, token_budget, dimensionality)`
- Call `scheduler.create_thread("main".to_string())` to create the initial thread
- In the conversation loop, use `scheduler.active_thread_mut()` to get the current thread
- Pass signals context into the system prompt

- [ ] **Step 2: Add thread commands**

```rust
"/threads" => {
    let threads = scheduler.list_threads();
    if threads.is_empty() {
        interface.display_status("No threads.");
    } else {
        for (id, name, status) in threads {
            let active = if Some(id) == scheduler.active_thread_id() { " *" } else { "" };
            interface.display_status(&format!(
                "[{}] {} ({:?}){}",
                id.0.to_string().get(..8).unwrap_or("?"),
                name,
                status,
                active,
            ));
        }
    }
}
"/thread" if arg.starts_with("new ") => {
    let name = arg.strip_prefix("new ").unwrap().trim();
    if name.is_empty() {
        interface.display_status("Usage: /thread new <name>");
    } else {
        let id = scheduler.create_thread(name.to_string());
        interface.display_status(&format!(
            "Thread created: {} ({})",
            name,
            id.0.to_string().get(..8).unwrap_or("?")
        ));
    }
}
"/thread" if arg.starts_with("switch ") => {
    let prefix = arg.strip_prefix("switch ").unwrap().trim();
    // Find thread by ID prefix
    let threads = scheduler.list_threads();
    let matches: Vec<_> = threads.iter()
        .filter(|(id, _, _)| id.0.to_string().starts_with(prefix))
        .collect();
    match matches.len() {
        0 => interface.display_status(&format!("No thread found matching '{prefix}'")),
        1 => {
            let id = matches[0].0;
            scheduler.switch_to(id)?;
            interface.display_status(&format!("Switched to thread: {}", matches[0].1));
        }
        n => interface.display_status(&format!("{n} threads match '{prefix}' — be more specific")),
    }
}
"/thread" if arg.starts_with("complete ") => {
    let prefix = arg.strip_prefix("complete ").unwrap().trim();
    let threads = scheduler.list_threads();
    let matches: Vec<_> = threads.iter()
        .filter(|(id, _, _)| id.0.to_string().starts_with(prefix))
        .collect();
    match matches.len() {
        0 => interface.display_status(&format!("No thread found matching '{prefix}'")),
        1 => {
            let id = matches[0].0;
            scheduler.complete(id)?;
            interface.display_status(&format!("Thread completed: {}", matches[0].1));
        }
        n => interface.display_status(&format!("{n} threads match '{prefix}' — be more specific")),
    }
}
```

- [ ] **Step 3: Update system prompt with signals context**

When building the system prompt, inject any pending signals from the active thread:

```rust
fn build_system_prompt(scheduler: &ThreadScheduler<MmapVectorStore>, goals: &GoalManager) -> String {
    let mut prompt = DEFAULT_SYSTEM_PROMPT.to_string();
    // ... goals ...
    // Inject signal summaries
    if let Some(thread) = scheduler.active_thread() {
        let signals = thread.pending_signals();
        if !signals.is_empty() {
            prompt.push_str("\n\n## Incoming Signals\n");
            for signal in signals {
                prompt.push_str(&format!(
                    "- [{:?}] from thread {}: {}\n",
                    signal.priority,
                    signal.source_thread.0.to_string().get(..8).unwrap_or("?"),
                    signal.summary,
                ));
            }
        }
    }
    prompt
}
```

- [ ] **Step 4: Update help text and system prompt**

Add thread commands to `/help` and update `DEFAULT_SYSTEM_PROMPT`.

- [ ] **Step 5: Verify build compiles**

Run: `cargo build -p animus-runtime`

- [ ] **Step 6: Run all tests**

Run: `cargo test --workspace`

- [ ] **Step 7: Commit**

```bash
git commit -m "feat(runtime): multi-thread support with /threads, /thread new, /thread switch commands"
```

---

### Task 5: Background Thread Goal Pursuit (Stub)

**Files:**
- Modify: `crates/animus-cortex/src/scheduler.rs`

Add the ability to mark a thread as Background with a bound goal. Background threads don't process human input — they are reserved for future autonomous goal pursuit. For V0.1, this is a status marker only.

- [ ] **Step 1: Write the failing test**

Add to `crates/animus-tests/tests/integration/threading_scheduler.rs`:

```rust
#[test]
fn set_thread_background() {
    let (mut scheduler, _dir) = make_scheduler();
    let id = scheduler.create_thread("worker".to_string());
    scheduler.set_background(id, GoalId::new()).unwrap();

    let threads = scheduler.list_threads();
    let worker = threads.iter().find(|(tid, _, _)| *tid == id).unwrap();
    assert_eq!(worker.2, ThreadStatus::Background);
}

#[test]
fn background_thread_cannot_be_switched_to() {
    let (mut scheduler, _dir) = make_scheduler();
    let main_id = scheduler.create_thread("main".to_string());
    let bg_id = scheduler.create_thread("background".to_string());
    scheduler.set_background(bg_id, GoalId::new()).unwrap();
    // Switching to a background thread should fail
    assert!(scheduler.switch_to(bg_id).is_err());
}
```

- [ ] **Step 2: Implement set_background**

Add to ThreadScheduler:

```rust
pub fn set_background(&mut self, thread_id: ThreadId, goal_id: GoalId) -> Result<()> {
    let thread = self.threads.get_mut(&thread_id)
        .ok_or_else(|| AnimusError::Llm(format!("thread not found: {thread_id}")))?;
    thread.set_status(ThreadStatus::Background)?;
    thread.bound_goals.push(goal_id);
    if self.active_thread == Some(thread_id) {
        self.active_thread = None;
    }
    Ok(())
}
```

Update `switch_to` to reject Background threads.

- [ ] **Step 3: Run tests**

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(cortex): background thread support with goal binding"
```

---

### Task 6: Signal Context Injection in Reasoning

**Files:**
- Modify: `crates/animus-cortex/src/thread.rs`

When a thread processes a turn, drain pending signals and include their summaries in the context sent to the LLM. This makes the AILF aware of inter-thread communication.

- [ ] **Step 1: Write the failing test**

Add to `crates/animus-tests/tests/integration/threading_signals.rs`:

```rust
#[tokio::test]
async fn signals_included_in_reasoning_context() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(MmapVectorStore::open(&dir.path().join("vfs"), 128).unwrap());
    let mut thread = ReasoningThread::new("test".to_string(), store, 8000, 128);

    // Deliver a signal
    let signal = Signal {
        source_thread: ThreadId::new(),
        target_thread: thread.id,
        priority: SignalPriority::Urgent,
        summary: "Important: build failed".to_string(),
        segment_refs: vec![],
        created: chrono::Utc::now(),
    };
    thread.deliver_signal(signal);

    // Process a turn — the signal should be included in context
    let embedder = SyntheticEmbedding::new(128);
    let engine = animus_cortex::MockEngine::new("I see the build failure.");

    let result = thread.process_turn(
        "How are things going?",
        "You are an AI.",
        &engine,
        &embedder,
    ).await.unwrap();

    // After processing, signals should be drained
    assert!(thread.pending_signals().is_empty());
}
```

- [ ] **Step 2: Implement signal injection in process_turn**

In `ReasoningThread::process_turn`, before calling the LLM:
1. Drain pending signals
2. Append signal summaries to the system prompt:
```
## Inter-Thread Signals
- [Urgent] from <thread-id>: Important: build failed
```

- [ ] **Step 3: Run tests**

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(cortex): inject pending signals into reasoning context"
```

---

### Task 7: Final Verification and Cleanup

- [ ] **Step 1: Run full workspace tests**

Run: `cargo test --workspace`

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --workspace -- -D warnings`

- [ ] **Step 3: Verify build**

Run: `cargo build --workspace`

- [ ] **Step 4: Commit any fixes**
