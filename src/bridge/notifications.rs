use chrono::Utc;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::models::{BridgeEvent, SummaryEvent, TranscriptEntry};

use super::CodexAppServerBridge;
use super::protocol::{compact_json, preview, unix_millis};
use super::transcript::{extract_delta_text, extract_user_text, trim_transcript};

impl CodexAppServerBridge {
    pub(super) fn handle_notification(&self, message: Value) {
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
                info!(
                    thread_id = self.get_state().thread_id.as_deref().unwrap_or("-"),
                    "received thread/started notification"
                );
            }
            "thread/status/changed" => {
                self.with_state(|state| {
                    state.thread_status = params.get("status").cloned();
                });
                info!(
                    thread_id = self.get_state().thread_id.as_deref().unwrap_or("-"),
                    status = %compact_json(&params.get("status").cloned().unwrap_or_else(|| json!({}))),
                    "received thread/status/changed notification"
                );
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
                info!(
                    thread_id = self.get_state().thread_id.as_deref().unwrap_or("-"),
                    turn_id = self.get_state().current_turn_id.as_deref().unwrap_or("-"),
                    "received turn/started notification"
                );
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
                info!(
                    thread_id = self.get_state().thread_id.as_deref().unwrap_or("-"),
                    status = self.get_state().last_turn_status.as_deref().unwrap_or("-"),
                    "received turn/completed notification"
                );
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
                warn!(error_message = %message, "received app-server error notification");
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
}

pub(super) fn summarize_notification(message: &Value) -> SummaryEvent {
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn summarize_notification_prefers_item_text_preview() {
        let message = json!({
            "method": "item/completed",
            "params": {
                "item": {
                    "id": "a1",
                    "type": "agentMessage",
                    "status": "completed",
                    "text": "Deployment image pushed to GHCR"
                }
            }
        });

        let summary = summarize_notification(&message);

        assert_eq!(summary.event_type, "notification");
        assert_eq!(summary.method.as_deref(), Some("item/completed"));
        assert_eq!(summary.item_type.as_deref(), Some("agentMessage"));
        assert_eq!(summary.item_id.as_deref(), Some("a1"));
        assert_eq!(summary.status.as_deref(), Some("completed"));
        assert_eq!(
            summary.text_preview.as_deref(),
            Some("Deployment image pushed to GHCR")
        );
    }
}
