//! OpenTelemetry observability for Animus.
//!
//! Initializes an OTLP exporter pointed at the Langfuse backend.
//! Provides helpers for creating LLM and tool execution spans that
//! follow the OpenTelemetry GenAI semantic conventions.

use opentelemetry::trace::{Span, SpanKind, Status, Tracer, TracerProvider};
use opentelemetry::KeyValue;
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::Resource;
use std::sync::OnceLock;
use std::time::Instant;

static TRACER: OnceLock<opentelemetry_sdk::trace::Tracer> = OnceLock::new();
static INSTANCE_ID: OnceLock<String> = OnceLock::new();
static INSTANCE_NAME: OnceLock<String> = OnceLock::new();

/// Initialize the OpenTelemetry tracer with OTLP export to Langfuse.
///
/// - `instance_id` — the Animus UUID, included in every trace/span.
/// - `instance_name` — optional human-readable name (e.g., "prod-main").
///   Read from `ANIMUS_INSTANCE_NAME` env var or falls back to instance_id.
pub fn init_tracing(instance_id: &str) -> Option<TracingGuard> {
    INSTANCE_ID.set(instance_id.to_string()).ok();
    let instance_name =
        std::env::var("ANIMUS_INSTANCE_NAME").unwrap_or_else(|_| instance_id.to_string());
    INSTANCE_NAME.set(instance_name.clone()).ok();
    let endpoint = match std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            tracing::info!("OTEL_EXPORTER_OTLP_ENDPOINT not set — Langfuse tracing disabled");
            return None;
        }
    };

    let headers = std::env::var("OTEL_EXPORTER_OTLP_HEADERS").unwrap_or_default();

    let header_pairs: Vec<(String, String)> = headers
        .split(',')
        .filter(|s| s.contains('='))
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect();

    let exporter = match opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(format!(
            "{}/api/public/otel/v1/traces",
            endpoint.trim_end_matches('/')
        ))
        .with_headers(header_pairs.into_iter().collect())
        .build()
    {
        Ok(exporter) => exporter,
        Err(e) => {
            tracing::warn!("Failed to build OTLP exporter: {e} — Langfuse tracing disabled");
            return None;
        }
    };

    let resource = Resource::new(vec![
        KeyValue::new("service.name", "animus"),
        KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
        KeyValue::new("animus.instance_id", instance_id.to_string()),
        KeyValue::new("animus.instance_name", instance_name),
    ]);

    let provider = opentelemetry_sdk::trace::TracerProvider::builder()
        .with_simple_exporter(exporter)
        .with_resource(resource)
        .build();

    let tracer = provider.tracer("animus");

    TRACER.set(tracer).ok();

    let _ = opentelemetry::global::set_tracer_provider(provider.clone());

    tracing::info!("Langfuse tracing initialized → {endpoint}");

    Some(TracingGuard {
        _provider: provider,
    })
}

pub struct TracingGuard {
    _provider: opentelemetry_sdk::trace::TracerProvider,
}

fn get_tracer() -> Option<&'static opentelemetry_sdk::trace::Tracer> {
    TRACER.get()
}

/// Returns instance_id and instance_name as OTel attributes for spans.
fn instance_attrs() -> Vec<KeyValue> {
    let id = INSTANCE_ID
        .get()
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());
    let name = INSTANCE_NAME.get().cloned().unwrap_or_else(|| id.clone());
    vec![
        KeyValue::new("animus.instance_id", id),
        KeyValue::new("animus.instance_name", name),
    ]
}

/// An in-progress LLM call span.
pub struct LlmSpan {
    model: String,
    provider: String,
    operation: String,
    started: Instant,
}

impl LlmSpan {
    pub fn start(model: &str, provider: &str, operation: &str) -> Self {
        Self {
            model: model.to_string(),
            provider: provider.to_string(),
            operation: operation.to_string(),
            started: Instant::now(),
        }
    }

    pub fn finish_ok(self, input_tokens: u64, output_tokens: u64) {
        let Some(tracer) = get_tracer() else { return };
        let mut attrs = instance_attrs();
        attrs.extend([
            KeyValue::new("gen_ai.system", self.provider.clone()),
            KeyValue::new("gen_ai.request.model", self.model.clone()),
            KeyValue::new("gen_ai.operation.name", self.operation.clone()),
            KeyValue::new("gen_ai.usage.input_tokens", input_tokens as i64),
            KeyValue::new("gen_ai.usage.output_tokens", output_tokens as i64),
            KeyValue::new(
                "gen_ai.latency_ms",
                self.started.elapsed().as_millis() as i64,
            ),
        ]);
        let mut span = tracer
            .span_builder(format!("gen_ai.{}", self.operation))
            .with_kind(SpanKind::Client)
            .with_attributes(attrs)
            .with_status(Status::Ok)
            .start(tracer);
        span.end();
    }

