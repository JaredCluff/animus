# Cognitive Resource Management Design

**Date:** 2026-03-26
**Status:** Approved
**Crates affected:** `animus-core`, `animus-cortex`, `animus-runtime`
**New crates:** `animus-provider-hunter` (Python, in-process subprocess)

---

## Problem

Animus currently routes to models based only on task class and route health. It has no awareness of:

- **Cost** — Anthropic calls burn real money; unconstrained routing can exhaust a monthly budget silently
- **Speed** — some tasks need sub-second responses; others can wait for a 70B model
- **Trust** — free or cheap providers may be operated under foreign intelligence mandates; routing PII or credentials to them is a security violation
- **Supply** — the current provider set is static and fixed at startup; Animus cannot acquire new providers autonomously

This spec defines the **Cognitive Resource Management (CRM)** system: the complete stack from provider trust taxonomy through autonomous account registration.

---

## Design Goals

1. Track monthly API spend against a user-defined budget ($50/month default); adjust routing pressure as spend approaches limits
2. Enrich `ModelSpec` with cost, speed, quality, and trust metadata so the `SmartRouter` can make semantically informed routing decisions
3. Define a provider trust taxonomy that hard-prohibits adversarial jurisdictions (PRC, Russia) regardless of price or performance
4. Route based on content sensitivity — credentials and private keys must never leave the local network
5. Enable Animus to discover new free-tier API providers, evaluate their trust profile, register an account, verify email (and optionally SMS/CAPTCHA with human-in-the-loop), extract an API key, test it, and hot-reload it into the engine registry — entirely autonomously except for SMS verification codes and CAPTCHA challenges

---

## Architecture Overview

```
ModelSpec  ────────────────────────────────────────────────────
  + CostTier / SpeedTier / QualityTier / ProviderTrust            │
  ↓                                                               │
ContentSensitivityDetector  (Sensorium Layer 1)                  │
  + scans input for PII / credentials / financial data           │
  ↓                                                               │
BudgetState  (animus-core)                                        │
  + monthly spend tracking                                        │
  + routing pressure tier (Normal / Careful / Emergency)         │
  ↓                                                               │
SmartRouter  (routing constraints applied)                       │
  + filters ModelSpec by trust ≥ required for content            │
  + filters by budget pressure (emergency → free only)           │
  + selects best match by speed/quality within constraints       │

ProviderHunter (Python subprocess)                               │
  + discovers free-tier API providers from catalogs              │
  ↓                                                               │
TrustEvaluator                                                   │
  + applies jurisdiction / ownership rules                       │
  + assigns ProviderTrustProfile                                  │
  ↓                                                               │
AccountRegistrar (Playwright, humanized)                         │
  + fills Lessons Agent identity from .env                       │
  + IMAP poll for verification email                             │
  + SMS → Telegram loop to Jared                                 │
  + CAPTCHA → screenshot → Telegram loop to Jared               │
  ↓                                                               │
ProviderTester                                                   │
  + test prompt → latency + quality score                        │
  ↓                                                               │
register_provider tool → providers.json → hot-reload watcher    │
  + EngineRegistry adds engine at runtime                        │
  + ModelPlan rebuilt by LLM with new engine in pool             │
```

---

## Section 1: ModelSpec Metadata

### 1.1 New types — `animus-core/src/provider_meta.rs`

```rust
/// Relative cost tier of a model provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CostTier {
    Free,       // $0 — free tier with usage limits
    Cheap,      // < $0.50 / MTok input
    Moderate,   // $0.50–$5 / MTok input
    Expensive,  // > $5 / MTok input (Opus-class)
}

/// Relative response speed tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SpeedTier {
    Fast,    // < 500ms TTFT typical (small models, fast endpoints)
    Medium,  // 500ms–2s TTFT
    Slow,    // > 2s TTFT (large models, heavy thinking)
}

/// Relative quality tier for general reasoning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum QualityTier {
    High,    // Frontier models (Sonnet/Opus, Llama 3.1 70B+)
    Medium,  // Mid-tier (Sonnet Haiku-class, Llama 8B)
    Low,     // Triage/fast (summary only, no complex reasoning)
}

/// Ownership/jurisdiction risk classification for a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum OwnershipRisk {
    /// Clean — US/EU/AU/CA/UK; publicly listed or well-audited; no known state ties
    Clean,
    /// Minor concerns — indirect state relationships, minor red flags, or unknown jurisdiction
    Minor,
    /// Major concerns — state-adjacent funding, lax data policies, or ambiguous ownership
    Major,
    /// Prohibited — PRC/Russia jurisdiction or mandate compliance is legally required
    /// National Intelligence Law 2017 (PRC), SORM (Russia).
    /// No exceptions. Zero price, zero latency cannot override this.
    Prohibited,
}

/// Data retention policy quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataPolicy {
    NoRetention,    // Contractually confirmed zero retention
    ShortWindow,    // Retained < 30 days for safety/abuse monitoring
    Retained,       // Retained for model training or unspecified duration
    Unknown,        // Policy not found or ambiguous
}

/// Complete trust profile for a provider. Stored alongside ModelSpec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderTrustProfile {
    pub provider_id: String,
    pub display_name: String,
    pub hq_country: String,      // ISO 3166-1 alpha-2, e.g. "US", "CN", "DE"
    pub ownership_risk: OwnershipRisk,
    pub data_policy: DataPolicy,
    /// Effective trust level used for routing decisions.
    /// Derived: Prohibited → 0, Major → 1, Minor → 2, Clean → 3
    pub effective_trust: u8,
    pub notes: String,           // Human-readable justification
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
```

