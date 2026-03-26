# Rate Limit Tracking Design

**Date:** 2026-03-26
**Status:** Approved
**Crates affected:** `animus-core`, `animus-cortex`

---

## Problem

The Anthropic API returns `anthropic-ratelimit-*` headers on every response, reporting how many requests and tokens remain in the current rate limit window. `AnthropicEngine` currently consumes the response body without reading these headers — the data is discarded on every call.

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

The struct itself has no lock dependencies — wrapping in `Arc<parking_lot::RwLock<RateLimitState>>` happens in `animus-cortex` (the same pattern used for `CapabilityState`). No `Cargo.toml` changes are needed for `animus-core`.

**Important:** `rate_limit.rs` must NOT import `parking_lot`. The lock wrapper (`Arc<parking_lot::RwLock<RateLimitState>>`) is applied only at the call site in `animus-cortex`. Any `use parking_lot` in `rate_limit.rs` would require adding `parking_lot` to `animus-core/Cargo.toml` — do not do this.

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
    /// Set true after the near-limit Signal fires.
    /// Reset to false by AnthropicEngine::reason() when capacity recovers.
    /// AnthropicEngine (Layer 1) exclusively owns writes to this flag.
    pub near_limit_notified: bool,
}
```

**Threshold constant** (also in `animus-core/src/rate_limit.rs`, exported):
```rust
pub const RATE_LIMIT_NEAR_THRESHOLD: f32 = 0.10;
```
Placing the constant here ensures both `AnthropicEngine` and `SmartRouter` use the same value.

**Key method:**
```rust
pub fn is_near_limit(&self, threshold_pct: f32) -> bool
```
Returns `true` if `requests_remaining` OR `tokens_remaining` is below `threshold_pct` of their respective limit. When headers are absent (values unknown), returns `false` — this is an **optimistic** choice that prefers availability. The reasoning: an absence of rate limit headers means the provider didn't report a constraint, so we proceed normally rather than blocking on uncertainty.

**Exported** from `animus_core::lib.rs` alongside `CapabilityState`:
```rust
pub mod rate_limit;
pub use rate_limit::{RateLimitState, RATE_LIMIT_NEAR_THRESHOLD};
```

---

### Component 2: `AnthropicEngine` changes — `animus-cortex/src/llm/anthropic.rs`

`AnthropicEngine` gains a new field:
```rust
pub struct AnthropicEngine {
    // ... existing fields ...
    rate_limit_state: Arc<parking_lot::RwLock<RateLimitState>>,
}
```

**Constructor changes:** Only the two leaf constructors (`new` and `with_oauth`) need to initialize this field, since all other constructors delegate to them:
- `new(api_key, model, max_tokens)` — initializes `rate_limit_state`
- `with_oauth(token, model, max_tokens)` — initializes `rate_limit_state`
- `from_claude_code` → calls no leaf constructor directly; needs explicit field initialization
- `from_env` → delegates to `new` → inherits automatically
- `from_oauth_env` → delegates to `with_oauth` → inherits automatically
- `from_best_available` → delegates to `with_oauth` or `from_claude_code` → inherits or needs explicit init

All six constructors must produce an `AnthropicEngine` with `rate_limit_state` initialized to `Arc::new(parking_lot::RwLock::new(RateLimitState::default()))`. Implementer should verify delegation chains and add explicit initialization to any constructor that doesn't flow through `new` or `with_oauth`.

**Header parsing function** (lives in `anthropic.rs`, not `animus-core`, because `reqwest::header::HeaderMap` is a reqwest type):
```rust
fn parse_rate_limit_headers(headers: &reqwest::header::HeaderMap) -> RateLimitState
```
Extracts six `anthropic-ratelimit-*` header values. Tolerates missing or malformed headers gracefully by using `Option`. Sets `last_updated = Utc::now()`. Sets `near_limit_notified = false` (placeholder — the real value is preserved by the update logic below).

**In `reason()`**, the current code discards headers before the body is read:
```rust
let body = response.text().await?; // headers lost here
```

Changed to:
```rust
let response_headers = response.headers().clone(); // save BEFORE consuming body
let body = response.text().await?;
```

After a successful API response (parse succeeds), update rate limit state:
```rust
let parsed = parse_rate_limit_headers(&response_headers);
{
    let mut state = self.rate_limit_state.write();
    // Preserve near_limit_notified across the state overwrite.
    // parse_rate_limit_headers() always returns near_limit_notified=false,
    // so we must save and restore the flag manually.
    let was_notified = state.near_limit_notified;
    let now_near = parsed.is_near_limit(RATE_LIMIT_NEAR_THRESHOLD);
    *state = parsed;
    // Layer 1 owns this flag:
    //   - if we were notified and are still near limit, preserve the flag
    //   - if we recovered (no longer near limit), reset it so next crossing fires again
    state.near_limit_notified = if now_near { was_notified } else { false };
}
```

This is the **only** place where `near_limit_notified` is written. The SmartRouter reads but does not modify this flag.

**`ReasoningEngine` trait extension** in `animus-cortex/src/llm/mod.rs`:
```rust
fn rate_limit_state(&self) -> Option<Arc<parking_lot::RwLock<RateLimitState>>> {
    None  // default — OpenAICompatEngine, MockEngine need no changes
}
```

`AnthropicEngine` overrides:
```rust
fn rate_limit_state(&self) -> Option<Arc<parking_lot::RwLock<RateLimitState>>> {
    Some(self.rate_limit_state.clone())
}
```

---

### Component 3: `SmartRouter` changes — `animus-cortex/src/smart_router.rs`

**New field:**
```rust
rate_limit_states: Arc<parking_lot::Mutex<HashMap<String, Arc<parking_lot::RwLock<RateLimitState>>>>>,
```

The map key is the model string (matching `engine.model_name()` which returns `&self.model`, and looked up against `selected_model.model` from `ModelSpec`).

**New method:**
```rust
pub fn register_rate_limit_state(
    &self,
    model_name: &str,
    state: Arc<parking_lot::RwLock<RateLimitState>>,
)
```

Called from `main.rs` after constructing each engine. Populates the map.

**In route selection**, after selecting a `ModelSpec`, before returning the `RouteDecision`:

```rust
// Check rate limit for selected provider.
// SmartRouter reads state only — it does NOT write near_limit_notified.
// Acquire the lock ONCE and read both values from the same guard to avoid TOCTOU.
let (near_limit, should_notify) = {
    let states = self.rate_limit_states.lock();
    if let Some(rl_arc) = states.get(&selected_model.model) {
        let state = rl_arc.read();
        let near = state.is_near_limit(RATE_LIMIT_NEAR_THRESHOLD);
        let notify = near && !state.near_limit_notified;
        (near, notify)
    } else {
        (false, false)
    }
    // Lock dropped here — before try_send
};

