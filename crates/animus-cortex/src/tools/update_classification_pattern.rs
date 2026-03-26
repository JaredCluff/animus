//! Introspective tool: update_classification_pattern
//!
//! Allows the AILF reasoning thread to refine classification patterns based on
//! observed misclassification. The classifier is immediately rebuilt after update.

use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

pub struct UpdateClassificationPatternTool;

#[async_trait::async_trait]
impl Tool for UpdateClassificationPatternTool {
    fn name(&self) -> &str { "update_classification_pattern" }

    fn description(&self) -> &str {
        "Update the keyword patterns for a task class. Keywords are used by the heuristic \
         classifier to route inputs. Use this when you observe inputs being misclassified \
         (e.g., a technical question being routed as Conversational). The classifier is \
         rebuilt immediately after the update."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "class_name": {
                    "type": "string",
                    "description": "Task class to update (must already exist in the plan)"
                },
                "keywords": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Replacement keyword list for this class (case-insensitive)"
                }
            },
            "required": ["class_name", "keywords"]
        })
    }

    fn required_autonomy(&self) -> Autonomy { Autonomy::Suggest }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let Some(router) = &ctx.smart_router else {
            return Ok(ToolResult {
                content: "Smart router not initialized.".to_string(),
                is_error: true,
            });
        };

        let class_name = params["class_name"].as_str().ok_or("missing class_name")?;
        let keywords: Vec<String> = params["keywords"]
            .as_array()
            .ok_or("keywords must be an array")?
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();

        if keywords.is_empty() {
            return Ok(ToolResult {
                content: "Keywords list is empty — provide at least one keyword.".to_string(),
                is_error: true,
            });
        }

        // Update in-memory classifier
        {
            let classifier_arc = router.classifier();
            let mut classifier = classifier_arc.write().await;
            classifier.update_pattern(class_name, keywords.clone());
        }

        // Also update keywords in the plan's task_classes for persistence
        {
            let plan_arc = router.plan();
            let mut plan = plan_arc.write().await;
            if let Some(tc) = plan.task_classes.iter_mut().find(|tc| tc.name == class_name) {
                tc.keywords = keywords.clone();
            }
        }

        tracing::info!("Classification pattern for '{}' updated: {} keywords", class_name, keywords.len());

        Ok(ToolResult {
            content: format!(
                "Pattern for '{}' updated with {} keywords: {}",
                class_name,
                keywords.len(),
                keywords.join(", ")
            ),
            is_error: false,
        })
    }
}