### 1.2 Updated `ModelSpec`

`ModelSpec` in `animus-cortex/src/model_plan.rs` gains four new optional fields. They are optional so that existing persisted plans load without schema migration — missing fields default to conservative assumptions.

```rust
pub struct ModelSpec {
    pub provider: String,
    pub model: String,
    pub think: ThinkLevel,
    // --- New CRM fields ---
    #[serde(default)]
    pub cost: Option<CostTier>,
    #[serde(default)]
    pub speed: Option<SpeedTier>,
    #[serde(default)]
    pub quality: Option<QualityTier>,
    /// Minimum effective_trust required to use this model.
    /// 0 = any (free providers), 3 = Clean providers only.
    #[serde(default)]
    pub trust_floor: u8,
}
```

### 1.3 Known provider metadata — `animus-core/src/provider_catalog.rs`

A static catalog of known providers, used during `SmartRouter` initialization and by `TrustEvaluator` as a seed:

```rust
pub fn known_providers() -> Vec<ProviderTrustProfile> {
    vec![
        ProviderTrustProfile {
            provider_id: "anthropic".to_string(),
            display_name: "Anthropic".to_string(),
            hq_country: "US".to_string(),
            ownership_risk: OwnershipRisk::Clean,
            data_policy: DataPolicy::NoRetention,
            effective_trust: 3,
            notes: "US-based, no training retention on API".to_string(),
        },
        ProviderTrustProfile {
            provider_id: "cerebras".to_string(),
            display_name: "Cerebras Systems".to_string(),
            hq_country: "US".to_string(),
            ownership_risk: OwnershipRisk::Clean,
            data_policy: DataPolicy::ShortWindow,
            effective_trust: 3,
            notes: "US hardware company, free tier for inference".to_string(),
        },
        ProviderTrustProfile {
            provider_id: "groq".to_string(),
            display_name: "Groq".to_string(),
            hq_country: "US".to_string(),
            ownership_risk: OwnershipRisk::Clean,
            data_policy: DataPolicy::ShortWindow,
            effective_trust: 3,
            notes: "US inference hardware company".to_string(),
        },
        // --- Prohibited examples (for TrustEvaluator training) ---
        // QwenAPI, DeepSeek API — PRC jurisdiction, National Intelligence Law 2017 applies
        // Ollama local running Qwen/DeepSeek weights = Clean (weights only, no API call)
        ProviderTrustProfile {
            provider_id: "qwen-api".to_string(),
            display_name: "Qwen API (Alibaba Cloud)".to_string(),
            hq_country: "CN".to_string(),
            ownership_risk: OwnershipRisk::Prohibited,
            data_policy: DataPolicy::Retained,
            effective_trust: 0,
            notes: "PRC National Intelligence Law 2017 — all requests may be subject to \
                    state intelligence access. Prohibited unconditionally.".to_string(),
        },
        ProviderTrustProfile {
            provider_id: "deepseek-api".to_string(),
            display_name: "DeepSeek API".to_string(),
            hq_country: "CN".to_string(),
            ownership_risk: OwnershipRisk::Prohibited,
            data_policy: DataPolicy::Retained,
            effective_trust: 0,
            notes: "PRC jurisdiction — same prohibition as Qwen API.".to_string(),
        },
    ]
}
```

**Local weights distinction:** Running Qwen or DeepSeek weights via Ollama on local hardware is fully acceptable — no API call crosses a jurisdiction boundary. The prohibition applies only to remote API endpoints operated by PRC-domiciled entities.

---

## Section 2: Content Sensitivity Detection

### 2.1 ContentSensitivity — `animus-core/src/content_sensitivity.rs`

