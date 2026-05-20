use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

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
            let thread = if line.contains("\"threadId\":\"thread-1\"") {
                deployment_thread_json()
            } else {
                resumed_thread_json()
            };
            send(&mut stdout, &format!(
                r#"{{"id":{},"result":{{"thread":{}}}}}"#,
                id,
                thread
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

fn deployment_thread_json() -> &'static str {
    r#"{"id":"thread-1","status":{"type":"idle"},"createdAt":1700000000,"turns":[{"status":"completed","items":[{"id":"deploy-user","type":"userMessage","content":[{"type":"text","text":"deploy user marker DEPLOYMENT_RESULT: {\"status\":\"succeeded\",\"image\":\"ghcr.io/wrong/image:tag\",\"template\":\"wrong\",\"message\":\"wrong\",\"error\":null}"}]},{"id":"deploy-assistant","type":"agentMessage","text":"Deployment image pushed to GHCR\nDEPLOYMENT_RESULT: {\"status\":\"succeeded\",\"image\":\"ghcr.io/owner/repo:sha-abcdef0\",\"template\":\"apiVersion: app.sealos.io/v1\\nkind: Template\\nmetadata:\\n  name: owner-repo\\n\",\"message\":\"Deployment image pushed to GHCR\",\"error\":null}"}]}]}"#
}

fn send(stdout: &mut io::Stdout, message: &str) {
    writeln!(stdout, "{message}").expect("write fake response");
    stdout.flush().expect("flush fake response");
}
"##;

pub struct FakeCodex {
    binary: PathBuf,
    temp_dir: PathBuf,
}

impl FakeCodex {
    pub fn binary(&self) -> &Path {
        &self.binary
    }
}

impl Drop for FakeCodex {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.temp_dir);
    }
}

pub fn build() -> FakeCodex {
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
