use serde_json::Value;

mod support;

use support::gateway::GatewayProcess;

#[test]
fn gateway_session_thread_and_turn_http_flow_against_fake_app_server() {
    let mut gateway = GatewayProcess::start();

    let (status, health) = gateway.json_request("GET", "/healthz", None);
    assert_eq!(status, 200);
    assert_eq!(health.get("ok").and_then(Value::as_bool), Some(true));

    let (status, created) = gateway.json_request("POST", "/api/sessions", Some("{}"));
    assert_eq!(status, 200);
    assert_eq!(
        created
            .pointer("/state/selectedModel")
            .and_then(Value::as_str),
        Some("fake-model")
    );
    assert_eq!(
        created.pointer("/state/threadId").and_then(Value::as_str),
        Some("thread-1")
    );
    let session_id = created
        .get("sessionId")
        .and_then(Value::as_str)
        .expect("session id")
        .to_string();

    let (status, turn) = gateway.json_request(
        "POST",
        &format!("/api/sessions/{session_id}/turn"),
        Some(r#"{"prompt":"hello from http"}"#),
    );
    assert_eq!(status, 200);
    assert_eq!(turn.get("ok").and_then(Value::as_bool), Some(true));

    let state = gateway.wait_for_json(&format!("/api/sessions/{session_id}/state"), |payload| {
        payload
            .pointer("/state/lastTurnStatus")
            .and_then(Value::as_str)
            == Some("completed")
            && payload
                .pointer("/state/activeTurn")
                .and_then(Value::as_bool)
                == Some(false)
    });
    assert_eq!(
        state
            .pointer("/state/currentTurnId")
            .and_then(Value::as_str),
        None
    );

    // Brain recovers a lost session by re-creating one with `threadId`.
    let (status, resumed) = gateway.json_request(
        "POST",
        "/api/sessions",
        Some(r#"{"threadId":"thread-resume"}"#),
    );
    assert_eq!(status, 200);
    assert_eq!(
        resumed.pointer("/state/threadId").and_then(Value::as_str),
        Some("thread-resume")
    );
    let resumed_session_id = resumed
        .get("sessionId")
        .and_then(Value::as_str)
        .expect("resumed session id")
        .to_string();
    let (status, _) = gateway.json_request(
        "DELETE",
        &format!("/api/sessions/{resumed_session_id}"),
        None,
    );
    assert_eq!(status, 200);

    let (status, removed_route) = gateway.json_request("GET", "/api/threads?limit=20", None);
    assert_eq!(status, 404);
    assert_eq!(
        removed_route.get("error").and_then(Value::as_str),
        Some("Not found")
    );

    let (status, deleted) =
        gateway.json_request("DELETE", &format!("/api/sessions/{session_id}"), None);
    assert_eq!(status, 200);
    assert_eq!(deleted.get("ok").and_then(Value::as_bool), Some(true));

    let (status, missing) =
        gateway.json_request("GET", &format!("/api/sessions/{session_id}/state"), None);
    assert_eq!(status, 404);
    assert!(
        missing
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("Unknown session"))
    );
}
