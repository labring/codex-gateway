//! Observability: log setup and Langfuse tracing of user/agent interactions.

mod langfuse;
mod recorder;
mod scrub;

pub use langfuse::LangfuseHandle;

use crate::config::AppConfig;
use crate::error::AppError;

pub fn init_tracing() {
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
}

/// Set up the Langfuse exporter when `LANGFUSE_PUBLIC_KEY` and
/// `LANGFUSE_SECRET_KEY` are configured; otherwise telemetry is disabled and
/// the gateway behaves exactly as before.
pub fn init_langfuse(config: &AppConfig) -> Result<Option<LangfuseHandle>, AppError> {
    match &config.langfuse {
        Some(langfuse) => Ok(Some(LangfuseHandle::init(langfuse)?)),
        None => Ok(None),
    }
}
