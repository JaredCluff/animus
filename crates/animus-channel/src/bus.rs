//! ChannelBus — unified inbound message bus across all channel adapters.

use crate::message::{ChannelMessage, OutboundMessage};
use crate::plugin::ChannelPlugin;
use animus_core::error::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};

/// The central bus through which all inbound messages flow.
///
/// Channel adapters publish ChannelMessages here. The MessageRouter
/// subscribes and dispatches to reasoning threads. Outbound responses
/// are routed back through the originating channel adapter.
pub struct ChannelBus {
    /// Broadcast sender for inbound messages.
    tx: broadcast::Sender<ChannelMessage>,
    /// Registered channel adapters, keyed by channel_id.
    adapters: Mutex<HashMap<String, Arc<dyn ChannelPlugin>>>,
}

impl ChannelBus {
    /// Create a new bus with the given inbound queue capacity.
    pub fn new(capacity: usize) -> Arc<Self> {
        let (tx, _) = broadcast::channel(capacity);
        Arc::new(Self {
            tx,
            adapters: Mutex::new(HashMap::new()),
        })
    }

    /// Register a channel adapter. Only configured adapters should be registered.
    pub async fn register(&self, adapter: Arc<dyn ChannelPlugin>) {
        let id = adapter.id().to_string();
        tracing::info!("ChannelBus: registered adapter '{}'", id);
        self.adapters.lock().await.insert(id, adapter);
    }

    /// Publish an inbound message to all subscribers.
    pub fn publish(&self, msg: ChannelMessage) {
        tracing::debug!("ChannelBus: {}", msg.summary());
        // Ignore send errors — no subscribers yet at startup is normal
        let _ = self.tx.send(msg);
    }

    /// Subscribe to inbound messages.
    pub fn subscribe(&self) -> broadcast::Receiver<ChannelMessage> {
        self.tx.subscribe()
    }

    /// Send an outbound message through the appropriate channel adapter.
    pub async fn send(&self, msg: OutboundMessage) -> Result<()> {
        let adapters = self.adapters.lock().await;
        match adapters.get(&msg.channel_id) {
            Some(adapter) => adapter.send(msg).await,
            None => {
                tracing::warn!("ChannelBus: no adapter for channel '{}'", msg.channel_id);
                Ok(())
            }
        }
    }

    /// Start all registered adapters. Each adapter spawns its own polling loop.
    pub async fn start_all(self: &Arc<Self>) -> Result<()> {
        let adapters: Vec<Arc<dyn ChannelPlugin>> = {
            self.adapters.lock().await.values().cloned().collect()
        };
        for adapter in adapters {
            tracing::info!("ChannelBus: starting adapter '{}'", adapter.id());
            adapter.start(self.clone()).await?;
        }
        Ok(())
    }
}
