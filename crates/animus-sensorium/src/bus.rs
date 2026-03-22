use animus_core::sensorium::SensorEvent;
use tokio::sync::broadcast;

pub struct EventBus {
    tx: broadcast::Sender<SensorEvent>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    pub async fn publish(&self, event: SensorEvent) -> animus_core::Result<()> {
        self.tx.send(event).map_err(|e| {
            animus_core::AnimusError::Sensorium(format!("failed to publish event: {e}"))
        })?;
        Ok(())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SensorEvent> {
        self.tx.subscribe()
    }
}
