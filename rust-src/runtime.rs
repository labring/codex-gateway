use std::io::Write;
use std::process::{Command, Stdio};

use thiserror::Error;

use crate::env_config::{OPENAI_API_KEY_ENV, OPENAI_BASE_URL_ENV, apply_codex_child_env, read_env};

const CUSTOM_PROVIDER_ID: &str = "OpenAI";

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub fn get_openai_base_url() -> Option<String> {
    read_env(OPENAI_BASE_URL_ENV)
}

pub fn get_codex_config_args() -> Vec<String> {
    let mut args = Vec::new();
    let base_url = get_openai_base_url();
    let has_api_key = read_env(OPENAI_API_KEY_ENV).is_some();

    if let Some(base_url) = base_url.as_deref() {
        args.extend(get_custom_provider_config_args(&base_url));
    }

    if base_url.is_some() || has_api_key {
        args.push("-c".to_string());
        args.push(r#"forced_login_method="api""#.to_string());
    }

    args
}

pub fn get_codex_app_server_config_args() -> Vec<String> {
    let mut args = get_codex_config_args();

    args.push("-c".to_string());
    args.push(format!(
        "sandbox_mode={}",
        toml_string("danger-full-access")
    ));
    args.push("-c".to_string());
    args.push(format!("approval_policy={}", toml_string("never")));

    args
}

pub fn maybe_login_with_api_key(codex_bin: &str) -> Result<bool, RuntimeError> {
    let Some(api_key) = read_env(OPENAI_API_KEY_ENV) else {
        return Ok(false);
    };

    let base_url = get_openai_base_url();
    let mut args = vec!["login".to_string()];
    args.extend(get_codex_config_args());
    args.push("--with-api-key".to_string());

    if let Some(base_url) = base_url {
        println!(
            "Initializing Codex auth from configured OpenAI API key with base URL override {base_url}"
        );
    } else {
        println!("Initializing Codex auth from configured OpenAI API key");
    }

    let mut child = Command::new(codex_bin);
    child
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    apply_codex_child_env(&mut child);

    let mut child = child.spawn().map_err(|error| {
        RuntimeError::Message(format!("Failed to start {codex_bin} login: {error}"))
    })?;

    let Some(stdin) = child.stdin.as_mut() else {
        return Err(RuntimeError::Message(format!(
            "Failed to access stdin for {codex_bin} login"
        )));
    };

    stdin.write_all(format!("{api_key}\n").as_bytes())?;

    let status = child.wait()?;
    if status.success() {
        return Ok(true);
    }

    Err(RuntimeError::Message(format!(
        "{codex_bin} login failed while reading the configured OpenAI API key (code={:?})",
        status.code()
    )))
}

fn get_custom_provider_config_args(base_url: &str) -> Vec<String> {
    let provider_path = format!("model_providers.{CUSTOM_PROVIDER_ID}");

    vec![
        "-c".to_string(),
        format!("model_provider={}", toml_string(CUSTOM_PROVIDER_ID)),
        "-c".to_string(),
        format!("{provider_path}.name={}", toml_string(CUSTOM_PROVIDER_ID)),
        "-c".to_string(),
        format!("{provider_path}.base_url={}", toml_string(base_url)),
        "-c".to_string(),
        format!("{provider_path}.wire_api={}", toml_string("responses")),
        "-c".to_string(),
        format!("{provider_path}.requires_openai_auth=true"),
        "-c".to_string(),
        format!("{provider_path}.supports_websockets=false"),
    ]
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("\"{value}\""))
}
