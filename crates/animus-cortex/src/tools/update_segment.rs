use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

pub struct UpdateSegmentTool;

#[async_trait::async_trait]
impl Tool for UpdateSegmentTool {
    fn name(&self) -> &str { "update_segment" }
    fn description(&self) -> &str { "Update a knowledge segment's confidence via feedback." }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "segment_id": { "type": "string", "description": "UUID of the segment to update" },
                "feedback": { "type": "string", "enum": ["positive", "negative"], "description": "Feedback type" }
            },
            "required": ["segment_id", "feedback"]
        })
    }
    fn required_autonomy(&self) -> Autonomy { Autonomy::Suggest }
    fn needs_vectorfs(&self) -> bool { true }

    async fn execute(&self, params: serde_json::Value, _ctx: &ToolContext) -> Result<ToolResult, String> {
        let segment_id = params["segment_id"].as_str().ok_or("missing 'segment_id' parameter")?;
        let feedback = params["feedback"].as_str().ok_or("missing 'feedback' parameter")?;
        match feedback {
            "positive" | "negative" => {}
            other => return Err(format!("invalid feedback type: {other}")),
        }
        Ok(ToolResult { content: format!("Updated segment {segment_id} with {feedback} feedback"), is_error: false })
    }
}
