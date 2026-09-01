//! Langfuse OTLP export. Langfuse acts as an OpenTelemetry backend: spans go
//! to `{host}/api/public/otel/v1/traces` over HTTP/protobuf with Basic auth.
//! Export is batched on a background thread; a Langfuse outage can only ever
//! drop telemetry, never block or fail gateway requests.

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine;
use opentelemetry::trace::TracerProvider;
use opentelemetry_otlp::{Protocol, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::{SdkTracer, SdkTracerProvider};
use tracing::info;

use crate::config::LangfuseConfig;
use crate::error::AppError;
use crate::session::SessionHook;

use super::recorder::{self, RecorderContext};

pub struct LangfuseHandle {
    provider: SdkTracerProvider,
    tracer: SdkTracer,
    config: LangfuseConfig,
}

impl LangfuseHandle {
    pub fn init(config: &LangfuseConfig) -> Result<Self, AppError> {
        let auth = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", config.public_key, config.secret_key));
        let endpoint = format!(
            "{}/api/public/otel/v1/traces",
            config.host.trim_end_matches('/')
        );

        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(endpoint.clone())
            .with_headers(HashMap::from([
                ("Authorization".to_string(), format!("Basic {auth}")),
                // Real-time ingestion on the Langfuse v4 data model.
                ("x-langfuse-ingestion-version".to_string(), "4".to_string()),
            ]))
            .build()
            .map_err(|error| {
                AppError::internal(format!("Failed to build Langfuse exporter: {error}"))
            })?;

        let provider = SdkTracerProvider::builder()
            .with_resource(
                Resource::builder()
                    .with_service_name("codex-gateway")
                    .build(),
            )
            .with_batch_exporter(exporter)
            .build();
        let tracer = provider.tracer("codex-gateway");

        info!(
            endpoint = %endpoint,
            user_id = config.user_id.as_deref().unwrap_or("-"),
            session_id = config.session_id.as_deref().unwrap_or("-"),
            "langfuse tracing enabled"
        );
        Ok(Self {
            provider,
            tracer,
            config: config.clone(),
        })
    }

    /// Hook attached to every new session: spawns a recorder task that maps
    /// session events to Langfuse traces.
    pub fn session_hook(&self) -> SessionHook {
        let tracer = self.tracer.clone();
        let config = self.config.clone();
        Arc::new(move |session| {
            recorder::spawn(
                session,
                RecorderContext::new(tracer.clone(), &config, &session.id),
            );
        })
    }

    /// Flush pending spans; called on graceful shutdown.
    pub fn shutdown(&self) -> Result<(), AppError> {
        self.provider
            .shutdown()
            .map_err(|error| AppError::internal(format!("Langfuse shutdown failed: {error}")))
    }
}
