//! manage_watcher tool — lets the LLM enable, disable, configure, and list background watchers.

use crate::telos::Autonomy;
use super::{Tool, ToolContext, ToolResult};
use std::time::Duration;

pub struct ManageWatcherTool;

#[async_trait::async_trait]
impl Tool for ManageWatcherTool {
    fn name(&self) -> &str { "manage_watcher" }

    fn description(&self) -> &str {
        "Enable, disable, or configure a background watcher. Watchers monitor conditions \
         without LLM involvement and signal you when something requires attention. \
         Use action=list to see all watchers and their current state."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["enable", "disable", "list", "set_param"],
                    "description": "Operation to perform"
                },
                "watcher_id": {
                    "type": "string",
                    "description": "Required for enable, disable, set_param. E.g. \"comms\""
                },
                "interval_secs": {
                    "type": "integer",
                    "description": "Optional poll interval override in seconds (for enable)"
                },
                "params": {
                    "type": "object",
                    "description": "Key-value pairs to merge into watcher params (for set_param)"
                }
            },
            "required": ["action"]
        })
    }

    fn required_autonomy(&self) -> Autonomy { Autonomy::Suggest }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let registry = match &ctx.watcher_registry {
            Some(r) => r,
            None => return Ok(ToolResult { content: "Watcher registry not available".to_string(), is_error: true }),
        };

        let action = params["action"].as_str().unwrap_or("");

        match action {
            "list" => {
                let entries = registry.list();
                if entries.is_empty() {
                    return Ok(ToolResult { content: "No watchers registered.".to_string(), is_error: false });
                }
                let mut out = String::from("Registered watchers:\n");
                for (id, name, cfg) in &entries {
                    let state = if cfg.enabled { "enabled" } else { "disabled" };
                    let interval = cfg.interval.map(|d| format!("{}s", d.as_secs())).unwrap_or_else(|| "default".to_string());
                    let last_fired = cfg.last_fired.map(|t| t.to_rfc3339()).unwrap_or_else(|| "never".to_string());
                    out.push_str(&format!("  {id} — {name} [{state}] interval={interval} last_fired={last_fired}\n"));
                }
                Ok(ToolResult { content: out, is_error: false })
            }

            "enable" => {
                let id = params["watcher_id"].as_str().ok_or("missing watcher_id")?;
                if !registry.has_watcher(id) {
                    return Ok(ToolResult { content: format!("Unknown watcher: {id}"), is_error: true });
                }
                let mut cfg = registry.get_config(id);
                cfg.enabled = true;
                if let Some(secs) = params["interval_secs"].as_u64() {
                    cfg.interval = Some(Duration::from_secs(secs));
                }
                match registry.update_config(id, cfg) {
                    Ok(()) => Ok(ToolResult { content: format!("Watcher '{id}' enabled."), is_error: false }),
                    Err(e) => Ok(ToolResult { content: e, is_error: true }),
                }
            }

            "disable" => {
                let id = params["watcher_id"].as_str().ok_or("missing watcher_id")?;
                if !registry.has_watcher(id) {
                    return Ok(ToolResult { content: format!("Unknown watcher: {id}"), is_error: true });
                }
                let mut cfg = registry.get_config(id);
                cfg.enabled = false;
                match registry.update_config(id, cfg) {
                    Ok(()) => Ok(ToolResult { content: format!("Watcher '{id}' disabled."), is_error: false }),
                    Err(e) => Ok(ToolResult { content: e, is_error: true }),
                }
            }

            "set_param" => {
                let id = params["watcher_id"].as_str().ok_or("missing watcher_id")?;
                if !registry.has_watcher(id) {
                    return Ok(ToolResult { content: format!("Unknown watcher: {id}"), is_error: true });
                }
                let new_params = params["params"].as_object().ok_or("params must be an object")?;
                let mut cfg = registry.get_config(id);
                let mut existing = match cfg.params.take() {
                    serde_json::Value::Object(m) => m,
                    _ => serde_json::Map::new(),
                };
                for (k, v) in new_params {
                    existing.insert(k.clone(), v.clone());
                }
                cfg.params = serde_json::Value::Object(existing);
                match registry.update_config(id, cfg) {
                    Ok(()) => Ok(ToolResult { content: format!("Watcher '{id}' params updated."), is_error: false }),
                    Err(e) => Ok(ToolResult { content: e, is_error: true }),
                }
            }

            other => Ok(ToolResult {
                content: format!("Unknown action: {other}. Valid: list, enable, disable, set_param"),
                is_error: true,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolContext;
    use crate::watcher::WatcherRegistry;
    use crate::watchers::CommsWatcher;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    fn make_test_registry(tmp_dir: &std::path::Path) -> WatcherRegistry {
        let (tx, _rx) = mpsc::channel(8);
        WatcherRegistry::new(
            vec![Box::new(CommsWatcher)],
            tx,
            tmp_dir.join("watchers.json"),
        )
    }

    fn make_ctx(tmp_dir: &std::path::Path, registry: WatcherRegistry) -> ToolContext {
        let store_dir = tmp_dir.join("vectorfs");
        std::fs::create_dir_all(&store_dir).unwrap();
        let store = Arc::new(animus_vectorfs::store::MmapVectorStore::open(&store_dir, 4).unwrap());
        let embedder = Arc::new(animus_embed::SyntheticEmbedding::new(4));
        let (signal_tx, _rx) = mpsc::channel(8);
        ToolContext {
            data_dir: tmp_dir.to_path_buf(),
            snapshot_dir: tmp_dir.join("snapshots"),
            store: store as Arc<dyn animus_vectorfs::VectorStore>,
            embedder: embedder as Arc<dyn animus_core::EmbeddingService>,
            signal_tx: Some(signal_tx),
            autonomy_tx: None,
            active_telegram_chat_id: Arc::new(parking_lot::Mutex::new(None)),
            watcher_registry: Some(registry),
            task_manager: None,
            self_event_filter: None,
            api_tracker: None,
            nats_client: None,
            federation_tx: None,
        }
    }

    #[tokio::test]
    async fn list_action_returns_watcher_table() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = make_test_registry(tmp.path());
        let ctx = make_ctx(tmp.path(), registry);
        let tool = ManageWatcherTool;
        let result = tool.execute(serde_json::json!({"action": "list"}), &ctx).await.unwrap();
        assert!(!result.is_error, "expected non-error, got: {}", result.content);
        assert!(result.content.contains("comms"), "expected 'comms' in output: {}", result.content);
        assert!(result.content.contains("Claude Code Comms"), "expected watcher name in output: {}", result.content);
    }

    #[tokio::test]
    async fn enable_action_enables_watcher() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = make_test_registry(tmp.path());
        let ctx = make_ctx(tmp.path(), registry);
        let tool = ManageWatcherTool;
        let result = tool.execute(serde_json::json!({"action": "enable", "watcher_id": "comms"}), &ctx).await.unwrap();
        assert!(!result.is_error, "expected non-error, got: {}", result.content);
        let cfg = ctx.watcher_registry.as_ref().unwrap().get_config("comms");
        assert!(cfg.enabled, "expected watcher to be enabled");
    }

    #[tokio::test]
    async fn disable_action_disables_watcher() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = make_test_registry(tmp.path());
        let ctx = make_ctx(tmp.path(), registry);
        let tool = ManageWatcherTool;
        // Enable first
        tool.execute(serde_json::json!({"action": "enable", "watcher_id": "comms"}), &ctx).await.unwrap();
        // Now disable
        let result = tool.execute(serde_json::json!({"action": "disable", "watcher_id": "comms"}), &ctx).await.unwrap();
        assert!(!result.is_error, "expected non-error, got: {}", result.content);
        let cfg = ctx.watcher_registry.as_ref().unwrap().get_config("comms");
        assert!(!cfg.enabled, "expected watcher to be disabled");
    }

    #[tokio::test]
    async fn unknown_watcher_id_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = make_test_registry(tmp.path());
        let ctx = make_ctx(tmp.path(), registry);
        let tool = ManageWatcherTool;
        let result = tool.execute(serde_json::json!({"action": "enable", "watcher_id": "nonexistent"}), &ctx).await.unwrap();
        assert!(result.is_error, "expected is_error=true");
        assert!(result.content.contains("nonexistent"), "expected 'nonexistent' in content: {}", result.content);
    }
}
