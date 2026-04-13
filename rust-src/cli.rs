use std::env;
use std::time::Duration;

use codex_gateway::bridge::{BridgeOptions, CodexAppServerBridge};
use codex_gateway::config::ClientInfo;
use codex_gateway::env_config::{
    CODEX_BIN_ENV, DEBUG_ENV, DEFAULT_MODEL_ENV, read_bool_flag, read_env,
};
use codex_gateway::error::AppError;
use codex_gateway::models::BridgeEvent;
use codex_gateway::runtime::maybe_login_with_api_key;

const DEFAULT_PROMPT: &str = "Reply with exactly the single word pong. Do not call tools. Do not read files. Do not run commands. Do not use markdown.";

#[tokio::main]
async fn main() -> Result<(), AppError> {
    let prompt = env::args().skip(1).collect::<Vec<_>>().join(" ");
    let prompt = if prompt.trim().is_empty() {
        DEFAULT_PROMPT.to_string()
    } else {
        prompt.trim().to_string()
    };

    let cwd = env::current_dir()?;
    let codex_bin = read_env(CODEX_BIN_ENV).unwrap_or_else(|| "codex".to_string());
    maybe_login_with_api_key(&codex_bin)?;

    let bridge = CodexAppServerBridge::new(BridgeOptions {
        cwd: cwd.clone(),
        codex_bin: codex_bin.clone(),
        debug: read_bool_flag(DEBUG_ENV),
        client_info: ClientInfo {
            name: "codex_gateway_cli".to_string(),
            title: "Codex Gateway CLI".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        default_model: read_env(DEFAULT_MODEL_ENV),
        activity_touch: std::sync::Arc::new(|| {}),
    });

    let mut receiver = bridge.subscribe();
    tokio::spawn(async move {
        loop {
            match receiver.recv().await {
                Ok(BridgeEvent::Notification(message)) => {
                    let method = message
                        .get("method")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("notification");
                    let params = message
                        .get("params")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let suffix = params
                        .get("item")
                        .and_then(|item| item.get("type"))
                        .and_then(serde_json::Value::as_str)
                        .map(|value| format!(" {value}"))
                        .or_else(|| {
                            params
                                .get("turn")
                                .and_then(|turn| turn.get("status"))
                                .and_then(serde_json::Value::as_str)
                                .map(|value| format!(" {value}"))
                        })
                        .unwrap_or_default();
                    println!("[notify] {method}{suffix}");
                }
                Ok(BridgeEvent::Warning(warning)) => {
                    println!("[warn] {}", warning.message);
                    if let Some(detail) = warning.detail {
                        println!("{detail}");
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

    let state = bridge.start().await?;
    println!("Starting {codex_bin} app-server from {}", state.cwd);
    println!("Initialized app-server");
    println!(
        "Runtime: {} / {}",
        state
            .runtime
            .platform_family
            .as_deref()
            .unwrap_or("unknown"),
        state.runtime.platform_os.as_deref().unwrap_or("unknown")
    );
    println!(
        "Account: {} | requiresOpenaiAuth={:?}",
        state.account.summary, state.account.requires_openai_auth
    );
    println!(
        "Selected model: {}",
        state.selected_model.as_deref().unwrap_or("unknown")
    );
    println!(
        "Thread: {}",
        state.thread_id.as_deref().unwrap_or("not started")
    );
    println!("Prompt: {prompt}");

    bridge.send_prompt(&prompt).await?;
    bridge
        .wait_for_turn_completion(Duration::from_secs(120))
        .await?;

    println!("\nFinal agent text:\n");
    println!("{}", bridge.get_latest_assistant_text());

    bridge.stop().await?;
    Ok(())
}
