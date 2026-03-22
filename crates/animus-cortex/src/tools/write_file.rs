use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

pub struct WriteFileTool;

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

    async fn execute(&self, params: serde_json::Value, _ctx: &ToolContext) -> Result<ToolResult, String> {
        let path = params["path"].as_str().ok_or("missing 'path' parameter")?;
        let content = params["content"].as_str().ok_or("missing 'content' parameter")?;
        if let Some(parent) = std::path::Path::new(path).parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return Ok(ToolResult { content: format!("Error creating directory: {e}"), is_error: true });
            }
        }
        match tokio::fs::write(path, content).await {
            Ok(()) => Ok(ToolResult { content: format!("Wrote {} bytes to {path}", content.len()), is_error: false }),
            Err(e) => Ok(ToolResult { content: format!("Error writing file: {e}"), is_error: true }),
        }
    }
}
