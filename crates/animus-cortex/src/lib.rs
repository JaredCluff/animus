pub mod engine_registry;
pub mod task_manager;
pub mod watcher;
pub mod watchers;
pub mod llm;
pub mod perception;
pub mod reconstitution;
pub mod reflection;
pub mod scheduler;
pub mod telos;
pub mod thread;
pub mod tools;

pub use engine_registry::{CognitiveRole, EngineConfig, EngineRegistry, Provider};
pub use llm::{
    AnthropicEngine, MockEngine, ReasoningEngine, ReasoningOutput, Role,
    StopReason, ToolCall, ToolDefinition, Turn, TurnContent,
};
pub use perception::{PerceptionLoop, PerceptionOutput, PerceivedEvent, PerceptionSignal};
pub use reconstitution::{ReconstitutionContext, shutdown_reflection, boot_reconstitution};
pub use reflection::{ReflectionLoop, ReflectionOutput, Synthesis, Contradiction, GoalUpdate, ReflectionSignal};
pub use scheduler::ThreadScheduler;
pub use telos::{Autonomy, Goal, GoalManager, GoalSource, GoalStatus, Priority};
pub use thread::ReasoningThread;
pub use watcher::{Watcher, WatcherConfig, WatcherEvent, WatcherRegistry};
pub use watchers::CommsWatcher;
