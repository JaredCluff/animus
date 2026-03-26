//! Introspective tool: get_route_stats
//!
//! Allows the AILF reasoning thread to inspect the current routing performance
//! for all task classes. This is Layer 4 — voluntary conscious reach into the Cortex substrate.

use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

pub struct GetRouteStatsTool;

#[async_trait::async_trait]
impl Tool for GetRouteStatsTool {
    fn name(&self) -> &str { "get_route_stats" }

    fn description(&self) -> &str {
        "Inspect routing performance for all task classes. Returns turn count, success rate, \
         average latency, and correction rate per route. Use this to evaluate whether the \
         current model routing plan is working well before proposing amendments."
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
                content: "Smart router not initialized — model plan not available.".to_string(),
                is_error: false,
            });
        };

        let stats = router.route_stats_snapshot().await;
        let health = router.route_health_snapshot();

        if stats.is_empty() {
            return Ok(ToolResult {
                content: "No routing stats yet — no turns have been processed.".to_string(),
                is_error: false,
            });
        }

        let mut lines = vec!["## Route Performance\n".to_string()];
        let mut class_names: Vec<&str> = stats.keys().map(|s| s.as_str()).collect();
        class_names.sort();

        for class in class_names {
            let s = &stats[class];
            let h = health.get(class);
            let status = if h.map(|h| h.degraded).unwrap_or(false) {
                " ⚠ DEGRADED"
            } else {
                ""
            };

            lines.push(format!(
                "**{}**{}\n  turns: {}  |  success: {:.0}%  |  avg latency: {}  |  corrections: {:.0}%",
                class,
                status,
                s.turn_count,
                s.success_rate() * 100.0,
                s.avg_latency_ms().map(|ms| format!("{}ms", ms)).unwrap_or_else(|| "n/a".to_string()),
                s.correction_rate() * 100.0,
            ));
        }

        Ok(ToolResult {
            content: lines.join("\n"),
            is_error: false,
        })
    }
}
