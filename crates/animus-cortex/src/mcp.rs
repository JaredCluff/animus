//! MCP (Model Context Protocol) client for Animus.
//!
//! Connects to MCP servers via stdio subprocess, discovers tools,
//! and returns McpToolProxy instances that implement the Tool trait.

use animus_core::config::McpServerConfig;
use anyhow::{Context, Result};
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use rmcp::ServiceExt;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::process::Command;
use tracing::{info, warn};

/// A discovered tool from an MCP server.
#[derive(Debug, Clone)]
pub struct McpDiscoveredTool {
    pub(crate) prefixed_name: String,  // "servername__toolname"
    pub(crate) description: String,
    pub(crate) input_schema: serde_json::Value,
    pub(crate) server_name: String,
    pub(crate) original_name: String,
}

/// rmcp service handle type alias
type McpService = rmcp::service::RunningService<rmcp::RoleClient, ()>;

/// A connected MCP server with its discovered tools.
struct McpConnection {
    service: McpService,
    tools: Vec<McpDiscoveredTool>,
}

/// MCP client manager — connect to servers, discover tools.
pub struct McpManager {
    connections: HashMap<String, McpConnection>,
}

impl McpManager {
    /// Connect to all configured MCP servers and discover tools.
    pub async fn connect(servers: &[McpServerConfig]) -> Self {
        let mut connections = HashMap::new();

        for server in servers {
            if !server.enabled {
                info!("[MCP] Server '{}' disabled, skipping", server.name);
                continue;
            }

            match Self::connect_server(server).await {
                Ok(conn) => {
                    info!("[MCP] Connected to '{}', {} tools", server.name, conn.tools.len());
                    connections.insert(server.name.clone(), conn);
                }
                Err(e) => {
                    warn!("[MCP] Failed to connect to '{}': {}", server.name, e);
                }
            }
        }

        Self { connections }
    }

    async fn connect_server(config: &McpServerConfig) -> Result<McpConnection> {
        if config.command.is_empty() {
            anyhow::bail!("MCP server '{}' has an empty command — check config", config.name);
        }

        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args);
        for (k, v) in &config.env {
            cmd.env(k, v);
        }

        let handshake_timeout = std::time::Duration::from_secs(30);
        let service = tokio::time::timeout(handshake_timeout, async {
            ().serve(
                TokioChildProcess::new(cmd)
                    .context("Failed to create child process transport")?,
            )
            .await
            .context("MCP service init failed")
        })
        .await
        .map_err(|_| anyhow::anyhow!(
            "MCP server '{}' handshake timed out after 30s", config.name
        ))??;

        let list_timeout = std::time::Duration::from_secs(15);
        let all_tools = tokio::time::timeout(list_timeout, service.list_all_tools())
            .await
            .map_err(|_| anyhow::anyhow!(
                "MCP server '{}' tool listing timed out after 15s", config.name
            ))?
            .context("Failed to list tools")?;

        let mut discovered = Vec::new();
        for tool in all_tools {
            let prefixed = format!("{}__{}", config.name.replace('-', "_"), tool.name);
            discovered.push(McpDiscoveredTool {
                prefixed_name: prefixed,
                description: tool.description.map(|d| d.to_string()).unwrap_or_default(),
                input_schema: serde_json::to_value(&tool.input_schema)
                    .unwrap_or_else(|_| serde_json::json!({"type": "object"})),
                server_name: config.name.clone(),
                original_name: tool.name.to_string(),
            });
        }

        Ok(McpConnection { service, tools: discovered })
    }

    /// Get all discovered tools for registration.
    pub fn take_tool_proxies(self) -> Vec<McpToolProxy> {
        let mut proxies = Vec::new();
        for (_server_name, conn) in self.connections {
            let service = Arc::new(conn.service);
            for tool in conn.tools {
                proxies.push(McpToolProxy {
                    tool,
                    service: service.clone(),
                });
            }
        }
        proxies
    }
}

/// A proxy that implements Tool trait by calling a remote MCP tool.
pub struct McpToolProxy {
    pub(crate) tool: McpDiscoveredTool,
    service: Arc<McpService>,
}

impl std::fmt::Debug for McpToolProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "McpToolProxy({})", self.tool.prefixed_name)
    }
}

#[async_trait::async_trait]
impl crate::tools::Tool for McpToolProxy {
    fn name(&self) -> &str { &self.tool.prefixed_name }
    fn description(&self) -> &str { &self.tool.description }
    fn parameters_schema(&self) -> serde_json::Value { self.tool.input_schema.clone() }
    fn required_autonomy(&self) -> crate::telos::Autonomy { crate::telos::Autonomy::Act }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &crate::tools::ToolContext,
    ) -> Result<crate::tools::ToolResult, String> {
        let result = self.service
            .call_tool(CallToolRequestParams {
                name: self.tool.original_name.clone().into(),
                arguments: params.as_object().cloned(),
                meta: None,
                task: None,
            })
            .await
            .map_err(|e| format!("MCP tool call failed: {e}"))?;

        let mut output = String::new();
        let is_error = result.is_error.unwrap_or(false);
        for content in &result.content {
            match &content.raw {
                rmcp::model::RawContent::Text(text) => {
                    if !output.is_empty() { output.push('\n'); }
                    output.push_str(&text.text);
                    if output.len() > 100_000 {
                        output.push_str("\n[truncated at 100KB]");
                        break;
                    }
                }
                rmcp::model::RawContent::Image(_) => {
                    warn!("[MCP] Tool '{}' returned image content — not yet supported, skipping",
                        self.tool.prefixed_name);
                    if !output.is_empty() { output.push('\n'); }
                    output.push_str("[image content not supported]");
                }
                rmcp::model::RawContent::Resource(_) => {
                    warn!("[MCP] Tool '{}' returned embedded resource content — skipping",
                        self.tool.prefixed_name);
                    if !output.is_empty() { output.push('\n'); }
                    output.push_str("[embedded resource content not supported]");
                }
                rmcp::model::RawContent::Audio(_) => {
                    warn!("[MCP] Tool '{}' returned audio content — not yet supported, skipping",
                        self.tool.prefixed_name);
                    if !output.is_empty() { output.push('\n'); }
                    output.push_str("[audio content not supported]");
                }
                rmcp::model::RawContent::ResourceLink(_) => {
                    warn!("[MCP] Tool '{}' returned resource link — skipping",
                        self.tool.prefixed_name);
                    if !output.is_empty() { output.push('\n'); }
                    output.push_str("[resource link content not supported]");
                }
            }
        }

        Ok(crate::tools::ToolResult { content: output, is_error })
    }
}
