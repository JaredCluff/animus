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

    /// Security and prompt injection protection configuration.
    pub security: SecurityConfig,
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
}

impl Default for NatsChannelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: "nats://localhost:4222".to_string(),
            subjects: vec!["animus.in.>".to_string()],
            reply_prefix: "animus.out".to_string(),
        }
    }
}

/// Top-level channels configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChannelsConfig {
    pub telegram: TelegramChannelConfig,
    pub http_api: HttpApiChannelConfig,
    pub nats: NatsChannelConfig,
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
}

impl Default for VectorFSConfig {
    fn default() -> Self {
        Self {
            dimensionality: 1024,
            max_segments: 100_000,
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
    /// LLM provider name (e.g., "anthropic", "openai", "mock").
    pub llm_provider: String,
    /// Model identifier.
    /// With Claude Max OAuth: use "claude-haiku-4-5-20251001".
    /// With ANTHROPIC_API_KEY: any model works (e.g., "claude-sonnet-4-20250514").
    pub model_id: String,
    /// API key for the LLM provider. Always read from env at runtime; never serialized.
    #[serde(skip)]
    pub api_key: Option<String>,
    /// Maximum tokens for LLM response.
    pub max_response_tokens: usize,
    /// System prompt prepended to every reasoning call.
    pub system_prompt: String,
}

impl Default for CortexConfig {
    fn default() -> Self {
        Self {
            llm_provider: "anthropic".to_string(),
            model_id: "claude-haiku-4-5-20251001".to_string(),
            api_key: None,
            max_response_tokens: 4096,
            system_prompt: String::new(),
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
    pub attention_similarity_threshold: f32,
}

impl Default for SensoriumConfig {
    fn default() -> Self {
        Self {
            watch_paths: Vec::new(),
            process_poll_interval_secs: 5,
            file_watching_enabled: false,
            process_monitoring_enabled: false,
            attention_similarity_threshold: 0.5,
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

        // Autonomy mode override
        if let Ok(mode) = std::env::var("ANIMUS_AUTONOMY_MODE") {
            match mode.parse::<AutonomyMode>() {
                Ok(m) => self.autonomy.default_mode = m,
                Err(e) => eprintln!("Warning: invalid ANIMUS_AUTONOMY_MODE value: {e}"),
            }
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
