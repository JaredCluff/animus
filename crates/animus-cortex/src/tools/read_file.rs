use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

pub struct ReadFileTool;

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
        let path = params["path"].as_str().ok_or("missing 'path' parameter")?;
        match tokio::fs::read_to_string(path).await {
            Ok(contents) => {
                let truncated = if contents.len() > 50_000 {
                    format!("{}...\n[truncated, {} total bytes]", &contents[..50_000], contents.len())
                } else { contents };
                Ok(ToolResult { content: truncated, is_error: false })
            }
            Err(e) => Ok(ToolResult { content: format!("Error reading file: {e}"), is_error: true }),
        }
    }
}
