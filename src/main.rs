use std::convert::Infallible;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_stream::stream;
use axum::body::Bytes;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::response::Response;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;
use tracing::{error, info, warn};
use uuid::Uuid;

use codex_gateway::auth::{AuthState, auth_middleware};
use codex_gateway::brain::deployments::{
    BrainDeploymentRegistry, BrainDeploymentStatusResponse, brain_deployment_status_from_thread,
    build_brain_deployment_prompt,
};
use codex_gateway::error::AppError;
use codex_gateway::models::BridgeEvent;
use codex_gateway::runtime::maybe_login_with_api_key;
use codex_gateway::{config::AppConfig, session_manager::SessionManager};

#[derive(Clone)]
struct AppState {
    session_manager: SessionManager,
    brain_deployment_registry: BrainDeploymentRegistry,
    public_dir: PathBuf,
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

#[derive(Debug, Default, Deserialize)]
struct ThreadRequest {
    model: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadResumeRequest {
    thread_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadListQuery {
    cursor: Option<String>,
    limit: Option<u64>,
    sort_key: Option<String>,
    archived: Option<bool>,
    cwd: Option<String>,
    search_term: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateBrainDeploymentRequest {
    github_token: Option<String>,
    repository: Option<String>,
    branch: Option<String>,
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
        public_dir = %config.public_dir.display(),
        codex_bin = %config.codex_bin,
        auth_enabled = config.auth.is_some(),
        debug = config.debug,
        max_sessions = config.max_sessions,
        brain_deployment_runtime = "embedded",
        session_ttl_ms = config.session_ttl.as_millis() as u64,
        session_sweep_interval_ms = config.session_sweep_interval.as_millis() as u64,
        "gateway configuration loaded"
    );
    maybe_login_with_api_key(&config.codex_bin)?;

    let session_manager = SessionManager::new(config.clone());
    let brain_deployment_registry = BrainDeploymentRegistry::new();
    let state = AppState {
        session_manager: session_manager.clone(),
        brain_deployment_registry,
        public_dir: config.public_dir.clone(),
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
        .route("/api/brain/deployments", post(create_brain_deployment))
        .route(
            "/api/brain/deployments/{thread_id}",
            get(get_brain_deployment),
        )
        .route("/api/threads", get(get_threads))
        .route("/api/threads/{thread_id}", get(get_thread))
        .route("/api/sessions/{id}/state", get(get_session_state))
        .route("/api/sessions/{id}/events", get(get_session_events))
        .route("/api/sessions/{id}/turn", post(post_turn))
        .route(
            "/api/sessions/{id}/turn/interrupt",
            post(post_interrupt_turn),
        )
        .route("/api/sessions/{id}/thread/resume", post(post_resume_thread))
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
    let raw_model = request.model.clone();
    let raw_resume_thread_id = request.resume_thread_id.clone();
    let model = trim_optional(request.model);
    let resume_thread_id = trim_optional(request.resume_thread_id);
    info!(
        input_model = raw_model.as_deref().unwrap_or(""),
        input_resume_thread_id = raw_resume_thread_id.as_deref().unwrap_or(""),
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

async fn create_brain_deployment(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<impl IntoResponse, AppError> {
    let request: CreateBrainDeploymentRequest = parse_json_body(body)?;
    let github_token = trim_optional(request.github_token)
        .ok_or_else(|| AppError::bad_request("githubToken must not be empty"))?;
    let repository = trim_optional(request.repository)
        .ok_or_else(|| AppError::bad_request("repository must not be empty"))?;
    validate_repository(&repository)?;
    let branch = trim_optional(request.branch);
    validate_branch(branch.as_deref())?;

    let create_guard = state.brain_deployment_registry.try_begin_create()?;
    info!("creating deployment task");

    let (session_id, _session, snapshot) = state
        .session_manager
        .create_brain_deployment_session()
        .await?;
    state
        .session_manager
        .touch_session_for(&session_id, state.brain_deployment_registry.timeout())?;
    let Some(thread_id) = snapshot.thread_id.clone() else {
        if let Err(error) = state
            .session_manager
            .close_session(&session_id, "deployment-thread-missing")
            .await
        {
            error!(
                session_id = %session_id,
                error = %error,
                "failed to close session after deployment thread creation failed"
            );
        }
        return Err(AppError::internal(
            "Deployment thread was not created by app-server",
        ));
    };

    let prompt =
        build_brain_deployment_prompt(&repository, branch.as_deref(), &github_token, false);
    if let Err(error) = state
        .session_manager
        .send_prompt(&session_id, &prompt)
        .await
    {
        if let Err(close_error) = state
            .session_manager
            .close_session(&session_id, "deployment-start-failed")
            .await
        {
            error!(
                session_id = %session_id,
                error = %close_error,
                "failed to close session after deployment turn start failed"
            );
        }
        return Err(error);
    }

    let record = create_guard.complete(
        thread_id.clone(),
        session_id.clone(),
        repository.clone(),
        branch.clone(),
    );
    info!(
        session_id = %session_id,
        thread_id = %thread_id,
        expires_at = %record.expires_at,
        "deployment task started"
    );

    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "threadId": thread_id,
            "status": "running",
        })),
    ))
}

async fn get_brain_deployment(
    State(state): State<AppState>,
    Path(thread_id): Path<String>,
) -> Result<Json<BrainDeploymentStatusResponse>, AppError> {
    let thread_id = thread_id.trim().to_string();
    if thread_id.is_empty() {
        return Err(AppError::bad_request("threadId must not be empty"));
    }

    let Some(record) = state.brain_deployment_registry.get(&thread_id) else {
        return Err(AppError::not_found(format!(
            "Unknown deployment thread: {thread_id}"
        )));
    };
    if let Some(response) = record.terminal_status.clone() {
        return Ok(Json(response));
    }
    if record.is_timed_out() {
        let response = record.timeout_response();
        mark_brain_deployment_terminal_and_close(
            &state,
            &thread_id,
            &record.session_id,
            response.clone(),
        )
        .await;
        return Ok(Json(response));
    }
    if state
        .session_manager
        .touch_session_for(
            &record.session_id,
            state.brain_deployment_registry.timeout(),
        )
        .is_err()
    {
        let response = record.stopped_response();
        mark_brain_deployment_terminal_and_close(
            &state,
            &thread_id,
            &record.session_id,
            response.clone(),
        )
        .await;
        return Ok(Json(response));
    }

    info!(thread_id = %thread_id, "reading deployment task");
    let thread_result = match state
        .session_manager
        .read_thread_with_session(&record.session_id, &thread_id)
        .await
    {
        Ok(result) => result,
        Err(error) if looks_like_missing_thread(&error) => {
            return Err(AppError::not_found(format!(
                "Unknown deployment thread: {thread_id}"
            )));
        }
        Err(error) if looks_like_app_server_stopped(&error) => {
            let response = record.stopped_response();
            mark_brain_deployment_terminal_and_close(
                &state,
                &thread_id,
                &record.session_id,
                response.clone(),
            )
            .await;
            return Ok(Json(response));
        }
        Err(error) => return Err(error),
    };

    if thread_result
        .get("thread")
        .and_then(|thread| thread.get("id"))
        .and_then(Value::as_str)
        .is_none()
    {
        return Err(AppError::not_found(format!(
            "Unknown deployment thread: {thread_id}"
        )));
    }

    let response = brain_deployment_status_from_thread(&thread_id, &thread_result);
    if response.status != "running" {
        mark_brain_deployment_terminal_and_close(
            &state,
            &thread_id,
            &record.session_id,
            response.clone(),
        )
        .await;
    }
    info!(
        thread_id = %thread_id,
        status = %response.status,
        image = response.image.as_deref().unwrap_or("-"),
        "deployment task read completed"
    );

    Ok(Json(response))
}

async fn mark_brain_deployment_terminal_and_close(
    state: &AppState,
    thread_id: &str,
    session_id: &str,
    response: BrainDeploymentStatusResponse,
) {
    state
        .brain_deployment_registry
        .mark_terminal(thread_id, response.clone());

    match state
        .session_manager
        .close_session(session_id, "deployment-terminal")
        .await
    {
        Ok(true) => {
            info!(
                session_id = %session_id,
                thread_id = %thread_id,
                status = %response.status,
                "deployment session closed after terminal status"
            );
        }
        Ok(false) => {
            warn!(
                session_id = %session_id,
                thread_id = %thread_id,
                "deployment session was already closed after terminal status"
            );
        }
        Err(error) => {
            warn!(
                session_id = %session_id,
                thread_id = %thread_id,
                error = %error,
                "failed to close deployment session after terminal status"
            );
        }
    }
}

async fn get_threads(
    State(state): State<AppState>,
    Query(query): Query<ThreadListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    info!(
        cursor = query.cursor.as_deref().unwrap_or("-"),
        limit = query.limit.unwrap_or_default(),
        sort_key = query.sort_key.as_deref().unwrap_or("-"),
        archived = query.archived.unwrap_or(false),
        cwd = query.cwd.as_deref().unwrap_or("-"),
        search_term = query.search_term.as_deref().unwrap_or("-"),
        "listing threads"
    );
    let result = state
        .session_manager
        .list_threads(thread_list_params(query))
        .await?;
    let thread_count = result
        .get("data")
        .and_then(Value::as_array)
        .map(|items| items.len())
        .unwrap_or(0);
    info!(thread_count, "listed threads");

    Ok(Json(json!({
        "ok": true,
        "threads": result.get("data").cloned().unwrap_or_else(|| json!([])),
        "nextCursor": result.get("nextCursor").cloned().unwrap_or(Value::Null),
        "raw": result,
    })))
}

async fn get_thread(
    State(state): State<AppState>,
    Path(thread_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    info!(input_thread_id = %thread_id, thread_id = %thread_id, "reading thread");
    let result = state.session_manager.read_thread(&thread_id).await?;
    let thread = result.get("thread").cloned().unwrap_or_else(|| json!({}));
    info!(thread_id = %thread_id, "thread read completed");

    Ok(Json(json!({
        "ok": true,
        "threadId": thread_id,
        "thread": thread,
        "raw": result,
    })))
}

async fn get_session_state(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    info!(input_session_id = %id, session_id = %id, "fetching session state");
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
    info!(
        input_session_id = %id,
        session_id = %session.id,
        "sse session events connected"
    );
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
    let raw_prompt = request.prompt.clone();
    let prompt = request
        .prompt
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::bad_request("Prompt must not be empty"))?;
    info!(
        session_id = %id,
        input_prompt = raw_prompt.as_deref().unwrap_or(""),
        prompt = %prompt,
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

async fn post_new_thread(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    let request: ThreadRequest = parse_json_body(body)?;
    let raw_model = request.model.clone();
    let model = trim_optional(request.model);
    info!(
        session_id = %id,
        input_model = raw_model.as_deref().unwrap_or(""),
        model = model.as_deref().unwrap_or("-"),
        "starting new thread"
    );
    let snapshot = state.session_manager.start_new_thread(&id, model).await?;
    let session = state.session_manager.get_session_info(&id)?;

    Ok(Json(json!({
        "ok": true,
        "sessionId": id,
        "session": session,
        "state": snapshot,
    })))
}

async fn post_resume_thread(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    let request: ThreadResumeRequest = parse_json_body(body)?;
    let raw_thread_id = request.thread_id.clone();
    let thread_id = trim_optional(request.thread_id)
        .ok_or_else(|| AppError::bad_request("threadId must not be empty"))?;
    info!(
        session_id = %id,
        input_thread_id = raw_thread_id.as_deref().unwrap_or(""),
        thread_id = %thread_id,
        "resuming thread"
    );
    let snapshot = state.session_manager.resume_thread(&id, &thread_id).await?;
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
    info!(input_session_id = %id, session_id = %id, "deleting session");
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

fn validate_repository(repository: &str) -> Result<(), AppError> {
    let parts = repository.split('/').collect::<Vec<_>>();
    if parts.len() != 2 || !is_repository_segment(parts[0]) || !is_repository_segment(parts[1]) {
        return Err(AppError::bad_request(
            "repository must use owner/repo format",
        ));
    }

    Ok(())
}

fn is_repository_segment(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
}

fn validate_branch(branch: Option<&str>) -> Result<(), AppError> {
    let Some(branch) = branch else {
        return Ok(());
    };

    if branch.len() > 255
        || branch.starts_with('-')
        || branch.starts_with('/')
        || branch.ends_with('/')
        || branch.ends_with(".lock")
        || branch.contains("..")
        || branch.contains("//")
        || branch
            .chars()
            .any(|ch| !ch.is_ascii_alphanumeric() && !matches!(ch, '/' | '-' | '_' | '.' | '#'))
    {
        return Err(AppError::bad_request(
            "branch contains unsupported characters",
        ));
    }

    Ok(())
}

fn looks_like_missing_thread(error: &AppError) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("not found")
        || message.contains("unknown thread")
        || message.contains("no thread")
}

fn looks_like_app_server_stopped(error: &AppError) -> bool {
    if matches!(error, AppError::ChannelClosed) {
        return true;
    }

    let message = error.to_string().to_ascii_lowercase();
    message.contains("app-server process is not available")
        || message.contains("background task channel closed")
        || message.contains("channel closed")
}

fn thread_list_params(query: ThreadListQuery) -> Value {
    let mut params = Map::new();

    if let Some(cursor) = trim_optional(query.cursor) {
        params.insert("cursor".to_string(), json!(cursor));
    }
    if let Some(limit) = query.limit {
        params.insert("limit".to_string(), json!(limit));
    }
    if let Some(sort_key) = trim_optional(query.sort_key) {
        params.insert("sortKey".to_string(), json!(sort_key));
    }
    if let Some(archived) = query.archived {
        params.insert("archived".to_string(), json!(archived));
    }
    if let Some(cwd) = trim_optional(query.cwd) {
        params.insert("cwd".to_string(), json!(cwd));
    }
    if let Some(search_term) = trim_optional(query.search_term) {
        params.insert("searchTerm".to_string(), json!(search_term));
    }

    Value::Object(params)
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
    info!(path = %path.display(), content_type, "serving static file");
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
    let thread_id = extract_thread_id(&path);
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
    let thread_id = thread_id.unwrap_or_else(|| "-".to_string());
    if status.is_server_error() {
        error!(
            request_id = %request_id,
            method = %method,
            path = %path,
            status = status.as_u16(),
            duration_ms,
            session_id = %session_id,
            thread_id = %thread_id,
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
            thread_id = %thread_id,
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
            thread_id = %thread_id,
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

fn extract_thread_id(path: &str) -> Option<String> {
    let segments = path.trim_matches('/').split('/').collect::<Vec<_>>();
    if segments.len() >= 3 && segments[0] == "api" && segments[1] == "threads" {
        Some(segments[2].to_string())
    } else if segments.len() >= 4
        && segments[0] == "api"
        && segments[1] == "brain"
        && segments[2] == "deployments"
    {
        Some(segments[3].to_string())
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
