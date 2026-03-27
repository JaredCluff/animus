// crates/animus-core/src/content_sensitivity.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ContentSensitivity {
    Public,
    Internal,
    Sensitive,
    Confidential,
    /// Private keys, API tokens, passwords. Local-only — trust floor 255 (no remote provider).
    Critical,
}

impl ContentSensitivity {
    /// Minimum `ProviderTrustProfile::effective_trust` required for this content.
    /// Critical returns 255 — no remote provider can satisfy this (local-only routing).
    pub fn required_trust_floor(self) -> u8 {
        match self {
            Self::Public       => 0,
            Self::Internal     => 1,
            Self::Sensitive    => 2,
            Self::Confidential => 3,
            Self::Critical     => 255,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SensitivityScan {
    pub level: ContentSensitivity,
    pub triggers: Vec<String>,
    pub required_trust_floor: u8,
}

impl SensitivityScan {
    pub fn clean() -> Self {
        Self {
            level: ContentSensitivity::Public,
            triggers: Vec::new(),
            required_trust_floor: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn critical_floor_is_255() {
        assert_eq!(ContentSensitivity::Critical.required_trust_floor(), 255);
    }

    #[test]
    fn public_floor_is_zero() {
        assert_eq!(ContentSensitivity::Public.required_trust_floor(), 0);
    }

    #[test]
    fn ordering() {
        assert!(ContentSensitivity::Critical > ContentSensitivity::Public);
        assert!(ContentSensitivity::Confidential > ContentSensitivity::Sensitive);
    }
}
