use std::path::PathBuf;
use std::time::Duration;

use crate::env_config::{
    BRIDGE_CWD_ENV, CODEX_BIN_ENV, DEBUG_ENV, DEFAULT_MODEL_ENV, DEPLOYMENT_TIMEOUT_MS_ENV,
    DEVBOX_ARCHIVE_AFTER_PAUSE_TIME_ENV, DEVBOX_BASE_URL_ENV, DEVBOX_BOOTSTRAP_TIMEOUT_SECONDS_ENV,
    DEVBOX_GATEWAY_READY_TIMEOUT_SECONDS_ENV, DEVBOX_JWT_SIGNING_KEY_ENV,
    DEVBOX_JWT_TTL_SECONDS_ENV, DEVBOX_NAMESPACE_ENV, DEVBOX_RUNTIME_IMAGE_ENV, DEVBOX_TOKEN_ENV,
    DEVBOX_WAIT_TIMEOUT_SECONDS_ENV, HOST_ENV, JWT_SECRET_ENV, MAX_DEPLOYMENTS_ENV,
    MAX_SESSIONS_ENV, PORT_ENV, SEALOS_HOST_ENV, SESSION_RUNTIME_ENV,
    SESSION_SWEEP_INTERVAL_MS_ENV, SESSION_TTL_MS_ENV, read_bool_flag, read_env, read_u16,
    read_u64, read_usize,
};

#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub name: String,
    pub title: String,
    pub version: String,
}

#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub jwt_secret: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRuntimeMode {
    Local,
    Devbox,
}

#[derive(Debug, Clone)]
pub struct DevboxConfig {
    pub base_url: String,
    pub namespace: String,
    pub token: Option<String>,
    pub jwt_signing_key: Option<String>,
    pub jwt_ttl_seconds: u64,
    pub runtime_image: Option<String>,
    pub archive_after_pause_time: String,
    pub wait_timeout: Duration,
    pub gateway_ready_timeout: Duration,
    pub bootstrap_timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub host: String,
    pub port: u16,
    pub bridge_cwd: PathBuf,
    pub public_dir: PathBuf,
    pub lab_dir: PathBuf,
    pub codex_bin: String,
    pub debug: bool,
    pub default_model: Option<String>,
    pub max_sessions: usize,
    pub max_deployments: usize,
    pub session_ttl: Duration,
    pub deployment_timeout: Duration,
    pub session_sweep_interval: Duration,
    pub client_info: ClientInfo,
    pub auth: Option<AuthConfig>,
    pub session_runtime: SessionRuntimeMode,
    pub devbox: Option<DevboxConfig>,
}

impl AppConfig {
    pub fn from_env(root_dir: PathBuf) -> Self {
        let public_dir = root_dir.join("public");
        let lab_dir = root_dir.join("lab");
        let session_runtime = read_session_runtime();
        let devbox = read_devbox_config(session_runtime);

        Self {
            host: read_env(HOST_ENV).unwrap_or_else(|| "0.0.0.0".to_string()),
            port: read_u16(PORT_ENV).unwrap_or(1317),
            bridge_cwd: read_env(BRIDGE_CWD_ENV)
                .map(PathBuf::from)
                .unwrap_or_else(|| root_dir.clone()),
            public_dir,
            lab_dir,
            codex_bin: read_env(CODEX_BIN_ENV).unwrap_or_else(|| "codex".to_string()),
            debug: read_bool_flag(DEBUG_ENV),
            default_model: read_env(DEFAULT_MODEL_ENV),
            max_sessions: read_usize(MAX_SESSIONS_ENV).unwrap_or(12),
            max_deployments: read_usize(MAX_DEPLOYMENTS_ENV).unwrap_or(4),
            session_ttl: Duration::from_millis(
                read_u64(SESSION_TTL_MS_ENV).unwrap_or(30 * 60 * 1000),
            ),
            deployment_timeout: Duration::from_millis(
                read_u64(DEPLOYMENT_TIMEOUT_MS_ENV).unwrap_or(60 * 60 * 1000),
            ),
            session_sweep_interval: Duration::from_millis(
                read_u64(SESSION_SWEEP_INTERVAL_MS_ENV).unwrap_or(60 * 1000),
            ),
            client_info: ClientInfo {
                name: "codex_gateway_web".to_string(),
                title: "Codex Gateway Web".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            auth: read_env(JWT_SECRET_ENV).map(|jwt_secret| AuthConfig { jwt_secret }),
            session_runtime,
            devbox,
        }
    }
}

fn read_session_runtime() -> SessionRuntimeMode {
    match read_env(SESSION_RUNTIME_ENV)
        .unwrap_or_else(|| "local".to_string())
        .to_ascii_lowercase()
        .as_str()
    {
        "devbox" => SessionRuntimeMode::Devbox,
        _ => SessionRuntimeMode::Local,
    }
}

fn read_devbox_config(session_runtime: SessionRuntimeMode) -> Option<DevboxConfig> {
    if session_runtime != SessionRuntimeMode::Devbox {
        return None;
    }

    let base_url = read_env(DEVBOX_BASE_URL_ENV).or_else(|| {
        read_env(SEALOS_HOST_ENV).map(|host| {
            let normalized = host
                .trim()
                .trim_end_matches('/')
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .to_string();
            format!("https://devbox-server.{normalized}")
        })
    });

    Some(DevboxConfig {
        base_url: base_url.unwrap_or_default(),
        namespace: read_env(DEVBOX_NAMESPACE_ENV).unwrap_or_else(|| "ns-test".to_string()),
        token: read_env(DEVBOX_TOKEN_ENV),
        jwt_signing_key: read_env(DEVBOX_JWT_SIGNING_KEY_ENV),
        jwt_ttl_seconds: read_u64(DEVBOX_JWT_TTL_SECONDS_ENV).unwrap_or(4 * 60 * 60),
        runtime_image: read_env(DEVBOX_RUNTIME_IMAGE_ENV),
        archive_after_pause_time: read_env(DEVBOX_ARCHIVE_AFTER_PAUSE_TIME_ENV)
            .unwrap_or_else(|| "24h".to_string()),
        wait_timeout: Duration::from_secs(read_u64(DEVBOX_WAIT_TIMEOUT_SECONDS_ENV).unwrap_or(60)),
        gateway_ready_timeout: Duration::from_secs(
            read_u64(DEVBOX_GATEWAY_READY_TIMEOUT_SECONDS_ENV).unwrap_or(60),
        ),
        bootstrap_timeout: Duration::from_secs(
            read_u64(DEVBOX_BOOTSTRAP_TIMEOUT_SECONDS_ENV).unwrap_or(300),
        ),
    })
}
