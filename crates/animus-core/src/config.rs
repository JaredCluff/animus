use crate::error::{AnimusError, Result};
use crate::tier::TierConfig;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Top-level configuration for an Animus instance.
///
/// Load priority (highest to lowest):
///   1. Environment variable overrides (applied after file load)
///   2. Config file (TOML) at `data_dir/config.toml` or `ANIMUS_CONFIG`
///   3. Built-in defaults
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimusConfig {
    /// Directory where VectorFS stores data.
    pub data_dir: PathBuf,

    /// Embedding model configuration.
    pub embedding: EmbeddingConfig,

    /// VectorFS configuration.
    pub vectorfs: VectorFSConfig,

    /// Mnemos configuration.
    pub mnemos: MnemosConfig,

    /// Tier management configuration.
    pub tier: TierConfig,

    /// Cortex reasoning layer configuration.
    pub cortex: CortexConfig,

    /// Terminal interface configuration.
    pub interface: InterfaceConfig,

    /// Sensorium perception layer configuration.
    pub sensorium: SensoriumConfig,

    /// Federation layer configuration.
    pub federation: FederationConfig,

    /// Health endpoint configuration.
    pub health: HealthConfig,

    /// Communication channel configuration.
    pub channels: ChannelsConfig,

    /// Autonomy mode configuration.
    pub autonomy: AutonomyConfig,

    /// Automatic snapshot / memory protection configuration.
    pub snapshot: SnapshotConfig,

    /// Security and prompt injection protection configuration.
    pub security: SecurityConfig,

    /// Voice input/output configuration (STT + TTS).
    #[serde(default)]
    pub voice: VoiceConfig,

    /// Budget tracking and routing pressure configuration.
    #[serde(default)]
    pub budget: BudgetConfig,
    /// Autonomous provider registration identity and timeouts.
    #[serde(default)]
    pub registration: RegistrationConfig,
}

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

/// Configuration for the Telegram bot channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramChannelConfig {
    /// Whether the Telegram channel is enabled.
    pub enabled: bool,
    /// Bot token from @BotFather. Overridden by `ANIMUS_TELEGRAM_TOKEN` env var.
    #[serde(default)]
    pub bot_token: String,
    /// Long-poll timeout in seconds.
    pub poll_timeout_secs: u64,
    /// Download directory for received files/photos.
    pub download_dir: String,
}

impl Default for TelegramChannelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_token: String::new(),
            poll_timeout_secs: 30,
            download_dir: "/tmp/animus-downloads".to_string(),
        }
    }
}

/// Configuration for the HTTP API channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpApiChannelConfig {
    /// Whether the HTTP API channel is enabled (extends the health endpoint).
    pub enabled: bool,
}

impl Default for HttpApiChannelConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Configuration for the NATS channel adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NatsChannelConfig {
    /// Whether the NATS channel is enabled.
    pub enabled: bool,
    /// NATS server URL. Overridden by `ANIMUS_NATS_URL` env var.
    pub url: String,
    /// Subjects to subscribe to for inbound messages (e.g. ["animus.in.>"]).
    #[serde(default)]
    pub subjects: Vec<String>,
    /// Subject prefix for outbound replies (e.g. "animus.out").
    pub reply_prefix: String,
    /// Subject prefixes whose publishers are granted `is_trusted = true`.
    /// Only publishers on subjects matching these prefixes bypass heavy injection scanning.
    /// For proper security, configure NATS server auth; this is an additional layer.
    #[serde(default = "NatsChannelConfig::default_trusted_prefixes")]
    pub trusted_subject_prefixes: Vec<String>,
}

impl NatsChannelConfig {
    fn default_trusted_prefixes() -> Vec<String> {
        vec!["animus.in.".to_string()]
    }
}

impl Default for NatsChannelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: "nats://localhost:4222".to_string(),
            subjects: vec!["animus.in.>".to_string()],
            reply_prefix: "animus.out".to_string(),
            trusted_subject_prefixes: Self::default_trusted_prefixes(),
        }
    }
}

