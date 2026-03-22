pub mod config;
pub mod embedding;
pub mod error;
pub mod identity;
pub mod segment;
pub mod sensorium;
pub mod threading;
pub mod tier;

pub use config::{AnimusConfig, CortexConfig, EmbeddingConfig, EmbeddingTier, FederationConfig, InterfaceConfig, MnemosConfig, SensoriumConfig, VectorFSConfig};
pub use embedding::EmbeddingService;
pub use error::{AnimusError, Result};
pub use identity::{AnimusIdentity, EventId, GoalId, InstanceId, PolicyId, SegmentId, SnapshotId, ThreadId};
pub use segment::{Content, DecayClass, Principal, Segment, Source, Tier};
pub use threading::{ThreadStatus, Signal, SignalPriority};
pub use tier::TierConfig;
