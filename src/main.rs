use std::convert::Infallible;
use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_stream::stream;
use axum::body::Bytes;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::response::Response;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::net::TcpListener;
use tracing::{error, info, warn};
use uuid::Uuid;

use codex_gateway::auth::{AuthState, auth_middleware};
use codex_gateway::error::AppError;
use codex_gateway::models::BridgeEvent;
use codex_gateway::runtime::maybe_login_with_api_key;
use codex_gateway::{config::AppConfig, session_manager::SessionManager};

#[derive(Clone)]
struct AppState {
    session_manager: SessionManager,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionRequest {
    model: Option<String>,
    resume_thread_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct TurnRequest {
    prompt: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), AppError> {
    init_tracing();

    let root_dir = env::current_dir()?;
    let config = AppConfig::from_env(root_dir)?;
    info!(
        host = %config.host,
        port = config.port,
        bridge_cwd = %config.bridge_cwd.display(),
        codex_bin = %config.codex_bin,
        auth_enabled = config.auth.is_some(),
        debug = config.debug,
        max_sessions = config.max_sessions,
        session_ttl_ms = config.session_ttl.as_millis() as u64,
        session_sweep_interval_ms = config.session_sweep_interval.as_millis() as u64,
        "gateway configuration loaded"
    );
    maybe_login_with_api_key(&config.codex_bin)?;

    let session_manager = SessionManager::new(config.clone());
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
            if let Err(error) = session_manager.shutdown().await {
                error!("failed to shutdown session manager: {error}");
            }
        })
        .await
        .map_err(AppError::from)
}

fn build_router(state: AppState) -> Router {
    let auth_state = Arc::new(AuthState::new(state.session_manager.config().auth.clone()));

    let protected = Router::new()
        .route("/api/sessions", post(create_session))
        .route("/api/sessions/{id}/state", get(get_session_state))
        .route("/api/sessions/{id}/events", get(get_session_events))
        .route("/api/sessions/{id}/turn", post(post_turn))
        .route(
            "/api/sessions/{id}/turn/interrupt",
            post(post_interrupt_turn),
        )
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
        .layer(middleware::from_fn(access_log_middleware))
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

async fn create_session(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    let request: CreateSessionRequest = parse_json_body(body)?;
    let model = trim_optional(request.model);
    let resume_thread_id = trim_optional(request.resume_thread_id);
    info!(
        model = model.as_deref().unwrap_or("-"),
        resume_thread_id = resume_thread_id.as_deref().unwrap_or("-"),
        "creating session"
    );
    let (session_id, session, snapshot) = state
        .session_manager
        .create_session(model, resume_thread_id)
        .await?;
    info!(
        session_id = %session_id,
        thread_id = snapshot.thread_id.as_deref().unwrap_or("-"),
        "session created via http"
    );

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
    info!(session_id = %session.id, "sse session events connected");
    let session_id = session.id.clone();

    let stream = stream! {
        let _guard = SseSessionGuard {
            session_id: session_id.clone(),
            connected_at: Instant::now(),
        };
        yield Ok(sse_json_event("session", &session));
        yield Ok(sse_json_event("state", &snapshot));

        loop {
            match receiver.recv().await {
                Ok(event) => yield Ok(bridge_event_to_sse(event)),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(session_id = %session_id, skipped, "sse event receiver lagged");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    info!(session_id = %session_id, "sse event receiver closed");
                    break;
                }
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
    let prompt = trim_optional(request.prompt)
        .ok_or_else(|| AppError::bad_request("Prompt must not be empty"))?;
    info!(
        session_id = %id,
        prompt_len = prompt.chars().count(),
        "starting turn"
    );
    let snapshot = state.session_manager.send_prompt(&id, &prompt).await?;
    let session = state.session_manager.get_session_info(&id)?;
    info!(
        session_id = %id,
        thread_id = snapshot.thread_id.as_deref().unwrap_or("-"),
        turn_id = snapshot.current_turn_id.as_deref().unwrap_or("-"),
        "turn start accepted"
    );

    Ok(Json(json!({
        "ok": true,
        "sessionId": id,
        "session": session,
        "state": snapshot,
    })))
}

async fn post_interrupt_turn(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    info!(session_id = %id, "interrupting active turn");
    let snapshot = state.session_manager.interrupt_turn(&id).await?;
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
    info!(session_id = %id, "deleting session");
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

fn init_tracing() {
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
}

struct SseSessionGuard {
    session_id: String,
    connected_at: Instant,
}

impl Drop for SseSessionGuard {
    fn drop(&mut self) {
        info!(
            session_id = %self.session_id,
            duration_ms = self.connected_at.elapsed().as_millis() as u64,
            "sse session events disconnected"
        );
    }
}

async fn access_log_middleware(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let request_id = Uuid::new_v4().to_string();
    let session_id = extract_session_id(&path);
    let started_at = Instant::now();

    let mut response = next.run(req).await;
    let status = response.status();
    let duration_ms = started_at.elapsed().as_millis() as u64;

    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response
            .headers_mut()
            .insert(HeaderName::from_static("x-request-id"), value);
    }

    let session_id = session_id.unwrap_or_else(|| "-".to_string());
    if status.is_server_error() {
        error!(
            request_id = %request_id,
            method = %method,
            path = %path,
            status = status.as_u16(),
            duration_ms,
            session_id = %session_id,
            "http request failed"
        );
    } else if status.is_client_error() {
        warn!(
            request_id = %request_id,
            method = %method,
            path = %path,
            status = status.as_u16(),
            duration_ms,
            session_id = %session_id,
            "http request rejected"
        );
    } else {
        info!(
            request_id = %request_id,
            method = %method,
            path = %path,
            status = status.as_u16(),
            duration_ms,
            session_id = %session_id,
            "http request completed"
        );
    }

    response
}

fn extract_session_id(path: &str) -> Option<String> {
    let segments = path.trim_matches('/').split('/').collect::<Vec<_>>();
    if segments.len() >= 3 && segments[0] == "api" && segments[1] == "sessions" {
        Some(segments[2].to_string())
    } else {
        None
    }
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
