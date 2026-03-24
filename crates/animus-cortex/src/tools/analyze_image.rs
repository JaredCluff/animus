//! Image analysis tool — sends an image to Claude's multimodal API.
//!
//! Works for photos received via Telegram (downloaded to local paths),
//! screenshots from the Mac Studio, and any other local image file.
//! Supports JPEG, PNG, GIF, and WebP.

use super::{Tool, ToolContext, ToolResult};
use crate::telos::Autonomy;
use base64::Engine as _;

pub struct AnalyzeImageTool;

#[async_trait::async_trait]
impl Tool for AnalyzeImageTool {
    fn name(&self) -> &str {
        "analyze_image"
    }

    fn description(&self) -> &str {
        "Analyze an image file and describe what you see. Accepts a local file path. \
        Useful for photos received via Telegram, screenshots, documents, diagrams, \
        or any visual content. Returns a detailed description of the image contents."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "image_path": {
                    "type": "string",
                    "description": "Absolute path to the image file to analyze."
                },
                "question": {
                    "type": "string",
                    "description": "Optional specific question about the image. If omitted, provides a general description."
                }
            },
            "required": ["image_path"]
        })
    }

    fn required_autonomy(&self) -> Autonomy {
        Autonomy::Suggest
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, String> {
        let image_path = params["image_path"]
            .as_str()
            .ok_or("missing image_path parameter")?;

        let question = params["question"].as_str().unwrap_or(
            "Please describe this image in detail. Include any text, objects, people, \
            layout, colors, and anything else that appears relevant.",
        );

        // Read and base64-encode the image
        let image_bytes =
            std::fs::read(image_path).map_err(|e| format!("failed to read image file: {e}"))?;

        let media_type = detect_media_type(image_path);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&image_bytes);

        // Get auth token from environment
        let (auth_header, auth_value, extra_header) = get_auth_headers()
            .ok_or("no Anthropic auth available (set CLAUDE_CODE_OAUTH_TOKEN or ANTHROPIC_API_KEY)")?;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;

        let model = std::env::var("ANIMUS_MODEL")
            .unwrap_or_else(|_| "claude-haiku-4-5-20251001".to_string());

        let body = serde_json::json!({
            "model": model,
            "max_tokens": 1024,
            "messages": [{
                "role": "user",
                "content": [
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": media_type,
                            "data": b64
                        }
                    },
                    {
                        "type": "text",
                        "text": question
                    }
                ]
            }]
        });

        let mut req = client
            .post("https://api.anthropic.com/v1/messages")
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .header(&auth_header, &auth_value);

        if let Some((hdr, val)) = extra_header {
            req = req.header(hdr, val);
        }

        let resp = req
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("analyze_image request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("analyze_image API error ({status}): {text}"));
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("analyze_image parse failed: {e}"))?;

        let description = json["content"][0]["text"]
            .as_str()
            .unwrap_or("(no description returned)")
            .to_string();

        Ok(ToolResult { content: description, is_error: false })
    }
}

/// Detect MIME type from file extension.
fn detect_media_type(path: &str) -> &'static str {
    let lower = path.to_lowercase();
    if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else {
        "image/jpeg" // default
    }
}

/// Returns (header_name, header_value, optional_extra_header) for Anthropic auth.
fn get_auth_headers() -> Option<(String, String, Option<(String, String)>)> {
    // OAuth token (Claude Code / Claude Max)
    if let Ok(token) = std::env::var("CLAUDE_CODE_OAUTH_TOKEN") {
        if !token.is_empty() {
            return Some((
                "authorization".to_string(),
                format!("Bearer {token}"),
                Some(("anthropic-beta".to_string(), "oauth-2025-04-20".to_string())),
            ));
        }
    }
    if let Ok(token) = std::env::var("CLAUDE_CODE_OATH_TOKEN") {
        if !token.is_empty() {
            return Some((
                "authorization".to_string(),
                format!("Bearer {token}"),
                Some(("anthropic-beta".to_string(), "oauth-2025-04-20".to_string())),
            ));
        }
    }
    if let Ok(token) = std::env::var("ANTHROPIC_OAUTH_TOKEN") {
        if !token.is_empty() {
            return Some((
                "authorization".to_string(),
                format!("Bearer {token}"),
                Some(("anthropic-beta".to_string(), "oauth-2025-04-20".to_string())),
            ));
        }
    }
    // API key
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.is_empty() {
            return Some(("x-api-key".to_string(), key, None));
        }
    }
    None
}
