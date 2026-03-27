// crates/animus-sensorium/src/sensitivity.rs
//! Pattern-based content sensitivity detector (Layer 1 — no LLM).
//!
//! Runs at zero LLM cost. Over-classification (false positive) is safe — it routes
//! to local-only unnecessarily. Under-classification (missing a credential) is the
//! failure mode to avoid, so patterns err on the side of sensitivity.

use animus_core::{ContentSensitivity, SensitivityScan};
use regex::Regex;
use std::sync::OnceLock;

struct Patterns {
    /// Critical — private keys, API tokens, passwords
    private_key: Regex,
    api_key_prefix: Regex,    // sk-ant-, csk-, sk-, Bearer token
    password_context: Regex,  // password=, "password":, passwd
    env_key_var: Regex,       // ENV vars ending in _KEY, _SECRET, _TOKEN, _PASSWORD

    /// Confidential — financial identifiers
    luhn_card: Regex,         // 13–19 digit sequences (rough Luhn candidate)
    ssn: Regex,               // 123-45-6789

    /// Sensitive — PII
    email: Regex,
    phone_nanp: Regex,        // (555) 867-5309, 555-867-5309, 5558675309
}

fn patterns() -> &'static Patterns {
    static ONCE: OnceLock<Patterns> = OnceLock::new();
    ONCE.get_or_init(|| Patterns {
        private_key:      Regex::new(r"-----BEGIN\s+(?:RSA\s+|EC\s+|OPENSSH\s+|)?PRIVATE KEY-----").unwrap(),
        api_key_prefix:   Regex::new(r"\b(?:sk-ant-api\d+-|csk-|sk-[A-Za-z0-9]{20,}|Bearer\s+[A-Za-z0-9\-._~+/]+=*)\b").unwrap(),
        password_context: Regex::new(r#"(?i)(?:password\s*=\s*\S+|"password"\s*:\s*"[^"]+"|passwd\s*=\s*\S+)"#).unwrap(),
        env_key_var:      Regex::new(r"[A-Z][A-Z0-9_]*(?:_KEY|_SECRET|_TOKEN|_PASSWORD)\s*=\s*\S+").unwrap(),
        luhn_card:        Regex::new(r"\b\d{4}[\s\-]?\d{4}[\s\-]?\d{4}[\s\-]?\d{1,7}\b").unwrap(),
        ssn:              Regex::new(r"\b\d{3}-\d{2}-\d{4}\b").unwrap(),
        email:            Regex::new(r"\b[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}\b").unwrap(),
        phone_nanp:       Regex::new(r"\b(?:\+1[\s\-]?)?\(?\d{3}\)?[\s\-]?\d{3}[\s\-]?\d{4}\b").unwrap(),
    })
}

/// Scan `text` for sensitive content. Returns the highest classification found.
pub fn scan(text: &str) -> SensitivityScan {
    let p = patterns();
    let mut level = ContentSensitivity::Public;
    let mut triggers: Vec<String> = Vec::new();

    // Critical patterns
    if p.private_key.is_match(text) {
        level = ContentSensitivity::Critical;
        triggers.push("private_key_header".to_string());
    }
    if p.api_key_prefix.is_match(text) {
        level = ContentSensitivity::Critical;
        triggers.push("api_key_prefix".to_string());
    }
    if p.password_context.is_match(text) {
        level = ContentSensitivity::Critical;
        triggers.push("password_context".to_string());
    }
    if p.env_key_var.is_match(text) {
        level = ContentSensitivity::Critical;
        triggers.push("env_key_assignment".to_string());
    }

    // Confidential patterns (only elevate if not already Critical)
    if level < ContentSensitivity::Confidential {
        if p.luhn_card.is_match(text) {
            level = ContentSensitivity::Confidential;
            triggers.push("card_number_pattern".to_string());
        }
        if p.ssn.is_match(text) {
            level = ContentSensitivity::Confidential;
            triggers.push("ssn_pattern".to_string());
        }
    }

    // Sensitive patterns (only elevate if still Public or Internal)
    if level < ContentSensitivity::Sensitive {
        if p.email.is_match(text) {
            level = ContentSensitivity::Sensitive;
            triggers.push("email_address".to_string());
        }
        if p.phone_nanp.is_match(text) {
            level = ContentSensitivity::Sensitive;
            triggers.push("phone_number".to_string());
        }
    }

    SensitivityScan {
        required_trust_floor: level.required_trust_floor(),
        level,
        triggers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_private_key_header() {
        let text = "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQ...";
        let scan = scan(text);
        assert_eq!(scan.level, ContentSensitivity::Critical);
        assert!(scan.triggers.contains(&"private_key_header".to_string()));
    }

    #[test]
    fn detects_anthropic_api_key() {
        let text = "my key is sk-ant-api03-abc123xyz";
        let scan = scan(text);
        assert_eq!(scan.level, ContentSensitivity::Critical);
    }

    #[test]
    fn detects_cerebras_key() {
        let text = "CEREBRAS_API_KEY=csk-5wp4hfcwk23tyc9yctwmkwmtcckrmhcc92m6m5r4c2prhtn2";
        let scan = scan(text);
        assert_eq!(scan.level, ContentSensitivity::Critical);
    }

    #[test]
    fn detects_password_context() {
        let text = r#"{"username": "foo", "password": "hunter2"}"#;
        let scan = scan(text);
        assert_eq!(scan.level, ContentSensitivity::Critical);
    }

    #[test]
    fn detects_ssn() {
        let text = "SSN: 123-45-6789";
        let scan = scan(text);
        assert_eq!(scan.level, ContentSensitivity::Confidential);
    }

    #[test]
    fn detects_email() {
        let text = "Contact me at jared@example.com for details.";
        let scan = scan(text);
        assert_eq!(scan.level, ContentSensitivity::Sensitive);
    }

    #[test]
    fn clean_text_is_public() {
        let text = "The capital of France is Paris.";
        let scan = scan(text);
        assert_eq!(scan.level, ContentSensitivity::Public);
        assert!(scan.triggers.is_empty());
    }

    #[test]
    fn critical_floor_is_255() {
        let text = "MY_SECRET_KEY=abc123";
        let scan = scan(text);
        assert_eq!(scan.required_trust_floor, 255);
    }

    #[test]
    fn code_without_secrets_is_not_critical() {
        // A code snippet that mentions "key" in a non-secret context
        let text = "fn get_key(map: &HashMap<String, String>, key: &str) -> Option<&String>";
        let scan = scan(text);
        // Should not trigger — no actual key assignment or PEM header
        assert!(scan.level < ContentSensitivity::Critical,
            "False positive: {scan:?}");
    }
}
