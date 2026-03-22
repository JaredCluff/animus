use animus_core::identity::ThreadId;
use animus_core::threading::{Signal, SignalPriority};
use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

pub struct SendSignalTool;

#[async_trait::async_trait]
impl Tool for SendSignalTool {
    fn name(&self) -> &str { "send_signal" }
    fn description(&self) -> &str { "Send a signal to the active reasoning thread." }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "target_thread_prefix": { "type": "string", "description": "ID prefix of the target thread (informational)" },
                "priority": { "type": "string", "enum": ["info", "normal", "urgent"], "description": "Signal priority" },
                "message": { "type": "string", "description": "Signal content" }
            },
            "required": ["target_thread_prefix", "message"]
        })
    }
    fn required_autonomy(&self) -> Autonomy { Autonomy::Inform }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let target = params["target_thread_prefix"].as_str().ok_or("missing 'target_thread_prefix'")?;
        let priority_str = params["priority"].as_str().unwrap_or("normal");
        let message = params["message"].as_str().ok_or("missing 'message'")?;

        let priority = match priority_str {
            "urgent" => SignalPriority::Urgent,
            "info" => SignalPriority::Info,
            _ => SignalPriority::Normal,
        };

        let sig = Signal {
            source_thread: ThreadId::default(),
            target_thread: ThreadId::default(),
            priority,
            summary: message.to_string(),
            segment_refs: vec![],
            created: chrono::Utc::now(),
        };

        match &ctx.signal_tx {
            Some(tx) => match tx.send(sig).await {
                Ok(()) => Ok(ToolResult {
                    content: format!("Signal sent (target prefix: '{target}', priority: {priority_str})"),
                    is_error: false,
                }),
                Err(e) => Ok(ToolResult {
                    content: format!("Failed to send signal: {e}"),
                    is_error: true,
                }),
            },
            None => Ok(ToolResult {
                content: "Signal channel not available".to_string(),
                is_error: true,
            }),
        }
    }
}