```rust
/// Sensitivity level of an input payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ContentSensitivity {
    /// General knowledge, no personal information.
    Public,
    /// Internal operational data; no PII or secrets.
    Internal,
    /// Contains PII (names, emails, phone numbers, addresses).
    Sensitive,
    /// Financial data (card numbers, bank accounts, transaction IDs).
    Confidential,
    /// Credentials, private keys, tokens, passwords, API keys.
    /// Must never leave the local network. Local-only routing required.
    Critical,
}

/// Result of scanning an input for sensitive content.
#[derive(Debug, Clone)]
pub struct SensitivityScan {
    pub level: ContentSensitivity,
    /// Which patterns triggered the classification.
    pub triggers: Vec<String>,
    /// Minimum trust floor required given this sensitivity level.
    pub required_trust_floor: u8,
}

impl ContentSensitivity {
    /// Minimum ProviderTrustProfile::effective_trust required for this content.
    pub fn required_trust_floor(&self) -> u8 {
        match self {
            Self::Public => 0,
            Self::Internal => 1,
            Self::Sensitive => 2,
            Self::Confidential => 3,
            Self::Critical => 255, // Local only — no remote provider satisfies this
        }
    }
}
```

### 2.2 Detector — `animus-sensorium/src/sensitivity.rs`

A pattern-based scanner that runs at Layer 1 (no LLM). Checks input text against regexes for:

| Pattern set | Examples | Classification |
|---|---|---|
| Private keys | `-----BEGIN PRIVATE KEY-----`, `sk-ant-`, `csk-` | Critical |
| Passwords | context around `password=`, `passwd`, JSON `"password":` | Critical |
| API tokens | `Bearer `, `Authorization:`, env vars ending in `_KEY` | Critical |
| Credit card numbers | Luhn-valid 13–19 digit sequences | Confidential |
| Bank accounts / routing | ABA routing + account number patterns | Confidential |
| Social Security Numbers | `\d{3}-\d{2}-\d{4}` | Confidential |
| Email addresses | RFC 5322 email regex | Sensitive |
| Phone numbers | NANP + E.164 patterns | Sensitive |

The scanner is fast (regex-only, no LLM). A miss is acceptable — over-classification triggers local routing unnecessarily, but is safe. Under-classification (missing a credential) is the failure mode to avoid, so patterns err on the side of sensitivity.

**Critical content routing:** When `ContentSensitivity::Critical` is detected, the only valid routing target is an Ollama engine running on `localhost` or a private network address. The SmartRouter enforces this as a hard block — not a preference. If no local engine is available, the turn fails with an error rather than routing to any remote provider.

---

## Section 3: Budget Tracking

### 3.0 BudgetConfig — `animus-core/src/config.rs` (new sub-struct in `AnimusConfig`)

All budget values are configurable. Nothing is hardcoded. Config follows the same pattern as every other Animus subsystem: TOML file with env var overrides.

```rust
/// Budget configuration — lives inside AnimusConfig, persisted in config.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    /// Monthly spend ceiling in USD.
    /// Override: ANIMUS_BUDGET_MONTHLY_USD (e.g. "50.00")
    pub monthly_limit_usd: f32,
    /// Fraction of budget spent that triggers Careful pressure (0.0–1.0).
    /// Override: ANIMUS_BUDGET_CAREFUL_PCT (e.g. "0.60")
    pub careful_threshold: f32,
    /// Fraction of budget spent that triggers Emergency pressure (0.0–1.0).
    /// Override: ANIMUS_BUDGET_EMERGENCY_PCT (e.g. "0.85")
    pub emergency_threshold: f32,
    /// Whether to block routing entirely when budget is exceeded (vs warn-only).
    /// Override: ANIMUS_BUDGET_HARD_CAP=1
    pub hard_cap: bool,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            monthly_limit_usd: 50.0,
            careful_threshold: 0.60,
            emergency_threshold: 0.85,
            hard_cap: false, // warn-only by default
        }
    }
}
```

`BudgetConfig` is added as `pub budget: BudgetConfig` to `AnimusConfig` and to `AnimusConfig::default()`.

`apply_env_overrides()` gains:
```rust
if let Ok(v) = std::env::var("ANIMUS_BUDGET_MONTHLY_USD") {
    if let Ok(n) = v.parse::<f32>() { self.budget.monthly_limit_usd = n; }
}
if let Ok(v) = std::env::var("ANIMUS_BUDGET_CAREFUL_PCT") {
    if let Ok(n) = v.parse::<f32>() { self.budget.careful_threshold = n; }
}
if let Ok(v) = std::env::var("ANIMUS_BUDGET_EMERGENCY_PCT") {
    if let Ok(n) = v.parse::<f32>() { self.budget.emergency_threshold = n; }
}
if std::env::var("ANIMUS_BUDGET_HARD_CAP").as_deref() == Ok("1") {
    self.budget.hard_cap = true;
}
```

