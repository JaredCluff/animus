//! animus-channel — Plugin-based communication channel framework.
//!
//! Provides the ChannelBus (unified inbound/outbound message routing),
//! plugin traits for channel adapters (Telegram, email, Discord, etc.),
//! message priority routing, and prompt injection protection.

pub mod bus;
pub mod message;
pub mod nats;
pub mod permission_gate;
pub mod plugin;
pub mod router;
pub mod scanner;
pub mod telegram;

pub use bus::ChannelBus;
pub use message::{ChannelMessage, MessagePriority, OutboundMessage, SenderIdentity};
pub use permission_gate::PermissionGate;
pub use plugin::ChannelPlugin;
pub use router::MessageRouter;
pub use scanner::{InjectionScanner, ScanResult};
