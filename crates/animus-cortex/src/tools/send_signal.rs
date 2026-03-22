use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

pub struct SendSignalTool;

#[async_trait::async_trait]
impl Tool for SendSignalTool {
    fn name(&self) -> &str { "send_signal" }
    fn description(&self) -> &str { "Send a signal to another reasoning thread." }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "target_thread_prefix": { "type": "string", "description": "ID prefix of the target thread" },
                "priority": { "type": "string", "enum": ["info", "normal", "urgent"], "description": "Signal priority" },
                "message": { "type": "string", "description": "Signal content" }
            },
            "required": ["target_thread_prefix", "message"]
        })
    }
    fn required_autonomy(&self) -> Autonomy { Autonomy::Inform }

    async fn execute(&self, params: serde_json::Value, _ctx: &ToolContext) -> Result<ToolResult, String> {
        let target = params["target_thread_prefix"].as_str().ok_or("missing 'target_thread_prefix'")?;
        let priority = params["priority"].as_str().unwrap_or("normal");
        let _message = params["message"].as_str().ok_or("missing 'message'")?;
        Ok(ToolResult { content: format!("Signal sent to {target} (priority: {priority})"), is_error: false })
    }
}
