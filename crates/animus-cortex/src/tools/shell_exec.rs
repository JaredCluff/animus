use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

pub struct ShellExecTool;

#[async_trait::async_trait]
impl Tool for ShellExecTool {
    fn name(&self) -> &str { "shell_exec" }
    fn description(&self) -> &str { "Execute a shell command and return its stdout/stderr." }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute" },
                "working_dir": { "type": "string", "description": "Working directory (optional)" }
            },
            "required": ["command"]
        })
    }
    fn required_autonomy(&self) -> Autonomy { Autonomy::Act }

    async fn execute(&self, params: serde_json::Value, _ctx: &ToolContext) -> Result<ToolResult, String> {
        let command = params["command"].as_str().ok_or("missing 'command' parameter")?;
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(command);
        if let Some(dir) = params["working_dir"].as_str() {
            use std::path::{Component, Path};
            let p = Path::new(dir);
            if !p.is_absolute() {
                return Ok(ToolResult { content: "working_dir must be absolute".to_string(), is_error: true });
            }
            if p.components().any(|c| matches!(c, Component::ParentDir)) {
                return Ok(ToolResult { content: "path traversal not allowed in working_dir".to_string(), is_error: true });
            }
            cmd.current_dir(p);
        }
        match cmd.output().await {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let mut result = String::new();
                if !stdout.is_empty() { result.push_str(&stdout); }
                if !stderr.is_empty() {
                    if !result.is_empty() { result.push('\n'); }
                    result.push_str("[stderr] ");
                    result.push_str(&stderr);
                }
                if result.is_empty() {
                    result = format!("Command completed with exit code {}", output.status.code().unwrap_or(-1));
                }
                if result.len() > 50_000 {
                    let boundary = result.floor_char_boundary(50_000);
                    result = format!("{}...\n[truncated]", &result[..boundary]);
                }
                Ok(ToolResult { content: result, is_error: !output.status.success() })
            }
            Err(e) => Ok(ToolResult { content: format!("Error executing command: {e}"), is_error: true }),
        }
    }
}