/// Top-level channels configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChannelsConfig {
    pub telegram: TelegramChannelConfig,
    pub http_api: HttpApiChannelConfig,
    pub nats: NatsChannelConfig,
    #[serde(default)]
    pub principals: Vec<PrincipalConfig>,
}

// ---------------------------------------------------------------------------
// Identity — Principals
// ---------------------------------------------------------------------------

/// Role of a known principal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalRole {
    /// Instance owner — highest trust.
    Owner,
    /// AI agent peer (e.g., Claude Code).
    AiAgent,
    /// Human peer.
    Peer,
    /// Internal system.
    System,
}

/// A known principal: a stable identity mapped from channel-specific IDs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrincipalConfig {
    /// Stable identifier (e.g., "jared", "claude-code").
    pub id: String,
    pub role: PrincipalRole,
    /// Channel binding keys in the form "channel_id:sender_id"
    /// (e.g., "telegram:8593276557", "terminal", "nats:animus.in.claude").
    pub channels: Vec<String>,
}

// ---------------------------------------------------------------------------
// Snapshot / Memory Protection
// ---------------------------------------------------------------------------

/// Configuration for automatic VectorFS snapshots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotConfig {
    /// Seconds between automatic background snapshots (0 = disabled).
    pub interval_secs: u64,
    /// Maximum number of snapshots to retain. Oldest are pruned when exceeded.
    pub max_snapshots: usize,
    /// Directory for snapshot storage.
    /// Empty string = default: sibling of data_dir named `{data_dir_name}-snapshots`.
    /// IMPORTANT: Should be outside data_dir so shell_exec protection covers both.
    pub snapshot_dir: String,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            interval_secs: 3600, // hourly
            max_snapshots: 24,
            snapshot_dir: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Budget
// ---------------------------------------------------------------------------

/// Budget and routing pressure configuration.
/// All thresholds are fractions of the monthly limit (0.0–1.0).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    /// Monthly spend ceiling in USD. Override: ANIMUS_BUDGET_MONTHLY_USD
    pub monthly_limit_usd: f32,
    /// Spend fraction that triggers Careful pressure. Override: ANIMUS_BUDGET_CAREFUL_PCT
    pub careful_threshold: f32,
    /// Spend fraction that triggers Emergency pressure. Override: ANIMUS_BUDGET_EMERGENCY_PCT
    pub emergency_threshold: f32,
    /// If true, block all non-Free routing when budget is exceeded. Override: ANIMUS_BUDGET_HARD_CAP=1
    pub hard_cap: bool,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            monthly_limit_usd: 50.0,
            careful_threshold: 0.60,
            emergency_threshold: 0.85,
            hard_cap: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Identity and timeout configuration for autonomous provider account registration.
/// Identity fields default to empty — must be set via env vars or config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrationConfig {
    /// Override: ANIMUS_REG_FIRST_NAME
    pub first_name: String,
    /// Override: ANIMUS_REG_LAST_NAME
    pub last_name: String,
    /// ISO date YYYY-MM-DD. Override: ANIMUS_REG_DOB
    pub dob: String,
    /// Primary phone (NANP, digits only). Override: ANIMUS_REG_PHONE_PRIMARY
    pub phone_primary: String,
    /// Fallback phone. Override: ANIMUS_REG_PHONE_FALLBACK
    pub phone_fallback: String,
    /// Seconds to wait for SMS code via Telegram. Override: ANIMUS_REG_SMS_TIMEOUT_SECS
    pub sms_timeout_secs: u64,
    /// Seconds to wait for CAPTCHA solution via Telegram. Override: ANIMUS_REG_CAPTCHA_TIMEOUT_SECS
    pub captcha_timeout_secs: u64,
    /// Seconds to wait for verification email. Override: ANIMUS_REG_EMAIL_TIMEOUT_SECS
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
            sms_timeout_secs: 300,
            captcha_timeout_secs: 300,
            email_timeout_secs: 120,
        }
    }
}

// ---------------------------------------------------------------------------
// Autonomy
// ---------------------------------------------------------------------------

