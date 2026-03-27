// crates/animus-core/src/provider_meta.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CostTier {
    Free,
    Cheap,
    Moderate,
    Expensive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SpeedTier {
    Fast,
    Medium,
    Slow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum QualityTier {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum OwnershipRisk {
    Clean,
    Minor,
    Major,
    /// PRC/Russia jurisdiction — National Intelligence Law 2017 (PRC), SORM (Russia).
    /// No exceptions. Zero price cannot override this.
    Prohibited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataPolicy {
    NoRetention,
    ShortWindow,
    Retained,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderTrustProfile {
    pub provider_id: String,
    pub display_name: String,
    /// ISO 3166-1 alpha-2 country code.
    pub hq_country: String,
    pub ownership_risk: OwnershipRisk,
    pub data_policy: DataPolicy,
    /// 0–3 derived score. Prohibited→0, Major→1, Minor→1–2, Clean→2–3.
    pub effective_trust: u8,
    pub notes: String,
}

impl ProviderTrustProfile {
    pub fn compute_effective_trust(risk: OwnershipRisk, policy: DataPolicy) -> u8 {
        match risk {
            OwnershipRisk::Prohibited => 0,
            OwnershipRisk::Major => 1,
            OwnershipRisk::Minor => match policy {
                DataPolicy::NoRetention => 2,
                _ => 1,
            },
            OwnershipRisk::Clean => match policy {
                DataPolicy::Retained => 2,
                _ => 3,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prohibited_always_zero() {
        assert_eq!(ProviderTrustProfile::compute_effective_trust(
            OwnershipRisk::Prohibited, DataPolicy::NoRetention), 0);
        assert_eq!(ProviderTrustProfile::compute_effective_trust(
            OwnershipRisk::Prohibited, DataPolicy::Unknown), 0);
    }

    #[test]
    fn clean_no_retention_is_three() {
        assert_eq!(ProviderTrustProfile::compute_effective_trust(
            OwnershipRisk::Clean, DataPolicy::NoRetention), 3);
    }

    #[test]
    fn clean_retained_is_two() {
        assert_eq!(ProviderTrustProfile::compute_effective_trust(
            OwnershipRisk::Clean, DataPolicy::Retained), 2);
    }

    #[test]
    fn cost_tier_ord() {
        assert!(CostTier::Free < CostTier::Cheap);
        assert!(CostTier::Cheap < CostTier::Moderate);
        assert!(CostTier::Moderate < CostTier::Expensive);
    }
}
