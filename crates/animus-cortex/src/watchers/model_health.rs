//! ModelHealthWatcher — background task that probes named engine endpoints.
//!
//! Periodically sends GET /v1/models to each registered engine's base URL.
//! Updates SmartRouter's engine health states so degraded providers are skipped
//! by `route_all_candidates()` before an inference call is even attempted.
//!
//! On state change (available→unavailable or vice-versa), fires a Signal so the
//! main loop can forward an adaptation notification to the user via Telegram.
//!
//! # CRITICAL-3: Parallel probes
//! All endpoints are probed concurrently via `futures::future::join_all`.
//! A single stalled or slow endpoint no longer delays the rest.
//!
//! # CRITICAL-4: Dynamic endpoint list
//! The caller holds `Arc<parking_lot::Mutex<Vec<(String, String)>>>` and can push
//! new entries at runtime as engines are hot-loaded — without restarting the watcher.
//! Each probe cycle snapshots the list so newly registered engines are picked up
//! on the next tick.

use crate::smart_router::SmartRouter;
use animus_core::identity::ThreadId;
use animus_core::threading::{Signal, SignalPriority};
use chrono::Utc;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Probe a single endpoint with a short timeout.
/// Returns `true` if the endpoint is reachable (HTTP 200, 401, or 403 — auth needed but live).
async fn probe_endpoint(http: &reqwest::Client, base_url: &str) -> bool {
    let url = format!("{base_url}/v1/models");
    match http.get(&url).send().await {
        Ok(resp) => {
            let s = resp.status().as_u16();
            // 200 = healthy, 401/403 = auth required but endpoint is live
            s == 200 || s == 401 || s == 403
        }
        Err(_) => false,
    }
}

/// Probe a batch and update health states.
///
/// When `emit_signals` is false the router health is updated but no Signals are fired.
/// This is used for the T=0 baseline probe.
///
/// When `emit_signals` is true, fires AT MOST ONE summary Signal per call regardless of
/// how many engines changed state. This prevents signal storms when many engines flip
/// simultaneously (e.g. a provider outage restoring 40+ endpoints at once).
/// Per-engine changes are still logged for diagnostics.
async fn probe_batch_inner(
    snapshot: &[(String, String)],
    router: &SmartRouter,
    signal_tx: &mpsc::Sender<Signal>,
    source_id: ThreadId,
    http: &reqwest::Client,
    emit_signals: bool,
) {
    if snapshot.is_empty() {
        return;
    }

    tracing::debug!("ModelHealthWatcher: probing {} engine(s)", snapshot.len());

    let probe_futures: Vec<_> = snapshot.iter()
        .map(|(key, base_url)| {
            let http = http.clone();
            let key = key.clone();
            let base_url = base_url.clone();
            async move {
                let available = probe_endpoint(&http, &base_url).await;
                (key, available)
            }
        })
        .collect();

    let results = futures::future::join_all(probe_futures).await;

    let mut went_down: Vec<String> = Vec::new();
    let mut came_up: Vec<String> = Vec::new();

    for (key, available) in results {
        let prev_weight = router.engine_health_weight(&key);
        router.set_engine_health(&key, if available { 1.0 } else { 0.0 });

        if prev_weight >= 1.0 && !available {
            tracing::warn!("ModelHealthWatcher: '{}' went offline", key);
            went_down.push(key);
        } else if prev_weight <= 0.0 && available {
            tracing::info!("ModelHealthWatcher: '{}' recovered", key);
            came_up.push(key);
        }
        // no-change cases: silent (debug builds only noise up logs)
    }

    if emit_signals && (!went_down.is_empty() || !came_up.is_empty()) {
        // One summary signal per cycle — never more than one regardless of engine count.
        let total_up = snapshot.iter().filter(|(k, _)| router.engine_health_weight(k) >= 1.0).count();
        let summary = match (went_down.len(), came_up.len()) {
            (d, 0) => format!("Adapting: {d} engine(s) offline — routing around them ({total_up} active)"),
            (0, u) => format!("Recovery: {u} engine(s) back online ({total_up} active)"),
            (d, u) => format!("Health update: {d} offline, {u} recovered ({total_up} active)"),
        };
        tracing::info!("ModelHealthWatcher: {summary}");
        let _ = signal_tx.try_send(Signal {
            source_thread: source_id,
            target_thread: ThreadId::default(),
            priority: SignalPriority::Normal,
            summary,
            segment_refs: vec![],
            created: Utc::now(),
        });
    }
}

