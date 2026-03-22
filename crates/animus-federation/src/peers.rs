use animus_core::InstanceId;
use chrono::{DateTime, Utc};
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrustLevel {
    Unknown,
    Verified,
    Trusted,
    Blocked,
}

#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub instance_id: InstanceId,
    pub verifying_key: VerifyingKey,
    pub address: SocketAddr,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub info: PeerInfoPersist,
    pub trust: TrustLevel,
    pub last_seen: DateTime<Utc>,
    pub last_handshake: Option<DateTime<Utc>>,
    pub segments_received: u64,
    pub segments_sent: u64,
}

/// Serializable version of PeerInfo (VerifyingKey stored as bytes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfoPersist {
    pub instance_id: InstanceId,
    pub verifying_key_bytes: [u8; 32],
    pub address: SocketAddr,
}

impl From<&PeerInfo> for PeerInfoPersist {
    fn from(info: &PeerInfo) -> Self {
        Self {
            instance_id: info.instance_id,
            verifying_key_bytes: info.verifying_key.to_bytes(),
            address: info.address,
        }
    }
}

impl PeerInfoPersist {
    pub fn to_peer_info(&self) -> Option<PeerInfo> {
        let vk = VerifyingKey::from_bytes(&self.verifying_key_bytes).ok()?;
        Some(PeerInfo {
            instance_id: self.instance_id,
            verifying_key: vk,
            address: self.address,
        })
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PeerRegistry {
    peers: HashMap<InstanceId, Peer>,
}

impl PeerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_peer(&mut self, info: PeerInfo) {
        let persist = PeerInfoPersist::from(&info);
        // If the peer already exists (e.g. mDNS rediscovery after reconnect), preserve
        // trust level and accumulated statistics rather than resetting them to defaults.
        if let Some(existing) = self.peers.get_mut(&info.instance_id) {
            existing.info = persist;
            existing.last_seen = Utc::now();
            return;
        }
        let peer = Peer {
            info: persist,
            trust: TrustLevel::Unknown,
            last_seen: Utc::now(),
            last_handshake: None,
            segments_received: 0,
            segments_sent: 0,
        };
        self.peers.insert(info.instance_id, peer);
    }

    pub fn remove_peer(&mut self, id: &InstanceId) {
        self.peers.remove(id);
    }

    pub fn get_peer(&self, id: &InstanceId) -> Option<&Peer> {
        self.peers.get(id)
    }

    pub fn get_peer_mut(&mut self, id: &InstanceId) -> Option<&mut Peer> {
        self.peers.get_mut(id)
    }

    pub fn set_trust(&mut self, id: &InstanceId, trust: TrustLevel) {
        if let Some(peer) = self.peers.get_mut(id) {
            peer.trust = trust;
        }
    }

    pub fn trusted_peers(&self) -> Vec<&Peer> {
        self.peers.values()
            .filter(|p| p.trust == TrustLevel::Trusted)
            .collect()
    }

    pub fn verified_or_trusted_peers(&self) -> Vec<&Peer> {
        self.peers.values()
            .filter(|p| p.trust == TrustLevel::Verified || p.trust == TrustLevel::Trusted)
            .collect()
    }

    pub fn all_peers(&self) -> Vec<&Peer> {
        self.peers.values().collect()
    }

    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    pub fn save(&self, path: &Path) -> animus_core::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self)?;
        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, &json)?;
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }

    pub fn load(path: &Path) -> animus_core::Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let data = std::fs::read_to_string(path)?;
        let registry: Self = serde_json::from_str(&data)?;
        Ok(registry)
    }
}
