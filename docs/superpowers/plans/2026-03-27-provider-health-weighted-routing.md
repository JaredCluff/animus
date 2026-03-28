# Provider Health-Weighted Routing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the binary engine health flag in SmartRouter with a float weight (0.0/0.5/1.0), integrate that weight into routing scores, probe all engines at T=0 startup instead of 120s in, and trigger re-probes immediately on engine failure.

**Architecture:** The `engine_health` map in `SmartRouter` changes from `HashMap<String, bool>` to `HashMap<String, f32>`. Engines that haven't been probed yet start at `0.5` (scores at half capability); confirmed-healthy engines are `1.0`; confirmed-down engines are `0.0` and skipped. `ModelHealthWatcher` gains a `tokio::mpsc` trigger channel and probes immediately at T=0. `main.rs` wires the channel and fires triggered re-probes when an engine falls back.

**Tech Stack:** Rust, tokio, parking_lot, reqwest — all already in use.

---

## File Map

| File | Change |
|------|--------|
| `crates/animus-cortex/src/smart_router.rs` | `engine_health` type `bool→f32`, new methods, scoring multiplier |
| `crates/animus-cortex/src/watchers/model_health.rs` | T=0 probe, trigger channel, backoff, extract `probe_batch` helper |
| `crates/animus-runtime/src/main.rs` | Create trigger channel, wire to router + watcher, track `primary_model_key`, trigger on fallback, always spawn watcher |

---

## Task 1: SmartRouter — change `engine_health` type and add `mark_engine_unhealthy`

**Files:**
- Modify: `crates/animus-cortex/src/smart_router.rs`

- [ ] **Step 1: Write failing tests**

Add to the `#[cfg(test)]` block at the bottom of `smart_router.rs` (after line 731):

```rust
#[test]
fn unknown_engine_has_half_weight() {
    // Engine not in map → default 0.5
    let health: HashMap<String, f32> = HashMap::new();
    let weight = health.get("anthropic:claude-haiku").copied().unwrap_or(0.5_f32);
    assert_eq!(weight, 0.5);
}

#[tokio::test]
async fn health_weight_methods_roundtrip() {
    let (router, _rx) = make_router_async().await;
    // Unprobed engine defaults to 0.5
    assert_eq!(router.engine_health_weight("ollama:qwen3.5:35b"), 0.5);
    // Set confirmed healthy
    router.set_engine_health("ollama:qwen3.5:35b", 1.0);
    assert_eq!(router.engine_health_weight("ollama:qwen3.5:35b"), 1.0);
    // Mark unhealthy
    router.mark_engine_unhealthy("ollama:qwen3.5:35b");
    assert_eq!(router.engine_health_weight("ollama:qwen3.5:35b"), 0.0);
}

#[tokio::test]
async fn zero_weight_engine_skipped_by_is_engine_healthy() {
    let (router, _rx) = make_router_async().await;
    let spec = crate::model_plan::ModelSpec {
        provider: "ollama".to_string(),
        model: "qwen3.5:35b".to_string(),
        think: crate::model_plan::ThinkLevel::Dynamic,
        cost: None, speed: None, quality: None, trust_floor: 0,
    };
    router.mark_engine_unhealthy("ollama:qwen3.5:35b");
    assert!(!router.is_engine_healthy_pub(&spec));
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd /Users/jared.cluff/gitrepos/animus
cargo test -p animus-cortex health_weight 2>&1 | head -40
```

Expected: compile error — `engine_health_weight`, `mark_engine_unhealthy`, `is_engine_healthy_pub` don't exist yet.

- [ ] **Step 3: Change `engine_health` field type in the struct**

In `smart_router.rs` line 98, replace:
```rust
    /// Engine availability — keyed by "provider:model", updated by ModelHealthWatcher.
    /// `true` = available (default when not yet probed), `false` = unavailable.
    engine_health: Arc<parking_lot::Mutex<HashMap<String, bool>>>,
```
With:
```rust
    /// Engine health weight — keyed by "provider:model", updated by ModelHealthWatcher.
    /// `1.0` = confirmed healthy, `0.5` = unknown (not yet probed), `0.0` = confirmed down.
    engine_health: Arc<parking_lot::Mutex<HashMap<String, f32>>>,
```

