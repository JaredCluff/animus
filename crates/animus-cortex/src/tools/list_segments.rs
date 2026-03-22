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

    async fn execute(&self, params: serde_json::Value, _ctx: &ToolContext) -> Result<ToolResult, String> {
        let _tier = params["tier"].as_str().unwrap_or("all");
        let _limit = params["limit"].as_u64().unwrap_or(20);
        Ok(ToolResult { content: "Segments listed by runtime".to_string(), is_error: false })
    }
}