### 3.1 BudgetState — `animus-core/src/budget.rs`

`BudgetState` is runtime-only state (spend counters). It references `BudgetConfig` for threshold comparisons but does not own it — config is passed in at call time.

```rust
/// Runtime spend state — persisted to ~/.animus/budget_state.json after each record_spend().
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetState {
    /// Spend accumulated since reset_date.
    pub spent_this_month_usd: f32,
    /// First day of current billing month (UTC).
    pub reset_date: DateTime<Utc>,
    /// 7-day rolling average daily spend (USD/day).
    pub burn_rate_usd_per_day: f32,
    /// Daily spend samples for burn rate calculation (ring buffer, last 7 entries).
    pub daily_samples: VecDeque<(Date<Utc>, f32)>,
}

impl BudgetState {
    /// Routing pressure tier based on current spend and config thresholds.
    pub fn pressure(&self, config: &BudgetConfig) -> BudgetPressure {
        if config.monthly_limit_usd <= 0.0 { return BudgetPressure::Normal; }
        let pct = self.spent_this_month_usd / config.monthly_limit_usd;
        match pct {
            p if p < config.careful_threshold => BudgetPressure::Normal,
            p if p < config.emergency_threshold => BudgetPressure::Careful,
            p if p < 1.0 => BudgetPressure::Emergency,
            _ => BudgetPressure::Exceeded,
        }
    }

    /// Estimated days until budget exhausted at current burn rate.
    pub fn days_remaining(&self, config: &BudgetConfig) -> Option<f32> {
        if self.burn_rate_usd_per_day <= 0.0 { return None; }
        let remaining = config.monthly_limit_usd - self.spent_this_month_usd;
        Some(remaining / self.burn_rate_usd_per_day)
    }

    /// Record spend from an API call. Accepts the config for threshold comparison.
    pub fn record_spend(&mut self, usd: f32) {
        self.spent_this_month_usd += usd;
        // update daily sample for burn rate…
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetPressure {
    Normal,    // < careful_threshold — all engines available
    Careful,   // careful..emergency — prefer Free/Cheap; Expensive only for explicit need
    Emergency, // emergency..100% — Free tier only; Moderate/Expensive blocked
    Exceeded,  // > 100% — Free tier only; fire Urgent Signal; warn on every turn
}
```

### 3.2 Cost recording

`AnthropicEngine::reason()` and `OpenAICompatEngine::reason()` extract token counts from the response and multiply by per-model rates to produce a USD cost. This cost is sent to `BudgetState::record_spend()` via an `mpsc` channel (no blocking on the critical path).

Per-model rates are stored in a static lookup table in `provider_catalog.rs`. Unknown models default to `Moderate` tier with a conservative estimate.

### 3.3 Routing pressure integration in SmartRouter

```rust
fn filter_by_budget(spec: &ModelSpec, pressure: BudgetPressure) -> bool {
    let cost = spec.cost.unwrap_or(CostTier::Moderate);
    match pressure {
        BudgetPressure::Normal => true,
        BudgetPressure::Careful => cost <= CostTier::Moderate,
        BudgetPressure::Emergency | BudgetPressure::Exceeded => cost == CostTier::Free,
    }
}
```

When the primary model fails the budget filter, the router cascades through fallbacks exactly as it does for rate limit proximity. If no fallback passes the budget filter, the router uses the cheapest available engine in any task class.

A `Signal` fires at `BudgetPressure::Emergency` threshold crossing (one signal per crossing). A second `Signal` fires at `Exceeded` with `Urgent` priority.

### 3.4 Persistence

`BudgetState` is serialized to `~/.animus/budget_state.json` after every `record_spend()` call (atomic write via temp file rename, same pattern as `ModelPlan`). Loaded at startup. If file is missing, initializes with `spent_this_month_usd = 0` and `reset_date = start of current UTC month`.

Auto-reset: on startup, if `now >= reset_date + 1 month`, zero out `spent_this_month_usd` and advance `reset_date`.

---

## Section 4: Provider Trust Taxonomy

### 4.1 Trust rules applied in SmartRouter

Trust filtering runs after task classification, before model selection:

```rust
fn filter_by_trust(spec: &ModelSpec, required_floor: u8, registry: &ProviderTrustRegistry) -> bool {
    let profile = registry.get(&spec.provider);
    let effective = profile.map(|p| p.effective_trust).unwrap_or(0);
    effective >= required_floor
}
```

