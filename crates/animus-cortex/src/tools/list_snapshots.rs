use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

/// List available VectorFS snapshots.
pub struct ListSnapshotsTool;

#[async_trait::async_trait]
impl Tool for ListSnapshotsTool {
    fn name(&self) -> &str { "list_snapshots" }
    fn description(&self) -> &str {
        "List available VectorFS memory snapshots. Shows snapshot names (use these with \
         restore_snapshot), segment counts, and timestamps. Only complete snapshots are listed."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    fn required_autonomy(&self) -> Autonomy { Autonomy::Inform }

    async fn execute(&self, _params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        if !ctx.snapshot_dir.exists() {
            return Ok(ToolResult {
                content: format!(
                    "No snapshots found (snapshot directory does not exist: {})",
                    ctx.snapshot_dir.display()
                ),
                is_error: false,
            });
        }

        let entries = match std::fs::read_dir(&ctx.snapshot_dir) {
            Ok(e) => e,
            Err(e) => return Ok(ToolResult {
                content: format!("Failed to read snapshot directory: {e}"),
                is_error: true,
            }),
        };

        let mut snapshots: Vec<(String, usize)> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path().is_dir() && e.path().join("COMPLETE").exists()
            })
            .map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let count = e.path().join("segments")
                    .read_dir()
                    .map(|rd| rd.filter_map(|f| f.ok())
                        .filter(|f| f.path().extension().is_some_and(|x| x == "bin"))
                        .count())
                    .unwrap_or(0);
                (name, count)
            })
            .collect();

        if snapshots.is_empty() {
            return Ok(ToolResult {
                content: "No complete snapshots found.".to_string(),
                is_error: false,
            });
        }

        // Sort newest first (names start with timestamp)
        snapshots.sort_by(|a, b| b.0.cmp(&a.0));

        let mut lines = vec![
            format!("Snapshots at {}:", ctx.snapshot_dir.display()),
            format!("{:<40} {:>8}", "Name", "Segments"),
            "-".repeat(50),
        ];
        for (name, count) in &snapshots {
            lines.push(format!("{:<40} {:>8}", name, count));
        }
        lines.push(format!("\nTotal: {} snapshot(s)", snapshots.len()));
        lines.push(format!("Use restore_snapshot with the snapshot name to restore."));

        Ok(ToolResult { content: lines.join("\n"), is_error: false })
    }
}
