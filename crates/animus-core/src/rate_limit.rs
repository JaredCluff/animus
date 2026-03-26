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
    pub near_limit_notified: bool,
}

impl RateLimitState {
    /// Returns true if requests_remaining OR tokens_remaining is below threshold_pct
    /// of their respective limit. Returns false (optimistic) when values are unknown.
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