`required_floor` comes from the `SensitivityScan` result for the current input. The SmartRouter receives the scan result as a parameter from the reasoning thread before calling `route()`.

### 4.2 Hard prohibition enforcement

Providers with `OwnershipRisk::Prohibited` have `effective_trust = 0`. Since `required_trust_floor` for `Public` content is also 0, a prohibited provider would technically satisfy public-content routing — this is a known gap in the numeric scheme.

The hard prohibition is therefore enforced separately: the SmartRouter maintains a `prohibited_providers: HashSet<String>` populated at startup from any provider with `OwnershipRisk::Prohibited`. A provider in this set is **never selected**, regardless of content sensitivity or budget pressure. This is a compile-time-like enforcement path that does not depend on the trust floor arithmetic.

```rust
// In SmartRouter::select_for_class()
if self.prohibited_providers.contains(&spec.provider) {
    continue; // skip unconditionally
}
```

### 4.3 Trust evaluation heuristics

When `TrustEvaluator` encounters a new provider not in the static catalog, it evaluates:

1. **Domain TLD:** `.cn`, `.ru`, `.ir`, `.kp` → Prohibited
2. **Company name / WHOIS:** Alibaba, Baidu, ByteDance, Tencent, Yandex, VK → Prohibited
3. **Funding disclosures:** State-linked sovereign funds → Major or Prohibited
4. **Privacy policy:** "may share with government" or similar clause → Major
5. **Data retention:** "used to improve our models" without opt-out → Retained
6. **Unknown:** If none of the above, defaults to `Minor` + `Unknown` data policy + `effective_trust = 1`

The evaluator uses a combination of web scraping (home page, privacy policy, terms of service) and a structured LLM prompt to produce a `ProviderTrustProfile`. The LLM prompt includes the full trust taxonomy and the hard prohibition rules, ensuring the LLM outputs a valid classification.

---

## Section 5: Autonomous Provider Acquisition

### 5.1 Architecture

The acquisition pipeline runs as an async background task under `TaskManager`. It is triggered by:
- Animus detecting `BudgetPressure::Emergency` and wanting to find free alternatives
- A Telos goal that includes provider diversification
- Jared's explicit instruction (Telegram message or tool call)

```
ProviderHunter
  → discovers candidates from free-tier catalogs
  → outputs: Vec<ProviderCandidate { name, signup_url, api_docs_url, free_tier_desc }>

TrustEvaluator
  → for each candidate: assign ProviderTrustProfile
  → filter out Prohibited
  → output: Vec<TrustedCandidate>

AccountRegistrar (one per trusted candidate)
  → Playwright browser automation
  → fill Lessons Agent identity (from env)
  → handle CAPTCHA (Telegram loop)
  → handle email verification (IMAP)
  → handle SMS verification (Telegram loop)
  → extract API key

ProviderTester
  → fire test prompt: "Respond with the word 'pong' only."
  → measure latency, verify response
  → if pass: proceed

register_provider tool
  → append to providers.json
  → signal hot-reload watcher

EngineRegistry::add_provider()
  → construct engine
  → register with SmartRouter

SmartRouter → trigger plan rebuild
  → LLM reasons about new model pool
  → update_plan()
```

### 5.2 ProviderHunter

Implemented as a Python subprocess invoked by Animus via `shell_exec` or a dedicated tool. Scrapes well-known free-tier index pages:

- `openrouter.ai` (free models section)
- `together.ai` (free tier)
- `fireworks.ai` (free tier)
- LLM benchmarking blogs / r/LocalLLaMA for new free endpoints
- Provider API documentation pages for free tier confirmation

Outputs a JSON array of `ProviderCandidate` objects. Animus reads this via stdout.

```python
# animus-provider-hunter/hunter.py
import json, httpx
from bs4 import BeautifulSoup

def discover() -> list[dict]:
    candidates = []
    # OpenRouter free models
    resp = httpx.get("https://openrouter.ai/models?max_price=0")
    # ... parse and extract provider signup URLs
    return candidates

if __name__ == "__main__":
    print(json.dumps(discover()))
```

### 5.3 AccountRegistrar — Playwright Humanization

The registrar uses Playwright (Python) with a Chromium headless browser. All interactions are humanized to avoid Cloudflare and other bot detection systems.

#### Registration configuration — `RegistrationConfig` in `AnimusConfig`

