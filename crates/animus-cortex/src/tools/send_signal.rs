use animus_core::identity::ThreadId;
use animus_core::threading::{Signal, SignalPriority};
use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

pub struct SendSignalTool;

#[async_trait::async_trait]
impl Tool for SendSignalTool {
    fn name(&self) -> &str { "send_signal" }
    fn description(&self) -> &str {
        "Send a signal to the runtime's signal bus. Signals are broadcast to all active \
        reasoning threads (the runtime routes them). Use priority 'urgent' for time-sensitive \
        events, 'normal' for typical inter-thread communication, 'info' for low-priority notes."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "priority": { "type": "string", "enum": ["info", "normal", "urgent"], "description": "Signal priority" },
                "message": { "type": "string", "description": "Signal content" }
            },
            "required": ["message"]
        })
    }
    fn required_autonomy(&self) -> Autonomy { Autonomy::Inform }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let priority_str = params["priority"].as_str().unwrap_or("normal");
        let message = params["message"].as_str().ok_or("missing 'message'")?;
        const MAX_SIGNAL_MESSAGE_BYTES: usize = 4 * 1024; // 4 KiB
        if message.len() > MAX_SIGNAL_MESSAGE_BYTES {
            return Ok(ToolResult {
                content: format!(
                    "Message too large: {} bytes (max {MAX_SIGNAL_MESSAGE_BYTES}). Summarize before sending.",
                    message.len()
                ),
                is_error: true,
            });
        }

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
                    content: format!("Signal sent (priority: {priority_str})"),
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
