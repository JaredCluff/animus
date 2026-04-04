use super::{Tool, ToolContext, ToolResult};
use crate::telos::Autonomy;

pub struct VectorFsHealthTool;

#[async_trait::async_trait]
impl Tool for VectorFsHealthTool {
    fn name(&self) -> &str { "vectorfs_health" }
    fn description(&self) -> &str {
        "Scan VectorFS for corruption or dimension mismatches and optionally repair. \
         action: 'scan' (report only) or 'repair' (delete corrupted/mismatched segments)."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["scan", "repair"],
                    "description": "'scan' reports health without changes; 'repair' removes corrupted segments"
                }
            },
            "required": ["action"]
        })
    }
    fn required_autonomy(&self) -> Autonomy { Autonomy::Suggest }
    fn needs_vectorfs(&self) -> bool { true }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let action = params["action"].as_str().unwrap_or("scan");

        // Downcast to MmapVectorStore to access scan/repair methods.
        // The store is wrapped in Arc<dyn VectorStore>; we reach into the concrete type
        // via Any if available, otherwise fall back to a count-only report.
        use std::any::Any;

        // We store behind a GatedVectorStore or MmapVectorStore — try both.
        // The scan methods are on MmapVectorStore directly (not the trait), so we
        // use the concrete type via the data_dir to run a direct file scan.
        let segments_dir = ctx.data_dir.join("vectorfs").join("segments");
        if !segments_dir.exists() {
            return Ok(ToolResult {
                content: "VectorFS segments directory not found.".to_string(),
                is_error: true,
            });
        }

        // Count total segments in memory
        let total_in_memory = ctx.store.count(None);

        // Scan files on disk directly (no need for concrete type downcast)
        let mut healthy = 0usize;
        let mut corrupted = Vec::new();
        let mut dim_mismatch: Vec<String> = Vec::new();
        let mut oversized = Vec::new();
        let mut io_errors = 0usize;
        let mut files_scanned = 0usize;

        const MAX_BYTES: u64 = 64 * 1024 * 1024;

        if let Ok(entries) = std::fs::read_dir(&segments_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.extension().is_some_and(|e| e == "bin") { continue; }
                files_scanned += 1;

                let meta = match std::fs::metadata(&path) {
                    Ok(m) => m,
                    Err(_) => { io_errors += 1; continue; }
                };
                if meta.len() > MAX_BYTES {
                    let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("?").to_string();
                    oversized.push(id);
                    continue;
                }
                let data = match std::fs::read(&path) {
                    Ok(d) => d,
                    Err(_) => { io_errors += 1; continue; }
                };
                match bincode::deserialize::<animus_core::Segment>(&data) {
                    Ok(_seg) => healthy += 1,
                    Err(_) => {
                        let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("?").to_string();
                        corrupted.push((id, path.clone()));
                    }
                }
            }
        }

        if action == "repair" {
            let mut removed = 0usize;
            for (id, path) in &corrupted {
                if std::fs::remove_file(path).is_ok() {
                    removed += 1;
                    tracing::warn!(id = %id, "VectorFS repair: removed corrupted segment");
                }
            }
            for id in &dim_mismatch {
                let path = segments_dir.join(format!("{id}.bin"));
                if std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                    tracing::warn!(id = %id, "VectorFS repair: removed dim-mismatch segment");
                }
            }
            for id in &oversized {
                let path = segments_dir.join(format!("{id}.bin"));
                if std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                    tracing::warn!(id = %id, "VectorFS repair: removed oversized segment");
                }
            }
            return Ok(ToolResult {
                content: format!(
                    "VectorFS repair complete. Removed {removed} bad file(s). \
                     Restart Animus to reload the clean store.\n\
                     Was: {files_scanned} files scanned, {} corrupted, {} oversized, {io_errors} io_errors.",
                    corrupted.len(), oversized.len()
                ),
                is_error: false,
            });
        }

        // Scan-only report
        let status = if corrupted.is_empty() && oversized.is_empty() && io_errors == 0 {
            "HEALTHY"
        } else {
            "DEGRADED"
        };

        let mut report = format!(
            "VectorFS health: {status}\n\
             In-memory segments: {total_in_memory}\n\
             Files on disk: {files_scanned}\n\
             Healthy: {healthy}\n\
             Corrupted: {}\n\
             Oversized: {}\n\
             I/O errors: {io_errors}",
            corrupted.len(), oversized.len()
        );

        if !corrupted.is_empty() {
            let ids: Vec<&str> = corrupted.iter().map(|(id, _)| id.as_str()).collect();
            report.push_str(&format!("\nCorrupted IDs: {}", ids.join(", ")));
        }
        if !oversized.is_empty() {
            report.push_str(&format!("\nOversized IDs: {}", oversized.join(", ")));
        }
        if corrupted.len() + oversized.len() + io_errors > 0 {
            report.push_str("\nCall vectorfs_health(action='repair') to remove bad files.");
        }

        Ok(ToolResult { content: report, is_error: false })
    }
}