/// Runtime-configurable autonomy mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyMode {
    /// Only acts when messaged. No background actions.
    Reactive,
    /// Has standing goals, acts on them independently. Responds to messages.
    GoalDirected,
    /// Acts on own judgment 24/7 within configured permissions.
    Full,
}

impl Default for AutonomyMode {
    fn default() -> Self {
        AutonomyMode::Reactive
    }
}

impl std::fmt::Display for AutonomyMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AutonomyMode::Reactive => write!(f, "reactive"),
            AutonomyMode::GoalDirected => write!(f, "goal_directed"),
            AutonomyMode::Full => write!(f, "full"),
        }
    }
}

impl std::str::FromStr for AutonomyMode {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s.to_lowercase().as_str() {
            "reactive" | "a" => Ok(AutonomyMode::Reactive),
            "goal_directed" | "goal-directed" | "b" => Ok(AutonomyMode::GoalDirected),
            "full" | "c" => Ok(AutonomyMode::Full),
            other => Err(format!("unknown autonomy mode: {other}")),
        }
    }
}

/// Autonomy configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutonomyConfig {
    /// Default autonomy mode at boot.
    pub default_mode: AutonomyMode,
}

impl Default for AutonomyConfig {
    fn default() -> Self {
        Self { default_mode: AutonomyMode::Reactive }
    }
}

// ---------------------------------------------------------------------------
// Security / Prompt Injection Protection
// ---------------------------------------------------------------------------

/// Configuration for prompt injection protection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Whether injection scanning is enabled.
    pub injection_scanning_enabled: bool,
    /// Confidence threshold above which content is quarantined (0.0–1.0).
    pub injection_threshold: f32,
    /// Trusted Telegram user IDs (bypass heavy scanning).
    pub trusted_telegram_ids: Vec<i64>,
    /// Trusted email addresses (bypass heavy scanning).
    pub trusted_email_addresses: Vec<String>,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            injection_scanning_enabled: true,
            injection_threshold: 0.7,
            trusted_telegram_ids: Vec::new(),
            trusted_email_addresses: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Embedding
// ---------------------------------------------------------------------------

/// Which embedding provider to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmbeddingProviderKind {
    /// Ollama HTTP API — local or remote. Default.
    Ollama,
    /// OpenAI Embeddings API (requires `OPENAI_API_KEY` env var).
    OpenAI,
    /// Synthetic hash-based embeddings — deterministic, no network, for testing.
    Synthetic,
}

impl Default for EmbeddingProviderKind {
    fn default() -> Self {
        EmbeddingProviderKind::Ollama
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// Which provider to use.
    pub provider: EmbeddingProviderKind,

    /// Base URL of the Ollama server (used when `provider = "ollama"`).
    /// Supports remote servers — not limited to localhost.
    pub ollama_url: String,

    /// Embedding model name.
    /// Ollama default: "mxbai-embed-large".
    /// OpenAI default: "text-embedding-3-small".
    pub model: String,

    /// Expected vector dimensionality.
    /// Set to `0` to auto-detect (Ollama only). For OpenAI text-embedding-3-small: 1536.
    /// For Synthetic: used as-is (default 128).
    pub dimensionality: usize,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            provider: EmbeddingProviderKind::Ollama,
            ollama_url: "http://localhost:11434".to_string(),
            model: "mxbai-embed-large".to_string(),
            dimensionality: 0, // auto-detect for Ollama
        }
    }
}

// ---------------------------------------------------------------------------
// VectorFS
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorFSConfig {
    /// Vector dimensionality (must match embedding model).
    pub dimensionality: usize,
    /// Maximum number of segments (hint for HNSW pre-allocation).
    pub max_segments: usize,
    #[serde(default)]
    pub quality_gate: QualityGateConfig,
}

