use crate::telos::Autonomy;
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
                    let secs = (end - rec.spawned_at).num_seconds().max(0);
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
            let secs = (end - rec.spawned_at).num_seconds().max(0);
            let runtime = format!("{:02}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60);
            let exit = rec.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "—".to_string());
            let label: &str = rec.label.char_indices().nth(32)
                .map(|(i, _)| &rec.label[..i])
                .unwrap_or(&rec.label);
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
            capability_state: None,
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
        let spawn_result = crate::tools::spawn_task::SpawnTaskTool.execute(
            serde_json::json!({"command": "echo status_test", "label": "status_test"}),
            &ctx
        ).await.unwrap();
        assert!(!spawn_result.is_error);

        let list_result = TaskStatusTool.execute(serde_json::json!({}), &ctx).await.unwrap();
        assert!(!list_result.is_error);
        assert!(list_result.content.contains("status_test") || list_result.content.contains("ID"));
    }
}
