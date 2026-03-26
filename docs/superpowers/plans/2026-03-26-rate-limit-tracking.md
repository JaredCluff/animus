# Rate Limit Tracking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Parse Anthropic rate limit response headers, expose state as a shared Arc, and have the SmartRouter proactively avoid near-limit providers by routing to fallbacks while firing exactly one Signal per threshold crossing.

**Architecture:** `RateLimitState` lives in `animus-core` (no lock imports — plain struct with `chrono`). `AnthropicEngine` holds `Arc<parking_lot::RwLock<RateLimitState>>`, updates it after every API call by saving headers before consuming the response body. `ReasoningEngine` trait gains an optional `rate_limit_state()` method (default: `None`). `SmartRouter` stores per-model state handles and checks them inside `select_for_class()` with a single lock acquisition to avoid TOCTOU.

**Tech Stack:** Rust, `parking_lot`, `reqwest`, `chrono`, `tokio` — all already in `animus-cortex`.

**Spec:** `docs/superpowers/specs/2026-03-26-rate-limit-tracking-design.md`

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/animus-core/src/rate_limit.rs` | Create | `RateLimitState` struct + `is_near_limit()` + `RATE_LIMIT_NEAR_THRESHOLD` constant |
| `crates/animus-core/src/lib.rs` | Modify | Export `rate_limit` module and types |
| `crates/animus-cortex/src/llm/anthropic.rs` | Modify | Add state field, save headers before body, update state in `reason()`, override trait method |
| `crates/animus-cortex/src/llm/mod.rs` | Modify | Add `rate_limit_state()` default method to `ReasoningEngine` trait |
| `crates/animus-cortex/src/smart_router.rs` | Modify | Add state map, `register_rate_limit_state()`, rate-limit fallback in `select_for_class()` |
| `crates/animus-runtime/src/main.rs` | Modify | Register engine rate limit states with SmartRouter after construction |

---

## Task 1: `RateLimitState` struct in animus-core

**Files:**
- Create: `crates/animus-core/src/rate_limit.rs`
- Modify: `crates/animus-core/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Add to `crates/animus-core/src/rate_limit.rs` (create the file with tests only first):

