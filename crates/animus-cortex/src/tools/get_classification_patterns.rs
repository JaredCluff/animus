//! Introspective tool: get_classification_patterns
//!
//! Shows the AILF reasoning thread how inputs are currently being classified.

use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

pub struct GetClassificationPatternsTool;

#[async_trait::async_trait]
impl Tool for GetClassificationPatternsTool {
    fn name(&self) -> &str { "get_classification_patterns" }

    fn description(&self) -> &str {
        "Inspect the current heuristic classification patterns used to route inputs to models. \
         Shows all task classes and their keywords. Use this to identify misclassification \
         before updating patterns with update_classification_pattern."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    fn required_autonomy(&self) -> Autonomy { Autonomy::Inform }

    async fn execute(&self, _params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let Some(router) = &ctx.smart_router else {
            return Ok(ToolResult {
                content: "Smart router not initialized.".to_string(),
                is_error: false,
            });
        };

        let classifier = router.classifier();
        let classifier = classifier.read().await;
        let patterns = classifier.patterns();

        if patterns.is_empty() {
            return Ok(ToolResult {
                content: "No classification patterns loaded.".to_string(),
                is_error: false,
            });
        }

        let mut lines = vec!["## Classification Patterns\n".to_string()];
        for (class, keywords) in patterns {
            lines.push(format!("**{}**: {}", class, keywords.join(", ")));
        }

        Ok(ToolResult {
            content: lines.join("\n"),
            is_error: false,
        })
    }
}
