use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

/// Restore VectorFS from a named snapshot.
pub struct RestoreSnapshotTool;

#[async_trait::async_trait]
impl Tool for RestoreSnapshotTool {
    fn name(&self) -> &str { "restore_snapshot" }
    fn description(&self) -> &str {
        "Restore VectorFS memory from a named snapshot (use list_snapshots to see available ones). \
         Merges snapshot segments into the current store without clearing existing memory. \
         Consider taking a snapshot_memory first to preserve current state before restoring. \
         Only restores from the protected snapshot directory."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "snapshot_name": {
                    "type": "string",
                    "description": "Name of the snapshot to restore (from list_snapshots output)"
                }
            },
            "required": ["snapshot_name"]
        })
    }
    fn required_autonomy(&self) -> Autonomy { Autonomy::Act }
    fn needs_vectorfs(&self) -> bool { true }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let name = params["snapshot_name"].as_str().ok_or("missing 'snapshot_name' parameter")?;

        // Reject path traversal attempts
        if name.contains("..") || name.contains('/') || name.contains('\\') {
            return Ok(ToolResult {
                content: "Invalid snapshot name: must be a plain directory name with no path separators".to_string(),
                is_error: true,
            });
        }

        let snap_path = ctx.snapshot_dir.join(name);

        if !snap_path.exists() {
            return Ok(ToolResult {
                content: format!(
                    "Snapshot '{name}' not found at {}. Use list_snapshots to see available snapshots.",
                    snap_path.display()
                ),
                is_error: true,
            });
        }

        if !snap_path.join("COMPLETE").exists() {
            return Ok(ToolResult {
                content: format!(
                    "Snapshot '{name}' is incomplete (missing COMPLETE marker). It may have been interrupted. \
                     Use list_snapshots to find a complete snapshot."
                ),
                is_error: true,
            });
        }

        match ctx.store.restore_from_snapshot(&snap_path) {
            Ok(count) => Ok(ToolResult {
                content: format!("Restored {count} segment(s) from snapshot '{name}'"),
                is_error: false,
            }),
            Err(e) => Ok(ToolResult {
                content: format!("Restore failed: {e}"),
                is_error: true,
            }),
        }
    }
}