```rust
use chrono::Utc;

pub const RATE_LIMIT_NEAR_THRESHOLD: f32 = 0.10;

#[derive(Debug, Clone, Default)]
pub struct RateLimitState {
    pub requests_limit: Option<u32>,
    pub requests_remaining: Option<u32>,
    pub requests_reset: Option<chrono::DateTime<Utc>>,
    pub tokens_limit: Option<u32>,
    pub tokens_remaining: Option<u32>,
    pub tokens_reset: Option<chrono::DateTime<Utc>>,
    pub last_updated: chrono::DateTime<Utc>,
    /// Set true after the near-limit Signal fires.
    /// Reset to false by AnthropicEngine::reason() when capacity recovers.
    /// AnthropicEngine (Layer 1) exclusively owns writes to this flag.
    pub near_limit_notified: bool,
}

impl RateLimitState {
    /// Returns true if requests_remaining OR tokens_remaining is below threshold_pct
    /// of their respective limit. Returns false (optimistic) when values are unknown.
    pub fn is_near_limit(&self, threshold_pct: f32) -> bool {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_requests(limit: u32, remaining: u32) -> RateLimitState {
        RateLimitState {
            requests_limit: Some(limit),
            requests_remaining: Some(remaining),
            ..Default::default()
        }
    }

    fn state_with_tokens(limit: u32, remaining: u32) -> RateLimitState {
        RateLimitState {
            tokens_limit: Some(limit),
            tokens_remaining: Some(remaining),
            ..Default::default()
        }
    }

    #[test]
    fn not_near_limit_when_plenty_remaining() {
        let s = state_with_requests(1000, 500); // 50% remaining
        assert!(!s.is_near_limit(RATE_LIMIT_NEAR_THRESHOLD));
    }

    #[test]
    fn not_near_limit_at_exactly_threshold() {
        // 100/1000 = exactly 10.0% — strict < means NOT near limit
        let s = state_with_requests(1000, 100);
        assert!(!s.is_near_limit(RATE_LIMIT_NEAR_THRESHOLD));
    }

    #[test]
    fn near_limit_just_below_threshold() {
        // 99/1000 = 9.9% — just under threshold, IS near limit
        let s = state_with_requests(1000, 99);
        assert!(s.is_near_limit(RATE_LIMIT_NEAR_THRESHOLD));
    }

    #[test]
    fn near_limit_below_threshold() {
        let s = state_with_requests(1000, 50); // 5%
        assert!(s.is_near_limit(RATE_LIMIT_NEAR_THRESHOLD));
    }

    #[test]
    fn near_limit_at_zero() {
        let s = state_with_requests(1000, 0); // 0%
        assert!(s.is_near_limit(RATE_LIMIT_NEAR_THRESHOLD));
    }

    #[test]
    fn not_near_limit_at_eleven_percent() {
        let s = state_with_requests(1000, 110); // 11%
        assert!(!s.is_near_limit(RATE_LIMIT_NEAR_THRESHOLD));
    }

    #[test]
    fn near_limit_via_tokens_even_if_requests_ok() {
        let mut s = state_with_requests(1000, 500); // requests ok
        s.tokens_limit = Some(100_000);
        s.tokens_remaining = Some(5_000); // 5% tokens
        assert!(s.is_near_limit(RATE_LIMIT_NEAR_THRESHOLD));
    }

    #[test]
    fn not_near_limit_when_values_unknown() {
        // Optimistic default: no headers → not near limit
        let s = RateLimitState::default();
        assert!(!s.is_near_limit(RATE_LIMIT_NEAR_THRESHOLD));
    }

    #[test]
    fn not_near_limit_when_limit_is_zero() {
        // Avoid divide-by-zero; treat zero limit as unknown
        let s = RateLimitState {
            requests_limit: Some(0),
            requests_remaining: Some(0),
            ..Default::default()
        };
        assert!(!s.is_near_limit(RATE_LIMIT_NEAR_THRESHOLD));
    }

    #[test]
    fn near_limit_notified_defaults_false() {
        let s = RateLimitState::default();
        assert!(!s.near_limit_notified);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cd /Users/jared.cluff/gitrepos/animus
cargo test -p animus-core 2>&1 | head -30
```

Expected: compile error `todo!()` panic or compile failure.

- [ ] **Step 3: Implement `is_near_limit()`**

Replace `todo!()` with:

```rust
pub fn is_near_limit(&self, threshold_pct: f32) -> bool {
    let requests_near = match (self.requests_limit, self.requests_remaining) {
        (Some(limit), Some(remaining)) if limit > 0 => {
            (remaining as f32 / limit as f32) < threshold_pct
        }
        _ => false,
    };
    let tokens_near = match (self.tokens_limit, self.tokens_remaining) {
        (Some(limit), Some(remaining)) if limit > 0 => {
            (remaining as f32 / limit as f32) < threshold_pct
        }
        _ => false,
    };
    requests_near || tokens_near
}
```

- [ ] **Step 4: Export from `animus-core/src/lib.rs`**

Add after `pub mod capability;`:
```rust
pub mod rate_limit;
```

Add to the `pub use` block after `pub use capability::{CapabilityState, CognitiveTier};`:
```rust
pub use rate_limit::{RateLimitState, RATE_LIMIT_NEAR_THRESHOLD};
```

- [ ] **Step 5: Run tests and verify pass**

```bash
cargo test -p animus-core 2>&1 | tail -20
```

Expected: all tests pass, including the 8 new `rate_limit` tests.

- [ ] **Step 6: Commit**

