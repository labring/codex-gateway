//! Logging setup. Langfuse tracing lives in this module as well (see the
//! `langfuse` submodule added alongside the exporter).

pub fn init_tracing() {
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
}
