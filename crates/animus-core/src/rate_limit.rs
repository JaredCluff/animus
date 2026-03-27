//! Rate limit state for Anthropic API responses.
//!
//! The Anthropic API returns `anthropic-ratelimit-*` headers on every response.
//! This module provides [`RateLimitState`] — a plain struct (no lock dependencies)
//! that holds the parsed values and the near-limit notification flag.
//!
//! ## Three-layer pattern
//!
//! - **Layer 1 (State):** `AnthropicEngine::reason()` writes parsed headers into a
//!   shared `Arc<parking_lot::RwLock<RateLimitState>>` after each successful API call.
//! - **Layer 2 (Delta):** `SmartRouter::select_for_class()` reads [`RateLimitState::is_near_limit`]
//!   and, on the first threshold crossing, atomically sets `near_limit_notified = true` ("arm").
//! - **Layer 3 (Signal):** One `Normal` Signal fires when the threshold is first crossed.
//!
//! ## `near_limit_notified` ownership
//!
//! Write authority is intentionally split by operation:
//!
//! - `SmartRouter` sets the flag `true` ("arm") when it fires the Signal.
//! - `AnthropicEngine::reason()` and `OpenAICompatEngine::reason()` reset the flag `false`
//!   ("reset") when capacity recovers, via [`RateLimitState::apply_update`].
//!
//! This ensures exactly one Signal per threshold crossing without a central coordinator.
//! Use [`RateLimitState::apply_update`] to apply a parsed update while preserving this flag
//! correctly — a plain `*state = parsed` would silently clear it.
//!
//! ## Lock policy
//!
//! This module does **not** import `parking_lot`. The `Arc<parking_lot::RwLock<RateLimitState>>`
//! wrapper is applied at the call site in `animus-cortex`, keeping `animus-core` lock-free.

use chrono::Utc;

/// The fraction of remaining capacity below which near-limit routing activates.
///
/// Uses a strict `<` comparison: exactly 10% remaining is **not** near-limit;
/// 9.9% is. Set to 10% to give SmartRouter time to route to a fallback before
/// a 429 occurs.
pub const RATE_LIMIT_NEAR_THRESHOLD: f32 = 0.10;

/// Parsed Anthropic API rate limit state from `anthropic-ratelimit-*` response headers.
///
/// Updated by `AnthropicEngine::reason()` after every successful API call.
/// Read by `SmartRouter::select_for_class()` to decide whether to route to a fallback.
/// Held behind `Arc<parking_lot::RwLock<RateLimitState>>` in `animus-cortex`.
#[derive(Debug, Clone, Default)]
pub struct RateLimitState {
    pub requests_limit: Option<u32>,
    pub requests_remaining: Option<u32>,
    pub requests_reset: Option<chrono::DateTime<Utc>>,
    pub tokens_limit: Option<u32>,
    pub tokens_remaining: Option<u32>,
    pub tokens_reset: Option<chrono::DateTime<Utc>>,
    /// When the state was last updated from response headers.
    ///
    /// Defaults to the Unix epoch (`DateTime<Utc>::default()`) before the first API call.
    pub last_updated: chrono::DateTime<Utc>,
    /// Whether the near-limit Signal has already fired for the current rate-limit window.
    ///
    /// Write authority is split by operation — each writer has exactly one direction:
    /// - `SmartRouter` sets this `true` ("arm") when it fires the Signal.
    /// - `AnthropicEngine::reason()` and `OpenAICompatEngine::reason()` reset this `false`
    ///   ("reset") when capacity recovers, via [`RateLimitState::apply_update`].
    ///
    /// When updating the whole struct from parsed response headers, use
    /// [`RateLimitState::apply_update`] rather than a plain `*state = parsed` — the plain
    /// replace silently clears this flag and causes SmartRouter to fire a duplicate Signal.
    pub near_limit_notified: bool,
}

impl RateLimitState {
    /// Returns `true` if `requests_remaining` OR `tokens_remaining` is strictly below
    /// `threshold_pct` of their respective limit.
    ///
    /// Returns `false` (optimistic) when either value is absent or when the limit is zero.
    /// Absent headers mean the provider did not report a constraint — prefer availability.
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