```bash
git checkout -b feat/rate-limit-tracking
git add crates/animus-core/src/rate_limit.rs crates/animus-core/src/lib.rs
git commit -m "feat(core): add RateLimitState with is_near_limit()"
```

---

## Task 2: Header parsing in AnthropicEngine

**Files:**
- Modify: `crates/animus-cortex/src/llm/anthropic.rs`

`parse_rate_limit_headers` lives in `anthropic.rs` because `reqwest::header::HeaderMap` is a reqwest type — it must not go in `animus-core`.

- [ ] **Step 1: Write the failing tests**

Add this test module to `anthropic.rs` (append to the existing `#[cfg(test)]` block):

```rust
    // --- rate limit header parsing tests ---

    fn make_headers(pairs: &[(&str, &str)]) -> reqwest::header::HeaderMap {
        let mut map = reqwest::header::HeaderMap::new();
        for (k, v) in pairs {
            map.insert(
                reqwest::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                reqwest::header::HeaderValue::from_str(v).unwrap(),
            );
        }
        map
    }

    #[test]
    fn parse_rate_limit_headers_extracts_all_fields() {
        let headers = make_headers(&[
            ("anthropic-ratelimit-requests-limit", "1000"),
            ("anthropic-ratelimit-requests-remaining", "950"),
            ("anthropic-ratelimit-requests-reset", "2026-03-26T13:00:00Z"),
            ("anthropic-ratelimit-tokens-limit", "100000"),
            ("anthropic-ratelimit-tokens-remaining", "90000"),
            ("anthropic-ratelimit-tokens-reset", "2026-03-26T13:00:00Z"),
        ]);
        let state = parse_rate_limit_headers(&headers);
        assert_eq!(state.requests_limit, Some(1000));
        assert_eq!(state.requests_remaining, Some(950));
        assert!(state.requests_reset.is_some());
        assert_eq!(state.tokens_limit, Some(100_000));
        assert_eq!(state.tokens_remaining, Some(90_000));
        assert!(state.tokens_reset.is_some());
        assert!(!state.near_limit_notified);
    }

    #[test]
    fn parse_rate_limit_headers_handles_missing_headers() {
        let headers = make_headers(&[]);
        let state = parse_rate_limit_headers(&headers);
        assert!(state.requests_limit.is_none());
        assert!(state.requests_remaining.is_none());
        assert!(state.tokens_limit.is_none());
        assert!(state.tokens_remaining.is_none());
        assert!(!state.near_limit_notified);
    }

    #[test]
    fn parse_rate_limit_headers_handles_malformed_values() {
        let headers = make_headers(&[
            ("anthropic-ratelimit-requests-limit", "not-a-number"),
            ("anthropic-ratelimit-tokens-remaining", ""),
        ]);
        let state = parse_rate_limit_headers(&headers);
        // Malformed → None, not a panic
        assert!(state.requests_limit.is_none());
        assert!(state.tokens_remaining.is_none());
    }
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test -p animus-cortex parse_rate_limit 2>&1 | head -20
```

Expected: compile error — `parse_rate_limit_headers` not defined yet.

- [ ] **Step 3: Implement `parse_rate_limit_headers`**

Add this function to `anthropic.rs` before the `#[async_trait]` impl block:

