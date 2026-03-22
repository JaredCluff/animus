pub mod config;
pub mod embedding;
pub mod error;
pub mod identity;
pub mod segment;
pub mod tier;

pub use config::{AnimusConfig, EmbeddingConfig, EmbeddingTier, MnemosConfig, VectorFSConfig};
pub use embedding::EmbeddingService;
pub use error::{AnimusError, Result};
pub use identity::{EventId, GoalId, InstanceId, PolicyId, SegmentId, SnapshotId, ThreadId};
pub use segment::{Content, Principal, Segment, Source, Tier};
pub use tier::TierConfig;
