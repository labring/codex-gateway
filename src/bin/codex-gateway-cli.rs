//! Smoke-test CLI: starts one codex app-server, runs a single prompt, and
//! prints the final agent text. Useful to verify auth and model wiring
//! inside a Devbox without going through HTTP.

use std::env;
use std::time::Duration;

use serde_json::{Value, json};

use codex_gateway::codex::{CodexClient, CodexEvent, SpawnOptions, login_with_api_key};
use codex_gateway::config::AppConfig;
use codex_gateway::error::AppError;

const DEFAULT_PROMPT: &str = "Reply with exactly the single word pong. Do not call tools. Do not read files. Do not run commands. Do not use markdown.";

#[tokio::main]
async fn main() -> Result<(), AppError> {
    let prompt = env::args().skip(1).collect::<Vec<_>>().join(" ");
    let prompt = if prompt.trim().is_empty() {
        DEFAULT_PROMPT.to_string()
    } else {
        prompt.trim().to_string()
    };

    let config = AppConfig::from_env(env::current_dir()?);
    login_with_api_key(&config)?;

    let client = CodexClient::spawn(SpawnOptions {
        label: "cli".to_string(),
        cwd: config.cwd.clone(),
        codex_bin: config.codex_bin.clone(),
        args: config.codex_app_server_args(),
        env: config.codex_child_env(),
        debug: config.debug,
        client_info: config.client_info.clone(),
    })?;
    let mut events = client.subscribe();

    client.initialize().await?;
    println!("Initialized {} app-server", config.codex_bin);

    let model = match config.default_model.clone() {
        Some(model) => model,
        None => {
            let result = client
                .request("model/list", json!({ "limit": 50, "includeHidden": false }))
                .await?;
            result
                .pointer("/data/0/model")
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .ok_or_else(|| AppError::internal("model/list returned no models"))?
        }
    };

    let thread = client
        .request("thread/start", json!({ "cwd": config.cwd, "model": model }))
        .await?;
    let thread_id = thread
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::internal("thread/start did not return a thread id"))?
        .to_string();
    println!("Thread: {thread_id}\nModel: {model}\nPrompt: {prompt}");

    client
        .request(
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": [ { "type": "text", "text": prompt } ]
            }),
        )
        .await?;

    let mut agent_text = String::new();
    let wait = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            match events.recv().await {
                Ok(CodexEvent::Notification(message)) => {
                    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
                    println!("[notify] {method}");
                    if method == "item/completed"
                        && message.pointer("/params/item/type").and_then(Value::as_str)
                            == Some("agentMessage")
                        && let Some(text) =
                            message.pointer("/params/item/text").and_then(Value::as_str)
                    {
                        agent_text = text.to_string();
                    }
                    if method == "turn/completed" {
                        return Ok(());
                    }
                }
                Ok(CodexEvent::Warning(warning)) => {
                    println!("[warn] {}", warning.message);
                }
                Ok(CodexEvent::Exited { code }) => {
                    return Err(AppError::internal(format!(
                        "app-server exited before turn completion (code={code:?})"
                    )));
                }
                Ok(_) => {}
                Err(_) => return Err(AppError::ChannelClosed),
            }
        }
    })
    .await
    .map_err(|_| AppError::internal("Timed out waiting for turn completion"))?;
    wait?;

    println!("\nFinal agent text:\n\n{agent_text}");
    client.stop().await;
    Ok(())
}
