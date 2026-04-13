use std::path::PathBuf;
use std::time::Duration;

use crate::env_config::{
    BRIDGE_CWD_ENV, CODEX_BIN_ENV, DEBUG_ENV, DEFAULT_MODEL_ENV, HOST_ENV, MAX_SESSIONS_ENV,
    PORT_ENV, SESSION_SWEEP_INTERVAL_MS_ENV, SESSION_TTL_MS_ENV, JWT_SECRET_ENV,
    read_bool_flag, read_env, read_u16, read_u64, read_usize,
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

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub host: String,
    pub port: u16,
    pub bridge_cwd: PathBuf,
    pub public_dir: PathBuf,
    pub codex_bin: String,
    pub debug: bool,
    pub default_model: Option<String>,
    pub max_sessions: usize,
    pub session_ttl: Duration,
    pub session_sweep_interval: Duration,
    pub client_info: ClientInfo,
    pub auth: Option<AuthConfig>,
}

impl AppConfig {
    pub fn from_env(root_dir: PathBuf) -> Self {
        let public_dir = root_dir.join("public");

        Self {
            host: read_env(HOST_ENV).unwrap_or_else(|| "0.0.0.0".to_string()),
            port: read_u16(PORT_ENV).unwrap_or(1317),
            bridge_cwd: read_env(BRIDGE_CWD_ENV)
                .map(PathBuf::from)
                .unwrap_or_else(|| root_dir.clone()),
            public_dir,
            codex_bin: read_env(CODEX_BIN_ENV).unwrap_or_else(|| "codex".to_string()),
            debug: read_bool_flag(DEBUG_ENV),
            default_model: read_env(DEFAULT_MODEL_ENV),
            max_sessions: read_usize(MAX_SESSIONS_ENV).unwrap_or(12),
            session_ttl: Duration::from_millis(
                read_u64(SESSION_TTL_MS_ENV).unwrap_or(30 * 60 * 1000),
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
        }
    }
}