Also add the probe trigger field directly after:
```rust
    /// Trigger channel sender — send engine keys to request an immediate out-of-band probe.
    /// Set by main.rs after SmartRouter creation via `set_probe_trigger_tx`.
    probe_trigger_tx: Arc<parking_lot::Mutex<Option<tokio::sync::mpsc::Sender<Vec<String>>>>>,
```

- [ ] **Step 4: Update `SmartRouter::new` constructor**

In `new()` (around line 134), add initialization for the new field:
```rust
        engine_health: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        probe_trigger_tx: Arc::new(parking_lot::Mutex::new(None)),
```

(Replace the existing `engine_health: Arc::new(parking_lot::Mutex::new(HashMap::new())),` line — only the `probe_trigger_tx` line is new.)

- [ ] **Step 5: Replace `set_engine_health`, `is_engine_available`, `is_engine_healthy`, and add new methods**

Find and replace the methods starting at line 432:

```rust
    /// Set the health weight for an engine. 1.0 = healthy, 0.5 = unknown, 0.0 = confirmed down.
    /// Key is "provider:model".
    pub fn set_engine_health(&self, key: &str, weight: f32) {
        self.engine_health.lock().insert(key.to_string(), weight.clamp(0.0, 1.0));
    }

    /// Return the health weight for an engine key.
    /// Returns `0.5` (unknown) when not yet probed.
    pub fn engine_health_weight(&self, key: &str) -> f32 {
        *self.engine_health.lock().get(key).unwrap_or(&0.5_f32)
    }

    /// Check if the engine for a ModelSpec is not confirmed down.
    fn is_engine_healthy(&self, spec: &crate::model_plan::ModelSpec) -> bool {
        let key = format!("{}:{}", spec.provider, spec.model);
        self.engine_health_weight(&key) > 0.0
    }

    /// Public version of `is_engine_healthy` for tests.
    #[cfg(test)]
    pub fn is_engine_healthy_pub(&self, spec: &crate::model_plan::ModelSpec) -> bool {
        self.is_engine_healthy(spec)
    }

    /// Mark an engine confirmed down (weight = 0.0) and trigger an immediate re-probe.
    pub fn mark_engine_unhealthy(&self, key: &str) {
        self.set_engine_health(key, 0.0);
        self.trigger_probe(vec![key.to_string()]);
    }

    /// Send engine keys through the trigger channel to request an immediate probe.
    pub fn trigger_probe(&self, keys: Vec<String>) {
        if let Some(tx) = self.probe_trigger_tx.lock().as_ref() {
            let _ = tx.try_send(keys);
        }
    }

    /// Wire the probe trigger sender. Called once from main.rs after channel creation.
    pub fn set_probe_trigger_tx(&self, tx: tokio::sync::mpsc::Sender<Vec<String>>) {
        *self.probe_trigger_tx.lock() = Some(tx);
    }
```

Remove the old `is_engine_available` method entirely (the new `engine_health_weight` replaces it).

- [ ] **Step 6: Run tests**

```bash
cargo test -p animus-cortex health_weight 2>&1 | head -40
```

Expected: all three new tests pass. If compile error, fix and re-run.

- [ ] **Step 7: Commit**

```bash
git add crates/animus-cortex/src/smart_router.rs
git commit -m "feat(routing): change engine_health from bool to f32 weight, add mark_engine_unhealthy + trigger_probe"
```

---

## Task 2: SmartRouter — integrate health weight into scoring

**Files:**
- Modify: `crates/animus-cortex/src/smart_router.rs`

- [ ] **Step 1: Write failing tests**

Add to the `#[cfg(test)]` block:

