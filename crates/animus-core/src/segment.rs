use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::identity::{EventId, InstanceId, PolicyId, SegmentId, ThreadId};

/// How knowledge decays over time. Different knowledge types have different half-lives.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DecayClass {
    /// Verifiable facts: very slow decay (half-life: 90 days).
    Factual,
    /// How-to knowledge: moderate decay (half-life: 30 days).
    Procedural,
    /// Events and experiences: faster decay (half-life: 14 days).
    Episodic,
    /// Opinions and preferences: fast decay (half-life: 7 days).
    Opinion,
    /// Default: moderate decay (half-life: 30 days).
    #[default]
    General,
}

impl DecayClass {
    /// Half-life in seconds for this decay class.
    pub fn half_life_secs(&self) -> f64 {
        const DAY: f64 = 86400.0;
        match self {
            Self::Factual => 90.0 * DAY,
            Self::Procedural => 30.0 * DAY,
            Self::Episodic => 14.0 * DAY,
            Self::Opinion => 7.0 * DAY,
            Self::General => 30.0 * DAY,
        }
    }
}

fn default_bayesian_param() -> f32 {
    1.0
}

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
    /// Updated automatically when alpha/beta change.
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

    /// Beta distribution positive parameter (successful retrievals/acceptances).
    /// Part of Bayesian confidence tracking: confidence = alpha / (alpha + beta).
    #[serde(default = "default_bayesian_param")]
    pub alpha: f32,

    /// Beta distribution negative parameter (corrections/rejections).
    #[serde(default = "default_bayesian_param")]
    pub beta: f32,

    /// How this knowledge decays over time.
    #[serde(default)]
    pub decay_class: DecayClass,
}

impl Segment {
    /// Create a new segment with the given content and embedding.
    pub fn new(content: Content, embedding: Vec<f32>, source: Source) -> Self {
        let now = Utc::now();
        let alpha = 1.0_f32;
        let beta = 1.0_f32;
        let confidence = alpha / (alpha + beta);
        Self {
            id: SegmentId::new(),
            embedding,
            content,
            source,
            confidence,
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
            alpha,
            beta,
            decay_class: DecayClass::default(),
        }
    }

    /// Record an access, updating count and timestamp.
    pub fn record_access(&mut self) {
        self.access_count = self.access_count.saturating_add(1);
        self.last_accessed = Utc::now();
    }

    /// Record positive feedback (acceptance). Updates Bayesian parameters and confidence.
    pub fn record_positive_feedback(&mut self) {
        self.alpha += 1.0;
        self.confidence = self.bayesian_confidence();
    }

    /// Record negative feedback (correction). Updates Bayesian parameters and confidence.
    pub fn record_negative_feedback(&mut self) {
        self.beta += 1.0;
        self.confidence = self.bayesian_confidence();
    }

    /// Mean of the Beta(alpha, beta) distribution.
    /// This is the Bayesian estimate of the segment's reliability.
    pub fn bayesian_confidence(&self) -> f32 {
        if self.alpha + self.beta == 0.0 {
            return 0.5;
        }
        self.alpha / (self.alpha + self.beta)
    }

    /// Temporal decay factor based on decay class and segment age.
    /// Returns a value in (0.0, 1.0] — 1.0 for brand new, approaching 0.0 for very old.
    /// Uses exponential decay: exp(-ln(2) * age / half_life).
    pub fn temporal_decay_factor(&self) -> f32 {
        let age_secs = (Utc::now() - self.created).num_seconds().max(0) as f64;
        let half_life = self.decay_class.half_life_secs();
        let lambda = (2.0_f64).ln() / half_life;
        (-lambda * age_secs).exp() as f32
    }

    /// Composite health score combining Bayesian confidence, temporal decay,
    /// and access patterns. Range: [0.0, 1.0].
    pub fn health_score(&self) -> f32 {
        let confidence = self.bayesian_confidence();
        let decay = self.temporal_decay_factor();
        let access = (self.access_count as f32 / 10.0).sqrt().min(1.0);

        // Weighted composite: confidence matters most, decay prevents stale data,
        // access patterns provide usage signal
        (0.5 * confidence * decay + 0.3 * self.relevance_score + 0.2 * access).clamp(0.0, 1.0)
    }

    /// Infer the decay class from the segment's source type.
    /// Call after creation to auto-classify knowledge.
    pub fn infer_decay_class(&mut self) {
        self.decay_class = match &self.source {
            // Sensorium observations are events — they decay faster
            Source::Observation { .. } => DecayClass::Episodic,
            // Reasoning outputs are procedural knowledge
            Source::SelfDerived { .. } => DecayClass::Procedural,
            // Consolidated knowledge has already been validated — treat as factual
            Source::Consolidation { .. } => DecayClass::Factual,
            // Federated knowledge from other AILFs — default, unknown provenance
            Source::Federation { .. } => DecayClass::General,
            // Conversation-derived knowledge — moderate decay
            Source::Conversation { .. } => DecayClass::General,
            // Manual bootstrap or user-remembered — general
            Source::Manual { .. } => DecayClass::General,
        };
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
