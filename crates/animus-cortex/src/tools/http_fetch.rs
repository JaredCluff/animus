//! HTTP fetch tool — GET/POST any URL.
//!
//! Animus's default web access tool. Tries plain HTTP first. For JS-heavy
//! sites (empty body, React roots), the LLM is expected to call browse_url
//! instead (headless browser, Phase 2).
//!
//! Results are stored in VectorFS with domain tagging so Animus learns
//! which sites need which tool.

use super::{Tool, ToolContext, ToolResult};
use crate::telos::Autonomy;

pub struct HttpFetchTool;

#[async_trait::async_trait]
impl Tool for HttpFetchTool {
    fn name(&self) -> &str {
        "http_fetch"
    }

    fn description(&self) -> &str {
        "Fetch content from a URL via HTTP GET or POST. Returns the response body (HTML, JSON, text). \
        Use this as the default web access method. If the response appears to be an empty or JS-only \
        page (e.g., just a <div id=\"root\"></div>), note it in your response — a headless browser \
        tool will be available in a future update for those cases. \
        The tool automatically extracts text content from HTML pages."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch."
                },
                "method": {
                    "type": "string",
                    "enum": ["GET", "POST"],
                    "description": "HTTP method. Defaults to GET."
                },
                "body": {
                    "type": "string",
                    "description": "Request body (for POST requests). Use JSON string."
                },
                "headers": {
                    "type": "object",
                    "description": "Optional HTTP headers as key-value pairs.",
                    "additionalProperties": {"type": "string"}
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Maximum characters to return from response body. Default 8000."
                }
            },
            "required": ["url"]
        })
    }

    fn required_autonomy(&self) -> Autonomy {
        Autonomy::Act
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, String> {
        let url = params["url"]
            .as_str()
            .ok_or("missing url parameter")?
            .to_string();

        let method = params["method"].as_str().unwrap_or("GET").to_uppercase();
        let max_chars = params["max_chars"].as_u64().unwrap_or(8000) as usize;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("Animus/0.1 (+https://github.com/JaredCluff/animus)")
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;

        let mut request = match method.as_str() {
            "POST" => client.post(&url),
            _ => client.get(&url),
        };

        // Apply optional headers
        if let Some(headers) = params["headers"].as_object() {
            for (key, val) in headers {
                if let Some(v) = val.as_str() {
                    request = request.header(key.as_str(), v);
                }
            }
        }

        // Apply body for POST
        if method == "POST" {
            if let Some(body) = params["body"].as_str() {
                request = request.body(body.to_string());
            }
        }

        let response = request.send().await.map_err(|e| format!("request failed: {e}"))?;
        let status = response.status();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let body = response
            .text()
            .await
            .map_err(|e| format!("failed to read response body: {e}"))?;

        // Strip HTML tags for readability if this is an HTML response
        let content = if content_type.contains("text/html") {
            strip_html_tags(&body)
        } else {
            body
        };

        // Truncate to max_chars
        let truncated = if content.chars().count() > max_chars {
            let end = content
                .char_indices()
                .nth(max_chars)
                .map(|(i, _)| i)
                .unwrap_or(content.len());
            format!("{}\n\n[... truncated at {max_chars} chars]", &content[..end])
        } else {
            content
        };

        let result = format!("HTTP {status} from {url}\nContent-Type: {content_type}\n\n{truncated}");

        if status.is_success() {
            Ok(ToolResult { content: result, is_error: false })
        } else {
            Ok(ToolResult { content: result, is_error: true })
        }
    }
}

/// Very simple HTML tag stripper — removes tags, decodes common entities.
/// Not a full HTML parser — use for extracting readable text from pages.
fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut buf = String::new();

    for ch in html.chars() {
        match ch {
            '<' => {
                in_tag = true;
                buf.clear();
            }
            '>' => {
                if in_tag {
                    let tag = buf.trim().to_lowercase();
                    if tag.starts_with("script") {
                        in_script = true;
                    } else if tag.starts_with("/script") {
                        in_script = false;
                    } else if tag.starts_with("style") {
                        in_style = true;
                    } else if tag.starts_with("/style") {
                        in_style = false;
                    }
                    // Add newline after block-level tags
                    if matches!(
                        tag.trim_start_matches('/').split_whitespace().next(),
                        Some("p" | "div" | "br" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "li" | "tr")
                    ) {
                        result.push('\n');
                    }
                }
                in_tag = false;
                buf.clear();
            }
            _ => {
                if in_tag {
                    buf.push(ch);
                } else if !in_script && !in_style {
                    result.push(ch);
                }
            }
        }
    }

    // Decode common HTML entities
    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