    pub fn finish_err(self, error: &str) {
        let Some(tracer) = get_tracer() else { return };
        let mut attrs = instance_attrs();
        attrs.extend([
            KeyValue::new("gen_ai.system", self.provider.clone()),
            KeyValue::new("gen_ai.request.model", self.model.clone()),
            KeyValue::new("gen_ai.operation.name", self.operation.clone()),
            KeyValue::new(
                "gen_ai.latency_ms",
                self.started.elapsed().as_millis() as i64,
            ),
            KeyValue::new("gen_ai.error", error.to_string()),
        ]);
        let mut span = tracer
            .span_builder(format!("gen_ai.{}", self.operation))
            .with_kind(SpanKind::Client)
            .with_attributes(attrs)
            .with_status(Status::error(error.to_string()))
            .start(tracer);
        span.end();
    }
}

/// A tool execution span.
pub struct ToolSpan {
    tool_name: String,
    started: Instant,
}

impl ToolSpan {
    pub fn start(tool_name: &str) -> Self {
        Self {
            tool_name: tool_name.to_string(),
            started: Instant::now(),
        }
    }

    pub fn finish_ok(self) {
        let Some(tracer) = get_tracer() else { return };
        let mut attrs = instance_attrs();
        attrs.extend([
            KeyValue::new("animus.tool.name", self.tool_name.clone()),
            KeyValue::new(
                "animus.tool.latency_ms",
                self.started.elapsed().as_millis() as i64,
            ),
        ]);
        let mut span = tracer
            .span_builder(format!("tool.{}", self.tool_name))
            .with_kind(SpanKind::Internal)
            .with_attributes(attrs)
            .with_status(Status::Ok)
            .start(tracer);
        span.end();
    }

    pub fn finish_err(self, error: &str) {
        let Some(tracer) = get_tracer() else { return };
        let mut attrs = instance_attrs();
        attrs.extend([
            KeyValue::new("animus.tool.name", self.tool_name.clone()),
            KeyValue::new(
                "animus.tool.latency_ms",
                self.started.elapsed().as_millis() as i64,
            ),
            KeyValue::new("animus.tool.error", error.to_string()),
        ]);
        let mut span = tracer
            .span_builder(format!("tool.{}", self.tool_name))
            .with_kind(SpanKind::Internal)
            .with_attributes(attrs)
            .with_status(Status::error(error.to_string()))
            .start(tracer);
        span.end();
    }
}

/// A reasoning turn span that wraps an entire LLM interaction.
pub struct ReasoningSpan {
    thread_name: String,
    model: String,
    started: Instant,
}

impl ReasoningSpan {
    pub fn start(thread_name: &str, model: &str) -> Self {
        Self {
            thread_name: thread_name.to_string(),
            model: model.to_string(),
            started: Instant::now(),
        }
    }

    pub fn finish_ok(self, response_len: usize) {
        let Some(tracer) = get_tracer() else { return };
        let mut attrs = instance_attrs();
        attrs.extend([
            KeyValue::new("animus.thread", self.thread_name),
            KeyValue::new("animus.model", self.model),
            KeyValue::new(
                "animus.reasoning.latency_ms",
                self.started.elapsed().as_millis() as i64,
            ),
            KeyValue::new("animus.reasoning.response_len", response_len as i64),
        ]);
        let mut span = tracer
            .span_builder("reasoning.turn")
            .with_kind(SpanKind::Internal)
            .with_attributes(attrs)
            .with_status(Status::Ok)
            .start(tracer);
        span.end();
    }

    pub fn finish_err(self, error: &str) {
        let Some(tracer) = get_tracer() else { return };
        let mut attrs = instance_attrs();
        attrs.extend([
            KeyValue::new("animus.thread", self.thread_name),
            KeyValue::new("animus.model", self.model),
            KeyValue::new(
                "animus.reasoning.latency_ms",
                self.started.elapsed().as_millis() as i64,
            ),
            KeyValue::new("animus.reasoning.error", error.to_string()),
        ]);
        let mut span = tracer
            .span_builder("reasoning.turn")
            .with_kind(SpanKind::Internal)
            .with_attributes(attrs)
            .with_status(Status::error(error.to_string()))
            .start(tracer);
        span.end();
    }
}
