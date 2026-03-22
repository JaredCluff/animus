use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::identity::{EventId, InstanceId, PolicyId, SegmentId, ThreadId};

/// The atomic unit of VectorFS storage. A unit of meaning with context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    /// Unique, immutable identifier.
    pub id: SegmentId,

    /// Vector representation of content.
    pub embedding: Vec<f32>,

    /// The actual knowledge stored.
    pub content: Content,

    /// Where this segment came from.
    pub source: Source,

    /// Validation level (0.0 - 1.0). Higher = more trusted.
    pub confidence: f32,

    /// Parent segments (for consolidation tracking).
    pub lineage: Vec<SegmentId>,

    /// Current storage tier.
    pub tier: Tier,

    /// Current computed relevance score.
    pub relevance_score: f32,

    /// How many times this segment has been retrieved.
    pub access_count: u64,

    /// Last time this segment was accessed.
    pub last_accessed: DateTime<Utc>,

    /// When this segment was created.
    pub created: DateTime<Utc>,

    /// Weighted links to related segments.
    pub associations: Vec<(SegmentId, f32)>,

    /// Which consent rule permitted creation.
    pub consent_policy: Option<PolicyId>,

    /// Who can see this segment.
    pub observable_by: Vec<Principal>,

    /// User-defined key-value labels for categorization and federation scoping.
    #[serde(default)]
    pub tags: HashMap<String, String>,
}

impl Segment {
    /// Create a new segment with the given content and embedding.
    pub fn new(content: Content, embedding: Vec<f32>, source: Source) -> Self {
        let now = Utc::now();
        Self {
            id: SegmentId::new(),
            embedding,
            content,
            source,
            confidence: 0.5,
            lineage: Vec::new(),
            tier: Tier::Warm,
            relevance_score: 0.5,
            access_count: 0,
            last_accessed: now,
            created: now,
            associations: Vec::new(),
            consent_policy: None,
            observable_by: Vec::new(),
            tags: HashMap::new(),
        }
    }

    /// Record an access, updating count and timestamp.
    pub fn record_access(&mut self) {
        self.access_count += 1;
        self.last_accessed = Utc::now();
    }

    /// Estimated token count for context budgeting.
    pub fn estimated_tokens(&self) -> usize {
        match &self.content {
            Content::Text(t) => t.len() / 4, // rough estimate: 4 chars per token
            Content::Structured(v) => v.to_string().len() / 4,
            Content::Binary { .. } => 0, // binary content doesn't go into LLM context
            Content::Reference { summary, .. } => summary.len() / 4,
        }
    }
}

/// The content stored in a segment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Content {
    Text(String),
    Structured(serde_json::Value),
    Binary { mime_type: String, data: Vec<u8> },
    Reference { uri: String, summary: String },
}

/// Where a segment originated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Source {
    Conversation {
        thread_id: ThreadId,
        turn: u64,
    },
    Observation {
        event_type: String,
        raw_event_id: EventId,
    },
    Consolidation {
        merged_from: Vec<SegmentId>,
    },
    Federation {
        source_ailf: InstanceId,
        original_id: SegmentId,
    },
    SelfDerived {
        reasoning_chain: String,
    },
    /// Bootstrap or manually injected knowledge.
    Manual {
        description: String,
    },
}

/// Storage tier for a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tier {
    /// Currently loaded in reasoning context.
    Hot,
    /// Vector-indexed, retrievable in <10ms.
    Warm,
    /// Compressed, archived, retrievable but not instant.
    Cold,
}

/// An entity that can observe or be observed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Principal {
    Ailf(InstanceId),
    Human(String),
}
