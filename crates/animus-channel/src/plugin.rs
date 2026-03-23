//! ChannelPlugin trait — the interface every channel adapter must implement.

use crate::bus::ChannelBus;
use crate::message::OutboundMessage;
use animus_core::error::Result;
use std::sync::Arc;

/// A communication channel adapter — both a source of inbound messages
/// and a sink for outbound responses.
///
/// Implementations are statically compiled initially. Each adapter only
/// activates if its credentials are present in config.
#[async_trait::async_trait]
pub trait ChannelPlugin: Send + Sync {
    /// Unique identifier for this channel (e.g. "telegram", "email").
    fn id(&self) -> &str;

    /// Human-readable name for logging.
    fn name(&self) -> &str;

    /// Start listening for inbound messages and publishing them to the bus.
    /// This should run forever (until cancelled). Implementors should spawn
    /// an inner tokio task and return immediately.
    async fn start(&self, bus: Arc<ChannelBus>) -> Result<()>;

    /// Send an outbound message through this channel.
    async fn send(&self, msg: OutboundMessage) -> Result<()>;

    /// Whether this channel is properly configured (credentials present, etc.)
    fn is_configured(&self) -> bool;
}
