pub mod audit;
pub mod auth;
pub mod client;
pub mod discovery;
pub mod handoff;
pub mod knowledge;
pub mod orchestrator;
pub mod peers;
pub mod protocol;
pub mod server;

pub use handoff::{HandoffBundle, HandoffSegment};
// Mesh and succession types live in animus-core (shared layer) to avoid
// a circular dependency: animus-federation → animus-cortex → animus-core.
pub use animus_core::{AttestationFields, CapabilityAttestation, MeshRole, RoleMesh, SuccessionPolicy, VerifiedAttestation};
