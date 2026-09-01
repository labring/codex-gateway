//! Automatic answers for server-initiated requests.
//!
//! The gateway is headless and runs inside a disposable sandbox, so approval
//! requests are accepted and interactive prompts are answered with an empty
//! or cancelled response instead of stalling the turn.

use std::env;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{Map, Value, json};

pub(super) struct HandledServerRequest {
    pub result: Value,
    pub status: String,
}

pub(super) fn auto_handle(
    method: &str,
    params: &Value,
    cwd: &Path,
) -> Option<HandledServerRequest> {
    match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            Some(HandledServerRequest {
                result: json!({ "decision": approval_decision(params) }),
                status: "auto-accepted".to_string(),
            })
        }
        "item/permissions/requestApproval" => Some(HandledServerRequest {
            result: json!({
                "permissions": params.get("permissions").cloned().unwrap_or_else(|| json!({})),
                "scope": "session"
            }),
            status: "auto-accepted".to_string(),
        }),
        "execCommandApproval" | "applyPatchApproval" => Some(HandledServerRequest {
            result: json!({ "decision": "approved_for_session" }),
            status: "auto-accepted".to_string(),
        }),
        "item/tool/call" => Some(handle_tool_call(params, cwd)),
        "item/tool/requestUserInput" => Some(handle_user_input_request(params)),
        "mcpServer/elicitation/request" => Some(HandledServerRequest {
            result: json!({
                "action": "cancel",
                "content": Value::Null,
                "_meta": Value::Null
            }),
            status: "cancelled".to_string(),
        }),
        _ => None,
    }
}

fn approval_decision(params: &Value) -> &'static str {
    if let Some(decisions) = params.get("availableDecisions").and_then(Value::as_array)
        && decisions
            .iter()
            .any(|decision| decision.as_str() == Some("acceptForSession"))
    {
        return "acceptForSession";
    }
    "accept"
}

fn handle_tool_call(params: &Value, cwd: &Path) -> HandledServerRequest {
    let tool = params
        .get("tool")
        .and_then(Value::as_str)
        .unwrap_or("unknown");

    let (success, text) = match tool {
        "load_workspace_dependencies" => (true, workspace_report_text(cwd)),
        "install_workspace_dependencies" => (
            false,
            format!(
                "codex-gateway does not provide the desktop thread dependency installer. \
Return to the caller and use system runtimes directly if possible.\n\nDetected runtime:\n{}",
                workspace_report_text(cwd)
            ),
        ),
        _ => (
            false,
            format!(
                "Unsupported tool call in codex-gateway: {tool}. Arguments: {}",
                params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}))
            ),
        ),
    };

    HandledServerRequest {
        result: json!({
            "success": success,
            "contentItems": [
                { "type": "inputText", "text": text }
            ]
        }),
        status: if success { "completed" } else { "failed" }.to_string(),
    }
}

fn handle_user_input_request(params: &Value) -> HandledServerRequest {
    let answers = params
        .get("questions")
        .and_then(Value::as_array)
        .map(|questions| {
            questions
                .iter()
                .filter_map(|question| {
                    question
                        .get("id")
                        .and_then(Value::as_str)
                        .map(|id| (id.to_string(), json!({ "answers": [] })))
                })
                .collect::<Map<String, Value>>()
        })
        .unwrap_or_default();

    HandledServerRequest {
        result: json!({ "answers": answers }),
        status: "auto-empty".to_string(),
    }
}

fn workspace_report_text(cwd: &Path) -> String {
    let python = resolve_executable("python3").or_else(|| resolve_executable("python"));
    let python_modules = python
        .as_ref()
        .map(|python_bin| {
            json!({
                "docx": python_module_available(python_bin, "docx"),
                "openpyxl": python_module_available(python_bin, "openpyxl"),
                "pdfplumber": python_module_available(python_bin, "pdfplumber"),
                "pypdf": python_module_available(python_bin, "pypdf"),
                "pptx": python_module_available(python_bin, "pptx"),
            })
        })
        .unwrap_or(Value::Null);

    let report = json!({
        "runtime": "codex-gateway",
        "cwd": cwd.display().to_string(),
        "os": env::consts::OS,
        "arch": env::consts::ARCH,
        "executables": {
            "node": resolve_executable("node"),
            "npm": resolve_executable("npm"),
            "python": python,
            "cargo": resolve_executable("cargo"),
        },
        "pythonModules": python_modules,
        "note": "This report is generated by the gateway host runtime, not by the Codex desktop app thread dependency bundle."
    });
    serde_json::to_string_pretty(&report).unwrap_or_else(|_| report.to_string())
}

fn resolve_executable(name: &str) -> Option<String> {
    let paths = env::var_os("PATH")?;
    env::split_paths(&paths)
        .map(|path| path.join(name))
        .find(|candidate| candidate.is_file())
        .map(|candidate| candidate.display().to_string())
}

fn python_module_available(python_bin: &str, module: &str) -> bool {
    Command::new(python_bin)
        .arg("-c")
        .arg(format!("import {module}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn approval_prefers_accept_for_session_when_available() {
        let params = json!({
            "availableDecisions": ["decline", "acceptForSession", "accept"]
        });
        assert_eq!(approval_decision(&params), "acceptForSession");
    }

    #[test]
    fn approval_falls_back_to_accept() {
        let params = json!({ "availableDecisions": ["decline", "accept"] });
        assert_eq!(approval_decision(&params), "accept");
    }

    #[test]
    fn unsupported_tool_call_returns_structured_failure() {
        let handled = auto_handle(
            "item/tool/call",
            &json!({ "tool": "mystery", "arguments": { "a": 1 } }),
            std::path::Path::new("/tmp"),
        )
        .expect("tool call is handled");

        assert_eq!(handled.status, "failed");
        assert_eq!(
            handled.result.get("success").and_then(Value::as_bool),
            Some(false)
        );
    }

    #[test]
    fn unknown_server_request_is_not_handled() {
        assert!(auto_handle("mystery/method", &json!({}), std::path::Path::new("/tmp")).is_none());
    }
}
