use chrono::{Datelike, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Configuration-driven thresholds (lives in AnimusConfig, not here).
/// Passed into `BudgetState::pressure()` at call time so state is config-agnostic.
#[derive(Debug, Clone)]
pub struct BudgetThresholds {
    pub monthly_limit_usd: f32,
    pub careful_threshold: f32,
    pub emergency_threshold: f32,
}

impl Default for BudgetThresholds {
    fn default() -> Self {
        Self {
            monthly_limit_usd: 50.0,
            careful_threshold: 0.60,
            emergency_threshold: 0.85,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetPressure {
    Normal,
    Careful,
    Emergency,
    Exceeded,
}

/// Runtime spend state — serialized to `~/.animus/budget_state.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetState {
    pub spent_this_month_usd: f32,
    /// First day of the current billing month (UTC).
    pub reset_date: NaiveDate,
    /// 7-day rolling burn rate (USD/day). Recomputed from `daily_samples`.
    pub burn_rate_usd_per_day: f32,
    /// Last 7 days of spend samples: (date, usd_spent_that_day).
    pub daily_samples: VecDeque<(NaiveDate, f32)>,
}

impl Default for BudgetState {
    fn default() -> Self {
        let today = Utc::now().naive_utc().date();
        Self {
            spent_this_month_usd: 0.0,
            reset_date: NaiveDate::from_ymd_opt(today.year(), today.month(), 1)
                .unwrap_or(today),
            burn_rate_usd_per_day: 0.0,
            daily_samples: VecDeque::new(),
        }
    }
}

impl BudgetState {
    pub fn pressure(&self, t: &BudgetThresholds) -> BudgetPressure {
        if t.monthly_limit_usd <= 0.0 {
            return BudgetPressure::Normal;
        }
        let pct = self.spent_this_month_usd / t.monthly_limit_usd;
        if pct < t.careful_threshold {
            BudgetPressure::Normal
        } else if pct < t.emergency_threshold {
            BudgetPressure::Careful
        } else if pct < 1.0 {
            BudgetPressure::Emergency
        } else {
            BudgetPressure::Exceeded
        }
    }

    pub fn days_remaining(&self, t: &BudgetThresholds) -> Option<f32> {
        if self.burn_rate_usd_per_day <= 0.0 {
            return None;
        }
        let remaining = t.monthly_limit_usd - self.spent_this_month_usd;
        Some(remaining / self.burn_rate_usd_per_day)
    }

    pub fn record_spend(&mut self, usd: f32) {
        self.spent_this_month_usd += usd;
        let today = Utc::now().naive_utc().date();
        if let Some(last) = self.daily_samples.back_mut() {
            if last.0 == today {
                last.1 += usd;
            } else {
                self.daily_samples.push_back((today, usd));
            }
        } else {
            self.daily_samples.push_back((today, usd));
        }
        while self.daily_samples.len() > 7 {
            self.daily_samples.pop_front();
        }
        let total: f32 = self.daily_samples.iter().map(|(_, v)| v).sum();
        let days = self.daily_samples.len() as f32;
        self.burn_rate_usd_per_day = if days > 0.0 { total / days } else { 0.0 };
    }

    /// If the reset_date is in a past month, zero the spend and advance the reset date.
    pub fn maybe_reset(&mut self) {
        let today = Utc::now().naive_utc().date();
        if today.year() > self.reset_date.year()
            || (today.year() == self.reset_date.year() && today.month() > self.reset_date.month())
        {
            self.spent_this_month_usd = 0.0;
            self.daily_samples.clear();
            self.burn_rate_usd_per_day = 0.0;
            self.reset_date = NaiveDate::from_ymd_opt(today.year(), today.month(), 1)
                .unwrap_or(today);
        }
    }

    pub fn load(path: &std::path::Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &std::path::Path) -> crate::error::Result<()> {
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| crate::error::AnimusError::Storage(format!("budget serialize: {e}")))?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &json)
            .map_err(|e| crate::error::AnimusError::Storage(format!("budget write: {e}")))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| crate::error::AnimusError::Storage(format!("budget rename: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thresholds() -> BudgetThresholds {
        BudgetThresholds {
            monthly_limit_usd: 100.0,
            careful_threshold: 0.60,
            emergency_threshold: 0.85,
        }
    }

    #[test]
    fn pressure_normal_when_zero_limit() {
        let state = BudgetState::default();
        let t = BudgetThresholds { monthly_limit_usd: 0.0, ..thresholds() };
        assert_eq!(state.pressure(&t), BudgetPressure::Normal);
    }

    #[test]
    fn pressure_tiers() {
        let mut state = BudgetState::default();
        let t = thresholds();

        state.spent_this_month_usd = 50.0; // 50% — Normal
        assert_eq!(state.pressure(&t), BudgetPressure::Normal);

        state.spent_this_month_usd = 70.0; // 70% — Careful
        assert_eq!(state.pressure(&t), BudgetPressure::Careful);

        state.spent_this_month_usd = 90.0; // 90% — Emergency
        assert_eq!(state.pressure(&t), BudgetPressure::Emergency);

        state.spent_this_month_usd = 105.0; // >100% — Exceeded
        assert_eq!(state.pressure(&t), BudgetPressure::Exceeded);
    }

    #[test]
    fn record_spend_accumulates() {
        let mut state = BudgetState::default();
        state.record_spend(1.50);
        state.record_spend(0.25);
        assert!((state.spent_this_month_usd - 1.75).abs() < 0.001);
    }

    #[test]
    fn daily_samples_capped_at_seven() {
        let mut state = BudgetState::default();
        // Manually insert 8 samples with distinct dates
        for i in 0..8u32 {
            let d = NaiveDate::from_ymd_opt(2026, 1, i + 1).unwrap();
            state.daily_samples.push_back((d, 1.0));
            while state.daily_samples.len() > 7 {
                state.daily_samples.pop_front();
            }
        }
        assert_eq!(state.daily_samples.len(), 7);
    }
}
