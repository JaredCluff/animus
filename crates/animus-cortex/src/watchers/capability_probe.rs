//! Capability probe watcher — continuous self-assessment of cognitive capability.
//!
//! Implements the `Watcher` trait so it plugs directly into `WatcherRegistry`.
//! Unlike other watchers, `CapabilityProbe` is **enabled by default** at startup —
//! self-awareness is always on.
//!
//! ## Layer compliance
//!
//! - **Layer 1:** Updates `CapabilityState` on every probe cycle (no LLM).
//! - **Layer 2:** Detects tier change by comparing new vs. stored tier.
//! - **Layer 3:** Returns `WatcherEvent` only on tier change — one Signal, not a stream.
//!   Degradation fires `Urgent`; improvement fires `Normal`.
//!
//! ## Probe technique
//!
//! Uses a TCP connect timeout against the model endpoint host:port.
//! **No LLM tokens are burned.** The probe is purely a transport-layer reachability check.

use crate::watcher::{Watcher, WatcherConfig, WatcherEvent};
use animus_core::capability::{CapabilityState, CognitiveTier};
use animus_core::threading::SignalPriority;
use animus_core::EmbeddingService;
use animus_vectorfs::VectorStore;
use std::net::ToSocketAddrs;
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEFAULT_INTERVAL_SECS: u64 = 30;
const PROBE_TIMEOUT_SECS: u64 = 5;
/// Latency above this threshold means the model is reachable but slow → Reduced tier.
const SLOW_LATENCY_THRESHOLD_MS: u64 = 30_000;
const HIGH_MEMORY_PRESSURE: f32 = 0.9;
/// Assumed maximum segment count for memory pressure calculation.
const MEMORY_CAPACITY_ESTIMATE: usize = 50_000;

pub struct CapabilityProbe {
    /// Shared state — also read by `get_capability_state` introspective tool.
    capability_state: Arc<parking_lot::RwLock<CapabilityState>>,
    /// `host:port` extracted from the probe URL for TCP connect check.
    probe_addr: String,
    /// Human-readable model identifier stored in state after each probe.
    probe_model: String,
    /// Provider name ("anthropic", "ollama", "openai", etc.) — used for tier derivation.
    probe_provider: String,
    /// VectorFS store — checked for health and memory pressure.
    store: Arc<dyn VectorStore>,
    /// Embedding service — held for future probing; not currently used in check().
    #[allow(dead_code)]
    embedder: Arc<dyn EmbeddingService>,
}

impl CapabilityProbe {
    pub fn new(
        capability_state: Arc<parking_lot::RwLock<CapabilityState>>,
        probe_url: &str,
        probe_model: String,
        probe_provider: String,
        store: Arc<dyn VectorStore>,
        embedder: Arc<dyn EmbeddingService>,
    ) -> Self {
        let probe_addr = extract_tcp_addr(probe_url);
        Self {
            capability_state,
            probe_addr,
            probe_model,
            probe_provider,
            store,
            embedder,
        }
    }

    /// Synchronous TCP connect probe — no LLM call, no token burn.
    /// Returns `(reachable, Some(latency_ms))` or `(false, None)`.
    fn probe_model_reachability(&self) -> (bool, Option<u64>) {
        let addrs: Vec<_> = match self.probe_addr.to_socket_addrs() {
            Ok(it) => it.collect(),
            Err(_) => return (false, None),
        };

        let start = Instant::now();
        for addr in addrs {
            match std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(PROBE_TIMEOUT_SECS)) {
                Ok(_) => return (true, Some(start.elapsed().as_millis() as u64)),
                Err(_) => continue,
            }
        }
        (false, None)
    }
}

/// Extract `host:port` from a URL string for use with `TcpStream::connect_timeout`.
fn extract_tcp_addr(url: &str) -> String {
    let without_scheme = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    // Strip path component
    let host_port = without_scheme.split('/').next().unwrap_or(without_scheme);
    if host_port.contains(':') {
        host_port.to_string()
    } else if url.starts_with("https://") {
        format!("{}:443", host_port)
    } else {
        format!("{}:80", host_port)
    }
}

