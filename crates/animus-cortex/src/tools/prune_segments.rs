use animus_core::segment::{DecayClass, Source};
use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

/// Bulk deletion of VectorFS segments by filter criteria.
pub struct PruneSegmentsTool;

#[async_trait::async_trait]
impl Tool for PruneSegmentsTool {
    fn name(&self) -> &str { "prune_segments" }
    fn description(&self) -> &str {
        "Bulk-delete memory segments matching filter criteria. At least one filter is required. \
         Automatically takes a snapshot before deleting if more than 10 segments are matched. \
         Use dry_run=true to preview what would be deleted without actually deleting. \
         Filters are ANDed together. Use delete_segment for precise single-segment deletion."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "dry_run": {
                    "type": "boolean",
                    "description": "If true, show what would be deleted without actually deleting. Default: false."
                },
                "source": {
                    "type": "string",
                    "description": "Filter by source type: 'manual', 'conversation', 'observation', 'consolidation', 'federation', 'self_derived'"
                },
                "decay_class": {
                    "type": "string",
                    "description": "Filter by decay class: 'factual', 'procedural', 'episodic', 'opinion', 'general'"
                },
                "tag_key": {
                    "type": "string",
                    "description": "Filter by tag key (used with tag_value)"
                },
                "tag_value": {
                    "type": "string",
                    "description": "Filter by tag value (used with tag_key)"
                },
                "older_than_days": {
                    "type": "number",
                    "description": "Delete segments created more than N days ago"
                },
                "confidence_below": {
                    "type": "number",
                    "description": "Delete segments with confidence score below this threshold (0.0–1.0)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of segments to delete in one call (default: 500)"
                }
            }
        })
    }
    fn required_autonomy(&self) -> Autonomy { Autonomy::Act }
    fn needs_vectorfs(&self) -> bool { true }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let dry_run = params["dry_run"].as_bool().unwrap_or(false);

        // Require at least one filter to prevent accidental full wipe
        let has_filter = params["source"].is_string()
            || params["decay_class"].is_string()
            || params["tag_key"].is_string()
            || params["older_than_days"].is_number()
            || params["confidence_below"].is_number();

        if !has_filter {
            return Ok(ToolResult {
                content: "At least one filter is required (source, decay_class, tag_key, older_than_days, or confidence_below). \
                          Use list_segments to inspect memory before pruning.".to_string(),
                is_error: true,
            });
        }

        let filter_source = params["source"].as_str();
        let filter_decay = params["decay_class"].as_str();
        let filter_tag_key = params["tag_key"].as_str();
        let filter_tag_val = params["tag_value"].as_str();
        let filter_older_days = params["older_than_days"].as_f64();
        let filter_conf_below = params["confidence_below"].as_f64().map(|v| v as f32);
        let limit = params["limit"].as_u64().unwrap_or(500) as usize;

        let now = chrono::Utc::now();

        // Gather candidates using get_raw (no access count side-effects)
        let all_ids = ctx.store.segment_ids(None);
        let mut matched = Vec::new();

        for id in &all_ids {
            let seg = match ctx.store.get_raw(*id) {
                Ok(Some(s)) => s,
                _ => continue,
            };

            // Source filter
            if let Some(src_filter) = filter_source {
                let matches = match src_filter {
                    "manual" => matches!(seg.source, Source::Manual { .. }),
                    "conversation" => matches!(seg.source, Source::Conversation { .. }),
                    "observation" => matches!(seg.source, Source::Observation { .. }),
                    "consolidation" => matches!(seg.source, Source::Consolidation { .. }),
                    "federation" => matches!(seg.source, Source::Federation { .. }),
                    "self_derived" => matches!(seg.source, Source::SelfDerived { .. }),
                    _ => false,
                };
                if !matches { continue; }
            }

            // Decay class filter
            if let Some(dc_filter) = filter_decay {
                let expected = match dc_filter {
                    "factual" => DecayClass::Factual,
                    "procedural" => DecayClass::Procedural,
                    "episodic" => DecayClass::Episodic,
                    "opinion" => DecayClass::Opinion,
                    _ => DecayClass::General,
                };
                if seg.decay_class != expected { continue; }
            }

            // Tag filter
            if let Some(key) = filter_tag_key {
                match filter_tag_val {
                    Some(val) => {
                        if seg.tags.get(key).map(|v| v.as_str()) != Some(val) { continue; }
                    }
                    None => {
                        if !seg.tags.contains_key(key) { continue; }
                    }
                }
            }

            // Age filter
            if let Some(days) = filter_older_days {
                let age = now - seg.created;
                if age.num_seconds() < (days * 86400.0) as i64 { continue; }
            }

            // Confidence filter
            if let Some(threshold) = filter_conf_below {
                if seg.confidence >= threshold { continue; }
            }

            matched.push(seg.id);
            if matched.len() >= limit { break; }
        }

        if matched.is_empty() {
            return Ok(ToolResult {
                content: "No segments matched the given filters.".to_string(),
                is_error: false,
            });
        }

        if dry_run {
            return Ok(ToolResult {
                content: format!(
                    "dry_run: would delete {} segment(s). Run without dry_run=true to proceed.",
                    matched.len()
                ),
                is_error: false,
            });
        }

        // Auto-snapshot before bulk deletion
        let mut snapshot_note = String::new();
        if matched.len() > 10 {
            let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
            let snap_path = ctx.snapshot_dir.join(format!("pre-prune-{timestamp}"));
            match ctx.store.snapshot(&snap_path) {
                Ok(count) => {
                    snapshot_note = format!(" (snapshot saved: {count} segments at {})", snap_path.display());
                }
                Err(e) => {
                    return Ok(ToolResult {
                        content: format!(
                            "Refusing to prune: auto-snapshot failed before bulk deletion: {e}. \
                             Fix the snapshot issue or use delete_segment for individual deletions."
                        ),
                        is_error: true,
                    });
                }
            }
        }

        let total = matched.len();
        let mut deleted = 0;
        let mut errors = Vec::new();
        for id in matched {
            match ctx.store.delete(id) {
                Ok(()) => deleted += 1,
                Err(e) => errors.push(format!("{}: {e}", id.0)),
            }
        }

        let mut msg = format!("Pruned {deleted}/{total} segments{snapshot_note}.");
        if !errors.is_empty() {
            msg.push_str(&format!(" {} deletion error(s): {}", errors.len(), errors.join("; ")));
        }
        Ok(ToolResult { content: msg, is_error: !errors.is_empty() })
    }
}