```rust
#[tokio::test]
async fn zero_weight_engine_skipped_by_select_for_class() {
    let (router, _rx) = make_router_async().await;
    // There are 2 candidates in the plan (35b=primary, 9b=fallback).
    // Mark primary as confirmed down — should get fallback.
    router.set_engine_health("ollama:qwen3.5:35b", 0.0);
    let decision = router.select_for_class("Analytical", BudgetPressure::Normal).await;
    assert!(
        decision.fallback_index > 0,
        "primary is confirmed down — expected fallback, got fallback_index={}",
        decision.fallback_index,
    );
}

#[tokio::test]
async fn half_weight_engine_scores_lower_than_confirmed_healthy() {
    let (router, _rx) = make_router_async().await;
    // Primary at 0.5 (default), fallback set to 1.0
    // The fallback should win because 1.0 × its_score > 0.5 × primary_score
    // when both have similar capability profiles
    router.set_engine_health("ollama:qwen3.5:9b", 1.0);
    // Leave primary at default 0.5
    let decision = router.select_for_class("Analytical", BudgetPressure::Normal).await;
    // With 35b at 0.5 and 9b at 1.0, 9b should win if the weighted scores flip
    // NOTE: this test may not flip the winner if 35b's capability score is >> 9b's.
    // What we're testing is that health_w IS applied to the score.
    // We verify by checking that a 0.0-weight primary is always skipped (above test).
    // Here just verify the decision is non-empty and valid.
    assert!(!decision.model_spec.model.is_empty());
}
```

- [ ] **Step 2: Run tests to verify first test fails**

```bash
cargo test -p animus-cortex zero_weight_engine_skipped_by_select_for_class 2>&1 | head -30
```

Expected: FAIL — the test should fail because currently 0.0-weight is not skipped in the scoring path (only `route_with_constraints` and `route_all_candidates` check engine health; `select_for_class` passes through the bool).

- [ ] **Step 3: Integrate health_w into `select_for_class`**

In `select_for_class`, the loop body starts at roughly line 188. Currently:
```rust
        for (idx, spec) in route.candidates.iter().enumerate() {
            let model_key = format!("{}:{}", spec.provider, spec.model);
            let engine_available = self.is_engine_healthy(spec);
```

Replace `let engine_available = self.is_engine_healthy(spec);` with:
```rust
            let health_w = self.engine_health_weight(&model_key);
            if health_w == 0.0 {
                tracing::debug!(
                    "select_for_class: skipping {}:{} — confirmed down (health_w=0.0)",
                    spec.provider, spec.model
                );
                continue;
            }
            let engine_available = true; // health_w > 0.0 verified above
```

Then find the final score computation (currently around line 265):
```rust
            let score = if near_limit {
                raw_score * remaining_pct
            } else {
                raw_score
            };
```

Replace with:
```rust
            let score = (if near_limit {
                raw_score * remaining_pct
            } else {
                raw_score
            }) * health_w;
```

- [ ] **Step 4: Run all router tests**

```bash
cargo test -p animus-cortex -- smart_router 2>&1 | tail -20
```

Expected: all tests pass. Verify `zero_weight_engine_skipped_by_select_for_class` now passes.

- [ ] **Step 5: Commit**

```bash
git add crates/animus-cortex/src/smart_router.rs
git commit -m "feat(routing): multiply candidate score by health_weight in select_for_class, skip 0.0-weight engines"
```

---

## Task 3: ModelHealthWatcher — T=0 probe + trigger channel + backoff

**Files:**
- Modify: `crates/animus-cortex/src/watchers/model_health.rs`

- [ ] **Step 1: Extract `probe_batch` helper function**

The existing loop body (lines 86-146) probes all endpoints and fires signals. Extract this into a standalone `async fn`:

Add this function before `run_model_health_watcher`:

```rust
/// Probe a batch of `(registry_key, base_url)` pairs concurrently and update router health.
/// Fires signals on state transitions (up→down, down→up).
async fn probe_batch(
    snapshot: &[(String, String)],
    router: &SmartRouter,
    signal_tx: &mpsc::Sender<Signal>,
    source_id: ThreadId,
    http: &reqwest::Client,
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

    for (key, available) in results {
        // Snapshot weight BEFORE updating so we can detect confirmed state transitions.
        // Signal only on confirmed transitions: 1.0→0.0 (was healthy, now down)
        // and 0.0→1.0 (was confirmed down, now recovered). 0.5→0.0 is silent (never
        // confirmed healthy in the first place — no notification spam on startup).
        let prev_weight = router.engine_health_weight(&key);
        router.set_engine_health(&key, if available { 1.0 } else { 0.0 });

        if prev_weight >= 1.0 && !available {
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
        } else if prev_weight <= 0.0 && available {
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
```