    /// Apply a freshly parsed update, preserving `near_limit_notified` correctly.
    ///
    /// `parse_rate_limit_headers()` always returns `near_limit_notified: false`. A plain
    /// `*state = parsed` would silently clear the flag and cause SmartRouter to fire a
    /// duplicate Signal on the next routing call. This method carries the flag over
    /// according to the recovery rule:
    ///
    /// - **Still near limit:** keep `current_notified` — suppresses duplicate Signal.
    /// - **Capacity recovered:** reset to `false` — next crossing fires a fresh Signal.
    ///
    /// Usage in `AnthropicEngine::reason()`:
    /// ```ignore
    /// let parsed = parse_rate_limit_headers(&headers);
    /// let mut state = self.rate_limit_state.write();
    /// *state = parsed.apply_update(state.near_limit_notified, RATE_LIMIT_NEAR_THRESHOLD);
    /// ```
    pub fn apply_update(self, current_notified: bool, threshold: f32) -> Self {
        let now_near = self.is_near_limit(threshold);
        Self {
            near_limit_notified: if now_near { current_notified } else { false },
            ..self
        }
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

    // --- is_near_limit tests ---

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
    fn near_limit_via_tokens_only() {
        let s = state_with_tokens(100_000, 5_000); // 5% tokens, no request data
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

    // --- apply_update tests ---

    #[test]
    fn apply_update_resets_flag_when_capacity_recovers() {
        // Was notified (flag=true) + recovery response (80% remaining) → flag resets to false
        // so the next threshold crossing fires a fresh Signal
        let parsed = state_with_requests(1000, 800);
        let result = parsed.apply_update(true, RATE_LIMIT_NEAR_THRESHOLD);
        assert!(!result.near_limit_notified, "must reset to false on recovery");
    }

    #[test]
    fn apply_update_preserves_flag_while_still_near() {
        // Was notified (flag=true) + still near limit (4%) → flag stays true
        // so SmartRouter does not fire a duplicate Signal
        let parsed = state_with_requests(1000, 40);
        let result = parsed.apply_update(true, RATE_LIMIT_NEAR_THRESHOLD);
        assert!(result.near_limit_notified, "must stay true while still near limit");
    }

    #[test]
    fn apply_update_keeps_false_on_first_crossing() {
        // current_notified=false + now near limit: flag must stay false.
        // apply_update does NOT arm the flag — SmartRouter arms it separately
        // when it fires the Signal. apply_update must not arm it prematurely.
        let parsed = state_with_requests(1000, 50); // 5% — near limit
        let result = parsed.apply_update(false, RATE_LIMIT_NEAR_THRESHOLD);
        assert!(!result.near_limit_notified, "flag must stay false until SmartRouter arms it");
    }

    #[test]
    fn apply_update_preserves_flag_when_tokens_near_requests_recovered() {
        // requests fine (80%), tokens still near (5%) — is_near_limit() uses OR,
        // so overall still near: flag must stay true, not incorrectly reset.
        let mut parsed = state_with_requests(1000, 800);
        parsed.tokens_limit = Some(100_000);
        parsed.tokens_remaining = Some(5_000); // 5% tokens
        let result = parsed.apply_update(true, RATE_LIMIT_NEAR_THRESHOLD);
        assert!(result.near_limit_notified, "flag must stay true while tokens are still near limit");
    }

    #[test]
    fn apply_update_false_stays_false_when_not_near() {
        // Was not notified + not near limit → still false
        let parsed = state_with_requests(1000, 500);
        let result = parsed.apply_update(false, RATE_LIMIT_NEAR_THRESHOLD);
        assert!(!result.near_limit_notified);
    }

    #[test]
    fn apply_update_preserves_all_other_fields() {
        // apply_update must not silently discard header values
        let parsed = RateLimitState {
            requests_limit: Some(1000),
            requests_remaining: Some(800),
            tokens_limit: Some(50_000),
            tokens_remaining: Some(45_000),
            ..Default::default()
        };
        let result = parsed.apply_update(false, RATE_LIMIT_NEAR_THRESHOLD);
        assert_eq!(result.requests_limit, Some(1000));
        assert_eq!(result.requests_remaining, Some(800));
        assert_eq!(result.tokens_limit, Some(50_000));
        assert_eq!(result.tokens_remaining, Some(45_000));
    }
}