/// Launch the model health watcher as a background task.
///
/// `endpoints` — shared, mutable list of `(registry_key, base_url)` pairs.
/// Only inference endpoints (chat models) should be in this list — the caller
/// filters out embedding, guard, reward, and TTS models before passing it here.
///
/// The caller holds the same `Arc` and extends it at runtime when engines are
/// hot-loaded (CRITICAL-4).
///
/// `interval_secs` — periodic full-scan interval. 120s is a reasonable default;
/// targeted probes handle faster recovery from inference failures.
///
/// `probe_trigger_rx` — channel for targeted on-demand probes after an inference
/// failure. Fires with exponential backoff to avoid hammering a known-down engine.
pub async fn run_model_health_watcher(
    endpoints: Arc<parking_lot::Mutex<Vec<(String, String)>>>,
    router: SmartRouter,
    signal_tx: mpsc::Sender<Signal>,
    source_id: ThreadId,
    interval_secs: u64,
    mut probe_trigger_rx: tokio::sync::mpsc::Receiver<Vec<String>>,
) {
    let http = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("ModelHealthWatcher: failed to build HTTP client: {e}");
            return;
        }
    };

    tracing::info!(
        "ModelHealthWatcher started — {} endpoint(s), {}s interval (T=0 baseline probe, no signals)",
        endpoints.lock().len(),
        interval_secs,
    );

    // T=0: establish baseline health state WITHOUT firing signals.
    // Firing individual signals for every engine at startup floods the proactive channel
    // (signal storm) since all 100+ endpoints complete simultaneously. The baseline just
    // sets health weights so the router has accurate state before the first request arrives.
    {
        let snapshot: Vec<(String, String)> = endpoints.lock().clone();
        probe_batch_inner(&snapshot, &router, &signal_tx, source_id, &http, false).await;
        let up = snapshot.iter().filter(|(k, _)| router.engine_health_weight(k) >= 1.0).count();
        let down = snapshot.iter().filter(|(k, _)| router.engine_health_weight(k) == 0.0).count();
        tracing::info!("ModelHealthWatcher: baseline established — {up} up, {down} down out of {}", snapshot.len());
    }

    // Track consecutive triggered-probe failures per engine for backoff.
    let mut consecutive_failures: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    let mut last_triggered_probe: std::collections::HashMap<String, std::time::Instant> =
        std::collections::HashMap::new();

    let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
    interval.tick().await; // consume the immediate first tick so loop starts at T+interval_secs

    loop {
        tokio::select! {
            _ = interval.tick() => {
                // Periodic full scan — emit signals for state transitions
                let snapshot: Vec<(String, String)> = endpoints.lock().clone();
                probe_batch_inner(&snapshot, &router, &signal_tx, source_id, &http, true).await;
                // Reset backoff counters for engines that recovered.
                for (key, _) in &snapshot {
                    if router.engine_health_weight(key) > 0.0 {
                        consecutive_failures.remove(key);
                        last_triggered_probe.remove(key);
                    }
                }
            }
            Some(keys) = probe_trigger_rx.recv() => {
                // Targeted probe after an inference failure — emit signals, apply backoff
                let now = std::time::Instant::now();
                let all_endpoints = endpoints.lock().clone();
                let targeted: Vec<(String, String)> = all_endpoints.into_iter()
                    .filter(|(key, _)| {
                        if !keys.contains(key) {
                            return false;
                        }
                        // Exponential backoff: 30s base, doubles per consecutive failure, cap 300s
                        let failures = consecutive_failures.get(key).copied().unwrap_or(0);
                        let backoff_secs = (30u64 * 2u64.pow(failures.min(4))).min(300);
                        let backoff = std::time::Duration::from_secs(backoff_secs);
                        match last_triggered_probe.get(key) {
                            Some(last) if now.duration_since(*last) < backoff => {
                                tracing::debug!(
                                    "ModelHealthWatcher: skipping triggered probe for '{}' — backoff {}s",
                                    key, backoff_secs
                                );
                                false
                            }
                            _ => true,
                        }
                    })
                    .collect();

                if !targeted.is_empty() {
                    probe_batch_inner(&targeted, &router, &signal_tx, source_id, &http, true).await;
                    for (key, _) in &targeted {
                        last_triggered_probe.insert(key.clone(), now);
                        if router.engine_health_weight(key) == 0.0 {
                            *consecutive_failures.entry(key.clone()).or_insert(0) += 1;
                        } else {
                            consecutive_failures.remove(key);
                        }
                    }
                }
            }
        }
    }
}
