use std::io::Write;
use std::time::Instant;

use serde_json::{Value, json};
use tokio::sync::oneshot;
use tracing::{debug, error, info};

use crate::error::AppError;
use crate::models::{BridgeEvent, WarningEvent};

use super::protocol::{is_response, is_server_request};
use super::{CodexAppServerBridge, PendingRequest};

impl CodexAppServerBridge {
    pub(super) async fn request(&self, method: &str, params: Value) -> Result<Value, AppError> {
        let id = self
            .inner
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        let started_at = Instant::now();

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
            error!(
                rpc_id = id,
                app_server_method = %method,
                error = %error,
                "failed to send app-server request"
            );
            return Err(error);
        }

        match rx.await {
            Ok(result) => match result {
                Ok(value) => {
                    info!(
                        rpc_id = id,
                        app_server_method = %method,
                        duration_ms = started_at.elapsed().as_millis() as u64,
                        "app-server request completed"
                    );
                    Ok(value)
                }
                Err(error) => {
                    error!(
                        rpc_id = id,
                        app_server_method = %method,
                        duration_ms = started_at.elapsed().as_millis() as u64,
                        error = %error,
                        "app-server request failed"
                    );
                    Err(error)
                }
            },
            Err(_) => {
                error!(
                    rpc_id = id,
                    app_server_method = %method,
                    duration_ms = started_at.elapsed().as_millis() as u64,
                    "app-server request channel closed"
                );
                Err(AppError::ChannelClosed)
            }
        }
    }

    pub(super) fn handle_line(&self, line: String) {
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
            debug!(payload = %line, "received raw app-server message");
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

    pub(super) fn send_json(&self, message: &Value) -> Result<(), AppError> {
        if self.inner.closed.load(std::sync::atomic::Ordering::SeqCst) {
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

    pub(super) fn fail_pending(&self, message: String) {
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
}