All registration identity and timeout values are configurable. Added as `pub registration: RegistrationConfig` to `AnimusConfig`.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrationConfig {
    /// First name for provider signups. Override: ANIMUS_REG_FIRST_NAME
    pub first_name: String,
    /// Last name for provider signups. Override: ANIMUS_REG_LAST_NAME
    pub last_name: String,
    /// Date of birth in YYYY-MM-DD. Override: ANIMUS_REG_DOB
    pub dob: String,
    /// Primary phone for SMS verification. Override: ANIMUS_REG_PHONE_PRIMARY
    pub phone_primary: String,
    /// Fallback phone if primary fails. Override: ANIMUS_REG_PHONE_FALLBACK
    pub phone_fallback: String,
    /// Seconds to wait for a Telegram reply with an SMS code. Override: ANIMUS_REG_SMS_TIMEOUT_SECS
    pub sms_timeout_secs: u64,
    /// Seconds to wait for a Telegram reply with a CAPTCHA solution. Override: ANIMUS_REG_CAPTCHA_TIMEOUT_SECS
    pub captcha_timeout_secs: u64,
    /// Seconds to wait for a verification email to arrive. Override: ANIMUS_REG_EMAIL_TIMEOUT_SECS
    pub email_timeout_secs: u64,
}

impl Default for RegistrationConfig {
    fn default() -> Self {
        Self {
            first_name: String::new(),
            last_name: String::new(),
            dob: String::new(),
            phone_primary: String::new(),
            phone_fallback: String::new(),
            sms_timeout_secs: 300,     // 5 min
            captcha_timeout_secs: 300, // 5 min
            email_timeout_secs: 120,   // 2 min
        }
    }
}
```

The identity fields (`first_name`, `last_name`, `dob`, `phone_*`) are intentionally empty in the default — they must be set via `.env` or config file. If any are empty when registration is attempted, `AccountRegistrar` aborts with a clear error rather than submitting blank fields.

#### Humanization techniques

```python
import asyncio, random
from playwright.async_api import async_playwright

async def human_type(page, selector: str, text: str):
    """Type with per-character random delays like a real human."""
    await page.click(selector)
    for char in text:
        await page.keyboard.type(char)
        await asyncio.sleep(random.uniform(0.05, 0.18))  # 50–180ms per keystroke

async def human_move_click(page, selector: str):
    """Move mouse to element with a slight random offset before clicking."""
    box = await page.locator(selector).bounding_box()
    if box:
        x = box["x"] + box["width"] * random.uniform(0.3, 0.7)
        y = box["y"] + box["height"] * random.uniform(0.3, 0.7)
        await page.mouse.move(x + random.uniform(-3, 3), y + random.uniform(-3, 3))
        await asyncio.sleep(random.uniform(0.1, 0.3))
        await page.mouse.click(x, y)

BROWSER_LAUNCH_ARGS = {
    "headless": True,
    "args": [
        "--disable-blink-features=AutomationControlled",
        "--no-sandbox",
        "--disable-dev-shm-usage",
    ],
}

CONTEXT_ARGS = {
    "viewport": {"width": random.choice([1280, 1440, 1920]), "height": random.choice([800, 900, 1080])},
    "user_agent": random.choice([
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/121.0.0.0 Safari/537.36",
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36",
    ]),
    "locale": "en-US",
    "timezone_id": "America/Chicago",
}
```

Additional hardening:
- Patch `navigator.webdriver` to `false` via `page.add_init_script()`
- Set realistic Accept-Language and other navigator properties
- Introduce random pauses between form interactions (1–3 seconds between fields)
- Scroll the page before filling forms
- Don't fill all fields at the same speed — vary it

#### CAPTCHA handling loop

```python
async def handle_captcha(page, telegram_tool) -> bool:
    """Take a screenshot, send to Jared via Telegram, wait for solution."""
    screenshot_path = "/tmp/captcha_{}.png".format(int(time.time()))
    await page.screenshot(path=screenshot_path)

    # Send via Telegram MCP tool
    await telegram_tool.send_message(
        chat_id=JARED_CHAT_ID,
        text="I hit a CAPTCHA on a provider signup. What does it say / what should I enter?",
        files=[screenshot_path]
    )

    # Wait for reply (poll or websocket — implementation-defined)
    solution = await telegram_tool.wait_for_reply(timeout_seconds=300)
    return solution
```

#### Email verification loop

```python
import imaplib, email

async def wait_for_verification_email(subject_contains: str, timeout_seconds: int = 120) -> str:
    """Poll Gmail IMAP for a verification email. Returns the verification link or code."""
    deadline = time.time() + timeout_seconds
    while time.time() < deadline:
        mail = imaplib.IMAP4_SSL(IMAP_HOST, IMAP_PORT)
        mail.login(EMAIL_ADDRESS, EMAIL_PASSWORD)
        mail.select("INBOX")
        _, data = mail.search(None, f'(UNSEEN SUBJECT "{subject_contains}")')
        if data[0]:
            _, msg_data = mail.fetch(data[0].split()[-1], "(RFC822)")
            msg = email.message_from_bytes(msg_data[0][1])
            # Extract link or code from body
            body = extract_body(msg)
            link = extract_verification_link(body)
            mail.logout()
            return link
        mail.logout()
        await asyncio.sleep(5)
    raise TimeoutError(f"No verification email with '{subject_contains}' in {timeout_seconds}s")
