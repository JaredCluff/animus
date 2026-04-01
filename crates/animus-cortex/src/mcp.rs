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
use tokio::process::Command;
use tracing::{info, warn};

/// A discovered tool from an MCP server.
#[derive(Debug, Clone)]
pub struct McpDiscoveredTool {
    pub prefixed_name: String,  // "servername__toolname"
    pub description: String,
    pub input_schema: serde_json::Value,
    pub server_name: String,
    pub original_name: String,
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
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args);
        for (k, v) in &config.env {
            cmd.env(k, v);
        }

        let service = ()
            .serve(TokioChildProcess::new(cmd)
                .context("Failed to create child process transport")?)
            .await
            .context("MCP service init failed")?;

        let all_tools = service
            .list_all_tools()
            .await
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
            use std::sync::Arc;
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
    pub tool: McpDiscoveredTool,
    service: std::sync::Arc<McpService>,
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
            if let rmcp::model::RawContent::Text(ref text) = content.raw {
                if !output.is_empty() { output.push('\n'); }
                output.push_str(&text.text);
                if output.len() > 100_000 {
                    output.push_str("\n[truncated at 100KB]");
                    break;
                }
            }
        }

        Ok(crate::tools::ToolResult { content: output, is_error })
    }
}
