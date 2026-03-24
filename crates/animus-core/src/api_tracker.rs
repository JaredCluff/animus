use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Tracks API call patterns for self-awareness.
/// The AILF can query this to understand its own behavior and detect loops.
pub struct ApiTracker {
    inner: Mutex<ApiTrackerInner>,
}

struct ApiTrackerInner {
    /// Timestamps of recent API calls (sliding window).
    call_timestamps: VecDeque<Instant>,
    /// Token counts for recent calls.
    token_counts: VecDeque<TokenRecord>,
    /// How long the sliding window is.
    window: Duration,
    /// Threshold for "high frequency" warning (calls per second).
    high_frequency_threshold: f64,
    /// Optional daily budget (total tokens).
    daily_budget: Option<u64>,
    /// Tokens used today (reset at midnight UTC).
    daily_tokens_used: u64,
    /// Last time the daily counter was reset.
    last_daily_reset: chrono::DateTime<chrono::Utc>,
    /// Whether the system is currently in a "cool down" (self-aware pause).
    in_cooldown: bool,
    /// When cooldown started.
    cooldown_start: Option<Instant>,
    /// Minimum cooldown duration.
    cooldown_duration: Duration,
}

#[derive(Debug, Clone)]
struct TokenRecord {
    timestamp: Instant,
    input_tokens: u64,
    output_tokens: u64,
}

/// Snapshot of the tracker's current state, for the AILF to reason about.
#[derive(Debug, Clone)]
pub struct ApiUsageSnapshot {
    /// Calls in the sliding window.
    pub calls_in_window: usize,
    /// Current rate (calls per second).
    pub calls_per_second: f64,
    /// Tokens used in the sliding window.
    pub tokens_in_window: u64,
    /// Daily tokens used.
    pub daily_tokens_used: u64,
    /// Daily budget, if set.
    pub daily_budget: Option<u64>,
    /// Whether the system is currently in cooldown.
    pub in_cooldown: bool,
    /// Whether the current rate is considered "high".
    pub is_high_frequency: bool,
    /// Seconds until daily budget is exhausted at current rate (if budget set).
    pub estimated_seconds_to_budget: Option<f64>,
}

impl ApiTracker {
    pub fn new(window: Duration, high_frequency_threshold: f64) -> Self {
        Self {
            inner: Mutex::new(ApiTrackerInner {
                call_timestamps: VecDeque::new(),
                token_counts: VecDeque::new(),
                window,
                high_frequency_threshold,
                daily_budget: None,
                daily_tokens_used: 0,
                last_daily_reset: chrono::Utc::now(),
                in_cooldown: false,
                cooldown_start: None,
                cooldown_duration: Duration::from_secs(10),
            }),
        }
    }

    /// Set a daily token budget. Pass None to remove the budget.
    pub fn set_daily_budget(&self, budget: Option<u64>) {
        let mut inner = self.inner.lock().unwrap();
        inner.daily_budget = budget;
    }

    /// Record an API call with token counts.
    pub fn record_call(&self, input_tokens: u64, output_tokens: u64) {
        let mut inner = self.inner.lock().unwrap();
        let now = Instant::now();

        inner.call_timestamps.push_back(now);
        inner.token_counts.push_back(TokenRecord {
            timestamp: now,
            input_tokens,
            output_tokens,
        });

        inner.daily_tokens_used += input_tokens + output_tokens;

        // Evict old entries
        inner.evict_old(now);
    }

    /// Record an API call without token counts (e.g., when not available).
    pub fn record_call_simple(&self) {
        self.record_call(0, 0);
    }

    /// Get a snapshot of current usage for the AILF to reason about.
    pub fn snapshot(&self) -> ApiUsageSnapshot {
        let mut inner = self.inner.lock().unwrap();
        let now = Instant::now();
        inner.evict_old(now);
        inner.check_daily_reset();

        let calls_in_window = inner.call_timestamps.len();
        let calls_per_second = calls_in_window as f64 / inner.window.as_secs_f64();

        let tokens_in_window: u64 = inner
            .token_counts
            .iter()
            .map(|r| r.input_tokens + r.output_tokens)
            .sum();

        let is_high_frequency = calls_per_second >= inner.high_frequency_threshold;

        // Check if cooldown has expired
        if inner.in_cooldown {
            if let Some(start) = inner.cooldown_start {
                if start.elapsed() >= inner.cooldown_duration {
                    inner.in_cooldown = false;
                    inner.cooldown_start = None;
                }
            }
        }

        let estimated_seconds_to_budget = inner.daily_budget.map(|budget| {
            if tokens_in_window == 0 {
                f64::INFINITY
            } else {
                let remaining = budget.saturating_sub(inner.daily_tokens_used);
                let tokens_per_second = tokens_in_window as f64 / inner.window.as_secs_f64();
                if tokens_per_second > 0.0 {
                    remaining as f64 / tokens_per_second
                } else {
                    f64::INFINITY
                }
            }
        });

        ApiUsageSnapshot {
            calls_in_window,
            calls_per_second,
            tokens_in_window,
            daily_tokens_used: inner.daily_tokens_used,
            daily_budget: inner.daily_budget,
            in_cooldown: inner.in_cooldown,
            is_high_frequency,
            estimated_seconds_to_budget,
        }
    }