/// Derive the cognitive tier from probe results.
/// Returns `(tier, reason_str)`.
fn derive_tier(
    reasoning_available: bool,
    latency_ms: Option<u64>,
    vectorfs_healthy: bool,
    memory_pressure: f32,
    provider: &str,
) -> (CognitiveTier, &'static str) {
    if reasoning_available {
        let slow = latency_ms.map_or(true, |l| l >= SLOW_LATENCY_THRESHOLD_MS);
        if slow {
            return (CognitiveTier::Reduced, "reasoning reachable but slow");
        }
        if vectorfs_healthy && memory_pressure < HIGH_MEMORY_PRESSURE {
            // Cloud provider = Full; local (Ollama/any non-cloud) = Strong
            let is_cloud = matches!(provider, "anthropic" | "openai" | "openai-compat" | "openai_compat");
            if is_cloud {
                (CognitiveTier::Full, "cloud reasoning + healthy memory")
            } else {
                (CognitiveTier::Strong, "local reasoning + healthy memory")
            }
        } else {
            (CognitiveTier::Reduced, "reasoning available but memory strained")
        }
    } else if vectorfs_healthy {
        (CognitiveTier::MemoryOnly, "no reasoning; memory intact")
    } else {
        (CognitiveTier::DeadReckoning, "no reasoning; memory degraded")
    }
}

impl Watcher for CapabilityProbe {
    fn id(&self) -> &str { "capability_probe" }
    fn name(&self) -> &str { "Capability Probe" }
    fn default_interval(&self) -> Duration { Duration::from_secs(DEFAULT_INTERVAL_SECS) }