impl Default for VectorFSConfig {
    fn default() -> Self {
        Self {
            dimensionality: 1024,
            max_segments: 100_000,
            quality_gate: QualityGateConfig::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Memory Quality Gate
// ---------------------------------------------------------------------------

/// Configures the write-time memory quality filter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityGateConfig {
    /// Enable/disable the quality gate entirely.
    pub enabled: bool,
    /// Cosine similarity threshold above which a write is considered a duplicate (0.0–1.0).
    pub dedup_similarity_threshold: f32,
    /// Window in hours within which dedup is checked.
    pub dedup_window_hours: u64,
    /// Cooldown in minutes for null-state segments (silence, keepalive failures).
    pub null_state_cooldown_minutes: u64,
}

impl Default for QualityGateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dedup_similarity_threshold: 0.92,
            dedup_window_hours: 24,
            null_state_cooldown_minutes: 60,
        }
    }
}

// ---------------------------------------------------------------------------
// Mnemos
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MnemosConfig {
    /// Maximum token budget for context assembly.
    pub context_token_budget: usize,
    /// Number of segments to retrieve per query.
    pub retrieval_top_k: usize,
    /// Cosine similarity threshold for consolidation.
    pub consolidation_similarity_threshold: f32,
}

impl Default for MnemosConfig {
    fn default() -> Self {
        Self {
            context_token_budget: 100_000,
            retrieval_top_k: 20,
            consolidation_similarity_threshold: 0.95,
        }
    }
}

// ---------------------------------------------------------------------------
// Cortex / LLM
// ---------------------------------------------------------------------------

/// Configuration for the Cortex reasoning layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CortexConfig {
    /// LLM provider name: "anthropic" | "ollama" | "openai" | "mock".
    pub llm_provider: String,
    /// Model identifier.
    /// With Claude Max OAuth: use "claude-haiku-4-5-20251001".
    /// With ANTHROPIC_API_KEY: any model works (e.g., "claude-sonnet-4-20250514").
    /// For Ollama: use the model tag, e.g. "llama3.1:8b".
    pub model_id: String,
    /// API key for the LLM provider. Always read from env at runtime; never serialized.
    #[serde(skip)]
    pub api_key: Option<String>,
    /// Maximum tokens for LLM response.
    pub max_response_tokens: usize,
    /// System prompt prepended to every reasoning call.
    pub system_prompt: String,
    /// Base URL for OpenAI-compatible endpoint (ollama or openai provider).
    /// Ollama default: "http://127.0.0.1:11434"
    /// OpenAI default: "https://api.openai.com"
    pub openai_base_url: String,

    // ── Safety-net / fallback engine ─────────────────────────────────────
    // The always-available engine that the system falls back to when the
    // primary provider (and all cloud alternatives) are down. Can be any
    // OpenAI-compatible server: local Ollama, LM Studio, vLLM, cloud
    // Ollama, text-generation-inference, etc.

    /// Base URL of the safety-net endpoint.
    /// Env: `ANIMUS_FALLBACK_URL`. Falls back to `ANIMUS_OLLAMA_URL`.
    #[serde(default = "default_fallback_url")]
    pub fallback_url: String,
    /// Provider type for the safety-net endpoint ("ollama" | "openai_compat").
    /// Env: `ANIMUS_FALLBACK_PROVIDER`.
    #[serde(default = "default_fallback_provider")]
    pub fallback_provider: String,
    /// Explicit model override for the safety net. If empty, auto-discovered
    /// from the endpoint at boot.
    /// Env: `ANIMUS_FALLBACK_MODEL`.
    #[serde(default)]
    pub fallback_model: String,
}

fn default_fallback_url() -> String { "http://localhost:11434".to_string() }
fn default_fallback_provider() -> String { "ollama".to_string() }