```

#### SMS verification loop

When a provider requires phone number verification:

1. Registrar fills the primary phone number (`ANIMUS_REG_PHONE_PRIMARY`)
2. After submitting, registrar sends a Telegram message to Jared:
   > "I need the SMS verification code sent to 6158296667 for [provider name] signup."
3. Waits up to 5 minutes for Jared's reply
4. Enters the code into the verification field
5. If the code is rejected, waits another 30 seconds and asks again ("Was that the right code? The provider says it's invalid.")
6. If the primary number fails entirely, retries with the fallback number (`ANIMUS_REG_PHONE_FALLBACK`) and repeats the Telegram loop

### 5.4 providers.json — hot-reload format

New providers are appended to `~/.animus/providers.json`:

```json
[
  {
    "provider_id": "cerebras",
    "display_name": "Cerebras Systems",
    "base_url": "https://api.cerebras.ai/v1",
    "api_key_env": "CEREBRAS_API_KEY",
    "api_key": "csk-...",
    "models": [
      {
        "model_id": "llama3.1-8b",
        "cost_tier": "Free",
        "speed_tier": "Fast",
        "quality_tier": "Medium"
      },
      {
        "model_id": "qwen-3-235b-a22b-instruct-2507",
        "cost_tier": "Free",
        "speed_tier": "Medium",
        "quality_tier": "High"
      }
    ],
    "trust": {
      "hq_country": "US",
      "ownership_risk": "Clean",
      "data_policy": "ShortWindow",
      "effective_trust": 3
    },
    "registered_at": "2026-03-26T00:00:00Z",
    "registration_source": "autonomous"
  }
]
```

### 5.5 Hot-reload watcher

The existing watcher subsystem (`animus-cortex/src/watcher.rs`) gains a new watcher type: `ProvidersJsonWatcher`. It monitors `~/.animus/providers.json` using `inotify`/`kqueue` (via `notify` crate). On change:

1. Parse the new providers.json
2. Diff against known engines
3. For each new entry, construct an `OpenAICompatEngine` with the provided `base_url` and API key
4. Call `engine_registry.add_engine(role, engine)` (new method — see below)
5. Append the new models to `SmartRouter`'s available model pool
6. Trigger `ModelPlan` rebuild via the existing plan rebuild Signal

### 5.6 EngineRegistry hot-add

```rust
impl EngineRegistry {
    /// Add a new engine at runtime. Used for hot-reload of autonomously registered providers.
    pub fn add_engine(&mut self, role: CognitiveRole, engine: Box<dyn ReasoningEngine>) {
        // Insert; if a role already has an engine, add as fallback
        // (role→engine is currently 1:1 — this may evolve to role→Vec<engine>)
        self.engines.insert(role, engine);
    }
}
```

The `CognitiveRole` for a newly discovered provider is assigned by the plan rebuild LLM call — it places the model where it fits based on quality/speed/cost metadata.

### 5.7 `register_provider` tool

A new tool in `animus-cortex/src/tools/register_provider.rs` that Animus can call directly (not just from the autonomous pipeline):

```
Tool: register_provider
Arguments:
  - provider_id: String
  - display_name: String
  - base_url: String
  - api_key: String
  - models: Vec<{ model_id, cost_tier, speed_tier, quality_tier }>
  - trust_profile: ProviderTrustProfile
```

The tool validates the trust profile (rejects Prohibited), appends to providers.json, and signals the hot-reload watcher. Returns success/failure with a reason.

---

## Section 6: Cerebras Wire-Up (Immediate)

This is not future work — Cerebras is already registered and its API key is in `.env`. The models available on the free tier:

| Model | cost_tier | speed_tier | quality_tier | Use case |
|---|---|---|---|---|
| `llama3.1-8b` | Free | Fast | Low | Fast triage, Perception class |
| `qwen-3-235b-a22b-instruct-2507` | Free | Medium | High | Reasoning fallback, Analytical class |

Wire-up required in `main.rs`:

```rust
// Cerebras — OpenAI-compatible
let cerebras_fast = OpenAICompatEngine::new(
    std::env::var("CEREBRAS_API_KEY").ok().unwrap_or_default(),
    "llama3.1-8b".to_string(),
    "https://api.cerebras.ai/v1".to_string(),
    4096,
);
registry.set_engine(CognitiveRole::Perception, Box::new(cerebras_fast));

