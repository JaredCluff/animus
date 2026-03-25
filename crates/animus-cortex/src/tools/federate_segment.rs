//! federate_segment tool — broadcasts a stored segment to all trusted federation peers.
//!
//! The tool resolves the segment by ID, then sends it to the runtime's federation
//! broadcast channel. The runtime evaluates the federation policy and publishes
//! to all trusted peers.

use super::{Tool, ToolContext, ToolResult};
use crate::telos::Autonomy;
use animus_core::identity::SegmentId;

pub struct FederateSegmentTool;

#[async_trait::async_trait]
impl Tool for FederateSegmentTool {
    fn name(&self) -> &str {
        "federate_segment"
    }

    fn description(&self) -> &str {
        "Broadcast a stored memory segment to all trusted federation peers. \
         The segment must already exist in VectorFS (use remember first). \
         Federation must be enabled and at least one trusted peer must be configured."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "segment_id": {
                    "type": "string",
                    "description": "The full UUID of the segment to broadcast."
                }
            },
            "required": ["segment_id"]
        })
    }

    fn required_autonomy(&self) -> Autonomy {
        Autonomy::Suggest
    }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let id_str = params["segment_id"]
            .as_str()
            .ok_or("segment_id is required")?;

        let uuid = uuid::Uuid::parse_str(id_str)
            .map_err(|e| format!("invalid segment_id (not a UUID): {e}"))?;
        let segment_id = SegmentId(uuid);

        // Verify the segment exists before queuing the broadcast
        ctx.store
            .get_raw(segment_id)
            .map_err(|e| format!("store error: {e}"))?
            .ok_or_else(|| format!("segment {id_str} not found in VectorFS"))?;

        let tx = ctx
            .federation_tx
            .as_ref()
            .ok_or("federation is not enabled or not configured")?;

        tx.send(segment_id)
            .await
            .map_err(|_| "federation broadcast channel closed".to_string())?;

        Ok(ToolResult {
            content: format!(
                "Segment {id_str} queued for federation broadcast to trusted peers."
            ),
            is_error: false,
        })
    }
}
