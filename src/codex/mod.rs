//! Async client for a `codex app-server` child process.
//!
//! The client owns the process, correlates JSON-RPC requests with responses,
//! auto-answers server-initiated requests (approvals, tool calls), and
//! broadcasts everything else as [`CodexEvent`]s. It holds no session state;
//! that is the session layer's job.

mod approvals;
mod login;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, oneshot};
use tracing::{debug, error, info, warn};

use crate::config::ClientInfo;
use crate::error::AppError;

pub use login::login_with_api_key;

#[derive(Clone)]
pub struct SpawnOptions {
    /// Used in log lines to attribute child output to its session.
    pub label: String,
    pub cwd: PathBuf,
    pub codex_bin: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub debug: bool,
    pub client_info: ClientInfo,
}

#[derive(Debug, Clone)]
pub enum CodexEvent {
    /// A JSON-RPC notification: the full `{method, params}` message.
    Notification(Value),
    /// A server-initiated request the gateway answered automatically.
    ServerRequest(ServerRequestEvent),
    Warning(WarningEvent),
    /// Raw stdout line, emitted only in debug mode.
    Raw(String),
    /// The child process exited.
    Exited {
        code: Option<i32>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerRequestEvent {
    pub method: String,
    pub params: Value,
    pub handled: bool,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcErrorPayload>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RpcErrorPayload {
    pub code: i64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WarningEvent {
    #[serde(rename = "type")]
    pub warning_type: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone)]
pub struct CodexClient {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    options: SpawnOptions,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    pending: Mutex<HashMap<u64, PendingRequest>>,
    next_id: AtomicU64,
    closed: AtomicBool,
    kill: Mutex<Option<oneshot::Sender<()>>>,
    events: broadcast::Sender<CodexEvent>,
}

struct PendingRequest {
    method: String,
    tx: oneshot::Sender<Result<Value, AppError>>,
}

impl CodexClient {
    pub fn spawn(options: SpawnOptions) -> Result<Self, AppError> {
        info!(
            label = %options.label,
            codex_bin = %options.codex_bin,
            cwd = %options.cwd.display(),
            "spawning codex app-server"
        );

        let mut command = Command::new(&options.codex_bin);
        command
            .arg("app-server")
            .args(&options.args)
            .current_dir(&options.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (name, value) in &options.env {
            command.env(name, value);
        }

        let mut child = command.spawn().map_err(|error| {
            AppError::internal(format!(
                "Failed to start {} app-server: {error}",
                options.codex_bin
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
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| AppError::internal("Failed to capture app-server stderr"))?;

        let (events, _) = broadcast::channel(256);
        let (kill_tx, kill_rx) = oneshot::channel();
        let client = Self {
            inner: Arc::new(ClientInner {
                options,
                stdin: tokio::sync::Mutex::new(Some(stdin)),
                pending: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                closed: AtomicBool::new(false),
                kill: Mutex::new(Some(kill_tx)),
                events,
            }),
        };

        client.spawn_stdout_task(stdout);
        client.spawn_stderr_task(stderr);
        client.spawn_wait_task(child, kill_rx);
        Ok(client)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CodexEvent> {
        self.inner.events.subscribe()
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    /// Run the `initialize` handshake.
    pub async fn initialize(&self) -> Result<Value, AppError> {
        let result = self
            .request(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": self.inner.options.client_info.name,
                        "title": self.inner.options.client_info.title,
                        "version": self.inner.options.client_info.version,
                    }
                }),
            )
            .await?;
        self.send_json(&json!({ "method": "initialized", "params": {} }))
            .await?;
        Ok(result)
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value, AppError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().unwrap().insert(
            id,
            PendingRequest {
                method: method.to_string(),
                tx,
            },
        );

        let message = json!({ "method": method, "id": id, "params": params });
        if let Err(error) = self.send_json(&message).await {
            self.inner.pending.lock().unwrap().remove(&id);
            error!(
                label = %self.inner.options.label,
                app_server_method = %method,
                error = %error,
                "failed to send app-server request"
            );
            return Err(error);
        }

        match rx.await {
            Ok(result) => {
                if let Err(error) = &result {
                    warn!(
                        label = %self.inner.options.label,
                        app_server_method = %method,
                        error = %error,
                        "app-server request failed"
                    );
                }
                result
            }
            Err(_) => Err(AppError::ChannelClosed),
        }
    }

    pub async fn stop(&self) {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        info!(label = %self.inner.options.label, "stopping codex app-server");
        self.fail_pending("app-server is shutting down");
        self.inner.stdin.lock().await.take();
        if let Some(kill) = self.inner.kill.lock().unwrap().take() {
            let _ = kill.send(());
        }
    }

    async fn send_json(&self, message: &Value) -> Result<(), AppError> {
        if self.is_closed() {
            return Err(AppError::internal("app-server process is not available"));
        }

        let mut guard = self.inner.stdin.lock().await;
        let Some(stdin) = guard.as_mut() else {
            return Err(AppError::internal("app-server process is not available"));
        };

        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        stdin.write_all(&line).await?;
        stdin.flush().await?;
        Ok(())
    }

    fn spawn_stdout_task(&self, stdout: tokio::process::ChildStdout) {
        let client = self.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => client.handle_line(line).await,
                    Ok(None) => break,
                    Err(error) => {
                        client.emit_warning(
                            "stdio-read-error",
                            "Failed to read app-server message",
                            Some(error.to_string()),
                        );
                        break;
                    }
                }
            }
        });
    }

    fn spawn_stderr_task(&self, stderr: tokio::process::ChildStderr) {
        let label = self.inner.options.label.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if !line.trim().is_empty() {
                    warn!(label = %label, line = %line, "codex stderr");
                }
            }
        });
    }

    fn spawn_wait_task(&self, mut child: Child, kill: oneshot::Receiver<()>) {
        let client = self.clone();
        tokio::spawn(async move {
            let status = tokio::select! {
                status = child.wait() => status.ok(),
                _ = kill => {
                    let _ = child.start_kill();
                    child.wait().await.ok()
                }
            };

            let code = status.and_then(|status| status.code());
            let expected = client.inner.closed.swap(true, Ordering::SeqCst);
            client.fail_pending("app-server exited before replying");
            if !expected {
                warn!(
                    label = %client.inner.options.label,
                    code = code.unwrap_or(-1),
                    "codex app-server exited unexpectedly"
                );
                client.emit_warning(
                    "process-exit",
                    "codex app-server exited",
                    Some(format!("code={code:?}")),
                );
            }
            let _ = client.inner.events.send(CodexEvent::Exited { code });
        });
    }

    async fn handle_line(&self, line: String) {
        let message: Value = match serde_json::from_str(&line) {
            Ok(message) => message,
            Err(error) => {
                self.emit_warning(
                    "json-parse-error",
                    "Failed to parse app-server message",
                    Some(error.to_string()),
                );
                return;
            }
        };

        if self.inner.options.debug {
            debug!(label = %self.inner.options.label, payload = %line, "raw app-server message");
            let _ = self.inner.events.send(CodexEvent::Raw(line));
        }

        let has_id = message.get("id").is_some();
        let has_method = message.get("method").and_then(Value::as_str).is_some();
        match (has_id, has_method) {
            (true, true) => self.handle_server_request(message).await,
            (true, false) => self.handle_response(message),
            (false, true) => {
                let _ = self.inner.events.send(CodexEvent::Notification(message));
            }
            (false, false) => {
                self.emit_warning(
                    "unknown-message",
                    "Received unknown app-server message shape",
                    Some(message.to_string()),
                );
            }
        }
    }

    fn handle_response(&self, message: Value) {
        let Some(id) = message.get("id").and_then(Value::as_u64) else {
            return;
        };
        let Some(pending) = self.inner.pending.lock().unwrap().remove(&id) else {
            self.emit_warning(
                "unexpected-response",
                &format!("Received response for unknown id={id}"),
                None,
            );
            return;
        };

        let result = if let Some(error) = message.get("error") {
            Err(AppError::Rpc {
                method: pending.method,
                code: error
                    .get("code")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
                message: error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown app-server error")
                    .to_string(),
            })
        } else {
            Ok(message.get("result").cloned().unwrap_or_else(|| json!({})))
        };
        let _ = pending.tx.send(result);
    }

    async fn handle_server_request(&self, message: Value) {
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

        let event = match approvals::auto_handle(&method, &params, &self.inner.options.cwd) {
            Some(handled) => {
                info!(
                    label = %self.inner.options.label,
                    app_server_method = %method,
                    status = %handled.status,
                    "auto-handled server request"
                );
                let response = json!({ "id": id, "result": handled.result });
                if let Err(error) = self.send_json(&response).await {
                    error!(
                        label = %self.inner.options.label,
                        app_server_method = %method,
                        error = %error,
                        "failed to answer server request"
                    );
                }
                ServerRequestEvent {
                    method,
                    params,
                    handled: true,
                    status: handled.status,
                    result: Some(handled.result.to_string()),
                    error: None,
                }
            }
            None => {
                let error = RpcErrorPayload {
                    code: -32601,
                    message: format!("Unsupported server request in codex-gateway: {method}"),
                };
                warn!(
                    label = %self.inner.options.label,
                    app_server_method = %method,
                    "rejected unsupported server request"
                );
                let response = json!({
                    "id": id,
                    "error": { "code": error.code, "message": error.message }
                });
                if let Err(send_error) = self.send_json(&response).await {
                    error!(
                        label = %self.inner.options.label,
                        app_server_method = %method,
                        error = %send_error,
                        "failed to reject server request"
                    );
                }
                ServerRequestEvent {
                    method,
                    params,
                    handled: false,
                    status: "rejected".to_string(),
                    result: None,
                    error: Some(error),
                }
            }
        };

        let _ = self.inner.events.send(CodexEvent::ServerRequest(event));
    }

    fn emit_warning(&self, warning_type: &str, message: &str, detail: Option<String>) {
        warn!(
            label = %self.inner.options.label,
            warning_type = %warning_type,
            detail = detail.as_deref().unwrap_or("-"),
            "{message}"
        );
        let _ = self.inner.events.send(CodexEvent::Warning(WarningEvent {
            warning_type: warning_type.to_string(),
            message: message.to_string(),
            detail,
        }));
    }

    fn fail_pending(&self, reason: &str) {
        let pending = {
            let mut guard = self.inner.pending.lock().unwrap();
            guard.drain().map(|(_, entry)| entry).collect::<Vec<_>>()
        };
        for entry in pending {
            let _ = entry.tx.send(Err(AppError::internal(format!(
                "{reason} before replying to {}",
                entry.method
            ))));
        }
    }
}
