//! Maps session events onto the Langfuse data model.
//!
//! One trace per turn: the root span carries the user prompt as input and
//! the final agent message as output. Every completed codex item becomes a
//! child observation (command executions, file changes, tool calls, agent
//! messages), errors and auto-answered server requests become events, and
//! token usage lands on the root span.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use opentelemetry::trace::{Span, TraceContextExt, Tracer};
use opentelemetry::{Context, KeyValue};
use opentelemetry_sdk::trace::SdkTracer;
use serde_json::Value;
use tokio::sync::broadcast;
use tracing::warn;

use crate::config::LangfuseConfig;
use crate::session::{Session, SessionEvent};

use super::scrub::{scrub, scrub_and_truncate};

pub struct RecorderContext {
    tracer: SdkTracer,
    user_id: Option<String>,
    /// Langfuse session id: the Brain deploy task when available, otherwise
    /// the codex thread id (filled in per turn).
    session_id: Option<String>,
    project_id: Option<String>,
    gateway_session_id: String,
}

impl RecorderContext {
    pub fn new(tracer: SdkTracer, config: &LangfuseConfig, gateway_session_id: &str) -> Self {
        Self {
            tracer,
            user_id: config.user_id.clone(),
            session_id: config.session_id.clone(),
            project_id: config.project_id.clone(),
            gateway_session_id: gateway_session_id.to_string(),
        }
    }
}

pub fn spawn(session: &Arc<Session>, context: RecorderContext) {
    let receiver = session.subscribe();
    let session_id = session.id.clone();
    tokio::spawn(async move {
        Recorder::new(context).run(receiver, session_id).await;
    });
}

struct Recorder {
    context: RecorderContext,
    thread_id: Option<String>,
    model: Option<String>,
    turn: Option<TurnRecord>,
}

struct TurnRecord {
    cx: Context,
    open_items: HashMap<String, (SystemTime, Value)>,
    last_agent_text: Option<String>,
    usage: Option<Value>,
    failed: bool,
}

impl Recorder {
    fn new(context: RecorderContext) -> Self {
        Self {
            context,
            thread_id: None,
            model: None,
            turn: None,
        }
    }

