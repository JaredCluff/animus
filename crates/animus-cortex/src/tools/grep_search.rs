use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

pub struct GrepSearchTool;

fn collect_files(dir: &std::path::Path, glob_filter: Option<&str>) -> Vec<std::path::PathBuf> {
    use walkdir::WalkDir;
    let glob_pat = glob_filter.and_then(|f| glob::Pattern::new(f).ok());

    WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            if let Some(ref pat) = glob_pat {
                let filename = e.file_name().to_string_lossy();
                pat.matches(&filename)
            } else {
                true
            }
        })
        .map(|e| e.path().to_path_buf())
        .collect()
}

#[async_trait::async_trait]
impl Tool for GrepSearchTool {
    fn name(&self) -> &str { "grep_search" }

    fn description(&self) -> &str {
        "Search files for a regex pattern. Supports output modes: 'files' (default, paths with matches), 'content' (matching lines with context), 'count' (match counts per file)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search (default: data_dir)"
                },
                "glob": {
                    "type": "string",
                    "description": "File filter glob (e.g. *.rs, *.ts)"
                },
                "output_mode": {
                    "type": "string",
                    "description": "Output mode: 'files' (default), 'content', or 'count'",
                    "enum": ["files", "content", "count"]
                },
                "context": {
                    "type": "integer",
                    "description": "Lines of context around each match (only for 'content' mode, default: 0)"
                }
            },
            "required": ["pattern"]
        })
    }

    fn required_autonomy(&self) -> Autonomy { Autonomy::Inform }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let pattern_str = params["pattern"].as_str().ok_or("missing 'pattern' parameter")?;
        let regex = regex::Regex::new(pattern_str)
            .map_err(|e| format!("invalid regex: {e}"))?;

        let search_dir = if let Some(p) = params["path"].as_str() {
            std::path::PathBuf::from(p)
        } else {
            ctx.data_dir.clone()
        };

        let glob_filter = params["glob"].as_str();
        let output_mode = params["output_mode"].as_str().unwrap_or("files");
        let context_lines = params["context"].as_u64().unwrap_or(0) as usize;

        // Collect files in a blocking task to avoid blocking the async runtime
        let search_dir_clone = search_dir.clone();
        let glob_filter_owned = glob_filter.map(|s| s.to_string());
        let files = tokio::task::spawn_blocking(move || {
            collect_files(&search_dir_clone, glob_filter_owned.as_deref())
        })
        .await
        .map_err(|e| format!("file collection failed: {e}"))?;

        const MAX_OUTPUT_BYTES: usize = 50 * 1024; // 50KB
        let mut output = String::new();
        let mut total_matches: usize = 0;
        let mut truncated = false;

        match output_mode {
            "content" => {
                for file_path in &files {
                    if truncated { break; }
                    let content = match std::fs::read(file_path) {
                        Ok(bytes) => match String::from_utf8(bytes) {
                            Ok(s) => s,
                            Err(_) => continue, // skip binary files
                        },
                        Err(_) => continue,
                    };

                    let lines: Vec<&str> = content.lines().collect();
                    // Find all matching line indices
                    let matching_indices: Vec<usize> = lines
                        .iter()
                        .enumerate()
                        .filter(|(_, line)| regex.is_match(line))
                        .map(|(i, _)| i)
                        .collect();

                    if matching_indices.is_empty() {
                        continue;
                    }

                    // Build a set of lines to include (match lines + context)
                    let mut include: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
                    for &idx in &matching_indices {
                        let start = idx.saturating_sub(context_lines);
                        let end = (idx + context_lines + 1).min(lines.len());
                        for i in start..end {
                            include.insert(i);
                        }
                    }

                    for line_idx in include {
                        let entry = format!("{}:{}: {}\n",
                            file_path.display(),
                            line_idx + 1,
                            lines[line_idx]
                        );
                        if output.len() + entry.len() > MAX_OUTPUT_BYTES {
                            truncated = true;
                            break;
                        }
                        output.push_str(&entry);
                        total_matches += 1;
                    }
                }
            }
            "count" => {
                for file_path in &files {
                    if truncated { break; }
                    let content = match std::fs::read(file_path) {
                        Ok(bytes) => match String::from_utf8(bytes) {
                            Ok(s) => s,
                            Err(_) => continue,
                        },
                        Err(_) => continue,
                    };

                    let count = regex.find_iter(&content).count();
                    if count == 0 {
                        continue;
                    }
                    total_matches += count;

                    let entry = format!("{}: {} matches\n", file_path.display(), count);
                    if output.len() + entry.len() > MAX_OUTPUT_BYTES {
                        truncated = true;
                        break;
                    }
                    output.push_str(&entry);
                }
                if !truncated {
                    output.push_str(&format!("Total: {} matches\n", total_matches));
                }
            }
            _ => {
                // "files" mode (default)
                for file_path in &files {
                    if truncated { break; }
                    let content = match std::fs::read(file_path) {
                        Ok(bytes) => match String::from_utf8(bytes) {
                            Ok(s) => s,
                            Err(_) => continue,
                        },
                        Err(_) => continue,
                    };

                    if regex.is_match(&content) {
                        total_matches += 1;
                        let entry = format!("{}\n", file_path.display());
                        if output.len() + entry.len() > MAX_OUTPUT_BYTES {
                            truncated = true;
                            break;
                        }
                        output.push_str(&entry);
                    }
                }
            }
        }

        if truncated {
            output.push_str("\n[truncated at 50KB]");
        }

        if output.is_empty() {
            return Ok(ToolResult {
                content: format!("No matches found for pattern: {}", pattern_str),
                is_error: false,
            });
        }

        Ok(ToolResult { content: output.trim_end().to_string(), is_error: false })
    }
}
