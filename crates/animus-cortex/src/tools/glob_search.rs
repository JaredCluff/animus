use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

pub struct GlobSearchTool;

#[async_trait::async_trait]
impl Tool for GlobSearchTool {
    fn name(&self) -> &str { "glob_search" }

    fn description(&self) -> &str {
        "Find files matching a glob pattern (e.g. **/*.rs, src/**/*.ts). Returns absolute paths sorted by most recently modified first."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match (e.g. **/*.rs, src/**/*.ts)"
                },
                "path": {
                    "type": "string",
                    "description": "Base directory to search from (default: data_dir)"
                }
            },
            "required": ["pattern"]
        })
    }

    fn required_autonomy(&self) -> Autonomy { Autonomy::Inform }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let pattern = params["pattern"].as_str().ok_or("missing 'pattern' parameter")?;

        let base_dir = if let Some(p) = params["path"].as_str() {
            std::path::PathBuf::from(p)
        } else {
            ctx.data_dir.clone()
        };

        let full_pattern = format!("{}/{}", base_dir.display(), pattern);

        let mut paths: Vec<std::path::PathBuf> = glob::glob(&full_pattern)
            .map_err(|e| format!("invalid glob pattern: {e}"))?
            .filter_map(|e| e.ok())
            .collect();

        // Sort by modification time descending (most recently modified first)
        paths.sort_by(|a, b| {
            let mtime_a = std::fs::metadata(a)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let mtime_b = std::fs::metadata(b)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            mtime_b.cmp(&mtime_a)
        });

        const MAX_RESULTS: usize = 500;
        let truncated = paths.len() > MAX_RESULTS;
        if truncated {
            paths.truncate(MAX_RESULTS);
        }

        if paths.is_empty() {
            return Ok(ToolResult {
                content: format!("No files matched pattern: {}", pattern),
                is_error: false,
            });
        }

        let mut output = paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");

        if truncated {
            output.push_str(&format!("\n[truncated at {} results]", MAX_RESULTS));
        }

        Ok(ToolResult { content: output, is_error: false })
    }
}
