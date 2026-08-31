//! HTTP router and handlers. The API surface is intentionally small: it is
//! exactly what the Brain deploy runner consumes.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_stream::stream;
use axum::body::Bytes;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::auth::{AuthState, auth_middleware};
use crate::error::AppError;
use crate::session::{Session, SessionEvent, SessionManager};

#[derive(Clone)]
pub struct AppState {
    pub session_manager: SessionManager,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionRequest {
    model: Option<String>,
    /// `threadId` is accepted as an alias: the Brain deploy runner sends that
    /// name when it recovers a lost session.
    #[serde(alias = "threadId")]
    resume_thread_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct TurnRequest {
    prompt: Option<String>,
}

pub fn build_router(state: AppState) -> Router {
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
    let session = state
        .session_manager
        .create_session(model, resume_thread_id)
        .await?;

    Ok(Json(session_response(&session)))
}

async fn get_session_state(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let session = state.session_manager.get(&id)?;
    Ok(Json(session_response(&session)))
}

async fn get_session_events(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>, AppError> {
    let session = state.session_manager.get(&id)?;
    let mut receiver = session.subscribe();
    info!(session_id = %session.id, "sse session events connected");
    let session_id = session.id.clone();
    let info = session.info();
    let snapshot = session.snapshot();

    let stream = stream! {
        let _guard = SseGuard {
            session_id: session_id.clone(),
            connected_at: Instant::now(),
        };
        yield Ok(sse_json_event("session", &info));
        yield Ok(sse_json_event("state", &snapshot));

        loop {
            match receiver.recv().await {
                Ok(event) => {
                    let Some(event) = session_event_to_sse(&session_id, event) else {
                        continue;
                    };
                    yield Ok(event);
                }
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
    let session = state.session_manager.get(&id)?;
    info!(
        session_id = %id,
        prompt_len = prompt.chars().count(),
        "starting turn"
    );
    session.send_prompt(&prompt).await?;
    let snapshot = session.snapshot();
    info!(
        session_id = %id,
        thread_id = snapshot.thread_id.as_deref().unwrap_or("-"),
        turn_id = snapshot.current_turn_id.as_deref().unwrap_or("-"),
        "turn start accepted"
    );

    Ok(Json(session_response(&session)))
}

async fn post_interrupt_turn(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let session = state.session_manager.get(&id)?;
    info!(session_id = %id, "interrupting active turn");
    session.interrupt_turn().await?;

    Ok(Json(session_response(&session)))
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

fn session_response(session: &Session) -> serde_json::Value {
    json!({
        "ok": true,
        "sessionId": session.id,
        "session": session.info(),
        "state": session.snapshot(),
    })
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

fn session_event_to_sse(session_id: &str, event: SessionEvent) -> Option<Event> {
    match event {
        SessionEvent::State(snapshot) => Some(sse_json_event("state", &snapshot)),
        SessionEvent::Notification(payload) => Some(sse_json_event("notification", &payload)),
        SessionEvent::ServerRequest(payload) => Some(sse_json_event("server-request", &payload)),
        SessionEvent::Warning(payload) => Some(sse_json_event("warning", &payload)),
        SessionEvent::Raw(payload) => Some(sse_json_event("raw", &payload)),
        SessionEvent::Closed { reason } => Some(sse_json_event(
            "session-closed",
            &json!({ "sessionId": session_id, "reason": reason }),
        )),
        SessionEvent::TurnSubmitted { .. } => None,
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

struct SseGuard {
    session_id: String,
    connected_at: Instant,
}

impl Drop for SseGuard {
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
    let session_id = extract_session_id(&path).unwrap_or_else(|| "-".to_string());
    let started_at = Instant::now();

    let mut response = next.run(req).await;
    let status = response.status();
    let duration_ms = started_at.elapsed().as_millis() as u64;

    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response
            .headers_mut()
            .insert(HeaderName::from_static("x-request-id"), value);
    }

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
    let mut segments = path.trim_matches('/').split('/');
    if segments.next() == Some("api") && segments.next() == Some("sessions") {
        segments.next().map(ToString::to_string)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_session_request_accepts_thread_id_alias() {
        let request: CreateSessionRequest =
            serde_json::from_str(r#"{"model":"m","threadId":"thread-9"}"#).expect("valid body");
        assert_eq!(request.resume_thread_id.as_deref(), Some("thread-9"));

        let request: CreateSessionRequest =
            serde_json::from_str(r#"{"resumeThreadId":"thread-8"}"#).expect("valid body");
        assert_eq!(request.resume_thread_id.as_deref(), Some("thread-8"));
    }

    #[test]
    fn extract_session_id_only_matches_session_paths() {
        assert_eq!(
            extract_session_id("/api/sessions/abc/turn"),
            Some("abc".to_string())
        );
        assert_eq!(extract_session_id("/healthz"), None);
    }
}
