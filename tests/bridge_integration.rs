use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use codex_gateway::bridge::{BridgeOptions, CodexAppServerBridge};
use codex_gateway::config::ClientInfo;
use codex_gateway::models::BridgeStateSnapshot;

mod support;

use support::fake_codex;

#[tokio::test]
async fn bridge_runs_prompt_round_trip_against_fake_app_server() {
    let fake_codex = fake_codex::build();
    let bridge = new_bridge(fake_codex.binary().to_path_buf());

    let initial_state = bridge.start().await.expect("bridge starts");
    assert!(initial_state.ready);
    assert_eq!(
        initial_state.runtime.user_agent.as_deref(),
        Some("fake-codex/1.0")
    );
    assert_eq!(initial_state.account.summary, "chatgpt:fake@example.com");
    assert_eq!(initial_state.selected_model.as_deref(), Some("fake-model"));
    assert_eq!(initial_state.thread_id.as_deref(), Some("thread-1"));

    tokio::time::timeout(
        Duration::from_secs(5),
        bridge.send_prompt("hello fake app-server"),
    )
    .await
    .expect("turn/start returns")
    .expect("turn/start succeeds");

    let turn = bridge
        .wait_for_turn_completion(Duration::from_secs(5))
        .await
        .expect("turn completion is observed");
    assert_eq!(
        turn.get("status").and_then(serde_json::Value::as_str),
        Some("completed")
    );

    let completed = wait_for_state(&bridge, |state| {
        !state.active_turn
            && state
                .transcript
                .iter()
                .any(|entry| entry.role == "assistant" && entry.text == "fake assistant reply")
    })
    .await;

    assert_eq!(completed.last_turn_status.as_deref(), Some("completed"));
    assert!(completed.transcript.iter().any(|entry| {
        entry.role == "user" && entry.text == "hello fake app-server" && entry.source == "local"
    }));
    assert!(completed.recent_events.iter().any(|event| {
        event.method.as_deref() == Some("item/commandExecution/requestApproval")
            && event.status.as_deref() == Some("auto-accepted")
    }));
    assert_eq!(bridge.get_latest_assistant_text(), "fake assistant reply");

    bridge.stop().await.expect("bridge stops");
}

#[tokio::test]
async fn bridge_reads_and_resumes_thread_history_against_fake_app_server() {
    let fake_codex = fake_codex::build();
    let bridge = new_bridge(fake_codex.binary().to_path_buf());

    let initial_state = bridge
        .start_without_thread()
        .await
        .expect("bridge starts without thread");
    assert!(initial_state.ready);
    assert_eq!(initial_state.thread_id, None);

    let raw_thread = bridge
        .read_thread("thread-resume")
        .await
        .expect("thread read");
    assert_eq!(
        raw_thread
            .get("thread")
            .and_then(|thread| thread.get("id"))
            .and_then(serde_json::Value::as_str),
        Some("thread-resume")
    );

    bridge
        .resume_thread("thread-resume")
        .await
        .expect("thread resumes");
    let state = bridge.get_state();

    assert_eq!(state.thread_id.as_deref(), Some("thread-resume"));
    assert_eq!(state.selected_model.as_deref(), Some("fake-model"));
    assert_eq!(state.active_turn, false);
    assert!(state.transcript.iter().any(|entry| {
        entry.id == "resume-user" && entry.role == "user" && entry.text == "resumed user"
    }));
    assert!(state.transcript.iter().any(|entry| {
        entry.id == "resume-assistant"
            && entry.role == "assistant"
            && entry.text == "resumed assistant"
    }));

    bridge.stop().await.expect("bridge stops");
}

fn new_bridge(codex_bin: PathBuf) -> CodexAppServerBridge {
    CodexAppServerBridge::new(BridgeOptions {
        cwd: std::env::current_dir().expect("current dir"),
        codex_bin: codex_bin.display().to_string(),
        debug: false,
        client_info: ClientInfo {
            name: "bridge_integration_test".to_string(),
            title: "Bridge Integration Test".to_string(),
            version: "test".to_string(),
        },
        default_model: None,
        activity_touch: Arc::new(|| {}),
    })
}

async fn wait_for_state(
    bridge: &CodexAppServerBridge,
    predicate: impl Fn(&BridgeStateSnapshot) -> bool,
) -> BridgeStateSnapshot {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = bridge.get_state();
            if predicate(&state) {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("state predicate is satisfied")
}
