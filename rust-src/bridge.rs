use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::Utc;
use serde_json::{Value, json};
use tokio::sync::{broadcast, oneshot};

use crate::config::ClientInfo;
use crate::env_config::apply_codex_child_env;
use crate::error::AppError;
use crate::models::{
    AccountSnapshot, BridgeEvent, BridgeStateSnapshot, ModelInfo, RpcErrorPayload,
    ServerRequestEvent, SessionClosedEvent, SummaryEvent, TranscriptEntry, WarningEvent,
};
use crate::runtime::get_codex_config_args;

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
        if self.inner.started.load(Ordering::SeqCst) {
            return Ok(self.get_state());
        }

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
        self.start_new_thread(None).await?;

        self.with_state(|state| {
            state.ready = true;
        });
        self.inner.started.store(true, Ordering::SeqCst);
        self.emit_state();
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
        Ok(thread_id)
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

        let thread_id = self
            .get_state()
            .thread_id
            .ok_or_else(|| AppError::internal("Thread not ready for turn/start"))?;

        self.request(
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
        .await
    }

    pub async fn wait_for_turn_completion(&self, timeout: Duration) -> Result<Value, AppError> {
        let state = self.get_state();
        if !state.active_turn {
            return Ok(json!({
                "status": state.last_turn_status
            }));
        }

        let mut receiver = self.subscribe();
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

    pub async fn stop(&self) -> Result<(), AppError> {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        self.fail_pending(format!(
            "{} app-server process is not available",
            self.inner.codex_bin
        ));

        self.inner.stdin.lock().unwrap().take();

        if let Some(mut child) = self.inner.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }

        Ok(())
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, AppError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();

        self.inner.pending.lock().unwrap().insert(
            id,
            PendingRequest {
                method: method.to_string(),
                tx,
            },
        );

        if let Err(error) = self.send_json(&json!({
            "method": method,
            "id": id,
            "params": params
        })) {
            self.inner.pending.lock().unwrap().remove(&id);
            return Err(error);
        }

        match rx.await {
            Ok(result) => result,
            Err(_) => Err(AppError::ChannelClosed),
        }
    }

    fn spawn_child(&self) -> Result<(), AppError> {
        let mut child = Command::new(&self.inner.codex_bin);
        child
            .arg("app-server")
            .args(get_codex_config_args())
            .current_dir(&self.inner.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        apply_codex_child_env(&mut child);

        let mut child = child.spawn().map_err(|error| {
            AppError::internal(format!(
                "Failed to start {} app-server: {error}",
                self.inner.codex_bin
            ))
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppError::internal("Failed to capture app-server stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppError::internal("Failed to capture app-server stdout"))?;

        *self.inner.stdin.lock().unwrap() = Some(stdin);
        *self.inner.child.lock().unwrap() = Some(child);
        self.inner.closed.store(false, Ordering::SeqCst);

        self.spawn_stdout_thread(stdout);
        self.spawn_wait_thread();

        Ok(())
    }

    fn spawn_stdout_thread(&self, stdout: ChildStdout) {
        let bridge = self.clone();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) => bridge.handle_line(line),
                    Err(error) => {
                        bridge.emit_warning(WarningEvent {
                            warning_type: "stdio-read-error".to_string(),
                            message: "Failed to read app-server message".to_string(),
                            detail: Some(error.to_string()),
                        });
                        break;
                    }
                }
            }
        });
    }

    fn spawn_wait_thread(&self) {
        let bridge = self.clone();
        thread::spawn(move || {
            loop {
                if bridge.inner.closed.load(Ordering::SeqCst) {
                    break;
                }

                let status = {
                    let mut child_guard = bridge.inner.child.lock().unwrap();
                    match child_guard.as_mut() {
                        Some(child) => child.try_wait(),
                        None => break,
                    }
                };

                match status {
                    Ok(Some(exit_status)) => {
                        bridge.inner.closed.store(true, Ordering::SeqCst);
                        bridge.fail_pending(format!(
                            "{} app-server exited before replying",
                            bridge.inner.codex_bin
                        ));
                        bridge.emit_warning(WarningEvent {
                            warning_type: "process-exit".to_string(),
                            message: format!("{} app-server exited", bridge.inner.codex_bin),
                            detail: Some(format!("code={:?}", exit_status.code())),
                        });
                        break;
                    }
                    Ok(None) => thread::sleep(Duration::from_millis(250)),
                    Err(error) => {
                        bridge.emit_warning(WarningEvent {
                            warning_type: "process-error".to_string(),
                            message: format!(
                                "Failed to inspect {} app-server",
                                bridge.inner.codex_bin
                            ),
                            detail: Some(error.to_string()),
                        });
                        break;
                    }
                }
            }
        });
    }

    fn handle_line(&self, line: String) {
        let message: Value = match serde_json::from_str(&line) {
            Ok(message) => message,
            Err(error) => {
                self.emit_warning(WarningEvent {
                    warning_type: "json-parse-error".to_string(),
                    message: "Failed to parse app-server message".to_string(),
                    detail: Some(error.to_string()),
                });
                return;
            }
        };

        if self.inner.debug {
            let _ = self.inner.events.send(BridgeEvent::Raw(line.clone()));
        }

        if is_server_request(&message) {
            self.handle_server_request(message);
            return;
        }

        if is_response(&message) {
            self.handle_response(message);
            return;
        }

        if message.get("method").and_then(Value::as_str).is_some() {
            self.handle_notification(message);
            return;
        }

        self.emit_warning(WarningEvent {
            warning_type: "unknown-message".to_string(),
            message: "Received unknown app-server message shape".to_string(),
            detail: Some(line),
        });
    }

    fn handle_response(&self, message: Value) {
        let Some(id) = message.get("id").and_then(Value::as_u64) else {
            return;
        };

        let Some(pending) = self.inner.pending.lock().unwrap().remove(&id) else {
            self.emit_warning(WarningEvent {
                warning_type: "unexpected-response".to_string(),
                message: format!("Received response for unknown id={id}"),
                detail: None,
            });
            return;
        };

        if let Some(error) = message.get("error") {
            let code = error
                .get("code")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            let text = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Unknown app-server error");
            let _ = pending.tx.send(Err(AppError::internal(format!(
                "{} failed: {text} (code={code})",
                pending.method
            ))));
            return;
        }

        let _ = pending.tx.send(Ok(message
            .get("result")
            .cloned()
            .unwrap_or_else(|| json!({}))));
    }

    fn handle_server_request(&self, message: Value) {
        let id = message
            .get("id")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));

        if method == "item/commandExecution/requestApproval"
            || method == "item/fileChange/requestApproval"
        {
            let _ = self.send_json(&json!({
                "id": id,
                "result": "decline"
            }));
            self.record_summary_event(SummaryEvent {
                at: Utc::now().to_rfc3339(),
                event_type: "serverRequest".to_string(),
                method: Some(method.clone()),
                item_type: Some(if method.contains("commandExecution") {
                    "commandExecution".to_string()
                } else {
                    "fileChange".to_string()
                }),
                item_id: None,
                status: Some("auto-declined".to_string()),
                text_preview: preview(params.get("reason"))
                    .or_else(|| preview(params.get("command")))
                    .or_else(|| preview(params.get("cwd"))),
            });
            self.push_system_note(format!("Auto-declined {method} in the gateway web UI."));
            let _ = self
                .inner
                .events
                .send(BridgeEvent::ServerRequest(ServerRequestEvent {
                    method,
                    params,
                    handled: true,
                    result: Some("decline".to_string()),
                    error: None,
                }));
            self.emit_state();
            return;
        }

        let error = RpcErrorPayload {
            code: -32601,
            message: format!("Unsupported server request in gateway demo: {method}"),
        };
        let _ = self.send_json(&json!({
            "id": id,
            "error": {
                "code": error.code,
                "message": error.message
            }
        }));
        self.record_summary_event(SummaryEvent {
            at: Utc::now().to_rfc3339(),
            event_type: "serverRequest".to_string(),
            method: Some(method.clone()),
            item_type: None,
            item_id: None,
            status: Some("rejected".to_string()),
            text_preview: Some(error.message.clone()),
        });
        self.push_system_note(error.message.clone());
        let _ = self
            .inner
            .events
            .send(BridgeEvent::ServerRequest(ServerRequestEvent {
                method,
                params,
                handled: false,
                result: None,
                error: Some(error),
            }));
        self.emit_state();
    }

    fn handle_notification(&self, message: Value) {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
        let item = params.get("item").cloned().unwrap_or_else(|| json!({}));

        match method.as_str() {
            "thread/started" => {
                let thread_id = params
                    .get("thread")
                    .and_then(|thread| thread.get("id"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                self.with_state(|state| {
                    if let Some(thread_id) = thread_id {
                        state.thread_id = Some(thread_id);
                    }
                });
            }
            "thread/status/changed" => {
                self.with_state(|state| {
                    state.thread_status = params.get("status").cloned();
                });
            }
            "turn/started" => {
                let turn_id = params
                    .get("turn")
                    .and_then(|turn| turn.get("id"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                self.with_state(|state| {
                    state.current_turn_id = turn_id;
                    state.active_turn = true;
                    state.last_turn_status = Some("inProgress".to_string());
                });
            }
            "turn/completed" => {
                let status = params
                    .get("turn")
                    .and_then(|turn| turn.get("status"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                self.with_state(|state| {
                    state.current_turn_id = None;
                    state.active_turn = false;
                    state.last_turn_status = status;
                });
            }
            "item/started" => self.handle_started_item(&item),
            "item/agentMessage/delta" => self.handle_agent_message_delta(&params),
            "item/completed" => self.handle_completed_item(&item),
            "error" => {
                let message = params
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown app-server error");
                self.push_system_note(message.to_string());
            }
            _ => {}
        }

        self.record_summary_event(summarize_notification(&message));
        self.touch_activity();
        let _ = self.inner.events.send(BridgeEvent::Notification(message));
        self.emit_state();
    }

    fn handle_started_item(&self, item: &Value) {
        let item_type = item.get("type").and_then(Value::as_str);
        match item_type {
            Some("agentMessage") => {
                let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
                let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
                self.upsert_transcript(TranscriptEntry {
                    id: id.to_string(),
                    role: "assistant".to_string(),
                    text: text.to_string(),
                    status: "inProgress".to_string(),
                    source: "app-server".to_string(),
                    created_at: unix_millis(),
                });
            }
            Some("userMessage") => {
                let text = extract_user_text(item);
                if !text.is_empty() && !self.has_recent_user_text(&text) {
                    self.push_transcript(TranscriptEntry {
                        id: item
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        role: "user".to_string(),
                        text,
                        status: "completed".to_string(),
                        source: "app-server".to_string(),
                        created_at: unix_millis(),
                    });
                }
            }
            _ => {}
        }
    }

    fn handle_completed_item(&self, item: &Value) {
        let item_type = item.get("type").and_then(Value::as_str);
        match item_type {
            Some("agentMessage") => {
                let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
                let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
                self.upsert_transcript(TranscriptEntry {
                    id: id.to_string(),
                    role: "assistant".to_string(),
                    text: text.to_string(),
                    status: "completed".to_string(),
                    source: "app-server".to_string(),
                    created_at: unix_millis(),
                });
            }
            Some("commandExecution")
                if item.get("status").and_then(Value::as_str) == Some("declined") =>
            {
                self.push_system_note(
                    "A command execution request was declined by the gateway UI.".to_string(),
                );
            }
            Some("fileChange")
                if item.get("status").and_then(Value::as_str) == Some("declined") =>
            {
                self.push_system_note(
                    "A file change request was declined by the gateway UI.".to_string(),
                );
            }
            _ => {}
        }
    }

    fn handle_agent_message_delta(&self, params: &Value) {
        let item_id = params.get("itemId").and_then(Value::as_str);
        let delta = extract_delta_text(params);

        let Some(item_id) = item_id else {
            return;
        };
        if delta.is_empty() {
            return;
        }

        self.with_state(|state| {
            if let Some(existing) = state
                .transcript
                .iter_mut()
                .find(|entry| entry.id == item_id)
            {
                existing.text.push_str(&delta);
                existing.status = "inProgress".to_string();
                return;
            }

            state.transcript.push(TranscriptEntry {
                id: item_id.to_string(),
                role: "assistant".to_string(),
                text: delta,
                status: "inProgress".to_string(),
                source: "app-server".to_string(),
                created_at: unix_millis(),
            });
            trim_transcript(&mut state.transcript);
        });
    }

    fn emit_warning(&self, warning: WarningEvent) {
        self.record_summary_event(SummaryEvent {
            at: Utc::now().to_rfc3339(),
            event_type: "warning".to_string(),
            method: Some(warning.warning_type.clone()),
            item_type: None,
            item_id: None,
            status: Some("warning".to_string()),
            text_preview: preview_string(&warning.message).or_else(|| warning.detail.clone()),
        });
        self.touch_activity();
        let _ = self.inner.events.send(BridgeEvent::Warning(warning));
        self.emit_state();
    }

    fn emit_state(&self) {
        self.touch_activity();
        let _ = self.inner.events.send(BridgeEvent::State(self.get_state()));
    }

    fn send_json(&self, message: &Value) -> Result<(), AppError> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(AppError::internal("app-server process is not available"));
        }

        let mut guard = self.inner.stdin.lock().unwrap();
        let Some(stdin) = guard.as_mut() else {
            return Err(AppError::internal("app-server process is not available"));
        };

        serde_json::to_writer(&mut *stdin, message)?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    }

    fn with_state<T>(&self, mutator: impl FnOnce(&mut BridgeStateSnapshot) -> T) -> T {
        let mut state = self.inner.state.write().unwrap();
        mutator(&mut state)
    }

    fn push_transcript(&self, entry: TranscriptEntry) {
        self.with_state(|state| {
            state.transcript.push(entry);
            trim_transcript(&mut state.transcript);
        });
    }

    fn upsert_transcript(&self, entry: TranscriptEntry) {
        self.with_state(|state| {
            if let Some(existing) = state
                .transcript
                .iter_mut()
                .find(|current| current.id == entry.id)
            {
                existing.role = entry.role;
                existing.text = entry.text;
                existing.status = entry.status;
                existing.source = entry.source;
                return;
            }

            state.transcript.push(entry);
            trim_transcript(&mut state.transcript);
        });
    }

    fn push_system_note(&self, text: String) {
        self.push_transcript(TranscriptEntry {
            id: self.make_local_id("system"),
            role: "system".to_string(),
            text,
            status: "completed".to_string(),
            source: "bridge".to_string(),
            created_at: unix_millis(),
        });
    }

    fn has_recent_user_text(&self, text: &str) -> bool {
        self.get_state()
            .transcript
            .into_iter()
            .rev()
            .find(|entry| entry.role == "user")
            .map(|entry| entry.text == text)
            .unwrap_or(false)
    }

    fn record_summary_event(&self, entry: SummaryEvent) {
        self.with_state(|state| {
            state.recent_events.push(entry);
            if state.recent_events.len() > MAX_EVENTS {
                let drain = state.recent_events.len() - MAX_EVENTS;
                state.recent_events.drain(0..drain);
            }
        });
    }

    fn fail_pending(&self, message: String) {
        let pending = {
            let mut guard = self.inner.pending.lock().unwrap();
            guard
                .drain()
                .map(|(_, pending)| pending)
                .collect::<Vec<_>>()
        };

        for pending in pending {
            let _ = pending.tx.send(Err(AppError::internal(format!(
                "{message} before replying to {}",
                pending.method
            ))));
        }
    }

    fn make_local_id(&self, prefix: &str) -> String {
        let next = self.inner.local_counter.fetch_add(1, Ordering::SeqCst) + 1;
        format!("local-{prefix}-{next}")
    }

    fn touch_activity(&self) {
        (self.inner.activity_touch)();
    }
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

fn is_server_request(message: &Value) -> bool {
    message.get("id").is_some() && message.get("method").and_then(Value::as_str).is_some()
}

fn is_response(message: &Value) -> bool {
    message.get("id").is_some() && message.get("method").is_none()
}

fn summarize_notification(message: &Value) -> SummaryEvent {
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
    let item = params.get("item").cloned().unwrap_or_else(|| json!({}));

    SummaryEvent {
        at: Utc::now().to_rfc3339(),
        event_type: "notification".to_string(),
        method: message
            .get("method")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        item_type: item
            .get("type")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        item_id: item
            .get("id")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .or_else(|| {
                params
                    .get("itemId")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            }),
        status: params
            .get("turn")
            .and_then(|turn| turn.get("status"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .or_else(|| {
                item.get("status")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            }),
        text_preview: preview(item.get("text"))
            .or_else(|| preview(params.get("delta")))
            .or_else(|| preview(params.get("error").and_then(|error| error.get("message"))))
            .or_else(|| preview(item.get("command")))
            .or_else(|| preview(item.get("query"))),
    }
}

fn extract_user_text(item: &Value) -> String {
    item.get("content")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string()
        })
        .unwrap_or_default()
}

fn extract_delta_text(params: &Value) -> String {
    for key in ["delta", "text", "textDelta", "chunk", "content"] {
        if let Some(value) = params.get(key).and_then(Value::as_str) {
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }

    String::new()
}

fn trim_transcript(transcript: &mut Vec<TranscriptEntry>) {
    if transcript.len() > MAX_TRANSCRIPT {
        let drain = transcript.len() - MAX_TRANSCRIPT;
        transcript.drain(0..drain);
    }
}

fn preview(value: Option<&Value>) -> Option<String> {
    preview_string(value.and_then(Value::as_str)?)
}

fn preview_string(value: &str) -> Option<String> {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return None;
    }

    if compact.chars().count() > 120 {
        let trimmed = compact.chars().take(119).collect::<String>();
        return Some(format!("{trimmed}…"));
    }

    Some(compact)
}

fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
