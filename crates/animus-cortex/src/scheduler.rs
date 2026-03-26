use animus_core::error::{AnimusError, Result};
use animus_core::identity::{GoalId, SegmentId, ThreadId};
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
}

impl<S: VectorStore> ThreadScheduler<S> {
    pub fn new(store: Arc<S>, token_budget: usize) -> Self {
        Self {
            threads: HashMap::new(),
            active_thread: None,
            store,
            token_budget,
        }
    }

    pub fn create_thread(&mut self, name: String) -> ThreadId {
        let mut thread = ReasoningThread::new(
            name,
            self.store.clone(),
            self.token_budget,
        );

        let id = thread.id;
        if self.active_thread.is_some() {
            thread.set_status(ThreadStatus::Suspended)
                .expect("new Active thread must be suspendable");
        }
        if self.active_thread.is_none() {
            self.active_thread = Some(id);
        }
        self.threads.insert(id, thread);
        id
    }

    pub fn switch_to(&mut self, thread_id: ThreadId) -> Result<()> {
        let target = self.threads.get(&thread_id)
            .ok_or_else(|| AnimusError::Threading(format!("thread not found: {thread_id}")))?;
        if target.status() == ThreadStatus::Completed {
            return Err(AnimusError::Threading("cannot switch to a completed thread".to_string()));
        }
        if target.status() == ThreadStatus::Background {
            return Err(AnimusError::Threading("cannot switch to a background thread".to_string()));
        }

        if let Some(current_id) = self.active_thread {
            if current_id != thread_id {
                if let Some(current) = self.threads.get_mut(&current_id) {
                    current.set_status(ThreadStatus::Suspended)?;
                }
            }
        }

        if let Some(target) = self.threads.get_mut(&thread_id) {
            target.set_status(ThreadStatus::Active)?;
        }
        self.active_thread = Some(thread_id);
        Ok(())
    }

    pub fn suspend(&mut self, thread_id: ThreadId) -> Result<()> {
        let thread = self.threads.get_mut(&thread_id)
            .ok_or_else(|| AnimusError::Threading(format!("thread not found: {thread_id}")))?;
        thread.set_status(ThreadStatus::Suspended)?;
        if self.active_thread == Some(thread_id) {
            self.active_thread = None;
        }
        Ok(())
    }

    pub fn complete(&mut self, thread_id: ThreadId) -> Result<()> {
        let thread = self.threads.get_mut(&thread_id)
            .ok_or_else(|| AnimusError::Threading(format!("thread not found: {thread_id}")))?;
        thread.set_status(ThreadStatus::Completed)?;
        if self.active_thread == Some(thread_id) {
            self.active_thread = None;
        }
        Ok(())
    }

    pub fn active_thread_id(&self) -> Option<ThreadId> {
        self.active_thread
    }

    pub fn active_thread_mut(&mut self) -> Option<&mut ReasoningThread<S>> {
        self.active_thread.and_then(|id| self.threads.get_mut(&id))
    }

    pub fn active_thread(&self) -> Option<&ReasoningThread<S>> {
        self.active_thread.and_then(|id| self.threads.get(&id))
    }

    pub fn list_threads(&self) -> Vec<(ThreadId, String, ThreadStatus)> {
        let mut list: Vec<_> = self.threads.values()
            .map(|t| (t.id, t.name.clone(), t.status()))
            .collect();
        list.sort_by(|a, b| a.1.cmp(&b.1));
        list
    }

    pub fn thread_count(&self) -> usize {
        self.threads.len()
    }

    pub fn send_signal(
        &mut self,
        source: ThreadId,
        target: ThreadId,
        priority: SignalPriority,
        summary: String,
        segment_refs: Vec<SegmentId>,
    ) -> Result<()> {
        if !self.threads.contains_key(&source) {
            return Err(AnimusError::Threading(format!("source thread not found: {source}")));
        }
        if !self.threads.contains_key(&target) {
            return Err(AnimusError::Threading(format!("target thread not found: {target}")));
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

    pub fn drain_signals(&mut self, thread_id: ThreadId) -> Vec<Signal> {
        self.threads.get_mut(&thread_id)
            .map(|t| t.drain_signals())
            .unwrap_or_default()
    }

    /// Set a thread to background mode with a bound goal.
    pub fn set_background(&mut self, thread_id: ThreadId, goal_id: GoalId) -> Result<()> {
        let thread = self.threads.get_mut(&thread_id)
            .ok_or_else(|| AnimusError::Threading(format!("thread not found: {thread_id}")))?;
        thread.set_status(ThreadStatus::Background)?;
        thread.bound_goals.push(goal_id);
        if self.active_thread == Some(thread_id) {
            self.active_thread = None;
        }
        Ok(())
    }
}
