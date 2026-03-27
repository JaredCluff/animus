//! Introspective tool: get_capability_state
//!
//! Allows the AILF reasoning thread to read the current cognitive tier and all
//! probe metrics. Use this to understand operational state before taking on
//! complex tasks, or when a tier-change Signal arrives from the Cortex substrate.

use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

pub struct GetCapabilityStateTool;

#[async_trait::async_trait]
impl Tool for GetCapabilityStateTool {
    fn name(&self) -> &str { "get_capability_state" }

    fn description(&self) -> &str {
        "Return the current cognitive capability assessment: tier (Full/Strong/Reduced/MemoryOnly/\
         DeadReckoning), model availability and latency, VectorFS health, and memory pressure. \
         Use this to understand your current operational state before taking on complex reasoning \
         tasks, or when a cognitive tier-change Signal arrives."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    fn required_autonomy(&self) -> Autonomy { Autonomy::Inform }

    async fn execute(&self, _params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let Some(capability_state) = &ctx.capability_state else {
            return Ok(ToolResult {
                content: "Capability probe not initialized.".to_string(),
                is_error: true,
            });
        };

        let state = capability_state.read();

        let latency_str = state.latency_ms
            .map(|l| format!("{}ms", l))
            .unwrap_or_else(|| "unknown".to_string());

        let content = format!(
            "Cognitive Tier: {} ({})\n\
             Reasoning available: {}\n\
             Embedding available: {}\n\
             VectorFS healthy: {}\n\
             Memory pressure: {:.1}%\n\
             Active model: {}\n\
             Last probe latency: {}\n\
             Last probed: {}",
            state.tier as u8,
            state.tier.label(),
            state.reasoning_available,
            state.embedding_available,
            state.vectorfs_healthy,
            state.memory_pressure * 100.0,
            state.active_model.as_deref().unwrap_or("none"),
            latency_str,
            state.last_probed.format("%Y-%m-%dT%H:%M:%SZ"),
        );

        tracing::debug!("get_capability_state: tier={}", state.tier.label());

        Ok(ToolResult { content, is_error: false })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolContext;
    use animus_core::capability::{CapabilityState, CognitiveTier};
    use std::sync::Arc;
    use chrono::Utc;

    fn make_ctx_with_state(state: CapabilityState) -> ToolContext {
        let (signal_tx, _rx) = tokio::sync::mpsc::channel(8);
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().join("vectorfs");
        std::fs::create_dir_all(&store_dir).unwrap();
        let store = Arc::new(animus_vectorfs::store::MmapVectorStore::open(&store_dir, 4).unwrap());
        let embedder = Arc::new(animus_embed::SyntheticEmbedding::new(4));
        ToolContext {
            data_dir: tmp.path().to_path_buf(),
            snapshot_dir: tmp.path().join("snapshots"),
            store: store as Arc<dyn animus_vectorfs::VectorStore>,
            embedder: embedder as Arc<dyn animus_core::EmbeddingService>,
            signal_tx: Some(signal_tx),
            autonomy_tx: None,
            active_telegram_chat_id: Arc::new(parking_lot::Mutex::new(None)),
            watcher_registry: None,
            task_manager: None,
            self_event_filter: None,
            api_tracker: None,
            nats_client: None,
            federation_tx: None,
            smart_router: None,
            capability_state: Some(Arc::new(parking_lot::RwLock::new(state))),
            role_mesh: None,
            budget_state: None,
            budget_config: None,
        }
    }

    #[tokio::test]
    async fn no_probe_returns_error() {
        let (signal_tx, _rx) = tokio::sync::mpsc::channel(8);
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().join("vectorfs");
        std::fs::create_dir_all(&store_dir).unwrap();
        let store = Arc::new(animus_vectorfs::store::MmapVectorStore::open(&store_dir, 4).unwrap());
        let embedder = Arc::new(animus_embed::SyntheticEmbedding::new(4));
        let ctx = ToolContext {
            data_dir: tmp.path().to_path_buf(),
            snapshot_dir: tmp.path().join("snapshots"),
            store: store as Arc<dyn animus_vectorfs::VectorStore>,
            embedder: embedder as Arc<dyn animus_core::EmbeddingService>,
            signal_tx: Some(signal_tx),
            autonomy_tx: None,
            active_telegram_chat_id: Arc::new(parking_lot::Mutex::new(None)),
            watcher_registry: None,
            task_manager: None,
            self_event_filter: None,
            api_tracker: None,
            nats_client: None,
            federation_tx: None,
            smart_router: None,
            capability_state: None,
            role_mesh: None,
            budget_state: None,
            budget_config: None,
        };
        let result = GetCapabilityStateTool.execute(serde_json::json!({}), &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("not initialized"));
    }

    #[tokio::test]
    async fn returns_formatted_state() {
        let state = CapabilityState {
            tier: CognitiveTier::Strong,
            reasoning_available: true,
            embedding_available: true,
            vectorfs_healthy: true,
            memory_pressure: 0.12,
            active_model: Some("ollama:llama3".to_string()),
            latency_ms: Some(750),
            last_probed: Utc::now(),
        };
        let ctx = make_ctx_with_state(state);
        let result = GetCapabilityStateTool.execute(serde_json::json!({}), &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("Strong"));
        assert!(result.content.contains("750ms"));
        assert!(result.content.contains("ollama:llama3"));
        assert!(result.content.contains("12.0%"));
    }
}
