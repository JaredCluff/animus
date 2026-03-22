use animus_core::SegmentId;
use animus_vectorfs::SegmentUpdate;
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

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let segment_id_str = params["segment_id"].as_str().ok_or("missing 'segment_id' parameter")?;
        let feedback = params["feedback"].as_str().ok_or("missing 'feedback' parameter")?;

        let uuid = uuid::Uuid::parse_str(segment_id_str)
            .map_err(|e| format!("invalid segment_id: {e}"))?;
        let id = SegmentId(uuid);

        let seg = match ctx.store.get_raw(id) {
            Ok(Some(s)) => s,
            Ok(None) => return Ok(ToolResult { content: format!("Segment {id} not found"), is_error: true }),
            Err(e) => return Ok(ToolResult { content: format!("Failed to retrieve segment: {e}"), is_error: true }),
        };

        // Cap alpha and beta to prevent runaway accumulation from repeated tool calls.
        const MAX_BAYES_PARAM: f32 = 100.0;
        let update = match feedback {
            "positive" => SegmentUpdate {
                alpha: Some((seg.alpha + 1.0).min(MAX_BAYES_PARAM)),
                ..Default::default()
            },
            "negative" => SegmentUpdate {
                beta: Some((seg.beta + 1.0).min(MAX_BAYES_PARAM)),
                ..Default::default()
            },
            other => return Err(format!("invalid feedback type: {other}")),
        };

        match ctx.store.update_meta(id, update) {
            Ok(()) => Ok(ToolResult {
                content: format!("Updated segment {id} with {feedback} feedback"),
                is_error: false,
            }),
            Err(e) => Ok(ToolResult {
                content: format!("Failed to update segment: {e}"),
                is_error: true,
            }),
        }
    }
}
