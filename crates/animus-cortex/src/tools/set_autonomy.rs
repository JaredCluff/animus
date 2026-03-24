//! Autonomy mode tool — change Animus's autonomy level at runtime.
//!
//! Allows the user to say "switch to goal-directed mode" or "go reactive"
//! via any channel, and have the change take effect immediately.

use super::{Tool, ToolContext, ToolResult};
use crate::telos::Autonomy;

pub struct SetAutonomyTool;

#[async_trait::async_trait]
impl Tool for SetAutonomyTool {
    fn name(&self) -> &str {
        "set_autonomy"
    }

    fn description(&self) -> &str {
        "Change Animus's autonomy mode at runtime. \
        Use when the user asks to switch modes, e.g. 'go reactive', 'switch to goal-directed mode', \
        'enable full autonomy'. The change takes effect immediately and persists until changed again.\n\n\
        Modes:\n\
        - reactive: Only responds when messaged. No background actions.\n\
        - goal_directed: Has standing goals, acts on them independently. Responds to messages.\n\
        - full: 24/7 autonomous action within configured permissions. May send unprompted messages."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "mode": {
                    "type": "string",
                    "enum": ["reactive", "goal_directed", "full"],
                    "description": "The autonomy mode to switch to."
                },
                "reason": {
                    "type": "string",
                    "description": "Optional: why this mode is being selected (for logging and memory)."
                }
            },
            "required": ["mode"]
        })
    }

    fn required_autonomy(&self) -> Autonomy {
        Autonomy::Inform // Always allowed — this is a meta-control tool
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, String> {
        let mode_str = params["mode"]
            .as_str()
            .ok_or("missing mode parameter")?;

        let mode: animus_core::config::AutonomyMode = mode_str
            .parse()
            .map_err(|e| format!("invalid mode: {e}"))?;

        // Signal the runtime via the autonomy watch channel
        if let Some(tx) = &ctx.autonomy_tx {
            tx.send(mode)
                .map_err(|_| "autonomy channel closed".to_string())?;
        }

        let reason = params["reason"].as_str().unwrap_or("user request");
        let msg = format!("Autonomy mode set to '{}'. Reason: {reason}", mode);
        tracing::info!("{msg}");

        Ok(ToolResult { content: msg, is_error: false })
    }
}
