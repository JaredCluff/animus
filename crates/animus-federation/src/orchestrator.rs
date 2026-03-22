use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use animus_core::{AnimusIdentity, AnimusError, FederationConfig, Result};
use animus_vectorfs::VectorStore;
use parking_lot::RwLock;
use tokio::sync::mpsc;

use crate::auth::FederationAuth;
use crate::discovery::{DiscoveryEvent, DiscoveryService};
use crate::peers::PeerRegistry;
use crate::server::FederationServer;

/// Top-level coordinator for the federation subsystem.
///
/// Wires together authentication, peer registry, discovery, and the HTTP server.
/// All federation errors are treated as non-fatal: they are logged and the
/// orchestrator continues operating.
pub struct FederationOrchestrator<S: VectorStore + Send + Sync + 'static> {
    identity: AnimusIdentity,
    config: FederationConfig,
    store: Arc<S>,
    peers: Arc<RwLock<PeerRegistry>>,
    peers_path: PathBuf,
    server_addr: Option<SocketAddr>,
}

impl<S: VectorStore + Send + Sync + 'static> FederationOrchestrator<S> {
    /// Create a new federation orchestrator.
    ///
    /// - `identity`: the AILF identity used for cryptographic authentication
    /// - `config`: federation configuration (bind address, port, static peers, etc.)
    /// - `store`: the vector store for segment retrieval by the HTTP server
    /// - `data_dir`: directory where peer registry and other federation data are persisted
    pub fn new(
        identity: AnimusIdentity,
        config: FederationConfig,
        store: Arc<S>,
        data_dir: &Path,
    ) -> Self {
        let peers_path = data_dir.join("federation").join("peers.json");

        let registry = match PeerRegistry::load(&peers_path) {
            Ok(r) => {
                tracing::info!(
                    "Loaded peer registry from {} ({} peers)",
                    peers_path.display(),
                    r.peer_count()
                );
                r
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to load peer registry from {}: {e}; starting fresh",
                    peers_path.display()
                );
                PeerRegistry::new()
            }
        };

        let peers = Arc::new(RwLock::new(registry));

        Self {
            identity,
            config,
            store,
            peers,
            peers_path,
            server_addr: None,
        }
    }

    /// Start the federation subsystem.
    ///
    /// If federation is disabled in configuration, this is a no-op.
    /// Otherwise it:
    /// 1. Starts the federation HTTP server
    /// 2. Creates a discovery service and starts it
    /// 3. Registers this instance as an mDNS service (if not using static peers)
    /// 4. Spawns a background task to handle discovery events
    pub async fn start(&mut self) -> Result<()> {
        if !self.config.enabled {
            tracing::info!("Federation is disabled; skipping start");
            return Ok(());
        }

        // Parse bind address
        let bind_addr: SocketAddr = format!("{}:{}", self.config.bind_address, self.config.port)
            .parse()
            .map_err(|e| {
                AnimusError::Federation(format!(
                    "invalid bind address '{}:{}': {e}",
                    self.config.bind_address, self.config.port
                ))
            })?;

        // Start the HTTP server.
        // The server gets its own FederationAuth and a fresh PeerRegistry.
        // Peers discovered via handshakes are managed by the server internally;
        // peers discovered via mDNS are managed by the orchestrator's registry.
        let server_auth = FederationAuth::new(self.identity.clone());
        let server = FederationServer::new(
            server_auth,
            PeerRegistry::new(),
            self.store.clone(),
            self.config.max_requests_per_minute,
        );

        let actual_addr = server.start(bind_addr).await?;
        self.server_addr = Some(actual_addr);
        tracing::info!("Federation server started on {actual_addr}");

        // Set up discovery
        let (event_tx, event_rx) = mpsc::channel::<DiscoveryEvent>(64);
        let discovery = DiscoveryService::new(
            self.identity.instance_id,
            self.config.static_peers.clone(),
            event_tx,
        );

        // Start discovery (mDNS browsing or static peer mode)
        if let Err(e) = discovery.start().await {
            tracing::warn!("Failed to start discovery service: {e}");
        }

        // Register mDNS service if not using static peers
        if self.config.static_peers.is_empty() {
            let vk_hex = hex::encode(self.identity.verifying_key().to_bytes());
            if let Err(e) = discovery.register_service(actual_addr.port(), &vk_hex) {
                tracing::warn!("Failed to register mDNS service: {e}");
            }
        }

        // Spawn discovery event handler
        let peers = self.peers.clone();
        let peers_path = self.peers_path.clone();
        tokio::spawn(async move {
            Self::handle_discovery_events(event_rx, peers, peers_path).await;
        });

        Ok(())
    }

    /// Background task that processes discovery events and adds discovered peers
    /// to the registry.
    async fn handle_discovery_events(
        mut rx: mpsc::Receiver<DiscoveryEvent>,
        peers: Arc<RwLock<PeerRegistry>>,
        peers_path: PathBuf,
    ) {
        while let Some(event) = rx.recv().await {
            match event {
                DiscoveryEvent::PeerDiscovered(info) => {
                    tracing::info!(
                        peer = %info.instance_id,
                        addr = %info.address,
                        "Discovered new peer via mDNS"
                    );
                    {
                        let mut registry = peers.write();
                        registry.add_peer(*info);
                        if let Err(e) = registry.save(&peers_path) {
                            tracing::warn!("Failed to save peer registry: {e}");
                        }
                    }
                }
                DiscoveryEvent::PeerLost(id) => {
                    tracing::info!(peer = %id, "Peer lost (mDNS)");
                    // We don't remove peers on loss — they may come back.
                    // The peer registry retains them for future reconnection.
                }
            }
        }
        tracing::debug!("Discovery event channel closed");
    }

    /// Returns the number of known peers in the registry.
    pub fn peer_count(&self) -> usize {
        self.peers.read().peer_count()
    }

    /// Returns the number of trusted peers in the registry.
    pub fn trusted_peer_count(&self) -> usize {
        self.peers.read().trusted_peers().len()
    }

    /// Returns the bound server address, if the server has been started.
    pub fn server_addr(&self) -> Option<SocketAddr> {
        self.server_addr
    }

    /// Returns the shared peer registry for runtime commands.
    pub fn peers(&self) -> Arc<RwLock<PeerRegistry>> {
        self.peers.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use animus_core::{Segment, SegmentId, Tier};
    use animus_vectorfs::SegmentUpdate;

    /// Minimal in-memory VectorStore for testing.
    #[derive(Default)]
    struct MockStore;

    impl VectorStore for MockStore {
        fn store(&self, _segment: Segment) -> animus_core::Result<SegmentId> {
            Ok(SegmentId::new())
        }

        fn query(
            &self,
            _embedding: &[f32],
            _top_k: usize,
            _tier_filter: Option<Tier>,
        ) -> animus_core::Result<Vec<Segment>> {
            Ok(vec![])
        }

        fn get(&self, _id: SegmentId) -> animus_core::Result<Option<Segment>> {
            Ok(None)
        }

        fn get_raw(&self, _id: SegmentId) -> animus_core::Result<Option<Segment>> {
            Ok(None)
        }

        fn update_meta(&self, _id: SegmentId, _update: SegmentUpdate) -> animus_core::Result<()> {
            Ok(())
        }

        fn set_tier(&self, _id: SegmentId, _tier: Tier) -> animus_core::Result<()> {
            Ok(())
        }

        fn delete(&self, _id: SegmentId) -> animus_core::Result<()> {
            Ok(())
        }

        fn merge(&self, _source_ids: Vec<SegmentId>, _merged: Segment) -> animus_core::Result<SegmentId> {
            Ok(SegmentId::new())
        }

        fn count(&self, _tier_filter: Option<Tier>) -> usize {
            0
        }

        fn segment_ids(&self, _tier_filter: Option<Tier>) -> Vec<SegmentId> {
            vec![]
        }
    }

    #[test]
    fn test_new_creates_orchestrator() {
        let tmp = tempfile::TempDir::new().unwrap();
        let identity = AnimusIdentity::generate("test-model".to_string());
        let config = FederationConfig::default();
        let store = Arc::new(MockStore);

        let orch = FederationOrchestrator::new(identity, config, store, tmp.path());

        assert_eq!(orch.peer_count(), 0);
        assert_eq!(orch.trusted_peer_count(), 0);
        assert!(orch.server_addr().is_none());
    }

    #[test]
    fn test_disabled_federation_skips_start() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let identity = AnimusIdentity::generate("test-model".to_string());
        let config = FederationConfig {
            enabled: false,
            ..Default::default()
        };
        let store = Arc::new(MockStore);

        let mut orch = FederationOrchestrator::new(identity, config, store, tmp.path());
        rt.block_on(async {
            orch.start().await.unwrap();
        });

        // Server should not have started
        assert!(orch.server_addr().is_none());
    }

    #[test]
    fn test_peers_returns_shared_registry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let identity = AnimusIdentity::generate("test-model".to_string());
        let config = FederationConfig::default();
        let store = Arc::new(MockStore);

        let orch = FederationOrchestrator::new(identity, config, store, tmp.path());
        let peers = orch.peers();

        // Verify we can read/write through the returned Arc
        assert_eq!(peers.read().peer_count(), 0);
    }

    #[tokio::test]
    async fn test_start_with_enabled_federation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let identity = AnimusIdentity::generate("test-model".to_string());
        let config = FederationConfig {
            enabled: true,
            bind_address: "127.0.0.1".to_string(),
            port: 0, // OS-assigned port
            static_peers: vec!["127.0.0.1:9999".to_string()], // use static peers to skip mDNS
            ..Default::default()
        };
        let store = Arc::new(MockStore);

        let mut orch = FederationOrchestrator::new(identity, config, store, tmp.path());
        orch.start().await.unwrap();

        // Server should have started and bound to a port
        let addr = orch.server_addr().expect("server should have started");
        assert_ne!(addr.port(), 0);
        assert_eq!(orch.peer_count(), 0);
    }
}
