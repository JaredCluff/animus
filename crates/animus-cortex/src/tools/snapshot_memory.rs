use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

/// Manual VectorFS snapshot trigger.
pub struct SnapshotMemoryTool;

#[async_trait::async_trait]
impl Tool for SnapshotMemoryTool {
    fn name(&self) -> &str { "snapshot_memory" }
    fn description(&self) -> &str {
        "Create a named snapshot of all VectorFS memory. Snapshots are stored outside the data \
         directory and are protected from accidental shell_exec deletion. Use list_snapshots to \
         see existing snapshots and restore_snapshot to roll back."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "label": {
                    "type": "string",
                    "description": "Optional human-readable label appended to the timestamp (e.g. 'before-cleanup')"
                }
            }
        })
    }
    fn required_autonomy(&self) -> Autonomy { Autonomy::Suggest }
    fn needs_vectorfs(&self) -> bool { true }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        let dir_name = match params["label"].as_str().filter(|s| !s.is_empty()) {
            Some(label) => {
                // Sanitize label: allow alphanumeric, dash, underscore only
                let clean: String = label.chars()
                    .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
                    .take(64)
                    .collect();
                format!("{timestamp}-{clean}")
            }
            None => timestamp.to_string(),
        };

        if let Err(e) = std::fs::create_dir_all(&ctx.snapshot_dir) {
            return Ok(ToolResult {
                content: format!("Failed to create snapshot directory {}: {e}", ctx.snapshot_dir.display()),
                is_error: true,
            });
        }

        let snap_path = ctx.snapshot_dir.join(&dir_name);
        match ctx.store.snapshot(&snap_path) {
            Ok(count) => Ok(ToolResult {
                content: format!(
                    "Snapshot '{dir_name}' created: {count} segment(s) at {}",
                    snap_path.display()
                ),
                is_error: false,
            }),
            Err(e) => Ok(ToolResult {
                content: format!("Snapshot failed: {e}"),
                is_error: true,
            }),
        }
    }
}