impl Default for CortexConfig {
    fn default() -> Self {
        Self {
            llm_provider: "anthropic".to_string(),
            model_id: "claude-haiku-4-5-20251001".to_string(),
            api_key: None,
            max_response_tokens: 4096,
            system_prompt: String::new(),
            openai_base_url: "http://127.0.0.1:11434".to_string(),
            fallback_url: default_fallback_url(),
            fallback_provider: default_fallback_provider(),
            fallback_model: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Health endpoint
// ---------------------------------------------------------------------------

/// HTTP health endpoint configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthConfig {
    /// Whether to expose the health endpoint.
    pub enabled: bool,
    /// Address to bind (e.g., "0.0.0.0:8080"). Used when `enabled = true`.
    pub bind: String,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bind: "0.0.0.0:8080".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Sensorium
// ---------------------------------------------------------------------------

/// Configuration for the Sensorium perception layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensoriumConfig {
    pub watch_paths: Vec<PathBuf>,
    pub process_poll_interval_secs: u64,
    pub file_watching_enabled: bool,
    pub process_monitoring_enabled: bool,
    /// Cosine similarity threshold for Tier 2 attention filtering.
    /// Events whose embedding similarity to all active goal embeddings falls
    /// below this value are silently dropped (logged to Cold only).
    /// Range: 0.0–1.0. Default: 0.25.
    /// Override: `ANIMUS_SENSORIUM_ATTENTION_THRESHOLD` env var.
    pub attention_threshold: f32,
}

impl Default for SensoriumConfig {
    fn default() -> Self {
        Self {
            watch_paths: Vec::new(),
            process_poll_interval_secs: 5,
            file_watching_enabled: false,
            process_monitoring_enabled: false,
            attention_threshold: 0.25,
        }
    }
}

// ---------------------------------------------------------------------------
// Interface
// ---------------------------------------------------------------------------

/// Configuration for the terminal interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterfaceConfig {
    /// Prompt string shown to the user.
    pub prompt: String,
    /// Whether to display system status on startup.
    pub show_status_on_start: bool,
}

impl Default for InterfaceConfig {
    fn default() -> Self {
        Self {
            prompt: ">> ".to_string(),
            show_status_on_start: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Voice
// ---------------------------------------------------------------------------

/// Configuration for voice input/output (STT and TTS).
///
/// STT: delegates to the `macos-stt` HTTP service (SFSpeechRecognizer on macOS).
/// TTS: Cartesia neural TTS.
///
/// Neither API key is ever serialized to disk — both come from env vars only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceConfig {
    /// Enable voice input (speech-to-text transcription of Telegram voice messages).
    pub enabled: bool,

    /// Enable voice replies (text-to-speech via Cartesia).
    pub tts_enabled: bool,

    // ── STT (macos-stt service) ──────────────────────────────────────────────

    /// Base URL of the macos-stt service (e.g. "http://127.0.0.1:7600").
    pub stt_url: String,

    /// Bearer key for the macos-stt service. Set via `ANIMUS_STT_KEY`; never serialized.
    #[serde(skip)]
    pub stt_key: String,

    // ── TTS (Cartesia) ───────────────────────────────────────────────────────

    /// Cartesia voice UUID.
    pub cartesia_voice_id: String,

    /// Cartesia model ID (default: "sonic-2").
    pub cartesia_model: String,

    /// Cartesia API key. Set via `ANIMUS_CARTESIA_KEY`; never serialized.
    #[serde(skip)]
    pub cartesia_api_key: String,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tts_enabled: false,
            stt_url: "http://127.0.0.1:7600".to_string(),
            stt_key: String::new(),
            cartesia_voice_id: String::new(),
            cartesia_model: "sonic-2".to_string(),
            cartesia_api_key: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Federation
// ---------------------------------------------------------------------------

/// Configuration for the Federation layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationConfig {
    pub enabled: bool,
    pub bind_address: String,
    pub port: u16,
    pub static_peers: Vec<String>,
    pub relevance_threshold: f32,
    pub federated_confidence_trusted: f32,
    pub federated_confidence_verified: f32,
    pub max_requests_per_minute: u32,
}

impl Default for FederationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_address: "127.0.0.1".to_string(),
            port: 0,
            static_peers: Vec::new(),
            relevance_threshold: 0.5,
            federated_confidence_trusted: 0.3,
            federated_confidence_verified: 0.1,
            max_requests_per_minute: 100,
        }
    }
}

// ---------------------------------------------------------------------------
// AnimusConfig impl
// ---------------------------------------------------------------------------

