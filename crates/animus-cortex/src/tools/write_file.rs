use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

pub struct WriteFileTool;

/// Reject paths that are relative, contain parent-directory traversal, or resolve
/// via symlink to a location that could bypass intended restrictions.
fn validate_path(path: &str) -> Result<std::path::PathBuf, String> {
    use std::path::{Component, Path};
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err("path must be absolute".to_string());
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("path traversal not allowed".to_string());
    }
    // Resolve symlinks on the parent directory so we catch symlinks that escape
    // allowed directories even when the target file doesn't exist yet.
    if let Some(parent) = p.parent() {
        if let Ok(canonical_parent) = std::fs::canonicalize(parent) {
            let file_name = p.file_name().ok_or("path has no file name")?;
            return Ok(canonical_parent.join(file_name));
        }
    }
    Ok(p.to_path_buf())
}

#[async_trait::async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str { "write_file" }
    fn description(&self) -> &str { "Create or overwrite a file with the given content." }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path to write to" },
                "content": { "type": "string", "description": "Content to write to the file" }
            },
            "required": ["path", "content"]
        })
    }
    fn required_autonomy(&self) -> Autonomy { Autonomy::Act }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let path_str = params["path"].as_str().ok_or("missing 'path' parameter")?;
        let path = validate_path(path_str)
            .map_err(|e| format!("invalid path: {e}"))?;
        let content = params["content"].as_str().ok_or("missing 'content' parameter")?;
        const MAX_WRITE_BYTES: usize = 10 * 1024 * 1024; // 10 MiB
        if content.len() > MAX_WRITE_BYTES {
            return Ok(ToolResult {
                content: format!("Content too large: {} bytes (max {} bytes)", content.len(), MAX_WRITE_BYTES),
                is_error: true,
            });
        }
        if let Some(parent) = path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return Ok(ToolResult { content: format!("Error creating directory: {e}"), is_error: true });
            }
        }
        // Register path with self-event filter before writing to prevent perception feedback loop
        if let Some(filter) = &ctx.self_event_filter {
            filter.register(path.to_string_lossy().to_string()).await;
        }
        match tokio::fs::write(&path, content).await {
            Ok(()) => Ok(ToolResult { content: format!("Wrote {} bytes to {}", content.len(), path.display()), is_error: false }),
            Err(e) => Ok(ToolResult { content: format!("Error writing file: {e}"), is_error: true }),
        }
    }
}