    /// The AILF can call this when it detects it might be in a loop.
    /// Enters a cooldown period where `should_pause()` returns true.
    pub fn enter_cooldown(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.in_cooldown = true;
        inner.cooldown_start = Some(Instant::now());
    }

    /// Check if the system should pause (self-aware pause, not mechanical).
    pub fn should_pause(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        if !inner.in_cooldown {
            return false;
        }
        if let Some(start) = inner.cooldown_start {
            return start.elapsed() < inner.cooldown_duration;
        }
        false
    }

    /// Check if the daily budget is exhausted.
    pub fn is_budget_exhausted(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        inner
            .daily_budget
            .map_or(false, |budget| inner.daily_tokens_used >= budget)
    }

    /// Get the hard budget status: (used, budget). None if no budget set.
    pub fn budget_status(&self) -> Option<(u64, u64)> {
        let inner = self.inner.lock().unwrap();
        inner
            .daily_budget
            .map(|budget| (inner.daily_tokens_used, budget))
    }
}

impl ApiTrackerInner {
    fn evict_old(&mut self, now: Instant) {
        while let Some(front) = self.call_timestamps.front() {
            if now.duration_since(*front) > self.window {
                self.call_timestamps.pop_front();
            } else {
                break;
            }
        }
        while let Some(front) = self.token_counts.front() {
            if now.duration_since(front.timestamp) > self.window {
                self.token_counts.pop_front();
            } else {
                break;
            }
        }
    }

    fn check_daily_reset(&mut self) {
        let now = chrono::Utc::now();
        let last_reset_date = self.last_daily_reset.date_naive();
        let today = now.date_naive();
        if today > last_reset_date {
            self.daily_tokens_used = 0;
            self.last_daily_reset = now;
        }
    }
}

impl Default for ApiTracker {
    fn default() -> Self {
        // Default: 60-second window, 2 calls/sec threshold
        Self::new(Duration::from_secs(60), 2.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_tracking() {
        let tracker = ApiTracker::new(Duration::from_secs(10), 2.0);
        tracker.record_call(100, 200);
        tracker.record_call(150, 250);

        let snap = tracker.snapshot();
        assert_eq!(snap.calls_in_window, 2);
        assert_eq!(snap.tokens_in_window, 700);
        assert!(!snap.is_high_frequency);
    }

    #[test]
    fn test_high_frequency_detection() {
        let tracker = ApiTracker::new(Duration::from_secs(5), 2.0);
        // Record 15 calls quickly
        for _ in 0..15 {
            tracker.record_call_simple();
        }

        let snap = tracker.snapshot();
        assert_eq!(snap.calls_in_window, 15);
        assert!(snap.is_high_frequency); // 15 calls / 5 sec = 3/sec > 2.0
    }

    #[test]
    fn test_cooldown() {
        let tracker = ApiTracker::new(Duration::from_secs(10), 2.0);
        assert!(!tracker.should_pause());

        tracker.enter_cooldown();
        assert!(tracker.should_pause());

        let snap = tracker.snapshot();
        assert!(snap.in_cooldown);
    }

    #[test]
    fn test_daily_budget() {
        let tracker = ApiTracker::new(Duration::from_secs(10), 2.0);
        tracker.set_daily_budget(Some(1000));

        tracker.record_call(300, 200);
        tracker.record_call(200, 100);

        assert!(!tracker.is_budget_exhausted());
        let (used, budget) = tracker.budget_status().unwrap();
        assert_eq!(used, 800);
        assert_eq!(budget, 1000);

        tracker.record_call(100, 150);
        assert!(tracker.is_budget_exhausted());
    }

    #[test]
    fn test_window_eviction() {
        let tracker = ApiTracker::new(Duration::from_millis(100), 2.0);
        tracker.record_call(100, 200);
        assert_eq!(tracker.snapshot().calls_in_window, 1);

        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(tracker.snapshot().calls_in_window, 0);
    }
}
