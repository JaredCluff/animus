use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

/// Proactively publish a message to a NATS subject.
pub struct NatsPublishTool;

#[async_trait::async_trait]
impl Tool for NatsPublishTool {
    fn name(&self) -> &str { "nats_publish" }
    fn description(&self) -> &str {
        "Publish a message to a NATS subject. Use this to proactively send messages to other \
         systems connected to NATS — e.g., triggering pipelines, broadcasting status, or \
         communicating with other Animus instances. You receive inbound messages on \
         animus.in.* — replies are handled automatically. Use this for proactive outbound \
         messages to any subject."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "subject": {
                    "type": "string",
                    "description": "NATS subject to publish to (e.g. 'animus.out.claude', 'events.status')"
                },
                "payload": {
                    "type": "string",
                    "description": "Message payload (UTF-8 text)"
                },
                "conversation_id": {
                    "type": "string",
                    "description": "Optional: principal ID of the originating conversation (e.g. 'jared'). When set, Animus's response will be routed back to that conversation's thread."
                }
            },
            "required": ["subject", "payload"]
        })
    }
    fn required_autonomy(&self) -> Autonomy { Autonomy::Act }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let subject = params["subject"].as_str().ok_or("missing 'subject' parameter")?;
        let payload = params["payload"].as_str().ok_or("missing 'payload' parameter")?;
        let conversation_id = params["conversation_id"].as_str();

        let client = match &ctx.nats_client {
            Some(c) => c,
            None => return Ok(ToolResult {
                content: "NATS is not configured on this Animus instance. Set ANIMUS_NATS_URL or \
                          enable channels.nats in config to use nats_publish.".to_string(),
                is_error: true,
            }),
        };

        // Wrap payload with routing metadata when a conversation_id is provided,
        // so the responder can route the reply back to the originating conversation thread.
        let wire_payload = if let Some(cid) = conversation_id {
            serde_json::json!({
                "payload": payload,
                "x-conversation-id": cid,
            }).to_string()
        } else {
            payload.to_string()
        };

        match client.publish(subject.to_string(), wire_payload.as_bytes().to_vec().into()).await {
            Ok(()) => Ok(ToolResult {
                content: format!("Published to '{subject}': {}", &payload[..payload.len().min(200)]),
                is_error: false,
            }),
            Err(e) => Ok(ToolResult {
                content: format!("NATS publish to '{subject}' failed: {e}"),
                is_error: true,
            }),
        }
    }
}
