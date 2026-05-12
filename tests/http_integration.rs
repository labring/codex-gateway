use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

mod support;

use support::fake_codex;

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
            .pointer("/state/transcript")
            .and_then(Value::as_array)
            .is_some_and(|entries| {
                entries.iter().any(|entry| {
                    entry.get("role").and_then(Value::as_str) == Some("assistant")
                        && entry.get("text").and_then(Value::as_str) == Some("fake assistant reply")
                })
            })
    });
    assert_eq!(
        state
            .pointer("/state/lastTurnStatus")
            .and_then(Value::as_str),
        Some("completed")
    );

    let (status, threads) = gateway.json_request("GET", "/api/threads?limit=20", None);
    assert_eq!(status, 200);
    assert_eq!(
        threads.pointer("/threads/0/id").and_then(Value::as_str),
        Some("thread-resume")
    );

    let (status, thread) = gateway.json_request("GET", "/api/threads/thread-resume", None);
    assert_eq!(status, 200);
    assert_eq!(
        thread.pointer("/thread/id").and_then(Value::as_str),
        Some("thread-resume")
    );

    let (status, resumed) = gateway.json_request(
        "POST",
        &format!("/api/sessions/{session_id}/thread/resume"),
        Some(r#"{"threadId":"thread-resume"}"#),
    );
    assert_eq!(status, 200);
    assert_eq!(
        resumed.pointer("/state/threadId").and_then(Value::as_str),
        Some("thread-resume")
    );
    assert!(
        resumed
            .pointer("/state/transcript")
            .and_then(Value::as_array)
            .expect("resumed transcript")
            .iter()
            .any(|entry| {
                entry.get("role").and_then(Value::as_str) == Some("assistant")
                    && entry.get("text").and_then(Value::as_str) == Some("resumed assistant")
            })
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

struct GatewayProcess {
    child: Child,
    port: u16,
    codex_home: PathBuf,
    _fake_codex: fake_codex::FakeCodex,
}

impl GatewayProcess {
    fn start() -> Self {
        let fake_codex = fake_codex::build();
        let mut last_error = String::new();

        for _ in 0..5 {
            let port = free_port();
            let codex_home = temp_dir("codex-gateway-http-codex-home");
            fs::create_dir_all(&codex_home).expect("create codex home");

            let mut child = Command::new(env!("CARGO_BIN_EXE_codex-gateway"))
                .env("CODEX_GATEWAY_HOST", "127.0.0.1")
                .env("CODEX_GATEWAY_PORT", port.to_string())
                .env("CODEX_GATEWAY_CODEX_BIN", fake_codex.binary())
                .env("CODEX_GATEWAY_CWD", std::env::current_dir().expect("cwd"))
                .env("CODEX_GATEWAY_CODEX_HOME", &codex_home)
                .env("CODEX_GATEWAY_MAX_SESSIONS", "4")
                .env("CODEX_GATEWAY_SESSION_TTL_MS", "60000")
                .env("CODEX_GATEWAY_SESSION_SWEEP_INTERVAL_MS", "60000")
                .env_remove("CODEX_GATEWAY_MODEL")
                .env_remove("CODEX_GATEWAY_DEBUG")
                .env_remove("CODEX_GATEWAY_OPENAI_API_KEY")
                .env_remove("CODEX_GATEWAY_OPENAI_BASE_URL")
                .env_remove("CODEX_GATEWAY_JWT_SECRET")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn gateway");

            match wait_for_gateway(&mut child, port) {
                Ok(()) => {
                    return Self {
                        child,
                        port,
                        codex_home,
                        _fake_codex: fake_codex,
                    };
                }
                Err(error) => {
                    last_error = error;
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = fs::remove_dir_all(&codex_home);
                }
            }
        }

        panic!("failed to start gateway after retries: {last_error}");
    }

    fn json_request(&mut self, method: &str, path: &str, body: Option<&str>) -> (u16, Value) {
        let (status, body) = self.raw_request(method, path, body);
        let payload = serde_json::from_str(&body)
            .unwrap_or_else(|error| panic!("invalid json response: {error}\nbody:\n{body}"));
        (status, payload)
    }

    fn raw_request(&mut self, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
        if let Some(status) = self.child.try_wait().expect("gateway status") {
            panic!("gateway exited before request {method} {path}: {status}");
        }

        http_request(self.port, method, path, body).unwrap_or_else(|error| {
            panic!("http request failed for {method} {path}: {error}");
        })
    }

    fn wait_for_json(&mut self, path: &str, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let (status, payload) = self.json_request("GET", path, None);
            assert_eq!(status, 200);
            if predicate(&payload) {
                return payload;
            }
            if Instant::now() >= deadline {
                panic!("timed out waiting for {path}; last payload: {payload}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for GatewayProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.codex_home);
    }
}

fn wait_for_gateway(child: &mut Child, port: u16) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().expect("gateway status") {
            return Err(format!("gateway exited before readiness: {status}"));
        }

        if let Ok((status, body)) = http_request(port, "GET", "/healthz", None)
            && status == 200
            && serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|payload| payload.get("ok").and_then(Value::as_bool))
                == Some(true)
        {
            return Ok(());
        }

        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for gateway on port {port}"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn http_request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> std::io::Result<(u16, String)> {
    let body = body.unwrap_or("");
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;

    let mut parts = response.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or_default();
    let body = parts.next().unwrap_or_default().to_string();
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or_default();

    Ok((status, body))
}

fn free_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind free port");
    listener.local_addr().expect("local addr").port()
}

fn temp_dir(prefix: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos()
    ))
}