```rust
use animus_core::rate_limit::RateLimitState;

fn parse_rate_limit_headers(headers: &reqwest::header::HeaderMap) -> RateLimitState {
    fn get_u32(headers: &reqwest::header::HeaderMap, key: &str) -> Option<u32> {
        headers.get(key)?.to_str().ok()?.parse().ok()
    }
    fn get_datetime(headers: &reqwest::header::HeaderMap, key: &str) -> Option<chrono::DateTime<chrono::Utc>> {
        let s = headers.get(key)?.to_str().ok()?;
        chrono::DateTime::parse_from_rfc3339(s).ok().map(|dt| dt.with_timezone(&chrono::Utc))
    }
    RateLimitState {
        requests_limit: get_u32(headers, "anthropic-ratelimit-requests-limit"),
        requests_remaining: get_u32(headers, "anthropic-ratelimit-requests-remaining"),
        requests_reset: get_datetime(headers, "anthropic-ratelimit-requests-reset"),
        tokens_limit: get_u32(headers, "anthropic-ratelimit-tokens-limit"),
        tokens_remaining: get_u32(headers, "anthropic-ratelimit-tokens-remaining"),
        tokens_reset: get_datetime(headers, "anthropic-ratelimit-tokens-reset"),
        last_updated: chrono::Utc::now(),
        near_limit_notified: false, // always false from parsing; caller preserves existing value
    }
}
```

- [ ] **Step 4: Run tests and verify pass**

```bash
cargo test -p animus-cortex parse_rate_limit 2>&1
```

Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/animus-cortex/src/llm/anthropic.rs
git commit -m "feat(cortex/llm): add parse_rate_limit_headers() to AnthropicEngine"
```

---

## Task 3: AnthropicEngine state field + reason() update + trait extension

**Files:**
- Modify: `crates/animus-cortex/src/llm/anthropic.rs`
- Modify: `crates/animus-cortex/src/llm/mod.rs`

- [ ] **Step 1: Write failing test for state update in reason()**

Add to the `#[cfg(test)]` block in `anthropic.rs`:

```rust
    #[test]
    fn rate_limit_state_returns_some() {
        // AnthropicEngine should expose its rate limit state handle
        let engine = AnthropicEngine::new("fake-key".to_string(), "claude-3-5-haiku-20241022".to_string(), 1024);
        assert!(engine.rate_limit_state().is_some());
    }

    #[test]
    fn rate_limit_state_default_has_no_data() {
        let engine = AnthropicEngine::new("fake-key".to_string(), "claude-3-5-haiku-20241022".to_string(), 1024);
        let state = engine.rate_limit_state().unwrap();
        let s = state.read();
        assert!(s.requests_limit.is_none());
        assert!(s.tokens_limit.is_none());
        assert!(!s.near_limit_notified);
    }
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test -p animus-cortex rate_limit_state 2>&1 | head -20
```

Expected: compile error — `rate_limit_state()` method doesn't exist yet.

- [ ] **Step 3: Add `rate_limit_state` field to `AnthropicEngine`**

In `anthropic.rs`, find:
```rust
pub struct AnthropicEngine {
    client: reqwest::Client,
    auth: Auth,
    model: String,
    max_tokens: usize,
}
```

Replace with:
```rust
pub struct AnthropicEngine {
    client: reqwest::Client,
    auth: Auth,
    model: String,
    max_tokens: usize,
    rate_limit_state: std::sync::Arc<parking_lot::RwLock<RateLimitState>>,
}
```

- [ ] **Step 4: Update the three constructors that build `Self` directly**

The three constructors that construct `Self { ... }` directly (not by delegating to another constructor) are `new`, `with_oauth`, and `from_claude_code`. Each needs the new field:

In `new()`:
```rust
// After the existing fields:
rate_limit_state: std::sync::Arc::new(parking_lot::RwLock::new(RateLimitState::default())),
```

In `with_oauth()`:
```rust
rate_limit_state: std::sync::Arc::new(parking_lot::RwLock::new(RateLimitState::default())),
```

In `from_claude_code()`:
```rust
rate_limit_state: std::sync::Arc::new(parking_lot::RwLock::new(RateLimitState::default())),
```

The other constructors (`from_env`, `from_oauth_env`, `from_best_available`) delegate to these three and inherit the field automatically.

- [ ] **Step 5: Update `reason()` to save headers and update state**

In `reason()`, find the lines:
```rust
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| AnimusError::Llm(format!("failed to read response: {e}")))?;
```

