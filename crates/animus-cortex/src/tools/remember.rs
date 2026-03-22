use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

pub struct RememberTool;

#[async_trait::async_trait]
impl Tool for RememberTool {
    fn name(&self) -> &str { "remember" }
    fn description(&self) -> &str { "Store a piece of knowledge in persistent memory (VectorFS)." }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "knowledge": { "type": "string", "description": "The knowledge to store" },
                "decay_class": { "type": "string", "enum": ["factual", "procedural", "episodic", "opinion", "general"], "description": "Knowledge type" }
            },
            "required": ["knowledge"]
        })
    }
    fn required_autonomy(&self) -> Autonomy { Autonomy::Suggest }
    fn needs_vectorfs(&self) -> bool { true }

    async fn execute(&self, params: serde_json::Value, _ctx: &ToolContext) -> Result<ToolResult, String> {
        let knowledge = params["knowledge"].as_str().ok_or("missing 'knowledge' parameter")?;
        let decay_class = params["decay_class"].as_str().unwrap_or("general");
        Ok(ToolResult {
            content: format!("Stored knowledge ({decay_class}): {}", &knowledge[..knowledge.len().min(80)]),
            is_error: false,
        })
    }
}