    fn check(&self, _config: &WatcherConfig) -> Option<WatcherEvent> {
        // Layer 1: no LLM — TCP probe + store metrics only
        let (reasoning_available, latency_ms) = self.probe_model_reachability();

        let segment_count = self.store.count(None);
        let memory_pressure = (segment_count as f32 / MEMORY_CAPACITY_ESTIMATE as f32).min(1.0);
        let vectorfs_healthy = true; // if store.count() returned without panic, VectorFS is up

        let (new_tier, reason) = derive_tier(
            reasoning_available,
            latency_ms,
            vectorfs_healthy,
            memory_pressure,
            &self.probe_provider,
        );

        // Always update shared state so introspective tools see fresh data (Layer 1)
        let old_tier = {
            let mut state = self.capability_state.write();
            let old = state.tier;
            state.tier = new_tier;
            state.reasoning_available = reasoning_available;
            // Embedding service is co-located with the model endpoint; proxy availability
            state.embedding_available = reasoning_available;
            state.vectorfs_healthy = vectorfs_healthy;
            state.memory_pressure = memory_pressure;
            state.active_model = Some(self.probe_model.clone());
            state.latency_ms = latency_ms;
            state.last_probed = chrono::Utc::now();
            old
        };

        // Layer 2 → Layer 3: fire one Signal on tier change only
        if new_tier != old_tier {
            let priority = if (new_tier as u8) > (old_tier as u8) {
                SignalPriority::Urgent  // degradation — higher number = worse
            } else {
                SignalPriority::Normal  // improvement
            };
            tracing::info!(
                "Cognitive tier change: {} → {} ({})",
                old_tier.label(), new_tier.label(), reason
            );
            Some(WatcherEvent {
                priority,
                summary: format!(
                    "Cognitive tier: {} → {} ({})",
                    old_tier.label(), new_tier.label(), reason
                ),
                segment_refs: vec![],
            })
        } else {
            tracing::debug!(
                "Capability probe: tier {} unchanged (latency={:?}ms, memory_pressure={:.1}%)",
                new_tier.label(),
                latency_ms,
                memory_pressure * 100.0,
            );
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use animus_core::capability::CognitiveTier;
    use animus_embed::SyntheticEmbedding;
    use animus_vectorfs::store::MmapVectorStore;
    use std::sync::Arc;

    fn make_probe(dir: &std::path::Path) -> (CapabilityProbe, Arc<parking_lot::RwLock<CapabilityState>>) {
        let state = Arc::new(parking_lot::RwLock::new(CapabilityState::default()));
        let store = Arc::new(MmapVectorStore::open(dir, 4).unwrap());
        let embedder = Arc::new(SyntheticEmbedding::new(4));

        // Port 1 is reserved and effectively guaranteed to refuse connections everywhere.
        let probe = CapabilityProbe::new(
            state.clone(),
            "http://127.0.0.1:1",
            "test-model".to_string(),
            "ollama".to_string(),
            store as Arc<dyn VectorStore>,
            embedder as Arc<dyn EmbeddingService>,
        );
        (probe, state)
    }

    fn empty_config() -> WatcherConfig {
        WatcherConfig::default()
    }

    #[test]
    fn check_updates_state_active_model_set() {
        let tmp = tempfile::tempdir().unwrap();
        let (probe, state) = make_probe(tmp.path());

        // Probe runs against port 1 (unreachable) — state should still be updated with model name
        let _event = probe.check(&empty_config());

        let s = state.read();
        assert_eq!(s.active_model.as_deref(), Some("test-model"));
        // Port 1 is guaranteed unreachable — no reasoning
        assert!(!s.reasoning_available);
    }

    #[test]
    fn tier_change_fires_event() {
        // Set state to Full, then probe a guaranteed-unreachable endpoint → degradation
        let state = Arc::new(parking_lot::RwLock::new(CapabilityState {
            tier: CognitiveTier::Full,
            reasoning_available: true,
            embedding_available: true,
            vectorfs_healthy: true,
            memory_pressure: 0.0,
            active_model: Some("anthropic:claude".to_string()),
            latency_ms: Some(200),
            last_probed: chrono::Utc::now(),
        }));

        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(MmapVectorStore::open(tmp.path(), 4).unwrap());
        let embedder = Arc::new(SyntheticEmbedding::new(4));

        // Port 1 is guaranteed to refuse — reasoning_available will be false
        let probe = CapabilityProbe::new(
            state.clone(),
            "http://127.0.0.1:1",
            "test-model".to_string(),
            "anthropic".to_string(),
            store as Arc<dyn VectorStore>,
            embedder as Arc<dyn EmbeddingService>,
        );

        let event = probe.check(&empty_config());

        // Full → MemoryOnly = degradation → Urgent signal
        let ev = event.expect("tier changed from Full to MemoryOnly — event must fire");
        assert_eq!(ev.priority, SignalPriority::Urgent);
        assert!(ev.summary.contains("Full"));
        assert_eq!(state.read().tier, CognitiveTier::MemoryOnly);
    }

    #[test]
    fn no_tier_change_returns_none_when_state_matches() {
        // State already MemoryOnly; guaranteed-unreachable endpoint → MemoryOnly again → no event
        let state = Arc::new(parking_lot::RwLock::new(CapabilityState::default()));
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(MmapVectorStore::open(tmp.path(), 4).unwrap());
        let embedder = Arc::new(SyntheticEmbedding::new(4));

        let probe = CapabilityProbe::new(
            state,
            "http://127.0.0.1:1",
            "test-model".to_string(),
            "ollama".to_string(),
            store as Arc<dyn VectorStore>,
            embedder as Arc<dyn EmbeddingService>,
        );

        let event = probe.check(&empty_config());
        assert!(event.is_none());
    }

    #[test]
    fn derive_tier_cloud_fast_healthy_is_full() {
        let (tier, _) = derive_tier(true, Some(500), true, 0.1, "anthropic");
        assert_eq!(tier, CognitiveTier::Full);
    }

    #[test]
    fn derive_tier_local_fast_healthy_is_strong() {
        let (tier, _) = derive_tier(true, Some(800), true, 0.1, "ollama");
        assert_eq!(tier, CognitiveTier::Strong);
    }

    #[test]
    fn derive_tier_slow_is_reduced() {
        let (tier, _) = derive_tier(true, Some(31_000), true, 0.1, "anthropic");
        assert_eq!(tier, CognitiveTier::Reduced);
    }

    #[test]
    fn derive_tier_memory_pressure_is_reduced() {
        let (tier, _) = derive_tier(true, Some(500), true, 0.95, "anthropic");
        assert_eq!(tier, CognitiveTier::Reduced);
    }

    #[test]
    fn derive_tier_no_reasoning_healthy_store_is_memory_only() {
        let (tier, _) = derive_tier(false, None, true, 0.0, "ollama");
        assert_eq!(tier, CognitiveTier::MemoryOnly);
    }

    #[test]
    fn derive_tier_no_reasoning_unhealthy_store_is_dead_reckoning() {
        let (tier, _) = derive_tier(false, None, false, 0.0, "ollama");
        assert_eq!(tier, CognitiveTier::DeadReckoning);
    }

    #[test]
    fn extract_tcp_addr_ollama() {
        assert_eq!(extract_tcp_addr("http://localhost:11434"), "localhost:11434");
    }

    #[test]
    fn extract_tcp_addr_anthropic() {
        assert_eq!(extract_tcp_addr("https://api.anthropic.com/v1"), "api.anthropic.com:443");
    }

    #[test]
    fn extract_tcp_addr_no_port_http() {
        assert_eq!(extract_tcp_addr("http://example.com/path"), "example.com:80");
    }
}
