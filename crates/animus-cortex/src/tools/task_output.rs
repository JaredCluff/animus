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
                    let s = String::from_utf8_lossy(&bytes);
                    // Find a char boundary at or after the byte offset so we don't split a codepoint
                    let byte_offset = bytes.len() - MAX_OUTPUT_BYTES;
                    let char_offset = s[byte_offset..].char_indices().next()
                        .map(|(i, _)| byte_offset + i)
                        .unwrap_or(s.len());
                    format!("[...truncated, showing last 1MB]\n{}", &s[char_offset..])
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
        let id = spawn_result.content.split("id=").nth(1).unwrap().split_whitespace().next().unwrap().to_string();

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
