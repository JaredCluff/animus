# Rate Limit Tracking Design

**Date:** 2026-03-26
**Status:** Approved
**Crates affected:** `animus-core`, `animus-cortex`

---

## Problem

The Anthropic API returns `anthropic-ratelimit-*` headers on every response, reporting how many requests and tokens remain in the current rate limit window. The `AnthropicEngine` currently consumes the response body without reading these headers — the data is discarded on every call.

Without this state, the `SmartRouter` has no way to proactively avoid a near-limit provider. The first indication of a limit is a hard 429 error, which causes a routing failure and delays the response.

---

## Design Goals

1. Parse and persist rate limit state from Anthropic response headers (Layer 1 — no LLM)
2. Detect when remaining capacity drops below a threshold (Layer 2 — no LLM)
3. Fire exactly one `Signal` when the threshold is crossed (Layer 3 — LLM notified once)
4. SmartRouter avoids near-limit providers by routing to fallbacks before a 429 occurs

This follows the three-layer pattern established throughout Animus:
```
State Management (no LLM) → Delta Detection (no LLM) → Signal (LLM on change only)
```

---

## Architecture

### Component 1: `RateLimitState` — `animus-core/src/rate_limit.rs`

A new shared state type, placed in `animus-core` so both `AnthropicEngine` (writer) and `SmartRouter` (reader) can access it without circular dependencies. Follows the same pattern as `CapabilityState`.

```rust
#[derive(Debug, Clone, Default)]
pub struct RateLimitState {
    pub requests_limit: Option<u32>,
    pub requests_remaining: Option<u32>,
    pub requests_reset: Option<DateTime<Utc>>,
    pub tokens_limit: Option<u32>,
    pub tokens_remaining: Option<u32>,
    pub tokens_reset: Option<DateTime<Utc>>,
    pub last_updated: DateTime<Utc>,
    /// Set to true after the near-limit Signal fires; reset when state recovers.
    /// Prevents repeated Signals for the same crossing event.
    pub near_limit_notified: bool,
}
```

**Key method:**
```rust
pub fn is_near_limit(&self, threshold_pct: f32) -> bool
```
Returns `true` if `requests_remaining` OR `tokens_remaining` is below `threshold_pct` of their respective limit. If either value is unknown (headers absent), conservatively returns `false`.

**Exported** from `animus_core::lib.rs` alongside `CapabilityState`.

---

### Component 2: `AnthropicEngine` changes — `animus-cortex/src/llm/anthropic.rs`

`AnthropicEngine` gains a new field:
```rust
pub struct AnthropicEngine {
    // ... existing fields ...
    rate_limit_state: Arc<parking_lot::RwLock<RateLimitState>>,
}
```

All constructors (`new`, `with_oauth`, `from_claude_code`, `from_best_available`) initialize this field with a default `Arc<parking_lot::RwLock<RateLimitState::default()>>`.

**Header parsing** lives in `anthropic.rs` (not `animus-core`) because `reqwest::header::HeaderMap` is a reqwest type:

```rust
fn parse_rate_limit_headers(headers: &reqwest::header::HeaderMap) -> RateLimitState
```

This function extracts the six `anthropic-ratelimit-*` header values, tolerating missing or malformed headers gracefully by using `Option`.

**In `reason()`**, the current code consumes the response body without reading headers:
```rust
let body = response.text().await?; // headers lost here
```

Changed to:
```rust
let headers = response.headers().clone(); // save before consuming
let body = response.text().await?;
// After successful parse, update rate limit state
let parsed = parse_rate_limit_headers(&headers);
{
    let mut state = self.rate_limit_state.write();
    // Update all fields; preserve near_limit_notified unless we've recovered
    let recovered = !parsed.is_near_limit(RATE_LIMIT_NEAR_THRESHOLD);
    *state = parsed;
    if recovered { state.near_limit_notified = false; }
}
```

**`ReasoningEngine` trait extension:**
```rust
fn rate_limit_state(&self) -> Option<Arc<parking_lot::RwLock<RateLimitState>>> {
    None  // default — other engines need no changes
}
```

`AnthropicEngine` overrides this to return `Some(self.rate_limit_state.clone())`.

---

### Component 3: `SmartRouter` changes — `animus-cortex/src/smart_router.rs`

**New field:**
```rust
rate_limit_states: Arc<parking_lot::Mutex<HashMap<String, Arc<parking_lot::RwLock<RateLimitState>>>>>,
```