if near_limit {
    if should_notify {
        let _ = self.signal_tx.try_send(Signal { ... });
    }
    // Route to fallback
    return RouteDecision { ..., fallback_index: 1 };
}
```

**Flag write ownership (split by operation):**
- `SmartRouter` sets `near_limit_notified = true` when it fires the Signal (it is the one firing, so it owns the "arm" operation)
- `AnthropicEngine::reason()` resets `near_limit_notified = false` when capacity recovers (it is the one observing header data, so it owns the "reset" operation)

**Single lock acquisition:** Acquire the `rate_limit_states` Mutex exactly once per routing decision. Use a **write lock** on the inner `RwLock<RateLimitState>` so that reading `is_near_limit()` + `near_limit_notified` and setting `near_limit_notified = true` are atomic within the same lock guard. Drop all locks before calling `try_send`. This eliminates any TOCTOU window between reading and writing the flag.

**Constant reuse:** Import `RATE_LIMIT_NEAR_THRESHOLD` from `animus_core::rate_limit`.

---

### Component 4: `main.rs` wiring

After constructing each `ReasoningEngine`, register its rate limit state with the `SmartRouter`:

```rust
if let Some(rl_state) = engine.rate_limit_state() {
    smart_router.register_rate_limit_state(engine.model_name(), rl_state);
}
```

No new `Arc` allocations in `main.rs` — the handle comes from the engine itself.

---

## Data Flow

```
AnthropicEngine::reason()
  → response.headers().clone()          // save before body consumed
  → parse_rate_limit_headers(headers)   // Layer 1: state update
  → write-lock state:
      was_notified = state.near_limit_notified
      *state = parsed
      state.near_limit_notified = if now_near { was_notified } else { false }

