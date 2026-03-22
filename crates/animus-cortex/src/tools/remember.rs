use animus_core::segment::{Content, DecayClass, Segment, Source};
use crate::telos::Autonomy;
use super::{Tool, ToolResult, ToolContext};

pub struct RememberTool;

#[async_trait::async_trait]
impl Tool for RememberTool {
    fn name(&self) -> &str { "remember" }
    fn description(&self) -> &str { "Store a piece of knowledge in persistent memory (VectorFS)." }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "knowledge": { "type": "string", "description": "The knowledge to store" },
                "decay_class": { "type": "string", "enum": ["factual", "procedural", "episodic", "opinion", "general"], "description": "Knowledge type" }
            },
            "required": ["knowledge"]
        })
    }
    fn required_autonomy(&self) -> Autonomy { Autonomy::Suggest }
    fn needs_vectorfs(&self) -> bool { true }

    async fn execute(&self, params: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let knowledge = params["knowledge"].as_str().ok_or("missing 'knowledge' parameter")?;
        const MAX_KNOWLEDGE_BYTES: usize = 10 * 1024; // 10 KiB — keeps embeddings tractable
        if knowledge.len() > MAX_KNOWLEDGE_BYTES {
            return Ok(ToolResult {
                content: format!("Knowledge too large: {} bytes (max {} bytes). Summarize before storing.", knowledge.len(), MAX_KNOWLEDGE_BYTES),
                is_error: true,
            });
        }
        let decay_class_str = params["decay_class"].as_str().unwrap_or("general");
        let decay_class = match decay_class_str {
            "factual" => DecayClass::Factual,
            "procedural" => DecayClass::Procedural,
            "episodic" => DecayClass::Episodic,
            "opinion" => DecayClass::Opinion,
            _ => DecayClass::General,
        };

        let embedding = match ctx.embedder.embed_text(knowledge).await {
            Ok(e) => e,
            Err(e) => return Ok(ToolResult { content: format!("Embedding failed: {e}"), is_error: true }),
        };

        let mut segment = Segment::new(
            Content::Text(knowledge.to_string()),
            embedding,
            Source::Manual { description: "LLM tool-remembered knowledge".to_string() },
        );
        segment.decay_class = decay_class;

        match ctx.store.store(segment) {
            Ok(id) => Ok(ToolResult {
                content: format!("Knowledge stored (id: {id}, class: {decay_class_str})"),
                is_error: false,
            }),
            Err(e) => Ok(ToolResult {
                content: format!("Failed to store knowledge: {e}"),
                is_error: true,
            }),
        }
    }
}
