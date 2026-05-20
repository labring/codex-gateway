use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Child, ChildStdin};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use chrono::Utc;
use serde_json::{Value, json};
use tokio::sync::{broadcast, oneshot};
use tracing::{error, info};

use crate::config::ClientInfo;
use crate::error::AppError;
use crate::models::{
    AccountSnapshot, BridgeEvent, BridgeStateSnapshot, ModelInfo, SessionClosedEvent, SummaryEvent,
    TranscriptEntry,
};

mod notifications;
mod process;
mod protocol;
mod rpc;
mod server_requests;
mod state;
mod transcript;
mod workspace_tools;

use protocol::unix_millis;
use transcript::transcript_from_thread;

const MAX_EVENTS: usize = 120;
const MAX_TRANSCRIPT: usize = 100;

#[derive(Clone)]
pub struct BridgeOptions {
    pub cwd: PathBuf,
    pub codex_bin: String,
    pub debug: bool,
    pub client_info: ClientInfo,
    pub default_model: Option<String>,
    pub activity_touch: Arc<dyn Fn() + Send + Sync>,
}

#[derive(Clone)]
pub struct CodexAppServerBridge {
    inner: Arc<BridgeInner>,
}

struct BridgeInner {
    cwd: PathBuf,
    codex_bin: String,
    debug: bool,
    client_info: ClientInfo,
    default_model: Option<String>,
    activity_touch: Arc<dyn Fn() + Send + Sync>,
    state: RwLock<BridgeStateSnapshot>,
    pending: Mutex<HashMap<u64, PendingRequest>>,
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Option<Child>>,
    next_id: AtomicU64,
    local_counter: AtomicU64,
    started: AtomicBool,
    closed: AtomicBool,
    events: broadcast::Sender<BridgeEvent>,
}

struct PendingRequest {
    method: String,
    tx: oneshot::Sender<Result<Value, AppError>>,
}

struct HandledServerRequest {
    result: Value,
    item_type: Option<String>,
    status: String,
    text_preview: Option<String>,
    system_note: Option<String>,
}

impl CodexAppServerBridge {
    pub fn new(options: BridgeOptions) -> Self {
        let cwd = options.cwd.display().to_string();
        let (events, _) = broadcast::channel(256);

        Self {
            inner: Arc::new(BridgeInner {
                cwd: options.cwd,
                codex_bin: options.codex_bin,
                debug: options.debug,
                client_info: options.client_info,
                default_model: options.default_model,
                activity_touch: options.activity_touch,
                state: RwLock::new(BridgeStateSnapshot::new(cwd)),
                pending: Mutex::new(HashMap::new()),
                stdin: Mutex::new(None),
                child: Mutex::new(None),
                next_id: AtomicU64::new(1),
                local_counter: AtomicU64::new(0),
                started: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                events,
            }),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BridgeEvent> {
        self.inner.events.subscribe()
    }

    pub fn get_state(&self) -> BridgeStateSnapshot {
        self.inner.state.read().unwrap().clone()
    }

    pub async fn start(&self) -> Result<BridgeStateSnapshot, AppError> {
        self.start_inner(true).await
    }

    pub async fn start_without_thread(&self) -> Result<BridgeStateSnapshot, AppError> {
        self.start_inner(false).await
    }

    async fn start_inner(&self, start_thread: bool) -> Result<BridgeStateSnapshot, AppError> {
        if self.inner.started.load(Ordering::SeqCst) {
            return Ok(self.get_state());
        }

        info!(
            cwd = %self.inner.cwd.display(),
            codex_bin = %self.inner.codex_bin,
            start_thread,
            "starting app-server bridge"
        );
        self.spawn_child()?;

        self.with_state(|state| {
            state.started_at = Some(Utc::now().to_rfc3339());
        });

        let initialize_result = self
            .request(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": self.inner.client_info.name,
                        "title": self.inner.client_info.title,
                        "version": self.inner.client_info.version,
                    }
                }),
            )
            .await?;

        self.send_json(&json!({
            "method": "initialized",
            "params": {}
        }))?;

        self.with_state(|state| {
            state.runtime.platform_family = initialize_result
                .get("platformFamily")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            state.runtime.platform_os = initialize_result
                .get("platformOs")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            state.runtime.user_agent = initialize_result
                .get("userAgent")
                .and_then(Value::as_str)
                .map(ToString::to_string);
        });

