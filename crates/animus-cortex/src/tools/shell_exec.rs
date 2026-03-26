use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

const TIMEOUT_SECS: u64 = 30;

/// Returns true if the command appears to be a destructive recursive operation
/// targeting data_dir or snapshot_dir. This is a heuristic guardrail — not a
/// security boundary — to prevent accidental memory self-wipe.
fn targets_protected_path(command: &str, ctx: &ToolContext) -> bool {
    // Canonicalize protected paths so symlinks/relative segments can't bypass the check.
    let data_canonical = std::fs::canonicalize(&ctx.data_dir).unwrap_or_else(|_| ctx.data_dir.clone());
    let snap_canonical = std::fs::canonicalize(&ctx.snapshot_dir).unwrap_or_else(|_| ctx.snapshot_dir.clone());
    let data_str = data_canonical.to_string_lossy();
    let snap_str = snap_canonical.to_string_lossy();

    // Does the command mention a protected path?
    let mentions_data = command.contains(data_str.as_ref());
    let mentions_snap = command.contains(snap_str.as_ref());
    // Also catch $ANIMUS_DATA_DIR variable references
    let mentions_var = command.contains("$ANIMUS_DATA_DIR")
        || command.contains("${ANIMUS_DATA_DIR}");

    if !mentions_data && !mentions_snap && !mentions_var {
        return false;
    }

    // Check for destructive recursive operations
    if command.contains("rm") {
        // Match -r/-R/-rf/-Rf/-rRf/etc. anywhere in the command
        let recursive = ["-r", "-R", "-rf", "-Rf", "-rF", "-fR", "-fr", "-rRf"];
        if recursive.iter().any(|f| command.contains(f)) {
            return true;
        }
    }

    // rmdir (always recursive)
    if command.contains("rmdir") {
        return true;
    }

    // find ... -delete
    if command.contains("find") && command.contains("-delete") {
        return true;
    }

    false
}

pub struct ShellExecTool;

#[async_trait::async_trait]
impl Tool for ShellExecTool {
    fn name(&self) -> &str { "shell_exec" }
    fn description(&self) -> &str { "Execute a shell command and return its stdout/stderr. Commands are killed after 30 seconds; do not use for long-running or background processes." }
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

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let command = params["command"].as_str().ok_or("missing 'command' parameter")?;

        // Guard against accidental recursive deletion of protected directories.
        // Use delete_segment or prune_segments for memory cleanup instead.
        if targets_protected_path(command, ctx) {
            return Ok(ToolResult {
                content: format!(
                    "Blocked: shell_exec cannot recursively delete the data directory or snapshot \
                     directory. Use delete_segment(segment_id) or prune_segments(filters) for \
                     memory cleanup, or snapshot_memory to save a checkpoint. \
                     Protected paths: data_dir={}, snapshot_dir={}",
                    ctx.data_dir.display(),
                    ctx.snapshot_dir.display(),
                ),
                is_error: true,
            });
        }
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
        let timeout = std::time::Duration::from_secs(TIMEOUT_SECS);
        match tokio::time::timeout(timeout, cmd.output()).await {
            Err(_elapsed) => {
                return Ok(ToolResult {
                    content: format!("Command timed out after {TIMEOUT_SECS}s. Do not use shell_exec for long-running or background processes."),
                    is_error: true,
                });
            }
            Ok(Err(e)) => {
                return Ok(ToolResult { content: format!("Error executing command: {e}"), is_error: true });
            }
            Ok(Ok(output)) => {
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
        }
    }
}
