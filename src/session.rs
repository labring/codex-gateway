//! Session lifecycle: each session owns one `codex app-server` process,
//! tracks a minimal turn-state snapshot, and re-broadcasts codex events to
//! SSE subscribers and the telemetry recorder.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::sync::broadcast;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::codex::{CodexClient, CodexEvent, SpawnOptions};
use crate::config::AppConfig;
use crate::error::AppError;

/// The state consumed by gateway clients. Thread history lives in codex
/// itself; the gateway only tracks what it needs to run turns.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateSnapshot {
    pub ready: bool,
    pub cwd: String,
    pub started_at: Option<String>,
    pub selected_model: Option<String>,
    pub thread_id: Option<String>,
    pub current_turn_id: Option<String>,
    pub active_turn: bool,
    pub last_turn_status: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub last_access_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub enum SessionEvent {
    State(Box<StateSnapshot>),
    /// Full JSON-RPC notification message from codex.
    Notification(Value),
    ServerRequest(Value),
    Warning(Value),
    Raw(String),
    /// A prompt was accepted for a new turn. Consumed by the telemetry
    /// recorder; not forwarded over SSE.
    TurnSubmitted {
        prompt: String,
    },
    Closed {
        reason: String,
    },
}

pub struct Session {
    pub id: String,
    client: CodexClient,
    state: RwLock<StateSnapshot>,
    meta: SessionMeta,
    events: broadcast::Sender<SessionEvent>,
}

struct SessionMeta {
    created_at: DateTime<Utc>,
    last_access_at: RwLock<DateTime<Utc>>,
    expires_at: RwLock<DateTime<Utc>>,
    ttl: Duration,
}

impl Session {
    pub fn snapshot(&self) -> StateSnapshot {
        self.state.read().unwrap().clone()
    }

