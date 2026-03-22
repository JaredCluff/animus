use animus_core::segment::Content;
use animus_core::Tier;
use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

pub struct ListSegmentsTool;

#[async_trait::async_trait]
impl Tool for ListSegmentsTool {
    fn name(&self) -> &str { "list_segments" }
    fn description(&self) -> &str { "Query stored knowledge segments by tier." }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "tier": { "type": "string", "enum": ["hot", "warm", "cold", "all"], "description": "Filter by storage tier" },
                "limit": { "type": "integer", "description": "Maximum segments to return" }
            }
        })
    }
    fn required_autonomy(&self) -> Autonomy { Autonomy::Inform }
    fn needs_vectorfs(&self) -> bool { true }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let tier_filter = match params["tier"].as_str().unwrap_or("all") {
            "hot" => Some(Tier::Hot),
            "warm" => Some(Tier::Warm),
            "cold" => Some(Tier::Cold),
            _ => None,
        };
        const MAX_LIST_LIMIT: usize = 500;
        let limit = (params["limit"].as_u64().unwrap_or(20) as usize).min(MAX_LIST_LIMIT);

        let ids = ctx.store.segment_ids(tier_filter);
        let total = ids.len();

        let mut output = if let Some(t) = tier_filter {
            format!("{total} segment(s) in {t:?} tier:\n")
        } else {
            format!("{total} segment(s) total:\n")
        };

        for id in ids.into_iter().take(limit) {
            if let Ok(Some(seg)) = ctx.store.get_raw(id) {
                let preview = match &seg.content {
                    Content::Text(t) => t.chars().take(60).collect::<String>(),
                    Content::Structured(_) => "[structured data]".to_string(),
                    Content::Binary { .. } => "[binary data]".to_string(),
                    Content::Reference { uri, .. } => format!("[ref: {uri}]"),
                };
                output.push_str(&format!(
                    "- [{id}] conf={:.2} decay={:?}: {preview}\n",
                    seg.confidence, seg.decay_class
                ));
            }
        }

        Ok(ToolResult { content: output, is_error: false })
    }
}
