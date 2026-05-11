use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use codex_gateway::bridge::{BridgeOptions, CodexAppServerBridge};
use codex_gateway::config::ClientInfo;
use codex_gateway::models::BridgeStateSnapshot;

const FAKE_CODEX_SOURCE: &str = r##"
use std::io::{self, BufRead, Write};

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    if args.get(1).map(String::as_str) != Some("app-server") {
        eprintln!("fake codex only supports app-server");
        std::process::exit(2);
    }

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut lines = stdin.lock().lines();

    while let Some(line) = lines.next() {
        let line = line.expect("stdin line");
        if line.contains("\"method\":\"initialized\"") {
            continue;
        }

        let id = request_id(&line).unwrap_or(0);
        if line.contains("\"method\":\"initialize\"") {
            send(&mut stdout, &format!(
                r#"{{"id":{},"result":{{"platformFamily":"unix","platformOs":"fake-os","userAgent":"fake-codex/1.0"}}}}"#,
                id
            ));
        } else if line.contains("\"method\":\"account/read\"") {
            send(&mut stdout, &format!(
                r#"{{"id":{},"result":{{"account":{{"type":"chatgpt","email":"fake@example.com"}},"requiresOpenaiAuth":false}}}}"#,
                id
            ));
        } else if line.contains("\"method\":\"model/list\"") {
            send(&mut stdout, &format!(
                r#"{{"id":{},"result":{{"data":[{{"model":"fake-model","displayName":"Fake Model","isDefault":true,"hidden":false,"supportsPersonality":false,"inputModalities":["text"]}}]}}}}"#,
                id
            ));
        } else if line.contains("\"method\":\"thread/start\"") {
            send(&mut stdout, &format!(
                r#"{{"id":{},"result":{{"thread":{{"id":"thread-1"}}}}}}"#,
                id
            ));
        } else if line.contains("\"method\":\"turn/start\"") {
            send(&mut stdout, r#"{"id":900,"method":"item/commandExecution/requestApproval","params":{"command":"echo fake","reason":"integration test approval","availableDecisions":["decline","acceptForSession","accept"]}}"#);
            let approval = lines.next().expect("approval response").expect("approval line");
            if !approval.contains("\"decision\":\"acceptForSession\"") {
                eprintln!("unexpected approval response: {approval}");
                std::process::exit(3);
            }

            send(&mut stdout, &format!(
                r#"{{"id":{},"result":{{"turn":{{"id":"turn-1","status":"inProgress"}}}}}}"#,
                id
            ));
            send(&mut stdout, r#"{"method":"turn/started","params":{"turn":{"id":"turn-1","status":"inProgress"}}}"#);
            send(&mut stdout, r#"{"method":"item/started","params":{"item":{"id":"assistant-1","type":"agentMessage","text":""}}}"#);
            send(&mut stdout, r#"{"method":"item/agentMessage/delta","params":{"itemId":"assistant-1","delta":"fake "}}"#);
            send(&mut stdout, r#"{"method":"item/agentMessage/delta","params":{"itemId":"assistant-1","delta":"assistant reply"}}"#);
            send(&mut stdout, r#"{"method":"item/completed","params":{"item":{"id":"assistant-1","type":"agentMessage","status":"completed","text":"fake assistant reply"}}}"#);
            send(&mut stdout, r#"{"method":"turn/completed","params":{"turn":{"id":"turn-1","status":"completed"}}}"#);
        } else if line.contains("\"method\":\"thread/read\"") {
            send(&mut stdout, &format!(
                r#"{{"id":{},"result":{{"thread":{}}}}}"#,
                id,
                resumed_thread_json()
            ));
        } else if line.contains("\"method\":\"thread/resume\"") {
            send(&mut stdout, &format!(
                r#"{{"id":{},"result":{{"thread":{},"model":"fake-model"}}}}"#,
                id,
                resumed_thread_json()
            ));
        } else if line.contains("\"method\":\"thread/list\"") {
            send(&mut stdout, &format!(
                r#"{{"id":{},"result":{{"data":[{{"id":"thread-resume","name":"Resume fixture","updatedAt":1700000001}}],"nextCursor":null}}}}"#,
                id
            ));
        } else {
            send(&mut stdout, &format!(
                r#"{{"id":{},"error":{{"code":-32601,"message":"unsupported fake method"}}}}"#,
                id
            ));
        }
    }
}

fn request_id(line: &str) -> Option<u64> {
    let marker = "\"id\":";
    let start = line.find(marker)? + marker.len();
    let digits = line[start..]
        .chars()
        .take_while(|value| value.is_ascii_digit())
        .collect::<String>();
    digits.parse().ok()
}

fn resumed_thread_json() -> &'static str {
    r#"{"id":"thread-resume","status":{"type":"idle"},"createdAt":1700000000,"turns":[{"status":"completed","items":[{"id":"resume-user","type":"userMessage","content":[{"type":"text","text":"resumed user"}]},{"id":"resume-assistant","type":"agentMessage","text":"resumed assistant"}]}]}"#
}

fn send(stdout: &mut io::Stdout, message: &str) {
    writeln!(stdout, "{message}").expect("write fake response");
    stdout.flush().expect("flush fake response");
}
"##;

#[tokio::test]
async fn bridge_runs_prompt_round_trip_against_fake_app_server() {
    let fake_codex = build_fake_codex();
    let bridge = new_bridge(fake_codex.binary.clone());

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
    let fake_codex = build_fake_codex();
    let bridge = new_bridge(fake_codex.binary.clone());

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

struct FakeCodex {
    binary: PathBuf,
    temp_dir: PathBuf,
}

impl Drop for FakeCodex {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.temp_dir);
    }
}

fn build_fake_codex() -> FakeCodex {
    let temp_dir = std::env::temp_dir().join(format!(
        "codex-gateway-fake-codex-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos()
    ));
    fs::create_dir_all(&temp_dir).expect("create fake codex temp dir");

    let source = temp_dir.join("fake_codex.rs");
    let binary = temp_dir.join(format!("fake-codex{}", std::env::consts::EXE_SUFFIX));
    fs::write(&source, FAKE_CODEX_SOURCE).expect("write fake codex source");

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let output = Command::new(rustc)
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("run rustc for fake codex");
    if !output.status.success() {
        panic!(
            "failed to compile fake codex\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    FakeCodex { binary, temp_dir }
}
