pub mod read_file;
pub mod write_file;
pub mod shell_exec;
pub mod remember;
pub mod list_segments;
pub mod send_signal;
pub mod update_segment;
pub mod http_fetch;
pub mod analyze_image;
pub mod set_autonomy;
pub mod telegram_send;
pub mod manage_watcher;
pub mod spawn_task;
pub mod task_status;
pub mod task_output;
pub mod task_cancel;
pub mod delete_segment;
pub mod prune_segments;
pub mod snapshot_memory;
pub mod list_snapshots;
pub mod restore_snapshot;
pub mod nats_publish;

use crate::llm::ToolDefinition;
use crate::task_manager::TaskManager;
use crate::telos::Autonomy;
use crate::watcher::WatcherRegistry;
use crate::perception::SelfEventFilter;
use animus_core::{ApiTracker, EmbeddingService, Signal};
use animus_vectorfs::VectorStore;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Context provided to tools at execution time by the runtime.
pub struct ToolContext {
    /// Root data directory for the AILF.
    pub data_dir: PathBuf,
    /// Snapshot directory — kept outside data_dir so shell_exec protection covers both.
    pub snapshot_dir: PathBuf,
    /// VectorFS store for memory tools.
    pub store: Arc<dyn VectorStore>,
    /// Embedding service for memory tools.
    pub embedder: Arc<dyn EmbeddingService>,
    /// Signal channel for inter-thread communication tools.
    pub signal_tx: Option<mpsc::Sender<Signal>>,
    /// Watch sender for runtime autonomy mode changes (set_autonomy tool).
    pub autonomy_tx: Option<tokio::sync::watch::Sender<animus_core::config::AutonomyMode>>,
    /// Active Telegram chat_id for the current conversation (for proactive sends).
    /// Wrapped in Arc<Mutex> so the runtime can update it between calls without rebuilding ToolContext.
    pub active_telegram_chat_id: Arc<parking_lot::Mutex<Option<i64>>>,
    /// Watcher registry, if the runtime has one configured.
    pub watcher_registry: Option<WatcherRegistry>,
    /// Task manager for background process execution.
    pub task_manager: Option<TaskManager>,
    /// Self-event filter — tools register paths they modify to prevent perception feedback loops.
    pub self_event_filter: Option<Arc<SelfEventFilter>>,
    /// API usage tracker — the AILF can query its own usage patterns.
    pub api_tracker: Option<Arc<ApiTracker>>,
    /// NATS client for proactive publishing via the nats_publish tool.
    pub nats_client: Option<async_nats::Client>,
}

/// A tool the AILF can use to interact with the world.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;
    fn required_autonomy(&self) -> Autonomy;

    /// Whether this tool requires VectorFS access at the runtime level.
    fn needs_vectorfs(&self) -> bool { false }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, String>;
}

/// Result of executing a tool.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
}

/// Registry of available tools.
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.push(tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.iter().find(|t| t.name() == name).map(|t| t.as_ref())
    }

    /// Generate ToolDefinitions for the LLM.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .iter()
            .map(|t| ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.parameters_schema(),
            })
            .collect()
    }

    /// Get definitions for tools available at a given autonomy level.
    pub fn definitions_for_autonomy(&self, granted: Autonomy) -> Vec<ToolDefinition> {
        self.tools
            .iter()
            .filter(|t| granted >= t.required_autonomy())
            .map(|t| ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.parameters_schema(),
            })
            .collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether a tool execution is permitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutonomyDecision {
    Execute,
    Denied,
}

/// Check if the granted autonomy level permits the required autonomy.
pub fn check_autonomy(granted: Autonomy, required: Autonomy) -> AutonomyDecision {
    if granted >= required {
        AutonomyDecision::Execute
    } else {
        AutonomyDecision::Denied
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telos::Autonomy;

    struct DummyTool;

    #[async_trait::async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str { "dummy" }
        fn description(&self) -> &str { "A test tool" }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        fn required_autonomy(&self) -> Autonomy { Autonomy::Inform }

        async fn execute(
            &self,
            _params: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolResult, String> {
            Ok(ToolResult { content: "done".to_string(), is_error: false })
        }
    }

    #[test]
    fn test_registry_register_and_get() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DummyTool));
        assert!(registry.get("dummy").is_some());
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_registry_definitions() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DummyTool));
        let defs = registry.definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "dummy");
    }

    #[test]
    fn test_autonomy_check_allows_when_sufficient() {
        assert_eq!(
            check_autonomy(Autonomy::Act, Autonomy::Suggest),
            AutonomyDecision::Execute
        );
    }

    #[test]
    fn test_autonomy_check_denies_when_insufficient() {
        assert_eq!(
            check_autonomy(Autonomy::Inform, Autonomy::Act),
            AutonomyDecision::Denied
        );
    }

    #[test]
    fn test_autonomy_ordering() {
        assert!(Autonomy::Full >= Autonomy::Act);
        assert!(Autonomy::Act >= Autonomy::Suggest);
        assert!(Autonomy::Suggest >= Autonomy::Inform);
    }
}
