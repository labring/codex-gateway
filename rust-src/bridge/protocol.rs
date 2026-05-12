use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

pub(super) fn is_server_request(message: &Value) -> bool {
    message.get("id").is_some() && message.get("method").and_then(Value::as_str).is_some()
}

pub(super) fn is_response(message: &Value) -> bool {
    message.get("id").is_some() && message.get("method").is_none()
}

pub(super) fn dynamic_tool_text_response(success: bool, text: impl Into<String>) -> Value {
    json!({
        "success": success,
        "contentItems": [
            {
                "type": "inputText",
                "text": text.into()
            }
        ]
    })
}

pub(super) fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

pub(super) fn compact_json_pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| compact_json(value))
}

pub(super) fn preview(value: Option<&Value>) -> Option<String> {
    preview_string(value.and_then(Value::as_str)?)
}

pub(super) fn preview_string(value: &str) -> Option<String> {
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

pub(super) fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