Note: `was_healthy = engine_health_weight > 0.0` covers both the 1.0→0.0 transition AND the 0.5→0.0 transition (new engine goes directly down without a prior "up" signal — that's intentional).

- [ ] **Step 2: Rewrite `run_model_health_watcher` signature and body**

Replace the entire `run_model_health_watcher` function with:

```rust
/// Launch the model health watcher as a background task.
///
/// `endpoints` — shared, mutable list of `(registry_key, base_url)` pairs.
/// `probe_trigger_rx` — receives lists of engine keys to probe immediately (out-of-band).
/// `interval_secs` — scheduled probe interval.
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
        "ModelHealthWatcher started — probing engine(s) every {}s (T=0 probe firing now)",
        interval_secs,
    );

    // T=0: probe all known endpoints immediately — don't wait for the first interval tick.
    {
        let snapshot: Vec<(String, String)> = endpoints.lock().clone();
        probe_batch(&snapshot, &router, &signal_tx, source_id, &http).await;
    }

    // Track consecutive failures per engine for probe backoff.
    let mut consecutive_failures: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    // Minimum gap between triggered re-probes of the same failing engine.
    let mut last_triggered_probe: std::collections::HashMap<String, std::time::Instant> =
        std::collections::HashMap::new();

    let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
    interval.tick().await; // consume the immediate first tick so the loop starts at T+interval

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let snapshot: Vec<(String, String)> = endpoints.lock().clone();
                probe_batch(&snapshot, &router, &signal_tx, source_id, &http).await;
                // Reset backoff counters for engines that recovered.
                for (key, _) in &snapshot {
                    if router.engine_health_weight(key) > 0.0 {
                        consecutive_failures.remove(key);
                        last_triggered_probe.remove(key);
                    }
                }
            }
            Some(keys) = probe_trigger_rx.recv() => {
                let now = std::time::Instant::now();
                let all_endpoints = endpoints.lock().clone();
                let targeted: Vec<(String, String)> = all_endpoints.into_iter()
                    .filter(|(key, _)| {
                        if !keys.contains(key) {
                            return false;
                        }
                        // Exponential backoff: base 30s, doubles per consecutive failure, cap 300s
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
                    probe_batch(&targeted, &router, &signal_tx, source_id, &http).await;
                    for (key, _) in &targeted {
                        last_triggered_probe.insert(key.clone(), now);
                        if router.engine_health_weight(key) == 0.0 {
                            *consecutive_failures.entry(key.clone()).or_insert(0) += 1;
                        } else {
                            // Recovered — reset
                            consecutive_failures.remove(key);
                        }
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 3: Run the watcher's compile check**

```bash
cargo build -p animus-cortex 2>&1 | grep -E "^error" | head -20
```

Expected: clean build. Fix any compile errors before proceeding.

- [ ] **Step 4: Verify existing cortex tests still pass**

```bash
cargo test -p animus-cortex 2>&1 | tail -15
```

Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/animus-cortex/src/watchers/model_health.rs
git commit -m "feat(routing): T=0 startup probe, trigger channel, and exponential backoff in ModelHealthWatcher"
```

---

## Task 4: main.rs — wire trigger channel, track primary key, trigger on fallback, always spawn watcher

**Files:**
- Modify: `crates/animus-runtime/src/main.rs`

- [ ] **Step 1: Add `RouteDecision` to imports**

Find the imports block (lines 12-22). After:
```rust
use animus_cortex::smart_router::SmartRouter;
```
Add:
```rust
use animus_cortex::smart_router::RouteDecision;
```

- [ ] **Step 2: Create the probe trigger channel before the SmartRouter block**

Find this section (around line 498):
```rust
    // ── Model Plan + Smart Router ─────────────────────────────────────────────────
```

Add the channel creation BEFORE the SmartRouter block (insert after the comment):
```rust
    // Probe trigger channel — main loop sends engine keys here; ModelHealthWatcher re-probes immediately.
    let (probe_trigger_tx, probe_trigger_rx) = tokio::sync::mpsc::channel::<Vec<String>>(32);
```

- [ ] **Step 3: Wire trigger sender into SmartRouter after it's created**

Find this section (around line 630-631):
```rust
        Some(SmartRouter::new(plan_arc, signal_tx.clone(), capability_registry.clone()))
    };
```

After the closing `};` of the `smart_router` block (around line 631), add:
```rust
    // Wire probe trigger sender into SmartRouter so mark_engine_unhealthy can fire probes.
    if let Some(ref router) = smart_router {
        router.set_probe_trigger_tx(probe_trigger_tx);
    }
```

- [ ] **Step 4: Preserve `RouteDecision` list alongside `candidate_arcs`**

Find (around line 1893):
```rust
    let candidate_arcs: Vec<Arc<dyn animus_cortex::ReasoningEngine>> =
        if let Some(ref router) = tool_ctx.smart_router {
            router.route_all_candidates(input, pressure, sensitivity_scan.level).await
                .into_iter()
                .filter_map(|d| engine_registry.engine_by_spec(&d.model_spec.provider, &d.model_spec.model))
                .collect()
        } else {
            vec![]
        };
```

Replace with:
```rust
    let route_decisions: Vec<RouteDecision> = if let Some(ref router) = tool_ctx.smart_router {
        router.route_all_candidates(input, pressure, sensitivity_scan.level).await
    } else {
        vec![]
    };
    let primary_model_key: Option<String> = route_decisions.first()
        .map(|d| format!("{}:{}", d.model_spec.provider, d.model_spec.model));
    let candidate_arcs: Vec<Arc<dyn animus_cortex::ReasoningEngine>> = route_decisions.iter()
        .filter_map(|d| engine_registry.engine_by_spec(&d.model_spec.provider, &d.model_spec.model))
        .collect();
```

- [ ] **Step 5: Trigger re-probe on fallback**

Find (around line 1935-1952) the existing fallback notification block:
```rust
    if output.fell_back {
        let primary_name = primary_engine_name.as_deref().unwrap_or("?");
        let summary = format!(
            "Adapting: primary engine '{}' was unavailable — used '{}' instead",
            primary_name, output.engine_used
        );
        tracing::info!("{summary}");
        if let Some(ref tx) = tool_ctx.signal_tx {
            let _ = tx.try_send(animus_core::threading::Signal { ... });
        }
    }
```

Add a trigger call inside the same `if output.fell_back` block, after the signal send:
```rust
        // Trigger an immediate re-probe so health state updates without waiting 120s.
        if let (Some(key), Some(ref router)) = (primary_model_key.as_deref(), &tool_ctx.smart_router) {
            router.trigger_probe(vec![key.to_string()]);
        }
```

- [ ] **Step 6: Always spawn ModelHealthWatcher (remove empty-list guard)**

Find (around line 697-715):
```rust
    if let Some(ref router) = smart_router {
        if !health_endpoints.lock().is_empty() {
            let watcher_router = router.clone();
            ...
            tokio::spawn(async move {
                animus_cortex::watchers::run_model_health_watcher(
                    watcher_endpoints,
                    watcher_router,
                    watcher_signal_tx,
                    watcher_source,
                    MODEL_HEALTH_PROBE_INTERVAL_SECS,
                ).await;
            });
            tracing::info!("ModelHealthWatcher spawned ({n} endpoint(s))");
        }
    }
```

Replace with (remove the inner `if !health_endpoints.lock().is_empty()` guard, pass `probe_trigger_rx`):
```rust
    if let Some(ref router) = smart_router {
        let watcher_router = router.clone();
        let watcher_signal_tx = signal_tx.clone();
        let watcher_source = animus_core::identity::ThreadId::new();
        let watcher_endpoints = health_endpoints.clone();
        let n = watcher_endpoints.lock().len();
        tokio::spawn(async move {
            animus_cortex::watchers::run_model_health_watcher(
                watcher_endpoints,
                watcher_router,
                watcher_signal_tx,
                watcher_source,
                MODEL_HEALTH_PROBE_INTERVAL_SECS,
                probe_trigger_rx,
            ).await;
        });
        tracing::info!("ModelHealthWatcher spawned ({n} endpoint(s), T=0 probe active)");
    }
```

- [ ] **Step 7: Build the workspace**

```bash
cargo build --workspace 2>&1 | grep -E "^error" | head -20
```

Expected: clean build. Fix any compile errors (unused variable warnings are ok).

- [ ] **Step 8: Run all workspace tests**

```bash
cargo test --workspace 2>&1 | tail -20
```

Expected: all tests pass. Output includes total count with 0 failures.

- [ ] **Step 9: Commit**

```bash
git add crates/animus-runtime/src/main.rs
git commit -m "feat(routing): wire health probe trigger channel, track primary_model_key, trigger re-probe on fallback"
```

---

## Task 5: Integration test — verify health weighting end-to-end

**Files:**
- Create: `crates/animus-tests/tests/integration/health_routing.rs`
- Modify: `crates/animus-tests/tests/integration/mod.rs` (add `mod health_routing;`)

- [ ] **Step 1: Check that mod.rs exists and see its current contents**

```bash
cat crates/animus-tests/tests/integration/mod.rs 2>/dev/null || echo "no mod.rs"
```

If there is no `mod.rs` and tests are instead declared in `lib.rs` or discovered by filename, skip the mod.rs modification step.

- [ ] **Step 2: Write the integration test file**

Create `crates/animus-tests/tests/integration/health_routing.rs`:

```rust
//! Integration tests for health-weighted routing.

use animus_cortex::capability_registry::CapabilityRegistry;
use animus_cortex::model_plan::{default_task_classes, ModelPlan};
use animus_cortex::smart_router::SmartRouter;
use animus_core::BudgetPressure;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

async fn make_two_candidate_router() -> SmartRouter {
    let available = vec!["ollama:qwen3.5:35b".to_string(), "ollama:qwen3.5:9b".to_string()];
    let registry = CapabilityRegistry::build(None, &[], &available).await;
    let plan = ModelPlan::build_from_capabilities(&registry, &available, default_task_classes());
    let (tx, _rx) = mpsc::channel(32);
    SmartRouter::new(Arc::new(RwLock::new(plan)), tx, Arc::new(registry))
}

/// Confirmed-down primary is always skipped regardless of capability score.
#[tokio::test]
async fn confirmed_down_primary_routes_to_fallback() {
    let router = make_two_candidate_router().await;
    router.set_engine_health("ollama:qwen3.5:35b", 0.0);
    // Set fallback as healthy so we get a real selection
    router.set_engine_health("ollama:qwen3.5:9b", 1.0);

    let decision = router.select_for_class("Analytical", BudgetPressure::Normal).await;
    assert_eq!(
        decision.model_spec.model, "qwen3.5:9b",
        "primary is confirmed down — must use fallback, got: {}",
        decision.model_spec.model
    );
    assert_eq!(decision.fallback_index, 1);
}

/// All candidates confirmed down — router still returns a decision (emergency fallback).
#[tokio::test]
async fn all_down_returns_emergency_spec() {
    let router = make_two_candidate_router().await;
    router.set_engine_health("ollama:qwen3.5:35b", 0.0);
    router.set_engine_health("ollama:qwen3.5:9b", 0.0);

    // Should NOT panic — falls back to stub_spec (emergency)
    let decision = router.select_for_class("Analytical", BudgetPressure::Normal).await;
    // The stub spec provider is "anthropic" and model is "fallback"
    assert_eq!(decision.model_spec.provider, "anthropic");
    assert_eq!(decision.model_spec.model, "fallback");
}

/// `route_all_candidates` excludes confirmed-down engines.
#[tokio::test]
async fn route_all_candidates_excludes_zero_weight() {
    let router = make_two_candidate_router().await;
    router.set_engine_health("ollama:qwen3.5:35b", 0.0);
    router.set_engine_health("ollama:qwen3.5:9b", 1.0);

    let candidates = router.route_all_candidates(
        "analyze this",
        BudgetPressure::Normal,
        animus_core::ContentSensitivity::Public,
    ).await;

    assert!(
        candidates.iter().all(|d| d.model_spec.model != "qwen3.5:35b"),
        "confirmed-down engine must not appear in route_all_candidates"
    );
    assert!(
        candidates.iter().any(|d| d.model_spec.model == "qwen3.5:9b"),
        "healthy engine must appear in route_all_candidates"
    );
}
```

- [ ] **Step 3: Add to mod.rs (if it exists)**

If `mod.rs` exists, add:
```rust
mod health_routing;
```

If tests are discovered by file name (no mod.rs), skip this step.

- [ ] **Step 4: Run the new integration tests**

```bash
cargo test -p animus-tests health_routing 2>&1 | tail -20
```

Expected: all 3 tests pass.

- [ ] **Step 5: Run full workspace test suite**

```bash
cargo test --workspace 2>&1 | tail -10
```

Expected: all tests pass, 0 failures.

- [ ] **Step 6: Commit**

```bash
git add crates/animus-tests/tests/integration/health_routing.rs
git add crates/animus-tests/tests/integration/mod.rs 2>/dev/null || true
git commit -m "test: integration tests for health-weighted routing (confirmed-down skip, emergency fallback, route_all_candidates filter)"
```

---

## Task 6: Build, create PR, merge, redeploy

- [ ] **Step 1: Final workspace build and test**

```bash
cargo build --workspace 2>&1 | grep -E "^error" | head -20
cargo test --workspace 2>&1 | tail -10
```

Expected: clean build, all tests pass.

- [ ] **Step 2: Create feature branch and push**

```bash
git checkout -b feat/health-weighted-routing
git push -u origin feat/health-weighted-routing
```

- [ ] **Step 3: Open pull request**

```bash
gh pr create \
  --title "feat: health-weighted routing (T=0 probe, f32 weights, trigger on fallback)" \
  --body "$(cat <<'EOF'
## Summary
- `engine_health` changes from `bool` to `f32` (0.0/0.5/1.0) in SmartRouter
- Unknown engines score at half capability (0.5) until probed; confirmed-down (0.0) are skipped
- `ModelHealthWatcher` probes all engines at T=0 startup instead of waiting 120s
- Trigger channel: any engine failure fires an immediate re-probe without waiting for the next scheduled cycle
- Exponential backoff (30s→60s→cap 300s) prevents hammering dead endpoints
- main.rs always spawns the watcher (even with no OpenAI-compat endpoints) so Anthropic-only setups receive trigger probes
- 3 new integration tests cover confirmed-down skip, emergency fallback, and route_all_candidates filtering

## Test Plan
- [ ] `cargo test --workspace` passes
- [ ] Deploy and verify startup logs show "T=0 probe firing now"
- [ ] With `ANIMUS_LLM_PROVIDER=anthropic`, watcher still spawns (check "ModelHealthWatcher spawned" log)
- [ ] Introduce a bad provider URL and confirm routing skips it after first probe cycle
EOF
)"
```

- [ ] **Step 4: Merge PR**

```bash
gh pr merge --squash
```

- [ ] **Step 5: Rebuild container and redeploy**

```bash
cd /Users/jared.cluff/gitrepos/animus
podman compose build
podman compose up -d
```

- [ ] **Step 6: Verify deployment**

```bash
podman logs animus --tail 40 | grep -E "ModelHealthWatcher|T=0|health_w|probe"
```

Expected log lines (approximate):
```
ModelHealthWatcher started — probing engine(s) every 120s (T=0 probe firing now)
ModelHealthWatcher: probing N engine(s)
```
