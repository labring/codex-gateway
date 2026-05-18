use std::env;
use std::process::Command;

pub const HOST_ENV: &[&str] = &["CODEX_GATEWAY_HOST"];
pub const PORT_ENV: &[&str] = &["CODEX_GATEWAY_PORT"];
pub const BRIDGE_CWD_ENV: &[&str] = &["CODEX_GATEWAY_CWD"];
pub const CODEX_BIN_ENV: &[&str] = &["CODEX_GATEWAY_CODEX_BIN"];
pub const DEBUG_ENV: &[&str] = &["CODEX_GATEWAY_DEBUG"];
pub const DEFAULT_MODEL_ENV: &[&str] = &["CODEX_GATEWAY_MODEL"];
pub const MAX_SESSIONS_ENV: &[&str] = &["CODEX_GATEWAY_MAX_SESSIONS"];
pub const MAX_DEPLOYMENTS_ENV: &[&str] = &["CODEX_GATEWAY_MAX_DEPLOYMENTS"];
pub const SESSION_TTL_MS_ENV: &[&str] = &["CODEX_GATEWAY_SESSION_TTL_MS"];
pub const DEPLOYMENT_TIMEOUT_MS_ENV: &[&str] = &["CODEX_GATEWAY_DEPLOYMENT_TIMEOUT_MS"];
pub const SESSION_SWEEP_INTERVAL_MS_ENV: &[&str] = &["CODEX_GATEWAY_SESSION_SWEEP_INTERVAL_MS"];
pub const OPENAI_API_KEY_ENV: &[&str] = &["CODEX_GATEWAY_OPENAI_API_KEY"];
pub const OPENAI_BASE_URL_ENV: &[&str] = &["CODEX_GATEWAY_OPENAI_BASE_URL"];
pub const CODEX_HOME_ENV: &[&str] = &["CODEX_GATEWAY_CODEX_HOME"];
pub const JWT_SECRET_ENV: &[&str] = &["CODEX_GATEWAY_JWT_SECRET"];
pub const SESSION_RUNTIME_ENV: &[&str] = &["CODEX_GATEWAY_SESSION_RUNTIME"];
pub const DEVBOX_BASE_URL_ENV: &[&str] = &["CODEX_GATEWAY_DEVBOX_BASE_URL"];
pub const SEALOS_HOST_ENV: &[&str] = &["SEALOS_HOST"];
pub const DEVBOX_TOKEN_ENV: &[&str] = &["CODEX_GATEWAY_DEVBOX_TOKEN", "DEVBOX_TOKEN"];
pub const DEVBOX_JWT_SIGNING_KEY_ENV: &[&str] = &[
    "CODEX_GATEWAY_DEVBOX_JWT_SIGNING_KEY",
    "DEVBOX_JWT_SIGNING_KEY",
];
pub const DEVBOX_JWT_TTL_SECONDS_ENV: &[&str] = &[
    "CODEX_GATEWAY_DEVBOX_JWT_TTL_SECONDS",
    "DEVBOX_JWT_TTL_SECONDS",
];
pub const DEVBOX_NAMESPACE_ENV: &[&str] = &["CODEX_GATEWAY_DEVBOX_NAMESPACE", "DEVBOX_NAMESPACE"];
pub const DEVBOX_RUNTIME_IMAGE_ENV: &[&str] =
    &["CODEX_GATEWAY_DEVBOX_RUNTIME_IMAGE", "DEVBOX_RUNTIME_IMAGE"];
pub const DEVBOX_ARCHIVE_AFTER_PAUSE_TIME_ENV: &[&str] = &[
    "CODEX_GATEWAY_DEVBOX_ARCHIVE_AFTER_PAUSE_TIME",
    "DEVBOX_ARCHIVE_AFTER_PAUSE_TIME",
];
pub const DEVBOX_BOOTSTRAP_TIMEOUT_SECONDS_ENV: &[&str] =
    &["CODEX_GATEWAY_DEVBOX_BOOTSTRAP_TIMEOUT_SECONDS"];
pub const DEVBOX_WAIT_TIMEOUT_SECONDS_ENV: &[&str] = &["CODEX_GATEWAY_DEVBOX_WAIT_TIMEOUT_SECONDS"];
pub const DEVBOX_GATEWAY_READY_TIMEOUT_SECONDS_ENV: &[&str] =
    &["CODEX_GATEWAY_DEVBOX_GATEWAY_READY_TIMEOUT_SECONDS"];

pub fn read_env(names: &[&str]) -> Option<String> {
    for name in names {
        let Ok(value) = env::var(name) else {
            continue;
        };
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    None
}

pub fn read_bool_flag(names: &[&str]) -> bool {
    read_env(names).as_deref() == Some("1")
}

pub fn read_u16(names: &[&str]) -> Option<u16> {
    read_env(names)?.parse().ok()
}

pub fn read_u64(names: &[&str]) -> Option<u64> {
    read_env(names)?.parse().ok().filter(|value| *value > 0)
}

pub fn read_usize(names: &[&str]) -> Option<usize> {
    read_env(names)?.parse().ok().filter(|value| *value > 0)
}

pub fn apply_codex_child_env(command: &mut Command) {
    if let Some(value) = read_env(CODEX_HOME_ENV) {
        command.env("CODEX_HOME", value);
    }

    if let Some(value) = read_env(DEFAULT_MODEL_ENV) {
        command.env("CODEX_MODEL", value);
    }

    if let Some(value) = read_env(OPENAI_API_KEY_ENV) {
        command.env("OPENAI_API_KEY", value);
    }

    if let Some(value) = read_env(OPENAI_BASE_URL_ENV) {
        command.env("CODEX_OPENAI_BASE_URL", value.clone());
        command.env("OPENAI_BASE_URL", value);
    }
}
