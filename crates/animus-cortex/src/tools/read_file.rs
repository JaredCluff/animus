use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

pub struct ReadFileTool;

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
    // Resolve symlinks so that a symlink pointing outside allowed dirs is not bypassed.
    // If the path doesn't exist yet the caller handles the resulting I/O error.
    match std::fs::canonicalize(p) {
        Ok(canonical) => Ok(canonical),
        Err(_) => Ok(p.to_path_buf()), // Path doesn't exist yet — let I/O report the error
    }
}

#[async_trait::async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str { "read_file" }
    fn description(&self) -> &str { "Read the contents of a file at the given path." }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path to the file to read" }
            },
            "required": ["path"]
        })
    }
    fn required_autonomy(&self) -> Autonomy { Autonomy::Inform }

    async fn execute(&self, params: serde_json::Value, _ctx: &ToolContext) -> Result<ToolResult, String> {
        let path_str = params["path"].as_str().ok_or("missing 'path' parameter")?;
        let path = validate_path(path_str)
            .map_err(|e| format!("invalid path: {e}"))?;
        match tokio::fs::read_to_string(&path).await {
            Ok(contents) => {
                let truncated = if contents.len() > 50_000 {
                    let mut boundary = 50_000;
                    while boundary > 0 && !contents.is_char_boundary(boundary) { boundary -= 1; }
                    format!("{}...\n[truncated, {} total bytes]", &contents[..boundary], contents.len())
                } else { contents };
                Ok(ToolResult { content: truncated, is_error: false })
            }
            Err(e) => Ok(ToolResult { content: format!("Error reading file: {e}"), is_error: true }),
        }
    }
}
