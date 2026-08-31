use std::env;

use tokio::net::TcpListener;
use tracing::{error, info};

use codex_gateway::codex::login_with_api_key;
use codex_gateway::config::AppConfig;
use codex_gateway::error::AppError;
use codex_gateway::http::{AppState, build_router};
use codex_gateway::session::SessionManager;
use codex_gateway::telemetry;

#[tokio::main]
async fn main() -> Result<(), AppError> {
    telemetry::init_tracing();

    let config = AppConfig::from_env(env::current_dir()?);
    info!(
        host = %config.host,
        port = config.port,
        cwd = %config.cwd.display(),
        codex_bin = %config.codex_bin,
        auth_enabled = config.auth.is_some(),
        debug = config.debug,
        max_sessions = config.max_sessions,
        session_ttl_ms = config.session_ttl.as_millis() as u64,
        session_sweep_interval_ms = config.session_sweep_interval.as_millis() as u64,
        langfuse_enabled = config.langfuse.is_some(),
        "gateway configuration loaded"
    );
    login_with_api_key(&config)?;

    let langfuse = telemetry::init_langfuse(&config)?;
    let session_hook = langfuse.as_ref().map(|handle| handle.session_hook());
    let session_manager = SessionManager::new(config.clone(), session_hook);
    let state = AppState {
        session_manager: session_manager.clone(),
    };

    let app = build_router(state);
    let listener = TcpListener::bind(format!("{}:{}", config.host, config.port)).await?;

    println!(
        "Codex gateway listening at http://{}:{}",
        config.host, config.port
    );
    info!(host = %config.host, port = config.port, "gateway listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            session_manager.shutdown().await;
            if let Some(handle) = langfuse
                && let Err(error) = handle.shutdown()
            {
                error!("failed to flush langfuse traces: {error}");
            }
        })
        .await
        .map_err(AppError::from)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut stream) = signal(SignalKind::terminate()) {
            let _ = stream.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