    pub fn info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id.clone(),
            created_at: self.meta.created_at,
            last_access_at: *self.meta.last_access_at.read().unwrap(),
            expires_at: *self.meta.expires_at.read().unwrap(),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.events.subscribe()
    }

    pub async fn send_prompt(&self, prompt: &str) -> Result<StateSnapshot, AppError> {
        if self.snapshot().active_turn {
            return Err(AppError::conflict("A turn is already in progress"));
        }
        let thread_id = self
            .snapshot()
            .thread_id
            .ok_or_else(|| AppError::internal("Thread not ready for turn/start"))?;

        self.update_state(|state| {
            state.active_turn = true;
            state.current_turn_id = None;
            state.last_turn_status = Some("inProgress".to_string());
        });
        let _ = self.events.send(SessionEvent::TurnSubmitted {
            prompt: prompt.to_string(),
        });

        let result = self
            .client
            .request(
                "turn/start",
                json!({
                    "threadId": thread_id,
                    "input": [ { "type": "text", "text": prompt } ]
                }),
            )
            .await;

        match result {
            Ok(result) => {
                let turn_id = result
                    .pointer("/turn/id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                let status = result
                    .pointer("/turn/status")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                self.update_state(|state| {
                    // The turn may already have completed: notifications can
                    // outrun the turn/start response. Never resurrect it.
                    let still_active = state.active_turn
                        && state
                            .current_turn_id
                            .as_deref()
                            .is_none_or(|current| turn_id.as_deref() == Some(current));
                    if still_active {
                        if let Some(turn_id) = turn_id.as_ref() {
                            state.current_turn_id = Some(turn_id.clone());
                        }
                        if let Some(status) = status.as_ref() {
                            state.last_turn_status = Some(status.clone());
                        }
                    }
                });
                Ok(self.snapshot())
            }
            Err(error) => {
                self.update_state(|state| {
                    state.active_turn = false;
                    state.current_turn_id = None;
                    state.last_turn_status = Some("failed".to_string());
                });
                error!(session_id = %self.id, error = %error, "turn/start failed");
                Err(error)
            }
        }
    }

    pub async fn interrupt_turn(&self) -> Result<StateSnapshot, AppError> {
        let snapshot = self.snapshot();
        if !snapshot.active_turn {
            return Err(AppError::conflict("No active turn to interrupt"));
        }
        let thread_id = snapshot
            .thread_id
            .ok_or_else(|| AppError::conflict("No active thread to interrupt"))?;
        let turn_id = snapshot
            .current_turn_id
            .ok_or_else(|| AppError::conflict("Active turn is not ready to interrupt yet"))?;

        self.client
            .request(
                "turn/interrupt",
                json!({ "threadId": thread_id, "turnId": turn_id }),
            )
            .await?;

        self.update_state(|state| {
            if state.active_turn {
                state.last_turn_status = Some("interruptRequested".to_string());
            }
        });
        info!(session_id = %self.id, turn_id = %turn_id, "turn interrupt requested");
        Ok(self.snapshot())
    }

    pub async fn close(&self, reason: &str) {
        let _ = self.events.send(SessionEvent::Closed {
            reason: reason.to_string(),
        });
        self.client.stop().await;
    }

    fn touch(&self) {
        let now = Utc::now();
        *self.meta.last_access_at.write().unwrap() = now;
        *self.meta.expires_at.write().unwrap() =
            now + chrono::Duration::from_std(self.meta.ttl).unwrap_or_default();
    }

    fn is_expired(&self, now: DateTime<Utc>) -> bool {
        *self.meta.expires_at.read().unwrap() <= now
    }

    fn update_state(&self, mutate: impl FnOnce(&mut StateSnapshot)) {
        let snapshot = {
            let mut state = self.state.write().unwrap();
            mutate(&mut state);
            state.clone()
        };
        let _ = self.events.send(SessionEvent::State(Box::new(snapshot)));
    }

    /// Forwards codex events, keeping the snapshot in sync with turn
    /// lifecycle notifications.
    fn handle_codex_event(&self, event: CodexEvent) {
        match event {
            CodexEvent::Notification(message) => {
                self.touch();
                self.apply_notification(&message);
                let _ = self.events.send(SessionEvent::Notification(message));
            }
            CodexEvent::ServerRequest(request) => {
                self.touch();
                let payload = serde_json::to_value(&request).unwrap_or_else(|_| json!({}));
                let _ = self.events.send(SessionEvent::ServerRequest(payload));
            }
            CodexEvent::Warning(warning) => {
                let payload = serde_json::to_value(&warning).unwrap_or_else(|_| json!({}));
                let _ = self.events.send(SessionEvent::Warning(payload));
            }
            CodexEvent::Raw(line) => {
                let _ = self.events.send(SessionEvent::Raw(line));
            }
            CodexEvent::Exited { code } => {
                // A dead app-server can never complete the turn; surface the
                // failure immediately instead of letting clients poll into
                // their own timeout.
                warn!(session_id = %self.id, code = code.unwrap_or(-1), "app-server exited");
                self.update_state(|state| {
                    state.ready = false;
                    if state.active_turn {
                        state.active_turn = false;
                        state.current_turn_id = None;
                        state.last_turn_status = Some("failed".to_string());
                    }
                });
            }
        }
    }

    fn apply_notification(&self, message: &Value) {
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params");
        match method {
            "thread/started" => {
                let thread_id = params
                    .and_then(|params| params.pointer("/thread/id"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                if let Some(thread_id) = thread_id {
                    self.update_state(|state| state.thread_id = Some(thread_id));
                }
            }
            "turn/started" => {
                let turn_id = params
                    .and_then(|params| params.pointer("/turn/id"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                self.update_state(|state| {
                    state.current_turn_id = turn_id;
                    state.active_turn = true;
                    state.last_turn_status = Some("inProgress".to_string());
                });
            }
            "turn/completed" => {
                let status = params
                    .and_then(|params| params.pointer("/turn/status"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                self.update_state(|state| {
                    state.current_turn_id = None;
                    state.active_turn = false;
                    state.last_turn_status = status;
                });
            }
            _ => {}
        }
    }
}

/// Called for every newly created session, before its thread starts. Used to
/// attach observers (e.g. the telemetry recorder) without coupling the
/// session layer to them.
pub type SessionHook = Arc<dyn Fn(&Arc<Session>) + Send + Sync>;

#[derive(Clone)]
pub struct SessionManager {
    inner: Arc<ManagerInner>,
}

struct ManagerInner {
    config: AppConfig,
    started_at: Instant,
    sessions: RwLock<HashMap<String, Arc<Session>>>,
    create_lock: Mutex<()>,
    session_hook: Option<SessionHook>,
}

impl SessionManager {
    pub fn new(config: AppConfig, session_hook: Option<SessionHook>) -> Self {
        let manager = Self {
            inner: Arc::new(ManagerInner {
                config,
                started_at: Instant::now(),
                sessions: RwLock::new(HashMap::new()),
                create_lock: Mutex::new(()),
                session_hook,
            }),
        };
        manager.spawn_sweeper();
        manager
    }

    pub fn config(&self) -> &AppConfig {
        &self.inner.config
    }

    pub fn uptime_seconds(&self) -> u64 {
        self.inner.started_at.elapsed().as_secs()
    }

    pub fn count(&self) -> usize {
        self.inner.sessions.read().unwrap().len()
    }

    pub async fn create_session(
        &self,
        model: Option<String>,
        resume_thread_id: Option<String>,
    ) -> Result<Arc<Session>, AppError> {
        let _guard = self.inner.create_lock.lock().await;
        self.sweep_expired_sessions().await;

        if self.count() >= self.inner.config.max_sessions {
            warn!(
                active_sessions = self.count(),
                max_sessions = self.inner.config.max_sessions,
                "maximum concurrent sessions reached"
            );
            return Err(AppError::service_unavailable(format!(
                "Maximum concurrent sessions reached ({})",
                self.inner.config.max_sessions
            )));
        }

        let id = Uuid::new_v4().to_string();
        info!(
            session_id = %id,
            model = model.as_deref().unwrap_or("-"),
            resume_thread_id = resume_thread_id.as_deref().unwrap_or("-"),
            "creating session"
        );

        let config = &self.inner.config;
        let client = CodexClient::spawn(SpawnOptions {
            label: id.clone(),
            cwd: config.cwd.clone(),
            codex_bin: config.codex_bin.clone(),
            args: config.codex_app_server_args(),
            env: config.codex_child_env(),
            debug: config.debug,
            client_info: config.client_info.clone(),
        })?;

        let now = Utc::now();
        let (events, _) = broadcast::channel(256);
        let session = Arc::new(Session {
            id: id.clone(),
            client: client.clone(),
            state: RwLock::new(StateSnapshot {
                ready: false,
                cwd: config.cwd.display().to_string(),
                started_at: Some(now.to_rfc3339_opts(SecondsFormat::Millis, true)),
                selected_model: None,
                thread_id: None,
                current_turn_id: None,
                active_turn: false,
                last_turn_status: None,
            }),
            meta: SessionMeta {
                created_at: now,
                last_access_at: RwLock::new(now),
                expires_at: RwLock::new(
                    now + chrono::Duration::from_std(config.session_ttl).unwrap_or_default(),
                ),
                ttl: config.session_ttl,
            },
            events,
        });

        spawn_event_pump(Arc::clone(&session), client.subscribe());
        if let Some(hook) = &self.inner.session_hook {
            hook(&session);
        }

        if let Err(error) = self.start_thread(&session, model, resume_thread_id).await {
            session.close("startup-failed").await;
            return Err(error);
        }

        self.inner
            .sessions
            .write()
            .unwrap()
            .insert(id.clone(), Arc::clone(&session));
        info!(
            session_id = %id,
            thread_id = session.snapshot().thread_id.as_deref().unwrap_or("-"),
            "session created"
        );
        Ok(session)
    }

    pub fn get(&self, session_id: &str) -> Result<Arc<Session>, AppError> {
        let session = self
            .inner
            .sessions
            .read()
            .unwrap()
            .get(session_id)
            .cloned()
            .ok_or_else(|| AppError::not_found(format!("Unknown session: {session_id}")))?;
        session.touch();
        Ok(session)
    }

    pub async fn close_session(&self, session_id: &str, reason: &str) -> Result<bool, AppError> {
        let session = self.inner.sessions.write().unwrap().remove(session_id);
        let Some(session) = session else {
            return Ok(false);
        };
        session.close(reason).await;
        info!(session_id = %session_id, reason = %reason, "session closed");
        Ok(true)
    }

    pub async fn shutdown(&self) {
        let ids = self
            .inner
            .sessions
            .read()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for id in ids {
            let _ = self.close_session(&id, "shutdown").await;
        }
    }

    async fn start_thread(
        &self,
        session: &Session,
        model: Option<String>,
        resume_thread_id: Option<String>,
    ) -> Result<(), AppError> {
        session.client.initialize().await?;

        if let Some(thread_id) = resume_thread_id {
            let result = session
                .client
                .request(
                    "thread/resume",
                    json!({
                        "threadId": thread_id,
                        "cwd": self.inner.config.cwd,
                        "persistExtendedHistory": false
                    }),
                )
                .await?;
            let thread = result.get("thread").cloned().unwrap_or_else(|| json!({}));
            let resumed_id = thread
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or(&thread_id)
                .to_string();
            let resumed_model = result
                .get("model")
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .or(model)
                .or_else(|| self.inner.config.default_model.clone());
            let active_turn =
                thread.pointer("/status/type").and_then(Value::as_str) == Some("active");
            session.update_state(|state| {
                state.ready = true;
                state.thread_id = Some(resumed_id.clone());
                state.selected_model = resumed_model;
                state.active_turn = active_turn;
            });
            info!(
                session_id = %session.id,
                thread_id = %resumed_id,
                active_turn,
                "resumed thread"
            );
            return Ok(());
        }

        let selected_model = match model.or_else(|| self.inner.config.default_model.clone()) {
            Some(model) => model,
            None => self.default_model_from_catalog(&session.client).await?,
        };
        let result = session
            .client
            .request(
                "thread/start",
                json!({
                    "cwd": self.inner.config.cwd,
                    "model": selected_model
                }),
            )
            .await?;
        let thread_id = result
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .ok_or_else(|| AppError::internal("thread/start did not return a thread id"))?;
        session.update_state(|state| {
            state.ready = true;
            state.thread_id = Some(thread_id.clone());
            state.selected_model = Some(selected_model.clone());
        });
        info!(
            session_id = %session.id,
            thread_id = %thread_id,
            model = %selected_model,
            "started new thread"
        );
        Ok(())
    }

    async fn default_model_from_catalog(&self, client: &CodexClient) -> Result<String, AppError> {
        let result = client
            .request("model/list", json!({ "limit": 50, "includeHidden": false }))
            .await?;
        let models = result
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        models
            .iter()
            .find(|model| model.get("isDefault").and_then(Value::as_bool) == Some(true))
            .or_else(|| models.first())
            .and_then(|model| model.get("model").and_then(Value::as_str))
            .map(ToString::to_string)
            .ok_or_else(|| AppError::internal("model/list returned no visible models"))
    }

    async fn sweep_expired_sessions(&self) {
        let now = Utc::now();
        let expired = self
            .inner
            .sessions
            .read()
            .unwrap()
            .iter()
            .filter(|(_, session)| session.is_expired(now))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in expired {
            if let Err(error) = self.close_session(&id, "expired").await {
                error!(session_id = %id, error = %error, "failed to close expired session");
            }
        }
    }

    fn spawn_sweeper(&self) {
        let manager = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(manager.inner.config.session_sweep_interval).await;
                manager.sweep_expired_sessions().await;
            }
        });
    }
}

fn spawn_event_pump(session: Arc<Session>, mut receiver: broadcast::Receiver<CodexEvent>) {
    tokio::spawn(async move {
        loop {
            match receiver.recv().await {
                Ok(event) => session.handle_codex_event(event),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(session_id = %session.id, skipped, "session event pump lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}
