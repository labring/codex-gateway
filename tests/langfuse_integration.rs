//! End-to-end check of the Langfuse export path: run a turn against the fake
//! codex app-server and assert that OTLP spans arrive at a fake Langfuse
//! endpoint with the right path, auth, and span payload.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

mod support;

use support::gateway::GatewayProcess;

#[derive(Debug, Clone)]
struct CapturedRequest {
    path: String,
    authorization: Option<String>,
    ingestion_version: Option<String>,
    body: Vec<u8>,
}

#[test]
fn gateway_exports_turn_traces_to_langfuse() {
    let (otlp_port, captured) = start_fake_otlp_server();

    let mut gateway = GatewayProcess::start_with_env(&[
        ("LANGFUSE_HOST", format!("http://127.0.0.1:{otlp_port}")),
        ("LANGFUSE_PUBLIC_KEY", "pk-lf-test".to_string()),
        ("LANGFUSE_SECRET_KEY", "sk-lf-test".to_string()),
        ("SEALAI_NAMESPACE", "ns-user-1".to_string()),
        ("SEALAI_DEPLOY_TASK_ID", "task-e2e-1".to_string()),
        ("SEALAI_PROJECT_ID", "project-e2e-1".to_string()),
        // Export quickly so the test does not wait for the 5s default.
        ("OTEL_BSP_SCHEDULE_DELAY", "200".to_string()),
    ]);

    let (status, created) = gateway.json_request("POST", "/api/sessions", Some("{}"));
    assert_eq!(status, 200);
    let session_id = created
        .get("sessionId")
        .and_then(Value::as_str)
        .expect("session id")
        .to_string();

    let (status, _) = gateway.json_request(
        "POST",
        &format!("/api/sessions/{session_id}/turn"),
        Some(r#"{"prompt":"deploy the ghp_abcdef1234567890 repo"}"#),
    );
    assert_eq!(status, 200);

    gateway.wait_for_json(&format!("/api/sessions/{session_id}/state"), |payload| {
        payload
            .pointer("/state/lastTurnStatus")
            .and_then(Value::as_str)
            == Some("completed")
    });

    let request = wait_for_export(&captured, Duration::from_secs(10));

    assert_eq!(request.path, "/api/public/otel/v1/traces");
    // echo -n "pk-lf-test:sk-lf-test" | base64
    assert_eq!(
        request.authorization.as_deref(),
        Some("Basic cGstbGYtdGVzdDpzay1sZi10ZXN0")
    );
    assert_eq!(request.ingestion_version.as_deref(), Some("4"));

    // Protobuf embeds strings verbatim: check the trace shape without a
    // protobuf decoder.
    let body = request.body.as_slice();
    assert!(contains(body, b"codex-turn"), "root span name exported");
    assert!(
        contains(body, b"langfuse.session.id") && contains(body, b"task-e2e-1"),
        "session id from SEALAI_DEPLOY_TASK_ID"
    );
    assert!(
        contains(body, b"langfuse.user.id") && contains(body, b"ns-user-1"),
        "user id from SEALAI_NAMESPACE"
    );
    assert!(
        contains(body, b"agent-message"),
        "agent message observation exported"
    );
    assert!(
        contains(body, b"fake assistant reply"),
        "turn output exported"
    );
    assert!(
        contains(body, b"ghp_[REDACTED]") && !contains(body, b"ghp_abcdef1234567890"),
        "secrets scrubbed from exported input"
    );
    assert!(
        contains(body, b"langfuse.observation.usage_details"),
        "token usage exported"
    );
}

fn start_fake_otlp_server() -> (u16, Arc<Mutex<Vec<CapturedRequest>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake otlp server");
    let port = listener.local_addr().expect("local addr").port();
    let captured = Arc::new(Mutex::new(Vec::new()));

    let requests = Arc::clone(&captured);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let requests = Arc::clone(&requests);
            std::thread::spawn(move || {
                let _ = handle_connection(stream, &requests);
            });
        }
    });

    (port, captured)
}

fn handle_connection(
    stream: std::net::TcpStream,
    requests: &Mutex<Vec<CapturedRequest>>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut stream = stream;

    loop {
        let mut request_line = String::new();
        if reader.read_line(&mut request_line)? == 0 {
            return Ok(());
        }
        let path = request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_string();

        let mut authorization = None;
        let mut ingestion_version = None;
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim().to_string();
            match name.to_ascii_lowercase().as_str() {
                "authorization" => authorization = Some(value),
                "x-langfuse-ingestion-version" => ingestion_version = Some(value),
                "content-length" => content_length = value.parse().unwrap_or(0),
                _ => {}
            }
        }

        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body)?;

        requests.lock().unwrap().push(CapturedRequest {
            path,
            authorization,
            ingestion_version,
            body,
        });

        stream.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/x-protobuf\r\nContent-Length: 0\r\n\r\n",
        )?;
        stream.flush()?;
    }
}

fn wait_for_export(
    captured: &Arc<Mutex<Vec<CapturedRequest>>>,
    timeout: Duration,
) -> CapturedRequest {
    let deadline = Instant::now() + timeout;
    loop {
        {
            let requests = captured.lock().unwrap();
            if let Some(request) = requests.iter().find(|request| !request.body.is_empty()) {
                return request.clone();
            }
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for OTLP export");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
