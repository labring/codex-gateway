use serde_json::Value;

use crate::models::TranscriptEntry;

use super::MAX_TRANSCRIPT;
use super::protocol::unix_millis;

pub(super) fn extract_user_text(item: &Value) -> String {
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

pub(super) fn transcript_from_thread(thread: &Value) -> Vec<TranscriptEntry> {
    let created_at = thread
        .get("createdAt")
        .and_then(Value::as_i64)
        .map(|seconds| seconds.saturating_mul(1000))
        .unwrap_or_else(unix_millis);

    let mut transcript = Vec::new();
    let Some(turns) = thread.get("turns").and_then(Value::as_array) else {
        return transcript;
    };

    for turn in turns {
        let turn_status = turn
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("completed");
        let Some(items) = turn.get("items").and_then(Value::as_array) else {
            continue;
        };

        for item in items {
            let item_type = item.get("type").and_then(Value::as_str);
            match item_type {
                Some("userMessage") => {
                    let text = extract_user_text(item);
                    if text.is_empty() {
                        continue;
                    }
                    transcript.push(TranscriptEntry {
                        id: item
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        role: "user".to_string(),
                        text,
                        status: "completed".to_string(),
                        source: "thread/read".to_string(),
                        created_at,
                    });
                }
                Some("agentMessage") => {
                    let text = item
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if text.is_empty() {
                        continue;
                    }
                    transcript.push(TranscriptEntry {
                        id: item
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        role: "assistant".to_string(),
                        text,
                        status: turn_status.to_string(),
                        source: "thread/read".to_string(),
                        created_at,
                    });
                }
                _ => {}
            }
        }
    }

    trim_transcript(&mut transcript);
    transcript
}

pub(super) fn extract_delta_text(params: &Value) -> String {
    for key in ["delta", "text", "textDelta", "chunk", "content"] {
        if let Some(value) = params.get(key).and_then(Value::as_str) {
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }

    String::new()
}

pub(super) fn trim_transcript(transcript: &mut Vec<TranscriptEntry>) {
    if transcript.len() > MAX_TRANSCRIPT {
        let drain = transcript.len() - MAX_TRANSCRIPT;
        transcript.drain(0..drain);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn extract_delta_text_accepts_known_delta_keys() {
        assert_eq!(extract_delta_text(&json!({ "delta": "hello" })), "hello");
        assert_eq!(
            extract_delta_text(&json!({ "textDelta": "fallback" })),
            "fallback"
        );
        assert_eq!(
            extract_delta_text(&json!({ "content": "content text" })),
            "content text"
        );
    }

    #[test]
    fn extract_user_text_joins_text_content_parts() {
        let item = json!({
            "type": "userMessage",
            "content": [
                { "type": "text", "text": "first" },
                { "type": "image", "url": "ignored" },
                { "type": "text", "text": "second" }
            ]
        });

        assert_eq!(extract_user_text(&item), "first\nsecond");
    }

    #[test]
    fn transcript_from_thread_extracts_user_and_assistant_messages() {
        let thread = json!({
            "createdAt": 1_700_000_000_i64,
            "turns": [
                {
                    "status": "completed",
                    "items": [
                        {
                            "id": "u1",
                            "type": "userMessage",
                            "content": [
                                { "type": "text", "text": "Deploy this repository" }
                            ]
                        },
                        {
                            "id": "a1",
                            "type": "agentMessage",
                            "text": "Deployment started"
                        },
                        {
                            "id": "cmd1",
                            "type": "commandExecution",
                            "command": "ignored"
                        }
                    ]
                }
            ]
        });

        let transcript = transcript_from_thread(&thread);

        assert_eq!(transcript.len(), 2);
        assert_eq!(transcript[0].id, "u1");
        assert_eq!(transcript[0].role, "user");
        assert_eq!(transcript[0].text, "Deploy this repository");
        assert_eq!(transcript[1].id, "a1");
        assert_eq!(transcript[1].role, "assistant");
        assert_eq!(transcript[1].text, "Deployment started");
        assert_eq!(transcript[1].status, "completed");
    }
}
