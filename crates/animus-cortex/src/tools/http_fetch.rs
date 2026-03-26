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

/// Returns an error string if the URL is disallowed (SSRF protection).
///
/// Blocked:
/// - Non-HTTP(S) schemes
/// - Loopback addresses (127.x, ::1)
/// - Link-local / APIPA (169.254.x.x, fe80::)
/// - Private RFC-1918 ranges (10.x, 172.16-31.x, 192.168.x)
/// - Cloud metadata endpoints (169.254.169.254, metadata.google.internal, etc.)
fn check_url_allowed(url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("invalid URL: {e}"))?;

    match parsed.scheme() {
        "http" | "https" => {}
        s => return Err(format!("scheme '{s}' is not allowed — use http or https")),
    }

    let host = parsed.host_str().ok_or("URL has no host")?;

    // Deny well-known metadata hostnames
    let blocked_hostnames = [
        "metadata.google.internal",
        "metadata.goog",
        "169.254.169.254",
        "instance-data",
    ];
    if blocked_hostnames.iter().any(|&h| host.eq_ignore_ascii_case(h)) {
        return Err(format!("host '{host}' is blocked (cloud metadata endpoint)"));
    }

    // For IP addresses, deny private/loopback/link-local ranges
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if ip.is_loopback() {
            return Err(format!("loopback address '{ip}' is not allowed"));
        }
        match ip {
            std::net::IpAddr::V4(v4) => {
                let octets = v4.octets();
                // 169.254.0.0/16 link-local (APIPA, cloud metadata)
                if octets[0] == 169 && octets[1] == 254 {
                    return Err(format!("link-local address '{ip}' is not allowed"));
                }
                // 10.0.0.0/8
                if octets[0] == 10 {
                    return Err(format!("private address '{ip}' is not allowed"));
                }
                // 172.16.0.0/12
                if octets[0] == 172 && (16..=31).contains(&octets[1]) {
                    return Err(format!("private address '{ip}' is not allowed"));
                }
                // 192.168.0.0/16
                if octets[0] == 192 && octets[1] == 168 {
                    return Err(format!("private address '{ip}' is not allowed"));
                }
                // 100.64.0.0/10 (shared address space / Tailscale/VPN)
                if octets[0] == 100 && (64..=127).contains(&octets[1]) {
                    return Err(format!("shared/carrier-grade NAT address '{ip}' is not allowed"));
                }
            }
            std::net::IpAddr::V6(v6) => {
                let segments = v6.segments();
                // fc00::/7 (unique local)
                if (segments[0] & 0xfe00) == 0xfc00 {
                    return Err(format!("unique-local IPv6 address '{ip}' is not allowed"));
                }
            }
        }
    }

    Ok(())
}

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

        // SSRF protection: block private/loopback/metadata URLs
        if let Err(reason) = check_url_allowed(&url) {
            return Ok(ToolResult { content: format!("Blocked: {reason}"), is_error: true });
        }

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