    async fn run(mut self, mut receiver: broadcast::Receiver<SessionEvent>, session_id: String) {
        loop {
            match receiver.recv().await {
                Ok(event) => {
                    if !self.handle(event) {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(session_id = %session_id, skipped, "telemetry recorder lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
        self.close_turn("aborted");
    }

    /// Returns `false` once the session is closed.
    fn handle(&mut self, event: SessionEvent) -> bool {
        match event {
            SessionEvent::TurnSubmitted { prompt } => {
                self.close_turn("superseded");
                self.open_turn(Some(prompt));
            }
            SessionEvent::State(snapshot) => {
                self.thread_id = snapshot.thread_id.clone();
                self.model = snapshot.selected_model.clone();
            }
            SessionEvent::Notification(message) => self.handle_notification(&message),
            SessionEvent::ServerRequest(payload) => {
                let method = payload
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let status = payload
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("handled");
                let handled = payload.get("handled").and_then(Value::as_bool) == Some(true);
                self.record_event(
                    &format!("server-request:{method}"),
                    if handled { "DEFAULT" } else { "WARNING" },
                    Some(status.to_string()),
                    Some(compact(&payload)),
                );
            }
            SessionEvent::Warning(payload) => {
                let message = payload
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("gateway warning");
                self.record_event("warning", "WARNING", Some(message.to_string()), None);
            }
            SessionEvent::Raw(_) => {}
            SessionEvent::Closed { reason } => {
                self.close_turn(&format!("aborted:{reason}"));
                return false;
            }
        }
        true
    }

    fn handle_notification(&mut self, message: &Value) {
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        match method {
            "turn/started" => {
                if self.turn.is_none() {
                    // Turn recovered after a session resume; the prompt is
                    // not visible to this gateway process.
                    self.open_turn(None);
                }
            }
            "item/started" => {
                if let (Some(turn), Some(id)) = (
                    self.turn.as_mut(),
                    params.pointer("/item/id").and_then(Value::as_str),
                ) {
                    let item = params.get("item").cloned().unwrap_or(Value::Null);
                    turn.open_items
                        .insert(id.to_string(), (SystemTime::now(), item));
                }
            }
            "item/completed" => self.record_item(params.get("item").unwrap_or(&Value::Null)),
            "turn/completed" => {
                let status = params
                    .pointer("/turn/status")
                    .and_then(Value::as_str)
                    .unwrap_or("completed")
                    .to_string();
                if let Some(error) = params.pointer("/turn/error")
                    && !error.is_null()
                {
                    self.record_event(
                        "turn-error",
                        "ERROR",
                        error
                            .get("message")
                            .and_then(Value::as_str)
                            .map(ToString::to_string),
                        Some(compact(error)),
                    );
                }
                self.close_turn(&status);
            }
            "error" => {
                let message = params
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .or_else(|| params.get("message").and_then(Value::as_str))
                    .unwrap_or("app-server error");
                if let Some(turn) = self.turn.as_mut() {
                    turn.failed = true;
                }
                self.record_event("error", "ERROR", Some(message.to_string()), None);
            }
            "thread/tokenUsage/updated" => {
                if let Some(turn) = self.turn.as_mut() {
                    turn.usage = params
                        .get("tokenUsage")
                        .or_else(|| params.get("usage"))
                        .cloned();
                }
            }
            _ => {}
        }
    }

    fn open_turn(&mut self, prompt: Option<String>) {
        let now = SystemTime::now();
        let mut attributes = self.common_attributes();
        attributes.push(KeyValue::new("langfuse.trace.name", "codex-turn"));
        if let Some(prompt) = &prompt {
            attributes.push(KeyValue::new(
                "langfuse.observation.input",
                scrub_and_truncate(prompt),
            ));
        }
        if let Some(project_id) = &self.context.project_id {
            attributes.push(KeyValue::new(
                "langfuse.trace.metadata.projectId",
                project_id.clone(),
            ));
        }
        attributes.push(KeyValue::new(
            "langfuse.trace.metadata.gatewaySessionId",
            self.context.gateway_session_id.clone(),
        ));
        if let Some(thread_id) = &self.thread_id {
            attributes.push(KeyValue::new(
                "langfuse.trace.metadata.threadId",
                thread_id.clone(),
            ));
        }

        let span = self
            .context
            .tracer
            .span_builder("codex-turn")
            .with_start_time(now)
            .with_attributes(attributes)
            .start(&self.context.tracer);
        self.turn = Some(TurnRecord {
            cx: Context::new().with_span(span),
            open_items: HashMap::new(),
            last_agent_text: None,
            usage: None,
            failed: false,
        });
    }

    fn close_turn(&mut self, status: &str) {
        let Some(turn) = self.turn.take() else {
            return;
        };
        let span = turn.cx.span();
        span.set_attribute(KeyValue::new("codex.turn.status", status.to_string()));
        if let Some(text) = &turn.last_agent_text {
            span.set_attribute(KeyValue::new(
                "langfuse.observation.output",
                scrub_and_truncate(text),
            ));
        }
        if let Some(usage) = turn.usage.as_ref().and_then(usage_details) {
            span.set_attribute(KeyValue::new("langfuse.observation.usage_details", usage));
        }
        if turn.failed || status == "failed" {
            span.set_attribute(KeyValue::new("langfuse.observation.level", "ERROR"));
            span.set_attribute(KeyValue::new(
                "langfuse.observation.status_message",
                status.to_string(),
            ));
        }
        span.end_with_timestamp(SystemTime::now());
    }

    fn record_item(&mut self, item: &Value) {
        let Some(item_type) = item.get("type").and_then(Value::as_str) else {
            return;
        };
        if item_type == "userMessage" {
            // The prompt is already the trace input.
            return;
        }

        let item_id = item.get("id").and_then(Value::as_str).unwrap_or("");
        let model = self.model.clone();
        let Some(turn) = self.turn.as_mut() else {
            return;
        };
        let started_at = turn
            .open_items
            .remove(item_id)
            .map(|(started_at, _)| started_at)
            .unwrap_or_else(SystemTime::now);

        if item_type == "agentMessage"
            && let Some(text) = item.get("text").and_then(Value::as_str)
        {
            turn.last_agent_text = Some(text.to_string());
        }

        let spec = item_span_spec(item_type, item, model.as_deref());
        let mut attributes = self.common_attributes();
        attributes.push(KeyValue::new(
            "langfuse.observation.type",
            spec.observation_type,
        ));
        if let Some(input) = spec.input {
            attributes.push(KeyValue::new(
                "langfuse.observation.input",
                scrub_and_truncate(&input),
            ));
        }
        if let Some(output) = spec.output {
            attributes.push(KeyValue::new(
                "langfuse.observation.output",
                scrub_and_truncate(&output),
            ));
        }
        if let Some(model) = spec.model {
            attributes.push(KeyValue::new("langfuse.observation.model.name", model));
        }
        if let Some(status) = item.get("status").and_then(Value::as_str) {
            attributes.push(KeyValue::new(
                "langfuse.observation.metadata.status",
                status.to_string(),
            ));
            if status == "failed" || status == "declined" {
                attributes.push(KeyValue::new("langfuse.observation.level", "ERROR"));
            }
        }

        let turn = self.turn.as_ref().expect("turn is open");
        let mut span = self
            .context
            .tracer
            .span_builder(spec.name)
            .with_start_time(started_at)
            .with_attributes(attributes)
            .start_with_context(&self.context.tracer, &turn.cx);
        span.end_with_timestamp(SystemTime::now());
    }

    fn record_event(
        &mut self,
        name: &str,
        level: &str,
        status_message: Option<String>,
        detail: Option<String>,
    ) {
        let Some(turn) = self.turn.as_ref() else {
            return;
        };
        let mut attributes = self.common_attributes();
        attributes.push(KeyValue::new("langfuse.observation.type", "event"));
        attributes.push(KeyValue::new(
            "langfuse.observation.level",
            level.to_string(),
        ));
        if let Some(message) = status_message {
            attributes.push(KeyValue::new(
                "langfuse.observation.status_message",
                scrub(&message),
            ));
        }
        if let Some(detail) = detail {
            attributes.push(KeyValue::new(
                "langfuse.observation.input",
                scrub_and_truncate(&detail),
            ));
        }

        let now = SystemTime::now();
        let mut span = self
            .context
            .tracer
            .span_builder(name.to_string())
            .with_start_time(now)
            .with_attributes(attributes)
            .start_with_context(&self.context.tracer, &turn.cx);
        span.end_with_timestamp(now);
    }

    /// Attributes Langfuse expects on every span so session/user filters
    /// work across observations.
    fn common_attributes(&self) -> Vec<KeyValue> {
        let mut attributes = Vec::new();
        let session_id = self
            .context
            .session_id
            .clone()
            .or_else(|| self.thread_id.clone())
            .unwrap_or_else(|| self.context.gateway_session_id.clone());
        attributes.push(KeyValue::new("langfuse.session.id", session_id));
        if let Some(user_id) = &self.context.user_id {
            attributes.push(KeyValue::new("langfuse.user.id", user_id.clone()));
        }
        attributes
    }
}

struct ItemSpanSpec {
    name: String,
    observation_type: &'static str,
    input: Option<String>,
    output: Option<String>,
    model: Option<String>,
}

fn item_span_spec(item_type: &str, item: &Value, model: Option<&str>) -> ItemSpanSpec {
    let text = |key: &str| {
        item.get(key)
            .and_then(Value::as_str)
            .map(ToString::to_string)
    };

    match item_type {
        "agentMessage" => ItemSpanSpec {
            name: "agent-message".to_string(),
            observation_type: "generation",
            input: None,
            output: text("text"),
            model: model.map(ToString::to_string),
        },
        "reasoning" => ItemSpanSpec {
            name: "reasoning".to_string(),
            observation_type: "span",
            input: None,
            output: text("text").or_else(|| text("summary")),
            model: None,
        },
        "commandExecution" => ItemSpanSpec {
            name: "command".to_string(),
            observation_type: "span",
            input: text("command"),
            output: text("aggregatedOutput").or_else(|| {
                item.get("exitCode")
                    .and_then(Value::as_i64)
                    .map(|code| format!("exit code {code}"))
            }),
            model: None,
        },
        "fileChange" => ItemSpanSpec {
            name: "file-change".to_string(),
            observation_type: "span",
            input: item.get("changes").map(compact),
            output: None,
            model: None,
        },
        "mcpToolCall" => {
            let server = item.get("server").and_then(Value::as_str).unwrap_or("mcp");
            let tool = item.get("tool").and_then(Value::as_str).unwrap_or("tool");
            ItemSpanSpec {
                name: format!("mcp:{server}/{tool}"),
                observation_type: "span",
                input: item.get("arguments").map(compact),
                output: item
                    .get("result")
                    .or_else(|| item.get("error"))
                    .map(compact),
                model: None,
            }
        }
        "webSearch" => ItemSpanSpec {
            name: "web-search".to_string(),
            observation_type: "span",
            input: text("query"),
            output: None,
            model: None,
        },
        other => ItemSpanSpec {
            name: other.to_string(),
            observation_type: "span",
            input: Some(compact(item)),
            output: None,
            model: None,
        },
    }
}

/// Extract Langfuse `usage_details` JSON from a codex token usage payload.
/// Codex reports camelCase token counts either flat or nested under `total`.
fn usage_details(token_usage: &Value) -> Option<String> {
    let source = if token_usage.get("inputTokens").is_some() {
        token_usage
    } else {
        token_usage.get("total").unwrap_or(token_usage)
    };

    let mut details = serde_json::Map::new();
    for (from, to) in [
        ("inputTokens", "input"),
        ("cachedInputTokens", "cached_input"),
        ("outputTokens", "output"),
        ("reasoningOutputTokens", "reasoning_output"),
        ("totalTokens", "total"),
    ] {
        if let Some(count) = source.get(from).and_then(Value::as_u64) {
            details.insert(to.to_string(), Value::from(count));
        }
    }
    if details.is_empty() {
        return None;
    }
    serde_json::to_string(&Value::Object(details)).ok()
}

fn compact(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn usage_details_maps_flat_token_usage() {
        let usage = json!({ "inputTokens": 10, "outputTokens": 5, "totalTokens": 15 });
        let details = usage_details(&usage).expect("details");
        let parsed: Value = serde_json::from_str(&details).expect("valid json");
        assert_eq!(parsed.get("input").and_then(Value::as_u64), Some(10));
        assert_eq!(parsed.get("output").and_then(Value::as_u64), Some(5));
        assert_eq!(parsed.get("total").and_then(Value::as_u64), Some(15));
    }

    #[test]
    fn usage_details_maps_nested_totals() {
        let usage = json!({ "total": { "inputTokens": 7, "totalTokens": 9 } });
        let details = usage_details(&usage).expect("details");
        assert!(details.contains("\"input\":7"));
        assert!(details.contains("\"total\":9"));
    }

    #[test]
    fn usage_details_skips_unknown_shapes() {
        assert_eq!(usage_details(&json!({ "other": true })), None);
    }

    #[test]
    fn command_items_map_to_spans_with_input_and_output() {
        let item = json!({
            "id": "c1",
            "type": "commandExecution",
            "command": "echo hi",
            "aggregatedOutput": "hi",
            "status": "completed"
        });
        let spec = item_span_spec("commandExecution", &item, None);
        assert_eq!(spec.name, "command");
        assert_eq!(spec.observation_type, "span");
        assert_eq!(spec.input.as_deref(), Some("echo hi"));
        assert_eq!(spec.output.as_deref(), Some("hi"));
    }

    #[test]
    fn agent_messages_map_to_generations() {
        let item = json!({ "id": "a1", "type": "agentMessage", "text": "done" });
        let spec = item_span_spec("agentMessage", &item, Some("gpt-5.5"));
        assert_eq!(spec.observation_type, "generation");
        assert_eq!(spec.output.as_deref(), Some("done"));
        assert_eq!(spec.model.as_deref(), Some("gpt-5.5"));
    }
}