impl Default for AnimusConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("./animus-data"),
            embedding: EmbeddingConfig::default(),
            vectorfs: VectorFSConfig::default(),
            mnemos: MnemosConfig::default(),
            tier: TierConfig::default(),
            cortex: CortexConfig::default(),
            interface: InterfaceConfig::default(),
            sensorium: SensoriumConfig::default(),
            federation: FederationConfig::default(),
            health: HealthConfig::default(),
            channels: ChannelsConfig::default(),
            autonomy: AutonomyConfig::default(),
            security: SecurityConfig::default(),
            snapshot: SnapshotConfig::default(),
            voice: VoiceConfig::default(),
            budget: BudgetConfig::default(),
            registration: RegistrationConfig::default(),
        }
    }
}

impl AnimusConfig {
    /// Load config from a TOML file.
    ///
    /// Returns `Ok(default)` if the file does not exist; returns an error
    /// only if the file exists but cannot be parsed.
    pub fn from_toml(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path).map_err(|e| {
            AnimusError::Storage(format!("failed to read config {}: {e}", path.display()))
        })?;
        toml::from_str(&text).map_err(|e| {
            AnimusError::Storage(format!(
                "failed to parse config {}: {e}",
                path.display()
            ))
        })
    }

    /// Serialize and write this config to a TOML file atomically.
    pub fn save_toml(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).map_err(|e| {
            AnimusError::Storage(format!("failed to serialize config: {e}"))
        })?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AnimusError::Storage(format!("failed to create config dir: {e}"))
            })?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &text).map_err(|e| {
            AnimusError::Storage(format!("failed to write config: {e}"))
        })?;
        std::fs::rename(&tmp, path).map_err(|e| {
            AnimusError::Storage(format!("failed to rename config: {e}"))
        })?;
        Ok(())
    }

    /// Apply environment variable overrides on top of the loaded config.
    ///
    /// Environment variables take precedence over file values so that
    /// container deployments can inject settings without modifying config files.
    pub fn apply_env_overrides(&mut self) {
        // Embedding overrides
        if let Ok(url) = std::env::var("ANIMUS_OLLAMA_URL") {
            self.embedding.ollama_url = url;
        }
        if let Ok(model) = std::env::var("ANIMUS_EMBED_MODEL") {
            self.embedding.model = model;
        }
        if let Ok(provider) = std::env::var("ANIMUS_EMBED_PROVIDER") {
            match provider.to_lowercase().as_str() {
                "ollama" => self.embedding.provider = EmbeddingProviderKind::Ollama,
                "openai" => self.embedding.provider = EmbeddingProviderKind::OpenAI,
                "synthetic" => self.embedding.provider = EmbeddingProviderKind::Synthetic,
                other => eprintln!("Warning: unknown ANIMUS_EMBED_PROVIDER value: {other}"),
            }
        }

        // LLM overrides
        if let Ok(model) = std::env::var("ANIMUS_MODEL") {
            self.cortex.model_id = model;
        }
        if let Ok(provider) = std::env::var("ANIMUS_LLM_PROVIDER") {
            self.cortex.llm_provider = provider;
        }
        if let Ok(url) = std::env::var("ANIMUS_OPENAI_BASE_URL") {
            self.cortex.openai_base_url = url;
        } else if let Ok(url) = std::env::var("ANIMUS_OLLAMA_URL") {
            // ANIMUS_OLLAMA_URL doubles as the base URL for both embeddings and LLM.
            self.cortex.openai_base_url = url;
        }

        // Safety-net / fallback overrides
        if let Ok(url) = std::env::var("ANIMUS_FALLBACK_URL") {
            self.cortex.fallback_url = url;
        } else if self.cortex.fallback_url == default_fallback_url() {
            // If no explicit fallback URL, inherit from ANIMUS_OLLAMA_URL for backwards compat
            if let Ok(url) = std::env::var("ANIMUS_OLLAMA_URL") {
                self.cortex.fallback_url = url;
            }
        }
        if let Ok(provider) = std::env::var("ANIMUS_FALLBACK_PROVIDER") {
            self.cortex.fallback_provider = provider;
        }
        if let Ok(model) = std::env::var("ANIMUS_FALLBACK_MODEL") {
            self.cortex.fallback_model = model;
        }

        // Health overrides
        if let Ok(bind) = std::env::var("ANIMUS_HEALTH_BIND") {
            self.health.bind = bind;
            self.health.enabled = true;
        }
        if std::env::var("ANIMUS_HEALTH_DISABLED").is_ok() {
            self.health.enabled = false;
        }

        // Federation overrides
        if std::env::var("ANIMUS_FEDERATION").as_deref() == Ok("1") {
            self.federation.enabled = true;
        }

        // Channel overrides
        if let Ok(token) = std::env::var("ANIMUS_TELEGRAM_TOKEN") {
            if !token.is_empty() {
                self.channels.telegram.bot_token = token;
                self.channels.telegram.enabled = true;
            }
        }
        if std::env::var("ANIMUS_TELEGRAM_DISABLED").is_ok() {
            self.channels.telegram.enabled = false;
        }

        // NATS channel overrides
        if let Ok(url) = std::env::var("ANIMUS_NATS_URL") {
            if !url.is_empty() {
                self.channels.nats.url = url;
                self.channels.nats.enabled = true;
            }
        }
        if std::env::var("ANIMUS_NATS_DISABLED").is_ok() {
            self.channels.nats.enabled = false;
        }
        // Append extra subjects (comma-separated) without replacing defaults.
        // e.g. ANIMUS_NATS_EXTRA_SUBJECTS=claude.*.out.>
        if let Ok(extra) = std::env::var("ANIMUS_NATS_EXTRA_SUBJECTS") {
            for s in extra.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                if !self.channels.nats.subjects.contains(&s.to_string()) {
                    self.channels.nats.subjects.push(s.to_string());
                }
            }
        }

        // Autonomy mode override
        if let Ok(mode) = std::env::var("ANIMUS_AUTONOMY_MODE") {
            match mode.parse::<AutonomyMode>() {
                Ok(m) => self.autonomy.default_mode = m,
                Err(e) => eprintln!("Warning: invalid ANIMUS_AUTONOMY_MODE value: {e}"),
            }
        }

        // Snapshot overrides
        if let Ok(dir) = std::env::var("ANIMUS_SNAPSHOT_DIR") {
            if !dir.is_empty() {
                self.snapshot.snapshot_dir = dir;
            }
        }
        if let Ok(interval) = std::env::var("ANIMUS_SNAPSHOT_INTERVAL") {
            if let Ok(v) = interval.parse::<u64>() {
                self.snapshot.interval_secs = v;
            }
        }
        if std::env::var("ANIMUS_SNAPSHOT_DISABLED").is_ok() {
            self.snapshot.interval_secs = 0;
        }

        // Security overrides
        if std::env::var("ANIMUS_INJECTION_SCAN_DISABLED").is_ok() {
            self.security.injection_scanning_enabled = false;
        }
        if let Ok(ids) = std::env::var("ANIMUS_TRUSTED_TELEGRAM_IDS") {
            for id_str in ids.split(',') {
                if let Ok(id) = id_str.trim().parse::<i64>() {
                    if !self.security.trusted_telegram_ids.contains(&id) {
                        self.security.trusted_telegram_ids.push(id);
                    }
                }
            }
        }

        // Sensorium overrides
        if let Ok(threshold) = std::env::var("ANIMUS_SENSORIUM_ATTENTION_THRESHOLD") {
            if let Ok(v) = threshold.parse::<f32>() {
                if (0.0..=1.0).contains(&v) {
                    self.sensorium.attention_threshold = v;
                } else {
                    eprintln!("Warning: ANIMUS_SENSORIUM_ATTENTION_THRESHOLD must be in 0.0–1.0, got {v}");
                }
            }
        }

        // Voice overrides
        if std::env::var("ANIMUS_VOICE_ENABLED").as_deref() == Ok("1") {
            self.voice.enabled = true;
        }
        if std::env::var("ANIMUS_VOICE_TTS_ENABLED").as_deref() == Ok("1") {
            self.voice.tts_enabled = true;
        }
        // STT service connection
        if let Ok(url) = std::env::var("ANIMUS_STT_URL") {
            if !url.is_empty() {
                self.voice.stt_url = url;
            }
        }
        if let Ok(key) = std::env::var("ANIMUS_STT_KEY") {
            if !key.is_empty() {
                self.voice.stt_key = key;
            }
        }
        // Cartesia TTS
        if let Ok(key) = std::env::var("ANIMUS_CARTESIA_KEY") {
            if !key.is_empty() {
                self.voice.cartesia_api_key = key;
            }
        }
        if let Ok(id) = std::env::var("ANIMUS_CARTESIA_VOICE_ID") {
            if !id.is_empty() {
                self.voice.cartesia_voice_id = id;
            }
        }

        // Budget overrides
        if let Ok(v) = std::env::var("ANIMUS_BUDGET_MONTHLY_USD") {
            if let Ok(n) = v.parse::<f32>() {
                self.budget.monthly_limit_usd = n;
            }
        }
        if let Ok(v) = std::env::var("ANIMUS_BUDGET_CAREFUL_PCT") {
            if let Ok(n) = v.parse::<f32>() {
                self.budget.careful_threshold = n;
            }
        }
        if let Ok(v) = std::env::var("ANIMUS_BUDGET_EMERGENCY_PCT") {
            if let Ok(n) = v.parse::<f32>() {
                self.budget.emergency_threshold = n;
            }
        }
        if std::env::var("ANIMUS_BUDGET_HARD_CAP").as_deref() == Ok("1") {
            self.budget.hard_cap = true;
        }

        // Registration overrides
        if let Ok(v) = std::env::var("ANIMUS_REG_FIRST_NAME") { self.registration.first_name = v; }
        if let Ok(v) = std::env::var("ANIMUS_REG_LAST_NAME") { self.registration.last_name = v; }
        if let Ok(v) = std::env::var("ANIMUS_REG_DOB") { self.registration.dob = v; }
        if let Ok(v) = std::env::var("ANIMUS_REG_PHONE_PRIMARY") { self.registration.phone_primary = v; }
        if let Ok(v) = std::env::var("ANIMUS_REG_PHONE_FALLBACK") { self.registration.phone_fallback = v; }
        if let Ok(v) = std::env::var("ANIMUS_REG_SMS_TIMEOUT_SECS") {
            if let Ok(n) = v.parse::<u64>() { self.registration.sms_timeout_secs = n; }
        }
        if let Ok(v) = std::env::var("ANIMUS_REG_CAPTCHA_TIMEOUT_SECS") {
            if let Ok(n) = v.parse::<u64>() { self.registration.captcha_timeout_secs = n; }
        }
        if let Ok(v) = std::env::var("ANIMUS_REG_EMAIL_TIMEOUT_SECS") {
            if let Ok(n) = v.parse::<u64>() { self.registration.email_timeout_secs = n; }
        }
    }

    /// Load config from the standard path inside `data_dir`, applying
    /// environment variable overrides. Creates a default config file if
    /// none exists.
    pub fn load(data_dir: &Path) -> Result<Self> {
        // Allow override of config file path via env var
        let config_path = std::env::var("ANIMUS_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| data_dir.join("config.toml"));

        let mut config = Self::from_toml(&config_path)?;

        // Keep data_dir in sync with where we loaded from (unless overridden in file)
        if config.data_dir == PathBuf::from("./animus-data") {
            config.data_dir = data_dir.to_path_buf();
        }

        config.apply_env_overrides();

        // Write default config if it didn't exist yet (helps users discover options)
        if !config_path.exists() {
            if let Err(e) = config.save_toml(&config_path) {
                eprintln!("Warning: could not write default config to {}: {e}", config_path.display());
            } else {
                eprintln!("Info: wrote default config to {}", config_path.display());
            }
        }

        Ok(config)
    }
}
