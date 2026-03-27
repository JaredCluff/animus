pub mod capability_registry;
pub mod engine_registry;
pub mod model_plan;
pub mod smart_router;
pub mod situational_awareness;
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

pub use capability_registry::CapabilityRegistry;
pub use engine_registry::{CognitiveRole, EngineConfig, EngineRegistry, Provider};
pub use model_plan::{HeuristicClassifier, ModelPlan, ModelSpec, Route, RouteStats, TaskClass, ThinkLevel};
pub use smart_router::{RouteDecision, RouteHealth, SmartRouter};
pub use llm::{
    AnthropicEngine, MockEngine, ReasoningEngine, ReasoningOutput, Role,
    StopReason, ToolCall, ToolDefinition, Turn, TurnContent,
};
pub use perception::{PerceptionLoop, PerceptionOutput, PerceivedEvent, PerceptionSignal, SelfEventFilter};
pub use reconstitution::{ReconstitutionContext, shutdown_reflection, boot_reconstitution};
pub use reflection::{ReflectionLoop, ReflectionOutput, Synthesis, Contradiction, GoalUpdate, ReflectionSignal};
pub use scheduler::ThreadScheduler;
pub use telos::{Autonomy, Goal, GoalManager, GoalSource, GoalStatus, Priority};
pub use thread::ReasoningThread;
pub use watcher::{Watcher, WatcherConfig, WatcherEvent, WatcherRegistry};
pub use watchers::{CapabilityProbe, CommsWatcher, ProvidersJsonWatcher, SegmentPressureWatcher, SensoriumHealthWatcher};
pub use task_manager::{TaskManager, TaskRecord, TaskState, new_task_id};
pub use situational_awareness::SituationalAwareness;
