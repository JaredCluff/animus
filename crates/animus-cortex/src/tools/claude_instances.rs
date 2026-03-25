use super::{Tool, ToolContext, ToolResult};
use crate::telos::Autonomy;
use async_nats::jetstream;
use futures::StreamExt;

const AGENTS_KV_BUCKET: &str = "agents-registry";

/// List Claude Code instances that are currently registered in the agent registry.
///
/// Instances self-register at startup via nuntius (NUNTIUS_INSTANCE_ID env var).
/// Use the returned instance IDs to target specific Claude Code sessions via NATS:
///   - Inbound (Animus → Claude): `claude.{instance_id}.in.{topic}`
///   - Outbound (Claude → Animus): `claude.{instance_id}.out.{topic}`
pub struct ClaudeInstancesTool;

#[async_trait::async_trait]
impl Tool for ClaudeInstancesTool {
    fn name(&self) -> &str {
        "claude_instances"
    }

    fn description(&self) -> &str {
        "List active Claude Code instances registered in the agent registry. \
         Each instance registers itself at startup via nuntius (NUNTIUS_INSTANCE_ID). \
         Returns instance IDs, last-seen timestamps, and the subjects to use for targeting. \
         To send a task to a specific instance: nats_publish('claude.{instance_id}.in.task', payload). \
         Animus receives responses on claude.{instance_id}.out.{topic} (subscribe via ANIMUS_NATS_EXTRA_SUBJECTS=claude.*.out.>)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    fn required_autonomy(&self) -> Autonomy {
        Autonomy::Inform
    }

    async fn execute(
        &self,
        _params: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, String> {
        let client = match &ctx.nats_client {
            Some(c) => c.clone(),
            None => {
                return Ok(ToolResult {
                    content: "NATS is not configured. Set ANIMUS_NATS_URL to enable.".to_string(),
                    is_error: true,
                })
            }
        };

        let js = jetstream::new(client);

        let kv = match js.get_key_value(AGENTS_KV_BUCKET).await {
            Ok(kv) => kv,
            Err(_) => {
                // Bucket doesn't exist yet — no agents have registered
                return Ok(ToolResult {
                    content: serde_json::json!({
                        "instances": [],
                        "count": 0,
                        "note": "No agents have registered yet. Ensure nuntius is running with NUNTIUS_INSTANCE_ID set."
                    })
                    .to_string(),
                    is_error: false,
                });
            }
        };

        let keys_stream = match kv.keys().await {
            Ok(s) => s,
            Err(e) => {
                return Ok(ToolResult {
                    content: format!("Failed to list agent registry: {e}"),
                    is_error: true,
                })
            }
        };

        let mut keys: Vec<String> = Vec::new();
        let mut stream = keys_stream;
        while let Some(key) = stream.next().await {
            if let Ok(k) = key {
                keys.push(k);
            }
        }

        let mut instances = Vec::new();
        for key in keys {
            let entry = match kv.entry(&key).await {
                Ok(Some(e)) => e,
                _ => continue,
            };

            if entry.operation != jetstream::kv::Operation::Put {
                continue;
            }

            let record: serde_json::Value = match serde_json::from_slice(&entry.value) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let caps: Vec<&str> = record["capabilities"]
                .as_array()
                .map(|a| a.iter().filter_map(|c| c.as_str()).collect())
                .unwrap_or_default();

            if !caps.contains(&"claude-code") {
                continue;
            }

            let id = record["agent_id"].as_str().unwrap_or(&key).to_string();
            instances.push(serde_json::json!({
                "instance_id": id,
                "last_seen": record["last_seen"],
                "metadata": record["metadata"],
                "inbound_subject": format!("claude.{id}.in.{{topic}}"),
                "outbound_subject": format!("claude.{id}.out.{{topic}}"),
            }));
        }

        let count = instances.len();
        Ok(ToolResult {
            content: serde_json::json!({
                "instances": instances,
                "count": count,
                "targeting": "nats_publish('claude.{instance_id}.in.task', payload, conversation_id?)"
            })
            .to_string(),
            is_error: false,
        })
    }
}