Replace with:
```rust
        let status = response.status();
        let response_headers = response.headers().clone(); // save BEFORE consuming body
        let body = response
            .text()
            .await
            .map_err(|e| AnimusError::Llm(format!("failed to read response: {e}")))?;
```

After the existing `if !status.is_success()` block (after the `serde_json::from_str` of the response), add state update:

```rust
        // Layer 1: update rate limit state from response headers (no LLM)
        {
            let parsed = parse_rate_limit_headers(&response_headers);
            let mut state = self.rate_limit_state.write();
            // Preserve near_limit_notified across the overwrite:
            // parse_rate_limit_headers() always returns false for this field,
            // so we save and restore it based on whether we're still near limit.
            let was_notified = state.near_limit_notified;
            let now_near = parsed.is_near_limit(animus_core::RATE_LIMIT_NEAR_THRESHOLD);
            *state = parsed;
            // If still near limit, keep the flag (prevent duplicate Signal from SmartRouter).
            // If recovered, reset it so the next threshold crossing fires again.
            state.near_limit_notified = if now_near { was_notified } else { false };
        }
```

Place this immediately before the `let content = api_response.content...` line.

- [ ] **Step 6: Add `rate_limit_state()` to the `ReasoningEngine` trait**

In `crates/animus-cortex/src/llm/mod.rs`, find the trait definition. Add this default method after `fn model_name(&self) -> &str;`:

```rust
    /// Return a handle to the provider's current rate limit state, if tracked.
    /// Default: None (providers that don't track rate limits need no changes).
    fn rate_limit_state(&self) -> Option<std::sync::Arc<parking_lot::RwLock<animus_core::RateLimitState>>> {
        None
    }
```

- [ ] **Step 7: Override `rate_limit_state()` in `AnthropicEngine`**

In `anthropic.rs`, inside the `impl ReasoningEngine for AnthropicEngine` block, add after `fn model_name(&self)`:

```rust
    fn rate_limit_state(&self) -> Option<std::sync::Arc<parking_lot::RwLock<animus_core::RateLimitState>>> {
        Some(self.rate_limit_state.clone())
    }
```

- [ ] **Step 8: Run tests**

```bash
cargo test -p animus-cortex rate_limit_state 2>&1
```

Expected: 2 new tests pass.

- [ ] **Step 9: Run all cortex tests to check for regressions**

```bash
cargo test -p animus-cortex 2>&1 | tail -20
```

Expected: all tests pass.

- [ ] **Step 10: Commit**

```bash
git add crates/animus-cortex/src/llm/anthropic.rs crates/animus-cortex/src/llm/mod.rs
git commit -m "feat(cortex/llm): AnthropicEngine tracks rate limit state from response headers"
```

---

## Task 4: SmartRouter rate-limit-aware routing

**Files:**
- Modify: `crates/animus-cortex/src/smart_router.rs`

- [ ] **Step 1: Write failing tests**

Add to the `#[cfg(test)]` block in `smart_router.rs`:

