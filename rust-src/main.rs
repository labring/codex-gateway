use std::convert::Infallible;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_stream::stream;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware;
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::net::TcpListener;
use tracing::error;

use codex_gateway::auth::{AuthState, auth_middleware};
use codex_gateway::error::AppError;
use codex_gateway::models::BridgeEvent;
use codex_gateway::runtime::maybe_login_with_api_key;
use codex_gateway::{config::AppConfig, session_manager::SessionManager};

#[derive(Clone)]
struct AppState {
    session_manager: SessionManager,
    public_dir: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
struct CreateSessionRequest {
    model: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct TurnRequest {
    prompt: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ThreadRequest {
    model: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), AppError> {
    init_tracing();

    let root_dir = env::current_dir()?;
    let config = AppConfig::from_env(root_dir);
    maybe_login_with_api_key(&config.codex_bin)?;

    let session_manager = SessionManager::new(config.clone());
    let state = AppState {
        session_manager: session_manager.clone(),
        public_dir: config.public_dir.clone(),
    };

    let app = build_router(state);
    let listener = TcpListener::bind(format!("{}:{}", config.host, config.port)).await?;

    println!(
        "Codex gateway listening at http://{}:{}",
        config.host, config.port
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            if let Err(error) = session_manager.shutdown().await {
                error!("failed to shutdown session manager: {error}");
            }
        })
        .await
        .map_err(AppError::from)
}

fn build_router(state: AppState) -> Router {
    let auth_state = Arc::new(AuthState::new(
        state.session_manager.config().auth.clone(),
    ));

    let protected = Router::new()
        .route("/", get(index_html))
        .route("/app.js", get(app_js))
        .route("/styles.css", get(styles_css))
        .route(
            "/api/state",
            get(legacy_single_session_gone).post(legacy_single_session_gone),
        )
        .route(
            "/api/events",
            get(legacy_single_session_gone).post(legacy_single_session_gone),
        )
        .route(
            "/api/turn",
            get(legacy_single_session_gone).post(legacy_single_session_gone),
        )
        .route(
            "/api/thread/new",
            get(legacy_single_session_gone).post(legacy_single_session_gone),
        )
        .route("/api/sessions", post(create_session))
        .route("/api/sessions/{id}/state", get(get_session_state))
        .route("/api/sessions/{id}/events", get(get_session_events))
        .route("/api/sessions/{id}/turn", post(post_turn))
        .route("/api/sessions/{id}/thread/new", post(post_new_thread))
        .route("/api/sessions/{id}", delete(delete_session))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&auth_state),
            auth_middleware,
        ))
        .with_state(state.clone());

    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .merge(protected)
        .fallback(not_found)
        .with_state(state)
}

async fn healthz(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "ok": true,
        "uptimeSeconds": state.session_manager.uptime_seconds(),
    }))
}

async fn readyz(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "ok": true,
        "activeSessions": state.session_manager.count(),
    }))
}

async fn index_html(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    serve_static_file(
        state.public_dir.join("index.html"),
        "text/html; charset=utf-8",
    )
    .await
}

async fn app_js(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    serve_static_file(
        state.public_dir.join("app.js"),
        "text/javascript; charset=utf-8",
    )
    .await
}

async fn styles_css(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    serve_static_file(
        state.public_dir.join("styles.css"),
        "text/css; charset=utf-8",
    )
    .await
}

async fn legacy_single_session_gone() -> Result<Json<serde_json::Value>, AppError> {
    Err(AppError::gone(
        "Legacy single-session endpoints were removed. Create a session first via POST /api/sessions.",
    ))
}

async fn create_session(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    let request: CreateSessionRequest = parse_json_body(body)?;
    let model = trim_optional(request.model);
    let (session_id, session, snapshot) = state.session_manager.create_session(model).await?;

    Ok(Json(json!({
        "ok": true,
        "sessionId": session_id,
        "session": session,
        "state": snapshot,
    })))
}

async fn get_session_state(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let session = state.session_manager.get_session_info(&id)?;
    let snapshot = state.session_manager.get_state(&id)?;

    Ok(Json(json!({
        "ok": true,
        "sessionId": id,
        "session": session,
        "state": snapshot,
    })))
}

async fn get_session_events(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>, AppError> {
    let (session, snapshot, mut receiver) = state.session_manager.subscribe(&id)?;

    let stream = stream! {
        yield Ok(sse_json_event("session", &session));
        yield Ok(sse_json_event("state", &snapshot));

        loop {
            match receiver.recv().await {
                Ok(event) => yield Ok(bridge_event_to_sse(event)),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    ))
}

async fn post_turn(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    let request: TurnRequest = parse_json_body(body)?;
    let prompt = request
        .prompt
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::bad_request("Prompt must not be empty"))?;
    let snapshot = state.session_manager.send_prompt(&id, &prompt).await?;
    let session = state.session_manager.get_session_info(&id)?;

    Ok(Json(json!({
        "ok": true,
        "sessionId": id,
        "session": session,
        "state": snapshot,
    })))
}

async fn post_new_thread(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    let request: ThreadRequest = parse_json_body(body)?;
    let model = trim_optional(request.model);
    let snapshot = state.session_manager.start_new_thread(&id, model).await?;
    let session = state.session_manager.get_session_info(&id)?;

    Ok(Json(json!({
        "ok": true,
        "sessionId": id,
        "session": session,
        "state": snapshot,
    })))
}

async fn delete_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let removed = state.session_manager.close_session(&id, "deleted").await?;
    if !removed {
        return Err(AppError::not_found(format!("Unknown session: {id}")));
    }

    Ok(Json(json!({
        "ok": true,
        "sessionId": id,
    })))
}

async fn not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": "Not found"
        })),
    )
}

fn parse_json_body<T>(body: Bytes) -> Result<T, AppError>
where
    T: DeserializeOwned + Default,
{
    if body.is_empty() {
        return Ok(T::default());
    }

    serde_json::from_slice(&body)
        .map_err(|_| AppError::bad_request("Request body must be valid JSON"))
}

fn trim_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn bridge_event_to_sse(event: BridgeEvent) -> Event {
    match event {
        BridgeEvent::State(payload) => sse_json_event("state", &payload),
        BridgeEvent::Notification(payload) => sse_json_event("notification", &payload),
        BridgeEvent::ServerRequest(payload) => sse_json_event("server-request", &payload),
        BridgeEvent::Warning(payload) => sse_json_event("warning", &payload),
        BridgeEvent::Raw(payload) => sse_json_event("raw", &payload),
        BridgeEvent::SessionClosed(payload) => sse_json_event("session-closed", &payload),
    }
}

fn sse_json_event<T>(name: &str, payload: &T) -> Event
where
    T: serde::Serialize,
{
    Event::default()
        .event(name)
        .data(serde_json::to_string(payload).unwrap_or_else(|_| "null".to_string()))
}

async fn serve_static_file(
    path: PathBuf,
    content_type: &'static str,
) -> Result<impl IntoResponse, AppError> {
    let bytes = tokio::fs::read(path).await?;
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        bytes,
    ))
}

fn init_tracing() {
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info,tower_http=info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
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