let cerebras_reasoning = OpenAICompatEngine::new(
    std::env::var("CEREBRAS_API_KEY").ok().unwrap_or_default(),
    "qwen-3-235b-a22b-instruct-2507".to_string(),
    "https://api.cerebras.ai/v1".to_string(),
    8192,
);
// used as fallback for Analytical class in model plan
```

`.env` addition:
```
CEREBRAS_API_KEY=csk-5wp4hfcwk23tyc9yctwmkwmtcckrmhcc92m6m5r4c2prhtn2
```

---

## What This Does NOT Do

- Does not implement per-request token counting (uses a response-field token count from the API response body, already present in most OpenAI-compatible responses)
- Does not handle multi-seat budgets or per-user spend tracking (Animus is single-user)
- Does not build a full browser automation framework — Playwright is used only for provider signups, not as a general computer control system (that is a separate capability)
- Does not include a UI for reviewing autonomously registered providers — Animus reports via Telegram; approval is implicit in Jared not revoking the key
- Does not handle TOTP/authenticator-app 2FA — this would require building a TOTP seed capture step which is out of scope; Animus falls back to "SMS verification needed" Telegram message
- Does not handle providers that require credit card for free tier (Animus has no payment credentials)

---

## Testing

| Test | Where | What it validates |
|------|-------|-------------------|
| `OwnershipRisk::Prohibited` → `effective_trust = 0` | `provider_meta.rs` | Trust math |
| `ContentSensitivity::Critical` → `required_trust_floor = 255` | `content_sensitivity.rs` | Critical floor |
| Prohibited provider never selected in routing | `smart_router.rs` | Hard prohibition |
| Budget `Careful` → blocks `Expensive` tier models | `smart_router.rs` | Budget filter |
| Budget `Emergency` → only `Free` tier passes | `smart_router.rs` | Emergency filter |
| Budget reset on month rollover | `budget.rs` | Auto-reset |
| `SensitivityDetector` matches API key pattern | `sensitivity.rs` | Critical detection |
| `SensitivityDetector` matches email address | `sensitivity.rs` | Sensitive detection |
| `SensitivityDetector` does not false-positive on code keywords | `sensitivity.rs` | Specificity |
| `providers.json` hot-reload adds engine to registry | `watchers/providers.rs` | Hot-reload path |
| `register_provider` tool rejects Prohibited provider | `tools/register_provider.rs` | Trust gate on tool |
| `ModelSpec` with missing CRM fields deserializes from old format | `model_plan.rs` | Schema migration |

---

## Files Changed

| File | Change |
|------|--------|
| `crates/animus-core/src/provider_meta.rs` | New — `CostTier`, `SpeedTier`, `QualityTier`, `OwnershipRisk`, `DataPolicy`, `ProviderTrustProfile` |
| `crates/animus-core/src/provider_catalog.rs` | New — static known-provider catalog |
| `crates/animus-core/src/content_sensitivity.rs` | New — `ContentSensitivity` enum, `SensitivityScan` |
| `crates/animus-core/src/budget.rs` | New — `BudgetState`, `BudgetPressure` |
| `crates/animus-core/src/lib.rs` | Export new modules |
| `crates/animus-core/src/config.rs` | Add `BudgetConfig`, `RegistrationConfig` to `AnimusConfig`; add env var overrides for all new fields |
| `crates/animus-sensorium/src/sensitivity.rs` | New — regex-based `SensitivityDetector` |
| `crates/animus-cortex/src/model_plan.rs` | Add CRM fields to `ModelSpec` |
| `crates/animus-cortex/src/smart_router.rs` | Add budget + trust + sensitivity filtering |
| `crates/animus-cortex/src/watchers/providers.rs` | New — `ProvidersJsonWatcher` |
| `crates/animus-cortex/src/tools/register_provider.rs` | New — `register_provider` tool |
| `crates/animus-cortex/src/tools/mod.rs` | Register new tool |
| `crates/animus-runtime/src/main.rs` | Wire Cerebras engines; initialize `BudgetState`; start providers watcher |
| `animus-provider-hunter/hunter.py` | New — Python provider discovery script |
| `animus-provider-hunter/registrar.py` | New — Playwright account registrar |
| `animus-provider-hunter/requirements.txt` | New — `playwright`, `httpx`, `beautifulsoup4` |
| `Dockerfile` | Add Chromium deps + `pip install playwright && playwright install chromium` |
| `.env` | Add `CEREBRAS_API_KEY` |
