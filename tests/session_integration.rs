use std::path::Path;
use std::time::Duration;

use codex_gateway::config::{AppConfig, ClientInfo};
use codex_gateway::session::{Session, SessionManager, StateSnapshot};

mod support;

use support::fake_codex;

#[tokio::test]
async fn session_runs_prompt_round_trip_against_fake_app_server() {
    let fake_codex = fake_codex::build();
    let manager = new_manager(fake_codex.binary());

    let session = manager
        .create_session(None, None)
        .await
        .expect("session is created");
    let initial = session.snapshot();
    assert!(initial.ready);
    assert_eq!(initial.selected_model.as_deref(), Some("fake-model"));
    assert_eq!(initial.thread_id.as_deref(), Some("thread-1"));
    assert!(!initial.active_turn);

    tokio::time::timeout(
        Duration::from_secs(5),
        session.send_prompt("hello fake app-server"),
    )
    .await
    .expect("turn/start returns")
    .expect("turn/start succeeds");

    let completed = wait_for_state(&session, |state| {
        !state.active_turn && state.last_turn_status.as_deref() == Some("completed")
    })
    .await;
    assert_eq!(completed.current_turn_id, None);

    manager
        .close_session(&session.id, "test-done")
        .await
        .expect("session closes");
}

#[tokio::test]
async fn session_keeps_completed_turn_status_when_completion_precedes_start_response() {
    let fake_codex = fake_codex::build_with_turn_notifications_before_response();
    let manager = new_manager(fake_codex.binary());

    let session = manager
        .create_session(None, None)
        .await
        .expect("session is created");
    session
        .send_prompt("hello fake app-server")
        .await
        .expect("turn/start succeeds");

    let completed = wait_for_state(&session, |state| {
        !state.active_turn && state.last_turn_status.as_deref() == Some("completed")
    })
    .await;

    assert_eq!(completed.last_turn_status.as_deref(), Some("completed"));
    assert_eq!(completed.current_turn_id, None);

    manager
        .close_session(&session.id, "test-done")
        .await
        .expect("session closes");
}

#[tokio::test]
async fn session_resumes_existing_thread() {
    let fake_codex = fake_codex::build();
    let manager = new_manager(fake_codex.binary());

    let session = manager
        .create_session(None, Some("thread-resume".to_string()))
        .await
        .expect("session resumes");
    let state = session.snapshot();

    assert!(state.ready);
    assert_eq!(state.thread_id.as_deref(), Some("thread-resume"));
    assert_eq!(state.selected_model.as_deref(), Some("fake-model"));
    assert!(!state.active_turn);

    manager
        .close_session(&session.id, "test-done")
        .await
        .expect("session closes");
}

fn new_manager(codex_bin: &Path) -> SessionManager {
    let config = AppConfig {
        host: "127.0.0.1".to_string(),
        port: 0,
        cwd: std::env::current_dir().expect("current dir"),
        codex_bin: codex_bin.display().to_string(),
        codex_home: None,
        openai_api_key: None,
        openai_base_url: None,
        default_model: None,
        debug: false,
        max_sessions: 4,
        session_ttl: Duration::from_secs(60),
        session_sweep_interval: Duration::from_secs(60),
        client_info: ClientInfo {
            name: "session_integration_test".to_string(),
            title: "Session Integration Test".to_string(),
            version: "test".to_string(),
        },
        auth: None,
        langfuse: None,
    };
    SessionManager::new(config, None)
}

async fn wait_for_state(
    session: &Session,
    predicate: impl Fn(&StateSnapshot) -> bool,
) -> StateSnapshot {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = session.snapshot();
            if predicate(&state) {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("state predicate is satisfied")
}