        self.refresh_account().await?;
        self.refresh_models().await?;
        if start_thread {
            self.start_new_thread(None).await?;
        }

        self.with_state(|state| {
            state.ready = true;
        });
        self.inner.started.store(true, Ordering::SeqCst);
        self.emit_state();
        info!(
            thread_id = self.get_state().thread_id.as_deref().unwrap_or("-"),
            "app-server bridge ready"
        );
        Ok(self.get_state())
    }

    pub async fn refresh_account(&self) -> Result<(), AppError> {
        let result = self
            .request("account/read", json!({ "refreshToken": false }))
            .await?;

        let account = result.get("account").cloned();
        let requires_openai_auth = result.get("requiresOpenaiAuth").and_then(Value::as_bool);
        let summary = describe_account(account.as_ref());

        self.with_state(|state| {
            state.account = AccountSnapshot {
                raw: account,
                summary,
                requires_openai_auth,
            };
        });
        self.emit_state();
        let state = self.get_state();
        info!(
            account_summary = %state.account.summary,
            requires_openai_auth = state.account.requires_openai_auth.unwrap_or(false),
            "refreshed account snapshot"
        );
        Ok(())
    }

    pub async fn refresh_models(&self) -> Result<(), AppError> {
        let result = self
            .request(
                "model/list",
                json!({
                    "limit": 50,
                    "includeHidden": false
                }),
            )
            .await?;

        let models = result
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|model| ModelInfo {
                model: model
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                display_name: model
                    .get("displayName")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                is_default: model
                    .get("isDefault")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                hidden: model
                    .get("hidden")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                supports_personality: model
                    .get("supportsPersonality")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                input_modalities: model
                    .get("inputModalities")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                    })
                    .filter(|items| !items.is_empty())
                    .unwrap_or_else(|| vec!["text".to_string(), "image".to_string()]),
            })
            .collect::<Vec<_>>();

        if models.is_empty() {
            return Err(AppError::internal("model/list returned no visible models"));
        }

        let selected_model = self
            .inner
            .default_model
            .clone()
            .or_else(|| {
                models
                    .iter()
                    .find(|model| model.is_default)
                    .map(|model| model.model.clone())
            })
            .or_else(|| models.first().map(|model| model.model.clone()));

        self.with_state(|state| {
            state.models = models;
            state.selected_model = selected_model;
        });
        self.emit_state();
        let state = self.get_state();
        info!(
            model_count = state.models.len(),
            selected_model = state.selected_model.as_deref().unwrap_or("-"),
            "refreshed model catalog"
        );
        Ok(())
    }

    pub async fn start_new_thread(&self, model: Option<String>) -> Result<String, AppError> {
        let selected_model = model
            .or_else(|| self.get_state().selected_model)
            .ok_or_else(|| AppError::internal("No model available for thread/start"))?;

        let result = self
            .request(
                "thread/start",
                json!({
                    "cwd": self.inner.cwd,
                    "model": selected_model
                }),
            )
            .await?;

        let thread_id = result
            .get("thread")
            .and_then(|thread| thread.get("id"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .ok_or_else(|| AppError::internal("thread/start did not return a thread id"))?;

        self.with_state(|state| {
            state.thread_id = Some(thread_id.clone());
            state.selected_model = Some(selected_model.clone());
            state.thread_status = Some(json!({ "type": "idle" }));
            state.current_turn_id = None;
            state.active_turn = false;
            state.last_turn_status = None;
            state.transcript.clear();
        });
        self.record_summary_event(SummaryEvent {
            at: Utc::now().to_rfc3339(),
            event_type: "local".to_string(),
            method: Some("thread/new".to_string()),
            item_type: None,
            item_id: None,
            status: Some("completed".to_string()),
            text_preview: Some(format!("Started thread {thread_id}")),
        });
        self.emit_state();
        info!(
            thread_id = %thread_id,
            model = %selected_model,
            "started new thread"
        );
        Ok(thread_id)
    }

    pub async fn list_threads(&self, params: Value) -> Result<Value, AppError> {
        self.request("thread/list", params).await
    }

    pub async fn read_thread(&self, thread_id: &str) -> Result<Value, AppError> {
        let thread_id = thread_id.trim();
        if thread_id.is_empty() {
            return Err(AppError::bad_request("threadId must not be empty"));
        }

        self.request(
            "thread/read",
            json!({
                "threadId": thread_id,
                "includeTurns": true
            }),
        )
        .await
    }

    pub async fn resume_thread(&self, thread_id: &str) -> Result<Value, AppError> {
        let thread_id = thread_id.trim();
        if thread_id.is_empty() {
            return Err(AppError::bad_request("threadId must not be empty"));
        }

        if self.get_state().active_turn {
            return Err(AppError::conflict(
                "Cannot resume thread while a turn is active",
            ));
        }

        let result = self
            .request(
                "thread/resume",
                json!({
                    "threadId": thread_id,
                    "cwd": self.inner.cwd,
                    "persistExtendedHistory": false
                }),
            )
            .await?;

        let thread = result.get("thread").cloned().unwrap_or_else(|| json!({}));
        let transcript = transcript_from_thread(&thread);
        let resumed_thread_id = thread
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(thread_id)
            .to_string();
        let model = result
            .get("model")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .or_else(|| self.get_state().selected_model);
        let thread_status = thread
            .get("status")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "idle" }));
        let active_turn = thread_status.get("type").and_then(Value::as_str) == Some("active");

        self.with_state(|state| {
            state.thread_id = Some(resumed_thread_id.clone());
            state.selected_model = model;
            state.thread_status = Some(thread_status);
            state.current_turn_id = None;
            state.active_turn = active_turn;
            state.last_turn_status = None;
            state.transcript = transcript;
        });
        self.record_summary_event(SummaryEvent {
            at: Utc::now().to_rfc3339(),
            event_type: "local".to_string(),
            method: Some("thread/resume".to_string()),
            item_type: None,
            item_id: None,
            status: Some("completed".to_string()),
            text_preview: Some(format!("Resumed thread {resumed_thread_id}")),
        });
        self.emit_state();
        info!(
            thread_id = %resumed_thread_id,
            model = self.get_state().selected_model.as_deref().unwrap_or("-"),
            active_turn,
            "resumed thread"
        );

        Ok(result)
    }

    pub async fn send_prompt(&self, prompt_text: &str) -> Result<Value, AppError> {
        let prompt = prompt_text.trim();
        if prompt.is_empty() {
            return Err(AppError::bad_request("Prompt must not be empty"));
        }

        if self.get_state().active_turn {
            return Err(AppError::conflict("A turn is already in progress"));
        }

        if self.get_state().thread_id.is_none() {
            self.start_new_thread(None).await?;
        }

        self.push_transcript(TranscriptEntry {
            id: self.make_local_id("user"),
            role: "user".to_string(),
            text: prompt.to_string(),
            status: "completed".to_string(),
            source: "local".to_string(),
            created_at: unix_millis(),
        });

        self.with_state(|state| {
            state.active_turn = true;
            state.last_turn_status = Some("inProgress".to_string());
        });
        self.emit_state();
        info!(
            thread_id = self.get_state().thread_id.as_deref().unwrap_or("-"),
            prompt_len = prompt.chars().count(),
            "sending turn/start to app-server"
        );

        let thread_id = self
            .get_state()
            .thread_id
            .ok_or_else(|| AppError::internal("Thread not ready for turn/start"))?;

        let result = self
            .request(
                "turn/start",
                json!({
                    "threadId": thread_id,
                    "input": [
                        {
                            "type": "text",
                            "text": prompt
                        }
                    ]
                }),
            )
            .await;

        match result {
            Ok(result) => {
                let turn_id = result
                    .get("turn")
                    .and_then(|turn| turn.get("id"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                let status = result
                    .get("turn")
                    .and_then(|turn| turn.get("status"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string);

                self.with_state(|state| {
                    let matches_active_turn = state.active_turn
                        && state
                            .current_turn_id
                            .as_deref()
                            .is_none_or(|current_turn_id| {
                                turn_id.as_deref() == Some(current_turn_id)
                            });

                    if matches_active_turn {
                        if let Some(turn_id) = turn_id.as_ref() {
                            state.current_turn_id = Some(turn_id.clone());
                        }
                        if let Some(status) = status.as_ref() {
                            state.last_turn_status = Some(status.clone());
                        }
                    }
                });
                self.emit_state();
                info!(
                    thread_id = self.get_state().thread_id.as_deref().unwrap_or("-"),
                    turn_id = self.get_state().current_turn_id.as_deref().unwrap_or("-"),
                    status = self.get_state().last_turn_status.as_deref().unwrap_or("-"),
                    "turn/start accepted"
                );
                Ok(result)
            }
            Err(error) => {
                self.with_state(|state| {
                    state.current_turn_id = None;
                    state.active_turn = false;
                    state.last_turn_status = Some("failed".to_string());
                });
                self.emit_state();
                error!(error = %error, "turn/start failed");
                Err(error)
            }
        }
    }

    pub async fn interrupt_turn(&self) -> Result<Value, AppError> {
        let state = self.get_state();
        if !state.active_turn {
            return Err(AppError::conflict("No active turn to interrupt"));
        }

        let thread_id = state
            .thread_id
            .clone()
            .ok_or_else(|| AppError::conflict("No active thread to interrupt"))?;
        let turn_id = state
            .current_turn_id
            .clone()
            .ok_or_else(|| AppError::conflict("Active turn is not ready to interrupt yet"))?;
        let requested_turn_id = turn_id.clone();

        let result = self
            .request(
                "turn/interrupt",
                json!({
                    "threadId": thread_id,
                    "turnId": turn_id
                }),
            )
            .await?;

        self.with_state(|state| {
            if state.active_turn {
                state.last_turn_status = Some("interruptRequested".to_string());
            }
        });
        self.record_summary_event(SummaryEvent {
            at: Utc::now().to_rfc3339(),
            event_type: "local".to_string(),
            method: Some("turn/interrupt".to_string()),
            item_type: None,
            item_id: Some(requested_turn_id.clone()),
            status: Some("accepted".to_string()),
            text_preview: Some("Interrupt requested".to_string()),
        });
        self.emit_state();
        info!(
            thread_id = self.get_state().thread_id.as_deref().unwrap_or("-"),
            turn_id = %requested_turn_id,
            "turn interrupt requested"
        );

        Ok(result)
    }

    pub async fn wait_for_turn_completion(&self, timeout: Duration) -> Result<Value, AppError> {
        let mut receiver = self.subscribe();

        let state = self.get_state();
        if !state.active_turn {
            return Ok(turn_result_from_state(&state));
        }

        let result = tokio::time::timeout(timeout, async move {
            loop {
                match receiver.recv().await {
                    Ok(BridgeEvent::Notification(message)) => {
                        if message.get("method").and_then(Value::as_str) == Some("turn/completed") {
                            return Ok(message
                                .get("params")
                                .and_then(|params| params.get("turn"))
                                .cloned()
                                .unwrap_or_else(|| json!({})));
                        }
                    }
                    Ok(BridgeEvent::State(state)) => {
                        if !state.active_turn {
                            return Ok(turn_result_from_state(&state));
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(AppError::ChannelClosed);
                    }
                }
            }
        })
        .await;

        match result {
            Ok(value) => value,
            Err(_) => Err(AppError::internal(format!(
                "Timed out waiting for turn completion after {}ms",
                timeout.as_millis()
            ))),
        }
    }

    pub fn get_latest_assistant_text(&self) -> String {
        self.get_state()
            .transcript
            .into_iter()
            .rev()
            .find(|entry| entry.role == "assistant" && !entry.text.is_empty())
            .map(|entry| entry.text)
            .unwrap_or_default()
    }

    pub fn broadcast_session_closed(&self, session_id: &str, reason: &str) {
        let _ = self
            .inner
            .events
            .send(BridgeEvent::SessionClosed(SessionClosedEvent {
                session_id: session_id.to_string(),
                reason: reason.to_string(),
            }));
    }
}

fn turn_result_from_state(state: &BridgeStateSnapshot) -> Value {
    json!({
        "status": state.last_turn_status
    })
}

fn describe_account(account: Option<&Value>) -> String {
    let Some(account) = account else {
        return "none".to_string();
    };

    if account.get("type").and_then(Value::as_str) == Some("chatgpt") {
        let email = account
            .get("email")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return format!("chatgpt:{email}");
    }

    account
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}
