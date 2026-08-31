use std::io::Write;
use std::process::{Command, Stdio};

use tracing::info;

use crate::config::AppConfig;
use crate::error::AppError;

/// Run `codex login --with-api-key` at startup when an API key is configured,
/// so every app-server child starts authenticated. Returns `false` when no
/// key is configured.
pub fn login_with_api_key(config: &AppConfig) -> Result<bool, AppError> {
    let Some(api_key) = config.openai_api_key.as_deref() else {
        return Ok(false);
    };

    info!(
        base_url = config.openai_base_url.as_deref().unwrap_or("-"),
        "initializing codex auth from configured OpenAI API key"
    );

    let mut command = Command::new(&config.codex_bin);
    command
        .arg("login")
        .args(config.codex_config_args())
        .arg("--with-api-key")
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    for (name, value) in config.codex_child_env() {
        command.env(name, value);
    }

    let mut child = command.spawn().map_err(|error| {
        AppError::internal(format!(
            "Failed to start {} login: {error}",
            config.codex_bin
        ))
    })?;

    let stdin = child
        .stdin
        .as_mut()
        .ok_or_else(|| AppError::internal("Failed to access codex login stdin"))?;
    stdin.write_all(format!("{api_key}\n").as_bytes())?;

    let status = child.wait()?;
    if status.success() {
        return Ok(true);
    }

    Err(AppError::internal(format!(
        "{} login failed while reading the configured OpenAI API key (code={:?})",
        config.codex_bin,
        status.code()
    )))
}
