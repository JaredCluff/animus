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

        let display_label = {
            let base = label.as_deref().unwrap_or(&command);
            base.char_indices().nth(40)
                .map(|(i, _)| base[..i].to_string())
                .unwrap_or_else(|| base.to_string())
        };
        match manager.spawn_task(command, label, timeout_secs).await {
            Ok(id) => Ok(ToolResult {
                content: format!("Task spawned: id={id} label=\"{display_label}\""),
                is_error: false,
            }),
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
            snapshot_dir: dir.join("snapshots"),
            store: store as Arc<dyn animus_vectorfs::VectorStore>,
            embedder: embedder as Arc<dyn animus_core::EmbeddingService>,
            signal_tx: None,
            autonomy_tx: None,
            active_telegram_chat_id: Arc::new(parking_lot::Mutex::new(None)),
            watcher_registry: None,
            task_manager: Some(mgr),
            self_event_filter: None,
            api_tracker: None,
            nats_client: None,
            federation_tx: None,
            smart_router: None,
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
            snapshot_dir: tmp.path().join("snapshots"),
            store: store as Arc<dyn animus_vectorfs::VectorStore>,
            embedder: embedder as Arc<dyn animus_core::EmbeddingService>,
            signal_tx: None,
            autonomy_tx: None,
            active_telegram_chat_id: Arc::new(parking_lot::Mutex::new(None)),
            watcher_registry: None,
            task_manager: None,
            self_event_filter: None,
            api_tracker: None,
            nats_client: None,
            federation_tx: None,
            smart_router: None,
        };
        let result = SpawnTaskTool.execute(
            serde_json::json!({"command": "echo hi"}),
            &ctx
        ).await.unwrap();
        assert!(result.is_error);
    }
}