**New method:**
```rust
pub fn register_rate_limit_state(
    &self,
    model_name: &str,
    state: Arc<parking_lot::RwLock<RateLimitState>>,
)
```

Called from `main.rs` after constructing each engine. Populates the map.

**In `route()` / route selection**, after selecting a `ModelSpec`, before returning:

```rust
// Check rate limit for selected provider
if let Some(rl_state) = rate_limit_states.get(&selected_model.model_id) {
    let mut state = rl_state.write();
    if state.is_near_limit(RATE_LIMIT_NEAR_THRESHOLD) {
        if !state.near_limit_notified {
            state.near_limit_notified = true;
            // Fire one Signal — Normal priority (informational, not urgent)
            let _ = signal_tx.try_send(Signal { ... content: "Rate limit warning ..." ... });
        }
        // Skip to next fallback in the ModelPlan
        // ... fallback selection ...
        return RouteDecision { ..., fallback_index: 1 };
    } else if state.near_limit_notified {
        // Recovered — allow re-notification next time threshold is crossed
        state.near_limit_notified = false;
    }
}
```

**Threshold constant:**
```rust
const RATE_LIMIT_NEAR_THRESHOLD: f32 = 0.10; // fire when ≤ 10% remaining
```

---

### Component 4: `main.rs` wiring

After constructing each `ReasoningEngine`, register its rate limit state with the `SmartRouter`:

```rust
if let Some(rl_state) = engine.rate_limit_state() {
    smart_router.register_rate_limit_state(engine.model_name(), rl_state);
}
```

No new `Arc` allocations in main.rs — the handle comes from the engine itself.

---

## Data Flow

```
AnthropicEngine::reason()
  → API call succeeds
  → headers.clone() before body.text()
  → parse_rate_limit_headers() → RateLimitState
  → write-lock state, update all fields
  → if recovered: near_limit_notified = false

SmartRouter::route()
  → select ModelSpec from plan
  → read-lock rate limit state for that model
  → is_near_limit(0.10)?
      yes + not notified → near_limit_notified = true → Signal (Normal) → use fallback
      yes + already notified → use fallback silently
      no + was notified → near_limit_notified = false (recovered)
      no → proceed normally
```

---

## What This Does NOT Do

- Does not add rate limit tracking to `OpenAICompatEngine` (out of scope; Anthropic-specific headers)
- Does not expose rate limit state as an introspective tool (future: `get_rate_limit_state` tool, same pattern as `get_capability_state`)
- Does not implement backoff/retry — existing failure handling (`RouteHealth`) covers 429s that slip through
- Does not parse `retry-after` header — that is handled by existing error recovery

---

## Testing

| Test | Where |
|------|-------|
| `RateLimitState::is_near_limit()` — boundary conditions (0%, 10%, 11%, 100%) | `rate_limit.rs` |
| `parse_rate_limit_headers()` — valid headers, missing headers, malformed values | `anthropic.rs` |
| `near_limit_notified` reset on recovery | `rate_limit.rs` |
| SmartRouter routes to fallback when provider is near limit | `smart_router.rs` |
| SmartRouter does NOT fire duplicate Signals for same window | `smart_router.rs` |
| Signal fired exactly once on threshold crossing | `smart_router.rs` |
| `reason()` still succeeds when `anthropic-ratelimit-*` headers absent | `anthropic.rs` |

---

## Layer Compliance

| Layer | Component | LLM used? |
|-------|-----------|-----------|
| Layer 1 (State) | `RateLimitState` updated by `AnthropicEngine` after each call | No |
| Layer 2 (Delta) | `SmartRouter::route()` checks `is_near_limit()` | No |
| Layer 3 (Signal) | One `Signal` fired on threshold crossing | No (Signal notifies LLM, but firing itself is token-free) |

---

## Files Changed

| File | Change |
|------|--------|
| `crates/animus-core/src/rate_limit.rs` | New — `RateLimitState` struct |
| `crates/animus-core/src/lib.rs` | Add `pub mod rate_limit; pub use rate_limit::RateLimitState;` |
| `crates/animus-cortex/src/llm/anthropic.rs` | Add field, parse headers, update state, override trait method |
| `crates/animus-cortex/src/llm/mod.rs` | Add `rate_limit_state()` method to `ReasoningEngine` trait |
| `crates/animus-cortex/src/smart_router.rs` | Add rate limit state map, fallback logic, Signal |
| `crates/animus-runtime/src/main.rs` | Register engine rate limit states with SmartRouter |