SmartRouter::route()
  → select ModelSpec from plan
  → read-lock rate limit state for that model  // Layer 2: delta check
  → is_near_limit(0.10)?
      yes: read near_limit_notified
           if false → Signal (Normal priority)   // Layer 3: one notification
           drop all locks → route to fallback
      no:  proceed normally
  NOTE: SmartRouter reads, never writes near_limit_notified
```

---

## What This Does NOT Do

- Does not add rate limit tracking to `OpenAICompatEngine` (Anthropic-specific headers)
- Does not expose rate limit state as an introspective tool (future work: `get_rate_limit_state` tool, same pattern as `get_capability_state`)
- Does not implement backoff/retry — existing `RouteHealth` failure tracking handles 429s
- Does not parse `retry-after` header — handled by existing error recovery

---

## Testing

| Test | Where | What it validates |
|------|-------|-------------------|
| `is_near_limit()` — boundary conditions at 0%, 9.9%, 10%, 10.1%, 100% | `rate_limit.rs` | Threshold math correct |
| `is_near_limit()` — returns false when headers absent (None values) | `rate_limit.rs` | Optimistic default |
| `near_limit_notified` preserved across state overwrite when still near limit | `anthropic.rs` | Flag not silently reset |
| `near_limit_notified` reset to false when capacity recovers | `anthropic.rs` | Recovery clears flag |
| `parse_rate_limit_headers()` — valid headers | `anthropic.rs` | Header parsing correct |
| `parse_rate_limit_headers()` — missing headers | `anthropic.rs` | Graceful degradation |
| `parse_rate_limit_headers()` — malformed values | `anthropic.rs` | No panic on bad input |
| SmartRouter routes to fallback when provider near limit | `smart_router.rs` | Avoidance works |
| SmartRouter does not fire Signal when `near_limit_notified = true` | `smart_router.rs` | No duplicate Signal on same window |
| `RATE_LIMIT_NEAR_THRESHOLD` same value used in both call sites | `rate_limit.rs` | Single constant, no drift |

---

## Layer Compliance

| Layer | Component | LLM used? |
|-------|-----------|-----------|
| Layer 1 (State) | `RateLimitState` updated by `AnthropicEngine` after each call | No |
| Layer 2 (Delta) | `SmartRouter::route()` checks `is_near_limit()` | No |
| Layer 3 (Signal) | One `Signal` fired on threshold crossing | No (Signal notifies LLM; firing itself is token-free) |

---

## Files Changed

| File | Change |
|------|--------|
| `crates/animus-core/src/rate_limit.rs` | New — `RateLimitState` struct + `RATE_LIMIT_NEAR_THRESHOLD` constant |
| `crates/animus-core/src/lib.rs` | Add `pub mod rate_limit; pub use rate_limit::{RateLimitState, RATE_LIMIT_NEAR_THRESHOLD};` |
| `crates/animus-cortex/src/llm/anthropic.rs` | Add field, save headers before body, parse headers, update state with flag preservation, override trait method |
| `crates/animus-cortex/src/llm/mod.rs` | Add `rate_limit_state()` default method to `ReasoningEngine` trait |
| `crates/animus-cortex/src/smart_router.rs` | Add rate limit state map, fallback logic, Signal (read-only on flag) |
| `crates/animus-runtime/src/main.rs` | Register engine rate limit states with SmartRouter |

No `Cargo.toml` changes needed: `parking_lot` and `chrono` are already dependencies of `animus-cortex` and `animus-core` respectively.
