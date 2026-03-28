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

/// Launch the model health watcher as a background task.
///
/// `endpoints` — shared, mutable list of `(registry_key, base_url)` pairs where
/// `registry_key` is the `"provider:model"` string used in `EngineRegistry::by_name`
/// and the SmartRouter's `engine_health` map.
///
/// The caller holds the same `Arc` and extends it at runtime when engines are
/// hot-loaded, so newly registered engines are probed on the next cycle without
/// restarting the watcher (CRITICAL-4).
///
/// `interval_secs` — how often to probe all endpoints.
pub async fn run_model_health_watcher(
    endpoints: Arc<parking_lot::Mutex<Vec<(String, String)>>>,
    router: SmartRouter,
    signal_tx: mpsc::Sender<Signal>,
    source_id: ThreadId,
    interval_secs: u64,
) {
    if endpoints.lock().is_empty() {
        return;
    }

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
        "ModelHealthWatcher started — probing engine(s) every {}s",
        interval_secs,
    );

    let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
    interval.tick().await; // skip immediate first tick — let startup settle

    loop {
        interval.tick().await;

        // CRITICAL-4: snapshot under lock so hot-added engines are picked up each cycle.
        let snapshot: Vec<(String, String)> = endpoints.lock().clone();

        if snapshot.is_empty() {
            continue;
        }

        tracing::debug!("ModelHealthWatcher: probing {} engine(s)", snapshot.len());

        // CRITICAL-3: probe all endpoints concurrently — one stalled endpoint no longer
        // blocks the rest of the fleet from being evaluated.
        // reqwest::Client is cheap to clone (shares the underlying connection pool).
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

        for (key, available) in results {
            let was_available = router.engine_health_weight(&key) > 0.0;
            router.set_engine_health(&key, if available { 1.0 } else { 0.0 });

            if was_available && !available {
                let summary = format!(
                    "Adapting: engine '{key}' probe failed — routing around it until it recovers"
                );
                tracing::warn!("{summary}");
                let _ = signal_tx.try_send(Signal {
                    source_thread: source_id,
                    target_thread: ThreadId::default(),
                    priority: SignalPriority::Normal,
                    summary,
                    segment_refs: vec![],
                    created: Utc::now(),
                });
            } else if !was_available && available {
                let summary = format!("Engine '{key}' is back online — resuming normal routing");
                tracing::info!("{summary}");
                let _ = signal_tx.try_send(Signal {
                    source_thread: source_id,
                    target_thread: ThreadId::default(),
                    priority: SignalPriority::Normal,
                    summary,
                    segment_refs: vec![],
                    created: Utc::now(),
                });
            } else {
                tracing::debug!(
                    "ModelHealthWatcher: '{}' = {}",
                    key,
                    if available { "up" } else { "down" }
                );
            }
        }
    }
}