```rust
    use animus_core::rate_limit::RateLimitState;
    use std::sync::Arc;
    use parking_lot::RwLock;

    fn make_near_limit_state() -> Arc<RwLock<RateLimitState>> {
        Arc::new(RwLock::new(RateLimitState {
            requests_limit: Some(1000),
            requests_remaining: Some(50), // 5% — near limit
            near_limit_notified: false,
            ..Default::default()
        }))
    }

    fn make_ok_state() -> Arc<RwLock<RateLimitState>> {
        Arc::new(RwLock::new(RateLimitState {
            requests_limit: Some(1000),
            requests_remaining: Some(500), // 50% — fine
            near_limit_notified: false,
            ..Default::default()
        }))
    }

    #[tokio::test]
    async fn register_rate_limit_state_stores_handle() {
        let (router, _rx) = make_router();
        let state = make_ok_state();
        // Analytical primary is "ollama:qwen3.5:35b" in the test plan
        router.register_rate_limit_state("ollama:qwen3.5:35b", state.clone());
        // verify it doesn't panic and the map has an entry
        // (no public accessor — test via routing behavior below)
    }

    #[tokio::test]
    async fn routes_to_fallback_when_primary_near_limit() {
        let (router, _rx) = make_router();
        // Register the Analytical primary as near-limit
        // default_plan with [35b, 9b] sets Analytical primary = "ollama:qwen3.5:35b"
        let state = make_near_limit_state();
        router.register_rate_limit_state("ollama:qwen3.5:35b", state);
        let decision = router.select_for_class("Analytical").await;
        // Should have used a fallback (fallback_index > 0)
        assert!(decision.fallback_index > 0, "expected fallback route, got primary");
    }

    #[tokio::test]
    async fn fires_signal_on_first_near_limit_crossing() {
        let (router, mut rx) = make_router();
        let state = make_near_limit_state();
        router.register_rate_limit_state("ollama:qwen3.5:35b", state);
        router.select_for_class("Analytical").await;
        // Signal should have been sent
        let signal = rx.try_recv();
        assert!(signal.is_ok(), "expected a Signal to be fired");
        assert_eq!(signal.unwrap().priority, SignalPriority::Normal);
    }

    #[tokio::test]
    async fn does_not_fire_duplicate_signal_when_already_notified() {
        let (router, mut rx) = make_router();
        let state = Arc::new(RwLock::new(RateLimitState {
            requests_limit: Some(1000),
            requests_remaining: Some(50),
            near_limit_notified: true, // already fired — flag was set by a prior routing call
            ..Default::default()
        }));
        router.register_rate_limit_state("ollama:qwen3.5:35b", state);
        router.select_for_class("Analytical").await;
        // No new Signal
        assert!(rx.try_recv().is_err(), "should not fire duplicate Signal");
    }
```

**Model name note:** `default_plan(&["ollama:qwen3.5:35b", "ollama:qwen3.5:9b"])` assigns `ModelSpec { model: "ollama:qwen3.5:35b" }` as the Analytical primary. The `rate_limit_states` map key matches `route.primary.model` exactly — use `"ollama:qwen3.5:35b"` in tests.

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test -p animus-cortex register_rate_limit 2>&1 | head -20
cargo test -p animus-cortex routes_to_fallback 2>&1 | head -20
```

Expected: compile errors — `register_rate_limit_state` not defined.

- [ ] **Step 3: Add `rate_limit_states` field to `SmartRouter`**

In `smart_router.rs`, find the `SmartRouter` struct. Add the field:

```rust
pub struct SmartRouter {
    plan: Arc<RwLock<ModelPlan>>,
    classifier: Arc<RwLock<HeuristicClassifier>>,
    route_health: Arc<Mutex<HashMap<String, RouteHealth>>>,
    signal_tx: mpsc::Sender<Signal>,
    source_id: ThreadId,
    /// Per-model rate limit state handles — populated by register_rate_limit_state().
    rate_limit_states: Arc<Mutex<HashMap<String, std::sync::Arc<parking_lot::RwLock<animus_core::RateLimitState>>>>>,
}
```

Add to the `Self { ... }` in `SmartRouter::new()`:
```rust
rate_limit_states: Arc::new(Mutex::new(HashMap::new())),
```

- [ ] **Step 4: Add `register_rate_limit_state()` method**

Add to `impl SmartRouter`:

```rust
/// Register a rate limit state handle for a model.
/// Call once per engine at startup: `router.register_rate_limit_state(engine.model_name(), engine.rate_limit_state().unwrap())`.
pub fn register_rate_limit_state(
    &self,
    model_name: &str,
    state: std::sync::Arc<parking_lot::RwLock<animus_core::RateLimitState>>,
) {
    self.rate_limit_states.lock().insert(model_name.to_string(), state);
}
```

- [ ] **Step 5: Add rate-limit check to `select_for_class()`**

In `select_for_class()`, find the non-degraded early return:
```rust
        if !primary_degraded {
            return RouteDecision {
                class_name: class_name.to_string(),
                model_spec: route.primary.clone(),
                fallback_index: 0,
            };
        }
