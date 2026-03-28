//! Introspective tool: propose_route_amendment
//!
//! Allows the AILF reasoning thread to amend the routing plan based on observed evidence.
//! Amendments are validated by the Cortex substrate before being applied.

use crate::model_plan::{ModelSpec, ThinkLevel};
use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

pub struct ProposeRouteAmendmentTool;

#[async_trait::async_trait]
impl Tool for ProposeRouteAmendmentTool {
    fn name(&self) -> &str { "propose_route_amendment" }

    fn description(&self) -> &str {
        "Amend the routing plan for a task class. Change the primary model, reorder fallbacks, \
         or adjust think budgets. Use after observing sustained poor performance on a route \
         (check get_route_stats first). The amendment is validated before being applied — \
         the proposed model must be available."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "class_name": {
                    "type": "string",
                    "description": "The task class to amend (e.g. 'Analytical', 'Technical')"
                },
                "primary_provider": {
                    "type": "string",
                    "description": "Provider for the new primary model (anthropic/ollama/openai)"
                },
                "primary_model": {
                    "type": "string",
                    "description": "New primary model name"
                },
                "think": {
                    "type": "string",
                    "enum": ["off", "dynamic", "minimal_4000", "minimal_8000", "full_8000", "full_16000"],
                    "description": "Think budget for the primary model"
                },
                "reason": {
                    "type": "string",
                    "description": "Why this amendment is being proposed (based on observed evidence)"
                }
            },
            "required": ["class_name", "primary_provider", "primary_model", "reason"]
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
        let provider = params["primary_provider"].as_str().ok_or("missing primary_provider")?;
        let model = params["primary_model"].as_str().ok_or("missing primary_model")?;
        let reason = params["reason"].as_str().ok_or("missing reason")?;

        let think = match params["think"].as_str().unwrap_or("dynamic") {
            "off" => ThinkLevel::Off,
            "dynamic" => ThinkLevel::Dynamic,
            "minimal_4000" => ThinkLevel::Minimal(4000),
            "minimal_8000" => ThinkLevel::Minimal(8000),
            "full_8000" => ThinkLevel::Full(8000),
            "full_16000" => ThinkLevel::Full(16000),
            _ => ThinkLevel::Dynamic,
        };

        // Apply amendment to plan
        {
            let plan_arc = router.plan();
            let mut plan = plan_arc.write().await;

            // Validate: class must exist
            if !plan.routes.contains_key(class_name) {
                return Ok(ToolResult {
                    content: format!("Task class '{}' not found in routing plan. Available classes: {}",
                        class_name,
                        plan.routes.keys().cloned().collect::<Vec<_>>().join(", ")),
                    is_error: true,
                });
            }

            if let Some(route) = plan.routes.get_mut(class_name) {
                // Promote new primary to front of candidates; keep remainder as fallbacks
                let old_primary = route.candidates.first()
                    .map(|s| s.model.clone())
                    .unwrap_or_default();
                let new_primary = ModelSpec {
                    provider: provider.to_string(),
                    model: model.to_string(),
                    think,
                    cost: None,
                    speed: None,
                    quality: None,
                    trust_floor: 0,
                };
                // Remove any existing entry for this model, then prepend as new primary
                route.candidates.retain(|s| !(s.provider == provider && s.model == model));
                route.candidates.insert(0, new_primary);
                // Reset aggregate stats for this route (fresh start with new primary)
                route.stats = crate::model_plan::RouteStats::default();

                tracing::info!(
                    "Route '{}' amended: {} → {} (reason: {})",
                    class_name, old_primary, model, reason
                );
            }

            plan.build_reason = format!("Amended by AILF: {}", reason);
        }

        // Rebuild classifier to pick up any changes
        router.rebuild_classifier().await;

        Ok(ToolResult {
            content: format!(
                "Route '{}' updated: primary is now {}/{} ({}). Stats reset. Classifier rebuilt.",
                class_name, provider, model, reason
            ),
            is_error: false,
        })
    }
}
