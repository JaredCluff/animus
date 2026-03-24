use animus_core::SegmentId;
use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

/// Surgical deletion of a single VectorFS segment by ID.
pub struct DeleteSegmentTool;

#[async_trait::async_trait]
impl Tool for DeleteSegmentTool {
    fn name(&self) -> &str { "delete_segment" }
    fn description(&self) -> &str {
        "Delete a single memory segment by its ID. Use this for precise removal of a specific \
         segment. For bulk cleanup based on filters, use prune_segments instead. \
         IMPORTANT: Deletion is permanent — take a snapshot first if uncertain."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "segment_id": {
                    "type": "string",
                    "description": "UUID of the segment to delete (from list_segments output)"
                }
            },
            "required": ["segment_id"]
        })
    }
    fn required_autonomy(&self) -> Autonomy { Autonomy::Act }
    fn needs_vectorfs(&self) -> bool { true }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let id_str = params["segment_id"].as_str().ok_or("missing 'segment_id' parameter")?;
        let uuid = uuid::Uuid::parse_str(id_str)
            .map_err(|e| format!("invalid segment ID '{id_str}': {e}"))?;
        let id = SegmentId(uuid);

        // Verify it exists before deleting
        match ctx.store.get_raw(id) {
            Ok(None) => return Ok(ToolResult {
                content: format!("Segment {id_str} not found"),
                is_error: true,
            }),
            Err(e) => return Ok(ToolResult {
                content: format!("Error looking up segment: {e}"),
                is_error: true,
            }),
            Ok(Some(_)) => {}
        }

        match ctx.store.delete(id) {
            Ok(()) => Ok(ToolResult {
                content: format!("Deleted segment {id_str}"),
                is_error: false,
            }),
            Err(e) => Ok(ToolResult {
                content: format!("Failed to delete segment {id_str}: {e}"),
                is_error: true,
            }),
        }
    }
}