```

Replace with:

```rust
        if !primary_degraded {
            // Layer 2 + 3: rate limit check with single write-lock (atomic read + arm flag)
            // Write lock prevents TOCTOU: reading is_near_limit() and setting near_limit_notified
            // happen inside the same guard. Drop all rate_limit_states locks before try_send.
            let (near_limit, should_notify) = {
                let states = self.rate_limit_states.lock();
                if let Some(rl_arc) = states.get(&route.primary.model) {
                    let mut state = rl_arc.write(); // write lock — need to set flag atomically
                    let near = state.is_near_limit(animus_core::RATE_LIMIT_NEAR_THRESHOLD);
                    let notify = near && !state.near_limit_notified;
                    if notify {
                        state.near_limit_notified = true; // arm: SmartRouter owns this write
                    }
                    (near, notify)
                } else {
                    (false, false)
                }
            }; // all rate_limit_states locks dropped here — safe to try_send

            if near_limit {
                // Layer 3: fire one Signal on threshold crossing (try_send is non-blocking — no .await needed)
                if should_notify {
                    tracing::info!("Rate limit near for model '{}' — routing to fallback", route.primary.model);
                    let _ = self.signal_tx.try_send(Signal {
                        source_thread: self.source_id,
                        target_thread: ThreadId::default(),
                        priority: SignalPriority::Normal,
                        summary: format!(
                            "Rate limit near for model '{}' — routing to fallback",
                            route.primary.model
                        ),
                        segment_refs: vec![],
                        created: Utc::now(),
                    });
                }

                // Route to first non-degraded fallback (plan and health still in scope — no re-acquire needed)
                for (i, fallback) in route.fallbacks.iter().enumerate() {
                    let fb_key = format!("{}:fallback:{}", class_name, i);
                    let fb_degraded = health.get(&fb_key).map(|h| h.degraded).unwrap_or(false);
                    if !fb_degraded {
                        return RouteDecision {
                            class_name: class_name.to_string(),
                            model_spec: fallback.clone(),
                            fallback_index: i + 1,
                        };
                    }
                }

                // No fallback available — return primary anyway
                return RouteDecision {
                    class_name: class_name.to_string(),
                    model_spec: route.primary.clone(),
                    fallback_index: 0,
                };
            }

            return RouteDecision {
                class_name: class_name.to_string(),
                model_spec: route.primary.clone(),
                fallback_index: 0,
            };
        }
