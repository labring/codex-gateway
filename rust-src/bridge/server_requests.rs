use chrono::Utc;
use serde_json::{Value, json};

use crate::models::{BridgeEvent, RpcErrorPayload, ServerRequestEvent, SummaryEvent};

use super::protocol::{compact_json, compact_json_pretty, dynamic_tool_text_response, preview};
use super::workspace_tools::workspace_dependency_report;
use super::{CodexAppServerBridge, HandledServerRequest};

impl CodexAppServerBridge {
    pub(super) fn handle_server_request(&self, message: Value) {
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

        if let Some(handled) = self.auto_handle_server_request(&method, &params) {
            let _ = self.send_json(&json!({
                "id": id,
                "result": handled.result
            }));
            self.record_summary_event(SummaryEvent {
                at: Utc::now().to_rfc3339(),
                event_type: "serverRequest".to_string(),
                method: Some(method.clone()),
                item_type: handled.item_type.clone(),
                item_id: None,
                status: Some(handled.status.clone()),
                text_preview: handled.text_preview.clone(),
            });
            if let Some(note) = handled.system_note.clone() {
                self.push_system_note(note);
            }
            let _ = self
                .inner
                .events
                .send(BridgeEvent::ServerRequest(ServerRequestEvent {
                    method,
                    params,
                    handled: true,
                    result: Some(compact_json(&handled.result)),
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

    fn auto_handle_server_request(
        &self,
        method: &str,
        params: &Value,
    ) -> Option<HandledServerRequest> {
        match method {
            "item/commandExecution/requestApproval" => {
                let decision = auto_approval_decision(params);
                Some(HandledServerRequest {
                    result: json!({ "decision": decision }),
                    item_type: Some("commandExecution".to_string()),
                    status: "auto-accepted".to_string(),
                    text_preview: preview(params.get("reason"))
                        .or_else(|| preview(params.get("command")))
                        .or_else(|| preview(params.get("cwd"))),
                    system_note: Some(format!("Auto-accepted {method} in the gateway.")),
                })
            }
            "item/fileChange/requestApproval" => {
                let decision = auto_approval_decision(params);
                Some(HandledServerRequest {
                    result: json!({ "decision": decision }),
                    item_type: Some("fileChange".to_string()),
                    status: "auto-accepted".to_string(),
                    text_preview: preview(params.get("reason"))
                        .or_else(|| preview(params.get("grantRoot"))),
                    system_note: Some(format!("Auto-accepted {method} in the gateway.")),
                })
            }
            "item/permissions/requestApproval" => Some(HandledServerRequest {
                result: json!({
                    "permissions": params.get("permissions").cloned().unwrap_or_else(|| json!({})),
                    "scope": "session"
                }),
                item_type: Some("permissions".to_string()),
                status: "auto-accepted".to_string(),
                text_preview: preview(params.get("reason")),
                system_note: Some(format!("Auto-accepted {method} in the gateway.")),
            }),
            "execCommandApproval" => Some(HandledServerRequest {
                result: json!({ "decision": "approved_for_session" }),
                item_type: Some("commandExecution".to_string()),
                status: "auto-accepted".to_string(),
                text_preview: preview(params.get("reason")).or_else(|| preview(params.get("cwd"))),
                system_note: Some(format!("Auto-accepted {method} in the gateway.")),
            }),
            "applyPatchApproval" => Some(HandledServerRequest {
                result: json!({ "decision": "approved_for_session" }),
                item_type: Some("fileChange".to_string()),
                status: "auto-accepted".to_string(),
                text_preview: preview(params.get("reason"))
                    .or_else(|| preview(params.get("grantRoot"))),
                system_note: Some(format!("Auto-accepted {method} in the gateway.")),
            }),
            "item/tool/call" => Some(self.handle_dynamic_tool_call(params)),
            "item/tool/requestUserInput" => Some(self.handle_tool_user_input_request(params)),
            "mcpServer/elicitation/request" => Some(self.handle_mcp_elicitation_request(params)),
            _ => None,
        }
    }

    fn handle_dynamic_tool_call(&self, params: &Value) -> HandledServerRequest {
        let tool = params
            .get("tool")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));

        let (result, note) = match tool {
            "load_workspace_dependencies" => {
                let report = workspace_dependency_report(&self.inner.cwd);
                (
                    dynamic_tool_text_response(true, compact_json_pretty(&report)),
                    None,
                )
            }
            "install_workspace_dependencies" => {
                let report = workspace_dependency_report(&self.inner.cwd);
                let text = format!(
                    "codex-gateway does not provide the desktop thread dependency installer. \
Return to the caller and use system runtimes directly if possible.\n\nDetected runtime:\n{}",
                    compact_json_pretty(&report)
                );
                (
                    dynamic_tool_text_response(false, text),
                    Some(
                        "install_workspace_dependencies is not available in codex-gateway; returned a structured tool failure instead of stalling the turn."
                            .to_string(),
                    ),
                )
            }
            _ => {
                let text = format!(
                    "Unsupported tool call in codex-gateway: {tool}. Arguments: {}",
                    compact_json(&arguments)
                );
                (
                    dynamic_tool_text_response(false, text),
                    Some(format!(
                        "Tool {tool} is not implemented by codex-gateway; returned a structured tool failure."
                    )),
                )
            }
        };

        let success = result
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        HandledServerRequest {
            result,
            item_type: Some("tool".to_string()),
            status: if success {
                "completed".to_string()
            } else {
                "failed".to_string()
            },
            text_preview: Some(format!("tool={tool} success={success}")),
            system_note: note,
        }
    }

    fn handle_tool_user_input_request(&self, params: &Value) -> HandledServerRequest {
        let answers = params
            .get("questions")
            .and_then(Value::as_array)
            .map(|questions| {
                questions
                    .iter()
                    .filter_map(|question| {
                        question.get("id").and_then(Value::as_str).map(|id| {
                            (
                                id.to_string(),
                                json!({
                                    "answers": []
                                }),
                            )
                        })
                    })
                    .collect::<serde_json::Map<String, Value>>()
            })
            .unwrap_or_default();

        HandledServerRequest {
            result: json!({ "answers": answers }),
            item_type: Some("toolUserInput".to_string()),
            status: "auto-empty".to_string(),
            text_preview: Some(
                "Returned empty answers because gateway has no interactive UI".to_string(),
            ),
            system_note: Some(
                "A tool requested user input, but codex-gateway has no interactive prompt UI; empty answers were returned."
                    .to_string(),
            ),
        }
    }

    fn handle_mcp_elicitation_request(&self, params: &Value) -> HandledServerRequest {
        HandledServerRequest {
            result: json!({
                "action": "cancel",
                "content": Value::Null,
                "_meta": Value::Null
            }),
            item_type: Some("mcpElicitation".to_string()),
            status: "cancelled".to_string(),
            text_preview: preview(params.get("message")).or_else(|| preview(params.get("serverName"))),
            system_note: Some(
                "An MCP elicitation request was cancelled because codex-gateway has no form or browser UI."
                    .to_string(),
            ),
        }
    }
}

pub(super) fn auto_approval_decision(params: &Value) -> &'static str {
    if let Some(decisions) = params.get("availableDecisions").and_then(Value::as_array) {
        if decisions
            .iter()
            .any(|decision| decision.as_str() == Some("acceptForSession"))
        {
            return "acceptForSession";
        }
    }

    "accept"
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn auto_approval_prefers_accept_for_session_when_available() {
        let params = json!({
            "availableDecisions": ["decline", "acceptForSession", "accept"]
        });

        assert_eq!(auto_approval_decision(&params), "acceptForSession");
    }

    #[test]
    fn auto_approval_falls_back_to_accept() {
        let params = json!({
            "availableDecisions": ["decline", "accept"]
        });

        assert_eq!(auto_approval_decision(&params), "accept");
    }
}
