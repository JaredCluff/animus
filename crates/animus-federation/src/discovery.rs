use animus_core::{AnimusError, InstanceId, Result};
use crate::peers::PeerInfo;
use ed25519_dalek::VerifyingKey;
use std::net::SocketAddr;
use tokio::sync::mpsc;

/// Events emitted by the discovery service when peers appear or disappear.
#[derive(Debug)]
pub enum DiscoveryEvent {
    /// A new peer was discovered on the local network.
    PeerDiscovered(Box<PeerInfo>),
    /// A previously discovered peer is no longer available.
    PeerLost(InstanceId),
}

/// Service for discovering other Animus instances on the local network.
///
/// Uses mDNS (DNS-SD) to announce and browse for `_animus._tcp.local.` services.
/// Falls back to a static peer list when `static_peers` is non-empty.
pub struct DiscoveryService {
    own_instance_id: InstanceId,
    static_peers: Vec<String>,
    event_tx: mpsc::Sender<DiscoveryEvent>,
    /// Held to keep the mDNS registration alive; dropped on shutdown.
    _registration_daemon: Option<mdns_sd::ServiceDaemon>,
}

const SERVICE_TYPE: &str = "_animus._tcp.local.";

impl DiscoveryService {
    pub fn new(
        own_instance_id: InstanceId,
        static_peers: Vec<String>,
        event_tx: mpsc::Sender<DiscoveryEvent>,
    ) -> Self {
        Self {
            own_instance_id,
            static_peers,
            event_tx,
            _registration_daemon: None,
        }
    }

    /// Start discovery. If static_peers is non-empty, skip mDNS and use static list.
    /// Otherwise, use mDNS (DNS-SD) to discover peers on the local network.
    pub async fn start(&self) -> Result<()> {
        if !self.static_peers.is_empty() {
            tracing::info!(
                "Federation discovery: using {} static peers",
                self.static_peers.len()
            );
            // Static peers are added by the orchestrator after handshake
            return Ok(());
        }

        // mDNS discovery
        tracing::info!("Federation discovery: starting mDNS browsing for {SERVICE_TYPE}");
        let daemon = mdns_sd::ServiceDaemon::new().map_err(|e| {
            AnimusError::Federation(format!("failed to create mDNS daemon: {e}"))
        })?;

        let receiver = daemon.browse(SERVICE_TYPE).map_err(|e| {
            AnimusError::Federation(format!("failed to browse mDNS: {e}"))
        })?;

        let own_id = self.own_instance_id;
        let tx = self.event_tx.clone();

        tokio::spawn(async move {
            // Keep the daemon alive for the lifetime of the browse task.
            let _daemon = daemon;
            while let Ok(event) = receiver.recv_async().await {
                match event {
                    mdns_sd::ServiceEvent::ServiceResolved(info) => {
                        if let Some(peer) = Self::parse_service_info(&info, own_id) {
                            let _ = tx.send(DiscoveryEvent::PeerDiscovered(Box::new(peer))).await;
                        }
                    }
                    mdns_sd::ServiceEvent::ServiceRemoved(_service_type, fullname) => {
                        // Extract instance_id from the fullname if possible.
                        // Fullname format: "animus-<8chars>._animus._tcp.local."
                        tracing::info!("mDNS: service removed: {fullname}");
                    }
                    _ => {} // SearchStarted, ServiceFound, SearchStopped — ignore
                }
            }
        });

        Ok(())
    }

    /// Register this AILF as an mDNS service so other instances can discover it.
    ///
    /// Publishes TXT records containing:
    /// - `instance_id`: the full UUID of this instance
    /// - `vk`: the Ed25519 verifying key in hex
    /// - `proto`: protocol version (currently "1")
    pub fn register_service(&mut self, port: u16, vk_hex: &str) -> Result<()> {
        let daemon = mdns_sd::ServiceDaemon::new().map_err(|e| {
            AnimusError::Federation(format!("failed to create mDNS daemon: {e}"))
        })?;

        let instance_name = format!("animus-{}", &self.own_instance_id.0.to_string()[..8]);

        // Properties must be passed at construction time (mdns-sd 0.11 has no public set_property).
        let properties: &[(&str, &str)] = &[
            ("instance_id", &self.own_instance_id.0.to_string()),
            ("vk", vk_hex),
            ("proto", "1"),
        ];

        let service = mdns_sd::ServiceInfo::new(
            SERVICE_TYPE,
            &instance_name,
            &format!("{instance_name}.local."),
            "", // empty IP — use enable_addr_auto() to let the daemon fill in host addresses
            port,
            properties,
        )
        .map_err(|e| {
            AnimusError::Federation(format!("failed to create mDNS service info: {e}"))
        })?
        .enable_addr_auto();

        daemon.register(service).map_err(|e| {
            AnimusError::Federation(format!("failed to register mDNS service: {e}"))
        })?;

        // Store the daemon handle so it stays alive and keeps the service registered.
        // When DiscoveryService is dropped, the daemon is cleanly shut down.
        self._registration_daemon = Some(daemon);

        tracing::info!("mDNS: registered {SERVICE_TYPE} on port {port}");
        Ok(())
    }

    /// Parse an mDNS ServiceInfo into a PeerInfo, skipping our own instance.
    fn parse_service_info(
        info: &mdns_sd::ServiceInfo,
        own_id: InstanceId,
    ) -> Option<PeerInfo> {
        let instance_id_str = info.get_property_val_str("instance_id")?;
        let instance_id = InstanceId(instance_id_str.parse().ok()?);

        // Skip self
        if instance_id == own_id {
            return None;
        }

        let vk_hex = info.get_property_val_str("vk")?;
        let vk_bytes: [u8; 32] = hex::decode(vk_hex).ok()?.try_into().ok()?;
        let verifying_key = VerifyingKey::from_bytes(&vk_bytes).ok()?;

        let port = info.get_port();
        let addresses = info.get_addresses();
        let addr = addresses.iter().next()?;
        let address = SocketAddr::new(*addr, port);

        Some(PeerInfo {
            instance_id,
            verifying_key,
            address,
        })
    }

    /// Returns the static peer addresses configured for this service.
    pub fn static_peer_addresses(&self) -> &[String] {
        &self.static_peers
    }
}