```

Note: `Utc` and `ThreadId` are already imported in `smart_router.rs`. Verify imports before running.

- [ ] **Step 6: Check that `is_near_limit` and `RATE_LIMIT_NEAR_THRESHOLD` are in scope**

At the top of `smart_router.rs`, add if not already present:
```rust
use animus_core::{RateLimitState, RATE_LIMIT_NEAR_THRESHOLD};
```

- [ ] **Step 7: Verify the rate_limit_states field compiles with RwLock type annotation**

The `rate_limit_states` field uses nested lock types. Confirm the full type annotation compiles without ambiguity:
```rust
// In struct:
rate_limit_states: Arc<Mutex<HashMap<String, std::sync::Arc<parking_lot::RwLock<animus_core::RateLimitState>>>>>,
// In register_rate_limit_state signature:
state: std::sync::Arc<parking_lot::RwLock<animus_core::RateLimitState>>,
```

If `Arc` is already imported as `std::sync::Arc` at the top of `smart_router.rs`, use `Arc` directly. Confirm with a quick build before running tests.

- [ ] **Step 8: Run tests**

```bash
cargo test -p animus-cortex smart_router 2>&1 | tail -30
```

Expected: new rate limit tests pass, all existing tests still pass.

- [ ] **Step 9: Run full cortex test suite**

```bash
cargo test -p animus-cortex 2>&1 | tail -20
```

Expected: all tests pass.

- [ ] **Step 10: Commit**

```bash
git add crates/animus-cortex/src/smart_router.rs
git commit -m "feat(cortex): SmartRouter routes to fallback when provider near rate limit"
```

---

## Task 5: Wire up in `main.rs`

**Files:**
- Modify: `crates/animus-runtime/src/main.rs`

- [ ] **Step 1: Locate where engines are constructed**

Search for `AnthropicEngine::from_best_available` or the engine construction block in `main.rs`. The `smart_router` variable is already constructed. Find where both are accessible.

- [ ] **Step 2: Register the engine's rate limit state with the SmartRouter**

After each engine is constructed and the `smart_router` is initialized, add:

```rust
// Register rate limit state for Anthropic engine with SmartRouter
if let Some(engine_ref) = reasoning_engine_for_rl_registration {
    if let Some(rl_state) = engine_ref.rate_limit_state() {
        smart_router.register_rate_limit_state(engine_ref.model_name(), rl_state);
    }
}
```

The exact implementation depends on how the engine is held. If the engine is stored as `Arc<dyn ReasoningEngine>`, call `.rate_limit_state()` on the arc before boxing it. The engine must implement the trait method — which `AnthropicEngine` now does.

**Read the relevant section of `main.rs` around engine construction and `smart_router` initialization to determine the exact variable names and insertion point.** The engine is typically constructed before the `EngineRegistry` is built.

- [ ] **Step 3: Build**

```bash
cargo build -p animus-runtime 2>&1 | tail -20
```

Expected: clean build, no errors.

- [ ] **Step 4: Commit**

```bash
git add crates/animus-runtime/src/main.rs
git commit -m "feat(runtime): register Anthropic engine rate limit state with SmartRouter"
```

---

## Task 6: Final validation and PR

- [ ] **Step 1: Run all affected tests**

```bash
cargo test -p animus-core -p animus-cortex 2>&1 | tail -30
```

Expected: all tests pass, including all new rate limit tests.

- [ ] **Step 2: Build runtime**

```bash
cargo build -p animus-runtime 2>&1 | tail -10
```

Expected: clean.

- [ ] **Step 3: Check work hours before committing**

```bash
TZ=America/Chicago date '+%A %H:%M'
```

Expected: outside 8am–5pm Mon–Fri CT.

- [ ] **Step 4: Push branch and open PR**

```bash
git push -u origin feat/rate-limit-tracking
gh pr create \
  --title "feat: Rate limit tracking for Anthropic engine" \
  --body "$(cat <<'EOF'
## Summary

- Adds RateLimitState to animus-core — parses anthropic-ratelimit-* headers, tracks near_limit_notified flag
- AnthropicEngine saves response headers before body consumption, updates shared Arc<RwLock<RateLimitState>> after each call
- ReasoningEngine trait gains optional rate_limit_state() method (default: None — other engines unchanged)
- SmartRouter gains register_rate_limit_state(), checks rate limit with single lock acquisition before routing, fires one Normal Signal on threshold crossing, routes to fallback

## Three-layer compliance
Layer 1: AnthropicEngine updates state from headers (no LLM)
Layer 2: SmartRouter checks threshold before routing (no LLM)
Layer 3: One Signal fired on threshold crossing

## Test plan
- [ ] cargo test -p animus-core (RateLimitState unit tests)
- [ ] cargo test -p animus-cortex (header parsing, state update, SmartRouter fallback routing)
- [ ] cargo build -p animus-runtime (clean)
EOF
)"
```

- [ ] **Step 5: Merge**

```bash
gh pr merge --squash --auto
```

- [ ] **Step 6: Pull master**

```bash
git checkout master && git pull origin master
```
